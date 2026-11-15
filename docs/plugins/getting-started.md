# Getting started

This page takes you from nothing to a plugin running in All-in-1 Manager: install the tools, create a project, build
it, test it, pack it, check the package, and load it in the Manager.

Every command below was run on Windows 11 with Rust 1.99.1; the output is real, with long paths shortened to `…`.
On Linux the commands are the same.

## 1. Install Rust and the target

Install Rust from <https://rustup.rs>, then add the WebAssembly target plugins are built for:

```sh
rustup target add wasm32-wasip2
```

A plugin is a WebAssembly *component* for WASI preview 2. The older `wasm32-wasip1` and `wasm32-unknown-unknown`
targets give a core module, which the Manager cannot load.

## 2. Install the `aio-plugin` tool

The tool is the crate `aio-plugin-cli` in the All-in-1 Manager repository. From a checkout of the repository:

```sh
cargo install --path source/crates/plugin_cli --locked
```

This installs the `aio-plugin` command into Cargo's `bin` folder (on your `PATH` when Rust was installed with rustup).
`aio-plugin --help` lists the commands:

```text
aio-plugin: create, build, pack and check All-in-1 Manager plugins

usage:
  aio-plugin new <dir> --id <id> [--sdk-path <dir>]
      A new plugin project in <dir> (which must not exist or be empty). --sdk-path uses a local copy of
      aio-plugin-sdk instead of the git dependency.
  aio-plugin build [--locked] [--offline] [--features <f>]
      Builds the plugin in the current folder for wasm32-wasip2 (release).
  aio-plugin pack [--out <file>] [--locked] [--offline] [--features <f>]
      Builds, then packs dist/<id>.aiop (or --out). The same build gives the same bytes.
  aio-plugin check <file.aiop|dir>
      Checks a package (or a built plugin folder) as the Manager does and prints what it shows.
  aio-plugin dev
      Prints how to load the plugin in the current folder in the Manager's developer mode.
```

`build`, `pack` and `dev` work on the plugin in the current folder. Every error is printed as `error: ...` with a
non-zero exit code.

## 3. Create a project

```sh
aio-plugin new my-plugin --id my-plugin
```

```text
Created the plugin my-plugin in my-plugin
Next: cd my-plugin && aio-plugin build && aio-plugin dev
```

The id is the plugin's identity for the Manager: 2 to 40 characters, lowercase letters, digits and dashes, starting
with a letter or a digit. The ids of the Manager's built-in plugins (`cms`, `gitea`, `fail2ban`, `crowdsec`,
`default-catalog`, `sql-editor`, `habbo-assets`, `deploy-assets`, `asset-import`, `asset-explorer`) are refused. The
folder must not exist or must be empty.

The project holds:

| File | What it is |
|---|---|
| `Cargo.toml` | A `cdylib` crate with a small release profile and its own `[workspace]` table, so it builds on its own. |
| `manifest.json` | What the Manager reads: id, name, version, author, permissions, ... (see below). |
| `src/lib.rs` | A counter with one button. |
| `.gitignore` | Ignores `/target` and `/dist`. |
| `README.md` | The commands of this page, in short. |

The SDK dependency in `Cargo.toml` is a git dependency on its release tag:

```toml
[dependencies]
aio-plugin-sdk = { git = "https://github.com/duckietm/All-in-1-Manager", tag = "sdk-v1.0.0" }
```

The first build downloads the repository once; `Cargo.lock` then pins the exact commit.

> **Before the SDK is released.** The tag `sdk-v1.0.0` is created when the SDK is merged upstream. Until then, use a
> local checkout of the repository with `--sdk-path`, which writes a path dependency instead (the git line stays in
> `Cargo.toml` as a comment, ready to switch back):
>
> ```sh
> aio-plugin new my-plugin --id my-plugin --sdk-path <checkout>/source/plugins/sdk
> ```
>
> The output on this page was made that way.

### `src/lib.rs`

This is the file `aio-plugin new` writes (it builds as it is):

```rust
//! My Plugin: an All-in-1 Manager plugin written on `aio-plugin-sdk`.
//!
//! The plugin never draws anything itself: `render` describes the page with the `Ui` builder, `on_event`
//! receives what the user did (a click, an edited input) and the Manager calls `render` again afterwards.

use aio_plugin_sdk::{export_plugin, Event, Plugin, Ui};

struct Counter {
    /// How many times the button was clicked in this session.
    clicks: u64,
}

impl Plugin for Counter {
    fn new() -> Self {
        Counter { clicks: 0 }
    }

    fn render(&mut self, ui: &mut Ui) {
        ui.heading("My Plugin");
        ui.text(format!("Clicked {} times", self.clicks));
        ui.primary_button("count", "Click me");
    }

    fn on_event(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::Clicked { id } if id == "count" => self.clicks += 1,
            // Other events do not concern this plugin.
            _ => {}
        }
        Ok(())
    }
}

export_plugin!(Counter);
```

The model in four lines:

- The Manager makes one `Counter` with `new()` when the page opens, and keeps it while the page is open.
- `render` describes the page from the current state. It is called when the page opens and after every event.
- `on_event` gets what the user did and changes the state. An `Err(text)` is shown to the user and logged.
- `export_plugin!(Counter)` makes `Counter` the component's plugin. Use it once per crate.

Closing the page drops the instance: anything that must survive goes into the plugin's private files
([`files`](api.md#files)).

### `manifest.json`

```json
{
  "id": "my-plugin",
  "name": "My Plugin",
  "version": "0.1.0",
  "author": "Your name",
  "description": "What My Plugin does, in one sentence.",
  "api": "1.0",
  "permissions": ["files"],
  "page_title": "My Plugin",
  "tile": {
    "title": "My Plugin",
    "description": "What My Plugin does, in one sentence.",
    "color": "#4f8cff"
  },
  "dev_wasm": "target/wasm32-wasip2/release/my_plugin.wasm"
}
```

| Field | Rule |
|---|---|
| `id` | As above. It must not change between versions. |
| `name`, `page_title` | Required, not empty. `page_title` is the plugin's entry in the sidebar (under PLUGINS). |
| `version` | `MAJOR.MINOR.PATCH`, plain numbers without leading zeros (at most 6 digits each). |
| `author` | Who made it. An update must keep the same author (see [Publishing](publishing.md)). |
| `description` | One sentence. |
| `api` | `"1.0"`: the plugin API version the plugin needs. This Manager accepts `1.0`. |
| `permissions` | What the plugin may do: see [Permissions](permissions.md). At most 64, each one once. |
| `tile` | Optional: a tile on the Dashboard and the Addons page (`title`, `description`, `color` as `#rrggbb`). |
| `dev_wasm` | Optional, for developer mode only: where the built wasm is, relative to the folder. A package ignores it. |

Every text holds at most 500 characters and no control or invisible characters (tab and newline excepted). Change
`name`, `author`, `description` and `version` before you publish.

## 4. Build

```sh
cd my-plugin
aio-plugin build
```

```text
   …
   Compiling wit-bindgen v0.63.0
   …
   Compiling aio-plugin-sdk v1.0.0 (…\source\plugins\sdk)
   Compiling my-plugin v0.1.0 (…\my-plugin)
    Finished `release` profile [optimized] target(s) in 28.77s
…\my-plugin\target\wasm32-wasip2\release\my_plugin.wasm
```

`build` checks that the `wasm32-wasip2` target is installed, runs `cargo build --release --target wasm32-wasip2`,
and prints the path of the built file. The first build compiles the SDK's dependencies (about 30 s here); later
builds take a second.

Options: `--locked` (use exactly the versions in `Cargo.lock`; fails without one), `--offline` (never use the
network), `--features <f>` (build with Cargo features; such a build goes to `target/features-<f>/`).

You can also build with Cargo directly: `cargo build --release --target wasm32-wasip2`.

## 5. Test

`export_plugin!` expands to nothing outside WebAssembly, so `cargo test` runs your plugin's logic natively. Build a
page with `Ui::new()`, call `render`, and look at the finished page:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aio_plugin_sdk::ui::Element;

    #[test]
    fn a_click_is_counted() {
        let mut plugin = Counter::new();
        plugin.on_event(Event::Clicked { id: "count".into() }).unwrap();

        let mut ui = Ui::new();
        plugin.render(&mut ui);
        let page = ui.finish().expect("the page breaks no rule");
        assert!(page.elements().contains(&Element::Text("Clicked 1 times".into())));
    }
}
```

```sh
cargo test
```

```text
running 1 test
test tests::a_click_is_counted ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Run tests natively (plain `cargo test`, without `--target`). Natively, `host::log` and `host::status` print to
stderr; every other host call (files, database, dialogs, ...) panics, so keep those out of the code under test, or
put them behind your own function.

## 6. Pack

```sh
aio-plugin pack
```

```text
    Finished `release` profile [optimized] target(s) in 0.14s
…\my-plugin\dist\my-plugin.aiop
```

`pack` builds first (it takes the same options as `build`), then writes `dist/<id>.aiop`, or the file named with
`--out <file>`. A `.aiop` is a zip with `manifest.json`, `plugin.wasm` and, when they exist, `icon.png` and
`i18n/<lang>.json`. Packing is reproducible: the same build gives the same bytes (fixed entry order, time
1980-01-01 00:00:00, mode 0644, deflate level 9), identical to `source/tools/pack_plugin.py`.

`icon.png` and `i18n/*.json` are accepted in a package and kept, but this version of the Manager does not show them.

## 7. Check

```sh
aio-plugin check dist/my-plugin.aiop
```

```text
id:          my-plugin
name:        My Plugin
version:     0.1.0
author:      Your name
api:         1.0
permissions:
  - files: Keep its own files in a private folder (always allowed)
size:        25331 bytes
sha256:      54e16b3dcb117f372506ac6b465764348ee186a30b2b88556065584a2642e596
OK: the package is valid.
```

`check` reads the package the way the Manager does when it installs one, checks that `plugin.wasm` is a
WebAssembly component, and prints what the Manager's consent window will show: each permission in plain words, the
size and the SHA-256. `aio-plugin check .` checks a built plugin folder without writing a package (it packs in memory,
so it prints the same size and hash). A refused package exits with an error that names the problem, for example:

```text
error: . is not a valid plugin package: Invalid table list in permission: *
```

See [Troubleshooting](troubleshooting.md#a-refused-manifest-or-package) for the other messages.

## 8. Try it in the Manager

There are two ways. Developer mode is the one for day-to-day work.

### Developer mode (run from the folder)

```sh
aio-plugin dev
```

```text
Developer folder: …\my-plugin

1. In the Manager, open Settings and turn on Developer mode.
2. On the Plugins page, under "Developer plugins", click "Load a plugin folder" and pick the folder above.
3. After each `aio-plugin build`, reload the plugin from its card.

The Manager loads target/wasm32-wasip2/release/my_plugin.wasm (the manifest's dev_wasm) from that folder.
```

1. In the Manager, open **Settings** and switch **Developer mode** on.
2. Open the **Plugins** page. Under **Developer plugins**, click **Load a plugin folder …** and pick your project
   folder (the one with `manifest.json`).
3. The consent window **Load this plugin folder?** lists the permissions. Click **Load**.
4. The plugin's card appears with red **Developer** and **Not verified** badges, and its `page_title` appears in the
   sidebar under **PLUGINS**. Open it there (or from its tile).
5. After each `aio-plugin build`, click **Reload** on the card: the Manager reads `manifest.json` and the wasm file
   again. If the new manifest asks for more permissions, the plugin stays off until you review them again.

The Manager runs the file named by `dev_wasm` (or `plugin.wasm` next to the manifest when there is no `dev_wasm`).
It must be a relative path inside the folder. The private data of a developer plugin is kept in
`configuration/plugins/ext-dev/<id>/data/`, apart from installed plugins. A folder whose id is already installed shows
"id already installed" and does not run. Your consent is not remembered across restarts: after a restart, you review
the permissions again.

The plugin page has a **Plugin log** section (click to open) with what the plugin logged, its stdout and stderr, and
every refused call.

### Install from file (the packed `.aiop`)

1. On the **Plugins** page, **Installed** tab, click **Install from file …** and pick `dist/my-plugin.aiop`.
2. The window **Install this plugin?** shows the name, author, version and permissions, with the red line
   "Not verified — installed from a file". Click **Install**.
3. The plugin is installed in `configuration/plugins/ext/<id>/` and its private data in
   `configuration/plugins/ext/<id>/data/`.

This is how your users will install a plugin that is not in the official index, so try it once before you publish.

## Next

- [The page (UI)](ui.md): what a page can hold and how events work.
- [API reference](api.md): files, database, HTTP, local connections, scripts, dialogs.
- `source/plugins/notes` in the repository: a commented example with files, dialogs and a database read.
