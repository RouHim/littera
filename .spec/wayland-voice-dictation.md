# Feature Specification: Lingua — Local Voice Dictation for Linux/Wayland (MVP)

**Created**: 2026-10-15
**Status**: Approved
**Input**: Build an OpenWhispr clone specialized for Linux/Wayland: hotkey-triggered, fully local dictation with a fast multilingual model (English + German), Vulkan acceleration, Rust + iced GUI, simple onboarding (model selection, key binding) and a small indicator at the bottom center of the monitor; keep the first MVP simple.

## Goal

Deliver a privacy-first dictation application for Linux Wayland desktops: the user presses a global hotkey, speaks, and the transcribed text is inserted at the cursor of the focused application. Speech recognition runs entirely on the local machine with GPU acceleration (Vulkan) and no cloud dependency, no account, and no separately installed speech runtime. The MVP targets Wayland only and covers four desktop families — KDE Plasma, Hyprland (wlroots), COSMIC and GNOME — with explicit, user-visible handling of each compositor's capability gaps. X11, macOS, Windows, accounts, cloud/BYOK models, transcript history, custom dictionaries, and meeting transcription are outside the scope of this specification.

## User Scenarios

### Scenario 1 - First-run onboarding (P1)

A user installs the application and launches it for the first time. Onboarding walks through selecting a speech model, choosing the dictation language, binding a global hotkey, and a system check that reports whether hotkey access, paste permission, and the GPU backend are ready, with the exact command needed for anything that is not.

**Acceptance**
1. Given the application has never run on this machine, When the user launches it, Then onboarding appears with exactly four steps (model, language, key binding, system check) and the user can leave and return to any step.
2. Given the user selects a model, When they confirm, Then the download shows progress, can be interrupted and resumed, is accepted only after its checksum matches, and the model becomes usable without restarting the application.
3. Given the compositor offers the global-shortcut portal, When the key-binding step runs, Then the application proposes the default hotkey through the compositor's own dialog and displays the trigger that was actually bound.
4. Given the compositor offers no shortcut portal, When the key-binding step runs, Then the user binds a combination by pressing it inside the application window and the application reports the captured combination.
5. Given a required capability is missing (hotkey access, paste permission, GPU backend), When the system check runs, Then that check fails with the exact command or configuration needed and a re-check action, and the application still starts and remains usable except for the affected capability.
6. Given onboarding was completed, When the application is launched again, Then onboarding does not re-run and the settings surface opens instead.

### Scenario 2 - Dictate into a focused application (P1)

The application runs idle in the background. The user focuses a text field in another application, presses the hotkey, speaks, and stops; the text appears at the cursor.

**Acceptance**
1. Given the application is running idle, When the user presses the bound hotkey, Then recording starts and the overlay appears without changing the keyboard focus of the target application.
2. Given recording is active, When the user speaks, Then partial text appears in the overlay within one second of speech onset and continues to update while speaking.
3. Given recording is active, When the user presses the hotkey again or releases it after holding it, Then recording stops, the final transcript is produced, and the text is inserted at the cursor of the application that had focus.
4. Given a dictation finished, When the user pastes manually, Then the transcript is still present on the clipboard.
5. Given the language is set to `de-DE` or `en-US`, When the user dictates a sentence in that language, Then the inserted text is correct including punctuation, capitalization and language-specific characters such as umlauts and ß.
6. Given the user says nothing, When recording stops, Then nothing is inserted and the overlay states that no speech was detected.

### Scenario 3 - Settings and tray after onboarding (P2)

After onboarding the application keeps running in the background. The user returns to change the model, language or hotkey, and sees the current state in a tray icon.

**Acceptance**
1. Given the application is already running, When the user launches the application again, Then the running instance opens its settings surface and the second process exits.
2. Given a tray host is available, When the application is running, Then a tray icon shows the current dictation state and offers "Open settings" and "Quit".
3. Given no tray host is available, When the application is running, Then settings remain reachable by relaunching the application and the missing tray host is only recorded in the log.
4. Given the user changes model, language or hotkey in settings, When the change is made, Then it applies without restarting the application and is persisted immediately.

### Scenario 4 - Degraded and failing paths (P1)

Capabilities that this platform does not grant by default must degrade into a working, explained state instead of a silent failure.

**Acceptance**
1. Given the keystroke-injection device is not writable, When a dictation finishes, Then the transcript is placed on the clipboard and the overlay tells the user to paste manually, naming the blocked device path.
2. Given the hotkey must be read from input devices and those are not readable, When the system check runs, Then it shows the exact command that grants access and re-checks on demand without restarting the application.
3. Given no Vulkan device is present or the Vulkan backend fails to initialize, When the engine starts, Then it continues on the CPU backend and the overlay notice plus the diagnostics output state that fallback explicitly.
4. Given a model download is interrupted, When the user retries, Then it resumes and is accepted only after the checksum matches; a file that fails verification is rejected with an actionable error.
5. Given the engine reports an error during a dictation, When the error surfaces, Then the overlay shows a short actionable message and the recording is discarded without inserting partial text.
6. Given a second dictation is started while the previous one is still being finalized, When the engine serializes the work, Then the new recording is queued and the overlay reflects the queued state instead of dropping audio.

### Scenario 5 - Overlay placement per compositor (P2)

The overlay is pinned to the bottom centre wherever the platform allows it, and is honest about the cases where it cannot be.

**Acceptance**
1. Given the compositor supports layer surfaces (KDE Plasma, Hyprland, COSMIC), When recording starts, Then the overlay is pinned to the bottom centre of the monitor that has keyboard focus and reserves no space on that monitor.
2. Given the compositor does not support layer surfaces (GNOME), When recording starts, Then the overlay appears as a regular non-activating window and the application does not substitute the indicator with desktop notifications or other shell chrome.
3. Given the overlay is visible, When the user types, Then the keystrokes reach the target application unchanged, on both a layer-surface compositor and GNOME.

## Functional Requirements

- **FR-001**: The first run MUST present an onboarding flow whose steps are model selection, language selection, key binding, and system check; completion MUST be persisted so the flow does not repeat.
- **FR-002**: The model selector MUST offer at least three entries with size and quality notes: the multilingual Nemotron 3.5 ASR Streaming Q8_0 (approximately 751 MB, default), the same model at Q4_K_M (approximately 496 MB), and the English-only Nemotron Speech Streaming EN 0.6B. Every entry MUST state its download size before the user commits to it.
- **FR-003**: Model artifacts MUST be downloaded on demand, resumed after interruption, verified against a published SHA-256, and stored outside the binary in an XDG data directory. The application MUST refuse to load unverified artifacts and MUST check free disk space before starting a download.
- **FR-004**: Speech recognition MUST run inside the application process from the application's own binary. Installing a separate speech runtime, background daemon, Python environment, or using a cloud service MUST NOT be required for dictation to work.
- **FR-005**: The engine MUST use a Vulkan GPU backend when a Vulkan device is available, MUST report the active backend and device name in the diagnostics output, and MUST fall back to CPU with an explicit, user-visible notice when no usable Vulkan device exists.
- **FR-006**: Recognition MUST be streaming: partial text is produced while the user is speaking, and exactly one final insertion happens per dictation.
- **FR-007**: The language hint MUST support `auto` (the default) and every language the bundled model catalog can serve, each as one selectable entry in onboarding and in settings. When `auto` is selected, language tags produced by the model MUST NOT appear in inserted text.
- **FR-008**: Global hotkey registration MUST prefer the desktop's global-shortcut portal and MUST fall back to a listen-only key-device listener when the portal is missing or refuses registration. Exactly one mechanism may be active at a time and the active mechanism MUST be visible in settings.
- **FR-009**: The proposed default hotkey MUST be `Ctrl+Super`. The user MUST be able to rebind it during onboarding and later in settings, and a rebind MUST take effect without restarting the application.
- **FR-010**: Both trigger gestures MUST be supported: press-to-start/press-to-stop, and hold-to-record/release-to-stop, separated by a minimum hold duration so that a short release belonging to a tap never stops the recording that tap started.
- **FR-011**: The dictation overlay MUST appear on the monitor that has keyboard focus — as a layer-surface overlay where the compositor supports that protocol, otherwise as a regular non-activating window. It MUST NOT take keyboard focus, MUST be visible only while recording or transcribing, MUST display the current state (recording with elapsed time and input level, transcribing, no speech, error) plus the streaming partial text, and MUST NOT reserve layout space on the monitor.
- **FR-012**: The final transcript MUST be placed on the Wayland clipboard and MUST remain there after insertion; the application MUST NOT implement clipboard restoration.
- **FR-013**: Automatic insertion MUST synthesize the paste chord in-process into the kernel's input-injection device. If that device is not writable, the application MUST degrade to clipboard-only mode and state the reason and the remediation in the UI. External paste tools and paste daemons MUST NOT be dependencies.
- **FR-014**: A system-check surface MUST report hotkey availability, paste-permission availability, GPU backend/device, and overlay availability, and MUST name the exact command or configuration to fix any failing check. When a check fails at startup, a notice window MUST open by itself — also when the daemon otherwise starts without any window. Checks MUST be re-runnable on demand without restarting the application.
- **FR-015**: The application MUST run as a single instance using an XDG runtime-directory socket; a second launch MUST forward "open settings" to the running instance and exit.
- **FR-016**: Settings MUST be persisted in the XDG config directory and MUST cover at least: selected model, language, hotkey binding plus its source (portal or device listener), onboarding-completed flag, and backend preference. The settings surface MUST show where models are stored.
- **FR-017**: A tray icon (StatusNotifierItem) MUST show the dictation state and offer "Open settings" and "Quit". Without a tray host the application MUST continue to work, with settings reachable by relaunching it.
- **FR-018**: A recording MUST be capped at five minutes and MUST then auto-stop and insert the captured text. Recordings below a minimum duration, and recordings whose transcript is empty, MUST be discarded without inserting anything.
- **FR-019**: User-visible failures MUST be actionable text naming cause and remedy, never silent. Internal logging MUST use `tracing` with contextual spans instead of `println!`.
- **FR-020**: Non-test code MUST NOT use `unwrap`/`expect`. HTTPS downloads MUST use rustls. Static UI assets (fonts, icons) MUST be embedded in the binary, and the repository MUST pass `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test`.
- **FR-021**: The source MUST be organized as flat, single-responsibility Rust files declared at the crate root, without nested module trees; engine, hotkey source, text inserter, and overlay presentation MUST sit behind narrow interfaces so each can be replaced or tested independently.
- **FR-022**: `lingua --diagnostics` MUST expose the effective backend, device, model, language and hotkey mechanism with their fallback states in one output, so support questions can be answered without reading logs.

## Key Entities

- **Settings record**: persisted configuration — selected model identifier, language hint, hotkey chord plus binding source, onboarding completion flag, backend preference, and the model storage directory.
- **Model catalog entry**: one selectable model variant — display name, quantization, download size, source URL, expected SHA-256, local path, and state (not downloaded, downloading with progress, verifying, ready, failed).
- **Dictation session**: one press-to-insert cycle — state (idle, recording, transcribing, inserting, inserted, failed), start timestamp, accumulated partial text, final text, and the language actually used.
- **Hotkey binding**: the active trigger — chord, human-readable trigger description as reported by the compositor or captured from the device listener, and the mechanism that owns it.
- **System check result**: one capability probe — capability name (hotkey access, paste permission, GPU backend, overlay backend), status, detected detail (for example device name or blocked path), and the remediation command when failing.
- **Transcript**: the recognition result of one dictation — text, language, whether it was inserted or only copied to the clipboard, and the reason when it was discarded.

## Edge Cases

- On GNOME the overlay cannot be pinned; the compositor places the window and other windows may cover it. The application must not present it as pinned and must not replace it with desktop notifications.
- The overlay surface must not request focus and must not reserve an exclusive zone, so the compositor never moves or resizes existing windows when the overlay appears or disappears.
- A modifier-only chord such as the default `Ctrl+Super` can be refused by a portal binding dialog. The application then shows what the compositor actually bound (possibly nothing) and lets the user repeat the binding step.
- A listen-only device listener cannot suppress compositor key bindings, so the same chord may additionally trigger a desktop action. The listener must never grab devices exclusively, and onboarding must warn about chords that desktops commonly reserve.
- The portal route and the device-listener route must not both register the same chord, or a single press would start and immediately stop a recording.
- Multiple monitors, HiDPI and fractional scaling: the overlay follows the monitor with keyboard focus and the compositor's scale factor.
- Session loss or compositor restart invalidates the Wayland connection: the application exits cleanly with a logged reason instead of remaining half-functional.
- The engine serializes work per loaded model; overlapping dictations queue (see Scenario 4.6) and must not lose audio.
- The model file can disappear or be corrupted after a successful download; the next dictation then reports an actionable error and offers to download again.
- The first download can be up to 751 MB on a nearly full disk; free space is checked before the download starts.
- Voice-activity detection and endpointing are not part of the MVP; silence is handled only through the empty-transcript rule, and very short utterances under automatic language detection may be mislabelled, so an explicit language must always be selectable.
- Terminals frequently require a different paste chord than `Ctrl+V`; the MVP uses `Ctrl+V` only and documents this as a known limitation, with the clipboard path as the manual workaround.

## Research Notes

- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html — the portal exposes session creation, binding with a preferred trigger, and Activated/Deactivated signals, which is the only rootless global-hotkey route and also the basis for hold-to-talk.
- https://github.com/Wayclip/global-hotkey — desktop support matrix for that portal is KDE Plasma 5.27+, GNOME 48+, Hyprland 0.20+, with no wlroots/Sway backend, which makes the device-listener fallback mandatory rather than optional.
- Verified on the reference machine (2026-10-15, COSMIC 1.9.0): D-Bus introspection of `org.freedesktop.portal.Desktop` lists no `GlobalShortcuts` interface, so the user's own desktop has to use the device-listener path.
- Verified on the reference machine: `cosmic-comp` 1.9.0 contains the `zwlr_layer_shell_v1` protocol strings, so the pinned bottom-centre overlay is achievable on COSMIC.
- https://gitlab.gnome.org/GNOME/mutter/-/work_items/973 — mutter still has no layer-shell implementation (only third-party forks provide it), so the pinned overlay is impossible on GNOME and the plain-window path is required.
- Verified on the reference machine: `/dev/uinput` carries a logind ACL granting the session user write access while `/dev/input/event*` is not readable, so keystroke injection works out of the box and only the hotkey listener needs the `input` group.
- https://docs.openwhispr.com/platform/linux — reference product behaviour: `Ctrl+Super` is the Linux default hotkey, auto-paste uses a native helper that needs input-device access, "Clipboard Mode on Wayland" is the designed fallback, and terminals are special-cased; we adopt the default and the fallback but not the per-desktop registration code.
- https://docs.openwhispr.com/help/dictation/auto-paste-and-clipboard — clipboard plus auto-paste is the expected UX; the reference implementation depends on ydotool/ydotoold, which the MVP replaces with in-process keystroke injection.
- https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b — the model is a 0.6B cache-aware FastConformer-RNNT with language-ID prompting across 40 locales, German is transcription-ready (FLEURS de WER 8.31 as reported by NVIDIA), punctuation and capitalization are native, and a language target must be supplied explicitly (OpenMDW-1.1 licence).
- https://github.com/handy-computer/transcribe.cpp/blob/main/docs/models/nemotron-3.5-asr-streaming-0.6b.md — ready GGUF quants (Q8_0 751 MB, Q4_K_M 496 MB), chunked streaming with runtime-selectable lookahead (byte-equal to the offline result at the highest lookahead), measured Q8_0 FLEURS German WER 10.33%, and a Vulkan measurement of 17x realtime on an integrated AMD GPU, which bounds expected latency on the reference Radeon RX 6950 XT.
- https://docs.rs/crate/transcribe-cpp/latest and https://docs.rs/crate/transcribe-cpp-sys/latest — Rust bindings (0.4.0, 2026-10-12) provide the streaming API surface, a `vulkan` cargo feature, a static link by default, committed bindgen output (no libclang needed), and a CMake-based native build with an escape hatch for prebuilt installs; this is the assumed engine integration for FR-004/FR-005.
- https://github.com/NVIDIA/NeMo-Speech.cpp (README, docs/sdk.md, docs/build.md, docs/install.md) — the official NVIDIA runtime also runs this model with Vulkan and streaming, but publishes shared libraries resolved through DT_RPATH (no static archive) and a separate CLI/server; it remains a documented alternative for the deferred integration deep-dive.
- https://github.com/k2-fsa/sherpa-onnx/pull/3671 and the `asr-models` release tag — Nemotron 3.5 streaming ONNX packages exist and Rust crates (`sherpa-onnx`, `sherpa-rs`) can load them, but ONNX Runtime offers no Vulkan execution provider, which fails FR-005.
- https://docs.rs/crate/nemotron-asr/latest — an alternative Rust bindings crate with Vulkan and static linking, but published 2026-04-18, before Nemotron 3.5 existed, so it supports only the English streaming model and cannot satisfy the multilingual requirement.
- https://docs.rs/crate/iced/latest with https://github.com/MalpenZibo/iced_layershell and https://github.com/waycrate/exwlshelleventloop — iced 0.14 is the current stable release and has no built-in layer-shell support; third-party backends exist for it, so how the pinned overlay and the ordinary settings surface coexist (single process with a layered UI, or an engine process plus a settings process) is a planning decision.

## Assumptions

- The application is named after its repository, "Lingua"; its user interface is English-only for the MVP.
- The reference machine for latency and success criteria is the developer machine: COSMIC 1.9.0 on Wayland, AMD Radeon RX 6950 XT with the Mesa RADV Vulkan driver, PipeWire audio.
- Engine integration is assumed to be the in-process, statically linked Rust bindings route; the alternatives are documented above and the final choice is confirmed during planning.
- The default model is the multilingual Q8_0 variant; the default language is `auto`.
- Models remain resident in memory after the first dictation and are released when the application quits.
- The system-default microphone is used; no input-device picker is part of the MVP.
- The transcript stays on the clipboard after insertion; no clipboard restoration is implemented.
- Recordings shorter than roughly 0.3 seconds and recordings whose transcript is empty are discarded.
- Settings are reopened by relaunching the application or through the tray; there is no dedicated "open settings" hotkey.
- Terminal applications that need a different paste chord are a documented limitation of the MVP, not a handled case.
- Build-time prerequisites for the engine (C++ toolchain, CMake, Vulkan headers and `glslc`) are acceptable and documented in the repository README.
- Flat, single-responsibility source files declared at the crate root are the intended layout; nested module trees are not used.

## Success Criteria

- **SC-001**: On the reference machine a fresh build reaches a first successful dictation with no terminal use other than the permission command the application itself displays.
- **SC-002**: For a ten-second utterance with the default model on the reference GPU, the first partial text is displayed within one second of speech onset and the final text is inserted within 1.5 seconds of the stop gesture.
- **SC-003**: Ten scripted sentences each in German and English, including umlauts and ß, are inserted at the cursor with correct punctuation and capitalization, and the inserted text never contains language tags.
- **SC-004**: The diagnostics view reports the active backend as Vulkan together with the device name; forcing the CPU path still completes dictations and displays the fallback notice.
- **SC-005**: With the input-injection device made unwritable, a dictation still completes end-to-end, the transcript is on the clipboard, and the overlay names the blocked path.
- **SC-006**: With input devices unreadable and no shortcut portal available, the system check names the exact granting command and reports success after the permission change, without restarting the application.
- **SC-007**: While idle for sixty seconds the process uses less than one percent average CPU, and twenty consecutive dictations leave resident memory unchanged.
- **SC-008**: `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` pass; non-test sources contain no `unwrap(` or `expect(`; and a release binary started from an otherwise empty directory runs because all UI assets are embedded.
- **SC-009**: An interrupted model download resumes and completes with a matching checksum, while a deliberately tampered artifact is rejected with an actionable error.
- **SC-010**: Typing during an active recording reaches the target application unchanged on both a layer-surface compositor and on GNOME, proving the overlay never takes keyboard focus.
