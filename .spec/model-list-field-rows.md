# Feature Specification: Compact Model List — Two-Line Field Rows

**Created**: 2026-10-17
**Status**: Approved
**Input**: The model list in the settings window is a wall of tall cards — one has to scroll far past it to reach the sections below. Replace the cards with compact two-line field rows that carry the same facts and actions.

## Goal

At the shipped 900 px width one variant card costs ≈ 250 px (measured: the Voxtral card spans y 262→513 in `.impeccable/review/settings-bottom-1280x1515.png`), so the eight variants take ≈ 2000 px — about 60 % of the settings page — and every visit scrolls past them. This feature renders each variant as a **two-line field row (≈ 80 px, measured 70–82)**: the facts and actions a user decides between stay in the row (name, size, state and its action, the language summary, the quant choice, the quality/speed comparison, the language warning), while the catalog record and the published detail (parameter count, license, exact values, datasets, reference machine, file/SHA/URL) expand in place below the row. The list stays plain page content, so a window keeps exactly one scroll surface — the finish review's nested-scroller finding stays closed.

Scope boundary: the model list's row composition and its display order. Filters, sorting, search, download/verification logic, the catalog snapshot and the detail content itself stay as they are.

## User Scenarios

### Scenario 1 - Switching models quickly (P1)

The user opens Settings mid-work to change the model and wants their current choice in reach without hunting through the list.

**Acceptance**
1. Given the settings window opens with any sort order active, When the model list is rendered, Then the active model's row is the first row, visually marked as active, and does not appear a second time among the sorted rows.
2. Given the user clicks another row's radio, When the selection applies, Then the config is written as today and the newly active row is shown first on the next render.

### Scenario 2 - Comparing variants in place (P1)

The user weighs quality against speed and size across the listed variants.

**Acceptance**
1. Given the default window, When the list is rendered, Then every listed variant shows its name, download size, quality and speed bars, its offered quants and its state without expanding anything.
2. Given two variants with published measurements, When the list shows them, Then their bars are scaled against the best listed variant, exactly as today (base spec SC-007: an unmeasured quant shows `—`, never a bar).

### Scenario 3 - Managing a download from the row (P1)

The user downloads, watches, cancels or retries a model without leaving the list.

**Acceptance**
1. Given a variant whose artifact is not downloaded, When its row is rendered, Then the state is stated and a `Download` action sits in the row.
2. Given a download in flight, When its row is rendered, Then it shows the progress bar with a percentage and a `Cancel` action, and no other row starts a second download (base spec SC-008).
3. Given a failed download, When its row is rendered, Then the failure reason and a `Retry` action are shown, together with the existing resume hint.
4. Given a verified or on-disk artifact, When its row is rendered, Then it offers `Verify`, as today.

### Scenario 4 - Reading the published detail (P2)

The user wants the exact numbers behind a bar.

**Acceptance**
1. Given any row, When the user activates its `Details` control, Then that row's detail block expands directly beneath it, in page flow, without a modal or a second surface.
2. Given one row expanded, When another row's `Details` is activated, Then the first row collapses (one expanded row at a time, as today).
3. Given an expanded row, When the page is scrolled, Then the detail scrolls with its row (no inner scrolling).

### Scenario 5 - A variant the configured language cannot use (P2)

The user has `de-DE` configured and looks at an English-only variant.

**Acceptance**
1. Given a listed variant whose catalog entries do not serve the configured language, When its row is rendered, Then a warning marker is visible in the row **without** expanding it, and the row stays selectable (base spec FR-011).
2. Given the marker is visible, When the row's detail is expanded, Then the full warning text is shown there as well.

## Functional Requirements

- **FR-001**: Each listed variant MUST render as one two-line field row: line 1 carries the selection radio and the variant label, right-aligned the download size (mono), the state lamp and the state's control; line 2 carries the metadata line, the quant chips, the quality/speed indicators and — when applicable — the language-warning marker, right-aligned the state's action control and the `Details` control.
- **FR-002**: The active model's row MUST be the list's first row and MUST NOT be repeated among the sorted rows that follow.
- **FR-003**: The list MUST remain plain page content: no inner scrollable, no fixed cap, no pagination, no modal — a window has exactly one scroll surface.
- **FR-004**: Every state of the existing state vocabulary (`ui::model_state`) MUST be reachable and operable from the row, stated by its lamp and its control, not by words: not downloaded (warning lamp + download), downloading (lamp + progress + percentage + cancel), verifying (lamp + cancel), on disk (hollow lamp + tick), verified (run-green lamp + tick), failed (fault lamp + reason + retry + resume hint).
- **FR-005**: A language warning MUST be visible in the row without expanding it, and MUST NOT block selecting the variant; the full text remains in the detail block.
- **FR-006**: Quality and speed MUST stay rendered as indicators relative to the best listed variant, and a quant without a published measurement MUST show the dash, never a bar (base spec SC-007). Both indicators MUST hold one slot anchored to the row's right edge, so their bars stand on the same axis in every row regardless of what precedes them.
- **FR-007**: `Details` MUST expand exactly one row at a time, in place, in page flow; the block carries the published values as today plus the catalog facts the row leaves out (the parameter count and the license).
- **FR-008**: The row MUST hold at the minimum window size (620×480): the label and the metadata line give up width first and clip within their allotted space rather than pushing the state tag, the action or the `Details` control out of the row; no control may become unreachable.
- **FR-009**: The metadata line MUST carry the variant's language summary only, and the quant chips MUST offer every quant with the selected one marked. The parameter count and the license are catalog facts, not scan fields: they MUST be shown in the detail block (with the exact published values), never in the row.
- **FR-010**: Search, filters and sorting MUST keep their semantics; the pinned active row precedes the sorted remainder regardless of the sort order.
- **FR-011**: Selecting a variant, choosing a quant, starting/cancelling/retrying a download and toggling the detail MUST work as today, driven by the same messages; the row introduces no new interaction pattern.
- **FR-012**: Controls whose meaning carries itself MUST be drawn marks instead of words: the disclosure chevron, the download arrow, the verify tick, the cancel cross and the retry cycle (`src/ui_icons.rs`). Every icon-only control MUST keep its word in a tooltip. A row's state MUST be stated by its lamp (solid in the state's tone, the hollow ring for on-disk-unchecked) together with the control's mark — never by a caption in the row; the word waits in the tooltip.
- **FR-013**: The marks MUST be drawn in code with iced's canvas on a fixed grid (1.5-unit strokes, 18px box), inked in the palette's role colours and dimmed when their control is unavailable — never font glyphs, emoji, a framework icon set or bitmap assets.
- **FR-014**: The change MUST use only the existing design tokens and the embedded faces; the only new build input is iced's canvas feature (the vector geometry layer the drawn marks need). No new runtime dependency, no new asset, no second typeface.

## Key Entities

- **Field row**: the two-line rendering of one catalog selection (variant + artifact): line 1 identity and state, line 2 facts and actions.
- **Pinned row**: the active model's row, rendered before the sorted remainder.
- **Warning marker**: the compact, warning-toned statement that the variant does not serve the configured language.
- **Detail block**: the existing expansion carrying exact published values, datasets, reference machine, backend, file name, size, SHA-256 and URL, plus the catalog facts the row leaves out (parameter count, license).

## Edge Cases

- Long labels ("Voxtral Mini 4B Realtime (2602)", "Multitalker Parakeet Streaming 0.6B v1") clip inside the row; the full label stays readable in the detail block.
- A variant offering four quants (Nemotron 3.5) must not push line 2 past the row width at 620 px.
- A download in flight while the user changes filters: the row follows the filter like any other; the transfer is unaffected.
- The active model is also the only match of the current filter: it renders once (pinned), not twice.
- A filter hides the expanded row: nothing renders the expanded detail until that row is visible again; the expansion state is unchanged behavior.
- The loading state (capability checks not yet arrived) keeps its skeleton; the empty filter result keeps its banner.
- An unmeasured quant in the pinned row behaves like any other row (dash, no bar).
- A future catalog with more variants grows the page linearly — rows are deliberately uncapped; the fix for length is row height, not hiding rows.

## Research Notes

- In-repo precedent: the Microphone section already renders one-line option rows (label + technical identifier + note) for a comparable pick-list, so a compact row is this surface's established idiom, not a new one — the model list was the outlier.
- The direction contract (`.impeccable/surfaces/settings.md`, FIRST VIEWPORT) already promises "the model list as field rows (name, quant and size in mono, state stamp, one action)"; the finish review recorded the card rendering as an adaptation of that line. This feature aligns the build with the recorded contract.
- The finish review's nested-scroller finding (`DISPOSITION: ship` round, 2026-10-17) is satisfied by keeping the list in page flow — reintroducing a capped inner scroll area would reopen it.
- Measured baseline: one card ≈ 251 px at 900 px width (`.impeccable/review/settings-bottom-1280x1515.png`), eight variants ≈ 2000 px of a ≈ 3300 px page.

## Assumptions

- The list's content stays the eight bundled streaming variants; no pagination, no "show all" toggle, no separate model screen.
- Pinning is a display order only; it introduces no second selection mechanism and does not change sorting semantics for the remainder.
- The detail block's content and the download machinery are untouched; the row only re-lays-out their triggers and status.
- Window sizes are unchanged (default 900×720, minimum 620×480).
- Design documentation (DESIGN.md / `.impeccable/design.json`, the surface brief) and AGENTS.md are updated in the same change if they describe the card rendering.

## Success Criteria

- **SC-001**: With the default window and all eight variants listed, the model list is at most 700 px tall in total (baseline ≈ 2000 px), verified from a captured screenshot.
- **SC-002**: Exactly one scroll surface exists per window at 900×720 and at 620×480, verified from captures (no second scrollbar, no inner thumb).
- **SC-003**: For every listed variant, name, size, the state's lamp and control, the metadata line, the quant selection and both indicators are readable without expanding, verified from a capture at 900×720.
- **SC-004**: A variant the configured language cannot serve shows its warning marker without expanding, verified from a capture with a mismatched language configured (base spec FR-011).
- **SC-005**: The six download states render with their action as specified in FR-004; each state's rendering is verified against a capture or the state table in the test module.
- **SC-006**: Selecting a variant from a row writes the config and swaps the pinned row, verified by the existing selection path (no new interaction).
