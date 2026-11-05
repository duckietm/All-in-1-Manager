//! Fix offer IDs: catalog_items.offer_id and the server's FurnitureData.json brought in line, as the All-in-1
//! Converter's SetOfferID does (see aio_remote::offer_fix). The page shows what changes first.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aio_remote::offer_fix::{self, OfferPlan};
use aio_remote::{fail, furni_import, server_tools, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum List {
    #[default]
    Updates,
    Generated,
    Kept,
}

struct Form {
    plan: Option<Arc<OfferPlan>>,
    /// The server's FurnitureData.json as the check read it: the import reads it again and stops when it changed.
    source: Option<Arc<serde_json::Value>>,
    database: String,
    restart: bool,
    list: List,
    filter: String,
    /// The rows of the list shown, made when the list or the search changes (not on every frame).
    rows: Option<((List, String), Arc<Vec<Vec<String>>>)>,
}

impl Default for Form {
    fn default() -> Self {
        Self { plan: None, source: None, database: String::new(), restart: true, list: List::Updates, filter: String::new(), rows: None }
    }
}

impl Form {
    fn rows(&mut self) -> Arc<Vec<Vec<String>>> {
        let key = (self.list, self.filter.trim().to_lowercase());
        if let Some((have, rows)) = &self.rows {
            if *have == key {
                return rows.clone();
            }
        }
        let matches = |name: &str| key.1.is_empty() || name.to_lowercase().contains(&key.1);
        let rows = match (&self.plan, self.list) {
            (None, _) => Vec::new(),
            (Some(p), List::Updates) => p.updates.iter().filter(|u| matches(&u.1)).map(|(id, name, from, to)| vec![id.to_string(), name.clone(), from.to_string(), to.to_string()]).collect(),
            (Some(p), List::Generated) => p.generated.iter().filter(|g| matches(&g.0)).map(|(name, id)| vec![name.clone(), id.to_string()]).collect(),
            (Some(p), List::Kept) => p.kept.iter().filter(|k| matches(&k.0)).map(|(name, id)| vec![name.clone(), id.to_string()]).collect(),
        };
        let rows = Arc::new(rows);
        self.rows = Some((key, rows.clone()));
        rows
    }
}

#[derive(Default, Clone)]
struct Output {
    updated: usize,
    generated: usize,
    safety_backup: Option<String>,
    furnidata: Option<(PathBuf, Option<String>)>,
}

pub struct OfferFixStep {
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl OfferFixStep {
    pub fn new() -> Self {
        Self { form: Mutex::new(Form::default()), output: Mutex::new(Output::default()) }
    }
}

impl Default for OfferFixStep {
    fn default() -> Self {
        Self::new()
    }
}

/// One cell: a fixed width, cut with … when too long.
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

fn table(ui: &mut egui::Ui, palette: &Palette, id: &str, widths: &[f32], header: &[&str], rows: &[Vec<String>]) {
    ui.horizontal(|ui| {
        for (title, width) in header.iter().zip(widths) {
            cell(ui, title, *width, palette.muted, true);
        }
    });
    egui::ScrollArea::vertical().id_salt(id).max_height(300.0).auto_shrink([false, true]).show_rows(ui, 18.0, rows.len(), |ui, range| {
        for row in &rows[range] {
            ui.horizontal(|ui| {
                for (i, (text, width)) in row.iter().zip(widths).enumerate() {
                    cell(ui, text, *width, if i == 1 || widths.len() == 2 && i == 0 { palette.text } else { palette.muted }, false);
                }
            });
        }
    });
}

/// The server's FurnitureData.json, downloaded into a temporary file and read.
async fn download_furnidata(server: &dyn Server) -> anyhow::Result<serde_json::Value> {
    let local = std::env::temp_dir().join(format!("all-in-1-furnidata-{}-{}.json", std::process::id(), aio_core::config_dir::timestamp()));
    let downloaded = server.download(&offer_fix::furniture_data_path(), &local, &|_| {}).await;
    let bytes = std::fs::read(&local);
    let _ = std::fs::remove_file(&local);
    downloaded?;
    let bytes = bytes?;
    Ok(serde_json::from_slice(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes))?)
}

#[async_trait]
impl InstallStep for OfferFixStep {
    fn title(&self) -> String {
        t("Fix offer IDs").into()
    }

    fn caption(&self) -> String {
        t("Catalogue and FurnitureData.json").into()
    }

    fn install_label(&self) -> String {
        t("Fix the offer IDs").into()
    }

    fn rerun_label(&self) -> String {
        t("Check again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shopping,
            heading: t("Bring the offer IDs in line").into(),
            description: tf(
                "The client buys a furni with the offer id of FurnitureData.json; the emulator looks it up in catalog_items.offer_id. Where they differ, or FurnitureData.json has -1, the furni cannot be bought. This fixes both, like the All-in-1 Converter's SetOfferID, on {file}.",
                &[("file", &offer_fix::furniture_data_path())],
            ),
            bullets: vec![
                t("A furni with offerid -1 in FurnitureData.json gets a free offer id above 60000 (every entry of its classname); one whose catalogue items already share an offer id nothing else uses keeps that one.").into(),
                t("Every catalogue item whose catalog_name is a furni's classname gets that furni's offer id; other catalogue items (deals, bundles) stay as they are.").into(),
                tf("A backup of the database comes first ({folder}); the update runs as one transaction. The old FurnitureData.json is kept next to it as .bak-<time>.", &[("folder", &server_tools::backup_directory())]),
            ],
            footnote: Some(t("Run it after an import (Import Assets, Import custom furniture) or a new FurnitureData.json; running it again changes nothing that is right.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the database").into(), t("Backup and update").into(), "FurnitureData.json".into(), t("Restart the emulator").into()]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        let Some(plan) = form.plan.clone() else { return };
        let rows = form.rows();
        ui.add_space(8.0);
        ui.label(RichText::new(t("What changes")).size(14.0).strong());
        widgets::muted(
            ui,
            palette,
            &tf(
                "{furni} furni in FurnitureData.json, {items} catalogue items: {updates} catalogue items get another offer_id, {generated} furni get a new offer id, {kept} keep the one their catalogue items have.",
                &[
                    ("furni", &plan.furni.to_string()),
                    ("items", &plan.catalog_items.to_string()),
                    ("updates", &plan.updates.len().to_string()),
                    ("generated", &plan.generated.len().to_string()),
                    ("kept", &plan.kept.len().to_string()),
                ],
            ),
            12.5,
        );
        ui.horizontal_wrapped(|ui| {
            for (list, label) in [
                (List::Updates, tf("Catalogue items ({count})", &[("count", &plan.updates.len().to_string())])),
                (List::Generated, tf("New offer ids ({count})", &[("count", &plan.generated.len().to_string())])),
                (List::Kept, tf("Kept from the catalogue ({count})", &[("count", &plan.kept.len().to_string())])),
            ] {
                ui.selectable_value(&mut form.list, list, RichText::new(label).size(12.5));
            }
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut form.filter).hint_text(RichText::new(t("Search ...")).italics()).desired_width(180.0));
        });
        match form.list {
            List::Updates => table(ui, palette, "offer_updates", &[90.0, 280.0, 110.0, 110.0], &["catalog_items.id", "catalog_name", t("offer_id now"), t("New offer_id")], &rows),
            List::Generated => table(ui, palette, "offer_generated", &[300.0, 120.0], &["classname", "offerid"], &rows),
            List::Kept => table(ui, palette, "offer_kept", &[300.0, 120.0], &["classname", "offerid"], &rows),
        }
        ui.add_space(8.0);
        ui.checkbox(&mut form.restart, t("Restart the emulator afterwards"));
        widgets::muted(ui, palette, t("The emulator reads items_base and the catalogue when it starts: players are disconnected for a moment. Off: restart it yourself later (Tools)."), 11.5);
    }

    fn validate(&self) -> Option<String> {
        let form = self.form.lock().unwrap();
        let Some(plan) = &form.plan else { return Some(t("Nothing to fix: see the message above.").into()) };
        (plan.updates.is_empty() && !plan.furnidata_changes()).then(|| t("Every offer id is right already.").into())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        self.form.lock().unwrap().plan = None;
        let (database, catalog) = offer_fix::read_catalog(server.as_ref()).await?;
        // Always the server's own FurnitureData.json: it can be a merged or custom one.
        let furnidata = Arc::new(download_furnidata(server.as_ref()).await?);
        let source = furnidata.clone();
        let plan = tokio::task::spawn_blocking(move || offer_fix::plan_new(&furnidata, &catalog)).await?;
        let (updates, generated, nothing) = (plan.updates.len(), plan.generated.len(), plan.updates.is_empty() && !plan.furnidata_changes());
        {
            let mut form = self.form.lock().unwrap();
            form.database = database.clone();
            form.plan = Some(Arc::new(plan));
            form.source = Some(source);
            form.rows = None;
        }
        if nothing {
            ui.detected(ResultKind::Success, t("Every offer id is right already"), &tf("{database}: catalog_items and FurnitureData.json agree.", &[("database", &database)]));
            return Ok(true);
        }
        ui.detected(
            ResultKind::Info,
            &tf("{count} catalogue items get another offer_id", &[("count", &updates.to_string())]),
            &tf("{generated} furni get a new offer id in FurnitureData.json. Check the list below first.", &[("generated", &generated.to_string())]),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let (plan, source, restart) = {
            let form = self.form.lock().unwrap();
            let (Some(plan), Some(source)) = (form.plan.clone(), form.source.clone()) else { fail!("Nothing to fix.") };
            (plan, source, form.restart)
        };
        log("> Reading the server's FurnitureData.json again ...");
        // The fix is worked out from the file the check read; a file changed since (a new merge or custom file)
        // would be overwritten with the old one.
        if download_furnidata(server).await? != *source {
            fail!("{}", t("FurnitureData.json on the server changed since the check: press Check again, so the fix starts from the new one."));
        }
        log("  unchanged since the check");
        log("> Checking the database again ...");
        let (database, catalog) = offer_fix::read_catalog(server).await?;
        log(&format!("  {database}: {} catalogue items, {} to update", catalog.len(), plan.updates.len()));
        ui.done(&database);

        let folder = aio_core::InstallerSettings::load().assets_folder().join("Import_SQL");
        std::fs::create_dir_all(&folder)?;
        let stamp = aio_core::config_dir::timestamp();
        let directory = server_tools::backup_directory();
        server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
        let mut output = Output { generated: plan.generated.len(), ..Output::default() };
        if !plan.updates.is_empty() {
            let local_sql = folder.join(format!("offer_ids_{stamp}.sql"));
            std::fs::write(&local_sql, offer_fix::sql(&plan))?;
            log(&format!("  The SQL is kept in {}", local_sql.display()));
            let remote_sql = format!("{directory}/offer-ids-{stamp}.sql");
            server.upload(&local_sql, &remote_sql, &|_| {}).await?;
            let result = furni_import::import(server, &remote_sql, &log).await?;
            output.safety_backup = Some(result.safety_backup);
            output.updated = plan.updates.len();
            ui.done(&tf("{count} catalogue items", &[("count", &plan.updates.len().to_string())]));
        } else {
            ui.done(t("nothing to update"));
        }

        if plan.furnidata_changes() {
            let local_json = folder.join(format!("FurnitureData_offer_ids_{stamp}.json"));
            std::fs::write(&local_json, serde_json::to_string_pretty(&plan.furnidata)?)?;
            let remote_json = format!("{directory}/offer-ids-{stamp}.json");
            server.upload(&local_json, &remote_json, &|_| {}).await?;
            let old = furni_import::put_furniture_data(server, &remote_json, &log).await?;
            output.furnidata = Some((local_json, old));
            ui.done(&tf("{count} new offer ids", &[("count", &plan.generated.len().to_string())]));
        } else {
            ui.done(t("unchanged"));
        }
        *self.output.lock().unwrap() = output.clone();

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
            t("The offer IDs are in line"),
            &tf(
                "{updates} catalogue items got another offer_id, {generated} furni a new offer id. Players see it after they reload the hotel (and the client loads FurnitureData.json anew).",
                &[("updates", &output.updated.to_string()), ("generated", &output.generated.to_string())],
            ),
        ))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(OfferFixStep::new());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| {
            let output = finish_step.output.lock().unwrap().clone();
            super::finished::offer_fix(output.updated, output.generated, output.safety_backup, output.furnidata, info.steps)
        }),
        t("Offer IDs fixed"),
        t("Completed"),
    )
}
