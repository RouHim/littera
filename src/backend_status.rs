//! The settings compute-acceleration indicator (`.spec/model-load-feedback-and-idle-unload.md`
//! FR-017, base FR-005).
//!
//! Pure state derivation over the model lifecycle, the boot capability probe and the
//! configured preference. The capability plate renders this value; every word and tone is
//! pinned here instead of in a view, so a fallback can never read as a healthy GPU run and a
//! deliberate CPU choice can never read as a failure.

use crate::app::ModelStatus;
use crate::config::BackendPreference;
use crate::diagnostics;
use crate::engine::EngineInfo;
use crate::ui::Tone;

/// The Backend field value while no model is resident and the probe predicts Vulkan.
const LOADS_NEXT: &str = " — loads with the next dictation";
/// The value while the model is loading (FR-002: never a previous backend).
const LOADING: &str = "Loading…";
/// The value when the last load failed (the reason travels in the note).
const COULD_NOT_LOAD: &str = "Could not load";
/// The value when the probe registered no compute device at all.
const UNAVAILABLE: &str = "Unavailable";
/// Suffix of a CPU path the user asked for (`backend = cpu`), never a failure.
const SELECTED: &str = " (selected)";
/// Prefix of the historical line shown after an unload when the last load disagrees with the probe.
const LAST_LOAD: &str = "Last load: ";
const CPU: &str = "CPU";

/// One backend statement for the settings capability plate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    /// The Backend field value, already worded (FR-017).
    pub value: String,
    /// The placard under the plate: a fallback cause or a failure reason.
    pub note: Option<(String, Tone)>,
    /// A labelled history fact, never the current state (FR-012/FR-017).
    pub last_load: Option<String>,
    /// The cached driver remedy, rendered as the plate's Fix line.
    pub fix: Option<String>,
}

/// Derive the plate's backend statement.
///
/// `remediation` is the GPU check's cached remedy (`diagnostics::Check::remediation`); the
/// caller refreshes it per load, never per frame. It is attached only where the state is a
/// fallback or a probe failure.
pub fn status(
    model: &ModelStatus,
    probe: Option<&EngineInfo>,
    probe_error: Option<&str>,
    preference: BackendPreference,
    last_load: Option<&EngineInfo>,
    remediation: Option<String>,
) -> BackendStatus {
    match model {
        ModelStatus::Loading => plain(LOADING.to_owned()),
        ModelStatus::Failed(reason) => BackendStatus {
            value: COULD_NOT_LOAD.to_owned(),
            note: Some((reason.clone(), Tone::Error)),
            last_load: None,
            fix: None,
        },
        ModelStatus::Resident(info) => resident(info, preference, remediation),
        ModelStatus::NotResident => {
            predicted(probe, probe_error, preference, last_load, remediation)
        }
    }
}

/// A state that needs neither a placard nor a remedy.
fn plain(value: String) -> BackendStatus {
    BackendStatus {
        value,
        note: None,
        last_load: None,
        fix: None,
    }
}

/// The engine's report while the model is resident (FR-001/FR-017).
fn resident(
    info: &EngineInfo,
    preference: BackendPreference,
    remediation: Option<String>,
) -> BackendStatus {
    let value = format!("{} on {}", info.backend, info.device);
    if info.backend == CPU && preference == BackendPreference::Cpu {
        return plain(format!("{value}{SELECTED}"));
    }
    if diagnostics::fell_back(info, preference) {
        let cause = info.fallback.as_deref().unwrap_or_default();
        return BackendStatus {
            value,
            note: Some((crate::ui::fallback_text(cause), Tone::Warning)),
            last_load: None,
            fix: remediation,
        };
    }
    plain(value)
}

/// The boot probe's prediction while the model is not resident (FR-012/FR-017).
fn predicted(
    probe: Option<&EngineInfo>,
    probe_error: Option<&str>,
    preference: BackendPreference,
    last_load: Option<&EngineInfo>,
    remediation: Option<String>,
) -> BackendStatus {
    match probe {
        Some(info) if info.backend == CPU && preference == BackendPreference::Cpu => plain(
            format!("{} on {}{SELECTED}{LOADS_NEXT}", info.backend, info.device),
        ),
        Some(info) if diagnostics::fell_back(info, preference) => {
            let cause = info.fallback.as_deref().unwrap_or_default();
            BackendStatus {
                value: format!("{} on {}{LOADS_NEXT}", info.backend, info.device),
                note: Some((crate::ui::fallback_text(cause), Tone::Warning)),
                last_load: None,
                fix: remediation,
            }
        }
        Some(info) => BackendStatus {
            value: format!("{} on {}{LOADS_NEXT}", info.backend, info.device),
            note: None,
            last_load: history(last_load, preference, info),
            fix: None,
        },
        None => BackendStatus {
            value: UNAVAILABLE.to_owned(),
            note: Some((
                probe_error
                    .unwrap_or("no compute device is registered")
                    .to_owned(),
                Tone::Error,
            )),
            last_load: None,
            fix: remediation,
        },
    }
}

/// FR-012/FR-017: after an unload, the previous fallback is history, shown only
/// when the probe now predicts a different outcome, and never as the current state.
fn history(
    last_load: Option<&EngineInfo>,
    preference: BackendPreference,
    probe: &EngineInfo,
) -> Option<String> {
    let last = last_load?;
    if probe.backend == CPU || !diagnostics::fell_back(last, preference) {
        return None;
    }
    let cause = last.fallback.as_deref().unwrap_or_default();
    Some(format!("{LAST_LOAD}{}", crate::ui::fallback_text(cause)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_VULKAN: &str = "AMD Radeon RX 6950 XT (RADV NAVI21)";
    const DEVICE_CPU: &str = "AMD Ryzen 9 7950X";
    const REMEDY: &str = "sudo pacman -S vulkan-radeon";
    const NO_VULKAN: &str = "no Vulkan device found";
    const CURATED_FAILURE: &str = "the Vulkan backend could not initialize";

    fn info(backend: &str, device: &str, fallback: Option<&str>) -> EngineInfo {
        EngineInfo {
            backend: backend.to_owned(),
            device: device.to_owned(),
            fallback: fallback.map(str::to_owned),
        }
    }

    fn vulkan() -> EngineInfo {
        info("Vulkan", DEVICE_VULKAN, None)
    }

    fn cpu_selected() -> EngineInfo {
        info("CPU", DEVICE_CPU, None)
    }

    fn cpu_fallback(reason: &str) -> EngineInfo {
        info("CPU", DEVICE_CPU, Some(reason))
    }

    fn resident(info: EngineInfo) -> ModelStatus {
        ModelStatus::Resident(info)
    }

    fn status_of(
        model: &ModelStatus,
        probe: Option<&EngineInfo>,
        preference: BackendPreference,
        last_load: Option<&EngineInfo>,
    ) -> BackendStatus {
        status(
            model,
            probe,
            Some("engine not loaded"),
            preference,
            last_load,
            Some(REMEDY.to_owned()),
        )
    }

    #[test]
    fn resident_vulkan_names_the_device() {
        let status = status_of(
            &resident(vulkan()),
            Some(&vulkan()),
            BackendPreference::Auto,
            None,
        );
        assert_eq!(status.value, format!("Vulkan on {DEVICE_VULKAN}"));
        assert_eq!(status.note, None);
        assert_eq!(status.last_load, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn resident_cpu_selected_is_neutral() {
        let status = status_of(
            &resident(cpu_selected()),
            Some(&cpu_selected()),
            BackendPreference::Cpu,
            None,
        );
        assert_eq!(status.value, format!("CPU on {DEVICE_CPU} (selected)"));
        assert_eq!(status.note, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn resident_cpu_fallback_names_the_curated_cause_and_fix() {
        let status = status_of(
            &resident(cpu_fallback(
                "failed to initialize the Vulkan backend: no device",
            )),
            Some(&vulkan()),
            BackendPreference::Auto,
            None,
        );
        assert_eq!(status.value, format!("CPU on {DEVICE_CPU}"));
        assert_eq!(
            status.note,
            Some((format!("CPU fallback — {CURATED_FAILURE}"), Tone::Warning,))
        );
        assert_eq!(status.fix, Some(REMEDY.to_owned()));
    }

    #[test]
    fn loading_hides_a_previous_backend() {
        let status = status_of(
            &ModelStatus::Loading,
            Some(&vulkan()),
            BackendPreference::Auto,
            Some(&cpu_fallback("no Vulkan device found")),
        );
        assert_eq!(status.value, "Loading…");
        assert_eq!(status.note, None);
        assert_eq!(status.last_load, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn failed_load_shows_the_reason_and_no_backend() {
        let failure = "model load failed on both the Vulkan and the CPU backend: boom";
        let status = status_of(
            &ModelStatus::Failed(failure.to_owned()),
            Some(&vulkan()),
            BackendPreference::Auto,
            Some(&vulkan()),
        );
        assert_eq!(status.value, "Could not load");
        assert_eq!(status.note, Some((failure.to_owned(), Tone::Error)));
        assert_eq!(status.last_load, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn not_resident_probe_names_the_predicted_vulkan_device() {
        let probe = vulkan();
        let status = status_of(
            &ModelStatus::NotResident,
            Some(&probe),
            BackendPreference::Auto,
            None,
        );
        assert_eq!(
            status.value,
            format!("Vulkan on {DEVICE_VULKAN} — loads with the next dictation")
        );
        assert_eq!(status.note, None);
        assert_eq!(status.last_load, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn not_resident_probe_cpu_only_warns_with_the_remedy() {
        let probe = cpu_fallback(NO_VULKAN);
        let status = status_of(
            &ModelStatus::NotResident,
            Some(&probe),
            BackendPreference::Auto,
            None,
        );
        assert_eq!(
            status.value,
            format!("CPU on {DEVICE_CPU} — loads with the next dictation")
        );
        assert_eq!(
            status.note,
            Some((format!("CPU fallback — {NO_VULKAN}"), Tone::Warning))
        );
        assert_eq!(status.fix, Some(REMEDY.to_owned()));
    }

    #[test]
    fn not_resident_selected_cpu_is_neutral() {
        let probe = cpu_selected();
        let status = status_of(
            &ModelStatus::NotResident,
            Some(&probe),
            BackendPreference::Cpu,
            None,
        );
        assert_eq!(
            status.value,
            format!("CPU on {DEVICE_CPU} (selected) — loads with the next dictation")
        );
        assert_eq!(status.note, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn not_resident_last_fallback_is_a_labelled_history_line() {
        let probe = vulkan();
        let last = cpu_fallback("failed to initialize the Vulkan backend: no device");
        let status = status_of(
            &ModelStatus::NotResident,
            Some(&probe),
            BackendPreference::Auto,
            Some(&last),
        );
        assert_eq!(
            status.last_load,
            Some(format!("Last load: CPU fallback — {CURATED_FAILURE}"))
        );
        assert_eq!(status.note, None);
        assert_eq!(status.fix, None);
    }

    #[test]
    fn not_resident_last_fallback_is_omitted_when_the_probe_agrees() {
        let probe = cpu_fallback(NO_VULKAN);
        let last = cpu_fallback(NO_VULKAN);
        let status = status_of(
            &ModelStatus::NotResident,
            Some(&probe),
            BackendPreference::Auto,
            Some(&last),
        );
        assert_eq!(status.last_load, None);
        assert_eq!(
            status.note,
            Some((format!("CPU fallback — {NO_VULKAN}"), Tone::Warning))
        );
    }

    #[test]
    fn probe_error_is_unavailable_with_the_reason() {
        let error = "no compute device is registered (no Vulkan and no CPU device)";
        let status = status(
            &ModelStatus::NotResident,
            None,
            Some(error),
            BackendPreference::Auto,
            None,
            Some(REMEDY.to_owned()),
        );
        assert_eq!(status.value, "Unavailable");
        assert_eq!(status.note, Some((error.to_owned(), Tone::Error)));
        assert_eq!(status.last_load, None);
        assert_eq!(status.fix, Some(REMEDY.to_owned()));
    }
}
