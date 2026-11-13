//! [`AppServices`]: what the Manager gives one running external plugin (its [`HostServices`]). One per plugin
//! instance. The runtime checks permissions and gets the user's approval before calling into here, so these methods
//! only carry the call out: through the login (`Shared::session`), the hotel database, the HTTP client, the window's
//! dialogs and the status line.
//!
//! Every method is cancel-safe: the runtime drops a future when a call is stopped. A dropped database call loses the
//! MariaDB session it held (the next call opens a new one), so no half-read connection is ever reused.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use aio_plugin_host::net::MAX_HTTP_BODY;
use aio_plugin_host::services::{HostServices, Level, Rows, ScriptRequest, ScriptResult, SqlValue, WriteBatch};
use aio_plugin_host::{async_trait, display_safe};
use aio_remote::sql_session::{SqlSession, SqlValue as DbValue};
use aio_remote::Server;
use parking_lot::Mutex;

use crate::gui::dialogs::Choice;
use crate::gui::log_view::LogLines;
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// Lines kept in a plugin page's log.
const LOG_LINES: usize = 2_000;
/// Longest dialog text taken from a plugin.
const DIALOG_TEXT: usize = 1_000;

/// What the plugin produced for its page besides the tree: its log and the last file it handed over.
#[derive(Default)]
pub struct PluginOutput {
    pub log: LogLines,
    /// The file `open-import-preview` saved last (Ruling 17), for the page's "Open folder".
    pub export: Option<PathBuf>,
}

impl PluginOutput {
    pub fn new() -> Self {
        Self { log: LogLines::with_limit(LOG_LINES), export: None }
    }
}

/// The open MariaDB session, with the login it belongs to.
struct Database {
    login: Login,
    name: String,
    session: SqlSession,
}


/// A login, held by identity: a `Weak` (it keeps the allocation, so its address cannot be reused by a later login while
/// this is held) plus the host and port it was made to. Never a bare address.
#[derive(Debug, Clone)]
struct Login {
    server: Weak<dyn Server>,
    host: String,
    port: u16,
}

impl Login {
    fn of(server: &Arc<dyn Server>) -> Self {
        Self { server: Arc::downgrade(server), host: server.login().host.clone(), port: server.login().port }
    }

    /// True when `server` is this very login: still alive, the same object, the same host and port.
    fn is(&self, server: &Arc<dyn Server>) -> bool {
        self.server.upgrade().is_some_and(|mine| std::ptr::addr_eq(Arc::as_ptr(&mine), Arc::as_ptr(server)))
            && self.host == server.login().host
            && self.port == server.login().port
    }
}

/// Counts approved work from the user's Approve until the run ends; dropping it (also by cancellation) counts it
/// down. It takes over the count the Approve click made (see [`AppServices::approve`]).
struct Executing(Arc<AtomicUsize>);

impl Executing {
    fn start(executing: &Arc<AtomicUsize>, approved: &AtomicUsize) -> Self {
        // Up first, then the approval's count down: the total never passes through 0 in between.
        executing.fetch_add(1, Ordering::AcqRel);
        let _ = approved.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
        Self(executing.clone())
    }
}

impl Drop for Executing {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct AppServices {
    shared: Shared,
    /// The plugin's display name (safe to show).
    name: String,
    /// Where `open-import-preview` saves files: `configuration/plugins/ext-exports/<id>/` (Ruling 18).
    exports: PathBuf,
    output: Arc<Mutex<PluginOutput>>,
    /// Approved scripts and write batches running now, and approvals given whose run has not started yet: while
    /// above 0 the window counts as busy (no navigation, disconnect, server switch or close), so approved work is
    /// never cut off, not even in the moment between the click and the start.
    executing: Arc<AtomicUsize>,
    approved: AtomicUsize,
    /// The login of the request waiting for approval (set by `before_approval`, used up by the run).
    bound: Mutex<Option<Login>>,
    http: reqwest::Client,
    /// Opened on the first database call. Taken out while a call uses it, so a cancelled call drops it.
    database: tokio::sync::Mutex<Option<Database>>,
}

impl AppServices {
    pub fn new(shared: Shared, name: &str, exports: PathBuf, output: Arc<Mutex<PluginOutput>>) -> Self {
        Self {
            shared,
            name: display_safe(name, 80),
            exports,
            output,
            executing: Arc::default(),
            approved: AtomicUsize::new(0),
            bound: Mutex::new(None),
            http: http_client(),
            database: tokio::sync::Mutex::new(None),
        }
    }

    /// Approved work counted as running: executing now, or approved and about to start.
    pub fn executing(&self) -> usize {
        self.executing.load(Ordering::Acquire) + self.approved.load(Ordering::Acquire)
    }

    /// The user clicked Approve: busy from now on, before the runtime even starts the run.
    pub fn approve(&self) {
        self.approved.fetch_add(1, Ordering::AcqRel);
    }

    /// The instance ended without starting what was approved (its task finished): nothing is about to start.
    pub fn forget_approved(&self) {
        self.approved.store(0, Ordering::Release);
    }

    /// The server the waiting request was made on (the approval card names it).
    pub fn bound_host(&self) -> Option<String> {
        self.bound.lock().as_ref().map(|b| b.host.clone())
    }

    /// True when the login is no longer the one the waiting request was made on.
    pub fn server_changed(&self) -> bool {
        let current = self.shared.session();
        self.bound.lock().as_ref().is_some_and(|b| !current.as_ref().is_some_and(|s| b.is(s)))
    }

    /// The login an approved request may run on: the very one it was made on, still connected. Uses the binding up.
    fn bound_server(&self) -> Result<Arc<dyn Server>, String> {
        let bound = self.bound.lock().take();
        let server = self.shared.session().ok_or_else(|| "Not connected".to_string())?;
        match bound {
            Some(b) if b.is(&server) => Ok(server),
            _ => Err("The server changed since the request".into()),
        }
    }

    /// "The plugin X asks": the title of every dialog it opens.
    fn asks(&self) -> String {
        tf("The plugin {plugin} asks", &[("plugin", &self.name)])
    }

    fn push_log(&self, line: String) {
        self.output.lock().log.push(line);
        self.shared.ctx.request_repaint();
    }

    /// A session on `server` (the hotel database's name with it).
    async fn open_database(&self, server: &Arc<dyn Server>) -> Result<Database, String> {
        if server.is_dry_run() {
            return Err("Dry run: there is no database".into());
        }
        let name = aio_remote::catalog_admin::hotel_database(&**server).await.map_err(|e| format!("{e:#}"))?;
        let host = server.login().host.clone();
        let Some(saved) = aio_core::config_dir::load_database_credentials(&host) else {
            return Err(format!(
                "No saved MariaDB account for {host} on this computer: log in once in the SQL Editor or the Catalogue and save it"
            ));
        };
        let user = Some(saved.root_user.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into());
        let port = if saved.port > 0 { saved.port } else { 3306 };
        let session = SqlSession::open(&**server, &user, &saved.root_password, port).await.map_err(|e| format!("{e:#}"))?;
        Ok(Database { login: Login::of(server), name, session })
    }

    /// Runs `work` on a database session of `server`, reusing the cached one only when it belongs to that very login.
    /// The session goes back only after a call that succeeded; a failed or cancelled one leaves a fresh session to the
    /// next call.
    async fn with_database<T, F, Fut>(&self, server: &Arc<dyn Server>, work: F) -> Result<T, String>
    where
        F: FnOnce(Database) -> Fut,
        Fut: std::future::Future<Output = (Database, Result<T, String>)>,
    {
        let mut slot = self.database.lock().await;
        let cached = slot.take().filter(|d| d.login.is(server));
        let database = match cached {
            Some(d) => d,
            None => self.open_database(server).await?,
        };
        let (database, result) = work(database).await;
        if result.is_ok() {
            *slot = Some(database);
        }
        result
    }
}

fn to_db(v: SqlValue) -> DbValue {
    match v {
        SqlValue::Null => DbValue::Null,
        SqlValue::Int(i) => DbValue::Int(i),
        SqlValue::Float(f) => DbValue::Double(f),
        SqlValue::Text(s) => DbValue::Text(s),
        SqlValue::Bytes(b) => DbValue::Bytes(b),
    }
}

fn from_db(v: DbValue) -> SqlValue {
    match v {
        DbValue::Null => SqlValue::Null,
        DbValue::Int(i) => SqlValue::Int(i),
        DbValue::UInt(u) => i64::try_from(u).map(SqlValue::Int).unwrap_or_else(|_| SqlValue::Text(u.to_string())),
        DbValue::Float(f) => SqlValue::Float(f64::from(f)),
        DbValue::Double(f) => SqlValue::Float(f),
        DbValue::Bytes(b) => SqlValue::Bytes(b),
        DbValue::Decimal(s) | DbValue::Text(s) | DbValue::Date(s) | DbValue::DateTime(s) | DbValue::Time(s) => SqlValue::Text(s),
    }
}

/// The user agent of every plugin request.
pub fn user_agent() -> String {
    format!("All-in-1-Manager-plugin/{}", crate::update::CURRENT_VERSION)
}

/// Follows a redirect only to the same host (any case), over http or https, on the scheme's default port and never
/// from https down to http: a plugin may only reach the host its permission names (Ruling 10).
fn same_host_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let (Some(first), next) = (attempt.previous().first(), attempt.url()) else {
            return attempt.error("redirect refused");
        };
        let same_host = match (first.host_str(), next.host_str()) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        let scheme_ok = matches!((first.scheme(), next.scheme()), ("https", "https") | ("http", "http") | ("http", "https"));
        let default_port = next.port().is_none();
        if !(same_host && scheme_ok && default_port) {
            attempt.error("a redirect to another host was refused")
        } else if attempt.previous().len() > 5 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// The client of `http-get`: no redirect off the checked host, the plugin user agent.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .redirect(same_host_only())
        .build()
        .expect("HTTP client")
}

/// GET `url`; a non-2xx status is `Err("HTTP <code>")`, and the body read stops past [`MAX_HTTP_BODY`].
pub async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let too_large = || format!("the response is larger than {} MiB", MAX_HTTP_BODY / (1024 * 1024));
    let mut response = client.get(url).send().await.map_err(|e| request_error(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    if response.content_length().is_some_and(|n| n > MAX_HTTP_BODY as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| request_error(&e))? {
        if body.len() + chunk.len() > MAX_HTTP_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A request error in words, with the cause (a refused redirect says so).
fn request_error(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

/// A plugin-chosen file name made safe for this computer's file system, without folders.
pub fn export_name(name: &str) -> String {
    let shown = display_safe(name, 100);
    let mut clean: String = shown
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\u{FFFD}') || c.is_control() { '_' } else { c })
        .collect();
    clean = clean.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    let stem = clean.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if clean.is_empty() || reserved {
        clean = format!("import-{clean}");
    }
    clean
}

/// File-dialog extensions from a plugin: letters and digits only, at most 20 of 10 characters.
fn extensions(list: &[String]) -> Vec<String> {
    list.iter().filter(|e| !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric())).take(20).cloned().collect()
}

fn file_dialog(asks: &str, title: &str, exts: &[String]) -> rfd::AsyncFileDialog {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(format!("{asks}: {}", display_safe(title, 120)));
    let exts = extensions(exts);
    if !exts.is_empty() {
        dialog = dialog.add_filter(exts.join(", "), &exts);
    }
    dialog
}

/// Saves `bytes` in `folder` (the plugin's exports folder, Ruling 18) under a safe form of `name`, as
/// `<stem>-2.<ext>`, `-3`, … when that file exists already (nothing is overwritten); returns the file.
pub fn save_export(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let name = export_name(name);
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    for n in 1..10_000 {
        let file = if n == 1 { folder.join(&name) } else { folder.join(format!("{stem}-{n}{ext}")) };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&file) {
            Ok(mut out) => {
                std::io::Write::write_all(&mut out, bytes)?;
                return Ok(file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

#[async_trait]
impl HostServices for AppServices {
    async fn ask(&self, question: String) -> bool {
        // Closing the question (Escape, a click outside) answers No.
        let options = vec![Choice::new(t("Yes"), "", true).primary(), Choice::new(t("No"), "", false)];
        self.shared.dialogs.choose(&self.asks(), &display_safe(&question, DIALOG_TEXT), "", false, options).await
    }

    fn log(&self, level: Level, text: &str) {
        let tag = match level {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        };
        self.push_log(format!("[{tag}] {}", display_safe(text, aio_plugin_host::runtime::MAX_LOG_ENTRY)));
    }

    fn status(&self, text: &str) {
        self.shared.set_status(&format!("{}: {}", self.name, display_safe(text, 300)));
    }

    fn before_approval(&self) {
        *self.bound.lock() = self.shared.session().map(|s| Login::of(&s));
    }

    async fn run_script(&self, req: ScriptRequest) -> Result<ScriptResult, String> {
        // Counted first: the approval's count is taken over even when the run is refused below.
        let _executing = Executing::start(&self.executing, &self.approved);
        let server = self.bound_server()?;
        let result = if req.as_root { server.run_as_root(&req.script).await } else { server.run(&req.script, None).await };
        let r = result.map_err(|e| format!("{e:#}"))?;
        Ok(ScriptResult { exit_code: r.exit_code, output: r.output, error: r.error })
    }

    async fn sql_select(&self, sql: String, params: Vec<SqlValue>) -> Result<Rows, String> {
        let params: Vec<DbValue> = params.into_iter().map(to_db).collect();
        let server = self.shared.session().ok_or_else(|| "Not connected".to_string())?;
        self.with_database(&server, |db| async move {
            let result = db.session.select_in(&db.name, &sql, params).await.map_err(|e| format!("{e:#}"));
            let rows = result.map(|(columns, rows)| Rows {
                columns,
                rows: rows.into_iter().map(|r| r.into_iter().map(from_db).collect()).collect(),
            });
            (db, rows)
        })
        .await
    }

    async fn sql_write(&self, batch: WriteBatch) -> Result<u64, String> {
        let _executing = Executing::start(&self.executing, &self.approved);
        // The very login the request was approved on, not whatever is current a moment later.
        let server = self.bound_server()?;
        let statements: Vec<(String, Vec<DbValue>)> =
            batch.statements.into_iter().map(|(sql, params)| (sql, params.into_iter().map(to_db).collect())).collect();
        self.with_database(&server, |db| async move {
            let result = db.session.execute_in(&db.name, &statements).await.map(|(changed, _)| changed).map_err(|e| format!("{e:#}"));
            (db, result)
        })
        .await
    }

    async fn http_get(&self, url: &str) -> Result<Vec<u8>, String> {
        fetch(&self.http, url).await
    }

    async fn prompt_text(&self, title: String, label: String, default: String) -> Option<String> {
        let dialogs = self.shared.dialogs.clone();
        let (title, label, default) = (display_safe(&title, DIALOG_TEXT), display_safe(&label, DIALOG_TEXT), display_safe(&default, DIALOG_TEXT));
        dialogs.input(&self.asks(), &title, "", &label, &default, false, t("OK"), None).await
    }

    async fn pick_open_file(&self, title: String, extensions: Vec<String>) -> Option<PathBuf> {
        file_dialog(&self.asks(), &title, &extensions).pick_file().await.map(|f| f.path().to_path_buf())
    }

    async fn pick_save_file(&self, title: String, suggested: String, extensions: Vec<String>) -> Option<PathBuf> {
        let dialog = file_dialog(&self.asks(), &title, &extensions).set_file_name(export_name(&suggested));
        dialog.save_file().await.map(|f| f.path().to_path_buf())
    }

    fn open_import_preview(&self, name: String, bytes: Vec<u8>) {
        // Off the runtime's workers: the file can be 32 MiB. A failure shows on the status line.
        let (folder, shared, output, plugin) = (self.exports.clone(), self.shared.clone(), self.output.clone(), self.name.clone());
        self.shared.runtime.spawn_blocking(move || {
            match save_export(&folder, &name, &bytes) {
                Ok(file) => {
                    let shown = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    shared.set_status(&tf("Saved {name}: open it with Catalogue → Import a file", &[("name", &shown)]));
                    output.lock().export = Some(file);
                }
                Err(e) => shared.set_status(&tf("{plugin}: the file could not be saved: {error}", &[("plugin", &plugin), ("error", &e.to_string())])),
            }
            shared.ctx.request_repaint();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aio_remote::recorder::Recorder;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A server on 127.0.0.1 that answers every connection with `reply(port)`.
    async fn serve(reply: impl Fn(u16) -> Vec<u8> + Send + Sync + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reply = Arc::new(reply);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let reply = reply.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(&reply(port)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn redirect_to_other_host_is_refused() {
        // 127.0.0.1 sends the client on to localhost: the same machine, but another host name.
        let port = serve(|port| format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()).await;
        let error = fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.unwrap_err();
        assert!(error.contains("another host"), "{error}");
    }

    #[tokio::test]
    async fn non_2xx_and_redirects_to_other_ports_are_refused() {
        let port = serve(|_| b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        assert_eq!(fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.unwrap_err(), "HTTP 404");
        let port = serve(|_| b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi".to_vec()).await;
        assert_eq!(fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.unwrap(), b"hi");
        // A redirect to another port of the same host leaves the declared (default) port: refused too.
        let port = serve(|port| format!("HTTP/1.1 301 Moved\r\nLocation: http://127.0.0.1:{port}/y\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()).await;
        assert!(fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.is_err());
    }

    #[tokio::test]
    async fn http_body_over_32_mib_is_refused() {
        // Without a Content-Length: the body is read until the cap, then refused.
        let port = serve(|_| {
            let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            reply.resize(reply.len() + MAX_HTTP_BODY + 1, b'a');
            reply
        })
        .await;
        let error = fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.unwrap_err();
        assert!(error.contains("larger than 32 MiB"), "{error}");
        // With one that says so: refused before reading.
        let port = serve(|_| format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_HTTP_BODY + 1).into_bytes()).await;
        let error = fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.unwrap_err();
        assert!(error.contains("larger than 32 MiB"), "{error}");
        // Exactly the cap is fine.
        let port = serve(|_| {
            let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            reply.resize(reply.len() + MAX_HTTP_BODY, b'a');
            reply
        })
        .await;
        assert_eq!(fetch(&http_client(), &format!("http://127.0.0.1:{port}/")).await.unwrap().len(), MAX_HTTP_BODY);
    }

    #[test]
    fn the_user_agent_names_the_plugin_client() {
        assert_eq!(user_agent(), format!("All-in-1-Manager-plugin/{}", crate::update::CURRENT_VERSION));
    }

    fn services(shared: &Shared, dir: &Path) -> AppServices {
        AppServices::new(shared.clone(), "Demo", dir.to_path_buf(), Arc::new(Mutex::new(PluginOutput::new())))
    }

    #[tokio::test]
    async fn scripts_run_as_the_login_user_or_as_root() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_tests(tokio::runtime::Handle::current());
        let app = services(&shared, dir.path());
        let req = |as_root| ScriptRequest { plugin: "demo".into(), script: "echo hi".into(), as_root };

        assert_eq!(app.run_script(req(false)).await.unwrap_err(), "Not connected");

        let recorder = Arc::new(Recorder::new().with_responder(|_, _, _| aio_remote::CommandResult::ok("out")));
        let server: Arc<dyn Server> = recorder.clone();
        shared.set_session_for_tests(Some(server));
        app.before_approval();
        assert_eq!(app.bound_host().as_deref(), Some(recorder.login().host.as_str()), "the card names the server");
        let r = app.run_script(req(false)).await.unwrap();
        assert_eq!((r.exit_code, r.output.as_str()), (0, "out"));
        // Each run needs its own approval: the binding is used up.
        assert_eq!(app.run_script(req(true)).await.unwrap_err(), "The server changed since the request");
        app.before_approval();
        app.run_script(req(true)).await.unwrap();
        assert_eq!(recorder.text(), "=== run\necho hi\n=== root\necho hi\n");
    }

    #[tokio::test]
    async fn an_approval_is_bound_to_the_server_it_was_made_on() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_tests(tokio::runtime::Handle::current());
        let app = services(&shared, dir.path());
        let first = Arc::new(Recorder::new());
        let second = Arc::new(Recorder::new());
        let server: Arc<dyn Server> = first.clone();
        shared.set_session_for_tests(Some(server));
        app.before_approval();
        assert!(!app.server_changed());
        // The user logs in to another server before approving.
        let server: Arc<dyn Server> = second.clone();
        shared.set_session_for_tests(Some(server));
        assert!(app.server_changed(), "the card warns");
        let req = ScriptRequest { plugin: "demo".into(), script: "echo hi".into(), as_root: true };
        assert_eq!(app.run_script(req).await.unwrap_err(), "The server changed since the request");
        app.before_approval();
        let server: Arc<dyn Server> = first.clone();
        shared.set_session_for_tests(Some(server));
        assert_eq!(app.sql_write(WriteBatch::default()).await.unwrap_err(), "The server changed since the request");
        assert_eq!((first.text(), second.text()), (String::new(), String::new()), "nothing ran anywhere");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn running_approved_script_keeps_app_busy() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_tests(tokio::runtime::Handle::current());
        let app = Arc::new(services(&shared, dir.path()));
        // The script runs until the test lets it end.
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        let recorder = Arc::new(Recorder::new().with_responder(move |_, _, _| {
            let _ = wait.lock().unwrap().recv();
            aio_remote::CommandResult::ok("done")
        }));
        let server: Arc<dyn Server> = recorder;
        shared.set_session_for_tests(Some(server));
        app.before_approval();
        assert_eq!(app.executing(), 0, "waiting for approval is not busy");
        let running = app.clone();
        let task = tokio::spawn(async move { running.run_script(ScriptRequest { plugin: "demo".into(), script: "sleep".into(), as_root: true }).await });
        let start = std::time::Instant::now();
        while app.executing() == 0 {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "the run never counted");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(app.executing(), 1);
        release.send(()).unwrap();
        assert_eq!(task.await.unwrap().unwrap().output, "done");
        assert_eq!(app.executing(), 0, "done: not busy any more");
    }

    #[tokio::test]
    async fn approval_refused_after_reconnect_even_if_address_reused() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_tests(tokio::runtime::Handle::current());
        let app = services(&shared, dir.path());
        let first: Arc<dyn Server> = Arc::new(Recorder::new());
        shared.set_session_for_tests(Some(first.clone()));
        app.before_approval();
        // Disconnect: the first login is gone (nothing holds it but the binding's Weak).
        shared.set_session_for_tests(None);
        drop(first);
        assert!(app.bound.lock().as_ref().unwrap().server.upgrade().is_none(), "the old login cannot come back");
        // A new login to the same host and port, made while the old one's memory may be free.
        let second = Arc::new(Recorder::new());
        let server: Arc<dyn Server> = second.clone();
        shared.set_session_for_tests(Some(server));
        assert!(app.server_changed());
        let req = ScriptRequest { plugin: "demo".into(), script: "echo hi".into(), as_root: true };
        assert_eq!(app.run_script(req).await.unwrap_err(), "The server changed since the request");
        assert_eq!(second.text(), "", "nothing ran on the new login");

        // The same identity check on a hand-made binding whose address equals the current session's but whose
        // login is another one (host): refused too.
        let current = shared.session().unwrap();
        let mut forged = Login::of(&current);
        forged.host = "other.example".into();
        assert!(!forged.is(&current));
        assert!(Login::of(&current).is(&current));
    }

    #[tokio::test]
    async fn busy_from_the_approve_click_until_the_run_ends() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_tests(tokio::runtime::Handle::current());
        let app = services(&shared, dir.path());
        let server: Arc<dyn Server> = Arc::new(Recorder::new());
        shared.set_session_for_tests(Some(server));
        app.before_approval();
        app.approve();
        assert_eq!(app.executing(), 1, "busy as soon as Approve is clicked, before the run starts");
        let req = ScriptRequest { plugin: "demo".into(), script: "true".into(), as_root: false };
        app.run_script(req.clone()).await.unwrap();
        assert_eq!(app.executing(), 0, "the run took the count over and ended");
        // A refused run (no binding) still gives the count back.
        app.approve();
        assert!(app.run_script(req).await.is_err());
        assert_eq!(app.executing(), 0);
        // An instance that ended before starting what was approved.
        app.approve();
        app.forget_approved();
        assert_eq!(app.executing(), 0);
    }

    #[tokio::test]
    async fn database_calls_need_a_login() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::for_tests(tokio::runtime::Handle::current());
        let app = services(&shared, dir.path());
        assert_eq!(app.sql_select("SELECT 1".into(), vec![]).await.unwrap_err(), "Not connected");
        assert_eq!(app.sql_write(WriteBatch::default()).await.unwrap_err(), "Not connected");
    }

    #[test]
    fn exported_names_stay_in_the_folder() {
        assert_eq!(export_name("catalog.sql"), "catalog.sql");
        assert_eq!(export_name("../../evil.sql"), "_.._evil.sql");
        assert_eq!(export_name("a\\b:c"), "a_b_c");
        assert_eq!(export_name("con.txt"), "import-con.txt");
        assert_eq!(export_name(".."), "import-");
        assert_eq!(export_name("x\u{202E}y"), "x_y");
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("ext-exports").join("demo");
        let file = save_export(&folder, "../up.sql", b"data").unwrap();
        assert_eq!(file.parent().unwrap(), folder);
        assert_eq!(std::fs::read(&file).unwrap(), b"data");
        // A second file of that name does not overwrite the first.
        let again = save_export(&folder, "../up.sql", b"new").unwrap();
        assert_eq!(again.file_name().unwrap(), "_up-2.sql");
        assert_eq!(std::fs::read(&file).unwrap(), b"data");
    }
}
