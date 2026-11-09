//! End to end: a fake G-Earth (the extension server side of the protocol) and a fake hotel
//! answering from the emulator-built packets; the real rip runs against them, once as an
//! official Habbo hotel (header ids from G-Earth's packet info) and once as retros (no packet
//! info: the ids come from the built-in Arcturus / Nitro profiles, and the server sends the
//! older packet variants).

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aio_catalog_rip::gearth::{self, ids, ConnectionInfo, PacketInfo};
use aio_catalog_rip::profiles::{self, builtin_profiles, HotelKind};
use aio_catalog_rip::wire::{Packet, Reader, Writer};
use aio_catalog_rip::{
    reexport, rip_with, Cancelled, ExistingCatalogue, ItemsBaseRow, ItemsSource, ManagerMode,
    PartialOutput, ProfileChoice, RipEvent, RipHooks, RipOptions, RipProgress, RipResult,
};
use futures::future::BoxFuture;
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

async fn read_frame(s: &mut TcpStream) -> Option<Packet> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await.ok()?;
    let mut payload = vec![0u8; i32::from_be_bytes(len) as usize];
    s.read_exact(&mut payload).await.ok()?;
    Some(Packet {
        header: u16::from_be_bytes([payload[0], payload[1]]),
        body: payload[2..].to_vec(),
    })
}

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
        223 => Writer::new(2347).i32(100).i32(6).i32(1).i32(1).i32(2).i32(40).i32(99).packet(),
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

async fn fake_gearth(listener: TcpListener, seen: Arc<Mutex<Seen>>, hotel: Hotel) {
    let (mut s, _) = listener.accept().await.unwrap();
    s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes())
        .await
        .unwrap();
    let info = read_frame(&mut s).await.unwrap();
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
    s.write_all(&init.to_bytes()).await.unwrap();
    let conn = ConnectionInfo {
        host: hotel.host.into(),
        port: 30000,
        hotel_version: hotel.version.into(),
        client_identifier: "".into(),
        client_type: hotel.client_type.into(),
        packets: hotel.packets.clone(),
    };
    s.write_all(&conn.message().to_bytes()).await.unwrap();

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
    s.write_all(&out).await.unwrap();

    let mut slow_page_asked = 0;
    let mut client_opened_catalogue = false;
    let mut index_attempts = 0;
    let mut browsing = hotel.client_browses;
    let mut ticker = tokio::time::interval(Duration::from_millis(150));
    loop {
        let frame = tokio::select! {
            frame = read_frame(&mut s) => frame,
            _ = ticker.tick(), if browsing => {
                // The user's client opens the catalogue and a page; the hotel answers.
                let mut out = Vec::new();
                let get_index = aio_catalog_rip::packets::get_catalog_index(GET_INDEX, "NORMAL");
                send_intercept(&mut out, true, &get_index, &mut pending, &seen);
                send_intercept(&mut out, false, &Writer::new(2000).i32(0).packet(), &mut pending, &seen);
                send_intercept(&mut out, false, &index_answer(&hotel, "NORMAL").unwrap(), &mut pending, &seen);
                let get_page = aio_catalog_rip::packets::get_catalog_page(GET_PAGE, 3, "NORMAL");
                send_intercept(&mut out, true, &get_page, &mut pending, &seen);
                let page = if hotel.retro { retro_page_answer(3) } else { page_answer(3, "NORMAL") };
                send_intercept(&mut out, false, &page.unwrap(), &mut pending, &seen);
                if s.write_all(&out).await.is_err() {
                    break;
                }
                continue;
            }
        };
        let Some(frame) = frame else { break };
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
                if let Some(answer) = extra_answer(packet.header) {
                    seen.lock().unwrap().requests.push((packet.header, -1, String::new(), Instant::now()));
                    send_intercept(&mut out, false, &answer, &mut pending, &seen);
                    if s.write_all(&out).await.is_err() {
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
                                    &aio_catalog_rip::packets::get_catalog_index(
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
        if !out.is_empty() && s.write_all(&out).await.is_err() {
            break;
        }
    }
}

/// A tiny HTTP server for the image downloads: `<name>.png` exists, `.gif` does not, except
/// for the names listed as gif; icons exist for 1-4.
async fn fake_cdn(listener: TcpListener) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let found = (path.starts_with("/c_images/catalogue/catalog_header")
                && path.ends_with(".gif"))
                || (path.starts_with("/c_images/catalogue/")
                    && path.ends_with(".png")
                    && !path.contains("missing"))
                || path.starts_with("/icons/icon_");
            let body = format!("image:{path}");
            let response = if found {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            };
            let _ = s.write_all(response.as_bytes()).await;
        });
    }
}

fn options(port: u16, root: &std::path::Path) -> RipOptions {
    RipOptions {
        ports: vec![port],
        output_folder: root.to_path_buf(),
        delay_ms: 20,
        timeout_ms: 400,
        gearth_wait_ms: 5_000,
        hotel_wait_ms: 10_000,
        resend_every_ms: 500,
        download_images: false,
        use_furnidata: false,
        ..RipOptions::default()
    }
}

/// What the callbacks saw.
#[derive(Default)]
struct Calls {
    log: Mutex<Vec<String>>,
    asked: Mutex<Vec<String>>,
    progress: Mutex<Vec<RipProgress>>,
    events: Mutex<Vec<RipEvent>>,
}

impl Calls {
    fn log_text(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }
}

async fn run(
    opts: &RipOptions,
    choice: ProfileChoice,
    items: Option<&dyn ItemsSource>,
    calls: &Calls,
    cancel_after_pages: Option<usize>,
) -> anyhow::Result<RipResult> {
    let cancel = CancellationToken::new();
    let log = |line: &str| calls.log.lock().unwrap().push(line.to_string());
    let ask = |q: String| -> BoxFuture<'static, bool> {
        calls.asked.lock().unwrap().push(q);
        Box::pin(async { false })
    };
    let progress = |p: RipProgress| {
        if cancel_after_pages.is_some_and(|n| p.done >= n) {
            cancel.cancel();
        }
        calls.progress.lock().unwrap().push(p);
    };
    let event = |e: &RipEvent| calls.events.lock().unwrap().push(e.clone());
    let hooks = RipHooks {
        event: Some(&event),
        items,
        ..RipHooks::new(&log, &ask, &progress)
    };
    rip_with(opts, choice, &hooks, &cancel).await
}

fn detect() -> ProfileChoice {
    ProfileChoice::Detect(builtin_profiles())
}

fn items_base() -> Vec<ItemsBaseRow> {
    let row = |id, sprite_id, t: &str, name: &str| ItemsBaseRow {
        id,
        sprite_id,
        item_type: t.into(),
        item_name: name.into(),
        effect_id: 0,
    };
    vec![
        row(10, 4001, "s", "rare_dragon"),
        row(11, 4002, "s", "table"),
        row(12, 5001, "i", "poster"),
        row(13, 0, "b", "ACH_Test1"),
        row(14, 3001, "i", "wallpaper"),
        row(15, 4003, "s", "chair"),
        row(16, 4004, "s", "color_chair"),
    ]
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

#[tokio::test]
async fn rips_an_official_hotel_through_a_fake_gearth() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let server = tokio::spawn(fake_gearth(listener, seen.clone(), official()));
    let cdn = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdn_port = cdn.local_addr().unwrap().port();
    tokio::spawn(fake_cdn(cdn));

    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.download_images = true;
    opts.catalog_url = Some(format!("http://127.0.0.1:{cdn_port}/c_images/catalogue"));
    opts.icon_url = Some(format!("http://127.0.0.1:{cdn_port}/icons/icon_"));
    let calls = Calls::default();
    let report = run(&opts, detect(), None, &calls, None).await.unwrap();
    server.await.unwrap();

    // --- protocol
    let seen = seen.lock().unwrap();
    check_protocol(&seen);
    let requests: Vec<(u16, i32, &str)> = seen
        .requests
        .iter()
        .map(|(h, id, m, _)| (*h, *id, m.as_str()))
        .collect();
    assert_eq!(requests[0], (GET_INDEX, -1, "NORMAL"));
    assert_eq!(requests[1], (GET_INDEX, -1, "BUILDERS_CLUB"));
    // After the pages: the club offers (named in G-Earth's info); the other extras have no ids.
    assert_eq!(requests.last(), Some(&(3285, -1, "")));
    let requests = &requests[..requests.len() - 1];
    let pages: Vec<(i32, &str)> = requests[2..]
        .iter()
        .map(|(h, id, m)| {
            assert_eq!(*h, GET_PAGE);
            (*id, *m)
        })
        .collect();
    // depth first; 6 retried once (answered), 7 retried once (never answered); -1 never asked
    assert_eq!(
        pages,
        vec![
            (1, "NORMAL"),
            (2, "NORMAL"),
            (3, "NORMAL"),
            (4, "NORMAL"),
            (5, "NORMAL"),
            (6, "NORMAL"),
            (6, "NORMAL"),
            (7, "NORMAL"),
            (7, "NORMAL"),
            (100, "BUILDERS_CLUB"),
            (4, "BUILDERS_CLUB")
        ]
    );

    // --- callbacks
    assert!(calls.asked.lock().unwrap().is_empty(), "no G-Earth folder: never asks");
    let progress = calls.progress.lock().unwrap();
    assert_eq!((progress[0].done, progress[0].total), (0, 9));
    assert_eq!(progress.last().unwrap().done, 9);
    assert!(progress.windows(2).all(|w| w[1].done == w[0].done + 1));
    let log = calls.log_text();
    assert!(log.contains("Hotel profile: Habbo (official)"), "{log}");
    assert!(log.contains("GetCatalogIndex 1195 (G-Earth)"), "{log}");
    // Pages 6 and 7 went unanswered once each: the delay (at least 50 ms) doubled twice.
    assert!(log.contains("slowing down: 100 ms"), "{log}");
    assert_eq!(report.delay_ms, 200);
    let events = calls.events.lock().unwrap();
    assert!(events.contains(&RipEvent::DelayChanged { delay_ms: 200 }), "{events:?}");
    assert!(events.contains(&RipEvent::IndexRead { mode: "NORMAL".into(), pages: 8 }), "{events:?}");
    assert!(events.contains(&RipEvent::PageDone { mode: "NORMAL".into(), page_id: 7, ok: false }));
    assert!(matches!(events.last(), Some(RipEvent::Finished { complete: true, .. })));
    drop(events);

    // --- result
    assert!(report.complete);
    assert_eq!(report.profile, profiles::OFFICIAL);
    assert_eq!(report.kind, HotelKind::Official);
    assert_eq!(report.headers.page, PAGE);
    assert_eq!(report.pages, 8); // 1-6 NORMAL, 100 + 4 BUILDERS_CLUB
    assert_eq!(report.pages_failed, 1);
    assert_eq!(report.items, 7);
    assert_eq!(report.frontpage_items, 3);
    assert_eq!(report.sql.pages, 10); // 1..7 + the disabled page; 100 and 4 in the _bc tables
    assert_eq!((report.sql.bc_pages, report.sql.bc_items), (2, 1));
    assert_eq!(report.sql.synthetic_page_ids, 1);
    assert_eq!(report.sql.items, 7); // 5 shop offers + color chair + bc table
    assert_eq!(report.manager.pages, 10); // 8 NORMAL + 100 and 4 in the warehouse
    assert_eq!(report.manager.offers, 0, "no items_base: no literal item_ids");
    let out = &report.folder;
    assert!(out
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("habbo.nl_"));

    // --- catalog.json
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("catalog.json")).unwrap()).unwrap();
    assert_eq!(json["hotel"], "habbo.nl");
    assert_eq!(json["client_type"], "FLASH");
    let normal = &json["catalogs"][0];
    assert_eq!(normal["mode"], "NORMAL");
    assert_eq!(normal["failed_pages"], serde_json::json!([7]));
    let front = &normal["pages"][0];
    assert_eq!(front["layout"], "frontpage4");
    assert_eq!(front["frontpage_items"][1]["product_offer_id"], 300);
    let shop = &normal["pages"][1]["children"][0];
    assert_eq!(shop["id"], 3);
    assert_eq!(shop["parent_id"], 2);
    assert_eq!(shop["offers"][0]["products"][0]["limited_series_size"], 100);
    assert_eq!(shop["offers"][0]["products"][0]["limited_items_left"], 63);
    assert_eq!(shop["offers"][1]["products"][0]["product_type"], "b");
    assert_eq!(shop["offers"][2]["item_ids"], "12:2;13:1");
    let hidden = &normal["pages"][1]["children"][2];
    assert_eq!(hidden["visible"], false);
    assert_eq!(hidden["children"][0]["id"], 5);
    assert_eq!(normal["pages"][3]["page_status"], "no answer");
    assert_eq!(json["catalogs"][1]["mode"], "BUILDERS_CLUB");
    assert_eq!(json["catalogs"][1]["pages"][0]["packet_layout"], "classic");

    // --- SQL
    let pages_sql = std::fs::read_to_string(out.join("catalog_pages.sql")).unwrap();
    let items_sql = std::fs::read_to_string(out.join("catalog_items.sql")).unwrap();
    assert!(pages_sql.contains("(1, -1, 'front_page', 'Front Page', 'frontpage', 1, 1, 1, 1, '1', '1', '0', 'NORMAL', '0', 'catalog_frontpage_headline_shop_EN', 'front_teaser', '', 'Redeem a voucher code here:', 'Second text', '', 'Teaser text', 0, '')"), "{pages_sql}");
    assert!(pages_sql.contains("(3, 2, 'shop_3x3', 'Shop 3x3', 'default_3x3', 1, 3, 1, 1, '1', '1', '0', 'NORMAL', '0', 'catalog_header_roombuilder', 'cr_infopic3', 'catalog_special_txtbg1', 'It\\'s a \\\"shop\\\"\\nline 2', '', 'Details \\\\ backslash', 'Teaser', 0, '')"), "{pages_sql}");
    assert!(pages_sql.contains("(4, 2, 'rentables', 'Rentables', 'info_rentables', 1, 4, 1, 2, '1', '1', '0', 'NORMAL', '0', 'catalog_rentables_header', '', '', 'Rent one||Rent two'"), "{pages_sql}");
    assert!(pages_sql.contains("(8, 2, 'hidden_cat', 'Hidden category', 'default_3x3', 1, 5, 1, 3, '0', '0', '0', 'NORMAL'"), "{pages_sql}");
    // recolorable_furniture is in Polaris' enum (migration V20261014120000): kept as is.
    assert!(pages_sql.contains("(5, 8, 'hidden_child', 'Hidden child', 'recolorable_furniture'"), "{pages_sql}");
    assert!(!pages_sql.contains("bc_addons"), "{pages_sql}");
    let pages_bc = std::fs::read_to_string(out.join("catalog_pages_bc.sql")).unwrap();
    let items_bc = std::fs::read_to_string(out.join("catalog_items_bc.sql")).unwrap();
    assert!(pages_bc.contains("(100, -1, 'BC Addons', 'builders_club_addons', 1, 9, 1, '1', '1', 'bc_header', 'bc_teaser', ''"), "{pages_bc}");
    assert!(items_bc.contains("INSERT INTO `catalog_items_bc` (`id`, `item_ids`, `page_id`, `catalog_name`, `order_number`, `extradata`)"), "{items_bc}");
    assert!(items_bc.contains(", 100, 'bc_table', 1, '')"), "{items_bc}");
    assert!(pages_sql.contains("(6, -1, 'slow_page', 'Slow page', 'pets', 1, 7, 1, 3, '1', '1', '0', 'NORMAL', '0', 'pets_h', 'pets_t', '', 'p1', 'p2', 'pd', 'pt'"), "{pages_sql}");
    assert!(items_sql.contains("(300, CONCAT_WS(';', (SELECT `id` FROM `items_base` WHERE `type` = 's' AND `sprite_id` = 4001 ORDER BY `id` LIMIT 1)), 3, 'rare_dragon_ltd', 50, 0, 0, 1, 100, 37, 1, 300, 0, '', '1', '0')"), "{items_sql}");
    assert!(items_sql.contains("(302, CONCAT_WS(';', (SELECT CONCAT(`id`, ':2') FROM `items_base` WHERE `type` = 's' AND `sprite_id` = 4002 ORDER BY `id` LIMIT 1), (SELECT CONCAT(`id`, ':1') FROM `items_base` WHERE `type` = 'i' AND `sprite_id` = 5001 ORDER BY `id` LIMIT 1)), 3, 'bundle_o\\'brien', 25, 0, 0, 1, 0, 0, 3, 302, 0, '', '0', '1')"), "{items_sql}");
    assert!(
        items_sql.contains(", 3, 'chair_x5', 5, 0, 0, 5, 0, 0, 5, 304,"),
        "{items_sql}"
    );
    let manager_sql = std::fs::read_to_string(out.join("catalog_manager.sql")).unwrap();
    assert!(manager_sql.contains("INSERT INTO catalog_pages (id, catalog_type, "), "{manager_sql}");
    assert!(manager_sql.contains("-- skipped: NORMAL offer 300"), "{manager_sql}");

    // --- images: header as gif, others png, icons, frontpage promos; missing ones counted
    let images = out.join("images");
    assert!(images.join("catalog_header_roombuilder.gif").is_file());
    assert!(images.join("cr_infopic3.png").is_file());
    assert!(images.join("icons/icon_3.png").is_file());
    assert!(images.join("frontpage/feature_cata_vert_new.png").is_file());
    assert_eq!(
        std::fs::read_to_string(images.join("frontpage/feature_cata_vert_new.png")).unwrap(),
        "image:/c_images/catalogue/feature_cata_vert_new.png"
    );
    assert_eq!(report.images.failed, 0);
    assert!(report.images.downloaded >= 20, "{:?}", report.images);

    assert!(out.join("packets/session.json").is_file());
    assert!(out.join("packets/page_NORMAL_3.bin").is_file());
    assert!(out.join("packets/index_BUILDERS_CLUB.bin").is_file());
    let readme = std::fs::read_to_string(out.join("README.txt")).unwrap();
    assert!(readme.contains("Import catalog_pages.sql, then catalog_items.sql"));
    assert!(readme.contains("ALL-IN-1 MANAGER IMPORT"));
    let extras: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("catalog_extras.json")).unwrap()).unwrap();
    assert_eq!(extras["club_offers"][0]["price_credits"], 25);
    assert!(extras.get("bundle_discount").is_none());
    assert!(calls.log_text().contains("BundleDiscountRuleset: no header ids"));
    assert!(report.indexes.contains_key("BUILDERS_CLUB"));
    assert!(report.changes.is_none(), "no earlier rip to compare with");
    assert!(readme.contains("Habbo (official) (official Habbo)"));
}

#[tokio::test]
async fn rips_a_flash_retro_with_the_arcturus_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let server = tokio::spawn(fake_gearth(listener, seen.clone(), flash_retro()));
    let dir = tempfile::tempdir().unwrap();
    let opts = options(port, dir.path());
    let calls = Calls::default();
    let items = items_base();
    let report = run(&opts, detect(), Some(&items), &calls, None)
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    server.await.unwrap();

    let seen = seen.lock().unwrap();
    check_protocol(&seen);
    let requests: Vec<(u16, i32, &str)> = seen
        .requests
        .iter()
        .map(|(h, id, m, _)| (*h, *id, m.as_str()))
        .collect();
    // The Builders Club index goes unanswered: asked once more, then skipped.
    assert_eq!(
        requests,
        vec![
            (GET_INDEX, -1, "NORMAL"),
            (GET_INDEX, -1, "BUILDERS_CLUB"),
            (GET_INDEX, -1, "BUILDERS_CLUB"),
            (GET_PAGE, 1, "NORMAL"),
            (GET_PAGE, 2, "NORMAL"),
            (GET_PAGE, 3, "NORMAL"),
            (GET_PAGE, 4, "NORMAL"),
            (GET_PAGE, 5, "NORMAL"),
            (GET_PAGE, 6, "NORMAL"),
            (GET_PAGE, 6, "NORMAL"),
            (GET_PAGE, 7, "NORMAL"),
            (GET_PAGE, 7, "NORMAL"),
            (3285, -1, ""),
            (223, -1, ""),
            (418, -1, ""),
        ]
    );
    let extras: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report.folder.join("catalog_extras.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(extras["bundle_discount"]["bundle_size"], 6);
    assert_eq!(extras["gift_wrapping"]["gift_wrappers"], serde_json::json!([3372]));
    assert!(report.folder.join("packets/extra_gift_wrapping.bin").is_file());

    let log = calls.log_text();
    assert!(log.contains("picked by the client revision PRODUCTION-201611291003-338511768"), "{log}");
    assert!(log.contains("GetCatalogIndex 1195 (profile)"), "{log}");
    assert!(log.contains("BUILDERS_CLUB: no index"), "{log}");
    assert_eq!(report.profile, profiles::ARCTURUS_FLASH);
    assert_eq!(report.kind, HotelKind::FlashRetro);
    assert!(report.complete);
    assert_eq!(report.pages, 6);
    assert_eq!(report.pages_failed, 1);
    assert_eq!(report.items, 6);
    assert_eq!(report.frontpage_items, 3);
    assert!(report.folder.file_name().unwrap().to_string_lossy().starts_with("game.myretro.net_"));

    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report.folder.join("catalog.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["catalogs"].as_array().unwrap().len(), 1);
    let normal = &json["catalogs"][0];
    assert_eq!(normal["mode"], "NORMAL");
    assert_eq!(normal["index_has_parent_ids"], false);
    let shop = &normal["pages"][1]["children"][0];
    assert_eq!(shop["packet_layout"], "classic without mode");
    assert_eq!(shop["offers"][2]["products"][1]["extra_param"], "poster 7");

    // Every product is in items_base: the Manager file has every offer, literal ids.
    assert_eq!(report.manager.offers, 6);
    assert_eq!(report.manager.offers_unresolved, 0);
    let manager_sql = std::fs::read_to_string(report.folder.join("catalog_manager.sql")).unwrap();
    let target = aio_remote::catalog_transfer::target(&Default::default(), &manager_sql)
        .unwrap_or_else(|e| panic!("{e}\n{manager_sql}"));
    let entity = |page, id| aio_remote::catalog_journal::Entity { builders: false, page, id };
    assert_eq!(target[&entity(false, 302)]["itemIds"], "11:2;12:1");
    assert_eq!(target[&entity(false, 304)]["amount"], 5);
    assert_eq!(target[&entity(true, 3)]["catalogType"], "NORMAL");
    let items_sql = std::fs::read_to_string(report.folder.join("catalog_items.sql")).unwrap();
    assert!(items_sql.contains("(300, '10', 3,"), "{items_sql}");
}

#[tokio::test]
async fn rips_a_nitro_retro_with_the_nitro_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let hotel = Hotel {
        host: "ws.mynitro.net",
        version: "",
        client_type: "NITRO",
        ..flash_retro()
    };
    let server = tokio::spawn(fake_gearth(listener, seen.clone(), hotel));
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    let calls = Calls::default();
    let report = run(&opts, detect(), None, &calls, None)
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    server.await.unwrap();
    assert_eq!(report.profile, profiles::NITRO);
    assert_eq!(report.kind, HotelKind::NitroRetro);
    assert_eq!(report.pages, 6);
    assert!(seen.lock().unwrap().requests.iter().all(|r| r.2 == "NORMAL" || r.2.is_empty()));
}

#[tokio::test]
async fn an_unverified_retro_profile_stops_with_a_clear_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let hotel = Hotel {
        version: "PRODUCTION-201601012205-226667486",
        ..flash_retro()
    };
    tokio::spawn(fake_gearth(listener, seen.clone(), hotel));
    let dir = tempfile::tempdir().unwrap();
    let calls = Calls::default();
    let mut opts = options(port, dir.path());
    opts.learn_headers = false;
    let err = run(&opts, detect(), None, &calls, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(profiles::PLUS_FLASH), "{err}");
    assert!(err.contains("set the four ids in the hotel profile"), "{err}");
    assert!(seen.lock().unwrap().requests.is_empty(), "nothing sent");
}

#[tokio::test]
async fn a_fixed_profile_with_override_ids() {
    // The user picked a profile whose ids are wrong; the overrides fix them.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), flash_retro()));
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    opts.header_overrides =
        aio_catalog_rip::HeaderMap::parse(&format!("CatalogPage={PAGE}")).unwrap();
    let mut profile = builtin_profiles()
        .into_iter()
        .find(|p| p.name == profiles::NITRO)
        .unwrap();
    profile.headers.catalog_page = Some(1);
    profile.name = "Mine".into();
    let calls = Calls::default();
    let report = run(&opts, ProfileChoice::Fixed(profile), None, &calls, None)
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    assert_eq!(report.profile, "Mine");
    assert_eq!(report.pages, 6);
    assert!(calls.log_text().contains(&format!("CatalogPage {PAGE} (override)")));
}

#[tokio::test]
async fn waits_until_the_client_opened_the_catalogue() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let hotel = Hotel {
        index_locked_until_client: true,
        ..official()
    };
    let server = tokio::spawn(fake_gearth(listener, seen.clone(), hotel));
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    let report = run(&opts, detect(), None, &Calls::default(), None)
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(report.pages, 6);
    let seen = seen.lock().unwrap();
    let index_requests = seen.requests.iter().filter(|r| r.0 == GET_INDEX).count();
    assert_eq!(
        index_requests, 3,
        "first try, the periodic retry, then again after the client's own request"
    );
}

#[tokio::test]
async fn cancelling_keeps_what_was_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), official()));
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.download_images = true; // skipped: cancelled
    let calls = Calls::default();
    let err = run(&opts, detect(), None, &calls, Some(2)).await.unwrap_err();
    assert!(err.is::<Cancelled>(), "{err:#}");
    let partial = err.downcast_ref::<PartialOutput>().expect("partial output");
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert!(err.to_string().contains("saved in"), "{err}");
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(partial.folder.join("catalog.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["complete"], false);
    assert!(!partial.folder.join("images").exists());
    let pages = seen.lock().unwrap().requests.iter().filter(|r| r.0 == GET_PAGE).count();
    assert_eq!(pages, 2, "nothing sent after the cancel");
}

#[tokio::test]
async fn fails_clearly_without_gearth() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.gearth_wait_ms = 100;
    // A G-Earth folder: the rip asks before downloading; "no" downloads nothing.
    opts.gearth_folder = Some(dir.path().join("G-Earth"));
    let calls = Calls::default();
    let err = run(&opts, detect(), None, &calls, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("G-Earth was not found"), "{err}");
    let asked = calls.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].contains("Download the official G-Earth"), "{asked:?}");
    assert!(!dir.path().join("G-Earth").exists());
}

#[tokio::test]
async fn hotel_disconnect_keeps_partial_output() {
    // G-Earth announces the connection, answers the index, then the hotel connection ends.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes())
            .await
            .unwrap();
        read_frame(&mut s).await.unwrap();
        let conn = ConnectionInfo {
            host: "game-de.habbo.com".into(),
            port: 1,
            hotel_version: "v".into(),
            client_identifier: "".into(),
            client_type: "FLASH".into(),
            packets: packet_infos(),
        };
        s.write_all(
            &Writer::new(ids::INIT)
                .bool(true)
                .string_latin1("G-Earth")
                .string_latin1("1")
                .i32(0)
                .to_bytes(),
        )
        .await
        .unwrap();
        s.write_all(&conn.message().to_bytes()).await.unwrap();
        while let Some(frame) = read_frame(&mut s).await {
            if frame.header == ids::SEND_MESSAGE {
                let mut r = Reader::new(&frame.body);
                r.byte("side").unwrap();
                let len = r.i32("len").unwrap() as usize;
                let packet = Packet::from_bytes(r.bytes(len, "p").unwrap()).unwrap();
                if packet.header == GET_INDEX {
                    let msg = gearth::intercept_message(
                        false,
                        1,
                        &index_packet(INDEX, "NORMAL", &normal_roots(), false, false),
                    );
                    s.write_all(&msg.to_bytes()).await.unwrap();
                } else {
                    s.write_all(&Writer::new(ids::CONNECTION_END).to_bytes())
                        .await
                        .unwrap();
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    let err = run(&opts, detect(), None, &Calls::default(), None)
        .await
        .unwrap_err();
    assert!(!err.is::<Cancelled>());
    let text = err.to_string();
    assert!(text.contains("hotel connection ended"), "{text}");
    assert!(text.contains("saved in"), "{text}");
    let out = aio_catalog_rip::rip::latest_output(dir.path()).unwrap();
    assert_eq!(err.downcast_ref::<PartialOutput>().unwrap().folder, out);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("catalog.json")).unwrap()).unwrap();
    assert_eq!(json["complete"], false);
    assert!(std::fs::read_to_string(out.join("README.txt"))
        .unwrap()
        .contains("INCOMPLETE"));
    assert!(out.join("catalog_pages.sql").is_file());
    assert!(out.join("catalog_manager.sql").is_file());
}


#[tokio::test]
async fn learns_the_header_ids_of_an_unknown_retro() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    // No profile matches (not a PRODUCTION- revision) and G-Earth has no packet info.
    let hotel = Hotel {
        host: "game.unknown-retro.org",
        version: "WIN63-custom",
        client_browses: true,
        ..flash_retro()
    };
    let server = tokio::spawn(fake_gearth(listener, seen.clone(), hotel));
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    let profiles_dir = dir.path().join("profiles");
    opts.profiles_folder = Some(profiles_dir.clone());
    let calls = Calls::default();
    let report = run(&opts, detect(), None, &calls, None)
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    server.await.unwrap();
    check_protocol(&seen.lock().unwrap());

    let log = calls.log_text();
    assert!(log.contains("Open the catalogue in your client now"), "{log}");
    assert!(log.contains("GetCatalogPage 412 (learned)"), "{log}");
    assert!(report.learned_headers);
    assert_eq!(report.headers.page, PAGE);
    assert_eq!(report.pages, 6);
    assert_eq!(report.profile, "game.unknown-retro.org (learned)");
    let events = calls.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, RipEvent::LearningHeaders { missing } if missing.len() == 4)));
    assert!(events.iter().any(|e| matches!(e, RipEvent::HeadersLearned { saved_to: Some(_), .. })));

    // The saved profile is picked by the host next time.
    let all = profiles::profiles(&profiles_dir);
    let conn = ConnectionInfo {
        host: "game.unknown-retro.org".into(),
        port: 1,
        hotel_version: "WIN63-custom".into(),
        client_identifier: String::new(),
        client_type: "FLASH".into(),
        packets: Vec::new(),
    };
    let (picked, _) = profiles::detect(&all, &conn).unwrap();
    assert_eq!(picked.headers.to_text(), format!("GetCatalogIndex={GET_INDEX},GetCatalogPage={GET_PAGE},CatalogIndex={INDEX},CatalogPage={PAGE}"));
}

#[tokio::test]
async fn a_stopped_rip_goes_on_where_it_was() {
    let dir = tempfile::tempdir().unwrap();
    // First run: cancelled after two pages.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(fake_gearth(listener, Arc::new(Mutex::new(Seen::default())), official()));
    let err = run(&options(port, dir.path()), detect(), None, &Calls::default(), Some(2))
        .await
        .unwrap_err();
    let folder = err.downcast_ref::<PartialOutput>().unwrap().folder.clone();

    // Second run: only what is missing is asked, into the same folder.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let server = tokio::spawn(fake_gearth(listener, seen.clone(), official()));
    let mut opts = options(port, dir.path());
    opts.resume_from = Some(folder.clone());
    let calls = Calls::default();
    let report = run(&opts, detect(), None, &calls, None)
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    server.await.unwrap();
    assert_eq!(report.folder, folder);
    assert_eq!(report.resumed_pages, 2);
    assert!(report.complete);
    assert_eq!(report.pages, 8);
    let asked: Vec<(u16, i32)> = seen.lock().unwrap().requests.iter().map(|r| (r.0, r.1)).collect();
    assert!(!asked.iter().any(|r| r.0 == GET_INDEX), "indexes came from the recording: {asked:?}");
    assert!(!asked.contains(&(GET_PAGE, 1)) && !asked.contains(&(GET_PAGE, 2)), "{asked:?}");
    assert!(asked.contains(&(GET_PAGE, 100)), "{asked:?}");
    assert!(calls.events.lock().unwrap().iter().any(|e| matches!(e, RipEvent::Resumed { pages: 2, indexes: 2, .. })));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("catalog.json")).unwrap()).unwrap();
    assert_eq!(json["complete"], true);

    // A folder of another hotel is refused.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(fake_gearth(listener, Arc::new(Mutex::new(Seen::default())), flash_retro()));
    let mut opts = options(port, dir.path());
    opts.resume_from = Some(folder);
    let err = run(&opts, detect(), None, &Calls::default(), None).await.unwrap_err();
    assert!(err.to_string().contains("was ripped from game-nl.habbo.com"), "{err}");
}

/// items_base where no sprite id matches the source hotel, and the hotel's catalogue ids.
struct OtherHotel;

#[async_trait::async_trait]
impl ItemsSource for OtherHotel {
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>> {
        let row = |id, name: &str, t: &str| ItemsBaseRow {
            id,
            sprite_id: 90_000 + id as i32,
            item_type: t.into(),
            item_name: name.into(),
            effect_id: 0,
        };
        Ok(vec![
            row(500, "rare_dragon", "s"),
            row(501, "table", "s"),
            row(502, "poster", "i"),
            row(503, "ACH_Test1", "b"),
        ])
    }

    async fn existing_catalogue(&self) -> anyhow::Result<Option<ExistingCatalogue>> {
        let mut existing = ExistingCatalogue::default();
        existing.pages.insert("NORMAL".into(), [1, 2, 1000].into_iter().collect());
        existing.offers.insert("NORMAL".into(), [7, 5000].into_iter().collect());
        Ok(Some(existing))
    }
}

#[tokio::test]
async fn reexports_with_furnidata_and_adds_next_to_the_catalogue() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(fake_gearth(listener, Arc::new(Mutex::new(Seen::default())), flash_retro()));
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    let first = run(&opts, detect(), None, &Calls::default(), None).await.unwrap();
    assert_eq!(first.manager.offers, 0, "no items_base the first time");

    // Again without the hotel: the source's furnidata, the other hotel's items_base, Add mode.
    let furnidata = dir.path().join("furnidata.xml");
    std::fs::write(
        &furnidata,
        r#"<furnidata><roomitemtypes><furnitype id="4001" classname="rare_dragon"/><furnitype id="4002" classname="table"/></roomitemtypes>
           <wallitemtypes><furnitype id="5001" classname="poster"/></wallitemtypes></furnidata>"#,
    )
    .unwrap();
    let mut opts = options(1, dir.path());
    opts.use_furnidata = true;
    opts.furnidata_url = Some(furnidata.display().to_string());
    opts.manager_mode = ManagerMode::Add;
    let calls = Calls::default();
    let log = |line: &str| calls.log.lock().unwrap().push(line.to_string());
    let ask = |_: String| -> BoxFuture<'static, bool> { Box::pin(async { false }) };
    let progress = |_: RipProgress| {};
    let hooks = RipHooks {
        items: Some(&OtherHotel),
        ..RipHooks::new(&log, &ask, &progress)
    };
    let again = reexport(&first.folder, &opts, &hooks, &CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    assert_eq!(again.folder, first.folder);
    assert_eq!(again.pages, first.pages);
    assert_eq!(again.furnidata_names, 3);
    assert_eq!(again.headers.page, PAGE);
    // dragon (4001), badge, bundle (4002 + poster 5001) resolve by class name; the rest not.
    assert_eq!(again.manager.offers, 3, "{:?}", again.manager);
    assert_eq!(again.manager.mode, ManagerMode::Add);

    let text = std::fs::read_to_string(again.folder.join("catalog_manager.sql")).unwrap();
    assert!(!text.contains("DELETE"), "{text}");
    let target = aio_remote::catalog_transfer::target(&Default::default(), &text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
    let entity = |page, id| aio_remote::catalog_journal::Entity { builders: false, page, id };
    // Pages above the hotel's highest (1000), offers above its highest (5000).
    let shop = &target[&entity(true, 1003)];
    assert_eq!(shop["captionSave"], "shop_3x3");
    assert_eq!(shop["parentId"], 1002);
    assert_eq!(target[&entity(true, 1001)]["parentId"], -1);
    let dragon = &target[&entity(false, 5300)];
    assert_eq!(dragon["itemIds"], "500");
    assert_eq!(dragon["pageId"], 1003);
    assert_eq!(dragon["offerIdClient"], -1);
    assert_eq!(target[&entity(false, 5302)]["itemIds"], "501:2;502:1");
    // The Arcturus SQL looks the unknown ones up by class name too.
    let items_sql = std::fs::read_to_string(again.folder.join("catalog_items.sql")).unwrap();
    assert!(items_sql.contains("(300, '500', 3,"), "{items_sql}");
    let readme = std::fs::read_to_string(again.folder.join("README.txt")).unwrap();
    assert!(readme.contains("ADDED next to the existing"), "{readme}");
    assert!(readme.contains("FURNIDATA"), "{readme}");
}


#[tokio::test]
async fn second_rip_reports_the_changes_and_branches_can_be_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let items = items_base();
    // First rip: the whole normal catalogue.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(fake_gearth(listener, Arc::new(Mutex::new(Seen::default())), flash_retro()));
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    opts.rip_extras = false;
    let first = run(&opts, detect(), Some(&items), &Calls::default(), None).await.unwrap();
    assert!(first.changes.is_none());
    // The folders are named by the second: keep the second rip apart.
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // Second rip, of the "furni" branch only (2 and below), after the hotel changed the dragon's
    // price: compared with the first.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), flash_retro()));
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    opts.rip_extras = false;
    opts.only_pages = Some([("NORMAL".to_string(), vec![2])].into_iter().collect());
    let calls = Calls::default();
    let second = run(&opts, detect(), Some(&items), &calls, None).await.unwrap();
    let pages: Vec<i32> = seen.lock().unwrap().requests.iter().filter(|r| r.0 == GET_PAGE).map(|r| r.1).collect();
    assert_eq!(pages, vec![2, 3, 4, 5], "only the chosen branch");
    assert!(!second.complete, "a branch is not the whole catalogue");
    let changes = second.changes.as_ref().unwrap();
    assert_eq!(changes.since.as_deref(), Some(first.folder.as_path()));
    // The pages the branch rip did not read (1, 6, 7) are not "changed" or "removed".
    assert!(changes.is_empty(), "{changes:?}");
    let text = std::fs::read_to_string(second.folder.join("catalog_manager_changes.sql")).unwrap();
    assert!(text.contains("Summary: no changes"), "{text}");
    assert!(calls.events.lock().unwrap().iter().any(|e| matches!(e, RipEvent::CatalogChanged { .. })));
    assert!(second.folder.join("catalog_changes.json").is_file());
}

#[tokio::test]
async fn index_only_then_resume_with_a_branch() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), official()));
    let mut opts = options(port, dir.path());
    opts.index_only = true;
    let first = run(&opts, detect(), None, &Calls::default(), None).await.unwrap();
    assert_eq!(first.pages, 0);
    assert!(!first.complete);
    assert_eq!(first.indexes["NORMAL"].root.children.len(), 4);
    assert!(seen.lock().unwrap().requests.iter().all(|r| r.0 == GET_INDEX));

    // The user picked the Builders Club "addons" page only.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), official()));
    let mut opts = options(port, dir.path());
    opts.resume_from = Some(first.folder.clone());
    opts.rip_extras = false;
    opts.only_pages = Some([("BUILDERS_CLUB".to_string(), vec![100])].into_iter().collect());
    let second = run(&opts, detect(), None, &Calls::default(), None).await.unwrap();
    let asked: Vec<(u16, i32, String)> = seen.lock().unwrap().requests.iter().map(|r| (r.0, r.1, r.2.clone())).collect();
    assert_eq!(asked, vec![(GET_PAGE, 100, "BUILDERS_CLUB".to_string())]);
    assert_eq!(second.folder, first.folder);
    assert_eq!(second.pages, 1);
}

#[tokio::test]
async fn rips_from_a_gearth_packet_logger_export() {
    // What the Packet Logger shows while someone opens the catalogue and two pages.
    let text_of = |p: &Packet| -> String {
        p.to_bytes()
            .iter()
            .map(|&b| {
                if !(32..=160).contains(&b) || b == 127 || matches!(b, b'[' | b']' | b'{' | b'}') {
                    format!("[{b}]")
                } else {
                    (b as char).to_string()
                }
            })
            .collect()
    };
    let entry = |out: bool, p: &Packet| {
        format!(
            "{}[{}] -> {}\n--------------------\n",
            if out { "Outgoing" } else { "Incoming" },
            p.header,
            text_of(p)
        )
    };
    let mut log = String::new();
    log += &entry(true, &aio_catalog_rip::packets::get_catalog_index(GET_INDEX, "NORMAL"));
    log += &entry(false, &index_packet(INDEX, "NORMAL", &normal_roots(), true, false));
    for id in [3, 1] {
        log += &entry(true, &aio_catalog_rip::packets::get_catalog_page(GET_PAGE, id, "NORMAL"));
        log += &entry(false, &page_answer(id, "NORMAL").unwrap());
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("packets.txt");
    std::fs::write(&file, log).unwrap();
    let opts = options(1, dir.path());
    let calls = Calls::default();
    let logf = |line: &str| calls.log.lock().unwrap().push(line.to_string());
    let ask = |_: String| -> BoxFuture<'static, bool> { Box::pin(async { false }) };
    let progress = |_: RipProgress| {};
    let hooks = RipHooks::new(&logf, &ask, &progress);
    let result = aio_catalog_rip::gearth_log::import_log(
        &file,
        "game-nl.habbo.com",
        None,
        &opts,
        &hooks,
        &CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    assert_eq!(result.hotel, "habbo.nl");
    assert_eq!(result.pages, 2);
    assert_eq!(result.items, 5);
    assert_eq!(result.headers.page, PAGE);
    assert!(!result.complete);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(result.folder.join("catalog.json")).unwrap()).unwrap();
    assert_eq!(json["catalogs"][0]["pages"][1]["children"][0]["layout"], "default_3x3");
}

#[tokio::test]
async fn rips_repeatedly_until_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // One G-Earth session per rip: serve two, then nothing listens any more.
    tokio::spawn(async move {
        for _ in 0..2 {
            let (s, _) = listener.accept().await.unwrap();
            drop(s); // refused: the rip fails this turn and is tried again
        }
    });
    let mut opts = options(port, dir.path());
    opts.gearth_wait_ms = 300;
    let calls = Calls::default();
    let cancel = CancellationToken::new();
    let log = |line: &str| calls.log.lock().unwrap().push(line.to_string());
    let ask = |_: String| -> BoxFuture<'static, bool> { Box::pin(async { false }) };
    let progress = |_: RipProgress| {};
    let hooks = RipHooks::new(&log, &ask, &progress);
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        stopper.cancel();
    });
    let done = aio_catalog_rip::rip_every(&opts, detect(), &hooks, Duration::from_millis(200), &cancel)
        .await
        .unwrap();
    assert!(done.is_empty());
    let turns = calls.log.lock().unwrap().iter().filter(|l| l.contains("this turn's rip failed")).count();
    assert!(turns >= 2, "{}", calls.log_text());
}


#[tokio::test]
async fn connects_again_when_gearth_drops_the_extension() {
    // G-Earth accepts the extension, then lets it go (switched to Nitro mode, restarted); the
    // next time it is the full session.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen2 = seen.clone();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes()).await.unwrap();
        read_frame(&mut s).await.unwrap();
        let init = Writer::new(ids::INIT).bool(false).string_latin1("G-Earth").string_latin1("1").i32(0);
        s.write_all(&init.to_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(s);
        fake_gearth(listener, seen2, official()).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(port, dir.path());
    opts.builders_club = false;
    opts.rip_extras = false;
    let calls = Calls::default();
    let report = run(&opts, detect(), None, &calls, None)
        .await
        .unwrap_or_else(|e| panic!("{e:#}\n{}", calls.log_text()));
    server.await.unwrap();
    assert_eq!(report.pages, 6);
    assert!(calls.log_text().contains("let the extension go before the hotel connection"), "{}", calls.log_text());
}
