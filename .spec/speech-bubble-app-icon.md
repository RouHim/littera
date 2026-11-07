# Feature Specification: Speech-bubble app icon as a multi-size, desktop-integrated icon set

**Created**: 2026-10-16
**Status**: Approved
**Input**: Build a proper app icon for Littera from the provided speech-right SVG (speech bubble + sound waves).

## Goal

Littera currently ships one 64×64 PNG tile icon as its only visual identity. This feature replaces it with a proper app icon derived from the supplied source artwork: one committed master, one app-styled visual language, one icon set that stays legible everywhere the app appears — tray, dock, window/taskbar and app menus — from 16 px up to 256 px. The icon also becomes resolvable by name through the freedesktop icon-theme mechanism, so launchers show it outside the running window after a documented user-local install step. Runtime behaviour is unchanged: all runtime assets stay embedded in the binary and degrade to "no icon" instead of failing.

## User Scenarios

### Scenario 1 - Icon in every runtime context (P1)

The user sees the new icon wherever the running application appears: the StatusNotifierItem tray icon and the window/dock icon, consistently at every display density and panel background.

**Acceptance**
1. Given a tray host is available, When the application runs, Then the tray shows the new icon rendered from the embedded asset as a square ARGB32 pixmap with correct alpha, and no corrupt, blurry or wrong-aspect pixels appear.
2. Given the desktop is in dark theme, When the tray icon is displayed, Then the icon remains clearly visible against the dark panel because it does not rely on black or near-black fills.
3. Given a HiDPI display, When the tray renders the icon smaller than its native size, Then downscaling does not blur the glyph because the embedded asset is the largest available rendering.
4. Given the application window is open, When the user looks at the window/dock icon, Then it shows the same icon identity as the tray, on a headless-capable smoke path where the window icon cannot fail the daemon start.

### Scenario 2 - Icon in launchers and app menus (P2)

After a documented install step, the desktop environment resolves the icon by name outside the running process: app menus, docks and launcher entries show the icon instead of a generic fallback.

**Acceptance**
1. Given the documented install step was run, When the user searches for the application in the desktop's app menu, Then the entry is present and its icon renders from the installed icon set.
2. Given the installation was performed, When a desktop shell associates the application window with its desktop entry, Then the taskbar/dock icon comes from the same icon set.
3. Given the user's XDG data directories do not yet contain the target paths, When the install step runs, Then it creates the needed directories and succeeds.
4. Given the user opts not to install anything, When the application runs, Then nothing breaks: tray and window behaviour is fully covered by the embedded asset alone.

### Scenario 3 - Derivation of the artwork (P3)

The committed artwork is traceable to the supplied source and to one master: anyone can tell how every committed rendering was produced.

**Acceptance**
1. Given the repository, When a contributor opens the assets, Then the master SVG and every raster rendering live in one documented location and no second, unrelated icon family exists.
2. Given the derivation procedure, When it is run again from the master by a contributor, Then it reproduces the committed renderings without any build-time dependency being added to the project.
3. Given the original design source, When the icon is committed, Then it is visually reprocessed into the app's language (see Assumptions) rather than copied literally.

### Scenario 4 - Degraded icon paths (P2)

Icon problems never break the daemon.

**Acceptance**
1. Given the embedded icon cannot be decoded at runtime, When the tray becomes ready, Then a warning is logged and the application continues with the theme's fallback or no icon.
2. Given the desktop entry refers to an icon name that no installed theme provides, When the menu entry is rendered, Then the failure is a normal fallback rendering and never blocks the application.

## Functional Requirements

- **FR-001**: The icon MUST be derived from the supplied source artwork (`speech-right` SVG: speech bubble with sound waves) and re-created in the app's visual language: a tile-coloured background using the app's accent token with the motif in the app's on-accent/light token, optically centred with sufficient padding to stay legible at 16 px.
- **FR-002**: The committed master MUST be a single SVG source in the repository from which every other icon artifact is derived, and the derivation MUST be documented in one place (master file, produced artifacts, how to regenerate).
- **FR-003**: A complete raster set MUST exist under a single, consistent, lower-case icon name: square PNGs at 16, 24, 32, 48, 64, 128 and 256 px, non-premultiplied RGBA8 with alpha; the name MUST be a single lowercase token without spaces, commas or uppercase letters, matching the desktop entry's icon name and the application's executable identity.
- **FR-004**: A scalable vector rendering of the final artwork MUST be part of the installed set so high densities resolve without pixelation.
- **FR-005**: A single square RGBA8 PNG at ≥128×128 px MUST remain embedded in the binary and MUST be the source for the tray pixmap and the window icon; every runtime context MUST work without any installed files.
- **FR-006**: The previous single-size icon asset MUST be removed from the repository once the new set is in place; no duplicate or fallback copy of the old icon remains.
- **FR-007**: Icon rendering MUST fail soft: decode failure logs a warning and the application continues; no icon requirement may be part of the daemon's success condition.
- **FR-008**: A freedesktop desktop entry MUST be shipped with the repository, referencing the icon by its icon name (FR-003) with the standard window-association key set so running windows can be matched to the entry where the platform supports it; its command MUST launch the same executable the user runs today.
- **FR-009**: A documented one-command install step MUST place the desktop entry and the icon set under the user's XDG data directories in the freedesktop-conformant layout (`applications/` and `icons/hicolor/<size>/apps/`), create missing directories, and be safe to re-run; it MUST NOT require root, packaging, build-time dependencies or any new runtime dependency.
- **FR-010**: User-facing documentation MUST name the master asset, the icon set and the install step.

## Key Entities

- **Icon master**: the single committed SVG from which every rendering derives.
- **Rendered icon set**: the square PNG sizes plus the scalable vector rendering, under one icon name.
- **Embedded asset**: the binary-embedded PNG that backs the tray and window icon at runtime.
- **Desktop entry**: the `.desktop` file that gives the application its launcher identity, icon-name reference and window association.

## Edge Cases

- Legibility at 16 px: the motif must remain recognizable at the smallest size; fine detail that disappears below 32 px is not acceptable.
- Background independence: the artwork must not depend on brightness assumptions; a dark-on-dark or black-fill rendering is forbidden because dark panels and tray theming are the common case.
- Icon-name portability: uppercase letters or spaces in the icon name break theme lookup paths; a single lowercase token is required (FR-003).
- Absent target directories are normal, not an error: the install step creates them.
- Platform limits: when the desktop cannot associate a window with its entry, the launcher, menu and tray contexts (FR-004, FR-005, FR-009) MUST remain fully correct.
- The size change moves a pinned contract: the tray test currently asserts a 64×64 icon; it must follow the new embedded size rather than keep a stale literal.

## Research Notes

- <https://specifications.freedesktop.org/icon-theme/latest/index.html> — hicolor is the mandatory fallback theme; icons are looked up by name in `<size>/apps` directories; PNG is the recommended bitmap format and SVG support is optional. Implication: the icon set MUST live in the hicolor layout under one name, with PNGs for compatibility and SVG for scalability.
- <https://specifications.freedesktop.org/desktop-entry/latest-single/> — the desktop entry's `Icon=` value is a name resolved through the icon-theme algorithm, and user entries live under the user's data directory. Implication: a shipped `.desktop` plus a user-local install is the mechanism that surfaces the icon outside the process.
- <https://developer.gnome.org/hig/guidelines/app-icons.html> — app icons are drawn at 128×128, typically viewed at 64×64 and scaled to 32×32; excess detail is lost at small sizes. Implication: the master and the ≤64 px renderings must be checked for small-size legibility, not only for the large rendering.
- <https://specifications.freedesktop.org/status-notifier-item/latest/icon-pixmap.html> — the tray transmits an ARGB32 pixmap and panel hosts may scale it in either direction; a lingering 32-bit-Xwayland constraint (`BadMatch` on ARGB children) applies only to the overlay window, not the tray. Implication: the embedded tray asset must be large enough to survive downscaling and must keep clean alpha.
- Repo-internal findings: the icon is consumed in exactly two places — the tray decoder and the window-icon factory, both reading one embedded PNG through a single accessor, so a size change is a one-contract change; in the pinned iced/winit versions (iced 0.14.0, winit 0.30.13, checked in the local registry sources) no public API sets the window's application id or WM class, so window-to-entry matching relies on the platform's own mechanism and the install step stays user-local.

## Assumptions

- The supplied source SVG is a motif, not a layout: the committed icon adapts it to an accent-tile + light-glyph app language (consistent with the existing icon's tile colour and the app accent token) with adjusted framing and optical centering rather than literal copying.
- The deliverable is the three-layer set — master SVG, rendered PNG/vector icon set, embedded runtime asset — the desktop entry, and documentation; larger design deliverables (multiple variants, alternate colour schemes) are out of scope.
- User-local installation under the XDG data directories is the deployment form for now; system-wide installation, packages and distribution packaging remain out of scope.
- Raster derivation from the master is performed with an external SVG renderer available to the maintainer; the project itself gains no build-time rasterization dependency and commits the rendered PNGs.
- The rendering chain keeps binary expectations as today: square, non-premultiplied RGBA8/ARGB-compatible output, no interlacing, alpha preserved.
- The icon name token is aligned to the application's existing executable identity so that name-based lookups and window matching agree.

## Success Criteria

- **SC-001**: The icon appears, with identical visual identity, in the tray, the window/dock and the app menu after installing; no context shows a generic placeholder or the old placeholder artwork.
- **SC-002**: At 16 px the icon is recognizable without artifacts, and at 256 px it is free of pixelation and edge defects.
- **SC-003**: Every committed rendering derives from the single committed master via the documented procedure, and no derivation step requires adding a build-time dependency to the project.
- **SC-004**: On the supported desktops, the documented install step completes successfully as a non-root user, and the launcher entry's icon resolves by name without the running process being involved.
- **SC-005**: With the embedded asset deliberately corrupted, the daemon still starts, logs a warning and continues; no icon failure path is fatal.
- **SC-006**: The repository's existing gates — `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — pass with the new asset, and the tray icon contract test reflects the new embedded size rather than a stale literal.
