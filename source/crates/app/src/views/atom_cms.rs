//! AtomCMS becomes the hotel's website; Octane moves to /client/. (AtomCmsStep.cs, InstallerWindow._atomWizard)
//!
//! Also holds what the CMS steps share (the option fields, the default hotel name, "Folder not empty").

use std::sync::Arc;

use aio_remote::atom_cms::{self, AtomCmsState};
use aio_remote::git_clone::{self, CloneTargetKind};
use aio_remote::web_access::{self, WebAccessSettings};
use aio_remote::{cms_site, fail, harden, octane, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::dialogs::Choice;
use crate::gui::icons::Icon;
use crate::gui::step::{Extra, InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistingFolderChoice {
    MoveAside,
    Abort,
}

/// A field label of the step options: 12 px, muted, semi-bold.
pub(super) fn field_label(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(palette.muted).strong());
}

/// A hint under a field: 12 px, subtle, wrapping.
pub(super) fn hint(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(palette.subtle)).wrap());
}

/// A single-line text box; returns its response (for lost focus).
pub(super) fn text_box(ui: &mut egui::Ui, value: &mut String, placeholder: &str, width: f32, max: usize) -> egui::Response {
    ui.add(egui::TextEdit::singleline(value).hint_text(placeholder).desired_width(width).char_limit(max))
}

/// The hotel's name checked as AtomCMS and EpicNext want it (trimmed in place).
pub(super) fn hotel_name_problem(name: &mut String) -> Option<String> {
    *name = name.trim().to_string();
    if name.is_empty() {
        return Some(t("Enter the hotel's name.").into());
    }
    if name.chars().any(char::is_control) {
        return Some(t("The hotel's name cannot contain line breaks or tabs.").into());
    }
    None
}

/// "Example Hotel" for www.example.com; empty for an IP address.
pub(super) fn hotel_name_from_site(site: Option<&WebAccessSettings>) -> String {
    let Some(site) = site.filter(|s| !s.is_ip_address()) else { return String::new() };
    let host = harden::whitelist_host(site);
    let first = host.split('.').next().unwrap_or("");
    let mut chars = first.chars();
    match chars.next() {
        Some(c) => format!("{}{} Hotel", c.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// "Folder not empty": moves the folder aside, or false to abort. (MoveAsideAsync of the CMS steps)
pub(super) async fn move_aside(server: &dyn Server, ui: &StepUi, directory: &str, reason: &str, move_description: &str) -> anyhow::Result<bool> {
    let choice = ui
        .choose(
            t("Folder not empty"),
            &tf("{directory} already has other content", &[("directory", directory)]),
            reason,
            ExistingFolderChoice::Abort,
            vec![
                Choice::new(t("Move it aside and continue"), move_description, ExistingFolderChoice::MoveAside).primary(),
                Choice::new(t("Abort installation"), t("Changes nothing on the server."), ExistingFolderChoice::Abort),
            ],
        )
        .await;
    if choice == ExistingFolderChoice::Abort {
        return Ok(false);
    }
    let log = ui.logger();
    git_clone::move_aside(server, directory, &log).await?;
    Ok(true)
}

#[derive(Default)]
struct Form {
    hotel_name: String,
}

/// Set after a run (or found by detect), for the finished page.
#[derive(Default, Clone)]
pub struct AtomResult {
    pub installation_key: Option<String>,
    pub installation_completed: bool,
}

#[derive(Default)]
pub struct AtomCmsStep {
    form: Mutex<Form>,
    result: Mutex<AtomResult>,
}

impl AtomCmsStep {
    pub fn result(&self) -> AtomResult {
        self.result.lock().clone()
    }
}

fn missing(state: &AtomCmsState) -> Option<&'static str> {
    if state.site.is_none() {
        Some(t("Run the Web access step first: AtomCMS is set up for the address chosen there."))
    } else if state.php_version.is_none() {
        Some(t("PHP 8.5 is not installed: run the PHP step first."))
    } else if !state.composer_present || state.node_version.is_none() {
        Some(t("Composer or Node.js is missing: run the Dev tools step first."))
    } else if !state.emulator_configured {
        Some(t("The emulator is not set up: run the Emulator step first (AtomCMS uses its database)."))
    } else if !state.octane_built {
        Some(t("Octane is not built yet: run the Octane step first."))
    } else {
        None
    }
}

#[async_trait]
impl InstallStep for AtomCmsStep {
    fn title(&self) -> String {
        "AtomCMS".into()
    }

    fn caption(&self) -> String {
        t("CMS & /client").into()
    }

    fn install_label(&self) -> String {
        t("Install AtomCMS").into()
    }

    fn rerun_label(&self) -> String {
        t("Update and rebuild").into()
    }

    fn intro(&self) -> Intro {
        let directory = atom_cms::directory();
        Intro {
            icon: Icon::Puzzle,
            heading: t("Install AtomCMS").into(),
            description: t("AtomCMS (github.com/ObjectRetros/atomcms) becomes the hotel's website: visitors register and log in there, and the CMS opens Octane on /client.").into(),
            bullets: vec![
                tf("Clones AtomCMS to {directory}, writes .env (address, database login cms, https) and runs composer install.", &[("directory", &directory)]),
                t("php artisan key:generate, storage:link and migrate --seed: AtomCMS' tables go into the hotel database next to the emulator's.").into(),
                t("Sets the CMS' settings for this server: theme atom, nitro_path /client, the badge, group badge and furniture icon paths in /gamedata.").into(),
                t("Builds the atom theme and the housekeeping (npm ci, npm run build:atom).").into(),
                t("Octane moves to /client: vite --base=/client/ in package.json, client-mode.json with /client/configuration/, its login screen off (the CMS logs in), and a rebuild.").into(),
                tf(
                    "NGINX serves {public} with PHP 8.5 on its own port (the first free one from {first} to {last}); the hotel site keeps Octane on /client/. Traefik sends only Polaris' own /api paths to the emulator, AtomCMS keeps the rest of /api.",
                    &[("public", &atom_cms::public()), ("first", &cms_site::FIRST_PORT.to_string()), ("last", &cms_site::LAST_PORT.to_string())],
                ),
            ],
            footnote: Some(t("Run the full server installation first (at least NGINX, PHP, MariaDB, Dev tools, the clones, Emulator, Web access and Octane), from this computer: the database login comes from configuration/database/<server>/mariadb.txt.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        [
            n("Check the server"),
            n("Clone or update AtomCMS"),
            n(".env"),
            n("composer install"),
            n("Database: migrate and settings"),
            n("Theme: npm run build:atom"),
            n("Octane on /client: configure and build"),
            n("NGINX and Traefik"),
            n("Test the CMS, /client and /api"),
            n("Installation key for the web installer"),
        ]
        .iter()
        .map(|c| t(c).to_string())
        .collect()
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock();
        ui.add_space(2.0);
        field_label(ui, palette, t("The hotel's name (APP_NAME and the CMS' hotel_name)"));
        text_box(ui, &mut form.hotel_name, "My Hotel", 380.0, 100);
    }

    fn validate(&self) -> Option<String> {
        hotel_name_problem(&mut self.form.lock().hotel_name)
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = atom_cms::inspect(server.as_ref()).await?;
        {
            let mut form = self.form.lock();
            if form.hotel_name.trim().is_empty() {
                form.hotel_name = match &state.hotel_name {
                    Some(name) if !name.is_empty() && name != "Atom CMS" => name.clone(),
                    _ => hotel_name_from_site(state.site.as_ref()),
                };
            }
        }
        if let Some(missing) = missing(&state) {
            ui.detected(ResultKind::Warning, t("The server is not ready for AtomCMS yet"), missing);
            return Ok(false);
        }
        let site = state.site.clone().unwrap_or_default();
        if state.is_complete() {
            *self.result.lock() =
                AtomResult { installation_key: state.installation_key.clone(), installation_completed: state.installation_completed };
            let installer = if state.installation_completed {
                t("The AtomCMS web installer is done. ").to_string()
            } else if let Some(key) = &state.installation_key {
                tf("Finish AtomCMS' web installer on {url} with key {key}. ", &[("url", &site.url()), ("key", key)])
            } else {
                t("The installation key for AtomCMS' web installer is made when this step runs. ").to_string()
            };
            ui.detected(
                ResultKind::Success,
                &tf("AtomCMS is the website of {url}", &[("url", &site.url())]),
                &format!(
                    "{}{installer}{}",
                    tf("Octane is on {url}. ", &[("url", &site.client_url())]),
                    t("You can skip this step, or update AtomCMS (git pull, composer, migrations) and rebuild.")
                ),
            );
            return Ok(true);
        }
        ui.detected(
            ResultKind::Info,
            &tf("AtomCMS will be installed for {url}", &[("url", &site.url())]),
            &tf(
                "The CMS on {url}, Octane on {url}/client/ (PHP {php}, Node.js {node}).",
                &[
                    ("url", &site.url()),
                    ("php", state.php_version.as_deref().unwrap_or("")),
                    ("node", state.node_version.as_deref().unwrap_or("")),
                ],
            ),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let hotel_name = self.form.lock().hotel_name.trim().to_string();
        let log = ui.logger();
        let server = server.as_ref();
        let directory = atom_cms::directory();

        log("> Checking the server ...");
        let state = atom_cms::inspect(server).await?;
        if let Some(missing) = missing(&state) {
            fail!("{missing}");
        }
        let mut site = WebAccessSettings { atom_cms: true, custom_cms: None, epic_next: false, ..state.site.clone().unwrap_or_default() };
        log(&format!(
            "  {}: PHP {}, Node.js {}, Octane built",
            site.url(),
            state.php_version.as_deref().unwrap_or(""),
            state.node_version.as_deref().unwrap_or("")
        ));
        ui.done(&site.url());

        let clone = git_clone::inspect(server, atom_cms::REPOSITORY, &directory).await?;
        if clone.kind == CloneTargetKind::OtherContent {
            let reason = match &clone.origin {
                None => t("It is not a clone of AtomCMS.").to_string(),
                Some(origin) => tf("It is a clone of {origin}, not of ObjectRetros/atomcms.", &[("origin", origin)]),
            };
            let choice = ui
                .choose(
                    t("Folder not empty"),
                    &tf("{directory} already has other content", &[("directory", &directory)]),
                    &reason,
                    ExistingFolderChoice::Abort,
                    vec![
                        Choice::new(
                            t("Move it aside and clone"),
                            tf("{directory} is renamed to {directory}.bak-<date>; nothing is deleted.", &[("directory", &directory)]),
                            ExistingFolderChoice::MoveAside,
                        )
                        .primary(),
                        Choice::new(t("Abort installation"), t("Changes nothing on the server and disconnects."), ExistingFolderChoice::Abort),
                    ],
                )
                .await;
            if choice == ExistingFolderChoice::Abort {
                return Ok(ui.aborted(&server.login().host));
            }
            git_clone::move_aside(server, &directory, &log).await?;
        }
        let commit = if clone.kind == CloneTargetKind::SameRepository {
            git_clone::update(server, &directory, None, &log).await?
        } else {
            git_clone::clone(server, atom_cms::REPOSITORY, &directory, None, &log).await?
        };
        ui.done(&commit);

        let host = server.login().host.clone();
        let credentials = aio_core::config_dir::load_database_credentials(&host);
        if credentials.is_none() {
            log(&format!("  No saved MariaDB passwords for {host}: the database login already in .env is kept."));
        }
        atom_cms::configure_env(server, &site, &hotel_name, credentials.as_ref(), &log).await?;
        ui.done(&site.url());

        atom_cms::composer_install(server, &log).await?;
        ui.done("vendor/");

        let first = atom_cms::migrate(server, &hotel_name, &log).await?;
        ui.done(if first { t("installed and seeded") } else { t("up to date") });

        atom_cms::build_theme(server, &log).await?;
        ui.done("public/build");

        // Octane is configured and built for /client/ before the site switches, so it is never served half-way.
        octane::configure(server, &site, &log, false).await?;
        octane::build(server, &site, &log).await?;
        ui.done(&site.client_url());

        // AtomCMS gets its own NGINX site on a free port (85-95); the hotel site keeps Octane on /client/ and the assets.
        let port = cms_site::apply(server, &site, atom_cms::SITE_NAME, &atom_cms::public(), true, &log, None).await?;
        site.custom_cms = None;
        site.cms_port = port;
        web_access::configure_routes(server, &site, &log).await?;
        octane::publish(server, &site, &log).await?;
        ui.done(&tf("CMS on / (port {port}), Octane on /client/", &[("port", &port.to_string())]));

        atom_cms::test(server, &site, &log).await?;
        ui.done("OK");

        let (key, completed) = atom_cms::installation_key(server, &site, &log).await?;
        *self.result.lock() = AtomResult { installation_key: key.clone(), installation_completed: completed };
        ui.done(if completed { t("web installer done") } else { key.as_deref().unwrap_or("") });
        if completed {
            return Ok(ui.succeeded(
                &tf("AtomCMS is the website of {url}", &[("url", &site.url())]),
                &tf("Octane is on {client}; the CMS opens it after logging in. Update later by running this step again.", &[("client", &site.client_url())]),
            ));
        }
        ui.add_extra(Extra::Copy { label: t("Installation key").into(), value: key.unwrap_or_default() });
        Ok(ui.succeeded_with_warning(
            &tf("AtomCMS is installed on {url}", &[("url", &site.url())]),
            &tf(
                "Copy the installation key below, open {url} and paste it in AtomCMS' web installer. The paths are already filled in.",
                &[("url", &site.url())],
            ),
        ))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(AtomCmsStep::default());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step],
        false,
        Arc::new(move |info| {
            let result = finish_step.result();
            super::finished::atom_cms(info.site, result.installation_key, result.installation_completed, info.steps)
        }),
        t("Web installer"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotel_name_from_the_address() {
        let site = WebAccessSettings::new("www.example.com", true, false);
        assert_eq!(hotel_name_from_site(Some(&site)), "Example Hotel");
        let site = WebAccessSettings::new("habbo.nl", true, false);
        assert_eq!(hotel_name_from_site(Some(&site)), "Habbo Hotel");
        let ip = WebAccessSettings::new("203.0.113.5", false, false);
        assert_eq!(hotel_name_from_site(Some(&ip)), "");
        assert_eq!(hotel_name_from_site(None), "");
    }

    #[test]
    fn hotel_name_checks() {
        let mut name = "  My Hotel ".to_string();
        assert_eq!(hotel_name_problem(&mut name), None);
        assert_eq!(name, "My Hotel");
        assert!(hotel_name_problem(&mut "   ".to_string()).is_some());
        assert!(hotel_name_problem(&mut "a\tb".to_string()).is_some());
    }
}
