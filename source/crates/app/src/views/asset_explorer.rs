//! Two-pane file manager in the style of Norton Commander. The left pane is this computer and the right pane is the
//! server, locked to the gamedata folder. Uploads get owner www-data, folders 2775 and files 664.
//! (AssetExplorerView.cs and NameDialog.cs)
//!
//! The panes live in a `State` behind a mutex: the tasks (listing, copying, renaming) change it, the page draws it.
//! The page never holds the lock while it starts an action: it collects what was clicked or pressed, unlocks, and
//! then acts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use aio_remote::gamedata_files;
use aio_remote::Server;
use chrono::{DateTime, Local};
use eframe::egui::{self, Color32, RichText};
use parking_lot::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

use crate::gui::dialogs::Choice;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{self, Palette};
use crate::gui::widgets;
use crate::gui::{Page, Request, Shared, View};
use crate::i18n::{n, t, tf};

const IMAGE_EXTENSIONS: &[&str] = &["png", "gif", "jpg", "jpeg", "bmp", "webp"];
const ROW_HEIGHT: f32 = 22.0;
const COLUMN_ICON: f32 = 26.0;
const COLUMN_SIZE: f32 = 90.0;
const COLUMN_DATE: f32 = 130.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Local,
    Remote,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Local => Side::Remote,
            Side::Remote => Side::Local,
        }
    }
}

/// A row of a pane.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    name: String,
    is_directory: bool,
    size: i64,
    modified: Option<DateTime<Local>>,
    /// Local pane only: the absolute path.
    full_path: PathBuf,
    is_parent: bool,
    is_drive: bool,
}

impl Item {
    fn parent() -> Self {
        Item { name: "..".into(), is_directory: true, size: 0, modified: None, full_path: PathBuf::new(), is_parent: true, is_drive: false }
    }
}

fn is_image(name: &str) -> bool {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

#[derive(Default)]
struct Pane {
    /// An absolute folder or "" for the drive list on the local pane, a path relative to gamedata on the server pane.
    location: String,
    items: Vec<Item>,
    selected: BTreeSet<usize>,
    /// The row the keyboard is on; also the end of a Shift range.
    cursor: Option<usize>,
    /// Where a Shift range starts.
    anchor: Option<usize>,
    path_text: String,
    /// Scroll the cursor row into view on the next frame.
    scroll: bool,
    /// Changes with every new list or selection, so the page knows when to preview again.
    version: u64,
}

impl Pane {
    fn changed(&mut self) {
        self.version += 1;
    }

    /// The new list after a load; selects the named items, or else the first row.
    fn apply(&mut self, items: Vec<Item>, path_text: String, select: Option<&[String]>) {
        self.items = items;
        self.path_text = path_text;
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        let wanted: Vec<usize> = match select {
            Some(names) if !names.is_empty() => self.items.iter().enumerate().filter(|(_, i)| names.contains(&i.name)).map(|(n, _)| n).collect(),
            _ => Vec::new(),
        };
        if !wanted.is_empty() {
            self.selected.extend(wanted.iter().copied());
            self.cursor = wanted.first().copied();
            self.anchor = self.cursor;
        } else if !self.items.is_empty() {
            self.select_only(0);
        }
        self.scroll = true;
        self.changed();
    }

    fn select_only(&mut self, index: usize) {
        self.selected.clear();
        self.selected.insert(index);
        self.cursor = Some(index);
        self.anchor = Some(index);
        self.changed();
    }

    /// A click on a row: plain selects only it, Ctrl adds or removes it, Shift selects the range from the anchor.
    fn click(&mut self, index: usize, ctrl: bool, shift: bool) {
        if index >= self.items.len() {
            return;
        }
        if shift {
            let anchor = self.anchor.unwrap_or(index);
            self.selected = (anchor.min(index)..=anchor.max(index)).collect();
            self.cursor = Some(index);
        } else if ctrl {
            if !self.selected.remove(&index) {
                self.selected.insert(index);
            }
            self.cursor = Some(index);
            self.anchor = Some(index);
        } else {
            self.select_only(index);
            return;
        }
        self.changed();
    }

    /// The arrow keys (and Home, End, Page Up/Down): moves the cursor, Shift extends the selection.
    fn move_cursor(&mut self, delta: isize, shift: bool) {
        if self.items.is_empty() {
            return;
        }
        let current = self.cursor.unwrap_or(0) as isize;
        let target = (current + delta).clamp(0, self.items.len() as isize - 1) as usize;
        if shift {
            let anchor = self.anchor.unwrap_or(current as usize);
            self.selected = (anchor.min(target)..=anchor.max(target)).collect();
            self.cursor = Some(target);
            self.changed();
        } else {
            self.select_only(target);
        }
        self.scroll = true;
    }

    fn select_all(&mut self) {
        self.selected = (0..self.items.len()).collect();
        self.changed();
    }

    /// The selection without the ".." row.
    fn selected_items(&self) -> Vec<Item> {
        self.selected.iter().filter_map(|&i| self.items.get(i)).filter(|i| !i.is_parent).cloned().collect()
    }

    /// The row Enter opens: the cursor row when it is selected, or the first selected row.
    fn current(&self) -> Option<&Item> {
        let index = self.cursor.filter(|c| self.selected.contains(c)).or_else(|| self.selected.first().copied())?;
        self.items.get(index)
    }

    fn summary(&self) -> String {
        let folders = self.items.iter().filter(|i| i.is_directory && !i.is_parent && !i.is_drive).count();
        let files: Vec<&Item> = self.items.iter().filter(|i| !i.is_directory).collect();
        let size: i64 = files.iter().map(|i| i.size).sum();
        let mut text = tf(
            "{folders} folders, {files} files ({size})",
            &[("folders", &folders.to_string()), ("files", &files.len().to_string()), ("size", &bytes(size))],
        );
        let selected = self.selected_items().len();
        if selected > 1 {
            text += &tf(" · {count} selected", &[("count", &selected.to_string())]);
        }
        text
    }
}

/// Where a drop goes: into the folder row under the pointer, or else into the folder on screen. None: no drop here.
fn drop_folder(local: bool, location: &str, row: Option<&Item>) -> Option<String> {
    if local {
        if location.is_empty() {
            return row.filter(|r| r.is_drive).map(|r| r.full_path.to_string_lossy().into_owned());
        }
        return Some(match row {
            Some(r) if r.is_directory && !r.is_parent => r.full_path.to_string_lossy().into_owned(),
            _ => location.to_string(),
        });
    }
    Some(match row {
        Some(r) if r.is_directory && !r.is_parent => remote_join(location, &r.name),
        _ => location.to_string(),
    })
}

fn remote_join(location: &str, name: &str) -> String {
    if location.is_empty() { name.to_string() } else { format!("{location}/{name}") }
}

/// One folder up on the server: (the new location, the folder we came from). None at the gamedata root.
fn remote_up(location: &str) -> Option<(String, String)> {
    if location.is_empty() {
        return None;
    }
    Some(match location.rfind('/') {
        Some(slash) => (location[..slash].to_string(), location[slash + 1..].to_string()),
        None => (String::new(), location.to_string()),
    })
}

/// One folder up on this computer; above a drive or / comes the drive list (""). None on the drive list.
fn local_up(location: &str) -> Option<(String, String)> {
    if location.is_empty() {
        return None;
    }
    let path = Path::new(location);
    Some(match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            (parent.to_string_lossy().into_owned(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        }
        _ => (String::new(), String::new()),
    })
}

/// A number with `digits` decimals, halves rounded away from zero like .NET's format strings.
fn fixed(value: f64, digits: i32) -> String {
    let scale = 10f64.powi(digits);
    format!("{:.*}", digits as usize, (value * scale).round() / scale)
}

/// ServerStatusView.Bytes.
fn bytes(bytes: i64) -> String {
    let b = bytes as f64;
    if bytes >= 1 << 40 {
        format!("{} TB", fixed(b / (1u64 << 40) as f64, 1))
    } else if bytes >= 1 << 30 {
        format!("{} GB", fixed(b / (1u64 << 30) as f64, 1))
    } else if bytes >= 1 << 20 {
        format!("{} MB", fixed(b / (1u64 << 20) as f64, 0))
    } else {
        format!("{} KB", fixed(b / 1024.0, 0))
    }
}

/// ServerStatusView.Rate.
fn rate(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1024.0 * 1024.0 {
        format!("{} MB/s", fixed(bytes_per_second / (1024.0 * 1024.0), 2))
    } else if bytes_per_second >= 1024.0 {
        format!("{} KB/s", fixed(bytes_per_second / 1024.0, 1))
    } else {
        format!("{} B/s", fixed(bytes_per_second, 0))
    }
}

fn date(modified: Option<DateTime<Local>>) -> String {
    modified.map(|m| m.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

fn home() -> String {
    std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default()
}

// The text aio_remote::gamedata_files::name_problem returns (shown through t() below), copied here so
// source/tools/i18n_keys.py collects it. Keep in step with gamedata_files.
const _: &str = n("The name in the console: letters, digits, '.', '-' and '_', at most 64.");

/// What is wrong with a new name, or None (NameProblem in the C#).
fn name_problem(name: &str, local: bool, existing: &[String]) -> Option<String> {
    if let Some(problem) = gamedata_files::name_problem(name) {
        return Some(t(problem).to_string());
    }
    let invalid: &[char] = if cfg!(windows) { &['<', '>', ':', '"', '/', '\\', '|', '?', '*'] } else { &['/'] };
    if local && name.chars().any(|c| invalid.contains(&c) || c == '\0' || (cfg!(windows) && (c as u32) < 32)) {
        return Some(t("That name has characters this computer does not allow.").to_string());
    }
    if existing.iter().any(|e| e == name) {
        return Some(tf("{name} already exists here.", &[("name", name)]));
    }
    None
}

// ---- This computer ----

/// The drives on Windows, or / elsewhere, plus the home folder.
fn local_roots() -> Vec<Item> {
    let drive = |name: String, path: PathBuf| Item { name, is_directory: true, size: 0, modified: None, full_path: path, is_parent: false, is_drive: true };
    let mut roots = Vec::new();
    if cfg!(windows) {
        for letter in b'A'..=b'Z' {
            let path = PathBuf::from(format!("{}:\\", letter as char));
            if std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                roots.push(drive(format!("{}:", letter as char), path));
            }
        }
    } else {
        roots.push(drive("/".into(), PathBuf::from("/")));
    }
    let home = home();
    if !home.is_empty() {
        roots.push(drive(format!("Home  {home}"), PathBuf::from(&home)));
    }
    roots
}

#[cfg(windows)]
fn is_system(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x4 != 0
}

#[cfg(not(windows))]
fn is_system(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn load_local(location: &str) -> std::io::Result<Vec<Item>> {
    if location.is_empty() {
        return Ok(local_roots());
    }
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(location)? {
        // Unreadable entries are left out (IgnoreInaccessible), and so are system files.
        let Ok(entry) = entry else { continue };
        let Ok(metadata) = entry.metadata() else { continue };
        if is_system(&metadata) {
            continue;
        }
        let path = entry.path();
        // A link is followed to see whether it is a folder; anything else is known from its metadata already.
        let is_directory = if metadata.file_type().is_symlink() { path.is_dir() } else { metadata.is_dir() };
        let item = Item {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_directory,
            size: if is_directory { 0 } else { metadata.len() as i64 },
            modified: metadata.modified().ok().map(DateTime::<Local>::from),
            full_path: path,
            is_parent: false,
            is_drive: false,
        };
        if is_directory { folders.push(item) } else { files.push(item) }
    }
    sort_by_name(&mut folders);
    sort_by_name(&mut files);
    let mut items = vec![Item::parent()];
    items.extend(folders);
    items.extend(files);
    Ok(items)
}

fn sort_by_name(items: &mut [Item]) {
    items.sort_by_key(|i| i.name.to_lowercase());
}

/// The bytes of a file, or of every file under a folder (links left out).
fn local_size(path: &Path) -> i64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return 0 };
    if metadata.is_file() {
        return metadata.len() as i64;
    }
    if !metadata.is_dir() {
        return 0;
    }
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else { continue };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total += metadata.len() as i64;
            }
        }
    }
    total
}

// ---- The state and the actions ----

struct Preview {
    request: u64,
    image: Arc<egui::ColorImage>,
}

/// The previews shown last (file, size and date as the key), so going back to one is instant.
const PREVIEW_CACHE: usize = 24;

#[derive(Default)]
struct State {
    local: Pane,
    remote: Pane,
    active: Option<Side>,
    status: String,
    status_error: bool,
    progress: Option<f32>,
    busy: bool,
    preview_text: String,
    preview_request: u64,
    preview: Option<Preview>,
    /// The pane whose selection changed and wants a preview, and whether the preview worker runs: one at a time,
    /// always for the latest selection, so holding an arrow key does not start a download per row.
    preview_wanted: Option<Side>,
    preview_running: bool,
    preview_cache: std::collections::VecDeque<(String, Arc<egui::ColorImage>)>,
}

impl State {
    fn pane(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    fn pane_ref(&self, side: Side) -> &Pane {
        match side {
            Side::Local => &self.local,
            Side::Remote => &self.remote,
        }
    }

    fn active(&self) -> Side {
        self.active.unwrap_or(Side::Local)
    }
}

/// The actions, usable from tasks.
#[derive(Clone)]
struct Ops {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

impl Ops {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock()
    }

    fn show_status(&self, text: String, error: bool) {
        let mut state = self.lock();
        state.status = text;
        state.status_error = error;
        drop(state);
        self.shared.ctx.request_repaint();
    }

    fn show_progress(&self, text: String, done: u64, total: i64) {
        let mut state = self.lock();
        state.progress = Some(if total <= 0 { 0.0 } else { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 });
        state.status = text;
        state.status_error = false;
        drop(state);
        self.shared.ctx.request_repaint();
    }

    /// RunAsync: one action at a time, its failure in the status line.
    async fn run<T>(&self, what: &str, action: impl std::future::Future<Output = anyhow::Result<T>>) -> Option<T> {
        {
            let mut state = self.lock();
            if state.busy {
                return None;
            }
            state.busy = true;
        }
        let result = action.await;
        let failed = result.as_ref().err().map(|e| e.to_string());
        {
            let mut state = self.lock();
            state.busy = false;
            state.progress = None;
            let active = state.active();
            state.pane(active).scroll = true;
        }
        if let Some(message) = failed {
            self.show_status(tf("{what} failed: {message}", &[("what", what), ("message", &message)]), true);
        }
        self.shared.ctx.request_repaint();
        result.ok()
    }

    fn server(&self) -> Option<Arc<dyn Server>> {
        self.shared.session()
    }

    /// Lists a pane's folder again and selects `select` (or the first row).
    async fn load(&self, side: Side, select: Option<Vec<String>>) -> anyhow::Result<()> {
        let location = self.lock().pane_ref(side).location.clone();
        let (items, path_text) = match side {
            Side::Local => {
                let folder = location.clone();
                let items = tokio::task::spawn_blocking(move || load_local(&folder)).await??;
                (items, if location.is_empty() { t("Drives").to_string() } else { location.clone() })
            }
            Side::Remote => {
                let Some(server) = self.server() else { return Ok(()) };
                let entries = gamedata_files::list(server.as_ref(), &location).await?;
                let mut folders = Vec::new();
                let mut files = Vec::new();
                for e in entries {
                    let item = Item {
                        name: e.name,
                        is_directory: e.is_directory,
                        size: if e.is_directory { 0 } else { e.size },
                        modified: Some(e.modified),
                        full_path: PathBuf::new(),
                        is_parent: false,
                        is_drive: false,
                    };
                    if item.is_directory { folders.push(item) } else { files.push(item) }
                }
                sort_by_name(&mut folders);
                sort_by_name(&mut files);
                // No ".." at the gamedata root, since the pane is locked to it.
                let mut items = if location.is_empty() { Vec::new() } else { vec![Item::parent()] };
                items.extend(folders);
                items.extend(files);
                (items, gamedata_files::absolute(&location)?)
            }
        };
        let mut state = self.lock();
        let pane = state.pane(side);
        // A load for a folder that is no longer on screen is dropped.
        if pane.location == location {
            pane.apply(items, path_text, select.as_deref());
        }
        drop(state);
        self.shared.ctx.request_repaint();
        Ok(())
    }

    /// Opens a folder; on failure the pane stays where it was.
    async fn try_load(&self, side: Side, previous: String, select: Option<Vec<String>>) {
        if let Err(error) = self.load(side, select).await {
            let mut state = self.lock();
            state.pane(side).location = previous;
            drop(state);
            self.show_status(tf("Cannot open that folder: {message}", &[("message", &error.to_string())]), true);
        }
        self.lock().pane(side).scroll = true;
    }

    /// The first time the page is on screen with this login (ConnectedAsync).
    async fn connected(&self) {
        {
            let mut state = self.lock();
            state.remote.location.clear();
            if state.local.location.is_empty() && !cfg!(windows) {
                state.local.location = home();
            }
        }
        // Like Refresh: a server without gamedata yet shows the reason instead of stopping the program.
        self.run(t("Opening the folders"), async {
            self.load(Side::Local, None).await?;
            self.load(Side::Remote, None).await
        })
        .await;
        self.lock().active = Some(Side::Local);
    }

    async fn open_selected(&self, side: Side) {
        let (item, location) = {
            let state = self.lock();
            if state.busy {
                return;
            }
            let pane = state.pane_ref(side);
            let Some(item) = pane.current().cloned() else { return };
            (item, pane.location.clone())
        };
        if item.is_parent {
            self.up(side).await;
            return;
        }
        if !item.is_directory {
            return;
        }
        let target = match side {
            Side::Local => item.full_path.to_string_lossy().into_owned(),
            Side::Remote => remote_join(&location, &item.name),
        };
        self.lock().pane(side).location = target;
        self.try_load(side, location, None).await;
    }

    async fn up(&self, side: Side) {
        let (previous, step) = {
            let state = self.lock();
            if state.busy {
                return;
            }
            let location = state.pane_ref(side).location.clone();
            let step = match side {
                Side::Local => local_up(&location),
                Side::Remote => remote_up(&location), // locked to gamedata
            };
            (location, step)
        };
        let Some((target, child)) = step else { return };
        self.lock().pane(side).location = target;
        self.try_load(side, previous, (!child.is_empty()).then(|| vec![child])).await;
    }

    async fn refresh(&self) {
        self.run(t("Refreshing"), async {
            self.load(Side::Local, None).await?;
            self.load(Side::Remote, None).await?;
            self.show_status(t("Refreshed.").to_string(), false);
            Ok(())
        })
        .await;
    }

    async fn copy(&self) {
        let (side, items, remote_location, local_location) = {
            let state = self.lock();
            let side = state.active();
            let items: Vec<Item> = state.pane_ref(side).selected_items().into_iter().filter(|i| !i.is_drive).collect();
            (side, items, state.remote.location.clone(), state.local.location.clone())
        };
        if items.is_empty() {
            self.show_status(t("Select files or folders first.").to_string(), false);
            return;
        }
        match side {
            Side::Local => self.upload(items.into_iter().map(|i| i.full_path).collect(), remote_location).await,
            Side::Remote => {
                if local_location.is_empty() {
                    self.show_status(t("Open a folder on this computer (left) to download into.").to_string(), true);
                    return;
                }
                self.download(items.into_iter().map(|i| i.name).collect(), remote_location, local_location).await;
            }
        }
    }

    /// Returns 0 to replace, 1 to keep the existing files, or None to cancel.
    async fn ask_overwrite(&self, clashes: &[String], place: &str) -> Option<u8> {
        let mut list = clashes.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        if clashes.len() > 8 {
            list += &tf(" and {count} more", &[("count", &(clashes.len() - 8).to_string())]);
        }
        list.push('.');
        self.shared
            .dialogs
            .choose(
                t("Already there"),
                &tf("{count} item(s) already exist in {where}", &[("count", &clashes.len().to_string()), ("where", place)]),
                &list,
                None,
                vec![
                    Choice::new(t("Replace them"), t("The copies here replace the files there."), Some(0)).primary(),
                    Choice::new(t("Keep the existing files"), t("Only copy what is not there yet."), Some(1)),
                    Choice::new(t("Cancel"), t("Copy nothing."), None),
                ],
            )
            .await
    }

    async fn upload(&self, local_paths: Vec<PathBuf>, remote_dir: String) {
        let Some(server) = self.server() else { return };
        if self.lock().busy || local_paths.is_empty() {
            return;
        }
        let names: Vec<String> = local_paths.iter().map(|p| top_name(p)).collect();
        // A drop on a folder row targets a folder other than the one on screen, so list it from the server.
        let on_screen = {
            let state = self.lock();
            (state.remote.location == remote_dir).then(|| state.remote.items.iter().filter(|i| !i.is_parent).map(|i| i.name.clone()).collect::<Vec<_>>())
        };
        let existing = match on_screen {
            Some(names) => names,
            None => match gamedata_files::list(server.as_ref(), &remote_dir).await {
                Ok(entries) => entries.into_iter().map(|e| e.name).collect(),
                Err(error) => {
                    self.show_status(tf("Uploading failed: {message}", &[("message", &error.to_string())]), true);
                    return;
                }
            },
        };
        let clashes: Vec<String> = names.iter().filter(|n| existing.contains(n)).cloned().collect();
        let mut skip = false;
        if !clashes.is_empty() {
            let place = gamedata_files::absolute(&remote_dir).unwrap_or_else(|_| remote_dir.clone());
            match self.ask_overwrite(&clashes, &place).await {
                None => return,
                Some(answer) => skip = answer == 1,
            }
        }
        self.run(t("Uploading"), async {
            let paths = local_paths.clone();
            let total: i64 = tokio::task::spawn_blocking(move || paths.iter().map(|p| local_size(p)).sum()).await?;
            let target = gamedata_files::absolute(&remote_dir)?;
            self.show_progress(
                tf(
                    "Uploading {count} item(s), {size}, to {target} ...",
                    &[("count", &local_paths.len().to_string()), ("size", &bytes(total)), ("target", &target)],
                ),
                0,
                total,
            );
            let started = Instant::now();
            let ops = self.clone();
            let shown_target = target.clone();
            let progress = move |sent: u64| {
                let speed = sent as f64 / started.elapsed().as_secs_f64().max(0.1);
                ops.show_progress(
                    tf(
                        "Uploading to {target}: {sent} of {total} · {rate}",
                        &[("target", &shown_target), ("sent", &bytes(sent as i64)), ("total", &bytes(total)), ("rate", &rate(speed))],
                    ),
                    sent,
                    total,
                );
            };
            gamedata_files::upload(server.as_ref(), &local_paths, &remote_dir, skip, &progress).await?;
            if self.lock().remote.location == remote_dir {
                self.load(Side::Remote, Some(names.clone())).await?;
            }
            self.show_status(
                tf(
                    "Uploaded {count} item(s) ({size}) to {target}{kept}. Rights set: owner www-data, folders 2775, files 664.",
                    &[
                        ("count", &local_paths.len().to_string()),
                        ("size", &bytes(total)),
                        ("target", &target),
                        ("kept", if skip { t(", existing files kept") } else { "" }),
                    ],
                ),
                false,
            );
            Ok(())
        })
        .await;
    }

    async fn download(&self, names: Vec<String>, remote_dir: String, local_dir: String) {
        let Some(server) = self.server() else { return };
        if self.lock().busy || names.is_empty() {
            return;
        }
        let Some(files) = self.run(t("Reading the files"), gamedata_files::files_under(server.as_ref(), &remote_dir, &names)).await else {
            return;
        };
        if files.is_empty() {
            self.show_status(t("There are no files to download in the selection.").to_string(), false);
            return;
        }
        let Ok(remote_root) = gamedata_files::absolute(&remote_dir) else { return };
        let mut map: Vec<(String, PathBuf, i64)> = files
            .iter()
            .map(|(path, size)| {
                let mut local = PathBuf::from(&local_dir);
                local.extend(path.split('/'));
                (format!("{remote_root}/{path}"), local, *size)
            })
            .collect();
        let clashes: Vec<String> = map
            .iter()
            .filter(|m| m.1.exists())
            .map(|m| m.1.strip_prefix(&local_dir).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| m.1.to_string_lossy().into_owned()))
            .collect();
        if !clashes.is_empty() {
            match self.ask_overwrite(&clashes, &local_dir).await {
                None => return,
                Some(1) => map.retain(|m| !m.1.exists()),
                Some(_) => {}
            }
        }
        self.run(t("Downloading"), async {
            let total: i64 = map.iter().map(|m| m.2).sum();
            let started = Instant::now();
            self.show_progress(
                tf(
                    "Downloading {count} file(s), {size}, to {target} ...",
                    &[("count", &map.len().to_string()), ("size", &bytes(total)), ("target", &local_dir)],
                ),
                0,
                total,
            );
            let ops = self.clone();
            let shown_target = local_dir.clone();
            let progress = move |done: u64| {
                let speed = done as f64 / started.elapsed().as_secs_f64().max(0.1);
                ops.show_progress(
                    tf(
                        "Downloading to {target}: {done} of {total} · {rate}",
                        &[("target", &shown_target), ("done", &bytes(done as i64)), ("total", &bytes(total)), ("rate", &rate(speed))],
                    ),
                    done,
                    total,
                );
            };
            let pairs: Vec<(String, PathBuf)> = map.iter().map(|m| (m.0.clone(), m.1.clone())).collect();
            server.download_files(&pairs, &progress, CancellationToken::new()).await?;
            if self.lock().local.location == local_dir {
                self.load(Side::Local, Some(names.clone())).await?;
            }
            self.show_status(
                tf("Downloaded {count} file(s) ({size}) to {target}.", &[("count", &map.len().to_string()), ("size", &bytes(total)), ("target", &local_dir)]),
                false,
            );
            Ok(())
        })
        .await;
    }

    /// NameDialog: asks for a file or folder name; None when cancelled.
    async fn ask_name(&self, side: Side, heading: &str, message: &str, initial: &str, ok: &str) -> Option<String> {
        let existing: Vec<String> = self.lock().pane_ref(side).items.iter().filter(|i| !i.is_parent).map(|i| i.name.clone()).collect();
        let local = side == Side::Local;
        let check = Box::new(move |text: &str| name_problem(text.trim(), local, &existing));
        let name = self.shared.dialogs.input(heading, heading, message, "", initial, false, ok, Some(check)).await?;
        Some(name.trim().to_string())
    }

    async fn rename(&self) {
        let (side, items, location) = {
            let state = self.lock();
            let side = state.active();
            (side, state.pane_ref(side).selected_items(), state.pane_ref(side).location.clone())
        };
        if items.len() != 1 || items[0].is_drive {
            self.show_status(t("Select one file or folder to rename.").to_string(), false);
            return;
        }
        let item = items[0].clone();
        let Some(name) = self.ask_name(side, t("Rename"), &tf("New name for {name}:", &[("name", &item.name)]), &item.name, t("Rename")).await else {
            return;
        };
        if name == item.name {
            return;
        }
        self.run(t("Renaming"), async {
            match side {
                Side::Local => {
                    let target = item.full_path.parent().map(|p| p.join(&name)).unwrap_or_else(|| PathBuf::from(&name));
                    std::fs::rename(&item.full_path, target)?;
                }
                Side::Remote => {
                    if let Some(server) = self.server() {
                        gamedata_files::rename(server.as_ref(), &location, &item.name, &name).await?;
                    }
                }
            }
            if self.lock().pane_ref(side).location == location {
                self.load(side, Some(vec![name.clone()])).await?;
            }
            self.show_status(tf("Renamed {old} to {new}.", &[("old", &item.name), ("new", &name)]), false);
            Ok(())
        })
        .await;
    }

    async fn new_folder(&self) {
        let (side, location) = {
            let state = self.lock();
            let side = state.active();
            (side, state.pane_ref(side).location.clone())
        };
        if side == Side::Local && location.is_empty() {
            self.show_status(t("Open a drive or folder first.").to_string(), false);
            return;
        }
        let message = match side {
            Side::Local => tf("In {location}:", &[("location", &location)]),
            Side::Remote => tf(
                "In {location} (owner www-data, rights 2775):",
                &[("location", &gamedata_files::absolute(&location).unwrap_or_else(|_| location.clone()))],
            ),
        };
        let Some(name) = self.ask_name(side, t("New folder"), &message, "", t("Create")).await else { return };
        self.run(t("Creating the folder"), async {
            match side {
                Side::Local => std::fs::create_dir_all(Path::new(&location).join(&name))?,
                Side::Remote => {
                    if let Some(server) = self.server() {
                        gamedata_files::create_folder(server.as_ref(), &location, &name).await?;
                    }
                }
            }
            if self.lock().pane_ref(side).location == location {
                self.load(side, Some(vec![name.clone()])).await?;
            }
            self.show_status(tf("Created {name}.", &[("name", &name)]), false);
            Ok(())
        })
        .await;
    }

    async fn delete(&self) {
        let (side, items, location) = {
            let state = self.lock();
            let side = state.active();
            let items: Vec<Item> = state.pane_ref(side).selected_items().into_iter().filter(|i| !i.is_drive).collect();
            (side, items, state.pane_ref(side).location.clone())
        };
        if items.is_empty() {
            self.show_status(t("Select files or folders to delete.").to_string(), false);
            return;
        }
        let place = match side {
            Side::Local => t("from this computer").to_string(),
            Side::Remote => tf(
                "from the server ({location})",
                &[("location", &gamedata_files::absolute(&location).unwrap_or_else(|_| location.clone()))],
            ),
        };
        let mut list = items
            .iter()
            .take(10)
            .map(|i| if i.is_directory { tf("{name}/ (with everything in it)", &[("name", &i.name)]) } else { i.name.clone() })
            .collect::<Vec<_>>()
            .join(", ");
        if items.len() > 10 {
            list += &tf(" and {count} more", &[("count", &(items.len() - 10).to_string())]);
        }
        list += t(". This cannot be undone.");
        let go = self
            .shared
            .dialogs
            .choose(
                t("Delete"),
                &tf("Delete {count} item(s) {where}?", &[("count", &items.len().to_string()), ("where", &place)]),
                &list,
                false,
                vec![Choice::new(t("Delete"), t("Remove them for good."), true).primary(), Choice::new(t("Cancel"), t("Keep them."), false)],
            )
            .await;
        if !go {
            return;
        }
        self.run(t("Deleting"), async {
            match side {
                Side::Local => {
                    for item in &items {
                        if item.full_path.is_dir() {
                            std::fs::remove_dir_all(&item.full_path)?;
                        } else {
                            std::fs::remove_file(&item.full_path)?;
                        }
                    }
                }
                Side::Remote => {
                    if let Some(server) = self.server() {
                        let names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
                        gamedata_files::delete(server.as_ref(), &location, &names).await?;
                    }
                }
            }
            if self.lock().pane_ref(side).location == location {
                self.load(side, None).await?;
            }
            self.show_status(tf("Deleted {count} item(s).", &[("count", &items.len().to_string())]), false);
            Ok(())
        })
        .await;
    }

    /// Shows the preview of the latest selection, after the selection stopped changing for a moment; then the next
    /// one when it changed meanwhile, until none is wanted.
    async fn preview_worker(&self) {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            let side = {
                let mut state = self.lock();
                match state.preview_wanted.take() {
                    Some(side) => side,
                    None => {
                        state.preview_running = false;
                        return;
                    }
                }
            };
            // Still moving: wait for the selection to settle.
            if self.lock().preview_wanted.is_some() {
                continue;
            }
            self.preview(side).await;
        }
    }

    /// The selected file's name, size and date, and for an image a preview with its pixel size.
    async fn preview(&self, side: Side) {
        let (request, items, remote_location) = {
            let mut state = self.lock();
            state.preview_request += 1;
            (state.preview_request, state.pane_ref(side).selected_items(), state.remote.location.clone())
        };
        let item = if items.len() == 1 { Some(items[0].clone()) } else { None };
        let Some(item) = item.filter(|i| !i.is_directory) else {
            let text = if items.len() > 1 {
                let size: i64 = items.iter().filter(|i| !i.is_directory).map(|i| i.size).sum();
                tf("{count} selected · {size} in files", &[("count", &items.len().to_string()), ("size", &bytes(size))])
            } else {
                String::new()
            };
            let mut state = self.lock();
            state.preview = None;
            state.preview_text = text;
            return;
        };
        let info = format!("{} · {} · {}", item.name, bytes(item.size), date(item.modified));
        {
            let mut state = self.lock();
            state.preview = None;
            state.preview_text = info.clone();
        }
        if !is_image(&item.name) {
            return;
        }
        let key = format!("{side:?}|{}|{}|{}|{}", remote_location, item.full_path.display(), item.name, item.size);
        let cached = self.lock().preview_cache.iter().find(|(k, _)| *k == key).map(|(_, image)| image.clone());
        if let Some(image) = cached {
            let [width, height] = image.size;
            let mut state = self.lock();
            if state.preview_request == request {
                state.preview = Some(Preview { request, image });
                state.preview_text = format!("{info} · {width}×{height}");
            }
            drop(state);
            self.shared.ctx.request_repaint();
            return;
        }
        // Another file was selected meanwhile: it gets the next turn.
        if self.lock().preview_request != request || self.lock().preview_wanted.is_some() {
            return;
        }
        let data = match side {
            Side::Local if item.size <= 8 << 20 => tokio::fs::read(&item.full_path).await.ok(),
            Side::Local => None,
            Side::Remote => match (self.server(), gamedata_files::absolute(&remote_location)) {
                (Some(server), Ok(folder)) => server.read_file(&format!("{folder}/{}", item.name), 4 << 20).await.ok().flatten(),
                _ => None,
            },
        };
        let Some(data) = data else { return };
        // Not a readable image: only the text is shown.
        let Ok(Ok(decoded)) = tokio::task::spawn_blocking(move || image::load_from_memory(&data).map(|i| i.to_rgba8())).await else { return };
        let (width, height) = decoded.dimensions();
        let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], decoded.as_raw()));
        let mut state = self.lock();
        state.preview_cache.retain(|(k, _)| *k != key);
        state.preview_cache.push_back((key, image.clone()));
        while state.preview_cache.len() > PREVIEW_CACHE {
            state.preview_cache.pop_front();
        }
        if state.preview_request != request {
            return;
        }
        state.preview = Some(Preview { request, image });
        state.preview_text = format!("{info} · {width}×{height}");
        drop(state);
        self.shared.ctx.request_repaint();
    }
}

/// The last part of a path, ignoring a trailing separator.
fn top_name(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches(['/', '\\']);
    Path::new(if trimmed.is_empty() { text.as_ref() } else { trimmed }).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ---- The page ----

/// What a drag between the panes carries: the pane it started in (its selection is what is dragged).
#[derive(Debug, Clone, Copy)]
struct DragPayload(Side);

/// What the page does after drawing, with the lock released.
enum Action {
    Activate(Side),
    Click { side: Side, index: usize, ctrl: bool, shift: bool },
    Open(Side),
    OpenRow(Side, usize),
    Up(Side),
    DragStart(Side, usize),
    Drop { to: Side, from: Side, folder: String },
    FilesDropped { folder: String, paths: Vec<PathBuf> },
    Copy,
    Rename,
    NewFolder,
    Delete,
    Refresh,
    SelectAll(Side),
    Move(Side, isize, bool),
    Status(String),
    Back,
}

/// Where a pane and its visible rows were drawn (for drops, which are decided before the next frame is drawn).
#[derive(Clone)]
struct Geometry {
    rect: egui::Rect,
    rows: Vec<(usize, egui::Rect)>,
    offset: f32,
    view_height: f32,
}

impl Default for Geometry {
    fn default() -> Self {
        Self { rect: egui::Rect::NOTHING, rows: Vec::new(), offset: 0.0, view_height: 0.0 }
    }
}

impl Geometry {
    fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().find(|(_, r)| r.contains(pos)).map(|(i, _)| *i)
    }
}

pub struct AssetExplorerView {
    ops: Ops,
    loaded_for: Option<usize>,
    texture: Option<(u64, egui::TextureHandle)>,
    geometry: [Geometry; 2],
    versions: [u64; 2],
}

fn index(side: Side) -> usize {
    match side {
        Side::Local => 0,
        Side::Remote => 1,
    }
}

impl AssetExplorerView {
    pub fn new(shared: &Shared) -> Self {
        let state = State { active: Some(Side::Local), ..Default::default() };
        Self {
            ops: Ops { state: Arc::new(Mutex::new(state)), shared: shared.clone() },
            loaded_for: None,
            texture: None,
            geometry: Default::default(),
            versions: [0, 0],
        }
    }

    fn spawn<F, Fut>(&self, shared: &Shared, f: F)
    where
        F: FnOnce(Ops) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        shared.spawn(f(self.ops.clone()));
    }

    fn act(&mut self, action: Action, shared: &Shared) {
        match action {
            Action::Activate(side) => self.ops.lock().active = Some(side),
            Action::Click { side, index, ctrl, shift } => {
                let mut state = self.ops.lock();
                state.active = Some(side);
                state.pane(side).click(index, ctrl, shift);
            }
            Action::Open(side) => self.spawn(shared, move |ops| async move { ops.open_selected(side).await }),
            Action::OpenRow(side, index) => {
                {
                    let mut state = self.ops.lock();
                    state.active = Some(side);
                    state.pane(side).select_only(index);
                }
                self.spawn(shared, move |ops| async move { ops.open_selected(side).await });
            }
            Action::Up(side) => self.spawn(shared, move |ops| async move { ops.up(side).await }),
            Action::DragStart(side, index) => {
                let count = {
                    let mut state = self.ops.lock();
                    state.active = Some(side);
                    let pane = state.pane(side);
                    if !pane.selected.contains(&index) {
                        pane.select_only(index);
                    }
                    pane.selected_items().iter().filter(|i| !i.is_drive).count()
                };
                if count > 0 {
                    egui::DragAndDrop::set_payload(&shared.ctx, DragPayload(side));
                    let text = match side {
                        Side::Local => tf("Drag {count} item(s) to the server pane to upload (or onto a folder there).", &[("count", &count.to_string())]),
                        Side::Remote => tf("Drag {count} item(s) to the left pane to download.", &[("count", &count.to_string())]),
                    };
                    self.ops.show_status(text, false);
                }
            }
            Action::Drop { to, from, folder } => {
                let (local, remote, remote_location) = {
                    let state = self.ops.lock();
                    let local: Vec<PathBuf> = state.local.selected_items().into_iter().filter(|i| !i.is_drive).map(|i| i.full_path).collect();
                    let remote: Vec<String> = state.remote.selected_items().into_iter().map(|i| i.name).collect();
                    (local, remote, state.remote.location.clone())
                };
                match (from, to) {
                    (Side::Local, Side::Remote) => self.spawn(shared, move |ops| async move { ops.upload(local, folder).await }),
                    (Side::Remote, Side::Local) => self.spawn(shared, move |ops| async move { ops.download(remote, remote_location, folder).await }),
                    _ => {}
                }
            }
            Action::FilesDropped { folder, paths } => {
                let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
                if paths.is_empty() {
                    self.ops.show_status(t("Only files and folders on this computer can be dropped here.").to_string(), true);
                } else {
                    self.spawn(shared, move |ops| async move { ops.upload(paths, folder).await });
                }
            }
            Action::Copy => self.spawn(shared, |ops| async move { ops.copy().await }),
            Action::Rename => self.spawn(shared, |ops| async move { ops.rename().await }),
            Action::NewFolder => self.spawn(shared, |ops| async move { ops.new_folder().await }),
            Action::Delete => self.spawn(shared, |ops| async move { ops.delete().await }),
            Action::Refresh => self.spawn(shared, |ops| async move { ops.refresh().await }),
            Action::SelectAll(side) => self.ops.lock().pane(side).select_all(),
            Action::Move(side, delta, shift) => self.ops.lock().pane(side).move_cursor(delta, shift),
            Action::Status(text) => self.ops.show_status(text, false),
            Action::Back => {
                if !self.ops.lock().busy {
                    shared.request(Request::Go(Page::Dashboard));
                }
            }
        }
    }

    /// The keys of the C# KeyAsync, when no text field or question has the keyboard.
    fn keys(&self, ctx: &egui::Context, shared: &Shared, active: Side, busy: bool, actions: &mut Vec<Action>) {
        if ctx.wants_keyboard_input() || shared.dialogs.is_open() {
            return;
        }
        let page_rows = (self.geometry[index(active)].view_height / ROW_HEIGHT).floor().max(1.0) as isize;
        ctx.input_mut(|i| {
            use egui::{Key, Modifiers};
            let mut key = |modifiers: Modifiers, key: Key| i.consume_key(modifiers, key);
            if key(Modifiers::NONE, Key::Tab) {
                actions.push(Action::Activate(active.other()));
            }
            for (k, delta) in [(Key::ArrowDown, 1), (Key::ArrowUp, -1), (Key::PageDown, page_rows), (Key::PageUp, -page_rows), (Key::End, isize::MAX / 2), (Key::Home, -(isize::MAX / 2))] {
                if key(Modifiers::SHIFT, k) {
                    actions.push(Action::Move(active, delta, true));
                } else if key(Modifiers::NONE, k) {
                    actions.push(Action::Move(active, delta, false));
                }
            }
            if key(Modifiers::COMMAND, Key::A) {
                actions.push(Action::SelectAll(active));
            }
            if busy {
                return;
            }
            if key(Modifiers::COMMAND, Key::R) {
                actions.push(Action::Refresh);
            }
            if key(Modifiers::NONE, Key::Enter) {
                actions.push(Action::Open(active));
            }
            if key(Modifiers::NONE, Key::Backspace) {
                actions.push(Action::Up(active));
            }
            if key(Modifiers::NONE, Key::F5) {
                actions.push(Action::Copy);
            }
            if key(Modifiers::NONE, Key::F2) || key(Modifiers::NONE, Key::F6) {
                actions.push(Action::Rename);
            }
            if key(Modifiers::NONE, Key::F7) {
                actions.push(Action::NewFolder);
            }
            if key(Modifiers::NONE, Key::F8) || key(Modifiers::NONE, Key::Delete) {
                actions.push(Action::Delete);
            }
        });
    }

    /// Drags between the panes and files from the OS: the target under the pointer, its highlight, and the drop.
    fn drops(&self, ctx: &egui::Context, state: &State, actions: &mut Vec<Action>) -> Option<Side> {
        let pointer = ctx.input(|i| i.pointer.latest_pos());
        let target_at = |pos: egui::Pos2| [Side::Local, Side::Remote].into_iter().find(|s| self.geometry[index(*s)].rect.contains(pos));
        let folder_for = |side: Side, pos: Option<egui::Pos2>| {
            let pane = state.pane_ref(side);
            let row = pos.and_then(|p| self.geometry[index(side)].row_at(p)).and_then(|i| pane.items.get(i));
            drop_folder(side == Side::Local, &pane.location, row)
        };

        // Rows dragged from one pane to the other.
        if let Some(payload) = egui::DragAndDrop::payload::<DragPayload>(ctx) {
            let from = payload.0;
            let target = pointer.and_then(target_at).filter(|to| *to == from.other() && !state.busy);
            let folder = target.and_then(|to| folder_for(to, pointer));
            if let (Some(to), Some(folder)) = (target, folder) {
                if ctx.input(|i| i.pointer.any_released()) {
                    egui::DragAndDrop::clear_payload(ctx);
                    actions.push(Action::Drop { to, from, folder });
                    return None;
                }
                let text = match to {
                    Side::Local => tf("Drop to download into {folder}", &[("folder", &folder)]),
                    Side::Remote => tf("Drop to upload into {folder}", &[("folder", &gamedata_files::absolute(&folder).unwrap_or(folder.clone()))]),
                };
                if state.status != text {
                    actions.push(Action::Status(text));
                }
                return Some(to);
            }
            return None;
        }

        // Files from Explorer go to the server pane only. The pointer is not always known during an OS drag, so
        // without it the drop goes into the server folder on screen.
        let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
        let over_remote = pointer.filter(|p| self.geometry[1].rect.contains(*p));
        if !dropped.is_empty() && !state.busy {
            let folder = folder_for(Side::Remote, over_remote).unwrap_or_else(|| state.remote.location.clone());
            let paths: Vec<PathBuf> = dropped.into_iter().filter_map(|f| f.path).collect();
            actions.push(Action::FilesDropped { folder, paths });
            return None;
        }
        if hovering && !state.busy {
            return Some(Side::Remote);
        }
        None
    }

    fn connected_check(&mut self, shared: &Shared) {
        let id = shared.session().map(|s| Arc::as_ptr(&s) as *const () as usize);
        if id.is_some() && id != self.loaded_for {
            self.loaded_for = id;
            self.spawn(shared, |ops| async move { ops.connected().await });
        }
    }
}

/// The arrows of the C# texts, or ASCII ones when the font has no such glyph (the Ubuntu font lacks ⇄ → ←).
fn arrows(ui: &egui::Ui, text: &str) -> String {
    let font = egui::FontId::proportional(12.5);
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '⇄' | '→' | '←' if !ui.fonts(|f| f.has_glyph(&font, c)) => out.push_str(match c {
                '⇄' => "<->",
                '→' => "->",
                _ => "<-",
            }),
            _ => out.push(c),
        }
    }
    out
}

fn key_button(palette: &Palette, key: &str, label: &str) -> egui::Button<'static> {
    let mut job = egui::text::LayoutJob::default();
    job.append(key, 0.0, egui::TextFormat::simple(egui::FontId::new(11.5, egui::FontFamily::Name("bold".into())), palette.accent_text));
    job.append(label, 6.0, egui::TextFormat::simple(egui::FontId::proportional(12.5), palette.text));
    egui::Button::new(job).corner_radius(if theme::hotel() { 6 } else { 8 }).min_size(egui::vec2(0.0, 30.0))
}

/// One line of text cut with "…" to `width`.
fn truncated(ui: &egui::Ui, text: &str, font: egui::FontId, color: Color32, width: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.fonts(|f| f.layout_job(job))
}

/// Draws one pane; returns where it was drawn.
#[allow(clippy::too_many_arguments)]
fn draw_pane(
    ui: &mut egui::Ui,
    palette: &Palette,
    side: Side,
    pane: &mut Pane,
    active: bool,
    drop_target: bool,
    busy: bool,
    size: egui::Vec2,
    previous: &Geometry,
    actions: &mut Vec<Action>,
) -> Geometry {
    let local = side == Side::Local;
    let mut geometry = Geometry::default();
    let (outer, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    geometry.rect = outer;
    let fill = if drop_target { palette.accent_soft } else { palette.card };
    let stroke = egui::Stroke::new(2.0_f32, if active { palette.accent_text } else { palette.card_border });
    ui.painter().rect(outer, 10.0, fill, stroke, egui::StrokeKind::Inside);
    if ui.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| outer.contains(p))) {
        actions.push(Action::Activate(side));
    }

    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(outer.shrink2(egui::vec2(0.0, 8.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    let inner_width = outer.width();
    // Header and path.
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        icons::show(ui, if local { Icon::Home } else { Icon::Server }, 16.0, palette.text);
        ui.add_space(4.0);
        ui.label(RichText::new(if local { t("This computer") } else { t("Server · gamedata (locked)") }).size(13.0).strong());
    });
    ui.add_space(2.0);
    {
        let galley = truncated(ui, &pane.path_text, egui::FontId::monospace(12.5), palette.muted, inner_width - 24.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(inner_width, galley.size().y + 2.0), egui::Sense::hover());
        ui.painter().galley(rect.min + egui::vec2(12.0, 0.0), galley, palette.muted);
    }
    ui.add_space(4.0);
    // Column headings.
    let left = outer.left() + 14.0;
    let right = outer.right() - 22.0;
    {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(inner_width, 16.0), egui::Sense::hover());
        let font = egui::FontId::new(11.0, egui::FontFamily::Name("bold".into()));
        let y = rect.center().y;
        ui.painter().text(egui::pos2(left + COLUMN_ICON, y), egui::Align2::LEFT_CENTER, t("Name"), font.clone(), palette.subtle);
        ui.painter().text(egui::pos2(right - COLUMN_DATE, y), egui::Align2::RIGHT_CENTER, t("Size"), font.clone(), palette.subtle);
        ui.painter().text(egui::pos2(right, y), egui::Align2::RIGHT_CENTER, t("Modified"), font, palette.subtle);
    }

    // The rows.
    let summary_height = 20.0;
    let list_height = (ui.available_height() - summary_height).max(ROW_HEIGHT);
    let mut scroll = egui::ScrollArea::vertical().id_salt(("explorer", index(side))).auto_shrink([false, false]).max_height(list_height);
    if pane.scroll {
        pane.scroll = false;
        if let Some(cursor) = pane.cursor {
            let top = cursor as f32 * ROW_HEIGHT;
            let view = previous.view_height.max(ROW_HEIGHT);
            if top < previous.offset {
                scroll = scroll.vertical_scroll_offset(top);
            } else if top + ROW_HEIGHT > previous.offset + view {
                scroll = scroll.vertical_scroll_offset(top + ROW_HEIGHT - view);
            }
        }
    }
    let modifiers = ui.input(|i| i.modifiers);
    let mut rows = Vec::new();
    let output = scroll.show_rows(ui, ROW_HEIGHT, pane.items.len(), |ui, range| {
        for i in range {
            let item = &pane.items[i];
            let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::click_and_drag());
            rows.push((i, rect));
            let selected = pane.selected.contains(&i);
            if selected {
                ui.painter().rect_filled(rect.shrink2(egui::vec2(4.0, 0.0)), 5.0, if active { palette.selected } else { palette.hover });
            } else if response.hovered() {
                ui.painter().rect_filled(rect.shrink2(egui::vec2(4.0, 0.0)), 5.0, palette.hover);
            }
            if active && pane.cursor == Some(i) {
                ui.painter().rect_stroke(rect.shrink2(egui::vec2(4.0, 0.0)), 5.0, egui::Stroke::new(1.0_f32, palette.accent_text), egui::StrokeKind::Inside);
            }
            let icon = if item.is_parent {
                Icon::ArrowUp
            } else if item.is_drive {
                Icon::Database
            } else if item.is_directory {
                Icon::Folder
            } else if is_image(&item.name) {
                Icon::Image
            } else {
                Icon::File
            };
            let icon_color = if item.is_directory || item.is_parent || item.is_drive { palette.accent_text } else { palette.muted };
            let icon_rect = egui::Rect::from_min_size(egui::pos2(left, rect.center().y - 7.5), egui::vec2(15.0, 15.0));
            icons::image(icon, 15.0, icon_color).paint_at(ui, icon_rect);
            let has_date = !item.is_parent && !item.is_drive;
            let name_width = if item.is_parent { right - left - COLUMN_ICON } else if has_date { right - left - COLUMN_ICON - COLUMN_SIZE - COLUMN_DATE } else { right - left - COLUMN_ICON - COLUMN_SIZE };
            let font = if item.is_directory { egui::FontId::new(12.5, egui::FontFamily::Name("bold".into())) } else { egui::FontId::proportional(12.5) };
            let galley = truncated(ui, if item.is_parent { ".." } else { &item.name }, font, palette.text, name_width - 6.0);
            ui.painter().galley(egui::pos2(left + COLUMN_ICON, rect.center().y - galley.size().y / 2.0), galley, palette.text);
            if !item.is_parent {
                let size_text = if item.is_drive {
                    if item.size > 0 { tf("{size} free", &[("size", &bytes(item.size))]) } else { String::new() }
                } else if item.is_directory {
                    "<DIR>".to_string()
                } else {
                    bytes(item.size)
                };
                let size_right = if has_date { right - COLUMN_DATE } else { right };
                ui.painter().text(egui::pos2(size_right, rect.center().y), egui::Align2::RIGHT_CENTER, size_text, egui::FontId::proportional(12.0), palette.muted);
                if has_date {
                    ui.painter().text(egui::pos2(right, rect.center().y), egui::Align2::RIGHT_CENTER, date(item.modified), egui::FontId::proportional(12.0), palette.muted);
                }
            }
            if response.double_clicked() {
                actions.push(Action::OpenRow(side, i));
            } else if response.clicked() {
                actions.push(Action::Click { side, index: i, ctrl: modifiers.command, shift: modifiers.shift });
            } else if response.drag_started() && !busy && !item.is_parent && !item.is_drive {
                // The ".." and drive rows cannot be dragged.
                actions.push(Action::DragStart(side, i));
            }
        }
    });
    geometry.rows = rows;
    geometry.offset = output.state.offset.y;
    geometry.view_height = output.inner_rect.height();
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.add(egui::Label::new(RichText::new(pane.summary()).size(11.5).color(palette.subtle)).truncate());
    });
    geometry
}

impl View for AssetExplorerView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.connected_check(shared);
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        let state_arc = self.ops.state.clone();
        let mut state = state_arc.lock();
        let busy = state.busy;
        let active = state.active();
        self.keys(&ctx, shared, active, busy, &mut actions);
        let drop_target = self.drops(&ctx, &state, &mut actions);

        // Heading with Back.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 90.0).max(100.0));
                if let Some(session) = shared.session() {
                    let login = session.login();
                    let title = arrows(ui, &format!("{}  ⇄  {}@{}:{}", t("This computer"), login.user, login.host, gamedata_files::root()));
                    ui.label(RichText::new(title).size(13.0).strong());
                }
                ui.label(
                    RichText::new(tf(
                        "The server side is locked to {root}: uploads get owner www-data, folders 2775, files 664.",
                        &[("root", &gamedata_files::root())],
                    ))
                    .size(12.0)
                    .color(palette.muted),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                    actions.push(Action::Back);
                }
            });
        });
        ui.add_space(10.0);

        // The panes.
        let info_height = 100.0;
        let keys_height = 76.0;
        let width = ui.available_width();
        let pane_height = (ui.available_height() - info_height - keys_height - 30.0).max(220.0);
        let pane_width = (width - 12.0) / 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for side in [Side::Local, Side::Remote] {
                let previous = self.geometry[index(side)].clone();
                let pane = state.pane(side);
                let geometry = draw_pane(ui, palette, side, pane, active == side, drop_target == Some(side), busy, egui::vec2(pane_width, pane_height), &previous, &mut actions);
                self.geometry[index(side)] = geometry;
            }
        });
        ui.add_space(10.0);

        // The preview, the status and the progress.
        let preview_size = state.preview.as_ref().map(|p| (p.request, p.image.size));
        match (&state.preview, &self.texture) {
            (Some(preview), Some((request, _))) if *request == preview.request => {}
            (Some(preview), _) => {
                self.texture = Some((preview.request, ctx.load_texture("explorer-preview", (*preview.image).clone(), egui::TextureOptions::NEAREST)));
            }
            (None, _) => self.texture = None,
        }
        ui.horizontal(|ui| {
            egui::Frame::new().fill(palette.surface).stroke(egui::Stroke::new(1.0_f32, palette.card_border)).corner_radius(8).inner_margin(6).show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 88.0), egui::Sense::hover());
                if let (Some((_, texture)), Some((_, [w, h]))) = (&self.texture, preview_size) {
                    let scale = (rect.width() / w as f32).min(rect.height() / h as f32);
                    let shown = egui::Rect::from_center_size(rect.center(), egui::vec2(w as f32 * scale, h as f32 * scale));
                    ui.painter().image(texture.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
            });
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(&state.preview_text).size(12.0).color(palette.muted)).wrap());
                ui.add_space(4.0);
                let color = if state.status_error { palette.error_text } else { palette.muted };
                ui.add(egui::Label::new(RichText::new(&state.status).size(12.5).color(color)).wrap());
                if let Some(progress) = state.progress {
                    ui.add_space(4.0);
                    ui.add(egui::ProgressBar::new(progress).desired_height(6.0));
                }
            });
        });
        ui.add_space(10.0);

        // The keys.
        ui.horizontal_wrapped(|ui| {
            let copy_label = arrows(ui, if active == Side::Local { t("Upload →") } else { t("← Download") });
            let buttons: [(&str, &str, Action); 6] = [
                ("Enter", t("Open"), Action::Open(active)),
                ("F5", &copy_label, Action::Copy),
                ("F6", t("Rename"), Action::Rename),
                ("F7", t("New folder"), Action::NewFolder),
                ("F8", t("Delete"), Action::Delete),
                ("Ctrl+R", t("Refresh"), Action::Refresh),
            ];
            for (key, label, action) in buttons {
                if ui.add_enabled(!busy, key_button(palette, key, label)).clicked() {
                    actions.push(action);
                }
            }
            ui.label(
                RichText::new(t("Tab switches panes · Backspace goes up · Ctrl/Shift+click selects more · double-click opens · drag files between the panes, or from Explorer onto the server"))
                    .size(11.5)
                    .color(palette.subtle),
            );
        });

        // A preview when a pane's selection changed.
        let mut preview = None;
        for side in [Side::Local, Side::Remote] {
            let version = state.pane_ref(side).version;
            if self.versions[index(side)] != version {
                self.versions[index(side)] = version;
                preview = Some(side);
            }
        }
        let start_worker = match preview {
            Some(side) => {
                state.preview_wanted = Some(side);
                !std::mem::replace(&mut state.preview_running, true)
            }
            None => false,
        };
        drop(state);
        if start_worker {
            self.spawn(shared, |ops| async move { ops.preview_worker().await });
        }
        for action in actions {
            self.act(action, shared);
        }
        if egui::DragAndDrop::has_payload_of_type::<DragPayload>(&ctx) || busy {
            ctx.request_repaint();
        }
    }

    fn busy(&self) -> bool {
        self.ops.lock().busy
    }

    fn session_changed(&mut self, _shared: &Shared) {
        // The next time the page is on screen with a login, it opens the folders again.
        self.loaded_for = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, dir: bool) -> Item {
        Item { name: name.into(), is_directory: dir, size: 10, modified: None, full_path: PathBuf::from(format!("/x/{name}")), is_parent: false, is_drive: false }
    }

    #[test]
    fn sizes_and_rates() {
        assert_eq!(bytes(512), "1 KB");
        assert_eq!(bytes(5 << 20), "5 MB");
        assert_eq!(bytes(3 << 30), "3.0 GB");
        assert_eq!(rate(500.0), "500 B/s");
        assert_eq!(rate(2048.0), "2.0 KB/s");
        assert_eq!(rate(3.0 * 1024.0 * 1024.0), "3.00 MB/s");
    }

    #[test]
    fn going_up() {
        assert_eq!(remote_up(""), None);
        assert_eq!(remote_up("a"), Some((String::new(), "a".into())));
        assert_eq!(remote_up("a/b/c"), Some(("a/b".into(), "c".into())));
        assert_eq!(local_up(""), None);
        assert_eq!(local_up("/"), Some((String::new(), String::new())));
        let (parent, child) = local_up(&format!("{}{}", Path::new("/home").join("me").display(), "")).unwrap();
        assert_eq!(child, "me");
        assert!(parent.ends_with("home"));
        #[cfg(windows)]
        assert_eq!(local_up("C:\\"), Some((String::new(), String::new())));
    }

    #[test]
    fn drop_targets() {
        let folder = item("sub", true);
        let file = item("a.png", false);
        assert_eq!(drop_folder(false, "", Some(&folder)), Some("sub".into()));
        assert_eq!(drop_folder(false, "x", Some(&folder)), Some("x/sub".into()));
        assert_eq!(drop_folder(false, "x", Some(&file)), Some("x".into()));
        assert_eq!(drop_folder(false, "x", Some(&Item::parent())), Some("x".into()));
        assert_eq!(drop_folder(true, "", Some(&folder)), None);
        let mut drive = item("C:", true);
        drive.is_drive = true;
        drive.full_path = PathBuf::from("C:\\");
        assert_eq!(drop_folder(true, "", Some(&drive)), Some("C:\\".into()));
        assert_eq!(drop_folder(true, "/x", Some(&file)), Some("/x".into()));
        assert_eq!(drop_folder(true, "/x", Some(&folder)), Some(folder.full_path.to_string_lossy().into_owned()));
    }

    #[test]
    fn selection() {
        let mut pane = Pane::default();
        pane.apply(vec![Item::parent(), item("a", true), item("b", false), item("c", false)], "/x".into(), None);
        assert_eq!(pane.selected.iter().copied().collect::<Vec<_>>(), [0]);
        assert!(pane.selected_items().is_empty());
        pane.click(1, false, false);
        pane.click(3, false, true);
        assert_eq!(pane.selected_items().len(), 3);
        pane.click(2, true, false);
        assert_eq!(pane.selected_items().iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        pane.move_cursor(-1, false);
        assert_eq!(pane.current().unwrap().name, "a");
        pane.move_cursor(10, true);
        assert_eq!(pane.selected_items().len(), 3);
        pane.apply(pane.items.clone(), "/x".into(), Some(&["c".to_string()]));
        assert_eq!(pane.current().unwrap().name, "c");
        assert!(pane.summary().starts_with("1 folders, 2 files"));
    }

    #[test]
    fn names() {
        let existing = vec!["taken".to_string()];
        assert_eq!(name_problem("", false, &existing).as_deref(), Some("Enter a name."));
        assert_eq!(name_problem("taken", false, &existing).as_deref(), Some("taken already exists here."));
        assert_eq!(name_problem("ok.png", true, &existing), None);
        #[cfg(windows)]
        assert_eq!(name_problem("a?b", true, &existing).as_deref(), Some("That name has characters this computer does not allow."));
        assert_eq!(top_name(Path::new("/x/pack/")), "pack");
        assert!(is_image("A.PNG") && !is_image("a.txt"));
    }

    #[test]
    fn local_listing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Zeta")).unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        std::fs::write(dir.path().join("b.txt"), "12345").unwrap();
        std::fs::write(dir.path().join("A.png"), "1").unwrap();
        let items = load_local(&dir.path().to_string_lossy()).unwrap();
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["..", "alpha", "Zeta", "A.png", "b.txt"]);
        assert_eq!(local_size(dir.path()), 6);
    }
}
