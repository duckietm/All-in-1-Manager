//! The approval queue: a plugin's SSH script or database write runs only after the user saw exactly what
//! will run and clicked Approve (spec anti-goal 1, Ruling 14).

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aio_plugin_host::approvals::{Approvals, Decision, Pending, MAX_SCRIPT};
use aio_plugin_host::bindings::aio::plugin::host::Host;
use aio_plugin_host::bindings::aio::plugin::types::{CompareOp, Condition, DeleteOp, InsertOp, SqlValue as WitValue, WriteOp};
use aio_plugin_host::runtime::{engine, PluginInstance, SCRIPT_TIMEOUT, SQL_TIMEOUT};
use aio_plugin_host::services::{HostServices, Level, Rows, ScriptRequest, ScriptResult, SqlValue, WriteBatch};
use aio_plugin_host::{inspect, install, Permission};

/// Records what reached the server and the database.
#[derive(Default)]
struct Fake {
    logs: Mutex<Vec<(Level, String)>>,
    scripts: Mutex<Vec<ScriptRequest>>,
    writes: Mutex<Vec<WriteBatch>>,
    /// No SSH / database session: the services answer "Not connected".
    offline: bool,
    /// The services never answer.
    hang: bool,
}

#[aio_plugin_host::async_trait]
impl HostServices for Fake {
    async fn ask(&self, _q: String) -> bool {
        true
    }
    fn log(&self, level: Level, text: &str) {
        self.logs.lock().unwrap().push((level, text.to_string()));
    }
    fn status(&self, _text: &str) {}
    async fn run_script(&self, req: ScriptRequest) -> Result<ScriptResult, String> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.offline {
            return Err("Not connected".into());
        }
        self.scripts.lock().unwrap().push(req);
        Ok(ScriptResult { exit_code: 0, output: "hi\n".into(), error: String::new() })
    }
    async fn sql_select(&self, _sql: String, _params: Vec<SqlValue>) -> Result<Rows, String> {
        panic!("sql_select is not part of these tests")
    }
    async fn sql_write(&self, batch: WriteBatch) -> Result<u64, String> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.offline {
            return Err("Not connected".into());
        }
        let n = batch.statements.len() as u64;
        self.writes.lock().unwrap().push(batch);
        Ok(n)
    }
    async fn http_get(&self, _url: &str) -> Result<Vec<u8>, String> {
        panic!("http_get is not part of these tests")
    }
    async fn prompt_text(&self, _t: String, _l: String, d: String) -> Option<String> {
        Some(d)
    }
    async fn pick_open_file(&self, _t: String, _e: Vec<String>) -> Option<PathBuf> {
        None
    }
    async fn pick_save_file(&self, _t: String, _s: String, _e: Vec<String>) -> Option<PathBuf> {
        None
    }
    fn open_import_preview(&self, _name: String, _bytes: Vec<u8>) {}
}

fn perms(list: &[&str]) -> Vec<Permission> {
    list.iter().map(|p| Permission::parse(p).unwrap()).collect()
}

/// An `Approvals` whose notifier counts the changes of the pending list.
fn approvals() -> (Arc<Approvals>, Arc<AtomicUsize>) {
    let changes = Arc::new(AtomicUsize::new(0));
    let c = changes.clone();
    (Arc::new(Approvals::with_notifier(move || {
        c.fetch_add(1, Ordering::SeqCst);
    })), changes)
}

/// `hello` (or one of its variants) loaded as if its manifest declared `permissions`.
async fn hello(aiop: &[u8], root: &Path, permissions: &[&str], fake: Arc<Fake>, approvals: Arc<Approvals>) -> PluginInstance {
    let pkg = inspect(aiop, &[]).unwrap();
    let dir = install(&pkg, root).unwrap();
    let mut manifest = pkg.manifest.clone();
    manifest.permissions = perms(permissions);
    PluginInstance::load(&engine(), &dir, manifest, fake, approvals).await.unwrap()
}

/// Waits (up to 10 s, real time) until `n` requests are pending.
async fn wait_pending(a: &Approvals, n: usize) -> Vec<(u64, Pending)> {
    for _ in 0..2000 {
        let p = a.pending();
        if p.len() == n {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("expected {n} pending requests, have {:?}", a.pending());
}

fn delete_room(id: i64) -> WriteOp {
    WriteOp::Delete(DeleteOp {
        table: "rooms".into(),
        conditions: vec![Condition { column: "id".into(), op: CompareOp::Eq, value: WitValue::Int(id) }],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn script_runs_only_after_approval() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (queue, changes) = approvals();
    let inst = hello(&support::hello_aiop(), root.path(), &["ssh"], fake.clone(), queue.clone()).await;
    let script = "#!/bin/sh\n\techo hi\nid\n";
    let call = tokio::spawn(async move {
        let mut inst = inst;
        inst.host_for_tests().run_script(script.into(), false).await
    });

    let pending = wait_pending(&queue, 1).await;
    let (id, item) = &pending[0];
    assert_eq!(*item, Pending::Script { plugin: "hello".into(), script: script.into(), as_root: false });
    assert_eq!(item.plugin(), "hello");
    // The admin sees exactly what will run.
    assert_eq!(item.text(), script);
    assert!(changes.load(Ordering::SeqCst) >= 1, "the UI is woken when a request arrives");
    // Nothing ran while the request waits.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fake.scripts.lock().unwrap().is_empty());
    assert!(!call.is_finished());

    queue.decide(*id, Decision::Approved);
    let result = call.await.unwrap().unwrap();
    assert_eq!((result.exit_code, result.output.as_str()), (0, "hi\n"));
    assert_eq!(
        *fake.scripts.lock().unwrap(),
        vec![ScriptRequest { plugin: "hello".into(), script: script.into(), as_root: false }]
    );
    assert!(queue.pending().is_empty());
    let after = changes.load(Ordering::SeqCst);
    assert!(after >= 2, "the UI is woken when a request leaves");
    // Deciding again, or an id that never existed, does nothing.
    queue.decide(*id, Decision::Rejected);
    queue.decide(9_999, Decision::Approved);
    assert_eq!(changes.load(Ordering::SeqCst), after);
    assert_eq!(fake.scripts.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_run_only_after_approval() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (queue, _) = approvals();
    let inst = hello(&support::hello_aiop(), root.path(), &["sql:write:rooms"], fake.clone(), queue.clone()).await;
    let ins = WriteOp::Insert(InsertOp { table: "rooms".into(), values: vec![("name".into(), WitValue::Text("Lobby\u{202e}".into()))] });
    let call = tokio::spawn(async move {
        let mut inst = inst;
        inst.host_for_tests().sql_write(vec![ins, delete_room(7)]).await
    });
    let pending = wait_pending(&queue, 1).await;
    let (id, item) = &pending[0];
    let Pending::Writes { plugin, batch } = item else { panic!("{item:?}") };
    assert_eq!(plugin, "hello");
    assert_eq!(batch.statements.len(), 2);
    // The preview is the summary, one line per change, with plugin text sanitised.
    assert_eq!(item.text(), "INSERT INTO rooms (name) VALUES ('Lobby\u{fffd}')\nDELETE FROM rooms WHERE id = 7");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fake.writes.lock().unwrap().is_empty());

    queue.decide(*id, Decision::Approved);
    assert_eq!(call.await.unwrap().unwrap(), 2);
    let writes = fake.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0], *batch, "exactly the batch that was shown is applied");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_script_returns_error_to_plugin() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (queue, _) = approvals();
    let inst = hello(&support::hello_aiop(), root.path(), &["ssh", "sql:write:rooms"], fake.clone(), queue.clone()).await;
    let q = queue.clone();
    let (script, write) = tokio::spawn(async move {
        let mut inst = inst;
        let host = inst.host_for_tests();
        let decide = |q: Arc<Approvals>| async move {
            let p = wait_pending(&q, 1).await;
            q.decide(p[0].0, Decision::Rejected);
        };
        let d = tokio::spawn(decide(q.clone()));
        let script = host.run_script("rm -rf /var/www".into(), false).await;
        d.await.unwrap();
        let d = tokio::spawn(decide(q));
        let write = host.sql_write(vec![delete_room(1)]).await;
        d.await.unwrap();
        (script, write)
    })
    .await
    .unwrap();
    let e = script.unwrap_err();
    assert!(e.contains("rejected"), "{e}");
    let e = write.unwrap_err();
    assert!(e.contains("rejected"), "{e}");
    assert!(fake.scripts.lock().unwrap().is_empty());
    assert!(fake.writes.lock().unwrap().is_empty());
    assert!(queue.pending().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn root_script_needs_ssh_root() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (queue, changes) = approvals();
    let mut inst = hello(&support::hello_aiop(), root.path(), &["ssh", "sql:read:rooms"], fake.clone(), queue.clone()).await;
    let host = inst.host_for_tests();
    // Refused before it reaches the queue: the admin is never even asked.
    let e = host.run_script("id".into(), true).await.unwrap_err();
    assert!(e.starts_with("permission denied") && e.contains("ssh:root"), "{e}");
    let e = host.sql_write(vec![delete_room(1)]).await.unwrap_err();
    assert!(e.starts_with("permission denied"), "{e}");
    assert!(queue.pending().is_empty());
    assert_eq!(changes.load(Ordering::SeqCst), 0);

    // With ssh:root the request is queued as a root script.
    let root2 = tempfile::tempdir().unwrap();
    let inst = hello(&support::hello_aiop(), root2.path(), &["ssh:root"], fake.clone(), queue.clone()).await;
    let call = tokio::spawn(async move {
        let mut inst = inst;
        let host = inst.host_for_tests();
        // ssh:root alone does not grant scripts as the login user.
        let plain = host.run_script("id".into(), false).await;
        (plain, host.run_script("id".into(), true).await)
    });
    let p = wait_pending(&queue, 1).await;
    assert_eq!(p[0].1, Pending::Script { plugin: "hello".into(), script: "id".into(), as_root: true });
    queue.decide(p[0].0, Decision::Approved);
    let (plain, rooted) = call.await.unwrap();
    assert!(plain.unwrap_err().starts_with("permission denied"));
    rooted.unwrap();
    assert!(fake.scripts.lock().unwrap()[0].as_root);
}

#[tokio::test(flavor = "multi_thread")]
async fn script_must_be_shown_exactly() {
    // Ruling 14: what the admin sees is what runs, so a script the preview would alter is refused.
    assert_eq!(MAX_SCRIPT, 64 * 1024);
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (queue, changes) = approvals();
    let mut inst = hello(&support::hello_aiop(), root.path(), &["ssh"], fake.clone(), queue.clone()).await;
    let host = inst.host_for_tests();
    for bad in [
        "echo safe\rrm -rf /",
        "echo \u{1b}[8mhidden",
        "echo a\u{202e}b",
        "echo a\u{2066}b",
        "echo a\u{200b}b",
        "echo a\u{feff}",
        "echo a\u{2028}b",
        "echo a\0b",
        "echo a\u{7f}",
        "echo a\u{85}",
    ] {
        let e = host.run_script(bad.into(), false).await.unwrap_err();
        assert!(e.starts_with("permission denied"), "{bad:?}: {e}");
    }
    let big = format!("# {}\n", "x".repeat(MAX_SCRIPT));
    let e = host.run_script(big, false).await.unwrap_err();
    assert!(e.starts_with("permission denied") && e.contains("64 KiB"), "{e}");
    let e = host.run_script(String::new(), false).await.unwrap_err();
    assert!(e.starts_with("permission denied"), "{e}");
    assert!(queue.pending().is_empty());
    assert_eq!(changes.load(Ordering::SeqCst), 0);
    // The refusals are logged, sanitised.
    {
        let logs = fake.logs.lock().unwrap();
        assert!(logs.iter().any(|(l, t)| *l == Level::Warn && t.starts_with("permission denied")));
        assert!(logs.iter().all(|(_, t)| !t.contains('\u{202e}') && !t.contains('\u{1b}')));
    }

    // Exactly 64 KiB, with tabs and newlines, is shown in full.
    let mut max = "\techo ok\n".repeat(MAX_SCRIPT / 9);
    max.push_str(&"#".repeat(MAX_SCRIPT - max.len()));
    assert_eq!(max.len(), MAX_SCRIPT);
    let call = tokio::spawn(async move {
        let mut inst = inst;
        inst.host_for_tests().run_script(max, false).await
    });
    let p = wait_pending(&queue, 1).await;
    assert_eq!(p[0].1.text().len(), MAX_SCRIPT);
    queue.decide(p[0].0, Decision::Approved);
    call.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_without_session_fails() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake { offline: true, ..Fake::default() });
    let (queue, _) = approvals();
    let inst = hello(&support::hello_aiop(), root.path(), &["ssh", "sql:write:rooms"], fake.clone(), queue.clone()).await;
    let q = queue.clone();
    let call = tokio::spawn(async move {
        let mut inst = inst;
        let host = inst.host_for_tests();
        let approve = tokio::spawn(async move {
            for _ in 0..2 {
                let p = wait_pending(&q, 1).await;
                q.decide(p[0].0, Decision::Approved);
                // Wait for the request to leave before looking for the next.
                wait_pending(&q, 0).await;
            }
        });
        let script = host.run_script("id".into(), false).await;
        let write = host.sql_write(vec![delete_room(1)]).await;
        approve.await.unwrap();
        (script, write)
    });
    let (script, write) = call.await.unwrap();
    assert_eq!(script.unwrap_err(), "Not connected");
    assert_eq!(write.unwrap_err(), "Not connected");
}

#[tokio::test]
async fn approved_work_has_its_own_timeout() {
    assert_eq!((SCRIPT_TIMEOUT.as_secs(), SQL_TIMEOUT.as_secs()), (600, 30));
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake { hang: true, ..Fake::default() });
    let (queue, _) = approvals();
    let inst = hello(&support::hello_aiop(), root.path(), &["ssh", "sql:write:rooms"], fake, queue.clone()).await;
    tokio::time::pause();
    let q = queue.clone();
    let call = tokio::spawn(async move {
        let mut inst = inst;
        let host = inst.host_for_tests();
        let started = tokio::time::Instant::now();
        let script = host.run_script("sleep infinity".into(), false).await;
        let script_took = started.elapsed();
        let started = tokio::time::Instant::now();
        let write = host.sql_write(vec![delete_room(1)]).await;
        (script, script_took, write, started.elapsed())
    });
    // The approval wait itself has no time limit: an hour later the request is still there.
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let p = q.pending();
    assert_eq!(p.len(), 1);
    q.decide(p[0].0, Decision::Approved);
    let mut p = q.pending();
    while p.is_empty() {
        // A timer (not yield_now) so that paused time can advance to the script's timeout.
        tokio::time::sleep(Duration::from_secs(1)).await;
        p = q.pending();
    }
    q.decide(p[0].0, Decision::Approved);
    let (script, script_took, write, write_took) = call.await.unwrap();
    assert!(script.unwrap_err().contains("timed out"));
    assert!(write.unwrap_err().contains("timed out"));
    assert!(script_took >= Duration::from_secs(3600 + 600), "{script_took:?}");
    assert_eq!(write_took.as_secs(), 30);
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_for_approval_does_not_time_out_the_call() {
    // A real guest call: render asks to run a script; the admin takes 7 s (more than the 5 s budget).
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (queue, _) = approvals();
    let mut inst = hello(&support::hello_aiop_with("scripter"), root.path(), &["ssh"], fake.clone(), queue.clone()).await;
    let q = queue.clone();
    let admin = tokio::spawn(async move {
        let p = wait_pending(&q, 1).await;
        tokio::time::sleep(Duration::from_secs(7)).await;
        q.decide(p[0].0, Decision::Approved);
    });
    let start = Instant::now();
    inst.render().await.unwrap();
    assert!(start.elapsed() >= Duration::from_secs(7));
    admin.await.unwrap();
    let logs = fake.logs.lock().unwrap().clone();
    assert!(logs.iter().any(|(_, t)| t == "script: 0 hi"), "{logs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_page_rejects_pending() {
    let (queue, changes) = approvals();
    // Another plugin's request, which must survive.
    let other = {
        let q = queue.clone();
        tokio::spawn(async move {
            q.request(Pending::Script { plugin: "other".into(), script: "uptime".into(), as_root: false }).await
        })
    };
    wait_pending(&queue, 1).await;

    // 1. The page is closed while a guest call waits for approval: the call is dropped with the
    // instance and its request leaves the queue.
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let mut inst = hello(&support::hello_aiop_with("scripter"), root.path(), &["ssh"], fake.clone(), queue.clone()).await;
    let page = tokio::spawn(async move { inst.render().await });
    let p = wait_pending(&queue, 2).await;
    let id = p.iter().find(|(_, x)| x.plugin() == "hello").unwrap().0;
    let before = changes.load(Ordering::SeqCst);
    page.abort();
    assert!(page.await.unwrap_err().is_cancelled());
    let p = wait_pending(&queue, 1).await;
    assert_eq!(p[0].1.plugin(), "other");
    assert!(changes.load(Ordering::SeqCst) > before, "the UI is woken");
    // Approving the gone request afterwards runs nothing.
    queue.decide(id, Decision::Approved);
    assert!(fake.scripts.lock().unwrap().is_empty());

    // 2. Dropping an instance rejects every request queued in its name.
    let root2 = tempfile::tempdir().unwrap();
    let inst = hello(&support::hello_aiop(), root2.path(), &["ssh"], fake.clone(), queue.clone()).await;
    let mine = {
        let q = queue.clone();
        tokio::spawn(async move { q.request(Pending::Script { plugin: "hello".into(), script: "id".into(), as_root: false }).await })
    };
    wait_pending(&queue, 2).await;
    drop(inst);
    assert_eq!(mine.await.unwrap(), Decision::Rejected);
    let p = queue.pending();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].1.plugin(), "other");

    // 3. A poisoned instance rejects them too.
    let root3 = tempfile::tempdir().unwrap();
    let mut inst = hello(&support::hello_aiop_with("runaway"), root3.path(), &["ssh"], fake.clone(), queue.clone()).await;
    let mine = {
        let q = queue.clone();
        tokio::spawn(async move { q.request(Pending::Script { plugin: "hello".into(), script: "id".into(), as_root: false }).await })
    };
    wait_pending(&queue, 2).await;
    assert!(inst.render().await.is_err());
    assert_eq!(mine.await.unwrap(), Decision::Rejected);
    assert_eq!(queue.pending().len(), 1);

    // The other plugin's request is still there and still decidable.
    let p = queue.pending();
    queue.decide(p[0].0, Decision::Approved);
    assert_eq!(other.await.unwrap(), Decision::Approved);
    drop(inst);
    assert!(fake.scripts.lock().unwrap().is_empty());
}

fn queue_one(q: &Arc<Approvals>, p: Pending) -> tokio::task::JoinHandle<Decision> {
    let q = q.clone();
    tokio::spawn(async move { q.request(p).await })
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_ids_order_and_reject_all_for() {
    let (queue, changes) = approvals();
    let batch = WriteBatch { statements: vec![], summary: vec!["DELETE FROM rooms WHERE id = 1".into()] };
    let a = queue_one(&queue, Pending::Script { plugin: "a".into(), script: "x".into(), as_root: false });
    wait_pending(&queue, 1).await;
    let b = queue_one(&queue, Pending::Writes { plugin: "b".into(), batch: batch.clone() });
    wait_pending(&queue, 2).await;
    let a2 = queue_one(&queue, Pending::Script { plugin: "a".into(), script: "y".into(), as_root: true });
    let p = wait_pending(&queue, 3).await;
    // Monotonic ids, oldest first.
    assert!(p[0].0 < p[1].0 && p[1].0 < p[2].0, "{p:?}");
    assert_eq!(p[1].1.text(), "DELETE FROM rooms WHERE id = 1");
    assert_eq!(changes.load(Ordering::SeqCst), 3);

    queue.reject_all_for("a");
    assert_eq!(a.await.unwrap(), Decision::Rejected);
    assert_eq!(a2.await.unwrap(), Decision::Rejected);
    assert_eq!(changes.load(Ordering::SeqCst), 4, "one wake-up for the whole change");
    // Nothing to reject: no wake-up.
    queue.reject_all_for("a");
    queue.reject_all_for("nobody");
    assert_eq!(changes.load(Ordering::SeqCst), 4);
    let p = queue.pending();
    assert_eq!(p.len(), 1);
    queue.decide(p[0].0, Decision::Approved);
    assert_eq!(b.await.unwrap(), Decision::Approved);
    // A fresh id is never reused.
    let c = queue_one(&queue, Pending::Script { plugin: "a".into(), script: "z".into(), as_root: false });
    let q = wait_pending(&queue, 1).await;
    assert!(q[0].0 > p[0].0);
    // A request whose waiter went away leaves the queue.
    c.abort();
    wait_pending(&queue, 0).await;
}
