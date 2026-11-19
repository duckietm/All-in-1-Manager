//! "Catalogue ripper": the built-in catalogue ripper as a plugin, a thin page over `catalog-rip-core`.
//!
//! The user starts G-Earth and logs in to the hotel; the page finds G-Earth on one of the ports 9092 to 9101,
//! "Rip catalogue" asks for the output folder and runs the core's [`Job`] in ticks: first the hotel's items_base
//! (one SQL read per tick), then the G-Earth connection (one port per tick), then [`Job::step`] with
//! [`JOB_BUDGET_MS`] per tick until it is done. The job pumps G-Earth inside its step, so every tick asks for the
//! next one with `keep_working`; Cancel goes in between.
//!
//! `existing` (the hotel's catalogue ids) is not passed: the plugin cannot read catalog_pages / catalog_items, and
//! the default catalog_manager.sql mode replaces the whole catalogue, which does not need them.

// Off wasm32 `export_plugin!` expands to nothing, so the page is unused in a native build (the tests only cover
// the adapters' pure parts).
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod adapters;

use adapters::{
    add_profile, after_failure, all_profiles, drop_items_base, extension_info, generator, items_row, parse_delay, AfterFailure, ConnTransport, FolderSink, HttpFetch, ItemsLoader, ItemsSource, Load, Probe,
    SourceError, SysClock, FIRST_PORT, LAST_PORT, PROFILES_FILE, SQL_LIMIT,
};
use aio_plugin_sdk::folders::{self, Folder};
use aio_plugin_sdk::i18n::{tr, tr_with};
use aio_plugin_sdk::sql::{Op, Select};
use aio_plugin_sdk::ui::BannerKind;
use aio_plugin_sdk::{export_plugin, files, host, tcp, ErrorKind, Event, Plugin, Ui};
use catalog_rip_core::Message;
use catalog_rip_core::output::items_base;
use catalog_rip_core::transport::{Closed, Transport};
use catalog_rip_core::{
    Clock, Failure, HotelProfile, ItemsBase, ItemsBaseRow, Job, JobIo, Phase, ProfileChoice, RipOptions, Step, Summary,
};

/// What one tick gives the job; the rest of the tick's 5 s is margin (the job's G-Earth waits are host I/O).
const JOB_BUDGET_MS: u32 = 200;
/// Log lines the page keeps.
const MAX_LOG: usize = 400;
const GEARTH_RELEASES: &str = "https://github.com/sirjonasxx/G-Earth/releases";
const MANAGER_SQL: &str = "catalog_manager.sql";

/// Where a running rip is.
enum Stage {
    /// Reading the hotel's items_base, one query per tick.
    Items(ItemsLoader),
    /// Looking for G-Earth: the ports left to try (the next one last), one per tick.
    Connect(Vec<u16>),
    /// The job. `conn` is dropped (closed) as soon as the job is done with G-Earth. `rest`: the
    /// ports still to try if this one does not answer as G-Earth.
    Rip { job: Box<Job>, conn: Option<tcp::Conn>, rest: Vec<u16> },
}

/// A rip in progress.
struct Run {
    stage: Stage,
    folder: Folder,
    options: RipOptions,
    items: Option<ItemsBase>,
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
    folder: Folder,
    options: RipOptions,
    items: Option<ItemsBase>,
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
    /// The delay input's text, checked when a rip starts.
    delay_text: String,
    run: Option<Run>,
    outcome: Option<Outcome>,
    log: Vec<String>,
    message: Option<(BannerKind, String)>,
}

/// items_base through the Manager's SQL (permission `sql:read:items_base`).
struct SqlItems;

fn source_error(e: aio_plugin_sdk::Error) -> SourceError {
    SourceError { denied: e.kind() == ErrorKind::Denied, message: e.message().to_string() }
}

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

impl Run {
    /// One tick of work; `Some` when the rip ended. Log lines go to `log`.
    fn tick(&mut self, clock: &SysClock, log: &mut Vec<String>) -> Option<Ended> {
        match &mut self.stage {
            Stage::Items(loader) => {
                let load = loader.step(&mut SqlItems);
                log.extend(loader.take_notes());
                match load {
                    Load::More => None,
                    Load::Done => {
                        log.push(tr_with("Read {count} items_base rows.", &[("count", &loader.rows.len().to_string())]));
                        self.items = Some(items_base(&loader.rows));
                        self.stage = Stage::Connect(self.ports());
                        None
                    }
                    Load::Unavailable(why) => {
                        log.push(tr_with(
                            "items_base cannot be read ({why}): the item ids are written as items_base subqueries, \
                             resolved when the SQL is imported.",
                            &[("why", &why)],
                        ));
                        self.stage = Stage::Connect(self.ports());
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
                        self.stage = Stage::Rip { job: Box::new(Job::new(options)), conn: Some(conn), rest };
                        None
                    }
                    Err(e) => {
                        log.push(tr_with("Port {port}: {error}", &[("port", &port.to_string()), ("error", &e.to_string())]));
                        ports.is_empty().then_some(Ended::NoGearth)
                    }
                }
            }
            Stage::Rip { job, conn, rest } => {
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
                let mut sink = FolderSink(&self.folder);
                let mut io = JobIo {
                    transport,
                    fetch: &mut fetch,
                    sink: &mut sink,
                    clock,
                    items: self.items.as_ref(),
                    existing: None,
                    // Resuming, importing a Packet Logger export and the comparison with an
                    // earlier rip read the picked folder (wired with the settings for them).
                    source: None,
                };
                let step = job.step(&mut io, JOB_BUDGET_MS);
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
                // Memory: the files still to write (catalog.json, the biggest) do not need it. Kept after a
                // failure: the next port's job or Try again reuses it.
                if drop_items_base(&step, job.uses_items_base()) {
                    self.items = None;
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
        if self.log.len() > MAX_LOG {
            let extra = self.log.len() - MAX_LOG;
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

    fn options(&self, delay_ms: u64) -> RipOptions {
        let profile = match self.profile {
            0 => ProfileChoice::Detect(self.profiles.clone()),
            n => ProfileChoice::Fixed(self.profiles[n - 1].clone()),
        };
        RipOptions {
            profile,
            delay_ms,
            normal: self.normal,
            builders_club: self.builders_club,
            download_images: self.images,
            generator: generator(),
            extension: extension_info(),
            ..RipOptions::default()
        }
    }

    fn start(&mut self) {
        if !self.normal && !self.builders_club {
            self.say(BannerKind::Warning, tr("Choose the normal catalogue, the Builders Club one, or both."));
            return;
        }
        let delay_ms = match parse_delay(&self.delay_text) {
            Ok(ms) => ms,
            Err(e) => {
                self.say(BannerKind::Warning, format!("{e} {}", tr("Correct it, then click Rip catalogue again.")));
                return;
            }
        };
        let Some(folder) = folders::pick(&tr("Where to save the catalogue")) else {
            return;
        };
        self.message = None;
        self.outcome = None;
        self.log.clear();
        self.push_log([tr_with("Saving into the folder {folder}.", &[("folder", &folder.name())])]);
        self.run = Some(Run {
            stage: Stage::Items(ItemsLoader::new()),
            folder,
            options: self.options(delay_ms),
            items: None,
            order: self.probe.connect_order(),
            port: None,
            waiting: None,
            cancelling: false,
        });
    }

    /// "Try again" after G-Earth let the extension go: a new job on a new connection, same folder and settings.
    fn retry(&mut self) {
        let Some(outcome) = self.outcome.take() else { return };
        self.message = None;
        self.push_log([tr("Trying again with a new connection to G-Earth.")]);
        let order = self.probe.connect_order();
        self.run = Some(Run {
            stage: Stage::Connect(order.iter().rev().copied().collect()),
            folder: outcome.folder,
            options: outcome.options,
            items: outcome.items,
            order,
            port: None,
            waiting: None,
            cancelling: false,
        });
    }

    fn cancel(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        match &mut run.stage {
            Stage::Rip { job, .. } => {
                job.cancel();
                run.cancelling = true;
            }
            // Nothing was asked from the hotel yet.
            Stage::Items(_) | Stage::Connect(_) => self.finish(Ended::Cancelled),
        }
    }

    fn finish(&mut self, ended: Ended) {
        let Some(run) = self.run.take() else { return };
        match &ended {
            Ended::Done(summary) => {
                self.save_learned(&summary.learned_profile);
                let mut text = tr_with(
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
                    text.push(' ');
                    text.push_str(&tr("Cancelled while writing: the catalogue is whole, some images are missing."));
                }
                if !summary.missing_furni.is_empty() {
                    text.push(' ');
                    text.push_str(&tr_with(
                        "{count} products are not in this hotel's items_base (missing_furni.json).",
                        &[("count", &summary.missing_furni.len().to_string())],
                    ));
                }
                self.say(BannerKind::Success, text);
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
        self.outcome = Some(Outcome { ended, folder: run.folder, options: run.options, items: run.items });
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
                match (&run.stage, run.port) {
                    (Stage::Rip { conn: Some(_), .. }, Some(port)) => ui.text(tr_with("Connected on port {port}.", &[("port", &port.to_string())])),
                    (Stage::Rip { .. }, _) => ui.text(tr("The rip is done with G-Earth; the connection is closed.")),
                    _ => ui.text(tr("Connecting when the rip starts.")),
                }
                return;
            }
            ui.text(self.probe.status());
            if self.probe.found().is_none() {
                ui.text(tr_with("Download G-Earth: {address}", &[("address", GEARTH_RELEASES)]));
            }
            if self.probe.sweep_port().is_none() {
                ui.button("check", tr("Check again"));
            }
        });
    }

    fn render_settings(&self, ui: &mut Ui) {
        ui.card(tr("Settings"), |ui| {
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
            ui.input("delay", tr("Delay between requests (ms)"), self.delay_text.as_str());
        });
    }

    fn render_run(run: &Run, ui: &mut Ui) {
        ui.card(tr("Progress"), |ui| match &run.stage {
            Stage::Items(loader) => ui.text(tr_with("Reading the hotel's items_base ({count} rows so far)...", &[("count", &loader.rows.len().to_string())])),
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
            delay_text: RipOptions::default().delay_ms.to_string(),
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
                "cancel" => self.cancel(),
                "check" => self.probe.start_sweep(),
                "reveal" => self.reveal(),
                "import" => self.import(),
                _ => {}
            },
            // Settings apply to the next rip; while one runs the page does not show them.
            Event::Changed { id, value } if !running => match id.as_str() {
                "profile" => {
                    if let Ok(n) = value.parse::<usize>() {
                        self.profile = n.min(self.profiles.len());
                    }
                }
                "normal" => self.normal = value == "true",
                "builders_club" => self.builders_club = value == "true",
                "images" => self.images = value == "true",
                "delay" => {
                    match parse_delay(&value) {
                        Ok(_) => {
                            if matches!(&self.message, Some((BannerKind::Warning, _))) {
                                self.message = None;
                            }
                        }
                        Err(e) => self.say(BannerKind::Warning, e),
                    }
                    self.delay_text = value;
                }
                _ => {}
            },
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
                let ended = run.tick(&self.clock, &mut lines);
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
