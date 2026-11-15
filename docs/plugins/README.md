# Writing plugins for All-in-1 Manager

An external plugin is a small program that runs inside All-in-1 Manager, in a sandbox. You write it in Rust with the
SDK crate `aio-plugin-sdk`, build it as a WebAssembly component (`wasm32-wasip2`), and pack it as a `.aiop` file.

A plugin never draws anything itself. The Manager asks it to describe its page (headings, text, buttons, inputs,
tables, ...), draws that page with its own widgets, and tells the plugin what the user did. To reach anything outside
the sandbox (its own files, the hotel database, the server, the internet, a local program), the plugin calls the
Manager, which checks every call against the permissions the plugin declared and the user accepted.

## The guide

| Page | What it covers |
|---|---|
| [Getting started](getting-started.md) | Install Rust and the target, create a plugin with `aio-plugin new`, build, test, pack, check, and try it in the Manager. |
| [The page (UI)](ui.md) | The `Ui` builder, events, how inputs report their text, and the page limits. |
| [API reference](api.md) | Every public item of the SDK, with the permission it needs, its limits and its error kinds. |
| [Permissions](permissions.md) | Every permission string, what the user reads, private database columns, and approvals. |
| [Limits](limits.md) | Memory, time, logs, files, SQL, HTTP, TCP, scripts, the page, and packages, in one place. |
| [Publishing](publishing.md) | Getting a plugin into the official index, updates, revocation. |
| [Troubleshooting](troubleshooting.md) | Common errors and what they mean. |

## Licence

The SDK (`source/plugins/sdk`) is licensed under **MIT OR Apache-2.0**, at your option: it is compiled into your plugin,
so you can license your plugin as you like. The rest of All-in-1 Manager stays GPL-3.0-only.

## What you need

- Rust 1.97 or newer (1.88 for a plugin alone) with the `wasm32-wasip2` target.
- A clone of this repository, for the `aio-plugin` tool (crate `aio-plugin-cli`) and, until the SDK is tagged, the SDK itself.
- All-in-1 Manager, to try the plugin.

## The pieces

- `source/plugins/sdk`: the SDK, `aio-plugin-sdk` 1.0.0. It supports exactly the plugin API `aio:plugin@1.0.0`
  (`source/wit/aio-plugin/plugin.wit`). A plugin depends only on this crate.
- `source/crates/plugin_cli`: the `aio-plugin` tool (`new`, `build`, `pack`, `check`, `dev`).
- `source/plugins/hello` and `source/plugins/notes`: two example plugins. `notes` is commented line by line and shows
  private files, dialogs, a database read and how to fail gracefully.
