//! Layer-shell overlay presenter for compositors that implement `zwlr_layer_shell_v1`
//! (COSMIC, KDE, Hyprland, …).
//!
//! The presenter owns a dedicated thread with its own `wayland_client::Connection` and a `calloop`
//! event loop that carries two sources: the Wayland socket and a command channel fed by the
//! [`OverlayPresenter`] methods.
//!
//! The indicator surface is created for every dictation and destroyed again on `hide`. A freshly
//! mapped layer surface is placed by the compositor on the output that currently has keyboard
//! focus, which is why the surface is created without an output. It reserves no space
//! (`set_exclusive_zone(0)`), never takes keyboard focus (`KeyboardInteractivity::None`) and does
//! not accept pointer input (an empty input region), so the user keeps typing into the focused
//! application while the indicator is visible.

use std::sync::mpsc::{self, Sender as StdSender};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, anyhow};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region, SurfaceData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            Error as LoopError, EventLoop, LoopHandle, RegistrationToken,
            channel::{Channel, Event as ChannelEvent, Sender as ChannelSender, channel},
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, EventQueue, Proxy, QueueHandle,
    globals::{BindError, Global, GlobalList, registry_queue_init},
    protocol::{wl_output, wl_shm, wl_surface},
};

use crate::overlay::{OverlayFrame, OverlayPresenter};
use crate::overlay_render;

/// The overlay event loop together with the state its sources dispatch into.
type OverlayLoop = (EventLoop<'static, LayerLoop>, LayerLoop);

/// Namespace of the indicator surface; compositors show it as the surface's app id.
const NAMESPACE: &str = "littera-overlay";

/// Reported when the compositor does not implement layer-shell at all.
const LAYER_SHELL_MISSING: &str =
    "zwlr_layer_shell_v1 is not available on this compositor, so the overlay cannot be created";

/// Reported when the Wayland display cannot be reached.
const CONNECT_ERROR: &str = "cannot connect to the Wayland display (is WAYLAND_DISPLAY set?)";

/// Reported when the compositor globals cannot be enumerated.
const REGISTRY_ERROR: &str = "cannot enumerate the Wayland globals";

/// Reported when the overlay's own event loop cannot be created.
const EVENT_LOOP_ERROR: &str = "cannot create the overlay event loop";

/// Reported when the empty input region cannot be created.
const INPUT_REGION_ERROR: &str = "cannot create the empty wl_region for the overlay input region";

/// How long [`spawn`] waits for the presenter thread to complete its registry roundtrip.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a `show`/`draw`/`hide` call waits for the presenter thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Back-off used when the event loop keeps failing although the Wayland source is already gone.
const FAILED_POLL_BACKOFF: Duration = Duration::from_millis(100);

/// Reported when the presenter thread does not finish its startup in time.
const STARTUP_TIMEOUT_ERROR: &str =
    "the overlay presenter thread did not finish its Wayland registry roundtrip in time";

/// Reported when the presenter thread dies during startup.
const STARTUP_ABORTED_ERROR: &str = "the overlay presenter thread stopped during startup";

/// Reported when the presenter thread dies while a command is in flight.
const REPLY_ABORTED_ERROR: &str = "the overlay presenter thread stopped before answering";

/// The compositor advertises `zwlr_layer_shell_v1`, so the layer-shell overlay can be presented.
pub fn available() -> bool {
    match probe() {
        Ok(present) => present,
        Err(err) => {
            tracing::debug!(error = %format!("{err:#}"), "cannot probe the layer-shell overlay");
            false
        }
    }
}

/// Spawn the presenter thread.
///
/// Returns only after the thread has completed its registry roundtrip and bound every global it
/// needs, so a compositor without `zwlr_layer_shell_v1` is an error instead of a presenter that
/// silently does nothing.
pub fn spawn() -> anyhow::Result<Box<dyn OverlayPresenter>> {
    // Pre-flight on the caller's thread: a compositor without layer-shell is reported before a
    // thread and a second connection are created. The thread repeats the check on its own
    // connection, where the answer stays authoritative.
    if !available() {
        return Err(anyhow!(LAYER_SHELL_MISSING));
    }

    let (commands, channel) = channel::<OverlayCmd>();
    let (ready, ready_rx) = mpsc::channel::<Result<(), String>>();
    let builder = std::thread::Builder::new().name("littera-overlay".to_owned());
    let spawned = builder.spawn(move || start_thread(ready, channel));
    let thread = spawned.context("cannot spawn the overlay presenter thread")?;

    let seconds = STARTUP_TIMEOUT.as_secs();
    let presenter = match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
        Ok(Ok(())) => Ok(WaylandOverlay {
            commands,
            _thread: thread,
        }),
        Ok(Err(message)) => Err(anyhow!("{message}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(startup_timeout_error(seconds)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(STARTUP_ABORTED_ERROR)),
    };
    let presenter: Box<dyn OverlayPresenter> = Box::new(presenter?);
    Ok(presenter)
}

/// The error reported when the presenter thread did not finish its startup in time.
fn startup_timeout_error(seconds: u64) -> anyhow::Error {
    anyhow!("{STARTUP_TIMEOUT_ERROR} ({seconds} s)")
}

// --- presenter thread --------------------------------------------------------------------------

fn start_thread(ready: StdSender<Result<(), String>>, commands: Channel<OverlayCmd>) {
    match run(ready, commands) {
        Ok(()) => tracing::debug!("overlay presenter thread finished"),
        Err(err) => tracing::warn!(error = %format!("{err:#}"), "overlay presenter thread stopped"),
    }
}

fn run(ready: StdSender<Result<(), String>>, commands: Channel<OverlayCmd>) -> anyhow::Result<()> {
    let (mut event_loop, mut state) = match setup(commands) {
        Ok(setup) => setup,
        Err(err) => {
            // The caller blocks on this channel, so it always has to be answered.
            if ready.send(Err(format!("{err:#}"))).is_err() {
                tracing::debug!("the overlay presenter caller disappeared during startup");
            }
            return Err(err);
        }
    };

    if ready.send(Ok(())).is_err() {
        tracing::debug!("the overlay presenter caller disappeared during startup");
        return Ok(());
    }

    while !state.exit {
        if let Err(err) = event_loop.dispatch(None, &mut state) {
            state.note_event_loop_failure(err);
        }
    }
    Ok(())
}

/// Connect, bind every global the overlay needs and build the event loop with both of its sources.
fn setup(commands: Channel<OverlayCmd>) -> anyhow::Result<OverlayLoop> {
    let connected = Connection::connect_to_env();
    let connection = connected.context(CONNECT_ERROR)?;
    let enumerated = registry_queue_init::<LayerLoop>(&connection);
    let (globals, queue) = enumerated.context(REGISTRY_ERROR)?;

    if !layer_shell_available(&globals) {
        return Err(anyhow!(LAYER_SHELL_MISSING));
    }
    if fractional_scale_advertised(&globals) {
        tracing::warn!(
            "the compositor advertises fractional output scaling \
             (wp_fractional_scale_manager_v1): the overlay renders at the rounded integer output \
             scale, so fractional scaling is not pixel-exact"
        );
    }

    let qh = queue.handle();
    let compositor = bind_global(CompositorState::bind(&globals, &qh), "wl_compositor")?;
    let shm = bind_global(Shm::bind(&globals, &qh), "wl_shm")?;
    let layer_shell = bind_global(LayerShell::bind(&globals, &qh), "zwlr_layer_shell_v1")?;
    let output_state = OutputState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);

    let created = EventLoop::try_new();
    let mut event_loop: EventLoop<LayerLoop> = created.context(EVENT_LOOP_ERROR)?;
    let wayland_source = register_sources(&mut event_loop, &connection, queue, commands)?;

    let state = LayerLoop {
        registry_state,
        compositor,
        shm,
        layer_shell,
        output_state,
        qh,
        loop_handle: event_loop.handle(),
        wayland_source: Some(wayland_source),
        layer: None,
        pool: None,
        buffers: Vec::new(),
        next_buffer: 0,
        configure_size: (overlay_render::OVERLAY_W, overlay_render::OVERLAY_H),
        scale: 1,
        scale_supported: true,
        configured: false,
        frame: None,
        pending_error: None,
        fatal: None,
        exit: false,
    };

    tracing::info!(namespace = NAMESPACE, "layer-shell overlay presenter ready");
    Ok((event_loop, state))
}

/// Bind a global that the overlay cannot work without.
fn bind_global<T>(bound: Result<T, BindError>, interface: &str) -> anyhow::Result<T> {
    bound.map_err(|err| anyhow!("{interface} is not available on this compositor: {err}"))
}

/// Insert the Wayland socket and the command channel into `event_loop`; the returned registration
/// token is what lets a dead connection be dropped from the loop again.
fn register_sources(
    event_loop: &mut EventLoop<'static, LayerLoop>,
    connection: &Connection,
    queue: EventQueue<LayerLoop>,
    commands: Channel<OverlayCmd>,
) -> anyhow::Result<RegistrationToken> {
    let loop_handle = event_loop.handle();
    let owned_connection = Connection::clone(connection);
    let source = WaylandSource::new(owned_connection, queue);
    let token = match source.insert(loop_handle.clone()) {
        Ok(token) => token,
        Err(err) => {
            let reason = err.error;
            return Err(anyhow!("cannot watch the Wayland socket: {reason}"));
        }
    };
    if let Err(err) = loop_handle.insert_source(commands, handle_channel_event) {
        let reason = err.error;
        return Err(anyhow!(
            "cannot watch the overlay command channel: {reason}"
        ));
    }
    Ok(token)
}

fn handle_channel_event(
    event: ChannelEvent<OverlayCmd>,
    _metadata: &mut (),
    state: &mut LayerLoop,
) {
    match event {
        ChannelEvent::Msg(command) => state.handle_command(command),
        ChannelEvent::Closed => state.exit = true,
    }
}

fn probe() -> anyhow::Result<bool> {
    let connected = Connection::connect_to_env();
    let connection = connected.context(CONNECT_ERROR)?;
    let enumerated = registry_queue_init::<LayerLoop>(&connection);
    let (globals, _queue) = enumerated.context(REGISTRY_ERROR)?;
    Ok(layer_shell_available(&globals))
}

fn layer_shell_available(globals: &GlobalList) -> bool {
    let contents = globals.contents();
    contents.with_list(|list| list.iter().any(is_layer_shell))
}

fn fractional_scale_advertised(globals: &GlobalList) -> bool {
    let contents = globals.contents();
    contents.with_list(|list| list.iter().any(is_fractional_scale))
}

fn is_layer_shell(global: &Global) -> bool {
    global.interface == "zwlr_layer_shell_v1"
}

fn is_fractional_scale(global: &Global) -> bool {
    global.interface == "wp_fractional_scale_manager_v1"
}

// --- event loop state --------------------------------------------------------------------------

/// Everything the overlay's event loop owns: the bound globals, the surface and its buffers.
struct LayerLoop {
    registry_state: RegistryState,
    compositor: CompositorState,
    shm: Shm,
    layer_shell: LayerShell,
    output_state: OutputState,
    qh: QueueHandle<LayerLoop>,
    loop_handle: LoopHandle<'static, LayerLoop>,
    /// Registration token of the Wayland source, dropped when the connection is unusable.
    wayland_source: Option<RegistrationToken>,
    /// The mapped surface, `None` while nothing is presented.
    layer: Option<LayerSurface>,
    pool: Option<SlotPool>,
    /// Two buffers: the compositor scans one while the other one is rendered into.
    buffers: Vec<Buffer>,
    next_buffer: usize,
    /// Surface-local size the compositor asked for in the last configure.
    configure_size: (u32, u32),
    /// Integer buffer scale of the output the surface is on.
    scale: i32,
    /// Whether the surface supports `set_buffer_scale`, which needs wl_surface version 3.
    scale_supported: bool,
    configured: bool,
    frame: Option<OverlayFrame>,
    /// A failure that happened while events were dispatched; reported by the next command.
    pending_error: Option<String>,
    /// Unrecoverable failure: the Wayland connection is unusable.
    fatal: Option<String>,
    exit: bool,
}

impl LayerLoop {
    fn handle_command(&mut self, command: OverlayCmd) {
        match command {
            OverlayCmd::Show(frame, reply) => {
                let result = self.show(&frame);
                send_reply(reply, result);
            }
            OverlayCmd::Draw(frame, reply) => {
                let result = self.draw(&frame);
                send_reply(reply, result);
            }
            OverlayCmd::Hide(reply) => {
                let result = self.hide();
                send_reply(reply, result);
            }
        }
    }

    fn show(&mut self, frame: &OverlayFrame) -> anyhow::Result<()> {
        if let Some(message) = self.take_failure() {
            return Err(anyhow!("{message}"));
        }
        self.frame = Some(frame.clone());
        // A fresh surface per dictation: a newly mapped layer surface is placed on the output with
        // keyboard focus, and the previous surface must not linger anywhere.
        self.teardown();

        let surface = self.compositor.create_surface(&self.qh);
        self.scale_supported = surface.version() >= 3;
        let shell = &self.layer_shell;
        let qh = &self.qh;
        let layer = shell.create_layer_surface(qh, surface, Layer::Overlay, Some(NAMESPACE), None);

        layer.set_anchor(Anchor::BOTTOM);
        layer.set_size(overlay_render::OVERLAY_W, overlay_render::OVERLAY_H);
        layer.set_margin(0, 0, overlay_render::BOTTOM_MARGIN, 0);
        // Reserving no space keeps the other windows from moving when the indicator appears.
        layer.set_exclusive_zone(0);
        // Never take keyboard focus: the user keeps typing into the focused application.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);

        // A `wl_region` without rectangles is an empty input region, so pointer input falls
        // through. The compositor copies the region, so it can be destroyed right away.
        let region = Region::new(&self.compositor).context(INPUT_REGION_ERROR)?;
        layer.set_input_region(Some(region.wl_region()));
        drop(region);

        // Double-buffered surface state: the scale set here is only a guess, because the output the
        // compositor picks is unknown until the surface has entered it. The authoritative scale is
        // applied in `configure`/`present` before the first buffer is attached.
        self.apply_buffer_scale(&layer);

        // The initial commit without a buffer makes the compositor send the configure that tells us
        // the surface size; the first frame is rendered from there.
        layer.commit();

        self.layer = Some(layer);
        self.configured = false;
        self.configure_size = (overlay_render::OVERLAY_W, overlay_render::OVERLAY_H);
        Ok(())
    }

    fn draw(&mut self, frame: &OverlayFrame) -> anyhow::Result<()> {
        if let Some(message) = self.take_failure() {
            return Err(anyhow!("{message}"));
        }
        self.frame = Some(frame.clone());
        if !self.configured {
            // The configure handler renders the frame that is stored now; nothing to present yet.
            return Ok(());
        }
        self.present(frame)
    }

    fn hide(&mut self) -> anyhow::Result<()> {
        self.frame = None;
        self.teardown();
        // Teardown always succeeds locally, but a dead connection is still reported to the caller.
        match self.take_failure() {
            Some(message) => Err(anyhow!("{message}")),
            None => Ok(()),
        }
    }

    /// Release the surface and its buffers; the next `show` creates both again.
    fn teardown(&mut self) {
        self.buffers.clear();
        self.pool = None;
        self.layer = None;
        self.configured = false;
        self.next_buffer = 0;
    }

    /// Render `frame` into the spare buffer and present it.
    fn present(&mut self, frame: &OverlayFrame) -> anyhow::Result<()> {
        let Some(layer) = self.layer.as_ref() else {
            return Ok(());
        };
        if self.buffers.is_empty() {
            return Ok(());
        }

        let scale = self.effective_scale();
        let pixmap = overlay_render::render(frame, scale)?;
        let pixels = overlay_render::premultiplied_bgra(&pixmap);

        let (width, height) = self.configure_size;
        let buffer_width = width.max(1).saturating_mul(scale as u32);
        let buffer_height = height.max(1).saturating_mul(scale as u32);
        let stride = buffer_width as usize * 4;

        let index = self.next_buffer % self.buffers.len();
        let buffer = &self.buffers[index];
        let Some(pool) = self.pool.as_mut() else {
            return Ok(());
        };
        // A buffer the compositor is still scanning has no canvas; the frame committed before stays
        // on screen and the next draw retries with the other buffer.
        let Some(canvas) = pool.canvas(buffer) else {
            return Ok(());
        };
        let (pixmap_width, pixmap_height) = (pixmap.width() as usize, pixmap.height() as usize);
        blit(&pixels, pixmap_width, pixmap_height, canvas, stride);

        self.apply_buffer_scale(layer);
        if buffer.attach_to(layer.wl_surface()).is_err() {
            // The compositor took the buffer back between the canvas check and the attach; the
            // frame committed before stays on screen and the next draw retries.
            return Ok(());
        }
        damage(layer, (width, height), (buffer_width, buffer_height));
        layer.commit();
        self.next_buffer = index + 1;
        Ok(())
    }

    /// Allocate the two `wl_shm` buffers for a surface of `width` × `height` at `scale`.
    fn build_buffers(&mut self, width: u32, height: u32, scale: i32) -> anyhow::Result<()> {
        let scale = scale.max(1) as u32;
        let buffer_width = width.max(1).saturating_mul(scale) as i32;
        let buffer_height = height.max(1).saturating_mul(scale) as i32;
        let stride = buffer_width * 4;
        let bytes = (stride as usize).saturating_mul(buffer_height as usize);

        // Both buffers are allocated up front, so the pool never has to be resized (which re-maps
        // it) while the compositor holds a buffer.
        let mut pool = SlotPool::new(bytes * 2, &self.shm).context("cannot create the shm pool")?;
        let mut buffers = Vec::with_capacity(2);
        for index in 0..2 {
            let format = wl_shm::Format::Argb8888;
            let created = pool.create_buffer(buffer_width, buffer_height, stride, format);
            let (buffer, _canvas) =
                created.with_context(|| format!("no overlay buffer {index}"))?;
            buffers.push(buffer);
        }

        self.buffers = buffers;
        self.pool = Some(pool);
        self.next_buffer = 0;
        Ok(())
    }

    /// Buffer scale the surface is rendered at; `set_buffer_scale` needs wl_surface version 3.
    fn effective_scale(&self) -> i32 {
        if self.scale_supported {
            self.scale.max(1)
        } else {
            1
        }
    }

    fn apply_buffer_scale(&self, layer: &LayerSurface) {
        if !self.scale_supported {
            // Without buffer scale support the overlay is rendered at scale 1.
            return;
        }
        if layer
            .set_buffer_scale(self.effective_scale() as u32)
            .is_err()
        {
            tracing::debug!("the overlay layer surface refused the buffer scale request");
        }
    }

    /// Scale of the output the compositor placed the surface on.
    fn surface_scale(&self, layer: &LayerSurface) -> i32 {
        // Since wl_surface v6 the compositor reports its preferred buffer scale directly; for older
        // surfaces sctk derives it from the scale of the outputs the surface entered. The scale is
        // an integer, so a fractional output scale is rounded to the nearest step.
        let tracked = layer
            .wl_surface()
            .data::<SurfaceData<()>>()
            .map(|data| data.scale_factor())
            .filter(|scale| *scale > 0);
        tracked.unwrap_or_else(|| self.scale.max(1))
    }

    fn update_scale(&mut self, factor: i32) {
        let factor = factor.max(1);
        if factor == self.scale {
            return;
        }
        tracing::debug!(scale = factor, "overlay buffer scale changed");
        self.scale = factor;
        if !self.configured {
            // The first buffer is built by the configure handler, which picks the scale up.
            return;
        }
        let (width, height) = self.configure_size;
        if let Err(err) = self.build_buffers(width, height, self.effective_scale()) {
            self.pending_error = Some(format!("{err:#}"));
            return;
        }
        if let Some(frame) = self.frame.clone()
            && let Err(err) = self.present(&frame)
        {
            self.pending_error = Some(format!("{err:#}"));
        }
    }

    /// Whether `layer` is the surface this loop currently presents. Events for a surface that was
    /// already torn down can still be in flight, and they must not touch the new surface.
    fn is_our_layer(&self, layer: &LayerSurface) -> bool {
        match self.layer.as_ref() {
            Some(ours) => ours == layer,
            None => false,
        }
    }

    fn is_our_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        match self.layer.as_ref() {
            Some(ours) => ours.wl_surface() == surface,
            None => false,
        }
    }

    /// The failure a `show`/`draw`/`hide` call should report, if any.
    fn take_failure(&mut self) -> Option<String> {
        if let Some(message) = self.pending_error.take() {
            return Some(message);
        }
        self.fatal.clone()
    }

    /// The Wayland connection is unusable: stop presenting, but keep answering commands.
    fn note_event_loop_failure(&mut self, err: LoopError) {
        if self.fatal.is_none() {
            tracing::warn!(error = %err, "overlay presentation stopped: connection failed");
            self.fatal = Some(format!("overlay presentation stopped: {err}"));
        }
        match self.wayland_source.take() {
            // Stop polling the dead connection so the loop keeps serving commands.
            Some(token) => self.loop_handle.remove(token),
            // Another source keeps failing as well: do not spin on it.
            None => std::thread::sleep(FAILED_POLL_BACKOFF),
        }
    }
}

impl ProvidesRegistryState for LayerLoop {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

impl ShmHandler for LayerLoop {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for LayerLoop {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        // New outputs are bound by `registry_handlers![OutputState]`.
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl CompositorHandler for LayerLoop {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if !self.is_our_surface(surface) {
            return;
        }
        self.update_scale(new_factor);
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        // The indicator is rendered upright and only placed by the compositor.
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // No frame callbacks are requested.
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if !self.is_our_surface(surface) {
            return;
        }
        // sctk already tracks the surface's scale; this makes sure the scale of the output the
        // compositor actually placed the surface on is applied as well.
        if let Some(info) = self.output_state.info(output) {
            self.update_scale(info.scale_factor);
        }
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
        // The surface is destroyed on `hide`; the next `show` picks the new output up again.
    }
}

impl LayerShellHandler for LayerLoop {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if !self.is_our_layer(layer) {
            return;
        }
        tracing::info!("the compositor closed the overlay layer surface");
        self.teardown();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if !self.is_our_layer(layer) {
            return;
        }
        // The compositor may suggest a different size; zero means "pick your own". The configure
        // was already acknowledged by sctk before this handler runs.
        let (mut width, mut height) = configure.new_size;
        if width == 0 {
            width = overlay_render::OVERLAY_W;
        }
        if height == 0 {
            height = overlay_render::OVERLAY_H;
        }
        self.configure_size = (width, height);
        self.scale = self.surface_scale(layer);
        tracing::debug!(
            width,
            height,
            scale = self.scale,
            "overlay layer surface configured"
        );

        if let Err(err) = self.build_buffers(width, height, self.effective_scale()) {
            self.pending_error = Some(format!("{err:#}"));
            return;
        }
        self.configured = true;
        if let Some(frame) = self.frame.clone()
            && let Err(err) = self.present(&frame)
        {
            self.pending_error = Some(format!("{err:#}"));
        }
    }
}

delegate_registry!(LayerLoop);

smithay_client_toolkit::delegate_dispatch2!(LayerLoop);

// --- command plumbing --------------------------------------------------------------------------

/// Reply channel of a single command; the presenter thread never blocks on it.
type Reply = StdSender<anyhow::Result<()>>;

enum OverlayCmd {
    Show(OverlayFrame, Reply),
    Draw(OverlayFrame, Reply),
    Hide(Reply),
}

fn send_reply(reply: Reply, result: anyhow::Result<()>) {
    if reply.send(result).is_err() {
        tracing::debug!("the caller dropped the overlay command's reply channel");
    }
}

/// Presenter handle owning the command channel into the presenter thread.
struct WaylandOverlay {
    commands: ChannelSender<OverlayCmd>,
    /// Owned so the thread handle is not dropped silently; the thread stops on its own once
    /// `commands` is dropped.
    _thread: JoinHandle<()>,
}

impl WaylandOverlay {
    fn call(&self, build: impl FnOnce(Reply) -> OverlayCmd) -> anyhow::Result<()> {
        let (reply, response) = mpsc::channel();
        let command = build(reply);
        let queued = self.commands.send(command);
        queued.map_err(|_| anyhow!("the overlay presenter thread has stopped"))?;
        match response.recv_timeout(REPLY_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(reply_timeout_error()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(REPLY_ABORTED_ERROR)),
        }
    }
}

/// The error reported when the presenter thread does not answer a command in time.
fn reply_timeout_error() -> anyhow::Error {
    let millis = REPLY_TIMEOUT.as_millis();
    anyhow!("the overlay presenter did not answer within {millis} ms")
}

impl OverlayPresenter for WaylandOverlay {
    fn show(&self, frame: &OverlayFrame) -> anyhow::Result<()> {
        self.call(|reply| OverlayCmd::Show(frame.clone(), reply))
    }

    fn draw(&self, frame: &OverlayFrame) -> anyhow::Result<()> {
        self.call(|reply| OverlayCmd::Draw(frame.clone(), reply))
    }

    fn hide(&self) -> anyhow::Result<()> {
        self.call(OverlayCmd::Hide)
    }
}

// --- rendering helpers -------------------------------------------------------------------------

/// Copy `source` (BGRA, `source_width` × `source_height`) into a shared-memory canvas, clamping to
/// the canvas instead of failing when the compositor asked for a different surface size.
fn blit(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    canvas: &mut [u8],
    canvas_stride: usize,
) {
    if source_width == 0 || source_height == 0 || canvas_stride == 0 {
        return;
    }
    let canvas_height = canvas.len() / canvas_stride;
    let rows = source_height.min(canvas_height);
    let columns = (source_width * 4).min(canvas_stride);
    for row in 0..rows {
        let from = row * source_width * 4;
        let to = row * canvas_stride;
        if from + columns > source.len() || to + columns > canvas.len() {
            break;
        }
        canvas[to..to + columns].copy_from_slice(&source[from..from + columns]);
    }
}

/// Damage the whole surface; `damage_buffer` takes buffer coordinates, the older `damage` request
/// takes surface-local ones.
fn damage(layer: &LayerSurface, surface_size: (u32, u32), buffer_size: (u32, u32)) {
    let surface = layer.wl_surface();
    if surface.version() >= 4 {
        surface.damage_buffer(0, 0, buffer_size.0 as i32, buffer_size.1 as i32);
    } else {
        surface.damage(0, 0, surface_size.0 as i32, surface_size.1 as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::blit;
    use crate::overlay::{self, OverlayBackend, OverlayFrame, OverlayState};
    use std::time::Duration;

    fn frame(level: f32, text: &str) -> OverlayFrame {
        OverlayFrame {
            state: OverlayState::Recording,
            elapsed: Duration::from_secs(3),
            level,
            text: text.to_owned(),
        }
    }

    #[test]
    #[ignore = "needs a layer-shell compositor (COSMIC/KDE/Hyprland)"]
    fn layer_overlay_shows_draws_and_hides() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!("skipping: WAYLAND_DISPLAY is not set");
            return;
        }

        let (backend, reason) = overlay::select_backend();
        if backend != OverlayBackend::Layer {
            eprintln!("skipping: layer-shell unavailable ({reason})");
            return;
        }

        let presenter =
            overlay::spawn(OverlayBackend::Layer).expect("layer-shell overlay presenter");
        presenter.show(&frame(0.2, "")).expect("show");
        std::thread::sleep(Duration::from_millis(200));
        presenter
            .draw(&frame(0.9, "Testing the layer-shell overlay surface."))
            .expect("draw");
        std::thread::sleep(Duration::from_millis(200));
        presenter.hide().expect("hide");
    }

    #[test]
    fn blit_copies_a_matching_canvas() {
        let source: Vec<u8> = (0..16).collect();
        let mut canvas = vec![0u8; 16];
        blit(&source, 2, 2, &mut canvas, 8);
        assert_eq!(canvas, source);
    }

    #[test]
    fn blit_clamps_to_a_narrower_canvas() {
        let source: Vec<u8> = (0..16).collect();
        // A canvas stride of 4 bytes fits one pixel per row, so only the left pixel is copied.
        let mut canvas = vec![0u8; 8];
        blit(&source, 2, 2, &mut canvas, 4);
        assert_eq!(canvas, vec![0, 1, 2, 3, 8, 9, 10, 11]);
    }

    #[test]
    fn blit_stops_at_the_last_canvas_row() {
        let source: Vec<u8> = (0..16).collect();
        let mut canvas = vec![0u8; 8];
        blit(&source, 1, 4, &mut canvas, 4);
        assert_eq!(canvas, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn blit_ignores_degenerate_sizes() {
        let mut canvas = vec![7u8; 4];
        blit(&[], 0, 0, &mut canvas, 4);
        assert_eq!(canvas, vec![7u8; 4]);
    }
}
