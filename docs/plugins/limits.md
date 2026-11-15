# Limits

Every limit the Manager puts on a plugin, in one place. "Per call" means per call into the plugin: `new` with the
first `render`, one `on_event`, one `render`, or one `tick` of a [long job](ui.md#long-jobs) (API 1.1).

## Memory

| What | Limit |
|---|---|
| Linear memory, over all the plugin's memories | 64 MiB |
| WebAssembly table elements, over all its tables | 100,000 |

An allocation past the limit fails inside the plugin (Rust aborts), and the plugin stops: its page says
"The plugin stopped: the plugin crashed: ..." with **Reload**. Large files and responses count too: reading a 32 MiB
file needs 32 MiB of the 64.

## Time

| What | Limit |
|---|---|
| Plugin code per call (instantiation included) | 5 s (each `tick` has its own 5 s) |
| A long job (`tick` after `tick`) | No limit while the page is open; it ends when the page closes |
| Waiting for the user (a dialog, an approval) | No limit, and not counted in the 5 s |
| A database read (`Select::run`) | 30 s, not counted in the 5 s |
| An approved database batch (`sql::write`) | 30 s once approved |
| An HTTP request (`http::get`), redirects and body included | 60 s, not counted in the 5 s |
| Connecting to a local port (`tcp::connect`) | 5 s, not counted in the 5 s |
| One `Conn::recv` | 30 s, not counted in the 5 s |
| One `Conn::recv_timeout` (API 1.1) | What you ask, at most 30 s (30,000 ms; a longer wait is cut to 30 s); not counted in the 5 s, and not an error when it runs out (`Ok(None)`) |
| An approved script (`script::run`, `run_as_root`) | 600 s (10 minutes) once approved |

Past the 5 s, the call is stopped and the plugin with it ("The plugin stopped: the plugin took too long and was
stopped"); **Reload** starts a fresh instance. Past one of the other limits, only that call fails, with
`ErrorKind::Timeout` and the message `<what> timed out after <n> s`, and the plugin keeps running.

Events wait in a queue of at most 32 while the plugin is busy; past that, clicks are dropped and the status line says
"<plugin> is busy: wait until it has finished."

Removing or updating a plugin that is still running waits up to 10 s for it to stop, then fails with "The plugin is
still running: close its page and try again."

## Log and dialogs

| What | Limit |
|---|---|
| One log entry or status text | 4 KiB (the rest is cut) |
| Log entries per call (status texts included) | 1,000 |
| Log bytes per call | 256 KiB |
| Lines kept in the page's **Plugin log** | 2,000 |
| Dialogs (`ask`, `prompt_text`, file pickers, the folder picker, `http:ask` questions) per call | 20 |
| Text of `ask` and `prompt_text` (title, label, default) | 1,000 characters shown |
| Title of a file dialog | 120 characters shown |

Past the per-call log budget, one entry "log truncated by the host" is written and the rest of that call's log is
dropped. Past 20 dialogs, every further dialog of that call gets the cancel answer without being shown, and the log
says "more than 20 dialogs in one call: further dialogs are cancelled without being shown". stdout and stderr go to the
log too (one entry per line) and count the same way. Control, direction-changing and invisible characters in log lines
are replaced.

## Files

| What | Limit |
|---|---|
| Path | 255 bytes, 8 levels, names of `A-Z a-z 0-9 . _ -` only |
| One private file (read or write) | 32 MiB |
| The private folder in all | 64 MiB |
| Files and folders in the private folder | 10,000 |
| A picked file read (`PickedFile::read`) or handed to `open_import_preview` | 32 MiB |
| Files saved by `open_import_preview` (`configuration/plugins/ext-exports/<id>/`) | 100 files and 256 MiB per plugin |

When the exports folder is full, the status line says so and the file is not saved.

## Picked folders (API 1.1)

| What | Limit |
|---|---|
| Path | As private files: 255 bytes, 8 levels, names of `A-Z a-z 0-9 . _ -` only, no `..`, no drive, no device name |
| File types | The last extension one of `json`, `sql`, `txt`, `md`, `csv`, `tsv`, `xml`, `html`, `png`, `gif`, `jpg`, `jpeg`, `webp`, `bmp`, `svg`, `zip`, `nitro`, `swf`, `bin`, `log` (any letter case); no name starting with `.` |
| Files written through one `Folder` | 100,000 (`folders::MAX_FILES`) |
| Bytes written through one `Folder`, in all | 2 GiB, the current sizes of its files (`folders::MAX_BYTES`) |
| One file | 256 MiB (`folders::MAX_FILE`) |
| A file handed to `Folder::open_import_preview` | 32 MiB |
| `Folder::reveal` | Once every 2 s per `Folder` |

Past a size or count limit, the write fails with `permission denied: folder limit reached`.

## SQL

| What | Limit |
|---|---|
| Rows of a read without `limit` | 1,000 |
| Rows of a read, at most | 10,000 (a larger `limit` is lowered) |
| Columns of a read, or set by one insert or update | 256 |
| Conditions of one statement | 64 |
| Table and column names | 64 characters: a letter or `_`, then letters, digits or `_` |
| Operations in one `sql::write` batch | 1 to 1,000 |
| Values in one batch | 8 MiB (text and bytes by length, 8 bytes per number) |

## HTTP

| What | Limit |
|---|---|
| URL | 2,048 bytes, printable ASCII, `http` or `https`, default port only, no user information |
| Redirects | 5, to the same host only, on its default port, never from https to http |
| Response body | 32 MiB |
| Status | 2xx; anything else is `HTTP <code>` |
| Hosts allowed under `http:ask` (API 1.1) | 64 per plugin, remembered |
| Hosts refused under `http:ask` (API 1.1) | Remembered for the session; after 256, every other host is refused without asking |

## Local TCP

| What | Limit |
|---|---|
| Address | `127.0.0.1` only, declared ports only (1 to 65535) |
| A port range `tcp:localhost:<a>-<b>` (API 1.1) | At most 16 ports |
| Connections open at once | 4 |
| Bytes returned by one `recv` or `recv_timeout` | 1 MiB (`tcp::MAX_RECV`) |
| Wait of one `recv_timeout` | 30 s (`tcp::MAX_RECV_TIMEOUT`) |

## Scripts

| What | Limit |
|---|---|
| Script size | 1 byte to 64 KiB (`script::MAX_SCRIPT`) |
| Characters | No control character other than tab and newline (no carriage return), no invisible or text-direction character |

## The page

| What | Limit |
|---|---|
| Elements | 2,000, the page's own implicit column included (so 1,999 of yours) |
| Nesting | 32 levels, the page being level 0 (a leaf sits in at most 31 containers) |
| Table cells (headers included) plus select options | 20,000 over the page |
| Text lines plus log entries | 5,000 over the page |
| Characters in an input | 4,000 |

The Manager also cuts long page text for display, without an error: headings at 200 characters, button labels at
80, table headers at 80 and cells at 200, banners at 500, each text line at 2,000, log entries at 1,000, card titles,
input labels and select options at 120, checkbox labels at 200.

These four page limits are `aio_plugin_sdk::ui::limits::{MAX_ELEMENTS, MAX_DEPTH, MAX_CELLS, MAX_LINES}`. See
[The page](ui.md#rules-and-limits).

## Packages and manifests

| What | Limit |
|---|---|
| A `.aiop` package | 32 MiB |
| Files in a package | 64; only `manifest.json`, `plugin.wasm`, `icon.png` and `i18n/<lang>.json` |
| `plugin.wasm` | 24 MiB, not empty (also the limit for a developer folder's wasm) |
| `manifest.json` | 256 KiB |
| `icon.png` | 2 MiB |
| One `i18n/<lang>.json` | 512 KiB (`<lang>`: 1 to 16 characters `A-Z a-z 0-9 - _`) |
| Plugin id | 2 to 40 characters: lowercase letters, digits and `-`, starting with a letter or digit; not a Windows device name (`con`, `prn`, `aux`, `nul`, `com1` to `com9`, `lpt1` to `lpt9`) and not a built-in plugin's id |
| `version` | `MAJOR.MINOR.PATCH`, each part at most 6 digits, no leading zeros |
| `api` | `1.0` or `1.1` (this Manager implements API 1.1); at least the API the component imports |
| Each manifest text | 500 characters, no control or invisible characters (tab and newline allowed) |
| Permissions | 64, each one once |
