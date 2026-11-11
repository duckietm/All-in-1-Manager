//! MariaDB accounts for one server, in `configuration/database/<server>/mariadb.txt`. The emulator account is filled
//! in by the emulator step.

use rand::Rng;

use crate::layout::ServerLayout;

// Alphanumeric so passwords are safe in any config file or connection string.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
pub const PASSWORD_LENGTH: usize = 32; // ~190 bits

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseCredentials {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub root_user: String,
    pub root_password: String,
    pub cms_user: String,
    pub cms_password: String,
    pub emulator_user: String,
    pub emulator_password: String,
}

/// A random password of letters and digits (from the operating system's random source).
pub fn new_password() -> String {
    let mut rng = rand::rngs::OsRng;
    (0..PASSWORD_LENGTH).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

impl DatabaseCredentials {
    pub fn new(host: &str, port: u16, database: &str, root_user: &str, root_password: &str, cms_user: &str, cms_password: &str) -> Self {
        Self {
            host: host.into(),
            port,
            database: database.into(),
            root_user: root_user.into(),
            root_password: root_password.into(),
            cms_user: cms_user.into(),
            cms_password: cms_password.into(),
            emulator_user: "emulator".into(),
            emulator_password: String::new(),
        }
    }

    pub fn generate(host: &str) -> Self {
        Self::new(host, 3306, &ServerLayout::current().database, "root", &new_password(), "cms", &new_password())
    }

    pub fn to_file_text(&self) -> String {
        format!(
            "# MariaDB accounts on {host}, created by All-in-1 Manager.\n\
             # Keep this file safe: root can change every database on the server.\n\
             host={host}\nport={port}\ndatabase={database}\nroot_user={root_user}\nroot_password={root_password}\n\
             cms_user={cms_user}\ncms_password={cms_password}\nemulator_user={emulator_user}\nemulator_password={emulator_password}\n",
            host = self.host,
            port = self.port,
            database = self.database,
            root_user = self.root_user,
            root_password = self.root_password,
            cms_user = self.cms_user,
            cms_password = self.cms_password,
            emulator_user = self.emulator_user,
            emulator_password = self.emulator_password,
        )
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut values = std::collections::HashMap::new();
        for line in text.split('\n').map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some((key, value)) = line.split_once('=') {
                values.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        let get = |key: &str| values.get(key).filter(|v| !v.is_empty()).cloned();
        let host = get("host")?;
        let root_password = get("root_password")?;
        let cms_password = get("cms_password")?;
        Some(Self {
            host,
            port: get("port").and_then(|p| p.parse().ok()).unwrap_or(3306),
            database: get("database").unwrap_or_else(|| ServerLayout::current().database.clone()),
            root_user: get("root_user").unwrap_or_else(|| "root".into()),
            root_password,
            cms_user: get("cms_user").unwrap_or_else(|| "cms".into()),
            cms_password,
            emulator_user: get("emulator_user").unwrap_or_else(|| "emulator".into()),
            emulator_password: get("emulator_password").unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_format() {
        let mut c = DatabaseCredentials::new("192.168.0.155", 3306, "habbo", "root", "R", "cms", "C");
        c.emulator_password = "E".into();
        let text = c.to_file_text();
        assert_eq!(
            text,
            "# MariaDB accounts on 192.168.0.155, created by All-in-1 Manager.\n# Keep this file safe: root can change every database on the server.\nhost=192.168.0.155\nport=3306\ndatabase=habbo\nroot_user=root\nroot_password=R\ncms_user=cms\ncms_password=C\nemulator_user=emulator\nemulator_password=E\n"
        );
        assert_eq!(DatabaseCredentials::parse(&text), Some(c));
        assert_eq!(DatabaseCredentials::parse("host=x\nroot_password=\ncms_password=c"), None);
    }

    #[test]
    fn passwords() {
        let p = new_password();
        assert_eq!(p.len(), 32);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(p, new_password());
    }
}
