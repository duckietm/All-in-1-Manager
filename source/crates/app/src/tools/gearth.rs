//! G-Earth on this computer (Tools → G-Earth), ported from the catalogue ripper's `gearth_install.rs`: the official
//! release (github.com/G-Realm/G-Earth, MIT licence, Java included) for this platform, downloaded into
//! `configuration/tools/G-Earth/` and started from there. The page asks before anything is downloaded, removed or
//! started; this module only does what it is told.
//!
//! The download is checked against the SHA-256 the release publishes (GitHub's asset digest, or a `<asset>.sha256`
//! file) when there is one, capped at [`MAX_DOWNLOAD`], and unpacked into `G-Earth.new` next to the folder, which then
//! takes the folder's place by renaming: a failed, cancelled or interrupted install never leaves a half-unpacked
//! G-Earth behind. Archive entries outside the folder (`..`, absolute paths) refuse the whole archive; links are never
//! created.

use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::update::{self, GithubRelease};

pub const REPOSITORY: &str = "G-Realm/G-Earth";

#[cfg(windows)]
pub const ASSET: &str = "G-Earth-windows-x64.zip";
#[cfg(target_os = "linux")]
pub const ASSET: &str = "G-Earth-linux-x64.tar.gz";
#[cfg(target_os = "macos")]
pub const ASSET: &str = "G-Earth-mac-x64.zip";

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

/// `configuration/tools/G-Earth`.
pub fn folder() -> PathBuf {
    aio_core::config_dir::directory().join("tools").join("G-Earth")
}

/// `G-Earth<suffix>` next to the folder: `.part` (the download), `.new` (being unpacked), `.old` (being replaced).
fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "G-Earth".into());
    name.push(suffix);
    dir.with_file_name(name)
}

/// The G-Earth launcher in `dir`, at the top or one folder down (an archive with one top folder).
pub fn installed_launcher(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join(LAUNCHER);
    if direct.is_file() {
        return Some(direct);
    }
    let mut candidates: Vec<PathBuf> =
        std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path().join(LAUNCHER)).filter(|p| p.is_file()).collect();
    candidates.sort();
    candidates.pop()
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

/// Puts the previous G-Earth back when the program stopped between the two renames of an update.
pub fn recover(dir: &Path) {
    let old = sibling(dir, ".old");
    if !dir.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, dir);
    }
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
    pub asset_url: String,
    pub size: u64,
    pub checksum: Checksum,
}

impl Release {
    pub fn verifiable(&self) -> bool {
        self.checksum != Checksum::None
    }
}

/// The newest published release (`api`: the GitHub API, a local server in tests). Downloads nothing.
pub async fn latest(api: &str) -> Result<Release> {
    let url = format!("{api}/repos/{REPOSITORY}/releases/latest");
    let response = update::client()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("Could not reach GitHub")?;
    if !response.status().is_success() {
        bail!("GitHub answered {} for {url}", response.status());
    }
    release_for(response.json().await.context("Unexpected answer from GitHub")?)
}

fn release_for(release: GithubRelease) -> Result<Release> {
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == ASSET)
        .with_context(|| format!("The newest G-Earth release ({}) has no {ASSET}.", release.tag_name))?;
    if asset.size > MAX_DOWNLOAD {
        bail!("The G-Earth download is {} MiB, more than the {} MiB accepted.", asset.size / (1024 * 1024), MAX_DOWNLOAD / (1024 * 1024));
    }
    let checksum_file = format!("{ASSET}.sha256");
    let checksum = match asset.sha256() {
        Some(digest) => Checksum::Digest(digest),
        None => match release.assets.iter().find(|a| a.name == checksum_file) {
            Some(file) => Checksum::File(file.browser_download_url.clone()),
            None => Checksum::None,
        },
    };
    Ok(Release {
        version: release.tag_name.trim_start_matches('v').to_string(),
        asset_url: asset.browser_download_url.clone(),
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
    let (archive, target, token) = (part.to_path_buf(), new.to_path_buf(), cancel.clone());
    tokio::task::spawn_blocking(move || extract(&archive, &target, &token)).await.context("Unpacking stopped")??;
    if installed_launcher(new).is_none() {
        bail!("{LAUNCHER} was not found in the G-Earth download.");
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
        std::fs::rename(dir, &old)
            .with_context(|| format!("Could not replace {} (is G-Earth still open?)", dir.display()))?;
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

/// Removes the G-Earth folder and whatever an interrupted install left next to it.
pub fn remove(dir: &Path) -> Result<()> {
    for leftover in [sibling(dir, ".new"), sibling(dir, ".old")] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    let _ = std::fs::remove_file(sibling(dir, ".part"));
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("Could not remove {} (is G-Earth still open?)", dir.display()))?;
    }
    Ok(())
}

/// Starts the launcher. On Windows it runs elevated (Windows asks first): for the Flash/AIR client G-Earth edits the
/// hosts file, which needs administrator rights. Blocks until Windows' question is answered.
pub fn start(launcher: &Path) -> Result<()> {
    let launcher = std::path::absolute(launcher)?;
    let dir = launcher.parent().unwrap_or(Path::new(".")).to_path_buf();
    #[cfg(windows)]
    {
        // PowerShell single-quoted string: a quote is written twice.
        let quote = |p: &Path| p.display().to_string().replace('\'', "''");
        let script = format!("Start-Process -FilePath '{}' -WorkingDirectory '{}' -Verb RunAs", quote(&launcher), quote(&dir));
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .context("Could not start G-Earth")?;
        if !status.success() {
            bail!(
                "G-Earth was not started (the administrator prompt was refused or failed). Start {} as administrator yourself.",
                launcher.display()
            );
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(&launcher)
            .current_dir(&dir)
            .spawn()
            .with_context(|| format!("Could not start {}", launcher.display()))?;
    }
    Ok(())
}

/// Unpacks the platform's archive into `target` (created; must not exist).
fn extract(archive: &Path, target: &Path, cancel: &CancellationToken) -> Result<()> {
    std::fs::create_dir(target).with_context(|| format!("Could not create {}", target.display()))?;
    if ASSET.ends_with(".zip") {
        extract_zip(archive, target, cancel)
    } else {
        extract_tar_gz(archive, target, cancel)
    }
}

/// An archive path as a path inside the target: only plain names (and `.`), nothing absolute, no `..`, no drive or
/// stream (`:`).
fn inside(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if !name.to_string_lossy().contains(':') => out.push(name),
            Component::CurDir => {}
            _ => bail!("The archive holds an unsafe path ({}); nothing was installed.", path.display()),
        }
    }
    if out.as_os_str().is_empty() {
        bail!("The archive holds an empty path; nothing was installed.");
    }
    Ok(out)
}

/// What an archive has unpacked so far.
#[derive(Default)]
struct Budget {
    bytes: u64,
    entries: usize,
}

impl Budget {
    fn entry(&mut self, cancel: &CancellationToken) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            bail!("The archive has more than {MAX_ENTRIES} entries; nothing was installed.");
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
        budget.entry(cancel)?;
        let mut entry = zip.by_index(index)?;
        let name = PathBuf::from(entry.name());
        let relative = inside(&name)?;
        if entry.enclosed_name().is_none() {
            bail!("The archive holds an unsafe path ({}); nothing was installed.", name.display());
        }
        // Links are never created.
        if entry.is_symlink() {
            continue;
        }
        let out = target.join(relative);
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
        budget.entry(cancel)?;
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let path = entry.path()?.into_owned();
        let relative = inside(&path)?;
        let out = target.join(relative);
        if kind.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else if kind.is_file() {
            budget.write(&mut entry, &out)?;
            #[cfg(unix)]
            set_mode(&out, entry.header().mode().unwrap_or(0o644))?;
        }
        // Symbolic and hard links, devices, FIFOs: never created.
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

    pub struct Server {
        pub base: String,
        pub hits: Arc<Mutex<Vec<String>>>,
        routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    pub async fn serve(routes: HashMap<String, Vec<u8>>) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::new(Mutex::new(routes));
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
                    let body = routes.lock().get(&path).cloned();
                    let (status, body) = match body {
                        Some(body) => ("200 OK", body),
                        None => ("404 Not Found", b"not found".to_vec()),
                    };
                    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Server { base, hits, routes }
    }

    impl Server {
        pub fn route(&self, path: &str, body: Vec<u8>) {
            self.routes.lock().insert(path.to_string(), body);
        }

        pub fn asked(&self, path: &str) -> bool {
            self.hits.lock().iter().any(|p| p == path)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::io::Write;

    use super::*;

    /// One archive entry: a file with content, or a symbolic link (to its target).
    pub(crate) enum Entry<'a> {
        File(&'a str, &'a [u8]),
        Link(&'a str, &'a str),
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
                Entry::Link(name, to) => zip.add_symlink(*name, *to, options).unwrap(),
            }
        }
        zip.finish().unwrap().into_inner()
    }

    pub(crate) fn tar_gz_of(entries: &[Entry]) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            match entry {
                Entry::File(name, bytes) => {
                    header.set_size(bytes.len() as u64);
                    header.set_mode(0o755);
                    header.set_entry_type(tar::EntryType::Regular);
                    // Written raw, so a path the builder would refuse (`..`) can be tested too.
                    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    tar.append(&header, *bytes).unwrap();
                }
                Entry::Link(name, to) => {
                    header.set_size(0);
                    header.set_entry_type(tar::EntryType::Symlink);
                    tar.append_link(&mut header, name, to).unwrap();
                }
            }
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    /// The platform's archive.
    pub(crate) fn archive_of(entries: &[Entry]) -> Vec<u8> {
        if ASSET.ends_with(".zip") {
            zip_of(entries)
        } else {
            tar_gz_of(entries)
        }
    }

    pub(crate) fn release_json(version: &str, base: &str, size: usize, digest: Option<&str>, checksum_file: bool) -> Vec<u8> {
        let mut assets = vec![serde_json::json!({
            "name": ASSET,
            "browser_download_url": format!("{base}/download/{ASSET}"),
            "size": size,
            "digest": digest.map(|d| format!("sha256:{d}")),
        })];
        if checksum_file {
            assets.push(serde_json::json!({
                "name": format!("{ASSET}.sha256"),
                "browser_download_url": format!("{base}/download/{ASSET}.sha256"),
                "size": 64,
            }));
        }
        serde_json::to_vec(&serde_json::json!({
            "tag_name": format!("v{version}"),
            "html_url": format!("{base}/release"),
            "assets": assets,
        }))
        .unwrap()
    }

    pub(crate) fn good_archive() -> Vec<u8> {
        archive_of(&[Entry::File(&format!("G-Earth-1.5.4/{LAUNCHER}"), b"MZ"), Entry::File("G-Earth-1.5.4/G-Earth.jar", b"jar")])
    }

    pub(crate) const RELEASE_PATH: &str = "/repos/G-Realm/G-Earth/releases/latest";

    fn download_path() -> String {
        format!("/download/{ASSET}")
    }

    /// A fake GitHub with one release whose file is `archive`; `digest` / `file` publish its SHA-256 (or a `wrong` one).
    pub(crate) async fn github(archive: Vec<u8>, digest: bool, file: bool, wrong: bool) -> fake::Server {
        let sha = if wrong { "0".repeat(64) } else { update::sha256_of(&archive) };
        let server = fake::serve(HashMap::new()).await;
        server.route(&format!("{}.sha256", download_path()), format!("{sha}  {ASSET}
").into_bytes());
        server.route(RELEASE_PATH, release_json("1.5.4", &server.base, archive.len(), digest.then_some(&sha), file));
        server.route(&download_path(), archive);
        server
    }

    /// Unpacks `archive` (zip or tar.gz) into `<temp>/out`.
    fn unpack(archive: &[u8]) -> (tempfile::TempDir, Result<()>) {
        let work = tempfile::tempdir().unwrap();
        let file = work.path().join("archive");
        std::fs::write(&file, archive).unwrap();
        let target = work.path().join("out");
        std::fs::create_dir(&target).unwrap();
        let result = if archive.starts_with(b"PK") {
            extract_zip(&file, &target, &CancellationToken::new())
        } else {
            extract_tar_gz(&file, &target, &CancellationToken::new())
        };
        (work, result)
    }

    fn assert_nothing_left(dir: &Path) {
        for suffix in [".part", ".new", ".old"] {
            assert!(!sibling(dir, suffix).exists(), "{suffix} left behind");
        }
    }

    #[test]
    fn installed_launcher_found_top_or_one_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("G-Earth");
        assert!(installed_launcher(&root).is_none());
        let nested = root.join("G-Earth-1.5.4");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(LAUNCHER), b"").unwrap();
        assert_eq!(installed_launcher(&root), Some(nested.join(LAUNCHER)));
        std::fs::write(root.join(LAUNCHER), b"").unwrap();
        assert_eq!(installed_launcher(&root), Some(root.join(LAUNCHER)));
        // Put there by hand: no version, not verified.
        assert_eq!(installed(&root), Some(Installed { launcher: root.join(LAUNCHER), version: None, verified: false }));
    }

    #[tokio::test]
    async fn download_is_verified_or_marked_unverified() {
        // (GitHub digest, .sha256 file) → verified.
        for (digest, file, verified) in [(true, false, true), (false, true, true), (true, true, true), (false, false, false)] {
            let server = github(good_archive(), digest, file, false).await;
            let release = latest(&server.base).await.unwrap();
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
            let release = latest(&server.base).await.unwrap();
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("G-Earth");
            let error = install(&release, &dir, &CancellationToken::new(), |_| {}).await.unwrap_err();
            assert!(format!("{error:#}").contains("not the file the release publishes"), "{error:#}");
            assert!(!dir.exists());
            assert_nothing_left(&dir);
        }

        // Over the cap: stopped, nothing kept.
        let server = github(good_archive(), false, false, false).await;
        let release = latest(&server.base).await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("G-Earth");
        let error = install_capped(&release, &dir, &CancellationToken::new(), |_| {}, 16).await.unwrap_err();
        assert!(format!("{error:#}").contains("bigger than"), "{error:#}");
        assert!(!dir.exists());
        assert_nothing_left(&dir);

        // A release that says it is too big is refused before any download.
        let mut too_big: GithubRelease = serde_json::from_slice(&release_json("2.0", "http://x", 0, None, false)).unwrap();
        too_big.assets[0].size = MAX_DOWNLOAD + 1;
        assert!(release_for(too_big).is_err());
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

        // Links are never created.
        for archive in [zip_of(&[Entry::File("x/a.txt", b"a"), Entry::Link("x/link", "../../outside")]), tar_gz_of(&[Entry::File("x/a.txt", b"a"), Entry::Link("x/link", "/etc/passwd")])] {
            let (work, result) = unpack(&archive);
            result.unwrap();
            let target = work.path().join("out");
            assert!(target.join("x").join("a.txt").is_file());
            assert!(std::fs::symlink_metadata(target.join("x").join("link")).is_err());
        }

        // An install from an archive with an unsafe path changes nothing.
        let bad = archive_of(&[Entry::File(&format!("G-Earth-1.5.4/{LAUNCHER}"), b"new"), Entry::File("../evil.txt", b"evil")]);
        let server = github(bad, true, false, false).await;
        let release = latest(&server.base).await.unwrap();
        assert!(install(&release, &dir, &CancellationToken::new(), |_| {}).await.is_err());
        assert_eq!(std::fs::read(dir.join("G-Earth-1.5.3").join(LAUNCHER)).unwrap(), b"old");
        assert!(!root.path().join("evil.txt").exists());
        assert_nothing_left(&dir);

        // Cancelled: nothing downloaded, nothing changed.
        let server = github(good_archive(), true, false, false).await;
        let release = latest(&server.base).await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = install(&release, &dir, &cancel, |_| {}).await.unwrap_err();
        assert!(error.is::<Cancelled>());
        assert_eq!(std::fs::read(dir.join("G-Earth-1.5.3").join(LAUNCHER)).unwrap(), b"old");
        assert_nothing_left(&dir);

        // The program stopped between the two renames of an update: the previous G-Earth comes back.
        std::fs::rename(&dir, sibling(&dir, ".old")).unwrap();
        std::fs::create_dir(sibling(&dir, ".new")).unwrap();
        recover(&dir);
        assert!(installed(&dir).is_some());

        // A good update replaces the old folder as a whole.
        let installed_now = install(&release, &dir, &CancellationToken::new(), |_| {}).await.unwrap();
        assert_eq!(installed_now.launcher, dir.join("G-Earth-1.5.4").join(LAUNCHER));
        assert!(!dir.join("G-Earth-1.5.3").exists());
        assert_nothing_left(&dir);

        remove(&dir).unwrap();
        assert!(!dir.exists());
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
