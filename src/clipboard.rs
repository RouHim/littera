//! Clipboard ownership (FR-012).
//!
//! Littera has to stay the selection source: whoever pastes asks *us* for the data, so the transcript
//! lives in this process instead of a buffer. Four mechanisms can do that, and [`Clipboard::new`]
//! picks the first one that works — a background daemon owns no surface and must not steal focus,
//! which rules out the plain core protocol:
//!
//! 1. `zwlr_data_control_v1` — the compositor lets an unfocused client own the selection, and
//!    `set_selection` takes no serial. Everything except GNOME speaks this.
//! 2. `ext_data_control_v1` — the standardized successor with the same shape, used when the wlr one
//!    is absent.
//! 3. The X11 `CLIPBOARD` selection — for GNOME, whose Wayland side has neither data-control
//!    protocol: mutter's Xwayland bridge mirrors an X11 selection into the Wayland clipboard.
//! 4. Core `wl_data_device.set_selection` — the last resort. It wants a keyboard-focus serial that a
//!    background client cannot produce, so compositors that validate the serial ignore it.
//!
//! Every mechanism serves a transcript from a dedicated thread, keeps the current selection alive
//! until the next copy, and reports itself through [`Clipboard::mechanism`]. Before a paste
//! displaces the clipboard, [`Clipboard::snapshot`] reads the text that was there, and
//! [`Clipboard::restore`] puts it back once the paste had its chance — a copy that arrived in
//! between wins (FR-012); non-text selections are left to the transcript. The core mechanism
//! (4.) cannot read a foreign selection without keyboard focus, so it never restores; it is the
//! documented last resort anyway.
//!
//! `smithay-clipboard` is deliberately not used: it stores a selection only while its client has
//! keyboard focus (`store_selection` returns `None` unless the seat saw a `wl_keyboard.enter`),
//! which a dictation daemon never has.

use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{
    Mutex,
    mpsc::{self, RecvTimeoutError, Sender},
};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use calloop::channel::{Channel, Event as ChannelEvent, Sender as ChannelSender};
use calloop::generic::Generic;
use calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_callback, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source,
        wl_display, wl_registry, wl_seat,
    },
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::Event as X11Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as X11ConnectionExt, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// Mime types every transcript is offered under (the same UTF-8 bytes for all of them).
const MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// Mime preference for reading a foreign selection back for restoration (FR-012): the
/// transcript's own mime first, then the generic text mimes. Anything else — images, files,
/// rich markup — has no text to put back and is left alone.
const TEXT_MIME_PREFERENCE: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// The mime type a snapshot reads `offered` under, in [`TEXT_MIME_PREFERENCE`] order.
fn preferred_text_mime(offered: &[String]) -> Option<&'static str> {
    TEXT_MIME_PREFERENCE
        .iter()
        .find(|wanted| offered.iter().any(|mime| mime == *wanted))
        .copied()
}

/// How long [`Clipboard::copy_text`] waits for the compositor or server to confirm the selection.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
/// How long starting a clipboard thread may take (it only builds an event loop).
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long [`Clipboard::snapshot`] waits for the current selection owner to answer.
///
/// The owner answers within milliseconds when it is alive; the bound only exists so a hung
/// client cannot stall a dictation.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(400);
/// Upper bound for the selection a snapshot reads; a larger selection is not restored.
const SNAPSHOT_MAX_BYTES: usize = 1024 * 1024;

/// Mechanism names reported by [`Clipboard::mechanism`].
const MECHANISM_WLR: &str = "zwlr_data_control_v1";
const MECHANISM_EXT: &str = "ext_data_control_v1";
const MECHANISM_X11: &str = "X11 CLIPBOARD bridge";
const MECHANISM_CORE: &str = "wl_data_device (needs focus)";

/// Error when there is no Wayland session.
const NO_COMPOSITOR: &str = "cannot connect to the Wayland compositor (is WAYLAND_DISPLAY set?)";
/// Error when the compositor has no data device manager.
const NO_MANAGER: &str =
    "the compositor does not expose wl_data_device_manager, so the clipboard cannot be used";
/// Error when the compositor has no seat.
const NO_SEAT: &str = "the compositor exposes no wl_seat";
/// Error when there is no X11 display to fall back to.
const NO_X11: &str = "cannot connect to the X11 server (is DISPLAY set?)";
/// Error when an earlier panic left the clipboard unusable.
const POISONED: &str = "clipboard is poisoned after an earlier panic — restart Littera";
/// Error when the clipboard thread is not running.
const WORKER_GONE: &str = "the clipboard thread is not running — restart Littera";
/// Error when the compositor never confirmed the selection.
const NOT_CONFIRMED: &str =
    "the compositor did not confirm the clipboard selection in time — is the session alive?";
/// Error when the selection could not be handed to the compositor at all.
const FLUSH_FAILED: &str = "cannot reach the Wayland compositor to set the clipboard selection";

/// What a clipboard thread is asked to do.
enum Cmd {
    /// Own the selection with `text`; `reply` fires once the compositor confirmed it.
    Copy {
        text: String,
        reply: Sender<Result<()>>,
    },
    /// Put `text` back as the selection, unless another client took the selection meanwhile —
    /// `Ok(false)` then, because the newer copy must win (FR-012). `reply` settles once the
    /// compositor confirmed it.
    Restore {
        text: String,
        reply: Sender<Result<bool>>,
    },
    /// Read the current selection, which a paste is about to displace, so it can be put back
    /// later (FR-012). `reply` carries `None` when there is no text selection to read.
    Snapshot { reply: Sender<Option<String>> },
    /// Stop the event loop so the clipboard thread can exit.
    Shutdown,
}

/// Which selection object a source belongs to.
#[derive(Clone, Copy)]
enum ControlKind {
    Wlr,
    Ext,
    Core,
}

/// Create a fresh source, advertise the mime types and make it the selection.
///
/// A source may only be used for one `set_selection` request, so the previous one is destroyed
/// first. The data-control protocols take no serial; core `wl_data_device` does.
macro_rules! take_selection {
    ($manager:expr, $device:expr, $previous:expr, $queue:expr) => {{
        if let Some(previous) = $previous.take() {
            previous.destroy();
        }
        let source = $manager.create_data_source($queue, ());
        for mime_type in MIME_TYPES {
            source.offer(mime_type.to_owned());
        }
        $device.set_selection(Some(&source));
        *$previous = Some(source);
    }};
    ($manager:expr, $device:expr, $previous:expr, $queue:expr, $serial:expr) => {{
        if let Some(previous) = $previous.take() {
            previous.destroy();
        }
        let source = $manager.create_data_source($queue, ());
        for mime_type in MIME_TYPES {
            source.offer(mime_type.to_owned());
        }
        $device.set_selection(Some(&source), $serial);
        *$previous = Some(source);
    }};
}

/// A copy that is waiting for the compositor's `wl_display.sync` confirmation.
struct Pending {
    /// Identifies the sync request this copy waits for.
    sequence: u32,
    /// Characters in the text, for the log line that proves the selection is live.
    chars: usize,
    /// Who waits for the confirmation, and what it means to them.
    confirmation: Confirmation,
}

/// The caller waiting for the compositor's confirmation of a selection request.
enum Confirmation {
    /// [`Clipboard::copy_text`] waits for `Ok(())` — the transcript is the selection.
    Copy(Sender<Result<()>>),
    /// [`Clipboard::restore`] waits for `Ok(true)` — the previous text is back.
    Restore(Sender<Result<bool>>),
}

impl Confirmation {
    /// The compositor processed the request; the selection is live.
    fn confirmed(self, chars: usize) {
        match self {
            Confirmation::Copy(reply) => {
                tracing::info!(chars, "transcript is now the wayland clipboard selection");
                let _ = reply.send(Ok(()));
            }
            Confirmation::Restore(reply) => {
                tracing::info!(
                    chars,
                    "previous clipboard content is back on the wayland clipboard"
                );
                let _ = reply.send(Ok(true));
            }
        }
    }

    /// The request never reached the compositor.
    fn failed(self, error: anyhow::Error) {
        match self {
            Confirmation::Copy(reply) => {
                let _ = reply.send(Err(error));
            }
            Confirmation::Restore(reply) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

/// The Wayland object that owns the selection for this connection.
enum Selection {
    /// `zwlr_data_control_v1`: no focus and no serial needed.
    Wlr {
        manager: ZwlrDataControlManagerV1,
        device: ZwlrDataControlDeviceV1,
        source: Option<ZwlrDataControlSourceV1>,
    },
    /// `ext_data_control_v1`: the standardized successor, same shape.
    Ext {
        manager: ExtDataControlManagerV1,
        device: ExtDataControlDeviceV1,
        source: Option<ExtDataControlSourceV1>,
    },
    /// Core `wl_data_device`, which wants a keyboard-focus serial.
    Core {
        manager: wl_data_device_manager::WlDataDeviceManager,
        device: wl_data_device::WlDataDevice,
        source: Option<wl_data_source::WlDataSource>,
    },
}

/// An offer a foreign selection owner handed to this connection.
enum ForeignOffer {
    Wlr(ZwlrDataControlOfferV1),
    Ext(ExtDataControlOfferV1),
    Core(wl_data_offer::WlDataOffer),
}

impl ForeignOffer {
    /// Ask the owner for `mime_type`; the text arrives on `fd` (the owner writes and closes it).
    fn receive(&self, mime_type: String, fd: BorrowedFd<'_>) {
        match self {
            ForeignOffer::Wlr(offer) => offer.receive(mime_type, fd),
            ForeignOffer::Ext(offer) => offer.receive(mime_type, fd),
            ForeignOffer::Core(offer) => offer.receive(mime_type, fd),
        }
    }

    /// Drop the offer; the client must release every offer it received.
    fn destroy(self) {
        match self {
            ForeignOffer::Wlr(offer) => offer.destroy(),
            ForeignOffer::Ext(offer) => offer.destroy(),
            ForeignOffer::Core(offer) => offer.destroy(),
        }
    }
}

/// The current selection of another client, kept so a paste can read it back first (FR-012).
struct ForeignSelection {
    offer: ForeignOffer,
    /// The mime types the offer advertises.
    mimes: Vec<String>,
}

/// Wayland state, owned by the clipboard thread.
struct Worker {
    connection: Connection,
    queue_handle: QueueHandle<Worker>,
    display: wl_display::WlDisplay,
    selection: Selection,
    /// Bytes served for every advertised mime type.
    text: String,
    /// Serial of the last input event this connection saw. A connection that owns no surface never
    /// receives `wl_pointer`/`wl_keyboard` events, so it stays `0`; the core mechanism sends it
    /// anyway, which is exactly why that mechanism is the last resort.
    serial: u32,
    /// The in-flight copy, if one is waiting for confirmation.
    pending: Option<Pending>,
    /// Counter for the sync requests that confirm copies.
    next_sequence: u32,
    /// The current foreign selection — the content a paste would displace; kept alive so a
    /// snapshot can read it back, and destroyed when the next selection replaces it.
    foreign: Option<ForeignSelection>,
    /// Mime types announced for the offer the last `data_offer` event introduced; the
    /// `selection` event that follows moves them into [`Worker::foreign`].
    announced_mimes: Vec<String>,
    /// Set once the clipboard thread's event loop exists, so it can be stopped again.
    stop_signal: Option<LoopSignal>,
}

impl Worker {
    fn new(
        connection: &Connection,
        queue_handle: QueueHandle<Worker>,
        selection: Selection,
    ) -> Self {
        Worker {
            connection: connection.clone(),
            queue_handle,
            display: connection.display(),
            selection,
            text: String::new(),
            serial: 0,
            pending: None,
            next_sequence: 0,
            foreign: None,
            announced_mimes: Vec::new(),
            stop_signal: None,
        }
    }

    /// Own the selection with `text` and ask the compositor to confirm it.
    fn copy(&mut self, text: String, confirmation: Confirmation) {
        let chars = text.chars().count();
        self.text = text;
        let serial = self.serial;
        match &mut self.selection {
            Selection::Wlr {
                manager,
                device,
                source,
            } => {
                take_selection!(manager, device, source, &self.queue_handle);
            }
            Selection::Ext {
                manager,
                device,
                source,
            } => {
                take_selection!(manager, device, source, &self.queue_handle);
            }
            Selection::Core {
                manager,
                device,
                source,
            } => {
                take_selection!(manager, device, source, &self.queue_handle, serial);
            }
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        // A client's requests are processed in order, so this sync callback proves the compositor
        // handled `set_selection`. Reporting from the callback keeps this thread free of blocking
        // reads, which belong to the event loop.
        self.display.sync(&self.queue_handle, sequence);
        self.pending = Some(Pending {
            sequence,
            chars,
            confirmation,
        });

        if let Err(err) = self.connection.flush() {
            tracing::warn!(
                error = %err,
                "cannot flush the clipboard selection to the compositor",
            );
            if let Some(pending) = self.pending.take() {
                pending.confirmation.failed(anyhow!(FLUSH_FAILED));
            }
            return;
        }
        tracing::debug!(chars, "asked the compositor for the clipboard selection");
    }

    /// Put `text` back as the selection — unless another client took it meanwhile, in which case
    /// its newer content wins and nothing is sent (`Ok(false)`, FR-012).
    ///
    /// The ownership check rides on the compositor's `cancelled` event for our source, so a copy
    /// that raced the restore's grace period is caught as long as the event was dispatched —
    /// best effort, by design (the X11 mechanism checks the owner directly instead).
    fn restore(&mut self, text: String, reply: Sender<Result<bool>>) {
        if !self.owns_selection() {
            tracing::debug!("the selection was taken by another client; not restoring over it");
            let _ = reply.send(Ok(false));
            return;
        }
        self.copy(text, Confirmation::Restore(reply));
    }

    /// Whether the compositor still lets this connection own the selection.
    fn owns_selection(&self) -> bool {
        match &self.selection {
            Selection::Wlr { source, .. } => source.is_some(),
            Selection::Ext { source, .. } => source.is_some(),
            Selection::Core { source, .. } => source.is_some(),
        }
    }

    /// Start reading the current foreign selection so it can be put back after a paste.
    ///
    /// The reply is settled from the reader thread once the owner closed its end of the pipe —
    /// the read blocks on the owner's schedule, so it must not run on this thread (a hung client
    /// would freeze clipboard serving). The offer is kept alive until the transfer is done.
    fn snapshot(&mut self, reply: Sender<Option<String>>) {
        let Some(foreign) = self.foreign.take() else {
            let _ = reply.send(None);
            return;
        };
        let Some(mime) = preferred_text_mime(&foreign.mimes) else {
            tracing::debug!("the current selection offers no text mime; nothing to restore");
            foreign.offer.destroy();
            let _ = reply.send(None);
            return;
        };
        let (read_end, write_end) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(err) => {
                tracing::warn!(error = %err, "cannot create the pipe for reading the selection");
                foreign.offer.destroy();
                let _ = reply.send(None);
                return;
            }
        };
        tracing::debug!(mime_type = %mime, "reading the selection a paste is about to displace");
        foreign.offer.receive(mime.to_owned(), write_end.as_fd());
        if let Err(err) = self.connection.flush() {
            tracing::warn!(error = %err, "cannot ask the selection owner for its data");
            foreign.offer.destroy();
            let _ = reply.send(None);
            return;
        }
        read_foreign_offer(foreign.offer, read_end, write_end, reply);
    }

    /// Adopt the offer of a new foreign selection, dropping the one it replaces.
    fn adopt_foreign(&mut self, offer: ForeignOffer) {
        self.drop_foreign();
        self.foreign = Some(ForeignSelection {
            offer,
            mimes: std::mem::take(&mut self.announced_mimes),
        });
    }

    /// Forget the foreign selection, releasing its offer as the protocol requires.
    fn drop_foreign(&mut self) {
        if let Some(previous) = self.foreign.take() {
            previous.offer.destroy();
        }
    }

    /// The compositor processed everything sent before sync request `sequence`.
    fn confirm(&mut self, sequence: u32) {
        match self.pending.as_ref() {
            Some(pending) if pending.sequence == sequence => {}
            _ => return,
        }
        if let Some(pending) = self.pending.take() {
            pending.confirmation.confirmed(pending.chars);
        }
    }

    /// Forget the source the compositor cancelled: it took the selection away from us.
    fn release_source(&mut self, kind: ControlKind) {
        match (&mut self.selection, kind) {
            (Selection::Wlr { source, .. }, ControlKind::Wlr) => *source = None,
            (Selection::Ext { source, .. }, ControlKind::Ext) => *source = None,
            (Selection::Core { source, .. }, ControlKind::Core) => *source = None,
            _ => {}
        }
    }

    /// Stop the event loop when the clipboard is going away.
    fn shutdown(&mut self) {
        if let Some(signal) = self.stop_signal.take() {
            signal.stop();
        }
    }
}

/// Read a foreign selection to EOF on its own thread and settle `reply` (FR-012).
///
/// The pipe read blocks until the owning client closes its end, so it runs off the clipboard
/// thread — a hung or deadline-ignoring client must not freeze clipboard serving. `None` covers
/// every failure (truncated writes, invalid UTF-8, a selection past [`SNAPSHOT_MAX_BYTES`]); the
/// caller then simply does not restore.
fn read_foreign_offer(
    offer: ForeignOffer,
    mut read_end: std::io::PipeReader,
    write_end: std::io::PipeWriter,
    reply: Sender<Option<String>>,
) {
    let spawned = std::thread::Builder::new()
        .name("littera-clipboard-read".to_owned())
        .spawn(move || {
            // Only the owner writes; dropping our end makes its close signal EOF.
            drop(write_end);
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 16 * 1024];
            let mut readable = true;
            loop {
                match read_end.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        bytes.extend_from_slice(&chunk[..read]);
                        if bytes.len() > SNAPSHOT_MAX_BYTES {
                            tracing::debug!("the displaced selection is too large to restore");
                            readable = false;
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "reading the displaced selection failed");
                        readable = false;
                        break;
                    }
                }
            }
            offer.destroy();
            let text = readable.then(|| String::from_utf8(bytes).ok()).flatten();
            let _ = reply.send(text);
        });
    if let Err(err) = spawned {
        // The reply sender died with the closure, so the caller's wait ends on the disconnect.
        tracing::warn!(error = %err, "cannot start the clipboard reader thread");
    }
}

/// Write the transcript to the pipe a clipboard consumer handed us.
///
/// The consumer reads until EOF, so the fd has to be closed after the write — dropping the `File`
/// does that. Transcripts are far smaller than a pipe buffer, so this cannot block for long.
fn serve(text: &str, mime_type: &str, fd: OwnedFd) {
    let mut pipe = std::fs::File::from(fd);
    match pipe.write_all(text.as_bytes()) {
        Ok(()) => tracing::debug!(
            mime_type = %mime_type,
            bytes = text.len(),
            "served the clipboard selection",
        ),
        Err(err) => tracing::warn!(
            mime_type = %mime_type,
            error = %err,
            "the clipboard consumer stopped reading before the transcript was written",
        ),
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Worker {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        // The global list is maintained by `GlobalListContents` itself; nothing to do here.
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for Worker {
    fn event(
        state: &mut Self,
        _device: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // The offer's mime types follow immediately; the selection that names it does too.
            wl_data_device::Event::DataOffer { .. } => state.announced_mimes.clear(),
            wl_data_device::Event::Selection { id: Some(offer) } => {
                state.adopt_foreign(ForeignOffer::Core(offer));
            }
            wl_data_device::Event::Selection { id: None } => state.drop_foreign(),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Worker, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
    ]);
}

impl Dispatch<wl_data_source::WlDataSource, ()> for Worker {
    fn event(
        state: &mut Self,
        _source: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // A consumer (the application the user pastes into) fetches the transcript.
            wl_data_source::Event::Send { mime_type, fd } => serve(&state.text, &mime_type, fd),
            wl_data_source::Event::Cancelled => state.release_source(ControlKind::Core),
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Worker {
    fn event(
        state: &mut Self,
        _callback: &wl_callback::WlCallback,
        event: wl_callback::Event,
        sequence: &u32,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.confirm(*sequence);
        }
    }
}

delegate_noop!(Worker: ignore wl_seat::WlSeat);
delegate_noop!(Worker: ignore wl_data_device_manager::WlDataDeviceManager);

// Every offer type announces its mime types through `offer` events; they are collected per
// announced offer so a snapshot knows under which mime the foreign text can be read.
impl Dispatch<wl_data_offer::WlDataOffer, ()> for Worker {
    fn event(
        state: &mut Self,
        _offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            state.announced_mimes.push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for Worker {
    fn event(
        state: &mut Self,
        _offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.announced_mimes.push(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlOfferV1, ()> for Worker {
    fn event(
        state: &mut Self,
        _offer: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.announced_mimes.push(mime_type);
        }
    }
}

// The two data-control protocols have the same shape; `zwlr` is preferred over `ext` but both are
// wired up, so whichever the compositor advertises can own the selection.
delegate_noop!(Worker: ignore ZwlrDataControlManagerV1);
delegate_noop!(Worker: ignore ExtDataControlManagerV1);

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Worker {
    fn event(
        state: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // The offer's mime types follow immediately; the selection that names it does too.
            zwlr_data_control_device_v1::Event::DataOffer { .. } => state.announced_mimes.clear(),
            zwlr_data_control_device_v1::Event::Selection { id: Some(offer) } => {
                state.adopt_foreign(ForeignOffer::Wlr(offer));
            }
            zwlr_data_control_device_v1::Event::Selection { id: None } => state.drop_foreign(),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Worker, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for Worker {
    fn event(
        state: &mut Self,
        _source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // A consumer (the application the user pastes into) fetches the transcript.
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                serve(&state.text, &mime_type, fd);
            }
            zwlr_data_control_source_v1::Event::Cancelled => {
                state.release_source(ControlKind::Wlr);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for Worker {
    fn event(
        state: &mut Self,
        _device: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // The offer's mime types follow immediately; the selection that names it does too.
            ext_data_control_device_v1::Event::DataOffer { .. } => state.announced_mimes.clear(),
            ext_data_control_device_v1::Event::Selection { id: Some(offer) } => {
                state.adopt_foreign(ForeignOffer::Ext(offer));
            }
            ext_data_control_device_v1::Event::Selection { id: None } => state.drop_foreign(),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Worker, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlSourceV1, ()> for Worker {
    fn event(
        state: &mut Self,
        _source: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => {
                serve(&state.text, &mime_type, fd);
            }
            ext_data_control_source_v1::Event::Cancelled => {
                state.release_source(ControlKind::Ext);
            }
            _ => {}
        }
    }
}

/// Serve one message from a Wayland clipboard thread's channel.
fn handle_wayland_command(event: ChannelEvent<Cmd>, _metadata: &mut (), worker: &mut Worker) {
    match event {
        ChannelEvent::Msg(Cmd::Copy { text, reply }) => {
            worker.copy(text, Confirmation::Copy(reply));
        }
        ChannelEvent::Msg(Cmd::Restore { text, reply }) => worker.restore(text, reply),
        ChannelEvent::Msg(Cmd::Snapshot { reply }) => worker.snapshot(reply),
        ChannelEvent::Msg(Cmd::Shutdown) => worker.shutdown(),
        // Every sender is gone, so nobody can ask for a copy any more.
        ChannelEvent::Closed => worker.shutdown(),
    }
}

/// Build the Wayland event loop inside the clipboard thread and run it until asked to stop.
///
/// `ready` reports whether the loop could be set up at all, so [`Clipboard::new`] never hands out a
/// clipboard that cannot work.
fn run_wayland_worker(
    mut worker: Worker,
    queue: EventQueue<Worker>,
    channel: Channel<Cmd>,
    ready: Sender<Result<()>>,
) {
    let mut event_loop: EventLoop<'static, Worker> = match EventLoop::try_new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            let _ = ready.send(Err(anyhow!(
                "cannot create the clipboard event loop: {err}"
            )));
            return;
        }
    };
    let handle = event_loop.handle();
    if let Err(err) = handle.insert_source(channel, handle_wayland_command) {
        let _ = ready.send(Err(anyhow!(
            "cannot watch for clipboard commands: {}",
            err.error
        )));
        return;
    }
    // `WaylandSource` dispatches the event queue whenever the socket is readable and flushes our
    // requests around every poll, which is what keeps the selection being served.
    let socket = WaylandSource::new(worker.connection.clone(), queue);
    if let Err(err) = socket.insert(handle) {
        let _ = ready.send(Err(anyhow!(
            "cannot watch the wayland socket: {}",
            err.error
        )));
        return;
    }
    worker.stop_signal = Some(event_loop.get_signal());
    let _ = ready.send(Ok(()));

    let idle_timeout: Option<Duration> = None;
    if let Err(err) = event_loop.run(idle_timeout, &mut worker, |_| {}) {
        tracing::warn!(error = %err, "the wayland clipboard event loop stopped");
    }
}

/// Start a Wayland clipboard thread for the given selection handler.
fn spawn_wayland(
    queue: EventQueue<Worker>,
    worker: Worker,
    mechanism: &'static str,
) -> Result<Clipboard> {
    let (cmd, channel) = calloop::channel::channel::<Cmd>();
    let (ready, started) = mpsc::channel::<Result<()>>();
    let thread = std::thread::Builder::new()
        .name("littera-clipboard".to_owned())
        .spawn(move || run_wayland_worker(worker, queue, channel, ready))
        .context("cannot spawn the clipboard thread")?;
    finish_start(started, thread, cmd, mechanism)
}

/// The X11 atoms Littera compares against or writes.
struct X11Atoms {
    clipboard: Atom,
    targets: Atom,
    atom: Atom,
    utf8_string: Atom,
    text: Atom,
    string: Atom,
    plain: Atom,
    /// The property a snapshot reads the foreign selection into.
    snapshot: Atom,
}

impl X11Atoms {
    /// Intern every atom the clipboard needs.
    fn intern(connection: &RustConnection) -> Result<Self> {
        Ok(X11Atoms {
            clipboard: intern(connection, b"CLIPBOARD")?,
            targets: intern(connection, b"TARGETS")?,
            atom: intern(connection, b"ATOM")?,
            utf8_string: intern(connection, b"UTF8_STRING")?,
            text: intern(connection, b"TEXT")?,
            string: intern(connection, b"STRING")?,
            plain: intern(connection, b"text/plain;charset=utf-8")?,
            snapshot: intern(connection, b"LITTERA_SNAPSHOT")?,
        })
    }
}

/// Intern one atom name.
fn intern(connection: &RustConnection, name: &[u8]) -> Result<Atom> {
    let atom = connection
        .intern_atom(false, name)
        .context("cannot intern an X11 clipboard atom")?
        .reply()
        .context("no reply while interning an X11 clipboard atom")?
        .atom;
    Ok(atom)
}

/// X11 state, owned by the clipboard thread.
struct X11State {
    connection: RustConnection,
    /// The window that owns `CLIPBOARD` (created, never mapped), and the requestor of snapshots.
    window: Window,
    atoms: X11Atoms,
    /// Bytes served for every text target.
    text: String,
    /// The selection read that is waiting for its `SelectionNotify`, if any.
    pending_snapshot: Option<PendingSnapshot>,
    /// Set once the thread's event loop exists, so it can be stopped again.
    stop_signal: Option<LoopSignal>,
}

/// A read of the foreign selection that is waiting for the X server's `SelectionNotify`.
struct PendingSnapshot {
    reply: Sender<Option<String>>,
    /// The target being tried; when the owner refuses `UTF8_STRING`, `STRING` is tried once.
    target: SnapshotTarget,
}

/// The X11 target a snapshot reads the foreign selection as.
#[derive(Clone, Copy)]
enum SnapshotTarget {
    /// `UTF8_STRING` — what modern toolkits serve.
    Utf8String,
    /// `STRING` — the ICCCM fallback; its bytes are Latin-1, so mangled text is skipped.
    Latin1String,
}

impl SnapshotTarget {
    /// The atom this target converts to.
    fn atom(self, atoms: &X11Atoms) -> Atom {
        match self {
            SnapshotTarget::Utf8String => atoms.utf8_string,
            SnapshotTarget::Latin1String => atoms.string,
        }
    }

    /// The fallback to try when the owner refuses this target.
    fn fallback(self) -> Option<SnapshotTarget> {
        match self {
            SnapshotTarget::Utf8String => Some(SnapshotTarget::Latin1String),
            SnapshotTarget::Latin1String => None,
        }
    }
}

impl X11State {
    /// Connect to the X server and create the window that will own `CLIPBOARD`.
    fn new() -> Result<Self> {
        let (connection, screen_number) = x11rb::connect(None).context(NO_X11)?;
        let root = connection
            .setup()
            .roots
            .get(screen_number)
            .map(|screen| screen.root)
            .context(NO_X11)?;
        let window = connection
            .generate_id()
            .context("cannot allocate an X11 window id")?;
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .context("cannot create the X11 CLIPBOARD owner window")?;
        let atoms = X11Atoms::intern(&connection)?;
        connection
            .flush()
            .context("cannot flush the X11 clipboard setup")?;
        Ok(X11State {
            connection,
            window,
            atoms,
            text: String::new(),
            pending_snapshot: None,
            stop_signal: None,
        })
    }

    /// Claim `CLIPBOARD` with `text` and confirm that the server gave it to us.
    fn copy(&mut self, text: String, reply: Sender<Result<()>>) {
        let chars = text.chars().count();
        match self.claim_text(text) {
            Ok(()) => {
                tracing::info!(chars, "transcript is now the X11 CLIPBOARD selection");
                let _ = reply.send(Ok(()));
            }
            Err(err) => {
                tracing::warn!(error = %err, "cannot take the X11 CLIPBOARD selection");
                let _ = reply.send(Err(err));
            }
        }
    }

    /// Put `text` back as the selection — unless another client took the selection meanwhile, in
    /// which case its newer content wins and nothing is claimed (`Ok(false)`, FR-012).
    ///
    /// The check asks the server for the current owner, so a copy that raced the restore's grace
    /// period is always caught, regardless of when our event loop processes its `SelectionClear`.
    fn restore(&mut self, text: String, reply: Sender<Result<bool>>) {
        match selection_owner(&self.connection, self.atoms.clipboard) {
            Ok(owner) if owner != self.window => {
                tracing::debug!("the selection was taken by another client; not restoring over it");
                let _ = reply.send(Ok(false));
            }
            Ok(_) => {
                let chars = text.chars().count();
                match self.claim_text(text) {
                    Ok(()) => {
                        tracing::info!(
                            chars,
                            "previous clipboard content is back on the X11 CLIPBOARD"
                        );
                        let _ = reply.send(Ok(true));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "cannot put the previous CLIPBOARD content back");
                        let _ = reply.send(Err(err));
                    }
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "cannot check the X11 CLIPBOARD owner");
                let _ = reply.send(Err(err));
            }
        }
    }

    /// Take the `CLIPBOARD` with `text` and verify the server actually made us the owner.
    fn claim_text(&mut self, text: String) -> Result<()> {
        claim(&self.connection, self.window, self.atoms.clipboard)?;
        self.text = text;
        Ok(())
    }

    /// Start reading the foreign selection so it can be put back after a paste (FR-012).
    ///
    /// The answer arrives as a `SelectionNotify` on this connection's event loop and is settled
    /// in [`X11State::finish_snapshot`]; the caller's own timeout bounds the wait.
    fn snapshot(&mut self, reply: Sender<Option<String>>) {
        self.request_snapshot(SnapshotTarget::Utf8String, reply);
    }

    /// Ask the current selection owner to convert `target` into our snapshot property.
    fn request_snapshot(&mut self, target: SnapshotTarget, reply: Sender<Option<String>>) {
        if let Some(stale) = self
            .pending_snapshot
            .replace(PendingSnapshot { reply, target })
        {
            // Superseded before the owner answered; the newer read is the one that counts.
            let _ = stale.reply.send(None);
        }
        let convert_failed = self
            .connection
            .convert_selection(
                self.window,
                self.atoms.clipboard,
                target.atom(&self.atoms),
                self.atoms.snapshot,
                x11rb::CURRENT_TIME,
            )
            .err()
            .map(|err| err.to_string());
        if let Some(detail) = convert_failed {
            tracing::warn!(error = %detail, "cannot ask the X11 CLIPBOARD owner for its text");
            self.answer_snapshot(None);
            return;
        }
        if let Err(err) = self.connection.flush() {
            tracing::warn!(error = %err, "cannot flush the X11 CLIPBOARD read");
            self.answer_snapshot(None);
        }
    }

    /// Settle the pending snapshot, if any, with `text`.
    fn answer_snapshot(&mut self, text: Option<String>) {
        if let Some(pending) = self.pending_snapshot.take() {
            let _ = pending.reply.send(text);
        }
    }

    /// The owner answered a snapshot conversion: read the property or try the fallback target.
    fn finish_snapshot(&mut self, event: SelectionNotifyEvent) {
        if event.selection != self.atoms.clipboard {
            return;
        }
        let Some(pending) = self.pending_snapshot.as_ref() else {
            return;
        };
        if event.target != pending.target.atom(&self.atoms) {
            return;
        }
        let target = pending.target;
        if event.property == x11rb::NONE {
            tracing::debug!("the X11 CLIPBOARD owner refused the text target");
            let Some(fallback) = target.fallback() else {
                self.answer_snapshot(None);
                return;
            };
            let Some(reply) = self.pending_snapshot.take().map(|pending| pending.reply) else {
                return;
            };
            self.request_snapshot(fallback, reply);
            return;
        }
        let text = self.read_snapshot_property(event.property);
        self.answer_snapshot(text);
    }

    /// Read the snapshot property the owner just filled in.
    fn read_snapshot_property(&self, property: Atom) -> Option<String> {
        let reply = self
            .connection
            .get_property(
                true,
                self.window,
                property,
                AtomEnum::ANY,
                0,
                u32::try_from(SNAPSHOT_MAX_BYTES / 4).unwrap_or(u32::MAX),
            )
            .ok()?
            .reply()
            .ok()?;
        // Text is 8-bit; `bytes_after` means the selection exceeds the cap and `INCR` would start
        // an incremental transfer — restoring half of either would be worse than not restoring.
        if reply.format != 8 || reply.bytes_after != 0 || reply.value.is_empty() {
            tracing::debug!(
                format = reply.format,
                bytes_after = reply.bytes_after,
                "the displaced X11 selection is not plain text; leaving the transcript"
            );
            return None;
        }
        // A `STRING` selection is Latin-1 per ICCCM; only valid UTF-8 is ever put back.
        String::from_utf8(reply.value).ok()
    }

    /// Drain everything the X server sent us.
    fn pump(&mut self) {
        loop {
            match self.connection.poll_for_event() {
                Ok(Some(event)) => self.handle_event(event),
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(error = %err, "cannot read X11 events");
                    break;
                }
            }
        }
        if let Err(err) = self.connection.flush() {
            tracing::warn!(error = %err, "cannot flush X11 requests");
        }
    }

    /// Serve the selection events this connection receives.
    fn handle_event(&mut self, event: X11Event) {
        match event {
            X11Event::SelectionRequest(request) => self.serve_request(request),
            X11Event::SelectionNotify(notify) => {
                self.finish_snapshot(notify);
            }
            X11Event::SelectionClear(_) => {
                tracing::debug!("another X11 client took the CLIPBOARD selection");
            }
            _ => {}
        }
    }

    /// Answer a `SelectionRequest` the way ICCCM expects: the data goes into a property on the
    /// requestor and a `SelectionNotify` says whether the target was served.
    fn serve_request(&self, request: SelectionRequestEvent) {
        // ICCCM: a request without a property falls back to the target atom.
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let served = self.write_target(request.requestor, property, request.target);
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { x11rb::NONE },
        };
        if let Err(err) =
            self.connection
                .send_event(false, request.requestor, EventMask::NO_EVENT, notify)
        {
            tracing::warn!(error = %err, "cannot answer an X11 selection request");
        }
    }

    /// Write the requested target into `property` on the requestor.
    fn write_target(&self, requestor: Window, property: Atom, target: Atom) -> bool {
        if target == self.atoms.targets {
            let targets = [
                self.atoms.targets,
                self.atoms.utf8_string,
                self.atoms.text,
                self.atoms.string,
                self.atoms.plain,
            ];
            let data = targets
                .iter()
                .flat_map(|atom| atom.to_ne_bytes())
                .collect::<Vec<u8>>();
            return self.replace(requestor, property, self.atoms.atom, 32, &data);
        }
        if target == self.atoms.utf8_string
            || target == self.atoms.text
            || target == self.atoms.string
            || target == self.atoms.plain
        {
            return self.replace(requestor, property, target, 8, self.text.as_bytes());
        }
        tracing::debug!(atom = target, "unsupported X11 selection target");
        false
    }

    /// `ChangeProperty` on the requestor; reports whether the write was accepted.
    fn replace(
        &self,
        requestor: Window,
        property: Atom,
        type_: Atom,
        format: u8,
        data: &[u8],
    ) -> bool {
        let element = usize::from(format / 8).max(1);
        let data_len = u32::try_from(data.len() / element).unwrap_or(u32::MAX);
        let written = self.connection.change_property(
            PropMode::REPLACE,
            requestor,
            property,
            type_,
            format,
            data_len,
            data,
        );
        match written {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(error = %err, "cannot write an X11 selection property");
                false
            }
        }
    }

    /// Stop the event loop when the clipboard is going away.
    fn shutdown(&mut self) {
        if let Some(signal) = self.stop_signal.take() {
            signal.stop();
        }
    }
}

/// Serve one message from the X11 clipboard thread's channel.
fn handle_x11_command(event: ChannelEvent<Cmd>, _metadata: &mut (), state: &mut X11State) {
    match event {
        ChannelEvent::Msg(Cmd::Copy { text, reply }) => state.copy(text, reply),
        ChannelEvent::Msg(Cmd::Restore { text, reply }) => state.restore(text, reply),
        ChannelEvent::Msg(Cmd::Snapshot { reply }) => state.snapshot(reply),
        ChannelEvent::Msg(Cmd::Shutdown) => state.shutdown(),
        ChannelEvent::Closed => state.shutdown(),
    }
}

/// Build the X11 event loop inside the clipboard thread and run it until asked to stop.
fn run_x11_worker(mut state: X11State, channel: Channel<Cmd>, ready: Sender<Result<()>>) {
    let mut event_loop: EventLoop<'static, X11State> = match EventLoop::try_new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            let _ = ready.send(Err(anyhow!(
                "cannot create the X11 clipboard event loop: {err}"
            )));
            return;
        }
    };
    let handle = event_loop.handle();
    if let Err(err) = handle.insert_source(channel, handle_x11_command) {
        let _ = ready.send(Err(anyhow!(
            "cannot watch for clipboard commands: {}",
            err.error
        )));
        return;
    }
    let socket = match state.connection.stream().as_fd().try_clone_to_owned() {
        Ok(socket) => socket,
        Err(err) => {
            let _ = ready.send(Err(anyhow!("cannot watch the X11 socket: {err}")));
            return;
        }
    };
    // The socket is watched through calloop, so commands and `SelectionRequest` events share one
    // thread without polling.
    let source = Generic::new(socket, Interest::READ, Mode::Level);
    if let Err(err) = handle.insert_source(source, |_, _, state: &mut X11State| {
        state.pump();
        Ok(PostAction::Continue)
    }) {
        let _ = ready.send(Err(anyhow!("cannot watch the X11 socket: {}", err.error)));
        return;
    }
    state.stop_signal = Some(event_loop.get_signal());
    let _ = ready.send(Ok(()));

    let idle_timeout: Option<Duration> = None;
    if let Err(err) = event_loop.run(idle_timeout, &mut state, |_| {}) {
        tracing::warn!(error = %err, "the X11 clipboard event loop stopped");
    }
}

/// Own the X11 `CLIPBOARD` selection, which the Xwayland bridge mirrors into the Wayland clipboard.
fn start_x11() -> Result<Clipboard> {
    let state = X11State::new()?;
    let (cmd, channel) = calloop::channel::channel::<Cmd>();
    let (ready, started) = mpsc::channel::<Result<()>>();
    let thread = std::thread::Builder::new()
        .name("littera-clipboard-x11".to_owned())
        .spawn(move || run_x11_worker(state, channel, ready))
        .context("cannot spawn the X11 clipboard thread")?;
    finish_start(started, thread, cmd, MECHANISM_X11)
}

/// Claim the selection and wait for the server to confirm the new owner.
fn claim(connection: &RustConnection, window: Window, clipboard: Atom) -> Result<()> {
    connection
        .set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .context("cannot ask the X11 server for the CLIPBOARD selection")?;
    connection
        .flush()
        .context("cannot flush the X11 selection request")?;
    let owner = selection_owner(connection, clipboard)?;
    if owner != window {
        return Err(anyhow!(
            "the X11 server kept the CLIPBOARD selection (owner 0x{owner:x}, expected 0x{window:x})"
        ));
    }
    Ok(())
}

/// The window the X server currently has the `CLIPBOARD` assigned to (`x11rb::NONE` for none).
fn selection_owner(connection: &RustConnection, clipboard: Atom) -> Result<Window> {
    Ok(connection
        .get_selection_owner(clipboard)
        .context("cannot verify the X11 CLIPBOARD owner")?
        .reply()
        .context("no reply while verifying the X11 CLIPBOARD owner")?
        .owner)
}

/// Clipboard access that owns the selection with the current transcript.
///
/// `Send + Sync`: the command sender sits behind a `Mutex` and the thread handle is independent of
/// the event loop, so this can live inside the shared text inserter.
pub struct Clipboard {
    /// Commands for the clipboard thread.
    cmd: Mutex<ChannelSender<Cmd>>,
    /// The clipboard thread, joined when the clipboard is dropped.
    thread: Option<JoinHandle<()>>,
    /// Which mechanism owns the selection (see the module documentation).
    mechanism: &'static str,
}

impl Clipboard {
    /// The mechanism that owns the selection, e.g. `"zwlr_data_control_v1"`.
    pub fn mechanism(&self) -> &'static str {
        self.mechanism
    }

    /// Connect to the session and start the first clipboard mechanism that works.
    ///
    /// Fails only when no mechanism is available at all; the chosen one is logged at `info`.
    pub fn new() -> Result<Clipboard> {
        // 1. and 2.: the data-control protocols own the selection without focus or a serial.
        let mut missing = NO_COMPOSITOR;
        if let Ok(connection) = Connection::connect_to_env() {
            match registry_queue_init::<Worker>(&connection) {
                Ok((globals, queue)) => {
                    let queue_handle = queue.handle();
                    let Ok(seat) = globals.bind::<wl_seat::WlSeat, _, _>(&queue_handle, 1..=1, ())
                    else {
                        return x11_fallback(NO_SEAT);
                    };

                    let wlr_manager =
                        globals.bind::<ZwlrDataControlManagerV1, _, _>(&queue_handle, 1..=2, ());
                    if let Ok(manager) = wlr_manager {
                        let device = manager.get_data_device(&seat, &queue_handle, ());
                        let selection = Selection::Wlr {
                            manager,
                            device,
                            source: None,
                        };
                        let worker = Worker::new(&connection, queue_handle, selection);
                        return spawn_wayland(queue, worker, MECHANISM_WLR);
                    }

                    let ext_manager =
                        globals.bind::<ExtDataControlManagerV1, _, _>(&queue_handle, 1..=1, ());
                    if let Ok(manager) = ext_manager {
                        let device = manager.get_data_device(&seat, &queue_handle, ());
                        let selection = Selection::Ext {
                            manager,
                            device,
                            source: None,
                        };
                        let worker = Worker::new(&connection, queue_handle, selection);
                        return spawn_wayland(queue, worker, MECHANISM_EXT);
                    }

                    // 3.: the X11 bridge, which GNOME needs because it has no data-control protocol.
                    match start_x11() {
                        Ok(clipboard) => return Ok(clipboard),
                        Err(err) => tracing::debug!(
                            error = %err,
                            "the X11 CLIPBOARD bridge is unavailable",
                        ),
                    }

                    // 4.: core wl_data_device, the last resort.
                    let manager = globals
                        .bind::<wl_data_device_manager::WlDataDeviceManager, _, _>(
                            &queue_handle,
                            1..=3,
                            (),
                        )
                        .context(NO_MANAGER)?;
                    let device = manager.get_data_device(&seat, &queue_handle, ());
                    let selection = Selection::Core {
                        manager,
                        device,
                        source: None,
                    };
                    tracing::warn!(
                        "the compositor offers neither data-control protocol and no X11 display is \
                         available, so the selection goes through wl_data_device.set_selection, \
                         which needs keyboard focus and may be ignored"
                    );
                    let worker = Worker::new(&connection, queue_handle, selection);
                    return spawn_wayland(queue, worker, MECHANISM_CORE);
                }
                Err(err) => {
                    tracing::debug!(error = %err, "cannot enumerate wayland globals");
                    missing = "the wayland session cannot be enumerated";
                }
            }
        } else {
            tracing::debug!(NO_COMPOSITOR);
        }

        // No usable Wayland session: the X11 bridge is the only mechanism left.
        x11_fallback(missing)
    }

    /// Put `text` on the clipboard as the current selection and keep it there.
    ///
    /// Returns once the compositor or X server has confirmed that it processed the selection
    /// request, so a paste issued right afterwards sees the transcript instead of the previous
    /// clipboard content.
    pub fn copy_text(&self, text: &str) -> Result<()> {
        let (reply, confirmed) = mpsc::channel();
        {
            let cmd = self.cmd.lock().map_err(|_| anyhow!(POISONED))?;
            cmd.send(Cmd::Copy {
                text: text.to_owned(),
                reply,
            })
            .map_err(|_| anyhow!(WORKER_GONE))?;
        }
        match confirmed.recv_timeout(CONFIRM_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(anyhow!(NOT_CONFIRMED)),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow!(WORKER_GONE)),
        }
    }

    /// Read the current selection so it can be put back after a paste displaced it (FR-012).
    ///
    /// `None` whenever there is nothing to restore: no owner, no text mime, an oversized or
    /// non-UTF-8 selection, or a client that does not answer within [`SNAPSHOT_TIMEOUT`]. Never
    /// fails — a dictation must not depend on the old clipboard content.
    pub fn snapshot(&self) -> Option<String> {
        let (reply, answer) = mpsc::channel();
        {
            let cmd = self.cmd.lock().ok()?;
            cmd.send(Cmd::Snapshot { reply }).ok()?;
        }
        match answer.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(text) => text,
            Err(RecvTimeoutError::Timeout) => {
                tracing::debug!("the selection owner did not answer; nothing to restore");
                None
            }
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Put the previously snapshotted `text` back as the selection, unless another client took the
    /// selection first — its newer copy must win, reported as `Ok(false)` (FR-012).
    pub fn restore(&self, text: &str) -> Result<bool> {
        let (reply, confirmed) = mpsc::channel();
        {
            let cmd = self.cmd.lock().map_err(|_| anyhow!(POISONED))?;
            cmd.send(Cmd::Restore {
                text: text.to_owned(),
                reply,
            })
            .map_err(|_| anyhow!(WORKER_GONE))?;
        }
        match confirmed.recv_timeout(CONFIRM_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(anyhow!(NOT_CONFIRMED)),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow!(WORKER_GONE)),
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        tracing::debug!(mechanism = self.mechanism(), "clipboard shutting down");
        // Ask the worker to stop its event loop. Without the message (a poisoned lock) the thread is
        // left running rather than risking a blocked exit.
        let Ok(cmd) = self.cmd.lock() else { return };
        let _ = cmd.send(Cmd::Shutdown);
        drop(cmd);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the clipboard thread panicked");
        }
    }
}

/// Fall back to the X11 `CLIPBOARD` selection, naming what was missing.
fn x11_fallback(missing: &str) -> Result<Clipboard> {
    start_x11().map_err(|err| anyhow!("cannot own the clipboard: {missing} and {err}"))
}

/// Wait for a clipboard thread to report that its event loop is running.
fn finish_start(
    started: mpsc::Receiver<Result<()>>,
    thread: JoinHandle<()>,
    cmd: ChannelSender<Cmd>,
    mechanism: &'static str,
) -> Result<Clipboard> {
    match started.recv_timeout(START_TIMEOUT) {
        Ok(result) => result?,
        Err(RecvTimeoutError::Timeout) => {
            return Err(anyhow!("the clipboard thread did not start in time"));
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err(anyhow!("the clipboard thread stopped while starting"));
        }
    }
    tracing::info!(mechanism, "clipboard mechanism selected");
    Ok(Clipboard {
        cmd: Mutex::new(cmd),
        thread: Some(thread),
        mechanism,
    })
}

/// Serialises the tests that write the *global* clipboard selection: two
/// independent owners racing would make the read-back assertion flaky.
#[cfg(test)]
pub(crate) static SELECTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    #[test]
    fn the_transcript_mime_is_preferred_when_reading_a_selection() {
        let offered = [
            "text/html",
            "text/plain",
            "UTF8_STRING",
            "text/plain;charset=utf-8",
        ]
        .map(str::to_owned);
        assert_eq!(
            super::preferred_text_mime(&offered),
            Some("text/plain;charset=utf-8")
        );
    }

    #[test]
    fn utf8_string_beats_the_generic_plain_text_mime() {
        let offered = ["text/plain", "UTF8_STRING"].map(str::to_owned);
        assert_eq!(super::preferred_text_mime(&offered), Some("UTF8_STRING"));
    }

    #[test]
    fn a_selection_without_a_text_mime_is_not_read() {
        let offered = ["image/png", "text/html"].map(str::to_owned);
        assert_eq!(super::preferred_text_mime(&offered), None);
    }

    /// Poll the compositor until `wl-paste` reports `text`; the fixture owner claims the
    /// selection asynchronously, so its arrival must be waited for.
    fn wait_for_selection(text: &str) {
        for _ in 0..200 {
            if let Ok(output) = std::process::Command::new("wl-paste")
                .arg("--no-newline")
                .output()
                && output.stdout == text.as_bytes()
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the fixture selection {text:?} never became the clipboard");
    }

    /// Read the selection through an independent client.
    fn read_selection() -> String {
        let output = std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .output()
            .expect("wl-paste must run");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Spawn an independent selection owner (`wl-copy --foreground`), or `None` to skip the test
    /// when wl-clipboard is not installed.
    fn spawn_owner(text: &str) -> Option<std::process::Child> {
        match std::process::Command::new("wl-copy")
            .arg("--foreground")
            .arg(text)
            .spawn()
        {
            Ok(owner) => Some(owner),
            Err(error) => {
                eprintln!("skipping the test: wl-copy is unavailable ({error})");
                None
            }
        }
    }

    #[test]
    #[ignore = "needs a live session plus wl-clipboard (wl-copy/wl-paste) for the fixture"]
    fn snapshot_reads_the_selection_a_paste_would_displace() {
        let _guard = crate::clipboard::SELECTION_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let fixture = "littera snapshot fixture";
        let Some(mut owner) = spawn_owner(fixture) else {
            return;
        };
        wait_for_selection(fixture);

        let clipboard = crate::clipboard::Clipboard::new().expect("clipboard");
        let snapshot = clipboard.snapshot();
        // Reading is a request, not a takeover: the owner keeps serving afterwards.
        let still_there = read_selection();

        owner.kill().ok();
        owner.wait().ok();
        assert_eq!(snapshot.as_deref(), Some(fixture));
        assert_eq!(
            still_there, fixture,
            "a snapshot must not displace the owner"
        );
    }

    #[test]
    #[ignore = "needs a live session plus wl-clipboard (wl-copy/wl-paste) for the fixture"]
    fn restore_puts_the_displaced_selection_back() {
        let _guard = crate::clipboard::SELECTION_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let fixture = "littera restore fixture";
        let Some(mut owner) = spawn_owner(fixture) else {
            return;
        };
        wait_for_selection(fixture);

        let clipboard = crate::clipboard::Clipboard::new().expect("clipboard");
        let previous = clipboard.snapshot();
        assert_eq!(
            previous.as_deref(),
            Some(fixture),
            "the fixture must be readable"
        );
        clipboard.copy_text("littera transcript").expect("copy");
        let restored = clipboard
            .restore(previous.as_deref().expect("snapshot"))
            .expect("restore");
        let pasted = read_selection();

        owner.kill().ok();
        owner.wait().ok();
        assert!(restored, "the fixture cannot have been displaced");
        assert_eq!(pasted, fixture, "the independent reader must see it again");
    }

    #[test]
    #[ignore = "needs a live session plus wl-clipboard (wl-copy/wl-paste) for the fixture"]
    fn restore_yields_to_a_copy_that_arrived_meanwhile() {
        let _guard = crate::clipboard::SELECTION_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let clipboard = crate::clipboard::Clipboard::new().expect("clipboard");
        clipboard.copy_text("littera transcript").expect("copy");

        let newer = "the newer copy";
        let Some(mut owner) = spawn_owner(newer) else {
            return;
        };
        wait_for_selection(newer);

        let restored = clipboard.restore("the older selection").expect("restore");
        let pasted = read_selection();

        owner.kill().ok();
        owner.wait().ok();
        assert!(!restored, "a newer copy must win");
        assert_eq!(pasted, newer);
    }

    #[test]
    #[ignore = "needs a live Wayland session and wl-clipboard (wl-paste)"]
    fn copy_text_owns_the_selection_after_the_call() {
        let _guard = crate::clipboard::SELECTION_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let clipboard = crate::clipboard::Clipboard::new().expect("clipboard");
        let text = "littera clipboard chain test";
        clipboard.copy_text(text).expect("copy");

        // `wl-paste` is an independent client: it asks the compositor for the
        // selection, which is what proves the mechanism really took effect.
        let output = std::process::Command::new("wl-paste").output();
        match output {
            Ok(output) => {
                let pasted = String::from_utf8_lossy(&output.stdout);
                assert!(
                    pasted.contains(text),
                    "the transcript must be the selection (got {:?})",
                    pasted
                );
                eprintln!("clipboard mechanism: {}", clipboard.mechanism());
            }
            Err(error) => eprintln!("skipping the read-back: wl-paste is unavailable ({error})"),
        }
    }
}
