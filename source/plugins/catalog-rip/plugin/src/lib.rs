//! "Catalogue ripper": the built-in catalogue ripper as a plugin, a thin page over `catalog-rip-core`.
//!
//! The user starts G-Earth and logs in to the hotel; the page finds G-Earth on one of the ports 9092 to 9101,
//! "Rip catalogue" asks for the output folder and runs the core's [`Job`] in ticks: first the hotel's items_base
//! (one SQL read per tick), then, for the Manager modes that need them, the hotel's catalogue ids (catalog_pages,
//! catalog_items and the Builders Club pair, one read per tick), then the G-Earth connection (one port per tick), then
//! [`Job::step`] with [`JOB_BUDGET_MS`] per tick until it is done. The job pumps G-Earth inside its step, so every tick
//! asks for the next one with `keep_working`; Cancel goes in between.
//!
//! Besides a rip: "Resume a stopped rip" goes on with a rip that stopped ([`Job::resume`], the folder with its
//! `packets/` picked), "Import a Packet Logger file" makes a rip from a G-Earth Packet Logger export
//! ([`Job::import_log`], the picked file held in memory until the job has read it), and "Compare with an earlier rip"
//! chooses the rip the changes files are made against (else the newest earlier rip of the hotel in the output folder).
//! The job reads all of them through one [`RipsSource`].

// Off wasm32 `export_plugin!` expands to nothing, so the page is unused in a native build (the tests only cover
// the adapters' pure parts).
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod adapters;

use adapters::{
    add_profile, after_failure, all_profiles, drop_items_base, extension_info, generator, id_value, items_row, list_rips,
    log_host, needs_existing, parse_delay, Advanced, AfterFailure, ConnTransport, ExistingLoader, FolderSource,
    HttpFetch, IdSource, ItemsLoader, ItemsSource, Load, Probe, RipList, RipNeed, RipsSource, Route, SourceError,
    SysClock, ADVANCED_CHECKBOXES, FIRST_PORT, HEADER_INPUTS, LAST_PORT, MANAGER_MODES, MAX_LOG, NUMBER_INPUTS,
    PROFILES_FILE, SQL_LIMIT, URL_INPUTS,
};
use aio_plugin_sdk::folders::{self, Folder};
use aio_plugin_sdk::host::ManagerPage;
use aio_plugin_sdk::i18n::{tr, tr_with};
use aio_plugin_sdk::sql::{Op, Select};
use aio_plugin_sdk::ui::BannerKind;
use aio_plugin_sdk::{export_plugin, files, host, tcp, ErrorKind, Event, Plugin, Ui};
use catalog_rip_core::output::items_base;
use catalog_rip_core::transport::{Closed, Transport};
use catalog_rip_core::Message;
use catalog_rip_core::{
    Clock, ExistingCatalogue, Failure, HotelProfile, ImportLog, ItemsBase, ItemsBaseRow, Job, JobIo, Phase,
    ProfileChoice, RipOptions, Step, Summary,
};

/// What one tick gives the job; the rest of the tick's 5 s is margin (the job's G-Earth waits are host I/O).
const JOB_BUDGET_MS: u32 = 200;
/// Log lines the page keeps.
const MAX_LOG_LINES: usize = 400;
const MANAGER_SQL: &str = "catalog_manager.sql";

/// What kind of job a run makes.
#[derive(Clone)]
enum Kind {
    Rip,
    /// Go on with this stopped rip (a path of the [`RipsSource`]).
    Resume(String),
    /// A rip from a Packet Logger export.
    Import(Box<ImportLog>),
}

/// Where a running rip is.
enum Stage {
    /// Reading the hotel's items_base, one query per tick.
    Items(ItemsLoader),
    /// Reading the hotel's catalogue ids, one query per tick.
    Existing(ExistingLoader),
    /// Looking for G-Earth: the ports left to try (the next one last), one per tick.
    Connect(Vec<u16>),
    /// The job. `conn` is dropped (closed) as soon as the job is done with G-Earth. `rest`: the
    /// ports still to try if this one does not answer as G-Earth.
    Rip { job: Box<Job>, conn: Option<tcp::Conn>, rest: Vec<u16> },
}

/// Rips in a picked folder, one of them chosen (or the folder itself is the rip).
struct Picked {
    folder: Folder,
    /// The folder's name (the rip's name in the job's paths when the folder is the rip).
    name: String,
    list: RipList,
    choice: usize,
}

impl Picked {
    /// The chosen rip's name in the job's paths.
    fn key(&self) -> &str {
        if self.list.top {
            &self.name
        } else {
            &self.list.rips[self.choice.min(self.list.rips.len() - 1)]
        }
    }

    fn route(&self) -> Route<'_> {
        Route { key: self.key(), inside: !self.list.top, folder: &self.folder }
    }
}

/// A rip in progress.
struct Run {
    stage: Stage,
    kind: Kind,
    /// The output folder.
    folder: Folder,
    options: RipOptions,
    /// Read the hotel's items_base.
    items_lookup: bool,
    items: Option<ItemsBase>,
    existing: Option<ExistingCatalogue>,
    /// The stopped rip when it is a picked folder of its own (else it is in `folder`).
    resume_from: Option<Picked>,
    /// The Packet Logger export (its name and bytes), until the job has read it.
    log_file: Option<(String, Vec<u8>)>,
    /// The G-Earth ports to try, first to last (the one that answered on the page first).
    order: Vec<u16>,
    /// The port G-Earth answered on.
    port: Option<u16>,
    /// What the job waits for the user to do.
    waiting: Option<Message>,
    cancelling: bool,
}

/// How a rip ended.
enum Ended {
    Done(Box<Summary>),
    Failed(Failure),
    /// No port answered.
    NoGearth,
    /// Cancelled before the job began.
    Cancelled,
}

/// The last rip, with what the buttons after it need.
struct Outcome {
    ended: Ended,
    kind: Kind,
    folder: Folder,
    options: RipOptions,
    items: Option<ItemsBase>,
    existing: Option<ExistingCatalogue>,
    resume_from: Option<Picked>,
}

struct Ripper {
    clock: SysClock,
    probe: Probe,
    profiles: Vec<HotelProfile>,
    /// 0: detect; `n`: `profiles[n - 1]`.
    profile: usize,
    normal: bool,
    builders_club: bool,
    images: bool,
    items_lookup: bool,
    /// The delay input's text, checked when a rip starts.
    delay_text: String,
    /// Index in [`MANAGER_MODES`].
    mode: usize,
    show_advanced: bool,
    advanced: Advanced,
    /// The earlier rip to compare with (else the newest of the hotel in the output folder).
    compare: Option<Picked>,
    /// A stopped rip to go on with, being chosen.
    resume: Option<Picked>,
    run: Option<Run>,
    outcome: Option<Outcome>,
    log: Vec<String>,
    message: Option<(BannerKind, String)>,
}

fn source_error(e: aio_plugin_sdk::Error) -> SourceError {
    SourceError { denied: e.kind() == ErrorKind::Denied, message: e.message().to_string() }
}

/// items_base through the Manager's SQL (permission `sql:read:items_base`).
struct SqlItems;

impl ItemsSource for SqlItems {
    fn window(&mut self, from: i64, to: i64, effect: bool) -> Result<Vec<Result<ItemsBaseRow, String>>, SourceError> {
        let mut columns = vec!["id", "sprite_id", "type", "item_name"];
        if effect {
            columns.push("effect_id_male");
        }
        let rows = Select::from("items_base")
            .columns(columns)
            .filter("id", Op::Ge, from)
            .filter("id", Op::Lt, to)
            .limit(SQL_LIMIT as u32)
            .run()
            .map_err(source_error)?;
        Ok(rows.iter().map(|row| items_row(|column| row.value(column))).collect())
    }

    fn any_from(&mut self, from: i64) -> Result<bool, SourceError> {
        let rows = Select::from("items_base").columns(["id"]).filter("id", Op::Ge, from).limit(1).run().map_err(source_error)?;
        Ok(!rows.is_empty())
    }
}

/// The catalogue ids through the Manager's SQL (`sql:read:catalog_pages,catalog_items,...`): only `id`.
struct SqlIds;

impl IdSource for SqlIds {
    fn ids(&mut self, table: &str, from: i64, to: i64) -> Result<Vec<Result<i64, String>>, SourceError> {
        let rows = Select::from(table)
            .columns(["id"])
            .filter("id", Op::Ge, from)
            .filter("id", Op::Lt, to)
            .limit(SQL_LIMIT as u32)
            .run()
            .map_err(source_error)?;
        Ok(rows.iter().map(|row| id_value(row.value("id"))).collect())
    }

    fn any_from(&mut self, table: &str, from: i64) -> Result<bool, SourceError> {
        let rows = Select::from(table).columns(["id"]).filter("id", Op::Ge, from).limit(1).run().map_err(source_error)?;
        Ok(!rows.is_empty())
    }
}

fn phase_text(phase: Phase) -> String {
    match phase {
        Phase::Load => tr("Reading the recorded packets"),
        Phase::Connect => tr("Connecting to G-Earth"),
        Phase::WaitHotel => tr("Waiting for the hotel connection"),
        Phase::Learn => tr("Learning the packet ids from the client"),
        Phase::Index => tr("Reading the catalogue index"),
        Phase::Pages => tr("Reading the catalogue pages"),
        Phase::Extras => tr("Reading the extra catalogue messages"),
        Phase::Export => tr("Writing the files"),
        Phase::Images => tr("Downloading the images"),
        Phase::Done => tr("Done"),
    }
}

/// A message of the core in the Manager's language: its English template is the key, `it.json` has the
/// translation (the drift test reads the core's `Message::new("...")` literals), the values are filled after.
fn text(message: &Message) -> String {
    message.render(&|english| tr(english)) // scan:dynamic (the core's templates, checked in the core's sources)
}

/// The label of an advanced setting.
fn advanced_label(id: &str) -> String {
    if let Some((_, name)) = HEADER_INPUTS.iter().find(|(i, _)| *i == id) {
        return tr_with("{name} header id (empty: from the hotel profile or G-Earth)", &[("name", name)]);
    }
    match id {
        "catalog_url" => tr("Catalogue image address, …/c_images/catalogue (empty: from the hotel profile)"),
        "icon_url" => tr("Page icon address, before <n>.png (empty: from the hotel profile)"),
        "furnidata_url" => tr("The hotel's furnidata address (empty: from the hotel profile)"),
        "productdata_url" => tr("The hotel's productdata address (empty: from the hotel profile)"),
        "site_config_url" => tr("Client configuration address, read for the image addresses (empty: from the hotel profile)"),
        "timeout_ms" => tr("Wait for an answer (ms)"),
        "max_delay_ms" => tr("Longest pause when the hotel stops answering (ms)"),
        "hotel_wait_s" => tr("Wait for the hotel connection (s)"),
        "resend_s" => tr("Ask again while waiting for a first answer, every (s)"),
        "adaptive_delay" => tr("Pause longer when pages go unanswered"),
        "learn_headers" => tr("Learn unknown packet ids from the client"),
        "rip_extras" => tr("Also read the club offers, bundle discounts and gift wrapping (catalog_extras.json)"),
        "record_packets" => tr("Keep the packets in packets/ (to resume a stopped rip)"),
        "use_furnidata" => tr("Match the products by class name through the hotel's furnidata"),
        "diff_with_previous" => tr("Compare with the newest earlier rip of the hotel in the output folder"),
        "diff_delete_removed" => tr("The changes file also deletes the pages and offers the hotel removed"),
        _ => id.to_string(),
    }
}

impl Run {
    fn new(kind: Kind, folder: Folder, options: RipOptions, items_lookup: bool, order: Vec<u16>) -> Run {
        Run {
            stage: Stage::Items(ItemsLoader::new()),
            kind,
            folder,
            options,
            items_lookup,
            items: None,
            existing: None,
            resume_from: None,
            log_file: None,
            order,
            port: None,
            waiting: None,
            cancelling: false,
        }
    }

    /// The first stage: items_base, unless it is not to be read.
    fn first_stage(&self, log: &mut Vec<String>) -> Stage {
        if self.items_lookup {
            Stage::Items(ItemsLoader::new())
        } else {
            log.push(tr(
                "The furni are not matched with the hotel's items_base (turned off): the item ids are written as \
                 items_base subqueries, resolved when the SQL is imported.",
            ));
            self.after_items()
        }
    }

    /// After items_base: the catalogue ids when the Manager mode needs them.
    fn after_items(&self) -> Stage {
        if needs_existing(self.options.manager_mode) && self.existing.is_none() {
            Stage::Existing(ExistingLoader::new())
        } else {
            self.after_existing()
        }
    }

    /// After the hotel's database: G-Earth, or the job at once for a Packet Logger export.
    fn after_existing(&self) -> Stage {
        match &self.kind {
            Kind::Import(log) => Stage::Rip { job: Box::new(Job::import_log(self.options.clone(), (**log).clone())), conn: None, rest: Vec::new() },
            Kind::Rip | Kind::Resume(_) => Stage::Connect(self.ports()),
        }
    }

    /// One tick of work; `Some` when the rip ended. Log lines go to `log`. `compare`: the earlier rip to compare with.
    fn tick(&mut self, clock: &SysClock, log: &mut Vec<String>, compare: Option<&Picked>) -> Option<Ended> {
        match &mut self.stage {
            Stage::Items(loader) => {
                let load = loader.step(&mut SqlItems);
                log.extend(loader.take_notes());
                match load {
                    Load::More => None,
                    Load::Done => {
                        log.push(tr_with("Read {count} items_base rows.", &[("count", &loader.rows.len().to_string())]));
                        self.items = Some(items_base(&loader.rows));
                        self.stage = self.after_items();
                        None
                    }
                    Load::Unavailable(why) => {
                        log.push(tr_with(
                            "items_base cannot be read ({why}): the item ids are written as items_base subqueries, \
                             resolved when the SQL is imported.",
                            &[("why", &why)],
                        ));
                        self.stage = self.after_items();
                        None
                    }
                }
            }
            Stage::Existing(loader) => {
                let load = loader.step(&mut SqlIds);
                log.extend(loader.take_notes());
                match load {
                    Load::More => None,
                    Load::Done => {
                        log.push(tr_with(
                            "Read {count} page and offer ids of the hotel's catalogue.",
                            &[("count", &loader.rows.to_string())],
                        ));
                        self.existing = Some(std::mem::take(&mut loader.existing));
                        self.stage = self.after_existing();
                        None
                    }
                    Load::Unavailable(why) => {
                        log.push(tr_with(
                            "The hotel's catalogue ids cannot be read ({why}); catalog_manager.sql says what it does \
                             instead.",
                            &[("why", &why)],
                        ));
                        self.stage = self.after_existing();
                        None
                    }
                }
            }
            Stage::Connect(ports) => {
                let Some(port) = ports.pop() else { return Some(Ended::NoGearth) };
                match tcp::connect(port) {
                    Ok(conn) => {
                        log.push(tr_with("Connected to port {port} (G-Earth?).", &[("port", &port.to_string())]));
                        self.port = Some(port);
                        let mut options = self.options.clone();
                        options.gearth_port = port;
                        let rest = std::mem::take(ports);
                        let job = match &self.kind {
                            Kind::Resume(folder) => Job::resume(options, folder),
                            // A Packet Logger import never connects.
                            Kind::Rip | Kind::Import(_) => Job::new(options),
                        };
                        self.stage = Stage::Rip { job: Box::new(job), conn: Some(conn), rest };
                        None
                    }
                    Err(e) => {
                        log.push(tr_with("Port {port}: {error}", &[("port", &port.to_string()), ("error", &e.to_string())]));
                        ports.is_empty().then_some(Ended::NoGearth)
                    }
                }
            }
            Stage::Rip { job, conn, rest } => {
                let step = {
                    let mut closed = Closed;
                    let mut tcp_transport;
                    let transport: &mut dyn Transport = match conn.as_mut() {
                        Some(c) => {
                            tcp_transport = ConnTransport(c);
                            &mut tcp_transport
                        }
                        None => &mut closed,
                    };
                    let mut fetch = HttpFetch;
                    let mut sink = adapters::FolderSink(&self.folder);
                    let mut source = RipsSource {
                        output: &self.folder,
                        routes: self.resume_from.iter().chain(compare).map(Picked::route).collect(),
                        log: self.log_file.as_ref().map(|(name, data)| (name.as_str(), data.as_slice())),
                    };
                    let mut io = JobIo {
                        transport,
                        fetch: &mut fetch,
                        sink: &mut sink,
                        clock,
                        items: self.items.as_ref(),
                        existing: self.existing.as_ref(),
                        source: Some(&mut source),
                    };
                    job.step(&mut io, JOB_BUDGET_MS)
                };
                for line in job.take_log() {
                    // The host's log stays English; the page's log is in the Manager's language.
                    host::log(host::Level::Info, &line.to_string());
                    log.push(text(&line));
                }
                // Every event has a log line too; nothing more to show.
                job.take_events();
                if !job.uses_gearth() && conn.take().is_some() {
                    log.push(tr("Closed the connection to G-Earth."));
                }
                // Memory: the Packet Logger export is read once.
                if !job.reads_import_log() {
                    self.log_file = None;
                }
                // Memory: the files still to write (catalog.json, the biggest) do not need them. Kept after a
                // failure: the next port's job or Try again reuses them.
                if drop_items_base(&step, job.uses_items_base()) {
                    self.items = None;
                    self.existing = None;
                }
                match step {
                    Step::Running => {
                        self.waiting = None;
                        None
                    }
                    Step::Waiting(text) => {
                        self.waiting = Some(text);
                        None
                    }
                    Step::Done(summary) => Some(Ended::Done(Box::new(summary))),
                    Step::Failed(failure) => {
                        // A G-Earth error ends the G-Earth session (the job no longer uses it either way).
                        *conn = None;
                        match after_failure(&failure, rest.len()) {
                            AfterFailure::Ended => Some(Ended::Failed(failure)),
                            AfterFailure::NoGearth => {
                                log.push(format!("{} {}", text(&failure.message), tr("No port left to try.")));
                                Some(Ended::NoGearth)
                            }
                            AfterFailure::NextPort => {
                                log.push(format!("{} {}", text(&failure.message), tr("Trying the next port.")));
                                self.port = None;
                                self.waiting = None;
                                self.stage = Stage::Connect(std::mem::take(rest));
                                None
                            }
                        }
                    }
                }
            }
        }
    }

    /// The ports to try, the next one last.
    fn ports(&self) -> Vec<u16> {
        self.order.iter().rev().copied().collect()
    }
}

impl Ripper {
    fn say(&mut self, kind: BannerKind, text: impl Into<String>) {
        self.message = Some((kind, text.into()));
    }

    fn push_log(&mut self, lines: impl IntoIterator<Item = String>) {
        self.log.extend(lines);
        if self.log.len() > MAX_LOG_LINES {
            let extra = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..extra);
        }
    }

    fn load_profiles(&mut self) {
        let text = match files::read_to_string_if_exists(PROFILES_FILE) {
            Ok(text) => text,
            Err(e) => {
                self.push_log([tr_with("The saved hotel profiles cannot be read: {error}", &[("error", &e.to_string())])]);
                None
            }
        };
        let (profiles, errors) = all_profiles(text.as_deref());
        self.push_log(errors.into_iter().map(|e| tr_with("Saved hotel profiles: {error}", &[("error", &e)])));
        self.profiles = profiles;
        self.profile = self.profile.min(self.profiles.len());
    }

    /// Saves a learned profile among the user's profiles and lists it.
    fn save_learned(&mut self, learned: &Option<(String, String)>) {
        let Some((file_name, json)) = learned else { return };
        let saved = files::read_to_string_if_exists(PROFILES_FILE)
            .map_err(|e| e.to_string())
            .and_then(|old| add_profile(old.as_deref(), file_name, json))
            .and_then(|text| files::write_str(PROFILES_FILE, &text).map_err(|e| e.to_string()));
        match saved {
            Ok(()) => self.push_log([tr("The learned packet ids were saved as a hotel profile.")]),
            Err(e) => self.push_log([tr_with("The learned hotel profile could not be saved: {error}", &[("error", &e)])]),
        }
        self.load_profiles();
    }

    fn fixed_profile(&self) -> Option<&HotelProfile> {
        self.profile.checked_sub(1).and_then(|n| self.profiles.get(n))
    }

    /// The settings as the job's options, or what is wrong with them (said in the banner).
    fn checked_options(&mut self) -> Option<RipOptions> {
        if !self.normal && !self.builders_club {
            self.say(BannerKind::Warning, tr("Choose the normal catalogue, the Builders Club one, or both."));
            return None;
        }
        let delay_ms = match parse_delay(&self.delay_text) {
            Ok(ms) => ms,
            Err(e) => {
                self.say(BannerKind::Warning, format!("{e} {}", tr("Correct it, then start again.")));
                return None;
            }
        };
        let profile = match self.fixed_profile() {
            Some(profile) => ProfileChoice::Fixed(profile.clone()),
            None => ProfileChoice::Detect(self.profiles.clone()),
        };
        let mut options = RipOptions {
            profile,
            delay_ms,
            normal: self.normal,
            builders_club: self.builders_club,
            download_images: self.images,
            manager_mode: MANAGER_MODES[self.mode.min(MANAGER_MODES.len() - 1)],
            diff_against: self.compare.as_ref().map(|p| p.key().to_string()),
            generator: generator(),
            extension: extension_info(),
            ..RipOptions::default()
        };
        if let Err(e) = self.advanced.apply(&mut options) {
            self.say(BannerKind::Warning, format!("{e} {}", tr("Correct it in the advanced settings, then start again.")));
            self.show_advanced = true;
            return None;
        }
        Some(options)
    }

    /// Starts a run.
    fn launch(&mut self, run: Run) {
        self.message = None;
        self.outcome = None;
        self.log.clear();
        let mut lines = vec![tr_with("Saving into the folder {folder}.", &[("folder", &run.folder.name())])];
        match &run.kind {
            Kind::Rip => {}
            Kind::Resume(rip) => lines.push(tr_with("Going on with the stopped rip {rip}.", &[("rip", rip)])),
            Kind::Import(log) => lines.push(tr_with("Importing the Packet Logger file {file}.", &[("file", &log.path)])),
        }
        if let Some(compare) = &self.compare {
            lines.push(tr_with("Compared with the earlier rip {rip}.", &[("rip", compare.key())]));
        }
        let mut run = run;
        run.stage = run.first_stage(&mut lines);
        self.push_log(lines);
        self.run = Some(run);
    }

    fn start(&mut self) {
        let Some(options) = self.checked_options() else { return };
        let Some(folder) = folders::pick(&tr("Where to save the catalogue")) else { return };
        let run = Run::new(Kind::Rip, folder, options, self.items_lookup, self.probe.connect_order());
        self.launch(run);
    }

    /// Picks a folder and lists its rips that have `need`'s file; `None` (said in the banner) when it has none.
    fn pick_rips(&mut self, title: &str, need: RipNeed) -> Option<Picked> {
        let folder = folders::pick(title)?;
        let name = folder.name();
        match list_rips(&mut FolderSource(&folder), need) {
            Ok(list) if list.top || !list.rips.is_empty() => Some(Picked { folder, name, list, choice: 0 }),
            Ok(_) => {
                let text = match need {
                    RipNeed::Catalog => tr_with("{folder} has no rip to compare with (a rip folder with catalog.json).", &[("folder", &name)]),
                    RipNeed::Packets => tr_with(
                        "{folder} has no stopped rip to go on with (a rip folder with packets/session.json).",
                        &[("folder", &name)],
                    ),
                };
                self.say(BannerKind::Warning, text);
                None
            }
            Err(e) => {
                self.say(BannerKind::Error, tr_with("{folder} cannot be read: {error}", &[("folder", &name), ("error", &e)]));
                None
            }
        }
    }

    fn pick_compare(&mut self) {
        if let Some(picked) = self.pick_rips(&tr("The earlier rip, or the folder with it"), RipNeed::Catalog) {
            self.message = None;
            self.compare = Some(picked);
        }
    }

    fn pick_resume(&mut self) {
        if let Some(picked) = self.pick_rips(&tr("The stopped rip, or the folder with it"), RipNeed::Packets) {
            self.message = None;
            self.resume = Some(picked);
        }
    }

    /// "Resume this rip": into the folder the stopped rip is in, or (when the picked folder is the rip itself) into a
    /// folder to pick.
    fn start_resume(&mut self) {
        let Some(options) = self.checked_options() else { return };
        let Some(picked) = self.resume.take() else { return };
        let key = picked.key().to_string();
        let order = self.probe.connect_order();
        let run = if picked.list.top {
            let Some(folder) = folders::pick(&tr("Where to save the catalogue")) else {
                self.resume = Some(picked);
                return;
            };
            let mut run = Run::new(Kind::Resume(key), folder, options, self.items_lookup, order);
            run.resume_from = Some(picked);
            run
        } else {
            Run::new(Kind::Resume(key), picked.folder, options, self.items_lookup, order)
        };
        self.launch(run);
    }

    /// "Import a Packet Logger file": the export (read whole, at most [`MAX_LOG`]), the hotel's host for the folder's
    /// name, the output folder.
    fn import_log(&mut self) {
        let Some(options) = self.checked_options() else { return };
        let Some(file) = host::pick_open_file(&tr("The Packet Logger export (G-Earth: Export all)"), ["txt", "log"]) else {
            return;
        };
        let name = file.name();
        let data = match file.read() {
            Ok(data) => data,
            Err(e) => {
                self.say(BannerKind::Error, tr_with("{file} cannot be read: {error}", &[("file", &name), ("error", &e.to_string())]));
                return;
            }
        };
        drop(file);
        if data.len() > MAX_LOG {
            self.say(
                BannerKind::Error,
                tr_with(
                    "{file} is {size} MiB: the plugin reads Packet Logger files of at most {max} MiB.",
                    &[("file", &name), ("size", &(data.len() >> 20).to_string()), ("max", &(MAX_LOG >> 20).to_string())],
                ),
            );
            return;
        }
        let Some(typed) = host::prompt_text(
            &tr("Import a Packet Logger file"),
            &tr("The hotel's game host, for the folder's name (empty: gearth-log)"),
            "",
        ) else {
            return;
        };
        let host = match log_host(&typed) {
            Ok(host) => host,
            Err(e) => {
                self.say(BannerKind::Warning, e);
                return;
            }
        };
        let Some(folder) = folders::pick(&tr("Where to save the catalogue")) else { return };
        let log = ImportLog { path: name.clone(), host, profile: self.fixed_profile().cloned() };
        let mut run = Run::new(Kind::Import(Box::new(log)), folder, options, self.items_lookup, Vec::new());
        run.log_file = Some((name, data));
        self.launch(run);
    }

    /// "Try again" after G-Earth let the extension go: a new job on a new connection, same folder and settings.
    fn retry(&mut self) {
        let Some(outcome) = self.outcome.take() else { return };
        self.message = None;
        self.push_log([tr("Trying again with a new connection to G-Earth.")]);
        let order = self.probe.connect_order();
        let mut options = outcome.options;
        options.diff_against = self.compare.as_ref().map(|p| p.key().to_string());
        let mut run = Run::new(outcome.kind, outcome.folder, options, self.items_lookup, order);
        run.items = outcome.items;
        run.existing = outcome.existing;
        run.resume_from = outcome.resume_from;
        run.stage = Stage::Connect(run.ports());
        self.run = Some(run);
    }

    fn cancel(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        match &mut run.stage {
            Stage::Rip { job, .. } => {
                job.cancel();
                run.cancelling = true;
            }
            // Nothing was asked from the hotel yet.
            Stage::Items(_) | Stage::Existing(_) | Stage::Connect(_) => self.finish(Ended::Cancelled),
        }
    }

    fn finish(&mut self, ended: Ended) {
        let Some(run) = self.run.take() else { return };
        match &ended {
            Ended::Done(summary) => {
                self.save_learned(&summary.learned_profile);
                let mut said = tr_with(
                    "{pages} pages ({failed} failed) and {offers} offers saved in {folder}/{path}. Images: {images}.",
                    &[
                        ("pages", &summary.pages.to_string()),
                        ("failed", &summary.pages_failed.to_string()),
                        ("offers", &summary.items.to_string()),
                        ("folder", &run.folder.name()),
                        ("path", &summary.folder),
                        ("images", &text(&summary.images.describe())),
                    ],
                );
                // Each sentence is translated whole and the sentences are joined with a space.
                if summary.cancelled {
                    said.push(' ');
                    said.push_str(&tr("Cancelled while writing: the catalogue is whole, some images are missing."));
                }
                if let Some(changes) = &summary.changes {
                    said.push(' ');
                    said.push_str(&text(&changes.log_line()));
                }
                if !summary.missing_furni.is_empty() {
                    said.push(' ');
                    said.push_str(&tr_with(
                        "{count} products are not in this hotel's items_base (missing_furni.json).",
                        &[("count", &summary.missing_furni.len().to_string())],
                    ));
                }
                self.say(BannerKind::Success, said);
            }
            Ended::Failed(failure) => {
                if let Some(partial) = &failure.partial {
                    self.save_learned(&partial.learned_profile);
                }
                let kind = if failure.cancelled { BannerKind::Info } else { BannerKind::Error };
                let mut banner = text(&failure.message);
                if failure.reconnect {
                    banner.push(' ');
                    banner.push_str(&tr("Click Try again to connect to G-Earth again."));
                } else if failure.partial.is_some() && run.options.record_packets {
                    banner.push(' ');
                    banner.push_str(&tr("To go on with it, click Resume a stopped rip and pick its folder."));
                }
                self.say(kind, banner);
            }
            Ended::NoGearth => self.say(
                BannerKind::Error,
                tr_with(
                    "G-Earth did not answer on ports {first} to {last}. Start G-Earth and log in to the hotel, \
                     then click Try again.",
                    &[("first", &FIRST_PORT.to_string()), ("last", &LAST_PORT.to_string())],
                ),
            ),
            Ended::Cancelled => self.say(BannerKind::Info, tr("Cancelled before anything was asked from the hotel.")),
        }
        host::status(&match &ended {
            Ended::Done(_) => tr("Catalogue ripped."),
            Ended::Cancelled => tr("Catalogue rip cancelled."),
            _ => tr("The catalogue rip stopped."),
        });
        self.outcome = Some(Outcome {
            ended,
            kind: run.kind,
            folder: run.folder,
            options: run.options,
            items: run.items,
            existing: run.existing,
            resume_from: run.resume_from,
        });
        // Look for G-Earth again: one more sweep in ticks (or a recheck of the port that answered).
        self.probe.start_sweep();
    }

    fn import(&mut self) {
        let Some(Outcome { ended: Ended::Done(summary), folder, .. }) = &self.outcome else { return };
        let path = format!("{}/{MANAGER_SQL}", summary.folder);
        if let Err(e) = folder.open_import_preview(&path) {
            self.say(BannerKind::Error, tr_with("Could not open the import preview: {error}", &[("error", &e.to_string())]));
        }
    }

    fn reveal(&mut self) {
        let Some(outcome) = &self.outcome else { return };
        if let Err(e) = outcome.folder.reveal() {
            self.say(BannerKind::Warning, tr_with("Could not open the folder: {error}", &[("error", &e.to_string())]));
        }
    }

    /// While idle, in a render: one G-Earth port, at most every 2 s (not while a sweep runs in ticks).
    fn probe(&mut self) {
        if let Some(port) = self.probe.due(self.clock.now_ms()) {
            self.probe_port(port);
        }
    }

    /// One tick of a "Check again" sweep: one port.
    fn sweep(&mut self) {
        if let Some(port) = self.probe.sweep_port() {
            self.probe_port(port);
        }
    }

    fn probe_port(&mut self, port: u16) {
        // Connected and closed at once: G-Earth forgets an extension that leaves before it says who it is.
        let answered = tcp::connect(port).is_ok();
        self.probe.record(self.clock.now_ms(), port, answered);
    }

    /// Whether another tick is needed: a rip or a sweep runs.
    fn busy(&self) -> bool {
        self.run.is_some() || self.probe.sweep_port().is_some()
    }

    fn render_gearth(&self, ui: &mut Ui) {
        ui.card(tr("G-Earth"), |ui| {
            if let Some(run) = &self.run {
                match (&run.kind, &run.stage, run.port) {
                    (Kind::Import(_), _, _) => ui.text(tr("A Packet Logger import needs no G-Earth.")),
                    (_, Stage::Rip { conn: Some(_), .. }, Some(port)) => {
                        ui.text(tr_with("Connected on port {port}.", &[("port", &port.to_string())]))
                    }
                    (_, Stage::Rip { .. }, _) => ui.text(tr("The rip is done with G-Earth; the connection is closed.")),
                    _ => ui.text(tr("Connecting when the rip starts.")),
                }
                return;
            }
            ui.text(self.probe.status());
            ui.row(|ui| {
                if self.probe.sweep_port().is_none() {
                    ui.button("check", tr("Check again"));
                }
                if self.probe.found().is_none() {
                    ui.button("gearth_tool", tr("Install or start G-Earth"));
                }
            });
        });
    }

    fn render_settings(&self, ui: &mut Ui) {
        ui.card(tr("Settings"), |ui| {
            ui.text(tr("Hotel profile"));
            let options = std::iter::once(tr("Detect from the hotel connection")).chain(self.profiles.iter().map(|p| {
                let values = [("name", p.name.as_str()), ("kind", &text(&p.kind.label()))];
                if p.builtin {
                    tr_with("{name} ({kind})", &values)
                } else {
                    tr_with("{name} ({kind}, saved)", &values)
                }
            }));
            ui.select("profile", options, self.profile);
            ui.checkbox("normal", tr("Normal catalogue"), self.normal);
            ui.checkbox("builders_club", tr("Builders Club catalogue"), self.builders_club);
            ui.checkbox("images", tr("Download the images"), self.images);
            ui.checkbox(
                "items_lookup",
                tr("Match the furni with the hotel's items_base (for the import)"),
                self.items_lookup,
            );
            ui.input("delay", tr("Pause between requests (ms)"), self.delay_text.as_str());
            ui.text(tr("What Import into hotel does (catalog_manager.sql)"));
            ui.select(
                "mode",
                [
                    tr("Replace the whole catalogue"),
                    tr("Replace only the ripped catalogues"),
                    tr("Add next to the existing catalogue"),
                ],
                self.mode,
            );
            ui.checkbox("show_advanced", tr("Show the advanced settings"), self.show_advanced);
        });
        if self.show_advanced {
            ui.card(tr("Advanced settings"), |ui| {
                for (id, _, _) in NUMBER_INPUTS {
                    ui.input(id, advanced_label(id), self.advanced.text(id));
                }
                for id in ADVANCED_CHECKBOXES {
                    ui.checkbox(id, advanced_label(id), self.advanced.check(id));
                }
                for id in URL_INPUTS {
                    ui.input(id, advanced_label(id), self.advanced.text(id));
                }
                for (id, _) in HEADER_INPUTS {
                    ui.input(id, advanced_label(id), self.advanced.text(id));
                }
                ui.text(tr("Not in the plugin: ripping only the catalogue index, or only some branches."));
            });
        }
    }

    fn render_earlier(&self, ui: &mut Ui) {
        ui.card(tr("Earlier rips and Packet Logger files"), |ui| {
            match &self.compare {
                Some(compare) => {
                    ui.text(tr_with("Compared with the earlier rip {rip}.", &[("rip", compare.key())]));
                    if compare.list.rips.len() > 1 {
                        ui.select("compare_rip", compare.list.rips.iter().cloned(), compare.choice);
                    }
                }
                None if self.advanced.check("diff_with_previous") => ui.text(tr(
                    "Compared with the newest earlier rip of the hotel in the output folder, if there is one.",
                )),
                None => ui.text(tr("Not compared with an earlier rip.")),
            }
            ui.row(|ui| {
                ui.button("compare_pick", tr("Compare with an earlier rip"));
                if self.compare.is_some() {
                    ui.button("compare_clear", tr("Do not compare with it"));
                }
            });
            match &self.resume {
                Some(resume) => {
                    ui.text(tr_with("The stopped rip {rip}.", &[("rip", resume.key())]));
                    if resume.list.rips.len() > 1 {
                        ui.select("resume_rip", resume.list.rips.iter().cloned(), resume.choice);
                    }
                    ui.row(|ui| {
                        ui.primary_button("resume_start", tr("Resume this rip"));
                        ui.button("resume_close", tr("Close"));
                    });
                }
                None => ui.row(|ui| {
                    ui.button("resume_pick", tr("Resume a stopped rip"));
                    ui.button("import_log", tr("Import a Packet Logger file"));
                }),
            }
        });
    }

    fn render_run(run: &Run, ui: &mut Ui) {
        ui.card(tr("Progress"), |ui| match &run.stage {
            Stage::Items(loader) => ui.text(tr_with("Reading the hotel's items_base ({count} rows so far)...", &[("count", &loader.rows.len().to_string())])),
            Stage::Existing(loader) => ui.text(tr_with(
                "Reading the hotel's catalogue ids ({table}, {count} so far)...",
                &[("table", loader.table()), ("count", &loader.rows.to_string())],
            )),
            Stage::Connect(_) => ui.text(tr("Looking for G-Earth...")),
            Stage::Rip { job, .. } => {
                let progress = job.progress();
                let phase = phase_text(progress.phase);
                let done = progress.done.to_string();
                let total = progress.total.to_string();
                // The label is a name from the hotel's data, kept as it is.
                let line = match (progress.total > 0, progress.label.is_empty()) {
                    (true, true) => tr_with("{phase}: {done} of {total}", &[("phase", &phase), ("done", &done), ("total", &total)]),
                    (true, false) => tr_with(
                        "{phase}: {done} of {total} ({label})",
                        &[("phase", &phase), ("done", &done), ("total", &total), ("label", &progress.label)],
                    ),
                    (false, true) => phase,
                    (false, false) => tr_with("{phase} ({label})", &[("phase", &phase), ("label", &progress.label)]),
                };
                ui.text(line);
                ui.progress(progress.done, progress.total);
                if let Some(waiting) = &run.waiting {
                    ui.banner(BannerKind::Info, text(waiting));
                }
            }
        });
        if run.cancelling {
            ui.text(tr("Cancelling..."));
        } else {
            ui.button("cancel", tr("Cancel"));
        }
        ui.text(tr("Leaving this page stops the rip."));
    }

    fn render_outcome(outcome: &Outcome, ui: &mut Ui) {
        ui.row(|ui| {
            if let Ended::Done(summary) = &outcome.ended {
                ui.button("reveal", tr("Open folder"));
                if summary.files.iter().any(|f| f == MANAGER_SQL) {
                    ui.primary_button("import", tr("Import into hotel"));
                }
            } else if matches!(&outcome.ended, Ended::Failed(f) if f.partial.is_some()) {
                ui.button("reveal", tr("Open folder"));
            }
            let retry = match &outcome.ended {
                Ended::Failed(failure) => failure.reconnect,
                Ended::NoGearth => true,
                _ => false,
            };
            if retry {
                ui.primary_button("retry", tr("Try again"));
            }
        });
    }

    /// A changed setting.
    fn changed(&mut self, id: &str, value: String) {
        let index = || value.parse::<usize>().ok();
        match id {
            "profile" => {
                if let Some(n) = index() {
                    self.profile = n.min(self.profiles.len());
                }
            }
            "mode" => {
                if let Some(n) = index() {
                    self.mode = n.min(MANAGER_MODES.len() - 1);
                }
            }
            "compare_rip" => {
                if let (Some(n), Some(compare)) = (index(), self.compare.as_mut()) {
                    compare.choice = n.min(compare.list.rips.len().saturating_sub(1));
                }
            }
            "resume_rip" => {
                if let (Some(n), Some(resume)) = (index(), self.resume.as_mut()) {
                    resume.choice = n.min(resume.list.rips.len().saturating_sub(1));
                }
            }
            "normal" => self.normal = value == "true",
            "builders_club" => self.builders_club = value == "true",
            "images" => self.images = value == "true",
            "items_lookup" => self.items_lookup = value == "true",
            "show_advanced" => self.show_advanced = value == "true",
            "delay" => {
                let wrong = parse_delay(&value).err().unwrap_or_default();
                self.delay_text = value;
                self.input_checked(wrong);
            }
            other => {
                if let Some(wrong) = self.advanced.set(other, &value) {
                    self.input_checked(wrong);
                }
            }
        }
    }

    /// What is wrong with the input just changed (empty: nothing): said, or a warning about an input cleared.
    fn input_checked(&mut self, wrong: String) {
        if !wrong.is_empty() {
            self.say(BannerKind::Warning, wrong);
        } else if matches!(&self.message, Some((BannerKind::Warning, _))) {
            self.message = None;
        }
    }
}

impl Plugin for Ripper {
    fn new() -> Self {
        let mut ripper = Ripper {
            clock: SysClock::new(),
            probe: Probe::default(),
            profiles: Vec::new(),
            profile: 0,
            normal: true,
            builders_club: true,
            images: true,
            items_lookup: true,
            delay_text: RipOptions::default().delay_ms.to_string(),
            mode: 0,
            show_advanced: false,
            advanced: Advanced::default(),
            compare: None,
            resume: None,
            run: None,
            outcome: None,
            log: Vec::new(),
            message: None,
        };
        ripper.load_profiles();
        ripper
    }

    fn render(&mut self, ui: &mut Ui) {
        if self.run.is_none() {
            self.probe();
        }
        ui.heading(tr("Catalogue ripper"));
        ui.text(tr(
            "Reads the catalogue of the hotel you are logged in to through G-Earth, and saves it as SQL and JSON \
             files (with the images) in a folder you choose.",
        ));
        if let Some((kind, text)) = &self.message {
            ui.banner(*kind, text.as_str());
        }
        self.render_gearth(ui);
        match &self.run {
            Some(run) => Self::render_run(run, ui),
            None => {
                self.render_settings(ui);
                ui.primary_button("rip", tr("Rip catalogue"));
                if let Some(outcome) = &self.outcome {
                    Self::render_outcome(outcome, ui);
                }
                self.render_earlier(ui);
            }
        }
        if !self.log.is_empty() {
            ui.card(tr("Log"), |ui| ui.log(self.log.iter().cloned()));
        }
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        let running = self.run.is_some();
        match event {
            Event::Clicked { id } => match id.as_str() {
                "rip" if !running => self.start(),
                "retry" if !running => self.retry(),
                "compare_pick" if !running => self.pick_compare(),
                "compare_clear" if !running => self.compare = None,
                "resume_pick" if !running => self.pick_resume(),
                "resume_start" if !running => self.start_resume(),
                "resume_close" if !running => self.resume = None,
                "import_log" if !running => self.import_log(),
                // Switching pages stops a rip: only while idle.
                "gearth_tool" if !running => host::open_manager_page(ManagerPage::GearthTool),
                "cancel" => self.cancel(),
                "check" => self.probe.start_sweep(),
                "reveal" => self.reveal(),
                "import" => self.import(),
                _ => {}
            },
            // Settings apply to the next rip; while one runs the page does not show them.
            Event::Changed { id, value } if !running => self.changed(&id, value),
            _ => {}
        }
        // Every event stops the ticks unless it asks again.
        if self.busy() {
            host::keep_working();
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<(), String> {
        match self.run.as_mut() {
            Some(run) => {
                let mut lines = Vec::new();
                let ended = run.tick(&self.clock, &mut lines, self.compare.as_ref());
                self.push_log(lines);
                if let Some(ended) = ended {
                    self.finish(ended);
                }
            }
            None => self.sweep(),
        }
        if self.busy() {
            host::keep_working();
        }
        Ok(())
    }
}

export_plugin!(Ripper);
