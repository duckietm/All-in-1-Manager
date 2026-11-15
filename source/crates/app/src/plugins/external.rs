//! External plugins: the `.aiop` packages installed from a file into configuration/plugins/ext/<id>/. This is the
//! app side of [`aio_plugin_host::package`]: the list the Plugins page shows, the consent a user gives before
//! anything is written, and the plain-words texts for each permission.
//!
//! Switching one on or off uses the same store as the built-in plugins (`DisabledPlugins`), under the id
//! `ext:<id>`, so a third-party id can never collide with a built-in one.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aio_plugin_host::package::{self, Package, MAX_PACKAGE, MAX_WASM};
use aio_plugin_host::manifest::valid_id;
use aio_plugin_host::{display_safe, Manifest, Permission};
use aio_plugin_index::{Entry, Index, Revoked};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::index::{IndexSource, SignedIndex, OFFICIAL_PACKAGE_PREFIX};
use super::{packs, PLUGINS};
use crate::i18n::{n, t};

/// An installed external plugin, as the Plugins page lists it.
#[derive(Debug, Clone)]
pub struct ExternalInfo {
    pub manifest: Manifest,
    /// Installed from the official index and unchanged since: its `verified.json` matches the installed files (see
    /// [`ExternalPlugins::reload`]). A file install, a test-index install or a developer folder is never verified.
    pub verified: bool,
    /// Loaded from a developer folder (developer mode), not installed.
    pub dev: bool,
}

/// A package that was read and checked and waits for the user's consent. Nothing is written yet.
#[derive(Debug, Clone)]
pub struct Pending {
    /// Read-only ([`Pending::package`]): what was checked is what gets installed.
    package: Package,
    /// The installed version this one replaces, if any.
    pub previous: Option<Manifest>,
    /// Set only by [`ExternalPlugins::prepare_verified`] (the field is private so nothing else can make a verified
    /// Pending); `None` for a file.
    verified: Option<Verification>,
}

/// What a verified install was checked against: the signed index's entry and the key that signed the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub source: IndexSource,
    pub key_id: String,
    /// The index's `sequence`.
    pub sequence: u64,
    /// SHA-256 of the downloaded package (lowercase hex), equal to the entry's.
    pub sha256: String,
}

impl Pending {
    /// The checked package.
    pub fn package(&self) -> &Package {
        &self.package
    }

    /// How the package was verified against the signed index; `None` for a package from a file.
    pub fn verification(&self) -> Option<&Verification> {
        self.verified.as_ref()
    }

    /// Whether the permission is new compared with the installed version (never true for a first install).
    pub fn is_new_permission(&self, p: &Permission) -> bool {
        self.previous.as_ref().is_some_and(|old| !old.permissions.contains(p))
    }

    /// The installed plugin with this id has another author: this package is treated as a different plugin, which does
    /// not get the installed one's private data (Ruling 22).
    pub fn author_changed(&self) -> bool {
        self.previous.as_ref().is_some_and(|old| old.author != self.package.manifest.author)
    }
}

/// A developer folder that was read and checked and waits for the user's consent (or whose permissions grew).
#[derive(Debug, Clone, PartialEq)]
pub struct DevPending {
    pub folder: PathBuf,
    pub manifest: Manifest,
    /// The `.wasm` file inside the folder.
    pub wasm: PathBuf,
    /// What the user allowed before; None the first time.
    pub previous: Option<Vec<Permission>>,
}

impl DevPending {
    /// Whether the permission is new compared with what was allowed before (never true the first time).
    pub fn is_new_permission(&self, p: &Permission) -> bool {
        self.previous.as_ref().is_some_and(|old| !old.contains(p))
    }
}

/// True when running a plugin that now asks for `now` needs the user's consent: nothing was allowed yet, or it asks
/// for something that was not allowed. Asking for less than before needs none.
pub fn needs_consent(granted: Option<&[Permission]>, now: &[Permission]) -> bool {
    match granted {
        None => true,
        Some(granted) => now.iter().any(|p| !granted.contains(p)),
    }
}

/// How a developer folder stands.
#[derive(Debug, Clone)]
pub enum DevState {
    /// Allowed and valid: it is in [`ExternalPlugins::list`].
    Ready(ExternalInfo),
    /// Valid, but the user has not agreed to its permissions (yet, or since they grew).
    NeedsConsent(Manifest),
    /// Cannot be used; the text says why.
    Problem(String),
}

#[derive(Debug, Clone)]
pub struct DevRow {
    pub folder: PathBuf,
    pub state: DevState,
}

/// What the page's Reload found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevReload {
    Reloaded,
    /// The plugin asks for more than the user allowed: it is stopped until they agree again.
    NeedsConsent,
}

struct DevEntry {
    folder: PathBuf,
    /// What the user allowed in this session; None until they agree.
    granted: Option<Vec<Permission>>,
    /// The plugin id the consent was given for: a manifest that now has another id is a different plugin.
    granted_id: String,
    /// The last good read of the folder, or why it failed.
    loaded: Result<(Manifest, PathBuf), String>,
}

#[derive(Default)]
struct Dev {
    enabled: bool,
    entries: Vec<DevEntry>,
}

/// Reads a developer folder: `manifest.json` (checked like a package's) and the `.wasm` it runs, which is
/// `plugin.wasm` or the manifest's optional `dev_wasm` path, relative to the folder and inside it.
fn read_dev_folder(folder: &Path) -> Result<(Manifest, PathBuf)> {
    let json = std::fs::read(folder.join("manifest.json")).context("The folder has no readable manifest.json")?;
    let manifest = Manifest::parse(&json)?;
    if reserved_ids().contains(&manifest.id.as_str()) {
        bail!("The plugin id {} belongs to a built-in plugin", manifest.id);
    }
    let value: serde_json::Value = serde_json::from_slice(&json).context("manifest.json is not valid")?;
    let relative = match value.get("dev_wasm") {
        None | Some(serde_json::Value::Null) => "plugin.wasm",
        Some(serde_json::Value::String(path)) => path.as_str(),
        Some(_) => bail!("dev_wasm must be a path"),
    };
    let path = Path::new(relative);
    if relative.is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        bail!("dev_wasm must be a path inside the folder (no absolute path, no ..): {}", display_safe(relative, 100));
    }
    let base = std::fs::canonicalize(folder).context("The folder cannot be read")?;
    let wasm = std::fs::canonicalize(base.join(path)).with_context(|| format!("The plugin file {} was not found", display_safe(relative, 100)))?;
    // A link inside the folder that leads out of it is outside too.
    if !wasm.starts_with(&base) || !wasm.is_file() {
        bail!("dev_wasm must be a file inside the folder: {}", display_safe(relative, 100));
    }
    if std::fs::metadata(&wasm)?.len() > MAX_WASM {
        bail!("The plugin file is larger than 24 MiB");
    }
    Ok((manifest, wasm))
}

pub struct ExternalPlugins {
    root: PathBuf,
    list: Mutex<Vec<ExternalInfo>>,
    dev: Mutex<Dev>,
    revoked: Mutex<Revocations>,
    /// The ids of the official index keys: a verified install signed with another key is not shown as verified.
    trusted_keys: Vec<String>,
}

/// The revocations applied last ([`ExternalPlugins::set_revocations`]): they lock the switch of every installed
/// plugin they cover. The official ones (the persisted lock, see [`super::index::RevocationLock`]) always apply; the
/// test index's only add to them (Ruling 8).
#[derive(Debug, Clone, Default)]
struct Revocations {
    official: Vec<Revoked>,
    test: Vec<Revoked>,
}

impl Revocations {
    /// The revocation covering `version` of `id`, the official one first.
    fn find(&self, id: &str, version: &str) -> Option<(IndexSource, &Revoked)> {
        let official = self.official.iter().find(|r| covers(r, id, version)).map(|r| (IndexSource::Official, r));
        official.or_else(|| self.test.iter().find(|r| covers(r, id, version)).map(|r| (IndexSource::Test, r)))
    }
}

/// Why an installed plugin is revoked, and by which index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revocation {
    pub source: IndexSource,
    /// The index's reason, already passed through `display_safe`.
    pub reason: String,
}

/// Whether `revoked` covers `version` of plugin `id` (by that version or by `"*"`).
fn covers(revoked: &Revoked, id: &str, version: &str) -> bool {
    revoked.id == id && revoked.versions.iter().any(|v| v == "*" || v == version)
}

/// The entry of `index` that updates `installed`: the same id, the same author, a strictly greater version (compared
/// as numbers) and not revoked.
pub fn offered_update<'a>(index: &'a Index, installed: &Manifest) -> Option<&'a Entry> {
    let entry = index.entry(&installed.id)?;
    let newer = version_key(&entry.version)? > version_key(&installed.version)?;
    let usable = newer && entry.author == installed.author && index.revocation_for(&entry.id, &entry.version).is_none();
    usable.then_some(entry)
}

/// The process-wide list, loaded the first time it is needed from configuration/plugins/ext/.
pub fn plugins() -> &'static ExternalPlugins {
    static STATE: OnceLock<ExternalPlugins> = OnceLock::new();
    STATE.get_or_init(|| {
        let plugins = ExternalPlugins::new(packs::root().join("ext"));
        let settings = aio_core::InstallerSettings::load();
        let folders: Vec<PathBuf> = settings.developer_plugins.iter().map(PathBuf::from).collect();
        plugins.set_developer(settings.developer_mode, &folders);
        plugins
    })
}

/// The key of an external plugin in the on/off store.
pub fn settings_id(id: &str) -> String {
    format!("ext:{id}")
}

/// How many instances of each plugin are alive (a page open, or a call still ending after its page closed).
fn live() -> &'static Mutex<HashMap<String, usize>> {
    static STATE: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Held next to a running `PluginInstance`; while any exists, the plugin's files are not removed or replaced (its
/// `plugin.wasm` and `data/` may be open: Windows refuses to move or delete open files).
pub struct LiveGuard(String);

impl LiveGuard {
    pub fn new(id: &str) -> Self {
        *live().lock().entry(id.to_string()).or_default() += 1;
        Self(id.to_string())
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let mut all = live().lock();
        if let Some(n) = all.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                all.remove(&self.0);
            }
        }
    }
}

pub fn is_live(id: &str) -> bool {
    live().lock().contains_key(id)
}

/// Blocks until no instance of `id` is alive. A closed page's call ends within the 5 s call limit; 10 s is the most
/// this waits. Blocking: run it off the window's thread.
fn wait_until_stopped(id: &str) -> Result<()> {
    let start = Instant::now();
    while is_live(id) {
        if start.elapsed() > Duration::from_secs(10) {
            bail!("The plugin is still running: close its page and try again.");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn reserved_ids() -> Vec<&'static str> {
    PLUGINS.iter().map(|p| p.id).collect()
}

/// Reads a package file, refusing anything over the 32 MiB limit without reading it all.
fn read_capped(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("The file {} cannot be opened", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE as u64 + 1).read_to_end(&mut bytes).context("The file cannot be read")?;
    if bytes.len() > MAX_PACKAGE {
        bail!("The package is larger than 32 MiB");
    }
    Ok(bytes)
}

/// The GitHub hosts a release download of the official index redirects to (Ruling 3). Integrity comes from the signed
/// size and SHA-256, so this list is about availability: any other redirect is refused.
pub const PACKAGE_CDN_HOSTS: &[&str] = &["objects.githubusercontent.com", "release-assets.githubusercontent.com"];
/// The whole download, redirects included.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Most redirects one download follows.
const MAX_REDIRECTS: usize = 5;
/// `configuration/plugins/ext/<id>/verified.json`: what a verified install recorded.
const VERIFIED_FILE: &str = "verified.json";
/// `verified.json` is a few short fields.
const MAX_VERIFIED_BYTES: u64 = 4 * 1024;

/// Whether a package download that started at `first` may follow a redirect to `next`. The official index: only to
/// GitHub's asset CDN ([`PACKAGE_CDN_HOSTS`]), https on the default port, no login in the url. A test index: only the
/// same host and port as `first`, never from https down to http.
pub fn redirect_allowed(kind: IndexSource, first: &reqwest::Url, next: &reqwest::Url) -> bool {
    let plain = next.username().is_empty() && next.password().is_none();
    match kind {
        IndexSource::Official => {
            let host = next.host_str().unwrap_or_default().to_ascii_lowercase();
            plain && next.scheme() == "https" && next.port().is_none() && PACKAGE_CDN_HOSTS.contains(&host.as_str())
        }
        IndexSource::Test => {
            let same_host = matches!((first.host_str(), next.host_str()), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b));
            let scheme_ok = matches!((first.scheme(), next.scheme()), ("https", "https") | ("http", "http") | ("http", "https"));
            plain && same_host && scheme_ok && first.port_or_known_default() == next.port_or_known_default()
        }
    }
}

/// The client for package downloads from the index of `kind`: the redirect policy of [`redirect_allowed`] (at most
/// five redirects), the Manager's user agent.
pub fn package_client(kind: IndexSource) -> reqwest::Client {
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let allowed = attempt.previous().first().is_some_and(|first| redirect_allowed(kind, first, attempt.url()));
        if !allowed {
            attempt.error("a redirect to another host was refused")
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .user_agent(format!("All-in-1-Manager/{}", crate::update::CURRENT_VERSION))
        .redirect(policy)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("HTTP client")
}

/// Downloads the package of `entry` (from an index of `kind`), within [`DOWNLOAD_TIMEOUT`]. The body is streamed and
/// refused as soon as it passes `entry.size` (at most 32 MiB); its size and SHA-256 must equal the entry's. Nothing
/// the server says (headers, length) is trusted: only the signed entry.
pub async fn download(client: &reqwest::Client, entry: &Entry, kind: IndexSource) -> Result<Vec<u8>> {
    // `Index::parse` already bound the url to the feed's prefix; this keeps an official download on GitHub's releases
    // whatever the caller passes.
    if kind == IndexSource::Official && !entry.url.starts_with(OFFICIAL_PACKAGE_PREFIX) {
        bail!("The package url is not an official one");
    }
    match tokio::time::timeout(DOWNLOAD_TIMEOUT, fetch_package(client, entry)).await {
        Ok(result) => result,
        Err(_) => bail!("The download took longer than {} seconds", DOWNLOAD_TIMEOUT.as_secs()),
    }
}

async fn fetch_package(client: &reqwest::Client, entry: &Entry) -> Result<Vec<u8>> {
    let cap = entry.size.min(MAX_PACKAGE as u64);
    let too_large = || anyhow::anyhow!("The package is larger than the index says ({} bytes)", entry.size);
    let mut response = client.get(&entry.url).send().await.map_err(|e| anyhow::anyhow!("The package could not be downloaded: {}", error_chain(&e)))?;
    let status = response.status();
    if !status.is_success() {
        bail!("The package could not be downloaded: HTTP {}", status.as_u16());
    }
    // Only an early refusal: the body is counted below whatever the header says.
    if response.content_length().is_some_and(|n| n > cap) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| anyhow::anyhow!("The package could not be downloaded: {}", error_chain(&e)))? {
        if (body.len() + chunk.len()) as u64 > cap {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    check_integrity(entry, &body)?;
    Ok(body)
}

fn error_chain(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

/// Lowercase hex SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, read in pieces (a plugin.wasm can be 24 MiB).
fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

/// The package bytes are the ones the entry signed: the same size and SHA-256.
fn check_integrity(entry: &Entry, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != entry.size {
        bail!("The package size ({} bytes) is not the one the index lists ({} bytes)", bytes.len(), entry.size);
    }
    if sha256_hex(bytes) != entry.sha256 {
        bail!("The package's SHA-256 is not the one the index lists: it was changed or damaged");
    }
    Ok(())
}

/// The manifest of a downloaded package must say exactly what the signed entry says: id, version, author, api, and
/// the same permissions (as parsed values, in any order, none twice).
fn check_matches_entry(manifest: &Manifest, entry: &Entry) -> Result<()> {
    for (what, package, index) in [
        ("id", &manifest.id, &entry.id),
        ("version", &manifest.version, &entry.version),
        ("author", &manifest.author, &entry.author),
        ("api", &manifest.api, &entry.api),
    ] {
        if package != index {
            bail!("The package's {what} ({}) is not the one the index lists ({})", display_safe(package, 80), display_safe(index, 80));
        }
    }
    let listed = entry.permissions().context("The index lists an invalid permission")?;
    let twice = |list: &[Permission]| list.iter().enumerate().any(|(i, p)| list[..i].contains(p));
    let same = listed.len() == manifest.permissions.len()
        && !twice(&listed)
        && !twice(&manifest.permissions)
        && manifest.permissions.iter().all(|p| listed.contains(p));
    if !same {
        bail!("The package's permissions are not the ones the index lists");
    }
    Ok(())
}

/// `verified.json` in an installed plugin's folder, written by [`ExternalPlugins::commit`] after a verified install.
/// `sha256`, `version`, `sequence` and `key_id` say what the signed index listed; the package itself is not kept, so
/// `wasm_sha256` and `manifest_sha256` are the hashes of the installed `plugin.wasm` and `manifest.json`, compared
/// with the files on every [`ExternalPlugins::reload`]. A file install replaces the folder, which drops this file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedRecord {
    sha256: String,
    version: String,
    sequence: u64,
    key_id: String,
    /// `official` or `test`.
    source: String,
    wasm_sha256: String,
    manifest_sha256: String,
}

fn source_name(source: IndexSource) -> &'static str {
    match source {
        IndexSource::Official => "official",
        IndexSource::Test => "test",
    }
}

/// True when the plugin installed in `dir` with `manifest` was installed from the official index and its files are
/// unchanged since: `verified.json` parses, is from the official index, was signed with a key that is still one of
/// `trusted_keys` (a rotation away from a stolen key takes the mark off what that key's index installed), has the
/// manifest's version, and the hashes of `plugin.wasm` and `manifest.json` on disk are the recorded ones.
fn is_verified(dir: &Path, manifest: &Manifest, trusted_keys: &[String]) -> bool {
    let path = dir.join(VERIFIED_FILE);
    if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_VERIFIED_BYTES) {
        return false;
    }
    let Some(record) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<VerifiedRecord>(&bytes).ok()) else {
        return false;
    };
    record.source == source_name(IndexSource::Official)
        && trusted_keys.contains(&record.key_id)
        && record.version == manifest.version
        && file_sha256(&dir.join("plugin.wasm")).is_ok_and(|h| h == record.wasm_sha256)
        && file_sha256(&dir.join("manifest.json")).is_ok_and(|h| h == record.manifest_sha256)
}

/// Writes `verified.json` for a package just installed in `dir` (atomically: a crash leaves none, so the plugin is
/// only shown as not verified).
fn write_verified(dir: &Path, package: &Package, verification: &Verification) -> Result<()> {
    let record = VerifiedRecord {
        sha256: verification.sha256.clone(),
        version: package.manifest.version.clone(),
        sequence: verification.sequence,
        key_id: verification.key_id.clone(),
        source: source_name(verification.source).to_string(),
        wasm_sha256: sha256_hex(&package.wasm),
        manifest_sha256: file_sha256(&dir.join("manifest.json"))?,
    };
    super::index::write_atomic(&dir.join(VERIFIED_FILE), &serde_json::to_vec_pretty(&record)?)?;
    Ok(())
}

/// `MAJOR.MINOR.PATCH` as numbers, so 1.10.0 is after 1.9.0.
fn version_key(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    let key = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(key)
}

impl ExternalPlugins {
    /// The plugins installed under `root`; "Verified" trusts the official index keys (and, in tests, the test key).
    pub fn new(root: PathBuf) -> Self {
        #[allow(unused_mut)]
        let mut trusted: Vec<String> = super::index::official_key_ids().into_iter().map(str::to_string).collect();
        #[cfg(test)]
        trusted.push(tests::test_key_id());
        Self::with_trusted_keys(root, trusted)
    }

    /// [`ExternalPlugins::new`] with `trusted_keys` as the ids of the keys whose verified installs count.
    fn with_trusted_keys(root: PathBuf, trusted_keys: Vec<String>) -> Self {
        let this = Self {
            root,
            list: Mutex::new(Vec::new()),
            dev: Mutex::new(Dev::default()),
            revoked: Mutex::new(Revocations::default()),
            trusted_keys,
        };
        this.reload();
        this
    }

    /// Reads the folder again.
    pub fn reload(&self) {
        let list = package::installed(&self.root)
            .into_iter()
            .map(|manifest| ExternalInfo { verified: is_verified(&self.dir(&manifest.id), &manifest, &self.trusted_keys), manifest, dev: false })
            .collect();
        *self.list.lock() = list;
    }

    /// The installed plugins, then the developer ones that are allowed (developer mode on).
    pub fn list(&self) -> Vec<ExternalInfo> {
        let mut all = self.list.lock().clone();
        all.extend(self.dev_rows().into_iter().filter_map(|row| match row.state {
            DevState::Ready(info) => Some(info),
            _ => None,
        }));
        all
    }

    /// The plugin at `index` of [`ExternalPlugins::list`] (what `Page::External` holds).
    pub fn get(&self, index: usize) -> Option<ExternalInfo> {
        self.list().into_iter().nth(index)
    }

    /// Where the plugin `id` is in [`ExternalPlugins::list`] now.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.list().iter().position(|i| i.manifest.id == id)
    }

    /// Developer mode: the folders to show while `enabled`. What the user allowed earlier in this session is kept.
    pub fn set_developer(&self, enabled: bool, folders: &[PathBuf]) {
        let mut dev = self.dev.lock();
        let mut old = std::mem::take(&mut dev.entries);
        dev.enabled = enabled;
        for folder in folders {
            let (mut granted, granted_id) = match old.iter_mut().find(|e| e.folder == *folder) {
                Some(e) => (e.granted.take(), std::mem::take(&mut e.granted_id)),
                None => (None, String::new()),
            };
            let loaded = read_dev_folder(folder).map_err(|e| format!("{e:#}"));
            // The folder now holds another plugin id: what was allowed was allowed for the old one.
            if loaded.as_ref().is_ok_and(|(m, _)| m.id != granted_id) {
                granted = None;
            }
            dev.entries.push(DevEntry { folder: folder.clone(), granted, granted_id, loaded });
        }
    }

    pub fn developer_enabled(&self) -> bool {
        self.dev.lock().enabled
    }

    /// The folders, as the settings keep them.
    pub fn dev_folders(&self) -> Vec<PathBuf> {
        self.dev.lock().entries.iter().map(|e| e.folder.clone()).collect()
    }

    /// Every developer folder with its state; empty when developer mode is off.
    pub fn dev_rows(&self) -> Vec<DevRow> {
        let dev = self.dev.lock();
        if !dev.enabled {
            return Vec::new();
        }
        let installed: Vec<String> = self.list.lock().iter().map(|i| i.manifest.id.clone()).collect();
        let mut seen: Vec<String> = Vec::new();
        dev.entries
            .iter()
            .map(|entry| {
                let state = match &entry.loaded {
                    Err(error) => DevState::Problem(error.clone()),
                    Ok((manifest, _)) if installed.contains(&manifest.id) => DevState::Problem(t("id already installed").to_string()),
                    Ok((manifest, _)) if seen.contains(&manifest.id) => DevState::Problem(t("id already used by another developer folder").to_string()),
                    Ok((manifest, _)) => {
                        seen.push(manifest.id.clone());
                        if entry.granted_id != manifest.id || needs_consent(entry.granted.as_deref(), &manifest.permissions) {
                            DevState::NeedsConsent(manifest.clone())
                        } else {
                            DevState::Ready(ExternalInfo { manifest: manifest.clone(), verified: false, dev: true })
                        }
                    }
                };
                DevRow { folder: entry.folder.clone(), state }
            })
            .collect()
    }

    /// Reads a developer folder and checks it, for the consent. Nothing is remembered yet. Blocking (disk).
    pub fn prepare_dev(&self, folder: &Path) -> Result<DevPending> {
        if !self.developer_enabled() {
            bail!("Developer mode is off");
        }
        let (manifest, wasm) = read_dev_folder(folder)?;
        self.check_free(folder, &manifest)?;
        let dev = self.dev.lock();
        let previous = dev.entries.iter().find(|e| e.folder == folder && e.granted_id == manifest.id).and_then(|e| e.granted.clone());
        Ok(DevPending { folder: folder.to_path_buf(), manifest, wasm, previous })
    }

    /// The id is not an installed plugin's and not another developer folder's.
    fn check_free(&self, folder: &Path, manifest: &Manifest) -> Result<()> {
        if self.list.lock().iter().any(|i| i.manifest.id == manifest.id) {
            bail!("A plugin with the id {} is already installed", manifest.id);
        }
        let dev = self.dev.lock();
        if dev.entries.iter().any(|e| e.folder != folder && e.loaded.as_ref().is_ok_and(|(m, _)| m.id == manifest.id)) {
            bail!("Another developer folder already holds a plugin with the id {}", manifest.id);
        }
        Ok(())
    }

    /// Remembers the user's consent for what [`ExternalPlugins::prepare_dev`] showed. The folder is read again, and
    /// when it changed meanwhile nothing is allowed.
    pub fn accept_dev(&self, pending: &DevPending) -> Result<()> {
        let (manifest, wasm) = read_dev_folder(&pending.folder)?;
        if manifest != pending.manifest || wasm != pending.wasm {
            bail!("The folder changed while you were deciding. Load it again.");
        }
        // `read_dev_folder` ran `Manifest::parse` and the built-in id check; the id must be free as well.
        self.check_free(&pending.folder, &manifest)?;
        let mut dev = self.dev.lock();
        if !dev.enabled {
            bail!("Developer mode is off");
        }
        let entry = DevEntry { folder: pending.folder.clone(), granted: Some(manifest.permissions.clone()), granted_id: manifest.id.clone(), loaded: Ok((manifest, wasm)) };
        match dev.entries.iter_mut().find(|e| e.folder == pending.folder) {
            Some(existing) => *existing = entry,
            None => dev.entries.push(entry),
        }
        Ok(())
    }

    /// The page's Reload: reads manifest.json and the wasm path again. Asks for consent again only when the
    /// permissions grew. When the folder cannot be read the previous version stays and the error is returned.
    pub fn reload_dev(&self, id: &str) -> Result<DevReload> {
        let mut dev = self.dev.lock();
        let entry = dev.entries.iter_mut().find(|e| e.loaded.as_ref().is_ok_and(|(m, _)| m.id == id)).context("This is not a developer plugin")?;
        let (manifest, wasm) = read_dev_folder(&entry.folder)?;
        if manifest.id != id {
            bail!("The plugin id in manifest.json changed: add the folder again.");
        }
        let ask = needs_consent(entry.granted.as_deref(), &manifest.permissions);
        if !ask {
            entry.granted = Some(manifest.permissions.clone());
        }
        entry.loaded = Ok((manifest, wasm));
        Ok(if ask { DevReload::NeedsConsent } else { DevReload::Reloaded })
    }

    /// Takes a folder off the list. The plugin's private data stays.
    pub fn forget_dev(&self, folder: &Path) {
        self.dev.lock().entries.retain(|e| e.folder != folder);
    }

    /// The `.wasm` a developer plugin runs.
    pub fn dev_wasm(&self, id: &str) -> Option<PathBuf> {
        let dev = self.dev.lock();
        dev.entries.iter().find_map(|e| e.loaded.as_ref().ok().filter(|(m, _)| m.id == id).map(|(_, wasm)| wasm.clone()))
    }

    /// A developer plugin's private folder (its `data/` is inside): `configuration/plugins/ext-dev/<id>/`, never in
    /// the author's source folder.
    pub fn dev_dir(&self, id: &str) -> PathBuf {
        self.root.with_file_name("ext-dev").join(id)
    }

    /// Where a plugin's `open-import-preview` files go: `configuration/plugins/ext-exports/<id>/`, outside the plugin's
    /// folder so an update or a removal keeps them (Ruling 18).
    pub fn exports_dir(&self, id: &str) -> PathBuf {
        self.root.with_file_name("ext-exports").join(id)
    }

    /// The installed folder of a plugin.
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Reads and checks a package file. Blocking: run it off the window's thread.
    pub fn prepare(&self, path: &Path) -> Result<Pending> {
        let bytes = read_capped(path)?;
        let package = package::inspect(&bytes, &reserved_ids())?;
        let previous = self.list.lock().iter().find(|i| i.manifest.id == package.manifest.id).map(|i| i.manifest.clone());
        Ok(Pending { package, previous, verified: None })
    }

    /// Checks a package downloaded for `entry` of the signed index, for the consent: its size and SHA-256 equal the
    /// entry's, the entry is the one this index lists and is not revoked by it, [`package::inspect`] passes and the
    /// manifest equals the entry ([`check_matches_entry`]). Anything else refuses the install. Blocking.
    pub fn prepare_verified(&self, signed: &SignedIndex, entry: &Entry, bytes: Vec<u8>) -> Result<Pending> {
        if signed.index.entry(&entry.id) != Some(entry) {
            bail!("This plugin is not listed by the index as shown");
        }
        if let Some(revoked) = signed.index.revocation_for(&entry.id, &entry.version) {
            bail!("This version was revoked by the index: {}", display_safe(&revoked.reason, 300));
        }
        // Official revocations apply to every index, the test one too (Ruling 8).
        if let Some(reason) = self.official_revocation(&entry.id, &entry.version) {
            bail!("This version was revoked by the official index: {reason}");
        }
        check_integrity(entry, &bytes)?;
        let package = package::inspect(&bytes, &reserved_ids())?;
        check_matches_entry(&package.manifest, entry)?;
        let verified = Verification { source: signed.source, key_id: signed.key_id.to_string(), sequence: signed.index.sequence, sha256: entry.sha256.clone() };
        let previous = self.list.lock().iter().find(|i| i.manifest.id == package.manifest.id).map(|i| i.manifest.clone());
        Ok(Pending { package, previous, verified: Some(verified) })
    }

    /// The update the index offers for the installed plugin `id`, if any (see [`ExternalPlugins::update_in`]), from
    /// the index that may be installed from now. Never installs anything: the user clicks, and consents again.
    pub fn update_for(&self, id: &str) -> Option<Entry> {
        super::index::index_for_install().and_then(|index| self.update_in(&index, id))
    }

    /// The entry of `index` that updates the installed (not developer) plugin `id`: the same id, the same author, a
    /// strictly greater version (compared as numbers) and not revoked.
    pub fn update_in(&self, index: &Index, id: &str) -> Option<Entry> {
        let installed = self.list.lock().iter().find(|i| i.manifest.id == id).map(|i| i.manifest.clone())?;
        offered_update(index, &installed).cloned()
    }

    /// Records the revocations to enforce in place of the ones recorded before, and returns the ids of the installed
    /// plugins they cover (the caller switches those off): `official`, those of the official revocation lock, always;
    /// `test`, those of the test index, only while `developer_mode` is on, and only in addition to the official ones.
    /// An expired index revokes too. A plugin no longer covered is unlocked but not switched on again: that stays the
    /// user's choice.
    pub fn set_revocations(&self, official: &[Revoked], test: &[Revoked], developer_mode: bool) -> Vec<String> {
        let test = if developer_mode { test.to_vec() } else { Vec::new() };
        *self.revoked.lock() = Revocations { official: official.to_vec(), test };
        self.revoked_installed()
    }

    /// The installed plugins (developer folders are not installed) the recorded revocations cover.
    pub fn revoked_installed(&self) -> Vec<String> {
        let installed: Vec<Manifest> = self.list.lock().iter().map(|i| i.manifest.clone()).collect();
        let revoked = self.revoked.lock();
        installed.into_iter().filter(|m| revoked.find(&m.id, &m.version).is_some()).map(|m| m.id).collect()
    }

    /// Why the installed plugin `id` is revoked, if the recorded revocations cover its installed version (an official
    /// revocation is named before a test one).
    pub fn revocation(&self, id: &str) -> Option<Revocation> {
        let version = self.list.lock().iter().find(|i| i.manifest.id == id).map(|i| i.manifest.version.clone())?;
        let revoked = self.revoked.lock();
        let (source, found) = revoked.find(id, &version)?;
        Some(Revocation { source, reason: display_safe(&found.reason, 300) })
    }

    /// The official revocation recorded for `version` of `id` (whatever index is installed from).
    fn official_revocation(&self, id: &str, version: &str) -> Option<String> {
        let revoked = self.revoked.lock();
        revoked.official.iter().find(|r| covers(r, id, version)).map(|r| display_safe(&r.reason, 300))
    }

    /// Whether the user may switch the plugin `id` on: not while it is revoked.
    pub fn may_switch_on(&self, id: &str) -> bool {
        self.revocation(id).is_none()
    }

    /// [`ExternalPlugins::set_revocations`], then `switch_off` with the on/off key ([`settings_id`]) of every plugin
    /// it covers.
    pub fn apply_revocations_with(&self, official: &[Revoked], test: &[Revoked], developer_mode: bool, mut switch_off: impl FnMut(&str)) {
        for id in self.set_revocations(official, test, developer_mode) {
            switch_off(&settings_id(&id));
        }
    }

    /// Writes a package the user agreed to. Blocking. A verified package is checked again against the index that may
    /// be installed from now (see [`ExternalPlugins::commit_against`]): the index may have changed while the user was
    /// deciding.
    pub fn commit(&self, pending: &Pending) -> Result<Manifest> {
        let current = if pending.verified.is_some() { super::index::signed_index_for_install() } else { None };
        self.commit_against(pending, current.as_ref())
    }

    /// [`ExternalPlugins::commit`] with `current` as the index that may be installed from now. A verified Pending is
    /// refused, with nothing written, when there is no such index (none, no longer current, or a test one with
    /// developer mode off), when it comes from another source or another key than the one checked, or when it revokes
    /// this version.
    fn commit_against(&self, pending: &Pending, current: Option<&SignedIndex>) -> Result<Manifest> {
        if let Some(verification) = &pending.verified {
            let Some(current) = current else {
                bail!("The plugin index cannot be used to install right now (it is out of date or not available). Try again after it is checked.");
            };
            let manifest = &pending.package.manifest;
            // The index must still list exactly these bytes (a delisted or replaced entry is not installed).
            let still_listed =
                current.index.entry(&manifest.id).is_some_and(|e| e.sha256 == verification.sha256 && e.version == manifest.version);
            if current.source != verification.source || current.key_id.as_str() != verification.key_id || !still_listed {
                bail!("The plugin index changed while you were deciding. Open the plugin again.");
            }
            if let Some(revoked) = current.index.revocation_for(&manifest.id, &manifest.version) {
                bail!("This version was revoked by the index: {}", display_safe(&revoked.reason, 300));
            }
            if let Some(reason) = self.official_revocation(&manifest.id, &manifest.version) {
                bail!("This version was revoked by the official index: {reason}");
            }
        }
        // The consent was given for a comparison with this version: if the installed one changed meanwhile, ask again.
        let now = self.list.lock().iter().find(|i| i.manifest.id == pending.package.manifest.id).map(|i| i.manifest.clone());
        if now != pending.previous {
            bail!("The installed version changed while you were deciding. Choose the file again.");
        }
        // An update replaces the folder: the running version lets go of its files first.
        wait_until_stopped(&pending.package.manifest.id)?;
        let result = match pending.author_changed() {
            // Another author: the old plugin's private data is set aside, not handed over.
            true => package::install_without_data(&pending.package, &self.root).map(|_| ()),
            false => package::install(&pending.package, &self.root).map(|_| ()),
        };
        if result.is_ok() {
            let dir = self.dir(&pending.package.manifest.id);
            match &pending.verified {
                // Not fatal: the plugin is installed, it is only shown as not verified.
                Some(verification) => {
                    let _ = write_verified(&dir, &pending.package, verification);
                }
                // The install replaced the folder, so there is none; removed anyway: a file is never verified.
                None => {
                    let _ = std::fs::remove_file(dir.join(VERIFIED_FILE));
                }
            }
        }
        self.reload();
        result?;
        Ok(pending.package.manifest.clone())
    }

    /// `prepare` + `commit` without asking: for callers that already have the consent.
    #[cfg(test)]
    pub fn install_file(&self, path: &Path) -> Result<Manifest> {
        let pending = self.prepare(path)?;
        self.commit(&pending)
    }

    /// Deletes the plugin with its private data: its folder, what an interrupted install left, data copies kept aside,
    /// and the private folder a developer version with this id used (`ext-dev/<id>/`). The files it handed to the
    /// import (`ext-exports/<id>/`) are the user's and stay (Ruling 18).
    pub fn remove(&self, id: &str) -> Result<()> {
        wait_until_stopped(id)?;
        let mut result = package::remove(id, &self.root);
        if result.is_ok() && valid_id(id) {
            let dev = self.dev_dir(id);
            if std::fs::symlink_metadata(&dev).is_ok() {
                result = std::fs::remove_dir_all(&dev).with_context(|| format!("Could not remove {}", dev.display()));
            }
        }
        self.reload();
        result
    }
}

/// Developer mode on or off (Settings): remembered, and applied at once.
pub fn set_developer_mode(on: bool) {
    let plugins = plugins();
    plugins.set_developer(on, &plugins.dev_folders());
    let mut settings = aio_core::InstallerSettings::load();
    settings.developer_mode = on;
    // A read-only configuration folder only means the choice is not remembered.
    let _ = settings.save();
    // A test index must not outlive developer mode.
    super::index::developer_settings_changed(&settings);
}

/// Switches the plugin with the on/off key `key` off, saving only when it was on.
fn switch_off(key: &str) {
    if super::is_enabled(key) {
        super::set_enabled(key, false);
    }
}

/// Enforces `official` (the official revocation lock) and `test` (the revocations of the test index, current or
/// expired, only in developer mode) on the installed plugins: each one they revoke is switched off, which closes its
/// page and stops its instance on the next frame (the milestone-1 stop path: `App::sync_external`), and its switch
/// stays locked while they revoke it. Called at startup (the lock alone), after the cache loads, after every refresh
/// and when the developer settings change.
pub fn apply_revocations(official: &[Revoked], test: &[Revoked]) {
    let plugins = plugins();
    plugins.apply_revocations_with(official, test, plugins.developer_enabled(), switch_off);
}

/// Switches off again every installed plugin the recorded revocations cover (after an install from a file).
pub fn enforce_revocations() {
    for id in plugins().revoked_installed() {
        switch_off(&settings_id(&id));
    }
}

/// Whether the on/off key `key` may not be switched on: an external plugin that is revoked.
pub fn switch_locked(key: &str) -> bool {
    key.strip_prefix("ext:").is_some_and(|id| !plugins().may_switch_on(id))
}

/// Remembers the developer folders (after one was added or taken off).
pub fn save_dev_folders() {
    let mut settings = aio_core::InstallerSettings::load();
    settings.developer_plugins = plugins().dev_folders().iter().map(|f| f.display().to_string()).collect();
    let _ = settings.save();
}

/// Permissions shown in a warning tone: private columns, and running as root.
pub fn is_sensitive(p: &Permission) -> bool {
    matches!(p, Permission::SqlReadPrivate(..) | Permission::SshRoot)
}

/// One plain sentence for a permission, translated. The wording is the host's ([`Permission::plain_words`], shared
/// with the `aio-plugin` tool); the app only translates it.
pub fn permission_text(p: &Permission) -> String {
    let words = p.plain_words();
    words.fill(t(words.english))
}

/// The sentences of [`Permission::plain_words`], marked for source/tools/i18n_keys.py (a test checks they match).
#[allow(dead_code)]
const PERMISSION_TEXTS: &[&str] = &[
    n("Run commands on the hotel's server (you approve each script)"),
    n("Run commands as root on the hotel's server (you approve each script)"),
    n("Read the table {tables} of the hotel database"),
    n("Read the tables {tables} of the hotel database"),
    n("Change the table {tables} of the hotel database (you approve each change)"),
    n("Change the tables {tables} of the hotel database (you approve each change)"),
    n("Read the private column {column} of the table {table} (it can hold passwords, addresses or tokens)"),
    n("Fetch pages from {host} on the internet"),
    n("Connect to a program on this computer, port {port} (for example G-Earth)"),
    n("Keep its own files in a private folder (always allowed)"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const PERMISSIONS: &[&str] = &[
        "ssh",
        "ssh:root",
        "sql:read:rooms",
        "sql:read:rooms,items",
        "sql:write:rooms",
        "sql:write:rooms,items",
        "sql:read-private:users.mail",
        "http:example.com",
        "tcp:localhost:9092",
        "files",
    ];

    fn aiop(id: &str, version: &str, permissions: &[&str]) -> Vec<u8> {
        aiop_by("Someone", id, version, permissions)
    }

    fn aiop_by(author: &str, id: &str, version: &str, permissions: &[&str]) -> Vec<u8> {
        let manifest = serde_json::json!({
            "id": id, "name": "Demo", "version": version, "author": author, "description": "A demo",
            "api": "1.0", "permissions": permissions, "page_title": "Demo",
        });
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.start_file("plugin.wasm", options).unwrap();
        zip.write_all(b"\0asm\x01\0\0\0").unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn every_permission_has_a_sentence() {
        for text in PERMISSIONS {
            let permission = Permission::parse(text).unwrap();
            let sentence = permission_text(&permission);
            assert!(!sentence.is_empty(), "{text}");
            assert!(!sentence.contains('{'), "{text}: placeholder left in {sentence}");
            assert_ne!(sentence, permission.to_string(), "{text} shows its raw string");
        }
        let write = permission_text(&Permission::parse("sql:write:rooms").unwrap());
        assert_eq!(write, "Change the table rooms of the hotel database (you approve each change)");
    }

    #[test]
    fn permission_texts_are_the_hosts_sentences() {
        // Every sentence the host can give is marked for translation, and every marked one is still used.
        let used: std::collections::BTreeSet<&str> = PERMISSIONS.iter().map(|s| Permission::parse(s).unwrap().plain_words().english).collect();
        let marked: std::collections::BTreeSet<&str> = PERMISSION_TEXTS.iter().copied().collect();
        assert_eq!(used, marked);
    }

    #[test]
    fn external_ids_are_prefixed_in_settings() {
        assert_eq!(settings_id("hello"), "ext:hello");
        for plugin in PLUGINS {
            // Even a third party whose id equals a built-in one cannot touch its switch.
            assert_ne!(settings_id(plugin.id), plugin.id);
            assert!(!PLUGINS.iter().any(|other| other.id == settings_id(plugin.id)));
        }
    }

    #[test]
    fn install_then_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        assert!(ext.list().is_empty());

        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, aiop("demo", "1.0.0", &["ssh"])).unwrap();
        let manifest = ext.install_file(&file).unwrap();
        assert_eq!(manifest.id, "demo");
        let list = ext.list();
        assert_eq!(list.len(), 1);
        assert!(!list[0].verified && !list[0].dev);
        assert!(dir.path().join("ext/demo/plugin.wasm").exists());

        // An update with a new permission is flagged against the installed one.
        std::fs::write(&file, aiop("demo", "1.1.0", &["ssh", "ssh:root"])).unwrap();
        let pending = ext.prepare(&file).unwrap();
        assert_eq!(pending.previous.as_ref().unwrap().version, "1.0.0");
        assert!(!pending.is_new_permission(&Permission::Ssh));
        assert!(pending.is_new_permission(&Permission::SshRoot));

        // A built-in id and garbage are refused in words.
        std::fs::write(&file, aiop("cms", "1.0.0", &[])).unwrap();
        assert!(ext.prepare(&file).unwrap_err().to_string().contains("built-in"));
        std::fs::write(&file, b"not a zip").unwrap();
        assert!(ext.prepare(&file).is_err());

        ext.remove("demo").unwrap();
        assert!(ext.list().is_empty());
        assert!(!dir.path().join("ext/demo").exists());
    }

    /// Every entry of `dir` (names), sorted.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn remove_leaves_nothing_and_a_reinstall_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        let ext = ExternalPlugins::new(root.clone());
        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, aiop("demo", "1.0.0", &[])).unwrap();
        ext.install_file(&file).unwrap();
        std::fs::write(root.join("demo/data/secret.txt"), b"mine").unwrap();
        // What a crash during an install leaves, a copy kept aside, a developer version's data and an export.
        for leftover in [".demo.partial/data", ".demo.old", ".demo.data-from-old", ".demo.data-from-partial-2"] {
            std::fs::create_dir_all(root.join(leftover)).unwrap();
            std::fs::write(root.join(leftover).join("x.txt"), b"left").unwrap();
        }
        std::fs::create_dir_all(ext.dev_dir("demo").join("data")).unwrap();
        std::fs::write(ext.dev_dir("demo").join("data/dev.txt"), b"dev").unwrap();
        std::fs::create_dir_all(ext.exports_dir("demo")).unwrap();
        std::fs::write(ext.exports_dir("demo").join("export.sql"), b"saved").unwrap();
        // Another plugin whose id starts the same is not touched.
        std::fs::create_dir_all(root.join(".demo-two.data-from-old")).unwrap();

        ext.remove("demo").unwrap();
        assert_eq!(entries(&root), vec![".demo-two.data-from-old".to_string()]);
        assert!(!ext.dev_dir("demo").exists());
        assert!(ext.exports_dir("demo").join("export.sql").exists(), "files handed to the import are the user's (Ruling 18)");

        ext.install_file(&file).unwrap();
        assert!(entries(&root.join("demo/data")).is_empty(), "a reinstall starts with empty data");
    }

    #[test]
    fn another_author_does_not_get_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        let ext = ExternalPlugins::new(root.clone());
        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, aiop("demo", "1.0.0", &[])).unwrap();
        ext.install_file(&file).unwrap();
        std::fs::write(root.join("demo/data/secret.txt"), b"mine").unwrap();

        // The same author: an update, the data comes along.
        std::fs::write(&file, aiop("demo", "1.1.0", &[])).unwrap();
        let pending = ext.prepare(&file).unwrap();
        assert!(!pending.author_changed());
        ext.commit(&pending).unwrap();
        assert_eq!(std::fs::read(root.join("demo/data/secret.txt")).unwrap(), b"mine");

        // Another author: a different plugin; the data is set aside, not handed over.
        std::fs::write(&file, aiop_by("Somebody else", "demo", "2.0.0", &[])).unwrap();
        let pending = ext.prepare(&file).unwrap();
        assert!(pending.author_changed());
        ext.commit(&pending).unwrap();
        assert_eq!(ext.list()[0].manifest.author, "Somebody else");
        assert!(entries(&root.join("demo/data")).is_empty(), "the new author starts with empty data");
        assert_eq!(std::fs::read(root.join(".demo.data-from-previous-author/secret.txt")).unwrap(), b"mine");
    }

    /// A developer folder: manifest.json plus a wasm file (any bytes: nothing is run here).
    fn dev_folder(dir: &Path, name: &str, id: &str, permissions: &[&str], dev_wasm: Option<&str>) -> PathBuf {
        let folder = dir.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let mut manifest = serde_json::json!({
            "id": id, "name": "Dev demo", "version": "0.1.0", "author": "Me", "description": "In development",
            "api": "1.0", "permissions": permissions, "page_title": "Demo",
        });
        if let Some(path) = dev_wasm {
            manifest["dev_wasm"] = path.into();
        }
        std::fs::write(folder.join("manifest.json"), manifest.to_string()).unwrap();
        let wasm = folder.join(dev_wasm.unwrap_or("plugin.wasm"));
        if let Some(parent) = wasm.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(wasm, b"\0asm\x01\0\0\0").unwrap();
        folder
    }

    fn allowed(ext: &ExternalPlugins, folder: &Path) {
        let pending = ext.prepare_dev(folder).unwrap();
        ext.accept_dev(&pending).unwrap();
    }

    #[test]
    fn dev_folder_loads_without_install() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let folder = dev_folder(dir.path(), "mine", "mine", &["ssh"], Some("target/wasm32-wasip2/release/mine.wasm"));
        ext.set_developer(true, std::slice::from_ref(&folder));
        // Until the user agrees, the folder is known but does not run.
        assert!(ext.list().is_empty());
        assert!(matches!(ext.dev_rows()[0].state, DevState::NeedsConsent(_)));
        let pending = ext.prepare_dev(&folder).unwrap();
        assert!(pending.previous.is_none());
        ext.accept_dev(&pending).unwrap();

        let list = ext.list();
        assert_eq!(list.len(), 1);
        assert!(list[0].dev && !list[0].verified);
        assert_eq!(list[0].manifest.id, "mine");
        assert!(ext.dev_wasm("mine").unwrap().ends_with("mine.wasm"));
        // Nothing is copied into ext/, and the private data is not in the author's folder.
        assert!(!dir.path().join("ext").exists() || std::fs::read_dir(dir.path().join("ext")).unwrap().next().is_none());
        assert_eq!(ext.dev_dir("mine"), dir.path().join("ext-dev/mine"));
        assert!(!folder.join("data").exists());
    }

    #[test]
    fn dev_mode_off_ignores_folders() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let folder = dev_folder(dir.path(), "mine", "mine", &[], None);
        ext.set_developer(true, std::slice::from_ref(&folder));
        allowed(&ext, &folder);
        assert_eq!(ext.list().len(), 1);
        ext.set_developer(false, std::slice::from_ref(&folder));
        assert!(ext.list().is_empty() && ext.dev_rows().is_empty());
        assert!(ext.prepare_dev(&folder).is_err());
        // Switched on again, the folder is back and was agreed to already.
        ext.set_developer(true, &[folder]);
        assert_eq!(ext.list().len(), 1);
    }

    #[test]
    fn dev_wasm_outside_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        ext.set_developer(true, &[]);
        std::fs::write(dir.path().join("evil.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let up = dev_folder(dir.path(), "up", "up-plugin", &[], Some("../evil.wasm"));
        let error = ext.prepare_dev(&up).unwrap_err().to_string();
        assert!(error.contains("inside the folder"), "{error}");
        let absolute = dev_folder(dir.path(), "abs", "abs-plugin", &[], Some(&dir.path().join("evil.wasm").display().to_string()));
        assert!(ext.prepare_dev(&absolute).is_err());
        let missing = dev_folder(dir.path(), "none", "none-plugin", &[], None);
        std::fs::remove_file(missing.join("plugin.wasm")).unwrap();
        assert!(ext.prepare_dev(&missing).is_err());
        // The same validation as a package: a built-in id and a bad manifest are refused.
        let builtin = dev_folder(dir.path(), "cms", "cms", &[], None);
        assert!(ext.prepare_dev(&builtin).unwrap_err().to_string().contains("built-in"));
    }

    #[test]
    fn installed_id_wins_over_a_dev_folder() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, aiop("demo", "1.0.0", &[])).unwrap();
        ext.install_file(&file).unwrap();
        let folder = dev_folder(dir.path(), "demo-dev", "demo", &[], None);
        ext.set_developer(true, std::slice::from_ref(&folder));
        assert!(ext.prepare_dev(&folder).is_err());
        assert_eq!(ext.list().len(), 1);
        assert!(!ext.list()[0].dev);
        assert!(matches!(&ext.dev_rows()[0].state, DevState::Problem(text) if text.contains("already installed")));
    }

    #[test]
    fn reload_with_new_permissions_asks_again() {
        let ssh = Permission::Ssh;
        let root = Permission::SshRoot;
        assert!(needs_consent(None, &[]));
        assert!(!needs_consent(Some(&[ssh.clone(), root.clone()]), &[ssh.clone(), root.clone()]));
        assert!(!needs_consent(Some(&[ssh.clone(), root.clone()]), std::slice::from_ref(&ssh)), "fewer permissions: no question");
        assert!(needs_consent(Some(std::slice::from_ref(&ssh)), &[ssh.clone(), root.clone()]), "a new permission: ask");

        // End to end: the page's Reload.
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let folder = dev_folder(dir.path(), "mine", "mine", &["ssh"], None);
        ext.set_developer(true, std::slice::from_ref(&folder));
        allowed(&ext, &folder);
        assert_eq!(ext.reload_dev("mine").unwrap(), DevReload::Reloaded);

        dev_folder(dir.path(), "mine", "mine", &["ssh", "ssh:root"], None);
        assert_eq!(ext.reload_dev("mine").unwrap(), DevReload::NeedsConsent);
        assert!(ext.list().is_empty(), "it does not run until the user agrees");
        let pending = ext.prepare_dev(&folder).unwrap();
        assert!(pending.is_new_permission(&Permission::SshRoot) && !pending.is_new_permission(&Permission::Ssh));
        ext.accept_dev(&pending).unwrap();
        assert_eq!(ext.list().len(), 1);

        // Fewer permissions: no question, and the smaller set is what was agreed to.
        dev_folder(dir.path(), "mine", "mine", &["ssh"], None);
        assert_eq!(ext.reload_dev("mine").unwrap(), DevReload::Reloaded);
        dev_folder(dir.path(), "mine", "mine", &["ssh", "ssh:root"], None);
        assert_eq!(ext.reload_dev("mine").unwrap(), DevReload::NeedsConsent);

        // A broken manifest keeps what ran before and says why.
        std::fs::write(folder.join("manifest.json"), b"{").unwrap();
        assert!(ext.reload_dev("mine").is_err());
    }

    // ---- Verified installs from the signed index ----

    use crate::plugins::index::tests::{public, Server, SECRET};

    /// The id of the key the test indexes are signed with ([`ExternalPlugins::new`] trusts it in tests).
    pub(super) fn test_key_id() -> String {
        public(&SECRET).split_once(':').unwrap().0.to_string()
    }

    fn aiop_api(author: &str, id: &str, version: &str, api: &str, permissions: &[&str]) -> Vec<u8> {
        let manifest = serde_json::json!({
            "id": id, "name": "Demo", "version": version, "author": author, "description": "A demo",
            "api": api, "permissions": permissions, "page_title": "Demo",
        });
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.start_file("plugin.wasm", options).unwrap();
        zip.write_all(format!("\0asm\x01\0\0\0{id}{version}").as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// An index entry for `bytes` saying `id`, `version`, `author`, api 1.0 and `permissions`.
    fn entry_json(prefix: &str, bytes: &[u8], id: &str, version: &str, author: &str, permissions: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": "Demo", "version": version, "author": author, "description": "A demo",
            "api": "1.0", "permissions": permissions, "size": bytes.len(), "sha256": hex(bytes),
            "url": format!("{prefix}{id}-{version}/{id}.aiop"),
            "source": { "repo": "https://github.com/someone/demo", "commit": "0123456789abcdef0123456789abcdef01234567" }
        })
    }

    /// An index of sequence 7 signed with the test key, verified as an index of `source` would be.
    fn signed_index(prefix: &str, source: IndexSource, plugins: Vec<serde_json::Value>, revoked: serde_json::Value) -> SignedIndex {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema": 1, "sequence": 7, "generated": "2026-11-13T10:00:00Z", "expires": "2026-12-13T10:00:00Z",
            "plugins": plugins, "revoked": revoked,
        }))
        .unwrap();
        let sig = aio_plugin_index::sign(&bytes, &SECRET);
        let key = public(&SECRET);
        let (id, b64) = key.split_once(':').unwrap();
        let (index, key_id) = aio_plugin_index::verify_and_parse(&bytes, &sig, &[(id, b64)], prefix).unwrap();
        SignedIndex { index, source, key_id }
    }

    /// One package and the official index that lists it exactly.
    fn official(id: &str, version: &str, permissions: &[&str]) -> (Vec<u8>, SignedIndex) {
        let bytes = aiop_api("Someone", id, version, "1.0", permissions);
        let entry = entry_json(OFFICIAL_PACKAGE_PREFIX, &bytes, id, version, "Someone", permissions);
        (bytes, signed_index(OFFICIAL_PACKAGE_PREFIX, IndexSource::Official, vec![entry], serde_json::json!([])))
    }

    fn package_path(id: &str, version: &str) -> String {
        format!("/packages/{id}-{version}/{id}.aiop")
    }

    #[tokio::test]
    async fn tampered_package_is_refused() {
        let server = Server::start().await;
        let prefix = format!("http://{}/packages/", server.addr);
        let bytes = aiop("demo", "1.0.0", &["ssh"]);
        let signed = signed_index(&prefix, IndexSource::Test, vec![entry_json(&prefix, &bytes, "demo", "1.0.0", "Someone", &["ssh"])], serde_json::json!([]));
        let entry = signed.index.entry("demo").unwrap().clone();
        let client = package_client(IndexSource::Test);

        server.set(&package_path("demo", "1.0.0"), 200, bytes.clone());
        assert_eq!(download(&client, &entry, IndexSource::Test).await.unwrap(), bytes);

        // One byte changed, same size: the SHA-256 differs.
        let mut tampered = bytes.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        server.set(&package_path("demo", "1.0.0"), 200, tampered.clone());
        let error = download(&client, &entry, IndexSource::Test).await.unwrap_err().to_string();
        assert!(error.contains("SHA-256"), "{error}");

        // The same check when the bytes come from elsewhere.
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        assert!(ext.prepare_verified(&signed, &entry, tampered).is_err());
        // An entry that is not the one this index signed (another sha256) is refused.
        let forged = Entry { sha256: hex(b"other"), ..entry.clone() };
        assert!(ext.prepare_verified(&signed, &forged, bytes.clone()).is_err());
        assert!(ext.prepare_verified(&signed, &entry, bytes.clone()).is_ok());
        assert!(ext.list().is_empty(), "prepare writes nothing");

        // A redirect on the test host is followed; one to another port is not; an error status is refused.
        server.set("/elsewhere/demo.aiop", 200, bytes.clone());
        server.routes.lock().insert(package_path("demo", "1.0.0"), (302, vec![("Location".into(), "/elsewhere/demo.aiop".into())], vec![]));
        assert_eq!(download(&client, &entry, IndexSource::Test).await.unwrap(), bytes);
        server.routes.lock().insert(package_path("demo", "1.0.0"), (302, vec![("Location".into(), "http://127.0.0.1:1/demo.aiop".into())], vec![]));
        assert!(download(&client, &entry, IndexSource::Test).await.is_err());
        server.set(&package_path("demo", "1.0.0"), 404, vec![]);
        assert!(download(&client, &entry, IndexSource::Test).await.is_err());
    }

    #[tokio::test]
    async fn oversized_download_is_refused() {
        let server = Server::start().await;
        let prefix = format!("http://{}/packages/", server.addr);
        let bytes = aiop("demo", "1.0.0", &[]);
        let signed = signed_index(&prefix, IndexSource::Test, vec![entry_json(&prefix, &bytes, "demo", "1.0.0", "Someone", &[])], serde_json::json!([]));
        let entry = signed.index.entry("demo").unwrap().clone();
        let client = package_client(IndexSource::Test);

        // Longer than the entry says (the first bytes are the right ones).
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[0; 4096]);
        server.set(&package_path("demo", "1.0.0"), 200, longer.clone());
        let error = download(&client, &entry, IndexSource::Test).await.unwrap_err().to_string();
        assert!(error.contains("larger"), "{error}");
        // Shorter than the entry says.
        server.set(&package_path("demo", "1.0.0"), 200, bytes[..bytes.len() - 1].to_vec());
        assert!(download(&client, &entry, IndexSource::Test).await.is_err());
        // Far past 32 MiB: refused while streaming.
        server.set(&package_path("demo", "1.0.0"), 200, vec![0; MAX_PACKAGE + 1]);
        assert!(download(&client, &entry, IndexSource::Test).await.is_err());

        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let error = ext.prepare_verified(&signed, &entry, longer).unwrap_err().to_string();
        assert!(error.contains("size"), "{error}");

        // An official entry is only fetched from the official release prefix.
        assert!(download(&client, &entry, IndexSource::Official).await.is_err());
    }

    #[test]
    fn manifest_must_equal_entry() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let p = OFFICIAL_PACKAGE_PREFIX;
        let refused = |bytes: Vec<u8>, entry: serde_json::Value| {
            let signed = signed_index(p, IndexSource::Official, vec![entry], serde_json::json!([]));
            let entry = signed.index.plugins[0].clone();
            ext.prepare_verified(&signed, &entry, bytes).unwrap_err().to_string()
        };

        // An extra permission in the package that the index does not list.
        let bytes = aiop_api("Someone", "demo", "1.0.0", "1.0", &["ssh", "ssh:root"]);
        let error = refused(bytes.clone(), entry_json(p, &bytes, "demo", "1.0.0", "Someone", &["ssh"]));
        assert!(error.contains("permissions"), "{error}");
        // One fewer.
        let error = refused(bytes.clone(), entry_json(p, &bytes, "demo", "1.0.0", "Someone", &["ssh", "ssh:root", "files"]));
        assert!(error.contains("permissions"), "{error}");
        // Another author.
        let error = refused(bytes.clone(), entry_json(p, &bytes, "demo", "1.0.0", "Somebody else", &["ssh", "ssh:root"]));
        assert!(error.contains("author"), "{error}");
        // Another version.
        let error = refused(bytes.clone(), entry_json(p, &bytes, "demo", "1.0.1", "Someone", &["ssh", "ssh:root"]));
        assert!(error.contains("version"), "{error}");
        // Another id.
        let error = refused(bytes.clone(), entry_json(p, &bytes, "other", "1.0.0", "Someone", &["ssh", "ssh:root"]));
        assert!(error.contains("id"), "{error}");
        // Another api.
        let api = aiop_api("Someone", "demo", "1.0.0", "1.0.0", &["ssh", "ssh:root"]);
        let error = refused(api.clone(), entry_json(p, &api, "demo", "1.0.0", "Someone", &["ssh", "ssh:root"]));
        assert!(error.contains("api"), "{error}");

        // Equal: accepted, marked verified, nothing written yet.
        let (bytes, signed) = official("demo", "1.0.0", &["ssh"]);
        let pending = ext.prepare_verified(&signed, &signed.index.plugins[0], bytes.clone()).unwrap();
        let verification = pending.verification().unwrap();
        assert_eq!((verification.source, verification.sequence), (IndexSource::Official, 7));
        assert_eq!(verification.key_id, signed.key_id.to_string());
        assert_eq!(verification.sha256, hex(&bytes));
        assert!(ext.list().is_empty());
        // A built-in id is refused like a file's.
        let (bytes, signed) = official("cms", "1.0.0", &[]);
        assert!(ext.prepare_verified(&signed, &signed.index.plugins[0], bytes).unwrap_err().to_string().contains("built-in"));
    }

    #[test]
    fn permission_order_does_not_matter() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let p = OFFICIAL_PACKAGE_PREFIX;
        let bytes = aiop_api("Someone", "demo", "1.0.0", "1.0", &["ssh", "sql:read:rooms", "http:example.com"]);
        let signed = signed_index(p, IndexSource::Official, vec![entry_json(p, &bytes, "demo", "1.0.0", "Someone", &["http:example.com", "ssh", "sql:read:rooms"])], serde_json::json!([]));
        assert!(ext.prepare_verified(&signed, &signed.index.plugins[0], bytes).is_ok());

        // The counts must match too: a repeat never makes up for a missing one (an entry built by hand, since the
        // index itself refuses repeats).
        let manifest = Manifest::parse(br#"{"id":"demo","name":"D","version":"1.0.0","author":"A","description":"","api":"1.0","permissions":["ssh","ssh:root"],"page_title":"D"}"#).unwrap();
        let mut entry = signed.index.plugins[0].clone();
        (entry.id, entry.version, entry.author, entry.api) = ("demo".into(), "1.0.0".into(), "A".into(), "1.0".into());
        entry.permissions = vec!["ssh:root".into(), "ssh".into()];
        assert!(check_matches_entry(&manifest, &entry).is_ok());
        for wrong in [vec!["ssh", "ssh"], vec!["ssh", "ssh:root", "ssh"], vec!["ssh"], vec!["ssh", "files"]] {
            entry.permissions = wrong.iter().map(|s| s.to_string()).collect();
            assert!(check_matches_entry(&manifest, &entry).is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn revoked_entry_cannot_be_installed() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let p = OFFICIAL_PACKAGE_PREFIX;
        let bytes = aiop("demo", "1.0.0", &[]);
        for versions in [serde_json::json!(["1.0.0"]), serde_json::json!(["*"])] {
            let revoked = serde_json::json!([{ "id": "demo", "versions": versions, "reason": "It deletes rooms" }]);
            let signed = signed_index(p, IndexSource::Official, vec![entry_json(p, &bytes, "demo", "1.0.0", "Someone", &[])], revoked);
            let error = ext.prepare_verified(&signed, &signed.index.plugins[0], bytes.clone()).unwrap_err().to_string();
            assert!(error.contains("revoked") && error.contains("It deletes rooms"), "{error}");
        }
        // Another version revoked: this one installs.
        let revoked = serde_json::json!([{ "id": "demo", "versions": ["0.9.0"], "reason": "Old" }]);
        let signed = signed_index(p, IndexSource::Official, vec![entry_json(p, &bytes, "demo", "1.0.0", "Someone", &[])], revoked);
        assert!(ext.prepare_verified(&signed, &signed.index.plugins[0], bytes).is_ok());
    }

    fn verified_record(root: &Path, id: &str) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(root.join(id).join(VERIFIED_FILE)).unwrap()).unwrap()
    }

    #[test]
    fn verified_flag_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        let ext = ExternalPlugins::new(root.clone());
        let (bytes, signed) = official("demo", "1.0.0", &["ssh"]);
        let pending = ext.prepare_verified(&signed, &signed.index.plugins[0], bytes.clone()).unwrap();
        ext.commit_against(&pending, Some(&signed)).unwrap();
        assert!(ext.list()[0].verified);
        let record = verified_record(&root, "demo");
        assert_eq!(record["sha256"], hex(&bytes));
        assert_eq!(record["version"], "1.0.0");
        assert_eq!(record["sequence"], 7);
        assert_eq!(record["key_id"], signed.key_id.to_string());
        assert!(!root.join("demo").join(format!("{VERIFIED_FILE}.tmp")).exists());

        // A restart reads it again.
        let restarted = ExternalPlugins::new(root.clone());
        assert!(restarted.list()[0].verified);
        // A release that no longer trusts the key (rotated away from a stolen key): no longer verified.
        let rotated = ExternalPlugins::with_trusted_keys(root.clone(), vec!["0000000000000000".into()]);
        assert!(!rotated.list()[0].verified);

        // plugin.wasm changed on disk: no longer verified.
        let wasm = root.join("demo/plugin.wasm");
        let original = std::fs::read(&wasm).unwrap();
        std::fs::write(&wasm, b"\0asm\x01\0\0\0changed").unwrap();
        ext.reload();
        assert!(!ext.list()[0].verified);
        std::fs::write(&wasm, &original).unwrap();
        ext.reload();
        assert!(ext.list()[0].verified);
        // manifest.json changed (a permission added by hand): no longer verified.
        let manifest = root.join("demo/manifest.json");
        let saved = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, saved.replace("\"ssh\"", "\"ssh:root\"")).unwrap();
        ext.reload();
        assert_eq!(ext.list()[0].manifest.permissions, vec![Permission::SshRoot]);
        assert!(!ext.list()[0].verified);
        std::fs::write(&manifest, &saved).unwrap();
        // A record that does not match, or does not parse, or is missing.
        let path = root.join("demo").join(VERIFIED_FILE);
        let good = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, good.replace(&hex(&original), &hex(b"x"))).unwrap();
        ext.reload();
        assert!(!ext.list()[0].verified);
        std::fs::write(&path, b"{").unwrap();
        ext.reload();
        assert!(!ext.list()[0].verified);
        std::fs::write(&path, &good).unwrap();
        ext.reload();
        assert!(ext.list()[0].verified);

        // An install from a test index is recorded as such and never shown as verified.
        let bytes = aiop("other", "1.0.0", &[]);
        let test = signed_index(OFFICIAL_PACKAGE_PREFIX, IndexSource::Test, vec![entry_json(OFFICIAL_PACKAGE_PREFIX, &bytes, "other", "1.0.0", "Someone", &[])], serde_json::json!([]));
        let pending = ext.prepare_verified(&test, &test.index.plugins[0], bytes).unwrap();
        assert_eq!(pending.verification().unwrap().source, IndexSource::Test);
        ext.commit_against(&pending, Some(&test)).unwrap();
        assert_eq!(verified_record(&root, "other")["source"], "test");
        let other = ext.list().into_iter().find(|i| i.manifest.id == "other").unwrap();
        assert!(!other.verified);
    }

    #[test]
    fn file_install_clears_verified() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        let ext = ExternalPlugins::new(root.clone());
        let (bytes, signed) = official("demo", "1.0.0", &[]);
        ext.commit_against(&ext.prepare_verified(&signed, &signed.index.plugins[0], bytes.clone()).unwrap(), Some(&signed)).unwrap();
        assert!(ext.list()[0].verified);

        // The very same package, from a file: not verified any more, and the record is gone.
        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, &bytes).unwrap();
        let pending = ext.prepare(&file).unwrap();
        assert!(pending.verification().is_none());
        ext.commit(&pending).unwrap();
        assert!(!ext.list()[0].verified);
        assert!(!root.join("demo").join(VERIFIED_FILE).exists());

        // A record left by hand next to a file install does not make it verified either (its hashes must match).
        ext.commit_against(&ext.prepare_verified(&signed, &signed.index.plugins[0], bytes).unwrap(), Some(&signed)).unwrap();
        let record = std::fs::read(root.join("demo").join(VERIFIED_FILE)).unwrap();
        std::fs::write(&file, aiop("demo", "1.0.1", &[])).unwrap();
        ext.install_file(&file).unwrap();
        assert!(!root.join("demo").join(VERIFIED_FILE).exists());
        std::fs::write(root.join("demo").join(VERIFIED_FILE), record).unwrap();
        ext.reload();
        assert!(!ext.list()[0].verified);
    }

    #[test]
    fn update_detected_only_for_newer_same_author() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, aiop("demo", "1.9.0", &["ssh"])).unwrap();
        ext.install_file(&file).unwrap();
        let p = OFFICIAL_PACKAGE_PREFIX;
        let index = |version: &str, author: &str, revoked: serde_json::Value| {
            let bytes = aiop_by(author, "demo", version, &["ssh"]);
            signed_index(p, IndexSource::Official, vec![entry_json(p, &bytes, "demo", version, author, &["ssh"])], revoked).index
        };
        let none = serde_json::json!([]);

        // 1.10.0 is newer than 1.9.0 (numbers, not text).
        let found = ext.update_in(&index("1.10.0", "Someone", none.clone()), "demo").unwrap();
        assert_eq!(found.version, "1.10.0");
        assert_eq!(ext.list()[0].manifest.version, "1.9.0", "never installed by itself");
        assert!(ext.update_in(&index("2.0.0", "Someone", none.clone()), "demo").is_some());
        // The same, an older, another author's, a revoked one: no update.
        assert!(ext.update_in(&index("1.9.0", "Someone", none.clone()), "demo").is_none());
        assert!(ext.update_in(&index("1.8.12", "Someone", none.clone()), "demo").is_none());
        assert!(ext.update_in(&index("2.0.0", "Somebody else", none.clone()), "demo").is_none());
        let revoked = serde_json::json!([{ "id": "demo", "versions": ["2.0.0"], "reason": "Broken" }]);
        assert!(ext.update_in(&index("2.0.0", "Someone", revoked), "demo").is_none());
        // Not installed, or not listed.
        assert!(ext.update_in(&index("2.0.0", "Someone", none.clone()), "other").is_none());

        // A verified install updates the same way.
        let (bytes, signed) = official("demo", "2.0.0", &["ssh"]);
        ext.commit_against(&ext.prepare_verified(&signed, &signed.index.plugins[0], bytes).unwrap(), Some(&signed)).unwrap();
        assert!(ext.list()[0].verified);
        assert!(ext.update_in(&index("2.0.1", "Someone", none.clone()), "demo").is_some());
        assert!(ext.update_in(&index("2.0.0", "Someone", none), "demo").is_none());
    }

    #[test]
    fn revoked_while_consent_open_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        let ext = ExternalPlugins::new(root.clone());
        let p = OFFICIAL_PACKAGE_PREFIX;
        let bytes = aiop("demo", "1.0.0", &[]);
        let listed = || vec![entry_json(p, &bytes, "demo", "1.0.0", "Someone", &[])];
        let signed = signed_index(p, IndexSource::Official, listed(), serde_json::json!([]));
        let pending = ext.prepare_verified(&signed, &signed.index.plugins[0], bytes.clone()).unwrap();
        let nothing_installed = |ext: &ExternalPlugins| assert!(ext.list().is_empty() && !root.join("demo").exists());

        // Meanwhile the refresh brought a revocation of this version (or of every version).
        for versions in [serde_json::json!(["1.0.0"]), serde_json::json!(["*"])] {
            let revoked = serde_json::json!([{ "id": "demo", "versions": versions, "reason": "It deletes rooms" }]);
            let now = signed_index(p, IndexSource::Official, listed(), revoked);
            let error = ext.commit_against(&pending, Some(&now)).unwrap_err().to_string();
            assert!(error.contains("revoked") && error.contains("It deletes rooms"), "{error}");
            nothing_installed(&ext);
        }
        // No index that may be installed from (expired, gone, test index with developer mode off).
        assert!(ext.commit_against(&pending, None).is_err());
        nothing_installed(&ext);
        // Another source or another key than the one checked.
        let test = SignedIndex { source: IndexSource::Test, ..signed.clone() };
        assert!(ext.commit_against(&pending, Some(&test)).is_err());
        let other_key = SignedIndex { key_id: aio_plugin_index::key_id(&[1; 32]), ..signed.clone() };
        assert!(ext.commit_against(&pending, Some(&other_key)).is_err());
        nothing_installed(&ext);
        // The index no longer lists it (delisted, not revoked), or lists other bytes under this version.
        let delisted = signed_index(p, IndexSource::Official, vec![], serde_json::json!([]));
        assert!(ext.commit_against(&pending, Some(&delisted)).unwrap_err().to_string().contains("changed"));
        let other_bytes = aiop("demo", "1.0.0", &["ssh"]);
        let replaced = signed_index(p, IndexSource::Official, vec![entry_json(p, &other_bytes, "demo", "1.0.0", "Someone", &["ssh"])], serde_json::json!([]));
        assert!(ext.commit_against(&pending, Some(&replaced)).is_err());
        nothing_installed(&ext);

        // Another version revoked: it installs, verified. A file install never needs the index.
        let other = signed_index(p, IndexSource::Official, listed(), serde_json::json!([{ "id": "demo", "versions": ["0.9.0"], "reason": "Old" }]));
        ext.commit_against(&pending, Some(&other)).unwrap();
        assert!(ext.list()[0].verified);
        let file = dir.path().join("demo.aiop");
        std::fs::write(&file, aiop("demo", "1.0.1", &[])).unwrap();
        ext.commit(&ext.prepare(&file).unwrap()).unwrap();
    }

    #[test]
    fn redirect_to_github_cdn_allowed_other_refused() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        let first = url("https://github.com/all-in-1-plugins/index/releases/download/demo-1.0.0/demo.aiop");
        let official = |next: &str| redirect_allowed(IndexSource::Official, &first, &url(next));
        assert!(official("https://objects.githubusercontent.com/github-production-release-asset/1?x=y"));
        assert!(official("https://release-assets.githubusercontent.com/github-production-release-asset/1"));
        assert!(official("https://OBJECTS.githubusercontent.com/a"), "host case does not matter");
        for refused in [
            "http://objects.githubusercontent.com/a",
            "https://objects.githubusercontent.com:8443/a",
            "https://user:pw@objects.githubusercontent.com/a",
            "https://objects.githubusercontent.com.evil.example/a",
            "https://evil.example/objects.githubusercontent.com",
            "https://raw.githubusercontent.com/a",
            "https://github.com/someone-else/x/releases/download/a/b.aiop",
            "https://example.com/demo.aiop",
        ] {
            assert!(!official(refused), "{refused}");
        }

        // A test index: its own host and port only, never down to http.
        let first = url("http://127.0.0.1:8080/packages/demo-1.0.0/demo.aiop");
        let test = |next: &str| redirect_allowed(IndexSource::Test, &first, &url(next));
        assert!(test("http://127.0.0.1:8080/elsewhere/demo.aiop"));
        assert!(!test("http://127.0.0.1:8081/demo.aiop"));
        assert!(!test("http://localhost:8080/demo.aiop"));
        assert!(!test("https://objects.githubusercontent.com/a"), "the GitHub CDN is for the official index only");
        let first = url("https://test.example/packages/demo-1.0.0/demo.aiop");
        assert!(redirect_allowed(IndexSource::Test, &first, &url("https://test.example/x")));
        assert!(!redirect_allowed(IndexSource::Test, &first, &url("http://test.example/x")));
        assert!(!redirect_allowed(IndexSource::Test, &first, &url("https://other.example/x")));
    }

    #[test]
    fn dev_id_change_needs_consent_again() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let folder = dev_folder(dir.path(), "mine", "first", &["ssh"], None);
        ext.set_developer(true, std::slice::from_ref(&folder));
        allowed(&ext, &folder);
        assert_eq!(ext.list().len(), 1);

        // Same permissions, another id: it is another plugin.
        dev_folder(dir.path(), "mine", "second", &["ssh"], None);
        ext.set_developer(false, std::slice::from_ref(&folder));
        ext.set_developer(true, std::slice::from_ref(&folder));
        assert!(ext.list().is_empty(), "mode off/on must not carry the consent to a new id");
        assert!(matches!(ext.dev_rows()[0].state, DevState::NeedsConsent(_)));
        // "Check again" is the same call.
        ext.set_developer(true, &ext.dev_folders());
        assert!(ext.list().is_empty());
        assert!(ext.prepare_dev(&folder).unwrap().previous.is_none());

        // Going back to the first id does not revive the old consent either.
        dev_folder(dir.path(), "mine", "first", &["ssh"], None);
        ext.set_developer(true, &ext.dev_folders());
        assert!(ext.list().is_empty());
        let pending = ext.prepare_dev(&folder).unwrap();
        ext.accept_dev(&pending).unwrap();
        assert_eq!(ext.list()[0].manifest.id, "first");
    }

    /// An index (no entries) that revokes `revoked`, published on `generated` and expiring on `expires`.
    fn revoking(revoked: &[(&str, &[&str], &str)], expires: &str) -> Index {
        Index {
            schema: 1,
            sequence: 9,
            generated: "2026-10-01T10:00:00Z".parse().unwrap(),
            expires: expires.parse().unwrap(),
            plugins: Vec::new(),
            revoked: revoked
                .iter()
                .map(|(id, versions, reason)| Revoked { id: id.to_string(), versions: versions.iter().map(|v| v.to_string()).collect(), reason: reason.to_string() })
                .collect(),
        }
    }

    /// Two installed plugins, demo 1.0.0 and other 2.0.0.
    fn two_installed(dir: &Path) -> ExternalPlugins {
        let ext = ExternalPlugins::new(dir.join("ext"));
        let file = dir.join("p.aiop");
        std::fs::write(&file, aiop("demo", "1.0.0", &["ssh"])).unwrap();
        ext.install_file(&file).unwrap();
        std::fs::write(&file, aiop("other", "2.0.0", &[])).unwrap();
        ext.install_file(&file).unwrap();
        ext
    }

    #[test]
    fn revoked_plugin_is_switched_off_and_locked() {
        let dir = tempfile::tempdir().unwrap();
        let ext = two_installed(dir.path());
        let index = revoking(&[("demo", &["1.0.0"], "Steals\u{202e} tokens"), ("other", &["1.0.0"], "Only the old one")], "2026-12-30T10:00:00Z");
        let mut switched_off = Vec::new();
        ext.apply_revocations_with(&index.revoked, &[], false, |key| switched_off.push(key.to_string()));
        // Only the installed version is covered: other 2.0.0 is not revoked.
        assert_eq!(switched_off, vec!["ext:demo".to_string()]);
        let revocation = ext.revocation("demo").unwrap();
        assert_eq!(revocation.source, IndexSource::Official);
        assert_eq!(revocation.reason, display_safe("Steals\u{202e} tokens", 300), "the reason is shown safe");
        assert!(!ext.may_switch_on("demo"), "locked while revoked");
        assert!(ext.may_switch_on("other"));
        // "*" covers every version.
        ext.set_revocations(&revoking(&[("other", &["*"], "All of it")], "2026-12-30T10:00:00Z").revoked, &[], false);
        assert_eq!(ext.revoked_installed(), vec!["other".to_string()]);
        // A test index revokes only while developer mode is on.
        assert!(ext.set_revocations(&[], &index.revoked, false).is_empty());
        assert!(ext.may_switch_on("demo"));
        assert_eq!(ext.set_revocations(&[], &index.revoked, true), vec!["demo".to_string()]);
        assert_eq!(ext.revocation("demo").unwrap().source, IndexSource::Test);
        // A plugin that is not installed has nothing to lock.
        assert!(ext.revocation("absent").is_none() && ext.may_switch_on("absent"));
    }

    #[test]
    fn revocation_from_expired_index_still_applies() {
        let dir = tempfile::tempdir().unwrap();
        let ext = two_installed(dir.path());
        let index = revoking(&[("demo", &["*"], "Broken")], "2026-10-02T10:00:00Z");
        let now: chrono::DateTime<chrono::Utc> = "2026-11-13T10:00:00Z".parse().unwrap();
        assert_eq!(aio_plugin_index::check_fresh(&index, None, now), aio_plugin_index::Freshness::Expired);
        let mut switched_off = Vec::new();
        ext.apply_revocations_with(&index.revoked, &[], false, |key| switched_off.push(key.to_string()));
        assert_eq!(switched_off, vec!["ext:demo".to_string()]);
        assert!(!ext.may_switch_on("demo"));
    }

    #[test]
    fn official_revocations_hold_beside_test_ones() {
        let dir = tempfile::tempdir().unwrap();
        let ext = two_installed(dir.path());
        let official = revoking(&[("demo", &["*"], "Official reason")], "2026-12-30T10:00:00Z").revoked;
        let test = revoking(&[("other", &["2.0.0"], "Test reason"), ("demo", &["1.0.0"], "Test says demo")], "2026-12-30T10:00:00Z").revoked;
        // A test index that revokes nothing does not lift an official revocation.
        assert_eq!(ext.set_revocations(&official, &[], true), vec!["demo".to_string()]);
        // Test revocations add to the official ones; where both cover a plugin, the official one is named.
        assert_eq!(ext.set_revocations(&official, &test, true), vec!["demo".to_string(), "other".to_string()]);
        assert_eq!(ext.revocation("demo").unwrap(), Revocation { source: IndexSource::Official, reason: "Official reason".into() });
        assert_eq!(ext.revocation("other").unwrap().source, IndexSource::Test);
        // Developer mode off: the test ones go, the official ones stay.
        assert_eq!(ext.set_revocations(&official, &test, false), vec!["demo".to_string()]);
        assert!(ext.may_switch_on("other") && !ext.may_switch_on("demo"));
    }

    #[test]
    fn officially_revoked_version_is_refused_from_a_test_index() {
        let dir = tempfile::tempdir().unwrap();
        let ext = ExternalPlugins::new(dir.path().join("ext"));
        let prefix = "http://127.0.0.1:9/packages/";
        let bytes = aiop_api("Someone", "demo", "1.0.0", "1.0", &[]);
        let entry = entry_json(prefix, &bytes, "demo", "1.0.0", "Someone", &[]);
        let signed = signed_index(prefix, IndexSource::Test, vec![entry], serde_json::json!([]));
        let listed = signed.index.entry("demo").unwrap().clone();
        let pending = ext.prepare_verified(&signed, &listed, bytes.clone()).unwrap();
        ext.set_revocations(&revoking(&[("demo", &["1.0.0"], "Officially bad")], "2026-12-30T10:00:00Z").revoked, &[], true);
        let error = ext.prepare_verified(&signed, &listed, bytes).unwrap_err().to_string();
        assert!(error.contains("official index") && error.contains("Officially bad"), "{error}");
        // A consent given before the revocation arrived is refused too.
        let error = ext.commit_against(&pending, Some(&signed)).unwrap_err().to_string();
        assert!(error.contains("Officially bad"), "{error}");
        assert!(ext.list().is_empty());
    }

    #[test]
    fn unrevoked_plugin_stays_off_but_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        let ext = two_installed(dir.path());
        // The on/off store: the keys switched off.
        let mut off: std::collections::BTreeSet<String> = Default::default();
        ext.apply_revocations_with(&revoking(&[("demo", &["1.0.0"], "Broken")], "2026-12-30T10:00:00Z").revoked, &[], false, |key| {
            off.insert(key.to_string());
        });
        assert!(off.contains("ext:demo") && !ext.may_switch_on("demo"));
        // A newer index no longer revokes it: unlocked, but nothing switches it on again.
        let mut switched = Vec::new();
        ext.apply_revocations_with(&revoking(&[], "2026-12-30T10:00:00Z").revoked, &[], false, |key| switched.push(key.to_string()));
        assert!(switched.is_empty());
        assert!(off.contains("ext:demo"), "it stays off until the user switches it on");
        assert!(ext.may_switch_on("demo"));
        assert!(ext.revocation("demo").is_none());
    }
}
