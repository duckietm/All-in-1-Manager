//! Defaults from the Settings page that the login fills in. Stored in configuration/settings.json (the same file and
//! names as the C# installer wrote) so the settings backup includes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config_dir;
use crate::layout::ServerLayout;

pub const HOTEL_LOOK: &str = "hotel";
pub const PROFESSIONAL_LOOK: &str = "professional";
pub const DEFAULT_BRANCH: &str = "main";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct InstallerSettings {
    /// "host" or "host:port", as typed in the login: the server the login fills in. One of `servers`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_server: Option<String>,
    /// The servers (IP addresses and DNS names, with :port when needed) the login offers in its list.
    pub servers: Vec<String>,
    /// Private key path relative to the configuration folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_key: Option<String>,
    /// The one Nitro retro of the first version (before profiles); read once into `retros`, never written again.
    #[serde(default, skip_serializing)]
    pub retro: Option<RetroUrls>,
    /// Habbo assets: also the .swf of furniture, clothes, effects and pets (into the asset tools). New in the Rust port.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub assets_with_swf: bool,
    /// The Nitro retro profiles: a name and its renderer-config.json URLs. New in the Rust port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retros: Vec<RetroProfile>,
    /// The name of the profile last worked with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retro_selected: Option<String>,
    /// The hotel's name, filled in for %hotel_name% in the default catalogue's texts. New in the Rust port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotel_name: Option<String>,
    /// Where Get Assets downloads to (its gamedata/ mirrors the server's); None means [`default_assets_folder`].
    /// New in the Rust port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets_path: Option<String>,
    /// The branch chosen per repository ("duckietm/Octane" to "dev"); a repository that is not in here uses main.
    pub branches: BTreeMap<String, String>,
    /// Paths on the server and the emulator's account; None means the defaults. Read once at startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<ServerLayout>,
    /// "hotel" (the default) or "professional". Read once at startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look: Option<String>,
    /// The window language (new in the Rust port; the C# installer ignores it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// "light" or "dark"; none follows the system. New in the Rust port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// The sections folded in the window (the sidebar's groups, the Tools page's sections). New in the Rust port.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collapsed: Vec<String>,
    /// The plugins switched off on the Plugins page (their ids); their pages leave the menus. New in the Rust port.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_plugins: Vec<String>,
    /// Developer mode (Settings): external plugins may be loaded straight from a folder. Off by default. New in the
    /// Rust port.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub developer_mode: bool,
    /// The plugin folders added in developer mode (full paths). Only used while `developer_mode` is on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub developer_plugins: Vec<String>,
}

/// A branch to choose from, with the line the lists show behind its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchChoice {
    pub branch: &'static str,
    pub caption: &'static str,
}

impl std::fmt::Display for BranchChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}   ({})", self.branch, self.caption)
    }
}

/// What the Settings page and the clone steps offer. One repository writes it as "dev" and another as "Dev"; the
/// clone step matches the name case-insensitively against the branches the remote has.
pub const BRANCH_CHOICES: [BranchChoice; 2] = [
    BranchChoice { branch: DEFAULT_BRANCH, caption: "stable, the version the hotel runs on" },
    BranchChoice { branch: "dev", caption: "the newest changes, not always stable" },
];

/// The entry for a branch name, or main.
pub fn choice_for(branch: &str) -> BranchChoice {
    BRANCH_CHOICES.iter().copied().find(|b| b.branch == branch).unwrap_or(BRANCH_CHOICES[0])
}

fn file() -> PathBuf {
    config_dir::directory().join("settings.json")
}

/// A Nitro retro's renderer-config.json values for Get Assets' custom downloads. A folder may also be given as the
/// full pattern (".../furniture/%libname%.nitro").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetroUrls {
    /// furnidata.url: the retro's FurnitureData.json.
    #[serde(default)]
    pub furnidata: String,
    /// furni.asset.url: the furniture folder.
    #[serde(default)]
    pub furniture: String,
    /// furni.asset.icon.url: the furniture icons folder.
    #[serde(default)]
    pub furniture_icons: String,
    /// avatar.asset.url: the clothes folder.
    #[serde(default)]
    pub clothes: String,
    /// avatar.figuredata.url: FigureData.json.
    #[serde(default)]
    pub figuredata: String,
    /// avatar.figuremap.url: FigureMap.json.
    #[serde(default)]
    pub figuremap: String,
    /// The bundle format the retro serves: "hab", "nitro" or "swf"; empty tries all three (Auto).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    /// The Referer sent with every request ("https://leet.city/"), for a CDN that only answers its own hotel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub referer: String,
    /// Extra request headers, one "Name: value" per line (a User-Agent here replaces the built-in one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub headers: String,
    /// Download through the system's curl instead of the built-in client (for a CDN behind Cloudflare's bot check).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub use_curl: bool,
    /// Downloads at the same time; 0 is the default (16).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub parallel: u8,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

impl RetroUrls {
    pub fn is_empty(&self) -> bool {
        [&self.furnidata, &self.furniture, &self.furniture_icons, &self.clothes, &self.figuredata, &self.figuremap].iter().all(|v| v.trim().is_empty())
    }

    /// The Referer and the extra headers as (name, value) pairs, in the order given; empty lines and lines starting
    /// with '#' are skipped. A line without "Name: value" is an error that names it.
    pub fn request_headers(&self) -> Result<Vec<(String, String)>, String> {
        let mut headers = Vec::new();
        if !self.referer.trim().is_empty() {
            headers.push(("Referer".to_string(), self.referer.trim().to_string()));
        }
        for line in self.headers.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match line.split_once(':') {
                Some((name, value)) if !name.trim().is_empty() && !name.trim().contains(' ') => {
                    headers.push((name.trim().to_string(), value.trim().to_string()));
                }
                _ => return Err(format!("Header \"{line}\" is not \"Name: value\".")),
            }
        }
        Ok(headers)
    }
}

/// A named Nitro retro: its downloads go to Habbo_Assets/custom/<folder name>.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetroProfile {
    pub name: String,
    #[serde(flatten)]
    pub urls: RetroUrls,
}

impl RetroProfile {
    /// The name as a folder name: characters Windows refuses become '_', no leading or trailing dots and spaces.
    pub fn folder_name(&self) -> String {
        let cleaned: String =
            self.name.trim().chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
        let cleaned = cleaned.trim_matches(|c: char| c == '.' || c == ' ').to_string();
        if cleaned.is_empty() { "retro".into() } else { cleaned }
    }
}

impl RetroUrls {
    /// The host of the FurnitureData.json URL ("dinohotel.nl"), a name for a profile made from it.
    pub fn host(&self) -> Option<String> {
        let url = [&self.furnidata, &self.furniture, &self.clothes, &self.figuremap].into_iter().find(|u| !u.trim().is_empty())?;
        let rest = url.trim().split("://").nth(1).unwrap_or(url.trim());
        let host = rest.split(['/', '?', ':']).next().unwrap_or("").trim_start_matches("www.");
        (!host.is_empty()).then(|| host.to_string())
    }
}

/// Get Assets' folder when Settings name none: Habbo_Assets next to the configuration folder.
pub fn default_assets_folder() -> PathBuf {
    let configuration = config_dir::directory();
    configuration.parent().map(|p| p.join("Habbo_Assets")).unwrap_or_else(|| configuration.join("Habbo_Assets"))
}

impl InstallerSettings {
    pub fn load() -> Self {
        Self::load_from(&file())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&file())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// The saved servers, the default first. Settings from before the list only have `default_server`, which then
    /// is the list. Duplicates (ignoring case) are dropped.
    pub fn server_list(&self) -> Vec<String> {
        let mut list: Vec<String> = Vec::new();
        if let Some(server) = self.default_server.as_deref().filter(|s| !s.is_empty()) {
            list.push(server.to_string());
        }
        for server in self.servers.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            list.push(server.to_string());
        }
        let mut seen: Vec<String> = Vec::new();
        list.retain(|s| {
            let lower = s.to_lowercase();
            if seen.contains(&lower) {
                false
            } else {
                seen.push(lower);
                true
            }
        });
        list
    }

    /// The retro profiles; the single retro of the first version becomes a profile named after its host.
    pub fn retro_profiles(&self) -> Vec<RetroProfile> {
        let mut profiles = self.retros.clone();
        if profiles.is_empty() {
            if let Some(urls) = self.retro.clone().filter(|u| !u.is_empty()) {
                profiles.push(RetroProfile { name: urls.host().unwrap_or_else(|| "retro".into()), urls });
            }
        }
        profiles
    }

    /// Get Assets' folder: the one chosen in Settings, or [`default_assets_folder`].
    pub fn assets_folder(&self) -> PathBuf {
        self.assets_path.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(default_assets_folder)
    }

    /// The default key as a full path, when it still exists.
    pub fn default_key_path(&self) -> Option<PathBuf> {
        let key = self.default_key.as_deref().filter(|k| !k.is_empty())?;
        let path = config_dir::directory().join(key);
        path.is_file().then_some(path)
    }

    /// The branch chosen for a repository written as "owner/name", or main.
    pub fn branch_for(&self, repository: &str) -> String {
        self.branches.get(repository).filter(|b| !b.is_empty()).cloned().unwrap_or_else(|| DEFAULT_BRANCH.to_string())
    }

    /// Remembers the branch; main is the default, so it is not written.
    pub fn set_branch(&mut self, repository: &str, branch: &str) {
        if branch == DEFAULT_BRANCH {
            self.branches.remove(repository);
        } else {
            self.branches.insert(repository.to_string(), branch.to_string());
        }
    }

    pub fn is_professional_look(&self) -> bool {
        self.look.as_deref() == Some(PROFESSIONAL_LOOK)
    }
}

/// A key path relative to the configuration folder, with forward slashes.
pub fn relative_key(full_path: &Path) -> String {
    let dir = config_dir::directory();
    full_path.strip_prefix(&dir).unwrap_or(full_path).to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {

    #[test]
    fn old_settings_without_field_load() {
        let old = r#"{"Servers":["a.example"],"DisabledPlugins":["cms"],"Branches":{}}"#;
        let settings: InstallerSettings = serde_json::from_str(old).unwrap();
        assert!(!settings.developer_mode);
        assert!(settings.developer_plugins.is_empty());
        assert_eq!(settings.disabled_plugins, ["cms"]);
        // Off and empty are not written; set, they round-trip in PascalCase.
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(!saved.contains("Developer"), "{saved}");
        let on = InstallerSettings { developer_mode: true, developer_plugins: vec!["C:/dev/hello".into()], ..settings };
        let saved = serde_json::to_string(&on).unwrap();
        assert!(saved.contains("\"DeveloperMode\":true") && saved.contains("\"DeveloperPlugins\""), "{saved}");
        assert_eq!(serde_json::from_str::<InstallerSettings>(&saved).unwrap(), on);
    }

    #[test]
    fn the_first_retro_becomes_a_profile() {
        let old = r#"{"Servers":[],"Branches":{},"Retro":{"furnidata":"https://www.dinohotel.nl/gamedata/config/FurnitureData.json"}}"#;
        let settings: InstallerSettings = serde_json::from_str(old).unwrap();
        let profiles = settings.retro_profiles();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "dinohotel.nl");
        let saved = serde_json::to_string(&InstallerSettings { retros: profiles, ..settings }).unwrap();
        assert!(!saved.contains("\"Retro\":") && saved.contains("\"Retros\":"), "{saved}");
        let profile = RetroProfile { name: " My: Hotel. ".into(), urls: RetroUrls::default() };
        assert_eq!(profile.folder_name(), "My_ Hotel");
        assert_eq!(RetroProfile::default().folder_name(), "retro");
    }

    #[test]
    fn referer_and_custom_headers() {
        let urls = RetroUrls {
            referer: " https://leet.city/ ".into(),
            headers: "User-Agent: Mozilla/5.0 (Windows NT 10.0)\n\n# a comment\nX-Token:abc:def\n".into(),
            ..RetroUrls::default()
        };
        assert_eq!(
            urls.request_headers().unwrap(),
            vec![
                ("Referer".into(), "https://leet.city/".into()),
                ("User-Agent".into(), "Mozilla/5.0 (Windows NT 10.0)".into()),
                ("X-Token".into(), "abc:def".into()),
            ]
        );
        assert!(RetroUrls { headers: "no colon here".into(), ..RetroUrls::default() }.request_headers().is_err());
        assert!(RetroUrls::default().request_headers().unwrap().is_empty());
        // Empty ones are not written to the settings file.
        let empty = serde_json::to_string(&RetroUrls::default()).unwrap();
        assert!(!empty.contains("referer") && !empty.contains("use_curl") && !empty.contains("parallel"), "{empty}");
    }

    use super::*;

    #[test]
    fn reads_the_csharp_file() {
        let json = r#"{
  "DefaultServer": "hotel.example.com",
  "Servers": ["hotel.example.com", "10.0.0.2:2222", "HOTEL.example.com"],
  "DefaultKey": "keys/hotel.example.com/id_ed25519",
  "Branches": { "duckietm/Octane": "dev" },
  "Layout": { "EmulatorRepository": "/srv/emu", "AtomCmsInstalled": "Installed" },
  "Look": "professional"
}"#;
        let settings: InstallerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.server_list(), vec!["hotel.example.com", "10.0.0.2:2222"]);
        assert_eq!(settings.branch_for("duckietm/Octane"), "dev");
        assert_eq!(settings.branch_for("duckietm/Polaris-Emulator"), "main");
        assert!(settings.is_professional_look());
        let layout = settings.layout.clone().unwrap();
        assert_eq!(layout.emulator_repository, "/srv/emu");
        assert_eq!(layout.gamedata, "/var/www/gamedata");
    }

    #[test]
    fn disabled_plugins_round_trip_and_default_to_none() {
        let old: InstallerSettings = serde_json::from_str(r#"{"Servers":[]}"#).unwrap();
        assert!(old.disabled_plugins.is_empty());
        let settings = InstallerSettings { disabled_plugins: vec!["gitea".into()], ..Default::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""DisabledPlugins":["gitea"]"#));
        assert_eq!(serde_json::from_str::<InstallerSettings>(&json).unwrap().disabled_plugins, vec!["gitea"]);
    }

    #[test]
    fn writes_pascal_case_without_nulls() {
        let mut settings = InstallerSettings::default();
        settings.set_branch("duckietm/Octane", "dev");
        settings.set_branch("duckietm/Octane-Renderer", "main");
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(json, r#"{"Servers":[],"Branches":{"duckietm/Octane":"dev"}}"#);
        assert_eq!(choice_for("dev").to_string(), "dev   (the newest changes, not always stable)");
        assert_eq!(choice_for("x").branch, "main");
    }
}
