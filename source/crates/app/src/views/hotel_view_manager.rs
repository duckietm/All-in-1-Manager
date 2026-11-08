//! Hotel view: the landing page players see before they enter a room. Its five widgets (type, texts, link, progress
//! and the JSON config the client reads more options from), the scene images and the Hall of Fame box, the news
//! articles and the emulator settings of the bonus rare and the community goal, all edited in the hotel database
//! through the same MariaDB login as the SQL Editor.
//!
//! The emulator reads these tables when it starts. **Reload in the hotel** sends `:update_hotel_view` over RCON
//! (`executecommand`) as an online staff player, which loads the widgets, the scene, the news and the Hall of Fame
//! again and refreshes the hotel view of everyone online. The settings are read with `:update_config` or a restart.

use std::sync::{Arc, Mutex};

use aio_remote::content_admin::{self, Row, Setting};
use aio_remote::hotel_view_admin::{self as admin, ConfigKind, HotelView, Scene, Slot};
use aio_remote::rcon::{self, Answer};
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

const FIELD_HEIGHT: f32 = 26.0;
const LABEL_WIDTH: f32 = 190.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Widgets,
    Scene,
    News,
    Settings,
}

fn type_name(kind: &str) -> String {
    let name = match kind {
        "promotion" => t("Promotion"),
        "bonus" => t("Bonus rare"),
        "catalogpromo" => t("Catalogue promotion"),
        "catalogpromosmall" => t("Catalogue promotion (small)"),
        "expiringcatalogpage" => t("Expiring catalogue page"),
        "expiringcatalogpagesmall" => t("Expiring catalogue page (small)"),
        "communitygoal" => t("Community goal (vote)"),
        "dailyquest" => t("Daily quest"),
        "nextlimitedrarecountdown" => t("Next limited rare countdown"),
        "achievementcompetition_hall_of_fame" => t("Competition Hall of Fame"),
        "achievementcompetition_prizes" => t("Competition prizes"),
        "concurrentusersgoal" => t("Concurrent users goal"),
        _ => return kind.to_string(),
    };
    format!("{name} ({kind})")
}

fn mode_name(mode: &str) -> &'static str {
    match mode {
        "online_time" => t("Most time online"),
        "achievement_score" => t("Highest achievement score"),
        "currency" => t("Most of a currency"),
        _ => t("Newest players"),
    }
}

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

/// The slot being edited: its columns, the config fields of its type (by [`admin::CONFIG_KEYS`] index) and the rest
/// of the config as JSON text.
#[derive(Clone, Default)]
struct SlotEdit {
    slot: Slot,
    fields: Vec<String>,
    json: String,
    dirty: bool,
}

impl SlotEdit {
    fn new(slot: &Slot) -> Self {
        let config = admin::parse_config(&slot.config_json).unwrap_or_default();
        let fields = admin::CONFIG_KEYS.iter().map(|k| admin::config_value(&config, k.key)).collect();
        SlotEdit { slot: slot.clone(), fields, json: admin::pretty_config(&slot.config_json), dirty: false }
    }

    /// The slot with the config fields written into its JSON, or why that cannot be done.
    fn merged(&self) -> Result<Slot, String> {
        let keys = admin::config_keys_for(&self.slot.kind);
        let values: Vec<_> = keys
            .iter()
            .map(|k| {
                let index = admin::CONFIG_KEYS.iter().position(|c| c.key == k.key).unwrap_or(0);
                (*k, self.fields.get(index).cloned().unwrap_or_default())
            })
            .collect();
        let config_json = admin::merge_config(&self.json, &values)?;
        let slot = Slot { config_json, ..self.slot.clone() };
        match admin::slot_problem(&slot) {
            Some(problem) => Err(problem),
            None => Ok(slot),
        }
    }
}

#[derive(Default)]
struct State {
    tab: Tab,
    data: Option<HotelView>,
    slot: Option<SlotEdit>,
    scene: Option<Scene>,
    scene_dirty: bool,
    news: Option<Row>,
    news_dirty: bool,
    settings: Vec<(String, String)>,
    staff: String,
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
        let staff = std::mem::take(&mut self.staff);
        *self = State { generation, staff, ..State::default() };
    }

    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }

    fn dirty(&self) -> bool {
        self.slot.as_ref().is_some_and(|s| s.dirty) || self.scene_dirty || self.news_dirty || self.settings_changed().next().is_some()
    }

    fn settings_changed(&self) -> impl Iterator<Item = &(String, String)> {
        let stored = self.data.as_ref().map(|d| d.settings.as_slice()).unwrap_or(&[]);
        self.settings.iter().filter(move |(key, value)| stored.iter().find(|s| &s.key == key).is_none_or(|s| s.value.trim() != value.trim()))
    }

    /// Takes the freshly read data; the edits of rows that are still there are kept when `keep` is set.
    fn loaded(&mut self, data: HotelView, keep: bool) {
        let slot_id = self.slot.as_ref().map(|s| s.slot.id).unwrap_or(1);
        if !keep || self.slot.as_ref().is_none_or(|s| !s.dirty) {
            self.slot = data.slots.iter().find(|s| s.id == slot_id).map(SlotEdit::new);
        }
        if !keep || !self.scene_dirty {
            self.scene = data.scene.clone();
            self.scene_dirty = false;
        }
        if !keep || !self.news_dirty {
            let id = self.news.as_ref().map(|n| n.id).unwrap_or(0);
            self.news = data.news.iter().find(|n| n.id == id && id > 0).cloned();
            self.news_dirty = false;
        }
        self.settings = data.settings.iter().map(|s: &Setting| (s.key.clone(), s.value.clone())).collect();
        self.data = Some(data);
    }
}

pub struct HotelViewManager {
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
            self.load(false).await;
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
        let subtitle = tf("The hotel view in {database} on {host}, through the SSH tunnel.", &[("database", &database), ("host", &host)]);
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

    async fn load(&self, keep: bool) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let loaded = admin::load(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(data) => s.loaded(data, keep),
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    /// Runs a write, then reads everything again and says what to do for the hotel to show it.
    async fn write(&self, done: String, task: impl std::future::Future<Output = aio_remote::server::Result<()>>, clear: impl FnOnce(&mut State)) {
        let Some((generation, _, _)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let result = task.await;
        let worked = self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    clear(s);
                    s.ok(done);
                    true
                }
                Err(error) => {
                    s.error(error.to_string());
                    false
                }
            }
        });
        if worked == Some(true) {
            self.load(true).await;
        }
    }

    async fn save_slot(self) {
        let Some((_, session, database)) = self.session() else { return };
        let Some(edit) = self.lock().slot.clone() else { return };
        let slot = match edit.merged() {
            Ok(slot) => slot,
            Err(problem) => {
                self.lock().error(problem);
                return;
            }
        };
        let done = tf("Widget {id} saved. Reload the hotel view to show it.", &[("id", &slot.id.to_string())]);
        self.write(done, async move { admin::save_slot(&session, &database, &slot).await }, |s| {
            if let Some(edit) = s.slot.as_mut() {
                edit.dirty = false;
            }
        })
        .await;
    }

    async fn reset_votes(self, slot_id: i64, votes: i64) {
        let Some((_, session, database)) = self.session() else { return };
        let message = tf("The {count} votes cast on widget {id} are deleted, so every player can vote again.", &[("count", &votes.to_string()), ("id", &slot_id.to_string())]);
        let options = vec![Choice::new(t("Reset votes"), t("Delete the votes."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), t("Reset the votes?"), &message, false, options).await {
            return;
        }
        let done = tf("The votes of widget {id} are reset. Reload the hotel view to show it.", &[("id", &slot_id.to_string())]);
        self.write(done, async move { admin::reset_votes(&session, &database, slot_id).await.map(|_| ()) }, |_| {}).await;
    }

    async fn save_scene(self) {
        let Some((_, session, database)) = self.session() else { return };
        let Some(scene) = self.lock().scene.clone() else { return };
        if let Some(problem) = admin::scene_problem(&scene) {
            self.lock().error(problem);
            return;
        }
        self.write(t("The scene is saved. Reload the hotel view to show it.").into(), async move { admin::save_scene(&session, &database, &scene).await }, |s| s.scene_dirty = false)
            .await;
    }

    async fn save_news(self) {
        let Some((_, session, database)) = self.session() else { return };
        let Some(row) = self.lock().news.clone() else { return };
        if let Some(problem) = content_admin::row_problem(&admin::NEWS, &row) {
            self.lock().error(problem);
            return;
        }
        let Some(generation) = self.session().map(|s| s.0) else { return };
        self.with(generation, |s| s.busy = true);
        let saved = content_admin::save_row(&session, &database, &admin::NEWS, &row).await;
        let worked = self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(saved) => {
                    s.news = Some(saved);
                    s.news_dirty = false;
                    s.ok(t("The article is saved. Reload the hotel view to show it.").into());
                    true
                }
                Err(error) => {
                    s.error(error.to_string());
                    false
                }
            }
        });
        if worked == Some(true) {
            self.load(true).await;
        }
    }

    async fn delete_news(self) {
        let Some((_, session, database)) = self.session() else { return };
        let Some(row) = self.lock().news.clone() else { return };
        if content_admin::is_new(&admin::NEWS, &row) {
            {
                let mut s = self.lock();
                s.news = None;
                s.news_dirty = false;
            }
            self.shared.ctx.request_repaint();
            return;
        }
        let title = row.get(&admin::NEWS, "title").to_string();
        let options = vec![Choice::new(t("Delete"), t("Delete the article."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete the article {title}?", &[("title", &title)]), t("The article is deleted from hotelview_news."), false, options).await {
            return;
        }
        self.write(t("The article is deleted. Reload the hotel view to show it.").into(), async move { content_admin::delete_row(&session, &database, &admin::NEWS, &row, &[]).await.map(|_| ()) }, |s| {
            s.news = None;
            s.news_dirty = false;
        })
        .await;
    }

    async fn save_settings(self) {
        let Some((_, session, database)) = self.session() else { return };
        let changed: Vec<(String, String)> = self.lock().settings_changed().cloned().collect();
        if let Some((key, problem)) = changed.iter().find_map(|(k, v)| admin::setting_problem(k, v).map(|p| (k.clone(), p))) {
            self.lock().error(format!("{key}: {problem}"));
            return;
        }
        let done = tf("{count} settings saved. The emulator reads them with :update_config or a restart.", &[("count", &changed.len().to_string())]);
        self.write(
            done,
            async move {
                for (key, value) in &changed {
                    admin::save_setting(&session, &database, key, value).await?;
                }
                Ok(())
            },
            |_| {},
        )
        .await;
    }

    /// Sends `:update_hotel_view` over RCON as the online staff player typed.
    async fn reload(self) {
        let Some(server) = self.shared.session() else { return };
        let Some((generation, session, database)) = self.session() else { return };
        let name = self.lock().staff.trim().to_string();
        if name.is_empty() {
            self.lock().error(t("Type the name of a staff player who is online in the hotel.").into());
            return;
        }
        self.with(generation, |s| s.busy = true);
        let user = admin::online_user(&session, &database, &name).await;
        let user = match user {
            Ok(Some(user)) => user,
            Ok(None) => {
                self.with(generation, |s| {
                    s.busy = false;
                    s.error(tf("{name} is not online in the hotel. Log in to the hotel with a staff account first.", &[("name", &name)]));
                });
                return;
            }
            Err(error) => {
                self.with(generation, |s| {
                    s.busy = false;
                    s.error(error.to_string());
                });
                return;
            }
        };
        let answer = admin::reload(&*server, user.0).await;
        self.with(generation, |s| {
            s.busy = false;
            match answer {
                Ok(answer) => {
                    let (text, error) = reload_text(&user.1, &answer);
                    s.status = Status { text, error };
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }
}

/// The emulator's answer to the reload in words.
fn reload_text(name: &str, answer: &Answer) -> (String, bool) {
    match answer.status {
        rcon::STATUS_OK => (tf("The hotel view is loaded again and shown to everyone online ({name} got the confirmation).", &[("name", name)]), false),
        rcon::USER_NOT_FOUND => (tf("The emulator did not find {name} online.", &[("name", name)]), true),
        _ if answer.message == "command failed" => (tf("{name} may not use {command} (the cmd_update_hotel_view permission).", &[("name", name), ("command", admin::RELOAD_COMMAND)]), true),
        _ if answer.message == "command not allowed" => (tf("The emulator does not allow {command} over RCON (rcon.execute_command.* in emulator_settings).", &[("command", admin::RELOAD_COMMAND)]), true),
        _ => {
            let message = if answer.message.is_empty() { tf("status {status}", &[("status", &answer.status.to_string())]) } else { answer.message.clone() };
            (tf("The emulator refused: {message}", &[("message", &message)]), true)
        }
    }
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl HotelViewManager {
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
    Reload,
    SaveSlot,
    ResetVotes(i64, i64),
    SaveScene,
    SaveNews,
    DeleteNews,
    SaveSettings,
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(10).corner_radius(10)
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

/// A labelled one-line field; true when it changed.
fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) -> bool {
    ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(label).size(12.5)));
    let width = ui.available_width().clamp(160.0, 520.0);
    let changed = ui.add_sized([width, FIELD_HEIGHT], egui::TextEdit::singleline(value).hint_text(hint)).changed();
    ui.end_row();
    changed
}

fn number_row(ui: &mut egui::Ui, label: &str, value: &mut i64, range: std::ops::RangeInclusive<i64>) -> bool {
    ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(label).size(12.5)));
    let changed = ui.add_sized([120.0, FIELD_HEIGHT], egui::DragValue::new(value).range(range)).changed();
    ui.end_row();
    changed
}

impl View for HotelViewManager {
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
                ui.add_space(8.0);
                egui::Frame::new().show(ui, |ui| {
                    frame(palette).show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        body(ui, palette, s, &mut actions);
                    });
                });
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Refresh => ctl.spawn(|c| async move { c.load(false).await }),
                Action::Reload => ctl.spawn(|c| c.reload()),
                Action::SaveSlot => ctl.spawn(|c| c.save_slot()),
                Action::ResetVotes(id, votes) => ctl.spawn(move |c| c.reset_votes(id, votes)),
                Action::SaveScene => ctl.spawn(|c| c.save_scene()),
                Action::SaveNews => ctl.spawn(|c| c.save_news()),
                Action::DeleteNews => ctl.spawn(|c| c.delete_news()),
                Action::SaveSettings => ctl.spawn(|c| c.save_settings()),
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
    let ready = s.session.is_some() && !s.busy;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            let refresh = ui.add_enabled(ready, small(t("Refresh")));
            if refresh.clicked() {
                actions.push(Action::Refresh);
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Hotel view")).size(13.0).strong());
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t("Reload in the hotel as")).size(12.5));
        ui.add_sized([180.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.staff).hint_text(t("Online staff player")));
        let hover = tf("Sends {command} over RCON as this player, who must be online and may use the command.", &[("command", admin::RELOAD_COMMAND)]);
        if ui.add_enabled(ready, small(t("Reload in the hotel"))).on_hover_text(hover).clicked() {
            actions.push(Action::Reload);
        }
        widgets::muted(ui, palette, &tf("Or type {command} in the hotel.", &[("command", admin::RELOAD_COMMAND)]), 12.0);
    });
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
    if s.dirty() {
        ui.label(RichText::new(t("There are unsaved changes.")).size(12.0).color(palette.warning_text));
    }
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some(data) = s.data.as_ref() else {
        widgets::muted(ui, palette, t("Not loaded yet."), 12.5);
        return;
    };
    let missing = data.missing.clone();
    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::Widgets, t("Widgets")), (Tab::Scene, t("Scene and Hall of Fame")), (Tab::News, t("News")), (Tab::Settings, t("Bonus rare and goals"))] {
            ui.selectable_value(&mut s.tab, tab, RichText::new(label).size(13.0));
        }
    });
    if !missing.is_empty() {
        widgets::muted(ui, palette, &tf("Not in this database: {tables}. Update the emulator to add them.", &[("tables", &missing.join(", "))]), 12.0);
    }
    ui.separator();
    egui::ScrollArea::vertical().id_salt("hotel-view-body").auto_shrink([false, false]).show(ui, |ui| match s.tab {
        Tab::Widgets => widgets_tab(ui, palette, s, actions),
        Tab::Scene => scene_tab(ui, palette, s, actions),
        Tab::News => news_tab(ui, palette, s, actions),
        Tab::Settings => settings_tab(ui, palette, s, actions),
    });
}

fn widgets_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let Some(data) = s.data.as_ref() else { return };
    if data.slots.is_empty() {
        widgets::muted(ui, palette, t("There is no hotelview_landing_slots table."), 12.5);
        return;
    }
    let slots = data.slots.clone();
    let votes = data.votes.clone();
    let current = s.slot.as_ref().map(|e| e.slot.id);
    let dirty = s.slot.as_ref().is_some_and(|e| e.dirty);
    ui.horizontal_wrapped(|ui| {
        for slot in &slots {
            let label = format!("{} {}", slot.id, if slot.title.is_empty() { type_name(&slot.kind) } else { slot.title.clone() });
            let mut text = RichText::new(label).size(12.5);
            if !slot.enabled {
                text = text.color(palette.muted);
            }
            let response = ui.selectable_label(current == Some(slot.id), text);
            let response = if dirty && current != Some(slot.id) { response.on_hover_text(t("Save or the changes to this widget are lost.")) } else { response };
            if response.clicked() && current != Some(slot.id) {
                s.slot = Some(SlotEdit::new(slot));
            }
        }
    });
    ui.add_space(6.0);
    let Some(edit) = s.slot.as_mut() else { return };
    let mut changed = false;
    egui::Grid::new("hotel-view-slot").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t("Shown")).size(12.5)));
        changed |= ui.checkbox(&mut edit.slot.enabled, RichText::new(t("Shown on the hotel view")).size(12.5)).changed();
        ui.end_row();
        ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t("Type")).size(12.5)));
        egui::ComboBox::from_id_salt("hotel-view-type").selected_text(type_name(&edit.slot.kind)).width(320.0).height(420.0).show_ui(ui, |ui| {
            for kind in admin::SLOT_TYPES {
                if ui.selectable_label(edit.slot.kind == *kind, type_name(kind)).clicked() && edit.slot.kind != *kind {
                    edit.slot.kind = kind.to_string();
                    changed = true;
                }
            }
        });
        ui.end_row();
        changed |= text_row(ui, t("Title"), &mut edit.slot.title, "");
        ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t("Text")).size(12.5)));
        let width = ui.available_width().clamp(160.0, 520.0);
        changed |= ui.add_sized([width, 64.0], egui::TextEdit::multiline(&mut edit.slot.body)).changed();
        ui.end_row();
        changed |= text_row(ui, t("Image URL"), &mut edit.slot.image_url, "https://...");
        changed |= text_row(ui, t("Button text"), &mut edit.slot.button_text, "");
        changed |= text_row(ui, t("Button link"), &mut edit.slot.link, "catalog/open/credits");
        changed |= number_row(ui, t("Progress (0 to 100)"), &mut edit.slot.progress, 0..=100);
        changed |= text_row(ui, t("Progress text"), &mut edit.slot.progress_label, "");
    });

    ui.add_space(10.0);
    ui.label(RichText::new(t("Options of this type")).size(13.0).strong());
    widgets::muted(ui, palette, t("Saved in config_json. An empty field removes its key; the fields win over the same key in the JSON below."), 12.0);
    ui.add_space(4.0);
    let keys = admin::config_keys_for(&edit.slot.kind);
    egui::Grid::new("hotel-view-config").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for key in keys {
            let index = admin::CONFIG_KEYS.iter().position(|c| c.key == key.key).unwrap_or(0);
            let Some(value) = edit.fields.get_mut(index) else { continue };
            let label = format!("{} ({})", t(key.label), key.key);
            match key.kind {
                ConfigKind::Flag => {
                    ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(label).size(12.5)));
                    let text = match value.as_str() {
                        "1" => t("Yes"),
                        "0" => t("No"),
                        _ => t("Not set (the client's default)"),
                    };
                    egui::ComboBox::from_id_salt(("hotel-view-flag", key.key)).selected_text(text).width(220.0).show_ui(ui, |ui| {
                        for (option, name) in [("", t("Not set (the client's default)")), ("1", t("Yes")), ("0", t("No"))] {
                            if ui.selectable_label(value == option, name).clicked() && value != option {
                                *value = option.to_string();
                                changed = true;
                            }
                        }
                    });
                    ui.end_row();
                }
                ConfigKind::Number => changed |= text_row(ui, &label, value, if key.key.starts_with("position") { t("Default place") } else { "" }),
                ConfigKind::Text => {
                    let hint = if key.key == "endsAt" { "2026-12-31T20:00:00Z" } else { "" };
                    changed |= text_row(ui, &label, value, hint);
                }
            }
        }
    });

    ui.add_space(10.0);
    ui.label(RichText::new(t("Config (JSON)")).size(13.0).strong());
    widgets::muted(ui, palette, t("Every other option, such as voteOptions. It must be a JSON object; keys you do not change are kept."), 12.0);
    let width = ui.available_width().min(760.0);
    changed |= ui.add_sized([width, 140.0], egui::TextEdit::multiline(&mut edit.json).code_editor()).changed();
    if let Some(problem) = admin::config_problem(&edit.json) {
        ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
    }
    if changed {
        edit.dirty = true;
    }

    ui.add_space(8.0);
    let id = edit.slot.id;
    let cast = votes.iter().find(|(slot, _)| *slot == id).map(|(_, n)| *n).unwrap_or(0);
    ui.horizontal(|ui| {
        if ui.add_enabled(ready && edit.dirty, widgets::primary_button(t("Save widget"))).clicked() {
            actions.push(Action::SaveSlot);
        }
        if ui.add_enabled(edit.dirty, small(t("Undo changes"))).clicked() {
            if let Some(stored) = slots.iter().find(|s| s.id == id) {
                *edit = SlotEdit::new(stored);
            }
        }
        if cast > 0 {
            ui.label(RichText::new(tf("{count} votes cast", &[("count", &cast.to_string())])).size(12.5));
            if ui.add_enabled(ready, small(t("Reset votes"))).clicked() {
                actions.push(Action::ResetVotes(id, cast));
            }
        }
    });
}

fn scene_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let Some(scene) = s.scene.as_mut() else {
        widgets::muted(ui, palette, t("There is no hotelview_landing_settings table."), 12.5);
        return;
    };
    let mut changed = false;
    widgets::muted(ui, palette, t("Empty images use the client's own. A position of -1 is the client's default place."), 12.0);
    ui.add_space(4.0);
    egui::Grid::new("hotel-view-scene").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        changed |= text_row(ui, t("Background image"), &mut scene.background_url, "https://...");
        changed |= text_row(ui, t("Left image"), &mut scene.left_url, "https://...");
        changed |= text_row(ui, t("Right image"), &mut scene.right_url, "https://...");
        changed |= text_row(ui, t("Drape image"), &mut scene.drape_url, "https://...");
        let names = [t("Left x"), t("Left y"), t("Right x"), t("Right y"), t("Drape x"), t("Drape y"), t("Hall of Fame x"), t("Hall of Fame y")];
        for (name, position) in names.iter().zip(scene.positions.iter_mut()) {
            changed |= number_row(ui, name, position, -1..=10_000);
        }
    });
    ui.add_space(10.0);
    ui.label(RichText::new(t("Hall of Fame")).size(13.0).strong());
    egui::Grid::new("hotel-view-hof").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t("Shown")).size(12.5)));
        changed |= ui.checkbox(&mut scene.hall_of_fame_enabled, RichText::new(t("Show the Hall of Fame")).size(12.5)).changed();
        ui.end_row();
        ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t("Lists")).size(12.5)));
        egui::ComboBox::from_id_salt("hotel-view-hof-mode").selected_text(mode_name(&scene.hall_of_fame_mode)).width(240.0).show_ui(ui, |ui| {
            for mode in admin::HALL_OF_FAME_MODES {
                if ui.selectable_label(scene.hall_of_fame_mode == *mode, mode_name(mode)).clicked() && scene.hall_of_fame_mode != *mode {
                    scene.hall_of_fame_mode = mode.to_string();
                    changed = true;
                }
            }
        });
        ui.end_row();
        if scene.hall_of_fame_mode == "currency" {
            changed |= number_row(ui, t("Currency type"), &mut scene.hall_of_fame_currency_type, 0..=1000);
        }
    });
    if changed {
        s.scene_dirty = true;
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(ready && s.scene_dirty, widgets::primary_button(t("Save scene"))).clicked() {
            actions.push(Action::SaveScene);
        }
        if ui.add_enabled(s.scene_dirty, small(t("Undo changes"))).clicked() {
            s.scene = s.data.as_ref().and_then(|d| d.scene.clone());
            s.scene_dirty = false;
        }
    });
}

fn news_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let Some(data) = s.data.as_ref() else { return };
    if data.missing.iter().any(|m| m == "hotelview_news") {
        widgets::muted(ui, palette, t("There is no hotelview_news table."), 12.5);
        return;
    }
    let rows = data.news.clone();
    widgets::muted(ui, palette, &tf("The hotel shows the {count} newest articles (the highest ids).", &[("count", &admin::NEWS_SHOWN.to_string())]), 12.0);
    ui.add_space(4.0);
    let current = s.news.as_ref().map(|n| n.id);
    ui.horizontal(|ui| {
        if ui.add_enabled(ready, small(t("New article"))).clicked() {
            s.news = Some(content_admin::new_row(&admin::NEWS, None));
            s.news_dirty = true;
        }
    });
    egui::Grid::new("hotel-view-news-list").num_columns(2).spacing([12.0, 3.0]).striped(true).show(ui, |ui| {
        for (place, row) in rows.iter().enumerate() {
            let title = row.get(&admin::NEWS, "title");
            let mut text = RichText::new(format!("{} {}", row.id, title)).size(12.5);
            if place >= admin::NEWS_SHOWN {
                text = text.color(palette.muted);
            }
            if ui.selectable_label(current == Some(row.id), text).clicked() {
                s.news = Some(row.clone());
                s.news_dirty = false;
            }
            if place >= admin::NEWS_SHOWN {
                widgets::muted(ui, palette, t("Not shown"), 12.0);
            } else {
                ui.label("");
            }
            ui.end_row();
        }
    });
    let Some(row) = s.news.as_mut() else { return };
    ui.add_space(10.0);
    let heading = if content_admin::is_new(&admin::NEWS, row) { t("New article").to_string() } else { tf("Article {id}", &[("id", &row.id.to_string())]) };
    ui.label(RichText::new(heading).size(13.0).strong());
    let mut changed = false;
    egui::Grid::new("hotel-view-news").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (field, value) in admin::NEWS.fields.iter().zip(row.values.iter_mut()) {
            match field.kind {
                content_admin::FieldKind::Choice(options) => {
                    ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t(field.label)).size(12.5)));
                    let name = |o: &str| if o == "client" { t("A page in the client").to_string() } else { t("A web page").to_string() };
                    egui::ComboBox::from_id_salt(("hotel-view-news", field.column)).selected_text(name(value)).width(220.0).show_ui(ui, |ui| {
                        for option in options {
                            if ui.selectable_label(value == option, name(option)).clicked() && value != option {
                                *value = option.to_string();
                                changed = true;
                            }
                        }
                    });
                    ui.end_row();
                }
                _ if field.column == "text" => {
                    ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(t(field.label)).size(12.5)));
                    let width = ui.available_width().clamp(160.0, 520.0);
                    changed |= ui.add_sized([width, 64.0], egui::TextEdit::multiline(value)).changed();
                    ui.end_row();
                }
                _ => {
                    let hint = match field.column {
                        "image" => "https://...",
                        "button_link" => "https://... / catalog/open",
                        _ => "",
                    };
                    changed |= text_row(ui, t(field.label), value, hint);
                }
            }
        }
    });
    if changed {
        s.news_dirty = true;
    }
    if let Some(problem) = content_admin::row_problem(&admin::NEWS, row) {
        ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(ready && s.news_dirty, widgets::primary_button(t("Save article"))).clicked() {
            actions.push(Action::SaveNews);
        }
        if ui.add_enabled(ready, small(t("Delete"))).clicked() {
            actions.push(Action::DeleteNews);
        }
    });
}

fn settings_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    widgets::muted(ui, palette, t("Emulator settings (emulator_settings) the hotel view widgets read. The emulator reads them at start and with :update_config."), 12.0);
    ui.add_space(4.0);
    let stored: Vec<Setting> = s.data.as_ref().map(|d| d.settings.clone()).unwrap_or_default();
    egui::Grid::new("hotel-view-settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (key, value) in s.settings.iter_mut() {
            let about = admin::SETTINGS.iter().find(|(k, _)| k == key).map(|(_, about)| t(about)).unwrap_or("");
            let label = ui.add_sized([LABEL_WIDTH + 90.0, FIELD_HEIGHT], egui::Label::new(RichText::new(about).size(12.5)).truncate());
            label.on_hover_text(key.as_str());
            ui.vertical(|ui| {
                ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(value).hint_text(key.as_str()));
                if let Some(problem) = admin::setting_problem(key, value) {
                    ui.label(RichText::new(problem).size(12.0).color(palette.error_text));
                } else if stored.iter().find(|s| &s.key == key).is_none_or(|s| s.value.is_empty() && value.is_empty()) {
                    widgets::muted(ui, palette, t("Not set: the emulator uses its default."), 12.0);
                }
            });
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    let changed = s.settings_changed().count();
    ui.horizontal(|ui| {
        if ui.add_enabled(ready && changed > 0, widgets::primary_button(t("Save settings"))).clicked() {
            actions.push(Action::SaveSettings);
        }
        if ui.add_enabled(changed > 0, small(t("Undo changes"))).clicked() {
            s.settings = stored.iter().map(|x| (x.key.clone(), x.value.clone())).collect();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::n;

    /// The labels the data module names; marked here so they are collected for translation.
    const REMOTE_TEXTS: &[&str] = &[
        n("Position x"),
        n("Position y"),
        n("Second button text"),
        n("Second button link"),
        n("Catalogue page (name or id)"),
        n("Featured catalogue slot"),
        n("Ends at (date and time)"),
        n("Badge code"),
        n("Goal code"),
        n("Use the emulator's LTD"),
        n("Use the emulator's bonus rare"),
        n("Use the emulator's community goal"),
        n("Use the emulator's daily quest"),
        n("Title"),
        n("Text"),
        n("Button text"),
        n("Button opens"),
        n("Button link"),
        n("Image"),
        n("Bonus rare: the furni id (items_base) players get"),
        n("Bonus rare: the furni's class name, for its image"),
        n("Bonus rare: how many points earn it"),
        n("Bonus rare: the currency type that counts"),
        n("Concurrent users goal (0 hides the widget)"),
        n("Concurrent users goal: the badge given when it is reached"),
        n("Community goal Hall of Fame: how many players it lists (1 to 10)"),
    ];

    #[test]
    fn remote_labels_are_translatable() {
        let labels = admin::CONFIG_KEYS.iter().map(|k| k.label).chain(admin::NEWS.fields.iter().map(|f| f.label)).chain(admin::SETTINGS.iter().map(|(_, about)| *about));
        for label in labels {
            assert!(REMOTE_TEXTS.contains(&label), "{label} is not marked for translation");
        }
    }

    #[test]
    fn edits_merge_their_fields_into_the_config() {
        let slot = Slot { id: 2, enabled: true, kind: "bonus".into(), config_json: r#"{"positionX":1,"other":"kept"}"#.into(), ..Slot::default() };
        let mut edit = SlotEdit::new(&slot);
        let x = admin::CONFIG_KEYS.iter().position(|k| k.key == "positionX").unwrap();
        assert_eq!(edit.fields[x], "1");
        edit.fields[x] = "30".into();
        let merged = edit.merged().unwrap();
        let value: serde_json::Value = serde_json::from_str(&merged.config_json).unwrap();
        assert_eq!(value["positionX"], 30);
        assert_eq!(value["other"], "kept");
        edit.json = "[".into();
        assert!(edit.merged().is_err());
    }

    #[test]
    fn reload_answers_read_as_words() {
        let (_, error) = reload_text("bob", &Answer { status: 0, message: String::new() });
        assert!(!error);
        let (_, error) = reload_text("bob", &Answer { status: rcon::USER_NOT_FOUND, message: String::new() });
        assert!(error);
        let (text, error) = reload_text("bob", &Answer { status: 1, message: "command failed".into() });
        assert!(error && text.contains("cmd_update_hotel_view"), "{text}");
    }
}
