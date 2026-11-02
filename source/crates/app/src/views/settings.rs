//! Settings (SettingsView.cs): the look, the window language, the servers and default SSH key for the login, the
//! server layout with the emulator's account, the repository branches, importing your own SSH keys and the
//! All-in-1 downloader. Everything is stored in configuration/settings.json.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_core::config_dir;
use aio_core::layout::{AtomCmsPresence, ROOT_ACCOUNT};
use aio_core::settings::{self as core_settings, BRANCH_CHOICES, HOTEL_LOOK, PROFESSIONAL_LOOK};
use aio_core::{InstallerSettings, ServerLayout};
use aio_remote::server_users::{self, ServerUser};
use aio_remote::{Server, ServerLogin};
use eframe::egui::{self, RichText};

use super::dashboard::COMPONENTS;
use super::login::{import_private_key, is_valid_server, pick_key_file};
use crate::gui::dialogs::Dialogs;
use crate::gui::theme::{self, Palette};
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Request, Shared, View};
use crate::i18n::{self, n, t, tf, LANGUAGES};

type Banner = Option<(ResultKind, String, String)>;

fn banner(kind: ResultKind, heading: &str, message: &str) -> Banner {
    Some((kind, heading.to_string(), message.to_string()))
}

/// What the tasks of the page hand back.
#[derive(Default)]
struct Inbox {
    /// The users of the server, for the login they were read from.
    users: Option<(usize, Result<Vec<ServerUser>, String>)>,
    /// "Create a new user": the chosen account and what to show.
    account: Option<(String, Banner, bool)>,
    layout_result: Option<Banner>,
    import: Option<ImportOutcome>,
    downloader: Option<PathBuf>,
    creating: bool,
    importing: bool,
}

struct ImportOutcome {
    result: Banner,
    imported: bool,
    from_paste: bool,
}

/// A row of the key list.
struct KeyRow {
    name: String,
    kind: String,
    has_private: bool,
    detail: String,
}

/// The text boxes of the server layout card.
#[derive(Default)]
struct LayoutFields {
    emulator: String,
    logs: String,
    octane: String,
    renderer: String,
    gamedata: String,
    atom: String,
    atom_installed: usize,
    backups: String,
    database: String,
}

pub struct SettingsView {
    hotel_look: bool,
    look_result: Banner,
    look_restart: bool,
    language_result: Banner,

    servers: Vec<String>,
    default_server: Option<String>,
    new_server: String,
    keys: Vec<PathBuf>,
    default_key: Option<PathBuf>,
    defaults_result: Banner,

    layout: LayoutFields,
    account: String,
    users: Option<Vec<ServerUser>>,
    users_for: Option<usize>,
    layout_result: Banner,
    layout_restart: bool,

    branches: Vec<&'static str>,
    branches_result: Banner,

    key_rows: Vec<KeyRow>,
    import_for: String,
    paste: String,
    import_result: Banner,

    downloader: String,
    downloader_result: Banner,

    inbox: Arc<Mutex<Inbox>>,
}

fn session_id(server: &Option<Arc<dyn Server>>) -> Option<usize> {
    server.as_ref().map(|s| Arc::as_ptr(s) as *const () as usize)
}

fn save(settings: &InstallerSettings) -> Result<(), String> {
    settings.save().map_err(|e| e.to_string())
}

const ATOM_CHOICES: [&str; 3] = [n("Detect: look on the server"), n("Installed"), n("Not installed")];

impl SettingsView {
    pub fn new(_shared: &Shared) -> Self {
        let settings = InstallerSettings::load();
        let mut view = Self {
            hotel_look: !settings.is_professional_look(),
            look_result: None,
            look_restart: false,
            language_result: None,
            servers: Vec::new(),
            default_server: None,
            new_server: String::new(),
            keys: Vec::new(),
            default_key: None,
            defaults_result: None,
            layout: LayoutFields::default(),
            account: "emulator".into(),
            users: None,
            users_for: None,
            layout_result: None,
            layout_restart: false,
            branches: Vec::new(),
            branches_result: None,
            key_rows: Vec::new(),
            import_for: String::new(),
            paste: String::new(),
            import_result: None,
            downloader: String::new(),
            downloader_result: None,
            inbox: Arc::new(Mutex::new(Inbox::default())),
        };
        view.show_layout(&settings.layout.clone().unwrap_or_default());
        view.reload();
        view
    }

    /// Reloads the settings and the key folder, for when the page is shown again or settings were imported.
    pub fn refresh(&mut self, shared: &Shared) {
        self.reload();
        self.load_users(shared, false);
    }

    fn reload(&mut self) {
        let settings = InstallerSettings::load();
        self.show_servers(&settings);
        self.downloader = settings.downloader_path.clone().unwrap_or_default();
        self.show_downloader_state(false);
        self.keys = config_dir::all_private_keys();
        self.default_key = settings.default_key_path().filter(|p| self.keys.contains(p));
        if self.import_for.trim().is_empty() {
            if let Some(server) = settings.default_server.as_deref().filter(|s| !s.is_empty()) {
                self.import_for = ServerLogin::parse_host(server).0;
            }
        }
        self.branches = COMPONENTS.iter().map(|c| core_settings::choice_for(&settings.branch_for(c.2)).branch).collect();
        self.key_rows = key_rows(settings.default_key_path().as_deref());
    }

    fn show_servers(&mut self, settings: &InstallerSettings) {
        self.servers = settings.server_list();
        self.default_server = settings.default_server.clone();
    }

    fn show_layout(&mut self, layout: &ServerLayout) {
        self.layout = LayoutFields {
            emulator: layout.emulator_repository.clone(),
            logs: layout.emulator_logs.clone(),
            octane: layout.octane.clone(),
            renderer: layout.renderer.clone(),
            gamedata: layout.gamedata.clone(),
            atom: layout.atom_cms.clone(),
            atom_installed: match layout.atom_cms_installed {
                AtomCmsPresence::Detect => 0,
                AtomCmsPresence::Installed => 1,
                AtomCmsPresence::NotInstalled => 2,
            },
            backups: layout.backups.clone(),
            database: layout.database.clone(),
        };
        self.account = layout.emulator_account.clone();
    }

    /// Reads the users once per login (and again after creating one).
    fn load_users(&mut self, shared: &Shared, force: bool) {
        let session = shared.session();
        let Some(id) = session_id(&session) else {
            self.users = None;
            self.users_for = None;
            return;
        };
        if !force && self.users_for == Some(id) {
            return;
        }
        self.users_for = Some(id);
        self.users = None;
        let server = session.unwrap();
        let inbox = self.inbox.clone();
        shared.spawn(async move {
            let result = server_users::list(&*server).await.map_err(|e| e.to_string());
            inbox.lock().unwrap().users = Some((id, result));
        });
    }

    fn take_inbox(&mut self, shared: &Shared) {
        let mut inbox = self.inbox.lock().unwrap();
        if let Some((id, result)) = inbox.users.take() {
            if self.users_for == Some(id) {
                match result {
                    Ok(users) => self.users = Some(users),
                    Err(error) => {
                        self.users = Some(Vec::new());
                        self.layout_result = banner(ResultKind::Warning, t("Could not read the server's users"), &error);
                    }
                }
            }
        }
        if let Some(result) = inbox.layout_result.take() {
            self.layout_result = result;
        }
        let account = inbox.account.take();
        let import = inbox.import.take();
        let downloader = inbox.downloader.take();
        drop(inbox);
        if let Some((name, result, reload)) = account {
            self.account = name;
            self.layout_result = result;
            if reload {
                self.load_users(shared, true);
            }
        }
        if let Some(outcome) = import {
            self.import_result = outcome.result;
            if outcome.imported {
                if outcome.from_paste {
                    self.paste.clear();
                }
                self.reload();
            }
        }
        if let Some(path) = downloader {
            self.downloader = path.display().to_string();
            self.show_downloader_state(false);
        }
    }

    // ---- Look and language ----

    fn look_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        card_heading(ui, palette, t("Look"), t("How the installer looks. A change of look applies when the installer starts again; day or night changes at once."));
        ui.radio_value(&mut self.hotel_look, true, t("Hotel: the look of a Habbo hotel (blue, yellow, pixel titles, a hotel view on the dashboard)"));
        ui.radio_value(&mut self.hotel_look, false, t("Professional: the calm look of the All-in-1 Converter (indigo, soft cards)"));
        ui.horizontal(|ui| {
            if ui.add(widgets::primary_button(t("Save"))).clicked() {
                self.save_look();
            }
            if self.look_restart && ui.add(widgets::primary_button(t("Restart now"))).clicked() {
                shared.request(Request::RestartForLook);
            }
        });
        show_banner(ui, palette, &self.look_result);

        // Day or night: applies at once, like the switch at the top right.
        ui.add_space(8.0);
        ui.label(RichText::new(t("Day or night")).size(13.0).strong());
        let saved = crate::gui::day_night::saved();
        let mut choice = saved.clone();
        ui.horizontal(|ui| {
            ui.radio_value(&mut choice, None, t("Follow the system"));
            ui.radio_value(&mut choice, Some(crate::gui::day_night::LIGHT.to_string()), t("Day (light)"));
            ui.radio_value(&mut choice, Some(crate::gui::day_night::DARK.to_string()), t("Night (dark)"));
        });
        if choice != saved {
            crate::gui::day_night::choose(ui.ctx(), choice.as_deref());
        }
    }

    fn save_look(&mut self) {
        let mut settings = InstallerSettings::load();
        let hotel = self.hotel_look;
        settings.look = Some(if hotel { HOTEL_LOOK } else { PROFESSIONAL_LOOK }.to_string());
        if let Err(error) = save(&settings) {
            self.look_result = banner(ResultKind::Error, t("Not saved"), &error);
            return;
        }
        // The theme is read once at startup, so a different choice waits for a restart.
        let changed = hotel != theme::hotel();
        self.look_restart = changed;
        let look = theme::look_name(hotel);
        self.look_result = if changed {
            banner(
                ResultKind::Info,
                t("Saved"),
                &tf("The {look} look starts with the next start of the installer: Restart now, or later.", &[("look", t(look))]),
            )
        } else {
            banner(ResultKind::Success, t("Saved"), &tf("The {look} look is the one you see now.", &[("look", t(look))]))
        };
    }

    fn language_card(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        card_heading(
            ui,
            palette,
            t("Language"),
            t("The language of the window, right away. The step logs stay in English: they are what the server prints, and what gets pasted into a support question."),
        );
        let current = i18n::current();
        let mut chosen = None;
        // One button per language, with the flag of the Habbo hotel that speaks it.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            for lang in LANGUAGES {
                let selected = lang.code == current.code;
                let response = language_button(ui, palette, lang.native_name, lang.hotel_domain, selected).on_hover_text(lang.hotel_domain);
                if response.clicked() {
                    chosen = Some(*lang);
                }
            }
        });
        if let Some(lang) = chosen.filter(|l| l.code != current.code) {
            let mut settings = InstallerSettings::load();
            settings.language = Some(lang.code.to_string());
            match save(&settings) {
                Ok(()) => {
                    i18n::set(lang.code);
                    self.language_result = banner(ResultKind::Success, t("Saved"), &tf("The window is in {language} now.", &[("language", lang.native_name)]));
                }
                Err(error) => self.language_result = banner(ResultKind::Error, t("Not saved"), &error),
            }
            ui.ctx().request_repaint();
        }
        show_banner(ui, palette, &self.language_result);
    }

    // ---- Servers and default key ----

    fn defaults_card(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        card_heading(
            ui,
            palette,
            t("Servers"),
            t("The IP addresses and DNS names of your servers. The login at startup fills in the default one and lists the others to choose from, and selects the default key when you log in with an SSH key. Changes are saved right away."),
        );
        if self.servers.is_empty() {
            ui.label(RichText::new(t("No servers yet: add one below.")).size(12.5).color(palette.subtle));
        }
        let mut make_default = None;
        let mut remove = None;
        for server in &self.servers {
            ui.horizontal(|ui| {
                ui.set_max_width(640.0);
                let is_default = self.default_server.as_deref() == Some(server.as_str());
                let caption = if is_default { tf("{server}   (default)", &[("server", server)]) } else { server.clone() };
                if ui.radio(is_default, caption).clicked() && !is_default {
                    make_default = Some(server.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(widgets::secondary_button(t("Remove"))).clicked() {
                        remove = Some(server.clone());
                    }
                });
            });
        }
        if let Some(server) = make_default {
            let message = tf("The login fills in {server}.", &[("server", &server)]);
            self.change_servers(|s| s.default_server = Some(server.clone()), t("Default server"), &message);
        }
        if let Some(server) = remove {
            let message = tf("{server} is no longer in the list.", &[("server", &server)]);
            self.change_servers(
                |s| {
                    let rest: Vec<String> = s.server_list().into_iter().filter(|x| !x.eq_ignore_ascii_case(&server)).collect();
                    // The next one becomes the default when the default is removed.
                    if s.default_server.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&server)) {
                        s.default_server = rest.first().cloned();
                    }
                    s.servers = rest;
                },
                t("Removed"),
                &message,
            );
        }
        label(ui, palette, t("Add a server (IP or hostname, with :port for another SSH port)"));
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.new_server)
                    .hint_text(RichText::new(t("192.168.1.10 or hotel.example.com (add :2222 for another SSH port)")).color(palette.subtle))
                    .desired_width(380.0),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add(widgets::primary_button(t("Add"))).clicked() || enter {
                self.add_server();
            }
        });
        label(ui, palette, t("SSH key (from configuration/keys)"));
        let selected = self.default_key.as_deref().map(key_item).unwrap_or_else(|| t("(no default key)").to_string());
        let mut chosen = None;
        egui::ComboBox::from_id_salt("settings-default-key").selected_text(selected).width(380.0).show_ui(ui, |ui| {
            if ui.selectable_label(self.default_key.is_none(), t("(no default key)")).clicked() {
                chosen = Some(None);
            }
            for key in &self.keys {
                if ui.selectable_label(self.default_key.as_ref() == Some(key), key_item(key)).clicked() {
                    chosen = Some(Some(key.clone()));
                }
            }
        });
        if let Some(key) = chosen {
            self.save_key(key);
        }
        if ui.add(widgets::secondary_button(t("Clear all"))).clicked() {
            let mut cleared = InstallerSettings::load();
            cleared.default_server = None;
            cleared.servers = Vec::new();
            cleared.default_key = None;
            match save(&cleared) {
                Ok(()) => {
                    self.reload();
                    self.defaults_result = banner(ResultKind::Info, t("No saved servers"), t("The login starts empty again."));
                }
                Err(error) => self.defaults_result = banner(ResultKind::Error, t("Not saved"), &error),
            }
        }
        show_banner(ui, palette, &self.defaults_result);
    }

    fn change_servers(&mut self, change: impl FnOnce(&mut InstallerSettings), heading: &str, message: &str) {
        let mut settings = InstallerSettings::load();
        settings.servers = settings.server_list();
        change(&mut settings);
        if let Err(error) = save(&settings) {
            self.defaults_result = banner(ResultKind::Error, t("Not saved"), &error);
            return;
        }
        self.show_servers(&settings);
        self.defaults_result = banner(ResultKind::Success, heading, message);
    }

    fn add_server(&mut self) {
        let server = self.new_server.trim().to_string();
        if server.is_empty() {
            return;
        }
        if !is_valid_server(&server) {
            self.defaults_result = banner(ResultKind::Error, t("That is not a valid server"), t("Use an IP address or hostname, optionally with :port."));
            return;
        }
        let settings = InstallerSettings::load();
        if settings.server_list().iter().any(|s| s.eq_ignore_ascii_case(&server)) {
            self.defaults_result = banner(ResultKind::Info, t("Already in the list"), &tf("{server} is saved already.", &[("server", &server)]));
            return;
        }
        let message = if settings.default_server.is_none() {
            tf("{server} is in the list and is the default.", &[("server", &server)])
        } else {
            tf("{server} is in the list. Tick it to make it the default.", &[("server", &server)])
        };
        let added = server.clone();
        self.change_servers(
            move |s| {
                s.servers.push(added.clone());
                // The first server is the default.
                if s.default_server.is_none() {
                    s.default_server = Some(added);
                }
            },
            t("Added"),
            &message,
        );
        self.new_server.clear();
        if self.import_for.trim().is_empty() {
            self.import_for = ServerLogin::parse_host(&server).0;
        }
    }

    fn save_key(&mut self, key: Option<PathBuf>) {
        let mut settings = InstallerSettings::load();
        settings.default_key = key.as_deref().map(core_settings::relative_key);
        if let Err(error) = save(&settings) {
            self.defaults_result = banner(ResultKind::Error, t("Not saved"), &error);
            return;
        }
        self.default_key = key.clone();
        self.key_rows = key_rows(key.as_deref());
        let message = match &key {
            None => t("No default key.").to_string(),
            Some(path) => tf("The login selects the key {name}.", &[("name", &file_name(path))]),
        };
        self.defaults_result = banner(ResultKind::Success, t("Saved"), &message);
    }

    // ---- Server layout ----

    fn layout_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        card_heading(
            ui,
            palette,
            t("Server layout"),
            t("Where the hotel lives on the server and the account the emulator runs as. The installer installs to these places, and the tools, addons and the dashboard look for everything there. Changing a value does not move anything on the server: move the folder yourself (or install again). It applies when the installer starts again."),
        );
        let session = shared.session();
        let connected = session.is_some();
        let users = if self.users_for.is_some() && self.users_for == session_id(&session) { self.users.as_deref() } else { None };
        let items = account_items(users, connected, &self.account);
        let mut create = false;
        let creating = self.inbox.lock().unwrap().creating;
        ui.add_space(6.0);
        egui::Grid::new("settings-layout").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            let layout = &mut self.layout;
            let row = |ui: &mut egui::Ui, name: &str, value: &mut String, hint: &str| {
                label(ui, palette, name);
                ui.add(egui::TextEdit::singleline(value).hint_text(RichText::new(hint).color(palette.subtle)).font(egui::TextStyle::Monospace).desired_width(380.0));
                ui.end_row();
            };
            row(ui, t("Emulator (Polaris clone)"), &mut layout.emulator, "/var/www/emulator");
            label(ui, palette, t("Emulator account"));
            let current = items.iter().find(|i| i.0.as_deref() == Some(self.account.as_str())).map(account_text).unwrap_or_default();
            ui.add_enabled_ui(!creating, |ui| {
                egui::ComboBox::from_id_salt("settings-account").selected_text(current).width(380.0).show_ui(ui, |ui| {
                    for item in &items {
                        let selected = item.0.as_deref() == Some(self.account.as_str());
                        if ui.selectable_label(selected, account_text(item)).clicked() {
                            match &item.0 {
                                Some(name) => self.account = name.clone(),
                                None => create = true,
                            }
                        }
                    }
                });
            });
            ui.end_row();
            let layout = &mut self.layout;
            let row = |ui: &mut egui::Ui, name: &str, value: &mut String, hint: &str| {
                label(ui, palette, name);
                ui.add(egui::TextEdit::singleline(value).hint_text(RichText::new(hint).color(palette.subtle)).font(egui::TextStyle::Monospace).desired_width(380.0));
                ui.end_row();
            };
            row(ui, t("Emulator logs"), &mut layout.logs, "/var/log/emu");
            row(ui, "Octane", &mut layout.octane, "/var/www/Octane");
            row(ui, "Octane-Renderer", &mut layout.renderer, "/var/www/Octane-Renderer");
            row(ui, t("Gamedata (assets, served on /gamedata)"), &mut layout.gamedata, "/var/www/gamedata");
            row(ui, "AtomCMS", &mut layout.atom, "/var/www/atomcms");
            label(ui, palette, t("AtomCMS installed"));
            egui::ComboBox::from_id_salt("settings-atom").selected_text(t(ATOM_CHOICES[layout.atom_installed])).width(380.0).show_ui(ui, |ui| {
                for (i, choice) in ATOM_CHOICES.iter().enumerate() {
                    ui.selectable_value(&mut layout.atom_installed, i, t(choice));
                }
            });
            ui.end_row();
            row(ui, t("Database backups"), &mut layout.backups, "/var/backups/all-in-1");
            row(ui, t("Hotel database"), &mut layout.database, "habbo");
        });
        // root works, but the emulator then has every right on the server.
        if self.account == ROOT_ACCOUNT {
            widgets::result_banner(
                ui,
                palette,
                ResultKind::Warning,
                t("Use a normal account for the emulator, not root"),
                t("As root the emulator (and every plugin in it) can change anything on the server: a bug, or someone who finds a hole in it, can take over the whole machine. A normal account such as emulator can only touch the hotel's files. The installer creates the account for you."),
            );
        }
        ui.horizontal(|ui| {
            if ui.add(widgets::primary_button(t("Save"))).clicked() {
                self.save_layout();
            }
            if ui.add(widgets::secondary_button(t("Defaults"))).clicked() {
                self.show_layout(&ServerLayout::default());
                self.layout_result = banner(ResultKind::Info, t("The defaults are filled in"), t("Save to use them."));
            }
            if self.layout_restart && ui.add(widgets::primary_button(t("Restart now"))).clicked() {
                shared.request(Request::RestartForLook);
            }
        });
        show_banner(ui, palette, &self.layout_result);
        if create {
            self.create_account(shared, session);
        }
    }

    /// "Create a new user": asks for the name, and creates the account when logged in.
    fn create_account(&mut self, shared: &Shared, session: Option<Arc<dyn Server>>) {
        let taken: Vec<String> = self.users.iter().flatten().map(|u| u.name.clone()).collect();
        let dialogs = shared.dialogs.clone();
        let inbox = self.inbox.clone();
        inbox.lock().unwrap().creating = true;
        shared.spawn(async move {
            let message = match &session {
                None => t("A service account (no login, home in /var/lib/<name>, member of www-data). You are not logged in, so the Emulator step creates it on the server.").to_string(),
                Some(server) => tf(
                    "A service account on {host}, created now: no login, home in /var/lib/<name>, member of www-data. The Emulator step gives it the emulator's folders.",
                    &[("host", &server.login().host)],
                ),
            };
            let check: Box<dyn Fn(&str) -> Option<String> + Send> = Box::new(move |text: &str| account_problem(text.trim(), &taken));
            let name = dialogs
                .input(t("New account for the emulator"), t("New account for the emulator"), &message, "", "hotel", false, t("Create"), Some(check))
                .await
                .map(|n| n.trim().to_string());
            let outcome = match (name, &session) {
                (None, _) => None,
                (Some(name), Some(server)) => match server_users::create(&**server, &name).await {
                    Err(error) => {
                        inbox.lock().unwrap().layout_result =
                            Some(banner(ResultKind::Error, &tf("Creating {name} failed", &[("name", &name)]), &error.to_string()));
                        None
                    }
                    Ok(()) => Some((
                        name.clone(),
                        banner(
                            ResultKind::Success,
                            &tf("{name} is created on {host}", &[("name", &name), ("host", &server.login().host)]),
                            t("Save to make it the emulator's account; the Emulator step gives it the emulator's folders."),
                        ),
                        true,
                    )),
                },
                (Some(name), None) => Some((
                    name.clone(),
                    banner(
                        ResultKind::Success,
                        &tf("{name} is chosen", &[("name", &name)]),
                        t("Save to make it the emulator's account; the Emulator step gives it the emulator's folders."),
                    ),
                    false,
                )),
            };
            let mut inbox = inbox.lock().unwrap();
            inbox.creating = false;
            inbox.account = outcome;
        });
    }

    fn save_layout(&mut self) {
        let fields = &self.layout;
        let mut layout = ServerLayout {
            emulator_repository: fields.emulator.clone(),
            emulator_account: self.account.clone(),
            emulator_logs: fields.logs.clone(),
            octane: fields.octane.clone(),
            renderer: fields.renderer.clone(),
            gamedata: fields.gamedata.clone(),
            atom_cms: fields.atom.clone(),
            atom_cms_installed: match fields.atom_installed {
                1 => AtomCmsPresence::Installed,
                2 => AtomCmsPresence::NotInstalled,
                _ => AtomCmsPresence::Detect,
            },
            backups: fields.backups.clone(),
            database: fields.database.clone(),
        };
        layout.normalize();
        if let Some(problem) = layout.validate() {
            self.layout_result = banner(ResultKind::Error, t("Not saved"), t(&problem));
            return;
        }
        let mut settings = InstallerSettings::load();
        settings.layout = Some(layout.clone());
        if let Err(error) = save(&settings) {
            self.layout_result = banner(ResultKind::Error, t("Not saved"), &error);
            return;
        }
        self.show_layout(&layout);
        // Like the look, the layout is read once at startup.
        let changed = &layout != ServerLayout::current();
        self.layout_restart = changed;
        self.layout_result = if changed {
            banner(ResultKind::Info, t("Saved"), t("The installer uses the new layout from its next start: Restart now, or later."))
        } else {
            banner(ResultKind::Success, t("Saved"), t("This is the layout the installer uses now."))
        };
    }

    // ---- Branches ----

    fn branches_card(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        card_heading(
            ui,
            palette,
            t("Repository branches"),
            t("Which branch Install New Server clones for each repository: main for the stable version, dev for the newest changes. The clone steps start with what is chosen here, and a change made there is saved back to here. An existing clone on another branch is switched over by the step, never silently."),
        );
        for (i, component) in COMPONENTS.iter().enumerate() {
            label(ui, palette, &format!("{}   (github.com/{})", component.0, component.2));
            let current = core_settings::choice_for(self.branches[i]);
            egui::ComboBox::from_id_salt(("settings-branch", i)).selected_text(branch_text(current.branch, current.caption)).width(380.0).show_ui(ui, |ui| {
                for choice in BRANCH_CHOICES {
                    ui.selectable_value(&mut self.branches[i], choice.branch, branch_text(choice.branch, choice.caption));
                }
            });
        }
        if ui.add(widgets::primary_button(t("Save"))).clicked() {
            let mut settings = InstallerSettings::load();
            for (i, component) in COMPONENTS.iter().enumerate() {
                settings.set_branch(component.2, self.branches[i]);
            }
            self.branches_result = match save(&settings) {
                Ok(()) => {
                    let list: Vec<String> = COMPONENTS.iter().enumerate().map(|(i, c)| format!("{}: {}", c.0, self.branches[i])).collect();
                    banner(ResultKind::Success, t("Saved"), &tf("{list}. The clone steps of the next installation start with this.", &[("list", &list.join(", "))]))
                }
                Err(error) => banner(ResultKind::Error, t("Not saved"), &error),
            };
        }
        show_banner(ui, palette, &self.branches_result);
    }

    // ---- SSH keys ----

    fn keys_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let folder = config_dir::directory().join("keys");
        card_heading(
            ui,
            palette,
            t("SSH keys"),
            &tf(
                "Keys in {folder}: the ones the SSH key step generated, and the ones you import here. Import your own private key to log in with it (\"SSH key\" in the login); a public key alone can be installed for root in the SSH key step.",
                &[("folder", &folder.display().to_string())],
            ),
        );
        if self.key_rows.is_empty() {
            ui.label(RichText::new(t("No keys yet.")).size(12.5).color(palette.muted));
        }
        for row in &self.key_rows {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&row.name).size(12.5).strong());
                ui.add_space(8.0);
                ui.label(RichText::new(&row.kind).size(12.0).color(if row.has_private { palette.success_text } else { palette.muted }));
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(&row.detail).size(12.0).color(palette.subtle)).truncate());
            });
        }
        ui.add_space(4.0);
        ui.label(RichText::new(t("Import a key")).size(13.5).strong());
        let importing = self.inbox.lock().unwrap().importing;
        ui.add_enabled_ui(!importing, |ui| {
            label(ui, palette, t("For server (the folder in configuration/keys)"));
            ui.add(egui::TextEdit::singleline(&mut self.import_for).hint_text(RichText::new(t("server, e.g. 203.0.113.10")).color(palette.subtle)).desired_width(260.0));
            if ui.add(widgets::secondary_button(t("Import key file ..."))).clicked() {
                self.import(shared, None);
            }
            label(ui, palette, t("Or paste a private key (OpenSSH, PEM, PuTTY .ppk) or a public key"));
            ui.add(
                egui::TextEdit::multiline(&mut self.paste)
                    .hint_text(RichText::new(t("-----BEGIN OPENSSH PRIVATE KEY----- ...   or a public key: ssh-ed25519 AAAA... you@computer")).color(palette.subtle))
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(7)
                    .desired_width(f32::INFINITY),
            );
            if ui.add(widgets::secondary_button(t("Import pasted key"))).clicked() {
                self.import(shared, Some(self.paste.clone()));
            }
        });
        show_banner(ui, palette, &self.import_result);
    }

    /// Imports a pasted key (`Some`) or a key file the user picks (`None`).
    fn import(&mut self, shared: &Shared, pasted: Option<String>) {
        let folder = ServerLogin::parse_host(&self.import_for).0;
        let dialogs = shared.dialogs.clone();
        let inbox = self.inbox.clone();
        inbox.lock().unwrap().importing = true;
        shared.spawn(async move {
            let from_paste = pasted.is_some();
            let outcome = match pasted {
                Some(text) => Some(import_key(&dialogs, &folder, &text, "pasted key").await),
                None => match pick_key_file(t("Select an SSH key (private or .pub)")).await {
                    Ok(Some((name, text))) => Some(import_key(&dialogs, &folder, &text, &name).await),
                    Ok(None) => None,
                    Err(error) => Some((banner(ResultKind::Error, t("That key file cannot be used"), &error), false)),
                },
            };
            let mut inbox = inbox.lock().unwrap();
            inbox.importing = false;
            if let Some((result, imported)) = outcome {
                if result.is_some() {
                    inbox.import = Some(ImportOutcome { result, imported, from_paste });
                }
            }
        });
    }

    // ---- Downloader ----

    fn downloader_card(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        card_heading(
            ui,
            palette,
            t("All-in-1 downloader"),
            t("The All-in-1 downloader (github.com/duckietm/all-in-1-converter) saves the Habbo assets in Habbo_Default next to its executable. Addons → Deploy Assets copies them from there to /var/www/gamedata on the server."),
        );
        label(ui, palette, t("Executable (Habbo Downloader.exe, or the Linux binary)"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.downloader).hint_text(RichText::new(t("e.g. C:\\Tools\\All-in-1\\Habbo Downloader.exe")).color(palette.subtle)).desired_width(ui.available_width() - 110.0));
            if ui.add(widgets::secondary_button(t("Browse ..."))).clicked() {
                let inbox = self.inbox.clone();
                shared.spawn(async move {
                    let file = rfd::AsyncFileDialog::new().set_title(t("Select the All-in-1 downloader (Habbo Downloader)")).pick_file().await;
                    if let Some(file) = file {
                        inbox.lock().unwrap().downloader = Some(file.path().to_path_buf());
                    }
                });
            }
        });
        if ui.add(widgets::primary_button(t("Save"))).clicked() {
            let mut settings = InstallerSettings::load();
            let path = clean_path(&self.downloader);
            settings.downloader_path = (!path.is_empty()).then_some(path);
            match save(&settings) {
                Ok(()) => self.show_downloader_state(true),
                Err(error) => self.downloader_result = banner(ResultKind::Error, t("Not saved"), &error),
            }
        }
        show_banner(ui, palette, &self.downloader_result);
    }

    fn show_downloader_state(&mut self, saved: bool) {
        let path = clean_path(&self.downloader);
        if path.is_empty() {
            self.downloader_result = if saved { banner(ResultKind::Info, t("No downloader set"), t("Deploy Assets needs it.")) } else { None };
            return;
        }
        self.downloader_result = match aio_remote::asset_sync::downloader_folder(Some(&path)) {
            Some(folder) => {
                let hab = folder.join(aio_remote::asset_sync::DEFAULT_FOLDER).join("hof_furni").join("hab");
                let furniture = std::fs::read_dir(&hab)
                    .map(|entries| entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("hab"))).count())
                    .unwrap_or(0);
                banner(
                    ResultKind::Success,
                    if saved { t("Saved") } else { t("All-in-1 downloader found") },
                    &tf(
                        "{folder}: {count} furniture bundles (.hab) so far.",
                        &[("folder", &folder.join(aio_remote::asset_sync::DEFAULT_FOLDER).display().to_string()), ("count", &furniture.to_string())],
                    ),
                )
            }
            None => banner(
                ResultKind::Warning,
                if saved { t("Saved, but Habbo_Default is not there") } else { t("Habbo_Default is not next to this file") },
                t("Choose the downloader's executable; its folder needs Habbo_Default (the downloader makes it on its first run)."),
            ),
        };
    }
}

impl View for SettingsView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.take_inbox(shared);
        let margin = egui::Margin::symmetric(24, 20);
        widgets::card(ui, palette, margin, |ui| self.look_card(ui, palette, shared));
        ui.add_space(18.0);
        widgets::card(ui, palette, margin, |ui| self.language_card(ui, palette));
        ui.add_space(18.0);
        widgets::card(ui, palette, margin, |ui| self.defaults_card(ui, palette));
        ui.add_space(18.0);
        widgets::card(ui, palette, margin, |ui| self.layout_card(ui, palette, shared));
        ui.add_space(18.0);
        widgets::card(ui, palette, margin, |ui| self.branches_card(ui, palette));
        ui.add_space(18.0);
        widgets::card(ui, palette, margin, |ui| self.keys_card(ui, palette, shared));
        ui.add_space(18.0);
        widgets::card(ui, palette, margin, |ui| self.downloader_card(ui, palette, shared));
    }

    /// Creating the emulator's account on the server runs.
    fn busy(&self) -> bool {
        self.inbox.lock().unwrap().creating
    }

    fn session_changed(&mut self, shared: &Shared) {
        self.load_users(shared, false);
    }
}

/// A language: the flag of its hotel and its own name. Sized before it is drawn, so the row wraps.
pub(crate) fn language_button(ui: &mut egui::Ui, palette: &Palette, name: &str, domain: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(name.to_string(), egui::FontId::new(13.0, egui::FontFamily::Name("bold".into())), palette.text);
    let size = egui::vec2(10.0 + 24.0 + 8.0 + galley.size().x + 12.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    let fill = if selected { palette.selected } else if response.hovered() { palette.hover } else { palette.card };
    painter.rect_filled(rect, 8.0, fill);
    let stroke = if selected { egui::Stroke::new(2.0_f32, palette.accent_text) } else { egui::Stroke::new(1.0_f32, palette.card_border) };
    painter.rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);
    let flag = egui::Rect::from_min_size(egui::pos2(rect.left() + 10.0, rect.center().y - 8.0), egui::vec2(24.0, 16.0));
    crate::gui::flags::paint(painter, flag, domain);
    painter.galley(egui::pos2(flag.right() + 8.0, rect.center().y - galley.size().y / 2.0), galley, palette.text);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn card_heading(ui: &mut egui::Ui, palette: &Palette, title: &str, text: &str) {
    ui.spacing_mut().item_spacing.y = 10.0;
    ui.label(widgets::title_text(title, 15.0));
    widgets::muted(ui, palette, text, 12.5);
}

fn label(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.0).strong().color(palette.muted));
}

fn show_banner(ui: &mut egui::Ui, palette: &Palette, result: &Banner) {
    if let Some((kind, heading, message)) = result {
        widgets::result_banner(ui, palette, *kind, heading, message);
    }
}

// The captions of aio_core::settings::BRANCH_CHOICES (translated in branch_text), copied here so source/tools/i18n_keys.py
// collects them. Keep in step with BRANCH_CHOICES.
const _: [&str; 2] = [n("stable, the version the hotel runs on"), n("the newest changes, not always stable")];

fn branch_text(branch: &str, caption: &str) -> String {
    format!("{branch}   ({})", t(caption))
}

fn clean_path(text: &str) -> String {
    text.trim().trim_matches('"').to_string()
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// "file   (keys/folder)", as the default key list shows a key.
fn key_item(path: &Path) -> String {
    let folder = path.parent().map(file_name).unwrap_or_default();
    format!("{}   (keys/{folder})", file_name(path))
}

/// What is wrong with a new account name, or None.
fn account_problem(name: &str, taken: &[String]) -> Option<String> {
    if !ServerLayout::is_valid_account(name) {
        Some(t("Use lower case letters, digits, '_' and '-', starting with a letter or '_'.").into())
    } else if name == ROOT_ACCOUNT {
        Some(t("root already exists, and is not recommended for the emulator.").into())
    } else if taken.iter().any(|u| u == name) {
        Some(tf("{name} already exists: pick it in the list.", &[("name", name)]))
    } else {
        None
    }
}

/// The account list: the server's users when logged in, the chosen account, and "Create a new user" (no name).
fn account_items(users: Option<&[ServerUser]>, connected: bool, account: &str) -> Vec<(Option<String>, String)> {
    let mut items: Vec<(Option<String>, String)> = users
        .unwrap_or_default()
        .iter()
        .map(|user| {
            let caption = if user.uid == 0 {
                t("administrator, not recommended").to_string()
            } else {
                let mut caption = if user.is_system() { t("service account").to_string() } else { tf("user, uid {uid}", &[("uid", &user.uid.to_string())]) };
                if user.in_www_data {
                    caption.push_str(t(", in www-data"));
                }
                if !user.can_log_in() {
                    caption.push_str(t(", no login"));
                }
                caption
            };
            (Some(user.name.clone()), caption)
        })
        .collect();
    if !items.iter().any(|i| i.0.as_deref() == Some(account)) {
        let caption = if !connected {
            t("log in to see the server's users")
        } else if users.is_none() {
            t("reading the server's users ...")
        } else {
            t("not on the server yet: the Emulator step creates it")
        };
        items.insert(0, (Some(account.to_string()), caption.to_string()));
    }
    items.push((None, if connected { t("+ Create a new user ...") } else { t("+ Create a new user (the Emulator step creates it) ...") }.to_string()));
    items
}

fn account_text(item: &(Option<String>, String)) -> String {
    match &item.0 {
        None => item.1.clone(),
        Some(name) => format!("{name}   ({})", item.1),
    }
}

/// The .pub files in configuration/keys, with their private key when it is next to them.
fn key_rows(default_key: Option<&Path>) -> Vec<KeyRow> {
    let root = config_dir::directory().join("keys");
    let mut files = Vec::new();
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|e| e == "pub") {
                files.push(path);
            }
        }
    }
    walk(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|public| {
            let text = std::fs::read_to_string(&public).ok()?;
            let key = aio_core::keys::try_parse_public_key(&text).ok()?;
            let private = public.with_extension("");
            let has_private = private.is_file();
            let folder = public.parent().map(file_name).unwrap_or_default();
            let name = format!("keys/{folder}/{}", file_name(if has_private { &private } else { &public }));
            let kind = if !has_private {
                t("public key only")
            } else if default_key == Some(private.as_path()) {
                t("private key · default")
            } else {
                t("private key")
            };
            Some(KeyRow { name, kind: kind.to_string(), has_private, detail: format!("{}  {}", key.key_type, key.fingerprint()) })
        })
        .collect()
}

/// A private key is stored with its public key; a public key on its own as provided_*.pub. Returns what to show
/// (None when the password question was cancelled) and whether a key was stored.
async fn import_key(dialogs: &Dialogs, folder: &str, text: &str, source: &str) -> (Banner, bool) {
    if folder.is_empty() {
        return (banner(ResultKind::Error, t("For which server?"), t("Enter the server the key is for: it becomes the folder in configuration/keys.")), false);
    }
    if text.trim().is_empty() {
        return (banner(ResultKind::Error, t("Nothing to import"), t("Paste a key or choose a key file.")), false);
    }
    if !aio_core::keys::is_private_key(text) {
        let key = match aio_core::keys::try_parse_public_key(text) {
            Ok(key) => key,
            Err(error) => return (banner(ResultKind::Error, t("Not an SSH key"), t(&error)), false),
        };
        return match config_dir::save_public_key(folder, &key) {
            Ok(saved) => (
                banner(
                    ResultKind::Success,
                    t("Public key imported"),
                    &tf(
                        "{saved}. The SSH key step can install it for root on {folder}. To log in with it, import the private key too.",
                        &[("saved", &saved.display().to_string()), ("folder", folder)],
                    ),
                ),
                true,
            ),
            Err(error) => (banner(ResultKind::Error, t("That key file cannot be used"), &error.to_string()), false),
        };
    }
    match import_private_key(dialogs, text, source, folder).await {
        Err(error) => (banner(ResultKind::Error, t("Not a usable private key"), &error), false),
        Ok(None) => (None, false),
        Ok(Some((path, key))) => (
            banner(
                ResultKind::Success,
                t("Private key imported"),
                &tf(
                    "{path} ({type}, {fingerprint}). Choose it as the default key above, or pick it under \"SSH key\" when logging in.",
                    &[("path", &path.display().to_string()), ("type", &key.key_type), ("fingerprint", &key.fingerprint())],
                ),
            ),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str, uid: i32, shell: &str, www: bool) -> ServerUser {
        ServerUser { name: name.into(), uid, home: format!("/home/{name}"), shell: shell.into(), in_www_data: www }
    }

    #[test]
    fn lists_the_accounts() {
        let offline = account_items(None, false, "emulator");
        assert_eq!(offline.len(), 2);
        assert_eq!(account_text(&offline[0]), "emulator   (log in to see the server's users)");
        assert_eq!(account_text(&offline[1]), "+ Create a new user (the Emulator step creates it) ...");
        assert_eq!(account_text(&account_items(None, true, "emulator")[0]), "emulator   (reading the server's users ...)");

        let users = vec![user("root", 0, "/bin/bash", false), user("emulator", 998, "/usr/sbin/nologin", true), user("zoe", 1001, "/bin/bash", false)];
        let items = account_items(Some(&users), true, "emulator");
        let texts: Vec<String> = items.iter().map(account_text).collect();
        assert_eq!(
            texts,
            vec![
                "root   (administrator, not recommended)",
                "emulator   (service account, in www-data, no login)",
                "zoe   (user, uid 1001)",
                "+ Create a new user ...",
            ]
        );
        let missing = account_items(Some(&users), true, "hotel");
        assert_eq!(account_text(&missing[0]), "hotel   (not on the server yet: the Emulator step creates it)");
    }

    #[test]
    fn checks_new_account_names() {
        let taken = vec!["emulator".to_string()];
        assert!(account_problem("Hotel", &taken).is_some());
        assert_eq!(account_problem("root", &taken).as_deref(), Some("root already exists, and is not recommended for the emulator."));
        assert_eq!(account_problem("emulator", &taken).as_deref(), Some("emulator already exists: pick it in the list."));
        assert_eq!(account_problem("hotel", &taken), None);
    }

    #[test]
    fn small_helpers() {
        assert_eq!(clean_path("  \"C:\\Tools\\dl.exe\" "), "C:\\Tools\\dl.exe");
        assert_eq!(key_item(Path::new("/c/keys/hotel.example.com/id_ed25519")), "id_ed25519   (keys/hotel.example.com)");
        assert_eq!(branch_text("dev", "the newest changes, not always stable"), "dev   (the newest changes, not always stable)");
    }
}
