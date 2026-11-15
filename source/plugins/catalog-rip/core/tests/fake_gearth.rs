//! A fake G-Earth (the extension server side of the protocol) and a fake hotel answering from
//! the emulator-built packets, on std threads and `std::net` (tests only: the core itself has no
//! sockets). The G-Earth client runs against it through [`StdTransport`].
//!
//! These tests cover the G-Earth protocol (handshake, events, sending, answering intercepts);
//! the whole-rip tests built on the same fake come with the rip.
// The hotel variants, the extra answers and the pacing check are for the rip tests.
#![allow(dead_code)]

mod support;

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use catalog_rip_core::gearth::{self, ids, ConnectionInfo, Error, GEarth, GEarthEvent, PacketInfo};
use catalog_rip_core::wire::{Packet, Reader, Writer};
use support::*;

#[derive(Default, Debug)]
struct Seen {
    /// (header, page id or -1, mode, time) of every packet the extension sent to the server.
    requests: Vec<(u16, i32, String, Instant)>,
    title: String,
    intercepts_sent: usize,
    intercepts_echoed: usize,
    echo_mismatch: usize,
    other_messages: Vec<u16>,
}

/// The hotel behind the fake G-Earth.
#[derive(Clone)]
struct Hotel {
    host: &'static str,
    version: &'static str,
    client_type: &'static str,
    /// G-Earth's packet info (empty for a retro).
    packets: Vec<PacketInfo>,
    /// Older server: no parent ids, no mode / flag after the index, no mode in pages, classic
    /// offers, no Builders Club (its index request goes unanswered).
    retro: bool,
    /// Ignore GetCatalogIndex until the "client" has sent it itself (G-Earth without packet
    /// info / not logged in yet).
    index_locked_until_client: bool,
    /// The user opens the catalogue and a page (repeated until the ripper sends something).
    client_browses: bool,
}

fn packet_infos() -> Vec<PacketInfo> {
    let p = |id: u16, name: &str, out: bool| PacketInfo {
        header_id: id as i32,
        hash: None,
        name: Some(name.into()),
        structure: None,
        to_server: out,
        source: "Sulek".into(),
    };
    vec![
        p(GET_INDEX, "GetCatalogIndex", true),
        p(GET_PAGE, "GetCatalogPage", true),
        p(INDEX, "CatalogIndex", false),
        p(PAGE, "CatalogPage", false),
        p(4000, "ClientHello", true),
        p(999, "Ping", false),
        // Only the club offers have names here: the other extras are skipped for this hotel.
        p(3285, "GetClubOffers", true),
        p(2405, "HabboClubOffers", false),
    ]
}

/// The answers to the extra messages (Arcturus layouts).
fn extra_answer(header: u16) -> Option<Packet> {
    Some(match header {
        3285 => Writer::new(2405)
            .i32(1)
            .i32(7)
            .string("HABBO_CLUB_1_MONTH")
            .bool(false)
            .i32(25)
            .i32(0)
            .i32(0)
            .bool(false)
            .i32(1)
            .i32(31)
            .bool(false)
            .i32(31)
            .i32(2026)
            .i32(12)
            .i32(10)
            .i32(1)
            .packet(),
        223 => Writer::new(2347)
            .i32(100)
            .i32(6)
            .i32(1)
            .i32(1)
            .i32(2)
            .i32(40)
            .i32(99)
            .packet(),
        418 => Writer::new(2234)
            .bool(true)
            .i32(2)
            .i32(1)
            .i32(3372)
            .i32(2)
            .i32(0)
            .i32(1)
            .i32(1)
            .i32(0)
            .i32(0)
            .packet(),
        _ => return None,
    })
}

fn official() -> Hotel {
    Hotel {
        host: "game-nl.habbo.com",
        version: "WIN63-202501011200-123",
        client_type: "FLASH",
        packets: packet_infos(),
        retro: false,
        index_locked_until_client: false,
        client_browses: false,
    }
}

fn flash_retro() -> Hotel {
    Hotel {
        host: "game.myretro.net",
        version: "PRODUCTION-201611291003-338511768",
        client_type: "FLASH",
        packets: Vec::new(),
        retro: true,
        index_locked_until_client: false,
        client_browses: false,
    }
}

fn index_answer(hotel: &Hotel, mode: &str) -> Option<Packet> {
    if !hotel.retro {
        return Some(if mode == "NORMAL" {
            index_packet(INDEX, "NORMAL", &normal_roots(), true, false)
        } else {
            index_packet(INDEX, mode, &builders_roots(), false, false)
        });
    }
    (mode == "NORMAL").then(|| {
        without_index_tail(
            index_packet(INDEX, "NORMAL", &normal_roots(), false, false),
            "NORMAL",
            false,
        )
    })
}

fn retro_page_answer(id: i32) -> Option<Packet> {
    // The classic offer layout (page_answer writes it for any mode but NORMAL), without mode.
    page_answer(id, "BUILDERS_CLUB").map(without_page_mode)
}

/// How often the browsing "client" opens the catalogue.
const BROWSE_EVERY: Duration = Duration::from_millis(150);

/// Plays G-Earth for one extension connection on `listener` until the extension closes it.
fn fake_gearth(listener: TcpListener, seen: Arc<Mutex<Seen>>, hotel: Hotel) {
    let (stream, _) = listener.accept().unwrap();
    stream.set_nodelay(true).ok();
    let mut s = FrameReader::new(stream);
    s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes())
        .unwrap();
    let info = s.read_frame().unwrap();
    assert_eq!(info.header, ids::EXTENSION_INFO);
    seen.lock().unwrap().title = info.reader().string_latin1("title").unwrap();

    // Init (connected) with host info, then the connection start with the packet names.
    let init = Writer::new(ids::INIT)
        .bool(true)
        .string_latin1("G-Earth")
        .string_latin1("1.5.4")
        .i32(1)
        .string_latin1("theme")
        .string_latin1("G-Earth Dark");
    s.write_all(&init.to_bytes()).unwrap();
    let conn = ConnectionInfo {
        host: hotel.host.into(),
        port: 30000,
        hotel_version: hotel.version.into(),
        client_identifier: "".into(),
        client_type: hotel.client_type.into(),
        packets: hotel.packets.clone(),
    };
    s.write_all(&conn.message().to_bytes()).unwrap();

    let mut index = 0;
    let mut pending: Vec<Vec<u8>> = Vec::new(); // intercept bodies waiting for their echo
    let mut send_intercept = |s_buf: &mut Vec<u8>,
                              to_server: bool,
                              packet: &Packet,
                              pending: &mut Vec<Vec<u8>>,
                              seen: &Arc<Mutex<Seen>>| {
        index += 1;
        let msg = gearth::intercept_message(to_server, index, packet);
        pending.push(msg.body.clone());
        seen.lock().unwrap().intercepts_sent += 1;
        s_buf.extend_from_slice(&msg.to_bytes());
    };

    // Unrelated game traffic, with bytes that are not valid text, must come back unchanged.
    let mut out = Vec::new();
    send_intercept(
        &mut out,
        false,
        &Writer::new(999).raw(&[0xFF, 0x09, 0x00, 0x80]).packet(),
        &mut pending,
        &seen,
    );
    send_intercept(
        &mut out,
        true,
        &Writer::new(4000).string("PRODUCTION").packet(),
        &mut pending,
        &seen,
    );
    s.write_all(&out).unwrap();

    let mut slow_page_asked = 0;
    let mut client_opened_catalogue = false;
    let mut index_attempts = 0;
    let mut browsing = hotel.client_browses;
    // tokio's interval ticks at once, then every BROWSE_EVERY.
    let mut next_tick = Instant::now();
    loop {
        let wait = browsing.then(|| next_tick.saturating_duration_since(Instant::now()));
        let frame = match s.next(wait) {
            Ok(Some(frame)) => frame,
            Err(()) => break,
            Ok(None) => {
                // The user's client opens the catalogue and a page; the hotel answers.
                next_tick += BROWSE_EVERY;
                let mut out = Vec::new();
                let get_index = catalog_rip_core::packets::get_catalog_index(GET_INDEX, "NORMAL");
                send_intercept(&mut out, true, &get_index, &mut pending, &seen);
                send_intercept(
                    &mut out,
                    false,
                    &Writer::new(2000).i32(0).packet(),
                    &mut pending,
                    &seen,
                );
                send_intercept(
                    &mut out,
                    false,
                    &index_answer(&hotel, "NORMAL").unwrap(),
                    &mut pending,
                    &seen,
                );
                let get_page = catalog_rip_core::packets::get_catalog_page(GET_PAGE, 3, "NORMAL");
                send_intercept(&mut out, true, &get_page, &mut pending, &seen);
                let page = if hotel.retro {
                    retro_page_answer(3)
                } else {
                    page_answer(3, "NORMAL")
                };
                send_intercept(&mut out, false, &page.unwrap(), &mut pending, &seen);
                if s.write_all(&out).is_err() {
                    break;
                }
                continue;
            }
        };
        let mut out = Vec::new();
        match frame.header {
            ids::MANIPULATED_PACKET => {
                let mut l = seen.lock().unwrap();
                l.intercepts_echoed += 1;
                if pending.is_empty() || pending.remove(0) != frame.body {
                    l.echo_mismatch += 1;
                }
            }
            ids::SEND_MESSAGE => {
                browsing = false;
                let mut r = Reader::new(&frame.body);
                assert_eq!(r.byte("side").unwrap(), 1, "only packets to the server");
                let len = r.i32("len").unwrap() as usize;
                let packet = Packet::from_bytes(r.bytes(len, "packet").unwrap()).unwrap();
                assert_eq!(r.i32("format").unwrap(), 0);
                assert!(r.is_empty());
                if let Some(answer) = extra_answer(packet.header) {
                    seen.lock().unwrap().requests.push((
                        packet.header,
                        -1,
                        String::new(),
                        Instant::now(),
                    ));
                    send_intercept(&mut out, false, &answer, &mut pending, &seen);
                    if s.write_all(&out).is_err() {
                        break;
                    }
                    continue;
                }
                let mut pr = packet.reader();
                let (page_id, mode) = if packet.header == GET_PAGE {
                    let id = pr.i32("id").unwrap();
                    assert_eq!(pr.i32("offer").unwrap(), -1);
                    (id, pr.string("mode").unwrap())
                } else {
                    (-1, pr.string("mode").unwrap())
                };
                seen.lock().unwrap().requests.push((
                    packet.header,
                    page_id,
                    mode.clone(),
                    Instant::now(),
                ));
                match packet.header {
                    GET_INDEX => {
                        index_attempts += 1;
                        if hotel.index_locked_until_client && !client_opened_catalogue {
                            if index_attempts == 2 {
                                // The user opens the catalogue: the client's own request goes by.
                                client_opened_catalogue = true;
                                send_intercept(
                                    &mut out,
                                    true,
                                    &catalog_rip_core::packets::get_catalog_index(
                                        GET_INDEX, "NORMAL",
                                    ),
                                    &mut pending,
                                    &seen,
                                );
                            }
                        } else if let Some(answer) = index_answer(&hotel, &mode) {
                            send_intercept(&mut out, false, &answer, &mut pending, &seen);
                        }
                    }
                    GET_PAGE => {
                        if page_id == 6 && mode == "NORMAL" {
                            slow_page_asked += 1;
                            if slow_page_asked == 1 {
                                continue; // lost: answered only on the retry
                            }
                        }
                        let answer = if hotel.retro {
                            retro_page_answer(page_id)
                        } else {
                            page_answer(page_id, &mode)
                        };
                        if let Some(answer) = answer {
                            send_intercept(&mut out, false, &answer, &mut pending, &seen);
                        }
                    }
                    other => panic!("unexpected packet {other} sent to the server"),
                }
            }
            other => seen.lock().unwrap().other_messages.push(other),
        }
        if !out.is_empty() && s.write_all(&out).is_err() {
            break;
        }
    }
}

/// Starts the fake G-Earth on a free port: (port, what it saw, its thread).
fn start(hotel: Hotel) -> (u16, Arc<Mutex<Seen>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let thread = {
        let seen = seen.clone();
        std::thread::spawn(move || fake_gearth(listener, seen, hotel))
    };
    (port, seen, thread)
}

fn check_protocol(seen: &Seen) {
    assert_eq!(seen.title, "All-in-1 Manager");
    assert_eq!(seen.echo_mismatch, 0);
    assert_eq!(
        seen.intercepts_echoed, seen.intercepts_sent,
        "every intercepted packet answered"
    );
    assert!(
        seen.other_messages
            .iter()
            .all(|&h| h == ids::EXTENSION_CONSOLE_LOG),
        "{:?}",
        seen.other_messages
    );
    for pair in seen.requests.windows(2) {
        assert!(
            pair[1].3.duration_since(pair[0].3) >= Duration::from_millis(18),
            "requests are paced"
        );
    }
}

/// Pumps until `done` holds for the events so far (at most 10 s), returning them all.
fn pump_until(
    g: &mut GEarth,
    t: &mut StdTransport,
    done: impl Fn(&[GEarthEvent]) -> bool,
) -> Vec<GEarthEvent> {
    let mut events = Vec::new();
    for _ in 0..200 {
        events.extend(g.pump(t, 50).unwrap());
        if done(&events) {
            return events;
        }
    }
    panic!("gave up waiting; events so far: {events:?}");
}

fn connected(port: u16) -> (GEarth, StdTransport, ConnectionInfo) {
    let mut t = StdTransport::connect(port);
    let mut g = GEarth::handshake(&mut t, &gearth::ExtensionInfo::default(), 5_000).unwrap();
    let events = pump_until(&mut g, &mut t, |e| {
        e.iter()
            .any(|e| matches!(e, GEarthEvent::ConnectionStart(_)))
    });
    assert!(
        matches!(events[0], GEarthEvent::Init { connected: true }),
        "{events:?}"
    );
    let GEarthEvent::ConnectionStart(info) = events[1].clone() else {
        panic!("{events:?}")
    };
    (g, t, info)
}

#[test]
fn handshake_then_init_and_connection_start() {
    let (port, seen, fake) = start(official());
    let (mut g, mut t, info) = connected(port);
    assert_eq!(info.host, "game-nl.habbo.com");
    assert_eq!(info.port, 30000);
    assert_eq!(info.hotel_version, "WIN63-202501011200-123");
    assert_eq!(info.client_type, "FLASH");
    assert_eq!(info.packets, packet_infos());
    // The two unrelated game packets are not reported, only answered.
    let more = g.pump(&mut t, 100).unwrap();
    assert!(more.is_empty(), "{more:?}");
    t.finish();
    fake.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.intercepts_sent, 2);
    check_protocol(&seen);
}

#[test]
fn sends_to_the_server_and_reports_watched_packets() {
    let (port, seen, fake) = start(official());
    let (mut g, mut t, _) = connected(port);
    g.watch(false, INDEX);
    g.send_to_server(
        &mut t,
        &catalog_rip_core::packets::get_catalog_index(GET_INDEX, "NORMAL"),
    )
    .unwrap();
    let events = pump_until(&mut g, &mut t, |e| !e.is_empty());
    let [GEarthEvent::Packet(p)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert!(!p.to_server);
    assert_eq!(p.index, 3);
    assert_eq!(Some(p.packet.clone()), index_answer(&official(), "NORMAL"));

    // A page request: its answer is not watched, so it is only answered.
    g.send_to_server(
        &mut t,
        &catalog_rip_core::packets::get_catalog_page(GET_PAGE, 3, "NORMAL"),
    )
    .unwrap();
    g.console_log(&mut t, "page 3").unwrap();
    assert!(g.pump(&mut t, 200).unwrap().is_empty());
    t.finish();
    fake.join().unwrap();
    let seen = seen.lock().unwrap();
    let requests: Vec<_> = seen
        .requests
        .iter()
        .map(|r| (r.0, r.1, r.2.as_str()))
        .collect();
    assert_eq!(
        requests,
        [(GET_INDEX, -1, "NORMAL"), (GET_PAGE, 3, "NORMAL")]
    );
    assert_eq!(seen.intercepts_sent, 4);
    assert_eq!(seen.other_messages, [ids::EXTENSION_CONSOLE_LOG]);
    assert_eq!(seen.echo_mismatch, 0);
    assert_eq!(seen.intercepts_echoed, seen.intercepts_sent);
}

#[test]
fn watch_all_reports_the_clients_own_traffic() {
    let hotel = Hotel {
        client_browses: true,
        ..flash_retro()
    };
    let (port, seen, fake) = start(hotel.clone());
    let (mut g, mut t, info) = connected(port);
    assert!(info.packets.is_empty());
    g.watch_all(true);
    // The opening game packets and the first browse may have gone by before watch_all: the
    // client browses again every 150 ms, so look from the first catalogue request on.
    let browse = |events: &[GEarthEvent]| -> Vec<gearth::Intercepted> {
        events
            .iter()
            .map(|e| match e {
                GEarthEvent::Packet(p) => p.clone(),
                other => panic!("{other:?}"),
            })
            .filter(|p| ![999, 4000].contains(&p.packet.header))
            .skip_while(|p| !(p.to_server && p.packet.header == GET_INDEX))
            .take(5)
            .collect()
    };
    let events = pump_until(&mut g, &mut t, |e| browse(e).len() == 5);
    let browsed = browse(&events);
    let packets: Vec<_> = browsed
        .iter()
        .map(|p| (p.to_server, p.packet.header))
        .collect();
    assert_eq!(
        packets,
        [
            (true, GET_INDEX),
            (false, 2000),
            (false, INDEX),
            (true, GET_PAGE),
            (false, PAGE)
        ]
    );
    assert_eq!(Some(browsed[4].packet.clone()), retro_page_answer(3));
    g.watch_all(false);
    t.finish();
    fake.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.echo_mismatch, 0);
    assert_eq!(seen.intercepts_echoed, seen.intercepts_sent);
}

#[test]
fn every_intercept_is_answered_while_idle() {
    const COUNT: usize = 2_000;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut s = FrameReader::new(stream);
        s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes())
            .unwrap();
        assert_eq!(s.read_frame().unwrap().header, ids::EXTENSION_INFO);
        // Unrelated game traffic both ways, then the hotel connection ends: a writer thread,
        // so G-Earth never waits on its own reading.
        let intercepts: Vec<Packet> = (0..COUNT)
            .map(|i| {
                let game = Writer::new(1000 + (i % 50) as u16)
                    .i32(i as i32)
                    .raw(&[0xFF, b'\t', 0x00, 0x80])
                    .string("x\ty")
                    .packet();
                gearth::intercept_message(i % 2 == 0, i as i32 + 1, &game)
            })
            .collect();
        let expected: Vec<Vec<u8>> = intercepts.iter().map(|m| m.body.clone()).collect();
        let mut writer = s.stream.try_clone().unwrap();
        let write = std::thread::spawn(move || {
            use std::io::Write;
            for m in &intercepts {
                writer.write_all(&m.to_bytes()).unwrap();
            }
            writer
                .write_all(&Writer::new(ids::CONNECTION_END).to_bytes())
                .unwrap();
        });
        let mut answers = Vec::new();
        while answers.len() < COUNT {
            let frame = s
                .next(Some(Duration::from_secs(10)))
                .unwrap()
                .expect("an answer");
            assert_eq!(frame.header, ids::MANIPULATED_PACKET);
            answers.push(frame.body);
        }
        write.join().unwrap();
        assert!(answers == expected, "answered in order, unchanged");
        // Nothing more than the answers.
        assert!(s.read_frame().is_none());
    });

    let mut t = StdTransport::connect(port);
    let mut g = GEarth::handshake(&mut t, &gearth::ExtensionInfo::default(), 5_000).unwrap();
    // One pump: it answers every intercept and reports only the connection end.
    let events = g.pump(&mut t, 10_000).unwrap();
    assert!(
        matches!(events.as_slice(), [GEarthEvent::ConnectionEnd]),
        "{events:?}"
    );
    t.finish();
    fake.join().unwrap();
}

#[test]
fn gearth_closing_is_an_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut s = FrameReader::new(stream);
        s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes())
            .unwrap();
        assert_eq!(s.read_frame().unwrap().header, ids::EXTENSION_INFO);
        // G-Earth drops the extension.
    });
    let mut t = StdTransport::connect(port);
    let mut g = GEarth::handshake(&mut t, &gearth::ExtensionInfo::default(), 5_000).unwrap();
    fake.join().unwrap();
    let mut result = Ok(Vec::new());
    for _ in 0..100 {
        result = g.pump(&mut t, 50);
        if result.is_err() {
            break;
        }
    }
    let error = result.unwrap_err();
    assert!(
        matches!(error, Error::Closed | Error::Transport(_)),
        "{error:?}"
    );
    assert!(error.is_connection_lost());
}

#[test]
fn handshake_refuses_what_is_not_gearth() {
    // Something that talks first, but not G-Earth's InfoRequest.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut s = FrameReader::new(stream);
        s.write_all(&Writer::new(ids::INIT).bool(true).to_bytes())
            .unwrap();
        s.read_frame(); // until the extension closes
    });
    let mut t = StdTransport::connect(port);
    let error = GEarth::handshake(&mut t, &gearth::ExtensionInfo::default(), 5_000)
        .err()
        .unwrap();
    assert_eq!(error, Error::NotGEarth(ids::INIT));
    assert_eq!(
        error.to_string(),
        "unexpected first message 7 (expected G-Earth's InfoRequest)"
    );
    drop(t);
    fake.join().unwrap();

    // Something that says nothing at all.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        FrameReader::new(stream).read_frame();
    });
    let mut t = StdTransport::connect(port);
    let error = GEarth::handshake(&mut t, &gearth::ExtensionInfo::default(), 300)
        .err()
        .unwrap();
    assert_eq!(error, Error::NoInfoRequest);
    drop(t);
    fake.join().unwrap();
}
