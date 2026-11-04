//! Get Assets: Habbo's own assets downloaded on this computer into a folder laid out like the server's gamedata, so
//! Deploy Assets copies it as it is; and a Nitro retro's custom furniture and clothes, into a folder of their own to
//! upload by hand. No server connection is needed. (All-in-1 Converter: Habbo Original and Nitro Custom Downloads)

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_remote::furniture_language;
use aio_remote::habbo_assets::retro::{self, RetroUrls};
use aio_remote::habbo_assets::{self, Asset, Downloader, Options, Urls};
use eframe::egui::{self, RichText};
use tokio_util::sync::CancellationToken;

use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Page, Request, Shared, View};
use crate::i18n::{n, t, tf};

/// Title and what it gets, per download (the converter's menu texts).
fn texts(asset: Asset) -> (&'static str, &'static str) {
    match asset {
        Asset::Badges => (n("Badges"), n("Every badge of the nine hotels (.gif)")),
        Asset::Clothes => (n("Clothes"), n("FigureData.json, FigureMap.json and the clothing .hab files")),
        Asset::Effects => (n("Effects"), n("EffectMap.json, HabboAvatarActions.json and the effect .hab files")),
        Asset::Furnidata => (n("Furnidata"), n("FurnitureData.json in the hotel's language")),
        Asset::Furniture => (n("Furniture"), n("Every furniture .hab and its icons")),
        Asset::CatalogueIcons => (n("Catalogue icons"), n("The small icons of the catalogue pages")),
        Asset::Mp3 => (n("MP3"), n("The trax machine's sound samples")),
        Asset::ProductData => (n("Productdata"), n("ProductData.json: product names and descriptions")),
        Asset::Quests => (n("Quest images"), n("The images of the quests and achievements")),
        Asset::Reception => (n("Reception images"), n("The hotel view and promo images")),
        Asset::Texts => (n("Texts"), n("ExternalTexts.json: the game's texts in the hotel's language")),
        Asset::Variables => (n("Variables"), n("external_variables as JSON, to read here (not sent to the server)")),
        Asset::Pets => (n("Pets"), n("The pet .hab files")),
        Asset::EffectIcons => (n("Effect icons"), n("fx_icon_<type> for the avatar editor, from the Habbo Classic app")),
    }
}

/// The two halves of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Habbo,
    Retro,
}

/// One download of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    Habbo(Asset),
    RetroFurniture,
    RetroClothes,
    /// The .swf files kept in custom/source/ converted to .hab (no download).
    ConvertSwf,
}

impl Job {
    fn title(self) -> &'static str {
        match self {
            Job::Habbo(asset) => texts(asset).0,
            Job::RetroFurniture => n("Custom furniture"),
            Job::RetroClothes => n("Custom clothes"),
            Job::ConvertSwf => n("Convert the kept .swf files"),
        }
    }
}

/// The retro URL fields: (label, renderer-config.json key).
const RETRO_FIELDS: [(&str, &str); 6] = [
    ("FurnitureData.json", "furnidata.url"),
    (n("Furniture folder"), "furni.asset.url"),
    (n("Furniture icons"), "furni.asset.icon.url"),
    (n("Clothes folder"), "avatar.asset.url"),
    ("FigureData.json", "avatar.figuredata.url"),
    ("FigureMap.json", "avatar.figuremap.url"),
];

fn retro_field(urls: &mut RetroUrls, index: usize) -> &mut String {
    match index {
        0 => &mut urls.furnidata,
        1 => &mut urls.furniture,
        2 => &mut urls.furniture_icons,
        3 => &mut urls.clothes,
        4 => &mut urls.figuredata,
        _ => &mut urls.figuremap,
    }
}

#[derive(Default)]
struct State {
    running: bool,
    cancel: Option<CancellationToken>,
    log: LogLines,
    /// (title, summary or error) per download of the last run.
    results: Vec<(String, Result<String, String>)>,
    stopped: bool,
    /// The last run was the retro's (its folder is uploaded by hand, not by Deploy Assets).
    last_retro: bool,
    /// A folder or file the pickers chose, taken over by the page on the next frame.
    picked_folder: Option<PathBuf>,
    picked_source: Option<PathBuf>,
}

pub struct GetAssetsView {
    state: Arc<Mutex<State>>,
    hotel: &'static str,
    selected: BTreeSet<Asset>,
    not_sellable: bool,
    icon_source: String,
    /// The download folder, read from Settings once and again after a change.
    folder: PathBuf,
    custom_folder: bool,
    tab: Tab,
    /// The retro's URLs as edited, and as last saved in Settings.
    retro: RetroUrls,
    retro_saved: RetroUrls,
    retro_furniture: bool,
    retro_clothes: bool,
    /// Done was pressed: back to the top of the page on the next frame.
    scroll_top: bool,
}

fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    crate::gui::open_url(&path.display().to_string());
}

impl GetAssetsView {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            state: Arc::new(Mutex::new(State { log: LogLines::with_limit(4000), ..State::default() })),
            hotel: "nl",
            selected: Asset::ALL.iter().copied().filter(|a| a.in_download_all()).collect(),
            not_sellable: false,
            icon_source: String::new(),
            folder: PathBuf::new(),
            custom_folder: false,
            tab: if std::env::var("AIO_SCREENSHOT_TAB").as_deref() == Ok("retro") { Tab::Retro } else { Tab::Habbo },
            retro: RetroUrls::default(),
            retro_saved: RetroUrls::default(),
            retro_furniture: true,
            retro_clothes: true,
            scroll_top: false,
        }
        .with_folder_from_settings()
    }

    fn with_folder_from_settings(mut self) -> Self {
        self.reload_folder();
        self
    }

    fn reload_folder(&mut self) {
        let settings = aio_core::InstallerSettings::load();
        self.retro = settings.retro.clone().unwrap_or_default();
        self.retro_saved = self.retro.clone();
        self.folder = settings.assets_folder();
        self.custom_folder = settings.assets_path.is_some();
    }

    fn set_folder(&mut self, folder: Option<PathBuf>) {
        let mut settings = aio_core::InstallerSettings::load();
        let default = aio_core::settings::default_assets_folder();
        settings.assets_path = folder.filter(|f| *f != default).map(|f| f.display().to_string());
        let _ = settings.save();
        self.reload_folder();
    }

    /// Saves the retro's URLs in Settings, so they are there next time.
    fn save_retro(&mut self) {
        let mut settings = aio_core::InstallerSettings::load();
        settings.retro = (!self.retro.is_empty()).then(|| self.retro.clone());
        if settings.save().is_ok() {
            self.retro_saved = self.retro.clone();
        }
    }

    fn start(&mut self, shared: &Shared, jobs: Vec<Job>) {
        let Some(hotel) = furniture_language::language(self.hotel) else { return };
        let cancel = CancellationToken::new();
        {
            let mut state = self.state.lock().unwrap();
            if state.running {
                return;
            }
            state.running = true;
            state.stopped = false;
            state.last_retro = jobs.iter().any(|j| !matches!(j, Job::Habbo(_)));
            state.cancel = Some(cancel.clone());
            state.results.clear();
            state.log.clear();
        }
        let retro_run = jobs.iter().any(|j| !matches!(j, Job::Habbo(_)));
        let root = if retro_run { retro::custom_folder(&self.folder) } else { self.folder.clone() };
        let retro_urls = self.retro.clone();
        let options = Options {
            not_sellable: self.not_sellable,
            effect_icons_source: Some(self.icon_source.trim().to_string()).filter(|s| !s.is_empty()),
        };
        let (state, task_shared) = (self.state.clone(), shared.clone());
        shared.set_status(t("Getting the assets ..."));
        shared.spawn(async move {
            let ctx = task_shared.ctx.clone();
            let log_state = state.clone();
            let log = move |line: &str| {
                log_state.lock().unwrap().log.push(line);
                ctx.request_repaint();
            };
            let from = if retro_run { retro::folder_url(&retro_urls.furnidata) } else { hotel.domain.to_string() };
            log(&format!("> {from} → {}", habbo_assets::gamedata_folder(&root).display()));
            let result = Downloader::new(&root, Urls::habbo(hotel), options, &log, cancel.clone());
            match result {
                Err(e) => state.lock().unwrap().results.push((t("Get Assets").into(), Err(e.to_string()))),
                Ok(downloader) => {
                    for job in jobs {
                        if cancel.is_cancelled() {
                            break;
                        }
                        let title = t(job.title()).to_string();
                        log(&format!("> {title}"));
                        let outcome = match job {
                            Job::Habbo(asset) => downloader.run(asset).await,
                            Job::RetroFurniture => downloader.retro_furniture(&retro_urls).await,
                            Job::RetroClothes => downloader.retro_clothes(&retro_urls).await,
                            // CPU work: on this worker thread, without holding up the others.
                            Job::ConvertSwf => tokio::task::block_in_place(|| retro::convert_sources(&root, &log, &cancel)),
                        }
                        .map_err(|e| e.to_string());
                        match &outcome {
                            Ok(summary) => log(&format!("  {summary}")),
                            Err(e) => log(&format!("! {title}: {e}")),
                        }
                        state.lock().unwrap().results.push((title, outcome));
                    }
                }
            }
            let mut s = state.lock().unwrap();
            s.running = false;
            s.stopped = cancel.is_cancelled();
            s.cancel = None;
            drop(s);
            task_shared.set_status(t("Ready."));
        });
    }

    fn take_picked(&mut self) {
        let (folder, source) = {
            let mut state = self.state.lock().unwrap();
            (state.picked_folder.take(), state.picked_source.take())
        };
        if let Some(folder) = folder {
            self.set_folder(Some(folder));
        }
        if let Some(source) = source {
            self.icon_source = source.display().to_string();
            // Choosing the app.asar means: get the effect icons.
            self.selected.insert(Asset::EffectIcons);
        }
    }

    fn pick_folder(&self, shared: &Shared) {
        let state = self.state.clone();
        let start = self.folder.clone();
        shared.spawn(async move {
            let dialog = rfd::AsyncFileDialog::new().set_title(t("Where Get Assets downloads to")).set_directory(start.parent().unwrap_or(&start));
            if let Some(folder) = dialog.pick_folder().await {
                state.lock().unwrap().picked_folder = Some(folder.path().to_path_buf());
            }
        });
    }

    fn pick_source(&self, shared: &Shared) {
        let state = self.state.clone();
        shared.spawn(async move {
            let dialog = rfd::AsyncFileDialog::new()
                .set_title(t("The app.asar of the Habbo Classic app"))
                .add_filter("app.asar", &["asar"]);
            if let Some(file) = dialog.pick_file().await {
                state.lock().unwrap().picked_source = Some(file.path().to_path_buf());
            }
        });
    }

    /// The Nitro retro tab: its renderer-config.json URLs (saved in Settings) and what to download from it.
    fn retro_cards(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared, running: bool, start: &mut Option<Vec<Job>>) {
        let margin = egui::Margin::symmetric(24, 20);
        let custom = habbo_assets::gamedata_folder(&retro::custom_folder(&self.folder));
        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("Nitro retro"), 15.0));
            widgets::muted(ui, palette, t("Custom downloads from a Nitro hotel; copy the values from its renderer-config.json."), 12.5);
            ui.add_space(4.0);
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("retro_urls").num_columns(2).spacing(egui::vec2(18.0, 10.0)).show(ui, |ui| {
                    for (i, (label, key)) in RETRO_FIELDS.iter().enumerate() {
                        ui.vertical(|ui| {
                            ui.set_min_width(230.0);
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new(t(label)).size(13.0).strong());
                            ui.label(RichText::new(format!("renderer-config.json: {key}")).size(11.0).color(palette.subtle));
                        });
                        let width = (ui.available_width() - 4.0).max(240.0);
                        ui.add(egui::TextEdit::singleline(retro_field(&mut self.retro, i)).hint_text("https://").desired_width(width).margin(egui::vec2(8.0, 6.0)));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let changed = self.retro != self.retro_saved;
                if ui.add_enabled(!running && changed, widgets::primary_button(t("Save"))).clicked() {
                    self.save_retro();
                }
                if changed {
                    ui.label(RichText::new(t("Not saved yet")).size(12.0).color(palette.warning_text));
                } else if !self.retro_saved.is_empty() {
                    ui.label(RichText::new(t("Saved: they are filled in again next time.")).size(12.0).color(palette.muted));
                }
            });
        });
        ui.add_space(18.0);

        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("What to download"), 15.0));
            widgets::muted(
                ui,
                palette,
                &tf(
                    "Into {folder}, laid out like gamedata, to upload by hand (Asset Explorer). A retro serves .hab, .nitro or .swf: every bundle is saved as .hab; a .swf is kept in source/ until the SWF converter is built in.",
                    &[("folder", &custom.display().to_string())],
                ),
                12.5,
            );
            ui.add_enabled_ui(!running, |ui| {
                ui.checkbox(&mut self.retro_furniture, RichText::new(t("Custom furniture")).size(13.5).strong());
                ui.indent("retro_furniture", |ui| {
                    ui.label(RichText::new(t("FurnitureData.json, every furniture bundle and its icons")).size(11.5).color(palette.muted));
                });
                ui.checkbox(&mut self.retro_clothes, RichText::new(t("Custom clothes")).size(13.5).strong());
                ui.indent("retro_clothes", |ui| {
                    ui.label(RichText::new(t("FigureData.json, FigureMap.json and every clothing bundle")).size(11.5).color(palette.muted));
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let jobs: Vec<Job> = [(self.retro_furniture, Job::RetroFurniture), (self.retro_clothes, Job::RetroClothes)]
                    .iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, job)| *job)
                    .collect();
                if ui.add_enabled(!running && !jobs.is_empty(), widgets::primary_button(t("Download"))).clicked() {
                    // What is downloaded from is kept too.
                    if self.retro != self.retro_saved {
                        self.save_retro();
                    }
                    *start = Some(jobs);
                }
                if ui.add(widgets::secondary_button(t("Open the folder"))).clicked() {
                    open_folder(&custom);
                }
                if ui
                    .add_enabled(!running, widgets::secondary_button(t("Convert the kept .swf files")))
                    .on_hover_text(t("Converts the .swf files in source/ to .hab, without downloading them again."))
                    .clicked()
                {
                    *start = Some(vec![Job::ConvertSwf]);
                }
                if running && ui.add(widgets::secondary_button(t("Stop"))).clicked() {
                    if let Some(cancel) = &self.state.lock().unwrap().cancel {
                        cancel.cancel();
                    }
                }
                if running {
                    ui.spinner();
                }
            });
        });
        ui.add_space(18.0);
        let _ = shared;
    }
}

impl View for GetAssetsView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.take_picked();
        let running = self.state.lock().unwrap().running;
        let folder = self.folder.clone();
        let margin = egui::Margin::symmetric(24, 20);
        let mut start: Option<Vec<Job>> = None;
        let mut done = false;
        if std::mem::take(&mut self.scroll_top) {
            ui.scroll_to_cursor(Some(egui::Align::TOP));
        }
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [(Tab::Habbo, t("Habbo hotel")), (Tab::Retro, t("Nitro retro"))] {
                    let button = egui::Button::new(RichText::new(label).size(13.5).strong())
                        .selected(self.tab == tab)
                        .min_size(egui::vec2(150.0, 34.0));
                    if ui.add(button).clicked() {
                        self.tab = tab;
                    }
                }
            });
        });
        ui.add_space(12.0);
        if self.tab == Tab::Retro {
            self.retro_cards(ui, palette, shared, running, &mut start);
        } else {

            widgets::card(ui, palette, margin, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.label(widgets::title_text(t("Hotel"), 15.0));
                widgets::muted(
                    ui,
                    palette,
                    t("The furnidata, productdata, texts and the furniture come from this hotel, in its language. Badges come from all nine hotels."),
                    12.5,
                );
                ui.add_enabled_ui(!running, |ui| {
                    if let Some(code) = super::default_catalog::language_choice(ui, palette, self.hotel) {
                        self.hotel = code;
                    }
                });
            });
            ui.add_space(18.0);

            widgets::card(ui, palette, margin, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.label(widgets::title_text(t("Folder"), 15.0));
                widgets::muted(
                    ui,
                    palette,
                    &tf(
                        "Everything goes into {folder}, laid out like {gamedata} on the server: Deploy Assets copies it as it is. Only .hab bundles, JSON and images are kept; nothing is downloaded twice.",
                        &[("folder", &habbo_assets::gamedata_folder(&folder).display().to_string()), ("gamedata", &aio_remote::octane::gamedata())],
                    ),
                    12.5,
                );
                ui.horizontal(|ui| {
                    if ui.add(widgets::secondary_button(t("Open the folder"))).clicked() {
                        open_folder(&habbo_assets::gamedata_folder(&folder));
                    }
                    if ui.add_enabled(!running, widgets::secondary_button(t("Choose another folder ..."))).clicked() {
                        self.pick_folder(shared);
                    }
                    if self.custom_folder && ui.add_enabled(!running, widgets::secondary_button(t("Back to the default folder"))).clicked() {
                        self.set_folder(None);
                    }
                });
            });
            ui.add_space(18.0);

            widgets::card(ui, palette, margin, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    ui.label(widgets::title_text(t("What to download"), 15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!running, egui::Button::new(t("None")).small()).clicked() {
                            self.selected.clear();
                        }
                        if ui.add_enabled(!running, egui::Button::new(t("All")).small()).clicked() {
                            self.selected = Asset::ALL.iter().copied().collect();
                        }
                    });
                });
                ui.add_enabled_ui(!running, |ui| {
                    let columns = if ui.available_width() > 760.0 { 2 } else { 1 };
                    let cell = ((ui.available_width() - 28.0 * (columns as f32 - 1.0)) / columns as f32).floor();
                    egui::Grid::new("get_assets_list").num_columns(columns).spacing(egui::vec2(28.0, 12.0)).show(ui, |ui| {
                        for (i, asset) in Asset::ALL.iter().copied().enumerate() {
                            let (title, what) = texts(asset);
                            let mut on = self.selected.contains(&asset);
                            ui.vertical(|ui| {
                                ui.set_width(cell);
                                ui.spacing_mut().item_spacing.y = 2.0;
                                if ui.checkbox(&mut on, RichText::new(t(title)).size(13.5).strong()).changed() {
                                    if on {
                                        self.selected.insert(asset);
                                    } else {
                                        self.selected.remove(&asset);
                                    }
                                }
                                ui.indent(("get_assets_item", i), |ui| {
                                    ui.add(egui::Label::new(RichText::new(t(what)).size(11.5).color(palette.muted)).wrap());
                                    ui.add(egui::Label::new(RichText::new(asset.target()).size(10.5).monospace().color(palette.subtle)).wrap());
                                });
                            });
                            if (i + 1) % columns == 0 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.not_sellable, t("Clothes: make every clothing set wearable (sellable off in FigureData.json)"));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("Effect icons from:")).size(12.5));
                        let typed = ui.add(
                            egui::TextEdit::singleline(&mut self.icon_source).hint_text(t("The app.asar of the Habbo Classic app")).desired_width(360.0),
                        );
                        if typed.changed() && !self.icon_source.trim().is_empty() {
                            self.selected.insert(Asset::EffectIcons);
                        }
                        if ui.add(widgets::secondary_button(t("Choose ..."))).clicked() {
                            self.pick_source(shared);
                        }
                    });
                    widgets::muted(
                        ui,
                        palette,
                        &tf(
                            "Habbo has no download link for the effect icons: they are in {bundle}, inside the Habbo Classic app's resources/app.asar. Choose that app.asar.",
                            &[("bundle", aio_remote::habbo_assets::hab::INVENTORY_BUNDLE)],
                        ),
                        11.5,
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let count = self.selected.len();
                    if ui
                        .add_enabled(!running && count > 0, widgets::primary_button(&tf("Download the selected ({count})", &[("count", &count.to_string())])))
                        .clicked()
                    {
                        start = Some(Asset::ALL.iter().copied().filter(|a| self.selected.contains(a)).map(Job::Habbo).collect());
                    }
                    let all_tip = t("Everything but the effect icons, which are added when a source for them is set above.");
                    if ui.add_enabled(!running, widgets::secondary_button(t("Download all"))).on_hover_text(all_tip).clicked() {
                        let with_icons = !self.icon_source.trim().is_empty();
                        start = Some(Asset::ALL.iter().copied().filter(|a| a.in_download_all() || with_icons).map(Job::Habbo).collect());
                    }
                    if running && ui.add(widgets::secondary_button(t("Stop"))).clicked() {
                        if let Some(cancel) = &self.state.lock().unwrap().cancel {
                            cancel.cancel();
                        }
                    }
                    if running {
                        ui.spinner();
                    }
                });
            });
            ui.add_space(18.0);
        }

        let (results, stopped) = {
            let state = self.state.lock().unwrap();
            (state.results.clone(), state.stopped)
        };
        let has_log = !self.state.lock().unwrap().log.is_empty();
        if has_log || running {
            widgets::card(ui, palette, margin, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                if !running && !results.is_empty() {
                    let failed = results.iter().filter(|(_, r)| r.is_err()).count();
                    let lines: Vec<String> = results
                        .iter()
                        .map(|(title, r)| match r {
                            Ok(summary) => format!("{title}: {summary}"),
                            Err(e) => format!("{title}: {e}"),
                        })
                        .collect();
                    let (kind, heading) = if stopped {
                        (ResultKind::Warning, t("Stopped"))
                    } else if failed > 0 {
                        (ResultKind::Warning, t("Done, with problems"))
                    } else {
                        (ResultKind::Success, t("The assets are downloaded"))
                    };
                    widgets::result_banner(ui, palette, kind, heading, &lines.join("\n"));
                    let last_retro = self.state.lock().unwrap().last_retro;
                    ui.horizontal(|ui| {
                        if last_retro {
                            if ui.add(widgets::primary_button("Asset Explorer")).on_hover_text(t("Upload the custom folder by hand.")).clicked() {
                                shared.request(Request::Go(Page::AssetExplorer));
                            }
                        } else if ui.add(widgets::primary_button(t("Deploy to the server"))).on_hover_text(t("Addons → Deploy Assets copies the folder to the server.")).clicked() {
                            shared.request(Request::Go(Page::DeployAssets));
                        }
                        let shown = if last_retro { retro::custom_folder(&folder) } else { folder.clone() };
                        if ui.add(widgets::secondary_button(t("Open the folder"))).clicked() {
                            open_folder(&habbo_assets::gamedata_folder(&shown));
                        }
                        if ui.add(widgets::secondary_button(t("Done"))).on_hover_text(t("Clears the result and the log, so you can start over.")).clicked() {
                            done = true;
                        }
                    });
                }
                let mut state = self.state.lock().unwrap();
                log_view::show(ui, "get_assets_log", &mut state.log, Some(300.0), t("The log appears here."));
            });
        }

        if done {
            let mut state = self.state.lock().unwrap();
            state.results.clear();
            state.log.clear();
            state.stopped = false;
            self.scroll_top = true;
        }
        if let Some(assets) = start {
            self.start(shared, assets);
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().running
    }
}
