//! Removes CrowdSec: the bouncer out of Traefik first, then the engine; its data only when asked.
//! (RemoveCrowdSecStep.cs)

use std::sync::Arc;

use aio_remote::{crowdsec, Server};
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

#[derive(Default)]
pub struct RemoveCrowdSecStep {
    delete_data: Mutex<bool>,
    data_deleted: Mutex<bool>,
}

impl RemoveCrowdSecStep {
    pub fn data_deleted(&self) -> bool {
        *self.data_deleted.lock()
    }
}

fn result_message(deleted: bool) -> String {
    let middle = if deleted {
        tf("{directory} is deleted.", &[("directory", crowdsec::directory())])
    } else {
        tf(
            "Its data and enrollment stay in {directory}: Install CrowdSec brings it back without a new key.",
            &[("directory", crowdsec::directory())],
        )
    };
    format!(
        "{}{middle}{}",
        t("Traefik no longer checks visitors with CrowdSec. "),
        t(" Remove the engine from your CrowdSec console when you no longer need it.")
    )
}

#[async_trait]
impl InstallStep for RemoveCrowdSecStep {
    fn title(&self) -> String {
        t("Remove CrowdSec").into()
    }

    fn caption(&self) -> String {
        t("Bouncer and engine").into()
    }

    fn install_label(&self) -> String {
        t("Remove CrowdSec").into()
    }

    fn rerun_label(&self) -> String {
        t("Remove again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shield,
            heading: t("Remove CrowdSec").into(),
            description: t("Traefik no longer checks visitors with CrowdSec, and every ban is lifted. The hotel, Traefik and the firewall keep running as they are.").into(),
            bullets: vec![
                tf(
                    "Takes the bouncer out of Traefik ({file} and the plugin) and restarts Traefik (players are disconnected for a moment).",
                    &[("file", &crowdsec::middleware_file())],
                ),
                t("Stops and removes the CrowdSec container (docker compose down).").into(),
                tf(
                    "Keeps {directory} (its data and the enrollment) unless you tick the box below: installing again then needs no new key.",
                    &[("directory", crowdsec::directory())],
                ),
                t("Remove the engine from your CrowdSec console yourself when you no longer need it (Security Engines → the engine → Remove).").into(),
            ],
            footnote: None,
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the server").into(), t("Remove the bouncer and CrowdSec").into()]
    }

    fn options(&self, ui: &mut egui::Ui, _palette: &Palette) {
        let mut delete = self.delete_data.lock();
        ui.add_space(8.0);
        ui.checkbox(
            &mut delete,
            tf(
                "Also delete CrowdSec's data and enrollment ({directory}): installing again then needs a new key",
                &[("directory", crowdsec::directory())],
            ),
        );
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = crowdsec::inspect(server.as_ref()).await?;
        if !state.installed && !state.bouncer_configured {
            ui.detected(ResultKind::Success, t("CrowdSec is not installed"), t("There is nothing to remove. Running this step anyway removes what may be left."));
        } else {
            let version = state.version.as_deref().unwrap_or("");
            ui.detected(
                ResultKind::Info,
                &if state.running {
                    tf("CrowdSec {version} runs", &[("version", version)])
                } else {
                    tf("CrowdSec {version} is installed", &[("version", version)])
                },
                &tf(
                    "{count} address(es) banned by this server now; after this step nobody is blocked by CrowdSec.",
                    &[("count", &state.banned.len().to_string())],
                ),
            );
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let state = crowdsec::inspect(server).await?;
        let delete = *self.delete_data.lock();
        if state.installed {
            log(&format!(
                "  CrowdSec {}, bouncer in Traefik: {}",
                state.version.as_deref().unwrap_or(""),
                if state.bouncer_configured { "yes" } else { "no" }
            ));
        } else {
            log("  CrowdSec is not set up: removing what may be left");
        }
        ui.done(if state.installed { state.version.as_deref().unwrap_or(t("installed")) } else { t("not installed") });

        crowdsec::remove(server, state.site.as_ref(), delete, &log).await?;
        ui.done(if delete { t("removed, data deleted") } else { t("removed, data kept") });
        *self.data_deleted.lock() = delete;
        Ok(ui.succeeded(t("CrowdSec is removed"), &result_message(delete)))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(RemoveCrowdSecStep::default());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| super::finished::remove_crowdsec(finish_step.data_deleted(), info.steps)),
        t("CrowdSec removed"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_message_as_the_csharp() {
        assert_eq!(
            result_message(true),
            "Traefik no longer checks visitors with CrowdSec. /opt/crowdsec is deleted. Remove the engine from your CrowdSec console when you no longer need it."
        );
        assert!(result_message(false).contains("Install CrowdSec brings it back without a new key."));
    }
}
