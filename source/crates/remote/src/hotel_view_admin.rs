//! The hotel view (the landing page players see before they enter a room) as Polaris stores it:
//!
//! * `hotelview_landing_settings` (one row, id 1): the scene images and where they sit, and the Hall of Fame box.
//! * `hotelview_landing_slots` (ids 1 to 5): the widgets, each with a type, texts, a link and a JSON config
//!   (`config_json`) the client reads more options from (`positionX`, `endsAt`, `catalogPage`, `voteOptions`, ...).
//! * `hotelview_landing_votes`: the votes cast on a vote slot.
//! * `hotelview_news`: the news articles.
//! * `emulator_settings`: the bonus rare and the community goal settings the widgets read.
//!
//! Every value is a statement parameter. The slot config is parsed with serde_json before it is saved: the known keys
//! edited in the window are written into the object and every other key is kept as it was. The emulator reads these
//! tables when it starts; `:update_hotel_view` (sent over RCON `executecommand` as an online staff player) loads the
//! slots, the scene, the news and the Hall of Fame again and refreshes the players online.

use serde_json::{Map, Value};

use crate::content_admin::{Field, FieldKind, Setting, TableSpec};
use crate::rcon::{self, Answer};
use crate::server::{Result, Server};
use crate::sql_session::{quote, SqlSession, SqlValue};

/// The slots the emulator reads (`HotelViewManager` skips ids above 5).
pub const SLOT_IDS: std::ops::RangeInclusive<i64> = 1..=5;

/// The longest slot config the emulator keeps (`HotelViewLandingSaveEvent`); a longer one is replaced by `{}`.
pub const CONFIG_MAX: usize = 3000;

/// The slot types the emulator accepts (`HotelViewLandingSaveEvent.SLOT_TYPES`).
pub const SLOT_TYPES: &[&str] = &[
    "promotion",
    "bonus",
    "catalogpromo",
    "catalogpromosmall",
    "expiringcatalogpage",
    "expiringcatalogpagesmall",
    "communitygoal",
    "dailyquest",
    "nextlimitedrarecountdown",
    "achievementcompetition_hall_of_fame",
    "achievementcompetition_prizes",
    "habbotalentspromo",
    "habbowaypromo",
    "safetyquizpromo",
    "habbomoderationpromo",
    "promoarticle",
    "avatarimage",
    "roomhoppernetwork",
    "generic",
    "communitygoalvsmode",
    "communitygoalvsmodevote",
    "widgetcontainer",
    "concurrentusersgoal",
];

/// The Hall of Fame lists (`HotelViewManager.normalizeHallOfFameMode`; anything else reads as the first).
pub const HALL_OF_FAME_MODES: &[&str] = &["latest_registered", "online_time", "achievement_score", "currency"];

/// The command that loads the hotel view again (`commands.keys.cmd_update_hotel_view`).
pub const RELOAD_COMMAND: &str = ":update_hotel_view";

/// Keys the emulator adds to a slot's config while it runs; they are never saved.
const RUNTIME_KEYS: &[&str] = &["userVote", "voteCounts"];

// ---- Scene ------------------------------------------------------------------------------------------------------

/// The scene and the Hall of Fame box (`hotelview_landing_settings`, id 1). A position of -1 is the client's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub background_url: String,
    pub left_url: String,
    pub right_url: String,
    pub drape_url: String,
    /// left x/y, right x/y, drape x/y, Hall of Fame x/y.
    pub positions: [i64; 8],
    pub hall_of_fame_enabled: bool,
    pub hall_of_fame_mode: String,
    pub hall_of_fame_currency_type: i64,
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            background_url: String::new(),
            left_url: String::new(),
            right_url: String::new(),
            drape_url: String::new(),
            positions: [-1; 8],
            hall_of_fame_enabled: false,
            hall_of_fame_mode: HALL_OF_FAME_MODES[0].into(),
            hall_of_fame_currency_type: 0,
        }
    }
}

const SCENE_COLUMNS: &str = "`background_url`, `left_url`, `right_url`, `drape_url`, `left_x`, `left_y`, `right_x`, `right_y`, `drape_x`, `drape_y`, `hall_of_fame_x`, `hall_of_fame_y`, `hall_of_fame_enabled`, `hall_of_fame_mode`, `hall_of_fame_currency_type`";

pub fn scene_sql(database: &str) -> String {
    format!("SELECT {SCENE_COLUMNS} FROM {}.`hotelview_landing_settings` WHERE `id` = 1", quote(database))
}

/// Writes row 1, adding it when the table is empty (the migration adds it, a hand-made table may not).
pub fn save_scene_sql(database: &str) -> String {
    let marks = vec!["?"; 16].join(", ");
    let updates = SCENE_COLUMNS.split(", ").map(|c| format!("{c} = VALUES({c})")).collect::<Vec<_>>().join(", ");
    format!("INSERT INTO {}.`hotelview_landing_settings` (`id`, {SCENE_COLUMNS}) VALUES ({marks}) ON DUPLICATE KEY UPDATE {updates}", quote(database))
}

/// The URL fields' size (VARCHAR(512)).
pub const URL_MAX: usize = 512;

/// Why the scene cannot be saved, or None.
pub fn scene_problem(scene: &Scene) -> Option<String> {
    for (label, url) in [("Background", &scene.background_url), ("Left", &scene.left_url), ("Right", &scene.right_url), ("Drape", &scene.drape_url)] {
        if url.trim().chars().count() > URL_MAX {
            return Some(format!("{label}: at most {URL_MAX} characters."));
        }
    }
    if scene.positions.iter().any(|p| !(-1..=10_000).contains(p)) {
        return Some("Positions: from -1 (the default place) to 10000.".into());
    }
    if !HALL_OF_FAME_MODES.contains(&scene.hall_of_fame_mode.as_str()) {
        return Some(format!("Hall of Fame: one of {}.", HALL_OF_FAME_MODES.join(", ")));
    }
    if !(0..=1000).contains(&scene.hall_of_fame_currency_type) {
        return Some("Hall of Fame currency type: from 0 to 1000.".into());
    }
    None
}

fn scene_params(scene: &Scene) -> Vec<SqlValue> {
    let mut params = vec![SqlValue::Int(1)];
    for url in [&scene.background_url, &scene.left_url, &scene.right_url, &scene.drape_url] {
        params.push(SqlValue::Text(url.trim().to_string()));
    }
    params.extend(scene.positions.iter().map(|p| SqlValue::Int(*p)));
    params.push(SqlValue::Int(scene.hall_of_fame_enabled as i64));
    params.push(SqlValue::Text(scene.hall_of_fame_mode.clone()));
    params.push(SqlValue::Int(scene.hall_of_fame_currency_type));
    params
}

// ---- Slots ------------------------------------------------------------------------------------------------------

/// One widget of the hotel view (`hotelview_landing_slots`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Slot {
    pub id: i64,
    pub enabled: bool,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub image_url: String,
    pub button_text: String,
    pub link: String,
    pub progress: i64,
    pub progress_label: String,
    pub config_json: String,
}

pub fn slots_sql(database: &str) -> String {
    format!(
        "SELECT `id`, `enabled`, `type`, `title`, `body`, `image_url`, `button_text`, `link`, `progress`, `progress_label`, `config_json` FROM {}.`hotelview_landing_slots` WHERE `id` BETWEEN 1 AND 5 ORDER BY `id`",
        quote(database)
    )
}

/// Writes a slot, adding it when its row is missing.
pub fn save_slot_sql(database: &str) -> String {
    format!(
        "INSERT INTO {}.`hotelview_landing_slots` (`id`, `enabled`, `type`, `title`, `body`, `image_url`, `button_text`, `link`, `progress`, `progress_label`, `config_json`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE `enabled` = VALUES(`enabled`), `type` = VALUES(`type`), `title` = VALUES(`title`), `body` = VALUES(`body`), `image_url` = VALUES(`image_url`), `button_text` = VALUES(`button_text`), `link` = VALUES(`link`), `progress` = VALUES(`progress`), `progress_label` = VALUES(`progress_label`), `config_json` = VALUES(`config_json`)",
        quote(database)
    )
}

pub fn vote_counts_sql(database: &str) -> String {
    format!("SELECT `slot_id`, COUNT(*) FROM {}.`hotelview_landing_votes` GROUP BY `slot_id`", quote(database))
}

pub fn reset_votes_sql(database: &str) -> String {
    format!("DELETE FROM {}.`hotelview_landing_votes` WHERE `slot_id` = ?", quote(database))
}

/// Why the slot cannot be saved, or None. The config is checked with [`config_problem`].
pub fn slot_problem(slot: &Slot) -> Option<String> {
    if !SLOT_IDS.contains(&slot.id) {
        return Some("Slots are numbered 1 to 5.".into());
    }
    if !SLOT_TYPES.contains(&slot.kind.as_str()) {
        return Some(format!("Type: unknown type {}.", slot.kind));
    }
    let too_long = |label: &str, value: &str, max: usize| (value.trim().chars().count() > max).then(|| format!("{label}: at most {max} characters."));
    too_long("Title", &slot.title, 100)
        .or_else(|| too_long("Text", &slot.body, 500))
        .or_else(|| too_long("Image", &slot.image_url, URL_MAX))
        .or_else(|| too_long("Button text", &slot.button_text, 100))
        .or_else(|| too_long("Link", &slot.link, URL_MAX))
        .or_else(|| too_long("Progress text", &slot.progress_label, 100))
        .or_else(|| (!(0..=100).contains(&slot.progress)).then(|| "Progress: from 0 to 100.".into()))
        .or_else(|| config_problem(&slot.config_json))
}

fn slot_params(slot: &Slot) -> Vec<SqlValue> {
    vec![
        SqlValue::Int(slot.id),
        SqlValue::Int(slot.enabled as i64),
        SqlValue::Text(slot.kind.clone()),
        SqlValue::Text(slot.title.trim().to_string()),
        SqlValue::Text(slot.body.trim().to_string()),
        SqlValue::Text(slot.image_url.trim().to_string()),
        SqlValue::Text(slot.button_text.trim().to_string()),
        SqlValue::Text(slot.link.trim().to_string()),
        SqlValue::Int(slot.progress),
        SqlValue::Text(slot.progress_label.trim().to_string()),
        SqlValue::Text(slot.config_json.clone()),
    ]
}

// ---- Slot config (JSON) -----------------------------------------------------------------------------------------

/// The config keys the window edits as fields, and what they hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    Text,
    Number,
    Flag,
}

/// A key of the slot config edited as a field. `label` is the English text the window translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigKey {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ConfigKind,
}

pub const CONFIG_KEYS: &[ConfigKey] = &[
    ConfigKey { key: "positionX", label: "Position x", kind: ConfigKind::Number },
    ConfigKey { key: "positionY", label: "Position y", kind: ConfigKind::Number },
    ConfigKey { key: "secondaryButtonText", label: "Second button text", kind: ConfigKind::Text },
    ConfigKey { key: "secondaryLink", label: "Second button link", kind: ConfigKind::Text },
    ConfigKey { key: "catalogPage", label: "Catalogue page (name or id)", kind: ConfigKind::Text },
    ConfigKey { key: "featuredSlot", label: "Featured catalogue slot", kind: ConfigKind::Number },
    ConfigKey { key: "endsAt", label: "Ends at (date and time)", kind: ConfigKind::Text },
    ConfigKey { key: "badgeCode", label: "Badge code", kind: ConfigKind::Text },
    ConfigKey { key: "goalCode", label: "Goal code", kind: ConfigKind::Text },
    ConfigKey { key: "useServerLTD", label: "Use the emulator's LTD", kind: ConfigKind::Flag },
    ConfigKey { key: "useServerBonusRare", label: "Use the emulator's bonus rare", kind: ConfigKind::Flag },
    ConfigKey { key: "useServerGoal", label: "Use the emulator's community goal", kind: ConfigKind::Flag },
    ConfigKey { key: "useServerQuest", label: "Use the emulator's daily quest", kind: ConfigKind::Flag },
];

/// The config keys that matter for a slot type (the ones the client reads for it), plus the position.
pub fn config_keys_for(kind: &str) -> Vec<&'static ConfigKey> {
    let wanted: &[&str] = match kind {
        "catalogpromo" | "catalogpromosmall" => &["catalogPage"],
        "expiringcatalogpage" | "expiringcatalogpagesmall" => &["catalogPage", "featuredSlot", "endsAt"],
        "nextlimitedrarecountdown" => &["catalogPage", "endsAt", "useServerLTD"],
        "bonus" => &["useServerBonusRare"],
        "communitygoal" | "communitygoalvsmode" | "communitygoalvsmodevote" => &["useServerGoal", "goalCode", "badgeCode"],
        "concurrentusersgoal" => &["badgeCode"],
        "dailyquest" => &["useServerQuest", "secondaryButtonText", "secondaryLink"],
        _ => &["secondaryButtonText", "secondaryLink"],
    };
    CONFIG_KEYS.iter().filter(|k| k.key == "positionX" || k.key == "positionY" || wanted.contains(&k.key)).collect()
}

/// The config as an object: `{}` for an empty text, an error for text that is not a JSON object.
pub fn parse_config(text: &str) -> std::result::Result<Map<String, Value>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err("The config must be a JSON object ({...}).".into()),
        Err(error) => Err(format!("The config is not valid JSON: {error}")),
    }
}

/// Why the config cannot be saved, or None: it must be a JSON object of at most [`CONFIG_MAX`] characters (the
/// emulator replaces anything else by `{}`).
pub fn config_problem(text: &str) -> Option<String> {
    if let Err(error) = parse_config(text) {
        return Some(error);
    }
    (text.chars().count() > CONFIG_MAX).then(|| format!("The config is longer than {CONFIG_MAX} characters."))
}

/// A known key's value as the window shows it: text as is, a number in digits, a flag as "1"/"0", "" when unset.
pub fn config_value(config: &Map<String, Value>, key: &str) -> String {
    match config.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => if *flag { "1" } else { "0" }.into(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Writes the known keys' field values into the config and returns it as compact JSON. An empty field removes its
/// key; every key not in `values` is kept. The emulator's runtime keys (`userVote`, `voteCounts`) are dropped.
pub fn merge_config(text: &str, values: &[(&ConfigKey, String)]) -> std::result::Result<String, String> {
    let mut config = parse_config(text)?;
    for (key, value) in values {
        let value = value.trim();
        if value.is_empty() {
            config.remove(key.key);
            continue;
        }
        let json = match key.kind {
            ConfigKind::Text => Value::String(value.to_string()),
            ConfigKind::Number => match value.parse::<i64>() {
                Ok(number) => Value::from(number),
                Err(_) => return Err(format!("{}: a whole number.", key.label)),
            },
            ConfigKind::Flag => match value {
                "1" | "true" => Value::Bool(true),
                "0" | "false" => Value::Bool(false),
                _ => return Err(format!("{}: yes or no.", key.label)),
            },
        };
        config.insert(key.key.to_string(), json);
    }
    for key in RUNTIME_KEYS {
        config.remove(*key);
    }
    let json = Value::Object(config).to_string();
    match config_problem(&json) {
        Some(problem) => Err(problem),
        None => Ok(json),
    }
}

/// The config indented for reading and editing (the text as is when it is not valid JSON).
pub fn pretty_config(text: &str) -> String {
    match parse_config(text) {
        Ok(config) if config.is_empty() => "{}".into(),
        Ok(config) => serde_json::to_string_pretty(&Value::Object(config)).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

// ---- News -------------------------------------------------------------------------------------------------------

const NEWS_FIELDS: &[Field] = &[
    Field { column: "title", label: "Title", kind: FieldKind::Text { max: 100 }, default: "" },
    Field { column: "text", label: "Text", kind: FieldKind::Text { max: 500 }, default: "" },
    Field { column: "button_text", label: "Button text", kind: FieldKind::Text { max: 50 }, default: "" },
    Field { column: "button_type", label: "Button opens", kind: FieldKind::Choice(&["web", "client"]), default: "web" },
    Field { column: "button_link", label: "Button link", kind: FieldKind::Text { max: 200 }, default: "" },
    Field { column: "image", label: "Image", kind: FieldKind::Text { max: 200 }, default: "" },
];

/// `hotelview_news`: the emulator shows the ten newest (highest id) articles.
pub const NEWS: TableSpec = TableSpec { table: "hotelview_news", id: "id", key: &[], fields: NEWS_FIELDS, order_by: "`id` DESC", parent: None };

/// The articles the emulator shows (`NewsList`: `ORDER BY id DESC LIMIT 10`).
pub const NEWS_SHOWN: usize = 10;

// ---- Emulator settings ------------------------------------------------------------------------------------------

/// The emulator settings the hotel view widgets read, with what each holds. The emulator reads them at start and
/// with `:update_config`.
pub const SETTINGS: &[(&str, &str)] = &[
    ("hotelview.promotional.reward.id", "Bonus rare: the furni id (items_base) players get"),
    ("hotelview.promotional.reward.name", "Bonus rare: the furni's class name, for its image"),
    ("hotelview.promotional.points", "Bonus rare: how many points earn it"),
    ("hotelview.promotional.points.type", "Bonus rare: the currency type that counts"),
    ("hotel.communitygoal.concurrentusers.goal", "Concurrent users goal (0 hides the widget)"),
    ("hotel.communitygoal.concurrentusers.badge", "Concurrent users goal: the badge given when it is reached"),
    ("hotel.communitygoal.halloffame.size", "Community goal Hall of Fame: how many players it lists (1 to 10)"),
];

pub fn settings_sql(database: &str) -> String {
    let marks = vec!["?"; SETTINGS.len()].join(", ");
    format!("SELECT `key`, `value`, COALESCE(`comment`, '') FROM {}.`emulator_settings` WHERE `key` IN ({marks})", quote(database))
}

/// Sets a setting, adding it when it is not in the table yet.
pub fn save_setting_sql(database: &str) -> String {
    format!("INSERT INTO {}.`emulator_settings` (`key`, `value`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `value` = VALUES(`value`)", quote(database))
}

/// Why the value cannot go into the setting, or None.
pub fn setting_problem(key: &str, value: &str) -> Option<String> {
    let value = value.trim();
    let whole = |min: i64, max: i64| value.parse::<i64>().ok().filter(|n| (min..=max).contains(n)).is_none().then(|| format!("A whole number from {min} to {max}."));
    match key {
        "hotelview.promotional.reward.name" => {
            (value.is_empty() || value.chars().count() > 100 || value.contains(char::is_whitespace)).then(|| "A furni class name without spaces.".into())
        }
        "hotel.communitygoal.concurrentusers.badge" => {
            (value.is_empty() || value.chars().count() > 32 || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .then(|| "A badge code: 1 to 32 letters, digits, _ or -.".into())
        }
        "hotel.communitygoal.halloffame.size" => whole(1, 10),
        "hotelview.promotional.points.type" => whole(0, 1000),
        _ => whole(0, 2_000_000_000),
    }
}

// ---- Loading and saving -----------------------------------------------------------------------------------------

/// Everything the page shows. A table that is missing (an older database) is named in `missing`.
#[derive(Debug, Clone, Default)]
pub struct HotelView {
    pub scene: Option<Scene>,
    pub slots: Vec<Slot>,
    /// Votes cast per slot id.
    pub votes: Vec<(i64, i64)>,
    pub news: Vec<crate::content_admin::Row>,
    pub settings: Vec<Setting>,
    pub missing: Vec<String>,
}

fn int(value: Option<&SqlValue>) -> i64 {
    match value {
        Some(SqlValue::Int(v)) => *v,
        Some(SqlValue::UInt(v)) => *v as i64,
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

pub fn scene_from(row: &[SqlValue]) -> Scene {
    let mut positions = [-1; 8];
    for (i, position) in positions.iter_mut().enumerate() {
        *position = int(row.get(4 + i));
    }
    let mode = text(row.get(13));
    Scene {
        background_url: text(row.first()),
        left_url: text(row.get(1)),
        right_url: text(row.get(2)),
        drape_url: text(row.get(3)),
        positions,
        hall_of_fame_enabled: int(row.get(12)) != 0,
        hall_of_fame_mode: if HALL_OF_FAME_MODES.contains(&mode.as_str()) { mode } else { HALL_OF_FAME_MODES[0].into() },
        hall_of_fame_currency_type: int(row.get(14)).max(0),
    }
}

pub fn slot_from(row: &[SqlValue]) -> Slot {
    Slot {
        id: int(row.first()),
        enabled: int(row.get(1)) != 0,
        kind: text(row.get(2)),
        title: text(row.get(3)),
        body: text(row.get(4)),
        image_url: text(row.get(5)),
        button_text: text(row.get(6)),
        link: text(row.get(7)),
        progress: int(row.get(8)),
        progress_label: text(row.get(9)),
        config_json: text(row.get(10)),
    }
}

/// The slots 1 to 5: the stored ones, and a disabled promotion for an id that has no row.
pub fn all_slots(stored: Vec<Slot>) -> Vec<Slot> {
    SLOT_IDS
        .map(|id| stored.iter().find(|s| s.id == id).cloned().unwrap_or(Slot { id, kind: "promotion".into(), config_json: "{}".into(), ..Slot::default() }))
        .collect()
}

fn missing_table(error: &str) -> bool {
    error.contains("1146") || error.contains("doesn't exist")
}

pub async fn load(session: &SqlSession, database: &str) -> Result<HotelView> {
    let mut view = HotelView::default();
    match session.select(&scene_sql(database), Vec::new()).await {
        Ok(rows) => view.scene = Some(rows.first().map(|r| scene_from(r)).unwrap_or_default()),
        Err(error) if missing_table(&error.to_string()) => view.missing.push("hotelview_landing_settings".into()),
        Err(error) => return Err(error),
    }
    match session.select(&slots_sql(database), Vec::new()).await {
        Ok(rows) => view.slots = all_slots(rows.iter().map(|r| slot_from(r)).collect()),
        Err(error) if missing_table(&error.to_string()) => view.missing.push("hotelview_landing_slots".into()),
        Err(error) => return Err(error),
    }
    match session.select(&vote_counts_sql(database), Vec::new()).await {
        Ok(rows) => view.votes = rows.iter().map(|r| (int(r.first()), int(r.get(1)))).collect(),
        Err(error) if missing_table(&error.to_string()) => view.missing.push("hotelview_landing_votes".into()),
        Err(error) => return Err(error),
    }
    match crate::content_admin::load_rows(session, database, &NEWS, None).await {
        Ok(rows) => view.news = rows,
        Err(error) if missing_table(&error.to_string()) => view.missing.push("hotelview_news".into()),
        Err(error) => return Err(error),
    }
    let params = SETTINGS.iter().map(|(k, _)| SqlValue::Text(k.to_string())).collect();
    let rows = session.select(&settings_sql(database), params).await?;
    view.settings = SETTINGS
        .iter()
        .map(|(key, _)| {
            let row = rows.iter().find(|r| text(r.first()) == *key);
            Setting { key: key.to_string(), value: row.map(|r| text(r.get(1))).unwrap_or_default(), comment: row.map(|r| text(r.get(2))).unwrap_or_default() }
        })
        .collect();
    Ok(view)
}

pub async fn save_scene(session: &SqlSession, database: &str, scene: &Scene) -> Result<()> {
    session.execute(&[(save_scene_sql(database), scene_params(scene))]).await?;
    Ok(())
}

/// Saves a slot whose config already passed [`merge_config`].
pub async fn save_slot(session: &SqlSession, database: &str, slot: &Slot) -> Result<()> {
    session.execute(&[(save_slot_sql(database), slot_params(slot))]).await?;
    Ok(())
}

pub async fn reset_votes(session: &SqlSession, database: &str, slot_id: i64) -> Result<u64> {
    Ok(session.execute(&[(reset_votes_sql(database), vec![SqlValue::Int(slot_id)])]).await?.0)
}

pub async fn save_setting(session: &SqlSession, database: &str, key: &str, value: &str) -> Result<()> {
    let params = vec![SqlValue::Text(key.to_string()), SqlValue::Text(value.trim().to_string())];
    session.execute(&[(save_setting_sql(database), params)]).await?;
    Ok(())
}

// ---- Reload -----------------------------------------------------------------------------------------------------

pub fn online_user_sql(database: &str) -> String {
    format!("SELECT `id`, `username` FROM {}.`users` WHERE `username` = ? AND `online` = '1' LIMIT 1", quote(database))
}

/// The online player called `name`: (id, name), or None when nobody by that name is online.
pub async fn online_user(session: &SqlSession, database: &str, name: &str) -> Result<Option<(i64, String)>> {
    let rows = session.select(&online_user_sql(database), vec![SqlValue::Text(name.trim().to_string())]).await?;
    Ok(rows.first().map(|r| (int(r.first()), text(r.get(1)))))
}

/// The RCON request that runs [`RELOAD_COMMAND`] as the player `user_id`.
pub fn reload_request(user_id: i64) -> String {
    rcon::request("executecommand", serde_json::json!({ "user_id": user_id, "command": RELOAD_COMMAND }))
}

/// Makes the running emulator load the hotel view again, as the online staff player `user_id` (who needs the
/// `cmd_update_hotel_view` permission, and gets the emulator's whisper).
pub async fn reload(server: &dyn Server, user_id: i64) -> Result<Answer> {
    rcon::send(server, &reload_request(user_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> &'static ConfigKey {
        CONFIG_KEYS.iter().find(|k| k.key == name).unwrap()
    }

    #[test]
    fn merging_keeps_unknown_keys_and_drops_runtime_ones() {
        let text = r#"{"voteOptions":[{"id":1,"label":"A"}],"positionX":3,"custom":{"a":1},"userVote":2,"voteCounts":{"1":5}}"#;
        let merged = merge_config(text, &[(key("positionX"), "40".into()), (key("positionY"), " 12 ".into()), (key("catalogPage"), "".into())]).unwrap();
        let value: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["positionX"], 40);
        assert_eq!(value["positionY"], 12);
        assert_eq!(value["voteOptions"][0]["label"], "A");
        assert_eq!(value["custom"]["a"], 1);
        assert!(value.get("userVote").is_none() && value.get("voteCounts").is_none());
        assert!(value.get("catalogPage").is_none());
    }

    #[test]
    fn merging_removes_cleared_keys_and_types_values() {
        let merged = merge_config(r#"{"endsAt":"2026-01-01T00:00:00Z","useServerLTD":true}"#, &[(key("endsAt"), String::new()), (key("useServerLTD"), "0".into()), (key("badgeCode"), "ACH_1".into())]).unwrap();
        let value: Value = serde_json::from_str(&merged).unwrap();
        assert!(value.get("endsAt").is_none());
        assert_eq!(value["useServerLTD"], false);
        assert_eq!(value["badgeCode"], "ACH_1");
        assert!(merge_config("{}", &[(key("featuredSlot"), "two".into())]).is_err());
        assert!(merge_config("{}", &[(key("useServerGoal"), "maybe".into())]).is_err());
        assert_eq!(merge_config("", &[]).unwrap(), "{}");
    }

    #[test]
    fn configs_must_be_json_objects_that_fit() {
        assert!(config_problem("{}").is_none());
        assert!(config_problem("").is_none());
        assert!(config_problem("[1,2]").is_some());
        assert!(config_problem("{\"a\":").is_some());
        let long = format!("{{\"a\":\"{}\"}}", "x".repeat(CONFIG_MAX));
        assert!(config_problem(&long).is_some());
        assert!(merge_config(&long, &[]).is_err());
    }

    #[test]
    fn config_values_read_as_text() {
        let config = parse_config(r#"{"a":"x","n":7,"f":true,"g":false,"z":null}"#).unwrap();
        assert_eq!(config_value(&config, "a"), "x");
        assert_eq!(config_value(&config, "n"), "7");
        assert_eq!(config_value(&config, "f"), "1");
        assert_eq!(config_value(&config, "g"), "0");
        assert_eq!(config_value(&config, "z"), "");
        assert_eq!(config_value(&config, "missing"), "");
        assert_eq!(pretty_config(""), "{}");
        assert_eq!(pretty_config("not json"), "not json");
        assert!(pretty_config(r#"{"a":1}"#).contains("\n"));
    }

    #[test]
    fn keys_follow_the_slot_type() {
        let names = |kind: &str| config_keys_for(kind).iter().map(|k| k.key).collect::<Vec<_>>();
        assert_eq!(names("bonus"), ["positionX", "positionY", "useServerBonusRare"]);
        assert!(names("expiringcatalogpage").contains(&"featuredSlot"));
        assert!(names("promotion").contains(&"secondaryLink"));
    }

    #[test]
    fn slots_are_checked() {
        let slot = Slot { id: 1, enabled: true, kind: "promotion".into(), config_json: "{}".into(), ..Slot::default() };
        assert!(slot_problem(&slot).is_none());
        assert!(slot_problem(&Slot { id: 6, ..slot.clone() }).is_some());
        assert!(slot_problem(&Slot { kind: "unknown".into(), ..slot.clone() }).is_some());
        assert!(slot_problem(&Slot { progress: 101, ..slot.clone() }).is_some());
        assert!(slot_problem(&Slot { title: "x".repeat(101), ..slot.clone() }).is_some());
        assert!(slot_problem(&Slot { config_json: "[]".into(), ..slot.clone() }).is_some());
        let filled = all_slots(vec![Slot { id: 3, title: "kept".into(), ..slot }]);
        assert_eq!(filled.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert_eq!(filled[2].title, "kept");
        assert!(!filled[0].enabled && filled[0].kind == "promotion");
    }

    #[test]
    fn scenes_are_checked_and_read() {
        assert!(scene_problem(&Scene::default()).is_none());
        assert!(scene_problem(&Scene { hall_of_fame_mode: "best".into(), ..Scene::default() }).is_some());
        assert!(scene_problem(&Scene { positions: [-2, 0, 0, 0, 0, 0, 0, 0], ..Scene::default() }).is_some());
        assert!(scene_problem(&Scene { background_url: "x".repeat(513), ..Scene::default() }).is_some());
        let mut row: Vec<SqlValue> = ["bg", "l", "r", "d"].iter().map(|t| SqlValue::Text(t.to_string())).collect();
        row.extend((0..8).map(SqlValue::Int));
        row.extend([SqlValue::Int(1), SqlValue::Text("weird".into()), SqlValue::Int(-3)]);
        let scene = scene_from(&row);
        assert_eq!(scene.positions, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(scene.hall_of_fame_mode, "latest_registered");
        assert_eq!(scene.hall_of_fame_currency_type, 0);
        assert!(scene.hall_of_fame_enabled);
        assert_eq!(scene_params(&scene).len(), 16);
        assert_eq!(save_scene_sql("db").matches('?').count(), 16);
    }

    #[test]
    fn statements_use_parameters() {
        assert_eq!(save_slot_sql("hotel").matches('?').count(), slot_params(&Slot::default()).len());
        assert!(save_slot_sql("ho`tel").starts_with("INSERT INTO `ho``tel`."));
        assert_eq!(settings_sql("db").matches('?').count(), SETTINGS.len());
        assert!(online_user_sql("db").contains("`username` = ?"));
    }

    #[test]
    fn settings_are_checked() {
        assert!(setting_problem("hotelview.promotional.points", "120").is_none());
        assert!(setting_problem("hotelview.promotional.points", "-1").is_some());
        assert!(setting_problem("hotel.communitygoal.halloffame.size", "11").is_some());
        assert!(setting_problem("hotel.communitygoal.concurrentusers.badge", "Concurrent Users").is_some());
        assert!(setting_problem("hotelview.promotional.reward.name", "bonusbag20_2").is_none());
    }

    #[test]
    fn the_reload_runs_the_command_as_the_player() {
        let request: Value = serde_json::from_str(&reload_request(42)).unwrap();
        assert_eq!(request["key"], "executecommand");
        assert_eq!(request["data"]["user_id"], 42);
        assert_eq!(request["data"]["command"], RELOAD_COMMAND);
    }
}
