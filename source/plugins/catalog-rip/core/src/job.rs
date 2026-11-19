//! The rip as a job that is stepped: the built-in's async `rip_with` (connect to G-Earth, wait
//! for the hotel connection, pick the hotel profile or learn the header ids, read the catalogue
//! indexes, the pages and the extra messages, write the output folder, download the images)
//! turned into a state machine that the plugin drives with [`Job::step`] from its tick.
//!
//! The core has no clock, sockets, files or threads: every step gets a [`JobIo`] with the
//! G-Earth connection ([`Transport`]), the HTTP getter ([`Fetch`]), the output ([`Sink`]), the
//! time ([`Clock`]) and the hotel's items_base. A step returns within its budget (plus the
//! handling of what one read brought): it waits only by pumping G-Earth for at most what is left
//! of the budget, and paces requests by the clock (never by sleeping).
//!
//! Output: `<hotel>_<YYYY-mm-dd_HH-MM-SS>/` (UTC) with catalog.json, catalog_pages.sql,
//! catalog_items.sql (+ the `_bc` pair), catalog_manager.sql, missing_furni.json,
//! missing_furnidata.json, catalog_changes.json and catalog_manager_changes.sql (compared with an
//! earlier rip), catalog_extras.json, README.txt, `packets/` (the recorded packets), `images/`,
//! `images/icons/`, `images/frontpage/`, written through the sink with paths that start with that
//! folder name.
//!
//! Besides a rip ([`Job::new`]): [`Job::resume`] goes on with a stopped rip from its recorded
//! packets, [`Job::reexport`] writes a recorded rip again without the hotel, and
//! [`Job::import_log`] makes a rip from a G-Earth Packet Logger export. They read through
//! [`JobIo::source`], a piece per call, step by step. Unlike the built-in, which wrote a resumed
//! or re-exported rip into its own folder, they write a new rip folder (the plugin may not
//! rewrite files it did not write) and carry the recording over into its `packets/`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::conn::ConnectionInfo;
use crate::diff::{self, Changes, ChangesSummary, Digest, OldRows, PreviousRip};
use crate::export::{ItemsBase, RippedCatalog, SqlStats};
use crate::extras;
use crate::furnidata::{self, Furnidata};
use crate::gearth::{self, ExtensionInfo, GEarth, GEarthEvent};
use crate::gearth_log::{self, LogScan};
use crate::images::{self, ImageJob, ImageStats};
use crate::learn::Learner;
use crate::manager::{ExistingCatalogue, ManagerMode, ManagerStats};
use crate::message::Message;
use crate::missing::{self, MissingFurni};
use crate::names::hotel_name;
use crate::output::{self, n0, DataExport};
use crate::packets::{self, CatalogIndex, CatalogPage, Headers};
use crate::productdata::{self, Productdata};
use crate::profiles::{self, builtin_profiles, DetectedBy, HeaderMap, HotelKind, HotelProfile};
use crate::recording::{self, Loader, Recorded, Recorder, SessionInfo};
use crate::site_config;
use crate::source::Source;
use crate::time::Timestamp;
use crate::transport::Transport;
use crate::wire::Packet;
use crate::RipEvent;

// ---------------------------------------------------------------- the I/O a step gets

/// Why a [`Fetch`] got nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The file is not there (HTTP 403, 404 or 410).
    #[error("not found")]
    NotFound,
    /// Anything else: the host cannot be reached, another HTTP status, ...
    #[error("{0}")]
    Failed(String),
}

/// Gets a URL (the plugin's HTTP; a furnidata/productdata source may also be a file the plugin
/// can read). Blocking: the job calls it at most once in a row before it looks at its budget.
pub trait Fetch {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, FetchError>;
}

/// Where the output goes. `path` is relative to the folder the rips are made in and starts with
/// the rip folder (`habbo.nl_2026-11-15_12-00-00/images/icons/icon_3.png`), `/`-separated.
pub trait Sink {
    /// Creates the file `path` with `data` (an image: each file is written once). In `packets/`
    /// a file is written again (session.json as the rip goes on, a packet the hotel sent
    /// twice): the call replaces the file this sink wrote.
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String>;
    /// Appends `data` to the file `path`, creating it on the first call (the data files are
    /// streamed through it in pieces, so none is held whole in memory).
    fn append(&mut self, path: &str, data: &[u8]) -> Result<(), String>;
}

/// The time.
pub trait Clock {
    /// Milliseconds on a clock that never goes back (only differences are used).
    fn now_ms(&self) -> u64;
    /// Seconds since 1970-01-01T00:00:00Z (the folder name and "ripped at").
    fn unix_secs(&self) -> i64;
}

/// What one [`Job::step`] works with.
pub struct JobIo<'a> {
    /// The connection to G-Earth. Once [`Job::uses_gearth`] is false the job never touches it
    /// again (the plugin closes it and may pass [`crate::transport::Closed`]).
    pub transport: &'a mut dyn Transport,
    pub fetch: &'a mut dyn Fetch,
    pub sink: &'a mut dyn Sink,
    pub clock: &'a dyn Clock,
    /// The hotel's items_base ([`output::items_base`] from its rows). `None`: not known, the
    /// item ids are written as items_base subqueries resolved on import.
    pub items: Option<&'a ItemsBase>,
    /// The page and offer ids already in the hotel's catalogue, for
    /// [`ManagerMode::ReplaceRipped`] and [`ManagerMode::Add`]. `None`: not known.
    pub existing: Option<&'a ExistingCatalogue>,
    /// What the job may read (the folder the rips are made in): the recording of
    /// [`Job::resume`] / [`Job::reexport`], the log of [`Job::import_log`], the earlier rip to
    /// compare with. `None`: nothing (then there is no comparison).
    pub source: Option<&'a mut dyn Source>,
}

// ---------------------------------------------------------------- options

/// Which hotel profile to use.
#[derive(Debug, Clone)]
// Not boxed: callers build `Fixed(profile)` directly.
#[allow(clippy::large_enum_variant)]
pub enum ProfileChoice {
    /// Pick from these (e.g. [`profiles::profiles`]) by the connection's host, revision and
    /// client type; when none fits, G-Earth's packet info alone (or learned ids).
    Detect(Vec<HotelProfile>),
    /// Use this one.
    Fixed(HotelProfile),
}

impl Default for ProfileChoice {
    fn default() -> Self {
        ProfileChoice::Detect(builtin_profiles())
    }
}

/// The ripper's settings (every field has a default): the built-in's, without what the plugin
/// does itself (waiting for G-Earth to start, the ports, the output folder), without what is a
/// kind of job here (`resume_from`: [`Job::resume`]) and without what this version does not do
/// (index-only, branches).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RipOptions {
    /// The hotel profile: detected (default: among the built-in ones) or fixed.
    #[serde(skip)]
    pub profile: ProfileChoice,
    /// Time between two requests to the server, in ms (50-5,000).
    pub delay_ms: u64,
    /// How long to wait for an answer before retrying / giving up, in ms (at least 200).
    pub timeout_ms: u64,
    /// Header ids that win over the profile's and G-Earth's.
    pub header_overrides: HeaderMap,
    pub download_images: bool,
    /// Rip the normal catalogue.
    pub normal: bool,
    /// Rip the Builders Club catalogue (when the hotel profile says the hotel has one).
    pub builders_club: bool,
    /// How long to wait for the hotel connection, the login and a first answer, in ms.
    pub hotel_wait_ms: u64,
    /// While waiting for a first answer, how often to ask again, in ms.
    pub resend_every_ms: u64,
    /// Catalogue image URL (…/c_images/catalogue); wins over the profile's.
    pub catalog_url: Option<String>,
    /// Page icon URL prefix (`<prefix><n>.png`); wins over the profile's.
    pub icon_url: Option<String>,
    /// Match products by class name through the source hotel's furnidata.
    pub use_furnidata: bool,
    /// The source hotel's furnidata (URL); wins over the profile's. Official Habbo hotels
    /// need none.
    pub furnidata_url: Option<String>,
    /// How catalog_manager.sql changes the hotel's catalogue.
    pub manager_mode: ManagerMode,
    /// When no source has every header id, learn them from the client while the user opens the
    /// catalogue (instead of stopping).
    pub learn_headers: bool,
    /// Slow down when pages go unanswered (and speed up again when they answer).
    pub adaptive_delay: bool,
    /// The slowest the adaptive delay goes, in ms.
    pub max_delay_ms: u64,
    /// The source hotel's productdata (URL): product names for the reports. Official Habbo
    /// hotels need none.
    pub productdata_url: Option<String>,
    /// Also read the club offers, the bundle discount ruleset and the gift wrapping
    /// configuration (catalog_extras.json).
    pub rip_extras: bool,
    /// Where the client's configuration is (Nitro site / config file, or Flash
    /// external_variables): the image URLs are read from it. Wins over the profile's.
    pub site_config_url: Option<String>,
    /// What G-Earth shows in its Extensions tab.
    #[serde(skip)]
    pub extension: ExtensionInfo,
    /// The G-Earth port the plugin connected to (only for the log).
    pub gearth_port: u16,
    /// How long G-Earth may take to send its InfoRequest on the new connection, in ms.
    pub handshake_timeout_ms: u64,
    /// README.txt with Windows line ends (the built-in writes them on Windows, where the
    /// Manager runs).
    pub readme_crlf: bool,
    /// What the file headers name as their maker ("Catalogue ripper plugin 0.1.0"). Default:
    /// [`DEFAULT_GENERATOR`].
    #[serde(skip)]
    pub generator: String,
    /// Keep the raw packets in `<folder>/packets/` (needed to resume and re-export). A resumed
    /// rip always records.
    pub record_packets: bool,
    /// Compare with the newest earlier rip of the same hotel in the folder the rips are made in
    /// (one with a catalog.json, through [`JobIo::source`]): catalog_changes.json and
    /// catalog_manager_changes.sql.
    pub diff_with_previous: bool,
    /// Compare with this earlier rip folder instead (a [`Source`] path).
    pub diff_against: Option<String>,
    /// The changes file also deletes the pages and offers the hotel removed.
    pub diff_delete_removed: bool,
}

/// The maker named in the file headers when the options name none.
pub const DEFAULT_GENERATOR: &str = concat!("catalog-rip-core ", env!("CARGO_PKG_VERSION"));

impl Default for RipOptions {
    fn default() -> Self {
        Self {
            profile: ProfileChoice::default(),
            delay_ms: 400,
            timeout_ms: 10_000,
            header_overrides: HeaderMap::default(),
            download_images: true,
            normal: true,
            builders_club: true,
            hotel_wait_ms: 10 * 60 * 1000,
            resend_every_ms: 20_000,
            catalog_url: None,
            icon_url: None,
            use_furnidata: true,
            furnidata_url: None,
            manager_mode: ManagerMode::Replace,
            learn_headers: true,
            adaptive_delay: true,
            max_delay_ms: 5_000,
            productdata_url: None,
            rip_extras: true,
            site_config_url: None,
            extension: ExtensionInfo::default(),
            gearth_port: gearth::PORT_ONSET,
            handshake_timeout_ms: 3_000,
            readme_crlf: true,
            generator: DEFAULT_GENERATOR.into(),
            record_packets: true,
            diff_with_previous: true,
            diff_against: None,
            diff_delete_removed: false,
        }
    }
}

/// A G-Earth Packet Logger export to rip from ([`Job::import_log`]).
#[derive(Debug, Clone, Default)]
pub struct ImportLog {
    /// The .txt file (a [`Source`] path).
    pub path: String,
    /// The hotel's game host (the output folder's name); empty: "gearth-log".
    pub host: String,
    /// The hotel's profile, for its ids and image URLs, if known.
    pub profile: Option<HotelProfile>,
}

/// The pieces a Packet Logger export and an earlier catalog.json are read in.
const READ_PIECE: u32 = 1024 * 1024;
/// The most a step reads or writes of them, whatever its budget: pieces, recorded files.
const PIECES_PER_STEP: usize = 4;
const FILES_PER_STEP: usize = 200;

/// Goes on while the step has time, `max` times at most.
fn within(clock: &dyn Clock, end: u64, max: usize) -> impl FnMut() -> bool + '_ {
    let mut n = 1;
    move || {
        n += 1;
        n <= max && clock.now_ms() < end
    }
}

const DEFAULT_CATALOG_URL: &str = "https://images.habbo.com/c_images/catalogue";
const DEFAULT_ICON_URL: &str = "https://images.habbo.com/c_images/catalogue/icon_";
/// Answered requests in a row before the adaptive delay steps back down.
pub const SPEED_UP_AFTER: usize = 15;
/// The fastest and slowest request delay the options may ask for, in ms.
pub const MIN_DELAY_MS: u64 = 50;
pub const MAX_DELAY_MS: u64 = 5_000;

/// The options as numbers the job uses (ms).
#[derive(Debug, Clone, Copy)]
struct Timing {
    delay: u64,
    max_delay: u64,
    response_timeout: u64,
    hotel_wait: u64,
    resend_every: u64,
}

impl Timing {
    fn of(options: &RipOptions) -> Self {
        let delay = options.delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
        Self {
            delay,
            max_delay: options.max_delay_ms.max(delay),
            response_timeout: options.timeout_ms.max(200),
            hotel_wait: options.hotel_wait_ms,
            resend_every: options.resend_every_ms.max(100),
        }
    }
}

// ---------------------------------------------------------------- what a step says

/// Where the job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Phase {
    /// Reading a recording or a Packet Logger export.
    Load,
    /// Answering G-Earth's InfoRequest.
    Connect,
    /// Waiting for G-Earth to accept the extension and for the hotel connection.
    WaitHotel,
    /// Learning the header ids from the user's client.
    Learn,
    /// Reading the catalogue indexes.
    Index,
    Pages,
    /// Reading the extra messages (club offers, bundle discounts, gift wrapping).
    Extras,
    /// Writing the data files, one per step (and reading the furnidata / productdata for them,
    /// one per step).
    Export,
    /// Downloading the images, then README.txt.
    Images,
    Done,
}

/// How far the job is: pages in [`Phase::Pages`], images in [`Phase::Images`], else 0 / 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    /// The page or image last done.
    pub label: String,
}

/// What a finished rip wrote (and, in [`Failure::partial`], what a stopped one wrote).
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// The rip folder (the first part of every sink path).
    pub folder: String,
    /// The files written, relative to the rip folder (`images` for the images folder).
    pub files: Vec<String>,
    /// Output name of the hotel (habbo.nl, or the retro's host).
    pub hotel: String,
    pub host: String,
    pub client_type: String,
    pub hotel_version: String,
    pub profile: String,
    pub kind: HotelKind,
    pub headers: Headers,
    /// Pages read (over every catalogue).
    pub pages: usize,
    pub pages_failed: usize,
    /// Offers read.
    pub items: usize,
    pub products: usize,
    pub frontpage_items: usize,
    pub images: ImageStats,
    pub sql: SqlStats,
    pub manager: ManagerStats,
    pub complete: bool,
    /// From the first request to the end of the extras, in ms.
    pub elapsed_ms: u64,
    /// Class names in the source furnidata that was used (0: none).
    pub furnidata_names: usize,
    /// Product names in the source productdata that was used (0: none).
    pub productdata_names: usize,
    /// The header ids were learned from the client.
    pub learned_headers: bool,
    /// The learned ids as a hotel profile file: (file name, JSON), for the plugin to save in
    /// its profiles folder.
    pub learned_profile: Option<(String, String)>,
    /// The time between requests at the end (the adaptive delay may have raised it).
    pub delay_ms: u64,
    /// The products the hotel's items_base lacks (empty without items_base).
    pub missing_furni: Vec<MissingFurni>,
    /// The user cancelled (during the export or the images: the rip itself was whole).
    pub cancelled: bool,
    /// Pages that came from the recording of a stopped rip (not asked again).
    pub resumed_pages: usize,
    /// What changed since the earlier rip it was compared with.
    pub changes: Option<ChangesSummary>,
}

/// Why the job stopped early.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// What to tell the user (with "The catalogue read so far was saved in ..." when
    /// [`Failure::partial`] is set).
    pub message: Message,
    /// The user cancelled.
    pub cancelled: bool,
    /// The G-Earth connection failed or closed: the G-Earth session is over (it always is once
    /// the job ends; this says it was the reason).
    pub gearth_error: Option<gearth::Error>,
    /// G-Earth let the extension go after accepting it, before any hotel connection (it was
    /// restarted, or Connect in Nitro mode): a new job on a new connection should go on.
    pub reconnect: bool,
    /// The other side of the connection did not answer as G-Earth (no InfoRequest within
    /// [`RipOptions::handshake_timeout_ms`], another first message, or it closed): it is not
    /// G-Earth's extension port, and G-Earth may be on another port.
    pub not_gearth: bool,
    /// What was written before stopping (the built-in's `PartialOutput`).
    pub partial: Option<Box<Summary>>,
}

/// What [`Job::step`] returns.
#[derive(Debug, Clone, PartialEq)]
// Returned once per step; a finished job keeps a copy.
#[allow(clippy::large_enum_variant)]
pub enum Step {
    /// Working; step again.
    Running,
    /// Waiting for the user (the text says what to do); step again.
    Waiting(Message),
    Done(Summary),
    Failed(Failure),
}

// ---------------------------------------------------------------- inner state

/// The job stopped what it was doing (a request phase ends with what it has).
#[derive(Debug, Clone)]
struct Abort {
    message: Message,
    cancelled: bool,
    gearth: Option<gearth::Error>,
}

impl Abort {
    fn new(message: Message) -> Abort {
        Abort {
            message,
            cancelled: false,
            gearth: None,
        }
    }

    fn cancelled() -> Abort {
        Abort {
            message: Message::new("The catalogue rip was cancelled."),
            cancelled: true,
            gearth: None,
        }
    }

    fn failure(self) -> Failure {
        Failure {
            message: self.message,
            cancelled: self.cancelled,
            gearth_error: self.gearth,
            reconnect: false,
            not_gearth: false,
            partial: None,
        }
    }
}

#[derive(Clone, Debug)]
enum Want {
    Index(String),
    Page(String, i32),
    Raw(u16),
}

/// After a request is sent, what its wait means.
#[derive(Clone, Copy, Debug)]
enum WaitKind {
    /// The first try.
    First,
    /// The one retry of a kind the server answered before.
    Retry,
    /// Sent again because the client sent it itself (G-Earth lets it through now).
    AfterClient,
    /// The periodic resend while waiting for a first answer.
    Resend { end: u64 },
    /// One try, no retry (the extras).
    Once,
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    /// Send when the delay since the last request has passed.
    Send(WaitKind),
    /// Wait for the answer until `until`.
    Wait { until: u64, kind: WaitKind },
    /// Never answered yet: wait for an answer, the client's own request, or the next resend,
    /// until `end`. `until`: the next resend (set when the round starts).
    Patient { end: u64, until: Option<u64> },
}

#[derive(Clone, Debug)]
struct Request {
    want: Want,
    packet: Packet,
    label: Message,
    stage: Stage,
    /// Whether the answer was already there was looked at (once, before the first send: an
    /// answer that comes while the request waits for the delay is counted after sending, as
    /// the built-in does).
    looked: bool,
}

/// What a state handler tells the step loop.
// Lives only between a handler and the step loop.
#[allow(clippy::large_enum_variant)]
enum Ctl {
    /// Go on in this step (the state changed or there is more to do).
    Continue,
    /// The budget is spent (or nothing more can happen in this step).
    Yield,
    End(Step),
}

enum Poll<T> {
    Ready(T),
    Pending,
}

/// Comparing with an earlier rip.
enum DiffStage {
    Start,
    Find(PreviousRip),
    Read {
        earlier: String,
        offset: u64,
        digest: Box<Digest>,
        /// Found by looking (the next earlier rip is tried when this one cannot be read).
        found: Option<PreviousRip>,
    },
    Compare {
        earlier: String,
        rows: OldRows,
    },
}

enum ExportStage {
    Furnidata,
    Productdata,
    Diff(DiffStage),
    /// The data files, one per step.
    Data,
    SiteConfig {
        urls: Vec<String>,
        next: usize,
        fetched: Vec<(String, Vec<u8>)>,
    },
}

/// What kind of job.
#[derive(Debug, Clone)]
enum Begin {
    Rip,
    Resume(String),
    Reexport(String),
    Import(Box<ImportLog>),
}

enum State {
    Start,
    /// Reading a recording ([`Job::resume`], [`Job::reexport`]).
    Load(Box<Loader>),
    /// Reading a Packet Logger export.
    Import(Box<LogScan>),
    /// Taking what it found as a recording.
    Adding(Box<std::collections::btree_map::IntoIter<String, Vec<u8>>>),
    /// Writing the recording's files into the new rip folder's `packets/`.
    Copy {
        next: usize,
    },
    Connect {
        deadline: Option<u64>,
    },
    WaitHotel {
        deadline: u64,
        accepted: bool,
        last_note: u64,
    },
    Learn {
        learner: Learner,
        deadline: u64,
        partial: [Option<u16>; 4],
        sources: [&'static str; 4],
    },
    Index {
        i: usize,
    },
    Pages {
        n: usize,
    },
    Extras {
        i: usize,
    },
    Export(ExportStage),
    Images,
    Finished,
}

/// What the export works with, from the end of the requests on.
struct Out {
    catalog: RippedCatalog,
    database_note: String,
    /// An items_base was given (the plugin may drop it once [`Job::uses_items_base`] is false).
    with_items_base: bool,
    /// The source furnidata: the class names, and the entries of the furni items_base lacks
    /// (dropped once missing_furnidata.json is written).
    furnidata: Option<Furnidata>,
    /// The source productdata (dropped once the data files are written).
    products: Option<Productdata>,
    /// Class / product names in them (for the README and the summary).
    furnidata_names: Option<usize>,
    productdata_names: usize,
    /// The data files, one per step.
    data: DataExport,
    /// What changed since an earlier rip (made before the data files).
    changes: Option<Changes>,
    /// What it says (kept once the changes files are written and `changes` dropped).
    changes_summary: Option<ChangesSummary>,
    files: Vec<String>,
    catalog_url: String,
    icon_url: String,
    plan: Vec<ImageJob>,
    next_image: usize,
    next_url: usize,
    stats: ImageStats,
    /// Image files written (a second name for the same file is not fetched again).
    written: HashSet<String>,
    images_text: Option<String>,
}

/// The rip. Make one with [`Job::new`], then [`Job::step`] it until it returns
/// [`Step::Done`] or [`Step::Failed`]; [`Job::take_log`] after every step.
pub struct Job {
    options: RipOptions,
    timing: Timing,
    begin: Begin,
    state: State,
    cancel: bool,
    finished: Option<Step>,
    log: Vec<Message>,
    events: Vec<RipEvent>,
    progress: Progress,

    gearth: Option<GEarth>,
    /// Events read but not handled yet (they came with the hotel connection).
    backlog: Vec<GEarthEvent>,
    /// G-Earth was pumped in this step (every step pumps at least once).
    pumped: bool,
    /// Something was fetched in this step.
    fetched: bool,
    connection: Option<ConnectionInfo>,
    profile: Option<HotelProfile>,
    headers: Option<Headers>,
    learned: bool,
    learned_profile: Option<(String, String)>,
    folder: String,
    mode_list: Vec<String>,
    accepted: Vec<String>,
    rip_started: u64,
    elapsed_ms: u64,

    // The requests (the built-in's `Ripper`).
    delay: u64,
    answered_in_a_row: usize,
    indexes: HashMap<String, CatalogIndex>,
    pages: HashMap<(String, i32), CatalogPage>,
    /// Pages that answered with something unreadable.
    unreadable: HashMap<(String, i32), String>,
    client_sent: HashSet<u16>,
    /// Request headers the server has answered at least once.
    answered: HashSet<u16>,
    last_send: Option<u64>,
    /// The mode of the last index / page request, for answers that do not carry it.
    index_mode: String,
    page_mode: String,
    /// Answer headers kept as they come (the extra messages), and what came.
    raw_watch: HashSet<u16>,
    raw: HashMap<u16, Vec<u8>>,
    req: Option<Request>,

    jobs: Vec<(usize, i32, String)>,
    failed: BTreeMap<String, Vec<i32>>,
    skip_mode: Option<usize>,
    read_in_mode: HashMap<usize, usize>,
    extra_answers: BTreeMap<String, Vec<u8>>,
    aborted: Option<Abort>,
    out: Option<Out>,

    /// The packets of this rip (`packets/`).
    recorder: Recorder,
    record_warned: bool,
    /// session.json as it is now.
    session: Option<SessionInfo>,
    /// The recording being carried over (resume, re-export, import).
    recorded: Option<Recorded>,
    resumed_pages: usize,
}

fn progress_label(text: &str) -> String {
    text.replace(['%', '[', ']', '\r', '\n'], " ")
}

fn secs(ms: u64) -> u64 {
    ms / 1000
}

impl Job {
    pub fn new(options: RipOptions) -> Job {
        Job::with(options, Begin::Rip)
    }

    /// Goes on with a stopped rip: `folder` (a [`Source`] path) is its rip folder, with the
    /// `packets/` the built-in ripper or this one recorded. Only what the recording lacks is
    /// asked (G-Earth must be connected to the same hotel); the whole rip is written into a new
    /// rip folder, with the recording carried over and completed.
    pub fn resume(options: RipOptions, folder: &str) -> Job {
        Job::with(options, Begin::Resume(folder.to_string()))
    }

    /// Writes a recorded rip again without the hotel (the built-in's `reexport`): with other
    /// settings (Manager export mode, images, furnidata) or a newer items_base, into a new rip
    /// folder (with the recording).
    pub fn reexport(options: RipOptions, folder: &str) -> Job {
        Job::with(options, Begin::Reexport(folder.to_string()))
    }

    /// A rip from a G-Earth Packet Logger export (the built-in's `import_log`): the catalogue it
    /// holds is written as a rip of `log.host` (recorded packets, every file); only the pages
    /// that were opened while logging are in it.
    pub fn import_log(options: RipOptions, log: ImportLog) -> Job {
        Job::with(options, Begin::Import(Box::new(log)))
    }

    fn with(options: RipOptions, begin: Begin) -> Job {
        let timing = Timing::of(&options);
        Job {
            options,
            timing,
            begin,
            state: State::Start,
            cancel: false,
            finished: None,
            log: Vec::new(),
            events: Vec::new(),
            progress: Progress {
                phase: Phase::Connect,
                done: 0,
                total: 0,
                label: String::new(),
            },
            gearth: None,
            backlog: Vec::new(),
            pumped: false,
            fetched: false,
            connection: None,
            profile: None,
            headers: None,
            learned: false,
            learned_profile: None,
            folder: String::new(),
            mode_list: Vec::new(),
            accepted: Vec::new(),
            rip_started: 0,
            elapsed_ms: 0,
            delay: timing.delay,
            answered_in_a_row: 0,
            indexes: HashMap::new(),
            pages: HashMap::new(),
            unreadable: HashMap::new(),
            client_sent: HashSet::new(),
            answered: HashSet::new(),
            last_send: None,
            index_mode: String::new(),
            page_mode: String::new(),
            raw_watch: HashSet::new(),
            raw: HashMap::new(),
            req: None,
            jobs: Vec::new(),
            failed: BTreeMap::new(),
            skip_mode: None,
            read_in_mode: HashMap::new(),
            extra_answers: BTreeMap::new(),
            aborted: None,
            out: None,
            recorder: Recorder::default(),
            record_warned: false,
            session: None,
            recorded: None,
            resumed_pages: 0,
        }
    }

    /// Stop: the next step writes what was read (when the requests had begun) with a README
    /// that says the rip was cancelled, and ends the job. As in the built-in, a rip cancelled
    /// while it still asked the hotel ends as [`Step::Failed`] (with [`Failure::partial`] and
    /// `cancelled`), one cancelled during the export or the image downloads as [`Step::Done`]
    /// (its catalogue was whole; the images stop where they were).
    ///
    /// A cancel during the requests ends them in the step that sees it, which lets G-Earth go
    /// and returns (no fetch while the G-Earth connection is still open). The next steps finish
    /// the export whatever their budget, as the export always goes: like the built-in, it still
    /// reads the furnidata and the productdata when they are used (one download per step), then
    /// writes one data file per step. It downloads no images and no client configuration.
    pub fn cancel(&mut self) {
        self.cancel = true;
    }

    pub fn progress(&self) -> Progress {
        self.progress.clone()
    }

    /// The log lines since the last call.
    pub fn take_log(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.log)
    }

    /// The events since the last call (every event also has a log line).
    pub fn take_events(&mut self) -> Vec<RipEvent> {
        std::mem::take(&mut self.events)
    }

    /// Whether the job still needs the G-Earth connection. False once it is done with G-Earth
    /// (after the requests, as the built-in drops its session before writing the output), after
    /// any G-Earth error, and once the job ended: then the plugin closes the connection, so
    /// G-Earth stops holding game packets for the extension. True while a resumed rip reads its
    /// recording (it connects next); never for [`Job::reexport`] and [`Job::import_log`], which
    /// need no G-Earth.
    pub fn uses_gearth(&self) -> bool {
        self.gearth.is_some()
            || (matches!(self.state, State::Start | State::Load(_))
                && matches!(self.begin, Begin::Rip | Begin::Resume(_)))
    }

    /// Whether the job still reads [`JobIo::items`]. False once the data files that use it are
    /// written (and when the job ended): the plugin may drop the hotel's items_base then.
    pub fn uses_items_base(&self) -> bool {
        match &self.state {
            State::Export(ExportStage::Data) => self
                .out
                .as_ref()
                .is_none_or(|out| out.data.needs_items_base()),
            State::Export(ExportStage::SiteConfig { .. }) | State::Images | State::Finished => {
                false
            }
            _ => true,
        }
    }

    fn say(&mut self, message: Message) {
        self.log.push(message);
    }

    fn emit(&mut self, event: RipEvent) {
        self.events.push(event);
    }

    fn warn(&mut self, message: Message) {
        self.emit(RipEvent::Warning(message.to_string()));
        self.say(Message::new("Warning: {text}").with_message("text", message));
    }

    fn set_phase(&mut self, phase: Phase) {
        self.progress = Progress {
            phase,
            done: 0,
            total: 0,
            label: String::new(),
        };
    }

    /// Does what can be done within `budget_ms` (the waits for G-Earth add up to at most this;
    /// see the module docs). After [`Step::Done`] or [`Step::Failed`] every step returns the
    /// same again.
    pub fn step(&mut self, io: &mut JobIo, budget_ms: u32) -> Step {
        if let Some(end) = &self.finished {
            return end.clone();
        }
        let end = io.clock.now_ms().saturating_add(budget_ms.into());
        self.pumped = false;
        self.fetched = false;
        if self.cancel {
            match self.on_cancel(io) {
                Some(Ctl::End(step)) => return self.end(step),
                // The requests ended: G-Earth is let go before the export (see `after_requests`).
                Some(Ctl::Yield) => return self.pending(),
                _ => {}
            }
        }
        loop {
            let ctl = match self.state {
                State::Start => self.start(),
                State::Load(_) => self.load(io, end),
                State::Import(_) => self.import(io, end),
                State::Adding(_) => self.adding(io, end),
                State::Copy { .. } => self.copy(io, end),
                State::Connect { .. } => self.connect(io, end),
                State::WaitHotel { .. } => self.wait_hotel(io, end),
                State::Learn { .. } => self.learn(io, end),
                State::Index { .. } => self.index(io, end),
                State::Pages { .. } => self.page(io, end),
                State::Extras { .. } => self.extra(io, end),
                State::Export(_) => self.export(io, end),
                State::Images => self.images(io, end),
                State::Finished => Ctl::End(Step::Running),
            };
            match ctl {
                Ctl::Continue => continue,
                Ctl::Yield => return self.pending(),
                Ctl::End(step) => return self.end(step),
            }
        }
    }

    fn end(&mut self, step: Step) -> Step {
        self.gearth = None;
        self.state = State::Finished;
        self.progress.phase = Phase::Done;
        self.finished = Some(step.clone());
        step
    }

    /// Running, or Waiting with what the user should do.
    fn pending(&self) -> Step {
        let text = match &self.state {
            State::Connect { .. } => Some(Message::new("Waiting for G-Earth to accept the extension.")),
            State::WaitHotel { .. } => Some(Message::new(
                "Waiting for the hotel connection: connect G-Earth, then open your client and log in.",
            )),
            State::Learn { .. } => Some(Message::new(
                "Open the catalogue in your client and click any page: the ripper learns the header ids from it.",
            )),
            _ => match &self.req {
                Some(Request {
                    stage: Stage::Patient { .. },
                    label,
                    ..
                }) => Some(
                    Message::new(
                        "The hotel did not answer the {label} request yet: make sure your client is logged in.",
                    )
                    .with_message("label", label.clone()),
                ),
                _ => None,
            },
        };
        match text {
            Some(text) => Step::Waiting(text),
            None => Step::Running,
        }
    }

    /// The cancel, at the start of a step. `None`: the current phase sees it itself.
    fn on_cancel(&mut self, io: &mut JobIo) -> Option<Ctl> {
        match self.state {
            State::Start
            | State::Load(_)
            | State::Import(_)
            | State::Adding(_)
            | State::Connect { .. }
            | State::WaitHotel { .. }
            | State::Learn { .. } => Some(Ctl::End(Step::Failed(Abort::cancelled().failure()))),
            // A re-export or an import: nothing was written but packets/.
            State::Copy { .. } if self.gearth.is_none() => {
                Some(Ctl::End(Step::Failed(Abort::cancelled().failure())))
            }
            State::Copy { .. }
            | State::Index { .. }
            | State::Pages { .. }
            | State::Extras { .. } => {
                if self.aborted.is_none() {
                    self.aborted = Some(Abort::cancelled());
                }
                self.req = None;
                Some(self.after_requests(io))
            }
            State::Export(_) | State::Images | State::Finished => None,
        }
    }

    // ------------------------------------------------------------ G-Earth

    /// Pumps G-Earth for at most `ms`. `Ok((events, progressed))`: progressed is false when
    /// nothing came and the clock did not move (then the step should give up its turn).
    fn pump(&mut self, io: &mut JobIo, ms: u64) -> Result<(Vec<GEarthEvent>, bool), Abort> {
        if !self.backlog.is_empty() {
            return Ok((std::mem::take(&mut self.backlog), true));
        }
        let Some(g) = self.gearth.as_mut() else {
            return Err(Abort::new(Message::new("the connection to G-Earth ended")));
        };
        self.pumped = true;
        let before = io.clock.now_ms();
        let ms = u32::try_from(ms).unwrap_or(u32::MAX);
        match g.pump(&mut *io.transport, ms) {
            Ok(events) => {
                let progressed = !events.is_empty() || io.clock.now_ms() > before;
                Ok((events, progressed))
            }
            Err(e) => Err(self.gearth_lost(e)),
        }
    }

    /// The budget is spent, and G-Earth was pumped at least once in this step (so the game
    /// packets G-Earth holds are answered even with a budget of 0).
    fn out_of_time(&self, now: u64, end: u64) -> bool {
        now >= end && self.pumped
    }

    /// Whether a (blocking) fetch may start now: within the budget, or as the first thing of a
    /// step (so a job stepped with tiny budgets still gets on). A cancelled job gets on with its
    /// export whatever the budget (see [`Job::cancel`]).
    fn may_fetch(&self, now: u64, end: u64) -> bool {
        self.cancel || now < end || !(self.pumped || self.fetched)
    }

    /// A G-Earth error ends the G-Earth session.
    fn gearth_lost(&mut self, e: gearth::Error) -> Abort {
        self.gearth = None;
        Abort {
            message: Message::verbatim(e.to_string()),
            cancelled: false,
            gearth: Some(e),
        }
    }

    // ------------------------------------------------------------ connecting

    fn start(&mut self) -> Ctl {
        match self.begin.clone() {
            Begin::Rip | Begin::Resume(_) => {
                if !self.options.normal && !self.options.builders_club {
                    return Ctl::End(Step::Failed(
                        Abort::new(Message::new(
                            "Nothing to rip: turn on the normal and/or the Builders Club catalogue.",
                        ))
                        .failure(),
                    ));
                }
            }
            Begin::Reexport(_) | Begin::Import(_) => {}
        }
        match self.begin.clone() {
            Begin::Rip => self.start_gearth(),
            Begin::Resume(folder) | Begin::Reexport(folder) => {
                self.set_phase(Phase::Load);
                self.state = State::Load(Box::new(Loader::new(&folder)));
                Ctl::Continue
            }
            Begin::Import(log) => {
                self.set_phase(Phase::Load);
                let known = log
                    .profile
                    .as_ref()
                    .map(|p| p.headers.clone())
                    .unwrap_or_default();
                self.state = State::Import(Box::new(LogScan::new(&log.path, known, READ_PIECE)));
                Ctl::Continue
            }
        }
    }

    fn start_gearth(&mut self) -> Ctl {
        self.log_instructions();
        self.gearth = Some(GEarth::start(&self.options.extension));
        self.set_phase(Phase::Connect);
        self.state = State::Connect { deadline: None };
        Ctl::Continue
    }

    /// A failure that names nothing to read from.
    fn no_source() -> Ctl {
        Ctl::End(Step::Failed(
            Abort::new(Message::new(
                "There is no folder to read the earlier rip or the log from.",
            ))
            .failure(),
        ))
    }

    // ------------------------------------------------------------ recordings

    /// Reads the recording of a resume or a re-export.
    fn load(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let clock = io.clock;
        let Some(source) = io.source.as_deref_mut() else {
            return Self::no_source();
        };
        let State::Load(loader) = &mut self.state else {
            unreachable!()
        };
        let result = loader.step(source, &mut within(clock, end, FILES_PER_STEP));
        let (done, total) = loader.progress();
        self.progress.done = done as u64;
        self.progress.total = total as u64;
        match result {
            Err(e) => Ctl::End(Step::Failed(Abort::new(e).failure())),
            Ok(None) => Ctl::Yield,
            Ok(Some(recorded)) => match self.begin {
                Begin::Resume(_) => {
                    self.recorded = Some(recorded);
                    self.start_gearth()
                }
                _ => {
                    self.say(Self::writing_again(&recorded));
                    self.offline(recorded, io)
                }
            },
        }
    }

    /// Reads the Packet Logger export of an import.
    fn import(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let clock = io.clock;
        let Some(source) = io.source.as_deref_mut() else {
            return Self::no_source();
        };
        let State::Import(scan) = &mut self.state else {
            unreachable!()
        };
        let result = scan.step(source, &mut within(clock, end, PIECES_PER_STEP));
        let packets = scan.packets;
        self.progress.done = scan.progress().0;
        let found = match result {
            Err(e) => return Ctl::End(Step::Failed(Abort::new(e).failure())),
            Ok(None) => return Ctl::Yield,
            Ok(Some(found)) => found,
        };
        let Begin::Import(log) = self.begin.clone() else {
            unreachable!()
        };
        let Some(headers) = found.headers else {
            return Ctl::End(Step::Failed(Abort::new(Message::new("no ids")).failure()));
        };
        self.say(
            Message::new("{path}: {packets} packets, {indexes} catalogue indexes and {pages} pages.")
                .with("path", &log.path)
                .with("packets", packets)
                .with("indexes", found.indexes.len())
                .with("pages", found.pages.len()),
        );
        let host = if log.host.trim().is_empty() {
            "gearth-log".to_string()
        } else {
            log.host.trim().to_string()
        };
        let profile = log.profile.unwrap_or_else(|| HotelProfile {
            name: format!("{host} (G-Earth log)"),
            kind: HotelKind::FlashRetro,
            hosts: Vec::new(),
            revisions: Vec::new(),
            headers: HeaderMap::new(headers.get_index, headers.get_page, headers.index, headers.page),
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
        });
        let session = SessionInfo {
            format: recording::FORMAT,
            host: host.clone(),
            port: 0,
            hotel_version: String::new(),
            client_type: "G-Earth log".into(),
            profile,
            headers: Some(headers),
            modes: gearth_log::mode_order(found.indexes.keys().cloned()),
            failed: Default::default(),
            complete: false,
            ripped_at: Timestamp::new(io.clock.unix_secs()).date_time(),
        };
        let folder = self.new_folder(&host, io);
        self.recorded = Some(Recorded::new(&folder, session));
        // The packets are read as a recording, a few per step (the bodies are moved).
        self.state = State::Adding(Box::new(gearth_log::recorded_files(found).into_iter()));
        Ctl::Yield
    }

    /// Takes the packets an import found as a recording read back (the built-in wrote them,
    /// then read them back), a few per step.
    fn adding(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let State::Adding(files) = &mut self.state else {
            unreachable!()
        };
        let recorded = self.recorded.as_mut().expect("recording");
        let mut more = within(io.clock, end, FILES_PER_STEP);
        for (name, body) in files.by_ref() {
            if let Err(e) = recorded.add(&name, body) {
                return Ctl::End(Step::Failed(Abort::new(e).failure()));
            }
            if !more() {
                return Ctl::Yield;
            }
        }
        let recorded = self.recorded.take().expect("recording");
        self.say(Self::writing_again(&recorded));
        self.offline(recorded, io)
    }

    fn writing_again(recorded: &Recorded) -> Message {
        Message::new("Writing {folder} again from its recorded packets ({catalogues} catalogues, {pages} pages).")
            .with("folder", &recorded.folder)
            .with(
                "catalogues",
                recorded
                    .session
                    .modes
                    .iter()
                    .filter(|m| recorded.indexes.contains_key(*m))
                    .count(),
            )
            .with("pages", n0(recorded.pages.len()))
    }

    /// `<hotel>_<stamp>` for a new rip folder of `host`.
    fn new_folder(&self, host: &str, io: &JobIo) -> String {
        format!(
            "{}_{}",
            hotel_name(host),
            Timestamp::new(io.clock.unix_secs()).folder_stamp()
        )
    }

    /// A recording to write out without the hotel (re-export, import): it is carried over into
    /// a new rip folder, then written as a rip.
    fn offline(&mut self, recorded: Recorded, io: &mut JobIo) -> Ctl {
        if !recorded.unreadable.is_empty() {
            self.warn(
                Message::new("{count} recorded pages cannot be read and are left out.")
                    .with("count", recorded.unreadable.len()),
            );
        }
        let session = recorded.session.clone();
        self.folder = match self.begin {
            Begin::Import(_) => recorded.folder.clone(),
            _ => self.new_folder(&session.host, io),
        };
        self.connection = Some(ConnectionInfo {
            host: session.host.clone(),
            port: session.port,
            hotel_version: session.hotel_version.clone(),
            client_identifier: String::new(),
            client_type: session.client_type.clone(),
            packets: Vec::new(),
        });
        self.profile = Some(session.profile.clone());
        self.headers = session.headers;
        self.delay = self.options.delay_ms;
        self.recorder = Recorder::new(&self.folder, true);
        let written = match self.begin {
            Begin::Import(_) => self.recorder.session(&mut *io.sink, &session),
            _ => self.recorder.session_bytes(&mut *io.sink, &recorded.session_raw),
        };
        self.record_result(written);
        self.session = Some(session);
        self.recorded = Some(recorded);
        self.state = State::Copy { next: 0 };
        Ctl::Continue
    }

    /// The recording is carried over: the output, as a re-export writes it.
    fn offline_output(&mut self, io: &mut JobIo) -> Ctl {
        let Some(recorded) = self.recorded.take() else {
            unreachable!()
        };
        let session = recorded.session;
        self.extra_answers = recorded.extras;
        let catalog = output::assemble(
            &output::Source {
                host: &session.host,
                hotel_version: &session.hotel_version,
                client_type: &session.client_type,
                ripped_at: &session.ripped_at,
                generator: &self.options.generator,
            },
            &session.modes,
            recorded.indexes,
            recorded.pages,
            &session.failed,
            session.complete,
        );
        self.begin_output(catalog, io)
    }

    /// Carries the recording over into this rip's `packets/`, a few files per step. A resumed
    /// rip keeps answering G-Earth meanwhile.
    fn copy(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        if self.gearth.is_some() && !self.pumped {
            if let Err(abort) = self.pump_handle(io, 0) {
                self.aborted = Some(abort);
                return self.after_requests(io);
            }
        }
        loop {
            let State::Copy { next } = &mut self.state else {
                unreachable!()
            };
            let n = *next;
            let recorded = self.recorded.as_mut().expect("recording");
            let Some((name, body)) = recorded.files.get_mut(n) else {
                recorded.files = Vec::new();
                if self.gearth.is_some() {
                    self.recorded = None;
                    self.set_phase(Phase::Index);
                    self.state = State::Index { i: 0 };
                    return Ctl::Continue;
                }
                return self.offline_output(io);
            };
            *next += 1;
            let (name, body) = (std::mem::take(name), std::mem::take(body));
            let written = self.recorder.copy(&mut *io.sink, &name, &body);
            self.record_result(written);
            if io.clock.now_ms() >= end || (n + 1) % FILES_PER_STEP == 0 {
                return Ctl::Yield;
            }
        }
    }

    /// A packet file that could not be written: said once (the rip goes on without it, as the
    /// built-in's recorder did silently).
    fn record_result<T>(&mut self, result: Result<T, String>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                if !self.record_warned {
                    self.record_warned = true;
                    self.warn(
                        Message::new("the packets could not be recorded ({error}).").with("error", e),
                    );
                }
                None
            }
        }
    }

    /// Writes session.json as it is now.
    fn write_session(&mut self, sink: &mut dyn Sink) {
        if let Some(session) = &self.session {
            let written = self.recorder.session(sink, session);
            self.record_result(written);
        }
    }

    fn log_instructions(&mut self) {
        let title = self.options.extension.title.clone();
        self.say(Message::new("Rip the catalogue through G-Earth"));
        self.say(Message::new("This reads the catalogue the way your own client does: G-Earth relays the requests through your"));
        self.say(Message::new("logged-in session. Nothing is bought or changed; your game traffic passes through unchanged."));
        self.say(Message::new("  1. Start G-Earth AS ADMINISTRATOR (it edits the hosts file for the Flash/AIR client)."));
        self.say(Message::new("  2. With the client closed, press Connect in G-Earth; then open the client and log in."));
        self.say(
            Message::new("  3. G-Earth may ask whether to allow the extension \"{title}\": answer Yes.")
                .with("title", title),
        );
        self.say(Message::new("  4. Keep the catalogue window closed in your client while the rip runs."));
        let text = if self.options.adaptive_delay {
            Message::new(
                "Requests are sent one at a time, every {delay} ms (slower, up to {max} ms, when the hotel stops answering).",
            )
            .with("delay", self.timing.delay)
            .with("max", self.timing.max_delay)
        } else {
            Message::new("Requests are sent one at a time, every {delay} ms.")
                .with("delay", self.timing.delay)
        };
        self.say(text);
    }

    fn connect(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let now = io.clock.now_ms();
        let State::Connect { deadline } = &mut self.state else {
            unreachable!()
        };
        let deadline = *deadline.get_or_insert(now + self.options.handshake_timeout_ms);
        let Some(g) = self.gearth.as_mut() else {
            unreachable!()
        };
        let budget = end.min(deadline).saturating_sub(now);
        match g.handshake_step(
            &mut *io.transport,
            u32::try_from(budget).unwrap_or(u32::MAX),
        ) {
            Ok(true) => {
                let port = self.options.gearth_port;
                self.say(Message::new("Connected to G-Earth on port {port}.").with("port", port));
                self.emit(RipEvent::GearthConnected { port });
                self.set_phase(Phase::WaitHotel);
                self.state = State::WaitHotel {
                    deadline: now + self.timing.hotel_wait,
                    accepted: false,
                    last_note: now,
                };
                Ctl::Continue
            }
            Ok(false) if io.clock.now_ms() >= deadline => {
                let e = gearth::Error::NoInfoRequest;
                self.gearth = None;
                Ctl::End(Step::Failed(Failure {
                    message: Message::new(
                        "G-Earth did not answer on port {port} within {seconds} s ({error}): is it G-Earth's extension port?",
                    )
                    .with("port", self.options.gearth_port)
                    .with("seconds", secs(self.options.handshake_timeout_ms))
                    .with("error", &e),
                    cancelled: false,
                    gearth_error: Some(e),
                    reconnect: false,
                    not_gearth: true,
                    partial: None,
                }))
            }
            Ok(false) => Ctl::Yield,
            Err(e) => {
                let abort = self.gearth_lost(e);
                Ctl::End(Step::Failed(Failure {
                    message: Message::new("Connecting to G-Earth failed: {error}")
                        .with_message("error", abort.message.clone()),
                    not_gearth: true,
                    ..abort.failure()
                }))
            }
        }
    }

    fn wait_hotel(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        loop {
            let now = io.clock.now_ms();
            let State::WaitHotel {
                deadline,
                accepted,
                last_note,
            } = &mut self.state
            else {
                unreachable!()
            };
            let (deadline, was_accepted) = (*deadline, *accepted);
            if now >= deadline {
                return Ctl::End(Step::Failed(
                    Abort::new(
                        Message::new(
                            "No hotel connection through G-Earth within {seconds} s. Connect G-Earth to the hotel, then start your client and log in.",
                        )
                        .with("seconds", secs(self.timing.hotel_wait)),
                    )
                    .failure(),
                ));
            }
            if now.saturating_sub(*last_note) >= 30_000 {
                *last_note = now;
                self.say(
                    Message::new(
                        "Still waiting for the hotel connection ({seconds} s left): connect G-Earth, then open your client and log in.",
                    )
                    .with("seconds", secs(deadline - now)),
                );
            }
            if self.out_of_time(now, end) {
                return Ctl::Yield;
            }
            let wait = end.min(deadline).saturating_sub(now).min(5_000);
            let events = match self.pump(io, wait) {
                Ok((events, true)) => events,
                Ok((_, false)) => return Ctl::Yield,
                Err(abort) => {
                    let title = self.options.extension.title.clone();
                    return Ctl::End(Step::Failed(if was_accepted {
                        Failure {
                            message: Message::new(
                                "G-Earth let the extension go before the hotel connection ({error}; G-Earth restarted, or Connect in Nitro mode): connect again.",
                            )
                            .with_message("error", abort.message.clone()),
                            reconnect: true,
                            ..abort.failure()
                        }
                    } else {
                        Failure {
                            message: Message::new(
                                "G-Earth refused the extension ({error}). G-Earth asks once whether to allow an unknown extension; \
                                 if \"No\" was remembered, restart G-Earth and answer Yes when it asks about \"{title}\".",
                            )
                            .with_message("error", abort.message.clone())
                            .with("title", title),
                            ..abort.failure()
                        }
                    }));
                }
            };
            let mut events = events.into_iter();
            while let Some(event) = events.next() {
                match event {
                    GEarthEvent::Init { connected } => {
                        if let State::WaitHotel { accepted, .. } = &mut self.state {
                            *accepted = true;
                        }
                        let title = self.options.extension.title.clone();
                        self.say(
                            Message::new(
                                "G-Earth accepted the extension (shown in its Extensions tab as \"{title}\").",
                            )
                            .with("title", title),
                        );
                        if !connected {
                            self.say(Message::new("G-Earth is not connected to a hotel yet: press Connect in G-Earth, then open your client and log in."));
                        }
                    }
                    GEarthEvent::ConnectionStart(info) => {
                        // What came after it is for the next phase.
                        self.backlog.extend(events);
                        return self.hotel_connected(io, info);
                    }
                    GEarthEvent::ConnectionEnd => {
                        self.say(Message::new("The hotel connection ended; waiting for a new one."))
                    }
                    GEarthEvent::Packet(_) => {}
                }
            }
        }
    }

    fn hotel_connected(&mut self, io: &mut JobIo, connection: ConnectionInfo) -> Ctl {
        self.say(
            Message::new("Hotel connection: {host} ({client} client, {version})")
                .with("host", &connection.host)
                .with("client", &connection.client_type)
                .with("version", &connection.hotel_version),
        );
        self.emit(RipEvent::HotelConnected {
            host: connection.host.clone(),
            client_type: connection.client_type.clone(),
            hotel_version: connection.hotel_version.clone(),
        });
        match connection.client_type.to_ascii_uppercase().as_str() {
            "SHOCKWAVE" => return Ctl::End(Step::Failed(Abort::new(Message::new("The Shockwave (Origins) client uses another protocol and has no catalogue pages to rip; use the Flash/AIR or Nitro client.")).failure())),
            "UNITY" => self.warn(Message::new("the Unity client is not tested; trying with the Flash structures.")),
            _ => {}
        }
        if let Some(recorded) = &self.recorded {
            if !recorded.session.host.eq_ignore_ascii_case(&connection.host) {
                return Ctl::End(Step::Failed(
                    Abort::new(
                        Message::new(
                            "{folder} was ripped from {ripped}, but G-Earth is connected to {connected}: connect to the same hotel to go on with it.",
                        )
                        .with("folder", &recorded.folder)
                        .with("ripped", &recorded.session.host)
                        .with("connected", &connection.host),
                    )
                    .failure(),
                ));
            }
        }
        let choice = std::mem::take(&mut self.options.profile);
        let profile = match (&self.recorded, choice) {
            // Go on with what worked: the recorded profile and ids.
            (Some(recorded), ProfileChoice::Detect(_)) => {
                let mut profile = recorded.session.profile.clone();
                if let Some(h) = recorded.session.headers {
                    profile.headers = HeaderMap::new(h.get_index, h.get_page, h.index, h.page);
                }
                self.say(
                    Message::new("Hotel profile: {name} (from the stopped rip).")
                        .with("name", &profile.name),
                );
                self.emit(RipEvent::ProfileChosen {
                    name: profile.name.clone(),
                    kind: profile.kind,
                    detected_by: None,
                });
                profile
            }
            (_, choice) => self.choose_profile(choice, &connection),
        };
        let (partial, sources) = packets::resolve_partial(
            &connection.packets,
            &profile.headers,
            &self.options.header_overrides,
        );
        self.profile = Some(profile);
        self.connection = Some(connection);
        if partial.iter().all(Option::is_some) || !self.options.learn_headers {
            let connection = self.connection.as_ref().expect("connection");
            let profile = self.profile.as_ref().expect("profile");
            return match packets::resolve_headers(
                &connection.packets,
                &profile.headers,
                &self.options.header_overrides,
                &profile.name,
            ) {
                Ok(resolved) => self.headers_found(io, resolved.headers, resolved.sources),
                Err(e) => Ctl::End(Step::Failed(Abort::new(Message::verbatim(format!("{e:#}"))).failure())),
            };
        }
        // Learn the missing ids from the client while the user opens the catalogue.
        let learner = Learner::new(partial);
        let missing = learner.missing();
        self.say(
            Message::new("No hotel profile and no G-Earth packet info has the id of {missing}.")
                .with("missing", missing.join(", ")),
        );
        self.say(
            Message::new(
                "Open the catalogue in your client now and click any page: the ripper learns the ids from it (waiting up to {seconds} s).",
            )
            .with("seconds", secs(self.timing.hotel_wait)),
        );
        self.emit(RipEvent::LearningHeaders { missing });
        if let Some(g) = self.gearth.as_mut() {
            g.watch_all(true);
        }
        self.set_phase(Phase::Learn);
        self.state = State::Learn {
            learner,
            deadline: io.clock.now_ms() + self.timing.hotel_wait,
            partial,
            sources,
        };
        Ctl::Continue
    }

    fn fallback_profile(connection: &ConnectionInfo) -> HotelProfile {
        let kind = if connection.client_type.eq_ignore_ascii_case("NITRO") {
            HotelKind::NitroRetro
        } else {
            HotelKind::FlashRetro
        };
        HotelProfile {
            name: connection.host.clone(),
            kind,
            hosts: Vec::new(),
            revisions: Vec::new(),
            headers: HeaderMap::default(),
            catalog_url: None,
            icon_url: None,
            furnidata_url: None,
            productdata_url: None,
            site_config_url: None,
            extra_headers: BTreeMap::new(),
            builders_club: true,
            notes: String::new(),
            builtin: false,
            file: None,
        }
    }

    fn choose_profile(
        &mut self,
        choice: ProfileChoice,
        connection: &ConnectionInfo,
    ) -> HotelProfile {
        let (profile, detected_by) = match choice {
            ProfileChoice::Fixed(profile) => {
                self.say(
                    Message::new("Hotel profile: {name} ({kind}).")
                        .with("name", &profile.name)
                        .with_message("kind", profile.kind.label()),
                );
                (profile, None)
            }
            ProfileChoice::Detect(list) => match profiles::detect(&list, connection) {
                Some((profile, by)) => {
                    let how = match by {
                        DetectedBy::Host => {
                            Message::new("host {host}").with("host", &connection.host)
                        }
                        DetectedBy::Revision => Message::new("client revision {version}")
                            .with("version", &connection.hotel_version),
                        DetectedBy::ClientType => Message::new("{client} client")
                            .with("client", &connection.client_type),
                    };
                    self.say(
                        Message::new("Hotel profile: {name} ({kind}), picked by the {how}.")
                            .with("name", &profile.name)
                            .with_message("kind", profile.kind.label())
                            .with_message("how", how),
                    );
                    (profile.clone(), Some(by))
                }
                None => {
                    self.say(
                        Message::new(
                            "No hotel profile matches {host} ({client} client {version}): using G-Earth's packet info.",
                        )
                        .with("host", &connection.host)
                        .with("client", &connection.client_type)
                        .with("version", &connection.hotel_version),
                    );
                    (Self::fallback_profile(connection), None)
                }
            },
        };
        self.emit(RipEvent::ProfileChosen {
            name: profile.name.clone(),
            kind: profile.kind,
            detected_by,
        });
        profile
    }

    fn learn(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        loop {
            let State::Learn {
                learner, deadline, ..
            } = &self.state
            else {
                unreachable!()
            };
            if learner.headers().is_some() {
                return self.learned(io);
            }
            let deadline = *deadline;
            let now = io.clock.now_ms();
            if now >= deadline {
                let missing = learner.missing().join(", ");
                return Ctl::End(Step::Failed(
                    Abort::new(
                        Message::new(
                            "The header ids could not be learned within {seconds} s (still missing: {missing}). Open the catalogue in your client and click a page while the ripper waits, or set the ids in the hotel profile.",
                        )
                        .with("seconds", secs(self.timing.hotel_wait))
                        .with("missing", missing),
                    )
                    .failure(),
                ));
            }
            if self.out_of_time(now, end) {
                return Ctl::Yield;
            }
            let events = match self.pump(io, end.min(deadline).saturating_sub(now)) {
                Ok((events, true)) => events,
                Ok((_, false)) => return Ctl::Yield,
                Err(abort) => return Ctl::End(Step::Failed(abort.failure())),
            };
            let mut events = events.into_iter();
            while let Some(event) = events.next() {
                match event {
                    GEarthEvent::Packet(p) => {
                        let State::Learn { learner, .. } = &mut self.state else {
                            unreachable!()
                        };
                        if let Some(name) = learner.observe(p.to_server, &p.packet) {
                            self.say(
                                Message::new("Learned {name} = {header}.")
                                    .with("name", name)
                                    .with("header", p.packet.header),
                            );
                        }
                        let State::Learn { learner, .. } = &self.state else {
                            unreachable!()
                        };
                        if learner.headers().is_some() {
                            // What came after it is for the requests (an answer to one, say).
                            self.backlog.extend(events);
                            break;
                        }
                    }
                    GEarthEvent::ConnectionEnd => {
                        return Ctl::End(Step::Failed(
                            Abort::new(Message::new(
                                "the hotel connection ended (logged out or disconnected)",
                            ))
                            .failure(),
                        ))
                    }
                    _ => {}
                }
            }
        }
    }

    fn learned(&mut self, io: &mut JobIo) -> Ctl {
        let State::Learn {
            learner,
            partial,
            mut sources,
            ..
        } = std::mem::replace(&mut self.state, State::Start)
        else {
            unreachable!()
        };
        if let Some(g) = self.gearth.as_mut() {
            g.watch_all(false);
        }
        let headers = learner.headers().expect("learned ids complete");
        for (i, source) in sources.iter_mut().enumerate() {
            if partial[i].is_none() {
                *source = "learned";
            }
        }
        let connection = self.connection.as_ref().expect("connection");
        let profile = self.profile.as_mut().expect("profile");
        // Remember them: in the profile used from now on, and as a profile file.
        profile.headers = learner.known();
        if !profile
            .hosts
            .iter()
            .any(|h| profiles::host_matches(h, &connection.host))
        {
            profile.name = format!("{} (learned)", connection.host);
            profile.hosts = vec![connection.host.clone()];
            profile.builtin = false;
        }
        match profiles::save_profile(profile) {
            Ok(file) => self.learned_profile = Some(file),
            Err(e) => self.warn(
                Message::new("the learned profile could not be saved: {error}")
                    .with("error", format!("{e:#}")),
            ),
        }
        self.learned = true;
        self.emit(RipEvent::HeadersLearned {
            headers,
            saved_to: None,
        });
        self.headers_found(io, headers, sources)
    }

    fn headers_found(
        &mut self,
        io: &mut JobIo,
        headers: Headers,
        sources: [&'static str; 4],
    ) -> Ctl {
        self.say(
            Message::new(
                "Header ids: GetCatalogIndex {get_index} ({source_get_index}), GetCatalogPage {get_page} ({source_get_page}), CatalogIndex {index} ({source_index}), CatalogPage {page} ({source_page})",
            )
            .with("get_index", headers.get_index)
            .with("source_get_index", sources[0])
            .with("get_page", headers.get_page)
            .with("source_get_page", sources[1])
            .with("index", headers.index)
            .with("source_index", sources[2])
            .with("page", headers.page)
            .with("source_page", sources[3]),
        );
        self.emit(RipEvent::HeadersResolved { headers, sources });
        self.headers = Some(headers);
        // The modes to rip, in order.
        let profile = self.profile.as_ref().expect("profile");
        let mut modes = Vec::new();
        if self.options.normal {
            modes.push("NORMAL".to_string());
        }
        let mut skipped = None;
        if self.options.builders_club {
            if profile.builders_club {
                modes.push("BUILDERS_CLUB".to_string());
            } else {
                skipped = Some(
                    Message::new("the profile \"{name}\" has no Builders Club catalogue")
                        .with("name", &profile.name),
                );
            }
        }
        if let Some(reason) = skipped {
            self.emit(RipEvent::ModeSkipped {
                mode: "BUILDERS_CLUB".into(),
                reason: reason.to_string(),
            });
            self.say(
                Message::new("Only the normal catalogue is ripped: {reason}.")
                    .with_message("reason", reason),
            );
        }
        if modes.is_empty() {
            return Ctl::End(Step::Failed(Abort::new(Message::new("Nothing to rip: this hotel has no Builders Club catalogue and the normal one is turned off.")).failure()));
        }
        self.mode_list = modes;
        let console = format!(
            "[green]{}: ripping the catalogue (read-only)",
            self.options.extension.title
        );
        let Some(g) = self.gearth.as_mut() else {
            return Ctl::End(Step::Failed(
                Abort::new(Message::new("the connection to G-Earth ended")).failure(),
            ));
        };
        g.watch(false, headers.index);
        g.watch(false, headers.page);
        g.watch(true, headers.get_index);
        g.watch(true, headers.get_page);
        if let Err(e) = g.console_log(&mut *io.transport, &console) {
            return Ctl::End(Step::Failed(self.gearth_lost(e).failure()));
        }
        let connection = self.connection.as_ref().expect("connection");
        self.folder = self.new_folder(&connection.host, io);
        // The packets are recorded as they come.
        let resumed = self.recorded.is_some();
        self.recorder = Recorder::new(&self.folder, self.options.record_packets || resumed);
        let profile = self.profile.as_ref().expect("profile");
        self.session = Some(SessionInfo {
            format: recording::FORMAT,
            host: connection.host.clone(),
            port: connection.port,
            hotel_version: connection.hotel_version.clone(),
            client_type: connection.client_type.clone(),
            profile: HotelProfile {
                builtin: false,
                file: None,
                ..profile.clone()
            },
            headers: Some(headers),
            modes: Vec::new(),
            failed: BTreeMap::new(),
            complete: false,
            ripped_at: Timestamp::new(io.clock.unix_secs()).date_time(),
        });
        self.write_session(&mut *io.sink);
        self.rip_started = io.clock.now_ms();
        self.set_phase(Phase::Index);
        if let Some(recorded) = self.recorded.as_mut() {
            for mode in &self.mode_list {
                if let Some(index) = recorded.indexes.remove(mode) {
                    self.indexes.insert(mode.clone(), index);
                }
            }
            for (key, page) in recorded.pages.drain() {
                if self.mode_list.contains(&key.0) {
                    self.pages.insert(key, page);
                    self.resumed_pages += 1;
                }
            }
            self.extra_answers = std::mem::take(&mut recorded.extras);
            // What the recording has was answered: a request kind it holds is not one the hotel
            // never answered (the built-in would wait for a first answer of the next catalogue's
            // index the whole hotel wait, then stop).
            if !self.indexes.is_empty() {
                self.answered.insert(headers.get_index);
            }
            if self.resumed_pages > 0 {
                self.answered.insert(headers.get_page);
            }
            let folder = recorded.folder.clone();
            let indexes = self.indexes.len();
            self.say(
                Message::new(
                    "Going on with {folder}: {indexes} catalogue indexes and {pages} pages are already there; the rest is asked.",
                )
                .with("folder", &folder)
                .with("indexes", indexes)
                .with("pages", n0(self.resumed_pages)),
            );
            self.emit(RipEvent::Resumed {
                folder: PathBuf::from(folder),
                indexes,
                pages: self.resumed_pages,
            });
            self.state = State::Copy { next: 0 };
            return Ctl::Continue;
        }
        self.state = State::Index { i: 0 };
        Ctl::Continue
    }

    // ------------------------------------------------------------ requesting

    fn have(&self, want: &Want) -> bool {
        match want {
            Want::Index(mode) => self.indexes.contains_key(mode),
            Want::Page(mode, id) => {
                self.pages.contains_key(&(mode.clone(), *id))
                    || self.unreadable.contains_key(&(mode.clone(), *id))
            }
            Want::Raw(header) => self.raw.contains_key(header),
        }
    }

    /// What G-Earth reported, while asking the hotel.
    fn handle(&mut self, sink: &mut dyn Sink, event: GEarthEvent) -> Result<(), Abort> {
        let headers = self.headers.expect("headers");
        match event {
            GEarthEvent::Packet(p) if !p.to_server && self.raw_watch.contains(&p.packet.header) => {
                self.raw.insert(p.packet.header, p.packet.body);
            }
            GEarthEvent::Packet(p) if !p.to_server && p.packet.header == headers.index => {
                match packets::parse_index(&p.packet.body) {
                    Ok(mut index) => {
                        if index.mode.is_empty() {
                            index.mode = self.index_mode.clone();
                        }
                        let mode = index.mode.to_ascii_uppercase();
                        // An answer to the client's own request before any of ours: no mode.
                        if !mode.is_empty() {
                            let written = self.recorder.index(sink, &mode, &p.packet.body);
                            self.record_result(written);
                            self.indexes.insert(mode, index);
                        }
                    }
                    Err(e) => self.warn(
                        Message::new("a catalogue index could not be read: {error}")
                            .with("error", format!("{e:#}")),
                    ),
                }
            }
            GEarthEvent::Packet(p) if !p.to_server && p.packet.header == headers.page => {
                match packets::parse_page(&p.packet.body) {
                    Ok(mut page) => {
                        if page.mode.is_empty() {
                            page.mode = self.page_mode.clone();
                        }
                        let mode = page.mode.to_ascii_uppercase();
                        if !mode.is_empty() {
                            let written =
                                self.recorder.page(sink, &mode, page.page_id, &p.packet.body, false);
                            self.record_result(written);
                            self.pages.insert((mode, page.page_id), page);
                        }
                    }
                    Err(e) => {
                        if let Some((id, mut mode)) = packets::page_key(&p.packet.body) {
                            if mode.is_empty() {
                                mode = self.page_mode.clone();
                            }
                            let mode = mode.to_ascii_uppercase();
                            // Keep the raw packet so the layout can be worked out from real data.
                            let written = self.recorder.page(sink, &mode, id, &p.packet.body, true);
                            if let Some(Some(file)) = self.record_result(written) {
                                self.say(
                                    Message::new("  raw page saved: {file}").with("file", file),
                                );
                            }
                            self.unreadable.insert((mode, id), format!("{e:#}"));
                        } else {
                            self.warn(
                                Message::new("a catalogue page could not be read: {error}")
                                    .with("error", format!("{e:#}")),
                            );
                        }
                    }
                }
            }
            GEarthEvent::Packet(p) if p.to_server => {
                self.client_sent.insert(p.packet.header);
            }
            GEarthEvent::Packet(_) | GEarthEvent::Init { .. } | GEarthEvent::ConnectionStart(_) => {
            }
            GEarthEvent::ConnectionEnd => {
                return Err(Abort::new(Message::new("the hotel connection ended (logged out or disconnected; if the hotel kicks clients that ask too fast, raise the delay between requests)")));
            }
        }
        Ok(())
    }

    /// Pumps for at most `ms` and handles what came. `Ok(false)`: nothing happened (yield).
    fn pump_handle(&mut self, io: &mut JobIo, ms: u64) -> Result<bool, Abort> {
        let (events, progressed) = self.pump(io, ms)?;
        for event in events {
            self.handle(&mut *io.sink, event)?;
        }
        Ok(progressed)
    }

    fn set_delay(&mut self, delay: u64, why: Message) {
        if delay == self.delay {
            return;
        }
        self.delay = delay;
        self.say(
            Message::new("{why}: {delay} ms between requests now.")
                .with_message("why", why)
                .with("delay", delay),
        );
        self.emit(RipEvent::DelayChanged { delay_ms: delay });
    }

    /// A request kind the server answers before went unanswered: maybe it is throttling.
    fn slow_down(&mut self) {
        self.answered_in_a_row = 0;
        if self.options.adaptive_delay && self.delay < self.timing.max_delay {
            let slower = (self.delay * 2).min(self.timing.max_delay);
            self.set_delay(slower, Message::new("A request went unanswered, slowing down"));
        }
    }

    fn answered_one(&mut self) {
        self.answered_in_a_row += 1;
        if self.options.adaptive_delay
            && self.delay > self.timing.delay
            && self.answered_in_a_row >= SPEED_UP_AFTER
        {
            self.answered_in_a_row = 0;
            let faster = (self.delay / 2).max(self.timing.delay);
            self.set_delay(faster, Message::new("Answers come back again, speeding up"));
        }
    }

    fn begin(&mut self, want: Want, packet: Packet, label: Message, once: bool) {
        match &want {
            Want::Index(mode) => self.index_mode = mode.clone(),
            Want::Page(mode, _) => self.page_mode = mode.clone(),
            Want::Raw(_) => {}
        }
        self.req = Some(Request {
            want,
            packet,
            label,
            looked: false,
            stage: Stage::Send(if once {
                WaitKind::Once
            } else {
                WaitKind::First
            }),
        });
    }

    /// Drives the current request (see [`Job::request_step`]); clears it once it has an
    /// outcome.
    fn drive(&mut self, io: &mut JobIo, end: u64) -> Poll<Result<bool, Abort>> {
        let Some(mut req) = self.req.take() else {
            return Poll::Ready(Ok(false));
        };
        let result = self.request_step(io, end, &mut req);
        if let Poll::Pending = result {
            self.req = Some(req);
        }
        result
    }

    /// Sends a request and waits for its answer. The first time a request kind gets no answer,
    /// the user is told what to do and it is asked again until `hotel_wait` runs out; after
    /// that, a missing answer is retried once and then given up (`false`). Requests are sent no
    /// faster than the delay, by the clock; meanwhile G-Earth is pumped.
    fn request_step(
        &mut self,
        io: &mut JobIo,
        end: u64,
        req: &mut Request,
    ) -> Poll<Result<bool, Abort>> {
        let header = req.packet.header;
        loop {
            let now = io.clock.now_ms();
            // How long to wait (pump) before looking again; None: look again at once.
            let wait_until: u64 = match req.stage {
                Stage::Send(kind) => {
                    if !req.looked {
                        req.looked = true;
                        if self.have(&req.want) {
                            return Poll::Ready(Ok(true));
                        }
                    }
                    let next = self.last_send.map_or(now, |last| last + self.delay);
                    if now >= next {
                        let Some(g) = self.gearth.as_mut() else {
                            return Poll::Ready(Err(Abort::new(Message::new(
                                "the connection to G-Earth ended",
                            ))));
                        };
                        if let Err(e) = g.send_to_server(&mut *io.transport, &req.packet) {
                            return Poll::Ready(Err(self.gearth_lost(e)));
                        }
                        self.last_send = Some(now);
                        req.stage = Stage::Wait {
                            until: now + self.timing.response_timeout,
                            kind,
                        };
                        continue;
                    }
                    next
                }
                Stage::Wait { until, kind } => {
                    if self.have(&req.want) {
                        match kind {
                            WaitKind::First => {
                                self.answered.insert(header);
                                self.answered_one();
                            }
                            WaitKind::AfterClient | WaitKind::Resend { .. } => {
                                self.answered.insert(header);
                            }
                            WaitKind::Retry | WaitKind::Once => {}
                        }
                        return Poll::Ready(Ok(true));
                    }
                    if now >= until {
                        match kind {
                            WaitKind::First if self.answered.contains(&header) => {
                                self.slow_down();
                                self.say(
                                    Message::new("No answer for {label} within {seconds} s, asking once more.")
                                        .with_message("label", req.label.clone())
                                        .with(
                                            "seconds",
                                            format!("{:.1}", self.timing.response_timeout as f64 / 1000.0),
                                        ),
                                );
                                req.stage = Stage::Send(WaitKind::Retry);
                            }
                            WaitKind::First => {
                                // Never answered yet: not logged in yet, wrong header ids, or
                                // G-Earth blocks the packet.
                                let label = req.label.clone();
                                self.say(
                                    Message::new("The hotel did not answer the {label} request yet.")
                                        .with_message("label", label),
                                );
                                self.say(Message::new("  - Make sure your client is logged in (in the hotel view or a room)."));
                                self.say(Message::new("  - G-Earth only forwards packets it knows: if its packet info did not load, open the catalogue in"));
                                self.say(Message::new("    your client once and click any page (or turn on Developer mode in G-Earth's Extra tab)."));
                                self.say(Message::new(
                                    "  - On a retro, check the header ids of the hotel profile.",
                                ));
                                self.say(
                                    Message::new(
                                        "  Waiting up to {wait} s; the request is repeated every {every} s.",
                                    )
                                    .with("wait", secs(self.timing.hotel_wait))
                                    .with("every", secs(self.timing.resend_every)),
                                );
                                self.client_sent.remove(&header);
                                req.stage = Stage::Patient {
                                    end: now + self.timing.hotel_wait,
                                    until: None,
                                };
                            }
                            WaitKind::Retry | WaitKind::Once => return Poll::Ready(Ok(false)),
                            WaitKind::AfterClient => {
                                self.answered.insert(header);
                                return Poll::Ready(Ok(false));
                            }
                            WaitKind::Resend { end } => {
                                req.stage = Stage::Patient { end, until: None };
                            }
                        }
                        continue;
                    }
                    until
                }
                Stage::Patient {
                    end: patience,
                    until,
                } => {
                    let until = match until {
                        Some(until) => until,
                        None => {
                            if now >= patience {
                                return Poll::Ready(Err(Abort::new(
                                    Message::new(
                                        "The hotel never answered the {label} request (waited {seconds} s). On a retro, check the hotel profile's header ids.",
                                    )
                                    .with_message("label", req.label.clone())
                                    .with("seconds", secs(self.timing.hotel_wait)),
                                )));
                            }
                            let until = (now + self.timing.resend_every).min(patience);
                            req.stage = Stage::Patient {
                                end: patience,
                                until: Some(until),
                            };
                            until
                        }
                    };
                    if self.have(&req.want) {
                        self.answered.insert(header);
                        return Poll::Ready(Ok(true));
                    }
                    if self.client_sent.remove(&header) {
                        // The client sent this request itself, so G-Earth lets it through now.
                        req.stage = Stage::Send(WaitKind::AfterClient);
                        continue;
                    }
                    if now >= until {
                        req.stage = Stage::Send(WaitKind::Resend { end: patience });
                        continue;
                    }
                    until
                }
            };
            if self.out_of_time(now, end) {
                return Poll::Pending;
            }
            match self.pump_handle(io, wait_until.min(end).saturating_sub(now)) {
                Ok(true) => {}
                Ok(false) => return Poll::Pending,
                Err(abort) => return Poll::Ready(Err(abort)),
            }
        }
    }

    fn index(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let State::Index { i } = self.state else {
            unreachable!()
        };
        if i >= self.mode_list.len() {
            return self.after_requests(io);
        }
        let key = self.mode_list[i].to_ascii_uppercase();
        if self.req.is_none() {
            self.say(
                Message::new("Requesting the {mode} catalogue index ...").with("mode", &key),
            );
            let headers = self.headers.expect("headers");
            self.begin(
                Want::Index(key.clone()),
                packets::get_catalog_index(headers.get_index, &self.mode_list[i]),
                Message::new("{mode} index").with("mode", &key),
                false,
            );
        }
        let result = match self.drive(io, end) {
            Poll::Pending => return Ctl::Yield,
            Poll::Ready(result) => result,
        };
        self.state = State::Index { i: i + 1 };
        match result {
            Ok(true) => {
                let index = &self.indexes[&key];
                // A server without a Builders Club catalogue may answer with the normal tree.
                let same = i > 0
                    && self
                        .accepted
                        .first()
                        .and_then(|first| self.indexes.get(first))
                        .is_some_and(|first| first.root == index.root);
                if same {
                    let reason = Message::new("the hotel sent the normal catalogue again");
                    self.emit(RipEvent::ModeSkipped {
                        mode: key.clone(),
                        reason: reason.to_string(),
                    });
                    self.say(
                        Message::new("{mode}: {reason} (no separate {mode} catalogue); skipped.")
                            .with("mode", &key)
                            .with_message("reason", reason),
                    );
                    return Ctl::Continue;
                }
                let count = index.walk().len();
                self.say(
                    Message::new("{mode}: {pages} pages in the index.")
                        .with("mode", &key)
                        .with("pages", n0(count)),
                );
                self.emit(RipEvent::IndexRead {
                    mode: key.clone(),
                    pages: count,
                });
                self.accepted.push(key);
            }
            Ok(false) if i == 0 => {
                self.aborted = Some(Abort::new(
                    Message::new("The hotel did not send the {mode} catalogue index.")
                        .with("mode", &key),
                ));
                return self.after_requests(io);
            }
            Ok(false) => {
                let reason = Message::new("no index (this account or hotel may not have it)");
                self.emit(RipEvent::ModeSkipped {
                    mode: key.clone(),
                    reason: reason.to_string(),
                });
                self.say(
                    Message::new("{mode}: {reason}; skipped.")
                        .with("mode", &key)
                        .with_message("reason", reason),
                );
            }
            Err(abort) => {
                self.aborted = Some(abort);
                return self.after_requests(io);
            }
        }
        Ctl::Continue
    }

    /// After the indexes: the page list (depth first, mode by mode).
    fn begin_pages(&mut self, sink: &mut dyn Sink) -> Ctl {
        if let Some(session) = self.session.as_mut() {
            session.modes = self.accepted.clone();
        }
        self.write_session(sink);
        let mut jobs = Vec::new();
        for (m, mode) in self.accepted.iter().enumerate() {
            let mut seen = HashSet::new();
            for (node, _, _) in self.indexes[mode].walk() {
                if node.page_id > 0 && seen.insert(node.page_id) {
                    let prefix = if mode == "NORMAL" {
                        String::new()
                    } else {
                        format!("{mode}: ")
                    };
                    let label = if node.localization.is_empty() {
                        node.name.clone()
                    } else {
                        format!("{} ({})", node.localization, node.name)
                    };
                    jobs.push((m, node.page_id, progress_label(&format!("{prefix}{label}"))));
                }
            }
        }
        self.jobs = jobs;
        self.set_phase(Phase::Pages);
        self.progress.total = self.jobs.len() as u64;
        self.say(
            Message::new("Requesting {pages} pages ...").with("pages", n0(self.jobs.len())),
        );
        self.state = State::Pages { n: 0 };
        Ctl::Continue
    }

    fn page(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let State::Pages { n } = self.state else {
            unreachable!()
        };
        let total = self.jobs.len();
        if n >= total {
            return self.begin_extras(io);
        }
        let (m, page_id, label) = self.jobs[n].clone();
        let mode = self.accepted[m].clone();
        if self.skip_mode == Some(m) {
            self.failed.entry(mode).or_default().push(page_id);
            self.state = State::Pages { n: n + 1 };
            return Ctl::Continue;
        }
        let key = (mode.clone(), page_id);
        if self.req.is_none() {
            if !self.pages.contains_key(&key) {
                self.say(
                    Message::new("[{n}/{total}] {label}")
                        .with("n", n + 1)
                        .with("total", total)
                        .with("label", &label),
                );
            }
            let headers = self.headers.expect("headers");
            self.begin(
                Want::Page(mode.clone(), page_id),
                packets::get_catalog_page(headers.get_page, page_id, &mode),
                Message::new("page {page}").with("page", page_id),
                false,
            );
        }
        let result = match self.drive(io, end) {
            Poll::Pending => return Ctl::Yield,
            Poll::Ready(result) => result,
        };
        match result {
            Ok(true) => {
                let ok = self.pages.contains_key(&key);
                if ok {
                    *self.read_in_mode.entry(m).or_default() += 1;
                } else if let Some(error) = self.unreadable.get(&key).cloned() {
                    self.say(
                        Message::new("  page {page} could not be read: {error}")
                            .with("page", page_id)
                            .with("error", error),
                    );
                    self.failed.entry(mode.clone()).or_default().push(page_id);
                }
                self.emit(RipEvent::PageDone { mode, page_id, ok });
            }
            Ok(false) => {
                self.say(
                    Message::new("  page {page}: no answer, skipped.").with("page", page_id),
                );
                let list = self.failed.entry(mode.clone()).or_default();
                list.push(page_id);
                let failed_here = list.len();
                self.emit(RipEvent::PageDone {
                    mode: mode.clone(),
                    page_id,
                    ok: false,
                });
                if m > 0 && !self.read_in_mode.contains_key(&m) && failed_here >= 3 {
                    let reason = Message::new("its first pages got no answer");
                    self.emit(RipEvent::ModeSkipped {
                        mode: mode.clone(),
                        reason: reason.to_string(),
                    });
                    self.say(
                        Message::new(
                            "{mode}: {reason}; the rest of the {mode} catalogue is skipped.",
                        )
                        .with("mode", &mode)
                        .with_message("reason", reason),
                    );
                    self.skip_mode = Some(m);
                }
            }
            Err(abort) => {
                self.aborted = Some(abort);
                return self.after_requests(io);
            }
        }
        self.progress.done = n as u64 + 1;
        self.progress.label = label;
        self.state = State::Pages { n: n + 1 };
        Ctl::Continue
    }

    fn begin_extras(&mut self, io: &mut JobIo) -> Ctl {
        if !self.options.rip_extras {
            return self.after_requests(io);
        }
        self.set_phase(Phase::Extras);
        self.state = State::Extras { i: 0 };
        Ctl::Continue
    }

    fn extra(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let State::Extras { i } = self.state else {
            unreachable!()
        };
        let Some(extra) = extras::EXTRAS.get(i) else {
            return self.after_requests(io);
        };
        if self.req.is_none() {
            if self.extra_answers.contains_key(extra.key) {
                self.state = State::Extras { i: i + 1 };
                return Ctl::Continue;
            }
            let profile = self.profile.as_ref().expect("profile");
            let connection = self.connection.as_ref().expect("connection");
            let Some((ask_id, answer_id)) =
                extras::resolve(extra, &profile.extra_headers, &connection.packets)
            else {
                self.say(
                    Message::new("{name}: no header ids for this hotel; skipped.")
                        .with("name", extra.answer),
                );
                self.state = State::Extras { i: i + 1 };
                return Ctl::Continue;
            };
            if let Some(g) = self.gearth.as_mut() {
                g.watch(false, answer_id);
            }
            self.raw_watch.insert(answer_id);
            self.begin(
                Want::Raw(answer_id),
                extras::request(extra, ask_id),
                Message::verbatim(extra.answer),
                true,
            );
        }
        let Some(Want::Raw(answer_id)) = self.req.as_ref().map(|r| r.want.clone()) else {
            unreachable!()
        };
        let result = match self.drive(io, end) {
            Poll::Pending => return Ctl::Yield,
            Poll::Ready(result) => result,
        };
        self.state = State::Extras { i: i + 1 };
        match result {
            Ok(true) => {
                let body = self.raw.remove(&answer_id).unwrap_or_default();
                let written = self.recorder.extra(&mut *io.sink, extra.key, &body);
                self.record_result(written);
                self.say(Message::new("{name}: read.").with("name", extra.answer));
                self.extra_answers.insert(extra.key.to_string(), body);
            }
            Ok(false) => self.say(
                Message::new("{name}: no answer; skipped.").with("name", extra.answer),
            ),
            Err(abort) => {
                self.aborted = Some(abort);
                return self.after_requests(io);
            }
        }
        Ctl::Continue
    }

    /// The requests are over (done, given up or cancelled): G-Earth is let go and the output
    /// is written from what was read.
    fn after_requests(&mut self, io: &mut JobIo) -> Ctl {
        self.req = None;
        if let State::Index { .. } = self.state {
            if self.aborted.is_none() {
                // Every index asked: on to the pages.
                return if self.accepted.is_empty() {
                    Ctl::End(Step::Failed(
                        Abort::new(Message::new("no catalogue index was received")).failure(),
                    ))
                } else {
                    self.begin_pages(&mut *io.sink)
                };
            }
        }
        if self.accepted.is_empty() {
            let abort = self
                .aborted
                .take()
                .unwrap_or_else(|| Abort::new(Message::new("no catalogue index was received")));
            return Ctl::End(Step::Failed(abort.failure()));
        }
        // The built-in drops its G-Earth session here, before writing the output.
        self.gearth = None;
        let now = io.clock.now_ms();
        self.elapsed_ms = now.saturating_sub(self.rip_started);
        let whole = self.aborted.is_none();
        let ripped_at = Timestamp::new(io.clock.unix_secs()).date_time();
        if let Some(session) = self.session.as_mut() {
            session.modes = self.accepted.clone();
            session.failed = self.failed.clone();
            session.complete = whole;
            session.ripped_at = ripped_at.clone();
        }
        self.write_session(&mut *io.sink);
        let connection = self.connection.as_ref().expect("connection");
        let catalog = output::assemble(
            &output::Source {
                host: &connection.host,
                hotel_version: &connection.hotel_version,
                client_type: &connection.client_type,
                ripped_at: &ripped_at,
                generator: &self.options.generator,
            },
            &self.accepted,
            std::mem::take(&mut self.indexes),
            std::mem::take(&mut self.pages),
            &self.failed,
            whole,
        );
        self.begin_output(catalog, io)
    }

    /// The catalogue is read: on to the output (the built-in's `Output::write`).
    fn begin_output(&mut self, catalog: RippedCatalog, io: &mut JobIo) -> Ctl {
        // The note goes into README.txt (English, as the built-in wrote it); the line is for the log.
        let (database_note, items_line) = match io.items {
            None => ("No items_base was given: item_ids are subqueries on items_base, resolved when the SQL is imported.".to_string(), None),
            Some(_) => ("item_ids were matched against the hotel's items_base; products it lacked are looked up again on import (subqueries).".to_string(), Some(Message::new("Using the hotel's items_base."))),
        };
        if let Some(line) = items_line {
            self.say(line);
        }
        self.out = Some(Out {
            catalog,
            database_note,
            with_items_base: io.items.is_some(),
            furnidata: None,
            products: None,
            furnidata_names: None,
            productdata_names: 0,
            data: DataExport::new(),
            changes: None,
            changes_summary: None,
            files: Vec::new(),
            catalog_url: String::new(),
            icon_url: String::new(),
            plan: Vec::new(),
            next_image: 0,
            next_url: 0,
            stats: ImageStats::default(),
            written: HashSet::new(),
            images_text: None,
        });
        self.set_phase(Phase::Export);
        self.state = State::Export(ExportStage::Furnidata);
        // Return now: the plugin closes the G-Earth connection after a step in which
        // `uses_gearth()` turned false, so the export's fetches (which may wait on the user, for an
        // `http:ask` host) never run while G-Earth holds the user's game packets for us.
        Ctl::Yield
    }

    // ------------------------------------------------------------ the output

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.folder)
    }

    /// Reads a furnidata / productdata source.
    fn fetch_source(io: &mut JobIo, source: &str) -> Result<Vec<u8>, String> {
        let source = source.trim();
        match io.fetch.get(source) {
            Ok(bytes) => Ok(bytes),
            Err(FetchError::NotFound) => Err(format!("{source} is not there")),
            Err(FetchError::Failed(e)) => Err(format!("GET {source}: {e}")),
        }
    }

    fn source_url(
        &self,
        option: &Option<String>,
        profile: &Option<String>,
        official: fn(&str) -> Option<String>,
    ) -> Option<String> {
        let host = &self.connection.as_ref().expect("connection").host;
        let kind = self.profile.as_ref().expect("profile").kind;
        option
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| profile.clone().filter(|s| !s.trim().is_empty()))
            .or_else(|| {
                (kind == HotelKind::Official)
                    .then(|| official(host))
                    .flatten()
            })
    }

    fn export(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let site_source = self.site_config_source();
        let can_fetch = self.may_fetch(io.clock.now_ms(), end);
        let State::Export(stage) = &mut self.state else {
            unreachable!()
        };
        match stage {
            ExportStage::Furnidata => {
                if !self.options.use_furnidata {
                    self.state = State::Export(ExportStage::Productdata);
                    return Ctl::Continue;
                }
                let profile_url = self.profile.as_ref().and_then(|p| p.furnidata_url.clone());
                let Some(source) = self.source_url(
                    &self.options.furnidata_url,
                    &profile_url,
                    furnidata::official_url,
                ) else {
                    self.state = State::Export(ExportStage::Productdata);
                    return Ctl::Continue;
                };
                if !can_fetch {
                    return Ctl::Yield;
                }
                self.state = State::Export(ExportStage::Productdata);
                self.fetched = true;
                let host = self.connection.as_ref().expect("connection").host.clone();
                self.say(
                    Message::new("Reading the furnidata of {host} ({source}) ...")
                        .with("host", &host)
                        .with("source", &source),
                );
                let catalog = &self.out.as_ref().expect("out").catalog;
                // Of the whole entries, only those of the furni items_base lacks are kept (for
                // missing_furnidata.json); the response is dropped at the end of this step.
                let loaded = Self::fetch_source(io, &source).and_then(|bytes| {
                    furnidata::load_keeping(&source, &bytes, |names| {
                        Some(missing::missing_furni_keys(catalog, io.items, names))
                    })
                    .map_err(|e| format!("{e:#}"))
                });
                match loaded {
                    Ok(data) => {
                        self.say(
                            Message::new(
                                "Furnidata: {count} class names; products are matched by class name.",
                            )
                            .with("count", n0(data.len())),
                        );
                        let out = self.out.as_mut().expect("out");
                        out.furnidata_names = Some(data.len());
                        out.furnidata = Some(data);
                    }
                    Err(e) => self.warn(
                        Message::new(
                            "the furnidata could not be read ({error}); products are matched by sprite id.",
                        )
                        .with("error", e),
                    ),
                }
                // One download and parse per step.
                Ctl::Yield
            }
            ExportStage::Productdata => {
                if !self.options.use_furnidata {
                    self.state = State::Export(ExportStage::Diff(DiffStage::Start));
                    return Ctl::Continue;
                }
                let profile_url = self
                    .profile
                    .as_ref()
                    .and_then(|p| p.productdata_url.clone());
                let Some(source) = self.source_url(
                    &self.options.productdata_url,
                    &profile_url,
                    productdata::official_url,
                ) else {
                    self.state = State::Export(ExportStage::Diff(DiffStage::Start));
                    return Ctl::Continue;
                };
                if !can_fetch {
                    return Ctl::Yield;
                }
                self.state = State::Export(ExportStage::Diff(DiffStage::Start));
                self.fetched = true;
                let loaded = Self::fetch_source(io, &source).and_then(|bytes| {
                    productdata::load(&source, &bytes).map_err(|e| format!("{e:#}"))
                });
                match loaded {
                    Ok(data) => {
                        self.say(
                            Message::new("Productdata: {count} product names.")
                                .with("count", n0(data.len())),
                        );
                        let out = self.out.as_mut().expect("out");
                        out.productdata_names = data.len();
                        out.products = Some(data);
                    }
                    Err(e) => self.warn(
                        Message::new("the productdata could not be read ({error}).").with("error", e),
                    ),
                }
                Ctl::Yield
            }
            ExportStage::Diff(_) => self.diff(io, end),
            ExportStage::Data => self.write_data(io),
            ExportStage::SiteConfig {
                urls,
                next,
                fetched,
            } => {
                let source = site_source.expect("site config");
                if let Some(url) = urls.get(*next).cloned() {
                    if !can_fetch {
                        return Ctl::Yield;
                    }
                    *next += 1;
                    self.fetched = true;
                    match io.fetch.get(&url) {
                        Ok(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            let trimmed = text.trim_start_matches('\u{feff}').trim_start();
                            // external_variables: the rest is not needed.
                            if !trimmed.starts_with('{') && trimmed.contains('=') {
                                *next = urls.len();
                            }
                            fetched.push((url, bytes));
                        }
                        Err(FetchError::NotFound) => {}
                        Err(FetchError::Failed(e)) => {
                            self.warn(
                                Message::new(
                                    "the client configuration could not be read (GET {url}: {error}); the image URLs of the profile are used.",
                                )
                                .with("url", &url)
                                .with("error", e),
                            );
                            return self.begin_images(None);
                        }
                    }
                    return Ctl::Continue;
                }
                let fetched = std::mem::take(fetched);
                match site_config::load(&source, &fetched) {
                    Ok(urls) => {
                        self.say(
                            Message::new(
                                "Catalogue images from the client configuration: {catalogue} (icons {icons}<n>.png).",
                            )
                            .with("catalogue", &urls.catalog_url)
                            .with("icons", &urls.icon_url),
                        );
                        self.begin_images(Some((urls.catalog_url, urls.icon_url)))
                    }
                    Err(e) => {
                        self.warn(
                            Message::new(
                                "the client configuration could not be read ({error}); the image URLs of the profile are used.",
                            )
                            .with("error", format!("{e:#}")),
                        );
                        self.begin_images(None)
                    }
                }
            }
        }
    }

    fn site_config_source(&self) -> Option<String> {
        self.options
            .site_config_url
            .clone()
            .or_else(|| {
                self.profile
                    .as_ref()
                    .and_then(|p| p.site_config_url.clone())
            })
            .filter(|s| !s.trim().is_empty())
    }

    /// Compares with an earlier rip (before the data files, while the items_base and the
    /// furnidata are there): finds it, reads its catalog.json in pieces into the rows it had,
    /// then compares (a step of its own).
    fn diff(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let State::Export(ExportStage::Diff(stage)) =
            std::mem::replace(&mut self.state, State::Export(ExportStage::Data))
        else {
            unreachable!()
        };
        let mut stage = stage;
        if self.cancel {
            // Cancelled: the files are written without the comparison.
            return Ctl::Continue;
        }
        let mut more = within(io.clock, end, PIECES_PER_STEP);
        let mut first = true;
        loop {
            if !first && !more() {
                self.state = State::Export(ExportStage::Diff(stage));
                return Ctl::Yield;
            }
            first = false;
            let has_source = io.source.is_some();
            stage = match stage {
                DiffStage::Start => match (&self.options.diff_against, has_source) {
                    (Some(earlier), true) => DiffStage::Read {
                        earlier: earlier.trim_end_matches('/').to_string(),
                        offset: 0,
                        digest: Box::default(),
                        found: None,
                    },
                    (Some(earlier), false) => {
                        self.warn(
                            Message::new(
                                "the earlier rip {earlier} cannot be compared: there is no folder to read it from",
                            )
                            .with("earlier", earlier),
                        );
                        return Ctl::Continue;
                    }
                    (None, true) if self.options.diff_with_previous => {
                        let hotel = &self.out.as_ref().expect("out").catalog.hotel;
                        let mut skip = vec![self.folder.clone()];
                        if let Begin::Resume(folder) | Begin::Reexport(folder) = &self.begin {
                            skip.push(folder.trim_end_matches('/').to_string());
                        }
                        DiffStage::Find(PreviousRip::new(hotel, skip))
                    }
                    _ => return Ctl::Continue,
                },
                DiffStage::Find(mut find) => {
                    let source = io.source.as_deref_mut().expect("source");
                    match find.step(source) {
                        Ok(None) => DiffStage::Find(find),
                        Ok(Some(Some(earlier))) => DiffStage::Read {
                            earlier,
                            offset: 0,
                            digest: Box::default(),
                            found: Some(find),
                        },
                        // None there (or the folder cannot be listed): nothing to compare.
                        Ok(Some(None)) | Err(_) => return Ctl::Continue,
                    }
                }
                DiffStage::Read {
                    earlier,
                    offset,
                    mut digest,
                    found,
                } => {
                    let source = io.source.as_deref_mut().expect("source");
                    let path = diff::catalog_json_path(&earlier);
                    let furnidata = self.out.as_ref().and_then(|out| out.furnidata.as_ref());
                    let read = source
                        .read(&path, offset, READ_PIECE)
                        .map_err(|e| format!("reading {path}: {e}"))
                        .and_then(|data| {
                            let eof = data.is_empty();
                            digest
                                .feed(&data, eof, io.items, furnidata)
                                .map_err(|e| format!("{path}: {e}"))
                                .map(|()| (data.len() as u64, eof))
                        });
                    let (read, digest) = match read {
                        Ok((_, true)) => (digest.finish().map(Ok), None),
                        Ok((n, false)) => (Ok(Err(n)), Some(digest)),
                        Err(e) => (Err(e), None),
                    };
                    match read {
                        Ok(Ok(rows)) => {
                            // The comparison in a step of its own.
                            self.state = State::Export(ExportStage::Diff(DiffStage::Compare {
                                earlier,
                                rows,
                            }));
                            return Ctl::Yield;
                        }
                        Ok(Err(n)) => DiffStage::Read {
                            earlier,
                            offset: offset + n,
                            digest: digest.expect("reading on"),
                            found,
                        },
                        Err(e) => {
                            match found {
                                // Found by looking: the next earlier one, if any.
                                Some(find) => {
                                    self.warn(
                                        Message::new(
                                            "the earlier rip {earlier} cannot be compared ({error}); looking for an earlier one.",
                                        )
                                        .with("earlier", &earlier)
                                        .with("error", e),
                                    );
                                    DiffStage::Find(find)
                                }
                                None => {
                                    self.warn(
                                        Message::new(
                                            "the earlier rip {earlier} cannot be compared: {error}",
                                        )
                                        .with("earlier", &earlier)
                                        .with("error", e),
                                    );
                                    return Ctl::Continue;
                                }
                            }
                        }
                    }
                }
                DiffStage::Compare { earlier, rows } => {
                    let out = self.out.as_mut().expect("out");
                    out.changes = Some(diff::compare(
                        rows,
                        &earlier,
                        &out.catalog,
                        io.items,
                        out.furnidata.as_ref(),
                        self.options.diff_delete_removed,
                    ));
                    return Ctl::Yield;
                }
            };
        }
    }

    /// The next data file (one per step: each is streamed straight from the catalogue to the
    /// sink in pieces of about 1 MiB, but the biggest take a good part of a tick's time); once
    /// all are written, on to the images.
    fn write_data(&mut self, io: &mut JobIo) -> Ctl {
        let out = self.out.as_mut().expect("out");
        let folder = &self.folder;
        let sink = &mut *io.sink;
        let made = out.data.stream_next_file(
            &output::ExportInput {
                catalog: &out.catalog,
                items: io.items,
                furnidata: out.furnidata.as_ref(),
                products: out.products.as_ref(),
                manager_mode: self.options.manager_mode,
                existing: io.existing,
                extra_answers: &self.extra_answers,
                changes: out.changes.as_ref(),
            },
            &mut |name, data| sink.append(&format!("{folder}/{name}"), data),
        );
        if let Some(made) = made {
            if let Err(e) = made.result {
                let path = format!("{}/{}", self.folder, made.file.name());
                return Ctl::End(Step::Failed(
                    Abort::new(
                        Message::new("writing {path}: {error}")
                            .with("path", path)
                            .with("error", e),
                    )
                    .failure(),
                ));
            }
            if !out.data.needs_furnidata_entries() {
                if let Some(furnidata) = out.furnidata.as_mut() {
                    furnidata.entries.clear();
                }
            }
            let event = match (&out.changes, made.file) {
                (Some(changes), output::DataFile::Changes) => {
                    let s = &changes.summary;
                    Some(RipEvent::CatalogChanged {
                        since: PathBuf::from(&s.since),
                        summary: s.summary(),
                        added: s.added(),
                        changed: s.changed(),
                        removed: s.removed(),
                    })
                }
                _ => None,
            };
            if made.file == output::DataFile::ManagerChanges {
                // Both changes files are written: the earlier rip's rows go.
                if let Some(changes) = out.changes.take() {
                    out.changes_summary = Some(changes.summary);
                }
            }
            for line in made.log {
                self.say(line);
            }
            if let Some(event) = event {
                self.emit(event);
            }
            return Ctl::Yield;
        }
        out.files = out.data.files();
        // The images need neither.
        out.furnidata = None;
        out.products = None;
        // The image URLs: the settings', else the client configuration's, else the profile's.
        match self.site_config_source() {
            Some(source) if self.options.download_images && !self.cancel => {
                self.state = State::Export(ExportStage::SiteConfig {
                    urls: site_config::candidate_urls(&source),
                    next: 0,
                    fetched: Vec::new(),
                });
                Ctl::Continue
            }
            _ => self.begin_images(None),
        }
    }

    fn begin_images(&mut self, from_config: Option<(String, String)>) -> Ctl {
        let profile = self.profile.as_ref().expect("profile");
        let (config_catalog, config_icon) = match from_config {
            Some((c, i)) => (Some(c), Some(i)),
            None => (profile.catalog_url.clone(), profile.icon_url.clone()),
        };
        let catalog_url = self
            .options
            .catalog_url
            .clone()
            .or(config_catalog)
            .unwrap_or_else(|| DEFAULT_CATALOG_URL.into());
        let icon_url = self
            .options
            .icon_url
            .clone()
            .or(config_icon)
            .unwrap_or_else(|| DEFAULT_ICON_URL.into());
        let download = self.options.download_images;
        let cancelled = self.cancel;
        let out = self.out.as_mut().expect("out");
        out.catalog_url = catalog_url;
        out.icon_url = icon_url;
        if !download {
            out.images_text = Some("not downloaded (turned off)".into());
        } else if cancelled {
            out.images_text = Some("not downloaded (the rip was cancelled)".into());
        } else {
            out.plan = images::plan(&out.catalog, &out.catalog_url, &out.icon_url);
            let total = out.plan.len() as u64;
            self.say(Message::new("Downloading page images and icons ..."));
            self.set_phase(Phase::Images);
            self.progress.total = total;
        }
        self.state = State::Images;
        Ctl::Continue
    }

    fn images(&mut self, io: &mut JobIo, end: u64) -> Ctl {
        let folder = self.folder.clone();
        loop {
            let cancelled = self.cancel;
            let can_fetch = self.may_fetch(io.clock.now_ms(), end);
            let out = self.out.as_mut().expect("out");
            if out.images_text.is_some() {
                return self.finish(io);
            }
            if cancelled || out.next_image >= out.plan.len() {
                // For README.txt: English.
                let mut text = out.stats.describe().to_string();
                if cancelled {
                    text.push_str(" (stopped: cancelled)");
                }
                out.stats.missing_names.sort();
                out.images_text = Some(text);
                continue;
            }
            // The images folder is made for the first image tried (as the built-in does).
            if !out.files.iter().any(|f| f == "images") {
                out.files.push("images".into());
            }
            let job = &out.plan[out.next_image];
            let url = job.urls[out.next_url].clone();
            let last_url = out.next_url + 1 == job.urls.len();
            let dest = job.dest(&url);
            let label = job.label.clone();
            // Ok(Some(true)): there; Ok(Some(false)): on no URL; Ok(None): try the next URL.
            let outcome: Result<Option<bool>, String> = if out.written.contains(&dest) {
                Ok(Some(true))
            } else if !can_fetch {
                return Ctl::Yield;
            } else {
                self.fetched = true;
                match io.fetch.get(&url) {
                    Ok(bytes) => {
                        let path = format!("{folder}/{dest}");
                        match io.sink.write(&path, &bytes) {
                            Ok(()) => {
                                out.written.insert(dest);
                                Ok(Some(true))
                            }
                            Err(e) => Err(format!("{path}: {e}")),
                        }
                    }
                    Err(FetchError::NotFound) if last_url => Ok(Some(false)),
                    Err(FetchError::NotFound) => Ok(None),
                    // A dead host: try the next URL before giving up on the image.
                    Err(FetchError::Failed(_)) if !last_url => Ok(None),
                    Err(FetchError::Failed(e)) => Err(format!("GET {url}: {e}")),
                }
            };
            let mut line = None;
            match outcome {
                Ok(None) => out.next_url += 1,
                done => {
                    match done {
                        Ok(Some(true)) => out.stats.downloaded += 1,
                        Ok(_) => {
                            out.stats.missing += 1;
                            out.stats.missing_names.push(label.clone());
                        }
                        Err(e) => {
                            out.stats.failed += 1;
                            line = Some(
                                Message::new("Image {label}: {error}")
                                    .with("label", &label)
                                    .with("error", e),
                            );
                        }
                    }
                    out.next_image += 1;
                    out.next_url = 0;
                    self.progress.done = out.next_image as u64;
                    self.progress.label = label;
                }
            }
            if let Some(line) = line {
                self.say(line);
            }
        }
    }

    /// README.txt, the summary, and the end of the job.
    fn finish(&mut self, io: &mut JobIo) -> Ctl {
        let out = self.out.as_ref().expect("out");
        let data = &out.data;
        let profile = self.profile.as_ref().expect("profile");
        let cancelled = self.cancel;
        let readme = output::readme(&output::ReadmeInput {
            catalog: &out.catalog,
            sql: &data.sql,
            images: out.images_text.as_deref().unwrap_or_default(),
            database_note: &out.database_note,
            manager: &data.manager,
            with_items_base: out.with_items_base,
            profile,
            missing_furni: data.missing_furni.len(),
            furnidata_names: out.furnidata_names,
            changes: out.changes_summary.as_ref(),
            diff_delete_removed: self.options.diff_delete_removed,
            packets: self.recorder.any,
            cancelled,
            crlf: self.options.readme_crlf,
        });
        let path = self.path("README.txt");
        if let Err(e) = io.sink.write(&path, readme.as_bytes()) {
            return Ctl::End(Step::Failed(
                Abort::new(
                    Message::new("writing {path}: {error}")
                        .with("path", path)
                        .with("error", e),
                )
                .failure(),
            ));
        }
        let mut summary = self.summary();
        summary.files.push("README.txt".into());
        self.log_summary(&summary);
        self.emit(RipEvent::Finished {
            folder: PathBuf::from(&summary.folder),
            complete: summary.complete,
        });
        match self.aborted.take() {
            None => Ctl::End(Step::Done(summary)),
            Some(abort) => {
                let message = Message::new(
                    "{reason}\nThe catalogue read so far was saved in {folder}.",
                )
                .with_message("reason", abort.message.clone())
                .with("folder", &summary.folder);
                Ctl::End(Step::Failed(Failure {
                    message,
                    partial: Some(Box::new(summary)),
                    ..abort.failure()
                }))
            }
        }
    }

    fn summary(&self) -> Summary {
        let out = self.out.as_ref().expect("out");
        let data = &out.data;
        let profile = self.profile.as_ref().expect("profile");
        let catalog = &out.catalog;
        let mut summary = Summary {
            folder: self.folder.clone(),
            files: out.files.clone(),
            hotel: catalog.hotel.clone(),
            host: catalog.host.clone(),
            client_type: catalog.client_type.clone(),
            hotel_version: catalog.hotel_version.clone(),
            profile: profile.name.clone(),
            kind: profile.kind,
            headers: self.headers.unwrap_or(Headers {
                get_index: 0,
                get_page: 0,
                index: 0,
                page: 0,
            }),
            pages: 0,
            pages_failed: 0,
            items: 0,
            products: 0,
            frontpage_items: 0,
            images: out.stats.clone(),
            sql: data.sql.clone(),
            manager: data.manager.clone(),
            complete: catalog.complete,
            elapsed_ms: self.elapsed_ms,
            furnidata_names: out.furnidata_names.unwrap_or(0),
            productdata_names: out.productdata_names,
            learned_headers: self.learned,
            learned_profile: self.learned_profile.clone(),
            delay_ms: self.delay,
            missing_furni: data.missing_furni.clone(),
            cancelled: self.cancel,
            resumed_pages: self.resumed_pages,
            changes: out.changes_summary.clone(),
        };
        for mode in &catalog.modes {
            summary.pages += mode.pages.len();
            summary.pages_failed += mode.failed.len();
            for page in mode.pages.values() {
                summary.items += page.offers.len();
                summary.products += page.offers.iter().map(|o| o.products.len()).sum::<usize>();
                summary.frontpage_items += page.frontpage_items.len();
            }
        }
        summary
    }

    fn log_summary(&mut self, s: &Summary) {
        let mut lines = vec![
            Message::new("Catalogue rip summary"),
            Message::new("  Hotel:               {hotel} ({host}, profile {profile})")
                .with("hotel", &s.hotel)
                .with("host", &s.host)
                .with("profile", &s.profile),
            Message::new("  Time:                {minutes} min")
                .with("minutes", format!("{:.1}", s.elapsed_ms as f64 / 60_000.0)),
            Message::new("  Pages:               {count}").with("count", n0(s.pages)),
        ];
        if s.resumed_pages > 0 {
            lines.push(
                Message::new("  From the stopped rip: {count}").with("count", n0(s.resumed_pages)),
            );
        }
        if s.pages_failed > 0 {
            lines.push(
                Message::new("  Pages without answer: {count}").with("count", n0(s.pages_failed)),
            );
        }
        lines.push(Message::new("  Offers:              {count}").with("count", n0(s.items)));
        lines.push(Message::new("  Products:            {count}").with("count", n0(s.products)));
        lines.push(
            Message::new("  Frontpage items:     {count}").with("count", n0(s.frontpage_items)),
        );
        lines.push(
            Message::new("  Arcturus SQL:        {pages} catalog_pages, {items} catalog_items")
                .with("pages", n0(s.sql.pages))
                .with("items", n0(s.sql.items)),
        );
        lines.push(if s.manager.offers_unresolved > 0 {
            Message::new(
                "  Manager import:      {pages} pages, {offers} offers ({mode}), {left_out} left out: products not in items_base",
            )
            .with("left_out", n0(s.manager.offers_unresolved))
        } else {
            Message::new("  Manager import:      {pages} pages, {offers} offers ({mode})")
        }
        .with("pages", n0(s.manager.pages))
        .with("offers", n0(s.manager.offers))
        .with_message("mode", s.manager.mode.label()));
        lines.push(
            Message::new("  Images:              {images}").with_message("images", s.images.describe()),
        );
        lines.push(Message::new("  Output folder:       {folder}").with("folder", &s.folder));
        if !s.complete {
            lines.push(if self.recorder.any {
                Message::new("  INCOMPLETE: the rip stopped early (see README.txt); it can go on from this folder.")
            } else {
                Message::new("  INCOMPLETE: the rip stopped early (see README.txt).")
            });
        }
        self.log.extend(lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_cannot_fake_progress() {
        assert_eq!(progress_label("50% off [new]"), "50  off  new ");
    }

    #[test]
    fn delay_is_kept_between_50_and_5000() {
        let t = |delay_ms| {
            Timing::of(&RipOptions {
                delay_ms,
                ..RipOptions::default()
            })
            .delay
        };
        assert_eq!((t(0), t(400), t(60_000)), (50, 400, 5_000));
        let d = Timing::of(&RipOptions::default());
        assert_eq!(
            (
                d.max_delay,
                d.response_timeout,
                d.hotel_wait,
                d.resend_every
            ),
            (5_000, 10_000, 600_000, 20_000)
        );
    }
}
