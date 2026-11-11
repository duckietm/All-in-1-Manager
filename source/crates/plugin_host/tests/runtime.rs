//! The sandboxed runtime: loading, rendering, events, the private folder and the limits.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aio_plugin_host::bindings::aio::plugin::host::Host;
use aio_plugin_host::runtime::{engine, resolve_private, PluginError, PluginInstance, UiElement, UiEvent, UiTree};
use aio_plugin_host::services::{HostServices, Level, Rows, ScriptRequest, ScriptResult, SqlValue, WriteBatch};
use aio_plugin_host::{inspect, install};

/// Records what the plugin did; every permission-gated service panics, since the runtime must refuse
/// those calls before they ever reach the services (fail-closed until Tasks 5/6).
#[derive(Default)]
struct Fake {
    logs: Mutex<Vec<(Level, String)>>,
    statuses: Mutex<Vec<String>>,
    open_pick: Mutex<Option<PathBuf>>,
    previews: Mutex<Vec<(String, Vec<u8>)>>,
}

impl Fake {
    fn logs(&self) -> Vec<(Level, String)> {
        self.logs.lock().unwrap().clone()
    }
}

#[aio_plugin_host::async_trait]
impl HostServices for Fake {
    async fn ask(&self, _q: String) -> bool {
        true
    }
    fn log(&self, level: Level, text: &str) {
        self.logs.lock().unwrap().push((level, text.to_string()));
    }
    fn status(&self, text: &str) {
        self.statuses.lock().unwrap().push(text.to_string());
    }
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
    async fn prompt_text(&self, _title: String, _label: String, default: String) -> Option<String> {
        Some(default)
    }
    async fn pick_open_file(&self, _title: String, _exts: Vec<String>) -> Option<PathBuf> {
        self.open_pick.lock().unwrap().clone()
    }
    async fn pick_save_file(&self, _title: String, _suggested: String, _exts: Vec<String>) -> Option<PathBuf> {
        None
    }
    fn open_import_preview(&self, name: String, bytes: Vec<u8>) {
        self.previews.lock().unwrap().push((name, bytes));
    }
}

/// Installs a build of `hello` into `root` and returns its folder.
fn install_hello(root: &Path, aiop: &[u8]) -> PathBuf {
    let pkg = inspect(aiop, &[]).unwrap();
    install(&pkg, root).unwrap()
}

async fn load(dir: &Path, services: Arc<Fake>) -> PluginInstance {
    let pkg_manifest = aio_plugin_host::installed(dir.parent().unwrap()).into_iter().next().unwrap();
    PluginInstance::load(&engine(), dir, pkg_manifest, services).await.unwrap()
}

fn texts(tree: &UiTree) -> Vec<String> {
    tree.elements
        .iter()
        .filter_map(|e| match e {
            UiElement::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_renders_heading_and_button() {
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop());
    let mut inst = load(&dir, Arc::new(Fake::default())).await;
    let tree = inst.render().await.unwrap();
    assert!(matches!(tree.elements[tree.root as usize], UiElement::Column(_)));
    assert!(tree.elements.iter().any(|e| matches!(e, UiElement::Heading(h) if h == "Hello")));
    assert!(tree.elements.iter().any(|e| matches!(e, UiElement::Button(b) if b.id == "greet" && b.label == "Greet")));
    assert_eq!(texts(&tree), vec!["Greeted 0 times".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn click_increments_persisted_counter() {
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop());
    let fake = Arc::new(Fake::default());
    let inst = load(&dir, fake.clone()).await;
    // The instance and its calls are Send: the app runs them on tokio worker threads.
    let tree = tokio::spawn(async move {
        let mut inst = inst;
        inst.event(UiEvent::Changed(aio_plugin_host::runtime::ChangedData {
            id: "name".into(),
            value: "Ann".into(),
        }))
        .await
        .unwrap();
        inst.event(UiEvent::Clicked("greet".into())).await.unwrap();
        inst.render().await.unwrap()
    })
    .await
    .unwrap();
    assert_eq!(texts(&tree), vec!["Greeted 1 times".to_string()]);
    assert!(fake.logs().iter().any(|(l, t)| *l == Level::Info && t == "Hello Ann"));
    assert_eq!(std::fs::read_to_string(dir.join("data/count.txt")).unwrap(), "1");

    // A fresh instance (a new session) reads the counter back from the private folder.
    let mut again = load(&dir, fake.clone()).await;
    assert_eq!(texts(&again.render().await.unwrap()), vec!["Greeted 1 times".to_string()]);
}

#[test]
fn private_paths_are_validated() {
    let data = tempfile::tempdir().unwrap();
    let data = data.path();
    for ok in ["count.txt", "sub/dir/file.json", ".hidden", "a-b_c.1"] {
        let p = resolve_private(data, ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert!(p.starts_with(std::fs::canonicalize(data).unwrap()), "{ok}");
    }
    for bad in [
        "",
        "../manifest.json",
        "..",
        ".",
        "a/../../x",
        "a/./b",
        "/etc/passwd",
        "\\windows\\win.ini",
        "C:/Windows/win.ini",
        "C:win.ini",
        "..\\manifest.json",
        "a\\b",
        "//server/share/x",
        "a//b",
        "a/",
        "nul\0.txt",
        "file.txt:stream",
        "CON",
        "nul.txt",
        "com1",
        "LPT9.log",
        "trailing.",
        "PROGRA~1",
        "with space",
        "ünïcode",
    ] {
        assert!(resolve_private(data, bad).is_err(), "{bad:?} must be rejected");
    }
    let long = "a".repeat(300);
    assert!(resolve_private(data, &long).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn files_cannot_escape_private_folder() {
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop());
    std::fs::write(root.path().join("secret.txt"), b"ssh key").unwrap();
    let mut inst = load(&dir, Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    for path in ["../manifest.json", "../plugin.wasm", "../../secret.txt", "..\\manifest.json", "C:/Windows/win.ini"] {
        assert!(host.file_read(path.into()).await.is_err(), "read {path}");
        assert!(host.file_write(path.into(), b"x".to_vec()).await.is_err(), "write {path}");
    }
    assert_eq!(std::fs::read(root.path().join("secret.txt")).unwrap(), b"ssh key");
    // Inside the folder everything works, subfolders included.
    host.file_write("notes/a.txt".into(), b"hi".to_vec()).await.unwrap();
    assert_eq!(host.file_read("notes/a.txt".into()).await.unwrap(), b"hi");
    assert!(dir.join("data/notes/a.txt").is_file());
    assert!(host.file_read("missing.txt".into()).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn symlink_out_of_private_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop());
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"ssh key").unwrap();
    std::fs::create_dir_all(dir.join("data")).unwrap();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, dir.join("data").join("link")).is_ok() || {
        // Symlinks need a privilege; a directory junction does not and escapes the same way.
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("data").join("link"))
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, dir.join("data").join("link")).is_ok();
    if !made {
        eprintln!("symlinks are not allowed for this user; skipping");
        return;
    }
    let mut inst = load(&dir, Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    assert!(host.file_read("link/secret.txt".into()).await.is_err());
    assert!(host.file_write("link/new.txt".into(), b"x".to_vec()).await.is_err());
    assert!(!outside.join("new.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn gated_imports_are_denied() {
    use aio_plugin_host::bindings::aio::plugin::types::Select;
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop());
    let mut inst = load(&dir, Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    assert_eq!(host.run_script("id".into(), true).await.unwrap_err(), "permission denied");
    let q = Select { table: "users".into(), columns: vec!["id".into()], conditions: vec![], limit: None };
    assert_eq!(host.sql_select(q).await.unwrap_err(), "permission denied");
    assert_eq!(host.sql_write(vec![]).await.unwrap_err(), "permission denied");
    assert_eq!(host.http_get("https://example.com/".into()).await.unwrap_err(), "permission denied");
    assert!(host.tcp_connect(9092).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn picked_file_is_read_only_and_goes_to_the_preview() {
    use aio_plugin_host::bindings::aio::plugin::host::HostPickedFile;
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop());
    let picked = root.path().join("catalog.sql");
    std::fs::write(&picked, b"INSERT 1").unwrap();
    let fake = Arc::new(Fake::default());
    *fake.open_pick.lock().unwrap() = Some(picked.clone());
    let mut inst = load(&dir, fake.clone()).await;
    let host = inst.host_for_tests();
    let file = host.pick_open_file("Open".into(), vec!["sql".into()]).await.unwrap();
    let again = wasmtime::component::Resource::<aio_plugin_host::runtime::PickedFile>::new_borrow(file.rep());
    assert_eq!(host.name(again).await, "catalog.sql");
    let r = wasmtime::component::Resource::new_borrow(file.rep());
    assert_eq!(host.read(r).await.unwrap(), b"INSERT 1");
    let w = wasmtime::component::Resource::new_borrow(file.rep());
    assert!(host.write(w, b"evil".to_vec()).await.is_err(), "a file picked to open is read-only");
    assert_eq!(std::fs::read(&picked).unwrap(), b"INSERT 1");
    let b = wasmtime::component::Resource::new_borrow(file.rep());
    host.open_import_preview(b).await.unwrap();
    assert_eq!(fake.previews.lock().unwrap().clone(), vec![("catalog.sql".to_string(), b"INSERT 1".to_vec())]);
    // A cancelled dialog gives none.
    assert!(host.pick_save_file("Save".into(), "x.sql".into(), vec![]).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn runaway_call_is_interrupted() {
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop_with("runaway"));
    let mut inst = load(&dir, Arc::new(Fake::default())).await;
    let start = Instant::now();
    let err = inst.render().await.unwrap_err();
    let took = start.elapsed();
    assert!(matches!(err, PluginError::TimedOut), "{err:?}");
    assert!(took >= Duration::from_millis(4500) && took < Duration::from_secs(6), "took {took:?}");
    // Poisoned: the next call fails at once, without running guest code.
    let start = Instant::now();
    assert!(matches!(inst.render().await.unwrap_err(), PluginError::TimedOut));
    assert!(matches!(inst.event(UiEvent::Clicked("greet".into())).await.unwrap_err(), PluginError::TimedOut));
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_over_64_mib_traps() {
    let root = tempfile::tempdir().unwrap();
    let dir = install_hello(root.path(), &support::hello_aiop_with("hog"));
    let fake = Arc::new(Fake::default());
    let mut inst = load(&dir, fake.clone()).await;
    let err = inst.render().await.unwrap_err();
    assert!(matches!(err, PluginError::Trapped(_)), "{err:?}");
    // The guest's stderr (Rust prints the failed allocation) lands in the plugin log.
    assert!(
        fake.logs().iter().any(|(l, t)| *l == Level::Error && t.contains("memory allocation")),
        "{:?}",
        fake.logs()
    );
    assert!(matches!(inst.render().await.unwrap_err(), PluginError::Trapped(_)));
}
