# The page (UI)

A plugin has one page. It does not draw it: in `render` it describes the page with the `Ui` builder, the Manager
draws it with its own widgets inside a frame labelled "Drawn by the plugin <name>", and sends back what the user did
as an `Event`.

## The cycle

1. The page opens. The Manager makes the plugin's instance (`Plugin::new`) and calls `render`.
2. The user clicks a button, ticks a checkbox, picks an option or edits an input. The Manager calls `on_event` with
   that one event.
3. Whatever `on_event` returns, the Manager calls `render` again and shows the new page.

So `render` should only read the state and describe it; change the state in `on_event`. Events are handled one at a
time, in order. While the plugin is busy, the Manager queues up to 32 events; past that, further clicks are dropped
and the status line says "<plugin> is busy: wait until it has finished."

An `Err(text)` from `on_event` is shown above the page as "The plugin reported an error" with your text, and written
to the plugin log. The plugin keeps running. A panic, running out of memory or going over the time limit stops it
instead: the page says "The plugin stopped: <reason>" and offers **Reload**, which starts a fresh instance.

## The builder

`render` gets a `&mut Ui`. Each method appends one element to the current container: the page itself at top level,
or the card, row or column whose closure you are in. Elements show in the order you add them.

```rust
use aio_plugin_sdk::ui::{BannerKind, Ui};

let mut ui = Ui::new();
ui.heading("Backups");
ui.card("New backup", |ui| {
    ui.input("name", "Name", "nightly");
    ui.row(|ui| {
        ui.primary_button("start", "Start");
        ui.button("cancel", "Cancel");
    });
});
ui.banner(BannerKind::Info, "The last backup finished at 03:00.");
assert!(ui.finish().is_ok());
```

(This sample is a doctest of the SDK's `ui` module.)

Texts take anything that converts into `String` (`&str`, `String`, `format!(...)`); lists take any iterator of them.

| Method | Element | Sends |
|---|---|---|
| `heading(text)` | A title line. | |
| `text(text)` | Plain text; line breaks are kept. | |
| `button(id, label)` | A button. | `Clicked { id }` |
| `primary_button(id, label)` | A button drawn as the main action (use at most one per page or card). | `Clicked { id }` |
| `input(id, label, value)` | A one-line text field showing `value`. | `Changed { id, value: <the whole text> }`, see below |
| `checkbox(id, label, checked)` | A checkbox. | `Changed { id, value: "true" or "false" }` |
| `select(id, options, selected)` | A drop-down list; `selected` is the index of the chosen option. | `Changed { id, value: <the index as text, "0", "1", ...> }` |
| `table(columns, rows)` | A table of text cells: column headers, then rows of cells. Rows should have one cell per column. | |
| `progress(done, total)` | A progress bar. `done` past `total` shows full; a `total` of 0 shows empty. | |
| `log(lines)` | A scrolling list of log lines. | |
| `banner(kind, text)` | A coloured message line: `BannerKind::Info`, `Success`, `Warning` or `Error`. | |
| `card(title, \|ui\| ...)` | A framed group; the closure adds its children top to bottom. An empty title shows none. | |
| `row(\|ui\| ...)` | Children left to right. | |
| `column(\|ui\| ...)` | Children top to bottom. | |
| `finish()` | Ends the page: `Ok(UiTree)`, or the first rule broken as an `Error`. You call it only in tests. | |

Leaf methods return `()`, so they fit in closures as they are: `ui.row(|ui| ui.button("ok", "OK"))`.

## Events

```rust
pub enum Event {
    Clicked { id: String },
    Changed { id: String, value: String },
}
```

`event.id()` gives the id of the control in both cases. `Event` is `#[non_exhaustive]`: a later 1.x version may add
events, so always end a `match` on it with a `_` arm.

### Ids

Ids tell you which control an event came from, so every control of a page (button, input, checkbox, select) needs its
own non-empty id. Two controls with the same id, even of different kinds, break the page. For repeated rows, build the
id from the row: `format!("delete-{index}")`, then `id.strip_prefix("delete-")` in `on_event`.

### Inputs report their text when they lose the focus

An input does not send an event for each key. The Manager keeps what the user types on its side, and sends
`Changed { id, value }` with the whole text **only when the field loses the keyboard focus**: the user presses Enter,
clicks elsewhere (a button included), or presses Tab or Escape. Nothing is sent when the text equals the `value` you
rendered. A field holds at most 4,000 characters.

What this means for your plugin:

- **Keep the text in your state.** Store `value` from `Changed`, and pass it back as `value` in the next `render`.
  After each new page from the plugin, every field shows the plugin's `value` again, except the field the user is
  typing in, which keeps its text. A field whose `Changed` you ignore jumps back to your old value.
- **Typing then clicking a button** sends two events: the field loses the focus when the mouse button goes down, and
  the click counts when it is released. So the plugin normally gets `Changed` for the field first, then `Clicked` for
  the button, and the button's handler can use the stored text (as `add` does in the example below).
- **Text typed but never confirmed is lost** if the page closes while the field still has the focus.
- There is no multi-line text field in API 1.0.

## Rules and limits

The builder only produces pages the Manager accepts. A page that breaks one of these rules is refused as a whole:

| Rule | Limit |
|---|---|
| Elements on the page | At most 2,000, **counting the page itself**: the page is an implicit column holding your top-level elements, so you can add 1,999. Containers count as elements. |
| Nesting | At most 32 levels. The page is level 0, what you add at top level is level 1, and so on: a leaf can sit inside at most 31 cards, rows or columns. |
| Table cells and select options | At most 20,000 over the whole page, column headers included. |
| Lines | At most 5,000 over the whole page: each `text` counts its lines (at least one), each `log` entry counts one. |
| Control ids | Unique within the page, and not empty. |
| Select | `selected` must name an option (`0` is allowed when there are no options). |

The first problem found is the one reported; everything added after it is ignored. When `render` produces such a
page, the Manager shows a page with a single error banner holding the reason, for example
`the page has more than 2000 elements` or `the id "a" is used by two controls; ids must be unique within a page`,
and the plugin log gets a warning `the page was refused: <reason>`. The other messages are listed in
[Troubleshooting](troubleshooting.md#the-page-shows-a-single-red-banner).

The limits are also constants in the SDK: `aio_plugin_sdk::ui::limits::{MAX_ELEMENTS, MAX_DEPTH, MAX_CELLS, MAX_LINES}`.

For large data, show a page of it (for example 100 rows with **Previous** and **Next** buttons) rather than everything.

Every text you put on the page is shown as text: control, direction-changing and invisible characters are replaced
before drawing, so a plugin cannot imitate the Manager's own screens.

## A full example

A task list with an input, a button, a select, checkboxes and a progress bar, saved in the plugin's private folder
(permission `files`). It was compiled for `wasm32-wasip2` with clippy `-D warnings`, and its test passes.

```rust
use aio_plugin_sdk::ui::BannerKind;
use aio_plugin_sdk::{export_plugin, files, Event, Plugin, Ui};

const FILE: &str = "tasks.txt";

struct Tasks {
    /// The text of the "New task" field, as the Manager last sent it.
    draft: String,
    /// (done, text) for each task.
    items: Vec<(bool, String)>,
    /// 0 = all, 1 = open, 2 = done: the index of the chosen option.
    filter: usize,
    /// The last error, shown as a banner.
    error: Option<String>,
}

impl Tasks {
    fn save(&mut self) {
        let text: String = self
            .items
            .iter()
            .map(|(done, text)| format!("{}{text}\n", if *done { "x " } else { "- " }))
            .collect();
        self.error = files::write_str(FILE, &text).err().map(|e| e.to_string());
    }
}

impl Plugin for Tasks {
    fn new() -> Self {
        // A missing file is not an error: the list starts empty.
        let (text, error) = match files::read_to_string_if_exists(FILE) {
            Ok(text) => (text.unwrap_or_default(), None),
            Err(e) => (String::new(), Some(e.to_string())),
        };
        let items = text
            .lines()
            .filter_map(|line| line.split_at_checked(2))
            .map(|(mark, text)| (mark == "x ", text.to_string()))
            .collect();
        Tasks { draft: String::new(), items, filter: 0, error }
    }

    fn render(&mut self, ui: &mut Ui) {
        ui.heading("Tasks");
        if let Some(error) = &self.error {
            ui.banner(BannerKind::Error, error.as_str());
        }
        ui.row(|ui| {
            ui.input("draft", "New task", self.draft.as_str());
            ui.primary_button("add", "Add");
        });
        ui.select("filter", ["All", "Open", "Done"], self.filter);
        ui.card("List", |ui| {
            for (i, (done, text)) in self.items.iter().enumerate() {
                let shown = match self.filter {
                    1 => !done,
                    2 => *done,
                    _ => true,
                };
                if shown {
                    ui.checkbox(format!("task-{i}"), text.as_str(), *done);
                }
            }
        });
        let done = self.items.iter().filter(|(done, _)| *done).count();
        ui.progress(done as u64, self.items.len() as u64);
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        match event {
            // Sent when the field loses the focus (Enter, or a click elsewhere such as on "Add").
            Event::Changed { id, value } if id == "draft" => self.draft = value,
            Event::Clicked { id } if id == "add" => {
                let text = self.draft.trim().to_string();
                if !text.is_empty() {
                    self.items.push((false, text));
                    self.draft.clear();
                    self.save();
                }
            }
            Event::Changed { id, value } if id == "filter" => {
                self.filter = value.parse().map_err(|_| format!("unexpected filter {value}"))?;
            }
            Event::Changed { id, value } => {
                let index = id.strip_prefix("task-").and_then(|n| n.parse::<usize>().ok());
                if let Some(item) = index.and_then(|i| self.items.get_mut(i)) {
                    item.0 = value == "true";
                    self.save();
                }
            }
            _ => {}
        }
        Ok(())
    }
}

export_plugin!(Tasks);
```

## Testing a page

`Ui::new()` and `Ui::finish()` work in a native `cargo test`. `finish()` returns a `UiTree`: `elements()` is a flat
list where each container comes before its children and holds their indices, and `root()` is the page's own column
(always 0). Match elements with `..`, because struct-like variants may gain fields in 1.x:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aio_plugin_sdk::ui::Element;

    #[test]
    fn the_filter_hides_finished_tasks() {
        let mut plugin = Tasks { draft: String::new(), items: vec![(true, "a".into()), (false, "b".into())], filter: 1, error: None };
        let mut ui = Ui::new();
        plugin.render(&mut ui);
        let page = ui.finish().unwrap();
        let boxes = page.elements().iter().filter(|e| matches!(e, Element::Checkbox { .. })).count();
        assert_eq!(boxes, 1);
    }
}
```

The test builds `Tasks` directly instead of calling `Tasks::new()`, because `new()` reads a file, and file calls only
work inside the Manager.
