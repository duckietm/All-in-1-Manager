//! Installs MariaDB 12.3, secures it, and creates root@'%', cms@'%' and the habbo database. (MariaDbStep.cs)

use std::sync::Arc;

use aio_core::{config_dir, DatabaseCredentials};
use aio_remote::mariadb::{self, BRANCH};
use aio_remote::{fail, Server};
use async_trait::async_trait;

use crate::gui::dialogs::Choice;
use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

pub struct MariaDbStep;

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(MariaDbStep)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpgradeChoice {
    Upgrade,
    Abort,
}

#[async_trait]
impl InstallStep for MariaDbStep {
    fn title(&self) -> String {
        "MariaDB".into()
    }

    fn caption(&self) -> String {
        tf("Database {branch}", &[("branch", BRANCH)])
    }

    fn install_label(&self) -> String {
        t("Install MariaDB").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Database,
            heading: format!("MariaDB {BRANCH}"),
            description: t("The database for Polaris and the CMS.").into(),
            bullets: vec![
                tf(
                    "Adds the official MariaDB {branch} repository; its signing key is checked against the published fingerprint.",
                    &[("branch", BRANCH)],
                ),
                t("Does what mariadb-secure-installation does: removes anonymous users and the test database, keeps root's socket login and remote root.").into(),
                t("Sets bind-address = 0.0.0.0; port 3306 stays closed in the firewall: HeidiSQL and the like connect through an SSH tunnel with your key.").into(),
                t("Creates root@'%', cms@'%' and the habbo database with strong generated passwords, and tests both logins.").into(),
            ],
            footnote: Some(t("The passwords are saved on this computer in configuration/database/<server>/mariadb.txt.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            tf("Add the MariaDB {branch} repository", &[("branch", BRANCH)]),
            tf("Install MariaDB {branch}", &[("branch", BRANCH)]),
            t("Secure it and listen on 0.0.0.0").into(),
            t("Create the users and the habbo database").into(),
            t("Test the logins").into(),
        ]
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let server = server.as_ref();
        let host = server.login().host.clone();
        let state = mariadb::inspect(server).await?;
        let saved = config_dir::load_database_credentials(&host);
        let path = config_dir::database_credentials_path(&host).display().to_string();
        let version = state.display_version().unwrap_or_default();

        if let (true, Some(saved)) = (state.is_complete(), saved.as_ref()) {
            let quiet = |_: &str| {};
            let problem = mariadb::test_login(server, &saved.cms_user, &saved.cms_password, Some(&saved.database), true, &quiet, true).await?;
            if problem.is_none() {
                ui.detected(
                    ResultKind::Success,
                    &tf("MariaDB {version} is already installed", &[("version", &version)]),
                    &tf(
                        "The users and the habbo database exist and the saved passwords work ({path}). You can skip this step.",
                        &[("path", &path)],
                    ),
                );
                return Ok(true);
            }
            ui.detected(
                ResultKind::Warning,
                &tf("MariaDB {version} is installed, but the saved passwords do not work", &[("version", &version)]),
                t("Installing sets the passwords from the configuration folder again."),
            );
            return Ok(false);
        }

        if state.mysql_installed {
            ui.detected(ResultKind::Warning, t("MySQL is installed"), t("MariaDB cannot be installed next to MySQL. Remove MySQL first."));
        } else if state.needs_upgrade() {
            ui.detected(
                ResultKind::Warning,
                &tf("MariaDB {version} is installed", &[("version", &version)]),
                &tf("Installing upgrades it to MariaDB {branch}; all databases are backed up first.", &[("branch", BRANCH)]),
            );
        } else if state.users_exist() && saved.is_none() {
            ui.detected(
                ResultKind::Info,
                t("The database users exist, but their passwords are not in the configuration folder"),
                t("Installing gives root@'%' and cms@'%' new passwords and saves them. Anything using the old passwords must be updated."),
            );
        } else if state.installed_version.is_some() {
            ui.detected(
                ResultKind::Info,
                &tf("MariaDB {version} is installed, but not set up completely", &[("version", &version)]),
                t("Installing finishes the setup and keeps your data."),
            );
        } else {
            ui.detected(ResultKind::Info, t("MariaDB is not installed yet"), t("Install it now, or skip this step."));
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let host = server.login().host.clone();

        log("> Checking the server ...");
        let state = mariadb::inspect(server).await?;
        let version_now = state.display_version().unwrap_or_default();
        log(&format!("  {} ({})", server.os().map(|o| o.pretty_name).unwrap_or_default(), state.codename));
        log(&match &state.installed_version {
            None => "  MariaDB is not installed yet.".to_string(),
            Some(_) => format!(
                "  mariadb-server {version_now} is installed{}",
                if state.from_mariadb_org() { " (mariadb.org)." } else { " (from the distribution)." }
            ),
        });
        if state.mysql_installed {
            fail!("MySQL is installed. MariaDB cannot be installed next to it: remove MySQL first (back up your data), then try again.");
        }

        if state.needs_upgrade() {
            let choice = ui
                .choose(
                    t("MariaDB already installed"),
                    &tf("MariaDB {version} is installed", &[("version", &version_now)]),
                    &tf(
                        "It will be upgraded to MariaDB {branch}. Your databases are kept and upgraded.",
                        &[("branch", BRANCH)],
                    ),
                    UpgradeChoice::Abort,
                    vec![
                        Choice::new(
                            tf("Upgrade to MariaDB {branch}", &[("branch", BRANCH)]),
                            t("All databases are first saved to /root/mariadb-backup-<date>.sql.gz."),
                            UpgradeChoice::Upgrade,
                        )
                        .primary(),
                        Choice::new(t("Abort installation"), t("Changes nothing on the server and disconnects."), UpgradeChoice::Abort),
                    ],
                )
                .await;
            if choice == UpgradeChoice::Abort {
                return Ok(ui.aborted(&host));
            }
        }

        // Saved passwords, or new ones saved before anything changes on the server.
        let credentials_path = config_dir::database_credentials_path(&host).display().to_string();
        let account = match config_dir::load_database_credentials(&host) {
            Some(saved) => {
                log(&format!("  Using the saved passwords from {credentials_path}"));
                saved
            }
            None => {
                let generated = DatabaseCredentials::generate(&host);
                config_dir::save_database_credentials(&generated)?;
                log(&format!("  Generated passwords for root and cms, saved in {credentials_path}"));
                if state.users_exist() {
                    log("  root@'%' and cms@'%' exist already: they get these new passwords.");
                }
                generated
            }
        };
        ui.done(&if state.installed_version.is_none() {
            t("fresh install").to_string()
        } else if state.needs_upgrade() {
            tf("upgrading {version}", &[("version", &version_now)])
        } else {
            t("updating").to_string()
        });

        mariadb::add_repository(server, &log).await?;
        ui.done(&format!("mariadb.org {BRANCH} ({})", state.codename));

        if state.needs_upgrade() && state.running {
            let backup = mariadb::backup(server, &log).await?;
            log(&format!("  Backup: {backup}"));
        }
        let version = mariadb::install(server, &log).await?;
        log(&format!("  Installed MariaDB {version}"));
        ui.done(&version);

        mariadb::secure_and_listen(server, &log).await?;
        ui.done("0.0.0.0:3306");

        mariadb::create_users(server, &account, &log).await?;
        ui.done(&format!("{}@'%', {}@'%', {}", account.root_user, account.cms_user, account.database));

        // Both accounts log in over TCP and socket; cms can create tables in habbo.
        log("> Testing the logins");
        let mut problem = mariadb::test_login(server, &account.root_user, &account.root_password, None, false, &log, true).await?;
        if problem.is_none() {
            problem = mariadb::test_local_root(server, &log).await?;
        }
        if problem.is_none() {
            problem = mariadb::test_login(server, &account.cms_user, &account.cms_password, Some(&account.database), true, &log, true).await?;
        }
        if let Some(problem) = problem {
            fail!("{problem}");
        }
        ui.done(t("root and cms work"));

        Ok(ui.succeeded(
            &tf("MariaDB {version} is running", &[("version", &version)]),
            &tf(
                "Database '{database}' on {host}:{port}. The passwords for {root} and {cms} are in {path}.",
                &[
                    ("database", &account.database),
                    ("host", &host),
                    ("port", &account.port.to_string()),
                    ("root", &account.root_user),
                    ("cms", &account.cms_user),
                    ("path", &credentials_path),
                ],
            ),
        ))
    }
}
