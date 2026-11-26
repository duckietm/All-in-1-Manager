//! Hotel profiles: which hotel the rip talks to and where its header ids come from.
//!
//! * Official Habbo: G-Earth knows the client's packet names (Sulek / packet info), so the
//!   profile sets no ids and the ids G-Earth reports are used.
//! * Retros: G-Earth usually has no packet info for a retro's client, so the profile carries
//!   the four ids. Built-in profiles cover the common open-source revisions; the user adds or
//!   overrides profiles with JSON files in a profiles folder ([`profiles`]).
//!
//! Built-in ids, checked against the sources (November 2026):
//! * Arcturus Morningstar (`PRODUCTION-201611291003-338511768`), git.krews.org/krews/Morningstar,
//!   `Incoming.java`: `RequestCatalogModeEvent = 1195` (the handler that reads the mode and
//!   answers the index; `RequestCatalogIndexEvent = 2529` answers nothing),
//!   `RequestCatalogPageEvent = 412`; `Outgoing.java`: `CatalogPagesListComposer = 1032`,
//!   `CatalogPageComposer = 804`.
//! * Nitro (github.com/billsonnn/nitro-renderer): `OutgoingHeader.ts` `GET_CATALOG_INDEX = 1195`,
//!   `GET_CATALOG_PAGE = 412`; `IncomingHeader.ts` `CATALOG_PAGE_LIST = 1032`,
//!   `CATALOG_PAGE = 804`.
//! * Plus Emulator (`PRODUCTION-201601012205-226667486`): no source could be checked, so its
//!   profile is a template without ids; the rip stops with a message asking for them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::gearth::ConnectionInfo;

/// What kind of hotel a profile is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotelKind {
    /// habbo.com and its country hotels.
    Official,
    /// A retro played with a Flash/AIR client.
    FlashRetro,
    /// A retro played with the Nitro (HTML5) client.
    NitroRetro,
}

impl HotelKind {
    pub fn label(self) -> &'static str {
        match self {
            HotelKind::Official => "official Habbo",
            HotelKind::FlashRetro => "Flash retro",
            HotelKind::NitroRetro => "Nitro retro",
        }
    }
}

/// The four header ids; any of them may be missing (then the next source is used).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderMap {
    #[serde(rename = "GetCatalogIndex", default, skip_serializing_if = "Option::is_none")]
    pub get_catalog_index: Option<u16>,
    #[serde(rename = "GetCatalogPage", default, skip_serializing_if = "Option::is_none")]
    pub get_catalog_page: Option<u16>,
    #[serde(rename = "CatalogIndex", default, skip_serializing_if = "Option::is_none")]
    pub catalog_index: Option<u16>,
    #[serde(rename = "CatalogPage", default, skip_serializing_if = "Option::is_none")]
    pub catalog_page: Option<u16>,
}

impl HeaderMap {
    pub fn new(get_index: u16, get_page: u16, index: u16, page: u16) -> Self {
        Self {
            get_catalog_index: Some(get_index),
            get_catalog_page: Some(get_page),
            catalog_index: Some(index),
            catalog_page: Some(page),
        }
    }

    /// GetCatalogIndex, GetCatalogPage, CatalogIndex, CatalogPage.
    pub fn as_array(&self) -> [Option<u16>; 4] {
        [
            self.get_catalog_index,
            self.get_catalog_page,
            self.catalog_index,
            self.catalog_page,
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.as_array().iter().all(Option::is_none)
    }

    pub fn is_complete(&self) -> bool {
        self.as_array().iter().all(Option::is_some)
    }

    /// `GetCatalogIndex=1195,GetCatalogPage=412,CatalogIndex=1032,CatalogPage=804` (`,` or `;`
    /// between, `=` or `:` inside, names in any case). An empty text is an empty map.
    pub fn parse(text: &str) -> Result<Self> {
        let mut map = Self::default();
        for part in text
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let Some((name, id)) = part.split_once(['=', ':']) else {
                bail!("header ids: '{part}' is not name=id");
            };
            let Ok(id) = id.trim().parse::<u16>() else {
                bail!("header ids: '{}' is not a header id (0-65535)", id.trim());
            };
            let slot = match name.trim().to_ascii_lowercase().as_str() {
                "getcatalogindex" => &mut map.get_catalog_index,
                "getcatalogpage" => &mut map.get_catalog_page,
                "catalogindex" => &mut map.catalog_index,
                "catalogpage" => &mut map.catalog_page,
                other => bail!("header ids: unknown name '{other}' (use GetCatalogIndex, GetCatalogPage, CatalogIndex, CatalogPage)"),
            };
            *slot = Some(id);
        }
        Ok(map)
    }

    /// The text [`HeaderMap::parse`] reads.
    pub fn to_text(&self) -> String {
        let names = ["GetCatalogIndex", "GetCatalogPage", "CatalogIndex", "CatalogPage"];
        names
            .iter()
            .zip(self.as_array())
            .filter_map(|(n, id)| id.map(|id| format!("{n}={id}")))
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn default_true() -> bool {
    true
}

/// One hotel (or family of hotels).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotelProfile {
    /// Shown in the app and used as the key: a user profile with a built-in's name replaces it.
    pub name: String,
    pub kind: HotelKind,
    /// Host patterns of the game server, matched against G-Earth's connection host: an exact
    /// host, `*.example.com` (any sub-domain, and example.com itself) or `game-*.habbo.com`.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Client revisions (G-Earth's hotel version, e.g. PRODUCTION-201611291003-338511768) this
    /// profile is for; matched when no host pattern matched.
    #[serde(default)]
    pub revisions: Vec<String>,
    /// The ids; empty for official Habbo (G-Earth's packet info is used).
    #[serde(default)]
    pub headers: HeaderMap,
    /// Catalogue image folder (c_images/catalogue) of this hotel; the ripper settings' URL when
    /// not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_url: Option<String>,
    /// Page icon URL prefix (`<prefix><n>.png`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// This hotel's furnidata (URL or file), JSON or XML: product class ids become class names,
    /// matched against items_base.item_name of the target hotel. Official Habbo hotels need
    /// none (it follows from the host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub furnidata_url: Option<String>,
    /// This hotel's productdata (URL or file), for product names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub productdata_url: Option<String>,
    /// Where the client's configuration is: the hotel site (Nitro: renderer-config.json and
    /// ui-config.json there) or the Flash external_variables URL. The catalogue image and icon
    /// URLs are read from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_config_url: Option<String>,
    /// Header ids of the extra catalogue messages (club offers, bundle discounts, gift
    /// wrapping), by name: GetClubOffers, ClubOffers, GetBundleDiscountRuleset,
    /// BundleDiscountRuleset, GetGiftWrappingConfiguration, GiftWrappingConfiguration.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra_headers: std::collections::BTreeMap<String, u16>,
    /// Whether the hotel has a Builders Club catalogue to ask for.
    #[serde(default = "default_true")]
    pub builders_club: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Built into the library (not read from a file). Not stored.
    #[serde(skip)]
    pub builtin: bool,
    /// The file it was read from. Not stored.
    #[serde(skip)]
    pub file: Option<PathBuf>,
}

/// The Arcturus Morningstar / Nitro catalogue ids (see the module docs for the sources).
pub fn arcturus_headers() -> HeaderMap {
    HeaderMap::new(1195, 412, 1032, 804)
}

pub const OFFICIAL: &str = "Habbo (official)";
pub const ARCTURUS_FLASH: &str = "Arcturus Morningstar (Flash, PRODUCTION-201611291003-338511768)";
pub const NITRO: &str = "Nitro (Arcturus Morningstar + Nitro websockets)";
pub const PLUS_FLASH: &str = "Plus Emulator (Flash, PRODUCTION-201601012205-226667486)";

/// The profiles built into the library.
pub fn builtin_profiles() -> Vec<HotelProfile> {
    let base = |name: &str, kind: HotelKind| HotelProfile {
        name: name.into(),
        kind,
        hosts: Vec::new(),
        revisions: Vec::new(),
        headers: HeaderMap::default(),
        catalog_url: None,
        icon_url: None,
        furnidata_url: None,
        productdata_url: None,
        site_config_url: None,
        extra_headers: crate::extras::arcturus_extra_headers(),
        builders_club: true,
        notes: String::new(),
        builtin: true,
        file: None,
    };
    vec![
        HotelProfile {
            hosts: vec!["*.habbo.com".into(), "*.habbo.com.br".into(), "*.habbo.com.tr".into()],
            catalog_url: Some("https://images.habbo.com/c_images/catalogue".into()),
            icon_url: Some("https://images.habbo.com/c_images/catalogue/icon_".into()),
            notes: "Header ids come from G-Earth's packet info for the client's version; the furnidata from www.<hotel>/gamedata.".into(),
            extra_headers: BTreeMap::new(),
            ..base(OFFICIAL, HotelKind::Official)
        },
        HotelProfile {
            revisions: vec!["PRODUCTION-201611291003-338511768".into()],
            headers: arcturus_headers(),
            notes: "Arcturus Morningstar headers (Incoming.java / Outgoing.java). Set catalog_url to the hotel's c_images/catalogue for the images.".into(),
            ..base(ARCTURUS_FLASH, HotelKind::FlashRetro)
        },
        HotelProfile {
            headers: arcturus_headers(),
            notes: "nitro-renderer's default IncomingHeader.ts / OutgoingHeader.ts. Retros that changed their Nitro header ids need their own profile.".into(),
            ..base(NITRO, HotelKind::NitroRetro)
        },
        HotelProfile {
            revisions: vec!["PRODUCTION-201601012205-226667486".into()],
            builders_club: false,
            extra_headers: BTreeMap::new(),
            notes: "Template: the Plus Emulator ids could not be checked against a source. Copy this profile to a JSON file and fill in the four ids from the emulator's ClientPacketHeader.cs / ServerPacketHeader.cs.".into(),
            ..base(PLUS_FLASH, HotelKind::FlashRetro)
        },
    ]
}

/// Reads every `*.json` file in `dir` (one profile or an array of profiles each) and merges
/// them over the built-ins: a profile with a built-in's name (any case) replaces it. Files that
/// cannot be read are reported in the second list and skipped.
pub fn load_profiles(dir: &Path) -> (Vec<HotelProfile>, Vec<String>) {
    let mut all = builtin_profiles();
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    for file in files {
        match read_profile_file(&file) {
            Ok(list) => {
                for profile in list {
                    match all
                        .iter_mut()
                        .find(|p| p.name.eq_ignore_ascii_case(&profile.name))
                    {
                        Some(existing) => *existing = profile,
                        None => all.push(profile),
                    }
                }
            }
            Err(e) => errors.push(format!("{}: {e:#}", file.display())),
        }
    }
    (all, errors)
}

/// The built-in profiles merged with the user's JSON profiles in `dir` (unreadable files are
/// skipped; see [`load_profiles`] for the errors).
pub fn profiles(dir: &Path) -> Vec<HotelProfile> {
    load_profiles(dir).0
}

fn read_profile_file(file: &Path) -> Result<Vec<HotelProfile>> {
    let text = std::fs::read_to_string(file).context("reading")?;
    let value: serde_json::Value = serde_json::from_str(&text).context("not valid JSON")?;
    let mut list: Vec<HotelProfile> = if value.is_array() {
        serde_json::from_value(value).context("not a list of hotel profiles")?
    } else {
        vec![serde_json::from_value(value).context("not a hotel profile")?]
    };
    for profile in &mut list {
        if profile.name.trim().is_empty() {
            bail!("a profile has no name");
        }
        profile.builtin = false;
        profile.file = Some(file.to_path_buf());
    }
    Ok(list)
}

/// Writes `profile` to `<dir>/<name>.json` and returns the path.
pub fn save_profile(dir: &Path, profile: &HotelProfile) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.json", crate::images::safe_file_name(&profile.name)));
    std::fs::write(&path, serde_json::to_string_pretty(profile)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Whether `host` matches a host pattern (see [`HotelProfile::hosts`]).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }
    if let Some(domain) = pattern.strip_prefix("*.") {
        return host == domain || host.ends_with(&format!(".{domain}"));
    }
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            host.len() >= prefix.len() + suffix.len()
                && host.starts_with(prefix)
                && host.ends_with(suffix)
        }
        None => host == pattern,
    }
}

/// How a profile was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedBy {
    Host,
    Revision,
    ClientType,
}

/// Picks the profile for a hotel connection: a host pattern first (user profiles before
/// built-ins, the longest pattern wins), then the client revision, then the client type (a Nitro
/// client gets the Nitro profile; a Flash/AIR client with a PRODUCTION- revision the Arcturus
/// profile). `None` when nothing fits.
pub fn detect<'a>(
    profiles: &'a [HotelProfile],
    connection: &ConnectionInfo,
) -> Option<(&'a HotelProfile, DetectedBy)> {
    let ordered = || {
        profiles
            .iter()
            .filter(|p| !p.builtin)
            .chain(profiles.iter().filter(|p| p.builtin))
    };
    let by_host = ordered()
        .filter_map(|p| {
            p.hosts
                .iter()
                .filter(|h| host_matches(h, &connection.host))
                .map(|h| h.len())
                .max()
                .map(|len| (p, len))
        })
        .fold(None::<(&HotelProfile, usize)>, |best, (p, len)| match best {
            Some((_, best_len)) if best_len >= len => best,
            _ => Some((p, len)),
        });
    if let Some((profile, _)) = by_host {
        return Some((profile, DetectedBy::Host));
    }
    let version = connection.hotel_version.trim();
    if let Some(p) = ordered().find(|p| {
        p.revisions
            .iter()
            .any(|r| r.trim().eq_ignore_ascii_case(version))
    }) {
        return Some((p, DetectedBy::Revision));
    }
    let client = connection.client_type.to_ascii_uppercase();
    let wanted = if client == "NITRO" {
        Some(HotelKind::NitroRetro)
    } else if (client == "FLASH" || client.is_empty())
        && version.to_ascii_uppercase().starts_with("PRODUCTION-")
    {
        Some(HotelKind::FlashRetro)
    } else {
        None
    }?;
    // The first complete profile of that kind: a user's own before the built-in.
    ordered()
        .find(|p| p.kind == wanted && p.headers.is_complete() && p.hosts.is_empty())
        .map(|p| (p, DetectedBy::ClientType))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(host: &str, version: &str, client: &str) -> ConnectionInfo {
        ConnectionInfo {
            host: host.into(),
            port: 30000,
            hotel_version: version.into(),
            client_identifier: String::new(),
            client_type: client.into(),
            packets: Vec::new(),
        }
    }

    #[test]
    fn host_patterns() {
        assert!(host_matches("*.habbo.com", "game-nl.habbo.com"));
        assert!(host_matches("*.habbo.com", "habbo.com"));
        assert!(!host_matches("*.habbo.com", "nothabbo.com"));
        assert!(host_matches("game-*.habbo.com", "game-us.habbo.com"));
        assert!(!host_matches("game-*.habbo.com", "habbo.com"));
        assert!(host_matches("Game.MyRetro.net", "game.myretro.net."));
        assert!(!host_matches("", "x"));
    }

    #[test]
    fn header_text_round_trip() {
        let map = HeaderMap::parse("getcatalogindex:1; CatalogPage = 4").unwrap();
        assert_eq!(map.as_array(), [Some(1), None, None, Some(4)]);
        assert_eq!(map.to_text(), "GetCatalogIndex=1,CatalogPage=4");
        assert!(HeaderMap::parse("").unwrap().is_empty());
        assert!(HeaderMap::parse("CatalogPage=70000").is_err());
    }

    #[test]
    fn detection() {
        let all = builtin_profiles();
        let pick = |c: ConnectionInfo| detect(&all, &c).map(|(p, by)| (p.name.as_str(), by));
        assert_eq!(
            pick(conn("game-nl.habbo.com", "WIN63-2025", "FLASH")),
            Some((OFFICIAL, DetectedBy::Host))
        );
        assert_eq!(
            pick(conn("game.retro.net", "PRODUCTION-201611291003-338511768", "FLASH")),
            Some((ARCTURUS_FLASH, DetectedBy::Revision))
        );
        assert_eq!(
            pick(conn("ws.retro.net", "", "NITRO")),
            Some((NITRO, DetectedBy::ClientType))
        );
        assert_eq!(
            pick(conn("game.retro.net", "PRODUCTION-201701242205-837386173", "FLASH")),
            Some((ARCTURUS_FLASH, DetectedBy::ClientType))
        );
        assert_eq!(pick(conn("game.retro.net", "WIN63-x", "UNITY")), None);
    }

    #[test]
    fn user_profiles_override_and_win_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("my.json"),
            r#"[{"name":"My Retro","kind":"flash_retro","hosts":["*.myretro.net"],
                 "headers":{"GetCatalogIndex":1,"GetCatalogPage":2,"CatalogIndex":3,"CatalogPage":4}},
                {"name":"nitro (arcturus morningstar + nitro websockets)","kind":"nitro_retro",
                 "headers":{"GetCatalogIndex":10,"GetCatalogPage":11,"CatalogIndex":12,"CatalogPage":13}}]"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.json"), "{").unwrap();
        let (all, errors) = load_profiles(dir.path());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("broken.json"));
        assert_eq!(all.len(), builtin_profiles().len() + 1);
        let mine = detect(&all, &conn("game.myretro.net", "PRODUCTION-201611291003-338511768", "FLASH")).unwrap();
        assert_eq!((mine.0.name.as_str(), mine.1), ("My Retro", DetectedBy::Host));
        assert!(mine.0.builders_club, "builders_club defaults to true");
        let nitro = detect(&all, &conn("x", "", "NITRO")).unwrap().0;
        assert_eq!(nitro.headers.catalog_page, Some(13));
        assert!(!nitro.builtin);

        let saved = save_profile(dir.path(), mine.0).unwrap();
        let back = read_profile_file(&saved).unwrap();
        assert_eq!(back[0].headers, mine.0.headers);
    }

    #[test]
    fn builtins_are_consistent() {
        for p in builtin_profiles() {
            match p.kind {
                HotelKind::Official => assert!(p.headers.is_empty()),
                _ if p.name == PLUS_FLASH => assert!(p.headers.is_empty()),
                _ => assert!(p.headers.is_complete(), "{}", p.name),
            }
        }
    }
}
