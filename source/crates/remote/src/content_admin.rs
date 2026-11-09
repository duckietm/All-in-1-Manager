//! The content manager's data: plain hotel tables (achievements, quests, daily tasks, polls, the currency settings)
//! edited row by row through the SQL session's SSH tunnel. A table is described once by a [`TableSpec`]: its columns,
//! their kind and size; reading, adding, saving and deleting are built from it, with every value sent as a `?`
//! parameter. The rules a table needs beyond the column kinds are here too ([`achievement_problem`],
//! [`poll_options_problem`], ...).
//!
//! Most of these tables are read by the emulator when it starts; the page says how each is reloaded (a hotel command
//! like `:update_achievements`, or a restart).

use crate::server::Result;
use crate::sql_session::{quote, SqlSession, SqlValue};

/// What a column holds, and so how it is edited and checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// A whole number between the bounds.
    Int { min: i64, max: i64 },
    /// Text of at most this many characters.
    Text { max: usize },
    /// A whole number between the bounds, or nothing (NULL) when left empty.
    OptInt { min: i64, max: i64 },
    /// 0 or 1 (a TINYINT).
    Flag,
    /// One of the values (an ENUM or a fixed list).
    Choice(&'static [&'static str]),
}

/// One editable column. `label` is the English text the window translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub column: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    /// The value of a new row.
    pub default: &'static str,
}

/// A table edited row by row: its id column (or the columns that find a row), the editable columns, the order of the
/// list and the column the rows are filtered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSpec {
    pub table: &'static str,
    /// The AUTO_INCREMENT id column; unused when `key` is given.
    pub id: &'static str,
    /// For a table without such an id (navigator_publics, pet_breeds, guilds_elements, ...): the editable columns
    /// whose stored values find the row. Empty for a table with an id.
    pub key: &'static [&'static str],
    pub fields: &'static [Field],
    pub order_by: &'static str,
    /// A column the rows are filtered on (the poll of a question), or None.
    pub parent: Option<&'static str>,
}

/// One row: its id (0 for a row not added yet; for a table with a `key`, its place in the list) and the values of
/// the spec's fields, as text. `key` holds the key values the row is stored under (empty while it is not stored).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub id: i64,
    pub values: Vec<String>,
    pub key: Vec<String>,
}

impl Row {
    /// The value of a column, or "".
    pub fn get<'a>(&'a self, spec: &TableSpec, column: &str) -> &'a str {
        spec.fields.iter().position(|f| f.column == column).and_then(|i| self.values.get(i)).map_or("", String::as_str)
    }
}

/// True when the table finds rows by `key` columns instead of an id.
pub fn is_keyed(spec: &TableSpec) -> bool {
    !spec.key.is_empty()
}

/// True for a row not in the table yet.
pub fn is_new(spec: &TableSpec, row: &Row) -> bool {
    if is_keyed(spec) {
        row.key.is_empty()
    } else {
        row.id <= 0
    }
}

/// The values of the key columns as they are in the row now.
pub fn key_values(spec: &TableSpec, row: &Row) -> Vec<String> {
    spec.key.iter().map(|c| row.get(spec, c).trim().to_string()).collect()
}

/// True when both are the same stored row (by id, or by the key it is stored under).
pub fn same_row(spec: &TableSpec, a: &Row, b: &Row) -> bool {
    !is_new(spec, a) && if is_keyed(spec) { a.key == b.key } else { a.id == b.id }
}

/// The number for a new row: one above the highest of `taken`, and at least `floor`.
pub fn next_number(taken: impl IntoIterator<Item = i64>, floor: i64) -> i64 {
    taken.into_iter().max().map_or(floor, |top| (top + 1).max(floor))
}

/// A new row with the default values (and the parent's id in the parent column).
pub fn new_row(spec: &TableSpec, parent: Option<i64>) -> Row {
    let values = spec
        .fields
        .iter()
        .map(|f| match (spec.parent, parent) {
            (Some(column), Some(id)) if column == f.column => id.to_string(),
            _ => f.default.to_string(),
        })
        .collect();
    Row { id: 0, values, key: Vec::new() }
}

fn columns(spec: &TableSpec) -> String {
    spec.fields.iter().map(|f| quote(f.column)).collect::<Vec<_>>().join(", ")
}

/// `SELECT id, fields... FROM db.table [WHERE parent = ?] ORDER BY ...` (`0` for the id of a keyed table).
pub fn select_sql(spec: &TableSpec, database: &str) -> String {
    let filter = spec.parent.map(|p| format!(" WHERE {} = ?", quote(p))).unwrap_or_default();
    let id = if is_keyed(spec) { "0".to_string() } else { quote(spec.id) };
    format!("SELECT {id}, {} FROM {}.{}{} ORDER BY {}", columns(spec), quote(database), quote(spec.table), filter, spec.order_by)
}

/// `WHERE id = ?`, or `WHERE k1 = ? AND k2 = ? LIMIT 1` for a keyed table (one row, also when the table holds the
/// same values twice).
fn row_filter(spec: &TableSpec) -> String {
    if is_keyed(spec) {
        let keys = spec.key.iter().map(|c| format!("{} = ?", quote(c))).collect::<Vec<_>>().join(" AND ");
        format!("WHERE {keys} LIMIT 1")
    } else {
        format!("WHERE {} = ?", quote(spec.id))
    }
}

/// The parameters of [`row_filter`]: the id, or the key values the row is stored under.
fn filter_params(spec: &TableSpec, row: &Row) -> Vec<SqlValue> {
    if is_keyed(spec) {
        spec.key
            .iter()
            .zip(&row.key)
            .map(|(column, value)| spec.fields.iter().find(|f| f.column == *column).map_or(SqlValue::Text(value.clone()), |f| param(f, value)))
            .collect()
    } else {
        vec![SqlValue::Int(row.id)]
    }
}

pub fn insert_sql(spec: &TableSpec, database: &str) -> String {
    let marks = vec!["?"; spec.fields.len()].join(", ");
    format!("INSERT INTO {}.{} ({}) VALUES ({marks})", quote(database), quote(spec.table), columns(spec))
}

pub fn update_sql(spec: &TableSpec, database: &str) -> String {
    let sets = spec.fields.iter().map(|f| format!("{} = ?", quote(f.column))).collect::<Vec<_>>().join(", ");
    format!("UPDATE {}.{} SET {sets} {}", quote(database), quote(spec.table), row_filter(spec))
}

pub fn delete_sql(spec: &TableSpec, database: &str) -> String {
    format!("DELETE FROM {}.{} {}", quote(database), quote(spec.table), row_filter(spec))
}

/// The value as a statement parameter: numbers as numbers (an empty optional one as NULL), the rest as text. An ENUM
/// column is a Choice and so sent as text: a number would be taken as the member's index.
pub fn param(field: &Field, value: &str) -> SqlValue {
    match field.kind {
        FieldKind::OptInt { .. } if value.trim().is_empty() => SqlValue::Null,
        FieldKind::Int { .. } | FieldKind::OptInt { .. } | FieldKind::Flag => SqlValue::Int(value.trim().parse().unwrap_or(0)),
        FieldKind::Text { .. } | FieldKind::Choice(_) => SqlValue::Text(value.trim().to_string()),
    }
}

fn params(spec: &TableSpec, row: &Row) -> Vec<SqlValue> {
    spec.fields.iter().zip(&row.values).map(|(f, v)| param(f, v)).collect()
}

fn cell(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Int(v)) => v.to_string(),
        Some(SqlValue::UInt(v)) => v.to_string(),
        Some(SqlValue::Float(v)) => v.to_string(),
        Some(SqlValue::Double(v)) => v.to_string(),
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.clone(),
        _ => String::new(),
    }
}

pub fn row_from(row: &[SqlValue]) -> Row {
    Row { id: cell(row.first()).parse().unwrap_or(0), values: row.iter().skip(1).map(|v| cell(Some(v))).collect(), key: Vec::new() }
}

/// Why the value cannot go into the column, or None.
pub fn field_problem(field: &Field, value: &str) -> Option<String> {
    let value = value.trim();
    match field.kind {
        FieldKind::OptInt { .. } if value.is_empty() => None,
        FieldKind::Int { min, max } | FieldKind::OptInt { min, max } => match value.parse::<i64>() {
            Ok(n) if (min..=max).contains(&n) => None,
            Ok(_) => Some(format!("{}: from {min} to {max}.", field.label)),
            Err(_) => Some(format!("{}: a whole number.", field.label)),
        },
        FieldKind::Text { max } => (value.chars().count() > max).then(|| format!("{}: at most {max} characters.", field.label)),
        FieldKind::Flag => (!matches!(value, "0" | "1")).then(|| format!("{}: 0 or 1.", field.label)),
        FieldKind::Choice(options) => (!options.contains(&value)).then(|| format!("{}: one of {}.", field.label, options.join(", "))),
    }
}

/// The first problem of the row's values, or None.
pub fn row_problem(spec: &TableSpec, row: &Row) -> Option<String> {
    if row.values.len() != spec.fields.len() {
        return Some("The row does not fit the table.".into());
    }
    spec.fields.iter().zip(&row.values).find_map(|(f, v)| field_problem(f, v))
}

pub async fn load_rows(session: &SqlSession, database: &str, spec: &TableSpec, parent: Option<i64>) -> Result<Vec<Row>> {
    let params = parent.map(|id| vec![SqlValue::Int(id)]).unwrap_or_default();
    let rows = session.select(&select_sql(spec, database), params).await?;
    Ok(rows.iter().enumerate().map(|(i, r)| stored(spec, filled(spec, row_from(r)), i)).collect())
}

/// A keyed table's row read from the table: numbered by its place, with the key it is stored under.
fn stored(spec: &TableSpec, mut row: Row, place: usize) -> Row {
    if is_keyed(spec) {
        row.id = place as i64 + 1;
        row.key = key_values(spec, &row);
    }
    row
}

/// A NULL number (achievements.points may be NULL) reads as the column's default, so the row can be saved.
fn filled(spec: &TableSpec, mut row: Row) -> Row {
    for (field, value) in spec.fields.iter().zip(row.values.iter_mut()) {
        if value.is_empty() && matches!(field.kind, FieldKind::Int { .. } | FieldKind::Flag) {
            *value = field.default.to_string();
        }
    }
    row
}

/// Saves the row; a new one is added. Returns it as stored (with its new id, or the key it is stored under now).
pub async fn save_row(session: &SqlSession, database: &str, spec: &TableSpec, row: &Row) -> Result<Row> {
    let mut saved = row.clone();
    if is_new(spec, row) {
        let (_, id) = session.execute(&[(insert_sql(spec, database), params(spec, row))]).await?;
        if !is_keyed(spec) {
            saved.id = id as i64;
        }
    } else {
        let mut values = params(spec, row);
        values.extend(filter_params(spec, row));
        session.execute(&[(update_sql(spec, database), values)]).await?;
    }
    if is_keyed(spec) {
        saved.key = key_values(spec, row);
    }
    Ok(saved)
}

/// The number the rows of other tables use for this row: its id, or a keyed table's first key value.
pub fn link_value(spec: &TableSpec, row: &Row) -> i64 {
    if is_keyed(spec) {
        row.key.first().and_then(|v| v.parse().ok()).unwrap_or(0)
    } else {
        row.id
    }
}

/// Deletes the row, and the rows of `children` that belong to it (a poll's questions and answers), in one
/// transaction: `(table, column)` pairs, matched on [`link_value`].
pub async fn delete_row(session: &SqlSession, database: &str, spec: &TableSpec, row: &Row, children: &[(&str, &str)]) -> Result<u64> {
    let link = link_value(spec, row);
    let mut statements: Vec<(String, Vec<SqlValue>)> = children
        .iter()
        .map(|(table, column)| (format!("DELETE FROM {}.{} WHERE {} = ?", quote(database), quote(table), quote(column)), vec![SqlValue::Int(link)]))
        .collect();
    statements.push((delete_sql(spec, database), filter_params(spec, row)));
    Ok(session.execute(&statements).await?.0)
}

/// `SELECT COUNT(*) FROM db.table WHERE column = ?`: how many rows use a value (the rooms of a room category).
pub fn count_sql(database: &str, table: &str, column: &str) -> String {
    format!("SELECT COUNT(*) FROM {}.{} WHERE {} = ?", quote(database), quote(table), quote(column))
}

pub async fn count_rows(session: &SqlSession, database: &str, table: &str, column: &str, value: i64) -> Result<i64> {
    let rows = session.select(&count_sql(database, table, column), vec![SqlValue::Int(value)]).await?;
    Ok(cell(rows.first().and_then(|r| r.first())).parse().unwrap_or(0))
}

// ---- Rules of the tables --------------------------------------------------------------------------------------

/// The achievement categories Polaris knows (the widened `achievements.category` enum).
pub const ACHIEVEMENT_CATEGORIES: &[&str] =
    &["identity", "explore", "music", "social", "games", "room_builder", "pets", "tools", "events", "other", "test", "invisible", "misc", "archive", "wired_games"];

/// The goals a quest or daily task can count (QuestGoalType: the official action names).
pub const GOAL_TYPES: &[&str] = &[
    "chat_with_someone",
    "enter_other_users_room",
    "place_item",
    "give_respect",
    "complete_quest",
    "claim_daily_task",
    "wired",
    "buy_from_catalogue",
    "change_motto",
    "change_figure",
    "friend_furni_locked",
    "create_room",
    "dance",
    "pet_eat",
    "follow_friend",
    "swim",
    "find_hand_item",
    "pet_level",
    "request_friend",
    "move_item",
    "rotate_item",
    "pet_respect",
    "place_builders_club_furni",
    "publish_picture",
    "replenish_respect",
    "send_messenger_invite",
    "send_messenger_message",
    "set_relationship_status",
    "switch_item_state",
    "use_habbicon",
    "teleport",
    "wave",
    "wear_badge",
];

/// The same goals by their enum names, which the emulator accepts too.
pub const GOAL_ENUM_NAMES: &[&str] = &[
    "TALK_IN_ROOM", "VISIT_ROOMS", "PLACE_FURNI", "GIVE_RESPECT", "COMPLETE_QUEST", "CLAIM_DAILY_TASK", "WIRED",
    "BUY_CATALOG_FURNI", "CHANGE_MOTTO", "CHANGE_FIGURE", "CLOSE_LOVE_LOCK", "CREATE_ROOM", "DANCE", "FEED_PET",
    "FOLLOW_FRIEND", "SWIM", "GRAB_HAND_ITEM", "LEVEL_PET", "REQUEST_FRIEND", "MOVE_FURNI", "ROTATE_FURNI", "RESPECT_PET",
    "PLACE_BUILDERS_CLUB_FURNI", "PUBLISH_PICTURE", "REPLENISH_RESPECT", "SEND_MESSENGER_INVITE", "SEND_MESSENGER_MESSAGE",
    "SET_RELATIONSHIP_STATUS", "USE_FURNI", "USE_HABBICON", "USE_TELEPORT", "WAVE", "WEAR_BADGE",
];

/// What a daily task can give (QuestRewards).
pub const DAILY_REWARD_TYPES: &[&str] = &["credits", "duckets", "diamonds", "badge", "furni"];

/// An achievement's name: the emulator puts `ACH_` before it (and the level after it) for the badge, ACH_RoomEntry1,
/// so the name itself is RoomEntry, of letters, digits and _.
pub fn achievement_name_problem(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some("The name has only letters, digits and _ (RoomEntry, ...).".into());
    }
    name.starts_with("ACH_").then(|| "Leave ACH_ out of the name: the emulator adds it for the badge.".into())
}

/// The row has the same values in `columns` as another row (a unique key of the table), or None.
pub fn duplicate_problem(spec: &TableSpec, row: &Row, others: &[Row], columns: &[&str]) -> Option<String> {
    let key = |r: &Row| columns.iter().map(|c| r.get(spec, c).trim().to_lowercase()).collect::<Vec<_>>();
    let mine = key(row);
    others.iter().any(|other| other.id != row.id && key(other) == mine).then(|| {
        let values = columns.iter().map(|c| row.get(spec, c).trim().to_string()).collect::<Vec<_>>().join(" / ");
        format!("{values} is there already.")
    })
}

/// A poll question's options: for type 1 (one answer) and 2 (several) `value:label;value:label`, at least two;
/// type 3 (written answer) has none. The emulator splits each option at `:` and fails on one without it.
pub fn poll_options_problem(kind: i64, options: &str) -> Option<String> {
    match kind {
        1 | 2 => {
            let parts: Vec<&str> = options.split(';').map(str::trim).filter(|p| !p.is_empty()).collect();
            if parts.len() < 2 {
                return Some("Give at least two options: value:label;value:label.".into());
            }
            parts
                .iter()
                .find(|p| p.split(':').count() != 2 || p.split(':').any(|half| half.trim().is_empty()))
                .map(|p| format!("The option {p} is not value:label."))
        }
        3 => None,
        _ => Some("The type is 1 (one answer), 2 (several answers) or 3 (written answer).".into()),
    }
}

/// A goal type the emulator knows (by its action name or its enum name).
pub fn goal_type_problem(goal: &str) -> Option<String> {
    let goal = goal.trim();
    let known = GOAL_TYPES.iter().chain(GOAL_ENUM_NAMES).any(|g| g.eq_ignore_ascii_case(goal));
    (!known).then(|| format!("Unknown goal {goal}."))
}

/// The kinds of group badge part (guilds_elements.type, GuildPartType in lower case).
pub const BADGE_PART_TYPES: &[&str] = &["base", "symbol", "base_color", "symbol_color", "background_color"];

/// What a pet says when (pet_vocals.type).
pub const PET_SOUND_TYPES: &[&str] = &[
    "DISOBEY", "DRINKING", "EATING", "GENERIC_HAPPY", "GENERIC_NEUTRAL", "GENERIC_SAD", "GREET_OWNER", "HUNGRY", "LEVEL_UP", "MUTED",
    "PLAYFUL", "SLEEPING", "THIRSTY", "TIRED", "UNKNOWN_COMMAND",
];

/// A group badge part: a colour (the three colour types) is six hex digits, `ffd601`; a base or symbol names its
/// image, `base_basic_1.gif` (and a second image for a part in two layers, or nothing).
pub fn badge_part_problem(kind: &str, first: &str, second: &str) -> Option<String> {
    let first = first.trim();
    if kind.ends_with("_color") {
        let hex = first.len() == 6 && first.chars().all(|c| c.is_ascii_hexdigit());
        return (!hex).then(|| "A colour is six hex digits, like ffd601.".into());
    }
    if first.is_empty() {
        return Some("Give the image of the part, like base_basic_1.gif.".into());
    }
    [first, second.trim()]
        .iter()
        .find(|name| !name.is_empty() && !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map(|name| format!("{name} is not an image name (letters, digits, _ - and .)."))
}

/// A calendar reward's day: empty (the random pool for the days without their own reward) or a day of the campaign,
/// counted from 1.
pub fn calendar_day_problem(day: &str, total_days: i64) -> Option<String> {
    let day = day.trim();
    if day.is_empty() {
        return None;
    }
    match day.parse::<i64>() {
        Ok(n) if n >= 1 && n <= total_days.max(1) => None,
        _ => Some(format!("The day is empty or from 1 to {}, the days of the campaign.", total_days.max(1))),
    }
}

/// What a calendar reward gives: at least one of the furni, credits, pixels, points, badge or subscription.
pub fn calendar_reward_problem(product: &str, item_id: &str, amounts: &[&str], badge: &str, subscription_days: &str) -> Option<String> {
    let positive = |v: &str| v.trim().parse::<i64>().is_ok_and(|n| n > 0);
    let gives = !product.trim().is_empty() || positive(item_id) || amounts.iter().any(|a| positive(a)) || !badge.trim().is_empty() || positive(subscription_days);
    (!gives).then(|| "The reward gives nothing: give a furni, an amount, a badge or a subscription.".into())
}

// ---- Currency settings (emulator_settings) --------------------------------------------------------------------

/// The emulator settings that name the hotel's currencies.
pub const CURRENCY_SETTINGS: &[&str] = &[
    "seasonal.types",
    "seasonal.primary.type",
    "seasonal.currency.names",
    "seasonal.currency.ducket",
    "seasonal.currency.pixel",
    "seasonal.currency.shell",
    "seasonal.currency.diamond",
    "hotel.marketplace.currency",
];

/// One emulator setting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub comment: String,
}

/// The currency settings (`?` once per key of [`CURRENCY_SETTINGS`]).
pub fn currency_settings_sql(database: &str) -> String {
    let marks = vec!["?"; CURRENCY_SETTINGS.len()].join(", ");
    format!("SELECT `key`, `value`, COALESCE(`comment`, '') FROM {}.emulator_settings WHERE `key` IN ({marks}) ORDER BY `key`", quote(database))
}

/// How many players hold each currency type and how much in all.
pub fn currency_totals_sql(database: &str) -> String {
    format!("SELECT `type`, COUNT(*), COALESCE(SUM(`amount`), 0) FROM {}.users_currency GROUP BY `type` ORDER BY `type`", quote(database))
}

pub fn update_setting_sql(database: &str) -> String {
    format!("UPDATE {}.emulator_settings SET `value` = ? WHERE `key` = ?", quote(database))
}

/// Why the value cannot go into the setting, or None: a list of type numbers for `seasonal.types`, names for
/// `seasonal.currency.names`, a type number for the others.
pub fn setting_problem(key: &str, value: &str) -> Option<String> {
    let value = value.trim();
    let number = |v: &str| v.parse::<i64>().ok().filter(|n| (0..=1000).contains(n)).is_some();
    match key {
        "seasonal.types" => {
            let parts: Vec<&str> = value.split(';').map(str::trim).collect();
            (parts.is_empty() || !parts.iter().all(|p| number(p))).then(|| "Type numbers 0 to 1000, separated by ;.".into())
        }
        "seasonal.currency.names" => {
            (value.is_empty() || !value.split(';').all(|p| !p.trim().is_empty() && p.trim().chars().all(|c| c.is_ascii_alphanumeric() || c == '_')))
                .then(|| "Names of letters and digits, separated by ;.".into())
        }
        _ => (!number(value)).then(|| "A type number from 0 to 1000.".into()),
    }
}

/// A currency type with how many players hold it and how much in all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrencyTotal {
    pub kind: i64,
    pub players: i64,
    pub amount: i64,
}

pub async fn load_currency_settings(session: &SqlSession, database: &str) -> Result<Vec<Setting>> {
    let params = CURRENCY_SETTINGS.iter().map(|k| SqlValue::Text(k.to_string())).collect();
    let rows = session.select(&currency_settings_sql(database), params).await?;
    Ok(rows.iter().map(|r| Setting { key: cell(r.first()), value: cell(r.get(1)), comment: cell(r.get(2)) }).collect())
}

pub async fn load_currency_totals(session: &SqlSession, database: &str) -> Result<Vec<CurrencyTotal>> {
    let rows = session.select(&currency_totals_sql(database), Vec::new()).await?;
    Ok(rows
        .iter()
        .map(|r| CurrencyTotal {
            kind: cell(r.first()).parse().unwrap_or(0),
            players: cell(r.get(1)).parse().unwrap_or(0),
            amount: cell(r.get(2)).parse().unwrap_or(0),
        })
        .collect())
}

pub async fn save_setting(session: &SqlSession, database: &str, key: &str, value: &str) -> Result<u64> {
    let statement = (update_setting_sql(database), vec![SqlValue::Text(value.trim().to_string()), SqlValue::Text(key.to_string())]);
    Ok(session.execute(&[statement]).await?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELDS: &[Field] = &[
        Field { column: "name", label: "Name", kind: FieldKind::Text { max: 10 }, default: "ACH_" },
        Field { column: "level", label: "Level", kind: FieldKind::Int { min: 1, max: 100 }, default: "1" },
        Field { column: "enabled", label: "Enabled", kind: FieldKind::Flag, default: "1" },
        Field { column: "category", label: "Category", kind: FieldKind::Choice(&["identity", "games"]), default: "identity" },
        Field { column: "poll_id", label: "Poll", kind: FieldKind::Int { min: 0, max: 1000 }, default: "0" },
    ];
    const SPEC: TableSpec = TableSpec { table: "things", id: "id", key: &[], fields: FIELDS, order_by: "`name`, `level`", parent: Some("poll_id") };
    const KEYED_FIELDS: &[Field] = &[
        Field { column: "id", label: "Id", kind: FieldKind::Int { min: 1, max: 1000 }, default: "1" },
        Field { column: "type", label: "Type", kind: FieldKind::Choice(BADGE_PART_TYPES), default: "base" },
        Field { column: "day", label: "Day", kind: FieldKind::OptInt { min: 1, max: 31 }, default: "" },
    ];
    const KEYED: TableSpec = TableSpec { table: "parts", id: "", key: &["id", "type"], fields: KEYED_FIELDS, order_by: "`type`, `id`", parent: None };

    #[test]
    fn a_keyed_table_finds_rows_by_their_stored_key() {
        assert_eq!(select_sql(&KEYED, "hotel"), "SELECT 0, `id`, `type`, `day` FROM `hotel`.`parts` ORDER BY `type`, `id`");
        assert_eq!(update_sql(&KEYED, "hotel"), "UPDATE `hotel`.`parts` SET `id` = ?, `type` = ?, `day` = ? WHERE `id` = ? AND `type` = ? LIMIT 1");
        assert_eq!(delete_sql(&KEYED, "hotel"), "DELETE FROM `hotel`.`parts` WHERE `id` = ? AND `type` = ? LIMIT 1");
        let mut row = new_row(&KEYED, None);
        assert!(is_new(&KEYED, &row));
        let read = stored(&KEYED, row_from(&[SqlValue::Int(0), SqlValue::Int(4), SqlValue::Text("symbol".into()), SqlValue::Null]), 2);
        assert_eq!((read.id, read.key.clone(), read.values[2].as_str()), (3, vec!["4".to_string(), "symbol".to_string()], ""));
        assert!(!is_new(&KEYED, &read));
        let mut moved = read.clone();
        moved.values[0] = "5".into();
        assert!(same_row(&KEYED, &read, &moved), "found by the key it is stored under, not the value typed");
        assert_eq!(filter_params(&KEYED, &moved), vec![SqlValue::Int(4), SqlValue::Text("symbol".into())]);
        assert_eq!(link_value(&KEYED, &read), 4);
        assert!(!same_row(&KEYED, &row, &read));
        row.values[0] = "2".into();
        assert_eq!(key_values(&KEYED, &row), vec!["2", "base"]);
        assert!(!same_row(&SPEC, &new_row(&SPEC, None), &new_row(&SPEC, None)), "two new rows are not the same row");
        assert_eq!(next_number([3, 9, 4], 1), 10);
        assert_eq!(next_number([0, 2], 46), 46);
        assert_eq!(next_number([], 1), 1);
        assert_eq!(count_sql("hotel", "rooms", "category"), "SELECT COUNT(*) FROM `hotel`.`rooms` WHERE `category` = ?");
    }

    #[test]
    fn an_empty_optional_number_is_null() {
        let day = &KEYED_FIELDS[2];
        assert_eq!(param(day, " "), SqlValue::Null);
        assert_eq!(param(day, "3"), SqlValue::Int(3));
        assert_eq!(field_problem(day, ""), None);
        assert_eq!(field_problem(day, "32"), Some("Day: from 1 to 31.".into()));
        assert_eq!(param(&KEYED_FIELDS[1], "symbol"), SqlValue::Text("symbol".into()), "an ENUM goes as text");
    }

    #[test]
    fn badge_parts_and_calendar_rewards_are_checked() {
        assert_eq!(badge_part_problem("base_color", "ffd601", ""), None);
        assert!(badge_part_problem("symbol_color", "#ffd601", "").is_some());
        assert!(badge_part_problem("background_color", "fff", "").is_some());
        assert_eq!(badge_part_problem("base", "base_basic_1.gif", ""), None);
        assert_eq!(badge_part_problem("symbol", "symbol_a.gif", "symbol_a_part2.gif"), None);
        assert!(badge_part_problem("symbol", "", "").is_some());
        assert!(badge_part_problem("base", "my base.gif", "").is_some());
        assert_eq!(calendar_day_problem("", 30), None);
        assert_eq!(calendar_day_problem("30", 30), None);
        assert!(calendar_day_problem("31", 30).is_some());
        assert!(calendar_day_problem("0", 30).is_some());
        assert!(calendar_day_problem("x", 30).is_some());
        assert_eq!(calendar_reward_problem("", "0", &["0", "50", "0"], "", "0"), None);
        assert_eq!(calendar_reward_problem("throne", "0", &["0"], "", "0"), None);
        assert_eq!(calendar_reward_problem("", "0", &["0"], "", "31"), None);
        assert!(calendar_reward_problem(" ", "0", &["0", "-5"], " ", "0").is_some());
    }

    #[test]
    fn statements_follow_the_spec() {
        assert_eq!(select_sql(&SPEC, "hotel"), "SELECT `id`, `name`, `level`, `enabled`, `category`, `poll_id` FROM `hotel`.`things` WHERE `poll_id` = ? ORDER BY `name`, `level`");
        assert_eq!(insert_sql(&SPEC, "hotel").matches('?').count(), FIELDS.len());
        assert_eq!(update_sql(&SPEC, "hotel").matches('?').count(), FIELDS.len() + 1);
        assert!(update_sql(&SPEC, "hotel").ends_with("WHERE `id` = ?"));
        assert_eq!(delete_sql(&SPEC, "h`x"), "DELETE FROM `h``x`.`things` WHERE `id` = ?");
        assert_eq!(param(&FIELDS[1], " 7 "), SqlValue::Int(7));
        assert_eq!(param(&FIELDS[0], " ACH_A "), SqlValue::Text("ACH_A".into()));
    }

    #[test]
    fn a_new_row_takes_the_defaults_and_its_parent() {
        let row = new_row(&SPEC, Some(12));
        assert_eq!(row.id, 0);
        assert_eq!(row.values, vec!["ACH_", "1", "1", "identity", "12"]);
        assert_eq!(row.get(&SPEC, "level"), "1");
        let read = row_from(&[SqlValue::Int(5), SqlValue::Text("ACH_A".into()), SqlValue::Int(2), SqlValue::Int(0), SqlValue::Text("games".into()), SqlValue::Int(3)]);
        assert_eq!((read.id, read.values.len()), (5, 5));
        let null = row_from(&[SqlValue::Int(6), SqlValue::Text("A".into()), SqlValue::Null, SqlValue::Null, SqlValue::Text("games".into()), SqlValue::Int(3)]);
        assert_eq!(filled(&SPEC, null).values, vec!["A", "1", "1", "games", "3"]);
    }

    #[test]
    fn values_are_checked_by_their_kind() {
        let mut row = new_row(&SPEC, None);
        assert_eq!(row_problem(&SPEC, &row), None);
        row.values[1] = "0".into();
        assert_eq!(row_problem(&SPEC, &row), Some("Level: from 1 to 100.".into()));
        row.values[1] = "x".into();
        assert_eq!(row_problem(&SPEC, &row), Some("Level: a whole number.".into()));
        row.values[1] = "2".into();
        row.values[0] = "ACH_TOO_LONG_NAME".into();
        assert_eq!(row_problem(&SPEC, &row), Some("Name: at most 10 characters.".into()));
        row.values[0] = "ACH_A".into();
        row.values[2] = "2".into();
        assert_eq!(row_problem(&SPEC, &row), Some("Enabled: 0 or 1.".into()));
        row.values[2] = "0".into();
        row.values[3] = "music".into();
        assert_eq!(row_problem(&SPEC, &row), Some("Category: one of identity, games.".into()));
    }

    #[test]
    fn a_unique_key_is_there_once() {
        let row = |id: i64, name: &str, level: &str| Row { id, values: vec![name.into(), level.into(), "1".into(), "identity".into(), "0".into()], key: Vec::new() };
        let others = vec![row(1, "RoomEntry", "1"), row(2, "RoomEntry", "2")];
        assert_eq!(duplicate_problem(&SPEC, &row(0, "RoomEntry", "3"), &others, &["name", "level"]), None);
        assert_eq!(duplicate_problem(&SPEC, &row(2, "RoomEntry", "2"), &others, &["name", "level"]), None, "saving itself");
        assert_eq!(duplicate_problem(&SPEC, &row(0, "roomentry", "2"), &others, &["name", "level"]), Some("roomentry / 2 is there already.".into()));
        assert_eq!(achievement_name_problem("RoomEntry"), None);
        assert!(achievement_name_problem("ACH_RoomEntry").is_some());
        assert!(achievement_name_problem("Room Entry").is_some());
        assert!(achievement_name_problem("").is_some());
    }

    #[test]
    fn poll_options_are_value_label_pairs() {
        assert_eq!(poll_options_problem(1, "1:Yes;2:No"), None);
        assert_eq!(poll_options_problem(2, "a:Red; b:Blue ;c:Green"), None);
        assert_eq!(poll_options_problem(3, ""), None);
        assert!(poll_options_problem(1, "1:Yes").is_some());
        assert_eq!(poll_options_problem(1, "1:Yes;No"), Some("The option No is not value:label.".into()));
        assert!(poll_options_problem(2, "1:Yes;2:").is_some());
        assert!(poll_options_problem(4, "").is_some());
    }

    #[test]
    fn goals_and_currency_settings_are_known() {
        assert_eq!(goal_type_problem("place_item"), None);
        assert_eq!(goal_type_problem("PLACE_FURNI"), None);
        assert!(goal_type_problem("jump around").is_some());
        assert!(goal_type_problem("JUMP_AROUND").is_some());
        assert_eq!(GOAL_TYPES.len(), GOAL_ENUM_NAMES.len());
        assert_eq!(setting_problem("seasonal.types", "0;1;5;101"), None);
        assert!(setting_problem("seasonal.types", "0;x").is_some());
        assert_eq!(setting_problem("seasonal.currency.names", "ducket;pixel;shell"), None);
        assert!(setting_problem("seasonal.currency.names", "duc ket").is_some());
        assert_eq!(setting_problem("seasonal.primary.type", "5"), None);
        assert!(setting_problem("hotel.marketplace.currency", "-1").is_some());
        assert_eq!(currency_settings_sql("hotel").matches('?').count(), CURRENCY_SETTINGS.len());
    }
}
