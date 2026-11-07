//! Chord parsing plus binding of exactly one dictation hotkey mechanism.
//!
//! FR-008: the XDG shortcut portal is preferred; the listen-only evdev device
//! listener is the fallback. The two are never active at the same time, and the
//! returned [`HotkeyHandle`] keeps whichever mechanism won alive — dropping the
//! handle stops it, so a rebind (settings) can never leave two mechanisms running.

use std::fmt;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use crate::config::HotkeySource;
use crate::hotkey_device::{DeviceBinding, DeviceKeepAlive};
use crate::hotkey_portal::{PortalBinding, PortalKeepAlive};

/// Default chord: both modifiers are held for the whole dictation.
pub const DEFAULT_CHORD: &str = "CTRL+SUPER";

/// Environment switch that disables the portal path (`LITTERA_HOTKEY_PORTAL=0`).
pub const OVERRIDE_PORTAL: &str = "LITTERA_HOTKEY_PORTAL";

/// Pinned settings text describing the trade-off of the device listener.
const DEVICE_LIMITATION_NOTE: &str =
    "device listener cannot suppress desktop bindings for the same chord";

/// Pinned remediation when the device listener has no readable input device.
const DEVICE_REMEDIATION: &str = "sudo usermod -aG input $USER && log out and back in";

/// Pinned detail when neither mechanism could be bound.
const NO_MECHANISM_DETAIL: &str = "no shortcut portal and /dev/input/event* not readable";

/// Remediation for a chord the parser rejects (the settings field is the cure).
const CHORD_REMEDIATION: &str =
    "enter a chord like CTRL+SUPER, CTRL+ALT+A or SUPER+SPACE in settings";

/// Tail of the pinned token error, kept separate so no line carries the whole
/// message.
const TOKEN_HELP: &str = "use CTRL, SUPER, ALT, SHIFT plus a letter, digit, F1–F12 or SPACE";

/// Press/release of the dictation chord. The portal reports both (hold-to-talk);
/// the device listener synthesises both from the set of pressed key codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Press,
    Release,
}

/// Modifier groups, in the canonical order used by [`Chord::canonical`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Super,
    Alt,
    Shift,
}

/// The single non-modifier token of a chord, if any. A modifier-only chord
/// (`CTRL+SUPER`) has `key == None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyToken {
    /// Uppercase `A`–`Z`.
    Letter(char),
    /// `0`–`9`.
    Digit(u8),
    /// `F1`–`F12`.
    F(u8),
    /// The space bar.
    Space,
}

/// A parsed, canonicalised key chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Modifiers in canonical order: `CTRL`, `SUPER`, `ALT`, `SHIFT`.
    pub modifiers: Vec<Modifier>,
    /// The non-modifier key, when the chord has one.
    pub key: Option<KeyToken>,
    /// Upper-case canonical rendering, e.g. `CTRL+SHIFT+A`.
    pub canonical: String,
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical)
    }
}

/// Parse a chord literal: case-insensitive input, upper-case canonical output.
///
/// Tokens are the four modifier names plus one optional `A`–`Z`, `0`–`9`,
/// `F1`–`F12` or `SPACE` token. Modifiers are re-ordered to
/// `CTRL`, `SUPER`, `ALT`, `SHIFT` and duplicates collapse.
pub fn parse_chord(raw: &str) -> anyhow::Result<Chord> {
    let tokens: Vec<&str> = raw.split('+').map(str::trim).collect();
    if tokens.len() < 2 {
        anyhow::bail!("a chord needs at least two tokens, e.g. CTRL+SUPER");
    }

    let mut modifiers: Vec<Modifier> = Vec::new();
    let mut key: Option<KeyToken> = None;

    for token in tokens {
        let upper = token.to_ascii_uppercase();
        match upper.as_str() {
            "CTRL" => modifiers.push(Modifier::Ctrl),
            "SUPER" => modifiers.push(Modifier::Super),
            "ALT" => modifiers.push(Modifier::Alt),
            "SHIFT" => modifiers.push(Modifier::Shift),
            "SPACE" => set_key(&mut key, KeyToken::Space, &upper)?,
            _ => {
                let parsed = parse_key_token(&upper).ok_or_else(|| token_error(&upper))?;
                set_key(&mut key, parsed, &upper)?;
            }
        }
    }

    // Canonical order CTRL, SUPER, ALT, SHIFT with duplicates collapsed.
    modifiers.sort_by_key(|modifier| modifier_rank(*modifier));
    modifiers.dedup();

    let mut canonical = String::new();
    for modifier in &modifiers {
        if !canonical.is_empty() {
            canonical.push('+');
        }
        canonical.push_str(modifier_label(*modifier));
    }
    if let Some(key_token) = key {
        if !canonical.is_empty() {
            canonical.push('+');
        }
        canonical.push_str(&key_label(key_token));
    }

    Ok(Chord {
        modifiers,
        key,
        canonical,
    })
}

/// Canonical form of a chord literal, e.g. `super+ctrl` → `CTRL+SUPER`.
pub fn normalize_chord(raw: &str) -> anyhow::Result<String> {
    Ok(parse_chord(raw)?.canonical)
}

/// Whether a compositor's report is the binding that was asked for.
///
/// `preferred_trigger` is only a hint: a portal that already holds a shortcut
/// for this application answers from its own store without asking anyone, and
/// the user would silently end up with that chord instead of the one they
/// recorded. Only an equal chord counts as bound; anything else sends the
/// binding to the device listener, which honours the chord as given.
fn portal_honoured(requested: &str, trigger: &str) -> bool {
    trigger_chord(trigger).is_some_and(|chord| chord == requested)
}

/// The chord a compositor's trigger description names, when it names one.
///
/// GNOME spells a bound keybinding as `Press <Modifier>key` — `Press <Alt>r`
/// for Alt+R — and reports a trigger that is already a chord (`Ctrl+Super`)
/// as-is. Anything else (another compositor's prose, a key Littera cannot
/// spell) is `None`: the requested chord stays the recorded one.
fn trigger_chord(trigger: &str) -> Option<String> {
    let trimmed = trigger.trim();
    let body = trimmed.strip_prefix("Press ").unwrap_or(trimmed);
    if !body.contains('<') {
        return normalize_chord(body).ok();
    }

    let mut tokens: Vec<&str> = Vec::new();
    let mut rest = body;
    while let Some(stripped) = rest.strip_prefix('<') {
        let (name, tail) = stripped.split_once('>')?;
        tokens.push(match name {
            "Control" | "Ctrl" | "Primary" => "CTRL",
            "Super" | "Meta" | "Mod4" => "SUPER",
            "Alt" | "Mod1" => "ALT",
            "Shift" => "SHIFT",
            _ => return None,
        });
        rest = tail;
    }
    if !rest.is_empty() {
        tokens.push(rest);
    }
    if tokens.is_empty() {
        return None;
    }

    normalize_chord(&tokens.join("+")).ok()
}

/// A bound mechanism: which chord is live and how it is delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub chord: String,
    pub trigger_description: String,
    pub source: HotkeySource,
    /// Why the portal was not used, when it was tried and refused and the
    /// device listener took over. `None` when the portal bound the shortcut
    /// itself, or when it was skipped by preference.
    pub portal_detail: Option<String>,
}

/// Why no mechanism could be bound, plus the remedy for the user.
#[derive(Debug, Clone)]
pub struct BindError {
    pub detail: String,
    pub remediation: String,
}

impl BindError {
    pub fn from_parts(detail: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            remediation: remediation.into(),
        }
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.remediation.is_empty() {
            f.write_str(&self.detail)
        } else {
            write!(f, "{} — {}", self.detail, self.remediation)
        }
    }
}

impl std::error::Error for BindError {}

/// Owns the bound mechanism and the event stream it produces.
///
/// The handle is cheap to clone: the mechanism and the receiver live behind one
/// `Arc`, and every clone keeps the mechanism running. Dropping the last clone
/// stops it (portal session closed / device threads retired), so a rebind can
/// never leave two mechanisms running. [`HotkeyHandle::take_receiver`] moves the
/// receiver out for a forwarder thread.
#[derive(Clone)]
pub struct HotkeyHandle {
    pub binding: Binding,
    inner: Arc<Inner>,
}

struct Inner {
    rx: Mutex<Option<Receiver<HotkeyEvent>>>,
    /// Dropped with the last clone: stops the mechanism.
    keepalive: Mechanism,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The payloads are opaque keep-alives; reading their liveness here is
        // what makes a mechanism that died silently visible (`alive=false`) in
        // the log at rebind or shutdown time.
        let (mechanism, alive) = self.keepalive.liveness();
        tracing::debug!(mechanism, alive, "hotkey mechanism retired");
    }
}

impl HotkeyHandle {
    /// Move the event receiver out of the handle, leaving `None` behind.
    pub fn take_receiver(&self) -> Option<Receiver<HotkeyEvent>> {
        self.inner
            .rx
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }

    fn new(binding: Binding, rx: Receiver<HotkeyEvent>, keepalive: Mechanism) -> Self {
        Self {
            binding,
            inner: Arc::new(Inner {
                rx: Mutex::new(Some(rx)),
                keepalive,
            }),
        }
    }
}

/// Owns whichever mechanism is active; the payloads exist to keep it alive
/// (CAS/portal session, device reader threads) and are read only for their
/// liveness (see [`Inner::drop`]).
enum Mechanism {
    Portal(PortalKeepAlive),
    Device(DeviceKeepAlive),
}

impl Mechanism {
    /// The mechanism's name and whether it was still running when it was read.
    fn liveness(&self) -> (&'static str, bool) {
        match self {
            Mechanism::Portal(keepalive) => ("portal", keepalive.is_alive()),
            Mechanism::Device(keepalive) => ("device listener", keepalive.is_alive()),
        }
    }
}

/// Bind exactly one mechanism.
///
/// The portal is tried first unless `preferred == Some(HotkeySource::Device)` or
/// `LITTERA_HOTKEY_PORTAL=0`; the device listener takes over when the portal is
/// unavailable, refuses, or binds nothing at all.
pub async fn bind(chord: &str, preferred: Option<HotkeySource>) -> Result<HotkeyHandle, BindError> {
    let parsed = parse_chord(chord)
        .map_err(|err| BindError::from_parts(err.to_string(), CHORD_REMEDIATION))?;

    let mut portal_detail: Option<String> = None;
    if portal_enabled(preferred) {
        match crate::hotkey_portal::bind(&parsed.canonical).await {
            Ok(bound) => {
                if portal_honoured(&parsed.canonical, &bound.trigger_description) {
                    let PortalBinding {
                        trigger_description,
                        rx,
                        keepalive,
                    } = bound;
                    tracing::info!(
                        chord = %parsed.canonical,
                        trigger = %trigger_description,
                        "dictation hotkey bound through the shortcut portal"
                    );
                    return Ok(HotkeyHandle::new(
                        Binding {
                            chord: parsed.canonical,
                            trigger_description,
                            source: HotkeySource::Portal,
                            portal_detail: None,
                        },
                        rx,
                        Mechanism::Portal(keepalive),
                    ));
                }

                // `preferred_trigger` is only a hint, and a portal that already
                // holds a shortcut for this application answers from its own
                // store instead — silently, with a chord nobody chose now.
                // Dropping the binding retires its forwarder, which closes the
                // session on its way out, so no second, different binding stays
                // alive while the device listener serves the chord that was
                // asked for.
                tracing::warn!(
                    chord = %parsed.canonical,
                    trigger = %bound.trigger_description,
                    "the shortcut portal did not bind the requested chord; the device listener takes over"
                );
                portal_detail = Some(format!(
                    "it bound `{}` instead of {}",
                    bound.trigger_description, parsed.canonical
                ));
                drop(bound);
            }
            Err(err) => {
                tracing::warn!(
                    chord = %parsed.canonical,
                    error = %err,
                    "shortcut portal binding unavailable; falling back to the device listener"
                );
                portal_detail = Some(err.to_string());
            }
        }
    } else if preferred == Some(HotkeySource::Device) {
        tracing::debug!(
            chord = %parsed.canonical,
            "portal binding skipped: the device listener is preferred"
        );
    } else {
        tracing::debug!(
            chord = %parsed.canonical,
            "portal binding skipped: disabled through the environment"
        );
    }

    match crate::hotkey_device::bind(&parsed) {
        Ok(bound) => {
            let DeviceBinding {
                trigger_description,
                rx,
                keepalive,
            } = bound;
            tracing::info!(
                chord = %parsed.canonical,
                "dictation hotkey bound through the device listener"
            );
            Ok(HotkeyHandle::new(
                Binding {
                    chord: parsed.canonical,
                    trigger_description,
                    source: HotkeySource::Device,
                    portal_detail,
                },
                rx,
                Mechanism::Device(keepalive),
            ))
        }
        Err(device_detail) => Err(BindError::from_parts(
            no_mechanism_detail(portal_detail, device_detail),
            DEVICE_REMEDIATION,
        )),
    }
}

/// Whether the portal path should be attempted at all.
fn portal_enabled(preferred: Option<HotkeySource>) -> bool {
    preferred != Some(HotkeySource::Device)
        && !std::env::var(OVERRIDE_PORTAL).is_ok_and(|value| value == "0")
}

/// Compose the `BindError.detail` for the "both mechanisms failed" case.
fn no_mechanism_detail(portal_detail: Option<String>, device_detail: String) -> String {
    match portal_detail {
        Some(portal) => {
            if device_detail == crate::hotkey_device::unreadable_detail() {
                NO_MECHANISM_DETAIL.to_owned()
            } else {
                format!("{NO_MECHANISM_DETAIL} ({portal}; {device_detail})")
            }
        }
        None => device_detail,
    }
}

/// Settings note about the device listener's listen-only trade-off.
pub fn device_limitation_note() -> &'static str {
    DEVICE_LIMITATION_NOTE
}

/// Whether at least one keyboard device is readable by this user.
pub fn device_available() -> bool {
    crate::hotkey_device::available()
}

/// Whether the `GlobalShortcuts` portal is present, probed without opening a
/// dialog (the app uses this to pick the onboarding binding flow).
pub async fn portal_available() -> bool {
    crate::hotkey_portal::available().await
}

/// Rank used for the canonical modifier order.
fn modifier_rank(modifier: Modifier) -> u8 {
    match modifier {
        Modifier::Ctrl => 0,
        Modifier::Super => 1,
        Modifier::Alt => 2,
        Modifier::Shift => 3,
    }
}

fn modifier_label(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::Ctrl => "CTRL",
        Modifier::Super => "SUPER",
        Modifier::Alt => "ALT",
        Modifier::Shift => "SHIFT",
    }
}

fn key_label(key: KeyToken) -> String {
    match key {
        KeyToken::Letter(letter) => letter.to_string(),
        KeyToken::Digit(digit) => digit.to_string(),
        KeyToken::F(number) => format!("F{number}"),
        KeyToken::Space => "SPACE".to_owned(),
    }
}

/// The pinned error for a token the chord grammar does not know.
fn token_error(token: &str) -> anyhow::Error {
    anyhow::anyhow!("unsupported chord token '{token}': {TOKEN_HELP}")
}

/// Parse one non-modifier token; `token` is already upper-cased.
fn parse_key_token(token: &str) -> Option<KeyToken> {
    let mut chars = token.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(letter), None, _) if letter.is_ascii_uppercase() => Some(KeyToken::Letter(letter)),
        (Some(digit), None, _) if digit.is_ascii_digit() => {
            Some(KeyToken::Digit(digit as u8 - b'0'))
        }
        (Some('F'), Some(first), rest)
            if first.is_ascii_digit() && rest.is_none_or(|digit| digit.is_ascii_digit()) =>
        {
            let value = token.get(1..)?.parse::<u8>().ok()?;
            (1..=12).contains(&value).then_some(KeyToken::F(value))
        }
        _ => None,
    }
}

fn set_key(key: &mut Option<KeyToken>, value: KeyToken, token: &str) -> anyhow::Result<()> {
    if key.is_some() {
        anyhow::bail!("chord has more than one key token ('{token}'): use one key plus modifiers");
    }
    *key = Some(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVALID_BOGUS: &str = "unsupported chord token 'BOGUS': use CTRL, SUPER, ALT, SHIFT plus a letter, digit, F1–F12 or SPACE";
    const INVALID_F13: &str = "unsupported chord token 'F13': use CTRL, SUPER, ALT, SHIFT plus a letter, digit, F1–F12 or SPACE";
    const INVALID_12: &str = "unsupported chord token '12': use CTRL, SUPER, ALT, SHIFT plus a letter, digit, F1–F12 or SPACE";
    const TOO_SHORT: &str = "a chord needs at least two tokens, e.g. CTRL+SUPER";

    fn canonical(raw: &str) -> String {
        normalize_chord(raw).unwrap()
    }

    #[test]
    fn a_portal_report_counts_as_bound_only_when_it_is_the_requested_chord() {
        // GNOME reports the accelerator as `Press <Modifier>key`; every
        // spelling Littera understands has to arrive as the requested chord.
        assert!(portal_honoured("ALT+R", "Press <Alt>r"));
        assert!(portal_honoured("CTRL+SHIFT+F9", "Press <Control><Shift>F9"));
        assert!(portal_honoured("CTRL+SUPER", "Ctrl+Super"));
        assert!(portal_honoured("CTRL+SUPER", "Press <Control><Super>"));
        assert!(portal_honoured("ALT+SHIFT+SPACE", "alt+shift+space"));
    }

    #[test]
    fn a_different_or_unreadable_portal_report_sends_the_binding_to_the_device_listener() {
        // A portal that already holds a shortcut for this application answers
        // from its store: that is not the chord the user just recorded, and it
        // must never replace it silently.
        assert!(!portal_honoured("CTRL+ALT", "Press <Alt>r"));
        assert!(!portal_honoured("CTRL+SUPER", ""));
        assert!(!portal_honoured("CTRL+SUPER", "nothing bound"));
        assert!(!portal_honoured("CTRL+SUPER", "Press <Alt>Page_Up"));
        assert!(!portal_honoured("CTRL+SUPER", "Press <Hyper>x"));
    }

    #[test]
    fn canonicalises_modifier_order() {
        assert_eq!(canonical("super+ctrl"), "CTRL+SUPER");
        assert_eq!(canonical("shift+alt+super+ctrl"), "CTRL+SUPER+ALT+SHIFT");
        assert_eq!(canonical("shift+ctrl"), "CTRL+SHIFT");
        assert_eq!(canonical("ctrl+super"), DEFAULT_CHORD);
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(canonical("Ctrl+Super"), "CTRL+SUPER");
        assert_eq!(canonical("ctrl+f5"), "CTRL+F5");
        assert_eq!(canonical("ctrl+f12"), "CTRL+F12");
        assert_eq!(canonical("alt+space"), "ALT+SPACE");
        assert_eq!(canonical("super+space"), "SUPER+SPACE");
        assert_eq!(canonical("ctrl+shift+a"), "CTRL+SHIFT+A");
        assert_eq!(canonical("ctrl+alt+3"), "CTRL+ALT+3");
        assert_eq!(canonical("ctrl+0"), "CTRL+0");
    }

    #[test]
    fn keeps_the_key_last_and_recorded() {
        let chord = parse_chord("A+ALT+CTRL").unwrap();
        assert_eq!(chord.canonical, "CTRL+ALT+A");
        assert_eq!(chord.modifiers, vec![Modifier::Ctrl, Modifier::Alt]);
        assert_eq!(chord.key, Some(KeyToken::Letter('A')));
        assert_eq!(chord.to_string(), "CTRL+ALT+A");
    }

    #[test]
    fn modifier_only_chord_has_no_key() {
        let chord = parse_chord(DEFAULT_CHORD).unwrap();
        assert_eq!(chord.key, None);
        assert_eq!(chord.modifiers.len(), 2);
        assert_eq!(chord.canonical, DEFAULT_CHORD);
    }

    #[test]
    fn duplicate_modifiers_collapse() {
        assert_eq!(canonical("ctrl+ctrl+super"), "CTRL+SUPER");
    }

    #[test]
    fn digit_keys_keep_their_value() {
        assert_eq!(parse_chord("CTRL+7").unwrap().key, Some(KeyToken::Digit(7)));
        assert_eq!(parse_chord("CTRL+0").unwrap().key, Some(KeyToken::Digit(0)));
        assert_eq!(parse_chord("CTRL+f1").unwrap().key, Some(KeyToken::F(1)));
    }

    #[test]
    fn rejects_unknown_tokens_with_the_pinned_literal() {
        assert_eq!(
            parse_chord("CTRL+BOGUS").unwrap_err().to_string(),
            INVALID_BOGUS
        );
        assert_eq!(
            parse_chord("ctrl+bogus").unwrap_err().to_string(),
            INVALID_BOGUS
        );
        assert_eq!(
            parse_chord("CTRL+F13").unwrap_err().to_string(),
            INVALID_F13
        );
        assert_eq!(parse_chord("CTRL+12").unwrap_err().to_string(), INVALID_12);
    }

    #[test]
    fn rejects_chords_that_are_too_short() {
        assert_eq!(parse_chord("CTRL").unwrap_err().to_string(), TOO_SHORT);
        assert_eq!(parse_chord("A").unwrap_err().to_string(), TOO_SHORT);
        assert_eq!(parse_chord("").unwrap_err().to_string(), TOO_SHORT);
    }

    #[test]
    fn rejects_more_than_one_key_token() {
        let err = parse_chord("CTRL+A+B").unwrap_err().to_string();
        assert_eq!(
            err,
            "chord has more than one key token ('B'): use one key plus modifiers"
        );
    }

    #[test]
    fn bind_error_exposes_detail_and_remediation() {
        let err = BindError::from_parts("broken", "fix it");
        assert_eq!(err.detail, "broken");
        assert_eq!(err.remediation, "fix it");
        assert_eq!(err.to_string(), "broken — fix it");
        assert_eq!(BindError::from_parts("broken", "").to_string(), "broken");
    }

    #[test]
    fn device_limitation_note_is_the_pinned_literal() {
        assert_eq!(
            device_limitation_note(),
            "device listener cannot suppress desktop bindings for the same chord"
        );
    }

    #[test]
    fn device_detail_stays_pinned_when_nothing_is_readable() {
        let portal = Some("shortcut portal error: boom".to_owned());
        let detail = no_mechanism_detail(portal, crate::hotkey_device::unreadable_detail());
        assert_eq!(detail, NO_MECHANISM_DETAIL);

        let device_specific = "cannot open /dev/input/event3".to_owned();
        assert_eq!(
            no_mechanism_detail(None, device_specific.clone()),
            device_specific
        );
        let composed = no_mechanism_detail(Some("nothing bound".to_owned()), device_specific);
        assert!(composed.contains("nothing bound"));
        assert!(composed.starts_with(NO_MECHANISM_DETAIL));
    }

    #[test]
    fn the_device_preference_never_uses_the_portal() {
        // `portal_enabled` also consults $LITTERA_HOTKEY_PORTAL; the device
        // preference must win regardless of the environment.
        assert!(!portal_enabled(Some(HotkeySource::Device)));
    }
}
