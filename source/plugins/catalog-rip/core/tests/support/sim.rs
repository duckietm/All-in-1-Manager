//! A simulated G-Earth and hotel on a simulated clock, in memory: the transport's waits move the
//! clock, so pacing and budgets are checked exactly and quickly. A `recv` gives at most one
//! G-Earth frame (a burst takes several reads, as over TCP).

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use catalog_rip_core::gearth::{self, ids, ConnectionInfo};
use catalog_rip_core::job::Clock;
use catalog_rip_core::transport::Transport;
use catalog_rip_core::wire::{Packet, Reader, Writer};

use super::fake::{extra_answer, packet_infos};
use super::mem::UNIX_SECS;
use super::*;

/// The simulated time, shared by the clock and the transport.
#[derive(Clone, Default)]
pub struct SimClock(pub Rc<Cell<u64>>);

impl Clock for SimClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }

    fn unix_secs(&self) -> i64 {
        UNIX_SECS + (self.0.get() / 1000) as i64
    }
}

type Answer = Box<dyn Fn(i32, &str) -> Option<Packet>>;

/// The hotel behind the simulated G-Earth.
pub struct SimHotel {
    pub connection: ConnectionInfo,
    pub index: Box<dyn Fn(&str) -> Option<Packet>>,
    pub page: Answer,
    /// (page id, mode) -> how many of its first requests get no answer.
    pub lose_first: HashMap<(i32, String), usize>,
    /// From G-Earth reading a request to the answer, in ms.
    pub latency_ms: u64,
    /// An unrelated game packet every this many ms (once the hotel connection is up).
    pub noise_every_ms: Option<u64>,
    /// The hotel connection ends at this page request (by count, 1-based).
    pub end_at_page_request: Option<usize>,
    /// (page id, nth request of it) -> (another page, ms): that page's answer comes this long
    /// after the request, unasked (as when the user's client asks for it).
    pub also_answer: HashMap<(i32, usize), (i32, u64)>,
    /// Game packets that go by together (to_server, packet), 20 ms after the hotel connection
    /// starts: the user's client opening the catalogue.
    pub browse_after_start: Vec<(bool, Packet)>,
}

/// The official test hotel of `support` (NORMAL and BUILDERS_CLUB, G-Earth packet info).
pub fn official_hotel() -> SimHotel {
    SimHotel {
        connection: ConnectionInfo {
            host: "game-nl.habbo.com".into(),
            port: 30000,
            hotel_version: "WIN63-202501011200-123".into(),
            client_identifier: String::new(),
            client_type: "FLASH".into(),
            packets: packet_infos(),
        },
        index: Box::new(|mode| {
            Some(if mode == "NORMAL" {
                index_packet(INDEX, "NORMAL", &normal_roots(), true, false)
            } else {
                index_packet(INDEX, mode, &builders_roots(), false, false)
            })
        }),
        page: Box::new(page_answer),
        lose_first: HashMap::new(),
        latency_ms: 5,
        noise_every_ms: None,
        end_at_page_request: None,
        also_answer: HashMap::new(),
        browse_after_start: Vec::new(),
    }
}

/// A hotel with one catalogue (NORMAL) of `pages` pages (ids 1..=pages), each with one offer.
pub fn big_hotel(pages: i32) -> SimHotel {
    let roots = move || -> Vec<Node> {
        (1..=pages)
            .map(|id| node(id, -1, "p", "Page", 1, vec![]))
            .collect()
    };
    SimHotel {
        index: Box::new(move |mode| {
            (mode == "NORMAL").then(|| index_packet(INDEX, "NORMAL", &roots(), true, false))
        }),
        page: Box::new(move |id, mode| {
            (mode == "NORMAL" && (1..=pages).contains(&id)).then(|| {
                page_packet(
                    PAGE,
                    id,
                    mode,
                    "default_3x3",
                    &["h"],
                    &["t"],
                    &[item(
                        id,
                        "chair",
                        1,
                        vec![Prod::Furni {
                            kind: "s",
                            class_id: 4001,
                            extra: "",
                            amount: 1,
                            limited: None,
                        }],
                    )],
                    None,
                    true,
                )
            })
        }),
        ..official_hotel()
    }
}

struct Out {
    bytes: Vec<u8>,
    /// The PacketIntercept body, for checking its echo.
    intercept: Option<Vec<u8>>,
}

/// G-Earth (and the hotel behind it) as a [`Transport`].
pub struct SimGEarth {
    pub clock: SimClock,
    pub hotel: SimHotel,
    inbox: Vec<u8>,
    /// (time, sequence) -> frame.
    outbox: BTreeMap<(u64, u64), Out>,
    seq: u64,
    /// (header, page id or -1, mode, time) of every packet sent to the server.
    pub requests: Vec<(u16, i32, String, u64)>,
    /// Intercepts read by the extension and not answered yet: (body, time read).
    unanswered: VecDeque<(Vec<u8>, u64)>,
    pub intercepts_sent: usize,
    pub intercepts_echoed: usize,
    pub echo_mismatch: usize,
    /// The longest an intercept read by the extension waited for its answer, in ms.
    pub longest_hold_ms: u64,
    next_noise: Option<u64>,
    page_requests: usize,
    asked: HashMap<(i32, String), usize>,
    pub title: String,
}

impl SimGEarth {
    pub fn new(clock: SimClock, hotel: SimHotel) -> SimGEarth {
        let mut sim = SimGEarth {
            clock,
            hotel,
            inbox: Vec::new(),
            outbox: BTreeMap::new(),
            seq: 0,
            requests: Vec::new(),
            unanswered: VecDeque::new(),
            intercepts_sent: 0,
            intercepts_echoed: 0,
            echo_mismatch: 0,
            longest_hold_ms: 0,
            next_noise: None,
            page_requests: 0,
            asked: HashMap::new(),
            title: String::new(),
        };
        sim.schedule(0, Writer::new(ids::INFO_REQUEST).to_bytes(), None);
        sim
    }

    fn now(&self) -> u64 {
        self.clock.0.get()
    }

    fn schedule(&mut self, at: u64, bytes: Vec<u8>, intercept: Option<Vec<u8>>) {
        self.seq += 1;
        self.outbox.insert((at, self.seq), Out { bytes, intercept });
    }

    fn intercept(&mut self, at: u64, to_server: bool, packet: &Packet) {
        self.intercepts_sent += 1;
        let msg = gearth::intercept_message(to_server, self.intercepts_sent as i32, packet);
        self.schedule(at, msg.to_bytes(), Some(msg.body));
    }

    /// Every intercept read was answered, unchanged and in order.
    pub fn check_echoes(&self) {
        assert_eq!(self.echo_mismatch, 0, "intercepts answered unchanged");
        assert!(
            self.unanswered.is_empty(),
            "every intercept read is answered"
        );
    }

    fn handle(&mut self, frame: Packet) {
        let now = self.now();
        let later = now + self.hotel.latency_ms;
        match frame.header {
            ids::EXTENSION_INFO => {
                self.title = frame.reader().string_latin1("title").unwrap();
                let init = Writer::new(ids::INIT)
                    .bool(true)
                    .string_latin1("G-Earth")
                    .string_latin1("1.5.4")
                    .i32(0)
                    .to_bytes();
                self.schedule(later, init, None);
                let start = self.hotel.connection.message().to_bytes();
                self.schedule(later, start, None);
                for (to_server, packet) in std::mem::take(&mut self.hotel.browse_after_start) {
                    self.intercept(later + 20, to_server, &packet);
                }
                if let Some(every) = self.hotel.noise_every_ms {
                    self.next_noise = Some(later + every);
                }
            }
            ids::MANIPULATED_PACKET => {
                self.intercepts_echoed += 1;
                match self.unanswered.pop_front() {
                    Some((body, read_at)) => {
                        if body != frame.body {
                            self.echo_mismatch += 1;
                        }
                        self.longest_hold_ms = self.longest_hold_ms.max(now - read_at);
                    }
                    None => self.echo_mismatch += 1,
                }
            }
            ids::SEND_MESSAGE => {
                let mut r = Reader::new(&frame.body);
                assert_eq!(r.byte("side").unwrap(), 1);
                let len = r.i32("len").unwrap() as usize;
                let packet = Packet::from_bytes(r.bytes(len, "packet").unwrap()).unwrap();
                if let Some(answer) = extra_answer(packet.header) {
                    self.requests.push((packet.header, -1, String::new(), now));
                    self.intercept(later, false, &answer);
                    return;
                }
                let mut pr = packet.reader();
                let (page_id, mode) = if packet.header == GET_PAGE {
                    let id = pr.i32("id").unwrap();
                    pr.i32("offer").unwrap();
                    (id, pr.string("mode").unwrap())
                } else {
                    (-1, pr.string("mode").unwrap())
                };
                self.requests
                    .push((packet.header, page_id, mode.clone(), now));
                let answer = match packet.header {
                    GET_INDEX => (self.hotel.index)(&mode),
                    GET_PAGE => {
                        self.page_requests += 1;
                        if self.hotel.end_at_page_request == Some(self.page_requests) {
                            self.schedule(later, Writer::new(ids::CONNECTION_END).to_bytes(), None);
                            return;
                        }
                        let asked = self.asked.entry((page_id, mode.clone())).or_default();
                        *asked += 1;
                        let nth = *asked;
                        if let Some(&(other, ms)) = self.hotel.also_answer.get(&(page_id, nth)) {
                            if let Some(answer) = (self.hotel.page)(other, &mode) {
                                self.intercept(now + ms, false, &answer);
                            }
                        }
                        let asked = &self.asked[&(page_id, mode.clone())];
                        let lost = self
                            .hotel
                            .lose_first
                            .get(&(page_id, mode.clone()))
                            .copied()
                            .unwrap_or(0);
                        if *asked <= lost {
                            None
                        } else {
                            (self.hotel.page)(page_id, &mode)
                        }
                    }
                    other => panic!("unexpected packet {other} sent to the server"),
                };
                if let Some(answer) = answer {
                    self.intercept(later, false, &answer);
                }
            }
            _ => {}
        }
    }

    fn take_frame(&mut self) -> Option<Packet> {
        if self.inbox.len() < 4 {
            return None;
        }
        let len = i32::from_be_bytes([self.inbox[0], self.inbox[1], self.inbox[2], self.inbox[3]])
            as usize;
        if self.inbox.len() < 4 + len {
            return None;
        }
        let frame: Vec<u8> = self.inbox.drain(..4 + len).collect();
        Some(Packet {
            header: u16::from_be_bytes([frame[4], frame[5]]),
            body: frame[6..].to_vec(),
        })
    }

    fn noise_until(&mut self, until: u64) {
        let Some(every) = self.hotel.noise_every_ms else {
            return;
        };
        while let Some(at) = self.next_noise.filter(|&at| at <= until) {
            let game = Writer::new(1000 + (at % 7) as u16)
                .i32(at as i32)
                .raw(&[0xFF, b'\t', 0x00])
                .packet();
            self.intercept(at, at % 2 == 0, &game);
            self.next_noise = Some(at + every);
        }
    }
}

impl Transport for SimGEarth {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.inbox.extend_from_slice(data);
        while let Some(frame) = self.take_frame() {
            self.handle(frame);
        }
        Ok(())
    }

    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        let now = self.now();
        let until = now + u64::from(timeout_ms);
        self.noise_until(until);
        let Some((&(at, seq), _)) = self.outbox.iter().next() else {
            self.clock.0.set(until);
            return Ok(None);
        };
        if at > until {
            self.clock.0.set(until);
            return Ok(None);
        }
        if at > now {
            self.clock.0.set(at);
        }
        let out = self.outbox.remove(&(at, seq)).unwrap();
        assert!(out.bytes.len() <= max);
        if let Some(body) = out.intercept {
            self.unanswered.push_back((body, self.now()));
        }
        Ok(Some(out.bytes))
    }
}
