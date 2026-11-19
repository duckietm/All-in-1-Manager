//! The core's I/O traits over the SDK (G-Earth over `tcp::Conn`, HTTP, the picked folder, the clock), and the pure
//! parts around the job: the G-Earth port probing schedule, the items_base reader, the saved hotel profiles and the
//! delay input. The pure parts are tested natively; the SDK calls work only inside the Manager.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aio_plugin_sdk::folders::{self, Folder};
use aio_plugin_sdk::i18n::{tr, tr_with};
use aio_plugin_sdk::sql::Value;
use aio_plugin_sdk::{http, tcp};
use catalog_rip_core::gearth::ExtensionInfo;
use catalog_rip_core::job::{MAX_DELAY_MS, MIN_DELAY_MS};
use catalog_rip_core::profiles::{load_profiles, read_profile_file};
use catalog_rip_core::recording::{PACKETS_DIR, SESSION_FILE};
use catalog_rip_core::source::{self, MAX_READ};
use catalog_rip_core::transport::Transport;
use catalog_rip_core::{
    Clock, ExistingCatalogue, Failure, Fetch, FetchError, HeaderMap, HotelProfile, ItemsBaseRow, ManagerMode, RipOptions,
    Sink, Source, SourceEntry, Step,
};

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
            return Err(FetchError::Failed(tr_with(
                "{url}: the plugin can only read http and https addresses, not files",
                &[("url", url)],
            )));
        }
        http::get(url).map_err(|e| fetch_error(e.message()))
    }

    /// The images, up to 8 at a time ([`http::get_many`]).
    fn get_many(&mut self, urls: &[&str]) -> Vec<Result<Vec<u8>, FetchError>> {
        fetch_many(
            urls,
            |urls| http::get_many(urls).into_iter().map(|r| r.map_err(|e| e.message().to_string())).collect(),
            |url| http::get(url).map_err(|e| e.message().to_string()),
        )
    }

    fn max_batch(&self) -> usize {
        http::MAX_MANY
    }
}

/// The Manager's refusal of a body that did not fit in what was left of one `get_many` call's 32 MiB.
const BUDGET_EXCEEDED: &str = "response budget exceeded";

/// [`Fetch::get_many`] over the Manager's `get_many` (`many`, at most [`http::MAX_MANY`] URLs per call) and `get`
/// (`one`): one result per URL, in order. A URL that is not http(s) is refused without asking the Manager; a body that
/// did not fit in a call's budget is fetched again on its own, as one `get` would have fetched it (so the files are the
/// same as with one image after the other).
pub fn fetch_many(
    urls: &[&str],
    mut many: impl FnMut(&[&str]) -> Vec<Result<Vec<u8>, String>>,
    mut one: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Vec<Result<Vec<u8>, FetchError>> {
    let mut out: Vec<Option<Result<Vec<u8>, FetchError>>> = urls
        .iter()
        .map(|url| {
            (!is_http_url(url)).then(|| {
                Err(FetchError::Failed(tr_with(
                    "{url}: the plugin can only read http and https addresses, not files",
                    &[("url", url)],
                )))
            })
        })
        .collect();
    let asked: Vec<usize> = (0..urls.len()).filter(|i| out[*i].is_none()).collect();
    for chunk in asked.chunks(http::MAX_MANY) {
        let batch: Vec<&str> = chunk.iter().map(|i| urls[*i]).collect();
        let mut results = many(&batch).into_iter();
        for &i in chunk {
            let result = results.next().unwrap_or_else(|| Err("no answer".to_string()));
            let result = match result {
                Err(e) if e.contains(BUDGET_EXCEEDED) => one(urls[i]),
                other => other,
            };
            out[i] = Some(result.map_err(|e| fetch_error(&e)));
        }
    }
    out.into_iter().map(|r| r.expect("every URL answered")).collect()
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

// ---------------------------------------------------------------- reading picked folders

/// A rip in another picked folder that the job reads (`key` is the first part of its paths).
#[derive(Clone, Copy)]
pub struct Route<'a> {
    /// The rip's name in the job's paths.
    pub key: &'a str,
    /// The rip is a folder of `folder` (its paths are `folder`'s); else `folder` is the rip itself (the key is
    /// dropped from the paths).
    pub inside: bool,
    pub folder: &'a Folder,
}

/// Where one read goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The Packet Logger export held in memory.
    Log,
    /// The `n`th [`Route`], at this path.
    Earlier(usize, String),
    /// The output folder.
    Output,
}

/// Where `path` is read: the log when it is `log`'s path, a route when its first part is that route's key (`routes`:
/// key and `inside` of each; the first that matches), else the output folder.
pub fn target(path: &str, log: Option<&str>, routes: &[(&str, bool)]) -> Target {
    if log == Some(path) {
        return Target::Log;
    }
    let (first, rest) = path.split_once('/').unwrap_or((path, ""));
    for (n, (key, inside)) in routes.iter().enumerate() {
        if first == *key {
            return Target::Earlier(n, if *inside { path.to_string() } else { rest.to_string() });
        }
    }
    Target::Output
}

/// At most `max` bytes (and [`MAX_READ`]) of `data` from `offset`: fewer at the end, none past it.
pub fn piece(data: &[u8], offset: u64, max: u32) -> Vec<u8> {
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(data.len());
    let end = start.saturating_add(max.min(MAX_READ) as usize).min(data.len());
    data[start..end].to_vec()
}

/// The SDK's folder entries as the core's (the sizes as the Manager gives them).
pub fn entries(list: Vec<folders::Entry>) -> Vec<SourceEntry> {
    list.into_iter().map(|e| SourceEntry { name: e.name, is_folder: e.is_folder, size: e.size }).collect()
}

/// One picked folder as the core's [`Source`].
pub struct FolderSource<'a>(pub &'a Folder);

impl Source for FolderSource<'_> {
    fn read(&mut self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, String> {
        self.0.read(path, offset, max).map_err(|e| e.to_string())
    }

    fn list(&mut self, path: &str, start: u32) -> Result<Vec<SourceEntry>, String> {
        self.0.list(path, start).map(entries).map_err(|e| e.to_string())
    }
}

/// What the job reads: the output folder (the hotel's earlier rips to compare with), the rips the user chose in other
/// picked folders ([`Route`]), and a Packet Logger export held in memory (`log`: its path and bytes).
pub struct RipsSource<'a> {
    pub output: &'a Folder,
    pub routes: Vec<Route<'a>>,
    pub log: Option<(&'a str, &'a [u8])>,
}

impl RipsSource<'_> {
    fn target(&self, path: &str) -> Target {
        let routes: Vec<(&str, bool)> = self.routes.iter().map(|r| (r.key, r.inside)).collect();
        target(path, self.log.map(|(p, _)| p), &routes)
    }
}

impl Source for RipsSource<'_> {
    fn read(&mut self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, String> {
        match self.target(path) {
            Target::Log => Ok(piece(self.log.map_or(&[][..], |(_, data)| data), offset, max)),
            Target::Earlier(n, at) => self.routes[n].folder.read(&at, offset, max).map_err(|e| e.to_string()),
            Target::Output => self.output.read(path, offset, max).map_err(|e| e.to_string()),
        }
    }

    fn list(&mut self, path: &str, start: u32) -> Result<Vec<SourceEntry>, String> {
        let listed = match self.target(path) {
            Target::Log => return Ok(Vec::new()),
            Target::Earlier(n, at) => self.routes[n].folder.list(&at, start),
            Target::Output => self.output.list(path, start),
        };
        listed.map(entries).map_err(|e| e.to_string())
    }
}

/// What makes a folder a rip the user can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RipNeed {
    /// catalog.json: a rip to compare with.
    Catalog,
    /// packets/session.json: a recorded rip to go on with.
    Packets,
}

impl RipNeed {
    fn file(self, folder: &str) -> String {
        match self {
            RipNeed::Catalog => source::join(folder, "catalog.json"),
            RipNeed::Packets => source::join(folder, &format!("{PACKETS_DIR}/{SESSION_FILE}")),
        }
    }
}

/// The rips in a picked folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RipList {
    /// The picked folder is itself a rip.
    pub top: bool,
    /// Its folders that are rips, newest first (by the date and time at the end of the name).
    pub rips: Vec<String>,
}

/// Folders of a picked folder looked at, at most (one read each).
pub const MAX_RIP_FOLDERS: usize = 500;

/// The rips in `source` (a picked folder): itself, or its folders, when they have `need`'s file.
pub fn list_rips(source: &mut dyn Source, need: RipNeed) -> Result<RipList, String> {
    let has = |source: &mut dyn Source, folder: &str| source.read(&need.file(folder), 0, 1).is_ok();
    if has(source, "") {
        return Ok(RipList { top: true, rips: Vec::new() });
    }
    let folders: Vec<String> =
        source::list_all(source, "")?.into_iter().filter(|e| e.is_folder).take(MAX_RIP_FOLDERS).map(|e| e.name).collect();
    let mut rips: Vec<String> = folders.into_iter().filter(|name| has(source, name)).collect();
    // `<hotel>_<YYYY-mm-dd_HH-MM-SS>`: newest first, whatever the hotel.
    let stamp = |name: &String| name.len().checked_sub(19).and_then(|at| name.get(at..)).unwrap_or("").to_string();
    rips.sort_by(|a, b| stamp(b).cmp(&stamp(a)).then_with(|| a.cmp(b)));
    Ok(RipList { top: false, rips })
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
        let first = FIRST_PORT.to_string();
        let last = LAST_PORT.to_string();
        if let Some(port) = self.sweep_port() {
            return tr_with("Checking port {port}…", &[("port", &port.to_string())]);
        }
        match self.found {
            Some(port) => tr_with("Something answers on port {port} (G-Earth?).", &[("port", &port.to_string())]),
            None if self.none_answered() => tr_with(
                "Not found on ports {first}–{last}. Start G-Earth, then click Check again.",
                &[("first", &first), ("last", &last)],
            ),
            None if self.misses > 0 => tr_with(
                "Not found on ports {first}–{last} so far. Start G-Earth, then click Check again.",
                &[("first", &first), ("last", &(FIRST_PORT + self.misses - 1).to_string())],
            ),
            None => tr_with(
                "Start G-Earth, then click Check again to look for it on ports {first}–{last}.",
                &[("first", &first), ("last", &last)],
            ),
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
            self.notes.push(tr_with("items_base: the read with effect_id_male failed ({why}); using 0.", &[("why", &why)]));
        }
        if rows.len() >= SQL_LIMIT && self.width > WINDOW {
            self.width = (self.width / 2).max(WINDOW);
            return Load::More;
        }
        if rows.len() >= SQL_LIMIT {
            self.notes.push(tr_with(
                "items_base has {limit} rows with ids from {from} to {to}: ids repeat, so some rows may be missing.",
                &[("limit", &SQL_LIMIT.to_string()), ("from", &self.from.to_string()), ("to", &(to - 1).to_string())],
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
            self.notes.push(tr_with(
                "{count} items_base rows could not be read and were left out (first: {why}).",
                &[("count", &self.skipped.to_string()), ("why", &why)],
            ));
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
            Some(Value::Text(t)) => t.trim().parse().map(Some).map_err(|_| tr_with("{column} is not a number: {value}", &[("column", column), ("value", t)])),
            Some(Value::Null) | None => Ok(None),
            Some(_) => Err(tr_with("{column} is not a number", &[("column", column)])),
        }
    }
    fn text(value: Option<&Value>) -> String {
        match value {
            Some(Value::Text(t)) => t.clone(),
            Some(Value::Int(v)) => v.to_string(),
            _ => String::new(),
        }
    }
    let id = int(value("id"), "id")?.ok_or_else(|| tr("a row has no id"))?;
    let id32 = u32::try_from(id).map_err(|_| tr_with("id {id} is out of range", &[("id", &id.to_string())]))?;
    let sprite_id = int(value("sprite_id"), "sprite_id")?.unwrap_or(0);
    Ok(ItemsBaseRow {
        id: id32,
        sprite_id: i32::try_from(sprite_id).map_err(|_| tr_with("sprite_id of {id} is out of range", &[("id", &id.to_string())]))?,
        item_type: text(value("type")),
        item_name: text(value("item_name")),
        effect_id: int(value("effect_id_male"), "effect_id_male")?.and_then(|e| i32::try_from(e).ok()).unwrap_or(0),
    })
}

// ---------------------------------------------------------------- the hotel's catalogue ids

/// The tables of the hotel's catalogue, with their `catalog_type` and whether they hold pages (else offers): the
/// normal catalogue, and the Builders Club warehouse of Arcturus Morningstar (as the Manager's catalogue import reads
/// them).
pub const CATALOGUE_TABLES: [(&str, &str, bool); 4] = [
    ("catalog_pages", "NORMAL", true),
    ("catalog_items", "NORMAL", false),
    ("catalog_pages_bc", "BUILDER", true),
    ("catalog_items_bc", "BUILDER", false),
];

/// The id reads the [`ExistingLoader`] needs.
pub trait IdSource {
    /// The `id`s of `table` with `from <= id < to` (at most [`SQL_LIMIT`]); `Err` for a row whose id cannot be read.
    fn ids(&mut self, table: &str, from: i64, to: i64) -> Result<Vec<Result<i64, String>>, SourceError>;
    /// Whether `table` has an id `>= from`.
    fn any_from(&mut self, table: &str, from: i64) -> Result<bool, SourceError>;
}

/// The id of a row (`value`: its `id` column).
pub fn id_value(value: Option<&Value>) -> Result<i64, String> {
    match value {
        Some(Value::Int(v)) => Ok(*v),
        Some(Value::Text(t)) => t.trim().parse().map_err(|_| tr_with("{column} is not a number: {value}", &[("column", "id"), ("value", t)])),
        _ => Err(tr("a row has no id")),
    }
}

/// Reads the page and offer ids of the hotel's catalogue ([`CATALOGUE_TABLES`]), one query per step, in id windows
/// like the [`ItemsLoader`] (a read returns at most 10,000 rows). For [`ManagerMode::ReplaceRipped`] and
/// [`ManagerMode::Add`]. A Builders Club table that cannot be read counts as empty (a hotel without the warehouse).
#[derive(Debug, Clone)]
pub struct ExistingLoader {
    table: usize,
    from: i64,
    width: i64,
    check_rest: bool,
    pub existing: ExistingCatalogue,
    /// Rows read.
    pub rows: usize,
    notes: Vec<String>,
}

impl Default for ExistingLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ExistingLoader {
    pub fn new() -> Self {
        Self {
            table: 0,
            from: 0,
            width: WINDOW,
            check_rest: false,
            existing: ExistingCatalogue::default(),
            rows: 0,
            notes: Vec::new(),
        }
    }

    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// The table read now.
    pub fn table(&self) -> &'static str {
        CATALOGUE_TABLES[self.table.min(CATALOGUE_TABLES.len() - 1)].0
    }

    /// On to the next table; `Done` after the last.
    fn next_table(&mut self) -> Load {
        self.table += 1;
        self.from = 0;
        self.width = WINDOW;
        self.check_rest = false;
        if self.table >= CATALOGUE_TABLES.len() {
            Load::Done
        } else {
            Load::More
        }
    }

    /// A read of the current table failed.
    fn failed(&mut self, e: SourceError) -> Load {
        let (table, kind, _) = CATALOGUE_TABLES[self.table];
        if kind == "BUILDER" && !e.denied {
            self.notes.push(tr_with(
                "{table} cannot be read ({why}): the hotel has no Builders Club catalogue there.",
                &[("table", table), ("why", &e.message)],
            ));
            let (_, _, page) = CATALOGUE_TABLES[self.table];
            let ids = if page { &mut self.existing.pages } else { &mut self.existing.offers };
            ids.remove(kind);
            return self.next_table();
        }
        Load::Unavailable(e.message)
    }

    /// One read.
    pub fn step(&mut self, source: &mut dyn IdSource) -> Load {
        let Some(&(table, kind, page)) = CATALOGUE_TABLES.get(self.table) else { return Load::Done };
        if self.check_rest {
            return match source.any_from(table, self.from) {
                Ok(false) => self.next_table(),
                Ok(true) => {
                    self.check_rest = false;
                    self.width = self.width.saturating_mul(8).min(MAX_WINDOW);
                    Load::More
                }
                Err(e) => self.failed(e),
            };
        }
        let to = self.from.saturating_add(self.width);
        let rows = match source.ids(table, self.from, to) {
            Ok(rows) => rows,
            Err(e) => return self.failed(e),
        };
        if rows.len() >= SQL_LIMIT && self.width > WINDOW {
            self.width = (self.width / 2).max(WINDOW);
            return Load::More;
        }
        let count = rows.len();
        let ids = if page { &mut self.existing.pages } else { &mut self.existing.offers };
        let ids = ids.entry(kind.to_string()).or_default();
        // A row without a readable id cannot be deleted or collide by id: left out.
        for id in rows.into_iter().flatten() {
            ids.insert(id);
            self.rows += 1;
        }
        self.from = to;
        if to == i64::MAX {
            return self.next_table();
        }
        if count == 0 {
            self.check_rest = true;
        } else if count < SQL_LIMIT / 4 {
            self.width = self.width.saturating_mul(2).min(MAX_WINDOW);
        }
        Load::More
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
            .map_err(|e| {
                tr_with("{file} cannot be read ({error}), so it was not changed", &[("file", PROFILES_FILE), ("error", &format!("{e:#}"))])
            })?,
        _ => Vec::new(),
    };
    let new = read_profile_file(file_name, json).map_err(|e| tr_with("the learned profile cannot be read ({error})", &[("error", &format!("{e:#}"))]))?;
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
    match text.trim().parse::<u64>() {
        Ok(ms) if (MIN_DELAY_MS..=MAX_DELAY_MS).contains(&ms) => Ok(ms),
        _ => Err(tr_with(
            "The delay must be a number of milliseconds from {min} to {max}.",
            &[("min", &MIN_DELAY_MS.to_string()), ("max", &MAX_DELAY_MS.to_string())],
        )),
    }
}

/// The Manager modes of the select, in its order.
pub const MANAGER_MODES: [ManagerMode; 3] = [ManagerMode::Replace, ManagerMode::ReplaceRipped, ManagerMode::Add];

/// What the modes need: the hotel's catalogue ids.
pub fn needs_existing(mode: ManagerMode) -> bool {
    mode != ManagerMode::Replace
}

/// The header ids of the advanced settings, by input id: (input id, the id's name).
pub const HEADER_INPUTS: [(&str, &str); 4] = [
    ("h_get_index", "GetCatalogIndex"),
    ("h_get_page", "GetCatalogPage"),
    ("h_index", "CatalogIndex"),
    ("h_page", "CatalogPage"),
];

/// The address inputs of the advanced settings: (input id, what it is).
pub const URL_INPUTS: [&str; 5] = ["catalog_url", "icon_url", "furnidata_url", "productdata_url", "site_config_url"];

/// The checkboxes of the advanced settings, by id.
pub const ADVANCED_CHECKBOXES: [&str; 7] =
    ["adaptive_delay", "learn_headers", "rip_extras", "record_packets", "use_furnidata", "diff_with_previous", "diff_delete_removed"];

/// The number inputs of the advanced settings: (input id, smallest, largest).
pub const NUMBER_INPUTS: [(&str, u64, u64); 4] =
    [("timeout_ms", 200, 120_000), ("max_delay_ms", MIN_DELAY_MS, MAX_DELAY_MS), ("hotel_wait_s", 10, 3_600), ("resend_s", 1, 600)];

/// The "Advanced settings" card: every [`RipOptions`] field the built-in ripper has, but what the plugin decides
/// itself (the G-Earth ports and folder, the output folder, the profiles folder) and what the core does not do
/// (index-only rips, single branches). Kept as typed (`texts`), checked when a rip starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advanced {
    /// The text inputs by id ([`HEADER_INPUTS`], [`URL_INPUTS`], [`NUMBER_INPUTS`]).
    pub texts: std::collections::BTreeMap<&'static str, String>,
    /// The checkboxes by id ([`ADVANCED_CHECKBOXES`]).
    pub checks: std::collections::BTreeMap<&'static str, bool>,
}

impl Default for Advanced {
    fn default() -> Self {
        let d = RipOptions::default();
        let mut texts = std::collections::BTreeMap::new();
        for (id, _) in HEADER_INPUTS {
            texts.insert(id, String::new());
        }
        for id in URL_INPUTS {
            texts.insert(id, String::new());
        }
        texts.insert("timeout_ms", d.timeout_ms.to_string());
        texts.insert("max_delay_ms", d.max_delay_ms.to_string());
        texts.insert("hotel_wait_s", (d.hotel_wait_ms / 1000).to_string());
        texts.insert("resend_s", (d.resend_every_ms / 1000).to_string());
        let checks = [
            ("adaptive_delay", d.adaptive_delay),
            ("learn_headers", d.learn_headers),
            ("rip_extras", d.rip_extras),
            ("record_packets", d.record_packets),
            ("use_furnidata", d.use_furnidata),
            ("diff_with_previous", d.diff_with_previous),
            ("diff_delete_removed", d.diff_delete_removed),
        ]
        .into_iter()
        .collect();
        Advanced { texts, checks }
    }
}

impl Advanced {
    pub fn text(&self, id: &str) -> &str {
        self.texts.get(id).map_or("", String::as_str)
    }

    pub fn check(&self, id: &str) -> bool {
        self.checks.get(id).copied().unwrap_or(false)
    }

    /// A change of one of the card's controls: `Some` with what is wrong with an input's new text (empty when it is
    /// fine), `None` when `id` is not one of the card's.
    pub fn set(&mut self, id: &str, value: &str) -> Option<String> {
        if let Some(check) = self.checks.get_mut(id) {
            *check = value == "true";
            return Some(String::new());
        }
        *self.texts.get_mut(id)? = value.to_string();
        Some(self.check_text(id).err().unwrap_or_default())
    }

    /// What is wrong with the input `id`, if anything.
    fn check_text(&self, id: &str) -> Result<(), String> {
        let text = self.text(id).trim();
        if let Some((_, name)) = HEADER_INPUTS.iter().find(|(i, _)| *i == id) {
            return parse_header(name, text).map(|_| ());
        }
        if URL_INPUTS.contains(&id) {
            return parse_url(text).map(|_| ());
        }
        if let Some((_, min, max)) = NUMBER_INPUTS.iter().find(|(i, _, _)| *i == id) {
            return parse_number(text, *min, *max).map(|_| ());
        }
        Ok(())
    }

    /// The settings into `options`; the first input that is wrong is the error.
    pub fn apply(&self, options: &mut RipOptions) -> Result<(), String> {
        let header = |n: usize| parse_header(HEADER_INPUTS[n].1, self.text(HEADER_INPUTS[n].0).trim());
        options.header_overrides =
            HeaderMap { get_catalog_index: header(0)?, get_catalog_page: header(1)?, catalog_index: header(2)?, catalog_page: header(3)? };
        let url = |id: &str| parse_url(self.text(id).trim());
        options.catalog_url = url("catalog_url")?;
        options.icon_url = url("icon_url")?;
        options.furnidata_url = url("furnidata_url")?;
        options.productdata_url = url("productdata_url")?;
        options.site_config_url = url("site_config_url")?;
        let number = |id: &str| {
            let (_, min, max) = NUMBER_INPUTS.iter().find(|(i, _, _)| *i == id).expect("a number input");
            parse_number(self.text(id).trim(), *min, *max)
        };
        options.timeout_ms = number("timeout_ms")?;
        options.max_delay_ms = number("max_delay_ms")?;
        options.hotel_wait_ms = number("hotel_wait_s")? * 1000;
        options.resend_every_ms = number("resend_s")? * 1000;
        options.adaptive_delay = self.check("adaptive_delay");
        options.learn_headers = self.check("learn_headers");
        options.rip_extras = self.check("rip_extras");
        options.record_packets = self.check("record_packets");
        options.use_furnidata = self.check("use_furnidata");
        options.diff_with_previous = self.check("diff_with_previous");
        options.diff_delete_removed = self.check("diff_delete_removed");
        Ok(())
    }
}

/// A header id input: empty is `None` (the profile's or G-Earth's id is used).
pub fn parse_header(name: &str, text: &str) -> Result<Option<u16>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<u16>().map(Some).map_err(|_| tr_with("The {name} id must be a number from 0 to 65535, or empty.", &[("name", name)]))
}

/// An address input: empty is `None` (the hotel profile's is used); else an http or https address.
pub fn parse_url(text: &str) -> Result<Option<String>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    if is_http_url(text) && !text.chars().any(char::is_whitespace) {
        Ok(Some(text.to_string()))
    } else {
        Err(tr_with("{url} is not an http or https address.", &[("url", text)]))
    }
}

/// A whole number from `min` to `max`.
pub fn parse_number(text: &str, min: u64, max: u64) -> Result<u64, String> {
    match text.parse::<u64>() {
        Ok(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(tr_with(
            "{text} is not a number from {min} to {max}.",
            &[("text", text), ("min", &min.to_string()), ("max", &max.to_string())],
        )),
    }
}

// ---------------------------------------------------------------- a Packet Logger export

/// The largest Packet Logger export the plugin holds (a picked file is read whole; with the job reading it, a step
/// stays within the memory limit).
pub const MAX_LOG: usize = 16 * 1024 * 1024;

/// The output folder's name for a log's hotel: the host typed (trimmed, lower case), checked.
pub fn log_host(text: &str) -> Result<String, String> {
    let host = text.trim().to_ascii_lowercase();
    if host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        Ok(host)
    } else {
        Err(tr_with("{host} is not a host name.", &[("host", text.trim())]))
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
            message: catalog_rip_core::Message::verbatim("G-Earth did not answer on port 9092 within 3 s"),
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
                source: None,
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
        assert!(drop_items_base(&Step::Waiting(catalog_rip_core::Message::verbatim("x")), false));
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

    // ------------------------------------------------------------ the hotel's catalogue ids

    /// The catalogue tables in memory: ids per table; `missing`: tables that do not exist.
    struct Catalogue {
        ids: BTreeMap<&'static str, Vec<i64>>,
        missing: Vec<&'static str>,
        denied: bool,
        reads: usize,
    }

    impl IdSource for Catalogue {
        fn ids(&mut self, table: &str, from: i64, to: i64) -> Result<Vec<Result<i64, String>>, SourceError> {
            self.reads += 1;
            if self.denied {
                return Err(SourceError { denied: true, message: format!("permission denied: table '{table}'") });
            }
            if self.missing.contains(&table) {
                return Err(SourceError { denied: false, message: format!("Table '{table}' doesn't exist") });
            }
            let ids = self.ids.get(table).cloned().unwrap_or_default();
            Ok(ids.into_iter().rev().filter(|id| (from..to).contains(id)).take(SQL_LIMIT).map(Ok).collect())
        }

        fn any_from(&mut self, table: &str, from: i64) -> Result<bool, SourceError> {
            self.reads += 1;
            Ok(self.ids.get(table).is_some_and(|ids| ids.iter().any(|id| *id >= from)))
        }
    }

    fn load_existing(source: &mut Catalogue) -> (Load, ExistingLoader) {
        let mut loader = ExistingLoader::new();
        for _ in 0..10_000 {
            match loader.step(source) {
                Load::More => {}
                end => return (end, loader),
            }
        }
        panic!("the loader never ends");
    }

    #[test]
    fn the_catalogue_ids_are_read_in_windows_from_every_table() {
        let pages: Vec<i64> = (1..=3_000).chain(900_000..900_005).collect();
        let offers: Vec<i64> = (1..=60_000).collect();
        let mut db = Catalogue {
            ids: [("catalog_pages", pages.clone()), ("catalog_items", offers.clone()), ("catalog_pages_bc", vec![5, 6]), ("catalog_items_bc", vec![])]
                .into_iter()
                .collect(),
            missing: Vec::new(),
            denied: false,
            reads: 0,
        };
        let (end, loader) = load_existing(&mut db);
        assert_eq!(end, Load::Done);
        let e = &loader.existing;
        assert_eq!(e.pages["NORMAL"].iter().copied().collect::<Vec<_>>(), pages);
        assert_eq!(e.offers["NORMAL"].len(), offers.len());
        assert_eq!(e.pages["BUILDER"].iter().copied().collect::<Vec<_>>(), [5, 6]);
        assert_eq!(e.max_page("NORMAL"), 900_004);
        assert_eq!(e.max_offer("NORMAL"), 60_000);
        assert_eq!(e.max_offer("BUILDER"), 0);
        assert_eq!(loader.rows, pages.len() + offers.len() + 2);
        assert!(db.reads < 60, "{} reads", db.reads);
    }

    #[test]
    fn no_builders_club_tables_is_an_empty_warehouse_but_no_catalogue_is_unknown() {
        let mut db = Catalogue {
            ids: [("catalog_pages", vec![1, 2]), ("catalog_items", vec![7])].into_iter().collect(),
            missing: vec!["catalog_pages_bc", "catalog_items_bc"],
            denied: false,
            reads: 0,
        };
        let (end, mut loader) = load_existing(&mut db);
        assert_eq!(end, Load::Done);
        assert!(!loader.existing.pages.contains_key("BUILDER"));
        assert_eq!(loader.take_notes().len(), 2);

        db.missing = vec!["catalog_items"];
        let (end, _) = load_existing(&mut db);
        assert_eq!(end, Load::Unavailable("Table 'catalog_items' doesn't exist".into()));

        db.denied = true;
        db.reads = 0;
        let (end, _) = load_existing(&mut db);
        assert!(matches!(end, Load::Unavailable(m) if m.starts_with("permission denied")));
        assert_eq!(db.reads, 1);
    }

    #[test]
    fn catalogue_ids_may_come_as_text() {
        assert_eq!(id_value(Some(&Value::Int(4))), Ok(4));
        assert_eq!(id_value(Some(&Value::Text(" 12 ".into()))), Ok(12));
        assert!(id_value(Some(&Value::Null)).is_err());
        assert!(id_value(None).is_err());
        assert!(id_value(Some(&Value::Text("x".into()))).is_err());
    }

    // ------------------------------------------------------------ http::get_many

    #[test]
    fn get_many_keeps_the_order_refuses_files_and_refetches_what_did_not_fit() {
        let urls = ["https://a.test/1.png", "C:/x.png", "https://a.test/2.png", "https://a.test/3.png", "https://a.test/4.png"];
        let mut calls: Vec<Vec<String>> = Vec::new();
        let mut singles: Vec<String> = Vec::new();
        let got = fetch_many(
            &urls,
            |batch| {
                calls.push(batch.iter().map(|u| u.to_string()).collect());
                batch
                    .iter()
                    .map(|u| match *u {
                        "https://a.test/2.png" => Err("HTTP 404".to_string()),
                        "https://a.test/3.png" => Err("permission denied: response budget exceeded".to_string()),
                        "https://a.test/4.png" => Err("HTTP 500".to_string()),
                        u => Ok(u.as_bytes().to_vec()),
                    })
                    .collect()
            },
            |u| {
                singles.push(u.to_string());
                Ok(b"alone".to_vec())
            },
        );
        assert_eq!(calls, [vec!["https://a.test/1.png", "https://a.test/2.png", "https://a.test/3.png", "https://a.test/4.png"]]);
        assert_eq!(singles, ["https://a.test/3.png"], "the body that did not fit is fetched on its own");
        assert_eq!(got[0], Ok(b"https://a.test/1.png".to_vec()));
        assert!(matches!(&got[1], Err(FetchError::Failed(m)) if m.contains("only read http and https")));
        assert_eq!(got[2], Err(FetchError::NotFound));
        assert_eq!(got[3], Ok(b"alone".to_vec()));
        assert_eq!(got[4], Err(FetchError::Failed("HTTP 500".into())));

        // More than 8: several calls of at most 8.
        let many: Vec<String> = (0..19).map(|n| format!("http://b.test/{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut sizes = Vec::new();
        let got = fetch_many(
            &many,
            |batch| {
                sizes.push(batch.len());
                batch.iter().map(|u| Ok(u.as_bytes().to_vec())).collect()
            },
            |_| unreachable!(),
        );
        assert_eq!(sizes, [8, 8, 3]);
        assert!(got.iter().zip(&many).all(|(r, u)| r.as_deref() == Ok(u.as_bytes())));
        // A short answer fails the URLs it left out.
        let got = fetch_many(&many[..2], |_| vec![Ok(vec![1])], |_| unreachable!());
        assert_eq!(got[0], Ok(vec![1]));
        assert!(got[1].is_err());
    }

    // ------------------------------------------------------------ picked folders

    #[test]
    fn reads_go_to_the_log_the_chosen_rips_or_the_output() {
        let routes = [("habbo.nl_2026-01-01_10-00-00", true), ("Old rip", false)];
        let log = Some("packets.txt");
        assert_eq!(target("packets.txt", log, &routes), Target::Log);
        assert_eq!(
            target("habbo.nl_2026-01-01_10-00-00/catalog.json", log, &routes),
            Target::Earlier(0, "habbo.nl_2026-01-01_10-00-00/catalog.json".into())
        );
        assert_eq!(target("Old rip/packets/session.json", log, &routes), Target::Earlier(1, "packets/session.json".into()));
        assert_eq!(target("Old rip", log, &routes), Target::Earlier(1, String::new()));
        assert_eq!(target("habbo.nl_2026-02-01_10-00-00/catalog.json", log, &routes), Target::Output);
        assert_eq!(target("", log, &routes), Target::Output);
        assert_eq!(target("packets.txt", None, &routes), Target::Output);

        let data: Vec<u8> = (0..10).collect();
        assert_eq!(piece(&data, 2, 3), [2, 3, 4]);
        assert_eq!(piece(&data, 8, 5), [8, 9]);
        assert!(piece(&data, 10, 5).is_empty());
        assert!(piece(&data, u64::MAX, 5).is_empty());
    }

    #[test]
    fn rips_are_listed_newest_first() {
        use catalog_rip_core::MemSource;
        let mut folder = MemSource::new();
        for (path, body) in [
            ("habbo.nl_2026-01-02_10-00-00/catalog.json", "{}"),
            ("habbo.nl_2026-01-02_10-00-00/packets/session.json", "{}"),
            ("hotel.example_2026-03-01_09-00-00/catalog.json", "{}"),
            ("habbo.nl_2025-12-31_23-59-59/catalog.json", "{}"),
            ("stopped_2026-02-01_00-00-00/packets/session.json", "{}"),
            ("other/readme.txt", "x"),
            ("loose.json", "{}"),
        ] {
            folder.files.insert(path.into(), body.as_bytes().to_vec());
        }
        let rips = list_rips(&mut folder, RipNeed::Catalog).unwrap();
        assert_eq!(
            rips,
            RipList {
                top: false,
                rips: vec![
                    "hotel.example_2026-03-01_09-00-00".into(),
                    "habbo.nl_2026-01-02_10-00-00".into(),
                    "habbo.nl_2025-12-31_23-59-59".into()
                ]
            }
        );
        let stopped = list_rips(&mut folder, RipNeed::Packets).unwrap();
        assert_eq!(stopped.rips, ["stopped_2026-02-01_00-00-00", "habbo.nl_2026-01-02_10-00-00"]);

        // The picked folder is the rip itself.
        let mut rip = MemSource::new();
        rip.files.insert("packets/session.json".into(), b"{}".to_vec());
        assert_eq!(list_rips(&mut rip, RipNeed::Packets).unwrap(), RipList { top: true, rips: Vec::new() });
        assert_eq!(list_rips(&mut rip, RipNeed::Catalog).unwrap(), RipList::default());
    }

    // ------------------------------------------------------------ advanced settings

    #[test]
    fn advanced_settings_start_as_the_cores_defaults() {
        let mut options = RipOptions::default();
        Advanced::default().apply(&mut options).unwrap();
        let d = RipOptions::default();
        assert_eq!(options.header_overrides, d.header_overrides);
        assert_eq!((options.catalog_url, options.icon_url, options.furnidata_url), (None, None, None));
        assert_eq!((options.productdata_url, options.site_config_url), (None, None));
        assert_eq!(
            (options.timeout_ms, options.max_delay_ms, options.hotel_wait_ms, options.resend_every_ms),
            (d.timeout_ms, d.max_delay_ms, d.hotel_wait_ms, d.resend_every_ms)
        );
        assert_eq!(
            (options.adaptive_delay, options.learn_headers, options.rip_extras, options.record_packets),
            (d.adaptive_delay, d.learn_headers, d.rip_extras, d.record_packets)
        );
        assert_eq!(
            (options.use_furnidata, options.diff_with_previous, options.diff_delete_removed),
            (d.use_furnidata, d.diff_with_previous, d.diff_delete_removed)
        );
    }

    #[test]
    fn advanced_settings_are_checked_and_applied() {
        let mut a = Advanced::default();
        assert_eq!(a.set("h_get_page", "3965"), Some(String::new()));
        assert_eq!(a.set("catalog_url", " https://images.hotel.test/c_images/catalogue "), Some(String::new()));
        assert_eq!(a.set("hotel_wait_s", "120"), Some(String::new()));
        assert_eq!(a.set("adaptive_delay", "false"), Some(String::new()));
        assert_eq!(a.set("diff_delete_removed", "true"), Some(String::new()));
        assert_eq!(a.set("not_ours", "x"), None);
        let mut options = RipOptions::default();
        a.apply(&mut options).unwrap();
        assert_eq!(options.header_overrides.get_catalog_page, Some(3965));
        assert_eq!(options.header_overrides.get_catalog_index, None);
        assert_eq!(options.catalog_url.as_deref(), Some("https://images.hotel.test/c_images/catalogue"));
        assert_eq!(options.hotel_wait_ms, 120_000);
        assert!(!options.adaptive_delay);
        assert!(options.diff_delete_removed);

        for (id, bad) in [("h_index", "70000"), ("icon_url", "C:/icons/"), ("timeout_ms", "100"), ("resend_s", "x")] {
            let mut a = Advanced::default();
            let said = a.set(id, bad).unwrap();
            assert!(!said.is_empty(), "{id}={bad}");
            assert_eq!(a.apply(&mut RipOptions::default()), Err(said));
        }
        assert_eq!(parse_header("CatalogPage", ""), Ok(None));
        assert_eq!(parse_url(""), Ok(None));
        assert!(parse_url("https://a b").is_err());
        assert_eq!(parse_number("5", 1, 600), Ok(5));
    }

    #[test]
    fn manager_modes_and_log_hosts() {
        assert_eq!(MANAGER_MODES[0], ManagerMode::default());
        assert!(!needs_existing(ManagerMode::Replace));
        assert!(needs_existing(ManagerMode::ReplaceRipped) && needs_existing(ManagerMode::Add));
        assert_eq!(log_host(" Game-NL.Habbo.com "), Ok("game-nl.habbo.com".into()));
        assert_eq!(log_host(""), Ok(String::new()));
        assert!(log_host("a/b").is_err());
    }
}
