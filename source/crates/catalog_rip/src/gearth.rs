//! The extension side of G-Earth's network extension protocol (G-Earth's
//! `NetworkExtensionCodec` / `NetworkExtensionServer`, and the Java `Extension` class).
//!
//! G-Earth listens on 127.0.0.1, first free port from 9092 upwards. Every message, both ways,
//! is a packet in the Habbo wire format (int32 length, uint16 header, body):
//!
//! | G-Earth -> extension | id | body |
//! |---|---|---|
//! | OnDoubleClick | 1 | - |
//! | InfoRequest | 2 | - (first message after connecting) |
//! | PacketIntercept | 3 | long string `blocked \t index \t TOCLIENT/TOSERVER \t edited packet-bytes`, int format |
//! | FlagsCheck | 4 | int n, n strings |
//! | ConnectionStart | 5 | host, int port, hotel version, client id, client type, packet info list |
//! | ConnectionEnd | 6 | - |
//! | Init | 7 | bool connected, host info (packetlogger, version, int n, n key/value pairs) |
//! | UpdateHostInfo | 10 | host info |
//!
//! | extension -> G-Earth | id | body |
//! |---|---|---|
//! | ExtensionInfo | 1 | title, author, version, description, bool onClick, bool hasFile, file, cookie, bool canLeave, bool canDelete |
//! | ManipulatedPacket | 2 | long string (the intercepted message, possibly changed), int format |
//! | RequestFlags | 3 | - |
//! | SendMessage | 4 | byte side (0 client, 1 server), int length, packet bytes, int format |
//! | ExtensionConsoleLog | 98 | string |
//!
//! G-Earth holds every intercepted packet until all extensions answered its PacketIntercept, so
//! this client answers each one at once, from the socket reader task, with the bytes it got:
//! the user's game traffic is never held up or changed.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::wire::{Packet, Reader, Writer};

/// G-Earth's `NetworkExtensionServer.PORT_ONSET`: it binds the first free port from here up.
pub const PORT_ONSET: u16 = 9092;
/// Ports tried by default (G-Earth moves up one port for every port already taken).
pub fn default_ports() -> Vec<u16> {
    (PORT_ONSET..PORT_ONSET + 10).collect()
}

pub mod ids {
    // G-Earth -> extension
    pub const ON_DOUBLE_CLICK: u16 = 1;
    pub const INFO_REQUEST: u16 = 2;
    pub const PACKET_INTERCEPT: u16 = 3;
    pub const FLAGS_CHECK: u16 = 4;
    pub const CONNECTION_START: u16 = 5;
    pub const CONNECTION_END: u16 = 6;
    pub const INIT: u16 = 7;
    pub const UPDATE_HOST_INFO: u16 = 10;
    // extension -> G-Earth
    pub const EXTENSION_INFO: u16 = 1;
    pub const MANIPULATED_PACKET: u16 = 2;
    pub const REQUEST_FLAGS: u16 = 3;
    pub const SEND_MESSAGE: u16 = 4;
    pub const EXTENSION_CONSOLE_LOG: u16 = 98;
}

/// `HPacketFormat.EVA_WIRE`, the format of Flash/AIR, Unity and Nitro packets.
pub const FORMAT_EVA_WIRE: i32 = 0;

/// What the extension tells G-Earth about itself (shown in G-Earth's Extensions tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionInfo {
    pub title: String,
    pub author: String,
    pub version: String,
    pub description: String,
}

impl Default for ExtensionInfo {
    fn default() -> Self {
        Self {
            title: "All-in-1 Manager".into(),
            author: "SimoLeoLife".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Catalogue ripper: reads the catalogue index and pages through your own Habbo session".into(),
        }
    }
}

impl ExtensionInfo {
    /// The ExtensionInfo message. Not an installed extension: no file, no cookie, so G-Earth
    /// treats it as an external one (it may ask the user to allow it).
    pub fn message(&self) -> Packet {
        Writer::new(ids::EXTENSION_INFO)
            .string_latin1(&self.title)
            .string_latin1(&self.author)
            .string_latin1(&self.version)
            .string_latin1(&self.description)
            .bool(false) // onClick used
            .bool(false) // has file
            .string_latin1("") // file
            .string_latin1("") // cookie
            .bool(true) // can leave
            .bool(false) // can delete
            .packet()
    }
}

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
        let mut w = Writer::new(ids::CONNECTION_START)
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

/// An intercepted game packet as G-Earth sends it (`HMessage.stringify`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intercepted {
    pub to_server: bool,
    pub index: i32,
    pub packet: Packet,
}

/// Splits `blocked \t index \t direction \t edited+bytes`. The bytes are ISO-8859-1 on the
/// Java side, so they are the raw packet bytes here.
pub fn parse_intercept(message: &[u8]) -> Result<Intercepted> {
    let mut parts = message.splitn(4, |&b| b == b'\t');
    let _blocked = parts.next().context("intercept: no blocked flag")?;
    let index = parts.next().context("intercept: no index")?;
    let direction = parts.next().context("intercept: no direction")?;
    let rest = parts.next().context("intercept: no packet")?;
    let index: i32 = std::str::from_utf8(index)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1);
    let to_server = direction != b"TOCLIENT";
    if rest.is_empty() {
        bail!("intercept: empty packet");
    }
    let packet = Packet::from_bytes(&rest[1..])?;
    Ok(Intercepted {
        to_server,
        index,
        packet,
    })
}

/// The PacketIntercept message G-Earth sends for a game packet (used by tests).
pub fn intercept_message(to_server: bool, index: i32, packet: &Packet) -> Packet {
    let mut text = format!(
        "0\t{index}\t{}\t0",
        if to_server { "TOSERVER" } else { "TOCLIENT" }
    )
    .into_bytes();
    text.extend_from_slice(&packet.to_bytes());
    Writer::new(ids::PACKET_INTERCEPT)
        .long_bytes(&text)
        .i32(FORMAT_EVA_WIRE)
        .packet()
}

/// The answer to a PacketIntercept that lets the packet through unchanged: the same message
/// text and format, byte for byte.
pub fn unchanged_reply(intercept_body: &[u8]) -> Result<Packet> {
    let mut r = Reader::new(intercept_body);
    let message = r.long_bytes("intercepted message")?;
    let format = if r.remaining() >= 4 {
        r.i32("packet format")?
    } else {
        FORMAT_EVA_WIRE
    };
    Ok(Writer::new(ids::MANIPULATED_PACKET)
        .long_bytes(message)
        .i32(format)
        .packet())
}

/// What the session reports to the operation.
#[derive(Debug, Clone)]
pub enum Event {
    /// G-Earth accepted the extension; `connected` is true when a hotel connection is up.
    Init {
        connected: bool,
    },
    ConnectionStart(ConnectionInfo),
    ConnectionEnd,
    /// A watched game packet went by (see [`Session::watch`]).
    Packet(Intercepted),
    /// G-Earth closed the connection (or it failed).
    Closed(String),
}

/// A live connection to G-Earth.
pub struct Session {
    pub port: u16,
    events: mpsc::UnboundedReceiver<Event>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    watched: Arc<Mutex<HashSet<(bool, u16)>>>,
    watch_all: Arc<AtomicBool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Session {
    /// Closes the socket, so G-Earth removes the extension from its list.
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn read_frame(reader: &mut (impl AsyncReadExt + Unpin)) -> std::io::Result<Packet> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await?;
    let length = i32::from_be_bytes(length);
    if !(2..=64 * 1024 * 1024).contains(&length) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad frame length {length}"),
        ));
    }
    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await?;
    Ok(Packet {
        header: u16::from_be_bytes([payload[0], payload[1]]),
        body: payload[2..].to_vec(),
    })
}

impl Session {
    /// Connects to G-Earth on `port`, answers its InfoRequest and starts the reader.
    /// Fails when nothing listens there or what listens is not G-Earth (no InfoRequest within
    /// `handshake_timeout`).
    pub async fn connect(
        port: u16,
        info: &ExtensionInfo,
        handshake_timeout: Duration,
    ) -> Result<Session> {
        let stream =
            tokio::time::timeout(handshake_timeout, TcpStream::connect(("127.0.0.1", port)))
                .await
                .context("connect timed out")??;
        stream.set_nodelay(true).ok();
        let (mut read_half, mut write_half) = stream.into_split();

        let first = tokio::time::timeout(handshake_timeout, read_frame(&mut read_half))
            .await
            .context("no InfoRequest from G-Earth")??;
        if first.header != ids::INFO_REQUEST {
            bail!(
                "unexpected first message {} (expected G-Earth's InfoRequest)",
                first.header
            );
        }
        write_half.write_all(&info.message().to_bytes()).await?;

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer = tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if write_half.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let watched = Arc::new(Mutex::new(HashSet::new()));
        let watch_all = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(reader_loop(
            read_half,
            event_tx,
            out_tx.clone(),
            Watch {
                some: watched.clone(),
                all: watch_all.clone(),
            },
            info.clone(),
        ));

        Ok(Session {
            port,
            events: event_rx,
            out: out_tx,
            watched,
            watch_all,
            tasks: vec![reader, writer],
        })
    }

    /// Report packets with this header and direction as [`Event::Packet`].
    pub fn watch(&self, to_server: bool, header: u16) {
        self.watched.lock().unwrap().insert((to_server, header));
    }

    /// Report every game packet, both ways, as [`Event::Packet`] (to learn header ids), or
    /// only the watched ones again.
    pub fn watch_all(&self, on: bool) {
        self.watch_all.store(on, Ordering::SeqCst);
    }

    /// Sends a game packet to the hotel server through G-Earth (as the user's client).
    pub fn send_to_server(&self, packet: &Packet) -> Result<()> {
        let bytes = packet.to_bytes();
        let message = Writer::new(ids::SEND_MESSAGE)
            .byte(1)
            .i32(bytes.len() as i32)
            .raw(&bytes)
            .i32(FORMAT_EVA_WIRE);
        self.out
            .send(message.to_bytes())
            .map_err(|_| anyhow::anyhow!("the connection to G-Earth is closed"))
    }

    /// A line in G-Earth's extension console.
    pub fn console_log(&self, text: &str) {
        let _ = self.out.send(
            Writer::new(ids::EXTENSION_CONSOLE_LOG)
                .string_latin1(text)
                .to_bytes(),
        );
    }

    /// The next event, or `None` when the reader has stopped.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }
}

/// Which intercepted packets the reader reports.
struct Watch {
    some: Arc<Mutex<HashSet<(bool, u16)>>>,
    all: Arc<AtomicBool>,
}

async fn reader_loop(
    mut read_half: OwnedReadHalf,
    events: mpsc::UnboundedSender<Event>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    watched: Watch,
    info: ExtensionInfo,
) {
    let reason = loop {
        let frame = match read_frame(&mut read_half).await {
            Ok(frame) => frame,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break "G-Earth closed the connection".to_string()
            }
            Err(e) => break format!("connection to G-Earth failed: {e}"),
        };
        match frame.header {
            ids::PACKET_INTERCEPT => {
                // Answer first, unchanged, so the game traffic never waits on this extension.
                match unchanged_reply(&frame.body) {
                    Ok(reply) => {
                        let _ = out.send(reply.to_bytes());
                    }
                    Err(e) => break format!("unreadable intercepted packet from G-Earth: {e}"),
                }
                let mut r = Reader::new(&frame.body);
                if let Ok(message) = r.long_bytes("message") {
                    if let Ok(intercepted) = parse_intercept(message) {
                        let wanted = watched.all.load(Ordering::SeqCst)
                            || watched
                                .some
                                .lock()
                                .unwrap()
                                .contains(&(intercepted.to_server, intercepted.packet.header));
                        if wanted {
                            let _ = events.send(Event::Packet(intercepted));
                        }
                    }
                }
            }
            ids::INFO_REQUEST => {
                let _ = out.send(info.message().to_bytes());
            }
            ids::INIT => {
                let connected = frame.reader().bool("init").unwrap_or(false);
                let _ = events.send(Event::Init { connected });
            }
            ids::CONNECTION_START => match ConnectionInfo::parse(&frame) {
                Ok(info) => {
                    let _ = events.send(Event::ConnectionStart(info));
                }
                Err(e) => break format!("unreadable ConnectionStart from G-Earth: {e}"),
            },
            ids::CONNECTION_END => {
                let _ = events.send(Event::ConnectionEnd);
            }
            // Double click, flags, host info updates and the string helpers: nothing to do.
            _ => {}
        }
    };
    let _ = events.send(Event::Closed(reason));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_info_layout() {
        let info = ExtensionInfo {
            title: "T".into(),
            author: "A".into(),
            version: "1".into(),
            description: "D".into(),
        };
        let packet = info.message();
        assert_eq!(packet.header, 1);
        let mut r = packet.reader();
        for expected in ["T", "A", "1", "D"] {
            assert_eq!(r.string_latin1("s").unwrap(), expected);
        }
        assert!(!r.bool("onclick").unwrap());
        assert!(!r.bool("hasfile").unwrap());
        assert_eq!(r.string_latin1("file").unwrap(), "");
        assert_eq!(r.string_latin1("cookie").unwrap(), "");
        assert!(r.bool("leave").unwrap());
        assert!(!r.bool("delete").unwrap());
        assert!(r.is_empty());
    }

    #[test]
    fn intercept_reply_is_byte_identical() {
        // Bytes past 0x7F and tabs inside the packet must survive.
        let game = Writer::new(777)
            .string("a\tb")
            .raw(&[0xFF, 0x80, 0x09])
            .packet();
        let intercept = intercept_message(false, 42, &game);
        let reply = unchanged_reply(&intercept.body).unwrap();
        assert_eq!(reply.header, ids::MANIPULATED_PACKET);
        assert_eq!(reply.body, intercept.body);

        let mut r = intercept.reader();
        let parsed = parse_intercept(r.long_bytes("m").unwrap()).unwrap();
        assert_eq!(
            parsed,
            Intercepted {
                to_server: false,
                index: 42,
                packet: game
            }
        );
    }

    #[test]
    fn connection_start_round_trip() {
        let info = ConnectionInfo {
            host: "game-nl.habbo.com".into(),
            port: 30000,
            hotel_version: "WIN63-202501011200-123456789".into(),
            client_identifier: "".into(),
            client_type: "FLASH".into(),
            packets: vec![
                PacketInfo {
                    header_id: 1195,
                    hash: None,
                    name: Some("GetCatalogIndex".into()),
                    structure: None,
                    to_server: true,
                    source: "Sulek".into(),
                },
                PacketInfo {
                    header_id: 804,
                    hash: Some("abc".into()),
                    name: Some("CatalogPage".into()),
                    structure: Some("i".into()),
                    to_server: false,
                    source: "Sulek".into(),
                },
            ],
        };
        assert_eq!(ConnectionInfo::parse(&info.message()).unwrap(), info);
    }
}
