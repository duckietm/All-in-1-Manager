//! Permission checks of the gated imports: the SQL builder, HTTP and local TCP, and their wiring in the
//! runtime. Plugins are untrusted: every test here is an attack that must fail closed.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_plugin_host::bindings::aio::plugin::host::{Host, HostTcpConnection};
use aio_plugin_host::bindings::aio::plugin::types::{
    CompareOp, Condition, DeleteOp, InsertOp, Select, SqlValue as WitValue, UpdateOp, WriteOp,
};
use aio_plugin_host::approvals::Approvals;
use aio_plugin_host::net::{check_http, check_tcp, MAX_TCP_CONNECTIONS};
use aio_plugin_host::runtime::{engine, PluginInstance};
use aio_plugin_host::services::{HostServices, Level, Rows, ScriptRequest, ScriptResult, SqlValue, WriteBatch};
use aio_plugin_host::sql::{build_select, build_writes, DEFAULT_LIMIT, MAX_LIMIT, MAX_WRITE_BYTES, MAX_WRITE_OPS};
use aio_plugin_host::{inspect, install, Permission};

fn perms(list: &[&str]) -> Vec<Permission> {
    list.iter().map(|p| Permission::parse(p).unwrap()).collect()
}

fn select(table: &str, columns: &[&str]) -> Select {
    Select { table: table.into(), columns: columns.iter().map(|c| c.to_string()).collect(), conditions: vec![], limit: None }
}

fn cond(column: &str, op: CompareOp, value: WitValue) -> Condition {
    Condition { column: column.into(), op, value }
}

fn denied(r: Result<impl std::fmt::Debug, String>) -> String {
    let e = r.expect_err("must be refused");
    assert!(e.starts_with("permission denied"), "unexpected error text: {e}");
    e
}

// ---- SQL select -------------------------------------------------------------------------------

#[test]
fn select_on_declared_table_builds_parameterised_sql() {
    let granted = perms(&["sql:read:rooms,items"]);
    let mut q = select("rooms", &["id", "name"]);
    q.conditions = vec![
        cond("owner_id", CompareOp::Eq, WitValue::Int(7)),
        cond("name", CompareOp::Like, WitValue::Text("%' OR 1=1 -- ".into())),
        cond("description", CompareOp::IsNull, WitValue::Null),
    ];
    q.limit = Some(50);
    let (sql, params) = build_select(&q, &granted).unwrap();
    assert_eq!(
        sql,
        "SELECT `id`, `name` FROM `rooms` WHERE `owner_id` = ? AND `name` LIKE ? AND `description` IS NULL LIMIT 50"
    );
    // The value never reaches the text: it is a parameter.
    assert_eq!(params, vec![SqlValue::Int(7), SqlValue::Text("%' OR 1=1 -- ".into())]);
}

#[test]
fn select_on_undeclared_table_is_denied() {
    let granted = perms(&["sql:read:rooms", "sql:write:items", "sql:read-private:users.password"]);
    denied(build_select(&select("users", &["id"]), &granted));
    // Write permission is not read permission, nor is read-private.
    denied(build_select(&select("items", &["id"]), &granted));
    denied(build_select(&select("rooms", &["id"]), &[]));
}

#[test]
fn private_column_needs_read_private() {
    let without = perms(&["sql:read:users"]);
    denied(build_select(&select("users", &["id", "password"]), &without));
    for col in ["mail", "ip_register", "ip_current", "auth_ticket", "machine_id", "PASSWORD", "Mail"] {
        denied(build_select(&select("users", &["id", col]), &without));
    }
    // Pattern columns, on every table.
    let rooms = perms(&["sql:read:rooms"]);
    for col in ["password_hash", "api_token", "client_secret", "last_ip_seen", "TOKEN", "SecretKey", "IP_addr"] {
        denied(build_select(&select("rooms", &[col]), &rooms));
    }
    let with = perms(&["sql:read:users", "sql:read-private:users.password"]);
    let (sql, _) = build_select(&select("users", &["id", "password"]), &with).unwrap();
    assert_eq!(sql, "SELECT `id`, `password` FROM `users` LIMIT 1000");
    // The grant covers that one column only.
    denied(build_select(&select("users", &["password", "mail"]), &with));
    // A read-private grant alone does not grant the table.
    denied(build_select(&select("users", &["password"]), &perms(&["sql:read-private:users.password"])));
}

#[test]
fn select_star_on_users_excludes_private_columns() {
    // `*` would return private columns: columns must be listed, on every table.
    let granted = perms(&["sql:read:users,rooms"]);
    denied(build_select(&select("users", &["*"]), &granted));
    denied(build_select(&select("rooms", &["*"]), &granted));
    denied(build_select(&select("rooms", &["`*`"]), &granted));
    // No columns at all is refused too (it is not a shorthand for every column).
    denied(build_select(&select("rooms", &[]), &granted));
}

#[test]
fn identifier_injection_is_refused() {
    let granted = perms(&["sql:read:rooms", "sql:write:rooms"]);
    for table in [
        "rooms; DROP TABLE x",
        "rooms`",
        "rooms` UNION SELECT password FROM users -- ",
        "hotel.rooms",
        "rooms ",
        "",
        "1rooms",
        "rooms\0",
        "r\u{00F6}oms",
    ] {
        denied(build_select(&select(table, &["id"]), &granted));
    }
    for column in ["id`, `password", "id FROM users --", "(SELECT password FROM users)", "id,name", "COUNT(*)", ""] {
        denied(build_select(&select("rooms", &[column]), &granted));
        let mut q = select("rooms", &["id"]);
        q.conditions = vec![cond(column, CompareOp::Eq, WitValue::Int(1))];
        denied(build_select(&q, &granted));
        let ins = WriteOp::Insert(InsertOp { table: "rooms".into(), values: vec![(column.into(), WitValue::Int(1))] });
        denied(build_writes(&[ins], &granted));
    }
    // 64 characters is the longest identifier.
    let long = "a".repeat(65);
    denied(build_select(&select(&long, &["id"]), &perms(&["sql:read:rooms"])));
    denied(build_select(&select("rooms", &[&long]), &granted));
}

#[test]
fn mixed_case_table_alias_is_refused() {
    // On Linux `ROOMS` may be a different table than the declared `rooms`: never normalise.
    let granted = perms(&["sql:read:rooms", "sql:write:rooms"]);
    denied(build_select(&select("ROOMS", &["id"]), &granted));
    denied(build_select(&select("Rooms", &["id"]), &granted));
    let del = WriteOp::Delete(DeleteOp { table: "Rooms".into(), conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))] });
    denied(build_writes(&[del], &granted));
    // Private-column rules ignore case of the table, so a `Users` grant cannot read `Users.password`.
    let upper = perms(&["sql:read:Users"]);
    denied(build_select(&select("Users", &["password"]), &upper));
    assert!(build_select(&select("Users", &["id"]), &upper).is_ok());
}

#[test]
fn where_on_private_column_is_refused() {
    let granted = perms(&["sql:read:users"]);
    for (col, op) in [
        ("password", CompareOp::Eq),
        ("password", CompareOp::Like),
        ("Password", CompareOp::Lt),
        ("auth_ticket", CompareOp::NotNull),
        ("ip_current", CompareOp::Ne),
        ("reset_token", CompareOp::Ge),
    ] {
        let mut q = select("users", &["id"]);
        q.conditions = vec![cond(col, op, WitValue::Text("$2y$10$a".into()))];
        denied(build_select(&q, &granted));
    }
    // Writes: no probing through update or delete conditions either.
    let w = perms(&["sql:write:users"]);
    let upd = WriteOp::Update(UpdateOp {
        table: "users".into(),
        set: vec![("motto".into(), WitValue::Text("x".into()))],
        conditions: vec![cond("password", CompareOp::Like, WitValue::Text("$2y$%".into()))],
    });
    denied(build_writes(&[upd], &w));
    let del = WriteOp::Delete(DeleteOp {
        table: "users".into(),
        conditions: vec![cond("mail", CompareOp::Eq, WitValue::Text("a@b".into()))],
    });
    denied(build_writes(&[del], &w));
    // With the grant, comparing that column is allowed (the plugin may read it anyway).
    let ok = perms(&["sql:read:users", "sql:read-private:users.password"]);
    let mut q = select("users", &["id"]);
    q.conditions = vec![cond("password", CompareOp::Eq, WitValue::Text("x".into()))];
    assert!(build_select(&q, &ok).is_ok());
}

#[test]
fn limit_is_capped() {
    let granted = perms(&["sql:read:rooms"]);
    let (sql, _) = build_select(&select("rooms", &["id"]), &granted).unwrap();
    assert!(sql.ends_with(&format!(" LIMIT {DEFAULT_LIMIT}")), "{sql}");
    let mut q = select("rooms", &["id"]);
    q.limit = Some(u32::MAX);
    let (sql, _) = build_select(&q, &granted).unwrap();
    assert!(sql.ends_with(&format!(" LIMIT {MAX_LIMIT}")), "{sql}");
    q.limit = Some(0);
    let (sql, _) = build_select(&q, &granted).unwrap();
    assert!(sql.ends_with(" LIMIT 0"), "{sql}");
    assert_eq!((DEFAULT_LIMIT, MAX_LIMIT), (1_000, 10_000));
}

// ---- SQL writes -------------------------------------------------------------------------------

#[test]
fn write_needs_sql_write() {
    let read_only = perms(&["sql:read:rooms"]);
    let ins = WriteOp::Insert(InsertOp {
        table: "rooms".into(),
        values: vec![("name".into(), WitValue::Text("Lobby".into())), ("owner_id".into(), WitValue::Int(3))],
    });
    denied(build_writes(std::slice::from_ref(&ins), &read_only));
    denied(build_writes(std::slice::from_ref(&ins), &perms(&["sql:write:items"])));

    let granted = perms(&["sql:write:rooms"]);
    let upd = WriteOp::Update(UpdateOp {
        table: "rooms".into(),
        set: vec![("name".into(), WitValue::Text("x".into()))],
        conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(3))],
    });
    let del = WriteOp::Delete(DeleteOp { table: "rooms".into(), conditions: vec![cond("id", CompareOp::Gt, WitValue::Int(9))] });
    let batch = build_writes(&[ins, upd, del], &granted).unwrap();
    assert_eq!(
        batch.statements,
        vec![
            (
                "INSERT INTO `rooms` (`name`, `owner_id`) VALUES (?, ?)".to_string(),
                vec![SqlValue::Text("Lobby".into()), SqlValue::Int(3)]
            ),
            ("UPDATE `rooms` SET `name` = ? WHERE `id` = ?".to_string(), vec![SqlValue::Text("x".into()), SqlValue::Int(3)]),
            ("DELETE FROM `rooms` WHERE `id` > ?".to_string(), vec![SqlValue::Int(9)]),
        ]
    );
    assert_eq!(
        batch.summary,
        vec![
            "INSERT INTO rooms (name, owner_id) VALUES ('Lobby', 3)".to_string(),
            "UPDATE rooms SET name = 'x' WHERE id = 3".to_string(),
            "DELETE FROM rooms WHERE id > 9".to_string(),
        ]
    );
    // One refused operation refuses the whole batch.
    let bad = WriteOp::Delete(DeleteOp { table: "users".into(), conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))] });
    let good = WriteOp::Delete(DeleteOp { table: "rooms".into(), conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))] });
    denied(build_writes(&[good, bad], &granted));
}

#[test]
fn write_to_private_column_needs_read_private() {
    // Setting `users.auth_ticket` (an SSO ticket) or a password would take over an account.
    let granted = perms(&["sql:write:users"]);
    let upd = WriteOp::Update(UpdateOp {
        table: "users".into(),
        set: vec![("auth_ticket".into(), WitValue::Text("x".into()))],
        conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))],
    });
    denied(build_writes(std::slice::from_ref(&upd), &granted));
    let ins = WriteOp::Insert(InsertOp { table: "users".into(), values: vec![("password".into(), WitValue::Text("x".into()))] });
    denied(build_writes(&[ins], &granted));
    let ok = perms(&["sql:write:users", "sql:read-private:users.auth_ticket"]);
    assert!(build_writes(&[upd], &ok).is_ok());
}

#[test]
fn update_without_conditions_is_refused() {
    let granted = perms(&["sql:write:rooms"]);
    let upd = WriteOp::Update(UpdateOp { table: "rooms".into(), set: vec![("name".into(), WitValue::Null)], conditions: vec![] });
    denied(build_writes(&[upd], &granted));
    let del = WriteOp::Delete(DeleteOp { table: "rooms".into(), conditions: vec![] });
    denied(build_writes(&[del], &granted));
    let nothing = WriteOp::Update(UpdateOp {
        table: "rooms".into(),
        set: vec![],
        conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))],
    });
    denied(build_writes(&[nothing], &granted));
    let empty_insert = WriteOp::Insert(InsertOp { table: "rooms".into(), values: vec![] });
    denied(build_writes(&[empty_insert], &granted));
}

#[test]
fn batch_size_is_capped() {
    let granted = perms(&["sql:write:rooms"]);
    let op = WriteOp::Delete(DeleteOp { table: "rooms".into(), conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))] });
    assert_eq!(build_writes(&vec![op.clone(); MAX_WRITE_OPS], &granted).unwrap().statements.len(), MAX_WRITE_OPS);
    denied(build_writes(&vec![op; MAX_WRITE_OPS + 1], &granted));
    denied(build_writes(&[], &granted));
}

#[test]
fn oversized_batch_is_refused() {
    // The approval card shows every value in full: a batch with more than 8 MiB of values cannot be shown.
    let granted = perms(&["sql:write:rooms"]);
    let insert = |len: usize| WriteOp::Insert(InsertOp { table: "rooms".into(), values: vec![("name".into(), WitValue::Text("x".repeat(len)))] });
    let ops: Vec<WriteOp> = (0..8).map(|_| insert(1024 * 1024)).collect();
    assert_eq!(build_writes(&ops, &granted).unwrap().statements.len(), 8, "exactly 8 MiB is fine");
    let mut over = ops.clone();
    over.push(insert(1));
    let e = denied(build_writes(&over, &granted));
    assert!(e.contains("too large to show"), "{e}");
    // Bytes and condition values count too.
    let update = WriteOp::Update(UpdateOp {
        table: "rooms".into(),
        set: vec![("data".into(), WitValue::Bytes(vec![0; MAX_WRITE_BYTES]))],
        conditions: vec![cond("name", CompareOp::Eq, WitValue::Text("a".into()))],
    });
    denied(build_writes(&[update], &granted));
}

#[test]
fn summary_clips_and_sanitises_values() {
    let granted = perms(&["sql:write:rooms"]);
    let sneaky = format!("ok\u{202E}evil\nDROP{}", "x".repeat(200));
    let ins = WriteOp::Insert(InsertOp {
        table: "rooms".into(),
        values: vec![
            ("name".into(), WitValue::Text(sneaky.clone())),
            ("quote".into(), WitValue::Text("it's".into())),
            ("data".into(), WitValue::Bytes(vec![0xff, 0, 1])),
            ("score".into(), WitValue::Float(1.5)),
            ("gone".into(), WitValue::Null),
        ],
    });
    let batch = build_writes(&[ins], &granted).unwrap();
    let line = &batch.summary[0];
    assert!(!line.contains('\n') && !line.contains('\u{202E}'), "{line:?}");
    assert!(line.contains("'ok\u{FFFD}evil\u{FFFD}DROP"), "{line:?}");
    assert!(line.contains("(212 chars)"), "a clipped value says how long it really is: {line:?}");
    assert!(line.contains("'it''s'") && line.contains("0xff0001 (3 bytes)") && line.contains("1.5") && line.contains("NULL"), "{line:?}");
    // The statement keeps the full value.
    assert_eq!(batch.statements[0].1[0], SqlValue::Text(sneaky));
}

#[test]
fn non_finite_floats_are_refused() {
    let granted = perms(&["sql:read:rooms"]);
    let mut q = select("rooms", &["id"]);
    q.conditions = vec![cond("score", CompareOp::Gt, WitValue::Float(f64::NAN))];
    assert!(build_select(&q, &granted).is_err());
}

// ---- HTTP and TCP -----------------------------------------------------------------------------

#[test]
fn http_only_to_declared_host() {
    let granted = perms(&["http:api.example.com"]);
    assert_eq!(check_http("https://api.example.com/v1/x?y=1", &granted).unwrap(), "https://api.example.com/v1/x?y=1");
    assert_eq!(check_http("HTTP://API.Example.COM", &granted).unwrap(), "http://api.example.com");
    assert!(check_http("https://api.example.com:443/", &granted).is_ok());
    assert!(check_http("http://api.example.com:80/", &granted).is_ok());
    for url in [
        "https://example.com/",
        "https://evil.api.example.com/",
        "https://api.example.com.evil.com/",
        "https://api.example.comevil.com/",
        "https://api.example.com./",
        "https://api.example.com:8443/",
        "http://api.example.com:443/",
        "https://api.example.com:0/",
        "https://api.example.com:99999/",
        "https://api.example.com:/",
        "https://evil.com/?https://api.example.com/",
        "https://evil.com#@api.example.com",
        "https://api.example.com\\@evil.com/",
        "https://api.example.com%2eevil.com/",
        "https://[::1]/",
        "https://127.0.0.1/",
        "https:// api.example.com/",
        "https://api.example.com/\r\nHost: evil.com",
        "https://api.example.com/\u{00e9}",
        "//api.example.com/",
        "api.example.com",
        "",
    ] {
        denied(check_http(url, &granted));
    }
    denied(check_http("https://api.example.com/", &[]));
}

#[test]
fn http_userinfo_and_non_http_schemes_refused() {
    let granted = perms(&["http:api.example.com", "http:evil.com"]);
    for url in [
        "file:///etc/passwd",
        "file://api.example.com/etc/passwd",
        "ftp://api.example.com/",
        "gopher://api.example.com/",
        "javascript:alert(1)",
        "http://user@api.example.com/",
        "https://user:pass@api.example.com/",
        "https://api.example.com@evil.com/",
        "https://@api.example.com/",
    ] {
        denied(check_http(url, &granted));
    }
}

#[test]
fn tcp_only_declared_localhost_port() {
    let granted = perms(&["tcp:localhost:30001", "http:localhost"]);
    assert!(check_tcp(30001, &granted).is_ok());
    denied(check_tcp(30002, &granted));
    denied(check_tcp(80, &granted));
    denied(check_tcp(0, &granted));
    denied(check_tcp(30001, &[]));
}

// ---- Runtime wiring (the hello plugin with a chosen set of permissions) ------------------------

#[derive(Default)]
struct Fake {
    logs: Mutex<Vec<(Level, String)>>,
    selects: Mutex<Vec<(String, Vec<SqlValue>)>>,
    gets: Mutex<Vec<String>>,
    /// Size of the body `http_get` answers with.
    body: usize,
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
    async fn run_script(&self, _req: ScriptRequest) -> Result<ScriptResult, String> {
        panic!("run_script reached the services (these tests never approve)")
    }
    async fn sql_select(&self, sql: String, params: Vec<SqlValue>) -> Result<Rows, String> {
        self.selects.lock().unwrap().push((sql, params));
        Ok(Rows { columns: vec!["id".into()], rows: vec![vec![SqlValue::Int(1)]] })
    }
    async fn sql_write(&self, _batch: WriteBatch) -> Result<u64, String> {
        panic!("sql_write reached the services (these tests never approve)")
    }
    async fn http_get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.gets.lock().unwrap().push(url.to_string());
        Ok(vec![b'x'; self.body])
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

/// The hello plugin, loaded as if its manifest declared `permissions`.
async fn hello_with(root: &Path, permissions: &[&str], fake: Arc<Fake>) -> PluginInstance {
    hello_variant(&support::hello_aiop(), root, permissions, fake).await
}

async fn hello_variant(aiop: &[u8], root: &Path, permissions: &[&str], fake: Arc<Fake>) -> PluginInstance {
    let pkg = inspect(aiop, &[]).unwrap();
    let dir = install(&pkg, root).unwrap();
    let mut manifest = pkg.manifest.clone();
    manifest.permissions = perms(permissions);
    PluginInstance::load(&engine(), &dir, manifest, fake, Arc::new(Approvals::new())).await.unwrap()
}

fn warned(fake: &Fake, needle: &str) -> bool {
    fake.logs.lock().unwrap().iter().any(|(l, t)| *l == Level::Warn && t.starts_with("permission denied") && t.contains(needle))
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_sql_select_is_checked_then_live() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let mut inst = hello_with(root.path(), &["sql:read:rooms"], fake.clone()).await;
    let host = inst.host_for_tests();
    let rows = host.sql_select(select("rooms", &["id"])).await.unwrap();
    assert_eq!(rows.columns, vec!["id".to_string()]);
    assert!(matches!(rows.rows[0][0], WitValue::Int(1)));
    assert_eq!(fake.selects.lock().unwrap()[0].0, "SELECT `id` FROM `rooms` LIMIT 1000");

    let e = host.sql_select(select("users", &["password"])).await.unwrap_err();
    assert!(e.starts_with("permission denied"), "{e}");
    assert_eq!(fake.selects.lock().unwrap().len(), 1, "a refused query never reaches the database");
    // The denial is logged, sanitised (the table name is the plugin's text).
    host.sql_select(select("evil\u{202E}\n", &["id"])).await.unwrap_err();
    assert!(warned(&fake, "users"));
    let logs = fake.logs.lock().unwrap();
    assert!(logs.iter().all(|(_, t)| !t.contains('\u{202E}')));
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_sql_write_and_scripts_are_checked_before_approval() {
    // Valid requests wait for the user's approval (tests/approvals.rs); refused ones never reach the queue.
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let mut inst = hello_with(root.path(), &["sql:write:rooms"], fake.clone()).await;
    let host = inst.host_for_tests();
    // A batch the permissions refuse says why.
    let bad = WriteOp::Delete(DeleteOp { table: "users".into(), conditions: vec![cond("id", CompareOp::Eq, WitValue::Int(1))] });
    let e = host.sql_write(vec![bad]).await.unwrap_err();
    assert!(e.starts_with("permission denied") && e.contains("users"), "{e}");
    assert!(warned(&fake, "users"));
    assert!(host.run_script("id".into(), false).await.unwrap_err().starts_with("permission denied"));
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_http_get_is_checked_and_capped() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake { body: 3, ..Fake::default() });
    let mut inst = hello_with(root.path(), &["http:api.example.com"], fake.clone()).await;
    let host = inst.host_for_tests();
    assert_eq!(host.http_get("https://API.example.com/x".into()).await.unwrap(), b"xxx");
    assert_eq!(*fake.gets.lock().unwrap(), vec!["https://api.example.com/x".to_string()]);
    denied(host.http_get("https://evil.com/".into()).await);
    denied(host.http_get("file:///etc/passwd".into()).await);
    assert_eq!(fake.gets.lock().unwrap().len(), 1);
    assert!(warned(&fake, "evil.com"));

    let big = Arc::new(Fake { body: aio_plugin_host::net::MAX_HTTP_BODY + 1, ..Fake::default() });
    let root2 = tempfile::tempdir().unwrap();
    let mut inst = hello_with(root2.path(), &["http:api.example.com"], big).await;
    assert!(inst.host_for_tests().http_get("https://api.example.com/".into()).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_connection_limit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = tokio::spawn(async move {
        let mut peers = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            peers.push(s);
        }
    });
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let mut inst = hello_with(root.path(), &[&format!("tcp:localhost:{port}")], fake.clone()).await;
    let host = inst.host_for_tests();
    denied(host.tcp_connect(port.wrapping_add(1).max(1)).await);
    let mut conns = Vec::new();
    for _ in 0..MAX_TCP_CONNECTIONS {
        conns.push(host.tcp_connect(port).await.unwrap());
    }
    assert_eq!(MAX_TCP_CONNECTIONS, 4);
    let Err(e) = host.tcp_connect(port).await else { panic!("the 5th connection must be refused") };
    assert!(e.contains("connections"), "{e}");
    // Closing one frees a slot; a closed connection cannot be used.
    let first = conns.remove(0);
    let first_rep = first.rep();
    host.close(wasmtime::component::Resource::new_borrow(first_rep)).await;
    assert!(host.send(wasmtime::component::Resource::new_borrow(first_rep), b"x".to_vec()).await.is_err());
    let again = host.tcp_connect(port).await.unwrap();
    host.send(again, b"hi".to_vec()).await.unwrap();
    accepted.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_send_and_recv_roundtrip() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        s.write_all(&buf).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let mut inst = hello_with(root.path(), &[&format!("tcp:localhost:{port}")], Arc::new(Fake::default())).await;
    let host = inst.host_for_tests();
    let c = host.tcp_connect(port).await.unwrap();
    let rep = c.rep();
    host.send(c, b"ping".to_vec()).await.unwrap();
    let mut got = Vec::new();
    while got.len() < 4 {
        let part = host.recv(wasmtime::component::Resource::new_borrow(rep), 1 << 30).await.unwrap();
        assert!(!part.is_empty());
        got.extend(part);
    }
    assert_eq!(got, b"ping");
    echo.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_connections_close_when_poisoned() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = tempfile::tempdir().unwrap();
    let aiop = support::hello_aiop_with("runaway");
    let mut inst = hello_variant(&aiop, root.path(), &[&format!("tcp:localhost:{port}")], Arc::new(Fake::default())).await;
    inst.host_for_tests().tcp_connect(port).await.unwrap();
    let (mut peer, _) = listener.accept().await.unwrap();
    assert_eq!(inst.open_connections(), 1);
    assert!(inst.render().await.is_err(), "the runaway render is stopped and poisons the instance");
    assert_eq!(inst.open_connections(), 0);
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), peer.read(&mut buf)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0, "the peer sees the connection closed");
}
#[test]
fn bytes_values_are_visible_in_summary() {
    // MariaDB stores bytes in text columns: what a plugin writes as bytes must be readable in the preview.
    let granted = perms(&["sql:write:emulator_settings"]);
    let url = b"https://evil.example/payload\xe2\x80\x8b".to_vec();
    let upd = WriteOp::Update(UpdateOp {
        table: "emulator_settings".into(),
        set: vec![("value".into(), WitValue::Bytes(url))],
        conditions: vec![cond("key", CompareOp::Eq, WitValue::Bytes(b"camera.url".to_vec()))],
    });
    let batch = build_writes(&[upd], &granted).unwrap();
    assert_eq!(
        batch.summary[0],
        "UPDATE emulator_settings SET value = 'https://evil.example/payload\u{fffd}' WHERE key = 'camera.url'"
    );
    // Long UTF-8 bytes are clipped like text.
    let long = WriteOp::Insert(InsertOp {
        table: "emulator_settings".into(),
        values: vec![("value".into(), WitValue::Bytes(vec![b'a'; 100]))],
    });
    let line = &build_writes(&[long], &granted).unwrap().summary[0];
    assert!(line.contains(&format!("'{}\u{2026}' (100 chars)", "a".repeat(80))), "{line}");
    // Not UTF-8: hex of the first 32 bytes and the length.
    let mut raw = vec![0xffu8, 0xfe];
    raw.extend(std::iter::repeat_n(0xabu8, 38));
    let bin = WriteOp::Insert(InsertOp { table: "emulator_settings".into(), values: vec![("value".into(), WitValue::Bytes(raw))] });
    let line = &build_writes(&[bin], &granted).unwrap().summary[0];
    assert!(line.contains(&format!("0xfffe{}\u{2026} (40 bytes)", "ab".repeat(30))), "{line}");
}

#[test]
fn bans_ip_is_private() {
    let granted = perms(&["sql:read:bans"]);
    for col in ["ip", "IP", "Ip", "machine_id", "MACHINE_ID"] {
        denied(build_select(&select("bans", &["id", col]), &granted));
        let mut q = select("bans", &["id"]);
        q.conditions = vec![cond(col, CompareOp::Eq, WitValue::Text("1.2.3.4".into()))];
        denied(build_select(&q, &granted));
    }
    // Only the exact name: `zip` or `ipv` are not private by this rule.
    assert!(build_select(&select("bans", &["zip", "ipv"]), &granted).is_ok());
    let ok = perms(&["sql:read:bans", "sql:read-private:bans.ip"]);
    assert!(build_select(&select("bans", &["ip"]), &ok).is_ok());
}

// ---- Host I/O timeouts (Ruling 12) ------------------------------------------------------------

/// `sql_select` sleeps `delay` (tokio time), or never answers when `None`.
struct Slow {
    delay: Option<std::time::Duration>,
}

#[aio_plugin_host::async_trait]
impl HostServices for Slow {
    async fn ask(&self, _q: String) -> bool {
        true
    }
    fn log(&self, _level: Level, _text: &str) {}
    fn status(&self, _text: &str) {}
    async fn run_script(&self, _req: ScriptRequest) -> Result<ScriptResult, String> {
        unreachable!()
    }
    async fn sql_select(&self, _sql: String, _params: Vec<SqlValue>) -> Result<Rows, String> {
        match self.delay {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
        Ok(Rows { columns: vec!["id".into()], rows: vec![] })
    }
    async fn sql_write(&self, _batch: WriteBatch) -> Result<u64, String> {
        unreachable!()
    }
    async fn http_get(&self, _url: &str) -> Result<Vec<u8>, String> {
        match self.delay {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
        Ok(vec![])
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

async fn slow_hello(root: &Path, delay: Option<std::time::Duration>) -> PluginInstance {
    let pkg = inspect(&support::hello_aiop(), &[]).unwrap();
    let dir = install(&pkg, root).unwrap();
    let mut manifest = pkg.manifest.clone();
    manifest.permissions = perms(&["sql:read:rooms", "http:api.example.com"]);
    PluginInstance::load(&engine(), &dir, manifest, Arc::new(Slow { delay }), Arc::new(Approvals::new())).await.unwrap()
}

#[tokio::test]
async fn slow_sql_is_not_cut_by_the_call_budget() {
    let root = tempfile::tempdir().unwrap();
    let mut inst = slow_hello(root.path(), Some(std::time::Duration::from_secs(7))).await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    assert!(inst.host_for_tests().sql_select(select("rooms", &["id"])).await.is_ok());
    assert!(started.elapsed() >= std::time::Duration::from_secs(7));
}

#[tokio::test]
async fn hung_host_io_times_out_without_poisoning() {
    use aio_plugin_host::runtime::{HTTP_TIMEOUT, SQL_TIMEOUT, TCP_CONNECT_TIMEOUT, TCP_RECV_TIMEOUT};
    assert_eq!([SQL_TIMEOUT, HTTP_TIMEOUT, TCP_CONNECT_TIMEOUT, TCP_RECV_TIMEOUT].map(|d| d.as_secs()), [30, 60, 5, 30]);
    let root = tempfile::tempdir().unwrap();
    let mut inst = slow_hello(root.path(), None).await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let e = inst.host_for_tests().sql_select(select("rooms", &["id"])).await.unwrap_err();
    assert!(e.contains("timed out"), "{e}");
    assert_eq!(started.elapsed().as_secs(), 30);
    let started = tokio::time::Instant::now();
    let e = inst.host_for_tests().http_get("https://api.example.com/".into()).await.unwrap_err();
    assert!(e.contains("timed out"), "{e}");
    assert_eq!(started.elapsed().as_secs(), 60);
    // The instance is still usable.
    assert!(inst.render().await.is_ok());
}