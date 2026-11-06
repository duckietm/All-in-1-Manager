//! Fix items_base: sprite_id, size, sit / lay / walk and interaction modes of the furni already in
//! items_base, as Generate SQL works them out (see aio_remote::item_fix). Multiheight furni are left alone. The page
//! shows every change first.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aio_remote::furni_import::{self, Bundle};
use aio_remote::habbo_assets::{self, retro};
use aio_remote::item_fix::{self, FixPlan, Fields, ItemRow};
use aio_remote::{fail, server_tools, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use serde_json::Value;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// Where the .hab files are on this computer: Habbo's download first, then every Nitro retro profile's folder.
fn bundle_folders() -> Vec<PathBuf> {
    let settings = aio_core::InstallerSettings::load();
    let assets = settings.assets_folder();
    std::iter::once(habbo_assets::gamedata_folder(&assets).join("furniture"))
        .chain(settings.retro_profiles().iter().map(|p| habbo_assets::gamedata_folder(&retro::custom_folder(&assets, p)).join("furniture")))
        .collect()
}

/// Which FurnitureData.json the furni are matched to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum Source {
    /// The one on the server (/var/www/gamedata/config): what the hotel uses now, also when it is a merged one.
    #[default]
    Server,
    /// Habbo's original, from Get Assets' Habbo download.
    Habbo,
    /// Habbo's plus the custom furni of the Nitro retro profiles, as Import Assets merged it (custom/FurnitureData.json).
    Custom,
}

impl Source {
    const ALL: [Source; 3] = [Source::Server, Source::Habbo, Source::Custom];

    fn label(self) -> &'static str {
        match self {
            Source::Server => t("The server's FurnitureData.json"),
            Source::Habbo => t("Habbo's original FurnitureData.json (Get Assets)"),
            Source::Custom => t("Habbo's with the custom furni (Import Assets' merged one)"),
        }
    }

    /// The file on this computer; None for the server's.
    fn local_file(self) -> Option<PathBuf> {
        let assets = aio_core::InstallerSettings::load().assets_folder();
        match self {
            Source::Server => None,
            Source::Habbo => Some(habbo_assets::gamedata_folder(&assets).join("config").join("FurnitureData.json")),
            Source::Custom => Some(assets.join(retro::CUSTOM).join("FurnitureData.json")),
        }
    }
}

fn read_local_json(path: &std::path::Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: Value = serde_json::from_slice(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).map_err(|e| format!("{}: {e}", path.display()))?;
    if !value["roomitemtypes"]["furnitype"].is_array() && !value["wallitemtypes"]["furnitype"].is_array() {
        return Err(format!("{}: {}", path.display(), t("no roomitemtypes or wallitemtypes in it")));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum List {
    #[default]
    Changes,
    NotInFurnidata,
    NoBundle,
    MultiheightNotByHab,
    MultiheightCount,
}

/// What the check read, kept to work out the plan again when the choices change.
struct Read {
    rows: Vec<ItemRow>,
    /// The server's FurnitureData.json (the local ones are read when chosen).
    server_furnidata: Result<Arc<Value>, String>,
    bundles: HashMap<String, Option<Bundle>>,
}

struct Form {
    read: Option<Arc<Read>>,
    fields: Fields,
    source: Source,
    /// The hotel database read, and every database on the server with an items_base table (to choose from).
    database: String,
    databases: Vec<String>,
    /// To read another database from the page: the server and the runtime the check ran on.
    server: Option<Arc<dyn Server>>,
    runtime: Option<tokio::runtime::Handle>,
    /// Reading another database now, or why that failed.
    reloading: bool,
    reload_error: Option<String>,
    /// A database backup before the update (on by default).
    backup: bool,
    /// The local FurnitureData.json files read so far (or why they could not be).
    local: HashMap<Source, Result<Arc<Value>, String>>,
    /// The plan, and the choices it was made for.
    plan: Option<(Fields, Source, Arc<FixPlan>)>,
    restart: bool,
    list: List,
    filter: String,
    /// The rows of the list shown, made when it changes (not on every frame).
    rows: Option<((Fields, Source, List, String), Arc<Vec<Vec<String>>>)>,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            read: None,
            fields: Fields::default(),
            source: Source::default(),
            database: String::new(),
            databases: Vec::new(),
            server: None,
            runtime: None,
            reloading: false,
            reload_error: None,
            backup: true,
            local: HashMap::new(),
            plan: None,
            restart: true,
            list: List::Changes,
            filter: String::new(),
            rows: None,
        }
    }
}

impl Form {
    /// The plan for the current choices; made again only when they change.
    fn plan(&mut self) -> Option<Arc<FixPlan>> {
        let read = self.read.clone()?;
        if let Some((fields, source, plan)) = &self.plan {
            if *fields == self.fields && *source == self.source {
                return Some(plan.clone());
            }
        }
        let furnidata = self.furnidata().ok()?;
        let plan = Arc::new(item_fix::plan(&read.rows, &furnidata, &read.bundles, self.fields));
        self.plan = Some((self.fields, self.source, plan.clone()));
        Some(plan)
    }

    /// The chosen FurnitureData.json: the server's as the check read it, a local one read once.
    fn furnidata(&mut self) -> Result<Arc<Value>, String> {
        let Some(path) = self.source.local_file() else {
            return self.read.as_ref().map(|r| r.server_furnidata.clone()).unwrap_or_else(|| Err(t("Nothing read yet.").to_string()));
        };
        self.local.entry(self.source).or_insert_with(|| read_local_json(&path).map(Arc::new)).clone()
    }

    fn rows(&mut self) -> Arc<Vec<Vec<String>>> {
        let key = (self.fields, self.source, self.list, self.filter.trim().to_lowercase());
        if let Some((have, rows)) = &self.rows {
            if *have == key {
                return rows.clone();
            }
        }
        let Some(plan) = self.plan() else { return Arc::new(Vec::new()) };
        let matches = |name: &str| key.3.is_empty() || name.to_lowercase().contains(&key.3);
        let rows: Vec<Vec<String>> = match self.list {
            List::Changes => plan.changes.iter().filter(|c| matches(&c.name)).map(|c| vec![c.id.to_string(), c.name.clone(), c.column.to_string(), c.from.clone(), c.to.clone()]).collect(),
            List::NotInFurnidata => plan.not_in_furnidata.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect(),
            List::NoBundle => plan.no_bundle.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect(),
            List::MultiheightNotByHab => plan.multiheight_not_by_hab.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect(),
            List::MultiheightCount => plan.multiheight_count_differs.iter().filter(|m| matches(&m.0)).map(|(name, db, hab)| vec![name.clone(), db.clone(), hab.clone()]).collect(),
        };
        let rows = Arc::new(rows);
        self.rows = Some((key, rows.clone()));
        rows
    }
}

#[derive(Default, Clone)]
struct Output {
    rows: usize,
    changes: usize,
    safety_backup: Option<String>,
}

pub struct ItemFixStep {
    form: Arc<Mutex<Form>>,
    output: Mutex<Output>,
}

impl ItemFixStep {
    pub fn new() -> Self {
        Self { form: Arc::new(Mutex::new(Form::default())), output: Mutex::new(Output::default()) }
    }
}

impl Default for ItemFixStep {
    fn default() -> Self {
        Self::new()
    }
}

fn cell(ui: &mut egui::Ui, text: &str, width: f32, color: egui::Color32, strong: bool) {
    let mut text = RichText::new(text).size(12.5).color(color);
    if strong {
        text = text.strong();
    }
    ui.allocate_ui_with_layout(egui::vec2(width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(width);
        ui.add(egui::Label::new(text).truncate());
    });
}

fn table(ui: &mut egui::Ui, palette: &Palette, id: &str, widths: &[f32], header: &[&str], rows: &[Vec<String>], name_column: usize) {
    ui.horizontal(|ui| {
        for (title, width) in header.iter().zip(widths) {
            cell(ui, title, *width, palette.muted, true);
        }
    });
    egui::ScrollArea::vertical().id_salt(id).max_height(300.0).auto_shrink([false, true]).show_rows(ui, 18.0, rows.len(), |ui, range| {
        for row in &rows[range] {
            ui.horizontal(|ui| {
                for (i, (text, width)) in row.iter().zip(widths).enumerate() {
                    cell(ui, text, *width, if i == name_column { palette.text } else { palette.muted }, false);
                }
            });
        }
    });
}

#[async_trait]
impl InstallStep for ItemFixStep {
    fn title(&self) -> String {
        t("Fix items_base").into()
    }

    fn caption(&self) -> String {
        t("sprite_id, size and settings").into()
    }

    fn install_label(&self) -> String {
        t("Fix items_base").into()
    }

    /// The button runs the fix (there is no separate check button).
    fn rerun_label(&self) -> String {
        t("Fix items_base").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Wrench,
            heading: t("Bring items_base in line with FurnitureData.json and the .hab files").into(),
            description: tf(
                "Matches every furni of items_base to {file} by item_name = classname and sets what Generate SQL would set for it, like the All-in-1 Converter's Fix sprite_id, Fix width / length / height, Fix item settings and Fix interaction count.",
                &[("file", &aio_remote::offer_fix::furniture_data_path())],
            ),
            bullets: vec![
                t("Only floor and wall furni; pets, effects and bots are left alone, and every multiheight furni keeps its heights, set by hand per state, unless it has more heights than its .hab has states. A furni whose .hab is multiheight can become one.").into(),
                t("sprite_id from the FurnitureData.json you choose below (the server's, Habbo's original or the merged one with the custom furni); sit / lay / walk, and on request the type, from it too.").into(),
                t("Size and stack height from the .hab files you downloaded (Habbo assets, then the Nitro retro profiles); a wall furni is 1 x 1 x 0.00. Furni without a .hab here keep their size.").into(),
                t("Interaction modes only for interaction_type default: other interactions count their own states.").into(),
                t("Exceptions: a few furni whose values no file can give (a pommel horse you sit on at height 2) are set exactly as the installer's list of exceptions has them, also when they are multiheight.").into(),
                tf("A backup of the database comes first ({folder}); the update runs as one transaction.", &[("folder", &server_tools::backup_directory())]),
            ],
            footnote: Some(t("Choose below what to fix; the list shows every value that changes.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the database").into(), t("Backup and update").into(), t("Restart the emulator").into()]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        if form.read.is_none() {
            return;
        }
        ui.add_space(8.0);
        ui.label(RichText::new(t("Hotel database")).size(14.0).strong());
        let mut picked = form.database.clone();
        ui.add_enabled_ui(!form.reloading, |ui| {
            egui::ComboBox::from_id_salt("item_fix_database").selected_text(&picked).width(260.0).show_ui(ui, |ui| {
                for database in &form.databases {
                    ui.selectable_value(&mut picked, database.clone(), database);
                }
            });
        });
        if form.reloading {
            widgets::muted(ui, palette, &tf("Reading {database} ...", &[("database", &picked)]), 11.5);
        } else if let Some(error) = &form.reload_error {
            widgets::muted(ui, palette, &tf("Cannot be used: {error}", &[("error", error)]), 12.0);
        } else {
            widgets::muted(ui, palette, t("Every database on the server with an items_base table. The choice is used by every tool and saved in Settings (Server layout, Hotel database)."), 11.5);
        }
        if picked != form.database && !form.reloading {
            // Started while the page holds the lock: the task takes it only when it is done reading.
            reload(form, self.form.clone(), picked, ui.ctx().clone());
        }
        ui.add_space(8.0);
        ui.label(RichText::new(t("FurnitureData.json to match")).size(14.0).strong());
        for source in Source::ALL {
            ui.radio_value(&mut form.source, source, source.label());
        }
        match form.source.local_file() {
            None => match form.furnidata() {
                Ok(_) => widgets::muted(ui, palette, &tf("{file}: what the hotel uses now, also when it is a merged or custom one.", &[("file", &aio_remote::offer_fix::furniture_data_path())]), 11.5),
                Err(error) => widgets::muted(ui, palette, &tf("Cannot be used: {error}", &[("error", &error)]), 12.0),
            },
            Some(path) => {
                if let Err(error) = form.furnidata() {
                    widgets::muted(ui, palette, &tf("Cannot be used: {error}", &[("error", &error)]), 12.0);
                    if form.source == Source::Habbo {
                        widgets::muted(ui, palette, t("Download it first: ASSETS → Habbo assets, FurnitureData.json."), 11.5);
                    } else {
                        widgets::muted(ui, palette, t("Make it first: ASSETS → Import custom furniture."), 11.5);
                    }
                } else {
                    widgets::muted(ui, palette, &tf("{file}. Furni that are not in it are left alone; sprite_id and sit / lay / walk come from it, not from the server's.", &[("file", &path.display().to_string())]), 11.5);
                }
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new(t("What to fix")).size(14.0).strong());
        let fields = &mut form.fields;
        ui.checkbox(&mut fields.sprite, t("sprite_id, from FurnitureData.json"));
        ui.checkbox(&mut fields.kind, t("type (floor s / wall i), from FurnitureData.json"));
        widgets::muted(ui, palette, t("Off (default) for type: furni players already placed keep their floor or wall position, so a furni that changes type can end up broken in their rooms."), 11.5);
        ui.checkbox(&mut fields.size, t("width, length and stack_height, from the .hab files"));
        widgets::muted(ui, palette, t("A multiheight furni gets its width and length, and the height of its first state as stack_height (0.8 for 0.8;2.0;3.0;4.0): the state it is placed in."), 11.5);
        ui.checkbox(&mut fields.flags, t("allow_sit, allow_lay and allow_walk, from FurnitureData.json"));
        ui.checkbox(&mut fields.modes, t("interaction_modes_count, from the .hab files (interaction_type default only)"));
        ui.checkbox(&mut fields.multiheight, t("multiheight, as the .hab files say (logicType furniture_multiheight)"));
        widgets::muted(ui, palette, t("Set where the .hab is a multiheight furni and items_base is not: interaction_type multiheight, a height per state from its images (rounded to a quarter, at least 0.10) and the first state's height as stack_height; recolorable keeps its interaction. A furni that is multiheight in items_base keeps its heights (only too many are cut off, below): many really are while their .hab says otherwise (stairs, stages); they are listed to check."), 11.5);
        widgets::muted(ui, palette, t("A recolourable furni (interaction recolorable, name ending in _recolorable) gets the heights, number of states and first height of its multiheight twin (bc_cylinder_recolorable from bc_cylinder), as the emulator's own migration does: they share their states."), 11.5);
        ui.checkbox(&mut fields.extra_heights, t("multiheight with more heights than its .hab has states: keep the first ones"));
        widgets::muted(ui, palette, t("The emulator goes through every height, so a height without a state shows no graphic (5 heights for 4 states: the 5th click shows nothing). The first heights stay, one per state, and interaction_modes_count follows. Fewer heights than states are only listed: the missing heights are not in the .hab."), 11.5);
        widgets::muted(ui, palette, t("allow_stack is never changed: no file says whether a furni can be stacked on (the .hab only has its height), so the value you set stays."), 11.5);
        let Some(plan) = form.plan() else { return };
        let rows = form.rows();
        ui.add_space(8.0);
        ui.label(RichText::new(t("What changes")).size(14.0).strong());
        widgets::muted(
            ui,
            palette,
            &tf(
                "{checked} furni checked, {multiheight} multiheight furni left alone, {exceptions} with an exception: {rows} furni get {changes} new values.",
                &[
                    ("exceptions", &plan.exceptions.to_string()),
                    ("checked", &plan.checked.to_string()),
                    ("multiheight", &plan.multiheight.to_string()),
                    ("rows", &plan.rows().to_string()),
                    ("changes", &plan.changes.len().to_string()),
                ],
            ),
            12.5,
        );
        ui.horizontal_wrapped(|ui| {
            for (list, label) in [
                (List::Changes, tf("Changes ({count})", &[("count", &plan.changes.len().to_string())])),
                (List::NotInFurnidata, tf("Not in FurnitureData.json ({count})", &[("count", &plan.not_in_furnidata.len().to_string())])),
                (List::NoBundle, tf("No .hab file ({count})", &[("count", &plan.no_bundle.len().to_string())])),
            ]
            .into_iter()
            .chain((!plan.multiheight_not_by_hab.is_empty()).then(|| (List::MultiheightNotByHab, tf("Multiheight, not by the .hab ({count})", &[("count", &plan.multiheight_not_by_hab.len().to_string())]))))
            .chain((!plan.multiheight_count_differs.is_empty()).then(|| (List::MultiheightCount, tf("Number of heights differs ({count})", &[("count", &plan.multiheight_count_differs.len().to_string())]))))
            {
                ui.selectable_value(&mut form.list, list, RichText::new(label).size(12.5));
            }
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut form.filter).hint_text(RichText::new(t("Search ...")).italics()).desired_width(180.0));
        });
        match form.list {
            List::Changes => table(ui, palette, "item_changes", &[90.0, 250.0, 170.0, 90.0, 90.0], &["id", "item_name", t("Column"), t("Now"), t("New")], &rows, 1),
            List::NotInFurnidata => {
                widgets::muted(ui, palette, t("In items_base, but the server's FurnitureData.json has no classname for them: left as they are (the client cannot show them either)."), 12.0);
                table(ui, palette, "item_not_in_fd", &[400.0], &["item_name"], &rows, 0);
            }
            List::MultiheightNotByHab => {
                widgets::muted(ui, palette, t("Multiheight in items_base, but their .hab is not a furniture_multiheight furni (stairs and stages are often furniture_multistate): left as they are. Only check these if one does not behave in the hotel."), 12.0);
                table(ui, palette, "item_mh_not_by_hab", &[400.0], &["item_name"], &rows, 0);
            }
            List::MultiheightCount => {
                widgets::muted(ui, palette, t("Multiheight in both, but items_base has another number of heights than the .hab has states: your heights stay (too many are cut off with the choice above). The .hab's are shown to compare."), 12.0);
                table(ui, palette, "item_mh_count", &[260.0, 220.0, 220.0], &["item_name", "items_base.multiheight", ".hab"], &rows, 0);
            }
            List::NoBundle => {
                widgets::muted(ui, palette, t("Their .hab file is not in your downloads (Habbo assets, Nitro retro): their size and modes stay as they are; sprite_id and sit / lay / walk are still fixed."), 12.0);
                table(ui, palette, "item_no_bundle", &[400.0], &["item_name"], &rows, 0);
            }
        }
        ui.add_space(8.0);
        ui.checkbox(&mut form.backup, t("Back up the database first"));
        widgets::muted(ui, palette, &tf("A full dump in {folder} before anything changes, to put back with Tools > Import full database. Off: faster, but nothing to go back to.", &[("folder", &server_tools::backup_directory())]), 11.5);
        ui.checkbox(&mut form.restart, t("Restart the emulator afterwards"));
        widgets::muted(ui, palette, t("The emulator reads items_base and the catalogue when it starts: players are disconnected for a moment. Off: restart it yourself later (Tools)."), 11.5);
    }

    fn validate(&self) -> Option<String> {
        let mut form = self.form.lock().unwrap();
        if form.reloading {
            return Some(t("Still reading the database: wait a moment.").into());
        }
        let Some(plan) = form.plan() else { return Some(t("Nothing to fix: see the message above.").into()) };
        plan.updates.is_empty().then(|| t("Nothing changes with what is chosen.").into())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        {
            let mut form = self.form.lock().unwrap();
            form.read = None;
            form.plan = None;
            form.rows = None;
            // The local files are read again: they may have been downloaded or merged since.
            form.local.clear();
        }
        let (database_used, databases) = choose_database(server.as_ref(), &ui).await?;
        {
            let mut form = self.form.lock().unwrap();
            form.database = database_used;
            form.databases = databases;
            form.server = Some(server.clone());
            form.runtime = Some(tokio::runtime::Handle::current());
            form.reloading = false;
            form.reload_error = None;
        }
        let (database, rows) = item_fix::read_items(server.as_ref()).await?;
        // The server's FurnitureData.json (it can be a merged or custom one) and the local ones. The chosen one is
        // used; when it is not there, the first of the others that is (a server without the file yet, say).
        let server_furnidata = super::offer_fix::download_furnidata(server.as_ref()).await.map(Arc::new).map_err(|e| format!("{}: {e}", aio_remote::offer_fix::furniture_data_path()));
        let folders = bundle_folders();
        let (fields, chosen_source) = {
            let form = self.form.lock().unwrap();
            (form.fields, form.source)
        };
        let order = std::iter::once(chosen_source).chain(Source::ALL.into_iter().filter(|s| *s != chosen_source));
        let mut found = None;
        let mut reasons = Vec::new();
        for source in order {
            let attempt = match source.local_file() {
                None => server_furnidata.clone(),
                Some(path) => {
                    let read = tokio::task::spawn_blocking(move || read_local_json(&path).map(Arc::new)).await?;
                    self.form.lock().unwrap().local.insert(source, read.clone());
                    read
                }
            };
            match attempt {
                Ok(value) => {
                    found = Some((source, value));
                    break;
                }
                Err(reason) => reasons.push(format!("{}: {reason}", source.label())),
            }
        }
        let Some((source, furnidata)) = found else {
            fail!("{}\n{}", t("No FurnitureData.json can be used:"), reasons.join("\n"));
        };
        if source != chosen_source {
            ui.log(&tf("> FurnitureData.json: {chosen} cannot be used, {used} is", &[("chosen", chosen_source.label()), ("used", source.label())]));
        }
        self.form.lock().unwrap().source = source;
        // Thousands of .hab files: not on the window's thread.
        let chosen = furnidata.clone();
        let (read, plan) = tokio::task::spawn_blocking(move || {
            let bundles = item_fix::read_bundles(&item_fix::bundle_names(&rows), &folders);
            let plan = item_fix::plan(&rows, &chosen, &bundles, fields);
            (Read { rows, server_furnidata, bundles }, plan)
        })
        .await?;
        let (count, changes, checked) = (plan.rows(), plan.changes.len(), plan.checked);
        // "Done" only when no choice on the page could change anything: the button is set from this once, and
        // ticking another box must not leave it at Skip.
        let all = Fields { sprite: true, kind: true, size: true, flags: true, modes: true, multiheight: true, extra_heights: true };
        let nothing_at_all = item_fix::plan(&read.rows, &furnidata, &read.bundles, all).updates.is_empty();
        {
            let mut form = self.form.lock().unwrap();
            form.read = Some(Arc::new(read));
            form.plan = Some((fields, source, Arc::new(plan)));
        }
        if nothing_at_all {
            ui.detected(ResultKind::Success, t("items_base is in line"), &tf("{database}: {checked} furni checked, nothing to change.", &[("database", &database), ("checked", &checked.to_string())]));
            return Ok(true);
        }
        if count == 0 {
            ui.detected(ResultKind::Info, t("Nothing changes with what is chosen"), &tf("{database}: {checked} furni checked; another choice below does change something.", &[("database", &database), ("checked", &checked.to_string())]));
            return Ok(false);
        }
        ui.detected(
            ResultKind::Info,
            &tf("{count} furni can be fixed", &[("count", &count.to_string())]),
            &tf("{database}: {changes} values change. Check the list below first.", &[("database", &database), ("changes", &changes.to_string())]),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let (read, fields, restart, source, furnidata, backup) = {
            let mut form = self.form.lock().unwrap();
            let Some(read) = form.read.clone() else { fail!("Nothing to fix.") };
            let furnidata = match form.furnidata() {
                Ok(furnidata) => furnidata,
                Err(error) => fail!("{error}"),
            };
            (read, form.fields, form.restart, form.source, furnidata, form.backup)
        };
        match source.local_file() {
            None => {
                log("> Reading the server's FurnitureData.json again ...");
                if read.server_furnidata.as_deref().ok() != Some(&super::offer_fix::download_furnidata(server).await?) {
                    fail!("{}", t("FurnitureData.json on the server changed since the check: go Back and open this page again, so the fix starts from the new one."));
                }
            }
            Some(path) => log(&format!("> FurnitureData.json: {}", path.display())),
        }
        log("> Checking the database again ...");
        // From items_base as it is now, with the .hab files the check read.
        let (database, rows) = item_fix::read_items(server).await?;
        let plan = item_fix::plan(&rows, &furnidata, &read.bundles, fields);
        if plan.updates.is_empty() {
            fail!("{}", t("Nothing changes any more: items_base is in line."));
        }
        log(&format!("  {database}: {} furni get {} new values", plan.rows(), plan.changes.len()));
        ui.done(&database);

        let folder = aio_core::InstallerSettings::load().assets_folder().join("Import_SQL");
        std::fs::create_dir_all(&folder)?;
        let stamp = aio_core::config_dir::timestamp();
        let local_sql = folder.join(format!("items_base_fix_{stamp}.sql"));
        std::fs::write(&local_sql, item_fix::sql(&plan))?;
        log(&format!("  The SQL is kept in {}", local_sql.display()));
        let directory = server_tools::backup_directory();
        server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
        let remote_sql = format!("{directory}/items-base-fix-{stamp}.sql");
        server.upload(&local_sql, &remote_sql, &|_| {}).await?;
        let result = furni_import::import_with(server, &remote_sql, backup, &log).await?;
        ui.done(&tf("{count} furni", &[("count", &plan.rows().to_string())]));
        let safety_backup = Some(result.safety_backup).filter(|b| !b.is_empty());
        *self.output.lock().unwrap() = Output { rows: plan.rows(), changes: plan.changes.len(), safety_backup };

        if restart {
            let stopped = server_tools::stop_emulator(server, &log).await?;
            if stopped {
                server_tools::start_emulator(server, &log).await?;
            }
            ui.done(if stopped { t("restarted") } else { t("not installed") });
        } else {
            ui.done(t("skipped"));
        }
        Ok(ui.succeeded(
            t("items_base is in line"),
            &tf("{rows} furni got {changes} new values. Players see it after they reload the hotel.", &[("rows", &plan.rows().to_string()), ("changes", &plan.changes.len().to_string())]),
        ))
    }
}

/// Makes sure the tools use a database that has items_base. The configured one (the emulator's config.ini, else
/// Settings) is used when it has one; otherwise the only database that has items_base, or the one the user picks
/// among several. The choice holds for every tool until the installer closes and is saved in Settings.
pub(super) async fn choose_database(server: &dyn Server, ui: &StepUi) -> anyhow::Result<(String, Vec<String>)> {
    let (configured, found) = item_fix::hotel_databases(server).await?;
    if found.iter().any(|d| *d == configured) {
        return Ok((configured, found));
    }
    let chosen = match found.as_slice() {
        [] => fail!("{}", tf("No database on this server has an items_base table (looked for {database} first): install the emulator's database first.", &[("database", &configured)])),
        [only] => only.clone(),
        several => {
            let options = several.iter().map(|d| crate::gui::dialogs::Choice::new(d, t("Has an items_base table."), Some(d.clone()))).collect();
            let message = tf("{database} has no items_base table on this server. Which database is the hotel's? The choice is used by every tool and saved in Settings (Server layout, Hotel database).", &[("database", &configured)]);
            match ui.choose(t("Hotel database"), t("Which database is the hotel's?"), &message, None, options).await {
                Some(chosen) => chosen,
                None => fail!("{}", t("No hotel database chosen.")),
            }
        }
    };
    use_database(&chosen);
    ui.log(&tf("> Hotel database: {database} ({configured} has no items_base table)", &[("database", &chosen), ("configured", &configured)]));
    Ok((chosen, found))
}

/// `database` for every tool until the installer closes, and saved as Settings > Server layout > Hotel database.
pub(super) fn use_database(database: &str) {
    aio_core::ServerLayout::choose_database(database);
    let mut settings = aio_core::InstallerSettings::load();
    let mut layout = settings.layout.clone().unwrap_or_default();
    layout.database = database.to_string();
    settings.layout = Some(layout);
    let _ = settings.save();
}

/// Reads `database`'s items_base (and the .hab files its furni need) from the page, after the user chose it.
fn reload(locked: &mut Form, form: Arc<Mutex<Form>>, database: String, ctx: egui::Context) {
    let (Some(server), Some(runtime)) = (locked.server.clone(), locked.runtime.clone()) else { return };
    locked.reloading = true;
    locked.reload_error = None;
    use_database(&database);
    runtime.spawn(async move {
        let result = async {
            let (_, rows) = item_fix::read_items(server.as_ref()).await?;
            let folders = bundle_folders();
            let read = tokio::task::spawn_blocking(move || {
                let bundles = item_fix::read_bundles(&item_fix::bundle_names(&rows), &folders);
                (rows, bundles)
            })
            .await?;
            anyhow::Ok(read)
        }
        .await;
        let mut f = form.lock().unwrap();
        f.reloading = false;
        match result {
            Ok((rows, bundles)) => {
                let server_furnidata = f.read.as_ref().map(|r| r.server_furnidata.clone()).unwrap_or_else(|| Err(t("Nothing read yet.").to_string()));
                f.read = Some(Arc::new(Read { rows, server_furnidata, bundles }));
                f.database = database;
                f.plan = None;
                f.rows = None;
            }
            Err(error) => f.reload_error = Some(format!("{database}: {error}")),
        }
        drop(f);
        ctx.request_repaint();
    });
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(ItemFixStep::new());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| {
            let output = finish_step.output.lock().unwrap().clone();
            super::finished::item_fix(output.rows, output.changes, output.safety_backup, info.steps)
        }),
        t("items_base fixed"),
        t("Completed"),
    )
}
