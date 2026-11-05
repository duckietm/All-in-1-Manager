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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum List {
    #[default]
    Changes,
    NotInFurnidata,
    NoBundle,
}

/// What the check read, kept to work out the plan again when the choices change.
struct Read {
    rows: Vec<ItemRow>,
    furnidata: Value,
    bundles: HashMap<String, Option<Bundle>>,
}

struct Form {
    read: Option<Arc<Read>>,
    fields: Fields,
    /// The plan, and the choices it was made for.
    plan: Option<(Fields, Arc<FixPlan>)>,
    restart: bool,
    list: List,
    filter: String,
    /// The rows of the list shown, made when it changes (not on every frame).
    rows: Option<((Fields, List, String), Arc<Vec<Vec<String>>>)>,
}

impl Default for Form {
    fn default() -> Self {
        Self { read: None, fields: Fields::default(), plan: None, restart: true, list: List::Changes, filter: String::new(), rows: None }
    }
}

impl Form {
    /// The plan for the current choices; made again only when they change.
    fn plan(&mut self) -> Option<Arc<FixPlan>> {
        let read = self.read.clone()?;
        if let Some((fields, plan)) = &self.plan {
            if *fields == self.fields {
                return Some(plan.clone());
            }
        }
        let plan = Arc::new(item_fix::plan(&read.rows, &read.furnidata, &read.bundles, self.fields));
        self.plan = Some((self.fields, plan.clone()));
        Some(plan)
    }

    fn rows(&mut self) -> Arc<Vec<Vec<String>>> {
        let key = (self.fields, self.list, self.filter.trim().to_lowercase());
        if let Some((have, rows)) = &self.rows {
            if *have == key {
                return rows.clone();
            }
        }
        let Some(plan) = self.plan() else { return Arc::new(Vec::new()) };
        let matches = |name: &str| key.2.is_empty() || name.to_lowercase().contains(&key.2);
        let rows: Vec<Vec<String>> = match self.list {
            List::Changes => plan.changes.iter().filter(|c| matches(&c.name)).map(|c| vec![c.id.to_string(), c.name.clone(), c.column.to_string(), c.from.clone(), c.to.clone()]).collect(),
            List::NotInFurnidata => plan.not_in_furnidata.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect(),
            List::NoBundle => plan.no_bundle.iter().filter(|n| matches(n)).map(|n| vec![n.clone()]).collect(),
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
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl ItemFixStep {
    pub fn new() -> Self {
        Self { form: Mutex::new(Form::default()), output: Mutex::new(Output::default()) }
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
                t("Only floor and wall furni; pets, effects and bots are left alone, and so is every multiheight furni (interaction_type multiheight, or a multiheight value).").into(),
                t("sprite_id from the server's FurnitureData.json (it can be a custom one); sit / lay / walk, and on request the type, from it too.").into(),
                t("Size and stack height from the .hab files you downloaded (Habbo assets, then the Nitro retro profiles); a wall furni is 1 x 1 x 0.00. Furni without a .hab here keep their size.").into(),
                t("Interaction modes only for interaction_type default: other interactions count their own states.").into(),
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
        ui.label(RichText::new(t("What to fix")).size(14.0).strong());
        let fields = &mut form.fields;
        ui.checkbox(&mut fields.sprite, t("sprite_id, from FurnitureData.json"));
        ui.checkbox(&mut fields.kind, t("type (floor s / wall i), from FurnitureData.json"));
        widgets::muted(ui, palette, t("Off (default) for type: furni players already placed keep their floor or wall position, so a furni that changes type can end up broken in their rooms."), 11.5);
        ui.checkbox(&mut fields.size, t("width, length and stack_height, from the .hab files"));
        ui.checkbox(&mut fields.flags, t("allow_sit, allow_lay and allow_walk, from FurnitureData.json"));
        ui.checkbox(&mut fields.modes, t("interaction_modes_count, from the .hab files (interaction_type default only)"));
        widgets::muted(ui, palette, t("allow_stack is never changed: no file says whether a furni can be stacked on (the .hab only has its height), so the value you set stays."), 11.5);
        let Some(plan) = form.plan() else { return };
        let rows = form.rows();
        ui.add_space(8.0);
        ui.label(RichText::new(t("What changes")).size(14.0).strong());
        widgets::muted(
            ui,
            palette,
            &tf(
                "{checked} furni checked, {multiheight} multiheight furni left alone: {rows} furni get {changes} new values.",
                &[
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
            ] {
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
            List::NoBundle => {
                widgets::muted(ui, palette, t("Their .hab file is not in your downloads (Habbo assets, Nitro retro): their size and modes stay as they are; sprite_id and sit / lay / walk are still fixed."), 12.0);
                table(ui, palette, "item_no_bundle", &[400.0], &["item_name"], &rows, 0);
            }
        }
        ui.add_space(8.0);
        ui.checkbox(&mut form.restart, t("Restart the emulator afterwards"));
        widgets::muted(ui, palette, t("The emulator reads items_base and the catalogue when it starts: players are disconnected for a moment. Off: restart it yourself later (Tools)."), 11.5);
    }

    fn validate(&self) -> Option<String> {
        let mut form = self.form.lock().unwrap();
        let Some(plan) = form.plan() else { return Some(t("Nothing to fix: see the message above.").into()) };
        plan.updates.is_empty().then(|| t("Nothing changes with what is chosen.").into())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        {
            let mut form = self.form.lock().unwrap();
            form.read = None;
            form.plan = None;
            form.rows = None;
        }
        let (database, rows) = item_fix::read_items(server.as_ref()).await?;
        // Always the server's own FurnitureData.json: it can be a merged or custom one.
        let furnidata = super::offer_fix::download_furnidata(server.as_ref()).await?;
        let folders = bundle_folders();
        let fields = self.form.lock().unwrap().fields;
        // Thousands of .hab files: not on the window's thread.
        let (read, plan) = tokio::task::spawn_blocking(move || {
            let bundles = item_fix::read_bundles(&item_fix::bundle_names(&rows), &folders);
            let plan = item_fix::plan(&rows, &furnidata, &bundles, fields);
            (Read { rows, furnidata, bundles }, plan)
        })
        .await?;
        let (count, changes, checked) = (plan.rows(), plan.changes.len(), plan.checked);
        // "Done" only when no choice on the page could change anything: the button is set from this once, and
        // ticking another box must not leave it at Skip.
        let all = Fields { sprite: true, kind: true, size: true, flags: true, modes: true };
        let nothing_at_all = item_fix::plan(&read.rows, &read.furnidata, &read.bundles, all).updates.is_empty();
        {
            let mut form = self.form.lock().unwrap();
            form.read = Some(Arc::new(read));
            form.plan = Some((fields, Arc::new(plan)));
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
        let (read, fields, restart) = {
            let form = self.form.lock().unwrap();
            let Some(read) = form.read.clone() else { fail!("Nothing to fix.") };
            (read, form.fields, form.restart)
        };
        log("> Reading the server's FurnitureData.json again ...");
        if super::offer_fix::download_furnidata(server).await? != read.furnidata {
            fail!("{}", t("FurnitureData.json on the server changed since the check: go Back and open this page again, so the fix starts from the new one."));
        }
        log("> Checking the database again ...");
        // From items_base as it is now, with the .hab files the check read.
        let (database, rows) = item_fix::read_items(server).await?;
        let plan = item_fix::plan(&rows, &read.furnidata, &read.bundles, fields);
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
        let result = furni_import::import(server, &remote_sql, &log).await?;
        ui.done(&tf("{count} furni", &[("count", &plan.rows().to_string())]));
        *self.output.lock().unwrap() = Output { rows: plan.rows(), changes: plan.changes.len(), safety_backup: Some(result.safety_backup) };

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
