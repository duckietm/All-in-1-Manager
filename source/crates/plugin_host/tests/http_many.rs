//! `http-get-many` (API 1.1): up to 8 GETs of one call fetched together, each one checked exactly like `http-get`,
//! the `http:ask` questions asked one at a time before anything is fetched, and the bodies of a call within 32 MiB.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aio_plugin_host::approvals::Approvals;
use aio_plugin_host::bindings::aio::plugin::host::Host;
use aio_plugin_host::net::{BUDGET_EXCEEDED, MAX_HTTP_BODY, MAX_MANY_BYTES, MAX_MANY_URLS};
use aio_plugin_host::runtime::{engine, PluginInstance, MAX_DIALOGS};
use aio_plugin_host::services::{HostServices, Level, Rows, ScriptRequest, ScriptResult, SqlValue, WriteBatch};
use aio_plugin_host::{inspect, install, Permission};

/// Answers `http_get` with the body size the URL's last path segment names (`/size/1024`, else 1 byte), after
/// `delay`; records what happened in order.
#[derive(Default)]
struct Fake {
    logs: Mutex<Vec<(Level, String)>>,
    /// "ask <host>", "get <url>", "asked-get <url>", "done <url>", in order.
    events: Mutex<Vec<String>>,
    /// `ask_host` answers No.
    refuse: bool,
    delay: Duration,
    /// Questions or fetches running at once, now and at most.
    asking: AtomicUsize,
    most_asking: AtomicUsize,
    fetching: AtomicUsize,
    most_fetching: AtomicUsize,
}

impl Fake {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn body(url: &str) -> Vec<u8> {
        let size = url.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(1);
        vec![b'x'; size]
    }
}

fn enter(now: &AtomicUsize, most: &AtomicUsize) {
    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
    most.fetch_max(n, Ordering::SeqCst);
}

#[aio_plugin_host::async_trait]
impl HostServices for Fake {
    async fn ask(&self, _q: String) -> bool {
        false
    }
    async fn ask_host(&self, _plugin: &str, host: &str) -> Option<bool> {
        enter(&self.asking, &self.most_asking);
        self.events.lock().unwrap().push(format!("ask {host}"));
        tokio::time::sleep(self.delay).await;
        self.asking.fetch_sub(1, Ordering::SeqCst);
        Some(!self.refuse)
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
    async fn http_get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.events.lock().unwrap().push(format!("get {url}"));
        enter(&self.fetching, &self.most_fetching);
        let wait = if url.ends_with("/hang") { Duration::from_secs(3600) } else { self.delay };
        tokio::time::sleep(wait).await;
        self.fetching.fetch_sub(1, Ordering::SeqCst);
        self.events.lock().unwrap().push(format!("done {url}"));
        if url.ends_with("/404") {
            return Err("HTTP 404".into());
        }
        Ok(Self::body(url))
    }
    async fn http_get_asked(&self, url: &str, host: &str) -> Result<Vec<u8>, String> {
        assert!(url.contains(host));
        self.events.lock().unwrap().push(format!("asked-get {url}"));
        self.http_get(url).await
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
    fn open_import_preview(&self, _name: String, _bytes: Vec<u8>) {}
}

/// The hello plugin, loaded as if its manifest declared `permissions`.
async fn hello_with(root: &Path, permissions: &[&str], fake: Arc<Fake>) -> PluginInstance {
    let pkg = inspect(&support::hello_aiop(), &[]).unwrap();
    let dir = install(&pkg, root).unwrap();
    let mut manifest = pkg.manifest.clone();
    manifest.permissions = permissions.iter().map(|p| Permission::parse(p).unwrap()).collect();
    PluginInstance::load(&engine(), &dir, manifest, fake, Arc::new(Approvals::new())).await.unwrap()
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn denied(result: &Result<Vec<u8>, String>) -> &str {
    let e = result.as_ref().expect_err("must be refused");
    assert!(e.starts_with("permission denied"), "unexpected error text: {e}");
    e
}

#[tokio::test(flavor = "multi_thread")]
async fn get_many_applies_every_get_rule() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake { delay: Duration::from_millis(100), ..Fake::default() });
    let mut inst = hello_with(root.path(), &["http:api.example.com", "http:ask"], fake.clone()).await;
    let host = inst.host_for_tests();
    let urls = strings(&[
        "https://API.example.com/size/3",
        "https://evil.example.com@api.example.com/",
        "file:///etc/passwd",
        "https://other.example.org/size/5",
        "http://127.0.0.1/",
        "https://api.example.com:8443/",
        "https://api.example.com/404",
        "http://nas.local/",
    ]);
    let results = host.http_get_many(urls).await;
    assert_eq!(results.len(), 8, "one result per URL, in order");
    assert_eq!(results[0].as_deref().unwrap(), b"xxx");
    denied(&results[1]);
    denied(&results[2]);
    assert_eq!(results[3].as_deref().unwrap(), b"xxxxx");
    assert_eq!(denied(&results[4]), "permission denied: host 127.0.0.1 cannot be asked for");
    denied(&results[5]);
    assert_eq!(results[6].as_deref().unwrap_err(), "HTTP 404");
    assert_eq!(denied(&results[7]), "permission denied: host nas.local cannot be asked for");
    // The declared host through the normal fetch, the asked one through the fetch that refuses local addresses; the
    // URLs rebuilt from their checked parts; nothing refused was fetched.
    let events = fake.events();
    let gets: Vec<_> = events.iter().filter(|e| e.starts_with("get ")).cloned().collect();
    assert_eq!(gets.len(), 3, "{events:?}");
    for url in ["https://api.example.com/size/3", "https://other.example.org/size/5", "https://api.example.com/404"] {
        assert!(gets.contains(&format!("get {url}")), "{events:?}");
    }
    let asked: Vec<_> = events.iter().filter(|e| e.starts_with("asked-get ")).cloned().collect();
    assert_eq!(asked, ["asked-get https://other.example.org/size/5"]);
    // Fetched together, not one after the other.
    assert_eq!(fake.most_fetching.load(Ordering::SeqCst), 3);
    // Each refusal is logged.
    let warnings = fake.logs.lock().unwrap().iter().filter(|(l, t)| *l == Level::Warn && t.starts_with("permission denied")).count();
    assert_eq!(warnings, 5);

    // Without http:ask, an undeclared host is refused as by http-get, not asked for.
    let fake = Arc::new(Fake::default());
    let root2 = tempfile::tempdir().unwrap();
    let mut inst = hello_with(root2.path(), &["http:api.example.com"], fake.clone()).await;
    let results = inst.host_for_tests().http_get_many(strings(&["https://other.example.org/"])).await;
    assert_eq!(denied(&results[0]), "permission denied: host 'other.example.org' is not declared with http:other.example.org");
    assert!(fake.events().is_empty());

    // More than 8 URLs: the whole call is refused, nothing fetched.
    let host = inst.host_for_tests();
    let nine = vec!["https://api.example.com/".to_string(); MAX_MANY_URLS + 1];
    let results = host.http_get_many(nine).await;
    assert_eq!(results.len(), MAX_MANY_URLS + 1);
    for r in &results {
        assert_eq!(denied(r), "permission denied: at most 8 URLs per call");
    }
    assert!(fake.events().is_empty());
    assert!(host.http_get_many(Vec::new()).await.is_empty());
    // Exactly 8 is fine.
    let eight = vec!["https://api.example.com/".to_string(); MAX_MANY_URLS];
    assert!(host.http_get_many(eight).await.iter().all(|r| r.as_deref() == Ok(b"x".as_slice())));
}

#[tokio::test(flavor = "multi_thread")]
async fn get_many_asks_hosts_one_at_a_time() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake { delay: Duration::from_millis(50), ..Fake::default() });
    let mut inst = hello_with(root.path(), &["http:api.example.com", "http:ask"], fake.clone()).await;
    let host = inst.host_for_tests();
    let urls = strings(&[
        "https://a.example.org/1",
        "https://api.example.com/2",
        "https://b.example.org/3",
        "https://A.example.org/4",
        "https://c.example.org/5",
    ]);
    let results = host.http_get_many(urls).await;
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
    let events = fake.events();
    // Every question first, in order, each host once; then the fetches.
    let asks: Vec<_> = events.iter().take_while(|e| e.starts_with("ask ")).cloned().collect();
    assert_eq!(asks, ["ask a.example.org", "ask b.example.org", "ask c.example.org"], "{events:?}");
    assert!(events[asks.len()..].iter().all(|e| !e.starts_with("ask ")), "{events:?}");
    assert_eq!(fake.most_asking.load(Ordering::SeqCst), 1, "never two questions at once");

    // Refused hosts are refused for the rest of the session, without a second question.
    let fake = Arc::new(Fake { refuse: true, ..Fake::default() });
    let root2 = tempfile::tempdir().unwrap();
    let mut inst = hello_with(root2.path(), &["http:ask"], fake.clone()).await;
    let host = inst.host_for_tests();
    let results = host.http_get_many(strings(&["https://evil.example/1", "https://evil.example/2"])).await;
    for r in &results {
        assert_eq!(denied(r), "permission denied: host evil.example was refused");
    }
    assert_eq!(fake.events(), ["ask evil.example"]);

    // The questions count toward the call's dialogs: past the limit they are not shown.
    let fake = Arc::new(Fake::default());
    let root3 = tempfile::tempdir().unwrap();
    let mut inst = hello_with(root3.path(), &["http:ask"], fake.clone()).await;
    let host = inst.host_for_tests();
    for _ in 0..MAX_DIALOGS - 1 {
        host.ask("Again?".into()).await;
    }
    let results = host.http_get_many(strings(&["https://one.example.org/", "https://two.example.org/"])).await;
    assert!(results[0].is_ok());
    assert_eq!(denied(&results[1]), "permission denied: host two.example.org was not asked (too many dialogs in one call)");
    assert_eq!(fake.events().iter().filter(|e| e.starts_with("ask ")).count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_many_body_budget() {
    assert_eq!(MAX_MANY_BYTES, 32 * 1024 * 1024);
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let mut inst = hello_with(root.path(), &["http:api.example.com"], fake.clone()).await;
    let host = inst.host_for_tests();
    let mib = 1024 * 1024;
    // 3 x 10 MiB fits; a fourth does not: whichever finishes last of the four gets the refusal.
    let urls: Vec<String> = (0..4).map(|_| format!("https://api.example.com/size/{}", 10 * mib)).collect();
    let results = host.http_get_many(urls).await;
    let refused: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
    assert_eq!(refused.len(), 1, "one of the four is over the budget");
    assert_eq!(refused[0].as_deref().unwrap_err(), BUDGET_EXCEEDED);
    assert_eq!(results.iter().filter_map(|r| r.as_ref().ok()).map(Vec::len).sum::<usize>(), 30 * mib);
    // Exactly the budget fits.
    let urls = strings(&["https://api.example.com/size/16777216", "https://api.example.com/size/16777216"]);
    assert!(host.http_get_many(urls).await.iter().all(|r| r.is_ok()));
    // A body over the per-request cap fails as with http-get and takes nothing from the budget.
    let urls = vec![format!("https://api.example.com/size/{}", MAX_HTTP_BODY + 1), "https://api.example.com/size/2".to_string()];
    let results = host.http_get_many(urls).await;
    assert_eq!(results[0].as_deref().unwrap_err(), "the response is larger than 32 MiB");
    assert_eq!(results[1].as_deref().unwrap(), b"xx");

    // Once over the budget, the fetches not finished yet are stopped and refused too.
    let urls = vec![
        "https://api.example.com/hang".to_string(),
        format!("https://api.example.com/size/{}", 20 * mib),
        format!("https://api.example.com/size/{}", 20 * mib),
    ];
    let started = std::time::Instant::now();
    let results = host.http_get_many(urls).await;
    assert!(started.elapsed() < Duration::from_secs(30), "the hanging fetch was stopped");
    assert_eq!(results[0].as_deref().unwrap_err(), BUDGET_EXCEEDED);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(!fake.events().contains(&"done https://api.example.com/hang".to_string()));
}
