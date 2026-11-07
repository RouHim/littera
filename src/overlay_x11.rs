//! X11 / XWayland overlay backend — the non-activating indicator used on GNOME.
//!
//! GNOME/mutter cannot host a non-activating Wayland toplevel, so on GNOME the indicator is an
//! X11 window in the session's XWayland. Its entire non-focusing behaviour rests on the window
//! properties that are set **before the first `map_window`**:
//!
//! * `_NET_WM_WINDOW_TYPE = _NET_WM_WINDOW_TYPE_NOTIFICATION` — mutter sets `takes_focus = FALSE`
//!   for notification windows while still keeping them above normal windows,
//! * `WM_HINTS.input = False` — `meta_window_x11_is_focusable()` returns `FALSE`, so the window
//!   can never be given the input focus,
//! * an empty `SHAPE` *input* region, so pointer input falls through to whatever is below,
//! * `_NET_WM_STATE = _NET_WM_STATE_ABOVE | _NET_WM_STATE_SKIP_TASKBAR | _NET_WM_STATE_SKIP_PAGER`,
//!   `_MOTIF_WM_HINTS` with decorations off, `WM_CLASS`, `_NET_WM_PID` and `WM_NORMAL_HINTS` with
//!   `USPosition` plus the target coordinates.
//!
//! `SetInputFocus` is never called. The window is unmanaged by the user: the compositor is free
//! to keep it on top, and it never steals a keystroke from the application being dictated into.
//!
//! All X11 work happens on one dedicated thread (`littera-overlay-x11`) that owns the connection
//! and every X11 resource; the presenter only sends commands over a channel and waits for the
//! acknowledgement. Pixels travel through a server-allocated MIT-SHM segment (SHM 1.2
//! `CreateSegment`), which is written through the returned file descriptor, so no `mmap` and no
//! `unsafe` are needed.

use std::fs::File;
use std::os::unix::fs::FileExt as _;
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow, bail};
use x11rb::connection::Connection as _;
use x11rb::cookie::{Cookie, CookieWithFds, VoidCookie};
use x11rb::errors::ConnectionError;
use x11rb::properties::{WmHints, WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::randr::{self, MonitorInfo};
use x11rb::protocol::shape::{self, SK, SO};
use x11rb::protocol::shm::{self, Seg};
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ClipOrdering, Colormap, ColormapAlloc, ConfigureWindowAux, CreateGCAux,
    CreateWindowAux, Gcontext, ImageFormat, PropMode, VisualClass, Visualid, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::x11_utils::{TryParse, TryParseFd};

use crate::overlay::{OverlayFrame, OverlayPresenter};
use crate::overlay_render;

/// The overlay is rasterised in X11 pixels. Under rootless XWayland the X coordinate space
/// already matches the compositor's logical pixel space — the compositor scales the whole
/// Xwayland surface itself — so one X pixel is one logical pixel and the indicator keeps the
/// same logical size as the layer-shell backend (which renders at the output's integer scale).
const SCALE: i32 = 1;

/// Overlay size in X11 pixels; `SCALE == 1`, so this equals the renderer's logical size.
const OVERLAY_W_PX: u16 = overlay_render::OVERLAY_W as u16 * SCALE as u16;
const OVERLAY_H_PX: u16 = overlay_render::OVERLAY_H as u16 * SCALE as u16;

/// `MWM_HINTS_DECORATIONS` of `_MOTIF_WM_HINTS`: the window manager must add no decorations.
const MWM_HINTS_DECORATIONS: u32 = 1 << 1;

/// The renderer's card colour (`#1E1E24`) as a conventional 24-bit TrueColor pixel. Used as the
/// window background and — in the byte order `overlay_render::premultiplied_bgra` emits — to make
/// the rendered image opaque when the server has no 32-bit ARGB visual and the window is
/// therefore 24 bits deep (a 24-bit window ignores the alpha channel).
const CARD_PIXEL: u32 = 0x001E_1E24;
const CARD_RGB: (u8, u8, u8) = (0x1E, 0x1E, 0x24);

const THREAD_NAME: &str = "littera-overlay-x11";

/// A monitor (or screen) rectangle in root coordinates: `(x, y, width, height)`.
type Rect = (i32, i32, i32, i32);

/// The answer channel of a single overlay command.
type Ack = mpsc::Sender<Result<()>>;

/// `true` when `$DISPLAY` is usable: the variable is set and a connection succeeds.
pub fn available() -> bool {
    if std::env::var_os("DISPLAY").is_none() {
        return false;
    }
    x11rb::connect(None).is_ok()
}

/// Start the X11 overlay. The connection, the window properties and the MIT-SHM probe all happen
/// on the overlay thread during startup, so every failure to present is reported here.
pub fn spawn() -> Result<Box<dyn OverlayPresenter>> {
    let (tx, rx) = mpsc::channel::<Command>();
    let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();
    let thread = std::thread::Builder::new()
        .name(THREAD_NAME.to_owned())
        .spawn(move || run(rx, ready_tx))
        .context("spawn the X11 overlay thread")?;

    match ready_rx.recv() {
        Ok(Ok(())) => {
            let presenter: Box<dyn OverlayPresenter> = Box::new(X11Presenter {
                tx,
                thread: Some(thread),
            });
            Ok(presenter)
        }
        Ok(Err(message)) => {
            let _ = thread.join();
            Err(anyhow!("{message}"))
        }
        Err(_) => {
            let _ = thread.join();
            Err(anyhow!(
                "the X11 overlay thread exited before it was ready (Xwayland required)"
            ))
        }
    }
}

/// One command sent to the overlay thread. The thread answers every presentation command, so a
/// failed request is reported to the caller instead of being swallowed.
enum Command {
    Show { frame: OverlayFrame, ack: Ack },
    Draw { frame: OverlayFrame, ack: Ack },
    Hide { ack: Ack },
    Shutdown,
}

/// The overlay thread: connect once, then serve commands until the presenter drops.
fn run(rx: mpsc::Receiver<Command>, ready: mpsc::Sender<std::result::Result<(), String>>) {
    let mut x11 = match Display::connect() {
        Ok(x11) => x11,
        Err(err) => {
            let _ = ready.send(Err(format!("{err:#}")));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        // The presenter is already gone; nothing to present for.
        return;
    }

    while let Ok(command) = rx.recv() {
        match command {
            Command::Show { frame, ack } => {
                let result = x11.show(&frame);
                let _ = ack.send(result);
            }
            Command::Draw { frame, ack } => {
                let result = x11.draw(&frame);
                let _ = ack.send(result);
            }
            Command::Hide { ack } => {
                let result = x11.hide();
                let _ = ack.send(result);
            }
            Command::Shutdown => break,
        }
    }
    x11.shutdown();
}

/// The public presenter: a thin command channel to the overlay thread.
pub struct X11Presenter {
    tx: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl X11Presenter {
    fn request(&self, command: impl FnOnce(Ack) -> Command) -> Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel::<Result<()>>();
        self.tx
            .send(command(ack_tx))
            .map_err(|_| anyhow!("the X11 overlay thread is not running"))?;
        match ack_rx.recv() {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "the X11 overlay thread stopped before it answered a request"
            )),
        }
    }
}

impl OverlayPresenter for X11Presenter {
    fn show(&self, frame: &OverlayFrame) -> Result<()> {
        self.request(|ack| Command::Show {
            frame: frame.clone(),
            ack,
        })
    }

    fn draw(&self, frame: &OverlayFrame) -> Result<()> {
        self.request(|ack| Command::Draw {
            frame: frame.clone(),
            ack,
        })
    }

    fn hide(&self) -> Result<()> {
        self.request(|ack| Command::Hide { ack })
    }
}

impl Drop for X11Presenter {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The interned atoms the overlay needs. `InternAtom` on the name strings, as the contract
/// requires, so the property set is byte-for-byte the one mutter looks up.
struct Atoms {
    window_type: Atom,
    window_type_notification: Atom,
    state: Atom,
    state_above: Atom,
    state_skip_taskbar: Atom,
    state_skip_pager: Atom,
    motif_wm_hints: Atom,
    net_wm_name: Atom,
    net_wm_pid: Atom,
    utf8_string: Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Atoms> {
        Ok(Atoms {
            window_type: intern(conn, b"_NET_WM_WINDOW_TYPE")?,
            window_type_notification: intern(conn, b"_NET_WM_WINDOW_TYPE_NOTIFICATION")?,
            state: intern(conn, b"_NET_WM_STATE")?,
            state_above: intern(conn, b"_NET_WM_STATE_ABOVE")?,
            state_skip_taskbar: intern(conn, b"_NET_WM_STATE_SKIP_TASKBAR")?,
            state_skip_pager: intern(conn, b"_NET_WM_STATE_SKIP_PAGER")?,
            motif_wm_hints: intern(conn, b"_MOTIF_WM_HINTS")?,
            net_wm_name: intern(conn, b"_NET_WM_NAME")?,
            net_wm_pid: intern(conn, b"_NET_WM_PID")?,
            utf8_string: intern(conn, b"UTF8_STRING")?,
        })
    }
}

/// The ARGB (32-bit) visual and its colormap, when the server offers one.
struct ArgbVisual {
    depth: u8,
    visual_id: Visualid,
    colormap: Colormap,
}

/// A server-allocated MIT-SHM segment. The file descriptor is the server's own shared-memory
/// object; writing into it at offset zero is what `shm::put_image` reads back.
struct Segment {
    shmseg: Seg,
    file: File,
    len: usize,
}

impl Segment {
    fn create(conn: &RustConnection, len: usize) -> Result<Segment> {
        let shmseg = conn
            .generate_id()
            .context("allocate an MIT-SHM segment id")?;
        let size = u32::try_from(len).context("overlay image is larger than 4 GiB")?;
        let reply = answered_fd(
            shm::create_segment(conn, shmseg, size, false),
            "create a shared-memory segment (MIT-SHM CreateSegment)",
        )?;
        Ok(Segment {
            shmseg,
            file: File::from(reply.shm_fd),
            len,
        })
    }

    fn write(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.len {
            bail!(
                "overlay image of {} bytes does not fit the {}-byte shared segment",
                bytes.len(),
                self.len
            );
        }
        self.file
            .write_all_at(bytes, 0)
            .context("write the overlay image into the shared-memory segment")
    }
}

/// The X11 side of the overlay: connection, window, shared segment and the once-only failure
/// state. Lives entirely on the overlay thread.
struct Display {
    conn: RustConnection,
    root: Window,
    screen_w: u16,
    screen_h: u16,
    root_depth: u8,
    root_visual: Visualid,
    gc: Gcontext,
    atoms: Atoms,
    argb: Option<ArgbVisual>,
    window: Option<Window>,
    mapped: bool,
    segment: Option<Segment>,
    shape_ok: bool,
    /// Whether a "no RandR geometry, using the whole screen" notice was already logged.
    screen_fallback_logged: bool,
    /// Set once a failure has permanently disabled the overlay; the reason is reported to the
    /// caller on every further call so the app never retries a broken overlay every 100 ms.
    dead: Option<String>,
}

impl Display {
    fn connect() -> Result<Display> {
        let (conn, screen_num) =
            x11rb::connect(None).context("connect to the X server named by $DISPLAY")?;
        let screen = conn
            .setup()
            .roots
            .get(screen_num)
            .context("the X server reported no such screen")?;
        let (root, screen_w, screen_h) =
            (screen.root, screen.width_in_pixels, screen.height_in_pixels);
        let (root_depth, root_visual) = (screen.root_depth, screen.root_visual);
        let argb = create_argb_visual(&conn, screen_num, root)?;

        // MIT-SHM 1.2 is required for `CreateSegment` (the server allocates the segment and hands
        // us the descriptor). 1.2 dates from 2015 and every Xwayland provides it; an older or
        // unavailable extension is reported instead of silently degrading.
        let shm = answered(
            shm::query_version(&conn),
            "query the MIT-SHM extension version",
        )
        .context("the X server provides no MIT-SHM: the X11 overlay needs Xwayland")?;
        if !(shm.major_version > 1 || (shm.major_version == 1 && shm.minor_version >= 2)) {
            bail!(
                "the X server only provides MIT-SHM {}.{}, but the X11 overlay needs 1.2 (CreateSegment): \
                 use Xwayland",
                shm.major_version,
                shm.minor_version
            );
        }

        let shape_ok = match shape::query_version(&conn) {
            Ok(cookie) => cookie.reply().is_ok(),
            Err(_) => false,
        };
        if !shape_ok {
            tracing::warn!(
                "the X server has no SHAPE extension — the overlay cannot set an empty input region, \
                 so pointer input may land on it (keyboard input is unaffected)"
            );
        }

        let gc = conn.generate_id().context("allocate the overlay GC id")?;
        checked(
            xproto::create_gc(&conn, gc, root, &CreateGCAux::default()),
            "create the overlay graphics context",
        )?;

        let atoms = Atoms::intern(&conn)?;
        // Named `x11`, not `display`: the `tracing` macros expand to code that imports
        // `tracing::field::display`, which would shadow a local named `display` in the fields.
        let x11 = Display {
            conn,
            root,
            screen_w,
            screen_h,
            root_depth,
            root_visual,
            gc,
            atoms,
            argb,
            window: None,
            mapped: false,
            segment: None,
            shape_ok,
            screen_fallback_logged: false,
            dead: None,
        };
        tracing::info!(
            display_name = %std::env::var("DISPLAY").unwrap_or_default(),
            root_depth = x11.root_depth,
            argb_visual = x11.argb.is_some(),
            shape = shape_ok,
            "X11 non-activating overlay connected"
        );
        Ok(x11)
    }

    fn show(&mut self, frame: &OverlayFrame) -> Result<()> {
        self.guard()?;
        self.show_inner(frame).map_err(|err| self.fail(err))
    }

    fn draw(&mut self, frame: &OverlayFrame) -> Result<()> {
        self.guard()?;
        self.draw_inner(frame).map_err(|err| self.fail(err))
    }

    fn hide(&mut self) -> Result<()> {
        self.guard()?;
        self.hide_inner().map_err(|err| self.fail(err))
    }

    fn show_inner(&mut self, frame: &OverlayFrame) -> Result<()> {
        let (x, y) = self.target_position()?;
        let window = self.ensure_window(x, y)?;
        // Paint before mapping so the first frame the compositor shows is already the overlay.
        self.paint(window, frame)?;
        if !self.mapped {
            checked(
                xproto::map_window(&self.conn, window),
                "map the overlay window",
            )?;
            self.mapped = true;
            // A window manager may still reposition the window while mapping it; re-asserting the
            // requested position afterwards is what guarantees the overlay sits where RandR says.
            self.move_window(window, x, y)?;
        }
        self.conn
            .flush()
            .context("flush the X11 requests for the overlay")
    }

    fn draw_inner(&mut self, frame: &OverlayFrame) -> Result<()> {
        let Some(window) = self.window else {
            tracing::debug!("draw before show — presenting the overlay instead");
            return self.show_inner(frame);
        };
        self.paint(window, frame)?;
        self.conn
            .flush()
            .context("flush the X11 requests for the overlay")
    }

    fn hide_inner(&mut self) -> Result<()> {
        // Unmapping an unmapped window is a no-op, so no mapping state is needed here.
        if let Some(window) = self.window {
            checked(
                xproto::unmap_window(&self.conn, window),
                "unmap the overlay window",
            )?;
        }
        self.mapped = false;
        self.conn
            .flush()
            .context("flush the X11 requests for the overlay")
    }

    /// Best-effort teardown: the window disappears and the segment is released before the
    /// connection is dropped. Failures here cannot be acted upon, so they are logged only.
    fn shutdown(&mut self) {
        if let Some(window) = self.window {
            checked_or_log(
                xproto::unmap_window(&self.conn, window),
                "unmap the overlay window",
            );
            self.mapped = false;
        }
        if let Some(segment) = self.segment.take() {
            checked_or_log(
                shm::detach(&self.conn, segment.shmseg),
                "detach the overlay shared-memory segment",
            );
        }
        let _ = self.conn.flush();
    }

    fn guard(&self) -> Result<()> {
        match &self.dead {
            Some(reason) => Err(anyhow!("X11 overlay unavailable: {reason}")),
            None => Ok(()),
        }
    }

    /// An overlay failure is not transient: the same request would fail again on the next frame.
    /// Log it once, stop presenting, and let dictation continue (the transcript still reaches the
    /// clipboard and the application via the inserter).
    fn fail(&mut self, err: anyhow::Error) -> anyhow::Error {
        let reason = format!("{err:#}");
        if self.dead.is_none() {
            tracing::warn!(
                error = %reason,
                "X11 overlay failed — the overlay is disabled for this session; dictation, \
                 clipboard and insertion continue"
            );
            self.dead = Some(reason.clone());
        }
        anyhow!("X11 overlay unavailable: {reason}")
    }

    /// The overlay position on every `show`: bottom-centre of the monitor under the pointer,
    /// `BOTTOM_MARGIN` above its bottom edge. RandR is re-read on each call.
    fn target_position(&mut self) -> Result<(i16, i16)> {
        let pointer = answered(
            xproto::query_pointer(&self.conn, self.root),
            "query the pointer position",
        )?;
        let (px, py) = (i32::from(pointer.root_x), i32::from(pointer.root_y));
        let (mw, mh) = (i32::from(OVERLAY_W_PX), i32::from(OVERLAY_H_PX));
        let (mx, my, mon_w, mon_h) = self.monitor_area(px, py);
        let x = mx + (mon_w - mw).max(0) / 2;
        let y = my + (mon_h - mh - overlay_render::BOTTOM_MARGIN).max(0);
        Ok((
            i16::try_from(x).context("the overlay x position does not fit an X11 coordinate")?,
            i16::try_from(y).context("the overlay y position does not fit an X11 coordinate")?,
        ))
    }

    /// The monitor under the pointer, else the primary monitor, else the primary RandR output,
    /// else the whole screen.
    fn monitor_area(&mut self, px: i32, py: i32) -> Rect {
        let monitors = self.monitors();
        if monitors.is_empty() {
            if let Some(rect) = self.primary_output_area() {
                return rect;
            }
            if !self.screen_fallback_logged {
                self.screen_fallback_logged = true;
                tracing::debug!(
                    "no RandR monitor geometry available — placing the overlay on the whole X screen"
                );
            }
            return (0, 0, i32::from(self.screen_w), i32::from(self.screen_h));
        }
        let chosen = monitors
            .iter()
            .find(|monitor| contains(monitor, px, py))
            .or_else(|| monitors.iter().find(|monitor| monitor.primary))
            .or_else(|| monitors.first());
        match chosen {
            Some(monitor) => (
                i32::from(monitor.x),
                i32::from(monitor.y),
                i32::from(monitor.width),
                i32::from(monitor.height),
            ),
            None => (0, 0, i32::from(self.screen_w), i32::from(self.screen_h)),
        }
    }

    /// RandR 1.5 `GetMonitors`, active monitors first. An empty result means "no RandR monitor
    /// information", which the caller handles by falling back.
    fn monitors(&self) -> Vec<MonitorInfo> {
        self.get_monitors(true)
            .or_else(|| self.get_monitors(false))
            .unwrap_or_default()
    }

    fn get_monitors(&self, active: bool) -> Option<Vec<MonitorInfo>> {
        let reply = match randr::get_monitors(&self.conn, self.root, active) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply,
                Err(err) => {
                    tracing::debug!(error = %err, active, "RandR GetMonitors failed");
                    return None;
                }
            },
            Err(err) => {
                tracing::debug!(error = %err, active, "RandR GetMonitors is unavailable");
                return None;
            }
        };
        (!reply.monitors.is_empty()).then_some(reply.monitors)
    }

    /// The primary output's rectangle (RandR 1.2 path for servers without `GetMonitors`).
    fn primary_output_area(&self) -> Option<Rect> {
        let resources = randr::get_screen_resources_current(&self.conn, self.root)
            .ok()?
            .reply()
            .ok()?;
        let primary = randr::get_output_primary(&self.conn, self.root)
            .ok()?
            .reply()
            .ok()?
            .output;
        if primary == 0 {
            return None;
        }
        for crtc in &resources.crtcs {
            let info = randr::get_crtc_info(&self.conn, *crtc, resources.config_timestamp)
                .ok()?
                .reply()
                .ok()?;
            if info.outputs.contains(&primary) {
                if info.width == 0 || info.height == 0 {
                    return None;
                }
                return Some((
                    i32::from(info.x),
                    i32::from(info.y),
                    i32::from(info.width),
                    i32::from(info.height),
                ));
            }
        }
        None
    }

    /// Create the window on first use (moving it on every later `show`), with every window
    /// property set before the window is ever mapped.
    fn ensure_window(&mut self, x: i16, y: i16) -> Result<Window> {
        if let Some(window) = self.window {
            self.move_window(window, x, y)?;
            return Ok(window);
        }

        let window = match self.create_window(x, y) {
            Ok(window) => window,
            Err(error) if self.argb.is_some() => {
                // Some servers refuse a 32-bit child of the root outright (Xwayland does, with
                // BadMatch naming the root window). Degrade to the opaque card on the root
                // visual: every property that FR-011 depends on is unaffected, only the
                // rounded-corner transparency is lost.
                tracing::info!(
                    error = %error,
                    "the X server refused a 32-bit overlay window; falling back to the opaque card"
                );
                self.argb = None;
                self.create_window(x, y)?
            }
            Err(error) => return Err(error),
        };

        self.set_properties(window, i32::from(x), i32::from(y))?;
        self.window = Some(window);
        Ok(window)
    }

    /// Create the (still unmapped) overlay window with the currently selected visual.
    ///
    /// No event mask is selected: the overlay never reacts to events. If the server ever does
    /// discard the window contents, the exposed area is repainted with `background_pixel`
    /// (transparent for a 32-bit visual, the card colour otherwise) instead of garbage, and the
    /// next frame (the app redraws at most 100 ms apart while listening) replaces it.
    fn create_window(&self, x: i16, y: i16) -> Result<Window> {
        let window = self
            .conn
            .generate_id()
            .context("allocate the overlay window id")?;
        let (depth, visual, colormap, background) = match &self.argb {
            Some(argb) => (argb.depth, argb.visual_id, Some(argb.colormap), 0),
            None => (self.root_depth, self.root_visual, None, CARD_PIXEL),
        };
        let mut aux = CreateWindowAux::new().background_pixel(background);
        if let Some(colormap) = colormap {
            aux = aux.colormap(colormap);
        }
        checked(
            xproto::create_window(
                &self.conn,
                depth,
                window,
                self.root,
                x,
                y,
                OVERLAY_W_PX,
                OVERLAY_H_PX,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &aux,
            ),
            "create the overlay window",
        )?;
        Ok(window)
    }

    /// Put the (already created) window at the resolved position and size.
    fn move_window(&self, window: Window, x: i16, y: i16) -> Result<()> {
        let aux = ConfigureWindowAux::new()
            .x(i32::from(x))
            .y(i32::from(y))
            .width(u32::from(OVERLAY_W_PX))
            .height(u32::from(OVERLAY_H_PX));
        checked(
            xproto::configure_window(&self.conn, window, &aux),
            "move the overlay window",
        )
    }

    /// Everything that decides whether mutter treats the window as non-activating. Called once,
    /// before the first `map_window`, when the window is still unmapped.
    fn set_properties(&self, window: Window, x: i32, y: i32) -> Result<()> {
        let atoms = &self.atoms;
        set_property32(
            &self.conn,
            window,
            "_NET_WM_WINDOW_TYPE",
            atoms.window_type,
            AtomEnum::ATOM.into(),
            &[atoms.window_type_notification],
        )?;
        checked(
            WmHints {
                input: Some(false),
                ..WmHints::new()
            }
            .set(&self.conn, window),
            "set WM_HINTS (input = False) on the overlay window",
        )?;
        set_property32(
            &self.conn,
            window,
            "_NET_WM_STATE",
            atoms.state,
            AtomEnum::ATOM.into(),
            &[
                atoms.state_above,
                atoms.state_skip_taskbar,
                atoms.state_skip_pager,
            ],
        )?;
        set_property32(
            &self.conn,
            window,
            "_MOTIF_WM_HINTS",
            atoms.motif_wm_hints,
            atoms.motif_wm_hints,
            &[MWM_HINTS_DECORATIONS, 0, 0, 0, 0],
        )?;
        set_property8(
            &self.conn,
            window,
            "WM_CLASS",
            AtomEnum::WM_CLASS.into(),
            AtomEnum::STRING.into(),
            b"littera\0Littera\0",
        )?;
        // `xprop -name littera` matches WM_NAME, which is also what a WM shows in any window list.
        set_property8(
            &self.conn,
            window,
            "WM_NAME",
            AtomEnum::WM_NAME.into(),
            AtomEnum::STRING.into(),
            b"littera",
        )?;
        set_property8(
            &self.conn,
            window,
            "_NET_WM_NAME",
            atoms.net_wm_name,
            atoms.utf8_string,
            b"littera",
        )?;
        set_property32(
            &self.conn,
            window,
            "_NET_WM_PID",
            atoms.net_wm_pid,
            AtomEnum::CARDINAL.into(),
            &[std::process::id()],
        )?;
        checked(
            WmSizeHints {
                position: Some((WmSizeHintsSpecification::UserSpecified, x, y)),
                ..WmSizeHints::new()
            }
            .set_normal_hints(&self.conn, window),
            "set WM_NORMAL_HINTS (USPosition) on the overlay window",
        )?;
        if self.shape_ok {
            // An empty input region: the pointer and all button/key events pass through to
            // whatever is below the overlay.
            checked(
                shape::rectangles(
                    &self.conn,
                    SO::SET,
                    SK::INPUT,
                    ClipOrdering::UNSORTED,
                    window,
                    0,
                    0,
                    &[],
                ),
                "set an empty input region on the overlay window",
            )?;
        }
        Ok(())
    }

    /// Render `frame` and push it into the window through the shared segment.
    fn paint(&mut self, window: Window, frame: &OverlayFrame) -> Result<()> {
        let pixmap = overlay_render::render(frame, SCALE).context("render the overlay frame")?;
        let width = u16::try_from(pixmap.width())
            .context("the rendered overlay is too wide for an X11 request")?;
        let height = u16::try_from(pixmap.height())
            .context("the rendered overlay is too tall for an X11 request")?;
        if width != OVERLAY_W_PX || height != OVERLAY_H_PX {
            bail!(
                "the overlay renderer produced a {width}×{height} image, expected {OVERLAY_W_PX}×{OVERLAY_H_PX} \
                 at scale {SCALE}"
            );
        }
        let mut bytes = overlay_render::premultiplied_bgra(&pixmap);
        let expected = usize::from(width) * usize::from(height) * 4;
        if bytes.len() != expected {
            bail!(
                "the overlay renderer produced {} bytes for a {width}×{height} image",
                bytes.len()
            );
        }
        if self.argb.is_none() {
            // A 24-bit window ignores alpha: make the image opaque against the card colour.
            composite_over_card(&mut bytes);
        }

        let shmseg = self.upload(&bytes)?;
        let depth = match &self.argb {
            Some(argb) => argb.depth,
            None => self.root_depth,
        };
        checked(
            shm::put_image(
                &self.conn,
                window,
                self.gc,
                width,
                height,
                0,
                0,
                width,
                height,
                0,
                0,
                depth,
                u8::from(ImageFormat::Z_PIXMAP),
                false,
                shmseg,
                0,
            ),
            "upload the overlay image (MIT-SHM PutImage)",
        )
    }

    /// Write `bytes` into the segment, creating or resizing it as needed, and return its id.
    fn upload(&mut self, bytes: &[u8]) -> Result<Seg> {
        let segment = match self.segment.take() {
            Some(segment) if segment.len == bytes.len() => segment,
            Some(segment) => {
                if let Err(err) = checked(
                    shm::detach(&self.conn, segment.shmseg),
                    "detach the previous overlay shared-memory segment",
                ) {
                    tracing::debug!(error = %err, "could not detach the previous overlay segment");
                }
                Segment::create(&self.conn, bytes.len())?
            }
            None => Segment::create(&self.conn, bytes.len())?,
        };
        segment.write(bytes)?;
        let shmseg = segment.shmseg;
        self.segment = Some(segment);
        Ok(shmseg)
    }
}

/// The screen's 32-bit visual, if it has one. A colormap is installed for it so the window can be
/// created with that depth; the root visual (already usable with the default colormap) is used
/// when the screen itself is 32 bits deep.
fn create_argb_visual(
    conn: &RustConnection,
    screen_num: usize,
    root: Window,
) -> Result<Option<ArgbVisual>> {
    let screen = conn
        .setup()
        .roots
        .get(screen_num)
        .context("the X server reported no such screen for the overlay visual lookup")?;
    if screen.root_depth == 32 {
        let colormap = screen.default_colormap;
        return Ok(Some(ArgbVisual {
            depth: 32,
            visual_id: screen.root_visual,
            colormap,
        }));
    }
    let visual = screen
        .allowed_depths
        .iter()
        .filter(|depth| depth.depth == 32)
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| {
            visual.class == VisualClass::TRUE_COLOR || visual.class == VisualClass::DIRECT_COLOR
        });
    let Some(visual) = visual.copied() else {
        return Ok(None);
    };
    let colormap = conn
        .generate_id()
        .context("allocate the ARGB colormap id")?;
    checked(
        xproto::create_colormap(conn, ColormapAlloc::NONE, colormap, root, visual.visual_id),
        "create the colormap for the 32-bit overlay visual",
    )?;
    Ok(Some(ArgbVisual {
        depth: 32,
        visual_id: visual.visual_id,
        colormap,
    }))
}

/// Composite the renderer's premultiplied BGRA output over the opaque card colour, so a 24-bit
/// window (whose pixels carry no alpha) shows the card instead of black where the renderer
/// intended transparency.
fn composite_over_card(bgra: &mut [u8]) {
    let (r, g, b) = CARD_RGB;
    for pixel in bgra.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        let backdrop = |channel: u8| ((u32::from(channel) * (255 - alpha)) / 255) as u8;
        pixel[0] = pixel[0].saturating_add(backdrop(b));
        pixel[1] = pixel[1].saturating_add(backdrop(g));
        pixel[2] = pixel[2].saturating_add(backdrop(r));
        pixel[3] = 255;
    }
}

/// Whether a RandR monitor rectangle contains the given root coordinates.
fn contains(monitor: &MonitorInfo, x: i32, y: i32) -> bool {
    let (mx, my) = (i32::from(monitor.x), i32::from(monitor.y));
    x >= mx && y >= my && x < mx + i32::from(monitor.width) && y < my + i32::from(monitor.height)
}

fn intern(conn: &RustConnection, name: &[u8]) -> Result<Atom> {
    let label = String::from_utf8_lossy(name).into_owned();
    let reply = answered(
        xproto::intern_atom(conn, false, name),
        &format!("intern the {label} atom"),
    )?;
    Ok(reply.atom)
}

fn set_property32(
    conn: &RustConnection,
    window: Window,
    name: &str,
    property: Atom,
    type_: Atom,
    values: &[u32],
) -> Result<()> {
    checked(
        conn.change_property32(PropMode::REPLACE, window, property, type_, values),
        &format!("set the {name} window property"),
    )
}

fn set_property8(
    conn: &RustConnection,
    window: Window,
    name: &str,
    property: Atom,
    type_: Atom,
    data: &[u8],
) -> Result<()> {
    checked(
        conn.change_property8(PropMode::REPLACE, window, property, type_, data),
        &format!("set the {name} window property"),
    )
}

/// Send a request whose only interesting result is a protocol error.
fn checked(
    cookie: Result<VoidCookie<'_, RustConnection>, ConnectionError>,
    what: &str,
) -> Result<()> {
    cookie
        .map_err(anyhow::Error::from)?
        .check()
        .with_context(|| what.to_owned())
}

/// [`checked`] for teardown paths, where there is nothing left to do with the error.
fn checked_or_log(cookie: Result<VoidCookie<'_, RustConnection>, ConnectionError>, what: &str) {
    if let Err(err) = checked(cookie, what) {
        tracing::debug!(error = %err, what, "X11 teardown request failed");
    }
}

/// Send a request and wait for its reply.
fn answered<T: TryParse>(
    cookie: Result<Cookie<'_, RustConnection, T>, ConnectionError>,
    what: &str,
) -> Result<T> {
    cookie
        .map_err(anyhow::Error::from)?
        .reply()
        .with_context(|| what.to_owned())
}

/// Send a request whose reply carries a file descriptor and wait for it.
fn answered_fd<T: TryParseFd>(
    cookie: Result<CookieWithFds<'_, RustConnection, T>, ConnectionError>,
    what: &str,
) -> Result<T> {
    cookie
        .map_err(anyhow::Error::from)?
        .reply()
        .with_context(|| what.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::overlay::{self, OverlayBackend, OverlayFrame, OverlayState};
    use std::time::Duration;

    /// Depth-first search for the window carrying `_NET_WM_PID == pid`.
    ///
    /// Xwayland's window manager reparents client windows, so the overlay is a
    /// descendant of the root, not necessarily a direct child.
    fn find_window_by_pid(
        conn: &x11rb::rust_connection::RustConnection,
        root: Window,
        pid: u32,
    ) -> Option<Window> {
        use x11rb::protocol::xproto::ConnectionExt as _;

        let net_pid = conn
            .intern_atom(false, b"_NET_WM_PID")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let mut stack = vec![root];

        while let Some(window) = stack.pop() {
            if window != root {
                let owned = xproto::get_property(conn, false, window, net_pid, 0u32, 0, 1)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok());
                if let Some(reply) = owned
                    && reply
                        .value32()
                        .is_some_and(|mut values| values.any(|value| value == pid))
                {
                    return Some(window);
                }
            }

            if let Some(reply) = xproto::query_tree(conn, window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
            {
                stack.extend(reply.children);
            }
        }

        None
    }

    fn frame(level: f32, text: &str) -> OverlayFrame {
        OverlayFrame {
            state: OverlayState::Recording,
            elapsed: Duration::from_secs(3),
            level,
            text: text.to_owned(),
        }
    }

    #[test]
    #[ignore = "needs an X server on DISPLAY (XWayland counts)"]
    fn x11_overlay_is_a_non_activating_notification_window() {
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("skipping: DISPLAY is not set");
            return;
        }

        use x11rb::connection::Connection as _;
        use x11rb::protocol::shape::ConnectionExt as _;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
        use x11rb::protocol::{shape, xproto};

        let presenter = overlay::spawn(OverlayBackend::X11).expect("x11 overlay presenter");
        let shown = frame(0.4, "Testing the X11 non-activating overlay window.");
        presenter.show(&shown).expect("show");
        std::thread::sleep(Duration::from_millis(300));

        let (connection, screen) = x11rb::connect(None).expect("connect to the X server");
        let root = connection.setup().roots[screen].root;

        let intern = |name: &str| -> u32 {
            connection
                .intern_atom(false, name.as_bytes())
                .expect("intern_atom")
                .reply()
                .expect("intern_atom reply")
                .atom
        };

        let wm_class_atom = intern("WM_CLASS");
        let wm_hints_atom = intern("WM_HINTS");
        let window_type_atom = intern("_NET_WM_WINDOW_TYPE");
        let notification_atom = intern("_NET_WM_WINDOW_TYPE_NOTIFICATION");

        // Find our window: Xwayland's window manager reparents client windows, so the overlay is
        // a descendant of the root window rather than necessarily a direct child.
        let window = find_window_by_pid(&connection, root, std::process::id())
            .expect("the overlay window (_NET_WM_PID = this process) must exist while shown");

        let class = xproto::get_property(
            &connection,
            false,
            window,
            wm_class_atom,
            AtomEnum::STRING,
            0,
            64,
        )
        .expect("get WM_CLASS")
        .reply()
        .expect("WM_CLASS reply");
        assert!(
            String::from_utf8_lossy(&class.value)
                .to_ascii_lowercase()
                .contains("littera"),
            "the overlay window must identify as littera, got {:?}",
            class.value
        );

        // 1. It is a notification window: mutter then keeps it above and non-focusable.
        let types = connection
            .get_property(false, window, window_type_atom, AtomEnum::ATOM, 0, 16)
            .expect("get _NET_WM_WINDOW_TYPE")
            .reply()
            .expect("_NET_WM_WINDOW_TYPE reply")
            .value32()
            .expect("_NET_WM_WINDOW_TYPE is a list of atoms")
            .collect::<Vec<u32>>();
        assert!(
            types.contains(&notification_atom),
            "expected _NET_WM_WINDOW_TYPE_NOTIFICATION, got {types:?}"
        );

        // 2. WM_HINTS.input == False (meta_window_x11_is_focusable() == FALSE).
        let hints = connection
            .get_property(false, window, wm_hints_atom, wm_hints_atom, 0, 16)
            .expect("get WM_HINTS")
            .reply()
            .expect("WM_HINTS reply")
            .value32()
            .expect("WM_HINTS is CARD32s")
            .collect::<Vec<u32>>();
        assert!(hints.len() >= 3, "WM_HINTS must be set, got {hints:?}");
        assert_eq!(hints[0] & 1, 1, "WM_HINTS must carry the InputHint flag");
        assert_eq!(hints[2], 0, "WM_HINTS.input must be False");

        // 3. The input shape is empty, so pointer input passes through.
        let rectangles = connection
            .shape_get_rectangles(window, shape::SK::INPUT)
            .expect("shape_get_rectangles")
            .reply()
            .expect("shape_get_rectangles reply");
        assert!(
            rectangles.rectangles.is_empty(),
            "the input region must be empty, got {:?}",
            rectangles.rectangles
        );

        // 4. It is actually on screen, and the request never took focus.
        let attributes = connection
            .get_window_attributes(window)
            .expect("get_window_attributes")
            .reply()
            .expect("get_window_attributes reply");
        assert_eq!(attributes.map_state, xproto::MapState::VIEWABLE);

        let focus = connection
            .get_input_focus()
            .expect("get_input_focus")
            .reply()
            .expect("get_input_focus reply");
        assert_ne!(
            focus.focus, window,
            "the overlay must never be the input focus"
        );

        // A redraw on the live window must still succeed.
        presenter.draw(&frame(1.0, "Redrawn")).expect("draw");
        std::thread::sleep(Duration::from_millis(100));
        presenter.hide().expect("hide");
    }

    #[test]
    fn compositing_replaces_transparency_with_the_card_colour() {
        // transparent pixel, then an opaque blue pixel (BGRA)
        let mut bytes = vec![0, 0, 0, 0, 0xFF, 0x00, 0x00, 0xFF];
        composite_over_card(&mut bytes);
        assert_eq!(&bytes[0..4], &[0x24, 0x1E, 0x1E, 0xFF]);
        assert_eq!(&bytes[4..8], &[0xFF, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn compositing_blends_partial_alpha_towards_the_card_colour() {
        // premultiplied mid-grey at 50 % alpha over #1E1E24
        let mut bytes = vec![0x80, 0x80, 0x80, 0x80];
        composite_over_card(&mut bytes);
        assert_eq!(bytes[3], 255);
        // BGRA order: B == 0x80 + 0x24 * 127 / 255 == 128 + 17, G/R == 0x80 + 0x1E * 127 / 255 == 142.
        assert_eq!(&bytes[0..3], &[145, 142, 142]);
    }

    #[test]
    fn monitor_rectangle_containment_is_half_open() {
        let monitor = MonitorInfo {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
            ..MonitorInfo::default()
        };
        assert!(contains(&monitor, -1920, 0));
        assert!(contains(&monitor, -1, 1079));
        assert!(!contains(&monitor, 0, 0));
        assert!(!contains(&monitor, -1920, 1080));
    }
}
