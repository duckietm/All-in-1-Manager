//! `manifest.json` of an `.aiop` package.

use crate::permissions::Permission;
use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// The highest minor version of API major 1 this host implements.
pub const HOST_API_MINOR: u32 = 0;

/// Invisible, direction-changing and separator characters that can make text lie about what it says. One list,
/// shared by the manifest check, [`display_safe`] and the runtime's log sanitiser.
pub fn spoofing(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// A character a manifest text must not hold: control characters (newline and tab excepted) and [`spoofing`] ones.
fn refused(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\t')) || spoofing(c)
}

/// Plugin-supplied text made safe to show: control and spoofing characters become U+FFFD, whitespace runs one space,
/// and the text is cut to `max_chars` characters with an ellipsis.
pub fn display_safe(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if refused(c) {
            if space {
                out.push(' ');
                space = false;
            }
            out.push('\u{FFFD}');
        } else if c.is_whitespace() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if out.chars().count() > max_chars {
        let keep = max_chars.saturating_sub(1);
        out = out.chars().take(keep).collect::<String>() + "\u{2026}";
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tile {
    pub title: String,
    pub description: String,
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub api: String,
    pub permissions: Vec<Permission>,
    pub page_title: String,
    pub tile: Option<Tile>,
}

#[derive(Deserialize)]
struct Raw {
    id: String,
    name: String,
    version: String,
    author: String,
    description: String,
    api: String,
    #[serde(default)]
    permissions: Vec<String>,
    page_title: String,
    #[serde(default)]
    tile: Option<Tile>,
}

/// Dot separated plain decimal numbers (no signs, no leading zeros), `min..=max` of them.
fn numbers(s: &str, min: usize, max: usize) -> Option<Vec<u32>> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < min || parts.len() > max {
        return None;
    }
    parts
        .iter()
        .map(|p| {
            let plain = !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0'));
            if plain { p.parse().ok() } else { None }
        })
        .collect()
}

/// `^[a-z0-9][a-z0-9-]{1,39}$`
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    let reserved = matches!(id, "con" | "prn" | "aux" | "nul")
        || ((id.starts_with("com") || id.starts_with("lpt")) && id.len() == 4 && matches!(b[3], b'1'..=b'9'));
    !reserved
        && (2..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl Manifest {
    pub fn parse(json: &[u8]) -> Result<Manifest> {
        let raw: Raw = serde_json::from_slice(json).context("manifest.json is not valid")?;
        if !valid_id(&raw.id) {
            bail!("Invalid plugin id: {}", raw.id);
        }
        if numbers(&raw.version, 3, 3).is_none() {
            bail!("Invalid version (expected MAJOR.MINOR.PATCH): {}", raw.version);
        }
        match numbers(&raw.api, 2, 3).as_deref() {
            Some([1, minor, ..]) if (0..=HOST_API_MINOR).contains(minor) => {}
            _ => bail!("Unsupported plugin API: {}", raw.api),
        }
        if raw.name.trim().is_empty() || raw.page_title.trim().is_empty() {
            bail!("name and page_title must not be empty");
        }
        let tile_texts = raw.tile.iter().flat_map(|t| [&t.title, &t.description, &t.color]);
        for s in [&raw.name, &raw.author, &raw.description, &raw.page_title].into_iter().chain(tile_texts) {
            if s.chars().count() > 500 {
                bail!("A manifest text is too long");
            }
            if s.chars().any(refused) {
                bail!("A manifest text holds control or invisible characters");
            }
        }
        let permissions = raw.permissions.iter().map(|p| Permission::parse(p)).collect::<Result<Vec<_>>>()?;
        Ok(Manifest {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            author: raw.author,
            description: raw.description,
            api: raw.api,
            permissions,
            page_title: raw.page_title,
            tile: raw.tile,
        })
    }
}
