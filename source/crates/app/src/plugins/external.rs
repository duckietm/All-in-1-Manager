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
use aio_plugin_host::{display_safe, Manifest, Permission};
use parking_lot::Mutex;

use super::{packs, PLUGINS};
use crate::i18n::{t, tf};

/// An installed external plugin, as the Plugins page lists it.
#[derive(Debug, Clone)]
pub struct ExternalInfo {
    pub manifest: Manifest,
    /// Signed by the program's authors. Nothing is in milestone 1: every plugin shows "Not verified".
    pub verified: bool,
    /// Loaded from a developer folder (developer mode), not installed.
    pub dev: bool,
}

/// A package that was read and checked and waits for the user's consent. Nothing is written yet.
#[derive(Debug, Clone)]
pub struct Pending {
    pub package: Package,
    /// The installed version this one replaces, if any.
    pub previous: Option<Manifest>,
}

impl Pending {
    /// Whether the permission is new compared with the installed version (never true for a first install).
    pub fn is_new_permission(&self, p: &Permission) -> bool {
        self.previous.as_ref().is_some_and(|old| !old.permissions.contains(p))
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

impl ExternalPlugins {
    pub fn new(root: PathBuf) -> Self {
        let this = Self { root, list: Mutex::new(Vec::new()), dev: Mutex::new(Dev::default()) };
        this.reload();
        this
    }

    /// Reads the folder again.
    pub fn reload(&self) {
        let list = package::installed(&self.root).into_iter().map(|manifest| ExternalInfo { manifest, verified: false, dev: false }).collect();
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
        Ok(Pending { package, previous })
    }

    /// Writes a package the user agreed to. Blocking.
    pub fn commit(&self, pending: &Pending) -> Result<Manifest> {
        // The consent was given for a comparison with this version: if the installed one changed meanwhile, ask again.
        let now = self.list.lock().iter().find(|i| i.manifest.id == pending.package.manifest.id).map(|i| i.manifest.clone());
        if now != pending.previous {
            bail!("The installed version changed while you were deciding. Choose the file again.");
        }
        // An update replaces the folder: the running version lets go of its files first.
        wait_until_stopped(&pending.package.manifest.id)?;
        let result = package::install(&pending.package, &self.root);
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

    /// Deletes the plugin with its private data.
    pub fn remove(&self, id: &str) -> Result<()> {
        wait_until_stopped(id)?;
        let result = package::remove(id, &self.root);
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

/// One plain sentence for a permission. Every kind is matched, so a new permission cannot be forgotten.
pub fn permission_text(p: &Permission) -> String {
    match p {
        Permission::Ssh => t("Run commands on the hotel's server (you approve each script)").to_string(),
        Permission::SshRoot => t("Run commands as root on the hotel's server (you approve each script)").to_string(),
        Permission::SqlRead(tables) => {
            let list = tables.join(", ");
            if tables.len() == 1 {
                tf("Read the table {tables} of the hotel database", &[("tables", &list)])
            } else {
                tf("Read the tables {tables} of the hotel database", &[("tables", &list)])
            }
        }
        Permission::SqlWrite(tables) => {
            let list = tables.join(", ");
            if tables.len() == 1 {
                tf("Change the table {tables} of the hotel database (you approve each change)", &[("tables", &list)])
            } else {
                tf("Change the tables {tables} of the hotel database (you approve each change)", &[("tables", &list)])
            }
        }
        Permission::SqlReadPrivate(table, column) => {
            tf("Read the private column {column} of the table {table} (it can hold passwords, addresses or tokens)", &[("table", table), ("column", column)])
        }
        Permission::Http(host) => tf("Fetch pages from {host} on the internet", &[("host", host)]),
        Permission::TcpLocal(port) => tf("Connect to a program on this computer, port {port} (for example G-Earth)", &[("port", &port.to_string())]),
        Permission::Files => t("Keep its own files in a private folder (always allowed)").to_string(),
    }
}

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
        let manifest = serde_json::json!({
            "id": id, "name": "Demo", "version": version, "author": "Someone", "description": "A demo",
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
        std::fs::write(wasm, b" asm   ").unwrap();
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
        std::fs::write(dir.path().join("evil.wasm"), b" asm   ").unwrap();
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
}
