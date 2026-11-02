//! Firewall, Polaris' websocket whitelist and visitor IP, MariaDB over SSH tunnels, and SYN flood protection.
//! (HardenStep.cs)

use std::sync::{Arc, Mutex};

use aio_core::config_dir;
use aio_remote::harden::{self, HardenState, UFW_LOG, UFW_RSYSLOG};
use aio_remote::{fail, mariadb, web_access, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::icons::Icon;
use crate::gui::step::{error_message, InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

pub struct HardenStep {
    firewall_note: Mutex<String>,
}

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(HardenStep { firewall_note: Mutex::new(String::new()) })
}

fn ssh_ports(state_ports: &[i32], login_port: u16) -> String {
    let mut ports: Vec<i32> = state_ports.to_vec();
    ports.push(i32::from(login_port));
    ports.sort();
    ports.dedup();
    ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
}

fn firewall_note(state: &HardenState, cloudflare: bool, login_port: u16) -> String {
    let ports = ssh_ports(&state.ssh_ports, login_port);
    if cloudflare {
        tf("Cloudflare is used: ports 80 and 443 only accept Cloudflare's IP ranges. SSH: port {ports} (rate limited).", &[("ports", &ports)])
    } else {
        tf("Ports 80 and 443 are open for everyone (Traefik). SSH: port {ports} (rate limited).", &[("ports", &ports)])
    }
}

#[async_trait]
impl InstallStep for HardenStep {
    fn title(&self) -> String {
        t("Harden").into()
    }

    fn caption(&self) -> String {
        t("Firewall & TCP").into()
    }

    fn install_label(&self) -> String {
        t("Harden the server").into()
    }

    fn rerun_label(&self) -> String {
        t("Apply again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shield,
            heading: t("Harden the server").into(),
            description: t("Closes everything that does not need to be reachable from the internet.").into(),
            bullets: vec![
                t("Firewall (ufw): deny incoming, allow outgoing, SSH rate limited (ufw limit), HTTP/HTTPS open; with Cloudflare only for Cloudflare's IP ranges.").into(),
                tf(
                    "Firewall log: ufw logging on, and '& stop' in {rsyslog}, so blocked connections go only to {log} (Debug → Firewall shows it live); rsyslog is restarted.",
                    &[("rsyslog", UFW_RSYSLOG), ("log", UFW_LOG)],
                ),
                t("Closes MariaDB (3306) and the emulator's ports (3000, 2096: the game runs through Traefik on /ws). HeidiSQL and the like connect through an SSH tunnel with your key.").into(),
                t("Polaris: ws.whitelist = only the hotel's address, ws.ip.header = X-Forwarded-For (the emulator is stopped while they are set).").into(),
                t("TCP stack: SYN cookies, a larger SYN backlog and fewer SYN-ACK retries in /etc/sysctl.conf.").into(),
            ],
            footnote: Some(
                t("The firewall starts from a clean rule set; ufw keeps the old rules as /etc/ufw/*.rules.<date>. Afterwards the installer logs in again to check SSH still works.").into(),
            ),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            t("Polaris: whitelist and visitor IP").into(),
            t("MariaDB: root login through an SSH tunnel").into(),
            t("TCP stack (sysctl)").into(),
            t("Firewall (ufw)").into(),
            t("Log in again over SSH").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        let note = self.firewall_note.lock().unwrap().clone();
        if !note.is_empty() {
            ui.add(egui::Label::new(RichText::new(note).size(12.0).color(palette.subtle)).wrap());
        }
        ui.add(
            egui::Label::new(
                RichText::new(t(
                    "MariaDB from your computer: in HeidiSQL choose \"MariaDB or MySQL (SSH tunnel)\", SSH host = the server with your SSH key, database host 127.0.0.1 and the passwords from configuration/database/<server>/mariadb.txt.",
                ))
                .size(11.5)
                .color(palette.subtle),
            )
            .wrap(),
        );
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = harden::inspect(server.as_ref()).await?;
        let Some(site) = state.site.clone() else {
            ui.detected(
                ResultKind::Warning,
                t("The hotel's address is not known yet"),
                t("Run the Web access step first: the firewall and the websocket whitelist depend on it."),
            );
            return Ok(false);
        };
        *self.firewall_note.lock().unwrap() = firewall_note(&state, site.cloudflare, server.login().port);

        if state.is_complete() {
            ui.detected(
                ResultKind::Success,
                t("The server is hardened"),
                &format!(
                    "{} {}",
                    tf(
                        "ufw is active (deny incoming, 3306 closed), the TCP settings are in /etc/sysctl.conf and ws.whitelist is {whitelist}.",
                        &[("whitelist", state.whitelist.as_deref().unwrap_or(""))]
                    ),
                    t("You can skip this step, or apply it again.")
                ),
            );
            return Ok(true);
        }
        let heading = if state.maria_db_open {
            t("MariaDB (3306) is open in the firewall")
        } else if state.ufw_active {
            t("The firewall is on, but not set up by this step")
        } else {
            t("The firewall is off")
        };
        ui.detected(
            if state.maria_db_open { ResultKind::Warning } else { ResultKind::Info },
            heading,
            &tf("Everything below is applied for {url}.", &[("url", &site.url())]),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let login = server.login().clone();
        log("> Checking the server ...");
        let state = harden::inspect(server).await?;
        let Some(site) = state.site.clone() else {
            fail!("The hotel's address is not known: run the Web access step first.");
        };
        log(&format!(
            "  {}, Cloudflare: {}, SSH port(s): {}",
            site.url(),
            if site.cloudflare { "yes" } else { "no" },
            ssh_ports(&state.ssh_ports, login.port)
        ));
        ui.done(&site.url());

        // Only the hotel's own pages may open the websocket.
        let polaris = harden::configure_polaris(server, &site, &log).await?;
        ui.done(&if polaris {
            format!("ws.whitelist = {}", harden::whitelist_host(&site))
        } else {
            t("emulator not set up yet").into()
        });
        // The visitor's real address behind Cloudflare, in NGINX too, kept current weekly.
        web_access::configure_nginx_real_ip(server, &site, &log).await?;
        web_access::install_cloudflare_refresh(server, &site, &log).await?;

        // An SSH tunnel arrives as root@localhost, so that account takes the root password too.
        match config_dir::load_database_credentials(&login.host) {
            None => {
                log(&format!("  No saved MariaDB passwords for {} (MariaDB step skipped?): root@localhost is left as it is.", login.host));
                ui.done(t("skipped"));
            }
            Some(credentials) => {
                mariadb::allow_tunnel_root_login(server, &credentials, &log).await?;
                ui.done(t("root@localhost: socket or password"));
            }
        }

        harden::configure_sysctl(server, &log).await?;
        ui.done(t("SYN cookies on"));

        // Firewall last: the rest needs nothing from the internet.
        harden::configure_firewall(server, &site, &state.ssh_ports, &log).await?;
        ui.done(if site.cloudflare { t("80/443 Cloudflare only") } else { t("80/443 open") });

        // A fresh SSH login proves the firewall still lets SSH in. This session stays open either way.
        log(&format!("> Logging in again to {}:{} through the firewall ...", login.host, login.port));
        match super::ssh_login::fresh_login(server, login.clone()).await {
            Ok(_) => {
                log("  SSH login works.");
                ui.done("OK");
            }
            Err(error) => {
                let message = error_message(&error);
                log(&format!("! {message}"));
                ui.fail();
                return Ok(ui.succeeded_with_warning(
                    t("The server is hardened, but a new SSH login failed"),
                    &tf("Keep this window open and check 'ufw status' on the server ({message}). To undo: ufw disable.", &[("message", &message)]),
                ));
            }
        }

        let web = if site.cloudflare { t("HTTP/HTTPS only from Cloudflare") } else { t("HTTP/HTTPS open") };
        if !polaris {
            return Ok(ui.succeeded_with_warning(
                t("The server is hardened"),
                &tf(
                    "Firewall on ({web}) and TCP settings applied. The emulator is not set up yet: run this step again after the Emulator step for ws.whitelist.",
                    &[("web", web)],
                ),
            ));
        }
        Ok(ui.succeeded(
            t("The server is hardened"),
            &tf(
                "Firewall on ({web}, SSH rate limited, MariaDB closed: connect through an SSH tunnel), ws.whitelist = {whitelist}, SYN flood protection on.",
                &[("web", web), ("whitelist", &harden::whitelist_host(&site))],
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_sorted_without_duplicates() {
        assert_eq!(ssh_ports(&[2222, 22], 22), "22, 2222");
        assert_eq!(ssh_ports(&[], 22), "22");
    }
}
