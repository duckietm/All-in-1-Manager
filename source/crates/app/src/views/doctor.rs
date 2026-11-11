//! Health check (Tools → Health check): one button reads the server (aio_remote::doctor, read only) and shows every
//! part of the hotel as a green, yellow or red line, the problems first, with a Fix button that runs the matching tool.

use std::sync::Arc;

use aio_remote::doctor::{self, Facts, Service};
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use super::dashboard::chip;
use crate::gui::theme::Palette;
use crate::gui::{step, widgets, Shared, View};
use crate::i18n::{t, tf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Problem,
    Warning,
    Ok,
}

/// What a Fix button runs: a tool of the Tools page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    RestartEmulator,
    RestartMariaDb,
    RestartNginx,
    RestartTraefik,
    UpdateSystem,
    BackupDatabase,
    EnableFirewall,
}

impl Fix {
    fn label(self) -> &'static str {
        match self {
            Fix::RestartEmulator => t("Restart emulator"),
            Fix::RestartMariaDb => t("Restart MariaDB"),
            Fix::RestartNginx => t("Restart NGINX"),
            Fix::RestartTraefik => t("Restart Traefik"),
            Fix::UpdateSystem => t("Update system"),
            Fix::BackupDatabase => t("Create full database backup"),
            Fix::EnableFirewall => t("Enable firewall"),
        }
    }
}

/// One line of the result.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub status: Status,
    pub title: String,
    pub detail: String,
    pub fix: Option<Fix>,
}

fn line(status: Status, title: &str, detail: String, fix: Option<Fix>) -> Line {
    Line { status, title: title.to_string(), detail, fix }
}

fn size(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gb = bytes as f64 / GB;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn service(lines: &mut Vec<Line>, name: &str, state: Service, fix: Option<Fix>) {
    match state {
        Service::Missing => {}
        Service::Running => lines.push(line(Status::Ok, name, t("Running.").into(), None)),
        Service::Stopped => lines.push(line(Status::Problem, name, t("Not running.").into(), fix)),
    }
}

/// The lines for what the check found: problems first, then warnings, then what is fine.
pub fn classify(facts: &Facts) -> Vec<Line> {
    let mut lines = Vec::new();
    match (facts.emulator, facts.emulator_port) {
        (Service::Running, false) => lines.push(line(
            Status::Warning,
            t("Emulator"),
            t("Running, but port 3000 does not answer: it may still be loading, or it is stuck.").into(),
            Some(Fix::RestartEmulator),
        )),
        (Service::Running, true) => lines.push(line(Status::Ok, t("Emulator"), t("Running; port 3000 answers.").into(), None)),
        (state, _) => service(&mut lines, t("Emulator"), state, Some(Fix::RestartEmulator)),
    }
    service(&mut lines, "MariaDB", facts.mariadb, Some(Fix::RestartMariaDb));
    service(&mut lines, "NGINX", facts.nginx, Some(Fix::RestartNginx));
    service(&mut lines, "PHP-FPM 8.5", facts.php_fpm, Some(Fix::RestartNginx));
    match facts.hotel_status {
        Some(200) => lines.push(line(Status::Ok, t("Hotel site"), t("Answers on 127.0.0.1:85 (HTTP 200).").into(), None)),
        Some(0) => lines.push(line(Status::Problem, t("Hotel site"), t("Does not answer on 127.0.0.1:85.").into(), Some(Fix::RestartNginx))),
        Some(code) => {
            lines.push(line(Status::Problem, t("Hotel site"), tf("Answers HTTP {code} instead of 200.", &[("code", &code.to_string())]), Some(Fix::RestartNginx)))
        }
        None => {}
    }
    if facts.docker == Service::Stopped {
        lines.push(line(Status::Problem, "Docker", t("Not running: Traefik and the addons in Docker are down. On the server: systemctl start docker").into(), None));
    }
    match facts.traefik_running {
        Some(true) => lines.push(line(Status::Ok, "Traefik", t("Running.").into(), None)),
        Some(false) => lines.push(line(Status::Problem, "Traefik", t("Stopped: the hotel cannot be reached from the internet.").into(), Some(Fix::RestartTraefik))),
        None => {}
    }
    if facts.fail2ban == Service::Stopped {
        lines.push(line(Status::Warning, "Fail2Ban", t("Installed, but not running: SSH has no brute-force protection.").into(), None));
    }
    if facts.crowdsec_running == Some(false) {
        lines.push(line(Status::Warning, "CrowdSec", t("Stopped: attackers are not blocked in Traefik.").into(), None));
    }

    if let Some(used) = facts.disk_used_percent {
        let free = facts.disk_free_bytes.map(size).unwrap_or_default();
        let detail = tf("{used}% used, {free} free.", &[("used", &used.to_string()), ("free", &free)]);
        let status = if used >= 90 { Status::Problem } else if used >= 80 { Status::Warning } else { Status::Ok };
        lines.push(line(status, t("Disk"), detail, None));
    }
    if let (Some(total), Some(available)) = (facts.memory_total_bytes, facts.memory_available_bytes) {
        let percent = (available * 100).checked_div(total).unwrap_or(100);
        let detail = tf("{available} of {total} free.", &[("available", &size(available)), ("total", &size(total))]);
        lines.push(line(if percent < 10 { Status::Warning } else { Status::Ok }, t("Memory"), detail, None));
    }
    if let (Some(load), Some(cores)) = (facts.load, facts.cores) {
        let detail = tf("Load {load} on {cores} cores.", &[("load", &format!("{load:.2}")), ("cores", &cores.to_string())]);
        lines.push(line(if load > cores as f64 * 1.5 { Status::Warning } else { Status::Ok }, "CPU", detail, None));
    }
    if let Some(address) = &facts.https_address {
        match facts.certificate_days {
            Some(days) if days < 7 => lines.push(line(
                Status::Problem,
                t("Certificate"),
                tf("{address}: expires in {days} days. Let's Encrypt renews it from 30 days before: is port 80 reachable?", &[("address", address), ("days", &days.to_string())]),
                Some(Fix::RestartTraefik),
            )),
            Some(days) if days < 21 => lines.push(line(
                Status::Warning,
                t("Certificate"),
                tf("{address}: expires in {days} days and should have been renewed by now.", &[("address", address), ("days", &days.to_string())]),
                Some(Fix::RestartTraefik),
            )),
            Some(days) => lines.push(line(Status::Ok, t("Certificate"), tf("{address}: valid for {days} more days.", &[("address", address), ("days", &days.to_string())]), None)),
            None => lines.push(line(Status::Warning, t("Certificate"), tf("{address}: the certificate could not be read on port 443.", &[("address", address)]), None)),
        }
    }
    if facts.mariadb != Service::Missing {
        match facts.backup_age_days {
            None => lines.push(line(Status::Warning, t("Database backup"), t("No backup of the hotel database yet.").into(), Some(Fix::BackupDatabase))),
            Some(days) if days > 7 => lines.push(line(
                Status::Warning,
                t("Database backup"),
                tf("The newest backup is {days} days old.", &[("days", &days.to_string())]),
                Some(Fix::BackupDatabase),
            )),
            Some(0) => lines.push(line(Status::Ok, t("Database backup"), t("The newest backup is from today.").into(), None)),
            Some(days) => lines.push(line(Status::Ok, t("Database backup"), tf("The newest backup is {days} days old.", &[("days", &days.to_string())]), None)),
        }
    }
    match (facts.security_updates, facts.updates) {
        (Some(security), _) if security > 0 => lines.push(line(
            Status::Warning,
            t("Updates"),
            tf("{security} security updates waiting.", &[("security", &security.to_string())]),
            Some(Fix::UpdateSystem),
        )),
        (_, Some(count)) if count > 0 => {
            lines.push(line(Status::Ok, t("Updates"), tf("{count} updates waiting, none for security.", &[("count", &count.to_string())]), Some(Fix::UpdateSystem)))
        }
        (_, Some(_)) => lines.push(line(Status::Ok, t("Updates"), t("The system is up to date.").into(), None)),
        _ => {}
    }
    if facts.reboot_required {
        lines.push(line(Status::Warning, t("Reboot"), t("The server needs a reboot for its updates (a new kernel). In the SSH terminal: sudo reboot").into(), None));
    }
    match (facts.ufw_installed, facts.ufw_active) {
        (true, true) => lines.push(line(Status::Ok, t("Firewall"), t("ufw is on.").into(), None)),
        (true, false) => lines.push(line(Status::Warning, t("Firewall"), t("ufw is off: every port is reachable.").into(), Some(Fix::EnableFirewall))),
        (false, _) => lines.push(line(Status::Warning, t("Firewall"), t("ufw is not installed: the Harden step sets it up.").into(), None)),
    }
    match facts.clock_synchronized {
        Some(true) => lines.push(line(Status::Ok, t("Clock"), t("Synchronised (NTP).").into(), None)),
        Some(false) => lines.push(line(Status::Warning, t("Clock"), t("Not synchronised: certificates and apt need the right time.").into(), None)),
        None => {}
    }
    match facts.cpu_missing.as_deref() {
        Some([]) => lines.push(line(Status::Ok, t("Instruction set"), t("x86-64-v2 or newer.").into(), None)),
        Some(missing) => lines.push(line(
            Status::Problem,
            t("Instruction set"),
            tf(
                "Not x86-64-v2 (missing: {features}). Building the AtomCMS theme and EpicNext, and the All-in-1 converter, stop with \"Illegal instruction\" or refuse to start. On a VPS: ask the host to set the CPU type to \"host\" (or x86-64-v2 or newer).",
                &[("features", &missing.join(", "))],
            ),
            None,
        )),
        None => {}
    }
    lines.sort_by_key(|l| l.status);
    lines
}

#[derive(Default)]
struct State {
    checking: bool,
    result: Option<Result<Vec<Line>, String>>,
    at: Option<chrono::DateTime<chrono::Local>>,
    /// The login the result belongs to.
    session: Option<usize>,
}

pub struct DoctorView {
    state: Arc<Mutex<State>>,
    fix: Option<Fix>,
}

fn session_id(shared: &Shared) -> Option<usize> {
    shared.session().map(|s| Arc::as_ptr(&s) as *const () as usize)
}

impl DoctorView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())), fix: None }
    }

    /// The Fix that was clicked, once (the Tools page runs it).
    pub fn take_fix(&mut self) -> Option<Fix> {
        self.fix.take()
    }

    pub fn check(&self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        {
            let mut state = self.state.lock();
            if state.checking {
                return;
            }
            state.checking = true;
            state.session = session_id(shared);
        }
        let state = self.state.clone();
        shared.spawn(async move {
            let result = doctor::check(server.as_ref()).await.map(|facts| classify(&facts)).map_err(|e| step::error_message(&e));
            let mut state = state.lock();
            state.result = Some(result);
            state.at = Some(chrono::Local::now());
            state.checking = false;
        });
    }
}

impl View for DoctorView {
    fn session_changed(&mut self, _shared: &Shared) {
        *self.state.lock() = State::default();
    }

    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        // The first look at the page (or a new login) checks at once.
        let (checking, result, at, stale) = {
            let state = self.state.lock();
            (state.checking, state.result.clone(), state.at, state.session != session_id(shared))
        };
        if stale && !checking {
            self.check(shared);
        }
        widgets::card(ui, palette, egui::Margin::symmetric(24, 20), |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(widgets::title_text(t("Health check"), 17.0));
                    let when = at.map(|a| tf("Checked at {time}. Nothing on the server is changed by the check.", &[("time", &a.format("%H:%M:%S").to_string())]));
                    widgets::muted(ui, palette, &when.unwrap_or_else(|| t("Reads the services, disk, memory, certificate, backups, updates, firewall and clock.").into()), 12.5);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!checking, widgets::primary_button(t("Check again"))).clicked() {
                        self.check(shared);
                    }
                    if checking {
                        ui.spinner();
                    }
                });
            });
            ui.add_space(10.0);
            match &result {
                None => {
                    ui.label(RichText::new(t("Checking the server ...")).color(palette.muted));
                }
                Some(Err(error)) => widgets::result_banner(ui, palette, widgets::ResultKind::Error, t("The health check failed"), error),
                Some(Ok(lines)) => {
                    let problems = lines.iter().filter(|l| l.status == Status::Problem).count();
                    let warnings = lines.iter().filter(|l| l.status == Status::Warning).count();
                    let (kind, heading) = match (problems, warnings) {
                        (0, 0) => (widgets::ResultKind::Success, t("Everything looks fine").to_string()),
                        (0, 1) => (widgets::ResultKind::Warning, t("1 thing to look at").to_string()),
                        (0, w) => (widgets::ResultKind::Warning, tf("{count} things to look at", &[("count", &w.to_string())])),
                        (1, 0) => (widgets::ResultKind::Error, t("1 problem").to_string()),
                        (p, 0) => (widgets::ResultKind::Error, tf("{count} problems", &[("count", &p.to_string())])),
                        (1, w) => (widgets::ResultKind::Error, tf("1 problem, {warnings} to look at", &[("warnings", &w.to_string())])),
                        (p, w) => (widgets::ResultKind::Error, tf("{problems} problems, {warnings} to look at", &[("problems", &p.to_string()), ("warnings", &w.to_string())])),
                    };
                    widgets::result_banner(ui, palette, kind, &heading, t("A Fix button runs the matching tool, with its question and log on the Tools page."));
                    ui.add_space(8.0);
                    for item in lines {
                        ui.horizontal(|ui| {
                            let (text, back, fore) = match item.status {
                                Status::Ok => (t("OK"), palette.success_back, palette.success_text),
                                Status::Warning => (t("LOOK AT IT"), palette.warning_back, palette.warning_text),
                                Status::Problem => (t("PROBLEM"), palette.error_back, palette.error_text),
                            };
                            // Fixed columns, so the chips, titles and details line up whatever the chip says.
                            ui.allocate_ui_with_layout(egui::vec2(124.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_min_width(124.0);
                                chip(ui, text, back, fore);
                            });
                            ui.allocate_ui_with_layout(egui::vec2(150.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_min_width(150.0);
                                ui.label(RichText::new(&item.title).size(13.0).strong());
                            });
                            ui.add(egui::Label::new(RichText::new(&item.detail).size(12.5).color(palette.muted)).wrap());
                            if let Some(fix) = item.fix.filter(|_| item.status != Status::Ok) {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add(widgets::secondary_button(&tf("Fix: {tool}", &[("tool", fix.label())]))).clicked() {
                                        self.fix = Some(fix);
                                    }
                                });
                            }
                        });
                        ui.add_space(4.0);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(output: &str) -> Facts {
        doctor::parse(output)
    }

    #[test]
    fn problems_come_first_with_their_fix() {
        let lines = classify(&facts(
            "emulator=failed\nmariadb=active\nnginx=active\nhotel=502\ndisk_used=93\ndisk_free=1073741824\nufw_installed=yes\nufw=yes\nupdates=0\nsecurity=0\n",
        ));
        assert_eq!(lines[0].status, Status::Problem);
        let emulator = lines.iter().find(|l| l.title == "Emulator").unwrap();
        assert_eq!((emulator.status, emulator.fix), (Status::Problem, Some(Fix::RestartEmulator)));
        let site = lines.iter().find(|l| l.title == "Hotel site").unwrap();
        assert_eq!(site.fix, Some(Fix::RestartNginx));
        assert!(site.detail.contains("502"));
        assert_eq!(lines.iter().find(|l| l.title == "Disk").unwrap().status, Status::Problem);
        // MariaDB runs and has no backup yet.
        assert_eq!(lines.iter().find(|l| l.title == "Database backup").unwrap().fix, Some(Fix::BackupDatabase));
        assert!(lines.windows(2).all(|w| w[0].status <= w[1].status));
    }

    #[test]
    fn a_healthy_server_is_green() {
        let lines = classify(&facts(
            "emulator=active\nemulator_port=yes\nmariadb=active\nnginx=active\nphp=active\ndocker=active\ntraefik=true\nhotel=200\n\
             disk_used=41\ndisk_free=52613349376\nmem_total=8331239424\nmem_available=5123345408\nload=0.42\ncores=4\n\
             https=hotel.example.com\ncert_days=63\nbackup_age=0\nupdates=0\nsecurity=0\nufw_installed=yes\nufw=yes\nntp=yes\n",
        ));
        assert!(lines.iter().all(|l| l.status == Status::Ok), "{lines:?}");
        assert!(lines.iter().any(|l| l.title == "Certificate"));
    }

    #[test]
    fn certificate_and_updates_thresholds() {
        let lines = classify(&facts("https=hotel.example.com\ncert_days=5\nupdates=4\nsecurity=2\nreboot=yes\n"));
        let certificate = lines.iter().find(|l| l.title == "Certificate").unwrap();
        assert_eq!(certificate.status, Status::Problem);
        let updates = lines.iter().find(|l| l.title == "Updates").unwrap();
        assert_eq!((updates.status, updates.fix), (Status::Warning, Some(Fix::UpdateSystem)));
        assert!(lines.iter().any(|l| l.title == "Reboot"));
        // Services that are not installed are not listed.
        assert!(!lines.iter().any(|l| l.title == "MariaDB" || l.title == "Database backup"));
    }

    #[test]
    fn a_processor_without_x86_64_v2_is_a_problem() {
        let lines = classify(&facts("machine=x86_64\ncpu_flags= fpu sse sse2 pni cx16 lahf_lm hypervisor\n"));
        let cpu = lines.iter().find(|l| l.title == "Instruction set").unwrap();
        assert_eq!(cpu.status, Status::Problem);
        assert!(cpu.detail.contains("SSE4.2, POPCNT"), "{}", cpu.detail);
        let fine = classify(&facts("machine=x86_64\ncpu_flags= pni ssse3 cx16 sse4_1 sse4_2 popcnt lahf_lm\n"));
        assert_eq!(fine.iter().find(|l| l.title == "Instruction set").unwrap().status, Status::Ok);
        assert!(!classify(&facts("machine=aarch64\n")).iter().any(|l| l.title == "Instruction set"));
    }
}
