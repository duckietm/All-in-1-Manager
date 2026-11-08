//! Logs: the hotel's log tables (commands, room and private chat, currencies, purchases, permission and staff
//! actions, room visits, name changes, trades), read-only, through the same MariaDB login as the SQL Editor.
//! A list shows 200 rows a page, newest first; a room chat line shows the lines around it in the room.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use aio_remote::logs_admin::{self, Filter, Log, LogPage, LogRow, LOGS};
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

const LOG_COUNT: usize = LOGS.len();

fn label(log: Log) -> &'static str {
    match log {
        Log::Commands => t("Commands"),
        Log::RoomChat => t("Room chat"),
        Log::PrivateChat => t("Private chat"),
        Log::Economy => t("Currencies"),
        Log::Purchases => t("Purchases"),
        Log::Permissions => t("Permissions"),
        Log::StaffActions => t("Staff actions"),
        Log::RoomVisits => t("Room visits"),
        Log::NameChanges => t("Name changes"),
        Log::Trades => t("Trades"),
    }
}

/// The column names, one per column of the log's spec.
fn headers(log: Log) -> Vec<&'static str> {
    match log {
        Log::Commands => vec![t("Id"), t("Time"), t("Player"), t("Command"), t("Parameters"), t("Done")],
        Log::RoomChat => vec![t("Id"), t("Time"), t("Player"), t("Whispered to"), t("Room id"), t("Room"), t("Message")],
        Log::PrivateChat => vec![t("Id"), t("Time"), t("Sender"), t("Receiver"), t("Message")],
        Log::Economy => vec![t("Id"), t("Time"), t("Player"), t("Operation"), t("Currency type"), t("Amount"), t("Before"), t("After"), t("Item"), t("Context")],
        Log::Purchases => vec![t("Id"), t("Time"), t("Player"), t("Offer"), t("Offer id"), t("Amount"), t("Credits"), t("Points"), t("Points type")],
        Log::Permissions => vec![t("Id"), t("Time"), t("By"), t("Action"), t("Target"), t("Subject"), t("Old value"), t("New value"), t("Via")],
        Log::StaffActions => vec![t("Id"), t("Time"), t("By"), t("Action"), t("Target"), t("Detail"), t("Done")],
        Log::RoomVisits => vec![t("Id"), t("Entered"), t("Left"), t("Player"), t("Room id"), t("Room")],
        Log::NameChanges => vec![t("Time"), t("Player"), t("Old name"), t("New name")],
        Log::Trades => vec![t("Id"), t("Time"), t("Player one"), t("Items"), t("Player two"), t("Items")],
    }
}

/// What the words filter is looked for in.
fn words_hint(log: Log) -> &'static str {
    match log {
        Log::Commands => t("Command or parameters"),
        Log::RoomChat | Log::PrivateChat => t("Words in the message"),
        Log::Economy => t("Operation or context"),
        Log::Purchases => t("Offer name"),
        Log::Permissions => t("Action or subject"),
        Log::StaffActions => t("Action or detail"),
        Log::NameChanges => t("Old or new name"),
        Log::RoomVisits | Log::Trades => "",
    }
}

fn player_hint(log: Log) -> &'static str {
    match log {
        Log::PrivateChat => t("Sender or receiver"),
        Log::Permissions | Log::StaffActions => t("Staff member or player"),
        Log::Trades => t("Either player"),
        _ => t("Player name"),
    }
}

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

/// The filter fields as typed.
#[derive(Default, Clone)]
struct Inputs {
    player: String,
    room: String,
    words: String,
    from: String,
    until: String,
}

/// One log: the filter typed, the filter of the page shown, and the page.
#[derive(Default)]
struct Tab {
    inputs: Inputs,
    applied: Option<Filter>,
    page_number: i64,
    page: Option<LogPage>,
}

#[derive(Default)]
struct State {
    log: Log,
    tabs: [Tab; LOG_COUNT],
    /// The log tables in the database (None until looked up).
    existing: Option<HashSet<Log>>,
    /// The room chat line chosen and the lines around it.
    context: Option<(i64, Vec<LogRow>)>,
    generation: u64,
    session: Option<Arc<SqlSession>>,
    session_for: Option<usize>,
    opening: bool,
    prompt: Option<Prompt>,
    login_visible: bool,
    database: String,
    subtitle: String,
    busy: bool,
    status: Status,
}

impl State {
    fn close(&mut self, shared: &Shared) {
        self.generation += 1;
        if let Some(session) = self.session.take() {
            shared.spawn(async move {
                if let Ok(session) = Arc::try_unwrap(session) {
                    session.close().await;
                }
            });
        }
        let generation = self.generation;
        *self = State { generation, ..State::default() };
    }

    fn tab(&mut self, log: Log) -> &mut Tab {
        &mut self.tabs[log.index()]
    }

    fn exists(&self, log: Log) -> bool {
        self.existing.as_ref().is_some_and(|e| e.contains(&log))
    }

    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }
}

pub struct LogsView {
    state: Arc<Mutex<State>>,
}

#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

impl Ctl {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn with<R>(&self, generation: u64, change: impl FnOnce(&mut State) -> R) -> Option<R> {
        let result = {
            let mut state = self.lock();
            (state.generation == generation).then(|| change(&mut state))
        };
        self.shared.ctx.request_repaint();
        result
    }

    fn session(&self) -> Option<(u64, Arc<SqlSession>, String)> {
        let state = self.lock();
        state.session.clone().map(|s| (state.generation, s, state.database.clone()))
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, task: impl FnOnce(Ctl) -> F) {
        self.shared.spawn(task(self.clone()));
    }

    /// Finds the hotel database, logs in to MariaDB like the SQL Editor (the saved root account, or asks), looks up
    /// which log tables exist and loads the log shown.
    async fn open(self, ask_first: bool) {
        let Some(server) = self.shared.session() else { return };
        let generation = {
            let mut state = self.lock();
            if state.opening {
                return;
            }
            state.opening = true;
            state.generation
        };
        if self.connect(server, ask_first, generation).await {
            self.load_tables().await;
            let log = self.lock().log;
            self.search(log, 0, true).await;
        }
        self.with(generation, |s| s.opening = false);
    }

    async fn connect(&self, server: Arc<dyn Server>, ask_first: bool, generation: u64) -> bool {
        if server.is_dry_run() {
            self.with(generation, |s| s.error(t("Dry run: there is no MariaDB to connect to.").into()));
            return false;
        }
        self.with(generation, |s| s.subtitle = t("Looking for the hotel database ...").into());
        let database = match aio_remote::catalog_admin::hotel_database(&*server).await {
            Ok(database) => database,
            Err(error) => {
                self.with(generation, |s| s.error(error.to_string()));
                return false;
            }
        };
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
                    return false;
                }
                match answer.await.ok().flatten() {
                    Some(entered) => login = entered,
                    None => {
                        self.with(generation, |s| {
                            s.login_visible = true;
                            s.subtitle = t("Not logged in to MariaDB. Click Log in to MariaDB.").into();
                        });
                        return false;
                    }
                }
            }
            self.with(generation, |s| s.subtitle = tf("Connecting to MariaDB as {user} ...", &[("user", &login.user)]));
            match SqlSession::open(&*server, &login.user, &login.password, login.port).await {
                Ok(session) => break session,
                Err(error) => {
                    let message = error.to_string();
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
        let subtitle = tf("The logs in {database} on {host}, through the SSH tunnel. Read-only.", &[("database", &database), ("host", &host)]);
        let kept = self.with(generation, |s| {
            s.session = Some(session.clone());
            s.database = database.clone();
            s.login_visible = false;
            s.subtitle = subtitle;
        });
        if kept.is_none() {
            if let Ok(session) = Arc::try_unwrap(session) {
                session.close().await;
            }
            return false;
        }
        true
    }

    async fn load_tables(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let existing = logs_admin::existing_logs(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match existing {
                Ok(existing) => s.existing = Some(existing),
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    /// Loads a page of a log: with the filter typed when `fresh` (the player's name looked up first), else with the
    /// filter of the page shown.
    async fn search(&self, log: Log, page: i64, fresh: bool) {
        let Some((generation, session, database)) = self.session() else { return };
        let (inputs, applied, exists) = {
            let mut s = self.lock();
            let exists = s.exists(log);
            let tab = s.tab(log);
            (tab.inputs.clone(), tab.applied.clone(), exists)
        };
        if !exists {
            return;
        }
        let filter = match applied.filter(|_| !fresh) {
            Some(filter) => filter,
            None => match self.filter(&session, &database, &inputs).await {
                Ok(filter) => filter,
                Err(problem) => {
                    self.with(generation, |s| s.error(problem));
                    return;
                }
            },
        };
        self.with(generation, |s| s.busy = true);
        let loaded = logs_admin::load_page(&session, &database, log, &filter, page).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(loaded) => {
                    let text = if loaded.rows.is_empty() {
                        t("No rows found.").to_string()
                    } else {
                        tf("{count} rows on page {page} of {table}, newest first.", &[("count", &loaded.rows.len().to_string()), ("page", &(page + 1).to_string()), ("table", log.spec().table)])
                    };
                    s.ok(text);
                    if log == Log::RoomChat {
                        s.context = None;
                    }
                    let tab = s.tab(log);
                    tab.applied = Some(filter);
                    tab.page_number = page;
                    tab.page = Some(loaded);
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    /// The filter typed, the player's name turned into the player's id.
    async fn filter(&self, session: &SqlSession, database: &str, inputs: &Inputs) -> Result<Filter, String> {
        let from = logs_admin::from_time(&inputs.from).map_err(|_| tf("From: {text} is not a date (YYYY-MM-DD or YYYY-MM-DD HH:MM).", &[("text", inputs.from.trim())]))?;
        let until = logs_admin::until_time(&inputs.until).map_err(|_| tf("Until: {text} is not a date (YYYY-MM-DD or YYYY-MM-DD HH:MM).", &[("text", inputs.until.trim())]))?;
        let room = match inputs.room.trim() {
            "" => None,
            room => Some(room.parse::<i64>().map_err(|_| tf("Room id: {text} is not a number.", &[("text", room)]))?),
        };
        let player = match inputs.player.trim() {
            "" => None,
            name => match logs_admin::player_id(session, database, name).await.map_err(|e| e.to_string())? {
                Some(id) => Some(id),
                None => return Err(tf("No player is called {name}.", &[("name", name)])),
            },
        };
        Ok(Filter { player, room, words: inputs.words.trim().to_string(), from, until })
    }

    async fn context(self, line: LogRow) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = logs_admin::load_context(&session, &database, &line).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(rows) => s.context = Some((line.id, rows)),
                Err(error) => s.error(error.to_string()),
            }
        });
    }
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl LogsView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())) }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

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
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

enum Action {
    Login,
    Refresh,
    Search(Log, i64, bool),
    Context(LogRow),
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(10).corner_radius(10)
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

const FIELD_HEIGHT: f32 = 26.0;

impl View for LogsView {
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
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Refresh => ctl.spawn(|c| async move {
                    c.load_tables().await;
                    let (log, page) = {
                        let mut s = c.lock();
                        let log = s.log;
                        (log, s.tab(log).page_number)
                    };
                    c.search(log, page, false).await;
                }),
                Action::Search(log, page, fresh) => ctl.spawn(move |c| async move { c.search(log, page, fresh).await }),
                Action::Context(line) => ctl.spawn(move |c| c.context(line)),
            }
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    fn session_changed(&mut self, shared: &Shared) {
        self.state.lock().unwrap().close(shared);
    }
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            if ui.add_enabled(s.session.is_some() && !s.busy, small(t("Refresh"))).clicked() {
                actions.push(Action::Refresh);
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Logs")).size(13.0).strong());
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for log in LOGS {
            let missing = s.existing.is_some() && !s.exists(log);
            let text = RichText::new(label(log)).size(15.0);
            let text = if missing { text.color(palette.subtle) } else { text };
            if ui.selectable_label(s.log == log, text).clicked() && s.log != log {
                s.log = log;
                if s.tabs[log.index()].page.is_none() && s.session.is_some() && s.exists(log) {
                    actions.push(Action::Search(log, 0, true));
                }
            }
        }
    });
    ui.add_space(6.0);
    let log = s.log;
    if s.existing.is_some() && !s.exists(log) {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            let text = tf("The table {table} is not in this database: this emulator does not write this log.", &[("table", log.spec().table)]);
            widgets::muted(ui, palette, &text, 12.5);
        });
        return;
    }
    egui::TopBottomPanel::top(egui::Id::new(("logs-filter", log.index()))).frame(frame(palette).outer_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 6 })).show_inside(ui, |ui| {
        filters(ui, s, log, actions);
    });
    if log == Log::RoomChat && s.context.is_some() {
        egui::TopBottomPanel::bottom("logs-context")
            .resizable(true)
            .default_height(260.0)
            .height_range(120.0..=600.0)
            .frame(frame(palette).outer_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 0 }))
            .show_inside(ui, |ui| context(ui, palette, s));
    }
    egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
        let ready = s.session.is_some() && !s.busy;
        let tab = &s.tabs[log.index()];
        let Some(page) = tab.page.as_ref() else {
            widgets::muted(ui, palette, t("Not loaded yet."), 12.5);
            return;
        };
        if log == Log::RoomChat {
            widgets::muted(ui, palette, t("Click a line to see the lines before and after it in the same room."), 12.0);
            ui.add_space(4.0);
        }
        let chosen = s.context.as_ref().map(|(id, _)| *id).filter(|_| log == Log::RoomChat);
        if let Some(line) = rows_table(ui, palette, log, &page.rows, chosen, ("logs-rows", log.index()), log == Log::RoomChat && ready) {
            actions.push(Action::Context(line));
        }
    });
}

fn field(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) -> bool {
    let response = ui.add_sized([width, FIELD_HEIGHT], egui::TextEdit::singleline(value).hint_text(hint));
    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

fn filters(ui: &mut egui::Ui, s: &mut State, log: Log, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let spec = log.spec();
    let tab = &mut s.tabs[log.index()];
    let mut enter = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t("Player")).size(12.5));
        enter |= field(ui, &mut tab.inputs.player, player_hint(log), 160.0);
        if spec.room_filter.is_some() {
            ui.label(RichText::new(t("Room id")).size(12.5));
            enter |= field(ui, &mut tab.inputs.room, "", 80.0);
        }
        if !spec.text.is_empty() {
            ui.label(RichText::new(t("Words")).size(12.5));
            enter |= field(ui, &mut tab.inputs.words, words_hint(log), 200.0);
        }
        ui.label(RichText::new(t("From")).size(12.5));
        enter |= field(ui, &mut tab.inputs.from, "YYYY-MM-DD", 130.0);
        ui.label(RichText::new(t("Until")).size(12.5));
        enter |= field(ui, &mut tab.inputs.until, "YYYY-MM-DD", 130.0);
        if ui.add_enabled(ready, small(t("Search"))).clicked() {
            enter = true;
        }
        if ui.add_enabled(ready, small(t("Clear"))).clicked() {
            tab.inputs = Inputs::default();
            enter = true;
        }
    });
    if enter && ready {
        actions.push(Action::Search(log, 0, true));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let number = tab.page_number;
        let more = tab.page.as_ref().is_some_and(|p| p.more);
        if ui.add_enabled(ready && number > 0 && tab.page.is_some(), small(t("Previous"))).clicked() {
            actions.push(Action::Search(log, number - 1, false));
        }
        ui.label(RichText::new(tf("Page {page}", &[("page", &(number + 1).to_string())])).size(12.5));
        if ui.add_enabled(ready && more, small(t("Next"))).clicked() {
            actions.push(Action::Search(log, number + 1, false));
        }
        if tab.applied.as_ref().is_some_and(|f| *f != Filter::default()) {
            ui.label(RichText::new(t("Filtered.")).size(12.0).italics());
        }
    });
}

/// The rows as a table; a row clicked when `clickable` is returned.
fn rows_table(ui: &mut egui::Ui, palette: &Palette, log: Log, rows: &[LogRow], chosen: Option<i64>, id: (&str, usize), clickable: bool) -> Option<LogRow> {
    let names = headers(log);
    let mut clicked = None;
    if rows.is_empty() {
        widgets::muted(ui, palette, t("No rows found."), 12.5);
        return None;
    }
    egui::ScrollArea::horizontal().id_salt((id.0, id.1, "h")).auto_shrink([false, false]).show(ui, |ui| {
        let mut table = TableBuilder::new(ui)
            .id_salt(id)
            .striped(true)
            .resizable(true)
            .sense(if clickable { egui::Sense::click() } else { egui::Sense::hover() })
            .auto_shrink([false, false])
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (i, _) in names.iter().enumerate() {
            let column = if i + 1 == names.len() { Column::remainder().at_least(120.0) } else { Column::auto().at_least(44.0).at_most(360.0) };
            table = table.column(column.clip(true).resizable(true));
        }
        table
            .header(24.0, |mut header| {
                for name in &names {
                    header.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(*name).size(12.5).strong()).truncate());
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let line = &rows[row.index()];
                    row.set_selected(chosen == Some(line.id));
                    let mut response: Option<egui::Response> = None;
                    for value in &line.cells {
                        let (_, cell) = row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(value.as_str()).size(12.5)).truncate().selectable(false)).on_hover_text(value.as_str());
                        });
                        response = Some(match response {
                            Some(r) => r.union(cell),
                            None => cell,
                        });
                    }
                    if response.is_some_and(|r| r.clicked()) {
                        clicked = Some(line.clone());
                    }
                });
            });
    });
    clicked
}

/// The lines around the chosen room chat line, the chosen one marked.
fn context(ui: &mut egui::Ui, palette: &Palette, s: &mut State) {
    let mut close = false;
    if let Some((chosen, rows)) = s.context.as_ref() {
        ui.horizontal(|ui| {
            let room = rows.iter().find(|r| r.id == *chosen).map(|r| r.room).unwrap_or(0);
            ui.label(RichText::new(tf("Around line {id} in room {room}", &[("id", &chosen.to_string()), ("room", &room.to_string())])).size(13.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.add(small(t("Close"))).clicked();
            });
        });
        ui.add_space(4.0);
        rows_table(ui, palette, Log::RoomChat, rows, Some(*chosen), ("logs-context", 0), false);
    }
    if close {
        s.context = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_log_has_its_headers() {
        for log in LOGS {
            assert_eq!(headers(log).len(), log.spec().columns.len(), "{log:?}");
            assert!(!label(log).is_empty());
            assert_eq!(words_hint(log).is_empty(), log.spec().text.is_empty(), "{log:?}");
        }
    }
}
