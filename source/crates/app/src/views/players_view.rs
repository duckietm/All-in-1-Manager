//! Players: search the hotel's players by name or id, see what the hotel stores about one (rank, currencies, club,
//! badges, furni, pets, bots, rooms, room visits, bans and mutes) and act on them through the emulator's RCON: give
//! credits, points or a badge, send an alert, disconnect, set the rank, mute, ban and lift bans. Reading goes through
//! the same MariaDB login as the SQL Editor; every action is confirmed first and the emulator's answer is shown.

use std::sync::Arc;

use aio_remote::housekeeping::{self as hk, Action as Hk};
use aio_remote::players_admin::{self, Action as Rcon, BanKind, Player, PlayerRow, Problem, Rank, SearchPage};
use aio_remote::rcon::{self, Answer};
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use parking_lot::Mutex;

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

const FIELD_HEIGHT: f32 = 28.0;

fn currency_name(kind: i64) -> String {
    match kind {
        0 => t("Duckets").to_string(),
        5 => t("Diamonds").to_string(),
        _ => tf("Points type {type}", &[("type", &kind.to_string())]),
    }
}

fn furni_kind(kind: &str) -> &'static str {
    match kind {
        "s" => t("Floor furni"),
        "i" => t("Wall furni"),
        "e" => t("Effects"),
        "b" => t("Badge furni"),
        "r" => t("Bot furni"),
        "p" => t("Pet furni"),
        "h" => t("Club furni"),
        "" => t("Unknown furni"),
        _ => t("Other furni"),
    }
}

/// Mute lengths offered, in seconds; the emulator takes a week at most by default.
const MUTE_LENGTHS: [i64; 5] = [5 * 60, 15 * 60, 60 * 60, 24 * 60 * 60, players_admin::MUTE_MAX_SECONDS];
const MUTE_DEFAULT: usize = 1;

/// Ban lengths offered, in seconds; the last is "permanent".
const BAN_LENGTHS: [i64; 7] = [60 * 60, 24 * 60 * 60, 3 * 24 * 60 * 60, 7 * 24 * 60 * 60, 30 * 24 * 60 * 60, 365 * 24 * 60 * 60, players_admin::BAN_MAX_SECONDS];
const BAN_DEFAULT: usize = 1;
/// A temporary rank lasts one of these (seconds); a trade lock one of these (hours).
const RANK_LENGTHS: [i64; 5] = [60 * 60, 24 * 60 * 60, 7 * 24 * 60 * 60, 30 * 24 * 60 * 60, 365 * 24 * 60 * 60];
const RANK_DEFAULT: usize = 1;
const TRADE_HOURS: [i64; 5] = [1, 24, 72, 168, 720];
const TRADE_DEFAULT: usize = 1;

fn length_name(seconds: i64) -> &'static str {
    match seconds {
        300 => t("5 minutes"),
        900 => t("15 minutes"),
        3600 => t("1 hour"),
        86_400 => t("1 day"),
        259_200 => t("3 days"),
        604_800 => t("1 week"),
        2_592_000 => t("30 days"),
        31_536_000 => t("1 year"),
        players_admin::BAN_MAX_SECONDS => t("Permanent"),
        _ => t("Other length"),
    }
}

fn ban_kind_name(kind: BanKind) -> &'static str {
    match kind {
        BanKind::Account => t("Account only"),
        BanKind::Ip => t("Address (IP)"),
        BanKind::Machine => t("Machine"),
        BanKind::Super => t("Address and machine"),
    }
}

fn ban_kind_hover(kind: BanKind) -> &'static str {
    match kind {
        BanKind::Account => t("Only this account is banned."),
        BanKind::Ip => t("Every account that logs in from this player's last address is refused. Without a known address it becomes an account ban."),
        BanKind::Machine => t("Every account on this player's computer is refused. Without a known machine it becomes an address ban."),
        BanKind::Super => t("Both the address and the machine are banned."),
    }
}

fn action_name(action: &Rcon) -> String {
    match action {
        Rcon::GiveCredits(amount) => tf("Give {amount} credits", &[("amount", &amount.to_string())]),
        Rcon::GivePoints(kind, amount) => tf("Give {amount} {currency}", &[("amount", &amount.to_string()), ("currency", &currency_name(*kind))]),
        Rcon::GiveBadge(code) => tf("Give the badge {code}", &[("code", code)]),
        Rcon::Alert(_) => t("Send an alert").to_string(),
        Rcon::Disconnect => t("Disconnect").to_string(),
        Rcon::SetRank(rank) => tf("Set the rank to {rank}", &[("rank", &rank.to_string())]),
        Rcon::Mute(seconds) => tf("Mute for {length}", &[("length", length_name(*seconds))]),
        Rcon::Unmute => t("Unmute").to_string(),
        Rcon::Ban(seconds, ..) if *seconds == players_admin::BAN_MAX_SECONDS => t("Ban permanently").to_string(),
        Rcon::Ban(seconds, ..) => tf("Ban for {length}", &[("length", length_name(*seconds))]),
        Rcon::Unban => t("Lift the bans").to_string(),
    }
}

fn problem_text(problem: Problem) -> &'static str {
    match problem {
        Problem::Amount => t("The amount must be a whole number above 0."),
        Problem::CurrencyType => t("The currency type cannot be below 0."),
        Problem::Badge => t("A badge code is 1 to 32 letters, digits, _ or -."),
        Problem::Alert => t("The alert must have 1 to 4096 characters."),
        Problem::Rank => t("Choose a rank."),
        Problem::Duration => t("Choose how long."),
        Problem::Reason => t("The ban reason must have 1 to 200 characters."),
    }
}

/// The emulator's answer in words.
fn answer_text(action: &Rcon, name: &str, answer: &Answer) -> (String, bool) {
    let done = action_name(action);
    match answer.status {
        rcon::STATUS_OK if answer.message == "offline" => (tf("{action} for {name}: done. {name} is offline, so it was written to the database.", &[("action", &done), ("name", name)]), false),
        rcon::STATUS_OK if answer.message.is_empty() => (tf("{action} for {name}: done.", &[("action", &done), ("name", name)]), false),
        rcon::STATUS_OK => (tf("{action} for {name}: done ({message}).", &[("action", &done), ("name", name), ("message", &answer.message)]), false),
        _ if rcon::is_unknown_command(answer) => (
            tf("{action} for {name}: this emulator does not know the command {command}. Update Polaris to use it from here.", &[("action", &done), ("name", name), ("command", action.key())]),
            true,
        ),
        rcon::USER_NOT_FOUND if matches!(action, Rcon::Unban) => (tf("{name} has no active ban.", &[("name", name)]), true),
        rcon::USER_NOT_FOUND if action.online_only() => (tf("{action} for {name}: the emulator did not find {name} online.", &[("action", &done), ("name", name)]), true),
        _ => {
            let message = if answer.message.is_empty() { tf("status {status}", &[("status", &answer.status.to_string())]) } else { answer.message.clone() };
            (tf("{action} for {name}: the emulator refused: {message}", &[("action", &done), ("name", name), ("message", &message)]), true)
        }
    }
}

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

/// The action fields as typed.
#[derive(Default)]
struct Inputs {
    credits: String,
    points: String,
    points_type: String,
    badge: String,
    alert: String,
    rank: Option<i64>,
    /// Indexes into [`MUTE_LENGTHS`] and [`BAN_LENGTHS`].
    mute_length: Option<usize>,
    ban_length: Option<usize>,
    ban_reason: String,
    ban_kind: Option<BanKind>,
    kick_reason: String,
    /// Indexes into [`TRADE_HOURS`] and [`RANK_LENGTHS`].
    trade_length: Option<usize>,
    trade_reason: String,
    temporary_rank: Option<i64>,
    rank_length: Option<usize>,
    item: String,
    item_quantity: String,
    club_days: String,
    note: String,
}

/// What the housekeeping keeps about a player beside the account: read with the player, each empty when its table is
/// not in the database (an emulator before housekeeping).
#[derive(Default)]
struct Extra {
    notes: Vec<hk::Note>,
    clones: Vec<hk::CloneAccount>,
    sanctions: Vec<hk::Sanction>,
    temporary_rank: Option<hk::TemporaryRank>,
    /// The IPs and mail, only after "Reveal".
    private: Option<hk::Private>,
}

#[derive(Default)]
struct State {
    query: String,
    online_only: bool,
    applied: (String, bool),
    page_number: i64,
    page: Option<SearchPage>,
    ranks: Vec<Rank>,
    chosen: Option<i64>,
    player: Option<Player>,
    extra: Extra,
    inputs: Inputs,
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

    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }

    fn rank_name(&self, id: i64) -> String {
        self.ranks.iter().find(|r| r.id == id).map(|r| format!("{} ({})", r.name, r.id)).unwrap_or_else(|| id.to_string())
    }
}

pub struct PlayersView {
    state: Arc<Mutex<State>>,
}

#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
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

    /// Logs in to MariaDB like the SQL Editor, loads the ranks and the first page of players.
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
            self.load_ranks().await;
            self.search(0, true).await;
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
        let subtitle = tf("The players in {database} on {host}, through the SSH tunnel. Actions go to the emulator over RCON.", &[("database", &database), ("host", &host)]);
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

    async fn load_ranks(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        let ranks = players_admin::ranks(&session, &database).await;
        self.with(generation, |s| match ranks {
            Ok(ranks) => s.ranks = ranks,
            Err(error) => s.error(error.to_string()),
        });
    }

    /// Loads a page of players: with the search typed when `fresh`, else with the search of the page shown.
    async fn search(&self, page: i64, fresh: bool) {
        let Some((generation, session, database)) = self.session() else { return };
        let (query, online_only) = {
            let mut s = self.lock();
            if fresh {
                s.applied = (s.query.trim().to_string(), s.online_only);
            }
            s.busy = true;
            s.applied.clone()
        };
        let loaded = players_admin::search(&session, &database, &query, online_only, page).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(loaded) => {
                    let text = if loaded.rows.is_empty() { t("No players found.").to_string() } else { tf("{count} players on page {page}.", &[("count", &loaded.rows.len().to_string()), ("page", &(page + 1).to_string())]) };
                    s.ok(text);
                    s.page_number = page;
                    s.page = Some(loaded);
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    async fn load_player(&self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| {
            s.busy = true;
            s.chosen = Some(id);
        });
        let loaded = players_admin::load(&session, &database, id).await;
        let extra = Extra {
            notes: hk::notes(&session, &database, id).await.unwrap_or_default(),
            clones: hk::clones(&session, &database, id).await.unwrap_or_default(),
            sanctions: hk::sanctions(&session, &database, id).await.unwrap_or_default(),
            temporary_rank: hk::temporary_rank(&session, &database, id).await.ok().flatten(),
            private: None,
        };
        self.with(generation, |s| {
            s.busy = false;
            if s.chosen != Some(id) {
                return;
            }
            match loaded {
                Ok(Some(player)) => {
                    let same = s.player.as_ref().map(|p| p.id) == Some(id);
                    if !same {
                        s.inputs = Inputs { rank: Some(player.rank), ..Inputs::default() };
                    }
                    // A revealed IP stays revealed while the same player is open.
                    let private = if same { s.extra.private.take() } else { None };
                    s.extra = Extra { private, ..extra };
                    s.player = Some(player);
                }
                Ok(None) => {
                    s.player = None;
                    s.error(tf("There is no player with id {id}.", &[("id", &id.to_string())]));
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    /// Asks first, sends the action over RCON, shows the answer and reads the player again.
    async fn act(self, id: i64, name: String, online: bool, action: Rcon) {
        let Some(server) = self.shared.session() else { return };
        let Some((generation, _, _)) = self.session() else { return };
        if let Some(problem) = action.problem() {
            self.with(generation, |s| s.error(problem_text(problem).to_string()));
            return;
        }
        let what = action_name(&action);
        let mut message = match &action {
            Rcon::Alert(text) => tf("{name} gets this alert:\n\n{text}", &[("name", &name), ("text", text)]),
            Rcon::Disconnect => tf("{name} is disconnected from the hotel and can log in again.", &[("name", &name)]),
            Rcon::SetRank(rank) => {
                let rank = self.lock().rank_name(*rank);
                tf("{name} gets the rank {rank} and its permissions. The emulator logs the change.", &[("name", &name), ("rank", &rank)])
            }
            Rcon::Mute(seconds) => tf("{name} cannot chat for {length}. The mute also holds after logging in again.", &[("name", &name), ("length", length_name(*seconds))]),
            Rcon::Unmute => tf("{name} can chat again.", &[("name", &name)]),
            Rcon::Ban(seconds, reason, kind) => {
                let length = if *seconds == players_admin::BAN_MAX_SECONDS { t("for good").to_string() } else { tf("for {length}", &[("length", length_name(*seconds))]) };
                tf(
                    "{name} cannot log in {length}. Ban: {kind}. Reason: {reason}\n\nAn online player sees the ban and is disconnected. Staff can lift it in the hotel or here.",
                    &[("name", &name), ("length", &length), ("kind", ban_kind_name(*kind)), ("reason", reason.trim())],
                )
            }
            Rcon::Unban => tf("Every active ban of {name} ends now, so {name} can log in again.", &[("name", &name)]),
            _ => tf("The emulator gives this to {name} and logs it.", &[("name", &name)]),
        };
        if !online && !action.online_only() {
            message.push_str("\n\n");
            message.push_str(t("The player is offline: the emulator writes it to the database."));
        }
        let options = vec![Choice::new(what.clone(), t("Send it to the emulator now."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("{action} for {name}?", &[("action", &what), ("name", &name)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let answer = players_admin::act(&*server, id, &action).await;
        let worked = self.with(generation, |s| {
            s.busy = false;
            match answer {
                Ok(answer) => {
                    let (text, error) = answer_text(&action, &name, &answer);
                    s.status = Status { text, error };
                    if !error {
                        match &action {
                            Rcon::GiveCredits(_) => s.inputs.credits.clear(),
                            Rcon::GivePoints(..) => s.inputs.points.clear(),
                            Rcon::GiveBadge(_) => s.inputs.badge.clear(),
                            Rcon::Alert(_) => s.inputs.alert.clear(),
                            Rcon::Ban(..) => s.inputs.ban_reason.clear(),
                            _ => {}
                        }
                    }
                    true
                }
                Err(error) => {
                    s.error(error.to_string());
                    false
                }
            }
        });
        if worked == Some(true) {
            // The emulator saves an online player's changes itself; give it a moment before reading them back.
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            self.load_player(id).await;
        }
    }
}

impl Ctl {
    /// A housekeeping action (the emulator's `housekeeping` RCON command): asks first, sends it, shows the answer
    /// (a new password in a dialog) and reads the player again.
    async fn housekeeping(self, id: i64, name: String, action: Hk) {
        let Some(server) = self.shared.session() else { return };
        let Some((generation, _, _)) = self.session() else { return };
        if let Some(problem) = action.problem() {
            self.with(generation, |s| s.error(t(&problem).to_string()));
            return;
        }
        let (what, message) = housekeeping_question(&action, &name, &self.lock());
        let options = vec![Choice::new(what.clone(), t("Send it to the emulator now."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("{action} for {name}?", &[("action", &what), ("name", &name)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let answer = hk::send(&*server, &action).await;
        let password = answer.as_ref().ok().filter(|a| a.ok()).and_then(|a| hk::parse_password(&a.message));
        let worked = self.with(generation, |s| {
            s.busy = false;
            match answer {
                Ok(answer) if answer.ok() => {
                    s.ok(tf("Done: {action} for {name}.", &[("action", &what), ("name", &name)]));
                    match &action {
                        Hk::Kick { .. } => s.inputs.kick_reason.clear(),
                        Hk::TradeLock { .. } => s.inputs.trade_reason.clear(),
                        Hk::GrantItem { .. } => s.inputs.item.clear(),
                        Hk::AddNote { .. } => s.inputs.note.clear(),
                        _ => {}
                    }
                    true
                }
                Ok(answer) if hk::unsupported(&answer) => {
                    s.error(t("The emulator does not know the housekeeping RCON command yet: update the emulator (Polaris) to use this.").into());
                    false
                }
                Ok(answer) => {
                    s.error(super::housekeeping_view::error_text(&answer));
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
        if let Some(password) = password {
            let text = tf("The new password of {name}:\n\n{password}\n\nGive it to the player; it is not shown again.", &[("name", &name), ("password", &password)]);
            self.shared.dialogs.choose(t("Password reset"), &tf("{name} has a new password", &[("name", &name)]), &text, (), vec![Choice::new(t("Close"), t("I have noted it."), ()).primary()]).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        self.load_player(id).await;
    }

    /// Reads the player's IPs and mail (hidden until asked, as in the hotel's panel).
    async fn reveal(&self, id: i64) {
        let Some((generation, session, database)) = self.session() else { return };
        let private = hk::private(&session, &database, id).await;
        self.with(generation, |s| match private {
            Ok(private) if s.chosen == Some(id) => s.extra.private = Some(private),
            Ok(_) => {}
            Err(error) => s.error(error.to_string()),
        });
    }
}

/// The confirm dialog's button and text for a housekeeping action.
fn housekeeping_question(action: &Hk, name: &str, s: &State) -> (String, String) {
    let n = ("name", name);
    match action {
        Hk::Kick { reason, .. } => (t("Kick").into(), tf("{name} is disconnected and sees the reason: {reason}", &[n, ("reason", reason.trim())])),
        Hk::TradeLock { hours, reason, .. } => (
            t("Trade lock").into(),
            tf("{name} cannot trade for {length}. Reason: {reason}", &[n, ("length", length_name(hours * 3600)), ("reason", reason.trim())]),
        ),
        Hk::ResetPassword { .. } => (t("Reset password").into(), tf("{name} gets a new random password, shown once afterwards. An online player is disconnected.", &[n])),
        Hk::TemporaryRank { rank, seconds, .. } => (
            t("Temporary rank").into(),
            tf("{name} gets the rank {rank} for {length}; then the rank before it comes back by itself.", &[n, ("rank", &s.rank_name(*rank)), ("length", length_name(*seconds))]),
        ),
        Hk::GrantItem { item, quantity, .. } => (
            t("Give furni").into(),
            tf("{name} gets {quantity} x furni {item} (items_base id) in the inventory.", &[n, ("quantity", &quantity.to_string()), ("item", &item.to_string())]),
        ),
        Hk::SetClub { days, .. } if *days == 0 => (t("End Habbo Club").into(), tf("The Habbo Club of {name} ends now.", &[n])),
        Hk::SetClub { days, .. } => (t("Habbo Club").into(), tf("{name} has Habbo Club for {days} days from now.", &[n, ("days", &days.to_string())])),
        Hk::AddNote { note, .. } => (t("Add note").into(), tf("Staff see this note on {name}:\n\n{note}", &[n, ("note", note.trim())])),
        Hk::DeleteNote { .. } => (t("Delete note").into(), tf("The note on {name} is deleted.", &[n])),
        other => (other.name().into(), tf("The emulator does this for {name} and logs it.", &[n])),
    }
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl PlayersView {
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
            let mut state = self.state.lock();
            if state.session_for == Some(id) {
                return;
            }
            state.close(shared);
            state.session_for = Some(id);
            // The screenshot of the page shows a sample player: the dry run has no database.
            if server.is_dry_run() && std::env::var_os("AIO_SCREENSHOT").is_some() {
                state.ranks = vec![Rank { id: 1, name: "Member".into() }, Rank { id: 7, name: "Administrator".into() }];
                state.chosen = Some(1);
                state.inputs.rank = Some(1);
                state.player = Some(Player { id: 1, name: "duckie".into(), rank: 1, rank_name: "Member".into(), online: true, credits: 2500, ..Player::default() });
                state.extra = Extra {
                    notes: vec![hk::Note { id: 3, note: "Warned about scamming in the trade room.".into(), author: "admin".into(), created_at: 1_762_600_000 }],
                    clones: vec![hk::CloneAccount { id: 42, name: "duckie2".into(), last_online: 1_762_500_000, ip: String::new() }],
                    sanctions: vec![hk::Sanction { level: 1, reason: "Scamming".into(), probation_until: 1_763_200_000, trade_locked_until: 0, mute_minutes: 0 }],
                    temporary_rank: Some(hk::TemporaryRank { rank: 7, previous: 1, expires_at: 1_762_900_000, staff: "admin".into(), reason: String::new() }),
                    private: None,
                };
                return;
            }
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

enum Action {
    Login,
    Refresh,
    Search(i64, bool),
    Open(i64),
    Rcon(i64, String, bool, Rcon),
    Housekeeping(i64, String, Hk),
    Reveal(i64),
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(10).corner_radius(10)
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

fn field(ui: &mut egui::Ui, value: &mut String, hint: &str, width: f32) -> bool {
    let response = ui.add_sized([width, FIELD_HEIGHT], egui::TextEdit::singleline(value).hint_text(hint).vertical_align(egui::Align::Center));
    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

impl View for PlayersView {
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
                ui.add_space(10.0);
                body(ui, palette, s, &mut actions);
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::Refresh => ctl.spawn(|c| async move {
                    c.load_ranks().await;
                    let (page, chosen) = {
                        let s = c.lock();
                        (s.page_number, s.chosen)
                    };
                    c.search(page, false).await;
                    if let Some(id) = chosen {
                        c.load_player(id).await;
                    }
                }),
                Action::Search(page, fresh) => ctl.spawn(move |c| async move { c.search(page, fresh).await }),
                Action::Open(id) => ctl.spawn(move |c| async move { c.load_player(id).await }),
                Action::Rcon(id, name, online, action) => ctl.spawn(move |c| c.act(id, name, online, action)),
                Action::Housekeeping(id, name, action) => ctl.spawn(move |c| c.housekeeping(id, name, action)),
                Action::Reveal(id) => ctl.spawn(move |c| async move { c.reveal(id).await }),
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
                ui.label(RichText::new(t("Players")).size(13.0).strong());
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
    egui::SidePanel::left("players-list")
        .resizable(true)
        .default_width(330.0)
        .width_range(260.0..=520.0)
        .frame(frame(palette).outer_margin(egui::Margin { left: 0, right: 6, top: 0, bottom: 0 }))
        .show_inside(ui, |ui| list(ui, palette, s, actions));
    egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
        if s.player.is_none() {
            widgets::muted(ui, palette, t("Choose a player on the left."), 12.5);
            return;
        }
        // AIO_SCREENSHOT_BOTTOM: the screenshot shows the end of the details (the actions).
        let bottom = std::env::var_os("AIO_SCREENSHOT_BOTTOM").is_some();
        egui::ScrollArea::vertical().id_salt("players-detail").auto_shrink([false, false]).stick_to_bottom(bottom).show(ui, |ui| detail(ui, palette, s, actions));
    });
}

fn list(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let ready = s.session.is_some() && !s.busy;
    let mut enter = false;
    ui.horizontal(|ui| {
        enter |= field(ui, &mut s.query, t("Name or id"), (ui.available_width() - 80.0).max(120.0));
        if ui.add_enabled(ready, small(t("Search"))).clicked() {
            enter = true;
        }
    });
    ui.horizontal(|ui| {
        if ui.checkbox(&mut s.online_only, RichText::new(t("Online only")).size(12.5)).changed() {
            enter = true;
        }
    });
    if enter && ready {
        actions.push(Action::Search(0, true));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let number = s.page_number;
        let more = s.page.as_ref().is_some_and(|p| p.more);
        if ui.add_enabled(ready && number > 0 && s.page.is_some(), small(t("Previous"))).clicked() {
            actions.push(Action::Search(number - 1, false));
        }
        ui.label(RichText::new(tf("Page {page}", &[("page", &(number + 1).to_string())])).size(12.5));
        if ui.add_enabled(ready && more, small(t("Next"))).clicked() {
            actions.push(Action::Search(number + 1, false));
        }
    });
    ui.add_space(4.0);
    let Some(page) = s.page.as_ref() else {
        widgets::muted(ui, palette, t("Not loaded yet."), 12.5);
        return;
    };
    if page.rows.is_empty() {
        widgets::muted(ui, palette, t("No players found."), 12.5);
        return;
    }
    let rows: &[PlayerRow] = &page.rows;
    let chosen = s.chosen;
    let mut clicked = None;
    TableBuilder::new(ui)
        .id_salt("players-rows")
        .striped(true)
        .sense(egui::Sense::click())
        .auto_shrink([false, false])
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(14.0))
        .column(Column::auto().at_least(40.0))
        .column(Column::remainder().at_least(100.0).clip(true))
        .column(Column::auto().at_least(40.0))
        .header(22.0, |mut header| {
            for name in ["", t("Id"), t("Name"), t("Rank")] {
                header.col(|ui| {
                    ui.label(RichText::new(name).size(12.5).strong());
                });
            }
        })
        .body(|body| {
            body.rows(22.0, rows.len(), |mut row| {
                let player = &rows[row.index()];
                row.set_selected(chosen == Some(player.id));
                let dot = if player.online { palette.success_text } else { palette.subtle };
                let hover = if player.online { t("Online").to_string() } else { tf("Last online {time}", &[("time", &player.last_online)]) };
                let mut response = row.col(|ui| {
                    ui.label(RichText::new("●").size(11.0).color(dot)).on_hover_text(&hover);
                }).1;
                for value in [player.id.to_string(), player.name.clone(), player.rank.to_string()] {
                    let (_, cell) = row.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(value.as_str()).size(12.5)).truncate().selectable(false)).on_hover_text(&player.motto);
                    });
                    response = response.union(cell);
                }
                if response.clicked() {
                    clicked = Some(player.id);
                }
            });
        });
    if let Some(id) = clicked {
        if ready {
            actions.push(Action::Open(id));
        }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).size(13.5).strong());
    ui.add_space(4.0);
}

fn pair(ui: &mut egui::Ui, palette: &Palette, label: &str, value: &str) {
    ui.label(RichText::new(label).size(12.5).color(palette.muted));
    ui.add(egui::Label::new(RichText::new(value).size(12.5)).wrap());
    ui.end_row();
}

fn detail(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some(p) = s.player.as_ref() else { return };
    let rank = if p.rank_name.is_empty() { p.rank.to_string() } else { format!("{} ({})", p.rank_name, p.rank) };
    ui.horizontal(|ui| {
        ui.label(RichText::new(&p.name).size(17.0).strong());
        let (text, color) = if p.online { (t("Online"), palette.success_text) } else { (t("Offline"), palette.muted) };
        ui.label(RichText::new(text).size(12.5).color(color));
    });
    if !p.motto.is_empty() {
        widgets::muted(ui, palette, &p.motto, 12.5);
    }
    ui.add_space(6.0);
    egui::Grid::new("players-overview").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        pair(ui, palette, t("Id"), &p.id.to_string());
        pair(ui, palette, t("Rank"), &rank);
        pair(ui, palette, t("Mail"), if p.mail.is_empty() { "-" } else { &p.mail });
        pair(ui, palette, t("Account created"), &p.created);
        pair(ui, palette, t("Last online"), if p.last_online.is_empty() { "-" } else { &p.last_online });
        let room = match (&p.room, p.online) {
            (Some((id, name)), _) => format!("{name} ({id})"),
            (None, true) => t("In no room").to_string(),
            (None, false) => "-".to_string(),
        };
        pair(ui, palette, t("Current room"), &room);
        pair(ui, palette, t("Home room"), &if p.home_room > 0 { p.home_room.to_string() } else { "-".into() });
        pair(ui, palette, t("Look"), &format!("{} ({})", p.look, p.gender));
    });

    section(ui, t("Wallet"));
    egui::Grid::new("players-wallet").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        pair(ui, palette, t("Credits"), &p.credits.to_string());
        for currency in &p.currencies {
            pair(ui, palette, &currency_name(currency.kind), &currency.amount.to_string());
        }
        let club = p.subscriptions.iter().find(|s| s.active);
        let text = match club {
            Some(club) => tf("{type} until {end}", &[("type", &club.kind), ("end", &club.ends)]),
            None => t("None").to_string(),
        };
        pair(ui, palette, t("Subscription"), &text);
    });

    section(ui, &tf("Badges ({count})", &[("count", &p.badges.len().to_string())]));
    if p.badges.is_empty() {
        widgets::muted(ui, palette, t("No badges."), 12.5);
    } else {
        ui.horizontal_wrapped(|ui| {
            for badge in &p.badges {
                let text = RichText::new(&badge.code).size(12.5);
                if badge.slot > 0 {
                    ui.label(text.strong().color(palette.accent_text)).on_hover_text(tf("Worn in slot {slot}", &[("slot", &badge.slot.to_string())]));
                } else {
                    ui.label(text);
                }
                ui.add_space(6.0);
            }
        });
    }

    section(ui, t("Possessions"));
    egui::Grid::new("players-owned").num_columns(3).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
        ui.label(RichText::new("").size(12.5));
        ui.label(RichText::new(t("In the inventory")).size(12.5).strong());
        ui.label(RichText::new(t("In a room")).size(12.5).strong());
        ui.end_row();
        for furni in &p.furni {
            ui.label(RichText::new(furni_kind(&furni.kind)).size(12.5));
            ui.label(RichText::new(furni.inventory.to_string()).size(12.5));
            ui.label(RichText::new(furni.placed.to_string()).size(12.5));
            ui.end_row();
        }
        for (label, (all, kept)) in [(t("Pets"), p.pets), (t("Bots"), p.bots)] {
            ui.label(RichText::new(label).size(12.5));
            ui.label(RichText::new(kept.to_string()).size(12.5));
            ui.label(RichText::new((all - kept).to_string()).size(12.5));
            ui.end_row();
        }
        ui.label(RichText::new(t("Rooms owned")).size(12.5));
        ui.label(RichText::new(p.rooms.to_string()).size(12.5));
        ui.end_row();
    });

    section(ui, t("Recent room visits"));
    if p.visits.is_empty() {
        widgets::muted(ui, palette, t("No room visits logged."), 12.5);
    } else {
        egui::Grid::new("players-visits").num_columns(3).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
            for name in [t("Entered"), t("Left"), t("Room")] {
                ui.label(RichText::new(name).size(12.5).strong());
            }
            ui.end_row();
            for visit in &p.visits {
                ui.label(RichText::new(&visit.entered).size(12.5));
                ui.label(RichText::new(if visit.left.is_empty() { "-" } else { &visit.left }).size(12.5));
                let room = if visit.room.is_empty() { visit.room_id.to_string() } else { format!("{} ({})", visit.room, visit.room_id) };
                ui.label(RichText::new(room).size(12.5));
                ui.end_row();
            }
        });
    }

    section(ui, t("Bans and mutes"));
    match &p.muted_until {
        Some(until) => {
            ui.label(RichText::new(tf("Muted until {time}", &[("time", until)])).size(12.5).color(palette.warning_text));
        }
        None => widgets::muted(ui, palette, t("Not muted."), 12.5),
    }
    if p.bans.is_empty() {
        widgets::muted(ui, palette, t("No bans."), 12.5);
    } else {
        egui::Grid::new("players-bans").num_columns(5).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
            for name in [t("Given"), t("Ends"), t("Type"), t("By"), t("Reason")] {
                ui.label(RichText::new(name).size(12.5).strong());
            }
            ui.end_row();
            for ban in &p.bans {
                let color = if ban.active { palette.error_text } else { palette.muted };
                ui.label(RichText::new(&ban.given).size(12.5).color(color));
                ui.label(RichText::new(if ban.expires.is_empty() { t("Never") } else { &ban.expires }).size(12.5).color(color));
                ui.label(RichText::new(&ban.kind).size(12.5));
                ui.label(RichText::new(&ban.by).size(12.5));
                ui.add(egui::Label::new(RichText::new(&ban.reason).size(12.5)).wrap());
                ui.end_row();
            }
        });
    }
    if !p.missing.is_empty() {
        ui.add_space(8.0);
        widgets::muted(ui, palette, &tf("Not in this database: {tables}.", &[("tables", &p.missing.join(", "))]), 12.0);
    }

    staff(ui, palette, s, actions);
    controls(ui, palette, s, actions);
}

/// What the housekeeping keeps on the player: temporary rank, sanctions, accounts on the same IP, the hidden IPs and
/// mail, and the staff notes.
fn staff(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some(p) = s.player.as_ref() else { return };
    let (id, name) = (p.id, p.name.clone());
    let ready = s.session.is_some() && !s.busy;
    let row = |ui: &mut egui::Ui, text: String| ui.add(egui::Label::new(RichText::new(text).size(12.5)).wrap());

    if let Some(temporary) = &s.extra.temporary_rank {
        section(ui, t("Temporary rank"));
        let mut text = tf(
            "{rank} until {end}, then back to {previous}.",
            &[("rank", &s.rank_name(temporary.rank)), ("end", &hk::when(temporary.expires_at)), ("previous", &s.rank_name(temporary.previous))],
        );
        if !temporary.staff.is_empty() {
            text.push(' ');
            text.push_str(&tf("Given by {staff}.", &[("staff", &temporary.staff)]));
        }
        row(ui, text);
    }

    section(ui, &tf("Sanctions ({count})", &[("count", &s.extra.sanctions.len().to_string())]));
    if s.extra.sanctions.is_empty() {
        widgets::muted(ui, palette, t("No sanctions (the mod tools' warning ladder)."), 12.5);
    } else {
        egui::Grid::new("players-sanctions").num_columns(5).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
            for name in [t("Level"), t("Probation until"), t("Trade lock until"), t("Mute"), t("Reason")] {
                ui.label(RichText::new(name).size(12.5).strong());
            }
            ui.end_row();
            let time = |ts: i64| if ts > 0 { hk::when(ts) } else { "-".to_string() };
            for sanction in &s.extra.sanctions {
                ui.label(RichText::new(sanction.level.to_string()).size(12.5));
                ui.label(RichText::new(time(sanction.probation_until)).size(12.5));
                ui.label(RichText::new(time(sanction.trade_locked_until)).size(12.5));
                ui.label(RichText::new(if sanction.mute_minutes > 0 { tf("{minutes} min", &[("minutes", &sanction.mute_minutes.to_string())]) } else { "-".into() }).size(12.5));
                ui.add(egui::Label::new(RichText::new(&sanction.reason).size(12.5)).wrap());
                ui.end_row();
            }
        });
    }

    section(ui, &tf("Accounts on the same IP ({count})", &[("count", &s.extra.clones.len().to_string())]));
    if s.extra.clones.is_empty() {
        widgets::muted(ui, palette, t("No other account registered or logs in from this player's IPs."), 12.5);
    } else {
        egui::Grid::new("players-clones").num_columns(3).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
            for name in [t("Player"), t("Last online"), ""] {
                ui.label(RichText::new(name).size(12.5).strong());
            }
            ui.end_row();
            for clone in &s.extra.clones {
                ui.label(RichText::new(format!("{} ({})", clone.name, clone.id)).size(12.5));
                ui.label(RichText::new(if clone.last_online > 0 { hk::when(clone.last_online) } else { "-".into() }).size(12.5));
                if ui.add_enabled(ready, small(t("Open"))).clicked() {
                    actions.push(Action::Open(clone.id));
                }
                ui.end_row();
            }
        });
    }

    section(ui, t("IP and mail"));
    match &s.extra.private {
        Some(private) => {
            egui::Grid::new("players-private").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                let dash = |v: &str| if v.is_empty() { "-".to_string() } else { v.to_string() };
                pair(ui, palette, t("Current IP"), &dash(&private.ip_current));
                pair(ui, palette, t("Registered from"), &dash(&private.ip_register));
                pair(ui, palette, t("Mail"), &dash(&private.mail));
            });
        }
        None => {
            ui.horizontal(|ui| {
                widgets::muted(ui, palette, t("Hidden, as in the hotel's panel."), 12.5);
                if ui.add_enabled(ready, small(t("Reveal"))).clicked() {
                    actions.push(Action::Reveal(id));
                }
            });
        }
    }

    section(ui, &tf("Staff notes ({count})", &[("count", &s.extra.notes.len().to_string())]));
    for note in &s.extra.notes {
        widgets::card_frame(palette).inner_margin(8).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width().min(640.0));
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 80.0).max(120.0));
                    ui.add(egui::Label::new(RichText::new(&note.note).size(12.5)).wrap());
                    widgets::muted(ui, palette, &format!("{} · {}", note.author, hk::when(note.created_at)), 11.5);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.add_enabled(ready, small(t("Delete"))).clicked() {
                        actions.push(Action::Housekeeping(id, name.clone(), Hk::DeleteNote { user: id, note: note.id }));
                    }
                });
            });
        });
        ui.add_space(4.0);
    }
    ui.horizontal_top(|ui| {
        let width = (ui.available_width() - 110.0).clamp(160.0, 530.0);
        ui.add_sized([width, 48.0], egui::TextEdit::multiline(&mut s.inputs.note).hint_text(t("A note only staff see")));
        if ui.add_enabled(ready && !s.inputs.note.trim().is_empty(), small(t("Add note"))).clicked() {
            actions.push(Action::Housekeeping(id, name.clone(), Hk::AddNote { user: id, note: s.inputs.note.clone() }));
        }
    });
}

fn controls(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some(p) = s.player.as_ref() else { return };
    let (id, name, online) = (p.id, p.name.clone(), p.online);
    let (muted, banned) = (p.muted_until.is_some(), p.bans.iter().any(|b| b.active));
    let mut kinds: Vec<i64> = p.currencies.iter().map(|c| c.kind).collect();
    for kind in [0, 5] {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds.sort();
    let ready = s.session.is_some() && !s.busy;
    // Both kinds of action go to one list; the closures share it.
    let pending = std::cell::RefCell::new(Vec::new());
    let send = |action: Rcon| pending.borrow_mut().push(Action::Rcon(id, name.clone(), online, action));
    let hk_send = |action: Hk| pending.borrow_mut().push(Action::Housekeeping(id, name.clone(), action));
    let number = |text: &str| text.trim().parse::<i64>().unwrap_or(0);
    let inputs = &mut s.inputs;

    section(ui, t("Actions (RCON)"));
    widgets::muted(ui, palette, t("Sent to the running emulator. Each one asks first and shows the emulator's answer."), 12.0);
    ui.add_space(4.0);
    // Three columns: the label, the value (always CONTROL wide), the buttons; one row height.
    let control = ACTION_CONTROL;
    let button = |text: &str| small(text).min_size(egui::vec2(ACTION_BUTTON, ROW));
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        // Every field, list and button one row high, so they line up.
        ui.spacing_mut().interact_size.y = ROW;
        ui.spacing_mut().button_padding.y = 4.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Credits"));
            let enter = field(ui, &mut inputs.credits, t("Amount"), control);
            ui.horizontal(|ui| {
                if (ui.add_enabled(ready, button(t("Give"))).clicked() || enter) && ready {
                    send(Rcon::GiveCredits(number(&inputs.credits)));
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Points"));
            let enter = ui
                .vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        let kind = inputs.points_type.trim().parse::<i64>().unwrap_or(5);
                        let type_width = 56.0;
                        let type_label = 40.0;
                        let combo = control - type_width - type_label - 2.0 * ui.spacing().item_spacing.x;
                        egui::ComboBox::from_id_salt("players-points-type").selected_text(currency_name(kind)).width(combo).height(300.0).show_ui(ui, |ui| {
                            for k in &kinds {
                                if ui.selectable_label(kind == *k, currency_name(*k)).clicked() {
                                    inputs.points_type = k.to_string();
                                }
                            }
                        });
                        ui.add_sized([type_label, ROW], egui::Label::new(RichText::new(t("Type")).size(12.5)));
                        field(ui, &mut inputs.points_type, "5", type_width);
                    });
                    field(ui, &mut inputs.points, t("Amount"), control)
                })
                .inner;
            ui.vertical(|ui| {
                // The button beside the amount, on the second line.
                ui.add_space(ROW + 6.0);
                if (ui.add_enabled(ready, button(t("Give"))).clicked() || enter) && ready {
                    let kind = inputs.points_type.trim().parse::<i64>().unwrap_or(5);
                    send(Rcon::GivePoints(kind, number(&inputs.points)));
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Badge"));
            let enter = field(ui, &mut inputs.badge, t("Badge code"), control);
            ui.horizontal(|ui| {
                if (ui.add_enabled(ready, button(t("Give"))).clicked() || enter) && ready {
                    send(Rcon::GiveBadge(inputs.badge.trim().to_string()));
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Rank"));
            let chosen = inputs.rank.unwrap_or(0);
            let text = s.ranks.iter().find(|r| r.id == chosen).map(|r| format!("{} ({})", r.name, r.id)).unwrap_or_else(|| chosen.to_string());
            egui::ComboBox::from_id_salt("players-rank").selected_text(text).width(control).height(300.0).show_ui(ui, |ui| {
                for rank in &s.ranks {
                    if ui.selectable_label(chosen == rank.id, format!("{} ({})", rank.name, rank.id)).clicked() {
                        inputs.rank = Some(rank.id);
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(ready && !s.ranks.is_empty(), button(t("Set rank"))).clicked() {
                    send(Rcon::SetRank(chosen));
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Mute"));
            let chosen = inputs.mute_length.unwrap_or(MUTE_DEFAULT).min(MUTE_LENGTHS.len() - 1);
            egui::ComboBox::from_id_salt("players-mute-length").selected_text(length_name(MUTE_LENGTHS[chosen])).width(control).show_ui(ui, |ui| {
                for (index, seconds) in MUTE_LENGTHS.iter().enumerate() {
                    if ui.selectable_label(chosen == index, length_name(*seconds)).clicked() {
                        inputs.mute_length = Some(index);
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(ready, button(t("Mute"))).clicked() {
                    send(Rcon::Mute(MUTE_LENGTHS[chosen]));
                }
                if ui.add_enabled(ready && muted, button(t("Unmute"))).on_disabled_hover_text(t("The player is not muted.")).clicked() {
                    send(Rcon::Unmute);
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Ban"));
            let ban_length = inputs.ban_length.unwrap_or(BAN_DEFAULT).min(BAN_LENGTHS.len() - 1);
            let ban_kind = inputs.ban_kind.unwrap_or(BanKind::Account);
            let enter = ui
                .vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        let length_width = 120.0;
                        egui::ComboBox::from_id_salt("players-ban-length").selected_text(length_name(BAN_LENGTHS[ban_length])).width(length_width).show_ui(ui, |ui| {
                            for (index, seconds) in BAN_LENGTHS.iter().enumerate() {
                                if ui.selectable_label(ban_length == index, length_name(*seconds)).clicked() {
                                    inputs.ban_length = Some(index);
                                }
                            }
                        });
                        egui::ComboBox::from_id_salt("players-ban-kind").selected_text(ban_kind_name(ban_kind)).width(control - length_width - ui.spacing().item_spacing.x).show_ui(ui, |ui| {
                            for k in BanKind::ALL {
                                if ui.selectable_label(ban_kind == k, ban_kind_name(k)).on_hover_text(ban_kind_hover(k)).clicked() {
                                    inputs.ban_kind = Some(k);
                                }
                            }
                        });
                    });
                    field(ui, &mut inputs.ban_reason, t("Reason"), control)
                })
                .inner;
            ui.vertical(|ui| {
                ui.add_space(ROW + 6.0);
                ui.horizontal(|ui| {
                    if (ui.add_enabled(ready, button(t("Ban"))).clicked() || enter) && ready {
                        send(Rcon::Ban(BAN_LENGTHS[ban_length], inputs.ban_reason.clone(), ban_kind));
                    }
                    if ui.add_enabled(ready && banned, button(t("Lift the bans"))).on_disabled_hover_text(t("The player has no active ban.")).clicked() {
                        send(Rcon::Unban);
                    }
                });
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Temp. rank"));
            let chosen = inputs.temporary_rank.unwrap_or(0);
            let length = inputs.rank_length.unwrap_or(RANK_DEFAULT).min(RANK_LENGTHS.len() - 1);
            let text = s.ranks.iter().find(|r| r.id == chosen).map(|r| format!("{} ({})", r.name, r.id)).unwrap_or_else(|| t("Rank").to_string());
            ui.horizontal(|ui| {
                let length_width = 110.0;
                egui::ComboBox::from_id_salt("players-temp-rank").selected_text(text).width(control - length_width - ui.spacing().item_spacing.x).height(300.0).show_ui(ui, |ui| {
                    for rank in &s.ranks {
                        if ui.selectable_label(chosen == rank.id, format!("{} ({})", rank.name, rank.id)).clicked() {
                            inputs.temporary_rank = Some(rank.id);
                        }
                    }
                });
                egui::ComboBox::from_id_salt("players-temp-length").selected_text(length_name(RANK_LENGTHS[length])).width(length_width).show_ui(ui, |ui| {
                    for (index, seconds) in RANK_LENGTHS.iter().enumerate() {
                        if ui.selectable_label(length == index, length_name(*seconds)).clicked() {
                            inputs.rank_length = Some(index);
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(ready && chosen > 0, button(t("Give"))).on_hover_text(t("The rank for a while; the rank before it comes back by itself.")).clicked() {
                    hk_send(Hk::TemporaryRank { user: id, rank: chosen, seconds: RANK_LENGTHS[length] });
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Furni"));
            let enter = ui
                .horizontal(|ui| {
                    let quantity_width = 70.0;
                    let a = field(ui, &mut inputs.item, t("items_base id"), control - quantity_width - ui.spacing().item_spacing.x);
                    let b = field(ui, &mut inputs.item_quantity, "1", quantity_width);
                    a || b
                })
                .inner;
            ui.horizontal(|ui| {
                if (ui.add_enabled(ready, button(t("Give"))).on_hover_text(t("1 to 100 of the furni, into the inventory.")).clicked() || enter) && ready {
                    let quantity = if inputs.item_quantity.trim().is_empty() { 1 } else { number(&inputs.item_quantity) };
                    hk_send(Hk::GrantItem { user: id, item: number(&inputs.item), quantity });
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Club"));
            let enter = field(ui, &mut inputs.club_days, t("Days from now (0 ends it)"), control);
            ui.horizontal(|ui| {
                if (ui.add_enabled(ready && !inputs.club_days.trim().is_empty(), button(t("Set"))).clicked() || enter) && ready {
                    hk_send(Hk::SetClub { user: id, days: number(&inputs.club_days) });
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Trade lock"));
            let length = inputs.trade_length.unwrap_or(TRADE_DEFAULT).min(TRADE_HOURS.len() - 1);
            let enter = ui
                .horizontal(|ui| {
                    let length_width = 110.0;
                    egui::ComboBox::from_id_salt("players-trade-length").selected_text(length_name(TRADE_HOURS[length] * 3600)).width(length_width).show_ui(ui, |ui| {
                        for (index, hours) in TRADE_HOURS.iter().enumerate() {
                            if ui.selectable_label(length == index, length_name(hours * 3600)).clicked() {
                                inputs.trade_length = Some(index);
                            }
                        }
                    });
                    field(ui, &mut inputs.trade_reason, t("Reason"), control - length_width - ui.spacing().item_spacing.x)
                })
                .inner;
            ui.horizontal(|ui| {
                if (ui.add_enabled(ready, button(t("Lock"))).clicked() || enter) && ready {
                    hk_send(Hk::TradeLock { user: id, hours: TRADE_HOURS[length], reason: inputs.trade_reason.clone() });
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Kick"));
            let enter = field(ui, &mut inputs.kick_reason, t("Reason the player sees"), control);
            let hint = if online { "" } else { t("Only to a player who is online.") };
            ui.horizontal(|ui| {
                if (ui.add_enabled(ready && online, button(t("Kick"))).on_disabled_hover_text(hint).clicked() || (enter && online)) && ready {
                    hk_send(Hk::Kick { user: id, reason: inputs.kick_reason.clone() });
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Password"));
            ui.allocate_ui_with_layout(egui::vec2(control, ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_size(egui::vec2(control, ROW));
                widgets::muted(ui, palette, t("A new random password, shown once."), 12.5);
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(ready, button(t("Reset"))).clicked() {
                    hk_send(Hk::ResetPassword { user: id });
                }
            });
        });

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            action_label(ui, t("Alert"));
            ui.add_sized([control, 64.0], egui::TextEdit::multiline(&mut inputs.alert).hint_text(t("The message the player sees")));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let hint = if online { "" } else { t("Only to a player who is online.") };
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready && online, button(t("Send alert"))).on_disabled_hover_text(hint).clicked() {
                        send(Rcon::Alert(inputs.alert.trim().to_string()));
                    }
                    if ui.add_enabled(ready && online, button(t("Disconnect"))).on_disabled_hover_text(hint).clicked() {
                        send(Rcon::Disconnect);
                    }
                });
                if !online {
                    widgets::muted(ui, palette, hint, 12.0);
                }
            });
        });
    });
    actions.extend(pending.into_inner());
}

/// The height of a row of the actions, the width of their value column and the least width of a button.
const ROW: f32 = 28.0;
const ACTION_CONTROL: f32 = 300.0;
const ACTION_BUTTON: f32 = 96.0;

/// An action's name, in the label column, level with the first line of its row.
fn action_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(egui::vec2(72.0, ROW), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(egui::vec2(72.0, ROW));
        ui.label(RichText::new(text).size(12.5));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_read_as_words() {
        let ok = Answer { status: 0, message: "offline".into() };
        let (text, error) = answer_text(&Rcon::GiveCredits(5), "bob", &ok);
        assert!(!error && text.contains("offline"), "{text}");
        let (_, error) = answer_text(&Rcon::Disconnect, "bob", &Answer { status: rcon::USER_NOT_FOUND, message: String::new() });
        assert!(error);
        let (text, error) = answer_text(&Rcon::SetRank(99), "bob", &Answer { status: 1, message: "rank exceeds rcon ceiling".into() });
        assert!(error && text.contains("ceiling"), "{text}");
        let (text, error) = answer_text(&Rcon::Unban, "bob", &Answer { status: rcon::USER_NOT_FOUND, message: "no active ban".into() });
        assert!(error && text.contains("no active ban"), "{text}");
        let (text, error) = answer_text(&Rcon::Ban(60, "x".into(), BanKind::Account), "bob", &Answer { status: 1, message: rcon::UNKNOWN_COMMAND.into() });
        assert!(error && text.contains("banuser"), "{text}");
    }

    #[test]
    fn every_offered_length_has_a_name() {
        for seconds in MUTE_LENGTHS.iter().chain(BAN_LENGTHS.iter()) {
            assert_ne!(length_name(*seconds), t("Other length"), "{seconds}");
        }
        assert!(MUTE_LENGTHS.iter().all(|s| Rcon::Mute(*s).problem().is_none()));
        assert!(BAN_LENGTHS.iter().all(|s| Rcon::Ban(*s, "x".into(), BanKind::Account).problem().is_none()));
    }
}
