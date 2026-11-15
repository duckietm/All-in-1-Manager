//! Picked folders (API 1.1): a plugin creates files in a folder the user picked, never anything else.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_plugin_host::approvals::Approvals;
use aio_plugin_host::bindings::aio::plugin::host::{Host, HostPickedFolder};
use aio_plugin_host::folders::{FolderLimits, PickedFolder, LIMIT_REACHED, MANAGER_FOLDER};
use aio_plugin_host::runtime::{engine, PluginInstance};
use aio_plugin_host::services::{HostServices, Level, Rows, ScriptRequest, ScriptResult, SqlValue, WriteBatch};
use aio_plugin_host::{inspect, install};
use wasmtime::component::Resource;

#[derive(Default)]
struct Fake {
    logs: Mutex<Vec<(Level, String)>>,
    /// What the folder dialog answers.
    folder: Mutex<Option<PathBuf>>,
    /// The Manager's folders.
    manager: Mutex<Vec<PathBuf>>,
    revealed: Mutex<Vec<PathBuf>>,
    previews: Mutex<Vec<(String, Vec<u8>)>>,
}

#[aio_plugin_host::async_trait]
impl HostServices for Fake {
    async fn ask(&self, _q: String) -> bool {
        false
    }
    fn log(&self, level: Level, text: &str) {
        self.logs.lock().unwrap().push((level, text.to_string()));
    }
    fn status(&self, _text: &str) {}
    async fn run_script(&self, _req: ScriptRequest) -> Result<ScriptResult, String> {
        panic!("run_script reached the services")
    }
    async fn sql_select(&self, _sql: String, _params: Vec<SqlValue>) -> Result<Rows, String> {
        panic!("sql_select reached the services")
    }
    async fn sql_write(&self, _batch: WriteBatch) -> Result<u64, String> {
        panic!("sql_write reached the services")
    }
    async fn http_get(&self, _url: &str) -> Result<Vec<u8>, String> {
        panic!("http_get reached the services")
    }
    async fn prompt_text(&self, _t: String, _l: String, _d: String) -> Option<String> {
        None
    }
    async fn pick_open_file(&self, _t: String, _e: Vec<String>) -> Option<PathBuf> {
        None
    }
    async fn pick_save_file(&self, _t: String, _s: String, _e: Vec<String>) -> Option<PathBuf> {
        None
    }
    fn open_import_preview(&self, name: String, bytes: Vec<u8>) {
        self.previews.lock().unwrap().push((name, bytes));
    }
    async fn pick_folder(&self, _title: String) -> Option<PathBuf> {
        self.folder.lock().unwrap().clone()
    }
    fn reveal_folder(&self, folder: &Path) -> Result<(), String> {
        self.revealed.lock().unwrap().push(folder.to_path_buf());
        Ok(())
    }
    fn manager_folders(&self) -> Vec<PathBuf> {
        self.manager.lock().unwrap().clone()
    }
}

/// A plugin (`hello`) installed under `root/plugins`, loaded with `fake`; the user picks `root/out` (created).
async fn setup(root: &Path, fake: Arc<Fake>) -> (PluginInstance, PathBuf) {
    let plugins = root.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let pkg = inspect(&support::hello_aiop(), &[]).unwrap();
    let dir = install(&pkg, &plugins).unwrap();
    let manifest = aio_plugin_host::installed(&plugins).into_iter().next().unwrap();
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    *fake.folder.lock().unwrap() = Some(out.clone());
    let inst = PluginInstance::load(&engine(), &dir, manifest, fake, Arc::new(Approvals::new())).await.unwrap();
    (inst, out)
}

/// A second borrow of the same handle (the guest passes `borrow<picked-folder>` to each method).
fn again(folder: &Resource<PickedFolder>) -> Resource<PickedFolder> {
    Resource::new_borrow(folder.rep())
}

#[tokio::test(flavor = "multi_thread")]
async fn write_creates_nested_files() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Where to save the catalogue".into()).await.expect("the user picked a folder");
    assert_eq!(HostPickedFolder::name(host, again(&folder)).await, "out");
    host.write(again(&folder), "catalog.sql".into(), b"INSERT 1".to_vec()).await.unwrap();
    host.write(again(&folder), "icons/furni/chair_icon.png".into(), b"png".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("catalog.sql")).unwrap(), b"INSERT 1");
    assert_eq!(std::fs::read(out.join("icons/furni/chair_icon.png")).unwrap(), b"png");
    host.reveal(again(&folder)).await.unwrap();
    assert_eq!(fake.revealed.lock().unwrap().clone(), vec![std::fs::canonicalize(&out).unwrap()]);
    // Each reveal opens a window: not again right away.
    assert!(host.reveal(again(&folder)).await.unwrap_err().starts_with("denied:"));
    // A cancelled dialog gives none.
    *fake.folder.lock().unwrap() = None;
    assert!(host.pick_folder("Again".into()).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_overwrite_of_existing_file() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    std::fs::write(out.join("notes.txt"), b"the user's notes").unwrap();
    std::fs::create_dir_all(out.join("photos")).unwrap();
    std::fs::write(out.join("photos/cat.jpg"), b"cat").unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    let error = host.write(again(&folder), "notes.txt".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "denied: notes.txt already exists");
    let error = host.write(again(&folder), "photos/cat.jpg".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "denied: photos/cat.jpg already exists");
    // A folder is not a file to replace either.
    let error = host.write(again(&folder), "photos".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "denied: photos already exists");
    assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"the user's notes");
    assert_eq!(std::fs::read(out.join("photos/cat.jpg")).unwrap(), b"cat");
    // Existing folders are fine to create new files in.
    host.write(again(&folder), "photos/dog.jpg".into(), b"dog".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("photos/dog.jpg")).unwrap(), b"dog");
    // A file this handle wrote, then written by a new handle of the same folder: that one did not write it.
    let other = host.pick_folder("Save".into()).await.unwrap();
    let error = host.write(again(&other), "photos/dog.jpg".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "denied: photos/dog.jpg already exists");
}

#[tokio::test(flavor = "multi_thread")]
async fn rewrite_of_own_file_allowed() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    host.write(again(&folder), "data/progress.json".into(), b"{\"done\":1}".to_vec()).await.unwrap();
    host.write(again(&folder), "data/progress.json".into(), b"{\"done\":22}".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("data/progress.json")).unwrap(), b"{\"done\":22}");
    host.write(again(&folder), "data/progress.json".into(), b"{}".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("data/progress.json")).unwrap(), b"{}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_escape() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    let long = "a".repeat(256);
    let deep = "a/".repeat(8) + "f";
    for bad in [
        "",
        "..",
        "../escape.txt",
        "sub/../../escape.txt",
        "/escape.txt",
        "\\escape.txt",
        "C:/escape.txt",
        "C:escape.txt",
        "C:",
        "//server/share/escape.txt",
        "\\\\server\\share\\escape.txt",
        "sub\\escape.txt",
        "CON",
        "nul.txt",
        "com1.sql",
        "LPT9",
        "file.txt:stream",
        "trailing.",
        "./x",
        long.as_str(),
        deep.as_str(),
    ] {
        assert!(host.write(again(&folder), bad.into(), b"x".to_vec()).await.is_err(), "{bad:?} must be refused");
    }
    assert!(!root.path().join("escape.txt").exists());
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "nothing was created in the folder");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_symlink_inside_folder() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let link = out.join("link");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok() || {
        // Symlinks need a privilege; a directory junction does not and escapes the same way.
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
    if !made {
        eprintln!("this user may create neither symlinks nor junctions; skipping");
        return;
    }
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    for path in ["link/new.txt", "link/sub/new.txt", "link"] {
        let error = host.write(again(&folder), path.into(), b"x".to_vec()).await.unwrap_err();
        assert!(error.starts_with("denied:"), "{path}: {error}");
    }
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing was written through the link");
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_enforced() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    host.set_folder_limits(FolderLimits { files: 3, bytes: 100, file: 40 });
    let folder = host.pick_folder("Save".into()).await.unwrap();
    // One file: at most 40 bytes.
    assert_eq!(host.write(again(&folder), "big".into(), vec![1; 41]).await.unwrap_err(), LIMIT_REACHED);
    assert!(!out.join("big").exists());
    host.write(again(&folder), "a".into(), vec![1; 40]).await.unwrap();
    host.write(again(&folder), "b".into(), vec![1; 40]).await.unwrap();
    // 80 of 100 bytes used: 21 more do not fit, 20 do.
    assert_eq!(host.write(again(&folder), "c".into(), vec![1; 21]).await.unwrap_err(), LIMIT_REACHED);
    host.write(again(&folder), "c".into(), vec![1; 20]).await.unwrap();
    // Three files written: no fourth, not even an empty one.
    assert_eq!(host.write(again(&folder), "d".into(), Vec::new()).await.unwrap_err(), LIMIT_REACHED);
    assert!(!out.join("d").exists());
    // Rewriting counts the new size only.
    host.write(again(&folder), "a".into(), vec![2; 30]).await.unwrap();
    host.write(again(&folder), "a".into(), vec![2; 40]).await.unwrap();
    assert_eq!(host.write(again(&folder), "a".into(), vec![2; 41]).await.unwrap_err(), LIMIT_REACHED);
    assert_eq!(std::fs::read(out.join("a")).unwrap(), vec![2; 40]);
    // The defaults are the documented ones.
    assert_eq!(FolderLimits::default(), FolderLimits { files: 100_000, bytes: 2 << 30, file: 256 << 20 });
}

#[tokio::test(flavor = "multi_thread")]
async fn import_preview_only_for_own_files() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    std::fs::write(out.join("secret.sql"), b"the user's own file").unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    let error = HostPickedFolder::open_import_preview(host, again(&folder), "secret.sql".into()).await.unwrap_err();
    assert!(error.starts_with("denied:"), "{error}");
    assert!(HostPickedFolder::open_import_preview(host, again(&folder), "missing.sql".into()).await.is_err());
    host.write(again(&folder), "out/catalog.sql".into(), b"INSERT 1".to_vec()).await.unwrap();
    HostPickedFolder::open_import_preview(host, again(&folder), "out/catalog.sql".into()).await.unwrap();
    assert_eq!(fake.previews.lock().unwrap().clone(), vec![("catalog.sql".to_string(), b"INSERT 1".to_vec())]);
    // At most 32 MiB, like 1.0's open-import-preview.
    host.write(again(&folder), "big.sql".into(), vec![b' '; 32 * 1024 * 1024 + 1]).await.unwrap();
    let error = HostPickedFolder::open_import_preview(host, again(&folder), "big.sql".into()).await.unwrap_err();
    assert!(error.contains("larger than 32 MiB"), "{error}");
    assert_eq!(fake.previews.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_manager_folder() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, _out) = setup(root.path(), fake.clone()).await;
    // The Manager's configuration: plugins, http-hosts.json, verified.json.
    let config = root.path().join("configuration");
    std::fs::create_dir_all(config.join("plugins/ext/hello")).unwrap();
    *fake.manager.lock().unwrap() = vec![config.clone()];
    let host = inst.host_for_tests();
    for picked in [config.join("plugins"), config.join("plugins/ext/hello"), config.clone()] {
        *fake.folder.lock().unwrap() = Some(picked.clone());
        assert!(host.pick_folder("Save".into()).await.is_none(), "{} must be refused", picked.display());
    }
    // The plugin's own folder (its code and private data) too, without the app naming it.
    for own in ["plugins/hello", "plugins/hello/data"] {
        *fake.folder.lock().unwrap() = Some(root.path().join(own));
        assert!(host.pick_folder("Save".into()).await.is_none(), "{own} must be refused");
    }
    assert!(fake.logs.lock().unwrap().iter().any(|(_, l)| l.contains(MANAGER_FOLDER)), "the refusal is logged");
    // A folder above the configuration: writes into it are refused, in any letter case.
    *fake.folder.lock().unwrap() = Some(root.path().to_path_buf());
    let folder = host.pick_folder("Save".into()).await.expect("a folder above the Manager's may be picked");
    let mut paths = vec!["configuration/plugins/ext/hello/http-hosts.json", "configuration/new.txt", "plugins/hello/data/count.txt"];
    if cfg!(windows) {
        // The same folders on a case-insensitive file system.
        paths.push("CONFIGURATION/Plugins/x.json");
    }
    for path in paths {
        let error = host.write(again(&folder), path.into(), b"{}".to_vec()).await.unwrap_err();
        assert_eq!(error, MANAGER_FOLDER, "{path}");
    }
    assert!(!config.join("plugins/ext/hello/http-hosts.json").exists());
    assert!(!config.join("new.txt").exists());
    host.write(again(&folder), "elsewhere.txt".into(), b"ok".to_vec()).await.unwrap();
}
