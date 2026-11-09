//! Server status (ServerStatusView.cs): a live dashboard of the server that samples every two seconds while it is on
//! screen: CPU, memory, network, uptime with the emulator and its players, disks, logged in users, and the versions
//! of the hotel's clones with what a "git pull" would bring in. The small building blocks (tiles, panels, bars and
//! the size formats) are shared with the emulator monitor, as in the C#.

use std::collections::HashMap;
use std::sync::Arc;

use aio_remote::server_status::{self, DiskUsage, LoggedInUser, RepositoryVersion, StatusSample};
use aio_remote::Server;
use eframe::egui::{self, Color32, RichText};
use parking_lot::Mutex;

use super::line_chart::LineChart;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{self, Fixed, Palette};
use crate::gui::{widgets, Shared, View};
use crate::i18n::{t, tf};

/// Seconds between two samples.
pub(crate) const INTERVAL: f64 = 2.0;

fn cpu_color() -> Color32 {
    Fixed::brand()
}
fn mem_color() -> Color32 {
    theme::hex("#8B5CF6")
}
fn in_color() -> Color32 {
    theme::hex("#0E9F9E")
}
fn out_color() -> Color32 {
    theme::hex("#E8590C")
}

/// The texts of the tiles and the lists, as the last sample left them.
struct Figures {
    updated: String,
    cpu: String,
    cpu_detail: String,
    mem: String,
    mem_detail: String,
    net_in: String,
    net_out: String,
    uptime: String,
    uptime_detail: String,
    net_scale: String,
    disks: Option<Vec<DiskUsage>>,
    users: Option<Vec<LoggedInUser>>,
}

impl Default for Figures {
    fn default() -> Self {
        let big = "…".to_string();
        Self {
            updated: t("Connecting ...").into(),
            cpu: big.clone(),
            cpu_detail: String::new(),
            mem: big.clone(),
            mem_detail: String::new(),
            net_in: big.clone(),
            net_out: String::new(),
            uptime: big,
            uptime_detail: String::new(),
            net_scale: String::new(),
            disks: None,
            users: None,
        }
    }
}

enum Versions {
    Waiting,
    Failed(String),
    Rows(Vec<RepositoryVersion>),
}

/// The colour of a text, by its Pro.* token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Subtle,
    Warning,
    Success,
    Error,
}

impl Tone {
    pub(crate) fn color(self, palette: &Palette) -> Color32 {
        match self {
            Tone::Subtle => palette.subtle,
            Tone::Warning => palette.warning_text,
            Tone::Success => palette.success_text,
            Tone::Error => palette.error_text,
        }
    }
}

/// The "Updates" cell of a repository row.
#[derive(Clone, Debug, PartialEq)]
struct UpdateCell {
    text: String,
    tone: Tone,
    tip: Option<String>,
}

struct State {
    /// Bumped by a new login: results of the old one are dropped.
    generation: u64,
    figures: Figures,
    usage_chart: LineChart,
    net_chart: LineChart,
    previous: Option<StatusSample>,
    sampling: bool,
    timer: bool,
    versions: Versions,
    /// By repository folder, in the order of the rows.
    cells: HashMap<String, UpdateCell>,
    checking_updates: bool,
}

pub struct ServerStatusView {
    state: Arc<Mutex<State>>,
    last_pass: Option<u64>,
    next_tick: f64,
}

impl ServerStatusView {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                generation: 0,
                figures: Figures::default(),
                usage_chart: LineChart::new(&[cpu_color(), mem_color()]).with_maximum(100.0),
                net_chart: LineChart::new(&[in_color(), out_color()]),
                previous: None,
                sampling: false,
                timer: false,
                versions: Versions::Waiting,
                cells: HashMap::new(),
                checking_updates: false,
            })),
            last_pass: None,
            next_tick: 0.0,
        }
    }

    /// Restarts sampling, for example after a new login. (StartAsync)
    fn start(&mut self, shared: &Shared, now: f64) {
        let generation = {
            let mut s = self.state.lock();
            s.previous = None;
            s.timer = true;
            s.generation
        };
        self.next_tick = now + INTERVAL;
        let Some(server) = self.connection(shared) else { return };
        let state = self.state.clone();
        shared.spawn(async move {
            sample(&state, &server, generation).await;
            load_versions(&state, &server, generation).await;
            load_updates(&state, &server, generation).await;
        });
    }

    /// The login, or None after stopping the timer with "Not connected.".
    fn connection(&self, shared: &Shared) -> Option<Arc<dyn Server>> {
        let server = shared.session();
        if server.is_none() {
            let mut s = self.state.lock();
            s.timer = false;
            s.figures.updated = t("Not connected.").into();
        }
        server
    }

    fn tick(&mut self, shared: &Shared) {
        let Some(server) = self.connection(shared) else { return };
        let state = self.state.clone();
        let generation = state.lock().generation;
        shared.spawn(async move { sample(&state, &server, generation).await });
    }
}

async fn sample(state: &Arc<Mutex<State>>, server: &Arc<dyn Server>, generation: u64) {
    {
        let mut s = state.lock();
        if s.sampling || s.generation != generation {
            return;
        }
        s.sampling = true;
    }
    let result = server_status::sample(server.as_ref()).await;
    let mut s = state.lock();
    if s.generation != generation {
        return;
    }
    s.sampling = false;
    match result {
        Ok(sample) => {
            show_sample(&mut s, sample);
            s.figures.updated = live_text();
        }
        Err(e) => {
            s.timer = false;
            s.figures.updated = tf("Stopped: {message}", &[("message", &e.to_string())]);
        }
    }
}

/// "Live, every 2 seconds · 12:34:56".
pub(crate) fn live_text() -> String {
    tf(
        "Live, every {seconds} seconds · {time}",
        &[("seconds", &fixed(INTERVAL, 0)), ("time", &chrono::Local::now().format("%H:%M:%S").to_string())],
    )
}

fn show_sample(s: &mut State, sample: StatusSample) {
    let mem_used = sample.mem_used();
    let mem_percent = if sample.mem_total > 0 { mem_used as f64 * 100.0 / sample.mem_total as f64 } else { 0.0 };
    let f = &mut s.figures;
    f.mem = format!("{}%", fixed(mem_percent, 0));
    f.mem_detail = tf("{used} of {total}", &[("used", &bytes(mem_used)), ("total", &bytes(sample.mem_total))]);
    if sample.swap_total > 0 {
        f.mem_detail += &tf(" · swap {size}", &[("size", &bytes(sample.swap_total - sample.swap_free))]);
    }
    f.uptime = uptime(sample.uptime_seconds);
    f.uptime_detail = if sample.emulator_running { t("Emulator running") } else { t("Emulator not running") }.to_string();
    if let Some(players) = sample.players_online {
        f.uptime_detail += &tf(" · {players} online", &[("players", &players.to_string())]);
    }

    if let Some(previous) = &s.previous {
        let cpu = sample.cpu_percent(previous);
        let (in_rate, out_rate) = sample.bytes_per_second(previous);
        f.cpu = format!("{}%", fixed(cpu, 0));
        f.net_in = format!("↓ {}", rate(in_rate));
        f.net_out = format!("↑ {}", rate(out_rate));
        s.usage_chart.add(&[cpu, mem_percent]);
        s.net_chart.add(&[in_rate, out_rate]);
        f.net_scale = tf(
            "Top of the graph: {top} · last {minutes} minutes",
            &[("top", &rate(s.net_chart.top())), ("minutes", &fixed(s.net_chart.capacity as f64 * INTERVAL / 60.0, 0))],
        );
    } else {
        f.cpu = "…".into();
        f.net_in = "↓ …".into();
        f.net_out = "↑ …".into();
    }
    f.cpu_detail = tf("{cores} cores · load {load}", &[("cores", &sample.cores.to_string()), ("load", &sample.load)]);
    f.disks = Some(sample.disks.clone());
    f.users = Some(sample.users.clone());
    s.previous = Some(sample);
}

async fn load_versions(state: &Arc<Mutex<State>>, server: &Arc<dyn Server>, generation: u64) {
    let result = server_status::versions(server.as_ref()).await;
    let mut s = state.lock();
    if s.generation != generation {
        return;
    }
    s.cells.clear();
    match result {
        Err(e) => s.versions = Versions::Failed(tf("Could not read the versions: {message}", &[("message", &e.to_string())])),
        Ok(versions) => {
            for v in &versions {
                s.cells.insert(v.directory.clone(), UpdateCell { text: "…".into(), tone: Tone::Subtle, tip: None });
            }
            s.versions = Versions::Rows(versions);
        }
    }
}

/// Fetches the clones and shows per repository how many commits a "git pull" would bring in. Only the remote
/// branches are refreshed, so the hotel is not touched.
async fn load_updates(state: &Arc<Mutex<State>>, server: &Arc<dyn Server>, generation: u64) {
    {
        let mut s = state.lock();
        if s.checking_updates || s.cells.is_empty() || s.generation != generation {
            return;
        }
        s.checking_updates = true;
        for cell in s.cells.values_mut() {
            *cell = UpdateCell { text: t("checking …").into(), tone: Tone::Subtle, tip: None };
        }
    }
    let result = server_status::updates(server.as_ref()).await;
    let mut s = state.lock();
    if s.generation != generation {
        return;
    }
    s.checking_updates = false;
    match result {
        Ok(updates) => {
            for update in &updates {
                if let Some(cell) = s.cells.get_mut(&update.directory) {
                    *cell = update_cell(update);
                }
            }
            // A clone the check did not reach, for example one without a remote branch.
            for cell in s.cells.values_mut().filter(|c| c.text == t("checking …")) {
                cell.text = "?".into();
            }
        }
        Err(e) => {
            let tip = tf("Could not check for updates: {message}", &[("message", &e.to_string())]);
            for cell in s.cells.values_mut() {
                *cell = UpdateCell { text: "?".into(), tone: Tone::Error, tip: Some(tip.clone()) };
            }
        }
    }
}

fn update_cell(update: &server_status::RepositoryUpdate) -> UpdateCell {
    let waiting = update.has_update() || update.switches_branch();
    let target = update.target.clone().unwrap_or_default();
    let behind = update.behind.to_string();
    let text = if update.switches_branch() {
        if update.has_update() {
            tf("→ {target}, {behind} new", &[("target", &target), ("behind", &behind)])
        } else {
            format!("→ {target}")
        }
    } else if update.has_update() {
        tf("{behind} new", &[("behind", &behind)])
    } else {
        t("up to date").to_string()
    };
    let tip = update.newest.as_ref().map(|newest| {
        let mut tip = String::new();
        if update.switches_branch() {
            tip += &tf(
                "The chosen branch is {target}; this clone is on {branch}.",
                &[("target", &target), ("branch", update.branch.as_deref().unwrap_or(""))],
            );
            tip.push('\n');
        }
        tip += &tf("Newest on {branch}: {newest}", &[("branch", update.counted().unwrap_or("")), ("newest", newest)]);
        tip
    });
    UpdateCell { text, tone: if waiting { Tone::Warning } else { Tone::Success }, tip }
}

impl View for ServerStatusView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let now = ui.input(|i| i.time);
        let pass = ui.ctx().cumulative_pass_nr();
        // Back on screen (AttachedToVisualTree): start again. Off screen nothing ticks.
        if self.last_pass.is_none_or(|last| pass > last + 1) {
            self.start(shared, now);
        }
        self.last_pass = Some(pass);
        if self.state.lock().timer {
            if now >= self.next_tick {
                self.next_tick = now + INTERVAL;
                self.tick(shared);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((self.next_tick - now).max(0.05)));
        }

        let mut refresh = false;
        let mut check = false;
        ui.vertical(|ui| {
            let s = self.state.lock();
            let f = &s.figures;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&f.updated).size(12.0).color(palette.subtle));
                });
            });
            ui.add_space(6.0);
            ui.columns(4, |c| {
                tile(&mut c[0], palette, "CPU", Icon::Cog, cpu_color(), &f.cpu, &f.cpu_detail);
                tile(&mut c[1], palette, t("Memory"), Icon::Database, mem_color(), &f.mem, &f.mem_detail);
                tile(&mut c[2], palette, t("Network in / out"), Icon::Swap, in_color(), &f.net_in, &f.net_out);
                tile(&mut c[3], palette, t("Uptime"), Icon::History, out_color(), &f.uptime, &f.uptime_detail);
            });
            ui.add_space(14.0);
            ui.columns(2, |c| {
                panel(&mut c[0], palette, t("CPU and memory (%)"), |ui| legend(ui, palette, &[("CPU", cpu_color()), (t("Memory"), mem_color())]), |ui| {
                    s.usage_chart.show(ui, 150.0, palette.card_border);
                });
                panel(&mut c[1], palette, t("Network"), |ui| legend(ui, palette, &[(t("In"), in_color()), (t("Out"), out_color())]), |ui| {
                    s.net_chart.show(ui, 150.0, palette.card_border);
                    ui.label(RichText::new(&f.net_scale).size(11.5).color(palette.subtle));
                });
            });
            ui.add_space(14.0);
            ui.columns(2, |c| {
                panel(&mut c[0], palette, t("Disks"), |_| {}, |ui| disks(ui, palette, f.disks.as_deref()));
                panel(&mut c[1], palette, t("Logged in users"), |_| {}, |ui| users(ui, palette, f.users.as_deref()));
            });
            ui.add_space(14.0);
            panel(
                ui,
                palette,
                t("Versions"),
                |ui| {
                    refresh = ui.add(widgets::secondary_button(t("Refresh"))).clicked();
                    check = ui.add(widgets::secondary_button(t("Check for updates"))).clicked();
                },
                |ui| versions(ui, palette, &s),
            );
        });

        if refresh || check {
            if let Some(server) = shared.session() {
                let state = self.state.clone();
                let generation = state.lock().generation;
                shared.spawn(async move {
                    if refresh {
                        load_versions(&state, &server, generation).await;
                    }
                    load_updates(&state, &server, generation).await;
                });
            }
        }
    }

    fn session_changed(&mut self, _shared: &Shared) {
        let mut s = self.state.lock();
        s.generation += 1;
        s.previous = None;
        s.sampling = false;
        s.checking_updates = false;
        s.timer = false;
        // The figures and graphs belonged to the old login.
        s.figures = Figures::default();
        s.usage_chart.clear();
        s.net_chart.clear();
        s.versions = Versions::Waiting;
        s.cells.clear();
        drop(s);
        // Starts again on the next frame it is shown.
        self.last_pass = None;
    }
}

fn disks(ui: &mut egui::Ui, palette: &Palette, disks: Option<&[DiskUsage]>) {
    let Some(disks) = disks else { return };
    for disk in disks {
        ui.horizontal(|ui| {
            ui.label(bold(&format!("{}  ({})", disk.mount, disk.device), 12.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = tf(
                    "{used} of {total} · {free} free",
                    &[("used", &bytes(disk.used)), ("total", &bytes(disk.total)), ("free", &bytes(disk.available))],
                );
                ui.label(RichText::new(text).size(12.0).color(palette.muted));
            });
        });
        bar(ui, palette, disk.percent(), None, ui.available_width());
        ui.add_space(6.0);
    }
    if disks.is_empty() {
        ui.label(RichText::new(t("No disks found.")).size(12.5).color(palette.muted));
    }
}

fn users(ui: &mut egui::Ui, palette: &Palette, users: Option<&[LoggedInUser]>) {
    let Some(users) = users else { return };
    for user in users {
        let mut text = tf(
            "{user}   {terminal}   since {since}",
            &[("user", &user.user), ("terminal", &user.terminal), ("since", &user.since)],
        );
        if !user.from.is_empty() {
            text += &tf("   from {from}", &[("from", &user.from)]);
        }
        ui.label(RichText::new(text).size(12.5));
    }
    if users.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new(t("Nobody is logged in on a terminal (this installer uses commands, not a terminal)."))
                    .size(12.5)
                    .color(palette.muted),
            )
            .wrap(),
        );
    }
}

fn versions(ui: &mut egui::Ui, palette: &Palette, s: &State) {
    match &s.versions {
        Versions::Waiting => {}
        Versions::Failed(message) => {
            ui.label(RichText::new(message).size(12.5).color(palette.error_text));
        }
        Versions::Rows(rows) if rows.is_empty() => {
            ui.label(RichText::new(t("No hotel repositories found on this server.")).size(12.5).color(palette.muted));
        }
        Versions::Rows(rows) => {
            egui::Grid::new("server_status_versions").spacing(egui::vec2(18.0, 6.0)).show(ui, |ui| {
                for heading in ["", t("Commit"), t("Version"), t("Updates"), t("Date"), t("Last change")] {
                    ui.label(bold(heading, 11.5).color(palette.muted));
                }
                ui.end_row();
                for v in rows {
                    ui.label(bold(&v.name, 12.5));
                    let mut commit = v.commit.clone().unwrap_or_else(|| "?".into());
                    if let Some(branch) = &v.branch {
                        commit += &format!(" ({branch})");
                    }
                    ui.add(egui::Label::new(mono(&commit, palette)).selectable(true));
                    ui.label(RichText::new(v.build.as_deref().unwrap_or("")).size(12.5));
                    match s.cells.get(&v.directory) {
                        Some(cell) => {
                            let response = ui.label(RichText::new(&cell.text).size(12.5).color(cell.tone.color(palette)));
                            if let Some(tip) = &cell.tip {
                                response.on_hover_text(tip);
                            }
                        }
                        None => {
                            ui.label("");
                        }
                    }
                    ui.label(RichText::new(v.date.as_deref().unwrap_or("")).size(12.5).color(palette.muted));
                    ui.add(egui::Label::new(RichText::new(v.subject.as_deref().unwrap_or("")).size(12.5).color(palette.muted)).truncate());
                    ui.end_row();
                }
            });
        }
    }
}

// ---- Building blocks (also used by the emulator monitor) ----------------------------------------------------------

/// SemiBold text.
pub(crate) fn bold(text: &str, size: f32) -> RichText {
    RichText::new(text).size(size).family(egui::FontFamily::Name("bold".into()))
}

pub(crate) fn mono(text: &str, palette: &Palette) -> RichText {
    RichText::new(text).monospace().size(12.5).color(palette.text)
}

/// The frame of a tile or panel (the C# Box), in the window's card look.
fn boxed<R>(ui: &mut egui::Ui, palette: &Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // The page is already a card: the boxes have only their border, as in the C#.
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0_f32, palette.card_border))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), add).inner
        })
        .inner
}

/// A figure with its icon, title, big value and detail line.
pub(crate) fn tile(ui: &mut egui::Ui, palette: &Palette, title: &str, icon: Icon, color: Color32, value: &str, detail: &str) {
    boxed(ui, palette, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.horizontal(|ui| {
            icons::show(ui, icon, 15.0, color);
            ui.label(bold(title, 12.0).color(palette.muted));
        });
        arrow_label(ui, value, 24.0, palette.text, true);
        if detail.starts_with(['↑', '↓']) {
            arrow_label(ui, detail, 12.0, palette.muted, false);
        } else {
            ui.add(egui::Label::new(RichText::new(detail).size(12.0).color(palette.muted)).wrap());
        }
    });
}

/// A text that may start with ↑ or ↓: the Hotel look's Ubuntu font has no arrows, so they come from the monospace
/// font.
fn arrow_label(ui: &mut egui::Ui, text: &str, size: f32, color: Color32, strong: bool) {
    let body = |text: &str| if strong { bold(text, size).color(color) } else { RichText::new(text).size(size).color(color) };
    match text.strip_prefix('↑').map(|rest| ('↑', rest)).or_else(|| text.strip_prefix('↓').map(|rest| ('↓', rest))) {
        Some((arrow, rest)) => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new(arrow.to_string()).monospace().size(size).color(color));
                ui.label(body(rest));
            });
        }
        None => {
            ui.label(body(text));
        }
    }
}

/// A titled box with an optional control on the right of the title (laid out right to left).
pub(crate) fn panel(
    ui: &mut egui::Ui,
    palette: &Palette,
    title: &str,
    side: impl FnOnce(&mut egui::Ui),
    content: impl FnOnce(&mut egui::Ui),
) {
    boxed(ui, palette, |ui| {
        ui.horizontal(|ui| {
            ui.label(bold(title, 13.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), side);
        });
        ui.add_space(4.0);
        content(ui);
    });
}

/// Colour squares with names, for a graph (added right to left, so the order is reversed here).
pub(crate) fn legend(ui: &mut egui::Ui, palette: &Palette, items: &[(&str, Color32)]) {
    for (name, color) in items.iter().rev() {
        ui.label(RichText::new(*name).size(11.5).color(palette.muted));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, *color);
        ui.add_space(6.0);
    }
}

/// The colour of a usage bar: green, orange or red by how full it is.
pub(crate) fn bar_color(percent: f64) -> Color32 {
    if percent >= 90.0 {
        theme::hex("#DC2626")
    } else if percent >= 75.0 {
        theme::hex("#D97706")
    } else {
        theme::hex("#16A34A")
    }
}

/// A usage bar coloured by how full it is, unless a fixed colour is given.
pub(crate) fn bar(ui: &mut egui::Ui, palette: &Palette, percent: f64, fixed_color: Option<Color32>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width.max(20.0), 8.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, palette.hover);
    let share = (percent.max(0.5) / (percent.max(0.5) + (100.0 - percent).max(0.5))) as f32;
    let filled = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * share, rect.height()));
    let corners = egui::CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 };
    ui.painter().rect_filled(filled, corners, fixed_color.unwrap_or_else(|| bar_color(percent)));
}

/// C#'s fixed-point format ("0", "0.0", "0.00"): rounds half away from zero.
pub(crate) fn fixed(value: f64, decimals: usize) -> String {
    let scale = 10f64.powi(decimals as i32);
    let rounded = (value * scale).round() / scale;
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    format!("{rounded:.decimals$}")
}

pub fn rate(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1024.0 * 1024.0 {
        format!("{} MB/s", fixed(bytes_per_second / (1024.0 * 1024.0), 2))
    } else if bytes_per_second >= 1024.0 {
        format!("{} KB/s", fixed(bytes_per_second / 1024.0, 1))
    } else {
        format!("{} B/s", fixed(bytes_per_second, 0))
    }
}

pub(crate) fn bytes(bytes: i64) -> String {
    let b = bytes as f64;
    if bytes >= 1 << 40 {
        format!("{} TB", fixed(b / (1u64 << 40) as f64, 1))
    } else if bytes >= 1 << 30 {
        format!("{} GB", fixed(b / (1u64 << 30) as f64, 1))
    } else if bytes >= 1 << 20 {
        format!("{} MB", fixed(b / (1u64 << 20) as f64, 0))
    } else {
        format!("{} KB", fixed(b / 1024.0, 0))
    }
}

pub(crate) fn uptime(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3600 % 24, seconds / 60 % 60);
    if days >= 1 {
        format!("{days}d {hours}h {minutes}m")
    } else if seconds >= 3600 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(1023.4), "1023 B/s");
        assert_eq!(rate(1536.0), "1.5 KB/s");
        assert_eq!(rate(3.0 * 1024.0 * 1024.0), "3.00 MB/s");
        assert_eq!(bytes(512), "1 KB");
        assert_eq!(bytes(5 << 20), "5 MB");
        assert_eq!(bytes(3 << 29), "1.5 GB");
        assert_eq!(bytes(2 << 40), "2.0 TB");
        assert_eq!(uptime(59), "0m");
        assert_eq!(uptime(3600 + 120), "1h 2m");
        assert_eq!(uptime(2 * 86_400 + 3 * 3600 + 4 * 60), "2d 3h 4m");
        assert_eq!(fixed(2.5, 0), "3");
        assert_eq!(fixed(-0.2, 0), "0");
        assert_eq!(fixed(0.05, 1), "0.1");
    }

    #[test]
    fn update_cells() {
        let update = |branch: Option<&str>, target: Option<&str>, behind: i32| server_status::RepositoryUpdate {
            name: "Emulator".into(),
            directory: "/x".into(),
            branch: branch.map(Into::into),
            target: target.map(Into::into),
            behind,
            newest: Some("abc fix".into()),
        };
        let cell = update_cell(&update(Some("main"), None, 0));
        assert_eq!((cell.text.as_str(), cell.tone), ("up to date", Tone::Success));
        assert_eq!(cell.tip.as_deref(), Some("Newest on main: abc fix"));
        let cell = update_cell(&update(Some("main"), Some("main"), 3));
        assert_eq!((cell.text.as_str(), cell.tone), ("3 new", Tone::Warning));
        let cell = update_cell(&update(Some("main"), Some("dev"), 2));
        assert_eq!(cell.text, "→ dev, 2 new");
        assert_eq!(cell.tip.as_deref(), Some("The chosen branch is dev; this clone is on main.\nNewest on dev: abc fix"));
        assert_eq!(update_cell(&update(Some("main"), Some("dev"), 0)).text, "→ dev");
    }

    #[test]
    fn bar_colors() {
        assert_eq!(bar_color(10.0), theme::hex("#16A34A"));
        assert_eq!(bar_color(75.0), theme::hex("#D97706"));
        assert_eq!(bar_color(95.0), theme::hex("#DC2626"));
    }
}
