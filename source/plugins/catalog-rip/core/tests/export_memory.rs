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
//! catalog.json is ~40 MiB at this size (pretty-printed, byte for byte the built-in's): like
//! every data file it is streamed to the sink in pieces of 1 MiB (`Sink::append`, the picked
//! folder's `append`), so no step holds a whole file. The packets are recorded (`packets/`, a
//! file per page).
//!
//! `diff_memory_at_habbo_size` then stops a second rip after 2,000 pages and goes on with it
//! from its recording ([`Job::resume`]), compared with the first rip (its 40 MiB catalog.json
//! read in pieces into the rows the comparison needs) after the hotel changed some prices.
//!
//! A counting global allocator, so this file is its own test binary (its tests take turns).
//! What the test itself keeps (the files of the earlier rips, read back through a `Source`) is
//! not counted. Slow in a debug build: run it with
//! `cargo test -p catalog-rip-core --release --test export_memory -- --ignored --nocapture`.

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use catalog_rip_core::job::{
    Fetch, FetchError, ImportLog, Job, JobIo, Phase, RipOptions, Sink, Step, Summary,
};
use catalog_rip_core::output::items_base;
use catalog_rip_core::source::{MemSource, Source};
use catalog_rip_core::transport::{Closed, Transport};
use catalog_rip_core::wire::{Packet, Writer};
use catalog_rip_core::ItemsBaseRow;
use support::sim::*;
use support::{INDEX, PAGE};

// ------------------------------------------------------------------ the counting allocator

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// Set while the test handles what it keeps (not counted: see `hidden`). (The lint does not see
// the `const` initializer it asks for.)
#[allow(clippy::missing_const_for_thread_local)]
mod hidden_flag {
    use std::cell::Cell;
    thread_local!(pub static HIDDEN: Cell<bool> = const { Cell::new(false) });
}
use hidden_flag::HIDDEN;

fn counted() -> bool {
    !HIDDEN.with(Cell::get)
}

/// Runs `f` without counting what it allocates or frees: only for what the test keeps (made,
/// grown and dropped in here), never for what the job sees.
fn hidden<R>(f: impl FnOnce() -> R) -> R {
    HIDDEN.with(|h| h.set(true));
    let r = f();
    HIDDEN.with(|h| h.set(false));
    r
}

/// The tests take turns (one counter).
static TURN: Mutex<()> = Mutex::new(());

fn grew(bytes: usize) {
    let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() && counted() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() && counted() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        if counted() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !counted() {
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
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

/// How the hotel's catalogue changed since the earlier rip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    None,
    /// Every 50th offer costs a credit more.
    SomePrices,
    /// Every offer costs a credit more.
    AllPrices,
    /// Every offer has another id.
    NewIds,
}

/// A page of the hotel after `change`.
fn page_packet_priced(id: i32, change: Change) -> Packet {
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
        let offer = id * 10 + k + if change == Change::NewIds { 1_000_000 } else { 0 };
        let n = (id - ROOTS - 1) * OFFERS_PER_PAGE + k;
        let class = n % FURNI + 1;
        let name = classname(class).replace('*', "_");
        w = w
            .i32(offer)
            .string(&name)
            .bool(false)
            .i32(
                3 + n % 23
                    + i32::from(
                        change == Change::AllPrices || (change == Change::SomePrices && n % 50 == 0),
                    ),
            )
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

/// Counts what was written (a streamed file once, with its whole size), and keeps the files
/// `keep` names (not counted: the test's, not the job's).
#[derive(Default)]
struct CountingSink {
    files: Vec<(String, usize)>,
    keep: Option<fn(&str) -> bool>,
    kept: BTreeMap<String, Vec<u8>>,
}

impl CountingSink {
    fn keeping(keep: fn(&str) -> bool) -> CountingSink {
        CountingSink {
            keep: Some(keep),
            ..CountingSink::default()
        }
    }

    fn put(&mut self, path: &str, data: &[u8], replace: bool) {
        if self.keep.is_some_and(|keep| keep(path)) {
            hidden(|| {
                let file = self.kept.entry(path.to_string()).or_default();
                if replace {
                    file.clear();
                }
                file.extend_from_slice(data);
            });
        }
    }

    /// The kept files as a source (moved, not counted).
    fn into_source(mut self, into: &mut MemSource) {
        hidden(|| {
            into.files.append(&mut self.kept);
            drop(self);
        });
    }
}

impl Sink for CountingSink {
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        if !path.contains("/images/") && !path.contains("/packets/") {
            self.files.push((path.to_string(), data.len()));
        }
        self.put(path, data, true);
        Ok(())
    }

    fn append(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        if !path.contains("/packets/") {
            match self.files.last_mut() {
                Some((last, len)) if last == path => *len += data.len(),
                _ => self.files.push((path.to_string(), data.len())),
            }
        }
        self.put(path, data, false);
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

/// The Habbo-sized test hotel after `change`; `end_at`: the hotel connection ends at that page
/// request.
fn habbo(change: Change, end_at: Option<usize>) -> SimHotel {
    SimHotel {
        index: Box::new(|mode| (mode == "NORMAL").then(index_packet)),
        page: Box::new(move |id, mode| {
            (mode == "NORMAL" && (1..=PAGES).contains(&id)).then(|| page_packet_priced(id, change))
        }),
        end_at_page_request: end_at,
        ..official_hotel()
    }
}

fn habbo_options() -> RipOptions {
    RipOptions {
        delay_ms: 50,
        timeout_ms: 2_000,
        hotel_wait_ms: 60_000,
        resend_every_ms: 5_000,
        ..RipOptions::default()
    }
}

/// Steps `job` to its end as the plugin does (budget 200 per tick, G-Earth closed and items_base
/// dropped once the job is done with them), measuring every step: the end and the steps.
fn measure(
    mut job: Job,
    hotel: SimHotel,
    start_ms: u64,
    sink: &mut CountingSink,
    mut source: Option<&mut MemSource>,
) -> (Step, Vec<StepStat>) {
    let base = live();
    let clock = SimClock::default();
    clock.0.set(start_ms);
    let mut sim = Some(Chunked {
        sim: SimGEarth::new(clock.clone(), hotel),
        pending: Vec::new(),
    });
    let mut items = Some({
        let rows = items_rows();
        items_base(&rows)
    });
    let mut fetch = BigFetch::default();
    let mut steps: Vec<StepStat> = Vec::new();
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
            sink: &mut *sink,
            clock: &clock,
            items: items.as_ref(),
            existing: None,
            source: match source.as_mut() {
                Some(s) => Some(&mut **s as &mut dyn Source),
                None => None,
            },
        };
        let phase = job.progress().phase;
        reset_peak();
        let started = Instant::now();
        let step = job.step(&mut io, 200);
        let time = started.elapsed();
        let step_peak = peak().saturating_sub(base);
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
        if !job.uses_gearth() {
            sim = None;
        }
        if !job.uses_items_base() {
            items = None;
        }
        if matches!(step, Step::Done(_) | Step::Failed(_)) {
            break step;
        }
        assert!(steps.len() < 2_000_000, "the job never ends");
    };
    (end, steps)
}

/// Prints the steps that took the most memory and time, and checks every step's.
fn report(name: &str, steps: &[StepStat]) {
    let overall = steps.iter().map(|s| s.peak).max().unwrap_or(0);
    println!(
        "{name}: {} steps; peak live heap {:.1} MiB",
        steps.len(),
        mib(overall)
    );
    let mut by_peak: Vec<&StepStat> = steps.iter().collect();
    by_peak.sort_by_key(|s| std::cmp::Reverse(s.peak));
    for s in by_peak.iter().take(8) {
        println!("  peak {:.1} MiB in a {:?} step [{}] ({:?})", mib(s.peak), s.phase, s.what, s.time);
    }
    let mut by_time: Vec<&StepStat> = steps.iter().collect();
    by_time.sort_by_key(|s| std::cmp::Reverse(s.time));
    for s in by_time.iter().take(5) {
        println!("  {:?} in a {:?} step [{}] (peak {:.1} MiB)", s.time, s.phase, s.what, mib(s.peak));
    }
    // Every step, the data files' included, stays under 40 MiB.
    for s in steps {
        assert!(
            s.peak <= 40 * 1024 * 1024,
            "{name}: peak live heap {:.1} MiB > 40 MiB in a {:?} step [{}]",
            mib(s.peak),
            s.phase,
            s.what
        );
    }
    let slowest = by_time[0];
    assert!(
        slowest.time < Duration::from_millis(800),
        "{name}: slowest step {:?} ({:?})",
        slowest.time,
        slowest.phase
    );
}

fn summary(step: Step) -> Summary {
    match step {
        Step::Done(summary) => summary,
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "slow in a debug build; run with --release (see the module docs)"]
fn habbo_sized_rip_fits_in_the_plugins_memory() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let items_heap = {
        let rows = items_rows();
        let before = live();
        let items = items_base(&rows);
        let heap = live() - before;
        drop(items);
        heap
    };
    let mut sink = CountingSink::default();
    let (end, steps) = measure(
        Job::new(habbo_options()),
        habbo(Change::None, None),
        0,
        &mut sink,
        None,
    );
    let end = summary(end);
    assert_eq!(end.pages, PAGES as usize);
    assert_eq!(end.items, ((PAGES - ROOTS) * OFFERS_PER_PAGE) as usize);
    assert!(end.furnidata_names >= FURNI as usize);
    assert!(!end.missing_furni.is_empty());
    assert!(end.files.contains(&"catalog.json".to_string()));

    println!("items_base: {:.1} MiB", mib(items_heap));
    for (name, len) in &sink.files {
        println!("  {name}: {:.2} MiB", mib(*len));
    }
    report("rip", &steps);
    // catalog.json is bigger than any step's heap may be: it is streamed.
    let catalog_json = sink
        .files
        .iter()
        .find(|(name, _)| name.ends_with("/catalog.json"))
        .map(|(_, len)| *len)
        .expect("catalog.json");
    assert!(catalog_json > 30 * 1024 * 1024, "catalog.json is {:.1} MiB", mib(catalog_json));
}

/// The earlier rip of the diff and re-export cases: a whole rip whose catalog.json and packets/
/// are kept (not counted) in `source`. Returns its folder.
fn earlier_rip(source: &mut MemSource) -> String {
    let mut sink = CountingSink::keeping(|path| {
        path.ends_with("/catalog.json") || path.contains("/packets/")
    });
    let (end, _) = measure(
        Job::new(habbo_options()),
        habbo(Change::None, None),
        0,
        &mut sink,
        None,
    );
    let earlier = summary(end).folder;
    sink.into_source(source);
    assert!(source.files[&format!("{earlier}/catalog.json")].len() > 30 * 1024 * 1024);
    earlier
}

/// A rip of the hotel after `change`, compared with `earlier` (in `source`).
fn compared(
    name: &str,
    change: Change,
    earlier: &str,
    start_ms: u64,
    source: &mut MemSource,
) -> Summary {
    let options = RipOptions {
        diff_against: Some(earlier.to_string()),
        ..habbo_options()
    };
    let mut sink = CountingSink::default();
    let (end, steps) = measure(
        Job::new(options),
        habbo(change, None),
        start_ms,
        &mut sink,
        Some(source),
    );
    for (file, len) in sink.files.iter().filter(|(f, _)| f.contains("changes")) {
        println!("  {file}: {:.2} MiB", mib(*len));
    }
    report(name, &steps);
    summary(end)
}

/// A rip stopped after 2,000 pages goes on from its recording (copied into the new rip folder,
/// the rest asked) and is compared with an earlier whole rip (its catalog.json streamed into the
/// rows of the comparison) after the hotel changed some prices; then the worst cases of the
/// comparison: every price changed, and every offer under a new id (all removed and added).
/// Every step within 40 MiB.
#[test]
#[ignore = "slow in a debug build; run with --release (see the module docs)"]
fn diff_memory_at_habbo_size() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let mut source = hidden(MemSource::new);
    let earlier = earlier_rip(&mut source);

    // A rip that stops after 2,000 pages: its packets are kept.
    let mut sink = CountingSink::keeping(|path| path.contains("/packets/"));
    let (end, steps) = measure(
        Job::new(habbo_options()),
        habbo(Change::SomePrices, Some(2_001)),
        3_600_000,
        &mut sink,
        None,
    );
    let Step::Failed(failure) = end else {
        panic!("{end:?}")
    };
    let stopped = failure.partial.expect("partial").folder;
    report("stopped rip", &steps);
    sink.into_source(&mut source);

    // It goes on, compared with the earlier rip.
    let options = RipOptions {
        diff_against: Some(earlier.clone()),
        ..habbo_options()
    };
    let mut sink = CountingSink::default();
    let (end, steps) = measure(
        Job::resume(options, &stopped),
        habbo(Change::SomePrices, None),
        7_200_000,
        &mut sink,
        Some(&mut source),
    );
    let end = summary(end);
    for (name, len) in &sink.files {
        println!("  {name}: {:.2} MiB", mib(*len));
    }
    report("resumed and compared", &steps);
    assert!(end.complete);
    assert_eq!(end.pages, PAGES as usize);
    assert_eq!(end.resumed_pages, 2_000);
    let changes = end.changes.expect("compared");
    assert_eq!(changes.since, earlier);
    // Every 50th offer that is a row of catalog_manager.sql (its products are in items_base).
    assert!(changes.changed() > 100, "{}", changes.summary());
    assert_eq!(changes.added() + changes.removed(), 0, "{}", changes.summary());
    assert!(sink
        .files
        .iter()
        .any(|(name, _)| name.ends_with("/catalog_manager_changes.sql")));

    let all = compared("every price changed", Change::AllPrices, &earlier, 10_800_000, &mut source);
    let all = all.changes.expect("compared");
    println!("  {}", all.summary());
    assert!(all.changed() > 15_000, "{}", all.summary());

    let ids = compared("every offer under a new id", Change::NewIds, &earlier, 14_400_000, &mut source);
    let ids = ids.changes.expect("compared");
    println!("  {}", ids.summary());
    assert!(ids.added() > 15_000 && ids.removed() > 15_000, "{}", ids.summary());

    println!(
        "earlier catalog.json read in {} reads of at most {:.1} MiB",
        source.reads,
        mib(source.largest_read as usize)
    );
    assert!(source.largest_read <= 4 * 1024 * 1024);
    hidden(|| drop(source));
}

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

/// A recorded Habbo-sized rip written again without the hotel, and the same catalogue imported
/// from a G-Earth Packet Logger export of someone opening every page: every step within 40 MiB.
#[test]
#[ignore = "slow in a debug build; run with --release (see the module docs)"]
fn reexport_and_import_memory_at_habbo_size() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let mut source = hidden(MemSource::new);
    let earlier = earlier_rip(&mut source);

    let options = RipOptions {
        diff_with_previous: false,
        ..habbo_options()
    };
    let mut sink = CountingSink::default();
    let (end, steps) = measure(
        Job::reexport(options.clone(), &earlier),
        habbo(Change::None, None),
        3_600_000,
        &mut sink,
        Some(&mut source),
    );
    report("re-export", &steps);
    let end = summary(end);
    assert_eq!(end.pages, PAGES as usize);

    // The log: the index, then every page asked and answered, with a game packet between.
    let log = hidden(|| {
        let mut log = String::new();
        let entry = |log: &mut String, out: bool, p: &Packet| {
            let _ = write!(
                log,
                "{}[{}] -> {}\n--------------------\n",
                if out { "Outgoing" } else { "Incoming" },
                p.header,
                packet_text(p)
            );
        };
        let noise = Writer::new(2000).i32(7).string("hello").packet();
        entry(&mut log, true, &catalog_rip_core::packets::get_catalog_index(support::GET_INDEX, "NORMAL"));
        entry(&mut log, false, &index_packet());
        for id in 1..=PAGES {
            entry(&mut log, true, &catalog_rip_core::packets::get_catalog_page(support::GET_PAGE, id, "NORMAL"));
            entry(&mut log, false, &noise);
            entry(&mut log, false, &page_packet_priced(id, Change::None));
        }
        log.into_bytes()
    });
    println!("Packet Logger export: {:.1} MiB", mib(log.len()));
    hidden(|| source.files.insert("packets.txt".into(), log));
    let mut sink = CountingSink::default();
    let (end, steps) = measure(
        Job::import_log(
            options,
            ImportLog {
                path: "packets.txt".into(),
                host: "game-nl.habbo.com".into(),
                profile: None,
            },
        ),
        habbo(Change::None, None),
        7_200_000,
        &mut sink,
        Some(&mut source),
    );
    report("Packet Logger import", &steps);
    let end = summary(end);
    assert_eq!(end.pages, PAGES as usize);
    assert_eq!(end.items, ((PAGES - ROOTS) * OFFERS_PER_PAGE) as usize);
    hidden(|| drop(source));
}
