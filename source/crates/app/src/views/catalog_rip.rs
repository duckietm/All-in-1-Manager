//! The Catalogue ripper plugin: copies the catalogue of Habbo or a retro hotel (Flash or Nitro) through G-Earth, with
//! the aio-catalog-rip crate. The user runs G-Earth and logs in to the hotel with their own client; the ripper is a
//! G-Earth extension that asks for the catalogue index and every page, a few per second. The result is a folder of
//! files (catalog.json, Arcturus SQL, catalog_manager.sql, images, README.txt); "Import into the hotel" opens the
//! Catalogue manager's "Merge a rip" with the rip folder, where nothing is written until Apply.

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
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// What the page asks the window to do.
pub enum Click {
    /// Open the Catalogue manager's "Merge a rip" with this rip folder.
    Merge(PathBuf),
}

/// configuration/plugins/catalog-rip/: the hotel profiles and G-Earth.
fn plugin_folder() -> PathBuf {
    aio_core::config_dir::directory().join("plugins").join("catalog-rip")
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
    /// A FurnitureData.json chosen in the file dialog, for the page to take.
    picked_hotel_furnidata: Option<PathBuf>,
    /// A c_images/catalogue folder chosen in the folder dialog.
    picked_own_images: Option<PathBuf>,
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
    /// The hotel's own FurnitureData.json (empty: none).
    hotel_furnidata: String,
    /// The owner's own c_images/catalogue folder (empty: none).
    own_images: String,
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
            hotel_furnidata: String::new(),
            own_images: String::new(),
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

        if let Some(path) = self.state.lock().picked_hotel_furnidata.take() {
            self.hotel_furnidata = path.display().to_string();
        }
        if let Some(path) = self.state.lock().picked_own_images.take() {
            self.own_images = path.display().to_string();
        }
        ui.add_enabled_ui(!running, |ui| self.options(ui, palette, shared));
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

    fn options(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
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
                    ui.checkbox(&mut self.images, t("Download the page images, icons and thumbnails"));
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

                let hint = t("Optional. The furni it already lists are left out of missing_furnidata.json, and ids it gives other furni are renumbered.");
                ui.label(t("Your hotel's FurnitureData.json")).on_hover_text(hint);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.hotel_furnidata).hint_text(t("Empty: ids are not checked")).desired_width(360.0))
                        .on_hover_text(hint);
                    if ui.add(widgets::secondary_button(t("Choose ..."))).on_hover_text(hint).clicked() {
                        let state = self.state.clone();
                        shared.spawn(async move {
                            let dialog = rfd::AsyncFileDialog::new()
                                .set_title(t("Your hotel's FurnitureData.json"))
                                .add_filter("JSON / XML", &["json", "xml", "txt"]);
                            if let Some(file) = dialog.pick_file().await {
                                state.lock().picked_hotel_furnidata = Some(file.path().to_path_buf());
                            }
                        });
                    }
                });
                ui.end_row();

                let hint = t("Optional. Your c_images/catalogue folder: images it already has (including the th_* thumbnails) are not downloaded again.");
                ui.label(t("Your catalogue images folder")).on_hover_text(hint);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.own_images).hint_text(t("Empty: everything is downloaded")).desired_width(360.0))
                        .on_hover_text(hint);
                    if ui.add(widgets::secondary_button(t("Choose ..."))).on_hover_text(hint).clicked() {
                        let state = self.state.clone();
                        shared.spawn(async move {
                            let dialog = rfd::AsyncFileDialog::new().set_title(t("Your catalogue images folder"));
                            if let Some(folder) = dialog.pick_folder().await {
                                state.lock().picked_own_images = Some(folder.path().to_path_buf());
                            }
                        });
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
            hotel_furnidata: Some(self.hotel_furnidata.trim().to_string()).filter(|s| !s.is_empty()),
            own_catalogue_images: Some(self.own_images.trim().to_string()).filter(|s| !s.is_empty()),
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
    ui.horizontal(|ui| {
        if ui.add(widgets::secondary_button(t("Open the folder"))).on_hover_text(result.folder.display().to_string()).clicked() {
            open_folder(&result.folder);
        }
        let can_import = result.folder.join("packets").is_dir() && shared.session().is_some();
        let hint = if shared.session().is_none() {
            t("Log in to the hotel's server first")
        } else {
            t("Opens the Catalogue manager's Merge a rip with this rip: it is fitted into your catalogue and you see every change first; nothing is written until you click Apply there.")
        };
        if ui.add_enabled(can_import, widgets::primary_button(t("Import into the hotel ..."))).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
            click = Some(import_click(&result.folder));
        }
        if shared.session().is_none() && ui.add(widgets::secondary_button(t("Log in"))).clicked() {
            shared.request(crate::gui::Request::Login);
        }
    });
    click
}

/// What "Import into the hotel" asks for: the Catalogue manager's Merge a rip, with the rip folder.
pub fn import_click(folder: &Path) -> Click {
    Click::Merge(folder.to_path_buf())
}

/// items_base and the catalogue of the hotel database through an open SQL session (the Catalogue manager's).
pub struct SessionItems {
    pub session: Arc<SqlSession>,
    pub database: String,
}

#[async_trait::async_trait]
impl rip::ItemsSource for SessionItems {
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>> {
        read_items_base(&self.session, &self.database).await
    }

    async fn existing_catalogue(&self) -> anyhow::Result<Option<rip::ExistingCatalogue>> {
        let reader = self.session.clone();
        let select = move |sql: String| -> futures::future::BoxFuture<'static, anyhow::Result<Vec<Vec<SqlValue>>>> {
            let reader = reader.clone();
            Box::pin(async move { reader.select(&sql, Vec::new()).await })
        };
        load_existing(&select, &self.database).await.map(Some)
    }
}

/// items_base's id, sprite_id, type, item_name and effect_id_male (0 without that column).
async fn read_items_base(session: &SqlSession, database: &str) -> anyhow::Result<Vec<ItemsBaseRow>> {
    let table = format!("{}.`items_base`", aio_remote::sql_session::quote(database));
    let rows = match session.select(&format!("SELECT id, sprite_id, type, item_name, effect_id_male FROM {table}"), Vec::new()).await {
        Ok(rows) => Ok(rows),
        // An emulator without effect_id_male.
        Err(_) => session.select(&format!("SELECT id, sprite_id, type, item_name, 0 FROM {table}"), Vec::new()).await,
    };
    Ok(rows?.into_iter().filter_map(|row| items_row(&row)).collect())
}

/// items_base of the hotel database on the logged-in server, through MariaDB with the saved account (the one the
/// other database pages use); without a saved account the rip goes on without it.
struct HotelItems(Arc<dyn Server>);

impl HotelItems {
    /// A MariaDB session with the saved account, and the hotel database's name.
    async fn session(&self) -> anyhow::Result<(SqlSession, String)> {
        let server = &*self.0;
        let host = server.login().host.clone();
        let saved = aio_core::config_dir::load_database_credentials(&host)
            .ok_or_else(|| anyhow::anyhow!("No saved MariaDB account for {host}: open the Catalogue manager once to log in to MariaDB."))?;
        let user = if saved.root_user.is_empty() { "root".to_string() } else { saved.root_user.clone() };
        let port = if saved.port > 0 { saved.port } else { 3306 };
        let database = aio_remote::catalog_admin::hotel_database(server).await?;
        let session = SqlSession::open(server, &user, &saved.root_password, port).await?;
        Ok((session, database))
    }
}

#[async_trait::async_trait]
impl rip::ItemsSource for HotelItems {
    async fn items_base(&self) -> anyhow::Result<Vec<ItemsBaseRow>> {
        let (session, database) = self.session().await?;
        let rows = read_items_base(&session, &database).await;
        session.close().await;
        rows
    }

    /// The hotel's pages and offers (both catalogues), for the merge and the id ranges.
    async fn existing_catalogue(&self) -> anyhow::Result<Option<rip::ExistingCatalogue>> {
        let (session, database) = self.session().await?;
        let session = Arc::new(session);
        let reader = session.clone();
        let select = move |sql: String| -> futures::future::BoxFuture<'static, anyhow::Result<Vec<Vec<SqlValue>>>> {
            let reader = reader.clone();
            Box::pin(async move { reader.select(&sql, Vec::new()).await })
        };
        let existing = load_existing(&select, &database).await;
        // The closure holds a reference to the session: let it go so the session can close.
        drop(select);
        if let Ok(session) = Arc::try_unwrap(session) {
            session.close().await;
        }
        existing.map(Some)
    }
}

/// One read-only SELECT on the hotel database.
type Select = dyn Fn(String) -> futures::future::BoxFuture<'static, anyhow::Result<Vec<Vec<SqlValue>>>> + Send + Sync;

/// Rows per read: the Manager's reads stop at 10,000 rows.
const WINDOW: usize = aio_remote::sql_session::MAX_QUERY_ROWS;

/// MariaDB's ER_NO_SUCH_TABLE (1146): by the server's error number, else by its text.
fn missing_table(error: &anyhow::Error) -> bool {
    if let Some(code) = aio_remote::sql_session::server_error_code(error) {
        return code == 1146;
    }
    let text = format!("{error:#}");
    text.contains("1146") || text.contains("doesn't exist")
}

/// Every row of `columns` (the first one the id) of `table`, read WINDOW rows at a time in id order. None: the table
/// does not exist and `optional` (the Builders Club warehouse of an emulator without one); any other error fails.
async fn read_windowed(select: &Select, table: &str, columns: &str, optional: bool) -> anyhow::Result<Option<Vec<Vec<SqlValue>>>> {
    let mut all = Vec::new();
    let mut after = i64::MIN;
    loop {
        let sql = format!("SELECT {columns} FROM {table} WHERE id > {after} ORDER BY id LIMIT {WINDOW}");
        let rows = match select(sql).await {
            Ok(rows) => rows,
            Err(error) if optional && all.is_empty() && missing_table(&error) => return Ok(None),
            Err(error) => return Err(error.context(format!("reading {table}"))),
        };
        let count = rows.len();
        if let Some(last) = rows.last() {
            after = last.first().and_then(sql_int).ok_or_else(|| anyhow::anyhow!("{table}: an id that is not a number"))?;
        }
        all.extend(rows);
        if count < WINDOW {
            return Ok(Some(all));
        }
    }
}

/// The hotel's catalogue pages and offers, normal and Builders Club, with only the columns the merge needs.
async fn load_existing(select: &Select, database: &str) -> anyhow::Result<rip::ExistingCatalogue> {
    let db = aio_remote::sql_session::quote(database);
    let (mut pages, mut offers) = (Vec::new(), Vec::new());
    for (kind, page_table, offer_table, optional) in [("NORMAL", "catalog_pages", "catalog_items", false), ("BUILDER", "catalog_pages_bc", "catalog_items_bc", true)] {
        let Some(page_rows) = read_windowed(select, &format!("{db}.`{page_table}`"), "id, parent_id, caption, order_num", optional).await? else { continue };
        let offer_rows = read_windowed(select, &format!("{db}.`{offer_table}`"), "id, page_id, item_ids, extradata, order_number", optional).await?.unwrap_or_default();
        for row in page_rows {
            let Some(id) = row.first().and_then(sql_int) else { continue };
            pages.push(rip::ExistingPage {
                id,
                parent_id: row.get(1).and_then(sql_int).unwrap_or(-1),
                caption: row.get(2).map(sql_text).unwrap_or_default(),
                catalog_type: kind.into(),
                order_num: row.get(3).and_then(sql_int).unwrap_or(0),
            });
        }
        for row in offer_rows {
            let Some(id) = row.first().and_then(sql_int) else { continue };
            offers.push(rip::ExistingOffer {
                id,
                page_id: row.get(1).and_then(sql_int).unwrap_or(0),
                catalog_type: kind.into(),
                item_ids: row.get(2).map(sql_text).unwrap_or_default(),
                extradata: row.get(3).map(sql_text).unwrap_or_default(),
                order_number: row.get(4).and_then(sql_int).unwrap_or(0),
            });
        }
    }
    // Catalog Studio's id sequences (an emulator without them: none).
    let sequences = match select(format!("SELECT entity_type, catalog_type, next_id FROM {db}.`catalog_id_sequences`")).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| Some((sql_text(r.first()?), sql_text(r.get(1)?), sql_int(r.get(2)?)?)))
            .collect(),
        Err(error) if missing_table(&error) => Vec::new(),
        Err(error) => return Err(error.context("reading catalog_id_sequences")),
    };
    Ok(rip::ExistingCatalogue::from_rows(pages, offers).with_sequences(&sequences))
}

fn sql_int(v: &SqlValue) -> Option<i64> {
    match v {
        SqlValue::Int(i) => Some(*i),
        SqlValue::UInt(u) => i64::try_from(*u).ok(),
        SqlValue::Text(s) | SqlValue::Decimal(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn sql_text(v: &SqlValue) -> String {
    match v {
        SqlValue::Text(s) => s.clone(),
        SqlValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        SqlValue::Null => String::new(),
        other => sql_int(other).map(|i| i.to_string()).unwrap_or_default(),
    }
}

fn items_row(row: &[SqlValue]) -> Option<ItemsBaseRow> {
    Some(ItemsBaseRow {
        id: u32::try_from(sql_int(row.first()?)?).ok()?,
        sprite_id: sql_int(row.get(1)?).unwrap_or(0) as i32,
        item_type: sql_text(row.get(2)?),
        item_name: sql_text(row.get(3)?),
        effect_id: row.get(4).and_then(sql_int).unwrap_or(0) as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database of `pages` normal pages (one offer each) and no Builders Club tables, answering windowed SELECTs
    /// and recording them.
    fn fake_database(pages: i64, bc_error: &'static str) -> (Arc<Mutex<Vec<String>>>, Box<Select>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let select = move |sql: String| -> futures::future::BoxFuture<'static, anyhow::Result<Vec<Vec<SqlValue>>>> {
            log.lock().push(sql.clone());
            let result = (|| {
                if sql.contains("_bc`") {
                    anyhow::bail!("{bc_error}");
                }
                if sql.contains("catalog_id_sequences") {
                    return Ok(vec![vec![SqlValue::Text("PAGE".into()), SqlValue::Text("NORMAL".into()), SqlValue::Int(30_000)]]);
                }
                let after: i64 = sql.split("WHERE id > ").nth(1).and_then(|s| s.split(' ').next()).and_then(|s| s.parse().ok()).unwrap();
                let limit: usize = sql.rsplit("LIMIT ").next().and_then(|s| s.trim().parse().ok()).unwrap();
                let ids = (1..=pages).filter(|id| *id > after).take(limit);
                Ok(if sql.contains("`catalog_pages`") {
                    ids.map(|id| vec![SqlValue::UInt(id as u64), SqlValue::Int(if id == 1 { -1 } else { 1 }), SqlValue::Bytes(format!("Page {id}").into_bytes()), SqlValue::Int(id)]).collect()
                } else {
                    ids.map(|id| vec![SqlValue::Int(id), SqlValue::Int(id), SqlValue::Text(format!("{}:2;7", id + 100)), SqlValue::Null, SqlValue::Int(4)]).collect()
                })
            })();
            Box::pin(async move { result })
        };
        (seen, Box::new(select))
    }

    #[tokio::test]
    async fn loader_windows_and_missing_bc_table() {
        let (seen, select) = fake_database(25_000, "ERROR 42S02 (1146): Table 'habbo.catalog_pages_bc' doesn't exist");
        let existing = load_existing(&*select, "habbo").await.unwrap();
        assert_eq!(existing.page_tree.len(), 25_000);
        assert_eq!(existing.max_page("NORMAL"), 25_000);
        assert_eq!(existing.max_offer("NORMAL"), 25_000);
        assert_eq!(existing.page_path("NORMAL", 3).unwrap(), vec!["page 1", "page 3"]);
        assert_eq!(existing.offers_by_page["NORMAL"][&3], vec![(vec![7, 103], String::new())]);
        assert_eq!(existing.max_order["NORMAL"][&3], 4);
        assert_eq!(existing.first_free_page("NORMAL"), 30_000, "the emulator's sequence is ahead");
        assert!(!existing.pages.contains_key("BUILDER"), "no warehouse");
        let seen = seen.lock().clone();
        // 3 windows per table (10,000 + 10,000 + 5,000), then the one missing BC table (its offers are not read),
        // then the id sequences.
        assert_eq!(seen.len(), 8, "{seen:#?}");
        assert_eq!(seen[0], format!("SELECT id, parent_id, caption, order_num FROM `habbo`.`catalog_pages` WHERE id > {} ORDER BY id LIMIT 10000", i64::MIN));
        assert!(seen[1].contains("WHERE id > 10000 ORDER BY id LIMIT 10000"), "{}", seen[1]);
        assert!(seen[3].starts_with("SELECT id, page_id, item_ids, extradata, order_number FROM `habbo`.`catalog_items` WHERE"), "{}", seen[3]);
        assert!(seen[7].contains("catalog_id_sequences"));
        assert!(seen[6].contains("`catalog_pages_bc`"));
        assert!(seen.iter().all(|s| s.starts_with("SELECT ")), "read-only");

        // An exact multiple of the window needs one more (empty) read.
        let (seen, select) = fake_database(10_000, "ERROR 42S02 (1146): no such table");
        load_existing(&*select, "habbo").await.unwrap();
        assert_eq!(seen.lock().len(), 6);

        // Any other error fails the load.
        let (_, select) = fake_database(5, "ERROR 1142 (42000): SELECT command denied");
        let error = load_existing(&*select, "habbo").await.unwrap_err();
        assert!(format!("{error:#}").contains("1142"), "{error:#}");
    }

    #[test]
    fn items_base_rows_are_read_from_the_values() {
        let row = vec![SqlValue::UInt(12), SqlValue::Int(3001), SqlValue::Text("s".into()), SqlValue::Bytes(b"chair_norja".to_vec()), SqlValue::Null];
        let read = items_row(&row).unwrap();
        assert_eq!((read.id, read.sprite_id, read.item_type.as_str(), read.item_name.as_str(), read.effect_id), (12, 3001, "s", "chair_norja", 0));
        assert!(items_row(&[SqlValue::Null]).is_none());
    }
}
