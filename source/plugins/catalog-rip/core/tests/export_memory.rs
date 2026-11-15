//! Memory and time of a Habbo-sized rip, natively: ~3,500 pages with ~31,500 offers, a 25,000
//! entry furnidata (~10 MB of JSON), a 25,000 product productdata and a 20,000 row items_base,
//! stepped through the whole job (simulated G-Earth, in-memory fetch, a sink that keeps
//! nothing) the way the plugin steps it (budget 200 per tick).
//!
//! The plugin runs in 64 MiB of wasm linear memory; the live heap of the job must stay well
//! below that (40 MiB leaves room for the allocator's overhead and fragmentation, the wasm
//! stack and data, and the plugin's own state). Each step must also stay far below the host's
//! 5 s CPU limit per call (wasm runs roughly 1.5-3x slower than native).
//!
//! Not met by one step: catalog.json is ~40 MiB at this size (pretty-printed, byte for byte
//! the built-in's), and a picked folder's `write` takes a whole file, so that step holds the
//! file next to the catalogue it is made from (~62 MiB live). The test pins that bound; getting
//! under 40 MiB needs a streamed (appending) write in the host API, or another catalog.json.
//!
//! A counting global allocator, so this file is its own test binary with a single test. Slow in
//! a debug build: run it with
//! `cargo test -p catalog-rip-core --release --test export_memory -- --ignored --nocapture`.

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use catalog_rip_core::job::{Fetch, FetchError, Job, JobIo, Phase, RipOptions, Sink, Step};
use catalog_rip_core::output::items_base;
use catalog_rip_core::transport::{Closed, Transport};
use catalog_rip_core::wire::{Packet, Writer};
use catalog_rip_core::ItemsBaseRow;
use support::sim::*;
use support::{INDEX, PAGE};

// ------------------------------------------------------------------ the counting allocator

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(bytes: usize) {
    let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Counted as the worst case of a moving realloc: old and new block both live at once.
        grew(new_size);
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            LIVE.fetch_sub(new_size, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn reset_peak() {
    PEAK.store(live(), Ordering::Relaxed);
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

// ------------------------------------------------------------------ the synthetic hotel

const ROOTS: i32 = 50;
const CHILDREN: i32 = 69;
/// 50 categories + 50 x 69 pages.
const PAGES: i32 = ROOTS + ROOTS * CHILDREN;
const OFFERS_PER_PAGE: i32 = 9;
const FLOOR: i32 = 22_000;
const FURNI: i32 = 25_000;
/// items_base has the furni 1..=KNOWN (by class name); the rest is missing.
const KNOWN: i32 = 18_000;

fn classname(id: i32) -> String {
    if id % 5 == 0 {
        format!("furni_line{}_item{id}*{}", id % 40, id % 9 + 1)
    } else {
        format!("furni_line{}_item{id}", id % 40)
    }
}

fn kind_of(class: i32) -> &'static str {
    if class <= FLOOR {
        "s"
    } else {
        "i"
    }
}

fn index_packet() -> Packet {
    let mut w = Writer::new(INDEX)
        .bool(true)
        .i32(0)
        .i32(-1)
        .i32(-1)
        .string("root")
        .string("")
        .i32(0)
        .i32(ROOTS);
    for r in 0..ROOTS {
        let id = r + 1;
        w = w
            .bool(true)
            .i32(id % 200 + 1)
            .i32(id)
            .i32(-1)
            .string(&format!("category_{id}"))
            .string(&format!("Category number {id}"))
            .i32(0)
            .i32(CHILDREN);
        for c in 0..CHILDREN {
            let child = ROOTS + r * CHILDREN + c + 1;
            w = w
                .bool(c % 17 != 0)
                .i32(child % 400 + 1)
                .i32(child)
                .i32(id)
                .string(&format!("page_{child}_set"))
                .string(&format!("Furni set number {child}"))
                .i32(OFFERS_PER_PAGE);
            for k in 0..OFFERS_PER_PAGE {
                w = w.i32(child * 10 + k);
            }
            w = w.i32(0);
        }
    }
    w.bool(false).string("NORMAL").packet()
}

fn page_packet(id: i32) -> Packet {
    let mut w = Writer::new(PAGE)
        .i32(id)
        .string("NORMAL")
        .string("default_3x3")
        .i32(2)
        .string(&format!("catalog_header_{id}"))
        .string(&format!("catalog_teaser_{}", id % 300))
        .i32(3)
        .string(&format!(
            "<b>Furni set {id}</b><br>Everything you need to decorate a room in the style of set {id}: \
             chairs, tables, lamps and a few rare pieces for collectors."
        ))
        .string("Click an item to see what it looks like, then buy it with credits or diamonds.")
        .string(&format!("New in set {id}!"));
    let offers = if id <= ROOTS { 0 } else { OFFERS_PER_PAGE };
    w = w.i32(offers);
    for k in 0..offers {
        let offer = id * 10 + k;
        let n = (id - ROOTS - 1) * OFFERS_PER_PAGE + k;
        let class = n % FURNI + 1;
        let name = classname(class).replace('*', "_");
        w = w
            .i32(offer)
            .string(&name)
            .bool(false)
            .i32(3 + n % 23)
            .i32(if n % 11 == 0 { 5 } else { 0 })
            .i32(if n % 11 == 0 { 5 } else { 0 })
            .bool(true);
        let products: Vec<i32> = if n % 7 == 0 {
            vec![class, (class + 101) % FURNI + 1, (class + 202) % FURNI + 1]
        } else {
            vec![class]
        };
        let badge = n % 50 == 0;
        w = w.i32(products.len() as i32 + i32::from(badge));
        for p in &products {
            let limited = n % 97 == 0;
            w = w
                .string(kind_of(*p))
                .i32(*p)
                .string(if kind_of(*p) == "i" { "3" } else { "" })
                .i32(1)
                .bool(limited);
            if limited {
                w = w.i32(500).i32(123);
            }
        }
        if badge {
            w = w.string("b").string(&format!("ACH_Collector{n}"));
        }
        w = w
            .i32(i32::from(n % 13 == 0))
            .bool(true)
            .bool(false)
            .string(&format!("{name}.png"))
            .string("")
            .bool(true);
    }
    w.i32(-1).bool(false).packet()
}

fn furnidata_json() -> Vec<u8> {
    let mut out = String::with_capacity(11 * 1024 * 1024);
    out.push_str("{\"roomitemtypes\":{\"furnitype\":[");
    for id in 1..=FLOOR {
        if id > 1 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"id\":{id},\"classname\":\"{}\",\"revision\":{},\"category\":\"furniture\",\"defaultdir\":0,\"xdim\":{},\"ydim\":{},\
             \"partcolors\":{{\"color\":[\"#FFFFFF\",\"#F7EBBC\",\"#{:06X}\"]}},\"name\":\"Furni number {id}\",\
             \"description\":\"A lovely piece of furniture, number {id} of the test set.\",\"adurl\":\"\",\"offerid\":{},\
             \"buyout\":true,\"rentofferid\":-1,\"rentbuyout\":false,\"bc\":true,\"excludeddynamic\":false,\"customparams\":\"\",\
             \"specialtype\":1,\"canstandon\":false,\"cansiton\":{},\"canlayon\":false,\"furniline\":\"line{}\",\"environment\":\"\",\"rare\":{}}}",
            classname(id),
            50_000 + id,
            id % 3 + 1,
            id % 2 + 1,
            id * 4093 % 0xFF_FFFF,
            id * 10,
            id % 4 == 0,
            id % 40,
            id % 100 == 0,
        );
    }
    out.push_str("]},\"wallitemtypes\":{\"furnitype\":[");
    for id in FLOOR + 1..=FURNI {
        if id > FLOOR + 1 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"id\":{id},\"classname\":\"{}\",\"revision\":{},\"category\":\"wall\",\"name\":\"Wall item {id}\",\
             \"description\":\"Something for the wall, number {id}.\",\"adurl\":\"\",\"offerid\":{},\"buyout\":true,\
             \"rentofferid\":-1,\"rentbuyout\":false,\"bc\":true,\"excludeddynamic\":false,\"specialtype\":1,\
             \"furniline\":\"line{}\",\"environment\":\"\",\"rare\":false}}",
            classname(id),
            50_000 + id,
            id * 10,
            id % 40,
        );
    }
    out.push_str("]}}");
    out.into_bytes()
}

fn productdata_json() -> Vec<u8> {
    let mut out = String::with_capacity(4 * 1024 * 1024);
    out.push_str("{\"productdata\":{\"product\":[");
    for id in 1..=FURNI {
        if id > 1 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"code\":\"{}\",\"name\":\"Furni number {id}\",\"description\":\"A lovely piece of furniture, number {id} of the test set.\"}}",
            classname(id)
        );
    }
    out.push_str("]}}");
    out.into_bytes()
}

fn items_rows() -> Vec<ItemsBaseRow> {
    (1..=KNOWN)
        .map(|id| ItemsBaseRow {
            id: id as u32 + 1000,
            sprite_id: id + 7,
            item_type: kind_of(id).into(),
            item_name: classname(id),
            effect_id: 0,
        })
        .chain((0..2_000).map(|n| ItemsBaseRow {
            id: 100_000 + n,
            sprite_id: 0,
            item_type: "b".into(),
            item_name: format!("ACH_Collector{}", n * 50),
            effect_id: 0,
        }))
        .collect()
}

// ------------------------------------------------------------------ I/O

/// The official hotel's furnidata and productdata, images of a few KB; nothing kept but what
/// was asked last.
#[derive(Default)]
struct BigFetch {
    last: Option<String>,
}

impl Fetch for BigFetch {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, FetchError> {
        self.last = Some(url.rsplit('/').nth(1).unwrap_or(url).to_string());
        // An HTTP response is a buffer of its size: what making these takes is not the job's.
        let response = |make: fn() -> Vec<u8>| {
            let before = peak();
            let mut made = make();
            let bytes = std::mem::take(&mut made).into_boxed_slice().into_vec();
            drop(made);
            PEAK.store(before.max(live()), Ordering::Relaxed);
            Ok(bytes)
        };
        if url.contains("/gamedata/furnidata_json/") {
            response(furnidata_json)
        } else if url.contains("/gamedata/productdata_json/") {
            response(productdata_json)
        } else if url.ends_with(".png") {
            Ok(vec![0x89; 4096])
        } else {
            Err(FetchError::NotFound)
        }
    }
}

/// The simulated G-Earth, read in pieces of at most what the job asks for (as over TCP: the
/// catalogue index of a big hotel is larger than one read).
struct Chunked {
    sim: SimGEarth,
    pending: Vec<u8>,
}

impl Transport for Chunked {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.sim.send(data)
    }

    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        if self.pending.is_empty() {
            match self.sim.recv(usize::MAX, timeout_ms)? {
                Some(bytes) => self.pending = bytes,
                None => return Ok(None),
            }
        }
        let n = max.min(self.pending.len());
        let rest = self.pending.split_off(n);
        Ok(Some(std::mem::replace(&mut self.pending, rest)))
    }
}

/// Counts what was written.
#[derive(Default)]
struct CountingSink {
    files: Vec<(String, usize)>,
}

impl Sink for CountingSink {
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        if !path.contains("/images/") {
            self.files.push((path.to_string(), data.len()));
        }
        Ok(())
    }
}

/// What a step did and took.
struct StepStat {
    phase: Phase,
    /// The files it wrote or what it downloaded.
    what: String,
    peak: usize,
    time: Duration,
}

#[test]
#[ignore = "slow in a debug build; run with --release (see the module docs)"]
fn habbo_sized_rip_fits_in_the_plugins_memory() {
    let base = live();
    let clock = SimClock::default();
    let hotel = SimHotel {
        index: Box::new(|mode| (mode == "NORMAL").then(index_packet)),
        page: Box::new(|id, mode| (mode == "NORMAL" && (1..=PAGES).contains(&id)).then(|| page_packet(id))),
        ..official_hotel()
    };
    let mut sim = Some(Chunked {
        sim: SimGEarth::new(clock.clone(), hotel),
        pending: Vec::new(),
    });
    let mut items = Some({
        let rows = items_rows();
        items_base(&rows)
    });
    let items_heap = live() - base;
    let mut job = Job::new(RipOptions {
        delay_ms: 50,
        timeout_ms: 2_000,
        hotel_wait_ms: 60_000,
        resend_every_ms: 5_000,
        ..RipOptions::default()
    });
    let mut fetch = BigFetch::default();
    let mut sink = CountingSink::default();
    let mut steps: Vec<StepStat> = Vec::new();
    let mut overall = 0;
    let end = loop {
        let files_before = sink.files.len();
        fetch.last = None;
        let mut closed = Closed;
        let transport: &mut dyn Transport = match sim.as_mut() {
            Some(t) => t,
            None => &mut closed,
        };
        let mut io = JobIo {
            transport,
            fetch: &mut fetch,
            sink: &mut sink,
            clock: &clock,
            items: items.as_ref(),
            existing: None,
        };
        let phase = job.progress().phase;
        reset_peak();
        let started = Instant::now();
        let step = job.step(&mut io, 200);
        let time = started.elapsed();
        let step_peak = peak() - base;
        overall = overall.max(step_peak);
        let mut what: Vec<String> = sink.files[files_before..]
            .iter()
            .map(|(name, _)| name.rsplit('/').next().unwrap_or(name).to_string())
            .collect();
        what.extend(fetch.last.take().filter(|_| phase != Phase::Images));
        steps.push(StepStat {
            phase,
            what: what.join(", "),
            peak: step_peak,
            time,
        });
        job.take_log();
        job.take_events();
        // As the plugin does: G-Earth is closed, and items_base dropped, once the job is done
        // with them.
        if !job.uses_gearth() {
            sim = None;
        }
        if !job.uses_items_base() {
            items = None;
        }
        match step {
            Step::Done(summary) => break summary,
            Step::Failed(failure) => panic!("{}", failure.message),
            _ => {}
        }
        assert!(steps.len() < 2_000_000, "the job never ends");
    };

    assert_eq!(end.pages, PAGES as usize);
    assert_eq!(end.items, ((PAGES - ROOTS) * OFFERS_PER_PAGE) as usize);
    assert!(end.furnidata_names >= FURNI as usize);
    assert!(!end.missing_furni.is_empty());

    println!("steps: {}", steps.len());
    println!(
        "items_base: {:.1} MiB; peak live heap of the rip (with items_base): {:.1} MiB",
        mib(items_heap),
        mib(overall)
    );
    for (name, len) in &sink.files {
        println!("  {name}: {:.2} MiB", mib(*len));
    }
    let mut by_peak: Vec<&StepStat> = steps.iter().collect();
    by_peak.sort_by_key(|s| std::cmp::Reverse(s.peak));
    for s in by_peak.iter().take(12) {
        println!("  peak {:.1} MiB in a {:?} step [{}] ({:?})", mib(s.peak), s.phase, s.what, s.time);
    }
    let mut by_time: Vec<&StepStat> = steps.iter().collect();
    by_time.sort_by_key(|s| std::cmp::Reverse(s.time));
    for s in by_time.iter().take(8) {
        println!("  {:?} in a {:?} step [{}] (peak {:.1} MiB)", s.time, s.phase, s.what, mib(s.peak));
    }
    let slowest = by_time[0];
    // Every step but the one that writes catalog.json stays under 40 MiB.
    let catalog_json = sink
        .files
        .iter()
        .find(|(name, _)| name.ends_with("/catalog.json"))
        .map(|(_, len)| *len)
        .expect("catalog.json");
    for s in &steps {
        if s.what == "catalog.json" {
            // That file is one buffer of its whole size (the sink writes whole files), next to
            // the catalogue it is made from: not more than that.
            assert!(
                s.peak <= catalog_json + 24 * 1024 * 1024,
                "the catalog.json step peaks at {:.1} MiB for a {:.1} MiB file",
                mib(s.peak),
                mib(catalog_json)
            );
        } else {
            assert!(
                s.peak <= 40 * 1024 * 1024,
                "peak live heap {:.1} MiB > 40 MiB in a {:?} step [{}]",
                mib(s.peak),
                s.phase,
                s.what
            );
        }
    }
    assert!(
        slowest.time < Duration::from_millis(800),
        "slowest step {:?} ({:?})",
        slowest.time,
        slowest.phase
    );
}
