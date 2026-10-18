//! FR-014 / FR-022 environment checks.
//!
//! The same probe results feed three surfaces: the onboarding "System check" step,
//! the startup problem notice and `lingua --diagnostics`. Every check is a pure
//! formatter over values gathered elsewhere, so a failure reads identically in all
//! three places and can be unit-tested without any hardware or session.

use crate::config::{BackendPreference, Config, HotkeySource};
use crate::engine::EngineInfo;
use crate::hotkey::{self, BindError, Binding};
use crate::hotkey_device;
use crate::inserter;
use crate::overlay::OverlayBackend;
use crate::vulkan_icd;

const NAME_HOTKEY: &str = "hotkey access";
const NAME_PASTE: &str = "paste permission";
const NAME_GPU: &str = "GPU backend";
const NAME_OVERLAY: &str = "overlay backend";

const REMEDIATION_HOTKEY: &str = "sudo usermod -aG input $USER && log out and back in";
const REMEDIATION_PASTE: &str =
    "grant write access to /dev/uinput (logind uaccess ACL); log out and back in if missing";
const REMEDIATION_GPU: &str =
    "install/repair the Vulkan driver for this GPU (vulkaninfo shows what the loader finds)";
const REMEDIATION_OVERLAY: &str =
    "install a layer-shell compositor or run under XWayland for the non-activating overlay";

const PASTE_OK_DETAIL: &str = "/dev/uinput writable — auto-paste enabled";

/// One diagnostic result: a stable name, a pass/fail flag, the observed value and
/// an actionable remedy when the check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    pub remediation: Option<String>,
}

/// Everything [`collect`] needs to render the four checks. Gathered once from the
/// live subsystems by the app, or by `main` for `lingua --diagnostics`.
#[derive(Debug, Clone)]
pub struct State {
    pub binding: Option<Binding>,
    pub hotkey_error: Option<BindError>,
    pub device_ok: bool,
    pub engine: Option<EngineInfo>,
    pub engine_error: Option<String>,
    pub overlay_backend: OverlayBackend,
    pub overlay_reason: String,
}

// The default state is "nothing probed yet": no binding, no engine, no overlay
// backend, so it names `OverlayBackend::None` explicitly instead of deriving.
impl Default for State {
    fn default() -> Self {
        State {
            binding: None,
            hotkey_error: None,
            device_ok: false,
            engine: None,
            engine_error: None,
            overlay_backend: OverlayBackend::None,
            overlay_reason: String::new(),
        }
    }
}

/// Hotkey capability: the mechanism that actually holds the chord, or the reason
/// nothing could be bound. `device_ok` is `hotkey_device::available()` — whether
/// the read-only evdev listener found a keyboard at all.
///
/// The failure's own remediation is preferred: a chord the parser rejects needs
/// a different cure than unreadable input devices (FR-014).
pub fn hotkey_check(
    binding: Option<&Binding>,
    error: Option<&BindError>,
    device_ok: bool,
) -> Check {
    if let Some(binding) = binding {
        return Check {
            name: NAME_HOTKEY,
            ok: true,
            detail: binding_detail(binding),
            remediation: None,
        };
    }

    let (detail, remediation) = match (error, device_ok) {
        (Some(error), _) if !error.remediation.is_empty() => {
            (error.detail.clone(), error.remediation.clone())
        }
        (Some(error), _) => (error.detail.clone(), REMEDIATION_HOTKEY.to_owned()),
        (None, true) => (
            "no dictation shortcut is bound".to_owned(),
            REMEDIATION_HOTKEY.to_owned(),
        ),
        (None, false) => (
            hotkey_device::unreadable_detail(),
            REMEDIATION_HOTKEY.to_owned(),
        ),
    };
    Check {
        name: NAME_HOTKEY,
        ok: false,
        detail,
        remediation: Some(remediation),
    }
}

/// Which mechanism holds the chord, and — for the listen-only evdev listener —
/// the compositor binding it cannot suppress.
fn binding_detail(binding: &Binding) -> String {
    match binding.source {
        HotkeySource::Portal => format!("portal: {}", binding.trigger_description),
        HotkeySource::Device => {
            let note = hotkey::device_limitation_note();
            format!("device listener: {} — {}", binding.chord, note)
        }
    }
}

/// Auto-paste capability: probing `/dev/uinput` for write access.
pub fn paste_check() -> Check {
    match inserter::uinput_writable() {
        Ok(()) => Check {
            name: NAME_PASTE,
            ok: true,
            detail: PASTE_OK_DETAIL.to_owned(),
            remediation: None,
        },
        Err(reason) => Check {
            name: NAME_PASTE,
            ok: false,
            detail: reason,
            remediation: Some(REMEDIATION_PASTE.to_owned()),
        },
    }
}

/// Transcription backend: `"Vulkan: AMD Radeon RX 6950 XT (RADV NAVI21)"` once the
/// engine reported a device.
///
/// A Vulkan→CPU fallback the preference did not ask for fails the check
/// (FR-005/FR-014): the app still transcribes, but on the CPU, and the reason is
/// a machine that cannot reach its GPU. `missing_driver` — the command that
/// installs the absent driver, when the machine names one — replaces the generic
/// remedy. `backend = "cpu"` asks for the CPU path, so it stays a pass.
pub fn gpu_check(
    info: Option<&EngineInfo>,
    error: Option<&str>,
    preference: BackendPreference,
    missing_driver: Option<String>,
) -> Check {
    let (ok, detail) = match info {
        Some(info) => (!fell_back(info, preference), gpu_detail(info)),
        None => (false, error.unwrap_or("engine not loaded").to_owned()),
    };
    Check {
        name: NAME_GPU,
        ok,
        detail,
        remediation: if ok {
            None
        } else {
            Some(missing_driver.unwrap_or_else(|| REMEDIATION_GPU.to_owned()))
        },
    }
}

/// Whether the engine abandoned the GPU the preference asked for (FR-005). A
/// Vulkan backend that held up carries no fallback, and `cpu` was the request.
fn fell_back(info: &EngineInfo, preference: BackendPreference) -> bool {
    info.fallback.is_some() && preference != BackendPreference::Cpu
}

/// The GPU check as the app and `lingua --diagnostics` render it: [`gpu_check`]
/// with the live driver inventory attached. The probe runs only for a fallback,
/// so a healthy machine reads no `/sys` for this check.
pub fn gpu_check_probed(cfg: &Config, info: Option<&EngineInfo>, error: Option<&str>) -> Check {
    let missing_driver = match info {
        Some(info) if fell_back(info, cfg.backend) => vulkan_icd::advice(),
        _ => None,
    };
    gpu_check(info, error, cfg.backend, missing_driver)
}

/// `"{backend}: {device}"`, with the fallback notice appended when the engine had
/// to fall back (FR-005 / SC-004).
fn gpu_detail(info: &EngineInfo) -> String {
    match &info.fallback {
        Some(fallback) => format!("{}: {} (fallback: {})", info.backend, info.device, fallback),
        None => format!("{}: {}", info.backend, info.device),
    }
}

/// Overlay presentation: both real backends pass, `OverlayBackend::None` fails
/// because dictation would then give no on-screen feedback (FR-011).
pub fn overlay_check(backend: OverlayBackend, reason: &str) -> Check {
    let (ok, label) = match backend {
        OverlayBackend::Layer => (true, "layer-shell"),
        OverlayBackend::X11 => (true, "X11 non-activating window"),
        OverlayBackend::None => (false, "disabled"),
    };
    Check {
        name: NAME_OVERLAY,
        ok,
        detail: format!("{label}: {reason}"),
        remediation: if ok {
            None
        } else {
            Some(REMEDIATION_OVERLAY.to_owned())
        },
    }
}

/// The four checks in `lingua --diagnostics` output order: hotkey, paste, GPU,
/// overlay. `cfg` supplies the model/language context when the engine has not been
/// loaded yet and reported neither an info nor an error.
pub fn collect(cfg: &Config, state: &State) -> Vec<Check> {
    let hotkey_result = hotkey_check(
        state.binding.as_ref(),
        state.hotkey_error.as_ref(),
        state.device_ok,
    );
    let paste_result = paste_check();
    let gpu_result = gpu_check_from_state(cfg, state);
    let overlay_result = overlay_check(state.overlay_backend, &state.overlay_reason);
    vec![hotkey_result, paste_result, gpu_result, overlay_result]
}

/// The app knows the engine only once it reported in; before that the check names
/// the configured model instead of printing a bare "not loaded".
fn gpu_check_from_state(cfg: &Config, state: &State) -> Check {
    match (&state.engine, state.engine_error.as_deref()) {
        (Some(info), _) => gpu_check_probed(cfg, Some(info), None),
        (None, Some(error)) => gpu_check_probed(cfg, None, Some(error)),
        (None, None) => Check {
            name: NAME_GPU,
            ok: false,
            detail: not_loaded_detail(cfg),
            remediation: None,
        },
    }
}

/// Names the configured model and language while the engine is still unloaded.
/// Carries no remediation: the remedy is the model step of onboarding, not a
/// driver repair.
fn not_loaded_detail(cfg: &Config) -> String {
    let model = &cfg.model;
    let language = cfg.language.label();
    format!("model {model} not loaded yet (language {language})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vulkan_info() -> EngineInfo {
        EngineInfo {
            backend: "Vulkan".to_owned(),
            device: "AMD Radeon RX 6950 XT (RADV NAVI21)".to_owned(),
            fallback: None,
        }
    }

    fn portal_binding() -> Binding {
        Binding {
            chord: hotkey::DEFAULT_CHORD.to_owned(),
            trigger_description: hotkey::DEFAULT_CHORD.to_owned(),
            source: HotkeySource::Portal,
            portal_detail: None,
        }
    }

    fn device_binding() -> Binding {
        Binding {
            chord: hotkey::DEFAULT_CHORD.to_owned(),
            trigger_description: "CTRL+SUPER (device listener)".to_owned(),
            source: HotkeySource::Device,
            portal_detail: None,
        }
    }

    #[test]
    fn checks_compare_by_value() {
        let check = gpu_check(Some(&vulkan_info()), None, BackendPreference::Auto, None);
        assert_eq!(
            check,
            gpu_check(Some(&vulkan_info()), None, BackendPreference::Auto, None)
        );

        let mut changed = check.clone();
        changed.ok = false;
        assert_ne!(check, changed);
    }

    #[test]
    fn names_are_the_contracted_literals() {
        assert_eq!(NAME_HOTKEY, "hotkey access");
        assert_eq!(NAME_PASTE, "paste permission");
        assert_eq!(NAME_GPU, "GPU backend");
        assert_eq!(NAME_OVERLAY, "overlay backend");
    }

    #[test]
    fn remediations_are_the_contracted_literals() {
        assert_eq!(
            REMEDIATION_HOTKEY,
            "sudo usermod -aG input $USER && log out and back in"
        );
        assert_eq!(
            REMEDIATION_GPU,
            "install/repair the Vulkan driver for this GPU (vulkaninfo shows what the loader finds)"
        );
        assert!(REMEDIATION_PASTE.contains("/dev/uinput (logind uaccess ACL)"));
        assert!(REMEDIATION_PASTE.ends_with("log out and back in if missing"));
        assert!(REMEDIATION_OVERLAY.contains("layer-shell compositor"));
        assert!(REMEDIATION_OVERLAY.ends_with("non-activating overlay"));
        assert_eq!(PASTE_OK_DETAIL, "/dev/uinput writable — auto-paste enabled");
    }

    #[test]
    fn gpu_check_reports_backend_and_device() {
        let check = gpu_check(Some(&vulkan_info()), None, BackendPreference::Auto, None);
        assert_eq!(check.name, "GPU backend");
        assert!(check.ok);
        assert_eq!(check.detail, "Vulkan: AMD Radeon RX 6950 XT (RADV NAVI21)");
        assert_eq!(check.remediation, None);
    }

    /// The reported defect: an Intel GPU without an ICD showed up as a passing
    /// check, so onboarding said nothing about the missing driver package.
    #[test]
    fn gpu_check_fails_a_fallback_the_preference_did_not_ask_for() {
        let info = EngineInfo {
            backend: "CPU".to_owned(),
            device: "12th Gen Intel(R) Core(TM) i7-12700H".to_owned(),
            fallback: Some("no Vulkan device found".to_owned()),
        };
        let driver = "sudo pacman -S vulkan-intel — no Vulkan ICD for the Intel GPU".to_owned();

        let check = gpu_check(
            Some(&info),
            Some("stale load error"),
            BackendPreference::Auto,
            Some(driver.clone()),
        );
        assert_eq!(check.name, "GPU backend");
        assert!(!check.ok);
        assert_eq!(
            check.detail,
            "CPU: 12th Gen Intel(R) Core(TM) i7-12700H (fallback: no Vulkan device found)"
        );
        assert_eq!(check.remediation.as_deref(), Some(driver.as_str()));

        // `vulkan` is a request for the GPU, so the fallback fails there too.
        assert!(!gpu_check(Some(&info), None, BackendPreference::Vulkan, None).ok);
    }

    #[test]
    fn gpu_check_uses_the_generic_remedy_when_no_driver_is_named() {
        let info = EngineInfo {
            backend: "CPU".to_owned(),
            device: "AMD Ryzen 9 5950X".to_owned(),
            fallback: Some("Vulkan load failed".to_owned()),
        };
        let check = gpu_check(Some(&info), None, BackendPreference::Auto, None);

        assert!(!check.ok);
        assert_eq!(check.remediation.as_deref(), Some(REMEDIATION_GPU));
    }

    #[test]
    fn gpu_check_passes_a_cpu_backend_that_was_requested() {
        let info = EngineInfo {
            backend: "CPU".to_owned(),
            device: "AMD Ryzen 9 5950X".to_owned(),
            fallback: Some("no Vulkan device found".to_owned()),
        };
        let check = gpu_check(Some(&info), None, BackendPreference::Cpu, None);

        assert!(check.ok);
        assert_eq!(check.remediation, None);
    }

    #[test]
    fn gpu_check_failure_uses_the_error() {
        let check = gpu_check(None, Some("boom"), BackendPreference::Auto, None);
        assert_eq!(check.name, "GPU backend");
        assert!(!check.ok);
        assert_eq!(check.detail, "boom");
        assert_eq!(check.remediation.as_deref(), Some(REMEDIATION_GPU));
    }

    #[test]
    fn gpu_check_without_error_says_not_loaded() {
        let check = gpu_check(None, None, BackendPreference::Auto, None);
        assert!(!check.ok);
        assert_eq!(check.detail, "engine not loaded");
        assert_eq!(check.remediation.as_deref(), Some(REMEDIATION_GPU));
    }

    #[test]
    fn overlay_check_passes_for_both_backends() {
        let layer = overlay_check(OverlayBackend::Layer, "zwlr_layer_shell_v1 available");
        assert_eq!(layer.name, "overlay backend");
        assert!(layer.ok);
        assert_eq!(layer.detail, "layer-shell: zwlr_layer_shell_v1 available");
        assert_eq!(layer.remediation, None);

        let reason = "no zwlr_layer_shell_v1; using the X11 overlay";
        let x11 = overlay_check(OverlayBackend::X11, reason);
        assert!(x11.ok);
        assert!(x11.detail.starts_with("X11 non-activating window: "));
        assert!(x11.detail.ends_with(reason));
        assert_eq!(x11.remediation, None);
    }

    #[test]
    fn overlay_check_fails_when_disabled() {
        let check = overlay_check(OverlayBackend::None, "no layer-shell and no X11");
        assert_eq!(check.name, "overlay backend");
        assert!(!check.ok);
        assert_eq!(check.detail, "disabled: no layer-shell and no X11");
        assert_eq!(check.remediation.as_deref(), Some(REMEDIATION_OVERLAY));
    }

    #[test]
    fn hotkey_check_portal_binding_shows_the_trigger() {
        let check = hotkey_check(Some(&portal_binding()), None, false);
        assert_eq!(check.name, "hotkey access");
        assert!(check.ok);
        assert_eq!(check.detail, "portal: CTRL+SUPER");
        assert_eq!(check.remediation, None);
    }

    #[test]
    fn hotkey_check_device_binding_states_the_limitation() {
        let check = hotkey_check(Some(&device_binding()), None, true);
        assert!(check.ok);
        assert_eq!(check.name, "hotkey access");

        let detail = check.detail.as_str();
        assert!(detail.starts_with("device listener: CTRL+SUPER"));
        assert!(detail.contains("device listener cannot suppress"));
        assert!(detail.contains(hotkey::device_limitation_note()));
        assert_eq!(check.remediation, None);
    }

    #[test]
    fn hotkey_check_error_carries_the_remediation_of_its_cause() {
        // Access failure: the error's own input-group remedy is surfaced.
        let access = BindError::from_parts(
            "no shortcut portal and /dev/input/event* not readable",
            "sudo usermod -aG input $USER && log out and back in",
        );
        let check = hotkey_check(None, Some(&access), false);

        assert_eq!(check.name, "hotkey access");
        assert!(!check.ok);
        assert_eq!(
            check.detail,
            "no shortcut portal and /dev/input/event* not readable"
        );
        assert_eq!(check.remediation.as_deref(), Some(REMEDIATION_HOTKEY));

        // Chord typo: the input-group command would not fix it, so the parser's
        // remedy must be the one shown.
        let typo = BindError::from_parts(
            "unsupported chord token 'FOO'",
            "enter a chord like CTRL+SUPER, CTRL+ALT+A or SUPER+SPACE in settings",
        );
        assert_eq!(
            hotkey_check(None, Some(&typo), true).remediation.as_deref(),
            Some("enter a chord like CTRL+SUPER, CTRL+ALT+A or SUPER+SPACE in settings")
        );

        // An error without a remedy falls back to the access command.
        let bare = BindError::from_parts("broken", "");
        assert_eq!(
            hotkey_check(None, Some(&bare), true).remediation.as_deref(),
            Some(REMEDIATION_HOTKEY)
        );
    }

    #[test]
    fn hotkey_check_without_error_reports_the_unreadable_device() {
        let check = hotkey_check(None, None, false);
        assert!(!check.ok);
        assert!(!check.detail.is_empty());
        assert_eq!(check.remediation.as_deref(), Some(REMEDIATION_HOTKEY));
    }

    #[test]
    fn state_defaults_to_no_overlay_and_no_binding() {
        let state = State::default();
        assert_eq!(state.overlay_backend, OverlayBackend::None);
        assert!(state.overlay_reason.is_empty());
        assert!(state.binding.is_none());
        assert!(state.hotkey_error.is_none());
        assert!(!state.device_ok);
        assert!(state.engine.is_none());
        assert!(state.engine_error.is_none());
    }

    #[test]
    fn collect_returns_the_four_checks_in_order() {
        let cfg = Config::default();
        let state = State {
            binding: Some(device_binding()),
            device_ok: true,
            overlay_backend: OverlayBackend::Layer,
            overlay_reason: "zwlr_layer_shell_v1 available".to_owned(),
            ..State::default()
        };

        let checks = collect(&cfg, &state);
        assert_eq!(checks.len(), 4);
        let names: Vec<&str> = checks.iter().map(|check| check.name).collect();
        assert_eq!(names[0], "hotkey access");
        assert_eq!(names[1], "paste permission");
        assert_eq!(names[2], "GPU backend");
        assert_eq!(names[3], "overlay backend");

        assert!(checks[0].ok);
        // No engine reported in yet: the check names the configured model.
        assert!(!checks[2].ok);
        assert!(checks[2].detail.contains(&cfg.model));
        assert!(checks[3].ok);
    }

    #[test]
    fn collect_prefers_engine_info_over_a_stale_error() {
        let cfg = Config::default();
        let state = State {
            engine: Some(vulkan_info()),
            engine_error: Some("stale".to_owned()),
            ..State::default()
        };

        let checks = collect(&cfg, &state);
        assert!(checks[2].ok);
        assert_eq!(
            checks[2].detail,
            "Vulkan: AMD Radeon RX 6950 XT (RADV NAVI21)"
        );
    }
}
