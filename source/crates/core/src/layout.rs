//! Where the hotel lives on the server and the account the emulator runs as (Settings → Server layout). Read once
//! at startup, like the look, so every step of a run uses the same places. The values go into shell scripts that run
//! as root, so [`ServerLayout::validate`] only lets plain paths and names through.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Whether AtomCMS is on the server: found by looking, or set by hand in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AtomCmsPresence {
    #[default]
    Detect,
    Installed,
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ServerLayout {
    pub emulator_repository: String,
    pub emulator_account: String,
    pub emulator_logs: String,
    pub octane: String,
    pub renderer: String,
    pub gamedata: String,
    pub atom_cms: String,
    pub atom_cms_installed: AtomCmsPresence,
    pub backups: String,
    pub database: String,
    /// The EpicNext CMS (its build) and the folder its source is cloned to.
    pub epicnext: String,
    pub epicnext_source: String,
    /// Gitea's docker compose folder.
    pub gitea: String,
    /// Traefik's folder (compose, `dynamic/` routes, certificates) and its access log folder.
    pub traefik: String,
    pub traefik_logs: String,
    /// CrowdSec's docker compose folder.
    pub crowdsec: String,
    /// The hotel's NGINX site file and its log folder.
    pub nginx_site: String,
    pub nginx_logs: String,
}

impl Default for ServerLayout {
    fn default() -> Self {
        Self {
            emulator_repository: "/var/www/emulator".into(),
            emulator_account: "emulator".into(),
            emulator_logs: "/var/log/emu".into(),
            octane: "/var/www/Octane".into(),
            renderer: "/var/www/Octane-Renderer".into(),
            gamedata: "/var/www/gamedata".into(),
            atom_cms: "/var/www/atomcms".into(),
            atom_cms_installed: AtomCmsPresence::Detect,
            backups: "/var/backups/all-in-1".into(),
            database: "habbo".into(),
            epicnext: "/var/www/epicnext".into(),
            epicnext_source: "/var/www/epicnext-src".into(),
            gitea: "/opt/gitea".into(),
            traefik: "/opt/traefik".into(),
            traefik_logs: "/var/log/traefik".into(),
            crowdsec: "/opt/crowdsec".into(),
            nginx_site: "/etc/nginx/sites-available/hotel".into(),
            nginx_logs: "/var/log/nginx/hotel".into(),
        }
    }
}

pub const ROOT_ACCOUNT: &str = "root";

static CURRENT: OnceLock<ServerLayout> = OnceLock::new();

/// The hotel database chosen during this run (a tool found the configured one missing and the user picked
/// another): wins over the emulator's config.ini and the layout until the installer closes.
static CHOSEN_DATABASE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

fn path_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^/[A-Za-z0-9._/-]+$").unwrap())
}

fn account_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^[a-z_][a-z0-9_-]{0,31}$").unwrap())
}

fn database_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^[A-Za-z0-9_]{1,64}$").unwrap())
}

impl ServerLayout {
    /// The layout this run of the installer uses: set once at startup from the settings (the defaults otherwise).
    pub fn current() -> &'static ServerLayout {
        CURRENT.get_or_init(|| crate::settings::InstallerSettings::load().layout.unwrap_or_default())
    }

    /// Fixes the layout for this run (startup, tests). Ignored when it was already read.
    pub fn init(layout: ServerLayout) {
        let _ = CURRENT.set(layout);
    }

    /// Uses `name` as the hotel database for the rest of this run. False (and nothing changes) for a name that is
    /// not a plain database name.
    pub fn choose_database(name: &str) -> bool {
        let name = name.trim();
        if !database_pattern().is_match(name) {
            return false;
        }
        *CHOSEN_DATABASE.write().unwrap() = Some(name.to_string());
        true
    }

    /// The database chosen during this run, if one was.
    pub fn chosen_database() -> Option<String> {
        CHOSEN_DATABASE.read().unwrap().clone()
    }

    /// A Linux user name: lower case letters, digits, '_' and '-', starting with a letter or '_'.
    pub fn is_valid_account(name: &str) -> bool {
        account_pattern().is_match(name)
    }

    /// Home folder of the emulator's account (Maven's ~/.m2 lives there).
    pub fn emulator_home(&self) -> String {
        if self.emulator_account == ROOT_ACCOUNT {
            "/root".into()
        } else {
            format!("/var/lib/{}", self.emulator_account)
        }
    }

    /// The paths with their names, for checks and messages.
    pub fn paths(&self) -> Vec<(&'static str, &str)> {
        vec![
            ("Emulator", &self.emulator_repository),
            ("Emulator logs", &self.emulator_logs),
            ("Octane", &self.octane),
            ("Octane-Renderer", &self.renderer),
            ("Gamedata", &self.gamedata),
            ("AtomCMS", &self.atom_cms),
            ("Database backups", &self.backups),
            ("EpicNext CMS", &self.epicnext),
            ("EpicNext source", &self.epicnext_source),
            ("Gitea", &self.gitea),
            ("Traefik", &self.traefik),
            ("Traefik logs", &self.traefik_logs),
            ("CrowdSec", &self.crowdsec),
            ("NGINX hotel site", &self.nginx_site),
            ("NGINX hotel logs", &self.nginx_logs),
        ]
    }

    /// None when every value is usable, else what is wrong.
    pub fn validate(&self) -> Option<String> {
        for (name, value) in self.paths() {
            if !path_pattern().is_match(value) || value.split('/').any(|part| part == "." || part == "..") {
                return Some(format!(
                    "{name}: \"{value}\" is not a usable path. Use an absolute path such as /var/www/hotel, with letters, digits, '.', '_', '-' and '/'."
                ));
            }
            let trimmed = value.trim_end_matches('/');
            if trimmed.chars().count() < 2 || matches!(trimmed, "/var" | "/var/www" | "/var/log" | "/usr" | "/etc" | "/etc/nginx" | "/opt" | "/home" | "/root" | "/tmp") {
                return Some(format!("{name}: \"{value}\" is a system folder; use a folder of its own inside it."));
            }
        }
        let all: Vec<&str> = self.paths().iter().map(|(_, v)| v.trim_end_matches('/')).collect();
        let mut distinct = all.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() != all.len() {
            return Some("Two of the paths are the same folder: every part needs a folder of its own.".into());
        }
        if !account_pattern().is_match(&self.emulator_account) {
            return Some(
                "Emulator account: use a Linux user name (lower case letters, digits, '_' and '-', starting with a letter or '_').".into(),
            );
        }
        if !database_pattern().is_match(&self.database) {
            return Some("Database: use letters, digits and '_' only.".into());
        }
        None
    }

    /// Removes trailing slashes and spaces around the values.
    pub fn normalize(&mut self) {
        fn clean(path: &str) -> String {
            let p = path.trim();
            if p.chars().count() > 1 {
                p.trim_end_matches('/').to_string()
            } else {
                p.to_string()
            }
        }
        self.emulator_repository = clean(&self.emulator_repository);
        self.emulator_logs = clean(&self.emulator_logs);
        self.octane = clean(&self.octane);
        self.renderer = clean(&self.renderer);
        self.gamedata = clean(&self.gamedata);
        self.atom_cms = clean(&self.atom_cms);
        self.backups = clean(&self.backups);
        for path in [
            &mut self.epicnext,
            &mut self.epicnext_source,
            &mut self.gitea,
            &mut self.traefik,
            &mut self.traefik_logs,
            &mut self.crowdsec,
            &mut self.nginx_site,
            &mut self.nginx_logs,
        ] {
            *path = clean(path);
        }
        self.emulator_account = self.emulator_account.trim().to_string();
        self.database = self.database.trim().to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        assert_eq!(ServerLayout::default().validate(), None);
        assert_eq!(ServerLayout::default().emulator_home(), "/var/lib/emulator");
    }

    #[test]
    fn the_service_folders_are_checked_too() {
        let layout = ServerLayout { gitea: "/opt/traefik".into(), ..Default::default() };
        assert!(layout.validate().unwrap().contains("same folder"));
        let layout = ServerLayout { traefik_logs: "/var/log".into(), ..Default::default() };
        assert!(layout.validate().unwrap().contains("system folder"));
        let mut layout = ServerLayout { nginx_site: " /etc/nginx/conf.d/hotel.conf/ ".into(), ..Default::default() };
        layout.normalize();
        assert_eq!(layout.nginx_site, "/etc/nginx/conf.d/hotel.conf");
        assert_eq!(layout.validate(), None);
        // Settings saved before these existed get the defaults.
        let old: ServerLayout = serde_json::from_str(r#"{"Gamedata": "/srv/gamedata"}"#).unwrap();
        assert_eq!((old.gamedata.as_str(), old.traefik.as_str()), ("/srv/gamedata", "/opt/traefik"));
    }

    #[test]
    fn refuses_unsafe_values() {
        let mut layout = ServerLayout { gamedata: "/var/www".into(), ..Default::default() };
        assert!(layout.validate().unwrap().contains("system folder"));
        layout.gamedata = "/var/www/a b".into();
        assert!(layout.validate().unwrap().contains("not a usable path"));
        layout.gamedata = "/var/www/../etc".into();
        assert!(layout.validate().unwrap().contains("not a usable path"));
        layout.gamedata = "/var/www/Octane/".into();
        assert!(layout.validate().unwrap().contains("same folder"));
        layout = ServerLayout { emulator_account: "Emu".into(), ..Default::default() };
        assert!(layout.validate().unwrap().starts_with("Emulator account"));
        layout = ServerLayout { database: "hab-bo".into(), ..Default::default() };
        assert!(layout.validate().unwrap().starts_with("Database"));
    }

    #[test]
    fn normalize_trims() {
        let mut layout = ServerLayout { octane: " /var/www/oct/ ".into(), database: " db ".into(), ..Default::default() };
        layout.normalize();
        assert_eq!(layout.octane, "/var/www/oct");
        assert_eq!(layout.database, "db");
    }

    #[test]
    fn json_matches_the_csharp_names() {
        let json = serde_json::to_string(&ServerLayout::default()).unwrap();
        assert!(json.contains("\"EmulatorRepository\":\"/var/www/emulator\""));
        assert!(json.contains("\"AtomCmsInstalled\":\"Detect\""));
        let parsed: ServerLayout = serde_json::from_str(r#"{"Gamedata":"/srv/gd","AtomCmsInstalled":"NotInstalled"}"#).unwrap();
        assert_eq!(parsed.gamedata, "/srv/gd");
        assert_eq!(parsed.octane, "/var/www/Octane");
        assert_eq!(parsed.atom_cms_installed, AtomCmsPresence::NotInstalled);
    }
}
