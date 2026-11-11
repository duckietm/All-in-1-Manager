//! Players: find a player by name or id, read what the hotel stores about them, and act on them through the
//! emulator's RCON (give credits, points or a badge, alert, disconnect, set the rank).
//!
//! Reading goes through the SQL session; every value is a statement parameter and a name is matched as a prefix
//! with LIKE, its `%`, `_` and `\` escaped. The password hash, the SSO ticket, the remember-me token, the secret key
//! and the pincode are never selected. Changes go through RCON only, so the emulator applies them to an online player
//! straight away and keeps its own checks (the grant ceiling, the highest rank RCON may give).

use std::collections::HashSet;

use serde_json::json;

use crate::catalog_admin::expiry_text;
use crate::rcon::{self, Answer};
use crate::server::{Result, Server};
use crate::sql_session::{quote, SqlSession, SqlValue};

/// Players on one page of a search.
pub const PAGE_ROWS: i64 = 100;

/// Room visits shown.
pub const VISIT_ROWS: i64 = 15;

/// Bans shown.
pub const BAN_ROWS: i64 = 20;

/// What the emulator's RCON accepts in a badge code (`GiveBadge`); the table column is 32 characters.
pub const BADGE_MAX: usize = 32;

/// The longest alert RCON takes.
pub const ALERT_MAX: usize = 4096;

/// The longest ban reason: the `bans.ban_reason` column is 200 characters.
pub const BAN_REASON_MAX: usize = 200;

/// The longest ban Polaris' `banuser` takes, 50 years in seconds; the manager's "permanent".
pub const BAN_MAX_SECONDS: i64 = 50 * 365 * 24 * 60 * 60;

/// The longest mute the emulator's `muteuser` takes by default (`rcon.mute.max_duration_seconds`), a week.
pub const MUTE_MAX_SECONDS: i64 = 7 * 24 * 60 * 60;

/// What a ban covers, as Polaris names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BanKind {
    /// The account only.
    Account,
    /// Every account on the player's last address.
    Ip,
    /// Every account on the player's machine.
    Machine,
    /// Address and machine.
    Super,
}

impl BanKind {
    pub const ALL: [BanKind; 4] = [BanKind::Account, BanKind::Ip, BanKind::Machine, BanKind::Super];

    pub fn key(self) -> &'static str {
        match self {
            BanKind::Account => "account",
            BanKind::Ip => "ip",
            BanKind::Machine => "machine",
            BanKind::Super => "super",
        }
    }
}

fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
        Some(SqlValue::Float(v)) => *v as i64,
        Some(SqlValue::Double(v)) => *v as i64,
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.trim().parse().unwrap_or(0),
        Some(SqlValue::Bytes(b)) => String::from_utf8_lossy(b).trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Int(v)) => v.to_string(),
        Some(SqlValue::UInt(v)) => v.to_string(),
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.clone(),
        Some(SqlValue::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

/// `text%` for LIKE, the `\`, `%` and `_` in it escaped.
pub fn prefix_pattern(text: &str) -> String {
    format!("{}%", text.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// A player in a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerRow {
    pub id: i64,
    pub name: String,
    pub rank: i64,
    pub online: bool,
    pub last_online: String,
    pub motto: String,
}

#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    pub rows: Vec<PlayerRow>,
    /// Whether there is a next page.
    pub more: bool,
}

/// A page of players whose name starts with `query`, or whose id is `query` when it is a number; online players
/// first, then the ones seen last.
pub fn search_sql(database: &str, query: &str, online_only: bool, page: i64) -> (String, Vec<SqlValue>) {
    let query = query.trim();
    let mut conditions = Vec::new();
    let mut params = Vec::new();
    if !query.is_empty() {
        match query.parse::<i64>() {
            Ok(id) => {
                conditions.push("(`id` = ? OR `username` LIKE ?)");
                params.push(SqlValue::Int(id));
            }
            Err(_) => conditions.push("`username` LIKE ?"),
        }
        params.push(SqlValue::Text(prefix_pattern(query)));
    }
    if online_only {
        conditions.push("`online` = '1'");
    }
    let filter = if conditions.is_empty() { String::new() } else { format!(" WHERE {}", conditions.join(" AND ")) };
    params.push(SqlValue::Int(page.max(0) * PAGE_ROWS));
    let sql = format!(
        "SELECT `id`, `username`, `rank`, `online`, `last_online`, `motto` FROM {}.`users`{filter} \
         ORDER BY `online` = '1' DESC, `last_online` DESC, `id` DESC LIMIT {} OFFSET ?",
        quote(database),
        PAGE_ROWS + 1
    );
    (sql, params)
}

pub async fn search(session: &SqlSession, database: &str, query: &str, online_only: bool, page: i64) -> Result<SearchPage> {
    let (sql, params) = search_sql(database, query, online_only, page);
    let rows = session.select(&sql, params).await?;
    let more = rows.len() as i64 > PAGE_ROWS;
    let rows = rows
        .iter()
        .take(PAGE_ROWS as usize)
        .map(|r| PlayerRow {
            id: int(r.first()),
            name: text(r.get(1)),
            rank: int(r.get(2)),
            online: text(r.get(3)) == "1",
            last_online: expiry_text(int(r.get(4))),
            motto: text(r.get(5)),
        })
        .collect();
    Ok(SearchPage { rows, more })
}

/// A rank the hotel has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rank {
    pub id: i64,
    pub name: String,
}

/// A currency balance (`users_currency`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Currency {
    pub kind: i64,
    pub amount: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub kind: String,
    pub started: String,
    pub ends: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub code: String,
    /// The slot the badge is worn in, 0 when it is not worn.
    pub slot: i64,
}

/// Furni of one kind (`items_base.type`: s floor, i wall, e effect, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FurniCount {
    pub kind: String,
    pub inventory: i64,
    pub placed: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub room_id: i64,
    pub room: String,
    pub entered: String,
    pub left: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ban {
    pub id: i64,
    pub kind: String,
    pub given: String,
    pub expires: String,
    pub active: bool,
    pub reason: String,
    pub by: String,
}

/// Everything shown about one player.
#[derive(Clone, Debug, Default)]
pub struct Player {
    pub id: i64,
    pub name: String,
    pub rank: i64,
    pub rank_name: String,
    pub motto: String,
    pub look: String,
    pub gender: String,
    pub mail: String,
    pub created: String,
    pub last_online: String,
    pub online: bool,
    /// The room the player is in now (from the room visit log), when online.
    pub room: Option<(i64, String)>,
    pub home_room: i64,
    pub credits: i64,
    pub currencies: Vec<Currency>,
    pub subscriptions: Vec<Subscription>,
    pub badges: Vec<Badge>,
    pub furni: Vec<FurniCount>,
    pub pets: (i64, i64),
    pub bots: (i64, i64),
    pub rooms: i64,
    pub visits: Vec<Visit>,
    pub bans: Vec<Ban>,
    /// The time the mute ends, when the player is muted now.
    pub muted_until: Option<String>,
    /// The tables that are not in this database (so those parts are not shown).
    pub missing: Vec<&'static str>,
}

/// The columns read from `users`: never the password, the auth ticket, the remember-me token, the secret key or the
/// pincode.
pub const USER_COLUMNS: [&str; 12] =
    ["id", "username", "rank", "motto", "look", "gender", "mail", "account_created", "last_online", "online", "credits", "home_room"];

pub fn user_sql(database: &str) -> String {
    let columns: Vec<String> = USER_COLUMNS.iter().map(|c| format!("`{c}`")).collect();
    format!("SELECT {} FROM {}.`users` WHERE `id` = ? LIMIT 1", columns.join(", "), quote(database))
}

/// The tables the details are read from besides `users`; the ones not in the database are left out.
pub const DETAIL_TABLES: [&str; 12] = [
    "permission_ranks",
    "permissions",
    "users_currency",
    "users_subscriptions",
    "users_badges",
    "items",
    "items_base",
    "users_pets",
    "bots",
    "rooms",
    "room_enter_log",
    "bans",
];

async fn tables(session: &SqlSession, database: &str) -> Result<HashSet<String>> {
    let rows = session.select(crate::logs_admin::tables_sql(), vec![SqlValue::Text(database.to_string())]).await?;
    Ok(rows.iter().map(|r| text(r.first()).to_lowercase()).collect())
}

/// The table the ranks are in: `permission_ranks` (Polaris) when it has rows, else the older `permissions`.
async fn rank_table(session: &SqlSession, database: &str, tables: &HashSet<String>) -> Result<Option<&'static str>> {
    if tables.contains("permission_ranks") {
        let rows = session.select(&format!("SELECT COUNT(*) FROM {}.`permission_ranks`", quote(database)), vec![]).await?;
        if rows.first().is_some_and(|r| int(r.first()) > 0) {
            return Ok(Some("permission_ranks"));
        }
    }
    Ok(tables.contains("permissions").then_some("permissions"))
}

/// The ranks of the hotel, lowest first.
pub async fn ranks(session: &SqlSession, database: &str) -> Result<Vec<Rank>> {
    let tables = tables(session, database).await?;
    let Some(table) = rank_table(session, database, &tables).await? else { return Ok(Vec::new()) };
    let rows = session.select(&format!("SELECT `id`, `rank_name` FROM {}.`{table}` ORDER BY `id`", quote(database)), vec![]).await?;
    Ok(rows.iter().map(|r| Rank { id: int(r.first()), name: text(r.get(1)) }).collect())
}

pub fn currencies_sql(database: &str) -> String {
    format!("SELECT `type`, `amount` FROM {}.`users_currency` WHERE `user_id` = ? ORDER BY `type`", quote(database))
}

pub fn subscriptions_sql(database: &str) -> String {
    format!(
        "SELECT `subscription_type`, `timestamp_start`, `duration`, `active` FROM {}.`users_subscriptions` WHERE `user_id` = ? \
         ORDER BY `active` DESC, `timestamp_start` + `duration` DESC LIMIT 10",
        quote(database)
    )
}

/// Worn badges first, in slot order.
pub fn badges_sql(database: &str) -> String {
    format!("SELECT `badge_code`, `slot_id` FROM {}.`users_badges` WHERE `user_id` = ? ORDER BY `slot_id` = 0, `slot_id`, `badge_code`", quote(database))
}

/// Furni counted by kind, in the inventory (`room_id` 0) and placed in a room.
pub fn furni_sql(database: &str, with_base: bool) -> String {
    let db = quote(database);
    if with_base {
        format!(
            "SELECT COALESCE(b.`type`, ''), SUM(i.`room_id` = 0), SUM(i.`room_id` <> 0) FROM {db}.`items` i \
             LEFT JOIN {db}.`items_base` b ON b.`id` = i.`item_id` WHERE i.`user_id` = ? GROUP BY 1 ORDER BY 1"
        )
    } else {
        format!("SELECT '', SUM(`room_id` = 0), SUM(`room_id` <> 0) FROM {db}.`items` WHERE `user_id` = ?")
    }
}

/// How many the player has, and how many of those are not placed in a room.
pub fn owned_sql(database: &str, table: &str) -> String {
    format!("SELECT COUNT(*), COALESCE(SUM(`room_id` = 0), 0) FROM {}.`{table}` WHERE `user_id` = ?", quote(database))
}

pub fn rooms_sql(database: &str) -> String {
    format!("SELECT COUNT(*) FROM {}.`rooms` WHERE `owner_id` = ?", quote(database))
}

pub fn visits_sql(database: &str, with_rooms: bool) -> String {
    let db = quote(database);
    let (room, join) = if with_rooms { ("COALESCE(r.`name`, '')", format!(" LEFT JOIN {db}.`rooms` r ON r.`id` = l.`room_id`")) } else { ("''", String::new()) };
    format!(
        "SELECT l.`room_id`, {room}, l.`timestamp`, l.`exit_timestamp` FROM {db}.`room_enter_log` l{join} \
         WHERE l.`user_id` = ? ORDER BY l.`timestamp` DESC, l.`id` DESC LIMIT {VISIT_ROWS}"
    )
}

/// The bans of the player, newest first, with the name of the staff member who gave each.
pub fn bans_sql(database: &str) -> String {
    let db = quote(database);
    format!(
        "SELECT b.`id`, b.`type`, b.`timestamp`, b.`ban_expire`, b.`ban_reason`, COALESCE(s.`username`, CONCAT('#', b.`user_staff_id`)) \
         FROM {db}.`bans` b LEFT JOIN {db}.`users` s ON s.`id` = b.`user_staff_id` WHERE b.`user_id` = ? \
         ORDER BY b.`timestamp` DESC, b.`id` DESC LIMIT {BAN_ROWS}"
    )
}

pub fn mute_sql(database: &str) -> String {
    format!("SELECT `mute_end_timestamp` FROM {}.`users_settings` WHERE `user_id` = ? LIMIT 1", quote(database))
}

/// A ban counts until its end; an end of 0 never ends.
pub fn ban_active(expire: i64, now: i64) -> bool {
    expire <= 0 || expire > now
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Reads the player with this id; None when there is none.
pub async fn load(session: &SqlSession, database: &str, id: i64) -> Result<Option<Player>> {
    let rows = session.select(&user_sql(database), vec![SqlValue::Int(id)]).await?;
    let Some(row) = rows.first() else { return Ok(None) };
    let tables = tables(session, database).await?;
    let has = |table: &str| tables.contains(table);
    let user = || vec![SqlValue::Int(id)];
    let now = now();
    let mut player = Player {
        id: int(row.first()),
        name: text(row.get(1)),
        rank: int(row.get(2)),
        motto: text(row.get(3)),
        look: text(row.get(4)),
        gender: text(row.get(5)),
        mail: text(row.get(6)),
        created: expiry_text(int(row.get(7))),
        last_online: expiry_text(int(row.get(8))),
        online: text(row.get(9)) == "1",
        credits: int(row.get(10)),
        home_room: int(row.get(11)),
        ..Player::default()
    };
    player.missing = DETAIL_TABLES.into_iter().filter(|t| !has(t) && *t != "permissions" && *t != "items_base").collect();

    if let Some(table) = rank_table(session, database, &tables).await? {
        let rows = session.select(&format!("SELECT `rank_name` FROM {}.`{table}` WHERE `id` = ? LIMIT 1", quote(database)), vec![SqlValue::Int(player.rank)]).await?;
        player.rank_name = rows.first().map(|r| text(r.first())).unwrap_or_default();
    }
    if has("users_currency") {
        let rows = session.select(&currencies_sql(database), user()).await?;
        player.currencies = rows.iter().map(|r| Currency { kind: int(r.first()), amount: int(r.get(1)) }).collect();
    }
    if has("users_subscriptions") {
        let rows = session.select(&subscriptions_sql(database), user()).await?;
        player.subscriptions = rows
            .iter()
            .map(|r| {
                let start = int(r.get(1));
                let end = start + int(r.get(2));
                Subscription { kind: text(r.first()), started: expiry_text(start), ends: expiry_text(end), active: int(r.get(3)) != 0 && end > now }
            })
            .collect();
    }
    if has("users_badges") {
        let rows = session.select(&badges_sql(database), user()).await?;
        player.badges = rows.iter().map(|r| Badge { code: text(r.first()), slot: int(r.get(1)) }).collect();
    }
    if has("items") {
        let rows = session.select(&furni_sql(database, has("items_base")), user()).await?;
        player.furni = rows
            .iter()
            .map(|r| FurniCount { kind: text(r.first()), inventory: int(r.get(1)), placed: int(r.get(2)) })
            .filter(|f| f.inventory + f.placed > 0)
            .collect();
    }
    for (table, slot) in [("users_pets", &mut player.pets), ("bots", &mut player.bots)] {
        if has(table) {
            let rows = session.select(&owned_sql(database, table), user()).await?;
            *slot = rows.first().map(|r| (int(r.first()), int(r.get(1)))).unwrap_or_default();
        }
    }
    if has("rooms") {
        let rows = session.select(&rooms_sql(database), user()).await?;
        player.rooms = rows.first().map(|r| int(r.first())).unwrap_or(0);
    }
    if has("room_enter_log") {
        let rows = session.select(&visits_sql(database, has("rooms")), user()).await?;
        player.visits = rows
            .iter()
            .map(|r| Visit { room_id: int(r.first()), room: text(r.get(1)), entered: expiry_text(int(r.get(2))), left: expiry_text(int(r.get(3))) })
            .collect();
        if player.online {
            player.room = player.visits.first().filter(|v| v.left.is_empty()).map(|v| (v.room_id, v.room.clone()));
        }
    }
    if has("bans") {
        let rows = session.select(&bans_sql(database), user()).await?;
        player.bans = rows
            .iter()
            .map(|r| {
                let expire = int(r.get(3));
                Ban {
                    id: int(r.first()),
                    kind: text(r.get(1)),
                    given: expiry_text(int(r.get(2))),
                    expires: expiry_text(expire),
                    active: ban_active(expire, now),
                    reason: text(r.get(4)),
                    by: text(r.get(5)),
                }
            })
            .collect();
    }
    if has("users_settings") {
        let rows = session.select(&mute_sql(database), user()).await?;
        let end = rows.first().map(|r| int(r.first())).unwrap_or(0);
        player.muted_until = (end > now).then(|| expiry_text(end));
    }
    Ok(Some(player))
}

/// Why an action is not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The amount is not a whole number above 0.
    Amount,
    /// The currency type is below 0.
    CurrencyType,
    /// The badge code is not 1 to [`BADGE_MAX`] letters, digits, `_` or `-`.
    Badge,
    /// The alert is empty or longer than [`ALERT_MAX`].
    Alert,
    /// No rank is chosen.
    Rank,
    /// The ban or mute is not 1 second up to its longest.
    Duration,
    /// The ban reason is empty or longer than [`BAN_REASON_MAX`].
    Reason,
}

/// What can be done to a player through RCON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    GiveCredits(i64),
    /// A `users_currency` type and an amount.
    GivePoints(i64, i64),
    GiveBadge(String),
    Alert(String),
    Disconnect,
    SetRank(i64),
    /// Seconds; also for a player who is offline.
    Mute(i64),
    Unmute,
    /// Seconds, reason and what the ban covers. Polaris' `banuser`.
    Ban(i64, String, BanKind),
    /// Lifts every active ban. Polaris' `unbanuser`.
    Unban,
}

impl Action {
    pub fn key(&self) -> &'static str {
        match self {
            Action::GiveCredits(_) => "givecredits",
            Action::GivePoints(..) => "givepoints",
            Action::GiveBadge(_) => "givebadge",
            Action::Alert(_) => "alertuser",
            Action::Disconnect => "disconnect",
            Action::SetRank(_) => "setrank",
            Action::Mute(_) | Action::Unmute => "muteuser",
            Action::Ban(..) => "banuser",
            Action::Unban => "unbanuser",
        }
    }

    /// Whether the emulator only does this to a player who is online (alert and disconnect).
    pub fn online_only(&self) -> bool {
        matches!(self, Action::Alert(_) | Action::Disconnect)
    }

    /// What is wrong with the action before it is sent, checked like the emulator does.
    pub fn problem(&self) -> Option<Problem> {
        match self {
            Action::GiveCredits(amount) | Action::GivePoints(_, amount) if *amount <= 0 => Some(Problem::Amount),
            Action::GivePoints(kind, _) if *kind < 0 => Some(Problem::CurrencyType),
            Action::GiveBadge(code) if code.is_empty() || code.len() > BADGE_MAX || !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => {
                Some(Problem::Badge)
            }
            Action::Alert(message) if message.trim().is_empty() || message.chars().count() > ALERT_MAX => Some(Problem::Alert),
            Action::SetRank(rank) if *rank <= 0 => Some(Problem::Rank),
            Action::Mute(seconds) if *seconds <= 0 || *seconds > MUTE_MAX_SECONDS => Some(Problem::Duration),
            Action::Ban(seconds, ..) if *seconds <= 0 || *seconds > BAN_MAX_SECONDS => Some(Problem::Duration),
            Action::Ban(_, reason, _) if reason.trim().is_empty() || reason.trim().chars().count() > BAN_REASON_MAX => Some(Problem::Reason),
            _ => None,
        }
    }

    /// The RCON request for the player with this id.
    pub fn request(&self, user_id: i64) -> String {
        let data = match self {
            Action::GiveCredits(amount) => json!({ "user_id": user_id, "credits": amount }),
            Action::GivePoints(kind, amount) => json!({ "user_id": user_id, "points": amount, "type": kind }),
            Action::GiveBadge(code) => json!({ "user_id": user_id, "badge": code }),
            Action::Alert(message) => json!({ "user_id": user_id, "message": message }),
            Action::Disconnect => json!({ "user_id": user_id }),
            Action::SetRank(rank) => json!({ "user_id": user_id, "rank": rank }),
            Action::Mute(seconds) => json!({ "user_id": user_id, "duration": seconds }),
            Action::Unmute => json!({ "user_id": user_id, "duration": 0 }),
            Action::Ban(seconds, reason, kind) => json!({ "user_id": user_id, "duration": seconds, "reason": reason.trim(), "type": kind.key() }),
            Action::Unban => json!({ "user_id": user_id }),
        };
        rcon::request(self.key(), data)
    }
}

/// Sends the action for the player to the emulator and returns its answer.
pub async fn act(server: &dyn Server, user_id: i64, action: &Action) -> Result<Answer> {
    rcon::send(server, &action.request(user_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_escapes_the_name_and_pages() {
        let (sql, params) = search_sql("hotel", "a_b%", false, 2);
        assert!(sql.contains("FROM `hotel`.`users` WHERE `username` LIKE ?"));
        assert!(sql.contains(&format!("LIMIT {} OFFSET ?", PAGE_ROWS + 1)));
        assert_eq!(params, vec![SqlValue::Text("a\\_b\\%%".into()), SqlValue::Int(2 * PAGE_ROWS)]);

        let (sql, params) = search_sql("hotel", " 42 ", true, 0);
        assert!(sql.contains("WHERE (`id` = ? OR `username` LIKE ?) AND `online` = '1'"));
        assert_eq!(params, vec![SqlValue::Int(42), SqlValue::Text("42%".into()), SqlValue::Int(0)]);

        let (sql, params) = search_sql("hotel", "", false, 0);
        assert!(!sql.contains("WHERE"));
        assert_eq!(params, vec![SqlValue::Int(0)]);
        assert_eq!(prefix_pattern("a\\b"), "a\\\\b%");
    }

    #[test]
    fn secrets_are_never_selected() {
        let sql = user_sql("hotel");
        for secret in ["password", "auth_ticket", "remember_token", "secret_key", "pincode", "access_token", "ip_", "machine_id"] {
            assert!(!sql.contains(secret), "{secret} in {sql}");
        }
        assert!(sql.contains("WHERE `id` = ? LIMIT 1"));
    }

    #[test]
    fn detail_queries_take_the_player_id() {
        for sql in [currencies_sql("h"), subscriptions_sql("h"), badges_sql("h"), furni_sql("h", true), furni_sql("h", false), owned_sql("h", "bots"), rooms_sql("h"), visits_sql("h", true), visits_sql("h", false), bans_sql("h"), mute_sql("h")] {
            assert_eq!(sql.matches('?').count(), 1, "{sql}");
            assert!(sql.contains("`h`."), "{sql}");
        }
        assert!(badges_sql("h").contains("ORDER BY `slot_id` = 0, `slot_id`"));
    }

    #[test]
    fn bans_without_an_end_stay() {
        assert!(ban_active(0, 100));
        assert!(ban_active(200, 100));
        assert!(!ban_active(50, 100));
    }

    #[test]
    fn requests_match_the_emulator_handlers() {
        let parse = |a: Action| serde_json::from_str::<serde_json::Value>(&a.request(7)).unwrap();
        let v = parse(Action::GiveCredits(500));
        assert_eq!((v["key"].as_str(), v["data"]["user_id"].as_i64(), v["data"]["credits"].as_i64()), (Some("givecredits"), Some(7), Some(500)));
        let v = parse(Action::GivePoints(5, 10));
        assert_eq!((v["key"].as_str(), v["data"]["points"].as_i64(), v["data"]["type"].as_i64()), (Some("givepoints"), Some(10), Some(5)));
        let v = parse(Action::GiveBadge("ADM".into()));
        assert_eq!((v["key"].as_str(), v["data"]["badge"].as_str()), (Some("givebadge"), Some("ADM")));
        let v = parse(Action::Alert("hi 'there'".into()));
        assert_eq!((v["key"].as_str(), v["data"]["message"].as_str()), (Some("alertuser"), Some("hi 'there'")));
        let v = parse(Action::Disconnect);
        assert_eq!((v["key"].as_str(), v["data"]["user_id"].as_i64()), (Some("disconnect"), Some(7)));
        let v = parse(Action::SetRank(3));
        assert_eq!((v["key"].as_str(), v["data"]["rank"].as_i64()), (Some("setrank"), Some(3)));
        let v = parse(Action::Mute(900));
        assert_eq!((v["key"].as_str(), v["data"]["duration"].as_i64()), (Some("muteuser"), Some(900)));
        let v = parse(Action::Unmute);
        assert_eq!((v["key"].as_str(), v["data"]["duration"].as_i64()), (Some("muteuser"), Some(0)));
        let v = parse(Action::Ban(86400, " spam ".into(), BanKind::Ip));
        assert_eq!(
            (v["key"].as_str(), v["data"]["duration"].as_i64(), v["data"]["reason"].as_str(), v["data"]["type"].as_str()),
            (Some("banuser"), Some(86400), Some("spam"), Some("ip"))
        );
        let v = parse(Action::Unban);
        assert_eq!((v["key"].as_str(), v["data"]["user_id"].as_i64()), (Some("unbanuser"), Some(7)));
    }

    #[test]
    fn actions_are_checked_first() {
        assert!(Action::GiveCredits(0).problem().is_some());
        assert!(Action::GivePoints(-1, 5).problem().is_some());
        assert!(Action::GivePoints(5, 5).problem().is_none());
        assert!(Action::GiveBadge("AB C".into()).problem().is_some());
        assert!(Action::GiveBadge("x".repeat(33)).problem().is_some());
        assert!(Action::GiveBadge("ACH_Login1".into()).problem().is_none());
        assert!(Action::Alert("  ".into()).problem().is_some());
        assert!(Action::SetRank(0).problem().is_some());
        assert!(Action::Disconnect.problem().is_none() && Action::Disconnect.online_only());
        assert!(!Action::GiveBadge("A".into()).online_only());
        assert!(Action::Mute(0).problem().is_some() && Action::Mute(MUTE_MAX_SECONDS + 1).problem().is_some());
        assert!(Action::Mute(60).problem().is_none() && !Action::Mute(60).online_only());
        assert_eq!(Action::Ban(60, " ".into(), BanKind::Account).problem(), Some(Problem::Reason));
        assert_eq!(Action::Ban(60, "x".repeat(201), BanKind::Account).problem(), Some(Problem::Reason));
        assert_eq!(Action::Ban(BAN_MAX_SECONDS + 1, "x".into(), BanKind::Account).problem(), Some(Problem::Duration));
        assert!(Action::Ban(BAN_MAX_SECONDS, "x".into(), BanKind::Super).problem().is_none());
    }
}
