//! A recording of every packet between the client and the hotel, both ways, for developers who build the wired (or
//! anything else) into an emulator: what the official client sends when a box is opened, saved or triggered, and
//! what the hotel answers.
//!
//! A recording starts at the next room the user enters, so the login (with its SSO ticket) is never in it. It
//! writes, in its own folder:
//! * `packets.jsonl`: one line per packet: time in ms since the start, direction, header id, name (G-Earth's packet
//!   info, or the Room ripper's own names), length and the body in hex;
//! * `wired.txt`: the wired packets only (box settings, the save packets, opening a box, the box lights), readable:
//!   the settings decoded where the Room ripper can, and a hex dump with offsets;
//! * `README.txt`: the hotel, its client, the profile and the header ids.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;

use super::packets::{self as p, RoomHeaders, WiredKind};
use crate::gearth::PacketInfo;

/// The most packets one recording keeps (it stops itself after that).
pub const MAX_PACKETS: usize = 500_000;

/// The save packets of the wired editor (client to server), by name, and their Arcturus/Nitro ids.
const SAVE_PACKETS: &[(&str, u16)] = &[
    ("UpdateTrigger", 1520),
    ("UpdateAction", 2281),
    ("UpdateCondition", 3203),
    ("UpdateAddon", 1883),
    ("UpdateSelector", 3883),
    ("UpdateVariable", 2310),
];

#[derive(Serialize)]
struct Line<'a> {
    t: u128,
    dir: &'a str,
    header: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    length: usize,
    hex: String,
}

/// Packet names by direction and id.
pub struct Names {
    names: HashMap<(bool, u16), String>,
    /// Ids that belong to the wired.
    wired: HashMap<(bool, u16), Option<WiredKind>>,
}

impl Names {
    pub fn new(packets: &[PacketInfo], headers: &RoomHeaders, retro: bool) -> Names {
        let mut names = HashMap::new();
        for packet in packets {
            if let (Some(name), Ok(id)) = (&packet.name, u16::try_from(packet.header_id)) {
                names.insert((packet.to_server, id), name.clone());
            }
        }
        for (key, id) in &headers.incoming {
            names.entry((false, *id)).or_insert_with(|| key.clone());
        }
        for (key, id) in &headers.outgoing {
            names.entry((true, *id)).or_insert_with(|| key.clone());
        }
        if retro {
            for (name, id) in SAVE_PACKETS {
                names.entry((true, *id)).or_insert_with(|| name.to_string());
            }
        }
        let mut wired = HashMap::new();
        for (message, kind) in [
            (p::WIRED_TRIGGER, Some(WiredKind::Trigger)),
            (p::WIRED_ACTION, Some(WiredKind::Action)),
            (p::WIRED_CONDITION, Some(WiredKind::Condition)),
            (p::WIRED_ADDON, Some(WiredKind::Addon)),
            (p::WIRED_SELECTOR, Some(WiredKind::Selector)),
            (p::WIRED_VARIABLE, Some(WiredKind::Variable)),
        ] {
            if let Some(id) = headers.incoming.get(message.key) {
                wired.insert((false, *id), kind);
            }
        }
        // Every other packet whose name says wired, the save packets, and opening a box.
        for ((to_server, id), name) in &names {
            let lower = name.to_ascii_lowercase();
            let save = SAVE_PACKETS.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
            if lower.contains("wired") || save || (*to_server && (name == p::WIRED_OPEN.key || name == p::USE_FURNITURE.key)) {
                wired.entry((*to_server, *id)).or_insert(None);
            }
        }
        Names { names, wired }
    }

    pub fn name(&self, to_server: bool, header: u16) -> Option<&str> {
        self.names.get(&(to_server, header)).map(String::as_str)
    }

    fn wired(&self, to_server: bool, header: u16) -> Option<Option<WiredKind>> {
        self.wired.get(&(to_server, header)).copied()
    }
}

fn hex(body: &[u8]) -> String {
    body.iter().map(|b| format!("{b:02x}")).collect()
}

/// A hex dump with offsets and the printable characters, 16 bytes a line.
pub fn hex_dump(body: &[u8]) -> String {
    let mut out = String::new();
    for (row, chunk) in body.chunks(16).enumerate() {
        let bytes: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let text: String = chunk.iter().map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' }).collect();
        out.push_str(&format!("    {:06x}  {:<48}  {text}\n", row * 16, bytes.join(" ")));
    }
    out
}

/// A running recording.
pub struct PacketLog {
    pub folder: PathBuf,
    started: Instant,
    all: BufWriter<File>,
    wired: BufWriter<File>,
    pub packets: usize,
    pub wired_packets: usize,
}

impl PacketLog {
    /// Starts a recording in `<parent>/<hotel>_<time>/`, with its README.
    pub fn start(parent: &Path, hotel: &str, readme: &str) -> Result<PacketLog> {
        let safe: String = hotel.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        let folder = parent.join(format!("{safe}_{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
        std::fs::write(folder.join("README.txt"), readme)?;
        let all = BufWriter::new(File::create(folder.join("packets.jsonl"))?);
        let mut wired = BufWriter::new(File::create(folder.join("wired.txt"))?);
        writeln!(wired, "Wired packets of {hotel}: every packet of the wired editor and the wired boxes, both ways.\n")?;
        Ok(PacketLog { folder, started: Instant::now(), all, wired, packets: 0, wired_packets: 0 })
    }

    /// Writes one packet. Returns false once the recording is full.
    pub fn add(&mut self, to_server: bool, header: u16, body: &[u8], names: &Names) -> bool {
        let t = self.started.elapsed().as_millis();
        let name = names.name(to_server, header);
        let line = Line { t, dir: if to_server { "out" } else { "in" }, header, name, length: body.len(), hex: hex(body) };
        if let Ok(json) = serde_json::to_string(&line) {
            let _ = writeln!(self.all, "{json}");
        }
        self.packets += 1;
        if let Some(kind) = names.wired(to_server, header) {
            self.wired_packets += 1;
            let arrow = if to_server { "client -> hotel" } else { "hotel -> client" };
            let _ = writeln!(self.wired, "[{:>9.3} s] {arrow}  {header} {}  ({} bytes)", t as f64 / 1000.0, name.unwrap_or("?"), body.len());
            if let Some(kind) = kind {
                match p::parse_wired(body, kind) {
                    Ok(settings) => {
                        let _ = writeln!(
                            self.wired,
                            "    {kind:?} box {}: code {}, ints {:?}, text {:?}, furni {:?}, furni 2 {:?}, furni sources {:?}, user sources {:?}, variables {:?}, delay {}, quantifier {}",
                            settings.box_id,
                            settings.code,
                            settings.int_params,
                            settings.string_param,
                            settings.stuff_ids,
                            settings.stuff_ids2,
                            settings.furni_sources,
                            settings.user_sources,
                            settings.variable_ids,
                            settings.delay,
                            settings.quantifier
                        );
                    }
                    Err(e) => {
                        let _ = writeln!(self.wired, "    (not decoded: {e:#})");
                    }
                }
            }
            let _ = write!(self.wired, "{}", hex_dump(body));
            let _ = writeln!(self.wired);
        }
        self.packets < MAX_PACKETS
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        writeln!(self.wired, "\n{} packets recorded, {} of them wired.", self.packets, self.wired_packets)?;
        self.all.flush()?;
        self.wired.flush()?;
        Ok(self.folder)
    }
}

/// The README of a recording.
pub fn readme(host: &str, client_type: &str, hotel_version: &str, profile: &str, names: &Names) -> String {
    let mut ids: Vec<(&(bool, u16), &String)> = names.names.iter().collect();
    ids.sort_by_key(|((out, id), _)| (!*out, *id));
    let mut text = format!(
        "Packet recording of the All-in-1 Manager's Room ripper\n\n\
         Hotel:    {host}\nClient:   {client_type} {hotel_version}\nProfile:  {profile}\nStarted:  {}\n\n\
         The recording starts at the first room entered after it was switched on, so the login is not in it. It still\n\
         holds everything the client and the hotel sent in those rooms (chat too): share it only with people you trust.\n\n\
         packets.jsonl  one packet a line: t (ms since the start), dir (out = client to hotel, in = hotel to client),\n\
         \x20              header, name (when known), length, hex (the body, after the header).\n\
         wired.txt      the wired packets only, decoded where possible, with a hex dump.\n\n\
         Wire format: int = 4 bytes big endian, short = 2, bool = 1 byte, string = short length + UTF-8.\n\n\
         Known packet names (direction, id, name):\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    for ((out, id), name) in ids {
        text.push_str(&format!("  {} {id:>5}  {name}\n", if *out { "out" } else { "in " }));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Writer;

    #[test]
    fn records_every_packet_and_the_wired_ones_readably() {
        let dir = tempfile::tempdir().unwrap();
        let headers = RoomHeaders::resolve(&Default::default(), &[], true);
        let names = Names::new(&[], &headers, true);
        let mut log = PacketLog::start(dir.path(), "myretro.net", &readme("game.myretro.net", "NITRO", "1", "Nitro", &names)).unwrap();
        log.add(true, 2281, &Writer::new(1).i32(12).i32(0).packet().body, &names);
        log.add(false, 1778, &[0, 0, 0, 0], &names);
        log.add(true, 4000, b"hello", &names);
        let folder = log.finish().unwrap();
        let all = std::fs::read_to_string(folder.join("packets.jsonl")).unwrap();
        assert_eq!(all.lines().count(), 3);
        assert!(all.contains(r#""dir":"out","header":2281,"name":"UpdateAction""#), "{all}");
        let wired = std::fs::read_to_string(folder.join("wired.txt")).unwrap();
        assert!(wired.contains("client -> hotel  2281 UpdateAction") && !wired.contains("1778"), "{wired}");
        assert!(wired.contains("1 of them wired") || wired.contains("3 packets recorded, 1 of them wired"));
        let readme = std::fs::read_to_string(folder.join("README.txt")).unwrap();
        assert!(readme.contains("in   1778  Objects") && readme.contains("login is not in it"));
        assert_eq!(hex_dump(b"AB"), "    000000  41 42                                             AB\n");
    }
}
