//! The capture: a passive G-Earth extension that records every room the user enters, and on request reads the
//! settings of the room's wired boxes.
//!
//! It never sends anything that changes the hotel. While capturing it only listens and sends nothing by itself: a
//! packet sent with the id of another client version can make the hotel close the connection. "Read the wired" opens
//! the room's wired boxes one at a time with Open (no visible effect). Only when the user allows it, UseFurniture (a
//! double click) is used where Open is unknown or gets no answer: the box's light then flips for everyone in the room.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::packet_log::{self, Names, PacketLog};
use super::packets::{self as p, Door, FloorItem, RoomHeaders, RoomInfo, WallItem, WiredKind, WiredSettings};
use crate::furnidata::Furnidata;
use crate::gearth::{Event, ExtensionInfo, Session};
use crate::wire::Packet;
use crate::profiles::{HotelKind, HotelProfile};
use crate::rip::{ProfileChoice, RipOptions};
use crate::{Ask, Cancelled, Log, RipHooks};

pub const CAPTURE_VERSION: u32 = 1;

/// One captured room.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoomCapture {
    pub version: u32,
    /// The game server and what the hotel is called in file names (habbo.nl, or the retro's host).
    pub host: String,
    pub hotel: String,
    pub client_type: String,
    pub profile: String,
    pub official: bool,
    /// When the capture started (local time, RFC 3339).
    pub captured_at: String,
    pub room_id: i32,
    pub model_name: String,
    /// Rows separated by `\r`, as the hotel sends it.
    pub heightmap: String,
    pub wall_height: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub door: Option<Door>,
    /// The door came from the hotel (else it is a guess from the heightmap; the import lets you change it).
    #[serde(default)]
    pub door_from_hotel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visualization: Option<(bool, i32, i32)>,
    pub floor: String,
    pub wallpaper: String,
    pub landscape: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<RoomInfo>,
    pub owners: BTreeMap<i32, String>,
    pub floor_items: BTreeMap<i32, FloorItem>,
    pub wall_items: BTreeMap<i32, WallItem>,
    /// The settings of the wired boxes that were read, by box id.
    pub wired: BTreeMap<i32, WiredSettings>,
    /// Whether the hotel said the user may read the wired here (None: it did not say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_read_wired: Option<bool>,
    /// The packets that could not be read, for the report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    /// The class names (lower case) of the room's furni that the other hotel's furnidata marks `rare`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub rare_classes: BTreeSet<String>,
    /// The furnidata was there to fill `rare_classes` (false for a capture without it, or made before).
    #[serde(default)]
    pub rares_checked: bool,
}

/// The class names (lower case) of the room's furni that the furnidata marks `rare` (true, 1 or "1").
pub fn furnidata_rares(room: &RoomCapture, furnidata: &Furnidata) -> BTreeSet<String> {
    let rare = |kind: char, type_id: i32| {
        furnidata.entries.get(&(kind, type_id)).and_then(|e| e.get("rare")).is_some_and(|v| match v {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::Number(n) => n.as_i64() == Some(1),
            serde_json::Value::String(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true"),
            _ => false,
        })
    };
    let floor = room.floor_items.values().filter(|i| rare('s', i.type_id)).map(|i| (i.type_id, false));
    let wall = room.wall_items.values().filter(|i| rare('i', i.type_id)).map(|i| (i.type_id, true));
    floor
        .chain(wall)
        .filter_map(|(type_id, wall)| if wall { furnidata.wall.get(&type_id) } else { furnidata.floor.get(&type_id) }.map(|c| c.to_ascii_lowercase()))
        .collect()
}

/// What the room list shows of a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSummary {
    pub room_id: i32,
    pub name: String,
    pub owner: String,
    pub floor_items: usize,
    pub wall_items: usize,
    pub wired_boxes: usize,
    pub wired_read: usize,
}

impl RoomCapture {
    pub fn name(&self) -> String {
        self.info.as_ref().map(|i| i.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Room {}", self.room_id))
    }

    /// The floor furni that are wired boxes (by class name; a furni without one cannot be told).
    pub fn wired_box_ids(&self) -> Vec<i32> {
        self.floor_items.values().filter(|i| i.class_name.as_deref().is_some_and(p::is_wired_class)).map(|i| i.id).collect()
    }

    pub fn summary(&self) -> CaptureSummary {
        let boxes = self.wired_box_ids();
        CaptureSummary {
            room_id: self.room_id,
            name: self.name(),
            owner: self.info.as_ref().map(|i| i.owner_name.clone()).unwrap_or_default(),
            floor_items: self.floor_items.len(),
            wall_items: self.wall_items.len(),
            wired_boxes: boxes.len().max(self.wired.len()),
            wired_read: self.wired.len(),
        }
    }

    /// The file name: `<hotel>_<room id>.json`.
    pub fn file_name(&self) -> String {
        let safe: String = self.hotel.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        format!("{safe}_{}.json", self.room_id)
    }

    pub fn save(&self, folder: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
        let path = folder.join(self.file_name());
        let temporary = path.with_extension("json.part");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temporary, &path)?;
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<RoomCapture> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("{} is not a room capture", path.display()))
    }

    /// The door: the hotel's, or the guess of [`guess_door`].
    pub fn door_or_guess(&self) -> Option<Door> {
        self.door.or_else(|| guess_door(&self.heightmap))
    }
}

/// Every capture in `folder`, newest first.
pub fn list_captures(folder: &Path) -> Vec<(PathBuf, RoomCapture)> {
    let mut list: Vec<(PathBuf, RoomCapture, std::time::SystemTime)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            RoomCapture::load(&p).ok().map(|c| (p, c, modified))
        })
        .collect();
    list.sort_by_key(|entry| std::cmp::Reverse(entry.2));
    list.into_iter().map(|(p, c, _)| (p, c)).collect()
}

/// The rows of a heightmap (`\r` or `\n` separated), without empty ones.
pub fn rows(heightmap: &str) -> Vec<Vec<char>> {
    heightmap.split(['\r', '\n']).filter(|r| !r.is_empty()).map(|r| r.chars().collect()).collect()
}

fn walkable(c: char) -> bool {
    c.is_ascii_alphanumeric() && !c.eq_ignore_ascii_case(&'x')
}

/// A door for a room whose door tile the hotel did not send: the one open tile of the left column (facing east), else
/// of the top row (facing south), else the first open tile.
pub fn guess_door(heightmap: &str) -> Option<Door> {
    let rows = rows(heightmap);
    let column: Vec<i32> = rows.iter().enumerate().filter(|(_, r)| r.first().copied().is_some_and(walkable)).map(|(y, _)| y as i32).collect();
    if column.len() == 1 {
        return Some(Door { x: 0, y: column[0], dir: 2 });
    }
    if let Some(top) = rows.first() {
        let open: Vec<i32> = top.iter().enumerate().filter(|(_, c)| walkable(**c)).map(|(x, _)| x as i32).collect();
        if open.len() == 1 {
            return Some(Door { x: open[0], y: 0, dir: 4 });
        }
    }
    rows.iter().enumerate().find_map(|(y, r)| r.iter().position(|c| walkable(*c)).map(|x| Door { x: x as i32, y: y as i32, dir: 2 }))
}

// ---------------------------------------------------------------------------------------------- the session

/// How to capture.
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub folder: PathBuf,
    /// The hotel's furnidata (URL or file) for the class names; official Habbo finds its own.
    pub furnidata_url: Option<String>,
    /// The pause between two wired boxes.
    pub wired_delay_ms: u64,
    /// How long one box may take to answer before the hotel is taken not to let the user read it.
    pub wired_timeout_ms: u64,
    pub ports: Vec<u16>,
    pub gearth_folder: Option<PathBuf>,
    /// Where packet recordings go.
    pub packets_folder: PathBuf,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            folder: PathBuf::from("Rooms"),
            packets_folder: PathBuf::from("Packets"),
            furnidata_url: None,
            wired_delay_ms: 400,
            wired_timeout_ms: 4000,
            ports: crate::gearth::default_ports(),
            gearth_folder: None,
        }
    }
}

/// What the page asks a running capture to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open the wired boxes of the room the user is in; `double_click`: UseFurniture may be used where Open is unknown
    /// or gets no answer (the boxes blink for everyone in the room).
    ReadWired { double_click: bool },
    /// Stop reading the wired.
    StopReading,
    /// For developers: record every packet, both ways, from the next room the user enters (true), or stop (false).
    RecordPackets(bool),
}

/// What a running capture reports.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    Connected { host: String, client_type: String, profile: String },
    /// A room was entered or changed; saved in `path`.
    Room { path: PathBuf, summary: CaptureSummary },
    /// Reading the wired: done of total.
    WiredProgress { done: usize, total: usize },
    WiredDone { read: usize, total: usize, no_rights: bool },
    /// The packet recording changed.
    Recording(Recording),
}

/// Where the packet recording is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recording {
    /// It starts at the next room entered.
    Waiting,
    Running { packets: usize, wired: usize },
    /// Saved in this folder.
    Finished(PathBuf),
    /// Not recording (it could not start, or was stopped before a room).
    Off,
}

pub type CaptureSink<'a> = &'a (dyn Fn(CaptureEvent) + Send + Sync);

/// The extension as G-Earth lists it.
pub fn extension_info() -> ExtensionInfo {
    ExtensionInfo {
        title: "All-in-1 Room ripper".into(),
        description: "Copies the rooms you enter (floor plan, furni, wired) for the All-in-1 Manager.".into(),
        ..ExtensionInfo::default()
    }
}

/// Reading the wired boxes of a room, one at a time.
struct WiredReader {
    queue: Vec<i32>,
    total: usize,
    read: usize,
    /// The box asked last, when, and whether UseFurniture was used for it.
    waiting: Option<(i32, Instant, bool)>,
    next_at: Instant,
    /// Open got no answer, UseFurniture is used from now on.
    use_furniture: bool,
    /// The user allowed UseFurniture.
    double_click: bool,
    /// Something answered: the hotel lets the user read the wired.
    answered: bool,
}

struct Capturer<'a> {
    options: &'a CaptureOptions,
    headers: RoomHeaders,
    furnidata: Option<Furnidata>,
    connection_host: String,
    hotel: String,
    client_type: String,
    profile: String,
    official: bool,
    room: Option<RoomCapture>,
    dirty: bool,
    last_save: Instant,
    reader: Option<WiredReader>,
    log: Log<'a>,
    sink: CaptureSink<'a>,
    names: Names,
    hotel_version: String,
    /// A recording was asked for: it starts at the next room entered.
    record_wanted: bool,
    recording: Option<PacketLog>,
}

impl Capturer<'_> {
    fn start_recording(&mut self, session: &Session) {
        let readme = packet_log::readme(&self.connection_host, &self.client_type, &self.hotel_version, &self.profile, &self.names);
        match PacketLog::start(&self.options.packets_folder, &self.hotel, &readme) {
            Ok(recording) => {
                session.watch_all(true);
                (self.log)(&format!("Recording every packet into {} ...", recording.folder.display()));
                self.recording = Some(recording);
                (self.sink)(CaptureEvent::Recording(Recording::Running { packets: 0, wired: 0 }));
            }
            Err(e) => {
                self.record_wanted = false;
                (self.log)(&format!("The packet recording could not start: {e:#}"));
                (self.sink)(CaptureEvent::Recording(Recording::Off));
            }
        }
    }

    fn stop_recording(&mut self, session: &Session) {
        self.record_wanted = false;
        let Some(recording) = self.recording.take() else {
            (self.sink)(CaptureEvent::Recording(Recording::Off));
            return;
        };
        session.watch_all(false);
        let (packets, wired) = (recording.packets, recording.wired_packets);
        match recording.finish() {
            Ok(folder) => {
                (self.log)(&format!("Packet recording saved: {packets} packets ({wired} wired) in {}.", folder.display()));
                (self.sink)(CaptureEvent::Recording(Recording::Finished(folder)));
            }
            Err(e) => {
                (self.log)(&format!("The packet recording could not be finished: {e:#}"));
                (self.sink)(CaptureEvent::Recording(Recording::Off));
            }
        }
    }

    /// Adds a packet to the recording; stops it when it is full.
    fn record(&mut self, to_server: bool, header: u16, body: &[u8], session: &Session) {
        let Some(recording) = &mut self.recording else { return };
        let room = recording.add(to_server, header, body, &self.names);
        let (packets, wired) = (recording.packets, recording.wired_packets);
        if !room {
            (self.log)(&format!("The recording is full ({} packets): it stops here.", packet_log::MAX_PACKETS));
            self.stop_recording(session);
        } else if packets % 200 == 0 {
            (self.sink)(CaptureEvent::Recording(Recording::Running { packets, wired }));
        }
    }

    fn class_of(&self, type_id: i32, wall: bool) -> Option<String> {
        let data = self.furnidata.as_ref()?;
        if wall { data.wall.get(&type_id) } else { data.floor.get(&type_id) }.cloned()
    }

    fn name_floor(&self, mut item: FloorItem) -> FloorItem {
        if item.class_name.is_none() {
            item.class_name = self.class_of(item.type_id, false);
        }
        item
    }

    fn name_wall(&self, mut item: WallItem) -> WallItem {
        if item.class_name.is_none() {
            item.class_name = self.class_of(item.type_id, true);
        }
        item
    }

    fn save(&mut self) {
        let Some(room) = &mut self.room else { return };
        if let Some(furnidata) = &self.furnidata {
            room.rare_classes = furnidata_rares(room, furnidata);
            room.rares_checked = true;
        }
        match room.save(&self.options.folder) {
            Ok(path) => (self.sink)(CaptureEvent::Room { path, summary: room.summary() }),
            Err(e) => (self.log)(&format!("The capture of {} could not be saved: {e:#}", room.name())),
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    fn problem(&mut self, what: &str, error: anyhow::Error) {
        let text = format!("{what}: {error:#}");
        (self.log)(&format!("  A packet could not be read ({text})."));
        if let Some(room) = &mut self.room {
            if room.problems.len() < 50 {
                room.problems.push(text);
            }
        }
    }

    fn new_room(&mut self, model: String, room_id: i32) {
        if self.dirty {
            self.save();
        }
        self.reader = None;
        // A room entered again starts from what was captured before (its wired stays read).
        let previous = self.room.take().filter(|r| r.room_id == room_id);
        let wired = previous.map(|r| r.wired).unwrap_or_default();
        self.room = Some(RoomCapture {
            version: CAPTURE_VERSION,
            host: self.connection_host.clone(),
            hotel: self.hotel.clone(),
            client_type: self.client_type.clone(),
            profile: self.profile.clone(),
            official: self.official,
            captured_at: chrono::Local::now().to_rfc3339(),
            room_id,
            model_name: model,
            floor: "0.0".into(),
            wallpaper: "0.0".into(),
            landscape: "0.0".into(),
            wired,
            ..RoomCapture::default()
        });
        (self.log)(&format!("Entered room {room_id}: capturing it."));
        self.dirty = true;
    }

    /// Handles an intercepted packet from the hotel.
    fn packet(&mut self, key: &str, body: &[u8]) {
        if key == p::ROOM_READY.key {
            match p::parse_room_ready(body) {
                // Nothing is sent: a packet sent with an id of another client version can make the hotel close
                // the connection. The door comes from RoomEntryTile when the client asks for it (the floor plan
                // editor), else it is guessed and shown for the user to check.
                Ok((model, room_id)) => self.new_room(model, room_id),
                Err(e) => self.problem("RoomReady", e),
            }
            return;
        }
        if self.room.is_none() {
            // The room info of a room seen in the navigator, before entering one.
            return;
        }
        let result: Result<()> = (|| {
            match key {
                k if k == p::FLOOR_HEIGHT_MAP.key => {
                    let (_, wall_height, heightmap) = p::parse_floor_height_map(body)?;
                    let room = self.room.as_mut().unwrap();
                    room.heightmap = heightmap;
                    room.wall_height = wall_height;
                }
                k if k == p::ROOM_VISUALIZATION.key => self.room.as_mut().unwrap().visualization = Some(p::parse_visualization(body)?),
                k if k == p::ROOM_ENTRY_TILE.key => {
                    let room = self.room.as_mut().unwrap();
                    room.door = Some(p::parse_entry_tile(body)?);
                    room.door_from_hotel = true;
                }
                k if k == p::ROOM_PROPERTY.key => {
                    let (property, value) = p::parse_property(body)?;
                    let room = self.room.as_mut().unwrap();
                    match property.as_str() {
                        "floor" => room.floor = value,
                        "wallpaper" => room.wallpaper = value,
                        "landscape" => room.landscape = value,
                        _ => {}
                    }
                }
                k if k == p::OBJECTS.key => {
                    let (owners, items) = p::parse_objects(body)?;
                    let items: Vec<FloorItem> = items.into_iter().map(|i| self.name_floor(i)).collect();
                    let room = self.room.as_mut().unwrap();
                    room.owners.extend(owners);
                    for item in items {
                        room.floor_items.insert(item.id, item);
                    }
                }
                k if k == p::OBJECT_ADD.key || k == p::OBJECT_UPDATE.key => {
                    let item = self.name_floor(p::parse_object(body)?);
                    self.room.as_mut().unwrap().floor_items.insert(item.id, item);
                }
                k if k == p::OBJECT_REMOVE.key => {
                    let id = p::parse_remove(body)?;
                    self.room.as_mut().unwrap().floor_items.remove(&id);
                }
                k if k == p::OBJECT_REMOVE_MULTIPLE.key => {
                    let room = self.room.as_mut().unwrap();
                    for id in p::parse_remove_multiple(body)? {
                        room.floor_items.remove(&id);
                    }
                }
                k if k == p::OBJECT_DATA_UPDATE.key => {
                    let (id, stuff, limited) = p::parse_object_data(body)?;
                    if let Some(item) = self.room.as_mut().unwrap().floor_items.get_mut(&id) {
                        item.stuff = stuff;
                        item.limited = limited.or(item.limited);
                    }
                }
                k if k == p::OBJECTS_DATA_UPDATE.key => {
                    let room = self.room.as_mut().unwrap();
                    for (id, stuff, limited) in p::parse_objects_data(body)? {
                        if let Some(item) = room.floor_items.get_mut(&id) {
                            item.stuff = stuff;
                            item.limited = limited.or(item.limited);
                        }
                    }
                }
                k if k == p::ITEMS.key => {
                    let (owners, items) = p::parse_items(body)?;
                    let items: Vec<WallItem> = items.into_iter().map(|i| self.name_wall(i)).collect();
                    let room = self.room.as_mut().unwrap();
                    room.owners.extend(owners);
                    for item in items {
                        room.wall_items.insert(item.id, item);
                    }
                }
                k if k == p::ITEM_ADD.key || k == p::ITEM_UPDATE.key => {
                    let item = self.name_wall(p::parse_item(body)?);
                    self.room.as_mut().unwrap().wall_items.insert(item.id, item);
                }
                k if k == p::ITEM_REMOVE.key => {
                    let id = p::parse_remove(body)?;
                    self.room.as_mut().unwrap().wall_items.remove(&id);
                }
                k if k == p::ITEMS_STATE_UPDATE.key => {
                    let room = self.room.as_mut().unwrap();
                    for (id, state) in p::parse_items_state(body)? {
                        if let Some(item) = room.wall_items.get_mut(&id) {
                            item.data = state;
                        }
                    }
                }
                k if k == p::ROOM_INFO.key => {
                    let info = p::parse_room_info(body)?;
                    let room = self.room.as_mut().unwrap();
                    if info.room_id == room.room_id {
                        room.info = Some(info);
                    }
                }
                k if k == p::WIRED_ROOM_SETTINGS.key || k == p::WIRED_PERMISSIONS.key => {
                    self.room.as_mut().unwrap().can_read_wired = Some(p::parse_wired_rights(k, body)?);
                }
                k => {
                    let kind = match k {
                        _ if k == p::WIRED_TRIGGER.key => WiredKind::Trigger,
                        _ if k == p::WIRED_ACTION.key => WiredKind::Action,
                        _ if k == p::WIRED_CONDITION.key => WiredKind::Condition,
                        _ if k == p::WIRED_ADDON.key => WiredKind::Addon,
                        _ if k == p::WIRED_SELECTOR.key => WiredKind::Selector,
                        _ if k == p::WIRED_VARIABLE.key => WiredKind::Variable,
                        _ => return Ok(()),
                    };
                    let settings = p::parse_wired(body, kind)?;
                    let id = settings.box_id;
                    self.room.as_mut().unwrap().wired.insert(id, settings);
                    if let Some(reader) = &mut self.reader {
                        if reader.waiting.is_some_and(|(asked, _, _)| asked == id) {
                            reader.waiting = None;
                            reader.read += 1;
                            reader.answered = true;
                            (self.sink)(CaptureEvent::WiredProgress { done: reader.total - reader.queue.len(), total: reader.total });
                        }
                    }
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.dirty = true,
            Err(e) => self.problem(key, e),
        }
    }

    fn start_reading(&mut self, double_click: bool) {
        let Some(room) = &self.room else {
            (self.log)("Enter a room first: the wired is read in the room you are in.");
            return;
        };
        if room.can_read_wired == Some(false) {
            (self.log)("The hotel says you may not read the wired in this room (ask its owner for rights, or to let everyone inspect the wired for a moment).");
            (self.sink)(CaptureEvent::WiredDone { read: 0, total: 0, no_rights: true });
            return;
        }
        let mut queue: Vec<i32> = room.wired_box_ids().into_iter().filter(|id| !room.wired.contains_key(id)).collect();
        if queue.is_empty() {
            let known = room.wired_box_ids().len();
            if known == 0 && self.furnidata.is_none() {
                (self.log)("Without the hotel's furnidata the wired boxes cannot be told apart from other furni: fill in its furnidata URL and capture again.");
            } else {
                (self.log)(&format!("Nothing to read: {} of {known} wired boxes are read already.", room.wired.len()));
            }
            (self.sink)(CaptureEvent::WiredDone { read: room.wired.len(), total: known, no_rights: false });
            return;
        }
        let open = self.headers.out(&p::WIRED_OPEN).is_some();
        if !open && !(double_click && self.headers.out(&p::USE_FURNITURE).is_some()) {
            if self.headers.out(&p::USE_FURNITURE).is_some() {
                (self.log)("The id of Open is not known for this hotel: open the boxes by hand (double click), or allow the double click for reading (the boxes blink for everyone in the room).");
            } else {
                (self.log)("The id of the packet that opens a wired box is not known for this hotel: open the boxes by hand (double click), the capture keeps what they show.");
            }
            (self.sink)(CaptureEvent::WiredDone { read: 0, total: 0, no_rights: false });
            return;
        }
        queue.reverse();
        let total = queue.len();
        (self.log)(&format!("Reading {total} wired boxes, one every {} ms ...", self.options.wired_delay_ms));
        self.reader = Some(WiredReader {
            queue,
            total,
            read: 0,
            waiting: None,
            next_at: Instant::now(),
            use_furniture: !open,
            double_click,
            answered: false,
        });
        (self.sink)(CaptureEvent::WiredProgress { done: 0, total });
    }

    /// Asks the next box when it is time; gives up on a box that does not answer.
    fn step_reading(&mut self, session: &Session) {
        if let Some(packet) = self.next_request(Instant::now()) {
            let _ = session.send_to_server(&packet);
            // G-Earth does not show an extension its own packets: the recording gets them here.
            self.record(true, packet.header, &packet.body, session);
        }
    }

    /// The packet that asks the next box, when it is time.
    fn next_request(&mut self, now: Instant) -> Option<Packet> {
        let reader = self.reader.as_mut()?;
        let timeout = Duration::from_millis(self.options.wired_timeout_ms);
        if let Some((id, asked, by_use)) = reader.waiting {
            if now.duration_since(asked) < timeout {
                return None;
            }
            reader.waiting = None;
            if !reader.answered {
                if !by_use && reader.double_click && self.headers.out(&p::USE_FURNITURE).is_some() {
                    (self.log)("Open got no answer: trying a double click (the box's light flips for everyone in the room while it is read).");
                    reader.use_furniture = true;
                    reader.queue.push(id);
                } else if !by_use && self.headers.out(&p::USE_FURNITURE).is_some() {
                    // Not taken as "no rights": the hotel may only lack Open, and the user may allow the double click.
                    (self.log)("Open got no answer: the hotel may not let you read the wired in this room, or may not know Open. Ask the room's owner for rights, or allow the double click for reading (the boxes blink for everyone in the room) and read again.");
                    self.reader = None;
                    (self.sink)(CaptureEvent::WiredDone { read: 0, total: 0, no_rights: true });
                    return None;
                } else {
                    (self.log)("The first wired box did not answer: the hotel does not let you read the wired in this room. Ask its owner for rights, or to let everyone inspect the wired for a moment.");
                    if let Some(room) = &mut self.room {
                        room.can_read_wired = Some(false);
                    }
                    self.reader = None;
                    self.dirty = true;
                    (self.sink)(CaptureEvent::WiredDone { read: 0, total: 0, no_rights: true });
                    return None;
                }
            } else {
                (self.log)(&format!("  Box {id} did not answer; skipped."));
            }
        }
        if now < reader.next_at {
            return None;
        }
        let Some(id) = reader.queue.pop() else {
            let (read, total) = (reader.read, reader.total);
            (self.log)(&format!("The wired is read: {read} of {total} boxes."));
            self.reader = None;
            self.dirty = true;
            (self.sink)(CaptureEvent::WiredDone { read, total, no_rights: false });
            return None;
        };
        let by_use = reader.use_furniture;
        reader.waiting = Some((id, now, by_use));
        reader.next_at = now + Duration::from_millis(self.options.wired_delay_ms);
        let header = if by_use { self.headers.out(&p::USE_FURNITURE) } else { self.headers.out(&p::WIRED_OPEN) };
        header.map(|header| p::open_box(header, by_use, id))
    }
}

/// Captures until `cancel`: connects to G-Earth, waits for the hotel, then records every room entered. `commands`
/// asks for the wired to be read.
pub async fn capture(
    options: &CaptureOptions,
    choice: ProfileChoice,
    log: Log<'_>,
    ask: Ask<'_>,
    sink: CaptureSink<'_>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    cancel: &CancellationToken,
) -> Result<()> {
    let rip_options = RipOptions {
        ports: options.ports.clone(),
        gearth_folder: options.gearth_folder.clone(),
        extension: extension_info(),
        ..RipOptions::default()
    };
    let progress = |_| {};
    let hooks = RipHooks::new(log, ask, &progress);
    log("Start G-Earth, connect it to the hotel and log in with your own client: every room you enter is captured.");
    let (mut session, connection) = crate::rip::connect_to_hotel(&rip_options, &hooks, cancel).await?;
    // The Arcturus/Nitro ids are only certain for a profile chosen by hand, found by the hotel's host or client
    // revision, or for the Nitro client. A Flash retro of another revision matched by its client type alone has
    // other ids: the capture then only uses ids its profile or G-Earth gives, and sends nothing.
    let certain = match &choice {
        ProfileChoice::Fixed(_) => true,
        ProfileChoice::Detect(list) => crate::profiles::detect(list, &connection).is_some_and(|(profile, by)| {
            profile.kind == HotelKind::Official || by != crate::profiles::DetectedBy::ClientType || connection.client_type.eq_ignore_ascii_case("NITRO")
        }),
    };
    let profile: HotelProfile = crate::rip::choose_profile(choice, &connection, &hooks);
    let official = profile.kind == HotelKind::Official;
    let retro_ids = !official && certain && profile.headers.is_complete();
    let headers = RoomHeaders::resolve(&profile.extra_headers, &connection.packets, retro_ids);
    if !official && !retro_ids {
        log(&format!(
            "The room packet ids of this client ({} {}) are not certain: choose a hotel profile for it (with its ids in extra_headers) to capture here.",
            connection.client_type, connection.hotel_version
        ));
    }
    let missing = headers.missing_required();
    if !missing.is_empty() {
        anyhow::bail!(
            "The header ids of {} are not known for this hotel ({} client). Official Habbo needs G-Earth's packet info; a retro needs a profile with these ids in its extra_headers.",
            missing.join(", "),
            connection.client_type
        );
    }
    for header in headers.incoming.values() {
        session.watch(false, *header);
    }
    let furnidata_source = options
        .furnidata_url
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| profile.furnidata_url.clone().filter(|s| !s.trim().is_empty()))
        .or_else(|| official.then(|| crate::furnidata::official_url(&connection.host)).flatten());
    let furnidata = match furnidata_source {
        Some(source) => match crate::furnidata::load(&source).await {
            Ok(data) => {
                log(&format!("Furnidata: {} class names ({source}).", data.len()));
                Some(data)
            }
            Err(e) => {
                log(&format!("The furnidata could not be read ({e:#}): furni are matched by sprite id, and the wired boxes are only read when you open them."));
                None
            }
        },
        None => {
            log("No furnidata for this hotel: furni are matched by sprite id, and the wired boxes are only read when you open them by hand.");
            None
        }
    };
    let hotel = crate::rip::hotel_name(&connection.host);
    sink(CaptureEvent::Connected { host: connection.host.clone(), client_type: connection.client_type.clone(), profile: profile.name.clone() });
    log(&format!("Connected to {} ({} client, profile {}). Enter a room.", connection.host, connection.client_type, profile.name));
    let headers_for_names = headers.clone();
    let mut capturer = Capturer {
        options,
        headers,
        furnidata,
        connection_host: connection.host.clone(),
        hotel,
        client_type: connection.client_type.clone(),
        profile: profile.name.clone(),
        official,
        room: None,
        dirty: false,
        last_save: Instant::now(),
        reader: None,
        log,
        sink,
        names: Names::new(&connection.packets, &headers_for_names, retro_ids),
        hotel_version: connection.hotel_version.clone(),
        record_wanted: false,
        recording: None,
    };
    let mut commands_open = true;
    let result = loop {
        let tick = tokio::time::sleep(Duration::from_millis(100));
        tokio::select! {
            _ = cancel.cancelled() => break Err(Cancelled.into()),
            command = commands.recv(), if commands_open => match command {
                Some(Command::ReadWired { double_click }) => capturer.start_reading(double_click),
                Some(Command::StopReading) => {
                    if capturer.reader.take().is_some() {
                        log("Reading the wired stopped.");
                    }
                }
                Some(Command::RecordPackets(true)) => {
                    if capturer.recording.is_none() && !capturer.record_wanted {
                        capturer.record_wanted = true;
                        log("The packet recording starts in the next room you enter (so your login is not in it): enter or reload a room.");
                        (capturer.sink)(CaptureEvent::Recording(Recording::Waiting));
                    }
                }
                Some(Command::RecordPackets(false)) => capturer.stop_recording(&session),
                // The page let go of the capture: no more commands.
                None => commands_open = false,
            },
            event = session.next_event() => match event {
                Some(Event::Packet(packet)) => {
                    let key = if packet.to_server { None } else { capturer.headers.incoming_key(packet.packet.header).map(str::to_string) };
                    if capturer.record_wanted && capturer.recording.is_none() && key.as_deref() == Some(p::ROOM_READY.key) {
                        capturer.start_recording(&session);
                    }
                    capturer.record(packet.to_server, packet.packet.header, &packet.packet.body, &session);
                    if let Some(key) = key {
                        capturer.packet(&key, &packet.packet.body);
                    }
                }
                Some(Event::ConnectionEnd) => {
                    log("The hotel connection ended: log in again through G-Earth to go on capturing.");
                    if capturer.dirty {
                        capturer.save();
                    }
                }
                Some(Event::Closed(reason)) => break Err(anyhow::anyhow!("G-Earth closed the connection ({reason}).")),
                None => break Err(anyhow::anyhow!("The connection to G-Earth ended.")),
                Some(_) => {}
            },
            _ = tick => {}
        }
        capturer.step_reading(&session);
        if capturer.dirty && capturer.last_save.elapsed() >= Duration::from_secs(2) {
            capturer.save();
        }
    };
    if capturer.dirty {
        capturer.save();
    }
    if capturer.recording.is_some() {
        capturer.stop_recording(&session);
    }
    match result {
        Err(e) if e.is::<Cancelled>() => {
            log("Capture stopped.");
            Ok(())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_furnidata_rares_in_the_room_are_kept() {
        let data = Furnidata::parse(
            r#"{"roomitemtypes":{"furnitype":[{"id":10,"classname":"Rare_Dragon","rare":true},{"id":11,"classname":"chair","rare":false},{"id":12,"classname":"rare_elsewhere","rare":true}]},
                "wallitemtypes":{"furnitype":[{"id":20,"classname":"wall_rare","rare":"1"},{"id":21,"classname":"poster","rare":0}]}}"#,
        )
        .unwrap();
        let mut room = RoomCapture::default();
        for (id, type_id) in [(1, 10), (2, 11)] {
            room.floor_items.insert(id, FloorItem { id, type_id, class_name: None, x: 0, y: 0, z: 0.0, dir: 0, stuff: p::StuffData::Empty, limited: None, owner_id: 1 });
        }
        for (id, type_id) in [(3, 20), (4, 21)] {
            room.wall_items.insert(id, WallItem { id, type_id, class_name: None, location: String::new(), data: String::new(), owner_id: 1 });
        }
        assert_eq!(furnidata_rares(&room, &data), BTreeSet::from(["rare_dragon".to_string(), "wall_rare".to_string()]));
    }

    #[test]
    fn the_door_is_guessed_from_the_heightmap() {
        // model_a style: the door is the one open tile of the left column.
        assert_eq!(guess_door("xxxx\rx000\r0000\rx000"), Some(Door { x: 0, y: 2, dir: 2 }));
        assert_eq!(guess_door("xx0x\rx000\rx000"), Some(Door { x: 2, y: 0, dir: 4 }));
        assert_eq!(guess_door("xxx\rx00\rx00"), Some(Door { x: 1, y: 1, dir: 2 }));
        assert_eq!(guess_door("xxx"), None);
    }

    /// A capturer in a room with two wired boxes, its events kept in `events`.
    fn reading<'a>(options: &'a CaptureOptions, sink: CaptureSink<'a>, headers: RoomHeaders) -> Capturer<'a> {
        let mut room = RoomCapture { room_id: 1, ..RoomCapture::default() };
        for id in [5, 6] {
            let item = FloorItem { id, type_id: 1, class_name: Some("wf_act_toggle_state".into()), x: 0, y: 0, z: 0.0, dir: 0, stuff: p::StuffData::Empty, limited: None, owner_id: 1 };
            room.floor_items.insert(id, item);
        }
        Capturer {
            options,
            names: Names::new(&[], &headers, true),
            headers,
            furnidata: None,
            connection_host: String::new(),
            hotel: String::new(),
            client_type: String::new(),
            profile: String::new(),
            official: false,
            room: Some(room),
            dirty: false,
            last_save: Instant::now(),
            reader: None,
            log: &|_| {},
            sink,
            hotel_version: String::new(),
            record_wanted: false,
            recording: None,
        }
    }

    #[test]
    fn reading_the_wired_sends_only_open_unless_the_double_click_is_allowed() {
        let options = CaptureOptions::default();
        let events = std::sync::Mutex::new(Vec::new());
        let sink = |e: CaptureEvent| events.lock().unwrap().push(e);
        let retro = RoomHeaders::resolve(&BTreeMap::new(), &[], true);
        let late = |c: &Capturer<'_>| Instant::now() + Duration::from_millis(c.options.wired_timeout_ms + 1);

        // By default: Open, and when it gets no answer the reading stops; UseFurniture is never sent.
        let mut c = reading(&options, &sink, retro.clone());
        c.start_reading(false);
        let packet = c.next_request(Instant::now()).unwrap();
        assert_eq!((packet.header, packet.body.len()), (768, 4));
        assert!(c.next_request(late(&c)).is_none());
        assert!(c.reader.is_none());
        // Not marked "no rights": reading again with the double click allowed must stay possible.
        assert_eq!(c.room.as_ref().unwrap().can_read_wired, None);
        assert!(matches!(events.lock().unwrap().last(), Some(CaptureEvent::WiredDone { no_rights: true, .. })));

        // Allowed: the same box again with UseFurniture.
        let mut c = reading(&options, &sink, retro.clone());
        c.start_reading(true);
        assert_eq!(c.next_request(Instant::now()).unwrap().header, 768);
        let packet = c.next_request(late(&c)).unwrap();
        assert_eq!((packet.header, packet.body.len()), (99, 8));

        // A hotel without Open: nothing is sent unless allowed.
        let mut no_open = retro.clone();
        no_open.outgoing.remove(p::WIRED_OPEN.key);
        let mut c = reading(&options, &sink, no_open.clone());
        c.start_reading(false);
        assert!(c.reader.is_none() && c.next_request(Instant::now()).is_none());
        let mut c = reading(&options, &sink, no_open);
        c.start_reading(true);
        assert_eq!(c.next_request(Instant::now()).unwrap().header, 99);
    }

    #[test]
    fn captures_round_trip_and_name_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let capture = RoomCapture { hotel: "habbo.nl".into(), room_id: 42, heightmap: "x0\r00".into(), ..RoomCapture::default() };
        let path = capture.save(dir.path()).unwrap();
        assert!(path.ends_with("habbo.nl_42.json"));
        assert_eq!(RoomCapture::load(&path).unwrap(), capture);
        assert_eq!(list_captures(dir.path()).len(), 1);
        assert_eq!(capture.name(), "Room 42");
    }
}
