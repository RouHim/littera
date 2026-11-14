//! Verified, resumable, cancellable model downloads (FR-003).
//!
//! The offered artifacts and their pinned byte sizes and SHA-256 come from the
//! bundled snapshot in [`crate::model_catalog`]; this module only moves bytes.
//! A download is written to `<file>.part` and only moved into place once the
//! byte length and the digest both match, so a corrupt or truncated artifact can
//! never be loaded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::model_catalog::Artifact;

/// Head-room kept free on top of the pending download.
const FREE_SPACE_SLACK: u64 = 64 * 1024 * 1024;
/// Minimum interval between two `progress` callbacks.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// How long the agent waits for a connection / the response headers before failing.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Per transport read: how long one body read may take before the transfer counts as
/// stalled. It restarts for every read, so it never caps the total transfer time.
const BODY_TIMEOUT: Duration = Duration::from_secs(60);
/// Read buffer used when hashing an already-downloaded `.part` prefix.
const HASH_BUFFER_BYTES: usize = 1024 * 1024;
/// Read buffer used while a download is streaming to disk.
const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;

/// On-disk path of an artifact: `<data dir>/littera/models/<file_name>` (FR-003).
pub fn path_for(artifact: &Artifact) -> Result<PathBuf> {
    Ok(path_in(
        crate::config::Config::models_dir()?.as_path(),
        artifact,
    ))
}

/// The file name join that [`path_for`] performs, with the base directory injected
/// so the join can be tested without resolving (or creating) the data directory.
fn path_in(base: &Path, artifact: &Artifact) -> PathBuf {
    base.join(artifact.file_name)
}

/// Total bytes and file count of `dir`: final models plus resumable `.part`
/// downloads. Unreadable directories and entries count as empty — the figure is
/// informational.
pub fn dir_usage(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };

    let mut bytes = 0;
    let mut files = 0;
    for entry in entries.flatten() {
        if let Ok(metadata) = entry.metadata()
            && metadata.is_file()
        {
            bytes += metadata.len();
            files += 1;
        }
    }
    (bytes, files)
}

/// Byte count in decimal units, formatted like the catalog's size labels
/// (`751_094_240` bytes → `"751 MB"`): one decimal below ten units, none above.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }

    match unit {
        0 => format!("{bytes} B"),
        _ if value < 10.0 => format!("{value:.1} {}", UNITS[unit]),
        _ => format!("{value:.0} {}", UNITS[unit]),
    }
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
    artifact: &'static Artifact,
    path: PathBuf,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Option<Verify>> {
    let shown = path.display().to_string();
    tokio::task::spawn_blocking(move || {
        if !path.exists() {
            return Ok(None);
        }
        verify_matches_entry(artifact, path.as_path(), cancel.as_deref()).map(Some)
    })
    .await
    .with_context(|| format!("cannot verify {shown}: the verification task failed"))?
}

/// Remove `path`, treating "it is already gone" as success.
pub(crate) fn remove_file_if_present(path: &Path) -> Result<()> {
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

/// Ensure `artifact` is present in the model directory, verified against its
/// pinned size and SHA-256, resuming a previous partial download when possible.
///
/// `cancel` is polled between streamed chunks; setting it aborts with
/// `"download cancelled"` while keeping `<file>.part` so a later call resumes.
/// `progress(received, total)` is invoked at most every 100 ms.
///
/// Everything blocking (creating the model directory, digest verification, the
/// free-space query, the transfer itself) runs on `tokio`'s blocking pool.
pub async fn ensure(
    artifact: &'static Artifact,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<PathBuf> {
    // `Config::models_dir()` creates `<data dir>/littera/models`, so it also runs off
    // the runtime thread (the project rule: blocking work never occupies a worker).
    let dir = tokio::task::spawn_blocking(crate::config::Config::models_dir)
        .await
        .context("cannot prepare the model directory")??;
    ensure_in(dir, artifact, cancel, progress, false).await
}

/// [`ensure`] against an explicit directory.
///
/// The directory is a parameter so the download flow can be tested without creating
/// anything below `$HOME`; `allow_plain_http` exists for the same reason (a local
/// fixture speaks HTTP).
async fn ensure_in(
    dir: PathBuf,
    artifact: &'static Artifact,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
    allow_plain_http: bool,
) -> Result<PathBuf> {
    let path = path_in(dir.as_path(), artifact);
    let file_name = artifact.file_name;

    // (1) A complete artifact is only accepted when it verifies. The whole probe
    // (existence + digest) runs on the blocking pool.
    match verify_entry_off_runtime(artifact, path.clone(), Some(Arc::clone(&cancel))).await? {
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
    if part_len > artifact.size_bytes {
        tracing::warn!(
            file = %part.display(),
            "partial download is larger than expected; discarding it"
        );
        remove_file_if_present_off_runtime(part.as_path()).await?;
        part_len = 0;
    }
    if part_len == artifact.size_bytes {
        let part_verified =
            match verify_entry_off_runtime(artifact, part.clone(), Some(Arc::clone(&cancel)))
                .await?
            {
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
            report(progress.as_ref(), artifact.size_bytes, artifact.size_bytes);
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
    let needed = artifact
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
            artifact,
            part_for_transfer.as_path(),
            part_len,
            &cancel_for_transfer,
            progress_for_transfer.as_ref(),
            allow_plain_http,
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
    if final_len != artifact.size_bytes || !digest.eq_ignore_ascii_case(artifact.sha256) {
        tokio::fs::remove_file(part.as_path()).await.ok();
        let expected = artifact.sha256;
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
    artifact: &Artifact,
    part: &Path,
    mut part_len: u64,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
    allow_plain_http: bool,
) -> Result<Sha256> {
    use std::io::{Read as _, Write as _};

    let url = artifact.url;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("littera/", env!("CARGO_PKG_VERSION")))
        // A stalled CDN must fail the download instead of hanging the blocking pool.
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(BODY_TIMEOUT))
        // Artifacts only ever come from HTTPS, and no redirect may leave it. The
        // local test fixture opts out explicitly.
        .https_only(!allow_plain_http)
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
        report(progress, received, artifact.size_bytes);
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
                report(progress, received, artifact.size_bytes);
                last_report = Instant::now();
            }
        }
        file.flush()
            .with_context(|| format!("cannot flush {}", part.display()))?;
        report(progress, received, artifact.size_bytes);
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

/// Stream `path` and compare its SHA-256 against `expected_sha256`
/// (case-insensitive). [`Verify::Mismatch`] means "read fine, digest differs".
/// Outcome of a checksum probe that a cancel flag can abandon.
///
/// Reading a 751 MB artifact takes seconds, so the caller — a download that the
/// user can cancel, or the engine's pre-flight — passes its flag through and
/// gets [`Verify::Cancelled`] instead of waiting the read out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// Length and digest match the snapshot artifact.
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

/// Whether `path` is exactly `artifact`'s file: the byte length must match
/// before the (much more expensive) digest is computed.
pub fn verify_matches_entry(
    artifact: &Artifact,
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<Verify> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("cannot inspect the model file {}", path.display()))?
        .len();
    if len != artifact.size_bytes {
        return Ok(Verify::Mismatch);
    }
    verify_sha256(path, artifact.sha256, cancel)
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
        std::env::temp_dir().join(format!("littera-models-{}-{tag}-{n}", std::process::id()))
    }

    fn write_temp(tag: &str, bytes: &[u8]) -> PathBuf {
        let path = temp_file(tag);
        let mut file = std::fs::File::create(path.as_path()).expect("create temp file");
        file.write_all(bytes).expect("write temp file");
        file.flush().expect("flush temp file");
        path
    }

    #[test]
    fn path_in_ends_with_the_entry_file_name() {
        // `path_for` is `path_in(models_dir(), artifact)`. Only the join is ours to
        // test: `Config::models_dir()` *creates* `<data dir>/littera/models`, and tests
        // must never create anything below the user's `$HOME` (nor mutate
        // `XDG_DATA_HOME`, which other modules' tests read concurrently).
        let base = Path::new("/models");
        for selection in crate::model_catalog::artifacts() {
            let artifact = selection.artifact;
            let path = path_in(base, artifact);
            assert!(
                path.starts_with(base),
                "{} must live below the base directory",
                path.display()
            );
            assert!(
                path.ends_with(artifact.file_name),
                "{} must end with {file_name}",
                path.display(),
                file_name = artifact.file_name
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
        let path = write_temp("sha-bad", b"littera models verification payload");
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
        const THREE_BYTES: Artifact = Artifact {
            id: "test-3-bytes",
            quant: "Q8_0",
            file_name: "test-3-bytes.gguf",
            url: "https://example.invalid/test-3-bytes.gguf",
            size_bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert_eq!(
            verify_matches_entry(&THREE_BYTES, &path, Some(&cancelled)).expect("verify"),
            Verify::Cancelled
        );
        let running = AtomicBool::new(false);
        assert_eq!(
            verify_matches_entry(&THREE_BYTES, &path, Some(&running)).expect("verify"),
            Verify::Verified
        );

        std::fs::remove_file(path.as_path()).ok();
    }

    #[test]
    fn verify_matches_entry_rejects_a_wrong_length_file() {
        let artifact = crate::model_catalog::default_selection().artifact;
        let path = write_temp("matches-short", b"too short");
        // The length mismatch must be reported as "not a match", not as an error,
        // and must not fall through to the (much slower) digest comparison.
        assert_eq!(
            verify_matches_entry(artifact, &path, None).expect("length check must succeed"),
            Verify::Mismatch
        );
        assert_eq!(
            verify_matches_entry(artifact, &path, None).expect("length check must be repeatable"),
            Verify::Mismatch
        );

        std::fs::remove_file(path.as_path()).ok();
    }

    #[test]
    fn verify_matches_entry_errors_on_a_missing_file() {
        let missing = temp_file("matches-missing");
        let artifact = crate::model_catalog::default_selection().artifact;
        assert!(verify_matches_entry(artifact, &missing, None).is_err());
    }

    #[test]
    fn human_size_matches_the_catalog_labels() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(500), "500 B");
        assert_eq!(human_size(2_500), "2.5 KB");
        assert_eq!(human_size(10_000), "10 KB");
        assert_eq!(human_size(1_610_612_736), "1.6 GB");
        // The largest offered artifact (Voxtral Q8_0); its label stays in the
        // two-decimal band.
        assert_eq!(human_size(4_731_791_648), "4.7 GB");
        assert_eq!(human_size(50_462_816), "50 MB");
    }

    #[test]
    fn dir_usage_sums_files_and_ignores_missing_directories() {
        let dir = temp_file("usage");
        std::fs::create_dir_all(&dir).expect("create the temp directory");
        std::fs::write(dir.join("model.gguf"), vec![0u8; 3]).expect("write the model stub");
        std::fs::write(dir.join("model.gguf.part"), vec![0u8; 5]).expect("write the partial");

        assert_eq!(dir_usage(&dir), (8, 2));
        assert_eq!(dir_usage(&dir.join("missing")), (0, 0));

        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let path = temp_file(tag);
        std::fs::create_dir_all(path.as_path()).expect("create temp dir");
        path
    }

    /// Leak a test artifact: the production type stores `'static` catalog data.
    fn artifact_for(url: &str, bytes: &[u8]) -> &'static Artifact {
        let sha = hex_lower(&Sha256::digest(bytes));
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed);
        let file_name: &'static str =
            Box::leak(format!("littera-fixture-{tag}.gguf").into_boxed_str());
        let url: &'static str = Box::leak(url.to_owned().into_boxed_str());
        let sha256: &'static str = Box::leak(sha.into_boxed_str());
        Box::leak(Box::new(Artifact {
            id: "fixture",
            quant: "Q8_0",
            file_name,
            url,
            size_bytes: bytes.len() as u64,
            sha256,
        }))
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    /// One-shot HTTP fixture; every response is written to its own connection and the
    /// captured requests are returned for assertions.
    fn http_server(
        responses: Vec<String>,
    ) -> (
        String,
        Arc<std::sync::Mutex<Vec<String>>>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture");
        let addr = listener.local_addr().expect("fixture address");
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0u8; 4096];
                let read = socket.read(&mut buffer).unwrap_or(0);
                captured
                    .lock()
                    .expect("requests")
                    .push(String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase());
                let _ = socket.write_all(response.as_bytes());
            }
        });
        (format!("http://{addr}/model.gguf"), requests, handle)
    }

    fn response(status: &str, body: &[u8]) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        )
    }

    #[test]
    fn a_fresh_download_is_verified_and_written() {
        let bytes = b"fixture-model-body";
        let (url, _requests, server) = http_server(vec![response("200 OK", bytes)]);
        let dir = temp_dir("download");
        let part = dir.join("model.gguf.part");

        let digest = transfer_to_part(
            artifact_for(&url, bytes),
            part.as_path(),
            0,
            &AtomicBool::new(false),
            &|_, _| {},
            true,
        )
        .expect("download");
        let _ = server.join();

        assert_eq!(std::fs::read(part.as_path()).expect("part"), bytes);
        assert_eq!(
            hex_lower(&digest.finalize()),
            hex_lower(&Sha256::digest(bytes))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_ranged_request_resumes_from_the_partial_file() {
        let bytes = b"0123456789";
        let (url, requests, server) =
            http_server(vec![response("206 Partial Content", &bytes[4..])]);
        let dir = temp_dir("resume");
        let part = dir.join("model.gguf.part");
        std::fs::write(part.as_path(), &bytes[..4]).expect("prefix");

        let digest = transfer_to_part(
            artifact_for(&url, bytes),
            part.as_path(),
            4,
            &AtomicBool::new(false),
            &|_, _| {},
            true,
        )
        .expect("resume");
        let _ = server.join();

        assert_eq!(std::fs::read(part.as_path()).expect("part"), bytes);
        assert_eq!(
            hex_lower(&digest.finalize()),
            hex_lower(&Sha256::digest(bytes))
        );
        assert!(
            requests.lock().expect("requests")[0].contains("range: bytes=4-"),
            "the resumed request must ask for the missing suffix"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_server_that_ignores_the_range_causes_one_clean_restart() {
        let bytes = b"0123456789";
        let (url, requests, server) =
            http_server(vec![response("200 OK", bytes), response("200 OK", bytes)]);
        let dir = temp_dir("restart");
        let part = dir.join("model.gguf.part");
        std::fs::write(part.as_path(), &bytes[..4]).expect("prefix");

        let digest = transfer_to_part(
            artifact_for(&url, bytes),
            part.as_path(),
            4,
            &AtomicBool::new(false),
            &|_, _| {},
            true,
        )
        .expect("restart");
        let _ = server.join();

        assert_eq!(std::fs::read(part.as_path()).expect("part"), bytes);
        assert_eq!(
            hex_lower(&digest.finalize()),
            hex_lower(&Sha256::digest(bytes))
        );
        assert_eq!(requests.lock().expect("requests").len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_oversized_partial_file_is_discarded_before_downloading() {
        let bytes = b"0123456789";
        let artifact = artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        let dir = temp_dir("oversized");
        let part = dir.join(format!("{}.part", artifact.file_name));
        std::fs::write(part.as_path(), b"much longer than the artifact").expect("part");

        let result = block_on(ensure_in(
            dir.clone(),
            artifact,
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_, _| {}),
            true,
        ));
        let _ = result;

        assert!(!part.exists(), "an oversized prefix must be discarded");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_complete_partial_file_is_verified_and_published() {
        let bytes = b"complete-fixture";
        let artifact = artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        let dir = temp_dir("complete-part");
        let part = dir.join(format!("{}.part", artifact.file_name));
        std::fs::write(part.as_path(), bytes).expect("part");

        let path = block_on(ensure_in(
            dir.clone(),
            artifact,
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_, _| {}),
            true,
        ))
        .expect("publish the verified part");

        assert_eq!(std::fs::read(path.as_path()).expect("model"), bytes);
        assert!(!part.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_verified_file_short_circuits_without_touching_the_network() {
        let bytes = b"already-here";
        let artifact = artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        let dir = temp_dir("already-there");
        std::fs::write(dir.join(artifact.file_name), bytes).expect("model");

        let path = block_on(ensure_in(
            dir.clone(),
            artifact,
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_, _| {}),
            true,
        ))
        .expect("verified file");

        assert_eq!(path, dir.join(artifact.file_name));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelling_verification_reports_the_cancel_and_keeps_the_file() {
        let bytes = b"cancel-me";
        let artifact = artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        let dir = temp_dir("cancel");
        let path = dir.join(artifact.file_name);
        std::fs::write(path.as_path(), bytes).expect("model");
        let cancel = Arc::new(AtomicBool::new(true));

        let error = block_on(ensure_in(
            dir.clone(),
            artifact,
            cancel,
            Arc::new(|_, _| {}),
            true,
        ))
        .expect_err("a cancelled verification must not proceed");

        assert!(format!("{error:#}").contains("download cancelled"));
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_free_space_gate_refuses_an_impossible_download() {
        let bytes = b"x";
        let mut artifact = *artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        artifact.size_bytes = u64::MAX / 2;
        let artifact: &'static Artifact = Box::leak(Box::new(artifact));
        let dir = temp_dir("space");

        let error = block_on(ensure_in(
            dir.clone(),
            artifact,
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_, _| {}),
            true,
        ))
        .expect_err("free space must gate the download");

        assert!(format!("{error:#}").contains("not enough free disk space"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tampered_file_is_removed_before_downloading() {
        let bytes = b"trusted-model";
        let artifact = artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        let dir = temp_dir("tamper");
        let path = dir.join(artifact.file_name);
        std::fs::write(path.as_path(), b"tampered-mode").expect("tampered model");

        let error = block_on(ensure_in(
            dir.clone(),
            artifact,
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_, _| {}),
            true,
        ))
        .expect_err("a digest mismatch must trigger a re-download");

        assert!(!path.exists(), "the corrupt file must be deleted");
        assert!(format!("{error:#}").contains("cannot start the download"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelling_a_transfer_keeps_the_partial_file() {
        let bytes = b"0123456789abcdef";
        let artifact = artifact_for("http://127.0.0.1:1/model.gguf", bytes);
        let dir = temp_dir("cancel-part");
        let part = dir.join(format!("{}.part", artifact.file_name));
        std::fs::write(part.as_path(), &bytes[..4]).expect("partial download");

        let error = block_on(ensure_in(
            dir.clone(),
            artifact,
            Arc::new(AtomicBool::new(true)),
            Arc::new(|_, _| {}),
            true,
        ))
        .expect_err("a cancelled transfer must abort");

        assert!(format!("{error:#}").contains("download cancelled"));
        assert_eq!(std::fs::read(part.as_path()).expect("part"), &bytes[..4]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
