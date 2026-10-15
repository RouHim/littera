# Lingua — local voice dictation for Linux/Wayland

Press a global hotkey, speak, and the transcript lands at the cursor of the focused
application. Speech recognition runs **fully in-process** (Nemotron 3.5 ASR streaming
0.6B via [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp), static
Rust bindings, Vulkan with automatic CPU fallback). No account, no cloud, no separate
speech runtime.

Targets Wayland desktops: KDE Plasma, Hyprland (wlroots), COSMIC and GNOME — with
explicit, user-visible handling of each compositor's capability gaps.

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
  `lingua --diagnostics` and in the app's *Diagnostics* view.
- **No other platform glue**: no GTK/Qt shell (the GUI is iced), no
  `ydotool`/`xdotool`/`wtype`, no Python, no separately installed speech runtime, no
  cloud.

## Runtime dependencies

What the built binary needs on the machine. Everything below is checked by
`lingua --diagnostics` (hotkey access, paste permission, GPU backend, overlay backend)
and named again in the app's *Diagnostics* section (effective backend/device, clipboard
mechanism, microphone format, overlay backend).

### Libraries the binary links directly

`ldd target/release/lingua` on the reference machine prints exactly these:

| Library | Package |
| --- | --- |
| `libc.so.6`, `libm.so.6`, `libstdc++.so.6`, `libgcc_s.so.1` | `glibc`, `gcc-libs` |
| `libasound.so.2` | `alsa-lib` |
| `libvulkan.so.1` | `vulkan-icd-loader` |
| `libblas.so.3` | `blas` |
| `libcblas.so.3` | `cblas` |
| `libgfortran.so.5` | `libgfortran` (pulled in by `blas`) |

### Libraries loaded on demand (`dlopen`)

Not link-time dependencies, but the windowing stack resolves them at runtime. Measured
as the remaining `lib*.so[.N]` literals inside the binary
(`strings -a target/release/lingua | grep -oE 'lib[a-zA-Z0-9_+-]*\.so(\.[0-9]+)*'`):

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
| logind `uaccess` ACL on `/dev/input/event*`, or membership in the `input` group | reading the hotkey | the system check prints `sudo usermod -aG input $USER` + re-login |
| logind `uaccess` ACL on `/dev/uinput` | synthesizing the paste chord | clipboard-only mode; the overlay names the blocked path (FR-013) |
| XWayland | GNOME overlay and the X11 clipboard bridge | no overlay on GNOME, no X11 clipboard fallback |
| `ca-certificates` (system trust store) | HTTPS model download (rustls with the platform verifier) | the download cannot be verified |
| a compositor implementing `zwlr_data_control_v1`/`ext_data_control_v1`, or XWayland | owning the clipboard selection from the background | only the core `wl_data_device` path remains, which needs keyboard focus — logged as a warning |

Permissions, concretely:

1. **Hotkey listening.** Without a shortcut portal Lingua reads the keyboard devices
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
| C++ toolchain (`base-devel`: gcc, make) and `pkgconf` | transcribe.cpp is compiled through CMake; `alsa-sys` probes ALSA with `pkg-config` |
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
./target/release/lingua          # first run: onboarding (model, language, key, check)
```

The first dictation downloads the selected model on demand (751 MB for the default
Q8_0 variant) into `~/.local/share/lingua/models`, resumable and verified against the
published SHA-256 before it is loaded. The download can be cancelled in the model step
and resumed later.

`lingua` runs as a single instance: launching it again forwards "open settings" to the
running process and exits. A tray icon (StatusNotifierItem) shows the dictation state
and offers *Open settings* / *Quit*; without a tray host, relaunching the binary opens
the settings window.

### Command-line flags

```
lingua --diagnostics
lingua --audio-file <16-kHz-mono-PCM-WAV> [--print-transcript] [--latency]
```

- `--diagnostics` prints the four system checks (`hotkey access`, `paste permission`,
  `GPU backend`, `overlay backend`) with the remediation command for anything failing,
  then exits 0.
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
- The final transcript stays on the clipboard after insertion (no clipboard
  restoration), so `Ctrl+V` always works manually. The selection is owned by an
  in-process wayland client that picks the first available mechanism —
  `zwlr_data_control_v1`, then `ext_data_control_v1` (both let a background client own
  the selection without keyboard focus), then an **X11 `CLIPBOARD` owner bridged by
  Xwayland** (the mutter/GNOME-compatible route, since mutter validates the
  `wl_data_device` serial and a dictation daemon never holds focus), and finally the core
  `wl_data_device` with a logged warning. `copy_text` waits for the compositor's
  acknowledgement before the paste chord is synthesized, so a stale clipboard can never
  be pasted. (`smithay-clipboard` was not usable here: it only sets a selection while its
  own client holds keyboard focus. The diagnostics view names the active mechanism.)
- Settings are stored in `~/.config/lingua/config.toml` and apply to the next
  dictation without a restart.
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
- **Terminals** often paste with `Ctrl+Shift+V`; Lingua always sends `Ctrl+V`, so the
  clipboard path is the workaround.
- A **listen-only device listener cannot suppress desktop bindings**, so a chord like
  the default `Ctrl+Super` may additionally trigger a desktop action. Pick a chord your
  desktop does not use, or use a compositor with the shortcut portal (KDE, GNOME 48+,
  Hyprland), where Lingua binds through the compositor and nothing is read from the
  input devices.
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
- `assets/lingua-64.png` — the tray window icon, also embedded.

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
LINGUA_OVERLAY=x11 cargo run --release   # exercise the GNOME overlay path locally
```
