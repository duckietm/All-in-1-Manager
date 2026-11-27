//! The Merge a rip tab: a folder of the Catalogue ripper fitted into the hotel's catalogue (normal and Builders Club).
//! Pages with the same path are joined (only offers are added there), the others go under "Imported from <hotel>",
//! offers whose furni the page already sells are skipped (`aio_catalog_rip::merge_plan`). In the same preview, the
//! furni interactions the rip's furni should have, from the owner's bundles (`aio_remote::catalog_merge`). Apply
//! writes it all in one transaction, recorded in the history so it can be undone there.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aio_catalog_rip::manager::ManagerStats;
use aio_catalog_rip::RipOptions;
use aio_remote::catalog_merge;
use eframe::egui::{self, RichText};

use super::{small, Action, Ctl, State, Status};
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::i18n::{t, tf};
use crate::views::catalog_rip::SessionItems;

/// The tab's fields and the merge previewed.
#[derive(Default)]
pub(super) struct MergeTool {
    /// The rip folder, as typed or chosen.
    pub folder: String,
    /// The bundled/furniture folder (the setting), read once.
    pub furniture: Option<String>,
    pub preview: Option<MergePreview>,
}

/// A merge read and worked out; nothing is written yet.
pub(super) struct MergePreview {
    pub folder: PathBuf,
    pub hotel: String,
    pub stats: ManagerStats,
    pub notes: Vec<String>,
    pub checked_interactions: bool,
    pub result: catalog_merge::Preview,
}

impl MergeTool {
    pub fn furniture(&mut self) -> &mut String {
        self.furniture.get_or_insert_with(|| aio_core::settings::InstallerSettings::load().merge_furniture.unwrap_or_default())
    }
}

/// Saves the bundled/furniture folder setting.
fn save_furniture(folder: &str) {
    let mut settings = aio_core::settings::InstallerSettings::load();
    let value = Some(folder.trim().to_string()).filter(|f| !f.is_empty());
    if settings.merge_furniture != value {
        settings.merge_furniture = value;
        let _ = settings.save();
    }
}

impl State {
    /// Opens the tab with this rip folder, to preview as soon as the database is open (the ripper's "Import into the
    /// hotel").
    pub(super) fn open_merge(&mut self, folder: PathBuf) {
        self.tab = super::Tab::Merge;
        self.merge.folder = folder.display().to_string();
        self.merge.preview = None;
        self.pending_merge = Some(folder);
    }
}

impl Ctl {
    pub(super) async fn pick_rip(self) {
        let Some(folder) = rfd::AsyncFileDialog::new().set_title(t("Choose the rip folder")).pick_folder().await else { return };
        let folder = folder.path().to_path_buf();
        self.lock().merge.folder = folder.display().to_string();
        self.preview_merge(folder).await;
    }

    pub(super) async fn pick_furniture(self) {
        let Some(folder) = rfd::AsyncFileDialog::new().set_title(t("Your bundled/furniture folder")).pick_folder().await else { return };
        let folder = folder.path().display().to_string();
        save_furniture(&folder);
        let mut state = self.lock();
        state.merge.furniture = Some(folder);
        state.merge.preview = None;
    }

    /// Reads the hotel's catalogue and items_base, merges the rip into it and shows what would change.
    pub(super) async fn preview_merge(self, folder: PathBuf) {
        let Some((generation, session, database)) = self.session() else { return };
        let furniture = {
            let mut state = self.lock();
            let furniture = state.merge.furniture().trim().to_string();
            save_furniture(&furniture);
            Some(PathBuf::from(furniture)).filter(|f| !f.as_os_str().is_empty())
        };
        if let Some(folder) = furniture.as_ref().filter(|f| !f.is_dir()) {
            let text = tf("{folder} is not a folder: choose your bundled/furniture folder, or empty the field to merge without checking interactions.", &[("folder", &folder.display().to_string())]);
            self.with(generation, |s| s.status = Status::error(text));
            return;
        }
        self.with(generation, |s| {
            s.busy = true;
            s.merge.preview = None;
            s.status = Status::ok(t("Reading the hotel's catalogue and items_base, then merging the rip ...").into());
        });
        let result = merge(session, &database, &folder, furniture.as_deref()).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(preview) => {
                    s.status = Status::ok(tf(
                        "{changes} catalogue changes and {interactions} interaction changes. Check them, then Apply.",
                        &[("changes", &preview.result.changes.len().to_string()), ("interactions", &preview.result.interactions.len().to_string())],
                    ));
                    s.merge.preview = Some(preview);
                }
                Err(error) => s.status = Status::error(tf("The rip cannot be merged: {error}", &[("error", &format!("{error:#}"))])),
            }
        });
    }

    /// Asks first, then writes the previewed merge.
    pub(super) async fn apply_merge(self) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((hotel, changes, interactions, history)) =
            self.lock().merge.preview.as_ref().map(|p| (p.hotel.clone(), p.result.changes.len(), p.result.interactions.len(), p.result.history))
        else {
            return;
        };
        let options = vec![Choice::new(t("Apply"), t("Write the changes to the hotel database."), true).primary(), Choice::new(t("Cancel"), t("Change nothing."), false)];
        let mut message = tf(
            "{changes} pages and offers are created and {interactions} furni interactions changed, in one go. Your own pages and offers are not changed.",
            &[("changes", &changes.to_string()), ("interactions", &interactions.to_string())],
        );
        message.push(' ');
        message.push_str(if history { t("It is recorded in the history, so it can be undone there.") } else { t("This database has no catalogue history: it cannot be undone. Make a backup first.") });
        if !self.shared.dialogs.choose(t("Merge a rip"), &hotel, &message, false, options).await {
            return;
        }
        let Some(preview) = self.lock().merge.preview.take() else { return };
        self.with(generation, |s| s.busy = true);
        let summary = format!("Merge of {}: {} pages and offers, {} furni interactions", preview.hotel, changes, interactions);
        let result = catalog_merge::apply(&session, &database, &preview.result, &summary).await;
        self.with(generation, |s| {
            s.busy = false;
            match result {
                Ok(()) => {
                    s.unpublished = true;
                    s.history = None;
                    s.status = Status::ok(tf("The rip of {hotel} is merged. Publish to show it in the hotel.", &[("hotel", &preview.hotel)]));
                }
                Err(error) => {
                    s.status = Status::error(error.to_string());
                    s.merge.preview = Some(preview);
                }
            }
        });
    }
}

/// The merge of the rip in `folder` into the hotel's catalogue, previewed.
async fn merge(session: Arc<aio_remote::sql_session::SqlSession>, database: &str, folder: &Path, furniture: Option<&Path>) -> anyhow::Result<MergePreview> {
    let items = SessionItems { session: session.clone(), database: database.to_string() };
    let log = |_: &str| {};
    let plan = aio_catalog_rip::merge_plan(folder, &RipOptions::default(), &items, &log).await?;
    let result = catalog_merge::preview(&session, database, &plan.text, &plan.items, furniture).await?;
    Ok(MergePreview { folder: folder.to_path_buf(), hotel: plan.hotel, stats: plan.stats, notes: plan.notes, checked_interactions: furniture.is_some(), result })
}

fn change_rows(ui: &mut egui::Ui, palette: &Palette, changes: &[aio_remote::catalog_journal::Change]) {
    for change in changes.iter().take(400) {
        ui.horizontal_wrapped(|ui| {
            if change.entity.builders {
                ui.label(RichText::new("BC").size(11.0).strong().color(egui::Color32::from_rgb(226, 132, 10)));
            }
            let what = if change.entity.page { t("Page") } else { t("Offer") };
            ui.label(RichText::new(format!("{what} #{} {}", change.entity.id, change.name())).size(12.0).strong());
            ui.label(RichText::new(&change.operation).size(11.0).monospace().color(palette.muted));
        });
    }
    if changes.len() > 400 {
        widgets::muted(ui, palette, &tf("... and {count} more.", &[("count", &(changes.len() - 400).to_string())]), 11.5);
    }
}

fn lines(ui: &mut egui::Ui, palette: &Palette, list: &[&String]) {
    for line in list.iter().take(400) {
        ui.label(RichText::new(line.as_str()).size(11.5).monospace().color(palette.muted));
    }
    if list.len() > 400 {
        widgets::muted(ui, palette, &tf("... and {count} more.", &[("count", &(list.len() - 400).to_string())]), 11.5);
    }
}

/// The Merge a rip tab.
pub(super) fn show(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let ready = s.session.is_some() && !busy;
    ui.label(widgets::title_text(t("Merge a rip"), 17.0));
    widgets::muted(
        ui,
        palette,
        t("Fits a catalogue copied with the Catalogue ripper into this hotel's catalogue, normal and Builders Club: a ripped page with the same path as one of yours (the same captions from the root) is joined, and only offers are added to it; the other pages go under a new page \"Imported from <hotel>\"; an offer whose furni that page already sells is skipped. Your own pages and offers are never changed. Merging the same rip again adds nothing."),
        12.0,
    );
    ui.add_space(6.0);
    egui::Grid::new("catalog-merge-options").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(t("Rip folder"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut s.merge.folder).hint_text(t("The folder the ripper saved (with packets/)")).desired_width(420.0));
            if ui.add_enabled(ready, small(t("Choose ..."))).clicked() {
                actions.push(Action::PickRip);
            }
        });
        ui.end_row();
        let hint = t("Optional. Your hotel's bundled/furniture folder on this computer (the .nitro or .hab files, as in Nitro-Files): the furni interactions are read from it. Empty: interactions are not checked.");
        ui.label(t("Furniture bundles")).on_hover_text(hint);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(s.merge.furniture()).hint_text(t("Empty: interactions are not checked")).desired_width(420.0)).on_hover_text(hint);
            if ui.add_enabled(!busy, small(t("Choose ..."))).on_hover_text(hint).clicked() {
                actions.push(Action::PickMergeFurniture);
            }
        });
        ui.end_row();
    });
    ui.add_space(4.0);
    let folder = PathBuf::from(s.merge.folder.trim());
    ui.horizontal(|ui| {
        if ui.add_enabled(ready && !s.merge.folder.trim().is_empty(), widgets::primary_button(t("Preview the merge"))).clicked() {
            actions.push(Action::PreviewMerge(folder.clone()));
        }
        let file = folder.join("catalog_manager.sql");
        let hint = t("The catalog_manager.sql the ripper wrote, in the mode it was written with: a file made in Replace mode deletes your whole catalogue first. The import preview shows every change.");
        if ui.add_enabled(ready && file.is_file(), small(t("Import catalog_manager.sql as written ..."))).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
            actions.push(Action::ImportFile(file));
        }
    });
    if s.session.is_none() {
        widgets::muted(ui, palette, t("Log in to MariaDB first."), 12.0);
    }
    let mut cancel = false;
    if let Some(preview) = &s.merge.preview {
        ui.add_space(8.0);
        widgets::card_frame(palette).inner_margin(10).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            preview_card(ui, palette, preview, busy, actions, &mut cancel);
        });
    }
    if cancel {
        s.merge.preview = None;
    }
}

fn preview_card(ui: &mut egui::Ui, palette: &Palette, preview: &MergePreview, busy: bool, actions: &mut Vec<Action>, cancel: &mut bool) {
    ui.label(RichText::new(tf("The rip of {hotel}", &[("hotel", &preview.hotel)])).size(13.5).strong());
    ui.label(RichText::new(preview.folder.display().to_string()).size(11.5).color(palette.muted));
    let merge = preview.stats.merge.clone().unwrap_or_default();
    let left = preview.stats.offers_unresolved + preview.stats.offers_without_products;
    ui.label(
        RichText::new(tf(
            "{joined} ripped pages joined to yours, {new} new pages (under \"Imported from {hotel}\"), {added} offers added, {skipped} offers skipped (that page already sells their furni), {left} offers left out (their furni is not in items_base).",
            &[
                ("joined", &merge.joined_pages.to_string()),
                ("new", &merge.new_pages.to_string()),
                ("hotel", &preview.hotel),
                ("added", &merge.offers_added.to_string()),
                ("skipped", &merge.offers_skipped.to_string()),
                ("left", &left.to_string()),
            ],
        ))
        .size(12.5),
    );
    if !preview.stats.note.is_empty() {
        widgets::muted(ui, palette, &preview.stats.note, 12.0);
    }
    for ambiguous in &merge.ambiguous {
        widgets::muted(ui, palette, &tf("Same caption twice: {what}", &[("what", ambiguous)]), 12.0);
    }
    if left > 0 {
        widgets::muted(ui, palette, t("Offers left out need their furni in items_base first: the rip folder's missing_furnidata.json goes to the furni import (Import Assets), then merge again."), 12.0);
    }
    let pages: Vec<&String> = preview.notes.iter().filter(|n| n.starts_with("merge:")).collect();
    let sold: Vec<&String> = preview.notes.iter().filter(|n| n.starts_with("skipped:") && n.contains("already sells")).collect();
    let missing: Vec<&String> = preview.notes.iter().filter(|n| n.starts_with("skipped:") && !n.contains("already sells")).collect();
    let result = &preview.result;
    egui::ScrollArea::vertical().id_salt("catalog-merge-preview").max_height(320.0).show(ui, |ui| {
        egui::CollapsingHeader::new(RichText::new(tf("Pages ({count})", &[("count", &pages.len().to_string())])).size(12.0)).id_salt("merge-pages").show(ui, |ui| lines(ui, palette, &pages));
        egui::CollapsingHeader::new(RichText::new(tf("Offers skipped, already sold ({count})", &[("count", &sold.len().to_string())])).size(12.0))
            .id_salt("merge-sold")
            .show(ui, |ui| lines(ui, palette, &sold));
        egui::CollapsingHeader::new(RichText::new(tf("Offers left out, furni not in items_base ({count})", &[("count", &missing.len().to_string())])).size(12.0))
            .id_salt("merge-missing")
            .show(ui, |ui| lines(ui, palette, &missing));
        egui::CollapsingHeader::new(RichText::new(tf("Catalogue changes ({count})", &[("count", &result.changes.len().to_string())])).size(12.0))
            .id_salt("merge-changes")
            .show(ui, |ui| change_rows(ui, palette, &result.changes));
        egui::CollapsingHeader::new(RichText::new(tf("Furni interactions ({count})", &[("count", &result.interactions.len().to_string())])).size(12.0))
            .id_salt("merge-interactions")
            .default_open(!result.interactions.is_empty())
            .show(ui, |ui| {
                for change in result.interactions.iter().take(400) {
                    let before = change.before.as_deref().filter(|b| !b.is_empty()).unwrap_or("default");
                    ui.label(RichText::new(format!("#{} {}: {before} → {}", change.item_id, change.item_name, change.after)).size(12.0));
                }
                if result.interactions.len() > 400 {
                    widgets::muted(ui, palette, &tf("... and {count} more.", &[("count", &(result.interactions.len() - 400).to_string())]), 11.5);
                }
            });
    });
    if !preview.checked_interactions {
        widgets::muted(ui, palette, t("Interactions were not checked: choose your bundled/furniture folder above to check them too."), 12.0);
    } else {
        widgets::muted(
            ui,
            palette,
            &tf(
                "Interactions: only furni whose interaction is 'default' (or empty) get the one their bundle's logic gives; a furni with its own interaction is never changed. {missing} furni have no bundle in the folder.",
                &[("missing", &result.without_bundle.to_string())],
            ),
            12.0,
        );
    }
    if !result.history {
        widgets::muted(ui, palette, t("This database has no catalogue history: the merge cannot be undone. Make a backup first."), 12.0);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let count = result.changes.len() + result.interactions.len();
        let label = tf("Apply {count} changes", &[("count", &count.to_string())]);
        if ui.add_enabled(!busy && count > 0, widgets::primary_button(&label)).clicked() {
            actions.push(Action::ApplyMerge);
        }
        if ui.add_enabled(!busy, small(t("Cancel"))).clicked() {
            *cancel = true;
        }
        if count == 0 {
            ui.label(RichText::new(t("Nothing to change: this rip is already in the catalogue.")).size(12.0).color(palette.muted));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ripper_button_opens_the_tool_with_the_folder() {
        let folder = PathBuf::from("Catalog/hotel_20261127");
        let crate::views::catalog_rip::Click::Merge(asked) = crate::views::catalog_rip::import_click(&folder);
        assert_eq!(asked, folder);
        let mut state = State::default();
        state.open_merge(asked);
        assert_eq!(state.tab, super::super::Tab::Merge);
        assert_eq!(state.merge.folder, folder.display().to_string());
        assert_eq!(state.pending_merge, Some(folder), "previewed as soon as the database is open");
    }
}
