# Feature Specification: Model Load Feedback and Idle Model Unload

**Created**: 2026-10-16
**Status**: Approved
**Input**: Report the model load to the GUI, and add a setting that unloads the model after a period of non-use so idle memory usage drops (brief, condensed).

## Goal

Today the daemon loads the ASR model silently and then holds it in memory for the whole process lifetime: the user gets no signal while a load or reload is running, and no memory is released during idle periods. This feature makes the model's load lifecycle visible in the GUI (loading, resident with the active backend, unloaded, failed) and adds a user-configurable idle-unload timeout after which the model is released. A dictation that needs a non-resident model keeps recording and completes transparently once the model is back. Scope boundary: the daemon's resident-model lifecycle and its GUI reporting; downloads/verification, the CLI paths, and every other way a model gets loaded stay as they are.

## User Scenarios

### Scenario 1 - Model load state is visible in the GUI (P1)

While the model is being loaded (daemon start with the model on disk, model switch, or right after a download), the user opens the settings window and sees that the model is loading, then sees the actual backend/device once it is ready; a failure shows its reason instead of a stale success line.

**Acceptance**
1. Given the model file exists and the app has just started (or the user switched models or finished a download), When the model is loading and the settings window is open, Then the model section shows an indeterminate loading indication naming the model, and no percentage.
2. Given a load finishes successfully, When the engine reports ready, Then the model section shows the active backend and device, and a CPU fallback still carries its warning.
3. Given a load fails, When the engine reports the failure, Then the model section shows the failure reason and never presents the previous load's backend as the current state.

### Scenario 2 - Idle unload frees memory; the next dictation reloads transparently (P1)

After a dictation the user stops using the app; once the configured timeout has passed the model is released, and the next dictation reloads it without any extra gesture or lost audio.

**Acceptance**
1. Given the user dictated once and then does nothing for longer than the configured timeout, When the timeout elapses, Then the model is released and the model section shows the not-resident state, naming the model and stating that the next dictation reloads it.
2. Given the model is not resident, When the user starts a dictation, Then recording starts immediately, the on-screen indicator shows that the model is loading, and after the load finishes the complete transcript of that dictation is inserted.
3. Given an automatic unload happened, When no dictation follows, Then the app does not reload the model on its own.

### Scenario 3 - Configuring the idle timeout (P2)

The user changes the new setting and the change is persisted and effective immediately.

**Acceptance**
1. Given the settings window, When the user selects Never, Then the model stays resident for the rest of the session and the choice survives a restart.
2. Given the model is resident, When the user selects a different preset (e.g. 5 minutes), Then the config file is updated immediately and the new timeout applies without restarting the app.
3. Given the model has already been idle longer than a newly selected shorter timeout, When the selection is made, Then the unload happens promptly without a restart.

## Functional Requirements

- **FR-001**: The app MUST expose the selected model's load lifecycle to the GUI with at least these states: loading, resident (with the active backend and device), not resident (unloaded), failed (with reason).
- **FR-002**: While a load or reload of the selected model is in flight, the GUI MUST show an indeterminate loading indication that names the model, and MUST NOT display a previous load's backend or error as if it were the current state.
- **FR-003**: The loading indication MUST NOT show fabricated progress; no percentage is shown while the model is loaded into memory.
- **FR-004**: When a dictation starts while the selected model is on disk but not resident, the app MUST record normally, keep the captured audio, show the loading indication on the on-screen indicator, and complete the dictation (insert the transcript) once the model is resident — without requiring a new hotkey gesture.
- **FR-005**: "Downloaded but not resident" MUST NOT be treated as "not downloaded": the hotkey path records and reloads instead of triggering a download.
- **FR-006**: The app MUST provide a persisted setting for the model idle-unload timeout with the values Never, 1, 5, 15, 30 and 60 minutes; the default MUST be 15 minutes.
- **FR-007**: A setting change MUST be saved immediately and MUST take effect without restarting the app, consistent with the existing autosave behavior.
- **FR-008**: The idle countdown MUST be reset when a dictation ends; a model that is resident but has not served a dictation MUST count as idle from the moment it became resident.
- **FR-009**: Automatic unload MUST happen only when the engine is idle — no active or queued dictation, no transcript still in flight, no download, verification or load running — and MUST otherwise be deferred until these conditions hold.
- **FR-010**: The countdown MUST run while a model is resident, regardless of window visibility or user interaction.
- **FR-011**: Automatic unload MUST release the memory the model occupies (host RAM and, on GPU runs, VRAM) and MUST leave the app fully operational (hotkey, tray, IPC socket, settings) and able to reload later.
- **FR-012**: After an automatic unload, the GUI MUST show the not-resident state, including that the next dictation reloads the model; the previously reported backend/device MUST NOT be shown as current.
- **FR-013**: With the setting at Never, no automatic unload MUST occur; the model stays resident until app exit or until it is replaced by a model switch.
- **FR-014**: After an automatic unload the app MUST NOT reload the model until it is next needed (a dictation, a completed download, or a model switch).
- **FR-015**: Replacing the model (settings switch or onboarding finish) MUST release the previously loaded model's memory as today; the newly loaded model MUST follow the same idle rules.
- **FR-016**: A reload failure MUST surface through the existing engine failure handling (dictation discarded with a visible reason) and MUST leave the app able to retry on the next use.

## Key Entities

- **Model residency state**: the selected model's in-memory status — loading, resident (backend, device, optional CPU-fallback reason), not resident, failed (reason) — plus the time of the last dictation that used it; distinct from the artifact's download state.
- **Idle-unload setting**: one global value — Never, 1, 5, 15, 30 or 60 minutes, default 15 — persisted in the config file.
- **Model artifact**: the existing catalog entry (id, size, SHA-256); "on disk" and "resident" are independent, and only the latter consumes memory.

## Edge Cases

- A hotkey press lands exactly at the unload deadline or during a reload: no audio may be lost; the engine's existing buffering covers this, so the worst case is a slightly later transcript, never a dropped or truncated dictation.
- A too-short gesture while a reload is starting is discarded as today (nothing to transcribe), and the engine is released again so no stream stays open.
- A download or verification is running: an automatic unload is not attempted; once the completed download has been loaded, the idle countdown starts fresh.
- The setting is switched to a shorter timeout after the model has already been idle beyond it: the unload happens promptly without a restart.
- Pure idleness (no dictation at all): the model is unloaded once and no load/unload churn follows.
- On Vulkan the model can occupy host RAM, VRAM or both; the release must be observable wherever it resides.
- App exit while the model is resident is unchanged: process teardown releases the model.
- CLI paths (`--audio-file`, `--diagnostics`) are unaffected; there is no resident daemon and no idle behavior.
- Onboarding reuses the same state card and therefore shows the loading/resident/not-resident states consistently.

## Research Notes

- https://github.com/ollama/ollama/blob/main/docs/faq.mdx — Ollama keeps a model in memory 5 minutes by default and exposes the duration as a configurable keep-alive; validates a timeout-based unload with a user-facing default.
- https://github.com/ggml-org/llama.cpp/discussions/18939 — llama-server's `--sleep-idle-seconds` unloads a model after a configurable idle period; precedent for a small fixed set of durations plus an off switch.
- https://localai.io/docs/features/runtime-settings/ — LocalAI's watchdog settings unload idle models; the timeout + Never pattern is established practice for local inference.
- https://github.com/Beingpax/VoiceInk/issues/595 — reports slow first transcription when a model has been paged out after idle; motivates a transparent, buffered reload and a 15-minute default above Ollama's 5 minutes.

## Assumptions

- The existing eager startup preload stays: when the model file exists, the daemon loads it at start (now visibly), and the idle countdown covers a model that never served a dictation.
- The setting lives in the settings window in its own section right below the model list ("Unload idle model"), using the existing selection vocabulary (like the language choice); one global value, not per model.
- Unloading releases the model (and its session/stream state) only; the engine thread, hotkey, tray, IPC and clipboard keep running. App-exit behavior is unchanged.
- Scope is the daemon; the CLI paths stay unchanged, and the onboarding model card reuses the same states.
- Reload has no latency budget; success is transparency — full capture, transcript after the load, no extra gesture.
- The 1-minute preset exists for easy manual verification of the behavior.

## Success Criteria

- **SC-001**: With the default timeout, after the last dictation the model is released within 30 seconds of the timeout elapsing, and the process's resident memory (plus VRAM on GPU runs) drops back to within ~100 MB of the level measured before the model was first loaded.
- **SC-002**: With Never, the model stays resident through at least two times the default timeout of pure idling (resident memory stays flat, no unload).
- **SC-003**: From the start of a load, the settings model section shows the loading indication within 1 second and transitions to the current backend/device (or the failure reason) when the load concludes; a stale backend line is never shown as current.
- **SC-004**: Ten repetitions of "dictate -> wait past the timeout -> dictate" yield ten complete transcripts, none missing its beginning and none requiring a second hotkey gesture.
- **SC-005**: A timeout change is written to the config file immediately, survives a restart, and applies without one; a missing or unrecognized value falls back to the 15-minute default.
- **SC-006**: After a failed reload, the app remains operational (hotkey, settings, diagnostics) and a subsequent attempt succeeds once the cause is removed.
