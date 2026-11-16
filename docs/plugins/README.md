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
| [The page (UI)](ui.md) | The `Ui` builder, events, how inputs report their text, long jobs in steps, and the page limits. |
| [API reference](api.md) | Every public item of the SDK, with the permission it needs, its limits and its error kinds. |
| [Permissions](permissions.md) | Every permission string, what the user reads, private database columns, and approvals. |
| [Limits](limits.md) | Memory, time, logs, files, picked folders, SQL, HTTP, TCP, scripts, the page, and packages, in one place. |
| [Publishing](publishing.md) | Getting a plugin into the official index, updates, revocation. |
| [Troubleshooting](troubleshooting.md) | Common errors and what they mean. |

## Licence

The SDK (`source/plugins/sdk`) is licensed under **MIT OR Apache-2.0**, at your option: it is compiled into your plugin,
so you can license your plugin as you like. The rest of All-in-1 Manager stays GPL-3.0-only.

## What you need

- Rust 1.97 or newer (1.88 for a plugin alone) with the `wasm32-wasip2` target.
- The SDK `aio-plugin-sdk` 1.1 (git tag `sdk-v1.1.0`), for the plugin API 1.1.
- A clone of this repository, for the `aio-plugin` tool (crate `aio-plugin-cli`) and, until the SDK is tagged, the SDK itself.
- All-in-1 Manager with plugin API 1.1, to try the plugin. Plugins made for API 1.0 keep working there unchanged.

## The pieces

- `source/plugins/sdk`: the SDK, `aio-plugin-sdk` 1.1.0. It supports exactly the plugin API `aio:plugin@1.1.0`
  (`source/wit/aio-plugin/plugin.wit`). A plugin depends only on this crate.
  Components built for `aio:plugin@1.0.0` still load.
- `source/crates/plugin_cli`: the `aio-plugin` tool (`new`, `build`, `pack`, `check`, `dev`).
- `source/plugins/hello` and `source/plugins/notes`: two example plugins. `notes` is commented line by line and shows
  private files, dialogs, a database read and how to fail gracefully.

## What is new in API 1.1

API 1.1 only adds to 1.0. A plugin built with SDK 1.1 says `"api": "1.1"` in its manifest (`aio-plugin new` writes
it, and `aio-plugin check` refuses a lower one).

- **Long jobs**: `Plugin::tick` and `host::keep_working` run a job in steps between pages, with the page still
  usable (a Cancel button works). See [Long jobs](ui.md#long-jobs).
- **Waiting with a time limit**: `tcp::Conn::recv_timeout` returns `None` when nothing arrived in time.
- **Port ranges**: `tcp:localhost:<a>-<b>`, at most 16 ports. See [Permissions](permissions.md).
- **Asking for hosts**: `http:ask` lets a plugin fetch from a public host it does not name, after the user allows
  it. See [Asking for hosts](permissions.md#asking-for-hosts-httpask).
- **Picked folders**: `folders::pick` lets the user choose a folder where the plugin may create data files. See
  [`folders`](api.md#folders).
