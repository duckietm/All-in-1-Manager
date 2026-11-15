//! The extension side of G-Earth's network extension protocol (G-Earth's
//! `NetworkExtensionCodec` / `NetworkExtensionServer`, and the Java `Extension` class), driven
//! synchronously over a [`Transport`]: no sockets, threads or clock of its own.
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
//! [`GEarth::pump`] answers each one as soon as it has read it, with the bytes it got, before
//! it returns: the user's game traffic is never changed, and held up only between two pumps.
//! The plugin pumps whenever it waits (a short budget, often).

use std::collections::HashSet;

use anyhow::{bail, Context};

use crate::transport::Transport;
use crate::wire::{Packet, Reader, Writer};

pub use crate::conn::{ConnectionInfo, PacketInfo};

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

/// Largest G-Earth frame accepted (length field, header included).
const MAX_FRAME: i32 = 64 * 1024 * 1024;
/// Bytes asked of the transport per `recv`.
const RECV_MAX: usize = 64 * 1024;
/// The longest single wait inside a pump: a pump returns soon after its events arrive.
const SLICE_MS: u32 = 20;

/// What went wrong with the connection to G-Earth. Once a pump fails, every later pump fails
/// the same way: the connection is done.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("G-Earth closed the connection")]
    Closed,
    #[error("connection to G-Earth failed: {0}")]
    Transport(String),
    /// Nothing came from the other side within the handshake timeout.
    #[error("no InfoRequest from G-Earth")]
    NoInfoRequest,
    /// The other side talked, but not as G-Earth does.
    #[error("unexpected first message {0} (expected G-Earth's InfoRequest)")]
    NotGEarth(u16),
    /// G-Earth sent something unreadable (the message says what).
    #[error("{0}")]
    Protocol(String),
}

impl Error {
    /// The connection is gone (closed or failed), as opposed to a protocol mismatch.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Error::Closed | Error::Transport(_))
    }
}

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

/// An intercepted game packet as G-Earth sends it (`HMessage.stringify`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intercepted {
    pub to_server: bool,
    pub index: i32,
    pub packet: Packet,
}

/// Splits `blocked \t index \t direction \t edited+bytes`. The bytes are ISO-8859-1 on the
/// Java side, so they are the raw packet bytes here.
pub fn parse_intercept(message: &[u8]) -> anyhow::Result<Intercepted> {
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
pub fn unchanged_reply(intercept_body: &[u8]) -> anyhow::Result<Packet> {
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

/// The SendMessage frame that sends `packet` to the hotel server (as the user's client).
pub fn send_to_server_message(packet: &Packet) -> Vec<u8> {
    let bytes = packet.to_bytes();
    Writer::new(ids::SEND_MESSAGE)
        .byte(1)
        .i32(bytes.len() as i32)
        .raw(&bytes)
        .i32(FORMAT_EVA_WIRE)
        .to_bytes()
}

/// What a pump reports. G-Earth closing or failing is an [`Error`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GEarthEvent {
    /// G-Earth accepted the extension; `connected` is true when a hotel connection is up.
    Init {
        connected: bool,
    },
    ConnectionStart(ConnectionInfo),
    ConnectionEnd,
    /// A watched game packet went by (see [`GEarth::watch`]).
    Packet(Intercepted),
}

/// The extension's state on a connection to G-Earth. The connection itself (the
/// [`Transport`]) belongs to the caller, who passes it to every call; dropping it closes the
/// connection, so G-Earth removes the extension from its list.
pub struct GEarth {
    info: ExtensionInfo,
    /// Bytes read but not yet a whole frame (kept across recv calls and pumps).
    buf: Vec<u8>,
    watched: HashSet<(bool, u16)>,
    watch_all: bool,
    /// The connection failed: every later pump says so.
    failed: Option<Error>,
}

impl GEarth {
    /// Answers G-Earth's InfoRequest on a fresh connection. Fails when the other side closes,
    /// says something else first, or says nothing within `timeout_ms` (then it is not G-Earth).
    pub fn handshake(
        t: &mut impl Transport,
        info: &ExtensionInfo,
        timeout_ms: u32,
    ) -> Result<GEarth, Error> {
        let mut g = GEarth {
            info: info.clone(),
            buf: Vec::new(),
            watched: HashSet::new(),
            watch_all: false,
            failed: None,
        };
        let mut remaining = timeout_ms;
        let first = loop {
            if let Some(frame) = g.take_frame()? {
                break frame;
            }
            if remaining == 0 {
                return Err(Error::NoInfoRequest);
            }
            let slice = remaining.min(SLICE_MS);
            remaining -= slice;
            g.read(t, slice)?;
        };
        if first.header != ids::INFO_REQUEST {
            return Err(Error::NotGEarth(first.header));
        }
        t.send(&info.message().to_bytes())
            .map_err(Error::Transport)?;
        Ok(g)
    }

    /// Report packets with this header and direction as [`GEarthEvent::Packet`].
    pub fn watch(&mut self, to_server: bool, header: u16) {
        self.watched.insert((to_server, header));
    }

    /// Report every game packet, both ways, as [`GEarthEvent::Packet`] (to learn header ids),
    /// or only the watched ones again.
    pub fn watch_all(&mut self, on: bool) {
        self.watch_all = on;
    }

    /// Sends a game packet to the hotel server through G-Earth (as the user's client).
    pub fn send_to_server(&mut self, t: &mut impl Transport, packet: &Packet) -> Result<(), Error> {
        self.send(t, &send_to_server_message(packet))
    }

    /// A line in G-Earth's extension console.
    pub fn console_log(&mut self, t: &mut impl Transport, text: &str) -> Result<(), Error> {
        let message = Writer::new(ids::EXTENSION_CONSOLE_LOG)
            .string_latin1(text)
            .to_bytes();
        self.send(t, &message)
    }

    /// Reads what G-Earth sent, waiting at most `budget_ms` in all (the timeouts given to
    /// [`Transport::recv`] add up to at most this; 0 only takes what is there already). Every
    /// intercepted packet read is answered unchanged, in order, before this returns. Returns
    /// once the budget is spent, or earlier when there are events and nothing more is coming.
    pub fn pump(
        &mut self,
        t: &mut impl Transport,
        budget_ms: u32,
    ) -> Result<Vec<GEarthEvent>, Error> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        let mut events = Vec::new();
        let result = self.pump_into(t, budget_ms, &mut events);
        match result {
            Ok(()) => Ok(events),
            Err(error) => {
                self.failed = Some(error.clone());
                // What came before the failure is still reported; the next pump fails.
                if events.is_empty() {
                    Err(error)
                } else {
                    Ok(events)
                }
            }
        }
    }

    fn pump_into(
        &mut self,
        t: &mut impl Transport,
        budget_ms: u32,
        events: &mut Vec<GEarthEvent>,
    ) -> Result<(), Error> {
        // Frames already read (left whole in the buffer by the handshake).
        self.handle_frames(t, events)?;
        let mut remaining = budget_ms;
        loop {
            let slice = remaining.min(SLICE_MS);
            remaining -= slice;
            if self.read(t, slice)? {
                self.handle_frames(t, events)?;
            } else if !events.is_empty() {
                return Ok(());
            }
            if remaining == 0 {
                return Ok(());
            }
        }
    }

    /// One recv into the buffer: false when nothing came in time.
    fn read(&mut self, t: &mut impl Transport, timeout_ms: u32) -> Result<bool, Error> {
        match t.recv(RECV_MAX, timeout_ms).map_err(Error::Transport)? {
            None => Ok(false),
            Some(data) if data.is_empty() => Err(Error::Closed),
            Some(data) => {
                self.buf.extend_from_slice(&data);
                Ok(true)
            }
        }
    }

    fn send(&mut self, t: &mut impl Transport, data: &[u8]) -> Result<(), Error> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        t.send(data).map_err(|e| {
            let error = Error::Transport(e);
            self.failed = Some(error.clone());
            error
        })
    }

    /// The next whole frame in the buffer, if there is one.
    fn take_frame(&mut self) -> Result<Option<Packet>, Error> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let length = i32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        if !(2..=MAX_FRAME).contains(&length) {
            return Err(Error::Protocol(format!(
                "connection to G-Earth failed: bad frame length {length}"
            )));
        }
        let end = 4 + length as usize;
        if self.buf.len() < end {
            return Ok(None);
        }
        let packet = Packet {
            header: u16::from_be_bytes([self.buf[4], self.buf[5]]),
            body: self.buf[6..end].to_vec(),
        };
        self.buf.drain(..end);
        Ok(Some(packet))
    }

    /// Handles every whole frame in the buffer; the answers go out together, in order, even
    /// when a later frame is unreadable.
    fn handle_frames(
        &mut self,
        t: &mut impl Transport,
        events: &mut Vec<GEarthEvent>,
    ) -> Result<(), Error> {
        let mut out = Vec::new();
        let result = self.handle_frames_into(&mut out, events);
        if !out.is_empty() {
            self.send(t, &out)?;
        }
        result
    }

    fn handle_frames_into(
        &mut self,
        out: &mut Vec<u8>,
        events: &mut Vec<GEarthEvent>,
    ) -> Result<(), Error> {
        while let Some(frame) = self.take_frame()? {
            match frame.header {
                ids::PACKET_INTERCEPT => {
                    // Answer first, unchanged, so the game traffic never waits on this extension.
                    let reply = unchanged_reply(&frame.body).map_err(|e| {
                        Error::Protocol(format!("unreadable intercepted packet from G-Earth: {e}"))
                    })?;
                    out.extend_from_slice(&reply.to_bytes());
                    let mut r = Reader::new(&frame.body);
                    if let Ok(message) = r.long_bytes("message") {
                        if let Ok(intercepted) = parse_intercept(message) {
                            let wanted = self.watch_all
                                || self
                                    .watched
                                    .contains(&(intercepted.to_server, intercepted.packet.header));
                            if wanted {
                                events.push(GEarthEvent::Packet(intercepted));
                            }
                        }
                    }
                }
                ids::INFO_REQUEST => out.extend_from_slice(&self.info.message().to_bytes()),
                ids::INIT => {
                    let connected = frame.reader().bool("init").unwrap_or(false);
                    events.push(GEarthEvent::Init { connected });
                }
                ids::CONNECTION_START => {
                    let info = ConnectionInfo::parse(&frame).map_err(|e| {
                        Error::Protocol(format!("unreadable ConnectionStart from G-Earth: {e}"))
                    })?;
                    events.push(GEarthEvent::ConnectionStart(info));
                }
                ids::CONNECTION_END => events.push(GEarthEvent::ConnectionEnd),
                // Double click, flags, host info updates and the string helpers: nothing to do.
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// A scripted transport: each recv takes the next answer (None when the script is done).
    #[derive(Default)]
    struct Script {
        answers: VecDeque<Option<Vec<u8>>>,
        timeouts: Vec<u32>,
        sent: Vec<u8>,
    }

    impl Script {
        fn new(answers: impl IntoIterator<Item = Option<Vec<u8>>>) -> Script {
            Script {
                answers: answers.into_iter().collect(),
                ..Script::default()
            }
        }

        /// The frames sent so far (and forgets them).
        fn sent_frames(&mut self) -> Vec<Packet> {
            let mut frames = Vec::new();
            let mut data = &self.sent[..];
            while !data.is_empty() {
                let len = i32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
                frames.push(Packet::from_bytes(&data[..4 + len]).unwrap());
                data = &data[4 + len..];
            }
            self.sent.clear();
            frames
        }
    }

    impl Transport for Script {
        fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.sent.extend_from_slice(data);
            Ok(())
        }

        fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            self.timeouts.push(timeout_ms);
            let answer = self.answers.pop_front().flatten();
            assert!(answer.as_ref().is_none_or(|a| a.len() <= max));
            Ok(answer)
        }
    }

    fn info_request() -> Vec<u8> {
        Writer::new(ids::INFO_REQUEST).to_bytes()
    }

    fn handshaken(t: &mut Script) -> GEarth {
        let g = GEarth::handshake(t, &ExtensionInfo::default(), 1_000).unwrap();
        assert_eq!(t.sent_frames(), [ExtensionInfo::default().message()]);
        t.timeouts.clear();
        g
    }

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

    #[test]
    fn send_to_server_layout() {
        let game = Writer::new(1195).string("NORMAL").packet();
        let mut t = Script::new([Some(info_request())]);
        let mut g = handshaken(&mut t);
        g.send_to_server(&mut t, &game).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&(2 + 1 + 4 + 14 + 4i32).to_be_bytes());
        expected.extend_from_slice(&ids::SEND_MESSAGE.to_be_bytes());
        expected.push(1); // to the server
        expected.extend_from_slice(&14i32.to_be_bytes());
        expected.extend_from_slice(&[0, 0, 0, 10, 0x04, 0xAB, 0, 6]);
        expected.extend_from_slice(b"NORMAL");
        expected.extend_from_slice(&FORMAT_EVA_WIRE.to_be_bytes());
        assert_eq!(t.sent, expected);
    }

    #[test]
    fn pump_waits_at_most_its_budget() {
        let mut t = Script::new([Some(info_request())]);
        let mut g = handshaken(&mut t);
        assert_eq!(g.pump(&mut t, 95).unwrap(), []);
        assert_eq!(t.timeouts.iter().sum::<u32>(), 95);
        assert!(t.timeouts.iter().all(|&ms| ms <= SLICE_MS));
        t.timeouts.clear();
        // Budget 0: one look at what is there.
        assert_eq!(g.pump(&mut t, 0).unwrap(), []);
        assert_eq!(t.timeouts, [0]);
        t.timeouts.clear();
        // Data that keeps coming does not stretch the budget.
        let end = Writer::new(ids::UPDATE_HOST_INFO).to_bytes();
        t.answers = std::iter::repeat_n(Some(end), 100).collect();
        assert_eq!(g.pump(&mut t, 50).unwrap(), []);
        assert!(t.timeouts.iter().sum::<u32>() <= 50, "{:?}", t.timeouts);
    }

    #[test]
    fn pump_returns_early_with_events() {
        let mut t = Script::new([
            Some(info_request()),
            Some(Writer::new(ids::CONNECTION_END).to_bytes()),
            None,
        ]);
        let mut g = handshaken(&mut t);
        assert_eq!(
            g.pump(&mut t, 10_000).unwrap(),
            [GEarthEvent::ConnectionEnd]
        );
        assert_eq!(t.timeouts, [SLICE_MS, SLICE_MS]);
    }

    #[test]
    fn frames_split_across_recvs_and_pumps() {
        // The InfoRequest and the start of an intercept arrive together, the rest a byte at a
        // time with pauses in between.
        let game = Writer::new(999).raw(&[0xFF, b'\t']).packet();
        let intercept = intercept_message(false, 1, &game);
        let bytes = intercept.to_bytes();
        let mut first = info_request();
        first.extend_from_slice(&bytes[..3]);
        let mut t = Script::new([Some(first)]);
        let mut g = handshaken(&mut t);
        g.watch(false, 999);
        for (i, &b) in bytes[3..].iter().enumerate() {
            t.answers = [Some(vec![b]), None].into();
            let events = g.pump(&mut t, 5).unwrap();
            if i + 4 < bytes.len() {
                assert_eq!(events, [], "byte {i}");
                assert!(t.sent.is_empty());
            } else {
                let [GEarthEvent::Packet(p)] = events.as_slice() else {
                    panic!("{events:?}")
                };
                assert_eq!(p.packet, game);
            }
        }
        let answers = t.sent_frames();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].body, intercept.body);
    }

    #[test]
    fn info_request_after_the_handshake_is_answered_again() {
        let mut t = Script::new([Some(info_request()), Some(info_request())]);
        let mut g = handshaken(&mut t);
        g.pump(&mut t, 0).unwrap();
        assert_eq!(t.sent_frames(), [ExtensionInfo::default().message()]);
    }

    #[test]
    fn closed_peer_is_an_error_after_the_events_before_it() {
        let mut t = Script::new([
            Some(info_request()),
            Some(Writer::new(ids::INIT).bool(true).to_bytes()),
            Some(Vec::new()),
        ]);
        let mut g = handshaken(&mut t);
        assert_eq!(
            g.pump(&mut t, 100).unwrap(),
            [GEarthEvent::Init { connected: true }]
        );
        assert_eq!(g.pump(&mut t, 100), Err(Error::Closed));
        assert_eq!(g.pump(&mut t, 100), Err(Error::Closed));
        assert!(g.send_to_server(&mut t, &Writer::new(1).packet()).is_err());
        assert!(Error::Closed.is_connection_lost());
    }

    #[test]
    fn bad_frames() {
        let mut t = Script::new([Some(info_request()), Some(vec![0, 0, 0, 1, 0])]);
        let mut g = handshaken(&mut t);
        let error = g.pump(&mut t, 0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "connection to G-Earth failed: bad frame length 1"
        );
        assert!(!error.is_connection_lost());

        // An unreadable intercept after a good one: the good one is still answered.
        let good = intercept_message(true, 1, &Writer::new(5).packet());
        let mut data = good.to_bytes();
        data.extend_from_slice(&Writer::new(ids::PACKET_INTERCEPT).i32(50).to_bytes());
        let mut t = Script::new([Some(info_request()), Some(data)]);
        let mut g = handshaken(&mut t);
        let error = g.pump(&mut t, 0).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("unreadable intercepted packet from G-Earth:"),
            "{error}"
        );
        assert_eq!(t.sent_frames()[0].body, good.body);
    }

    #[test]
    fn handshake_failures() {
        let mut t = Script::new([None, None]);
        assert_eq!(
            GEarth::handshake(&mut t, &ExtensionInfo::default(), 30).err(),
            Some(Error::NoInfoRequest)
        );
        assert_eq!(t.timeouts, [20, 10]);
        let mut t = Script::new([Some(Vec::new())]);
        assert_eq!(
            GEarth::handshake(&mut t, &ExtensionInfo::default(), 30).err(),
            Some(Error::Closed)
        );
        let mut t = Script::new([Some(Writer::new(ids::INIT).to_bytes())]);
        assert_eq!(
            GEarth::handshake(&mut t, &ExtensionInfo::default(), 30).err(),
            Some(Error::NotGEarth(ids::INIT))
        );
        assert!(t.sent.is_empty());
    }

    #[test]
    fn default_ports_start_at_gearths_onset() {
        assert_eq!(default_ports(), (9092..9102).collect::<Vec<_>>());
    }
}
