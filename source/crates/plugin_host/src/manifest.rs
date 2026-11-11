//! `manifest.json` of an `.aiop` package.

use crate::permissions::Permission;
use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// The highest minor version of API major 1 this host implements.
pub const HOST_API_MINOR: u32 = 0;

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
    (2..=40).contains(&b.len())
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
            Some([1, minor, ..]) if *minor <= HOST_API_MINOR => {}
            _ => bail!("Unsupported plugin API: {}", raw.api),
        }
        if raw.name.trim().is_empty() || raw.page_title.trim().is_empty() {
            bail!("name and page_title must not be empty");
        }
        for s in [&raw.name, &raw.author, &raw.description, &raw.page_title] {
            if s.chars().count() > 500 {
                bail!("A manifest text is too long");
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
