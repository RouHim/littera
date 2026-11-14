![Littera](assets/banner.png)

<p align="center">
    <a href="https://github.com/RouHim/littera/actions/workflows/ci.yml"><img src="https://github.com/RouHim/littera/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://github.com/RouHim/littera/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/RouHim/littera?label=release"></a>
    <a href="https://aur.archlinux.org/packages/littera-bin"><img alt="AUR version" src="https://img.shields.io/aur/version/littera-bin?label=AUR"></a>
    <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/RouHim/littera"></a>
</p>

<p align="center">
    <i>Voice dictation for Linux: press a hotkey, speak, and the text appears at your cursor.</i>
</p>

![Littera settings window with the model list, language filter and hotkey status](assets/screenshot.png)

Littera runs entirely on your machine, with no account and no cloud service.

## Requirements

- Linux with a Wayland session. KDE Plasma, GNOME 48+, Hyprland and COSMIC work. X11 sessions do not.
- A microphone that PipeWire or ALSA can open.
- A Vulkan GPU is recommended. Without one, Littera uses the CPU.

## Install

Arch Linux:

```sh
yay -S littera-bin    # or: paru -S littera-bin
littera
```

Debian 13+ and Ubuntu 24.04+ — grab the `.deb` from the [latest release](https://github.com/RouHim/littera/releases/latest); every asset ships a `.sha256`:

```sh
sudo apt install ./littera_<version>_amd64.deb
littera
```

The first start opens a setup window for model, language, hotkey and a system check. The default model supports 32 languages, is about 750 MB, and downloads on first use. Smaller English-only models are in the settings.

Any distribution — download `littera-<version>-x86_64.tar.gz` from the [latest release](https://github.com/RouHim/littera/releases/latest) and install it for your user:

```sh
tar xzf littera-<version>-x86_64.tar.gz
install -Dm755 littera-<version>-x86_64/littera ~/.local/bin/littera
install -Dm644 littera-<version>-x86_64/littera.desktop ~/.local/share/applications/littera.desktop
install -d ~/.local/share/icons
cp -R littera-<version>-x86_64/icons/hicolor ~/.local/share/icons/
~/.local/bin/littera
```

The desktop entry is what the global-shortcut portal identifies Littera by (`Exec=littera` resolves on `PATH`, which includes `~/.local/bin` in a login shell), so install it even from a checkout. This route carries the same runtime dependencies as the Arch package. A binary installed under your home directory updates itself — see [Updates](#updates).

To build from source (Rust 1.98 or newer):

```sh
sudo pacman -S --needed base-devel cmake vulkan-headers spirv-headers vulkan-icd-loader \
  shaderc alsa-lib blas cblas libxkbcommon libxcb
cargo build --release
./target/release/littera
```

Build, test and release details are in [DEVELOPMENT.md](DEVELOPMENT.md).

A build run straight from `target/release` is never replaced by the updater; rebuild it when a release lands. A copy installed under `~/.local/bin` is a Littera-owned install, so the next release replaces it in place. To give the checkout build an app-menu entry and the portal hotkey route, install the entry and icons as above (including the `install -d` for the icons directory), pointing `install` at `target/release/littera`.

## Use

- Tap the hotkey (default Ctrl+Super) to start recording. Tap again to stop.
- Or hold the hotkey while you speak, then release.
- The transcript is pasted at your cursor. Terminals often need Ctrl+Shift+V instead.
- The tray icon opens the settings. Change the hotkey, language or model there. Changes apply immediately.

## Configuration

The settings window writes `~/.config/littera/config.toml`; the same values can be edited by
hand. Every key is optional and an absent key keeps its default.

| Key | Values | Default |
| --- | --- | --- |
| `onboarding_complete` | `true`/`false` | `false` until setup finishes |
| `model` | a catalog id (for example `nemotron-3.5-asr-streaming-0.6b-Q8_0`) | the multilingual Q8_0 variant |
| `language` | `auto` or a locale from the catalog's language table (32 languages) | `auto`; an unknown token falls back to `auto` |
| `hotkey` | a chord such as `CTRL+SUPER` | `CTRL+SUPER` |
| `hotkey_source` | `portal`, `device` | omitted while default (automatic) |
| `backend` | `auto`, `vulkan`, `cpu` | `auto` |
| `input_device` | `system` or `<host>:<device>` | `system`; omitted while default |
| `idle_unload` | `never`, `1`, `5`, `15`, `30`, `60` (minutes) | `15`; an unknown value falls back to `15` |

## Updates

Littera asks GitHub once per start whether a newer release exists and shows the answer in the overlay notice. That check is the only automatic network access: one request per start, plus the verified update download when Littera owns the binary. Model downloads are always user-initiated, and nothing is stored about the check.

- A binary you installed yourself with the tarball route is replaced in place: the published SHA-256 must match and the new file must report the expected version before it is renamed over the running one. It applies on the next start.
- A pacman- or dpkg-owned binary is never written to. The notice names how to get the release — `yay -Syu` / `paru -Syu` for the AUR package, and the release-page `littera_<version>_amd64.deb` for the `.deb` (`sudo apt install ./littera_<version>_amd64.deb`).
- A build run from `target/release` is left alone; rebuild with `cargo build --release`. A copy under `~/.local/bin` is updated like any other install Littera owns.

## Troubleshooting

```sh
littera --diagnostics
```

The check covers hotkey access, paste permission, GPU and overlay. Each failure comes with a fix. Littera also opens a problem window at startup when something is wrong.

- Hotkey does nothing: for a source install, install the desktop entry as described under Install and restart Littera. If the diagnostics ask for it, run `sudo usermod -aG input $USER`, then log in again.
- Nothing is pasted: without write access to `/dev/uinput`, Littera copies the transcript instead of typing it. Paste it by hand. The diagnostics name the blocked path.
- Your files: config in `~/.config/littera/config.toml` (see [Configuration](#configuration)), models in `~/.local/share/littera/models`.
- Bugs and questions: [GitHub issues](https://github.com/RouHim/littera/issues).
