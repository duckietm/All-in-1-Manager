//! The hotel's housekeeping, as Octane's in-game panel does it, from the Manager: actions go to the running emulator
//! over RCON (`housekeeping`, the same work as the panel's packets, recorded in `housekeeping_log` as
//! "All-in-1 Manager"), lists are read from the hotel database through the SQL Editor's login (the same tables the
//! emulator reads them from). The RCON contract is `Gameserver/docs/rcon-housekeeping.md` in the emulator repository.

use serde_json::{json, Map, Value};

use crate::rcon::{self, Answer};
use crate::server::{Result, Server};
use crate::sql_session::{quote, SqlSession, SqlValue};

/// The RCON key of every housekeeping action.
pub const KEY: &str = "housekeeping";

/// Limits the emulator checks too, so a form says what is wrong before anything is sent.
pub const MAX_REASON: usize = 500;
pub const MAX_ALERT: usize = 1000;
pub const MAX_NOTE: usize = 500;
pub const MAX_MAINTENANCE_MESSAGE: usize = 200;
pub const MAX_MAINTENANCE_MINUTES: i64 = 120;
pub const MAX_ROOM_NAME: usize = 50;
pub const MAX_ROOM_DESCRIPTION: usize = 250;
pub const MAX_TAG: usize = 15;
pub const MAX_WORD: usize = 256;
pub const MAX_REPLACEMENT: usize = 16;
/// The longest temporary rank: a year.
pub const MAX_RANK_SECONDS: i64 = 31_536_000;

/// Who a hotel alert goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    Hotel,
    Staff,
    User(String),
    Room(i64),
}

impl Recipient {
    /// As the emulator reads it: nothing for everyone, "staff", "user:<name>", "room:<id>".
    pub fn text(&self) -> Option<String> {
        match self {
            Recipient::Hotel => None,
            Recipient::Staff => Some("staff".into()),
            Recipient::User(name) => Some(format!("user:{}", name.trim())),
            Recipient::Room(id) => Some(format!("room:{id}")),
        }
    }
}

/// A room's settings as the housekeeping panel changes them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoomSettings {
    pub name: String,
    pub description: String,
    pub users_max: i64,
    pub category: i64,
    /// 0 no trading, 1 with the owner's rights, 2 everyone.
    pub trade_mode: i64,
    pub tags: Vec<String>,
}

/// How a support ticket is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Resolved,
    Useless,
    Abusive,
}

impl Resolution {
    pub fn key(self) -> &'static str {
        match self {
            Resolution::Resolved => "resolved",
            Resolution::Useless => "useless",
            Resolution::Abusive => "abusive",
        }
    }
}

/// One housekeeping action.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Kick { user: i64, reason: String },
    TradeLock { user: i64, hours: i64, reason: String },
    ResetPassword { user: i64 },
    /// A rank for a time (seconds > 0), after which the old rank comes back.
    TemporaryRank { user: i64, rank: i64, seconds: i64 },
    GrantItem { user: i64, item: i64, quantity: i64 },
    SetClub { user: i64, days: i64 },
    AddNote { user: i64, note: String },
    DeleteNote { user: i64, note: i64 },
    OpenRoom { room: i64 },
    CloseRoom { room: i64 },
    RoomSettings { room: i64, settings: RoomSettings },
    /// Mutes the room until it is unmuted (`on` false).
    MuteRoom { room: i64, on: bool },
    KickAll { room: i64 },
    TransferRoom { room: i64, owner: i64 },
    DeleteRoom { room: i64 },
    Alert { message: String, to: Recipient },
    /// start (a countdown of `minutes`, 0 at once), cancel (the countdown) or disable (maintenance off).
    Maintenance { mode: &'static str, message: String, minutes: i64 },
    MaintenanceStatus,
    Lockdown { on: bool },
    LockdownStatus,
    AddWord { word: String, replacement: String },
    RemoveWord { word: String },
    SetPermission { permission: String, rank: i64, value: i64 },
    Reload { target: &'static str },
    RevokeBan { ban: i64 },
    Dashboard,
    Tickets,
    CloseTicket { ticket: i64, resolution: Resolution },
    ReplyTicket { ticket: i64, message: String, close: bool },
}

/// What `hotel.reload` loads again.
pub const RELOAD_TARGETS: &[&str] = &["catalog", "texts", "permissions", "items", "navigator", "config", "wordfilter"];

impl Action {
    /// The action's name in the emulator (and in `housekeeping_log`).
    pub fn name(&self) -> &'static str {
        match self {
            Action::Kick { .. } => "user.kick",
            Action::TradeLock { .. } => "user.trade_lock",
            Action::ResetPassword { .. } => "user.reset_password",
            Action::TemporaryRank { .. } => "user.set_rank",
            Action::GrantItem { .. } => "user.grant_item",
            Action::SetClub { .. } => "user.set_hc",
            Action::AddNote { .. } => "user.note.add",
            Action::DeleteNote { .. } => "user.note.delete",
            Action::OpenRoom { .. } => "room.open",
            Action::CloseRoom { .. } => "room.close",
            Action::RoomSettings { .. } => "room.settings",
            Action::MuteRoom { .. } => "room.mute",
            Action::KickAll { .. } => "room.kick_all",
            Action::TransferRoom { .. } => "room.transfer",
            Action::DeleteRoom { .. } => "room.delete",
            Action::Alert { .. } => "hotel.alert",
            Action::Maintenance { .. } => "hotel.maintenance",
            Action::MaintenanceStatus => "hotel.maintenance.status",
            Action::Lockdown { .. } => "hotel.lockdown",
            Action::LockdownStatus => "hotel.lockdown.status",
            Action::AddWord { .. } => "hotel.wordfilter.add",
            Action::RemoveWord { .. } => "hotel.wordfilter.remove",
            Action::SetPermission { .. } => "hotel.permission.set",
            Action::Reload { .. } => "hotel.reload",
            Action::RevokeBan { .. } => "ban.revoke",
            Action::Dashboard => "hotel.dashboard",
            Action::Tickets => "ticket.list",
            Action::CloseTicket { .. } => "ticket.close",
            Action::ReplyTicket { .. } => "ticket.reply",
        }
    }

    /// The fields beside `action`.
    fn fields(&self) -> Value {
        match self {
            Action::Kick { user, reason } => json!({ "user_id": user, "reason": reason.trim() }),
            Action::TradeLock { user, hours, reason } => json!({ "user_id": user, "hours": hours, "reason": reason.trim() }),
            Action::ResetPassword { user } => json!({ "user_id": user }),
            Action::TemporaryRank { user, rank, seconds } => json!({ "user_id": user, "rank": rank, "seconds": seconds }),
            Action::GrantItem { user, item, quantity } => json!({ "user_id": user, "item_id": item, "quantity": quantity }),
            Action::SetClub { user, days } => json!({ "user_id": user, "days": days }),
            Action::AddNote { user, note } => json!({ "user_id": user, "note": note.trim() }),
            Action::DeleteNote { user, note } => json!({ "user_id": user, "note_id": note }),
            Action::OpenRoom { room } | Action::CloseRoom { room } | Action::KickAll { room } | Action::DeleteRoom { room } => json!({ "room_id": room }),
            Action::RoomSettings { room, settings } => json!({
                "room_id": room,
                "name": settings.name.trim(),
                "description": settings.description.trim(),
                "users_max": settings.users_max,
                "category_id": settings.category,
                "trade_mode": settings.trade_mode,
                "tags": settings.tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>(),
            }),
            // The emulator reads any minutes above 0 as "muted until unmuted", 0 as unmute.
            Action::MuteRoom { room, on } => json!({ "room_id": room, "minutes": i64::from(*on) }),
            Action::TransferRoom { room, owner } => json!({ "room_id": room, "owner_id": owner }),
            Action::Alert { message, to } => match to.text() {
                Some(recipient) => json!({ "message": message.trim(), "recipient": recipient }),
                None => json!({ "message": message.trim() }),
            },
            Action::Maintenance { mode, message, minutes } => json!({ "mode": mode, "message": message.trim(), "minutes": minutes }),
            Action::Lockdown { on } => json!({ "enabled": on }),
            Action::AddWord { word, replacement } => json!({ "word": word.trim(), "replacement": replacement.trim() }),
            Action::RemoveWord { word } => json!({ "word": word.trim() }),
            Action::SetPermission { permission, rank, value } => json!({ "permission": permission, "rank": rank, "value": value }),
            Action::Reload { target } => json!({ "target": target }),
            Action::RevokeBan { ban } => json!({ "ban_id": ban }),
            Action::CloseTicket { ticket, resolution } => json!({ "ticket_id": ticket, "resolution": resolution.key() }),
            Action::ReplyTicket { ticket, message, close } => json!({ "ticket_id": ticket, "message": message.trim(), "close": close }),
            Action::MaintenanceStatus | Action::LockdownStatus | Action::Dashboard | Action::Tickets => json!({}),
        }
    }

    /// The RCON request.
    pub fn request(&self) -> String {
        let mut data = Map::new();
        data.insert("action".into(), json!(self.name()));
        if let Value::Object(fields) = self.fields() {
            data.extend(fields);
        }
        rcon::request(KEY, Value::Object(data))
    }

    /// What is wrong with it before it is sent (the emulator checks the same), or None.
    pub fn problem(&self) -> Option<String> {
        let long = |text: &str, max: usize| text.trim().chars().count() > max;
        match self {
            Action::Kick { user, reason } | Action::TradeLock { user, reason, .. } if *user <= 0 || long(reason, MAX_REASON) => {
                Some(format!("Choose a player, and a reason of at most {MAX_REASON} characters."))
            }
            Action::TradeLock { hours, .. } if *hours <= 0 => Some("The trade lock needs a length.".into()),
            Action::TemporaryRank { rank, seconds, .. } if *rank <= 0 || !(1..=MAX_RANK_SECONDS).contains(seconds) => {
                Some("Choose a rank and how long it lasts (at most a year).".into())
            }
            Action::GrantItem { item, quantity, .. } if *item <= 0 || !(1..=100).contains(quantity) => {
                Some("Choose the furni (its items_base id) and 1 to 100 of it.".into())
            }
            Action::SetClub { days, .. } if !(0..=3650).contains(days) => Some("Habbo Club days are 0 (end it) to 3650.".into()),
            Action::AddWord { word, replacement } if long(word, MAX_WORD) || long(replacement, MAX_REPLACEMENT) => {
                Some(format!("A word is at most {MAX_WORD} characters, its replacement at most {MAX_REPLACEMENT}."))
            }
            Action::AddNote { note, .. } if note.trim().is_empty() || long(note, MAX_NOTE) => {
                Some(format!("A note is 1 to {MAX_NOTE} characters."))
            }
            Action::RoomSettings { settings, .. } => {
                if settings.name.trim().is_empty() || long(&settings.name, MAX_ROOM_NAME) {
                    Some(format!("A room name is 1 to {MAX_ROOM_NAME} characters."))
                } else if long(&settings.description, MAX_ROOM_DESCRIPTION) {
                    Some(format!("The description is at most {MAX_ROOM_DESCRIPTION} characters."))
                } else if !(1..=200).contains(&settings.users_max) {
                    Some("Maximum users is 1 to 200.".into())
                } else if !(0..=2).contains(&settings.trade_mode) {
                    Some("Trading is 0, 1 or 2.".into())
                } else if settings.tags.iter().filter(|t| !t.trim().is_empty()).count() > 2
                    || settings.tags.iter().any(|t| t.trim().chars().count() > MAX_TAG || t.contains(';'))
                {
                    Some(format!("At most 2 tags of at most {MAX_TAG} characters, without ;."))
                } else {
                    None
                }
            }
            Action::TransferRoom { owner, .. } if *owner <= 0 => Some("Choose the new owner.".into()),
            Action::Alert { message, to } => {
                if message.trim().is_empty() || long(message, MAX_ALERT) {
                    Some(format!("An alert is 1 to {MAX_ALERT} characters."))
                } else if matches!(to, Recipient::User(name) if name.trim().is_empty()) || matches!(to, Recipient::Room(id) if *id <= 0) {
                    Some("Choose the player or the room the alert goes to.".into())
                } else {
                    None
                }
            }
            Action::Maintenance { minutes, message, .. } if !(0..=MAX_MAINTENANCE_MINUTES).contains(minutes) || long(message, MAX_MAINTENANCE_MESSAGE) => {
                Some(format!("Maintenance starts in 0 to {MAX_MAINTENANCE_MINUTES} minutes, with a message of at most {MAX_MAINTENANCE_MESSAGE} characters."))
            }
            Action::AddWord { word, .. } | Action::RemoveWord { word } if word.trim().is_empty() => Some("Type the word.".into()),
            Action::ReplyTicket { message, .. } if message.trim().is_empty() || long(message, MAX_ALERT) => {
                Some(format!("A reply is 1 to {MAX_ALERT} characters."))
            }
            _ => None,
        }
    }
}

/// Sends the action and returns the emulator's answer.
pub async fn send(server: &dyn Server, action: &Action) -> Result<Answer> {
    rcon::send(server, &action.request()).await
}

/// The emulator does not know `housekeeping` (an emulator before it).
pub fn unsupported(answer: &Answer) -> bool {
    rcon::is_unknown_command(answer)
}

// ---- Answers of the query actions ------------------------------------------------------------------------------

fn num(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0)
}

fn string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// The hotel's numbers (`hotel.dashboard`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dashboard {
    pub online_users: i64,
    pub total_users: i64,
    pub active_rooms: i64,
    pub total_rooms: i64,
    pub peak_today: i64,
    pub peak_all_time: i64,
    pub pending_tickets: i64,
    pub uptime_seconds: i64,
    pub version: String,
}

pub fn parse_dashboard(message: &str) -> Option<Dashboard> {
    let v: Value = serde_json::from_str(message).ok()?;
    v.is_object().then(|| Dashboard {
        online_users: num(&v, "online_users"),
        total_users: num(&v, "total_users"),
        active_rooms: num(&v, "active_rooms"),
        total_rooms: num(&v, "total_rooms"),
        peak_today: num(&v, "peak_online_today"),
        peak_all_time: num(&v, "peak_online_all_time"),
        pending_tickets: num(&v, "pending_tickets"),
        uptime_seconds: num(&v, "server_uptime_seconds"),
        version: string(&v, "server_version"),
    })
}

/// Maintenance as it is now (`hotel.maintenance.status`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceState {
    pub enabled: bool,
    pub min_rank: i64,
    pub message: String,
    /// Unix time the countdown ends, 0 for none.
    pub countdown_ends_at: i64,
}

pub fn parse_maintenance(message: &str) -> Option<MaintenanceState> {
    let v: Value = serde_json::from_str(message).ok()?;
    v.is_object().then(|| MaintenanceState {
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        min_rank: num(&v, "min_rank"),
        message: string(&v, "message"),
        countdown_ends_at: num(&v, "countdown_ends_at"),
    })
}

/// Whether the lockdown is on (`hotel.lockdown.status`).
pub fn parse_lockdown(message: &str) -> Option<bool> {
    serde_json::from_str::<Value>(message).ok()?.get("enabled").and_then(Value::as_bool)
}

/// The password `user.reset_password` made, when it says one.
pub fn parse_password(message: &str) -> Option<String> {
    serde_json::from_str::<Value>(message).ok()?.get("password").and_then(Value::as_str).map(str::to_string)
}

/// A call for help (`ticket.list`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ticket {
    pub id: i64,
    pub state: String,
    pub category: i64,
    pub reporter_id: i64,
    pub reporter: String,
    pub reported_id: i64,
    pub reported: String,
    pub moderator: String,
    pub message: String,
    pub room_id: i64,
    pub room: String,
    pub created_at: i64,
}

pub fn parse_tickets(message: &str) -> Vec<Ticket> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(message) else { return Vec::new() };
    list.iter()
        .map(|t| Ticket {
            id: num(t, "id"),
            state: string(t, "state"),
            category: num(t, "category_id"),
            reporter_id: num(t, "reporter_id"),
            reporter: string(t, "reporter_name"),
            reported_id: num(t, "reported_id"),
            reported: string(t, "reported_name"),
            moderator: string(t, "moderator_name"),
            message: string(t, "message"),
            room_id: num(t, "room_id"),
            room: string(t, "room_name"),
            created_at: num(t, "created_at"),
        })
        .collect()
}

// ---- Lists from the hotel database -----------------------------------------------------------------------------

pub(crate) fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
        Some(SqlValue::Float(v)) => *v as i64,
        Some(SqlValue::Double(v)) => *v as i64,
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

pub(crate) fn text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) | Some(SqlValue::Date(t)) | Some(SqlValue::DateTime(t)) | Some(SqlValue::Time(t)) => t.clone(),
        Some(SqlValue::Int(v)) => v.to_string(),
        Some(SqlValue::UInt(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// Unix time as "YYYY-MM-DD HH:MM" in this computer's time zone; "-" for none.
pub fn when(timestamp: i64) -> String {
    let text = crate::catalog_admin::expiry_text(timestamp);
    if text.is_empty() {
        "-".into()
    } else {
        text
    }
}

/// A player who is online.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OnlinePlayer {
    pub id: i64,
    pub name: String,
    pub rank: String,
    pub room_id: i64,
    pub room: String,
    pub ip: String,
    pub since: i64,
}

/// The players online, with the room they are in now (their last room entry without an exit).
pub async fn online(session: &SqlSession, database: &str) -> Result<Vec<OnlinePlayer>> {
    let db = quote(database);
    let sql = format!(
        "SELECT u.id, u.username, COALESCE(p.rank_name, u.`rank`), COALESCE(r.id, 0), COALESCE(r.name, ''), u.ip_current, u.last_login \
         FROM {db}.users u LEFT JOIN {db}.permission_ranks p ON p.id = u.`rank` \
         LEFT JOIN {db}.room_enter_log e ON e.id = (SELECT MAX(x.id) FROM {db}.room_enter_log x WHERE x.user_id = u.id AND x.exit_timestamp = 0) \
         LEFT JOIN {db}.rooms r ON r.id = e.room_id \
         WHERE u.online = '1' ORDER BY u.username LIMIT 1000"
    );
    let rows = session.select(&sql, Vec::new()).await?;
    Ok(rows
        .iter()
        .map(|r| OnlinePlayer { id: int(r.first()), name: text(r.get(1)), rank: text(r.get(2)), room_id: int(r.get(3)), room: text(r.get(4)), ip: text(r.get(5)), since: int(r.get(6)) })
        .collect())
}

/// The hotel's numbers from the database, when the emulator's are not there: players online and in all, rooms with
/// people in them and in all.
pub async fn dashboard_from_database(session: &SqlSession, database: &str) -> Result<Dashboard> {
    let db = quote(database);
    let sql = format!(
        "SELECT (SELECT COUNT(*) FROM {db}.users WHERE online = '1'), (SELECT COUNT(*) FROM {db}.users), \
         (SELECT COUNT(*) FROM {db}.rooms WHERE users > 0), (SELECT COUNT(*) FROM {db}.rooms)"
    );
    let row = session.select(&sql, Vec::new()).await?.into_iter().next().unwrap_or_default();
    Ok(Dashboard { online_users: int(row.first()), total_users: int(row.get(1)), active_rooms: int(row.get(2)), total_rooms: int(row.get(3)), ..Dashboard::default() })
}

/// A room as the Rooms tab lists and edits it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Room {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub owner_id: i64,
    pub owner: String,
    pub users: i64,
    pub users_max: i64,
    pub state: String,
    pub category: i64,
    pub trade_mode: i64,
    pub tags: Vec<String>,
    pub public: bool,
}

impl Room {
    pub fn settings(&self) -> RoomSettings {
        RoomSettings {
            name: self.name.clone(),
            description: self.description.clone(),
            users_max: self.users_max,
            category: self.category,
            trade_mode: self.trade_mode,
            tags: self.tags.clone(),
        }
    }
}

const ROOM_COLUMNS: &str = "id, name, description, owner_id, owner_name, users, users_max, state, category, trade_mode, tags, is_public";

fn room_from_row(r: &[SqlValue]) -> Room {
    Room {
        id: int(r.first()),
        name: text(r.get(1)),
        description: text(r.get(2)),
        owner_id: int(r.get(3)),
        owner: text(r.get(4)),
        users: int(r.get(5)),
        users_max: int(r.get(6)),
        state: text(r.get(7)),
        category: int(r.get(8)),
        trade_mode: int(r.get(9)),
        tags: text(r.get(10)).split(';').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect(),
        public: text(r.get(11)) == "1",
    }
}

/// Rooms by id, name or owner (empty: the rooms with people in them, fullest first).
pub async fn rooms(session: &SqlSession, database: &str, search: &str) -> Result<Vec<Room>> {
    let db = quote(database);
    let search = search.trim();
    let (sql, params) = if search.is_empty() {
        (format!("SELECT {ROOM_COLUMNS} FROM {db}.rooms WHERE users > 0 ORDER BY users DESC, id LIMIT 200"), Vec::new())
    } else {
        let like = format!("%{}%", search.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let id = search.parse::<i64>().unwrap_or(-1);
        (
            format!("SELECT {ROOM_COLUMNS} FROM {db}.rooms WHERE id = ? OR name LIKE ? OR owner_name LIKE ? ORDER BY users DESC, id LIMIT 200"),
            vec![SqlValue::Int(id), SqlValue::Text(like.clone()), SqlValue::Text(like)],
        )
    };
    Ok(session.select(&sql, params).await?.iter().map(|r| room_from_row(r)).collect())
}

pub async fn room(session: &SqlSession, database: &str, id: i64) -> Result<Option<Room>> {
    let sql = format!("SELECT {ROOM_COLUMNS} FROM {}.rooms WHERE id = ?", quote(database));
    Ok(session.select(&sql, vec![SqlValue::Int(id)]).await?.first().map(|r| room_from_row(r)))
}

/// The room categories (navigator), for the settings form.
pub async fn room_categories(session: &SqlSession, database: &str) -> Result<Vec<(i64, String)>> {
    let sql = format!("SELECT id, caption FROM {}.navigator_flatcats ORDER BY order_num, id", quote(database));
    Ok(session.select(&sql, Vec::new()).await?.iter().map(|r| (int(r.first()), text(r.get(1)))).collect())
}

/// An active ban.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ban {
    pub id: i64,
    pub user_id: i64,
    pub user: String,
    pub kind: String,
    pub reason: String,
    pub expires: i64,
    pub staff: String,
    pub time: i64,
    pub ip: String,
}

/// The active bans of the hotel, newest first (as the emulator's ban list reads them).
pub async fn bans(session: &SqlSession, database: &str) -> Result<Vec<Ban>> {
    let db = quote(database);
    let now = chrono::Utc::now().timestamp();
    let sql = format!(
        "SELECT b.id, b.user_id, COALESCE(t.username, ''), b.type, b.ban_reason, b.ban_expire, COALESCE(s.username, ''), b.timestamp, b.ip \
         FROM {db}.bans b LEFT JOIN {db}.users t ON t.id = b.user_id LEFT JOIN {db}.users s ON s.id = b.user_staff_id \
         WHERE b.ban_expire > ? ORDER BY b.timestamp DESC LIMIT 500"
    );
    let rows = session.select(&sql, vec![SqlValue::Int(now)]).await?;
    Ok(rows
        .iter()
        .map(|r| Ban {
            id: int(r.first()),
            user_id: int(r.get(1)),
            user: text(r.get(2)),
            kind: text(r.get(3)),
            reason: text(r.get(4)),
            expires: int(r.get(5)),
            staff: text(r.get(6)),
            time: int(r.get(7)),
            ip: text(r.get(8)),
        })
        .collect())
}

/// A word of the word filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Word {
    pub word: String,
    pub replacement: String,
    pub hide: bool,
    pub report: bool,
    pub mute: i64,
    pub prefix: bool,
}

pub async fn word_filter(session: &SqlSession, database: &str) -> Result<Vec<Word>> {
    let sql = format!("SELECT `key`, replacement, hide, report, mute, prefix_only FROM {}.wordfilter ORDER BY `key`", quote(database));
    Ok(session
        .select(&sql, Vec::new())
        .await?
        .iter()
        .map(|r| Word { word: text(r.first()), replacement: text(r.get(1)), hide: text(r.get(2)) == "1", report: text(r.get(3)) == "1", mute: int(r.get(4)), prefix: text(r.get(5)) == "1" })
        .collect())
}

/// The permission grid: every permission against every rank.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    pub ranks: Vec<(i64, String)>,
    pub rows: Vec<PermissionRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionRow {
    pub key: String,
    pub comment: String,
    /// 1 for a yes/no permission; 2 when it also has "with rights" (2).
    pub max: i64,
    /// One per rank, in the order of `ranks`.
    pub values: Vec<i64>,
}

pub async fn permissions(session: &SqlSession, database: &str) -> Result<Permissions> {
    let db = quote(database);
    let ranks: Vec<(i64, String)> =
        session.select(&format!("SELECT id, rank_name FROM {db}.permission_ranks ORDER BY id"), Vec::new()).await?.iter().map(|r| (int(r.first()), text(r.get(1)))).collect();
    let columns: Vec<String> = session
        .select(
            "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = 'permission_definitions'",
            vec![SqlValue::Text(database.to_string())],
        )
        .await?
        .iter()
        .map(|r| text(r.first()).to_lowercase())
        .collect();
    // A rank without its column in the table has every permission off.
    let picks: Vec<String> = ranks.iter().map(|(id, _)| if columns.contains(&format!("rank_{id}")) { format!("`rank_{id}`") } else { "0".into() }).collect();
    let select = if picks.is_empty() { String::new() } else { format!(", {}", picks.join(", ")) };
    let comment = if columns.contains(&"comment".to_string()) { "COALESCE(comment, '')" } else { "''" };
    let max = if columns.contains(&"max_value".to_string()) { "max_value" } else { "1" };
    let sql = format!("SELECT permission_key, {comment}, {max}{select} FROM {db}.permission_definitions ORDER BY permission_key");
    let rows = session
        .select(&sql, Vec::new())
        .await?
        .iter()
        .map(|r| PermissionRow { key: text(r.first()), comment: text(r.get(1)), max: int(r.get(2)).max(1), values: (0..ranks.len()).map(|i| int(r.get(3 + i))).collect() })
        .collect();
    Ok(Permissions { ranks, rows })
}

/// A staff note on a player.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    pub id: i64,
    pub note: String,
    pub author: String,
    pub created_at: i64,
}

pub async fn notes(session: &SqlSession, database: &str, user: i64) -> Result<Vec<Note>> {
    let sql = format!("SELECT id, note, author_name, created_at FROM {}.housekeeping_user_notes WHERE user_id = ? ORDER BY id DESC LIMIT 200", quote(database));
    Ok(session.select(&sql, vec![SqlValue::Int(user)]).await?.iter().map(|r| Note { id: int(r.first()), note: text(r.get(1)), author: text(r.get(2)), created_at: int(r.get(3)) }).collect())
}

/// Another account on the same IP.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloneAccount {
    pub id: i64,
    pub name: String,
    pub last_online: i64,
    pub ip: String,
}

/// The accounts that registered or log in from an IP this player used, as the emulator finds clones.
pub async fn clones(session: &SqlSession, database: &str, user: i64) -> Result<Vec<CloneAccount>> {
    let db = quote(database);
    let sql = format!(
        "SELECT o.id, o.username, o.last_online, o.ip_current FROM {db}.users o JOIN {db}.users u ON u.id = ? \
         WHERE o.id <> u.id AND ((u.ip_register <> '' AND (o.ip_register = u.ip_register OR o.ip_current = u.ip_register)) \
         OR (u.ip_current <> '' AND (o.ip_register = u.ip_current OR o.ip_current = u.ip_current))) ORDER BY o.id DESC LIMIT 50"
    );
    Ok(session.select(&sql, vec![SqlValue::Int(user)]).await?.iter().map(|r| CloneAccount { id: int(r.first()), name: text(r.get(1)), last_online: int(r.get(2)), ip: text(r.get(3)) }).collect())
}

/// What the panel hides behind "reveal": the IPs and the mail address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Private {
    pub ip_current: String,
    pub ip_register: String,
    pub mail: String,
}

pub async fn private(session: &SqlSession, database: &str, user: i64) -> Result<Private> {
    let sql = format!("SELECT ip_current, ip_register, COALESCE(mail, '') FROM {}.users WHERE id = ?", quote(database));
    let row = session.select(&sql, vec![SqlValue::Int(user)]).await?.into_iter().next().unwrap_or_default();
    Ok(Private { ip_current: text(row.first()), ip_register: text(row.get(1)), mail: text(row.get(2)) })
}

/// A sanction of the mod tools' ladder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sanction {
    pub level: i64,
    pub reason: String,
    pub probation_until: i64,
    pub trade_locked_until: i64,
    pub mute_minutes: i64,
}

pub async fn sanctions(session: &SqlSession, database: &str, user: i64) -> Result<Vec<Sanction>> {
    let sql = format!(
        "SELECT sanction_level, reason, probation_timestamp, trade_locked_until, mute_duration FROM {}.sanctions WHERE habbo_id = ? ORDER BY id DESC LIMIT 100",
        quote(database)
    );
    Ok(session
        .select(&sql, vec![SqlValue::Int(user)])
        .await?
        .iter()
        .map(|r| Sanction { level: int(r.first()), reason: text(r.get(1)), probation_until: int(r.get(2)), trade_locked_until: int(r.get(3)), mute_minutes: int(r.get(4)) })
        .collect())
}

/// A temporary rank that is running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemporaryRank {
    pub rank: i64,
    pub previous: i64,
    pub expires_at: i64,
    pub staff: String,
    pub reason: String,
}

pub async fn temporary_rank(session: &SqlSession, database: &str, user: i64) -> Result<Option<TemporaryRank>> {
    let sql = format!(
        "SELECT rank_id, previous_rank_id, expires_at, COALESCE(set_by_name, ''), COALESCE(reason, '') FROM {}.user_temporary_ranks WHERE user_id = ? ORDER BY expires_at DESC LIMIT 1",
        quote(database)
    );
    Ok(session
        .select(&sql, vec![SqlValue::Int(user)])
        .await?
        .first()
        .map(|r| TemporaryRank { rank: int(r.first()), previous: int(r.get(1)), expires_at: int(r.get(2)), staff: text(r.get(3)), reason: text(r.get(4)) }))
}

/// A sound of the soundboard catalogue (`soundboard_sounds`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sound {
    pub id: i64,
    pub name: String,
    pub classname: String,
    pub url: String,
    pub enabled: bool,
    pub sort_order: i64,
    pub min_rank: i64,
    pub cooldown_seconds: i64,
}

pub const MAX_SOUND_NAME: usize = 64;
pub const MAX_SOUND_URL: usize = 255;
pub const MAX_COOLDOWN: i64 = 3600;

impl Sound {
    /// What the emulator's catalogue checks would refuse, or None.
    pub fn problem(&self) -> Option<String> {
        if self.name.trim().is_empty() || self.name.trim().chars().count() > MAX_SOUND_NAME {
            Some(format!("A sound's name is 1 to {MAX_SOUND_NAME} characters."))
        } else if self.url.trim().is_empty() || self.url.trim().chars().count() > MAX_SOUND_URL {
            Some(format!("A sound's address is 1 to {MAX_SOUND_URL} characters."))
        } else if !(self.url.trim().starts_with('/') || self.url.trim().starts_with("https://") || self.url.trim().starts_with("http://")) {
            Some("A sound's address starts with / (on the hotel) or http(s)://.".into())
        } else if !self.classname.trim().is_empty() && !self.classname.trim().chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            Some("A class name has letters, digits, _ and - only.".into())
        } else if self.min_rank < 1 {
            Some("The lowest rank is at least 1.".into())
        } else if !(0..=MAX_COOLDOWN).contains(&self.cooldown_seconds) {
            Some(format!("The cooldown is 0 to {MAX_COOLDOWN} seconds."))
        } else {
            None
        }
    }
}

pub async fn sounds(session: &SqlSession, database: &str) -> Result<Vec<Sound>> {
    let db = quote(database);
    let columns: Vec<String> = session
        .select(
            "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = 'soundboard_sounds'",
            vec![SqlValue::Text(database.to_string())],
        )
        .await?
        .iter()
        .map(|r| text(r.first()).to_lowercase())
        .collect();
    let classname = if columns.contains(&"classname".to_string()) { "COALESCE(classname, '')" } else { "''" };
    let cooldown = if columns.contains(&"cooldown_seconds".to_string()) { "cooldown_seconds" } else { "0" };
    let sql = format!("SELECT id, name, {classname}, url, enabled, sort_order, min_rank, {cooldown} FROM {db}.soundboard_sounds ORDER BY sort_order, id");
    Ok(session
        .select(&sql, Vec::new())
        .await?
        .iter()
        .map(|r| Sound {
            id: int(r.first()),
            name: text(r.get(1)),
            classname: text(r.get(2)),
            url: text(r.get(3)),
            enabled: int(r.get(4)) == 1,
            sort_order: int(r.get(5)),
            min_rank: int(r.get(6)),
            cooldown_seconds: int(r.get(7)),
        })
        .collect())
}

/// Saves a sound (a new one when id is 0), then the emulator loads the catalogue again (`updatesoundboard`).
pub async fn save_sound(session: &SqlSession, database: &str, sound: &Sound) -> Result<i64> {
    let db = quote(database);
    let classname = if sound.classname.trim().is_empty() { SqlValue::Null } else { SqlValue::Text(sound.classname.trim().to_string()) };
    let enabled = SqlValue::Int(i64::from(sound.enabled));
    if sound.id <= 0 {
        let sql = format!(
            "INSERT INTO {db}.soundboard_sounds (name, classname, url, enabled, sort_order, min_rank, cooldown_seconds) \
             SELECT ?, ?, ?, ?, COALESCE(MAX(sort_order), 0) + 10, ?, ? FROM {db}.soundboard_sounds"
        );
        let params = vec![
            SqlValue::Text(sound.name.trim().to_string()),
            classname,
            SqlValue::Text(sound.url.trim().to_string()),
            enabled,
            SqlValue::Int(sound.min_rank),
            SqlValue::Int(sound.cooldown_seconds),
        ];
        return Ok(session.execute(&[(sql, params)]).await?.1 as i64);
    }
    let sql = format!("UPDATE {db}.soundboard_sounds SET name = ?, classname = ?, url = ?, enabled = ?, min_rank = ?, cooldown_seconds = ? WHERE id = ?");
    let params = vec![
        SqlValue::Text(sound.name.trim().to_string()),
        classname,
        SqlValue::Text(sound.url.trim().to_string()),
        enabled,
        SqlValue::Int(sound.min_rank),
        SqlValue::Int(sound.cooldown_seconds),
        SqlValue::Int(sound.id),
    ];
    session.execute(&[(sql, params)]).await?;
    Ok(sound.id)
}

pub async fn delete_sound(session: &SqlSession, database: &str, id: i64) -> Result<()> {
    session.execute(&[(format!("DELETE FROM {}.soundboard_sounds WHERE id = ?", quote(database)), vec![SqlValue::Int(id)])]).await?;
    Ok(())
}

/// Puts the sounds in this order (10, 20, 30 ...), in one transaction.
pub async fn order_sounds(session: &SqlSession, database: &str, ids: &[i64]) -> Result<()> {
    let sql = format!("UPDATE {}.soundboard_sounds SET sort_order = ? WHERE id = ?", quote(database));
    let statements: Vec<_> = ids.iter().enumerate().map(|(i, id)| (sql.clone(), vec![SqlValue::Int((i as i64 + 1) * 10), SqlValue::Int(*id)])).collect();
    session.execute(&statements).await?;
    Ok(())
}

/// The RCON request that makes the emulator load the soundboard catalogue again.
pub fn reload_sounds_request() -> String {
    rcon::request("updatesoundboard", json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(action: &Action) -> Value {
        serde_json::from_str::<Value>(&action.request()).unwrap()["data"].clone()
    }

    #[test]
    fn requests_follow_the_emulators_contract() {
        let v: Value = serde_json::from_str(&Action::Kick { user: 7, reason: " spam ".into() }.request()).unwrap();
        assert_eq!(v["key"], KEY);
        assert_eq!((v["data"]["action"].as_str(), v["data"]["user_id"].as_i64(), v["data"]["reason"].as_str()), (Some("user.kick"), Some(7), Some("spam")));
        let d = data(&Action::RoomSettings { room: 3, settings: RoomSettings { name: "Lobby".into(), users_max: 25, tags: vec!["a".into(), " ".into()], ..RoomSettings::default() } });
        assert_eq!((d["action"].as_str(), d["users_max"].as_i64(), d["tags"].as_array().map(Vec::len)), (Some("room.settings"), Some(25), Some(1)));
        assert!(data(&Action::Alert { message: "hi".into(), to: Recipient::Hotel }).get("recipient").is_none(), "everyone: no recipient");
        assert_eq!(data(&Action::Alert { message: "hi".into(), to: Recipient::Room(12) })["recipient"], "room:12");
        assert_eq!(data(&Action::Alert { message: "hi".into(), to: Recipient::User(" bob ".into()) })["recipient"], "user:bob");
        assert_eq!(data(&Action::CloseTicket { ticket: 4, resolution: Resolution::Abusive })["resolution"], "abusive");
        assert_eq!(data(&Action::Dashboard)["action"], "hotel.dashboard");
    }

    #[test]
    fn forms_are_checked_as_the_emulator_checks_them() {
        assert!(Action::Kick { user: 0, reason: String::new() }.problem().is_some());
        assert!(Action::Kick { user: 1, reason: "x".repeat(MAX_REASON + 1) }.problem().is_some());
        assert!(Action::Kick { user: 1, reason: "spam".into() }.problem().is_none());
        assert!(Action::TradeLock { user: 1, hours: 0, reason: String::new() }.problem().is_some());
        assert!(Action::GrantItem { user: 1, item: 5, quantity: 101 }.problem().is_some());
        assert!(Action::Maintenance { mode: "start", message: String::new(), minutes: 121 }.problem().is_some());
        let room = |tags: Vec<&str>| Action::RoomSettings { room: 1, settings: RoomSettings { name: "x".into(), users_max: 10, tags: tags.into_iter().map(String::from).collect(), ..RoomSettings::default() } };
        assert!(room(vec!["a", "b"]).problem().is_none());
        assert!(room(vec!["a", "b", "c"]).problem().is_some());
        assert!(room(vec!["a;b"]).problem().is_some());
        assert!(Action::Alert { message: "hi".into(), to: Recipient::Room(0) }.problem().is_some());
    }

    #[test]
    fn answers_are_read() {
        let d = parse_dashboard(r#"{"online_users":5,"total_users":"90","peak_online_today":7,"server_version":"4.2"}"#).unwrap();
        assert_eq!((d.online_users, d.total_users, d.peak_today, d.version.as_str()), (5, 90, 7, "4.2"));
        assert_eq!(parse_maintenance(r#"{"enabled":true,"min_rank":5,"message":"brb","countdown_ends_at":100}"#).unwrap().countdown_ends_at, 100);
        assert_eq!(parse_lockdown(r#"{"enabled":false}"#), Some(false));
        assert_eq!(parse_password(r#"{"password":"Ab12"}"#).as_deref(), Some("Ab12"));
        let tickets = parse_tickets(r#"[{"id":3,"reporter_name":"amy","reported_id":9,"message":"rude","created_at":1700000000}]"#);
        assert_eq!((tickets[0].id, tickets[0].reporter.as_str(), tickets[0].reported_id), (3, "amy", 9));
        assert!(parse_tickets("not json").is_empty());
    }

    #[test]
    fn sounds_are_checked() {
        let ok = Sound { name: "Bell".into(), url: "/sounds/bell.mp3".into(), min_rank: 1, ..Sound::default() };
        assert_eq!(ok.problem(), None);
        assert!(Sound { url: "javascript:alert(1)".into(), ..ok.clone() }.problem().is_some());
        assert!(Sound { classname: "a b".into(), ..ok.clone() }.problem().is_some());
        assert!(Sound { cooldown_seconds: -1, ..ok }.problem().is_some());
    }
}
