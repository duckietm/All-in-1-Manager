//! Removes fail2ban: stopped, off at boot, the installer's settings and the package removed. (RemoveFail2BanStep.cs)

use std::sync::Arc;

use aio_remote::{fail2ban, Server};
use async_trait::async_trait;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

pub struct RemoveFail2BanStep;

#[async_trait]
impl InstallStep for RemoveFail2BanStep {
    fn title(&self) -> String {
        t("Remove Fail2Ban").into()
    }

    fn caption(&self) -> String {
        t("Stop and uninstall").into()
    }

    fn install_label(&self) -> String {
        t("Remove Fail2Ban").into()
    }

    fn rerun_label(&self) -> String {
        t("Remove again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shield,
            heading: t("Remove Fail2Ban").into(),
            description: t("SSH is no longer protected against password guessing. The firewall (ufw) and everything else stay as they are.").into(),
            bullets: vec![
                t("Stops fail2ban and takes it off at boot: every current ban is lifted.").into(),
                tf("Deletes {file} (the settings and the whitelist).", &[("file", fail2ban::CONFIG_FILE)]),
                t("Removes the fail2ban package (apt-get remove).").into(),
                t("With SSH keys only (no passwords) the risk is small; with password logins, keep fail2ban.").into(),
            ],
            footnote: None,
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the server").into(), t("Stop and remove fail2ban").into()]
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = fail2ban::inspect(server.as_ref()).await?;
        if !state.installed {
            ui.detected(ResultKind::Success, t("Fail2ban is not installed"), t("There is nothing to remove. Running this step anyway removes what may be left."));
        } else {
            let version = state.version.as_deref().unwrap_or("");
            ui.detected(
                ResultKind::Info,
                &if state.running {
                    tf("Fail2ban {version} runs", &[("version", version)])
                } else {
                    tf("Fail2ban {version} is installed", &[("version", version)])
                },
                &tf("{count} address(es) are banned now; they are let in again.", &[("count", &state.banned.len().to_string())]),
            );
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let state = fail2ban::inspect(server).await?;
        if state.installed {
            log(&format!("  fail2ban {}, {} banned", state.version.as_deref().unwrap_or(""), state.banned.len()));
        } else {
            log("  fail2ban is not installed: removing what may be left");
        }
        ui.done(if state.installed { state.version.as_deref().unwrap_or(t("installed")) } else { t("not installed") });

        fail2ban::remove(server, &log).await?;
        ui.done(t("removed"));
        Ok(ui.succeeded(t("Fail2Ban is removed"), t("SSH is no longer protected by fail2ban. Addons → Fail2Ban → Install sets it up again.")))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![Arc::new(RemoveFail2BanStep) as Arc<dyn InstallStep>],
        false,
        Arc::new(|info| super::finished::remove_fail2ban(info.steps)),
        t("Fail2Ban removed"),
        t("Completed"),
    )
}
