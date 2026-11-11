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
    /// Where the files and folders are inside gamedata, for hotels with their own structure.
    pub gamedata_layout: GamedataLayout,
}

/// Where each gamedata file and folder is: a path inside the gamedata folder (`config/FurnitureData.json`,
/// `furniture`), or an absolute path for one kept elsewhere. Settings saved before this existed get the standard
/// layout, which is what Octane's renderer-config uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct GamedataLayout {
    pub furnidata: String,
    pub productdata: String,
    pub figuredata: String,
    pub figuremap: String,
    pub effectmap: String,
    pub external_texts: String,
    pub ui_texts: String,
    pub avatar_actions: String,
    pub furniture: String,
    pub icons: String,
    pub clothes: String,
    pub pets: String,
    pub effects: String,
    pub sounds: String,
    pub images: String,
    pub badges: String,
}

impl Default for GamedataLayout {
    fn default() -> Self {
        Self {
            furnidata: "config/FurnitureData.json".into(),
            productdata: "config/ProductData.json".into(),
            figuredata: "config/FigureData.json".into(),
            figuremap: "config/FigureMap.json".into(),
            effectmap: "config/EffectMap.json".into(),
            external_texts: "config/ExternalTexts.json".into(),
            ui_texts: "config/UITexts.jsonc".into(),
            avatar_actions: "config/HabboAvatarActions.json".into(),
            furniture: "furniture".into(),
            icons: "icons".into(),
            clothes: "clothes".into(),
            pets: "pets".into(),
            effects: "effects".into(),
            sounds: "sounds".into(),
            images: "c_images".into(),
            badges: "c_images/album1584".into(),
        }
    }
}

/// One entry of the gamedata layout: its key, its name in Settings, whether it is a file, and its standard value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GamedataEntry {
    pub key: &'static str,
    pub label: &'static str,
    pub file: bool,
}

pub const GAMEDATA_ENTRIES: [GamedataEntry; 16] = [
    GamedataEntry { key: "furnidata", label: "Furniture data (FurnitureData.json)", file: true },
    GamedataEntry { key: "productdata", label: "Product data (ProductData.json)", file: true },
    GamedataEntry { key: "figuredata", label: "Figure data (FigureData.json)", file: true },
    GamedataEntry { key: "figuremap", label: "Figure map (FigureMap.json)", file: true },
    GamedataEntry { key: "effectmap", label: "Effect map (EffectMap.json)", file: true },
    GamedataEntry { key: "external_texts", label: "External texts (ExternalTexts.json)", file: true },
    GamedataEntry { key: "ui_texts", label: "UI texts (UITexts.jsonc)", file: true },
    GamedataEntry { key: "avatar_actions", label: "Avatar actions (HabboAvatarActions.json)", file: true },
    GamedataEntry { key: "furniture", label: "Furniture (.hab / .nitro)", file: false },
    GamedataEntry { key: "icons", label: "Furniture icons", file: false },
    GamedataEntry { key: "clothes", label: "Clothes", file: false },
    GamedataEntry { key: "pets", label: "Pets", file: false },
    GamedataEntry { key: "effects", label: "Effects", file: false },
    GamedataEntry { key: "sounds", label: "Sounds", file: false },
    GamedataEntry { key: "images", label: "Images (c_images)", file: false },
    GamedataEntry { key: "badges", label: "Badges", file: false },
];

fn relative_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$").unwrap())
}

impl GamedataLayout {
    pub fn get(&self, key: &str) -> &str {
        match key {
            "furnidata" => &self.furnidata,
            "productdata" => &self.productdata,
            "figuredata" => &self.figuredata,
            "figuremap" => &self.figuremap,
            "effectmap" => &self.effectmap,
            "external_texts" => &self.external_texts,
            "ui_texts" => &self.ui_texts,
            "avatar_actions" => &self.avatar_actions,
            "furniture" => &self.furniture,
            "icons" => &self.icons,
            "clothes" => &self.clothes,
            "pets" => &self.pets,
            "effects" => &self.effects,
            "sounds" => &self.sounds,
            "images" => &self.images,
            "badges" => &self.badges,
            _ => "",
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "furnidata" => &mut self.furnidata,
            "productdata" => &mut self.productdata,
            "figuredata" => &mut self.figuredata,
            "figuremap" => &mut self.figuremap,
            "effectmap" => &mut self.effectmap,
            "external_texts" => &mut self.external_texts,
            "ui_texts" => &mut self.ui_texts,
            "avatar_actions" => &mut self.avatar_actions,
            "furniture" => &mut self.furniture,
            "icons" => &mut self.icons,
            "clothes" => &mut self.clothes,
            "pets" => &mut self.pets,
            "effects" => &mut self.effects,
            "sounds" => &mut self.sounds,
            "images" => &mut self.images,
            "badges" => &mut self.badges,
            _ => return None,
        })
    }

    /// The standard value of an entry.
    pub fn standard(key: &str) -> String {
        GamedataLayout::default().get(key).to_string()
    }

    /// The full server path of a value: itself when absolute, else inside `gamedata`.
    pub fn resolve(gamedata: &str, value: &str) -> String {
        let value = value.trim();
        if value.starts_with('/') {
            value.trim_end_matches('/').to_string()
        } else {
            format!("{}/{}", gamedata.trim_end_matches('/'), value.trim_matches('/'))
        }
    }

    /// Why a value cannot be used, or None: a plain path inside gamedata, or an absolute one, never with `..`.
    fn problem(entry: &GamedataEntry, value: &str) -> Option<String> {
        let value = value.trim();
        let plain = if value.starts_with('/') { path_pattern().is_match(value) } else { relative_pattern().is_match(value) };
        if !plain || value.split('/').any(|part| part == "." || part == "..") {
            return Some(format!(
                "{}: \"{value}\" is not a usable path. Use a path inside gamedata such as {} or an absolute one, with letters, digits, '.', '_', '-' and '/'.",
                entry.label,
                GamedataLayout::standard(entry.key)
            ));
        }
        if entry.file && !(value.ends_with(".json") || value.ends_with(".jsonc")) {
            return Some(format!("{}: \"{value}\" should be a .json file.", entry.label));
        }
        None
    }

    pub fn validate(&self) -> Option<String> {
        GAMEDATA_ENTRIES.iter().find_map(|entry| GamedataLayout::problem(entry, self.get(entry.key)))
    }

    /// Trims spaces and slashes at the end (and at the start of a relative value).
    pub fn normalize(&mut self) {
        for entry in GAMEDATA_ENTRIES {
            if let Some(value) = self.get_mut(entry.key) {
                let trimmed = value.trim();
                *value = if trimmed.starts_with('/') { trimmed.trim_end_matches('/').to_string() } else { trimmed.trim_matches('/').to_string() };
                if value.is_empty() {
                    *value = GamedataLayout::standard(entry.key);
                }
            }
        }
    }

    /// Whether every entry is the standard one.
    pub fn is_standard(&self) -> bool {
        *self == GamedataLayout::default()
    }
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
            gamedata_layout: GamedataLayout::default(),
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

    /// The server path of a gamedata entry (see [`GamedataLayout`]): `furnidata`, `furniture`, ...
    pub fn gamedata_path(&self, key: &str) -> String {
        GamedataLayout::resolve(&self.gamedata, self.gamedata_layout.get(key))
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
        self.gamedata_layout.validate()
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
        self.gamedata_layout.normalize();
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
    fn the_gamedata_layout_has_the_standard_places_and_takes_others() {
        let layout = ServerLayout::default();
        assert_eq!(layout.gamedata_path("furnidata"), "/var/www/gamedata/config/FurnitureData.json");
        assert_eq!(layout.gamedata_path("badges"), "/var/www/gamedata/c_images/album1584");
        let mut layout = ServerLayout {
            gamedata_layout: GamedataLayout { furnidata: " /srv/data/furnidata.json ".into(), pets: "/bundled/pet/".into(), furniture: "bundled/furniture".into(), icons: "".into(), ..Default::default() },
            ..Default::default()
        };
        layout.normalize();
        assert_eq!(layout.validate(), None);
        assert_eq!(layout.gamedata_path("furnidata"), "/srv/data/furnidata.json");
        assert_eq!(layout.gamedata_path("pets"), "/bundled/pet");
        assert_eq!(layout.gamedata_path("furniture"), "/var/www/gamedata/bundled/furniture");
        assert_eq!(layout.gamedata_layout.icons, "icons", "an emptied entry goes back to the standard");
        for (key, bad) in [("furniture", "../etc"), ("furnidata", "config/furni.txt"), ("pets", "a b"), ("clothes", "/x/../y")] {
            let mut layout = ServerLayout::default();
            *layout.gamedata_layout.get_mut(key).unwrap() = bad.into();
            assert!(layout.validate().is_some(), "{key} = {bad}");
        }
        // Settings saved before the gamedata layout existed get the standard one.
        let old: ServerLayout = serde_json::from_str(r#"{"Gamedata": "/srv/gamedata"}"#).unwrap();
        assert!(old.gamedata_layout.is_standard());
        for entry in GAMEDATA_ENTRIES {
            assert!(!ServerLayout::default().gamedata_layout.get(entry.key).is_empty(), "{}", entry.key);
        }
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
