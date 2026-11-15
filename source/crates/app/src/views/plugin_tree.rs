//! The `ui-tree` an external plugin describes its page with. It is untrusted, so it goes through three steps:
//!
//! 1. [`validate`]: the structure (every index exists, no cycle, no element used twice, nesting and element count
//!    bounded) and a work budget over the whole tree (table cells and select options, text and log lines), so no
//!    page can make a frame expensive;
//! 2. [`prepare`]: once per tree, on the plugin's task (not the window's thread), into a display model [`Shown`] with
//!    every string already made safe with `display_safe`;
//! 3. [`show`]: every frame, draws the model with the window's widgets and collects what the user did. Nothing is
//!    sanitised again here.

use std::collections::HashMap;

use aio_plugin_host::display_safe;
use aio_plugin_host::runtime::{BannerKind, ChangedData, UiElement, UiEvent, UiTree};
use eframe::egui::{self, RichText};

use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};

/// Most elements a page may have.
pub const MAX_ELEMENTS: usize = 2_000;
/// Deepest nesting of containers (the root is depth 0).
pub const MAX_DEPTH: usize = 32;
/// Table cells (headers included) plus select options in a whole page.
pub const MAX_CELLS: usize = 20_000;
/// Lines of text and log elements in a whole page.
pub const MAX_LINES: usize = 5_000;

/// Strings made safe so far, per thread (the test checks that drawing makes none). Test builds only.
/// Strings made safe so far, per thread (the test checks that drawing makes none). Test builds only.
#[cfg(test)]
static SANITISED: std::sync::Mutex<Vec<(std::thread::ThreadId, usize)>> = std::sync::Mutex::new(Vec::new());

/// Strings this thread made safe so far (test builds only).
#[cfg(test)]
fn sanitised_here() -> usize {
    let me = std::thread::current().id();
    SANITISED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(t, _)| *t == me).map_or(0, |(_, n)| *n)
}

/// `display_safe`, counted in tests.
fn safe(text: &str, max_chars: usize) -> String {
    #[cfg(test)]
    {
        let me = std::thread::current().id();
        let mut all = SANITISED.lock().unwrap_or_else(|e| e.into_inner());
        match all.iter_mut().find(|(t, _)| *t == me) {
            Some((_, n)) => *n += 1,
            None => all.push((me, 1)),
        }
    }
    display_safe(text, max_chars)
}

/// The children of a container.
fn children(e: &UiElement) -> &[u32] {
    match e {
        UiElement::Card(c) => &c.children,
        UiElement::Row(c) | UiElement::Column(c) => c,
        _ => &[],
    }
}

/// Checks a tree before anything is prepared or drawn: the root and every child index exist, no element is reached
/// twice (so no cycle), nesting at most [`MAX_DEPTH`], at most [`MAX_ELEMENTS`] elements, and the whole page within
/// [`MAX_CELLS`] and [`MAX_LINES`].
pub fn validate(tree: &UiTree) -> Result<(), String> {
    let count = tree.elements.len();
    if count > MAX_ELEMENTS {
        return Err(format!("the page has {count} elements; at most {MAX_ELEMENTS} are allowed"));
    }
    let root = tree.root as usize;
    if root >= count {
        return Err(format!("the top element {root} does not exist (the page has {count} elements)"));
    }
    let (mut cells, mut lines) = (0usize, 0usize);
    for e in &tree.elements {
        match e {
            UiElement::Table(t) => cells += t.columns.len() + t.rows.iter().map(Vec::len).sum::<usize>(),
            UiElement::Select(s) => cells += s.options.len(),
            UiElement::Text(text) => lines += text.lines().count().max(1),
            UiElement::Log(entries) => lines += entries.len(),
            _ => {}
        }
    }
    if cells > MAX_CELLS {
        return Err(format!("the page has {cells} table cells and options; at most {MAX_CELLS} are allowed"));
    }
    if lines > MAX_LINES {
        return Err(format!("the page has {lines} lines of text and log; at most {MAX_LINES} are allowed"));
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

/// One element ready to draw: every text already safe. Containers keep their children's indices.
#[derive(Debug, Clone)]
pub enum Node {
    Card { title: String, children: Vec<usize> },
    Heading(String),
    Text(String),
    Button { id: String, label: String, primary: bool },
    Input { id: String, label: String, value: String },
    Checkbox { id: String, label: String, checked: bool },
    Select { id: String, options: Vec<String>, selected: usize, current: String },
    Table { columns: Vec<String>, rows: Vec<Vec<String>> },
    Progress(f32),
    /// Index into [`Shown::logs`].
    Log(usize),
    Banner(ResultKind, String),
    Row(Vec<usize>),
    Column(Vec<usize>),
}

/// A checked page, ready to draw.
#[derive(Clone)]
pub struct Shown {
    pub root: usize,
    pub nodes: Vec<Node>,
    /// The log elements' lines (a log view keeps its joined text between frames).
    pub logs: Vec<LogLines>,
}

fn indices(list: &[u32]) -> Vec<usize> {
    list.iter().map(|&i| i as usize).collect()
}

/// Checks `tree` ([`validate`]) and turns it into the display model. Run it once per tree, off the window's thread.
pub fn prepare(tree: &UiTree) -> Result<Shown, String> {
    validate(tree)?;
    let mut logs = Vec::new();
    let nodes = tree
        .elements
        .iter()
        .map(|e| match e {
            UiElement::Card(c) => Node::Card { title: safe(&c.title, 120), children: indices(&c.children) },
            UiElement::Heading(text) => Node::Heading(safe(text, 200)),
            // Line breaks stay; each line is made safe on its own.
            UiElement::Text(text) => Node::Text(text.lines().map(|l| safe(l, 2_000)).collect::<Vec<_>>().join("\n")),
            UiElement::Button(b) => Node::Button { id: b.id.clone(), label: safe(&b.label, 80), primary: b.primary },
            UiElement::Input(i) => Node::Input { id: i.id.clone(), label: safe(&i.label, 120), value: safe(&i.value, 4_000) },
            UiElement::Checkbox(c) => Node::Checkbox { id: c.id.clone(), label: safe(&c.label, 200), checked: c.checked },
            UiElement::Select(s) => {
                let options: Vec<String> = s.options.iter().map(|o| safe(o, 120)).collect();
                let selected = s.selected as usize;
                let current = options.get(selected).cloned().unwrap_or_default();
                Node::Select { id: s.id.clone(), options, selected, current }
            }
            UiElement::Table(t) => Node::Table {
                columns: t.columns.iter().map(|c| safe(c, 80)).collect(),
                rows: t.rows.iter().map(|r| r.iter().map(|c| safe(c, 200)).collect()).collect(),
            },
            UiElement::Progress(p) => Node::Progress(if p.total == 0 { 0.0 } else { (p.done.min(p.total) as f64 / p.total as f64) as f32 }),
            UiElement::Log(entries) => {
                let mut log = LogLines::default();
                for entry in entries {
                    log.push(safe(entry, 1_000));
                }
                logs.push(log);
                Node::Log(logs.len() - 1)
            }
            UiElement::Banner(b) => {
                let kind = match b.kind {
                    BannerKind::Info => ResultKind::Info,
                    BannerKind::Success => ResultKind::Success,
                    BannerKind::Warning => ResultKind::Warning,
                    BannerKind::Error => ResultKind::Error,
                };
                Node::Banner(kind, safe(&b.text, 500))
            }
            UiElement::Row(list) => Node::Row(indices(list)),
            UiElement::Column(list) => Node::Column(indices(list)),
        })
        .collect();
    Ok(Shown { root: tree.root as usize, nodes, logs })
}

/// What drawing a page gave: the user's actions and the number of elements drawn.
#[derive(Default)]
pub struct Drawn {
    pub events: Vec<UiEvent>,
    pub elements: usize,
    /// The id of the input that has the keyboard focus.
    pub focused: Option<String>,
}

/// Draws a prepared page. `inputs` holds what the user typed, by input id, until the plugin draws the page again.
pub fn show(ui: &mut egui::Ui, palette: &Palette, shown: &mut Shown, inputs: &mut HashMap<String, String>) -> Drawn {
    let mut drawn = Drawn::default();
    let Shown { root, nodes, logs } = shown;
    node(ui, palette, nodes, logs, *root, inputs, &mut drawn);
    drawn
}

/// One element and its children (the model came from a checked tree: every index exists, the depth is bounded).
fn node(ui: &mut egui::Ui, palette: &Palette, nodes: &[Node], logs: &mut [LogLines], index: usize, inputs: &mut HashMap<String, String>, out: &mut Drawn) {
    out.elements += 1;
    ui.push_id(("plugin-element", index), |ui| match &nodes[index] {
        Node::Card { title, children } => {
            widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
                if !title.is_empty() {
                    ui.label(widgets::title_text(title, 16.0));
                    ui.add_space(4.0);
                }
                for &child in children {
                    node(ui, palette, nodes, logs, child, inputs, out);
                }
            });
        }
        Node::Heading(text) => {
            ui.label(widgets::title_text(text, 18.0));
        }
        Node::Text(text) => widgets::text(ui, text, 13.5),
        Node::Button { id, label, primary } => {
            let button = if *primary { widgets::primary_button(label) } else { widgets::secondary_button(label) };
            if ui.add(button).clicked() {
                out.events.push(UiEvent::Clicked(id.clone()));
            }
        }
        Node::Input { id, label, value } => {
            ui.horizontal(|ui| {
                if !label.is_empty() {
                    ui.label(RichText::new(label).strong());
                }
                let buffer = inputs.entry(id.clone()).or_insert_with(|| value.clone());
                let response = ui.add(egui::TextEdit::singleline(buffer).char_limit(4_000).desired_width(320.0));
                if response.has_focus() {
                    out.focused = Some(id.clone());
                }
                if response.lost_focus() && buffer != value {
                    out.events.push(UiEvent::Changed(ChangedData { id: id.clone(), value: buffer.clone() }));
                }
            });
        }
        Node::Checkbox { id, label, checked } => {
            let mut on = *checked;
            if ui.checkbox(&mut on, label.as_str()).changed() {
                out.events.push(UiEvent::Changed(ChangedData { id: id.clone(), value: on.to_string() }));
            }
        }
        Node::Select { id, options, selected, current } => {
            egui::ComboBox::from_id_salt(("plugin-select", index)).selected_text(current.as_str()).show_ui(ui, |ui| {
                for (n, option) in options.iter().enumerate() {
                    if ui.selectable_label(n == *selected, option.as_str()).clicked() && n != *selected {
                        out.events.push(UiEvent::Changed(ChangedData { id: id.clone(), value: n.to_string() }));
                    }
                }
            });
        }
        Node::Table { columns, rows } => {
            egui::ScrollArea::horizontal().id_salt(("plugin-table", index)).show(ui, |ui| {
                egui::Grid::new(("plugin-grid", index)).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                    for column in columns {
                        ui.label(RichText::new(column).strong());
                    }
                    ui.end_row();
                    for row in rows {
                        for cell in row {
                            ui.label(cell.as_str());
                        }
                        ui.end_row();
                    }
                });
            });
        }
        Node::Progress(fraction) => {
            ui.add(egui::ProgressBar::new(*fraction).show_percentage().desired_height(10.0));
        }
        Node::Log(n) => {
            if let Some(log) = logs.get_mut(*n) {
                log_view::show(ui, &format!("plugin-log-{index}"), log, Some(180.0), "");
            }
        }
        Node::Banner(kind, text) => widgets::result_banner(ui, palette, *kind, text, ""),
        Node::Row(list) => {
            ui.horizontal_wrapped(|ui| {
                for &child in list {
                    node(ui, palette, nodes, logs, child, inputs, out);
                }
            });
        }
        Node::Column(list) => {
            ui.vertical(|ui| {
                for &child in list {
                    node(ui, palette, nodes, logs, child, inputs, out);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use aio_plugin_host::runtime::{BannerData, ButtonData, CardData, CheckboxData, InputData, ProgressData, SelectData, TableData};

    /// A short name per element kind. Every kind is matched, like in [`prepare`]: a new kind in the WIT does not
    /// compile until it is drawn, and the test below then needs a sample of it.
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

    /// Draws a prepared page in a window-less egui frame `frames` times (the first one installs the fonts).
    fn draw(shown: &mut Shown, frames: usize) -> Drawn {
        let ctx = egui::Context::default();
        crate::gui::theme::install_fonts(&ctx);
        let mut result = None;
        for _ in 0..frames {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let palette = Palette::current(ctx);
                    result = Some(show(ui, &palette, shown, &mut HashMap::new()));
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
        let drawn = draw(&mut prepare(&tree).unwrap(), 2);
        assert_eq!(drawn.elements, tree.elements.len(), "every element is drawn");
        assert!(drawn.events.is_empty());
    }

    #[test]
    fn ui_tree_with_bad_child_index_is_rejected() {
        let mut tree = every_kind();
        tree.elements[0] = UiElement::Column(vec![1, 99]);
        let error = prepare(&tree).err().expect("a bad index is an error, not a panic");
        assert!(error.contains("99"), "{error}");
        tree.root = 50;
        assert!(validate(&tree).is_err());
        assert!(validate(&UiTree { root: 0, elements: vec![] }).is_err());
    }

    #[test]
    fn ui_tree_with_a_cycle_is_rejected() {
        let tree = UiTree { root: 0, elements: vec![UiElement::Row(vec![1]), UiElement::Column(vec![0])] };
        let error = prepare(&tree).err().expect("a cycle is an error");
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
        assert!(prepare(&chain(10_000)).is_err());
        let wide = UiTree { root: 0, elements: std::iter::once(UiElement::Row((1..=MAX_ELEMENTS as u32).collect())).chain((0..MAX_ELEMENTS).map(|_| UiElement::Text("t".into()))).collect() };
        assert!(validate(&wide).unwrap_err().contains("elements"));
    }

    #[test]
    fn oversized_table_budget_is_rejected() {
        let table = |rows: usize| UiElement::Table(TableData { columns: vec!["a".into(); 50], rows: vec![vec!["x".into(); 50]; rows] });
        // 50 header cells + 399 × 50 = 20,000 cells: the limit.
        assert!(validate(&UiTree { root: 0, elements: vec![table(399)] }).is_ok());
        let error = validate(&UiTree { root: 0, elements: vec![table(400)] }).unwrap_err();
        assert!(error.contains("table cells"), "{error}");
        // Spread over many small tables, the total counts.
        let many = UiTree { root: 0, elements: std::iter::once(UiElement::Column((1..=41).collect())).chain((0..41).map(|_| table(9))).collect() };
        assert!(validate(&many).is_err());
        // Lines of text and logs across the page.
        let lines = UiTree {
            root: 0,
            elements: vec![UiElement::Column(vec![1, 2]), UiElement::Text("l\n".repeat(3_000)), UiElement::Log(vec!["e".into(); 2_001])],
        };
        assert!(validate(&lines).unwrap_err().contains("lines"));
    }

    #[test]
    fn rendering_does_not_sanitise_again() {
        let tree = every_kind();
        let before = sanitised_here();
        let mut shown = prepare(&tree).unwrap();
        let prepared = sanitised_here();
        assert!(prepared > before, "prepare makes the strings safe");
        draw(&mut shown, 3);
        assert_eq!(sanitised_here(), prepared, "drawing reuses the prepared strings");
    }

    #[test]
    fn prepared_strings_are_safe() {
        let tree = UiTree { root: 0, elements: vec![UiElement::Heading("a\u{202E}b".into())] };
        let shown = prepare(&tree).unwrap();
        assert!(matches!(&shown.nodes[0], Node::Heading(h) if h == "a\u{FFFD}b"));
    }
}
