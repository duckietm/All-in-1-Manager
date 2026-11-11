//! A rip from a G-Earth Packet Logger export, without a live session: someone who browsed the
//! catalogue with the Packet Logger open saves it ("Export all", a .txt of the logger's text)
//! and the catalogue is rebuilt from the packets in it.
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

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio_util::sync::CancellationToken;

use crate::gearth::PacketInfo;
use crate::learn::Learner;
use crate::packets;
use crate::profiles::{HeaderMap, HotelKind, HotelProfile};
use crate::recording::{Recorder, SessionInfo};
use crate::rip::{reexport, RipOptions, RipResult};
use crate::wire::Packet;
use crate::RipHooks;

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

/// The packets of a Packet Logger export.
pub fn parse_log(text: &str) -> Result<Vec<LoggedPacket>> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut hexdump = false;
    for line in text.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.len() >= 10 && trimmed.chars().all(|c| c == '-') {
            name = None;
            continue;
        }
        let (direction, rest) = if let Some(rest) = trimmed.strip_prefix("Incoming[") {
            (false, rest)
        } else if let Some(rest) = trimmed.strip_prefix("Outgoing[") {
            (true, rest)
        } else {
            // `[CatalogPage][hash][804]`: the name is the first non-number in brackets.
            if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains(" -> ") {
                name = trimmed
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
                    .or(name.take());
            } else if trimmed.starts_with("0000") || trimmed.contains("  |") {
                hexdump = true;
            }
            continue;
        };
        let Some((id, packet_text)) = rest.split_once("] -> ") else {
            continue;
        };
        let Ok(header) = id.trim().parse::<u16>() else {
            continue;
        };
        let bytes = decode_packet_text(packet_text);
        // The text is the whole packet (length, header, body); a body alone is taken as is.
        let packet = match Packet::from_bytes(&bytes) {
            Ok(p) if p.header == header => p,
            _ => Packet {
                header,
                body: bytes,
            },
        };
        out.push(LoggedPacket {
            to_server: direction,
            packet,
            name: name.take(),
        });
    }
    if out.is_empty() {
        if hexdump {
            bail!("the log is in G-Earth's hexdump view, which cannot be read back: export it again with the normal (or raw hex) view");
        }
        bail!("no packets in the log (lines like \"Incoming[804] -> ...\" from G-Earth's Packet Logger \"Export all\")");
    }
    Ok(out)
}

/// What a log held of the catalogue.
#[derive(Debug, Clone, Default)]
pub struct LoggedCatalogue {
    pub headers: Option<packets::Headers>,
    /// mode -> index body.
    pub indexes: HashMap<String, Vec<u8>>,
    /// (mode, page id) -> page body.
    pub pages: HashMap<(String, i32), Vec<u8>>,
}

/// Finds the catalogue in the packets: ids from the names (or `known`), else learned.
pub fn catalogue_of(packets_in: &[LoggedPacket], known: &HeaderMap) -> Result<LoggedCatalogue> {
    let infos: Vec<PacketInfo> = packets_in
        .iter()
        .filter_map(|p| {
            Some(PacketInfo {
                header_id: p.packet.header as i32,
                hash: None,
                name: Some(p.name.clone()?),
                structure: None,
                to_server: p.to_server,
                source: "log".into(),
            })
        })
        .collect();
    let (partial, _) = packets::resolve_partial(&infos, known, &HeaderMap::default());
    let mut learner = Learner::new(partial);
    for p in packets_in {
        if learner.headers().is_some() {
            break;
        }
        learner.observe(p.to_server, &p.packet);
    }
    let Some(headers) = learner.headers() else {
        bail!(
            "the log does not show which packets are the catalogue's (missing: {}): open the catalogue and a page while logging, or give the ids",
            learner.missing().join(", ")
        );
    };
    let mut found = LoggedCatalogue {
        headers: Some(headers),
        ..LoggedCatalogue::default()
    };
    let (mut index_mode, mut page_mode) = ("NORMAL".to_string(), "NORMAL".to_string());
    for p in packets_in {
        let header = p.packet.header;
        if p.to_server {
            let mut r = p.packet.reader();
            if header == headers.get_index {
                if let Ok(mode) = r.string("mode") {
                    index_mode = mode.to_ascii_uppercase();
                }
            } else if header == headers.get_page {
                let _ = (r.i32("id"), r.i32("offer"));
                if let Ok(mode) = r.string("mode") {
                    page_mode = mode.to_ascii_uppercase();
                }
            }
        } else if header == headers.index {
            if let Ok(index) = packets::parse_index(&p.packet.body) {
                let mode = if index.mode.is_empty() { index_mode.clone() } else { index.mode.to_ascii_uppercase() };
                found.indexes.insert(mode, p.packet.body.clone());
            }
        } else if header == headers.page {
            if let Some((id, mode)) = packets::page_key(&p.packet.body) {
                let mode = if mode.is_empty() { page_mode.clone() } else { mode.to_ascii_uppercase() };
                found.pages.insert((mode, id), p.packet.body.clone());
            }
        }
    }
    if found.indexes.is_empty() {
        bail!("the log has no catalogue index: open the catalogue while logging");
    }
    Ok(found)
}

/// Rips from a Packet Logger export: the catalogue it holds is written as a rip of `host`
/// (output folder, recorded packets, every file) — only the pages that were opened while
/// logging are in it. `profile`: the hotel's, for its ids and image URLs, if known.
pub async fn import_log(
    log_file: &Path,
    host: &str,
    profile: Option<HotelProfile>,
    options: &RipOptions,
    hooks: &RipHooks<'_>,
    cancel: &CancellationToken,
) -> Result<RipResult> {
    let bytes = std::fs::read(log_file).with_context(|| format!("reading {}", log_file.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let logged = parse_log(&text)?;
    let known = profile.as_ref().map(|p| p.headers.clone()).unwrap_or_default();
    let found = catalogue_of(&logged, &known)?;
    let headers = found.headers.context("no ids")?;
    (hooks.log)(&format!(
        "{}: {} packets, {} catalogue indexes and {} pages.",
        log_file.display(),
        logged.len(),
        found.indexes.len(),
        found.pages.len()
    ));
    let host = if host.trim().is_empty() { "gearth-log" } else { host.trim() };
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let folder = options
        .output_folder
        .join(format!("{}_{stamp}", crate::rip::hotel_name(host)));
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let recorder = Recorder::new(&folder, true);
    for (mode, body) in &found.indexes {
        recorder.index(mode, body);
    }
    for ((mode, id), body) in &found.pages {
        recorder.page(mode, *id, body);
    }
    let mut modes: Vec<String> = found.indexes.keys().cloned().collect();
    modes.sort_by_key(|m| (m != "NORMAL", m.clone()));
    let profile = profile.unwrap_or_else(|| HotelProfile {
        name: format!("{host} (G-Earth log)"),
        kind: HotelKind::FlashRetro,
        hosts: Vec::new(),
        revisions: Vec::new(),
        headers: HeaderMap::new(headers.get_index, headers.get_page, headers.index, headers.page),
        catalog_url: None,
        icon_url: None,
        furnidata_url: None,
        productdata_url: None,
        site_config_url: None,
        extra_headers: Default::default(),
        builders_club: true,
        notes: String::new(),
        builtin: false,
        file: None,
    });
    recorder.session(&SessionInfo {
        format: 1,
        host: host.to_string(),
        port: 0,
        hotel_version: String::new(),
        client_type: "G-Earth log".into(),
        profile,
        headers: Some(headers),
        modes,
        failed: Default::default(),
        complete: false,
        ripped_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    reexport(&folder, options, hooks, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn reads_an_export() {
        let get = Writer::new(1195).string("NORMAL").packet();
        let index = Writer::new(1032)
            .bool(true).i32(0).i32(-1).string("root").string("").i32(0).i32(1)
            .bool(true).i32(1).i32(5).string("shop").string("Shop").i32(0).i32(0)
            .bool(false).string("NORMAL")
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
        assert!(parse_log("0000  00 01 |..|").unwrap_err().to_string().contains("hexdump"));
        // Two of four ids from the names; no page in the log, so it stops clearly.
        let err = catalogue_of(&logged, &HeaderMap::default()).unwrap_err().to_string();
        assert!(err.contains("GetCatalogPage"), "{err}");
        let found = catalogue_of(&logged, &HeaderMap::new(1195, 412, 1032, 804)).unwrap();
        assert!(found.indexes.contains_key("NORMAL"));
    }
}
