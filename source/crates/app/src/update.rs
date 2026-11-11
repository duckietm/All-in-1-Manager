//! Keeps the program itself up to date (AppUpdate.cs). At startup it asks GitHub for the newest release; when that
//! release is newer than the running build, the download replaces the running file and the program starts again.
//! Releases of the Rust build are tagged `rust-v<version>` and carry `all-in-1-manager-windows-x64.exe` /
//! `all-in-1-manager-linux-x64`. Only published releases count: no internet, a draft or pre-release, or a
//! repository without releases all mean no question is asked.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = "duckietm/All-in-1-Manager";
const TAG_PREFIX: &str = "rust-v";

#[cfg(windows)]
pub const ASSET_NAME: &str = "all-in-1-manager-windows-x64.exe";
#[cfg(not(windows))]
pub const ASSET_NAME: &str = "all-in-1-manager-linux-x64";

/// The release file holding `ASSET_NAME`'s SHA-256, written by the release workflow.
#[cfg(windows)]
pub const CHECKSUM_NAME: &str = "all-in-1-manager-windows-x64.exe.sha256";
#[cfg(not(windows))]
pub const CHECKSUM_NAME: &str = "all-in-1-manager-linux-x64.sha256";

/// Where to get the newest program by hand.
pub fn download_page() -> String {
    format!("https://github.com/{REPOSITORY}/releases")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    pub asset_url: String,
    pub asset_bytes: u64,
    /// `<asset>.sha256` of the release, when it publishes one: what the download is checked against.
    pub asset_sha256_url: Option<String>,
    pub notes: String,
    /// Every published version newer than this build, newest first, with its notes: what an update brings.
    pub changes: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        // The GitHub API refuses requests without a user agent.
        .user_agent(format!("All-in-1-Manager/{CURRENT_VERSION}"))
        .timeout(std::time::Duration::from_secs(20 * 60))
        .build()
        .expect("HTTP client")
}

/// True for a build that can replace itself: a release build (cargo run of a debug build never offers an update).
/// Never on macOS: the releases have no macOS program, and taking the Linux one would replace a working build with a
/// file that does not start. A macOS build is updated by building it again from the sources.
pub fn can_update() -> bool {
    !cfg!(debug_assertions) && !cfg!(target_os = "macos") && std::env::current_exe().is_ok()
}

/// The newest published release when it is newer than this build, or None. Never fails.
pub async fn check() -> Option<Release> {
    if !can_update() {
        return None;
    }
    latest().await.ok().flatten().filter(|r| is_newer(&r.version))
}

/// The newest published release, newer than this build or not.
pub async fn latest() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30");
    let response = client()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .context("Could not reach GitHub")?;
    // 404 is the normal answer for a repository that has no releases yet.
    if !response.status().is_success() {
        return Ok(None);
    }
    let releases: Vec<GithubRelease> = response.json().await.context("Unexpected answer from GitHub")?;
    Ok(newest(releases))
}

fn newest(releases: Vec<GithubRelease>) -> Option<Release> {
    let mut changes: Vec<((u64, u64, u64), String, String)> = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = r.tag_name.strip_prefix(TAG_PREFIX)?;
            let parsed = parse_version(version)?;
            is_newer(version).then(|| (parsed, version.to_string(), r.body.clone().unwrap_or_default()))
        })
        .collect();
    changes.sort_by_key(|c| std::cmp::Reverse(c.0));
    let changes: Vec<(String, String)> = changes.into_iter().map(|(_, version, notes)| (version, notes)).collect();
    releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = parse_version(r.tag_name.strip_prefix(TAG_PREFIX)?)?;
            let asset = r.assets.iter().find(|a| a.name == ASSET_NAME)?;
            let checksum = r.assets.iter().find(|a| a.name == CHECKSUM_NAME);
            Some((
                version,
                Release {
                    version: r.tag_name.trim_start_matches(TAG_PREFIX).to_string(),
                    page_url: r.html_url.clone(),
                    asset_url: asset.browser_download_url.clone(),
                    asset_bytes: asset.size,
                    asset_sha256_url: checksum.map(|a| a.browser_download_url.clone()),
                    notes: r.body.clone().unwrap_or_default(),
                    // Not past the version this update installs.
                    changes: changes.iter().filter(|(v, _)| parse_version(v).is_some_and(|p| p <= version)).cloned().collect(),
                },
            ))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, release)| release)
}

/// Major.minor.patch, so 1.2 and 1.2.0 count as the same version.
pub fn is_newer(version: &str) -> bool {
    matches!((parse_version(version), parse_version(CURRENT_VERSION)), (Some(other), Some(current)) if other > current)
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").split(['-', '+']).next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_os_string();
    text.push(suffix);
    PathBuf::from(text)
}

/// The SHA-256 of `bytes` as lowercase hex.
fn sha256_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The digest from a published checksum file: "<hex>" or the `sha256sum` form "<hex>  <file name>".
fn checksum_in(text: &str) -> Option<String> {
    let digest = text.split_whitespace().next()?.trim().to_ascii_lowercase();
    (digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit())).then_some(digest)
}

/// Downloads the release file next to the running one, moves the running one aside (`.old`, removed at the next
/// start) and puts the new one in its place, keeping its name. `progress` gets (done, total) bytes.
pub async fn download_and_replace(release: &Release, progress: impl Fn(u64, u64)) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("The program path is unknown")?;
    let downloaded = with_suffix(&exe, ".new");
    let moved = with_suffix(&exe, ".old");

    let mut response = client().get(&release.asset_url).send().await.context("Download failed")?.error_for_status()?;
    let total = response.content_length().unwrap_or(release.asset_bytes);
    let mut bytes = Vec::with_capacity(total as usize);
    while let Some(chunk) = response.chunk().await.context("Download failed")? {
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }
    // A half-finished download must never replace a working program.
    if total > 0 && bytes.len() as u64 != total {
        bail!("The download stopped early ({} of {total} bytes).", bytes.len());
    }
    if bytes.len() < 1024 * 1024 {
        bail!("The downloaded file is too small ({} bytes); not installing it", bytes.len());
    }
    // What the release says the file should be. A release without a .sha256 is installed as before, so an older
    // release stays installable; when one is published it has to match, and a changed download never runs.
    if let Some(url) = &release.asset_sha256_url {
        let published = client().get(url).send().await.context("Downloading the checksum failed")?.error_for_status()?;
        let expected = checksum_in(&published.text().await.context("Downloading the checksum failed")?)
            .context("The published checksum could not be read")?;
        let actual = sha256_of(&bytes);
        if actual != expected {
            bail!(
                "The downloaded file is not the one this release publishes.
  Expected: {expected}
  Got:      {actual}
Nothing was installed. Try again, and if it keeps happening download it by hand from {}.",
                download_page()
            );
        }
    }
    std::fs::write(&downloaded, &bytes).with_context(|| format!("Could not write {}", downloaded.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&downloaded, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_file(&moved);
    // A running program cannot be overwritten, but it can be renamed.
    std::fs::rename(&exe, &moved).with_context(|| format!("Could not move {} aside", exe.display()))?;
    if let Err(error) = std::fs::rename(&downloaded, &exe) {
        let _ = std::fs::rename(&moved, &exe);
        return Err(error).with_context(|| format!("Could not put the new version in place of {}", exe.display()));
    }
    Ok(exe)
}

/// Removes what an earlier update left behind. Safe to call at every start.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        for suffix in [".old", ".new"] {
            // Still in use or not ours to delete: the next start tries again.
            let _ = std::fs::remove_file(with_suffix(&exe, suffix));
        }
    }
}

/// Starts this program once more (after an update, or for a setting that applies at startup such as the look).
pub fn start_again() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::Command::new(&exe).args(args).current_dir(exe.parent().unwrap_or(Path::new("."))).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_the_sha256_as_hex() {
        // The empty input's SHA-256, the standard test vector.
        assert_eq!(sha256_of(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_of(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn a_published_checksum_is_read_in_either_form() {
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(checksum_in(digest).as_deref(), Some(digest));
        assert_eq!(checksum_in(&format!("{digest}
")).as_deref(), Some(digest));
        // The sha256sum form: the digest, two spaces, the file name.
        assert_eq!(checksum_in(&format!("{digest}  {ASSET_NAME}
")).as_deref(), Some(digest));
        // Upper case is the same digest.
        assert_eq!(checksum_in(&digest.to_ascii_uppercase()).as_deref(), Some(digest));
    }

    #[test]
    fn nonsense_is_not_taken_for_a_checksum() {
        assert_eq!(checksum_in(""), None);
        assert_eq!(checksum_in("   "), None);
        assert_eq!(checksum_in("not a checksum"), None);
        // Too short, too long, and not hex.
        assert_eq!(checksum_in("ba7816bf"), None);
        assert_eq!(checksum_in(&"a".repeat(65)), None);
        assert_eq!(checksum_in(&"z".repeat(64)), None);
    }

    fn release(tag: &str, asset: bool, prerelease: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            html_url: format!("https://example.test/{tag}"),
            body: None,
            draft: false,
            prerelease,
            assets: if asset {
                vec![GithubAsset { name: ASSET_NAME.into(), browser_download_url: format!("https://example.test/{tag}/{ASSET_NAME}"), size: 5 }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn picks_the_newest_rust_release_with_our_asset() {
        let releases = vec![
            // Far above this build's version, which goes up with every release.
            release("rust-v100.0.1", true, false),
            release("rust-v100.2.0", true, false),
            release("rust-v300.0.0", false, false),
            release("rust-v400.0.0", true, true),
            release("rust-v0.9.0", true, false),
            release("v900.0.0", true, false), // the .NET build
        ];
        let release = newest(releases).unwrap();
        assert_eq!(release.version, "100.2.0");
        // What an update brings: the versions newer than this build only, newest first.
        let versions: Vec<&str> = release.changes.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(versions, vec!["100.2.0", "100.0.1"]);
        assert!(is_newer("99.0.0"));
        assert!(!is_newer(CURRENT_VERSION));
        assert!(!is_newer("0.1.0"));
        assert_eq!(parse_version("v1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
    }
}
