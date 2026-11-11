//! G-Earth on demand: when the catalogue rip finds no G-Earth running, it offers to download the
//! official release (github.com/G-Realm/G-Earth, MIT licence, Java included) into a folder the
//! caller chooses, once, and to start it. Nothing is downloaded or started without a "yes" from
//! the `Ask` callback.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio_util::sync::CancellationToken;

use crate::{Ask, Cancelled, Log};

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

/// The G-Earth launcher in `dir`, at the top or one folder down (an archive with one top folder).
pub fn installed_launcher(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join(LAUNCHER);
    if direct.is_file() {
        return Some(direct);
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(LAUNCHER))
        .filter(|p| p.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

/// True when something listens on one of G-Earth's extension ports.
pub async fn is_running(ports: &[u16]) -> bool {
    for &port in ports {
        let connect = tokio::net::TcpStream::connect(("127.0.0.1", port));
        if matches!(
            tokio::time::timeout(Duration::from_millis(400), connect).await,
            Ok(Ok(_))
        ) {
            return true;
        }
    }
    false
}

/// Asks to start G-Earth from `dir`, downloading it there first when it is missing. Returns true
/// when it was started.
pub async fn offer_to_start(
    dir: &Path,
    log: Log<'_>,
    ask: Ask<'_>,
    cancel: &CancellationToken,
) -> Result<bool> {
    let launcher = match installed_launcher(dir) {
        Some(launcher) => {
            if !ask(format!(
                "G-Earth is not running. Start the G-Earth in {} now?",
                dir.display()
            ))
            .await
            {
                return Ok(false);
            }
            launcher
        }
        None => {
            if !ask(format!(
                "G-Earth is not running and is not installed yet. Download the official G-Earth \
                 (about 100 MB, MIT licence, github.com/{REPOSITORY}) into {} and start it?",
                dir.display()
            ))
            .await
            {
                log("Start G-Earth yourself; the ripper keeps looking for it.");
                return Ok(false);
            }
            download_and_extract(dir, log, cancel).await?
        }
    };
    start(&launcher, log)?;
    log("G-Earth started. Close your Habbo client, press Connect in G-Earth, then open the client and log in.");
    Ok(true)
}

/// Downloads the newest release for this platform into `dir` and unpacks it; returns the launcher.
pub async fn download_and_extract(
    dir: &Path,
    log: Log<'_>,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    // GitHub sends the newest release's asset for this link, without an API call.
    let url = format!("https://github.com/{REPOSITORY}/releases/latest/download/{ASSET}");
    log(&format!("Downloading {url}"));
    let mut response = crate::http::client()
        .get(&url)
        .timeout(Duration::from_secs(30 * 60))
        .send()
        .await
        .context("Could not reach GitHub")?;
    if !response.status().is_success() {
        bail!("GitHub answered {} for {url}", response.status());
    }
    let total = response.content_length().unwrap_or(0);
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let archive_path = dir.join(format!("{ASSET}.part"));
    let mut file = std::fs::File::create(&archive_path)?;
    let mut done = 0u64;
    let mut last_step = u64::MAX;
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                drop(file);
                let _ = std::fs::remove_file(&archive_path);
                return Err(Cancelled.into());
            }
            chunk = response.chunk() => chunk?,
        };
        let Some(chunk) = chunk else { break };
        file.write_all(&chunk)?;
        done += chunk.len() as u64;
        if let Some(percent) = (done * 100).checked_div(total) {
            if percent / 10 != last_step {
                last_step = percent / 10;
                log(&format!(
                    "G-Earth download: {} / {} MB ({percent}%)",
                    done / (1024 * 1024),
                    total / (1024 * 1024)
                ));
            }
        }
    }
    drop(file);

    log(&format!("Unpacking into {} ...", dir.display()));
    let archive = archive_path.clone();
    let target = dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract(&archive, &target)).await??;
    let _ = std::fs::remove_file(&archive_path);
    installed_launcher(dir).with_context(|| {
        format!(
            "{LAUNCHER} was not found in the G-Earth download ({})",
            dir.display()
        )
    })
}

fn extract(archive: &Path, target: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    std::fs::File::open(archive)?.read_to_end(&mut bytes)?;
    if ASSET.ends_with(".zip") {
        zip::ZipArchive::new(std::io::Cursor::new(bytes))?.extract(target)?;
    } else {
        let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        tar::Archive::new(gz).unpack(target)?;
    }
    Ok(())
}

/// Starts G-Earth. On Windows it runs elevated (Windows asks first): for the Flash/AIR client
/// G-Earth edits the hosts file, which needs administrator rights.
fn start(launcher: &Path, log: Log<'_>) -> Result<()> {
    let launcher = std::path::absolute(launcher)?;
    let dir = launcher.parent().unwrap_or(Path::new(".")).to_path_buf();
    #[cfg(windows)]
    {
        // PowerShell single-quoted string: a quote is written twice.
        let quote = |p: &Path| p.display().to_string().replace('\'', "''");
        let script = format!(
            "Start-Process -FilePath '{}' -WorkingDirectory '{}' -Verb RunAs",
            quote(&launcher),
            quote(&dir)
        );
        log("Windows will ask to let G-Earth make changes: answer Yes (it edits the hosts file).");
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .context("Could not start G-Earth")?;
        if !status.success() {
            bail!("G-Earth was not started (the administrator prompt was refused or failed). Start {} as administrator yourself.", launcher.display());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = log;
        std::process::Command::new(&launcher)
            .current_dir(&dir)
            .spawn()
            .with_context(|| format!("Could not start {}", launcher.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_launcher_at_the_top_or_one_folder_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("G-Earth");
        assert!(installed_launcher(&root).is_none());
        let nested = root.join("G-Earth-1.5.4");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(LAUNCHER), b"").unwrap();
        assert_eq!(installed_launcher(&root), Some(nested.join(LAUNCHER)));
        std::fs::write(root.join(LAUNCHER), b"").unwrap();
        assert_eq!(installed_launcher(&root), Some(root.join(LAUNCHER)));
    }

    #[test]
    fn unpacks_a_zip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("g.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        zip.start_file("G-Earth.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"MZ").unwrap();
        zip.finish().unwrap();
        if ASSET.ends_with(".zip") {
            extract(&archive, &dir.path().join("out")).unwrap();
            assert!(dir.path().join("out").join("G-Earth.exe").is_file());
        }
    }

    #[tokio::test]
    async fn nothing_happens_without_a_yes() {
        let dir = tempfile::tempdir().unwrap();
        let asked = std::sync::Mutex::new(Vec::new());
        let log = |_: &str| {};
        let ask = |q: String| -> futures::future::BoxFuture<'static, bool> {
            asked.lock().unwrap().push(q);
            Box::pin(async { false })
        };
        let started = offer_to_start(dir.path(), &log, &ask, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!started);
        assert!(asked.lock().unwrap()[0].contains("Download the official G-Earth"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
