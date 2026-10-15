//! Listen-only evdev hotkey listener: the fallback mechanism when the XDG
//! shortcut portal is missing (the reference machine's case).
//!
//! Devices are opened read-only and are **never** grabbed, so the compositor's
//! own binding for the same chord keeps working — at the price of not being able
//! to suppress it (`hotkey::device_limitation_note`).
//!
//! One reader thread per keyboard device plus one supervisor thread that
//! re-enumerates `/dev/input` every five seconds for hotplugged keyboards.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use evdev::{Device, EventType, KeyCode};

use crate::hotkey::{Chord, HotkeyEvent, KeyToken, Modifier};

/// Left/right `Ctrl`.
const CTRL_CODES: [u16; 2] = [29, 97];
/// Left/right `Super` (Meta).
const SUPER_CODES: [u16; 2] = [125, 126];
/// Left/right `Alt`.
const ALT_CODES: [u16; 2] = [56, 100];
/// Left/right `Shift`.
const SHIFT_CODES: [u16; 2] = [42, 54];

/// How often the supervisor looks for hotplugged keyboards.
const SUPERVISOR_INTERVAL: Duration = Duration::from_secs(5);
/// Granularity of the supervisor's interruptible wait, so dropping the handle
/// retires the mechanism promptly.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

const READER_THREAD_NAME: &str = "lingua-hotkey-device";
const SUPERVISOR_THREAD_NAME: &str = "lingua-hotkey-supervisor";

/// Whether at least one keyboard device is readable by this user.
///
/// `evdev::enumerate` only yields devices it could open, so this is exactly the
/// probe the diagnostics check needs.
pub fn available() -> bool {
    !keyboard_devices().is_empty()
}

/// Detail reported when no keyboard device could be opened.
pub fn unreadable_detail() -> String {
    "no /dev/input/event* device is readable by this user".to_owned()
}

/// A live device-listener binding.
pub struct DeviceBinding {
    pub trigger_description: String,
    pub rx: Receiver<HotkeyEvent>,
    pub(crate) keepalive: DeviceKeepAlive,
}

/// Retires the listener threads when the binding is dropped.
pub(crate) struct DeviceKeepAlive {
    stop: Arc<AtomicBool>,
    supervisor: Option<JoinHandle<()>>,
}

impl Drop for DeviceKeepAlive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(supervisor) = self.supervisor.take()
            && supervisor.join().is_err()
        {
            tracing::debug!("the hotkey device supervisor thread panicked");
        }
    }
}

impl DeviceKeepAlive {
    /// Whether the listener is still running: a retired stop flag or a finished
    /// supervisor means it silently stopped reading devices.
    pub(crate) fn is_alive(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
            && self
                .supervisor
                .as_ref()
                .is_some_and(|supervisor| !supervisor.is_finished())
    }
}

/// Chord state machine over the set of currently pressed key codes.
///
/// Kept free of evdev so the transitions are unit-testable without hardware.
#[derive(Debug)]
pub struct ChordState {
    modifiers: Vec<Modifier>,
    key_codes: Vec<u16>,
    pressed: HashSet<u16>,
    active: bool,
}

impl ChordState {
    pub fn new(chord: &Chord) -> Self {
        Self {
            modifiers: chord.modifiers.clone(),
            key_codes: chord.key.map(key_codes).unwrap_or_default(),
            pressed: HashSet::new(),
            active: false,
        }
    }

    /// Whether the chord is currently held (test-facing accessor).
    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Feed one key event; yields a transition exactly on the inactive→active
    /// and active→inactive edges.
    pub fn apply(&mut self, code: u16, pressed: bool) -> Option<HotkeyEvent> {
        if pressed {
            self.pressed.insert(code);
        } else {
            self.pressed.remove(&code);
        }
        let active = self.compute_active();
        if active == self.active {
            return None;
        }
        self.active = active;
        Some(if active {
            HotkeyEvent::Press
        } else {
            HotkeyEvent::Release
        })
    }

    fn compute_active(&self) -> bool {
        let modifiers_held = self.modifiers.iter().all(|modifier| {
            modifier_codes(*modifier)
                .iter()
                .any(|c| self.pressed.contains(c))
        });
        let key_held =
            self.key_codes.is_empty() || self.key_codes.iter().any(|c| self.pressed.contains(c));
        modifiers_held && key_held
    }
}

/// Bind the listen-only device listener for `chord`.
///
/// Fails with the user-facing detail (and no remediation — the caller adds it)
/// when no keyboard device is readable.
pub fn bind(chord: &Chord) -> Result<DeviceBinding, String> {
    let devices = keyboard_devices();
    if devices.is_empty() {
        return Err(unreadable_detail());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let shared: Shared = Arc::new(Mutex::new(Readers::default()));
    let (events, rx) = std::sync::mpsc::channel();

    for (path, device) in devices {
        if let Err(err) = spawn_reader(
            path.clone(),
            device,
            chord.clone(),
            events.clone(),
            Arc::clone(&stop),
            Arc::clone(&shared),
        ) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "cannot start a hotkey device reader thread"
            );
        }
    }

    if readers(&shared).attached.is_empty() {
        return Err(unreadable_detail());
    }

    let supervisor = {
        let chord = chord.clone();
        let events = events.clone();
        let stop = Arc::clone(&stop);
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(SUPERVISOR_THREAD_NAME.to_owned())
            .spawn(move || supervisor_loop(&chord, &events, &stop, &shared))
            .map_err(|err| format!("cannot start the hotkey device supervisor thread: {err}"))?
    };

    tracing::info!(chord = %chord, "hotkey device listener bound");
    Ok(DeviceBinding {
        trigger_description: device_trigger_description(chord),
        rx,
        keepalive: DeviceKeepAlive {
            stop,
            supervisor: Some(supervisor),
        },
    })
}

/// The pinned trigger description of the device listener.
fn device_trigger_description(chord: &Chord) -> String {
    format!("{chord} (device listener)")
}

/// Readable keyboard devices: those whose key set contains `KEY_A`.
///
/// The `KEY_A` filter also excludes Lingua's own uinput paste device, which only
/// advertises `KEY_LEFTCTRL` and `KEY_V`.
fn keyboard_devices() -> Vec<(PathBuf, Device)> {
    evdev::enumerate()
        .filter(|(_, device)| is_keyboard(device))
        .collect()
}

fn is_keyboard(device: &Device) -> bool {
    device
        .supported_keys()
        .is_some_and(|keys| keys.contains(KeyCode::KEY_A))
}

/// Devices whose reader threads are alive, plus those that ended with a read
/// error and are therefore not retried while they stay plugged in.
#[derive(Default)]
struct Readers {
    attached: HashSet<PathBuf>,
    failed: HashSet<PathBuf>,
}

type Shared = Arc<Mutex<Readers>>;

fn readers(shared: &Shared) -> MutexGuard<'_, Readers> {
    shared.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn spawn_reader(
    path: PathBuf,
    device: Device,
    chord: Chord,
    events: Sender<HotkeyEvent>,
    stop: Arc<AtomicBool>,
    shared: Shared,
) -> std::io::Result<()> {
    // Registered before the thread starts so a reader that fails immediately
    // still removes its own entry, and so `bind` can tell whether any device is
    // actually being listened to.
    let key = path.clone();
    let reader_shared = Arc::clone(&shared);
    readers(&shared).attached.insert(key.clone());

    let spawned = thread::Builder::new()
        .name(READER_THREAD_NAME.to_owned())
        .spawn(move || reader_loop(&path, device, &chord, &events, &stop, &reader_shared));
    match spawned {
        Ok(_handle) => Ok(()),
        Err(err) => {
            readers(&shared).attached.remove(&key);
            Err(err)
        }
    }
}

fn reader_loop(
    path: &Path,
    mut device: Device,
    chord: &Chord,
    events: &Sender<HotkeyEvent>,
    stop: &AtomicBool,
    shared: &Shared,
) {
    let mut state = ChordState::new(chord);
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match device.fetch_events() {
            Ok(device_events) => {
                for event in device_events {
                    if event.event_type() != EventType::KEY {
                        continue;
                    }
                    // 0 = released, 1 = pressed, 2 = autorepeat (still held).
                    let Some(transition) = state.apply(event.code(), event.value() != 0) else {
                        continue;
                    };
                    // A retired binding must not deliver events, not even the one
                    // that unblocked the read; the outer loop then cleans up.
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if events.send(transition).is_err() {
                        // The binding was dropped; retire quietly.
                        readers(shared).attached.remove(path);
                        return;
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "hotkey: input device became unreadable; its listener stops"
                );
                detach_failed(shared, path);
                return;
            }
        }
    }
    readers(shared).attached.remove(path);
}

fn detach_failed(shared: &Shared, path: &Path) {
    let mut readers = readers(shared);
    readers.attached.remove(path);
    readers.failed.insert(path.to_path_buf());
}

fn supervisor_loop(
    chord: &Chord,
    events: &Sender<HotkeyEvent>,
    stop: &Arc<AtomicBool>,
    shared: &Shared,
) {
    while !stop.load(Ordering::Relaxed) {
        if !wait_for_next_scan(stop) {
            return;
        }

        let mut to_attach: Vec<(PathBuf, Device)> = Vec::new();
        let devices = keyboard_devices();
        {
            let mut readers = readers(shared);
            let present: HashSet<PathBuf> = devices.iter().map(|(path, _)| path.clone()).collect();
            // A device that disappeared may be re-plugged: forget its failure.
            readers.failed.retain(|path| present.contains(path));
            for (path, device) in devices {
                if readers.attached.contains(&path) || readers.failed.contains(&path) {
                    continue;
                }
                to_attach.push((path, device));
            }
        }

        for (path, device) in to_attach {
            tracing::info!(
                path = %path.display(),
                "hotkey: attaching newly available input device"
            );
            if let Err(err) = spawn_reader(
                path.clone(),
                device,
                chord.clone(),
                events.clone(),
                Arc::clone(stop),
                Arc::clone(shared),
            ) {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "cannot start a hotkey device reader thread"
                );
            }
        }
    }
}

/// Wait for the next hotplug scan; `false` once the binding was retired, so a
/// dropped handle stops the supervisor promptly instead of after five seconds.
fn wait_for_next_scan(stop: &AtomicBool) -> bool {
    let mut waited = Duration::ZERO;
    while waited < SUPERVISOR_INTERVAL {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(STOP_POLL_INTERVAL);
        waited += STOP_POLL_INTERVAL;
    }
    true
}

/// Key codes `evdev` reports for a chord key token.
fn key_codes(token: KeyToken) -> Vec<u16> {
    let code = match token {
        KeyToken::Letter(letter) => match letter.to_ascii_uppercase() {
            'A' => KeyCode::KEY_A.0,
            'B' => KeyCode::KEY_B.0,
            'C' => KeyCode::KEY_C.0,
            'D' => KeyCode::KEY_D.0,
            'E' => KeyCode::KEY_E.0,
            'F' => KeyCode::KEY_F.0,
            'G' => KeyCode::KEY_G.0,
            'H' => KeyCode::KEY_H.0,
            'I' => KeyCode::KEY_I.0,
            'J' => KeyCode::KEY_J.0,
            'K' => KeyCode::KEY_K.0,
            'L' => KeyCode::KEY_L.0,
            'M' => KeyCode::KEY_M.0,
            'N' => KeyCode::KEY_N.0,
            'O' => KeyCode::KEY_O.0,
            'P' => KeyCode::KEY_P.0,
            'Q' => KeyCode::KEY_Q.0,
            'R' => KeyCode::KEY_R.0,
            'S' => KeyCode::KEY_S.0,
            'T' => KeyCode::KEY_T.0,
            'U' => KeyCode::KEY_U.0,
            'V' => KeyCode::KEY_V.0,
            'W' => KeyCode::KEY_W.0,
            'X' => KeyCode::KEY_X.0,
            'Y' => KeyCode::KEY_Y.0,
            'Z' => KeyCode::KEY_Z.0,
            _ => return Vec::new(),
        },
        KeyToken::Digit(digit) => match digit {
            0 => KeyCode::KEY_0.0,
            1 => KeyCode::KEY_1.0,
            2 => KeyCode::KEY_2.0,
            3 => KeyCode::KEY_3.0,
            4 => KeyCode::KEY_4.0,
            5 => KeyCode::KEY_5.0,
            6 => KeyCode::KEY_6.0,
            7 => KeyCode::KEY_7.0,
            8 => KeyCode::KEY_8.0,
            9 => KeyCode::KEY_9.0,
            _ => return Vec::new(),
        },
        KeyToken::F(number) => match number {
            1 => KeyCode::KEY_F1.0,
            2 => KeyCode::KEY_F2.0,
            3 => KeyCode::KEY_F3.0,
            4 => KeyCode::KEY_F4.0,
            5 => KeyCode::KEY_F5.0,
            6 => KeyCode::KEY_F6.0,
            7 => KeyCode::KEY_F7.0,
            8 => KeyCode::KEY_F8.0,
            9 => KeyCode::KEY_F9.0,
            10 => KeyCode::KEY_F10.0,
            11 => KeyCode::KEY_F11.0,
            12 => KeyCode::KEY_F12.0,
            _ => return Vec::new(),
        },
        KeyToken::Space => KeyCode::KEY_SPACE.0,
    };
    vec![code]
}

fn modifier_codes(modifier: Modifier) -> &'static [u16] {
    match modifier {
        Modifier::Ctrl => &CTRL_CODES,
        Modifier::Super => &SUPER_CODES,
        Modifier::Alt => &ALT_CODES,
        Modifier::Shift => &SHIFT_CODES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::parse_chord;

    const LEFTCTRL: u16 = 29;
    const RIGHTCTRL: u16 = 97;
    const LEFTMETA: u16 = 125;
    const RIGHTMETA: u16 = 126;
    const LEFTALT: u16 = 56;
    const RIGHTALT: u16 = 100;
    const LEFTSHIFT: u16 = 42;
    const RIGHTSHIFT: u16 = 54;
    const KEY_A: u16 = 30;
    const KEY_B: u16 = 48;
    const KEY_SPACE: u16 = 57;

    fn state_for(chord: &str) -> ChordState {
        ChordState::new(&parse_chord(chord).unwrap())
    }

    #[test]
    fn maps_key_tokens_to_evdev_codes() {
        assert_eq!(key_codes(KeyToken::Letter('A')), vec![KEY_A]);
        assert_eq!(key_codes(KeyToken::Letter('B')), vec![KEY_B]);
        assert_eq!(key_codes(KeyToken::Letter('Z')), vec![44]);
        assert_eq!(key_codes(KeyToken::Letter('Q')), vec![16]);
        assert_eq!(key_codes(KeyToken::Space), vec![KEY_SPACE]);
        assert_eq!(key_codes(KeyToken::Digit(0)), vec![11]);
        assert_eq!(key_codes(KeyToken::Digit(3)), vec![4]);
        assert_eq!(key_codes(KeyToken::Digit(9)), vec![10]);
        assert_eq!(key_codes(KeyToken::F(1)), vec![59]);
        assert_eq!(key_codes(KeyToken::F(10)), vec![68]);
        assert_eq!(key_codes(KeyToken::F(11)), vec![87]);
        assert_eq!(key_codes(KeyToken::F(12)), vec![88]);
        assert!(key_codes(KeyToken::Letter('Ä')).is_empty());
        assert!(key_codes(KeyToken::F(13)).is_empty());
    }

    #[test]
    fn modifier_only_chord_ignores_irrelevant_keys() {
        let mut state = state_for("CTRL+SUPER");
        assert_eq!(state.apply(LEFTCTRL, true), None);
        assert!(!state.is_active());
        assert_eq!(state.apply(KEY_A, true), None);
        assert_eq!(state.apply(KEY_A, false), None);
        assert_eq!(state.apply(LEFTMETA, true), Some(HotkeyEvent::Press));
        assert!(state.is_active());
        // Irrelevant keys pressed while the chord is held change nothing.
        assert_eq!(state.apply(KEY_A, true), None);
        assert_eq!(state.apply(KEY_A, false), None);
        assert_eq!(state.apply(LEFTMETA, false), Some(HotkeyEvent::Release));
        assert!(!state.is_active());
        assert_eq!(state.apply(LEFTCTRL, false), None);
    }

    #[test]
    fn autorepeat_does_not_duplicate_transitions() {
        let mut state = state_for("CTRL+SUPER");
        assert_eq!(state.apply(LEFTCTRL, true), None);
        assert_eq!(state.apply(LEFTMETA, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(LEFTMETA, true), None);
        assert_eq!(state.apply(LEFTMETA, false), Some(HotkeyEvent::Release));
        assert_eq!(state.apply(LEFTMETA, false), None);
    }

    #[test]
    fn modifier_group_aliases_work() {
        let mut state = state_for("CTRL+ALT");
        assert_eq!(state.apply(RIGHTALT, true), None);
        assert_eq!(state.apply(RIGHTCTRL, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(RIGHTALT, false), Some(HotkeyEvent::Release));
        assert_eq!(state.apply(RIGHTCTRL, false), None);

        let mut state = state_for("SHIFT+CTRL");
        assert_eq!(state.apply(RIGHTSHIFT, true), None);
        assert_eq!(state.apply(RIGHTCTRL, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(RIGHTSHIFT, false), Some(HotkeyEvent::Release));
    }

    #[test]
    fn all_required_modifiers_must_be_held() {
        let mut state = state_for("CTRL+ALT");
        assert_eq!(state.apply(LEFTCTRL, true), None);
        assert_eq!(state.apply(LEFTSHIFT, true), None);
        assert_eq!(state.apply(LEFTALT, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(LEFTCTRL, false), Some(HotkeyEvent::Release));
        assert_eq!(state.apply(LEFTALT, false), None);
    }

    #[test]
    fn key_chord_requires_the_exact_key() {
        let mut state = state_for("CTRL+ALT+A");
        assert_eq!(state.apply(LEFTCTRL, true), None);
        assert_eq!(state.apply(LEFTALT, true), None);
        assert_eq!(state.apply(KEY_B, true), None);
        assert_eq!(state.apply(KEY_B, false), None);
        assert!(!state.is_active());
        assert_eq!(state.apply(KEY_A, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(KEY_A, true), None);
        assert_eq!(state.apply(KEY_A, false), Some(HotkeyEvent::Release));
        assert_eq!(state.apply(LEFTALT, false), None);
        assert_eq!(state.apply(LEFTCTRL, false), None);
    }

    #[test]
    fn release_after_tap() {
        let mut state = state_for("SUPER+SPACE");
        assert_eq!(state.apply(RIGHTMETA, true), None);
        assert_eq!(state.apply(KEY_SPACE, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(KEY_SPACE, false), Some(HotkeyEvent::Release));
        assert_eq!(state.apply(RIGHTMETA, false), None);
    }

    #[test]
    fn keyed_chord_without_its_modifier_stays_inactive() {
        let mut state = state_for("CTRL+F5");
        assert_eq!(state.apply(63, true), None);
        assert!(!state.is_active());
        assert_eq!(state.apply(LEFTCTRL, true), Some(HotkeyEvent::Press));
        assert_eq!(state.apply(LEFTCTRL, false), Some(HotkeyEvent::Release));
    }

    #[test]
    fn device_trigger_description_names_the_mechanism() {
        let chord = parse_chord("CTRL+SUPER").unwrap();
        assert_eq!(
            device_trigger_description(&chord),
            "CTRL+SUPER (device listener)"
        );
    }

    #[test]
    fn unreadable_detail_is_the_pinned_literal() {
        assert_eq!(
            unreadable_detail(),
            "no /dev/input/event* device is readable by this user"
        );
    }
}
