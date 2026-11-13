//! `index.json` schema 1: parsing with every limit and field check, revocations and freshness.

use aio_plugin_host::manifest::{spoofing, valid_id};
use aio_plugin_host::Permission;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The only `schema` value this Manager understands.
pub const SCHEMA: u32 = 1;
/// Most plugins one index may list.
pub const MAX_PLUGINS: usize = 2000;
/// Largest `index.json` accepted, checked before parsing.
pub const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
/// Largest package an entry may announce: the host's package cap, so nothing installable is refused.
pub const MAX_PACKAGE_BYTES: u64 = aio_plugin_host::package::MAX_PACKAGE as u64;
/// Most permissions one entry may list (as in a manifest).
pub const MAX_PERMISSIONS: usize = aio_plugin_host::manifest::MAX_PERMISSIONS;
/// Longest text field, in characters (as in a manifest).
const MAX_TEXT_CHARS: usize = 500;
/// Longest `url`, in bytes.
const MAX_URL_BYTES: usize = 2048;

/// RFC 3339 timestamps via `DateTime::parse_from_rfc3339`: a time-zone offset is required (a missing one is
/// refused); like RFC 3339 section 5.6, chrono also accepts a space or lowercase `t` as the date/time separator and a
/// lowercase `z`. Offsets are converted to UTC.
mod rfc3339 {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|_| D::Error::custom(format!("not an RFC 3339 timestamp: {s}")))
    }
}

/// Where an entry's package was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub repo: String,
    pub commit: String,
}

/// One plugin release listed by the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub api: String,
    /// Permission strings, each valid by [`Permission::parse`] (see [`Entry::permissions`]).
    pub permissions: Vec<String>,
    /// Package size in bytes.
    pub size: u64,
    /// SHA-256 of the package, 64 lowercase hex characters.
    pub sha256: String,
    pub url: String,
    pub source: Source,
}

/// Versions of a plugin the index withdraws; `versions` is `["*"]` for every version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revoked {
    pub id: String,
    pub versions: Vec<String>,
    pub reason: String,
}

/// A parsed and checked `index.json`. Only [`Index::parse`] makes one from untrusted bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema: u32,
    pub sequence: u64,
    #[serde(with = "rfc3339")]
    pub generated: DateTime<Utc>,
    #[serde(with = "rfc3339")]
    pub expires: DateTime<Utc>,
    pub plugins: Vec<Entry>,
    pub revoked: Vec<Revoked>,
}

/// Whether a verified index may be used to install and update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Not older than the last index seen and not past `expires`.
    Current,
    /// Past `expires`: it can still revoke, but not install or update.
    Expired,
    /// Its `sequence` is lower than the highest one seen: refused entirely.
    Rollback,
}

/// Dot separated plain decimal numbers (no signs, no leading zeros), `min..=max` of them (the manifest's rule).
fn numbers(s: &str, min: usize, max: usize) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    (min..=max).contains(&parts.len())
        && parts.iter().all(|p| {
            !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0'))
        })
}

fn valid_version(v: &str) -> bool {
    numbers(v, 3, 3)
}

fn lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A text field: at most [`MAX_TEXT_CHARS`] characters, no control characters (newline and tab excepted), no
/// invisible or direction-changing ones.
fn check_text(what: &str, s: &str) -> Result<()> {
    if s.chars().count() > MAX_TEXT_CHARS {
        bail!("{what} is too long");
    }
    if s.chars().any(|c| (c.is_control() && !matches!(c, '\n' | '\t')) || spoofing(c)) {
        bail!("{what} holds control or invisible characters");
    }
    Ok(())
}

/// Printable ASCII without `%`, `\`, `?`, `#` or a `..` sequence: nothing an HTTP client would decode, normalise or
/// cut off, so the url it fetches is the string that was checked.
fn plain_url(url: &str) -> bool {
    url.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'%' | b'\\' | b'?' | b'#')) && !url.contains("..")
}

/// The package url prefix: a plain url ending in `/`, so `http://host:8080` cannot match `http://host:8080.evil`.
fn check_prefix(url_prefix: &str) -> Result<()> {
    if !url_prefix.ends_with('/') || !plain_url(url_prefix) || url_prefix.len() > MAX_URL_BYTES {
        bail!("Invalid package url prefix (it must be a plain url ending in /): {url_prefix}");
    }
    Ok(())
}

fn check_entry(e: &Entry, url_prefix: &str) -> Result<()> {
    if !valid_id(&e.id) {
        bail!("Invalid plugin id in the index: {}", e.id);
    }
    let id = &e.id;
    if !valid_version(&e.version) {
        bail!("Invalid version of {id}: {}", e.version);
    }
    if !numbers(&e.api, 2, 3) {
        bail!("Invalid api of {id}: {}", e.api);
    }
    if e.name.trim().is_empty() {
        bail!("The name of {id} is empty");
    }
    for (what, s) in [
        ("name", &e.name),
        ("author", &e.author),
        ("description", &e.description),
        ("source repo", &e.source.repo),
        ("source commit", &e.source.commit),
    ] {
        check_text(&format!("The {what} of {id}"), s)?;
    }
    if e.permissions.len() > MAX_PERMISSIONS {
        bail!("{id} lists more than {MAX_PERMISSIONS} permissions");
    }
    for (i, p) in e.permissions.iter().enumerate() {
        Permission::parse(p).with_context(|| format!("Invalid permission of {id}"))?;
        if e.permissions[..i].contains(p) {
            bail!("{id} lists a permission twice: {p}");
        }
    }
    if e.size == 0 || e.size > MAX_PACKAGE_BYTES {
        bail!("Invalid package size of {id}: {}", e.size);
    }
    if !lower_hex(&e.sha256, 64) {
        bail!("Invalid sha256 of {id}");
    }
    if e.url.len() > MAX_URL_BYTES || !plain_url(&e.url) {
        bail!("Invalid package url of {id}");
    }
    // Exactly `<prefix><id>-<version>/<id>.aiop`, compared as a string: the url cannot leave the prefix's directory.
    let expected = format!("{url_prefix}{id}-{}/{id}.aiop", e.version);
    if e.url != expected {
        bail!("The package url of {id} is not {expected}");
    }
    Ok(())
}

fn check_revoked(r: &Revoked) -> Result<()> {
    if !valid_id(&r.id) {
        bail!("Invalid plugin id in the revocations: {}", r.id);
    }
    if r.versions.is_empty() {
        bail!("The revocation of {} lists no versions", r.id);
    }
    let star = r.versions.iter().any(|v| v == "*");
    if star && r.versions.len() != 1 {
        bail!("The revocation of {} mixes \"*\" with versions", r.id);
    }
    if let Some(v) = r.versions.iter().find(|v| !star && !valid_version(v)) {
        bail!("Invalid revoked version of {}: {v}", r.id);
    }
    check_text(&format!("The revocation reason of {}", r.id), &r.reason)
}

impl Index {
    /// Parses and checks `index.json`. Every entry `url` must be exactly `<url_prefix><id>-<version>/<id>.aiop`, where
    /// `url_prefix` ends in `/` (the official
    /// `https://github.com/all-in-1-plugins/index/releases/download/`, or a test prefix). Any problem refuses the
    /// whole index.
    pub fn parse(bytes: &[u8], url_prefix: &str) -> Result<Index> {
        if bytes.len() > MAX_INDEX_BYTES {
            bail!("The plugin index is larger than 4 MiB");
        }
        check_prefix(url_prefix)?;
        let index: Index = serde_json::from_slice(bytes).context("The plugin index is not valid")?;
        if index.schema != SCHEMA {
            bail!("Unsupported plugin index schema: {}", index.schema);
        }
        if index.generated > index.expires {
            bail!("The plugin index expires before it was generated");
        }
        if index.plugins.len() > MAX_PLUGINS {
            bail!("The plugin index lists more than {MAX_PLUGINS} plugins");
        }
        let mut ids: Vec<&str> = Vec::with_capacity(index.plugins.len());
        for e in &index.plugins {
            check_entry(e, url_prefix)?;
            ids.push(&e.id);
        }
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            bail!("The plugin index lists {} twice", w[0]);
        }
        for r in &index.revoked {
            check_revoked(r)?;
        }
        Ok(index)
    }

    /// The entry for plugin `id`.
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.plugins.iter().find(|e| e.id == id)
    }

    /// The revocation that covers `version` of plugin `id` (by that version or by `"*"`).
    pub fn revocation_for(&self, id: &str, version: &str) -> Option<&Revoked> {
        self.revoked.iter().find(|r| r.id == id && r.versions.iter().any(|v| v == "*" || v == version))
    }
}

impl Entry {
    /// The permissions as parsed values ([`Index::parse`] already refused any that do not parse).
    pub fn permissions(&self) -> Result<Vec<Permission>> {
        self.permissions.iter().map(|p| Permission::parse(p)).collect()
    }
}

/// Freshness of a verified index: [`Freshness::Rollback`] when its `sequence` is lower than `last_sequence` (the
/// highest one seen; the same one is fine), else [`Freshness::Expired`] when `now` is past `expires`, else
/// [`Freshness::Current`].
pub fn check_fresh(index: &Index, last_sequence: Option<u64>, now: DateTime<Utc>) -> Freshness {
    if last_sequence.is_some_and(|last| index.sequence < last) {
        Freshness::Rollback
    } else if now <= index.expires {
        Freshness::Current
    } else {
        Freshness::Expired
    }
}
