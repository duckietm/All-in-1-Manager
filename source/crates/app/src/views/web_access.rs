//! Docker + Traefik in front of NGINX (port 85) and Polaris (/ws and /api on port 2096). (WebAccessStep.cs)

use std::sync::{Arc, Mutex};

use aio_remote::web_access::{self, WebAccessSettings, NGINX_PORT, POLARIS_PORT};
use aio_remote::{emulator, fail, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::dialogs::Choice;
use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::i18n::{t, tf};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConflictChoice {
    Replace,
    Abort,
}

/// The questions on the intro.
struct Options {
    cloudflare: bool,
    https: bool,
    address: String,
}

pub struct WebAccessStep {
    options: Mutex<Options>,
}

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(WebAccessStep { options: Mutex::new(Options { cloudflare: false, https: true, address: String::new() }) })
}

// A full URL typed in the address box sets http or https, and only its host is kept.
fn settings_from(address: &str, https: bool, cloudflare: bool) -> WebAccessSettings {
    let mut text = address.trim();
    let mut https = https;
    if text.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")) {
        https = true;
        text = &text[8..];
    } else if text.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://")) {
        https = false;
        text = &text[7..];
    }
    let host = text.split('/').next().unwrap_or("").trim().trim_start_matches('[').trim_end_matches(']').to_lowercase();
    WebAccessSettings::new(&host, https, cloudflare)
}

/// `^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$`
fn is_domain_name(text: &str) -> bool {
    if text.is_empty() || text.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = text.split('.').collect();
    let Some((last, rest)) = labels.split_last() else { return false };
    if rest.is_empty() || !(2..=63).contains(&last.len()) || !last.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    rest.iter().all(|label| {
        let bytes = label.as_bytes();
        (1..=63).contains(&bytes.len())
            && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            && bytes[0] != b'-'
            && bytes[bytes.len() - 1] != b'-'
    })
}

fn settings_problem(raw: &str, s: &WebAccessSettings) -> Option<String> {
    if raw.trim().is_empty() {
        return Some(t("Enter the hotel's address: a domain name like example.com, or the server's IP address.").into());
    }
    let ip = s.is_ip_address();
    if s.address.contains(':') && !ip {
        return Some(t("Leave out the port: Traefik answers on 80 (http) and 443 (https).").into());
    }
    if !ip && !is_domain_name(&s.address) {
        return Some(tf("'{address}' is not a valid domain name or IP address.", &[("address", &s.address)]));
    }
    if ip && s.cloudflare {
        return Some(t("Cloudflare works with a domain name, not an IP address.").into());
    }
    if ip && s.https {
        return Some(t("Let's Encrypt only gives certificates for domain names. Use http with an IP address, or enter your domain.").into());
    }
    None
}

fn preview(s: &WebAccessSettings) -> String {
    if s.address.is_empty() {
        return String::new();
    }
    let url = s.url();
    let mut text = format!(
        "{url}  →  NGINX on localhost:{NGINX_PORT}\n{url}/ws  and  {url}/api  →  Polaris on localhost:{POLARIS_PORT}"
    );
    if let Some(alias) = s.www_alias() {
        text.push('\n');
        text.push_str(&tf("{alias}  →  redirects to {url}, once {alias} has a DNS record that points here too", &[("alias", &alias), ("url", &url)]));
    }
    if s.cloudflare {
        text.push('\n');
        text.push_str(t("Cloudflare: set SSL/TLS to \"Full (strict)\" for https, and keep the DNS record proxied (orange cloud)."));
    }
    text
}

impl WebAccessStep {
    fn settings(&self) -> WebAccessSettings {
        let o = self.options.lock().unwrap();
        settings_from(&o.address, o.https, o.cloudflare)
    }
}

fn question(ui: &mut egui::Ui, palette: &Palette, label: &str, inputs: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new(label).size(12.0).strong().color(palette.muted));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            inputs(ui);
        });
    });
}

#[async_trait]
impl InstallStep for WebAccessStep {
    fn title(&self) -> String {
        t("Web access").into()
    }

    fn caption(&self) -> String {
        t("Docker & Traefik").into()
    }

    fn install_label(&self) -> String {
        t("Set up web access").into()
    }

    fn rerun_label(&self) -> String {
        t("Set up again").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Globe,
            heading: t("Web access with Traefik").into(),
            description: t("How visitors reach the hotel: Traefik answers on ports 80 and 443 and passes everything on.").into(),
            bullets: vec![
                t("Installs Docker Engine from the official Docker repository (docs.docker.com/engine/install); its signing key is checked.").into(),
                tf("Runs Traefik v3 in Docker ({directory}). With https it gets and renews a Let's Encrypt certificate.", &[("directory", web_access::traefik_directory())]),
                tf("NGINX now only listens on localhost:{port}: Traefik sends the site there.", &[("port", NGINX_PORT)]),
                tf(
                    "/ws (the game websocket) and /api go straight to Polaris on localhost:{port}; the address is added to the emulator's whitelist.",
                    &[("port", POLARIS_PORT)],
                ),
            ],
            footnote: Some(
                t("With https the domain must already point to this server (or to Cloudflare), and port 80 must be reachable from the internet for Let's Encrypt.").into(),
            ),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            t("Install Docker Engine").into(),
            tf("NGINX site on localhost:{port}", &[("port", NGINX_PORT)]),
            t("Start Traefik").into(),
            t("Polaris: whitelist and visitor IP").into(),
            t("Test the site, /api and /ws").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut guard = self.options.lock().unwrap();
        let o = &mut *guard;
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.y = 14.0;
        question(ui, palette, t("Do you use Cloudflare?"), |ui| {
            ui.radio_value(&mut o.cloudflare, false, t("No"));
            ui.radio_value(&mut o.cloudflare, true, t("Yes, the domain runs through Cloudflare"));
        });
        question(ui, palette, t("http or https?"), |ui| {
            ui.radio_value(&mut o.https, true, t("https (Let's Encrypt certificate)"));
            ui.radio_value(&mut o.https, false, "http");
        });
        question(ui, palette, t("The hotel's address (domain name or IP)"), |ui| {
            ui.add(egui::TextEdit::singleline(&mut o.address).hint_text("example.com  or  203.0.113.10").desired_width(380.0));
        });
        let text = preview(&settings_from(&o.address, o.https, o.cloudflare));
        if !text.is_empty() {
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(palette.subtle)).wrap());
        }
    }

    fn validate(&self) -> Option<String> {
        let mut o = self.options.lock().unwrap();
        let s = settings_from(&o.address, o.https, o.cloudflare);
        if let Some(problem) = settings_problem(&o.address, &s) {
            return Some(problem);
        }
        // Keep the radio buttons in line with a scheme typed in the address box.
        o.https = s.https;
        o.address = s.address;
        None
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = web_access::inspect(server.as_ref()).await?;
        if let Some(configured) = &state.configured {
            let mut o = self.options.lock().unwrap();
            if o.address.trim().is_empty() {
                o.address = configured.address.clone();
                o.https = configured.https;
                o.cloudflare = configured.cloudflare;
            }
        }

        let docker = state.docker_version.clone().unwrap_or_default();
        if state.is_complete() {
            let url = state.configured.as_ref().map(|c| c.url()).unwrap_or_default();
            ui.detected(
                ResultKind::Success,
                &tf("Traefik is running for {url}", &[("url", &url)]),
                &format!(
                    "{} {}",
                    tf(
                        "Docker {docker}, Traefik in {traefik}, NGINX on localhost:{port}.",
                        &[("docker", &docker), ("traefik", web_access::traefik_directory()), ("port", NGINX_PORT)]
                    ),
                    t("You can skip this step, or set it up again (for example with another address).")
                ),
            );
            return Ok(true);
        }
        if !state.conflicting_packages.is_empty() {
            ui.detected(
                ResultKind::Warning,
                t("Docker from the distribution is installed"),
                &tf(
                    "{packages} conflict with Docker Engine; the step asks before it replaces them.",
                    &[("packages", &state.conflicting_packages.join(", "))],
                ),
            );
        } else if state.docker_version.is_some() {
            ui.detected(
                ResultKind::Info,
                &tf("Docker {version} is installed", &[("version", &docker)]),
                t("Answer the questions below to set up Traefik."),
            );
        } else {
            ui.detected(ResultKind::Info, t("Docker and Traefik are not installed yet"), t("Answer the questions below, then set up web access."));
        }
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let mut settings = self.settings();

        // Traefik takes ports 80/443; NGINX moves to 85.
        log("> Checking the server ...");
        let state = web_access::inspect(server).await?;
        // The CMS addon stays in place (CMS on the site, Octane on /client).
        if let Some(configured) = &state.configured {
            settings.atom_cms = configured.atom_cms;
            settings.custom_cms = configured.custom_cms.clone();
            settings.cms_port = configured.cms_port;
            settings.epic_next = configured.epic_next;
        }
        log(&format!("  {}, Cloudflare: {}", settings.url(), if settings.cloudflare { "yes" } else { "no" }));
        match &state.docker_version {
            None => log("  Docker Engine is not installed yet."),
            Some(version) => log(&format!("  Docker {version} is installed.")),
        }
        for (port, owner) in [("80", &state.port80_process), ("443", &state.port443_process)] {
            let Some(owner) = owner else { continue };
            log(&format!("  Port {port} is in use by: {owner}"));
            if !matches!(owner.as_str(), "nginx" | "traefik" | "docker-proxy") {
                fail!("Port {port} is in use by '{owner}', and Traefik needs it. Stop and disable it first (for example: systemctl disable --now {owner}), then try again.");
            }
        }
        if !state.emulator_running {
            log(&format!("  Nothing listens on port {POLARIS_PORT} yet: /ws and /api only answer once the emulator runs."));
        }

        if !state.conflicting_packages.is_empty() {
            let choice = ui
                .choose(
                    t("Docker already installed"),
                    t("Docker from Ubuntu/Debian is installed"),
                    &tf(
                        "{packages} conflict with Docker Engine from docker.com (docs.docker.com: \"Uninstall old versions\").",
                        &[("packages", &state.conflicting_packages.join(", "))],
                    ),
                    ConflictChoice::Abort,
                    vec![
                        Choice::new(
                            t("Replace it with Docker Engine"),
                            t("Removes those packages and installs Docker Engine. Images, containers and volumes in /var/lib/docker are kept."),
                            ConflictChoice::Replace,
                        )
                        .primary(),
                        Choice::new(t("Abort installation"), t("Changes nothing on the server and disconnects."), ConflictChoice::Abort),
                    ],
                )
                .await;
            if choice == ConflictChoice::Abort {
                return Ok(ui.aborted(&server.login().host));
            }
        }
        ui.done(&settings.url());

        // Also on a re-run, so apt upgrades Docker.
        web_access::install_docker(server, &log).await?;
        ui.done(t("docker-ce, compose plugin"));

        web_access::configure_nginx(server, &settings, &log).await?;
        ui.done(&format!("127.0.0.1:{NGINX_PORT}"));

        web_access::configure_traefik(server, &settings, &log).await?;
        ui.done(if settings.https { t("ports 80 + 443") } else { t("port 80") });

        log("> Polaris: ws.whitelist and ws.ip.header (emulator_settings)");
        let polaris = web_access::configure_polaris(server, &settings, &log).await?;
        ui.done(&if polaris { tf("{address} whitelisted", &[("address", &settings.address)]) } else { t("emulator not set up yet").into() });
        // Behind Cloudflare: its ranges for NGINX, Traefik and Polaris, refreshed weekly.
        web_access::install_cloudflare_refresh(server, &settings, &log).await?;

        // Test through Traefik from the server itself.
        let result = web_access::test(server, &settings, &log).await?;
        ui.done(if result.web_socket_answered { t("site, /api and /ws answer") } else { t("site answers") });

        let url = settings.url();
        let routes = tf(
            "{url} → NGINX (localhost:{nginx}); {url}/ws and /api → Polaris (localhost:{polaris}).",
            &[("url", &url), ("nginx", NGINX_PORT), ("polaris", POLARIS_PORT)],
        );
        let mut problems: Vec<String> = Vec::new();
        if settings.https && !result.certificate_valid {
            problems.push(tf(
                "There is no Let's Encrypt certificate yet ({problem}). Check that {address} points to {target} and that port 80 is open; Traefik keeps trying.",
                &[
                    ("problem", result.certificate_problem.as_deref().unwrap_or("")),
                    ("address", &settings.address),
                    ("target", if settings.cloudflare { t("Cloudflare (proxied)") } else { t("this server") }),
                ],
            ));
        }
        if !result.api_answered || !result.web_socket_answered {
            problems.push(if state.emulator_running || polaris {
                tf(
                    "Polaris did not answer on /api or /ws: check that the emulator runs (Emulator step) and see {logs}/emulator.log.",
                    &[("logs", &emulator::log_directory())],
                )
            } else {
                t("Polaris does not run yet: run the Emulator step, then this step again.").into()
            });
        }
        if !problems.is_empty() {
            return Ok(ui.succeeded_with_warning(t("Traefik is running, with something to check"), &format!("{routes} {}", problems.join(" "))));
        }
        let renew = if settings.https { format!(" {}", t("The certificate renews automatically.")) } else { String::new() };
        Ok(ui.succeeded(
            &tf("The hotel answers on {url}", &[("url", &url)]),
            &format!("{routes}{renew} {}", tf("Settings: {directory}.", &[("directory", web_access::traefik_directory())])),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_url_sets_scheme() {
        let s = settings_from(" HTTPS://Hotel.Example.com/path ", false, false);
        assert_eq!((s.address.as_str(), s.https), ("hotel.example.com", true));
        let s = settings_from("http://[2001:db8::1]/", true, false);
        assert_eq!((s.address.as_str(), s.https), ("2001:db8::1", false));
        assert!(s.is_ip_address());
    }

    #[test]
    fn domains() {
        assert!(is_domain_name("example.com"));
        assert!(is_domain_name("a-b.hotel.co"));
        assert!(!is_domain_name("localhost"));
        assert!(!is_domain_name("-a.com"));
        assert!(!is_domain_name("a..com"));
        assert!(!is_domain_name("a.c0m"));
    }

    #[test]
    fn problems() {
        let check = |text: &str, https: bool, cf: bool| settings_problem(text, &settings_from(text, https, cf));
        assert!(check("", true, false).is_some());
        assert!(check("example.com:8080", true, false).is_some());
        assert!(check("203.0.113.10", true, false).is_some());
        assert!(check("203.0.113.10", false, true).is_some());
        assert!(check("203.0.113.10", false, false).is_none());
        assert!(check("example.com", true, true).is_none());
        assert!(preview(&settings_from("example.com", true, true)).contains("Cloudflare"));
    }
}
