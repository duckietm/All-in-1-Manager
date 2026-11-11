//! The local "configuration" directory, next to the installer or, if that is read-only, under the local application
//! data folder (`%LOCALAPPDATA%\All-in-1 Manager\configuration`, `~/.local/share/All-in-1 Manager/configuration`).
//! The same folder and file names as the C# installer, so an existing configuration keeps working.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::credentials::DatabaseCredentials;
use crate::keys::{self, AuthorizedKey, SshKeyPair};

static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

pub fn directory() -> PathBuf {
    DIRECTORY.get_or_init(resolve).clone()
}

/// Uses `path` as the configuration directory for this run (tests, screenshots). Ignored once it was resolved.
pub fn set_directory(path: PathBuf) {
    let _ = DIRECTORY.set(path);
}

fn create_restricted(path: &Path) -> PathBuf {
    let _ = std::fs::create_dir_all(path);
    restrict_to_owner(path, true);
    path.to_path_buf()
}

pub fn keys_directory(host: &str) -> PathBuf {
    create_restricted(&directory().join("keys").join(safe_name(host)))
}

/// Returns the private key path. The public and PuTTY keys are saved next to it as .pub and .ppk.
pub fn save_key_pair(host: &str, pair: &SshKeyPair) -> std::io::Result<PathBuf> {
    let path = free_path(&keys_directory(host), pair.file_name(), ".pub");
    // OpenSSH refuses private keys that others can read.
    write_private(&path, &pair.private_key_pem)?;
    std::fs::write(with_suffix(&path, ".pub"), pair.public_key.line() + "\n")?;
    write_private(&with_suffix(&path, ".ppk"), &pair.putty_key)?;
    Ok(path)
}

pub fn save_public_key(host: &str, key: &AuthorizedKey) -> std::io::Result<PathBuf> {
    let name = format!("provided_{}", key.key_type.split('@').next().unwrap_or("").replace("ssh-", ""));
    let path = with_suffix(&free_path(&keys_directory(host), &name, ""), ".pub");
    std::fs::write(&path, key.line() + "\n")?;
    Ok(path)
}

fn files_newest_first(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    fn walk(dir: &Path, recursive: bool, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if recursive {
                    walk(&path, recursive, files);
                }
            } else {
                files.push(path);
            }
        }
    }
    walk(dir, recursive, &mut files);
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    files.sort_by_key(|p| std::cmp::Reverse(modified(p)));
    files
}

/// Newest first.
pub fn saved_public_keys(host: &str) -> Vec<(PathBuf, AuthorizedKey)> {
    files_newest_first(&keys_directory(host), false)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "pub"))
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            keys::try_parse_public_key(&text).ok().map(|key| (p, key))
        })
        .collect()
}

fn is_private_key_file(path: &Path) -> bool {
    !matches!(path.extension().and_then(|e| e.to_str()), Some("pub" | "ppk")) && with_suffix(path, ".pub").is_file()
}

/// Private keys saved by the SSH key step (a key file with its .pub next to it), newest first.
pub fn saved_private_keys(host: &str) -> Vec<PathBuf> {
    let dir = directory().join("keys").join(safe_name(host));
    if !dir.is_dir() {
        return Vec::new();
    }
    files_newest_first(&dir, false).into_iter().filter(|p| is_private_key_file(p)).collect()
}

/// Every private key in configuration/keys (all servers), newest first.
pub fn all_private_keys() -> Vec<PathBuf> {
    let dir = directory().join("keys");
    if !dir.is_dir() {
        return Vec::new();
    }
    files_newest_first(&dir, true).into_iter().filter(|p| is_private_key_file(p)).collect()
}

/// Stores an imported private key and its .pub under keys/<folder> without overwriting existing files.
pub fn save_imported_key(folder: &str, private_key_text: &str, public_key: &AuthorizedKey) -> std::io::Result<PathBuf> {
    let name = format!(
        "imported_{}",
        public_key.key_type.split('@').next().unwrap_or("").replace("ssh-", "").replace("ecdsa-sha2-", "ecdsa-")
    );
    let path = free_path(&keys_directory(folder), &name, ".pub");
    let text = private_key_text.replace("\r\n", "\n").replace('\r', "\n");
    write_private(&path, &(text.trim_end().to_string() + "\n"))?;
    std::fs::write(with_suffix(&path, ".pub"), public_key.line() + "\n")?;
    Ok(path)
}

pub fn database_credentials_path(host: &str) -> PathBuf {
    create_restricted(&directory().join("database").join(safe_name(host))).join("mariadb.txt")
}

pub fn load_database_credentials(host: &str) -> Option<DatabaseCredentials> {
    let path = database_credentials_path(host);
    std::fs::read_to_string(path).ok().and_then(|text| DatabaseCredentials::parse(&text))
}

/// A changed file is backed up as mariadb.txt.bak-<time>.
pub fn save_database_credentials(credentials: &DatabaseCredentials) -> std::io::Result<PathBuf> {
    let path = database_credentials_path(&credentials.host);
    let text = credentials.to_file_text();
    if let Ok(old) = std::fs::read_to_string(&path) {
        if old != text {
            std::fs::copy(&path, with_suffix(&path, &format!(".bak-{}", timestamp())))?;
        }
    }
    write_private(&path, &text)?;
    Ok(path)
}

/// Another secret file next to mariadb.txt (gitea.txt), owner-only.
pub fn database_file(host: &str, name: &str) -> PathBuf {
    database_credentials_path(host).with_file_name(name)
}

/// Restricts the empty file before writing so the secret is never readable by others.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, "")?;
    restrict_to_owner(path, false);
    std::fs::write(path, text)
}

pub fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_os_string();
    text.push(suffix);
    PathBuf::from(text)
}

/// Appends a timestamp when the name is taken so no key is ever overwritten.
fn free_path(directory: &Path, name: &str, companion_suffix: &str) -> PathBuf {
    let taken = |p: &Path| {
        p.exists()
            || with_suffix(p, ".pub").exists()
            || with_suffix(p, ".ppk").exists()
            || (!companion_suffix.is_empty() && with_suffix(p, companion_suffix).exists())
    };
    let path = directory.join(name);
    if !taken(&path) {
        return path;
    }
    let stamp = timestamp();
    let mut stamped = directory.join(format!("{name}_{stamp}"));
    let mut i = 2;
    while taken(&stamped) {
        stamped = directory.join(format!("{name}_{stamp}_{i}"));
        i += 1;
    }
    stamped
}

pub fn safe_name(host: &str) -> String {
    host.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

fn resolve() -> PathBuf {
    if let Some(beside) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.join("configuration"))) {
        if is_writable(&beside) {
            return beside;
        }
    }
    let base = local_data_dir().unwrap_or_else(std::env::temp_dir);
    // The folder of the name before the rename (All-in-1 Installer) is used while it is there: nothing is lost.
    let old = base.join("All-in-1 Installer").join("configuration");
    let fallback = if old.is_dir() { old } else { base.join("All-in-1 Manager").join("configuration") };
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// `%LOCALAPPDATA%` on Windows, `$XDG_DATA_HOME` or `~/.local/share` elsewhere (.NET's LocalApplicationData).
fn local_data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
    }
}

fn is_writable(directory: &Path) -> bool {
    if std::fs::create_dir_all(directory).is_err() {
        return false;
    }
    let probe = directory.join(".write-test");
    if std::fs::write(&probe, "").is_err() {
        return false;
    }
    let _ = std::fs::remove_file(&probe);
    restrict_to_owner(directory, true);
    true
}

/// Owner-only permissions (0700 / 0600) on Linux and macOS; Windows relies on the inherited folder permissions.
pub fn restrict_to_owner(path: &Path, directory: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names_and_free_paths() {
        assert_eq!(safe_name("[::1]:2222"), "___1__2222");
        assert_eq!(safe_name("hotel.example.com"), "hotel.example.com");
        let dir = tempfile::tempdir().unwrap();
        let first = free_path(dir.path(), "id_ed25519", ".pub");
        assert_eq!(first, dir.path().join("id_ed25519"));
        std::fs::write(dir.path().join("id_ed25519.pub"), "x").unwrap();
        let second = free_path(dir.path(), "id_ed25519", ".pub");
        assert!(second.file_name().unwrap().to_string_lossy().starts_with("id_ed25519_"));
    }
}
