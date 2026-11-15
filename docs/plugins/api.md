# API reference

Every public item of `aio-plugin-sdk` 1.0.0, module by module, with the permission it needs, its limits and the error
kinds it returns. The SDK's own documentation (`cargo doc --open` in your plugin project) has the same items with
more examples.

Every Rust sample of this guide is built for `wasm32-wasip2` by the SDK's test `tests/guide_samples.rs`, so they
stay in step with the SDK. They need the permissions named next to them in the plugin's `manifest.json`.

Contents: [the crate root](#the-crate-root) · [errors](#errors) · [`ui`](#ui) · [`host`](#host) · [`files`](#files)
· [`sql`](#sql) · [`http`](#http) · [`tcp`](#tcp) · [`script`](#script)

## The crate root

```rust
pub trait Plugin: 'static {
    fn new() -> Self where Self: Sized;
    fn render(&mut self, ui: &mut Ui);
    fn on_event(&mut self, event: Event) -> std::result::Result<(), String>;
}
```

| Item | What it does |
|---|---|
| `Plugin::new()` | The starting state. Called once, the first time the Manager needs the plugin (when its page opens), before the first `render`. Host calls are allowed here. |
| `Plugin::render(&mut self, ui)` | Describes the page. Called when the page opens and after every event. See [The page](ui.md). |
| `Plugin::on_event(&mut self, event)` | Handles one user action. `Err(text)` is shown to the user ("The plugin reported an error") and logged; the plugin keeps running and the page is rendered again. |
| `export_plugin!(Type)` | Exports `Type` (which implements `Plugin`) as the component's plugin. Use it once, at the top level of the crate. On targets other than `wasm32` it expands to nothing, so `cargo test` works natively. |
| `enum Event` | `Clicked { id: String }` or `Changed { id: String, value: String }`. `#[non_exhaustive]`: match it with a `_` arm. |
| `Event::id(&self) -> &str` | The id of the control the event came from. |
| `Ui` | The page builder (re-export of `ui::Ui`). |
| `Error`, `ErrorKind`, `Result<T, E = Error>` | See [Errors](#errors). |

The Manager keeps one instance per open page and calls it from one thread. The instance lives while the page is open;
closing the page, switching the plugin off or removing it drops it. A plugin never runs in the background.

Each call into the plugin (`new` plus the first `render`, each `on_event`, each `render`) may run for at most 5 s of
plugin time. Time spent in a dialog, waiting for an approval, or waiting on the database, the network or a local
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
| `Denied` | The plugin is not allowed to do this: a permission is missing from its manifest, or the request is outside what the permission covers (another table, host, port, a private column, a path that leads out of the private folder). Also a picked file used the wrong way, and an HTTP redirect to another host. The message starts with `permission denied: ` in most cases (see [Troubleshooting](troubleshooting.md#permission-denied)). |
| `Rejected` | The user was asked to approve a script or database changes and said no. |
| `Timeout` | The action passed its time limit. The message is `<what> timed out after <n> s`. |
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

The Manager itself: the plugin log, the status line, dialogs, and files the user picks. Permission: none.

| Item | What it does |
|---|---|
| `enum Level` | `Debug`, `Info`, `Warn`, `Error`. `#[non_exhaustive]`. |
| `log(level: Level, text: &str)` | Appends a line to the plugin log (on the plugin's page, **Plugin log**). |
| `status(text: &str)` | Sets the Manager's status line. Also logged. |
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

Before a file dialog opens, the Manager shows its own window saying what the plugin wants ("The plugin wants you to
choose a file to open. It can read that one file, nothing else."). Every dialog of a plugin also offers **Stop this
plugin**; the call then gets the cancel answer (`false` or `None`) and the plugin is stopped.

Limits: one call (`render` or `on_event`) may open at most 20 dialogs. Past that, every further dialog gets the cancel
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

Off `wasm32` (a native `cargo test`), `log` and `status` print to stderr; the other calls panic.

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

HTTP(S) GET to the hosts the plugin declares. Permission: `http:<host>` for each host.

| Item | What it does |
|---|---|
| `get(url: &str) -> Result<Vec<u8>>` | GETs `url` and returns the response body. |

Rules:

- only `http://` and `https://` URLs of at most 2,048 bytes, printable ASCII, no user information (`user@`), on the
  scheme's default port (80 or 443; no other port);
- the host must equal a declared `http:<host>` exactly, ignoring case: `http:example.com` does not cover
  `www.example.com` or `api.example.com`; declare each one;
- a redirect is followed only to the same host, on its default port, and never from https to http; at most 5
  redirects;
- a status other than 2xx is an error;
- the response body has at most 32 MiB, and the whole request has 60 s.

| Kind | When |
|---|---|
| `Denied` | `permission denied: host 'example.com' is not declared with http:example.com`; `permission denied: '<url>' is not an allowed http(s) URL`; `permission denied: only http and https URLs are allowed, not '<url>'`; `permission denied: a URL with user information is not allowed: '<url>'`; `permission denied: '<url>': only the default port of https is allowed`; a redirect to another host (the message ends with `a redirect to another host was refused`). |
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
    Ok(())
}
```

## `tcp`

Connections to a program on the user's computer (for example G-Earth). Permission: `tcp:localhost:<port>` for each
port.

| Item | What it does |
|---|---|
| `connect(port: u16) -> Result<Conn>` | Connects to `127.0.0.1:<port>`. |
| `struct Conn` | An open connection. Closed by `close` or when dropped. |
| `Conn::send(&mut self, data: &[u8]) -> Result<()>` | Sends all of `data`. |
| `Conn::recv(&mut self, max: usize) -> Result<Vec<u8>>` | Waits for data and returns what is available, at most `max` bytes (and at most `MAX_RECV`). An empty result means the other side closed the connection. `max` must be at least 1. |
| `Conn::close(self)` | Closes the connection (the same as dropping it). |
| `MAX_RECV` | `1024 * 1024`: most bytes one `recv` returns (1 MiB). |

Rules: only `127.0.0.1`; only a declared port; at most 4 connections open at once. Connecting has 5 s; each `recv`
has 30 s. Every connection is closed when the plugin stops.

| Kind | When |
|---|---|
| `Denied` | `permission denied: port 3000 is not declared with tcp:localhost:3000`; `permission denied: at most 4 local connections may be open at once` |
| `Timeout` | `connecting timed out after 5 s`; `recv timed out after 30 s` |
| `Other` | `connecting to port <port> failed (<reason>)`, for example `(ConnectionRefused)` when nothing listens; `the connection is closed`; `max must be at least 1` (`recv(0)`); other I/O errors. |

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
