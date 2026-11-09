//! Your own CMS becomes the hotel's website, as AtomCMS does: from a Git link or a ZIP; Octane moves to /client/.
//! (CustomCmsStep.cs, InstallerWindow._customCmsWizard)

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use aio_core::DatabaseCredentials;
use aio_remote::atom_cms::AtomCmsState;
use aio_remote::git_clone::{self, CloneTargetKind};
use aio_remote::web_access::{self, WebAccessSettings};
use aio_remote::{cms_site, custom_cms, fail, octane, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use super::atom_cms::{field_label, hint, move_aside, text_box};
use crate::gui::icons::Icon;
use crate::gui::step::{Extra, InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

struct Form {
    from_git: bool,
    git_url: String,
    zip_path: Option<PathBuf>,
    zip_label: String,
    picking: bool,
    name: String,
    php: bool,
    root: String,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            from_git: true,
            git_url: String::new(),
            zip_path: None,
            zip_label: String::new(),
            picking: false,
            name: String::new(),
            php: true,
            root: "/public".into(),
        }
    }
}

/// Set after a run, for the finished page.
#[derive(Default, Clone)]
pub struct CustomCmsResult {
    pub name: Option<String>,
    pub root: Option<String>,
    pub uses_php: bool,
}

pub struct CustomCmsStep {
    form: Arc<Mutex<Form>>,
    result: Mutex<CustomCmsResult>,
    runtime: tokio::runtime::Handle,
    ctx: egui::Context,
}

impl CustomCmsStep {
    pub fn new(runtime: tokio::runtime::Handle, ctx: egui::Context) -> Self {
        Self { form: Arc::default(), result: Mutex::default(), runtime, ctx }
    }

    pub fn result(&self) -> CustomCmsResult {
        self.result.lock().clone()
    }

    fn choose_zip(&self) {
        let (form, ctx) = (self.form.clone(), self.ctx.clone());
        form.lock().picking = true;
        self.runtime.spawn(async move {
            let file = rfd::AsyncFileDialog::new()
                .set_title(t("Select the ZIP file with your CMS"))
                .add_filter(t("ZIP files"), &["zip"])
                .add_filter(t("All files"), &["*"])
                .pick_file()
                .await;
            let mut form = form.lock();
            form.picking = false;
            if let Some(file) = file {
                let path = file.path().to_path_buf();
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                form.zip_label = zip_label(&path, bytes);
                form.zip_path = Some(path);
            }
            ctx.request_repaint();
        });
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

fn zip_label(path: &std::path::Path, bytes: u64) -> String {
    format!("{} ({} MB)", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), megabytes(bytes))
}

/// The root as the step keeps it: "/" when empty, a leading '/', no trailing one.
fn clean_root(root: &str) -> String {
    let mut root = root.trim().to_string();
    if root.is_empty() {
        root = "/".into();
    }
    if !root.starts_with('/') {
        root.insert(0, '/');
    }
    custom_cms::normalize_root(&root)
}

/// The percentage to log after `done` of `total` bytes, every 20 %; `reported` is the last one logged.
fn upload_step(done: u64, total: u64, reported: i64) -> Option<i64> {
    let percent = if total > 0 { (done as u128 * 100 / total as u128) as i64 } else { 0 };
    if percent < reported + 20 {
        None
    } else {
        Some(percent / 20 * 20)
    }
}

fn missing(state: &AtomCmsState, php: bool) -> Option<&'static str> {
    if state.site.is_none() {
        Some(t("Run the Web access step first: the CMS is set up for the address chosen there."))
    } else if php && state.php_version.is_none() {
        Some(t("PHP 8.5 is not installed: run the PHP step first (or choose a static website)."))
    } else if state.node_version.is_none() {
        Some(t("Node.js is missing: run the Dev tools step first (Octane is rebuilt for /client)."))
    } else if !state.emulator_configured {
        Some(t("The emulator is not set up: run the Emulator step first."))
    } else if !state.octane_built {
        Some(t("Octane is not built yet: run the Octane step first."))
    } else {
        None
    }
}

/// The database login for the user's CMS, under the result. (FinishedView.CmsDatabaseLogin)
pub(super) fn database_login_extras(credentials: Option<&DatabaseCredentials>, host: &str) -> Vec<Extra> {
    let mut extras = vec![Extra::Lines(vec![t("The installer cannot fill in the database settings of a CMS it does not know: put these in your CMS' settings (for example DB_HOST, DB_DATABASE, DB_USERNAME and DB_PASSWORD in its .env). Use the cms account the server installation made, not root.").into()])];
    let Some(c) = credentials else {
        extras.push(Extra::Lines(vec![tf(
            "The MariaDB passwords of {host} are not saved on this computer. They are in configuration/database/<server>/mariadb.txt on the computer that ran the MariaDB step.",
            &[("host", host)],
        )]));
        return extras;
    };
    extras.push(Extra::Lines(vec![
        format!("{}: 127.0.0.1", t("Host")),
        format!("{}: {}", t("Port"), c.port),
        format!("{}: {}", t("Database"), c.database),
        format!("{}: {}", t("User"), c.cms_user),
    ]));
    extras.push(Extra::Copy { label: t("Password").into(), value: c.cms_password.clone() });
    extras.push(Extra::Lines(vec![tf(
        "Saved on this computer in {path}.",
        &[("path", &aio_core::config_dir::database_credentials_path(host).display().to_string())],
    )]));
    extras
}

#[async_trait]
impl InstallStep for CustomCmsStep {
    fn title(&self) -> String {
        t("Your CMS").into()
    }

    fn caption(&self) -> String {
        t("CMS & /client").into()
    }

    fn install_label(&self) -> String {
        t("Install the CMS").into()
    }

    fn rerun_label(&self) -> String {
        t("Update and set up again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Puzzle,
            heading: t("Install your own CMS").into(),
            description: t("Your own CMS becomes the hotel's website, the same way as AtomCMS: visitors use the CMS, and the CMS opens Octane on /client.").into(),
            bullets: vec![
                tf("Gets your CMS: clones the GitHub link, or unpacks your ZIP, into {parent}/<folder name>.", &[("parent", custom_cms::PARENT)]),
                t("Octane moves to /client: vite --base=/client/, client-mode.json with /client/configuration/, its login screen off (the CMS logs in), and a rebuild.").into(),
                tf(
                    "NGINX serves the CMS from its root folder on its own port (the first free one from {first} to {last}), with PHP 8.5 or as a static website.",
                    &[("first", &cms_site::FIRST_PORT.to_string()), ("last", &cms_site::LAST_PORT.to_string())],
                ),
                t("Traefik sends the site to the CMS, /client and /gamedata to the hotel site, and only Polaris' own /api paths to the emulator: the CMS keeps the rest of /api.").into(),
                t("The database settings of your CMS are yours to fill in: the end of this step shows the database login to use.").into(),
            ],
            footnote: Some(t("Run the full server installation first (at least NGINX, PHP, MariaDB, the clones, Emulator, Web access and Octane). One CMS is the website at a time: this replaces AtomCMS as the site (its files stay).").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        [
            n("Check the server"),
            n("Get the CMS"),
            n("Check the root folder"),
            n("Octane on /client: configure and build"),
            n("NGINX (own port) and Traefik"),
            n("Test the CMS, /client and /api"),
        ]
        .iter()
        .map(|c| t(c).to_string())
        .collect()
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut choose = false;
        {
            let mut form = self.form.lock();
            let form = &mut *form;
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(2.0);
            field_label(ui, palette, t("Where does your CMS come from?"));
            ui.radio_value(&mut form.from_git, true, t("A GitHub link (the server clones it; later runs update it with git pull)"));
            ui.radio_value(&mut form.from_git, false, t("A ZIP file with the whole CMS, from this computer"));
            if form.from_git {
                let response = text_box(ui, &mut form.git_url, "https://github.com/owner/cms.git", 460.0, 500);
                // The repository's name is a good folder name.
                if response.lost_focus() && form.name.trim().is_empty() && !form.git_url.trim().is_empty() {
                    form.name = custom_cms::name_from_url(form.git_url.trim());
                }
                hint(ui, palette, t("The https link of the repository. It must be public: for a private one, download it as a ZIP and choose ZIP."));
            } else {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!form.picking, widgets::secondary_button(t("Choose ZIP ..."))).clicked() {
                        choose = true;
                    }
                    ui.add_space(4.0);
                    let label = if form.zip_path.is_some() { form.zip_label.as_str() } else { t("No ZIP file chosen yet.") };
                    ui.label(RichText::new(label).size(12.5).color(palette.muted));
                });
            }

            ui.add_space(4.0);
            field_label(ui, palette, t("Folder name on the server"));
            text_box(ui, &mut form.name, "mycms", 220.0, custom_cms::MAX_NAME_LENGTH);
            hint(
                ui,
                palette,
                &tf(
                    "Lower case letters and digits only (a-z, 0-9): no spaces, capitals or other characters, at most {max}.",
                    &[("max", &custom_cms::MAX_NAME_LENGTH.to_string())],
                ),
            );
            let name = form.name.trim();
            if !name.is_empty() {
                hint(ui, palette, &tf("The CMS goes to {directory}", &[("directory", &custom_cms::directory(name))]));
            }

            ui.add_space(4.0);
            field_label(ui, palette, t("Does the website use PHP?"));
            ui.radio_value(&mut form.php, true, t("PHP: the CMS has .php files (Laravel, WordPress, most retro CMSes)"));
            ui.radio_value(&mut form.php, false, t("Static: only HTML, CSS, JavaScript and images, no PHP"));

            ui.add_space(4.0);
            field_label(ui, palette, t("Root folder"));
            text_box(ui, &mut form.root, "/public", 220.0, 200);
            ui.scope(|ui| {
                ui.set_max_width(700.0);
                hint(ui, palette, t("The root is the folder inside your CMS that visitors' browsers get: the one with the start page (index.php or index.html). Everything outside it, such as the settings with passwords, stays hidden. Laravel-based CMSes (like AtomCMS) use /public. When the start page is in the top folder of your CMS, enter just /."));
            });
        }
        if choose {
            self.choose_zip();
        }
    }

    fn validate(&self) -> Option<String> {
        let mut form = self.form.lock();
        form.name = form.name.trim().to_string();
        if form.from_git {
            form.git_url = form.git_url.trim().to_string();
            if form.git_url.is_empty() {
                return Some(t("Enter the GitHub link of your CMS.").into());
            }
            if let Some(problem) = custom_cms::git_url_problem(&form.git_url) {
                return Some(t(&problem).into());
            }
        } else if !form.zip_path.as_ref().is_some_and(|p| p.is_file()) {
            return Some(t("Choose the ZIP file with your CMS.").into());
        }
        if let Some(problem) = custom_cms::name_problem(&form.name) {
            return Some(format!("{}{}", t("Folder name: "), t(&problem)));
        }
        form.root = clean_root(&form.root);
        custom_cms::root_problem(&form.root).map(|problem| format!("{}{}", t("Root folder: "), t(&problem)))
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = custom_cms::inspect(server.as_ref(), None).await?;
        let php = {
            let mut form = self.form.lock();
            // What is set up already fills the form in, for an update.
            if let Some(current) = &state.current_name {
                if form.name.trim().is_empty() {
                    form.name = current.clone();
                    if let Some(root) = &state.current_root {
                        form.root = root.clone();
                    }
                    if let Some(php) = state.current_php {
                        form.php = php;
                    }
                }
            }
            form.php
        };
        if let Some(missing) = missing(&state.server, php) {
            ui.detected(ResultKind::Warning, t("The server is not ready for a CMS yet"), missing);
            return Ok(false);
        }
        let site = state.site().cloned().unwrap_or_default();
        if let Some(name) = &state.current_name {
            ui.detected(
                ResultKind::Success,
                &tf("Your CMS ({directory}) is the website of {url}", &[("directory", &custom_cms::directory(name)), ("url", &site.url())]),
                &tf(
                    "Octane is on {client}. Run this step again to update it (git pull, or a new ZIP) and set it up again.",
                    &[("client", &site.client_url())],
                ),
            );
        } else {
            let current = if site.has_cms() {
                format!(
                    " {}",
                    tf("{cms} is the website now: your CMS takes its place (its files stay).", &[("cms", &site.cms_name().unwrap_or_default())])
                )
            } else {
                String::new()
            };
            ui.detected(
                ResultKind::Info,
                &tf("Your CMS will be the website of {url}", &[("url", &site.url())]),
                &format!("{}{current}", tf("Octane moves to {url}/client/.", &[("url", &site.url())])),
            );
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let (name, root, php, git, url, zip_path) = {
            let form = self.form.lock();
            (
                form.name.trim().to_string(),
                custom_cms::normalize_root(&form.root),
                form.php,
                form.from_git,
                form.git_url.trim().to_string(),
                form.zip_path.clone(),
            )
        };
        let directory = custom_cms::directory(&name);
        let log = ui.logger();
        let host = server.login().host.clone();
        let server = server.as_ref();

        log("> Checking the server ...");
        let state = custom_cms::inspect(server, Some(&name)).await?;
        if let Some(missing) = missing(&state.server, php) {
            fail!("{missing}");
        }
        let mut site = WebAccessSettings { atom_cms: false, custom_cms: Some(name.clone()), epic_next: false, ..state.site().cloned().unwrap_or_default() };
        let kind = if php { format!("PHP {}", state.server.php_version.as_deref().unwrap_or("")) } else { "static website".to_string() };
        log(&format!("  {}: {kind}, Octane built", site.url()));
        ui.done(&site.url());

        if git {
            custom_cms::check_git_access(server, &url, &log).await?;
            let clone = git_clone::inspect(server, &url, &directory).await?;
            if clone.kind == CloneTargetKind::OtherContent {
                let reason = match &clone.origin {
                    None => t("It is not a clone of that link.").to_string(),
                    Some(origin) => tf("It is a clone of {origin}.", &[("origin", origin)]),
                };
                if !move_aside(server, &ui, &directory, &reason, &move_description(&directory)).await? {
                    return Ok(ui.aborted(&host));
                }
            }
            let commit = if clone.kind == CloneTargetKind::SameRepository {
                git_clone::update(server, &directory, None, &log).await?
            } else {
                git_clone::clone(server, &url, &directory, None, &log).await?
            };
            ui.done(&commit);
        } else {
            let Some(zip_path) = zip_path else { fail!("{}", t("Choose the ZIP file with your CMS.")) };
            if state.folder_has_content
                && !move_aside(server, &ui, &directory, t("The ZIP is unpacked into an empty folder."), &move_description(&directory)).await?
            {
                return Ok(ui.aborted(&host));
            }
            let remote = format!("/root/cms-upload-{}.zip", chrono::Local::now().format("%Y%m%d-%H%M%S"));
            let bytes = std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
            let file_name = zip_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            log(&format!("> Uploading {file_name} ({} MB) to {remote}", megabytes(bytes)));
            let reported = AtomicI64::new(0);
            let progress = |done: u64| {
                if let Some(step) = upload_step(done, bytes, reported.load(Ordering::Relaxed)) {
                    reported.store(step, Ordering::Relaxed);
                    log(&format!("  {step} %"));
                }
            };
            server.upload(&zip_path, &remote, &progress).await?;
            custom_cms::extract_zip(server, &remote, &name, &log).await?;
            ui.done(&file_name);
        }

        custom_cms::prepare(server, &name, &root, php, &log).await?;
        ui.done(&custom_cms::root_path(&name, &root));

        // Octane is configured and built for /client/ before the site switches, so it is never served half-way.
        octane::configure(server, &site, &log, false).await?;
        octane::build(server, &site, &log).await?;
        ui.done(&site.client_url());

        let port = cms_site::apply(server, &site, &name, &custom_cms::root_path(&name, &root), php, &log, None).await?;
        site.cms_port = port;
        web_access::configure_routes(server, &site, &log).await?;
        octane::publish(server, &site, &log).await?;
        ui.done(&tf("CMS on / (port {port}), Octane on /client/", &[("port", &port.to_string())]));

        custom_cms::test(server, &site, &log).await?;
        ui.done("OK");

        *self.result.lock() = CustomCmsResult { name: Some(name), root: Some(root), uses_php: php };
        let credentials = aio_core::config_dir::load_database_credentials(&host);
        for extra in database_login_extras(credentials.as_ref(), &host) {
            ui.add_extra(extra);
        }
        Ok(ui.succeeded_with_warning(
            &tf("Your CMS is the website of {url}", &[("url", &site.url())]),
            &tf(
                "Fill in the database settings of your CMS yourself (its .env or config file): the installer cannot do that for a CMS it does not know. Use the cms database login below, which the server installation made. Octane is on {client}.",
                &[("client", &site.client_url())],
            ),
        ))
    }
}

fn move_description(directory: &str) -> String {
    tf(
        "{directory} is renamed to {directory}.bak-<date>; nothing is deleted (copy your settings, such as .env, back from there).",
        &[("directory", directory)],
    )
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(CustomCmsStep::new(shared.runtime.clone(), shared.ctx.clone()));
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step],
        false,
        Arc::new(move |info| {
            let result = finish_step.result();
            let credentials = aio_core::config_dir::load_database_credentials(&info.host);
            super::finished::custom_cms(info.site, result.name, result.root, result.uses_php, credentials, info.host, info.steps)
        }),
        t("Database settings"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_are_cleaned() {
        assert_eq!(clean_root(""), "/");
        assert_eq!(clean_root("  public/ "), "/public");
        assert_eq!(clean_root("/"), "/");
        assert_eq!(clean_root("/web/public/"), "/web/public");
    }

    #[test]
    fn upload_progress_every_twenty_percent() {
        assert_eq!(upload_step(10, 100, 0), None);
        assert_eq!(upload_step(25, 100, 0), Some(20));
        assert_eq!(upload_step(39, 100, 20), None);
        assert_eq!(upload_step(100, 100, 80), Some(100));
        assert_eq!(upload_step(5, 0, 0), None);
    }

    #[test]
    fn the_password_is_copyable() {
        let c = DatabaseCredentials {
            host: "203.0.113.5".into(),
            port: 3306,
            database: "hotel".into(),
            root_user: "root".into(),
            root_password: "r".into(),
            cms_user: "cms".into(),
            cms_password: "secret".into(),
            emulator_user: "emulator".into(),
            emulator_password: "e".into(),
        };
        let extras = database_login_extras(Some(&c), "203.0.113.5");
        assert!(extras.contains(&Extra::Copy { label: "Password".into(), value: "secret".into() }));
        assert_eq!(database_login_extras(None, "203.0.113.5").len(), 2);
    }
}
