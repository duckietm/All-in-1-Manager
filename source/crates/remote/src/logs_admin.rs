//! The hotel logs the emulator writes (commands, chat, currencies, purchases, permission and staff actions, room
//! visits, name changes, trades), read through the SQL session. Read-only: nothing here changes a row.
//!
//! Every list is newest first and paged ([`PAGE_ROWS`] rows a page). A player filter is turned into the player's id
//! first (`users.username = ?`), so the log is searched on its indexed id column, never with a LIKE on a join.
//! Words are matched with LIKE, the `%`, `_` and `\` in them escaped. All values go in as statement parameters.

use std::collections::HashSet;

use crate::catalog_admin::expiry_text;
use crate::server::Result;
use crate::sql_session::{quote, SqlSession, SqlValue};

/// Rows on one page.
pub const PAGE_ROWS: i64 = 200;

/// Lines shown before and after a room chat line.
pub const CONTEXT_LINES: i64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Log {
    #[default]
    Commands,
    RoomChat,
    PrivateChat,
    Economy,
    Purchases,
    Permissions,
    StaffActions,
    RoomVisits,
    NameChanges,
    Trades,
}

pub const LOGS: [Log; 10] =
    [Log::Commands, Log::RoomChat, Log::PrivateChat, Log::Economy, Log::Purchases, Log::Permissions, Log::StaffActions, Log::RoomVisits, Log::NameChanges, Log::Trades];

/// How the log stores its time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Time {
    /// A unix time in an INT column.
    Unix,
    /// A TIMESTAMP / DATETIME column.
    Stamp,
}

/// One shown column: the SQL that reads it, and whether it is a unix time to show as a local date.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub sql: &'static str,
    pub time: bool,
}

const fn col(sql: &'static str) -> Column {
    Column { sql, time: false }
}

const fn time(sql: &'static str) -> Column {
    Column { sql, time: true }
}

/// How one log is read. `{db}` in `joins` is the quoted database.
#[derive(Debug)]
pub struct LogSpec {
    pub table: &'static str,
    /// The row's id (`l.id`), or None for a table without one (read by time).
    pub id: Option<&'static str>,
    /// The room a row is in, for the room chat context.
    pub room: Option<&'static str>,
    pub time_column: &'static str,
    pub time: Time,
    pub joins: &'static str,
    pub columns: &'static [Column],
    /// Conditions with one `?` each, joined with OR, all given the player's id.
    pub player: &'static [&'static str],
    /// The room id condition, with one `?`.
    pub room_filter: Option<&'static str>,
    /// Text columns the words are looked for in (any of them).
    pub text: &'static [&'static str],
}

const USER: &str = " LEFT JOIN {db}.`users` u ON u.`id` = l.`user_id`";

const COMMANDS: LogSpec = LogSpec {
    table: "commandlogs",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: USER,
    columns: &[col("l.`id`"), time("l.`timestamp`"), col("COALESCE(u.`username`, CONCAT('#', l.`user_id`))"), col("l.`command`"), col("l.`params`"), col("l.`succes`")],
    player: &["l.`user_id` = ?"],
    room_filter: None,
    text: &["l.`command`", "l.`params`"],
};

const ROOM_CHAT: LogSpec = LogSpec {
    table: "chatlogs_room",
    id: Some("l.`id`"),
    room: Some("l.`room_id`"),
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: " LEFT JOIN {db}.`users` u ON u.`id` = l.`user_from_id` LEFT JOIN {db}.`users` t ON t.`id` = l.`user_to_id` LEFT JOIN {db}.`rooms` r ON r.`id` = l.`room_id`",
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        col("COALESCE(u.`username`, CONCAT('#', l.`user_from_id`))"),
        col("IF(l.`user_to_id` = 0, '', COALESCE(t.`username`, CONCAT('#', l.`user_to_id`)))"),
        col("l.`room_id`"),
        col("COALESCE(r.`name`, '')"),
        col("l.`message`"),
    ],
    player: &["l.`user_from_id` = ?"],
    room_filter: Some("l.`room_id` = ?"),
    text: &["l.`message`"],
};

const PRIVATE_CHAT: LogSpec = LogSpec {
    table: "chatlogs_private",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: " LEFT JOIN {db}.`users` u ON u.`id` = l.`user_from_id` LEFT JOIN {db}.`users` t ON t.`id` = l.`user_to_id`",
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        col("COALESCE(u.`username`, CONCAT('#', l.`user_from_id`))"),
        col("COALESCE(t.`username`, CONCAT('#', l.`user_to_id`))"),
        col("l.`message`"),
    ],
    player: &["l.`user_from_id` = ?", "l.`user_to_id` = ?"],
    room_filter: None,
    text: &["l.`message`"],
};

const ECONOMY: LogSpec = LogSpec {
    table: "logs_economy",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`created_at`",
    time: Time::Stamp,
    joins: USER,
    columns: &[
        col("l.`id`"),
        time("UNIX_TIMESTAMP(l.`created_at`)"),
        col("COALESCE(u.`username`, CONCAT('#', l.`user_id`))"),
        col("l.`operation`"),
        col("l.`currency_type`"),
        col("l.`amount`"),
        col("l.`balance_before`"),
        col("l.`balance_after`"),
        col("COALESCE(l.`item_id`, '')"),
        col("l.`context`"),
    ],
    player: &["l.`user_id` = ?"],
    room_filter: None,
    text: &["l.`operation`", "l.`context`"],
};

const PURCHASES: LogSpec = LogSpec {
    table: "logs_shop_purchases",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: USER,
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        col("COALESCE(u.`username`, CONCAT('#', l.`user_id`))"),
        col("COALESCE(l.`catalog_name`, '')"),
        col("COALESCE(l.`catalog_item_id`, '')"),
        col("COALESCE(l.`amount`, '')"),
        col("COALESCE(l.`cost_credits`, '')"),
        col("COALESCE(l.`cost_points`, '')"),
        col("COALESCE(l.`points_type`, '')"),
    ],
    player: &["l.`user_id` = ?"],
    room_filter: None,
    text: &["l.`catalog_name`"],
};

const PERMISSIONS: LogSpec = LogSpec {
    table: "permission_audit",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: " LEFT JOIN {db}.`users` t ON l.`target_type` = 'user' AND t.`id` = l.`target_id`",
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        col("IF(l.`actor_name` = '', CONCAT('#', l.`actor_id`), l.`actor_name`)"),
        col("l.`action`"),
        col("CONCAT(l.`target_type`, ' ', COALESCE(t.`username`, CONCAT('#', l.`target_id`)))"),
        col("l.`subject`"),
        col("l.`old_value`"),
        col("l.`new_value`"),
        col("l.`via`"),
    ],
    player: &["l.`actor_id` = ?", "(l.`target_type` = 'user' AND l.`target_id` = ?)"],
    room_filter: None,
    text: &["l.`action`", "l.`subject`"],
};

const STAFF_ACTIONS: LogSpec = LogSpec {
    table: "housekeeping_log",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: "",
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        col("IF(l.`actor_name` = '', CONCAT('#', l.`actor_id`), l.`actor_name`)"),
        col("l.`action`"),
        col("CONCAT(l.`target_type`, ' ', IF(l.`target_label` = '', CONCAT('#', l.`target_id`), l.`target_label`))"),
        col("l.`detail`"),
        col("IF(l.`success` = 0, 'no', 'yes')"),
    ],
    player: &["l.`actor_id` = ?", "(l.`target_type` = 'user' AND l.`target_id` = ?)"],
    room_filter: None,
    text: &["l.`action`", "l.`detail`"],
};

const ROOM_VISITS: LogSpec = LogSpec {
    table: "room_enter_log",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: " LEFT JOIN {db}.`users` u ON u.`id` = l.`user_id` LEFT JOIN {db}.`rooms` r ON r.`id` = l.`room_id`",
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        time("l.`exit_timestamp`"),
        col("COALESCE(u.`username`, CONCAT('#', l.`user_id`))"),
        col("l.`room_id`"),
        col("COALESCE(r.`name`, '')"),
    ],
    player: &["l.`user_id` = ?"],
    room_filter: Some("l.`room_id` = ?"),
    text: &[],
};

const NAME_CHANGES: LogSpec = LogSpec {
    table: "namechange_log",
    id: None,
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: USER,
    columns: &[time("l.`timestamp`"), col("COALESCE(u.`username`, CONCAT('#', l.`user_id`))"), col("l.`old_name`"), col("l.`new_name`")],
    player: &["l.`user_id` = ?"],
    room_filter: None,
    text: &["l.`old_name`", "l.`new_name`"],
};

const TRADES: LogSpec = LogSpec {
    table: "room_trade_log",
    id: Some("l.`id`"),
    room: None,
    time_column: "l.`timestamp`",
    time: Time::Unix,
    joins: " LEFT JOIN {db}.`users` u ON u.`id` = l.`user_one_id` LEFT JOIN {db}.`users` t ON t.`id` = l.`user_two_id`",
    columns: &[
        col("l.`id`"),
        time("l.`timestamp`"),
        col("COALESCE(u.`username`, CONCAT('#', l.`user_one_id`))"),
        col("l.`user_one_item_count`"),
        col("COALESCE(t.`username`, CONCAT('#', l.`user_two_id`))"),
        col("l.`user_two_item_count`"),
    ],
    player: &["l.`user_one_id` = ?", "l.`user_two_id` = ?"],
    room_filter: None,
    text: &[],
};

impl Log {
    pub fn spec(self) -> &'static LogSpec {
        match self {
            Log::Commands => &COMMANDS,
            Log::RoomChat => &ROOM_CHAT,
            Log::PrivateChat => &PRIVATE_CHAT,
            Log::Economy => &ECONOMY,
            Log::Purchases => &PURCHASES,
            Log::Permissions => &PERMISSIONS,
            Log::StaffActions => &STAFF_ACTIONS,
            Log::RoomVisits => &ROOM_VISITS,
            Log::NameChanges => &NAME_CHANGES,
            Log::Trades => &TRADES,
        }
    }

    pub fn index(self) -> usize {
        LOGS.iter().position(|l| *l == self).unwrap_or(0)
    }
}

/// What a list is narrowed to. Times are unix times; `until` is exclusive.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub player: Option<i64>,
    pub room: Option<i64>,
    pub words: String,
    pub from: Option<i64>,
    pub until: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogRow {
    pub id: i64,
    pub room: i64,
    pub cells: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogPage {
    pub rows: Vec<LogRow>,
    /// Another page follows.
    pub more: bool,
}

/// `%text%` for LIKE, with `\`, `%` and `_` in the text matched as themselves.
pub fn contains_pattern(text: &str) -> String {
    format!("%{}%", text.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

fn time_param(spec: &LogSpec) -> &'static str {
    match spec.time {
        Time::Unix => "?",
        Time::Stamp => "FROM_UNIXTIME(?)",
    }
}

/// The WHERE clause (or nothing) of a filter, with its parameters.
fn conditions(spec: &LogSpec, filter: &Filter) -> (String, Vec<SqlValue>) {
    let mut parts = Vec::new();
    let mut params = Vec::new();
    if let Some(player) = filter.player {
        if !spec.player.is_empty() {
            parts.push(format!("({})", spec.player.join(" OR ")));
            params.extend(spec.player.iter().map(|_| SqlValue::Int(player)));
        }
    }
    if let (Some(room), Some(condition)) = (filter.room, spec.room_filter) {
        parts.push(condition.to_string());
        params.push(SqlValue::Int(room));
    }
    if !filter.words.trim().is_empty() && !spec.text.is_empty() {
        let pattern = contains_pattern(&filter.words);
        parts.push(format!("({})", spec.text.iter().map(|c| format!("{c} LIKE ?")).collect::<Vec<_>>().join(" OR ")));
        params.extend(spec.text.iter().map(|_| SqlValue::Text(pattern.clone())));
    }
    if let Some(from) = filter.from {
        parts.push(format!("{} >= {}", spec.time_column, time_param(spec)));
        params.push(SqlValue::Int(from));
    }
    if let Some(until) = filter.until {
        parts.push(format!("{} < {}", spec.time_column, time_param(spec)));
        params.push(SqlValue::Int(until));
    }
    let clause = if parts.is_empty() { String::new() } else { format!(" WHERE {}", parts.join(" AND ")) };
    (clause, params)
}

fn select_list(spec: &LogSpec) -> String {
    let id = spec.id.unwrap_or("0");
    let room = spec.room.unwrap_or("0");
    let columns = spec.columns.iter().map(|c| c.sql).collect::<Vec<_>>().join(", ");
    format!("{id}, {room}, {columns}")
}

fn from(spec: &LogSpec, database: &str) -> String {
    format!("{}.{} l{}", quote(database), quote(spec.table), spec.joins.replace("{db}", &quote(database)))
}

/// Newest first: by id, or by time for a table without one.
fn order(spec: &LogSpec) -> String {
    match spec.id {
        Some(id) => format!("{id} DESC"),
        None => format!("{} DESC", spec.time_column),
    }
}

/// One page of a log, with one row more than a page to tell whether another follows. The last parameter is the
/// offset.
pub fn page_sql(spec: &LogSpec, database: &str, filter: &Filter, page: i64) -> (String, Vec<SqlValue>) {
    let (clause, mut params) = conditions(spec, filter);
    let sql = format!("SELECT {} FROM {}{clause} ORDER BY {} LIMIT {} OFFSET ?", select_list(spec), from(spec, database), order(spec), PAGE_ROWS + 1);
    params.push(SqlValue::Int(page.max(0) * PAGE_ROWS));
    (sql, params)
}

/// The room chat lines around one: up to [`CONTEXT_LINES`] before it and after it in the same room, oldest first.
/// Parameters: room, id, room, id.
pub fn context_sql(database: &str) -> String {
    let spec = &ROOM_CHAT;
    let select = format!("SELECT {} FROM {}", select_list(spec), from(spec, database));
    format!(
        "({select} WHERE l.`room_id` = ? AND l.`id` < ? ORDER BY l.`id` DESC LIMIT {CONTEXT_LINES}) UNION ALL ({select} WHERE l.`room_id` = ? AND l.`id` >= ? ORDER BY l.`id` LIMIT {}) ORDER BY 1",
        CONTEXT_LINES + 1
    )
}

pub fn player_id_sql(database: &str) -> String {
    format!("SELECT `id` FROM {}.`users` WHERE `username` = ? LIMIT 1", quote(database))
}

pub fn tables_sql() -> &'static str {
    "SELECT `table_name` FROM information_schema.`tables` WHERE `table_schema` = ?"
}

fn text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Int(v)) => v.to_string(),
        Some(SqlValue::UInt(v)) => v.to_string(),
        Some(SqlValue::Float(v)) => v.to_string(),
        Some(SqlValue::Double(v)) => v.to_string(),
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) | Some(SqlValue::Date(t)) | Some(SqlValue::DateTime(t)) | Some(SqlValue::Time(t)) => t.clone(),
        Some(SqlValue::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

/// A row read with [`page_sql`] or [`context_sql`]: id, room, then the columns, times shown in local time.
pub fn row_from(spec: &LogSpec, row: &[SqlValue]) -> LogRow {
    let id = text(row.first()).parse().unwrap_or(0);
    let room = text(row.get(1)).parse().unwrap_or(0);
    let cells = spec
        .columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let value = text(row.get(i + 2));
            if column.time {
                expiry_text(value.parse().unwrap_or(0))
            } else {
                value
            }
        })
        .collect();
    LogRow { id, room, cells }
}

/// The page read, cut to [`PAGE_ROWS`] rows.
pub fn page_from(spec: &LogSpec, rows: &[Vec<SqlValue>]) -> LogPage {
    let more = rows.len() as i64 > PAGE_ROWS;
    LogPage { rows: rows.iter().take(PAGE_ROWS as usize).map(|r| row_from(spec, r)).collect(), more }
}

/// The first unix time of a date (`2026-11-07`, or with a time).
pub fn from_time(text: &str) -> std::result::Result<Option<i64>, ()> {
    match crate::catalog_admin::parse_expiry(text) {
        Some(-1) => Ok(None),
        Some(time) => Ok(Some(time)),
        None => Err(()),
    }
}

/// The end (exclusive) of a date range: the day after a date, or the minute after a date with a time.
pub fn until_time(text: &str) -> std::result::Result<Option<i64>, ()> {
    let has_time = text.contains(':');
    from_time(text).map(|time| time.map(|t| t + if has_time { 60 } else { 86_400 }))
}

/// The log tables of the hotel database that exist.
pub async fn existing_logs(session: &SqlSession, database: &str) -> Result<HashSet<Log>> {
    let rows = session.select(tables_sql(), vec![SqlValue::Text(database.to_string())]).await?;
    let names: HashSet<String> = rows.iter().map(|r| text(r.first()).to_lowercase()).collect();
    Ok(LOGS.into_iter().filter(|l| names.contains(l.spec().table)).collect())
}

/// The id of the player with this name, or None.
pub async fn player_id(session: &SqlSession, database: &str, name: &str) -> Result<Option<i64>> {
    let rows = session.select(&player_id_sql(database), vec![SqlValue::Text(name.trim().to_string())]).await?;
    Ok(rows.first().and_then(|r| text(r.first()).parse().ok()))
}

pub async fn load_page(session: &SqlSession, database: &str, log: Log, filter: &Filter, page: i64) -> Result<LogPage> {
    let spec = log.spec();
    let (sql, params) = page_sql(spec, database, filter, page);
    let rows = session.select(&sql, params).await?;
    Ok(page_from(spec, &rows))
}

pub async fn load_context(session: &SqlSession, database: &str, line: &LogRow) -> Result<Vec<LogRow>> {
    let params = vec![SqlValue::Int(line.room), SqlValue::Int(line.id), SqlValue::Int(line.room), SqlValue::Int(line.id)];
    let rows = session.select(&context_sql(database), params).await?;
    Ok(rows.iter().map(|r| row_from(&ROOM_CHAT, r)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_escaped() {
        assert_eq!(contains_pattern(" 50%_off\\ "), "%50\\%\\_off\\\\%");
    }

    #[test]
    fn a_page_without_filters() {
        let (sql, params) = page_sql(Log::Commands.spec(), "hotel", &Filter::default(), 2);
        assert!(sql.starts_with("SELECT l.`id`, 0, l.`id`, l.`timestamp`"));
        assert!(sql.contains("FROM `hotel`.`commandlogs` l LEFT JOIN `hotel`.`users` u ON u.`id` = l.`user_id`"));
        assert!(!sql.contains("WHERE"));
        assert!(sql.ends_with("ORDER BY l.`id` DESC LIMIT 201 OFFSET ?"));
        assert_eq!(params, vec![SqlValue::Int(400)]);
    }

    #[test]
    fn filters_become_parameters() {
        let filter = Filter { player: Some(7), room: Some(12), words: "hi".into(), from: Some(100), until: Some(200) };
        let (sql, params) = page_sql(Log::RoomChat.spec(), "hotel", &filter, 0);
        assert!(sql.contains(" WHERE (l.`user_from_id` = ?) AND l.`room_id` = ? AND (l.`message` LIKE ?) AND l.`timestamp` >= ? AND l.`timestamp` < ? ORDER BY"));
        assert_eq!(params, vec![SqlValue::Int(7), SqlValue::Int(12), SqlValue::Text("%hi%".into()), SqlValue::Int(100), SqlValue::Int(200), SqlValue::Int(0)]);
        assert_eq!(sql.matches('?').count(), params.len());
    }

    #[test]
    fn a_player_on_either_side() {
        let filter = Filter { player: Some(3), ..Filter::default() };
        let (sql, params) = page_sql(Log::PrivateChat.spec(), "hotel", &filter, 0);
        assert!(sql.contains("WHERE (l.`user_from_id` = ? OR l.`user_to_id` = ?)"));
        assert_eq!(params.len(), 3);
        let (sql, _) = page_sql(Log::Permissions.spec(), "hotel", &filter, 0);
        assert!(sql.contains("(l.`actor_id` = ? OR (l.`target_type` = 'user' AND l.`target_id` = ?))"));
    }

    #[test]
    fn filters_a_log_lacks_are_left_out() {
        let filter = Filter { room: Some(5), words: "x".into(), ..Filter::default() };
        let (sql, params) = page_sql(Log::Trades.spec(), "hotel", &filter, 0);
        assert!(!sql.contains("WHERE"));
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn stamps_are_compared_as_times() {
        let filter = Filter { from: Some(1), until: Some(2), ..Filter::default() };
        let (sql, _) = page_sql(Log::Economy.spec(), "hotel", &filter, 0);
        assert!(sql.contains("l.`created_at` >= FROM_UNIXTIME(?) AND l.`created_at` < FROM_UNIXTIME(?)"));
        assert!(sql.contains("UNIX_TIMESTAMP(l.`created_at`)"));
    }

    #[test]
    fn a_table_without_id_is_read_by_time() {
        let (sql, _) = page_sql(Log::NameChanges.spec(), "hotel", &Filter::default(), 0);
        assert!(sql.starts_with("SELECT 0, 0, l.`timestamp`"));
        assert!(sql.contains("ORDER BY l.`timestamp` DESC"));
    }

    #[test]
    fn the_context_reads_both_sides() {
        let sql = context_sql("hotel");
        assert_eq!(sql.matches('?').count(), 4);
        assert!(sql.contains("l.`id` < ? ORDER BY l.`id` DESC LIMIT 10"));
        assert!(sql.contains("l.`id` >= ? ORDER BY l.`id` LIMIT 11"));
    }

    #[test]
    fn rows_and_pages() {
        let spec = Log::RoomVisits.spec();
        let row = vec![SqlValue::Int(9), SqlValue::Int(0), SqlValue::Int(9), SqlValue::Int(0), SqlValue::Int(0), SqlValue::Text("ann".into()), SqlValue::Int(4), SqlValue::Null];
        let read = row_from(spec, &row);
        assert_eq!(read.id, 9);
        assert_eq!(read.cells, vec!["9", "", "", "ann", "4", ""]);
        let rows = vec![row; PAGE_ROWS as usize + 1];
        let page = page_from(spec, &rows);
        assert!(page.more);
        assert_eq!(page.rows.len(), PAGE_ROWS as usize);
        assert!(!page_from(spec, &rows[..3]).more);
    }

    #[test]
    fn date_ranges() {
        assert_eq!(from_time(""), Ok(None));
        assert!(from_time("tomorrow").is_err());
        let day = from_time("2026-11-07").unwrap().unwrap();
        assert_eq!(until_time("2026-11-07"), Ok(Some(day + 86_400)));
        assert_eq!(until_time("2026-11-07 10:00"), Ok(Some(from_time("2026-11-07 10:00").unwrap().unwrap() + 60)));
    }

    #[test]
    fn every_log_has_a_table() {
        let tables: HashSet<&str> = LOGS.iter().map(|l| l.spec().table).collect();
        assert_eq!(tables.len(), LOGS.len());
        for (i, log) in LOGS.iter().enumerate() {
            assert_eq!(log.index(), i);
            assert!(!log.spec().columns.is_empty());
        }
    }
}
