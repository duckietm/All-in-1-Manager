//! The Room ripper: copies rooms from official Habbo or a retro through G-Earth (aio-catalog-rip's room module) and
//! imports them into the hotel as new rooms. The capture is passive: every room the user enters with their own
//! client is saved (configuration/plugins/room-rip/captures/); "Read the wired" opens the room's wired boxes where
//! the hotel lets the user read them. The import shows what it will make first (furni matched by class name, what
//! is cleaned off, wired converted or empty), writes everything in one transaction, and keeps a record for Undo.

use std::path::PathBuf;
use std::sync::Arc;

use aio_catalog_rip::room::{self, BaseItem, CaptureEvent, CaptureOptions, Command, Door, ImportChoice, ImportPlan, ImportRecord, PlanValue, Recording, RoomCapture, Target};
use aio_catalog_rip::{HotelProfile, ProfileChoice};
use aio_remote::sql_session::{Linked, SqlSession, SqlValue};
use aio_remote::Server;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::gui::dialogs::Choice;
use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// configuration/plugins/room-rip/: captures, import records and G-Earth (shared with the catalogue ripper).
fn plugin_folder() -> PathBuf {
    aio_core::config_dir::directory().join("plugins").join("room-rip")
}

fn captures_folder() -> PathBuf {
    plugin_folder().join("captures")
}

fn imports_folder() -> PathBuf {
    plugin_folder().join("imports")
}

fn packets_folder() -> PathBuf {
    plugin_folder().join("packets")
}

fn open_folder(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    crate::gui::open_url(&path.display().to_string());
}

fn gearth_folder() -> PathBuf {
    aio_core::config_dir::directory().join("plugins").join("catalog-rip").join("G-Earth")
}

fn profiles_folder() -> PathBuf {
    aio_core::config_dir::directory().join("plugins").join("catalog-rip").join("profiles")
}

type Banner = Option<(ResultKind, String, String)>;

#[derive(Default)]
struct State {
    capturing: bool,
    log: LogLines,
    cancel: Option<CancellationToken>,
    commands: Option<UnboundedSender<Command>>,
    connected: Option<String>,
    wired: Option<(usize, usize)>,
    reading: bool,
    /// The packet recording: waiting for a room, or recording (packets, wired packets).
    recording: Option<(bool, usize, usize)>,
    /// The folder of the last finished recording.
    recorded: Option<PathBuf>,
    /// The captures changed: the list is read again.
    refresh: bool,
    error: Option<String>,
    /// The preview of the import chosen, or what the import or undo did.
    plan: Option<(PathBuf, ImportPlan)>,
    working: bool,
    banner: Banner,
}

/// The import form of one capture.
struct Form {
    path: PathBuf,
    capture: RoomCapture,
    owner: String,
    name: String,
    door: Door,
}

pub struct RoomRipView {
    state: Arc<Mutex<State>>,
    profiles: Vec<HotelProfile>,
    profile: Option<String>,
    furnidata_url: String,
    captures: Vec<(PathBuf, RoomCapture)>,
    imports: Vec<(PathBuf, ImportRecord)>,
    form: Option<Form>,
}

fn banner(kind: ResultKind, heading: &str, message: &str) -> Banner {
    Some((kind, heading.to_string(), message.to_string()))
}

impl RoomRipView {
    pub fn new(_shared: &Shared) -> Self {
        let mut view = Self {
            state: Arc::new(Mutex::new(State { log: LogLines::with_limit(3000), ..State::default() })),
            profiles: aio_catalog_rip::load_profiles(&profiles_folder()).0,
            profile: None,
            furnidata_url: String::new(),
            captures: Vec::new(),
            imports: Vec::new(),
            form: None,
        };
        view.reload();
        view
    }

    fn reload(&mut self) {
        self.captures = room::list_captures(&captures_folder());
        self.imports = std::fs::read_dir(imports_folder())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter_map(|p| std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<ImportRecord>(&t).ok()).map(|r| (p, r)))
            .collect();
        self.imports.sort_by(|a, b| b.1.imported_at.cmp(&a.1.imported_at));
    }

    pub fn busy(&self) -> bool {
        let state = self.state.lock();
        state.capturing || state.working
    }

    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        if std::mem::take(&mut self.state.lock().refresh) {
            self.reload();
        }
        self.capture_card(ui, palette, shared);
        ui.add_space(14.0);
        self.rooms_card(ui, palette, shared);
        if self.form.is_some() {
            ui.add_space(14.0);
            self.import_card(ui, palette, shared);
        }
        if !self.imports.is_empty() {
            ui.add_space(14.0);
            self.imports_card(ui, palette, shared);
        }
    }

    // ---- Capture ----

    fn capture_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let capturing = self.state.lock().capturing;
        widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
            ui.label(widgets::title_text(t("Capture rooms"), 17.0));
            ui.add_space(4.0);
            for (n, line) in [
                t("Start G-Earth (the ripper can download and start it for you), connect it to the hotel and log in with your own client."),
                t("Click Start, then walk into the rooms you want: each room you enter is saved with its floor plan, decoration and furni."),
                t("In a room whose wired you may read (your own, with rights, or where everyone may inspect it), click Read the wired."),
            ]
            .iter()
            .enumerate()
            {
                ui.label(RichText::new(format!("{}. {line}", n + 1)).size(12.5));
            }
            widgets::muted(
                ui,
                palette,
                t("Official Habbo and retros (Flash and Nitro) work. Nothing in the other hotel is changed: the capture only listens, and reading the wired opens the boxes the way their owner does. Copy only rooms you may copy."),
                12.0,
            );
            ui.add_space(8.0);
            ui.add_enabled_ui(!capturing, |ui| {
                egui::Grid::new("room-rip-options").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label(t("Hotel profile"));
                    let selected = self.profile.clone().unwrap_or_else(|| t("Detect from the connection").to_string());
                    egui::ComboBox::from_id_salt("room-rip-profile").selected_text(selected).width(320.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.profile, None, t("Detect from the connection"));
                        for profile in &self.profiles {
                            ui.selectable_value(&mut self.profile, Some(profile.name.clone()), &profile.name);
                        }
                    });
                    ui.end_row();
                    ui.label(t("Furnidata of that hotel"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.furnidata_url)
                            .hint_text(t("Not needed for official Habbo; for a retro its furnidata URL"))
                            .desired_width(420.0),
                    )
                    .on_hover_text(t("The class names of the furni: to match them with your items_base and to know which furni are wired boxes."));
                    ui.end_row();
                });
            });
            ui.add_space(8.0);
            let (reading, wired, connected) = {
                let state = self.state.lock();
                (state.reading, state.wired, state.connected.clone())
            };
            ui.horizontal(|ui| {
                if capturing {
                    if ui.add(widgets::secondary_button(t("Stop"))).clicked() {
                        if let Some(cancel) = &self.state.lock().cancel {
                            cancel.cancel();
                        }
                    }
                    if reading {
                        if ui.add(widgets::secondary_button(t("Stop reading"))).clicked() {
                            self.command(Command::StopReading);
                        }
                    } else if ui.add_enabled(connected.is_some(), widgets::primary_button(t("Read the wired"))).clicked() {
                        self.state.lock().reading = true;
                        self.command(Command::ReadWired);
                    }
                } else if ui.add(widgets::primary_button(t("Start"))).clicked() {
                    self.start(shared);
                }
                if let Some(connected) = &connected {
                    ui.label(RichText::new(connected).size(12.0).color(palette.success_text));
                }
            });
            if capturing {
                self.recording_row(ui, palette);
            }
            if let Some(folder) = self.state.lock().recorded.clone() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tf("Packet recording saved in {folder}", &[("folder", &folder.display().to_string())])).size(12.0).color(palette.success_text));
                    if ui.add(widgets::secondary_button(t("Open the recording"))).clicked() {
                        open_folder(&folder);
                    }
                });
            }
            if let Some((done, total)) = wired.filter(|_| reading) {
                ui.add_space(6.0);
                ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).text(tf("Wired: {done} of {total} boxes", &[("done", &done.to_string()), ("total", &total.to_string())])));
            }
            if let Some(error) = self.state.lock().error.clone() {
                ui.add_space(6.0);
                widgets::result_banner(ui, palette, ResultKind::Error, t("The capture stopped"), &error);
            }
            ui.add_space(8.0);
            log_view::show(ui, "room-rip", &mut self.state.lock().log, Some(200.0), t("The capture's log appears here."));
        });
    }

    /// For developers: record every packet, both ways.
    fn recording_row(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let recording = self.state.lock().recording;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            match recording {
                None => {
                    if ui
                        .add(widgets::secondary_button(t("Record every packet (developers)")))
                        .on_hover_text(t("Records every packet between your client and the hotel, both ways, from the next room you enter: packets.jsonl (all of them) and wired.txt (the wired ones, decoded). For building the wired into the emulator."))
                        .clicked()
                    {
                        self.state.lock().recorded = None;
                        self.command(Command::RecordPackets(true));
                    }
                }
                Some((waiting, packets, wired)) => {
                    if ui.add(widgets::secondary_button(t("Stop recording"))).clicked() {
                        self.command(Command::RecordPackets(false));
                    }
                    let text = if waiting {
                        t("Recording starts in the next room you enter (so your login is not in it): enter or reload a room.").to_string()
                    } else {
                        tf("Recording: {packets} packets, {wired} of them wired", &[("packets", &packets.to_string()), ("wired", &wired.to_string())])
                    };
                    ui.label(RichText::new(text).size(12.0).color(palette.warning_text));
                }
            }
        });
        if recording.is_some() {
            widgets::muted(ui, palette, t("The recording holds everything your client and the hotel send in those rooms, chat too: share it only with people you trust."), 11.5);
        }
    }

    fn command(&self, command: Command) {
        if let Some(commands) = &self.state.lock().commands {
            let _ = commands.send(command);
        }
    }

    fn start(&mut self, shared: &Shared) {
        let cancel = CancellationToken::new();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        {
            let mut state = self.state.lock();
            state.capturing = true;
            state.log.clear();
            state.error = None;
            state.connected = None;
            state.wired = None;
            state.reading = false;
            state.recording = None;
            state.recorded = None;
            state.cancel = Some(cancel.clone());
            state.commands = Some(commands);
        }
        let options = CaptureOptions {
            folder: captures_folder(),
            furnidata_url: Some(self.furnidata_url.trim().to_string()).filter(|s| !s.is_empty()),
            gearth_folder: Some(gearth_folder()),
            packets_folder: packets_folder(),
            ..CaptureOptions::default()
        };
        let choice = match self.profile.as_ref().and_then(|name| self.profiles.iter().find(|p| &p.name == name)) {
            Some(profile) => ProfileChoice::Fixed(profile.clone()),
            None => ProfileChoice::Detect(self.profiles.clone()),
        };
        let state = self.state.clone();
        let task_shared = shared.clone();
        shared.set_status(t("Capturing rooms: enter the rooms you want to copy."));
        shared.spawn(async move {
            let shared = task_shared;
            let ctx = shared.ctx.clone();
            let log_state = state.clone();
            let log_ctx = ctx.clone();
            let log = move |line: &str| {
                log_state.lock().log.push(line);
                log_ctx.request_repaint();
            };
            let dialogs = shared.dialogs.clone();
            let ask = move |question: String| -> futures::future::BoxFuture<'static, bool> {
                let dialogs = dialogs.clone();
                Box::pin(async move {
                    let options = vec![Choice::new(t("Yes"), "", true).primary(), Choice::new(t("No"), "", false)];
                    dialogs.choose(t("Room ripper"), t("Room ripper"), &question, false, options).await
                })
            };
            let event_state = state.clone();
            let sink = move |event: CaptureEvent| {
                let mut s = event_state.lock();
                match event {
                    CaptureEvent::Connected { host, client_type, profile } => s.connected = Some(tf("Connected: {host} ({client}, {profile})", &[("host", &host), ("client", &client_type), ("profile", &profile)])),
                    CaptureEvent::Room { .. } => s.refresh = true,
                    CaptureEvent::WiredProgress { done, total } => s.wired = Some((done, total)),
                    CaptureEvent::WiredDone { .. } => {
                        s.reading = false;
                        s.refresh = true;
                    }
                    CaptureEvent::Recording(recording) => match recording {
                        Recording::Waiting => s.recording = Some((true, 0, 0)),
                        Recording::Running { packets, wired } => s.recording = Some((false, packets, wired)),
                        Recording::Finished(folder) => {
                            s.recording = None;
                            s.recorded = Some(folder);
                        }
                        Recording::Off => s.recording = None,
                    },
                }
                ctx.request_repaint();
            };
            let result = room::capture(&options, choice, &log, &ask, &sink, receiver, &cancel).await;
            let mut s = state.lock();
            s.capturing = false;
            s.reading = false;
            s.recording = None;
            s.cancel = None;
            s.commands = None;
            s.connected = None;
            s.refresh = true;
            if let Err(error) = result {
                s.error = Some(format!("{error:#}"));
            }
            shared.set_status(t("The room capture stopped."));
        });
    }

    // ---- The captured rooms ----

    fn rooms_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let mut open: Option<usize> = None;
        let mut delete: Option<usize> = None;
        widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
            ui.horizontal(|ui| {
                ui.label(widgets::title_text(t("Captured rooms"), 17.0));
                if ui.add(widgets::secondary_button(t("Open the folder"))).on_hover_text(captures_folder().display().to_string()).clicked() {
                    open_folder(&captures_folder());
                }
            });
            if self.captures.is_empty() {
                widgets::muted(ui, palette, t("No rooms yet: start the capture and enter a room."), 12.5);
                return;
            }
            ui.add_space(6.0);
            egui::Grid::new("room-rip-captures").num_columns(5).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                for h in [t("Room"), t("Hotel"), t("Furni"), t("Wired read"), ""] {
                    ui.label(RichText::new(h).size(12.0).strong().color(palette.muted));
                }
                ui.end_row();
                for (index, (_, capture)) in self.captures.iter().enumerate() {
                    let summary = capture.summary();
                    ui.label(RichText::new(&summary.name).strong()).on_hover_text(tf("Room {id}, by {owner}", &[("id", &summary.room_id.to_string()), ("owner", &summary.owner)]));
                    ui.label(&capture.hotel);
                    ui.label(tf("{floor} floor, {wall} wall", &[("floor", &summary.floor_items.to_string()), ("wall", &summary.wall_items.to_string())]));
                    let wired = if summary.wired_boxes == 0 { "-".to_string() } else { format!("{} / {}", summary.wired_read, summary.wired_boxes) };
                    ui.label(wired);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(shared.session().is_some(), widgets::primary_button(t("Import ..."))).on_disabled_hover_text(t("Log in to the server of your hotel first.")).clicked() {
                            open = Some(index);
                        }
                        if ui.add(widgets::secondary_button(t("Delete"))).clicked() {
                            delete = Some(index);
                        }
                    });
                    ui.end_row();
                }
            });
        });
        if let Some(index) = open {
            let (path, capture) = self.captures[index].clone();
            let door = capture.door_or_guess().unwrap_or(Door { x: 0, y: 0, dir: 2 });
            self.form = Some(Form { path, owner: String::new(), name: capture.name(), door, capture });
            self.state.lock().plan = None;
            self.state.lock().banner = None;
        }
        if let Some(index) = delete {
            let path = self.captures[index].0.clone();
            let _ = std::fs::remove_file(&path);
            if self.form.as_ref().is_some_and(|f| f.path == path) {
                self.form = None;
            }
            self.reload();
        }
    }

    // ---- Import ----

    fn import_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let (working, plan, banner) = {
            let s = self.state.lock();
            (s.working, s.plan.clone(), s.banner.clone())
        };
        let mut close = false;
        let mut preview = false;
        let mut import = false;
        let Some(form) = self.form.as_mut() else { return };
        widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
            ui.horizontal(|ui| {
                ui.label(widgets::title_text(&tf("Import {room} into the hotel", &[("room", &form.capture.name())]), 17.0));
                if ui.small_button("✕").clicked() {
                    close = true;
                }
            });
            widgets::muted(
                ui,
                palette,
                t("A new room of the owner you choose, open to everyone, with the floor plan, decoration and furni of the capture. The furni are matched with your items_base by class name; LTD serials, engravings, group links and ads of the other hotel are removed. Nothing is written before you click Import, and Undo removes the room again."),
                12.0,
            );
            ui.add_space(6.0);
            let mut changed = false;
            ui.add_enabled_ui(!working, |ui| {
                egui::Grid::new("room-rip-import").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label(t("Owner (user name)"));
                    changed |= ui.add(egui::TextEdit::singleline(&mut form.owner).hint_text(t("a user of your hotel")).desired_width(260.0)).changed();
                    ui.end_row();
                    ui.label(t("Room name"));
                    changed |= ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(260.0)).changed();
                    ui.end_row();
                    ui.label(t("Door"));
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(&mut form.door.x).prefix("x ").range(0..=200)).changed();
                        changed |= ui.add(egui::DragValue::new(&mut form.door.y).prefix("y ").range(0..=200)).changed();
                        changed |= ui.add(egui::DragValue::new(&mut form.door.dir).prefix(t("direction ")).range(0..=7)).changed();
                        if !form.capture.door_from_hotel {
                            ui.label(RichText::new(t("guessed from the floor plan: check it")).size(11.5).color(palette.warning_text));
                        }
                    });
                    ui.end_row();
                });
            });
            if changed {
                self.state.lock().plan = None;
            }
            ui.add_space(8.0);
            let previewed = plan.as_ref().is_some_and(|(p, _)| p == &form.path);
            ui.horizontal(|ui| {
                if ui.add_enabled(!working && !form.owner.trim().is_empty(), widgets::secondary_button(t("Preview"))).clicked() {
                    preview = true;
                }
                if ui.add_enabled(!working && previewed, widgets::primary_button(t("Import"))).clicked() {
                    import = true;
                }
                if working {
                    ui.spinner();
                }
            });
            if let Some((_, plan)) = plan.filter(|(p, _)| p == &form.path) {
                ui.add_space(8.0);
                report(ui, palette, &plan);
            }
            if let Some((kind, heading, message)) = &banner {
                ui.add_space(8.0);
                widgets::result_banner(ui, palette, *kind, heading, message);
            }
        });
        if close {
            self.form = None;
            return;
        }
        if preview || import {
            if let Some(server) = shared.session() {
                self.run_import(shared, server, import);
            }
        }
    }

    fn run_import(&mut self, shared: &Shared, server: Arc<dyn Server>, write: bool) {
        let Some(form) = &self.form else { return };
        let (capture, path, owner, name, door) = (form.capture.clone(), form.path.clone(), form.owner.trim().to_string(), form.name.trim().to_string(), form.door);
        let state = self.state.clone();
        let shared_task = shared.clone();
        {
            let mut s = state.lock();
            s.working = true;
            s.banner = None;
        }
        shared.spawn(async move {
            let result = import_task(&*server, &capture, &owner, &name, door, write).await;
            let mut s = state.lock();
            s.working = false;
            match result {
                Ok((plan, None)) => s.plan = Some((path, plan)),
                Ok((_, Some(record))) => {
                    s.plan = None;
                    s.refresh = true;
                    s.banner = banner(
                        ResultKind::Success,
                        &tf("{room} is in the hotel", &[("room", &record.room_name)]),
                        &tf("Room {id} of {owner}, with {items} furni. Undo (below) removes it again.", &[("id", &record.room_id.to_string()), ("owner", &record.owner_name), ("items", &record.item_ids.len().to_string())]),
                    );
                    shared_task.set_status(&tf("{room} is imported as room {id}.", &[("room", &record.room_name), ("id", &record.room_id.to_string())]));
                }
                Err(error) => s.banner = banner(ResultKind::Error, if write { t("The import failed: nothing was written") } else { t("The preview failed") }, &format!("{error:#}")),
            }
            shared_task.ctx.request_repaint();
        });
    }

    // ---- Imported rooms ----

    fn imports_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let working = self.state.lock().working;
        let mut undo: Option<usize> = None;
        widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
            ui.label(widgets::title_text(t("Imported rooms"), 17.0));
            widgets::muted(ui, palette, t("Undo removes the room and the furni that came with it (also when they were picked up since). Anyone in the room is sent to the hotel view first."), 12.0);
            ui.add_space(6.0);
            egui::Grid::new("room-rip-imports").num_columns(4).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                for (index, (_, record)) in self.imports.iter().enumerate() {
                    ui.label(RichText::new(&record.room_name).strong());
                    ui.label(tf("room {id} of {owner} on {server}", &[("id", &record.room_id.to_string()), ("owner", &record.owner_name), ("server", &record.server)]));
                    ui.label(RichText::new(&record.imported_at).size(11.5).color(palette.subtle));
                    let here = shared.session().is_some_and(|s| s.login().host == record.server);
                    if ui
                        .add_enabled(here && !working, widgets::secondary_button(t("Undo")))
                        .on_disabled_hover_text(tf("Log in to {server} to undo it.", &[("server", &record.server)]))
                        .clicked()
                    {
                        undo = Some(index);
                    }
                    ui.end_row();
                }
            });
        });
        if let (Some(index), Some(server)) = (undo, shared.session()) {
            let (path, record) = self.imports[index].clone();
            let state = self.state.clone();
            let dialogs = shared.dialogs.clone();
            let shared_task = shared.clone();
            shared.spawn(async move {
                let options = vec![Choice::new(t("Remove the room"), t("The room, its floor plan and the furni that came with it are deleted."), true).primary()];
                let question = tf("Remove {room} (room {id}) from the hotel again?", &[("room", &record.room_name), ("id", &record.room_id.to_string())]);
                if !dialogs.choose(t("Undo the import"), &question, t("Anyone in the room is sent to the hotel view."), false, options).await {
                    return;
                }
                state.lock().working = true;
                let result = undo_task(&*server, &record).await;
                let mut s = state.lock();
                s.working = false;
                s.refresh = true;
                match result {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&path);
                        s.banner = banner(ResultKind::Success, t("Undone"), &tf("{room} is removed from the hotel.", &[("room", &record.room_name)]));
                    }
                    Err(error) => s.banner = banner(ResultKind::Error, t("Undo failed: nothing was removed"), &format!("{error:#}")),
                }
                shared_task.ctx.request_repaint();
            });
        }
    }
}

fn report(ui: &mut egui::Ui, palette: &Palette, plan: &ImportPlan) {
    let r = &plan.report;
    let line = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(text).size(12.5));
    };
    ui.label(RichText::new(t("What the import makes")).size(13.5).strong());
    line(ui, tf("{floor} floor and {wall} wall furni", &[("floor", &r.floor_items.to_string()), ("wall", &r.wall_items.to_string())]));
    line(
        ui,
        if r.custom_model { t("Its own floor plan (a custom model)").to_string() } else { tf("The hotel's model {model}", &[("model", &r.model)]) },
    );
    if r.wired_converted > 0 {
        line(ui, tf("{count} wired boxes with their settings", &[("count", &r.wired_converted.to_string())]));
    }
    if r.matched_by_sprite > 0 {
        line(ui, tf("{count} furni matched by sprite id (their class name was not known)", &[("count", &r.matched_by_sprite.to_string())]));
    }
    let list = |ui: &mut egui::Ui, heading: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(RichText::new(format!("{heading} ({})", items.len())).size(12.5).color(palette.warning_text)).show(ui, |ui| {
            for item in items.iter().take(200) {
                ui.label(RichText::new(item).size(12.0).monospace());
            }
        });
    };
    list(ui, t("Not in your items_base, left out"), r.missing.iter().map(|(name, n)| format!("{n} × {name}")).collect());
    list(ui, t("Wired boxes that come empty"), r.wired_empty.clone());
    list(ui, t("Cleaned off"), r.cleaned.clone());
    list(ui, t("Notes"), r.notes.clone());
}

fn to_linked(value: &PlanValue) -> Linked {
    match value {
        PlanValue::Int(i) => Linked::Value(SqlValue::Int(*i)),
        PlanValue::Double(d) => Linked::Value(SqlValue::Double(*d)),
        PlanValue::Text(text) => Linked::Value(SqlValue::Text(text.clone())),
        PlanValue::InsertId(n) => Linked::InsertId(*n),
        PlanValue::Template(text) => Linked::Text(text.clone()),
    }
}

fn int(value: &SqlValue) -> Option<i64> {
    match value {
        SqlValue::Int(i) => Some(*i),
        SqlValue::UInt(u) => i64::try_from(*u).ok(),
        SqlValue::Text(s) | SqlValue::Decimal(s) => s.trim().parse().ok(),
        SqlValue::Bytes(b) => String::from_utf8_lossy(b).trim().parse().ok(),
        _ => None,
    }
}

fn text(value: &SqlValue) -> String {
    match value {
        SqlValue::Text(s) => s.clone(),
        SqlValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        SqlValue::Null => String::new(),
        other => int(other).map(|i| i.to_string()).unwrap_or_default(),
    }
}

/// The hotel's database through MariaDB, with the saved account (as the other database pages).
async fn open_hotel(server: &dyn Server) -> anyhow::Result<(SqlSession, String)> {
    let host = server.login().host.clone();
    let saved = aio_core::config_dir::load_sql_login(&host)
        .ok_or_else(|| anyhow::anyhow!("No saved MariaDB account for {host}: open the Catalogue manager once to log in to MariaDB."))?;
    let user = if saved.root_user.is_empty() { "root".to_string() } else { saved.root_user.clone() };
    let port = if saved.port > 0 { saved.port } else { 3306 };
    let database = aio_remote::catalog_admin::hotel_database(server).await?;
    let session = SqlSession::open(server, &user, &saved.root_password, port).await?;
    Ok((session, database))
}

/// Reads what the plan needs from the hotel and plans the import; with `write`, runs it and keeps the record.
async fn import_task(server: &dyn Server, capture: &RoomCapture, owner: &str, name: &str, door: Door, write: bool) -> anyhow::Result<(ImportPlan, Option<ImportRecord>)> {
    let (session, database) = open_hotel(server).await?;
    let result = async {
        let db = aio_remote::sql_session::quote(&database);
        let owner_row = session.select(&format!("SELECT `id`, `username` FROM {db}.`users` WHERE `username` = ? LIMIT 1"), vec![SqlValue::Text(owner.to_string())]).await?;
        let Some(owner_row) = owner_row.first() else {
            anyhow::bail!("There is no user {owner} in the hotel: the owner must be a user of your hotel.");
        };
        let (owner_id, owner_name) = (owner_row.first().and_then(int).unwrap_or(0), owner_row.get(1).map(text).unwrap_or_default());
        let items_base = session
            .select(&format!("SELECT `id`, `sprite_id`, `item_name`, `type`, `interaction_type` FROM {db}.`items_base`"), Vec::new())
            .await?
            .into_iter()
            .filter_map(|row| {
                Some(BaseItem {
                    id: u32::try_from(int(row.first()?)?).ok()?,
                    sprite_id: row.get(1).and_then(int).unwrap_or(0) as i32,
                    item_name: text(row.get(2)?),
                    item_type: text(row.get(3)?),
                    interaction_type: text(row.get(4)?),
                })
            })
            .collect();
        let mut target = Target { database: database.clone(), items_base, ..Target::default() };
        let columns = session
            .select(
                "SELECT `TABLE_NAME`, `COLUMN_NAME`, `CHARACTER_MAXIMUM_LENGTH` FROM information_schema.`COLUMNS` WHERE `TABLE_SCHEMA` = ? AND `TABLE_NAME` IN ('rooms', 'items')",
                vec![SqlValue::Text(database.clone())],
            )
            .await?;
        for row in columns {
            let (table, column) = (row.first().map(text).unwrap_or_default(), row.get(1).map(text).unwrap_or_default());
            if let Some(length) = row.get(2).and_then(int) {
                target.lengths.insert(format!("{table}.{column}"), length as usize);
            }
            if table == "rooms" {
                target.room_columns.insert(column);
            } else {
                target.item_columns.insert(column);
            }
        }
        if target.room_columns.is_empty() || target.item_columns.is_empty() {
            anyhow::bail!("{database} has no rooms or items table: is it the hotel database?");
        }
        let model = session
            .select(&format!("SELECT `heightmap`, `door_x`, `door_y`, `door_dir` FROM {db}.`room_models` WHERE `name` = ? LIMIT 1"), vec![SqlValue::Text(capture.model_name.clone())])
            .await
            .unwrap_or_default();
        target.model = model.first().map(|row| {
            let n = |i: usize| row.get(i).and_then(int).unwrap_or(0) as i32;
            (row.first().map(text).unwrap_or_default(), Door { x: n(1), y: n(2), dir: n(3) })
        });
        let choice = ImportChoice { owner_id, owner_name: owner_name.clone(), room_name: name.to_string(), door, now: chrono::Utc::now().timestamp() };
        let plan = room::plan(capture, &target, &choice)?;
        if !write {
            return Ok((plan, None));
        }
        let statements: Vec<(String, Vec<Linked>)> = plan.statements.iter().map(|(sql, values)| (sql.clone(), values.iter().map(to_linked).collect())).collect();
        let ids = session.execute_linked(&statements).await?;
        let record = ImportRecord {
            server: server.login().host.clone(),
            database: database.clone(),
            room_id: ids.first().copied().unwrap_or(0),
            room_name: name.to_string(),
            owner_id,
            owner_name,
            item_ids: plan.item_statements.iter().filter_map(|s| ids.get(*s).copied()).collect(),
            custom_model: plan.report.custom_model,
            source: format!("{} room {}", capture.hotel, capture.room_id),
            imported_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        };
        save_record(&record)?;
        Ok((plan, Some(record)))
    }
    .await;
    session.close().await;
    result
}

fn save_record(record: &ImportRecord) -> anyhow::Result<()> {
    let folder = imports_folder();
    std::fs::create_dir_all(&folder)?;
    let file: String = format!("{}_{}.json", record.server, record.room_id).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
    std::fs::write(folder.join(file), serde_json::to_vec_pretty(record)?)?;
    Ok(())
}

/// Sends anyone in the room to the hotel view (Polaris unloads a room when its owner is set again over RCON), then
/// deletes what the import made, in one transaction.
async fn undo_task(server: &dyn Server, record: &ImportRecord) -> anyhow::Result<()> {
    let request = aio_remote::rcon::request("changeroomowner", serde_json::json!({ "room_id": record.room_id, "user_id": record.owner_id, "username": record.owner_name }));
    // Without a running emulator nothing has the room loaded: the rows can go as they are.
    let _ = aio_remote::rcon::send(server, &request).await;
    let (session, _) = open_hotel(server).await?;
    let statements: Vec<(String, Vec<Linked>)> = room::undo_statements(record).iter().map(|(sql, values)| (sql.clone(), values.iter().map(to_linked).collect())).collect();
    let result = session.execute_linked(&statements).await;
    session.close().await;
    result?;
    Ok(())
}
