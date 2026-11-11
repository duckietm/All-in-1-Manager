//! What the Manager provides to a running plugin.
//!
//! The runtime (`crate::runtime`) translates every WIT import into a call of [`HostServices`]. The
//! permission checks and approvals (Tasks 5 and 6) sit inside the runtime, *before* these calls: an
//! implementation of this trait may assume the plugin is allowed to make the call.
//!
//! The data types here are plain Rust types (no WIT bindgen types), so the app can implement the
//! trait without depending on the generated bindings.

use std::path::PathBuf;

use async_trait::async_trait;

/// Severity of a plugin log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

/// A script a plugin wants to run on the logged-in server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRequest {
    /// Id of the plugin asking (shown in the approval dialog).
    pub plugin: String,
    pub script: String,
    pub as_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptResult {
    pub exit_code: i32,
    pub output: String,
    pub error: String,
}

/// A database value.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

/// The result of a read query.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
}

/// Database changes applied in one transaction after the user approved them (filled by Task 5).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WriteBatch {
    /// Parameterised statements, in order.
    pub statements: Vec<(String, Vec<SqlValue>)>,
    /// One human readable line per change, for the preview.
    pub summary: Vec<String>,
}

/// The Manager side of the plugin API. Calls come from tokio worker threads, never from the egui
/// thread; the dialog methods must hand the request to the UI and wait for the user's answer.
#[async_trait]
pub trait HostServices: Send + Sync {
    /// A yes/no question to the user.
    async fn ask(&self, question: String) -> bool;
    /// A line of the plugin log (also the plugin's stdout (Info) and stderr (Error)).
    fn log(&self, level: Level, text: &str);
    /// The status line text.
    fn status(&self, text: &str);
    async fn run_script(&self, req: ScriptRequest) -> Result<ScriptResult, String>;
    async fn sql_select(&self, sql: String, params: Vec<SqlValue>) -> Result<Rows, String>;
    async fn sql_write(&self, batch: WriteBatch) -> Result<u64, String>;
    /// `url` passed `crate::net::check_http` (rebuilt from its checked parts). The implementation must not
    /// follow a redirect to another host, must stop reading past `crate::net::MAX_HTTP_BODY` bytes, and
    /// returns a non-2xx status as `Err("HTTP <code>")`.
    async fn http_get(&self, url: &str) -> Result<Vec<u8>, String>;
    /// A line of text from the user; `None` when cancelled.
    async fn prompt_text(&self, title: String, label: String, default: String) -> Option<String>;
    /// An open-file dialog; `extensions` have no dots, empty = every file. `None` when cancelled.
    /// The runtime never shows the path to the plugin.
    async fn pick_open_file(&self, title: String, extensions: Vec<String>) -> Option<PathBuf>;
    /// A save-file dialog; `None` when cancelled. The runtime never shows the path to the plugin.
    async fn pick_save_file(&self, title: String, suggested: String, extensions: Vec<String>) -> Option<PathBuf>;
    /// Opens the Manager's import preview on a copy of a file the user picked (`name` without folders).
    fn open_import_preview(&self, name: String, bytes: Vec<u8>);
}
