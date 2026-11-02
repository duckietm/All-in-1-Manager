//! Installs PHP 8.5 (at least 8.5.12) with PHP-FPM and the CMS extensions from packages.sury.org. (PhpStep.cs)

use std::sync::Arc;

use aio_remote::{php, Server};
use async_trait::async_trait;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

pub struct PhpStep;

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(PhpStep)
}

/// The detected banner's message for an installed but incomplete PHP.
fn incomplete_message(missing: &[String], fpm_running: bool) -> String {
    let mut message = String::new();
    if !missing.is_empty() {
        message.push_str(&tf("Missing: {packages}. ", &[("packages", &missing.join(", "))]));
    }
    if !fpm_running {
        message.push_str(t("PHP-FPM is not running. "));
    }
    message.push_str(t("Installing completes it."));
    message
}

#[async_trait]
impl InstallStep for PhpStep {
    fn title(&self) -> String {
        "PHP".into()
    }

    fn caption(&self) -> String {
        format!("PHP {}", php::BRANCH)
    }

    fn install_label(&self) -> String {
        t("Install PHP").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Cube,
            heading: tf("PHP {branch} with PHP-FPM", &[("branch", php::BRANCH)]),
            description: tf(
                "For the hotel website. Version {minimum} or newer is required: older releases have security problems.",
                &[("minimum", php::MINIMUM_VERSION)],
            ),
            bullets: vec![
                t("Adds Ondřej Surý's PHP repository (packages.sury.org); its keyring is checked before it is installed.").into(),
                t("Installs PHP-FPM, the CLI and the extensions: mysql, xml, xmlrpc, curl, gd, imagick, mbstring, soap, zip, intl, bcmath, plus unzip.").into(),
                tf("Refuses a PHP older than {minimum} and checks that every extension is loaded.", &[("minimum", php::MINIMUM_VERSION)]),
                t("Starts PHP-FPM at boot and, when NGINX is installed, connects them and tests a PHP page through NGINX.").into(),
            ],
            footnote: Some(tf("Packages: {packages}", &[("packages", &php::PACKAGES.join(" "))])),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            t("Add the packages.sury.org PHP repository").into(),
            tf("Install PHP {branch} and extensions", &[("branch", php::BRANCH)]),
            t("Check the version and extensions").into(),
            t("Start PHP-FPM and connect it to NGINX").into(),
        ]
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = php::inspect(server.as_ref()).await?;
        let version = state.php_version.clone().unwrap_or_default();
        if state.is_complete() {
            ui.detected(
                ResultKind::Success,
                &tf("PHP {version} is already installed", &[("version", &version)]),
                t("PHP-FPM is running with all extensions. You can skip this step, or run it again to update."),
            );
            return Ok(true);
        }
        if state.php_version.is_some() && !state.version_ok() {
            ui.detected(
                ResultKind::Warning,
                &tf("PHP {version} is too old", &[("version", &version)]),
                &tf(
                    "At least {minimum} is required for security fixes. Installing updates it.",
                    &[("minimum", php::MINIMUM_VERSION)],
                ),
            );
        } else if state.php_version.is_some() {
            ui.detected(
                ResultKind::Info,
                &tf("PHP {version} is installed, but not complete", &[("version", &version)]),
                &incomplete_message(&state.missing_packages, state.fpm_running),
            );
        } else {
            ui.detected(
                ResultKind::Info,
                &tf("PHP {branch} is not installed yet", &[("branch", php::BRANCH)]),
                t("Install it now, or skip this step."),
            );
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let state = php::inspect(server).await?;
        let os = server.os();
        log(&format!("  {} ({})", os.as_ref().map(|o| o.pretty_name.as_str()).unwrap_or(""), state.codename));
        log(&match &state.php_version {
            None => format!("  PHP {} is not installed yet.", php::BRANCH),
            Some(version) => format!("  PHP {version} is installed."),
        });
        if !state.other_versions.is_empty() {
            log(&format!("  Other PHP versions on the server: {} (left as they are)", state.other_versions.join(", ")));
        }
        ui.done(&match &state.php_version {
            None => t("fresh install").to_string(),
            Some(version) => tf("updating {version}", &[("version", version)]),
        });

        php::add_repository(server, &log).await?;
        ui.done(&format!("packages.sury.org ({})", state.codename));

        // The script refuses anything older than the minimum.
        let version = php::install_packages(server, &log).await?;
        log(&format!("  Installed PHP {version}"));
        ui.done(&version);

        php::check_extensions(server, &log).await?;
        ui.done(&tf("≥ {minimum}, all extensions", &[("minimum", php::MINIMUM_VERSION)]));

        let with_nginx = php::start_and_connect(server, &log).await?;
        ui.done(if with_nginx { t("PHP page served by NGINX") } else { t("running (no NGINX)") });

        Ok(ui.succeeded(
            &tf("PHP {version} is running", &[("version", &version)]),
            if with_nginx {
                t("PHP-FPM listens on /run/php/php8.5-fpm.sock. In an NGINX site: include snippets/fastcgi-php.conf; fastcgi_pass unix:/run/php/php8.5-fpm.sock;")
            } else {
                t("PHP-FPM listens on /run/php/php8.5-fpm.sock. NGINX is not installed, so it was not connected.")
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_lists_what_is_missing() {
        assert_eq!(incomplete_message(&[], true), "Installing completes it.");
        assert_eq!(
            incomplete_message(&["php8.5-gd".into(), "unzip".into()], false),
            "Missing: php8.5-gd, unzip. PHP-FPM is not running. Installing completes it."
        );
    }
}
