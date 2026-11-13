//! Client configuration: the Octane client's settings files (`<octane>/public/configuration`): renderer-config,
//! ui-config, client-mode, news, hot looks and the rest. On the left the files with plain names, on the right the chosen
//! one as a form (sections, switches, numbers, text, groups that open, lists) with a search, or as text with a live
//! check. Saving keeps a backup, and writes the served copy too, so players get it when they reload the hotel.

use std::sync::Arc;

use aio_remote::client_config::{self as config, ConfigFile, Loaded};
use aio_remote::Server;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use serde_json::Value;

use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{n, t, tf};

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Form,
    Text,
}

/// The file being edited.
struct Doc {
    loaded: Loaded,
    /// The settings as edited (None: the file is edited as text only, as it is not a list of settings).
    value: Option<Value>,
    saved_value: Option<Value>,
    /// The text as edited (Text mode) and as loaded.
    text: String,
    saved_text: String,
    mode: Mode,
    /// Why the text is not valid JSON, when it is not.
    problem: Option<String>,
    /// Settings this Octane version's example has that the file lacks.
    missing: Vec<String>,
    /// A .jsonc file's comments, put back when it is saved from the form.
    comments: Option<config::Comments>,
}

impl Doc {
    fn new(loaded: Loaded) -> Self {
        let jsonc = loaded.name.ends_with(".jsonc");
        let value = config::parse(&loaded.name, &loaded.text).ok().filter(Value::is_object);
        let comments = jsonc.then(|| config::comments(&loaded.text));
        let mode = if value.is_some() { Mode::Form } else { Mode::Text };
        let mut doc = Doc {
            text: loaded.text.clone(),
            saved_text: loaded.text.clone(),
            saved_value: value.clone(),
            value,
            mode,
            problem: None,
            missing: Vec::new(),
            comments,
            loaded,
        };
        doc.check();
        doc
    }

    /// The text as it would be saved now.
    fn current_text(&self) -> String {
        match (&self.mode, &self.value, &self.comments) {
            // A .jsonc file keeps its comments and its own indent.
            (Mode::Form, Some(value), Some(comments)) => config::write_with_comments(value, &config::jsonc_layout(&self.text), comments),
            (Mode::Form, Some(value), None) => config::write(value, &self.loaded.layout),
            _ => self.text.clone(),
        }
    }

    fn changed(&self) -> bool {
        self.loaded.is_new()
            || match (&self.mode, &self.value) {
                (Mode::Form, Some(value)) => Some(value) != self.saved_value.as_ref(),
                _ => self.text != self.saved_text,
            }
    }

    /// The text's problem, and the settings the example adds.
    fn check(&mut self) {
        self.problem = match self.mode {
            Mode::Text => config::parse(&self.loaded.name, &self.text).err(),
            Mode::Form => None,
        };
        let current = match self.mode {
            Mode::Form => self.value.clone(),
            Mode::Text => config::parse(&self.loaded.name, &self.text).ok(),
        };
        self.missing = match (self.loaded.example.as_deref().and_then(|e| config::parse("x.example", e).ok()), current) {
            (Some(example), Some(current)) if example.is_object() && current.is_object() => config::add_from_example(&example, &current).1,
            _ => Vec::new(),
        };
    }

    fn switch_to_text(&mut self) {
        if self.mode == Mode::Form && self.value.is_some() {
            self.text = self.current_text();
        }
        self.mode = Mode::Text;
        self.check();
    }

    /// Back to the form: only when the text is valid JSON with settings in it.
    fn switch_to_form(&mut self) -> bool {
        match config::parse(&self.loaded.name, &self.text) {
            Ok(value) if value.is_object() => {
                if self.comments.is_some() {
                    self.comments = Some(config::comments(&self.text));
                }
                self.value = Some(value);
                self.mode = Mode::Form;
                self.check();
                true
            }
            _ => false,
        }
    }
}

#[derive(Default)]
struct State {
    files: Option<Vec<ConfigFile>>,
    selected: Option<String>,
    doc: Option<Doc>,
    search: String,
    busy: bool,
    status: Status,
    generation: u64,
    listed_for: Option<usize>,
    sample: bool,
}

impl State {
    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }
}

pub struct ClientConfigView {
    state: Arc<Mutex<State>>,
}

#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

impl Ctl {
    fn lock(&self) -> parking_lot::MutexGuard<'_, State> {
        self.state.lock()
    }

    fn with<R>(&self, generation: u64, change: impl FnOnce(&mut State) -> R) -> Option<R> {
        let result = {
            let mut state = self.lock();
            (state.generation == generation).then(|| change(&mut state))
        };
        self.shared.ctx.request_repaint();
        result
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, task: impl FnOnce(Ctl) -> F) {
        self.shared.spawn(task(self.clone()));
    }

    fn start(&self) -> Option<(u64, Arc<dyn Server>)> {
        let server = self.shared.session()?;
        let mut state = self.lock();
        if state.busy || state.sample {
            return None;
        }
        if server.is_dry_run() {
            state.error(t("Dry run: there are no files to edit.").into());
            return None;
        }
        state.busy = true;
        Some((state.generation, server))
    }

    async fn list(self) {
        let Some((generation, server)) = self.start() else { return };
        let files = config::list(&*server).await;
        self.with(generation, |s| {
            s.busy = false;
            match files {
                Ok(files) if files.is_empty() => {
                    s.files = Some(files);
                    s.error(tf("{folder} has no settings files: run the Octane step first.", &[("folder", &aio_remote::octane::configuration_directory())]));
                }
                Ok(files) => s.files = Some(files),
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    async fn open(self, name: String) {
        let Some((generation, server)) = self.start() else { return };
        let loaded = config::load(&*server, &name).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(loaded) => {
                    let message = if loaded.is_new() {
                        tf("{file} is not made yet: this is its example. Save to make it.", &[("file", &name)])
                    } else {
                        tf("{file} loaded.", &[("file", &name)])
                    };
                    s.selected = Some(name);
                    s.doc = Some(Doc::new(loaded));
                    s.search.clear();
                    s.ok(message);
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    async fn save(self) {
        let Some((loaded, text)) = self.lock().doc.as_ref().map(|d| (d.loaded.clone(), d.current_text())) else { return };
        let Some((generation, server)) = self.start() else { return };
        let log = |_: &str| {};
        let saved = config::save(&*server, &loaded, &text, &log).await;
        let name = loaded.name.clone();
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(saved) => {
                    s.ok(if saved.live {
                        tf("{file} saved. Players get it when they reload the hotel.", &[("file", &name)])
                    } else {
                        tf("{file} saved. The hotel serves the built copy: run Rebuild Octane (Tools) to use it.", &[("file", &name)])
                    });
                }
                Err(error) => s.error(error.to_string()),
            }
        });
        // Read it back: the checksum, the backup and what the hotel now has.
        self.clone().open(name).await;
        self.list().await;
    }
}

impl ClientConfigView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())) }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

    fn list_if_needed(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        let id = Arc::as_ptr(&server) as *const () as usize;
        {
            let mut s = self.state.lock();
            if s.listed_for == Some(id) {
                return;
            }
            s.listed_for = Some(id);
            if server.is_dry_run() && std::env::var_os("AIO_SCREENSHOT").is_some() {
                sample(&mut s);
                return;
            }
        }
        self.ctl(shared).spawn(|c| c.list());
    }
}

enum Action {
    List,
    Open(String),
    Save,
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

impl View for ClientConfigView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.list_if_needed(shared);
        let mut actions = Vec::new();
        {
            let mut guard = self.state.lock();
            let s = &mut *guard;
            egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 18 }).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                header(ui, palette, s, shared, &mut actions);
                ui.add_space(8.0);
                egui::SidePanel::left("client-config-files")
                    .resizable(true)
                    .default_width(270.0)
                    .width_range(220.0..=380.0)
                    .frame(widgets::card_frame(palette).inner_margin(10).corner_radius(10).outer_margin(egui::Margin { left: 0, right: 6, top: 0, bottom: 0 }))
                    .show_inside(ui, |ui| files(ui, palette, s, &mut actions));
                egui::CentralPanel::default().frame(widgets::card_frame(palette).inner_margin(10).corner_radius(10)).show_inside(ui, |ui| editor(ui, palette, s, &mut actions));
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::List => ctl.spawn(|c| c.list()),
                Action::Open(name) => ctl.spawn(move |c| c.open(name)),
                Action::Save => ctl.spawn(|c| c.save()),
            }
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().busy
    }

    fn session_changed(&mut self, _shared: &Shared) {
        let mut s = self.state.lock();
        let generation = s.generation + 1;
        *s = State { generation, ..State::default() };
    }
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            if ui.add_enabled(!s.busy && shared.session().is_some(), small(t("Refresh"))).clicked() {
                actions.push(Action::List);
                if let Some(name) = s.selected.clone() {
                    actions.push(Action::Open(name));
                }
            }
            if s.busy {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Client configuration")).size(13.0).strong());
                widgets::muted(
                    ui,
                    palette,
                    &tf(
                        "The Octane client's settings in {folder}. A save keeps the old file as .bak-<time> and goes live at once: players get it when they reload the hotel.",
                        &[("folder", &aio_remote::octane::configuration_directory())],
                    ),
                    12.0,
                );
            });
        });
    });
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
}

/// A file's plain name and what it is for.
fn describe(name: &str) -> (String, &'static str) {
    let stem = name.trim_end_matches(".jsonc").trim_end_matches(".json");
    let known: Option<(&str, &'static str)> = match stem {
        "renderer-config" => Some((n("Connection and assets"), n("Where the client connects (socket.url) and where it finds the gamedata, furniture, clothes and images."))),
        "ui-config" => Some((n("Interface"), n("Login screen, hotel view, toolbar, catalogue, navigator and every other part of the interface."))),
        "client-mode" => Some((n("Client mode"), n("Where the client finds its configuration, and the login it uses."))),
        "news" => Some((n("Login news"), n("The news on the login screen."))),
        "hotlooks" => Some((n("Hot looks"), n("The looks the avatar editor offers."))),
        "infostand_backgrounds" => Some((n("Infostand backgrounds"), n("The backgrounds players can choose for their infostand."))),
        "pets" => Some((n("Pets"), n("The pets and their colours the client knows."))),
        "adsense" => Some((n("Advertising"), n("Whether the client shows ads, and where they come from."))),
        "radio-stations" => Some((n("Radio stations"), n("The stations of the hotel radio."))),
        "soundboard-layout" => Some((n("Soundboard layout"), n("How the soundboard is laid out."))),
        "soundboard-sounds" => Some((n("Soundboard sounds"), n("The sounds of the soundboard."))),
        _ => None,
    };
    match known {
        Some((title, what)) => (t(title).to_string(), t(what)),
        None if stem.starts_with("housekeeping-texts-") => (tf("Housekeeping texts ({language})", &[("language", stem.trim_start_matches("housekeeping-texts-"))]), t("The texts of the in-hotel housekeeping panel.")),
        None if stem.starts_with("rarevalues-texts-") => (tf("Rare values texts ({language})", &[("language", stem.trim_start_matches("rarevalues-texts-"))]), t("The texts of the rare values window.")),
        None => (stem.replace(['-', '_'], " "), ""),
    }
}

/// The group a file is listed under.
fn group(name: &str) -> usize {
    let stem = name.trim_end_matches(".jsonc").trim_end_matches(".json");
    match stem {
        "renderer-config" | "ui-config" | "client-mode" => 0,
        "news" | "hotlooks" | "infostand_backgrounds" | "pets" | "adsense" | "radio-stations" | "soundboard-layout" | "soundboard-sounds" => 1,
        _ if stem.contains("texts") => 2,
        _ => 3,
    }
}

fn files(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some(list) = s.files.clone() else {
        widgets::muted(ui, palette, if s.busy { t("Reading the settings files ...") } else { t("Log in to a server to edit its client settings.") }, 12.5);
        return;
    };
    egui::ScrollArea::vertical().id_salt("client-config-file-list").auto_shrink([false, false]).show(ui, |ui| {
        for (index, heading) in [t("Main"), t("Content"), t("Texts"), t("Other")].into_iter().enumerate() {
            let in_group: Vec<&ConfigFile> = list.iter().filter(|f| group(&f.name) == index).collect();
            if in_group.is_empty() {
                continue;
            }
            ui.add_space(if index == 0 { 0.0 } else { 10.0 });
            ui.label(RichText::new(heading.to_uppercase()).size(11.0).strong().color(palette.muted));
            ui.add_space(2.0);
            for file in in_group {
                let (title, what) = describe(&file.name);
                let selected = s.selected.as_deref() == Some(file.name.as_str());
                let fill = if selected { palette.accent_text.gamma_multiply(0.18) } else { egui::Color32::TRANSPARENT };
                let response = egui::Frame::new().fill(fill).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&title).size(13.0).strong());
                        if file.size.is_none() {
                            ui.label(RichText::new(t("not made yet")).size(10.5).color(palette.warning_text));
                        } else if !file.served {
                            ui.label(RichText::new(t("not served")).size(10.5).color(palette.warning_text)).on_hover_text(t("The built client has no copy: run Rebuild Octane (Tools)."));
                        }
                    });
                    ui.label(RichText::new(&file.name).size(11.0).monospace().color(palette.subtle));
                })
                .response
                .interact(egui::Sense::click());
                let response = if what.is_empty() { response } else { response.on_hover_text(what) };
                if response.clicked() && !selected && !s.busy {
                    if s.doc.as_ref().is_some_and(Doc::changed) && !s.doc.as_ref().is_some_and(|d| d.loaded.is_new()) {
                        s.error(t("Save or undo the changes first.").into());
                    } else {
                        actions.push(Action::Open(file.name.clone()));
                    }
                }
            }
        }
    });
}

fn editor(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy;
    let Some(doc) = s.doc.as_mut() else {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(RichText::new(t("Choose a settings file on the left")).size(15.0).strong());
            widgets::muted(ui, palette, t("Connection and assets (renderer-config) and Interface (ui-config) hold most of the client's settings."), 12.5);
        });
        return;
    };
    let (title, what) = describe(&doc.loaded.name);
    ui.horizontal(|ui| {
        ui.label(widgets::title_text(&title, 17.0));
        ui.label(RichText::new(&doc.loaded.name).size(12.0).monospace().color(palette.subtle));
    });
    if !what.is_empty() {
        widgets::muted(ui, palette, what, 12.0);
    }
    ui.add_space(6.0);

    // The bar: form or text, the search, save and undo.
    let changed = doc.changed();
    let can_form = doc.saved_value.is_some();
    ui.horizontal(|ui| {
        for (mode, label) in [(Mode::Form, t("Settings")), (Mode::Text, t("Text"))] {
            let enabled = mode == Mode::Text || can_form;
            let response = ui.add_enabled(enabled, egui::SelectableLabel::new(doc.mode == mode, RichText::new(label).size(13.0)));
            let response = if mode == Mode::Form && !can_form { response.on_disabled_hover_text(t("This file is not a list of settings: it is edited as text.")) } else { response };
            if response.clicked() && doc.mode != mode {
                match mode {
                    Mode::Text => doc.switch_to_text(),
                    Mode::Form => {
                        if !doc.switch_to_form() {
                            doc.check();
                        }
                    }
                }
            }
        }
        ui.add_space(10.0);
        if doc.mode == Mode::Form {
            ui.add_sized([240.0, 26.0], egui::TextEdit::singleline(&mut s.search).hint_text(t("Search settings")).vertical_align(egui::Align::Center));
            if !s.search.is_empty() && ui.add(small("✕")).clicked() {
                s.search.clear();
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if doc.loaded.is_new() { t("Make the file") } else { t("Save") };
            if ui.add_enabled(!busy && changed && doc.problem.is_none(), widgets::primary_button(label)).clicked() {
                actions.push(Action::Save);
            }
            if changed && !doc.loaded.is_new() && ui.add_enabled(!busy, small(t("Undo the changes"))).clicked() {
                doc.text = doc.saved_text.clone();
                doc.value = doc.saved_value.clone();
                doc.mode = if doc.value.is_some() { Mode::Form } else { Mode::Text };
                doc.check();
            }
            if changed {
                ui.label(RichText::new(t("Unsaved changes")).size(12.0).color(palette.warning_text));
            }
        });
    });
    if let Some(problem) = &doc.problem {
        ui.label(RichText::new(tf("Not valid JSON, {problem}: it cannot be saved like this.", &[("problem", problem)])).size(12.0).color(palette.error_text));
    }
    if !doc.missing.is_empty() {
        ui.add_space(4.0);
        egui::Frame::new().fill(palette.accent_text.gamma_multiply(0.12)).corner_radius(8).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let shown: Vec<&str> = doc.missing.iter().take(6).map(String::as_str).collect();
                let more = doc.missing.len().saturating_sub(shown.len());
                let mut list = shown.join(", ");
                if more > 0 {
                    list.push_str(&tf(" and {count} more", &[("count", &more.to_string())]));
                }
                let text = if doc.missing.len() == 1 {
                    tf("This Octane version has a setting this file lacks: {list}", &[("list", &list)])
                } else {
                    tf("This Octane version has {count} settings this file lacks: {list}", &[("count", &doc.missing.len().to_string()), ("list", &list)])
                };
                ui.label(RichText::new(text).size(12.0));
                if ui.add_enabled(!busy && doc.mode == Mode::Form, small(t("Add them"))).on_disabled_hover_text(t("Switch to Settings first.")).clicked() {
                    if let (Some(example), Some(value)) = (doc.loaded.example.as_deref().and_then(|e| config::parse("x.example", e).ok()), doc.value.as_ref()) {
                        doc.value = Some(config::add_from_example(&example, value).0);
                        doc.check();
                    }
                }
            });
        });
    }
    ui.add_space(6.0);
    ui.separator();

    match doc.mode {
        Mode::Text => {
            egui::ScrollArea::vertical().id_salt("client-config-text").auto_shrink([false, false]).show(ui, |ui| {
                let response = ui.add(
                    egui::TextEdit::multiline(&mut doc.text).font(egui::TextStyle::Monospace).code_editor().desired_width(f32::INFINITY).desired_rows(30).lock_focus(true),
                );
                if response.changed() {
                    doc.check();
                }
            });
        }
        Mode::Form => {
            let query = s.search.trim().to_lowercase();
            let saved = doc.saved_value.clone();
            let mut edited = false;
            egui::ScrollArea::vertical().id_salt(("client-config-form", doc.loaded.name.clone())).auto_shrink([false, false]).show(ui, |ui| {
                if let Some(Value::Object(map)) = doc.value.as_mut() {
                    let saved_map = saved.as_ref().and_then(Value::as_object);
                    let notes = Notes { comments: doc.comments.as_ref(), path: String::new() };
                    edited |= object_rows(ui, palette, map, saved_map, "", &query, 0, &notes);
                    if !query.is_empty() && !map.iter().any(|(k, v)| matches(k, v, &query, "")) {
                        widgets::muted(ui, palette, t("No setting matches the search."), 12.5);
                    }
                }
            });
            if edited {
                doc.check();
            }
        }
    }
}

/// The section heading a key stands for (`"// ---- Login screen ----": ""`), if it is one.
fn section(key: &str) -> Option<String> {
    let rest = key.strip_prefix("//")?;
    let title = rest.trim().trim_matches('-').trim();
    Some(title.to_string())
}

/// Whether the setting (or one inside it) matches the search.
fn matches(key: &str, value: &Value, query: &str, path: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let full = format!("{path}{key}").to_lowercase();
    if full.contains(query) {
        return true;
    }
    match value {
        Value::Object(map) => map.iter().any(|(k, v)| matches(k, v, query, &format!("{full}."))),
        Value::Array(items) => items.iter().any(|v| matches("", v, query, &format!("{full}."))),
        Value::String(text) => text.to_lowercase().contains(query),
        other => other.to_string().to_lowercase().contains(query),
    }
}

/// A short help for a well-known setting.
fn hint(key: &str) -> Option<&'static str> {
    Some(match key {
        "socket.url" => t("The emulator's WebSocket address the client connects to (wss://...)."),
        "asset.url" => t("The web address of gamedata."),
        "image.library.url" => t("Where the catalogue, badge and hotel view images are (c_images)."),
        "hof.furni.url" => t("The address furniture images are read from."),
        "furni.asset.url" => t("The furniture files: %libname% becomes the furniture's name."),
        "furni.asset.icon.url" => t("The furniture icons in the catalogue and inventory."),
        "pet.asset.url" => t("The pet files."),
        "avatar.asset.url" => t("The clothes files."),
        "avatar.asset.effect.url" => t("The effect files."),
        "furnidata.url" => t("FurnitureData.json: names and settings of every furniture."),
        "productdata.url" => t("ProductData.json: the product names."),
        "avatar.figuredata.url" => t("FigureData.json: the clothes the avatar editor offers."),
        "avatar.figuremap.url" => t("FigureMap.json: which file holds which clothing part."),
        "avatar.effectmap.url" => t("EffectMap.json: which file holds which effect."),
        "external.texts.url" => t("The game's texts (ExternalTexts.json)."),
        "external.texts.txt" => t("The game's texts (ExternalTexts.json)."),
        "api.url" => t("The website the client calls for its web parts."),
        "camera.url" => t("Where camera photos are kept."),
        "thumbnails.url" => t("Where room thumbnails are kept."),
        "url.prefix" => t("The website's address."),
        "habbopages.url" => t("Where the help pages are."),
        "group.badge.url" => t("The group badge images."),
        _ => return None,
    })
}

/// The rows of an object's settings. Returns whether something was changed.
/// A .jsonc file's comments, and where in the file the rows are: what the form shows as help beside the settings.
struct Notes<'a> {
    comments: Option<&'a config::Comments>,
    /// The keys from the top, joined by "\u{1}" (as [`config::Comments`] has them).
    path: String,
}

impl<'a> Notes<'a> {
    fn child(&self, key: &str) -> Notes<'a> {
        let path = if self.path.is_empty() { key.to_string() } else { format!("{}\u{1}{key}", self.path) };
        Notes { comments: self.comments, path }
    }

    /// No comments for what is inside a list (they are not followed there).
    fn none() -> Notes<'a> {
        Notes { comments: None, path: String::new() }
    }

    fn above(&self) -> Vec<String> {
        self.comments.and_then(|c| c.before.get(&self.path)).map(|lines| lines.iter().map(|l| comment_text(l)).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
    }

    fn beside(&self) -> Option<String> {
        self.comments.and_then(|c| c.after.get(&self.path)).map(|l| comment_text(l)).filter(|l| !l.is_empty())
    }
}

/// A comment as help text: without its // or /* */ marks.
fn comment_text(comment: &str) -> String {
    let lines: Vec<String> = comment
        .lines()
        .map(|line| line.trim().trim_start_matches("//").trim_start_matches("/*").trim_end_matches("*/").trim_start_matches('*').trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    lines.join(" ")
}

#[allow(clippy::too_many_arguments)]
fn object_rows(ui: &mut egui::Ui, palette: &Palette, map: &mut serde_json::Map<String, Value>, saved: Option<&serde_json::Map<String, Value>>, path: &str, query: &str, depth: usize, notes: &Notes) -> bool {
    let mut edited = false;
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if let Some(title) = section(&key) {
            if query.is_empty() && !title.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new(title).size(14.5).strong().color(palette.accent_text));
                ui.add_space(2.0);
            }
            continue;
        }
        let Some(value) = map.get_mut(&key) else { continue };
        if !matches(&key, value, query, path) {
            continue;
        }
        let before = saved.and_then(|m| m.get(&key));
        edited |= setting(ui, palette, &key, value, before, &format!("{path}{key}."), query, depth, &notes.child(&key));
    }
    edited
}

/// One setting: its name and the editor for its kind of value.
#[allow(clippy::too_many_arguments)]
fn setting(ui: &mut egui::Ui, palette: &Palette, key: &str, value: &mut Value, saved: Option<&Value>, path: &str, query: &str, depth: usize, notes: &Notes) -> bool {
    let changed = saved != Some(&*value);
    let mut edited = false;
    // The file's own comment above the setting, as help.
    for line in notes.above() {
        ui.label(RichText::new(line).size(11.5).italics().color(palette.subtle));
    }
    match value {
        Value::Object(map) => {
            let count = map.len();
            let header = egui::CollapsingHeader::new(RichText::new(format!("{key}  ")).size(13.0).strong())
                .id_salt(path)
                .open(if query.is_empty() { None } else { Some(true) })
                .show(ui, |ui| {
                    let saved_map = saved.and_then(Value::as_object);
                    edited |= object_rows(ui, palette, map, saved_map, path, query, depth + 1, notes);
                });
            let amount = if count == 1 { t("1 setting").to_string() } else { tf("{count} settings", &[("count", &count.to_string())]) };
            let label = if changed { tf("{amount} · changed", &[("amount", &amount)]) } else { amount };
            ui.painter().text(
                header.header_response.rect.right_center() + egui::vec2(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(11.0),
                if changed { palette.accent_text } else { palette.subtle },
            );
            return edited;
        }
        Value::Array(items) => {
            let saved_items = saved.and_then(Value::as_array);
            egui::CollapsingHeader::new(RichText::new(tf("{key}  ({count})", &[("key", key), ("count", &items.len().to_string())])).size(13.0).strong())
                .id_salt(path)
                .open(if query.is_empty() { None } else { Some(true) })
                .show(ui, |ui| {
                    let mut remove = None;
                    let len = items.len();
                    for (index, item) in items.iter_mut().enumerate() {
                        let before = saved_items.and_then(|s| s.get(index));
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new(format!("{}", index + 1)).size(11.0).color(palette.subtle));
                            ui.vertical(|ui| {
                                edited |= match item {
                                    Value::Object(_) | Value::Array(_) => setting(ui, palette, &tf("Item {number}", &[("number", &(index + 1).to_string())]), item, before, &format!("{path}{index}."), query, depth + 1, &Notes::none()),
                                    _ => scalar(ui, palette, item, before, &format!("{path}{index}")),
                                };
                            });
                            if ui.add(small("✕")).on_hover_text(t("Take it out of the list")).clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove {
                        items.remove(index);
                        edited = true;
                    }
                    let template = items.last().cloned().map(|v| empty_like(&v)).unwrap_or(Value::String(String::new()));
                    if ui.add(small(t("+ Add"))).on_hover_text(if len == 0 { t("Adds an empty text") } else { t("Adds one more, like the last") }).clicked() {
                        items.push(template);
                        edited = true;
                    }
                });
            return edited;
        }
        _ => {}
    }
    ui.horizontal(|ui| {
        let label_width = (270.0 - depth as f32 * 12.0).max(160.0);
        ui.allocate_ui_with_layout(egui::vec2(label_width, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(label_width);
            let response = ui.add(egui::Label::new(RichText::new(key).size(12.5).color(if changed { palette.accent_text } else { palette.text })).truncate());
            if let Some(help) = hint(key) {
                response.on_hover_text(help);
            }
        });
        edited |= scalar(ui, palette, value, saved, path);
        if let Some(note) = notes.beside() {
            ui.label(RichText::new(note).size(11.5).italics().color(palette.subtle));
        }
        if changed && saved.is_some() && ui.small_button("↺").on_hover_text(t("Back to the saved value")).clicked() {
            *value = saved.cloned().unwrap_or(Value::Null);
            edited = true;
        }
    });
    if let Some(help) = hint(key) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(help).size(11.0).color(palette.subtle));
        });
    }
    edited
}

/// An empty value of the same kind (for a new list item).
fn empty_like(value: &Value) -> Value {
    match value {
        Value::String(_) => Value::String(String::new()),
        Value::Number(_) => Value::from(0),
        Value::Bool(_) => Value::Bool(false),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), empty_like(v))).collect()),
        Value::Array(_) => Value::Array(Vec::new()),
        Value::Null => Value::Null,
    }
}

/// The editor of a text, number, switch or empty value.
fn scalar(ui: &mut egui::Ui, palette: &Palette, value: &mut Value, saved: Option<&Value>, path: &str) -> bool {
    let _ = (saved, path);
    match value {
        Value::Bool(on) => {
            let response = widgets::switch(ui, palette, on);
            ui.label(RichText::new(if *on { t("On") } else { t("Off") }).size(12.0).color(palette.muted));
            response.changed()
        }
        Value::Number(number) => {
            if let Some(mut whole) = number.as_i64() {
                let changed = ui.add_sized([120.0, 24.0], egui::DragValue::new(&mut whole).speed(1)).changed();
                if changed {
                    *value = Value::from(whole);
                }
                changed
            } else {
                let mut real = number.as_f64().unwrap_or(0.0);
                let changed = ui.add_sized([120.0, 24.0], egui::DragValue::new(&mut real).speed(0.01).max_decimals(4)).changed();
                if changed {
                    if let Some(n) = serde_json::Number::from_f64(real) {
                        *value = Value::Number(n);
                    }
                }
                changed
            }
        }
        Value::String(text) => {
            let width = ui.available_width().clamp(200.0, 620.0);
            let looks_like_address = text.contains("://") || text.contains("${") || text.starts_with('/') || text.starts_with('#');
            let edit = if text.chars().count() > 90 || text.contains('\n') {
                egui::TextEdit::multiline(text).desired_rows(2)
            } else {
                egui::TextEdit::singleline(text).vertical_align(egui::Align::Center)
            };
            let edit = if looks_like_address { edit.font(egui::TextStyle::Monospace) } else { edit };
            ui.add_sized([width, 24.0], edit).changed()
        }
        Value::Null => {
            ui.label(RichText::new(t("(empty)")).size(12.0).color(palette.subtle));
            if ui.small_button(t("Make it a text")).clicked() {
                *value = Value::String(String::new());
                return true;
            }
            false
        }
        _ => false,
    }
}

/// The screenshot's sample: the files and ui-config opened.
fn sample(s: &mut State) {
    s.sample = true;
    let file = |name: &str, served: bool| ConfigFile { name: name.into(), size: Some(1000), example: true, served };
    s.files = Some(vec![
        file("renderer-config.json", true),
        file("ui-config.json", true),
        file("client-mode.json", true),
        file("news.json", true),
        file("hotlooks.json", true),
        file("infostand_backgrounds.json", true),
        file("pets.json", false),
        ConfigFile { name: "adsense.json".into(), size: None, example: true, served: false },
        file("radio-stations.jsonc", true),
        file("housekeeping-texts-en.json", true),
        file("rarevalues-texts-en.json", true),
    ]);
    let which = std::env::var("AIO_SCREENSHOT_TAB").unwrap_or_default();
    let (name, text) = if which == "radio" {
        (
            "radio-stations.jsonc",
            r##"// The stations of the hotel radio.
{
    // Shown in the radio window, in this order.
    "stations": [
        "https://radio.example.com/live",
        "https://radio.example.com/chill",
    ],
    "volume": 50, // 0 to 100
    "autoplay": false,
}
"##,
        )
    } else if which == "renderer" {
        (
            "renderer-config.json",
            r##"{
    "socket.url": "wss://hotel.example.com/ws",
    "asset.url": "https://hotel.example.com/gamedata",
    "image.library.url": "https://hotel.example.com/gamedata/c_images/",
    "hof.furni.url": "https://hotel.example.com",
    "furni.asset.url": "${asset.url}/furniture/%libname%.hab",
    "furni.asset.icon.url": "https://hotel.example.com/gamedata/icons/%libname%%param%_icon.png",
    "furnidata.url": "${asset.url}/config/FurnitureData.json",
    "productdata.url": "${asset.url}/config/ProductData.json",
    "system.animation.fps": 24,
    "system.limits.fps": true,
    "system.pong.manually": true,
    "room.color.skip.transition": true,
    "avatar.mandatory.libraries": ["bd:1", "li:0"]
}
"##,
        )
    } else {
        (
            "ui-config.json",
            r##"{
    "// ---- Login screen ----": "",
    "loginview": {
        "images": {
            "background": "${asset.url}/c_images/reception/stretch_blue.png",
            "background.colour": "#6eadc8"
        }
    },
    "login.remember.cookie": false,
    "login.remember.rotate.interval.minutes": 15,
    "// ---- Hotel view ----": "",
    "hotelview": {
        "room.pool": "791",
        "show.avatar": true
    },
    "// ---- Toolbar ----": "",
    "toolbar.hide.quests": false,
    "game.center.enabled": true,
    "guides.enabled": true,
    "catalog.links": ["hc_membership", "loyalty_info"],
    "chat.input.maxlength": 100,
    "motto.max.length": 38,
    "camera.available.effects": ["dark_sepia", "increase_saturation"]
}
"##,
        )
    };
    let example = format!("{}  \"new.setting\": true\n}}\n", text.trim_end().trim_end_matches('}').trim_end().to_string() + ",\n");
    let loaded = Loaded {
        name: name.into(),
        text: text.into(),
        layout: aio_remote::gamedata_admin::detect_layout(text),
        sha256: "x".into(),
        example: Some(example),
    };
    let mut doc = Doc::new(loaded);
    if let Some(Value::Object(map)) = doc.value.as_mut().filter(|_| name == "ui-config.json") {
        map.insert("chat.input.maxlength".into(), Value::from(120));
    }
    doc.check();
    s.selected = Some(name.into());
    s.doc = Some(doc);
    s.status = Status { text: tf("{file} loaded.", &[("file", name)]), error: false };
}
