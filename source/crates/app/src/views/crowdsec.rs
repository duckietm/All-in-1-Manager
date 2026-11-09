//! CrowdSec in Docker with Traefik's bouncer, enrolled in the CrowdSec console with the user's key. (CrowdSecStep.cs)

use std::sync::Arc;

use aio_remote::crowdsec::{self, CrowdSecResult, CrowdSecSettings, CrowdSecState};
use aio_remote::web_access::WebAccessSettings;
use aio_remote::{fail, fail2ban, Server};
use async_trait::async_trait;
use eframe::egui::{self, Color32, RichText};
use parking_lot::Mutex;

use super::fail2ban::{client_hint, whitelist_editor};
use crate::gui::icons::Icon;
use crate::gui::step::{Banner, InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{t, tf};

/// What the intro asks, and what detect found for it.
#[derive(Default)]
struct Form {
    key: String,
    name: String,
    whitelist: String,
    enrolled: bool,
    filled: bool,
    detected: bool,
    client_ip: Option<String>,
    cloudflare: Option<Banner>,
}

impl Form {
    /// The settings, or what is wrong with them. (Read)
    fn read(&self) -> Result<CrowdSecSettings, String> {
        let name = self.name.trim().to_string();
        if let Some(problem) = crowdsec::name_problem(&name) {
            return Err(problem);
        }
        if let Some(problem) = crowdsec::key_problem(&self.key, self.enrolled) {
            return Err(problem);
        }
        let (whitelist, problem) = fail2ban::parse_whitelist(&self.whitelist);
        if let Some(problem) = problem {
            return Err(problem);
        }
        Ok(CrowdSecSettings { enroll_key: crowdsec::key_from(&self.key).unwrap_or_default(), instance_name: name, whitelist })
    }
}

#[derive(Default)]
struct Output {
    site: Option<WebAccessSettings>,
    instance_name: Option<String>,
    enrolled_now: bool,
    result: Option<CrowdSecResult>,
}

#[derive(Default)]
pub struct CrowdSecStep {
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl CrowdSecStep {
    pub fn site(&self) -> Option<WebAccessSettings> {
        self.output.lock().site.clone()
    }
    pub fn instance_name(&self) -> Option<String> {
        self.output.lock().instance_name.clone()
    }
    pub fn enrolled_now(&self) -> bool {
        self.output.lock().enrolled_now
    }
    pub fn result(&self) -> Option<CrowdSecResult> {
        self.output.lock().result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Subtle,
    Warning,
    Success,
    Error,
}

/// The line under the key box. (ShowKeyCheck)
fn key_check(text: &str, enrolled: bool) -> (String, Tone) {
    if text.trim().is_empty() {
        if enrolled {
            (t("This server is already enrolled: leave it empty to keep that, or paste a new key to enroll it again.").into(), Tone::Subtle)
        } else {
            (t("Required.").into(), Tone::Warning)
        }
    } else if let Some(key) = crowdsec::key_from(text) {
        (tf("✓ Key found: {key}…", &[("key", &key[..key.len().min(6)])]), Tone::Success)
    } else {
        (t("No key found in that text: copy the key (or the whole command) again from the console.").into(), Tone::Error)
    }
}

/// The hotel's address, as a name the console takes. (DefaultName)
fn default_name(site: Option<&WebAccessSettings>) -> String {
    let name: String = site
        .map(|s| s.address.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(64).collect())
        .unwrap_or_default();
    if name.is_empty() {
        "hotel".into()
    } else {
        name
    }
}

/// How the hotel's domain and Cloudflare fit together. (ShowCloudflare)
fn cloudflare_banner(state: &CrowdSecState) -> Option<Banner> {
    let site = state.site.as_ref()?;
    let dns = state
        .dns_addresses
        .as_ref()
        .map(|ips| tf(" ({address} points to {ips})", &[("address", &site.address), ("ips", ips)]))
        .unwrap_or_default();
    let address = site.address.as_str();
    let banner = |kind, heading: String, message: String| Some(Banner { kind, heading, message });
    if site.is_ip_address() {
        banner(
            ResultKind::Info,
            t("No Cloudflare: the hotel is on an IP address").into(),
            t("Visitors connect straight to this server, so CrowdSec sees their own addresses.").into(),
        )
    } else if state.dns_behind_cloudflare == Some(true) && !site.cloudflare {
        banner(
            ResultKind::Error,
            t("Cloudflare is used, but the hotel is not set up for it").into(),
            tf(
                "{address} is behind Cloudflare's proxy{dns}, but Web access was set up without Cloudflare. Run Install New Server → Web access again with Cloudflare on Yes first: otherwise every visitor looks like Cloudflare, and a ban would block everyone.",
                &[("address", address), ("dns", &dns)],
            ),
        )
    } else if site.cloudflare {
        let not_pointing = state.dns_behind_cloudflare == Some(false);
        let first = if not_pointing {
            tf(
                "{address} does not point to Cloudflare{dns}: turn its proxy on (the orange cloud) in Cloudflare's DNS. CrowdSec works either way. ",
                &[("address", address), ("dns", &dns)],
            )
        } else {
            tf("{address} is behind Cloudflare's proxy{dns}. ", &[("address", address), ("dns", &dns)])
        };
        banner(
            if not_pointing { ResultKind::Warning } else { ResultKind::Success },
            if not_pointing { t("Set up for Cloudflare, but the domain does not point to Cloudflare") } else { t("Cloudflare is used: good") }.into(),
            first + t("CrowdSec and the bouncer take the visitor's real address from Cloudflare's X-Forwarded-For, trusted only from Cloudflare's IP ranges (kept current weekly)."),
        )
    } else {
        let first = if state.dns_behind_cloudflare == Some(false) {
            tf("{address} points straight to this server{dns}. ", &[("address", address), ("dns", &dns)])
        } else {
            String::new()
        };
        banner(
            ResultKind::Info,
            t("No Cloudflare: visitors connect straight to this server").into(),
            first + t("CrowdSec sees the visitors' own addresses."),
        )
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).size(13.0).strong());
}

fn small(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
}

#[async_trait]
impl InstallStep for CrowdSecStep {
    fn title(&self) -> String {
        "CrowdSec".into()
    }

    fn caption(&self) -> String {
        t("Blocks attackers in Traefik").into()
    }

    fn install_label(&self) -> String {
        t("Install CrowdSec").into()
    }

    fn rerun_label(&self) -> String {
        t("Update CrowdSec").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shield,
            heading: t("Install CrowdSec").into(),
            description: t("CrowdSec (crowdsec.net) watches every request to your hotel through Traefik and blocks addresses that attack it: vulnerability scans, probing, known exploits, brute force. Attacks seen by all CrowdSec users are shared, so addresses that attacked others are blocked before they reach you.").into(),
            bullets: vec![
                tf(
                    "Runs the CrowdSec security engine in Docker ({directory}, image {image}). It reads Traefik's access log; its API is only on 127.0.0.1.",
                    &[("directory", crowdsec::directory()), ("image", crowdsec::IMAGE)],
                ),
                t("Adds CrowdSec's bouncer plugin to Traefik, on every route (the hotel, the CMS, Polaris, Gitea): a banned address gets 403 Forbidden.").into(),
                t("Behind Cloudflare it uses the visitor's real address (past Cloudflare's IP ranges), never Cloudflare's: the step checks how your domain is set up.").into(),
                t("The hotel's own files (gamedata, the client) are never counted, so loading a room cannot look like an attack. If CrowdSec stops, the hotel stays reachable.").into(),
                t("Required: a free CrowdSec account and its enrollment key (see 1 and 2 below). The engine then shows in your CrowdSec console, with its alerts.").into(),
                t("Traefik restarts once (players are disconnected for a moment). If the hotel does not answer as before, the previous Traefik configuration comes back by itself.").into(),
            ],
            footnote: Some(t("Run the Web access step first (Docker and Traefik).").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server and Cloudflare").into(),
            t("CrowdSec in Docker").into(),
            t("Connect to the CrowdSec console").into(),
            t("Bouncer in Traefik").into(),
            t("Test").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock();
        let form = &mut *form;
        ui.add_space(8.0);
        if let Some(banner) = &form.cloudflare {
            widgets::result_banner(ui, palette, banner.kind, &banner.heading, &banner.message);
            ui.add_space(4.0);
        }

        heading(ui, t("1. Make a free CrowdSec account"));
        widgets::text(ui, t("Sign up on the CrowdSec console (free: the Community plan is enough). Already have an account? Just log in."), 12.5);
        if ui.add(widgets::secondary_button(t("Open app.crowdsec.net"))).clicked() {
            crate::gui::open_url(crowdsec::CONSOLE_URL);
        }

        heading(ui, t("2. Copy your enrollment key"));
        widgets::text(
            ui,
            t("In the console: Security Engines → Engines → Add Security Engine (on a new account the welcome page shows it right away). It shows a command like: sudo cscli console enroll -e context cm1abc...  Copy it and paste it below: the whole command is fine, the installer takes the key out."),
            12.5,
        );
        ui.add(
            egui::TextEdit::singleline(&mut form.key)
                .hint_text(t("cm1abc...   or the whole command: sudo cscli console enroll -e context cm1abc..."))
                .desired_width(520.0_f32.min(ui.available_width())),
        );
        let (check, tone) = key_check(&form.key, form.enrolled);
        let color = match tone {
            Tone::Subtle => palette.subtle,
            Tone::Warning => palette.warning_text,
            Tone::Success => palette.success_text,
            Tone::Error => palette.error_text,
        };
        small(ui, &check, color);

        heading(ui, t("3. Name of this server in the console"));
        ui.add(egui::TextEdit::singleline(&mut form.name).char_limit(64).desired_width(320.0));

        heading(ui, t("Whitelist (never banned)"));
        whitelist_editor(ui, &mut form.whitelist, 5, t("One address or network per line, for example\n203.0.113.7\n192.168.0.0/24"));
        if form.detected {
            small(ui, &client_hint(form.client_ip.as_deref(), &form.whitelist), palette.subtle);
        }
        small(ui, t("After the install, accept this server in the CrowdSec console: the last page shows how."), palette.subtle);
    }

    fn validate(&self) -> Option<String> {
        self.form.lock().read().err()
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = crowdsec::inspect(server.as_ref()).await?;
        self.output.lock().site = state.site.clone();
        {
            let mut form = self.form.lock();
            form.enrolled = state.enrolled;
            if !form.filled {
                form.name = state.instance_name.clone().unwrap_or_else(|| default_name(state.site.as_ref()));
                let whitelist: Vec<String> = if state.installed { state.whitelist.clone() } else { state.client_ip.iter().cloned().collect() };
                form.whitelist = whitelist.join("\n");
                form.filled = true;
            }
            form.cloudflare = cloudflare_banner(&state);
            form.client_ip = state.client_ip.clone();
            form.detected = true;
        }
        ui.wake();
        if let Some(problem) = crowdsec::problem(&state) {
            ui.detected(ResultKind::Warning, t("The server is not ready for CrowdSec yet"), &problem);
            return Ok(false);
        }
        if state.is_complete() {
            ui.detected(
                ResultKind::Success,
                &tf("CrowdSec {version} protects the hotel through Traefik", &[("version", state.version.as_deref().unwrap_or(""))]),
                &tf(
                    "{count} address(es) banned by this server now. You can skip this step, or update CrowdSec (its data and enrollment stay).",
                    &[("count", &state.banned.len().to_string())],
                ),
            );
            return Ok(true);
        }
        ui.detected(
            ResultKind::Info,
            if state.installed { t("CrowdSec is partly set up: running this step finishes it") } else { t("CrowdSec will be installed") },
            t("In Docker next to Traefik, with the bouncer on every route."),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        log("> Checking the server ...");
        let read = self.form.lock().read();
        let settings = match read {
            Ok(settings) => settings,
            Err(problem) => fail!("{problem}"),
        };
        let state = crowdsec::inspect(server).await?;
        if let Some(problem) = crowdsec::problem(&state) {
            fail!("{problem}");
        }
        let Some(site) = state.site.clone() else { fail!("Run the Web access step first: CrowdSec works with its Docker and Traefik.") };
        log(&format!(
            "  {}, Cloudflare: {}{}; Docker and Traefik run",
            site.url(),
            if site.cloudflare { "yes" } else { "no" },
            state.dns_addresses.as_ref().map(|ips| format!(" ({} → {ips})", site.address)).unwrap_or_default()
        ));
        ui.done(if site.cloudflare { "Cloudflare" } else { t("no Cloudflare") });

        crowdsec::install_engine(server, &settings, &log).await?;
        ui.done(t("running"));

        let enroll_now = !settings.enroll_key.is_empty();
        if enroll_now {
            crowdsec::enroll(server, &settings.enroll_key, &settings.instance_name, &log).await?;
        } else {
            log("  Already enrolled: kept");
        }
        ui.done(if enroll_now { t("accept it in the console") } else { t("kept") });

        crowdsec::configure_traefik(server, &site, &settings.whitelist, &log).await?;
        ui.done(t("every route"));

        let result = crowdsec::test(server, &site, &log).await?;
        ui.done(if result.bouncer_connected { t("connected") } else { t("not connected yet") });

        {
            let mut output = self.output.lock();
            output.site = Some(site.clone());
            output.instance_name = Some(settings.instance_name.clone());
            output.enrolled_now = enroll_now;
            output.result = Some(result);
        }
        let accept = if enroll_now {
            tf(
                " Now accept \"{name}\" in the CrowdSec console (app.crowdsec.net): see the next page.",
                &[("name", &settings.instance_name)],
            )
        } else {
            String::new()
        };
        Ok(if result.bouncer_connected {
            ui.succeeded(t("CrowdSec protects the hotel"), &(t("Every request through Traefik is checked; banned addresses get 403.").to_string() + &accept))
        } else {
            ui.succeeded_with_warning(
                t("CrowdSec runs, but the bouncer has not connected yet"),
                &(t("Traefik's plugin fetches the ban list once a minute: check Debug → Traefik for crowdsec lines.").to_string() + &accept),
            )
        })
    }
}

/// The CrowdSec addon. The finish page's Open button goes to the console, where the new engine is accepted.
pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(CrowdSecStep::default());
    let finish_step = step.clone();
    let mut wizard = Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| {
            let s = &finish_step;
            super::finished::crowdsec(s.site(), s.instance_name(), s.enrolled_now(), s.result(), info.steps)
        }),
        t("Hotel protected"),
        t("Completed"),
    );
    wizard.open_url = Some(Arc::new(|| Some(crowdsec::CONSOLE_URL.to_string())));
    wizard
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(site: Option<WebAccessSettings>, dns: Option<&str>, behind: Option<bool>) -> CrowdSecState {
        CrowdSecState {
            site,
            docker_running: true,
            traefik_running: true,
            installed: false,
            running: false,
            bouncer_configured: false,
            enrolled: false,
            version: None,
            instance_name: None,
            port: 0,
            whitelist: vec![],
            banned: vec![],
            dns_addresses: dns.map(str::to_string),
            dns_behind_cloudflare: behind,
            client_ip: None,
        }
    }

    #[test]
    fn key_check_tones() {
        assert_eq!(key_check(" ", false).1, Tone::Warning);
        assert_eq!(key_check("", true).1, Tone::Subtle);
        let (text, tone) = key_check("sudo cscli console enroll -e context cm1abcdefghijklmnop", false);
        assert_eq!((text.as_str(), tone), ("✓ Key found: cm1abc…", Tone::Success));
        assert_eq!(key_check("hello", false).1, Tone::Error);
    }

    #[test]
    fn default_name_from_the_address() {
        assert_eq!(default_name(None), "hotel");
        assert_eq!(default_name(Some(&WebAccessSettings::new("www.my-hotel.nl", true, false))), "www.my-hotel.nl");
        assert_eq!(default_name(Some(&WebAccessSettings::new("é", true, false))), "hotel");
    }

    #[test]
    fn cloudflare_banners() {
        assert!(cloudflare_banner(&state(None, None, None)).is_none());
        let ip = cloudflare_banner(&state(Some(WebAccessSettings::new("203.0.113.7", false, false)), None, None)).unwrap();
        assert_eq!(ip.heading, "No Cloudflare: the hotel is on an IP address");
        let wrong = cloudflare_banner(&state(Some(WebAccessSettings::new("a.com", true, false)), Some("104.16.0.1"), Some(true))).unwrap();
        assert_eq!(wrong.kind, ResultKind::Error);
        assert!(wrong.message.starts_with("a.com is behind Cloudflare's proxy (a.com points to 104.16.0.1), but"));
        let good = cloudflare_banner(&state(Some(WebAccessSettings::new("a.com", true, true)), None, Some(true))).unwrap();
        assert_eq!(good.kind, ResultKind::Success);
        let off = cloudflare_banner(&state(Some(WebAccessSettings::new("a.com", true, true)), Some("1.2.3.4"), Some(false))).unwrap();
        assert_eq!(off.kind, ResultKind::Warning);
        let direct = cloudflare_banner(&state(Some(WebAccessSettings::new("a.com", true, false)), Some("1.2.3.4"), Some(false))).unwrap();
        assert_eq!(direct.message, "a.com points straight to this server (a.com points to 1.2.3.4). CrowdSec sees the visitors' own addresses.");
    }

    #[test]
    fn read_needs_name_and_key() {
        let mut form = Form { name: "hotel".into(), ..Default::default() };
        assert!(form.read().unwrap_err().contains("it is required"));
        form.enrolled = true;
        assert_eq!(form.read().unwrap().enroll_key, "");
        form.key = "cm1abcdefghijklmnop".into();
        assert_eq!(form.read().unwrap().enroll_key, "cm1abcdefghijklmnop");
        form.name = "bad name".into();
        assert!(form.read().is_err());
    }
}
