# Littera — local voice dictation for Linux/Wayland

Press a global hotkey, speak, and the transcript lands at the cursor of the focused
application. Speech recognition runs **fully in-process** (Nemotron 3.5 ASR streaming
0.6B via [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp), static
Rust bindings, Vulkan with automatic CPU fallback). No account, no cloud, no separate
speech runtime.

Targets Wayland desktops: KDE Plasma, Hyprland (wlroots), COSMIC and GNOME — with
explicit, user-visible handling of each compositor's capability gaps.

## Installation

**Arch Linux (AUR).** The AUR package ships the prebuilt release binary:

```sh
yay -S littera-bin            # or: paru -S littera-bin
littera                       # first run: onboarding (model, language, key, system check)
```

The package is `littera-bin` on the [AUR](https://aur.archlinux.org/packages/littera-bin);
`provides=('littera')`, so a later source package with that name can replace it. The
binary is built for baseline x86-64 (`TRANSCRIBE_X86_CONSERVATIVE`), not for the
release runner's CPU.

**From source** (see [Build dependencies](#build-dependencies)):

```sh
cargo build --release
./install-desktop.sh          # desktop entry + hicolor icons into the XDG data dirs
./target/release/littera
```

`install-desktop.sh` is required for the hotkey portal path: the portal identifies a
caller by its installed desktop entry, and without one it refuses the shortcut dialog.

**Renamed from `lingua`.** Builds from before the rename used `lingua` for the
binary, the config/data directories, the IPC socket and the desktop entry. Move the state
once and re-install the entry:

```sh
mv ~/.config/lingua ~/.config/littera
mv ~/.local/share/lingua ~/.local/share/littera
rm -f ~/.local/share/applications/lingua.desktop
./install-desktop.sh
```

The new desktop-entry stem (`littera`) is a new portal application id, so the compositor
asks for the hotkey once more.

## Scope: Linux only, Wayland sessions only

**This is a Linux/Wayland project.** It needs a Wayland compositor on a Linux desktop
and is not ported to macOS, Windows, BSD, or to an X11 session.

- **X11 is not a supported session type.** X11 code exists exclusively *inside* a
  Wayland session through XWayland, for the two cases a Wayland-only route cannot
  cover: the GNOME overlay window (mutter implements neither `zwlr_layer_shell_v1` nor
  a non-focusing Wayland toplevel) and the X11 `CLIPBOARD` bridge for compositors
  without a data-control protocol. Running the binary under a plain Xorg session is
  neither tested nor supported.
- **Wayland capabilities are probed, never assumed.** The overlay picks
  `zwlr_layer_shell_v1`, else the XWayland window, else nothing; the clipboard picks
  `zwlr_data_control_v1`, `ext_data_control_v1`, the Xwayland bridge, else the core
  `wl_data_device`; the hotkey picks the `GlobalShortcuts` portal, else the read-only
  evdev listener; auto-paste needs `/dev/uinput`, else the transcript is copied only.
  Every gap degrades to a documented fallback and is named in
  `littera --diagnostics` and in the app's startup problem notice.
- **No other platform glue**: no GTK/Qt shell (the GUI is iced), no
  `ydotool`/`xdotool`/`wtype`, no Python, no separately installed speech runtime, no
  cloud.

## Runtime dependencies

What the built binary needs on the machine. Everything below is checked by
`littera --diagnostics` (hotkey access, paste permission, GPU backend, overlay backend);
the app opens a **problem notice** by itself whenever one of those checks fails — also
when it otherwise starts without any window.

### Libraries the binary links directly

`ldd target/release/littera` on the reference machine prints exactly these:

| Library | Package |
| --- | --- |
| `libc.so.6`, `libm.so.6`, `libstdc++.so.6`, `libgcc_s.so.1` | `glibc`, `gcc-libs` |
| `libasound.so.2` | `alsa-lib` |
| `libvulkan.so.1` | `vulkan-icd-loader` |
| `libblas.so.3` | `blas` |
| `libcblas.so.3` | `cblas` |
| `libgfortran.so.5` | `libgfortran` (pulled in by `blas`) |

Not in that list because it is **linked in statically**: `mimalloc`, installed as the
process-wide Rust allocator at the crate root (`#[global_allocator]`, `src/main.rs`).
Every Rust allocation avoids glibc's allocator because the daemon allocates from several
threads at once and frees across them; C/C++ code (`ggml`, glibc itself) still goes
through glibc's `malloc`.

### Libraries loaded on demand (`dlopen`)

Not link-time dependencies, but the windowing stack resolves them at runtime. Measured
as the remaining `lib*.so[.N]` literals inside the binary
(`strings -a target/release/littera | grep -oE 'lib[a-zA-Z0-9_+-]*\.so(\.[0-9]+)*'`):

| Library | Package | Loaded for |
| --- | --- | --- |
| `libwayland-client.so.0` | `wayland` | every Wayland path: iced windows and the layer-shell overlay |
| `libxkbcommon.so.0` | `libxkbcommon` | keyboard handling of the GUI windows (Wayland and X11) |
| `libX11.so.6` | `libx11` | the X11 backend of the windowing stack — only reached inside a Wayland session when the GNOME/Xwayland overlay path is used |
| `libxkbcommon-x11.so.0` | `libxkbcommon-x11` | X11 keymap handling alongside that backend |
| `libXi.so.6` | `libxi` | X11 input extension of that backend |
| `libxcb.so.1` | `libxcb` | `x11rb`'s `xcb` backend, used by the X11 overlay window and the X11 `CLIPBOARD` bridge (Xwayland) |

No `libEGL`/`libGL`/`libglvnd`, no `libwayland-egl`, no `libXcursor`, `libXrandr`,
`libXinerama` or `libXxf86vm`: iced renders and presents through `tiny-skia` +
`softbuffer` (`softbuffer` uses `wl_shm` on Wayland and `xcb`/SHM on X11), and the X11
code paths above only need the six libraries listed.

### Services, devices and permissions

| Requirement | Needed for | Without it |
| --- | --- | --- |
| Vulkan ICD (`mesa`, RADV on the reference machine) | GPU inference | CPU backend plus an explicit, user-visible notice (FR-005) |
| PipeWire plus `pipewire-alsa` (`pipewire`, `pipewire-pulse`, `wireplumber`) | microphone capture | `no default audio input device found (PipeWire/ALSA): check wpctl status` |
| D-Bus session bus (`/run/user/<uid>/bus`) | tray icon and desktop portal | the tray is only logged as unavailable; the portal path falls back to the device listener |
| StatusNotifier host (provided by the desktop shell) | tray icon (FR-017) | settings stay reachable by relaunching the binary |
| `xdg-desktop-portal` exposing `GlobalShortcuts` | compositor-side hotkey binding | the listen-only device listener is used instead (FR-008) |
| an installed desktop entry (`install-desktop.sh` puts `littera.desktop` in `~/.local/share/applications`) | the portal can identify the application before it opens its shortcut dialog | the portal refuses the request (`NotAllowed: An app id is required`) and the listen-only key devices bind the recorded combination instead |
| logind `uaccess` ACL on `/dev/input/event*`, or membership in the `input` group | reading the hotkey | the system check prints `sudo usermod -aG input $USER` + re-login |
| logind `uaccess` ACL on `/dev/uinput` | synthesizing the paste chord | clipboard-only mode; the overlay names the blocked path (FR-013) |
| XWayland | GNOME overlay and the X11 clipboard bridge | no overlay on GNOME, no X11 clipboard fallback |
| `xdg-open` (`xdg-utils`) | opening the models folder from the settings window | the click reports the failure (FR-019); dictation is unaffected |
| `ca-certificates` (system trust store) | HTTPS model download (rustls with the platform verifier) | the download cannot be verified |
| a compositor implementing `zwlr_data_control_v1`/`ext_data_control_v1`, or XWayland | owning the clipboard selection from the background | only the core `wl_data_device` path remains, which needs keyboard focus — logged as a warning |

Permissions, concretely:

1. **Hotkey listening.** Without a shortcut portal Littera reads the keyboard devices
   read-only and **never grabs them**:
   ```sh
   sudo usermod -aG input $USER   # then log out and back in
   ```
   logind's `uaccess` ACL usually makes this unnecessary for the session's keyboards; the
   system check prints the command whenever the devices are genuinely unreadable.
2. **Auto-paste.** The paste chord is synthesized in-process into `/dev/uinput`; logind
   grants the session user write access through its `uaccess` ACL, so this normally works
   out of the box.

**Not** runtime dependencies: `ydotool`/`ydotoold` or any other injection daemon, a Python
interpreter, a separately installed speech runtime, `libdbus` (the D-Bus protocol is
spoken over the session socket), a clipboard manager, or on-screen notification daemons.

## Build dependencies

| Dependency | Why it is needed |
| --- | --- |
| Rust 1.98+ (edition 2024) | the project itself |
| C++ toolchain (`base-devel`: gcc, make) and `pkgconf` | transcribe.cpp is compiled through CMake; the `mimalloc` crate builds its C sources with `cc`; `alsa-sys` probes ALSA with `pkg-config` |
| `cmake` (>= 3.16) | the engine's build system (ggml) |
| `vulkan-headers`, `spirv-headers` and `vulkan-icd-loader` | ggml's Vulkan backend requires `find_package(Vulkan COMPONENTS glslc REQUIRED)` and `find_package(SPIRV-Headers CONFIG REQUIRED)`, and **links** `libvulkan.so` — that loader stub comes from `vulkan-icd-loader`, not from the headers package |
| `shaderc` (provides `glslc`) and `glslang` (provides `glslangValidator`) | compile and validate the Vulkan compute shaders; `shaderc` depends on `glslang`, so installing it is enough |
| `alsa-lib` | ALSA headers and `libasound` for `cpal` — capture goes through `pipewire-alsa` |
| `blas` + `cblas` | the host decoder calls CBLAS whenever `cblas.h` is present; `build.rs` mirrors that probe and links `-lcblas`. Distributions whose BLAS ships the C interface in the same library (OpenBLAS, MKL, Accelerate) cover both |

```sh
sudo pacman -S --needed base-devel cmake vulkan-headers spirv-headers vulkan-icd-loader shaderc alsa-lib blas cblas
```

**Not** needed at build time: Wayland/X11 development packages (every protocol is
implemented in Rust — `wayland-client`, `wayland-protocols*`, `smithay-client-toolkit`,
`x11rb` — and the native libraries are loaded at runtime), `libclang`/`bindgen` (the
generated bindings ship with the crate), a Python interpreter, `git` (the C++ sources
are vendored inside `transcribe-cpp-sys`), and OpenMP.

**No GL/EGL/`wgpu` at all**: iced is built with the `tiny-skia` renderer and presents
through `softbuffer` (`cargo tree -i wgpu` prints nothing), so neither the build nor the
runtime needs a GL stack, `libglvnd` or `libwayland-egl`.


## Build and run

```sh
cargo build --release
./target/release/littera          # first run: onboarding (model, language, key, check)
```

The model section lists every streaming ASR variant of the bundled `transcribe.cpp`
catalog snapshot (8 variants, 23 quantized artifacts) as a two-line field row: line one
carries the name, the download size, the state's lamp and its one icon control (amber lamp
+ download arrow when not downloaded, hollow lamp + tick when on disk and unchecked, green
lamp + tick when verified, a progress bar + cross while a transfer runs, a fault lamp +
retry cycle after a failure; each control keeps its word in a tooltip, and the state is
never spelled out in the row); line two the language summary, the quant selector, the accuracy/speed bars
scaled to the best listed model, a `No <language>` marker when the variant cannot serve
the configured language, and the chevron that opens the detail. The active model's row
leads the list and is not repeated below. Rows are plain page content — the page scroll is
the only scroll — and the exact published values, the parameter count, the license, the
dataset and reference machine, the variant's note and a file's SHA-256 are in the row's
expandable detail; the catalog snapshot version is stated once, in the Storage card. The
quant a row preselects is the catalog's
per-model recommendation: the smallest quant measured within 0.05 pp of Q8_0 *and* with a
published throughput — Q4_K_M on Voxtral Mini 4B (also the only measured speed win),
Parakeet Unified EN and Parakeet Multitalker; Q8_0 wherever nothing smaller is measured on
both axes (both Nemotrons, Moonshine). The compute backend plays no part in it: the error
of a quantisation is backend-independent, and Q4_K_M is 1.0–3.9 % *slower* on either
backend wherever it is measured — except the memory-bound Voxtral, where it wins on both
(+15 % Vulkan, +26 % CPU). Q6_K/Q5_K_M stay one click away on every card. One download
runs at
a time: it is resumable and verified against the pinned SHA-256 before the artifact is
loaded, it can be cancelled (the partial file is kept) and retried, and the finished
model becomes active immediately. The default is Nemotron 3.5 ASR Streaming 0.6B in
Q8_0 (751 MB) — multilingual, 32 locales; the list also offers the English-only
variants (down to 50 MB) and Voxtral Mini 4B Realtime (4.73 GB).

`littera` runs as a single instance: launching it again forwards "open settings" to the
running process and exits. A tray icon (StatusNotifierItem) shows the dictation state
and offers *Open settings* / *Quit*; without a tray host, relaunching the binary opens
the settings window.

### Command-line flags

```
littera --diagnostics
littera --audio-file <16-kHz-mono-PCM-WAV> [--print-transcript] [--latency]
```

- `--diagnostics` prints the four system checks (`hotkey access`, `paste permission`,
  `GPU backend`, `overlay backend`) with the remediation command for anything failing,
  plus the effective model, language and backend preference (FR-022), then exits 0.
- `--audio-file` runs the production pipeline (chunking → engine → clipboard +
  insertion) on a file instead of the microphone, which makes streaming behaviour
  measurable without a human: `--print-transcript` prints the final text (plus
  `language=<code>` — the language the model reported for the recording, or the
  configured hint when it tagged none), `--latency` prints `first_partial_ms=`
  (first chunk → first non-empty partial) and `final_ms=` (stop → final transcript).
  The overlay is interactive-only and is not exercised by this flag.

## Behaviour

| Gesture | Result |
| --- | --- |
| Press, release within 300 ms, press again | press-to-start / press-to-stop |
| Press, hold ≥ 300 ms, release | hold-to-talk |
| Silence, or release below ~0.3 s of audio | nothing is inserted |
| A new press while the previous dictation finalizes | the recording is queued, no audio is lost |
| Five minutes of audio | recording auto-stops and inserts |

- The overlay is pinned bottom-centre of the monitor with keyboard focus via
  `zwlr_layer_shell_v1` where the compositor supports it, reserves no layout space,
  takes no keyboard focus and lets pointer input pass through. On GNOME (no
  layer-shell) it is a non-activating, notification-typed X11 window — GNOME itself
  is the only target without the layer protocol, and GNOME 48+ binds the hotkey
  through the compositor's own shortcut portal.
- A dictation does not eat the clipboard: auto-paste reads the text selection it is about to
  displace and puts it back once the paste chord had its chance (a copy that arrives in between
  wins; non-text selections are left to the transcript). In clipboard-only mode — no writing
  injection device — the transcript itself stays on the clipboard. The selection is owned by an
  in-process wayland client that picks the first available mechanism —
  `zwlr_data_control_v1`, then `ext_data_control_v1` (both let a background client own
  the selection without keyboard focus), then an **X11 `CLIPBOARD` owner bridged by
  Xwayland** (the mutter/GNOME-compatible route, since mutter validates the
  `wl_data_device` serial and a dictation daemon never holds focus), and finally the core
  `wl_data_device` with a logged warning. `copy_text` waits for the compositor's
  acknowledgement before the paste chord is synthesized, so a stale clipboard can never
  be pasted. (`smithay-clipboard` was not usable here: it only sets a selection while its
  own client holds keyboard focus. The settings window names the active mechanism.)
- Failed capability checks (hotkey access, paste permission, GPU backend, overlay)
  open a **problem notice** window at startup — the daemon itself starts without any
  window — listing each failure with its exact fix and a *Re-check* button; a passing
  re-check closes the notice. A Vulkan→CPU fallback is a failed GPU check unless
  `backend = "cpu"` asked for it: it keeps the fallback reason on screen, names the
  command that installs the driver the machine is missing (the GPU's vendor is read from
  `/sys/bus/pci/devices`, the installed ICDs from `/usr/share/vulkan/icd.d`, the package
  manager from `PATH` — e.g. `sudo pacman -S vulkan-intel — no Vulkan ICD for the Intel
  GPU`), and dictation keeps working on the CPU.
- Settings are stored in `~/.config/littera/config.toml`; every change in the settings
  window applies immediately (autosave — there is no Save button) and persists without a
  restart. The **Language** section is one pick list: `Auto-detect` (the default) first,
  then every language the bundled catalog can serve — and the model list's language
  filter is a pick list over the same set, so the list can be narrowed to the languages
  that actually matter here. Selecting a model never blocks on the language: a model that
  cannot serve the configured language stays selectable, warns on its card, and runs
  without a language hint (the engine's hint spelling follows what the loaded GGUF
  advertises — `de-DE` for Nemotron 3.5, bare `de` for Voxtral). A change the app cannot
  apply (a failed config write) keeps the previous value and says why right there in that
  section. The
  model section shows the selected model's load lifecycle: `Loading <model>…` while a
  load or reload is in flight, `Backend  Vulkan — <device>` once it is resident (a CPU
  fallback keeps its reason on screen, FR-005), `<model> is not loaded — the next
  dictation loads it` after an automatic unload, and `Load failed — <model>` with the
  reason when a load fails. Below it, the **Unload idle model** card picks the idle
  timeout (Never, 1, 5, 15, 30 or 60 minutes, default 15): once the model has been idle
  that long its memory is released, and the next dictation keeps recording while it
  loads again.
- The settings window's **Microphone** section is one pick list: `System default` first, then
  every capture device the audio backend can open *at that moment* (name plus technical
  identifier on the option line), and picking one stores it in the `input_device` key. The list
  is rebuilt on every visit to the window (no refresh control, no background watcher). A
  configured device that is currently unavailable stays in the list, marked `currently
  unavailable`, and a recording that cannot resolve or open it transparently falls back to the
  system default — the identifier and the cause go to the log, never to the UI. A recording in
  progress keeps the device it started on.
- The engine reads the language tag the model emits per segment (`<en-US>`, with
  `keep_special_tags` on) and strips it before anything user-visible sees the text, so
  FR-007 still holds: neither the overlay, nor the clipboard, nor the synthesized
  keyboard input ever contains a tag. The reported language is logged
  (`detected=…`) and printed by `--print-transcript` as `language=<code>`; a
  recording that switches language mid-way is reported with the language it
  started in, and a recording the model did not tag (or a short one) falls back to
  the configured hint.

## Known limitations

- **Measured on the reference machine** (`--audio-file samples/jfk.wav --latency`, 11 s
  utterance, cold process): `final_ms` = 30 ms (Vulkan) / 36 ms (CPU), i.e. the final
  transcript is inserted well inside the 1.5 s budget, while the *first partial* arrives
  after ≈1.3–1.4 s. The one-second target is therefore not met by the streaming model
  itself at zero lookahead (`att_context_right = 0`); the overlay covers that window with
  an immediate `Listening… 0:00` state and a live input-level bar. Raising
  `att_context_right` would delay the first partial further, not earlier.
- **Terminals** often paste with `Ctrl+Shift+V`; Littera always sends `Ctrl+V`, so the
  clipboard path is the workaround.
- **Microphone pinning under PipeWire**: the entries in the Microphone section follow the
  ALSA device layer, so the PipeWire plugins (`alsa:default`, `alsa:pipewire`) record from
  the system's current *default source*, and raw hardware PCMs are only offered while the
  sound server does not hold them. Pinning one specific PipeWire source is done outside the
  app (change the default source in WirePlumber/`wpctl`), not in the list.
- A **listen-only device listener cannot suppress desktop bindings**, so a chord like
  the default `Ctrl+Super` may additionally trigger a desktop action. Pick a chord your
  desktop does not use, or use a compositor with the shortcut portal (KDE, GNOME 48+,
  Hyprland), where Littera binds through the compositor and nothing is read from the
  input devices.
- The **`Bind…` button in the Hotkey section asks for the combination**: press it in the
  window and it is bound. The mechanism still follows the desktop's capability — the
  compositor's portal when it binds exactly that combination (its own dialog shows up for
  a first binding, and a chord chosen there counts), otherwise the listen-only key
  devices. A portal that already holds a shortcut for the application answers from its
  own store without asking anyone; that answer never replaces the recorded combination,
  the card names it, and the key devices serve the chord you pressed. If the portal never
  opens a dialog at all, it cannot identify the process — run `./install-desktop.sh`
  once and restart Littera.
- **Fractional scaling**: the layer-shell overlay rounds the fractional output scale to
  the nearest integer (Wayland's `wp_fractional_scale_v1` is not implemented yet).
- GNOME's non-activating X11 overlay is covered by this repository's smoke test
  (`cargo test -- --ignored`, which asserts `_NET_WM_WINDOW_TYPE_NOTIFICATION`,
  `WM_HINTS.input = False`, the empty `SHAPE` input region and that the window never
  holds the input focus); a full GNOME 48/49 session remains the acceptance environment
  for the portal path.
- **Xwayland refuses 32-bit (ARGB) children of its root window** (`BadMatch` naming the
  root window, tested against the local Xwayland with and without
  `override_redirect`, with every 32-bit visual and its own colormap). The X11 overlay
  therefore degrades automatically to an opaque card on the root visual; only the
  rounded-corner transparency is lost, while every property FR-011 depends on
  (`_NET_WM_WINDOW_TYPE_NOTIFICATION`, `WM_HINTS.input = False`, empty `SHAPE` input
  region, never focused) is identical. On a real Xorg server the ARGB path is used —
  an Xorg *session* is still not a supported target (see [Scope](#scope-linux-only-wayland-sessions-only)).
- **Hotkey device access** on the reference machine comes from logind's `uaccess` ACL on
  the keyboard devices, so the listener works without the `input` group; where the ACL is
  absent (or a device is created by `uinput`, which gets no ACL), the system check prints
  the `usermod` command instead.

## Assets

- `assets/Inter-Regular.ttf` — Inter, SIL Open Font License 1.1 (see
  `assets/Inter-LICENSE.txt`), embedded into the binary and used for the overlay text.
- `assets/icon/hicolor/scalable/apps/littera.svg` — the icon master: the tile in the
  palette's accent colour with the motif in the on-accent ink, derived from the SVG Repo
  artwork *speech-right*. Every raster is rendered from this one file with
  `rsvg-convert`, so the project gains no build-time rasterizer:

  ```sh
  for n in 16 24 32 48 64 128 256; do
    rsvg-convert -w $n -h $n assets/icon/hicolor/scalable/apps/littera.svg \
      -o assets/icon/hicolor/${n}x${n}/apps/littera.png
  done
  ```
- `assets/icon/hicolor/<size>/apps/littera.png` — the 8-bit RGBA set at 16, 24, 32, 48,
  64, 128 and 256 px plus the scalable SVG, in the freedesktop hicolor layout under the
  single lowercase name `littera`.
- `assets/icon/hicolor/256x256/apps/littera.png` — the same file the binary embeds as
  its tray pixmap and window icon; a decode failure only logs a warning and the daemon
  keeps running.
- `assets/icon/littera.desktop` and `./install-desktop.sh` — the desktop entry
  (`Icon=littera`, `StartupWMClass=littera`) and the one command that copies it plus the
  icon set into `${XDG_DATA_HOME:-~/.local/share}`; it rewrites `Exec` to this
  checkout's `target/release/littera`, needs no root and is safe to re-run.

## Releases and AUR packaging

The repository publishes itself; there is no manual tagging step.

- **Versioning** — pull requests are squash-merged with a
  [Conventional Commit](https://www.conventionalcommits.org/) title (`feat:`, `fix:`,
  `perf:`, …); `.github/workflows/pr-title.yml` rejects anything else.
  [semantic-release](https://semantic-release.gitbook.io/) reads the merged titles on
  `main`, writes `CHANGELOG.md`, bumps `Cargo.toml` and `Cargo.lock` through
  `scripts/bump-version.sh`, commits that as `chore(release): X.Y.Z [skip ci]` and tags
  `vX.Y.Z`. A `fix`/`feat`/`perf` type produces a release; `chore`/`ci`/`docs` do not.
- **Build** (`release.yml`, job `build`) — runs in an `archlinux:base-devel` container,
  builds with `TRANSCRIBE_CMAKE_ARGS=-DTRANSCRIBE_X86_CONSERVATIVE=ON` (no `-march=native`,
  no per-tier x86 flags), strips the binary, and uploads two release assets:
  `littera-X.Y.Z-x86_64.tar.gz` (binary, desktop entry, hicolor icons, `LICENSE`) and its
  `.sha256`.
- **AUR** (`release.yml`, job `aur`) — renders `packaging/PKGBUILD` with the version, the
  archive checksum and the glibc floor read from the built binary (`depends=('glibc>=…')`,
  so pacman refuses an install on a system older than the build container), regenerates
  `.SRCINFO` with `makepkg --printsrcinfo`, and pushes to
  `ssh://aur@aur.archlinux.org/littera-bin.git`. It authenticates with the repository
  secret `AUR_SSH_KEY` (a passphrase-less ed25519 key whose public half is registered in
  the AUR account) and the repository variables `AUR_USERNAME` and `AUR_MAINTAINER_EMAIL`;
  the AUR host key is pinned in `.github/aur_known_hosts`.
- **First submission is manual** — the AUR creates packages through its web form, not
  through a push. Submit `packaging/PKGBUILD` once at
  <https://aur.archlinux.org/submit>; the job then detects the missing remote, fails with
  that instruction, and succeeds on its next run.
- **Why the package is a binary package and why it is not statically linked** — a fully
  static binary cannot carry this product: the Vulkan loader (`libvulkan.so.1`) is a
  shared object that `dlopen`s the ICD, `libwayland-client`, `libxkbcommon`, `libX11`,
  `libXi` and `libxcb` are resolved at runtime, and ALSA loads its PCM plugins with
  `dlopen`. A static musl build would fail at the first of those. The package therefore
  declares the dynamic dependencies Arch already provides (`alsa-lib`,
  `vulkan-icd-loader`, `blas`, `cblas`).

## Verification quick reference

`samples/jfk.wav` is an 11-second 16 kHz mono PCM fixture (public-domain JFK
speech sample, same content as whisper.cpp's `samples/jfk.wav`) used by the
automated checks below.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -- --ignored                # overlay window-property smoke tests (needs a display server)
cargo run --release -- --diagnostics
cargo run --release -- --audio-file samples/jfk.wav --print-transcript --latency
LITTERA_OVERLAY=x11 cargo run --release   # exercise the GNOME overlay path locally
```

## License

MIT — see [`LICENSE`](LICENSE). The bundled fonts carry their own licenses
(`assets/Inter-LICENSE.txt`, `assets/BarlowCondensed-LICENSE.txt`); the speech model is
downloaded separately at first use and is governed by the terms of its distribution.
