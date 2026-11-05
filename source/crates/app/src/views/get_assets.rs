//! Get Assets: Habbo's own assets downloaded on this computer into a folder laid out like the server's gamedata, so
//! Deploy Assets copies it as it is; and a Nitro retro's custom furniture and clothes, into a folder of their own to
//! upload by hand. No server connection is needed. (All-in-1 Converter: Habbo Original and Nitro Custom Downloads)

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_remote::furniture_language;
use aio_core::settings::{RetroProfile, RetroUrls};
use aio_remote::habbo_assets::retro;
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

/// The two pages of the view: Habbo assets and Nitro retro.
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

/// What the Referer, Custom headers and Connection fields of a retro profile do (shown below each).
const REFERER_HELP: &str = n(
    "Tells the server which website the request comes from. Some hotels only hand out their files to their own website: fill in the hotel's address then. Empty (default): no Referer is sent.",
);
const HEADERS_HELP: &str = n(
    "Extra headers sent with every request, one \"Name: value\" per line. A User-Agent here replaces the built-in one (a recent Edge on Windows); Origin says which website asks. Empty (default): the built-in headers are used.",
);
const CONNECTION_HELP: &str = n(
    "Off (default): the built-in downloader. On: every file is downloaded with curl on this computer; Cloudflare's bot check often lets curl through where the built-in downloader is blocked. At once: how many files are downloaded at the same time; fewer is slower but less likely to be blocked.",
);

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
    /// The folder the last run downloaded into.
    last_root: PathBuf,
    /// A folder or file the pickers chose, taken over by the page on the next frame.
    picked_folder: Option<PathBuf>,
    picked_source: Option<PathBuf>,
    /// A JSON file chosen for one of the retro's fields: (field index, path).
    picked_retro: Option<(usize, PathBuf)>,
}

pub struct GetAssetsView {
    state: Arc<Mutex<State>>,
    hotel: &'static str,
    selected: BTreeSet<Asset>,
    not_sellable: bool,
    icon_source: String,
    /// Also the .swf of every library (into the asset tools); kept in Settings.
    also_swf: bool,
    /// The download folder, read from Settings once and again after a change.
    folder: PathBuf,
    custom_folder: bool,
    tab: Tab,
    /// The saved retro profiles, the one being edited (None: a new one) and its name and URLs as edited.
    profiles: Vec<RetroProfile>,
    current: Option<usize>,
    edit: RetroProfile,
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
            also_swf: aio_core::InstallerSettings::load().assets_with_swf,
            folder: PathBuf::new(),
            custom_folder: false,
            tab: Tab::Habbo,
            profiles: Vec::new(),
            current: None,
            edit: RetroProfile::default(),
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
        self.profiles = settings.retro_profiles();
        let selected = settings.retro_selected.as_deref().and_then(|name| self.profiles.iter().position(|p| p.name == name));
        self.current = selected.or(if self.profiles.is_empty() { None } else { Some(0) });
        self.edit = self.current.map(|i| self.profiles[i].clone()).unwrap_or_default();
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

    /// Habbo assets or Nitro retro: the sidebar's two pages share this view (and its run and log).
    pub fn set_retro(&mut self, retro: bool) {
        self.tab = if retro { Tab::Retro } else { Tab::Habbo };
    }

    /// The edited profile differs from what is saved (or is a new one with something in it).
    fn retro_changed(&self) -> bool {
        match self.current {
            Some(i) => self.profiles.get(i) != Some(&self.edit),
            None => !self.edit.name.trim().is_empty() || !self.edit.urls.is_empty(),
        }
    }

    fn write_profiles(&self) {
        let mut settings = aio_core::InstallerSettings::load();
        settings.retros = self.profiles.clone();
        settings.retro = None;
        settings.retro_selected = self.current.and_then(|i| self.profiles.get(i)).map(|p| p.name.clone());
        let _ = settings.save();
    }

    /// Saves the edited profile, so it is there next time. Without a name it is named after its host; a new profile
    /// with the name of a saved one replaces that one.
    fn save_retro(&mut self) {
        self.edit.name = self.edit.name.trim().to_string();
        if self.edit.name.is_empty() {
            self.edit.name = self.edit.urls.host().unwrap_or_else(|| "retro".into());
        }
        let same_name = self.profiles.iter().position(|p| p.name.eq_ignore_ascii_case(&self.edit.name));
        let index = match (self.current, same_name) {
            (Some(i), Some(j)) if i != j => {
                self.profiles[j] = self.edit.clone();
                self.profiles.remove(i);
                if j > i { j - 1 } else { j }
            }
            (Some(i), _) => {
                self.profiles[i] = self.edit.clone();
                i
            }
            (None, Some(j)) => {
                self.profiles[j] = self.edit.clone();
                j
            }
            (None, None) => {
                self.profiles.push(self.edit.clone());
                self.profiles.len() - 1
            }
        };
        self.current = Some(index);
        self.write_profiles();
    }

    fn select_profile(&mut self, index: Option<usize>) {
        self.current = index.filter(|i| *i < self.profiles.len());
        self.edit = self.current.map(|i| self.profiles[i].clone()).unwrap_or_default();
        if self.current.is_some() {
            self.write_profiles();
        }
    }

    fn delete_profile(&mut self) {
        if let Some(i) = self.current {
            self.profiles.remove(i);
            self.current = if self.profiles.is_empty() { None } else { Some(0) };
            self.edit = self.current.map(|i| self.profiles[i].clone()).unwrap_or_default();
            self.write_profiles();
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
        let root = if retro_run { retro::custom_folder(&self.folder, &self.edit) } else { self.folder.clone() };
        self.state.lock().unwrap().last_root = root.clone();
        let retro_urls = self.edit.urls.clone();
        // The profile's Referer and custom headers, for a retro run only (Habbo's CDN needs none).
        let headers = if retro_run { retro_urls.request_headers() } else { Ok(Vec::new()) };
        let (headers, header_error) = match headers {
            Ok(headers) => (headers, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let options = Options {
            not_sellable: self.not_sellable,
            effect_icons_source: Some(self.icon_source.trim().to_string()).filter(|s| !s.is_empty()),
            also_swf: self.also_swf,
            headers,
            use_curl: retro_run && retro_urls.use_curl,
            parallel: if retro_run { retro_urls.parallel as usize } else { 0 },
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
            if !options.headers.is_empty() {
                let names: Vec<&str> = options.headers.iter().map(|(name, _)| name.as_str()).collect();
                log(&format!("  with the headers {}", names.join(", ")));
            }
            let result = match header_error {
                Some(e) => Err(anyhow::anyhow!(e)),
                None => Downloader::new(&root, Urls::habbo(hotel), options, &log, cancel.clone()),
            };
            match result {
                Err(e) => state.lock().unwrap().results.push((t("Habbo assets").into(), Err(e.to_string()))),
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
        let picked_retro = self.state.lock().unwrap().picked_retro.take();
        if let Some((index, path)) = picked_retro {
            *retro_field(&mut self.edit.urls, index) = path.display().to_string();
        }
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
            let dialog = rfd::AsyncFileDialog::new().set_title(t("Where the assets are downloaded to")).set_directory(start.parent().unwrap_or(&start));
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
        let custom = habbo_assets::gamedata_folder(&retro::custom_folder(&self.folder, &self.edit));
        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("Nitro retro"), 15.0));
            widgets::muted(ui, palette, t("Custom downloads from a Nitro hotel; copy the values from its renderer-config.json."), 12.5);
            ui.add_space(4.0);
            // The saved profiles, one click to switch; a new one starts empty.
            let mut pick: Option<Option<usize>> = None;
            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(t("Profiles:")).size(12.5).color(palette.muted));
                    for (i, profile) in self.profiles.iter().enumerate() {
                        let button = egui::Button::new(RichText::new(&profile.name).size(13.0).strong()).selected(self.current == Some(i));
                        if ui.add(button).clicked() {
                            pick = Some(Some(i));
                        }
                    }
                    let new = egui::Button::new(RichText::new(t("+ New profile")).size(13.0)).selected(self.current.is_none());
                    if ui.add(new).clicked() {
                        pick = Some(None);
                    }
                });
            });
            if let Some(index) = pick {
                self.select_profile(index);
            }
            ui.add_space(4.0);
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("retro_urls").num_columns(2).spacing(egui::vec2(18.0, 10.0)).show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_min_width(230.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(t("Name")).size(13.0).strong());
                        ui.label(RichText::new(format!("custom/{}", self.edit.folder_name())).size(11.0).color(palette.subtle));
                    });
                    let width = (ui.available_width() - 4.0).max(240.0);
                    let hint = self.edit.urls.host().unwrap_or_else(|| t("For example the hotel's name").into());
                    ui.add(egui::TextEdit::singleline(&mut self.edit.name).hint_text(hint).desired_width(width).margin(egui::vec2(8.0, 6.0)));
                    ui.end_row();
                    ui.vertical(|ui| {
                        ui.set_min_width(230.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(t("Format")).size(13.0).strong());
                        ui.label(RichText::new(t("what the retro serves")).size(11.0).color(palette.subtle));
                    });
                    ui.horizontal(|ui| {
                        for (value, label) in [("", t("Auto (try all)")), ("hab", ".hab"), ("nitro", ".nitro"), ("swf", ".swf")] {
                            ui.radio_value(&mut self.edit.urls.format, value.to_string(), label);
                        }
                    });
                    ui.end_row();
                    ui.vertical(|ui| {
                        ui.set_min_width(230.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("Referer").size(13.0).strong());
                        ui.label(RichText::new(t("sent with every request (optional)")).size(11.0).color(palette.subtle));
                    });
                    // The examples are hints only: an empty field means the default.
                    ui.vertical(|ui| {
                        let width = (ui.available_width() - 4.0).max(240.0);
                        ui.add(egui::TextEdit::singleline(&mut self.edit.urls.referer).hint_text("https://example.com/").desired_width(width).margin(egui::vec2(8.0, 6.0)));
                        ui.label(RichText::new(t(REFERER_HELP)).size(11.5).color(palette.muted));
                    });
                    ui.end_row();
                    ui.vertical(|ui| {
                        ui.set_min_width(230.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(t("Custom headers")).size(13.0).strong());
                        ui.label(RichText::new(t("one \"Name: value\" per line (optional)")).size(11.0).color(palette.subtle));
                    });
                    ui.vertical(|ui| {
                        let width = (ui.available_width() - 4.0).max(240.0);
                        ui.add(
                            egui::TextEdit::multiline(&mut self.edit.urls.headers)
                                .hint_text(format!("User-Agent:{}\nOrigin:https://example.com/", furniture_language::USER_AGENT))
                                .desired_rows(4)
                                .desired_width(width)
                                .margin(egui::vec2(8.0, 6.0)),
                        );
                        if let Err(e) = self.edit.urls.request_headers() {
                            ui.label(RichText::new(e).size(11.5).color(palette.warning_text));
                        }
                        ui.label(RichText::new(t(HEADERS_HELP)).size(11.5).color(palette.muted));
                    });
                    ui.end_row();
                    ui.vertical(|ui| {
                        ui.set_min_width(230.0);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(t("Connection")).size(13.0).strong());
                        ui.label(RichText::new(t("for a retro behind Cloudflare")).size(11.0).color(palette.subtle));
                    });
                    ui.vertical(|ui| {
                        ui.checkbox(&mut self.edit.urls.use_curl, RichText::new(t("Download with curl")).size(13.0))
                            .on_hover_text(t("Every request through the system's curl (curl.exe on Windows 10 and later). Cloudflare's bot check often lets curl through where the built-in downloader is blocked."));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t("At once:")).size(12.5).color(palette.muted));
                            for (value, label) in [(0u8, "16"), (8, "8"), (4, "4"), (1, "1")] {
                                ui.radio_value(&mut self.edit.urls.parallel, value, label);
                            }
                        });
                        ui.label(RichText::new(t(CONNECTION_HELP)).size(11.5).color(palette.muted));
                    });
                    ui.end_row();
                    for (i, (label, key)) in RETRO_FIELDS.iter().enumerate() {
                        ui.vertical(|ui| {
                            ui.set_min_width(230.0);
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new(t(label)).size(13.0).strong());
                            ui.label(RichText::new(format!("renderer-config.json: {key}")).size(11.0).color(palette.subtle));
                        });
                        // The JSON fields (FurnitureData, FigureData, FigureMap) also take a file on this computer.
                        let json = matches!(i, 0 | 4 | 5);
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - if json { 104.0 } else { 4.0 }).max(200.0);
                            let hint = if json { t("https:// or a file on this computer") } else { "https://" };
                            ui.add(egui::TextEdit::singleline(retro_field(&mut self.edit.urls, i)).hint_text(hint).desired_width(width).margin(egui::vec2(8.0, 6.0)));
                            if json && ui.add(widgets::secondary_button(t("Choose ..."))).on_hover_text(t("A JSON or XML file on this computer, saved from the browser for example.")).clicked() {
                                let state = self.state.clone();
                                shared.spawn(async move {
                                    let dialog = rfd::AsyncFileDialog::new().add_filter("JSON / XML", &["json", "xml", "txt"]);
                                    if let Some(file) = dialog.pick_file().await {
                                        state.lock().unwrap().picked_retro = Some((i, file.path().to_path_buf()));
                                    }
                                });
                            }
                        });
                        ui.end_row();
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let changed = self.retro_changed();
                if ui.add_enabled(!running && changed, widgets::primary_button(t("Save"))).clicked() {
                    self.save_retro();
                }
                if self.current.is_some()
                    && ui.add_enabled(!running, widgets::secondary_button(t("Delete profile"))).on_hover_text(t("Removes the profile; its downloaded folder stays.")).clicked()
                {
                    self.delete_profile();
                }
                if changed {
                    ui.label(RichText::new(t("Not saved yet")).size(12.0).color(palette.warning_text));
                } else if self.current.is_some() {
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
                    "Into {folder}, laid out like gamedata, to upload by hand (Asset Explorer). A retro serves .hab, .nitro or .swf: every bundle is saved as .hab (a .nitro repacked, a .swf converted); a .swf that cannot be converted is kept in source/.",
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
                    if self.retro_changed() {
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
                    if widgets::switch(ui, palette, &mut self.also_swf).changed() {
                        let mut settings = aio_core::InstallerSettings::load();
                        settings.assets_with_swf = self.also_swf;
                        let _ = settings.save();
                    }
                    let label = if self.also_swf { t("Download .hab and .swf") } else { t("Download only .hab") };
                    ui.label(RichText::new(label).size(13.0).strong());
                });
                widgets::muted(
                    ui,
                    palette,
                    t("With .swf on, furniture, clothes, effects and pets also get their .swf, into Assets_tools\\Original_SWF_Files, for the asset tools. They are never sent to the server."),
                    11.5,
                );
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
                        let shown = if last_retro { self.state.lock().unwrap().last_root.clone() } else { folder.clone() };
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
