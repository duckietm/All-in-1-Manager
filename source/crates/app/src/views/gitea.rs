//! Gitea, your own Git server, in Docker behind the hotel's Traefik on a domain of its own. (GiteaStep.cs)

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aio_remote::gitea::{self, GiteaResult, GiteaSettings, GiteaState};
use aio_remote::web_access::WebAccessSettings;
use aio_remote::{fail, local_ports, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use crate::gui::icons::Icon;
use crate::gui::step::{Extra, InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

/// What the intro asks, and what detect found for it.
struct Form {
    domain: String,
    cloudflare: bool,
    admin_user: String,
    admin_email: String,
    site: Option<WebAccessSettings>,
}

/// Set after a run (or a detect that found Gitea), for the finished page.
#[derive(Default)]
struct Output {
    settings: Option<GiteaSettings>,
    result: Option<GiteaResult>,
}

pub struct GiteaStep {
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl GiteaStep {
    pub fn new() -> Self {
        Self {
            form: Mutex::new(Form { domain: String::new(), cloudflare: false, admin_user: "gitadmin".into(), admin_email: String::new(), site: None }),
            output: Mutex::new(Output::default()),
        }
    }

    pub fn settings(&self) -> Option<GiteaSettings> {
        self.output.lock().unwrap().settings.clone()
    }

    pub fn result(&self) -> Option<GiteaResult> {
        self.output.lock().unwrap().result.clone()
    }
}

impl Default for GiteaStep {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the admin's password is kept: next to the MariaDB passwords, so the encrypted settings backup takes it along.
/// (GiteaLoginPath)
pub fn login_path(host: &str) -> PathBuf {
    aio_core::config_dir::database_file(host, "gitea.txt")
}

fn login_text(settings: &GiteaSettings, password: &str) -> String {
    format!(
        "# Gitea admin (All-in-1 Installer)\nurl={}\nuser={}\nemail={}\npassword={password}\n",
        settings.url(),
        settings.admin_user,
        settings.admin_email
    )
}

fn save_login(host: &str, settings: &GiteaSettings, password: &str) -> std::io::Result<()> {
    aio_core::config_dir::write_private(&login_path(host), &login_text(settings, password))
}

/// Before anything else: without the A record Let's Encrypt cannot give Gitea a certificate. (ShowDnsWarning)
fn dns_warning(domain: &str, cloudflare: bool) -> (String, String) {
    let domain = match domain.trim() {
        "" => "git.example.com",
        d => d,
    };
    let middle = if cloudflare {
        t(", and in Cloudflare turn its proxy on (the orange cloud). ")
    } else {
        t(". With Cloudflare, turn its proxy on (the orange cloud) and choose Yes below. ")
    };
    (
        tf("First add {domain} to your DNS as an A record", &[("domain", domain)]),
        format!(
            "{}{middle}{}",
            tf("Point the A record of {domain} to this server's IP address", &[("domain", domain)]),
            t("Do this before installing: without it Let's Encrypt cannot give Gitea its certificate. The step checks it first.")
        ),
    )
}

/// admin@example.com for git.example.com; nothing when the parent is not a domain itself.
fn suggested_email(domain: &str) -> Option<String> {
    let (_, parent) = domain.split_once('.')?;
    parent.contains('.').then(|| format!("admin@{parent}"))
}

/// Why the server is not ready for Gitea, or None. (Missing)
fn missing(state: &GiteaState) -> Option<&'static str> {
    if state.site.is_none() {
        Some(n("Run the Web access step first: Gitea uses its Docker and Traefik."))
    } else if !state.docker_running {
        Some(n("Docker does not run: run the Web access step first."))
    } else if !state.traefik_running {
        Some(n("Traefik does not run: run the Web access step first."))
    } else {
        None
    }
}

fn label(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.0).strong().color(palette.muted));
}

fn hint(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(palette.subtle)).wrap());
}

#[async_trait]
impl InstallStep for GiteaStep {
    fn title(&self) -> String {
        "Gitea".into()
    }

    fn caption(&self) -> String {
        t("Your own Git server").into()
    }

    fn install_label(&self) -> String {
        t("Install Gitea").into()
    }

    fn rerun_label(&self) -> String {
        t("Update Gitea").into()
    }

    fn intro(&self) -> Intro {
        let first = local_ports::FIRST.to_string();
        let last = local_ports::LAST.to_string();
        Intro {
            icon: Icon::Git,
            heading: t("Install Gitea").into(),
            description: t("Gitea (gitea.com) is a light, easy Git server of your own, like a small GitHub: repositories, issues and pull requests for your hotel's code, on a domain of its own such as git.example.com.").into(),
            bullets: vec![
                tf(
                    "Runs Gitea in Docker ({directory}, image {image}) with SQLite: no database to set up. Repositories and settings stay in {directory}/data.",
                    &[("directory", gitea::DIRECTORY), ("image", gitea::IMAGE)],
                ),
                tf(
                    "Its web port is only on 127.0.0.1 (the first free port from {first} to {last}); Traefik answers for the domain and gets a Let's Encrypt certificate, as for the hotel.",
                    &[("first", &first), ("last", &last)],
                ),
                t("Clone and push over https (with your Gitea password or a token). Gitea's SSH stays off: Cloudflare's proxy does not pass SSH, and the server's own SSH stays apart.").into(),
                t("Creates the admin account with a random password, shown at the end. Registration is off (no Register button): the admin makes the accounts. The start page is the repository list (/explore/repos).").into(),
                t("Running this again updates Gitea; the repositories and accounts stay.").into(),
            ],
            footnote: Some(t("Run the Web access step first (Docker and Traefik).").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server and the domain").into(),
            t("Gitea in Docker and the Traefik route").into(),
            t("Test through Traefik").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock().unwrap();
        ui.add_space(8.0);
        let (heading, message) = dns_warning(&form.domain, form.cloudflare);
        widgets::result_banner(ui, palette, ResultKind::Warning, &heading, &message);
        ui.add_space(8.0);
        label(ui, palette, t("Domain name for Gitea"));
        ui.add(egui::TextEdit::singleline(&mut form.domain).hint_text("git.example.com").desired_width(320.0));
        hint(ui, palette, t("A name of its own, not the hotel's: for a hotel on example.com, use git.example.com."));
        ui.add_space(8.0);
        label(ui, palette, t("Is the domain behind Cloudflare?"));
        ui.radio_value(&mut form.cloudflare, true, t("Yes: the domain is in Cloudflare, with its proxy on (orange cloud)"));
        ui.radio_value(&mut form.cloudflare, false, t("No: the A record points straight to this server"));
        ui.add_space(8.0);
        label(ui, palette, t("Admin account"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut form.admin_user).hint_text("gitadmin").char_limit(40).desired_width(220.0));
            ui.add_space(4.0);
            ui.add(egui::TextEdit::singleline(&mut form.admin_email).hint_text("you@example.com").desired_width(320.0));
        });
        hint(ui, palette, t("Its password is made at random and shown at the end. Gitea keeps the name \"admin\" for itself."));
    }

    fn validate(&self) -> Option<String> {
        let mut form = self.form.lock().unwrap();
        form.domain = form.domain.trim().trim_end_matches('.').to_lowercase();
        if let Some(problem) = gitea::domain_problem(&form.domain, form.site.as_ref()) {
            return Some(problem);
        }
        form.admin_user = form.admin_user.trim().to_string();
        if let Some(problem) = gitea::user_problem(&form.admin_user) {
            return Some(problem);
        }
        form.admin_email = form.admin_email.trim().to_string();
        gitea::email_problem(&form.admin_email)
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = gitea::inspect(server.as_ref()).await?;
        {
            let mut form = self.form.lock().unwrap();
            form.site = state.site.clone();
            if form.domain.trim().is_empty() {
                form.domain = state.domain.clone().unwrap_or_else(|| gitea::suggested_domain(state.site.as_ref()));
            }
            if state.cloudflare.or(state.site.as_ref().map(|s| s.cloudflare)).unwrap_or(false) {
                form.cloudflare = true;
            }
            if form.admin_email.trim().is_empty() {
                if let Some(email) = suggested_email(&form.domain) {
                    form.admin_email = email;
                }
            }
        }
        ui.wake();
        if let Some(missing) = missing(&state) {
            ui.detected(ResultKind::Warning, t("The server is not ready for Gitea yet"), t(missing));
            return Ok(false);
        }
        let site = state.site.clone().unwrap_or_default();
        let domain = state.domain.clone().unwrap_or_default();
        if state.installed() {
            // Skipped as installed: the finish page and its Open button still need Gitea's address.
            self.output.lock().unwrap().settings = Some(GiteaSettings {
                domain: domain.clone(),
                https: site.https,
                cloudflare: state.cloudflare.unwrap_or(false),
                admin_user: String::new(),
                admin_email: String::new(),
            });
            ui.detected(
                ResultKind::Success,
                &tf(
                    "Gitea {version} runs on {scheme}://{domain}",
                    &[("version", state.version.as_deref().unwrap_or("")), ("scheme", site.scheme()), ("domain", &domain)],
                ),
                t("You can skip this step, or update Gitea (its repositories and accounts stay)."),
            );
            return Ok(true);
        }
        ui.detected(
            ResultKind::Info,
            t("Gitea will be installed"),
            &tf(
                "In Docker next to Traefik, {how}.",
                &[("how", if site.https { t("with a Let's Encrypt certificate") } else { t("over http, like the hotel") })],
            ),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let host = server.login().host.clone();
        log("> Checking the server ...");
        let state = gitea::inspect(server).await?;
        if let Some(missing) = missing(&state) {
            fail!("{missing}");
        }
        let https = state.site.as_ref().map(|s| s.https).unwrap_or(false);
        let settings = {
            let form = self.form.lock().unwrap();
            GiteaSettings {
                domain: form.domain.trim().to_string(),
                https,
                cloudflare: form.cloudflare,
                admin_user: form.admin_user.trim().to_string(),
                admin_email: form.admin_email.trim().to_string(),
            }
        };
        log(&format!("  {}, Cloudflare: {}, Docker and Traefik run", settings.url(), if settings.cloudflare { "yes" } else { "no" }));
        gitea::check_dns(server, &settings, &log).await?;
        ui.done(&settings.domain);

        let result = gitea::install(server, &settings, &log).await?;
        ui.done(&tf("port {port}", &[("port", &result.port.to_string())]));

        let certificate = gitea::test(server, &settings, &log).await?;
        ui.done(if certificate { t("OK") } else { t("no certificate yet") });

        {
            let mut output = self.output.lock().unwrap();
            output.settings = Some(settings.clone());
            output.result = Some(GiteaResult { certificate_valid: certificate, ..result.clone() });
        }
        if let Some(password) = &result.admin_password {
            save_login(&host, &settings, password)?;
            ui.add_extra(Extra::Copy { label: t("Password").into(), value: password.clone() });
        }
        let login = if result.admin_password.is_some() {
            tf(
                "Log in as {user} with the password below (also saved on this computer in {path}).",
                &[("user", &settings.admin_user), ("path", &login_path(&host).display().to_string())],
            )
        } else {
            t("The admin account was there already and is kept.").to_string()
        };
        Ok(if certificate {
            ui.succeeded(&tf("Gitea runs on {url}", &[("url", &settings.url())]), &login)
        } else {
            ui.succeeded_with_warning(
                &tf("Gitea runs, but {domain} has no certificate yet", &[("domain", &settings.domain)]),
                &tf(
                    "Check the A record of {domain}{proxy}; Traefik keeps trying. {login}",
                    &[("domain", &settings.domain), ("proxy", if settings.cloudflare { t(" (proxy on)") } else { "" }), ("login", &login)],
                ),
            )
        })
    }
}

/// The Gitea addon: the step, and the finish page with the login. Open goes to Gitea's own domain, not the hotel's.
pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(GiteaStep::new());
    let finish_step = step.clone();
    let open_step = step.clone();
    let mut wizard = Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| {
            let login = login_path(&info.host).display().to_string();
            super::finished::gitea(finish_step.settings(), finish_step.result(), login, info.steps)
        }),
        t("Your Git server"),
        t("Completed"),
    );
    wizard.open_url = Some(Arc::new(move || open_step.settings().map(|s| s.url())));
    wizard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_from_the_parent_domain() {
        assert_eq!(suggested_email("git.example.com").as_deref(), Some("admin@example.com"));
        assert_eq!(suggested_email("git.sub.example.org").as_deref(), Some("admin@sub.example.org"));
        assert_eq!(suggested_email("example.com"), None);
        assert_eq!(suggested_email(""), None);
    }

    #[test]
    fn dns_warning_names_the_domain() {
        let (heading, message) = dns_warning("  ", false);
        assert_eq!(heading, "First add git.example.com to your DNS as an A record");
        assert!(message.contains("choose Yes below"));
        let (heading, message) = dns_warning("git.hotel.nl", true);
        assert_eq!(heading, "First add git.hotel.nl to your DNS as an A record");
        assert!(message.starts_with("Point the A record of git.hotel.nl to this server's IP address, and in Cloudflare"));
    }

    #[test]
    fn login_file_as_the_csharp_writes_it() {
        let settings = GiteaSettings { domain: "git.a.com".into(), https: true, cloudflare: false, admin_user: "gitadmin".into(), admin_email: "x@a.com".into() };
        assert_eq!(
            login_text(&settings, "pw"),
            "# Gitea admin (All-in-1 Installer)\nurl=https://git.a.com\nuser=gitadmin\nemail=x@a.com\npassword=pw\n"
        );
    }

    #[test]
    fn missing_needs_web_access_docker_and_traefik() {
        let mut state = GiteaState {
            site: None,
            docker_running: true,
            traefik_running: true,
            domain: None,
            cloudflare: None,
            port: 0,
            running: false,
            version: None,
        };
        assert!(missing(&state).unwrap().starts_with("Run the Web access step first"));
        state.site = Some(WebAccessSettings::new("a.com", true, false));
        assert_eq!(missing(&state), None);
        state.traefik_running = false;
        assert!(missing(&state).unwrap().starts_with("Traefik"));
        state.docker_running = false;
        assert!(missing(&state).unwrap().starts_with("Docker"));
    }
}
