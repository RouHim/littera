# Feature Specification: Selectable Audio Input Device for Dictation

**Created**: 2026-10-16
**Status**: Approved
**Input**: Add a config option to choose the input device used for speech recording; the default is the system default.

## Goal

Dictation always records from whichever capture device the operating system's audio layer designates, so a user with several microphones (built-in, webcam, headset, USB desk mic) cannot pin the one that actually sits in front of them, and nothing inside the app offers that choice. This feature adds exactly one persisted option that selects the capture device used for dictation, with "system default" as a distinguished default state that reproduces today's behaviour exactly. Scope boundary: the option chooses an input device and nothing else — no microphone preview, no per-application routing, no change to the recognition pipeline, onboarding, tray, overlay, or the `--audio-file` verification path. It retires the assumption recorded in `.spec/wayland-voice-dictation.md:142` ("The system-default microphone is used; no input-device picker is part of the MVP").

## User Scenarios

### Scenario 1 - Choose the microphone for dictation (P1)

An owner of several microphones opens the settings window. The Microphone section lists "System default" first, followed by every input device the audio backend can open at that moment, each with a readable name and its technical identifier. Selecting one saves immediately and the next dictation records from that device — no restart. Selecting "System default" again returns to the previous behaviour.

**Acceptance**
1. Given the settings window, When the Microphone section opens, Then the first entry is "System default" and every further entry is a device that can be opened for capture at that moment, shown with a readable name and its technical identifier.
2. Given the Microphone section, When the user selects a device, Then the choice is persisted immediately (autosave) and the section shows it as selected afterwards.
3. Given a configured non-default device, When the user dictates, Then the recording is taken from that device and everything downstream (overlay, transcript, insertion) behaves as usual.
4. Given a recording in progress, When the user selects a different device, Then the running recording stays on its device and only the next dictation uses the new one.

### Scenario 2 - Existing installations keep working (P1)

A user who never touches the new option experiences no difference: capture uses the device the operating system designates, and the configuration file carries no trace of the new option.

**Acceptance**
1. Given the option is absent or at its default, When a dictation runs, Then capture opens the system default device exactly as before, and writing the configuration afterwards does not introduce a key for the option.
2. Given a configuration file written by an earlier version, When the app starts, Then it is accepted unchanged — no migration, no warning, no behaviour change.
3. Given the option names a specific device, When the user switches back to "System default", Then the following dictation is taken from the system default device again.

### Scenario 3 - Configured device is unavailable (P1)

The configured headset was unplugged, was renumbered on the USB bus, or is held by another application. Dictation must still work: capture opens the system default instead, and the user is not interrupted.

**Acceptance**
1. Given a configured device that cannot be resolved or opened, When a dictation starts, Then capture opens the system default device and the dictation completes normally.
2. Given such a substitution, When it happens, Then the user sees no error, notice or overlay change, and the log records the configured identifier and the cause of the substitution.
3. Given a configuration value that names no existing device, When a dictation starts, Then the same substitution applies and the stored value is left untouched.
4. Given the configured device is unavailable, When the Microphone section is opened, Then the configured device is still shown, marked as currently unavailable, and is not silently replaced by another entry.

### Scenario 4 - Devices change while the app runs (P2)

Hardware is attached or removed while the app is running. The settings section enumerates freshly each time it is opened, so a microphone that was just plugged in becomes selectable without restarting the app — and one that was unplugged disappears.

**Acceptance**
1. Given a microphone attached after the previous settings visit, When the Microphone section is opened again, Then that device appears in the list.
2. Given a device detached since the last visit, When the Microphone section is opened again, Then it is no longer offered.
3. Given the Microphone section is building its list, When the window is used meanwhile, Then the window stays responsive (no frozen frame while devices are probed).

## Functional Requirements

- **FR-001**: The configuration offers one option selecting the capture device used for dictation. Its default value — and the meaning of an absent key — is "system default": the recording device the system's audio layer currently designates.
- **FR-002**: The option is hand-editable in the configuration file and follows the lifecycle of every other option: it round-trips through load/save, saving is atomic with no leftover temporary file, and a present-but-malformed file is reported to the user instead of being silently reset.
- **FR-003**: The stored value identifies a device stably rather than by its position in a list, and still denotes the same device after an application restart and after a reboot.
- **FR-004**: Settings offers a Microphone section: the "System default" entry first, then the currently selectable devices. Every entry is rendered with a human-readable name and its technical identifier, so devices of identical model remain distinguishable.
- **FR-005**: A device is offered as selectable only if the audio backend can open it for capture at the moment the list is built; devices that would fail to open are not offered.
- **FR-006**: A configured device that is currently not selectable is still displayed in the section, marked as currently unavailable; the stored value is never rewritten or substituted by the UI on its own.
- **FR-007**: Selecting an entry persists the choice immediately (autosave, like every other setting) and applies to the next recording without restarting the app or re-binding anything. A recording in progress keeps the device it started on.
- **FR-008**: The set of selectable devices is determined when the settings section is opened and reflects the devices available at that moment, including ones attached since the previous visit. There is no manual refresh control and no background device watcher.
- **FR-009**: Enumerating and probing devices never blocks the UI path: the settings window stays responsive while the list is built.
- **FR-010**: When a recording starts, capture opens the system default device if the option is at its default value, and the configured device otherwise.
- **FR-011**: If the configured device cannot be resolved or opened at recording start, capture opens the system default device instead and the dictation proceeds as usual. No user-visible error, notice or overlay change is produced; the substitution and its cause are logged together with the configured identifier.
- **FR-012**: The capture contract is identical for every selectable device: the stream is converted to mono at 16 kHz and split into 100 ms chunks exactly as today; device-specific sample rates and channel counts are absorbed by the existing conversion.
- **FR-013**: A device that disappears during a recording keeps today's behaviour (the capture error path), independent of this option.
- **FR-014**: The `--audio-file` verification path opens no capture device and is unaffected by this option.

## Key Entities

- **Input device selection**: the persisted option. Its value is either the distinguished "system default" state or a stable device identifier. It carries no ordering or position information.
- **Device list entry**: what the settings section renders — technical identifier, human-readable name, and whether the device is currently usable. Entries are derived from the live device set and are never persisted.

## Edge Cases

- Configured device unplugged, renumbered on the USB bus, or held exclusively by another application (opening fails with "device busy") — all take the same substitution path (FR-011).
- Hand-edited value that is empty, misspelled, or refers to a device that no longer exists — treated as unavailable; the app keeps working and does not overwrite the value by itself.
- Two identical device models attached at once — must stay distinguishable in the list and individually selectable through their identifiers.
- No capture hardware at all — the existing "no default audio input device found" failure and its guidance stay exactly as they are; this option cannot make that case worse.
- A selectable device supporting only unusual formats (odd sample rate, many channels) — absorbed by the existing down-mix/resample path (FR-012).
- Under PipeWire, individual sources are not addressable through the capture backend: entries follow the system default *source* unless a raw hardware device is openable. Pinning one specific source requires changing the system default source outside the app — a documented limitation, not a defect of this option.
- Changing the option never re-routes a recording that is already in progress (FR-007).

## Research Notes

- cpal 0.18.2 (the capture library in use), ALSA backend: device enumeration merges ALSA name hints with physical card probing, and `DeviceId` is documented as stable "across program runs, device disconnections, and system reboots where possible", persistable as `"<host>:<device>"` and re-resolvable via `HostTrait::device_by_id` (code read locally: `cpal-0.18.2/src/host/alsa/enumerate.rs`, `src/lib.rs:268-310`, `src/traits.rs:74-100`). Implication: FR-003 and FR-010 are implementable — store the identifier and re-resolve it, never store a list index.
- ALSA hint semantics — https://alsa-project.org/alsa-doc/alsa-lib/group___hint.html — a hint without an IOID marks a device that may work in either direction; usability can only be settled by attempting to open the device, which is the rule behind FR-005.
- Reference machine (Wayland session, PipeWire), verified 2026-10-16 with the ALSA utilities: `arecord -D default` and `-D pipewire` capture successfully, while `-D hw:CARD=Generic,DEV=0`, `-D sysdefault:CARD=Generic` and `-D front:CARD=Generic,DEV=0` fail with "Device or resource busy" because the sound server owns the hardware. Implication: offering raw hardware PCMs unconditionally would let users pick a device that can never record — the reason for the "usable only" rule (FR-005) and for keeping a saved-but-unavailable device visible (FR-006).
- `/etc/alsa/conf.d/99-pipewire-default.conf` routes `pcm.!default` through the PipeWire plugin with `capture_node "-1"` (follow the default source) — local evidence that ALSA-level device selection follows the system default source rather than addressing individual PipeWire sources. The plugin's own selection keys (`capture_node`, `target.object`) and the `PIPEWIRE_NODE` environment variable are not reachable through the capture API (keys present in `/usr/lib/alsa-lib/libasound_module_pcm_pipewire.so`), which is why per-source pinning is out of scope.
- PipeWire Audio documentation — https://docs.pipewire.org/page_audio.html — sources and sinks can be marked as the default that applications are routed to; that state is exactly what the "system default" entry follows.
- `.spec/wayland-voice-dictation.md:142` records "The system-default microphone is used; no input-device picker is part of the MVP." — this specification retires that assumption. Otherwise it follows the established patterns of that spec: configuration persistence (FR-016) and live reload without restart (Scenario 3.4).

## Assumptions

- The configuration key is `input_device`; its distinguished value — and the absent key — is `system`, meaning "follow the system default"; any other value is a backend device identifier (e.g. `alsa:<device id>`).
- Only the platform's single audio host is used; there is no host picker.
- List freshness comes from reopening the settings section: no manual refresh control and no continuous hotplug watcher (explicit choice).
- The substitution to the system default is silent to the user — a log line is the only trace (explicit choice). The consequence is accepted knowingly: a dictation can capture a different microphone than the one the settings display.
- Onboarding, tray, overlay and `--diagnostics` output keep their current content; there is no microphone preview or level meter in this iteration.
- The existing autosave and live-reload semantics (main spec FR-016, Scenario 3.4) apply to the new option unchanged; the running process never needs to be restarted.

## Success Criteria

- **SC-001**: On a machine with more than one input device, choosing a non-default device and dictating records from that device, verified without restarting the application.
- **SC-002**: An untouched or pre-existing configuration contains no key for the option and behaves exactly as before the feature; no migration and no user action are required.
- **SC-003**: With the configured device unplugged or held by another application, a dictation still produces a transcript, shows no error, notice or overlay change, and the log names the configured identifier and the cause of the substitution.
- **SC-004**: The settings section never offers a device that fails to open when selected, and a configured-but-unavailable device remains visible as such instead of being replaced.
- **SC-005**: Switching back to "System default" restores the pre-feature capture behaviour, and a chosen device survives an application restart (still selected, still used).
- **SC-006**: Hand-editing the key to another device identifier changes the device used for the next dictation without any UI interaction, and a value naming no device degrades to the system default instead of failing the application.
