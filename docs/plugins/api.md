# API reference

Every public item of `aio-plugin-sdk` 1.1.0, module by module, with the permission it needs, its limits and the error
kinds it returns. The SDK's own documentation (`cargo doc --open` in your plugin project) has the same items with
more examples.

Every Rust sample of this guide is built for `wasm32-wasip2` by the SDK's test `tests/guide_samples.rs`, so they
stay in step with the SDK. They need the permissions named next to them in the plugin's `manifest.json`.

Items marked **API 1.1** are new in SDK 1.1 and need `"api": "1.1"` in the manifest. A plugin built with SDK 1.1
needs it anyway (`aio-plugin check` says so), whether it uses them or not.

Contents: [the crate root](#the-crate-root) · [errors](#errors) · [`ui`](#ui) · [`host`](#host) · [`files`](#files)
· [`folders`](#folders) · [`sql`](#sql) · [`http`](#http) · [`tcp`](#tcp) · [`script`](#script)

## The crate root

```rust
pub trait Plugin: 'static {
    fn new() -> Self where Self: Sized;
    fn render(&mut self, ui: &mut Ui);
    fn on_event(&mut self, event: Event) -> std::result::Result<(), String>;
    fn tick(&mut self) -> std::result::Result<(), String> { Ok(()) }
}
```

| Item | What it does |
|---|---|
| `Plugin::new()` | The starting state. Called once, the first time the Manager needs the plugin (when its page opens), before the first `render`. Host calls are allowed here. |
| `Plugin::render(&mut self, ui)` | Describes the page. Called when the page opens and after every event. See [The page](ui.md). |
| `Plugin::on_event(&mut self, event)` | Handles one user action. `Err(text)` is shown to the user ("The plugin reported an error") and logged; the plugin keeps running and the page is rendered again. |
| `Plugin::tick(&mut self)` | **API 1.1.** One step of a long job: called after a render when the last event or tick called `host::keep_working`. Events waiting meanwhile go first. `Err(text)` is shown like one from `on_event`. The default does nothing. See [Long jobs](ui.md#long-jobs). |
| `export_plugin!(Type)` | Exports `Type` (which implements `Plugin`) as the component's plugin, with its `render`, `on_event` and `tick`. Use it once, at the top level of the crate. On targets other than `wasm32` it expands to nothing, so `cargo test` works natively. |
| `enum Event` | `Clicked { id: String }` or `Changed { id: String, value: String }`. `#[non_exhaustive]`: match it with a `_` arm. |
| `Event::id(&self) -> &str` | The id of the control the event came from. |
| `Ui` | The page builder (re-export of `ui::Ui`). |
| `Error`, `ErrorKind`, `Result<T, E = Error>` | See [Errors](#errors). |

The Manager keeps one instance per open page and calls it from one thread. The instance lives while the page is open;
closing the page, switching the plugin off or removing it drops it. A plugin never runs in the background: a long job
runs in ticks only while its page is open.

Each call into the plugin (`new` plus the first `render`, each `on_event`, each `render`, each `tick`) may run for at
most 5 s of plugin time. Time spent in a dialog, waiting for an approval, or waiting on the database, the network or a local
connection does not count: those have their own limits, listed below. See [Limits](limits.md).

## Errors

Every fallible call returns `aio_plugin_sdk::Result<T>`, that is `Result<T, aio_plugin_sdk::Error>`.

| Item | What it does |
|---|---|
| `Error::new(kind, message)` | An error of your own, reported the same way. |
| `Error::kind(&self) -> ErrorKind` | The broad reason, to decide what to do. |
| `Error::message(&self) -> &str` | The text to show. `Display` prints the same text. |
| `impl std::error::Error for Error` | So it works with `Box<dyn Error>` and friends. |
| `impl From<Error> for String` | So `?` works inside `on_event`, which returns `Result<(), String>`. The string is the message. |

`ErrorKind` (`#[non_exhaustive]`, match it with a `_` arm):

| Kind | When |
|---|---|
| `Denied` | The plugin is not allowed to do this: a permission is missing from its manifest, or the request is outside what the permission covers (another table, host, port, a private column, a path that leads out of the private folder). Also a picked file used the wrong way, an HTTP redirect to another host, a host the user refused under `http:ask`, and a write a picked folder refuses. The message starts with `permission denied: ` in most cases (see [Troubleshooting](troubleshooting.md#permission-denied)). |
| `Rejected` | The user was asked to approve a script or database changes and said no. |
| `Timeout` | The action passed its time limit. The message is `<what> timed out after <n> s`. (`Conn::recv_timeout` is not an error when its time is up: it returns `Ok(None)`.) |
| `NotFound` | A private file that does not exist, or a picked file that was removed after the user chose it. The message is exactly `file not found`. |
| `Other` | Anything else: no server connected (`Not connected`), a network or database error, `HTTP 404`, a quota, an invalid path, a value of the wrong type, a page past the limits. |

```rust
use aio_plugin_sdk::{Error, ErrorKind};

fn explain(error: &Error) -> String {
    match error.kind() {
        ErrorKind::Denied => format!("Not allowed: {error}"),
        ErrorKind::Rejected => "You said no.".to_string(),
        ErrorKind::Timeout => "That took too long; try again.".to_string(),
        ErrorKind::NotFound => "The file is gone.".to_string(),
        _ => format!("It failed: {error}"),
    }
}
```

A refused call is also written to the plugin log as a warning (`permission denied: <reason>`), so you see it while
developing even if your code ignores the error.

## `ui`

The page builder. The rules and the event semantics are in [The page](ui.md); this is the list of items.

| Item | Signature / meaning |
|---|---|
| `Ui::new()` | An empty page (also `Ui::default()`). The Manager hands you one in `render`; make your own in tests. |
| `Ui::heading` | `fn heading(&mut self, text: impl Into<String>)` |
| `Ui::text` | `fn text(&mut self, text: impl Into<String>)`: each line counts towards `MAX_LINES`. |
| `Ui::button` | `fn button(&mut self, id: impl Into<String>, label: impl Into<String>)` |
| `Ui::primary_button` | `fn primary_button(&mut self, id: impl Into<String>, label: impl Into<String>)` |
| `Ui::input` | `fn input(&mut self, id: impl Into<String>, label: impl Into<String>, value: impl Into<String>)`: at most 4,000 characters, reported on focus loss. |
| `Ui::checkbox` | `fn checkbox(&mut self, id: impl Into<String>, label: impl Into<String>, checked: bool)` |
| `Ui::select` | `fn select(&mut self, id: impl Into<String>, options: impl IntoIterator<Item = impl Into<String>>, selected: usize)`: each option counts towards `MAX_CELLS`. |
| `Ui::table` | `fn table(&mut self, columns: impl IntoIterator<Item = impl Into<String>>, rows: impl IntoIterator<Item = impl IntoIterator<Item = impl Into<String>>>)`: every header and cell counts towards `MAX_CELLS`. |
| `Ui::progress` | `fn progress(&mut self, done: u64, total: u64)` |
| `Ui::log` | `fn log(&mut self, lines: impl IntoIterator<Item = impl Into<String>>)`: each entry counts towards `MAX_LINES`. |
| `Ui::banner` | `fn banner(&mut self, kind: BannerKind, text: impl Into<String>)` |
| `Ui::card` | `fn card(&mut self, title: impl Into<String>, add: impl FnOnce(&mut Ui))` |
| `Ui::row` | `fn row(&mut self, add: impl FnOnce(&mut Ui))` |
| `Ui::column` | `fn column(&mut self, add: impl FnOnce(&mut Ui))` |
| `Ui::finish` | `fn finish(self) -> Result<UiTree, Error>`: the page, or the first rule broken (kind `Other`). |
| `UiTree::root()` | `-> usize`: index of the page's own column (always 0). |
| `UiTree::elements()` | `-> &[Element]`: every element; a container comes before its children and lists their indices. |
| `enum Element` | `Card { title, children }`, `Heading(String)`, `Text(String)`, `Button { id, label, primary }`, `Input { id, label, value }`, `Checkbox { id, label, checked }`, `Select { id, options, selected }`, `Table { columns, rows }`, `Progress { done, total }`, `Log(Vec<String>)`, `Banner { kind, text }`, `Row(Vec<usize>)`, `Column(Vec<usize>)`. For tests. `#[non_exhaustive]`, and the struct-like variants are too: match them with `..`. |
| `enum BannerKind` | `Info`, `Success`, `Warning`, `Error`. `#[non_exhaustive]`. |
| `ui::limits::MAX_ELEMENTS` | `2_000` (the page's own column included) |
| `ui::limits::MAX_DEPTH` | `32` (the page's column is depth 0) |
| `ui::limits::MAX_CELLS` | `20_000` (table headers and cells plus select options) |
| `ui::limits::MAX_LINES` | `5_000` (text lines plus log entries) |

Permission: none.

## `host`

The Manager itself: the plugin log, the status line, dialogs, files the user picks, long jobs and the Manager's other
pages. Permission: none.

| Item | What it does |
|---|---|
| `enum Level` | `Debug`, `Info`, `Warn`, `Error`. `#[non_exhaustive]`. |
| `log(level: Level, text: &str)` | Appends a line to the plugin log (on the plugin's page, **Plugin log**). |
| `status(text: &str)` | Sets the Manager's status line. Also logged. |
| `language() -> String` | **API 1.1.** The Manager's current language code: `"en"`, or one of `it fr de nl es pt tr fi`. It can change between two calls. No permission. See [Translations](translations.md). |
| `ask(question: &str) -> bool` | A yes/no question. `true` for yes. Closing the dialog answers no. |
| `prompt_text(title: &str, label: &str, default: &str) -> Option<String>` | Asks for one line of text; `default` is already typed in. `None` if cancelled. |
| `pick_open_file(title: &str, extensions: impl IntoIterator<Item = impl AsRef<str>>) -> Option<PickedFile>` | An open-file dialog. `extensions` without dots (`["sql", "txt"]`); an empty list allows every file (write it with its type: `[] as [&str; 0]`). `None` if cancelled. |
| `pick_save_file(title: &str, suggested_name: &str, extensions: impl IntoIterator<Item = impl AsRef<str>>) -> Option<PickedFile>` | A save-file dialog with `suggested_name` filled in. `None` if cancelled. |
| `open_import_preview(file: &PickedFile) -> Result<()>` | Hands a file picked with `pick_open_file` to the Catalogue's import: the Manager saves a copy in `configuration/plugins/ext-exports/<id>/` and the status line says "Saved <name>: open it with Catalogue → Import a file". At most 32 MiB. |
| `struct PickedFile` | A file the user picked. The plugin never sees its path. Released when dropped. |
| `PickedFile::name(&self) -> String` | The file name, without folders. |
| `PickedFile::is_for_saving(&self) -> bool` | `true` for a file from `pick_save_file`. |
| `PickedFile::read(&self) -> Result<Vec<u8>>` | The whole content of a file picked for opening. At most 32 MiB. |
| `PickedFile::write(&self, data: &[u8]) -> Result<()>` | Replaces the content of a file picked for saving. |
| `enum ManagerPage` | **API 1.1.** A page of the Manager a plugin may open: `GearthTool` (Tools → G-Earth, where the user installs and starts G-Earth). `#[non_exhaustive]`. |
| `open_manager_page(page: ManagerPage)` | **API 1.1.** Switches the Manager to `page`, as if the user had chosen it: the plugin's page closes (the plugin stops, like when the user leaves its page) and nothing is started on the new page. Nothing happens while the Manager is busy (a tool or approved work runs). No permission. |
| `keep_working()` | **API 1.1.** Asks for one more `Plugin::tick` after the next render. It holds only for the call that makes it: each event and tick starts without it, so call it in every event handler and tick that leaves work to do. See [Long jobs](ui.md#long-jobs). |

Before a file dialog opens, the Manager shows its own window saying what the plugin wants ("The plugin wants you to
choose a file to open. It can read that one file, nothing else."). Every dialog of a plugin also offers **Stop this
plugin**; the call then gets the cancel answer (`false` or `None`) and the plugin is stopped.

Limits: one call (`render`, `on_event` or `tick`) may open at most 20 dialogs (the folder picker of
[`folders`](#folders) included). Past that, every further dialog gets the cancel
answer without being shown, and the log says once: "more than 20 dialogs in one call: further dialogs are cancelled
without being shown". The user's time in a dialog does not count against the call's 5 s. The texts of `ask` and
`prompt_text` are cut to 1,000 characters, a file dialog's title to 120. A log line keeps at most 4 KiB; one call may write at most 1,000 entries and 256 KiB (status texts
included), then the log says "log truncated by the host" and drops the rest until the next call. The plugin's stdout
and stderr (`println!`, `eprintln!`, a panic message) also go to the log, one entry per line, as Info and Error.

Errors:

| Call | Errors |
|---|---|
| `PickedFile::read`, `open_import_preview` | `Denied` on a file picked for saving (`a file picked for saving cannot be read`); `NotFound` if the file was removed; `Other` past 32 MiB (`file larger than 32 MiB`). |
| `PickedFile::write` | `Denied` on a file picked for opening (`a file picked for opening cannot be written`) or when the system refuses access; `Other` for other I/O errors. |

```rust
use aio_plugin_sdk::host::{self, Level};

fn host_calls() -> aio_plugin_sdk::Result<()> {
    host::log(Level::Info, "starting the export");
    host::status("Exporting ...");
    if !host::ask("Export every room?") {
        return Ok(());
    }
    let Some(name) = host::prompt_text("Export", "File title", "rooms") else {
        return Ok(()); // cancelled
    };
    if let Some(file) = host::pick_save_file("Export rooms", "rooms.csv", ["csv"]) {
        file.write(format!("{name}\n").as_bytes())?;
    }
    if let Some(file) = host::pick_open_file("Import a catalogue", ["sql"]) {
        host::open_import_preview(&file)?;
    }
    Ok(())
}
```

Off `wasm32` (a native `cargo test`), `log` and `status` print to stderr, `language` returns `"en"`; the other calls panic.

A plugin that needs G-Earth (a packet logger) can send the user to the page that installs it:

```rust
use aio_plugin_sdk::host::{self, ManagerPage};

fn need_gearth() {
    if host::ask("G-Earth is not running. Open the G-Earth page to install or start it?") {
        host::open_manager_page(ManagerPage::GearthTool);
    }
}
```

## `i18n`

Translations, **API 1.1**. Permission: none. See [Translations](translations.md) for the file format.

| Item | What it does |
|---|---|
| `tr(english: &str) -> String` | The translation of `english` in the Manager's current language, from the plugin's `i18n/<lang>.json`; `english` itself when there is none (English, no file, or no such key). Off `wasm32` it returns `english`. |
| `tr_with(english: &str, values: &[(&str, &str)]) -> String` | `tr`, then fills each `{name}` placeholder from `values` in one pass; a placeholder with no value stays as written. |

## `files`

The plugin's private folder: files only this plugin can read and write, kept between sessions and across updates by
the same author. Permission: `files`. The Manager lists it as always allowed and does not enforce it, but declare it
so the user sees that the plugin keeps files.

| Item | What it does |
|---|---|
| `read(path: &str) -> Result<Vec<u8>>` | The whole content of a private file. |
| `write(path: &str, data: &[u8]) -> Result<()>` | Creates or replaces a private file, creating missing folders. |
| `read_to_string(path: &str) -> Result<String>` | The content as text. `Other` if it is not UTF-8 (`the file '<path>' is not UTF-8 text`). |
| `write_str(path: &str, text: &str) -> Result<()>` | Writes `text` as UTF-8. |
| `read_if_exists(path: &str) -> Result<Option<Vec<u8>>>` | Like `read`, but a missing file is `Ok(None)`. |
| `read_to_string_if_exists(path: &str) -> Result<Option<String>>` | Like `read_to_string`, but a missing file is `Ok(None)`. |

Paths are relative to the private folder and use `/` between folders (`"notes/today.txt"`). Rules:

- at most 255 bytes and 8 levels; each name only `A-Z a-z 0-9 . _ -`; no empty, `.` or `..` names, no name ending in
  `.`, no Windows device name (`CON`, `NUL`, `COM1`, ...);
- one file holds at most 32 MiB; the folder holds at most 64 MiB in all and at most 10,000 files and folders.

Errors:

| Kind | Messages |
|---|---|
| `NotFound` | `file not found` (`read` only) |
| `Denied` | `access denied: outside the private folder` (a path that leads out through a link); `access denied` (the system refused) |
| `Other` | `invalid path: use a relative path with / and the characters A-Z a-z 0-9 . _ -`; `file larger than 32 MiB`; `the private folder is full (64 MiB)`; `the private folder is full (10000 files and folders)`; `a folder has that name`; `not a file`; `i/o error (<kind>)` |

```rust
use aio_plugin_sdk::{files, ErrorKind};

fn files_calls() -> aio_plugin_sdk::Result<()> {
    files::write_str("settings/theme.txt", "dark")?;
    let theme = files::read_to_string_if_exists("settings/theme.txt")?.unwrap_or_default();
    let raw: Vec<u8> = match files::read("cache.bin") {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let _ = (theme, raw);
    Ok(())
}
```

The folder is `configuration/plugins/ext/<id>/data/` for an installed plugin and
`configuration/plugins/ext-dev/<id>/data/` for a developer folder. Removing the plugin deletes it.

## `folders`

**API 1.1.** A folder the user picks, where the plugin may create data files (an export, a downloaded catalogue with
its pictures) and read the data files already there (an earlier export to resume or compare with). Permission: none,
for writing as for reading, because the user picks the folder each time. The plugin never sees the folder's path, only
its name.

| Item | What it does |
|---|---|
| `pick(title: &str) -> Option<Folder>` | Shows a folder dialog titled `title`. `None` if the user cancelled, picked a folder that cannot be picked (see below), or past the 20 dialogs of a call. |
| `struct Folder` | A picked folder. Released when dropped. |
| `Folder::name(&self) -> String` | The folder's name (its last path component), to tell the user where the files went. |
| `Folder::write(&self, path: &str, data: &[u8]) -> Result<()>` | Creates the file `path` (and the folders it needs), or rewrites a file this `Folder` created. |
| `Folder::write_str(&self, path: &str, text: &str) -> Result<()>` | The same with `text` as UTF-8. |
| `Folder::append(&self, path: &str, data: &[u8]) -> Result<()>` | Appends `data` to a file this `Folder` created (with `write` or `append`), or creates it (and the folders it needs) when it does not exist yet: a large file in pieces, never whole in memory. |
| `Folder::append_str(&self, path: &str, text: &str) -> Result<()>` | The same with `text` as UTF-8. |
| `Folder::read(&self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>>` | At most `max` bytes (at most `MAX_READ`, 4 MiB) of the data file `path` from byte `offset`: fewer at the end of the file, none at or past it. Any data file in the folder, not only this `Folder`'s own. |
| `Folder::read_all(&self, path: &str) -> Result<Vec<u8>>` | The whole file, read with `read` in pieces of 4 MiB until one comes back short. It must fit in the plugin's 64 MiB. |
| `Folder::list(&self, path: &str) -> Result<Vec<Entry>>` | The entries of the folder `path` (`""` for the picked folder itself), sorted by name, at most `MAX_ENTRIES`. Files of every type, with their sizes, and folders; links, hidden names and names a path cannot name are left out. |
| `struct Entry` | `name: String`, `is_folder: bool`, `size: u64` (0 for a folder). `#[non_exhaustive]`. |
| `Folder::reveal(&self) -> Result<()>` | Opens the folder in the system's file manager. At most once every 2 s per `Folder`. |
| `Folder::open_import_preview(&self, path: &str) -> Result<()>` | Hands a file this `Folder` wrote, of at most 32 MiB, to the Catalogue's import, like `host::open_import_preview`. |
| `DATA_EXTENSIONS` | The file types a plugin may write: `json`, `sql`, `txt`, `md`, `csv`, `tsv`, `xml`, `html`, `png`, `gif`, `jpg`, `jpeg`, `webp`, `bmp`, `svg`, `zip`, `nitro`, `swf`, `bin`, `log`. |
| `MAX_FILES` | `100_000`: most files one `Folder` writes. |
| `MAX_BYTES` | `2 * 1024 * 1024 * 1024`: most bytes one `Folder` writes in all (2 GiB, the current sizes of its files). |
| `MAX_FILE` | `256 * 1024 * 1024`: the largest file (256 MiB). |
| `MAX_READ` | `4 * 1024 * 1024`: most bytes one `read` returns (4 MiB). |
| `MAX_ENTRIES` | `10_000`: most entries one `list` returns. |

Before the dialog opens, the Manager says what the plugin wants ("The plugin wants you to choose a folder. It can
create data files in it (pictures, text, SQL, JSON), nothing else: files already there stay as they are.").

Rules:

- paths follow the [`files`](#files) rules: relative with `/`, at most 255 bytes and 8 levels, names of
  `A-Z a-z 0-9 . _ -`, no `..`, no drive, no Windows device name;
- only data files: the last extension (any letter case) is one of `DATA_EXTENSIONS`, and no name in the path starts
  with `.`;
- a write creates a new file. A file that is already there and was not written through this `Folder` is never
  touched, and neither is a path that leads through a link. A rewrite of the plugin's own file replaces it whole: a
  failed rewrite leaves the old content;
- `append` follows the same rules: it extends only a file this `Folder` created, and creates a new one like `write`.
  The file's size after the append counts against `MAX_FILE`, the appended bytes against `MAX_BYTES`, a new file
  against `MAX_FILES`. A failed append may leave part of the data at the end of the file (a failed first append
  leaves no file); a later `write` replaces the file whole and it stays the `Folder`'s own;
- `read` reads any data file in the folder by the same path rules and file types, also one the `Folder` did not
  write, but never through a link, never a file that has other names (a hard link, which could be a file outside the
  folder) and never a file in a folder of the Manager. Read a large file in pieces and keep only what you need of each:
  the plugin has 64 MiB of memory in all. The time spent reading does not count against the call's 5 s;
- `list` shows files of every type (only data files can be read) and folders; a folder with more than `MAX_ENTRIES`
  entries is refused;
- a new `pick` of the same folder starts over: it cannot rewrite the files of an earlier `Folder`;
- some folders cannot be picked: a drive's root, the home folder itself, a startup folder (or anything inside one), and
  the Manager's own install and configuration folders (or anything inside them). `pick` returns `None` and the plugin
  log says why, for example `the picked folder was refused: permission denied: pick a folder of its own for this`.

| Kind | When |
|---|---|
| `Denied` | `permission denied: <path> is not a data file`; `permission denied: <path> is hidden (a name starts with .)`; `permission denied: <path> already exists`; `permission denied: <path> leads through a link` (or `a file`); `permission denied: <path> has other names (hard links)` (`append` to, or `read` of, a file that is hard-linked); `permission denied: <path> is not a plain file` (`read`); `permission denied: at most 4 MiB can be read at once` (`read`); `permission denied: <path> has more than 10000 entries` (`list`; `the folder` for `""`); `permission denied: folder limit reached`; `permission denied: this folder belongs to the Manager`; `permission denied: the folder was replaced by a link`; `permission denied: <path> was not written through this folder` (`open_import_preview`); `permission denied: the folder was shown less than 2 s ago` (`reveal`). Every refusal starts with `permission denied: `. |
| `NotFound` | `file not found` (`read`, `list` or `open_import_preview` of a file or folder that is not there). |
| `Other` | An invalid path (the [`files`](#files) message); `not a file` (`read` of a folder); `not a folder` (`list` of a file); `file larger than 32 MiB` (`open_import_preview`); `the file manager could not be opened (<kind>)`; other I/O errors. |

```rust
use aio_plugin_sdk::{folders, host};

fn export_catalogue() -> aio_plugin_sdk::Result<()> {
    let Some(folder) = folders::pick("Save the catalogue") else {
        return Ok(()); // cancelled
    };
    folder.write_str("catalogue/furni.json", "{}")?;
    folder.write("catalogue/icons/chair.png", &[0x89, b'P', b'N', b'G'])?;
    // A large file in pieces: only one piece is in memory at a time.
    for page in 0..3 {
        folder.append_str("catalogue/items.sql", &format!("-- page {page}\n"))?;
    }
    host::status(&format!("Saved in {}", folder.name()));
    folder.reveal()?;
    Ok(())
}

/// Counts the lines of an earlier export without holding it whole in memory.
fn count_lines() -> aio_plugin_sdk::Result<usize> {
    let Some(folder) = folders::pick("Choose an earlier export") else {
        return Ok(0);
    };
    let mut lines = 0;
    for entry in folder.list("catalogue")? {
        if entry.is_folder || !entry.name.ends_with(".sql") {
            continue;
        }
        let path = format!("catalogue/{}", entry.name);
        let mut offset = 0;
        loop {
            let piece = folder.read(&path, offset, folders::MAX_READ)?;
            if piece.is_empty() {
                break;
            }
            lines += piece.iter().filter(|b| **b == b'\n').count();
            offset += piece.len() as u64;
        }
    }
    Ok(lines)
}
```

## `sql`

The hotel database the Manager is connected to. A plugin never sends SQL text: it builds a query from parts (table,
columns, conditions, values), and the Manager checks each part against the permissions and runs it with parameters,
so values never become SQL. The database calls need the Manager to be logged in to the server and to have a saved
MariaDB account for it (log in once in the SQL Editor or the Catalogue and save it).

### Reading

Permission: `sql:read:<table>`, with the table written exactly as in the query (case included). A
[private column](permissions.md#private-columns) also needs `sql:read-private:<table>.<column>`.

| Item | What it does |
|---|---|
| `Select::from(table: impl Into<String>) -> Select` | A query on `table`, with no columns yet. |
| `Select::columns(self, columns: impl IntoIterator<Item = impl Into<String>>) -> Select` | Adds columns to read (in that order). There is no `*`: list them. |
| `Select::filter(self, column: impl Into<String>, op: Op, value: impl Into<Value>) -> Select` | Keeps rows where `column <op> value`. Several filters are combined with AND. |
| `Select::limit(self, n: u32) -> Select` | Reads at most `n` rows. Without it, 1,000. The Manager lowers anything above 10,000 to 10,000. |
| `Select::run(&self) -> Result<Vec<Row>>` | Runs the query. |
| `Row::get<T: FromValue>(&self, column: &str) -> Result<T>` | The value as `i64`, `f64`, `String`, `Vec<u8>`, `Value`, or an `Option` of one of them for a column that can be NULL. No conversion is made: an integer column reads as `i64` only; DECIMAL, date and time columns arrive as text. A mismatch is `Other`, for example `column 'online' holds NULL, not text (read it as an Option)`. |
| `Row::value(&self, column: &str) -> Option<&Value>` | The raw value, if the query read that column. Names match exactly first, then ignoring ASCII case. |
| `Row::columns(&self) -> &[String]` | The column names, in the query's order. |
| `Row::values(&self) -> &[Value]` | The values, in the same order. |
| `trait FromValue` | The types `Row::get` can return (above). Sealed: you cannot implement it. |
| `enum Op` | `Eq` (`=`), `Ne` (`<>`), `Lt`, `Le`, `Gt`, `Ge`, `Like` (`%` and `_` are wildcards), `IsNull`, `NotNull` (their value is ignored: pass `Value::Null`). `#[non_exhaustive]`. |
| `enum Value` | `Null`, `Int(i64)`, `Float(f64)`, `Text(String)`, `Bytes(Vec<u8>)`. `#[non_exhaustive]`. Unsigned integers above `i64::MAX` arrive as `Text`. |
| `From<...> for Value` | `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32` (Int), `bool` (Int 1 or 0), `f32`, `f64` (Float), `String`, `&str`, `&String` (Text), `Vec<u8>`, `&[u8]` (Bytes), and `Option<T>` of any of them (`None` is Null). |

```rust
use aio_plugin_sdk::host::{self, Level};
use aio_plugin_sdk::sql::{Op, Select};

// manifest: "sql:read:users"
fn sql_read() -> aio_plugin_sdk::Result<()> {
    let rows = Select::from("users")
        .columns(["id", "username", "online"])
        .filter("rank", Op::Ge, 5)
        .filter("username", Op::Like, "adm%")
        .limit(50)
        .run()?;
    for row in &rows {
        let id: i64 = row.get("id")?;
        let name: String = row.get("username")?;
        let online: Option<String> = row.get("online")?;
        host::log(Level::Debug, &format!("{id} {name} {online:?}"));
    }
    Ok(())
}
```

### Changing

Permission: `sql:write:<table>` for every table changed (and `sql:read-private:<table>.<column>` to set or filter on
a private column).

| Item | What it does |
|---|---|
| `insert(table: impl Into<String>) -> Insert` | A new row. Add values with `Insert::value(self, column, value)`. |
| `update(table: impl Into<String>) -> Update` | A change. Add `Update::set(self, column, value)` and at least one `Update::filter(self, column, op, value)`. |
| `delete(table: impl Into<String>) -> Delete` | A removal. Add at least one `Delete::filter(self, column, op, value)`. |
| `struct WriteOp` | One operation of a batch: an `Insert`, `Update` or `Delete` converted with `.into()` (or `WriteOp::from`). |
| `write(ops: impl IntoIterator<Item = WriteOp>) -> Result<u64>` | Shows the whole batch to the user on an approval card and, once the user clicks **Approve**, applies it in one transaction. Returns the number of rows changed. |
| `MAX_WRITE_OPS` | `1_000`: most operations in one batch. |

```rust
use aio_plugin_sdk::host;
use aio_plugin_sdk::sql::{self, Op};

// manifest: "sql:write:bans,users,room_bans"
fn sql_write() -> aio_plugin_sdk::Result<()> {
    let changed = sql::write([
        sql::insert("bans").value("user_id", 7).value("reason", "spam").into(),
        sql::update("users").set("credits", 100).filter("id", Op::Eq, 7).into(),
        sql::delete("room_bans").filter("user_id", Op::Eq, 7).into(),
    ])?;
    host::status(&format!("{changed} rows changed"));
    Ok(())
}
```

The user's time to decide does not count against any limit; the approved batch then has 30 s.

### Rules and errors

- Table and column names: a letter or `_`, then letters, digits or `_`, at most 64 in all. No `db.table`, no
  expressions.
- A read lists 1 to 256 columns; a statement has at most 64 conditions; an insert or update sets 1 to 256 columns,
  each once.
- An update or a delete needs at least one condition (there is no "every row").
- A float must be finite (no NaN, no infinity), in values and in conditions.
- A batch has 1 to 1,000 operations and at most 8 MiB of values (text and bytes by length, 8 bytes per number).
- A read has 30 s; an approved batch has 30 s.

| Kind | When |
|---|---|
| `Denied` | A missing permission (`permission denied: table 'users' is not declared with sql:read`), a table name with another case (`... (write the table name exactly as declared)`), a private column (`permission denied: column 'users.mail' is private (needs sql:read-private:users.mail)`), or any rule above. The SDK refuses a read without columns, an empty or too large batch, an insert or update without a column, an update or delete without a condition and a non-finite float itself, with the Manager's message, before asking the Manager. |
| `Rejected` | `the user rejected the database changes` (`write` only). |
| `Timeout` | `the query timed out after 30 s`, `the database changes timed out after 30 s`. |
| `Other` | `Not connected` (no server); `The server changed since the request` (the login changed while the approval was open); `Dry run: there is no database`; `No saved MariaDB account for <host> on this computer: ...`; an error from MariaDB (an unknown column, a duplicate key, ...); a `Row::get` mismatch. |

## `http`

HTTP(S) GET to the hosts the plugin declares. Permission: `http:<host>` for each host, and with API 1.1 `http:ask` to
ask the user about other hosts.

| Item | What it does |
|---|---|
| `get(url: &str) -> Result<Vec<u8>>` | GETs `url` and returns the response body. |
| `get_many(urls: &[&str]) -> Vec<Result<Vec<u8>>>` | **API 1.1.** Up to `MAX_MANY` (8) GETs at once: one result per URL, in the order of `urls`. Each URL is checked like `get`; the bodies of one call add up to at most `MAX_MANY_BYTES` (32 MiB). |
| `MAX_MANY`, `MAX_MANY_BYTES` | `8` and `32 * 1024 * 1024`. |

Rules:

- only `http://` and `https://` URLs of at most 2,048 bytes, printable ASCII, no user information (`user@`), on the
  scheme's default port (80 or 443; no other port);
- the host must equal a declared `http:<host>` exactly, ignoring case: `http:example.com` does not cover
  `www.example.com` or `api.example.com`; declare each one;
- with `http:ask`, a request to a host that is not declared asks the user first ("<plugin> wants to fetch from
  <host>. Allow?"). Only public host names are asked about: an IP address (in any form), `localhost` or a name ending
  in `.localhost`, a single-label name and a name ending in `.local`, `.lan`, `.internal` or `.home.arpa` are refused
  without asking, and so is a host whose addresses are all on this computer or the local network. **Yes** is remembered
  for the plugin (at most 64 hosts; the user can **Forget** them on the Plugins page); **No** is remembered until the
  Manager closes. See [Asking for hosts](permissions.md#asking-for-hosts-httpask);
- a redirect is followed only to the same host, on its default port, and never from https to http; at most 5
  redirects;
- a status other than 2xx is an error;
- the response body has at most 32 MiB, and the whole request has 60 s;
- `get_many` follows every rule above for each URL. Under `http:ask` it first asks about the hosts not answered yet,
  one question at a time and in order (each one of the call's 20 dialogs; a host is asked about once), and only then
  fetches the allowed URLs, all together, each with its own 60 s. When a body does not fit in what is left of the
  call's 32 MiB, that URL and every one not finished yet fail with `permission denied: response budget exceeded`.
  More than 8 URLs: every result is `permission denied: at most 8 URLs per call` and nothing is fetched.

| Kind | When |
|---|---|
| `Denied` | `permission denied: host 'example.com' is not declared with http:example.com`; with `http:ask`: `permission denied: host <host> was refused` (the user said no, now or earlier in the session), `permission denied: host <host> was not asked (too many dialogs in one call)`, `permission denied: host <host> cannot be asked for` (not a public host name), `permission denied: host <host> resolves to a local address`, `permission denied: at most 64 hosts can be allowed; forget one on the Plugins page`; `permission denied: '<url>' is not an allowed http(s) URL`; `permission denied: only http and https URLs are allowed, not '<url>'`; `permission denied: a URL with user information is not allowed: '<url>'`; `permission denied: '<url>': only the default port of https is allowed`; `get_many`: `permission denied: response budget exceeded`, `permission denied: at most 8 URLs per call`; a redirect to another host (the message ends with `a redirect to another host was refused`). |
| `Timeout` | `the request timed out after 60 s` |
| `Other` | `HTTP <code>` (for example `HTTP 404`); `the response is larger than 32 MiB`; too many redirects; a network error. |

```rust
use aio_plugin_sdk::host::{self, Level};
use aio_plugin_sdk::http;

// manifest: "http:api.example.com"
fn http_call() -> aio_plugin_sdk::Result<()> {
    let body = http::get("https://api.example.com/v1/status")?;
    let text = String::from_utf8_lossy(&body);
    host::log(Level::Info, &text);
    // Several at once: one result per URL, in order.
    let urls = ["https://api.example.com/v1/a.png", "https://api.example.com/v1/b.png"];
    for (url, result) in urls.iter().zip(http::get_many(&urls)) {
        match result {
            Ok(body) => host::log(Level::Info, &format!("{url}: {} bytes", body.len())),
            Err(e) => host::log(Level::Warn, &format!("{url}: {e}")),
        }
    }
    Ok(())
}
```

## `tcp`

Connections to a program on the user's computer (for example G-Earth). Permission: `tcp:localhost:<port>` for each
port, or with API 1.1 `tcp:localhost:<a>-<b>` for a range of at most 16 ports.

| Item | What it does |
|---|---|
| `connect(port: u16) -> Result<Conn>` | Connects to `127.0.0.1:<port>`. |
| `struct Conn` | An open connection. Closed by `close` or when dropped. |
| `Conn::send(&mut self, data: &[u8]) -> Result<()>` | Sends all of `data`. |
| `Conn::recv(&mut self, max: usize) -> Result<Vec<u8>>` | Waits for data and returns what is available, at most `max` bytes (and at most `MAX_RECV`). An empty result means the other side closed the connection. `max` must be at least 1. |
| `Conn::recv_timeout(&mut self, max: usize, timeout: Duration) -> Result<Option<Vec<u8>>>` | **API 1.1.** Like `recv`, but waits at most `timeout` (`std::time::Duration`; at most `MAX_RECV_TIMEOUT`, a longer one waits that long; a part of a millisecond counts as a whole one). `Ok(None)`: nothing arrived in time, the connection stays usable. `Ok(Some(empty))`: the other side closed the connection. A zero `timeout` takes only what has already arrived. |
| `Conn::close(self)` | Closes the connection (the same as dropping it). |
| `MAX_RECV` | `1024 * 1024`: most bytes one `recv` or `recv_timeout` returns (1 MiB). |
| `MAX_RECV_TIMEOUT` | `Duration::from_secs(30)`: the longest wait of one `recv_timeout`. |

Rules: only `127.0.0.1`; only a declared port (alone or in a range); at most 4 connections open at once. Connecting
has 5 s; each `recv` has 30 s; `recv_timeout` waits as asked, at most 30 s. None of these waits counts against the
call's 5 s. Every connection is closed when the plugin stops.

In a [long job](ui.md#long-jobs), `recv_timeout` lets each tick wait a little and then return, so the page stays
usable while a program is quiet.

| Kind | When |
|---|---|
| `Denied` | `permission denied: port 3000 is not declared with tcp:localhost:3000`; `permission denied: at most 4 local connections may be open at once` |
| `Timeout` | `connecting timed out after 5 s`; `recv timed out after 30 s` |
| `Other` | `connecting to port <port> failed (<reason>)`, for example `(ConnectionRefused)` when nothing listens; `the connection is closed`; `max must be at least 1` (`recv(0)`, `recv_timeout(0, ...)`); other I/O errors. |

```rust
use aio_plugin_sdk::host::{self, Level};
use aio_plugin_sdk::tcp;

// manifest: "tcp:localhost:9092"
fn tcp_call() -> aio_plugin_sdk::Result<()> {
    let mut conn = tcp::connect(9092)?;
    conn.send(b"PING\n")?;
    let reply = conn.recv(4096)?;
    if reply.is_empty() {
        host::log(Level::Warn, "the program closed the connection");
    }
    conn.close();
    Ok(())
}

// manifest: "tcp:localhost:9092-9101" (API 1.1)
fn tcp_poll(conn: &mut tcp::Conn) -> aio_plugin_sdk::Result<bool> {
    // Wait at most 200 ms, then give the page back.
    match conn.recv_timeout(4096, std::time::Duration::from_millis(200))? {
        None => Ok(true),                           // nothing yet: try again in the next tick
        Some(bytes) if bytes.is_empty() => Ok(false), // closed
        Some(bytes) => {
            host::log(Level::Debug, &format!("{} bytes", bytes.len()));
            Ok(true)
        }
    }
}
```

## `script`

Shell scripts on the server the Manager is logged in to, over its SSH login. Permission: `ssh` for `run`, `ssh:root`
for `run_as_root`.

| Item | What it does |
|---|---|
| `run(script: &str) -> Result<ScriptResult>` | Runs `script` as the logged-in user, after the user approves it. |
| `run_as_root(script: &str) -> Result<ScriptResult>` | Runs `script` with root rights, after the user approves it. |
| `struct ScriptResult` | `exit_code: i32`, `output: String` (stdout), `error: String` (stderr). `#[non_exhaustive]`: read its fields, you cannot build one. |
| `ScriptResult::success(&self) -> bool` | `true` when `exit_code` is 0. |
| `MAX_SCRIPT` | `64 * 1024`: the largest script, in bytes (64 KiB). |

Before a script runs, the plugin's page shows an approval card with the whole script, the server, and the user it runs
as; nothing runs until the user clicks **Approve**. Rules: 1 byte to 64 KiB; Unix line endings; no control character
other than tab and newline (a carriage return is refused), and no invisible or text-direction characters, so the user
sees exactly what runs. The user's time to decide does not count; the approved script then has 10 minutes (600 s).

| Kind | When |
|---|---|
| `Denied` | `permission denied: running a script needs the 'ssh' permission` (or `'ssh:root'`); `permission denied: an empty script`; `permission denied: the script is larger than 64 KiB`; `permission denied: the script holds control or invisible characters (only tab and newline are allowed), so it could not be shown exactly as it would run`. The SDK refuses an empty or too large script and control characters itself, before asking the Manager. |
| `Rejected` | `the user rejected the script` |
| `Timeout` | `the script timed out after 600 s` |
| `Other` | `Not connected`; `The server changed since the request`; an SSH error. A script that runs but fails is not an error: check `success()` and `exit_code`. |

```rust
use aio_plugin_sdk::host::{self, Level};
use aio_plugin_sdk::script;

// manifest: "ssh", "ssh:root"
fn script_call() -> aio_plugin_sdk::Result<()> {
    let result = script::run("df -h /\n")?;
    if result.success() {
        host::log(Level::Info, &result.output);
    } else {
        host::log(Level::Error, &format!("exit {}: {}", result.exit_code, result.error));
    }
    let root = script::run_as_root("systemctl restart emulator\n")?;
    let _ = root;
    Ok(())
}
```
