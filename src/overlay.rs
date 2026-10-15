//! The overlay presentation contract.
//!
//! The indicator shown while dictating is one replaceable unit: the app composes
//! an [`OverlayFrame`] and hands it to an [`OverlayPresenter`]. Which presenter is
//! built depends on the session — `zwlr_layer_shell_v1` where the compositor
//! offers it, an X11 non-activating window on GNOME (mutter has no layer-shell and
//! cannot host a non-focusing Wayland toplevel), and nothing at all when neither
//! route exists.
//!
//! Backend selection never fails: [`select_backend`] always returns a backend plus
//! the reason it was chosen (that string is user-visible in diagnostics), and
//! [`spawn`] only fails when a real backend cannot be created — the app falls back
//! to [`noop`] then.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

/// What the indicator is currently showing.
///
/// `Notice` is the informational counterpart of `Error`: download progress and
/// "the transcript is on the clipboard" degradation reports use it, so the card
/// does not paint a failure for something that is merely worth telling the user.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayState {
    Recording,
    Transcribing,
    Queued,
    Inserting,
    NoSpeech,
    /// Short-lived informational text shown instead of a dictation state.
    Notice(String),
    Error(String),
}

/// One rendered indicator update. Composed by `app.rs`, consumed by a presenter
/// through `overlay_render::render`.
#[derive(Debug, Clone)]
pub struct OverlayFrame {
    pub state: OverlayState,
    pub elapsed: std::time::Duration,
    /// Input level, `0.0..=1.0` (values outside the range are clamped when drawn).
    pub level: f32,
    /// Streaming partial text; empty until the engine reports something.
    pub text: String,
}

/// A single overlay surface.
///
/// All three methods may be called from any thread and must be cheap enough for a
/// ~10 Hz draw cadence: `show` creates and maps the surface and draws the first
/// frame, `draw` re-renders into the existing surface, `hide` destroys it.
pub trait OverlayPresenter: Send {
    fn show(&self, frame: &OverlayFrame) -> Result<()>;
    fn draw(&self, frame: &OverlayFrame) -> Result<()>;
    fn hide(&self) -> Result<()>;
}

/// Which presentation route was selected for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayBackend {
    /// `zwlr_layer_shell_v1` overlay surface (COSMIC, KDE, Hyprland, sway, …).
    Layer,
    /// X11 non-activating window (the GNOME/Wayland route via XWayland).
    X11,
    /// No route available: the presenter presents nothing.
    None,
}

/// Environment override for the backend probe: `layer` or `x11`.
pub const OVERRIDE_ENV: &str = "LINGUA_OVERLAY";

/// The layer-shell interface name looked up in the Wayland registry (pinned by a test).
#[cfg(test)]
const LAYER_SHELL_INTERFACE: &str = "zwlr_layer_shell_v1";

const REASON_LAYER: &str = "zwlr_layer_shell_v1 available";
const REASON_X11: &str =
    "no zwlr_layer_shell_v1; using the X11 non-activating overlay (GNOME path)";
const REASON_NONE: &str = "no layer-shell and no X11 display — overlay disabled";
const REASON_X11_UNUSABLE: &str =
    "no zwlr_layer_shell_v1 and the X11 display is unusable — overlay disabled";

/// Choose the overlay backend for this session and describe why.
pub fn select_backend() -> (OverlayBackend, String) {
    if let Some(raw) = std::env::var_os(OVERRIDE_ENV) {
        let raw = raw.to_string_lossy();
        match override_backend(&raw) {
            Some(backend) => {
                let base = base_reason(backend);
                let reason = format!("{base} ({OVERRIDE_ENV}={})", raw.trim());
                tracing::info!(backend = ?backend, reason = %reason, "overlay backend selected");
                return (backend, reason);
            }
            None => {
                tracing::warn!(value = %raw, "unknown overlay override; probing the session");
            }
        }
    }

    let layer_shell = match layer_shell_available() {
        Ok(available) => available,
        Err(error) => {
            tracing::warn!(error = %error, "layer-shell probe failed");
            false
        }
    };

    let (backend, reason) = if layer_shell {
        (OverlayBackend::Layer, REASON_LAYER)
    } else if crate::overlay_x11::available() {
        (OverlayBackend::X11, REASON_X11)
    } else if std::env::var_os("DISPLAY").is_some() {
        (OverlayBackend::None, REASON_X11_UNUSABLE)
    } else {
        (OverlayBackend::None, REASON_NONE)
    };

    tracing::info!(backend = ?backend, reason = %reason, "overlay backend selected");
    (backend, reason.to_owned())
}

/// Build the presenter for `backend`. Only a real backend can fail here.
pub fn spawn(backend: OverlayBackend) -> Result<Box<dyn OverlayPresenter>> {
    match backend {
        OverlayBackend::Layer => {
            crate::overlay_wayland::spawn().context("building the layer-shell overlay presenter")
        }
        OverlayBackend::X11 => {
            crate::overlay_x11::spawn().context("building the X11 overlay presenter")
        }
        OverlayBackend::None => Ok(noop()),
    }
}

/// The infallible presenter: it accepts every frame and presents nothing. Used for
/// [`OverlayBackend::None`] and as the app's fallback when a real backend cannot
/// be created, so a broken overlay never blocks dictation.
pub fn noop() -> Box<dyn OverlayPresenter> {
    Box::<NoopPresenter>::default()
}

fn base_reason(backend: OverlayBackend) -> &'static str {
    match backend {
        OverlayBackend::Layer => REASON_LAYER,
        OverlayBackend::X11 => REASON_X11,
        OverlayBackend::None => REASON_NONE,
    }
}

/// Parse a `LINGUA_OVERLAY` value. Unknown values leave the probe in charge.
fn override_backend(raw: &str) -> Option<OverlayBackend> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "layer" => Some(OverlayBackend::Layer),
        "x11" => Some(OverlayBackend::X11),
        _ => None,
    }
}

/// The registry interface name the layer-shell protocol generates.
fn layer_shell_interface() -> &'static str {
    <ZwlrLayerShellV1 as Proxy>::interface().name
}

/// Throwaway connection plus registry roundtrip: does this compositor advertise
/// `zwlr_layer_shell_v1`?
fn layer_shell_available() -> Result<bool> {
    let connection = Connection::connect_to_env()
        .context("connecting to the Wayland display for the layer-shell probe")?;
    let (globals, _queue) = registry_queue_init::<ProbeState>(&connection)
        .context("reading the Wayland registry of the layer-shell probe")?;
    let interface = layer_shell_interface();
    let present = globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface == interface));
    Ok(present)
}

/// Registry dispatch state; the probe only reads the initial global list.
struct ProbeState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ProbeState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _contents: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
    }
}

/// Presenter for sessions with no overlay route: it drops every frame and logs the
/// situation once, so the app stays usable (the transcript still reaches the
/// clipboard).
#[derive(Debug, Default)]
struct NoopPresenter {
    announced: AtomicBool,
}

impl OverlayPresenter for NoopPresenter {
    fn show(&self, _frame: &OverlayFrame) -> Result<()> {
        if !self.announced.swap(true, Ordering::Relaxed) {
            tracing::info!("overlay disabled: no overlay backend is available");
        }
        Ok(())
    }

    fn draw(&self, _frame: &OverlayFrame) -> Result<()> {
        Ok(())
    }

    fn hide(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_values_are_recognized_case_insensitively() {
        assert_eq!(override_backend("layer"), Some(OverlayBackend::Layer));
        assert_eq!(override_backend("LAYER"), Some(OverlayBackend::Layer));
        assert_eq!(override_backend(" x11 "), Some(OverlayBackend::X11));
        assert_eq!(override_backend("X11"), Some(OverlayBackend::X11));
        assert_eq!(override_backend("none"), None);
        assert_eq!(override_backend(""), None);
        assert_eq!(override_backend("wayland"), None);
    }

    #[test]
    fn base_reasons_are_the_contract_literals() {
        assert_eq!(base_reason(OverlayBackend::Layer), REASON_LAYER);
        assert_eq!(
            base_reason(OverlayBackend::Layer),
            "zwlr_layer_shell_v1 available"
        );
        assert_eq!(base_reason(OverlayBackend::X11), REASON_X11);
        let x11 = "no zwlr_layer_shell_v1; using the X11 non-activating overlay (GNOME path)";
        assert_eq!(base_reason(OverlayBackend::X11), x11);
        assert_eq!(base_reason(OverlayBackend::None), REASON_NONE);
        let none = "no layer-shell and no X11 display — overlay disabled";
        assert_eq!(base_reason(OverlayBackend::None), none);
    }

    #[test]
    fn layer_shell_interface_matches_the_protocol_type() {
        assert_eq!(layer_shell_interface(), LAYER_SHELL_INTERFACE);
    }

    #[test]
    fn the_noop_presenter_accepts_and_drops_every_frame() {
        let presenter = noop();
        let frame = OverlayFrame {
            state: OverlayState::Transcribing,
            elapsed: std::time::Duration::from_secs(3),
            level: 0.5,
            text: "hello".to_owned(),
        };
        assert!(presenter.show(&frame).is_ok());
        assert!(presenter.draw(&frame).is_ok());
        assert!(presenter.hide().is_ok());
    }
}
