//! External plugins: the `.aiop` packages installed from a file into configuration/plugins/ext/<id>/. This is the
//! app side of [`aio_plugin_host::package`]: the list the Plugins page shows, the consent a user gives before
//! anything is written, and the plain-words texts for each permission.
//!
//! Switching one on or off uses the same store as the built-in plugins (`DisabledPlugins`), under the id
//! `ext:<id>`, so a third-party id can never collide with a built-in one.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use aio_plugin_host::package::{self, Package, MAX_PACKAGE};
use aio_plugin_host::{Manifest, Permission};
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

pub struct ExternalPlugins {
    root: PathBuf,
    list: Mutex<Vec<ExternalInfo>>,
}

/// The process-wide list, loaded the first time it is needed from configuration/plugins/ext/.
pub fn plugins() -> &'static ExternalPlugins {
    static STATE: OnceLock<ExternalPlugins> = OnceLock::new();
    STATE.get_or_init(|| ExternalPlugins::new(packs::root().join("ext")))
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
        let this = Self { root, list: Mutex::new(Vec::new()) };
        this.reload();
        this
    }

    /// Reads the folder again.
    pub fn reload(&self) {
        let list = package::installed(&self.root).into_iter().map(|manifest| ExternalInfo { manifest, verified: false, dev: false }).collect();
        *self.list.lock() = list;
    }

    pub fn list(&self) -> Vec<ExternalInfo> {
        self.list.lock().clone()
    }

    /// The plugin at `index` of [`ExternalPlugins::list`] (what `Page::External` holds).
    pub fn get(&self, index: usize) -> Option<ExternalInfo> {
        self.list.lock().get(index).cloned()
    }

    /// Where the plugin `id` is in [`ExternalPlugins::list`] now.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.list.lock().iter().position(|i| i.manifest.id == id)
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
}
