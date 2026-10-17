# Repository Guidelines

## Project Overview

**Lingua** — local voice dictation for Linux/Wayland. Global hotkey → speak → the transcript is inserted at the cursor of the focused application. ASR runs **fully in-process** (`transcribe-cpp` 0.4 → vendored transcribe.cpp/ggml, Vulkan with CPU fallback); no cloud, no account, no separately installed speech runtime.

- Single **binary** crate: `name = "lingua"`, `edition = "2024"`, `rust-version = "1.98"`, `publish = false`. No `src/lib.rs`, no `[[bin]]`/`[lib]`, no `[workspace]`, no `[features]`, no `[dev-dependencies]`, no `[lints]`.
- **The spec is the requirements source of truth**: `.spec/wayland-voice-dictation.md` (`FR-001…FR-022`, `SC-001…SC-010`, user Scenarios). Source and tests cite those ids (e.g. `FR-013` in `src/inserter.rs:7`, `FR-018` in `src/audio.rs:864`). Keep the ids accurate when you change behavior; a test that pins a spec literal is intentional, not redundant.
- **Platform scope: Linux + a Wayland session, nothing else — do not widen it.** The
  target is a Wayland compositor on a Linux desktop (COSMIC, KDE Plasma,
  Hyprland/wlroots, GNOME 48+). There is no macOS, Windows or BSD target, and **no
  X11-session support**: X11 appears *only* as an XWayland fallback inside a Wayland
  session, in exactly two places — the GNOME overlay window (`src/overlay_x11.rs`,
  because mutter implements neither `zwlr_layer_shell_v1` nor a non-focusing Wayland
  toplevel) and the X11 `CLIPBOARD` bridge (`src/clipboard.rs`, for compositors without
  a data-control protocol). Keep that shape: new code must stay Wayland-first, probe
  the capability at runtime and degrade to a documented, user-visible fallback
  (diagnostics + overlay/notice text) instead of assuming a protocol or routing around
  Wayland with a non-Wayland mechanism. Never add a hard X11/Xorg dependency, an
  X11-only code path that is not a fallback, or a non-Linux code path.
- Out of scope by spec: cloud/BYOK models, transcript history, custom dictionaries,
  meeting transcription, accounts, macOS/Windows.
- Repo state: branch `main` carries a single initial commit, `origin` = `github.com/RouHim/lingua` (**private**), no CI, no packaging. Exactly one root script (`install-desktop.sh`, the user-local desktop-entry/icon installer) and one shipped desktop entry (`assets/icon/lingua.desktop`) exist. Do not assume history or automation exists.

## Architecture & Data Flow

Flat crate: 26 `.rs` files directly in `src/`, every module declared at the crate root (`src/main.rs:9-33`) — no nested module trees (spec FR-021). Layers: **CLI/daemon (`main.rs`)** → **`app` (state machine + wiring)** → **iced surfaces (`ui`, `ui_onboarding`, `ui_settings`, `ui_problems`)**; platform adapters `audio`, `engine` (with `stream_lease` owning the session/stream borrow), `hotkey{,_device,_portal}`, `clipboard`, `inserter`, `overlay{,_render,_wayland,_x11}`, `tray`, `ipc`; infrastructure `config`, `model_catalog` (the bundled, versioned snapshot of the `transcribe.cpp` catalog), `models` (verified downloads), `diagnostics` + `vulkan_icd` (the capability checks and the missing-driver advice they show), `logging`.

```mermaid
flowchart LR
  HK["hotkey::bind<br/>portal (ashpd) · evdev listener — never both"] --> SM
  MIC["audio::start on its own thread<br/>cpal → downmix → rubato 16 kHz mono"] --> SM
  SM["app::DictationMachine<br/>pure; returns DictationEffects"] -->|Start · Feed · Stop| ENG["engine worker thread<br/>transcribe-cpp, Vulkan → CPU"]
  ENG -->|Partial · Final| SM
  SM -->|Insert| INS["TextInserter<br/>1. clipboard owns selection<br/>2. uinput Ctrl+V"]
  SM --> OV["OverlayPresenter<br/>layer-shell · X11 · noop"]
  SM --> TRAY["ksni tray"]
```

Key structural rules:

- `DictationMachine` (`src/app.rs:168`) is **pure**: `hotkey/chunk/partial/queued/final_text/insert_finished/failed/tick` each take an injected `now: Instant` and return effects. All I/O, spawning and UI mutation happen in `App::run` (`src/app.rs:898`). Never put I/O, `Instant::now()` or a channel send into the machine.
- **Blocking native work runs on dedicated `std::thread`s with `std::sync::mpsc`; tokio exists only where async is inherent** (ashpd portals, the `ureq` model download, `tokio::net` IPC, `ksni`, iced tasks). `src/engine.rs:12-13`: "Nothing here runs on a tokio task." `src/models.rs` states the project rule that blocking work never occupies a tokio worker.
- One process-wide inbound channel: `static EVENT_CHANNEL: OnceLock<…>` (`src/app.rs:85`) fed by pump threads, tray and IPC, drained by the iced subscription `Subscription::run(event_stream)` (`src/app.rs:1400`). Every producer goes through `AppEvent`.
- iced owns the tokio multi-thread runtime (`iced::daemon(…).run()` in `run_daemon`, `src/main.rs:287`); the app never builds one, except in the two verification CLI paths that run without iced (`src/main.rs:119`, `:156`).
- Single instance: `ipc::notify_running_instance(OpenSettings)` at daemon start forwards "open settings" to `$XDG_RUNTIME_DIR/lingua/ipc.sock` and exits 0.

## Key Directories

| Path | Purpose |
| --- | --- |
| `src/` | Entire application, flat: one responsibility per file (see Important Files) |
| `.spec/` | `wayland-voice-dictation.md` — approved spec with FR/SC ids; the only requirements doc |
| `PRODUCT.md` / `DESIGN.md` / `.impeccable/` | Design context (Impeccable): product truth in `PRODUCT.md` (schema-tagged), the visual system in `DESIGN.md` (token frontmatter + the `.impeccable/design.json` sidecar), the chosen direction contract in `.impeccable/surfaces/*.md`; ephemeral decision/round state under `.impeccable/mocks/` and `.impeccable/review/` is gitignored. The skill itself is vendored at `.agents/skills/impeccable/` (omp loads it project-scoped; the engine binary under its `scripts/bin/` is gitignored and re-downloaded) |
| `assets/` | `icon/` — the master `hicolor/scalable/apps/lingua.svg`, its seven `hicolor/<N>x<N>/apps/lingua.png` renders (`rsvg-convert`, no build-time rasterizer) and `lingua.desktop`; the 256 px render is `include_bytes!`-embedded as `ui::ICON_PNG` (tray pixmap + window icon). Fonts `Inter-Regular.ttf`, `BarlowCondensed-*.ttf` + OFL licenses |
| `samples/` | `jfk.wav`, 11 s 16 kHz mono PCM fixture for the `--audio-file` path (not read by any `#[test]`) |
| `target/` | Build output, gitignored (`.gitignore`: `/target` + Rust backup/editor/OS noise); contains the CMake trees of `transcribe-cpp-sys` |

Absent on purpose: `tests/`, `benches/`, `examples/`, `scripts/`, `Makefile`/`justfile`, `.github/`, `docs/`, `CONTRIBUTING`/`CHANGELOG`, `rustfmt.toml`/`clippy.toml`/`.editorconfig`, `rust-toolchain.toml`/`.cargo/config.toml`.

## Development Commands

```sh
# System deps (Arch; required before the first build)
sudo pacman -S --needed base-devel cmake vulkan-headers spirv-headers shaderc alsa-lib blas cblas

cargo build --release            # slow: CMake builds transcribe.cpp + ggml (CPU + Vulkan), ~200 .comp shaders
./target/release/lingua          # first run: onboarding (model, language, key, system check)

# Verification quick reference (README.md:237-251)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -- --ignored                       # overlay window-property smoke tests (they self-skip without a display)
cargo run --release -- --diagnostics
cargo run --release -- --audio-file samples/jfk.wav --print-transcript --latency
LINGUA_OVERLAY=x11 cargo run --release        # exercise the GNOME/XWayland overlay path locally
```

The spec words the lint gate as `cargo clippy -- -D warnings` (FR-020/SC-008); the README form with `--all-targets` is the stricter one. Run the README form.

CLI surface is exactly `--diagnostics`, `--audio-file <16-kHz-mono-PCM-WAV> [--print-transcript] [--latency]`; unknown args print usage and exit 2. `--audio-file` also triggers the model download if the GGUF is missing (`src/main.rs:162-185`).

## Code Conventions & Common Patterns

- **Formatting/lints are tool defaults** — no `rustfmt.toml`, no `clippy.toml`, no `[lints]` table. Match surrounding style instead of inventing config.
- **`unwrap`/`expect` are compile-time banned outside tests**: `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]` (`src/main.rs:7`). Use `?`, `.context(…)`, `bail!`, or `ok_or_else`.
- **Errors**: `anyhow::Result` everywhere; `.context("…")` on `?` at boundaries (`src/main.rs:56`). Typed errors exist only where the UI needs structure — `BindError { detail, remediation }` (`src/hotkey.rs:157`) and `PortalError` (`src/hotkey_portal.rs:35`). Cross-module failures travel as `String` via `format!("{error:#}")`.
- **User-visible strings are module `const`s** (reasons, remediations, failure texts), e.g. `src/overlay_wayland.rs:60-93`, `src/clipboard.rs:81-98`, `src/inserter.rs:38-47`. Tests pin them verbatim — change the const and the test together, never loosen the test.
- **Module docs (`//!`) cite FR/Scenario ids** and explain the *why* of non-obvious choices (why `smithay-clipboard` is unused, `src/clipboard.rs:21-22`). Every new file follows this.
- **RAII keepalives**: mechanisms stop when their handle drops — `impl Drop for Engine` (`src/engine.rs:123`), `impl Drop for DeviceKeepAlive` (`src/hotkey_device.rs:67`), `impl Drop for Inner` (`src/hotkey.rs:202`, logs the mechanism's liveness when the binding retires). `StreamLease` (`src/stream_lease.rs`) has no `Drop` impl on purpose: its field order (`stream` before `_session`) makes the language drop the stream first.
- **Structured `tracing` with field syntax; degrade, don't die**: `tracing::warn!(%error, "overlay presentation failed; continuing without the indicator")` (`src/app.rs:1073`). No tray host / no layer-shell / no `/dev/uinput` are *expected* degradations, not bugs.
- **Edition 2024 idioms used freely**: let-chains (`if let Some(d) = &self.download && d.is_running()`, `src/app.rs:1112`), `is_none_or`, `as_chunks_mut`.
- Naming: `snake_case` descriptive verb phrases for functions/tests, `CamelCase` types, `SCREAMING_SNAKE` consts. One module ≈ one concern; split rather than nest.

**Invariants you must not break** (mostly encoded in comments/consts):

- Clipboard must own the selection **before** the paste chord is emitted (`src/inserter.rs:136`, FR-013) — reverse it and the focused app pastes stale content.
- A dictation may insert **exactly once** (FR-006); `pending_finals` accounts for finals still in flight, including late finals from a previous cycle.
- Stale audio is dropped by `chunk_generation` (`src/app.rs:756`), and `StopEngine` must be sent even for too-short recordings so the model lease is released (`src/app.rs:252-271`).
- One stream at a time holds the model lease (`StreamLease::open` in `src/stream_lease.rs` retries `Busy` 5×20 ms); audio is routed via `audio_owner` (`src/engine.rs:380`).
- `MIN_HOLD = 300 ms` (tap vs hold-to-talk), 5-minute cap, 100 ms partial throttle, `PASTE_CHORD_HOLD = 25 ms`, 2 s clipboard confirm timeout — all pinned by tests.
- The only `unsafe` in the tree is `src/stream_lease.rs` (two blocks: `&'static mut Session` plus `mem::transmute` to `Stream<'static>`). It is safe for three reasons that live *only* in that module: `session` is a `Box` (stable address), `_session` is declared after `stream` (drop order), and both fields are private, so nothing outside can touch the session while the stream is alive. Keep it that way — do not unbox the session, reorder the fields, expose an accessor, or open a stream anywhere else.
- X11 overlay properties must be set **before the first `map_window`** (`src/overlay_x11.rs:1-23`).
- The engine's language hint must be spelled exactly as the loaded GGUF advertises the language (the C API refuses anything else, in `transcribe_run` *and* `transcribe_stream_begin`): `model_catalog::language_hint` picks an advertised spelling of the configured language — the locale for Nemotron 3.5 (`de-DE`), the bare code for Voxtral and the English-only variants (`de` → `de`) — and `None` for a language the variant does not advertise (warn, run without a hint — FR-011). Never invent a hint spelling. The configured vocabulary is `config::Language`'s table (one locale per language the catalog can serve, `auto` first), which the onboarding pick list, the settings pick list and the model-list filter all share.
- The Parakeet stream extension is attached only after probing `Model::accepts_ext(ExtSlot::Stream, TRANSCRIBE_EXT_KIND_PARAKEET_STREAM)` (`stream_extension`, `src/engine.rs`): the kind is a per-variant property (cache-aware vs chunked-attention vs neither), and an unaccepted kind fails `transcribe_stream_begin`. A variant that accepts the kind but refuses the requested lookahead falls back to its own default instead of losing the dictation.
- The engine keeps `keep_special_tags = true` (`run_options_for`, `src/engine.rs`) and removes the model's `<…>` tags itself via `strip_language_tags` before the text is displayed, copied or typed — that is what makes the *detected* language reachable (Spec: Dictation session, "the language actually used"). Never let a tag reach the overlay, the clipboard or `/dev/uinput`, and never turn `keep_special_tags` off without providing the language another way.

## Important Files

| File | Role |
| --- | --- |
| `src/main.rs` | Entry, 3 CLI modes, single-instance guard, `run_daemon`, crate-level lint attr, all `mod` decls |
| `src/app.rs` | `App`, `AppEvent`, pure `DictationMachine` + `DictationState`/`Gesture`/`DictationEffect`, effect execution, iced `boot`/`update`/`subscription`, downloads |
| `src/engine.rs` | ASR worker thread: model load/SHA preflight, backend probe + CPU retry, `EngineCmd`/`EngineEvent`, `AudioOwner`, partial throttling |
| `src/audio.rs` | cpal capture thread, downmix + `rubato` resample to 16 kHz mono, `Chunk`, WAV reader for `--audio-file` |
| `src/hotkey.rs` | Chord parse/normalise (`parse_chord`, `Chord`, `BindError`), mechanism selection (`portal` first, device fallback) |
| `src/hotkey_device.rs` | evdev listen-only listener: thread per keyboard, `ChordState`, hotplug supervisor |
| `src/hotkey_portal.rs` | `ashpd` GlobalShortcuts portal binding |
| `src/clipboard.rs` | In-process selection ownership: `zwlr_data_control_v1` → `ext_data_control_v1` → X11 `CLIPBOARD` (XWayland bridge) → core `wl_data_device`; one thread each via `calloop` |
| `src/inserter.rs` | `TextInserter` trait, `UinputInserter` (Ctrl+V via `/dev/uinput`), `ClipboardOnlyInserter` fallback, `InsertOutcome` |
| `src/overlay.rs` / `overlay_render.rs` / `overlay_wayland.rs` / `overlay_x11.rs` | Presenter trait + backend probe/override; shared tiny-skia/cosmic-text frame render; layer-shell thread; X11 notification window thread |
|`src/ui.rs` / `ui_onboarding.rs` / `ui_settings.rs` / `ui_problems.rs`|Shared iced vocabulary (messages, `palette`/`metrics`/`style` design tokens, widget helpers, `ChordCapture`, `Step`), onboarding, settings (autosave — every edit applies immediately; config persistence and live reload), startup problem notice (FR-014)|
| `src/config.rs` | `Config`, `Language`, `BackendPreference`, `HotkeySource`, `IdleUnload`, `SharedConfig = Arc<RwLock<Config>>`, atomic TOML save, XDG paths |
|`src/model_catalog.rs`|Bundled catalog snapshot: 8 streaming variants × 23 artifacts (size + SHA-256 + commit-pinned HF URL), ratings (accuracy/speed), the per-model quant recommendation the cards preselect (`Variant::recommended`, rule + tripwire test in the module; the compute backend deliberately plays no part in it), pure selection/language/rating helpers|
| `src/models.rs` | `path_for`, resumable verified `ensure` (`<file>.part` + `Range` + SHA-256 + free-space gate) for a `model_catalog::Artifact` |
| `src/ipc.rs` / `tray.rs` / `diagnostics.rs` / `vulkan_icd.rs` / `logging.rs` | Single-instance socket + JSON-line codec; ksni tray; the four system checks; the missing-Vulkan-driver advice a failed GPU check shows (`/sys/bus/pci/devices` + `/usr/share/vulkan/icd.d` + the package manager on `PATH`, no distro assumed); `tracing-subscriber` init |
| `build.rs` | Emits `-lcblas` when a `cblas.h` exists and `CBLAS_NO_LINK` is unset (transcribe-cpp-sys only names `libblas`); no-op off Linux |
| `README.md` | Operations reference: link/`dlopen` tables, permission matrix, build deps, flags, behaviour, verification commands |

## Runtime/Tooling Preferences

- **Toolchain**: stable Rust ≥ 1.98 (edition 2024). No `rust-toolchain.toml` — the MSRV is declarative only. Cargo is the only build tool; no task runner in the repo.
- **Native build**: needs `cmake ≥ 3.16`, a C/C++ compiler, `make`, `pkgconf`, `glslc` + `glslangValidator`, Vulkan **headers and the loader stub** (`vulkan-icd-loader` — ggml links `libvulkan.so`), SPIRV headers, ALSA headers, `blas` + `cblas`. Not needed: Wayland/X11 dev packages, `libclang`/bindgen (bindings are committed), Python, OpenMP, `ydotool`. The vendored C++ is linked **statically** (`shared = false`, backends `vulkan`+`cpu`).
- **`GGML_NATIVE=ON`** in the vendored CMake build → the release binary is host-CPU-tuned (`-march=native`) and **not portable** to other machines.
- **Runtime**: linked `alsa-lib`, `vulkan-icd-loader`, `blas`, `cblas` (+ `libgfortran` via `blas`); `dlopen`ed `wayland-client`, `xkbcommon`, `xkbcommon-x11` and — only on the X11/Xwayland paths — `X11`, `Xi`, `xcb`. **No GL stack**: iced uses the `tiny-skia` renderer with `softbuffer` (`cargo tree -i wgpu` prints nothing), so no EGL/GL/glvnd and no `libwayland-egl`; needs a Vulkan ICD (`mesa`), PipeWire + `pipewire-alsa`, D-Bus session bus, a StatusNotifier host, `xdg-desktop-portal` with `GlobalShortcuts`, logind `uaccess` on `/dev/uinput` (paste) and `/dev/input/event*` (device hotkey; otherwise `sudo usermod -aG input $USER` + re-login). `--diagnostics` reports all four checks with remediation.
- **Config** `~/.config/lingua/config.toml`: `onboarding_complete`, `model` (catalog id, default `nemotron-3.5-asr-streaming-0.6b-Q8_0`), `language` (`auto` or a locale from the catalog's language table — 32 languages, default `auto`; an unknown token falls back to `auto`), `hotkey` (default `CTRL+SUPER`), `hotkey_source` (`portal|device`), `backend` (`auto|vulkan|cpu`), `input_device` (`system|<host>:<device>`, default `system`, key omitted while default), `idle_unload` (`never|1|5|15|30|60` minutes, default `15`, always written; an unrecognized value falls back to the default). Models live in `~/.local/share/lingua/models` (default 751 MB, downloaded on first dictation, SHA-256-verified).
- **Env vars**: `RUST_LOG` (default `lingua=info`, stderr), `LINGUA_OVERLAY=layer|x11` (force overlay backend), `LINGUA_HOTKEY_PORTAL=0` (skip the portal, force the device listener), `XDG_RUNTIME_DIR` (required for the IPC socket), `DISPLAY`/`WAYLAND_DISPLAY`; build-time `CBLAS_NO_LINK`, `TRANSCRIBE_DIR`, `TRANSCRIBE_CMAKE_ARGS`, `CMAKE_ARGS`, `VULKAN_SDK`, `GGML_CCACHE=OFF`.
- **No CI, no packaging, no release process**: no `.github/`, `PKGBUILD`, systemd unit, Dockerfile or nix flake (`assets/icon/lingua.desktop` + `install-desktop.sh` install into the user's XDG data dirs only). `publish = false`. Nothing enforces the gates below except the person running them.

## Testing & QA

- All tests are **inline `#[cfg(test)] mod tests` blocks at the end of the file** (21 of 26 modules): 206 `#[test]` fns (the count drifts as work lands), 4 `#[ignore]`. No `tests/`, no test fixtures at all, no dev-dependencies, no async runtime in tests.
- Run: `cargo test` (unit), `cargo test -- --ignored` (display-dependent smoke), single test via module path: `cargo test app::tests::hold_to_talk_stops_on_release -- --exact`, whole module: `cargo test config::tests`. No feature flags exist (`--features` fails), and there is no lib target, so doc tests do not apply.
- Style to follow:
  - Descriptive snake_case sentence names, no `test_` prefix: `save_then_load_roundtrips_and_leaves_no_temp_file` (`src/config.rs:404`), `rejects_48k_stereo_wav_with_the_contract_literal` (`src/audio.rs:957`).
  - **Pin contract literals as `const`s inside the test module** and compare with production consts/strings (`src/hotkey.rs:412-415`, `src/diagnostics.rs:254-276`, `src/models.rs:532`). Comments cite FR/SC ids.
  - Inject time instead of sleeping: `machine.hotkey(Press, t0)` / `(Release, t0 + MIN_HOLD + 1 ms)` (`src/app.rs:1471-1483`).
  - Channel assertions are timeout-bounded: `Err(RecvTimeoutError::Timeout)` within 200 ms, liveness within 10 s (`src/engine.rs:1027-1043`). The engine worker is driven synchronously over `std::sync::mpsc`, not `#[tokio::test]`.
  - Temp files are hand-rolled under `std::env::temp_dir()` namespaced by `std::process::id()` (+ counter in `src/models.rs:478`) and removed at the end; tests must never write below `$HOME`. `src/config.rs:404` also asserts no `config.toml.tmp` survives.
  - Global-clipboard tests serialise on `pub(crate) static SELECTION_LOCK` (`src/clipboard.rs:1051`) — hold it in any new test that touches the selection.
  - `unwrap`/`expect` are fine in tests (allowed by the `cfg_attr` in `src/main.rs:7`).
- The 4 `#[ignore]` tests are live smoke tests: X11 window properties (`src/overlay_x11.rs:1062`), layer-shell show/draw/hide (`src/overlay_wayland.rs:870`), and clipboard ownership/insertion (`src/clipboard.rs:1056`, `src/inserter.rs:209`, needs `wl-paste` for independent verification). The overlay two **silently pass** when `DISPLAY`/`WAYLAND_DISPLAY` is unset; only the clipboard two hard-fail without a session.
- Coverage gaps to respect before widening scope: **no tests** in `logging.rs`, `ui_onboarding.rs`, `ui_problems.rs`, `hotkey_portal.rs`; **no coverage** of `models::ensure` (resume/tamper, SC-009), `ipc::serve`, tray runtime registration, iced `boot`/subscription wiring, real cpal capture, real ASR inference, and `build.rs`.
- **No automated non-test verification exists** — for behavior changes, run the matching command from the quick reference (`--diagnostics`, `--audio-file samples/jfk.wav --print-transcript --latency`, or the real daemon) rather than trusting unit tests alone.
