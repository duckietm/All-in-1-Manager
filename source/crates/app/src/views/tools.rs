//! The Tools page (ToolsView.cs): the server tools as tiles (restarts, firewall, Fail2Ban / CrowdSec whitelist and
//! bans, database backup and import, the SQL password, the update check and the updates), each with its question,
//! check list, result and log under the tiles. Server status, Emulator & wired and the SSH terminal are sub-pages of it,
//! built once (the terminal keeps its shell while other pages are shown). The settings backup card is drawn on the
//! Settings page, from here, since its password dialog is one of this view's dialogs.
//!
//! The check for updates runs by itself once per login; its result goes on the update tiles' captions, on the line
//! under them, and to the dashboard's components card ([`ToolsView::last_updates`], [`ToolsView::last_images`]).

mod actions;
mod modals;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aio_remote::server_status::{ImageStatus, RepositoryUpdate};
use aio_remote::{Server, ServerError};
use eframe::egui::{self, RichText};

use crate::gui::dialogs::{Choice, Dialogs};
use crate::gui::icons::{self, Icon};
use crate::gui::step::{self, Banner, StepUi};
use crate::gui::theme::{self, Fixed, Palette};
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Page, Request, Shared, View};
use crate::i18n::{n, t, tf};
use crate::views::emulator_monitor::EmulatorMonitorView;
use crate::views::server_status::ServerStatusView;
use crate::views::terminal::TerminalView;

/// What the dashboard's Update buttons update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Emulator,
    Octane,
    AtomCms,
    Traefik,
    Gitea,
    CrowdSec,
}

/// One tile of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tool {
    HealthCheck,
    ServerStatus,
    EmulatorMonitor,
    Terminal,
    RestartEmulator,
    RestartMariaDb,
    RestartNginx,
    RestartTraefik,
    RestartAll,
    FixVisitorIps,
    EnableFirewall,
    DisableFirewall,
    Fail2Ban,
    CrowdSec,
    BackupDatabase,
    ImportDatabase,
    ImportPlus,
    FurnitureLanguage,
    CheckMultiheight,
    RecoverSqlPassword,
    CheckUpdates,
    UpdateSystem,
    UpdateEmulator,
    UpdateOctane,
    UpdateAtomCms,
    UpdateTraefik,
    UpdateGitea,
    UpdateCrowdSec,
}

impl Tool {
    /// Title (English key), description (English key) and icon, as in ToolsView.BuildWorkspace.
    fn info(self) -> (&'static str, &'static str, Icon) {
        match self {
            Tool::HealthCheck => (n("Health check"), n("Services, disk, certificate, backups, updates, firewall: green, yellow or red, with a fix"), Icon::Check),
            Tool::ServerStatus => (n("Server status"), n("Live CPU, memory, network, disks, users, versions"), Icon::Info),
            Tool::EmulatorMonitor => (n("Emulator & wired"), n("CPU per wired thread and thread group, rooms with slow wired"), Icon::Cog),
            Tool::Terminal => (n("SSH terminal"), n("A shell (command line) on the server, in this window"), Icon::Console),
            Tool::RestartEmulator => (n("Restart emulator"), n("Disconnects everyone in the hotel"), Icon::History),
            Tool::RestartMariaDb => (n("Restart MariaDB"), n("The emulator reconnects by itself"), Icon::Database),
            Tool::RestartNginx => (n("Restart NGINX"), n("And PHP-FPM (AtomCMS)"), Icon::Globe),
            Tool::RestartTraefik => (n("Restart Traefik"), n("The Docker container on 80/443"), Icon::Swap),
            Tool::RestartAll => (n("Restart all"), n("MariaDB, emulator, NGINX, Traefik"), Icon::Play),
            Tool::FixVisitorIps => (n("Fix visitor IPs"), n("The players' real IP behind Cloudflare: Traefik, NGINX and Polaris"), Icon::Globe),
            Tool::EnableFirewall => (n("Enable firewall"), n("ufw on: SSH, 80 and 443 only (the Harden step's rules)"), Icon::Shield),
            Tool::DisableFirewall => (n("Disable firewall"), n("ufw off: every port is reachable again"), Icon::Alert),
            Tool::Fail2Ban => (n("Whitelist and bans"), n("Edit the addresses that are never banned; see and lift SSH bans"), Icon::Shield),
            Tool::CrowdSec => (n("CrowdSec whitelist and bans"), n("Addresses never banned; see and lift CrowdSec's bans"), Icon::Shield),
            Tool::BackupDatabase => (n("Create full database backup"), n("Dump on the server, download to this computer"), Icon::Download),
            Tool::ImportDatabase => (n("Import full database"), n("Replace the hotel database with a backup"), Icon::Folder),
            Tool::ImportPlus => (n("Import Plus database"), n("PlusEMU furniture and catalog into Polaris"), Icon::Swap),
            Tool::FurnitureLanguage => (n("Furniture language"), n("The furniture names in another hotel's language; the database follows"), Icon::Globe),
            Tool::CheckMultiheight => (n("Check multiheight"), n("Every multiheight against the states of the furni's .hab; too many heights fixed"), Icon::Wrench),
            Tool::RecoverSqlPassword => (n("Recover SQL password"), n("New MariaDB root password, saved in configuration"), Icon::Key),
            Tool::CheckUpdates => (n("Check for updates"), n("git fetch: what is new for the emulator, Octane and AtomCMS"), Icon::Download),
            Tool::UpdateSystem => (n("Update system"), n("apt update, upgrade and dist-upgrade"), Icon::Cog),
            Tool::UpdateEmulator => (n("Update emulator"), n("git pull, build.sh, start the new version"), Icon::Server),
            Tool::UpdateOctane => (n("Update Octane"), n("Octane-Renderer first, then Octane: git pull, fresh yarn install, build"), Icon::Cube),
            Tool::UpdateAtomCms => (n("Update AtomCMS"), n("git pull, composer install, migrations, theme build"), Icon::Puzzle),
            Tool::UpdateTraefik => (n("Update Traefik"), n("The newest traefik:v3 image (Docker Hub)"), Icon::Swap),
            Tool::UpdateGitea => (n("Update Gitea"), n("The newest gitea/gitea:1 image; the repositories stay"), Icon::Git),
            Tool::UpdateCrowdSec => (n("Update CrowdSec"), n("The newest image and detection rules; bans and enrollment stay"), Icon::Shield),
        }
    }

    fn title(self) -> &'static str {
        self.info().0
    }

    fn description(self) -> &'static str {
        self.info().1
    }

    fn for_component(component: Component) -> Tool {
        match component {
            Component::Emulator => Tool::UpdateEmulator,
            Component::Octane => Tool::UpdateOctane,
            Component::Traefik => Tool::UpdateTraefik,
            Component::Gitea => Tool::UpdateGitea,
            Component::CrowdSec => Tool::UpdateCrowdSec,
            Component::AtomCms => Tool::UpdateAtomCms,
        }
    }
}

/// The sections of the page, in order.
const SECTIONS: [(&str, &[Tool]); 8] = [
    (n("Status"), &[Tool::HealthCheck, Tool::ServerStatus, Tool::EmulatorMonitor, Tool::Terminal]),
    (n("Services"), &[Tool::RestartEmulator, Tool::RestartMariaDb, Tool::RestartNginx, Tool::RestartTraefik, Tool::RestartAll]),
    (n("Visitors"), &[Tool::FixVisitorIps]),
    (n("Firewall"), &[Tool::EnableFirewall, Tool::DisableFirewall]),
    (n("Fail2Ban"), &[Tool::Fail2Ban]),
    (n("CrowdSec"), &[Tool::CrowdSec]),
    (n("Database"), &[Tool::BackupDatabase, Tool::ImportDatabase, Tool::ImportPlus, Tool::FurnitureLanguage, Tool::CheckMultiheight, Tool::RecoverSqlPassword]),
    (
        n("Updates"),
        &[
            Tool::CheckUpdates,
            Tool::UpdateSystem,
            Tool::UpdateEmulator,
            Tool::UpdateOctane,
            Tool::UpdateAtomCms,
            Tool::UpdateTraefik,
            Tool::UpdateGitea,
            Tool::UpdateCrowdSec,
        ],
    ),
];

/// The settings backup buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    Export,
    Import,
}

impl SettingsAction {
    fn title(self) -> &'static str {
        match self {
            SettingsAction::Export => n("Export encrypted settings"),
            SettingsAction::Import => n("Import encrypted settings"),
        }
    }
}

/// What is under the tiles: the run panel of the last tool, or the banner of a tool that stopped before it ran.
#[derive(Clone)]
enum Host {
    Run { title: String, run: StepUi },
    Banner { title: String, banner: Banner },
}

/// The page's state, shared with the tools' tasks.
#[derive(Default)]
struct State {
    running: bool,
    host: Option<Host>,
    /// The run panel of the tool running now (None until it started one).
    current_run: Option<StepUi>,
    scroll_to_result: bool,
    updates_text: String,
    updates_warning: bool,
    /// Captions that say what the update check found, per update tile.
    captions: HashMap<Tool, String>,
    checking: bool,
    /// The login the last update check ran for (once per login).
    checked_for: Option<usize>,
    last_for: Option<usize>,
    last_updates: Option<Vec<RepositoryUpdate>>,
    last_images: Option<Vec<ImageStatus>>,
    settings_result: Option<Banner>,
    modal: Option<modals::Modal>,
}

type SharedState = Arc<Mutex<State>>;

fn session_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

pub struct ToolsView {
    page: Page,
    state: SharedState,
    server_status: Option<ServerStatusView>,
    doctor: Option<crate::views::doctor::DoctorView>,
    emulator_monitor: Option<EmulatorMonitorView>,
    terminal: Option<TerminalView>,
    /// What the search field above the tiles filters on.
    search: String,
}

/// True when every word of `query` is in the tool's title, description or caption (English or translated).
fn matches(query: &str, texts: &[&str]) -> bool {
    let haystack = texts.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>().join(" ");
    query.split_whitespace().all(|word| haystack.contains(&word.to_lowercase()))
}

impl ToolsView {
    pub fn new(_shared: &Shared) -> Self {
        Self { page: Page::Tools, state: Arc::new(Mutex::new(State::default())), server_status: None, doctor: None, emulator_monitor: None, terminal: None, search: String::new() }
    }

    /// Starts the update check once per login (not while a tool runs). (ToolsView.Refresh)
    pub fn refresh(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        {
            let mut state = self.state.lock().unwrap();
            if state.running || state.checked_for == Some(session_id(&server)) {
                return;
            }
            state.checked_for = Some(session_id(&server));
        }
        let (state, task_shared) = (self.state.clone(), shared.clone());
        shared.spawn(async move { check_quietly(&task_shared, &state, &server).await });
    }

    /// Tools, Server status, Emulator & wired or the SSH terminal.
    pub fn set_page(&mut self, page: Page) {
        self.page = page;
    }

    /// The sub-page on screen (Back returns to Tools).
    pub fn page(&self) -> Page {
        self.page
    }

    /// Runs an update with its question and progress on this page.
    pub fn update(&mut self, component: Component, shared: &Shared) {
        self.page = Page::Tools;
        self.start_tool(Tool::for_component(component), shared);
    }

    /// Restarts a Docker service so it runs the newer image that is already downloaded (no Docker Hub needed). Only
    /// Traefik has a restart tool; the others are updated, which recreates them from the downloaded image too.
    pub fn restart(&mut self, component: Component, shared: &Shared) {
        self.page = Page::Tools;
        let tool = if component == Component::Traefik { Tool::RestartTraefik } else { Tool::for_component(component) };
        self.start_tool(tool, shared);
    }

    /// The last update check for this login (None: not checked yet).
    pub fn last_updates(&self) -> Option<Vec<RepositoryUpdate>> {
        self.state.lock().unwrap().last_updates.clone()
    }

    /// The last Docker image check for this login.
    pub fn last_images(&self) -> Option<Vec<ImageStatus>> {
        self.state.lock().unwrap().last_images.clone()
    }

    /// Fetches GitHub again (the dashboard's Check again), without a run panel.
    pub fn check_updates_again(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        {
            let state = self.state.lock().unwrap();
            if state.running || state.checking {
                return;
            }
        }
        let (state, task_shared) = (self.state.clone(), shared.clone());
        shared.spawn(async move { check_quietly(&task_shared, &state, &server).await });
    }

    /// A tile was clicked (or an Update button of the dashboard). (RunToolAsync)
    fn start_tool(&mut self, tool: Tool, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        if self.state.lock().unwrap().running {
            return;
        }
        match tool {
            Tool::HealthCheck => {
                // Opening the page again checks again.
                if let Some(doctor) = &self.doctor {
                    doctor.check(shared);
                }
                return self.page = Page::Doctor;
            }
            Tool::ServerStatus => return self.page = Page::ServerStatus,
            Tool::EmulatorMonitor => return self.page = Page::EmulatorMonitor,
            Tool::Terminal => return self.page = Page::Terminal,
            _ => {}
        }
        let title = t(tool.title()).to_string();
        {
            let mut state = self.state.lock().unwrap();
            state.running = true;
            state.current_run = None;
        }
        shared.set_status(&tf("{title} ...", &[("title", &title)]));
        if server.is_dry_run() {
            // The recorded commands go to the run panel of the tool.
            let state = self.state.clone();
            crate::gui::dry_run::set_sink(move |line| {
                let run = state.lock().unwrap().current_run.clone();
                if let Some(run) = run {
                    run.log(line);
                }
            });
        }
        let job = actions::Job { shared: shared.clone(), state: self.state.clone(), server, title: title.clone() };
        let started = chrono::Local::now();
        shared.spawn(async move {
            let result = actions::perform(tool, &job).await;
            let failed = result.is_err();
            let run = job.state.lock().unwrap().current_run.clone();
            if let Err(error) = result {
                let message = step::error_message(&error);
                let heading = tf("{title} failed", &[("title", &title)]);
                match &run {
                    Some(run) => {
                        run.log(&format!("! {message}"));
                        run.fail();
                        run.result(ResultKind::Error, &heading, &message);
                    }
                    None => job.show(ResultKind::Error, &heading, &message),
                }
            }
            if let Some(run) = &run {
                run.set_busy(false);
                // The run panel's log goes to the history; a tool that stopped before it ran leaves nothing to keep.
                let outcome = if failed { aio_core::history::Outcome::Failed } else { aio_core::history::Outcome::Done };
                step::save_history(run, job.server.as_ref(), "tool", &title, outcome, started);
            }
            {
                let mut state = job.state.lock().unwrap();
                state.running = false;
                state.current_run = None;
            }
            job.shared.set_status(&if run.is_none() { t("Ready.").to_string() } else { tf("{title}: finished.", &[("title", &title)]) });
        });
    }

    /// The settings backup buttons: no server needed.
    fn start_settings(&mut self, action: SettingsAction, shared: &Shared) {
        {
            let mut state = self.state.lock().unwrap();
            if state.running {
                return;
            }
            state.running = true;
        }
        let title = t(action.title()).to_string();
        shared.set_status(&tf("{title} ...", &[("title", &title)]));
        let (state, task_shared) = (self.state.clone(), shared.clone());
        shared.spawn(async move {
            let result = match action {
                SettingsAction::Export => actions::export_settings(&task_shared, &state).await,
                SettingsAction::Import => actions::import_settings(&task_shared, &state).await,
            };
            let mut s = state.lock().unwrap();
            if let Err(error) = result {
                s.settings_result =
                    Some(Banner { kind: ResultKind::Error, heading: tf("{title} failed", &[("title", &title)]), message: step::error_message(&error) });
            }
            s.running = false;
            drop(s);
            task_shared.set_status(t("Ready."));
        });
    }

    /// The settings backup card (on the Settings page): export and import the encrypted settings. No server needed.
    pub fn settings_backup_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        modals::show(ui.ctx(), palette, &self.state);
        let (running, settings_result) = {
            let state = self.state.lock().unwrap();
            (state.running, state.settings_result.clone())
        };
        let mut clicked_settings = None;
        widgets::card(ui, palette, egui::Margin::symmetric(24, 20), |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(widgets::title_text(t("Settings backup"), 15.0));
            let directory = aio_core::config_dir::directory();
            widgets::muted(
                ui,
                palette,
                &tf(
                    "All settings of this installer in one file, encrypted with a password: the SSH keys (also the .ppk files) and the MariaDB passwords of every server, from {directory}. Import it on another computer to manage the same servers from there. No server connection needed.",
                    &[("directory", &directory.display().to_string())],
                ),
                12.5,
            );
            ui.horizontal(|ui| {
                for (action, icon) in [(SettingsAction::Export, Icon::Shield), (SettingsAction::Import, Icon::Key)] {
                    let button = egui::Button::image_and_text(icons::image(icon, 16.0, Fixed::brand()), RichText::new(t(action.title())).size(13.0).strong())
                        .corner_radius(if theme::hotel() { 6 } else { 8 })
                        .min_size(egui::vec2(0.0, 36.0));
                    if ui.add_enabled(!running, button).clicked() {
                        clicked_settings = Some(action);
                    }
                    ui.add_space(4.0);
                }
            });
            if let Some(banner) = &settings_result {
                widgets::result_banner(ui, palette, banner.kind, &banner.heading, &banner.message);
            }
        });
        if let Some(action) = clicked_settings {
            self.start_settings(action, shared);
        }
    }

    fn show_tools(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let connected = shared.session().is_some();
        let (running, host, captions, updates_text, updates_warning, scroll) = {
            let mut state = self.state.lock().unwrap();
            let scroll = std::mem::take(&mut state.scroll_to_result);
            (
                state.running,
                state.host.clone(),
                state.captions.clone(),
                state.updates_text.clone(),
                state.updates_warning,
                scroll,
            )
        };
        let mut clicked_tool = None;
        egui::ScrollArea::vertical().id_salt("tools-page").auto_shrink([false, false]).show(ui, |ui| {
            widgets::card(ui, palette, egui::Margin::symmetric(24, 20), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.horizontal(|ui| {
                    ui.label(widgets::title_text(t("Server tools"), 15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.search.is_empty() && ui.small_button("✕").on_hover_text(t("Clear the search")).clicked() {
                            self.search.clear();
                        }
                        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(t("Search the tools ...")).desired_width(240.0));
                    });
                });
                let query = self.search.trim().to_string();
                let mut found = 0;
                if !connected {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("Not connected: log in to the server to use these tools.")).size(12.5).color(palette.warning_text));
                        ui.add_space(12.0);
                        if ui.add(widgets::primary_button(t("Log in"))).clicked() {
                            shared.request(Request::Login);
                        }
                    });
                }
                for (heading, tools) in SECTIONS {
                    let shown: Vec<_> = tools
                        .iter()
                        .copied()
                        .filter(|tool| {
                            let (title, description, _) = tool.info();
                            let caption = captions.get(tool).cloned().unwrap_or_default();
                            query.is_empty() || matches(&query, &[title, t(title), description, t(description), &caption, heading, t(heading)])
                        })
                        .collect();
                    if shown.is_empty() {
                        continue;
                    }
                    found += shown.len();
                    ui.add_space(8.0);
                    // A folded section opens by itself while a search finds something in it.
                    let open = crate::gui::collapse::heading(ui, &format!("tools:{heading}"), &t(heading).to_uppercase(), 11.0, palette.muted, 0.0)
                        || !query.is_empty();
                    if !open {
                        continue;
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        if !connected {
                            ui.multiply_opacity(0.45);
                        }
                        for &tool in &shown {
                            let (title, description, icon) = tool.info();
                            let caption = captions.get(&tool).cloned().unwrap_or_else(|| t(description).to_string());
                            if tile(ui, palette, icon, t(title), &caption, connected && !running) {
                                clicked_tool = Some(tool);
                            }
                        }
                    });
                    if heading == "Updates" && !updates_text.is_empty() {
                        let color = if updates_warning { palette.warning_text } else { palette.subtle };
                        ui.add(egui::Label::new(RichText::new(&updates_text).size(12.0).color(color)).wrap());
                    }
                }
                if found == 0 {
                    ui.label(RichText::new(tf("No tool matches \"{search}\".", &[("search", &query)])).size(12.5).color(palette.muted));
                }
                if let Some(host) = &host {
                    ui.add_space(12.0);
                    let response = ui.vertical(|ui| match host {
                        Host::Run { title, run } => {
                            ui.label(RichText::new(title).size(16.0).strong());
                            step::show_run(ui, palette, run, 300.0);
                        }
                        Host::Banner { title, banner } => {
                            ui.label(RichText::new(title).size(16.0).strong());
                            ui.add_space(4.0);
                            widgets::result_banner(ui, palette, banner.kind, &banner.heading, &banner.message);
                        }
                    });
                    if scroll {
                        ui.scroll_to_rect(response.response.rect, Some(egui::Align::TOP));
                    }
                }
            });
            ui.add_space(18.0);
        });
        if let Some(tool) = clicked_tool {
            self.start_tool(tool, shared);
        }
    }

    fn back_button(&mut self, ui: &mut egui::Ui) {
        if ui.add(widgets::secondary_button(t("Back to the tools"))).clicked() {
            self.page = Page::Tools;
        }
    }
}

impl View for ToolsView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        modals::show(ui.ctx(), palette, &self.state);
        match self.page {
            Page::ServerStatus | Page::EmulatorMonitor => {
                egui::ScrollArea::vertical().id_salt(("tools-monitor", self.page == Page::ServerStatus)).auto_shrink([false, false]).show(ui, |ui| {
                    widgets::card(ui, palette, egui::Margin::symmetric(24, 20), |ui| {
                        if self.page == Page::ServerStatus {
                            self.server_status.get_or_insert_with(|| ServerStatusView::new(shared)).show(ui, palette, shared);
                        } else {
                            self.emulator_monitor.get_or_insert_with(|| EmulatorMonitorView::new(shared)).show(ui, palette, shared);
                        }
                        ui.add_space(16.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| self.back_button(ui));
                    });
                    ui.add_space(18.0);
                });
            }
            Page::Doctor => {
                let mut fix = None;
                egui::ScrollArea::vertical().id_salt("tools-doctor").auto_shrink([false, false]).show(ui, |ui| {
                    let doctor = self.doctor.get_or_insert_with(|| crate::views::doctor::DoctorView::new(shared));
                    doctor.show(ui, palette, shared);
                    fix = doctor.take_fix();
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| self.back_button(ui));
                    ui.add_space(18.0);
                });
                // A Fix button: the tool runs on the Tools page, with its question and log.
                if let Some(fix) = fix {
                    use crate::views::doctor::Fix;
                    let tool = match fix {
                        Fix::RestartEmulator => Tool::RestartEmulator,
                        Fix::RestartMariaDb => Tool::RestartMariaDb,
                        Fix::RestartNginx => Tool::RestartNginx,
                        Fix::RestartTraefik => Tool::RestartTraefik,
                        Fix::UpdateSystem => Tool::UpdateSystem,
                        Fix::BackupDatabase => Tool::BackupDatabase,
                        Fix::EnableFirewall => Tool::EnableFirewall,
                    };
                    self.page = Page::Tools;
                    self.start_tool(tool, shared);
                }
            }
            Page::Terminal => {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| self.back_button(ui));
                ui.add_space(8.0);
                self.terminal.get_or_insert_with(|| TerminalView::new(shared)).show(ui, palette, shared);
            }
            _ => self.show_tools(ui, palette, shared),
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().running
    }

    fn session_changed(&mut self, shared: &Shared) {
        let id = shared.session().map(|s| session_id(&s));
        {
            let mut state = self.state.lock().unwrap();
            if state.last_for != id {
                state.last_for = None;
                state.last_updates = None;
                state.last_images = None;
                state.captions.clear();
                state.updates_text.clear();
                state.updates_warning = false;
            }
        }
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.session_changed(shared);
        }
        if let Some(view) = self.server_status.as_mut() {
            view.session_changed(shared);
        }
        if let Some(view) = self.emulator_monitor.as_mut() {
            view.session_changed(shared);
        }
        if let Some(view) = self.doctor.as_mut() {
            view.session_changed(shared);
        }
        self.refresh(shared);
    }
}

/// A tool tile, 268 x 66 like the C# button: icon, bold title, a caption of up to two lines. True when clicked.
fn tile(ui: &mut egui::Ui, palette: &Palette, icon: Icon, title: &str, caption: &str, enabled: bool) -> bool {
    let size = egui::vec2(268.0, 66.0);
    // The Hotel look: a picture of its own behind every tool (the same one for the same tool), the text on a plate.
    if theme::hotel() {
        let scene = crate::gui::scenes::for_key(title);
        let response = crate::gui::scenes::button(ui, palette, size, scene, Fixed::brand(), icon, title, caption, crate::gui::scenes::Layout::Side, palette.card, enabled);
        return response.clicked();
    }
    let (rect, response) = ui.allocate_exact_size(size, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let hovered = enabled && response.hovered();
    let fill = if hovered { palette.hover } else { palette.card };
    let width = if theme::hotel() { 2.0_f32 } else { 1.0 };
    let stroke = egui::Stroke::new(width, if hovered { Fixed::brand() } else { palette.card_border_strong });
    ui.painter().rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
    let icon_rect = egui::Rect::from_min_size(egui::pos2(rect.left() + 14.0, rect.center().y - 9.0), egui::vec2(18.0, 18.0));
    icons::image(icon, 18.0, Fixed::brand()).paint_at(ui, icon_rect);
    let left = icon_rect.right() + 12.0;
    let text_width = rect.right() - 14.0 - left;
    let title_galley = layout(ui, title, egui::FontId::new(13.0, egui::FontFamily::Name("bold".into())), palette.text, text_width, 1);
    let caption_galley = layout(ui, caption, egui::FontId::proportional(11.5), palette.subtle, text_width, 2);
    let title_height = title_galley.size().y;
    let cut = caption_galley.elided || title_galley.elided;
    let top = rect.center().y - (title_height + 1.0 + caption_galley.size().y) / 2.0;
    let painter = ui.painter();
    painter.galley(egui::pos2(left, top), title_galley, palette.text);
    painter.galley(egui::pos2(left, top + title_height + 1.0), caption_galley, palette.subtle);
    let response = if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response };
    // The whole caption when it did not fit in two lines.
    let response = if cut { response.on_hover_text(caption) } else { response };
    response.clicked()
}

fn layout(ui: &egui::Ui, text: &str, font: egui::FontId, color: egui::Color32, width: f32, rows: usize) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, width);
    job.wrap.max_rows = rows;
    job.wrap.overflow_character = Some('…');
    ui.fonts(|fonts| fonts.layout_job(job))
}

/// The C# ConfirmAsync: "Are you sure?" with the action and Cancel.
async fn confirm(dialogs: &Dialogs, heading: &str, message: &str, go: &str) -> bool {
    dialogs
        .choose(
            t("Are you sure?"),
            heading,
            message,
            false,
            vec![Choice::new(go, t("Go ahead."), true).primary(), Choice::new(t("Cancel"), t("Change nothing."), false)],
        )
        .await
}

/// The update check without a run panel: after logging in, the dashboard's Check again, and after an update.
async fn check_quietly(shared: &Shared, state: &SharedState, server: &Arc<dyn Server>) {
    {
        let mut s = state.lock().unwrap();
        s.updates_text = t("Checking for updates ...").into();
        s.updates_warning = false;
        s.checking = true;
    }
    let result = async {
        let updates = aio_remote::server_status::updates(server.as_ref()).await?;
        let images = load_images(server.as_ref()).await?;
        anyhow::Ok((updates, images))
    }
    .await;
    match result {
        Ok((updates, images)) => show_updates(shared, state, server, updates, images),
        Err(error) => {
            let mut s = state.lock().unwrap();
            s.updates_text = tf("Could not check for updates: {message}", &[("message", &step::error_message(&error))]);
            s.updates_warning = false;
        }
    }
    state.lock().unwrap().checking = false;
    shared.ctx.request_repaint();
}

/// The Docker services' check never stops the git one: a Docker Hub problem leaves them unchecked.
async fn load_images(server: &dyn Server) -> anyhow::Result<Vec<ImageStatus>> {
    match aio_remote::server_status::images(server).await {
        Ok(images) => Ok(images),
        Err(error) if error.downcast_ref::<ServerError>().is_some() => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Puts what the check found on the update tiles and on the line under them, and refreshes the dashboard's card.
fn show_updates(shared: &Shared, state: &SharedState, server: &Arc<dyn Server>, updates: Vec<RepositoryUpdate>, images: Vec<ImageStatus>) {
    // A check that finished after the login changed belongs to the old login.
    if shared.session().map(|s| session_id(&s)) != Some(session_id(server)) {
        return;
    }
    let directories = UpdateDirectories::current();
    let captions = update_captions(&updates, &images, &directories);
    let (text, warning) = updates_line(&updates);
    {
        let mut s = state.lock().unwrap();
        s.captions = captions;
        s.updates_text = text;
        s.updates_warning = warning;
        s.last_updates = Some(updates);
        s.last_images = Some(images);
        s.last_for = Some(session_id(server));
    }
    shared.request(Request::ReloadVersions);
}

/// Where the clones of the update tiles are.
struct UpdateDirectories {
    emulator: String,
    octane: String,
    renderer: String,
    atom_cms: String,
}

impl UpdateDirectories {
    fn current() -> Self {
        Self {
            emulator: aio_remote::emulator::repository(),
            octane: aio_remote::octane::octane(),
            renderer: aio_remote::octane::renderer(),
            atom_cms: aio_remote::atom_cms::directory(),
        }
    }
}

/// The captions of the update tiles. (ShowUpdates and Caption)
fn update_captions(updates: &[RepositoryUpdate], images: &[ImageStatus], directories: &UpdateDirectories) -> HashMap<Tool, String> {
    let mut captions = HashMap::new();
    let clones = |dirs: &[&str]| updates.iter().filter(|u| dirs.contains(&u.directory.as_str())).collect::<Vec<_>>();
    captions.insert(Tool::UpdateEmulator, clone_caption(t(Tool::UpdateEmulator.description()), &clones(&[&directories.emulator])));
    captions.insert(Tool::UpdateOctane, clone_caption(t(Tool::UpdateOctane.description()), &clones(&[&directories.octane, &directories.renderer])));
    captions.insert(Tool::UpdateAtomCms, clone_caption(t(Tool::UpdateAtomCms.description()), &clones(&[&directories.atom_cms])));
    for (tool, name) in [(Tool::UpdateTraefik, "Traefik"), (Tool::UpdateGitea, "Gitea"), (Tool::UpdateCrowdSec, "CrowdSec")] {
        let description = t(tool.description());
        let image = images.iter().find(|i| i.name == name);
        let version = image.and_then(|i| i.version.clone()).unwrap_or_default();
        let caption = match image {
            None if name != "Traefik" => tf("not installed · {description}", &[("description", description)]),
            None => description.to_string(),
            Some(image) if image.has_update() => match &image.newest_version {
                Some(newest) => tf("{version} available · {description}", &[("version", newest), ("description", description)]),
                None => tf("update available · {description}", &[("description", description)]),
            },
            Some(image) if image.problem.is_some() => tf("{version} · {description}", &[("version", &version), ("description", description)]),
            Some(_) => tf("{version}, up to date · {description}", &[("version", &version), ("description", description)]),
        };
        captions.insert(tool, caption);
    }
    captions
}

fn clone_caption(description: &str, clones: &[&RepositoryUpdate]) -> String {
    if clones.is_empty() {
        return description.to_string();
    }
    let behind: i32 = clones.iter().map(|c| c.behind).sum();
    let behind_text = behind.to_string();
    match clones.iter().find(|c| c.switches_branch()).and_then(|c| c.target.clone()) {
        Some(target) if behind > 0 => {
            tf("to {target}, {behind} new · {description}", &[("target", &target), ("behind", &behind_text), ("description", description)])
        }
        Some(target) => tf("to {target} · {description}", &[("target", &target), ("description", description)]),
        None if behind > 0 => tf("{behind} new commit(s) · {description}", &[("behind", &behind_text), ("description", description)]),
        None => tf("up to date · {description}", &[("description", description)]),
    }
}

/// The line under the update tiles, and whether it is a warning (something to update).
fn updates_line(updates: &[RepositoryUpdate]) -> (String, bool) {
    if updates.is_empty() {
        return (t("No hotel clones found on this server.").into(), false);
    }
    let behind: Vec<_> = updates.iter().filter(|u| u.has_update()).collect();
    let switching: Vec<_> = updates.iter().filter(|u| u.switches_branch()).collect();
    let mut parts = Vec::new();
    if !behind.is_empty() {
        let list = behind.iter().map(|u| format!("{} {} on {}", u.name, u.behind, u.counted().unwrap_or_default())).collect::<Vec<_>>().join(", ");
        parts.push(tf("New commits: {list}.", &[("list", &list)]));
    }
    if !switching.is_empty() {
        parts.push(tf("Moving to the chosen branch: {list}.", &[("list", &switch_list(&switching))]));
    }
    if parts.is_empty() {
        return (tf("Checked {count} clone(s): everything is on its branch and up to date.", &[("count", &updates.len().to_string())]), false);
    }
    (parts.join("   "), true)
}

/// "name branch → target, ..."
fn switch_list(switching: &[&RepositoryUpdate]) -> String {
    switching
        .iter()
        .map(|u| format!("{} {} → {}", u.name, u.branch.as_deref().unwrap_or_default(), u.target.as_deref().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A file size as the C# Size: GB and MB with one decimal, KB without.
fn size(bytes: i64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    const MB: f64 = (1u64 << 20) as f64;
    let value = bytes as f64;
    // .NET's custom formats round half away from zero.
    let one = |x: f64| format!("{:.1}", (x * 10.0).round() / 10.0);
    if value >= GB {
        format!("{} GB", one(value / GB))
    } else if value >= MB {
        format!("{} MB", one(value / MB))
    } else {
        format!("{} KB", (value / 1024.0).round())
    }
}

/// The percentage step a transfer is logged at (every 10 %), or None when it is not time yet.
fn progress_step(done: u64, total: i64, reported: i64) -> Option<i64> {
    let percent = if total > 0 { (done as i64).saturating_mul(100) / total } else { 0 };
    if percent < reported + 10 {
        None
    } else {
        Some(percent / 10 * 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_every_word() {
        assert!(matches("restart nginx", &["Restart NGINX", "nginx -t first"]));
        assert!(matches("BACKUP", &["Create full database backup"]));
        assert!(!matches("restart gitea", &["Restart NGINX"]));
        assert!(matches("", &["anything"]));
    }

    fn update(name: &str, directory: &str, branch: &str, target: Option<&str>, behind: i32) -> RepositoryUpdate {
        RepositoryUpdate {
            name: name.into(),
            directory: directory.into(),
            branch: Some(branch.into()),
            target: target.map(Into::into),
            behind,
            newest: Some("abc fix".into()),
        }
    }

    fn image(name: &str, version: &str, newer: bool, newest: Option<&str>, problem: Option<&str>) -> ImageStatus {
        ImageStatus {
            name: name.into(),
            container: name.to_lowercase(),
            image: "img".into(),
            version: Some(version.into()),
            running: true,
            newer_on_hub: newer,
            not_restarted: false,
            newest_version: newest.map(Into::into),
            problem: problem.map(Into::into),
        }
    }

    fn dirs() -> UpdateDirectories {
        UpdateDirectories { emulator: "/e".into(), octane: "/o".into(), renderer: "/r".into(), atom_cms: "/a".into() }
    }

    #[test]
    fn sizes_like_the_csharp() {
        assert_eq!(size(512), "1 KB");
        assert_eq!(size(1536), "2 KB");
        assert_eq!(size(5 * 1024 * 1024 + 300 * 1024), "5.3 MB");
        assert_eq!(size(3 << 30), "3.0 GB");
        assert_eq!(size(0), "0 KB");
    }

    #[test]
    fn progress_every_ten_percent() {
        assert_eq!(progress_step(5, 100, 0), None);
        assert_eq!(progress_step(10, 100, 0), Some(10));
        assert_eq!(progress_step(19, 100, 10), None);
        assert_eq!(progress_step(57, 100, 10), Some(50));
        assert_eq!(progress_step(100, 100, 90), Some(100));
        assert_eq!(progress_step(10, 0, 0), None);
    }

    #[test]
    fn captions_of_the_update_tiles() {
        let updates = vec![update("Polaris", "/e", "main", None, 3), update("Octane", "/o", "main", Some("dev"), 2), update("AtomCMS", "/a", "main", None, 0)];
        let images = vec![image("Gitea", "1.22", true, Some("1.23"), None), image("CrowdSec", "1.6", false, None, Some("hub down"))];
        let captions = update_captions(&updates, &images, &dirs());
        assert_eq!(captions[&Tool::UpdateEmulator], "3 new commit(s) · git pull, build.sh, start the new version");
        assert_eq!(captions[&Tool::UpdateOctane], "to dev, 2 new · Octane-Renderer first, then Octane: git pull, fresh yarn install, build");
        assert_eq!(captions[&Tool::UpdateAtomCms], "up to date · git pull, composer install, migrations, theme build");
        assert_eq!(captions[&Tool::UpdateTraefik], "The newest traefik:v3 image (Docker Hub)");
        assert_eq!(captions[&Tool::UpdateGitea], "1.23 available · The newest gitea/gitea:1 image; the repositories stay");
        assert_eq!(captions[&Tool::UpdateCrowdSec], "1.6 · The newest image and detection rules; bans and enrollment stay");

        let none = update_captions(&[], &[], &dirs());
        assert_eq!(none[&Tool::UpdateEmulator], "git pull, build.sh, start the new version");
        assert_eq!(none[&Tool::UpdateGitea], "not installed · The newest gitea/gitea:1 image; the repositories stay");
    }

    #[test]
    fn line_under_the_update_tiles() {
        assert_eq!(updates_line(&[]), ("No hotel clones found on this server.".to_string(), false));
        let current = vec![update("Polaris", "/e", "main", None, 0)];
        assert_eq!(updates_line(&current), ("Checked 1 clone(s): everything is on its branch and up to date.".to_string(), false));
        let behind = vec![update("Polaris", "/e", "main", None, 3), update("Octane", "/o", "main", Some("dev"), 0)];
        assert_eq!(updates_line(&behind), ("New commits: Polaris 3 on main.   Moving to the chosen branch: Octane main → dev.".to_string(), true));
    }
}
