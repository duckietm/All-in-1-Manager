//! Picked folders (API 1.1): a plugin creates files in a folder the user picked, never anything else.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_plugin_host::approvals::Approvals;
use aio_plugin_host::bindings::aio::plugin::host::{Host, HostPickedFolder};
use aio_plugin_host::folders::{FolderLimits, PickedFolder, UserFolders, LIMIT_REACHED, MANAGER_FOLDER, OWN_FOLDER};
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
    assert!(host.reveal(again(&folder)).await.unwrap_err().starts_with("permission denied:"));
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
    assert_eq!(error, "permission denied: notes.txt already exists");
    let error = host.write(again(&folder), "photos/cat.jpg".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: photos/cat.jpg already exists");
    // A folder is not a file to replace either.
    std::fs::create_dir_all(out.join("album.png")).unwrap();
    let error = host.write(again(&folder), "album.png".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: album.png already exists");
    assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"the user's notes");
    assert_eq!(std::fs::read(out.join("photos/cat.jpg")).unwrap(), b"cat");
    // Existing folders are fine to create new files in.
    host.write(again(&folder), "photos/dog.jpg".into(), b"dog".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("photos/dog.jpg")).unwrap(), b"dog");
    // A file this handle wrote, then written by a new handle of the same folder: that one did not write it.
    let other = host.pick_folder("Save".into()).await.unwrap();
    let error = host.write(again(&other), "photos/dog.jpg".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: photos/dog.jpg already exists");
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
    let long = "a.txt".repeat(256);
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
        assert!(error.starts_with("permission denied:"), "{path}: {error}");
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
    assert_eq!(host.write(again(&folder), "big.bin".into(), vec![1; 41]).await.unwrap_err(), LIMIT_REACHED);
    assert!(!out.join("big.bin").exists());
    host.write(again(&folder), "a.txt".into(), vec![1; 40]).await.unwrap();
    host.write(again(&folder), "b.txt".into(), vec![1; 40]).await.unwrap();
    // 80 of 100 bytes used: 21 more do not fit, 20 do.
    assert_eq!(host.write(again(&folder), "c.txt".into(), vec![1; 21]).await.unwrap_err(), LIMIT_REACHED);
    host.write(again(&folder), "c.txt".into(), vec![1; 20]).await.unwrap();
    // Three files written: no fourth, not even an empty one.
    assert_eq!(host.write(again(&folder), "d.txt".into(), Vec::new()).await.unwrap_err(), LIMIT_REACHED);
    assert!(!out.join("d.txt").exists());
    // Rewriting counts the new size only.
    host.write(again(&folder), "a.txt".into(), vec![2; 30]).await.unwrap();
    host.write(again(&folder), "a.txt".into(), vec![2; 40]).await.unwrap();
    assert_eq!(host.write(again(&folder), "a.txt".into(), vec![2; 41]).await.unwrap_err(), LIMIT_REACHED);
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), vec![2; 40]);
    // Refusals carry the prefix every permission refusal has (the SDK maps it to ErrorKind::Denied).
    assert_eq!(LIMIT_REACHED, "permission denied: folder limit reached");
    assert_eq!(MANAGER_FOLDER, "permission denied: this folder belongs to the Manager");
    assert_eq!(OWN_FOLDER, "permission denied: pick a folder of its own for this");
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
    assert!(error.starts_with("permission denied:"), "{error}");
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

#[tokio::test(flavor = "multi_thread")]
async fn refuses_files_that_are_not_data() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    // Programs, libraries, scripts, shortcuts, files without an extension: never.
    for bad in ["version.dll", "run.exe", "start.bat", "start.cmd", "x.ps1", "x.lnk", "x.vbs", "x.js", "x.sql.exe", "x.tar.gz", "README", "sub/noext"] {
        let error = host.write(again(&folder), bad.into(), b"x".to_vec()).await.unwrap_err();
        assert_eq!(error, format!("permission denied: {bad} is not a data file"), "{bad}");
    }
    // Hidden files and folders (.config, .bashrc ...) neither.
    for bad in [".bashrc.txt", ".config/autostart/x.json", "sub/.hidden.png"] {
        let error = host.write(again(&folder), bad.into(), b"x".to_vec()).await.unwrap_err();
        assert!(error.starts_with("permission denied:"), "{bad}: {error}");
    }
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "nothing was created");
    // Every listed data type, in any letter case (the last extension counts).
    let good = [
        "a.json", "a.sql", "a.txt", "a.md", "a.csv", "a.tsv", "a.xml", "a.html", "a.png", "a.gif", "a.jpg", "a.jpeg",
        "a.webp", "a.bmp", "a.svg", "a.zip", "a.nitro", "a.swf", "a.bin", "a.log", "B.PNG", "c.Json", "d.exe.txt",
    ];
    for ok in good {
        host.write(again(&folder), ok.into(), b"x".to_vec()).await.unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_broad_roots() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    let home = root.path().join("home");
    let startup = home.join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup");
    std::fs::create_dir_all(startup.join("inside")).unwrap();
    let host = inst.host_for_tests();
    host.set_user_folders(UserFolders { home: Some(home.clone()), autostart: vec![startup.clone()] });
    // A drive (file system) root, the home folder itself, a startup folder and anything inside one.
    let drive = std::fs::canonicalize(root.path()).unwrap().ancestors().last().unwrap().to_path_buf();
    for picked in [drive, home.clone(), startup.clone(), startup.join("inside")] {
        *fake.folder.lock().unwrap() = Some(picked.clone());
        assert!(host.pick_folder("Save".into()).await.is_none(), "{} must be refused", picked.display());
    }
    assert!(fake.logs.lock().unwrap().iter().any(|(_, l)| l.contains(OWN_FOLDER)), "the refusal is logged");
    // A folder of its own inside the home folder is fine.
    std::fs::create_dir_all(home.join("Documents/rips")).unwrap();
    *fake.folder.lock().unwrap() = Some(home.join("Documents/rips"));
    assert!(host.pick_folder("Save".into()).await.is_some());
    *fake.folder.lock().unwrap() = Some(out);
    assert!(host.pick_folder("Save".into()).await.is_some());
}

/// Without the test hook, the user's real home folder and startup folders are refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_real_home_and_startup_folders_are_refused() {
    let user = UserFolders::of_this_user();
    let home = std::env::home_dir().expect("a home folder");
    assert_eq!(user.home.as_deref(), Some(home.as_path()));
    #[cfg(windows)]
    {
        let appdata = PathBuf::from(std::env::var_os("APPDATA").unwrap());
        assert!(user.autostart.contains(&appdata.join(r"Microsoft\Windows\Start Menu\Programs\Startup")), "{:?}", user.autostart);
    }
    #[cfg(target_os = "linux")]
    assert!(user.autostart.iter().any(|a| a.ends_with("autostart")), "{:?}", user.autostart);
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, _out) = setup(root.path(), fake.clone()).await;
    *fake.folder.lock().unwrap() = Some(home);
    assert!(inst.host_for_tests().pick_folder("Save".into()).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn rewrite_replaces_the_file_whole() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    host.set_folder_limits(FolderLimits { files: 2, bytes: 100, file: 100 });
    let folder = host.pick_folder("Save".into()).await.unwrap();
    for n in 1..=5u8 {
        host.write(again(&folder), "sub/state.json".into(), vec![n; 10 * n as usize]).await.unwrap();
    }
    assert_eq!(std::fs::read(out.join("sub/state.json")).unwrap(), vec![5; 50]);
    // No temporary files are left behind, and the rewrites did not use up the file count.
    let names: Vec<_> = std::fs::read_dir(out.join("sub")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    host.write(again(&folder), "other.txt".into(), vec![0; 50]).await.unwrap();
    assert_eq!(host.write(again(&folder), "other.txt".into(), vec![0; 51]).await.unwrap_err(), LIMIT_REACHED);
}

#[tokio::test(flavor = "multi_thread")]
async fn append_creates_then_extends_own_file() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    // Not there yet: created, with the folders it needs.
    host.append(again(&folder), "dump/catalog.sql".into(), b"INSERT 1;\n".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("dump/catalog.sql")).unwrap(), b"INSERT 1;\n");
    // Then extended, piece by piece (an empty piece too).
    host.append(again(&folder), "dump/catalog.sql".into(), b"INSERT 2;\n".to_vec()).await.unwrap();
    host.append(again(&folder), "dump/catalog.sql".into(), Vec::new()).await.unwrap();
    host.append(again(&folder), "dump/catalog.sql".into(), b"INSERT 3;\n".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("dump/catalog.sql")).unwrap(), b"INSERT 1;\nINSERT 2;\nINSERT 3;\n");
    // An empty first append creates an empty file.
    host.append(again(&folder), "empty.log".into(), Vec::new()).await.unwrap();
    assert_eq!(std::fs::read(out.join("empty.log")).unwrap(), b"");
    // The file is the handle's own: it may go to the import preview.
    HostPickedFolder::open_import_preview(host, again(&folder), "dump/catalog.sql".into()).await.unwrap();
    assert_eq!(fake.previews.lock().unwrap()[0].1, b"INSERT 1;\nINSERT 2;\nINSERT 3;\n");
    // Removed in the meantime: created again, like a first write.
    std::fs::remove_file(out.join("dump/catalog.sql")).unwrap();
    host.append(again(&folder), "dump/catalog.sql".into(), b"INSERT 4;\n".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("dump/catalog.sql")).unwrap(), b"INSERT 4;\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn append_refuses_files_not_created_by_handle() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    std::fs::write(out.join("notes.txt"), b"the user's notes").unwrap();
    std::fs::create_dir_all(out.join("album.png")).unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    let error = host.append(again(&folder), "notes.txt".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: notes.txt already exists");
    assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"the user's notes");
    // A folder is not a file to extend either.
    let error = host.append(again(&folder), "album.png".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: album.png already exists");
    // Created through this handle, then appended through a new handle of the same folder: that one did not create it.
    host.append(again(&folder), "mine.txt".into(), b"mine".to_vec()).await.unwrap();
    let other = host.pick_folder("Save".into()).await.unwrap();
    let error = host.append(again(&other), "mine.txt".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: mine.txt already exists");
    assert_eq!(std::fs::read(out.join("mine.txt")).unwrap(), b"mine");
    // An own file replaced by a folder is not extended.
    std::fs::remove_file(out.join("mine.txt")).unwrap();
    std::fs::create_dir(out.join("mine.txt")).unwrap();
    let error = host.append(again(&folder), "mine.txt".into(), b"x".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: mine.txt already exists");
}

#[tokio::test(flavor = "multi_thread")]
async fn append_follows_the_write_path_rules() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    // A path out of the folder, a program, a hidden name.
    assert!(host.append(again(&folder), "../escape.txt".into(), b"x".to_vec()).await.is_err());
    let error = host.append(again(&folder), "run.exe".into(), b"x".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: run.exe is not a data file");
    let error = host.append(again(&folder), ".bashrc.txt".into(), b"x".to_vec()).await.unwrap_err();
    assert!(error.starts_with("permission denied:"), "{error}");
    assert!(!root.path().join("escape.txt").exists());
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "nothing was created");
    // A Manager folder under a broader root.
    let config = root.path().join("configuration");
    std::fs::create_dir_all(&config).unwrap();
    *fake.manager.lock().unwrap() = vec![config.clone()];
    *fake.folder.lock().unwrap() = Some(root.path().to_path_buf());
    let above = host.pick_folder("Save".into()).await.unwrap();
    let error = host.append(again(&above), "configuration/new.json".into(), b"{}".to_vec()).await.unwrap_err();
    assert_eq!(error, MANAGER_FOLDER);
    assert!(!config.join("new.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn append_refuses_a_hard_link_put_in_place_of_own_file() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("victim.txt"), b"victim").unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    host.append(again(&folder), "own.txt".into(), b"own".to_vec()).await.unwrap();
    // A plain file to every look without following links, but the same file as one outside the folder.
    std::fs::remove_file(out.join("own.txt")).unwrap();
    std::fs::hard_link(outside.join("victim.txt"), out.join("own.txt")).expect("hard links need no privilege");
    let error = host.append(again(&folder), "own.txt".into(), b"evil".to_vec()).await.unwrap_err();
    assert_eq!(error, "permission denied: own.txt has other names (hard links)");
    assert_eq!(std::fs::read(outside.join("victim.txt")).unwrap(), b"victim");
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(windows, ignore = "needs symlink privilege on Windows")]
async fn append_refuses_a_link_put_in_place_of_own_file() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("victim.txt"), b"victim").unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Save".into()).await.unwrap();
    host.append(again(&folder), "own.txt".into(), b"own".to_vec()).await.unwrap();
    std::fs::remove_file(out.join("own.txt")).unwrap();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(outside.join("victim.txt"), out.join("own.txt")).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(outside.join("victim.txt"), out.join("own.txt")).is_ok();
    assert!(made, "a file symlink could not be made");
    let error = host.append(again(&folder), "own.txt".into(), b"evil".to_vec()).await.unwrap_err();
    assert!(error.starts_with("permission denied:"), "{error}");
    assert_eq!(std::fs::read(outside.join("victim.txt")).unwrap(), b"victim");
}

#[tokio::test(flavor = "multi_thread")]
async fn append_respects_file_and_folder_limits() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    host.set_folder_limits(FolderLimits { files: 3, bytes: 100, file: 60 });
    let folder = host.pick_folder("Save".into()).await.unwrap();
    // A first append larger than a file: no file.
    assert_eq!(host.append(again(&folder), "big.bin".into(), vec![1; 61]).await.unwrap_err(), LIMIT_REACHED);
    assert!(!out.join("big.bin").exists());
    // The file's size after the append counts: 50 + 11 > 60, 50 + 10 fits.
    host.append(again(&folder), "a.bin".into(), vec![1; 50]).await.unwrap();
    assert_eq!(host.append(again(&folder), "a.bin".into(), vec![2; 11]).await.unwrap_err(), LIMIT_REACHED);
    host.append(again(&folder), "a.bin".into(), vec![2; 10]).await.unwrap();
    assert_eq!(std::fs::read(out.join("a.bin")).unwrap().len(), 60);
    // The handle's total: 60 + 40 = 100 fits, one byte more does not.
    host.append(again(&folder), "b.bin".into(), vec![3; 40]).await.unwrap();
    assert_eq!(host.append(again(&folder), "b.bin".into(), vec![3; 1]).await.unwrap_err(), LIMIT_REACHED);
    assert_eq!(host.write(again(&folder), "c.bin".into(), vec![3; 1]).await.unwrap_err(), LIMIT_REACHED);
    assert_eq!(std::fs::read(out.join("b.bin")).unwrap(), vec![3; 40]);
    // A new file is one of the files: the third fits (empty), a fourth does not, by append or by write.
    host.append(again(&folder), "c.bin".into(), Vec::new()).await.unwrap();
    assert_eq!(host.append(again(&folder), "d.bin".into(), Vec::new()).await.unwrap_err(), LIMIT_REACHED);
    assert_eq!(host.write(again(&folder), "d.bin".into(), Vec::new()).await.unwrap_err(), LIMIT_REACHED);
    assert!(!out.join("d.bin").exists());
    // A rewrite makes room again: the totals follow the files' real sizes.
    host.write(again(&folder), "a.bin".into(), vec![4; 10]).await.unwrap();
    host.append(again(&folder), "c.bin".into(), vec![5; 50]).await.unwrap();
    assert_eq!(host.append(again(&folder), "c.bin".into(), vec![5; 1]).await.unwrap_err(), LIMIT_REACHED);
}

#[tokio::test(flavor = "multi_thread")]
async fn append_after_write_and_write_after_append() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    host.set_folder_limits(FolderLimits { files: 2, bytes: 100, file: 100 });
    let folder = host.pick_folder("Save".into()).await.unwrap();
    // Written, then extended.
    host.write(again(&folder), "data/state.json".into(), b"{\"a\":".to_vec()).await.unwrap();
    host.append(again(&folder), "data/state.json".into(), b"1}".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("data/state.json")).unwrap(), b"{\"a\":1}");
    // Appended, then rewritten whole: still the handle's own, so it can be extended again.
    host.append(again(&folder), "run.log".into(), b"one\n".to_vec()).await.unwrap();
    host.write(again(&folder), "run.log".into(), b"fresh\n".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("run.log")).unwrap(), b"fresh\n");
    host.append(again(&folder), "run.log".into(), b"two\n".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(out.join("run.log")).unwrap(), b"fresh\ntwo\n");
    // None of this used more than the two files.
    let names: Vec<_> = std::fs::read_dir(out.join("data")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    assert_eq!(host.append(again(&folder), "third.txt".into(), Vec::new()).await.unwrap_err(), LIMIT_REACHED);
}

// ---- Reading and listing a picked folder ---------------------------------------------------------

/// `link` -> `target` as a directory symlink, else a junction (no privilege needed); false when neither can be made.
fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn read_any_data_file_in_folder() {
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    // Files the user (or an earlier run) put there, not written through any handle.
    std::fs::create_dir_all(out.join("rip/packets")).unwrap();
    std::fs::write(out.join("rip/catalog.json"), b"{\"pages\":[]}").unwrap();
    std::fs::write(out.join("rip/packets/0001.BIN"), [0u8, 1, 2, 3]).unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Resume".into()).await.unwrap();
    assert_eq!(host.read(again(&folder), "rip/catalog.json".into(), 0, 1024).await.unwrap(), b"{\"pages\":[]}");
    // Any letter case of a data extension.
    assert_eq!(host.read(again(&folder), "rip/packets/0001.BIN".into(), 1, 2).await.unwrap(), [1, 2]);
    // Its own files too.
    host.write(again(&folder), "own.txt".into(), b"mine".to_vec()).await.unwrap();
    assert_eq!(host.read(again(&folder), "own.txt".into(), 0, 10).await.unwrap(), b"mine");
    // Missing files and folders are errors, not refusals.
    assert_eq!(host.read(again(&folder), "rip/missing.json".into(), 0, 10).await.unwrap_err(), "file not found");
    assert_eq!(host.read(again(&folder), "nowhere/missing.json".into(), 0, 10).await.unwrap_err(), "file not found");
    // A folder with a data name is not a file.
    std::fs::create_dir_all(out.join("album.png")).unwrap();
    assert_eq!(host.read(again(&folder), "album.png".into(), 0, 10).await.unwrap_err(), "not a file");
    // Reading changes nothing.
    assert_eq!(std::fs::read(out.join("rip/catalog.json")).unwrap(), b"{\"pages\":[]}");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_refuses_links_hard_links_and_non_data() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    // Not data files, hidden names, escapes.
    std::fs::write(out.join("tool.exe"), b"MZ").unwrap();
    std::fs::write(out.join("noext"), b"x").unwrap();
    std::fs::create_dir_all(out.join(".git")).unwrap();
    std::fs::write(out.join(".git/config.txt"), b"x").unwrap();
    std::fs::write(out.join(".env.txt"), b"x").unwrap();
    // A hard link to a file outside: a plain file to every look without following links.
    std::fs::hard_link(outside.join("secret.txt"), out.join("linked.txt")).expect("hard links need no privilege");
    // A Manager folder below the picked one.
    let manager = out.join("manager");
    std::fs::create_dir_all(&manager).unwrap();
    std::fs::write(manager.join("logins.json"), b"{}").unwrap();
    *fake.manager.lock().unwrap() = vec![manager.clone()];
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Resume".into()).await.unwrap();
    for path in [
        "tool.exe",
        "noext",
        ".git/config.txt",
        ".env.txt",
        "linked.txt",
        "manager/logins.json",
        "../outside/secret.txt",
        "C:/x.txt",
        "nul.txt",
    ] {
        let error = host.read(again(&folder), path.into(), 0, 100).await.unwrap_err();
        assert!(error.starts_with("permission denied:") || error.starts_with("invalid path"), "{path}: {error}");
    }
    assert_eq!(host.read(again(&folder), "tool.exe".into(), 0, 1).await.unwrap_err(), "permission denied: tool.exe is not a data file");
    assert_eq!(
        host.read(again(&folder), "linked.txt".into(), 0, 1).await.unwrap_err(),
        "permission denied: linked.txt has other names (hard links)"
    );
    assert_eq!(host.read(again(&folder), "manager/logins.json".into(), 0, 1).await.unwrap_err(), MANAGER_FOLDER);
    // Through a link (symlink or junction) to a folder outside.
    if dir_link(&outside, &out.join("link")) {
        let error = host.read(again(&folder), "link/secret.txt".into(), 0, 100).await.unwrap_err();
        assert_eq!(error, "permission denied: link/secret.txt leads through a link");
    } else {
        eprintln!("this user may create neither symlinks nor junctions; the link case is skipped");
    }
    // A file symlink (Unix; Windows needs a privilege for it).
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.txt"), out.join("sym.txt")).unwrap();
        let error = host.read(again(&folder), "sym.txt".into(), 0, 100).await.unwrap_err();
        assert!(error.starts_with("permission denied:"), "{error}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn read_chunks_and_eof() {
    use aio_plugin_host::folders::MAX_FOLDER_READ;
    let root = tempfile::tempdir().unwrap();
    let (mut inst, out) = setup(root.path(), Arc::new(Fake::default())).await;
    let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(out.join("big.bin"), &data).unwrap();
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Resume".into()).await.unwrap();
    // Chunks of 4096: two full, one short, then empty at and past the end.
    let mut all = Vec::new();
    let mut sizes = Vec::new();
    loop {
        let chunk = host.read(again(&folder), "big.bin".into(), all.len() as u64, 4096).await.unwrap();
        sizes.push(chunk.len());
        if chunk.is_empty() {
            break;
        }
        all.extend(chunk);
    }
    assert_eq!(sizes, [4096, 4096, 1808, 0]);
    assert_eq!(all, data);
    assert!(host.read(again(&folder), "big.bin".into(), u64::MAX, 10).await.unwrap().is_empty());
    assert!(host.read(again(&folder), "big.bin".into(), 5, 0).await.unwrap().is_empty());
    // At most 4 MiB per call; asking for more is refused.
    assert_eq!(MAX_FOLDER_READ, 4 * 1024 * 1024);
    assert_eq!(host.read(again(&folder), "big.bin".into(), 0, MAX_FOLDER_READ).await.unwrap().len(), 10_000);
    let error = host.read(again(&folder), "big.bin".into(), 0, MAX_FOLDER_READ + 1).await.unwrap_err();
    assert_eq!(error, "permission denied: at most 4 MiB can be read at once");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_hides_links_and_hidden_names() {
    use aio_plugin_host::folders::MAX_FOLDER_ENTRIES;
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (mut inst, out) = setup(root.path(), fake.clone()).await;
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(out.join("rip/images")).unwrap();
    std::fs::write(out.join("rip/catalog.json"), b"12345").unwrap();
    std::fs::write(out.join("rip/notes.exe"), b"MZ").unwrap();
    std::fs::write(out.join("rip/.hidden.txt"), b"x").unwrap();
    std::fs::create_dir_all(out.join("rip/.cache")).unwrap();
    std::fs::write(out.join("rip/bad name.txt"), b"x").unwrap();
    let linked = dir_link(&outside, &out.join("rip/link"));
    let host = inst.host_for_tests();
    let folder = host.pick_folder("Resume".into()).await.unwrap();
    let entry = |name: &str, is_folder: bool, size: u64| (name.to_string(), is_folder, size);
    let plain = |list: Vec<aio_plugin_host::bindings::aio::plugin::host::FolderEntry>| {
        list.into_iter().map(|e| (e.name, e.is_folder, e.size)).collect::<Vec<_>>()
    };
    let listed = plain(host.list(again(&folder), "rip".into()).await.unwrap());
    assert_eq!(
        listed,
        vec![entry("catalog.json", false, 5), entry("images", true, 0), entry("notes.exe", false, 2)],
        "links ({linked}), hidden names and names no path can name are left out"
    );
    // The picked folder itself.
    assert_eq!(plain(host.list(again(&folder), "".into()).await.unwrap()), vec![entry("rip", true, 0)]);
    assert!(host.list(again(&folder), "rip/images".into()).await.unwrap().is_empty());
    // Not through a link, nor into hidden folders, files or missing folders.
    if linked {
        assert_eq!(host.list(again(&folder), "rip/link".into()).await.unwrap_err(), "permission denied: rip/link leads through a link");
    }
    assert!(host.list(again(&folder), "rip/.cache".into()).await.unwrap_err().starts_with("permission denied:"));
    assert!(host.list(again(&folder), "../outside".into()).await.is_err());
    assert_eq!(host.list(again(&folder), "rip/catalog.json".into()).await.unwrap_err(), "not a folder");
    assert_eq!(host.list(again(&folder), "rip/missing".into()).await.unwrap_err(), "file not found");
    // Never into a Manager folder.
    *fake.manager.lock().unwrap() = vec![out.join("rip/images")];
    let folder = host.pick_folder("Resume".into()).await.unwrap();
    assert_eq!(host.list(again(&folder), "rip/images".into()).await.unwrap_err(), MANAGER_FOLDER);
    // At most 10,000 entries.
    assert_eq!(MAX_FOLDER_ENTRIES, 10_000);
    let many = out.join("many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..MAX_FOLDER_ENTRIES {
        std::fs::write(many.join(format!("{i}.png")), b"").unwrap();
    }
    assert_eq!(host.list(again(&folder), "many".into()).await.unwrap().len(), MAX_FOLDER_ENTRIES);
    std::fs::write(many.join("one-more.png"), b"").unwrap();
    assert_eq!(host.list(again(&folder), "many".into()).await.unwrap_err(), "permission denied: many has more than 10000 entries");
}
