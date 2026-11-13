//! The page task with the real example plugin (`source/plugins/hello`, packed by the plugin host's test support):
//! load, render, events, a runaway plugin, and a script through the approval queue to the login.

use std::path::Path;
use std::time::{Duration, Instant};

use aio_plugin_host::approvals::{Decision, Pending};
use aio_remote::recorder::Recorder;
use aio_remote::Server;

use super::*;
use crate::plugins::external::ExternalPlugins;

#[path = "../../../plugin_host/tests/support/mod.rs"]
mod support;

struct Page {
    _dir: tempfile::TempDir,
    state: Arc<Mutex<State>>,
    output: Arc<Mutex<PluginOutput>>,
    events: mpsc::UnboundedSender<UiEvent>,
    approvals: Arc<Approvals>,
    task: tokio::task::JoinHandle<()>,
    shared: Shared,
}

/// Installs `aiop` into a temporary folder and starts the page task on it, as if its manifest also declared
/// `permissions`.
fn open(aiop: Vec<u8>, permissions: &[&str]) -> Page {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plugin.aiop");
    std::fs::write(&file, aiop).unwrap();
    let ext = ExternalPlugins::new(dir.path().join("ext"));
    ext.install_file(&file).unwrap();
    let mut info = ext.get(0).unwrap();
    info.manifest.permissions.extend(permissions.iter().map(|p| aio_plugin_host::Permission::parse(p).unwrap()));
    let shared = Shared::for_tests(tokio::runtime::Handle::current());
    let output = Arc::new(Mutex::new(PluginOutput::new()));
    let plugin_dir = ext.dir(&info.manifest.id);
    let services: Arc<dyn HostServices> = Arc::new(AppServices::new(shared.clone(), &info.manifest.name, plugin_dir.clone(), output.clone()));
    let approvals = Arc::new(Approvals::new());
    let state = Arc::new(Mutex::new(State { busy: true, ..State::default() }));
    let (events, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(info, plugin_dir, services, approvals.clone(), state.clone(), receiver, shared.ctx.clone()));
    Page { _dir: dir, state, output, events, approvals, task, shared }
}

/// Waits (at most 60 s: the first load compiles the plugin) until `done` holds for the page state.
async fn until(page: &Page, what: &str, done: impl Fn(&State) -> bool) {
    let start = Instant::now();
    while !done(&page.state.lock()) {
        assert!(start.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The tests share the plugin id `hello`, and with it the live-instance count: one at a time.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SERIAL.lock().await
}

fn texts(tree: &UiTree) -> Vec<String> {
    tree.elements.iter().filter_map(|e| if let UiElement::Text(t) = e { Some(t.clone()) } else { None }).collect()
}

fn installed(dir: &Path) -> bool {
    dir.join("ext/hello/plugin.wasm").exists()
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_greets_and_counts() {
    let _serial = serial().await;
    let page = open(support::hello_aiop(), &[]);
    assert!(installed(page._dir.path()));
    until(&page, "the first page", |s| s.tree.is_some() && !s.busy).await;
    assert!(external::is_live("hello"), "the instance is alive while the page is open");
    let first = page.state.lock().tree.clone().unwrap();
    assert!(validate(&first).is_ok());
    assert_eq!(texts(&first), ["Greeted 0 times"]);

    page.events.send(UiEvent::Changed(ChangedData { id: "name".into(), value: "Ann".into() })).unwrap();
    page.events.send(UiEvent::Clicked("greet".into())).unwrap();
    until(&page, "the page after Greet", |s| s.tree.as_ref().is_some_and(|t| texts(t) == ["Greeted 1 times"]) && !s.busy).await;
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
    let page = open(support::hello_aiop_with("runaway"), &[]);
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
    let page = open(support::hello_aiop_with("scripter"), &["ssh"]);
    let server: Arc<dyn Server> = recorder.clone();
    page.shared.set_session_for_tests(Some(server));
    let start = Instant::now();
    let pending = loop {
        if let Some(p) = page.approvals.pending().into_iter().next() {
            break p;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "no approval request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(pending.1, Pending::Script { plugin: "hello".into(), script: "echo hi".into(), as_root: false });
    assert_eq!(recorder.text(), "", "nothing runs before Approve");
    page.approvals.decide(pending.0, Decision::Approved);
    until(&page, "the page", |s| s.tree.is_some()).await;
    assert_eq!(recorder.text(), "=== run\necho hi\n");
    assert!(page.output.lock().log.lines().iter().any(|l| l == "[info] script: 0 hi"), "{:?}", page.output.lock().log.lines());
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_page_rejects_what_waits_for_approval() {
    let _serial = serial().await;
    let page = open(support::hello_aiop_with("scripter"), &["ssh"]);
    let start = Instant::now();
    while page.approvals.pending().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(60), "no approval request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // What `ExternalPluginView::stop` does.
    page.task.abort();
    let _ = page.task.await;
    assert!(page.approvals.pending().is_empty());
    assert!(!external::is_live("hello"));
}
