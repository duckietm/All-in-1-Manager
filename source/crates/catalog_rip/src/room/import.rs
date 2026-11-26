//! The import of a captured room into a Polaris hotel: a plan (the statements of one transaction, and a report of
//! what they make and leave out) worked out from the capture and what the hotel has, so the page can show it before
//! anything is written. Pure: the caller reads the hotel and runs the statements.
//!
//! What the owner decided (November 2026):
//! * the room is always a new one; access is reset (open, the hotel's default category, trade and chat settings);
//! * furni are matched to items_base by class name (by sprite id when the class name is not known) and become
//!   normal items of the chosen owner, cleaned of what belongs to the source hotel: LTD serials, trophy
//!   engravings, group links, ads and badge displays (listed in the report);
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
}

/// What the user chose for the new room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportChoice {
    pub owner_id: i64,
    pub owner_name: String,
    pub room_name: String,
    pub door: Door,
    /// Seconds since 1970, for date_created.
    pub now: i64,
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

    for item in capture.floor_items.values() {
        let Some(base) = find(&item.class_name, item.type_id, "s", &mut report) else { continue };
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

/// An imported room as the hotel has it now (`rooms.owner_id`, and `rooms.users`: who is inside, kept by Polaris while
/// it runs and set to 0 when it starts), read before Undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomNow {
    pub owner_id: i64,
    pub users: i64,
}

/// A furni of an import as the hotel has it now: its owner (`items.user_id`) and room (`items.room_id`, 0 in an
/// inventory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemNow {
    pub id: u64,
    pub user_id: i64,
    pub room_id: i64,
}

/// Why Undo must not run now, one line per kind with the furni concerned (empty: it may). Undo deletes only a room
/// nobody is in, still of the owner it was imported for, with every furni that came with it still in it and still
/// that owner's: a furni picked up, moved, traded or given since belongs to someone's game now.
pub fn undo_problems(record: &ImportRecord, room: Option<&RoomNow>, items: &[ItemNow]) -> Vec<String> {
    let Some(room) = room else {
        return vec![format!("Room {} is no longer in the hotel.", record.room_id)];
    };
    let mut problems = Vec::new();
    if room.owner_id != record.owner_id {
        problems.push(format!("Room {} belongs to user {} now, not to {}.", record.room_id, room.owner_id, record.owner_name));
    }
    if room.users > 0 {
        problems.push(format!("{} users are in the room now: try again when it is empty.", room.users));
    }
    let (mut gone, mut picked_up, mut moved, mut traded) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for id in &record.item_ids {
        match items.iter().find(|i| i.id == *id) {
            None => gone.push(id.to_string()),
            Some(item) if item.user_id != record.owner_id => traded.push(format!("{id} (user {})", item.user_id)),
            Some(item) if item.room_id == 0 => picked_up.push(id.to_string()),
            Some(item) if item.room_id != record.room_id as i64 => moved.push(format!("{id} (room {})", item.room_id)),
            Some(_) => {}
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
        (&gone, "No longer in the hotel (deleted or recycled)".to_string()),
    ] {
        if !ids.is_empty() {
            problems.push(format!("{what}: {}", listed(ids)));
        }
    }
    problems
}

/// The statements that undo an import, once [`undo_problems`] found nothing: the furni that came with it are
/// deleted (only while they are in the room and the owner's, should anything change meanwhile), furni placed in the
/// room afterwards go back to their owner's inventory, then the custom model and the room go.
pub fn undo_statements(record: &ImportRecord) -> Vec<(String, Vec<PlanValue>)> {
    let db = q(&record.database);
    let room = PlanValue::Int(record.room_id as i64);
    let mut statements = Vec::new();
    for chunk in record.item_ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let mut values: Vec<PlanValue> = chunk.iter().map(|id| PlanValue::Int(*id as i64)).collect();
        values.extend([room.clone(), PlanValue::Int(record.owner_id)]);
        statements.push((format!("DELETE FROM {db}.`items` WHERE `id` IN ({marks}) AND `room_id` = ? AND `user_id` = ?"), values));
    }
    statements.push((format!("UPDATE {db}.`items` SET `room_id` = 0 WHERE `room_id` = ?"), vec![room.clone()]));
    if record.custom_model {
        statements.push((format!("DELETE FROM {db}.`room_models_custom` WHERE `id` = ?"), vec![room.clone()]));
    }
    statements.push((format!("DELETE FROM {db}.`rooms` WHERE `id` = ?"), vec![room]));
    statements
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::packets::{Limited, WallItem, WiredKind};

    fn floor(id: i32, class: &str, x: i32, stuff: StuffData) -> FloorItem {
        FloorItem { id, type_id: id + 1000, class_name: Some(class.into()), x, y: 1, z: 0.0, dir: 2, stuff, limited: None, owner_id: 5 }
    }

    fn base(id: u32, name: &str, kind: &str, interaction: &str) -> BaseItem {
        BaseItem { id, sprite_id: id as i32, item_name: name.into(), item_type: kind.into(), interaction_type: interaction.into() }
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
        ImportChoice { owner_id: 1, owner_name: "admin".into(), room_name: "Copy".into(), door: Door { x: 0, y: 1, dir: 2 }, now: 1_700_000_000 }
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
        let record = ImportRecord {
            server: "s".into(),
            database: "habbo".into(),
            room_id: 50,
            room_name: "Copy".into(),
            owner_id: 1,
            owner_name: "admin".into(),
            item_ids: vec![1, 2, 3],
            custom_model: true,
            source: "habbo.nl 9".into(),
            imported_at: String::new(),
        };
        let statements = undo_statements(&record);
        assert_eq!(statements.len(), 4);
        assert!(statements[0].0.contains("DELETE FROM `habbo`.`items` WHERE `id` IN (?, ?, ?)"));
        assert!(statements[3].0.contains("DELETE FROM `habbo`.`rooms`"));
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
        let room = RoomNow { owner_id: 1, users: 0 };
        assert!(undo_problems(&record(), Some(&room), &[in_room(1), in_room(2), in_room(3)]).is_empty());
        // Even then a furni deletes only while it is in the room and the owner's.
        let (sql, values) = &undo_statements(&record())[0];
        assert!(sql.ends_with("WHERE `id` IN (?, ?, ?) AND `room_id` = ? AND `user_id` = ?"), "{sql}");
        assert_eq!(values[3..], [PlanValue::Int(50), PlanValue::Int(1)]);
    }

    #[test]
    fn undo_refuses_a_room_that_is_gone_in_use_or_of_someone_else() {
        let items = [in_room(1), in_room(2), in_room(3)];
        let gone = undo_problems(&record(), None, &items);
        assert_eq!(gone.len(), 1);
        assert!(gone[0].contains("Room 50 is no longer in the hotel"), "{gone:?}");
        let busy = undo_problems(&record(), Some(&RoomNow { owner_id: 1, users: 2 }), &items);
        assert_eq!(busy.len(), 1);
        assert!(busy[0].contains("2 users are in the room"), "{busy:?}");
        let sold = undo_problems(&record(), Some(&RoomNow { owner_id: 7, users: 0 }), &items);
        assert_eq!(sold.len(), 1);
        assert!(sold[0].contains("belongs to user 7"), "{sold:?}");
    }

    #[test]
    fn undo_lists_every_furni_that_moved_was_picked_up_traded_or_is_gone() {
        let mut record = record();
        record.item_ids = vec![1, 2, 3, 4, 5, 6];
        let room = RoomNow { owner_id: 1, users: 0 };
        let items = [
            in_room(1),
            ItemNow { id: 2, user_id: 1, room_id: 0 },
            ItemNow { id: 3, user_id: 1, room_id: 88 },
            ItemNow { id: 4, user_id: 9, room_id: 0 },
            ItemNow { id: 5, user_id: 9, room_id: 50 },
        ];
        let problems = undo_problems(&record, Some(&room), &items);
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("Picked up") && p.ends_with(": 2")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("another room") && p.ends_with(": 3 (room 88)")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("another user") && p.ends_with(": 4 (user 9), 5 (user 9)")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("No longer in the hotel") && p.ends_with(": 6")), "{problems:?}");
    }
}
