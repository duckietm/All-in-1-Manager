//! The Plugins page: every plugin with its on/off switch, and the data packs to download or remove. A plugin that is
//! off leaves the sidebar, the Addons page and the dashboard; its pack (if downloaded) stays until removed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use aio_plugin_host::display_safe;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Page, Shared};
use crate::i18n::{t, tf};
use crate::gui::dialogs::Choice;
use crate::plugins::external::{self, DevPending, DevRow, DevState, ExternalInfo, Pending};
use crate::plugins::{self, packs::Pack, Plugin, PLUGINS};

/// A pack's download, shared by the Plugins page and the "pack missing" card on a plugin's page.
#[derive(Debug, Clone, PartialEq)]
enum Download {
    Running { done: u64, total: u64 },
    Failed(String),
}

fn downloads() -> &'static Mutex<HashMap<&'static str, Download>> {
    static STATE: OnceLock<Mutex<HashMap<&'static str, Download>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// True while a pack downloads: navigation and closing wait.
pub fn busy() -> bool {
    downloads().lock().values().any(|d| matches!(d, Download::Running { .. }))
}

fn start_download(pack: &'static Pack, shared: &Shared) {
    {
        let mut all = downloads().lock();
        if matches!(all.get(pack.id), Some(Download::Running { .. })) {
            return;
        }
        all.insert(pack.id, Download::Running { done: 0, total: 0 });
    }
    let ctx = shared.ctx.clone();
    let status = shared.clone();
    shared.spawn(async move {
        let progress = |done: u64, total: u64| {
            downloads().lock().insert(pack.id, Download::Running { done, total });
            ctx.request_repaint();
        };
        match pack.download(progress).await {
            Ok(()) => {
                downloads().lock().remove(pack.id);
                status.set_status(&tf("The {pack} pack is installed.", &[("pack", t(pack.title))]));
            }
            Err(error) => {
                downloads().lock().insert(pack.id, Download::Failed(format!("{error:#}")));
                status.set_status(&tf("The {pack} pack could not be downloaded.", &[("pack", t(pack.title))]));
            }
        }
    });
}

/// The pack's state, Download / Remove buttons and progress. Used on the Plugins page and on the plugin's own page.
fn pack_controls(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, pack: &'static Pack) {
    let state = downloads().lock().get(pack.id).cloned();
    match state {
        Some(Download::Running { done, total }) => {
            let text = if total > 0 {
                tf("Downloading {done} of {total} MB", &[("done", &format!("{:.1}", done as f64 / 1048576.0)), ("total", &format!("{:.1}", total as f64 / 1048576.0))])
            } else {
                t("Downloading…").to_string()
            };
            ui.label(RichText::new(text).size(12.5).color(palette.muted));
            if total > 0 {
                ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_height(8.0));
            } else {
                widgets::indeterminate_bar(ui, 8.0, palette.muted);
            }
            return;
        }
        Some(Download::Failed(error)) => {
            widgets::result_banner(ui, palette, ResultKind::Error, t("The download failed"), &error);
            ui.add_space(6.0);
        }
        None => {}
    }
    ui.horizontal(|ui| {
        if let Some(location) = pack.location() {
            ui.label(RichText::new(format!("✔ {}", t("Installed"))).size(12.5).color(palette.success_text));
            ui.label(RichText::new(location.display().to_string()).size(11.5).color(palette.subtle)).on_hover_text(t("Where the pack is"));
            if pack.is_removable() && ui.add(widgets::secondary_button(t("Remove"))).on_hover_text(t("Delete the downloaded pack; download it again at any time")).clicked() {
                match pack.remove() {
                    Ok(()) => shared.set_status(&tf("The {pack} pack is removed.", &[("pack", t(pack.title))])),
                    Err(error) => shared.set_status(&format!("{error:#}")),
                }
            }
            if pack.is_removable() && ui.add(widgets::secondary_button(t("Download again"))).clicked() {
                start_download(pack, shared);
            }
        } else {
            ui.label(RichText::new(tf("Not downloaded ({size})", &[("size", pack.size)])).size(12.5).color(palette.warning_text));
            if ui.add(widgets::primary_button(t("Download"))).on_hover_text(t("From the program's GitHub release, checked against its published SHA-256")).clicked() {
                downloads().lock().remove(pack.id);
                start_download(pack, shared);
            }
        }
    });
}

/// On a plugin's own page: a card when its pack is missing. True when the page can be used.
pub fn pack_ready(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, page: Page) -> bool {
    let Some(pack) = plugins::plugin_for(page).and_then(|p| p.pack) else { return true };
    if pack.is_installed() {
        return true;
    }
    widgets::card(ui, palette, egui::Margin::symmetric(28, 24), |ui| {
        ui.label(RichText::new(tf("This page needs the {pack} pack", &[("pack", t(pack.title))])).size(17.0).strong());
        widgets::muted(
            ui,
            palette,
            t("It is not built into the program any more, so the program stays small: download it once, it is kept in the configuration folder."),
            13.0,
        );
        ui.add_space(8.0);
        pack_controls(ui, palette, shared, pack);
    });
    false
}

#[derive(Default)]
pub struct PluginsView;

impl PluginsView {
    /// Returns a page to open (a plugin's Open button).
    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) -> Option<Page> {
        let mut open = None;
        widgets::muted(
            ui,
            palette,
            t("Switch off what you do not use: its pages leave the menus and the dashboard. Nothing is removed from the server, and you can switch it on again at any time."),
            13.5,
        );
        ui.add_space(12.0);
        for plugin in PLUGINS {
            if let Some(page) = plugin_card(ui, palette, shared, plugin) {
                open = Some(page);
            }
            ui.add_space(10.0);
        }
        ui.add_space(6.0);
        installed_section(ui, palette, shared);
        consent_window(ui.ctx(), palette, shared);
        open
    }
}

/// What waits for the user's consent: a package from a file, or a developer folder.
enum Consent {
    Install(Box<Pending>),
    Folder(Box<DevPending>),
}

/// The thing that waits for the user's consent, if any.
fn consent() -> &'static Mutex<Option<Arc<Consent>>> {
    static STATE: OnceLock<Mutex<Option<Arc<Consent>>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Plugin-supplied text (names, ids, versions inside error messages) made safe to show.
fn safe(text: &str) -> String {
    display_safe(text, 300)
}

/// True from the click on "Install from file" until the file is checked: no second picker meanwhile.
fn picking() -> &'static AtomicBool {
    static STATE: AtomicBool = AtomicBool::new(false);
    &STATE
}

/// Plugins whose Remove is being asked or done.
fn removing() -> &'static Mutex<HashSet<String>> {
    static STATE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Pick a file, read and check it off the window's thread, then ask.
fn start_install(shared: &Shared) {
    if consent().lock().is_some() || picking().swap(true, Ordering::AcqRel) {
        return;
    }
    let status = shared.clone();
    shared.spawn(async move {
        let picked = rfd::AsyncFileDialog::new().set_title(t("Install a plugin from a file")).add_filter(t("Plugin packages"), &["aiop"]).pick_file().await;
        let Some(file) = picked else {
            picking().store(false, Ordering::Release);
            return;
        };
        let path = file.path().to_path_buf();
        let checked = tokio::task::spawn_blocking(move || external::plugins().prepare(&path)).await;
        match checked {
            Ok(Ok(pending)) => *consent().lock() = Some(Arc::new(Consent::Install(Box::new(pending)))),
            Ok(Err(error)) => status.set_status(&tf("This file cannot be installed: {error}", &[("error", &safe(&format!("{error:#}")))])),
            Err(error) => status.set_status(&tf("This file cannot be installed: {error}", &[("error", &error.to_string())])),
        }
        picking().store(false, Ordering::Release);
    });
}

fn finish_install(shared: &Shared, consent: Arc<Consent>) {
    let status = shared.clone();
    shared.spawn(async move {
        let Consent::Install(pending) = &*consent else { return };
        let pending: Pending = (**pending).clone();
        match tokio::task::spawn_blocking(move || external::plugins().commit(&pending)).await {
            Ok(Ok(manifest)) => status.set_status(&tf("{plugin} {version} is installed.", &[("plugin", &display_safe(&manifest.name, 80)), ("version", &display_safe(&manifest.version, 40))])),
            Ok(Err(error)) => status.set_status(&tf("The plugin could not be installed: {error}", &[("error", &safe(&format!("{error:#}")))])),
            Err(error) => status.set_status(&tf("The plugin could not be installed: {error}", &[("error", &error.to_string())])),
        }
    });
}

fn start_remove(shared: &Shared, info: ExternalInfo) {
    if !removing().lock().insert(info.manifest.id.clone()) {
        return;
    }
    let status = shared.clone();
    shared.spawn(async move {
        let manifest = info.manifest;
        let name = display_safe(&manifest.name, 80);
        remove_confirmed(&status, &manifest.id, &name).await;
        removing().lock().remove(&manifest.id);
    });
}

async fn remove_confirmed(status: &Shared, id: &str, name: &str) {
    let options = vec![Choice::new(t("Remove"), t("Delete the plugin and its data."), true).primary(), Choice::new(t("Cancel"), t("Change nothing."), false)];
    let message = tf("{plugin} is removed together with its private data folder: whatever it saved there is deleted too. This cannot be undone.", &[("plugin", name)]);
    if !status.dialogs.choose(t("Remove the plugin"), name, &message, false, options).await {
        return;
    }
    // The question was open for a while: the plugin may be gone already.
    if !external::plugins().list().iter().any(|i| i.manifest.id == id) {
        return;
    }
    let owned = id.to_string();
    match tokio::task::spawn_blocking(move || external::plugins().remove(&owned)).await {
        Ok(Ok(())) => {
            plugins::set_enabled(&external::settings_id(id), true);
            status.set_status(&tf("{plugin} is removed.", &[("plugin", name)]));
        }
        Ok(Err(error)) => status.set_status(&safe(&format!("{error:#}"))),
        Err(error) => status.set_status(&error.to_string()),
    }
}

fn installed_section(ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Installed plugins"), 16.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::primary_button(t("Install from file …"))).on_hover_text(t("Choose a .aiop package")).clicked() {
                start_install(shared);
            }
        });
    });
    widgets::muted(ui, palette, t("Plugins made by other people. They run in a sandbox and can only do what you allow here."), 12.5);
    ui.add_space(8.0);
    let list: Vec<ExternalInfo> = external::plugins().list().into_iter().filter(|i| !i.dev).collect();
    if list.is_empty() {
        widgets::muted(ui, palette, t("No plugin is installed."), 12.5);
    }
    for info in list {
        external_card(ui, palette, shared, &info);
        ui.add_space(10.0);
    }
    if external::plugins().developer_enabled() {
        developer_section(ui, palette, shared);
    }
}

/// Developer mode: the plugin folders, each with its state.
fn developer_section(ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(t("Developer plugins"), 16.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::primary_button(t("Load a plugin folder …"))).on_hover_text(t("A folder with manifest.json and plugin.wasm")).clicked() {
                start_add_folder(shared);
            }
        });
    });
    widgets::muted(
        ui,
        palette,
        t("Plugins run straight from your folder, without packing a .aiop. They are not verified: the same warnings and permissions apply as for a package from a file."),
        12.5,
    );
    ui.add_space(8.0);
    let rows = external::plugins().dev_rows();
    if rows.is_empty() {
        widgets::muted(ui, palette, t("No plugin folder is loaded."), 12.5);
    }
    for row in rows {
        match &row.state {
            DevState::Ready(info) => external_card(ui, palette, shared, info),
            DevState::NeedsConsent(manifest) => dev_review_card(ui, palette, shared, &row, manifest),
            DevState::Problem(text) => dev_problem_card(ui, palette, shared, &row, text),
        }
        ui.add_space(10.0);
    }
}

/// Pick a folder, read and check it off the window's thread, then ask.
fn start_add_folder(shared: &Shared) {
    if consent().lock().is_some() || picking().swap(true, Ordering::AcqRel) {
        return;
    }
    let status = shared.clone();
    shared.spawn(async move {
        let picked = rfd::AsyncFileDialog::new().set_title(t("Load a plugin folder")).pick_folder().await;
        if let Some(folder) = picked {
            review_folder(&status, folder.path().to_path_buf()).await;
        }
        picking().store(false, Ordering::Release);
    });
}

/// Reads the folder and puts the consent window up.
async fn review_folder(status: &Shared, folder: PathBuf) {
    match tokio::task::spawn_blocking(move || external::plugins().prepare_dev(&folder)).await {
        Ok(Ok(pending)) => *consent().lock() = Some(Arc::new(Consent::Folder(Box::new(pending)))),
        Ok(Err(error)) => status.set_status(&tf("This folder cannot be loaded: {error}", &[("error", &safe(&format!("{error:#}")))])),
        Err(error) => status.set_status(&tf("This folder cannot be loaded: {error}", &[("error", &error.to_string())])),
    }
}

fn finish_folder(shared: &Shared, pending: DevPending) {
    let status = shared.clone();
    shared.spawn(async move {
        let name = display_safe(&pending.manifest.name, 80);
        match tokio::task::spawn_blocking(move || external::plugins().accept_dev(&pending)).await {
            Ok(Ok(())) => {
                external::save_dev_folders();
                status.set_status(&tf("{plugin} is loaded from its folder.", &[("plugin", &name)]));
            }
            Ok(Err(error)) => status.set_status(&tf("This folder cannot be loaded: {error}", &[("error", &safe(&format!("{error:#}")))])),
            Err(error) => status.set_status(&tf("This folder cannot be loaded: {error}", &[("error", &error.to_string())])),
        }
    });
}

fn forget_folder(shared: &Shared, folder: &Path) {
    external::plugins().forget_dev(folder);
    external::save_dev_folders();
    shared.set_status(t("The folder is taken off the list. Its files are not touched."));
}

/// Reload on a developer plugin's card: re-reads the folder; a plugin that asks for more is off until the user agrees.
fn reload_folder(shared: &Shared, id: &str) {
    let (status, id) = (shared.clone(), id.to_string());
    shared.spawn(async move {
        match tokio::task::spawn_blocking(move || external::plugins().reload_dev(&id)).await {
            Ok(Ok(external::DevReload::Reloaded)) => status.set_status(t("Reloaded from the folder.")),
            Ok(Ok(external::DevReload::NeedsConsent)) => status.set_status(t("The plugin asks for more permissions: review them below.")),
            Ok(Err(error)) => status.set_status(&tf("The developer folder cannot be read: {error}", &[("error", &safe(&format!("{error:#}")))])),
            Err(error) => status.set_status(&error.to_string()),
        }
    });
}

/// A folder whose plugin asks for permissions the user has not agreed to (yet, or since they grew).
fn dev_review_card(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, row: &DevRow, manifest: &aio_plugin_host::Manifest) {
    widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(widgets::title_text(&display_safe(&manifest.name, 60), 16.0));
                    not_verified(ui, palette, t("Developer"));
                });
                widgets::muted(ui, palette, &safe(&row.folder.display().to_string()), 12.0);
                ui.label(RichText::new(t("It is off until you agree to its permissions.")).size(12.5).color(palette.warning_text));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::secondary_button(t("Remove from list"))).clicked() {
                    forget_folder(shared, &row.folder);
                }
                if ui.add(widgets::primary_button(t("Review permissions"))).clicked() && consent().lock().is_none() {
                    let (status, folder) = (shared.clone(), row.folder.clone());
                    shared.spawn(async move { review_folder(&status, folder).await });
                }
            });
        });
    });
}

fn dev_problem_card(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, row: &DevRow, problem: &str) {
    widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::muted(ui, palette, &safe(&row.folder.display().to_string()), 12.5);
                ui.label(RichText::new(safe(problem)).size(12.5).color(palette.error_text));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::secondary_button(t("Remove from list"))).clicked() {
                    forget_folder(shared, &row.folder);
                }
                if ui.add(widgets::secondary_button(t("Check again"))).clicked() {
                    shared.spawn(async {
                        let _ = tokio::task::spawn_blocking(|| {
                            let plugins = external::plugins();
                            plugins.set_developer(true, &plugins.dev_folders());
                        })
                        .await;
                    });
                }
            });
        });
    });
}

fn not_verified(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.5).strong().color(palette.error_text));
}

fn external_card(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, info: &ExternalInfo) {
    let manifest = &info.manifest;
    widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
        ui.horizontal(|ui| {
            let key = external::settings_id(&manifest.id);
            let mut on = plugins::is_enabled(&key);
            if widgets::switch(ui, palette, &mut on).on_hover_text(if on { t("On: click to switch it off") } else { t("Off: click to switch it on") }).changed() {
                plugins::set_enabled(&key, on);
                let state = if on { t("on") } else { t("off") };
                shared.set_status(&tf("{plugin} is {state}.", &[("plugin", &display_safe(&manifest.name, 80)), ("state", state)]));
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(widgets::title_text(&display_safe(&manifest.name, 60), 16.0));
                    ui.label(RichText::new(format!("{} · {}", display_safe(&manifest.version, 40), display_safe(&manifest.author, 60))).size(12.5).color(palette.muted));
                    if !info.verified {
                        not_verified(ui, palette, t("Not verified"));
                    }
                    if info.dev {
                        not_verified(ui, palette, t("Developer"));
                    }
                });
                widgets::muted(ui, palette, &display_safe(&manifest.description, 300), 12.5);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if info.dev {
                    if ui.add(widgets::secondary_button(t("Remove from list"))).on_hover_text(t("Take the folder off the list; its files and the plugin's data stay")).clicked() {
                        let ready = |r: &DevRow| matches!(&r.state, DevState::Ready(i) if i.manifest.id == manifest.id);
                        if let Some(row) = external::plugins().dev_rows().into_iter().find(ready) {
                            forget_folder(shared, &row.folder);
                        }
                    }
                    if ui.add(widgets::secondary_button(t("Reload"))).on_hover_text(t("Read manifest.json and the wasm file again")).clicked() {
                        reload_folder(shared, &manifest.id);
                    }
                } else if ui.add(widgets::secondary_button(t("Remove"))).on_hover_text(t("Delete the plugin and its data")).clicked() {
                    start_remove(shared, info.clone());
                }
            });
        });
        egui::CollapsingHeader::new(RichText::new(tf("Permissions ({count})", &[("count", &manifest.permissions.len().to_string())])).size(12.5)).id_salt(("ext-permissions", &manifest.id)).show(ui, |ui| {
            if manifest.permissions.is_empty() {
                widgets::muted(ui, palette, t("It asks for no permission."), 12.5);
            }
            for permission in &manifest.permissions {
                let color = if external::is_sensitive(permission) { palette.warning_text } else { palette.muted };
                ui.label(RichText::new(format!("• {}", external::permission_text(permission))).size(12.5).color(color));
            }
        });
    });
}

/// The consent: shown over the page until the user agrees or cancels. Nothing is written or run before.
fn consent_window(ctx: &egui::Context, palette: &Palette, shared: &Shared) {
    let Some(waiting) = consent().lock().clone() else { return };
    let (manifest, is_new): (&aio_plugin_host::Manifest, Box<dyn Fn(&aio_plugin_host::Permission) -> bool + '_>) = match &*waiting {
        Consent::Install(pending) => (&pending.package.manifest, Box::new(|p| pending.is_new_permission(p))),
        Consent::Folder(pending) => (&pending.manifest, Box::new(|p| pending.is_new_permission(p))),
    };
    let from_folder = matches!(&*waiting, Consent::Folder(_));
    let (mut agree, mut cancel) = (false, false);
    let title = if from_folder { t("Load this plugin folder?") } else { t("Install this plugin?") };
    egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.label(widgets::title_text(&display_safe(&manifest.name, 60), 17.0));
        match &*waiting {
            Consent::Install(pending) if pending.previous.is_some() => {
                let old = pending.previous.as_ref().expect("checked");
                ui.label(tf("This updates the installed plugin from version {old} to {new}.", &[("old", &display_safe(&old.version, 40)), ("new", &display_safe(&manifest.version, 40))]));
            }
            _ => {
                ui.label(tf("Version {version}", &[("version", &display_safe(&manifest.version, 40))]));
            }
        }
        ui.label(tf("By {author}", &[("author", &display_safe(&manifest.author, 60))]));
        if let Consent::Install(pending) = &*waiting {
            if let (true, Some(old)) = (pending.author_changed(), pending.previous.as_ref()) {
                ui.add_space(4.0);
                ui.label(RichText::new(t("Different author")).strong().color(palette.error_text));
                ui.label(
                    RichText::new(tf(
                        "The installed plugin is by {author}. This package is treated as another plugin: it does not get the installed plugin's private data, which is kept aside.",
                        &[("author", &display_safe(&old.author, 60))],
                    ))
                    .color(palette.error_text),
                );
            }
        }
        ui.add_space(6.0);
        match &*waiting {
            Consent::Install(_) => not_verified(ui, palette, t("Not verified — installed from a file")),
            Consent::Folder(pending) => {
                not_verified(ui, palette, t("Developer — not verified"));
                widgets::muted(ui, palette, &tf("Runs straight from the folder {folder}", &[("folder", &safe(&pending.folder.display().to_string()))]), 12.0);
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new(t("This plugin will be able to:")).strong());
        // The sensitive ones first, and the list scrolls, so Install and Cancel stay on screen however long it is.
        let mut listed: Vec<&aio_plugin_host::Permission> = manifest.permissions.iter().collect();
        listed.sort_by_key(|p| !external::is_sensitive(p));
        egui::ScrollArea::vertical().id_salt("consent-permissions").max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
            for permission in listed {
                let color = if external::is_sensitive(permission) { palette.warning_text } else { ui.visuals().text_color() };
                let mut text = format!("• {}", external::permission_text(permission));
                if is_new(permission) {
                    text = format!("{text}  [{}]", t("new in this version"));
                }
                ui.label(RichText::new(text).color(color));
            }
        });
        if manifest.permissions.is_empty() {
            widgets::muted(ui, palette, t("It asks for no permission."), 12.5);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            agree = ui.add(widgets::primary_button(if from_folder { t("Load") } else { t("Install") })).clicked();
            cancel = ui.add(widgets::secondary_button(t("Cancel"))).clicked();
        });
    });
    drop(is_new);
    if agree || cancel {
        let taken = consent().lock().take();
        if let (true, Some(taken)) = (agree, taken) {
            match &*taken {
                Consent::Install(_) => finish_install(shared, taken.clone()),
                Consent::Folder(pending) => finish_folder(shared, (**pending).clone()),
            }
        }
    }
}

fn plugin_card(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, plugin: &'static Plugin) -> Option<Page> {
    let mut open = None;
    widgets::card(ui, palette, egui::Margin::symmetric(20, 14), |ui| {
        ui.horizontal(|ui| {
            let mut on = plugins::is_enabled(plugin.id);
            if widgets::switch(ui, palette, &mut on).on_hover_text(if on { t("On: click to switch it off") } else { t("Off: click to switch it on") }).changed() {
                plugins::set_enabled(plugin.id, on);
                let state = if on { t("on") } else { t("off") };
                shared.set_status(&tf("{plugin} is {state}.", &[("plugin", t(plugin.title)), ("state", state)]));
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(widgets::title_text(t(plugin.title), 16.0));
                widgets::muted(ui, palette, t(plugin.description), 12.5);
            });
            if on {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(widgets::secondary_button(t("Open"))).clicked() {
                        open = plugin.pages.first().copied();
                    }
                });
            }
        });
        if let Some(pack) = plugin.pack {
            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new(tf("Data pack: {pack}", &[("pack", t(pack.title))])).size(12.5).strong());
            pack_controls(ui, palette, shared, pack);
        }
    });
    open
}
