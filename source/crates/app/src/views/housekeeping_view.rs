//! Housekeeping: what Octane's in-game housekeeping panel does, from the Manager. The hotel's numbers, who is online,
//! the rooms (open, close, settings, mute, kick everyone, transfer, delete), hotel alerts, maintenance and the
//! lockdown, content reloads, the word filter, the permission grid, every active ban, the support tickets and the
//! soundboard catalogue. Lists are read from the hotel database through the SQL Editor's MariaDB login; actions go to
//! the running emulator over RCON (`aio_remote::housekeeping`) and are recorded in its housekeeping log as
//! "All-in-1 Manager". Every action asks first.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use aio_remote::housekeeping::{self as hk, Action as Hk, Recipient, Resolution, RoomSettings};
use aio_remote::players_admin::{self, Rank};
use aio_remote::rcon::Answer;
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Dashboard,
    Online,
    Rooms,
    Hotel,
    WordFilter,
    Permissions,
    Bans,
    Support,
    Soundboard,
}

const TABS: [Tab; 9] = [Tab::Dashboard, Tab::Online, Tab::Rooms, Tab::Hotel, Tab::WordFilter, Tab::Permissions, Tab::Bans, Tab::Support, Tab::Soundboard];

fn tab_name(tab: Tab) -> &'static str {
    match tab {
        Tab::Dashboard => t("Dashboard"),
        Tab::Online => t("Online"),
        Tab::Rooms => t("Rooms"),
        Tab::Hotel => t("Hotel"),
        Tab::WordFilter => t("Word filter"),
        Tab::Permissions => t("Permissions"),
        Tab::Bans => t("Bans"),
        Tab::Support => t("Support"),
        Tab::Soundboard => t("Soundboard"),
    }
}

/// What the form fields of the tabs hold.
#[derive(Default)]
struct Inputs {
    room_search: String,
    room_edit: RoomSettings,
    tags: [String; 2],
    room_owner: String,
    alert: String,
    /// 0 everyone, 1 staff, 2 one player, 3 one room.
    alert_scope: usize,
    alert_target: String,
    maintenance_minutes: i64,
    maintenance_message: String,
    reload: usize,
    word: String,
    replacement: String,
    word_search: String,
    permission_search: String,
    ban_search: String,
    replies: HashMap<i64, String>,
    sound: Option<hk::Sound>,
}

#[derive(Default)]
struct State {
    tab: Tab,
    inputs: Inputs,
    dashboard: Option<hk::Dashboard>,
    /// The numbers came from the emulator (live) rather than the database.
    dashboard_live: bool,
    online: Option<Vec<hk::OnlinePlayer>>,
    rooms: Option<Vec<hk::Room>>,
    room: Option<hk::Room>,
    categories: Vec<(i64, String)>,
    maintenance: Option<hk::MaintenanceState>,
    lockdown: Option<bool>,
    words: Option<Vec<hk::Word>>,
    permissions: Option<hk::Permissions>,
    bans: Option<Vec<hk::Ban>>,
    tickets: Option<Vec<hk::Ticket>>,
    sounds: Option<Vec<hk::Sound>>,
    ranks: Vec<Rank>,
    /// The emulator answered that it does not know the `housekeeping` RCON command.
    unsupported: bool,
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
    sample: bool,
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
        self.inputs.maintenance_minutes = 5;
    }

    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }

    fn rank_name(&self, id: i64) -> String {
        self.ranks.iter().find(|r| r.id == id).map(|r| r.name.clone()).unwrap_or_else(|| id.to_string())
    }
}

/// What to read again after an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    Rooms,
    Room(i64),
    Maintenance,
    Lockdown,
    Words,
    Permissions,
    Bans,
    Tickets,
}

enum Action {
    Login,
    Load(Tab),
    SearchRooms,
    ShowRoom(i64),
    TransferRoom(i64),
    Send { action: Hk, question: String, message: String, confirm: bool, after: After },
    SaveSound,
    DeleteSound(i64),
    MoveSound(usize, bool),
}

pub struct HousekeepingView {
    state: Arc<Mutex<State>>,
}

#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

/// The emulator's error keys (and statuses) in words.
pub(crate) fn error_text(answer: &Answer) -> String {
    let key = answer.message.trim();
    let known = match key {
        "housekeeping.error.invalid_input" => Some(t("The emulator refused the values: check them.")),
        "housekeeping.error.user_offline" => Some(t("The player is not online.")),
        "housekeeping.error.user_not_found" => Some(t("There is no such player.")),
        "housekeeping.error.room_not_found" => Some(t("There is no such room, or it is not loaded.")),
        "housekeeping.error.rank_too_high" => Some(t("The player's rank is too high for this.")),
        "housekeeping.error.target_unkickable" => Some(t("This player cannot be kicked.")),
        "housekeeping.error.no_active_ban" => Some(t("There is no active ban.")),
        "housekeeping.error.db_failed" | "housekeeping.error.ban_failed" | "housekeeping.error.hash_failed" | "housekeeping.error.note_failed" | "housekeeping.error.wordfilter_failed" | "housekeeping.error.permission_failed" | "housekeeping.error.economy_failed" => {
            Some(t("The emulator could not write it to the database: see its log."))
        }
        "housekeeping.error.room_not_active" => Some(t("Nobody is in that room right now (it is not loaded).")),
        "housekeeping.error.new_owner_not_found" => Some(t("There is no such player to give the room to.")),
        "housekeeping.error.invalid_category" => Some(t("There is no such room category.")),
        "housekeeping.error.rank_not_found" => Some(t("There is no such rank.")),
        "housekeeping.error.item_not_found" => Some(t("There is no furni with that items_base id.")),
        "housekeeping.error.lockout" => Some(t("Refused: then nobody could use housekeeping or edit permissions any more.")),
        "housekeeping.error.no_countdown" => Some(t("No countdown is running.")),
        "housekeeping.error.word_not_found" => Some(t("That word is not in the word filter.")),
        "housekeeping.error.ticket_not_found" => Some(t("That ticket is closed or gone.")),
        "housekeeping.error.note_not_found" => Some(t("That note is gone.")),
        "housekeeping.error.input_too_long" | "housekeeping.error.alert_empty" => Some(t("The text is empty or too long.")),
        _ => None,
    };
    match (known, answer.status) {
        (Some(text), _) => text.to_string(),
        (None, 2) => t("There is no such player.").to_string(),
        (None, 3) => t("There is no such room, or it is not loaded.").to_string(),
        (None, _) if key.is_empty() => tf("The emulator answered status {status}.", &[("status", &answer.status.to_string())]),
        (None, _) => key.to_string(),
    }
}

impl Ctl {
    fn lock(&self) -> parking_lot::MutexGuard<'_, State> {
        self.state.lock()
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
            if let Some((_, session, database)) = self.session() {
                if let Ok(ranks) = players_admin::ranks(&session, &database).await {
                    self.with(generation, |s| s.ranks = ranks);
                }
            }
            let tab = self.lock().tab;
            self.load(tab).await;
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
        let subtitle = tf("The hotel {database} on {host}: lists from the database, actions to the emulator over RCON.", &[("database", &database), ("host", &host)]);
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

    /// Sends a query action (no confirmation) and returns the answer's message when it worked.
    async fn query(&self, generation: u64, action: Hk) -> Option<String> {
        let server = self.shared.session()?;
        match hk::send(&*server, &action).await {
            Ok(answer) if answer.ok() => Some(answer.message),
            Ok(answer) => {
                self.with(generation, |s| {
                    if hk::unsupported(&answer) {
                        s.unsupported = true;
                    }
                });
                None
            }
            Err(_) => None,
        }
    }

    /// Reads what a tab shows.
    async fn load(&self, tab: Tab) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let result: Result<(), String> = match tab {
            Tab::Dashboard => {
                let live = self.query(generation, Hk::Dashboard).await.and_then(|m| hk::parse_dashboard(&m));
                match live {
                    Some(d) => {
                        self.with(generation, |s| {
                            s.dashboard = Some(d);
                            s.dashboard_live = true;
                        });
                        Ok(())
                    }
                    None => hk::dashboard_from_database(&session, &database).await.map(|d| {
                        self.with(generation, |s| {
                            s.dashboard = Some(d);
                            s.dashboard_live = false;
                        });
                    }).map_err(|e| e.to_string()),
                }
            }
            Tab::Online => hk::online(&session, &database).await.map(|list| {
                self.with(generation, |s| s.online = Some(list));
            }).map_err(|e| e.to_string()),
            Tab::Rooms => {
                if self.lock().categories.is_empty() {
                    if let Ok(categories) = hk::room_categories(&session, &database).await {
                        self.with(generation, |s| s.categories = categories);
                    }
                }
                let search = self.lock().inputs.room_search.clone();
                hk::rooms(&session, &database, &search).await.map(|list| {
                    self.with(generation, |s| s.rooms = Some(list));
                }).map_err(|e| e.to_string())
            }
            Tab::Hotel => {
                let maintenance = self.query(generation, Hk::MaintenanceStatus).await.and_then(|m| hk::parse_maintenance(&m));
                let lockdown = self.query(generation, Hk::LockdownStatus).await.and_then(|m| hk::parse_lockdown(&m));
                self.with(generation, |s| {
                    s.maintenance = maintenance;
                    s.lockdown = lockdown;
                });
                Ok(())
            }
            Tab::WordFilter => hk::word_filter(&session, &database).await.map(|list| {
                self.with(generation, |s| s.words = Some(list));
            }).map_err(|e| e.to_string()),
            Tab::Permissions => hk::permissions(&session, &database).await.map(|grid| {
                self.with(generation, |s| s.permissions = Some(grid));
            }).map_err(|e| e.to_string()),
            Tab::Bans => hk::bans(&session, &database).await.map(|list| {
                self.with(generation, |s| s.bans = Some(list));
            }).map_err(|e| e.to_string()),
            Tab::Support => {
                let list = self.query(generation, Hk::Tickets).await.map(|m| hk::parse_tickets(&m));
                self.with(generation, |s| s.tickets = Some(list.unwrap_or_default()));
                Ok(())
            }
            Tab::Soundboard => hk::sounds(&session, &database).await.map(|list| {
                self.with(generation, |s| s.sounds = Some(list));
            }).map_err(|e| e.to_string()),
        };
        self.with(generation, |s| {
            s.busy = false;
            if let Err(error) = result {
                s.error(error);
            }
        });
    }

    async fn show_room(&self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let room = hk::room(&session, &database, id).await;
        self.with(generation, |s| match room {
            Ok(Some(room)) => {
                s.inputs.room_edit = room.settings();
                s.inputs.tags = [room.tags.first().cloned().unwrap_or_default(), room.tags.get(1).cloned().unwrap_or_default()];
                s.room = Some(room);
            }
            Ok(None) => s.error(tf("There is no room {id}.", &[("id", &id.to_string())])),
            Err(error) => s.error(error.to_string()),
        });
    }

    /// The new owner from the form (an id, or a name looked up), then the transfer.
    async fn transfer_room(self, room: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let typed = self.lock().inputs.room_owner.trim().to_string();
        let owner = match typed.parse::<i64>() {
            Ok(id) => Some((id, typed.clone())),
            Err(_) => players_admin::search(&session, &database, &typed, false, 0)
                .await
                .ok()
                .and_then(|page| page.rows.into_iter().find(|p| p.name.eq_ignore_ascii_case(&typed)))
                .map(|p| (p.id, p.name)),
        };
        let Some((owner, name)) = owner else {
            self.with(generation, |s| s.error(tf("There is no player {name}.", &[("name", &typed)])));
            return;
        };
        let question = t("Transfer the room?").to_string();
        let message = tf("Room {room} gets {name} (id {id}) as its owner. Its furni stays where it is.", &[("room", &room.to_string()), ("name", &name), ("id", &owner.to_string())]);
        self.send(Hk::TransferRoom { room, owner }, question, message, true, After::Room(room)).await;
    }

    /// Asks first (when `confirm`), sends the action, shows the answer and reads again what it changed.
    async fn send(&self, action: Hk, question: String, message: String, confirm: bool, after: After) {
        let Some(server) = self.shared.session() else { return };
        let generation = self.lock().generation;
        if let Some(problem) = action.problem() {
            self.with(generation, |s| s.error(t(&problem).to_string()));
            return;
        }
        if confirm {
            let options = vec![Choice::new(t("Do it"), t("Send it to the emulator now."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
            if !self.shared.dialogs.choose(t("Housekeeping"), &question, &message, false, options).await {
                return;
            }
        }
        self.with(generation, |s| s.busy = true);
        let answer = hk::send(&*server, &action).await;
        let worked = self.with(generation, |s| {
            s.busy = false;
            match answer {
                Ok(answer) if answer.ok() => {
                    s.ok(tf("Done: {action}.", &[("action", action.name())]));
                    true
                }
                Ok(answer) if hk::unsupported(&answer) => {
                    s.unsupported = true;
                    s.error(t("The emulator does not know the housekeeping RCON command yet: update the emulator (Polaris) to use this.").into());
                    false
                }
                Ok(answer) => {
                    s.error(error_text(&answer));
                    false
                }
                Err(error) => {
                    s.error(error.to_string());
                    false
                }
            }
        });
        if worked != Some(true) {
            return;
        }
        match after {
            After::Nothing => {}
            After::Rooms => {
                self.with(generation, |s| s.room = None);
                self.load(Tab::Rooms).await;
            }
            After::Room(id) => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                self.show_room(id).await;
                self.load(Tab::Rooms).await;
            }
            After::Maintenance | After::Lockdown => self.load(Tab::Hotel).await,
            After::Words => self.load(Tab::WordFilter).await,
            After::Permissions => self.load(Tab::Permissions).await,
            After::Bans => self.load(Tab::Bans).await,
            After::Tickets => self.load(Tab::Support).await,
        }
    }

    async fn save_sound(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some(sound) = self.lock().inputs.sound.clone() else { return };
        if let Some(problem) = sound.problem() {
            self.with(generation, |s| s.error(t(&problem).to_string()));
            return;
        }
        let saved = hk::save_sound(&session, &database, &sound).await;
        let reloaded = self.reload_sounds().await;
        self.with(generation, |s| match saved {
            Ok(_) => {
                s.inputs.sound = None;
                s.ok(if reloaded { t("Sound saved; the emulator loaded the soundboard again.").into() } else { t("Sound saved. The emulator did not answer: it loads the soundboard when it starts.").into() });
            }
            Err(error) => s.error(error.to_string()),
        });
        self.load(Tab::Soundboard).await;
    }

    async fn delete_sound(self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let options = vec![Choice::new(t("Delete it"), t("Take the sound out of the soundboard for good."), true).primary(), Choice::new(t("Cancel"), t("Keep it."), false)];
        if !self.shared.dialogs.choose(t("Housekeeping"), t("Delete the sound?"), t("Rooms with it on a pad lose it. This cannot be undone."), false, options).await {
            return;
        }
        let result = hk::delete_sound(&session, &database, id).await;
        self.reload_sounds().await;
        self.with(generation, |s| {
            s.inputs.sound = None;
            if let Err(error) = result {
                s.error(error.to_string());
            }
        });
        self.load(Tab::Soundboard).await;
    }

    async fn move_sound(self, index: usize, up: bool) {
        let Some((generation, session, database)) = self.session() else { return };
        let mut ids: Vec<i64> = self.lock().sounds.iter().flatten().map(|s| s.id).collect();
        let other = if up { index.checked_sub(1) } else { Some(index + 1).filter(|i| *i < ids.len()) };
        let Some(other) = other else { return };
        ids.swap(index, other);
        let result = hk::order_sounds(&session, &database, &ids).await;
        self.reload_sounds().await;
        if let Err(error) = result {
            self.with(generation, |s| s.error(error.to_string()));
        }
        self.load(Tab::Soundboard).await;
    }

    /// The emulator loads the soundboard catalogue again; false when it did not answer.
    async fn reload_sounds(&self) -> bool {
        let Some(server) = self.shared.session() else { return false };
        aio_remote::rcon::send(&*server, &hk::reload_sounds_request()).await.is_ok_and(|a| a.ok())
    }
}

impl HousekeepingView {
    pub fn new(_shared: &Shared) -> Self {
        let mut state = State::default();
        state.inputs.maintenance_minutes = 5;
        Self { state: Arc::new(Mutex::new(state)) }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

    fn connect_if_needed(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        let id = server_id(&server);
        {
            let mut state = self.state.lock();
            if state.session_for == Some(id) {
                return;
            }
            state.close(shared);
            state.session_for = Some(id);
            if server.is_dry_run() && std::env::var_os("AIO_SCREENSHOT").is_some() {
                sample(&mut state);
                return;
            }
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

impl View for HousekeepingView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.connect_if_needed(shared);
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        {
            let mut guard = self.state.lock();
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
                body(ui, palette, s, &mut actions);
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Load(tab) => ctl.spawn(move |c| async move { c.load(tab).await }),
                Action::SearchRooms => ctl.spawn(|c| async move { c.load(Tab::Rooms).await }),
                Action::ShowRoom(id) => ctl.spawn(move |c| async move { c.show_room(id).await }),
                Action::TransferRoom(id) => ctl.spawn(move |c| c.transfer_room(id)),
                Action::Send { action, question, message, confirm, after } => ctl.spawn(move |c| async move { c.send(action, question, message, confirm, after).await }),
                Action::SaveSound => ctl.spawn(|c| c.save_sound()),
                Action::DeleteSound(id) => ctl.spawn(move |c| c.delete_sound(id)),
                Action::MoveSound(index, up) => ctl.spawn(move |c| c.move_sound(index, up)),
            }
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().busy
    }

    fn session_changed(&mut self, shared: &Shared) {
        self.state.lock().close(shared);
    }
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

fn field(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) -> bool {
    let response = ui.add_sized([width, 28.0], egui::TextEdit::singleline(value).hint_text(hint).vertical_align(egui::Align::Center));
    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

fn send(actions: &mut Vec<Action>, action: Hk, question: &str, message: String, after: After) {
    actions.push(Action::Send { action, question: question.to_string(), message, confirm: true, after });
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            if ui.add(widgets::secondary_button(t("Players →"))).on_hover_text(t("A player's account, wallet, sanctions, notes and actions are on the Players page.")).clicked() {
                shared.request(Request::Go(AppPage::Players));
            }
            if ui.add_enabled(s.session.is_some() && !s.busy, small(t("Refresh"))).clicked() {
                actions.push(Action::Load(s.tab));
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Housekeeping")).size(13.0).strong());
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
    if s.unsupported {
        ui.add_space(4.0);
        widgets::result_banner(
            ui,
            palette,
            widgets::ResultKind::Warning,
            t("The emulator has no housekeeping RCON command"),
            t("The lists work, but actions need an emulator (Polaris) with the housekeeping RCON command: update the emulator, then restart it."),
        );
    }
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
}

fn loaded(s: &State, tab: Tab) -> bool {
    match tab {
        Tab::Dashboard => s.dashboard.is_some(),
        Tab::Online => s.online.is_some(),
        Tab::Rooms => s.rooms.is_some(),
        Tab::Hotel => s.maintenance.is_some() || s.lockdown.is_some(),
        Tab::WordFilter => s.words.is_some(),
        Tab::Permissions => s.permissions.is_some(),
        Tab::Bans => s.bans.is_some(),
        Tab::Support => s.tickets.is_some(),
        Tab::Soundboard => s.sounds.is_some(),
    }
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for tab in TABS {
            if ui.selectable_label(s.tab == tab, RichText::new(tab_name(tab)).size(15.0)).clicked() && s.tab != tab {
                s.tab = tab;
                if !loaded(s, tab) && s.session.is_some() {
                    actions.push(Action::Load(tab));
                }
            }
        }
    });
    ui.add_space(6.0);
    egui::CentralPanel::default().frame(widgets::card_frame(palette).inner_margin(10).corner_radius(10)).show_inside(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(("housekeeping", s.tab as usize)).auto_shrink([false, false]).show(ui, |ui| match s.tab {
            Tab::Dashboard => dashboard(ui, palette, s),
            Tab::Online => online(ui, palette, s, actions),
            Tab::Rooms => rooms(ui, palette, s, actions),
            Tab::Hotel => hotel(ui, palette, s, actions),
            Tab::WordFilter => word_filter(ui, palette, s, actions),
            Tab::Permissions => permissions(ui, palette, s, actions),
            Tab::Bans => bans(ui, palette, s, actions),
            Tab::Support => support(ui, palette, s, actions),
            Tab::Soundboard => soundboard(ui, palette, s, actions),
        });
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).size(14.0).strong());
    ui.add_space(4.0);
}

fn loading(ui: &mut egui::Ui, palette: &Palette) {
    widgets::muted(ui, palette, t("Loading ..."), 12.5);
}

fn uptime(seconds: i64) -> String {
    let (d, h, m) = (seconds / 86_400, seconds % 86_400 / 3_600, seconds % 3_600 / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else {
        format!("{h}h {m}m")
    }
}

/// A simple striped table: the headers, then a row per item.
fn table(ui: &mut egui::Ui, id: &str, widths: &[f32], headers: &[&str], rows: usize, mut row: impl FnMut(usize, &mut egui_extras::TableRow)) {
    ui.push_id(id, |ui| {
        let mut builder = TableBuilder::new(ui).striped(true).cell_layout(egui::Layout::left_to_right(egui::Align::Center)).vscroll(false);
        for (i, w) in widths.iter().enumerate() {
            builder = builder.column(if i + 1 == widths.len() { Column::remainder().at_least(*w) } else { Column::initial(*w).resizable(true) });
        }
        builder
            .header(24.0, |mut header| {
                for h in headers {
                    header.col(|ui| {
                        ui.label(RichText::new(*h).size(12.0).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, rows, |mut r| {
                    let i = r.index();
                    row(i, &mut r);
                });
            });
    });
}

fn cell(row: &mut egui_extras::TableRow, text: impl Into<String>) {
    let text = text.into();
    row.col(|ui| {
        ui.add(egui::Label::new(RichText::new(text).size(12.5)).truncate());
    });
}

// ---- Dashboard and online ---------------------------------------------------------------------------------------

fn dashboard(ui: &mut egui::Ui, palette: &Palette, s: &mut State) {
    let Some(d) = s.dashboard.clone() else { return loading(ui, palette) };
    widgets::muted(
        ui,
        palette,
        if s.dashboard_live { t("Live from the running emulator.") } else { t("From the database (the emulator did not answer, or has no housekeeping RCON command): no tickets or uptime.") },
        12.0,
    );
    ui.add_space(8.0);
    let tiles: Vec<(&str, String)> = vec![
        (t("Players online"), d.online_users.to_string()),
        (t("Players"), d.total_users.to_string()),
        (t("Rooms with players"), d.active_rooms.to_string()),
        (t("Rooms"), d.total_rooms.to_string()),
        (t("Open tickets"), if s.dashboard_live { d.pending_tickets.to_string() } else { "-".into() }),
        (t("Emulator up"), if s.dashboard_live { uptime(d.uptime_seconds) } else { "-".into() }),
    ];
    // Rows of equal tiles that fit the width: a wrapped layout cannot measure a frame before it is drawn.
    let gap = 8.0;
    let fit = ((ui.available_width() + gap) / (170.0 + gap)).floor().clamp(1.0, tiles.len() as f32) as usize;
    // As many rows as needed, filled evenly (6 tiles: 3 and 3, not 5 and 1).
    let per_row = tiles.len().div_ceil(tiles.len().div_ceil(fit));
    // Each tile adds its margins (2 x 12) and its border (2 x 1) to the width it is given.
    let width = ((ui.available_width() - gap * (per_row as f32 - 1.0)) / per_row as f32 - 26.0).floor();
    for row in tiles.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (label, value) in row {
                widgets::card_frame(palette).inner_margin(12).corner_radius(10).show(ui, |ui| {
                    ui.set_width(width);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(value.as_str()).size(22.0).strong());
                        ui.label(RichText::new(*label).size(12.0).color(palette.muted));
                    });
                });
            }
        });
        ui.add_space(gap);
    }
    if s.dashboard_live && !d.version.is_empty() {
        ui.add_space(6.0);
        widgets::muted(ui, palette, &tf("Emulator version {version}.", &[("version", &d.version)]), 12.0);
    }
}

fn online(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some(list) = s.online.clone() else { return loading(ui, palette) };
    widgets::muted(ui, palette, &tf("{count} players online. Click a room to open it on the Rooms tab.", &[("count", &list.len().to_string())]), 12.0);
    ui.add_space(6.0);
    table(ui, "hk-online", &[60.0, 160.0, 120.0, 200.0, 130.0, 120.0], &[t("Id"), t("Player"), t("Rank"), t("Room"), t("IP"), t("Online since")], list.len(), |i, row| {
        let p = &list[i];
        cell(row, p.id.to_string());
        cell(row, &p.name);
        cell(row, &p.rank);
        row.col(|ui| {
            if p.room_id > 0 {
                if ui.link(RichText::new(format!("{} #{}", p.room, p.room_id)).size(12.5)).clicked() {
                    s.tab = Tab::Rooms;
                    actions.push(Action::ShowRoom(p.room_id));
                    actions.push(Action::Load(Tab::Rooms));
                }
            } else {
                ui.label(RichText::new(t("In no room")).size(12.5).color(palette.muted));
            }
        });
        cell(row, &p.ip);
        cell(row, hk::when(p.since));
    });
}

// ---- Rooms ------------------------------------------------------------------------------------------------------

fn rooms(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let enter = field(ui, &mut s.inputs.room_search, t("Room id, name or owner"), 280.0);
        if (ui.add_enabled(!s.busy, small(t("Search"))).clicked() || enter) && !s.busy {
            actions.push(Action::SearchRooms);
        }
    });
    widgets::muted(ui, palette, t("Without a search: the rooms with players in them, fullest first."), 12.0);
    ui.add_space(6.0);
    if let Some(room) = s.room.clone() {
        room_card(ui, palette, s, &room, actions);
        ui.add_space(10.0);
    }
    let Some(list) = s.rooms.clone() else { return loading(ui, palette) };
    if list.is_empty() {
        widgets::muted(ui, palette, t("No rooms found."), 12.5);
        return;
    }
    table(ui, "hk-rooms", &[60.0, 220.0, 140.0, 80.0, 90.0, 80.0], &[t("Id"), t("Room"), t("Owner"), t("Players"), t("State"), ""], list.len(), |i, row| {
        let r = &list[i];
        cell(row, r.id.to_string());
        cell(row, &r.name);
        cell(row, &r.owner);
        cell(row, format!("{}/{}", r.users, r.users_max));
        cell(row, &r.state);
        row.col(|ui| {
            if ui.add(small(t("Open ..."))).clicked() {
                actions.push(Action::ShowRoom(r.id));
            }
        });
    });
}

fn room_card(ui: &mut egui::Ui, palette: &Palette, s: &mut State, room: &hk::Room, actions: &mut Vec<Action>) {
    let busy = s.busy;
    widgets::card_frame(palette).inner_margin(12).corner_radius(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} #{}", room.name, room.id)).size(16.0).strong());
            ui.label(RichText::new(tf("owner {owner} · {users}/{max} players · {state}", &[("owner", &room.owner), ("users", &room.users.to_string()), ("max", &room.users_max.to_string()), ("state", &room.state)])).size(12.0).color(palette.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(small(t("Close"))).clicked() {
                    s.room = None;
                }
            });
        });
        ui.add_space(6.0);
        let id = room.id;
        // The actions on the room as it is.
        ui.horizontal_wrapped(|ui| {
            let locked = room.state != "open";
            if ui.add_enabled(!busy && locked, small(t("Open the room"))).clicked() {
                send(actions, Hk::OpenRoom { room: id }, t("Open the room?"), tf("{room} opens: everyone can enter again.", &[("room", &room.name)]), After::Room(id));
            }
            if ui.add_enabled(!busy && !locked, small(t("Close the room"))).clicked() {
                send(actions, Hk::CloseRoom { room: id }, t("Close the room?"), tf("{room} is locked: only the owner and staff can enter.", &[("room", &room.name)]), After::Room(id));
            }
            if ui.add_enabled(!busy, small(t("Kick everyone"))).clicked() {
                send(actions, Hk::KickAll { room: id }, t("Kick everyone out?"), tf("Everyone in {room} (staff with the right to stay excepted) is sent to the hotel view.", &[("room", &room.name)]), After::Room(id));
            }
            if ui.add_enabled(!busy, small(t("Mute the room"))).clicked() {
                send(actions, Hk::MuteRoom { room: id, on: true }, t("Mute the room?"), tf("Nobody but staff can chat in {room} until you unmute it.", &[("room", &room.name)]), After::Nothing);
            }
            if ui.add_enabled(!busy, small(t("Unmute"))).clicked() {
                send(actions, Hk::MuteRoom { room: id, on: false }, t("Unmute the room?"), tf("Everyone can chat in {room} again.", &[("room", &room.name)]), After::Nothing);
            }
        });
        ui.horizontal(|ui| {
            field(ui, &mut s.inputs.room_owner, t("New owner (name or id)"), 200.0);
            if ui.add_enabled(!busy && !s.inputs.room_owner.trim().is_empty(), small(t("Transfer"))).clicked() {
                actions.push(Action::TransferRoom(id));
            }
            if ui.add_enabled(!busy, small(t("Delete the room ..."))).clicked() {
                send(actions, Hk::DeleteRoom { room: id }, t("Delete the room?"), tf("{room} is deleted for good: everyone in it is sent out and the furni goes back to the owner's inventory, as the emulator deletes rooms. This cannot be undone.", &[("room", &room.name)]), After::Rooms);
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new(t("Settings")).size(13.0).strong());
        let edit = &mut s.inputs.room_edit;
        egui::Grid::new("hk-room-settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(t("Name"));
            ui.add_sized([320.0, 28.0], egui::TextEdit::singleline(&mut edit.name).vertical_align(egui::Align::Center));
            ui.end_row();
            ui.label(t("Description"));
            ui.add_sized([320.0, 48.0], egui::TextEdit::multiline(&mut edit.description));
            ui.end_row();
            ui.label(t("Maximum players"));
            ui.add(egui::DragValue::new(&mut edit.users_max).range(1..=200));
            ui.end_row();
            ui.label(t("Category"));
            let current = s.categories.iter().find(|(c, _)| *c == edit.category).map(|(_, n)| n.clone()).unwrap_or_else(|| edit.category.to_string());
            egui::ComboBox::from_id_salt("hk-room-category").selected_text(current).width(320.0).show_ui(ui, |ui| {
                for (c, name) in &s.categories {
                    ui.selectable_value(&mut edit.category, *c, name);
                }
            });
            ui.end_row();
            ui.label(t("Trading"));
            egui::ComboBox::from_id_salt("hk-room-trade").selected_text(trade_name(edit.trade_mode)).width(320.0).show_ui(ui, |ui| {
                for mode in 0..=2 {
                    ui.selectable_value(&mut edit.trade_mode, mode, trade_name(mode));
                }
            });
            ui.end_row();
            ui.label(t("Tags"));
            ui.horizontal(|ui| {
                field(ui, &mut s.inputs.tags[0], t("Tag"), 154.0);
                field(ui, &mut s.inputs.tags[1], t("Tag"), 154.0);
            });
            ui.end_row();
        });
        let mut settings = s.inputs.room_edit.clone();
        settings.tags = s.inputs.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        let changed = settings != room.settings();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy && changed, widgets::primary_button(t("Save settings"))).clicked() {
                send(actions, Hk::RoomSettings { room: id, settings: settings.clone() }, t("Save the room's settings?"), tf("{room} gets the new settings at once; players in it see them.", &[("room", &room.name)]), After::Room(id));
            }
            if let Some(problem) = (Hk::RoomSettings { room: id, settings }).problem() {
                ui.label(RichText::new(t(&problem)).size(12.0).color(palette.error_text));
            }
        });
    });
}

fn trade_name(mode: i64) -> &'static str {
    match mode {
        0 => t("No trading"),
        1 => t("Only with room rights"),
        _ => t("Everyone can trade"),
    }
}

// ---- Hotel ------------------------------------------------------------------------------------------------------

fn hotel(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    section(ui, t("Hotel alert"));
    let scopes = [t("Everyone online"), t("Staff only"), t("One player"), t("The players in one room")];
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("hk-alert-scope").selected_text(scopes[s.inputs.alert_scope]).width(200.0).show_ui(ui, |ui| {
            for (i, scope) in scopes.iter().enumerate() {
                ui.selectable_value(&mut s.inputs.alert_scope, i, *scope);
            }
        });
        if s.inputs.alert_scope >= 2 {
            let hint = if s.inputs.alert_scope == 2 { t("Player name") } else { t("Room id") };
            field(ui, &mut s.inputs.alert_target, hint, 180.0);
        }
    });
    ui.add_sized([520.0, 64.0], egui::TextEdit::multiline(&mut s.inputs.alert).hint_text(t("The message")));
    let to = match s.inputs.alert_scope {
        1 => Recipient::Staff,
        2 => Recipient::User(s.inputs.alert_target.trim().to_string()),
        3 => Recipient::Room(s.inputs.alert_target.trim().parse().unwrap_or(0)),
        _ => Recipient::Hotel,
    };
    if ui.add_enabled(!busy && !s.inputs.alert.trim().is_empty(), small(t("Send the alert"))).clicked() {
        let message = tf("{to} get this alert:\n\n{text}", &[("to", scopes[s.inputs.alert_scope]), ("text", s.inputs.alert.trim())]);
        send(actions, Hk::Alert { message: s.inputs.alert.clone(), to }, t("Send the alert?"), message, After::Nothing);
    }

    section(ui, t("Maintenance"));
    match &s.maintenance {
        Some(m) if m.enabled => {
            widgets::muted(ui, palette, &tf("On: only rank {rank} and up can be in the hotel. Message: {message}", &[("rank", &s.rank_name(m.min_rank)), ("message", &m.message)]), 12.5);
        }
        Some(m) if m.countdown_ends_at > 0 => {
            let left = (m.countdown_ends_at - chrono::Utc::now().timestamp()).max(0);
            widgets::muted(ui, palette, &tf("Starts in {minutes} min {seconds} s. Message: {message}", &[("minutes", &(left / 60).to_string()), ("seconds", &(left % 60).to_string()), ("message", &m.message)]), 12.5);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        Some(_) => widgets::muted(ui, palette, t("Off: the hotel is open."), 12.5),
        None => widgets::muted(ui, palette, t("The emulator did not say (is it running with the housekeeping RCON command?)."), 12.5),
    }
    widgets::muted(ui, palette, t("A countdown tells everyone online the hotel closes in N minutes and reminds them; at the end everyone below the maintenance rank is disconnected. 0 minutes: at once."), 12.0);
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut s.inputs.maintenance_minutes).range(0..=hk::MAX_MAINTENANCE_MINUTES).suffix(t(" min")));
        field(ui, &mut s.inputs.maintenance_message, t("Message (optional)"), 320.0);
    });
    ui.horizontal(|ui| {
        let minutes = s.inputs.maintenance_minutes;
        if ui.add_enabled(!busy, widgets::primary_button(t("Start maintenance"))).clicked() {
            let message = if minutes == 0 { t("Everyone below the maintenance rank is disconnected now.").to_string() } else { tf("Everyone online is told the hotel closes in {minutes} minutes; then everyone below the maintenance rank is disconnected.", &[("minutes", &minutes.to_string())]) };
            send(actions, Hk::Maintenance { mode: "start", message: s.inputs.maintenance_message.clone(), minutes }, t("Start maintenance?"), message, After::Maintenance);
        }
        if ui.add_enabled(!busy, small(t("Stop the countdown"))).clicked() {
            send(actions, Hk::Maintenance { mode: "cancel", message: String::new(), minutes: 0 }, t("Stop the countdown?"), t("The countdown stops; nobody is disconnected.").into(), After::Maintenance);
        }
        if ui.add_enabled(!busy, small(t("End maintenance"))).clicked() {
            send(actions, Hk::Maintenance { mode: "disable", message: String::new(), minutes: 0 }, t("End maintenance?"), t("Everyone can log in again.").into(), After::Maintenance);
        }
    });

    section(ui, t("Emergency lockdown"));
    widgets::muted(ui, palette, t("While it is on, only the highest rank can use the housekeeping panel in the hotel: for when a staff account is taken over."), 12.0);
    ui.horizontal(|ui| {
        let state = match s.lockdown {
            Some(true) => t("On"),
            Some(false) => t("Off"),
            None => t("Unknown"),
        };
        // Down to the middle of the button beside it.
        ui.vertical(|ui| {
            ui.add_space(7.0);
            ui.label(RichText::new(state).strong());
        });
        let on = s.lockdown == Some(true);
        if ui.add_enabled(!busy, small(if on { t("End the lockdown") } else { t("Start the lockdown") })).clicked() {
            let question = if on { t("End the lockdown?") } else { t("Start the lockdown?") };
            let message = if on { t("Every staff rank with the panel can use it again.") } else { t("Only the highest rank can use the housekeeping panel until you end it.") };
            send(actions, Hk::Lockdown { on: !on }, question, message.into(), After::Lockdown);
        }
    });

    section(ui, t("Reload content"));
    widgets::muted(ui, palette, t("The emulator loads one kind of content again from the database, as its :update_* commands do."), 12.0);
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("hk-reload").selected_text(hk::RELOAD_TARGETS[s.inputs.reload]).width(200.0).show_ui(ui, |ui| {
            for (i, target) in hk::RELOAD_TARGETS.iter().enumerate() {
                ui.selectable_value(&mut s.inputs.reload, i, *target);
            }
        });
        if ui.add_enabled(!busy, small(t("Reload"))).clicked() {
            let target = hk::RELOAD_TARGETS[s.inputs.reload];
            actions.push(Action::Send { action: Hk::Reload { target }, question: String::new(), message: String::new(), confirm: false, after: After::Nothing });
        }
    });
}

// ---- Word filter, permissions, bans -----------------------------------------------------------------------------

fn word_filter(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    ui.horizontal(|ui| {
        field(ui, &mut s.inputs.word, t("Word"), 200.0);
        field(ui, &mut s.inputs.replacement, t("Replacement (empty: bobba)"), 200.0);
        if ui.add_enabled(!busy && !s.inputs.word.trim().is_empty(), small(t("Add or change"))).clicked() {
            let word = s.inputs.word.trim().to_string();
            actions.push(Action::Send { action: Hk::AddWord { word, replacement: s.inputs.replacement.clone() }, question: String::new(), message: String::new(), confirm: false, after: After::Words });
            s.inputs.word.clear();
            s.inputs.replacement.clear();
        }
    });
    widgets::muted(ui, palette, t("The emulator replaces these words in chat at once (no reload needed)."), 12.0);
    ui.add_space(4.0);
    let Some(list) = s.words.clone() else { return loading(ui, palette) };
    field(ui, &mut s.inputs.word_search, t("Search"), 200.0);
    let search = s.inputs.word_search.trim().to_lowercase();
    let shown: Vec<hk::Word> = list.into_iter().filter(|w| search.is_empty() || w.word.to_lowercase().contains(&search)).collect();
    ui.add_space(4.0);
    table(ui, "hk-words", &[200.0, 160.0, 60.0, 60.0, 70.0, 80.0], &[t("Word"), t("Replacement"), t("Hide"), t("Report"), t("Mute (s)"), ""], shown.len(), |i, row| {
        let w = &shown[i];
        cell(row, &w.word);
        cell(row, &w.replacement);
        cell(row, if w.hide { "✔" } else { "" });
        cell(row, if w.report { "✔" } else { "" });
        cell(row, w.mute.to_string());
        row.col(|ui| {
            if ui.add_enabled(!busy, small(t("Remove"))).clicked() {
                send(actions, Hk::RemoveWord { word: w.word.clone() }, t("Remove the word?"), tf("\"{word}\" is no longer filtered.", &[("word", &w.word)]), After::Words);
            }
        });
    });
}

fn permissions(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let Some(grid) = s.permissions.clone() else { return loading(ui, palette) };
    widgets::muted(ui, palette, t("Every permission against every rank (permission_definitions). A click changes it in the running emulator and the database at once; 2 means \"in rooms with rights\"."), 12.0);
    field(ui, &mut s.inputs.permission_search, t("Search permissions"), 260.0);
    let search = s.inputs.permission_search.trim().to_lowercase();
    let rows: Vec<&hk::PermissionRow> = grid.rows.iter().filter(|r| search.is_empty() || r.key.to_lowercase().contains(&search) || r.comment.to_lowercase().contains(&search)).collect();
    ui.add_space(4.0);
    let mut widths = vec![240.0];
    widths.extend(grid.ranks.iter().map(|_| 70.0));
    let mut headers: Vec<String> = vec![t("Permission").to_string()];
    headers.extend(grid.ranks.iter().map(|(id, name)| format!("{name} ({id})")));
    let header_refs: Vec<&str> = headers.iter().map(String::as_str).collect();
    egui::ScrollArea::horizontal().id_salt("hk-permissions-wide").show(ui, |ui| {
        table(ui, "hk-permissions", &widths, &header_refs, rows.len(), |i, row| {
            let p = rows[i];
            row.col(|ui| {
                ui.add(egui::Label::new(RichText::new(&p.key).size(12.0).monospace()).truncate()).on_hover_text(&p.comment);
            });
            for (column, (rank, rank_name)) in grid.ranks.iter().enumerate() {
                let value = p.values.get(column).copied().unwrap_or(0);
                row.col(|ui| {
                    let label = match value {
                        0 => "·".to_string(),
                        1 => "✔".to_string(),
                        v => v.to_string(),
                    };
                    let next = if value >= p.max { 0 } else { value + 1 };
                    if ui.add_enabled(!busy, egui::Button::new(RichText::new(label).size(13.0)).min_size(egui::vec2(40.0, 22.0))).on_hover_text(tf("Click: {next}", &[("next", &next.to_string())])).clicked() {
                        send(
                            actions,
                            Hk::SetPermission { permission: p.key.clone(), rank: *rank, value: next },
                            t("Change the permission?"),
                            tf("{permission} for {rank}: {from} becomes {to}. Players of that rank have it at once.", &[("permission", &p.key), ("rank", rank_name), ("from", &value.to_string()), ("to", &next.to_string())]),
                            After::Permissions,
                        );
                    }
                });
            }
        });
    });
}

fn bans(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let Some(list) = s.bans.clone() else { return loading(ui, palette) };
    field(ui, &mut s.inputs.ban_search, t("Search player, reason or IP"), 260.0);
    let search = s.inputs.ban_search.trim().to_lowercase();
    let shown: Vec<hk::Ban> = list.into_iter().filter(|b| search.is_empty() || b.user.to_lowercase().contains(&search) || b.reason.to_lowercase().contains(&search) || b.ip.contains(&search)).collect();
    widgets::muted(ui, palette, &tf("{count} active bans.", &[("count", &shown.len().to_string())]), 12.0);
    ui.add_space(4.0);
    table(ui, "hk-bans", &[140.0, 80.0, 220.0, 130.0, 110.0, 120.0, 90.0], &[t("Player"), t("Type"), t("Reason"), t("Ends"), t("By"), t("IP"), ""], shown.len(), |i, row| {
        let b = &shown[i];
        cell(row, if b.user.is_empty() { format!("#{}", b.user_id) } else { b.user.clone() });
        cell(row, &b.kind);
        cell(row, &b.reason);
        cell(row, if b.expires >= 2_000_000_000 { t("never").to_string() } else { hk::when(b.expires) });
        cell(row, &b.staff);
        cell(row, &b.ip);
        row.col(|ui| {
            if ui.add_enabled(!busy, small(t("Revoke"))).clicked() {
                send(actions, Hk::RevokeBan { ban: b.id }, t("Revoke the ban?"), tf("The ban of {user} ends now: {reason}", &[("user", &b.user), ("reason", &b.reason)]), After::Bans);
            }
        });
    });
}

// ---- Support and soundboard -------------------------------------------------------------------------------------

fn support(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let Some(list) = s.tickets.clone() else { return loading(ui, palette) };
    widgets::muted(ui, palette, t("The calls for help players sent, from the running emulator. Answer one (the reporter gets your message) or close it."), 12.0);
    if list.is_empty() {
        ui.add_space(6.0);
        widgets::muted(ui, palette, t("No open tickets."), 12.5);
        return;
    }
    for ticket in &list {
        widgets::card_frame(palette).inner_margin(10).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("#{}", ticket.id)).strong());
                ui.label(RichText::new(tf("{reporter} reports {reported}", &[("reporter", &ticket.reporter), ("reported", if ticket.reported.is_empty() { "-" } else { &ticket.reported })])).size(12.5));
                if !ticket.room.is_empty() {
                    ui.label(RichText::new(tf("in {room}", &[("room", &ticket.room)])).size(12.0).color(palette.muted));
                }
                ui.label(RichText::new(hk::when(ticket.created_at)).size(12.0).color(palette.muted));
                if !ticket.moderator.is_empty() {
                    ui.label(RichText::new(tf("picked by {name}", &[("name", &ticket.moderator)])).size(12.0).color(palette.muted));
                }
            });
            ui.label(RichText::new(&ticket.message).size(12.5));
            let reply = s.inputs.replies.entry(ticket.id).or_default();
            ui.horizontal(|ui| {
                field(ui, reply, t("Answer to the reporter"), 360.0);
                let text = reply.clone();
                if ui.add_enabled(!busy && !text.trim().is_empty(), small(t("Answer and close"))).clicked() {
                    send(actions, Hk::ReplyTicket { ticket: ticket.id, message: text.clone(), close: true }, t("Answer and close the ticket?"), tf("{reporter} gets:\n\n{text}", &[("reporter", &ticket.reporter), ("text", text.trim())]), After::Tickets);
                }
            });
            ui.horizontal(|ui| {
                for (resolution, label) in [(Resolution::Resolved, t("Close: resolved")), (Resolution::Useless, t("Close: useless")), (Resolution::Abusive, t("Close: abusive"))] {
                    if ui.add_enabled(!busy, small(label)).clicked() {
                        send(actions, Hk::CloseTicket { ticket: ticket.id, resolution }, t("Close the ticket?"), tf("Ticket #{id} is closed as {how}.", &[("id", &ticket.id.to_string()), ("how", label)]), After::Tickets);
                    }
                }
            });
        });
        ui.add_space(6.0);
    }
}

fn soundboard(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let Some(list) = s.sounds.clone() else { return loading(ui, palette) };
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, small(t("New sound"))).clicked() {
            s.inputs.sound = Some(hk::Sound { enabled: true, min_rank: 1, ..hk::Sound::default() });
        }
        widgets::muted(ui, palette, t("The sounds players can put on soundboard pads (soundboard_sounds). After a change the emulator loads the catalogue again."), 12.0);
    });
    if let Some(sound) = s.inputs.sound.as_mut() {
        ui.add_space(6.0);
        let mut save = false;
        let mut cancel = false;
        widgets::card_frame(palette).inner_margin(10).corner_radius(8).show(ui, |ui| {
            egui::Grid::new("hk-sound").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(t("Name"));
                field(ui, &mut sound.name, "Bell", 300.0);
                ui.end_row();
                ui.label(t("Address"));
                field(ui, &mut sound.url, "/sounds/soundboard/bell.mp3", 300.0);
                ui.end_row();
                ui.label(t("Class name"));
                field(ui, &mut sound.classname, t("(optional)"), 300.0);
                ui.end_row();
                ui.label(t("Lowest rank"));
                ui.add(egui::DragValue::new(&mut sound.min_rank).range(1..=100));
                ui.end_row();
                ui.label(t("Cooldown"));
                ui.add(egui::DragValue::new(&mut sound.cooldown_seconds).range(0..=hk::MAX_COOLDOWN).suffix(" s"));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut sound.enabled, t("On"));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                save = ui.add_enabled(!busy && sound.problem().is_none(), widgets::primary_button(t("Save"))).clicked();
                cancel = ui.add(small(t("Cancel"))).clicked();
                if let Some(problem) = sound.problem() {
                    ui.label(RichText::new(t(&problem)).size(12.0).color(palette.error_text));
                }
            });
        });
        if save {
            actions.push(Action::SaveSound);
        }
        if cancel {
            s.inputs.sound = None;
        }
    }
    ui.add_space(6.0);
    let count = list.len();
    table(ui, "hk-sounds", &[50.0, 160.0, 260.0, 70.0, 80.0, 60.0, 220.0], &[t("Id"), t("Name"), t("Address"), t("Rank"), t("Cooldown"), t("On"), ""], count, |i, row| {
        let sound = &list[i];
        cell(row, sound.id.to_string());
        cell(row, &sound.name);
        cell(row, &sound.url);
        cell(row, sound.min_rank.to_string());
        cell(row, format!("{} s", sound.cooldown_seconds));
        cell(row, if sound.enabled { "✔" } else { "" });
        row.col(|ui| {
            if ui.add_enabled(!busy && i > 0, small("▲")).clicked() {
                actions.push(Action::MoveSound(i, true));
            }
            if ui.add_enabled(!busy && i + 1 < count, small("▼")).clicked() {
                actions.push(Action::MoveSound(i, false));
            }
            if ui.add_enabled(!busy, small(t("Edit"))).clicked() {
                s.inputs.sound = Some(sound.clone());
            }
            if ui.add_enabled(!busy, small(t("Delete"))).clicked() {
                actions.push(Action::DeleteSound(sound.id));
            }
        });
    });
}

/// The screenshot's sample (the dry run has no database).
fn sample(s: &mut State) {
    s.sample = true;
    s.subtitle = "The hotel habbo on hotel.example.com: lists from the database, actions to the emulator over RCON.".into();
    s.ranks = vec![Rank { id: 1, name: "Member".into() }, Rank { id: 7, name: "Administrator".into() }];
    s.dashboard = Some(hk::Dashboard { online_users: 42, total_users: 1830, active_rooms: 17, total_rooms: 960, peak_today: 61, peak_all_time: 233, pending_tickets: 2, uptime_seconds: 190_000, version: "Polaris 4.2".into() });
    s.dashboard_live = true;
    s.room = Some(hk::Room { id: 12, name: "Welcome Lounge".into(), description: "Say hi!".into(), owner_id: 1, owner: "duckie".into(), users: 14, users_max: 25, state: "open".into(), category: 2, trade_mode: 2, tags: vec!["chill".into()], public: true });
    s.inputs.room_edit = s.room.as_ref().unwrap().settings();
    s.inputs.tags = ["chill".into(), String::new()];
    s.rooms = Some(vec![s.room.clone().unwrap()]);
    s.categories = vec![(1, "Building".into()), (2, "Chat and discussion".into())];
    s.inputs.maintenance_minutes = 5;
    s.maintenance = Some(hk::MaintenanceState::default());
    s.lockdown = Some(false);
    s.tab = match std::env::var("AIO_SCREENSHOT_TAB").as_deref() {
        Ok("rooms") => Tab::Rooms,
        Ok("hotel") => Tab::Hotel,
        Ok("permissions") => Tab::Permissions,
        _ => Tab::Dashboard,
    };
    s.permissions = Some(hk::Permissions {
        ranks: vec![(1, "Member".into()), (5, "Moderator".into()), (7, "Administrator".into())],
        rows: vec![
            hk::PermissionRow { key: "acc_housekeeping".into(), comment: "Open the housekeeping panel".into(), max: 1, values: vec![0, 1, 1] },
            hk::PermissionRow { key: "cmd_alert".into(), comment: ":alert".into(), max: 1, values: vec![0, 1, 1] },
            hk::PermissionRow { key: "acc_placefurni".into(), comment: "Place furni".into(), max: 2, values: vec![2, 1, 1] },
        ],
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulator_errors_read_as_words() {
        let answer = |status, message: &str| Answer { status, message: message.into() };
        assert_eq!(error_text(&answer(1, "housekeeping.error.user_offline")), t("The player is not online."));
        assert_eq!(error_text(&answer(3, "")), t("There is no such room, or it is not loaded."));
        assert_eq!(error_text(&answer(1, "Unknown action. Use one of: x")), "Unknown action. Use one of: x");
        assert_eq!(uptime(90_061), "1d 1h 1m");
    }
}
