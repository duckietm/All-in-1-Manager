//! Import Assets: the Habbo furniture of Get Assets' download that the hotel database lacks into items_base, with a
//! catalogue item each on a new page "import_<date>" (parent_id -1). The page shows what will be imported first.
//! The items_base values are the All-in-1 Converter's Generate SQL (see aio_remote::furni_import).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aio_remote::furni_import::{self, ImportResult, NewFurni, Plan};
use aio_remote::{fail, server_tools, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// Get Assets' Habbo download: (FurnitureData.json, the furniture folder, the assets folder).
fn sources() -> (PathBuf, PathBuf, PathBuf) {
    let folder = aio_core::InstallerSettings::load().assets_folder();
    let gamedata = aio_remote::habbo_assets::gamedata_folder(&folder);
    (gamedata.join("config").join("FurnitureData.json"), gamedata.join("furniture"), folder)
}

/// Which list of the overview is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum List {
    #[default]
    New,
    NoBundle,
    Uncatalogued,
}

struct Form {
    plan: Option<Plan>,
    database: String,
    caption: String,
    /// The page the new page goes under, as typed: -1 (default) is the top of the catalogue.
    parent: String,
    /// The catalogue pages of the database (id -> caption), to check the parent.
    pages: std::collections::BTreeMap<i64, String>,
    /// The new page is shown in the catalogue straight away (off: hidden until it is moved).
    visible: bool,
    /// Also a catalogue item for the items_base furni no catalogue item sells.
    with_uncatalogued: bool,
    restart: bool,
    list: List,
    filter: String,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            plan: None,
            database: String::new(),
            caption: furni_import::page_caption(chrono::Local::now().date_naive()),
            parent: "-1".into(),
            pages: Default::default(),
            visible: false,
            with_uncatalogued: false,
            restart: true,
            list: List::New,
            filter: String::new(),
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
}

pub struct ImportAssetsStep {
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl ImportAssetsStep {
    pub fn new() -> Self {
        Self { form: Mutex::new(Form::default()), output: Mutex::new(Output::default()) }
    }

    fn output(&self) -> Output {
        self.output.lock().unwrap().clone()
    }
}

impl Default for ImportAssetsStep {
    fn default() -> Self {
        Self::new()
    }
}

/// The parent_id typed in: -1 or the id of a catalogue page of the database.
fn parent_id(typed: &str, pages: &std::collections::BTreeMap<i64, String>) -> Result<i64, String> {
    let typed = typed.trim();
    let id: i64 = typed.parse().map_err(|_| tf("parent_id \"{value}\" is not a number.", &[("value", typed)]))?;
    if id == -1 || pages.contains_key(&id) {
        Ok(id)
    } else {
        Err(tf("No catalogue page has id {id}: use -1 or the id of a page you have.", &[("id", &id.to_string())]))
    }
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

#[async_trait]
impl InstallStep for ImportAssetsStep {
    fn title(&self) -> String {
        t("Import Assets").into()
    }

    fn caption(&self) -> String {
        t("Missing Habbo furniture").into()
    }

    fn install_label(&self) -> String {
        t("Import the furniture").into()
    }

    fn rerun_label(&self) -> String {
        t("Import again").into()
    }

    fn intro(&self) -> Intro {
        let (_, _, folder) = sources();
        Intro {
            icon: Icon::Database,
            heading: t("Import the missing Habbo furniture").into(),
            description: tf(
                "Compares the Habbo furniture you downloaded ({folder}) with the hotel database, and adds what it lacks to items_base with a catalogue item each, the way the All-in-1 Converter's Generate SQL does.",
                &[("folder", &folder.display().to_string())],
            ),
            bullets: vec![
                t("items_base: size, height, sit / lay / walk / stack and the interaction modes from each furni's .hab file and FurnitureData.json; a colour variant (rare_dragonlamp*5) uses the .hab of its base.").into(),
                t("catalog_items: 5 credits, amount 1, the offer id of FurnitureData.json.").into(),
                t("Everything goes on one new catalogue page (parent_id -1, or under a page you choose); move the page, or its furni, where you want afterwards.").into(),
                tf("A backup of the database comes first ({folder}); the import runs as one transaction, so it is done completely or not at all.", &[("folder", &server_tools::backup_directory())]),
            ],
            footnote: Some(t("Download Furnidata and Furniture in Habbo assets first; Deploy Assets puts the .hab files on the server, so the client shows the furniture.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the database").into(), t("Upload the SQL").into(), t("Backup and import").into(), t("Restart the emulator").into()]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        let Some(plan) = &form.plan else { return };
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
            let lists = [
                (List::New, tf("To import ({count})", &[("count", &plan.new.len().to_string())])),
                (List::NoBundle, tf("No .hab file ({count})", &[("count", &plan.no_bundle.len().to_string())])),
                (List::Uncatalogued, tf("Not in the catalogue ({count})", &[("count", &plan.uncatalogued.len().to_string())])),
            ];
            for (list, label) in lists {
                ui.selectable_value(&mut form.list, list, RichText::new(label).size(12.5));
            }
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut form.filter).hint_text(RichText::new(t("Search ...")).italics()).desired_width(180.0));
        });
        let filter = form.filter.trim().to_lowercase();
        let matches = |name: &str| filter.is_empty() || name.to_lowercase().contains(&filter);
        match form.list {
            List::New => {
                let rows: Vec<Vec<String>> = plan
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
                    .collect();
                let header = ["item_name", t("Type"), t("Size"), t("Modes"), t("Allows"), "interaction", "sprite_id", "offer_id"];
                table(ui, palette, "import_new", &[230.0, 50.0, 100.0, 50.0, 130.0, 90.0, 70.0, 70.0], &header, &rows);
            }
            List::NoBundle => {
                widgets::muted(ui, palette, t("Missing in the database, but their .hab file is not in the download (or cannot be read): they are left out. Download Furniture in Habbo assets again to get them."), 12.0);
                let rows: Vec<Vec<String>> = plan.no_bundle.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect();
                table(ui, palette, "import_no_bundle", &[400.0], &["item_name"], &rows);
            }
            List::Uncatalogued => {
                widgets::muted(ui, palette, t("Floor and wall furniture in items_base that no catalogue item sells (often on purpose: prizes, gifts, internal furni). Only with the option below do they get a catalogue item on the new page."), 12.0);
                let rows: Vec<Vec<String>> = plan
                    .uncatalogued
                    .iter()
                    .filter(|u| matches(&u.name))
                    .map(|u| vec![u.name.clone(), u.id.to_string(), u.sprite_id.to_string(), u.offer_id.to_string()])
                    .collect();
                table(ui, palette, "import_uncatalogued", &[260.0, 100.0, 80.0, 80.0], &["item_name", "id", "sprite_id", "offer_id"], &rows);
            }
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
        ui.checkbox(&mut form.visible, t("Show the page in the catalogue straight away"));
        widgets::muted(ui, palette, t("Off (default): the page is hidden, so players do not see the new furniture (rares too, at 5 credits) before you have checked the prices and moved it."), 11.5);
        ui.checkbox(
            &mut form.with_uncatalogued,
            tf("Also a catalogue item for the {count} furniture that is in items_base but not in the catalogue", &[("count", &plan.uncatalogued.len().to_string())]),
        );
        ui.checkbox(&mut form.restart, t("Restart the emulator afterwards"));
        widgets::muted(ui, palette, t("The emulator reads items_base and the catalogue when it starts: players are disconnected for a moment. Off: restart it yourself later (Tools)."), 11.5);
    }

    fn validate(&self) -> Option<String> {
        let form = self.form.lock().unwrap();
        let Some(plan) = &form.plan else { return Some(t("Nothing to import: see the message above.").into()) };
        if form.caption.trim().is_empty() {
            return Some(t("Give the catalogue page a name.").into());
        }
        if let Err(e) = parent_id(&form.parent, &form.pages) {
            return Some(e);
        }
        let extra = if form.with_uncatalogued { plan.uncatalogued.len() } else { 0 };
        (plan.new.is_empty() && extra == 0).then(|| t("There is nothing to import.").into())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        self.form.lock().unwrap().plan = None;
        let (furnidata_path, furniture, _) = sources();
        let furnidata = match std::fs::read(&furnidata_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                ui.detected(
                    ResultKind::Warning,
                    t("Download the Habbo furniture first"),
                    &tf("{file} is not there: in Habbo assets, download Furnidata and Furniture, then come back.", &[("file", &furnidata_path.display().to_string())]),
                );
                return Ok(false);
            }
        };
        let furnidata: serde_json::Value = serde_json::from_slice(furnidata.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&furnidata))?;
        let state = furni_import::inspect(server.as_ref()).await?;
        let (database, items_base, catalog_items, pages) = (state.database.clone(), state.items_base, state.catalog_items, state.pages.clone());
        // Thousands of .hab files on a new hotel: not on the window's thread.
        let plan = tokio::task::spawn_blocking(move || furni_import::plan(&furnidata, &furniture, &state)).await?;
        let (new, no_bundle) = (plan.new.len(), plan.no_bundle.len());
        {
            let mut form = self.form.lock().unwrap();
            form.database = database.clone();
            form.pages = pages;
            form.plan = Some(plan);
        }
        let now = tf(
            "The database {database} has {furniture} furniture and {items} catalogue items.",
            &[("database", &database), ("furniture", &items_base.to_string()), ("items", &catalog_items.to_string())],
        );
        let skipped =
            if no_bundle > 0 { format!(" {}", tf("{count} more are missing but have no .hab file in the download.", &[("count", &no_bundle.to_string())])) } else { String::new() };
        if new == 0 {
            ui.detected(ResultKind::Success, t("Every downloaded Habbo furniture is in the database"), &format!("{now}{skipped}"));
            return Ok(true);
        }
        ui.detected(ResultKind::Info, &tf("{count} furniture can be imported", &[("count", &new.to_string())]), &format!("{now}{skipped} {}", t("Check the list below first.")));
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let (mut plan, caption, parent, visible, with_uncatalogued, restart) = {
            let form = self.form.lock().unwrap();
            let Some(plan) = form.plan.clone() else { fail!("Nothing to import.") };
            (plan, form.caption.trim().to_string(), form.parent.clone(), form.visible, form.with_uncatalogued, form.restart)
        };
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
        let extra = if with_uncatalogued { plan.uncatalogued.len() } else { 0 };
        if plan.new.is_empty() && extra == 0 {
            fail!("There is nothing to import any more.");
        }
        log(&format!("  {}: {} new furniture, {} catalogue items for furniture without one", state.database, plan.new.len(), extra));
        ui.done(&state.database);

        // The parent must still be there (checked against the database as it is now).
        let parent = match parent_id(&parent, &state.pages) {
            Ok(id) => id,
            Err(e) => fail!("{e}"),
        };
        let sql = furni_import::sql(&plan, &caption, parent, visible, with_uncatalogued);
        // A copy on this computer, to read back what ran.
        let (_, _, folder) = sources();
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
        log(&format!("  The catalogue page {caption} has id {}", result.page_id));
        ui.done(&tf("page {id}", &[("id", &result.page_id.to_string())]));
        *self.output.lock().unwrap() =
            Output { result: Some(result.clone()), caption: caption.clone(), furniture: plan.new.len(), catalogue_items: plan.new.len() + extra, local_sql: Some(local_sql) };

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
            &tf("{count} furniture imported", &[("count", &plan.new.len().to_string())]),
            &tf(
                "On the catalogue page {caption} (id {id}), {state}.",
                &[("caption", &caption), ("id", &result.page_id.to_string()), ("state", if visible { t("visible") } else { t("hidden until you move it") })],
            ),
        ))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(ImportAssetsStep::new());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| super::finished::import_assets(finish_step.output().into(), info.steps)),
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
}

impl From<Output> for Imported {
    fn from(o: Output) -> Self {
        Self { result: o.result, caption: o.caption, furniture: o.furniture, catalogue_items: o.catalogue_items, local_sql: o.local_sql }
    }
}
