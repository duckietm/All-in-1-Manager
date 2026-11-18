//! A rip from a G-Earth Packet Logger export, without a live session (the built-in's
//! `gearth_log.rs`): someone who browsed the catalogue with the Packet Logger open saves it
//! ("Export all", a .txt of the logger's text) and the catalogue is rebuilt from the packets in
//! it ([`crate::job::Job::import_log`]).
//!
//! The export is the logger's text area (G-Earth `UiLoggerController`): entries separated by a
//! line of dashes; in each, an optional `(timestamp: ..)` line, an optional message info line
//! (`[name][hash][id]`), maybe `[Blocked]` / `[Replaced]`, then `Incoming[id] -> <packet>` or
//! `Outgoing[id] -> <packet>`, where the packet is G-Earth's text form (bytes below 32, above
//! 160, `[ ] { }` and 127 as `[n]`, everything else as Latin-1 characters) or, in the raw hex
//! view, hex. The hexdump view cannot be read back.
//!
//! The header ids come from the info lines when the logger showed them, else they are learned
//! from the packets' shapes ([`crate::learn`]).
//!
//! The built-in read the whole file and kept every packet of it; [`LogScan`] reads it in pieces
//! through a [`Source`], three times (the names on the info lines, then the packets up to where
//! the ids are known, then the catalogue's packets), and keeps only the catalogue's packets: the
//! same result, with a log of any size.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::conn::PacketInfo;
use crate::learn::Learner;
use crate::packets::{self, Headers};
use crate::profiles::HeaderMap;
use crate::source::Source;
use crate::wire::Packet;

/// One packet of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedPacket {
    pub to_server: bool,
    pub packet: Packet,
    /// The message name from the info line, when the logger showed it.
    pub name: Option<String>,
}

/// G-Earth's packet text back to bytes (`[n]` is the byte n; any other character its Latin-1
/// byte), or hex.
pub fn decode_packet_text(text: &str) -> Vec<u8> {
    let text = text.trim_end_matches(['\r', '\n']);
    let hex = text.len() >= 12
        && text.len().is_multiple_of(2)
        && text.chars().all(|c| c.is_ascii_hexdigit());
    if hex {
        return (0..text.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
            .collect();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(len) = chars[i + 1..].iter().take(4).position(|c| *c == ']') {
                let digits: String = chars[i + 1..i + 1 + len].iter().collect();
                if let Ok(byte) = digits.parse::<u8>() {
                    out.push(byte);
                    i += len + 2;
                    continue;
                }
            }
        }
        let c = chars[i] as u32;
        out.push(if c < 256 { c as u8 } else { b'?' });
        i += 1;
    }
    out
}

/// Reads a log line by line (the built-in's loop over `text.lines()`), keeping the message name
/// of the last info line for the packet line after it.
#[derive(Debug, Default)]
pub struct LineReader {
    name: Option<String>,
    /// A line looked like G-Earth's hexdump view.
    pub hexdump: bool,
}

/// A packet line: direction, header id, the packet text, the name from its info line.
pub struct PacketLine<'a> {
    pub to_server: bool,
    pub header: u16,
    pub text: &'a str,
    pub name: Option<String>,
}

impl PacketLine<'_> {
    /// The packet: the text is the whole packet (length, header, body); a body alone is taken
    /// as is.
    pub fn packet(&self) -> Packet {
        let bytes = decode_packet_text(self.text);
        match Packet::from_bytes(&bytes) {
            Ok(p) if p.header == self.header => p,
            _ => Packet {
                header: self.header,
                body: bytes,
            },
        }
    }
}

impl LineReader {
    /// One line (its line end may still be on it).
    pub fn line<'a>(&mut self, line: &'a str) -> Option<PacketLine<'a>> {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.len() >= 10 && trimmed.chars().all(|c| c == '-') {
            self.name = None;
            return None;
        }
        let (direction, rest) = if let Some(rest) = trimmed.strip_prefix("Incoming[") {
            (false, rest)
        } else if let Some(rest) = trimmed.strip_prefix("Outgoing[") {
            (true, rest)
        } else {
            // `[CatalogPage][hash][804]`: the name is the first non-number in brackets.
            if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains(" -> ") {
                self.name = trimmed
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split("][")
                    .find(|part| {
                        !part.is_empty()
                            && part.parse::<i64>().is_err()
                            && !matches!(*part, "Blocked" | "Replaced")
                            && part.len() < 64
                            && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && part.chars().any(|c| c.is_ascii_uppercase())
                    })
                    .map(str::to_string)
                    .or(self.name.take());
            } else if trimmed.starts_with("0000") || trimmed.contains("  |") {
                self.hexdump = true;
            }
            return None;
        };
        let (id, text) = rest.split_once("] -> ")?;
        let header = id.trim().parse::<u16>().ok()?;
        Some(PacketLine {
            to_server: direction,
            header,
            text,
            name: self.name.take(),
        })
    }
}

const HEXDUMP: &str = "the log is in G-Earth's hexdump view, which cannot be read back: export it again with the normal (or raw hex) view";
const NO_PACKETS: &str = "no packets in the log (lines like \"Incoming[804] -> ...\" from G-Earth's Packet Logger \"Export all\")";

/// The packets of a Packet Logger export (the whole text at once, as the built-in reads it).
pub fn parse_log(text: &str) -> Result<Vec<LoggedPacket>, String> {
    let mut reader = LineReader::default();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(p) = reader.line(line) {
            out.push(LoggedPacket {
                to_server: p.to_server,
                packet: p.packet(),
                name: p.name,
            });
        }
    }
    if out.is_empty() {
        return Err(if reader.hexdump { HEXDUMP } else { NO_PACKETS }.into());
    }
    Ok(out)
}

/// What a log held of the catalogue.
#[derive(Debug, Clone, Default)]
pub struct LoggedCatalogue {
    pub headers: Option<Headers>,
    /// mode -> index body.
    pub indexes: HashMap<String, Vec<u8>>,
    /// (mode, page id) -> page body.
    pub pages: HashMap<(String, i32), Vec<u8>>,
}

fn missing_error(learner: &Learner) -> String {
    format!(
        "the log does not show which packets are the catalogue's (missing: {}): open the catalogue and a page while logging, or give the ids",
        learner.missing().join(", ")
    )
}

const NO_INDEX: &str = "the log has no catalogue index: open the catalogue while logging";

/// Takes the catalogue's packets, once the ids are known.
#[derive(Debug)]
struct Collector {
    headers: Headers,
    index_mode: String,
    page_mode: String,
    found: LoggedCatalogue,
}

impl Collector {
    fn new(headers: Headers) -> Collector {
        Collector {
            headers,
            index_mode: "NORMAL".into(),
            page_mode: "NORMAL".into(),
            found: LoggedCatalogue {
                headers: Some(headers),
                ..LoggedCatalogue::default()
            },
        }
    }

    fn packet(&mut self, to_server: bool, packet: Packet) {
        let headers = self.headers;
        let header = packet.header;
        if to_server {
            let mut r = packet.reader();
            if header == headers.get_index {
                if let Ok(mode) = r.string("mode") {
                    self.index_mode = mode.to_ascii_uppercase();
                }
            } else if header == headers.get_page {
                let _ = (r.i32("id"), r.i32("offer"));
                if let Ok(mode) = r.string("mode") {
                    self.page_mode = mode.to_ascii_uppercase();
                }
            }
        } else if header == headers.index {
            if let Ok(index) = packets::parse_index(&packet.body) {
                let mode = if index.mode.is_empty() {
                    self.index_mode.clone()
                } else {
                    index.mode.to_ascii_uppercase()
                };
                self.found.indexes.insert(mode, packet.body);
            }
        } else if header == headers.page {
            if let Some((id, mode)) = packets::page_key(&packet.body) {
                let mode = if mode.is_empty() {
                    self.page_mode.clone()
                } else {
                    mode.to_ascii_uppercase()
                };
                self.found.pages.insert((mode, id), packet.body);
            }
        }
    }

    fn finish(self) -> Result<LoggedCatalogue, String> {
        if self.found.indexes.is_empty() {
            return Err(NO_INDEX.into());
        }
        Ok(self.found)
    }
}

fn named_infos(names: impl IntoIterator<Item = (u16, String, bool)>) -> Vec<PacketInfo> {
    names
        .into_iter()
        .map(|(header, name, to_server)| PacketInfo {
            header_id: header as i32,
            hash: None,
            name: Some(name),
            structure: None,
            to_server,
            source: "log".into(),
        })
        .collect()
}

/// Finds the catalogue in the packets: ids from the names (or `known`), else learned (the
/// built-in's `catalogue_of`, on packets in memory).
pub fn catalogue_of(
    packets_in: &[LoggedPacket],
    known: &HeaderMap,
) -> Result<LoggedCatalogue, String> {
    let infos = named_infos(
        packets_in
            .iter()
            .filter_map(|p| Some((p.packet.header, p.name.clone()?, p.to_server))),
    );
    let (partial, _) = packets::resolve_partial(&infos, known, &HeaderMap::default());
    let mut learner = Learner::new(partial);
    for p in packets_in {
        if learner.headers().is_some() {
            break;
        }
        learner.observe(p.to_server, &p.packet);
    }
    let Some(headers) = learner.headers() else {
        return Err(missing_error(&learner));
    };
    let mut collector = Collector::new(headers);
    for p in packets_in {
        collector.packet(p.to_server, p.packet.clone());
    }
    collector.finish()
}

/// Splits bytes that come in pieces into lines (`\n`; a `\r` before it stays on the line).
#[derive(Debug, Default)]
struct Lines {
    tail: Vec<u8>,
}

impl Lines {
    fn feed(&mut self, data: &[u8], eof: bool, each: &mut dyn FnMut(&str) -> bool) -> bool {
        let mut rest = data;
        while let Some(at) = rest.iter().position(|&b| b == b'\n') {
            let line: std::borrow::Cow<str> = if self.tail.is_empty() {
                String::from_utf8_lossy(&rest[..at])
            } else {
                self.tail.extend_from_slice(&rest[..at]);
                std::borrow::Cow::Owned(
                    String::from_utf8_lossy(&std::mem::take(&mut self.tail)).into_owned(),
                )
            };
            rest = &rest[at + 1..];
            if !each(&line) {
                return false;
            }
        }
        self.tail.extend_from_slice(rest);
        if eof && !self.tail.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.tail)).into_owned();
            return each(&line);
        }
        true
    }
}

/// Which reading of the file.
#[derive(Debug)]
enum Pass {
    /// The message names of the info lines.
    Names {
        seen: HashSet<(u16, String, bool)>,
        names: Vec<(u16, String, bool)>,
    },
    /// The packets, until the ids are known.
    Learn(Learner),
    /// The catalogue's packets.
    Collect(Collector),
}

/// Reads a Packet Logger export through a [`Source`] in pieces of `piece` bytes: what
/// [`parse_log`] and [`catalogue_of`] find in it, without the whole file or its other packets in
/// memory. Step it with [`LogScan::step`].
#[derive(Debug)]
pub struct LogScan {
    path: String,
    known: HeaderMap,
    piece: u32,
    offset: u64,
    lines: Lines,
    reader: LineReader,
    pass: Pass,
    /// Packets in the log (counted in the first reading).
    pub packets: usize,
}

impl LogScan {
    /// `known`: the ids of the hotel's profile, if any.
    pub fn new(path: &str, known: HeaderMap, piece: u32) -> LogScan {
        LogScan {
            path: path.to_string(),
            known,
            piece: piece.clamp(1, crate::source::MAX_READ),
            offset: 0,
            lines: Lines::default(),
            reader: LineReader::default(),
            pass: Pass::Names {
                seen: HashSet::new(),
                names: Vec::new(),
            },
            packets: 0,
        }
    }

    /// The bytes read in this reading of the file, and which reading (1-3).
    pub fn progress(&self) -> (u64, u8) {
        let pass = match self.pass {
            Pass::Names { .. } => 1,
            Pass::Learn(_) => 2,
            Pass::Collect(_) => 3,
        };
        (self.offset, pass)
    }

    fn restart(&mut self, pass: Pass) {
        self.pass = pass;
        self.offset = 0;
        self.lines = Lines::default();
        self.reader = LineReader::default();
    }

    /// Reads on while `more()` says so (at least one piece). `Ok(Some(_))`: the catalogue.
    pub fn step(
        &mut self,
        source: &mut dyn Source,
        more: &mut dyn FnMut() -> bool,
    ) -> Result<Option<LoggedCatalogue>, String> {
        loop {
            let data = source
                .read(&self.path, self.offset, self.piece)
                .map_err(|e| format!("reading {}: {e}", self.path))?;
            let eof = data.is_empty();
            self.offset += data.len() as u64;
            let (reader, pass, count) = (&mut self.reader, &mut self.pass, &mut self.packets);
            let mut stop = false;
            self.lines.feed(&data, eof, &mut |line| {
                let Some(p) = reader.line(line) else {
                    return true;
                };
                match pass {
                    Pass::Names { seen, names } => {
                        *count += 1;
                        if let Some(name) = p.name {
                            let key = (p.header, name, p.to_server);
                            if seen.insert(key.clone()) {
                                names.push(key);
                            }
                        }
                    }
                    Pass::Learn(learner) => {
                        learner.observe(p.to_server, &p.packet());
                        if learner.headers().is_some() {
                            stop = true;
                            return false;
                        }
                    }
                    Pass::Collect(collector) => collector.packet(p.to_server, p.packet()),
                }
                true
            });
            if eof || stop {
                match std::mem::replace(&mut self.pass, Pass::Learn(Learner::new([None; 4]))) {
                    Pass::Names { names, .. } => {
                        if self.packets == 0 {
                            return Err(if self.reader.hexdump {
                                HEXDUMP
                            } else {
                                NO_PACKETS
                            }
                            .into());
                        }
                        let (partial, _) = packets::resolve_partial(
                            &named_infos(names),
                            &self.known,
                            &HeaderMap::default(),
                        );
                        let learner = Learner::new(partial);
                        match learner.headers() {
                            Some(headers) => self.restart(Pass::Collect(Collector::new(headers))),
                            None => self.restart(Pass::Learn(learner)),
                        }
                    }
                    Pass::Learn(learner) => match learner.headers() {
                        Some(headers) => self.restart(Pass::Collect(Collector::new(headers))),
                        None => return Err(missing_error(&learner)),
                    },
                    Pass::Collect(collector) => return collector.finish().map(Some),
                }
            }
            if !more() {
                return Ok(None);
            }
        }
    }
}

/// The modes in the order the built-in's import lists them: NORMAL first, then by name.
pub fn mode_order(modes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut modes: Vec<String> = modes.into_iter().collect();
    modes.sort_by_key(|m| (m != "NORMAL", m.clone()));
    modes
}

/// The packet files the built-in's import records for what was found (name -> body; a later
/// one of the same name wins, as the second `fs::write` did).
pub fn recorded_files(found: &LoggedCatalogue) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for (mode, body) in &found.indexes {
        files.insert(crate::recording::index_file(mode), body.clone());
    }
    for ((mode, id), body) in &found.pages {
        files.insert(crate::recording::page_file(mode, *id), body.clone());
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemSource;
    use crate::wire::Writer;

    /// G-Earth's text form of a packet (PacketStringUtils.toString).
    pub fn packet_text(packet: &Packet) -> String {
        packet
            .to_bytes()
            .iter()
            .map(|&b| {
                if !(32..=160).contains(&b) || b == 127 || matches!(b, b'[' | b']' | b'{' | b'}') {
                    format!("[{b}]")
                } else {
                    (b as char).to_string()
                }
            })
            .collect()
    }

    #[test]
    fn decodes_both_views() {
        let p = Writer::new(804).i32(3).string("NORMAL [x] é").packet();
        assert_eq!(decode_packet_text(&packet_text(&p)), p.to_bytes());
        let hex: String = p.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(decode_packet_text(&hex), p.to_bytes());
    }

    fn scan(text: &str, known: HeaderMap, piece: u32) -> Result<LoggedCatalogue, String> {
        let mut source = MemSource::new();
        source
            .files
            .insert("log.txt".into(), text.as_bytes().to_vec());
        let mut scan = LogScan::new("log.txt", known, piece);
        loop {
            if let Some(found) = scan.step(&mut source, &mut || false)? {
                return Ok(found);
            }
        }
    }

    #[test]
    fn reads_an_export() {
        let get = Writer::new(1195).string("NORMAL").packet();
        let index = Writer::new(1032)
            .bool(true)
            .i32(0)
            .i32(-1)
            .string("root")
            .string("")
            .i32(0)
            .i32(1)
            .bool(true)
            .i32(1)
            .i32(5)
            .string("shop")
            .string("Shop")
            .i32(0)
            .i32(0)
            .bool(false)
            .string("NORMAL")
            .packet();
        let text = format!(
            "(timestamp: 1700000000000)\n[GetCatalogIndex][abc][1195]\nOutgoing[1195] -> {}\n--------------------\n[CatalogIndex][def][1032]\nIncoming[1032] -> {}\n--------------------\n",
            packet_text(&get),
            packet_text(&index)
        );
        let logged = parse_log(&text).unwrap();
        assert_eq!(logged.len(), 2);
        assert_eq!(logged[0].name.as_deref(), Some("GetCatalogIndex"));
        assert!(logged[0].to_server);
        assert_eq!(logged[1].packet, index);
        assert!(parse_log("0000  00 01 |..|")
            .unwrap_err()
            .contains("hexdump"));
        // Two of four ids from the names; no page in the log, so it stops clearly.
        let err = catalogue_of(&logged, &HeaderMap::default()).unwrap_err();
        assert!(err.contains("GetCatalogPage"), "{err}");
        let found = catalogue_of(&logged, &HeaderMap::new(1195, 412, 1032, 804)).unwrap();
        assert!(found.indexes.contains_key("NORMAL"));

        // The same, read in pieces of every size (with CRLF line ends too).
        for piece in [1, 7, 64, 4096] {
            let err = scan(&text, HeaderMap::default(), piece).unwrap_err();
            assert!(err.contains("GetCatalogPage"), "{err}");
            let got = scan(
                &text.replace('\n', "\r\n"),
                HeaderMap::new(1195, 412, 1032, 804),
                piece,
            )
            .unwrap();
            assert_eq!(got.indexes, found.indexes);
        }
        assert!(scan("0000  00 01 |..|", HeaderMap::default(), 3)
            .unwrap_err()
            .contains("hexdump"));
        assert!(scan("nothing\n", HeaderMap::default(), 3)
            .unwrap_err()
            .contains("no packets"));
    }
}
