//! The import of a captured room into a Polaris hotel: a plan (the statements of one transaction, and a report of
//! what they make and leave out) worked out from the capture and what the hotel has, so the page can show it before
//! anything is written. Pure: the caller reads the hotel and runs the statements.
//!
//! What the owner decided (November 2026):
//! * the room is always a new one; access is reset (open, no password or doorbell, the hotel's trade setting); the
//!   category is the one chosen in the preview (one a normal user may pick), the most users and the chat settings
//!   come from the capture, within what the hotel allows;
//! * furni are matched to items_base by class name (by sprite id when the class name is not known) and become
//!   normal items of the chosen owner, cleaned of what belongs to the source hotel: LTD serials, trophy
//!   engravings, group links, ads and badge displays (listed in the report);
//! * valuables (LTD and rares, see [`Valuable`]) are left out unless the user includes them one by one: Polaris has no
//!   per-item "not tradeable" mark (trade checks only items_base.allow_trade and rent), so an included one becomes a
//!   normal, tradeable copy;
//! * a floor plan that is not one of the hotel's models becomes a custom one (room_models_custom, override_model);
//! * the wired settings that were read become Polaris' wired_data, their furni pointing at the new items; boxes of
//!   a kind not converted yet, or not read, come empty (listed);
//! * an [`ImportRecord`] keeps what was made, for Undo.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::capture::{rows, RoomCapture};
use super::packets::{Door, FloorItem, StuffData, WiredSettings};
#[cfg(test)]
use super::packets::{ChatSettings, RoomInfo};

/// A parameter of a planned statement: a value, or what an earlier statement of the plan inserted.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanValue {
    Int(i64),
    Double(f64),
    Text(String),
    /// The id statement N inserted.
    InsertId(usize),
    /// Text in which `{#N}` becomes the id statement N inserted.
    Template(String),
}

/// One row of the hotel's items_base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseItem {
    pub id: u32,
    pub sprite_id: i32,
    pub item_name: String,
    /// `s` floor, `i` wall.
    pub item_type: String,
    pub interaction_type: String,
    /// It can change hands: see [`can_change_hands`].
    pub tradeable: bool,
}

/// Whether a furni of items_base can leave its owner's hands: `allow_trade`, `allow_marketplace_sell` or
/// `allow_recycle` (None: the column is missing or NULL, taken as yes).
pub fn can_change_hands(allow_trade: Option<bool>, allow_marketplace_sell: Option<bool>, allow_recycle: Option<bool>) -> bool {
    [allow_trade, allow_marketplace_sell, allow_recycle].iter().any(|v| v.unwrap_or(true))
}

/// Whether Polaris redeems a furni of this items_base name for currency (RedeemItemEvent: `CF_`, `CFC_` (where the
/// hotel allows it; counted always), `CF_diamond_`, `DF_`, `PF_`; RedeemEmeraldFurniEvent: `nft_emerald...`).
pub fn redeemable(item_name: &str) -> bool {
    ["CF_", "CFC_", "DF_", "PF_"].iter().any(|p| item_name.starts_with(p)) || item_name.starts_with("nft_emerald")
}

/// A catalog_pages row, as far as who can open the page goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPage {
    pub id: i64,
    /// -1 under the root.
    pub parent_id: i64,
    pub enabled: bool,
    pub visible: bool,
    pub min_rank: i64,
    /// `catalog_mode` = 'BUILDER': a Builders Club page, whose furni are lent, not sold.
    pub builder: bool,
}

/// The pages a player (rank 1) reaches from the root: the page and every parent up to the root (-1) enabled, visible,
/// rank 1 and no Builders Club page. A page whose parent is missing, or in a loop, is not reached (Polaris leaves it
/// out too).
pub fn reachable_pages(pages: &[CatalogPage]) -> BTreeSet<i64> {
    let by_id: BTreeMap<i64, &CatalogPage> = pages.iter().map(|p| (p.id, p)).collect();
    let open = |p: &CatalogPage| p.enabled && p.visible && p.min_rank <= 1 && !p.builder;
    pages
        .iter()
        .filter(|page| {
            let mut current = *page;
            for _ in 0..=pages.len() {
                if !open(current) {
                    return false;
                }
                if current.parent_id == -1 {
                    return true;
                }
                match by_id.get(&current.parent_id) {
                    Some(parent) => current = parent,
                    None => return false,
                }
            }
            false
        })
        .map(|p| p.id)
        .collect()
}

/// The query of the catalogue's pages for [`CatalogPage`] (`with_mode`: the table has `catalog_mode`).
pub fn catalog_pages_query(database: &str, with_mode: bool) -> String {
    let mode = if with_mode { "`catalog_mode`" } else { "'NORMAL'" };
    format!("SELECT `id`, `parent_id`, `enabled`, `visible`, `min_rank`, {mode} FROM {}.`catalog_pages`", q(database))
}

/// Whether the pages query failed only because the table has no `catalog_mode` column (MariaDB 1054, "Unknown column
/// 'catalog_mode'"): then the query without it may run. Any other error must stop the import, or Builders Club
/// offers would count as sold.
pub fn missing_catalog_mode(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("catalog_mode") && (error.contains("1054") || error.contains("unknown column"))
}

/// The query of the catalogue's offers for [`Catalogue::new`]: page, item_ids and limited_stack.
pub fn catalog_offers_query(database: &str) -> String {
    format!("SELECT `page_id`, `item_ids`, `limited_stack` FROM {}.`catalog_items`", q(database))
}

/// What the hotel's catalogue sells, by items_base id, counting only offers on pages players reach
/// ([`reachable_pages`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalogue {
    /// In an offer that is no LTD.
    pub sold: BTreeSet<u32>,
    /// In an LTD offer (`limited_stack` > 0).
    pub sold_limited: BTreeSet<u32>,
}

impl Catalogue {
    /// From the pages and the offers (page id, `catalog_items.item_ids`: `id` or `id:amount` separated by `;`, and
    /// `limited_stack`). An offer on a page players do not reach adds nothing, nor does a bundle that names other
    /// offers (`@offer`): what it hands out is not counted as sold.
    pub fn new<'a>(pages: &[CatalogPage], offers: impl IntoIterator<Item = (i64, &'a str, i64)>) -> Self {
        let reachable = reachable_pages(pages);
        let mut catalogue = Self::default();
        for (page, item_ids, limited_stack) in offers {
            if !reachable.contains(&page) {
                continue;
            }
            let set = if limited_stack > 0 { &mut catalogue.sold_limited } else { &mut catalogue.sold };
            set.extend(item_ids.split(';').filter_map(|part| part.split(':').next()?.trim().parse::<u32>().ok()));
        }
        catalogue
    }
}

/// Why a furni counts as a valuable, which the import leaves out unless the user includes it. The rule, conservative
/// (when unsure, a valuable): an LTD of the other hotel, and a furni Polaris redeems for currency, always; otherwise
/// only a furni that can change hands here ([`BaseItem::tradeable`]) and that the other hotel's furnidata marks rare,
/// or that the hotel's catalogue sells only as LTD, or does not sell at all (a name counts as sold when any items_base
/// row of that name and type is; a colour variant `name*n` is a name of its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Valuable {
    /// An LTD in the capture: its serial and series size.
    Limited { serial: i32, size: i32 },
    /// Polaris redeems it for currency (credits, pixels, points, diamonds, emeralds).
    Currency,
    /// The other hotel's furnidata marks it rare.
    Rare,
    /// The hotel's catalogue sells it only as LTD.
    SoldAsLimited,
    /// The hotel's catalogue does not sell it.
    NotSold,
}

/// A valuable of the capture and whether the user included it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValuableItem {
    /// `s` floor or `i` wall and the furni's id in the capture, as [`ImportChoice::include`] names it.
    pub key: (char, i32),
    /// Its items_base item_name.
    pub name: String,
    pub why: Valuable,
    pub included: bool,
}

/// What the import needs to know of the hotel.
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub database: String,
    pub items_base: Vec<BaseItem>,
    /// The columns of `rooms` and `items` (empty: take the Polaris columns as there).
    pub room_columns: BTreeSet<String>,
    pub item_columns: BTreeSet<String>,
    /// The longest text a column takes, by "table.column".
    pub lengths: BTreeMap<String, usize>,
    /// The hotel's model of the captured model name, when it has one: its heightmap and door.
    pub model: Option<(String, Door)>,
    /// The hotel's catalogue (None: not read; then only LTD and furnidata rares are told apart).
    pub catalogue: Option<Catalogue>,
    /// The room categories a normal user may pick ([`room_categories_query`]), in the navigator's order.
    pub categories: Vec<RoomCategory>,
}

/// A room category of the navigator (`navigator_flatcats`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomCategory {
    pub id: i64,
    pub caption: String,
    /// The most users a room of this category may take (0: no limit of its own).
    pub max_users: i64,
}

/// The categories a normal user (rank 1) may give a room, in the navigator's order: not the staff ("public") ones.
pub fn room_categories_query(database: &str) -> String {
    format!(
        "SELECT `id`, `caption`, `max_user_count` FROM {}.`navigator_flatcats` WHERE `min_rank` <= 1 AND `public` = '0' ORDER BY `order_num`, `id`",
        q(database)
    )
}

/// The most users Polaris lets a room's settings have (RoomSettingsSaveEvent).
const MAX_USERS: i64 = 200;

/// What the user chose for the new room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportChoice {
    pub owner_id: i64,
    pub owner_name: String,
    pub room_name: String,
    pub door: Door,
    /// Seconds since 1970, for date_created.
    pub now: i64,
    /// The valuables to import all the same, by [`ValuableItem::key`].
    pub include: BTreeSet<(char, i32)>,
    /// The category chosen (None, or one the hotel does not offer: the first of [`Target::categories`]).
    pub category: Option<i64>,
}

/// What the import makes and leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub floor_items: usize,
    pub wall_items: usize,
    /// Furni the hotel's items_base lacks: class name (or sprite id) and how many.
    pub missing: Vec<(String, usize)>,
    /// Furni matched by sprite id because their class name is not known.
    pub matched_by_sprite: usize,
    pub wired_converted: usize,
    /// Wired boxes imported without their settings, and why.
    pub wired_empty: Vec<String>,
    /// What was cleaned off (LTD serials, engravings, group links, ads).
    pub cleaned: Vec<String>,
    /// Model used, or "custom" for a floor plan of its own.
    pub model: String,
    pub custom_model: bool,
    pub notes: Vec<String>,
    /// The valuables of the capture (LTD and rares), left out unless included.
    pub valuables: Vec<ValuableItem>,
    /// The categories to choose from, and the one the room gets (None: the hotel's default).
    pub categories: Vec<RoomCategory>,
    pub category: Option<i64>,
}

/// The statements and their report.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub statements: Vec<(String, Vec<PlanValue>)>,
    pub report: ImportReport,
    /// The statement that inserts each source furni (floor and wall), by source id.
    pub item_statements: Vec<usize>,
}

/// What an import made, kept for Undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRecord {
    /// The server (the Manager's login) and database it went to.
    pub server: String,
    pub database: String,
    pub room_id: u64,
    pub room_name: String,
    pub owner_id: i64,
    pub owner_name: String,
    pub item_ids: Vec<u64>,
    pub custom_model: bool,
    /// Where it came from: hotel and room id.
    pub source: String,
    pub imported_at: String,
}

fn q(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "``"))
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The heightmap as Polaris stores it: rows separated by `\r`, all as wide as the widest (padded with `x`).
pub fn normalized_heightmap(heightmap: &str) -> String {
    let rows = rows(heightmap);
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    rows.iter()
        .map(|r| {
            let mut row: String = r.iter().map(|c| if c.eq_ignore_ascii_case(&'x') { 'x' } else { c.to_ascii_lowercase() }).collect();
            row.extend(std::iter::repeat_n('x', width - r.len()));
            row
        })
        .collect::<Vec<_>>()
        .join("\r")
}

/// Whether the door is an open tile of the heightmap.
pub fn door_is_open(heightmap: &str, door: Door) -> bool {
    let rows = rows(heightmap);
    usize::try_from(door.y)
        .ok()
        .and_then(|y| rows.get(y))
        .and_then(|row| usize::try_from(door.x).ok().and_then(|x| row.get(x)))
        .is_some_and(|c| c.is_ascii_alphanumeric() && !c.eq_ignore_ascii_case(&'x'))
}

/// The extra_data a furni gets, and what was cleaned off (None: nothing).
fn clean_extra(item: &FloorItem, base: &BaseItem) -> (String, Option<String>) {
    let interaction = base.interaction_type.to_ascii_lowercase();
    let class = base.item_name.to_ascii_lowercase();
    if interaction == "trophy" {
        let had = !item.stuff.state().trim().is_empty();
        return (String::new(), had.then(|| format!("{}: engraving removed", base.item_name)));
    }
    if interaction == "badge_display" || interaction.contains("roomads") || interaction.starts_with("ads_") || class.starts_with("ads_") {
        return (String::new(), Some(format!("{}: its text points at the other hotel, removed", base.item_name)));
    }
    if interaction.contains("guild") || class.starts_with("gld_") {
        let state = item.stuff.state();
        return (state, Some(format!("{}: group link removed", base.item_name)));
    }
    let data = match &item.stuff {
        // Mannequin: gender:figure:name.
        StuffData::Map { entries } if entries.iter().any(|(k, _)| k == "FIGURE") => {
            let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
            format!("{}:{}:{}", get("GENDER"), get("FIGURE"), get("OUTFIT_NAME"))
        }
        // Background toner: a:b:c:d.
        StuffData::Ints { values } => values.iter().map(i32::to_string).collect::<Vec<_>>().join(":"),
        other => other.state(),
    };
    (data, None)
}

/// Why `base` (the match of a furni of the capture) is a valuable, if it is: see [`Valuable`].
/// `sold`: whether the catalogue sells the name (as a normal offer, as an LTD), None when it was not read.
fn valuable(limited: Option<super::packets::Limited>, base: &BaseItem, capture: &RoomCapture, sold: Option<(bool, bool)>) -> Option<Valuable> {
    if let Some(l) = limited {
        return Some(Valuable::Limited { serial: l.serial, size: l.size });
    }
    if redeemable(&base.item_name) {
        return Some(Valuable::Currency);
    }
    if !base.tradeable {
        return None;
    }
    if capture.rare_classes.contains(&base.item_name.to_ascii_lowercase()) {
        return Some(Valuable::Rare);
    }
    match sold? {
        (true, _) => None,
        (false, true) => Some(Valuable::SoldAsLimited),
        (false, false) => Some(Valuable::NotSold),
    }
}

/// A placeholder for an item id in wired JSON, replaced by `{#N}` after serializing.
fn id_marker(statement: usize) -> Value {
    Value::String(format!("@@ID:{statement}@@"))
}

fn with_ids(json: &Value) -> String {
    let text = json.to_string();
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find("\"@@ID:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 6..];
        match after.find("@@\"") {
            Some(end) => {
                out.push_str(&format!("{{#{}}}", &after[..end]));
                rest = &after[end + 3..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The wired_data of a box (Polaris' Gson JSON of the box class), or why it comes empty.
fn wired_json(interaction: &str, s: &WiredSettings, capture: &RoomCapture, statement_of: &BTreeMap<i32, usize>) -> std::result::Result<Value, String> {
    let ip = |n: usize| s.int_params.get(n).copied().unwrap_or(0);
    let item_ids: Vec<Value> = s.stuff_ids.iter().filter_map(|id| statement_of.get(id).map(|st| id_marker(*st))).collect();
    let furni_source = s.furni_sources.first().copied().unwrap_or(if s.stuff_ids.is_empty() { 0 } else { 100 });
    let user_source = s.user_sources.first().copied().unwrap_or(0);
    let delay = s.delay.clamp(0, 120);
    let snapshot = || -> Vec<Value> {
        s.stuff_ids
            .iter()
            .filter_map(|id| {
                let statement = statement_of.get(id)?;
                let item = capture.floor_items.get(id)?;
                Some(json!({"item_id": id_marker(*statement), "state": item.stuff.state(), "rotation": item.dir, "x": item.x, "y": item.y, "z": item.z}))
            })
            .collect()
    };
    Ok(match interaction {
        "wf_trg_walks_on_furni" | "wf_trg_walks_off_furni" => json!({"furniSource": furni_source, "itemIds": item_ids}),
        "wf_trg_enter_room" => json!({"username": s.string_param}),
        "wf_trg_says_something" => {
            let mode = ip(1);
            json!({"hideMessage": ip(2) == 1, "ownerOnly": ip(0) == 1, "key": if mode == 2 { String::new() } else { s.string_param.clone() }, "matchMode": mode})
        }
        "wf_trg_periodically" => json!({"repeatTime": (ip(0) * 500).max(500)}),
        "wf_trg_at_given_time" => json!({"executeTime": ip(0) * 500}),
        "wf_act_toggle_state" => json!({"delay": delay, "itemIds": item_ids, "toggleType": ip(0), "furniSource": furni_source}),
        "wf_act_move_rotate" => json!({"direction": ip(0), "rotation": ip(1), "delay": delay, "itemIds": item_ids, "furniSource": furni_source}),
        "wf_act_show_message" => json!({
            "message": s.string_param, "delay": delay, "userSource": user_source,
            "visibilitySelection": if ip(0) == 1 { 1 } else { 0 }, "bubbleStyle": ip(1), "bubbleWidthOverride": ip(2)
        }),
        "wf_act_teleport_to" => json!({"delay": delay, "itemIds": item_ids, "fastTeleport": ip(0) == 1, "furniSource": furni_source, "userSource": user_source}),
        "wf_act_match_to_sshot" => json!({
            "state": ip(0) == 1, "direction": ip(1) == 1, "position": ip(2) == 1, "altitude": ip(3) == 1,
            "items": snapshot(), "delay": delay, "furniSource": furni_source
        }),
        "wf_cnd_match_snapshot" => json!({
            "state": ip(0) == 1, "direction": ip(1) == 1, "position": ip(2) == 1, "altitude": ip(3) == 1,
            "settings": snapshot(), "furniSource": furni_source, "quantifier": s.quantifier
        }),
        "wf_cnd_furnis_hv_avtrs" => json!({"itemIds": item_ids, "furniSource": furni_source, "all": ip(0) == 1}),
        "wf_cnd_trggrer_on_frn" => json!({"itemIds": item_ids, "furniSource": furni_source, "userSource": user_source, "quantifier": s.quantifier}),
        "wf_act_reset_timers" => json!({"delay": delay}),
        "wf_act_give_score" => {
            let score = ip(0).abs();
            if !(1..=1000).contains(&score) {
                return Err("its score is not one Polaris takes".into());
            }
            json!({"score": score, "operation": if ip(0) < 0 { 1 } else { 0 }, "delay": delay, "userSource": user_source, "timesPerGame": ip(1).max(0)})
        }
        "wf_act_call_stacks" => json!({"delay": delay, "itemIds": item_ids, "furniSource": furni_source}),
        _ => return Err("this kind of box is not converted yet".into()),
    })
}

/// The plan of an import.
pub fn plan(capture: &RoomCapture, target: &Target, choice: &ImportChoice) -> Result<ImportPlan> {
    if capture.heightmap.trim().is_empty() {
        bail!("The capture has no floor plan: enter the room again while capturing.");
    }
    if choice.owner_id <= 0 || choice.owner_name.trim().is_empty() {
        bail!("Choose the new room's owner: a user of the hotel.");
    }
    let heightmap = normalized_heightmap(&capture.heightmap);
    if !door_is_open(&heightmap, choice.door) {
        bail!("The door ({}, {}) is not an open tile of the floor plan.", choice.door.x, choice.door.y);
    }
    let db = q(&target.database);
    let has = |columns: &BTreeSet<String>, column: &str| columns.is_empty() || columns.contains(column);
    let length = |key: &str, default: usize| target.lengths.get(key).copied().unwrap_or(default);
    let mut report = ImportReport::default();
    let mut statements: Vec<(String, Vec<PlanValue>)> = Vec::new();

    // The model: the hotel's own when it has this one with the same floor plan and door, else a custom one.
    let standard = target
        .model
        .as_ref()
        .filter(|(map, door)| normalized_heightmap(map) == heightmap && *door == choice.door && capture.model_name.chars().count() <= length("rooms.model", 20));
    report.custom_model = standard.is_none();
    report.model = if report.custom_model { "custom".into() } else { capture.model_name.clone() };

    // The room.
    let info = capture.info.clone().unwrap_or_default();
    let mut columns: Vec<(&str, PlanValue)> = vec![
        ("owner_id", PlanValue::Int(choice.owner_id)),
        ("owner_name", PlanValue::Text(cut(&choice.owner_name, length("rooms.owner_name", 25)))),
        ("name", PlanValue::Text(cut(choice.room_name.trim(), length("rooms.name", 50)))),
        ("description", PlanValue::Text(cut(&info.description, length("rooms.description", 512)))),
        ("model", PlanValue::Text(if report.custom_model { "model_a".into() } else { capture.model_name.clone() })),
        ("state", PlanValue::Text("open".into())),
        ("password", PlanValue::Text(String::new())),
        ("tags", PlanValue::Text(cut(&info.tags.join(";"), length("rooms.tags", 500)))),
        ("override_model", PlanValue::Text(if report.custom_model { "1" } else { "0" }.into())),
    ];
    for (column, value) in [("paper_floor", &capture.floor), ("paper_wall", &capture.wallpaper), ("paper_landscape", &capture.landscape)] {
        if value.chars().count() <= length(&format!("rooms.{column}"), 5) {
            columns.push((column, PlanValue::Text(value.clone())));
        } else {
            report.notes.push(format!("{column} {value} does not fit the hotel's column: the default is used."));
        }
    }
    if let Some((hide_walls, wall, floor)) = capture.visualization {
        columns.push(("allow_hidewall", PlanValue::Text(if hide_walls { "1" } else { "0" }.into())));
        columns.push(("thickness_wall", PlanValue::Int(wall.clamp(-2, 1).into())));
        columns.push(("thickness_floor", PlanValue::Int(floor.clamp(-2, 1).into())));
    }
    columns.push(("wall_height", PlanValue::Int(capture.wall_height.clamp(-1, 15).into())));
    // The category chosen, or the first one; the most users within what it and Polaris allow.
    let category = choice.category.and_then(|id| target.categories.iter().find(|c| c.id == id)).or(target.categories.first());
    report.categories = target.categories.clone();
    report.category = category.map(|c| c.id);
    match category {
        Some(category) => columns.push(("category", PlanValue::Int(category.id))),
        None => report.notes.push("The hotel has no room category a user may pick: the room gets the default one.".into()),
    }
    if info.max_users > 0 {
        let most = category.map(|c| c.max_users).filter(|m| *m > 0).map_or(MAX_USERS, |m| m.min(MAX_USERS));
        columns.push(("users_max", PlanValue::Int(i64::from(info.max_users).clamp(1, most))));
    }
    // The chat settings, within the ranges Polaris accepts when a room's settings are saved.
    if let Some(chat) = info.chat {
        columns.push(("chat_mode", PlanValue::Int(chat.mode.clamp(0, 2).into())));
        columns.push(("chat_weight", PlanValue::Int(chat.weight.clamp(0, 2).into())));
        columns.push(("chat_speed", PlanValue::Int(chat.speed.clamp(0, 2).into())));
        columns.push(("chat_hearing_distance", PlanValue::Int(chat.distance.clamp(1, 99).into())));
        columns.push(("chat_protection", PlanValue::Int(chat.protection.clamp(0, 2).into())));
    }
    columns.push(("date_created", PlanValue::Int(choice.now)));
    columns.retain(|(column, _)| has(&target.room_columns, column));
    statements.push((
        format!(
            "INSERT INTO {db}.`rooms` ({}) VALUES ({})",
            columns.iter().map(|(c, _)| q(c)).collect::<Vec<_>>().join(", "),
            vec!["?"; columns.len()].join(", ")
        ),
        columns.into_iter().map(|(_, v)| v).collect(),
    ));
    if report.custom_model {
        statements.push((
            format!("INSERT INTO {db}.`room_models_custom` (`id`, `name`, `door_x`, `door_y`, `door_dir`, `heightmap`) VALUES (?, ?, ?, ?, ?, ?)"),
            vec![
                PlanValue::InsertId(0),
                PlanValue::Template("custom_{#0}".into()),
                PlanValue::Int(choice.door.x.into()),
                PlanValue::Int(choice.door.y.into()),
                PlanValue::Int(choice.door.dir.into()),
                PlanValue::Text(heightmap.clone()),
            ],
        ));
        statements.push((format!("UPDATE {db}.`rooms` SET `model` = ? WHERE `id` = ?"), vec![PlanValue::Template("custom_{#0}".into()), PlanValue::InsertId(0)]));
    }

    // items_base by class name and type, and by sprite id.
    let mut by_name: BTreeMap<(String, String), &BaseItem> = BTreeMap::new();
    let mut by_sprite: BTreeMap<(i32, String), &BaseItem> = BTreeMap::new();
    for base in &target.items_base {
        let kind = base.item_type.to_ascii_lowercase();
        by_name.entry((base.item_name.to_ascii_lowercase(), kind.clone())).or_insert(base);
        by_sprite.entry((base.sprite_id, kind)).or_insert(base);
    }
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    let mut find = |class: &Option<String>, type_id: i32, kind: &str, report: &mut ImportReport| -> Option<&BaseItem> {
        match class {
            Some(class) => {
                let found = by_name.get(&(class.to_ascii_lowercase(), kind.to_string())).copied();
                if found.is_none() {
                    *missing.entry(class.clone()).or_default() += 1;
                }
                found
            }
            None => {
                let found = by_sprite.get(&(type_id, kind.to_string())).copied();
                match found {
                    Some(_) => report.matched_by_sprite += 1,
                    None => *missing.entry(format!("sprite {type_id}")).or_default() += 1,
                }
                found
            }
        }
    };

    let item_columns: Vec<&str> = ["user_id", "room_id", "item_id", "wall_pos", "x", "y", "z", "rot", "extra_data", "limited_data", "guild_id"]
        .into_iter()
        .filter(|c| has(&target.item_columns, c))
        .collect();
    let insert_item = format!(
        "INSERT INTO {db}.`items` ({}) VALUES ({})",
        item_columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "),
        vec!["?"; item_columns.len()].join(", ")
    );
    let extra_max = length("items.extra_data", 2096);
    let wall_max = length("items.wall_pos", 20);
    let mut statement_of: BTreeMap<i32, usize> = BTreeMap::new();
    let mut base_of: BTreeMap<i32, &BaseItem> = BTreeMap::new();
    let mut item_statements = Vec::new();
    let push_item = |values: BTreeMap<&str, PlanValue>, statements: &mut Vec<(String, Vec<PlanValue>)>| -> usize {
        statements.push((insert_item.clone(), item_columns.iter().map(|c| values.get(c).cloned().unwrap_or(PlanValue::Int(0))).collect()));
        statements.len() - 1
    };

    // A valuable is listed, and left out unless the user included it.
    // Sold (normal, LTD) by items_base name and type: a name is sold when any row of it is.
    let mut sold_names: BTreeMap<(String, String), (bool, bool)> = BTreeMap::new();
    if let Some(catalogue) = &target.catalogue {
        for base in &target.items_base {
            let entry = sold_names.entry((base.item_name.to_ascii_lowercase(), base.item_type.to_ascii_lowercase())).or_default();
            entry.0 |= catalogue.sold.contains(&base.id);
            entry.1 |= catalogue.sold_limited.contains(&base.id);
        }
    }
    let keep = |key: (char, i32), limited, base: &BaseItem, report: &mut ImportReport| -> bool {
        let sold = target
            .catalogue
            .as_ref()
            .map(|_| sold_names.get(&(base.item_name.to_ascii_lowercase(), base.item_type.to_ascii_lowercase())).copied().unwrap_or_default());
        let Some(why) = valuable(limited, base, capture, sold) else { return true };
        let included = choice.include.contains(&key);
        report.valuables.push(ValuableItem { key, name: base.item_name.clone(), why, included });
        included
    };
    if target.catalogue.is_none() {
        report.notes.push("The hotel's catalogue was not read: only LTD, currency furni and the furnidata's rares are told apart as valuables.".into());
    }
    if !capture.rares_checked {
        report.notes.push(
            "The capture does not say which furni the other hotel calls rare (captured without its furnidata, or before this was recorded): those are told apart only by your catalogue.".into(),
        );
    }

    for item in capture.floor_items.values() {
        let Some(base) = find(&item.class_name, item.type_id, "s", &mut report) else { continue };
        if !keep(('s', item.id), item.limited, base, &mut report) {
            continue;
        }
        let (extra, cleaned) = clean_extra(item, base);
        if let Some(cleaned) = cleaned {
            report.cleaned.push(cleaned);
        }
        if let Some(limited) = item.limited {
            report.cleaned.push(format!("{}: LTD {} of {} becomes a normal item", base.item_name, limited.serial, limited.size));
        }
        let values = BTreeMap::from([
            ("user_id", PlanValue::Int(choice.owner_id)),
            ("room_id", PlanValue::InsertId(0)),
            ("item_id", PlanValue::Int(base.id.into())),
            ("wall_pos", PlanValue::Text(String::new())),
            ("x", PlanValue::Int(item.x.into())),
            ("y", PlanValue::Int(item.y.into())),
            ("z", PlanValue::Double(item.z)),
            ("rot", PlanValue::Int(item.dir.into())),
            ("extra_data", PlanValue::Text(cut(&extra, extra_max))),
            ("limited_data", PlanValue::Text("0:0".into())),
            ("guild_id", PlanValue::Int(0)),
        ]);
        let statement = push_item(values, &mut statements);
        statement_of.insert(item.id, statement);
        base_of.insert(item.id, base);
        item_statements.push(statement);
        report.floor_items += 1;
    }
    for item in capture.wall_items.values() {
        let Some(base) = find(&item.class_name, item.type_id, "i", &mut report) else { continue };
        if !keep(('i', item.id), None, base, &mut report) {
            continue;
        }
        if item.location.chars().count() > wall_max {
            report.notes.push(format!("{}: its wall position {} does not fit the hotel's column, left out.", base.item_name, item.location));
            continue;
        }
        let values = BTreeMap::from([
            ("user_id", PlanValue::Int(choice.owner_id)),
            ("room_id", PlanValue::InsertId(0)),
            ("item_id", PlanValue::Int(base.id.into())),
            ("wall_pos", PlanValue::Text(item.location.clone())),
            ("extra_data", PlanValue::Text(cut(&item.data, extra_max))),
            ("limited_data", PlanValue::Text("0:0".into())),
        ]);
        let statement = push_item(values, &mut statements);
        item_statements.push(statement);
        report.wall_items += 1;
    }
    report.missing = missing.into_iter().collect();

    // The wired, once every furni it points at has its statement.
    if has(&target.item_columns, "wired_data") {
        for (id, base) in &base_of {
            let interaction = base.interaction_type.to_ascii_lowercase();
            if !interaction.starts_with("wf_") {
                continue;
            }
            let Some(settings) = capture.wired.get(id) else {
                report.wired_empty.push(format!("{}: its settings were not read", base.item_name));
                continue;
            };
            match wired_json(&interaction, settings, capture, &statement_of) {
                Ok(json) => {
                    let lost = settings.stuff_ids.iter().filter(|s| !statement_of.contains_key(s)).count();
                    if lost > 0 {
                        report.notes.push(format!("{}: {lost} of its furni are not imported, so it points at fewer.", base.item_name));
                    }
                    statements.push((
                        format!("UPDATE {db}.`items` SET `wired_data` = ? WHERE `id` = ?"),
                        vec![PlanValue::Template(with_ids(&json)), PlanValue::InsertId(statement_of[id])],
                    ));
                    report.wired_converted += 1;
                }
                Err(why) => report.wired_empty.push(format!("{}: {why}", base.item_name)),
            }
        }
    }
    if !capture.door_from_hotel {
        report.notes.push("The hotel did not send the door tile: it was guessed from the floor plan; check it.".into());
    }
    Ok(ImportPlan { statements, report, item_statements })
}

/// An imported room as the hotel has it now, read before Undo: `rooms.owner_id`, `rooms.users` (who is inside, kept
/// by Polaris while it runs and set to 0 when it starts), `rooms.guild_id` (a group whose home it is), and whether its
/// owner is online (`users.online` = '1'; Polaris keeps a room loaded while its owner is online).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomNow {
    pub owner_id: i64,
    pub users: i64,
    pub guild_id: i64,
    pub owner_online: bool,
}

/// A furni of an import as the hotel has it now: its owner (`items.user_id`) and room (`items.room_id`, 0 in an
/// inventory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemNow {
    pub id: u64,
    pub user_id: i64,
    pub room_id: i64,
}

/// The queries that read the room of `record` (owner, users, group, owner online) and its furni (id, owner, room,
/// 500 ids a query) for [`undo_problems`].
pub fn undo_check_queries(record: &ImportRecord) -> ((String, Vec<PlanValue>), Vec<(String, Vec<PlanValue>)>) {
    let db = q(&record.database);
    let room = (
        format!(
            "SELECT r.`owner_id`, r.`users`, r.`guild_id`, COALESCE(u.`online` = '1', 0) FROM {db}.`rooms` r LEFT JOIN {db}.`users` u ON u.`id` = r.`owner_id` WHERE r.`id` = ? LIMIT 1"
        ),
        vec![PlanValue::Int(record.room_id as i64)],
    );
    let items = record
        .item_ids
        .chunks(500)
        .map(|chunk| {
            let marks = vec!["?"; chunk.len()].join(", ");
            (format!("SELECT `id`, `user_id`, `room_id` FROM {db}.`items` WHERE `id` IN ({marks})"), chunk.iter().map(|id| PlanValue::Int(*id as i64)).collect())
        })
        .collect();
    (room, items)
}

/// Why Undo must not run now, one line per kind with the furni concerned (empty: it may). Undo deletes only a room
/// nobody is in, whose owner is offline, no group's home, still of the owner it was imported for, with every furni
/// that came with it still in it and still that owner's: a furni picked up, moved, traded or given since belongs to
/// someone's game now. A furni no longer in the hotel (deleted or recycled) is nobody's: it does not stop Undo
/// ([`undo_already_gone`] lists it).
pub fn undo_problems(record: &ImportRecord, room: Option<&RoomNow>, items: &[ItemNow]) -> Vec<String> {
    let Some(room) = room else {
        return vec![format!("Room {} is no longer in the hotel.", record.room_id)];
    };
    let mut problems = Vec::new();
    if room.owner_id != record.owner_id {
        problems.push(format!("Room {} belongs to user {} now, not to {}.", record.room_id, room.owner_id, record.owner_name));
    }
    if room.users > 0 {
        problems.push(format!(
            "{} users are in the room now (or the emulator stopped without clearing the count: start it once): try again when it is empty.",
            room.users
        ));
    }
    if room.owner_online {
        problems.push(format!(
            "{owner} is online, and Polaris keeps the room loaded while its owner is: ask {owner} to log out, then try again.",
            owner = record.owner_name
        ));
    }
    if room.guild_id > 0 {
        problems.push(format!("Group {} has the room as its home: remove the group first.", room.guild_id));
    }
    let (mut picked_up, mut moved, mut traded) = (Vec::new(), Vec::new(), Vec::new());
    for id in &record.item_ids {
        let Some(item) = items.iter().find(|i| i.id == *id) else { continue };
        if item.user_id != record.owner_id {
            traded.push(format!("{id} (user {})", item.user_id));
        } else if item.room_id == 0 {
            picked_up.push(id.to_string());
        } else if item.room_id != record.room_id as i64 {
            moved.push(format!("{id} (room {})", item.room_id));
        }
    }
    let listed = |ids: &[String]| {
        let mut text = ids.iter().take(30).cloned().collect::<Vec<_>>().join(", ");
        if ids.len() > 30 {
            text.push_str(&format!(" and {} more", ids.len() - 30));
        }
        text
    };
    for (ids, what) in [
        (&picked_up, format!("Picked up since (in {}'s inventory)", record.owner_name)),
        (&moved, "Moved to another room".to_string()),
        (&traded, "Owned by another user now (traded or given)".to_string()),
    ] {
        if !ids.is_empty() {
            problems.push(format!("{what}: {}", listed(ids)));
        }
    }
    problems
}

/// The furni of the import no longer in the hotel (deleted or recycled since): Undo skips them.
pub fn undo_already_gone(record: &ImportRecord, items: &[ItemNow]) -> Vec<u64> {
    record.item_ids.iter().copied().filter(|id| !items.iter().any(|i| i.id == *id)).collect()
}

/// The statement of [`undo_statements`] that must change a row, else the whole transaction is rolled back: the room
/// delete, which holds only while the room is still the owner's and nobody is in it.
pub const UNDO_ROOM_STATEMENT: usize = 0;

/// The statements that undo an import, once [`undo_problems`] found nothing, in one transaction. First the room goes,
/// only while it is still the owner's and empty ([`UNDO_ROOM_STATEMENT`]); then the furni that came with it are
/// deleted (only while they are in the room and the owner's); furni, bots and pets placed in the room by others go
/// back to their owners' inventories; and the room's rights, votes and word filter go, as Polaris' RoomDeleter does.
/// The room goes first because users_pets, room_rights, room_votes and room_wordfilter are MyISAM tables a rollback
/// cannot undo. The custom floor plan goes only after the commit ([`undo_model_statement`]).
pub fn undo_statements(record: &ImportRecord) -> Vec<(String, Vec<PlanValue>)> {
    let db = q(&record.database);
    let room = PlanValue::Int(record.room_id as i64);
    let mut statements = vec![(format!("DELETE FROM {db}.`rooms` WHERE `id` = ? AND `owner_id` = ? AND `users` = 0"), vec![room.clone(), PlanValue::Int(record.owner_id)])];
    for chunk in record.item_ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let mut values: Vec<PlanValue> = chunk.iter().map(|id| PlanValue::Int(*id as i64)).collect();
        values.extend([room.clone(), PlanValue::Int(record.owner_id)]);
        statements.push((format!("DELETE FROM {db}.`items` WHERE `id` IN ({marks}) AND `room_id` = ? AND `user_id` = ?"), values));
    }
    for table in ["items", "bots", "users_pets"] {
        statements.push((format!("UPDATE {db}.`{table}` SET `room_id` = 0 WHERE `room_id` = ?"), vec![room.clone()]));
    }
    for table in ["room_rights", "room_votes", "room_wordfilter"] {
        statements.push((format!("DELETE FROM {db}.`{table}` WHERE `room_id` = ?"), vec![room.clone()]));
    }
    statements
}

/// The statement that drops the room's custom floor plan (None: it had none), run only once the transaction of
/// [`undo_statements`] is committed: room_models_custom is MyISAM, so dropped inside a transaction that then fails
/// it would leave the room without its floor plan.
pub fn undo_model_statement(record: &ImportRecord) -> Option<(String, Vec<PlanValue>)> {
    record.custom_model.then(|| (format!("DELETE FROM {}.`room_models_custom` WHERE `id` = ?", q(&record.database)), vec![PlanValue::Int(record.room_id as i64)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::packets::{Limited, WallItem, WiredKind};

    fn floor(id: i32, class: &str, x: i32, stuff: StuffData) -> FloorItem {
        FloorItem { id, type_id: id + 1000, class_name: Some(class.into()), x, y: 1, z: 0.0, dir: 2, stuff, limited: None, owner_id: 5 }
    }

    fn base(id: u32, name: &str, kind: &str, interaction: &str) -> BaseItem {
        BaseItem { id, sprite_id: id as i32, item_name: name.into(), item_type: kind.into(), interaction_type: interaction.into(), tradeable: true }
    }

    fn capture() -> RoomCapture {
        let mut c = RoomCapture { room_id: 9, model_name: "model_x".into(), heightmap: "xxx\r000\rx00".into(), door_from_hotel: true, ..RoomCapture::default() };
        c.floor_items.insert(1, floor(1, "chair", 1, StuffData::Legacy { data: "1".into() }));
        let mut ltd = floor(2, "rare_dragon", 2, StuffData::Legacy { data: "0".into() });
        ltd.limited = Some(Limited { serial: 3, size: 50 });
        c.floor_items.insert(2, ltd);
        c.floor_items.insert(3, floor(3, "wf_act_toggle_state", 1, StuffData::Legacy { data: "0".into() }));
        c.floor_items.insert(4, floor(4, "trophy_gold", 2, StuffData::Legacy { data: "bob\t1-1-2020\tWell done".into() }));
        c.floor_items.insert(5, floor(5, "not_in_hotel", 2, StuffData::Empty));
        c.wall_items.insert(7, WallItem { id: 7, type_id: 4000, class_name: Some("poster".into()), location: ":w=1,2 l=3,4 r".into(), data: "5".into(), owner_id: 5 });
        c.wired.insert(
            3,
            WiredSettings { kind: Some(WiredKind::Action), box_id: 3, stuff_ids: vec![1, 5], int_params: vec![0], delay: 2, ..WiredSettings::default() },
        );
        c
    }

    fn target() -> Target {
        Target {
            database: "habbo".into(),
            items_base: vec![
                base(11, "chair", "s", "default"),
                base(12, "rare_dragon", "s", "default"),
                base(13, "wf_act_toggle_state", "s", "wf_act_toggle_state"),
                base(14, "trophy_gold", "s", "trophy"),
                base(15, "poster", "i", "default"),
            ],
            ..Target::default()
        }
    }

    fn choice() -> ImportChoice {
        ImportChoice {
            owner_id: 1,
            owner_name: "admin".into(),
            room_name: "Copy".into(),
            door: Door { x: 0, y: 1, dir: 2 },
            now: 1_700_000_000,
            include: BTreeSet::from([('s', 2)]),
            category: None,
        }
    }

    /// The value the room's INSERT gives a column.
    fn room_value(plan: &ImportPlan, column: &str) -> Option<PlanValue> {
        let (sql, values) = &plan.statements[0];
        let list = &sql[sql.find('(')? + 1..sql.find(')')?];
        list.split(", ").position(|c| c == format!("`{column}`")).map(|i| values[i].clone())
    }

    fn categories() -> Vec<RoomCategory> {
        vec![RoomCategory { id: 3, caption: "Chat".into(), max_users: 50 }, RoomCategory { id: 5, caption: "Games".into(), max_users: 0 }]
    }

    #[test]
    fn the_category_is_the_chosen_one_or_the_first() {
        let target = Target { categories: categories(), ..target() };
        let plan = plan_of(&capture(), &target, &choice());
        assert_eq!(room_value(&plan, "category"), Some(PlanValue::Int(3)));
        assert_eq!((plan.report.category, plan.report.categories.len()), (Some(3), 2));
        let plan = plan_of(&capture(), &target, &ImportChoice { category: Some(5), ..choice() });
        assert_eq!(room_value(&plan, "category"), Some(PlanValue::Int(5)));
        // One the hotel does not offer (a staff category, or gone): the first.
        let plan = plan_of(&capture(), &target, &ImportChoice { category: Some(99), ..choice() });
        assert_eq!(room_value(&plan, "category"), Some(PlanValue::Int(3)));
        // None at all: the column's default, and a note.
        let plan = plan_of(&capture(), &Target { categories: Vec::new(), ..target }, &choice());
        assert_eq!((room_value(&plan, "category"), plan.report.category), (None, None));
        assert!(plan.report.notes.iter().any(|n| n.contains("category")), "{:?}", plan.report.notes);
        assert!(room_categories_query("habbo").contains("FROM `habbo`.`navigator_flatcats` WHERE `min_rank` <= 1 AND `public` = '0'"));
    }

    #[test]
    fn the_most_users_and_the_chat_settings_come_within_the_hotels_limits_and_access_stays_open() {
        let mut c = capture();
        c.info = Some(RoomInfo {
            max_users: 75,
            door_mode: 2,
            trade_mode: 0,
            chat: Some(ChatSettings { mode: 1, weight: 9, speed: 2, distance: 0, protection: 1 }),
            ..RoomInfo::default()
        });
        let target = Target { categories: categories(), ..target() };
        // The chat category takes 50 at most.
        let plan = plan_of(&c, &target, &choice());
        assert_eq!(room_value(&plan, "users_max"), Some(PlanValue::Int(50)));
        for (column, value) in [("chat_mode", 1), ("chat_weight", 2), ("chat_speed", 2), ("chat_hearing_distance", 1), ("chat_protection", 1)] {
            assert_eq!(room_value(&plan, column), Some(PlanValue::Int(value)), "{column}");
        }
        assert_eq!(room_value(&plan, "state"), Some(PlanValue::Text("open".into())));
        assert_eq!(room_value(&plan, "password"), Some(PlanValue::Text(String::new())));
        assert_eq!(room_value(&plan, "trade_mode"), None);
        // Games has no limit of its own: Polaris' 200.
        c.info.as_mut().unwrap().max_users = 500;
        let plan = plan_of(&c, &target, &ImportChoice { category: Some(5), ..choice() });
        assert_eq!(room_value(&plan, "users_max"), Some(PlanValue::Int(200)));
        // Nothing captured: the hotel's defaults.
        let plan = plan_of(&capture(), &target, &choice());
        assert_eq!((room_value(&plan, "users_max"), room_value(&plan, "chat_mode")), (None, None));
    }

    #[test]
    fn a_room_with_furni_wired_and_a_custom_floor_plan() {
        let plan = plan(&capture(), &target(), &choice()).unwrap();
        let r = &plan.report;
        assert!(r.custom_model);
        assert_eq!((r.floor_items, r.wall_items, r.wired_converted), (4, 1, 1));
        assert_eq!(r.missing, vec![("not_in_hotel".to_string(), 1)]);
        assert!(r.cleaned.iter().any(|c| c.contains("LTD 3 of 50")) && r.cleaned.iter().any(|c| c.contains("engraving")));
        // room, custom model, its name, 5 items, 1 wired update.
        assert_eq!(plan.statements.len(), 3 + 5 + 1);
        assert!(plan.statements[0].0.starts_with("INSERT INTO `habbo`.`rooms`"));
        let (_, values) = &plan.statements[1];
        assert_eq!(values[1], PlanValue::Template("custom_{#0}".into()));
        assert_eq!(values[5], PlanValue::Text("xxx\r000\rx00".into()));
        // The trophy comes without its engraving, the LTD as a normal item.
        let trophy = plan.statements.iter().find(|(_, v)| v.contains(&PlanValue::Int(14))).unwrap();
        assert!(trophy.1.contains(&PlanValue::Text(String::new())));
        // The toggle box points at the chair's new id; the furni that is not in the hotel is dropped.
        let (sql, values) = plan.statements.last().unwrap();
        assert!(sql.starts_with("UPDATE `habbo`.`items` SET `wired_data`"));
        let chair_statement = plan.statements.iter().position(|(_, v)| v.contains(&PlanValue::Int(11))).unwrap();
        assert_eq!(values[0], PlanValue::Template(format!("{{\"delay\":2,\"itemIds\":[{{#{chair_statement}}}],\"toggleType\":0,\"furniSource\":100}}")));
        assert!(r.notes.iter().any(|n| n.contains("1 of its furni")));
    }

    #[test]
    fn the_hotels_own_model_is_used_when_it_is_the_same() {
        let mut target = target();
        target.model = Some(("xxx\r\n000\r\nx00".into(), Door { x: 0, y: 1, dir: 2 }));
        let mut c = capture();
        c.model_name = "model_a".into();
        let plan = plan(&c, &target, &choice()).unwrap();
        assert!(!plan.report.custom_model);
        assert_eq!(plan.report.model, "model_a");
        assert!(!plan.statements.iter().any(|(sql, _)| sql.contains("room_models_custom")));
    }

    #[test]
    fn the_plan_refuses_a_door_off_the_floor_and_a_missing_owner() {
        let mut bad = choice();
        bad.door = Door { x: 0, y: 0, dir: 2 };
        assert!(plan(&capture(), &target(), &bad).unwrap_err().to_string().contains("not an open tile"));
        bad = choice();
        bad.owner_id = 0;
        assert!(plan(&capture(), &target(), &bad).is_err());
    }

    #[test]
    fn heightmaps_are_padded_and_undo_removes_what_was_made() {
        assert_eq!(normalized_heightmap("xx\r\n0\r\n000"), "xxx\r0xx\r000");
        let mut record = record();
        record.custom_model = true;
        let statements = undo_statements(&record);
        let sql: Vec<&str> = statements.iter().map(|(sql, _)| sql.as_str()).collect();
        assert_eq!(
            sql,
            [
                "DELETE FROM `habbo`.`rooms` WHERE `id` = ? AND `owner_id` = ? AND `users` = 0",
                "DELETE FROM `habbo`.`items` WHERE `id` IN (?, ?, ?) AND `room_id` = ? AND `user_id` = ?",
                "UPDATE `habbo`.`items` SET `room_id` = 0 WHERE `room_id` = ?",
                "UPDATE `habbo`.`bots` SET `room_id` = 0 WHERE `room_id` = ?",
                "UPDATE `habbo`.`users_pets` SET `room_id` = 0 WHERE `room_id` = ?",
                "DELETE FROM `habbo`.`room_rights` WHERE `room_id` = ?",
                "DELETE FROM `habbo`.`room_votes` WHERE `room_id` = ?",
                "DELETE FROM `habbo`.`room_wordfilter` WHERE `room_id` = ?",
            ]
        );
        // The custom floor plan goes on its own, after the commit.
        let (model_sql, model_values) = undo_model_statement(&record).unwrap();
        assert_eq!(model_sql, "DELETE FROM `habbo`.`room_models_custom` WHERE `id` = ?");
        assert_eq!(model_values, [PlanValue::Int(50)]);
        record.custom_model = false;
        assert!(undo_model_statement(&record).is_none());
        // The guarded statement is the room's, and holds only for its owner; bots and pets go back from this room.
        assert_eq!(statements[UNDO_ROOM_STATEMENT].1, [PlanValue::Int(50), PlanValue::Int(1)]);
        assert!(statements[3..5].iter().all(|(_, values)| values == &[PlanValue::Int(50)]));
    }

    #[test]
    fn undo_reads_and_deletes_the_furni_500_at_a_time() {
        let mut record = record();
        record.item_ids = (1..=1001).collect();
        let (room_query, item_queries) = undo_check_queries(&record);
        assert!(room_query.0.contains("u.`online` = '1'") && room_query.0.contains("r.`guild_id`"), "{}", room_query.0);
        assert_eq!(room_query.1, [PlanValue::Int(50)]);
        assert_eq!(item_queries.iter().map(|(_, v)| v.len()).collect::<Vec<_>>(), [500, 500, 1]);
        assert_eq!(item_queries[2].1, [PlanValue::Int(1001)]);
        let deletes: Vec<usize> = undo_statements(&record).iter().filter(|(sql, _)| sql.starts_with("DELETE FROM `habbo`.`items`")).map(|(_, v)| v.len()).collect();
        assert_eq!(deletes, [502, 502, 3]);
    }

    fn free() -> RoomNow {
        RoomNow { owner_id: 1, users: 0, guild_id: 0, owner_online: false }
    }

    fn record() -> ImportRecord {
        ImportRecord {
            server: "s".into(),
            database: "habbo".into(),
            room_id: 50,
            room_name: "Copy".into(),
            owner_id: 1,
            owner_name: "admin".into(),
            item_ids: vec![1, 2, 3],
            custom_model: false,
            source: "habbo.nl 9".into(),
            imported_at: String::new(),
        }
    }

    fn in_room(id: u64) -> ItemNow {
        ItemNow { id, user_id: 1, room_id: 50 }
    }

    #[test]
    fn undo_goes_ahead_when_the_room_is_as_it_was_imported() {
        assert!(undo_problems(&record(), Some(&free()), &[in_room(1), in_room(2), in_room(3)]).is_empty());
        // Even then a furni deletes only while it is in the room and the owner's.
        let (sql, values) = &undo_statements(&record())[1];
        assert!(sql.ends_with("WHERE `id` IN (?, ?, ?) AND `room_id` = ? AND `user_id` = ?"), "{sql}");
        assert_eq!(values[3..], [PlanValue::Int(50), PlanValue::Int(1)]);
    }

    #[test]
    fn undo_refuses_a_room_that_is_gone_in_use_or_of_someone_else() {
        let items = [in_room(1), in_room(2), in_room(3)];
        let gone = undo_problems(&record(), None, &items);
        assert_eq!(gone.len(), 1);
        assert!(gone[0].contains("Room 50 is no longer in the hotel"), "{gone:?}");
        let busy = undo_problems(&record(), Some(&RoomNow { users: 2, ..free() }), &items);
        assert_eq!(busy.len(), 1);
        assert!(busy[0].contains("2 users are in the room") && busy[0].contains("start it once"), "{busy:?}");
        let sold = undo_problems(&record(), Some(&RoomNow { owner_id: 7, ..free() }), &items);
        assert_eq!(sold.len(), 1);
        assert!(sold[0].contains("belongs to user 7"), "{sold:?}");
        let online = undo_problems(&record(), Some(&RoomNow { owner_online: true, ..free() }), &items);
        assert_eq!(online.len(), 1);
        assert!(online[0].contains("ask admin to log out, then try again"), "{online:?}");
        let group = undo_problems(&record(), Some(&RoomNow { guild_id: 4, ..free() }), &items);
        assert_eq!(group.len(), 1);
        assert!(group[0].contains("Group 4 has the room as its home"), "{group:?}");
    }

    #[test]
    fn undo_lists_every_furni_that_moved_was_picked_up_or_traded() {
        let mut record = record();
        record.item_ids = vec![1, 2, 3, 4, 5, 6];
        let room = free();
        let items = [
            in_room(1),
            ItemNow { id: 2, user_id: 1, room_id: 0 },
            ItemNow { id: 3, user_id: 1, room_id: 88 },
            ItemNow { id: 4, user_id: 9, room_id: 0 },
            ItemNow { id: 5, user_id: 9, room_id: 50 },
        ];
        let problems = undo_problems(&record, Some(&room), &items);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("Picked up") && p.ends_with(": 2")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("another room") && p.ends_with(": 3 (room 88)")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("another user") && p.ends_with(": 4 (user 9), 5 (user 9)")), "{problems:?}");
    }

    #[test]
    fn furni_already_gone_do_not_block_undo_and_are_listed() {
        let mut record = record();
        record.item_ids = vec![1, 2, 3, 6, 7];
        let items = [in_room(1), in_room(2), in_room(3)];
        assert!(undo_problems(&record, Some(&free()), &items).is_empty());
        assert_eq!(undo_already_gone(&record, &items), vec![6, 7]);
        // Gone furni mixed with one picked up: only the picked-up one is a problem, the gone ones are still listed.
        let mixed = [in_room(1), ItemNow { id: 2, user_id: 1, room_id: 0 }, in_room(3)];
        let problems = undo_problems(&record, Some(&free()), &mixed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("Picked up") && problems[0].ends_with(": 2"), "{problems:?}");
        assert_eq!(undo_already_gone(&record, &mixed), vec![6, 7]);
    }

    #[test]
    fn an_ltd_is_left_out_unless_it_is_included() {
        let default = ImportChoice { include: BTreeSet::new(), ..choice() };
        let plan_out = plan(&capture(), &target(), &default).unwrap();
        let r = &plan_out.report;
        assert_eq!(r.valuables, vec![ValuableItem { key: ('s', 2), name: "rare_dragon".into(), why: Valuable::Limited { serial: 3, size: 50 }, included: false }]);
        assert_eq!(r.floor_items, 3);
        assert!(!plan_out.statements.iter().any(|(_, v)| v.contains(&PlanValue::Int(12))), "no rare_dragon row");
        assert!(!r.cleaned.iter().any(|c| c.contains("LTD")));
        // Chosen: it comes as the maintainer's normal item, limited_data 0:0, and the report says so.
        let plan_in = plan(&capture(), &target(), &choice()).unwrap();
        assert_eq!(plan_in.report.valuables.len(), 1);
        assert!(plan_in.report.valuables[0].included);
        assert_eq!(plan_in.report.floor_items, 4);
        let row = plan_in.statements.iter().find(|(_, v)| v.contains(&PlanValue::Int(12))).unwrap();
        assert!(row.1.contains(&PlanValue::Text("0:0".into())));
        assert!(plan_in.report.cleaned.iter().any(|c| c.contains("LTD 3 of 50")));
    }

    fn rares() -> (RoomCapture, Target) {
        let mut c = RoomCapture { room_id: 9, model_name: "model_x".into(), heightmap: "xxx\r000\rx00".into(), door_from_hotel: true, ..RoomCapture::default() };
        for (id, class) in [(1, "chair"), (2, "ltd_only"), (3, "event_prize"), (4, "untradeable_prize"), (5, "furnidata_rare")] {
            c.floor_items.insert(id, floor(id, class, 1, StuffData::Empty));
        }
        c.wall_items.insert(7, WallItem { id: 7, type_id: 4000, class_name: Some("poster".into()), location: ":w=1,2 l=3,4 r".into(), data: String::new(), owner_id: 5 });
        c.wall_items.insert(8, WallItem { id: 8, type_id: 4001, class_name: Some("wall_prize".into()), location: ":w=1,2 l=3,4 r".into(), data: String::new(), owner_id: 5 });
        c.rare_classes.insert("furnidata_rare".into());
        let mut untradeable = base(14, "untradeable_prize", "s", "default");
        untradeable.tradeable = false;
        let target = Target {
            database: "habbo".into(),
            items_base: vec![
                base(11, "chair", "s", "default"),
                base(12, "ltd_only", "s", "default"),
                base(13, "event_prize", "s", "default"),
                untradeable,
                base(15, "furnidata_rare", "s", "default"),
                base(16, "poster", "i", "default"),
                base(17, "wall_prize", "i", "default"),
            ],
            catalogue: Some(Catalogue { sold: BTreeSet::from([11, 15, 16]), sold_limited: BTreeSet::from([12]) }),
            ..Target::default()
        };
        (c, target)
    }

    #[test]
    fn rares_are_told_by_the_catalogue_the_furnidata_and_items_base() {
        let (c, target) = rares();
        let none = ImportChoice { include: BTreeSet::new(), ..choice() };
        let plan = plan(&c, &target, &none).unwrap();
        let found: Vec<((char, i32), &str, Valuable)> = plan.report.valuables.iter().map(|v| (v.key, v.name.as_str(), v.why)).collect();
        assert_eq!(
            found,
            vec![
                (('s', 2), "ltd_only", Valuable::SoldAsLimited),
                (('s', 3), "event_prize", Valuable::NotSold),
                (('s', 5), "furnidata_rare", Valuable::Rare),
                (('i', 8), "wall_prize", Valuable::NotSold),
            ]
        );
        assert!(plan.report.valuables.iter().all(|v| !v.included));
        // The chair and the poster the hotel sells, and the prize nobody can trade or sell, come.
        assert_eq!((plan.report.floor_items, plan.report.wall_items), (2, 1));
        for id in [11, 14, 16] {
            assert!(plan.statements.iter().any(|(_, v)| v.contains(&PlanValue::Int(id))), "{id} comes");
        }
        // Included one by one.
        let some = ImportChoice { include: BTreeSet::from([('s', 3), ('i', 8)]), ..choice() };
        let plan = plan_of(&c, &target, &some);
        assert_eq!((plan.report.floor_items, plan.report.wall_items), (3, 2));
        assert_eq!(plan.report.valuables.iter().filter(|v| v.included).map(|v| v.key).collect::<Vec<_>>(), [('s', 3), ('i', 8)]);
    }

    fn plan_of(c: &RoomCapture, target: &Target, choice: &ImportChoice) -> ImportPlan {
        plan(c, target, choice).unwrap()
    }

    #[test]
    fn without_the_catalogue_only_ltd_and_the_furnidata_rares_are_told_and_a_note_says_so() {
        let (c, mut target) = rares();
        target.catalogue = None;
        let plan = plan_of(&c, &target, &ImportChoice { include: BTreeSet::new(), ..choice() });
        assert_eq!(plan.report.valuables.iter().map(|v| v.key).collect::<Vec<_>>(), [('s', 5)]);
        assert!(plan.report.notes.iter().any(|n| n.contains("catalogue")), "{:?}", plan.report.notes);
    }

    #[test]
    fn a_wired_box_drops_the_valuables_left_out() {
        let mut c = capture();
        c.wired.get_mut(&3).unwrap().stuff_ids = vec![1, 2];
        let plan = plan_of(&c, &target(), &ImportChoice { include: BTreeSet::new(), ..choice() });
        let chair_statement = plan.statements.iter().position(|(_, v)| v.contains(&PlanValue::Int(11))).unwrap();
        let (sql, values) = plan.statements.last().unwrap();
        assert!(sql.starts_with("UPDATE `habbo`.`items` SET `wired_data`"));
        assert_eq!(values[0], PlanValue::Template(format!("{{\"delay\":2,\"itemIds\":[{{#{chair_statement}}}],\"toggleType\":0,\"furniSource\":100}}")));
        assert!(plan.report.notes.iter().any(|n| n.starts_with("wf_act_toggle_state: 1 of its furni")), "{:?}", plan.report.notes);
        // Included, the box keeps it.
        let plan = plan_of(&c, &target(), &choice());
        assert!(!plan.report.notes.iter().any(|n| n.contains("of its furni")), "{:?}", plan.report.notes);
    }

    fn page(id: i64, parent_id: i64) -> CatalogPage {
        CatalogPage { id, parent_id, enabled: true, visible: true, min_rank: 1, builder: false }
    }

    #[test]
    fn the_catalogue_counts_only_pages_a_player_reaches() {
        let pages = [
            page(1, -1),
            page(2, 1),
            CatalogPage { visible: false, ..page(3, -1) },
            page(4, 3),
            CatalogPage { min_rank: 5, ..page(5, -1) },
            page(6, 5),
            CatalogPage { builder: true, ..page(7, -1) },
            page(8, 7),
            page(9, 99),
            CatalogPage { enabled: false, ..page(10, 1) },
            page(11, 12),
            page(12, 11),
        ];
        assert_eq!(reachable_pages(&pages), BTreeSet::from([1, 2]));
        let c = Catalogue::new(
            &pages,
            [
                (1, "11", 0),
                (2, "12;13:2", 0),
                (2, "14", 100),
                (1, "11", 50),
                (1, "@20", 0),
                (2, " 15 ; x", 0),
                (4, "30", 0),
                (6, "31", 0),
                (8, "32", 0),
                (9, "33", 0),
                (10, "34", 0),
                (11, "35", 0),
            ],
        );
        assert_eq!(c.sold, BTreeSet::from([11, 12, 13, 15]));
        assert_eq!(c.sold_limited, BTreeSet::from([11, 14]));
        assert_eq!(catalog_pages_query("habbo", true), "SELECT `id`, `parent_id`, `enabled`, `visible`, `min_rank`, `catalog_mode` FROM `habbo`.`catalog_pages`");
        assert!(catalog_pages_query("habbo", false).contains(", 'NORMAL' FROM"));
        assert!(missing_catalog_mode("ERROR 42S22 (1054): Unknown column 'catalog_mode' in 'field list'"));
        assert!(!missing_catalog_mode("ERROR 42S22 (1054): Unknown column 'min_rank' in 'field list'"));
        assert!(!missing_catalog_mode("ERROR 2013 (HY000): Lost connection to MySQL server during query"));
        assert!(!missing_catalog_mode("ERROR 1142 (42000): SELECT command denied to user for table 'catalog_pages'"));
        assert_eq!(catalog_offers_query("habbo"), "SELECT `page_id`, `item_ids`, `limited_stack` FROM `habbo`.`catalog_items`");
    }

    #[test]
    fn trade_marketplace_or_recycle_lets_a_furni_change_hands() {
        assert!(!can_change_hands(Some(false), Some(false), Some(false)));
        assert!(can_change_hands(Some(false), Some(false), Some(true)));
        assert!(can_change_hands(Some(false), Some(true), Some(false)));
        assert!(can_change_hands(Some(true), Some(false), Some(false)));
        assert!(can_change_hands(Some(false), None, Some(false)), "a missing column counts as yes");
    }

    #[test]
    fn currency_furni_are_valuables_even_when_sold_and_not_tradeable() {
        for name in ["CF_50_goldbar", "CFC_200_moneybag", "CF_diamond_10", "DF_5_100", "PF_100", "nft_emerald_emerhand", "nft_emerald_5"] {
            assert!(redeemable(name), "{name}");
        }
        for name in ["chair", "rare_CF_x", "nft_ruby_5"] {
            assert!(!redeemable(name), "{name}");
        }
        let (mut c, mut target) = rares();
        c.floor_items.insert(6, floor(6, "CF_50_goldbar", 1, StuffData::Empty));
        let mut gold = base(18, "CF_50_goldbar", "s", "default");
        gold.tradeable = false;
        target.items_base.push(gold);
        target.catalogue.as_mut().unwrap().sold.insert(18);
        let plan = plan_of(&c, &target, &ImportChoice { include: BTreeSet::new(), ..choice() });
        assert!(plan.report.valuables.iter().any(|v| v.key == ('s', 6) && v.why == Valuable::Currency), "{:?}", plan.report.valuables);
        assert!(!plan.statements.iter().any(|(_, v)| v.contains(&PlanValue::Int(18))));
    }

    #[test]
    fn a_name_is_sold_when_any_row_of_it_is_and_a_colour_variant_is_its_own_name() {
        let mut c = RoomCapture { room_id: 9, model_name: "model_x".into(), heightmap: "xxx\r000\rx00".into(), door_from_hotel: true, ..RoomCapture::default() };
        c.floor_items.insert(1, floor(1, "chair", 1, StuffData::Empty));
        c.floor_items.insert(2, floor(2, "chair*2", 1, StuffData::Empty));
        let target = Target {
            database: "habbo".into(),
            // The class name matches the first row (21), which no offer sells; row 11 of the same name is sold.
            items_base: vec![base(21, "chair", "s", "default"), base(11, "chair", "s", "default"), base(22, "chair*2", "s", "default")],
            catalogue: Some(Catalogue { sold: BTreeSet::from([11]), sold_limited: BTreeSet::new() }),
            ..Target::default()
        };
        let plan = plan_of(&c, &target, &ImportChoice { include: BTreeSet::new(), ..choice() });
        assert_eq!(plan.report.valuables.iter().map(|v| (v.key, v.why)).collect::<Vec<_>>(), [(('s', 2), Valuable::NotSold)]);
        assert!(plan.statements.iter().any(|(_, v)| v.contains(&PlanValue::Int(21))));
    }

    #[test]
    fn a_note_says_when_the_furnidata_rares_could_not_be_checked() {
        let (mut c, target) = rares();
        let none = ImportChoice { include: BTreeSet::new(), ..choice() };
        assert!(plan_of(&c, &target, &none).report.notes.iter().any(|n| n.contains("calls rare")));
        c.rares_checked = true;
        assert!(!plan_of(&c, &target, &none).report.notes.iter().any(|n| n.contains("calls rare")));
    }
}
