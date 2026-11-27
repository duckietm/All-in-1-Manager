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

    /// The complete file the installers need: host, root and CMS passwords.
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_root(text).filter(|c| !c.cms_password.is_empty())
    }

    /// Enough for the SQL tools: host and root password (the CMS password may be missing).
    pub fn parse_root(text: &str) -> Option<Self> {
        Some(Self::lenient(text, "")).filter(|c| !c.host.is_empty() && !c.root_password.is_empty())
    }

    /// The file after an SQL login worked: that account and port, every other value of the old file kept.
    pub fn with_root_login(old_text: Option<&str>, host: &str, user: &str, password: &str, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            root_user: user.into(),
            root_password: password.into(),
            ..Self::lenient(old_text.unwrap_or(""), host)
        }
    }

    /// Every value the text has, defaults for the rest (empty passwords).
    fn lenient(text: &str, host: &str) -> Self {
        let mut values = std::collections::HashMap::new();
        for line in text.split('\n').map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some((key, value)) = line.split_once('=') {
                values.insert(key.trim().to_string(), without_comment(value).to_string());
            }
        }
        let get = |key: &str| values.get(key).filter(|v| !v.is_empty()).cloned();
        Self {
            host: get("host").unwrap_or_else(|| host.into()),
            port: get("port").and_then(|p| p.parse().ok()).unwrap_or(3306),
            database: get("database").unwrap_or_else(|| ServerLayout::current().database.clone()),
            root_user: get("root_user").unwrap_or_else(|| "root".into()),
            root_password: get("root_password").unwrap_or_default(),
            cms_user: get("cms_user").unwrap_or_else(|| "cms".into()),
            cms_password: get("cms_password").unwrap_or_default(),
            emulator_user: get("emulator_user").unwrap_or_else(|| "emulator".into()),
            emulator_password: get("emulator_password").unwrap_or_default(),
        }
    }
}

/// A value without its comment: `#` at the start or after a space starts one; `pa#ss` keeps its `#`.
fn without_comment(value: &str) -> &str {
    let value = value.trim();
    let start = value
        .char_indices()
        .find(|&(i, c)| c == '#' && (i == 0 || value[..i].ends_with(char::is_whitespace)))
        .map_or(value.len(), |(i, _)| i);
    value[..start].trim_end()
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
    fn comments_after_values() {
        // A real file: the comment is not the password, so the file is not saved and the SQL tools ask for an account.
        let user_file = "host=h\nroot_user=root\nroot_password=            # see note below\ncms_password=c\n";
        assert_eq!(DatabaseCredentials::parse(user_file), None);
        assert_eq!(DatabaseCredentials::parse_root(user_file), None);
        let c = DatabaseCredentials::parse("host=h # the server\nroot_password=abc # note\ncms_password=pa#ss\nport=3307\t# tab").unwrap();
        assert_eq!((c.host.as_str(), c.root_password.as_str(), c.cms_password.as_str(), c.port), ("h", "abc", "pa#ss", 3307));
        assert_eq!(without_comment("#x"), "");
        assert_eq!(without_comment(" a#b #c "), "a#b");
    }

    #[test]
    fn root_only_file_is_enough_for_sql_tools() {
        let text = "host=h\nroot_user=admin\nroot_password=R\n";
        assert_eq!(DatabaseCredentials::parse(text), None);
        let c = DatabaseCredentials::parse_root(text).unwrap();
        assert_eq!((c.root_user.as_str(), c.root_password.as_str(), c.port, c.cms_password.as_str()), ("admin", "R", 3306, ""));
        assert_eq!(DatabaseCredentials::parse_root("root_password=R"), None);
    }

    #[test]
    fn remembered_login_keeps_the_other_values() {
        let mut saved = DatabaseCredentials::new("h", 3306, "habbo2", "root", "old", "cms", "C");
        saved.emulator_password = "E".into();
        let text = saved.to_file_text();
        let new = DatabaseCredentials::with_root_login(Some(&text), "h", "admin", "new", 3307);
        assert_eq!(new, DatabaseCredentials { root_user: "admin".into(), root_password: "new".into(), port: 3307, ..saved.clone() });

        // Not complete (no CMS password, a comment as password): the other values are still kept.
        let partial = "host=h\ndatabase=habbo2\nroot_password=   # see note below\nemulator_password=E\n";
        let new = DatabaseCredentials::with_root_login(Some(partial), "h", "root", "P", 3306);
        assert_eq!((new.database.as_str(), new.emulator_password.as_str(), new.cms_password.as_str(), new.root_password.as_str()), ("habbo2", "E", "", "P"));
        assert_eq!(DatabaseCredentials::parse_root(&new.to_file_text()), Some(new.clone()));
        assert_eq!(DatabaseCredentials::parse(&new.to_file_text()), None);

        let fresh = DatabaseCredentials::with_root_login(None, "h", "root", "P", 3306);
        assert_eq!((fresh.host.as_str(), fresh.root_user.as_str(), fresh.root_password.as_str()), ("h", "root", "P"));
    }

    #[test]
    fn passwords() {
        let p = new_password();
        assert_eq!(p.len(), 32);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(p, new_password());
    }
}
