//! What the login remembers on this computer: the last server, the last user of each server and, only when asked for a
//! password login, its password. Kept in `configuration/logins.json`, readable by this user only; a password is
//! encrypted (AES-256-GCM) with a random key in `configuration/logins.key`, so the logins file alone (copied, or shown
//! on screen) gives nothing away. SSH key logins never store a password, nor the key's password.

use std::collections::BTreeMap;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::config_dir;

const NONCE_SIZE: usize = 12;

/// One server's remembered login.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLogin {
    pub user: String,
    /// The password, encrypted (base64 of nonce + sealed text); empty when it is not remembered.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
}

/// Every remembered login, by server ("host" or "host:port" as typed, lower case).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLogins {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_server: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, SavedLogin>,
}

fn path() -> PathBuf {
    config_dir::directory().join("logins.json")
}

fn key_path() -> PathBuf {
    config_dir::directory().join("logins.key")
}

fn server_key(server: &str) -> String {
    server.trim().to_lowercase()
}

/// The key that encrypts the passwords, made once.
fn key(create: bool) -> Option<[u8; 32]> {
    let path = key_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Some(key);
        }
    }
    if !create {
        return None;
    }
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    std::fs::write(&path, b"").ok()?;
    config_dir::restrict_to_owner(&path, false);
    std::fs::write(&path, key).ok()?;
    Some(key)
}

/// The password sealed with `key`, as base64.
pub fn seal(key: &[u8; 32], password: &str) -> String {
    let mut nonce = [0u8; NONCE_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let cipher = aes_gcm::Aes256Gcm::new(key.into());
    let sealed = cipher.encrypt(&nonce.into(), password.as_bytes()).unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode([nonce.as_slice(), &sealed].concat())
}

/// The password of [`seal`], or None when the key or the text does not fit.
pub fn open(key: &[u8; 32], sealed: &str) -> Option<String> {
    let data = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).ok()?;
    if data.len() <= NONCE_SIZE {
        return None;
    }
    let (nonce, body) = data.split_at(NONCE_SIZE);
    let cipher = aes_gcm::Aes256Gcm::new(key.into());
    let plain = cipher.decrypt(nonce.into(), body).ok()?;
    String::from_utf8(plain).ok()
}

impl SavedLogins {
    pub fn load() -> Self {
        std::fs::read_to_string(path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = config_dir::write_private(&path(), &text);
        }
    }

    /// The remembered user of a server.
    pub fn user(&self, server: &str) -> Option<String> {
        self.servers.get(&server_key(server)).map(|l| l.user.clone()).filter(|u| !u.is_empty())
    }

    /// The remembered password of a server, when it is for `user`.
    pub fn password(&self, server: &str, user: &str) -> Option<String> {
        let saved = self.servers.get(&server_key(server))?;
        if saved.password.is_empty() || saved.user.trim() != user.trim() {
            return None;
        }
        open(&key(false)?, &saved.password)
    }

    /// After a login: the server becomes the last one and its user is remembered; its password too when `password`
    /// is Some (a password login with Remember ticked), otherwise a remembered password is forgotten.
    pub fn remember(server: &str, user: &str, password: Option<&str>) {
        let mut logins = Self::load();
        let entry = logins.servers.entry(server_key(server)).or_default();
        entry.user = user.trim().to_string();
        entry.password = match password.filter(|p| !p.is_empty()).zip(key(true)) {
            Some((password, key)) => seal(&key, password),
            None => String::new(),
        };
        logins.last_server = Some(server.trim().to_string());
        logins.save();
    }

    /// After an SSH key login: the server becomes the last one and its user is remembered. A remembered password
    /// stays only while it is for that same user.
    pub fn remember_user(server: &str, user: &str) {
        let mut logins = Self::load();
        let entry = logins.servers.entry(server_key(server)).or_default();
        if entry.user.trim() != user.trim() {
            entry.password.clear();
        }
        entry.user = user.trim().to_string();
        logins.last_server = Some(server.trim().to_string());
        logins.save();
    }

    /// Forgets the password of a server (the user stays).
    pub fn forget_password(server: &str) {
        let mut logins = Self::load();
        if let Some(entry) = logins.servers.get_mut(&server_key(server)) {
            if !entry.password.is_empty() {
                entry.password.clear();
                logins.save();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_is_sealed_and_opened_with_the_same_key_only() {
        let key = [7u8; 32];
        let sealed = seal(&key, "geheim wachtwoord");
        assert!(!sealed.contains("geheim"));
        assert_ne!(seal(&key, "geheim wachtwoord"), sealed, "a new nonce each time");
        assert_eq!(open(&key, &sealed).as_deref(), Some("geheim wachtwoord"));
        assert_eq!(open(&[8u8; 32], &sealed), None);
        assert_eq!(open(&key, "not base64 !"), None);
    }

    #[test]
    fn logins_are_found_by_server_whatever_its_case() {
        let mut logins = SavedLogins::default();
        logins.servers.insert(server_key("Hotel.Example.com:2222"), SavedLogin { user: "admin".into(), password: String::new() });
        assert_eq!(logins.user("hotel.example.com:2222").as_deref(), Some("admin"));
        assert_eq!(logins.user("other"), None);
        assert_eq!(logins.password("hotel.example.com:2222", "admin"), None, "no password remembered");
        let text = serde_json::to_string(&logins).unwrap();
        assert!(!text.contains("password"), "an empty password is not written");
    }
}
