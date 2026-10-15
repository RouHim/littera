//! Clipboard ownership (FR-012).
//!
//! Lingua has to stay the selection source: whoever pastes asks *us* for the data, so the transcript
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
//! until the next copy, and reports itself through [`Clipboard::mechanism`]. No clipboard restoration
//! is implemented (FR-012).
//!
//! `smithay-clipboard` is deliberately not used: it stores a selection only while its client has
//! keyboard focus (`store_selection` returns `None` unless the seat saw a `wl_keyboard.enter`),
//! which a dictation daemon never has.

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
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
    ext_data_control_offer_v1::ExtDataControlOfferV1,
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::Event as X11Event;
use x11rb::protocol::xproto::{
    Atom, ConnectionExt as X11ConnectionExt, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// Mime types every transcript is offered under (the same UTF-8 bytes for all of them).
const MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// How long [`Clipboard::copy_text`] waits for the compositor or server to confirm the selection.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
/// How long starting a clipboard thread may take (it only builds an event loop).
const START_TIMEOUT: Duration = Duration::from_secs(5);

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
const POISONED: &str = "clipboard is poisoned after an earlier panic — restart Lingua";
/// Error when the clipboard thread is not running.
const WORKER_GONE: &str = "the clipboard thread is not running — restart Lingua";
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
    /// Characters in the transcript, for the log line that proves the selection is live.
    chars: usize,
    /// Reply channel of the waiting [`Clipboard::copy_text`].
    reply: Sender<Result<()>>,
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
            stop_signal: None,
        }
    }

    /// Own the selection with `text` and ask the compositor to confirm it.
    fn copy(&mut self, text: String, reply: Sender<Result<()>>) {
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
            reply,
        });

        if let Err(err) = self.connection.flush() {
            tracing::warn!(
                error = %err,
                "cannot flush the clipboard selection to the compositor",
            );
            if let Some(pending) = self.pending.take() {
                let _ = pending.reply.send(Err(anyhow!(FLUSH_FAILED)));
            }
            return;
        }
        tracing::debug!(chars, "asked the compositor for the clipboard selection");
    }

    /// The compositor processed everything sent before sync request `sequence`.
    fn confirm(&mut self, sequence: u32) {
        match self.pending.as_ref() {
            Some(pending) if pending.sequence == sequence => {}
            _ => return,
        }
        if let Some(pending) = self.pending.take() {
            tracing::info!(
                chars = pending.chars,
                "transcript is now the wayland clipboard selection",
            );
            let _ = pending.reply.send(Ok(()));
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
        _state: &mut Self,
        _device: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // Lingua only ever writes the selection, never reads one, so an offer is released.
            wl_data_device::Event::DataOffer { .. } => {}
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
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
delegate_noop!(Worker: ignore wl_data_offer::WlDataOffer);
delegate_noop!(Worker: ignore wl_data_device_manager::WlDataDeviceManager);

// The two data-control protocols have the same shape; `zwlr` is preferred over `ext` but both are
// wired up, so whichever the compositor advertises can own the selection.
delegate_noop!(Worker: ignore ZwlrDataControlManagerV1);
delegate_noop!(Worker: ignore ZwlrDataControlOfferV1);
delegate_noop!(Worker: ignore ExtDataControlManagerV1);
delegate_noop!(Worker: ignore ExtDataControlOfferV1);

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Worker {
    fn event(
        _state: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            // Lingua only ever writes the selection, never reads one, so an offer is released.
            zwlr_data_control_device_v1::Event::DataOffer { .. } => {}
            zwlr_data_control_device_v1::Event::Selection { id: Some(offer) } => offer.destroy(),
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
        _state: &mut Self,
        _device: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::DataOffer { .. } => {}
            ext_data_control_device_v1::Event::Selection { id: Some(offer) } => offer.destroy(),
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
        ChannelEvent::Msg(Cmd::Copy { text, reply }) => worker.copy(text, reply),
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
        .name("lingua-clipboard".to_owned())
        .spawn(move || run_wayland_worker(worker, queue, channel, ready))
        .context("cannot spawn the clipboard thread")?;
    finish_start(started, thread, cmd, mechanism)
}

/// The X11 atoms Lingua compares against or writes.
struct X11Atoms {
    clipboard: Atom,
    targets: Atom,
    atom: Atom,
    utf8_string: Atom,
    text: Atom,
    string: Atom,
    plain: Atom,
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
    /// The window that owns `CLIPBOARD` (created, never mapped).
    window: Window,
    atoms: X11Atoms,
    /// Bytes served for every text target.
    text: String,
    /// Set once the thread's event loop exists, so it can be stopped again.
    stop_signal: Option<LoopSignal>,
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
            stop_signal: None,
        })
    }

    /// Claim `CLIPBOARD` with `text` and confirm that the server gave it to us.
    fn copy(&mut self, text: String, reply: Sender<Result<()>>) {
        self.text = text;
        match claim(&self.connection, self.window, self.atoms.clipboard) {
            Ok(()) => {
                tracing::info!(
                    chars = self.text.chars().count(),
                    "transcript is now the X11 CLIPBOARD selection",
                );
                let _ = reply.send(Ok(()));
            }
            Err(err) => {
                tracing::warn!(error = %err, "cannot take the X11 CLIPBOARD selection");
                let _ = reply.send(Err(err));
            }
        }
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

    /// Serve the two selection events a clipboard owner receives.
    fn handle_event(&mut self, event: X11Event) {
        match event {
            X11Event::SelectionRequest(request) => self.serve_request(request),
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
        .name("lingua-clipboard-x11".to_owned())
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
    let owner = connection
        .get_selection_owner(clipboard)
        .context("cannot verify the X11 CLIPBOARD owner")?
        .reply()
        .context("no reply while verifying the X11 CLIPBOARD owner")?
        .owner;
    if owner != window {
        return Err(anyhow!(
            "the X11 server kept the CLIPBOARD selection (owner 0x{owner:x}, expected 0x{window:x})"
        ));
    }
    Ok(())
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
    #[ignore = "needs a live Wayland session and wl-clipboard (wl-paste)"]
    fn copy_text_owns_the_selection_after_the_call() {
        let _guard = crate::clipboard::SELECTION_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let clipboard = crate::clipboard::Clipboard::new().expect("clipboard");
        let text = "lingua clipboard chain test";
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
