//! The sandbox: runs one untrusted plugin component.
//!
//! What a guest can reach is exactly this list; everything else does not exist for it:
//!
//! * its own linear memory, at most [`MAX_MEMORY`] in total over all its memories (a `ResourceLimiter`
//!   refuses to grow past it, so the guest's allocator fails and the guest traps; the host never aborts);
//! * time: each call into the guest (and the instantiation) may take [`CALL_LIMIT`], not counting the time
//!   spent waiting for the user in a host dialog. Two mechanisms enforce it: epoch interruption stops wasm
//!   code (a ticker thread advances the engine's epoch every [`TICK`]; a call gets [`CALL_TICKS`] ticks and
//!   the epoch callback extends the deadline only by the dialog time), and a wall-clock watchdog stops a
//!   call parked in a host wait where no wasm runs (a WASI clock sleep or poll). Past the limit the call
//!   fails with [`PluginError::TimedOut`];
//! * WASI (a `wasm32-wasip2` std guest imports it): no preopened directories (no filesystem), no
//!   environment variables, no arguments, stdin closed, TCP/UDP off and every address denied, no name
//!   lookup. Clocks and random numbers are the host's. stdout and stderr become plugin log lines (Info and
//!   Error);
//! * the plugin log and status line, through one choke point (`LogSink`): every control character (C0,
//!   DEL, C1; tab excepted) and every bidi override or isolate becomes U+FFFD, a newline starts a new
//!   entry, each entry is cut to [`MAX_LOG_ENTRY`] bytes, and one call may emit at most
//!   [`MAX_LOG_ENTRIES`] entries / [`MAX_LOG_BYTES`] bytes, then one [`LOG_TRUNCATED`] entry and nothing
//!   more until the next call;
//! * the `aio:plugin/host` imports: `log` / `status` / `ask` / `prompt-text` / the file dialogs go to
//!   [`HostServices`]; `file-read` / `file-write` only inside `<plugin dir>/data/` (see
//!   [`resolve_private`]), with a quota of [`MAX_DATA_BYTES`] / [`MAX_DATA_ENTRIES`] and files of at most
//!   [`MAX_FILE_READ`]; a `picked-file` wraps the path the user picked (the guest sees only the file
//!   name); `open-import-preview` hands the picked file's bytes to the Manager. The permission-gated
//!   imports (`run-script`, `sql-select`, `sql-write`, `http-get`, `tcp-connect` and the connection
//!   methods) are fail-closed: they return `"permission denied"` until Tasks 5/6 wire the checks.
//!
//! The watchdog uses `tokio::time`: calls must run inside a tokio runtime (the app's).
//!
//! A call that traps, times out or is cancelled (its future dropped) poisons the instance: every later
//! call returns the same error without running guest code. The app reloads the plugin with a fresh
//! [`PluginInstance::load`].
//!
//! Page types: the WIT bindgen types are re-exported as they are ([`UiTree`], [`UiElement`], [`UiEvent`]
//! and the element records). They are already plain owned Rust data (strings, vectors, enums), so a copy
//! into look-alike types would add code without adding safety. Note: a tree comes from the guest and is
//! untrusted; indices may be out of range or form cycles (the renderer must check them).

use std::io;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use tokio::io::AsyncWrite;
use wasmtime::component::{Component, HasSelf, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap, UpdateDeadline};
use wasmtime_wasi::cli::{IsTerminal, StdoutStream};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::aio::plugin::host::{Host, HostPickedFile, HostTcpConnection};
use crate::bindings::aio::plugin::types::{LogLevel, Rows as WitRows, ScriptResult as WitScriptResult, Select, WriteOp};
use crate::bindings::Plugin;
use crate::manifest::Manifest;
use crate::services::{HostServices, Level};

pub use crate::bindings::aio::plugin::types::{
    BannerData, BannerKind, ButtonData, CardData, ChangedData, CheckboxData, InputData, ProgressData, SelectData,
    TableData, UiElement, UiEvent, UiTree,
};

/// Linear memory a plugin may use, summed over all its memories.
pub const MAX_MEMORY: usize = 64 * 1024 * 1024;
/// Table elements a plugin may use, summed over all its tables.
pub const MAX_TABLE_ELEMENTS: usize = 100_000;
/// How often the epoch advances.
pub const TICK: Duration = Duration::from_millis(100);
/// Guest time one call may take.
pub const CALL_LIMIT: Duration = Duration::from_secs(5);
/// [`CALL_LIMIT`] in ticks.
pub const CALL_TICKS: u64 = 50;
/// The largest private or picked file handed to a guest (it must fit in its memory anyway).
pub const MAX_FILE_READ: u64 = 32 * 1024 * 1024;
/// Longest log entry or status text kept (bytes); the rest is cut.
pub const MAX_LOG_ENTRY: usize = 4 * 1024;
/// Log entries (status texts included) one call may emit.
pub const MAX_LOG_ENTRIES: usize = 1_000;
/// Log bytes one call may emit.
pub const MAX_LOG_BYTES: usize = 256 * 1024;
/// The entry that replaces everything past the per-call log budget.
pub const LOG_TRUNCATED: &str = "log truncated by the host";
/// Total size of the files in a plugin's private folder.
pub const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024;
/// Files and folders in a plugin's private folder.
pub const MAX_DATA_ENTRIES: usize = 10_000;
/// Longest relative path accepted by `file-read` / `file-write`.
const MAX_PATH: usize = 255;
/// Most folder levels in a private path.
const MAX_DEPTH: usize = 8;

const DENIED: &str = "permission denied";

/// Why a call into a plugin failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginError {
    /// The guest trapped (a bug, `abort`, out of memory or a cancelled call). The instance is poisoned.
    #[error("the plugin crashed: {0}")]
    Trapped(String),
    /// The guest ran longer than [`CALL_LIMIT`]. The instance is poisoned.
    #[error("the plugin took too long and was stopped")]
    TimedOut,
    /// The plugin asked for something it is not allowed to do.
    #[error("permission denied: {0}")]
    Denied(String),
    /// `on-event` returned an error (the plugin's own message). The instance stays usable.
    #[error("the plugin reported an error: {0}")]
    Failed(String),
}

/// A wasmtime engine for plugins: component model, async calls, epoch interruption, and a background
/// thread that ticks its epoch every [`TICK`]. The thread ends when the last clone of the engine is
/// dropped. Create one per app and share it (it is cheap to clone).
pub fn engine() -> Engine {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.epoch_interruption(true);
    // Calls are async by construction in wasmtime 50 (`Config::async_support` is a deprecated no-op);
    // the stack limits stay at wasmtime's defaults (512 KiB wasm stack inside a 2 MiB fiber stack), so a
    // deep recursion traps instead of overflowing the host.
    let engine = Engine::new(&config).expect("the plugin engine configuration is valid");
    let weak = engine.weak();
    std::thread::Builder::new()
        .name("aio-plugin-epoch".into())
        .spawn(move || loop {
            std::thread::sleep(TICK);
            match weak.upgrade() {
                Some(engine) => engine.increment_epoch(),
                None => break,
            }
        })
        .expect("starting the plugin epoch thread");
    engine
}

/// Memory and table quotas of one instance.
#[derive(Default)]
struct Limits {
    memory: usize,
    table_elements: usize,
}

impl ResourceLimiter for Limits {
    fn memory_growing(&mut self, current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        let total = (self.memory.saturating_sub(current)).saturating_add(desired);
        if total > MAX_MEMORY {
            return Ok(false);
        }
        self.memory = total;
        Ok(true)
    }

    fn table_growing(&mut self, current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        let total = (self.table_elements.saturating_sub(current)).saturating_add(desired);
        if total > MAX_TABLE_ELEMENTS {
            return Ok(false);
        }
        self.table_elements = total;
        Ok(true)
    }

    fn instances(&self) -> usize {
        64
    }

    fn tables(&self) -> usize {
        64
    }

    fn memories(&self) -> usize {
        8
    }
}

/// Time accounting of the current call, shared by the epoch callback, the watchdog and the dialog imports.
#[derive(Default)]
struct CallClock {
    inner: Mutex<ClockState>,
}

#[derive(Default)]
struct ClockState {
    /// Start of the current call (None between calls).
    started: Option<Instant>,
    /// Dialog time of the current call, dialogs that ended.
    waited: Duration,
    /// Start of the dialog the call is waiting on now.
    waiting_since: Option<Instant>,
}

impl CallClock {
    fn lock(&self) -> MutexGuard<'_, ClockState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start(&self) {
        *self.lock() = ClockState { started: Some(Instant::now()), ..ClockState::default() };
    }

    fn stop(&self) {
        *self.lock() = ClockState::default();
    }

    fn begin_wait(&self) {
        self.lock().waiting_since = Some(Instant::now());
    }

    fn end_wait(&self) {
        let mut s = self.lock();
        if let Some(since) = s.waiting_since.take() {
            s.waited += since.elapsed();
        }
    }

    /// Time the current call has used, dialog time excluded; None between calls.
    fn used(&self) -> Option<Duration> {
        let s = self.lock();
        let started = s.started?;
        let now = Instant::now();
        let waited = s.waited + s.waiting_since.map(|w| now.duration_since(w)).unwrap_or_default();
        Some(now.duration_since(started).saturating_sub(waited))
    }
}

/// Replaces control characters (C0, DEL, C1; tab excepted) and bidi overrides / isolates with U+FFFD and
/// cuts the text to [`MAX_LOG_ENTRY`] bytes.
fn sanitise(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_LOG_ENTRY));
    for c in text.chars() {
        let bidi = matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
        let c = if (c.is_control() && c != '\t') || bidi { '\u{FFFD}' } else { c };
        if out.len() + c.len_utf8() > MAX_LOG_ENTRY {
            break;
        }
        out.push(c);
    }
    out
}

#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: usize,
    truncated: bool,
}

/// The only way guest text reaches the log and the status line: sanitised, within the per-call budget.
struct LogSink {
    services: Arc<dyn HostServices>,
    budget: Mutex<Budget>,
}

impl LogSink {
    fn reset(&self) {
        *self.budget.lock().unwrap_or_else(|e| e.into_inner()) = Budget::default();
    }

    /// Takes one entry of `len` bytes from the budget; false (after one truncation entry) once it is spent.
    fn take(&self, len: usize) -> bool {
        let mut b = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        if b.truncated {
            return false;
        }
        if b.entries >= MAX_LOG_ENTRIES || b.bytes + len > MAX_LOG_BYTES {
            b.truncated = true;
            drop(b);
            self.services.log(Level::Warn, LOG_TRUNCATED);
            return false;
        }
        b.entries += 1;
        b.bytes += len;
        true
    }

    /// One log entry per line of `text` (`\n` or `\r\n`; a final newline adds no empty entry).
    fn log(&self, level: Level, text: &str) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        for line in text.split('\n') {
            let entry = sanitise(line.strip_suffix('\r').unwrap_or(line));
            if self.take(entry.len()) {
                self.services.log(level, &entry);
            }
        }
    }

    /// The status line: one entry; a newline is replaced like any control character.
    fn status(&self, text: &str) {
        let entry = sanitise(text);
        if self.take(entry.len()) {
            self.services.status(&entry);
        }
    }
}

/// A file the user picked in a dialog. The guest gets a handle; the path stays here.
pub struct PickedFile {
    path: PathBuf,
    /// Picked with an open dialog: the plugin may read it, never write it.
    /// Picked with a save dialog: the plugin may write it, never read it.
    opened: bool,
}

/// A local TCP connection. Never created yet: `tcp-connect` is fail-closed until Task 6.
pub struct TcpConnection {
    _private: (),
}

/// The store data of one plugin instance.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    services: Arc<dyn HostServices>,
    /// Canonical `<plugin dir>/data`.
    data: PathBuf,
    limits: Limits,
    clock: Arc<CallClock>,
    log: Arc<LogSink>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl HostState {
    /// Awaits a host service that waits for the user, excluding that time from the call's time limit.
    async fn waiting<F: std::future::Future>(&mut self, fut: F) -> F::Output {
        self.clock.begin_wait();
        let out = fut.await;
        self.clock.end_wait();
        out
    }

    fn picked(&self, file: &Resource<PickedFile>) -> Result<&PickedFile, String> {
        self.table.get(file).map_err(|_| "invalid file handle".to_string())
    }
}

/// An I/O error as the guest sees it: the kind only, never a host path.
fn io_text(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "file not found".into(),
        io::ErrorKind::PermissionDenied => "access denied".into(),
        _ => format!("i/o error ({:?})", e.kind()),
    }
}

/// Reads a whole file, refusing anything but a regular file of at most [`MAX_FILE_READ`] bytes.
fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| io_text(&e))?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_FILE_READ {
        return Err(format!("file larger than {} MiB", MAX_FILE_READ / (1024 * 1024)));
    }
    std::fs::read(path).map_err(|e| io_text(&e))
}

/// `true` for names Windows maps to devices (`CON`, `nul.txt`, `COM1`, ...), in any folder.
fn windows_device(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}

/// Maps a plugin's relative path (`/` separated) to a path inside the private folder `data` (which must
/// exist). Refused: empty, too long or too deep paths; absolute paths, drive letters and UNC prefixes;
/// `\`, `:` (alternate streams), NUL and any character outside `A-Z a-z 0-9 . _ -`; empty, `.` and `..`
/// segments; segments ending in `.`; Windows device names. Then every part of the path that exists is
/// canonicalised and must still lie inside `data`, so a symlink or junction cannot lead out.
pub fn resolve_private(data: &Path, path: &str) -> Result<PathBuf, String> {
    const INVALID: &str = "invalid path: use a relative path with / and the characters A-Z a-z 0-9 . _ -";
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(INVALID.into());
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_DEPTH {
        return Err(INVALID.into());
    }
    for s in &segments {
        let ok = !s.is_empty()
            && *s != "."
            && *s != ".."
            && !s.ends_with('.')
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
            && !windows_device(s);
        if !ok {
            return Err(INVALID.into());
        }
    }
    let root = std::fs::canonicalize(data).map_err(|_| "the private folder is missing".to_string())?;
    let mut target = root.clone();
    for s in &segments {
        target.push(s);
        // Belt and braces: the checks above leave only normal components.
        if !matches!(target.components().next_back(), Some(PathComponent::Normal(_))) {
            return Err(INVALID.into());
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            let real = std::fs::canonicalize(&target).map_err(|e| io_text(&e))?;
            if !real.starts_with(&root) {
                return Err("access denied: outside the private folder".into());
            }
        }
    }
    Ok(target)
}

/// Guest stdout or stderr: each line becomes a plugin log line.
struct GuestOutput {
    log: Arc<LogSink>,
    level: Level,
}

impl IsTerminal for GuestOutput {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for GuestOutput {
    fn async_stream(&self) -> Box<dyn AsyncWrite + Send + Sync> {
        Box::new(LineWriter { log: self.log.clone(), level: self.level, pending: Vec::new() })
    }
}

struct LineWriter {
    log: Arc<LogSink>,
    level: Level,
    pending: Vec<u8>,
}

impl LineWriter {
    fn emit(&mut self, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        self.log.log(self.level, &String::from_utf8_lossy(line));
    }

    fn push(&mut self, data: &[u8]) {
        for &b in data {
            if b == b'\n' {
                let line = std::mem::take(&mut self.pending);
                self.emit(&line);
            } else {
                self.pending.push(b);
                if self.pending.len() >= MAX_LOG_ENTRY {
                    let line = std::mem::take(&mut self.pending);
                    self.emit(&line);
                }
            }
        }
    }

    fn finish(&mut self) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.emit(&line);
        }
    }
}

impl AsyncWrite for LineWriter {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        self.get_mut().push(data);
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().finish();
        Poll::Ready(Ok(()))
    }
}

impl Drop for LineWriter {
    fn drop(&mut self) {
        self.finish();
    }
}

impl crate::bindings::aio::plugin::types::Host for HostState {}

impl Host for HostState {
    async fn log(&mut self, level: LogLevel, text: String) {
        let level = match level {
            LogLevel::Debug => Level::Debug,
            LogLevel::Info => Level::Info,
            LogLevel::Warn => Level::Warn,
            LogLevel::Error => Level::Error,
        };
        self.log.log(level, &text);
    }

    async fn ask(&mut self, question: String) -> bool {
        let services = self.services.clone();
        self.waiting(services.ask(question)).await
    }

    async fn status(&mut self, text: String) {
        self.log.status(&text);
    }

    async fn run_script(&mut self, _script: String, _as_root: bool) -> Result<WitScriptResult, String> {
        Err(DENIED.into())
    }

    async fn sql_select(&mut self, _query: Select) -> Result<WitRows, String> {
        Err(DENIED.into())
    }

    async fn sql_write(&mut self, _ops: Vec<WriteOp>) -> Result<u64, String> {
        Err(DENIED.into())
    }

    async fn http_get(&mut self, _url: String) -> Result<Vec<u8>, String> {
        Err(DENIED.into())
    }

    async fn tcp_connect(&mut self, _port: u16) -> Result<Resource<TcpConnection>, String> {
        Err(DENIED.into())
    }

    async fn file_read(&mut self, path: String) -> Result<Vec<u8>, String> {
        let target = resolve_private(&self.data, &path)?;
        read_limited(&target)
    }

    async fn file_write(&mut self, path: String, data: Vec<u8>) -> Result<(), String> {
        // Every check comes before anything is created, so a refused write leaves nothing behind.
        if data.len() as u64 > MAX_FILE_READ {
            return Err(format!("file larger than {} MiB", MAX_FILE_READ / (1024 * 1024)));
        }
        let target = resolve_private(&self.data, &path)?;
        if target.is_dir() {
            return Err("a folder has that name".into());
        }
        check_quota(&self.data, &target, data.len() as u64)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_text(&e))?;
        }
        // Check again now that the folders exist (one of them could be a link made in the meantime).
        let target = resolve_private(&self.data, &path)?;
        if target.is_dir() {
            return Err("a folder has that name".into());
        }
        std::fs::write(&target, data).map_err(|e| io_text(&e))
    }

    async fn prompt_text(&mut self, title: String, label: String, default_value: String) -> Option<String> {
        let services = self.services.clone();
        self.waiting(services.prompt_text(title, label, default_value)).await
    }

    async fn pick_open_file(&mut self, title: String, extensions: Vec<String>) -> Option<Resource<PickedFile>> {
        let services = self.services.clone();
        let path = self.waiting(services.pick_open_file(title, extensions)).await?;
        self.table.push(PickedFile { path, opened: true }).ok()
    }

    async fn pick_save_file(
        &mut self,
        title: String,
        suggested_name: String,
        extensions: Vec<String>,
    ) -> Option<Resource<PickedFile>> {
        let services = self.services.clone();
        let path = self.waiting(services.pick_save_file(title, suggested_name, extensions)).await?;
        self.table.push(PickedFile { path, opened: false }).ok()
    }

    async fn open_import_preview(&mut self, file: Resource<PickedFile>) -> Result<(), String> {
        let picked = self.picked(&file)?;
        if !picked.opened {
            return Err("a file picked for saving cannot be read".into());
        }
        let name = file_name(&picked.path);
        let bytes = std::fs::read(&picked.path).map_err(|e| io_text(&e))?;
        self.services.open_import_preview(name, bytes);
        Ok(())
    }
}

/// Size of the files and number of files and folders under `root` (links are counted, never followed).
fn usage(root: &Path) -> io::Result<(u64, usize)> {
    let (mut bytes, mut entries) = (0u64, 0usize);
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder)? {
            let path = entry?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            entries += 1;
            if meta.is_dir() {
                folders.push(path);
            } else {
                bytes += meta.len();
            }
        }
    }
    Ok((bytes, entries))
}

/// Refuses a write of `len` bytes to `target` (inside `data`) that would take the private folder past
/// [`MAX_DATA_BYTES`] or [`MAX_DATA_ENTRIES`]. A file being replaced counts with its new size only.
fn check_quota(data: &Path, target: &Path, len: u64) -> Result<(), String> {
    let (used, entries) = usage(data).map_err(|e| io_text(&e))?;
    let old = std::fs::symlink_metadata(target).map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0);
    if used.saturating_sub(old) + len > MAX_DATA_BYTES {
        return Err(format!("the private folder is full ({} MiB)", MAX_DATA_BYTES / (1024 * 1024)));
    }
    // The file itself, if new, and every folder still to be created.
    let new_entries = target.ancestors().take_while(|p| *p != data).filter(|p| !p.exists()).count();
    if entries + new_entries > MAX_DATA_ENTRIES {
        return Err(format!("the private folder is full ({MAX_DATA_ENTRIES} files and folders)"));
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl HostPickedFile for HostState {
    async fn name(&mut self, file: Resource<PickedFile>) -> String {
        self.picked(&file).map(|p| file_name(&p.path)).unwrap_or_default()
    }

    async fn read(&mut self, file: Resource<PickedFile>) -> Result<Vec<u8>, String> {
        let picked = self.picked(&file)?;
        if !picked.opened {
            return Err("a file picked for saving cannot be read".into());
        }
        read_limited(&picked.path)
    }

    async fn write(&mut self, file: Resource<PickedFile>, data: Vec<u8>) -> Result<(), String> {
        let picked = self.picked(&file)?;
        if picked.opened {
            return Err("a file picked for opening cannot be written".into());
        }
        std::fs::write(&picked.path, data).map_err(|e| io_text(&e))
    }

    async fn drop(&mut self, file: Resource<PickedFile>) -> wasmtime::Result<()> {
        self.table.delete(file)?;
        Ok(())
    }
}

impl HostTcpConnection for HostState {
    async fn send(&mut self, _conn: Resource<TcpConnection>, _data: Vec<u8>) -> Result<(), String> {
        Err(DENIED.into())
    }

    async fn recv(&mut self, _conn: Resource<TcpConnection>, _max: u32) -> Result<Vec<u8>, String> {
        Err(DENIED.into())
    }

    async fn close(&mut self, _conn: Resource<TcpConnection>) {}

    async fn drop(&mut self, conn: Resource<TcpConnection>) -> wasmtime::Result<()> {
        self.table.delete(conn)?;
        Ok(())
    }
}

/// One loaded plugin. `Send`: run its calls on a tokio worker thread, never on the egui thread.
pub struct PluginInstance {
    store: Store<HostState>,
    plugin: Plugin,
    manifest: Manifest,
    /// The same clock as the store's `HostState::clock`, reachable while the store is borrowed by a call.
    clock: Arc<CallClock>,
    /// Set once a call trapped, timed out or was cancelled.
    poisoned: Option<PluginError>,
    /// True while a call runs; still true at the next call if the previous future was dropped.
    in_call: bool,
}

impl PluginInstance {
    /// Loads the installed plugin folder `dir` (`<root>/<id>/`): compiles `dir/plugin.wasm` and
    /// instantiates it under the limits above. `dir/data/` is created if missing; it is the only folder
    /// `file-read` / `file-write` touch. Compiling is CPU heavy: call this on a worker thread.
    pub async fn load(engine: &Engine, dir: &Path, manifest: Manifest, services: Arc<dyn HostServices>) -> Result<Self> {
        let wasm = std::fs::read(dir.join("plugin.wasm")).with_context(|| format!("reading {}", dir.display()))?;
        let data = dir.join("data");
        std::fs::create_dir_all(&data).with_context(|| format!("creating {}", data.display()))?;
        let dir_real = std::fs::canonicalize(dir)?;
        let data = std::fs::canonicalize(&data)?;
        if data.parent() != Some(dir_real.as_path()) {
            bail!("{} must be a folder inside {}, not a link", data.display(), dir_real.display());
        }

        let component = Component::from_binary(engine, &wasm).map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let mut linker: Linker<HostState> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| anyhow::anyhow!("{e:#}"))?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(|e| anyhow::anyhow!("{e:#}"))?;

        let log = Arc::new(LogSink { services: services.clone(), budget: Mutex::default() });
        let clock = Arc::new(CallClock::default());
        let wasi = WasiCtxBuilder::new()
            .stdout(GuestOutput { log: log.clone(), level: Level::Info })
            .stderr(GuestOutput { log: log.clone(), level: Level::Error })
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .build();
        let state = HostState {
            wasi,
            table: ResourceTable::new(),
            services,
            data,
            limits: Limits::default(),
            clock: clock.clone(),
            log,
        };
        let mut store = Store::new(engine, state);
        store.limiter(|s| &mut s.limits);
        store.epoch_deadline_callback(|ctx| {
            let Some(used) = ctx.data().clock.used() else {
                return Ok(UpdateDeadline::Continue(CALL_TICKS));
            };
            if used >= CALL_LIMIT {
                return Ok(UpdateDeadline::Interrupt);
            }
            let left = (CALL_LIMIT - used).as_millis().div_ceil(TICK.as_millis());
            Ok(UpdateDeadline::Continue((left as u64).max(1)))
        });

        // Instantiation can run guest code (start functions): it is a call like any other.
        arm(&mut store);
        let plugin = watchdog(&clock, Plugin::instantiate_async(&mut store, &component, &linker)).await;
        disarm(&mut store);
        let plugin = plugin.map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(PluginInstance { store, plugin, manifest, clock, poisoned: None, in_call: false })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The plugin's page.
    pub async fn render(&mut self) -> Result<UiTree, PluginError> {
        self.enter()?;
        let result = watchdog(&self.clock, self.plugin.call_render(&mut self.store)).await;
        self.leave(result)
    }

    /// Hands a user action to the plugin. The app renders again afterwards.
    pub async fn event(&mut self, e: UiEvent) -> Result<(), PluginError> {
        self.enter()?;
        let result = watchdog(&self.clock, self.plugin.call_on_event(&mut self.store, &e)).await;
        self.leave(result)?.map_err(PluginError::Failed)
    }

    /// Test hook: the host side of the imports, to call them as a guest would.
    #[cfg(feature = "test-hooks")]
    pub fn host_for_tests(&mut self) -> &mut HostState {
        self.store.data_mut()
    }

    fn enter(&mut self) -> Result<(), PluginError> {
        if self.in_call && self.poisoned.is_none() {
            self.poisoned = Some(PluginError::Trapped("a previous call was cancelled".into()));
        }
        if let Some(e) = &self.poisoned {
            return Err(e.clone());
        }
        self.in_call = true;
        arm(&mut self.store);
        Ok(())
    }

    fn leave<R>(&mut self, result: Result<R, PluginError>) -> Result<R, PluginError> {
        disarm(&mut self.store);
        self.in_call = false;
        result.inspect_err(|e| self.poisoned = Some(e.clone()))
    }
}

fn arm(store: &mut Store<HostState>) {
    let state = store.data();
    state.clock.start();
    state.log.reset();
    store.set_epoch_deadline(CALL_TICKS);
}

fn disarm(store: &mut Store<HostState>) {
    store.data().clock.stop();
}

/// Runs one guest call under the wall-clock limit: [`CALL_LIMIT`] by the call's clock, dialog waits
/// excluded. Epoch interruption cannot stop a call parked in a host wait (a WASI sleep or poll, where no
/// wasm runs); this does, by dropping the call future (wasmtime unwinds the guest's fiber). While wasm
/// runs, the future is not polled, so a busy guest is stopped by the epoch instead.
async fn watchdog<R>(
    clock: &CallClock,
    call: impl std::future::Future<Output = wasmtime::Result<R>>,
) -> Result<R, PluginError> {
    let mut call = std::pin::pin!(call);
    loop {
        let left = CALL_LIMIT.saturating_sub(clock.used().unwrap_or_default());
        if left.is_zero() {
            return Err(PluginError::TimedOut);
        }
        tokio::select! {
            result = &mut call => return result.map_err(classify),
            // While a dialog is open `used` stands still, so this just checks again later.
            _ = tokio::time::sleep(left.max(Duration::from_millis(10))) => {}
        }
    }
}

fn classify(e: wasmtime::Error) -> PluginError {
    if e.downcast_ref::<Trap>() == Some(&Trap::Interrupt) {
        PluginError::TimedOut
    } else {
        PluginError::Trapped(format!("{e:#}"))
    }
}
