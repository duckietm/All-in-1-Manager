//! Installs NGINX mainline from nginx.org with the Debian sites-available / sites-enabled layout. (NginxStep.cs)

use std::sync::Arc;

use aio_remote::{fail, nginx, Server};
use async_trait::async_trait;

use crate::gui::dialogs::Choice;
use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

pub struct NginxStep;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistingNginxChoice {
    Replace,
    Abort,
}

#[async_trait]
impl InstallStep for NginxStep {
    fn title(&self) -> String {
        "NGINX".into()
    }

    fn caption(&self) -> String {
        t("Web server").into()
    }

    fn install_label(&self) -> String {
        t("Install NGINX").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Server,
            heading: t("NGINX mainline").into(),
            description: t("The web server for the hotel: it serves Octane and forwards the game connection to Polaris.").into(),
            bullets: vec![
                t("Adds the official nginx.org mainline repository; its signing key is checked against the published fingerprint.").into(),
                t("Installs the newest NGINX mainline and keeps it updated through apt.").into(),
                t("Adds the Debian layout from nginx-common: /etc/nginx/sites-available, sites-enabled, snippets and proxy_params.").into(),
                t("Starts NGINX at boot, opens ports 80 and 443 when the ufw firewall is on, and checks that it answers.").into(),
            ],
            footnote: Some(t("The nginx-common package itself is not installed: nginx.org's NGINX conflicts with it, so the installer creates the same folders instead.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            t("Add the nginx.org mainline repository").into(),
            t("Install NGINX mainline").into(),
            t("Set up sites-available / sites-enabled").into(),
            t("Start NGINX and test it").into(),
        ]
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = nginx::inspect(server.as_ref()).await?;
        let version = state.installed_version.clone().unwrap_or_default();
        if state.is_complete() {
            ui.detected(
                ResultKind::Success,
                &tf("NGINX {version} is already installed", &[("version", &version)]),
                &format!(
                    "{}{} {}",
                    t("NGINX mainline from nginx.org with sites-available / sites-enabled"),
                    if state.running { t(", running.") } else { t(" (not running right now).") },
                    t("You can skip this step, or run it again to upgrade and repair.")
                ),
            );
            return Ok(true);
        }
        let os = server.os().map(|o| o.id).unwrap_or_default();
        if state.has_nginx_org_nginx() {
            ui.detected(
                ResultKind::Info,
                &tf("NGINX {version} from nginx.org is installed", &[("version", &version)]),
                t("The sites-available / sites-enabled layout is missing; installing adds it and keeps your configuration."),
            );
        } else if state.has_distro_nginx() {
            ui.detected(
                ResultKind::Warning,
                &tf("NGINX {version} from {os} is installed", &[("version", &version), ("os", &os)]),
                t("Installing replaces it with NGINX mainline (a backup of /etc/nginx is made first)."),
            );
        } else {
            ui.detected(ResultKind::Info, t("NGINX is not installed yet"), t("Install it now, or skip this step."));
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        // Nothing is changed until the user agreed.
        log("> Checking the server ...");
        let state = nginx::inspect(server).await?;
        let os = server.os();
        log(&format!("  {} ({})", os.as_ref().map(|o| o.pretty_name.as_str()).unwrap_or(""), state.codename));
        match &state.installed_version {
            None => log("  NGINX is not installed yet."),
            Some(version) if state.has_distro_nginx() => log(&format!(
                "  NGINX {version} is installed from {} ({}).",
                os.as_ref().map(|o| o.id.as_str()).unwrap_or("the distribution"),
                state.distro_packages.join(", ")
            )),
            Some(version) => log(&format!("  NGINX {version} is installed from nginx.org.")),
        }
        if let Some(process) = &state.port80_process {
            log(&format!("  Port 80 is in use by: {process}"));
        }
        if state.port80_blocked() {
            let process = state.port80_process.clone().unwrap_or_default();
            fail!("Port 80 is in use by '{process}'. Stop and disable it first (for example: systemctl disable --now {process}), then try again.");
        }

        if state.has_distro_nginx() {
            let id = os.as_ref().map(|o| o.id.clone()).unwrap_or_default();
            let version = state.installed_version.clone().unwrap_or_default();
            let choice = ui
                .choose(
                    t("NGINX already installed"),
                    t("NGINX from Ubuntu/Debian is installed"),
                    &tf(
                        "NGINX {version} from the {os} archive is installed. It cannot stay next to NGINX mainline from nginx.org.",
                        &[("version", &version), ("os", &id)],
                    ),
                    ExistingNginxChoice::Abort,
                    vec![
                        Choice::new(
                            t("Replace it with NGINX mainline"),
                            t("/etc/nginx is saved to /root/nginx-backup-<date>.tar.gz, the old packages are removed and the configuration starts fresh."),
                            ExistingNginxChoice::Replace,
                        )
                        .primary(),
                        Choice::new(t("Abort installation"), t("Changes nothing on the server and disconnects."), ExistingNginxChoice::Abort),
                    ],
                )
                .await;
            if choice == ExistingNginxChoice::Abort {
                return Ok(ui.aborted(&server.login().host));
            }
        }
        ui.done(&match &state.installed_version {
            None => t("fresh install").to_string(),
            Some(_) if state.has_distro_nginx() => t("replacing distro NGINX").to_string(),
            Some(version) => tf("upgrading {version}", &[("version", version)]),
        });

        nginx::add_repository(server, &log).await?;
        ui.done(&tf("nginx.org mainline ({codename})", &[("codename", &state.codename)]));

        if state.has_distro_nginx() {
            nginx::remove_distro_nginx(server, &log).await?;
        }
        let version = nginx::install_package(server, &log).await?;
        log(&format!("  Installed NGINX {version}"));
        ui.done(&version);

        nginx::set_up_debian_layout(server, &log).await?;
        ui.done(t("sites-enabled included"));

        nginx::start_and_test(server, &log).await?;
        ui.done("HTTP 200");

        Ok(ui.succeeded(
            &tf("NGINX {version} is running", &[("version", &version)]),
            &tf(
                "Put site configurations in /etc/nginx/sites-available and link them in sites-enabled. Test it: http://{host}/",
                &[("host", &server.login().host)],
            ),
        ))
    }
}
