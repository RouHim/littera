//! Persisted settings (FR-016) and the shared, live-reloadable configuration (Scenario 3.4).
//!
//! Every subsystem reads its configuration through [`SharedConfig`], so a change made in the
//! settings window is visible to the next dictation without restarting the process.

use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Application id: the XDG directory name and the single-instance/tray identifier.
pub const APP_ID: &str = "littera";

/// The distinguished `input_device` value — and the meaning of an absent key: record from the
/// microphone the system's audio layer currently designates (FR-001).
pub const INPUT_DEVICE_SYSTEM: &str = "system";

/// Transcription language hint. The model always gets an explicit language or none at all
/// (`auto`), which is why [`Language::api_code`] is the only source of this value (FR-007).
///
/// The vocabulary is [`SUPPORTED`]: every language the bundled model catalog can transcribe,
/// one locale per language, so both selectors offer exactly what the app can actually serve —
/// no language is singled out and nothing assumes English as the only alternative. The engine
/// spelling still follows the loaded model: [`crate::model_catalog::language_hint`] falls back
/// to the bare language (`fr` for `fr-FR`) where a variant advertises only that, and a language
/// the variant does not serve at all runs without a hint (the FR-011 warning, never a
/// rejection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language(Option<&'static str>);

/// Every language the bundled catalog can transcribe: (locale code, display name), ascending by
/// display name. The union of the `general.languages` entries the
/// [`crate::model_catalog::VARIANTS`] snapshot advertises — Nemotron 3.5 lists locale codes
/// (`de-DE`, `en-GB`, …), Voxtral bare ones (`de`, `en`, …) — canonicalised to one locale per
/// language in the spelling the config token and the engine hint use.
const SUPPORTED: [(&str, &str); 32] = [
    ("ar-AR", "Arabic"),
    ("bg-BG", "Bulgarian"),
    ("zh-CN", "Chinese"),
    ("hr-HR", "Croatian"),
    ("cs-CZ", "Czech"),
    ("da-DK", "Danish"),
    ("nl-NL", "Dutch"),
    ("en-GB", "English (UK)"),
    ("en-US", "English (US)"),
    ("et-EE", "Estonian"),
    ("fi-FI", "Finnish"),
    ("fr-CA", "French (Canada)"),
    ("fr-FR", "French (France)"),
    ("de-DE", "German"),
    ("hi-IN", "Hindi"),
    ("hu-HU", "Hungarian"),
    ("it-IT", "Italian"),
    ("ja-JP", "Japanese"),
    ("ko-KR", "Korean"),
    ("nb-NO", "Norwegian Bokmål"),
    ("pl-PL", "Polish"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("pt-PT", "Portuguese (Portugal)"),
    ("ro-RO", "Romanian"),
    ("ru-RU", "Russian"),
    ("sk-SK", "Slovak"),
    ("es-ES", "Spanish (Spain)"),
    ("es-US", "Spanish (US)"),
    ("sv-SE", "Swedish"),
    ("tr-TR", "Turkish"),
    ("uk-UA", "Ukrainian"),
    ("vi-VN", "Vietnamese"),
];

impl Language {
    /// Auto-detect: no hint at all, the model picks the language per recording (FR-007).
    pub const AUTO: Self = Self(None);

    /// Every language in UI order — `Auto` first, then [`SUPPORTED`] by display name.
    pub const ALL: [Language; SUPPORTED.len() + 1] = {
        let mut all = [Self::AUTO; SUPPORTED.len() + 1];
        let mut index = 0;
        while index < SUPPORTED.len() {
            all[index + 1] = Self::new(SUPPORTED[index].0);
            index += 1;
        }
        all
    };

    /// The language with this locale code; it must be one of the [`SUPPORTED`] codes — that is
    /// the vocabulary the config token, the selectors and the engine hint are built from.
    pub const fn new(code: &'static str) -> Self {
        Self(Some(code))
    }

    /// Resolve a stored token: `auto`, or the code of a [`SUPPORTED`] language.
    fn from_token(token: &str) -> Option<Self> {
        if token == "auto" {
            return Some(Self::AUTO);
        }
        SUPPORTED
            .iter()
            .find(|(code, _)| *code == token)
            .map(|(code, _)| Self::new(code))
    }

    /// The engine language hint — the only source of it (FR-007). `None` means `auto`.
    pub const fn api_code(self) -> Option<&'static str> {
        self.0
    }

    /// Whether no explicit language is configured.
    pub const fn is_auto(self) -> bool {
        self.0.is_none()
    }

    /// The (locale, bare language) codes of an explicit language; `None` for `auto`.
    ///
    /// `de-DE` → `("de-DE", "de")`. The catalog matches on both: a variant may advertise the
    /// locale, the bare code, or several locales of the same language.
    pub fn codes(self) -> Option<(&'static str, &'static str)> {
        let code = self.0?;
        let bare = code.split_once('-').map_or(code, |(bare, _)| bare);
        Some((code, bare))
    }

    /// The stable token used in the config file.
    pub const fn label(self) -> &'static str {
        match self.0 {
            None => "auto",
            Some(code) => code,
        }
    }

    /// The human-readable name for the UI.
    pub fn display_name(self) -> &'static str {
        match self.0 {
            None => "Auto-detect",
            Some(code) => SUPPORTED
                .iter()
                .find(|(entry, _)| *entry == code)
                .map_or(code, |(_, name)| *name),
        }
    }
}

impl std::fmt::Display for Language {
    /// The name every pick list renders, in the closed field and in the open menu.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

impl serde::Serialize for Language {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl<'de> serde::Deserialize<'de> for Language {
    /// Read a hand-edited file leniently (the SC-005 style): an unknown token falls back to
    /// `auto` instead of failing the whole file. Only ever called for TOML input.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = <String as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self::from_token(&token).unwrap_or(Self::AUTO))
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

/// Model idle-unload timeout (FR-006): Never, 1, 5, 15, 30 or 60 minutes.
///
/// Serialized as the [`IdleUnload::label`] token; a hand-edited file may also
/// carry the minute count as an integer. Anything unrecognized falls back to the
/// default instead of failing the whole file (SC-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum IdleUnload {
    #[serde(rename = "never")]
    Never,
    #[serde(rename = "1")]
    OneMinute,
    #[serde(rename = "5")]
    FiveMinutes,
    #[serde(rename = "15")]
    FifteenMinutes,
    #[serde(rename = "30")]
    ThirtyMinutes,
    #[serde(rename = "60")]
    SixtyMinutes,
}

impl IdleUnload {
    /// Every preset, in UI order.
    pub const ALL: [IdleUnload; 6] = [
        Self::Never,
        Self::OneMinute,
        Self::FiveMinutes,
        Self::FifteenMinutes,
        Self::ThirtyMinutes,
        Self::SixtyMinutes,
    ];

    /// The stable token used in the config file.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OneMinute => "1",
            Self::FiveMinutes => "5",
            Self::FifteenMinutes => "15",
            Self::ThirtyMinutes => "30",
            Self::SixtyMinutes => "60",
        }
    }

    /// The human-readable name for the settings UI.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::OneMinute => "1 minute",
            Self::FiveMinutes => "5 minutes",
            Self::FifteenMinutes => "15 minutes",
            Self::ThirtyMinutes => "30 minutes",
            Self::SixtyMinutes => "60 minutes",
        }
    }

    /// The idle timeout, or `None` for [`IdleUnload::Never`] (FR-013).
    pub const fn timeout(self) -> Option<Duration> {
        match self {
            Self::Never => None,
            Self::OneMinute => Some(Duration::from_secs(60)),
            Self::FiveMinutes => Some(Duration::from_secs(5 * 60)),
            Self::FifteenMinutes => Some(Duration::from_secs(15 * 60)),
            Self::ThirtyMinutes => Some(Duration::from_secs(30 * 60)),
            Self::SixtyMinutes => Some(Duration::from_secs(60 * 60)),
        }
    }

    /// The preset for a config token; `None` for an unrecognized token.
    fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|value| value.label() == token)
    }

    /// The preset for a raw minute count; `None` when it is not a preset.
    fn from_minutes(minutes: i64) -> Option<Self> {
        match minutes {
            1 => Some(Self::OneMinute),
            5 => Some(Self::FiveMinutes),
            15 => Some(Self::FifteenMinutes),
            30 => Some(Self::ThirtyMinutes),
            60 => Some(Self::SixtyMinutes),
            _ => None,
        }
    }
}

impl Default for IdleUnload {
    /// FR-006: 15 minutes — long enough that a normal dictation pause does not
    /// pay for a reload, short enough to free the memory during a work break.
    fn default() -> Self {
        Self::FifteenMinutes
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
    /// Model idle-unload timeout (FR-006); the token of [`IdleUnload::label`] or
    /// a minute count, always written. Anything else falls back to the default.
    #[serde(default, deserialize_with = "idle_unload_from_toml")]
    pub idle_unload: IdleUnload,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            onboarding_complete: false,
            model: crate::model_catalog::DEFAULT_ARTIFACT_ID.to_owned(),
            language: Language::AUTO,
            hotkey: crate::hotkey::DEFAULT_CHORD.to_owned(),
            hotkey_source: None,
            backend: BackendPreference::Auto,
            input_device: INPUT_DEVICE_SYSTEM.to_owned(),
            idle_unload: IdleUnload::FifteenMinutes,
        }
    }
}

impl Config {
    /// `$XDG_CONFIG_HOME/littera/config.toml`, falling back to `~/.config/littera/config.toml`.
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

    /// `$XDG_DATA_HOME/littera/models`, created on demand (FR-003).
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

/// Read the idle timeout leniently (SC-005): the [`IdleUnload::label`] token as a
/// string or the minute count as an integer; anything else — an unknown token, a
/// count that is not a preset, a bool or a float — falls back to the default
/// instead of failing the whole file. Only ever called for TOML input.
fn idle_unload_from_toml<'de, D>(deserializer: D) -> Result<IdleUnload, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = <toml::Value as serde::Deserialize>::deserialize(deserializer)?;
    let parsed = match &raw {
        toml::Value::String(token) => IdleUnload::from_token(token),
        toml::Value::Integer(minutes) => IdleUnload::from_minutes(*minutes),
        _ => None,
    };
    Ok(parsed.unwrap_or_default())
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
        let dir = std::env::temp_dir().join(format!("littera-config-{}-{tag}", std::process::id()));
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
        assert_eq!(cfg.language, Language::AUTO);
        assert_eq!(cfg.backend, BackendPreference::Auto);
        assert!(!cfg.onboarding_complete);
        assert_eq!(cfg.hotkey_source, None);
        assert_eq!(cfg.input_device, INPUT_DEVICE_SYSTEM);
        assert_eq!(cfg.idle_unload, IdleUnload::FifteenMinutes);
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
            language: Language::new("de-DE"),
            hotkey: "CTRL+SHIFT+D".to_owned(),
            hotkey_source: Some(HotkeySource::Device),
            backend: BackendPreference::Cpu,
            input_device: "alsa:default".to_owned(),
            idle_unload: IdleUnload::SixtyMinutes,
        };
        let text = toml::to_string(&cfg).expect("serialize config");
        assert!(text.contains("hotkey_source = \"device\""), "{text}");
        assert!(text.contains("language = \"de-DE\""), "{text}");
        assert!(text.contains("backend = \"cpu\""), "{text}");
        assert!(text.contains("input_device = \"alsa:default\""), "{text}");
        assert!(text.contains("idle_unload = \"60\""), "{text}");
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
            (Language::AUTO, "auto"),
            (Language::new("en-US"), "en-US"),
            (Language::new("de-DE"), "de-DE"),
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

        assert!(toml::from_str::<Config>("language = \"fr-FR\"\n").is_ok());
    }

    #[test]
    fn every_supported_language_roundtrips_and_unknown_tokens_fall_back_to_auto() {
        let base = Config::default();
        for language in Language::ALL {
            let cfg = Config {
                language,
                ..base.clone()
            };
            let line = format!("language = \"{}\"", language.label());
            assert_eq!(assert_token_roundtrip(&cfg, &line).language, language);
        }

        // A hand-edited token outside the table has no third state: the file stays
        // loadable and the value is the default (SC-005 leniency).
        let cfg: Config =
            toml::from_str("language = \"xx-XX\"\n").expect("an unknown language must load");
        assert_eq!(cfg.language, Language::AUTO);
    }

    #[test]
    fn the_language_table_is_unique_and_ordered_by_display_name() {
        assert_eq!(Language::ALL.len(), SUPPORTED.len() + 1);
        assert_eq!(Language::ALL[0], Language::AUTO);

        let names: Vec<&str> = Language::ALL[1..]
            .iter()
            .map(|language| language.display_name())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "the pick list order is the display-name order"
        );

        let mut codes: Vec<&str> = Language::ALL
            .iter()
            .map(|language| language.label())
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), Language::ALL.len(), "codes must be unique");

        // Every explicit language carries a locale and its bare language.
        for language in Language::ALL.iter().skip(1) {
            let (code, bare) = language.codes().expect("an explicit language has codes");
            assert_eq!(code, language.label());
            assert_eq!(
                Some(bare),
                code.split_once('-').map(|(bare, _)| bare),
                "{code} must split into language and region"
            );
        }
        assert_eq!(Language::AUTO.codes(), None);
        assert!(Language::AUTO.is_auto());
        assert!(
            Language::ALL[1..]
                .iter()
                .all(|language| !language.is_auto())
        );
    }

    #[test]
    fn language_labels_and_api_codes() {
        assert_eq!(Language::AUTO.api_code(), None);
        assert_eq!(Language::AUTO.label(), "auto");
        assert_eq!(Language::AUTO.display_name(), "Auto-detect");
        assert_eq!(Language::new("en-US").api_code(), Some("en-US"));
        assert_eq!(Language::new("en-US").display_name(), "English (US)");
        assert_eq!(Language::new("de-DE").api_code(), Some("de-DE"));
        assert_eq!(Language::new("de-DE").display_name(), "German");
        assert_eq!(Language::new("fr-FR").display_name(), "French (France)");
        assert_eq!(Language::new("de-DE").to_string(), "German");
        assert_eq!(Language::AUTO.to_string(), "Auto-detect");
        assert_eq!(HotkeySource::Portal.label(), "portal");
        assert_eq!(HotkeySource::Device.label(), "device listener");
    }

    #[test]
    fn idle_unload_defaults_to_fifteen_minutes() {
        // FR-006: the default timeout is 15 minutes.
        assert_eq!(Config::default().idle_unload, IdleUnload::FifteenMinutes);
    }

    #[test]
    fn idle_unload_tokens_and_durations_are_frozen() {
        let frozen = [
            (IdleUnload::Never, "never", "Never", None),
            (IdleUnload::OneMinute, "1", "1 minute", Some(1)),
            (IdleUnload::FiveMinutes, "5", "5 minutes", Some(5)),
            (IdleUnload::FifteenMinutes, "15", "15 minutes", Some(15)),
            (IdleUnload::ThirtyMinutes, "30", "30 minutes", Some(30)),
            (IdleUnload::SixtyMinutes, "60", "60 minutes", Some(60)),
        ];
        assert_eq!(IdleUnload::ALL.len(), frozen.len());

        for (value, token, name, minutes) in frozen {
            assert_eq!(value.label(), token);
            assert_eq!(value.display_name(), name);
            assert_eq!(
                value.timeout(),
                minutes.map(|minutes: u64| Duration::from_secs(minutes * 60))
            );
        }

        let cfg = Config {
            idle_unload: IdleUnload::FiveMinutes,
            ..Config::default()
        };
        assert_eq!(
            assert_token_roundtrip(&cfg, "idle_unload = \"5\"").idle_unload,
            IdleUnload::FiveMinutes
        );

        // Always written, also at its default (SC-005).
        let default_text = toml::to_string(&Config::default()).expect("serialize default config");
        assert!(
            default_text.contains("idle_unload = \"15\""),
            "{default_text}"
        );

        // A hand-edited minute count is accepted as well (SC-005).
        let path = temp_config_path("minutes");
        fs::write(&path, "idle_unload = 30\n").expect("write config");
        let cfg = Config::load_from(&path).expect("integer minutes load");
        assert_eq!(cfg.idle_unload, IdleUnload::ThirtyMinutes);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn unrecognized_idle_unload_falls_back_to_the_default() {
        let cases = [
            ("idle_unload = \"banana\"\n", Language::AUTO),
            (
                "idle_unload = 7\nlanguage = \"de-DE\"\n",
                Language::new("de-DE"),
            ),
            ("idle_unload = true\n", Language::AUTO),
            ("idle_unload = 2.5\n", Language::AUTO),
        ];
        for (body, language) in cases {
            let path = temp_config_path("unrecognized-idle-unload");
            fs::write(&path, body).expect("write config");
            let cfg =
                Config::load_from(&path).expect("an unrecognized value must not fail the file");
            assert_eq!(cfg.idle_unload, IdleUnload::FifteenMinutes, "{body}");
            assert_eq!(cfg.language, language, "{body}");
            let _ = fs::remove_file(&path);
        }
    }

    #[test]
    fn idle_unload_survives_a_save_load_roundtrip() {
        let path = temp_config_path("idle-unload-roundtrip");
        let cfg = Config {
            idle_unload: IdleUnload::OneMinute,
            ..Config::default()
        };
        cfg.save_to(&path).expect("save config");
        assert_eq!(Config::load_from(&path).expect("reload config"), cfg);
        let _ = fs::remove_file(&path);
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
        assert_eq!(cfg.language, Language::AUTO);
        assert_eq!(cfg.backend, BackendPreference::Auto);
        assert_eq!(cfg.input_device, INPUT_DEVICE_SYSTEM);
        assert_eq!(cfg.idle_unload, IdleUnload::FifteenMinutes);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_roundtrips_and_leaves_no_temp_file() {
        let path = temp_config_path("save");
        let cfg = Config {
            onboarding_complete: true,
            model: crate::model_catalog::DEFAULT_ARTIFACT_ID.to_owned(),
            language: Language::new("en-US"),
            hotkey: crate::hotkey::DEFAULT_CHORD.to_owned(),
            hotkey_source: Some(HotkeySource::Portal),
            backend: BackendPreference::Vulkan,
            input_device: "alsa:pipewire".to_owned(),
            idle_unload: IdleUnload::SixtyMinutes,
        };
        cfg.save_to(&path).expect("save config");
        assert_eq!(Config::load_from(&path).expect("reload config"), cfg);

        let text = fs::read_to_string(&path).expect("read saved config");
        assert!(text.contains("idle_unload = \"60\""), "{text}");

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
