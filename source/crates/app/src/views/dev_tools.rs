//! Installs Node.js 26 (NodeSource), Composer and Yarn (Corepack). (DevToolsStep.cs)

use std::sync::Arc;

use aio_remote::dev_tools::{self, DevToolsState, NODE_MAJOR};
use aio_remote::{fail, Server};
use async_trait::async_trait;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

pub struct DevToolsStep;

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(DevToolsStep)
}

/// The tools that are not installed yet, for "Missing: ...".
fn missing_tools(state: &DevToolsState) -> Vec<&'static str> {
    [
        state.node_version.is_none().then_some("Node.js"),
        state.composer_version.is_none().then_some("Composer"),
        state.yarn_version.is_none().then_some("Yarn"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[async_trait]
impl InstallStep for DevToolsStep {
    fn title(&self) -> String {
        "Node.js".into()
    }

    fn caption(&self) -> String {
        "Composer · Yarn".into()
    }

    fn install_label(&self) -> String {
        t("Install Node.js, Composer and Yarn").into()
    }

    fn intro(&self) -> Intro {
        let major = NODE_MAJOR.to_string();
        Intro {
            icon: Icon::Wrench,
            heading: tf("Node.js {major}, Composer and Yarn", &[("major", &major)]),
            description: t("The build tools: Octane and the renderer are built with Node.js and Yarn, the CMS uses Composer.").into(),
            bullets: vec![
                tf(
                    "Adds the NodeSource Node.js {major}.x repository; its signing key is checked against the published fingerprint.",
                    &[("major", &major)],
                ),
                tf("Installs Node.js {major} with npm.", &[("major", &major)]),
                t("Installs Composer (the latest stable composer.phar from getcomposer.org), checked against its published SHA-256 checksum.").into(),
                t("Installs Corepack and enables Yarn; each project uses the Yarn version in its package.json (Octane: yarn 4).").into(),
            ],
            footnote: Some(t("Composer needs PHP, so run the PHP step first.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        let major = NODE_MAJOR.to_string();
        vec![
            t("Check the server").into(),
            tf("Add the NodeSource Node.js {major} repository", &[("major", &major)]),
            tf("Install Node.js {major}", &[("major", &major)]),
            t("Install Composer").into(),
            t("Install Yarn (Corepack)").into(),
            t("Check the versions").into(),
        ]
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = dev_tools::inspect(server.as_ref()).await?;
        let node = state.node_version.clone().unwrap_or_default();
        if state.is_complete() && state.shadowing.is_empty() {
            ui.detected(
                ResultKind::Success,
                t("Node.js, Composer and Yarn are already installed"),
                &tf(
                    "node {node}, yarn {yarn}, composer {composer}. You can skip this step, or run it again to update.",
                    &[
                        ("node", &node),
                        ("yarn", state.yarn_version.as_deref().unwrap_or("")),
                        ("composer", state.composer_version.as_deref().unwrap_or("")),
                    ],
                ),
            );
            return Ok(true);
        }
        if state.is_complete() {
            // Installing again would not change this: skipping stays the main choice.
            ui.detected(
                ResultKind::Warning,
                t("Installed, but older copies come first in the PATH"),
                &tf(
                    "Node.js {major}, Composer and Yarn are in /usr/bin, but {paths} hide them. Remove those if you do not need them (rm {rm}).",
                    &[("major", &NODE_MAJOR.to_string()), ("paths", &state.shadowing.join(", ")), ("rm", &state.shadowing.join(" "))],
                ),
            );
            return Ok(true);
        }
        if !state.php_installed {
            ui.detected(ResultKind::Warning, t("PHP is not installed"), t("Composer needs PHP: run the PHP step first, or Composer will fail."));
        } else if state.node_version.is_some() && (state.node_major() != Some(NODE_MAJOR) || !state.node_from_node_source) {
            ui.detected(
                ResultKind::Info,
                &tf("node {version} is installed", &[("version", &node)]),
                &tf("Installing replaces it with Node.js {major} from NodeSource.", &[("major", &NODE_MAJOR.to_string())]),
            );
        } else {
            ui.detected(ResultKind::Info, t("Not installed yet"), &tf("Missing: {tools}.", &[("tools", &missing_tools(&state).join(", "))]));
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        // Composer needs PHP: stop before changing anything.
        log("> Checking the server ...");
        let state = dev_tools::inspect(server).await?;
        log(&format!(
            "  node: {}{}",
            state.node_version.as_deref().unwrap_or("not installed"),
            if state.node_version.is_some() && !state.node_from_node_source { " (not from NodeSource)" } else { "" }
        ));
        log(&format!(
            "  yarn: {}, composer: {}",
            state.yarn_version.as_deref().unwrap_or("not installed"),
            state.composer_version.as_deref().unwrap_or("not installed")
        ));
        if !state.php_installed {
            fail!("Composer needs PHP, which is not installed. Run the PHP step first (or skip this step).");
        }
        ui.done(&match &state.node_version {
            None => t("fresh install").to_string(),
            Some(version) => tf("updating from node {version}", &[("version", version)]),
        });

        dev_tools::add_node_source(server, &log).await?;
        ui.done(&format!("node_{NODE_MAJOR}.x"));
        let node = dev_tools::install_node(server, &log).await?;
        ui.done(&node);
        dev_tools::install_composer(server, &log).await?;
        ui.done(t("checksum verified"));
        dev_tools::install_yarn(server, &log).await?;
        ui.done(t("corepack enabled"));

        log("> Versions");
        let v = dev_tools::versions(server, &log).await?;
        ui.done(&format!("node {} · npm {} · yarn {} · composer {}", v.node, v.npm, v.yarn, v.composer));

        let versions = [("node", v.node.as_str()), ("npm", v.npm.as_str()), ("yarn", v.yarn.as_str()), ("composer", v.composer.as_str())];
        if !v.shadowing.is_empty() {
            log(&format!("! Older copies come first in root's PATH and hide the installed tools: {}", v.shadowing.join(", ")));
            let (paths, rm) = (v.shadowing.join(", "), v.shadowing.join(" "));
            let mut values = versions.to_vec();
            values.extend([("paths", paths.as_str()), ("rm", rm.as_str())]);
            return Ok(ui.succeeded_with_warning(
                t("Installed, but older copies come first in the PATH"),
                &tf(
                    "node {node}, npm {npm}, yarn {yarn}, composer {composer} are in /usr/bin, but typing the command runs {paths}. Remove those (for example: rm {rm}) if you do not need them.",
                    &values,
                ),
            ));
        }
        Ok(ui.succeeded(
            t("Node.js, Composer and Yarn are installed"),
            &tf("node {node}, npm {npm}, yarn {yarn}, composer {composer}.", &versions),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(node: Option<&str>, yarn: Option<&str>, composer: Option<&str>) -> DevToolsState {
        DevToolsState {
            node_version: node.map(Into::into),
            node_from_node_source: true,
            yarn_version: yarn.map(Into::into),
            composer_version: composer.map(Into::into),
            php_installed: true,
            shadowing: Vec::new(),
        }
    }

    #[test]
    fn missing_tools_in_order() {
        assert_eq!(missing_tools(&state(None, None, None)), ["Node.js", "Composer", "Yarn"]);
        assert_eq!(missing_tools(&state(Some("v26.0.0"), None, Some("2.8"))), ["Yarn"]);
        assert!(missing_tools(&state(Some("v26.0.0"), Some("4.1"), Some("2.8"))).is_empty());
    }
}
