//! Removes Gitea: the container, its network and the Traefik route; the data only when asked. (RemoveGiteaStep.cs)

use std::sync::{Arc, Mutex};

use aio_remote::{gitea, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

#[derive(Default)]
struct Output {
    removed_domain: Option<String>,
    data_deleted: bool,
}

#[derive(Default)]
pub struct RemoveGiteaStep {
    delete_data: Mutex<bool>,
    output: Mutex<Output>,
}

impl RemoveGiteaStep {
    pub fn removed_domain(&self) -> Option<String> {
        self.output.lock().unwrap().removed_domain.clone()
    }

    pub fn data_deleted(&self) -> bool {
        self.output.lock().unwrap().data_deleted
    }
}

fn result_message(domain: Option<&str>, deleted: bool) -> String {
    let mut message = String::new();
    if let Some(domain) = domain {
        message += &tf("{domain} no longer reaches Gitea. ", &[("domain", domain)]);
    }
    message += &if deleted {
        tf("{directory} is deleted.", &[("directory", gitea::DIRECTORY)])
    } else {
        tf(
            "Its repositories, accounts and settings stay in {directory}: Install Gitea brings them back.",
            &[("directory", gitea::DIRECTORY)],
        )
    };
    message += t(" Remove the domain's A record when you no longer need it.");
    message
}

#[async_trait]
impl InstallStep for RemoveGiteaStep {
    fn title(&self) -> String {
        t("Remove Gitea").into()
    }

    fn caption(&self) -> String {
        t("Container and route").into()
    }

    fn install_label(&self) -> String {
        t("Remove Gitea").into()
    }

    fn rerun_label(&self) -> String {
        t("Remove again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Git,
            heading: t("Remove Gitea").into(),
            description: t("Gitea stops and is no longer reachable on its domain. The hotel, Traefik and NGINX keep running as they are.").into(),
            bullets: vec![
                t("Stops and removes the Gitea container and its Docker network (docker compose down).").into(),
                tf(
                    "Removes its Traefik route ({file}): Traefik no longer answers for Gitea's domain, and its port is free again.",
                    &[("file", gitea::ROUTE_FILE)],
                ),
                tf(
                    "Keeps {directory} (the repositories, accounts and settings) unless you tick the box below: installing Gitea again then brings everything back.",
                    &[("directory", gitea::DIRECTORY)],
                ),
                t("Remove the domain's A record from your DNS yourself when you no longer need it.").into(),
            ],
            footnote: None,
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the server").into(), t("Stop Gitea and remove its route").into(), t("Check that the domain is gone").into()]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut delete = self.delete_data.lock().unwrap();
        ui.add_space(8.0);
        ui.checkbox(
            &mut delete,
            tf(
                "Also delete the repositories, accounts and settings ({directory}): this cannot be undone",
                &[("directory", gitea::DIRECTORY)],
            ),
        );
        ui.add(
            egui::Label::new(
                RichText::new(t("Without the box, the data stays on the server: download it first if you want a copy elsewhere (SFTP, for example WinSCP)."))
                    .size(12.0)
                    .color(palette.subtle),
            )
            .wrap(),
        );
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = gitea::inspect(server.as_ref()).await?;
        if state.domain.is_none() && !state.running {
            ui.detected(
                ResultKind::Success,
                t("Gitea is not installed"),
                t("There is nothing to remove. Running this step anyway removes what may be left (container, route)."),
            );
        } else {
            let domain = state.domain.clone().unwrap_or_default();
            ui.detected(
                ResultKind::Info,
                &tf("Gitea {version} runs on {domain}", &[("version", state.version.as_deref().unwrap_or("")), ("domain", &domain)]),
                &tf("After this step {domain} no longer reaches Gitea.", &[("domain", &domain)]),
            );
        }
        // Not "installed": a removal page shows its own button, never "Skip (already installed)".
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let state = gitea::inspect(server).await?;
        let delete = *self.delete_data.lock().unwrap();
        match &state.domain {
            None => log("  Gitea is not set up: removing what may be left"),
            Some(domain) => log(&format!("  Gitea {} on {domain} (port {})", state.version.as_deref().unwrap_or(""), state.port)),
        }
        ui.done(state.domain.as_deref().unwrap_or(t("not installed")));

        let https = state.site.as_ref().map(|s| s.https).unwrap_or(true);
        gitea::remove(server, state.domain.as_deref(), https, delete, &log).await?;
        ui.done(if delete { t("removed, data deleted") } else { t("removed, data kept") });

        // The admin's saved password belongs to the deleted data.
        let login = super::gitea::login_path(&server.login().host);
        if delete && login.exists() {
            std::fs::remove_file(&login)?;
        }
        ui.done(state.domain.as_deref().unwrap_or("-"));

        {
            let mut output = self.output.lock().unwrap();
            output.removed_domain = state.domain.clone();
            output.data_deleted = delete;
        }
        Ok(ui.succeeded(t("Gitea is removed"), &result_message(state.domain.as_deref(), delete)))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(RemoveGiteaStep::default());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| super::finished::remove_gitea(finish_step.removed_domain(), finish_step.data_deleted(), info.steps)),
        t("Gitea removed"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_message_as_the_csharp() {
        assert_eq!(
            result_message(Some("git.a.com"), true),
            "git.a.com no longer reaches Gitea. /opt/gitea is deleted. Remove the domain's A record when you no longer need it."
        );
        assert_eq!(
            result_message(None, false),
            "Its repositories, accounts and settings stay in /opt/gitea: Install Gitea brings them back. Remove the domain's A record when you no longer need it."
        );
    }
}
