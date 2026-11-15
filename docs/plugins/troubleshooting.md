# Troubleshooting

Common errors, where they show, and what they mean. The plugin's page in the Manager has a **Plugin log** section:
look there first. It holds what the plugin logged, its stdout and stderr, and a `permission denied: ...` warning for
every refused call.

## Building

**`error: the wasm32-wasip2 target is not installed. Install it with:`** followed by `rustup target add wasm32-wasip2`
(from `aio-plugin build` or `pack`). Run that command once. If you build with `cargo` directly, the same problem shows
as an error about `core` or `std` not being found for that target.

**`error: … is not a valid plugin package: plugin.wasm is not a WebAssembly component (build it for wasm32-wasip2,
crate-type cdylib)`** (from `aio-plugin check`). The wasm is a core module: it was built for `wasm32-wasip1` or
`wasm32-unknown-unknown`. Build with `aio-plugin build`, or `cargo build --release --target wasm32-wasip2`, and keep
`crate-type = ["cdylib"]` in `Cargo.toml`.

**`error: the build did not produce … (is crate-type cdylib?)`**. The build ran but wrote no `.wasm`: add
`crate-type = ["cdylib"]` under `[lib]`.

**`error: <folder> needs a Cargo.toml and a manifest.json (is it a plugin folder?)`**. `build`, `pack` and `dev` work
on the current folder: `cd` into the plugin project first.

**`error: --locked needs a Cargo.lock next to the workspace's Cargo.toml: … is missing`**. Run a build without
`--locked` once to create `Cargo.lock`, and commit it (the official index requires it).

**Cargo cannot fetch `aio-plugin-sdk` from git (tag `sdk-v1.0.0` not found)**. The SDK's tag is created when the SDK
is released upstream. Until then, make the project with `--sdk-path <checkout>/source/plugins/sdk`, or replace the git
line in `Cargo.toml` with `aio-plugin-sdk = { path = "<checkout>/source/plugins/sdk" }`.

**`error: cannot run cargo (is Rust installed?)`**. `cargo` is not on the `PATH`.

**A host call panics in `cargo test`.** Natively, only `host::log` and `host::status` work (they print to stderr).
Every other host call exists only inside the Manager. Test your logic and your pages with `Ui::new()` and
`Ui::finish()`, and build the plugin's state directly in tests (see [Getting started](getting-started.md#5-test)).

## A refused manifest or package

`aio-plugin check` prints these as `error: <file> is not a valid plugin package: <message>`; the Manager shows
"This file cannot be installed: <message>" (install from file) or "This folder cannot be loaded: <message>"
(developer folder).

| Message | Meaning |
|---|---|
| `manifest.json is not valid: …` | Not valid JSON, or a required field is missing (`id`, `name`, `version`, `author`, `description`, `api`, `page_title`) or of the wrong type. |
| `Invalid plugin id: <id>` | 2 to 40 characters: lowercase letters, digits and `-`, starting with a letter or digit. |
| `The id <id> belongs to a built-in plugin` | Choose another id (`cms`, `gitea`, `sql-editor`, ... are taken). |
| `Invalid version (expected MAJOR.MINOR.PATCH): <v>` | For example `1.0.0`, not `1.0` or `v1.0.0`. No leading zeros. |
| `Unsupported plugin API: <api>` | Use `"api": "1.0"`. A higher minor version (`1.1`) needs a newer Manager. |
| `name and page_title must not be empty` | Fill them in. |
| `A manifest text is too long` | A text (name, author, description, page title, tile texts) has more than 500 characters. |
| `A manifest text holds control or invisible characters` | Remove control characters (tab and newline are allowed) and invisible or text-direction characters. |
| `A manifest may list at most 64 permissions` | Ask for fewer. |
| `Unknown permission: <p>` | Not one of the [permission strings](permissions.md). Check the spelling (`sql:read:`, not `sql-read:`). |
| `Invalid table list in permission: <list>` | Table names are `A-Z a-z 0-9 _`, separated by commas without spaces. No `*`. |
| `Invalid permission: <p>` | A `sql:read-private:` permission that is not `<table>.<column>`. |
| `Invalid host in permission: <p>` | Only the host name: `http:example.com`, not `http:https://example.com/` or `http:example.com:8080`. |
| `Invalid port in permission: <p>` | A number from 1 to 65535. |
| `A permission is listed twice: <p>` | The same permission, the same table for the same access, or the same host (any case) appears twice. |
| `The package is larger than 32 MiB`, `<name> is too large` | See [Limits](limits.md#packages-and-manifests). |
| `The package is not a zip`, `The package has too many files` | Not a `.aiop` made by `aio-plugin pack`, or more than 64 files. |
| `Unexpected file in the package: <name>` | A package holds only `manifest.json`, `plugin.wasm`, `icon.png` and `i18n/<lang>.json`. |
| `Unsafe path in the package: <name>`, `Unexpected entry in the package: <name>`, `Duplicate file in the package: <name>` | The zip was not made by `aio-plugin pack`: a folder entry, a link, `..`, or the same file twice. |
| `manifest.json is missing`, `plugin.wasm is missing`, `plugin.wasm is empty` | Pack with `aio-plugin pack`. |

## Loading a developer folder

The Manager shows "This folder cannot be loaded: <message>" or, on the folder's card, the problem:

| Message | Meaning |
|---|---|
| `Developer mode is off` | Switch it on in Settings. |
| `The folder has no readable manifest.json` | Pick the project folder, the one with `manifest.json`. |
| `The plugin file <path> was not found` | The file named by `dev_wasm` does not exist: run `aio-plugin build` first. |
| `dev_wasm must be a path inside the folder (no absolute path, no ..): <path>` | Use a relative path such as `target/wasm32-wasip2/release/my_plugin.wasm`. |
| `dev_wasm must be a file inside the folder: <path>` | The path leads out of the folder (for example through a link), or is not a file. A shared Cargo target folder outside the project does not work: build inside the project. |
| `The plugin file is larger than 24 MiB` | Shrink the wasm: keep `opt-level = "s"`, `lto = true` and `strip = true` in the release profile. |
| `The plugin id <id> belongs to a built-in plugin` | Choose another id. |
| `id already installed` | An installed plugin has the same id. Remove it, or use another id while developing. |
| `id already used by another developer folder` | Two loaded folders have the same id. |

## The page shows a single red banner

`render` built a page the Manager would refuse, so the SDK shows the reason instead (and logs `the page was refused:
<reason>`). See [The page](ui.md#rules-and-limits).

| Message | Fix |
|---|---|
| `the page has more than 2000 elements` | Show fewer elements; the page itself counts as one. Page through long lists. |
| `the page is nested too deep (more than 32 levels)` | Fewer nested cards, rows and columns. |
| `the page has more than 20000 table cells and select options` | Show fewer rows, or page through them. |
| `the page has more than 5000 lines of text and log` | Shorten texts and logs; keep the last lines only. |
| `the id "<id>" is used by two controls; ids must be unique within a page` | Give each control its own id, for example `format!("delete-{index}")`. |
| `a control has an empty id` | Give the control an id. |
| `the select "<id>" has no option <n> (it has <m> options)` | `selected` must be an index into the options. |

## The plugin stopped

The page shows "The plugin stopped: <reason>" and a **Reload** button, which starts a fresh instance (`new()` runs
again; only the private files remain).

| Reason | Meaning |
|---|---|
| `the plugin crashed: …` | The plugin panicked, aborted or ran out of memory (64 MiB). The panic message is in the plugin log (stderr). |
| `the plugin took too long and was stopped` | One call ran more than 5 s of plugin code. Waiting for the user or for the database, network or a script does not count; a busy loop or a heavy computation does. Split the work over several events. |
| `you stopped it` | The user chose **Stop this plugin** (or **Reject and stop this plugin**). |

## "The plugin reported an error"

This banner above the page shows the `Err(text)` your `on_event` returned, for example an SDK error passed on with `?`.
The plugin is still running. Turn errors you expect into a friendlier banner of your own instead, as the `notes`
example does.

## Errors from host calls

Every SDK call returns an [`Error`](api.md#errors) with a kind and a message.

### Permission denied

Kind `Denied`. The call is outside the plugin's permissions, and the plugin log has the same line as a warning. The
rest of the message says what is missing:

| Message | Fix |
|---|---|
| `permission denied: table 'users' is not declared with sql:read` | Add `sql:read:users` (or `sql:write:...` for a change). |
| `... (write the table name exactly as declared)` | The table is declared with another case: names are case sensitive. |
| `permission denied: column 'users.mail' is private (needs sql:read-private:users.mail)` | A [private column](permissions.md#private-columns): drop it from the query, or declare that permission. |
| `permission denied: list the columns to read (there is no 'every column')` | Add `.columns([...])` to the `Select`. |
| `permission denied: invalid table name '...'`, `invalid column name '...'` | Names are a letter or `_`, then letters, digits or `_`, at most 64. |
| `permission denied: an update of '<t>' needs at least one condition`, `a delete from '<t>' needs at least one condition` | Add a `filter`. |
| `permission denied: an empty write batch`, `more than 1000 operations in one batch`, `the changes are too large to show (more than 8 MiB of values in one batch)` | Split the batch. |
| `permission denied: host 'example.com' is not declared with http:example.com` | Add `http:example.com`. Each subdomain is a separate host. |
| `permission denied: '<url>': only the default port of https is allowed` | No custom port in URLs. |
| `permission denied: port 3000 is not declared with tcp:localhost:3000` | Add `tcp:localhost:3000`. |
| `permission denied: at most 4 local connections may be open at once` | Close or drop connections you no longer use. |
| `permission denied: running a script needs the 'ssh' permission` | Add `ssh` (or `ssh:root` for `run_as_root`). |
| `permission denied: the script holds control or invisible characters …` | Use `\n` line endings, no `\r`, no escape sequences. |
| `access denied: outside the private folder` | A path in the private folder leads out of it through a link. |

Other `Denied` texts: `access denied` (the system refused a file), `a file picked for saving cannot be read`,
`a file picked for opening cannot be written`, and a request error ending in `a redirect to another host was refused`.

### Rejected, Timeout, NotFound

- `the user rejected the script`, `the user rejected the database changes`: kind `Rejected`. The user clicked
  **Reject**. Not a bug: tell the user nothing was changed.
- `<what> timed out after <n> s` (for example `the query timed out after 30 s`): kind `Timeout`. See
  [Limits](limits.md#time).
- `file not found`: kind `NotFound`. Use `files::read_if_exists` or `read_to_string_if_exists` for files that may not
  exist yet.

### "Not connected"

Kind `Other`. The Manager is not logged in to a server, so database reads and changes and scripts cannot run. The user
logs in (the **Log in** button at the top right); your plugin should say so instead of failing:

```rust
use aio_plugin_sdk::sql::Select;

fn users_or_message() -> Result<Vec<String>, String> {
    match Select::from("users").columns(["username"]).limit(10).run() {
        Ok(rows) => rows.iter().map(|r| r.get::<String>("username").map_err(String::from)).collect(),
        Err(e) if e.message() == "Not connected" => Err("Log in to the server first.".into()),
        Err(e) => Err(e.into()),
    }
}
```

Related `Other` messages:

- `No saved MariaDB account for <host> on this computer: log in once in the SQL Editor or the Catalogue and save it`:
  the Manager is logged in, but has no database account saved for that server.
- `Dry run: there is no database`: the Manager runs in dry-run mode.
- `The server changed since the request`: the user logged in to another server while an approval card was open;
  nothing ran.
- `HTTP <code>`: the web server answered with a status other than 2xx.

## Other surprises

**An input jumps back to its old text.** Store the `value` of `Changed` and render it back. See
[Inputs report their text when they lose the focus](ui.md#inputs-report-their-text-when-they-lose-the-focus).

**`ask` returns `false` and the dialogs return `None` at once.** The call already opened 20 dialogs, or the user chose
**Stop this plugin**. The log says "more than 20 dialogs in one call: further dialogs are cancelled without being
shown" in the first case.

**The log ends with "log truncated by the host".** One call wrote more than 1,000 entries or 256 KiB. Log less per
call.

**"<plugin> is busy: wait until it has finished."** More than 32 events are waiting: the plugin is slow to handle
them. Make `on_event` and `render` faster.

**The plugin does not appear in the sidebar.** It is switched off on the Plugins page, its permissions grew and wait
for review, or (developer folder) developer mode was switched off or the Manager restarted and the permissions must be
reviewed again.
