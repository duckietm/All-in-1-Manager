//! The Plugins page: every plugin with its on/off switch, and the data packs to download or remove. A plugin that is
//! off leaves the sidebar, the Addons page and the dashboard; its pack (if downloaded) stays until removed.

use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Page, Shared};
use crate::i18n::{t, tf};
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
        open
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
