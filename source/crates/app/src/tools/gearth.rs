//! G-Earth on this computer (Tools → G-Earth), ported from the catalogue ripper's `gearth_install.rs`: the official
//! release (github.com/G-Realm/G-Earth, MIT licence, Java included) for this platform and architecture, downloaded into
//! `configuration/tools/G-Earth/` and started from there. The page asks before anything is downloaded, removed or
//! started; this module only does what it is told.
//!
//! The download is checked against the SHA-256 the release publishes (GitHub's asset digest, or a `<asset>.sha256`
//! file) when there is one, capped at [`MAX_DOWNLOAD`], and unpacked into `G-Earth.new` next to the folder, which then
//! takes the folder's place by renaming: a failed, cancelled or interrupted install never leaves a half-unpacked
//! G-Earth behind. Archive entries outside the folder (`..`, absolute paths) refuse the whole archive. A link is
//! accepted only when its target is relative and stays inside the folder (created on Unix, skipped on Windows); any
//! other link, and any entry written through a link, refuses the whole archive.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::update::{self, GithubRelease};

pub const REPOSITORY: &str = "G-Realm/G-Earth";

/// The release file for an operating system and architecture (`std::env::consts` names), or None when G-Earth
/// publishes no build for it.
pub fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("G-Earth-windows-x64.zip"),
        ("windows", "x86") => Some("G-Earth-windows-x86.zip"),
        ("macos", "x86_64") => Some("G-Earth-mac-x64.zip"),
        ("macos", "aarch64") => Some("G-Earth-mac-aarch64.zip"),
        ("linux", "x86_64") => Some("G-Earth-linux-x64.tar.gz"),
        _ => None,
    }
}

/// The release file for this computer.
pub fn asset() -> Option<&'static str> {
    asset_for(std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(windows)]
pub const LAUNCHER: &str = "G-Earth.exe";
#[cfg(not(windows))]
pub const LAUNCHER: &str = "G-Earth";

/// G-Earth's extension ports: the first free one from 9092 upwards.
pub const PORTS: std::ops::RangeInclusive<u16> = 9092..=9101;
/// The largest download accepted (the release is about 100 MB).
pub const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;
/// The most an archive may unpack to, and the most entries it may have.
const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
/// The longest link target read from a zip entry.
const MAX_LINK: u64 = 4096;
/// What the Manager writes next to the files it unpacked: the version and whether it was verified.
const MARKER: &str = ".all-in-1-manager.json";

/// The download was cancelled.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// GitHub's limit for requests without a login is used up; `reset`: when it resets (Unix seconds), when GitHub says.
#[derive(Debug)]
pub struct RateLimited {
    pub reset: Option<i64>,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHub's limit for requests without a login is used up")
    }
}

impl std::error::Error for RateLimited {}

/// Where releases are looked up and where their files must come from.
#[derive(Debug, Clone)]
pub struct Source {
    /// The GitHub API.
    pub api: String,
    /// Every download URL (the archive, its `.sha256`) must start with this.
    pub downloads: String,
}

impl Source {
    pub fn github() -> Self {
        Source { api: update::GITHUB_API.to_string(), downloads: format!("https://github.com/{REPOSITORY}/releases/download/") }
    }
}

/// `configuration/tools/G-Earth`.
pub fn folder() -> PathBuf {
    aio_core::config_dir::directory().join("tools").join("G-Earth")
}

/// `G-Earth<suffix>` next to the folder: `.part` (the download), `.new` (being unpacked), `.old` (being replaced),
/// `.removed` (being removed).
fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "G-Earth".into());
    name.push(suffix);
    dir.with_file_name(name)
}

/// The first match (by name) at the top of `dir`, else one folder down.
fn top_or_one_down(dir: &Path, matches: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut top: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
    top.sort();
    if let Some(found) = top.iter().rev().find(|p| matches(p)) {
        return Some(found.clone());
    }
    let mut down: Vec<PathBuf> = top
        .iter()
        .filter(|p| p.is_dir())
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()))
        .filter(|p| matches(p))
        .collect();
    down.sort();
    down.pop()
}

/// A macOS application bundle (`*.app`).
fn is_app_bundle(path: &Path) -> bool {
    path.is_dir() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

/// This platform's launcher: the `*.app` bundle on macOS, `G-Earth(.exe)` elsewhere.
fn is_launcher(path: &Path) -> bool {
    if cfg!(target_os = "macos") {
        is_app_bundle(path)
    } else {
        path.file_name().is_some_and(|n| n == LAUNCHER) && path.is_file()
    }
}

/// The G-Earth launcher in `dir`, at the top or one folder down (an archive with one top folder).
pub fn installed_launcher(dir: &Path) -> Option<PathBuf> {
    top_or_one_down(dir, is_launcher)
}

/// An installed G-Earth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub launcher: PathBuf,
    /// None for a G-Earth put there by hand.
    pub version: Option<String>,
    /// The download matched the SHA-256 its release publishes.
    pub verified: bool,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    version: String,
    verified: bool,
}

/// The G-Earth in `dir`, if there is one.
pub fn installed(dir: &Path) -> Option<Installed> {
    let launcher = installed_launcher(dir)?;
    let marker: Option<Marker> = std::fs::read(dir.join(MARKER)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Some(Installed { launcher, version: marker.as_ref().map(|m| m.version.clone()), verified: marker.is_some_and(|m| m.verified) })
}

/// Tidies up after a stop in the middle of an update or a removal: puts the previous G-Earth back when the program
/// stopped between the two renames of an update, else deletes what is left of it and of a removal.
pub fn recover(dir: &Path) {
    let old = sibling(dir, ".old");
    if !dir.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, dir);
    } else if dir.exists() && old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    let _ = std::fs::remove_dir_all(sibling(dir, ".removed"));
}

/// True when something listens on one of G-Earth's extension ports on this computer.
pub async fn is_running(ports: impl IntoIterator<Item = u16>) -> bool {
    let probes = ports.into_iter().map(|port| async move {
        let connect = tokio::net::TcpStream::connect(("127.0.0.1", port));
        matches!(tokio::time::timeout(Duration::from_millis(300), connect).await, Ok(Ok(_)))
    });
    futures::future::join_all(probes).await.into_iter().any(|open| open)
}

/// How a download is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// The SHA-256 (hex) GitHub publishes with the asset.
    Digest(String),
    /// The `<asset>.sha256` file of the release.
    File(String),
    /// The release publishes none: the download cannot be verified.
    None,
}

/// The newest G-Earth release's file for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The file name (it says zip or tar.gz).
    pub asset: &'static str,
    pub asset_url: String,
    pub size: u64,
    pub checksum: Checksum,
}

impl Release {
    pub fn verifiable(&self) -> bool {
        self.checksum != Checksum::None
    }
}

/// The newest published release's `asset`. Downloads nothing.
pub async fn latest(source: &Source, asset: &'static str) -> Result<Release> {
    let url = format!("{}/repos/{REPOSITORY}/releases/latest", source.api);
    let response = update::client()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("Could not reach GitHub")?;
    let status = response.status().as_u16();
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::trim).map(str::to_string);
    if status == 429 || (status == 403 && header("x-ratelimit-remaining").as_deref() == Some("0")) {
        return Err(RateLimited { reset: header("x-ratelimit-reset").and_then(|v| v.parse().ok()) }.into());
    }
    if !response.status().is_success() {
        bail!("GitHub answered {} for {url}", response.status());
    }
    release_for(response.json().await.context("Unexpected answer from GitHub")?, asset, source)
}

fn release_for(release: GithubRelease, name: &'static str, source: &Source) -> Result<Release> {
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == name)
        .with_context(|| format!("The newest G-Earth release ({}) has no {name}.", release.tag_name))?;
    if asset.size > MAX_DOWNLOAD {
        bail!("The G-Earth download is {} MiB, more than the {} MiB accepted.", asset.size / (1024 * 1024), MAX_DOWNLOAD / (1024 * 1024));
    }
    let from_release = |url: &str| -> Result<String> {
        if !url.starts_with(&source.downloads) {
            bail!("The release names a download outside {} ({url}); nothing was downloaded.", source.downloads);
        }
        Ok(url.to_string())
    };
    let checksum_file = format!("{name}.sha256");
    let checksum = match asset.sha256() {
        Some(digest) => Checksum::Digest(digest),
        None => match release.assets.iter().find(|a| a.name == checksum_file) {
            Some(file) => Checksum::File(from_release(&file.browser_download_url)?),
            None => Checksum::None,
        },
    };
    Ok(Release {
        version: release.tag_name.trim_start_matches('v').to_string(),
        asset: name,
        asset_url: from_release(&asset.browser_download_url)?,
        size: asset.size,
        checksum,
    })
}

/// Where an install is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Bytes downloaded, of (0: unknown).
    Downloading(u64, u64),
    Unpacking,
}

/// Downloads `release` and puts it in `dir` in place of what is there. Nothing in `dir` changes unless it all worked.
pub async fn install(release: &Release, dir: &Path, cancel: &CancellationToken, progress: impl Fn(Step)) -> Result<Installed> {
    install_capped(release, dir, cancel, progress, MAX_DOWNLOAD).await
}

async fn install_capped(release: &Release, dir: &Path, cancel: &CancellationToken, progress: impl Fn(Step), cap: u64) -> Result<Installed> {
    let parent = dir.parent().context("The G-Earth folder has no parent folder")?;
    std::fs::create_dir_all(parent).with_context(|| format!("Could not create {}", parent.display()))?;
    recover(dir);
    let part = sibling(dir, ".part");
    let new = sibling(dir, ".new");
    // Left over from an install that was interrupted.
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_dir_all(&new);

    let result = download_check_unpack(release, dir, &part, &new, cancel, &progress, cap).await;
    let _ = std::fs::remove_file(&part);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&new);
    }
    result
}

async fn download_check_unpack(
    release: &Release,
    dir: &Path,
    part: &Path,
    new: &Path,
    cancel: &CancellationToken,
    progress: &impl Fn(Step),
    cap: u64,
) -> Result<Installed> {
    let actual = download(&release.asset_url, part, cancel, progress, cap, release.size).await?;
    let expected = match &release.checksum {
        Checksum::Digest(digest) => Some(digest.clone()),
        Checksum::File(url) => Some(update::published_checksum(url).await?),
        Checksum::None => None,
    };
    if let Some(expected) = &expected {
        if &actual != expected {
            bail!("The download is not the file the release publishes (SHA-256 {actual}, expected {expected}). Nothing was installed.");
        }
    }

    progress(Step::Unpacking);
    let zip = release.asset.ends_with(".zip");
    let (archive, target, token) = (part.to_path_buf(), new.to_path_buf(), cancel.clone());
    tokio::task::spawn_blocking(move || extract(&archive, &target, &token, zip)).await.context("Unpacking stopped")??;
    if installed_launcher(new).is_none() {
        bail!("The G-Earth program was not found in the download.");
    }
    let marker = Marker { version: release.version.clone(), verified: expected.is_some() };
    std::fs::write(new.join(MARKER), serde_json::to_vec(&marker)?)?;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    replace(new, dir)?;
    installed(dir).context("G-Earth was not found after the install")
}

/// Streams `url` into `path` and returns its SHA-256; stops at `cap` bytes or when cancelled.
async fn download(url: &str, path: &Path, cancel: &CancellationToken, progress: &impl Fn(Step), cap: u64, size: u64) -> Result<String> {
    use std::io::Write;
    let mut response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Cancelled.into()),
        response = update::client().get(url).timeout(Duration::from_secs(30 * 60)).send() => {
            response.context("The download failed")?.error_for_status().context("The download failed")?
        }
    };
    let too_big = || anyhow::anyhow!("The download is bigger than {} MiB; stopped.", cap / (1024 * 1024));
    if response.content_length().is_some_and(|n| n > cap) {
        return Err(too_big());
    }
    let total = response.content_length().unwrap_or(size);
    let mut file = std::fs::File::create(path).with_context(|| format!("Could not write {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    progress(Step::Downloading(0, total));
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Cancelled.into()),
            chunk = response.chunk() => chunk.context("The download failed")?,
        };
        let Some(chunk) = chunk else { break };
        done += chunk.len() as u64;
        if done > cap {
            return Err(too_big());
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        progress(Step::Downloading(done, total));
    }
    if total > 0 && done != total {
        bail!("The download stopped early ({done} of {total} bytes).");
    }
    file.sync_all()?;
    Ok(update::hex(&hasher.finalize()))
}

/// Puts `new` in place of `dir` (two renames; [`recover`] undoes a stop between them).
fn replace(new: &Path, dir: &Path) -> Result<()> {
    let old = sibling(dir, ".old");
    let _ = std::fs::remove_dir_all(&old);
    if dir.exists() {
        std::fs::rename(dir, &old).with_context(|| format!("Could not replace {} (is G-Earth still open?)", dir.display()))?;
    }
    if let Err(error) = std::fs::rename(new, dir) {
        if old.exists() {
            let _ = std::fs::rename(&old, dir);
        }
        return Err(error).with_context(|| format!("Could not move the new G-Earth into {}", dir.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Removes the G-Earth folder and whatever an interrupted install left next to it. The folder is first renamed to
/// `G-Earth.removed` (which fails cleanly while G-Earth has it open), then deleted: a G-Earth is never left half
/// deleted in its place, and [`recover`] finishes a deletion that was interrupted.
pub fn remove(dir: &Path) -> Result<()> {
    let removed = sibling(dir, ".removed");
    let _ = std::fs::remove_dir_all(&removed);
    if dir.exists() {
        std::fs::rename(dir, &removed).with_context(|| format!("Could not remove {} (is G-Earth still open?)", dir.display()))?;
    }
    for leftover in [sibling(dir, ".new"), sibling(dir, ".old")] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    let _ = std::fs::remove_file(sibling(dir, ".part"));
    if removed.exists() {
        std::fs::remove_dir_all(&removed).with_context(|| format!("Could not delete {}", removed.display()))?;
    }
    Ok(())
}

/// `text` as a PowerShell single-quoted string. PowerShell also takes the typographic quotes U+2018–U+201B for a
/// single quote, so each of them is doubled too.
#[cfg_attr(not(windows), allow(dead_code))]
fn powershell_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}'..='\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Starts the launcher. On Windows it runs elevated (Windows asks first): for the Flash/AIR client G-Earth edits the
/// hosts file, which needs administrator rights. Blocks until Windows' question is answered. On macOS the `.app`
/// bundle is opened with `open -a`.
pub fn start(launcher: &Path) -> Result<()> {
    let launcher = std::path::absolute(launcher)?;
    let dir = launcher.parent().unwrap_or(Path::new(".")).to_path_buf();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// No console window for the PowerShell helper.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = format!(
            "Start-Process -FilePath {} -WorkingDirectory {} -Verb RunAs",
            powershell_quoted(&launcher.display().to_string()),
            powershell_quoted(&dir.display().to_string())
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .context("Could not start G-Earth")?;
        if !status.success() {
            bail!(
                "G-Earth was not started (the administrator prompt was refused or failed). Start {} as administrator yourself.",
                launcher.display()
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg("-a")
            .arg(&launcher)
            .current_dir(&dir)
            .status()
            .with_context(|| format!("Could not start {}", launcher.display()))?;
        if !status.success() {
            bail!("Could not start {}", launcher.display());
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new(&launcher)
            .current_dir(&dir)
            .spawn()
            .with_context(|| format!("Could not start {}", launcher.display()))?;
    }
    Ok(())
}

/// Unpacks the archive (zip, else tar.gz) into `target` (created; must not exist).
fn extract(archive: &Path, target: &Path, cancel: &CancellationToken, zip: bool) -> Result<()> {
    std::fs::create_dir(target).with_context(|| format!("Could not create {}", target.display()))?;
    if zip {
        extract_zip(archive, target, cancel)
    } else {
        extract_tar_gz(archive, target, cancel)
    }
}

/// A plain file or folder name: not `..`, no drive, no stream (`:`).
fn plain(name: &std::ffi::OsStr) -> bool {
    !name.to_string_lossy().contains(':')
}

/// An archive path as a path inside the target: only plain names (and `.`), nothing absolute, no `..`, no drive or
/// stream (`:`).
fn inside(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if plain(name) => out.push(name),
            Component::CurDir => {}
            _ => bail!("The archive holds an unsafe path ({}); nothing was installed.", path.display()),
        }
    }
    if out.as_os_str().is_empty() {
        bail!("The archive holds an empty path; nothing was installed.");
    }
    Ok(out)
}

/// Checks a symbolic link at `at` (a path [`inside`] the folder) pointing to `to`: the target has to be relative and,
/// read from the link's own folder, stay inside the folder.
fn check_link(at: &Path, to: &Path) -> Result<()> {
    let refuse = || anyhow::anyhow!("The archive holds a link that points outside the G-Earth folder ({} -> {}); nothing was installed.", at.display(), to.display());
    let mut folder: Vec<OsString> = at.parent().map(|p| p.components().map(|c| c.as_os_str().to_os_string()).collect()).unwrap_or_default();
    if to.as_os_str().is_empty() {
        return Err(refuse());
    }
    for component in to.components() {
        match component {
            Component::Normal(name) if plain(name) => folder.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                folder.pop().ok_or_else(refuse)?;
            }
            _ => return Err(refuse()),
        }
    }
    Ok(())
}

/// What an archive has unpacked so far.
#[derive(Default)]
struct Budget {
    bytes: u64,
    entries: usize,
    /// The links accepted so far (paths inside the folder): nothing is written through one.
    links: HashSet<PathBuf>,
}

impl Budget {
    /// Counts an entry at `relative`; refuses it when it would be written through a link.
    fn entry(&mut self, cancel: &CancellationToken, relative: &Path) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            bail!("The archive has more than {MAX_ENTRIES} entries; nothing was installed.");
        }
        if relative.ancestors().any(|a| self.links.contains(a)) {
            bail!("The archive writes through a link ({}); nothing was installed.", relative.display());
        }
        Ok(())
    }

    fn write(&mut self, reader: &mut impl Read, out: &Path) -> Result<()> {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // create_new: never through something already there.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .with_context(|| format!("Could not write {}", out.display()))?;
        let left = MAX_UNPACKED - self.bytes;
        self.bytes += std::io::copy(&mut reader.take(left + 1), &mut file)?;
        if self.bytes > MAX_UNPACKED {
            bail!("The archive unpacks to more than {} MiB; nothing was installed.", MAX_UNPACKED / (1024 * 1024));
        }
        Ok(())
    }

    /// A symbolic link at `relative` to `to`, after [`check_link`]: created on Unix, left out on Windows.
    fn link(&mut self, target: &Path, relative: &Path, to: &Path) -> Result<()> {
        check_link(relative, to)?;
        self.links.insert(relative.to_path_buf());
        #[cfg(unix)]
        {
            let out = target.join(relative);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(to, &out).with_context(|| format!("Could not create the link {}", out.display()))?;
        }
        #[cfg(not(unix))]
        let _ = target;
        Ok(())
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o755))?;
    Ok(())
}

fn extract_zip(archive: &Path, target: &Path, cancel: &CancellationToken) -> Result<()> {
    let mut zip = zip::ZipArchive::new(BufReader::new(std::fs::File::open(archive)?)).context("The download is not a zip file")?;
    let mut budget = Budget::default();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let name = PathBuf::from(entry.name());
        let relative = inside(&name)?;
        if entry.enclosed_name().is_none() {
            bail!("The archive holds an unsafe path ({}); nothing was installed.", name.display());
        }
        budget.entry(cancel, &relative)?;
        if entry.is_symlink() {
            // A zip link (Unix mode S_IFLNK) keeps its target as the entry's content.
            let mut to = Vec::new();
            (&mut entry).take(MAX_LINK + 1).read_to_end(&mut to)?;
            if to.len() as u64 > MAX_LINK {
                bail!("The archive holds a link with a too long target ({}); nothing was installed.", name.display());
            }
            let to = String::from_utf8(to).with_context(|| format!("The link {} has an unreadable target", name.display()))?;
            budget.link(target, &relative, Path::new(&to))?;
            continue;
        }
        let out = target.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        budget.write(&mut entry, &out)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            set_mode(&out, mode)?;
        }
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, target: &Path, cancel: &CancellationToken) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(BufReader::new(std::fs::File::open(archive)?));
    let mut tar = tar::Archive::new(gz);
    let mut budget = Budget::default();
    for entry in tar.entries().context("The download is not a tar.gz file")? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let path = entry.path()?.into_owned();
        let relative = inside(&path)?;
        budget.entry(cancel, &relative)?;
        let out = target.join(&relative);
        let link_name = match kind {
            tar::EntryType::Symlink | tar::EntryType::Link => {
                Some(entry.link_name()?.map(|p| p.into_owned()).with_context(|| format!("The link {} has no target", path.display()))?)
            }
            _ => None,
        };
        match kind {
            tar::EntryType::Directory => std::fs::create_dir_all(&out)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                budget.write(&mut entry, &out)?;
                #[cfg(unix)]
                set_mode(&out, entry.header().mode().unwrap_or(0o644))?;
            }
            tar::EntryType::Symlink => {
                budget.link(target, &relative, link_name.as_deref().unwrap_or(Path::new("")))?;
            }
            tar::EntryType::Link => {
                // A hard link names another entry of the archive (from its root): a copy of that file, inside.
                let source = inside(link_name.as_deref().unwrap_or(Path::new("")))?;
                if source.ancestors().any(|a| budget.links.contains(a)) {
                    bail!("The archive holds a link through a link ({}); nothing was installed.", path.display());
                }
                #[cfg(unix)]
                {
                    let mut file = std::fs::File::open(target.join(&source))
                        .with_context(|| format!("The link {} points to a file that is not there", path.display()))?;
                    budget.write(&mut file, &out)?;
                }
            }
            // Devices, FIFOs and the like: never created.
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fake {
    //! A local stand-in for GitHub: serves fixed paths over plain HTTP and records what was asked.

    use std::collections::HashMap;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One answer.
    #[derive(Clone)]
    pub struct Reply {
        pub status: &'static str,
        pub headers: Vec<(&'static str, String)>,
        pub body: Vec<u8>,
        /// Send a Content-Length (else the body ends when the connection closes).
        pub length: bool,
    }

    pub struct Server {
        pub base: String,
        pub hits: Arc<Mutex<Vec<String>>>,
        routes: Arc<Mutex<HashMap<String, Reply>>>,
    }

    pub async fn serve() -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::new(Mutex::new(HashMap::new()));
        let (served, seen) = (routes.clone(), hits.clone());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (routes, seen) = (served.clone(), seen.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request);
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    seen.lock().push(path.clone());
                    let reply = routes.lock().get(&path).cloned().unwrap_or(Reply {
                        status: "404 Not Found",
                        headers: Vec::new(),
                        body: b"not found".to_vec(),
                        length: true,
                    });
                    let mut head = format!("HTTP/1.1 {}\r\nConnection: close\r\n", reply.status);
                    if reply.length {
                        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
                    }
                    for (name, value) in &reply.headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&reply.body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Server { base, hits, routes }
    }

    impl Server {
        pub fn route(&self, path: &str, body: Vec<u8>) {
            self.reply(path, Reply { status: "200 OK", headers: Vec::new(), body, length: true });
        }

        pub fn reply(&self, path: &str, reply: Reply) {
            self.routes.lock().insert(path.to_string(), reply);
        }

        pub fn asked(&self, path: &str) -> bool {
            self.hits.lock().iter().any(|p| p == path)
        }

        /// This server as the release source: the API, and downloads under `/download/`.
        pub fn source(&self) -> super::Source {
            super::Source { api: self.base.clone(), downloads: format!("{}/download/", self.base) }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    /// One archive entry: a file with content, or a symbolic link (to its target).
    pub(crate) enum Entry<'a> {
        File(&'a str, &'a [u8]),
        Link(&'a str, &'a str),
    }

    /// This computer's release file (the tests run where G-Earth has a build).
    pub(crate) fn this_asset() -> &'static str {
        asset().expect("G-Earth has a build for this computer")
    }

    pub(crate) fn zip_of(entries: &[Entry]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for entry in entries {
            match entry {
                Entry::File(name, bytes) => {
                    zip.start_file(*name, options).unwrap();
                    zip.write_all(bytes).unwrap();
                }
                // A Unix-mode link: S_IFLNK, the target as content.
                Entry::Link(name, to) => zip.add_symlink(*name, *to, options).unwrap(),
            }
        }
        zip.finish().unwrap().into_inner()
    }

    pub(crate) fn tar_gz_of(entries: &[Entry]) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            // Names written raw, so a path the builder would refuse (`..`) can be tested too.
            let (name, kind, size) = match entry {
                Entry::File(name, bytes) => (name, tar::EntryType::Regular, bytes.len() as u64),
                Entry::Link(name, _) => (name, tar::EntryType::Symlink, 0),
            };
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_size(size);
            header.set_mode(0o755);
            header.set_entry_type(kind);
            match entry {
                Entry::File(_, bytes) => {
                    header.set_cksum();
                    tar.append(&header, *bytes).unwrap();
                }
                Entry::Link(_, to) => {
                    header.set_link_name(to).unwrap();
                    header.set_cksum();
                    tar.append(&header, std::io::empty()).unwrap();
                }
            }
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    /// The platform's archive.
    pub(crate) fn archive_of(entries: &[Entry]) -> Vec<u8> {
        if this_asset().ends_with(".zip") {
            zip_of(entries)
        } else {
            tar_gz_of(entries)
        }
    }

    pub(crate) fn release_json(version: &str, downloads: &str, size: usize, digest: Option<&str>, checksum_file: bool) -> Vec<u8> {
        let asset = this_asset();
        let mut assets = vec![serde_json::json!({
            "name": asset,
            "browser_download_url": format!("{downloads}{asset}"),
            "size": size,
            "digest": digest.map(|d| format!("sha256:{d}")),
        })];
        if checksum_file {
            assets.push(serde_json::json!({
                "name": format!("{asset}.sha256"),
                "browser_download_url": format!("{downloads}{asset}.sha256"),
                "size": 64,
            }));
        }
        serde_json::to_vec(&serde_json::json!({
            "tag_name": format!("v{version}"),
            "html_url": format!("{downloads}release"),
            "assets": assets,
        }))
        .unwrap()
    }

    pub(crate) fn good_archive() -> Vec<u8> {
        archive_of(&[Entry::File(&format!("G-Earth-1.5.4/{LAUNCHER}"), b"MZ"), Entry::File("G-Earth-1.5.4/G-Earth.jar", b"jar")])
    }

    pub(crate) const RELEASE_PATH: &str = "/repos/G-Realm/G-Earth/releases/latest";

    pub(crate) fn download_path() -> String {
        format!("/download/{}", this_asset())
    }

    /// A fake GitHub with one release whose file is `archive`; `digest` / `file` publish its SHA-256 (or a `wrong` one).
    pub(crate) async fn github(archive: Vec<u8>, digest: bool, file: bool, wrong: bool) -> fake::Server {
        let sha = if wrong { "0".repeat(64) } else { update::sha256_of(&archive) };
        let server = fake::serve().await;
        server.route(&format!("{}.sha256", download_path()), format!("{sha}  {}\n", this_asset()).into_bytes());
        let downloads = server.source().downloads;
        server.route(RELEASE_PATH, release_json("1.5.4", &downloads, archive.len(), digest.then_some(&sha), file));
        server.route(&download_path(), archive);
        server
    }

    /// Unpacks `archive` (zip or tar.gz) into `<temp>/out`.
    fn unpack(archive: &[u8]) -> (tempfile::TempDir, Result<()>) {
        let work = tempfile::tempdir().unwrap();
        let file = work.path().join("archive");
        std::fs::write(&file, archive).unwrap();
        let target = work.path().join("out");
        let result = extract(&file, &target, &CancellationToken::new(), archive.starts_with(b"PK"));
        (work, result)
    }

    fn assert_nothing_left(dir: &Path) {
        for suffix in [".part", ".new", ".old", ".removed"] {
            assert!(!sibling(dir, suffix).exists(), "{suffix} left behind");
        }
    }

    #[test]
    fn asset_per_platform_and_architecture() {
        assert_eq!(asset_for("windows", "x86_64"), Some("G-Earth-windows-x64.zip"));
        assert_eq!(asset_for("windows", "x86"), Some("G-Earth-windows-x86.zip"));
        assert_eq!(asset_for("macos", "x86_64"), Some("G-Earth-mac-x64.zip"));
        assert_eq!(asset_for("macos", "aarch64"), Some("G-Earth-mac-aarch64.zip"));
        assert_eq!(asset_for("linux", "x86_64"), Some("G-Earth-linux-x64.tar.gz"));
        for (os, arch) in [("windows", "aarch64"), ("linux", "aarch64"), ("linux", "x86"), ("freebsd", "x86_64"), ("macos", "x86")] {
            assert_eq!(asset_for(os, arch), None, "{os} {arch}");
        }
    }

    #[test]
    fn installed_launcher_found_top_or_one_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("G-Earth");
        assert!(installed_launcher(&root).is_none());
        let nested = root.join("G-Earth-1.5.4");
        std::fs::create_dir_all(&nested).unwrap();
        if cfg!(target_os = "macos") {
            return;
        }
        std::fs::write(nested.join(LAUNCHER), b"").unwrap();
        assert_eq!(installed_launcher(&root), Some(nested.join(LAUNCHER)));
        std::fs::write(root.join(LAUNCHER), b"").unwrap();
        assert_eq!(installed_launcher(&root), Some(root.join(LAUNCHER)));
        // Put there by hand: no version, not verified.
        assert_eq!(installed(&root), Some(Installed { launcher: root.join(LAUNCHER), version: None, verified: false }));
    }

    #[test]
    fn app_bundle_found_top_or_one_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("G-Earth");
        let nested = root.join("G-Earth-1.5.4").join("G-Earth.app");
        std::fs::create_dir_all(nested.join("Contents")).unwrap();
        // A file named like a bundle is not one.
        std::fs::write(root.join("notes.app"), b"").unwrap();
        assert_eq!(top_or_one_down(&root, is_app_bundle), Some(nested.clone()));
        std::fs::create_dir_all(root.join("G-Earth.app")).unwrap();
        assert_eq!(top_or_one_down(&root, is_app_bundle), Some(root.join("G-Earth.app")));
    }

    #[test]
    fn powershell_quoting_doubles_every_single_quote() {
        assert_eq!(powershell_quoted(r"C:\G-Earth\G-Earth.exe"), r"'C:\G-Earth\G-Earth.exe'");
        assert_eq!(powershell_quoted("it's"), "'it''s'");
        assert_eq!(powershell_quoted("a\u{2018}b\u{2019}c\u{201A}d\u{201B}e"), "'a\u{2018}\u{2018}b\u{2019}\u{2019}c\u{201A}\u{201A}d\u{201B}\u{201B}e'");
    }

    #[tokio::test]
    async fn download_is_verified_or_marked_unverified() {
        // (GitHub digest, .sha256 file) → verified.
        for (digest, file, verified) in [(true, false, true), (false, true, true), (true, true, true), (false, false, false)] {
            let server = github(good_archive(), digest, file, false).await;
            let release = latest(&server.source(), this_asset()).await.unwrap();
            assert_eq!(release.version, "1.5.4");
            assert_eq!(release.verifiable(), verified);
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("tools").join("G-Earth");
            let steps = parking_lot::Mutex::new(Vec::new());
            let done = install(&release, &dir, &CancellationToken::new(), |s| steps.lock().push(s)).await.unwrap();
            assert_eq!(done.version.as_deref(), Some("1.5.4"));
            assert_eq!(done.verified, verified, "digest {digest}, file {file}");
            assert_eq!(installed(&dir), Some(done.clone()));
            assert_eq!(done.launcher, dir.join("G-Earth-1.5.4").join(LAUNCHER));
            assert!(steps.lock().contains(&Step::Unpacking));
            assert_nothing_left(&dir);
        }

        // A published checksum that does not match: nothing is installed.
        for (digest, file) in [(true, false), (false, true)] {
            let server = github(good_archive(), digest, file, true).await;
            let release = latest(&server.source(), this_asset()).await.unwrap();
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("G-Earth");
            let error = install(&release, &dir, &CancellationToken::new(), |_| {}).await.unwrap_err();
            assert!(format!("{error:#}").contains("not the file the release publishes"), "{error:#}");
            assert!(!dir.exists());
            assert_nothing_left(&dir);
        }

        // A .sha256 file over 4 KiB, with or without a Content-Length: refused, nothing installed.
        for length in [true, false] {
            let server = github(good_archive(), false, true, false).await;
            let mut body = format!("{}  {}\n", update::sha256_of(&good_archive()), this_asset()).into_bytes();
            body.resize(10 * 1024, b' ');
            server.reply(&format!("{}.sha256", download_path()), fake::Reply { status: "200 OK", headers: Vec::new(), body, length });
            let release = latest(&server.source(), this_asset()).await.unwrap();
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("G-Earth");
            let error = install(&release, &dir, &CancellationToken::new(), |_| {}).await.unwrap_err();
            assert!(format!("{error:#}").contains("checksum could not be read"), "{error:#}");
            assert!(!dir.exists());
            assert_nothing_left(&dir);
        }

        // Over the cap: stopped, nothing kept.
        let server = github(good_archive(), false, false, false).await;
        let release = latest(&server.source(), this_asset()).await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("G-Earth");
        let error = install_capped(&release, &dir, &CancellationToken::new(), |_| {}, 16).await.unwrap_err();
        assert!(format!("{error:#}").contains("bigger than"), "{error:#}");
        assert!(!dir.exists());
        assert_nothing_left(&dir);

        // A release that says it is too big is refused before any download.
        let source = Source::github();
        let mut too_big: GithubRelease = serde_json::from_slice(&release_json("2.0", &source.downloads, 0, None, false)).unwrap();
        assert!(release_for(serde_json::from_slice(&release_json("2.0", &source.downloads, 0, None, true)).unwrap(), this_asset(), &source).is_ok());
        too_big.assets[0].size = MAX_DOWNLOAD + 1;
        assert!(release_for(too_big, this_asset(), &source).is_err());

        // Downloads from anywhere but the project's release downloads are refused (the archive and the .sha256).
        let foreign = release_json("2.0", "https://example.com/G-Earth/releases/download/", 10, None, false);
        assert!(release_for(serde_json::from_slice(&foreign).unwrap(), this_asset(), &source).is_err());
        let mut sha_elsewhere: GithubRelease = serde_json::from_slice(&release_json("2.0", &source.downloads, 10, None, true)).unwrap();
        sha_elsewhere.assets[1].browser_download_url = "https://example.com/x.sha256".into();
        assert!(release_for(sha_elsewhere, this_asset(), &source).is_err());
        let other_repo = release_json("2.0", "https://github.com/someone/G-Earth/releases/download/", 10, None, false);
        assert!(release_for(serde_json::from_slice(&other_repo).unwrap(), this_asset(), &source).is_err());
    }

    #[tokio::test]
    async fn rate_limit_says_when_it_resets() {
        let server = fake::serve().await;
        let limited = |status: &'static str, remaining: &str| fake::Reply {
            status,
            headers: vec![("x-ratelimit-remaining", remaining.to_string()), ("x-ratelimit-reset", "1900000000".to_string())],
            body: b"{}".to_vec(),
            length: true,
        };
        for status in ["403 Forbidden", "429 Too Many Requests"] {
            server.reply(RELEASE_PATH, limited(status, "0"));
            let error = latest(&server.source(), this_asset()).await.unwrap_err();
            let limit = error.downcast_ref::<RateLimited>().unwrap_or_else(|| panic!("{error:#}"));
            assert_eq!(limit.reset, Some(1_900_000_000));
        }
        // A 403 for another reason is not the limit.
        server.reply(RELEASE_PATH, limited("403 Forbidden", "12"));
        let error = latest(&server.source(), this_asset()).await.unwrap_err();
        assert!(error.downcast_ref::<RateLimited>().is_none());
    }

    #[tokio::test]
    async fn extract_is_crash_safe() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("G-Earth");
        std::fs::create_dir_all(dir.join("G-Earth-1.5.3")).unwrap();
        std::fs::write(dir.join("G-Earth-1.5.3").join(LAUNCHER), b"old").unwrap();

        // Unsafe paths refuse the whole archive.
        for name in [format!("../{LAUNCHER}"), "a/../../evil.txt".to_string()] {
            for archive in [zip_of(&[Entry::File("ok.txt", b"ok"), Entry::File(&name, b"evil")]), tar_gz_of(&[Entry::File("ok.txt", b"ok"), Entry::File(&name, b"evil")])] {
                let (work, result) = unpack(&archive);
                assert!(result.is_err(), "{name} accepted");
                assert!(!work.path().join(LAUNCHER).exists() && !work.path().join("evil.txt").exists());
            }
        }
        for name in ["/etc/evil", "C:/evil", "C:evil", "a/b:stream", ""] {
            assert!(inside(Path::new(name)).is_err(), "{name} accepted");
        }
        assert_eq!(inside(Path::new("./G-Earth/x.jar")).unwrap(), Path::new("G-Earth").join("x.jar"));

        // An install from an archive with an unsafe path changes nothing.
        let bad = archive_of(&[Entry::File(&format!("G-Earth-1.5.4/{LAUNCHER}"), b"new"), Entry::File("../evil.txt", b"evil")]);
        let server = github(bad, true, false, false).await;
        let release = latest(&server.source(), this_asset()).await.unwrap();
        assert!(install(&release, &dir, &CancellationToken::new(), |_| {}).await.is_err());
        assert_eq!(std::fs::read(dir.join("G-Earth-1.5.3").join(LAUNCHER)).unwrap(), b"old");
        assert!(!root.path().join("evil.txt").exists());
        assert_nothing_left(&dir);

        // Cancelled: nothing downloaded, nothing changed.
        let server = github(good_archive(), true, false, false).await;
        let release = latest(&server.source(), this_asset()).await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = install(&release, &dir, &cancel, |_| {}).await.unwrap_err();
        assert!(error.is::<Cancelled>());
        assert!(!server.asked(&download_path()));
        assert_eq!(std::fs::read(dir.join("G-Earth-1.5.3").join(LAUNCHER)).unwrap(), b"old");
        assert_nothing_left(&dir);

        // The program stopped between the two renames of an update: the previous G-Earth comes back.
        std::fs::rename(&dir, sibling(&dir, ".old")).unwrap();
        std::fs::create_dir(sibling(&dir, ".new")).unwrap();
        recover(&dir);
        assert!(installed(&dir).is_some());
        // A leftover .old next to a working folder (stopped while deleting it) is deleted.
        std::fs::create_dir(sibling(&dir, ".old")).unwrap();
        recover(&dir);
        assert!(!sibling(&dir, ".old").exists());
        assert!(installed(&dir).is_some());

        // A good update replaces the old folder as a whole.
        let installed_now = install(&release, &dir, &CancellationToken::new(), |_| {}).await.unwrap();
        assert_eq!(installed_now.launcher, dir.join("G-Earth-1.5.4").join(LAUNCHER));
        assert!(!dir.join("G-Earth-1.5.3").exists());
        assert_nothing_left(&dir);

        // Remove: renamed away first, then deleted; a removal that stopped half way is finished by recover.
        remove(&dir).unwrap();
        assert!(!dir.exists());
        assert_nothing_left(&dir);
        std::fs::create_dir_all(sibling(&dir, ".removed").join("x")).unwrap();
        recover(&dir);
        assert_nothing_left(&dir);
    }

    #[test]
    fn links_inside_are_kept_others_refuse_the_archive() {
        // Safe: relative, and inside the folder read from the link's own folder.
        for make in [tar_gz_of as fn(&[Entry]) -> Vec<u8>, zip_of] {
            let archive = make(&[Entry::File("app/lib/a.so", b"a"), Entry::Link("app/lib/b.so", "a.so"), Entry::Link("app/bin/java", "../lib/./a.so")]);
            let (work, result) = unpack(&archive);
            result.unwrap();
            let out = work.path().join("out");
            assert!(out.join("app").join("lib").join("a.so").is_file());
            let created = std::fs::symlink_metadata(out.join("app").join("lib").join("b.so"));
            // Created on Unix (where G-Earth's bundles need them), left out on Windows.
            if cfg!(unix) {
                assert!(created.unwrap().file_type().is_symlink());
                assert_eq!(std::fs::read(out.join("app").join("bin").join("java")).unwrap(), b"a");
            } else {
                assert!(created.is_err());
            }
        }

        // Escaping, absolute or empty targets refuse the whole archive, also on Windows.
        let refused = [
            ("app/link", "../../outside"),
            ("link", ".."),
            ("app/lib/link", "../../../etc/passwd"),
            ("app/link", "/etc/passwd"),
            ("app/link", "C:/Windows"),
            ("app/link", "a:stream"),
        ];
        for make in [tar_gz_of as fn(&[Entry]) -> Vec<u8>, zip_of] {
            for (at, to) in refused {
                let (work, result) = unpack(&make(&[Entry::File("app/a.txt", b"a"), Entry::Link(at, to)]));
                let error = result.expect_err(&format!("{at} -> {to} accepted"));
                assert!(format!("{error:#}").contains("link"), "{error:#}");
                assert!(std::fs::symlink_metadata(work.path().join("out").join(at)).is_err());
            }
            // Nothing is written through a link, even one that points inside.
            for entries in [
                vec![Entry::Link("x", "."), Entry::File("x/evil.txt", b"evil")],
                vec![Entry::Link("s", "."), Entry::Link("s/up", "..")],
            ] {
                let (_work, result) = unpack(&make(&entries));
                assert!(format!("{:#}", result.unwrap_err()).contains("through a link"));
            }
        }
    }

    #[tokio::test]
    async fn running_is_a_listening_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_running([port]).await);
        drop(listener);
        assert!(!is_running([port]).await);
    }
}
