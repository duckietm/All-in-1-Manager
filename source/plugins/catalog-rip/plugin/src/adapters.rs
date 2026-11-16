//! The core's I/O traits over the SDK (G-Earth over `tcp::Conn`, HTTP, the picked folder, the clock), and the pure
//! parts around the job: the G-Earth port probing schedule, the items_base reader, the saved hotel profiles and the
//! delay input. The pure parts are tested natively; the SDK calls work only inside the Manager.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aio_plugin_sdk::folders::Folder;
use aio_plugin_sdk::sql::Value;
use aio_plugin_sdk::{http, tcp};
use catalog_rip_core::gearth::ExtensionInfo;
use catalog_rip_core::job::{MAX_DELAY_MS, MIN_DELAY_MS};
use catalog_rip_core::profiles::{load_profiles, read_profile_file};
use catalog_rip_core::transport::Transport;
use catalog_rip_core::{Clock, Failure, Fetch, FetchError, HotelProfile, ItemsBaseRow, Sink, Step};

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

    fn append(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        self.0.append(path, data).map_err(|e| e.to_string())
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
/// in one call would freeze the page for 20 s. The idle page is rendered only after an event, so on its own it
/// probes at most one port per render, at most every [`PROBE_EVERY_MS`], until one whole sweep of 9092-9101 got no
/// answer. [`Probe::start_sweep`] ("Check again", or a rip that ended) runs one whole sweep in ticks, one port per
/// tick ([`Probe::sweep_port`]), and stops at the first port that answers or after the last one ("Rip catalogue"
/// tries every port itself). A port that answered is rechecked (one cheap connect); when it stops answering, one new
/// sweep starts at 9092.
#[derive(Debug, Default, Clone)]
pub struct Probe {
    last_ms: Option<u64>,
    /// The next port of the sweep, as an offset from [`FIRST_PORT`].
    next: u16,
    found: Option<u16>,
    /// Ports of the current sweep that did not answer.
    misses: u16,
    /// A sweep runs in ticks.
    sweeping: bool,
}

impl Probe {
    /// The port a render may try at `now_ms`, or `None`: a sweep runs in ticks, the last probe was less than
    /// [`PROBE_EVERY_MS`] ago, or a whole sweep got no answer.
    pub fn due(&self, now_ms: u64) -> Option<u16> {
        if self.sweeping || self.none_answered() {
            return None;
        }
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
        if answered {
            self.found = Some(port);
            self.misses = 0;
            self.sweeping = false;
            return;
        }
        if self.found == Some(port) {
            // Gone: one new sweep of every port.
            self.found = None;
            self.next = 0;
            self.misses = 0;
        } else {
            self.next = (self.next + 1) % PORT_COUNT;
            self.misses = self.misses.saturating_add(1);
            if self.misses >= PORT_COUNT {
                self.sweeping = false;
            }
        }
    }

    /// One whole sweep in ticks, from 9092 ("Check again", or a rip ended). A found port is rechecked first.
    pub fn start_sweep(&mut self) {
        self.last_ms = None;
        self.sweeping = true;
        if self.found.is_none() {
            self.next = 0;
            self.misses = 0;
        }
    }

    /// The port the next tick of a sweep tries; `None` when no sweep runs.
    pub fn sweep_port(&self) -> Option<u16> {
        self.sweeping.then(|| self.found.unwrap_or(FIRST_PORT + self.next))
    }

    /// The idle page's line about G-Earth.
    pub fn status(&self) -> String {
        let ports = format!("{FIRST_PORT}\u{2013}{LAST_PORT}");
        if let Some(port) = self.sweep_port() {
            return format!("Checking port {port}\u{2026}");
        }
        match self.found {
            Some(port) => format!("Something answers on port {port} (G-Earth?)."),
            None if self.none_answered() => {
                format!("Not found on ports {ports}. Start G-Earth, then click Check again.")
            }
            None if self.misses > 0 => format!(
                "Not found on ports {FIRST_PORT}\u{2013}{} so far. Start G-Earth, then click Check again.",
                FIRST_PORT + self.misses - 1
            ),
            None => format!("Start G-Earth, then click Check again to look for it on ports {ports}."),
        }
    }

    pub fn found(&self) -> Option<u16> {
        self.found
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

/// What a rip does when its job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailure {
    /// The port did not answer as G-Earth: try the next one.
    NextPort,
    /// No port answered as G-Earth.
    NoGearth,
    /// The rip ended.
    Ended,
}

/// What to do when the job failed, with `ports_left` G-Earth ports not tried yet.
pub fn after_failure(failure: &Failure, ports_left: usize) -> AfterFailure {
    if !failure.not_gearth || failure.cancelled {
        AfterFailure::Ended
    } else if ports_left > 0 {
        AfterFailure::NextPort
    } else {
        AfterFailure::NoGearth
    }
}

/// Whether the plugin may drop the hotel's items_base after a step that returned `step`, the job saying
/// `uses_items_base`. Never after a failed job: a job on the next port, or Try again, reuses it (a finished job,
/// failed ones included, says it no longer uses it).
pub fn drop_items_base(step: &Step, uses_items_base: bool) -> bool {
    !uses_items_base && !matches!(step, Step::Failed(_))
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
    /// Reads with `effect_id_male` that failed (two in a row: read without it).
    effect_failures: u8,
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
            effect_failures: 0,
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
            // The read with it is tried twice first, so one passing failure does not drop the column.
            Err(e) if self.effect && !e.denied => {
                self.effect_failures += 1;
                if self.effect_failures >= 2 {
                    self.effect = false;
                    self.effect_error = Some(e.message);
                }
                return Load::More;
            }
            Err(e) => return Load::Unavailable(e.message),
        };
        if self.effect {
            self.effect_failures = 0;
        }
        if let Some(why) = self.effect_error.take() {
            self.notes.push(format!("items_base: the read with effect_id_male failed ({why}); using 0."));
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

/// One items_base row from its columns (`value` gives a column's value, `None` when not read). A row without a
/// usable `id` is an error (left out and counted), never item 0; a missing or NULL `sprite_id` or
/// `effect_id_male` is 0, a missing `type` or `item_name` empty.
pub fn items_row<'a>(value: impl Fn(&str) -> Option<&'a Value>) -> Result<ItemsBaseRow, String> {
    fn int(value: Option<&Value>, column: &str) -> Result<Option<i64>, String> {
        match value {
            Some(Value::Int(v)) => Ok(Some(*v)),
            Some(Value::Text(t)) => t.trim().parse().map(Some).map_err(|_| format!("{column} is not a number: {t}")),
            Some(Value::Null) | None => Ok(None),
            Some(_) => Err(format!("{column} is not a number")),
        }
    }
    fn text(value: Option<&Value>) -> String {
        match value {
            Some(Value::Text(t)) => t.clone(),
            Some(Value::Int(v)) => v.to_string(),
            _ => String::new(),
        }
    }
    let id = int(value("id"), "id")?.ok_or("a row has no id")?;
    let id32 = u32::try_from(id).map_err(|_| format!("id {id} is out of range"))?;
    let sprite_id = int(value("sprite_id"), "sprite_id")?.unwrap_or(0);
    Ok(ItemsBaseRow {
        id: id32,
        sprite_id: i32::try_from(sprite_id).map_err(|_| format!("sprite_id of {id} is out of range"))?,
        item_type: text(value("type")),
        item_name: text(value("item_name")),
        effect_id: int(value("effect_id_male"), "effect_id_male")?.and_then(|e| i32::try_from(e).ok()).unwrap_or(0),
    })
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

// ---------------------------------------------------------------- what the plugin calls itself

/// The plugin's version (the manifest's; a test keeps them equal).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The maker the output files name in their headers.
pub fn generator() -> String {
    format!("Catalogue ripper plugin {VERSION}")
}

/// What G-Earth shows in its Extensions tab: the core's text with the plugin's version.
pub fn extension_info() -> ExtensionInfo {
    ExtensionInfo { version: VERSION.into(), ..ExtensionInfo::default() }
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
        // The port that answered is checked again (cheap), not the next of the sweep.
        assert_eq!(p.due(4_000), Some(9093));
        p.record(4_000, 9093, true);
        assert_eq!(p.due(6_000), Some(9093));
        p.record(6_000, 9093, false);
        assert_eq!(p.found(), None);
        assert_eq!(p.due(8_000), Some(9092), "a new sweep starts at 9092");
    }

    /// Records misses on every port a tick sweep asks for until it asks for none.
    fn tick_sweep(p: &mut Probe, answers: Option<u16>) -> Vec<u16> {
        let mut tried = Vec::new();
        while let Some(port) = p.sweep_port() {
            assert!(tried.len() < 100, "the sweep never stops");
            assert_eq!(p.due(0), None, "renders do not probe while a sweep runs");
            assert_eq!(p.status(), format!("Checking port {port}\u{2026}"));
            p.record(0, port, Some(port) == answers);
            tried.push(port);
        }
        tried
    }

    #[test]
    fn check_again_sweeps_every_port_in_ticks_once() {
        let all: Vec<u16> = (9092..=9101).collect();
        let mut p = Probe::default();
        assert_eq!(p.status(), "Start G-Earth, then click Check again to look for it on ports 9092\u{2013}9101.");
        p.record(0, 9092, false);
        p.record(2_000, 9093, false);
        assert_eq!(p.status(), "Not found on ports 9092\u{2013}9093 so far. Start G-Earth, then click Check again.");

        // Check again: the whole sweep from 9092, one port per tick, without waiting; then it stops.
        p.start_sweep();
        assert_eq!(tick_sweep(&mut p, None), all);
        assert!(p.none_answered());
        assert_eq!(p.sweep_port(), None);
        assert_eq!(p.status(), "Not found on ports 9092\u{2013}9101. Start G-Earth, then click Check again.");

        // It stops at the first port that answers.
        p.start_sweep();
        assert_eq!(tick_sweep(&mut p, Some(9095)), [9092, 9093, 9094, 9095]);
        assert_eq!(p.status(), "Something answers on port 9095 (G-Earth?).");

        // With a port found, a sweep rechecks it; gone, it sweeps every port.
        p.start_sweep();
        assert_eq!(tick_sweep(&mut p, Some(9095)), [9095]);
        p.start_sweep();
        let mut expected = vec![9095];
        expected.extend(&all);
        assert_eq!(tick_sweep(&mut p, None), expected);
    }

    /// Records misses on every port the probe asks for until it asks for none.
    fn sweep_without_answer(p: &mut Probe, now: &mut u64) -> Vec<u16> {
        let mut tried = Vec::new();
        while let Some(port) = p.due(*now) {
            assert!(tried.len() < 100, "the probe never stops");
            p.record(*now, port, false);
            tried.push(port);
            *now += PROBE_EVERY_MS;
        }
        tried
    }

    #[test]
    fn probing_stops_after_one_sweep_without_answer() {
        let mut p = Probe::default();
        let mut now = 0;
        let all: Vec<u16> = (9092..=9101).collect();
        assert_eq!(sweep_without_answer(&mut p, &mut now), all);
        assert!(p.none_answered());
        assert_eq!(p.due(now + 60_000), None, "no more probing on its own");
        assert_eq!(p.connect_order(), all);

        // A found port that stops answering starts one sweep of all ten ports.
        p.start_sweep();
        assert_eq!(p.sweep_port(), Some(9092));
        p.record(now, 9092, false);
        p.record(now + PROBE_EVERY_MS, 9093, false);
        p.record(now + 2 * PROBE_EVERY_MS, 9094, true);
        assert!(!p.none_answered());
        assert_eq!(p.connect_order(), vec![9094, 9092, 9093, 9095, 9096, 9097, 9098, 9099, 9100, 9101]);
        now += 3 * PROBE_EVERY_MS;
        p.record(now, 9094, false);
        now += PROBE_EVERY_MS;
        assert_eq!(sweep_without_answer(&mut p, &mut now), all);
        assert_eq!(p.due(now + 60_000), None);
    }

    #[test]
    fn a_port_that_is_not_gearth_does_not_end_the_rip() {
        let failure = |not_gearth: bool, cancelled: bool| Failure {
            message: "G-Earth did not answer on port 9092 within 3 s".into(),
            cancelled,
            gearth_error: Some(catalog_rip_core::gearth::Error::NoInfoRequest),
            reconnect: false,
            not_gearth,
            partial: None,
        };
        assert_eq!(after_failure(&failure(true, false), 3), AfterFailure::NextPort);
        assert_eq!(after_failure(&failure(true, false), 0), AfterFailure::NoGearth);
        assert_eq!(after_failure(&failure(false, false), 3), AfterFailure::Ended, "G-Earth failed later");
        assert_eq!(after_failure(&failure(true, true), 3), AfterFailure::Ended, "cancelled");
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
        /// The next read fails with this (once).
        fail_once: Option<SourceError>,
        reads: usize,
    }

    impl Table {
        fn new(ids: impl IntoIterator<Item = i64>) -> Self {
            Self { ids: ids.into_iter().collect(), no_effect: false, down: None, fail_once: None, reads: 0 }
        }
    }

    impl ItemsSource for Table {
        fn window(&mut self, from: i64, to: i64, effect: bool) -> Result<Vec<Result<ItemsBaseRow, String>>, SourceError> {
            self.reads += 1;
            if let Some(e) = &self.down {
                return Err(e.clone());
            }
            if let Some(e) = self.fail_once.take() {
                return Err(e);
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
        assert_eq!(
            notes,
            ["items_base: the read with effect_id_male failed (Unknown column 'effect_id_male'); using 0."]
        );
        // Twice with effect_id_male, then without: the first window, the next one, the rest check.
        assert_eq!(table.reads, 5);
    }

    #[test]
    fn one_failed_read_keeps_effect_id_male() {
        let mut table = Table::new(1..=5);
        table.fail_once = Some(SourceError { denied: false, message: "Lost connection".into() });
        let mut loader = ItemsLoader::new();
        assert_eq!(loader.step(&mut table), Load::More);
        assert_eq!(loader.step(&mut table), Load::More);
        assert!(loader.effect, "a read with effect_id_male worked on the retry");
        assert_eq!(loader.rows.len(), 5);
        assert!(loader.take_notes().is_empty());
    }

    #[test]
    fn no_database_means_no_items_base() {
        let mut table = Table::new(1..=5);
        table.down = Some(SourceError { denied: false, message: "Not connected".into() });
        let (end, _) = load(&mut table);
        assert_eq!(end, Load::Unavailable("Not connected".into()));
        assert_eq!(table.reads, 3, "twice with effect_id_male, once without");

        let mut table = Table::new(1..=5);
        let denied = "permission denied: table 'items_base' is not declared with sql:read";
        table.down = Some(SourceError { denied: true, message: denied.into() });
        let (end, _) = load(&mut table);
        assert_eq!(end, Load::Unavailable(denied.into()));
        assert_eq!(table.reads, 1, "a refusal is not asked again");
    }

    #[test]
    fn items_rows_need_an_id() {
        let read = |cols: &[(&str, Value)]| {
            let cols: Vec<(String, Value)> = cols.iter().map(|(c, v)| (c.to_string(), v.clone())).collect();
            items_row(|c| cols.iter().find(|(n, _)| n == c).map(|(_, v)| v))
        };
        let row = read(&[
            ("id", Value::Int(7)),
            ("sprite_id", Value::Int(4001)),
            ("type", Value::Text("s".into())),
            ("item_name", Value::Text("chair".into())),
            ("effect_id_male", Value::Null),
        ])
        .unwrap();
        assert_eq!(
            row,
            ItemsBaseRow { id: 7, sprite_id: 4001, item_type: "s".into(), item_name: "chair".into(), effect_id: 0 }
        );
        assert_eq!(read(&[("id", Value::Text("8".into()))]).unwrap().id, 8, "unsigned ids may come as text");
        assert_eq!(read(&[("id", Value::Null), ("sprite_id", Value::Int(1))]), Err("a row has no id".to_string()));
        assert_eq!(read(&[("sprite_id", Value::Int(1))]), Err("a row has no id".to_string()));
        assert!(read(&[("id", Value::Int(-1))]).is_err());
        assert!(read(&[("id", Value::Text("x".into()))]).is_err());
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
    fn the_files_and_gearth_name_the_plugin_and_its_version() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!("../manifest.json")).unwrap();
        assert_eq!(manifest["version"], VERSION, "manifest.json and Cargo.toml give the same version");
        assert_eq!(generator(), format!("Catalogue ripper plugin {VERSION}"));
        let info = extension_info();
        assert_eq!(info.version, VERSION);
        assert_eq!(info.title, "Catalogue ripper (All-in-1 Manager)");
    }

    /// A job that fails at once: nothing behind the G-Earth connection.
    fn failed_job() -> (catalog_rip_core::Job, catalog_rip_core::Step) {
        use catalog_rip_core::transport::Closed;
        use catalog_rip_core::{Job, JobIo, RipOptions, Step};
        struct NoFetch;
        impl Fetch for NoFetch {
            fn get(&mut self, _url: &str) -> Result<Vec<u8>, FetchError> {
                Err(FetchError::NotFound)
            }
        }
        struct NoSink;
        impl Sink for NoSink {
            fn write(&mut self, _path: &str, _data: &[u8]) -> Result<(), String> {
                Ok(())
            }
            fn append(&mut self, _path: &str, _data: &[u8]) -> Result<(), String> {
                Ok(())
            }
        }
        let mut job = Job::new(RipOptions::default());
        let clock = SysClock::new();
        for _ in 0..100 {
            let mut io = JobIo {
                transport: &mut Closed,
                fetch: &mut NoFetch,
                sink: &mut NoSink,
                clock: &clock,
                items: None,
                existing: None,
            };
            let step = job.step(&mut io, 10);
            if matches!(step, Step::Failed(_)) {
                return (job, step);
            }
        }
        panic!("the job did not fail");
    }

    #[test]
    fn items_base_survives_a_failed_job_for_the_next_port_and_try_again() {
        use catalog_rip_core::Step;
        let (job, step) = failed_job();
        // A finished job no longer reads items_base...
        assert!(!job.uses_items_base());
        // ...but a failed one may be followed by a job on the next port or by Try again, which reuse it.
        assert!(!drop_items_base(&step, job.uses_items_base()));
        let Step::Failed(failure) = &step else { unreachable!() };
        for ports_left in [0, 3] {
            let next = after_failure(failure, ports_left);
            assert!(!drop_items_base(&step, false), "{next:?}: items_base kept");
        }
        // While it runs, and once it is done, the job's own answer decides.
        assert!(!drop_items_base(&Step::Running, true));
        assert!(drop_items_base(&Step::Running, false));
        assert!(drop_items_base(&Step::Waiting("x".into()), false));
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
