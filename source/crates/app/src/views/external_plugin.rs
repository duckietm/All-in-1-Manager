//! The page of an external plugin: the plugin describes it as a `ui-tree`, this draws it with the window's own widgets
//! and hands what the user did back as `ui-event`s.
//!
//! Lifecycle: one [`PluginInstance`] per open page. It lives in a task on the tokio runtime (never on the window's
//! thread) that loads it, renders, and then handles the page's events one by one, rendering after them. Leaving the
//! page drops the view, which stops the task: the instance is dropped and its waiting approvals are rejected. A
//! plugin that traps or runs too long shows "The plugin stopped: …" with Reload.
//!
//! The tree is untrusted: it is checked ([`validate`]) before anything is drawn, and every string in it goes through
//! `display_safe`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use aio_plugin_host::approvals::Approvals;
use aio_plugin_host::runtime::{BannerKind, ChangedData, PluginError, PluginInstance, UiElement, UiEvent, UiTree};
use aio_plugin_host::services::HostServices;
use aio_plugin_host::{display_safe, Engine};
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::plugin_approval::{self, Outcome};
use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::Shared;
use crate::i18n::{t, tf};
use crate::plugins::external::{self, ExternalInfo, LiveGuard};
use crate::plugins::services::{AppServices, PluginOutput};

/// Most elements a page may have.
pub const MAX_ELEMENTS: usize = 2_000;
/// Deepest nesting of containers (the root is depth 0).
pub const MAX_DEPTH: usize = 32;
/// Events waiting for the plugin before further clicks are dropped.
const MAX_QUEUE: usize = 32;
/// Rows and columns of a table that are drawn.
const MAX_ROWS: usize = 1_000;
const MAX_COLUMNS: usize = 50;
/// Options of a select, lines of a text or log element that are drawn.
const MAX_OPTIONS: usize = 500;
const MAX_LINES: usize = 1_000;

/// The engine every plugin runs on (one per program).
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(aio_plugin_host::runtime::engine)
}

/// The approval queue every plugin shares; it repaints the window when it changes.
pub fn approvals(ctx: &egui::Context) -> Arc<Approvals> {
    static APPROVALS: OnceLock<Arc<Approvals>> = OnceLock::new();
    APPROVALS
        .get_or_init(|| {
            let ctx = ctx.clone();
            Arc::new(Approvals::with_notifier(move || ctx.request_repaint()))
        })
        .clone()
}

/// The children of a container.
fn children(e: &UiElement) -> &[u32] {
    match e {
        UiElement::Card(c) => &c.children,
        UiElement::Row(c) | UiElement::Column(c) => c,
        _ => &[],
    }
}

/// Checks a tree before it is drawn: the root and every child index exist, no element is reached twice (so no
/// cycle), nesting at most [`MAX_DEPTH`], at most [`MAX_ELEMENTS`] elements.
pub fn validate(tree: &UiTree) -> Result<(), String> {
    let count = tree.elements.len();
    if count > MAX_ELEMENTS {
        return Err(format!("the page has {count} elements; at most {MAX_ELEMENTS} are allowed"));
    }
    let root = tree.root as usize;
    if root >= count {
        return Err(format!("the top element {root} does not exist (the page has {count} elements)"));
    }
    let mut seen = vec![false; count];
    let mut open = vec![false; count];
    visit(tree, root, 0, &mut seen, &mut open)
}

/// Depth-first check of `index` at `depth`; `open` marks the elements on the path from the root (a cycle), `seen`
/// every element reached so far (one used twice). The recursion is at most [`MAX_DEPTH`] + 1 deep.
fn visit(tree: &UiTree, index: usize, depth: usize, seen: &mut [bool], open: &mut [bool]) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("the page is nested too deep (more than {MAX_DEPTH} levels)"));
    }
    seen[index] = true;
    open[index] = true;
    for &child in children(&tree.elements[index]) {
        let child = child as usize;
        if child >= tree.elements.len() {
            return Err(format!("element {index} refers to element {child}, which does not exist"));
        }
        if open[child] {
            return Err(format!("element {index} contains element {child}, which contains it: a cycle"));
        }
        if seen[child] {
            return Err(format!("element {child} is used twice"));
        }
        visit(tree, child, depth + 1, seen, open)?;
    }
    open[index] = false;
    Ok(())
}

/// A short name per element kind. Every kind is matched, like in [`element`]: a new kind in the WIT does not compile
/// until it is drawn, and the test below then needs a sample of it.
#[cfg(test)]
fn kind_name(e: &UiElement) -> &'static str {
    match e {
        UiElement::Card(_) => "card",
        UiElement::Heading(_) => "heading",
        UiElement::Text(_) => "text",
        UiElement::Button(_) => "button",
        UiElement::Input(_) => "input",
        UiElement::Checkbox(_) => "checkbox",
        UiElement::Select(_) => "select",
        UiElement::Table(_) => "table",
        UiElement::Progress(_) => "progress",
        UiElement::Log(_) => "log",
        UiElement::Banner(_) => "banner",
        UiElement::Row(_) => "row",
        UiElement::Column(_) => "column",
    }
}

/// What drawing a tree gave: the user's actions and the number of elements drawn.
#[derive(Default)]
pub struct Drawn {
    pub events: Vec<UiEvent>,
    pub elements: usize,
    /// The id of the input that has the keyboard focus.
    focused: Option<String>,
}

/// Draws a tree after [`validate`]; Err (nothing drawn) for a bad tree.
pub fn render_tree(ui: &mut egui::Ui, palette: &Palette, tree: &UiTree, inputs: &mut HashMap<String, String>) -> Result<Drawn, String> {
    validate(tree)?;
    let mut drawn = Drawn::default();
    element(ui, palette, tree, tree.root as usize, inputs, &mut drawn);
    Ok(drawn)
}

fn lines(text: &str, max_chars: usize) -> String {
    text.lines().take(MAX_LINES).map(|l| display_safe(l, max_chars)).collect::<Vec<_>>().join("\n")
}

/// One element and its children (the tree passed [`validate`]: every index exists, the depth is bounded).
fn element(ui: &mut egui::Ui, palette: &Palette, tree: &UiTree, index: usize, inputs: &mut HashMap<String, String>, out: &mut Drawn) {
    out.elements += 1;
    ui.push_id(("plugin-element", index), |ui| match &tree.elements[index] {
        UiElement::Card(card) => {
            widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
                if !card.title.trim().is_empty() {
                    ui.label(widgets::title_text(&display_safe(&card.title, 120), 16.0));
                    ui.add_space(4.0);
                }
                for child in &card.children {
                    element(ui, palette, tree, *child as usize, inputs, out);
                }
            });
        }
        UiElement::Heading(text) => {
            ui.label(widgets::title_text(&display_safe(text, 200), 18.0));
        }
        UiElement::Text(text) => widgets::text(ui, &lines(text, 2_000), 13.5),
        UiElement::Button(b) => {
            let label = display_safe(&b.label, 80);
            let button = if b.primary { widgets::primary_button(&label) } else { widgets::secondary_button(&label) };
            if ui.add(button).clicked() {
                out.events.push(UiEvent::Clicked(b.id.clone()));
            }
        }
        UiElement::Input(input) => {
            let shown = display_safe(&input.value, 4_000);
            ui.horizontal(|ui| {
                if !input.label.trim().is_empty() {
                    ui.label(RichText::new(display_safe(&input.label, 120)).strong());
                }
                let buffer = inputs.entry(input.id.clone()).or_insert_with(|| shown.clone());
                let response = ui.add(egui::TextEdit::singleline(buffer).char_limit(4_000).desired_width(320.0));
                if response.has_focus() {
                    out.focused = Some(input.id.clone());
                }
                if response.lost_focus() && *buffer != shown {
                    out.events.push(UiEvent::Changed(ChangedData { id: input.id.clone(), value: buffer.clone() }));
                }
            });
        }
        UiElement::Checkbox(c) => {
            let mut checked = c.checked;
            if ui.checkbox(&mut checked, display_safe(&c.label, 200)).changed() {
                out.events.push(UiEvent::Changed(ChangedData { id: c.id.clone(), value: checked.to_string() }));
            }
        }
        UiElement::Select(s) => {
            let selected = s.selected as usize;
            let current = s.options.get(selected).map(|o| display_safe(o, 120)).unwrap_or_default();
            egui::ComboBox::from_id_salt(("plugin-select", index)).selected_text(current).show_ui(ui, |ui| {
                for (n, option) in s.options.iter().enumerate().take(MAX_OPTIONS) {
                    if ui.selectable_label(n == selected, display_safe(option, 120)).clicked() && n != selected {
                        out.events.push(UiEvent::Changed(ChangedData { id: s.id.clone(), value: n.to_string() }));
                    }
                }
            });
        }
        UiElement::Table(table) => {
            egui::ScrollArea::horizontal().id_salt(("plugin-table", index)).show(ui, |ui| {
                egui::Grid::new(("plugin-grid", index)).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                    for column in table.columns.iter().take(MAX_COLUMNS) {
                        ui.label(RichText::new(display_safe(column, 80)).strong());
                    }
                    ui.end_row();
                    for row in table.rows.iter().take(MAX_ROWS) {
                        for cell in row.iter().take(MAX_COLUMNS) {
                            ui.label(display_safe(cell, 200));
                        }
                        ui.end_row();
                    }
                });
            });
            if table.rows.len() > MAX_ROWS {
                widgets::muted(ui, palette, &tf("{count} more rows are not shown.", &[("count", &(table.rows.len() - MAX_ROWS).to_string())]), 12.0);
            }
        }
        UiElement::Progress(p) => {
            let fraction = if p.total == 0 { 0.0 } else { (p.done.min(p.total) as f64 / p.total as f64) as f32 };
            ui.add(egui::ProgressBar::new(fraction).show_percentage().desired_height(10.0));
        }
        UiElement::Log(entries) => {
            let mut log = LogLines::default();
            let skip = entries.len().saturating_sub(MAX_LINES);
            for entry in entries.iter().skip(skip) {
                log.push(display_safe(entry, 1_000));
            }
            log_view::show(ui, &format!("plugin-log-{index}"), &mut log, Some(180.0), "");
        }
        UiElement::Banner(b) => {
            let kind = match b.kind {
                BannerKind::Info => ResultKind::Info,
                BannerKind::Success => ResultKind::Success,
                BannerKind::Warning => ResultKind::Warning,
                BannerKind::Error => ResultKind::Error,
            };
            widgets::result_banner(ui, palette, kind, &display_safe(&b.text, 500), "");
        }
        UiElement::Row(list) => {
            ui.horizontal_wrapped(|ui| {
                for child in list {
                    element(ui, palette, tree, *child as usize, inputs, out);
                }
            });
        }
        UiElement::Column(list) => {
            ui.vertical(|ui| {
                for child in list {
                    element(ui, palette, tree, *child as usize, inputs, out);
                }
            });
        }
    });
}

/// What the page's task reports to the window.
#[derive(Default)]
struct State {
    /// A call (loading, rendering, an event) runs or waits.
    busy: bool,
    tree: Option<Arc<UiTree>>,
    /// Counts the trees received, so the view knows when to reset its input buffers.
    generation: u64,
    /// Why the instance is gone (trapped, timed out, failed to load, stopped by the user).
    stopped: Option<String>,
    /// The plugin's own error from its last event (the instance stays usable).
    failed: Option<String>,
    /// Events sent and not yet handled.
    queued: usize,
}

/// Loads the plugin, renders, then handles events until the page closes (the sender is dropped) or the plugin stops.
async fn run(
    info: ExternalInfo,
    dir: std::path::PathBuf,
    services: Arc<dyn HostServices>,
    approvals: Arc<Approvals>,
    state: Arc<Mutex<State>>,
    mut events: mpsc::UnboundedReceiver<UiEvent>,
    ctx: egui::Context,
) {
    let update = |f: &dyn Fn(&mut State)| {
        f(&mut state.lock());
        ctx.request_repaint();
    };
    let id = info.manifest.id.clone();
    // Held as long as the instance: removing or updating the plugin waits until it is gone.
    let _live = LiveGuard::new(&id);
    let mut instance = match PluginInstance::load(engine(), &dir, info.manifest, services, approvals).await {
        Ok(instance) => instance,
        Err(error) => {
            let message = format!("{error:#}");
            update(&|s| {
                s.busy = false;
                s.stopped = Some(message.clone());
            });
            return;
        }
    };
    loop {
        let rendered = instance.render().await;
        match rendered {
            Ok(tree) => {
                let tree = Arc::new(tree);
                update(&|s| {
                    s.busy = !events.is_empty();
                    s.tree = Some(tree.clone());
                    s.generation += 1;
                });
            }
            Err(error) => {
                let message = error.to_string();
                update(&|s| {
                    s.busy = false;
                    s.stopped = Some(message.clone());
                });
                return;
            }
        }
        // Every queued event first, then one render.
        loop {
            let Some(event) = events.recv().await else { return };
            update(&|s| {
                s.busy = true;
                s.queued = s.queued.saturating_sub(1);
            });
            match instance.event(event).await {
                Ok(()) => update(&|s| s.failed = None),
                Err(PluginError::Failed(message)) => update(&|s| s.failed = Some(message.clone())),
                Err(error) => {
                    let message = error.to_string();
                    update(&|s| {
                        s.busy = false;
                        s.stopped = Some(message.clone());
                    });
                    return;
                }
            }
            if events.is_empty() {
                break;
            }
        }
    }
}

/// An open external plugin page.
pub struct ExternalPluginView {
    info: ExternalInfo,
    /// The plugin's name, safe to show.
    name: String,
    /// Where the plugin is in the list now (`Page::External`).
    pub index: u16,
    approvals: Arc<Approvals>,
    state: Arc<Mutex<State>>,
    output: Arc<Mutex<PluginOutput>>,
    events: Option<mpsc::UnboundedSender<UiEvent>>,
    task: Option<tokio::task::AbortHandle>,
    /// What the user typed into the inputs, by input id, until the plugin draws the page again.
    inputs: HashMap<String, String>,
    generation: u64,
}

impl ExternalPluginView {
    /// Opens the page: starts the plugin.
    pub fn open(info: ExternalInfo, index: u16, shared: &Shared) -> Self {
        let mut view = Self {
            name: display_safe(&info.manifest.name, 80),
            info,
            index,
            approvals: approvals(&shared.ctx),
            state: Arc::default(),
            output: Arc::new(Mutex::new(PluginOutput::new())),
            events: None,
            task: None,
            inputs: HashMap::new(),
            generation: 0,
        };
        view.start(shared);
        view
    }

    pub fn id(&self) -> &str {
        &self.info.manifest.id
    }

    pub fn info(&self) -> &ExternalInfo {
        &self.info
    }

    fn start(&mut self, shared: &Shared) {
        self.stop();
        *self.state.lock() = State { busy: true, ..State::default() };
        self.inputs.clear();
        let (sender, receiver) = mpsc::unbounded_channel();
        let id = self.info.manifest.id.clone();
        let services: Arc<dyn HostServices> =
            Arc::new(AppServices::new(shared.clone(), &self.info.manifest.name, external::plugins().dir(&id), self.output.clone()));
        let dir = external::plugins().dir(&id);
        let task = shared.runtime.spawn(run(self.info.clone(), dir, services, self.approvals.clone(), self.state.clone(), receiver, shared.ctx.clone()));
        self.events = Some(sender);
        self.task = Some(task.abort_handle());
    }

    /// Drops the instance (at the task's next await; a running guest call ends within the call limit) and rejects
    /// whatever it still waits for.
    fn stop(&mut self) {
        self.events = None;
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.approvals.reject_all_for(&self.info.manifest.id);
    }

    fn send(&mut self, event: UiEvent, shared: &Shared) {
        let Some(sender) = &self.events else { return };
        let mut state = self.state.lock();
        if state.queued >= MAX_QUEUE {
            shared.set_status(&tf("{plugin} is busy: wait until it has finished.", &[("plugin", &self.name)]));
            return;
        }
        if sender.send(event).is_ok() {
            state.queued += 1;
            state.busy = true;
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let (busy, tree, generation, stopped, failed) = {
            let s = self.state.lock();
            (s.busy, s.tree.clone(), s.generation, s.stopped.clone(), s.failed.clone())
        };
        if generation != self.generation {
            self.generation = generation;
            // A new page from the plugin: its values win, except in the field being typed in.
            let focused = ui.ctx().data(|d| d.get_temp::<String>(egui::Id::new(("plugin-focus", self.id()))));
            self.inputs.retain(|id, _| Some(id) == focused.as_ref());
        }

        ui.horizontal(|ui| {
            ui.label(widgets::title_text(&self.name, 16.0));
            ui.label(RichText::new(display_safe(&self.info.manifest.version, 40)).size(12.5).color(palette.muted));
            plugin_approval::badges(ui, palette, &self.info);
            if busy && stopped.is_none() {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::secondary_button(t("Reload"))).on_hover_text(t("Start the plugin again")).clicked() {
                    self.start(shared);
                }
            });
        });
        ui.add_space(10.0);

        if plugin_approval::show(ui, palette, &self.approvals, &self.info, &self.name) == Outcome::Stop {
            self.stop();
            self.state.lock().stopped = Some(t("you stopped it").to_string());
            return;
        }

        if let Some(reason) = stopped {
            let mut reload = false;
            widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
                widgets::result_banner(ui, palette, ResultKind::Error, &tf("The plugin stopped: {reason}", &[("reason", &display_safe(&reason, 500))]), "");
                ui.add_space(8.0);
                reload = ui.add(widgets::primary_button(t("Reload"))).clicked();
            });
            if reload {
                self.start(shared);
            }
            ui.add_space(10.0);
        } else {
            if let Some(message) = failed {
                widgets::result_banner(ui, palette, ResultKind::Warning, t("The plugin reported an error"), &display_safe(&message, 500));
                ui.add_space(8.0);
            }
            match tree {
                Some(tree) => match render_tree(ui, palette, &tree, &mut self.inputs) {
                    Ok(drawn) => {
                        ui.ctx().data_mut(|d| match &drawn.focused {
                            Some(id) => d.insert_temp(egui::Id::new(("plugin-focus", self.id())), id.clone()),
                            None => d.remove::<String>(egui::Id::new(("plugin-focus", self.id()))),
                        });
                        for event in drawn.events {
                            self.send(event, shared);
                        }
                    }
                    Err(reason) => {
                        widgets::result_banner(ui, palette, ResultKind::Error, t("The plugin's page cannot be shown"), &display_safe(&reason, 300));
                    }
                },
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("Starting the plugin …"));
                    });
                }
            }
            ui.add_space(10.0);
        }

        let export = self.output.lock().export.clone();
        if let Some(file) = export {
            ui.horizontal(|ui| {
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                widgets::muted(ui, palette, &tf("Saved {name}: open it with Catalogue → Import a file", &[("name", &name)]), 12.5);
                if ui.add(widgets::secondary_button(t("Open folder"))).clicked() {
                    if let Some(folder) = file.parent() {
                        crate::gui::open_url(&folder.display().to_string());
                    }
                }
            });
            ui.add_space(8.0);
        }

        egui::CollapsingHeader::new(RichText::new(t("Plugin log")).size(13.0)).id_salt(("plugin-host-log", self.id())).show(ui, |ui| {
            let mut output = self.output.lock();
            log_view::show(ui, &format!("plugin-host-log-{}", self.info.manifest.id), &mut output.log, Some(200.0), t("Nothing logged yet."));
        });
    }
}

impl Drop for ExternalPluginView {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "external_plugin_e2e.rs"]
mod e2e;

#[cfg(test)]
mod tests {
    use super::*;
    use aio_plugin_host::runtime::{BannerData, ButtonData, CardData, CheckboxData, InputData, ProgressData, SelectData, TableData};

    /// One element of every kind, all reachable from a column at index 0.
    fn every_kind() -> UiTree {
        let elements = vec![
            UiElement::Column((1..=12).collect()),
            UiElement::Card(CardData { title: "Card".into(), children: vec![] }),
            UiElement::Heading("Heading".into()),
            UiElement::Text("line one\nline two".into()),
            UiElement::Button(ButtonData { id: "go".into(), label: "Go".into(), primary: true }),
            UiElement::Input(InputData { id: "name".into(), label: "Name".into(), value: "x".into() }),
            UiElement::Checkbox(CheckboxData { id: "on".into(), label: "On".into(), checked: true }),
            UiElement::Select(SelectData { id: "pick".into(), options: vec!["a".into(), "b".into()], selected: 9 }),
            UiElement::Table(TableData { columns: vec!["c".into()], rows: vec![vec!["1".into(), "extra".into()]] }),
            UiElement::Progress(ProgressData { done: 5, total: 0 }),
            UiElement::Log(vec!["l".into()]),
            UiElement::Banner(BannerData { kind: BannerKind::Warning, text: "careful".into() }),
            UiElement::Row(vec![]),
        ];
        UiTree { root: 0, elements }
    }

    /// Draws `tree` in a window-less egui frame (twice, so the fonts are in).
    fn draw(tree: &UiTree) -> Result<Drawn, String> {
        let ctx = egui::Context::default();
        crate::gui::theme::install_fonts(&ctx);
        let mut result = None;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let palette = Palette::current(ctx);
                    result = Some(render_tree(ui, &palette, tree, &mut HashMap::new()));
                });
            });
        }
        result.unwrap()
    }

    #[test]
    fn every_ui_node_kind_has_a_renderer() {
        let tree = every_kind();
        let mut kinds: Vec<&str> = tree.elements.iter().map(kind_name).collect();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), 13, "one element of each of the 13 kinds");
        let drawn = draw(&tree).unwrap();
        assert_eq!(drawn.elements, tree.elements.len(), "every element is drawn");
        assert!(drawn.events.is_empty());
    }

    #[test]
    fn ui_tree_with_bad_child_index_is_rejected() {
        let mut tree = every_kind();
        tree.elements[0] = UiElement::Column(vec![1, 99]);
        let error = draw(&tree).err().expect("a bad index is an error, not a panic");
        assert!(error.contains("99"), "{error}");
        tree.root = 50;
        assert!(validate(&tree).is_err());
        assert!(validate(&UiTree { root: 0, elements: vec![] }).is_err());
    }

    #[test]
    fn ui_tree_with_a_cycle_is_rejected() {
        let tree = UiTree { root: 0, elements: vec![UiElement::Row(vec![1]), UiElement::Column(vec![0])] };
        let error = draw(&tree).err().expect("a cycle is an error");
        assert!(error.contains("cycle"), "{error}");
        // The same element twice (no cycle, but drawn twice, and 2^n draws in a ladder of them).
        let tree = UiTree { root: 0, elements: vec![UiElement::Row(vec![1, 1]), UiElement::Heading("h".into())] };
        assert!(validate(&tree).is_err());
    }

    #[test]
    fn ui_tree_too_deep_or_too_large_is_rejected() {
        let chain = |n: u32| UiTree { root: 0, elements: (0..n).map(|i| if i + 1 < n { UiElement::Column(vec![i + 1]) } else { UiElement::Text("leaf".into()) }).collect() };
        // Depth 32 (33 elements) is the limit.
        assert!(validate(&chain(MAX_DEPTH as u32 + 1)).is_ok());
        let error = validate(&chain(MAX_DEPTH as u32 + 2)).unwrap_err();
        assert!(error.contains("deep"), "{error}");
        assert!(draw(&chain(10_000)).is_err());
        let wide = UiTree { root: 0, elements: std::iter::once(UiElement::Row((1..=MAX_ELEMENTS as u32).collect())).chain((0..MAX_ELEMENTS).map(|_| UiElement::Text("t".into()))).collect() };
        assert!(validate(&wide).unwrap_err().contains("elements"));
    }
}
