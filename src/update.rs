//! Startup update check and the in-place update for binaries Littera owns
//! (FR-023 … FR-028).
//!
//! One blocking pass runs per daemon start on the blocking pool: ask GitHub for
//! the latest release, compare it with the running version, decide whether this
//! binary may be replaced, and either install the published `littera-x86_64`
//! asset or report the release for the transient notice. Nothing is persisted
//! and no retry runs in the same process.
//!
//! A binary is replaceable only when Littera can honestly claim it: no package
//! manager owns it, it lives under `$HOME`, it is not a cargo build artifact,
//! and both the file and its directory are writable. `/usr/bin/littera` belongs
//! to pacman or dpkg on every shipped install route, so replacing it would lie
//! to a package database the next upgrade silently corrects.
//!
//! The replacement is verified before it is allowed to exist: the published
//! bare-digest checksum (and GitHub's per-asset digest, when present) must
//! match, the file must report the expected version through `--version`, and
//! only then is it renamed over the running binary in the same directory. The
//! running process keeps its old image; the next start uses the new file.

use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::models::{Verify, remove_file_if_present, verify_sha256};

/// Latest-release endpoint of the project (FR-023).
const API_URL: &str = "https://api.github.com/repos/RouHim/littera/releases/latest";
/// The standalone binary the release job publishes next to the tarball (FR-026).
const BIN_ASSET: &str = "littera-x86_64";
/// Its checksum sibling: the bare digest every release asset ships (FR-026).
const SUM_ASSET: &str = "littera-x86_64.sha256";
/// A check that takes longer than this counts as offline (FR-023).
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest release document accepted from the API.
const RELEASE_BODY_LIMIT: u64 = 1024 * 1024;
/// Largest checksum file accepted.
const SUM_BODY_LIMIT: u64 = 4096;
/// Read buffer while a binary streams to disk.
const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;
/// Executable mode of the installed binary.
const INSTALLED_MODE: u32 = 0o755;

/// The transient notices (FR-027); `{version}` is substituted at runtime.
const NOTICE_INSTALLED: &str = "Littera {version} installed — restart to apply";
const NOTICE_PACMAN: &str =
    "Littera {version} is available — update via your AUR helper (yay -Syu or paru -Syu)";
const NOTICE_DPKG: &str =
    "Littera {version} is available — run: sudo apt install --only-upgrade littera";
const NOTICE_RELEASE_PAGE: &str =
    "Littera {version} is available — download it from the release page";
const NOTICE_SOURCE_BUILD: &str =
    "Littera {version} is available — rebuild with cargo build --release";

/// The package manager that owns the running binary, if any (FR-025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    Pacman,
    Dpkg,
}

/// Write access the update needs: to the file itself and to its directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Writable {
    exe: bool,
    parent: bool,
}

/// Why the running binary may not be replaced (FR-025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Managed(Package),
    BuildArtifact,
    OutsideHome,
    NotWritable,
}

/// How the user should get the new release, per installation route (FR-025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    Package(Package),
    ReleasePage,
    SourceBuild,
}

/// What one startup pass found (FR-027).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Available { version: String, remedy: Remedy },
    Installed { version: String },
}

/// The transient notice for an outcome, or `None` when there is nothing to say
/// (FR-027).
pub fn notice(outcome: &Outcome) -> Option<String> {
    let (template, version) = match outcome {
        Outcome::UpToDate => return None,
        Outcome::Available { version, remedy } => (
            match remedy {
                Remedy::Package(Package::Pacman) => NOTICE_PACMAN,
                Remedy::Package(Package::Dpkg) => NOTICE_DPKG,
                Remedy::ReleasePage => NOTICE_RELEASE_PAGE,
                Remedy::SourceBuild => NOTICE_SOURCE_BUILD,
            },
            version,
        ),
        Outcome::Installed { version } => (NOTICE_INSTALLED, version),
    };
    Some(template.replace("{version}", version))
}

/// One startup pass, off the iced runtime (FR-023).
///
/// Errors are for the log only: an offline machine, a rate-limited API or a
/// failed install must not disturb dictation or show a notice (FR-023).
pub async fn run_off_runtime() -> std::result::Result<Outcome, String> {
    let current = env!("CARGO_PKG_VERSION").to_owned();
    let outcome = tokio::task::spawn_blocking(move || run(&current))
        .await
        .map_err(|error| format!("the update task did not run: {error}"))?;
    outcome.map_err(|error| format!("{error:#}"))
}

/// Check once and, when allowed, replace the running binary (FR-023 … FR-026).
pub fn run(current: &str) -> Result<Outcome> {
    let Some(found) = check(current)? else {
        return Ok(Outcome::UpToDate);
    };
    let version = &found.version;

    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let home = home_dir();
    let owner = package_owner(&exe);
    if let Some(refusal) = refusal(&exe, home.as_deref(), writable(&exe), owner) {
        tracing::info!(
            %version,
            ?refusal,
            "a newer release exists; leaving this binary alone"
        );
        return Ok(Outcome::Available {
            remedy: remedy(refusal),
            version: version.clone(),
        });
    }

    let Some(asset) = asset_named(&found.release, BIN_ASSET) else {
        tracing::warn!(%version, "the release publishes no {BIN_ASSET} asset");
        return Ok(Outcome::Available {
            version: version.clone(),
            remedy: Remedy::ReleasePage,
        });
    };

    install(&found, asset, &exe)?;
    Ok(Outcome::Installed {
        version: version.clone(),
    })
}

/// The pure ownership decision (FR-025): `None` means Littera may replace the
/// binary. Package ownership wins over every other reason, because that is the
/// remedy the user needs to see.
fn refusal(
    exe: &Path,
    home: Option<&Path>,
    writable: Writable,
    owner: Option<Package>,
) -> Option<Refusal> {
    if let Some(package) = owner {
        return Some(Refusal::Managed(package));
    }
    if exe
        .components()
        .any(|part| part.as_os_str() == std::ffi::OsStr::new("target"))
    {
        return Some(Refusal::BuildArtifact);
    }
    match home {
        Some(home) if exe.starts_with(home) => {}
        _ => return Some(Refusal::OutsideHome),
    }
    if !writable.exe || !writable.parent {
        return Some(Refusal::NotWritable);
    }
    None
}

/// The upgrade command for a refusal (FR-025).
const fn remedy(refusal: Refusal) -> Remedy {
    match refusal {
        Refusal::Managed(package) => Remedy::Package(package),
        Refusal::BuildArtifact => Remedy::SourceBuild,
        Refusal::OutsideHome | Refusal::NotWritable => Remedy::ReleasePage,
    }
}

/// Ask for the latest release; only a strictly newer, parseable tag counts
/// (FR-023/FR-024).
fn check(current: &str) -> Result<Option<Found>> {
    let agent = agent(Some(CHECK_TIMEOUT));
    let body = fetch_text(
        &agent,
        API_URL,
        RELEASE_BODY_LIMIT,
        Some("application/vnd.github+json"),
    )?;
    let release: Release =
        serde_json::from_str(&body).context("cannot parse the release document")?;
    let Some(version) = release.version() else {
        tracing::info!(tag = %release.tag_name, "ignoring a release tag that is not vX.Y.Z");
        return Ok(None);
    };
    if !is_newer(current, &version) {
        return Ok(None);
    }
    Ok(Some(Found { release, version }))
}

/// Download, verify and swap the published binary (FR-026).
///
/// The temporary file lives next to the running binary so the final rename
/// stays on one filesystem. Any failure removes it and leaves the old binary
/// untouched.
fn install(found: &Found, asset: &Asset, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .context("the running binary has no parent directory")?;
    let temp = dir.join(format!(".littera-update-{}.tmp", std::process::id()));
    remove_file_if_present(&temp)?;

    let result = replace_with(&temp, found, asset, exe);
    if result.is_err() {
        remove_file_if_present(&temp).ok();
    }
    result
}

fn replace_with(temp: &Path, found: &Found, asset: &Asset, exe: &Path) -> Result<()> {
    download_binary(&agent(None), &asset.browser_download_url, temp)?;
    verify_digest(&agent(Some(CHECK_TIMEOUT)), found, asset, temp)?;
    std::fs::set_permissions(temp, std::fs::Permissions::from_mode(INSTALLED_MODE))
        .with_context(|| format!("cannot make {} executable", temp.display()))?;
    prove_version(temp, &found.version)?;
    std::fs::rename(temp, exe).with_context(|| {
        format!(
            "cannot move the verified update {} over {}",
            temp.display(),
            exe.display(),
        )
    })?;
    tracing::info!(version = %found.version, file = %exe.display(), "updated in place");
    Ok(())
}

/// The downloaded file must run and report exactly the release version — proof
/// that the digest matched a working build (FR-026).
fn prove_version(binary: &Path, version: &str) -> Result<()> {
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run the downloaded binary {}", binary.display()))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    let expected = format!("littera {version}");
    if !output.status.success() || printed.trim() != expected {
        bail!(
            "the downloaded binary reported {:?} instead of {expected:?}",
            printed.trim(),
        );
    }
    Ok(())
}

/// Verify the download against the published checksum file and, when GitHub
/// publishes one, the per-asset digest; both must agree and must match
/// (FR-026).
fn verify_digest(agent: &ureq::Agent, found: &Found, asset: &Asset, file: &Path) -> Result<()> {
    let sums = match asset_named(&found.release, SUM_ASSET) {
        Some(sums) => Some(fetch_text(
            agent,
            &sums.browser_download_url,
            SUM_BODY_LIMIT,
            None,
        )?),
        None => None,
    };
    let expected = expected_digest(sums.as_deref(), asset.digest.as_deref())?;

    let verified = verify_sha256(file, &expected, None)
        .with_context(|| format!("cannot hash {}", file.display()))?;
    if verified != Verify::Verified {
        bail!("checksum mismatch for {BIN_ASSET}: the published digest is {expected}");
    }
    Ok(())
}

/// The digest the release publishes for the binary: the mandatory `.sha256`
/// file, cross-checked against GitHub's per-asset digest when both exist
/// (FR-026).
fn expected_digest(sums: Option<&str>, published: Option<&str>) -> Result<String> {
    let sums = sums.context("the release publishes no checksum file for the binary")?;
    let expected =
        parse_sha256(sums).context("the published checksum file carries no SHA-256 digest")?;
    if let Some(published) = published.and_then(parse_published_digest)
        && !published.eq_ignore_ascii_case(&expected)
    {
        bail!("the release publishes conflicting SHA-256 digests for {BIN_ASSET}");
    }
    Ok(expected)
}

/// Stream the asset to `destination` (FR-026).
fn download_binary(agent: &ureq::Agent, url: &str, destination: &Path) -> Result<()> {
    use std::io::Write as _;

    let response = agent
        .get(url)
        .call()
        .with_context(|| format!("cannot download {url}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("cannot download {url}: the server answered HTTP {status}");
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(destination)
        .with_context(|| format!("cannot create {}", destination.display()))?;
    let mut body = response.into_body().into_reader();
    let mut buffer = vec![0u8; DOWNLOAD_BUFFER_BYTES];
    loop {
        let read = body
            .read(&mut buffer)
            .with_context(|| format!("the download of {url} was interrupted"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("cannot write to {}", destination.display()))?;
    }
    file.flush()
        .with_context(|| format!("cannot flush {}", destination.display()))
}

/// The digest GitHub publishes per asset, `algorithm:hex` (FR-026).
fn parse_published_digest(value: &str) -> Option<String> {
    let (algorithm, hex) = value.split_once(':')?;
    algorithm
        .eq_ignore_ascii_case("sha256")
        .then(|| parse_sha256(hex))
        .flatten()
}

/// A bare or `<digest>  <name>` checksum file, as `sha256sum` writes it.
fn parse_sha256(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?.trim_start_matches('*');
    (token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| token.to_ascii_lowercase())
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("littera/", env!("CARGO_PKG_VERSION")))
        // Release assets only ever come from HTTPS, and no redirect may leave it.
        .https_only(true)
        // Non-2xx answers are inspected by the callers.
        .http_status_as_error(false)
        .timeout_global(timeout)
        .build()
        .into()
}

fn fetch_text(agent: &ureq::Agent, url: &str, limit: u64, accept: Option<&str>) -> Result<String> {
    let mut request = agent.get(url);
    if let Some(accept) = accept {
        request = request.header("accept", accept);
    }
    let mut response = request
        .call()
        .with_context(|| format!("cannot fetch {url}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        bail!("cannot fetch {url}: the server answered HTTP {status}");
    }
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .with_context(|| format!("cannot read the response from {url}"))
}

fn asset_named<'a>(release: &'a Release, name: &str) -> Option<&'a Asset> {
    release.assets.iter().find(|asset| asset.name == name)
}

/// `$HOME`, resolved so a symlinked home still matches `current_exe()`.
fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    std::fs::canonicalize(home).ok()
}

fn writable(exe: &Path) -> Writable {
    let allowed = |path: &Path| nix::unistd::access(path, nix::unistd::AccessFlags::W_OK).is_ok();
    Writable {
        exe: allowed(exe),
        parent: exe.parent().is_some_and(allowed),
    }
}

/// The package that owns `exe`, from the two managers the shipped routes use
/// (FR-025).
fn package_owner(exe: &Path) -> Option<Package> {
    if owns("pacman", "-Qo", exe) {
        return Some(Package::Pacman);
    }
    if owns("dpkg", "-S", exe) {
        return Some(Package::Dpkg);
    }
    None
}

fn owns(binary: &str, flag: &str, exe: &Path) -> bool {
    Command::new(binary)
        .arg(flag)
        .arg(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn is_newer(current: &str, release: &str) -> bool {
    match (parse_version(current), parse_version(release)) {
        (Some(current), Some(release)) => release > current,
        _ => false,
    }
}

fn parse_version(tag: &str) -> Option<Version> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let mut parts = tag.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

impl Release {
    fn version(&self) -> Option<String> {
        let tag = self.tag_name.strip_prefix('v')?;
        parse_version(tag).map(|version| version.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// A release that is strictly newer than the running version.
#[derive(Debug)]
struct Found {
    release: Release,
    version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE_JSON: &str = r#"{
        "tag_name": "v1.2.0",
        "assets": [
            {"name": "littera-1.2.0-x86_64.tar.gz", "browser_download_url": "https://example.invalid/tarball", "digest": null},
            {"name": "littera_1.2.0_amd64.deb", "browser_download_url": "https://example.invalid/deb"},
            {"name": "littera-x86_64", "browser_download_url": "https://example.invalid/bin", "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
            {"name": "littera-x86_64.sha256", "browser_download_url": "https://example.invalid/bin.sha256"}
        ]
    }"#;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // The contract sentences, pinned verbatim (FR-027).
    const PACMAN_NOTICE: &str =
        "Littera 1.2.0 is available — update via your AUR helper (yay -Syu or paru -Syu)";
    const DPKG_NOTICE: &str =
        "Littera 1.2.0 is available — run: sudo apt install --only-upgrade littera";
    const RELEASE_PAGE_NOTICE: &str =
        "Littera 1.2.0 is available — download it from the release page";
    const SOURCE_BUILD_NOTICE: &str =
        "Littera 1.2.0 is available — rebuild with cargo build --release";
    const INSTALLED_NOTICE: &str = "Littera 1.2.0 installed — restart to apply";

    fn home() -> &'static Path {
        Path::new("/home/u")
    }

    fn writable() -> Writable {
        Writable {
            exe: true,
            parent: true,
        }
    }

    #[test]
    fn newer_tags_compare_semantically_and_unparsable_tags_are_ignored() {
        assert!(is_newer("1.9.9", "1.10.0"));
        assert!(is_newer("1.1.0", "v1.1.1"));
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(!is_newer("1.1.0", "1.0.9"));
        assert!(!is_newer("1.1.0", "1.2"));
        assert!(!is_newer("1.1.0", "1.2.3-rc.1"));
        assert!(!is_newer("1.1.0", "release-2"));
        assert_eq!(
            parse_version("v1.2.0").map(|version| version.to_string()),
            Some("1.2.0".to_owned())
        );
    }

    #[test]
    fn release_document_yields_the_binary_and_its_checksum_sibling_by_exact_name() {
        let release: Release = serde_json::from_str(RELEASE_JSON).expect("fixture parses");
        assert_eq!(release.version().as_deref(), Some("1.2.0"));

        let binary = asset_named(&release, BIN_ASSET).expect("binary asset");
        assert_eq!(binary.browser_download_url, "https://example.invalid/bin");
        assert_eq!(
            binary.digest.as_deref(),
            Some(&format!("sha256:{DIGEST}")[..])
        );

        assert!(asset_named(&release, SUM_ASSET).is_some());
        assert!(
            asset_named(&release, "littera").is_none(),
            "the tarball and the .deb must never be selected as the binary"
        );
    }

    #[test]
    fn checksum_files_parse_bare_digests_sums_lines_and_reject_anything_else() {
        assert_eq!(parse_sha256(DIGEST), Some(DIGEST.to_owned()));
        assert_eq!(
            parse_sha256(&format!("{}\n", DIGEST.to_ascii_uppercase())),
            Some(DIGEST.to_owned())
        );
        assert_eq!(
            parse_sha256(&format!("{DIGEST}  littera-x86_64\n")),
            Some(DIGEST.to_owned())
        );
        assert_eq!(parse_sha256("not a digest"), None);
        assert_eq!(parse_sha256(""), None);
        assert_eq!(
            parse_published_digest(&format!("sha256:{DIGEST}")),
            Some(DIGEST.to_owned())
        );
        assert_eq!(parse_published_digest("sha512:abc"), None);
        assert_eq!(parse_published_digest("md5:abc"), None);
    }

    #[test]
    fn package_ownership_is_refused_before_any_other_reason() {
        assert_eq!(
            refusal(
                Path::new("/usr/bin/littera"),
                Some(home()),
                Writable {
                    exe: false,
                    parent: false,
                },
                Some(Package::Pacman),
            ),
            Some(Refusal::Managed(Package::Pacman))
        );
        assert_eq!(
            refusal(
                Path::new("/usr/bin/littera"),
                Some(home()),
                writable(),
                Some(Package::Dpkg),
            ),
            Some(Refusal::Managed(Package::Dpkg))
        );
    }

    #[test]
    fn cargo_build_artifacts_are_refused_even_when_writable() {
        assert_eq!(
            refusal(
                Path::new("/home/u/src/littera/target/release/littera"),
                Some(home()),
                writable(),
                None,
            ),
            Some(Refusal::BuildArtifact)
        );
    }

    #[test]
    fn paths_outside_home_are_refused() {
        assert_eq!(
            refusal(
                Path::new("/usr/bin/littera"),
                Some(home()),
                writable(),
                None
            ),
            Some(Refusal::OutsideHome)
        );
        assert_eq!(
            refusal(
                Path::new("/home/u/.local/bin/littera"),
                None,
                writable(),
                None
            ),
            Some(Refusal::OutsideHome),
            "an unresolvable HOME must not authorise a write"
        );
    }

    #[test]
    fn a_home_install_needs_write_access_to_the_file_and_its_directory() {
        let exe = Path::new("/home/u/.local/bin/littera");
        assert_eq!(
            refusal(
                exe,
                Some(home()),
                Writable {
                    exe: false,
                    parent: true,
                },
                None,
            ),
            Some(Refusal::NotWritable)
        );
        assert_eq!(
            refusal(
                exe,
                Some(home()),
                Writable {
                    exe: true,
                    parent: false,
                },
                None,
            ),
            Some(Refusal::NotWritable)
        );
        assert_eq!(
            refusal(exe, Some(home()), writable(), None),
            None,
            "a writable, unmanaged binary under HOME is Littera's to replace"
        );
    }

    #[test]
    fn remedies_name_the_package_manager_or_the_source_build() {
        assert_eq!(
            remedy(Refusal::Managed(Package::Pacman)),
            Remedy::Package(Package::Pacman)
        );
        assert_eq!(
            remedy(Refusal::Managed(Package::Dpkg)),
            Remedy::Package(Package::Dpkg)
        );
        assert_eq!(remedy(Refusal::BuildArtifact), Remedy::SourceBuild);
        assert_eq!(remedy(Refusal::OutsideHome), Remedy::ReleasePage);
        assert_eq!(remedy(Refusal::NotWritable), Remedy::ReleasePage);
    }

    #[test]
    fn notices_state_the_available_version_and_the_route_specific_command() {
        let available = |remedy| Outcome::Available {
            version: "1.2.0".to_owned(),
            remedy,
        };
        assert_eq!(
            notice(&available(Remedy::Package(Package::Pacman))).as_deref(),
            Some(PACMAN_NOTICE)
        );
        assert_eq!(
            notice(&available(Remedy::Package(Package::Dpkg))).as_deref(),
            Some(DPKG_NOTICE)
        );
        assert_eq!(
            notice(&available(Remedy::ReleasePage)).as_deref(),
            Some(RELEASE_PAGE_NOTICE)
        );
        assert_eq!(
            notice(&available(Remedy::SourceBuild)).as_deref(),
            Some(SOURCE_BUILD_NOTICE)
        );
        assert_eq!(
            notice(&Outcome::Installed {
                version: "1.2.0".to_owned()
            })
            .as_deref(),
            Some(INSTALLED_NOTICE)
        );
        assert_eq!(notice(&Outcome::UpToDate), None);
    }

    #[test]
    fn the_published_checksum_is_mandatory_and_must_agree_with_the_api_digest() {
        let published = format!("sha256:{DIGEST}");
        assert_eq!(
            expected_digest(Some(DIGEST), None).ok(),
            Some(DIGEST.to_owned())
        );
        assert_eq!(
            expected_digest(Some(DIGEST), Some(&published)).ok(),
            Some(DIGEST.to_owned())
        );
        assert!(
            expected_digest(None, Some(&published)).is_err(),
            "GitHub's per-asset digest alone must not replace the published file"
        );
        assert!(expected_digest(None, None).is_err());
        assert!(expected_digest(Some("not a digest"), None).is_err());
        let other = "f".repeat(64);
        assert!(
            expected_digest(Some(DIGEST), Some(&format!("sha256:{other}"))).is_err(),
            "two disagreeing published digests must abort the install"
        );
    }

    #[test]
    fn release_tags_must_carry_the_v_prefix() {
        let mut release: Release = serde_json::from_str(RELEASE_JSON).expect("fixture parses");
        assert_eq!(release.version().as_deref(), Some("1.2.0"));
        release.tag_name = "1.2.0".to_owned();
        assert_eq!(
            release.version(),
            None,
            "semantic-release tags are vX.Y.Z; a bare tag is not a release tag"
        );
    }

    /// Live smoke: the endpoint answers with a release this module can parse.
    /// `#[ignore]` because it needs the network, like the other live tests.
    #[test]
    #[ignore = "live: asks the GitHub releases API"]
    fn live_release_endpoint_parses_into_a_version() {
        let found = check("0.0.0")
            .expect("the live check succeeds")
            .expect("a release exists");
        assert_eq!(
            found.release.version().as_deref(),
            Some(found.version.as_str())
        );
        assert!(
            !found.release.assets.is_empty(),
            "the release must carry assets"
        );
    }
}
