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
) -> Result<Sha256> {
    use std::io::{Read as _, Write as _};

    let url = artifact.url;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("littera/", env!("CARGO_PKG_VERSION")))
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

/// Stream `path` and return whether its SHA-256 equals `expected_sha256`
/// (case-insensitive). `Ok(false)` means "read fine, digest differs".
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
    fn catalog_ids_are_unique_and_name_the_file() {
        // The snapshot owns the catalog now; this module only needs every id to
        // name exactly one file (its own `.gguf`).
        let all: Vec<&'static Artifact> = crate::model_catalog::artifacts()
            .map(|selection| selection.artifact)
            .collect();
        assert_eq!(all.len(), 23, "offered artifacts");

        let mut ids: Vec<&str> = all.iter().map(|artifact| artifact.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "artifact ids must be unique");

        for artifact in all {
            assert!(
                artifact.file_name.ends_with(".gguf"),
                "{} must name a GGUF file",
                artifact.file_name
            );
            assert!(!artifact.id.is_empty());
            assert!(!artifact.quant.is_empty());
            assert!(
                artifact.id.starts_with(artifact.quant) || artifact.id.contains(artifact.quant),
                "{} must name its quant",
                artifact.id
            );
            assert!(
                artifact.url.starts_with("https://"),
                "url must be HTTPS: {url}",
                url = artifact.url
            );
            assert_eq!(artifact.sha256.len(), 64, "sha256 must be 64 hex chars");
            assert!(
                artifact.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "sha256 must be hex: {sha}",
                sha = artifact.sha256
            );
        }
    }

    #[test]
    fn catalog_matches_the_pinned_contract() {
        // The three artifacts of the previous release keep their exact byte
        // length and digest, so an installed model needs no re-download (SC-006).
        const LEGACY: [(&str, u64, &str); 3] = [
            (
                "nemotron-3.5-asr-streaming-0.6b-Q8_0",
                751_094_240,
                "b94545b313b3223fda7b2857a52681da813935c2127643d1e9ff0c23d988089c",
            ),
            (
                "nemotron-3.5-asr-streaming-0.6b-Q4_K_M",
                495_831_520,
                "41c99fa5fb6f3d35f68e79adc3e755eca2232a8d921178bd647b71194792b8fd",
            ),
            (
                "nemotron-speech-streaming-en-0.6b-Q8_0",
                729_650_176,
                "90d8c89714cd31efc88be62a40c6b2bea57e0cc2063af1ffe2c28f1a228ca110",
            ),
        ];

        for (id, size, sha) in LEGACY {
            let artifact = crate::model_catalog::artifact(id)
                .unwrap_or_else(|| panic!("{id} must stay offered"))
                .artifact;
            assert_eq!(artifact.size_bytes, size, "{id}");
            assert_eq!(artifact.sha256, sha, "{id}");
            assert_eq!(artifact.file_name, format!("{id}.gguf"));
            // The label the UI shows is the formatted byte size.
            assert_eq!(
                human_size(artifact.size_bytes),
                match id {
                    "nemotron-3.5-asr-streaming-0.6b-Q4_K_M" => "496 MB",
                    "nemotron-speech-streaming-en-0.6b-Q8_0" => "730 MB",
                    _ => "751 MB",
                }
            );
        }
    }

    #[test]
    fn artifact_lookup_resolves_by_id_and_default() {
        const DEFAULT: &str = "nemotron-3.5-asr-streaming-0.6b-Q8_0";

        assert!(crate::model_catalog::artifact("does-not-exist").is_none());
        assert_eq!(
            crate::model_catalog::artifact(DEFAULT).map(|selection| selection.artifact.id),
            Some(DEFAULT)
        );
        for selection in crate::model_catalog::artifacts() {
            assert_eq!(
                crate::model_catalog::artifact(selection.artifact.id),
                Some(selection)
            );
        }
        assert_eq!(
            crate::model_catalog::default_selection().artifact.id,
            DEFAULT
        );
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
}
