//! Packs the configuration directory, including SSH keys and MariaDB passwords, into one password-encrypted file for
//! moving to another computer: AES-256-GCM with a key derived by PBKDF2-SHA256. The same file format as the C#
//! installer ("AIOSET01", salt, iterations, nonce, tag, cipher), so backups move between both.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use anyhow::{bail, Context, Result};
use rand::RngCore;

use crate::config_dir;

pub const EXTENSION: &str = ".aio-settings";
pub const MINIMUM_PASSWORD_LENGTH: usize = 8;

const MAGIC: &[u8; 8] = b"AIOSET01";
const ITERATIONS: u32 = 600_000;
const SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;

fn derive(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), salt, iterations, &mut key);
    key
}

/// Returns how many files went into `file`.
pub fn export(file: &Path, password: &str) -> Result<usize> {
    export_directory(&config_dir::directory(), file, password)
}

pub fn export_directory(directory: &Path, file: &Path, password: &str) -> Result<usize> {
    let (zip, count) = zip_directory(directory)?;
    let mut salt = [0u8; SALT_SIZE];
    let mut nonce = [0u8; NONCE_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let key = derive(password, &salt, ITERATIONS);
    let cipher = aes_gcm::Aes256Gcm::new(&key.into());
    // aes-gcm appends the tag; the file keeps it before the cipher text.
    let sealed = cipher.encrypt(&nonce.into(), Payload { msg: &zip, aad: MAGIC }).map_err(|_| anyhow::anyhow!("Encryption failed"))?;
    let (body, tag) = sealed.split_at(sealed.len() - TAG_SIZE);

    let mut output = std::fs::File::create(file).with_context(|| format!("Could not write {}", file.display()))?;
    output.write_all(MAGIC)?;
    output.write_all(&salt)?;
    output.write_all(&(ITERATIONS as i32).to_le_bytes())?;
    output.write_all(&nonce)?;
    output.write_all(tag)?;
    output.write_all(body)?;
    Ok(count)
}

/// Replaces the configuration directory with the backup and keeps the current one as configuration.bak-<time>.
/// Returns that folder (None if there was nothing to keep) and the file count.
pub fn import(file: &Path, password: &str) -> Result<(Option<PathBuf>, usize)> {
    import_into(&config_dir::directory(), file, password)
}

pub fn import_into(target: &Path, file: &Path, password: &str) -> Result<(Option<PathBuf>, usize)> {
    let data = std::fs::read(file).with_context(|| format!("Could not read {}", file.display()))?;
    let header = MAGIC.len() + SALT_SIZE + 4 + NONCE_SIZE + TAG_SIZE;
    if data.len() < header || &data[..MAGIC.len()] != MAGIC {
        bail!("This is not a settings backup of the All-in-1 Manager.");
    }
    let mut at = MAGIC.len();
    let salt = &data[at..at + SALT_SIZE];
    at += SALT_SIZE;
    let iterations = i32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    at += 4;
    let nonce = &data[at..at + NONCE_SIZE];
    at += NONCE_SIZE;
    let tag = &data[at..at + TAG_SIZE];
    at += TAG_SIZE;
    let body = &data[at..];
    if !(100_000..=10_000_000).contains(&iterations) {
        bail!("The settings backup is damaged.");
    }

    let key = derive(password, salt, iterations as u32);
    let cipher = aes_gcm::Aes256Gcm::new(&key.into());
    let sealed = [body, tag].concat();
    let zip = cipher
        .decrypt(nonce.into(), Payload { msg: &sealed, aad: MAGIC })
        .map_err(|_| anyhow::anyhow!("Wrong password, or the file is damaged."))?;

    let parent = target.parent().context("The configuration folder has no parent")?;
    let incoming = parent.join(format!(".configuration-import-{:032x}", rand::random::<u128>()));
    let count = unzip(&zip, &incoming)?;

    let mut previous = None;
    let has_entries = std::fs::read_dir(target).map(|mut d| d.next().is_some()).unwrap_or(false);
    if target.is_dir() && has_entries {
        let path = parent.join(format!("configuration.bak-{}", config_dir::timestamp()));
        std::fs::rename(target, &path)?;
        previous = Some(path);
    } else if target.is_dir() {
        std::fs::remove_dir(target)?;
    }
    std::fs::rename(&incoming, target)?;
    Ok((previous, count))
}

fn zip_directory(directory: &Path) -> Result<(Vec<u8>, usize)> {
    let mut count = 0;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut files = Vec::new();
    collect(directory, &mut files);
    files.sort();
    for path in files {
        let name = path.strip_prefix(directory).unwrap().to_string_lossy().replace('\\', "/");
        if name.starts_with('.') {
            continue; // the write probe
        }
        writer.start_file(name, options)?;
        writer.write_all(&std::fs::read(&path)?)?;
        count += 1;
    }
    Ok((writer.finish()?.into_inner(), count))
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// Owner-only permissions on every folder and file, like the installer writes them.
fn unzip(zip: &[u8], directory: &Path) -> Result<usize> {
    std::fs::create_dir_all(directory)?;
    let result = (|| -> Result<usize> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).context("The settings backup is damaged.")?;
        let mut count = 0;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            if entry.name().ends_with('/') {
                continue;
            }
            let Some(relative) = entry.enclosed_name() else {
                bail!("Unsafe path in the backup: {}", entry.name());
            };
            let path = directory.join(relative);
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder)?;
                config_dir::restrict_to_owner(folder, true);
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            std::fs::write(&path, bytes)?;
            config_dir::restrict_to_owner(&path, false);
            count += 1;
        }
        config_dir::restrict_to_owner(directory, true);
        Ok(count)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(directory);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_then_import() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("keys/host")).unwrap();
        std::fs::write(source.join("keys/host/id_ed25519"), "private").unwrap();
        std::fs::write(source.join("settings.json"), "{}").unwrap();
        std::fs::write(source.join(".write-test"), "").unwrap();
        let file = root.path().join("backup.aio-settings");
        assert_eq!(export_directory(&source, &file, "password1").unwrap(), 2);

        let target = root.path().join("configuration");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("old.txt"), "old").unwrap();
        assert!(import_into(&target, &file, "wrong pass").unwrap_err().to_string().contains("Wrong password"));
        let (previous, count) = import_into(&target, &file, "password1").unwrap();
        assert_eq!(count, 2);
        assert_eq!(std::fs::read_to_string(target.join("keys/host/id_ed25519")).unwrap(), "private");
        assert!(previous.unwrap().join("old.txt").is_file());
    }
}
