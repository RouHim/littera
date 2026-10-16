//! Persisted settings (FR-016) and the shared, live-reloadable configuration (Scenario 3.4).
//!
//! Every subsystem reads its configuration through [`SharedConfig`], so a change made in the
//! settings window is visible to the next dictation without restarting the process.

use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Application id: the XDG directory name and the single-instance/tray identifier.
pub const APP_ID: &str = "lingua";

/// The distinguished `input_device` value — and the meaning of an absent key: record from the
/// microphone the system's audio layer currently designates (FR-001).
pub const INPUT_DEVICE_SYSTEM: &str = "system";

/// Transcription language hint. The model always gets an explicit language or none at all
/// (`auto`), which is why [`Language::api_code`] is the only source of this value (FR-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Language {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en-US")]
    EnUs,
    #[serde(rename = "de-DE")]
    DeDe,
}

impl Language {
    /// Every variant, in UI order.
    pub const ALL: [Language; 3] = [Self::Auto, Self::EnUs, Self::DeDe];

    /// The engine language hint — the only source of it (FR-007). `None` means `auto`.
    pub const fn api_code(self) -> Option<&'static str> {
        match self {
            Language::Auto => None,
            Language::EnUs => Some("en-US"),
            Language::DeDe => Some("de-DE"),
        }
    }

    /// The stable token used in the config file.
    pub const fn label(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::EnUs => "en-US",
            Language::DeDe => "de-DE",
        }
    }

    /// The human-readable name for the settings UI.
    pub const fn display_name(self) -> &'static str {
        match self {
            Language::Auto => "Auto-detect",
            Language::EnUs => "English (US)",
            Language::DeDe => "German (Germany)",
        }
    }
}

/// Which `transcribe.cpp` backend the engine should load (FR-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackendPreference {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "vulkan")]
    Vulkan,
    #[serde(rename = "cpu")]
    Cpu,
}

impl BackendPreference {
    /// The stable token used in the config file.
    pub const fn label(self) -> &'static str {
        match self {
            BackendPreference::Auto => "auto",
            BackendPreference::Vulkan => "vulkan",
            BackendPreference::Cpu => "cpu",
        }
    }
}

/// Which mechanism holds the global hotkey (FR-008: exactly one is active).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HotkeySource {
    #[serde(rename = "portal")]
    Portal,
    #[serde(rename = "device")]
    Device,
}

impl HotkeySource {
    /// The description shown in settings and diagnostics.
    pub const fn label(self) -> &'static str {
        match self {
            HotkeySource::Portal => "portal",
            HotkeySource::Device => "device listener",
        }
    }
}

/// The persisted settings, stored as TOML at [`Config::path`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding_complete: bool,
    /// Catalog id from [`crate::model_catalog`].
    pub model: String,
    pub language: Language,
    /// Canonical chord literal, e.g. `CTRL+SUPER` (see [`crate::hotkey::DEFAULT_CHORD`]).
    pub hotkey: String,
    /// `None` until a mechanism actually bound the chord. TOML cannot represent `None`, so the
    /// key is omitted while the chord is unbound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotkey_source: Option<HotkeySource>,
    pub backend: BackendPreference,
    /// Capture device for dictation (FR-001/FR-003): [`INPUT_DEVICE_SYSTEM`] follows the system
    /// default, any other value is a backend device identifier (`<host>:<device>`). While it is
    /// the default the key is not written at all.
    #[serde(skip_serializing_if = "is_system_input_device")]
    pub input_device: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            onboarding_complete: false,
            model: crate::model_catalog::DEFAULT_ARTIFACT_ID.to_owned(),
            language: Language::Auto,
            hotkey: crate::hotkey::DEFAULT_CHORD.to_owned(),
            hotkey_source: None,
            backend: BackendPreference::Auto,
            input_device: INPUT_DEVICE_SYSTEM.to_owned(),
        }
    }
}

impl Config {
    /// `$XDG_CONFIG_HOME/lingua/config.toml`, falling back to `~/.config/lingua/config.toml`.
    pub fn path() -> Result<PathBuf> {
        let base = dirs::config_dir().ok_or_else(error_no_config_dir)?;
        Ok(base.join(APP_ID).join("config.toml"))
    }

    /// The capture device the next recording opens, or `None` for the system default (FR-010).
    pub fn capture_device(&self) -> Option<&str> {
        (!is_system_input_device(&self.input_device)).then_some(self.input_device.as_str())
    }

    /// Load the settings. A missing file is not an error: it yields the defaults. A file that is
    /// present but unreadable or malformed is an error naming the path and the failure — never a
    /// silent reset of the user's settings.
    pub fn load() -> Result<Config> {
        Self::load_from(&Self::path()?)
    }

    /// Persist the settings atomically: write `config.toml.tmp`, then rename it into place.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    /// `$XDG_DATA_HOME/lingua/models`, created on demand (FR-003).
    pub fn models_dir() -> Result<PathBuf> {
        let base = dirs::data_dir().ok_or_else(error_no_data_dir)?;
        let dir = base.join(APP_ID).join("models");
        let context = format!("cannot create model directory {}", dir.display());
        fs::create_dir_all(&dir).with_context(|| context)?;
        Ok(dir)
    }

    /// Path-parameterized [`Config::load`], so tests never touch the user's configuration.
    fn load_from(path: &Path) -> Result<Config> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(path = %path.display(), "no settings file yet; using defaults");
                return Ok(Config::default());
            }
            Err(err) => {
                let context = format!("cannot read settings file {}", path.display());
                return Err(err).with_context(|| context);
            }
        };
        toml::from_str(&text).map_err(|err| anyhow!("failed to parse {}: {err}", path.display()))
    }

    /// Path-parameterized [`Config::save`].
    fn save_to(&self, path: &Path) -> Result<()> {
        let no_parent = format!("settings path {} has no parent directory", path.display());
        let parent = path.parent().ok_or_else(|| anyhow!("{no_parent}"))?;
        let context = format!("cannot create settings directory {}", parent.display());
        fs::create_dir_all(parent).with_context(|| context)?;

        let context = format!("cannot serialize settings for {}", path.display());
        let text = toml::to_string(self).with_context(|| context)?;

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);

        let context = format!("cannot write {}", tmp.display());
        fs::write(&tmp, text.as_bytes()).with_context(|| context)?;
        let moved = format!(
            "cannot move {} into place at {}",
            tmp.display(),
            path.display()
        );
        fs::rename(&tmp, path).with_context(|| moved)?;

        tracing::info!(path = %path.display(), "settings saved");
        Ok(())
    }
}

/// Recovering a poisoned lock keeps the app usable: poisoning only happens when a writer
/// panicked while holding the lock.
fn warn_poisoned_lock() {
    tracing::warn!("settings lock poisoned by a panicking writer; reusing the recovered value");
}

/// Whether `value` is the distinguished [`INPUT_DEVICE_SYSTEM`] state.
fn is_system_input_device(value: &str) -> bool {
    value == INPUT_DEVICE_SYSTEM
}

/// The config directory is unknown when neither `$XDG_CONFIG_HOME` nor `$HOME` is set (FR-016).
fn error_no_config_dir() -> anyhow::Error {
    anyhow!("cannot determine XDG config directory ($XDG_CONFIG_HOME or $HOME unset)")
}

/// The data directory is unknown when neither `$XDG_DATA_HOME` nor `$HOME` is set (FR-003).
fn error_no_data_dir() -> anyhow::Error {
    anyhow!("cannot determine XDG data directory ($XDG_DATA_HOME or $HOME unset)")
}

/// The live configuration every subsystem reads from.
#[derive(Debug, Clone)]
pub struct SharedConfig(Arc<RwLock<Config>>);

impl SharedConfig {
    pub fn new(config: Config) -> Self {
        Self(Arc::new(RwLock::new(config)))
    }

    /// A consistent copy of the current settings.
    pub fn snapshot(&self) -> Config {
        match self.0.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                warn_poisoned_lock();
                poisoned.into_inner().clone()
            }
        }
    }

    /// Apply `f`, then persist. The write lock is held across the save, so concurrent updates
    /// cannot land their older state last. The in-memory value is updated even when saving
    /// fails; the save error is returned so the UI can report it.
    pub fn update(&self, f: impl FnOnce(&mut Config)) -> Result<()> {
        let mut guard = match self.0.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn_poisoned_lock();
                poisoned.into_inner()
            }
        };
        f(&mut guard);
        guard.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lingua-config-{}-{tag}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir.join("config.toml")
    }

    /// Serializes `cfg`, asserts `expected_line` appears verbatim, then reloads and returns the
    /// parsed value.
    fn assert_token_roundtrip(cfg: &Config, expected_line: &str) -> Config {
        let text = toml::to_string(cfg).expect("serialize config");
        assert!(
            text.contains(expected_line),
            "missing {expected_line:?} in {text}"
        );
        toml::from_str(&text).expect("deserialize config")
    }

    #[test]
    fn default_uses_sibling_module_literals() {
        let cfg = Config::default();
        assert_eq!(cfg.model, crate::model_catalog::DEFAULT_ARTIFACT_ID);
        assert_eq!(cfg.hotkey, crate::hotkey::DEFAULT_CHORD);
        assert_eq!(cfg.language, Language::Auto);
        assert_eq!(cfg.backend, BackendPreference::Auto);
        assert!(!cfg.onboarding_complete);
        assert_eq!(cfg.hotkey_source, None);
        assert_eq!(cfg.input_device, INPUT_DEVICE_SYSTEM);
        assert_eq!(cfg.capture_device(), None);
    }

    #[test]
    fn system_input_device_is_the_frozen_literal_default() {
        assert_eq!(INPUT_DEVICE_SYSTEM, "system");
    }

    #[test]
    fn system_input_device_is_omitted_from_the_file() {
        let text = toml::to_string(&Config::default()).expect("serialize default config");
        assert!(!text.contains("input_device"), "{text}");
        let back: Config = toml::from_str(&text).expect("deserialize default config");
        assert_eq!(back, Config::default());
    }

    #[test]
    fn configured_input_device_roundtrips_through_toml() {
        let cfg = Config {
            input_device: "alsa:pipewire".to_owned(),
            ..Config::default()
        };
        let text = toml::to_string(&cfg).expect("serialize config");
        assert!(text.contains("input_device = \"alsa:pipewire\""), "{text}");
        let back: Config = toml::from_str(&text).expect("deserialize config");
        assert_eq!(back, cfg);
        assert_eq!(back.capture_device(), Some("alsa:pipewire"));
    }

    #[test]
    fn non_default_config_roundtrips_through_toml() {
        let cfg = Config {
            onboarding_complete: true,
            model: "nemotron-speech-streaming-en-0.6b-Q8_0".to_owned(),
            language: Language::DeDe,
            hotkey: "CTRL+SHIFT+D".to_owned(),
            hotkey_source: Some(HotkeySource::Device),
            backend: BackendPreference::Cpu,
            input_device: "alsa:default".to_owned(),
        };
        let text = toml::to_string(&cfg).expect("serialize config");
        assert!(text.contains("hotkey_source = \"device\""), "{text}");
        assert!(text.contains("language = \"de-DE\""), "{text}");
        assert!(text.contains("backend = \"cpu\""), "{text}");
        assert!(text.contains("input_device = \"alsa:default\""), "{text}");
        let back: Config = toml::from_str(&text).expect("deserialize config");
        assert_eq!(cfg, back);
    }

    #[test]
    fn unbound_hotkey_source_is_omitted() {
        let text = toml::to_string(&Config::default()).expect("serialize default config");
        assert!(!text.contains("hotkey_source"), "{text}");
        let back: Config = toml::from_str(&text).expect("deserialize default config");
        assert_eq!(back, Config::default());
    }

    #[test]
    fn serde_tokens_match_the_frozen_literals() {
        let languages = [
            (Language::Auto, "auto"),
            (Language::EnUs, "en-US"),
            (Language::DeDe, "de-DE"),
        ];
        let backends = [
            (BackendPreference::Auto, "auto"),
            (BackendPreference::Vulkan, "vulkan"),
            (BackendPreference::Cpu, "cpu"),
        ];
        let sources = [
            (HotkeySource::Portal, "portal"),
            (HotkeySource::Device, "device"),
        ];

        assert_eq!(Language::ALL.len(), languages.len());

        let mut cfg = Config::default();
        for (language, token) in languages {
            cfg.language = language;
            assert_eq!(language.label(), token);
            let line = format!("language = \"{token}\"");
            assert_eq!(assert_token_roundtrip(&cfg, &line).language, language);
        }
        for (backend, token) in backends {
            cfg.backend = backend;
            assert_eq!(backend.label(), token);
            let line = format!("backend = \"{token}\"");
            assert_eq!(assert_token_roundtrip(&cfg, &line).backend, backend);
        }
        for (source, token) in sources {
            cfg.hotkey_source = Some(source);
            let line = format!("hotkey_source = \"{token}\"");
            assert_eq!(
                assert_token_roundtrip(&cfg, &line).hotkey_source,
                Some(source)
            );
        }

        assert!(toml::from_str::<Config>("language = \"fr-FR\"\n").is_err());
    }

    #[test]
    fn language_labels_and_api_codes() {
        assert_eq!(Language::Auto.api_code(), None);
        assert_eq!(Language::EnUs.api_code(), Some("en-US"));
        assert_eq!(Language::DeDe.api_code(), Some("de-DE"));
        assert_eq!(Language::Auto.display_name(), "Auto-detect");
        assert_eq!(Language::EnUs.display_name(), "English (US)");
        assert_eq!(Language::DeDe.display_name(), "German (Germany)");
        assert_eq!(HotkeySource::Portal.label(), "portal");
        assert_eq!(HotkeySource::Device.label(), "device listener");
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path = temp_config_path("missing").with_file_name("absent-config.toml");
        assert!(!path.exists());
        let cfg = Config::load_from(&path).expect("missing file yields defaults");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn malformed_file_reports_path_and_parse_failure() {
        let path = temp_config_path("malformed");
        fs::write(&path, "this is = not = valid toml\n").expect("write malformed file");
        let err = Config::load_from(&path).expect_err("malformed toml must fail");
        let message = format!("{err:#}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("TOML parse error"), "{message}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn partial_file_uses_defaults_for_missing_fields() {
        let body = "model = \"nemotron-speech-streaming-en-0.6b-Q8_0\"\n";
        let path = temp_config_path("partial");
        fs::write(&path, body).expect("write partial file");
        let cfg = Config::load_from(&path).expect("partial file loads");
        assert_eq!(cfg.model, "nemotron-speech-streaming-en-0.6b-Q8_0");
        assert_eq!(cfg.hotkey, crate::hotkey::DEFAULT_CHORD);
        assert_eq!(cfg.language, Language::Auto);
        assert_eq!(cfg.backend, BackendPreference::Auto);
        assert_eq!(cfg.input_device, INPUT_DEVICE_SYSTEM);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_roundtrips_and_leaves_no_temp_file() {
        let path = temp_config_path("save");
        let cfg = Config {
            onboarding_complete: true,
            model: crate::model_catalog::DEFAULT_ARTIFACT_ID.to_owned(),
            language: Language::EnUs,
            hotkey: crate::hotkey::DEFAULT_CHORD.to_owned(),
            hotkey_source: Some(HotkeySource::Portal),
            backend: BackendPreference::Vulkan,
            input_device: "alsa:pipewire".to_owned(),
        };
        cfg.save_to(&path).expect("save config");
        assert_eq!(Config::load_from(&path).expect("reload config"), cfg);

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(
            !PathBuf::from(tmp).exists(),
            "the temporary file must be renamed away"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn shared_config_snapshot_clones_the_current_value() {
        let shared = SharedConfig::new(Config::default());
        assert_eq!(shared.snapshot(), Config::default());
    }

    #[test]
    fn config_path_uses_the_app_id_and_config_toml() {
        // Pure path computation: no directory is created and no user config is touched.
        let path = Config::path().expect("config dir resolvable in tests");
        assert!(path.is_absolute(), "{}", path.display());
        assert!(
            path.ends_with(Path::new(APP_ID).join("config.toml")),
            "{}",
            path.display()
        );
    }
}
