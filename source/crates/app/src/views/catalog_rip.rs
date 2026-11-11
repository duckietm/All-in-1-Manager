//! The Catalogue ripper plugin: copies the catalogue of Habbo or a retro hotel (Flash or Nitro) through G-Earth, with
//! the aio-catalog-rip crate. The user runs G-Earth and logs in to the hotel with their own client; the ripper is a
//! G-Earth extension that asks for the catalogue index and every page, a few per second. The result is a folder of
//! files (catalog.json, Arcturus SQL, catalog_manager.sql, images, README.txt); "Import into the hotel" previews
//! catalog_manager.sql in the Catalogue manager, where nothing is written until Apply.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aio_catalog_rip::{self as rip, HotelProfile, ItemsBaseRow, ProfileChoice, RipOptions, RipResult};
use aio_remote::sql_session::{SqlSession, SqlValue};
use aio_remote::Server;
use eframe::egui;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::gui::dialogs::Choice;
use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Page, Shared};
use crate::i18n::{t, tf};

/// What the page asks the window to do.
pub enum Click {
    /// Open the Catalogue manager and preview this file as an import.
    Import(PathBuf),
}

/// configuration/plugins/catalog-rip/: the hotel profiles and G-Earth.
fn plugin_folder() -> PathBuf {
    crate::plugins::packs::root().join("catalog-rip")
}

fn profiles_folder() -> PathBuf {
    plugin_folder().join("profiles")
}

/// Where the rips go: Catalog/ next to the configuration folder (next to Habbo_Assets).
fn default_output() -> PathBuf {
    let configuration = aio_core::config_dir::directory();
    configuration.parent().map(|p| p.join("Catalog")).unwrap_or_else(|| configuration.join("Catalog"))
}

fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    crate::gui::open_url(&path.display().to_string());
}

#[derive(Default)]
struct State {
    running: bool,
    log: LogLines,
    progress: Option<rip::RipProgress>,
    result: Option<RipResult>,
    /// The folder of a rip that stopped early but saved what it had.
    partial: Option<PathBuf>,
    error: Option<String>,
    cancel: Option<CancellationToken>,
}

pub struct CatalogRipView {
    state: Arc<Mutex<State>>,
    profiles: Vec<HotelProfile>,
    profile_errors: Vec<String>,
    /// None: pick the profile from the connection (host, client revision, client type).
    profile: Option<String>,
    normal: bool,
    builders_club: bool,
    images: bool,
    items_lookup: bool,
    delay_ms: u64,
    output: String,
}

impl CatalogRipView {
    pub fn new(_shared: &Shared) -> Self {
        let mut view = Self {
            state: Arc::new(Mutex::new(State { log: LogLines::with_limit(5000), ..State::default() })),
            profiles: Vec::new(),
            profile_errors: Vec::new(),
            profile: None,
            normal: true,
            builders_club: true,
            images: true,
            items_lookup: true,
            delay_ms: RipOptions::default().delay_ms,
            output: default_output().display().to_string(),
        };
        view.reload_profiles();
        view
    }

    fn reload_profiles(&mut self) {
        let (profiles, errors) = rip::load_profiles(&profiles_folder());
        self.profiles = profiles;
        self.profile_errors = errors;
        if self.profile.as_ref().is_some_and(|name| !self.profiles.iter().any(|p| &p.name == name)) {
            self.profile = None;
        }
    }

    pub fn busy(&self) -> bool {
        self.state.lock().running
    }

    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) -> Option<Click> {
        let mut click = None;
        let running = self.busy();
        widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
            ui.label(widgets::title_text(t("How it works"), 17.0));
            ui.add_space(4.0);
            for (n, line) in [
                t("Start G-Earth (the ripper can download and start it for you) and connect it to the hotel."),
                t("Log in to the hotel with your own client, through G-Earth, and wait until you are in the hotel view or a room."),
                t("Click Start: the ripper asks the hotel for the catalogue index and every page, a few per second, as the client does when you browse."),
            ]
            .iter()
            .enumerate()
            {
                widgets::muted(ui, palette, &format!("{}. {line}", n + 1), 13.0);
            }
            ui.add_space(4.0);
            widgets::muted(
                ui,
                palette,
                t("Official Habbo works out of the box (G-Earth knows its packets). For a retro the hotel profile gives the packet ids: Arcturus Morningstar and Nitro retros are built in; for another emulator make a profile."),
                12.5,
            );
        });
        ui.add_space(12.0);

        ui.add_enabled_ui(!running, |ui| self.options(ui, palette));
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            if running {
                if ui.add(widgets::secondary_button(t("Cancel"))).clicked() {
                    if let Some(cancel) = &self.state.lock().cancel {
                        cancel.cancel();
                    }
                }
            } else if ui.add_enabled(self.normal || self.builders_club, widgets::primary_button(t("Start"))).clicked() {
                self.start(shared);
            }
        });
        ui.add_space(8.0);

        let mut state = self.state.lock();
        if let Some(progress) = state.progress.clone().filter(|_| state.running) {
            if progress.total > 0 {
                ui.add(
                    egui::ProgressBar::new(progress.done as f32 / progress.total as f32)
                        .text(tf("{done} of {total} pages · {page}", &[("done", &progress.done.to_string()), ("total", &progress.total.to_string()), ("page", &progress.label)])),
                );
            } else {
                widgets::indeterminate_bar(ui, 8.0, palette.muted);
            }
            ui.add_space(8.0);
        }
        if let Some(error) = state.error.clone() {
            widgets::result_banner(ui, palette, ResultKind::Error, t("The rip stopped"), &error);
            if let Some(folder) = state.partial.clone() {
                ui.add_space(4.0);
                if ui.add(widgets::secondary_button(t("Open the folder"))).on_hover_text(folder.display().to_string()).clicked() {
                    open_folder(&folder);
                }
            }
            ui.add_space(8.0);
        }
        if let Some(result) = state.result.clone() {
            click = result_card(ui, palette, shared, &result);
            ui.add_space(8.0);
        }
        log_view::show(ui, "catalog-rip", &mut state.log, Some(260.0), t("The ripper's log appears here."));
        click
    }

    fn options(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
            ui.label(widgets::title_text(t("Options"), 17.0));
            ui.add_space(6.0);
            egui::Grid::new("rip-options").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label(t("Hotel profile"));
                ui.horizontal(|ui| {
                    let selected = self.profile.clone().unwrap_or_else(|| t("Detect from the connection").to_string());
                    egui::ComboBox::from_id_salt("rip-profile").selected_text(selected).width(260.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.profile, None, t("Detect from the connection"));
                        for profile in &self.profiles {
                            let label = format!("{}  ({})", profile.name, profile.kind.label());
                            ui.selectable_value(&mut self.profile, Some(profile.name.clone()), label);
                        }
                    });
                    if ui.add(widgets::secondary_button(t("Profiles folder"))).on_hover_text(t("Your own profiles: one JSON file each; a profile with a built-in one's name replaces it")).clicked() {
                        open_folder(&profiles_folder());
                    }
                    if ui.add(widgets::secondary_button(t("Reload"))).clicked() {
                        self.reload_profiles();
                    }
                });
                ui.end_row();

                ui.label(t("Catalogues"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.normal, t("Normal"));
                    ui.checkbox(&mut self.builders_club, "Builders Club");
                });
                ui.end_row();

                ui.label(t("Also"));
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.images, t("Download the page images and icons"));
                    ui.checkbox(&mut self.items_lookup, t("Match the furni with items_base of the server you are logged in to (for the import)"));
                });
                ui.end_row();

                ui.label(t("Pause between pages"));
                ui.add(egui::DragValue::new(&mut self.delay_ms).range(50..=5000).suffix(" ms"))
                    .on_hover_text(t("Lower is faster; the hotel may disconnect a client that asks too fast"));
                ui.end_row();

                ui.label(t("Save in"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.output).desired_width(360.0));
                    if ui.add(widgets::secondary_button(t("Open"))).clicked() {
                        open_folder(Path::new(&self.output));
                    }
                });
                ui.end_row();
            });
            for error in &self.profile_errors {
                widgets::muted(ui, palette, &format!("⚠ {error}"), 12.0);
            }
        });
    }

    fn start(&mut self, shared: &Shared) {
        let cancel = CancellationToken::new();
        {
            let mut state = self.state.lock();
            state.running = true;
            state.log.clear();
            state.progress = None;
            state.result = None;
            state.partial = None;
            state.error = None;
            state.cancel = Some(cancel.clone());
        }
        let options = RipOptions {
            output_folder: PathBuf::from(self.output.trim()),
            delay_ms: self.delay_ms,
            download_images: self.images,
            normal: self.normal,
            builders_club: self.builders_club,
            gearth_folder: Some(plugin_folder().join("G-Earth")),
            ..RipOptions::default()
        };
        let choice = match self.profile.as_ref().and_then(|name| self.profiles.iter().find(|p| &p.name == name)) {
            Some(profile) => ProfileChoice::Fixed(profile.clone()),
            None => ProfileChoice::Detect(self.profiles.clone()),
        };
        let server = if self.items_lookup { shared.session() } else { None };
        let state = self.state.clone();
        let shared = shared.clone();
        shared.set_status(t("Ripping the catalogue ..."));
        let task_shared = shared.clone();
        shared.spawn(async move {
            let shared = task_shared;
            let log_state = state.clone();
            let ctx = shared.ctx.clone();
            let log = move |line: &str| {
                log_state.lock().log.push(line);
                ctx.request_repaint();
            };
            let dialogs = shared.dialogs.clone();
            let ask = move |question: String| -> futures::future::BoxFuture<'static, bool> {
                let dialogs = dialogs.clone();
                Box::pin(async move {
                    let options = vec![Choice::new(t("Yes"), "", true).primary(), Choice::new(t("No"), "", false)];
                    dialogs.choose(t("Catalogue ripper"), t("Catalogue ripper"), &question, false, options).await
                })
            };
            let progress_state = state.clone();
            let ctx = shared.ctx.clone();
            let progress = move |p: rip::RipProgress| {
                progress_state.lock().progress = Some(p);
                ctx.request_repaint();
            };
            let items = server.map(HotelItems);
            let result = rip::rip(&options, choice, items.as_ref().map(|i| i as &dyn rip::ItemsSource), &log, &ask, &progress, &cancel).await;
            let mut state = state.lock();
            state.running = false;
            state.cancel = None;
            match result {
                Ok(result) => {
                    shared.set_status(&tf("The catalogue of {hotel} is saved: {pages} pages, {items} offers.", &[("hotel", &result.hotel), ("pages", &result.pages.to_string()), ("items", &result.items.to_string())]));
                    state.result = Some(result);
                }
                Err(error) => {
                    state.partial = error.downcast_ref::<rip::PartialOutput>().map(|p| p.folder.clone());
                    if error.is::<rip::Cancelled>() || error.chain().any(|e| e.is::<rip::Cancelled>()) {
                        state.error = Some(t("Cancelled.").to_string());
                        shared.set_status(t("The catalogue rip was cancelled."));
                    } else {
                        state.error = Some(format!("{error:#}"));
                        shared.set_status(t("The catalogue rip stopped."));
                    }
                }
            }
        });
    }
}

fn result_card(ui: &mut egui::Ui, palette: &Palette, shared: &Shared, result: &RipResult) -> Option<Click> {
    let mut click = None;
    let kind = if result.complete { ResultKind::Success } else { ResultKind::Warning };
    let heading = if result.complete { tf("The catalogue of {hotel} is copied", &[("hotel", &result.hotel)]) } else { tf("The catalogue of {hotel} is copied, with gaps", &[("hotel", &result.hotel)]) };
    let mut lines = vec![tf(
        "{pages} pages and {items} offers ({profile}); {failed} pages could not be read.",
        &[("pages", &result.pages.to_string()), ("items", &result.items.to_string()), ("profile", &result.profile), ("failed", &result.pages_failed.to_string())],
    )];
    if result.images.downloaded + result.images.missing + result.images.failed > 0 {
        lines.push(tf(
            "Images: {downloaded} downloaded, {missing} not on the hotel's server, {failed} failed.",
            &[("downloaded", &result.images.downloaded.to_string()), ("missing", &result.images.missing.to_string()), ("failed", &result.images.failed.to_string())],
        ));
    }
    let manager = &result.manager;
    lines.push(tf(
        "For the import: {pages} pages and {offers} offers; {left} offers left out (their furni is not in items_base).",
        &[("pages", &manager.pages.to_string()), ("offers", &manager.offers.to_string()), ("left", &(manager.offers_unresolved + manager.offers_without_products).to_string())],
    ));
    widgets::result_banner(ui, palette, kind, &heading, &lines.join("\n"));
    ui.add_space(6.0);
    let manager_file = result.folder.join("catalog_manager.sql");
    ui.horizontal(|ui| {
        if ui.add(widgets::secondary_button(t("Open the folder"))).on_hover_text(result.folder.display().to_string()).clicked() {
            open_folder(&result.folder);
        }
        let can_import = manager_file.is_file() && shared.session().is_some() && crate::plugins::page_visible(Page::CatalogManager);
        let hint = if shared.session().is_none() {
            t("Log in to the hotel's server first")
        } else {
            t("Opens the Catalogue manager with what the import would change; nothing is written until you click Apply there. The import replaces the whole catalogue.")
        };
        if ui.add_enabled(can_import, widgets::primary_button(t("Import into the hotel ..."))).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
            click = Some(Click::Import(manager_file.clone()));
        }
        if shared.session().is_none() && ui.add(widgets::secondary_button(t("Log in"))).clicked() {
            shared.request(crate::gui::Request::Login);
        }
    });
    click
}

/// items_base of the hotel database on the logged-in server, through MariaDB with the saved account (the one the
/// other database pages use); without a saved account the rip goes on without it.
struct HotelItems(Arc<dyn Server>);

#[async_trait::async_trait]
impl rip::ItemsSource for HotelItems {
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>> {
        let server = &*self.0;
        let host = server.login().host.clone();
        let saved = aio_core::config_dir::load_database_credentials(&host)
            .ok_or_else(|| anyhow::anyhow!("No saved MariaDB account for {host}: open the Catalogue manager once to log in to MariaDB."))?;
        let user = if saved.root_user.is_empty() { "root".to_string() } else { saved.root_user.clone() };
        let port = if saved.port > 0 { saved.port } else { 3306 };
        let database = aio_remote::catalog_admin::hotel_database(server).await?;
        let session = SqlSession::open(server, &user, &saved.root_password, port).await?;
        let table = format!("{}.`items_base`", aio_remote::sql_session::quote(&database));
        let rows = match session.select(&format!("SELECT id, sprite_id, type, item_name, effect_id_male FROM {table}"), Vec::new()).await {
            Ok(rows) => rows,
            // An emulator without effect_id_male.
            Err(_) => session.select(&format!("SELECT id, sprite_id, type, item_name, 0 FROM {table}"), Vec::new()).await?,
        };
        session.close().await;
        Ok(rows.into_iter().filter_map(|row| items_row(&row)).collect())
    }
}

fn items_row(row: &[SqlValue]) -> Option<ItemsBaseRow> {
    let int = |v: &SqlValue| -> Option<i64> {
        match v {
            SqlValue::Int(i) => Some(*i),
            SqlValue::UInt(u) => i64::try_from(*u).ok(),
            SqlValue::Text(s) | SqlValue::Decimal(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    let text = |v: &SqlValue| -> String {
        match v {
            SqlValue::Text(s) => s.clone(),
            SqlValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            SqlValue::Null => String::new(),
            other => int(other).map(|i| i.to_string()).unwrap_or_default(),
        }
    };
    Some(ItemsBaseRow {
        id: u32::try_from(int(row.first()?)?).ok()?,
        sprite_id: int(row.get(1)?).unwrap_or(0) as i32,
        item_type: text(row.get(2)?),
        item_name: text(row.get(3)?),
        effect_id: row.get(4).and_then(int).unwrap_or(0) as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_base_rows_are_read_from_the_values() {
        let row = vec![SqlValue::UInt(12), SqlValue::Int(3001), SqlValue::Text("s".into()), SqlValue::Bytes(b"chair_norja".to_vec()), SqlValue::Null];
        let read = items_row(&row).unwrap();
        assert_eq!((read.id, read.sprite_id, read.item_type.as_str(), read.item_name.as_str(), read.effect_id), (12, 3001, "s", "chair_norja", 0));
        assert!(items_row(&[SqlValue::Null]).is_none());
    }
}
