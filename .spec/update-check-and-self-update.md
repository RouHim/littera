# Feature Specification: Update Check and Self-Update

**Created**: 2026-11-14
**Status**: Approved
**Input**: Tell the user about a newer release at startup, and let a binary Littera owns update itself in place.

## Goal

Once per daemon start Littera asks GitHub for the latest release and compares it with the running
version. When a newer release exists, the user learns about it in the existing transient overlay
notice. If the running binary is one Littera may honestly replace — user-writable, under `$HOME`,
not a package-managed file, not a cargo build artifact — Littera installs the published
`littera-x86_64` asset after verifying its SHA-256 and proving the new binary reports the expected
version, then swaps it in with an atomic rename. Every other installation is told the correct
upgrade command instead and is never written to. Nothing is persisted: no config key, no stamp
file, no tray item, no settings row. This extends `.spec/wayland-voice-dictation.md` (FR-014
notice vocabulary, FR-019 actionable text, FR-020 no `unwrap`/`expect`).

## User Scenarios

### Scenario 1 - A newer release exists (P1)

The daemon starts, asks GitHub once, and shows a transient notice naming the new version.

**Acceptance**
1. Given a newer release than the running version, When the daemon starts, Then a transient overlay notice names the new version and how to get it for this installation.
2. Given the release is not newer, When the daemon starts, Then nothing is shown.
3. Given the check fails (offline, TLS error, HTTP 403, malformed body), When the daemon starts, Then dictation is unaffected and only a log line records the failure.
4. Given the daemon runs in `--diagnostics` or `--audio-file` mode, When it runs, Then no update check is made.

### Scenario 2 - A binary Littera owns updates itself (P1)

For a binary the user installed themselves, the update is downloaded, verified and swapped in
without a restart prompt blocking anything.

**Acceptance**
1. Given a user-writable binary under `$HOME` outside a cargo `target/` directory, When a newer release exists, Then Littera downloads `littera-x86_64`, verifies its published SHA-256, proves the new file runs and reports the new version, and renames it over the running binary.
2. Given the download, digest or `--version` proof fails, When the update runs, Then the running binary is untouched, the temporary file is gone, and only a log line records the failure.
3. Given a successful swap, When the notice appears, Then it says the update applies on the next start; the running process keeps its current image.
4. Given the binary lives in a cargo `target/` directory, When a newer release exists, Then it is not replaced; the notice says to rebuild.

### Scenario 3 - A package-managed binary is never touched (P1)

**Acceptance**
1. Given `/usr/bin/littera` owned by pacman or dpkg, When a newer release exists, Then no file is written and the notice names the AUR helper or `apt` command.
2. Given a writable binary outside `$HOME` (or one whose parent directory is not writable), When a newer release exists, Then no file is written and the notice points at the release page.

## Functional Requirements

- **FR-023**: The daemon MUST check `https://api.github.com/repos/RouHim/littera/releases/latest` at most once per process start, only after onboarding is complete, off the boot path, with a 5 s timeout, `User-Agent: littera/<version>` and `Accept: application/vnd.github+json`. A failed check (offline, TLS, non-2xx, unparseable body) MUST leave the app unaffected and MUST be recorded only in the log. The CLI modes MUST NOT check.
- **FR-024**: A release counts only when its tag is `v<major>.<minor>.<patch>` and strictly newer than the running crate version; anything unparsable is ignored. Drafts and prereleases are excluded by the endpoint.
- **FR-025**: Littera MUST replace the running binary only when all of the following hold: no package manager owns it (`pacman -Qo`, `dpkg -S`), its path is under `$HOME`, no path component is `target`, and both the file and its parent directory are writable. Otherwise it MUST NOT write, and the notice MUST name the remedy: AUR helper, `apt`, release page, or rebuild from source respectively.
- **FR-026**: The update MUST use the release's exact `littera-x86_64` asset and verify it against the published `littera-x86_64.sha256` (bare digest) and, when present, GitHub's per-asset `sha256:` digest. The verified file MUST be made executable, MUST report the expected version through `littera --version`, and MUST replace the running binary by rename in the same directory. On any failure the old binary MUST remain and the temporary file MUST be removed.
- **FR-027**: The outcome MUST surface only as the existing transient overlay notice (8 s, non-interactive); no tray item, settings row, modal, persisted state or config key may be added. A successful in-place update MUST take effect on the next process start, without an automatic restart.
- **FR-028**: `littera --version` MUST print `littera <semver>` and exit 0, and the usage string MUST list it. The release pipeline MUST publish the standalone `littera-x86_64` binary and its `.sha256` next to the tarball.

## Key Entities

- **Release check**: The single GET to the latest-release endpoint and the comparison of its tag with `CARGO_PKG_VERSION`.
- **Ownership verdict**: The decision whether Littera may replace the running binary; a pure function of path, `$HOME`, writability and package-manager ownership.
- **Published binary asset**: `littera-x86_64`, the stripped baseline-x86-64 build, and its bare-digest checksum sibling.

## Edge Cases

- No newer release: no notice, no file access.
- Newer release but no `littera-x86_64` asset: the user is pointed at the release page; nothing is installed.
- A running daemon whose binary was replaced: the old image stays live until exit; the notice says the next start applies the update.
- Root running a package-managed binary: package ownership wins over writability; nothing is replaced.
- `$HOME` unset or unresolvable: treated as outside `$HOME`; notice only.
- Interrupted download: the temporary file is removed on the next attempt or left invisible as a dot-file; it is never executed or renamed.

## Assumptions

- The published binary is the same stripped, `TRANSCRIBE_X86_CONSERVATIVE=ON` build that the AUR
  tarball contains, so its runtime dependencies (ALSA, Vulkan loader, BLAS/cblas) match the
  documented Arch route.
- The GitHub release is the only update source; there is no channel, no prerelease track, and no
  separate update server.
- Keeping a downloaded binary for a later retry is not worth the state it would add.

## Success Criteria

- **SC-001**: On a machine with an older binary installed under `$HOME`, a daemon start installs the newer release, and the next start runs it (verified by `littera --version` and the changed file digest).
- **SC-002**: A pacman- or dpkg-owned binary is byte-identical after a start that found a newer release, and the notice names the package-manager command.
- **SC-003**: With no network, the daemon starts, dictates and logs exactly one failure without a notice.
- **SC-004**: A checksum-mismatched download leaves no temporary file and an unchanged binary.
- **SC-005**: `littera --version` prints the crate version and exits 0; `littera` with no arguments still runs the daemon.
