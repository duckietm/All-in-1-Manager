//! The recorded packets (`packets/`), going on with a stopped rip, writing a recorded rip again,
//! a rip from a G-Earth Packet Logger export, and the comparison with an earlier rip: against
//! the built-in's formats and functions (its `Recorder` in `support::builtin`, its `diff` as
//! `diff::diff`, its `gearth_log` import flow put together here).

mod support;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use catalog_rip_core::diff::{self, CatalogDiff};
use catalog_rip_core::export::{self, ItemsBase, RippedCatalog, RippedMode};
use catalog_rip_core::job::{Failure, ImportLog, Job, Phase, RipOptions, Step, Summary};
use catalog_rip_core::manager::{ManagerExport, ManagerMode};
use catalog_rip_core::output::{self, items_base, ExportInput};
use catalog_rip_core::profiles::{HeaderMap, HotelKind, HotelProfile};
use catalog_rip_core::recording::{Loader, Recorded};
use catalog_rip_core::source::{MemSource, Source, MAX_READ};
use catalog_rip_core::time::Timestamp;
use catalog_rip_core::transport::{Closed, Transport};
use catalog_rip_core::wire::Packet;
use catalog_rip_core::{packets, ItemsBaseRow, RipEvent};
use support::fake::*;
use support::mem::*;
use support::sim::*;
use support::*;

const GENERATOR: &str = "Catalogue ripper plugin 9.9.9";
const FOLDER: &str = "habbo.nl_2025-11-15_12-00-00";

fn options(port: u16) -> RipOptions {
    RipOptions {
        gearth_port: port,
        delay_ms: 20,
        timeout_ms: 400,
        hotel_wait_ms: 10_000,
        resend_every_ms: 500,
        download_images: false,
        use_furnidata: false,
        generator: GENERATOR.into(),
        ..RipOptions::default()
    }
}

fn done(run: &Run) -> &Summary {
    match &run.end {
        Step::Done(summary) => summary,
        other => panic!("{other:?}\n{}", run.log_text()),
    }
}

fn failed(run: &Run) -> &Failure {
    match &run.end {
        Step::Failed(failure) => failure,
        other => panic!("{other:?}\n{}", run.log_text()),
    }
}

fn show_diff(got: &BTreeMap<String, Vec<u8>>, want: &BTreeMap<String, Vec<u8>>) -> String {
    let mut text = String::new();
    for name in got.keys().chain(want.keys()) {
        match (got.get(name), want.get(name)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => text.push_str(&format!(
                "--- {name} differs:\n{}\n+++ expected:\n{}\n",
                String::from_utf8_lossy(a),
                String::from_utf8_lossy(b)
            )),
            (Some(_), None) => text.push_str(&format!("unexpected {name}\n")),
            (None, _) => text.push_str(&format!("missing {name}\n")),
        }
    }
    text
}

/// Runs a job against the TCP fake G-Earth (`port`; 0: no G-Earth), 50 ms per step; `cancel_after`:
/// cancels once that many pages were done.
fn run_tcp(
    mut job: Job,
    port: u16,
    sink: &mut MemSink,
    source: Option<&mut dyn Source>,
    cancel_after: Option<usize>,
) -> Run {
    let clock = RealClock::new();
    let transport = if port == 0 {
        Tcp(None)
    } else {
        Tcp(Some(StdTransport::connect(port)))
    };
    let mut pages = 0;
    run_job_from(
        &mut job,
        transport,
        |t: Tcp| {
            if let Some(t) = t.0 {
                t.finish();
            }
        },
        &mut MemFetch::none(),
        sink,
        &clock,
        (None, None),
        source,
        |_| 50,
        |job, _| {
            let progress = job.progress();
            if progress.phase == Phase::Pages {
                pages = progress.done as usize;
            }
            if cancel_after.is_some_and(|n| pages >= n) {
                job.cancel();
            }
        },
    )
}

/// G-Earth over TCP, or none (a job that never connects).
struct Tcp(Option<StdTransport>);

impl Transport for Tcp {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        match &mut self.0 {
            Some(t) => t.send(data),
            None => Closed.send(data),
        }
    }

    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        match &mut self.0 {
            Some(t) => t.recv(max, timeout_ms),
            None => Closed.recv(max, timeout_ms),
        }
    }
}

/// A resumed rip's files are those of `whole`, an uninterrupted rip. Only session.json differs,
/// as in the built-in: a resumed rip goes on with the recorded profile, and records it with the
/// header ids it used.
fn assert_resumed_equals(mut got: BTreeMap<String, Vec<u8>>, mut whole: BTreeMap<String, Vec<u8>>) {
    let session = |files: &mut BTreeMap<String, Vec<u8>>| -> serde_json::Value {
        let mut v: serde_json::Value =
            serde_json::from_slice(&files.remove("packets/session.json").unwrap()).unwrap();
        v["profile"]["headers"] = serde_json::Value::Null;
        v
    };
    assert_eq!(session(&mut got), session(&mut whole));
    assert!(got == whole, "{}", show_diff(&got, &whole));
}

/// An uninterrupted rip of the official test hotel over TCP: its files.
fn whole_rip() -> BTreeMap<String, Vec<u8>> {
    let (port, _seen, fake) = start(official());
    let mut sink = MemSink::default();
    let run = run_tcp(Job::new(options(port)), port, &mut sink, None, None);
    fake.join().unwrap();
    assert!(done(&run).complete);
    sink.under(FOLDER)
}

// ------------------------------------------------------------------ recording

/// `packets/` holds what the built-in's `Recorder` writes for the same answers (the whole rip of
/// `rip_job::full_rip_matches_reconstructed_builtin_output` checks it byte for byte); a rip that
/// does not record writes no `packets/`, but keeps a page it cannot read; a page that comes again
/// replaces its file.
#[test]
fn recording_matches_builtin_layout() {
    let got = whole_rip();
    let mut want = support::builtin::Recorder::default();
    for mode in ["NORMAL", "BUILDERS_CLUB"] {
        let index = if mode == "NORMAL" {
            index_packet(INDEX, "NORMAL", &normal_roots(), true, false)
        } else {
            index_packet(INDEX, mode, &builders_roots(), false, false)
        };
        want.index(mode, &index.body);
    }
    for (mode, id) in [
        ("NORMAL", 1),
        ("NORMAL", 2),
        ("NORMAL", 3),
        ("NORMAL", 4),
        ("NORMAL", 5),
        ("NORMAL", 6),
        ("BUILDERS_CLUB", 100),
        ("BUILDERS_CLUB", 4),
    ] {
        want.page(mode, id, &page_answer(id, mode).unwrap().body);
    }
    want.extra("club_offers", &extra_answer(3285).unwrap().body);
    let packets: BTreeMap<String, Vec<u8>> = got
        .iter()
        .filter(|(k, _)| k.starts_with("packets/") && !k.ends_with("session.json"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    want.files.remove("packets/session.json");
    assert!(packets == want.files, "{}", show_diff(&packets, &want.files));
    let session: support::builtin::SessionInfo =
        serde_json::from_slice(&got["packets/session.json"]).unwrap();
    assert_eq!(session.modes, ["NORMAL", "BUILDERS_CLUB"]);
    assert_eq!(session.failed, BTreeMap::from([("NORMAL".to_string(), vec![7])]));
    assert!(session.complete);
    assert_eq!(session.port, 30000);
    // The built-in writes it with serde_json's pretty printer from the same struct.
    assert_eq!(
        got["packets/session.json"],
        serde_json::to_string_pretty(&session).unwrap().into_bytes()
    );

    // Not recording: no packets/, no PACKETS section; an unreadable page is still kept.
    let sim_hotel = SimHotel {
        page: Box::new(|id, mode| {
            if id == 3 {
                // The page id and mode, then nothing a page can be read from.
                Some(Packet {
                    header: PAGE,
                    body: catalog_rip_core::wire::Writer::new(PAGE)
                        .i32(3)
                        .string(mode)
                        .i32(-99)
                        .packet()
                        .body,
                })
            } else {
                page_answer(id, mode)
            }
        }),
        ..official_hotel()
    };
    let sim = SimGEarth::new(SimClock::default(), sim_hotel);
    let clock = sim.clock.clone();
    let mut job = Job::new(RipOptions {
        record_packets: false,
        builders_club: false,
        rip_extras: false,
        delay_ms: 50,
        timeout_ms: 400,
        download_images: false,
        use_furnidata: false,
        ..RipOptions::default()
    });
    let mut sink = MemSink::default();
    let run = run_job(
        &mut job,
        sim,
        drop,
        &mut MemFetch::none(),
        &mut sink,
        &clock,
        (None, None),
        |_| 50,
        |_, _| {},
    );
    let report = done(&run);
    let files = sink.under(&report.folder);
    let packets: Vec<&String> = files.keys().filter(|k| k.starts_with("packets/")).collect();
    assert_eq!(packets, ["packets/page_NORMAL_3.bin"]);
    assert!(run.log_text().contains(&format!(
        "  raw page saved: {}/packets/page_NORMAL_3.bin",
        report.folder
    )));
    // ... which is the PACKETS section's condition in the built-in (packets/ exists).
    let readme = String::from_utf8(files["README.txt"].clone()).unwrap();
    assert!(readme.contains("\r\nPACKETS\r\n"));

    let sim = SimGEarth::new(SimClock::default(), official_hotel());
    let clock = sim.clock.clone();
    let mut job = Job::new(RipOptions {
        record_packets: false,
        rip_extras: false,
        delay_ms: 50,
        timeout_ms: 400,
        download_images: false,
        use_furnidata: false,
        ..RipOptions::default()
    });
    let mut sink = MemSink::default();
    let run = run_job(
        &mut job,
        sim,
        drop,
        &mut MemFetch::none(),
        &mut sink,
        &clock,
        (None, None),
        |_| 50,
        |_, _| {},
    );
    let files = sink.under(&done(&run).folder);
    assert!(!files.keys().any(|k| k.starts_with("packets/")));
    assert!(!String::from_utf8_lossy(&files["README.txt"]).contains("PACKETS"));
}

// ------------------------------------------------------------------ resume

/// A rip stopped after two pages, then resumed from its recording: only what was missing is
/// asked, and the files are those of a rip that was never stopped (folder for folder: the clock
/// says the same second, so the new folder has the same name).
#[test]
fn resume_equals_uninterrupted_rip() {
    let whole = whole_rip();

    // First run: cancelled after two pages.
    let (port, _seen, fake) = start(official());
    let mut first = MemSink::default();
    let run = run_tcp(Job::new(options(port)), port, &mut first, None, Some(2));
    fake.join().unwrap();
    let failure = failed(&run);
    assert!(failure.cancelled);
    let partial = failure.partial.as_ref().unwrap();
    assert_eq!(partial.folder, FOLDER);
    let session: support::builtin::SessionInfo =
        serde_json::from_slice(&first.files[&format!("{FOLDER}/packets/session.json")]).unwrap();
    assert!(!session.complete);

    // Second run: only what is missing is asked.
    let (port, seen, fake) = start(official());
    let mut source = MemSource::with_files(first.files.iter());
    let mut second = MemSink::default();
    let run = run_tcp(
        Job::resume(options(port), FOLDER),
        port,
        &mut second,
        Some(&mut source),
        None,
    );
    fake.join().unwrap();
    let report = done(&run);
    assert_eq!(report.folder, FOLDER);
    assert!(report.complete);
    assert_eq!(report.pages, 8);
    let resumed = report.resumed_pages;
    assert!(resumed >= 2, "{resumed}");
    let asked: Vec<(u16, i32)> = seen.lock().unwrap().requests.iter().map(|r| (r.0, r.1)).collect();
    assert!(!asked.iter().any(|r| r.0 == GET_INDEX), "indexes came from the recording: {asked:?}");
    assert!(!asked.contains(&(GET_PAGE, 1)) && !asked.contains(&(GET_PAGE, 2)), "{asked:?}");
    assert!(asked.contains(&(GET_PAGE, 100)), "{asked:?}");
    assert!(run
        .log_text()
        .contains(&format!("Going on with {FOLDER}: 2 catalogue indexes and")));
    assert!(run.log_text().contains("From the stopped rip:"));

    assert_resumed_equals(second.under(FOLDER), whole);
    assert!(source.largest_read <= MAX_READ);

    // A recording of another hotel is refused.
    let (port, _seen, fake) = start(flash_retro());
    let mut source = MemSource::with_files(first.files.iter());
    let run = run_tcp(
        Job::resume(options(port), FOLDER),
        port,
        &mut MemSink::default(),
        Some(&mut source),
        None,
    );
    fake.join().unwrap();
    let message = &failed(&run).message;
    assert!(message.contains("was ripped from game-nl.habbo.com"), "{message}");

    // A folder without a recording.
    let run = run_tcp(
        Job::resume(options(1), "nothing_here"),
        0,
        &mut MemSink::default(),
        Some(&mut MemSource::new()),
        None,
    );
    assert!(failed(&run).message.contains("has no recorded packets"));
}

/// A recording the built-in ripper made (written here by a copy of its `Recorder`,
/// `support::builtin`, into another folder: three pages, one of which it could not read, no
/// extras) goes on like the plugin's own.
#[test]
fn resume_from_builtin_recording() {
    let whole = whole_rip();
    let earlier = "habbo.nl_2025-01-01_10-00-00";
    let mut builtin = support::builtin::Recorder::default();
    builtin.index("NORMAL", &index_packet(INDEX, "NORMAL", &normal_roots(), true, false).body);
    builtin.index(
        "BUILDERS_CLUB",
        &index_packet(INDEX, "BUILDERS_CLUB", &builders_roots(), false, false).body,
    );
    for id in [1, 3] {
        builtin.page("NORMAL", id, &page_answer(id, "NORMAL").unwrap().body);
    }
    // A page the built-in kept although it could not read it: asked again.
    builtin.page("NORMAL", 4, &[0, 0, 0, 4, 0, 6, b'N', b'O']);
    let mut session = support::builtin::SessionInfo {
        modes: vec!["NORMAL".into(), "BUILDERS_CLUB".into()],
        ..serde_json::from_slice(&whole["packets/session.json"]).unwrap()
    };
    session.complete = false;
    session.failed = BTreeMap::new();
    session.ripped_at = "2025-01-01 10:00:00".into();
    builtin.session(&session);
    let mut source = MemSource::new();
    for (name, data) in builtin.files {
        source.files.insert(format!("{earlier}/{name}"), data);
    }

    let (port, seen, fake) = start(official());
    let mut sink = MemSink::default();
    let run = run_tcp(
        Job::resume(options(port), earlier),
        port,
        &mut sink,
        Some(&mut source),
        None,
    );
    fake.join().unwrap();
    let report = done(&run);
    assert_eq!(report.resumed_pages, 2);
    let pages: Vec<i32> = seen
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.0 == GET_PAGE && r.2 == "NORMAL")
        .map(|r| r.1)
        .collect();
    assert_eq!(pages, [2, 4, 5, 6, 6, 7, 7]);
    assert!(run.events.contains(&RipEvent::Resumed {
        folder: earlier.into(),
        indexes: 2,
        pages: 2
    }));
    // The same files as a rip that was never stopped: the unreadable page was replaced.
    assert_resumed_equals(sink.under(FOLDER), whole);
}

/// A recorded rip written again without the hotel: the same files, packets/ included.
#[test]
fn reexport_writes_the_same_rip_again() {
    let whole = whole_rip();
    let mut source = MemSource::new();
    for (name, data) in &whole {
        source.files.insert(format!("{FOLDER}/{name}"), data.clone());
    }
    let mut sink = MemSink::default();
    let run = run_tcp(
        Job::reexport(options(1), FOLDER),
        0,
        &mut sink,
        Some(&mut source),
        None,
    );
    let report = done(&run);
    assert!(report.complete);
    assert_eq!(report.headers.page, PAGE);
    assert!(run.log_text().contains(&format!(
        "Writing {FOLDER} again from its recorded packets (2 catalogues, 8 pages)."
    )));
    let got = sink.under(FOLDER);
    // Compared with itself: the only earlier rip is the one written again (skipped).
    assert!(got == whole, "{}", show_diff(&got, &whole));
}

// ------------------------------------------------------------------ Packet Logger import

/// G-Earth's text form of a packet (PacketStringUtils.toString).
fn packet_text(p: &Packet) -> String {
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
}

/// What the Packet Logger shows while someone opens the catalogue and two pages (the built-in's
/// `rips_from_a_gearth_packet_logger_export`), with other game packets in between.
fn logger_export() -> String {
    let entry = |out: bool, p: &Packet| {
        format!(
            "(timestamp: 1700000000000)\n{}[{}] -> {}\n--------------------\n",
            if out { "Outgoing" } else { "Incoming" },
            p.header,
            packet_text(p)
        )
    };
    let noise = catalog_rip_core::wire::Writer::new(2000).i32(7).string("hello [x]").packet();
    let mut log = String::new();
    log += &entry(false, &noise);
    log += &entry(true, &packets::get_catalog_index(GET_INDEX, "NORMAL"));
    log += &entry(false, &index_packet(INDEX, "NORMAL", &normal_roots(), true, false));
    for id in [3, 1] {
        log += &entry(true, &packets::get_catalog_page(GET_PAGE, id, "NORMAL"));
        log += &entry(false, &noise);
        log += &entry(false, &page_answer(id, "NORMAL").unwrap());
    }
    log
}

/// The import writes what the built-in's `import_log` writes: the recording of the catalogue
/// found in the log (its `Recorder`), then the rip written again from it (`reexport`), with the
/// header ids learned from the packets' shapes.
#[test]
fn packet_logger_import_matches_builtin() {
    let log = logger_export();
    let mut source = MemSource::new();
    source.files.insert("logs/packets.txt".into(), log.clone().into_bytes());
    let mut sink = MemSink::default();
    let run = run_tcp(
        Job::import_log(
            options(1),
            ImportLog {
                path: "logs/packets.txt".into(),
                host: "game-nl.habbo.com".into(),
                profile: None,
            },
        ),
        0,
        &mut sink,
        Some(&mut source),
        None,
    );
    let report = done(&run);
    assert_eq!(report.hotel, "habbo.nl");
    assert_eq!(report.pages, 2);
    assert_eq!(report.items, 5);
    assert_eq!(report.headers.page, PAGE);
    assert!(!report.complete);
    assert!(run
        .log_text()
        .contains("logs/packets.txt: 9 packets, 1 catalogue indexes and 2 pages."));

    // The built-in's flow, by hand.
    let headers = packets::Headers {
        get_index: GET_INDEX,
        get_page: GET_PAGE,
        index: INDEX,
        page: PAGE,
    };
    let ripped_at = Timestamp::new(UNIX_SECS).date_time();
    let profile = HotelProfile {
        name: "game-nl.habbo.com (G-Earth log)".into(),
        kind: HotelKind::FlashRetro,
        hosts: Vec::new(),
        revisions: Vec::new(),
        headers: HeaderMap::new(GET_INDEX, GET_PAGE, INDEX, PAGE),
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
    };
    let mut recorder = support::builtin::Recorder::default();
    let index_body = index_packet(INDEX, "NORMAL", &normal_roots(), true, false).body;
    recorder.index("NORMAL", &index_body);
    let mut pages = HashMap::new();
    for id in [3, 1] {
        let body = page_answer(id, "NORMAL").unwrap().body;
        recorder.page("NORMAL", id, &body);
        pages.insert(id, packets::parse_page(&body).unwrap());
    }
    recorder.session(&support::builtin::SessionInfo {
        format: 1,
        host: "game-nl.habbo.com".into(),
        port: 0,
        hotel_version: String::new(),
        client_type: "G-Earth log".into(),
        profile: profile.clone(),
        headers: Some(headers),
        modes: vec!["NORMAL".into()],
        failed: BTreeMap::new(),
        complete: false,
        ripped_at: ripped_at.clone(),
    });
    let mut index = packets::parse_index(&index_body).unwrap();
    index.mode = "NORMAL".into();
    for page in pages.values_mut() {
        page.mode = "NORMAL".into();
    }
    let catalog = RippedCatalog {
        hotel: "habbo.nl".into(),
        host: "game-nl.habbo.com".into(),
        hotel_version: String::new(),
        client_type: "G-Earth log".into(),
        ripped_at,
        generator: GENERATOR.into(),
        complete: false,
        modes: vec![RippedMode {
            mode: "NORMAL".into(),
            index,
            pages,
            failed: Vec::new(),
        }],
    };
    let answers = BTreeMap::new();
    let data = output::data_files(&ExportInput {
        catalog: &catalog,
        items: None,
        furnidata: None,
        products: None,
        manager_mode: ManagerMode::Replace,
        existing: None,
        extra_answers: &answers,
        changes: None,
    });
    let mut want: BTreeMap<String, Vec<u8>> = data.files.into_iter().collect();
    let mut readme = export::readme(
        &catalog,
        &data.sql,
        "not downloaded (turned off)",
        "No items_base was given: item_ids are subqueries on items_base, resolved when the SQL is imported.",
    );
    readme.push_str(&catalog_rip_core::manager::readme_section(&data.manager, false));
    readme.push_str("\nHOTEL PROFILE\n  game-nl.habbo.com (G-Earth log) (Flash retro)\n");
    readme.push_str(support::builtin::PACKETS_SECTION);
    want.insert("README.txt".into(), readme.replace('\n', "\r\n").into_bytes());
    want.extend(recorder.files);
    let got = sink.under(&report.folder);
    assert!(got == want, "{}", show_diff(&got, &want));
    let json: serde_json::Value = serde_json::from_slice(&got["catalog.json"]).unwrap();
    assert_eq!(json["catalogs"][0]["pages"][1]["children"][0]["layout"], "default_3x3");

    // With the hotel's profile: its ids (and name) are used, nothing is learned.
    let mut sink = MemSink::default();
    let mut official = catalog_rip_core::profiles::builtin_profiles().remove(0);
    official.headers = HeaderMap::new(GET_INDEX, GET_PAGE, INDEX, PAGE);
    let run = run_tcp(
        Job::import_log(
            options(1),
            ImportLog {
                path: "logs/packets.txt".into(),
                host: String::new(),
                profile: Some(official.clone()),
            },
        ),
        0,
        &mut sink,
        Some(&mut source),
        None,
    );
    let report = done(&run);
    assert_eq!(report.folder, "gearth-log_2025-11-15_12-00-00");
    assert_eq!(report.profile, official.name);

    // Not a log.
    source.files.insert("logs/dump.txt".into(), b"0000  00 01 |..|\n".to_vec());
    let run = run_tcp(
        Job::import_log(
            options(1),
            ImportLog {
                path: "logs/dump.txt".into(),
                ..ImportLog::default()
            },
        ),
        0,
        &mut MemSink::default(),
        Some(&mut source),
        None,
    );
    assert!(failed(&run).message.contains("hexdump"));
}

// ------------------------------------------------------------------ diff

fn items_rows() -> Vec<ItemsBaseRow> {
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
        row(17, 4005, "s", "lamp"),
    ]
}

fn furni(class_id: i32) -> Prod {
    Prod::Furni {
        kind: "s",
        class_id,
        extra: "",
        amount: 1,
        limited: None,
    }
}

/// The test hotel before (`after`: false) and after its catalogue changed: on page 3 the dragon
/// costs more, the wallpaper is gone and a lamp is new; page 2 shows the chairs of page 3 again
/// and an offer without an id (new ids); page 7 left the index and page 8 came; page 6 does not
/// answer the second time (not read: neither changed nor removed); the Builders Club page 100 got
/// another text.
fn hotel(after: bool) -> SimHotel {
    let roots = move || {
        let mut roots = normal_roots();
        if after {
            roots.pop();
            roots.push(node(8, -1, "new_page", "New page", 9, vec![]));
        }
        roots
    };
    let builders = || builders_roots();
    SimHotel {
        index: Box::new(move |mode| {
            Some(if mode == "NORMAL" {
                index_packet(INDEX, "NORMAL", &roots(), true, false)
            } else {
                index_packet(INDEX, mode, &builders(), false, false)
            })
        }),
        page: Box::new(move |id, mode| {
            let extended = mode == "NORMAL";
            match (id, mode) {
                (2, "NORMAL") => Some(page_packet(
                    PAGE,
                    2,
                    mode,
                    "default_3x3",
                    &["catalog_header_furni"],
                    &["Furni text"],
                    &[item(304, "chair_x5", 5, vec![furni(4003)]), item(0, "zero", 1, vec![furni(4004)])],
                    None,
                    extended,
                )),
                (3, "NORMAL") if after => {
                    let mut items = shop_items();
                    items[0].credits = 60;
                    items.retain(|i| i.id != 303);
                    items.push(item(305, "lamp", 4, vec![furni(4005)]));
                    Some(page_packet(
                        PAGE,
                        3,
                        mode,
                        "default_3x3",
                        &["catalog_header_roombuilder", "cr_infopic3", "catalog_special_txtbg1"],
                        &["It's a \"shop\"\nline 2", "Details \\ backslash", "Teaser"],
                        &items,
                        None,
                        extended,
                    ))
                }
                (6, _) if after => None,
                (8, "NORMAL") => Some(page_packet(
                    PAGE,
                    8,
                    mode,
                    "default_3x3",
                    &["h8"],
                    &["t8"],
                    &[item(800, "new_chair", 2, vec![furni(4003)])],
                    None,
                    extended,
                )),
                (100, _) if after => Some(page_packet(
                    PAGE,
                    100,
                    mode,
                    "builders_club_addons",
                    &["bc_header", "bc_teaser", ""],
                    &["Another BC text", "", ""],
                    &[item(1000, "bc_table", 0, vec![furni(4002)])],
                    None,
                    extended,
                )),
                _ => page_answer(id, mode),
            }
        }),
        ..official_hotel()
    }
}

/// The simulated G-Earth, shared with the test while the job runs.
#[derive(Clone)]
struct Link(Rc<RefCell<SimGEarth>>);

impl Transport for Link {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.borrow_mut().send(data)
    }

    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        self.0.borrow_mut().recv(max, timeout_ms)
    }
}

/// A rip of `hotel` on the simulated G-Earth, starting `at_ms` into the day (so two rips get
/// two folders).
fn sim_rip(
    hotel: SimHotel,
    at_ms: u64,
    options: RipOptions,
    items: Option<&ItemsBase>,
    source: Option<&mut dyn Source>,
) -> (Run, MemSink) {
    let clock = SimClock::default();
    clock.0.set(at_ms);
    let link = Link(Rc::new(RefCell::new(SimGEarth::new(clock.clone(), hotel))));
    let mut sink = MemSink::default();
    let mut job = Job::new(options);
    let run = run_job_from(
        &mut job,
        link,
        drop,
        &mut MemFetch::none(),
        &mut sink,
        &clock,
        (items, None),
        source,
        |_| 50,
        |_, _| {},
    );
    (run, sink)
}

fn sim_options() -> RipOptions {
    RipOptions {
        delay_ms: 50,
        timeout_ms: 400,
        hotel_wait_ms: 10_000,
        resend_every_ms: 500,
        download_images: false,
        use_furnidata: false,
        rip_extras: false,
        generator: GENERATOR.into(),
        ..RipOptions::default()
    }
}

/// The catalogue of a rip folder, read back from its recording (as the built-in's `diff` got
/// the earlier rip: `recording::read(folder).catalog()`).
fn recorded_catalog(files: &BTreeMap<String, Vec<u8>>, folder: &str) -> RippedCatalog {
    let mut source = MemSource::with_files(files.iter());
    let mut loader = Loader::new(folder);
    let recorded: Recorded = loop {
        if let Some(r) = loader.step(&mut source, &mut || true).unwrap() {
            break r;
        }
    };
    let s = &recorded.session;
    output::assemble(
        &output::Source {
            host: &s.host,
            hotel_version: &s.hotel_version,
            client_type: &s.client_type,
            ripped_at: &s.ripped_at,
            generator: GENERATOR,
        },
        &s.modes,
        recorded.indexes,
        recorded.pages,
        &s.failed,
        s.complete,
    )
}

/// catalog_changes.json and catalog_manager_changes.sql are the built-in `diff`'s bytes for the
/// same two rips (its earlier rip read from the recording, as it did; the job reads the earlier
/// catalog.json in pieces), with and without deleting the removed rows; a rip compared with an
/// identical one has no changes (every column of every row of the earlier catalog.json came
/// back the same).
#[test]
fn diff_matches_builtin() {
    let items = items_base(&items_rows());
    let (run, first) = sim_rip(hotel(false), 0, sim_options(), Some(&items), None);
    let earlier = done(&run).folder.clone();
    // Other folders in the same place: another hotel, an older rip without catalog.json.
    let mut files = first.files.clone();
    files.insert("habbo.de_2026-01-01_10-00-00/catalog.json".into(), b"{}".to_vec());
    files.insert("habbo.nl_2000-01-01_10-00-00/README.txt".into(), b"x".to_vec());

    for delete_removed in [false, true] {
        let mut source = MemSource::with_files(files.iter());
        let options = RipOptions {
            diff_delete_removed: delete_removed,
            ..sim_options()
        };
        let (run, second) = sim_rip(hotel(true), 3_600_000, options, Some(&items), Some(&mut source));
        let report = done(&run);
        assert_ne!(report.folder, earlier);
        let changes = report.changes.clone().expect("compared");
        assert_eq!(changes.since.as_deref(), Some(earlier.as_str()));

        let old = recorded_catalog(&first.files, &earlier);
        let new = recorded_catalog(&second.files, &report.folder);
        let (mut want, want_sql) = diff::diff(
            &old,
            &new,
            &ManagerExport {
                items: Some(&items),
                furnidata: None,
                ..ManagerExport::default()
            },
            delete_removed,
        );
        want.since = Some(earlier.clone());
        assert_eq!(changes, want);
        let files = second.under(&report.folder);
        assert_eq!(
            String::from_utf8_lossy(&files["catalog_changes.json"]),
            serde_json::to_string_pretty(&want).unwrap()
        );
        assert_eq!(String::from_utf8_lossy(&files["catalog_manager_changes.sql"]), want_sql);
        // What changed, as described above, and as the built-in's diff sees it: the new ids
        // start above the highest id of the catalogue, which grew (page 8, offer 800), so the
        // disabled page (8 -> 9; page 5 below it changes parent) and the offers without an id of
        // their own (501, 502 -> 801, 802) count as removed and added.
        assert_eq!(
            want.summary(),
            "1 page and 4 offers added, 3 pages and 1 offer changed, 1 page and 3 offers removed",
            "{want:#?}"
        );
        let ids = |list: &[diff::DiffEntry]| -> Vec<(&str, i64)> {
            list.iter().map(|e| (e.what, e.id)).collect()
        };
        assert_eq!(ids(&want.added), [("page", 9), ("offer", 801), ("offer", 802), ("offer", 305), ("offer", 800)]);
        assert_eq!(ids(&want.changed), [("page", 5), ("page", 8), ("page", 100), ("offer", 300)]);
        assert_eq!(ids(&want.removed), [("offer", 501), ("offer", 303), ("offer", 502), ("page", 7)]);
        assert!(want_sql.contains("UPDATE catalog_items SET cost_credits = 60 WHERE id = 300 AND catalog_type = 'NORMAL';"), "{want_sql}");
        assert_eq!(
            want_sql.contains("DELETE FROM catalog_items WHERE id = 303 AND catalog_type = 'NORMAL';"),
            delete_removed
        );
        assert!(run.events.iter().any(|e| matches!(e, RipEvent::CatalogChanged { added: 5, .. })));
        let readme = String::from_utf8_lossy(&files["README.txt"]).replace("\r\n", "\n");
        assert!(readme.contains(&diff::readme_section(&want, delete_removed)), "{readme}");
        assert!(run.log_text().contains(&format!("Since the rip of {} ({earlier}): ", want.since_ripped_at)));
        assert!(report.files.contains(&"catalog_manager_changes.sql".to_string()));
    }

    // The same catalogue again: no changes (the earlier rip named explicitly).
    let mut source = MemSource::with_files(files.iter());
    let options = RipOptions {
        diff_with_previous: false,
        diff_against: Some(earlier.clone()),
        ..sim_options()
    };
    let (run, _) = sim_rip(hotel(false), 3_600_000, options, Some(&items), Some(&mut source));
    let changes = done(&run).changes.clone().unwrap();
    assert_eq!(changes.summary(), "no changes", "{changes:#?}");
    // Without items_base the offers are not rows (nothing to compare but the pages).
    let mut source = MemSource::with_files(files.iter());
    let (run, second) = sim_rip(hotel(true), 3_600_000, sim_options(), None, Some(&mut source));
    let report = done(&run);
    let old = recorded_catalog(&first.files, &earlier);
    let new = recorded_catalog(&second.files, &report.folder);
    let (mut want, _) = diff::diff(&old, &new, &ManagerExport::default(), false);
    want.since = Some(earlier.clone());
    assert_eq!(report.changes.as_ref(), Some(&want));

    // An earlier rip that cannot be read: a warning, no changes files.
    let mut broken = MemSource::new();
    broken
        .files
        .insert(format!("{earlier}/catalog.json"), b"{\"catalogs\": [".to_vec());
    let (run, second) = sim_rip(hotel(true), 3_600_000, sim_options(), None, Some(&mut broken));
    let report = done(&run);
    assert!(report.changes.is_none());
    assert!(run.log_text().contains(&format!("the earlier rip {earlier} cannot be compared")));
    assert!(!second.under(&report.folder).contains_key("catalog_changes.json"));
    // No folder to read from: nothing compared, nothing said.
    let (run, _) = sim_rip(hotel(true), 3_600_000, sim_options(), None, None);
    assert!(done(&run).changes.is_none());
    let _: Option<CatalogDiff> = None;
}

/// The earlier catalog.json read in pieces of any size (a token cut anywhere) gives the same
/// rows: the comparison with the new rip is the same.
#[test]
fn digest_reads_catalog_json_in_any_pieces() {
    let items = items_base(&items_rows());
    let (run, first) = sim_rip(hotel(false), 0, sim_options(), Some(&items), None);
    let earlier = done(&run).folder.clone();
    let json = first.files[&format!("{earlier}/catalog.json")].clone();
    let (run, second) = sim_rip(hotel(true), 3_600_000, sim_options(), Some(&items), None);
    let new = recorded_catalog(&second.files, &done(&run).folder);
    let compare = |piece: usize| {
        let mut digest = diff::Digest::new();
        for chunk in json.chunks(piece) {
            digest.feed(chunk, false, Some(&items), None).unwrap();
        }
        digest.feed(&[], true, Some(&items), None).unwrap();
        let rows = digest.finish().unwrap();
        diff::compare(rows, &earlier, &new, Some(&items), None, true).report
    };
    let whole = compare(json.len());
    assert!(!whole.is_empty());
    for piece in [1, 2, 3, 7, 64, 1000, 4096] {
        assert_eq!(compare(piece), whole, "pieces of {piece}");
    }
    // Cut short, or not a rip's catalog.json.
    let mut digest = diff::Digest::new();
    digest.feed(&json[..json.len() / 2], false, None, None).unwrap();
    assert!(digest.feed(&[], true, None, None).is_err());
    let mut digest = diff::Digest::new();
    assert!(digest.feed(b"[1, 2]", true, None, None).is_err());
}
