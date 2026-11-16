//! The page builder: [`Ui`] describes a plugin page, [`Ui::finish`] turns it into a [`UiTree`].
//!
//! A page is a column of elements. Leaves (headings, texts, controls, tables, logs, banners) are added in order;
//! containers ([`card`](Ui::card), [`row`](Ui::row), [`column`](Ui::column)) take a closure that adds their
//! children:
//!
//! ```
//! use aio_plugin_sdk::ui::{BannerKind, Ui};
//!
//! let mut ui = Ui::new();
//! ui.heading("Backups");
//! ui.card("New backup", |ui| {
//!     ui.input("name", "Name", "nightly");
//!     ui.row(|ui| {
//!         ui.primary_button("start", "Start");
//!         ui.button("cancel", "Cancel");
//!     });
//! });
//! ui.banner(BannerKind::Info, "The last backup finished at 03:00.");
//! assert!(ui.finish().is_ok());
//! ```
//!
//! # Rules
//!
//! The builder only emits pages the Manager accepts. [`Ui::finish`] returns an [`Error`] (kind
//! [`ErrorKind::Other`]) instead of a tree when the page:
//!
//! * is past one of the Manager's [`limits`];
//! * gives two controls (buttons, inputs, checkboxes, selects) the same id, or one an empty id: ids tell the
//!   plugin which control an [`Event`](crate::Event) came from, so they must be unique within a page;
//! * has a select whose chosen index names no option.
//!
//! The first problem found is the one reported; everything added after it is ignored. When a plugin's
//! [`render`](crate::Plugin::render) produces such a page, [`export_plugin!`](crate::export_plugin) shows a page
//! with a single error banner holding the message instead.

use std::collections::HashSet;

use crate::error::{Error, ErrorKind};

/// The Manager's limits on a page. A page past any of them is refused by the Manager, so [`Ui::finish`] refuses it
/// first.
pub mod limits {
    /// Most elements a page may have, the page's own column included.
    pub const MAX_ELEMENTS: usize = 2_000;
    /// Deepest nesting of containers. The page's column is depth 0, what is added at top level depth 1, the
    /// children of a top-level container depth 2, and so on.
    pub const MAX_DEPTH: usize = 32;
    /// Table cells (column headers included) plus select options, over the whole page.
    pub const MAX_CELLS: usize = 20_000;
    /// Lines of text elements (a text counts its lines, at least one) plus log entries, over the whole page.
    pub const MAX_LINES: usize = 5_000;
}

use limits::{MAX_CELLS, MAX_DEPTH, MAX_ELEMENTS, MAX_LINES};

/// The colour and icon of a [`banner`](Ui::banner).
///
/// New kinds may be added in a later 1.x version: match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BannerKind {
    /// Neutral information.
    Info,
    /// Something finished well.
    Success,
    /// Something the user should look at.
    Warning,
    /// Something failed.
    Error,
}

/// One element of a finished [`UiTree`]. Containers refer to their children by index into
/// [`UiTree::elements`].
///
/// Plugins build pages with [`Ui`]; this type is what a finished page holds, for tests that check what a plugin
/// renders. New elements may be added in a later 1.x version: match with a `_` arm. Struct-like variants may gain
/// fields too: match them with `..` (for example `Element::Button { id, .. }`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Element {
    /// A framed group with an optional title (empty for none).
    #[non_exhaustive]
    Card {
        /// The title, shown above the children; empty for none.
        title: String,
        /// The children, top to bottom.
        children: Vec<usize>,
    },
    /// A title line.
    Heading(String),
    /// Plain text; line breaks are kept.
    Text(String),
    /// A button. Pressing it sends [`Event::Clicked`](crate::Event::Clicked).
    #[non_exhaustive]
    Button {
        /// The control id.
        id: String,
        /// The text on the button.
        label: String,
        /// Whether it is drawn as the page's main action.
        primary: bool,
    },
    /// A one-line text field. When it loses the keyboard focus with a changed text, the Manager sends
    /// [`Event::Changed`](crate::Event::Changed) with the text.
    #[non_exhaustive]
    Input {
        /// The control id.
        id: String,
        /// The label in front of the field.
        label: String,
        /// The text shown in the field.
        value: String,
    },
    /// A checkbox. Toggling it sends [`Event::Changed`](crate::Event::Changed) with `"true"` or `"false"`.
    #[non_exhaustive]
    Checkbox {
        /// The control id.
        id: String,
        /// The text next to the box.
        label: String,
        /// Whether it is ticked.
        checked: bool,
    },
    /// A drop-down list. Choosing an option sends [`Event::Changed`](crate::Event::Changed) with its index.
    #[non_exhaustive]
    Select {
        /// The control id.
        id: String,
        /// The options, in order.
        options: Vec<String>,
        /// Index into `options` of the chosen one.
        selected: usize,
    },
    /// A table of text cells.
    #[non_exhaustive]
    Table {
        /// The column headers.
        columns: Vec<String>,
        /// The rows, each a list of cells.
        rows: Vec<Vec<String>>,
    },
    /// A progress bar at `done` out of `total` (empty when `total` is 0).
    #[non_exhaustive]
    Progress {
        /// The work done so far.
        done: u64,
        /// The whole work.
        total: u64,
    },
    /// A scrolling list of log lines.
    Log(Vec<String>),
    /// A coloured message line.
    #[non_exhaustive]
    Banner {
        /// The colour and icon.
        kind: BannerKind,
        /// The message.
        text: String,
    },
    /// Children laid out left to right.
    Row(Vec<usize>),
    /// Children laid out top to bottom.
    Column(Vec<usize>),
}

/// A finished page: a flat list of elements and the index of the top one. Made by [`Ui::finish`]; it always
/// passes the Manager's checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiTree {
    root: usize,
    elements: Vec<Element>,
}

impl UiTree {
    /// Index into [`elements`](UiTree::elements) of the top element (the page's column).
    pub fn root(&self) -> usize {
        self.root
    }

    /// Every element of the page. A container comes before its children.
    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    /// A page holding only a banner: what is shown when a plugin's page is refused.
    pub(crate) fn banner_page(kind: BannerKind, text: &str) -> Self {
        Self { root: 0, elements: vec![Element::Banner { kind, text: text.to_string() }] }
    }

    /// The root and the elements, for the conversion to the WIT types.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn into_parts(self) -> (usize, Vec<Element>) {
        (self.root, self.elements)
    }
}

/// Describes a plugin page. Each method appends an element to the current container (the page itself at top
/// level); see the [module documentation](self) for the rules.
///
/// A plugin gets one in [`Plugin::render`](crate::Plugin::render). [`Ui::new`] makes one for tests.
#[derive(Debug, Clone)]
pub struct Ui {
    elements: Vec<Element>,
    /// The open containers, innermost last: indices into `elements`. The page's column (0) is always first.
    open: Vec<usize>,
    /// The control ids used so far.
    ids: HashSet<String>,
    /// Table cells and select options so far.
    cells: usize,
    /// Text lines and log entries so far.
    lines: usize,
    /// The first problem found; once set, nothing more is added.
    error: Option<Error>,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

/// Turns a list of strings into owned strings.
fn strings(items: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    items.into_iter().map(Into::into).collect()
}

impl Ui {
    /// An empty page.
    pub fn new() -> Self {
        Self {
            elements: vec![Element::Column(Vec::new())],
            open: vec![0],
            ids: HashSet::new(),
            cells: 0,
            lines: 0,
            error: None,
        }
    }

    /// A title line.
    pub fn heading(&mut self, text: impl Into<String>) {
        self.push(Element::Heading(text.into()));
    }

    /// Plain text. Line breaks are kept; each line counts towards [`limits::MAX_LINES`].
    pub fn text(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.count_lines(text.lines().count().max(1)) {
            self.push(Element::Text(text));
        }
    }

    /// A button. Pressing it sends [`Event::Clicked`](crate::Event::Clicked) with `id`.
    pub fn button(&mut self, id: impl Into<String>, label: impl Into<String>) {
        self.add_button(id.into(), label.into(), false);
    }

    /// A button drawn as the page's main action (use at most one per page or card). Pressing it sends
    /// [`Event::Clicked`](crate::Event::Clicked) with `id`.
    pub fn primary_button(&mut self, id: impl Into<String>, label: impl Into<String>) {
        self.add_button(id.into(), label.into(), true);
    }

    /// A one-line text field showing `value`.
    ///
    /// The Manager keeps what the user types on its side and sends [`Event::Changed`](crate::Event::Changed) with
    /// `id` and the whole text only when the field loses the keyboard focus (the user presses Enter or clicks
    /// elsewhere, a button included) and the text differs from `value`; not on every keystroke. Keep the text in
    /// the plugin's state and pass it back as `value` on the next render. The Manager accepts at most 4,000
    /// characters.
    pub fn input(&mut self, id: impl Into<String>, label: impl Into<String>, value: impl Into<String>) {
        let id = id.into();
        if self.claim(&id) {
            self.push(Element::Input { id, label: label.into(), value: value.into() });
        }
    }

    /// A checkbox. Toggling it sends [`Event::Changed`](crate::Event::Changed) with `id` and `"true"` or
    /// `"false"`.
    pub fn checkbox(&mut self, id: impl Into<String>, label: impl Into<String>, checked: bool) {
        let id = id.into();
        if self.claim(&id) {
            self.push(Element::Checkbox { id, label: label.into(), checked });
        }
    }

    /// A drop-down list with `options`, `selected` being the index of the chosen one (0 when `options` is empty).
    /// Choosing an option sends [`Event::Changed`](crate::Event::Changed) with `id` and the option's index as
    /// text. Each option counts towards [`limits::MAX_CELLS`].
    pub fn select(&mut self, id: impl Into<String>, options: impl IntoIterator<Item = impl Into<String>>, selected: usize) {
        let id = id.into();
        if !self.claim(&id) {
            return;
        }
        let options = strings(options);
        if selected >= options.len().max(1) {
            let message = format!("the select \"{id}\" has no option {selected} (it has {} options)", options.len());
            self.fail(message);
            return;
        }
        if self.count_cells(options.len()) {
            self.push(Element::Select { id, options, selected });
        }
    }

    /// A table with `columns` headers and `rows` of cells. Every header and cell counts towards
    /// [`limits::MAX_CELLS`]. Rows should have one cell per column.
    pub fn table(
        &mut self,
        columns: impl IntoIterator<Item = impl Into<String>>,
        rows: impl IntoIterator<Item = impl IntoIterator<Item = impl Into<String>>>,
    ) {
        if self.error.is_some() {
            return;
        }
        let columns = strings(columns);
        let rows: Vec<Vec<String>> = rows.into_iter().map(strings).collect();
        if self.count_cells(columns.len() + rows.iter().map(Vec::len).sum::<usize>()) {
            self.push(Element::Table { columns, rows });
        }
    }

    /// A progress bar at `done` out of `total`; `done` past `total` shows as full, a `total` of 0 as empty.
    pub fn progress(&mut self, done: u64, total: u64) {
        self.push(Element::Progress { done, total });
    }

    /// A scrolling list of log lines. Each entry counts towards [`limits::MAX_LINES`].
    pub fn log(&mut self, lines: impl IntoIterator<Item = impl Into<String>>) {
        if self.error.is_some() {
            return;
        }
        let lines = strings(lines);
        if self.count_lines(lines.len()) {
            self.push(Element::Log(lines));
        }
    }

    /// A coloured message line.
    pub fn banner(&mut self, kind: BannerKind, text: impl Into<String>) {
        self.push(Element::Banner { kind, text: text.into() });
    }

    /// A framed group: `add` adds its children, top to bottom. An empty `title` shows none.
    pub fn card(&mut self, title: impl Into<String>, add: impl FnOnce(&mut Ui)) {
        self.container(Element::Card { title: title.into(), children: Vec::new() }, add);
    }

    /// A row: `add` adds its children, laid out left to right.
    pub fn row(&mut self, add: impl FnOnce(&mut Ui)) {
        self.container(Element::Row(Vec::new()), add);
    }

    /// A column: `add` adds its children, laid out top to bottom.
    pub fn column(&mut self, add: impl FnOnce(&mut Ui)) {
        self.container(Element::Column(Vec::new()), add);
    }

    /// The finished page, or the first problem found (an [`Error`] of kind [`ErrorKind::Other`]): a Manager
    /// limit passed, a duplicate or empty control id, or a select index that names no option.
    pub fn finish(self) -> Result<UiTree, Error> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(UiTree { root: 0, elements: self.elements }),
        }
    }

    fn add_button(&mut self, id: String, label: String, primary: bool) {
        if self.claim(&id) {
            self.push(Element::Button { id, label, primary });
        }
    }

    /// Records the first problem; later ones are ignored.
    fn fail(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(Error::new(ErrorKind::Other, message));
        }
    }

    /// Reserves a control id: false (and the page fails) if the page already failed, or the id is empty or taken.
    fn claim(&mut self, id: &str) -> bool {
        if self.error.is_some() {
            return false;
        }
        if id.is_empty() {
            self.fail("a control has an empty id".to_string());
            return false;
        }
        if !self.ids.insert(id.to_string()) {
            self.fail(format!("the id \"{id}\" is used by two controls; ids must be unique within a page"));
            return false;
        }
        true
    }

    /// Adds `n` table cells or select options: false (and the page fails) past [`MAX_CELLS`].
    fn count_cells(&mut self, n: usize) -> bool {
        self.cells = self.cells.saturating_add(n);
        if self.cells > MAX_CELLS {
            self.fail(format!("the page has more than {MAX_CELLS} table cells and select options"));
            return false;
        }
        self.error.is_none()
    }

    /// Adds `n` text lines or log entries: false (and the page fails) past [`MAX_LINES`].
    fn count_lines(&mut self, n: usize) -> bool {
        self.lines = self.lines.saturating_add(n);
        if self.lines > MAX_LINES {
            self.fail(format!("the page has more than {MAX_LINES} lines of text and log"));
            return false;
        }
        self.error.is_none()
    }

    /// Appends `element` to the current container and returns its index, or `None` (and the page fails) when
    /// it would be past [`MAX_DEPTH`] or [`MAX_ELEMENTS`].
    fn push(&mut self, element: Element) -> Option<usize> {
        if self.error.is_some() {
            return None;
        }
        // The page's column is depth 0, so a new element sits at the number of open containers.
        if self.open.len() > MAX_DEPTH {
            self.fail(format!("the page is nested too deep (more than {MAX_DEPTH} levels)"));
            return None;
        }
        if self.elements.len() >= MAX_ELEMENTS {
            self.fail(format!("the page has more than {MAX_ELEMENTS} elements"));
            return None;
        }
        let index = self.elements.len();
        self.elements.push(element);
        let parent = *self.open.last().unwrap_or(&0);
        match &mut self.elements[parent] {
            Element::Card { children, .. } | Element::Row(children) | Element::Column(children) => children.push(index),
            // Only containers are opened; this is reached only if a closure swapped the `Ui` it was given.
            _ => self.fail("a container's children were added to another page".to_string()),
        }
        Some(index)
    }

    /// Adds a container, then its children with `add`.
    fn container(&mut self, element: Element, add: impl FnOnce(&mut Ui)) {
        if let Some(index) = self.push(element) {
            self.open.push(index);
            add(self);
            // The page's column is never closed, even if `add` swapped this `Ui` for another.
            if self.open.len() > 1 {
                self.open.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_page_is_a_single_banner() {
        let tree = UiTree::banner_page(BannerKind::Error, "oops");
        assert_eq!(tree.root(), 0);
        assert_eq!(tree.elements(), [Element::Banner { kind: BannerKind::Error, text: "oops".into() }]);
    }
}
