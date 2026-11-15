//! The G-Earth client against the fake G-Earth of `support::fake` (std threads and `std::net`;
//! the core itself has no sockets), through [`StdTransport`].
//!
//! These tests cover the G-Earth protocol (handshake, events, sending, answering intercepts);
//! the whole-rip tests on the same fake are in `rip_job.rs`.

mod support;

use std::net::TcpListener;
use std::time::Duration;

use catalog_rip_core::gearth::{self, ids, ConnectionInfo, Error, GEarth, GEarthEvent};
use catalog_rip_core::wire::{Packet, Writer};
use support::fake::*;
use support::*;

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
