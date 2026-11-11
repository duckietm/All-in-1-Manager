//! Emulator & wired (EmulatorMonitorView.cs): a live view of the emulator's CPU, memory and wired threads, plus the
//! rooms from the log's slow-wired warnings. Samples every two seconds, only while it is on screen.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use aio_remote::emulator_monitor::{self, EmulatorSample, JvmThread, SlowWiredRoom};
use aio_remote::Server;
use eframe::egui::{self, Color32, RichText};
use parking_lot::Mutex;

use super::line_chart::LineChart;
use super::server_status::{bar, bold, bytes, fixed, legend, live_text, mono, panel, tile, uptime, INTERVAL};
use crate::gui::icons::Icon;
use crate::gui::theme::{self, Fixed, Palette};
use crate::gui::{widgets, Shared, View};
use crate::i18n::{t, tf};

/// The log is read every this many samples.
const LOG_EVERY: u32 = 5;

fn emu_color() -> Color32 {
    Fixed::brand()
}
fn wired_color() -> Color32 {
    theme::hex("#E8590C")
}

/// A row of the wired threads: label, CPU (% of one core), running.
#[derive(Debug, Clone, PartialEq)]
struct WiredRow {
    label: String,
    cpu: f64,
    running: bool,
}

/// A row of "Where the emulator's CPU goes".
#[derive(Debug, Clone, PartialEq)]
struct GroupRow {
    name: String,
    cpu: f64,
    count: usize,
    share: f64,
}

struct Figures {
    updated: String,
    cpu: String,
    cpu_detail: String,
    wired: String,
    wired_detail: String,
    mem: String,
    mem_detail: String,
    threads: String,
    threads_detail: String,
    chart_scale: String,
    /// None: nothing shown (not running yet); Some(empty): "No wired threads".
    wired_rows: Option<Vec<WiredRow>>,
    groups: Vec<GroupRow>,
    log_summary: String,
    rooms: Vec<SlowWiredRoom>,
    recent: Vec<String>,
}

impl Default for Figures {
    fn default() -> Self {
        let big = || "…".to_string();
        Self {
            updated: t("Connecting ...").into(),
            cpu: big(),
            cpu_detail: String::new(),
            wired: big(),
            wired_detail: String::new(),
            mem: big(),
            mem_detail: String::new(),
            threads: big(),
            threads_detail: String::new(),
            chart_scale: String::new(),
            wired_rows: None,
            groups: Vec::new(),
            log_summary: String::new(),
            rooms: Vec::new(),
            recent: Vec::new(),
        }
    }
}

struct State {
    generation: u64,
    figures: Figures,
    chart: LineChart,
    previous: Option<EmulatorSample>,
    /// Full thread names from a JVM thread dump, read again when unknown threads appear.
    names: HashMap<i32, String>,
    names_pid: i32,
    sampling: bool,
    ticks: u32,
    timer: bool,
}

pub struct EmulatorMonitorView {
    state: Arc<Mutex<State>>,
    last_pass: Option<u64>,
    next_tick: f64,
}

impl EmulatorMonitorView {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                generation: 0,
                figures: Figures::default(),
                chart: LineChart::new(&[emu_color(), wired_color()]),
                previous: None,
                names: HashMap::new(),
                names_pid: 0,
                sampling: false,
                ticks: 0,
                timer: false,
            })),
            last_pass: None,
            next_tick: 0.0,
        }
    }

    fn start(&mut self, shared: &Shared, now: f64) {
        {
            let mut s = self.state.lock();
            s.previous = None;
            s.ticks = 0;
            s.timer = true;
        }
        self.next_tick = now + INTERVAL;
        self.tick(shared);
    }

    fn tick(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else {
            let mut s = self.state.lock();
            s.timer = false;
            s.figures.updated = t("Not connected.").into();
            return;
        };
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
    let result = sample_once(state, server, generation).await;
    let mut s = state.lock();
    if s.generation != generation {
        return;
    }
    s.sampling = false;
    match result {
        Ok(()) => s.figures.updated = live_text(),
        Err(e) => {
            s.timer = false;
            s.figures.updated = tf("Stopped: {message}", &[("message", &e.to_string())]);
        }
    }
}

async fn sample_once(state: &Arc<Mutex<State>>, server: &Arc<dyn Server>, generation: u64) -> aio_remote::Result<()> {
    let sample = emulator_monitor::sample(server.as_ref()).await?;
    let need_names = {
        let s = state.lock();
        sample.running() && (sample.pid != s.names_pid || sample.threads.iter().any(|t| !s.names.contains_key(&t.id)))
    };
    if need_names {
        let mut names = emulator_monitor::thread_names(server.as_ref(), sample.pid).await?;
        // A thread dump briefly pauses the JVM. Threads it did not list keep their /proc name so they don't trigger
        // another dump.
        for thread in &sample.threads {
            names.entry(thread.id).or_insert_with(|| thread.name.clone());
        }
        let mut s = state.lock();
        if s.generation != generation {
            return Ok(());
        }
        s.names = names;
        s.names_pid = sample.pid;
    }
    let read_log = {
        let mut s = state.lock();
        if s.generation != generation {
            return Ok(());
        }
        let mut sample = sample;
        for thread in &mut sample.threads {
            if let Some(name) = s.names.get(&thread.id) {
                thread.name = name.clone();
            }
        }
        show_sample(&mut s, sample);
        let read = s.ticks.is_multiple_of(LOG_EVERY);
        s.ticks = s.ticks.wrapping_add(1);
        read
    };
    if read_log {
        // A log problem must not stop the live figures.
        read_log_into(state, server, generation).await;
    }
    Ok(())
}

async fn read_log_into(state: &Arc<Mutex<State>>, server: &Arc<dyn Server>, generation: u64) {
    let result = emulator_monitor::wired_log(server.as_ref()).await;
    let mut s = state.lock();
    if s.generation != generation {
        return;
    }
    let f = &mut s.figures;
    match result {
        Err(e) => f.log_summary = tf("Could not read the log: {message}", &[("message", &e.to_string())]),
        Ok(summary) => {
            f.log_summary = log_summary_text(summary.slow_tasks, summary.rooms.len(), summary.dropped_warnings);
            f.rooms = summary.rooms.into_iter().take(15).collect();
            f.recent = summary.recent;
        }
    }
}

fn log_summary_text(slow_tasks: i32, rooms: usize, dropped: i32) -> String {
    if slow_tasks == 0 && dropped == 0 {
        return t("No slow wired tasks since the emulator started: every room's wired finished in time.").into();
    }
    let values = [("slow", slow_tasks.to_string()), ("rooms", rooms.to_string()), ("dropped", dropped.to_string())];
    let values: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
    if dropped > 0 {
        tf(
            "{slow} slow wired tasks in {rooms} rooms, and {dropped} times a room had so many wired tasks waiting that new ones were dropped.",
            &values,
        )
    } else {
        tf("{slow} slow wired tasks in {rooms} rooms.", &values)
    }
}

/// The order of the wired threads: the coordinator, the shards, then the heavy workers, each by thread id.
fn wired_order(thread: &JvmThread) -> (u8, i32) {
    let rank = if thread.name.starts_with("WiredTickCoordi") {
        0
    } else if thread.name.starts_with("WiredTickShard") {
        1
    } else {
        2
    };
    (rank, thread.id)
}

/// The label of a wired thread. The log numbers the heavy workers after the shards ("Slow wired task: shard=6" with
/// 6 shards).
fn wired_label(name: &str, shard_count: usize) -> String {
    match name.strip_prefix("WiredTickHeavy-").and_then(|n| n.parse::<i32>().ok()) {
        Some(heavy) => tf("Heavy rooms (shard {shard})", &[("shard", &(shard_count as i64 + heavy as i64).to_string())]),
        None => emulator_monitor::wired_name(name),
    }
}

fn show_sample(s: &mut State, sample: EmulatorSample) {
    let f = &mut s.figures;
    if !sample.running() {
        let dash = "–".to_string();
        f.cpu = dash.clone();
        f.wired = dash.clone();
        f.mem = dash.clone();
        f.threads = dash;
        f.cpu_detail = t("The emulator is not running.").into();
        f.wired_detail.clear();
        f.mem_detail.clear();
        f.threads_detail.clear();
        f.wired_rows = None;
        f.groups.clear();
        s.previous = None;
        return;
    }

    f.mem = bytes(sample.rss_bytes);
    f.mem_detail = if sample.max_heap_bytes > 0 {
        tf("in use · heap limit (-Xmx) {size}", &[("size", &bytes(sample.max_heap_bytes))])
    } else {
        t("in use").into()
    };
    f.threads = sample.threads.len().to_string();
    let running = sample.threads.iter().filter(|t| t.state == 'R').count();
    f.threads_detail =
        tf("running {count} · up {uptime}", &[("count", &running.to_string()), ("uptime", &uptime(sample.started_seconds_ago))]);

    // A restart gives a new pid, so the rates start over.
    let Some(previous) = s.previous.as_ref().filter(|p| p.pid == sample.pid) else {
        f.cpu = "…".into();
        f.wired = "…".into();
        f.cpu_detail = tf("pid {pid}", &[("pid", &sample.pid.to_string())]);
        s.previous = Some(sample);
        return;
    };

    let (server, core) = sample.cpu(previous);
    let per_thread = sample.thread_cpu(previous);
    let cpu = |t: &JvmThread| per_thread.get(&t.id).copied().unwrap_or(0.0);

    let mut wired: Vec<&JvmThread> = sample.threads.iter().filter(|t| emulator_monitor::group(&t.name) == "Wired").collect();
    wired.sort_by_key(|t| wired_order(t));
    let wired_cpu: f64 = wired.iter().map(|t| cpu(t)).sum();

    f.cpu = format!("{}%", fixed(core, 0));
    f.cpu_detail = tf(
        "of one core · {server}% of the server ({cores} cores) · pid {pid}",
        &[("server", &fixed(server, 1)), ("cores", &sample.cores.to_string()), ("pid", &sample.pid.to_string())],
    );
    f.wired = format!("{}%", fixed(wired_cpu, 1));
    f.wired_detail = if core > 0.0 {
        tf(
            "of one core · {share}% of the emulator's CPU · {count} threads",
            &[("share", &fixed(wired_cpu * 100.0 / core, 0)), ("count", &wired.len().to_string())],
        )
    } else {
        tf("of one core · {count} threads", &[("count", &wired.len().to_string())])
    };
    s.chart.add(&[core, wired_cpu]);
    f.chart_scale = tf(
        "Top of the graph: {top}% of one core · last {minutes} minutes",
        &[("top", &fixed(s.chart.top(), 0)), ("minutes", &fixed(s.chart.capacity as f64 * INTERVAL / 60.0, 0))],
    );

    let shard_count = wired.iter().filter(|t| t.name.starts_with("WiredTickShard-")).count();
    f.wired_rows =
        Some(wired.iter().map(|t| WiredRow { label: wired_label(&t.name, shard_count), cpu: cpu(t), running: t.state == 'R' }).collect());

    let mut groups: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
    for thread in &sample.threads {
        let entry = groups.entry(emulator_monitor::group(&thread.name)).or_default();
        entry.0 += 1;
        entry.1 += cpu(thread);
    }
    let mut groups: Vec<GroupRow> = groups
        .into_iter()
        .map(|(name, (count, group_cpu))| GroupRow {
            name: name.to_string(),
            cpu: group_cpu,
            count,
            share: if core > 0.0 { group_cpu * 100.0 / core } else { 0.0 },
        })
        .collect();
    groups.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then_with(|| a.name.cmp(&b.name)));
    f.groups = groups;
    s.previous = Some(sample);
}

impl View for EmulatorMonitorView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let now = ui.input(|i| i.time);
        let pass = ui.ctx().cumulative_pass_nr();
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

        let mut read_log = false;
        ui.vertical(|ui| {
            let s = self.state.lock();
            let f = &s.figures;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t("Read from the emulator's threads in /proc and from its log; the emulator itself is not changed."))
                        .size(12.0)
                        .color(palette.subtle),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&f.updated).size(12.0).color(palette.subtle));
                });
            });
            ui.add_space(6.0);
            ui.columns(4, |c| {
                tile(&mut c[0], palette, t("Emulator CPU"), Icon::Server, emu_color(), &f.cpu, &f.cpu_detail);
                tile(&mut c[1], palette, t("Wired CPU"), Icon::Cog, wired_color(), &f.wired, &f.wired_detail);
                tile(&mut c[2], palette, t("Memory (JVM)"), Icon::Database, theme::hex("#8B5CF6"), &f.mem, &f.mem_detail);
                tile(&mut c[3], palette, t("Threads"), Icon::History, theme::hex("#0E9F9E"), &f.threads, &f.threads_detail);
            });
            ui.add_space(14.0);
            panel(
                ui,
                palette,
                t("CPU (% of one core)"),
                |ui| legend(ui, palette, &[(t("Emulator"), emu_color()), (t("Wired"), wired_color())]),
                |ui| {
                    s.chart.show(ui, 160.0, palette.card_border);
                    ui.label(RichText::new(&f.chart_scale).size(11.5).color(palette.subtle));
                },
            );
            ui.add_space(14.0);
            ui.columns(2, |c| {
                panel(&mut c[0], palette, t("Wired threads"), |_| {}, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(t("The wired engine: a coordinator every 50 ms, shards that run the rooms' wired, and a worker for heavy rooms."))
                                .size(11.5)
                                .color(palette.subtle),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    wired_threads(ui, palette, f.wired_rows.as_deref());
                });
                panel(&mut c[1], palette, t("Where the emulator's CPU goes"), |_| {}, |ui| groups(ui, palette, &f.groups));
            });
            ui.add_space(14.0);
            panel(
                ui,
                palette,
                t("Rooms with slow wired (emulator log, since the last start)"),
                |ui| read_log = ui.add(widgets::secondary_button(t("Read the log now"))).clicked(),
                |ui| rooms(ui, palette, f),
            );
        });

        if read_log {
            if let Some(server) = shared.session() {
                let state = self.state.clone();
                let generation = state.lock().generation;
                shared.spawn(async move { read_log_into(&state, &server, generation).await });
            }
        }
    }

    fn session_changed(&mut self, _shared: &Shared) {
        let mut s = self.state.lock();
        s.generation += 1;
        s.figures = Figures::default();
        s.chart.clear();
        s.previous = None;
        s.names.clear();
        s.names_pid = 0;
        s.sampling = false;
        s.timer = false;
        drop(s);
        self.last_pass = None;
    }
}

/// A grid row's bar column: the bar fills the space the other columns leave.
const BAR_WIDTH: f32 = 140.0;

fn wired_threads(ui: &mut egui::Ui, palette: &Palette, rows: Option<&[WiredRow]>) {
    let Some(rows) = rows else { return };
    if rows.is_empty() {
        ui.label(RichText::new(t("No wired threads: this emulator version has no WiredTickService.")).size(12.5).color(palette.muted));
        return;
    }
    egui::Grid::new("emulator_wired_threads").spacing(egui::vec2(14.0, 7.0)).show(ui, |ui| {
        for row in rows {
            ui.label(bold(&row.label, 12.5));
            ui.centered_and_justified(|ui| bar(ui, palette, row.cpu.min(100.0), None, BAR_WIDTH));
            ui.label(RichText::new(format!("{}%", fixed(row.cpu, 1))).size(12.5));
            ui.label(RichText::new(if row.running { t("running") } else { t("waiting") }).size(12.0).color(palette.muted));
            ui.end_row();
        }
    });
}

fn groups(ui: &mut egui::Ui, palette: &Palette, groups: &[GroupRow]) {
    egui::Grid::new("emulator_cpu_groups").spacing(egui::vec2(14.0, 7.0)).show(ui, |ui| {
        for group in groups {
            ui.label(bold(&group.name, 12.5));
            // A share of the emulator's CPU rather than a warning level, so a single colour.
            ui.centered_and_justified(|ui| bar(ui, palette, group.share.min(100.0), Some(emu_color()), BAR_WIDTH));
            ui.label(RichText::new(format!("{}%", fixed(group.cpu, 1))).size(12.5));
            ui.label(RichText::new(tf("{count} threads", &[("count", &group.count.to_string())])).size(12.0).color(palette.muted));
            ui.end_row();
        }
    });
}

fn rooms(ui: &mut egui::Ui, palette: &Palette, f: &Figures) {
    ui.add(egui::Label::new(RichText::new(&f.log_summary).size(12.5).color(palette.muted)).wrap());
    if !f.rooms.is_empty() {
        ui.add_space(4.0);
        egui::Grid::new("emulator_slow_rooms").spacing(egui::vec2(16.0, 6.0)).show(ui, |ui| {
            for heading in [t("Room"), t("Name"), t("Slow tasks"), t("Slowest"), t("Total"), t("Shard / last seen")] {
                ui.label(bold(heading, 11.5).color(palette.muted));
            }
            ui.end_row();
            for room in &f.rooms {
                ui.add(egui::Label::new(mono(&room.room_id.to_string(), palette)).selectable(true));
                ui.label(RichText::new(room.name.as_deref().unwrap_or("?")).size(12.5));
                ui.label(RichText::new(room.count.to_string()).size(12.5));
                let slowest = if room.slowest_ms >= 500 { palette.error_text } else { palette.text };
                ui.label(RichText::new(format!("{} ms", room.slowest_ms)).size(12.5).color(slowest));
                ui.label(RichText::new(format!("{} ms", room.total_ms)).size(12.5));
                ui.label(RichText::new(format!("{} · {}", room.shards, room.last_seen)).size(12.0).color(palette.muted));
                ui.end_row();
            }
        });
    }
    if !f.recent.is_empty() {
        ui.add_space(6.0);
        ui.label(bold(t("Latest warnings"), 11.5).color(palette.muted));
        for line in &f.recent {
            ui.add(egui::Label::new(mono(line, palette)).selectable(true).wrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: i32, name: &str) -> JvmThread {
        JvmThread { id, name: name.into(), state: 'S', ticks: 0 }
    }

    #[test]
    fn wired_order_and_labels() {
        let mut threads = [thread(9, "WiredTickHeavy-0"), thread(5, "WiredTickShard-1"), thread(7, "WiredTickCoordinator"), thread(4, "WiredTickShard-0")];
        threads.sort_by_key(wired_order);
        let ids: Vec<i32> = threads.iter().map(|t| t.id).collect();
        assert_eq!(ids, [7, 4, 5, 9]);
        assert_eq!(wired_label("WiredTickHeavy-1", 6), "Heavy rooms (shard 7)");
        assert_eq!(wired_label("WiredTickShard-2", 6), emulator_monitor::wired_name("WiredTickShard-2"));
    }

    #[test]
    fn log_summaries() {
        assert_eq!(log_summary_text(0, 0, 0), "No slow wired tasks since the emulator started: every room's wired finished in time.");
        assert_eq!(log_summary_text(4, 2, 0), "4 slow wired tasks in 2 rooms.");
        assert_eq!(
            log_summary_text(4, 2, 3),
            "4 slow wired tasks in 2 rooms, and 3 times a room had so many wired tasks waiting that new ones were dropped."
        );
    }

    #[test]
    fn rates_start_over_after_a_restart() {
        let mut state = State {
            generation: 0,
            figures: Figures::default(),
            chart: LineChart::new(&[Color32::RED, Color32::BLUE]),
            previous: None,
            names: HashMap::new(),
            names_pid: 0,
            sampling: false,
            ticks: 0,
            timer: true,
        };
        let at = chrono::Utc::now();
        let sample = |pid: i32, secs: i64, ticks: i64| EmulatorSample {
            at: at + chrono::Duration::seconds(secs),
            pid,
            ticks_per_second: 100,
            cores: 4,
            machine_ticks: 1000 + secs * 400,
            process_ticks: ticks,
            rss_bytes: 1 << 30,
            max_heap_bytes: 0,
            started_seconds_ago: 60,
            threads: vec![JvmThread { id: 1, name: "WiredTickCoordi".into(), state: 'R', ticks }],
        };
        show_sample(&mut state, sample(10, 0, 0));
        assert_eq!(state.figures.cpu, "…");
        show_sample(&mut state, sample(10, 2, 100));
        assert_eq!(state.figures.cpu, "50%");
        assert_eq!(state.figures.wired, "50.0%");
        assert_eq!(state.figures.wired_rows.as_ref().map(Vec::len), Some(1));
        show_sample(&mut state, sample(11, 4, 300));
        assert_eq!(state.figures.cpu, "…");
        assert_eq!(state.figures.cpu_detail, "pid 11");
    }
}
