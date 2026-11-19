//! Data packs: what a plugin needs that is no longer built into the program (the client images, the default
//! catalogue). Each one is a file of the GitHub release (`plugin-<id>.zip`, with its `.sha256`), downloaded on demand
//! into configuration/plugins/<id>/. A pack that is half downloaded or does not match its checksum is never used.
//!
//! Where a pack is found, first wins:
//! 1. configuration/plugins/<id>/ (downloaded from the Plugins page);
//! 2. plugins/<id>/ next to the program (what build.cmd / build.sh put in public/release, to hand out);
//! 3. for the client images, Assets/ next to the program (what older versions came with);
//! 4. in a debug build, the source tree, so `cargo run` works without downloading anything.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::update;

/// How a pack is kept once downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The release zip is unpacked into the pack's folder.
    Folder,
    /// The release file is kept as it is, under this name, in the pack's folder.
    File(&'static str),
}

#[derive(Debug)]
pub struct Pack {
    /// Also the folder name and the release file `plugin-<id>.zip`.
    pub id: &'static str,
    /// English; translated with `t()`.
    pub title: &'static str,
    pub kind: Kind,
    /// About how big the download is, for the Plugins page.
    pub size: &'static str,
    /// The pack in the source tree, relative to this crate (debug builds only).
    dev_source: &'static str,
}

pub static CLIENT_ASSETS: Pack = Pack {
    id: "client-assets",
    title: crate::i18n::n("Client images"),
    kind: Kind::Folder,
    size: "10 MB",
    dev_source: "../../Assets",
};

pub static CATALOG: Pack = Pack {
    id: "default-catalog",
    title: crate::i18n::n("Default catalogue"),
    kind: Kind::File("Full_Catalog.zip"),
    size: "2 MB",
    dev_source: "assets/catalog/Full_Catalog.zip",
};

#[cfg(test)]
static ALL: [&Pack; 2] = [&CLIENT_ASSETS, &CATALOG];

/// configuration/plugins/
pub fn root() -> PathBuf {
    aio_core::config_dir::directory().join("plugins")
}

impl Pack {
    /// Where a download goes: configuration/plugins/<id>/.
    pub fn folder(&self) -> PathBuf {
        root().join(self.id)
    }

    /// The release file.
    pub fn asset_name(&self) -> String {
        format!("plugin-{}.zip", self.id)
    }

    /// The downloaded copy, when it is there and complete.
    fn downloaded(&self) -> Option<PathBuf> {
        let folder = self.folder();
        let path = match self.kind {
            Kind::Folder => folder,
            Kind::File(name) => folder.join(name),
        };
        let complete = match self.kind {
            Kind::Folder => path.is_dir() && std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_some()),
            Kind::File(_) => path.is_file(),
        };
        complete.then_some(path)
    }

    /// Where the pack is: the folder (Kind::Folder) or the file (Kind::File); None when it is not on this computer.
    pub fn location(&self) -> Option<PathBuf> {
        if let Some(path) = self.downloaded() {
            return Some(path);
        }
        // plugins/<id>/ next to the program: what build.cmd / build.sh put in public/release to hand out.
        if let Some(beside) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("plugins").join(self.id))) {
            let path = match self.kind {
                Kind::Folder => beside,
                Kind::File(name) => beside.join(name),
            };
            if path.exists() {
                return Some(path);
            }
        }
        if self.id == CLIENT_ASSETS.id {
            let beside = aio_remote::emulator_assets::local_assets();
            if beside.is_dir() {
                return Some(beside);
            }
        }
        if cfg!(debug_assertions) {
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(self.dev_source);
            if source.exists() {
                return Some(source);
            }
        }
        None
    }

    pub fn is_installed(&self) -> bool {
        self.location().is_some()
    }

    /// True when the pack was downloaded into configuration/plugins (so Remove can delete it).
    pub fn is_removable(&self) -> bool {
        self.downloaded().is_some()
    }

    /// The bytes of a Kind::File pack.
    pub fn read_file(&self) -> Result<Vec<u8>> {
        let path = self.location().with_context(|| format!("The {} pack is not on this computer: download it on the Plugins page.", self.title))?;
        std::fs::read(&path).with_context(|| format!("Could not read {}", path.display()))
    }

    /// The release URLs to try: this version's release first, then the newest one.
    fn urls(&self) -> [String; 2] {
        let name = self.asset_name();
        let repository = update::REPOSITORY;
        [
            format!("https://github.com/{repository}/releases/download/{}{}/{name}", update::TAG_PREFIX, update::CURRENT_VERSION),
            format!("https://github.com/{repository}/releases/latest/download/{name}"),
        ]
    }

    /// Downloads the pack and puts it in place. `progress` gets (done, total) bytes.
    pub async fn download(&self, progress: impl Fn(u64, u64)) -> Result<()> {
        let client = update::client();
        let mut last_error = None;
        for url in self.urls() {
            match fetch(&client, &url, &progress).await {
                Ok(bytes) => {
                    let (kind, into) = (self.kind, self.folder());
                    return tokio::task::spawn_blocking(move || install(kind, &bytes, &into)).await?;
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No download address"))).context(format!(
            "The {} pack could not be downloaded. Check the internet connection, or download {} by hand from {} and unpack it into {}.",
            self.title,
            self.asset_name(),
            update::download_page(),
            self.folder().display()
        ))
    }

    /// Deletes the downloaded copy (never Assets/ next to the program, nor the source tree).
    pub fn remove(&self) -> Result<()> {
        let folder = self.folder();
        if folder.exists() {
            std::fs::remove_dir_all(&folder).with_context(|| format!("Could not delete {}", folder.display()))?;
        }
        Ok(())
    }
}

/// The file and its published checksum; a file without one, or that does not match it, is refused.
async fn fetch(client: &reqwest::Client, url: &str, progress: &impl Fn(u64, u64)) -> Result<Vec<u8>> {
    let published = client.get(format!("{url}.sha256")).send().await?.error_for_status()?.text().await?;
    let expected = update::checksum_in(&published).context("The published checksum could not be read")?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    let total = response.content_length().unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }
    if total > 0 && bytes.len() as u64 != total {
        bail!("The download stopped early ({} of {total} bytes).", bytes.len());
    }
    verify(&bytes, &expected)?;
    Ok(bytes)
}

fn verify(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = update::sha256_of(bytes);
    if actual != expected {
        bail!("The downloaded file is not the one the release publishes (expected {expected}, got {actual}).");
    }
    Ok(())
}

/// Unpacks (or writes) the pack next to its folder first, then swaps it in: a failed or half install never replaces
/// a working pack.
fn install(kind: Kind, bytes: &[u8], into: &Path) -> Result<()> {
    let parent = into.parent().context("No folder for the plugins")?;
    std::fs::create_dir_all(parent)?;
    let name = into.file_name().context("No pack name")?.to_string_lossy().into_owned();
    let partial = parent.join(format!(".{name}.partial"));
    let old = parent.join(format!(".{name}.old"));
    // Every pack is a zip (a Kind::File pack too: the catalogue): check it opens before touching anything.
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("The downloaded file is not a zip")?;
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial)?;
    let written = match kind {
        Kind::File(file) => std::fs::write(partial.join(file), bytes).map_err(anyhow::Error::from),
        Kind::Folder => unpack(bytes, &partial),
    };
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(error);
    }
    let _ = std::fs::remove_dir_all(&old);
    if into.exists() {
        std::fs::rename(into, &old).with_context(|| format!("Could not move {} aside", into.display()))?;
    }
    if let Err(error) = std::fs::rename(&partial, into) {
        let _ = std::fs::rename(&old, into);
        return Err(error).with_context(|| format!("Could not put the pack in {}", into.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn unpack(bytes: &[u8], into: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("The downloaded file is not a zip")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        // enclosed_name drops entries that would land outside the folder ("../", absolute paths).
        let Some(relative) = entry.enclosed_name() else { continue };
        let path = into.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&path)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn a_folder_pack_is_unpacked_and_replaces_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("plugins").join("client-assets");
        install(Kind::Folder, &zip_of(&[("images/a.png", b"1"), ("old.png", b"x")]), &into).unwrap();
        install(Kind::Folder, &zip_of(&[("images/a.png", b"2")]), &into).unwrap();
        assert_eq!(std::fs::read(into.join("images/a.png")).unwrap(), b"2");
        assert!(!into.join("old.png").exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("plugins")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("client-assets")]);
    }

    #[test]
    fn a_broken_download_keeps_the_working_pack() {
        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("default-catalog");
        install(Kind::File("Full_Catalog.zip"), &zip_of(&[("Catalog_Export.sql", b"--")]), &into).unwrap();
        assert!(install(Kind::File("Full_Catalog.zip"), b"not a zip", &into).is_err());
        assert!(zip::ZipArchive::new(std::fs::File::open(into.join("Full_Catalog.zip")).unwrap()).is_ok());
        assert!(!dir.path().join(".default-catalog.partial").exists());
    }

    #[test]
    fn entries_outside_the_folder_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("pack");
        install(Kind::Folder, &zip_of(&[("../evil.txt", b"x"), ("ok.txt", b"y")]), &into).unwrap();
        assert!(into.join("ok.txt").is_file());
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn the_checksum_has_to_match() {
        let data = b"abc";
        assert!(verify(data, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").is_ok());
        assert!(verify(data, &"0".repeat(64)).is_err());
    }

    #[test]
    fn a_debug_build_finds_the_packs_in_the_source_tree() {
        for pack in ALL {
            assert!(pack.is_installed(), "{} not found", pack.id);
        }
    }
}
