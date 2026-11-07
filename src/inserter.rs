//! Text insertion: clipboard first, then a synthesized `Ctrl+V` (FR-012, FR-013).
//!
//! This module is the only place a selection is set and the only place a paste chord is
//! synthesized: the chord is written to a `/dev/uinput` virtual keyboard that the compositor picks
//! up like a real device — no `ydotool`, no external paste tool, no shelling out.
//!
//! Ordering invariant: the transcript reaches the clipboard *before* the chord is emitted,
//! otherwise the focused application pastes the previous clipboard content (FR-013). The text
//! selection that paste displaces is read first and put back once the chord had its chance, so a
//! dictation does not eat the user's clipboard (FR-012); a copy that arrives in between wins. When
//! `/dev/uinput` is not writable the app degrades to copying only (Scenario 4.1, SC-005) and says
//! so, naming the blocked path — in that mode the transcript *stays* on the clipboard, because
//! it is the only way the text reaches the user.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use evdev::{AttributeSet, InputEvent, KeyCode, KeyEvent, uinput::VirtualDevice};

use crate::clipboard::Clipboard;

/// The uinput device node the synthesized paste needs write access to.
pub const UINPUT_PATH: &str = "/dev/uinput";

/// Name the virtual keyboard advertises to the compositor.
const VIRTUAL_KEYBOARD_NAME: &str = "littera virtual keyboard";

/// Time between the chord press and the key release. The press needs its own report for the focused
/// toolkit to see it, and 25 ms gives the compositor room to publish the new selection before the
/// paste lands: the clipboard confirms the selection itself, this margin covers the target
/// application's fetch.
const PASTE_CHORD_HOLD: Duration = Duration::from_millis(25);

/// Time between the paste chord and putting the displaced selection back (FR-012).
///
/// The focused application fetches the transcript in reaction to the chord; this grace covers that
/// fetch — including a slowed-down toolkit — before the old text returns. Restore too early and
/// the paste would land on the old content instead of the transcript.
const CLIPBOARD_RESTORE_GRACE: Duration = Duration::from_millis(500);

/// `input_event` values for a pressed and a released key.
const KEY_PRESS: i32 = 1;
const KEY_RELEASE: i32 = 0;

/// Reason used when an earlier panic left the virtual keyboard unusable.
const POISONED: &str = "virtual keyboard is poisoned after an earlier panic — restart Littera";

/// `VirtualDevice::builder` failure: the device node is missing or not writable.
const OPEN_FAILED: &str = "cannot open /dev/uinput — is uinput available and writable?";
/// `VirtualDeviceBuilder::with_keys` failure: the capability ioctls were refused.
const KEYS_FAILED: &str = "cannot advertise KEY_LEFTCTRL/KEY_V on /dev/uinput";
/// `VirtualDeviceBuilder::build` failure: registering the device was refused.
const REGISTER_FAILED: &str = "cannot register the /dev/uinput virtual keyboard";
/// `VirtualDevice::emit` failure: the chord could not be written to the kernel.
const EMIT_FAILED: &str = "cannot write the Ctrl+V chord to the virtual keyboard";

/// What happened to a transcript handed to the focused application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// Clipboard updated and the paste chord emitted.
    Inserted,
    /// Auto-paste is unavailable: the transcript is on the clipboard, `reason` says why.
    CopiedOnly {
        reason: String,
        blocked_path: Option<PathBuf>,
    },
}

/// Inserts a transcript into the focused application.
pub trait TextInserter: Send {
    fn insert(&self, text: &str) -> Result<InsertOutcome>;
}

/// Probe for diagnostics and the settings surface: is auto-paste usable right now?
///
/// The error text is shown verbatim to the user.
pub fn uinput_writable() -> Result<(), String> {
    let opened = std::fs::OpenOptions::new().write(true).open(UINPUT_PATH);
    match opened {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("/dev/uinput ({err})")),
    }
}

/// The two key codes the paste chord is built from.
fn paste_chord_keys() -> AttributeSet<KeyCode> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_LEFTCTRL);
    keys.insert(KeyCode::KEY_V);
    keys
}

/// The chord press. `VirtualDevice::emit` terminates the batch with a `SYN_REPORT`.
fn paste_chord_press() -> [InputEvent; 2] {
    [
        KeyEvent::new(KeyCode::KEY_LEFTCTRL, KEY_PRESS).into(),
        KeyEvent::new(KeyCode::KEY_V, KEY_PRESS).into(),
    ]
}

/// The chord release, emitted as a second report after [`PASTE_CHORD_HOLD`].
fn paste_chord_release() -> [InputEvent; 2] {
    [
        KeyEvent::new(KeyCode::KEY_V, KEY_RELEASE).into(),
        KeyEvent::new(KeyCode::KEY_LEFTCTRL, KEY_RELEASE).into(),
    ]
}

/// Inserts by writing a `Ctrl+V` chord to a `/dev/uinput` virtual keyboard.
///
/// `insert` takes `&self` because the app shares the inserter through a trait object, while the
/// device itself needs `&mut self` to emit — so it lives behind a `Mutex`.
pub struct UinputInserter {
    clipboard: Arc<Clipboard>,
    device: Mutex<VirtualDevice>,
}

impl UinputInserter {
    /// Create the virtual keyboard.
    ///
    /// Fails when `/dev/uinput` cannot be opened or the device cannot be registered; the caller
    /// falls back to [`ClipboardOnlyInserter`].
    pub fn new(clipboard: Arc<Clipboard>) -> Result<Self> {
        let device = VirtualDevice::builder()
            .context(OPEN_FAILED)?
            .name(VIRTUAL_KEYBOARD_NAME)
            .with_keys(&paste_chord_keys())
            .context(KEYS_FAILED)?
            .build()
            .context(REGISTER_FAILED)?;
        tracing::info!(
            device = VIRTUAL_KEYBOARD_NAME,
            "auto-paste enabled via /dev/uinput",
        );
        Ok(UinputInserter {
            clipboard,
            device: Mutex::new(device),
        })
    }
}

impl TextInserter for UinputInserter {
    fn insert(&self, text: &str) -> Result<InsertOutcome> {
        // The text the paste is about to displace is read first (FR-012); without it the
        // dictation would silently eat whatever the user copied earlier.
        let displaced = self.clipboard.snapshot();
        // Invariant: the selection must be ours before the chord is emitted (FR-013).
        self.clipboard.copy_text(text)?;
        let mut device = self.device.lock().map_err(|_| anyhow::anyhow!(POISONED))?;
        device.emit(&paste_chord_press()).context(EMIT_FAILED)?;
        std::thread::sleep(PASTE_CHORD_HOLD);
        device.emit(&paste_chord_release()).context(EMIT_FAILED)?;
        drop(device);
        // Only after the grace may the previous selection return (FR-012).
        std::thread::sleep(CLIPBOARD_RESTORE_GRACE);
        restore_displaced(&self.clipboard, displaced);
        tracing::info!(
            chars = text.chars().count(),
            "transcript pasted with Ctrl+V",
        );
        Ok(InsertOutcome::Inserted)
    }
}

/// Put the selection auto-paste displaced back, best effort (FR-012).
///
/// Never fails the insertion: `None` means the snapshot read no text (nothing to put back, the
/// transcript stays), `Ok(false)` means a newer copy won and is kept.
fn restore_displaced(clipboard: &Clipboard, displaced: Option<String>) {
    let Some(previous) = displaced else {
        tracing::debug!("no text selection was displaced; the transcript stays on the clipboard");
        return;
    };
    match clipboard.restore(&previous) {
        Ok(true) => tracing::info!(
            chars = previous.chars().count(),
            "previous clipboard content restored",
        ),
        Ok(false) => tracing::debug!("a newer copy arrived; keeping it on the clipboard"),
        Err(err) => tracing::warn!(%err, "restoring the displaced clipboard content failed"),
    }
}

/// Fallback used when auto-paste is unavailable: the transcript still reaches the clipboard and the
/// user pastes it with `Ctrl+V` — so it *stays* there and nothing is restored (FR-012).
pub struct ClipboardOnlyInserter {
    clipboard: Arc<Clipboard>,
    reason: String,
}

impl ClipboardOnlyInserter {
    pub fn new(clipboard: Arc<Clipboard>, reason: String) -> Self {
        ClipboardOnlyInserter { clipboard, reason }
    }
}

impl TextInserter for ClipboardOnlyInserter {
    fn insert(&self, text: &str) -> Result<InsertOutcome> {
        self.clipboard.copy_text(text)?;
        tracing::info!(
            chars = text.chars().count(),
            reason = %self.reason,
            "transcript copied to the clipboard; auto-paste unavailable",
        );
        Ok(InsertOutcome::CopiedOnly {
            reason: self.reason.clone(),
            blocked_path: Some(PathBuf::from(UINPUT_PATH)),
        })
    }
}

/// Pick the best available inserter.
///
/// Returns the inserter plus `Some(reason)` when auto-paste is unavailable: `reason` is the
/// user-visible explanation — the [`uinput_writable`] error text, or why the virtual keyboard could
/// not be built. Never fails, because the app has to stay usable with the clipboard path alone.
pub fn select(clipboard: Arc<Clipboard>) -> (Box<dyn TextInserter>, Option<String>) {
    if let Err(reason) = uinput_writable() {
        tracing::warn!(
            reason = %reason,
            "auto-paste unavailable; falling back to clipboard-only",
        );
        let inserter = ClipboardOnlyInserter::new(clipboard, reason.clone());
        return (Box::new(inserter), Some(reason));
    }

    match UinputInserter::new(Arc::clone(&clipboard)) {
        Ok(inserter) => (Box::new(inserter), None),
        Err(err) => {
            let reason = format!("{err:#}");
            tracing::warn!(
                reason = %reason,
                "auto-paste unavailable; falling back to clipboard-only",
            );
            let inserter = ClipboardOnlyInserter::new(clipboard, reason.clone());
            (Box::new(inserter), Some(reason))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "needs a live Wayland session (owns the clipboard)"]
    fn clipboard_only_mode_reports_the_blocked_device() {
        let _guard = crate::clipboard::SELECTION_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let clipboard = std::sync::Arc::new(crate::clipboard::Clipboard::new().expect("clipboard"));
        let reason = format!("{UINPUT_PATH} (Permission denied (os error 13))");
        let inserter =
            ClipboardOnlyInserter::new(std::sync::Arc::clone(&clipboard), reason.clone());

        match inserter.insert("degraded insertion test").expect("insert") {
            InsertOutcome::CopiedOnly {
                reason: reported,
                blocked_path,
            } => {
                assert_eq!(reported, reason);
                assert_eq!(blocked_path, Some(std::path::PathBuf::from(UINPUT_PATH)));
            }
            InsertOutcome::Inserted => {
                panic!("clipboard-only mode must never report an insertion")
            }
        }
    }

    use super::*;

    /// Key codes the paste chord is built from.
    const CTRL: u16 = KeyCode::KEY_LEFTCTRL.code();
    const V: u16 = KeyCode::KEY_V.code();
    /// `input_event` values: a press and a release.
    const DOWN: i32 = KEY_PRESS;
    const UP: i32 = KEY_RELEASE;

    /// `(key code, value)` pairs of an emitted batch.
    fn pairs(events: &[InputEvent]) -> Vec<(u16, i32)> {
        events.iter().map(|e| (e.code(), e.value())).collect()
    }

    /// Key codes a capability set advertises.
    fn advertised(keys: &AttributeSet<KeyCode>) -> Vec<u16> {
        keys.iter().map(KeyCode::code).collect()
    }

    #[test]
    fn paste_chord_presses_then_releases_ctrl_and_v() {
        let press = paste_chord_press();
        let release = paste_chord_release();

        assert_eq!(pairs(&press), vec![(CTRL, DOWN), (V, DOWN)]);
        // The release reverses the press, so the modifier can never stick.
        assert_eq!(pairs(&release), vec![(V, UP), (CTRL, UP)]);
    }

    #[test]
    fn virtual_keyboard_advertises_only_the_chord_keys() {
        assert_eq!(advertised(&paste_chord_keys()), vec![CTRL, V]);
    }
}
