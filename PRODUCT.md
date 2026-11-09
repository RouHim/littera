# Product

<!-- impeccable:product-schema 1 -->

## Platform

linux-desktop

## Users

Linux desktop users running a Wayland session on one of the four desktop
families this MVP covers — KDE Plasma, Hyprland (wlroots), COSMIC and
GNOME 48+ — who want to dictate into whatever application currently has
keyboard focus: email, chat, editors, notes, terminals. The product aims at
broad adoption, so a stranger must be able to install it, complete onboarding,
pass the permission walls it names, and reach a first successful dictation
without terminal archaeology. English (`en-US`) and German (`de-DE`) speech are
first-class supported languages; `auto` detection is available. The reference
machine — COSMIC 1.9.0 on Wayland, AMD Radeon RX 6950 XT (Mesa RADV),
PipeWire, de-DE/en-US — is the measuring stick for latency and capability
behavior.

## Product Purpose

Littera is a privacy-first voice dictation application for Linux/Wayland: press
a global hotkey, speak, and the transcript is inserted at the cursor of the
focused application while streaming partials appear on a bottom-centre overlay.
Recognition runs entirely in the application's own process — no cloud, no
account, no separately installed speech runtime, and no network use beyond the
initial model download. Success means end-to-end dictation on the reference
machine with first partials within a second of speech onset and the final
insert within 1.5 s of the stop gesture, correct German and English output
including umlauts and ß, and every capability the platform does not grant by
default (hotkey access, paste permission, GPU backend, overlay backend)
surfacing as an explicit, actionable state rather than a silent failure.

## Positioning

An OpenWhispr-style dictation experience specialized for Linux/Wayland, with
the cloud and the external machinery removed. The speech-to-text engine is
statically linked into the binary and driven in-process (Vulkan with an
explicit CPU fallback), the paste chord is synthesized in-process through
`/dev/uinput` (no ydotool/ydotool daemon), and clipboard ownership is
implemented directly over Wayland data-control protocols (no helper binary).
The result is one binary with no daemons and no accounts — a shape the
reference product (cloud/BYOK, external runtime) and batch-only whisper
tooling cannot truthfully claim.

## Operating Context

- Wayland session on KDE Plasma / Hyprland / COSMIC / GNOME 48+; the
  `GlobalShortcuts` XDG portal where available, logind-`uaccess` ACLs on
  `/dev/uinput` (paste injection) and `/dev/input/event*` (device-listener
  hotkey) where not.
- Microphone capture from the system default device (PipeWire/ALSA through
  cpal, resampled to 16 kHz mono in-process).
- First use downloads a GGUF model from Hugging Face (default Nemotron 3.5 ASR
  Streaming Q8_0, ≈751 MB) into the XDG data dir — SHA-256-verified, resumable,
  with a free-space gate. Settings live in the XDG config dir; single instance
  via an `$XDG_RUNTIME_DIR` socket.
- Day-to-day interaction is the hotkey (default `Ctrl+Super`) plus an optional
  StatusNotifier tray; the settings window opens only when the user asks for it.
- Terminal-only verification paths that exist: `--diagnostics`, and
  `--audio-file samples/jfk.wav --print-transcript --latency`.

## Capabilities and Constraints

- Onboarding (model, language, key binding, system check) with persisted
  completion; model selector with size and quality notes; language `auto`,
  `en-US`, `de-DE`; hotkey rebindable live; tap-to-toggle and hold-to-talk
  separated by a minimum-hold threshold; 5-minute recording cap; exactly one
  insertion per dictation; the text selection that a paste displaces is put
  back afterwards, so a dictation does not eat the clipboard (clipboard-only
  mode keeps the transcript).
- Streaming recognition with partials on the overlay; overlay pinned bottom
  centre on layer-shell compositors, a plain non-activating window on GNOME;
  it never takes keyboard focus and reserves no layout space.
- Clipboard-only degradation with named remediation when `/dev/uinput` is not
  writable; system-check surface with exact fix commands and re-check; startup
  notice when a check fails.
- Constraints: Linux + a Wayland session only. No X11 session, no
  macOS/Windows/BSD. X11 appears only as an XWayland fallback inside a Wayland
  session, in exactly two places: the GNOME overlay window and the X11
  `CLIPBOARD` bridge. UI copy is English-only. Paste is `Ctrl+V` only
  (terminals: documented limitation).
- Out of scope by spec: cloud/BYOK models, transcript history, custom
  dictionaries, meeting transcription, accounts.
- The UI is a native desktop interface (Rust + iced, rendered by the
  application itself; no browser, DOM, or CSS): there is no web surface to
  scan or browse. `linux-desktop` is recorded truthfully as the platform;
  Impeccable's schema has no Linux-desktop value, so the engine falls back to
  `web` treatment with a warning.

## Brand Commitments

- Name: Littera.
- Assets on hand and embedded: `assets/icon/hicolor/256x256/apps/littera.png`
  (icon; master `assets/icon/hicolor/scalable/apps/littera.svg`, tray + window)
  and the design language's one face — `assets/AtkinsonHyperlegible-Regular.ttf`
  and `assets/AtkinsonHyperlegible-Bold.ttf`, SIL OFL 1.1 with the license text
  alongside.
- Voice: user-visible failure text is actionable — cause and remedy, never
  silent; internal logs via `tracing`, never `println!`. No marketing copy
  exists and none is invented.
- Interaction identity: non-intrusive by contract — never steals focus, the
  overlay exists only while recording or transcribing, and a second launch
  merely opens settings in the running instance.

## Evidence on Hand

- `.spec/wayland-voice-dictation.md` — the approved spec: Scenarios,
  FR-001…FR-022, SC-001…SC-010. The requirements authority for this product.
- `DESIGN.md` + `.impeccable/design.json` — the built visual system ("The First
  Reader"), written from the shipped code; `.impeccable/surfaces/settings.md`
  holds the chosen direction contract.
- `README.md` — operations reference: build/runtime deps, permission matrix,
  flags, behavior, verification commands.
- `samples/jfk.wav` — an 11 s, 16 kHz mono fixture used by the `--audio-file`
  path.
- `src/model_catalog.rs` — the bundled, versioned catalog snapshot with real
  sizes, SHA-256 digests and commit-pinned download URLs.
- Absent, and not to be fabricated by future work: website, landing page,
  published screenshots or benchmarks, testimonials, pricing, telemetry
  claims, and any packaging/release process (private repo, `publish = false`,
  no CI, no `.desktop` file, no systemd unit).

## Product Principles

1. **Everything local.** Recognition runs in-process from the application's
   own binary; no cloud, no account, no separately installed runtime; the only
   network use is verified model downloads.
2. **Degrade visibly, never silently.** Any capability the platform withholds
   gets an explicit state, the blocked path, and the exact command that fixes
   it; the application stays usable except for the affected capability.
3. **Wayland-first, honest about compositors.** Probe capabilities at runtime;
   prefer the portal over device listening, layer surfaces over plain windows,
   data-control over the XWayland bridge — and document the fallback instead
   of assuming a protocol.
4. **Stay out of the way.** No keyboard-focus capture, no layout reservation,
   no notifications substituting for the overlay, no tray requirement, no
   daemons or background services beyond the single process itself.
5. **One binary, boring operations.** Static engine link, embedded assets,
   in-process keystroke injection: what "install" means is copying one file.
