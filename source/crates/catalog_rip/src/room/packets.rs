//! The room packets the Room ripper reads, and their header ids.
//!
//! Layouts and ids checked against the sources (November 2026): the Nitro renderer's parsers
//! (Octane-Renderer `packages/communication/src/messages/parser/room/...`, `roomevents/...`)
//! and the Polaris emulator's composers, which use the same ids as Arcturus Morningstar
//! (`PRODUCTION-201611291003-338511768`). Official Habbo takes its ids from G-Earth's packet
//! info by the message names below (the AIR client's class names without their suffix).
//!
//! Polaris (and Octane) add seven ints after the owner id of every floor and wall furni entry
//! (allowStack/Sit/Lay/Walk, dimX, dimY, teleport target); official Habbo and plain Arcturus do
//! not. The list readers try both layouts and keep the one that reads the packet exactly.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::gearth::PacketInfo;
use crate::wire::{Packet, Reader, Writer};

/// One message the ripper uses: its key, its Arcturus/Nitro id, and the names official Habbo
/// (G-Earth) may give it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub key: &'static str,
    pub to_server: bool,
    pub retro_id: u16,
    pub names: &'static [&'static str],
}

const fn incoming(key: &'static str, retro_id: u16, names: &'static [&'static str]) -> Message {
    Message { key, to_server: false, retro_id, names }
}

const fn outgoing(key: &'static str, retro_id: u16, names: &'static [&'static str]) -> Message {
    Message { key, to_server: true, retro_id, names }
}

pub const ROOM_READY: Message = incoming("RoomReady", 2031, &["RoomReady", "RoomReadyMessageEvent"]);
pub const FLOOR_HEIGHT_MAP: Message = incoming("FloorHeightMap", 1301, &["FloorHeightMap", "FloorHeightMapMessageEvent", "FloorHeightMapEvent"]);
pub const ROOM_VISUALIZATION: Message = incoming("RoomVisualizationSettings", 3547, &["RoomVisualizationSettings", "RoomVisualizationSettingsEvent"]);
pub const ROOM_ENTRY_TILE: Message = incoming("RoomEntryTile", 1664, &["RoomEntryTile", "RoomEntryTileMessageEvent"]);
pub const ROOM_PROPERTY: Message = incoming("RoomProperty", 2454, &["RoomProperty", "RoomPropertyMessageEvent"]);
pub const OBJECTS: Message = incoming("Objects", 1778, &["Objects", "ObjectsMessageEvent"]);
pub const OBJECT_ADD: Message = incoming("ObjectAdd", 1534, &["ObjectAdd", "ObjectAddMessageEvent"]);
pub const OBJECT_UPDATE: Message = incoming("ObjectUpdate", 3776, &["ObjectUpdate", "ObjectUpdateMessageEvent"]);
pub const OBJECT_REMOVE: Message = incoming("ObjectRemove", 2703, &["ObjectRemove", "ObjectRemoveMessageEvent"]);
pub const OBJECT_REMOVE_MULTIPLE: Message = incoming("ObjectRemoveMultiple", 1451, &["ObjectRemoveMultiple", "ObjectRemoveMultipleMessageEvent"]);
pub const OBJECT_DATA_UPDATE: Message = incoming("ObjectDataUpdate", 2547, &["ObjectDataUpdate", "ObjectDataUpdateMessageEvent"]);
pub const OBJECTS_DATA_UPDATE: Message = incoming("ObjectsDataUpdate", 1453, &["ObjectsDataUpdate", "ObjectsDataUpdateMessageEvent"]);
pub const ITEMS: Message = incoming("Items", 1369, &["Items", "ItemsMessageEvent"]);
pub const ITEM_ADD: Message = incoming("ItemAdd", 2187, &["ItemAdd", "ItemAddMessageEvent"]);
pub const ITEM_UPDATE: Message = incoming("ItemUpdate", 2009, &["ItemUpdate", "ItemUpdateMessageEvent"]);
pub const ITEM_REMOVE: Message = incoming("ItemRemove", 3208, &["ItemRemove", "ItemRemoveMessageEvent"]);
pub const ITEMS_STATE_UPDATE: Message = incoming("ItemsStateUpdate", 3697, &["ItemsStateUpdate", "ItemsStateUpdateMessageEvent"]);
pub const ROOM_INFO: Message = incoming("GetGuestRoomResult", 687, &["GetGuestRoomResult", "GetGuestRoomResultEvent", "GetGuestRoomResultMessageEvent"]);
pub const WIRED_TRIGGER: Message = incoming("WiredFurniTrigger", 383, &["WiredFurniTrigger", "WiredFurniTriggerEvent"]);
pub const WIRED_ACTION: Message = incoming("WiredFurniAction", 1434, &["WiredFurniAction", "WiredFurniActionEvent"]);
pub const WIRED_CONDITION: Message = incoming("WiredFurniCondition", 1108, &["WiredFurniCondition", "WiredFurniConditionEvent"]);
pub const WIRED_ADDON: Message = incoming("WiredFurniAddon", 2168, &["WiredFurniAddon", "WiredFurniAddonEvent"]);
pub const WIRED_SELECTOR: Message = incoming("WiredFurniSelector", 2509, &["WiredFurniSelector", "WiredFurniSelectorEvent"]);
pub const WIRED_VARIABLE: Message = incoming("WiredFurniVariable", 2955, &["WiredFurniVariable", "WiredFurniVariableEvent"]);
/// Polaris' wired rights of the room (a custom message); official Habbo sends WiredPermissions.
pub const WIRED_ROOM_SETTINGS: Message = incoming("WiredRoomSettingsData", 5102, &["WiredRoomSettingsData"]);
pub const WIRED_PERMISSIONS: Message = incoming("WiredPermissions", 0, &["WiredPermissions", "WiredPermissionsEvent"]);

/// Opens a wired box's settings without switching its light (Polaris; official `Open`).
pub const WIRED_OPEN: Message = outgoing("Open", 768, &["Open", "OpenMessageComposer", "OpenMessage"]);
/// A double click on a floor furni: opens a wired box, but also flips its light for the room.
pub const USE_FURNITURE: Message = outgoing("UseFurniture", 99, &["UseFurniture", "UseFurnitureMessageComposer"]);

pub const INCOMING: &[Message] = &[
    ROOM_READY, FLOOR_HEIGHT_MAP, ROOM_VISUALIZATION, ROOM_ENTRY_TILE, ROOM_PROPERTY, OBJECTS, OBJECT_ADD, OBJECT_UPDATE, OBJECT_REMOVE,
    OBJECT_REMOVE_MULTIPLE, OBJECT_DATA_UPDATE, OBJECTS_DATA_UPDATE, ITEMS, ITEM_ADD, ITEM_UPDATE, ITEM_REMOVE, ITEMS_STATE_UPDATE, ROOM_INFO,
    WIRED_TRIGGER, WIRED_ACTION, WIRED_CONDITION, WIRED_ADDON, WIRED_SELECTOR, WIRED_VARIABLE, WIRED_ROOM_SETTINGS, WIRED_PERMISSIONS,
];
pub const OUTGOING: &[Message] = &[WIRED_OPEN, USE_FURNITURE];

/// The ids in use for this connection, by message key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomHeaders {
    pub incoming: BTreeMap<String, u16>,
    pub outgoing: BTreeMap<String, u16>,
}

impl RoomHeaders {
    /// The ids: a profile's own (`extra_headers`, by key) first, then G-Earth's packet info (official
    /// Habbo), then the Arcturus/Nitro ids when `retro` is true.
    pub fn resolve(profile: &BTreeMap<String, u16>, packets: &[PacketInfo], retro: bool) -> RoomHeaders {
        let id = |message: &Message| -> Option<u16> {
            profile
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(message.key))
                .map(|(_, v)| *v)
                .or_else(|| {
                    message.names.iter().find_map(|wanted| {
                        packets
                            .iter()
                            .filter(|p| p.to_server == message.to_server)
                            .find(|p| p.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(wanted)))
                            .and_then(|p| u16::try_from(p.header_id).ok())
                    })
                })
                .or_else(|| (retro && message.retro_id > 0).then_some(message.retro_id))
        };
        let mut headers = RoomHeaders::default();
        for message in INCOMING {
            if let Some(id) = id(message) {
                headers.incoming.insert(message.key.to_string(), id);
            }
        }
        for message in OUTGOING {
            if let Some(id) = id(message) {
                headers.outgoing.insert(message.key.to_string(), id);
            }
        }
        headers
    }

    /// The key of an incoming id.
    pub fn incoming_key(&self, header: u16) -> Option<&str> {
        self.incoming.iter().find(|(_, v)| **v == header).map(|(k, _)| k.as_str())
    }

    pub fn out(&self, message: &Message) -> Option<u16> {
        self.outgoing.get(message.key).copied()
    }

    /// The messages a capture cannot do without.
    pub fn missing_required(&self) -> Vec<&'static str> {
        [ROOM_READY, FLOOR_HEIGHT_MAP, OBJECTS, ITEMS].iter().filter(|m| !self.incoming.contains_key(m.key)).map(|m| m.key).collect()
    }
}

// ---------------------------------------------------------------------------------------------- the furni

/// A furni's state data ("stuff data"), in each format the client knows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum StuffData {
    Legacy { data: String },
    Map { entries: Vec<(String, String)> },
    Strings { values: Vec<String> },
    Vote { state: String, result: i32 },
    Empty,
    Ints { values: Vec<i32> },
    HighScore { state: String, score_type: i32, clear_type: i32 },
    Crackable { state: String, hits: i32, target: i32 },
}

impl StuffData {
    /// The state the client shows (Nitro's `getLegacyString`).
    pub fn state(&self) -> String {
        match self {
            StuffData::Legacy { data } => data.clone(),
            StuffData::Map { entries } => entries.iter().find(|(k, _)| k == "state").map(|(_, v)| v.clone()).unwrap_or_default(),
            StuffData::Strings { values } => values.first().cloned().unwrap_or_default(),
            StuffData::Vote { state, .. } | StuffData::HighScore { state, .. } | StuffData::Crackable { state, .. } => state.clone(),
            StuffData::Empty => String::new(),
            StuffData::Ints { values } => values.first().map(|v| v.to_string()).unwrap_or_default(),
        }
    }
}

/// An LTD's serial number and series size (stuff data flag 256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limited {
    pub serial: i32,
    pub size: i32,
}

/// Reads stuff data: `int flags` (format in the low byte), the format's payload, then the LTD fields when flag 256 is set.
pub fn read_stuff(r: &mut Reader<'_>) -> Result<(StuffData, Option<Limited>)> {
    let flags = r.i32("stuff data flags")?;
    let stuff = match flags & 0xFF {
        0 => StuffData::Legacy { data: r.string("legacy data")? },
        1 => {
            let n = r.count(4, "map entries")?;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push((r.string("map key")?, r.string("map value")?));
            }
            StuffData::Map { entries }
        }
        2 => {
            let n = r.count(2, "strings")?;
            StuffData::Strings { values: (0..n).map(|_| r.string("string")).collect::<Result<_>>()? }
        }
        3 => StuffData::Vote { state: r.string("vote state")?, result: r.i32("vote result")? },
        4 => StuffData::Empty,
        5 => {
            let n = r.count(4, "ints")?;
            StuffData::Ints { values: (0..n).map(|_| r.i32("int")).collect::<Result<_>>()? }
        }
        6 => {
            let state = r.string("highscore state")?;
            let score_type = r.i32("score type")?;
            let clear_type = r.i32("clear type")?;
            let n = r.count(8, "scores")?;
            for _ in 0..n {
                r.i32("score")?;
                let m = r.count(2, "score names")?;
                for _ in 0..m {
                    r.string("score name")?;
                }
            }
            StuffData::HighScore { state, score_type, clear_type }
        }
        7 => StuffData::Crackable { state: r.string("crackable state")?, hits: r.i32("hits")?, target: r.i32("target")? },
        other => bail!("unknown stuff data format {other}"),
    };
    let limited = if flags & 256 != 0 { Some(Limited { serial: r.i32("ltd serial")?, size: r.i32("ltd size")? }) } else { None };
    Ok((stuff, limited))
}

/// A floor furni as the room shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloorItem {
    pub id: i32,
    /// The furnidata id (sprite id) in the source hotel; negative with `class_name` for a furni without one.
    pub type_id: i32,
    /// The class name (furnidata classname), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub z: f64,
    pub dir: i32,
    pub stuff: StuffData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited: Option<Limited>,
    pub owner_id: i32,
}

/// A wall furni.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallItem {
    pub id: i32,
    pub type_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    /// `:w=x,y l=x,y r`
    pub location: String,
    pub data: String,
    pub owner_id: i32,
}

/// How many ints follow the owner id: 7 on Polaris/Octane, 0 elsewhere.
const POLARIS_EXTRA_INTS: usize = 7;

fn read_floor_item(r: &mut Reader<'_>, extra_ints: usize) -> Result<FloorItem> {
    let id = r.i32("item id")?;
    let type_id = r.i32("type id")?;
    let x = r.i32("x")?;
    let y = r.i32("y")?;
    let dir = r.i32("direction")?;
    let z = r.string("z")?;
    r.string("stack height")?;
    r.i32("extra")?;
    let (stuff, limited) = read_stuff(r)?;
    r.i32("expires")?;
    r.i32("usage policy")?;
    let owner_id = r.i32("owner id")?;
    for _ in 0..extra_ints {
        r.i32("polaris field")?;
    }
    let class_name = if type_id < 0 { Some(r.string("class name")?) } else { None };
    let z = z.replace(',', ".").trim().parse::<f64>().unwrap_or(0.0);
    Ok(FloorItem { id, type_id, class_name, x, y, z, dir: dir.rem_euclid(8), stuff, limited, owner_id })
}

fn read_wall_item(r: &mut Reader<'_>, extra_ints: usize) -> Result<WallItem> {
    let id = r.string("item id")?.trim().parse::<i32>().unwrap_or(0);
    let type_id = r.i32("type id")?;
    let location = r.string("location")?;
    let data = r.string("data")?;
    r.i32("expires")?;
    r.i32("usage policy")?;
    let owner_id = r.i32("owner id")?;
    for _ in 0..extra_ints {
        r.i32("polaris field")?;
    }
    Ok(WallItem { id, type_id, class_name: None, location, data, owner_id })
}

/// Reads with both layouts (with and without Polaris' seven ints) and keeps the one that reads `body` to its end.
fn either_layout<T>(body: &[u8], read: impl Fn(&mut Reader<'_>, usize) -> Result<T>) -> Result<(T, usize)> {
    let mut first_error = None;
    for extra in [POLARIS_EXTRA_INTS, 0] {
        let mut r = Reader::new(body);
        match read(&mut r, extra) {
            Ok(value) if r.is_empty() => return Ok((value, extra)),
            Ok(_) => first_error = first_error.or_else(|| Some(anyhow::anyhow!("{} bytes left over", r.remaining()))),
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    Err(first_error.unwrap_or_else(|| anyhow::anyhow!("unreadable")))
}

/// Like [`either_layout`], but a packet may carry more after the item (an owner name, Polaris' user id): the first
/// layout that reads the item (and, when `tail` says so, what follows) wins.
fn either_layout_loose<T>(body: &[u8], read: impl Fn(&mut Reader<'_>, usize) -> Result<T>) -> Result<T> {
    if let Ok((value, _)) = either_layout(body, &read) {
        return Ok(value);
    }
    let mut r = Reader::new(body);
    read(&mut r, 0)
}

fn owners(r: &mut Reader<'_>) -> Result<BTreeMap<i32, String>> {
    let n = r.count(6, "owners")?;
    let mut map = BTreeMap::new();
    for _ in 0..n {
        let id = r.i32("owner id")?;
        map.insert(id, r.string("owner name")?);
    }
    Ok(map)
}

/// Objects (the floor furni list; Polaris sends it in parts of 250): the owners and the furni.
pub fn parse_objects(body: &[u8]) -> Result<(BTreeMap<i32, String>, Vec<FloorItem>)> {
    either_layout(body, |r, extra| {
        let owners = owners(r)?;
        let n = r.count(30, "floor furni")?;
        let items = (0..n).map(|_| read_floor_item(r, extra)).collect::<Result<Vec<_>>>()?;
        Ok((owners, items))
    })
    .map(|(v, _)| v)
}

/// Items (the wall furni list).
pub fn parse_items(body: &[u8]) -> Result<(BTreeMap<i32, String>, Vec<WallItem>)> {
    either_layout(body, |r, extra| {
        let owners = owners(r)?;
        let n = r.count(20, "wall furni")?;
        let items = (0..n).map(|_| read_wall_item(r, extra)).collect::<Result<Vec<_>>>()?;
        Ok((owners, items))
    })
    .map(|(v, _)| v)
}

/// ObjectAdd (then an owner name) and ObjectUpdate.
pub fn parse_object(body: &[u8]) -> Result<FloorItem> {
    either_layout_loose(body, |r, extra| {
        let item = read_floor_item(r, extra)?;
        if !r.is_empty() {
            r.string("owner name")?;
        }
        Ok(item)
    })
}

/// ItemAdd / ItemUpdate.
pub fn parse_item(body: &[u8]) -> Result<WallItem> {
    either_layout_loose(body, |r, extra| {
        let item = read_wall_item(r, extra)?;
        if !r.is_empty() {
            r.string("owner name or user id")?;
        }
        Ok(item)
    })
}

/// ObjectRemove / ItemRemove: the id (as a string).
pub fn parse_remove(body: &[u8]) -> Result<i32> {
    Ok(Reader::new(body).string("item id")?.trim().parse::<i32>().unwrap_or(0))
}

/// ObjectRemoveMultiple: the ids.
pub fn parse_remove_multiple(body: &[u8]) -> Result<Vec<i32>> {
    let mut r = Reader::new(body);
    let n = r.count(4, "ids")?;
    (0..n).map(|_| r.i32("id")).collect()
}

/// ObjectDataUpdate: one furni's new state.
pub fn parse_object_data(body: &[u8]) -> Result<(i32, StuffData, Option<Limited>)> {
    let mut r = Reader::new(body);
    let id = r.string("item id")?.trim().parse::<i32>().unwrap_or(0);
    let (stuff, limited) = read_stuff(&mut r)?;
    Ok((id, stuff, limited))
}

/// ObjectsDataUpdate: several furni's new state.
pub fn parse_objects_data(body: &[u8]) -> Result<Vec<(i32, StuffData, Option<Limited>)>> {
    let mut r = Reader::new(body);
    let n = r.count(8, "states")?;
    (0..n)
        .map(|_| {
            let id = r.i32("item id")?;
            let (stuff, limited) = read_stuff(&mut r)?;
            Ok((id, stuff, limited))
        })
        .collect()
}

/// ItemsStateUpdate: wall furni states.
pub fn parse_items_state(body: &[u8]) -> Result<Vec<(i32, String)>> {
    let mut r = Reader::new(body);
    let n = r.count(6, "states")?;
    (0..n).map(|_| Ok((r.i32("item id")?, r.string("state")?))).collect()
}

// ---------------------------------------------------------------------------------------------- the room

/// RoomReady: the model name and the room id.
pub fn parse_room_ready(body: &[u8]) -> Result<(String, i32)> {
    let mut r = Reader::new(body);
    Ok((r.string("model name")?, r.i32("room id")?))
}

/// FloorHeightMap: scale, the fixed wall height (-1: automatic), the heightmap (rows separated by `\r`).
pub fn parse_floor_height_map(body: &[u8]) -> Result<(bool, i32, String)> {
    let mut r = Reader::new(body);
    Ok((r.bool("scale")?, r.i32("wall height")?, r.string("heightmap")?))
}

/// RoomVisualizationSettings: hide walls, wall thickness, floor thickness (-2..1).
pub fn parse_visualization(body: &[u8]) -> Result<(bool, i32, i32)> {
    let mut r = Reader::new(body);
    Ok((r.bool("hide walls")?, r.i32("wall thickness")?, r.i32("floor thickness")?))
}

/// RoomEntryTile: the door.
pub fn parse_entry_tile(body: &[u8]) -> Result<Door> {
    let mut r = Reader::new(body);
    Ok(Door { x: r.i32("door x")?, y: r.i32("door y")?, dir: r.i32("door direction")? })
}

/// RoomProperty: `floor`, `wallpaper` or `landscape`, and its value.
pub fn parse_property(body: &[u8]) -> Result<(String, String)> {
    let mut r = Reader::new(body);
    Ok((r.string("property")?, r.string("value")?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Door {
    pub x: i32,
    pub y: i32,
    pub dir: i32,
}

/// What GetGuestRoomResult says about a room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub room_id: i32,
    /// True when it is the room being entered.
    pub entering: bool,
    pub name: String,
    pub owner_name: String,
    /// 0 open, 1 doorbell, 2 password, 3 invisible.
    pub door_mode: i32,
    pub max_users: i32,
    pub description: String,
    pub trade_mode: i32,
    pub category: i32,
    pub tags: Vec<String>,
}

/// GetGuestRoomResult, up to the tags (what follows is not needed).
pub fn parse_room_info(body: &[u8]) -> Result<RoomInfo> {
    let mut r = Reader::new(body);
    let entering = r.bool("room enter")?;
    let room_id = r.i32("room id")?;
    let name = r.string("name")?;
    r.i32("owner id")?;
    let owner_name = r.string("owner name")?;
    let door_mode = r.i32("door mode")?;
    r.i32("user count")?;
    let max_users = r.i32("max users")?;
    let description = r.string("description")?;
    let trade_mode = r.i32("trade mode")?;
    r.i32("score")?;
    r.i32("ranking")?;
    let category = r.i32("category")?;
    let n = r.count(2, "tags")?;
    let tags = (0..n).map(|_| r.string("tag")).collect::<Result<_>>()?;
    Ok(RoomInfo { room_id, entering, name, owner_name, door_mode, max_users, description, trade_mode, category, tags })
}

// ---------------------------------------------------------------------------------------------- wired

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WiredKind {
    Trigger,
    Action,
    Condition,
    Addon,
    Selector,
    Variable,
}

/// A wired box's settings, as the hotel sent them when the box was opened: the same fields its save packet carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WiredSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WiredKind>,
    pub box_id: i32,
    pub code: i32,
    pub string_param: String,
    pub int_params: Vec<i32>,
    pub stuff_ids: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stuff_ids2: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub furni_sources: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_sources: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variable_ids: Vec<String>,
    /// Actions: the delay in pulses of 0.5 s.
    pub delay: i32,
    /// Conditions: the quantifier.
    pub quantifier: i32,
}

fn ints(r: &mut Reader<'_>, what: &str) -> Result<Vec<i32>> {
    let n = r.count(4, what)?;
    (0..n).map(|_| r.i32(what)).collect()
}

/// The current (official, Polaris and this Nitro) layout: `furniLimit, stuffIds, stuffIds2, stuffTypeId, id,
/// string, intParams, variableIds, furniSources, userSources, code`, then the kind's own field. What follows (input
/// sources, context) is not needed.
fn wired_current(body: &[u8], kind: WiredKind) -> Result<WiredSettings> {
    let mut r = Reader::new(body);
    r.i32("furni limit")?;
    let stuff_ids = ints(&mut r, "stuff ids")?;
    let stuff_ids2 = ints(&mut r, "stuff ids 2")?;
    r.i32("stuff type id")?;
    let box_id = r.i32("box id")?;
    let string_param = r.string("string param")?;
    let int_params = ints(&mut r, "int params")?;
    let n = r.count(2, "variable ids")?;
    let variable_ids = (0..n).map(|_| r.string("variable id")).collect::<Result<_>>()?;
    let furni_sources = ints(&mut r, "furni sources")?;
    let user_sources = ints(&mut r, "user sources")?;
    let code = r.i32("code")?;
    let (mut delay, mut quantifier) = (0, 0);
    match kind {
        WiredKind::Action => delay = r.i32("delay")?,
        WiredKind::Condition => quantifier = r.i32("quantifier")?,
        WiredKind::Selector => {
            r.bool("filter")?;
            r.bool("invert")?;
        }
        _ => {}
    }
    // A sanity check against reading the legacy layout as this one: the advanced-mode flag follows.
    let advanced = r.byte("advanced mode")?;
    if advanced > 1 {
        bail!("not the current wired layout");
    }
    Ok(WiredSettings { kind: Some(kind), box_id, code, string_param, int_params, stuff_ids, stuff_ids2, furni_sources, user_sources, variable_ids, delay, quantifier })
}

/// The legacy (Arcturus, 2016 Flash) layout: `bool stuffTypeSelection, furniLimit, stuffIds, stuffTypeId, id,
/// string, intParams`, then `stuffTypeSelectionCode` and for a trigger/action the conflicting boxes, an action's
/// type and delay, a condition's type.
fn wired_legacy(body: &[u8], kind: WiredKind) -> Result<WiredSettings> {
    let mut r = Reader::new(body);
    r.bool("stuff type selection")?;
    r.i32("furni limit")?;
    let stuff_ids = ints(&mut r, "stuff ids")?;
    r.i32("stuff type id")?;
    let box_id = r.i32("box id")?;
    let string_param = r.string("string param")?;
    let int_params = ints(&mut r, "int params")?;
    r.i32("stuff type selection code")?;
    let code = r.i32("code")?;
    let mut delay = 0;
    match kind {
        WiredKind::Trigger => {
            ints(&mut r, "conflicting actions")?;
        }
        WiredKind::Action => {
            delay = r.i32("delay")?;
            ints(&mut r, "conflicting triggers")?;
        }
        _ => {}
    }
    if !r.is_empty() {
        bail!("{} bytes left over", r.remaining());
    }
    Ok(WiredSettings { kind: Some(kind), box_id, code, string_param, int_params, stuff_ids, delay, ..WiredSettings::default() })
}

/// A wired definition packet, in whichever layout the hotel uses.
pub fn parse_wired(body: &[u8], kind: WiredKind) -> Result<WiredSettings> {
    match wired_current(body, kind) {
        Ok(settings) => Ok(settings),
        Err(current) => wired_legacy(body, kind).map_err(|legacy| anyhow::anyhow!("wired settings not readable ({current}; legacy layout: {legacy})")),
    }
}

/// Whether the user may read the wired here: Polaris' WiredRoomSettingsData (`roomId, inspectMask, modifyMask,
/// canInspect, ...`) or official WiredPermissions (`canModify, canRead`).
pub fn parse_wired_rights(key: &str, body: &[u8]) -> Result<bool> {
    let mut r = Reader::new(body);
    if key == WIRED_ROOM_SETTINGS.key {
        r.i32("room id")?;
        r.i32("inspect mask")?;
        r.i32("modify mask")?;
        r.bool("can inspect")
    } else {
        r.bool("can modify")?;
        r.bool("can read")
    }
}

/// The packet that opens a box: Open (`int id`) or UseFurniture (`int id, int state`).
pub fn open_box(header: u16, use_furniture: bool, box_id: i32) -> Packet {
    let writer = Writer::new(header).i32(box_id);
    if use_furniture {
        writer.i32(0).packet()
    } else {
        writer.packet()
    }
}

/// Whether a class name is a wired box (`wf_trg_*`, `wf_act_*`, `wf_cnd_*`, `wf_xtra_*`, `wf_slc_*`, `wf_var_*`).
pub fn is_wired_class(class_name: &str) -> bool {
    let c = class_name.to_ascii_lowercase();
    ["wf_trg_", "wf_act_", "wf_cnd_", "wf_xtra_", "wf_slc_", "wf_var_", "wf_blob"].iter().any(|p| c.starts_with(p)) && !c.starts_with("wf_blob")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn floor_entry(w: Writer, id: i32, type_id: i32, polaris: bool, stuff: impl Fn(Writer) -> Writer) -> Writer {
        let w = w.i32(id).i32(type_id).i32(3).i32(4).i32(2).string("0.5").string("1.0").i32(0);
        let w = stuff(w).i32(-1).i32(0).i32(77);
        let w = if polaris { (0..7).fold(w, |w, _| w.i32(1)) } else { w };
        if type_id < 0 {
            w.string("some_class")
        } else {
            w
        }
    }

    #[test]
    fn floor_furni_in_both_layouts() {
        for polaris in [true, false] {
            let w = Writer::new(1).i32(1).i32(77).string("owner");
            let w = w.i32(3);
            let w = floor_entry(w, 10, 200, polaris, |w| w.i32(0).string("1"));
            // An LTD in map format.
            let w = floor_entry(w, 11, 201, polaris, |w| w.i32(1 | 256).i32(1).string("state").string("2").i32(5).i32(100));
            let w = floor_entry(w, 12, -1, polaris, |w| w.i32(5).i32(4).i32(1).i32(2).i32(3).i32(4));
            let packet = w.packet();
            let (owners, items) = parse_objects(&packet.body).unwrap();
            assert_eq!(owners.get(&77).map(String::as_str), Some("owner"));
            assert_eq!(items.len(), 3, "polaris {polaris}");
            assert_eq!((items[0].x, items[0].y, items[0].z, items[0].dir), (3, 4, 0.5, 2));
            assert_eq!(items[0].stuff.state(), "1");
            assert_eq!(items[1].limited, Some(Limited { serial: 5, size: 100 }));
            assert_eq!(items[1].stuff.state(), "2");
            assert_eq!(items[2].class_name.as_deref(), Some("some_class"));
            assert_eq!(items[2].stuff, StuffData::Ints { values: vec![1, 2, 3, 4] });
        }
    }

    #[test]
    fn wall_furni_and_room_packets() {
        for polaris in [true, false] {
            let w = Writer::new(1).i32(0).i32(1).string("55").i32(300).string(":w=2,3 l=10,20 r").string("ff9cff").i32(-1).i32(0).i32(9);
            let w = if polaris { (0..7).fold(w, |w, _| w.i32(0)) } else { w };
            let (_, items) = parse_items(&w.packet().body).unwrap();
            assert_eq!((items[0].id, items[0].location.as_str(), items[0].data.as_str()), (55, ":w=2,3 l=10,20 r", "ff9cff"));
        }
        let ready = Writer::new(1).string("model_a").i32(42).packet();
        assert_eq!(parse_room_ready(&ready.body).unwrap(), ("model_a".to_string(), 42));
        let map = Writer::new(1).bool(true).i32(-1).string("xxx\rx00\rx00").packet();
        assert_eq!(parse_floor_height_map(&map.body).unwrap(), (true, -1, "xxx\rx00\rx00".to_string()));
        let info = Writer::new(1).bool(true).i32(42).string("Villa").i32(1).string("me").i32(2).i32(3).i32(25).string("desc").i32(1).i32(0).i32(0).i32(5).i32(2).string("a").string("b").i32(0).packet();
        let info = parse_room_info(&info.body).unwrap();
        assert_eq!((info.room_id, info.name.as_str(), info.door_mode, info.tags.len()), (42, "Villa", 2, 2));
    }

    #[test]
    fn wired_in_both_layouts() {
        let current = Writer::new(1)
            .i32(5)
            .i32(2).i32(10).i32(11)
            .i32(0)
            .i32(900)
            .i32(33)
            .string("hello")
            .i32(1).i32(1)
            .i32(0)
            .i32(1).i32(100)
            .i32(1).i32(0)
            .i32(0)
            .i32(4)
            .bool(false)
            .packet();
        let s = parse_wired(&current.body, WiredKind::Action).unwrap();
        assert_eq!((s.box_id, s.stuff_ids.clone(), s.string_param.as_str(), s.delay, s.furni_sources.clone()), (33, vec![10, 11], "hello", 4, vec![100]));
        let legacy = Writer::new(1).bool(false).i32(5).i32(1).i32(10).i32(900).i32(33).string("").i32(2).i32(1).i32(0).i32(0).i32(3).i32(2).i32(0).packet();
        let s = parse_wired(&legacy.body, WiredKind::Action).unwrap();
        assert_eq!((s.box_id, s.int_params.clone(), s.delay, s.code), (33, vec![1, 0], 2, 3));
    }

    #[test]
    fn headers_for_retros_and_official() {
        let retro = RoomHeaders::resolve(&BTreeMap::new(), &[], true);
        assert_eq!(retro.incoming.get("Objects"), Some(&1778));
        assert_eq!(retro.out(&WIRED_OPEN), Some(768));
        assert!(retro.missing_required().is_empty());
        let official = RoomHeaders::resolve(
            &BTreeMap::new(),
            &[PacketInfo { header_id: 3001, hash: None, name: Some("Objects".into()), structure: None, to_server: false, source: "sulek".into() }],
            false,
        );
        assert_eq!(official.incoming.get("Objects"), Some(&3001));
        assert!(official.missing_required().contains(&"RoomReady"));
        assert!(is_wired_class("wf_act_toggle_state") && is_wired_class("WF_TRG_ENTER_ROOM") && !is_wired_class("wf_blob") && !is_wired_class("chair"));
    }
}
