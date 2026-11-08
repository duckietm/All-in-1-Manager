//! A database client in the style of HeidiSQL (SqlEditorView.cs, DatabaseLoginDialog.cs): databases and tables on the
//! left; the data of a table (editable when it has a primary key), its structure and a query editor on the right.
//! MariaDB is reached through the SSH session.

mod grid;
pub(crate) mod login;
pub(crate) mod text;

use std::sync::{Arc, Mutex};

use aio_remote::sql_session::{KeyPart, SqlResult, SqlRun, SqlSession, SqlTableInfo, MAX_QUERY_ROWS, PAGE_SIZE};
use aio_remote::Server;
use eframe::egui::{self, RichText};
use tokio_util::sync::CancellationToken;

use crate::gui::dialogs::Choice;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{Fixed, Palette};
use crate::gui::{widgets, Page, Request, Shared, View};
use crate::i18n::{t, tf};
use grid::{Grid, GridAction};
use login::{Login, Prompt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Data,
    Structure,
    Query,
}

#[derive(Default)]
struct Status {
    text: String,
    error: bool,
}

impl Status {
    fn new(text: &str) -> Self {
        Status { text: text.to_string(), error: false }
    }

    fn error(text: String) -> Self {
        Status { text, error: true }
    }
}

struct DatabaseNode {
    name: String,
    expanded: bool,
    loaded: bool,
    tables: Option<Vec<SqlTableInfo>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selected {
    Database(String),
    Table(String, String),
}

#[derive(Debug, Clone)]
struct TableRef {
    database: String,
    table: String,
    is_view: bool,
}

enum ResultContent {
    Grid(Grid),
    Messages(String),
}

struct ResultTab {
    header: String,
    content: ResultContent,
}

struct State {
    /// Raised when the SQL session closes: a task of an older session changes nothing.
    generation: u64,
    session: Option<Arc<SqlSession>>,
    session_for: Option<usize>,
    opening: bool,
    title: String,
    subtitle: String,
    login_visible: bool,
    prompt: Option<Prompt>,
    nodes: Vec<DatabaseNode>,
    database_names: Vec<String>,
    database: Option<String>,
    selected: Option<Selected>,
    tab: Tab,
    // Data tab
    filter: String,
    data_status: Status,
    table: Option<TableRef>,
    primary_key: Vec<String>,
    data: Option<Grid>,
    offset: i64,
    order_by: Option<String>,
    descending: bool,
    data_busy: bool,
    // Structure tab
    columns: Option<Grid>,
    create: String,
    // Query tab
    editor: String,
    results: Vec<ResultTab>,
    result_tab: usize,
    query_status: Status,
    query_cancel: Option<CancellationToken>,
    query_finished: bool,
    select_line: Option<(usize, usize)>,
    next_grid: u64,
}

impl State {
    fn new() -> Self {
        State {
            generation: 0,
            session: None,
            session_for: None,
            opening: false,
            title: String::new(),
            subtitle: String::new(),
            login_visible: false,
            prompt: None,
            nodes: Vec::new(),
            database_names: Vec::new(),
            database: None,
            selected: None,
            tab: Tab::Query,
            filter: String::new(),
            data_status: Status::new(t("Select a table on the left.")),
            table: None,
            primary_key: Vec::new(),
            data: None,
            offset: 0,
            order_by: None,
            descending: false,
            data_busy: false,
            columns: None,
            create: String::new(),
            editor: String::new(),
            results: Vec::new(),
            result_tab: 0,
            query_status: Status::new(t("F9 or Ctrl+Enter runs the selected text, or everything when nothing is selected.")),
            query_cancel: None,
            query_finished: false,
            select_line: None,
            next_grid: 1,
        }
    }

    fn grid(&mut self, result: SqlResult, editable: bool, server_sort: bool) -> Grid {
        self.next_grid += 1;
        Grid::new(self.next_grid, result, editable, server_sort)
    }

    /// CloseSessionAsync: the database connections and the tunnel close; the SSH login stays.
    fn close(&mut self, shared: &Shared) {
        if let Some(cancel) = self.query_cancel.take() {
            cancel.cancel();
        }
        if let Some(session) = self.session.take() {
            // Closed inside the runtime, where the connections can say goodbye.
            shared.spawn(async move {
                if let Ok(session) = Arc::try_unwrap(session) {
                    session.close().await;
                }
            });
        }
        let editor = std::mem::take(&mut self.editor);
        let generation = self.generation + 1;
        let next_grid = self.next_grid;
        *self = State::new();
        self.editor = editor;
        self.generation = generation;
        self.next_grid = next_grid;
    }
}

fn error_text(error: &anyhow::Error) -> String {
    error.to_string()
}

pub struct SqlEditorView {
    state: Arc<Mutex<State>>,
}

/// What the tasks share: the state and the window.
#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl Ctl {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Changes the state when the session is still the one the task started with.
    fn with<R>(&self, generation: u64, change: impl FnOnce(&mut State) -> R) -> Option<R> {
        let result = {
            let mut state = self.lock();
            (state.generation == generation).then(|| change(&mut state))
        };
        self.shared.ctx.request_repaint();
        result
    }

    fn session(&self) -> Option<(u64, Arc<SqlSession>)> {
        let state = self.lock();
        state.session.clone().map(|s| (state.generation, s))
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, task: impl FnOnce(Ctl) -> F) {
        self.shared.spawn(task(self.clone()));
    }

    // ---- Session ------------------------------------------------------------------------------------------------

    /// Logs in with the saved MariaDB root account of this server, or asks for an account.
    async fn open_session(self, ask_first: bool) {
        let Some(server) = self.shared.session() else { return };
        let (generation, old) = {
            let mut state = self.lock();
            if state.opening {
                return;
            }
            state.opening = true;
            (state.generation, state.session.take())
        };
        if let Some(old) = old {
            if let Ok(old) = Arc::try_unwrap(old) {
                old.close().await;
            }
        }
        self.open_inner(server, ask_first, generation).await;
        self.with(generation, |s| s.opening = false);
    }

    async fn open_inner(&self, server: Arc<dyn Server>, ask_first: bool, generation: u64) {
        let host = server.login().host.clone();
        let saved = aio_core::config_dir::load_database_credentials(&host);
        let mut login = Login {
            user: saved.as_ref().map(|s| s.root_user.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into()),
            password: saved.as_ref().map(|s| s.root_password.clone()).unwrap_or_default(),
            port: saved.as_ref().map(|s| s.port).filter(|p| *p > 0).unwrap_or(3306),
        };
        let mut problem = saved.is_none().then(|| t("No saved MariaDB password for this server on this computer: enter an account.").to_string());
        let mut ask = ask_first || saved.is_none();
        let session = loop {
            if ask {
                let (prompt, answer) = Prompt::new(&host, &login, problem.take());
                if self.with(generation, |s| s.prompt = Some(prompt)).is_none() {
                    return;
                }
                match answer.await.ok().flatten() {
                    Some(entered) => login = entered,
                    None => {
                        self.with(generation, |s| {
                            s.login_visible = true;
                            s.subtitle = t("Not logged in to MariaDB. Click Log in to MariaDB.").into();
                        });
                        return;
                    }
                }
            }
            self.with(generation, |s| s.subtitle = tf("Connecting to MariaDB as {user} ...", &[("user", &login.user)]));
            let opened = if server.is_dry_run() {
                // The recorder has no database behind its port forward.
                Err(anyhow::anyhow!(t("Dry run: there is no MariaDB to connect to.").to_string()))
            } else {
                SqlSession::open(&*server, &login.user, &login.password, login.port).await
            };
            match opened {
                Ok(session) => break session,
                Err(error) => {
                    let message = error_text(&error);
                    problem = Some(if text::is_access_denied(&message) {
                        tf("MariaDB refused {user}: {message}", &[("user", &login.user), ("message", &message)])
                    } else {
                        tf("Could not reach MariaDB on 127.0.0.1:{port} of the server: {message}", &[("port", &login.port.to_string()), ("message", &message)])
                    });
                    ask = true;
                }
            }
        };
        let session = Arc::new(session);
        let subtitle = tf(
            "{user}@127.0.0.1 · {version} · through the SSH tunnel; nothing is opened to the internet.",
            &[("user", session.user()), ("version", session.server_version())],
        );
        let kept = self.with(generation, |s| {
            s.session = Some(session.clone());
            s.login_visible = false;
            s.subtitle = subtitle;
        });
        if kept.is_none() {
            // The SSH login changed meanwhile.
            if let Ok(session) = Arc::try_unwrap(session) {
                session.close().await;
            }
            return;
        }
        drop(session);
        self.load_databases(saved.map(|s| s.database).filter(|d| !d.is_empty())).await;
    }

    // ---- Database tree ------------------------------------------------------------------------------------------

    async fn load_databases(&self, select: Option<String>) {
        let Some((generation, session)) = self.session() else { return };
        match session.databases().await {
            Ok(databases) => {
                let expand = self.with(generation, |s| {
                    s.nodes = databases.iter().map(|d| DatabaseNode { name: d.clone(), expanded: false, loaded: false, tables: None }).collect();
                    let current = s.database.clone();
                    s.database = current.filter(|c| databases.contains(c)).or_else(|| select.filter(|c| databases.contains(c)));
                    s.database_names = databases;
                    s.database.clone()
                });
                if let Some(Some(chosen)) = expand {
                    self.expand(&chosen).await;
                }
            }
            Err(error) => {
                self.with(generation, |s| s.query_status = Status::error(tf("Loading the databases failed: {message}", &[("message", &error_text(&error))])));
            }
        }
    }

    /// Opens a database in the tree; its tables are read the first time.
    async fn expand(&self, database: &str) {
        let Some((generation, session)) = self.session() else { return };
        let load = self.with(generation, |s| match s.nodes.iter_mut().find(|n| n.name == database) {
            Some(node) => {
                node.expanded = true;
                let load = !node.loaded;
                node.loaded = true;
                load
            }
            None => false,
        });
        if load != Some(true) {
            return;
        }
        match session.tables(database).await {
            Ok(tables) => {
                self.with(generation, |s| {
                    if let Some(node) = s.nodes.iter_mut().find(|n| n.name == database) {
                        node.tables = Some(tables);
                    }
                });
            }
            Err(error) => {
                self.with(generation, |s| {
                    if let Some(node) = s.nodes.iter_mut().find(|n| n.name == database) {
                        node.loaded = false;
                    }
                    s.query_status = Status::error(tf("Loading the tables of {database} failed: {message}", &[("database", database), ("message", &error_text(&error))]));
                });
            }
        }
    }

    // ---- Data tab -----------------------------------------------------------------------------------------------

    async fn open_table(self, database: String, table: SqlTableInfo) {
        let Some((generation, session)) = self.session() else { return };
        self.with(generation, |s| {
            s.table = Some(TableRef { database: database.clone(), table: table.name.clone(), is_view: table.is_view });
            s.offset = 0;
            s.order_by = None;
            s.descending = false;
            s.filter.clear();
            s.columns = None;
            s.create.clear();
        });
        let key = if table.is_view { Ok(Vec::new()) } else { session.primary_key(&database, &table.name).await };
        let structure = self.with(generation, |s| {
            match key {
                Ok(key) => s.primary_key = key,
                Err(error) => {
                    s.primary_key = Vec::new();
                    s.data_status = Status::error(tf("Reading the primary key failed: {message}", &[("message", &error_text(&error))]));
                }
            }
            if s.tab == Tab::Structure {
                true
            } else {
                s.tab = Tab::Data;
                false
            }
        });
        match structure {
            None => return,
            Some(true) => self.load_structure().await,
            Some(false) => {}
        }
        self.load_data().await;
    }

    async fn load_data(&self) {
        let Some((generation, session)) = self.session() else { return };
        let Some(Some((table, filter, order_by, descending, offset))) = self.with(generation, |s| {
            let table = s.table.clone()?;
            if s.data_busy {
                return None;
            }
            s.data_busy = true;
            s.data_status = Status::new(&tf("Loading {database}.{table} ...", &[("database", &table.database), ("table", &table.table)]));
            Some((table, s.filter.clone(), s.order_by.clone(), s.descending, s.offset))
        }) else {
            return;
        };
        let result = session.table_data(&table.database, &table.table, &filter, order_by.as_deref(), descending, offset, PAGE_SIZE).await;
        self.with(generation, |s| {
            match result {
                Ok(result) => {
                    let columns = result.columns.clone().unwrap_or_default();
                    let editable = !s.primary_key.is_empty() && s.primary_key.iter().all(|k| columns.iter().any(|c| &c.name == k));
                    let status = text::data_status(&table.database, &table.table, result.rows.len(), offset, order_by.as_deref(), descending, result.elapsed.as_secs_f64(), editable, table.is_view);
                    s.data = Some(s.grid(result, editable, true));
                    s.data_status = Status::new(&status);
                }
                Err(error) => s.data_status = Status::error(tf("Loading {table} failed: {message}", &[("table", &table.table), ("message", &error_text(&error))])),
            }
            s.data_busy = false;
        });
    }

    fn key_of(state: &State, grid: &Grid, row: usize) -> Option<Vec<KeyPart>> {
        let values = grid.row(row)?;
        state.primary_key.iter().map(|k| grid.columns().iter().position(|c| &c.name == k).map(|i| (k.clone(), values[i].clone()))).collect()
    }

    async fn save_cell(self, grid_id: u64, row: usize, column: usize, value: Option<String>) {
        let Some((generation, session)) = self.session() else { return };
        let Some(Some((table, name, key))) = self.with(generation, |s| {
            let table = s.table.clone()?;
            let grid = s.data.as_ref().filter(|g| g.id == grid_id)?;
            let name = grid.columns().get(column)?.name.clone();
            let key = Self::key_of(s, grid, row)?;
            Some((table, name, key))
        }) else {
            return;
        };
        let changed = session.update_cell(&table.database, &table.table, &name, value.as_deref(), &key).await;
        self.with(generation, |s| match changed {
            Ok(changed) => {
                if let Some(grid) = s.data.as_mut().filter(|g| g.id == grid_id) {
                    grid.set_value(row, column, value.clone());
                }
                s.data_status = Status::new(&if changed > 0 {
                    let shown = match &value {
                        None => "NULL".to_string(),
                        Some(v) => format!("'{}'", text::shorten(v)),
                    };
                    let keys = key
                        .iter()
                        .map(|(c, v)| format!("{c} = {}", aio_remote::sql_session::display(v, None, 64).unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    tf("{column} set to {value} in {table} ({key}).", &[("column", &name), ("value", &shown), ("table", &table.table), ("key", &keys)])
                } else {
                    t("No row changed: the value was the same, or the row is gone (Refresh).").to_string()
                });
            }
            Err(error) => s.data_status = Status::error(tf("Saving {column} failed: {message}", &[("column", &name), ("message", &error_text(&error))])),
        });
    }

    async fn delete_rows(self) {
        let Some((generation, session)) = self.session() else { return };
        let check = self.with(generation, |s| {
            let (Some(table), Some(grid)) = (s.table.clone(), s.data.as_ref()) else { return None };
            let rows = grid.selected_rows();
            if rows.is_empty() {
                s.data_status = Status::new(t("Select the rows to delete first (Ctrl/Shift+click selects more)."));
                return None;
            }
            let keys: Option<Vec<Vec<KeyPart>>> =
                if s.primary_key.is_empty() || !grid.editable { None } else { rows.iter().map(|&r| Self::key_of(s, grid, r)).collect() };
            match keys {
                Some(keys) => Some((table, keys)),
                None => {
                    s.data_status = Status::error(t("This table has no primary key: delete rows with a query.").into());
                    None
                }
            }
        });
        let Some(Some((table, keys))) = check else { return };
        let count = keys.len();
        let heading = if count == 1 { t("Delete 1 row?").to_string() } else { tf("Delete {count} rows?", &[("count", &count.to_string())]) };
        let message = tf(
            "The selected rows are deleted from {database}.{table}. This cannot be undone (make a backup with Tools → Create full database backup).",
            &[("database", &table.database), ("table", &table.table)],
        );
        let options = vec![Choice::new(t("Delete"), t("Delete the rows."), true).primary(), Choice::new(t("Cancel"), t("Change nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &heading, &message, false, options).await {
            return;
        }
        match session.delete_rows(&table.database, &table.table, &keys).await {
            Ok(deleted) => {
                self.load_data().await;
                let text = if deleted == 1 {
                    tf("1 row deleted from {table}.", &[("table", &table.table)])
                } else {
                    tf("{count} rows deleted from {table}.", &[("count", &deleted.to_string()), ("table", &table.table)])
                };
                self.with(generation, |s| s.data_status = Status::new(&text));
            }
            Err(error) => {
                self.with(generation, |s| s.data_status = Status::error(tf("Deleting failed: {message}", &[("message", &error_text(&error))])));
            }
        }
    }

    // ---- Structure tab ------------------------------------------------------------------------------------------

    async fn load_structure(&self) {
        let Some((generation, session)) = self.session() else { return };
        let Some(Some(table)) = self.with(generation, |s| s.table.clone().filter(|_| s.columns.is_none())) else { return };
        let outcome = async {
            let columns = session.columns(&table.database, &table.table).await?;
            self.with(generation, |s| s.columns = Some(s.grid(columns, false, false)));
            session.create_statement(&table.database, &table.table).await
        }
        .await;
        self.with(generation, |s| {
            s.create = match outcome {
                Ok(create) => create,
                Err(error) => tf("-- Reading the structure of {table} failed: {message}", &[("table", &table.table), ("message", &error_text(&error))]),
            }
        });
    }

    // ---- Query tab ----------------------------------------------------------------------------------------------

    async fn run_query(self, script: String) {
        let Some((generation, session)) = self.session() else { return };
        let cancel = CancellationToken::new();
        let Some(Some(database)) = self.with(generation, |s| {
            if s.query_cancel.is_some() {
                return None;
            }
            if script.trim().is_empty() {
                s.query_status = Status::new(t("Type a query first."));
                return None;
            }
            s.query_cancel = Some(cancel.clone());
            s.query_finished = false;
            s.query_status = Status::new(t("Running ..."));
            Some(s.database.clone())
        }) else {
            return;
        };
        // A progress line posted just before the end must not replace the outcome.
        let progress_ctl = self.clone();
        let progress = move |n: usize, total: usize| {
            progress_ctl.with(generation, |s| {
                if !s.query_finished && total > 1 {
                    s.query_status = Status::new(&tf("Running statement {n} of {total} ...", &[("n", &n.to_string()), ("total", &total.to_string())]));
                }
            });
        };
        let run = session.run_script(&script, database.as_deref(), MAX_QUERY_ROWS, Some(&progress), &cancel).await;
        match run {
            Ok(run) => {
                self.with(generation, |s| {
                    s.query_finished = true;
                    Self::show_results(s, run);
                });
                if let Ok(now) = session.current_database().await {
                    let reload = self.with(generation, |s| {
                        if now == database {
                            return None;
                        }
                        // CREATE DATABASE or DROP DATABASE changes the list.
                        let reload = now.as_ref().filter(|n| !s.database_names.contains(n)).cloned();
                        s.database = now.clone();
                        reload
                    });
                    if let Some(Some(now)) = reload {
                        self.load_databases(Some(now.clone())).await;
                        self.with(generation, |s| s.database = Some(now));
                    }
                }
            }
            Err(error) => {
                self.with(generation, |s| {
                    s.query_finished = true;
                    s.query_status = Status::error(tf("The query failed: {message}", &[("message", &error_text(&error))]));
                });
            }
        }
        self.with(generation, |s| s.query_cancel = None);
    }

    fn show_results(s: &mut State, run: SqlRun) {
        let messages = text::run_messages(&run);
        let mut tabs = Vec::new();
        let mut number = 0;
        for result in run.results {
            if result.columns.is_none() {
                continue;
            }
            number += 1;
            let header = text::result_header(number, result.rows.len(), result.truncated);
            let grid = s.grid(result, false, false);
            tabs.push(ResultTab { header, content: ResultContent::Grid(grid) });
        }
        let first = messages.lines().next().unwrap_or_default().to_string();
        let header = if run.error.is_none() { t("Messages") } else { t("Messages ⚠") };
        tabs.push(ResultTab { header: header.to_string(), content: ResultContent::Messages(messages) });
        s.result_tab = if run.error.is_some() || number == 0 { tabs.len() - 1 } else { 0 };
        s.results = tabs;
        let status = match &run.error {
            None => first,
            Some(error) => format!("{first}  {error}"),
        };
        s.query_status = Status { text: status, error: run.error.is_some() && !run.cancelled };
        if run.error.is_some() && !run.cancelled {
            s.select_line = text::line_range(&s.editor, run.error_line);
        }
    }

    async fn open_script(self) {
        let Some(file) = rfd::AsyncFileDialog::new()
            .set_title(t("Open an SQL file"))
            .add_filter(t("SQL files"), &["sql"])
            .add_filter(t("All files"), &["*"])
            .pick_file()
            .await
        else {
            return;
        };
        let path = file.path().to_path_buf();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let length = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
        let generation = self.lock().generation;
        // Big dumps belong in Tools → Import full database; a text box of many megabytes is unusable.
        if length > 5 << 20 {
            let size = text::size(Some(length)).unwrap_or_default();
            self.with(generation, |s| {
                s.query_status = Status::error(tf("{name} is {size}: import big dumps with Tools → Import full database.", &[("name", &name), ("size", &size)]))
            });
            return;
        }
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&bytes);
                let script = String::from_utf8_lossy(bytes).into_owned();
                self.with(generation, |s| {
                    s.editor = script;
                    s.tab = Tab::Query;
                    s.query_status = Status::new(&tf("Opened {name}. F9 runs it.", &[("name", &name)]));
                });
            }
            Err(error) => {
                self.with(generation, |s| s.query_status = Status::error(error.to_string()));
            }
        }
    }

    async fn save_script(self) {
        let Some(file) = rfd::AsyncFileDialog::new().set_title(t("Save the query as")).set_file_name("query.sql").add_filter(t("SQL file"), &["sql"]).save_file().await else {
            return;
        };
        let path = file.path().to_path_buf();
        let (generation, script) = {
            let s = self.lock();
            (s.generation, s.editor.clone())
        };
        let status = match tokio::fs::write(&path, script).await {
            Ok(()) => Status::new(&tf("Saved {path}.", &[("path", &path.display().to_string())])),
            Err(error) => Status::error(error.to_string()),
        };
        self.with(generation, |s| s.query_status = status);
    }

    async fn export_csv(self, csv: Vec<u8>, rows: usize, name: String, from_query: bool) {
        let suggested = format!("{name}-{}.csv", chrono::Local::now().format("%Y%m%d-%H%M%S"));
        let Some(file) = rfd::AsyncFileDialog::new().set_title(t("Export as CSV")).set_file_name(&suggested).add_filter(t("CSV file"), &["csv"]).save_file().await else {
            return;
        };
        let path = file.path().to_path_buf();
        let generation = self.lock().generation;
        let status = match tokio::fs::write(&path, csv).await {
            Ok(()) => Status::new(&tf("{rows} rows exported to {path}.", &[("rows", &text::n0(rows as i64)), ("path", &path.display().to_string())])),
            Err(error) => Status::error(error.to_string()),
        };
        self.with(generation, |s| if from_query { s.query_status = status } else { s.data_status = status });
    }
}

impl SqlEditorView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::new())) }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

    /// ConnectedAsync: a new SSH login opens a new SQL session when the page is shown.
    fn connect_if_needed(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        let id = server_id(&server);
        {
            let mut state = self.state.lock().unwrap();
            if state.session_for == Some(id) {
                return;
            }
            state.close(shared);
            state.session_for = Some(id);
            state.title = tf("MariaDB on {host} (through SSH as {user})", &[("host", &server.login().host), ("user", &server.login().user)]);
        }
        self.ctl(shared).spawn(|ctl| ctl.open_session(false));
    }
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

fn small_primary(text: &str) -> egui::Button<'static> {
    widgets::primary_button(text).min_size(egui::vec2(0.0, 28.0))
}

fn status_label(ui: &mut egui::Ui, palette: &Palette, status: &Status) {
    ui.add(egui::Label::new(RichText::new(&status.text).size(12.0).color(if status.error { palette.error_text } else { palette.muted })).wrap());
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(8).corner_radius(10)
}

/// What a frame of the page asks the tasks to do.
enum Action {
    Login,
    Refresh,
    Expand(String),
    OpenTable(String, SqlTableInfo),
    LoadData,
    Structure,
    Grid(u64, GridAction),
    Delete,
    Run(String),
    Stop,
    OpenScript,
    SaveScript,
    Export(Vec<u8>, usize, String, bool),
}

const EDITOR_ID: &str = "sql-query-editor";

impl View for SqlEditorView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.connect_if_needed(shared);
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        {
            let mut guard = self.state.lock().unwrap();
            let s = &mut *guard;
            if let Some(prompt) = s.prompt.as_mut() {
                if prompt.show(&ctx, palette) {
                    s.prompt = None;
                }
            }
            egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 18 }).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                header(ui, palette, s, shared, &mut actions);
                ui.add_space(10.0);
                body(ui, palette, s, &mut actions);
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open_session(true)),
                Action::Refresh => ctl.spawn(|c| async move { c.load_databases(None).await }),
                Action::Expand(database) => ctl.spawn(|c| async move { c.expand(&database).await }),
                Action::OpenTable(database, table) => ctl.spawn(|c| c.open_table(database, table)),
                Action::LoadData => ctl.spawn(|c| async move { c.load_data().await }),
                Action::Structure => ctl.spawn(|c| async move { c.load_structure().await }),
                Action::Grid(_, GridAction::SortServer(column)) => {
                    // Sort the whole table on the server, not just this page.
                    {
                        let mut s = self.state.lock().unwrap();
                        if s.data_busy {
                            continue;
                        }
                        s.descending = s.order_by.as_deref() == Some(column.as_str()) && !s.descending;
                        s.order_by = Some(column);
                        s.offset = 0;
                    }
                    ctl.spawn(|c| async move { c.load_data().await });
                }
                Action::Grid(id, GridAction::Save { row, column, value }) => ctl.spawn(|c| c.save_cell(id, row, column, value)),
                Action::Grid(_, GridAction::Delete) | Action::Delete => ctl.spawn(|c| c.delete_rows()),
                Action::Run(script) => ctl.spawn(|c| c.run_query(script)),
                Action::Stop => {
                    if let Some(cancel) = self.state.lock().unwrap().query_cancel.as_ref() {
                        cancel.cancel();
                    }
                }
                Action::OpenScript => ctl.spawn(|c| c.open_script()),
                Action::SaveScript => ctl.spawn(|c| c.save_script()),
                Action::Export(csv, rows, name, from_query) => ctl.spawn(|c| c.export_csv(csv, rows, name, from_query)),
            }
        }
    }

    fn busy(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.query_cancel.is_some() || s.data_busy
    }

    fn session_changed(&mut self, shared: &Shared) {
        self.state.lock().unwrap().close(shared);
    }
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(Page::Dashboard));
            }
            if ui.add(small(t("Refresh"))).clicked() {
                actions.push(Action::Refresh);
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(&s.title).size(13.0).strong());
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    egui::SidePanel::left("sql-tree")
        .resizable(true)
        .default_width(260.0)
        .width_range(160.0..=600.0)
        .frame(frame(palette).outer_margin(egui::Margin { left: 0, right: 6, top: 0, bottom: 0 }))
        .show_inside(ui, |ui| {
            egui::ScrollArea::both().id_salt("sql-tree-scroll").auto_shrink([false, false]).show(ui, |ui| tree(ui, palette, s, actions));
        });
    egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            for (tab, label) in [(Tab::Data, t("Data")), (Tab::Structure, t("Structure")), (Tab::Query, t("Query"))] {
                if ui.selectable_label(s.tab == tab, RichText::new(label).size(16.0)).clicked() && s.tab != tab {
                    s.tab = tab;
                    if tab == Tab::Structure {
                        actions.push(Action::Structure);
                    }
                }
            }
        });
        ui.separator();
        match s.tab {
            Tab::Data => data_tab(ui, palette, s, actions),
            Tab::Structure => structure_tab(ui, palette, s, actions),
            Tab::Query => query_tab(ui, palette, s, actions),
        }
    });
}

fn tree_row(ui: &mut egui::Ui, palette: &Palette, icon: Icon, key: &str, name: &str, note: Option<&str>, selected: bool) -> egui::Response {
    let inner = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        icons::show(ui, icon, 13.0, Fixed::brand());
        ui.add(egui::Label::new(RichText::new(name).size(12.5).color(palette.text)).selectable(false));
        if let Some(note) = note {
            ui.add(egui::Label::new(RichText::new(note).size(11.0).color(palette.subtle)).selectable(false));
        }
    });
    let rect = inner.response.rect.expand(2.0);
    let response = ui.interact(rect, ui.id().with(("sql-tree-row", key)), egui::Sense::click());
    if selected || response.hovered() {
        let color = if selected { palette.selected } else { palette.hover };
        ui.painter().rect_filled(rect, 4.0, crate::gui::theme::with_alpha(color, 0x90));
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn tree(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let mut select = None;
    let mut toggle = None;
    for node in &s.nodes {
        ui.horizontal(|ui| {
            let arrow = if node.expanded { "⏷" } else { "⏵" };
            if ui.add(egui::Label::new(RichText::new(arrow).size(11.0).color(palette.muted)).sense(egui::Sense::click()).selectable(false)).clicked() {
                toggle = Some(node.name.clone());
            }
            let selected = s.selected == Some(Selected::Database(node.name.clone()));
            let response = tree_row(ui, palette, Icon::Database, &node.name, &node.name, None, selected);
            if response.clicked() {
                select = Some((Selected::Database(node.name.clone()), None));
            }
            if response.double_clicked() {
                toggle = Some(node.name.clone());
            }
        });
        if node.expanded {
            ui.indent(("sql-db", &node.name), |ui| match &node.tables {
                None => {
                    ui.label(RichText::new("…").size(12.5).color(palette.subtle));
                }
                Some(tables) => {
                    for table in tables {
                        let note = if table.is_view { Some(t("view").to_string()) } else { text::size(table.bytes) };
                        let selected = s.selected == Some(Selected::Table(node.name.clone(), table.name.clone()));
                        let icon = if table.is_view { Icon::Swap } else { Icon::File };
                        let key = format!("{}\u{1}{}", node.name, table.name);
                        if tree_row(ui, palette, icon, &key, &table.name, note.as_deref(), selected).clicked() {
                            select = Some((Selected::Table(node.name.clone(), table.name.clone()), Some(table.clone())));
                        }
                    }
                }
            });
        }
    }
    if let Some(name) = toggle {
        if let Some(node) = s.nodes.iter_mut().find(|n| n.name == name) {
            if node.expanded {
                node.expanded = false;
            } else {
                actions.push(Action::Expand(name));
            }
        }
    }
    if let Some((selected, table)) = select {
        if s.selected.as_ref() != Some(&selected) {
            match (&selected, table) {
                (Selected::Table(database, _), Some(table)) => {
                    if s.database_names.contains(database) {
                        s.database = Some(database.clone());
                    }
                    actions.push(Action::OpenTable(database.clone(), table));
                }
                (Selected::Database(database), _) => s.database = Some(database.clone()),
                _ => {}
            }
            s.selected = Some(selected);
        }
    }
}

fn panel_frame(top: i8, bottom: i8) -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top, bottom })
}

fn data_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    egui::TopBottomPanel::top("sql-data-toolbar").frame(panel_frame(4, 8)).show_separator_line(false).show_inside(ui, |ui| {
        let enabled = !s.data_busy;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui.add_enabled(enabled, small(t("Export CSV"))).clicked() {
                if let Some(grid) = &s.data {
                    if let Some(csv) = aio_remote::sql_session::csv_bytes(&grid.result) {
                        let name = s.table.as_ref().map(|t| t.table.clone()).unwrap_or_else(|| "data".into());
                        actions.push(Action::Export(csv, grid.result.rows.len(), name, false));
                    }
                }
            }
            if ui.add_enabled(enabled, small(t("Delete rows"))).clicked() {
                actions.push(Action::Delete);
            }
            if ui.add_enabled(enabled, small(t("Refresh"))).clicked() {
                actions.push(Action::LoadData);
            }
            let full_page = s.data.as_ref().is_some_and(|d| d.result.rows.len() == PAGE_SIZE as usize);
            if ui.add_enabled(enabled && full_page, small(t("Next ▶"))).clicked() {
                s.offset += PAGE_SIZE as i64;
                actions.push(Action::LoadData);
            }
            if ui.add_enabled(enabled && s.offset > 0, small(t("◀ Previous"))).clicked() {
                s.offset = (s.offset - PAGE_SIZE as i64).max(0);
                actions.push(Action::LoadData);
            }
            if ui.add_enabled(enabled, small(t("Apply filter"))).clicked() {
                s.offset = 0;
                actions.push(Action::LoadData);
            }
            let filter = ui.add(
                egui::TextEdit::singleline(&mut s.filter)
                    .hint_text(t("Filter: a WHERE condition, such as id > 100 AND username LIKE 'a%'"))
                    .font(egui::FontId::monospace(12.5))
                    .desired_width(ui.available_width()),
            );
            if filter.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                s.offset = 0;
                actions.push(Action::LoadData);
            }
        });
    });
    egui::TopBottomPanel::bottom("sql-data-status").frame(panel_frame(8, 0)).show_separator_line(false).show_inside(ui, |ui| {
        status_label(ui, palette, &s.data_status);
    });
    egui::CentralPanel::default().frame(egui::Frame::new()).show_inside(ui, |ui| {
        let order = s.order_by.clone().unwrap_or_default();
        let descending = s.descending;
        if let Some(grid) = s.data.as_mut() {
            if let Some(action) = grid.show(ui, palette, Some((&order, descending))) {
                actions.push(Action::Grid(grid.id, action));
            }
        }
    });
}

fn structure_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let half = (ui.available_height() / 2.0).max(80.0);
    egui::TopBottomPanel::top("sql-structure-columns").resizable(true).default_height(half).frame(panel_frame(8, 6)).show_inside(ui, |ui| {
        if let Some(grid) = s.columns.as_mut() {
            if let Some(action) = grid.show(ui, palette, None) {
                actions.push(Action::Grid(grid.id, action));
            }
        }
    });
    egui::CentralPanel::default().frame(panel_frame(6, 0)).show_inside(ui, |ui| {
        egui::ScrollArea::both().id_salt("sql-create").auto_shrink([false, false]).show(ui, |ui| {
            let mut create = s.create.as_str();
            let mut layouter = no_wrap(palette, 12.5);
            ui.add(egui::TextEdit::multiline(&mut create).font(egui::FontId::monospace(12.5)).desired_width(f32::INFINITY).desired_rows(8).layouter(&mut layouter));
        });
    });
}

/// Lays out text without wrapping (TextWrapping.NoWrap).
fn no_wrap(palette: &Palette, size: f32) -> impl FnMut(&egui::Ui, &str, f32) -> Arc<egui::Galley> {
    let color = palette.text;
    move |ui: &egui::Ui, text: &str, _wrap: f32| {
        let job = egui::text::LayoutJob::simple(text.to_owned(), egui::FontId::monospace(size), color, f32::INFINITY);
        ui.fonts(|f| f.layout_job(job))
    }
}

/// The selected characters of the query editor, when something is selected.
fn editor_selection(ctx: &egui::Context, id: egui::Id) -> Option<std::ops::Range<usize>> {
    let state = egui::text_edit::TextEditState::load(ctx, id)?;
    let [a, b] = state.cursor.char_range()?.sorted();
    (a.index != b.index).then_some(a.index..b.index)
}

/// The selected text, or everything when nothing is selected.
fn script_to_run(ctx: &egui::Context, id: egui::Id, editor: &str) -> String {
    match editor_selection(ctx, id) {
        Some(range) => editor.chars().skip(range.start).take(range.end - range.start).collect(),
        None => editor.to_string(),
    }
}

fn query_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let editor_id = egui::Id::new(EDITOR_ID);
    let ctx = ui.ctx().clone();
    let running = s.query_cancel.is_some();
    egui::TopBottomPanel::top("sql-query-toolbar").frame(panel_frame(4, 8)).show_separator_line(false).show_inside(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui.add_enabled(!running, small_primary(t("▶ Run (F9)"))).clicked() {
                actions.push(Action::Run(script_to_run(&ctx, editor_id, &s.editor)));
            }
            if ui.add_enabled(running, small(t("■ Stop"))).clicked() {
                actions.push(Action::Stop);
            }
            ui.label(RichText::new(t("Database:")).size(12.5).color(palette.muted));
            let shown = s.database.clone().unwrap_or_else(|| t("(no database)").to_string());
            egui::ComboBox::from_id_salt("sql-database").width(180.0).selected_text(RichText::new(shown).size(12.5)).show_ui(ui, |ui| {
                for name in &s.database_names {
                    ui.selectable_value(&mut s.database, Some(name.clone()), name);
                }
            });
            if ui.add(small(t("Open .sql"))).clicked() {
                actions.push(Action::OpenScript);
            }
            if ui.add(small(t("Save .sql"))).clicked() {
                actions.push(Action::SaveScript);
            }
            if ui.add(small(t("Export CSV"))).clicked() {
                if let Some(ResultTab { content: ResultContent::Grid(grid), .. }) = s.results.get(s.result_tab) {
                    if let Some(csv) = aio_remote::sql_session::csv_bytes(&grid.result) {
                        actions.push(Action::Export(csv, grid.result.rows.len(), "result".into(), true));
                    }
                }
            }
        });
    });
    egui::TopBottomPanel::bottom("sql-query-status").frame(panel_frame(8, 0)).show_separator_line(false).show_inside(ui, |ui| {
        status_label(ui, palette, &s.query_status);
    });
    let editor_height = (ui.available_height() * 0.4).max(80.0);
    egui::TopBottomPanel::top("sql-query-editor-panel").resizable(true).default_height(editor_height).frame(panel_frame(0, 6)).show_inside(ui, |ui| {
        // F9 or Ctrl+Enter runs the selection or everything.
        if ui.memory(|m| m.has_focus(editor_id)) {
            let run = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F9) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
            if run && !running {
                actions.push(Action::Run(script_to_run(&ctx, editor_id, &s.editor)));
            }
        }
        if let Some((start, end)) = s.select_line.take() {
            // Puts the caret on the line of the statement that failed (when the whole editor was run).
            if editor_selection(&ctx, editor_id).is_none() {
                let mut state = egui::text_edit::TextEditState::load(&ctx, editor_id).unwrap_or_default();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(start), egui::text::CCursor::new(end))));
                state.store(&ctx, editor_id);
                ctx.memory_mut(|m| m.request_focus(editor_id));
            }
        }
        egui::ScrollArea::both().id_salt("sql-editor-scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut layouter = no_wrap(palette, 13.0);
            ui.add_sized(
                ui.available_size(),
                egui::TextEdit::multiline(&mut s.editor)
                    .id(editor_id)
                    .code_editor()
                    .font(egui::FontId::monospace(13.0))
                    .lock_focus(true)
                    .hint_text("SELECT * FROM users ORDER BY id DESC LIMIT 50;")
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
        });
    });
    egui::CentralPanel::default().frame(panel_frame(6, 0)).show_inside(ui, |ui| {
        if s.results.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            for (index, tab) in s.results.iter().enumerate() {
                if ui.selectable_label(s.result_tab == index, RichText::new(&tab.header).size(13.5)).clicked() {
                    s.result_tab = index;
                }
            }
        });
        ui.add_space(4.0);
        match s.results.get_mut(s.result_tab).map(|r| &mut r.content) {
            Some(ResultContent::Grid(grid)) => {
                if let Some(action) = grid.show(ui, palette, None) {
                    actions.push(Action::Grid(grid.id, action));
                }
            }
            Some(ResultContent::Messages(messages)) => {
                egui::ScrollArea::vertical().id_salt("sql-messages").auto_shrink([false, false]).show(ui, |ui| {
                    let mut text = messages.as_str();
                    ui.add(egui::TextEdit::multiline(&mut text).font(egui::FontId::monospace(12.5)).desired_width(f32::INFINITY));
                });
            }
            None => {}
        }
    });
}
