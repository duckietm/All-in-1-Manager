# External WASM plugins, milestone 1: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** All-in-1 Manager can install a third-party plugin from a `.aiop` file (or load one from a folder in developer
mode), show its permissions for consent, run it in a wasmtime sandbox, render the page it describes, and carry out its
SSH and database requests only after the administrator approved a preview.

**Architecture:** A new crate `aio-plugin-host` holds the WIT API, the manifest and package handling, the permission
checks and the wasmtime runtime. It reaches the server, the database and the user only through a `HostServices` trait,
so it is tested with fakes. `aio-app` implements `HostServices` with the real SSH/MariaDB session and dialogs. It adds
an "Installed" section to the Plugins page, a generic external-plugin page that renders the plugin's UI tree, and the
approval previews. The example plugin lives in its own Cargo workspace and is built for `wasm32-wasip2`.

**Tech Stack:** Rust 1.99 (edition 2021), wasmtime with `component-model` (latest stable at implementation time),
`wit-bindgen` for guests, `zip`, `serde_json`, egui 0.31.

**Spec:** `docs/superpowers/specs/2026-11-09-wasm-plugins-design.md` (branch `docs/wasm-plugins-design`). Read it first.
This plan covers the spec's first milestone only: "runtime + API + install from file + dev mode". The official index,
signing, the "Available" tab, the SDK/guide and the Catalogue ripper port are later plans.

**Branch:** `feature/wasm-plugins` from `feature/plugin-system` (fork `SimoLeoLife/All-in-1-Manager`; PR towards
`duckietm/All-in-1-Manager` `dev`).

## Global Constraints

- API package: `aio:plugin@1.0.0`. The host accepts plugins whose manifest `api` has major 1 and minor ≤ the host's.
- Package: `.aiop` = zip with `manifest.json`, `plugin.wasm`, optional `icon.png`, optional `i18n/<lang>.json`; nothing
  else. Max package 32 MiB, max `plugin.wasm` 24 MiB.
- Manifest `id`: `^[a-z0-9][a-z0-9-]{1,39}$`; must not equal a built-in plugin id from `plugins::PLUGINS`.
- Installed into `configuration/plugins/ext/<id>/`; private data in `configuration/plugins/ext/<id>/data/`.
- Sandbox limits per instance: 64 MiB linear memory; every call into the plugin is interrupted after 5 s (epoch
  interruption); never called on the egui thread.
- Permissions (exact strings): `ssh`, `ssh:root`, `sql:read:<table>[,<table>…]`, `sql:write:<table>[,<table>…]`,
  `sql:read-private:<table>.<column>`, `http:<host>`, `tcp:localhost:<port>`, `files` (private folder; always granted).
- Private columns (blocked without `sql:read-private`): `users.password`, `users.mail`, `users.ip_register`,
  `users.ip_current`, `users.auth_ticket`, `users.machine_id`, plus any column whose name contains `password`, `token`,
  `secret` or `ip_`.
- No raw SQL from plugins; hotel database only (the one `aio_remote::catalog_admin::hotel_database` finds).
- Every SSH script and every SQL write batch is shown in full and runs only after the user clicks Approve.
- Plugins run only while their page is open or the user acts on it (no background work in 1.0).
- Unverified plugins (from file, dev mode) always show a red "Not verified" badge.
- Every user-visible text goes through `t()`/`tf()` and gets translations in all 8 languages.

## Review Focus

1. A plugin that loops forever or panics while rendering: the page shows "The plugin stopped: …" with a Reload button,
   and the window stays responsive. (Task 4 test `runaway_call_is_interrupted`, Task 8 manual check.)
2. A hostile `.aiop`: `../` or absolute paths in the zip, a manifest id like `../x` or `default-catalog`, an oversized
   wasm, a missing `plugin.wasm`: refused before anything is written. (Task 2 tests.)
3. A plugin asking for a table it did not declare, or for `users.password` without `sql:read-private`: the call returns
   a permission error the plugin can show, nothing is queried. (Task 5 tests.)
4. A plugin built for API 2.x, or missing the `render`/`on-event` exports: refused at install with a clear message.
   (Task 2, Task 4 tests.)
5. The user logs out or the connection drops while an SSH/SQL preview is waiting: Approve fails cleanly with
   "Not connected", nothing half-runs, and the plugin gets an error result. (Task 6 test `approve_without_session_fails`.)

---

### Task 1: WIT API and the `aio-plugin-host` crate skeleton

**Files:**
- Create: `source/wit/aio-plugin/plugin.wit`
- Create: `source/crates/plugin_host/Cargo.toml`, `src/lib.rs`
- Modify: `Cargo.toml` (workspace member + `aio-plugin-host` and `wasmtime` in `[workspace.dependencies]`)
- Test: `source/crates/plugin_host/tests/wit.rs`

**Interfaces:**
- Produces: WIT world `plugin` in package `aio:plugin@1.0.0` with
  - guest exports: `render() -> ui-node`, `on-event(event: ui-event) -> result<_, string>`;
  - host imports: interface `host` with `log(level, text)`, `ask(question) -> bool`, `status(text)`,
    `run-script(script, as-root: bool) -> result<script-result, string>`,
    `sql-select(query: select) -> result<rows, string>`, `sql-write(ops: list<write-op>) -> result<u64, string>`,
    `http-get(url) -> result<list<u8>, string>`, `tcp-connect(port) -> result<tcp-handle, string>` with
    `tcp-send/tcp-recv/tcp-close`, `file-read(path) -> result<list<u8>, string>`, `file-write(path, data)`,
    `open-import-preview(file-path)` (core handoff, unused until the ripper plan);
  - types: `ui-node` (variant: card, heading, text, button{id,label,primary}, input{id,label,value},
    checkbox{id,label,checked}, select{id,options,selected}, table{columns,rows}, progress{done,total},
    log{lines}, banner{kind,text}, row/column containers), `ui-event` (clicked{id}, changed{id,value}),
    `select{table,columns,where: list<condition>,limit}`, `write-op` (insert/update/delete with typed `sql-value`s),
    `sql-value` (null, int, float, text, bytes).
- `aio_plugin_host::bindings` = `wasmtime::component::bindgen!` output for world `plugin` (async).

- [ ] **Step 1:** Write test `wit_package_parses` in `tests/wit.rs`: it loads `source/wit/aio-plugin` with
  `wit_parser::Resolve::push_dir` and asserts the package name is `aio:plugin@1.0.0` and world `plugin` exports
  `render` and `on-event`.
- [ ] **Step 2:** Run `cargo test -p aio-plugin-host --test wit`; expected FAIL (crate/WIT missing).
- [ ] **Step 3:** Write the WIT with the types above (recursive UI as a flat `list<ui-element>` with child indices,
  since WIT has no recursive types) and the crate with `bindgen!`.
- [ ] **Step 4:** Run the test; expected PASS. Run `cargo build -p aio-plugin-host`.
- [ ] **Step 5:** Commit `feat(plugins): WIT API aio:plugin@1.0.0 and plugin host crate`.

### Task 2: Manifest and `.aiop` package install/remove

**Files:**
- Create: `source/crates/plugin_host/src/manifest.rs`, `src/package.rs`, `src/permissions.rs`
- Test: `source/crates/plugin_host/tests/package.rs`

**Interfaces:**
- Produces:
  - `pub struct Manifest { id, name, version: semver-string, author, description, api: String, permissions: Vec<Permission>, page_title: String, tile: Option<Tile> }`
    with `Manifest::parse(json: &[u8]) -> Result<Manifest>` (validates id regex, api major 1, permission grammar);
  - `pub enum Permission { Ssh, SshRoot, SqlRead(Vec<String>), SqlWrite(Vec<String>), SqlReadPrivate(String, String), Http(String), TcpLocal(u16), Files }` with `Permission::parse(&str)` and `Display` giving back the exact string;
  - `pub fn inspect(aiop: &[u8], reserved_ids: &[&str]) -> Result<Package>` (reads, validates, no writes);
  - `pub fn install(pkg: &Package, root: &Path) -> Result<PathBuf>` (atomic: `.partial` dir then rename, same pattern as `aio-app` `plugins/packs.rs::install`);
  - `pub fn remove(id: &str, root: &Path) -> Result<()>`; `pub fn installed(root: &Path) -> Vec<Manifest>`.

- [ ] **Step 1:** Write tests: `valid_package_installs_and_lists`, `zip_with_parent_path_is_refused`,
  `absolute_path_is_refused`, `id_with_dots_is_refused`, `builtin_id_is_refused` (`"default-catalog"`),
  `api_2_is_refused`, `missing_wasm_is_refused`, `oversized_wasm_is_refused` (25 MiB), `unknown_permission_is_refused`,
  `failed_install_keeps_old_version`, `permission_strings_round_trip` (every grammar form from Global Constraints).
- [ ] **Step 2:** Run `cargo test -p aio-plugin-host --test package`; expected FAIL.
- [ ] **Step 3:** Implement the three modules.
- [ ] **Step 4:** Run the tests; expected PASS.
- [ ] **Step 5:** Commit `feat(plugins): manifest, permissions and .aiop install`.

### Task 3: Example plugin "hello" and its build

**Files:**
- Create: `source/plugins/Cargo.toml` (separate workspace), `source/plugins/hello/{Cargo.toml,src/lib.rs,manifest.json}`
- Create: `source/tools/pack_plugin.py` (`python source/tools/pack_plugin.py source/plugins/hello` → `dist/hello.aiop`)
- Create: `source/crates/plugin_host/tests/support/mod.rs` with `fn hello_aiop() -> Vec<u8>` (builds via the script, panics with "rustup target add wasm32-wasip2" when the target is missing)
- Modify: `.github/workflows/ci.yml` and `release.yml`: `rustup target add wasm32-wasip2` before tests

**Interfaces:**
- Produces: plugin `hello` (only permission: `files`): renders a heading,
  a text input `name`, a button `greet`, and a counter persisted with `file-write("count.txt")`; on `clicked{greet}` it
  logs "Hello <name>" and increments the counter. A second build flag `--features runaway` makes `render` loop forever
  (used by Task 4's test).

- [ ] **Step 1:** Write test `hello_package_is_valid` (support builds it; `inspect` accepts it; manifest id `hello`).
- [ ] **Step 2:** Run it; expected FAIL.
- [ ] **Step 3:** Write the plugin with `wit-bindgen` against `source/wit/aio-plugin`, the pack script, the CI lines.
- [ ] **Step 4:** Run it; expected PASS.
- [ ] **Step 5:** Commit `feat(plugins): example plugin and packing script`.

### Task 4: Runtime: load, limits, render, events, files

**Files:**
- Create: `source/crates/plugin_host/src/runtime.rs`, `src/services.rs`
- Test: `source/crates/plugin_host/tests/runtime.rs`

**Interfaces:**
- Produces:
  - `#[async_trait] pub trait HostServices: Send + Sync { async fn ask(&self, q: String) -> bool; fn log(&self, level: Level, text: &str); fn status(&self, text: &str); async fn run_script(&self, req: ScriptRequest) -> Result<ScriptResult, String>; async fn sql_select(&self, sql: String, params: Vec<SqlValue>) -> Result<Rows, String>; async fn sql_write(&self, batch: WriteBatch) -> Result<u64, String>; async fn http_get(&self, url: &str) -> Result<Vec<u8>, String>; fn open_import_preview(&self, path: PathBuf); }`
    (the permission checks of Task 5/6 sit *before* these calls, inside the host);
  - `pub struct PluginInstance` with `PluginInstance::load(engine: &Engine, dir: &Path, manifest: Manifest, services: Arc<dyn HostServices>) -> Result<Self>`,
    `async fn render(&mut self) -> Result<UiTree, PluginError>`, `async fn event(&mut self, e: UiEvent) -> Result<(), PluginError>`;
  - `pub fn engine() -> Engine` (component model, async, epoch interruption; a background thread ticks the epoch every 100 ms);
  - `PluginError::{Trapped(String), TimedOut, Denied(String)}`.

- [ ] **Step 1:** Write tests with the hello package and a fake `HostServices`: `hello_renders_heading_and_button`,
  `click_increments_persisted_counter` (counter survives a new instance), `files_cannot_escape_private_folder`
  (`file-read("../manifest.json")` → error), `runaway_call_is_interrupted` (runaway build → `TimedOut` within 6 s),
  `memory_over_64_mib_traps`.
- [ ] **Step 2:** Run `cargo test -p aio-plugin-host --test runtime`; expected FAIL.
- [ ] **Step 3:** Implement: `wasmtime::Config` with `async_support`, `epoch_interruption`, `ResourceLimiter` for
  64 MiB; files mapped to `data/` with path normalisation that rejects `..`, absolute and drive paths.
- [ ] **Step 4:** Run; expected PASS.
- [ ] **Step 5:** Commit `feat(plugins): sandboxed runtime with limits`.

### Task 5: Permission-checked SQL, HTTP and local TCP

**Files:**
- Create: `source/crates/plugin_host/src/sql.rs`, `src/net.rs`
- Test: `source/crates/plugin_host/tests/permissions.rs`

**Interfaces:**
- Consumes: `Permission`, `HostServices`.
- Produces: `pub fn build_select(q: &Select, granted: &[Permission]) -> Result<(String, Vec<SqlValue>), String>` and
  `pub fn build_writes(ops: &[WriteOp], granted: &[Permission]) -> Result<WriteBatch, String>` (identifiers quoted with
  backticks via `aio_remote::sql_session::quote`, values always parameters); `WriteBatch { statements: Vec<(String, Vec<SqlValue>)>, summary: Vec<String> }`
  (summary lines for the preview, e.g. "UPDATE rooms SET name = 'x' WHERE id = 3"); `pub fn check_http(url, granted)`,
  `pub fn check_tcp(port, granted)`.

- [ ] **Step 1:** Write tests: `select_on_declared_table_builds_parameterised_sql`, `select_on_undeclared_table_is_denied`,
  `private_column_needs_read_private` (`users.password` denied; granted with `sql:read-private:users.password`),
  `select_star_on_users_excludes_private_columns` (columns must be listed: `*` is refused for tables with private
  columns), `identifier_injection_is_refused` (table `rooms; DROP TABLE x`), `write_needs_sql_write`,
  `http_only_to_declared_host` (subdomain and other port refused), `tcp_only_declared_localhost_port`.
- [ ] **Step 2:** Run; expected FAIL.
- [ ] **Step 3:** Implement; wire the checks into the runtime's host import functions (denials return `Err(String)`
  to the plugin and `PluginError::Denied` is logged).
- [ ] **Step 4:** Run; expected PASS (also Task 4 tests).
- [ ] **Step 5:** Commit `feat(plugins): permission-checked SQL, HTTP and TCP`.

### Task 6: Approval queue for SSH scripts and SQL writes

**Files:**
- Create: `source/crates/plugin_host/src/approvals.rs`
- Test: `source/crates/plugin_host/tests/approvals.rs`

**Interfaces:**
- Produces: `pub struct Approvals` (shared, `Arc`), `pub enum Pending { Script { plugin, script, as_root }, Writes { plugin, batch: WriteBatch } }`,
  `Approvals::request(&self, p: Pending) -> impl Future<Output = Decision>`, `Approvals::pending(&self) -> Vec<(u64, Pending)>`,
  `Approvals::decide(&self, id: u64, d: Decision)`, `Decision::{Approved, Rejected}`. The runtime's `run-script` /
  `sql-write` imports check `ssh` / `ssh:root` / `sql:write`, then wait on `request`, then call `HostServices`.

- [ ] **Step 1:** Write tests: `script_runs_only_after_approval`, `rejected_script_returns_error_to_plugin`,
  `root_script_needs_ssh_root`, `approve_without_session_fails` (fake services return "Not connected"; plugin gets
  `Err("Not connected")`), `closing_page_rejects_pending` (dropping the instance rejects its pending items).
- [ ] **Step 2:** Run; expected FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run; expected PASS.
- [ ] **Step 5:** Commit `feat(plugins): approval queue for scripts and database writes`.

### Task 7: Plugins page: install from file, consent, remove, switch

**Files:**
- Create: `source/crates/app/src/plugins/external.rs`
- Modify: `source/crates/app/src/views/plugins.rs`, `source/crates/app/src/plugins/mod.rs`, `source/crates/app/Cargo.toml`
- Test: unit tests in `plugins/external.rs`

**Interfaces:**
- Consumes: `aio_plugin_host::{package, Manifest, Permission}`.
- Produces: `pub struct ExternalPlugins` (loaded at startup from `configuration/plugins/ext/`) with
  `list() -> Vec<ExternalInfo { manifest, verified: bool, dev: bool }>`, `install_file(path) -> Result<Manifest>`,
  `remove(id)`; `fn permission_text(p: &Permission) -> String` (translated, one plain sentence each, e.g. `sql:write:rooms`
  → "Change the table rooms of the hotel database (you approve each change)"). External plugins use the same
  on/off switch store (`DisabledPlugins`) with ids prefixed `ext:`.

- [ ] **Step 1:** Write tests: `every_permission_has_a_sentence`, `external_ids_are_prefixed_in_settings`,
  `install_then_remove_round_trip` (temp configuration dir).
- [ ] **Step 2:** Run `cargo test -p aio-app plugins::external`; expected FAIL.
- [ ] **Step 3:** Implement; the Plugins page gets an "Installed plugins" section: "Install from file …" (rfd picker,
  `.aiop` filter) → consent dialog listing `permission_text` lines and a red "Not verified" note → install; each card
  shows name, version, author, permissions (collapsible), switch, Remove.
- [ ] **Step 4:** Run; expected PASS. Manual: install `dist/hello.aiop`, see it listed with its permission.
- [ ] **Step 5:** Commit `feat(plugins): install external plugins from file`.

### Task 8: External plugin pages, approvals UI and `HostServices` for the app

**Files:**
- Create: `source/crates/app/src/views/external_plugin.rs`, `source/crates/app/src/plugins/services.rs`
- Modify: `source/crates/app/src/gui/mod.rs` (Page variant, sidebar/Addons/dashboard entries for external plugins), `views/addons.rs`

**Interfaces:**
- Consumes: `PluginInstance`, `Approvals`, `UiTree`, `ExternalPlugins`.
- Produces: `Page::External(u16)` (index into `ExternalPlugins::list()`); `AppServices` implementing `HostServices`
  with `Shared` (dialogs, status, session) and a lazily opened `SqlSession` with the saved MariaDB account (as
  `views/catalog_rip.rs` `HotelItems` does); `ExternalPluginView::show(ui, palette, shared)` rendering every `ui-node`
  kind with existing widgets (`widgets::card`, `primary_button`, `log_view::show`, `result_banner`, …) and sending
  events through the runtime task; a pending-approval card above the page with the full script / SQL summary and
  Approve / Reject.

- [ ] **Step 1:** Write unit test `every_ui_node_kind_has_a_renderer` (exhaustive match helper over the generated enum)
  and `ui_tree_with_bad_child_index_is_rejected` (renderer returns an error card, no panic).
- [ ] **Step 2:** Run; expected FAIL.
- [ ] **Step 3:** Implement. Instances live while the page is open (dropped on navigate away → pending approvals
  rejected); runtime calls on the tokio runtime; a trapped/timed-out plugin shows "The plugin stopped: …" + Reload.
- [ ] **Step 4:** Run `cargo test --workspace`; expected PASS. Manual: open hello's page, type a name, click Greet, see
  the log line and counter; runaway build shows the stopped card while the window stays usable.
- [ ] **Step 5:** Commit `feat(plugins): external plugin pages and approvals`.

### Task 9: Developer mode

**Files:**
- Modify: `source/crates/core/src/settings.rs` (`developer_plugins: Vec<String>` folders, `#[serde(default)]`),
  `views/plugins.rs`, `plugins/external.rs`

**Interfaces:**
- Produces: Settings → "Developer mode" switch (off by default); when on, the Plugins page shows "Load a plugin folder …";
  a folder with `manifest.json` + `plugin.wasm` (or `target/wasm32-wasip2/release/*.wasm` via the manifest's
  `dev_wasm` path) is listed with a red "Developer" badge and a Reload button that re-reads the files.

- [ ] **Step 1:** Write tests: `dev_folder_loads_without_install`, `dev_mode_off_ignores_folders`,
  `old_settings_without_field_load` (settings.json without `DeveloperPlugins`).
- [ ] **Step 2:** Run; expected FAIL.
- [ ] **Step 3:** Implement (consent dialog the first time a folder is added, same as install from file).
- [ ] **Step 4:** Run; expected PASS.
- [ ] **Step 5:** Commit `feat(plugins): developer mode`.

### Task 10: Texts, README, final checks, PR

**Files:**
- Modify: `source/crates/app/assets/i18n/*.json`, `README.md` (section "External plugins": install, permissions,
  approvals, developer mode, building a plugin with `source/plugins/hello` as the example)

- [ ] **Step 1:** Run `python source/tools/i18n_keys.py --missing`; translate the missing keys in all 8 languages
  (keep `{placeholders}`), delete the `.missing.json` files; expected "0 missing" for every language.
- [ ] **Step 2:** Run `cargo test --workspace` and `cargo clippy --workspace --all-targets`; expected PASS and no new
  warnings.
- [ ] **Step 3:** Commit `docs(plugins): external plugins; translations`, push `feature/wasm-plugins` to `fork`, open
  the PR towards `duckietm/All-in-1-Manager` `dev` (base `feature/plugin-system` must be merged first, or the PR says
  it is stacked on it).
