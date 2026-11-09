//! Remembers each server's SSH host key fingerprint between sessions, the way OpenSSH's known_hosts does.
//!
//! Without this every login trusts whatever key the server offers, so a server that answers on the address is
//! trusted even when it is not the one logged in to before. The fingerprints live in
//! `configuration/known_hosts.json`, next to the other settings, so the settings backup includes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aio_core::config_dir;

const FILE: &str = "known_hosts.json";

fn file() -> PathBuf {
    config_dir::directory().join(FILE)
}

/// "host:port", the key a fingerprint is stored under.
fn address(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

/// An unreadable or damaged file must not stop anyone from logging in: it only means nothing is known yet.
fn read_from(file: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(file).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn write_to(file: &Path, hosts: &BTreeMap<String, String>) {
    if let Ok(text) = serde_json::to_string_pretty(hosts) {
        let _ = std::fs::write(file, text);
    }
}

fn trusted_in(file: &Path, host: &str, port: u16) -> Option<String> {
    read_from(file).remove(&address(host, port))
}

fn remember_in(file: &Path, host: &str, port: u16, fingerprint: &str) {
    let mut hosts = read_from(file);
    if hosts.get(&address(host, port)).is_some_and(|known| known == fingerprint) {
        return;
    }
    hosts.insert(address(host, port), fingerprint.to_string());
    write_to(file, &hosts);
}

fn forget_in(file: &Path, host: &str, port: u16) {
    let mut hosts = read_from(file);
    if hosts.remove(&address(host, port)).is_some() {
        write_to(file, &hosts);
    }
}

/// The fingerprint this server showed the last time, or None when it was never logged in to.
pub fn trusted(host: &str, port: u16) -> Option<String> {
    trusted_in(&file(), host, port)
}

/// Remembers `fingerprint` for this server. Writing failures are ignored: the login itself worked.
pub fn remember(host: &str, port: u16, fingerprint: &str) {
    remember_in(&file(), host, port, fingerprint);
}

/// Forgets this server's fingerprint, so the next login trusts the key it is shown (after a reinstall).
pub fn forget(host: &str, port: u16) {
    forget_in(&file(), host, port);
}

/// What to tell someone whose server shows a different host key than the one remembered.
pub fn changed_message(host: &str, port: u16, expected: &str, seen: &str) -> String {
    format!(
        "The SSH host key of {host}:{port} is not the one from the last login.\n  Known: {expected}\n  Now:   {seen}\nIf the server was reinstalled or its host keys were renewed, this is expected: remove it from configuration/{FILE} and log in again. Otherwise the connection is not going to the server logged in to before; nothing was sent to it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_forgets_and_separates_ports() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join(FILE);

        assert_eq!(trusted_in(&file, "10.0.0.1", 22), None);
        remember_in(&file, "10.0.0.1", 22, "ssh-ed25519 SHA256:aaa");
        remember_in(&file, "10.0.0.1", 2222, "ssh-ed25519 SHA256:bbb");
        assert_eq!(trusted_in(&file, "10.0.0.1", 22).as_deref(), Some("ssh-ed25519 SHA256:aaa"));
        assert_eq!(trusted_in(&file, "10.0.0.1", 2222).as_deref(), Some("ssh-ed25519 SHA256:bbb"));

        // The same fingerprint again leaves the file as it is.
        remember_in(&file, "10.0.0.1", 22, "ssh-ed25519 SHA256:aaa");
        assert_eq!(trusted_in(&file, "10.0.0.1", 22).as_deref(), Some("ssh-ed25519 SHA256:aaa"));

        // A renewed key takes the place of the old one.
        remember_in(&file, "10.0.0.1", 22, "ssh-ed25519 SHA256:ccc");
        assert_eq!(trusted_in(&file, "10.0.0.1", 22).as_deref(), Some("ssh-ed25519 SHA256:ccc"));

        forget_in(&file, "10.0.0.1", 22);
        assert_eq!(trusted_in(&file, "10.0.0.1", 22), None);
        assert_eq!(trusted_in(&file, "10.0.0.1", 2222).as_deref(), Some("ssh-ed25519 SHA256:bbb"));
    }

    #[test]
    fn a_damaged_file_means_nothing_is_known() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join(FILE);
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(trusted_in(&file, "10.0.0.1", 22), None);
        // And it can still be written again.
        remember_in(&file, "10.0.0.1", 22, "ssh-ed25519 SHA256:aaa");
        assert_eq!(trusted_in(&file, "10.0.0.1", 22).as_deref(), Some("ssh-ed25519 SHA256:aaa"));
    }

    #[test]
    fn the_message_names_both_fingerprints() {
        let message = changed_message("example.test", 22, "SHA256:old", "SHA256:new");
        assert!(message.contains("example.test:22"));
        assert!(message.contains("SHA256:old"));
        assert!(message.contains("SHA256:new"));
    }
}
