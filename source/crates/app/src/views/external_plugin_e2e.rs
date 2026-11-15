//! The page task with the real example plugin (`source/plugins/hello`, packed by the plugin host's test support):
//! load, render, events, a runaway plugin, a script through the approval queue to the login, and Reload.

use std::path::Path;
use std::time::{Duration, Instant};

use aio_plugin_host::approvals::{Decision, Pending};
use aio_remote::recorder::Recorder;
use aio_remote::Server;

use super::plugin_tree::Node;
use super::*;
use crate::plugins::external::ExternalPlugins;

#[path = "../../../plugin_host/tests/support/mod.rs"]
mod support;

/// `aiop` installed into a temporary folder, as if its manifest also declared `permissions`.
struct Installed {
    dir: tempfile::TempDir,
    info: ExternalInfo,
    plugin_dir: PathBuf,
}

fn install(aiop: Vec<u8>, permissions: &[&str]) -> Installed {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plugin.aiop");
    std::fs::write(&file, aiop).unwrap();
    let ext = ExternalPlugins::new(dir.path().join("ext"));
    ext.install_file(&file).unwrap();
    let mut info = ext.get(0).unwrap();
    info.manifest.permissions.extend(permissions.iter().map(|p| aio_plugin_host::Permission::parse(p).unwrap()));
    let plugin_dir = ext.dir(&info.manifest.id);
    Installed { dir, info, plugin_dir }
}

struct Page {
    installed: Installed,
    state: Arc<Mutex<State>>,
    output: Arc<Mutex<PluginOutput>>,
    events: mpsc::UnboundedSender<UiEvent>,
    approvals: Arc<Approvals>,
    task: tokio::task::JoinHandle<()>,
}

#[test]
fn the_hello_folder_loads_in_developer_mode() {
    // Packing builds hello into its own folder (source/plugins/hello/.cargo/config.toml), where dev_wasm points.
    support::hello_aiop();
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/hello");
    let dir = tempfile::tempdir().unwrap();
    let ext = ExternalPlugins::new(dir.path().join("ext"));
    ext.set_developer(true, std::slice::from_ref(&folder));
    let pending = ext.prepare_dev(&folder).unwrap();
    assert_eq!(pending.manifest.id, "hello");
    assert!(pending.wasm.ends_with("target/wasm32-wasip2/release/hello.wasm"), "{}", pending.wasm.display());
    assert!(pending.wasm.starts_with(std::fs::canonicalize(&folder).unwrap()));
}

/// Starts the page task on an installed plugin, with `session` as the login.
fn open(aiop: Vec<u8>, permissions: &[&str], session: Option<Arc<dyn Server>>) -> Page {
    let installed = install(aiop, permissions);
    let shared = Shared::for_tests(tokio::runtime::Handle::current());
    shared.set_session_for_tests(session);
    let output = Arc::new(Mutex::new(PluginOutput::new()));
    let exports = installed.dir.path().join("ext-exports/hello");
    let services: Arc<dyn HostServices> = Arc::new(AppServices::new(shared.clone(), &installed.info.manifest.name, exports, output.clone()));
    let approvals = Arc::new(Approvals::new());
    let state = Arc::new(Mutex::new(State { busy: true, ..State::default() }));
    let (events, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(installed.info.clone(), installed.plugin_dir.clone(), services, approvals.clone(), state.clone(), receiver, shared.ctx.clone()));
    Page { installed, state, output, events, approvals, task }
}

/// Waits (at most 60 s: the first load compiles the plugin) until `done` holds.
async fn wait_for(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn until(page: &Page, what: &str, done: impl Fn(&State) -> bool) {
    wait_for(what, || done(&page.state.lock())).await;
}

/// The tests share the plugin id `hello`, and with it the live-instance count: one at a time.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SERIAL.lock().await
}

fn texts(page: &Result<Shown, String>) -> Vec<String> {
    let shown = page.as_ref().expect("a valid page");
    shown.nodes.iter().filter_map(|n| if let Node::Text(t) = n { Some(t.clone()) } else { None }).collect()
}

fn installed(dir: &Path) -> bool {
    dir.join("ext/hello/plugin.wasm").exists()
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_greets_and_counts() {
    let _serial = serial().await;
    let page = open(support::hello_aiop(), &[], None);
    assert!(installed(page.installed.dir.path()));
    until(&page, "the first page", |s| s.page.is_some() && !s.busy).await;
    assert!(external::is_live("hello"), "the instance is alive while the page is open");
    let first = page.state.lock().page.clone().unwrap();
    assert_eq!(texts(&first), ["Greeted 0 times"]);

    page.events.send(UiEvent::Changed(aio_plugin_host::runtime::ChangedData { id: "name".into(), value: "Ann".into() })).unwrap();
    page.events.send(UiEvent::Clicked("greet".into())).unwrap();
    until(&page, "the page after Greet", |s| s.page.as_ref().is_some_and(|p| texts(p) == ["Greeted 1 times"]) && !s.busy).await;
    assert!(page.output.lock().log.lines().iter().any(|l| l == "[info] Hello Ann"), "{:?}", page.output.lock().log.lines());
    assert!(page.state.lock().stopped.is_none());

    // Closing the page: the sender goes, the task ends, the instance is dropped.
    drop(page.events);
    page.task.await.unwrap();
    assert!(!external::is_live("hello"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runaway_plugin_shows_the_stopped_card() {
    let _serial = serial().await;
    let page = open(support::hello_aiop_with("runaway"), &[], None);
    let start = Instant::now();
    until(&page, "the stop", |s| s.stopped.is_some()).await;
    let reason = page.state.lock().stopped.clone().unwrap();
    assert!(reason.contains("took too long"), "{reason}");
    assert!(!page.state.lock().busy, "no spinner on a stopped plugin");
    // Load (compile) plus the 5 s call limit; nothing waits for the guest beyond that.
    assert!(start.elapsed() < Duration::from_secs(60));
    page.task.await.unwrap();
    assert!(!external::is_live("hello"), "the stopped instance let go of its files");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_waits_for_approval_then_runs_as_the_login_user() {
    let _serial = serial().await;
    let recorder = Arc::new(Recorder::new().with_responder(|_, _, _| aio_remote::CommandResult::ok("hi\n")));
    let server: Arc<dyn Server> = recorder.clone();
    let page = open(support::hello_aiop_with("scripter"), &["ssh"], Some(server));
    wait_for("the approval request", || !page.approvals.pending().is_empty()).await;
    let pending = page.approvals.pending().into_iter().next().unwrap();
    assert_eq!(*pending.1, Pending::Script { plugin: "hello".into(), script: "echo hi".into(), as_root: false });
    assert_eq!(recorder.text(), "", "nothing runs before Approve");
    page.approvals.decide(pending.0, Decision::Approved);
    until(&page, "the page", |s| s.page.is_some()).await;
    assert_eq!(recorder.text(), "=== run\necho hi\n");
    assert!(page.output.lock().log.lines().iter().any(|l| l == "[info] script: 0 hi"), "{:?}", page.output.lock().log.lines());
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_page_rejects_what_waits_for_approval() {
    let _serial = serial().await;
    let page = open(support::hello_aiop_with("scripter"), &["ssh"], None);
    wait_for("the approval request", || !page.approvals.pending().is_empty()).await;
    // What `Running::halt` does.
    page.task.abort();
    let _ = page.task.await;
    assert!(page.approvals.pending().is_empty());
    assert!(!external::is_live("hello"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_isolates_old_instance() {
    let _serial = serial().await;
    let plugin = install(support::hello_aiop_with("scripter"), &["ssh"]);
    let shared = Shared::for_tests(tokio::runtime::Handle::current());
    let exports = plugin.dir.path().join("ext-exports/hello");
    let mut view = ExternalPluginView::open_in(plugin.info.clone(), 0, plugin.plugin_dir.clone(), exports, &shared);
    let old_approvals = view.running.approvals.clone();
    let old_state = view.running.state.clone();
    wait_for("the first instance's request", || !old_approvals.pending().is_empty()).await;

    view.restart(&shared);
    assert!(!Arc::ptr_eq(&old_state, &view.running.state), "a fresh state");
    assert!(!Arc::ptr_eq(&old_approvals, &view.running.approvals), "a queue of its own");
    assert!(old_approvals.pending().is_empty(), "the old instance's request is rejected");
    let new_approvals = view.running.approvals.clone();
    wait_for("the new instance's request", || !new_approvals.pending().is_empty()).await;
    // The old instance is gone for good (its drop ran, and rejected only its own requests).
    wait_for("the old instance to end", || Arc::strong_count(&old_state) == 1).await;
    assert_eq!(new_approvals.pending().len(), 1, "the new request survives the old instance's drop");
    let state = view.running.state.lock();
    assert!(state.stopped.is_none() && state.page.is_none(), "nothing of the old instance in the new state");
}
