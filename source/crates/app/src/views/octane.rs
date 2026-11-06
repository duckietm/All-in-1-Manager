//! Configures and builds the Octane client and serves it on the hotel's address. (OctaneStep.cs)

use std::sync::Arc;

use aio_remote::{emulator, emulator_assets, fail, hotel_site, octane, Server};
use async_trait::async_trait;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

pub struct OctaneStep;

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(OctaneStep)
}

#[async_trait]
impl InstallStep for OctaneStep {
    fn title(&self) -> String {
        "Octane".into()
    }

    fn caption(&self) -> String {
        t("Client build").into()
    }

    fn install_label(&self) -> String {
        t("Set up and build Octane").into()
    }

    fn rerun_label(&self) -> String {
        t("Configure and rebuild").into()
    }

    fn intro(&self) -> Intro {
        let (renderer, octane_dir, gamedata) = (octane::renderer(), octane::octane(), octane::gamedata());
        Intro {
            icon: Icon::Cube,
            heading: t("Set up the Octane client").into(),
            description: t("Configures Octane for the hotel's address (from the web access step), builds it and serves it with NGINX.").into(),
            bullets: vec![
                tf("Runs yarn install in {renderer} and {octane}.", &[("renderer", &renderer), ("octane", &octane_dir)]),
                t("Creates client-mode, hotlooks, infostand_backgrounds, news, renderer-config and ui-config .json from the .example files, with the hotel's URLs (wss:// for https, ws:// for http).").into(),
                tf(
                    "Creates the asset folders those files point to in {gamedata}, ready for the files from the All-in-1 download tool, and a welcome message in news/news.json.",
                    &[("gamedata", &gamedata)],
                ),
                tf(
                    "Runs yarn build and points the hotel site at {dist}; {octane}/build.sh builds it again later.",
                    &[("dist", &octane::dist()), ("octane", &octane_dir)],
                ),
                tf(
                    "Camera: photos in {camera} and generated badges in c_images/album1584 and groupbadge/ (the emulator's camera and imager settings; the emulator is stopped while they are set).",
                    &[("camera", &hotel_site::camera())],
                ),
                tf(
                    "Copies the emulator's Assets/images (Octane's preload images: without them the client does not start) and Assets/badgeparts into {gamedata}; files already there are kept.",
                    &[("gamedata", &gamedata)],
                ),
            ],
            footnote: Some(tf(
                "The Habbo game assets are not copied: download them with Assets → Habbo assets and put them in {gamedata} with Assets → Deploy Assets. Existing configuration files are kept; only their URLs are updated.",
                &[("gamedata", &gamedata)],
            )),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            t("Octane-Renderer: yarn install").into(),
            t("Octane: yarn install").into(),
            t("Configuration files and gamedata folders").into(),
            t("yarn build").into(),
            t("Serve dist/ and test").into(),
            t("Camera and badge imager in the emulator").into(),
            t("The emulator's images and group badge parts").into(),
        ]
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = octane::inspect(server.as_ref()).await?;
        let Some(site) = state.site.clone() else {
            ui.detected(
                ResultKind::Warning,
                t("The hotel's address is not known yet"),
                t("Run the Web access step first: Octane is configured with the address chosen there."),
            );
            return Ok(false);
        };
        if !state.octane_present || !state.renderer_present {
            ui.detected(
                ResultKind::Warning,
                t("Octane or Octane-Renderer is not cloned"),
                &tf("Run the clone steps first ({octane} and {renderer}).", &[("octane", &octane::octane()), ("renderer", &octane::renderer())]),
            );
            return Ok(false);
        }
        let configured = state.configured_socket_url.clone();
        let expected = state.expected_socket_url();
        if state.is_complete() {
            ui.detected(
                ResultKind::Success,
                &tf("Octane is built and served on {url}", &[("url", &site.client_url())]),
                &format!(
                    "socket.url {}. {}",
                    configured.as_deref().unwrap_or(""),
                    t("You can skip this step, or configure and rebuild it (e.g. after a git pull).")
                ),
            );
            return Ok(true);
        }
        if state.built && configured == expected && !state.layout_configured() {
            ui.detected(
                ResultKind::Info,
                &tf("Octane will be rebuilt for {url}", &[("url", &site.client_url())]),
                &if site.has_cms() {
                    tf("{cms} is the site now: Octane moves to /client/.", &[("cms", &site.cms_name().unwrap_or_default())])
                } else {
                    t("Octane is served from the site root.").into()
                },
            );
        } else if state.built && configured != expected {
            ui.detected(
                ResultKind::Info,
                t("Octane is built for another address"),
                &tf(
                    "socket.url is {configured}; it becomes {expected}.",
                    &[("configured", configured.as_deref().unwrap_or(t("not set"))), ("expected", expected.as_deref().unwrap_or(""))],
                ),
            );
        } else {
            let url = site.url();
            ui.detected(
                ResultKind::Info,
                &tf("Octane will be set up for {url}", &[("url", &url)]),
                &format!("socket.url {}, api.url {url}, assets {url}/gamedata.", expected.as_deref().unwrap_or("")),
            );
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let state = octane::inspect(server).await?;
        let Some(site) = state.site.clone() else {
            fail!("The hotel's address is not known: run the Web access step first.");
        };
        if !state.octane_present {
            fail!("{} has no package.json. Run the Octane clone step first.", octane::octane());
        }
        if !state.renderer_present {
            fail!("{} has no package.json. Run the Renderer clone step first.", octane::renderer());
        }
        let Some(node) = state.node_version.clone() else {
            fail!("Node.js is not installed in /usr/bin. Run the Node.js step first.");
        };
        log(&format!("  {}: socket {}, Node.js {node}", site.client_url(), octane::socket_url(&site)));
        if site.has_cms() {
            log(&format!(
                "  {} is the site: Octane is built for {} and opened by the CMS.",
                site.cms_name().unwrap_or_default(),
                site.client_path()
            ));
        }
        ui.done(&site.url());

        // Renderer first: Octane builds against its sources and node_modules.
        octane::yarn_install(server, &octane::renderer(), &log, false).await?;
        ui.done(t("done"));
        octane::yarn_install(server, &octane::octane(), &log, false).await?;
        ui.done(t("done"));

        octane::configure(server, &site, &log, false).await?;
        ui.done(&octane::socket_url(&site));

        octane::build(server, &site, &log).await?;
        ui.done("dist/");

        octane::publish(server, &site, &log).await?;
        ui.done(&site.client_url());

        // The emulator writes camera photos into gamedata/camera.
        let url = site.url();
        let camera = emulator::configure_imager(server, &url, &log).await?;
        ui.done(&if camera { format!("{url}/gamedata/camera/") } else { t("emulator not set up yet").into() });

        let assets = emulator_assets::install(server, &log).await?;
        ui.done(&if assets > 0 { tf("{count} files", &[("count", &assets.to_string())]) } else { t("already there").into() });

        let gamedata = octane::gamedata();
        if !camera {
            return Ok(ui.succeeded_with_warning(
                &tf("Octane is served on {url}", &[("url", &site.client_url())]),
                &tf(
                    "The emulator is not set up yet, so its camera and badge settings are not set: run the Emulator step, then this step again. Copy the game assets into {gamedata} with the All-in-1 download tool.",
                    &[("gamedata", &gamedata)],
                ),
            ));
        }
        Ok(ui.succeeded(
            &tf("Octane is served on {url}", &[("url", &site.client_url())]),
            &tf(
                "Copy the game assets into {gamedata} (the folders are there) with the All-in-1 download tool. After changing {configuration}, run {octane}/build.sh.",
                &[("gamedata", &gamedata), ("configuration", &octane::configuration_directory()), ("octane", &octane::octane())],
            ),
        ))
    }
}
