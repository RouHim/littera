//! The in-process ASR worker.
//!
//! `transcribe_cpp` allows exactly one in-flight compute per model — an active
//! stream holds the model's compute lease and an overlapping run or stream is
//! refused with `Error::Busy` — and neither `Session` nor `Stream` is `Sync`.
//! One dedicated `std::thread` therefore owns the model, the session and the
//! active stream, and serializes dictations through a job queue: a `Start` that
//! arrives while a stream is still active buffers its audio instead of being
//! refused, so no audio is ever dropped and `Busy` never reaches the user
//! (FR-006, FR-018, Scenario 4.6).
//!
//! Nothing here runs on a tokio task: model loading and every compute call are
//! blocking, long-running native calls. Besides the dictation events, the worker
//! reports the load lifecycle of whatever model it is asked to load
//! ([`EngineEvent::Loading`], [`EngineEvent::Ready`], [`EngineEvent::LoadFailed`])
//! and releases the model on demand ([`EngineCmd::Unload`]), which keeps the
//! one-thread/one-model invariant intact.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use anyhow::anyhow;
use transcribe_cpp::{
    Backend, CommitPolicy, ExtSlot, Model, ModelOptions, ParakeetStreamOptions, RunOptions,
    StreamExtension, StreamOptions,
};

use crate::config::{BackendPreference, Language};
use crate::model_catalog::{self, Artifact, Selection};
use crate::models;
use crate::stream_lease::StreamLease;

/// At most one `Partial` per window, so the overlay redraw rate stays bounded.
const PARTIAL_THROTTLE: Duration = Duration::from_millis(100);

/// What the engine ended up using — user-visible through diagnostics (FR-005, FR-022).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    /// Display label of the bound backend, e.g. `"Vulkan"` or `"CPU"`.
    pub backend: String,
    /// Human-readable compute device description, e.g.
    /// `"AMD Radeon RX 6950 XT (RADV NAVI21)"`.
    pub device: String,
    /// Set when the requested backend failed and the CPU backend served the
    /// model instead; carries the original load error.
    pub fallback: Option<String>,
}

/// Events the worker reports to the app over the channel given to [`spawn`].
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// A load or reload of the model at `path` started (FR-001): the GUI shows the
    /// indeterminate loading state. Emitted before the file is verified, so a long
    /// checksum also runs inside the loading state (FR-002/FR-003).
    Loading { path: PathBuf },
    /// The model at `path` finished loading (also sent after a fallback load).
    Ready { path: PathBuf, info: EngineInfo },
    /// The load of the model at `path` failed; the app reports the reason and the
    /// worker stays alive for a retry on the next use (FR-001/FR-016).
    LoadFailed { path: PathBuf, message: String },
    /// The model was released by the idle timeout; the app shows the not-resident
    /// state and does not reload on its own (FR-012/FR-014).
    Unloaded,
    /// Throttled streaming text of the active dictation.
    Partial { text: String },
    /// The finalized text of the dictation that was stopped.
    Final {
        text: String,
        language: Option<String>,
    },
    /// A `Start` arrived while another dictation was still using the model; its
    /// audio is buffered and transcribed next.
    Queued,
    /// A dictation could not be produced; the recording is discarded.
    Failed { message: String },
}

/// Commands the app sends to the worker.
#[derive(Debug, Clone)]
pub enum EngineCmd {
    /// Begin a dictation. While a stream is active this queues one instead.
    Start { language: Language },
    /// 16 kHz mono samples belonging to the most recent [`EngineCmd::Start`].
    Audio(Vec<f32>),
    /// End the active dictation and finalize its text.
    Stop,
    /// (Re)load a model from `path` with a new backend preference.
    LoadModel(PathBuf, BackendPreference),
    /// Release the loaded model — host RAM and, on GPU runs, VRAM — without
    /// stopping the worker (FR-011); the next `Start` or `LoadModel` loads again.
    Unload,
    /// Drop the model and stop the worker thread.
    Shutdown,
}

/// Handle to the engine worker thread.
pub struct Engine {
    tx: Sender<EngineCmd>,
    worker: Option<JoinHandle<()>>,
}

impl Engine {
    /// Send a command, ignoring a worker that is already gone (logged at debug).
    pub fn send(&self, cmd: EngineCmd) {
        if let Err(err) = self.tx.send(cmd) {
            tracing::debug!(error = %err, "engine worker is gone; command dropped");
        }
    }

    /// Stop the worker and wait for it to release the model.
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            if self.tx.send(EngineCmd::Shutdown).is_err() {
                tracing::debug!("engine worker was already stopped");
            }
            if worker.join().is_err() {
                tracing::error!("engine worker thread panicked");
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn the single engine worker thread.
///
/// `selection` is the snapshot variant/artifact of `path` (its size and SHA-256
/// drive the pre-flight check, its advertised languages the hint spelling),
/// `pref` is the backend preference used for the first load, and `events`
/// receives every [`EngineEvent`]. The model is loaded on the first
/// [`EngineCmd::Start`] or [`EngineCmd::LoadModel`], never at spawn: a missing
/// or incomplete model file must not produce an error at boot.
pub fn spawn(
    selection: Selection,
    path: PathBuf,
    pref: BackendPreference,
    events: Sender<EngineEvent>,
) -> Engine {
    let (tx, rx) = mpsc::channel();
    let worker = Worker {
        selection,
        path,
        pref,
        events,
        model: None,
        active: None,
        queue: VecDeque::new(),
        verified: None,
    };
    match thread::Builder::new()
        .name("lingua-engine".to_owned())
        .spawn(move || worker.run(rx))
    {
        Ok(handle) => Engine {
            tx,
            worker: Some(handle),
        },
        Err(err) => {
            // The receiver is dropped with the closure, so `send` fails and is
            // logged instead of blocking the app.
            tracing::error!(error = %err, "cannot start the engine thread");
            Engine { tx, worker: None }
        }
    }
}

/// The one-time `transcribe_cpp::init_backends_default()` call, shared by the
/// engine worker and diagnostics.
///
/// The native registry must not be raced and is not retryable in the same
/// process, so the outcome — success or failure — is memoized. Diagnostics and
/// the worker may call this from different threads concurrently; `LazyLock`
/// makes the first caller the only one that runs the initializer.
pub fn init_backends() -> anyhow::Result<()> {
    static BACKENDS: LazyLock<Result<(), String>> = LazyLock::new(|| {
        transcribe_cpp::init_backends_default()
            .map_err(|err| err.to_string())
            .inspect_err(|err| tracing::error!(error = %err, "backend initialization failed"))
    });
    match &*BACKENDS {
        Ok(()) => Ok(()),
        Err(detail) => Err(anyhow!("backend initialization failed: {detail}")),
    }
}

/// Enumerate the registered compute devices without loading a model.
///
/// Returns the engine info the user should expect for `pref`, plus the reason
/// nothing usable was found. Used by the app's capability check and
/// `lingua --diagnostics` (FR-014, FR-022).
pub fn probe_devices(pref: BackendPreference) -> (Option<EngineInfo>, Option<String>) {
    if let Err(err) = init_backends() {
        return (None, Some(err.to_string()));
    }
    let devices = transcribe_cpp::devices();
    let description = |kind: &str| {
        devices
            .iter()
            .find(|device| device.kind == kind)
            .map(|device| device.description.clone())
    };

    match pref {
        BackendPreference::Cpu => match description("cpu") {
            Some(device) => (
                Some(EngineInfo {
                    backend: backend_label("cpu"),
                    device,
                    fallback: None,
                }),
                None,
            ),
            None => (None, Some("no CPU compute device is registered".to_owned())),
        },
        BackendPreference::Auto | BackendPreference::Vulkan => match description("vulkan") {
            Some(device) => (
                Some(EngineInfo {
                    backend: backend_label("vulkan"),
                    device,
                    fallback: None,
                }),
                None,
            ),
            None => match description("cpu") {
                Some(device) => (
                    Some(EngineInfo {
                        backend: backend_label("cpu"),
                        device,
                        fallback: Some("no Vulkan device found".to_owned()),
                    }),
                    None,
                ),
                None => (
                    None,
                    Some(
                        "no compute device is registered (no Vulkan and no CPU device) — \
                         install/repair the Mesa RADV Vulkan driver (vulkaninfo)"
                            .to_owned(),
                    ),
                ),
            },
        },
    }
}

/// `RunOptions` for one dictation: the language hint (FR-007).
///
/// The hint must be spelled exactly as the loaded GGUF advertises the language,
/// or the C API refuses the run — the variants disagree (`en-US` for Nemotron
/// 3.5, bare `en` for Voxtral and the English-only models), so
/// [`model_catalog::language_hint`] picks the spelling and `None` means "run
/// without a hint". A configured language the model does not serve is a
/// non-blocking warning, never an error (FR-011).
///
/// `keep_special_tags` is on although FR-007 forbids tags in the inserted text:
/// the tags are how the *detected* language reaches the session record (the
/// pinned Nemotron 3.5 tags every committed segment, e.g. `<en-US>`, and the
/// library drops them before we could read them). [`strip_language_tags`]
/// removes them again, so nothing tagged ever reaches the overlay, the
/// clipboard or the keyboard path.
fn run_options_for(language: Language, selection: Selection) -> RunOptions {
    let hint = model_catalog::language_hint(selection.variant, language);
    if !language.is_auto() && hint.is_none() {
        tracing::warn!(
            variant = selection.variant.id,
            language = language.label(),
            "the model does not advertise this language; running without a language hint"
        );
    }
    RunOptions {
        language: hint.map(str::to_owned),
        keep_special_tags: true,
        ..RunOptions::default()
    }
}

/// Longest `<…>` run still treated as a tag rather than as dictated text.
const TAG_MAX_LEN: usize = 40;

/// Split `text` into the text without special tags and the first language tag
/// it carried.
///
/// Only `<…>` runs without whitespace, without nesting and at most
/// [`TAG_MAX_LEN`] long count as tags — `a < b` and `x <y z> w` are kept
/// verbatim — and a removal also takes the space that separated the tag from
/// the neighbouring word, so stripping never leaves a double space behind. A
/// tag whose content looks like a language code (`en`, `de-DE`, `zh-Hans-CN`)
/// is reported, any other tag (e.g. `<|nospeech|>`) is removed and ignored.
///
/// The first reported code is the session's language: a recording that switches
/// language mid-way is reported with the language it started in.
fn strip_language_tags(text: &str) -> (String, Option<String>) {
    if !text.contains('<') {
        return (text.to_owned(), None);
    }
    let mut cleaned = String::with_capacity(text.len());
    let mut detected: Option<String> = None;
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(offset) = rest[start..].find('>') else {
            break;
        };
        let end = start + offset;
        let inner = &rest[start + 1..end];
        if !is_tag(inner) {
            // Not a tag: keep the text up to and including this `<` and keep
            // scanning after it.
            cleaned.push_str(&rest[..=start]);
            rest = &rest[start + 1..];
            continue;
        }
        cleaned.push_str(&rest[..start]);
        if detected.is_none() && is_language_code(inner) {
            detected = Some(inner.to_owned());
        }
        let mut tail = &rest[end + 1..];
        // A removal also takes the space that tied the tag to its neighbour, so
        // stripping never leaves a double space (or a leading one) behind.
        if cleaned.is_empty() || cleaned.ends_with(' ') {
            if tail.starts_with(' ') {
                tail = tail.trim_start_matches(' ');
            } else if tail.is_empty() && cleaned.ends_with(' ') {
                cleaned.pop();
            }
        }
        rest = tail;
    }
    cleaned.push_str(rest);
    (cleaned, detected)
}

/// Whether `<inner>` is a special tag: non-empty, no whitespace, no nesting.
fn is_tag(inner: &str) -> bool {
    !inner.is_empty()
        && inner.len() <= TAG_MAX_LEN
        && !inner.contains('<')
        && !inner.contains(char::is_whitespace)
}

/// Whether tag content looks like a language code (`en`, `en-US`, `zh-Hans-CN`).
fn is_language_code(inner: &str) -> bool {
    let mut parts = inner.split('-');
    let Some(first) = parts.next() else {
        return false;
    };
    let primary = (first.len() == 2 || first.len() == 3)
        && first
            .chars()
            .all(|character| character.is_ascii_alphabetic());
    primary
        && parts.all(|part| {
            !part.is_empty()
                && part.len() <= 8
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
}

/// The backend request for a preference, plus whether a CPU retry is allowed
/// when the GPU backend fails to load (FR-005, SC-004).
fn backend_for(pref: BackendPreference) -> (Backend, bool) {
    match pref {
        BackendPreference::Auto | BackendPreference::Vulkan => (Backend::Vulkan, true),
        BackendPreference::Cpu => (Backend::Cpu, false),
    }
}

/// Stream parameters: commit the prefix as soon as the stable-prefix
/// implementation agrees once, with the lowest lookahead the model offers, so
/// partials grow immediately (SC-002).
fn stream_options(family: Option<StreamExtension>) -> StreamOptions {
    StreamOptions {
        commit_policy: CommitPolicy::Auto,
        stable_prefix_agreement_n: 1,
        family,
    }
}

/// The lowest-lookahead stream extension the loaded model accepts (SC-002).
///
/// Whether a stream extension is accepted is a property of the *model variant*,
/// not of the family: the cache-aware Parakeet variants take `ParakeetStream`
/// (0 frames of lookahead), `parakeet-unified-en-0.6b` takes the buffered tuple
/// instead, and Moonshine Streaming and Voxtral Mini take neither.
/// `transcribe_stream_begin` refuses an unaccepted kind with
/// `TRANSCRIBE_ERR_INVALID_ARG`, so the kind is probed at runtime — an
/// assumption here would break every non-cache-aware model in the snapshot.
fn stream_extension(model: &Model) -> Option<StreamExtension> {
    let cache_aware = model.accepts_ext(
        ExtSlot::Stream,
        transcribe_cpp::sys::TRANSCRIBE_EXT_KIND_PARAKEET_STREAM,
    );

    cache_aware.then_some(StreamExtension::ParakeetStream(ParakeetStreamOptions {
        att_context_right: Some(0),
    }))
}

/// Whether the throttle window since the last `Partial` has elapsed.
fn partial_due(last: Option<Instant>, now: Instant) -> bool {
    match last {
        None => true,
        Some(last) => now.duration_since(last) >= PARTIAL_THROTTLE,
    }
}

/// Display label for a `transcribe_cpp` backend/device kind (`"vulkan"` →
/// `"Vulkan"`), so diagnostics and the engine report the same spelling.
fn backend_label(kind: &str) -> String {
    // `Model::backend()` appends the device index for GPU backends ("Vulkan0",
    // "CUDA1"), so the label is decided by the name without that suffix.
    let name = kind.trim_end_matches(|character: char| character.is_ascii_digit());
    match name {
        "cpu" => "CPU".to_owned(),
        "vulkan" => "Vulkan".to_owned(),
        "cuda" => "CUDA".to_owned(),
        "rocm" => "ROCm".to_owned(),
        "metal" => "Metal".to_owned(),
        "sycl" => "SYCL".to_owned(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => "unknown".to_owned(),
            }
        }
    }
}

/// The message for a model file that is missing (FR-003 remedy).
fn model_missing_message(path: &Path) -> String {
    format!(
        "model file missing: {} — open settings to download it again",
        path.display()
    )
}

/// The message for a model file whose size or digest does not match the catalog.
fn model_corrupt_message(path: &Path) -> String {
    format!(
        "model file {} is corrupt (checksum mismatch) — download it again in settings",
        path.display()
    )
}

/// The snapshot selection for a model file `path`, matched by file name.
///
/// The worker is spawned with the selection of the configured model, but settings
/// can switch models at runtime (FR-002), so every load re-resolves the selection
/// from the file it is about to load instead of trusting the spawn-time one.
fn selection_for_path(path: &Path) -> Option<Selection> {
    let file_name = path.file_name().and_then(OsStr::to_str)?;
    model_catalog::selection_for_file_name(file_name)
}

/// `(length, mtime)` of a model file that exists and has the expected size.
fn model_file_stamp(artifact: &Artifact, path: &Path) -> Result<(u64, Option<SystemTime>), String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(model_missing_message(path));
        }
        Err(err) => {
            return Err(format!(
                "cannot read model file {}: {err} — check permissions or download it again in settings",
                path.display()
            ));
        }
    };
    let len = metadata.len();
    if len != artifact.size_bytes {
        return Err(model_corrupt_message(path));
    }
    Ok((len, metadata.modified().ok()))
}

/// Which dictation owns an incoming chunk of audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioOwner {
    /// The newest dictation is still waiting for the model.
    Queued,
    /// The dictation that holds the active stream.
    Active,
    /// Nothing is recording (or the failure already discarded it).
    Idle,
}

/// Route a chunk by the newest `Start`: while a dictation waits for the model,
/// every chunk after its `Start` belongs to *it*, even though an older
/// dictation may still be streaming. Anything else loses or misattributes
/// audio (Scenario 4.6, FR-006: exactly one insertion per dictation).
fn audio_owner(active: bool, queued: usize) -> AudioOwner {
    if queued > 0 {
        AudioOwner::Queued
    } else if active {
        AudioOwner::Active
    } else {
        AudioOwner::Idle
    }
}

/// One dictation waiting for the model.
struct Job {
    language: Language,
    /// Audio captured while this dictation waited for the active stream.
    buffered: Vec<f32>,
}

/// The dictation that currently owns the model's compute lease.
struct ActiveJob {
    language: Language,
    /// The session with its stream; `stream_lease` owns the borrow invariant.
    lease: StreamLease,
    last_partial: Option<Instant>,
    /// First language the model reported for this recording, kept from the
    /// partials in case the final text arrives without a tag.
    detected: Option<String>,
}

impl ActiveJob {
    /// Open a session and begin its stream.
    fn open(model: &Model, language: Language, selection: Selection) -> Result<Self, String> {
        let options = run_options_for(language, selection);
        // The lookahead extension is a per-variant capability. A variant that
        // accepts the kind but not the requested lookahead (the menu values are
        // per checkpoint) falls back to its own default instead of losing the
        // dictation.
        let lease = match stream_extension(model) {
            Some(family) => {
                match StreamLease::open(model, &options, &stream_options(Some(family))) {
                    Ok(lease) => lease,
                    Err(error) => {
                        tracing::warn!(
                            error,
                            variant = model.variant(),
                            "the model refused the low-lookahead stream extension; using its default"
                        );
                        StreamLease::open(model, &options, &stream_options(None))?
                    }
                }
            }
            None => StreamLease::open(model, &options, &stream_options(None))?,
        };
        Ok(Self {
            language,
            lease,
            last_partial: None,
            detected: None,
        })
    }

    /// Feed a chunk; returns display text when a throttled `Partial` is due.
    fn feed(&mut self, samples: &[f32]) -> Result<Option<String>, String> {
        if !self.lease.feed(samples)? {
            return Ok(None);
        }
        let now = Instant::now();
        if !partial_due(self.last_partial, now) {
            return Ok(None);
        }
        self.last_partial = Some(now);
        let (text, detected) = strip_language_tags(&self.lease.display());
        if self.detected.is_none() {
            self.detected = detected;
        }
        Ok(Some(text))
    }

    /// Flush the stream and read the final text plus the language it was in.
    ///
    /// The text is read from the stream *after* `finalize`, so it is available
    /// even when the returned update reports no change. The reported language is
    /// the one the model tagged in the final text, or — when that text carries
    /// no tag — the first language seen in the partials.
    fn finalize_text(&mut self) -> Result<(String, Option<String>), String> {
        self.lease.finalize()?;
        let (text, detected) = strip_language_tags(&self.lease.display());
        Ok((text, detected.or_else(|| self.detected.take())))
    }
}

/// The worker thread state.
struct Worker {
    selection: Selection,
    path: PathBuf,
    pref: BackendPreference,
    events: Sender<EngineEvent>,
    model: Option<Model>,
    active: Option<ActiveJob>,
    queue: VecDeque<Job>,
    /// `(length, mtime)` of the model file at its last successful verification.
    verified: Option<(u64, Option<SystemTime>)>,
}

impl Worker {
    fn run(mut self, rx: Receiver<EngineCmd>) {
        loop {
            let cmd = match rx.recv() {
                Ok(cmd) => cmd,
                Err(err) => {
                    tracing::debug!(error = %err, "engine command channel closed; worker exiting");
                    break;
                }
            };
            match cmd {
                EngineCmd::LoadModel(path, pref) => self.handle_load_model(path, pref),
                EngineCmd::Start { language } => self.handle_start(language),
                EngineCmd::Audio(samples) => self.handle_audio(samples),
                EngineCmd::Stop => self.handle_stop(),
                EngineCmd::Unload => self.handle_unload(),
                EngineCmd::Shutdown => {
                    tracing::debug!("engine shutdown requested");
                    break;
                }
            }
        }
        // Release the session, the stream and the model (and with it the weights
        // in RAM/VRAM) before the thread ends.
        self.active = None;
        self.model = None;
        tracing::info!("engine worker stopped");
    }

    fn emit(&self, event: EngineEvent) {
        if self.events.send(event).is_err() {
            tracing::debug!("engine event dropped: the receiving channel is closed");
        }
    }

    /// Discard the active recording and everything queued behind it.
    fn fail_with(&mut self, event: EngineEvent) {
        if let EngineEvent::Failed { message } = &event {
            tracing::error!(message, "dictation failed");
        } else if let EngineEvent::LoadFailed { path, message } = &event {
            tracing::error!(path = %path.display(), message, "model load failed");
        }
        self.active = None;
        if !self.queue.is_empty() {
            tracing::warn!(
                dropped = self.queue.len(),
                "dropping queued dictations because the engine failed"
            );
            self.queue.clear();
        }
        self.emit(event);
    }

    /// The failure of a run (stream, session): the model stays loaded.
    fn fail(&mut self, message: String) {
        self.fail_with(EngineEvent::Failed { message });
    }

    /// The failure of a load of `path`; the app's model status learns about it.
    fn fail_load_for(&mut self, path: PathBuf, message: String) {
        self.fail_with(EngineEvent::LoadFailed { path, message });
    }

    fn fail_load(&mut self, message: String) {
        self.fail_load_for(self.path.clone(), message);
    }

    /// Verify the model file before it is loaded or used.
    ///
    /// The `(length, mtime)` stamp only short-circuits the digest when the file
    /// has an mtime: a file without one looks identical to a same-length
    /// tampered artifact, so its digest is re-checked every time (FR-003). A
    /// passed check means the file is byte-identical to the snapshot artifact,
    /// so the loaded model never has to be reloaded.
    fn preflight(&mut self) -> Result<(), String> {
        let stamp = model_file_stamp(self.selection.artifact, &self.path)?;
        if stamp.1.is_some() && self.verified == Some(stamp) {
            return Ok(());
        }
        // No cancel flag: this runs once per artifact inside the worker, and the
        // `(length, mtime)` stamp short-circuits every later dictation.
        match models::verify_matches_entry(self.selection.artifact, &self.path, None)
            .map_err(|err| format!("cannot verify model file {}: {err}", self.path.display()))?
        {
            models::Verify::Verified => {}
            models::Verify::Mismatch | models::Verify::Cancelled => {
                return Err(model_corrupt_message(&self.path));
            }
        }
        if stamp.1.is_some() {
            self.verified = Some(stamp);
        }
        tracing::debug!(file = %self.path.display(), "model file verified");
        Ok(())
    }

    fn handle_load_model(&mut self, path: PathBuf, pref: BackendPreference) {
        // The snapshot artifact supplies the expected size and SHA-256, and the
        // selection captured at `spawn` is the *configured* model — settings can
        // switch models at runtime (FR-002, Scenario 3.4), so the selection is
        // re-resolved from the file name before any state changes.
        let Some(selection) = selection_for_path(&path) else {
            self.fail_load_for(
                path.clone(),
                format!(
                    "unknown model file {} — choose one of the models listed in settings",
                    path.display()
                ),
            );
            return;
        };
        tracing::info!(
            path = %path.display(),
            backend = pref.label(),
            "loading model"
        );
        self.selection = selection;
        self.path = path;
        self.pref = pref;
        self.verified = None;
        self.model = None;
        self.emit(EngineEvent::Loading {
            path: self.path.clone(),
        });
        if let Err(message) = self.preflight() {
            self.fail_load(message);
            return;
        }
        if let Err(message) = self.load_model() {
            self.fail_load(message);
        }
    }

    /// Load (or reload) the model and publish [`EngineEvent::Ready`].
    fn load_model(&mut self) -> Result<(), String> {
        init_backends().map_err(|err| err.to_string())?;
        let (backend, retry_cpu) = backend_for(self.pref);
        let requested = ModelOptions {
            backend,
            device: None,
        };
        let (model, fallback) = match Model::load_with(&self.path, &requested) {
            Ok(model) => (model, None),
            Err(err) if retry_cpu => {
                tracing::warn!(
                    error = %err,
                    path = %self.path.display(),
                    "the requested GPU backend failed to load the model; retrying on the CPU backend"
                );
                let cpu = Model::load_with(
                    &self.path,
                    &ModelOptions {
                        backend: Backend::Cpu,
                        device: None,
                    },
                )
                .map_err(|err| {
                    format!(
                        "model load failed on both the Vulkan and the CPU backend for {}: {err}",
                        self.path.display()
                    )
                })?;
                (cpu, Some(err.to_string()))
            }
            Err(err) => {
                return Err(format!(
                    "model load failed for {}: {err}",
                    self.path.display()
                ));
            }
        };

        let device = model.device().map_err(|err| {
            format!(
                "model loaded but no compute device was resolved: {err} (file: {})",
                self.path.display()
            )
        })?;
        let info = EngineInfo {
            backend: backend_label(&model.backend()),
            device: device.description,
            fallback,
        };
        tracing::info!(
            backend = %info.backend,
            device = %info.device,
            fallback = ?info.fallback,
            "engine ready"
        );
        self.model = Some(model);
        self.emit(EngineEvent::Ready {
            path: self.path.clone(),
            info,
        });
        Ok(())
    }

    fn handle_start(&mut self, language: Language) {
        if self.active.is_some() {
            tracing::info!(
                language = language.label(),
                queued = self.queue.len() + 1,
                "dictation queued behind the active one"
            );
            self.queue.push_back(Job {
                language,
                buffered: Vec::new(),
            });
            self.emit(EngineEvent::Queued);
            return;
        }
        if self.model.is_none() {
            // Recording starts while the model is not resident (FR-004): the
            // app shows the loading state, and the audio of this dictation is
            // buffered by the active job once the model is up.
            self.emit(EngineEvent::Loading {
                path: self.path.clone(),
            });
        }
        // A preflight failure with a still-resident model (file deleted or
        // replaced mid-session) is a load failure: the file is what every later
        // dictation needs, and the app's model status must say so (FR-016).
        if let Err(message) = self.preflight() {
            self.fail_load(message);
            return;
        }
        if let Err(message) = self.load_model_if_needed() {
            self.fail_load(message);
            return;
        }
        match self.model.as_ref() {
            Some(model) => match ActiveJob::open(model, language, self.selection) {
                Ok(job) => {
                    tracing::info!(language = language.label(), "dictation started");
                    self.active = Some(job);
                }
                Err(message) => self.fail(message),
            },
            None => self.fail(
                "transcription failed: no model is loaded — open settings to choose and download a model"
                    .to_owned(),
            ),
        }
    }

    fn load_model_if_needed(&mut self) -> Result<(), String> {
        if self.model.is_some() {
            return Ok(());
        }
        self.load_model()
    }

    /// Release the model after the app's idle timeout (FR-011).
    ///
    /// The app only asks while nothing is in flight; the refusal below is the
    /// safety net for a racing `Start`, because the model must never be dropped
    /// under a live session or a queued dictation. `verified` (the file stamp)
    /// survives, so the reload skips the digest.
    fn handle_unload(&mut self) {
        if self.active.is_some() || !self.queue.is_empty() {
            tracing::debug!("unload refused: a dictation is in flight");
            return;
        }
        if self.model.take().is_none() {
            tracing::debug!("unload requested with no model loaded");
            return;
        }
        tracing::info!(path = %self.path.display(), "model released after the idle timeout");
        self.emit(EngineEvent::Unloaded);
    }

    fn handle_audio(&mut self, samples: Vec<f32>) {
        if samples.is_empty() {
            return;
        }
        match audio_owner(self.active.is_some(), self.queue.len()) {
            // Buffer for the dictation that is still waiting for the model, so
            // its promotion replays exactly its own audio in order.
            AudioOwner::Queued => {
                if let Some(job) = self.queue.back_mut() {
                    job.buffered.extend_from_slice(&samples);
                }
            }
            AudioOwner::Active => {
                let Some(job) = self.active.as_mut() else {
                    return;
                };
                let outcome = job.feed(&samples);
                match outcome {
                    Ok(Some(text)) => self.emit(EngineEvent::Partial { text }),
                    Ok(None) => {}
                    Err(message) => self.fail(message),
                }
            }
            AudioOwner::Idle => tracing::debug!(
                samples = samples.len(),
                "audio arrived with no dictation in flight; dropped"
            ),
        }
    }

    fn handle_stop(&mut self) {
        if self.active.is_none() {
            // A queued dictation whose `Start` never reached a stream (its
            // predecessor was still holding the model) is finalized here.
            match self.queue.pop_front() {
                Some(job) => {
                    tracing::debug!("finalizing a queued dictation that never opened a stream");
                    if let Err(message) = self.promote(job) {
                        self.fail(message);
                        return;
                    }
                }
                None => {
                    // The engine must always answer a `Stop`; otherwise the app
                    // waits for a `Final` that never arrives.
                    tracing::debug!("stop with no dictation in flight");
                    self.emit(EngineEvent::Final {
                        text: String::new(),
                        language: None,
                    });
                    return;
                }
            }
        }
        let Some(mut job) = self.active.take() else {
            return;
        };
        let hint = job.language;
        match job.finalize_text() {
            Ok((text, detected)) => {
                let text = text.trim().to_owned();
                // The model's own tag wins; with an explicit hint it echoes the
                // hint, and a recording the model did not tag falls back to the
                // hint we sent (Spec: Dictation session — "the language actually
                // used").
                let language = detected.or_else(|| hint.api_code().map(str::to_owned));
                tracing::info!(
                    hint = hint.label(),
                    detected = ?language,
                    chars = text.chars().count(),
                    "dictation finished"
                );
                self.emit(EngineEvent::Final { text, language });
                self.drain_queue();
            }
            Err(message) => self.fail(message),
        }
    }

    /// Start the next queued dictation, if any.
    fn drain_queue(&mut self) {
        if let Some(job) = self.queue.pop_front() {
            tracing::info!(
                language = job.language.label(),
                remaining = self.queue.len(),
                "starting the queued dictation"
            );
            if let Err(message) = self.promote(job) {
                self.fail(message);
            }
        }
    }

    /// Open a stream for a queued dictation and replay its buffered audio.
    fn promote(&mut self, job: Job) -> Result<(), String> {
        let model = self.model.as_ref().ok_or_else(|| {
            "transcription failed: no model is loaded — open settings to choose and download a model"
                .to_owned()
        })?;
        let mut active = ActiveJob::open(model, job.language, self.selection)?;
        let partial = if job.buffered.is_empty() {
            None
        } else {
            active.feed(&job.buffered)?
        };
        self.active = Some(active);
        if let Some(text) = partial {
            self.emit(EngineEvent::Partial { text });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;

    #[test]
    fn run_options_carry_the_variant_spelling_and_ask_for_the_language_tags() {
        let selection = |id: &str| model_catalog::artifact(id).expect("offered");

        // Nemotron 3.5 advertises locale codes, so the hint is the locale.
        let nemotron = selection("nemotron-3.5-asr-streaming-0.6b-Q8_0");
        assert_eq!(run_options_for(Language::AUTO, nemotron).language, None);
        assert_eq!(
            run_options_for(Language::new("en-US"), nemotron).language,
            Some("en-US".to_owned())
        );
        assert_eq!(
            run_options_for(Language::new("de-DE"), nemotron).language,
            Some("de-DE".to_owned())
        );

        // Voxtral and the English-only models advertise bare codes; a locale
        // hint would be refused by the C API.
        let voxtral = selection("voxtral-mini-4b-realtime-2602-Q8_0");
        assert_eq!(
            run_options_for(Language::new("de-DE"), voxtral).language,
            Some("de".to_owned())
        );
        let moonshine = selection("moonshine-streaming-tiny-Q8_0");
        assert_eq!(
            run_options_for(Language::new("en-US"), moonshine).language,
            Some("en".to_owned())
        );
        // A language the model does not serve runs without a hint (FR-011)
        // instead of failing the run.
        assert_eq!(
            run_options_for(Language::new("de-DE"), moonshine).language,
            None
        );

        // The tags stay on for every combination: they carry the detected
        // language, and `strip_language_tags` removes them before anything
        // user-visible.
        for selection in model_catalog::artifacts() {
            for language in Language::ALL {
                let options = run_options_for(language, selection);
                assert!(options.keep_special_tags, "{}", selection.artifact.id);
                assert!(
                    options
                        .language
                        .is_none_or(|hint| selection.variant.languages.contains(&hint.as_str())),
                    "{} must only be hinted a spelling it advertises",
                    selection.artifact.id
                );
            }
        }
    }

    #[test]
    fn the_detected_language_is_read_from_the_model_tag_and_stripped() {
        // Verbatim output of the pinned Nemotron 3.5 model on `samples/jfk.wav`
        // with `keep_special_tags` and `language = None` (measured 2026-10-15):
        // one tag per committed segment, in the partials as well as the final.
        let tagged = "And so my fellow Americans ask not what your country can do for you. <en-US> \
                      Ask what you can do for your country. <en-US>";
        let clean = "And so my fellow Americans ask not what your country can do for you. \
                     Ask what you can do for your country.";
        let (text, detected) = strip_language_tags(tagged);
        assert_eq!(text, clean);
        assert_eq!(detected, Some("en-US".to_owned()));
        // Stripping is idempotent and a clean text reports nothing.
        assert_eq!(strip_language_tags(&text), (clean.to_owned(), None));
    }

    #[test]
    fn a_leading_tag_does_not_leave_a_leading_space() {
        let (text, detected) = strip_language_tags("<en>hello");
        assert_eq!((text.as_str(), detected), ("hello", Some("en".to_owned())));
        let (text, detected) = strip_language_tags("<de-DE> eins zwei <en-US> three");
        assert_eq!(
            (text.as_str(), detected),
            ("eins zwei three", Some("de-DE".to_owned()))
        );
        assert_eq!(
            strip_language_tags("<en>"),
            (String::new(), Some("en".to_owned()))
        );
    }

    #[test]
    fn tag_variants_are_stripped_and_only_language_codes_are_reported() {
        // Language codes in the shapes the model can emit.
        let (text, detected) = strip_language_tags("hello <de-DE>");
        assert_eq!(
            (text.as_str(), detected),
            ("hello", Some("de-DE".to_owned()))
        );
        let (text, detected) = strip_language_tags("a <zh-Hans-CN> b");
        assert_eq!(
            (text.as_str(), detected),
            ("a b", Some("zh-Hans-CN".to_owned()))
        );
        // The first tag is the session's language, later ones do not override it.
        let (text, detected) = strip_language_tags("eins <de-DE> zwei <en-US> three");
        assert_eq!(
            (text.as_str(), detected),
            ("eins zwei three", Some("de-DE".to_owned()))
        );
        // Any other tag is removed too, but never reported as a language.
        let (text, detected) = strip_language_tags("<|nospeech|>");
        assert_eq!((text.as_str(), detected), ("", None));
    }

    #[test]
    fn text_that_only_looks_like_a_tag_survives() {
        // No closing bracket, or whitespace inside the run: dictated text.
        let untouched = ["3 < 4", "x <y z> w", "a < b> c", "no brackets at all"];
        for text in untouched {
            assert_eq!(strip_language_tags(text), (text.to_owned(), None));
        }
        // A run with a nested `<` is not a tag either, but scanning continues
        // after it, so a well-formed tag further right is still removed.
        assert_eq!(
            strip_language_tags("a <b <en-US> d"),
            ("a <b d".to_owned(), Some("en-US".to_owned()))
        );
        // Empty and over-long bracket runs are not tags either.
        let (text, detected) = strip_language_tags("a <> b");
        assert_eq!((text.as_str(), detected), ("a <> b", None));
        let long = format!("a <{}> b", "x".repeat(TAG_MAX_LEN + 1));
        assert_eq!(strip_language_tags(&long), (long.clone(), None));
    }

    #[test]
    fn backend_preference_maps_to_a_backend_and_a_cpu_retry() {
        assert_eq!(
            backend_for(BackendPreference::Auto),
            (Backend::Vulkan, true)
        );
        assert_eq!(
            backend_for(BackendPreference::Vulkan),
            (Backend::Vulkan, true)
        );
        assert_eq!(backend_for(BackendPreference::Cpu), (Backend::Cpu, false));
    }

    #[test]
    fn stream_options_commit_early_and_carry_the_requested_lookahead() {
        // No extension: the model's own family default applies.
        let plain = stream_options(None);
        assert_eq!(plain.commit_policy, CommitPolicy::Auto);
        assert_eq!(plain.stable_prefix_agreement_n, 1);
        assert_eq!(plain.family, None);

        let cache_aware = stream_options(Some(StreamExtension::ParakeetStream(
            ParakeetStreamOptions {
                att_context_right: Some(0),
            },
        )));
        assert_eq!(cache_aware.commit_policy, CommitPolicy::Auto);
        assert_eq!(cache_aware.stable_prefix_agreement_n, 1);
        assert_eq!(
            cache_aware.family,
            Some(StreamExtension::ParakeetStream(ParakeetStreamOptions {
                att_context_right: Some(0)
            }))
        );
    }

    #[test]
    fn partials_are_throttled_to_one_per_window() {
        let start = Instant::now();
        assert!(partial_due(None, start));
        assert!(!partial_due(
            Some(start),
            start + PARTIAL_THROTTLE - Duration::from_millis(1)
        ));
        assert!(partial_due(Some(start), start + PARTIAL_THROTTLE));
    }

    #[test]
    fn backend_labels_are_user_facing() {
        assert_eq!(backend_label("vulkan"), "Vulkan");
        assert_eq!(backend_label("Vulkan0"), "Vulkan");
        assert_eq!(backend_label("vulkan1"), "Vulkan");
        assert_eq!(backend_label("cpu"), "CPU");
        assert_eq!(backend_label("gpu"), "Gpu");
        assert_eq!(backend_label(""), "unknown");
    }

    #[test]
    fn every_catalog_file_resolves_to_its_own_entry() {
        // Model switching (settings, FR-002) re-resolves the selection from the
        // file name; a stale spawn-time selection would reject every other model
        // with a bogus checksum failure.
        for selection in model_catalog::artifacts() {
            let path = PathBuf::from("/models").join(selection.artifact.file_name);
            assert_eq!(
                selection_for_path(&path),
                Some(selection),
                "{} must resolve to its own artifact",
                selection.artifact.file_name
            );
        }
        assert!(selection_for_path(Path::new("/models/not-a-lingua-model.gguf")).is_none());
        assert!(selection_for_path(Path::new("/models")).is_none());
    }

    #[test]
    fn audio_goes_to_the_newest_waiting_dictation() {
        // A dictation that is still waiting for the model owns every chunk that
        // arrives after its `Start`, even while an older dictation is still
        // streaming (Scenario 4.6: its audio must not land in the older stream).
        assert_eq!(audio_owner(true, 1), AudioOwner::Queued);
        assert_eq!(audio_owner(true, 3), AudioOwner::Queued);
        // With nothing waiting, the active stream owns the chunk.
        assert_eq!(audio_owner(true, 0), AudioOwner::Active);
        // Nothing recording at all.
        assert_eq!(audio_owner(false, 0), AudioOwner::Idle);
    }

    #[test]
    fn queued_audio_is_buffered_verbatim() {
        let (tx, _rx) = mpsc::channel();
        let mut worker = Worker {
            selection: model_catalog::default_selection(),
            path: PathBuf::from("/nonexistent/lingua-engine-test.gguf"),
            pref: BackendPreference::Auto,
            events: tx,
            model: None,
            active: None,
            queue: VecDeque::from([Job {
                language: Language::new("de-DE"),
                buffered: Vec::new(),
            }]),
            verified: None,
        };

        worker.handle_audio(vec![0.25, -0.5, 1.0]);
        worker.handle_audio(Vec::new());

        assert_eq!(worker.queue.len(), 1);
        assert_eq!(
            worker.queue.front().map(|job| job.buffered.clone()),
            Some(vec![0.25, -0.5, 1.0])
        );
        assert_eq!(
            worker.queue.front().map(|job| job.language),
            Some(Language::new("de-DE"))
        );
    }

    #[test]
    fn audio_and_stop_are_answered_without_a_model() {
        let selection = model_catalog::default_selection();
        let (tx, rx) = mpsc::channel();
        let engine = spawn(
            selection,
            PathBuf::from("/nonexistent/lingua-engine-test.gguf"),
            BackendPreference::Auto,
            tx,
        );

        // Audio with no dictation in flight must not produce an event (and must
        // not touch the model, which does not exist in this test).
        engine.send(EngineCmd::Audio(vec![0.0; 16]));
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ));

        // A `Stop` always answers, so the app's state machine never waits forever.
        engine.send(EngineCmd::Stop);
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(EngineEvent::Final { text, language }) => {
                assert!(text.is_empty());
                assert_eq!(language, None);
            }
            other => panic!("expected an empty Final, got {other:?}"),
        }

        drop(engine);
    }

    #[test]
    fn a_load_that_cannot_start_reports_loading_then_the_reason() {
        let path = PathBuf::from("/nonexistent/lingua-engine-test.gguf");
        let (tx, rx) = mpsc::channel();
        let engine = spawn(
            model_catalog::default_selection(),
            path.clone(),
            BackendPreference::Auto,
            tx,
        );

        engine.send(EngineCmd::Start {
            language: Language::AUTO,
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(EngineEvent::Loading { path: loading }) => assert_eq!(loading, path),
            other => panic!("expected Loading first, got {other:?}"),
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(EngineEvent::LoadFailed { message, .. }) => {
                assert!(message.contains("model file missing"), "{message}");
            }
            other => panic!("expected LoadFailed, got {other:?}"),
        }

        drop(engine);
    }

    #[test]
    fn an_explicit_load_of_a_missing_file_fails_with_its_path() {
        let path = PathBuf::from("/nonexistent")
            .join(model_catalog::default_selection().artifact.file_name);
        let (tx, rx) = mpsc::channel();
        let engine = spawn(
            model_catalog::default_selection(),
            PathBuf::from("/nonexistent/lingua-engine-test.gguf"),
            BackendPreference::Auto,
            tx,
        );

        engine.send(EngineCmd::LoadModel(path.clone(), BackendPreference::Auto));
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(EngineEvent::Loading { path: loading }) => assert_eq!(loading, path),
            other => panic!("expected Loading first, got {other:?}"),
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(EngineEvent::LoadFailed {
                path: failed,
                message,
            }) => {
                assert_eq!(failed, path);
                assert!(message.contains("model file missing"), "{message}");
            }
            other => panic!("expected LoadFailed, got {other:?}"),
        }

        drop(engine);
    }

    #[test]
    fn an_unknown_model_file_fails_without_a_loading_event() {
        let path = PathBuf::from("/models/not-a-lingua-model.gguf");
        let (tx, rx) = mpsc::channel();
        let engine = spawn(
            model_catalog::default_selection(),
            PathBuf::from("/nonexistent/lingua-engine-test.gguf"),
            BackendPreference::Auto,
            tx,
        );

        engine.send(EngineCmd::LoadModel(path.clone(), BackendPreference::Auto));
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(EngineEvent::LoadFailed {
                path: failed,
                message,
            }) => {
                assert_eq!(failed, path);
                assert!(message.contains("unknown model file"), "{message}");
            }
            other => panic!("expected LoadFailed first, got {other:?}"),
        }
        // Nothing was loading, so no `Loading` may follow either.
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ));

        drop(engine);
    }

    /// A worker without a model, for tests that must not touch a model file.
    fn worker_without_model(queue: VecDeque<Job>) -> (Worker, Receiver<EngineEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Worker {
                selection: model_catalog::default_selection(),
                path: PathBuf::from("/nonexistent/lingua-engine-test.gguf"),
                pref: BackendPreference::Auto,
                events: tx,
                model: None,
                active: None,
                queue,
                verified: None,
            },
            rx,
        )
    }

    #[test]
    fn unloading_without_a_model_changes_nothing() {
        let (mut worker, rx) = worker_without_model(VecDeque::new());

        worker.handle_unload();

        // No `Unloaded` for a model that was never resident (FR-012).
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn unloading_is_refused_while_a_dictation_waits() {
        let (mut worker, rx) = worker_without_model(VecDeque::from([Job {
            language: Language::AUTO,
            buffered: vec![0.5],
        }]));

        worker.handle_unload();

        // The queued dictation keeps its audio and no release is reported
        // (FR-009: never drop the model under an in-flight dictation).
        assert_eq!(worker.queue.len(), 1);
        assert_eq!(
            worker.queue.front().map(|job| job.buffered.clone()),
            Some(vec![0.5])
        );
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}
