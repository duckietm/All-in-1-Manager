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
    add_profile, all_profiles, parse_delay, ConnTransport, FolderSink, HttpFetch, ItemsLoader, ItemsSource, Load, Probe,
    SourceError, SysClock, FIRST_PORT, LAST_PORT, PROFILES_FILE, SQL_LIMIT,
};
use aio_plugin_sdk::folders::{self, Folder};
use aio_plugin_sdk::sql::{Op, Row, Select, Value};
use aio_plugin_sdk::ui::BannerKind;
use aio_plugin_sdk::{export_plugin, files, host, tcp, ErrorKind, Event, Plugin, Ui};
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
    /// The job. `conn` is dropped (closed) as soon as the job is done with G-Earth.
    Rip { job: Box<Job>, conn: Option<tcp::Conn> },
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
    waiting: Option<String>,
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
    delay_text: String,
    delay_ms: u64,
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

fn int(row: &Row, column: &str) -> Result<i64, String> {
    match row.value(column) {
        Some(Value::Int(v)) => Ok(*v),
        Some(Value::Text(t)) => t.trim().parse().map_err(|_| format!("{column} is not a number: {t}")),
        Some(Value::Null) | None => Ok(0),
        Some(_) => Err(format!("{column} is not a number")),
    }
}

fn text(row: &Row, column: &str) -> String {
    match row.value(column) {
        Some(Value::Text(t)) => t.clone(),
        Some(Value::Int(v)) => v.to_string(),
        _ => String::new(),
    }
}

fn items_row(row: &Row) -> Result<ItemsBaseRow, String> {
    let id = int(row, "id")?;
    Ok(ItemsBaseRow {
        id: u32::try_from(id).map_err(|_| format!("id {id} is out of range"))?,
        sprite_id: i32::try_from(int(row, "sprite_id")?).map_err(|_| format!("sprite_id of {id} is out of range"))?,
        item_type: text(row, "type"),
        item_name: text(row, "item_name"),
        effect_id: i32::try_from(int(row, "effect_id_male")?).unwrap_or(0),
    })
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
        Ok(rows.iter().map(items_row).collect())
    }

    fn any_from(&mut self, from: i64) -> Result<bool, SourceError> {
        let rows = Select::from("items_base").columns(["id"]).filter("id", Op::Ge, from).limit(1).run().map_err(source_error)?;
        Ok(!rows.is_empty())
    }
}

fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Connect => "Connecting to G-Earth",
        Phase::WaitHotel => "Waiting for the hotel connection",
        Phase::Learn => "Learning the packet ids from the client",
        Phase::Index => "Reading the catalogue index",
        Phase::Pages => "Reading the catalogue pages",
        Phase::Extras => "Reading the extra catalogue messages",
        Phase::Export => "Writing the files",
        Phase::Images => "Downloading the images",
        Phase::Done => "Done",
    }
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
                        log.push(format!("Read {} items_base rows.", loader.rows.len()));
                        self.items = Some(items_base(&loader.rows));
                        self.stage = Stage::Connect(self.ports());
                        None
                    }
                    Load::Unavailable(why) => {
                        log.push(format!(
                            "items_base cannot be read ({why}): the item ids are written as items_base subqueries, \
                             resolved when the SQL is imported."
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
                        log.push(format!("G-Earth answers on port {port}."));
                        self.port = Some(port);
                        let mut options = self.options.clone();
                        options.gearth_port = port;
                        self.stage = Stage::Rip { job: Box::new(Job::new(options)), conn: Some(conn) };
                        None
                    }
                    Err(e) => {
                        log.push(format!("Port {port}: {e}"));
                        ports.is_empty().then_some(Ended::NoGearth)
                    }
                }
            }
            Stage::Rip { job, conn } => {
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
                };
                let step = job.step(&mut io, JOB_BUDGET_MS);
                for line in job.take_log() {
                    host::log(host::Level::Info, &line);
                    log.push(line);
                }
                // Every event has a log line too; nothing more to show.
                job.take_events();
                if !job.uses_gearth() && conn.take().is_some() {
                    log.push("Closed the connection to G-Earth.".into());
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
                        Some(Ended::Failed(failure))
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
                self.push_log([format!("The saved hotel profiles cannot be read: {e}")]);
                None
            }
        };
        let (profiles, errors) = all_profiles(text.as_deref());
        self.push_log(errors.into_iter().map(|e| format!("Saved hotel profiles: {e}")));
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
            Ok(()) => self.push_log(["The learned packet ids were saved as a hotel profile.".to_string()]),
            Err(e) => self.push_log([format!("The learned hotel profile could not be saved: {e}")]),
        }
        self.load_profiles();
    }

    fn options(&self) -> RipOptions {
        let profile = match self.profile {
            0 => ProfileChoice::Detect(self.profiles.clone()),
            n => ProfileChoice::Fixed(self.profiles[n - 1].clone()),
        };
        RipOptions {
            profile,
            delay_ms: self.delay_ms,
            normal: self.normal,
            builders_club: self.builders_club,
            download_images: self.images,
            ..RipOptions::default()
        }
    }

    fn start(&mut self) {
        if !self.normal && !self.builders_club {
            self.say(BannerKind::Warning, "Choose the normal catalogue, the Builders Club one, or both.");
            return;
        }
        let Some(folder) = folders::pick("Where to save the catalogue") else {
            return;
        };
        self.message = None;
        self.outcome = None;
        self.log.clear();
        self.push_log([format!("Saving into the folder {}.", folder.name())]);
        self.run = Some(Run {
            stage: Stage::Items(ItemsLoader::new()),
            folder,
            options: self.options(),
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
        self.push_log(["Trying again with a new connection to G-Earth.".to_string()]);
        let mut run = Run {
            stage: Stage::Connect(Vec::new()),
            folder: outcome.folder,
            options: outcome.options,
            items: outcome.items,
            order: self.probe.connect_order(),
            port: None,
            waiting: None,
            cancelling: false,
        };
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
            Stage::Items(_) | Stage::Connect(_) => self.finish(Ended::Cancelled),
        }
    }

    fn finish(&mut self, ended: Ended) {
        let Some(run) = self.run.take() else { return };
        match &ended {
            Ended::Done(summary) => {
                self.save_learned(&summary.learned_profile);
                let mut text = format!(
                    "{} pages ({} failed) and {} offers saved in {}/{}. Images: {}.",
                    summary.pages,
                    summary.pages_failed,
                    summary.items,
                    run.folder.name(),
                    summary.folder,
                    summary.images.describe()
                );
                if summary.cancelled {
                    text.push_str(" Cancelled while writing: the catalogue is whole, some images are missing.");
                }
                if !summary.missing_furni.is_empty() {
                    text.push_str(&format!(
                        " {} products are not in this hotel's items_base (missing_furni.json).",
                        summary.missing_furni.len()
                    ));
                }
                self.say(BannerKind::Success, text);
            }
            Ended::Failed(failure) => {
                if let Some(partial) = &failure.partial {
                    self.save_learned(&partial.learned_profile);
                }
                let kind = if failure.cancelled { BannerKind::Info } else { BannerKind::Error };
                let mut text = failure.message.clone();
                if failure.reconnect {
                    text.push_str(" Click Try again to connect to G-Earth again.");
                }
                self.say(kind, text);
            }
            Ended::NoGearth => self.say(
                BannerKind::Error,
                format!(
                    "G-Earth did not answer on ports {FIRST_PORT} to {LAST_PORT}. Start G-Earth and log in to the hotel, \
                     then click Try again."
                ),
            ),
            Ended::Cancelled => self.say(BannerKind::Info, "Cancelled before anything was asked from the hotel."),
        }
        host::status(match &ended {
            Ended::Done(_) => "Catalogue ripped.",
            Ended::Cancelled => "Catalogue rip cancelled.",
            _ => "The catalogue rip stopped.",
        });
        self.outcome = Some(Outcome { ended, folder: run.folder, options: run.options, items: run.items });
        // Look for G-Earth again right away.
        self.probe.now();
    }

    fn import(&mut self) {
        let Some(Outcome { ended: Ended::Done(summary), folder, .. }) = &self.outcome else { return };
        let path = format!("{}/{MANAGER_SQL}", summary.folder);
        if let Err(e) = folder.open_import_preview(&path) {
            self.say(BannerKind::Error, format!("Could not open the import preview: {e}"));
        }
    }

    fn reveal(&mut self) {
        let Some(outcome) = &self.outcome else { return };
        if let Err(e) = outcome.folder.reveal() {
            self.say(BannerKind::Warning, format!("Could not open the folder: {e}"));
        }
    }

    /// While idle: one G-Earth port, at most every 2 s.
    fn probe(&mut self) {
        let now = self.clock.now_ms();
        if let Some(port) = self.probe.due(now) {
            // Connected and closed at once: G-Earth forgets an extension that leaves before it says who it is.
            let answered = tcp::connect(port).is_ok();
            self.probe.record(self.clock.now_ms(), port, answered);
        }
    }

    fn render_gearth(&self, ui: &mut Ui) {
        ui.card("G-Earth", |ui| {
            if let Some(run) = &self.run {
                match (&run.stage, run.port) {
                    (Stage::Rip { conn: Some(_), .. }, Some(port)) => ui.text(format!("Connected on port {port}.")),
                    (Stage::Rip { .. }, _) => ui.text("The rip is done with G-Earth; the connection is closed."),
                    _ => ui.text("Connecting when the rip starts."),
                }
                return;
            }
            match self.probe.found() {
                Some(port) => ui.text(format!("G-Earth answers on port {port}.")),
                None => {
                    ui.text("Start G-Earth and log in to the hotel.");
                    ui.text(format!("Download G-Earth: {GEARTH_RELEASES}"));
                    let tried = match self.probe.last_tried() {
                        Some(port) if self.probe.none_answered() => {
                            format!("No answer on ports {FIRST_PORT} to {LAST_PORT} (last tried: {port}).")
                        }
                        Some(port) => format!("Looking for G-Earth on ports {FIRST_PORT} to {LAST_PORT} (last tried: {port})."),
                        None => format!("Looking for G-Earth on ports {FIRST_PORT} to {LAST_PORT}."),
                    };
                    ui.text(tried);
                }
            }
            ui.button("check", "Check again");
        });
    }

    fn render_settings(&self, ui: &mut Ui) {
        ui.card("Settings", |ui| {
            let options = std::iter::once("Detect from the hotel connection".to_string()).chain(
                self.profiles.iter().map(|p| {
                    let saved = if p.builtin { "" } else { ", saved" };
                    format!("{} ({}{saved})", p.name, p.kind.label())
                }),
            );
            ui.select("profile", options, self.profile);
            ui.checkbox("normal", "Normal catalogue", self.normal);
            ui.checkbox("builders_club", "Builders Club catalogue", self.builders_club);
            ui.checkbox("images", "Download the images", self.images);
            ui.input("delay", "Delay between requests (ms)", self.delay_text.as_str());
        });
    }

    fn render_run(run: &Run, ui: &mut Ui) {
        ui.card("Progress", |ui| match &run.stage {
            Stage::Items(loader) => ui.text(format!("Reading the hotel's items_base ({} rows so far)...", loader.rows.len())),
            Stage::Connect(_) => ui.text("Looking for G-Earth..."),
            Stage::Rip { job, .. } => {
                let progress = job.progress();
                let mut line = phase_text(progress.phase).to_string();
                if progress.total > 0 {
                    line.push_str(&format!(": {} of {}", progress.done, progress.total));
                }
                if !progress.label.is_empty() {
                    line.push_str(&format!(" ({})", progress.label));
                }
                ui.text(line);
                ui.progress(progress.done, progress.total);
                if let Some(waiting) = &run.waiting {
                    ui.banner(BannerKind::Info, waiting.as_str());
                }
            }
        });
        if run.cancelling {
            ui.text("Cancelling...");
        } else {
            ui.button("cancel", "Cancel");
        }
        ui.text("Leaving this page stops the rip.");
    }

    fn render_outcome(outcome: &Outcome, ui: &mut Ui) {
        ui.row(|ui| {
            if let Ended::Done(summary) = &outcome.ended {
                ui.button("reveal", "Open folder");
                if summary.files.iter().any(|f| f == MANAGER_SQL) {
                    ui.primary_button("import", "Import into hotel");
                }
            } else if matches!(&outcome.ended, Ended::Failed(f) if f.partial.is_some()) {
                ui.button("reveal", "Open folder");
            }
            let retry = match &outcome.ended {
                Ended::Failed(failure) => failure.reconnect,
                Ended::NoGearth => true,
                _ => false,
            };
            if retry {
                ui.primary_button("retry", "Try again");
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
            delay_ms: RipOptions::default().delay_ms,
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
        ui.heading("Catalogue ripper");
        ui.text(
            "Reads the catalogue of the hotel you are logged in to through G-Earth, and saves it as SQL and JSON \
             files (with the images) in a folder you choose.",
        );
        if let Some((kind, text)) = &self.message {
            ui.banner(*kind, text.as_str());
        }
        self.render_gearth(ui);
        match &self.run {
            Some(run) => Self::render_run(run, ui),
            None => {
                self.render_settings(ui);
                ui.primary_button("rip", "Rip catalogue");
                if let Some(outcome) = &self.outcome {
                    Self::render_outcome(outcome, ui);
                }
            }
        }
        if !self.log.is_empty() {
            ui.card("Log", |ui| ui.log(self.log.iter().cloned()));
        }
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        let running = self.run.is_some();
        match event {
            Event::Clicked { id } => match id.as_str() {
                "rip" if !running => self.start(),
                "retry" if !running => self.retry(),
                "cancel" => self.cancel(),
                "check" => self.probe.now(),
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
                        Ok(ms) => {
                            self.delay_ms = ms;
                            if matches!(&self.message, Some((BannerKind::Warning, _))) {
                                self.message = None;
                            }
                        }
                        Err(e) => self.say(BannerKind::Warning, format!("{e} The rip uses {} ms.", self.delay_ms)),
                    }
                    self.delay_text = value;
                }
                _ => {}
            },
            _ => {}
        }
        // Every event stops the ticks unless it asks again.
        if self.run.is_some() {
            host::keep_working();
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<(), String> {
        let Some(run) = self.run.as_mut() else { return Ok(()) };
        let mut lines = Vec::new();
        let ended = run.tick(&self.clock, &mut lines);
        self.push_log(lines);
        match ended {
            Some(ended) => self.finish(ended),
            None => host::keep_working(),
        }
        Ok(())
    }
}

export_plugin!(Ripper);
