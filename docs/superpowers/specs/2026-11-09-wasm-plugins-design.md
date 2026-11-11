# External plugins (WebAssembly): design

Date: 2026-11-09. Status: approved by the owner (SimoLeoLife), 2026-11-11.

## Intent

All-in-1 Manager (a Rust egui desktop app that sets up and manages a Habbo retro server over SSH) has optional parts
("plugins": CMS, Gitea, Fail2Ban, CrowdSec, catalogue ripper, ...). Today they are compiled into the program and
registered by hand in 6-7 places; the Plugins page can only switch them on and off. The owner wants plugins that are
**not hardcoded**: installable, updatable and removable without rebuilding the Manager, and **written by other retro
developers too**, published through an **approved** index.

## Goals

- A plugin is a file (`.aiop`) that the Manager installs, updates, switches off and removes at run time.
- Plugins are WebAssembly components run in a sandbox; a plugin can only do what its manifest declares and the user
  accepted.
- A stable, versioned, documented API so a plugin built for API 1.x keeps working in later Managers.
- Third-party developers can write plugins (Rust first; other languages that build WebAssembly components are possible)
  with an SDK, a template, an example plugin and a guide.
- An official, **curated** plugin index: plugins enter it only after a maintainer approved their source; what the
  Manager installs is provably built from that source; the index is signed and plugins can be revoked.
- The Plugins page gains "Available" (index) next to "Installed", with install / update / remove and the permissions
  shown before installing.

## Non-goals

- Native plugins (DLL/.so) or giving plugins direct access to egui or to the Manager's memory.
- Arbitrary network or file access for plugins.
- Converting all built-in addons now. The built-in ones stay native; they move later, one at a time, if worth it.
- Paid plugins, ratings, comments.

## Anti-goals (a failure even if it "works")

- **A plugin that changes the server or the database without the administrator seeing it first.** Every SSH command
  and every database write a plugin asks for goes through a preview the user approves (as the Catalogue manager's
  import does), unless the user explicitly allowed that plugin to run a named, read-only kind of action unattended.
- A plugin escaping its sandbox, reading the Manager's configuration (SSH keys, passwords) or another plugin's data.
- An index that serves a binary no maintainer reviewed, or a signing key that lives on someone's laptop.
- Breaking every plugin when the Manager's look or internals change.

## Constraints and facts

- The Manager: workspace crates `aio-core` (settings, configuration folder, credentials), `aio-remote` (the SSH
  `Server` trait, `SqlSession` over an SSH tunnel, install scripts), `aio-app` (egui GUI). Plugin registry today:
  `source/crates/app/src/plugins/mod.rs` (`PLUGINS`, `page_visible`, switches in settings.json `DisabledPlugins`);
  data packs with SHA-256-checked download and atomic install: `plugins/packs.rs` (reuse its download/verify/install).
- Pages, wizards, the sidebar, the Addons tiles and the dashboard are hand-written per page (`gui/mod.rs`); a plugin
  page needs a generic page slot in that UI.
- The Manager runs on Windows and Linux desktops (release builds for windows-x64 and linux-x64).
- Plugins are untrusted code from third parties. They run on the administrator's PC, which holds SSH keys to the
  hotel server.
- The Manager is duckietm's project (`duckietm/All-in-1-Manager`); the owner contributes to it. The index's public key
  ends up inside the Manager, so duckietm has to agree with the trust setup.

## How it works (decided)

### Runtime

- **wasmtime** with the **WebAssembly Component Model**; the API is a **WIT** package with a semantic version
  (`aio:plugin@1.0.0`). The Manager supports one major version at a time and every minor version of it.
- Each plugin runs in its own store with memory and fuel/epoch limits, so a stuck or greedy plugin cannot freeze the
  window; calls into plugins never run on the UI thread.

### What a plugin can do (host API, behind permissions)

- **UI:** a plugin does not draw. It returns a **description of its page** (cards, headings, text, buttons, inputs,
  checkboxes, selects, tables, progress bars, a log, banners) and receives events (button clicked, value changed). The
  Manager renders it with its own widgets and theme. A plugin can register: a page, an Addons/dashboard tile, a
  sidebar entry in a fixed group.
- **Dialogs:** questions, confirmations, text input, file open/save (the user picks; the plugin gets only that file).
- **Server (permission `ssh`):** run a script on the logged-in server; shown in a preview (the full script) and run
  only after the user approves; output streamed back.
- **Database (permissions `sql:read` and `sql:write:<tables>`):** queries on the hotel database through the Manager's
  existing MariaDB session; reads may run unattended; writes are collected and applied in one transaction after a
  preview.
- **Files:** a private folder per plugin (`configuration/plugins/<id>/data/`); nothing else on disk.
- **Network (permission `http:<hosts>`):** HTTP(S) only to the hosts in the manifest. **Local TCP**
  (`tcp:localhost:<ports>`) for tools like G-Earth.
- **Log, progress, status line, translations** (the plugin ships its own texts per language).

### Package and manifest

- `.aiop` = zip with `manifest.json` (id, name, version, author, description, API version, permissions, pages/tiles,
  icon), the component `.wasm`, `icon.png`, `i18n/<lang>.json`.
- Installed into `configuration/plugins/<id>/`, next to the existing data packs.

### Trust and distribution

- **Official index:** a GitHub organization shared by the owner and duckietm (name to be chosen, e.g.
  `all-in-1-plugins`), repository `index`.
- **Submission:** a developer opens a pull request with the plugin's **source** (or a pinned commit of their repo).
  CI builds the `.aiop` from source in a fixed toolchain, validates the manifest and the API version, and posts the
  requested permissions in the PR. A maintainer reviews the code and merges.
- **Publishing:** after the merge, CI (in a protected environment that needs a maintainer's approval) builds again,
  publishes the `.aiop` as a release asset, and updates `index.json` (id, version, API version, permissions, SHA-256,
  download URL), **signed** with the index key, which exists only as a secret of that environment. The Manager embeds
  the public key and accepts only a correctly signed index; every download is checked against the index's SHA-256.
- **Revocation:** the signed index carries a revocation list; the Manager switches off a revoked plugin at the next
  start and tells the user why.
- **Updates:** same path; an update that asks for new permissions needs the user's consent again.
- **Outside the index:** installing a `.aiop` from a file is allowed (developers, testing), with a red "not verified"
  warning and the permissions shown; such plugins never update automatically.

### Developer kit

- An SDK crate for Rust (bindings generated from the WIT, helpers for the UI description), a `cargo` template, a
  command to build and pack the `.aiop`, a plugin that runs in a "dev mode" Manager loading from a folder with reload,
  and a guide (API reference, permissions, review rules).
- **First real plugin: the Catalogue ripper**, ported from the built-in addon, because it uses G-Earth (local TCP),
  SQL, files and a rich page: it proves the API.

### In the Manager

- The Plugins page lists built-in plugins (as today) and installed external ones; "Available" shows the index.
- The UI gets a generic plugin page slot and a generic tile/sidebar entry fed by the registry (the 6-7 hand-written
  places stay for built-in pages but read the registry for plugin ones).

## Calls made after the builder check (by Claude, for the owner to check)

- **Database scope.** Plugins never send raw SQL. The API offers structured operations (select / insert / update /
  delete on a named table, with typed values and parameterised conditions) on the **hotel database only**. Both reads
  and writes name their tables in the manifest (`sql:read:<tables>`, `sql:write:<tables>`). Sensitive columns
  (password hashes, emails, IPs, tokens of `users` and similar) are blocked unless a separate, loudly shown permission
  `sql:read-private:<table>.<column>` is granted.
- **SSH.** `ssh` runs as the login user; root is a separate permission `ssh:root`. Every script is shown in full and
  approved before it runs. "Unattended" covers only a fixed list of host-defined read-only actions (service status,
  disk space, reading a known log), never plugin-written scripts.
- **Lifecycle (1.0).** Plugins run only while their page is open or the user acts on them; no background work, timers
  or startup hooks in 1.0. An instance lives while its page is open and is torn down when switched off or revoked.
- **Build provenance (1.0).** CI builds from a pinned commit in the protected environment, signs the result and
  records the source commit in `index.json`. Bit-for-bit reproducible builds are a later goal, not 1.0.
- **Catalogue ripper.** The built-in one stays until the plugin version matches it; then the built-in one is removed and
  the `catalog-rip` switch carries over. The API gets one core handoff: "open this file as an import preview in the
  Catalogue manager".
- **Updates.** Index plugins never update by themselves: the Plugins page shows that an update is available and the
  user clicks Update (and accepts new permissions, if any).
- **Dev mode.** A setting in normal release builds ("Developer mode"), off by default, that loads a plugin from a
  folder with reload; it carries the same red "not verified" warning and consent as installing from a file.
- **Milestones.** Runtime + API + install from file + dev mode first, with the index's
  trust anchor (public key, index URL) kept swappable; the official index, signing and the "Available" tab come once
  the trust setup is agreed.

## Answered by the owner

1. **duckietm agrees** to external plugins and to the index's public key in the Manager. The work goes to
   `duckietm/All-in-1-Manager` as pull requests (towards `dev`), from branches of the fork `SimoLeoLife/All-in-1-Manager`.
   The GitHub organization for the index and its protected CI environment are created by the owner and duckietm (a
   person has to do it); the code keeps the trust anchor configurable until the real public key exists.

## Left to the builder

- Exact WIT interfaces and the UI description schema (keep 1.0 small; it can grow in minor versions).
- wasmtime configuration (limits, async, epochs), and how plugin calls are scheduled with the egui loop.
- Signature format (e.g. minisign/Ed25519) and index schema; CI workflows of the index repository.
- Preview UX for scripts and SQL writes.
- Which other languages to document beyond Rust.
- Milestones and order (e.g. runtime + API + dev mode → index + signing → SDK + guide → Catalogue ripper as a plugin).
