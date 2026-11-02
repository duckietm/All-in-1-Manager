//! Removes the CMS (AtomCMS, EpicNext or your own): Octane is the site again, as after the server installation.
//! (RemoveCmsStep.cs, InstallerWindow._removeCmsWizard)

use std::sync::{Arc, Mutex};

use aio_remote::web_access::{self, WebAccessSettings};
use aio_remote::{atom_cms, cms_site, custom_cms, epicnext, fail, hotel_site, octane, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

struct Form {
    delete_files: bool,
    delete_enabled: bool,
    delete_what: String,
}

impl Default for Form {
    fn default() -> Self {
        Self { delete_files: false, delete_enabled: true, delete_what: String::new() }
    }
}

/// Set after a run, for the finished page.
#[derive(Default, Clone)]
pub struct RemoveCmsResult {
    pub removed_name: Option<String>,
    pub files_deleted: bool,
}

#[derive(Default)]
pub struct RemoveCmsStep {
    form: Mutex<Form>,
    result: Mutex<RemoveCmsResult>,
}

impl RemoveCmsStep {
    pub fn result(&self) -> RemoveCmsResult {
        self.result.lock().unwrap().clone()
    }
}

/// The CMS on the server: its NGINX site name and folder.
fn cms(site: &WebAccessSettings) -> Option<(String, String)> {
    if site.atom_cms {
        Some((atom_cms::SITE_NAME.into(), atom_cms::directory()))
    } else if site.epic_next {
        Some((epicnext::SITE_NAME.into(), epicnext::DIRECTORY.into()))
    } else {
        match &site.custom_cms {
            Some(name) if custom_cms::name_problem(name).is_none() => Some((name.clone(), custom_cms::directory(name))),
            _ => None,
        }
    }
}

/// What ticking the box deletes, under it.
fn delete_what(site: &WebAccessSettings) -> String {
    let Some((site_name, folder)) = cms(site) else { return String::new() };
    let service = if site.epic_next { tf(", and the {service} service", &[("service", epicnext::SERVICE)]) } else { String::new() };
    tf("Deletes {folder} and {file}{service}.", &[("folder", &folder), ("file", &cms_site::file(&site_name)), ("service", &service)])
}

#[async_trait]
impl InstallStep for RemoveCmsStep {
    fn title(&self) -> String {
        t("Remove the CMS").into()
    }

    fn caption(&self) -> String {
        t("Octane on / again").into()
    }

    fn install_label(&self) -> String {
        t("Remove the CMS").into()
    }

    fn rerun_label(&self) -> String {
        t("Set it back again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Puzzle,
            heading: t("Remove the CMS").into(),
            description: t("The hotel goes back to how the server installation set it up: Octane is the website on /, with its own login screen, and the CMS is no longer served.").into(),
            bullets: vec![
                t("Octane moves back to /: vite without --base, client-mode.json with /configuration/, its own login screen on again, and a rebuild.").into(),
                tf(
                    "NGINX: the hotel site serves Octane on / again (127.0.0.1:{port}); the CMS' own NGINX site is disabled (and EpicNext's {service} service stopped).",
                    &[("port", hotel_site::PORT), ("service", epicnext::SERVICE)],
                ),
                t("Traefik: /ws and all of /api go to Polaris again, everything else to the hotel site.").into(),
                t("The CMS' files stay unless you tick the box below. Its database tables (such as AtomCMS' website_* tables) and users stay in the hotel database.").into(),
                t("Tests the site, Polaris' /api/health and the websocket through Traefik.").into(),
            ],
            footnote: Some(t("The hotel is briefly unavailable while Octane is rebuilt (about a minute).").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        [n("Check the server"), n("Octane on /: configure and build"), n("NGINX and Traefik"), n("Disable the CMS site"), n("Test the site, /api and /ws")]
            .iter()
            .map(|c| t(c).to_string())
            .collect()
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        let form = &mut *form;
        ui.add_space(2.0);
        ui.add_enabled(form.delete_enabled, egui::Checkbox::new(&mut form.delete_files, t("Also delete the CMS' folder and its NGINX site file (cannot be undone)")));
        if !form.delete_what.is_empty() {
            ui.add(egui::Label::new(RichText::new(&form.delete_what).size(12.0).color(palette.subtle)).wrap());
        }
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = atom_cms::inspect(server.as_ref()).await?;
        let Some(site) = state.site else {
            ui.detected(ResultKind::Warning, t("There is no website set up yet"), t("Run the Web access step first."));
            return Ok(false);
        };
        if !site.has_cms() {
            {
                let mut form = self.form.lock().unwrap();
                form.delete_what.clear();
                form.delete_enabled = false;
            }
            ui.detected(
                ResultKind::Success,
                t("There is no CMS"),
                &tf("Octane is the website of {url} already. Running this step anyway sets Octane, NGINX and Traefik back.", &[("url", &site.url())]),
            );
            // Not "installed": a removal page shows its own button, never "Skip (already installed)".
            return Ok(false);
        }
        {
            let mut form = self.form.lock().unwrap();
            form.delete_enabled = cms(&site).is_some();
            form.delete_what = delete_what(&site);
        }
        ui.detected(
            ResultKind::Info,
            &tf("{cms} is the website of {url}", &[("cms", &site.cms_name().unwrap_or_default()), ("url", &site.url())]),
            &tf("After this step Octane is the website on {url}/ again, with its own login screen.", &[("url", &site.url())]),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();

        log("> Checking the server ...");
        let state = atom_cms::inspect(server).await?;
        let Some(current) = state.site.clone() else { fail!("{}", t("Run the Web access step first: there is no website to set back.")) };
        if state.node_version.is_none() {
            fail!("{}", t("Node.js is missing: run the Dev tools step first (Octane is rebuilt)."));
        }
        if !state.octane_built {
            fail!("{}", t("Octane is not built yet: run the Octane step first."));
        }
        let cms = cms(&current);
        let delete = {
            let form = self.form.lock().unwrap();
            form.delete_files && form.delete_enabled
        } && cms.is_some();
        let site = WebAccessSettings { atom_cms: false, custom_cms: None, epic_next: false, cms_port: 0, ..current.clone() };
        let current_name = current.cms_name();
        if current.has_cms() {
            log(&format!("  {} is the website of {}: setting Octane back on /", current_name.clone().unwrap_or_default(), current.url()));
        } else {
            log(&format!("  No CMS on {}: setting Octane, NGINX and Traefik back anyway", current.url()));
        }
        ui.done(current_name.as_deref().unwrap_or(t("no CMS")));

        octane::configure(server, &site, &log, true).await?;
        octane::build(server, &site, &log).await?;
        ui.done(&site.client_url());

        // Traefik sends everything to the hotel site first: publishing checks Octane on / through Traefik, which would
        // still reach the CMS otherwise.
        web_access::configure_routes(server, &site, &log).await?;
        octane::publish(server, &site, &log).await?;
        ui.done(t("Octane on /"));

        // EpicNext's own server stops first: nothing serves it any more.
        if current.epic_next {
            epicnext::remove_service(server, delete, &log).await?;
        }
        let site_name = cms.as_ref().map(|(name, _)| name.as_str());
        let folder = if delete { cms.as_ref().map(|(_, folder)| folder.as_str()) } else { None };
        cms_site::remove(server, site_name, folder, &log).await?;
        ui.done(if delete { t("disabled and deleted") } else { t("disabled, files kept") });

        let result = web_access::test(server, &site, &log).await?;
        ui.done(if result.api_answered { "OK" } else { t("site OK, check /api") });

        *self.result.lock().unwrap() = RemoveCmsResult { removed_name: current_name.clone(), files_deleted: delete };
        let files = match &cms {
            None => String::new(),
            Some((_, folder)) if delete => format!(" {}", tf("{folder} is deleted.", &[("folder", folder)])),
            Some((_, folder)) => format!(" {}", tf("Its files stay in {folder}.", &[("folder", folder)])),
        };
        let removed = if current.has_cms() {
            format!("{}{files} ", tf("{cms} is removed.", &[("cms", &current_name.unwrap_or_default())]))
        } else {
            String::new()
        };
        Ok(ui.succeeded(
            &tf("Octane is the website of {url} again", &[("url", &site.url())]),
            &format!(
                "{removed}{}",
                t("Octane has its own login screen again; Traefik and NGINX are as after the server installation. Install a CMS again with Addons → CMS.")
            ),
        ))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(RemoveCmsStep::default());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step],
        false,
        Arc::new(move |info| {
            let result = finish_step.result();
            super::finished::remove_cms(info.site, result.removed_name, result.files_deleted, info.steps)
        }),
        t("Octane on /"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cms_to_remove() {
        let mut site = WebAccessSettings::new("hotel.example", true, false);
        assert_eq!(cms(&site), None);
        assert_eq!(delete_what(&site), "");
        site.epic_next = true;
        assert_eq!(cms(&site), Some((epicnext::SITE_NAME.to_string(), epicnext::DIRECTORY.to_string())));
        assert!(delete_what(&site).contains(epicnext::SERVICE));
        site.epic_next = false;
        site.custom_cms = Some("mycms".into());
        assert_eq!(cms(&site).map(|c| c.0), Some("mycms".to_string()));
        site.custom_cms = Some("Bad Name".into());
        assert_eq!(cms(&site), None);
    }
}
