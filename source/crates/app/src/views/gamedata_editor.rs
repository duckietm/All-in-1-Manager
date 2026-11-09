//! Gamedata: FurnitureData.json and ExternalTexts.json in the server's gamedata/config, edited here and written back
//! over SSH (checked against the loaded copy, a .bak-<time> kept, then one rename). Whether a furni is in items_base
//! is read with the root login's MariaDB socket, so no MariaDB password is needed.
//!
//! The emulator watches its furnidata and sends changed furni to everyone online by itself; texts are read by the
//! client when it loads.

use std::sync::Arc;

use aio_remote::gamedata_admin::{self as admin, Field, GamedataFile, Hit, ItemRow, Kind, Loaded};
use aio_remote::Server;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use serde_json::Value;

use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{t, tf};

const FIELD_HEIGHT: f32 = 26.0;
const LABEL_WIDTH: f32 = 150.0;
const RESULTS: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Furni,
    Texts,
}

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

/// One file: as loaded (None: not loaded, or the server has none) and the copy being edited.
#[derive(Default)]
struct Doc {
    loaded: Option<Loaded>,
    json: Value,
    missing: bool,
    dirty: bool,
}

impl Doc {
    fn take(&mut self, loaded: Option<Loaded>) {
        self.missing = loaded.is_none();
        self.json = loaded.as_ref().map(|l| l.json.clone()).unwrap_or(Value::Null);
        self.loaded = loaded;
        self.dirty = false;
    }
}

#[derive(Default)]
struct State {
    tab: Tab,
    furni: Doc,
    texts: Doc,
    // furni
    query: String,
    hits: Vec<Hit>,
    selected: Option<(Kind, usize)>,
    fields: Vec<Field>,
    fields_dirty: bool,
    items: Option<Result<Vec<ItemRow>, String>>,
    copy_id: i64,
    copy_classname: String,
    // texts
    text_query: String,
    text_hits: Vec<(String, String)>,
    edit_key: Option<String>,
    edit_value: String,
    new_key: String,
    new_value: String,
    generation: u64,
    loaded_for: Option<usize>,
    busy: bool,
    status: Status,
}

impl State {
    fn reset(&mut self) {
        let generation = self.generation + 1;
        *self = State { generation, ..State::default() };
    }

    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }

    fn search_furni(&mut self) {
        self.hits = admin::search(&self.furni.json, &self.query, RESULTS);
    }

    fn search_texts(&mut self) {
        self.text_hits = admin::search_texts(&self.texts.json, &self.text_query, RESULTS);
    }

    fn select(&mut self, kind: Kind, index: usize) {
        self.selected = Some((kind, index));
        self.fields = admin::entry(&self.furni.json, kind, index).map(admin::plain_fields).unwrap_or_default();
        self.fields_dirty = false;
        self.items = None;
        self.copy_id = admin::next_free_id(&self.furni.json);
        self.copy_classname = self.field("classname").map(|c| format!("{c}_copy")).unwrap_or_default();
    }

    fn field(&self, name: &str) -> Option<String> {
        self.fields.iter().find(|f| f.name == name).map(|f| f.text.clone())
    }
}

pub struct GamedataEditor {
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
        if state.busy {
            return None;
        }
        if server.is_dry_run() {
            state.error(t("Dry run: there are no files to edit.").into());
            return None;
        }
        state.busy = true;
        Some((state.generation, server))
    }

    async fn load(self, file: GamedataFile) {
        let Some((generation, server)) = self.start() else { return };
        let loaded = admin::load(&*server, file).await;
        self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(loaded) => {
                    let text = match &loaded {
                        Some(l) => tf("{file} loaded ({size} KB).", &[("file", file.file_name()), ("size", &(l.bytes / 1024).to_string())]),
                        None => tf("The server has no {file} yet.", &[("file", file.file_name())]),
                    };
                    match file {
                        GamedataFile::Furnidata => {
                            s.furni.take(loaded);
                            s.selected = None;
                            s.fields.clear();
                            s.search_furni();
                        }
                        GamedataFile::Texts => {
                            s.texts.take(loaded);
                            s.edit_key = None;
                            s.search_texts();
                        }
                    }
                    s.ok(text);
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    async fn check_items(self) {
        let (kind, id, classname) = {
            let s = self.lock();
            let Some((kind, _)) = s.selected else { return };
            (kind, s.field("id").and_then(|i| i.trim().parse::<i64>().ok()).unwrap_or(0), s.field("classname").unwrap_or_default())
        };
        let Some((generation, server)) = self.start() else { return };
        let rows = admin::items_for(&*server, id, &classname).await;
        self.with(generation, |s| {
            s.busy = false;
            s.items = Some(rows.map_err(|e| e.to_string()));
            if let Some(Ok(rows)) = &s.items {
                match admin::items_problem(kind, id, &classname, rows) {
                    None => s.ok(tf("{classname} is in items_base as it should be.", &[("classname", &classname)])),
                    Some(problem) => s.error(problem),
                }
            }
        });
    }

    async fn save(self, file: GamedataFile) {
        let (loaded, json) = {
            let s = self.lock();
            let doc = if file == GamedataFile::Furnidata { &s.furni } else { &s.texts };
            let Some(loaded) = doc.loaded.clone() else { return };
            (loaded, doc.json.clone())
        };
        let message = tf("{file} on the server is replaced. The file as it is now is kept next to it as {file}.bak-<time>.", &[("file", file.file_name())]);
        let options = vec![Choice::new(t("Save"), t("Write the file on the server."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Save {file}?", &[("file", file.file_name())]), &message, false, options).await {
            return;
        }
        let Some((generation, server)) = self.start() else { return };
        let saved = admin::save(&*server, &loaded, &json, &|_| {}).await;
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(saved) => {
                    let doc = if file == GamedataFile::Furnidata { &mut s.furni } else { &mut s.texts };
                    // Edits made while it was saving stay unsaved.
                    doc.dirty = doc.json != json;
                    if let Some(l) = doc.loaded.as_mut() {
                        l.json = json;
                        l.sha256 = saved.sha256;
                    }
                    let next = match file {
                        GamedataFile::Furnidata => t("The emulator sees the change by itself and sends the changed furni to everyone online (items.furnidata.watch.enabled); no command is needed."),
                        GamedataFile::Texts => t("Players see the new texts after they reload the hotel page."),
                    };
                    s.ok(format!("{} {next}", tf("{file} saved; the old one is {backup}.", &[("file", file.file_name()), ("backup", &saved.backup)])));
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    async fn delete_text(self, key: String) {
        let message = tf("The text {key} is deleted from the edited copy. Nothing changes on the server until you save.", &[("key", &key)]);
        let options = vec![Choice::new(t("Delete"), t("Delete the text."), true).primary(), Choice::new(t("Cancel"), t("Do nothing."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {key}?", &[("key", &key)]), &message, false, options).await {
            return;
        }
        let mut s = self.lock();
        if admin::remove_text(&mut s.texts.json, &key) {
            s.texts.dirty = true;
            if s.edit_key.as_deref() == Some(&key) {
                s.edit_key = None;
            }
            s.search_texts();
            s.ok(tf("{key} deleted. Save ExternalTexts.json to write it.", &[("key", &key)]));
        }
        drop(s);
        self.shared.ctx.request_repaint();
    }
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl GamedataEditor {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())) }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

    fn load_if_needed(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        let id = server_id(&server);
        {
            let mut state = self.state.lock();
            if state.loaded_for == Some(id) {
                return;
            }
            state.reset();
            state.loaded_for = Some(id);
        }
        let ctl = self.ctl(shared);
        ctl.spawn(|c| async move {
            c.clone().load(GamedataFile::Furnidata).await;
            c.load(GamedataFile::Texts).await;
        });
    }
}

enum Action {
    Load(GamedataFile),
    Save(GamedataFile),
    CheckItems,
    DeleteText(String),
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

impl View for GamedataEditor {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.load_if_needed(shared);
        let mut actions = Vec::new();
        {
            let mut guard = self.state.lock();
            let s = &mut *guard;
            egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 18 }).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                header(ui, palette, s, shared);
                ui.add_space(8.0);
                widgets::card_frame(palette).inner_margin(10).corner_radius(10).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    body(ui, palette, s, &mut actions);
                });
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Load(file) => ctl.spawn(move |c| c.load(file)),
                Action::Save(file) => ctl.spawn(move |c| c.save(file)),
                Action::CheckItems => ctl.spawn(|c| c.check_items()),
                Action::DeleteText(key) => ctl.spawn(move |c| c.delete_text(key)),
            }
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().busy
    }

    fn session_changed(&mut self, _shared: &Shared) {
        let mut state = self.state.lock();
        state.reset();
    }
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Gamedata")).size(13.0).strong());
                let path = GamedataFile::Furnidata.path();
                let folder = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(&path);
                widgets::muted(ui, palette, &tf("FurnitureData.json and ExternalTexts.json in {folder}, saved with a backup next to them.", &[("folder", folder)]), 12.0);
            });
        });
    });
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
    if s.furni.dirty || s.texts.dirty || s.fields_dirty {
        ui.label(RichText::new(t("There are unsaved changes.")).size(12.0).color(palette.warning_text));
    }
}

fn file_bar(ui: &mut egui::Ui, palette: &Palette, doc: &Doc, file: GamedataFile, busy: bool, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!busy, small(t("Load again"))).on_hover_text(t("Reads the file from the server; unsaved changes are dropped.")).clicked() {
            actions.push(Action::Load(file));
        }
        let save = tf("Save {file}", &[("file", file.file_name())]);
        if ui.add_enabled(!busy && doc.dirty && doc.loaded.is_some(), widgets::primary_button(&save)).clicked() {
            actions.push(Action::Save(file));
        }
        if doc.missing {
            widgets::muted(ui, palette, &tf("The server has no {file}: put one there first (Get Assets, Furni import or Client texts).", &[("file", file.file_name())]), 12.0);
        }
    });
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::Furni, t("Furnidata")), (Tab::Texts, t("Texts"))] {
            ui.selectable_value(&mut s.tab, tab, RichText::new(label).size(13.0));
        }
    });
    ui.separator();
    match s.tab {
        Tab::Furni => furni_tab(ui, palette, s, actions),
        Tab::Texts => texts_tab(ui, palette, s, actions),
    }
}

fn furni_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    file_bar(ui, palette, &s.furni, GamedataFile::Furnidata, s.busy, actions);
    if s.furni.loaded.is_none() {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("Search")).size(12.5));
        if ui.add_sized([320.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.query).hint_text(t("Id, classname or name"))).changed() {
            s.search_furni();
        }
        if !s.query.trim().is_empty() {
            widgets::muted(ui, palette, &tf("{count} found", &[("count", &s.hits.len().to_string())]), 12.0);
        }
    });
    ui.add_space(4.0);
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(300.0);
            egui::ScrollArea::vertical().id_salt("gamedata-hits").max_height(height).auto_shrink([false, false]).show(ui, |ui| {
                let hits = s.hits.clone();
                for hit in hits {
                    let kind = if hit.kind == Kind::Floor { t("floor") } else { t("wall") };
                    let label = format!("{} · {} ({kind})\n{}", hit.id, hit.classname, hit.name);
                    let selected = s.selected == Some((hit.kind, hit.index));
                    if ui.add_sized([290.0, 36.0], egui::Button::new(RichText::new(label).size(12.0)).selected(selected)).clicked() && !selected {
                        if s.fields_dirty {
                            s.error(t("Apply or undo the changes of this furni first.").into());
                        } else {
                            s.select(hit.kind, hit.index);
                        }
                    }
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            egui::ScrollArea::vertical().id_salt("gamedata-entry").max_height(height).auto_shrink([false, false]).show(ui, |ui| entry_form(ui, palette, s, actions));
        });
    });
}

fn entry_form(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let Some((kind, index)) = s.selected else {
        widgets::muted(ui, palette, t("Search for a furni and pick it to edit it."), 12.5);
        return;
    };
    let ready = !s.busy;
    let original_classname = admin::entry(&s.furni.json, kind, index).and_then(|e| e.get("classname")).and_then(Value::as_str).unwrap_or("").to_string();
    egui::Grid::new("gamedata-fields").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        for field in s.fields.iter_mut() {
            ui.add_sized([LABEL_WIDTH, FIELD_HEIGHT], egui::Label::new(RichText::new(&field.name).size(12.5)));
            let width = ui.available_width().clamp(160.0, 460.0);
            if ui.add_sized([width, FIELD_HEIGHT], egui::TextEdit::singleline(&mut field.text)).changed() {
                s.fields_dirty = true;
            }
            ui.end_row();
        }
    });
    widgets::muted(ui, palette, t("Lists and objects (such as partcolors) are kept as they are."), 12.0);
    if s.field("classname").is_some_and(|c| c != original_classname) {
        ui.label(RichText::new(t("A new classname needs a furni bundle and an items_base row with that name.")).size(12.0).color(palette.warning_text));
    }
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(ready && s.fields_dirty, small(t("Apply"))).on_hover_text(t("Puts the changes in the edited copy; Save writes the file.")).clicked() {
            match admin::apply_fields(&mut s.furni.json, kind, index, &s.fields) {
                Ok(()) => {
                    s.furni.dirty = true;
                    s.select(kind, index);
                    s.search_furni();
                    s.ok(t("Applied. Save FurnitureData.json to write it.").into());
                }
                Err(problem) => s.error(problem),
            }
        }
        if ui.add_enabled(s.fields_dirty, small(t("Undo"))).clicked() {
            s.select(kind, index);
        }
        if ui.add_enabled(ready, small(t("Check items_base"))).on_hover_text(t("Looks up this classname and sprite id in the hotel's items_base.")).clicked() {
            actions.push(Action::CheckItems);
        }
    });
    if let Some(items) = &s.items {
        match items {
            Ok(rows) if rows.is_empty() => widgets::muted(ui, palette, t("No items_base row has this classname or sprite id."), 12.0),
            Ok(rows) => {
                for row in rows {
                    widgets::muted(ui, palette, &tf("items_base {id}: {name}, sprite id {sprite}, type {type}", &[("id", &row.id.to_string()), ("name", &row.item_name), ("sprite", &row.sprite_id.to_string()), ("type", &row.kind)]), 12.0);
                }
            }
            Err(error) => {
                ui.label(RichText::new(error).size(12.0).color(palette.error_text));
            }
        }
    }
    ui.add_space(10.0);
    ui.label(RichText::new(t("New furni as a copy of this one")).size(12.5).strong());
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t("Id")).size(12.5));
        ui.add_sized([100.0, FIELD_HEIGHT], egui::DragValue::new(&mut s.copy_id).range(1..=i64::from(i32::MAX)));
        ui.label(RichText::new(t("Classname")).size(12.5));
        ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.copy_classname));
        if ui.add_enabled(ready && !s.fields_dirty, small(t("Add copy"))).on_hover_text(t("The copy gets offer id -1: Fix offer IDs gives it one.")).clicked() {
            let classname = s.copy_classname.trim().to_string();
            match admin::add_copy(&mut s.furni.json, kind, index, s.copy_id, &classname) {
                Ok(new_index) => {
                    s.furni.dirty = true;
                    s.query = classname.clone();
                    s.search_furni();
                    s.select(kind, new_index);
                    s.ok(tf("{classname} added. Save FurnitureData.json to write it.", &[("classname", &classname)]));
                }
                Err(problem) => s.error(problem),
            }
        }
    });
}

fn texts_tab(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    file_bar(ui, palette, &s.texts, GamedataFile::Texts, s.busy, actions);
    widgets::muted(ui, palette, t("Octane's UITexts.jsonc is not edited here: it has comments, which saving as JSON would drop."), 12.0);
    if s.texts.loaded.is_none() {
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("Search")).size(12.5));
        if ui.add_sized([320.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.text_query).hint_text(t("Key or text"))).changed() {
            s.search_texts();
        }
        if !s.text_query.trim().is_empty() {
            widgets::muted(ui, palette, &tf("{count} found", &[("count", &s.text_hits.len().to_string())]), 12.0);
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t("New key")).size(12.5));
        ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.new_key));
        ui.label(RichText::new(t("Text")).size(12.5));
        ui.add_sized([300.0, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.new_value));
        if ui.add_enabled(!s.busy && !s.new_key.trim().is_empty(), small(t("Add"))).clicked() {
            let key = s.new_key.clone();
            if s.texts.json.get(&key).is_some() {
                s.error(tf("{key} is already there: search for it to change it.", &[("key", &key)]));
            } else {
                match admin::set_text(&mut s.texts.json, &key, &s.new_value) {
                    Ok(_) => {
                        s.texts.dirty = true;
                        s.text_query = key.clone();
                        s.search_texts();
                        s.new_key.clear();
                        s.new_value.clear();
                        s.ok(tf("{key} added. Save ExternalTexts.json to write it.", &[("key", &key)]));
                    }
                    Err(problem) => s.error(problem),
                }
            }
        }
    });
    ui.separator();
    egui::ScrollArea::vertical().id_salt("gamedata-texts").auto_shrink([false, false]).show(ui, |ui| {
        let hits = s.text_hits.clone();
        for (key, value) in hits {
            let editing = s.edit_key.as_deref() == Some(key.as_str());
            ui.horizontal(|ui| {
                ui.add_sized([280.0, FIELD_HEIGHT], egui::Label::new(RichText::new(&key).size(12.0).monospace()).truncate());
                if editing {
                    let width = (ui.available_width() - 150.0).max(200.0);
                    ui.add_sized([width, FIELD_HEIGHT], egui::TextEdit::singleline(&mut s.edit_value));
                    if ui.add_enabled(!s.busy, small(t("Apply"))).clicked() {
                        let edited = s.edit_value.clone();
                        if admin::set_text(&mut s.texts.json, &key, &edited).is_ok() {
                            s.texts.dirty = true;
                            s.edit_key = None;
                            s.search_texts();
                            s.ok(tf("{key} changed. Save ExternalTexts.json to write it.", &[("key", &key)]));
                        }
                    }
                    if ui.add(small(t("Cancel"))).clicked() {
                        s.edit_key = None;
                    }
                } else {
                    let width = (ui.available_width() - 150.0).max(200.0);
                    ui.add_sized([width, FIELD_HEIGHT], egui::Label::new(RichText::new(&value).size(12.0)).truncate());
                    if ui.add_enabled(!s.busy, small(t("Edit"))).clicked() {
                        s.edit_key = Some(key.clone());
                        s.edit_value = value.clone();
                    }
                    if ui.add_enabled(!s.busy, small(t("Delete"))).clicked() {
                        actions.push(Action::DeleteText(key.clone()));
                    }
                }
            });
        }
        if s.text_query.trim().is_empty() {
            widgets::muted(ui, palette, &tf("{count} texts. Search for a key or a text.", &[("count", &s.texts.json.as_object().map_or(0, |m| m.len()).to_string())]), 12.0);
        }
    });
}
