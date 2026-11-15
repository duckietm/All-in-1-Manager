//! The rip job end to end: against the fake G-Earth over TCP (real time, as the built-in crate's
//! `fake_gearth.rs` whole-rip tests) and against the simulated G-Earth on a simulated clock
//! (pacing, budgets and cancelling, exactly).

mod support;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use catalog_rip_core::export::{self, RippedCatalog, RippedMode};
use catalog_rip_core::job::{
    Failure, Job, Phase, ProfileChoice, RipOptions, Step, Summary, SPEED_UP_AFTER,
};
use catalog_rip_core::manager::{self, ExistingCatalogue, ManagerExport, ManagerMode};
use catalog_rip_core::output::items_base;
use catalog_rip_core::profiles::{self, builtin_profiles, HotelKind};
use catalog_rip_core::time::Timestamp;
use catalog_rip_core::transport::Transport;
use catalog_rip_core::{extras, missing, packets, ItemsBaseRow, RipEvent};
use support::fake::*;
use support::mem::*;
use support::sim::*;
use support::*;

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

/// What the test rips name as their maker (the plugin passes its own name and version).
const GENERATOR: &str = "Catalogue ripper plugin 9.9.9";

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
    ]
}

/// The data files a rip wrote (not README.txt, not the images).
fn data_files(summary: &Summary) -> usize {
    summary
        .files
        .iter()
        .filter(|f| *f != "README.txt" && *f != "images")
        .count()
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

/// Runs a job against the TCP fake G-Earth on `port`, 50 ms per step.
fn run_tcp(
    options: RipOptions,
    port: u16,
    fetch: &mut MemFetch,
    sink: &mut MemSink,
    items: Option<&export::ItemsBase>,
) -> Run {
    let mut job = Job::new(options);
    let clock = RealClock::new();
    run_job(
        &mut job,
        StdTransport::connect(port),
        StdTransport::finish,
        fetch,
        sink,
        &clock,
        (items, None),
        |_| 50,
        |_, _| {},
    )
}

const FOLDER: &str = "habbo.nl_2025-11-15_12-00-00";

/// The output the built-in writes for the official test hotel: the catalogue assembled from
/// the same packets (parsed by the core's parsers), written by the exporters that are byte for
/// byte the built-in's (`export.rs`, `manager.rs`), with README.txt put together as the
/// built-in's `Output::write` does (its text copied here, not taken from the job's code), and
/// the images the fake CDN serves.
fn builtin_output(images_downloaded: usize) -> BTreeMap<String, Vec<u8>> {
    let mut modes = Vec::new();
    for (mode, roots, with_parent, page_ids, failed) in [
        (
            "NORMAL",
            normal_roots(),
            true,
            vec![1, 2, 3, 4, 5, 6],
            vec![7],
        ),
        (
            "BUILDERS_CLUB",
            builders_roots(),
            false,
            vec![100, 4],
            vec![],
        ),
    ] {
        let index =
            packets::parse_index(&index_packet(INDEX, mode, &roots, with_parent, false).body)
                .unwrap();
        let pages = page_ids
            .into_iter()
            .map(|id| {
                let page = packets::parse_page(&page_answer(id, mode).unwrap().body).unwrap();
                (id, page)
            })
            .collect::<HashMap<_, _>>();
        modes.push(RippedMode {
            mode: mode.into(),
            index,
            pages,
            failed,
        });
    }
    let catalog = RippedCatalog {
        hotel: "habbo.nl".into(),
        host: "game-nl.habbo.com".into(),
        hotel_version: "WIN63-202501011200-123".into(),
        client_type: "FLASH".into(),
        ripped_at: Timestamp::new(UNIX_SECS).date_time(),
        generator: GENERATOR.into(),
        complete: true,
        modes,
    };
    let mut out = BTreeMap::new();
    let mut put = |name: &str, data: Vec<u8>| {
        out.insert(name.to_string(), data);
    };
    let json = export::catalog_json(&catalog);
    put(
        "catalog.json",
        serde_json::to_string_pretty(&json).unwrap().into_bytes(),
    );
    let sql = export::arcturus_sql_with(&catalog, None, None);
    put("catalog_pages.sql", sql.pages.clone().into_bytes());
    put("catalog_items.sql", sql.items.clone().into_bytes());
    put("catalog_pages_bc.sql", sql.pages_bc.clone().into_bytes());
    put("catalog_items_bc.sql", sql.items_bc.clone().into_bytes());
    let manager = manager::manager_sql_with(
        &catalog,
        &ManagerExport {
            items: None,
            furnidata: None,
            mode: ManagerMode::Replace,
            existing: None,
        },
    );
    put("catalog_manager.sql", manager.text.clone().into_bytes());
    assert!(missing::missing_furni(&catalog, None, None, None).is_empty());
    let answers: BTreeMap<String, Vec<u8>> =
        [("club_offers".to_string(), extra_answer(3285).unwrap().body)].into();
    put(
        "catalog_extras.json",
        serde_json::to_string_pretty(&extras::extras_json(&answers))
            .unwrap()
            .into_bytes(),
    );
    let images = format!("{images_downloaded} downloaded, 0 not on the server");
    let mut readme = export::readme(
        &catalog,
        &sql.stats,
        &images,
        "No items_base was given: item_ids are subqueries on items_base, resolved when the SQL is imported.",
    );
    readme.push_str(&manager::readme_section(&manager.stats, false));
    readme.push_str("\nHOTEL PROFILE\n  Habbo (official) (official Habbo)\n");
    put("README.txt", readme.replace('\n', "\r\n").into_bytes());

    // The images: page images (gif for the catalog_header ones, else png), icons, promos.
    let names = [
        "catalog_frontpage_headline_shop_EN",
        "front_teaser",
        "catalog_header_furni",
        "catalog_header_roombuilder",
        "cr_infopic3",
        "catalog_special_txtbg1",
        "catalog_rentables_header",
        "h5",
        "t5",
        "s5",
        "pets_h",
        "pets_t",
        "bc_header",
        "bc_teaser",
    ];
    for name in names {
        let ext = if name.starts_with("catalog_header") {
            "gif"
        } else {
            "png"
        };
        put(
            &format!("images/{name}.{ext}"),
            format!("image:/c_images/catalogue/{name}.{ext}").into_bytes(),
        );
    }
    for icon in 1..=9 {
        put(
            &format!("images/icons/icon_{icon}.png"),
            format!("image:/icons/icon_{icon}.png").into_bytes(),
        );
    }
    for promo in [
        "feature_cata_vert_new.png",
        "feature_cata_hort_dragon.png",
        "feature_cata_hort_credits.png",
    ] {
        put(
            &format!("images/frontpage/{promo}"),
            format!("image:/c_images/catalogue/{promo}").into_bytes(),
        );
    }
    out
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

// ------------------------------------------------------------------ over TCP

/// Pins the job's glue (what it asks, assembles and hands to the exporters, the files it
/// writes, README.txt, the images and their order of URLs) against a reconstruction of the
/// built-in's output: the same packets parsed into a `RippedCatalog` and written by the
/// exporters that are byte-identical to the built-in's (`export.rs`, `manager.rs`), with the
/// README sections the built-in's `Output::write` adds copied into [`builtin_output`]. It is
/// not a differential run of the built-in crate (that crate is not a dependency here); the
/// exporters themselves are covered by being byte-identical copies.
#[test]
fn full_rip_matches_reconstructed_builtin_output() {
    let (port, seen, fake) = start(official());
    let mut opts = options(port);
    opts.download_images = true;
    opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
    opts.icon_url = Some(format!("{CDN}/icons/icon_"));
    let mut fetch = fake_cdn();
    let mut sink = MemSink::default();
    let run = run_tcp(opts, port, &mut fetch, &mut sink, None);
    fake.join().unwrap();
    let report = done(&run).clone();

    // --- the files: those of the built-in, byte for byte
    assert_eq!(report.folder, FOLDER);
    let got = sink.under(FOLDER);
    let want = builtin_output(26);
    assert!(got == want, "{}", show_diff(&got, &want));
    assert_eq!(report.images.downloaded, 26);
    assert_eq!(report.images.failed, 0);
    assert_eq!(
        report.files,
        [
            "catalog.json",
            "catalog_pages.sql",
            "catalog_items.sql",
            "catalog_pages_bc.sql",
            "catalog_items_bc.sql",
            "catalog_manager.sql",
            "catalog_extras.json",
            "images",
            "README.txt"
        ]
    );

    // --- protocol (as the built-in's test)
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
    // The images: each page image as gif first, then png; nothing from Habbo's host needed.
    assert!(fetch
        .asked
        .contains(&format!("{CDN}/c_images/catalogue/cr_infopic3.gif")));
    assert!(
        fetch.asked.iter().all(|u| u.starts_with(CDN)),
        "{:?}",
        fetch.asked
    );

    // --- log and events
    let log = run.log_text();
    assert!(log.contains("Hotel profile: Habbo (official)"), "{log}");
    assert!(log.contains("GetCatalogIndex 1195 (G-Earth)"), "{log}");
    // Pages 6 and 7 went unanswered once each: the delay (at least 50 ms) doubled twice.
    assert!(log.contains("slowing down: 100 ms"), "{log}");
    assert_eq!(report.delay_ms, 200);
    let events = &run.events;
    assert!(
        events.contains(&RipEvent::DelayChanged { delay_ms: 200 }),
        "{events:?}"
    );
    assert!(events.contains(&RipEvent::IndexRead {
        mode: "NORMAL".into(),
        pages: 8
    }));
    assert!(events.contains(&RipEvent::PageDone {
        mode: "NORMAL".into(),
        page_id: 7,
        ok: false
    }));
    assert!(matches!(
        events.last(),
        Some(RipEvent::Finished { complete: true, .. })
    ));

    // --- summary
    assert!(report.complete);
    assert_eq!(report.profile, profiles::OFFICIAL);
    assert_eq!(report.kind, HotelKind::Official);
    assert_eq!(report.headers.page, PAGE);
    assert_eq!(report.pages, 8);
    assert_eq!(report.pages_failed, 1);
    assert_eq!(report.items, 7);
    assert_eq!(report.frontpage_items, 3);
    assert_eq!(report.sql.pages, 10);
    assert_eq!((report.sql.bc_pages, report.sql.bc_items), (2, 1));
    assert_eq!(report.sql.synthetic_page_ids, 1);
    assert_eq!(report.sql.items, 7);
    assert_eq!(report.manager.pages, 10);
    assert_eq!(
        report.manager.offers, 0,
        "no items_base: no literal item_ids"
    );

    // --- a few contents, as the built-in's test checks them
    let path = |name: &str| format!("{FOLDER}/{name}");
    let json: serde_json::Value = serde_json::from_str(&sink.text(&path("catalog.json"))).unwrap();
    assert_eq!(json["hotel"], "habbo.nl");
    assert_eq!(json["generator"], "Catalogue ripper plugin 9.9.9 (G-Earth)");
    assert_eq!(json["catalogs"][0]["failed_pages"], serde_json::json!([7]));
    assert_eq!(json["catalogs"][0]["pages"][3]["page_status"], "no answer");
    assert_eq!(json["catalogs"][1]["pages"][0]["packet_layout"], "classic");
    let items_sql = sink.text(&path("catalog_items.sql"));
    assert!(items_sql.contains("(300, CONCAT_WS(';', (SELECT `id` FROM `items_base` WHERE `type` = 's' AND `sprite_id` = 4001 ORDER BY `id` LIMIT 1)), 3, 'rare_dragon_ltd', 50, 0, 0, 1, 100, 37, 1, 300, 0, '', '1', '0')"), "{items_sql}");
    assert!(sink
        .text(&path("catalog_manager.sql"))
        .contains("-- skipped: NORMAL offer 300"));
    let readme = sink.text(&path("README.txt"));
    assert!(readme.contains("Import catalog_pages.sql, then catalog_items.sql"));
    assert!(readme.contains("ALL-IN-1 MANAGER IMPORT"));
    assert!(readme.contains("by Catalogue ripper plugin 9.9.9\r\n"), "{readme}");
    assert!(items_sql.contains(" by Catalogue ripper plugin 9.9.9.\n"));
    assert!(readme.contains("Habbo (official) (official Habbo)"));
    assert!(!readme.contains("CANCELLED"));
    assert_eq!(
        sink.text(&path("images/frontpage/feature_cata_vert_new.png")),
        "image:/c_images/catalogue/feature_cata_vert_new.png"
    );
}

#[test]
fn rips_a_flash_retro_with_the_arcturus_profile() {
    let (port, seen, fake) = start(flash_retro());
    let rows = items_rows();
    let items = items_base(&rows);
    let mut sink = MemSink::default();
    let run = run_tcp(
        options(port),
        port,
        &mut MemFetch::none(),
        &mut sink,
        Some(&items),
    );
    fake.join().unwrap();
    let report = done(&run);

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
    let folder = &report.folder;
    assert!(folder.starts_with("game.myretro.net_"), "{folder}");
    let extras: serde_json::Value =
        serde_json::from_str(&sink.text(&format!("{folder}/catalog_extras.json"))).unwrap();
    assert_eq!(extras["bundle_discount"]["bundle_size"], 6);
    assert_eq!(
        extras["gift_wrapping"]["gift_wrappers"],
        serde_json::json!([3372])
    );

    let log = run.log_text();
    assert!(
        log.contains("picked by the client revision PRODUCTION-201611291003-338511768"),
        "{log}"
    );
    assert!(log.contains("GetCatalogIndex 1195 (profile)"), "{log}");
    assert!(log.contains("BUILDERS_CLUB: no index"), "{log}");
    assert_eq!(report.profile, profiles::ARCTURUS_FLASH);
    assert_eq!(report.kind, HotelKind::FlashRetro);
    assert!(report.complete);
    assert_eq!(report.pages, 6);
    assert_eq!(report.pages_failed, 1);
    assert_eq!(report.items, 6);
    let json: serde_json::Value =
        serde_json::from_str(&sink.text(&format!("{folder}/catalog.json"))).unwrap();
    assert_eq!(json["catalogs"].as_array().unwrap().len(), 1);
    assert_eq!(json["catalogs"][0]["index_has_parent_ids"], false);
    let shop = &json["catalogs"][0]["pages"][1]["children"][0];
    assert_eq!(shop["packet_layout"], "classic without mode");
    // Every product is in items_base: the Manager file has every offer, literal ids.
    assert_eq!(report.manager.offers, 6);
    assert_eq!(report.manager.offers_unresolved, 0);
    let items_sql = sink.text(&format!("{folder}/catalog_items.sql"));
    assert!(items_sql.contains("(300, '10', 3,"), "{items_sql}");
}

#[test]
fn waits_until_the_client_opened_the_catalogue() {
    let (port, seen, fake) = start(Hotel {
        index_locked_until_client: true,
        ..official()
    });
    let mut opts = options(port);
    opts.builders_club = false;
    let mut steps_waiting = 0;
    let mut job = Job::new(opts);
    let clock = RealClock::new();
    let run = run_job(
        &mut job,
        StdTransport::connect(port),
        StdTransport::finish,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        &clock,
        (None, None),
        |_| 50,
        |_, step| {
            if matches!(step, Step::Waiting(text) if text.contains("NORMAL index request")) {
                steps_waiting += 1;
            }
        },
    );
    fake.join().unwrap();
    assert_eq!(done(&run).pages, 6);
    assert!(steps_waiting > 0, "the page says what the user should do");
    let index_requests = seen
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.0 == GET_INDEX)
        .count();
    assert_eq!(
        index_requests, 3,
        "first try, the periodic retry, then again after the client's own request"
    );
}

#[test]
fn learns_the_header_ids_of_an_unknown_retro() {
    // No profile matches (not a PRODUCTION- revision) and G-Earth has no packet info.
    let (port, seen, fake) = start(Hotel {
        host: "game.unknown-retro.org",
        version: "WIN63-custom",
        client_browses: true,
        ..flash_retro()
    });
    let mut opts = options(port);
    opts.builders_club = false;
    let run = run_tcp(
        opts,
        port,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
    );
    fake.join().unwrap();
    check_protocol(&seen.lock().unwrap());
    let report = done(&run);
    let log = run.log_text();
    assert!(
        log.contains("Open the catalogue in your client now"),
        "{log}"
    );
    assert!(log.contains("GetCatalogPage 412 (learned)"), "{log}");
    assert!(report.learned_headers);
    assert_eq!(report.headers.page, PAGE);
    assert_eq!(report.pages, 6);
    assert_eq!(report.profile, "game.unknown-retro.org (learned)");
    assert!(run
        .events
        .iter()
        .any(|e| matches!(e, RipEvent::LearningHeaders { missing } if missing.len() == 4)));

    // The profile file the plugin saves is picked by the host next time.
    let file = report.learned_profile.clone().expect("a profile file");
    assert_eq!(file.0, "game.unknown-retro.org__learned_.json");
    let all = profiles::profiles(&[file]);
    let conn = catalog_rip_core::ConnectionInfo {
        host: "game.unknown-retro.org".into(),
        port: 1,
        hotel_version: "WIN63-custom".into(),
        client_identifier: String::new(),
        client_type: "FLASH".into(),
        packets: Vec::new(),
    };
    let (picked, _) = profiles::detect(&all, &conn).unwrap();
    assert_eq!(
        picked.headers.to_text(),
        format!("GetCatalogIndex={GET_INDEX},GetCatalogPage={GET_PAGE},CatalogIndex={INDEX},CatalogPage={PAGE}")
    );
}

#[test]
fn an_unverified_retro_profile_stops_with_a_clear_error() {
    let (port, seen, fake) = start(Hotel {
        version: "PRODUCTION-201601012205-226667486",
        ..flash_retro()
    });
    let mut opts = options(port);
    opts.learn_headers = false;
    let run = run_tcp(
        opts,
        port,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
    );
    fake.join().unwrap();
    let failure = failed(&run);
    assert!(
        failure.message.contains(profiles::PLUS_FLASH),
        "{}",
        failure.message
    );
    assert!(failure
        .message
        .contains("set the four ids in the hotel profile"));
    assert!(failure.partial.is_none() && failure.gearth_error.is_none());
    assert!(seen.lock().unwrap().requests.is_empty(), "nothing sent");
}

#[test]
fn a_fixed_profile_with_override_ids() {
    // The user picked a profile whose ids are wrong; the overrides fix them.
    let (port, _, fake) = start(flash_retro());
    let mut opts = options(port);
    opts.builders_club = false;
    opts.header_overrides =
        catalog_rip_core::HeaderMap::parse(&format!("CatalogPage={PAGE}")).unwrap();
    let mut profile = builtin_profiles()
        .into_iter()
        .find(|p| p.name == profiles::NITRO)
        .unwrap();
    profile.headers.catalog_page = Some(1);
    profile.name = "Mine".into();
    opts.profile = ProfileChoice::Fixed(profile);
    let run = run_tcp(
        opts,
        port,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
    );
    fake.join().unwrap();
    let report = done(&run);
    assert_eq!(report.profile, "Mine");
    assert_eq!(report.pages, 6);
    assert!(run
        .log_text()
        .contains(&format!("CatalogPage {PAGE} (override)")));
}

// ------------------------------------------------------------------ simulated

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

struct Sim {
    clock: SimClock,
    link: Link,
}

impl Sim {
    fn new(hotel: SimHotel) -> Sim {
        let clock = SimClock::default();
        let link = Link(Rc::new(RefCell::new(SimGEarth::new(clock.clone(), hotel))));
        Sim { clock, link }
    }

    fn g(&self) -> std::cell::RefMut<'_, SimGEarth> {
        self.link.0.borrow_mut()
    }

    /// Runs `job`; `each` gets the job, the step and the simulated time after it.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        job: &mut Job,
        fetch: &mut MemFetch,
        sink: &mut MemSink,
        items: Option<&export::ItemsBase>,
        budget: impl Fn(usize) -> u32,
        mut each: impl FnMut(&mut Job, &Step, u64),
    ) -> Run {
        let clock = self.clock.clone();
        run_job(
            job,
            self.link.clone(),
            drop,
            fetch,
            sink,
            &self.clock,
            (items, None),
            budget,
            |job, step| each(job, step, clock.0.get()),
        )
    }
}

fn sim_options() -> RipOptions {
    RipOptions {
        delay_ms: 50,
        timeout_ms: 400,
        hotel_wait_ms: 10_000,
        resend_every_ms: 500,
        download_images: false,
        use_furnidata: false,
        ..RipOptions::default()
    }
}

/// The page requests: (page id, time sent).
fn page_requests(sim: &Sim) -> Vec<(i32, u64)> {
    sim.g()
        .requests
        .iter()
        .filter(|r| r.0 == GET_PAGE)
        .map(|r| (r.1, r.3))
        .collect()
}

#[test]
fn pacing_respects_delay() {
    let sim = Sim::new(SimHotel {
        noise_every_ms: Some(7),
        ..big_hotel(30)
    });
    let mut job = Job::new(RipOptions {
        delay_ms: 300,
        adaptive_delay: false,
        rip_extras: false,
        builders_club: false,
        ..sim_options()
    });
    let run = sim.run(
        &mut job,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
        |_| 40,
        |_, _, _| {},
    );
    assert_eq!(done(&run).pages, 30);
    let times: Vec<u64> = sim.g().requests.iter().map(|r| r.3).collect();
    assert_eq!(times.len(), 31, "the index and 30 pages");
    for pair in times.windows(2) {
        let gap = pair[1] - pair[0];
        // Sent as soon as the delay has passed by the clock, never earlier.
        assert!((300..=305).contains(&gap), "{times:?}");
    }
    // The steps were much shorter than the delay; meanwhile the game kept flowing.
    assert!(run.steps >= 30 * 300 / 40, "{}", run.steps);
    let g = sim.g();
    assert!(g.intercepts_sent > 1_000, "{}", g.intercepts_sent);
    g.check_echoes();
    assert_eq!(
        g.longest_hold_ms, 0,
        "every intercept is answered as it is read"
    );
}

#[test]
fn adaptive_delay_backs_off_and_recovers() {
    let mut hotel = big_hotel(40);
    hotel.lose_first.insert((3, "NORMAL".into()), 1);
    hotel.lose_first.insert((5, "NORMAL".into()), 1);
    let sim = Sim::new(hotel);
    let mut job = Job::new(RipOptions {
        rip_extras: false,
        builders_club: false,
        ..sim_options()
    });
    let run = sim.run(
        &mut job,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
        |_| 100,
        |_, _, _| {},
    );
    let report = done(&run);
    assert_eq!(report.pages, 40);
    assert_eq!(report.pages_failed, 0);
    let delays: Vec<u64> = run
        .events
        .iter()
        .filter_map(|e| match e {
            RipEvent::DelayChanged { delay_ms } => Some(*delay_ms),
            _ => None,
        })
        .collect();
    // Doubled at each unanswered page, halved after every 15 answers in a row.
    assert_eq!(SPEED_UP_AFTER, 15);
    assert_eq!(delays, [100, 200, 100, 50]);
    assert_eq!(report.delay_ms, 50);
    let log = run.log_text();
    assert!(
        log.contains("A request went unanswered, slowing down: 200 ms between requests now."),
        "{log}"
    );
    assert!(
        log.contains("Answers come back again, speeding up: 50 ms between requests now."),
        "{log}"
    );

    // The gap before each request is the delay at that time.
    let requests = page_requests(&sim);
    let gap_before = |page: i32| {
        let i = requests.iter().rposition(|r| r.0 == page).unwrap();
        requests[i].1 - requests[i - 1].1
    };
    let ids: Vec<i32> = requests.iter().map(|r| r.0).collect();
    assert_eq!(&ids[..7], [1, 2, 3, 3, 4, 5, 5]);
    // Page 3's retry waits for its answer timeout (400 ms), then the slower delay holds.
    assert!(gap_before(4) >= 100);
    assert_eq!(gap_before(7), 200);
    assert_eq!(gap_before(20), 200);
    assert_eq!(gap_before(21), 100, "after 15 answers (pages 6-20)");
    assert_eq!(gap_before(35), 100);
    assert_eq!(gap_before(36), 50, "after 15 more (pages 21-35)");
}

#[test]
fn cancel_stops_within_one_tick() {
    let sim = Sim::new(SimHotel {
        noise_every_ms: Some(11),
        ..official_hotel()
    });
    let mut opts = sim_options();
    opts.download_images = true;
    opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
    opts.icon_url = Some(format!("{CDN}/icons/icon_"));
    let mut job = Job::new(opts);
    let mut fetch = fake_cdn();
    let mut sink = MemSink::default();
    let mut cancelled_at: Option<(usize, usize)> = None; // (step, requests so far)
    let mut step_no = 0;
    let link = sim.link.clone();
    let run = sim.run(
        &mut job,
        &mut fetch,
        &mut sink,
        None,
        |_| 30,
        |job, _, _| {
            step_no += 1;
            let progress = job.progress();
            if cancelled_at.is_none() && progress.phase == Phase::Pages && progress.done >= 2 {
                job.cancel();
                cancelled_at = Some((step_no, link.0.borrow().requests.len()));
            }
        },
    );
    let (cancel_step, requests_then) = cancelled_at.expect("cancelled during the pages");
    let partial = failed(&run).partial.as_ref().expect("what was written");
    // The step after the cancel ends the requests and lets G-Earth go; the next ones write the
    // output, one data file per step, and the last one README.txt (no images).
    assert_eq!(
        run.steps,
        cancel_step + 1 + data_files(partial) + 1,
        "the job ends right after the output is written"
    );
    assert_eq!(
        sim.g().requests.len(),
        requests_then,
        "nothing sent after the cancel"
    );
    assert!(!job.uses_gearth());

    let failure = failed(&run);
    assert!(failure.cancelled);
    assert!(failure.gearth_error.is_none() && !failure.reconnect);
    assert!(failure.message.contains("cancelled"), "{}", failure.message);
    assert!(failure.message.contains("saved in"), "{}", failure.message);
    let partial = failure.partial.as_ref().expect("what was written");
    assert!(!partial.complete);
    assert!(partial.cancelled);
    assert_eq!(partial.pages, 2);

    // The folder has what was read, without images, and the README says it was cancelled.
    let files = sink.under(&partial.folder);
    assert!(
        files.keys().all(|f| !f.starts_with("images")),
        "{:?}",
        files.keys()
    );
    assert!(fetch.asked.is_empty(), "no image downloads after a cancel");
    let json: serde_json::Value = serde_json::from_slice(&files["catalog.json"]).unwrap();
    assert_eq!(json["complete"], false);
    assert!(files.contains_key("catalog_pages.sql") && files.contains_key("catalog_manager.sql"));
    let readme = String::from_utf8(files["README.txt"].clone()).unwrap();
    assert!(
        readme.contains("\r\nCANCELLED\r\n  The rip was cancelled"),
        "{readme}"
    );
    assert!(
        readme.contains("not downloaded (the rip was cancelled)"),
        "{readme}"
    );
    sim.g().check_echoes();

    // A step after the end says the same again.
    let mut job2 = job;
    let again = job2.step(
        &mut catalog_rip_core::job::JobIo {
            transport: &mut catalog_rip_core::transport::Closed,
            fetch: &mut MemFetch::none(),
            sink: &mut MemSink::default(),
            clock: &sim.clock,
            items: None,
            existing: None,
        },
        10,
    );
    assert_eq!(again, run.end);
}

#[test]
fn cancel_during_the_images_finishes_the_rip() {
    let sim = Sim::new(official_hotel());
    let mut opts = sim_options();
    opts.download_images = true;
    opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
    opts.icon_url = Some(format!("{CDN}/icons/icon_"));
    let mut job = Job::new(opts);
    // Each download takes 30 ms: one image per step.
    let mut cdn = fake_cdn();
    let clock = sim.clock.clone();
    let mut fetch = MemFetch {
        answer: Box::new(move |url| {
            clock.0.set(clock.0.get() + 30);
            (cdn.answer)(url)
        }),
        asked: Vec::new(),
    };
    let mut sink = MemSink::default();
    let run = sim.run(
        &mut job,
        &mut fetch,
        &mut sink,
        None,
        |_| 20,
        |job, _, _| {
            let p = job.progress();
            if p.phase == Phase::Images && p.done == 3 {
                job.cancel();
            }
        },
    );
    let report = done(&run);
    assert!(report.complete && report.cancelled);
    assert_eq!(report.images.downloaded, 3);
    let readme = sink.text(&format!("{}/README.txt", report.folder));
    assert!(
        readme.contains("3 downloaded, 0 not on the server (stopped: cancelled)"),
        "{readme}"
    );
    assert!(readme.contains("CANCELLED"));
}

#[test]
fn budget_is_respected() {
    let sim = Sim::new(SimHotel {
        noise_every_ms: Some(3),
        latency_ms: 9,
        ..official_hotel()
    });
    let mut opts = sim_options();
    opts.download_images = true;
    opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
    opts.icon_url = Some(format!("{CDN}/icons/icon_"));
    let mut job = Job::new(opts);
    let budgets = [0u32, 1, 7, 25, 60, 13];
    let budget = |i: usize| budgets[i % budgets.len()];
    let mut before = 0;
    let mut i = 0;
    let mut longest = 0;
    let run = sim.run(
        &mut job,
        &mut fake_cdn(),
        &mut MemSink::default(),
        None,
        budget,
        |_, _, now| {
            let spent = now - before;
            assert!(
                spent <= u64::from(budget(i)),
                "step {i}: {spent} ms of {} ms",
                budget(i)
            );
            longest = longest.max(spent);
            before = now;
            i += 1;
        },
    );
    let report = done(&run);
    assert_eq!(report.pages, 8);
    assert_eq!(report.images.downloaded, 26);
    assert_eq!(
        longest, 60,
        "the biggest budgets were used up while waiting"
    );
    let g = sim.g();
    g.check_echoes();
    assert_eq!(g.longest_hold_ms, 0);
}

#[test]
fn missing_items_base_skips_lookup() {
    let rip = |items: Option<&export::ItemsBase>| {
        let sim = Sim::new(official_hotel());
        let mut job = Job::new(RipOptions {
            builders_club: false,
            ..sim_options()
        });
        let mut sink = MemSink::default();
        let run = sim.run(
            &mut job,
            &mut MemFetch::none(),
            &mut sink,
            items,
            |_| 50,
            |_, _, _| {},
        );
        let report = done(&run).clone();
        let text = |name: &str| sink.text(&format!("{}/{name}", report.folder));
        (
            report.clone(),
            text("catalog_items.sql"),
            text("README.txt"),
            sink,
            run.log_text(),
        )
    };

    // Without items_base: every item id is a subquery resolved on import.
    let (report, items_sql, readme, sink, _) = rip(None);
    assert!(
        items_sql.contains("(300, CONCAT_WS(';', (SELECT `id` FROM `items_base`"),
        "{items_sql}"
    );
    assert!(!items_sql.contains("(300, '10',"));
    assert!(readme.contains("No items_base was given: item_ids are subqueries on items_base"));
    assert_eq!(report.manager.offers, 0);
    assert!(report.missing_furni.is_empty());
    assert!(!sink.files.keys().any(|f| f.ends_with("missing_furni.json")));

    // With it: literal ids, and the Manager file has the offers.
    let rows = items_rows();
    let items = items_base(&rows);
    let (report, items_sql, readme, _, log) = rip(Some(&items));
    assert!(items_sql.contains("(300, '10', 3,"), "{items_sql}");
    assert!(readme.contains("item_ids were matched against the hotel's items_base"));
    assert_eq!(report.manager.offers, 6);
    assert!(log.contains("Using the hotel's items_base."), "{log}");
}

#[test]
fn furnidata_through_fetch_and_add_mode() {
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(RipOptions {
        builders_club: false,
        use_furnidata: true,
        furnidata_url: Some("http://data.test/furnidata.xml".into()),
        manager_mode: ManagerMode::Add,
        ..sim_options()
    });
    let mut fetch = MemFetch {
        answer: Box::new(|url| {
            match url {
            "http://data.test/furnidata.xml" => Ok(br#"<furnidata><roomitemtypes><furnitype id="4001" classname="rare_dragon"/><furnitype id="4002" classname="table"/></roomitemtypes>
           <wallitemtypes><furnitype id="5001" classname="poster"/></wallitemtypes></furnidata>"#.to_vec()),
            _ => Err(catalog_rip_core::job::FetchError::NotFound),
        }
        }),
        asked: Vec::new(),
    };
    // items_base where no sprite id matches the source hotel, and the hotel's catalogue ids.
    let row = |id, name: &str, t: &str| ItemsBaseRow {
        id,
        sprite_id: 90_000 + id as i32,
        item_type: t.into(),
        item_name: name.into(),
        effect_id: 0,
    };
    let rows = vec![
        row(500, "rare_dragon", "s"),
        row(501, "table", "s"),
        row(502, "poster", "i"),
        row(503, "ACH_Test1", "b"),
    ];
    let items = items_base(&rows);
    let mut existing = ExistingCatalogue::default();
    existing
        .pages
        .insert("NORMAL".into(), [1, 2, 1000].into_iter().collect());
    existing
        .offers
        .insert("NORMAL".into(), [7, 5000].into_iter().collect());
    let mut sink = MemSink::default();
    let clock = sim.clock.clone();
    let run = run_job(
        &mut job,
        sim.link.clone(),
        drop,
        &mut fetch,
        &mut sink,
        &clock,
        (Some(&items), Some(&existing)),
        |_| 50,
        |_, _| {},
    );
    let report = done(&run);
    assert_eq!(report.furnidata_names, 3);
    // The productdata of an official hotel follows from its host (not there: a warning).
    assert_eq!(
        fetch.asked,
        [
            "http://data.test/furnidata.xml",
            "https://www.habbo.nl/gamedata/productdata_json/1"
        ]
    );
    assert!(run
        .log_text()
        .contains("Warning: the productdata could not be read"));
    // dragon (4001), badge, bundle (4002 + poster 5001) resolve by class name; the rest not.
    assert_eq!(report.manager.offers, 3, "{:?}", report.manager);
    let folder = &report.folder;
    let items_sql = sink.text(&format!("{folder}/catalog_items.sql"));
    assert!(items_sql.contains("(300, '500', 3,"), "{items_sql}");
    let readme = sink.text(&format!("{folder}/README.txt"));
    assert!(readme.contains("FURNIDATA"), "{readme}");
    assert!(readme.contains("ADDED next to the existing"), "{readme}");
    assert!(!sink
        .text(&format!("{folder}/catalog_manager.sql"))
        .contains("DELETE"));
    assert!(sink
        .files
        .contains_key(&format!("{folder}/missing_furnidata.json")));
}

#[test]
fn hotel_disconnect_keeps_partial_output() {
    let sim = Sim::new(SimHotel {
        end_at_page_request: Some(3),
        ..official_hotel()
    });
    let mut job = Job::new(RipOptions {
        builders_club: false,
        ..sim_options()
    });
    let mut sink = MemSink::default();
    let run = sim.run(
        &mut job,
        &mut MemFetch::none(),
        &mut sink,
        None,
        |_| 50,
        |_, _, _| {},
    );
    let failure = failed(&run);
    assert!(!failure.cancelled);
    assert!(
        failure.message.contains("hotel connection ended"),
        "{}",
        failure.message
    );
    assert!(
        failure.message.contains("saved in habbo.nl_"),
        "{}",
        failure.message
    );
    let partial = failure.partial.as_ref().unwrap();
    assert_eq!(partial.pages, 2);
    let files = sink.under(&partial.folder);
    let json: serde_json::Value = serde_json::from_slice(&files["catalog.json"]).unwrap();
    assert_eq!(json["complete"], false);
    let readme = String::from_utf8(files["README.txt"].clone()).unwrap();
    assert!(readme.contains("INCOMPLETE"));
    assert!(!readme.contains("CANCELLED"));
    assert!(files.contains_key("catalog_pages.sql") && files.contains_key("catalog_manager.sql"));
}

#[test]
fn gearth_closing_ends_the_session_and_keeps_partial_output() {
    struct Dropping {
        link: Link,
        after_requests: usize,
    }
    impl Transport for Dropping {
        fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.link.send(data)
        }
        fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            if self.link.0.borrow().requests.len() >= self.after_requests {
                return Ok(Some(Vec::new())); // G-Earth closed the connection
            }
            self.link.recv(max, timeout_ms)
        }
    }
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(sim_options());
    let mut sink = MemSink::default();
    let mut closed_at = None;
    let mut steps = 0;
    let run = run_job(
        &mut job,
        Dropping {
            link: sim.link.clone(),
            after_requests: 4,
        },
        |_| {},
        &mut MemFetch::none(),
        &mut sink,
        &sim.clock,
        (None, None),
        |_| 50,
        |job, step| {
            steps += 1;
            if closed_at.is_none() && !job.uses_gearth() {
                closed_at = Some((steps, step.clone()));
            }
        },
    );
    let failure = failed(&run);
    assert_eq!(
        failure.gearth_error,
        Some(catalog_rip_core::gearth::Error::Closed)
    );
    assert!(
        failure.message.starts_with("G-Earth closed the connection"),
        "{}",
        failure.message
    );
    assert!(
        failure.partial.is_some(),
        "the indexes and pages read are written"
    );
    // The step that lost G-Earth returns at once (before the export's fetches); the next ones
    // write the partial output (one data file per step) and end the job.
    let partial = failure.partial.as_ref().unwrap();
    assert_eq!(
        closed_at,
        Some((run.steps - data_files(partial) - 1, Step::Running)),
        "the session ended in the step before the export"
    );
}

#[test]
fn reconnect_when_gearth_lets_the_extension_go_before_the_hotel() {
    struct Lets(Link);
    impl Transport for Lets {
        fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.0.send(data)
        }
        fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            // InfoRequest, then Init: then G-Earth lets the extension go.
            let got = self.0.recv(max, timeout_ms)?;
            match got {
                Some(frame) if frame[4..6] == [0, 5] => Ok(Some(Vec::new())),
                other => Ok(other),
            }
        }
    }
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(sim_options());
    let run = run_job(
        &mut job,
        Lets(sim.link.clone()),
        |_| {},
        &mut MemFetch::none(),
        &mut MemSink::default(),
        &sim.clock,
        (None, None),
        |_| 50,
        |_, _| {},
    );
    let failure = failed(&run);
    assert!(failure.reconnect, "{failure:?}");
    assert!(failure
        .message
        .contains("let the extension go before the hotel connection"));
    assert!(run.log_text().contains("G-Earth accepted the extension"));
}

#[test]
fn nothing_to_rip_and_no_gearth() {
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(RipOptions {
        normal: false,
        builders_club: false,
        ..sim_options()
    });
    let run = sim.run(
        &mut job,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
        |_| 50,
        |_, _, _| {},
    );
    assert!(failed(&run).message.starts_with("Nothing to rip"));

    // Something that never sends G-Earth's InfoRequest.
    struct Silent(SimClock);
    impl Transport for Silent {
        fn send(&mut self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn recv(&mut self, _: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            self.0 .0.set(self.0 .0.get() + u64::from(timeout_ms));
            Ok(None)
        }
    }
    let clock = SimClock::default();
    let mut job = Job::new(sim_options());
    let run = run_job(
        &mut job,
        Silent(clock.clone()),
        |_| {},
        &mut MemFetch::none(),
        &mut MemSink::default(),
        &clock,
        (None, None),
        |_| 100,
        |_, _| {},
    );
    let failure = failed(&run);
    assert_eq!(
        failure.gearth_error,
        Some(catalog_rip_core::gearth::Error::NoInfoRequest)
    );
    assert_eq!(clock.0.get(), 3_000, "the handshake timeout");
}

// ------------------------------------------------------------------ review fixes

/// A fetch that takes `ms` of simulated time per call and answers as `inner` does; the count
/// of calls is shared.
fn slow_fetch(
    clock: &SimClock,
    ms: u64,
    mut inner: MemFetch,
) -> (MemFetch, Rc<std::cell::Cell<usize>>) {
    let clock = clock.clone();
    let calls = Rc::new(std::cell::Cell::new(0));
    let counted = calls.clone();
    let fetch = MemFetch {
        answer: Box::new(move |url| {
            counted.set(counted.get() + 1);
            clock.0.set(clock.0.get() + ms);
            (inner.answer)(url)
        }),
        asked: Vec::new(),
    };
    (fetch, calls)
}

#[test]
fn an_answer_that_comes_during_the_delay_is_still_asked_and_counted() {
    // Page 2's first answer is lost (the delay doubles to 100 ms); with its retry, page 3's
    // answer comes unasked 50 ms later, while page 3 waits for the delay. As in the built-in,
    // page 3 is still asked, and its answer counts towards speeding up again.
    let mut hotel = big_hotel(20);
    hotel.lose_first.insert((2, "NORMAL".into()), 1);
    hotel.also_answer.insert((2, 2), (3, 50));
    let sim = Sim::new(hotel);
    let mut job = Job::new(RipOptions {
        rip_extras: false,
        builders_club: false,
        ..sim_options()
    });
    let run = sim.run(
        &mut job,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
        |_| 30,
        |_, _, _| {},
    );
    assert_eq!(done(&run).pages, 20);
    let requests = page_requests(&sim);
    let ids: Vec<i32> = requests.iter().map(|r| r.0).collect();
    assert_eq!(
        &ids[..5],
        [1, 2, 2, 3, 4],
        "page 3 is asked although its answer is there"
    );
    let gap_before = |page: i32| {
        let i = requests.iter().position(|r| r.0 == page).unwrap();
        requests[i].1 - requests[i - 1].1
    };
    assert_eq!(gap_before(3), 100);
    // 15 answers in a row (pages 3-17, page 3 included) bring the delay back to 50 ms.
    assert_eq!(gap_before(17), 100);
    assert_eq!(gap_before(18), 50);
}

#[test]
fn learning_keeps_what_comes_after_the_last_id() {
    // The user's client opens the catalogue: its requests and the answers go by together; the
    // last id is learned from the page answer, and the NORMAL index that follows in the same
    // read is kept for the requests. This hotel never answers the ripper's index request, so
    // the rip only gets on with the kept index.
    let index = index_packet(INDEX, "NORMAL", &normal_roots(), true, false);
    let mut hotel = official_hotel();
    hotel.connection.host = "game.unknown-retro.org".into();
    hotel.connection.hotel_version = "WIN63-custom".into();
    hotel.connection.packets = Vec::new();
    hotel.index = Box::new(|_| None);
    hotel.browse_after_start = vec![
        (true, packets::get_catalog_index(GET_INDEX, "NORMAL")),
        (false, index.clone()),
        (true, packets::get_catalog_page(GET_PAGE, 3, "NORMAL")),
        (false, page_answer(3, "NORMAL").unwrap()),
        (false, index),
    ];
    let sim = Sim::new(hotel);
    let mut job = Job::new(RipOptions {
        builders_club: false,
        rip_extras: false,
        ..sim_options()
    });
    let run = sim.run(
        &mut job,
        &mut MemFetch::none(),
        &mut MemSink::default(),
        None,
        |_| 50,
        |_, _, _| {},
    );
    let report = done(&run);
    assert!(report.learned_headers);
    assert_eq!(report.pages, 6);
    assert!(run.log_text().contains("CatalogPage 804 (learned)"));
    sim.g().check_echoes();
}

#[test]
fn cancel_with_furnidata_still_reads_it_one_fetch_per_step() {
    let sim = Sim::new(official_hotel());
    let mut opts = sim_options();
    opts.use_furnidata = true; // an official hotel: furnidata and productdata from its host
    opts.download_images = true;
    let mut job = Job::new(opts);
    let (mut fetch, calls) = slow_fetch(&sim.clock, 30, MemFetch::none());
    let mut sink = MemSink::default();
    let mut cancel_step = None;
    let mut step = 0;
    let (mut before, mut most) = (0, 0);
    let run = sim.run(
        &mut job,
        &mut fetch,
        &mut sink,
        None,
        |_| 20,
        |job, _, _| {
            step += 1;
            most = most.max(calls.get() - before);
            before = calls.get();
            let p = job.progress();
            if cancel_step.is_none() && p.phase == Phase::Pages && p.done >= 2 {
                job.cancel();
                cancel_step = Some(step);
            }
        },
    );
    let failure = failed(&run);
    // The next step lets G-Earth go; then one step per download, one per data file, and the
    // last one writes README.txt.
    let partial = failure.partial.as_ref().unwrap();
    assert_eq!(
        run.steps,
        cancel_step.unwrap() + 1 + 2 + data_files(partial) + 1
    );
    assert!(failure.cancelled);
    assert_eq!(calls.get(), 2);
    assert_eq!(most, 1, "one fetch per step");
    assert_eq!(
        fetch.asked,
        [
            "https://www.habbo.nl/gamedata/furnidata_json/1",
            "https://www.habbo.nl/gamedata/productdata_json/1"
        ]
    );
    let partial = failure.partial.as_ref().unwrap();
    assert!(!partial.files.iter().any(|f| f == "images"));
    let readme = sink.text(&format!("{}/README.txt", partial.folder));
    assert!(readme.contains("CANCELLED"));
}

#[test]
fn no_fetch_starts_after_the_budget() {
    // A read that overshoots its timeout (as socket timeouts do): the club offers answer is
    // read 25 ms late, so the requests end after the step's budget. The furnidata is not
    // fetched in that step any more, but first thing in the next one.
    struct Overshoot(Link, SimClock);
    impl Transport for Overshoot {
        fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.0.send(data)
        }
        fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            let got = self.0.recv(max, timeout_ms)?;
            let club = self.0 .0.borrow().requests.last().map(|r| r.0) == Some(3285);
            if club
                && got
                    .as_ref()
                    .is_some_and(|f| f.len() > 6 && f[4..6] == [0, 3])
            {
                self.1 .0.set(self.1 .0.get() + 25);
            }
            Ok(got)
        }
    }
    const BUDGET: u64 = 20;
    let sim = Sim::new(official_hotel());
    let mut opts = sim_options();
    opts.use_furnidata = true;
    opts.download_images = true;
    opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
    opts.icon_url = Some(format!("{CDN}/icons/icon_"));
    opts.site_config_url = Some(format!("{CDN}/client"));
    let mut job = Job::new(opts);
    // Every fetch takes 30 ms, longer than a step's budget. A fetch must start within the
    // budget, or as the first thing of its step.
    let step_start = Rc::new(std::cell::Cell::new(0u64));
    let late = Rc::new(std::cell::Cell::new(0usize));
    let (mut fetch, calls) = {
        let (clock, start, late) = (sim.clock.clone(), step_start.clone(), late.clone());
        let mut inner = fake_cdn();
        let calls = Rc::new(std::cell::Cell::new(0));
        let counted = calls.clone();
        let fetch = MemFetch {
            answer: Box::new(move |url| {
                let now = clock.0.get();
                if now > start.get() && now - start.get() >= BUDGET {
                    late.set(late.get() + 1);
                }
                counted.set(counted.get() + 1);
                clock.0.set(now + 30);
                (inner.answer)(url)
            }),
            asked: Vec::new(),
        };
        (fetch, calls)
    };
    let mut before = 0;
    let mut most = 0;
    let run = run_job(
        &mut job,
        Overshoot(sim.link.clone(), sim.clock.clone()),
        drop,
        &mut fetch,
        &mut MemSink::default(),
        &sim.clock,
        (None, None),
        |_| BUDGET as u32,
        |_, _| {
            most = most.max(calls.get() - before);
            before = calls.get();
            step_start.set(sim.clock.0.get());
        },
    );
    let report = done(&run);
    assert_eq!(report.images.downloaded, 26);
    assert_eq!(
        &fetch.asked[..2],
        [
            "https://www.habbo.nl/gamedata/furnidata_json/1",
            "https://www.habbo.nl/gamedata/productdata_json/1"
        ]
    );
    assert!(calls.get() > 30, "{}", calls.get());
    assert_eq!(late.get(), 0, "no fetch started after the budget");
    assert_eq!(most, 1, "one fetch per step at most");
}

#[test]
fn image_fallback_order() {
    let sim = Sim::new(official_hotel());
    let mut opts = sim_options();
    opts.download_images = true;
    opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
    opts.icon_url = Some(format!("{CDN}/icons/icon_"));
    let mut job = Job::new(opts);
    // The CDN lacks cr_infopic3 (Habbo's image host has it, as png) and front_teaser (nobody
    // has it).
    let mut cdn = fake_cdn();
    let mut fetch = MemFetch {
        answer: Box::new(move |url| {
            if url == "https://images.habbo.com/c_images/catalogue/cr_infopic3.png" {
                return Ok(b"habbo".to_vec());
            }
            if url.contains("cr_infopic3") || url.contains("front_teaser") {
                return Err(catalog_rip_core::job::FetchError::NotFound);
            }
            (cdn.answer)(url)
        }),
        asked: Vec::new(),
    };
    let mut sink = MemSink::default();
    let run = sim.run(&mut job, &mut fetch, &mut sink, None, |_| 50, |_, _, _| {});
    let report = done(&run);
    let asked = |name: &str| -> Vec<String> {
        fetch
            .asked
            .iter()
            .filter(|u| u.contains(name))
            .cloned()
            .collect()
    };
    assert_eq!(
        asked("cr_infopic3"),
        [
            format!("{CDN}/c_images/catalogue/cr_infopic3.gif"),
            format!("{CDN}/c_images/catalogue/cr_infopic3.png"),
            "https://images.habbo.com/c_images/catalogue/cr_infopic3.gif".to_string(),
            "https://images.habbo.com/c_images/catalogue/cr_infopic3.png".to_string(),
        ]
    );
    assert_eq!(asked("front_teaser").len(), 4);
    assert_eq!(report.images.downloaded, 25);
    assert_eq!(report.images.missing, 1);
    assert_eq!(report.images.missing_names, ["front_teaser"]);
    let files = sink.under(&report.folder);
    assert_eq!(files["images/cr_infopic3.png"], b"habbo");
    let readme = String::from_utf8(files["README.txt"].clone()).unwrap();
    assert!(
        readme.contains("images:                        25 downloaded, 1 not on the server"),
        "{readme}"
    );
}

#[test]
fn images_folder_is_listed_when_images_were_tried() {
    let files = |download_images: bool| {
        let sim = Sim::new(official_hotel());
        let mut job = Job::new(RipOptions {
            download_images,
            ..sim_options()
        });
        let run = sim.run(
            &mut job,
            &mut MemFetch::none(),
            &mut MemSink::default(),
            None,
            |_| 50,
            |_, _, _| {},
        );
        let report = done(&run);
        assert_eq!(report.images.downloaded, 0);
        report.files.clone()
    };
    // Nothing found, but tried: the built-in had made the folder, and listed it.
    assert!(files(true).iter().any(|f| f == "images"));
    assert!(!files(false).iter().any(|f| f == "images"));
}

/// Steps `job` and checks that no fetch runs in a step in which the job lets G-Earth go: the
/// plugin closes the connection only after such a step, so a fetch in it (an `http:ask`
/// dialog, a slow host) would hold up the user's game packets.
fn assert_no_fetch_while_gearth_open(
    sim: &Sim,
    job: &mut Job,
    transport: impl Transport,
    inner: MemFetch,
    mut each: impl FnMut(&mut Job, usize),
) -> (Run, usize) {
    let calls = Rc::new(std::cell::Cell::new(0usize));
    let mut fetch = {
        let counted = calls.clone();
        let mut inner = inner;
        MemFetch {
            answer: Box::new(move |url| {
                counted.set(counted.get() + 1);
                (inner.answer)(url)
            }),
            asked: Vec::new(),
        }
    };
    let mut used = false;
    let mut before = 0;
    let mut released = 0;
    let mut step = 0;
    let run = run_job(
        job,
        transport,
        drop,
        &mut fetch,
        &mut MemSink::default(),
        &sim.clock,
        (None, None),
        |_| 50,
        |job, _| {
            step += 1;
            let fetched = calls.get() - before;
            before = calls.get();
            if used && !job.uses_gearth() {
                released += 1;
                assert_eq!(fetched, 0, "step {step} let G-Earth go and fetched {fetched} time(s)");
            }
            used = job.uses_gearth();
            each(job, step);
        },
    );
    assert!(calls.get() > 0, "the export fetched something");
    (run, released)
}

#[test]
fn no_fetch_in_the_step_that_lets_gearth_go() {
    let furnidata_opts = || {
        let mut opts = sim_options();
        opts.use_furnidata = true; // official hotel: furnidata and productdata from its host
        opts.download_images = true;
        opts.catalog_url = Some(format!("{CDN}/c_images/catalogue"));
        opts.icon_url = Some(format!("{CDN}/icons/icon_"));
        opts
    };

    // A whole rip.
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(furnidata_opts());
    let (run, released) =
        assert_no_fetch_while_gearth_open(&sim, &mut job, sim.link.clone(), fake_cdn(), |_, _| {});
    done(&run);
    assert_eq!(released, 1);

    // Cancelled during the pages.
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(furnidata_opts());
    let mut cancelled = false;
    let (run, released) =
        assert_no_fetch_while_gearth_open(&sim, &mut job, sim.link.clone(), fake_cdn(), |job, _| {
            let p = job.progress();
            if !cancelled && p.phase == Phase::Pages && p.done >= 2 {
                job.cancel();
                cancelled = true;
            }
        });
    assert!(failed(&run).cancelled);
    assert_eq!(released, 1);

    // G-Earth closes the connection during the pages.
    struct Dropping(Link);
    impl Transport for Dropping {
        fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.0.send(data)
        }
        fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            if self.0 .0.borrow().requests.len() >= 4 {
                return Ok(Some(Vec::new()));
            }
            self.0.recv(max, timeout_ms)
        }
    }
    let sim = Sim::new(official_hotel());
    let mut job = Job::new(furnidata_opts());
    let (run, released) =
        assert_no_fetch_while_gearth_open(&sim, &mut job, Dropping(sim.link.clone()), fake_cdn(), |_, _| {});
    assert!(failed(&run).gearth_error.is_some());
    assert_eq!(released, 1);
}
