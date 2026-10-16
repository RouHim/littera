# Feature Specification: In-App Model Catalog in the Model Section

**Created**: 2026-10-16
**Status**: Approved
**Input**: Display the transcribe.cpp model catalog (language and speed ratings) in the app's model section and enable downloading its models from there.

## Goal

The model section offers every model the bundled engine can actually run — the streaming-capable transcribe.cpp ASR variants (eight at the snapshot's time of writing, two of them with German) — with quality (WER) and speed (×realtime) ratings plus metadata, all served from a bundled, versioned catalog snapshot. Each offered variant is downloadable in-app through the existing verified, resumable, cancellable pipeline and becomes the active model after successful verification. Onboarding and the dictation pipeline itself stay unchanged. This extends the model-selection and download requirements of `.spec/wayland-voice-dictation.md` (FR-002, FR-003).

## User Scenarios

### Scenario 1 - Browse and compare offered models (P1)

A user opens the settings model section. The list shows every offered variant with name, languages, parameter count, licence, download size, and visual quality/speed ratings; exact values including reference machine and backend are reachable in an expandable detail. Each variant offers a quant selection with the recommended quant preselected.

**Acceptance**
1. Given the app has no network, When the user opens the model section, Then all offered variants appear with metadata, ratings (or "—" where a measurement is missing), and download sizes.
2. Given the list is shown, When the user expands a variant's detail, Then the exact WER values (dataset, language, quant) and the exact speed values (machine, backend, ×realtime) are visible.
3. Given the dictation language is `de-DE` or `en-US`, When the list renders, Then each card shows the WER published for that language, and falls back to the model's headline English result when the language value is not published.

### Scenario 2 - Filter and sort the list (P1)

The user narrows the list by language and maximum download size, and sorts by quality, speed, size, or name.

**Acceptance**
1. Given the model section, When the filter "Deutsch" is applied, Then exactly the variants supporting German remain listed.
2. Given a maximum download size of 1 GB, When the filter is applied, Then variants whose selected quant exceeds 1 GB are hidden and the rest remain.
3. Given the list, When sorted by speed, Then the fastest model is first and variants without a speed value sort last.

### Scenario 3 - Download, verify, and activate a model (P1)

The user picks a variant and quant and starts the download; the card shows progress, cancel and retry work, and the model becomes active only after its artifact is verified on disk.

**Acceptance**
1. Given a variant whose artifact is not on disk, When the user starts its download, Then progress is shown as received/total bytes and other cards' download actions are unavailable until it finishes.
2. Given a running download, When the user cancels it, Then the partial file is retained and a retry resumes from it instead of restarting.
3. Given a finished download whose byte length or SHA-256 does not match the pinned snapshot, When verification fails, Then the model is not activated and a retry is offered.
4. Given a verified artifact, When the download completes, Then the variant becomes the active model and the engine reloads on it.
5. Given the download fails (no network, server error), When the failure is reported, Then the card shows an error with a retry action and the rest of the list stays usable.

### Scenario 4 - Existing installations keep working (P2)

Installs that already use one of the three previously offered models keep their configuration and behaviour; the onboarding model step gains only a hint about the extended list in the settings.

**Acceptance**
1. Given a `config.toml` with `model = "nemotron-3.5-asr-streaming-0.6b-Q8_0"`, When the app starts after the update, Then that model is selected and no migration, re-download, or re-verification is triggered.
2. Given the onboarding model step, When it renders, Then it still shows exactly the three curated models plus a note that further models are available in the settings.

## Functional Requirements

- **FR-001**: The app ships a versioned catalog snapshot generated from the transcribe.cpp model catalog. Each offered variant carries: id, display name, family, parameter count, licence, supported languages, streaming capability, quants (file name, exact byte size, SHA-256, download URL pinned to a commit revision), accuracy references (dataset, language, quant, error rate), and speed references (machine, backend, ×realtime).
- **FR-002**: The model section lists exactly the variants that are runnable with the bundled engine — streaming-capable ASR variants that work with the app's language handling (`de-DE`, `en-US`, `auto`; language tags stripped before display, clipboard, or typing) — as one card per variant.
- **FR-003**: Each card shows name, supported languages including whether German is supported, parameter count, licence, the download size of the selected quant, the quality and speed indicators, and a quant selector with the recommended quant preselected.
- **FR-004**: The quality indicator shows the WER published for the configured dictation language (falling back to the model's headline English result; for `auto` the headline result is used). The speed indicator shows ×realtime on the reference machine (Ryzen 7 PRO 4750U) with the Vulkan backend; the CPU value is available in the detail. Both indicators are visual bars scaled relative to the best value among the currently listed models; the exact values, dataset names, machine, and backend are reachable in an expandable detail; a missing measurement renders as "—" with an empty bar and is never substituted.
- **FR-005**: Above the list: a text search over model and family names, a language filter (All / German / English), a maximum-download-size filter, and sorting by quality, speed, size, or name.
- **FR-006**: Downloads use the existing verified pipeline: resumable from the partial file, progress reporting, cancellation, retry, byte-length and SHA-256 verification against the snapshot, and a free-space check before starting. Exactly one download runs at a time; while one runs, other cards' download actions are unavailable.
- **FR-007**: A download that fails or is cancelled leaves the model unactivated and the list usable; the failure is shown on the card with a retry action.
- **FR-008**: Selecting a model is always possible; the model becomes active only after its artifact is verified on disk, at which point the engine reloads on it. The active model is persisted as its catalog id in the configuration, and the default stays `nemotron-3.5-asr-streaming-0.6b-Q8_0`.
- **FR-009**: The previously offered model ids remain valid and remain part of the list (the two Nemotron 3.5 quants appear as quant choices of their variant); only their labels/notes/badges adapt to the new card presentation.
- **FR-010**: The onboarding model step keeps its three curated models and adds only a hint that further models are available in the settings.
- **FR-011**: Models whose supported languages do not include the configured dictation language stay selectable; their languages are visible on the card and the language filter helps narrow the list. No model is greyed out or hidden because of the language setting.
- **FR-012**: List, ratings, and metadata are available without network access; only downloads require it.

## Key Entities

- **Catalog snapshot**: The versioned data set of offered variants and their ratings that ships with the app; changes to the upstream catalog become visible only through an app update.
- **Catalog variant**: One selectable model (e.g. Nemotron 3.5 ASR Streaming 0.6B) with metadata, supported languages, capability flags, quant artifacts, and rating references.
- **Quant artifact**: A concrete GGUF file of a variant with exact size, SHA-256, pinned URL, and download state (absent / downloading / verified / error).
- **Rating**: A published measurement value with its source (dataset + language for accuracy, machine + backend for speed), optional per quant.
- **Active model**: The catalog id persisted in the configuration that names the variant/quant currently loaded by the engine.

## Edge Cases

- No network: the list, ratings, and metadata render from the snapshot; every download action reports an actionable error with retry instead of hanging.
- Interrupted download: the partial file is retained, a retry resumes it; an artifact that fails verification is never activated and can be re-downloaded.
- Missing measurements: cards show "—" and an empty bar, never a guessed or interpolated value.
- Large artifacts (up to ~2.5 GB in the offered set): the free-space check runs before the download starts; progress is reported per file.
- Language mismatch: a model that does not support the configured language remains selectable and keeps working for the languages it does support.
- Models without language tags: the detected language for a dictation stays "unknown"; nothing tagged ever reaches the overlay, clipboard, or keyboard path.
- Snapshot coupling: the offered list matches the bundled engine version; new upstream variants require an app update.
- Concurrent actions: while one download runs, starting a second is impossible; cancelling and retrying the running one is always possible.

## Research Notes

- https://github.com/handy-computer/transcribe.cpp — catalog holds 76 variants across 21 families; capability flags mark 10 streaming variants, of which 8 transcribe (`de` + streaming: only Nemotron 3.5 ASR Streaming 0.6B and Voxtral Mini 4B Realtime); speed matrices cover `m4-max` and `ryzen-4750u` with Vulkan/CPU.
- https://github.com/handy-computer/transcribe.cpp/releases/latest (download `catalog.db`, 819 KB) — tables `models`, `downloads`, `accuracy`, `speed`, `capabilities`, `languages`; accuracy is per quant/dataset/language, speed per machine/backend/quant, but the database contains neither SHA-256 values nor download URLs.
- https://huggingface.co/api/models/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/tree/main — the Hub API exposes per-file `lfs.oid` (SHA-256) and size; verified identical to the app's currently pinned Nemotron Q8_0 hash, so snapshot checksums are pinned from there and URLs point at the publishing repo.
- https://huggingface.co/spaces/hf-audio/open_asr_leaderboard and https://artificialanalysis.ai/speech-to-text/streaming — independent quality/speed comparisons, but measured with transformers/H200 or cloud APIs; not the source for the in-app ratings, which must match the shipped runtime.

## Assumptions

- Speed reference is the Ryzen 7 PRO 4750U with the Vulkan backend (CPU value in the detail view); the reference machine is labelled on the card.
- Additional filters beyond language: maximum download size; sorting by quality, speed, size, name (delegated by the user as "other sensible filter options").
- Exactly one download at a time.
- Rating bars scale relative to the best listed model; exact values live in the detail view.
- The onboarding hint names the settings surface without a deep link.
- The snapshot pins size and SHA-256 for every offered variant/quant; the quant set mirrors the catalog with Q8_0 as the default.

## Success Criteria

- **SC-001**: The model section lists every streaming-capable ASR variant of the bundled snapshot (eight), each with metadata, ratings or "—", and a working download action — verified by inspection against the snapshot and a smoke run in the app.
- **SC-002**: With no network, the full list including ratings is visible, and every download action fails with an actionable message instead of hanging or crashing.
- **SC-003**: A corrupted or truncated artifact can never become the active model; activation happens only after the byte-length and SHA-256 check against the snapshot passes.
- **SC-004**: After cancelling a download, a retry resumes from the existing partial file and re-transfers less than the full artifact size.
- **SC-005**: The filter "Deutsch" leaves exactly the German-capable variants of the snapshot listed; filtering by maximum size and sorting by quality/speed/size/name reorder or reduce the list as specified, with unrated models sorting last.
- **SC-006**: An existing `config.toml` naming one of the three previous models starts without migration, re-download, or re-verification, and the three models remain offered with unchanged ids, sizes, and hashes.
- **SC-007**: Missing measurements render as "—" with an empty bar and are never substituted by a fabricated value.
- **SC-008**: While one download runs, starting a second is impossible; after it finishes or fails, other downloads start normally.
- **SC-009**: The onboarding model step still shows exactly three curated models plus the settings hint.
