//! EpicNext CMS becomes the hotel's website, as AtomCMS does: Next.js on the server itself (not Docker), Octane on
//! /client/. (EpicNextStep.cs, InstallerWindow._epicNextWizard)

use std::sync::{Arc, Mutex};

use aio_remote::atom_cms::AtomCmsState;
use aio_remote::git_clone::{self, CloneTargetKind};
use aio_remote::web_access::{self, WebAccessSettings};
use aio_remote::{cms_site, custom_cms, epicnext, fail, octane, Server};
use async_trait::async_trait;
use eframe::egui;

use super::atom_cms::{field_label, hint, hotel_name_from_site, hotel_name_problem, move_aside, text_box};
use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

struct Form {
    git_url: String,
    branch: String,
    folder: String,
    hotel_name: String,
}

impl Default for Form {
    fn default() -> Self {
        Self { git_url: epicnext::REPOSITORY.into(), branch: String::new(), folder: epicnext::REPOSITORY_FOLDER.into(), hotel_name: String::new() }
    }
}

#[derive(Default)]
pub struct EpicNextStep {
    form: Mutex<Form>,
    /// Set after a run, for the finished page.
    atom_tables: Mutex<bool>,
}

impl EpicNextStep {
    pub fn atom_tables(&self) -> bool {
        *self.atom_tables.lock().unwrap()
    }
}

/// The folder inside the repository as the step keeps it: trimmed, without leading or trailing '/'.
fn clean_folder(folder: &str) -> String {
    folder.trim().trim_matches('/').to_string()
}

fn missing(state: &AtomCmsState) -> Option<&'static str> {
    if state.site.is_none() {
        Some(t("Run the Web access step first: EpicNext is set up for the address chosen there."))
    } else if state.node_version.is_none() {
        Some(t("Node.js is missing: run the Dev tools step first."))
    } else if !state.emulator_configured {
        Some(t("The emulator is not set up: run the Emulator step first."))
    } else if !state.octane_built {
        Some(t("Octane is not built yet: run the Octane step first."))
    } else {
        None
    }
}

#[async_trait]
impl InstallStep for EpicNextStep {
    fn title(&self) -> String {
        t("EpicNext CMS").into()
    }

    fn caption(&self) -> String {
        t("CMS & /client").into()
    }

    fn install_label(&self) -> String {
        t("Install EpicNext").into()
    }

    fn rerun_label(&self) -> String {
        t("Update and build again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Puzzle,
            heading: t("Install EpicNext CMS").into(),
            description: t("EpicNext CMS (Next.js) becomes the hotel's website, the same way as AtomCMS: visitors register and log in there, and it opens Octane on /client. It runs on the server itself, not in Docker.").into(),
            bullets: vec![
                tf(
                    "Clones EpicNext into {directory}, by default from its own repository ({repository}); a folder of another repository works too. Later runs update it.",
                    &[("directory", epicnext::directory()), ("repository", epicnext::REPOSITORY)],
                ),
                tf(
                    "Node.js {node} or newer and pnpm (through corepack), then pnpm install, pnpm db:migrate (only EpicNext's own tables) and pnpm build, under its memory cap.",
                    &[("node", epicnext::NODE_MINIMUM)],
                ),
                tf(
                    ".env: the hotel database with the cms login of the server installation, the site's address, a random AUTH_SECRET and RCON. Settings: the client URL (/client/index.html, Octane on this site) and the gamedata folder ({gamedata}). users.password is widened for EpicNext's password hashes.",
                    &[("gamedata", &octane::gamedata())],
                ),
                tf(
                    "Runs as the systemd service {service} (user www-data, {upstream}, started at boot). No PHP.",
                    &[("service", epicnext::SERVICE), ("upstream", epicnext::UPSTREAM)],
                ),
                tf(
                    "NGINX passes the site to it on its own port (the first free one from {first} to {last}); Octane moves to /client/ with its login screen off; Traefik sends only Polaris' own /api paths to the emulator.",
                    &[("first", &cms_site::FIRST_PORT.to_string()), ("last", &cms_site::LAST_PORT.to_string())],
                ),
            ],
            footnote: Some(t("Run the full server installation first (at least NGINX, MariaDB, Dev tools, the clones, Emulator, Web access and Octane). The build needs about 3 GB of free memory and 5 GB of free disk space. One CMS is the website at a time: this replaces AtomCMS or your own CMS as the site (their files stay).").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        [
            n("Check the server"),
            n("Get EpicNext"),
            n("Node.js, pnpm and .env"),
            n("pnpm install, migrate and build"),
            n("Octane on /client: configure and build"),
            n("Start EpicNext, NGINX (own port) and Traefik"),
            n("Test the CMS, /client and /api"),
        ]
        .iter()
        .map(|c| t(c).to_string())
        .collect()
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.add_space(2.0);
        field_label(ui, palette, t("Hotel name"));
        text_box(ui, &mut form.hotel_name, "My Hotel", 380.0, 100);
        field_label(ui, palette, t("Repository"));
        text_box(ui, &mut form.git_url, epicnext::REPOSITORY, 460.0, 500);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                field_label(ui, palette, t("Branch"));
                text_box(ui, &mut form.branch, t("default branch"), 220.0, 100);
            });
            ui.add_space(4.0);
            ui.vertical(|ui| {
                field_label(ui, palette, t("Folder in the repository"));
                text_box(ui, &mut form.folder, t("top folder"), 220.0, 200);
            });
        });
        hint(ui, palette, t("The https link of the repository (it must be public), the branch (empty: its default branch) and the folder with EpicNext's package.json (empty: the top folder). By default: EpicNext's own repository, its top folder."));
    }

    fn validate(&self) -> Option<String> {
        let mut form = self.form.lock().unwrap();
        if let Some(problem) = hotel_name_problem(&mut form.hotel_name) {
            return Some(problem);
        }
        form.git_url = form.git_url.trim().to_string();
        if let Some(problem) = custom_cms::git_url_problem(&form.git_url) {
            return Some(t(&problem).into());
        }
        form.branch = form.branch.trim().to_string();
        if let Some(problem) = epicnext::branch_problem(&form.branch) {
            return Some(format!("{}{}", t("Branch: "), t(&problem)));
        }
        form.folder = clean_folder(&form.folder);
        epicnext::folder_problem(&form.folder).map(|problem| format!("{}{}", t("Folder: "), t(&problem)))
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = epicnext::inspect(server.as_ref()).await?;
        {
            let mut form = self.form.lock().unwrap();
            if form.hotel_name.trim().is_empty() {
                form.hotel_name = match (&state.hotel_name, &state.server.hotel_name) {
                    (Some(name), _) if !name.is_empty() => name.clone(),
                    (_, Some(atom)) if !atom.is_empty() && atom != "Atom CMS" => atom.clone(),
                    _ => hotel_name_from_site(state.site()),
                };
            }
            if let Some(origin) = state.origin.as_ref().filter(|o| !o.is_empty()) {
                // What the last run used: a folder of a repository, or a clone of a repository of its own.
                form.git_url = origin.clone();
                form.branch = state.source_branch.clone().unwrap_or_default();
                form.folder = state.source_folder.clone().unwrap_or_default();
            }
        }
        if let Some(missing) = missing(&state.server) {
            ui.detected(ResultKind::Warning, t("The server is not ready for EpicNext yet"), missing);
            return Ok(false);
        }
        let current = state.site().cloned().unwrap_or_default();
        let atom = if state.atom_tables {
            format!(" {}", t("The hotel database has AtomCMS' tables: EpicNext also uses website_* tables, so tables with the same name keep AtomCMS' layout. EpicNext works best on a database without AtomCMS."))
        } else {
            String::new()
        };
        if state.installed() {
            ui.detected(
                ResultKind::Success,
                &tf("EpicNext is the website of {url}", &[("url", &current.url())]),
                &tf(
                    "Octane is on {client}. Run this step again to update EpicNext (git pull, pnpm install, migrate and build).",
                    &[("client", &current.client_url())],
                ),
            );
        } else {
            let cms = if current.has_cms() {
                format!(
                    " {}",
                    tf("{cms} is the website now: EpicNext takes its place (its files stay).", &[("cms", &current.cms_name().unwrap_or_default())])
                )
            } else {
                String::new()
            };
            ui.detected(
                if state.atom_tables { ResultKind::Warning } else { ResultKind::Info },
                &tf("EpicNext will be the website of {url}", &[("url", &current.url())]),
                &format!("{}{cms}{atom}", tf("Octane moves to {url}/client/.", &[("url", &current.url())])),
            );
        }
        Ok(state.installed())
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let (hotel_name, url, branch, folder) = {
            let form = self.form.lock().unwrap();
            (form.hotel_name.trim().to_string(), form.git_url.trim().to_string(), form.branch.trim().to_string(), clean_folder(&form.folder))
        };
        let directory = epicnext::directory();
        let log = ui.logger();
        let host = server.login().host.clone();
        let server = server.as_ref();

        log("> Checking the server ...");
        let state = epicnext::inspect(server).await?;
        if let Some(missing) = missing(&state.server) {
            fail!("{missing}");
        }
        let mut site = WebAccessSettings { atom_cms: false, custom_cms: None, epic_next: true, ..state.site().cloned().unwrap_or_default() };
        log(&format!("  {}: Node.js {}, Octane built", site.url(), state.server.node_version.as_deref().unwrap_or("")));
        if state.atom_tables {
            log("  WARNING: the hotel database has AtomCMS' website_* tables; EpicNext's tables with the same name keep AtomCMS' layout.");
        }
        let credentials = aio_core::config_dir::load_database_credentials(&host);
        if credentials.is_none() {
            log("  The MariaDB passwords of this server are not saved on this computer: the step looks for a database login on the server.");
        }
        ui.done(&site.url());

        custom_cms::check_git_access(server, &url, &log).await?;
        let commit = if !folder.is_empty() {
            // EpicNext is a folder of a larger repository (by default the installer's own).
            epicnext::get_from_folder(server, &url, &branch, &folder, &log).await?
        } else {
            let clone = git_clone::inspect(server, &url, directory).await?;
            if clone.kind == CloneTargetKind::OtherContent {
                let reason = match &clone.origin {
                    None => t("It is not a clone of EpicNext.").to_string(),
                    Some(origin) => tf("It is a clone of {origin}.", &[("origin", origin)]),
                };
                let description = tf("{directory} is renamed to {directory}.bak-<date>; nothing is deleted.", &[("directory", directory)]);
                if !move_aside(server, &ui, directory, &reason, &description).await? {
                    return Ok(ui.aborted(&host));
                }
            }
            let wanted = (!branch.is_empty()).then_some(branch.as_str());
            if clone.kind == CloneTargetKind::SameRepository {
                git_clone::update(server, directory, wanted, &log).await?
            } else {
                git_clone::clone(server, &url, directory, wanted, &log).await?
            }
        };
        ui.done(&commit);

        epicnext::prepare_tools(server, &log).await?;
        epicnext::configure_env(server, &site, &hotel_name, credentials.as_ref(), &log).await?;
        ui.done(".env");

        epicnext::build(server, &log).await?;
        ui.done(t("built"));

        // Octane is configured and built for /client/ before the site switches, so it is never served half-way.
        octane::configure(server, &site, &log, false).await?;
        octane::build(server, &site, &log).await?;
        ui.done(&site.client_url());

        epicnext::start_service(server, &log).await?;
        let port = cms_site::apply(server, &site, epicnext::SITE_NAME, directory, false, &log, Some(epicnext::UPSTREAM)).await?;
        site.cms_port = port;
        web_access::configure_routes(server, &site, &log).await?;
        octane::publish(server, &site, &log).await?;
        ui.done(&tf("EpicNext on / (port {port}), Octane on /client/", &[("port", &port.to_string())]));

        epicnext::test(server, &site, &log).await?;
        ui.done("OK");

        *self.atom_tables.lock().unwrap() = state.atom_tables;
        Ok(ui.succeeded(
            &tf("EpicNext is the website of {url}", &[("url", &site.url())]),
            &tf(
                "Register on {url}/register, make that account admin (rank 7) and set the client URL in Admin → CMS Settings: see the next page.",
                &[("url", &site.url())],
            ),
        ))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(EpicNextStep::default());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step],
        false,
        Arc::new(move |info| super::finished::epicnext(info.site, finish_step.atom_tables(), info.steps)),
        t("First admin"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_cleaned() {
        assert_eq!(clean_folder("  /NextCMS/epicnext-cms/ "), "NextCMS/epicnext-cms");
        assert_eq!(clean_folder(""), "");
        assert_eq!(clean_folder("/"), "");
    }

    #[test]
    fn the_form_starts_with_epicnexts_own_repository() {
        let form = Form::default();
        assert_eq!(form.git_url, epicnext::REPOSITORY);
        assert_eq!(form.folder, epicnext::REPOSITORY_FOLDER);
    }
}
