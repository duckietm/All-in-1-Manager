//! A fake G-Earth (the extension server side of the protocol) and a fake hotel answering from
//! the emulator-built packets, on std threads and `std::net` (tests only: the core itself has no
//! sockets). The G-Earth client and the rip job run against it through [`super::StdTransport`].

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use catalog_rip_core::gearth::{self, ids, ConnectionInfo, PacketInfo};
use catalog_rip_core::wire::{Packet, Reader, Writer};

use super::*;

#[derive(Default, Debug)]
pub struct Seen {
    /// (header, page id or -1, mode, time) of every packet the extension sent to the server.
    pub requests: Vec<(u16, i32, String, Instant)>,
    pub title: String,
    pub intercepts_sent: usize,
    pub intercepts_echoed: usize,
    pub echo_mismatch: usize,
    pub other_messages: Vec<u16>,
}

/// The hotel behind the fake G-Earth.
#[derive(Clone)]
pub struct Hotel {
    pub host: &'static str,
    pub version: &'static str,
    pub client_type: &'static str,
    /// G-Earth's packet info (empty for a retro).
    pub packets: Vec<PacketInfo>,
    /// Older server: no parent ids, no mode / flag after the index, no mode in pages, classic
    /// offers, no Builders Club (its index request goes unanswered).
    pub retro: bool,
    /// Ignore GetCatalogIndex until the "client" has sent it itself (G-Earth without packet
    /// info / not logged in yet).
    pub index_locked_until_client: bool,
    /// The user opens the catalogue and a page (repeated until the ripper sends something).
    pub client_browses: bool,
}

pub fn packet_infos() -> Vec<PacketInfo> {
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
pub fn extra_answer(header: u16) -> Option<Packet> {
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

pub fn official() -> Hotel {
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

pub fn flash_retro() -> Hotel {
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

pub fn index_answer(hotel: &Hotel, mode: &str) -> Option<Packet> {
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

pub fn retro_page_answer(id: i32) -> Option<Packet> {
    // The classic offer layout (page_answer writes it for any mode but NORMAL), without mode.
    page_answer(id, "BUILDERS_CLUB").map(without_page_mode)
}

/// How often the browsing "client" opens the catalogue.
pub const BROWSE_EVERY: Duration = Duration::from_millis(150);

/// Plays G-Earth for one extension connection on `listener` until the extension closes it.
pub fn fake_gearth(listener: TcpListener, seen: Arc<Mutex<Seen>>, hotel: Hotel) {
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
pub fn start(hotel: Hotel) -> (u16, Arc<Mutex<Seen>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let thread = {
        let seen = seen.clone();
        std::thread::spawn(move || fake_gearth(listener, seen, hotel))
    };
    (port, seen, thread)
}

pub fn check_protocol(seen: &Seen) {
    assert_eq!(seen.title, "Catalogue ripper (All-in-1 Manager)");
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
