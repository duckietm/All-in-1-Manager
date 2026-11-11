//! The Default catalogue addon: the catalogue that comes with the installer (assets/catalog/Full_Catalog.zip:
//! Catalog_Export.sql and FurnitureData.json) into the hotel database and gamedata/config.

use std::io::Read;
use std::sync::Arc;

use aio_remote::default_catalog::{self, CatalogResult};
use aio_remote::external_texts::{self, PreparedTexts};
use aio_remote::furniture_language::{self, FurniLanguage, SwitchResult};
use aio_remote::{fail, server_tools, Server};
use async_trait::async_trait;
use eframe::egui;
use parking_lot::Mutex;

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
/// Read once, from the first line of the SQL only: the page asks for it on every frame, and unpacking the whole
/// catalogue (22 MB) each time made the window crawl.
pub fn packed_version() -> String {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            let mut head = [0u8; 512];
            let read = zip::ZipArchive::new(std::io::Cursor::new(CATALOG_ZIP))
                .ok()
                .and_then(|mut archive| archive.by_name("Catalog_Export.sql").ok().and_then(|mut file| file.read(&mut head).ok()))
                .unwrap_or(0);
            default_catalog::version_of(&String::from_utf8_lossy(&head[..read])).unwrap_or_default()
        })
        .clone()
}

/// The translations of the catalogue's page texts (Dutch text -> text), per language, maintained with
/// source/tools/catalog_texts.py.
const PAGE_TEXTS: [(&str, &str); 8] = [
    ("en", include_str!("../../assets/catalog/texts/en.json")),
    ("es", include_str!("../../assets/catalog/texts/es.json")),
    ("pt", include_str!("../../assets/catalog/texts/pt.json")),
    ("de", include_str!("../../assets/catalog/texts/de.json")),
    ("fr", include_str!("../../assets/catalog/texts/fr.json")),
    ("it", include_str!("../../assets/catalog/texts/it.json")),
    ("fi", include_str!("../../assets/catalog/texts/fi.json")),
    ("tr", include_str!("../../assets/catalog/texts/tr.json")),
];

/// The page text dictionaries, read once.
pub fn page_dictionaries() -> &'static [(&'static str, aio_remote::catalog_texts::Dictionary)] {
    static DICTIONARIES: std::sync::OnceLock<Vec<(&'static str, aio_remote::catalog_texts::Dictionary)>> = std::sync::OnceLock::new();
    DICTIONARIES.get_or_init(|| PAGE_TEXTS.iter().map(|(code, json)| (*code, serde_json::from_str(json).unwrap_or_default())).collect())
}

/// The language the packed FurnitureData.json is in: chosen, it is used as it is.
pub const PACKED_LANGUAGE: &str = "nl";

struct Form {
    /// The catalogue on the server is replaced: asked when it has one.
    confirmed: bool,
    has_catalog: bool,
    /// The furniture language (furniture_language::LANGUAGES code).
    language: String,
    /// Also ExternalTexts.json in that language.
    texts: bool,
    /// Filled in where the catalogue's texts name the hotel (%hotel_name%); kept in Settings.
    hotel_name: String,
}

impl Default for Form {
    fn default() -> Self {
        let hotel_name = aio_core::InstallerSettings::load().hotel_name.unwrap_or_default();
        Self { confirmed: false, has_catalog: false, language: PACKED_LANGUAGE.into(), texts: false, hotel_name }
    }
}

#[derive(Default)]
struct Output {
    result: Option<CatalogResult>,
    version: String,
    language: Option<FurniLanguage>,
    switched: Option<SwitchResult>,
    /// ExternalTexts.json: (texts, of which the hotel's own).
    texts: Option<(usize, usize)>,
}

/// The name of a furniture language in its own language, as on the language button (Español, Suomi ...).
pub fn native_name(language: FurniLanguage) -> &'static str {
    crate::i18n::LANGUAGES.iter().find(|l| l.hotel_domain == language.domain).map(|l| l.native_name).unwrap_or(language.name)
}

/// The nine furniture languages as flag buttons; returns the one clicked.
pub(crate) fn language_choice(ui: &mut egui::Ui, palette: &Palette, current: &str) -> Option<&'static str> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        for language in furniture_language::LANGUAGES {
            let response = super::settings::language_button(ui, palette, native_name(language), language.domain, language.code == current)
                .on_hover_text(language.domain);
            if response.clicked() {
                clicked = Some(language.code);
            }
        }
    });
    clicked
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
        self.output.lock().result.clone()
    }

    pub fn version(&self) -> String {
        self.output.lock().version.clone()
    }

    pub fn language(&self) -> Option<FurniLanguage> {
        self.output.lock().language
    }

    pub fn texts(&self) -> Option<(usize, usize)> {
        self.output.lock().texts
    }

    pub fn switched(&self) -> Option<SwitchResult> {
        self.output.lock().switched.clone()
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
        vec![
            t("Check the server").into(),
            t("Upload the catalogue").into(),
            t("Stop the emulator").into(),
            t("Backup, import and FurnitureData.json").into(),
            t("Furniture language").into(),
            t("Start the emulator").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock();
        ui.add_space(8.0);
        ui.label(egui::RichText::new(t("Hotel name")).size(12.0).strong().color(palette.muted));
        ui.add(
            egui::TextEdit::singleline(&mut form.hotel_name)
                .hint_text(egui::RichText::new(t("For example Habbo")).italics())
                .desired_width(260.0)
                .char_limit(32),
        );
        let name = form.hotel_name.trim();
        let hint = match (name.is_empty(), default_catalog::hotel_name_problem(name)) {
            (false, Some(problem)) => egui::RichText::new(t(problem)).size(12.0).color(palette.warning_text),
            _ => egui::RichText::new(tf(
                "Where the catalogue names the hotel: \"De {name} Badgewinkel\", \"Huisdieren zijn inwoners van {name}\" and the like, in every language.",
                &[("name", if name.is_empty() { "…" } else { name })],
            ))
            .size(12.0)
            .color(palette.subtle),
        };
        ui.add(egui::Label::new(hint).wrap());
        ui.add_space(8.0);
        ui.label(egui::RichText::new(t("Language of the furniture names and descriptions")).size(12.0).strong().color(palette.muted));
        if let Some(code) = language_choice(ui, palette, &form.language) {
            form.language = code.into();
        }
        let hint = if form.language == PACKED_LANGUAGE {
            t("The catalogue's own FurnitureData.json (Dutch), as it is.").to_string()
        } else {
            let language = furniture_language::language(&form.language).unwrap_or(furniture_language::LANGUAGES[0]);
            tf(
                "Downloaded from {domain} on this computer; the database is brought in line with it (sprite ids and offer ids). Furni the hotel has but {domain} does not keep their Dutch names.",
                &[("domain", language.domain)],
            )
        };
        ui.add(egui::Label::new(egui::RichText::new(hint).size(12.0).color(palette.subtle)).wrap());
        let language = furniture_language::language(&form.language).unwrap_or(furniture_language::LANGUAGES[0]);
        ui.add_space(4.0);
        ui.checkbox(&mut form.texts, tf("Also ExternalTexts.json (the client's texts) in {language}", &[("language", native_name(language))]));
        if form.texts {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(tf(
                        "Downloaded from {domain}; texts the hotel has but {domain} does not (custom badges and the like) stay.",
                        &[("domain", language.domain)],
                    ))
                    .size(12.0)
                    .color(palette.subtle),
                )
                .wrap(),
            );
        }
        if form.has_catalog {
            ui.add_space(8.0);
            ui.checkbox(&mut form.confirmed, t("Yes, replace the catalogue and furniture list on the server (a backup is made first)"));
        }
    }

    fn validate(&self) -> Option<String> {
        let form = self.form.lock();
        if let Some(problem) = default_catalog::hotel_name_problem(&form.hotel_name) {
            return Some(t(problem).to_string());
        }
        (form.has_catalog && !form.confirmed).then(|| t("Tick the box to confirm that the catalogue on the server may be replaced.").to_string())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = default_catalog::inspect(server.as_ref()).await?;
        let version = packed_version();
        self.form.lock().has_catalog = state.pages > 0 || state.catalog_items > 0;
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
        let mut catalog = match open_catalog() {
            Ok(catalog) => catalog,
            Err(problem) => fail!("The catalogue in the installer is damaged: {problem}"),
        };
        // The hotel's name into the texts, and kept for next time.
        let hotel_name = self.form.lock().hotel_name.trim().to_string();
        if let Some(problem) = default_catalog::hotel_name_problem(&hotel_name) {
            fail!("{}", t(problem));
        }
        catalog.sql = default_catalog::with_hotel_name(&String::from_utf8_lossy(&catalog.sql), &hotel_name).into_bytes();
        let mut settings = aio_core::InstallerSettings::load();
        settings.hotel_name = Some(hotel_name.clone());
        let _ = settings.save();
        log(&format!("  Hotel name in the catalogue texts: {hotel_name}"));
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

        // Another furniture language is downloaded first, while the hotel still runs: a failed download changes nothing.
        let code = self.form.lock().language.clone();
        let language = furniture_language::language(&code).unwrap_or(furniture_language::LANGUAGES[0]);
        let prepared = if language.code == PACKED_LANGUAGE {
            None
        } else {
            let packed: serde_json::Value = serde_json::from_slice(strip_bom(&catalog.furniture_data))?;
            Some(furniture_language::prepare(language, Some(&packed), &log).await?)
        };
        let texts: Option<PreparedTexts> =
            if self.form.lock().texts { Some(external_texts::prepare(server, language, &log).await?) } else { None };

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

        // One stop of the emulator for everything below, and one start at the end.
        let stopped = server_tools::stop_emulator(server, &log).await?;
        ui.done(if stopped { t("stopped") } else { t("not installed") });
        let work = async {
            let result = default_catalog::install(server, &remote_sql, &remote_json, &catalog.version, false, &log).await?;
            ui.done(&tf("{pages} pages", &[("pages", &result.pages.to_string())]));
            {
                let mut output = self.output.lock();
                output.result = Some(result.clone());
                output.version = catalog.version.clone();
                output.language = Some(language);
            }
            match prepared {
                None => {
                    furniture_language::mark(server, language).await?;
                    log(&format!("> Furniture language: {} (the catalogue's own FurnitureData.json)", language.name));
                }
                Some(prepared) => {
                    let switched = furniture_language::finish(server, prepared, false, &log).await?;
                    self.output.lock().switched = Some(switched);
                }
            }
            // The page texts were imported in Dutch.
            if language.code != PACKED_LANGUAGE {
                // The translations name the hotel as the texts do: the same name goes into both.
                let dictionaries: Vec<(&str, aio_remote::catalog_texts::Dictionary)> = page_dictionaries()
                    .iter()
                    .map(|(code, dictionary)| {
                        let filled = dictionary
                            .iter()
                            .map(|(dutch, text)| (default_catalog::with_hotel_name(dutch, &hotel_name), default_catalog::with_hotel_name(text, &hotel_name)))
                            .collect();
                        (*code, filled)
                    })
                    .collect();
                aio_remote::catalog_texts::translate(server, &dictionaries, language.code, language.name, &log).await?;
            }
            // The emulator writes to ExternalTexts.json too, so it goes in while the emulator is stopped.
            let old_texts = match &texts {
                Some(texts) => {
                    let old = external_texts::put(server, texts, &log).await?;
                    self.output.lock().texts = Some((texts.count(), texts.kept));
                    Some(old)
                }
                None => None,
            };
            ui.done(native_name(language));
            anyhow::Ok((result, old_texts))
        }
        .await;
        // Whatever happened above, the hotel runs again.
        let (result, old_texts) = match work {
            Ok(done) => done,
            Err(error) => {
                if stopped {
                    if let Err(start) = server_tools::start_emulator(server, &log).await {
                        log(&format!("! {start}"));
                    }
                }
                return Err(error);
            }
        };
        if stopped {
            if let Err(error) = server_tools::start_emulator(server, &log).await {
                // It does not load with the new catalogue: the database and FurnitureData.json go back to before it.
                log("> The emulator does not start with the new catalogue");
                default_catalog::restore(server, &result.safety_backup, result.old_furniture_data.as_deref(), &log).await?;
                if let Some(old) = &old_texts {
                    external_texts::restore(server, old.as_deref(), &log).await?;
                }
                server_tools::start_emulator(server, &log).await?;
                fail!("The emulator does not start with the new catalogue ({error}). The database and FurnitureData.json are as they were before.");
            }
        }
        ui.done(if stopped { t("loaded") } else { t("not installed") });
        Ok(ui.succeeded(
            &tf("The default catalogue ({version}) is installed", &[("version", &catalog.version)]),
            &tf(
                "{pages} pages, {items} catalogue items, {furniture} furniture. Players see it after they reload the hotel.",
                &[("pages", &result.pages.to_string()), ("items", &result.catalog_items.to_string()), ("furniture", &result.furniture.to_string())],
            ),
        ))
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
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
        Arc::new(move |info| {
            super::finished::default_catalog(
                finish_step.result(),
                finish_step.version(),
                finish_step.language(),
                finish_step.switched(),
                finish_step.texts(),
                info.steps,
            )
        }),
        t("Catalogue installed"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_text_translations_fit_their_originals() {
        let keys: Vec<String> = serde_json::from_str(include_str!("../../assets/catalog/texts/keys.json")).unwrap();
        let keys: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
        for (code, json) in PAGE_TEXTS {
            let dictionary: std::collections::HashMap<String, String> = serde_json::from_str(json).unwrap_or_else(|e| panic!("{code}.json: {e}"));
            for (original, translated) in &dictionary {
                assert!(keys.contains(original.as_str()), "{code}.json: {original:?} is not in the catalogue (run source/tools/catalog_texts.py)");
                assert_eq!(original.matches('\n').count(), translated.matches('\n').count(), "{code}.json: line breaks of {original:?}");
                assert_eq!(original.matches('<').count(), translated.matches('<').count(), "{code}.json: HTML tags of {original:?}");
            }
        }
        assert_eq!(page_dictionaries().len(), 8);
    }

    #[test]
    fn the_version_is_quick() {
        let started = std::time::Instant::now();
        assert_eq!(packed_version(), "2026-11-05");
        // Asked on every frame: after the first time it must cost nothing.
        let again = std::time::Instant::now();
        for _ in 0..1000 {
            assert_eq!(packed_version(), "2026-11-05");
        }
        assert!(again.elapsed() < std::time::Duration::from_millis(50), "{:?}", again.elapsed());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn the_packed_catalogue_is_complete() {
        let catalog = open_catalog().unwrap();
        assert_eq!(catalog.version, "2026-11-05");
        assert!(catalog.sql.len() > 1_000_000 && catalog.furniture_data.len() > 1_000_000);
        let sql = String::from_utf8_lossy(&catalog.sql);
        assert!(sql.contains("CREATE TABLE `items_base`") && sql.contains("CREATE TABLE `catalog_pages`"));
        // The hotel's name is filled in at install time: no hotel of its own in the texts.
        assert!(sql.contains(default_catalog::HOTEL_NAME) && !sql.contains("Camwijs"));
        for (code, dictionary) in page_dictionaries() {
            for (dutch, text) in dictionary {
                assert_eq!(dutch.matches(default_catalog::HOTEL_NAME).count(), text.matches(default_catalog::HOTEL_NAME).count(), "{code}.json: {dutch:?}");
            }
        }
        // Nothing outside the catalogue: no database, user or grant statements.
        for line in sql.lines() {
            let upper = line.trim_start().to_uppercase();
            for forbidden in ["USE ", "CREATE DATABASE", "DROP DATABASE", "GRANT ", "CREATE USER", "DROP USER"] {
                assert!(!upper.starts_with(forbidden), "{line}");
            }
        }
    }
}
