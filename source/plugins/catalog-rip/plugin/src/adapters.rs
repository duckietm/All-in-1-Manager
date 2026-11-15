//! The core's I/O traits over the SDK (G-Earth over `tcp::Conn`, HTTP, the picked folder, the clock), and the pure
//! parts around the job: the G-Earth port probing schedule, the items_base reader, the saved hotel profiles and the
//! delay input. The pure parts are tested natively; the SDK calls work only inside the Manager.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aio_plugin_sdk::folders::Folder;
use aio_plugin_sdk::{http, tcp};
use catalog_rip_core::job::{MAX_DELAY_MS, MIN_DELAY_MS};
use catalog_rip_core::profiles::{load_profiles, read_profile_file};
use catalog_rip_core::transport::Transport;
use catalog_rip_core::{Clock, Fetch, FetchError, HotelProfile, ItemsBaseRow, Sink};

// ---------------------------------------------------------------- the job's I/O

/// The G-Earth connection as the core's byte stream.
pub struct ConnTransport<'a>(pub &'a mut tcp::Conn);

impl Transport for ConnTransport<'_> {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.send(data).map_err(|e| e.to_string())
    }

    fn recv(&mut self, max: usize, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        self.0.recv_timeout(max.max(1), Duration::from_millis(timeout_ms.into())).map_err(|e| e.to_string())
    }
}

/// The Manager's HTTP getter (`http:<host>` for official Habbo, `http:ask` for a retro's own hosts).
pub struct HttpFetch;

impl Fetch for HttpFetch {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, FetchError> {
        if !is_http_url(url) {
            return Err(FetchError::Failed(format!(
                "{url}: the plugin can only read http and https addresses, not files"
            )));
        }
        http::get(url).map_err(|e| fetch_error(e.message()))
    }
}

/// `http://` or `https://` (any case). A profile's furnidata may name a local file, which the plugin cannot read.
pub fn is_http_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The SDK's error text as the core's [`FetchError`]: the Manager reports a non-2xx status as `HTTP <code>` (kind
/// `Other`); 403, 404 and 410 mean "the file is not there", as the core expects.
pub fn fetch_error(message: &str) -> FetchError {
    match message.trim() {
        "HTTP 403" | "HTTP 404" | "HTTP 410" => FetchError::NotFound,
        _ => FetchError::Failed(message.to_string()),
    }
}

/// The picked folder as the core's output. The core's paths start with the rip folder's name.
pub struct FolderSink<'a>(pub &'a Folder);

impl Sink for FolderSink<'_> {
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        self.0.write(path, data).map_err(|e| e.to_string())
    }
}

/// WASI's clocks (`Instant` and `SystemTime` work in a `wasm32-wasip2` guest).
pub struct SysClock {
    start: Instant,
}

impl SysClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Clock for SysClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn unix_secs(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

// ---------------------------------------------------------------- G-Earth ports

/// G-Earth's first extension port; it moves up one port for every port already taken.
pub const FIRST_PORT: u16 = 9092;
/// Ports tried: 9092 to 9101 (the manifest's `tcp:localhost:9092-9101`).
pub const PORT_COUNT: u16 = 10;
pub const LAST_PORT: u16 = FIRST_PORT + PORT_COUNT - 1;
/// The page probes at most this often while idle.
pub const PROBE_EVERY_MS: u64 = 2_000;

/// When the idle page looks for G-Earth, and what it found.
///
/// One port per probe: on Windows a refused connection to a local port takes about 2 s, so a sweep of the ten ports
/// in one render would freeze the page for 20 s. The port that answered last is tried first; when it stops
/// answering, the sweep starts again at 9092.
#[derive(Debug, Default, Clone)]
pub struct Probe {
    last_ms: Option<u64>,
    /// The next port of the sweep, as an offset from [`FIRST_PORT`].
    next: u16,
    found: Option<u16>,
    /// Ports that did not answer since the last that did.
    misses: u16,
    last_tried: Option<u16>,
}

impl Probe {
    /// The port to try at `now_ms`, or `None` when the last probe was less than [`PROBE_EVERY_MS`] ago.
    pub fn due(&self, now_ms: u64) -> Option<u16> {
        if let Some(last) = self.last_ms {
            if now_ms.saturating_sub(last) < PROBE_EVERY_MS {
                return None;
            }
        }
        Some(self.found.unwrap_or(FIRST_PORT + self.next))
    }

    /// What the probe of `port` at `now_ms` gave.
    pub fn record(&mut self, now_ms: u64, port: u16, answered: bool) {
        self.last_ms = Some(now_ms);
        self.last_tried = Some(port);
        if answered {
            self.found = Some(port);
            self.misses = 0;
            return;
        }
        if self.found == Some(port) {
            self.found = None;
            self.next = 0;
        } else {
            self.next = (self.next + 1) % PORT_COUNT;
        }
        self.misses = self.misses.saturating_add(1);
    }

    /// Probe at the next chance (the user asked to check again).
    pub fn now(&mut self) {
        self.last_ms = None;
    }

    pub fn found(&self) -> Option<u16> {
        self.found
    }

    pub fn last_tried(&self) -> Option<u16> {
        self.last_tried
    }

    /// Every port was tried since the last one that answered.
    pub fn none_answered(&self) -> bool {
        self.found.is_none() && self.misses >= PORT_COUNT
    }

    /// The ports to try when a rip starts, first to last: the one that answered, then 9092 to 9101.
    pub fn connect_order(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.found.into_iter().collect();
        ports.extend((FIRST_PORT..=LAST_PORT).filter(|p| Some(*p) != self.found));
        ports
    }
}

// ---------------------------------------------------------------- items_base

/// Most rows one read returns (the Manager's limit).
pub const SQL_LIMIT: usize = 10_000;
/// The first id window. Ids are unique, so a window this wide never holds more rows than one read returns.
const WINDOW: i64 = SQL_LIMIT as i64;
const MAX_WINDOW: i64 = 1 << 40;

/// Why a read of items_base failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    /// The plugin may not read it (no `sql:read:items_base`): no point in asking again.
    pub denied: bool,
    pub message: String,
}

/// The reads the loader needs.
pub trait ItemsSource {
    /// The rows with `from <= id < to` (at most [`SQL_LIMIT`]), with `effect_id_male` when `effect`. A row that
    /// cannot be read is an `Err` with the reason.
    fn window(&mut self, from: i64, to: i64, effect: bool) -> Result<Vec<Result<ItemsBaseRow, String>>, SourceError>;
    /// Whether any row has `id >= from`.
    fn any_from(&mut self, from: i64) -> Result<bool, SourceError>;
}

/// What one [`ItemsLoader::step`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    More,
    Done,
    /// The database cannot be read (no server, no permission, ...): rip without items_base.
    Unavailable(String),
}

/// Reads the whole items_base, one query per step: a read returns at most 10,000 rows and has no order, so the
/// ids are read in windows (`from <= id < to`) that grow while they are sparse and shrink when one is full.
#[derive(Debug, Clone)]
pub struct ItemsLoader {
    from: i64,
    width: i64,
    effect: bool,
    /// The read with `effect_id_male` failed with this; said once a read without it works.
    effect_error: Option<String>,
    check_rest: bool,
    pub rows: Vec<ItemsBaseRow>,
    /// Rows that could not be read, and the first reason.
    pub skipped: usize,
    first_skip: Option<String>,
    /// Lines for the log since the last call of [`ItemsLoader::take_notes`].
    notes: Vec<String>,
}

impl Default for ItemsLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemsLoader {
    pub fn new() -> Self {
        Self {
            from: 0,
            width: WINDOW,
            effect: true,
            effect_error: None,
            check_rest: false,
            rows: Vec::new(),
            skipped: 0,
            first_skip: None,
            notes: Vec::new(),
        }
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// One read.
    pub fn step(&mut self, source: &mut dyn ItemsSource) -> Load {
        if self.check_rest {
            return match source.any_from(self.from) {
                Ok(false) => self.done(),
                Ok(true) => {
                    self.check_rest = false;
                    self.width = self.width.saturating_mul(8).min(MAX_WINDOW);
                    Load::More
                }
                Err(e) => Load::Unavailable(e.message),
            };
        }
        let to = self.from.saturating_add(self.width);
        let rows = match source.window(self.from, to, self.effect) {
            Ok(rows) => rows,
            // Like the built-in: an items_base without effect_id_male is read without it (effect ids 0).
            Err(e) if self.effect && !e.denied => {
                self.effect = false;
                self.effect_error = Some(e.message);
                return Load::More;
            }
            Err(e) => return Load::Unavailable(e.message),
        };
        if let Some(why) = self.effect_error.take() {
            self.notes.push(format!("items_base was read without effect_id_male ({why}): effect ids are 0."));
        }
        if rows.len() >= SQL_LIMIT && self.width > WINDOW {
            self.width = (self.width / 2).max(WINDOW);
            return Load::More;
        }
        if rows.len() >= SQL_LIMIT {
            self.notes.push(format!(
                "items_base has {SQL_LIMIT} rows with ids from {} to {}: ids repeat, so some rows may be missing.",
                self.from,
                to - 1
            ));
        }
        let count = rows.len();
        for row in rows {
            match row {
                Ok(row) => self.rows.push(row),
                Err(why) => {
                    self.skipped += 1;
                    self.first_skip.get_or_insert(why);
                }
            }
        }
        self.from = to;
        if to == i64::MAX {
            return self.done();
        }
        if count == 0 {
            self.check_rest = true;
        } else if count < SQL_LIMIT / 4 {
            self.width = self.width.saturating_mul(2).min(MAX_WINDOW);
        }
        Load::More
    }

    fn done(&mut self) -> Load {
        if let Some(why) = self.first_skip.take() {
            self.notes.push(format!("{} items_base rows could not be read and were left out (first: {why}).", self.skipped));
        }
        Load::Done
    }
}

// ---------------------------------------------------------------- saved hotel profiles

/// The private file with the user's hotel profiles (a JSON array; learned profiles are added to it).
pub const PROFILES_FILE: &str = "profiles.json";

/// The built-in profiles merged with the saved ones (`text`: the content of [`PROFILES_FILE`], if any), and the
/// reasons the file could not be read.
pub fn all_profiles(text: Option<&str>) -> (Vec<HotelProfile>, Vec<String>) {
    match text {
        Some(text) => load_profiles(&[(PROFILES_FILE.to_string(), text.to_string())]),
        None => load_profiles(&[]),
    }
}

/// [`PROFILES_FILE`]'s new text with the profile `(file_name, json)` (a job's learned profile) added; a saved
/// profile with the same name (any case) is replaced. An unreadable file is an error and stays as it is.
pub fn add_profile(existing: Option<&str>, file_name: &str, json: &str) -> Result<String, String> {
    let mut list = match existing {
        Some(text) if !text.trim().is_empty() => read_profile_file(PROFILES_FILE, text)
            .map_err(|e| format!("{PROFILES_FILE} cannot be read ({e:#}), so it was not changed"))?,
        _ => Vec::new(),
    };
    let new = read_profile_file(file_name, json).map_err(|e| format!("the learned profile cannot be read ({e:#})"))?;
    for profile in new {
        match list.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&profile.name)) {
            Some(old) => *old = profile,
            None => list.push(profile),
        }
    }
    serde_json::to_string_pretty(&list).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- settings

/// The delay input's text as milliseconds between two requests.
pub fn parse_delay(text: &str) -> Result<u64, String> {
    let range = format!("a number of milliseconds from {MIN_DELAY_MS} to {MAX_DELAY_MS}");
    match text.trim().parse::<u64>() {
        Ok(ms) if (MIN_DELAY_MS..=MAX_DELAY_MS).contains(&ms) => Ok(ms),
        _ => Err(format!("The delay must be {range}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn missing_files_are_not_found_and_other_errors_fail() {
        for text in ["HTTP 403", "HTTP 404", "HTTP 410"] {
            assert_eq!(fetch_error(text), FetchError::NotFound, "{text}");
        }
        for text in ["HTTP 500", "HTTP 401", "the request timed out after 60 s", "permission denied: host x was refused"] {
            assert_eq!(fetch_error(text), FetchError::Failed(text.to_string()), "{text}");
        }
        assert!(is_http_url("https://www.habbo.nl/gamedata/furnidata_json/1"));
        assert!(is_http_url("HTTP://hotel.example/x"));
        assert!(!is_http_url("C:/furnidata.xml"));
        assert!(!is_http_url("furnidata.json"));
    }

    #[test]
    fn probing_waits_two_seconds_between_ports() {
        let mut p = Probe::default();
        assert_eq!(p.due(0), Some(9092));
        p.record(0, 9092, false);
        assert_eq!(p.due(1_999), None);
        assert_eq!(p.due(2_000), Some(9093));
        p.record(2_000, 9093, true);
        assert_eq!(p.found(), Some(9093));
        // The port that answered is checked again, not the next of the sweep.
        assert_eq!(p.due(4_000), Some(9093));
        p.record(4_000, 9093, false);
        assert_eq!(p.found(), None);
        assert_eq!(p.due(6_000), Some(9092), "the sweep starts over");
        p.now();
        assert_eq!(p.due(6_001), Some(9092), "Check again does not wait");
    }

    #[test]
    fn a_whole_sweep_without_answer_says_none_answered() {
        let mut p = Probe::default();
        let mut now = 0;
        for i in 0..PORT_COUNT {
            assert!(!p.none_answered());
            let port = p.due(now).unwrap();
            assert_eq!(port, FIRST_PORT + i);
            p.record(now, port, false);
            now += PROBE_EVERY_MS;
        }
        assert!(p.none_answered());
        assert_eq!(p.due(now), Some(9092), "the sweep goes round");
        assert_eq!(p.connect_order(), (9092..=9101).collect::<Vec<_>>());
        p.record(now, 9092, false);
        p.record(now + PROBE_EVERY_MS, 9093, false);
        p.record(now + 2 * PROBE_EVERY_MS, 9094, true);
        assert!(!p.none_answered());
        assert_eq!(p.connect_order(), vec![9094, 9092, 9093, 9095, 9096, 9097, 9098, 9099, 9100, 9101]);
    }

    fn row(id: i64) -> ItemsBaseRow {
        ItemsBaseRow {
            id: id as u32,
            sprite_id: id as i32,
            item_type: "s".into(),
            item_name: format!("item_{id}"),
            effect_id: 0,
        }
    }

    /// items_base in memory; `no_effect`: the table has no effect_id_male column.
    struct Table {
        ids: Vec<i64>,
        no_effect: bool,
        down: Option<SourceError>,
        reads: usize,
    }

    impl Table {
        fn new(ids: impl IntoIterator<Item = i64>) -> Self {
            Self { ids: ids.into_iter().collect(), no_effect: false, down: None, reads: 0 }
        }
    }

    impl ItemsSource for Table {
        fn window(&mut self, from: i64, to: i64, effect: bool) -> Result<Vec<Result<ItemsBaseRow, String>>, SourceError> {
            self.reads += 1;
            if let Some(e) = &self.down {
                return Err(e.clone());
            }
            if effect && self.no_effect {
                return Err(SourceError { denied: false, message: "Unknown column 'effect_id_male'".into() });
            }
            // No order, and at most SQL_LIMIT rows: the newest first, so a full window shows.
            Ok(self.ids.iter().rev().filter(|id| (from..to).contains(*id)).take(SQL_LIMIT).map(|id| Ok(row(*id))).collect())
        }

        fn any_from(&mut self, from: i64) -> Result<bool, SourceError> {
            self.reads += 1;
            Ok(self.ids.iter().any(|id| *id >= from))
        }
    }

    fn load(table: &mut Table) -> (Load, ItemsLoader) {
        let mut loader = ItemsLoader::new();
        for _ in 0..10_000 {
            match loader.step(table) {
                Load::More => {}
                end => return (end, loader),
            }
        }
        panic!("the loader never ends");
    }

    #[test]
    fn items_base_is_read_whole_in_windows() {
        // Dense ids, a gap, then custom furni far above.
        let ids: Vec<i64> = (1..=25_000).chain(40_000..41_000).chain(5_000_000..5_000_010).collect();
        let mut table = Table::new(ids.clone());
        let (end, loader) = load(&mut table);
        assert_eq!(end, Load::Done);
        let mut got: Vec<i64> = loader.rows.iter().map(|r| i64::from(r.id)).collect();
        got.sort_unstable();
        assert_eq!(got, ids);
        assert!(table.reads < 40, "{} reads", table.reads);
    }

    #[test]
    fn a_full_window_is_read_again_narrower() {
        // 30,000 ids packed into a window that has grown past 10,000 ids wide.
        let ids: Vec<i64> = (1..=1_000).chain(20_000..50_000).collect();
        let mut table = Table::new(ids.clone());
        let (end, loader) = load(&mut table);
        assert_eq!(end, Load::Done);
        assert_eq!(loader.rows.len(), ids.len());
    }

    #[test]
    fn an_empty_table_and_the_column_fallback() {
        let (end, loader) = load(&mut Table::new([]));
        assert_eq!((end, loader.rows.len()), (Load::Done, 0));

        let mut table = Table::new(1..=5);
        table.no_effect = true;
        let (end, mut loader) = load(&mut table);
        assert_eq!((end, loader.rows.len()), (Load::Done, 5));
        let notes = loader.take_notes();
        assert!(notes[0].contains("without effect_id_male"), "{notes:?}");
    }

    #[test]
    fn no_database_means_no_items_base() {
        let mut table = Table::new(1..=5);
        table.down = Some(SourceError { denied: false, message: "Not connected".into() });
        let (end, _) = load(&mut table);
        assert_eq!(end, Load::Unavailable("Not connected".into()));
        assert_eq!(table.reads, 2, "once with effect_id_male, once without");

        let mut table = Table::new(1..=5);
        let denied = "permission denied: table 'items_base' is not declared with sql:read";
        table.down = Some(SourceError { denied: true, message: denied.into() });
        let (end, _) = load(&mut table);
        assert_eq!(end, Load::Unavailable(denied.into()));
        assert_eq!(table.reads, 1, "a refusal is not asked again");
    }

    fn learned(name: &str, get_index: u16) -> (String, String) {
        let mut profile = all_profiles(None).0.into_iter().find(|p| p.name == catalog_rip_core::profiles::NITRO).unwrap();
        profile.name = name.into();
        profile.builtin = false;
        profile.headers.get_catalog_index = Some(get_index);
        catalog_rip_core::save_profile(&profile).unwrap()
    }

    #[test]
    fn learned_profiles_are_saved_and_loaded() {
        let builtins = all_profiles(None).0.len();
        let (file, json) = learned("hotel.example (learned)", 100);
        let text = add_profile(None, &file, &json).unwrap();
        let (all, errors) = all_profiles(Some(&text));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(all.len(), builtins + 1);
        let saved = all.iter().find(|p| p.name == "hotel.example (learned)").unwrap();
        assert!(!saved.builtin);
        assert_eq!(saved.headers.get_catalog_index, Some(100));

        // Learned again (any case): replaced, not added.
        let (file, json) = learned("HOTEL.example (learned)", 200);
        let text = add_profile(Some(&text), &file, &json).unwrap();
        let (file, json) = learned("other.example (learned)", 300);
        let text = add_profile(Some(&text), &file, &json).unwrap();
        let all = all_profiles(Some(&text)).0;
        assert_eq!(all.len(), builtins + 2);
        let by_name: BTreeMap<_, _> = all.iter().map(|p| (p.name.to_ascii_lowercase(), p.headers.get_catalog_index)).collect();
        assert_eq!(by_name["hotel.example (learned)"], Some(200));
        assert_eq!(by_name["other.example (learned)"], Some(300));
    }

    #[test]
    fn a_broken_profiles_file_is_reported_and_kept() {
        let (all, errors) = all_profiles(Some("{ not json"));
        assert_eq!(all.len(), all_profiles(None).0.len());
        assert_eq!(errors.len(), 1);
        let (file, json) = learned("x (learned)", 1);
        let error = add_profile(Some("{ not json"), &file, &json).unwrap_err();
        assert!(error.contains("was not changed"), "{error}");
    }

    #[test]
    fn delay_is_checked() {
        assert_eq!(parse_delay(" 400 "), Ok(400));
        assert_eq!(parse_delay("50"), Ok(50));
        assert_eq!(parse_delay("5000"), Ok(5000));
        for bad in ["49", "5001", "", "fast", "-1"] {
            assert!(parse_delay(bad).is_err(), "{bad}");
        }
    }
}
