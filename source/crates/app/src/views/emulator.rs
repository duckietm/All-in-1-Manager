//! Builds the Polaris emulator and runs it as the "emulator" account with its own database account. (EmulatorStep.cs)

use std::sync::Arc;

use aio_core::{config_dir, credentials, layout::ROOT_ACCOUNT};
use aio_remote::emulator::{self, EmulatorState};
use aio_remote::{fail, Server};
use async_trait::async_trait;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::icons::Icon;
use crate::gui::step::{InstallStep, Intro, StepOutcome, StepUi};
use crate::gui::theme::Palette;
use crate::gui::widgets::ResultKind;
use crate::i18n::{n, t, tf};

const MEMORY_CHOICES_MB: [i32; 8] = [1024, 2048, 3072, 4096, 6144, 8192, 12288, 16384];

const DEFAULT_TIMEZONE: &str = "Europe/Amsterdam";

/// Zones offered when the server lists none (the C# falls back to this computer's IANA zones).
const FALLBACK_ZONES: &[&str] = &[
    "Africa/Cairo", "Africa/Johannesburg", "Africa/Lagos", "America/Argentina/Buenos_Aires", "America/Bogota",
    "America/Chicago", "America/Denver", "America/Halifax", "America/Los_Angeles", "America/Mexico_City",
    "America/New_York", "America/Phoenix", "America/Santiago", "America/Sao_Paulo", "America/Toronto",
    "America/Vancouver", "Asia/Bangkok", "Asia/Dubai", "Asia/Hong_Kong", "Asia/Jakarta", "Asia/Jerusalem",
    "Asia/Karachi", "Asia/Kolkata", "Asia/Manila", "Asia/Seoul", "Asia/Shanghai", "Asia/Singapore", "Asia/Taipei",
    "Asia/Tokyo", "Atlantic/Reykjavik", "Australia/Adelaide", "Australia/Brisbane", "Australia/Perth",
    "Australia/Sydney", "Europe/Amsterdam", "Europe/Athens", "Europe/Berlin", "Europe/Brussels", "Europe/Bucharest",
    "Europe/Budapest", "Europe/Copenhagen", "Europe/Dublin", "Europe/Helsinki", "Europe/Istanbul", "Europe/Kyiv",
    "Europe/Lisbon", "Europe/London", "Europe/Madrid", "Europe/Moscow", "Europe/Oslo", "Europe/Paris",
    "Europe/Prague", "Europe/Rome", "Europe/Stockholm", "Europe/Vienna", "Europe/Warsaw", "Europe/Zurich",
    "Pacific/Auckland", "Pacific/Honolulu", "UTC",
];

/// The settings on the intro: timezone, memory and the websocket whitelist.
#[derive(Default)]
struct Options {
    zones: Vec<String>,
    timezone: Option<String>,
    memory_index: usize,
    whitelist: String,
    ram_note: String,
    filter: String,
    popup_open: bool,
}

pub struct EmulatorStep {
    options: Mutex<Options>,
}

pub fn step() -> Arc<dyn InstallStep> {
    Arc::new(EmulatorStep { options: Mutex::new(Options { memory_index: 3, ..Default::default() }) })
}

/// The server's zones (what Java and MariaDB there accept), sorted; the fallback list when the server lists none.
fn zone_list(server_zones: &[String]) -> Vec<String> {
    let mut zones: Vec<String> =
        if server_zones.is_empty() { FALLBACK_ZONES.iter().map(|z| z.to_string()).collect() } else { server_zones.to_vec() };
    zones.sort();
    zones.dedup();
    zones
}

/// Zones for the typed text: those starting with it first, then those containing it (case-insensitive).
fn matching_zones<'a>(zones: &'a [String], filter: &str) -> Vec<&'a String> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return zones.iter().collect();
    }
    let mut starts: Vec<&String> = zones.iter().filter(|z| z.to_lowercase().starts_with(&filter)).collect();
    starts.extend(zones.iter().filter(|z| {
        let lower = z.to_lowercase();
        !lower.starts_with(&filter) && lower.contains(&filter)
    }));
    starts
}

/// The memory choice for what is configured, else 4 GB from 8 GB RAM, else half the RAM in whole GB (1 GB at least).
fn memory_index(configured_mb: Option<i32>, ram_mb: i32) -> usize {
    let memory = configured_mb.unwrap_or(if ram_mb >= 8192 { 4096 } else { (ram_mb / 2 / 1024 * 1024).max(1024) });
    MEMORY_CHOICES_MB.iter().rposition(|&mb| mb <= memory).unwrap_or(0)
}

/// "localhost,127.0.0.1,<host>" without duplicates (ignoring case).
fn default_whitelist(host: &str) -> String {
    let mut hosts: Vec<&str> = Vec::new();
    for h in ["localhost", "127.0.0.1", host] {
        if !hosts.iter().any(|x| x.eq_ignore_ascii_case(h)) {
            hosts.push(h);
        }
    }
    hosts.join(",")
}

/// The whitelist's problem, or None.
fn whitelist_problem(list: &str) -> Option<&'static str> {
    let list = list.trim();
    if list.is_empty() {
        return Some(n("Enter at least one host for the websocket whitelist (e.g. localhost)."));
    }
    if list.split(',').any(|h| h.trim().contains(' ') || h.trim().contains("://")) {
        return Some(n("The whitelist holds host names only (no spaces or http://), separated by commas."));
    }
    None
}

fn clean_whitelist(list: &str) -> String {
    list.split(',').map(str::trim).filter(|h| !h.is_empty()).collect::<Vec<_>>().join(",")
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// RAM in GB with at most one decimal ("0.#").
fn format_gb(ram_mb: i32) -> String {
    let gb = (ram_mb as f64 / 1024.0 * 10.0).round() / 10.0;
    if gb.fract() == 0.0 {
        format!("{gb:.0}")
    } else {
        format!("{gb:.1}")
    }
}

impl EmulatorStep {
    // Defaults: what is configured already, else Europe/Amsterdam and memory that fits the server's RAM.
    fn fill_options(&self, state: &EmulatorState, host: &str) {
        let mut o = self.options.lock();
        let wanted = o.timezone.clone().or_else(|| state.configured_timezone.clone()).unwrap_or_else(|| DEFAULT_TIMEZONE.into());
        o.zones = zone_list(&state.timezones);
        o.timezone = o
            .zones
            .iter()
            .find(|z| **z == wanted)
            .or_else(|| o.zones.iter().find(|z| *z == DEFAULT_TIMEZONE))
            .or_else(|| o.zones.first())
            .cloned();
        o.memory_index = memory_index(state.configured_memory_mb, state.ram_mb);
        o.ram_note = if state.ram_mb > 0 { tf("server has {ram} GB RAM", &[("ram", &format_gb(state.ram_mb))]) } else { String::new() };
        if o.whitelist.trim().is_empty() {
            o.whitelist = match state.configured_whitelist.as_deref() {
                Some(configured) if !configured.is_empty() && configured != "*" => configured.to_string(),
                _ => default_whitelist(host),
            };
        }
    }
}

fn labelled(ui: &mut egui::Ui, palette: &Palette, label: &str, input: impl FnOnce(&mut egui::Ui), help: Option<&str>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.label(RichText::new(label).size(12.0).strong().color(palette.muted));
        input(ui);
        if let Some(help) = help {
            ui.label(RichText::new(help).size(11.5).color(palette.subtle));
        }
    });
}

#[async_trait]
impl InstallStep for EmulatorStep {
    fn title(&self) -> String {
        t("Emulator").into()
    }

    fn caption(&self) -> String {
        t("Build & service").into()
    }

    fn install_label(&self) -> String {
        t("Build and start the emulator").into()
    }

    fn rerun_label(&self) -> String {
        t("Rebuild and restart").into()
    }

    fn intro(&self) -> Intro {
        let account = emulator::account();
        let (repository, logs) = (emulator::repository(), emulator::log_directory());
        Intro {
            icon: Icon::Cog,
            heading: t("Build and run the Polaris emulator").into(),
            description: tf("Builds {directory} and runs it as a service under its own account.", &[("directory", &emulator::directory())]),
            bullets: vec![
                t("Installs JDK 25 (OpenJDK, or Eclipse Temurin where the distribution has none) and builds with Emulator/build.sh.").into(),
                if account == ROOT_ACCOUNT {
                    tf(
                        "Runs the emulator as root (Settings → Server layout; a normal account is safer); root owns {repository} and the log folder {logs}.",
                        &[("repository", &repository), ("logs", &logs)],
                    )
                } else {
                    tf(
                        "Creates the account '{account}' (member of www-data); it owns {repository} and the log folder {logs}.",
                        &[("account", &account), ("repository", &repository), ("logs", &logs)],
                    )
                },
                t("Creates a MariaDB account just for the emulator, with a generated password saved in the configuration folder.").into(),
                t("Writes config.ini, the start script and emulator.service, starts it at boot, and waits until the emulator has loaded.").into(),
            ],
            footnote: Some(t("The first start creates all database tables, which can take a minute.").into()),
        }
    }

    fn checks(&self) -> Vec<String> {
        vec![
            t("Check the server").into(),
            t("Install JDK 25").into(),
            t("Create the emulator account and log folder").into(),
            t("Build the emulator (build.sh)").into(),
            t("Database account, config.ini and service").into(),
            t("Start the emulator").into(),
        ]
    }

    fn options(&self, ui: &mut egui::Ui, palette: &Palette) {
        let mut guard = self.options.lock();
        let o = &mut *guard;
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            labelled(
                ui,
                palette,
                t("Timezone (config.ini serverTimezone)"),
                |ui| {
                    // Typing the first letters, like "Europe/Am", jumps to the zone; Enter takes the first match.
                    let selected = o.timezone.clone().unwrap_or_default();
                    let shown = egui::ComboBox::from_id_salt("emulator_timezone")
                        .width(340.0)
                        .height(340.0)
                        .selected_text(selected.clone())
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show_ui(ui, |ui| {
                            let filter = ui.add(
                                egui::TextEdit::singleline(&mut o.filter)
                                    .hint_text(t("Type the first letters to jump, e.g. Europe/Am"))
                                    .desired_width(f32::INFINITY),
                            );
                            if !o.popup_open {
                                o.popup_open = true;
                                filter.request_focus();
                            }
                            let enter = filter.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            ui.separator();
                            let matches = matching_zones(&o.zones, &o.filter);
                            let mut chosen = if enter { matches.first().map(|z| z.to_string()) } else { None };
                            if matches.is_empty() {
                                ui.label(RichText::new(t("No timezone matches.")).size(12.0).color(palette.subtle));
                            }
                            for (i, zone) in matches.iter().enumerate() {
                                let response = ui.selectable_label(**zone == selected, zone.as_str());
                                if i == 0 && filter.changed() {
                                    response.scroll_to_me(Some(egui::Align::Min));
                                }
                                if response.clicked() {
                                    chosen = Some(zone.to_string());
                                }
                            }
                            if let Some(zone) = chosen {
                                o.timezone = Some(zone);
                                ui.memory_mut(|m| m.close_popup());
                            }
                        });
                    if shown.inner.is_none() && o.popup_open {
                        o.popup_open = false;
                        o.filter.clear();
                    }
                },
                Some(t("Type the first letters to jump, e.g. Europe/Am")),
            );
            labelled(
                ui,
                palette,
                t("Emulator memory (-Xmx)"),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let label = |mb: i32| format!("{} GB", mb / 1024);
                        let index = o.memory_index.min(MEMORY_CHOICES_MB.len() - 1);
                        egui::ComboBox::from_id_salt("emulator_memory").width(160.0).selected_text(label(MEMORY_CHOICES_MB[index])).show_ui(
                            ui,
                            |ui| {
                                for (i, mb) in MEMORY_CHOICES_MB.iter().enumerate() {
                                    ui.selectable_value(&mut o.memory_index, i, label(*mb));
                                }
                            },
                        );
                        ui.label(RichText::new(&o.ram_note).size(12.0).color(palette.subtle));
                    });
                },
                None,
            );
        });
        ui.add_space(12.0);
        labelled(
            ui,
            palette,
            t("Websocket whitelist (hosts the client may connect from)"),
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut o.whitelist)
                        .hint_text("localhost,127.0.0.1,hotel.example.com,*.hotel.example.com")
                        .desired_width(ui.available_width().clamp(420.0, 560.0)),
                );
            },
            Some(t("Comma separated; *.domain.com allows all subdomains. Add your hotel's domain here.")),
        );
    }

    fn validate(&self) -> Option<String> {
        let o = self.options.lock();
        if o.timezone.as_ref().is_none_or(|z| !o.zones.contains(z)) {
            return Some(t("Choose a timezone for the emulator.").into());
        }
        whitelist_problem(&o.whitelist).map(|p| t(p).to_string())
    }

    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool> {
        let state = emulator::inspect(server.as_ref()).await?;
        self.fill_options(&state, &server.login().host);
        ui.wake();

        if !state.repository_present {
            ui.detected(
                ResultKind::Warning,
                &tf("{repository} is not there", &[("repository", &emulator::repository())]),
                t("Run the Polaris clone step first: the emulator is built from that folder."),
            );
            return Ok(false);
        }
        if state.is_complete() {
            let jar = state.jar.as_deref().map(file_name).unwrap_or("");
            ui.detected(
                ResultKind::Success,
                t("The emulator is built and running"),
                &format!(
                    "{} {}",
                    tf(
                        "{jar} runs as 'emulator'{service}.",
                        &[("jar", jar), ("service", if state.service_installed { " (emulator.service)" } else { "" })]
                    ),
                    t("You can skip this step, or rebuild and restart it (e.g. after updating Polaris).")
                ),
            );
            return Ok(true);
        }
        ui.detected(
            ResultKind::Info,
            if state.jar.is_none() { t("The emulator is not built yet") } else { t("The emulator is built, but not running") },
            t("Choose the timezone, memory and whitelist below, then build and start it."),
        );
        Ok(false)
    }

    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome> {
        let log = ui.logger();
        let server = server.as_ref();
        let host = server.login().host.clone();
        let (timezone, whitelist, memory_mb) = {
            let o = self.options.lock();
            (
                o.timezone.clone().unwrap_or_default(),
                clean_whitelist(&o.whitelist),
                MEMORY_CHOICES_MB[o.memory_index.min(MEMORY_CHOICES_MB.len() - 1)],
            )
        };

        // The emulator's database password is saved before anything changes.
        log("> Checking the server ...");
        let state = emulator::inspect(server).await?;
        if !state.repository_present {
            fail!("{} has no Emulator/pom.xml. Run the Polaris clone step first.", emulator::repository());
        }
        let Some(mut credentials) = config_dir::load_database_credentials(&host) else {
            fail!("There are no saved MariaDB passwords for this server. Run the MariaDB step first.");
        };
        if credentials.emulator_password.is_empty() {
            credentials.emulator_password = credentials::new_password();
            config_dir::save_database_credentials(&credentials)?;
            log(&format!(
                "  Generated a password for the MariaDB account '{}', saved in {}",
                credentials.emulator_user,
                config_dir::database_credentials_path(&host).display()
            ));
        }
        log(&format!("  Timezone {timezone}, memory {} GB, whitelist {whitelist}", memory_mb / 1024));
        ui.done(if state.jar.is_none() { t("first build") } else { t("rebuild") });

        let java_home = emulator::install_java(server, &log).await?;
        ui.done(&java_home);
        emulator::create_account(server, &log).await?;
        ui.done(&format!("{} (www-data), {}", emulator::account(), emulator::log_directory()));
        let jar = emulator::build(server, &java_home, &log).await?;
        ui.done(file_name(&jar));

        emulator::configure(server, &credentials, &timezone, memory_mb, &java_home, &log).await?;
        ui.done(&format!("{}@localhost, config.ini, emulator.service", credentials.emulator_user));

        let started = emulator::start(server, &whitelist, &log).await?;
        ui.done(t("port 3000 listening"));

        let boot = if started.systemd {
            t("It starts automatically at boot (emulator.service).")
        } else {
            t("This machine has no systemd: it was started once, not at boot.")
        };
        let errors = if started.errors > 0 {
            tf(" ({errors} ERROR lines, mostly hotel content messages: worth a look).", &[("errors", &started.errors.to_string())])
        } else {
            ".".into()
        };
        Ok(ui.succeeded(
            t("The emulator is running"),
            &format!(
                "{}{errors}",
                tf(
                    "{jar} on port 3000. {boot} Log: {logs}/emulator.log",
                    &[("jar", file_name(&started.jar)), ("boot", boot), ("logs", &emulator::log_directory())]
                )
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_defaults() {
        assert_eq!(MEMORY_CHOICES_MB[memory_index(None, 16384)], 4096);
        assert_eq!(MEMORY_CHOICES_MB[memory_index(None, 4096)], 2048);
        assert_eq!(MEMORY_CHOICES_MB[memory_index(None, 1500)], 1024);
        assert_eq!(MEMORY_CHOICES_MB[memory_index(Some(5000), 0)], 4096);
        assert_eq!(MEMORY_CHOICES_MB[memory_index(Some(512), 0)], 1024);
    }

    #[test]
    fn whitelist() {
        assert_eq!(default_whitelist("LOCALHOST"), "localhost,127.0.0.1");
        assert_eq!(default_whitelist("hotel.example.com"), "localhost,127.0.0.1,hotel.example.com");
        assert!(whitelist_problem(" ").is_some());
        assert!(whitelist_problem("localhost, http://x").is_some());
        assert!(whitelist_problem("local host").is_some());
        assert!(whitelist_problem("localhost, *.example.com").is_none());
        assert_eq!(clean_whitelist(" a , ,b,"), "a,b");
    }

    #[test]
    fn zones_jump() {
        let zones = zone_list(&[]);
        assert!(zones.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(matching_zones(&zones, "europe/am")[0], "Europe/Amsterdam");
        assert_eq!(matching_zones(&zones, "york")[0], "America/New_York");
        assert_eq!(format_gb(8192), "8");
        assert_eq!(format_gb(3900), "3.8");
    }
}
