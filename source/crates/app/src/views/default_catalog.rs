//! The Default catalogue addon: the catalogue that comes with the installer (assets/catalog/Full_Catalog.zip:
//! Catalog_Export.sql and FurnitureData.json) into the hotel database and gamedata/config.

use std::io::Read;
use std::sync::{Arc, Mutex};

use aio_remote::default_catalog::{self, CatalogResult};
use aio_remote::{fail, server_tools, Server};
use async_trait::async_trait;
use eframe::egui;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// The catalogue, packed into the program.
const CATALOG_ZIP: &[u8] = include_bytes!("../../assets/catalog/Full_Catalog.zip");

/// The two files of the catalogue ZIP.
struct Catalog {
    sql: Vec<u8>,
    furniture_data: Vec<u8>,
    version: String,
}

/// Opens the packed catalogue and checks it: an export with its date, and FurnitureData.json that is JSON.
fn open_catalog() -> Result<Catalog, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(CATALOG_ZIP)).map_err(|e| e.to_string())?;
    let mut read = |name: &str| -> Result<Vec<u8>, String> {
        let mut file = archive.by_name(name).map_err(|_| format!("{name} is missing in the catalogue."))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    };
    let sql = read("Catalog_Export.sql")?;
    let furniture_data = read("FurnitureData.json")?;
    let head = String::from_utf8_lossy(&sql[..sql.len().min(512)]).into_owned();
    let version = default_catalog::version_of(&head).ok_or("Catalog_Export.sql has no export date in its first line.")?;
    serde_json::from_slice::<serde_json::Value>(&furniture_data).map_err(|e| format!("FurnitureData.json is not valid JSON: {e}"))?;
    Ok(Catalog { sql, furniture_data, version })
}

/// Why the catalogue cannot be installed (yet), in the window's language, or None. (CatalogState::problem)
fn problem(state: &default_catalog::CatalogState) -> Option<String> {
    state.problem()?;
    Some(if !state.database_ready {
        match &state.database {
            Some(db) => tf(
                "The hotel database {db} has no emulator tables yet: run Install New Server up to the Emulator step (the emulator creates its tables when it starts).",
                &[("db", db)],
            ),
            None => t("There is no hotel database yet: run Install New Server first (MariaDB and the Emulator step).").into(),
        }
    } else {
        tf("{folder} does not exist yet: run Install New Server up to the Octane step first.", &[("folder", &aio_remote::octane::gamedata())])
    })
}

/// The version of the packed catalogue (its export date), for the page.
pub fn packed_version() -> String {
    open_catalog().map(|c| c.version).unwrap_or_default()
}

#[derive(Default)]
struct Form {
    /// The catalogue on the server is replaced: asked when it has one.
    confirmed: bool,
    has_catalog: bool,
}

#[derive(Default)]
struct Output {
    result: Option<CatalogResult>,
    version: String,
}

pub struct DefaultCatalogStep {
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl DefaultCatalogStep {
    pub fn new() -> Self {
        Self { form: Mutex::new(Form::default()), output: Mutex::new(Output::default()) }
    }

    pub fn result(&self) -> Option<CatalogResult> {
        self.output.lock().unwrap().result.clone()
    }

    pub fn version(&self) -> String {
        self.output.lock().unwrap().version.clone()
    }
}

impl Default for DefaultCatalogStep {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InstallStep for DefaultCatalogStep {
    fn title(&self) -> String {
        t("Default catalogue").into()
    }

    fn caption(&self) -> String {
        t("Catalogue and furniture data").into()
    }

    fn install_label(&self) -> String {
        t("Install the catalogue").into()
    }

    fn rerun_label(&self) -> String {
        t("Install again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shopping,
            heading: t("Install the default catalogue").into(),
            description: tf(
                "A complete catalogue for a new hotel ({version}): the catalogue pages and items, the furniture (items_base) and the client's FurnitureData.json, which belong together.",
                &[("version", &packed_version())],
            ),
            bullets: vec![
                t("Replaces the catalogue tables and items_base in the hotel database (Catalog_Export.sql).").into(),
                tf("Puts FurnitureData.json in {folder}; an existing one is kept next to it as .bak-<time>.", &[("folder", &format!("{}/config", aio_remote::octane::gamedata()))]),
                tf("A backup of the database comes first ({folder}). If the import or the emulator's start fails, everything goes back.", &[("folder", &server_tools::backup_directory())]),
                t("The emulator is stopped during the import and started again: the players are disconnected for a moment.").into(),
                t("Meant for a new hotel: furniture players already own keeps its number, so on a hotel with a different furniture list it would turn into other furniture.").into(),
            ],
            footnote: Some(t("Run Install New Server first (MariaDB, the emulator and Octane). The furniture files and catalogue icons come from Deploy Assets.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the server").into(), t("Upload the catalogue").into(), t("Backup, import and FurnitureData.json").into()]
    }

    fn options(&self, ui: &mut egui::Ui, _palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        if form.has_catalog {
            ui.add_space(8.0);
            ui.checkbox(&mut form.confirmed, t("Yes, replace the catalogue and furniture list on the server (a backup is made first)"));
        }
    }

    fn validate(&self) -> Option<String> {
        let form = self.form.lock().unwrap();
        (form.has_catalog && !form.confirmed).then(|| t("Tick the box to confirm that the catalogue on the server may be replaced.").to_string())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = default_catalog::inspect(server.as_ref()).await?;
        let version = packed_version();
        self.form.lock().unwrap().has_catalog = state.pages > 0 || state.catalog_items > 0;
        ui.wake();
        if let Some(problem) = problem(&state) {
            ui.detected(ResultKind::Warning, t("The server is not ready for the catalogue yet"), &problem);
            return Ok(false);
        }
        if state.installed_version.as_deref() == Some(version.as_str()) {
            ui.detected(
                ResultKind::Success,
                &tf("The default catalogue ({version}) is installed", &[("version", &version)]),
                &tf(
                    "{pages} pages, {items} catalogue items, {furniture} furniture. You can skip this step, or install it again.",
                    &[("pages", &state.pages.to_string()), ("items", &state.catalog_items.to_string()), ("furniture", &state.furniture.to_string())],
                ),
            );
            return Ok(true);
        }
        let now = tf(
            "Now: {pages} pages, {items} catalogue items, {furniture} furniture.",
            &[("pages", &state.pages.to_string()), ("items", &state.catalog_items.to_string()), ("furniture", &state.furniture.to_string())],
        );
        ui.detected(
            ResultKind::Info,
            &tf("The default catalogue ({version}) will be installed", &[("version", &version)]),
            &format!("{now} {}", t("They are replaced; the database backup keeps them.")),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let catalog = match open_catalog() {
            Ok(catalog) => catalog,
            Err(problem) => fail!("The catalogue in the installer is damaged: {problem}"),
        };
        let state = default_catalog::inspect(server).await?;
        if let Some(problem) = problem(&state) {
            fail!("{problem}");
        }
        log(&format!(
            "  Database {}: {} pages, {} catalogue items, {} furniture",
            state.database.as_deref().unwrap_or("?"),
            state.pages,
            state.catalog_items,
            state.furniture
        ));
        ui.done(state.database.as_deref().unwrap_or(""));

        // Through files on this computer, because the upload sends files.
        let directory = server_tools::backup_directory();
        let stamp = aio_core::config_dir::timestamp();
        let local = tempfile_dir()?;
        let local_sql = local.join("Catalog_Export.sql");
        let local_json = local.join("FurnitureData.json");
        std::fs::write(&local_sql, &catalog.sql)?;
        std::fs::write(&local_json, &catalog.furniture_data)?;
        let remote_sql = format!("{directory}/catalog-{stamp}.sql");
        let remote_json = format!("{directory}/catalog-{stamp}.json");
        server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
        log(&format!("> Uploading Catalog_Export.sql ({}) and FurnitureData.json ({})", size(catalog.sql.len()), size(catalog.furniture_data.len())));
        let uploaded = async {
            server.upload(&local_sql, &remote_sql, &|_| {}).await?;
            server.upload(&local_json, &remote_json, &|_| {}).await
        }
        .await;
        let _ = std::fs::remove_dir_all(&local);
        uploaded?;
        ui.done(&size(catalog.sql.len() + catalog.furniture_data.len()));

        let result = default_catalog::install(server, &remote_sql, &remote_json, &catalog.version, &log).await?;
        ui.done(&tf("{pages} pages", &[("pages", &result.pages.to_string())]));
        {
            let mut output = self.output.lock().unwrap();
            output.result = Some(result.clone());
            output.version = catalog.version.clone();
        }
        Ok(ui.succeeded(
            &tf("The default catalogue ({version}) is installed", &[("version", &catalog.version)]),
            &tf(
                "{pages} pages, {items} catalogue items, {furniture} furniture. Players see it after they reload the hotel.",
                &[("pages", &result.pages.to_string()), ("items", &result.catalog_items.to_string()), ("furniture", &result.furniture.to_string())],
            ),
        ))
    }
}

/// A new folder for the two files in the system's temp folder.
fn tempfile_dir() -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("all-in-1-catalog-{}-{}", std::process::id(), aio_core::config_dir::timestamp()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn size(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(DefaultCatalogStep::new());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| super::finished::default_catalog(finish_step.result(), finish_step.version(), info.steps)),
        t("Catalogue installed"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packed_catalogue_is_complete() {
        let catalog = open_catalog().unwrap();
        assert_eq!(catalog.version, "2026-11-01");
        assert!(catalog.sql.len() > 1_000_000 && catalog.furniture_data.len() > 1_000_000);
        let sql = String::from_utf8_lossy(&catalog.sql);
        assert!(sql.contains("CREATE TABLE `items_base`") && sql.contains("CREATE TABLE `catalog_pages`"));
        // Nothing outside the catalogue: no database, user or grant statements.
        for line in sql.lines() {
            let upper = line.trim_start().to_uppercase();
            for forbidden in ["USE ", "CREATE DATABASE", "DROP DATABASE", "GRANT ", "CREATE USER", "DROP USER"] {
                assert!(!upper.starts_with(forbidden), "{line}");
            }
        }
    }
}
