# Development

Operational reference for building, verifying and releasing Littera. The user-facing
document is [README.md](README.md); the repository rules, architecture and invariants are in
[AGENTS.md](AGENTS.md).

## Build from source

Arch (required packages before the first build):

```sh
sudo pacman -S --needed base-devel cmake vulkan-headers spirv-headers vulkan-icd-loader \
  shaderc alsa-lib blas cblas libxkbcommon libxcb
cargo build --release
./target/release/littera
```

`cmake ≥ 3.16`, a C/C++ compiler, `make`, `pkgconf`, `glslc` + `glslangValidator`, the Vulkan
headers and loader stub, SPIRV headers, ALSA headers and `blas`/`cblas` are build inputs.
`libxkbcommon` and `libxcb` are also needed at build/link time: `smithay-client-toolkit`'s
default features compile an xkbcommon binding through `pkg-config`, and `x11rb`'s
`allow-unsafe-code` links `libxcb`. `libclang`/bindgen, Python, OpenMP and `ydotool` are not
needed; the bindings are committed and the vendored C++ is linked statically.

The first build is slow: CMake builds `transcribe.cpp` + `ggml` (CPU and Vulkan) and about 200
`.comp` shaders. `GGML_NATIVE=ON` makes a local build host-CPU-tuned and **not portable**; the
release workflow passes `TRANSCRIBE_CMAKE_ARGS=-DTRANSCRIBE_X86_CONSERVATIVE=ON` so published
artifacts run on baseline x86-64.

There is no fully static build: the Vulkan loader `dlopen`s the ICD,
`libwayland-client`/`libxkbcommon`/`libX11`/`libXi`/`libxcb` are loaded at runtime by the
windowing stack, and ALSA `dlopen`s its PCM plugins.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -- --ignored                       # live smoke tests (self-skip without a session)
cargo run --release -- --diagnostics
cargo run --release -- --audio-file samples/jfk.wav --print-transcript --latency
LITTERA_OVERLAY=x11 cargo run --release        # X11/XWayland overlay path
```

The spec words the lint gate as `cargo clippy -- -D warnings`; the `--all-targets` form above
is the stricter, CI-used one. CI runs exactly these gates inside `archlinux:base-devel`, so a
green local run and a green CI run can still differ in compiler version — CI is authoritative.

The `#[ignore]`d tests cover the X11 window properties, layer-shell show/draw/hide, the
clipboard ownership/snapshot/restore flow (needs `wl-clipboard`), and the GitHub release
endpoint (needs the network). They are **not part of CI** (the runner has no compositor), so
run them locally before touching those paths. They all use the shared guards in `src/main.rs`
(`test_support`) and print why they skipped instead of passing or panicking silently.

## CLI surface

Exactly `--version`, `--diagnostics`, `--audio-file <16-kHz-mono-PCM-WAV>
[--print-transcript] [--latency]`; unknown args print usage and exit 2. `--audio-file` also
triggers the model download if the GGUF is missing. `--diagnostics` and `--audio-file` run
without iced.

## Environment variables

| Variable | Effect |
| --- | --- |
| `RUST_LOG` | `tracing` filter, default `littera=info`, output on stderr |
| `LITTERA_OVERLAY` | `layer` or `x11`: force an overlay backend |
| `LITTERA_HOTKEY_PORTAL` | `0`: skip the portal and force the evdev device listener |
| `XDG_RUNTIME_DIR` | required for the single-instance IPC socket |
| `DISPLAY` / `WAYLAND_DISPLAY` | session detection |

Build-time: `CBLAS_NO_LINK`, `TRANSCRIBE_DIR`, `TRANSCRIBE_CMAKE_ARGS`, `CMAKE_ARGS`,
`VULKAN_SDK`, `GGML_CCACHE=OFF`.

## Runtime linkage and permissions

The binary links `libasound`, `libc`, `libgcc_s`, `libm`, `libstdc++` and `libvulkan` (checked
with `readelf -d`). Wayland (`libwayland-client`), `libxkbcommon`, `libX11`, `libXi` and
`libxcb` are `dlopen`ed; the X11 ones only on the overlay/X11-clipboard fallback paths. The
The `.deb` declares the hard runtime set in `Depends` and the optional/XWayland pieces in
`Recommends`; the AUR recipe declares the same hard set, including `wayland` and
`libxkbcommon`.

| Capability | Mechanism | Permission needed | Degradation when missing |
| --- | --- | --- | --- |
| Paste (`Ctrl+V`) | `/dev/uinput` | logind `uaccess` or membership in `input` | clipboard-only mode, notice names the blocked path |
| Hotkey (device) | `/dev/input/event*` | read access (`input` group) | portal route only |
| Hotkey (portal) | `xdg-desktop-portal` `GlobalShortcuts` | installed `littera.desktop` + portal | device listener |
| Overlay | `zwlr_layer_shell_v1` | — | X11/XWayland window on GNOME, then no indicator |
| Tray | StatusNotifier host | — | no tray icon |
| Model download | HTTPS | network + free disk space | download fails with a message |

`--diagnostics` reports hotkey access, paste permission, GPU and overlay with remediation.

## Release pipeline

A push to `main` runs `.github/workflows/release.yml`:

1. **semantic-release** reads the Conventional PR titles, writes `CHANGELOG.md`, bumps
   `Cargo.toml`/`Cargo.lock` (`scripts/bump-version.sh`), tags `vX.Y.Z` and publishes the
   GitHub release.
2. **build** (Arch container) compiles the release tarball, the standalone binary and the
   `.deb`, and uploads the tarball assets.
3. **deb-verify** installs the `.deb` into pristine `ubuntu:24.04` and `debian:13` rootfs
   images, runs `littera --diagnostics`, then uploads the `.deb`.
4. **aur** renders `packaging/PKGBUILD` with the version, SHA-256 and glibc floor and pushes
   `littera-bin` to the AUR.

`.github/workflows/ci.yml` is the fmt/clippy/test gate; `pr-title.yml` enforces Conventional
PR titles. Actions are pinned to commit SHAs. The Ubuntu/Debian images are pinned by digest; the
Arch build container stays on its rolling tag so it picks up security updates. The Debian job
installs a checksum-verified `rustup-init` with a pinned toolchain.
