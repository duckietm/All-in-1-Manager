//! Import Assets: furniture the hotel database lacks into items_base, with a catalogue item each on a new page
//! (parent_id -1 or a page you choose). The page shows what will be imported first. The items_base values are the
//! All-in-1 Converter's Generate SQL (see aio_remote::furni_import).
//!
//! Two pages use it: Habbo's furniture (Get Assets' download), and a Nitro retro profile's custom furniture. For a
//! profile, its FurnitureData.json is merged into Habbo's first (aio_remote::furni_merge): Habbo's furniture and ids
//! lead, then the profiles in the order they were merged, without a classname twice or an id twice. The merged
//! FurnitureData.json goes to the server with the import, so the client knows the custom furni's new ids.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_core::settings::RetroProfile;
use aio_remote::furni_import::{self, DbState, ImportResult, NewFurni, Plan};
use aio_remote::furni_merge::{self, MergeState, Merged};
use aio_remote::habbo_assets::{self, retro};
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

/// Habbo's furniture, or a Nitro retro profile's custom furniture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Habbo,
    Custom,
}

/// Get Assets' Habbo download: (FurnitureData.json, the furniture folder, the assets folder).
fn sources() -> (PathBuf, PathBuf, PathBuf) {
    let folder = aio_core::InstallerSettings::load().assets_folder();
    let gamedata = habbo_assets::gamedata_folder(&folder);
    (gamedata.join("config").join("FurnitureData.json"), gamedata.join("furniture"), folder)
}

/// A profile's own gamedata folder (custom/<name>/gamedata), by its name: also for a profile deleted since.
fn profile_gamedata(assets: &Path, name: &str) -> PathBuf {
    habbo_assets::gamedata_folder(&retro::custom_folder(assets, &RetroProfile { name: name.into(), ..RetroProfile::default() }))
}

/// Where the merge keeps its state (the profiles' order and ids) and the merged FurnitureData.json.
fn merge_files(assets: &Path) -> (PathBuf, PathBuf) {
    let folder = assets.join(retro::CUSTOM);
    (folder.join("merge.json"), folder.join("FurnitureData.json"))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_state(assets: &Path) -> MergeState {
    read_json(&merge_files(assets).0).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// A profile merged and planned: the merged FurnitureData.json, which report is the profile's, and its import.
struct Prepared {
    profile: String,
    merged: Merged,
    index: usize,
    plan: Plan,
}

impl Prepared {
    fn report(&self) -> &furni_merge::ProfileReport {
        &self.merged.reports[self.index]
    }
}

/// Merges `profile` (after the profiles merged before it) into Habbo's `original` and plans the import of its furni.
/// Reads .hab files: not on the window's thread.
fn prepare(original: &Value, profile: &str, db: &DbState, assets: &Path) -> Result<Prepared, String> {
    let state = load_state(assets);
    let order = state.order_with(profile);
    let mut profiles = Vec::new();
    for name in &order {
        let file = profile_gamedata(assets, name).join("config").join("FurnitureData.json");
        let json = read_json(&file).map_err(|e| {
            if name == profile {
                tf("{file} is not there: download Custom furniture for this profile in Nitro retro first.", &[("file", &file.display().to_string())])
            } else {
                tf("The profile {name} was merged before, but its FurnitureData.json cannot be read ({error}): download it again in Nitro retro.", &[("name", name), ("error", &e)])
            }
        })?;
        profiles.push((name.clone(), json));
    }
    let merged = furni_merge::merge_new(original, &profiles, &state, &db.offer_ids);
    let index = order.iter().position(|p| p == profile).unwrap_or(0);
    let mut plan = furni_import::plan(&merged.reports[index].added, &profile_gamedata(assets, profile).join("furniture"), db);
    // Furni without a catalogue item are the Habbo page's business.
    plan.uncatalogued.clear();
    // Custom furni of every merged profile that items_base has with another sprite_id than they have now (Habbo
    // took their id): items_base follows, or the client shows other furniture.
    for report in &merged.reports {
        for (section, kind) in [("roomitemtypes", 's'), ("wallitemtypes", 'i')] {
            for entry in report.added[section]["furnitype"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let (Some(name), Some(id)) = (entry["classname"].as_str(), entry["id"].as_i64()) else { continue };
                if let Some(&(sprite, db_kind)) = db.sprites.get(&name.trim().to_lowercase()) {
                    if db_kind == kind && sprite != id {
                        plan.moved.push(furni_import::Moved { name: name.trim().to_string(), kind, from: sprite, to: id });
                    }
                }
                // Its catalogue items with another offer id than FurnitureData.json now has for it.
                if let Some(offer) = entry["offerid"].as_i64() {
                    for &(item, now) in db.catalog.get(&name.trim().to_lowercase()).map(Vec::as_slice).unwrap_or_default() {
                        if now != offer {
                            plan.offers.push((item, name.trim().to_string(), now, offer));
                        }
                    }
                }
            }
        }
    }
    Ok(Prepared { profile: profile.into(), merged, index, plan })
}

/// Which list of the overview is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum List {
    #[default]
    New,
    NoBundle,
    Uncatalogued,
    Duplicates,
    Already,
    Renumbered,
    Moved,
    Offers,
    Capped,
    Multiheight,
    TooLong,
    Pages,
}

struct Form {
    plan: Option<Plan>,
    database: String,
    caption: String,
    /// The page the new page goes under, as typed: -1 (default) is the top of the catalogue.
    parent: String,
    /// The catalogue pages of the database (id -> caption), to check the parent.
    pages: BTreeMap<i64, String>,
    /// The new page is shown in the catalogue straight away (off: hidden until it is moved).
    visible: bool,
    /// At most this many furni per catalogue page, as typed (more: pages under a folder).
    per_page: String,
    /// Also a catalogue item for the items_base furni no catalogue item sells.
    with_uncatalogued: bool,
    restart: bool,
    list: List,
    filter: String,
    // Custom furniture only:
    /// The Nitro retro profiles, and the one chosen.
    profiles: Vec<String>,
    profile: Option<String>,
    /// Habbo's FurnitureData.json and the database, read by detect, for preparing another profile.
    original: Option<Arc<Value>>,
    db: Option<Arc<DbState>>,
    /// Shared: it holds the whole merged FurnitureData.json, and the page is drawn every frame.
    prepared: Option<Arc<Prepared>>,
    preparing: bool,
    problem: Option<String>,
    /// The merged FurnitureData.json to the server with the import.
    put_furnidata: bool,
    /// The merge order shown for the chosen profile ("Habbo → one → two (new)"), worked out when it is chosen.
    merge_order: String,
    /// Get Assets' folder, read from Settings when the page checks (not on every frame).
    assets: PathBuf,
    /// Counts up whenever `plan` changes, so the cached lists below know to make themselves again.
    version: u64,
    /// The rows of the list shown, and the catalogue pages, made when what they show changes (the page is drawn
    /// on every frame and mouse move; 26 000 rows each time made it slow).
    rows: Option<(RowsKey, Arc<Vec<Vec<String>>>)>,
    page_list: Option<(PagesKey, Arc<Vec<(String, usize)>>)>,
}

/// What the cached rows show.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowsKey {
    version: u64,
    list: List,
    filter: String,
    pages: PagesKey,
}

/// What the cached catalogue pages are made of.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PagesKey {
    version: u64,
    per_page: String,
    caption: String,
    with_uncatalogued: bool,
}

impl Form {
    fn set_plan(&mut self, plan: Option<Plan>) {
        self.plan = plan;
        self.version += 1;
    }

    fn pages_key(&self) -> PagesKey {
        PagesKey { version: self.version, per_page: self.per_page.trim().into(), caption: self.caption.trim().into(), with_uncatalogued: self.with_uncatalogued }
    }

    /// The catalogue pages of the plan, made again only when they change.
    fn pages(&mut self) -> Arc<Vec<(String, usize)>> {
        let key = self.pages_key();
        if let Some((have, pages)) = &self.page_list {
            if *have == key {
                return pages.clone();
            }
        }
        let pages = match (&self.plan, per_page(&key.per_page)) {
            (Some(plan), Ok(n)) => page_list(plan, &key.caption, key.with_uncatalogued, n),
            _ => Vec::new(),
        };
        let pages = Arc::new(pages);
        self.page_list = Some((key, pages.clone()));
        pages
    }

    /// The rows of the list shown, filtered, made again only when they change.
    fn rows(&mut self) -> Arc<Vec<Vec<String>>> {
        let key = RowsKey { version: self.version, list: self.list, filter: self.filter.trim().to_lowercase(), pages: self.pages_key() };
        if let Some((have, rows)) = &self.rows {
            if *have == key {
                return rows.clone();
            }
        }
        let pages = self.pages();
        let filter = key.filter.clone();
        let matches = |name: &str| filter.is_empty() || name.to_lowercase().contains(&filter);
        let names = |list: &[String]| -> Vec<Vec<String>> { list.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect() };
        let rows = match (&self.plan, self.list, &self.prepared) {
            (None, _, _) => Vec::new(),
            (Some(plan), List::New, _) => plan
                .new
                .iter()
                .filter(|f| matches(&f.classname))
                .map(|f| {
                    vec![
                        f.classname.clone(),
                        if f.kind == 'i' { t("wall").into() } else { t("floor").into() },
                        size(f),
                        f.modes.to_string(),
                        allows(f),
                        f.interaction_type.to_string(),
                        f.sprite_id.map_or("–".into(), |s| s.to_string()),
                        f.offer_id.to_string(),
                    ]
                })
                .collect(),
            (Some(plan), List::NoBundle, _) => names(&plan.no_bundle),
            (Some(plan), List::Uncatalogued, _) => plan
                .uncatalogued
                .iter()
                .filter(|u| matches(&u.name))
                .map(|u| vec![u.name.clone(), u.id.to_string(), u.sprite_id.to_string(), u.offer_id.to_string()])
                .collect(),
            (Some(_), List::Duplicates, Some(p)) => names(&p.report().duplicates),
            (Some(_), List::Already, Some(p)) => names(&p.report().already),
            (Some(_), List::Renumbered, Some(p)) => p.report().renumbered.iter().filter(|r| matches(&r.0)).map(|(name, old, new)| vec![name.clone(), old.to_string(), new.to_string()]).collect(),
            (Some(plan), List::TooLong, _) => names(&plan.too_long),
            (Some(plan), List::Multiheight, _) => plan
                .new
                .iter()
                .filter(|f| matches(&f.classname))
                .filter_map(|f| f.multiheight.as_ref().map(|h| vec![f.classname.clone(), f.interaction_type.to_string(), h.clone()]))
                .collect(),
            (Some(plan), List::Capped, _) => plan.capped.iter().filter(|c| matches(&c.0)).map(|(name, height)| vec![name.clone(), height.to_string(), height.clamp(-furni_import::MAX_STACK_HEIGHT, furni_import::MAX_STACK_HEIGHT).to_string()]).collect(),
            (Some(plan), List::Offers, _) => plan.offers.iter().filter(|o| matches(&o.1)).map(|(id, name, from, to)| vec![id.to_string(), name.clone(), from.to_string(), to.to_string()]).collect(),
            (Some(plan), List::Moved, _) => plan.moved.iter().filter(|m| matches(&m.name)).map(|m| vec![m.name.clone(), m.from.to_string(), m.to.to_string()]).collect(),
            (Some(_), List::Pages, _) => pages.iter().filter(|(c, _)| matches(c)).map(|(caption, count)| vec![caption.clone(), count.to_string()]).collect(),
            _ => Vec::new(),
        };
        let rows = Arc::new(rows);
        self.rows = Some((key, rows.clone()));
        rows
    }
}

impl Form {
    fn new(mode: Mode) -> Self {
        let date = chrono::Local::now().date_naive();
        Self {
            plan: None,
            database: String::new(),
            caption: match mode {
                Mode::Habbo => furni_import::page_caption(date),
                Mode::Custom => format!("custom_{}", date.format("%Y%m%d")),
            },
            parent: "-1".into(),
            pages: Default::default(),
            visible: false,
            per_page: "500".into(),
            with_uncatalogued: false,
            restart: true,
            list: List::New,
            filter: String::new(),
            profiles: Vec::new(),
            profile: None,
            original: None,
            db: None,
            prepared: None,
            preparing: false,
            problem: None,
            put_furnidata: true,
            merge_order: String::new(),
            assets: sources().2,
            version: 0,
            rows: None,
            page_list: None,
        }
    }
}

#[derive(Default, Clone)]
struct Output {
    result: Option<ImportResult>,
    caption: String,
    furniture: usize,
    catalogue_items: usize,
    local_sql: Option<PathBuf>,
    furnidata: Option<(PathBuf, Option<String>)>,
    /// The imported furni's .hab files and icons that are here but not on the server.
    missing_files: Vec<furni_import::ClientFile>,
}

pub struct ImportAssetsStep {
    mode: Mode,
    form: Arc<Mutex<Form>>,
    output: Mutex<Output>,
}

impl ImportAssetsStep {
    pub fn new(mode: Mode) -> Self {
        Self { mode, form: Arc::new(Mutex::new(Form::new(mode))), output: Mutex::new(Output::default()) }
    }

    fn output(&self) -> Output {
        self.output.lock().unwrap().clone()
    }

    /// Merges and plans `profile` on a thread of its own; the form shows it when done.
    fn start_preparing(&self, profile: String, ctx: egui::Context) {
        let mut form = self.form.lock().unwrap();
        let (Some(original), Some(db)) = (form.original.clone(), form.db.clone()) else { return };
        form.profile = Some(profile.clone());
        form.caption = format!("custom_{}_{}", RetroProfile { name: profile.clone(), ..RetroProfile::default() }.folder_name().to_lowercase().replace(' ', "_"), chrono::Local::now().format("%Y%m%d"));
        form.preparing = true;
        form.problem = None;
        form.set_plan(None);
        form.prepared = None;
        form.list = List::New;
        // The merge order, read once here rather than on every frame.
        let state = load_state(&form.assets);
        let steps: Vec<String> = std::iter::once("Habbo".to_string())
            .chain(state.order_with(&profile).iter().map(|p| if state.profiles.contains(p) { p.clone() } else { format!("{p} ({})", t("new")) }))
            .collect();
        form.merge_order = steps.join(" → ");
        drop(form);
        let shared = self.form.clone();
        std::thread::spawn(move || {
            let (_, _, assets) = sources();
            let result = prepare(&original, &profile, &db, &assets);
            let mut form = shared.lock().unwrap();
            // Another profile was chosen meanwhile: that one counts.
            if form.profile.as_deref() == Some(profile.as_str()) {
                match result {
                    Ok(prepared) => {
                        form.set_plan(Some(prepared.plan.clone()));
                        form.prepared = Some(Arc::new(prepared));
                    }
                    Err(problem) => form.problem = Some(problem),
                }
                form.preparing = false;
            }
            drop(form);
            ctx.request_repaint();
        });
    }
}

/// The parent_id typed in: -1 or the id of a catalogue page of the database.
fn parent_id(typed: &str, pages: &BTreeMap<i64, String>) -> Result<i64, String> {
    let typed = typed.trim();
    let id: i64 = typed.parse().map_err(|_| tf("parent_id \"{value}\" is not a number.", &[("value", typed)]))?;
    if id == -1 || pages.contains_key(&id) {
        Ok(id)
    } else {
        Err(tf("No catalogue page has id {id}: use -1 or the id of a page you have.", &[("id", &id.to_string())]))
    }
}

/// The furni per catalogue page typed in: 1 to 5000.
fn per_page(typed: &str) -> Result<usize, String> {
    match typed.trim().parse::<usize>() {
        Ok(n) if (1..=5000).contains(&n) => Ok(n),
        _ => Err(t("Furni per page: a number from 1 to 5000.").into()),
    }
}

/// The catalogue pages an import makes: (caption, furni) per page; one page when everything fits.
fn page_list(plan: &Plan, caption: &str, with_uncatalogued: bool, per_page: usize) -> Vec<(String, usize)> {
    let mut names: Vec<&str> = plan.new.iter().map(|f| f.classname.as_str()).collect();
    if with_uncatalogued {
        names.extend(plan.uncatalogued.iter().map(|u| u.name.as_str()));
    }
    if names.len() <= per_page {
        return if names.is_empty() { Vec::new() } else { vec![(caption.to_string(), names.len())] };
    }
    furni_import::pages(&names, per_page).into_iter().map(|(caption, items)| (caption, items.len())).collect()
}

/// "2 × 2 × 0.8".
fn size(furni: &NewFurni) -> String {
    format!("{} × {} × {}", furni.width, furni.length, furni.height)
}

/// What a furni allows: "sit, stack".
fn allows(furni: &NewFurni) -> String {
    let parts: Vec<&str> = [(furni.allow_sit, t("sit")), (furni.allow_lay, t("lay")), (furni.allow_walk, t("walk")), (furni.allow_stack, t("stack"))]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name)
        .collect();
    if parts.is_empty() { "–".into() } else { parts.join(", ") }
}

/// One cell of the overview: a fixed width, cut with … when too long.
fn cell(ui: &mut egui::Ui, text: impl Into<String>, width: f32, color: egui::Color32, strong: bool) {
    let mut text = RichText::new(text.into()).size(12.5).color(color);
    if strong {
        text = text.strong();
    }
    ui.allocate_ui_with_layout(egui::vec2(width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(width);
        ui.add(egui::Label::new(text).truncate());
    });
}

/// The rows of a list as cells; the widths of the columns, then the cells of each row.
fn table(ui: &mut egui::Ui, palette: &Palette, id: &str, widths: &[f32], header: &[&str], rows: &[Vec<String>]) {
    ui.horizontal(|ui| {
        for (title, width) in header.iter().zip(widths) {
            cell(ui, *title, *width, palette.muted, true);
        }
    });
    egui::ScrollArea::vertical().id_salt(id).max_height(300.0).auto_shrink([false, true]).show_rows(ui, 18.0, rows.len(), |ui, range| {
        for row in &rows[range] {
            ui.horizontal(|ui| {
                for (i, (text, width)) in row.iter().zip(widths).enumerate() {
                    cell(ui, text.as_str(), *width, if i == 0 { palette.text } else { palette.muted }, false);
                }
            });
        }
    });
}

/// Restarts the emulator (when it is there) and says how that went.
async fn restart_emulator(server: &dyn Server, ui: &StepUi, log: &(dyn Fn(&str) + Send + Sync)) -> anyhow::Result<()> {
    let stopped = server_tools::stop_emulator(server, log).await?;
    if stopped {
        server_tools::start_emulator(server, log).await?;
    }
    ui.done(if stopped { t("restarted") } else { t("not installed") });
    Ok(())
}

impl ImportAssetsStep {
    /// The profile buttons and the merge's result (custom furniture).
    fn custom_options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let (profiles, current, preparing, problem, prepared, merge_order) = {
            let form = self.form.lock().unwrap();
            if form.db.is_none() {
                return;
            }
            (form.profiles.clone(), form.profile.clone(), form.preparing, form.problem.clone(), form.prepared.clone(), form.merge_order.clone())
        };
        ui.add_space(8.0);
        ui.label(RichText::new(t("Nitro retro profile")).size(14.0).strong());
        if profiles.is_empty() {
            widgets::muted(ui, palette, t("There is no Nitro retro profile yet: make one in Nitro retro and download its custom furniture first."), 12.0);
            return;
        }
        let mut clicked = None;
        ui.horizontal_wrapped(|ui| {
            for name in &profiles {
                let button = egui::Button::new(RichText::new(name).size(13.0).strong()).selected(current.as_deref() == Some(name.as_str()));
                if ui.add_enabled(!preparing, button).clicked() {
                    clicked = Some(name.clone());
                }
            }
        });
        if let Some(name) = clicked {
            self.start_preparing(name, ui.ctx().clone());
            return;
        }
        if current.is_some() && !merge_order.is_empty() {
            widgets::muted(ui, palette, &tf("Merge order: {order}. Habbo's furniture and ids lead; every profile adds what is not there yet.", &[("order", &merge_order)]), 12.0);
        }
        if preparing {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(t("Merging and reading the .hab files ...")).size(12.5).color(palette.muted));
            });
            return;
        }
        if let Some(problem) = problem {
            ui.label(RichText::new(problem).size(12.5).color(palette.warning_text));
            return;
        }
        let Some(prepared) = prepared else {
            widgets::muted(ui, palette, t("Choose the profile whose custom furniture you want to import."), 12.0);
            return;
        };
        let report = prepared.report();
        ui.label(
            RichText::new(tf(
                "{profile}: {entries} entries in its FurnitureData.json; {duplicates} listed twice, {already} that Habbo or an earlier profile has, {renumbered} with a new id. {added} custom furniture added.",
                &[
                    ("profile", &prepared.profile),
                    ("entries", &report.entries.to_string()),
                    ("duplicates", &report.duplicates.len().to_string()),
                    ("already", &report.already.len().to_string()),
                    ("renumbered", &report.renumbered.len().to_string()),
                    ("added", &report.added_count().to_string()),
                ],
            ))
            .size(12.5)
            .color(palette.muted),
        );
    }
}

#[async_trait]
impl InstallStep for ImportAssetsStep {
    fn title(&self) -> String {
        match self.mode {
            Mode::Habbo => t("Import Assets").into(),
            Mode::Custom => t("Import custom furniture").into(),
        }
    }

    fn caption(&self) -> String {
        match self.mode {
            Mode::Habbo => t("Missing Habbo furniture").into(),
            Mode::Custom => t("From a Nitro retro profile").into(),
        }
    }

    fn install_label(&self) -> String {
        t("Import the furniture").into()
    }

    fn rerun_label(&self) -> String {
        t("Import again").into()
    }

    fn intro(&self) -> Intro {
        let folder = self.form.lock().unwrap().assets.clone();
        let common = [
            t("catalog_items: 5 credits, amount 1, the offer id of FurnitureData.json.").to_string(),
            t("Everything goes on one new catalogue page (parent_id -1, or under a page you choose); move the page, or its furni, where you want afterwards.").into(),
            tf("A backup of the database comes first ({folder}); the import runs as one transaction, so it is done completely or not at all.", &[("folder", &server_tools::backup_directory())]),
        ];
        match self.mode {
            Mode::Habbo => Intro {
                icon: Icon::Database,
                heading: t("Import the missing Habbo furniture").into(),
                description: tf(
                    "Compares the Habbo furniture you downloaded ({folder}) with the hotel database, and adds what it lacks to items_base with a catalogue item each, the way the All-in-1 Converter's Generate SQL does.",
                    &[("folder", &folder.display().to_string())],
                ),
                bullets: std::iter::once(t("items_base: size, height, sit / lay / walk / stack and the interaction modes from each furni's .hab file and FurnitureData.json; a colour variant (rare_dragonlamp*5) uses the .hab of its base.").to_string()).chain(common).collect(),
                footnote: Some(t("Download Furnidata and Furniture in Habbo assets first; Deploy Assets puts the .hab files on the server, so the client shows the furniture.").into()),
            },
            Mode::Custom => Intro {
                icon: Icon::Cube,
                heading: t("Import a retro's custom furniture").into(),
                description: t("Merges the profile's FurnitureData.json into Habbo's, then adds its furniture the database lacks to items_base with a catalogue item each, like Import Assets does for Habbo's.").into(),
                bullets: [
                    t("Habbo's FurnitureData.json (Habbo assets → Furnidata) is the source of truth: its furniture and ids stay as they are.").to_string(),
                    t("From the profile: a classname listed twice counts once, a classname that is already there is left out, and an id that is taken gets a free one above 100000.").into(),
                    t("Several profiles: they are merged in the order you import them, every time, so a later profile never changes an earlier one, and a newer Habbo download is merged in the same way.").into(),
                    tf("The merged FurnitureData.json goes to the server with the import (the old one is kept as .bak-<time>), and is kept in {folder}.", &[("folder", &merge_files(&folder).1.display().to_string())]),
                ]
                .into_iter()
                .chain(common)
                .collect(),
                footnote: Some(t("Download Furnidata in Habbo assets and the profile's Custom furniture in Nitro retro first; upload the profile's .hab files to the server's gamedata/furniture (Asset Explorer).").into()),
            },
        }
    }

    fn checks(&self) -> Vec<String> {
        let mut checks: Vec<String> = vec![t("Check the database").into(), t("Upload the SQL").into(), t("Backup and import").into()];
        if self.mode == Mode::Custom {
            checks.push("FurnitureData.json".into());
        }
        checks.push(t("Restart the emulator").into());
        checks
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        if self.mode == Mode::Custom {
            self.custom_options(ui, palette);
        }
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        if form.plan.is_none() {
            return;
        }
        let page_count = form.pages().len();
        let rows = form.rows();
        let Some(plan) = &form.plan else { return };
        let prepared = form.prepared.clone();
        ui.add_space(8.0);
        ui.label(RichText::new(t("What will be imported")).size(14.0).strong());
        ui.label(
            RichText::new(tf(
                "{count} new furniture: {floor} floor, {wall} wall, of which {variants} colour variants.",
                &[
                    ("count", &plan.new.len().to_string()),
                    ("floor", &plan.floor().to_string()),
                    ("wall", &plan.wall().to_string()),
                    ("variants", &plan.variants().to_string()),
                ],
            ))
            .size(12.5)
            .color(palette.muted),
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let mut lists = vec![
                (List::New, tf("To import ({count})", &[("count", &plan.new.len().to_string())])),
                (List::NoBundle, tf("No .hab file ({count})", &[("count", &plan.no_bundle.len().to_string())])),
            ];
            match &prepared {
                None => lists.push((List::Uncatalogued, tf("Not in the catalogue ({count})", &[("count", &plan.uncatalogued.len().to_string())]))),
                Some(p) => {
                    let r = p.report();
                    lists.push((List::Duplicates, tf("Listed twice ({count})", &[("count", &r.duplicates.len().to_string())])));
                    lists.push((List::Already, tf("Already there ({count})", &[("count", &r.already.len().to_string())])));
                    lists.push((List::Renumbered, tf("New id ({count})", &[("count", &r.renumbered.len().to_string())])));
                    if !plan.offers.is_empty() {
                        lists.push((List::Offers, tf("offer_id to update ({count})", &[("count", &plan.offers.len().to_string())])));
                    }
                    if !plan.moved.is_empty() {
                        lists.push((List::Moved, tf("sprite_id to update ({count})", &[("count", &plan.moved.len().to_string())])));
                    }
                }
            }
            if !plan.capped.is_empty() {
                lists.push((List::Capped, tf("Height too big ({count})", &[("count", &plan.capped.len().to_string())])));
            }
            let multiheight = plan.new.iter().filter(|f| f.multiheight.is_some()).count();
            if multiheight > 0 {
                lists.push((List::Multiheight, tf("Multiheight ({count})", &[("count", &multiheight.to_string())])));
            }
            if !plan.too_long.is_empty() {
                lists.push((List::TooLong, tf("Name too long ({count})", &[("count", &plan.too_long.len().to_string())])));
            }
            let pages = page_count;
            lists.push((List::Pages, tf("Pages ({count})", &[("count", &pages.to_string())])));
            for (list, label) in lists {
                ui.selectable_value(&mut form.list, list, RichText::new(label).size(12.5));
            }
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut form.filter).hint_text(RichText::new(t("Search ...")).italics()).desired_width(180.0));
        });
        match (form.list, &prepared) {
            (List::New, _) => {
                let header = ["item_name", t("Type"), t("Size"), t("Modes"), t("Allows"), "interaction", "sprite_id", "offer_id"];
                table(ui, palette, "import_new", &[230.0, 50.0, 100.0, 50.0, 130.0, 90.0, 70.0, 70.0], &header, &rows);
            }
            (List::NoBundle, None) => {
                widgets::muted(ui, palette, t("Missing in the database, but their .hab file is not in the download (or cannot be read): they are left out. Download Furniture in Habbo assets again to get them."), 12.0);
                table(ui, palette, "import_no_bundle", &[400.0], &["item_name"], &rows);
            }
            (List::NoBundle, Some(_)) => {
                widgets::muted(ui, palette, t("Missing in the database, but their .hab file is not in the profile's folder (or cannot be read): they are left out. Download Custom furniture in Nitro retro again to get them."), 12.0);
                table(ui, palette, "import_no_bundle", &[400.0], &["item_name"], &rows);
            }
            (List::Uncatalogued, _) => {
                widgets::muted(ui, palette, t("Floor and wall furniture in items_base that no catalogue item sells (often on purpose: prizes, gifts, internal furni). Only with the option below do they get a catalogue item on the new page."), 12.0);
                table(ui, palette, "import_uncatalogued", &[260.0, 100.0, 80.0, 80.0], &["item_name", "id", "sprite_id", "offer_id"], &rows);
            }
            (List::Duplicates, Some(_)) => {
                widgets::muted(ui, palette, t("Listed more than once in the profile's FurnitureData.json: the first entry counts, these extra ones are left out."), 12.0);
                table(ui, palette, "import_duplicates", &[400.0], &["classname"], &rows);
            }
            (List::Already, Some(_)) => {
                widgets::muted(ui, palette, t("Habbo's FurnitureData.json or an earlier profile has these classnames already: theirs stays, the profile's entry is left out."), 12.0);
                table(ui, palette, "import_already", &[400.0], &["classname"], &rows);
            }
            (List::Renumbered, Some(_)) => {
                widgets::muted(ui, palette, t("Their id was taken (by Habbo or an earlier profile): they get a free id above 100000, in FurnitureData.json and in items_base.sprite_id alike."), 12.0);
                table(ui, palette, "import_renumbered", &[260.0, 100.0, 100.0], &["classname", t("Its id"), t("New id")], &rows);
            }
            (List::Moved, Some(_)) => {
                widgets::muted(ui, palette, t("Already in items_base, but FurnitureData.json now has them under another id (Habbo took theirs): their sprite_id is updated, so the client keeps showing them."), 12.0);
                table(ui, palette, "import_moved", &[260.0, 100.0, 100.0], &["item_name", t("Now"), t("New id")], &rows);
            }
            (List::Offers, _) => {
                widgets::muted(ui, palette, t("Custom furniture already in the catalogue whose offer id is another one now (Habbo's furni keep theirs, every custom furni gets its own): their catalogue items follow, so they can be bought."), 12.0);
                table(ui, palette, "import_offers", &[90.0, 260.0, 110.0, 110.0], &["catalog_items.id", "catalog_name", t("offer_id now"), t("New offer_id")], &rows);
            }
            (List::Capped, _) => {
                widgets::muted(ui, palette, t("Their .hab file gives a height that items_base.stack_height cannot hold (at most 99.99): they get 99.99 (or -99.99). Check these furni in the hotel."), 12.0);
                table(ui, palette, "import_capped", &[260.0, 120.0, 120.0], &["item_name", t("In the .hab"), "stack_height"], &rows);
            }
            (List::Multiheight, _) => {
                widgets::muted(ui, palette, t("Their .hab is a multiheight furni (logicType furniture_multiheight): they get interaction_type multiheight and a height per state, worked out from how far each state's image moves (rounded to a quarter, at least 0.10). That is right for blocks, but only a rough guess for stages and stairs: check these heights, and correct them in items_base.multiheight where needed."), 12.0);
                table(ui, palette, "import_multiheight", &[260.0, 120.0, 300.0], &["item_name", "interaction", "multiheight"], &rows);
            }
            (List::TooLong, _) => {
                widgets::muted(ui, palette, t("Their classname is longer than items_base.item_name holds (70 characters): they are left out, because a shortened name would no longer match FurnitureData.json."), 12.0);
                table(ui, palette, "import_too_long", &[500.0], &["classname"], &rows);
            }
            (List::Pages, _) => match per_page(&form.per_page) {
                Ok(n) => {
                    if page_count > 1 {
                        widgets::muted(ui, palette, &tf("More than {count} furni: they go on these pages, under the page {caption}. Furni whose names start alike (adobe_kaars2, adobe_kekook) stay on one page; a name that has more gets pages of its own (name-1, name-2).", &[("count", &n.to_string()), ("caption", form.caption.trim())]), 12.0);
                    } else {
                        widgets::muted(ui, palette, &tf("Up to {count} furni go on one page.", &[("count", &n.to_string())]), 12.0);
                    }
                    table(ui, palette, "import_pages", &[300.0, 100.0], &["caption", t("Furni")], &rows);
                }
                Err(e) => {
                    ui.label(RichText::new(e).size(12.0).color(palette.warning_text));
                }
            },
            _ => {}
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("Catalogue page")).size(13.0).strong());
            ui.add(egui::TextEdit::singleline(&mut form.caption).desired_width(220.0));
            ui.add_space(12.0);
            ui.label(RichText::new("parent_id").size(13.0).strong());
            ui.add(egui::TextEdit::singleline(&mut form.parent).desired_width(70.0));
            match parent_id(&form.parent, &form.pages) {
                Ok(-1) => ui.label(RichText::new(t("at the top of the catalogue")).size(12.0).color(palette.muted)),
                Ok(id) => ui.label(RichText::new(tf("under \"{caption}\"", &[("caption", form.pages.get(&id).map(String::as_str).unwrap_or(""))])).size(12.0).color(palette.success_text)),
                Err(e) => ui.label(RichText::new(e).size(12.0).color(palette.warning_text)),
            };
        });
        widgets::muted(ui, palette, t("caption and caption_save of the new page, and the page it goes under: -1 (default) puts it at the top of the catalogue, or fill in the id of one of your pages (catalog_pages.id)."), 11.5);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("Furni per page")).size(13.0).strong());
            ui.add(egui::TextEdit::singleline(&mut form.per_page).desired_width(70.0));
            if let Err(e) = per_page(&form.per_page) {
                ui.label(RichText::new(e).size(12.0).color(palette.warning_text));
            }
        });
        widgets::muted(ui, palette, t("More furni than this: the page above becomes a folder with pages of at most this many under it (see Pages). 500 (default) keeps the catalogue quick."), 11.5);
        ui.checkbox(&mut form.visible, t("Show the page in the catalogue straight away"));
        widgets::muted(ui, palette, t("Off (default): the page is hidden, so players do not see the new furniture (rares too, at 5 credits) before you have checked the prices and moved it."), 11.5);
        if self.mode == Mode::Habbo {
            ui.checkbox(
                &mut form.with_uncatalogued,
                tf("Also a catalogue item for the {count} furniture that is in items_base but not in the catalogue", &[("count", &plan.uncatalogued.len().to_string())]),
            );
        } else {
            ui.checkbox(&mut form.put_furnidata, t("Put the merged FurnitureData.json on the server"));
            widgets::muted(ui, palette, t("On (default): the client then knows the custom furniture with the ids the database gets. The old one is kept next to it as .bak-<time>."), 11.5);
        }
        ui.checkbox(&mut form.restart, t("Restart the emulator afterwards"));
        widgets::muted(ui, palette, t("The emulator reads items_base and the catalogue when it starts: players are disconnected for a moment. Off: restart it yourself later (Tools)."), 11.5);
    }

    fn validate(&self) -> Option<String> {
        let form = self.form.lock().unwrap();
        if self.mode == Mode::Custom {
            if form.preparing {
                return Some(t("Wait until the profile is merged.").into());
            }
            if let Some(problem) = &form.problem {
                return Some(problem.clone());
            }
            if form.prepared.is_none() {
                return Some(t("Choose the profile whose custom furniture you want to import.").into());
            }
        }
        let Some(plan) = &form.plan else { return Some(t("Nothing to import: see the message above.").into()) };
        if form.caption.trim().is_empty() {
            return Some(t("Give the catalogue page a name.").into());
        }
        if let Err(e) = parent_id(&form.parent, &form.pages) {
            return Some(e);
        }
        if let Err(e) = per_page(&form.per_page) {
            return Some(e);
        }
        (plan.work(form.with_uncatalogued) == 0).then(|| t("There is nothing to import.").into())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        {
            let mut form = self.form.lock().unwrap();
            form.set_plan(None);
            form.prepared = None;
            form.db = None;
        }
        let (furnidata_path, furniture, assets) = sources();
        self.form.lock().unwrap().assets = assets.clone();
        let furnidata = match read_json(&furnidata_path) {
            Ok(json) => json,
            Err(_) => {
                ui.detected(
                    ResultKind::Warning,
                    t("Download the Habbo furniture first"),
                    &tf("{file} is not there: in Habbo assets, download Furnidata and Furniture, then come back.", &[("file", &furnidata_path.display().to_string())]),
                );
                return Ok(false);
            }
        };
        let state = furni_import::inspect(server.as_ref()).await?;
        let (database, items_base, catalog_items, pages) = (state.database.clone(), state.items_base, state.catalog_items, state.pages.clone());
        let now = tf(
            "The database {database} has {furniture} furniture and {items} catalogue items.",
            &[("database", &database), ("furniture", &items_base.to_string()), ("items", &catalog_items.to_string())],
        );
        if self.mode == Mode::Custom {
            let profiles: Vec<String> = aio_core::InstallerSettings::load().retro_profiles().into_iter().map(|p| p.name).collect();
            let chosen = {
                let mut form = self.form.lock().unwrap();
                form.database = database.clone();
                form.pages = pages;
                form.original = Some(Arc::new(furnidata));
                form.db = Some(Arc::new(state));
                // The profile chosen before, else the last one merged, else the first.
                let merged_last = load_state(&assets).profiles.last().cloned();
                let chosen = form.profile.clone().filter(|p| profiles.contains(p)).or(merged_last.filter(|p| profiles.contains(p))).or(profiles.first().cloned());
                form.profiles = profiles;
                chosen
            };
            match chosen {
                Some(profile) => self.start_preparing(profile, ui_ctx(&ui)),
                None => self.form.lock().unwrap().profile = None,
            }
            ui.detected(ResultKind::Info, t("Choose the profile and check the list below"), &now);
            return Ok(false);
        }
        // Thousands of .hab files on a new hotel: not on the window's thread.
        let plan = tokio::task::spawn_blocking(move || furni_import::plan(&furnidata, &furniture, &state)).await?;
        let (new, no_bundle, uncatalogued) = (plan.new.len(), plan.no_bundle.len(), plan.uncatalogued.len());
        {
            let mut form = self.form.lock().unwrap();
            form.database = database.clone();
            form.pages = pages;
            form.set_plan(Some(plan));
        }
        let skipped =
            if no_bundle > 0 { format!(" {}", tf("{count} more are missing but have no .hab file in the download.", &[("count", &no_bundle.to_string())])) } else { String::new() };
        if new == 0 {
            ui.detected(ResultKind::Success, t("Every downloaded Habbo furniture is in the database"), &format!("{now}{skipped}"));
            // Done, unless the option for the furniture without a catalogue item has something to do.
            return Ok(uncatalogued == 0);
        }
        ui.detected(ResultKind::Info, &tf("{count} furniture can be imported", &[("count", &new.to_string())]), &format!("{now}{skipped} {}", t("Check the list below first.")));
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let (mut plan, caption, parent, visible, with_uncatalogued, restart, prepared, put_furnidata, typed_per_page) = {
            let form = self.form.lock().unwrap();
            let Some(plan) = form.plan.clone() else { fail!("Nothing to import.") };
            (
                plan,
                form.caption.trim().to_string(),
                form.parent.clone(),
                form.visible,
                form.with_uncatalogued && self.mode == Mode::Habbo,
                form.restart,
                form.prepared.clone(),
                form.put_furnidata,
                form.per_page.clone(),
            )
        };
        let per_page = match per_page(&typed_per_page) {
            Ok(n) => n,
            Err(e) => fail!("{e}"),
        };
        if self.mode == Mode::Custom && prepared.is_none() {
            fail!("{}", t("Choose the profile whose custom furniture you want to import."));
        }
        log("> Checking the database again ...");
        // Something may have been added since the overview: that is left out now.
        let state = furni_import::inspect(server).await?;
        let before = plan.new.len();
        plan.new.retain(|f| !state.names.contains(&f.classname.to_lowercase()));
        let still: std::collections::HashSet<i64> = state.uncatalogued.iter().map(|u| u.id).collect();
        plan.uncatalogued.retain(|u| still.contains(&u.id));
        if plan.new.len() < before {
            log(&format!("  {} furniture were added in the meantime and are left out", before - plan.new.len()));
        }
        // A sprite_id that moved meanwhile is not touched (the UPDATE names the one it had).
        plan.moved.retain(|m| state.sprites.get(&m.name.to_lowercase()) == Some(&(m.from, m.kind)));
        plan.offers.retain(|(id, name, from, _)| state.catalog.get(&name.to_lowercase()).is_some_and(|items| items.contains(&(*id, *from))));
        let extra = if with_uncatalogued { plan.uncatalogued.len() } else { 0 };
        if plan.work(with_uncatalogued) == 0 {
            fail!("There is nothing to import any more.");
        }
        log(&format!(
            "  {}: {} new furniture, {} catalogue items for furniture without one, {} sprite_ids and {} offer_ids to update",
            state.database,
            plan.new.len(),
            extra,
            plan.moved.len(),
            plan.offers.len()
        ));
        ui.done(&state.database);

        // The parent must still be there (checked against the database as it is now).
        let parent = match parent_id(&parent, &state.pages) {
            Ok(id) => id,
            Err(e) => fail!("{e}"),
        };
        let sql = furni_import::sql(&plan, &caption, parent, visible, with_uncatalogued, per_page);
        // A copy on this computer, to read back what ran.
        let (_, _, folder) = sources();
        // Where the imported furni's .hab files and icons are on this computer.
        let client_gamedata = match &prepared {
            Some(p) => profile_gamedata(&folder, &p.profile),
            None => habbo_assets::gamedata_folder(&folder),
        };
        let stamp = aio_core::config_dir::timestamp();
        let local_dir = folder.join("Import_SQL");
        std::fs::create_dir_all(&local_dir)?;
        let safe: String = caption.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') { c } else { '_' }).collect();
        let local_sql = local_dir.join(format!("{safe}_{stamp}.sql"));
        std::fs::write(&local_sql, &sql)?;
        log(&format!("  The SQL is kept in {}", local_sql.display()));
        let directory = server_tools::backup_directory();
        let remote_sql = format!("{directory}/import-assets-{stamp}.sql");
        server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
        log("> Uploading the SQL");
        server.upload(&local_sql, &remote_sql, &|_| {}).await?;
        ui.done(&format!("{:.0} KB", sql.len() as f64 / 1024.0));

        let result = furni_import::import(server, &remote_sql, &log).await?;
        if result.page_id > 0 {
            log(&format!("  The catalogue page {caption} has id {}", result.page_id));
            if result.pages > 1 {
                log(&format!("  The catalogue items are on {} pages under it", result.pages));
            }
            ui.done(&tf("page {id}", &[("id", &result.page_id.to_string())]));
        } else {
            ui.done(&tf("{count} sprite_ids", &[("count", &plan.moved.len().to_string())]));
        }
        let mut output =
            Output { result: Some(result.clone()), caption: caption.clone(), furniture: plan.new.len(), catalogue_items: plan.new.len() + extra, local_sql: Some(local_sql), furnidata: None, missing_files: Vec::new() };

        if let Some(prepared) = prepared {
            // The ids are in the database now: the merge keeps them, and the merged file is kept here.
            let (state_file, merged_file) = merge_files(&folder);
            std::fs::create_dir_all(state_file.parent().unwrap_or(&folder))?;
            std::fs::write(&merged_file, serde_json::to_string_pretty(&prepared.merged.furnidata)?)?;
            std::fs::write(&state_file, serde_json::to_string_pretty(&prepared.merged.state)?)?;
            log(&format!("  The merged FurnitureData.json is kept in {}", merged_file.display()));
            if put_furnidata {
                let remote_json = format!("{directory}/import-assets-{stamp}.json");
                server.upload(&merged_file, &remote_json, &|_| {}).await?;
                let old = furni_import::put_furniture_data(server, &remote_json, &log).await?;
                output.furnidata = Some((merged_file.clone(), old));
                ui.done(t("on the server"));
            } else {
                output.furnidata = Some((merged_file.clone(), None));
                ui.done(t("kept here"));
            }
        }
        // Which of their files the server lacks: the finish page offers to upload them.
        let files = furni_import::client_files(&plan.new, &client_gamedata);
        match furni_import::missing_on_server(server, &files).await {
            Ok(missing) => {
                if !missing.is_empty() {
                    let habs = missing.iter().filter(|f| f.folder == "furniture").count();
                    log(&format!("  {habs} .hab files and {} icons are not on the server yet: the next page can upload them", missing.len() - habs));
                }
                output.missing_files = missing;
            }
            Err(e) => log(&format!("  ! {e}")),
        }
        *self.output.lock().unwrap() = output;

        if restart {
            restart_emulator(server, &ui, &log).await?;
        } else {
            ui.done(t("skipped"));
        }
        if result.page_id <= 0 {
            return Ok(ui.succeeded(
                &tf("{count} sprite_ids updated", &[("count", &plan.moved.len().to_string())]),
                t("items_base follows the merged FurnitureData.json again; there was no new furniture to import."),
            ));
        }
        if result.pages > 1 {
            return Ok(ui.succeeded(
                &tf("{count} furniture imported", &[("count", &plan.new.len().to_string())]),
                &tf(
                    "On {pages} catalogue pages under {caption} (id {id}), {state}.",
                    &[
                        ("pages", &result.pages.to_string()),
                        ("caption", &caption),
                        ("id", &result.page_id.to_string()),
                        ("state", if visible { t("visible") } else { t("hidden until you move it") }),
                    ],
                ),
            ));
        }
        Ok(ui.succeeded(
            &tf("{count} furniture imported", &[("count", &plan.new.len().to_string())]),
            &tf(
                "On the catalogue page {caption} (id {id}), {state}.",
                &[("caption", &caption), ("id", &result.page_id.to_string()), ("state", if visible { t("visible") } else { t("hidden until you move it") })],
            ),
        ))
    }
}

/// The window's context, to draw again when a thread is done.
fn ui_ctx(ui: &StepUi) -> egui::Context {
    ui.context()
}

pub fn wizard(shared: &Shared, mode: Mode) -> Wizard {
    let step = Arc::new(ImportAssetsStep::new(mode));
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        {
            let shared = shared.clone();
            Arc::new(move |info| super::finished::import_assets(finish_step.output().into(), info.steps, shared.clone()))
        },
        t("Furniture imported"),
        t("Completed"),
    )
}

/// What the finish page shows.
pub struct Imported {
    pub result: Option<ImportResult>,
    pub caption: String,
    pub furniture: usize,
    pub catalogue_items: usize,
    pub local_sql: Option<PathBuf>,
    /// The merged FurnitureData.json kept here, and the server's old one (custom furniture).
    pub furnidata: Option<(PathBuf, Option<String>)>,
    /// The imported furni's .hab files and icons that the server does not have.
    pub missing_files: Vec<furni_import::ClientFile>,
}

impl From<Output> for Imported {
    fn from(o: Output) -> Self {
        Self {
            result: o.result,
            caption: o.caption,
            furniture: o.furniture,
            catalogue_items: o.catalogue_items,
            local_sql: o.local_sql,
            furnidata: o.furnidata,
            missing_files: o.missing_files,
        }
    }
}

/// The upload of the missing files from the finish page.
#[derive(Default)]
pub struct FileUpload {
    pub running: bool,
    pub sent: u64,
    pub result: Option<Result<String, String>>,
}

/// Uploads `files` to the server the window is logged in to, on the runtime; `state` shows how far it is.
pub fn upload_missing(shared: &Shared, files: Vec<furni_import::ClientFile>, state: Arc<Mutex<FileUpload>>) {
    let Some(server) = shared.session() else {
        state.lock().unwrap().result = Some(Err(t("Not connected to the server: log in again and run the import once more.").into()));
        return;
    };
    *state.lock().unwrap() = FileUpload { running: true, ..FileUpload::default() };
    let ctx = shared.ctx.clone();
    shared.spawn(async move {
        let progress = |bytes: u64| {
            state.lock().unwrap().sent = bytes;
            ctx.request_repaint();
        };
        let result = furni_import::upload_client_files(server.as_ref(), &files, &progress).await;
        let mut s = state.lock().unwrap();
        s.running = false;
        s.result = Some(match result {
            Ok(count) => Ok(tf("{count} files uploaded. Players see the furniture after they reload the hotel.", &[("count", &count.to_string())])),
            Err(e) => Err(e.to_string()),
        });
    });
}
