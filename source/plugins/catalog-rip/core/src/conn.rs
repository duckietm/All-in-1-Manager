//! G-Earth's ConnectionStart message as plain data: the packet info list and the hotel connection.
//!
//! In the built-in crate these live in `gearth.rs` (the G-Earth client); the profile/header code
//! needs them too, so they live here and `gearth` re-exports them.

use anyhow::Result;

use crate::wire::{Packet, Writer};

/// G-Earth -> extension ConnectionStart message id.
const CONNECTION_START: u16 = 5;

/// One entry of G-Earth's packet info list (`PacketInfoManager.appendToPacket`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketInfo {
    pub header_id: i32,
    pub hash: Option<String>,
    pub name: Option<String>,
    pub structure: Option<String>,
    /// true: client -> server ("outgoing" in G-Earth), false: server -> client.
    pub to_server: bool,
    pub source: String,
}

/// G-Earth's ConnectionStart: the hotel connection the user's client just made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: i32,
    pub hotel_version: String,
    pub client_identifier: String,
    /// FLASH (also the AIR client), UNITY, NITRO or SHOCKWAVE.
    pub client_type: String,
    pub packets: Vec<PacketInfo>,
}

impl ConnectionInfo {
    pub fn parse(packet: &Packet) -> Result<Self> {
        let mut r = packet.reader();
        let host = r.string_latin1("host")?;
        let port = r.i32("port")?;
        let hotel_version = r.string_latin1("hotel version")?;
        let client_identifier = r.string_latin1("client identifier")?;
        let client_type = r.string_latin1("client type")?;
        let count = r.count(13, "packet info count")?;
        let mut packets = Vec::with_capacity(count);
        let null = |s: String| if s == "NULL" { None } else { Some(s) };
        for _ in 0..count {
            let header_id = r.i32("packet info header")?;
            let hash = null(r.string_latin1("packet info hash")?);
            let name = null(r.string_latin1("packet info name")?);
            let structure = null(r.string_latin1("packet info structure")?);
            let to_server = r.bool("packet info direction")?;
            let source = r.string_latin1("packet info source")?;
            packets.push(PacketInfo {
                header_id,
                hash,
                name,
                structure,
                to_server,
                source,
            });
        }
        Ok(Self {
            host,
            port,
            hotel_version,
            client_identifier,
            client_type,
            packets,
        })
    }

    /// The ConnectionStart message (used by tests that play G-Earth).
    pub fn message(&self) -> Packet {
        let mut w = Writer::new(CONNECTION_START)
            .string_latin1(&self.host)
            .i32(self.port)
            .string_latin1(&self.hotel_version)
            .string_latin1(&self.client_identifier)
            .string_latin1(&self.client_type)
            .i32(self.packets.len() as i32);
        let or_null = |s: &Option<String>| s.clone().unwrap_or_else(|| "NULL".into());
        for p in &self.packets {
            w = w
                .i32(p.header_id)
                .string_latin1(&or_null(&p.hash))
                .string_latin1(&or_null(&p.name))
                .string_latin1(&or_null(&p.structure))
                .bool(p.to_server)
                .string_latin1(&p.source);
        }
        w.packet()
    }
}
