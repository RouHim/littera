//! Model catalog plus verified, resumable, cancellable downloads (FR-003).
//!
//! Every entry pins the exact byte size and SHA-256 of its artifact. A download is
//! written to `<file>.part` and only moved into place once the byte length and the
//! digest both match, so a corrupt or truncated artifact can never be loaded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Catalog id of the default model (multilingual, best quality).
pub const DEFAULT_MODEL_ID: &str = "nemotron-3.5-asr-streaming-0.6b-Q8_0";

/// Head-room kept free on top of the pending download.
const FREE_SPACE_SLACK: u64 = 64 * 1024 * 1024;
/// Minimum interval between two `progress` callbacks.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Read buffer used when hashing an already-downloaded `.part` prefix.
const HASH_BUFFER_BYTES: usize = 1024 * 1024;
/// Read buffer used while a download is streaming to disk.
const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;

/// One downloadable speech model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelEntry {
    /// Stable catalog id, also the `<id>.gguf` file stem.
    pub id: &'static str,
    /// Human-readable name shown in onboarding/settings.
    pub label: &'static str,
    /// One-line description shown before the user commits (FR-002).
    pub note: &'static str,
    /// Pinned download URL (a Hugging Face commit revision).
    pub url: &'static str,
    /// On-disk file name inside the model directory.
    pub file_name: &'static str,
    /// Exact artifact size in bytes.
    pub size_bytes: u64,
    /// Exact SHA-256 of the artifact, lower-case hex.
    pub sha256: &'static str,
    /// Whether the model supports German in addition to English.
    pub multilingual: bool,
    /// Approximate size shown in the UI (`"751 MB"`).
    pub approx_size_label: &'static str,
}

/// The three models offered by the onboarding model step, in display order.
pub const CATALOG: [ModelEntry; 3] = [
    ModelEntry {
        id: "nemotron-3.5-asr-streaming-0.6b-Q8_0",
        label: "Nemotron 3.5 ASR Streaming 0.6B — Q8_0",
        note: "Multilingual (32 locales, incl. German). Best quality.",
        url: "https://huggingface.co/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/resolve/8139c4ec14bdc45c361adf8d57c27c28e7478272/nemotron-3.5-asr-streaming-0.6b-Q8_0.gguf",
        file_name: "nemotron-3.5-asr-streaming-0.6b-Q8_0.gguf",
        size_bytes: 751_094_240,
        sha256: "b94545b313b3223fda7b2857a52681da813935c2127643d1e9ff0c23d988089c",
        multilingual: true,
        approx_size_label: "751 MB",
    },
    ModelEntry {
        id: "nemotron-3.5-asr-streaming-0.6b-Q4_K_M",
        label: "Nemotron 3.5 ASR Streaming 0.6B — Q4_K_M",
        note: "Multilingual (32 locales, incl. German). Smaller, slightly lower accuracy.",
        url: "https://huggingface.co/handy-computer/nemotron-3.5-asr-streaming-0.6b-gguf/resolve/8139c4ec14bdc45c361adf8d57c27c28e7478272/nemotron-3.5-asr-streaming-0.6b-Q4_K_M.gguf",
        file_name: "nemotron-3.5-asr-streaming-0.6b-Q4_K_M.gguf",
        size_bytes: 495_831_520,
        sha256: "41c99fa5fb6f3d35f68e79adc3e755eca2232a8d921178bd647b71194792b8fd",
        multilingual: true,
        approx_size_label: "496 MB",
    },
    ModelEntry {
        id: "nemotron-speech-streaming-en-0.6b-Q8_0",
        label: "Nemotron Speech Streaming EN 0.6B — Q8_0",
        note: "English only. Smallest.",
        url: "https://huggingface.co/handy-computer/nemotron-speech-streaming-en-0.6b-gguf/resolve/9789e0ebf77277911272f0d9a35e1646b5aa6004/nemotron-speech-streaming-en-0.6b-Q8_0.gguf",
        file_name: "nemotron-speech-streaming-en-0.6b-Q8_0.gguf",
        size_bytes: 729_650_176,
        sha256: "90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110",
        multilingual: false,
        approx_size_label: "730 MB",
    },
];

/// Look up a catalog entry by id.
pub fn entry(id: &str) -> Option<&'static ModelEntry> {
    CATALOG.iter().find(|candidate| candidate.id == id)
}

/// The default catalog entry; falls back to the first entry if the catalog is
/// ever edited inconsistently, so this never panics.
pub fn default_entry() -> &'static ModelEntry {
    entry(DEFAULT_MODEL_ID).unwrap_or(&CATALOG[0])
}

/// On-disk path of an entry: `<data dir>/lingua/models/<file_name>` (FR-003).
pub fn path_for(entry: &ModelEntry) -> Result<PathBuf> {
    Ok(path_in(
        crate::config::Config::models_dir()?.as_path(),
        entry,
    ))
}

/// The file name join that [`path_for`] performs, with the base directory injected
/// so the join can be tested without resolving (or creating) the data directory.
fn path_in(base: &Path, entry: &ModelEntry) -> PathBuf {
    base.join(entry.file_name)
}

/// Invoke the download progress callback.
///
/// Taking a plain `&dyn Fn` keeps call sites free of the `Arc`/`Box` `Fn`-call
/// subtleties (only `Box<F>` and `&F` are callable as `Fn` in the standard library).
fn report(progress: &(dyn Fn(u64, u64) + Send + Sync), received: u64, total: u64) {
    progress(received, total);
}

/// [`verify_matches_entry`] on the blocking pool.
///
/// It reads the whole artifact (up to 751 MB) with blocking `std::fs`, which must
/// never occupy a runtime worker thread. `Ok(None)` means the file is not there;
/// otherwise the [`Verify`] outcome — including [`Verify::Cancelled`] when
/// `cancel` was set while hashing.
async fn verify_entry_off_runtime(
    entry: &'static ModelEntry,
    path: PathBuf,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Option<Verify>> {
    let shown = path.display().to_string();
    tokio::task::spawn_blocking(move || {
        if !path.exists() {
            return Ok(None);
        }
        verify_matches_entry(entry, path.as_path(), cancel.as_deref()).map(Some)
    })
    .await
    .with_context(|| format!("cannot verify {shown}: the verification task failed"))?
}

/// Remove `path`, treating "it is already gone" as success.
fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot remove {}", path.display())),
    }
}

/// [`remove_file_if_present`] on `tokio`'s blocking pool.
async fn remove_file_if_present_off_runtime(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || remove_file_if_present(path.as_path()))
        .await
        .context("removing a file did not run")?
}

/// Ensure `entry` is present in the model directory, verified against its
/// pinned size and SHA-256, resuming a previous partial download when possible.
///
/// `cancel` is polled between streamed chunks; setting it aborts with
/// `"download cancelled"` while keeping `<file>.part` so a later call resumes.
/// `progress(received, total)` is invoked at most every 100 ms.
///
/// Everything blocking (creating the model directory, digest verification, the
/// free-space query, the transfer itself) runs on `tokio`'s blocking pool.
pub async fn ensure(
    entry: &'static ModelEntry,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<PathBuf> {
    // `Config::models_dir()` creates `<data dir>/lingua/models`, so it also runs off
    // the runtime thread (the project rule: blocking work never occupies a worker).
    let dir = tokio::task::spawn_blocking(crate::config::Config::models_dir)
        .await
        .context("cannot prepare the model directory")??;
    let path = path_in(dir.as_path(), entry);
    let file_name = entry.file_name;

    // (1) A complete artifact is only accepted when it verifies. The whole probe
    // (existence + digest) runs on the blocking pool.
    match verify_entry_off_runtime(entry, path.clone(), Some(Arc::clone(&cancel))).await? {
        Some(Verify::Verified) => {
            tracing::info!(file = %path.display(), "model already present and verified");
            return Ok(path);
        }
        Some(Verify::Cancelled) => bail!("download cancelled"),
        Some(Verify::Mismatch) => {
            tracing::warn!(file = %path.display(), "existing model file is corrupt; re-downloading");
            remove_file_if_present_off_runtime(path.as_path()).await?;
        }
        None => {
            tracing::info!(file = %path.display(), "no model file yet; downloading it");
        }
    }

    // Determine how much of a previous download we can reuse.
    let part = dir.join(format!("{file_name}.part"));
    let mut part_len = match tokio::fs::metadata(part.as_path()).await {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => {
            bail!(
                "cannot inspect the partial download {}: {err}",
                part.display()
            );
        }
    };
    if part_len > entry.size_bytes {
        tracing::warn!(
            file = %part.display(),
            "partial download is larger than expected; discarding it"
        );
        remove_file_if_present_off_runtime(part.as_path()).await?;
        part_len = 0;
    }
    if part_len == entry.size_bytes {
        let part_verified =
            match verify_entry_off_runtime(entry, part.clone(), Some(Arc::clone(&cancel))).await? {
                Some(Verify::Verified) => true,
                // A cancel must not discard the finished download: the `.part` file
                // stays and the next call verifies it again.
                Some(Verify::Cancelled) => bail!("download cancelled"),
                Some(Verify::Mismatch) | None => false,
            };
        if part_verified {
            tokio::fs::rename(part.as_path(), path.as_path())
                .await
                .with_context(|| {
                    format!(
                        "cannot move {} into place at {}",
                        part.display(),
                        path.display(),
                    )
                })?;
            report(progress.as_ref(), entry.size_bytes, entry.size_bytes);
            tracing::info!(file = %path.display(), "checksum ok, moved");
            return Ok(path);
        }
        tracing::warn!(
            file = %part.display(),
            "partial download failed verification; discarding it"
        );
        remove_file_if_present_off_runtime(part.as_path()).await?;
        part_len = 0;
    }

    // (2) Refuse to start a download that cannot fit, plus slack.
    let needed = entry
        .size_bytes
        .saturating_sub(part_len)
        .saturating_add(FREE_SPACE_SLACK);
    let dir_label = dir.display().to_string();
    let stat = tokio::task::spawn_blocking(move || nix::sys::statvfs::statvfs(dir.as_path()))
        .await
        .with_context(|| format!("cannot check free space for {dir_label}: the query did not run"))?
        .with_context(|| format!("cannot read filesystem statistics for {dir_label}"))?;
    // On the supported 64-bit target `fsblkcnt_t` and `c_ulong` are both `u64`, so the
    // product is already the byte count and no cast (or lossy conversion) is involved.
    let available = stat.blocks_available() * stat.fragment_size();
    if available < needed {
        let needed_mb = needed / (1024 * 1024);
        let available_mb = available / (1024 * 1024);
        bail!(
            "not enough free disk space for {file_name}: needs {needed_mb} MB, \
             {available_mb} MB available in {dir_label}"
        );
    }

    // (3) Download to `<path>.part`, resuming from the existing prefix. The transfer
    // is blocking (`ureq`), so it runs on the blocking pool: partial length, the
    // cancel flag and the progress callback move in, the digest of everything that
    // ended up on disk comes back.
    let part_for_transfer = part.clone();
    let cancel_for_transfer = Arc::clone(&cancel);
    let progress_for_transfer = Arc::clone(&progress);
    let hasher = tokio::task::spawn_blocking(move || {
        transfer_to_part(
            entry,
            part_for_transfer.as_path(),
            part_len,
            &cancel_for_transfer,
            progress_for_transfer.as_ref(),
        )
    })
    .await
    .context("the model download did not run")??;

    // (4) Verify the complete artifact before it is allowed to exist.
    let digest = hex_lower(&hasher.finalize());
    let final_len = tokio::fs::metadata(part.as_path())
        .await
        .with_context(|| format!("cannot inspect the finished download {}", part.display()))?
        .len();
    if final_len != entry.size_bytes || !digest.eq_ignore_ascii_case(entry.sha256) {
        tokio::fs::remove_file(part.as_path()).await.ok();
        let expected = entry.sha256;
        bail!(
            "checksum mismatch for {file_name}: expected {expected}, got {digest}. \
             The download was rejected; retry from the model step."
        );
    }

    // (5) Publish atomically.
    tokio::fs::rename(part.as_path(), path.as_path())
        .await
        .with_context(|| {
            format!(
                "cannot move {} into place at {}",
                part.display(),
                path.display(),
            )
        })?;
    tracing::info!(file = %path.display(), "checksum ok, moved");
    Ok(path)
}

/// Stream one artifact into `<file>.part`, resuming from an existing prefix.
///
/// Blocking on purpose (`ureq` is a blocking client): this is the body of a
/// `spawn_blocking` task. Redirects are followed, a saved prefix is re-requested
/// with `Range`, and a server that cannot honour the range causes exactly one clean
/// restart. Returns the digest of everything now on disk, prefix included.
fn transfer_to_part(
    entry: &ModelEntry,
    part: &Path,
    mut part_len: u64,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<Sha256> {
    use std::io::{Read as _, Write as _};

    let url = entry.url;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("lingua/", env!("CARGO_PKG_VERSION")))
        // Artifacts only ever come from HTTPS, and no redirect may leave it.
        .https_only(true)
        // Non-2xx answers are inspected below: the `Range` handling depends on it.
        .http_status_as_error(false)
        .build()
        .into();

    let mut resumed = part_len > 0;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        // A resumed attempt continues the digest of the bytes already on disk; a
        // fresh one starts from an empty digest.
        let mut hasher = if resumed {
            // Hashing the prefix on disk is as cancellable as the transfer.
            let prefix = sha256_of_file(part, Some(cancel))
                .with_context(|| format!("cannot hash the partial download {}", part.display()))?;
            let Some(prefix) = prefix else {
                bail!("download cancelled");
            };
            tracing::info!(resumed_at = part_len, "resuming download");
            prefix
        } else {
            Sha256::new()
        };

        let mut request = agent.get(url);
        if resumed {
            request = request.header("range", format!("bytes={part_len}-"));
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("download cancelled");
        }
        let response = request
            .call()
            .with_context(|| format!("cannot start the download of {url}"))?;
        let status = response.status().as_u16();

        if resumed && status == 200 {
            // The server ignored `Range`; restart cleanly, but only once.
            if attempt > 1 {
                bail!(
                    "cannot resume the download of {url}: the server ignores byte ranges; \
                     delete {} and retry",
                    part.display(),
                );
            }
            tracing::warn!(
                file = %part.display(),
                "server answered 200 to a ranged request; restarting the download"
            );
            remove_file_if_present(part)?;
            part_len = 0;
            resumed = false;
            continue;
        }
        if resumed && status != 206 {
            // Anything else (416 for an unsatisfiable range, 403, …) means the saved
            // prefix can never be used; keeping it would wedge every later attempt.
            if attempt > 1 {
                bail!(
                    "cannot resume the download of {url}: the server answered HTTP {status} \
                     instead of 206; {} was discarded — retry",
                    part.display(),
                );
            }
            tracing::warn!(
                file = %part.display(),
                "server answered a resumed request without 206; restarting the download"
            );
            remove_file_if_present(part)?;
            part_len = 0;
            resumed = false;
            continue;
        }
        if !resumed && !(200..300).contains(&status) {
            bail!("cannot download {url}: the server answered HTTP {status}");
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(resumed)
            .write(!resumed)
            .truncate(!resumed)
            .open(part)
            .with_context(|| {
                if resumed {
                    format!("cannot open {} for appending", part.display())
                } else {
                    format!("cannot create {}", part.display())
                }
            })?;

        let mut received = if resumed { part_len } else { 0 };
        let mut last_report = Instant::now();
        report(progress, received, entry.size_bytes);
        let mut body = response.into_body().into_reader();
        let mut buffer = vec![0u8; DOWNLOAD_BUFFER_BYTES];
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("download cancelled");
            }
            let read = body
                .read(&mut buffer)
                .with_context(|| format!("the download of {url} was interrupted"))?;
            if read == 0 {
                break;
            }
            let bytes = &buffer[..read];
            hasher.update(bytes);
            file.write_all(bytes)
                .with_context(|| format!("cannot write to {}", part.display()))?;
            received += u64::try_from(read).unwrap_or(u64::MAX);
            if last_report.elapsed() >= PROGRESS_INTERVAL {
                report(progress, received, entry.size_bytes);
                last_report = Instant::now();
            }
        }
        file.flush()
            .with_context(|| format!("cannot flush {}", part.display()))?;
        report(progress, received, entry.size_bytes);
        return Ok(hasher);
    }
}

/// Stream `path` into a fresh SHA-256, in fixed-size steps (never read whole).
/// Hash `path` in `HASH_BUFFER_BYTES` steps; `None` means `cancel` was set
/// between two reads, so the caller learns about the abort within one step
/// instead of after the whole artifact.
fn sha256_of_file(path: &Path, cancel: Option<&AtomicBool>) -> Result<Option<Sha256>> {
    use std::io::Read as _;

    let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_BUFFER_BYTES];
    loop {
        if cancelled() {
            return Ok(None);
        }
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hasher))
}

/// Stream `path` and return whether its SHA-256 equals `expected_sha256`
/// (case-insensitive). `Ok(false)` means "read fine, digest differs".
/// Outcome of a checksum probe that a cancel flag can abandon.
///
/// Reading a 751 MB artifact takes seconds, so the caller — a download that the
/// user can cancel, or the engine's pre-flight — passes its flag through and
/// gets [`Verify::Cancelled`] instead of waiting the read out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// Length and digest match the catalog entry.
    Verified,
    /// The file is present but does not match: corrupt, never loadable.
    Mismatch,
    /// The cancel flag was set while hashing; the file was not touched.
    Cancelled,
}

pub fn verify_sha256(
    path: &Path,
    expected_sha256: &str,
    cancel: Option<&AtomicBool>,
) -> Result<Verify> {
    let Some(digest) = sha256_of_file(path, cancel)? else {
        return Ok(Verify::Cancelled);
    };
    if hex_lower(&digest.finalize()).eq_ignore_ascii_case(expected_sha256) {
        Ok(Verify::Verified)
    } else {
        Ok(Verify::Mismatch)
    }
}

/// Whether `path` is exactly `entry`'s artifact: the byte length must match
/// before the (much more expensive) digest is computed.
pub fn verify_matches_entry(
    entry: &ModelEntry,
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<Verify> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("cannot inspect the model file {}", path.display()))?
        .len();
    if len != entry.size_bytes {
        return Ok(Verify::Mismatch);
    }
    verify_sha256(path, entry.sha256, cancel)
}

/// Lower-case hex encoding of a digest.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Unique path in `std::env::temp_dir()`; never touches `$HOME`.
    fn temp_file(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lingua-models-{}-{tag}-{n}", std::process::id()))
    }

    fn write_temp(tag: &str, bytes: &[u8]) -> PathBuf {
        let path = temp_file(tag);
        let mut file = std::fs::File::create(path.as_path()).expect("create temp file");
        file.write_all(bytes).expect("write temp file");
        file.flush().expect("flush temp file");
        path
    }

    #[test]
    fn catalog_has_exactly_three_entries() {
        assert_eq!(CATALOG.len(), 3);
    }

    #[test]
    fn catalog_ids_are_unique_and_name_the_file() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len(), "catalog ids must be unique");
        for e in &CATALOG {
            assert_eq!(e.file_name, format!("{id}.gguf", id = e.id));
        }
    }

    #[test]
    fn catalog_text_fields_are_non_empty() {
        for e in &CATALOG {
            assert!(!e.id.is_empty());
            assert!(!e.label.is_empty());
            assert!(!e.note.is_empty());
            assert!(!e.file_name.is_empty());
            assert!(!e.approx_size_label.is_empty());
            assert!(
                e.url.starts_with("https://"),
                "url must be HTTPS: {url}",
                url = e.url
            );
            assert_eq!(e.sha256.len(), 64, "sha256 must be 64 hex chars");
            assert!(
                e.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "sha256 must be hex: {sha}",
                sha = e.sha256
            );
        }
    }

    #[test]
    fn catalog_matches_the_pinned_contract() {
        assert_eq!(CATALOG[0].id, DEFAULT_MODEL_ID);
        assert_eq!(CATALOG[0].size_bytes, 751_094_240);
        assert_eq!(CATALOG[0].approx_size_label, "751 MB");
        assert_eq!(
            CATALOG[0].sha256,
            "b94545b313b3223fda7b2857a52681da813935c2127643d1e9ff0c23d988089c"
        );
        assert!(CATALOG[0].multilingual);

        assert_eq!(CATALOG[1].size_bytes, 495_831_520);
        assert_eq!(CATALOG[1].approx_size_label, "496 MB");
        assert_eq!(
            CATALOG[1].sha256,
            "41c99fa5fb6f3d35f68e79adc3e755eca2232a8d921178bd647b71194792b8fd"
        );
        assert!(CATALOG[1].multilingual);

        assert_eq!(CATALOG[2].size_bytes, 729_650_176);
        assert_eq!(CATALOG[2].approx_size_label, "730 MB");
        assert_eq!(
            CATALOG[2].sha256,
            "90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110"
        );
        assert!(!CATALOG[2].multilingual);
    }

    #[test]
    fn entry_and_default_entry_resolve() {
        assert_eq!(entry("does-not-exist"), None);
        assert_eq!(
            entry(DEFAULT_MODEL_ID).map(|e| e.id),
            Some(DEFAULT_MODEL_ID)
        );
        for e in &CATALOG {
            assert_eq!(entry(e.id).map(|found| found.id), Some(e.id));
        }
        assert_eq!(default_entry().id, DEFAULT_MODEL_ID);
        // Value equality, not pointer identity: each use of a `const` may be a
        // distinct promoted allocation.
        assert_eq!(default_entry(), &CATALOG[0]);
    }

    #[test]
    fn path_in_ends_with_the_entry_file_name() {
        // `path_for` is `path_in(models_dir(), entry)`. Only the join is ours to test:
        // `Config::models_dir()` *creates* `<data dir>/lingua/models`, and tests must
        // never create anything below the user's `$HOME` (nor mutate `XDG_DATA_HOME`,
        // which other modules' tests read concurrently).
        let base = Path::new("/models");
        for e in &CATALOG {
            let path = path_in(base, e);
            assert!(
                path.starts_with(base),
                "{} must live below the base directory",
                path.display()
            );
            assert!(
                path.ends_with(e.file_name),
                "{} must end with {file_name}",
                path.display(),
                file_name = e.file_name
            );
        }
    }

    #[test]
    fn verify_sha256_accepts_the_correct_digest() {
        // Known-answer vector: SHA-256("abc") (FIPS 180-4 test vector).
        let path = write_temp("sha-ok", b"abc");
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        assert_eq!(
            verify_sha256(&path, expected, None).expect("verify"),
            Verify::Verified
        );
        // The comparison is case-insensitive.
        assert_eq!(
            verify_sha256(&path, &expected.to_uppercase(), None).expect("verify"),
            Verify::Verified
        );

        std::fs::remove_file(path.as_path()).ok();
    }

    #[test]
    fn verify_sha256_rejects_a_wrong_digest() {
        let path = write_temp("sha-bad", b"lingua models verification payload");
        let wrong = "0".repeat(64);

        assert_eq!(
            verify_sha256(&path, &wrong, None).expect("verify"),
            Verify::Mismatch
        );
        assert_eq!(
            verify_sha256(&path, "not-hex", None).expect("verify"),
            Verify::Mismatch
        );

        std::fs::remove_file(path.as_path()).ok();
    }

    #[test]
    fn verify_sha256_errors_on_a_missing_file() {
        let missing = temp_file("sha-missing");
        assert!(verify_sha256(&missing, &"0".repeat(64), None).is_err());
    }

    #[test]
    fn verify_sha256_reports_a_cancelled_probe_instead_of_a_mismatch() {
        // `abc`: SHA-256 from the FIPS vector, so the digest itself would match.
        let path = write_temp("sha-cancelled", b"abc");
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            verify_sha256(&path, expected, Some(&cancelled)).expect("verify"),
            Verify::Cancelled
        );
        // The same file with the flag clear verifies, and the length gate of
        // `verify_matches_entry` passes the flag through to the digest read.
        let entry = ModelEntry {
            id: "test-3-bytes",
            label: "test",
            note: "test",
            url: "https://example.invalid/test.gguf",
            file_name: "test-3-bytes.gguf",
            size_bytes: 3,
            sha256: expected,
            multilingual: false,
            approx_size_label: "3 B",
        };
        assert_eq!(
            verify_matches_entry(&entry, &path, Some(&cancelled)).expect("verify"),
            Verify::Cancelled
        );
        let running = AtomicBool::new(false);
        assert_eq!(
            verify_matches_entry(&entry, &path, Some(&running)).expect("verify"),
            Verify::Verified
        );

        std::fs::remove_file(path.as_path()).ok();
    }

    #[test]
    fn verify_matches_entry_rejects_a_wrong_length_file() {
        let entry = default_entry();
        let path = write_temp("matches-short", b"too short");
        // The length mismatch must be reported as "not a match", not as an error,
        // and must not fall through to the (much slower) digest comparison.
        assert_eq!(
            verify_matches_entry(entry, &path, None).expect("length check must succeed"),
            Verify::Mismatch
        );
        assert_eq!(
            verify_matches_entry(entry, &path, None).expect("length check must be repeatable"),
            Verify::Mismatch
        );

        std::fs::remove_file(path.as_path()).ok();
    }

    #[test]
    fn verify_matches_entry_errors_on_a_missing_file() {
        let missing = temp_file("matches-missing");
        assert!(verify_matches_entry(default_entry(), &missing, None).is_err());
    }
}
