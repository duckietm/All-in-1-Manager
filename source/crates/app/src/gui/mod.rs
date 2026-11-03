//! The window (InstallerWindow.cs): the hotel-blue sidebar, the header with the login pill, the pages and the footer
//! with the status line. One login at startup is shared by every page until the program closes or the server drops
//! it; then the login opens again as soon as nothing runs.

pub mod banner;
pub mod collapse;
pub mod day_night;
pub mod dialogs;
pub mod dry_run;
pub mod flags;
pub mod icons;
pub mod language_picker;
pub mod log_view;
pub mod scenes;
pub mod screenshot;
pub mod step;
pub mod theme;
pub mod widgets;
pub mod wizard;

use std::sync::{Arc, Mutex};

use aio_remote::Server;
use eframe::egui::{self, Color32, RichText};

use crate::i18n::{t, tf};
use crate::views;
use dialogs::{Choice, Dialogs};
use icons::Icon;
use theme::{Fixed, Palette};
use wizard::{Wizard, WizardAction};

/// The pages of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Install,
    Addons,
    AtomCms,
    CustomCms,
    EpicNext,
    RemoveCms,
    Gitea,
    RemoveGitea,
    Fail2Ban,
    RemoveFail2Ban,
    CrowdSec,
    RemoveCrowdSec,
    DefaultCatalog,
    DeployAssets,
    AssetExplorer,
    SqlEditor,
    Debug,
    Tools,
    ServerStatus,
    EmulatorMonitor,
    Terminal,
    Doctor,
    History,
    Settings,
    About,
}

/// What every page gets: the login, the runtime for its tasks, the questions, and the status line.
#[derive(Clone)]
pub struct Shared {
    pub runtime: tokio::runtime::Handle,
    pub dialogs: Dialogs,
    pub ctx: egui::Context,
    session: Arc<Mutex<Option<Arc<dyn Server>>>>,
    status: Arc<Mutex<String>>,
    /// `--dry-run`: the login records the commands instead of connecting.
    pub dry_run: bool,
    requests: Arc<Mutex<Vec<Request>>>,
}

/// What a page or task asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Go(Page),
    Login,
    /// Run a Tools update on the Tools page (from the dashboard's components card).
    Update(views::tools::Component),
    /// Restart a Docker service on the Tools page, so it runs the newer image that is already downloaded.
    Restart(views::tools::Component),
    RestartForLook,
    /// Fetch GitHub again for the dashboard's card.
    CheckUpdates,
    /// Read the installed versions again (after an update).
    ReloadVersions,
}

impl Shared {
    /// The login while it is connected.
    pub fn session(&self) -> Option<Arc<dyn Server>> {
        self.session.lock().unwrap().clone().filter(|s| s.is_connected())
    }

    pub fn set_status(&self, message: &str) {
        *self.status.lock().unwrap() = message.to_string();
        self.ctx.request_repaint();
    }

    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    pub fn request(&self, request: Request) {
        self.requests.lock().unwrap().push(request);
        self.ctx.request_repaint();
    }

    /// Runs a task on the runtime and draws the window again when it ends.
    pub fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, future: F) {
        let ctx = self.ctx.clone();
        self.runtime.spawn(async move {
            future.await;
            ctx.request_repaint();
        });
    }
}

/// A page that is not a wizard.
pub trait View {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared);
    /// A step or tool runs: navigation and closing wait.
    fn busy(&self) -> bool {
        false
    }
    /// The login changed (a new one, or none).
    fn session_changed(&mut self, _shared: &Shared) {}
}

pub(crate) struct Wizards {
    pub(crate) install: Wizard,
    pub(crate) atom: Wizard,
    pub(crate) custom_cms: Wizard,
    pub(crate) epicnext: Wizard,
    pub(crate) remove_cms: Wizard,
    pub(crate) gitea: Wizard,
    pub(crate) remove_gitea: Wizard,
    pub(crate) fail2ban: Wizard,
    pub(crate) remove_fail2ban: Wizard,
    pub(crate) crowdsec: Wizard,
    pub(crate) remove_crowdsec: Wizard,
    pub(crate) default_catalog: Wizard,
    pub(crate) deploy: Wizard,
}

impl Wizards {
    fn get(&mut self, page: Page) -> Option<&mut Wizard> {
        Some(match page {
            Page::Install => &mut self.install,
            Page::AtomCms => &mut self.atom,
            Page::CustomCms => &mut self.custom_cms,
            Page::EpicNext => &mut self.epicnext,
            Page::RemoveCms => &mut self.remove_cms,
            Page::Gitea => &mut self.gitea,
            Page::RemoveGitea => &mut self.remove_gitea,
            Page::Fail2Ban => &mut self.fail2ban,
            Page::RemoveFail2Ban => &mut self.remove_fail2ban,
            Page::CrowdSec => &mut self.crowdsec,
            Page::RemoveCrowdSec => &mut self.remove_crowdsec,
            Page::DefaultCatalog => &mut self.default_catalog,
            Page::DeployAssets => &mut self.deploy,
            _ => return None,
        })
    }

    fn all(&self) -> [&Wizard; 13] {
        [
            &self.install,
            &self.atom,
            &self.custom_cms,
            &self.epicnext,
            &self.remove_cms,
            &self.gitea,
            &self.remove_gitea,
            &self.fail2ban,
            &self.remove_fail2ban,
            &self.crowdsec,
            &self.remove_crowdsec,
            &self.default_catalog,
            &self.deploy,
        ]
    }
}

pub struct App {
    shared: Shared,
    page: Page,
    wizards: Wizards,
    login: views::login::LoginDialog,
    settings: views::settings::SettingsView,
    tools: views::tools::ToolsView,
    explorer: views::asset_explorer::AssetExplorerView,
    sql: views::sql_editor::SqlEditorView,
    debug: views::debug::DebugView,
    history: views::history::HistoryView,
    about: views::about::AboutView,
    dashboard: views::dashboard::Dashboard,
    /// The connection was lost: the login opens again as soon as nothing runs any more.
    login_pending: bool,
    last_watch: f64,
    known_session: Option<usize>,
    update_checked: bool,
    screenshot: screenshot::Screenshot,
}

fn session_id(server: &Option<Arc<dyn Server>>) -> Option<usize> {
    server.as_ref().map(|s| Arc::as_ptr(s) as *const () as usize)
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, runtime: tokio::runtime::Handle, dry_run: bool) -> Self {
        let ctx = cc.egui_ctx.clone();
        egui_extras::install_image_loaders(&ctx);
        ctx.set_theme(day_night::preference(day_night::saved().as_deref()));
        theme::install_fonts(&ctx);
        let dialogs = Dialogs::new(ctx.clone());
        let shared = Shared {
            runtime: runtime.clone(),
            dialogs: dialogs.clone(),
            ctx: ctx.clone(),
            session: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(t("Ready.").into())),
            dry_run,
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let wizards = views::wizards(&shared);
        let mut app = Self {
            page: Page::Dashboard,
            wizards,
            login: views::login::LoginDialog::new(&shared),
            settings: views::settings::SettingsView::new(&shared),
            tools: views::tools::ToolsView::new(&shared),
            explorer: views::asset_explorer::AssetExplorerView::new(&shared),
            sql: views::sql_editor::SqlEditorView::new(&shared),
            debug: views::debug::DebugView::new(&shared),
            history: views::history::HistoryView::new(&shared),
            about: views::about::AboutView::new(&shared),
            dashboard: views::dashboard::Dashboard::new(&shared),
            shared,
            login_pending: false,
            last_watch: 0.0,
            known_session: None,
            update_checked: false,
            screenshot: screenshot::Screenshot::from_env(),
        };
        if app.screenshot.active() {
            app.update_checked = true;
        }
        app
    }

    fn any_busy(&self) -> bool {
        self.wizards.all().iter().any(|w| w.is_busy())
            || self.tools.busy()
            || self.explorer.busy()
            || self.sql.busy()
            || self.settings.busy()
    }

    /// Opens the login (at startup, from a Log in button, or after a lost connection).
    pub fn show_login(&mut self, notice: Option<String>) {
        if self.login.is_open() || self.any_busy() || self.shared.session().is_some() {
            return;
        }
        self.login.open(notice);
    }

    /// The Disconnect button: closes the login; the pill then logs in again, to this server or another one.
    fn disconnect(&mut self) {
        let Some(session) = self.shared.session() else { return };
        if self.any_busy() {
            self.shared.set_status(t("Wait until the running step has finished before disconnecting."));
            return;
        }
        *self.shared.session.lock().unwrap() = None;
        // A drop noticed while closing is not a lost connection: no login pops up.
        self.login_pending = false;
        let host = session.login().host.clone();
        self.shared.spawn(async move { session.disconnect().await });
        self.shared.set_status(&tf("Disconnected from {host}. Log in with the button at the top right.", &[("host", &host)]));
    }

    fn set_session(&mut self, server: Arc<dyn Server>) {
        let login = server.login().clone();
        let user = server.login_user();
        let old = self.shared.session.lock().unwrap().replace(server);
        if let Some(old) = old {
            self.shared.spawn(async move { old.disconnect().await });
        }
        let who = if user == "root" { "root".to_string() } else { tf("{user} (root through sudo)", &[("user", &user)]) };
        self.shared.set_status(&tf("Connected to {host} as {who}.", &[("host", &login.host), ("who", &who)]));
    }

    /// Notices a connection the server dropped, and opens the login again once nothing runs any more.
    fn watch_session(&mut self, now: f64) {
        if now - self.last_watch < 3.0 {
            return;
        }
        self.last_watch = now;
        let lost = self.shared.session.lock().unwrap().clone().filter(|s| !s.is_connected());
        if let Some(lost) = lost {
            *self.shared.session.lock().unwrap() = None;
            self.shared.set_status(&tf("The connection to {host} was lost.", &[("host", &lost.login().host)]));
            self.login_pending = true;
        }
        if self.login_pending && !self.any_busy() && !self.login.is_open() {
            self.login_pending = false;
            let host = self.login.last_host().unwrap_or_else(|| t("the server").to_string());
            self.show_login(Some(tf("The connection to {host} was lost", &[("host", &host)])));
        }
    }

    fn session_changed(&mut self) {
        let shared = self.shared.clone();
        self.tools.session_changed(&shared);
        self.explorer.session_changed(&shared);
        self.sql.session_changed(&shared);
        self.debug.session_changed(&shared);
        self.history.session_changed(&shared);
        self.settings.session_changed(&shared);
        self.dashboard.session_changed(&shared);
        // A wizard on screen follows the new login (or the lack of one).
        let session = shared.session();
        if let Some(wizard) = self.wizards.get(self.page) {
            wizard.open(session.as_ref());
        }
    }

    pub fn go(&mut self, page: Page) {
        if self.any_busy() && page != self.page {
            return;
        }
        self.page = page;
        let session = self.shared.session();
        if let Some(wizard) = self.wizards.get(page) {
            wizard.open(session.as_ref());
        }
        match page {
            Page::Settings => self.settings.refresh(&self.shared),
            Page::Tools => self.tools.refresh(&self.shared),
            _ => {}
        }
    }

    /// Addons → CMS, Gitea, Fail2Ban and CrowdSec ask first what to do, unless one of their wizards runs.
    fn ask_addon(&mut self, addon: Page) {
        let choices: &[(Page, bool)] = match addon {
            Page::AtomCms => &[(Page::AtomCms, true), (Page::EpicNext, true), (Page::CustomCms, true), (Page::RemoveCms, true)],
            Page::Gitea => &[(Page::Gitea, true), (Page::RemoveGitea, true)],
            Page::Fail2Ban => &[(Page::Fail2Ban, true), (Page::RemoveFail2Ban, true)],
            _ => &[(Page::CrowdSec, true), (Page::RemoveCrowdSec, true)],
        };
        for (page, _) in choices {
            if self.wizards.get(*page).is_some_and(|w| w.is_busy()) {
                self.go(*page);
                return;
            }
        }
        let shared = self.shared.clone();
        let (title, heading, message, options): (&str, &str, &str, Vec<Choice<Option<Page>>>) = match addon {
            Page::AtomCms => (
                "CMS",
                t("Would you like another CMS?"),
                t("The CMS is the hotel's website: visitors register and log in there, and it opens the hotel (Octane) on /client. The installer sets up AtomCMS and EpicNext CMS completely. Your own CMS comes from a GitHub link or a ZIP file: the installer sets up NGINX (with PHP or as a static website), Traefik and Octane on /client for it, and you fill in its database settings."),
                vec![
                    Choice::new("AtomCMS", t("The CMS of github.com/ObjectRetros/atomcms, set up completely (recommended)."), Some(Page::AtomCms)).primary(),
                    Choice::new("EpicNext CMS", t("The Next.js CMS of gitea.epicnabbo.nl/Remco/EpicNext-Cms, built and run on the server (no PHP, no Docker)."), Some(Page::EpicNext)),
                    Choice::new(t("My own CMS"), t("Another CMS or website: from a GitHub link or a ZIP file."), Some(Page::CustomCms)),
                    Choice::new(t("Remove the CMS"), t("Octane is the website again, with its own login; NGINX and Traefik as after the server installation."), Some(Page::RemoveCms)),
                    Choice::new(t("Cancel"), t("Stay on this page."), None),
                ],
            ),
            Page::Gitea => (
                "Gitea",
                t("Gitea, your own Git server"),
                t("Install Gitea (or update it, when it is installed), or remove it."),
                vec![
                    Choice::new(t("Install or update Gitea"), t("On a domain of its own, such as git.example.com; an update keeps the repositories and accounts."), Some(Page::Gitea)).primary(),
                    Choice::new(t("Remove Gitea"), t("Stops it and removes its route; its repositories and accounts stay unless you choose to delete them."), Some(Page::RemoveGitea)),
                    Choice::new(t("Cancel"), t("Stay on this page."), None),
                ],
            ),
            Page::Fail2Ban => (
                "Fail2Ban",
                t("Fail2Ban, SSH brute-force protection"),
                t("Install Fail2Ban (or change its settings, when it is installed), or remove it. The whitelist and bans are also in Tools → Fail2Ban."),
                vec![
                    Choice::new(t("Install or change Fail2Ban"), t("Bans an address after too many failed SSH logins; your own IP goes on the whitelist."), Some(Page::Fail2Ban)).primary(),
                    Choice::new(t("Remove Fail2Ban"), t("Stops and uninstalls it; every ban is lifted."), Some(Page::RemoveFail2Ban)),
                    Choice::new(t("Cancel"), t("Stay on this page."), None),
                ],
            ),
            _ => (
                "CrowdSec",
                t("CrowdSec, attackers blocked in Traefik"),
                t("Install CrowdSec (or update it, when it is installed), or remove it. You need a free account on app.crowdsec.net: the step shows where to get the key."),
                vec![
                    Choice::new(t("Install or update CrowdSec"), t("In Docker next to Traefik, with the bouncer on every route; checks Cloudflare."), Some(Page::CrowdSec)).primary(),
                    Choice::new(t("Remove CrowdSec"), t("The bouncer out of Traefik and CrowdSec stopped; its data stays unless you choose otherwise."), Some(Page::RemoveCrowdSec)),
                    Choice::new(t("Cancel"), t("Stay on this page."), None),
                ],
            ),
        };
        let (title, heading, message) = (title.to_string(), heading.to_string(), message.to_string());
        let dialogs = shared.dialogs.clone();
        let requests = shared.clone();
        shared.spawn(async move {
            if let Some(page) = dialogs.choose(&title, &heading, &message, None, options).await {
                requests.request(Request::Go(page));
            }
        });
    }

    fn title(&self) -> (String, String) {
        let session = self.shared.session();
        let host = session.as_ref().map(|s| s.login().host.clone()).unwrap_or_else(|| t("the server").into());
        let gamedata = aio_remote::octane::gamedata();
        let (title, subtitle) = match self.page {
            Page::Install => (
                t("Install New Server").to_string(),
                if session.is_none() {
                    t("Log in to the Ubuntu / Debian server that will run the hotel.").to_string()
                } else {
                    tf("The full hotel stack on {host}: an SSH key for root first, then every part of the hotel.", &[("host", &host)])
                },
            ),
            Page::AtomCms => ("AtomCMS".into(), t("Addon: AtomCMS as the hotel's website, Octane on /client.").into()),
            Page::CustomCms => (t("Your own CMS").into(), t("Addon: your own CMS as the hotel's website, Octane on /client.").into()),
            Page::EpicNext => ("EpicNext CMS".into(), t("Addon: EpicNext CMS (Next.js) as the hotel's website, Octane on /client.").into()),
            Page::Gitea => ("Gitea".into(), t("Addon: your own Git server in Docker, on a domain of its own behind Traefik.").into()),
            Page::RemoveGitea => (t("Remove Gitea").into(), t("Addon: Gitea's container and route away; its data only when you choose.").into()),
            Page::Fail2Ban => ("Fail2Ban".into(), t("Addon: bans addresses that guess SSH passwords; your whitelist is never banned.").into()),
            Page::RemoveFail2Ban => (t("Remove Fail2Ban").into(), t("Addon: fail2ban stopped and uninstalled; every ban is lifted.").into()),
            Page::CrowdSec => ("CrowdSec".into(), t("Addon: blocks attackers in Traefik, with the community's blocklist; Cloudflare aware.").into()),
            Page::RemoveCrowdSec => (t("Remove CrowdSec").into(), t("Addon: the bouncer out of Traefik and CrowdSec stopped; its data only when you choose.").into()),
            Page::DefaultCatalog => (
                t("Default catalogue").into(),
                t("Addon: a complete catalogue for a new hotel, with its furniture list and FurnitureData.json.").into(),
            ),
            Page::RemoveCms => (t("Remove the CMS").into(), t("Addon: Octane as the hotel's website again, as after the server installation.").into()),
            Page::AssetExplorer => (
                "Asset Explorer".into(),
                if session.is_none() {
                    tf("Addon: manage the files in {gamedata}, with this computer next to it.", &[("gamedata", &gamedata)])
                } else {
                    tf("This computer on the left, {host}:{gamedata} on the right.", &[("host", &host), ("gamedata", &gamedata)])
                },
            ),
            Page::SqlEditor => (
                "SQL Editor".into(),
                if session.is_none() {
                    t("Addon: a database client like HeidiSQL, through the SSH login (no open MariaDB port needed).").into()
                } else {
                    tf("The databases of {host}: browse and edit tables, run queries.", &[("host", &host)])
                },
            ),
            Page::DeployAssets => (
                t("Deploy Assets").into(),
                tf("Addon: copy the hotel's assets, the client's UI texts and the emulator's images to {gamedata} on the server.", &[("gamedata", &gamedata)]),
            ),
            Page::Debug => (
                "Debug".into(),
                if session.is_none() {
                    t("Live logs of the emulator, NGINX, Traefik, the firewall, Fail2Ban and CrowdSec.").into()
                } else {
                    tf("Live logs on {host}: the emulator, NGINX, Traefik, the firewall, Fail2Ban and CrowdSec.", &[("host", &host)])
                },
            ),
            Page::Tools => (t("Tools").into(), t("Restart services, back up and import the database, and update.").into()),
            Page::Doctor => (t("Health check").into(), tf("How {host} is doing: services, disk, certificate, backups, updates.", &[("host", &host)])),
            Page::ServerStatus => (t("Server status").into(), tf("Live figures of {host}, every 2 seconds.", &[("host", &host)])),
            Page::Terminal => (
                t("SSH terminal").into(),
                tf(
                    "A shell on {host}, logged in as {user}.",
                    &[("host", &host), ("user", &session.as_ref().map(|s| s.login().user.clone()).unwrap_or_else(|| "root".into()))],
                ),
            ),
            Page::EmulatorMonitor => (
                t("Emulator & wired").into(),
                tf("The emulator on {host}: CPU per thread, the wired engine and slow wired rooms.", &[("host", &host)]),
            ),
            Page::History => (t("History").into(), t("What ran on which server, with its log: every install step and tool.").into()),
            Page::About => (t("About").into(), "All-in-1 Installer".into()),
            Page::Settings => (t("Settings").into(), t("The look, the settings backup, your servers and default SSH key, your own SSH keys, and the All-in-1 downloader.").into()),
            Page::Dashboard => (t("Dashboard").into(), t("Set up a complete hotel server over SSH.").into()),
            Page::Addons => (t("Addons").into(), t("Everything that goes on top of a finished server: pick one.").into()),
        };
        (title, subtitle)
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .exact_width(236.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(Fixed::sidebar()).inner_margin(egui::Margin::symmetric(14, 22)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    logo(ui);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        if theme::hotel() {
                            let text = RichText::new("All-in-1 Installer").size(17.0).family(theme::title_family()).color(Fixed::hotel_yellow());
                            let galley_pos = ui.cursor().min;
                            ui.painter().text(
                                galley_pos + egui::vec2(0.0, 2.0),
                                egui::Align2::LEFT_TOP,
                                "All-in-1 Installer",
                                egui::FontId::new(17.0, theme::title_family()),
                                Color32::from_black_alpha(0xB0),
                            );
                            ui.label(text);
                        } else {
                            ui.label(RichText::new("All-in-1 Installer").size(15.0).strong().color(Color32::WHITE));
                        }
                        ui.label(RichText::new(t("Hotel server setup")).size(11.5).color(Fixed::sidebar_muted()));
                    });
                });
                ui.add_space(26.0);
                let mut clicked = None;
                let current = self.page;
                let selected = |page: Page| match page {
                    // Every addon page belongs to Addons.
                    Page::Addons => matches!(
                        current,
                        Page::Addons
                            | Page::AtomCms
                            | Page::CustomCms
                            | Page::EpicNext
                            | Page::RemoveCms
                            | Page::Gitea
                            | Page::RemoveGitea
                            | Page::Fail2Ban
                            | Page::RemoveFail2Ban
                            | Page::CrowdSec
                            | Page::RemoveCrowdSec
                            | Page::DefaultCatalog
                            | Page::DeployAssets
                            | Page::AssetExplorer
                            | Page::SqlEditor
                    ),
                    Page::Tools => matches!(current, Page::Tools | Page::ServerStatus | Page::EmulatorMonitor | Page::Terminal | Page::Doctor),
                    other => current == other,
                };
                let mut nav = |ui: &mut egui::Ui, label: &str, icon: Icon, page: Page| {
                    if nav_button(ui, label, icon, selected(page)).clicked() {
                        clicked = Some(page);
                    }
                };
                nav(ui, t("Dashboard"), Icon::Home, Page::Dashboard);
                // Each group folds away with a click on its heading; a folded group still shows the page you are on.
                let groups: [(&str, &str, Vec<(&str, Icon, Page)>); 3] = [
                    // The addons are one page of picture buttons, not a long list here.
                    (
                        "sidebar:server",
                        t("SERVER"),
                        vec![(t("Install New Server"), Icon::ServerPlus, Page::Install), (t("Addons"), Icon::Puzzle, Page::Addons)],
                    ),
                    ("sidebar:tools", t("TOOLS"), vec![(t("Tools"), Icon::Wrench, Page::Tools), ("Debug", Icon::Bug, Page::Debug), (t("History"), Icon::History, Page::History)]),
                    ("sidebar:general", t("GENERAL"), vec![(t("Settings"), Icon::Cog, Page::Settings), (t("About"), Icon::Info, Page::About)]),
                ];
                for (key, heading, items) in groups {
                    ui.add_space(14.0);
                    let open = collapse::heading(ui, key, heading, 10.5, Fixed::sidebar_muted(), 12.0);
                    ui.add_space(2.0);
                    for (label, icon, page) in items {
                        if open || selected(page) {
                            nav(ui, label, icon, page);
                        }
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.label(RichText::new(t("Targets Ubuntu Server and Debian")).size(11.5).color(Fixed::sidebar_muted()));
                    ui.add_space(4.0);
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, Fixed::sidebar_hover());
                });
                if let Some(page) = clicked {
                    self.navigate(page);
                }
            });
    }

    /// A sidebar or tile click.
    fn navigate(&mut self, page: Page) {
        if self.any_busy() {
            return;
        }
        match page {
            Page::AtomCms | Page::Gitea | Page::Fail2Ban | Page::CrowdSec => self.ask_addon(page),
            other => self.go(other),
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let (title, subtitle) = self.title();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(widgets::title_text(&title, 28.0));
                ui.label(RichText::new(subtitle).size(13.5).color(palette.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let session = self.shared.session();
                language_picker::picker(ui, palette);
                ui.add_space(10.0);
                day_night::switch(ui);
                ui.add_space(4.0);
                if session.is_some() && ui.add(widgets::secondary_button(t("Disconnect"))).on_hover_text(t("Close the connection to the server; log in again (to this or another server) with the button next to it")).clicked() {
                    self.disconnect();
                }
                let (text, back, fore) = match &session {
                    Some(s) => {
                        let host = &s.login().host;
                        let user = s.login_user();
                        let text = if user == "root" { tf("Root on {host}", &[("host", host)]) } else { tf("{user} (sudo) on {host}", &[("user", &user), ("host", host)]) };
                        let text = if s.is_dry_run() { format!("{text} · {}", t("dry run")) } else { text };
                        (text, palette.success_back, palette.success_text)
                    }
                    None if self.login.is_open() => (t("Logging in").to_string(), palette.info_back, palette.info_text),
                    None => (t("Not connected · Log in").to_string(), palette.warning_back, palette.warning_text),
                };
                let pill = status_pill(ui, &text, back, fore);
                if session.is_none() {
                    let response = pill.interact(egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(t("Log in to your server"));
                    if response.clicked() {
                        self.show_login(None);
                    }
                }
            });
        });
    }

    fn footer(&self, ctx: &egui::Context, palette: &Palette) {
        egui::TopBottomPanel::bottom("footer")
            .frame(egui::Frame::new().fill(palette.card).inner_margin(egui::Margin::symmetric(32, 9)).stroke(egui::Stroke::new(1.0_f32, palette.card_border)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let wizard_status = match self.page {
                        Page::Dashboard | Page::Tools | Page::Settings | Page::About | Page::AssetExplorer | Page::SqlEditor | Page::Debug | Page::History => None,
                        _ => Some(()),
                    }
                    .and_then(|_| self.wizards.all().into_iter().find(|w| w.is_busy()).map(|w| w.status()))
                    .filter(|s| s != t("Ready."));
                    let status = wizard_status.or_else(|| self.current_wizard_status()).unwrap_or_else(|| self.shared.status());
                    ui.add(egui::Label::new(RichText::new(status).size(12.0).color(palette.muted)).truncate());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = match self.shared.session() {
                            None => t("No server connected").to_string(),
                            Some(s) => format!(
                                "{}@{}:{}  ·  {}",
                                s.login_user(),
                                s.login().host,
                                s.login().port,
                                s.os().map(|o| o.pretty_name).unwrap_or_default()
                            ),
                        };
                        ui.label(RichText::new(text).size(12.0).color(palette.muted));
                    });
                });
            });
    }

    fn current_wizard_status(&self) -> Option<String> {
        let wizard = match self.page {
            Page::Install => &self.wizards.install,
            Page::AtomCms => &self.wizards.atom,
            Page::CustomCms => &self.wizards.custom_cms,
            Page::EpicNext => &self.wizards.epicnext,
            Page::RemoveCms => &self.wizards.remove_cms,
            Page::Gitea => &self.wizards.gitea,
            Page::RemoveGitea => &self.wizards.remove_gitea,
            Page::Fail2Ban => &self.wizards.fail2ban,
            Page::RemoveFail2Ban => &self.wizards.remove_fail2ban,
            Page::CrowdSec => &self.wizards.crowdsec,
            Page::RemoveCrowdSec => &self.wizards.remove_crowdsec,
            Page::DefaultCatalog => &self.wizards.default_catalog,
            Page::DeployAssets => &self.wizards.deploy,
            _ => return None,
        };
        Some(wizard.status()).filter(|s| s != t("Ready."))
    }

    fn page(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let shared = self.shared.clone();
        let session = shared.session();
        let page = self.page;
        if let Some(wizard) = self.wizards.get(page) {
            let mut action = None;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(2.0);
                action = wizard.show(ui, palette, session.as_ref());
                ui.add_space(18.0);
            });
            match action {
                Some(WizardAction::Exit) => self.page = Page::Dashboard,
                Some(WizardAction::Login) => self.show_login(None),
                None => {}
            }
            return;
        }
        let needs_login = match page {
            Page::AssetExplorer => Some(tf("The Asset Explorer works on {gamedata} of the server you are logged in to.", &[("gamedata", &aio_remote::octane::gamedata())])),
            Page::Debug => Some(t("Debug follows the logs on the server you are logged in to.").to_string()),
            Page::SqlEditor => Some(t("The SQL Editor reaches MariaDB through the login to the server.").to_string()),
            _ => None,
        };
        if let (Some(message), None) = (&needs_login, &session) {
            if not_connected_card(ui, palette, message) {
                self.show_login(None);
            }
            return;
        }
        match page {
            Page::AssetExplorer => self.explorer.show(ui, palette, &shared),
            Page::SqlEditor => self.sql.show(ui, palette, &shared),
            Page::Debug => self.debug.show(ui, palette, &shared),
            Page::History => self.history.show(ui, palette, &shared),
            Page::Tools | Page::ServerStatus | Page::EmulatorMonitor | Page::Terminal | Page::Doctor => {
                self.tools.set_page(page);
                self.tools.show(ui, palette, &shared);
                self.page = self.tools.page();
            }
            Page::Settings => {
                let tools = &mut self.tools;
                scroll(ui, |ui| self.settings.show_with(ui, palette, &shared, |ui| tools.settings_backup_card(ui, palette, &shared)))
            }
            Page::About => scroll(ui, |ui| self.about.show(ui, palette, &shared)),
            Page::Addons => {
                let mut clicked = None;
                scroll(ui, |ui| clicked = views::addons::show(ui, palette));
                if let Some(page) = clicked {
                    self.navigate(page);
                }
            }
            _ => {
                let mut clicked = None;
                scroll(ui, |ui| clicked = self.dashboard.show(ui, palette, &shared, &self.tools));
                match clicked {
                    Some(views::dashboard::Click::Login) => self.show_login(None),
                    Some(views::dashboard::Click::Page(page)) => self.navigate(page),
                    None => {}
                }
            }
        }
    }

    fn handle_requests(&mut self) {
        let requests: Vec<Request> = std::mem::take(&mut *self.shared.requests.lock().unwrap());
        for request in requests {
            match request {
                Request::Go(page) => self.go(page),
                Request::Login => self.show_login(None),
                Request::Update(component) => {
                    if !self.any_busy() && self.shared.session().is_some() {
                        self.go(Page::Tools);
                        self.tools.update(component, &self.shared);
                    }
                }
                Request::Restart(component) => {
                    if !self.any_busy() && self.shared.session().is_some() {
                        self.go(Page::Tools);
                        self.tools.restart(component, &self.shared);
                    }
                }
                Request::CheckUpdates => {
                    if !self.any_busy() {
                        self.tools.check_updates_again(&self.shared);
                    }
                }
                Request::ReloadVersions => self.dashboard.reload_versions(&self.shared),
                Request::RestartForLook => {
                    if self.any_busy() {
                        self.shared.set_status(t("Wait until the running step has finished, then restart."));
                    } else if crate::update::start_again() {
                        self.shared.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.shared.set_status(t("The installer could not start itself again: close it and start it yourself to see the new look."));
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::apply(ctx);
        let palette = Palette::current(ctx);
        let now = ctx.input(|i| i.time);

        if !self.update_checked {
            // The update check first: when the installer replaces itself there is nothing to log in to.
            self.update_checked = true;
            views::update_dialog::check_at_startup(&self.shared, self.login.opener());
        }
        if let Some(server) = self.login.take_connected() {
            self.set_session(server);
        }
        let id = session_id(&self.shared.session());
        if id != self.known_session {
            self.known_session = id;
            self.session_changed();
        }
        self.watch_session(now);
        self.handle_requests();
        if self.login.take_open_request() {
            self.show_login(None);
        }

        // Closing waits while a step runs.
        if ctx.input(|i| i.viewport().close_requested()) && self.any_busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.shared.set_status(t("Wait until the running step has finished before closing."));
        }

        self.sidebar(ctx);
        self.footer(ctx, &palette);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(palette.surface).inner_margin(egui::Margin { left: 32, right: 32, top: 26, bottom: 0 })).show(ctx, |ui| {
            self.header(ui, &palette);
            ui.add_space(20.0);
            self.page(ui, &palette);
        });

        self.login.show(ctx, &self.shared);
        self.shared.dialogs.show(ctx);
        let mut screenshot = std::mem::replace(&mut self.screenshot, screenshot::Screenshot::from_env());
        screenshot.tick(self, ctx);
        self.screenshot = screenshot;
        // The live pages (status, logs) and tasks ask for repaints themselves; this keeps the session watch going.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

fn scroll(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        add(ui);
        ui.add_space(18.0);
    });
}

/// "Not connected" with a Log in button; true when it was clicked.
fn not_connected_card(ui: &mut egui::Ui, palette: &Palette, message: &str) -> bool {
    let mut clicked = false;
    widgets::card(ui, palette, egui::Margin::symmetric(28, 24), |ui| {
        ui.label(RichText::new(t("Not connected")).size(17.0).strong());
        widgets::muted(ui, palette, &format!("{message} {}", t("Log in first: the login stays open until you close the program.")), 13.0);
        ui.add_space(6.0);
        clicked = ui.add(widgets::primary_button(t("Log in"))).clicked();
    });
    clicked
}

/// The login state at the top right: a dot and the text on a rounded background.
fn status_pill(ui: &mut egui::Ui, text: &str, back: Color32, fore: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::new(12.0, egui::FontFamily::Name("bold".into())), fore);
    let size = egui::vec2(11.0 + 8.0 + 7.0 + galley.size().x + 11.0, galley.size().y + 10.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 14.0, back);
    ui.painter().circle_filled(egui::pos2(rect.left() + 15.0, rect.center().y), 4.0, fore);
    ui.painter().galley(egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0), galley, fore);
    response
}

fn nav_button(ui: &mut egui::Ui, label: &str, icon: Icon, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    let fill = if selected {
        Fixed::sidebar_selected()
    } else if response.hovered() {
        Fixed::sidebar_hover()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    let text_color = if selected || response.hovered() { Color32::WHITE } else { Fixed::sidebar_text() };
    let icon_color = if selected { Fixed::sidebar_accent() } else { text_color };
    let icon_rect = egui::Rect::from_min_size(rect.left_center() + egui::vec2(12.0, -8.5), egui::vec2(17.0, 17.0));
    icons::image(icon, 17.0, icon_color).paint_at(ui, icon_rect);
    let font = if selected { egui::FontId::new(13.5, egui::FontFamily::Name("bold".into())) } else { egui::FontId::proportional(13.5) };
    ui.painter().text(rect.left_center() + egui::vec2(41.0, 0.0), egui::Align2::LEFT_CENTER, label, font, text_color);
    ui.add_space(3.0);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Hotel look: the hotel logo's yellow with a dark edge and a hard shadow. Professional look: the converter's indigo.
fn logo(ui: &mut egui::Ui) {
    let size = if theme::hotel() { 38.0 } else { 36.0 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    if theme::hotel() {
        painter.rect_filled(rect.translate(egui::vec2(0.0, 3.0)), 8.0, Color32::from_black_alpha(0x66));
        painter.rect_filled(rect, 8.0, Fixed::hotel_yellow());
        painter.rect_stroke(rect, 8.0, egui::Stroke::new(2.0_f32, Fixed::outline()), egui::StrokeKind::Inside);
        icons::image(Icon::Server, 19.0, Fixed::outline()).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(19.0, 19.0)));
    } else {
        painter.rect_filled(rect, 10.0, theme::hex("#4F5FE6"));
        icons::image(Icon::Server, 19.0, Color32::WHITE).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(19.0, 19.0)));
    }
}

pub fn open_url(url: &str) {
    #[cfg(windows)]
    let result = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    let _ = result;
}

/// Starts the window.
pub fn run(runtime: tokio::runtime::Handle, dry_run: bool) -> eframe::Result<()> {
    let icon = image::load_from_memory(include_bytes!("../../resources/app.ico")).ok().map(|image| {
        let image = image.to_rgba8();
        let (width, height) = image.dimensions();
        egui::IconData { rgba: image.into_raw(), width, height }
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("All-in-1 Installer")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([1000.0, 680.0])
        .with_drag_and_drop(true);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    eframe::run_native("All-in-1 Installer", options, Box::new(move |cc| Ok(Box::new(App::new(cc, runtime, dry_run)))))
}
