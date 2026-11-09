//! Fail2ban for SSH: too many failed logins ban the address; the whitelist is never banned. (Fail2BanStep.cs)

use std::sync::Arc;

use aio_remote::fail2ban::{self, Fail2BanSettings};
use aio_remote::{fail, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

/// The settings as typed on the intro.
struct Form {
    max_retry: String,
    find_time: String,
    ban_time: String,
    increment: bool,
    whitelist: String,
    client_ip: Option<String>,
    // The address of this computer is known (after detect): the hint under the whitelist.
    detected: bool,
    filled: bool,
}

impl Form {
    fn fill(&mut self, settings: &Fail2BanSettings) {
        self.max_retry = settings.max_retry.to_string();
        self.find_time = settings.find_time_minutes.to_string();
        self.ban_time = settings.ban_time_minutes.to_string();
        self.increment = settings.increment;
        self.whitelist = settings.whitelist.join("\n");
    }

    /// The settings, or what is wrong with them. (Read)
    fn read(&self) -> Result<Fail2BanSettings, String> {
        let Ok(max_retry) = self.max_retry.trim().parse::<i32>() else {
            return Err(t("Failed logins before a ban: fill in a number.").into());
        };
        let Ok(find_time_minutes) = self.find_time.trim().parse::<i32>() else {
            return Err(t("Counted within: fill in a number of minutes.").into());
        };
        let Ok(ban_time_minutes) = self.ban_time.trim().parse::<i32>() else {
            return Err(t("Ban time: fill in a number of minutes.").into());
        };
        let (whitelist, problem) = fail2ban::parse_whitelist(&self.whitelist);
        if let Some(problem) = problem {
            return Err(problem);
        }
        let settings = Fail2BanSettings { max_retry, find_time_minutes, ban_time_minutes, increment: self.increment, whitelist };
        match fail2ban::settings_problem(&settings) {
            Some(problem) => Err(problem),
            None => Ok(settings),
        }
    }
}

#[derive(Default)]
struct Output {
    settings: Option<Fail2BanSettings>,
    ssh_ports: Vec<i32>,
    client_ip: Option<String>,
}

pub struct Fail2BanStep {
    form: Mutex<Form>,
    output: Mutex<Output>,
}

impl Fail2BanStep {
    pub fn new() -> Self {
        let mut form = Form {
            max_retry: String::new(),
            find_time: String::new(),
            ban_time: String::new(),
            increment: true,
            whitelist: String::new(),
            client_ip: None,
            detected: false,
            filled: false,
        };
        form.fill(&fail2ban::defaults());
        Self { form: Mutex::new(form), output: Mutex::new(Output::default()) }
    }

    pub fn settings(&self) -> Option<Fail2BanSettings> {
        self.output.lock().settings.clone()
    }

    pub fn ssh_ports(&self) -> Vec<i64> {
        self.output.lock().ssh_ports.iter().map(|&p| i64::from(p)).collect()
    }

    pub fn client_ip(&self) -> Option<String> {
        self.output.lock().client_ip.clone()
    }
}

impl Default for Fail2BanStep {
    fn default() -> Self {
        Self::new()
    }
}

/// The line under a whitelist: where this computer comes from, and whether it is on the list. (Also CrowdSec's.)
pub(crate) fn client_hint(client_ip: Option<&str>, whitelist: &str) -> String {
    match client_ip {
        Some(client) => {
            let mut hint = tf("This computer reaches the server from {client}.", &[("client", client)]);
            if !fail2ban::parse_whitelist(whitelist).0.iter().any(|e| e == client) {
                hint += t(" It is not on the whitelist: add it, unless that address changes often.");
            }
            hint
        }
        None => t("Your own IP address could not be found: add it yourself (search \"what is my IP\").").into(),
    }
}

/// "22" when sshd's ports are not known.
fn ports_text(ports: &[i32]) -> String {
    if ports.is_empty() {
        "22".into()
    } else {
        join(ports)
    }
}

fn join(ports: &[i32]) -> String {
    ports.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")
}

fn entries(count: usize) -> &'static str {
    if count == 1 {
        n("{count} whitelist entry; edit them later in Tools → Fail2Ban.")
    } else {
        n("{count} whitelist entries; edit them later in Tools → Fail2Ban.")
    }
}

/// A whitelist editor: monospace, one entry per line. (Also CrowdSec's.)
pub(crate) fn whitelist_editor(ui: &mut egui::Ui, text: &mut String, rows: usize, hint: &str) {
    ui.add(
        egui::TextEdit::multiline(text)
            .font(egui::TextStyle::Monospace)
            .desired_rows(rows)
            .desired_width(420.0_f32.min(ui.available_width()))
            .hint_text(hint),
    );
}

fn number_field(ui: &mut egui::Ui, palette: &Palette, label: &str, value: &mut String, limit: usize) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(12.0).strong().color(palette.muted));
        ui.add(egui::TextEdit::singleline(value).char_limit(limit).desired_width(90.0));
    });
}

#[async_trait]
impl InstallStep for Fail2BanStep {
    fn title(&self) -> String {
        "Fail2Ban".into()
    }

    fn caption(&self) -> String {
        t("SSH brute-force protection").into()
    }

    fn install_label(&self) -> String {
        t("Install Fail2Ban").into()
    }

    fn rerun_label(&self) -> String {
        t("Save and restart").into()
    }

    fn intro(&self) -> Intro {
        Intro {
            icon: Icon::Shield,
            heading: t("Install Fail2Ban").into(),
            description: t("Fail2ban watches the SSH logins and bans an address (in the firewall) after too many failed attempts: password guessing bots are shut out after a few tries.").into(),
            bullets: vec![
                t("Installs fail2ban from the distribution (apt) and starts it at boot.").into(),
                t("Protects SSH on every port sshd listens on, reading the systemd journal (also on Ubuntu 24.04+ and Debian 12+ without auth.log).").into(),
                t("The whitelist is never banned: your own IP address is filled in, so a typo in a password cannot lock you out. Add your office or home network as well.").into(),
                tf("The settings are written to {file}; the distribution's own files stay untouched.", &[("file", fail2ban::CONFIG_FILE)]),
                t("Later: Tools → Fail2Ban to edit the whitelist and to unban an address. Running this step again saves new settings.").into(),
            ],
            footnote: Some(t("Works with or without the Harden step's firewall (ufw).").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![t("Check the server").into(), t("Install fail2ban and protect SSH").into(), t("Check the SSH jail").into()]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut form = self.form.lock();
        let form = &mut *form;
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            number_field(ui, palette, t("Failed logins before a ban"), &mut form.max_retry, 3);
            number_field(ui, palette, t("Counted within (minutes)"), &mut form.find_time, 5);
            number_field(ui, palette, t("Ban time (minutes)"), &mut form.ban_time, 6);
        });
        ui.add_space(4.0);
        ui.checkbox(&mut form.increment, t("Ban repeat offenders longer each time (up to a week)"));
        ui.add_space(6.0);
        ui.label(RichText::new(t("Whitelist (never banned)")).size(12.0).strong().color(palette.muted));
        whitelist_editor(ui, &mut form.whitelist, 6, t("One address or network per line, for example\n203.0.113.7\n192.168.1.0/24"));
        if form.detected {
            let hint = client_hint(form.client_ip.as_deref(), &form.whitelist);
            ui.add(egui::Label::new(RichText::new(hint).size(12.0).color(palette.subtle)).wrap());
        }
    }

    fn validate(&self) -> Option<String> {
        self.form.lock().read().err()
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = fail2ban::inspect(server.as_ref()).await?;
        {
            let mut output = self.output.lock();
            output.client_ip = state.client_ip.clone();
            output.ssh_ports = state.ssh_ports.clone();
        }
        {
            let mut form = self.form.lock();
            if !form.filled {
                // The settings on the server, or the defaults with this computer's address on the whitelist.
                let settings = state.settings.clone().unwrap_or_else(|| Fail2BanSettings {
                    whitelist: state.client_ip.iter().cloned().collect(),
                    ..fail2ban::defaults()
                });
                form.fill(&settings);
                form.filled = true;
            }
            form.client_ip = state.client_ip.clone();
            form.detected = true;
        }
        ui.wake();
        let ports = ports_text(&state.ssh_ports);
        if state.is_complete() {
            self.output.lock().settings = state.settings.clone();
            ui.detected(
                ResultKind::Success,
                &tf(
                    "Fail2Ban {version} protects SSH (port {ports})",
                    &[("version", state.version.as_deref().unwrap_or("")), ("ports", &ports)],
                ),
                &tf(
                    "{banned} address(es) banned now, {total} in total since the last restart. You can skip this step, or save new settings.",
                    &[("banned", &state.banned.len().to_string()), ("total", &state.total_banned.to_string())],
                ),
            );
            return Ok(true);
        }
        if state.installed {
            ui.detected(
                ResultKind::Info,
                t("Fail2ban is installed, but not set up by the installer"),
                &tf(
                    "Its SSH jail is set up with the settings below (port {ports}); the distribution's other jails stay as they are.",
                    &[("ports", &ports)],
                ),
            );
        } else {
            ui.detected(ResultKind::Info, t("Fail2ban will be installed"), &tf("From apt, protecting SSH on port {ports}.", &[("ports", &ports)]));
        }
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
        let state = fail2ban::inspect(server).await?;
        let ssh_ports = if state.ssh_ports.is_empty() { vec![i32::from(server.login().port)] } else { state.ssh_ports.clone() };
        {
            let mut output = self.output.lock();
            output.ssh_ports = ssh_ports.clone();
            output.client_ip = state.client_ip.clone();
        }
        let ports = join(&ssh_ports);
        log(&format!(
            "  SSH on port {ports}; {}",
            if state.installed {
                format!("fail2ban {} is installed", state.version.as_deref().unwrap_or(""))
            } else {
                "fail2ban is not installed yet".to_string()
            }
        ));
        if let Some(client) = state.client_ip.as_ref().filter(|c| !settings.whitelist.contains(c)) {
            log(&format!("  Note: this computer ({client}) is not on the whitelist"));
        }
        ui.done(&tf("port {ports}", &[("ports", &ports)]));

        fail2ban::install(server, &settings, &ssh_ports, &log).await?;
        ui.done(t("running"));

        let after = fail2ban::inspect(server).await?;
        if !after.is_complete() {
            fail!("Fail2ban does not run with the SSH jail: see journalctl -u fail2ban.");
        }
        ui.done(&tf("{count} banned", &[("count", &after.banned.len().to_string())]));

        self.output.lock().settings = Some(settings.clone());
        let count = settings.whitelist.len();
        Ok(ui.succeeded(
            &tf("Fail2Ban protects SSH (port {ports})", &[("ports", &ports)]),
            &format!(
                "{} {}",
                tf(
                    "{retry} failed logins within {find} minutes ban an address for {ban} minutes.",
                    &[
                        ("retry", &settings.max_retry.to_string()),
                        ("find", &settings.find_time_minutes.to_string()),
                        ("ban", &settings.ban_time_minutes.to_string()),
                    ],
                ),
                tf(entries(count), &[("count", &count.to_string())])
            ),
        ))
    }
}

pub fn wizard(shared: &Shared) -> Wizard {
    let step = Arc::new(Fail2BanStep::new());
    let finish_step = step.clone();
    Wizard::new(
        shared.runtime.clone(),
        &shared.ctx,
        &shared.dialogs,
        vec![step as Arc<dyn InstallStep>],
        false,
        Arc::new(move |info| super::finished::fail2ban(finish_step.settings(), finish_step.ssh_ports(), finish_step.client_ip(), info.steps)),
        t("SSH protected"),
        t("Completed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(retry: &str, find: &str, ban: &str, whitelist: &str) -> Form {
        Form {
            max_retry: retry.into(),
            find_time: find.into(),
            ban_time: ban.into(),
            increment: true,
            whitelist: whitelist.into(),
            client_ip: None,
            detected: false,
            filled: false,
        }
    }

    #[test]
    fn read_checks_numbers_whitelist_and_ranges() {
        let settings = form(" 5 ", "10", "60", "203.0.113.7\n192.168.1.0/24").read().unwrap();
        assert_eq!(settings.max_retry, 5);
        assert_eq!(settings.whitelist, vec!["203.0.113.7", "192.168.1.0/24"]);
        assert_eq!(form("x", "10", "60", "").read().unwrap_err(), "Failed logins before a ban: fill in a number.");
        assert_eq!(form("5", "", "60", "").read().unwrap_err(), "Counted within: fill in a number of minutes.");
        assert_eq!(form("5", "10", "1.5", "").read().unwrap_err(), "Ban time: fill in a number of minutes.");
        assert!(form("5", "10", "60", "nope").read().unwrap_err().contains("is not an IP address"));
        assert_eq!(form("0", "10", "60", "").read().unwrap_err(), "Failed logins before a ban: a number from 1 to 100.");
    }

    #[test]
    fn defaults_fill_the_form() {
        let step = Fail2BanStep::new();
        let form = step.form.lock();
        assert_eq!((form.max_retry.as_str(), form.find_time.as_str(), form.ban_time.as_str()), ("5", "10", "60"));
        assert!(form.increment);
    }

    #[test]
    fn hint_says_whether_this_computer_is_whitelisted() {
        assert_eq!(client_hint(Some("1.2.3.4"), "1.2.3.4"), "This computer reaches the server from 1.2.3.4.");
        assert!(client_hint(Some("1.2.3.4"), "5.6.7.8").ends_with("unless that address changes often."));
        assert!(client_hint(None, "").starts_with("Your own IP address could not be found"));
    }

    #[test]
    fn ports_and_plural() {
        assert_eq!(ports_text(&[]), "22");
        assert_eq!(ports_text(&[22, 2222]), "22, 2222");
        assert_eq!(tf(entries(1), &[("count", "1")]), "1 whitelist entry; edit them later in Tools → Fail2Ban.");
        assert_eq!(tf(entries(0), &[("count", "0")]), "0 whitelist entries; edit them later in Tools → Fail2Ban.");
    }
}
