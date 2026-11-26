//! End to end: the Room ripper's capture against a fake G-Earth and a fake Polaris hotel (a Nitro client, no packet
//! info: the built-in retro ids). The hotel sends a room the way Polaris does (the floor furni in two parts, the
//! Polaris furni layout), the capture saves it, asks for the door tile, and reads the wired box with Open when asked.
//! It sends nothing else to the hotel.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aio_catalog_rip::gearth::{self, ids, ConnectionInfo};
use aio_catalog_rip::profiles::builtin_profiles;
use aio_catalog_rip::room::packets::StuffData;
use aio_catalog_rip::room::{self, CaptureEvent, CaptureOptions, Command, RoomCapture};
use aio_catalog_rip::wire::{Packet, Reader, Writer};
use aio_catalog_rip::ProfileChoice;
use futures::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

async fn read_frame(s: &mut TcpStream) -> Option<Packet> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await.ok()?;
    let mut payload = vec![0u8; i32::from_be_bytes(len) as usize];
    s.read_exact(&mut payload).await.ok()?;
    Some(Packet { header: u16::from_be_bytes([payload[0], payload[1]]), body: payload[2..].to_vec() })
}

/// A floor furni as Polaris writes it (with its seven extra ints).
fn floor(w: Writer, id: i32, type_id: i32, x: i32, state: &str) -> Writer {
    let w = w.i32(id).i32(type_id).i32(x).i32(1).i32(2).string("0.0").string("1.0").i32(0).i32(0).string(state).i32(-1).i32(1).i32(77);
    (0..7).fold(w, |w, _| w.i32(0))
}

fn room_packets() -> Vec<Packet> {
    let objects = |items: &[(i32, i32, i32, &str)]| {
        let w = Writer::new(1778).i32(1).i32(77).string("builder").i32(items.len() as i32);
        items.iter().fold(w, |w, (id, t, x, s)| floor(w, *id, *t, *x, s)).packet()
    };
    let wall = {
        let w = Writer::new(1369).i32(1).i32(77).string("builder").i32(1).string("90").i32(400).string(":w=1,1 l=5,5 r").string("4").i32(-1).i32(0).i32(77);
        (0..7).fold(w, |w, _| w.i32(0)).packet()
    };
    vec![
        Writer::new(2031).string("model_x").i32(42).packet(),
        Writer::new(1301).bool(true).i32(-1).string("xxxx\rx000\r0000\rx000").packet(),
        Writer::new(2454).string("wallpaper").string("101").packet(),
        Writer::new(2454).string("landscape").string("1.1").packet(),
        // Polaris sends the floor furni in parts: the capture adds them up.
        objects(&[(10, 200, 1, "0"), (11, 201, 2, "1")]),
        objects(&[(12, 202, 3, "0")]),
        wall,
        Writer::new(687).bool(true).i32(42).string("Villa Rosa").i32(77).string("builder").i32(1).i32(3).i32(25).string("A nice room").i32(0).i32(0).i32(0).i32(1).i32(1).string("party").i32(0).packet(),
        Writer::new(5102).i32(42).i32(1).i32(0).bool(true).bool(false).bool(false).packet(),
    ]
}

/// The settings of box 12 (a toggle box pointing at furni 10), in the current layout.
fn wired_answer() -> Packet {
    Writer::new(1434)
        .i32(5)
        .i32(1).i32(10)
        .i32(0)
        .i32(202)
        .i32(12)
        .string("")
        .i32(1).i32(0)
        .i32(0)
        .i32(1).i32(100)
        .i32(0)
        .i32(0)
        .i32(3)
        .bool(false)
        .packet()
}

#[derive(Default)]
struct Seen {
    sent: Vec<(u16, Vec<u8>)>,
    echoed: usize,
}

async fn fake_gearth(listener: TcpListener, seen: Arc<Mutex<Seen>>, client: (&'static str, &'static str)) {
    let (mut s, _) = listener.accept().await.unwrap();
    s.write_all(&Writer::new(ids::INFO_REQUEST).to_bytes()).await.unwrap();
    let info = read_frame(&mut s).await.unwrap();
    assert_eq!(info.header, ids::EXTENSION_INFO);
    let init = Writer::new(ids::INIT).bool(true).string_latin1("G-Earth").string_latin1("1.5.4").i32(0);
    s.write_all(&init.to_bytes()).await.unwrap();
    let connection = ConnectionInfo {
        host: "game.myretro.net".into(),
        port: 30000,
        hotel_version: client.1.into(),
        client_identifier: String::new(),
        client_type: client.0.into(),
        packets: Vec::new(),
    };
    s.write_all(&connection.message().to_bytes()).await.unwrap();
    let mut index = 0;
    let mut intercept = |packet: &Packet| {
        index += 1;
        gearth::intercept_message(false, index, packet).to_bytes()
    };
    // The user enters the room, a moment after logging in (the capture is listening by then).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut out = Vec::new();
    for packet in room_packets() {
        out.extend(intercept(&packet));
    }
    s.write_all(&out).await.unwrap();
    while let Some(frame) = read_frame(&mut s).await {
        match frame.header {
            ids::MANIPULATED_PACKET => seen.lock().unwrap().echoed += 1,
            ids::SEND_MESSAGE => {
                let mut r = Reader::new(&frame.body);
                assert_eq!(r.byte("side").unwrap(), 1);
                let len = r.i32("len").unwrap() as usize;
                let packet = Packet::from_bytes(r.bytes(len, "packet").unwrap()).unwrap();
                seen.lock().unwrap().sent.push((packet.header, packet.body.clone()));
                let answer = match packet.header {
                    768 => Some(wired_answer()),
                    other => panic!("the capture sent packet {other} to the hotel"),
                };
                if let Some(answer) = answer {
                    if s.write_all(&intercept(&answer)).await.is_err() {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

async fn wait_for(events: &Arc<Mutex<Vec<CaptureEvent>>>, what: &str, test: impl Fn(&CaptureEvent) -> bool) {
    for _ in 0..200 {
        if events.lock().unwrap().iter().any(&test) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no {what} event: {:?}", events.lock().unwrap().iter().map(|e| format!("{e:?}")).collect::<Vec<_>>());
}

#[tokio::test]
async fn captures_a_polaris_room_and_reads_its_wired() {
    let dir = tempfile::tempdir().unwrap();
    let furnidata = dir.path().join("furnidata.json");
    std::fs::write(
        &furnidata,
        r#"{"roomitemtypes":{"furnitype":[{"id":200,"classname":"chair"},{"id":201,"classname":"lamp"},{"id":202,"classname":"wf_act_toggle_state"}]},
            "wallitemtypes":{"furnitype":[{"id":400,"classname":"poster"}]}}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), ("NITRO", "NITRO-1")));

    let options = CaptureOptions {
        folder: dir.path().join("rooms"),
        furnidata_url: Some(furnidata.display().to_string()),
        wired_delay_ms: 50,
        wired_timeout_ms: 2000,
        ports: vec![port],
        gearth_folder: None,
        packets_folder: dir.path().join("packets"),
    };
    let events: Arc<Mutex<Vec<CaptureEvent>>> = Arc::default();
    let log_lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    // The developer recording, asked for before the room is entered: it starts with the room.
    commands.send(Command::RecordPackets(true)).unwrap();
    let cancel = CancellationToken::new();
    let task = {
        let (events, log_lines, cancel) = (events.clone(), log_lines.clone(), cancel.clone());
        tokio::spawn(async move {
            let log = move |line: &str| log_lines.lock().unwrap().push(line.to_string());
            let ask = |_q: String| -> BoxFuture<'static, bool> { Box::pin(async { false }) };
            let sink = move |e: CaptureEvent| events.lock().unwrap().push(e);
            room::capture(&options, ProfileChoice::Detect(builtin_profiles()), &log, &ask, &sink, receiver, &cancel).await
        })
    };

    wait_for(&events, "room", |e| matches!(e, CaptureEvent::Room { summary, .. } if summary.floor_items == 3 && summary.wall_items == 1)).await;
    commands.send(Command::ReadWired).unwrap();
    wait_for(&events, "wired done", |e| matches!(e, CaptureEvent::WiredDone { .. })).await;
    cancel.cancel();
    task.await.unwrap().unwrap();

    assert!(
        events.lock().unwrap().iter().any(|e| matches!(e, CaptureEvent::WiredDone { read: 1, total: 1, no_rights: false })),
        "{:?}",
        log_lines.lock().unwrap()
    );
    let captures = room::list_captures(&dir.path().join("rooms"));
    assert_eq!(captures.len(), 1);
    let capture: &RoomCapture = &captures[0].1;
    assert_eq!(capture.name(), "Villa Rosa");
    assert_eq!((capture.wallpaper.as_str(), capture.landscape.as_str(), capture.floor.as_str()), ("101", "1.1", "0.0"));
    // The hotel was not asked for the door: it is guessed from the floor plan, for the user to check.
    assert_eq!(capture.door, None);
    assert!(!capture.door_from_hotel);
    assert_eq!(capture.door_or_guess(), Some(room::Door { x: 0, y: 2, dir: 2 }));
    assert_eq!(capture.floor_items.len(), 3);
    assert_eq!(capture.floor_items[&11].stuff, StuffData::Legacy { data: "1".into() });
    assert_eq!(capture.floor_items[&12].class_name.as_deref(), Some("wf_act_toggle_state"));
    assert_eq!(capture.wall_items[&90].class_name.as_deref(), Some("poster"));
    assert_eq!(capture.wired[&12].stuff_ids, vec![10]);
    assert_eq!(capture.wired[&12].delay, 3);
    assert_eq!(capture.can_read_wired, Some(true));
    assert!(capture.problems.is_empty(), "{:?}", capture.problems);
    let seen = seen.lock().unwrap();
    // Only Open for box 12 went to the hotel: the capture sends nothing by itself.
    let sent: Vec<u16> = seen.sent.iter().map(|(h, _)| *h).collect();
    assert_eq!(sent, vec![768]);
    assert_eq!(seen.sent[0].1, 12i32.to_be_bytes().to_vec());
    // Every room packet went on to the client.
    assert!(seen.echoed >= room_packets().len(), "echoed {}", seen.echoed);

    // The recording holds every packet of the room, both ways, and the wired ones readably.
    let finished = events.lock().unwrap().iter().find_map(|e| match e {
        CaptureEvent::Recording(room::Recording::Finished(folder)) => Some(folder.clone()),
        _ => None,
    });
    let folder = finished.expect("the recording was finished");
    let all = std::fs::read_to_string(folder.join("packets.jsonl")).unwrap();
    assert!(all.lines().count() >= room_packets().len() + 2, "{all}");
    assert!(all.contains(r#""dir":"out","header":768"#) && all.contains(r#""dir":"in","header":2031,"name":"RoomReady""#), "{all}");
    let wired = std::fs::read_to_string(folder.join("wired.txt")).unwrap();
    assert!(wired.contains("client -> hotel  768 Open") && wired.contains("Action box 12"), "{wired}");
    assert!(std::fs::read_to_string(folder.join("README.txt")).unwrap().contains("game.myretro.net"));
}

/// A Flash retro of another client revision than Arcturus' is matched by its client type only: its room ids are not
/// the Arcturus ones, so the capture must not use them (a packet sent with a wrong id can get the user disconnected).
/// It stops with a clear message and sends nothing.
#[tokio::test]
async fn a_flash_retro_of_another_revision_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(fake_gearth(listener, seen.clone(), ("FLASH", "PRODUCTION-201701242205-837386173")));
    let options = CaptureOptions { folder: dir.path().join("rooms"), ports: vec![port], gearth_folder: None, ..CaptureOptions::default() };
    let (_commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let log = |_: &str| {};
    let ask = |_q: String| -> BoxFuture<'static, bool> { Box::pin(async { false }) };
    let sink = |_: CaptureEvent| {};
    let error = room::capture(&options, ProfileChoice::Detect(builtin_profiles()), &log, &ask, &sink, receiver, &CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not known for this hotel"), "{error}");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(seen.lock().unwrap().sent.is_empty(), "nothing may be sent");
    assert!(room::list_captures(&dir.path().join("rooms")).is_empty());
}
