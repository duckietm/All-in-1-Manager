//! The login at startup (LoginDialog.cs, SshKeyImport.cs): server, account and a password or an SSH key. A missing
//! key can be imported here. The connection, logged in with root rights, goes to the window; "Not now" continues
//! without a server. The update question at startup (update_dialog.rs) is drawn from here too, before the login.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aio_core::config_dir;
use aio_core::keys::AuthorizedKey;
use aio_core::saved_logins::SavedLogins;
use aio_core::InstallerSettings;
use aio_remote::server::ConnectStage;
use aio_remote::ssh::{self, KeyReadError};
use aio_remote::{Server, ServerError, ServerLogin};
use eframe::egui::{self, RichText};

use super::update_dialog::UpdatePrompt;
use crate::gui::dialogs::Dialogs;
use crate::gui::icons::{self, Icon};
use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::{self, Fixed, Palette};
use crate::gui::widgets::{self, ResultKind};
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

/// What the opener carries: a request to open the login, and the update question shown before it.
#[derive(Default)]
struct OpenerState {
    open: bool,
    update: Option<UpdatePrompt>,
}

/// Lets a task open the login (after the update check).
#[derive(Clone)]
pub struct LoginOpener(Arc<Mutex<OpenerState>>);

impl LoginOpener {
    pub fn open(&self) {
        self.0.lock().unwrap().open = true;
    }

    /// Shows "A new version is available"; the login opens when it is answered with Later.
    pub(crate) fn offer_update(&self, prompt: UpdatePrompt) {
        self.0.lock().unwrap().update = Some(prompt);
    }
}

/// What to fill in: the previous login (after a lost connection), never its passwords.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Prefill {
    host: String,
    user: String,
    use_key: bool,
    key_path: Option<PathBuf>,
}

/// What the tasks of the dialog (connecting, file pickers, the import) hand back to the window.
#[derive(Default)]
struct Inbox {
    stage: usize,
    log: Vec<String>,
    outcome: Option<Result<Arc<dyn Server>, String>>,
    browsed: Option<PathBuf>,
    imported: Option<Result<(PathBuf, String), String>>,
    /// A file picker or the key password question is open.
    picking: bool,
}

/// The dialog while it is open.
struct Form {
    servers: Vec<String>,
    host: String,
    user: String,
    password: String,
    key_password: String,
    use_key: bool,
    /// Remember the password on this computer (password logins only).
    remember_password: bool,
    /// What this computer remembers: the last server, its user and maybe its password.
    saved: SavedLogins,
    /// The server, user and password (None for a key login, or when it is not to be remembered) of the login under
    /// way, remembered once it succeeds.
    pending: Option<(String, String, Option<String>, bool)>,
    browsed: Vec<PathBuf>,
    picked: Option<PathBuf>,
    keys: Vec<PathBuf>,
    selected_key: Option<usize>,
    error: String,
    banner: Option<(ResultKind, String, String)>,
    busy: bool,
    checks_visible: bool,
    stage: usize,
    failed: bool,
    log: LogLines,
    focus_pending: bool,
}

pub struct LoginDialog {
    open: bool,
    request: Arc<Mutex<OpenerState>>,
    connected: Option<Arc<dyn Server>>,
    last_login: Option<Prefill>,
    form: Option<Form>,
    inbox: Arc<Mutex<Inbox>>,
}

impl LoginDialog {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            open: false,
            request: Arc::new(Mutex::new(OpenerState::default())),
            connected: None,
            last_login: None,
            form: None,
            inbox: Arc::new(Mutex::new(Inbox::default())),
        }
    }

    pub fn opener(&self) -> LoginOpener {
        LoginOpener(self.request.clone())
    }

    /// True once after a task asked for the login.
    pub fn take_open_request(&mut self) -> bool {
        std::mem::take(&mut self.request.lock().unwrap().open)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self, notice: Option<String>) {
        if self.open {
            return;
        }
        self.open = true;
        *self.inbox.lock().unwrap() = Inbox::default();
        let mut form = Form::new(self.last_login.as_ref());
        if let Some(notice) = notice {
            form.banner = Some((ResultKind::Warning, notice, t("Log in again to go on, or choose Not now.").into()));
        }
        self.form = Some(form);
    }

    /// The server of the last login, for "The connection to <host> was lost".
    pub fn last_host(&self) -> Option<String> {
        self.last_login.as_ref().map(|l| l.host.clone())
    }

    /// The new login, once.
    pub fn take_connected(&mut self) -> Option<Arc<dyn Server>> {
        self.connected.take()
    }

    pub fn show(&mut self, ctx: &egui::Context, shared: &Shared) {
        // The update question comes first: when the installer replaces itself there is nothing to log in to.
        let update_closed = {
            let mut state = self.request.lock().unwrap();
            match state.update.as_mut() {
                Some(prompt) => super::update_dialog::show(ctx, shared, prompt),
                None => false,
            }
        };
        if update_closed {
            let mut state = self.request.lock().unwrap();
            state.update = None;
            state.open = true;
        }
        if self.request.lock().unwrap().update.is_some() || !self.open {
            return;
        }
        let palette = Palette::current(ctx);
        self.take_inbox(shared);
        let Some(form) = self.form.as_mut() else { return };
        let picking = self.inbox.lock().unwrap().picking;
        let mut action = None;
        egui::Modal::new(egui::Id::new("login-dialog"))
            .frame(widgets::card_frame(&palette).inner_margin(egui::Margin::symmetric(28, 24)))
            .show(ctx, |ui| {
                ui.set_width(584.0);
                action = form.ui(ui, &palette, picking);
            });
        // Enter connects and Escape is "Not now", unless a question of the dialog is open on top.
        if action.is_none() && !shared.dialogs.is_open() && !picking {
            let (enter, escape) = ctx.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if enter && !form.busy {
                action = Some(Action::Connect);
            } else if escape && !form.busy {
                action = Some(Action::Later);
            }
        }
        match action {
            Some(Action::Connect) => self.connect(shared),
            Some(Action::Later) => {
                self.close();
                shared.set_status(t("Not connected. Log in with the button at the top right."));
            }
            Some(Action::Browse) => self.browse(shared),
            Some(Action::Import) => self.import(shared),
            None => {}
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.form = None;
    }

    /// What the tasks finished since the last frame.
    fn take_inbox(&mut self, shared: &Shared) {
        let Some(form) = self.form.as_mut() else { return };
        let mut inbox = self.inbox.lock().unwrap();
        for line in inbox.log.drain(..) {
            form.log.push(line);
        }
        if form.busy && inbox.stage > form.stage {
            form.stage = inbox.stage;
        }
        if let Some(path) = inbox.browsed.take() {
            form.browsed.retain(|p| p != &path);
            form.browsed.insert(0, path.clone());
            form.picked = Some(path);
            form.refresh_keys();
        }
        if let Some(imported) = inbox.imported.take() {
            match imported {
                Ok((path, key_type)) => {
                    form.picked = Some(path.clone());
                    form.refresh_keys();
                    form.banner = Some((
                        ResultKind::Success,
                        t("Key imported").into(),
                        tf("{path} ({type}). Enter its password below when it has one.", &[("path", &path.display().to_string()), ("type", &key_type)]),
                    ));
                }
                Err(error) => form.error = error,
            }
        }
        let outcome = inbox.outcome.take();
        drop(inbox);
        match outcome {
            Some(Ok(server)) => {
                if let Some((host, user, password, key)) = form.pending.take() {
                    if !shared.dry_run || std::env::var_os("AIO_SCREENSHOT").is_none() {
                        if key {
                            SavedLogins::remember_user(&host, &user);
                        } else {
                            SavedLogins::remember(&host, &user, password.as_deref());
                        }
                    }
                }
                let login = server.login();
                self.last_login = Some(Prefill {
                    host: host_text(&login.host, login.port),
                    user: login.user.clone(),
                    use_key: login.uses_key(),
                    key_path: login.private_keys.first().map(PathBuf::from),
                });
                self.connected = Some(server);
                self.close();
                shared.ctx.request_repaint();
            }
            Some(Err(message)) => {
                form.busy = false;
                form.failed = true;
                form.banner = Some((ResultKind::Error, t("Connection failed").into(), message));
            }
            None => {}
        }
    }

    fn connect(&mut self, shared: &Shared) {
        let Some(form) = self.form.as_mut() else { return };
        if form.busy {
            return;
        }
        if let Some(problem) = form.validate() {
            form.error = problem;
            return;
        }
        form.error.clear();
        let (host, port) = ServerLogin::parse_host(&form.host);
        let account = form.user.trim().to_string();
        form.pending = Some((form.host.trim().to_string(), account.clone(), (!form.use_key && form.remember_password).then(|| form.password.clone()), form.use_key));
        let login = if form.use_key {
            let key = form.keys[form.selected_key.unwrap_or(0)].to_string_lossy().into_owned();
            ServerLogin::key(&host, port, &account, vec![key], &form.key_password)
        } else {
            ServerLogin::password(&host, port, &account, &form.password)
        };
        form.busy = true;
        form.banner = None;
        form.checks_visible = true;
        form.stage = 0;
        form.failed = false;
        form.log.clear();
        {
            let mut inbox = self.inbox.lock().unwrap();
            inbox.stage = 0;
            inbox.log.clear();
            inbox.outcome = None;
        }
        let inbox = self.inbox.clone();
        let ctx = shared.ctx.clone();
        let dry_run = shared.dry_run;
        shared.spawn(async move {
            let log_inbox = inbox.clone();
            let log_ctx = ctx.clone();
            let log = move |line: &str| {
                log_inbox.lock().unwrap().log.push(line.to_string());
                log_ctx.request_repaint();
            };
            let stage_inbox = inbox.clone();
            let stage_ctx = ctx.clone();
            let stage = move |reached: ConnectStage| {
                let mut inbox = stage_inbox.lock().unwrap();
                inbox.stage = inbox.stage.max(stage_index(reached));
                stage_ctx.request_repaint();
            };
            let result: Result<Arc<dyn Server>, String> = if dry_run {
                log(&format!("> Connecting to {}:{} as {} (dry run: nothing reaches a server) ...", login.host, login.port, login.user));
                for reached in [ConnectStage::Login, ConnectStage::OperatingSystem, ConnectStage::Root] {
                    stage(reached);
                }
                let server: Arc<dyn Server> = crate::gui::dry_run::connect(login);
                Ok(server)
            } else {
                match ssh::connect(login, &log, &stage).await {
                    Ok(server) => {
                        let server: Arc<dyn Server> = server;
                        Ok(server)
                    }
                    Err(error) => Err(error_text(&error)),
                }
            };
            let mut inbox = inbox.lock().unwrap();
            if result.is_ok() {
                inbox.stage = 3;
            }
            inbox.outcome = Some(result);
        });
    }

    fn browse(&mut self, shared: &Shared) {
        self.inbox.lock().unwrap().picking = true;
        let inbox = self.inbox.clone();
        shared.spawn(async move {
            let file = rfd::AsyncFileDialog::new().set_title(t("Select your private SSH key")).pick_file().await;
            let mut inbox = inbox.lock().unwrap();
            inbox.picking = false;
            if let Some(file) = file {
                inbox.browsed = Some(file.path().to_path_buf());
            }
        });
    }

    fn import(&mut self, shared: &Shared) {
        let Some(form) = self.form.as_mut() else { return };
        let server = ServerLogin::parse_host(&form.host).0;
        if server.is_empty() {
            form.error = t("Enter the server first: the key is stored in a folder named after it.").into();
            form.focus_pending = true;
            return;
        }
        self.inbox.lock().unwrap().picking = true;
        let inbox = self.inbox.clone();
        let dialogs = shared.dialogs.clone();
        shared.spawn(async move {
            let result = async {
                let Some((name, text)) = pick_key_file(t("Select your private SSH key")).await? else { return Ok(None) };
                if !aio_core::keys::is_private_key(&text) {
                    return Err(tf(
                        "{name} is not a private key. Logging in needs the private key (a .pub file is only the public half).",
                        &[("name", &name)],
                    ));
                }
                Ok(import_private_key(&dialogs, &text, &name, &server).await?.map(|(path, key)| (path, key.key_type)))
            }
            .await;
            let mut inbox = inbox.lock().unwrap();
            inbox.picking = false;
            match result {
                Ok(Some(imported)) => inbox.imported = Some(Ok(imported)),
                Ok(None) => {}
                Err(error) => inbox.imported = Some(Err(error)),
            }
        });
    }
}

enum Action {
    Connect,
    Later,
    Browse,
    Import,
}

impl Form {
    fn new(prefill: Option<&Prefill>) -> Self {
        let settings = InstallerSettings::load();
        // The saved servers (Settings) to choose from, the default filled in; another one can be typed.
        let mut servers = settings.server_list();
        if let Some(earlier) = prefill.map(|p| p.host.as_str()).filter(|h| !h.is_empty()) {
            if !servers.iter().any(|s| s.eq_ignore_ascii_case(earlier)) {
                servers.insert(0, earlier.to_string());
            }
        }
        let saved = SavedLogins::load();
        let host = prefill
            .map(|p| p.host.clone())
            .or_else(|| saved.last_server.clone().filter(|s| !s.is_empty()))
            .or_else(|| settings.default_server.clone())
            .unwrap_or_default();
        if !host.is_empty() && !servers.iter().any(|s| s.eq_ignore_ascii_case(&host)) {
            servers.insert(0, host.clone());
        }
        let user = prefill.map(|p| p.user.clone()).or_else(|| saved.user(&host)).unwrap_or_else(|| "root".into());
        let remembered = saved.password(&host, &user);
        let use_key = prefill
            .map(|p| p.use_key)
            .unwrap_or_else(|| remembered.is_none() && (settings.default_key_path().is_some() || !config_dir::all_private_keys().is_empty()));
        let mut form = Self {
            servers,
            host,
            user,
            password: remembered.clone().unwrap_or_default(),
            key_password: String::new(),
            use_key,
            remember_password: remembered.is_some(),
            saved,
            pending: None,
            browsed: Vec::new(),
            picked: prefill.and_then(|p| p.key_path.clone()),
            keys: Vec::new(),
            selected_key: None,
            error: String::new(),
            banner: None,
            busy: false,
            checks_visible: false,
            stage: 0,
            failed: false,
            log: LogLines::with_limit(400),
            focus_pending: true,
        };
        form.refresh_keys();
        form
    }

    /// The key list: the key you picked, the default key from Settings, a key in a folder named after the server,
    /// the newest key.
    fn refresh_keys(&mut self) {
        let preset = InstallerSettings::load().default_key_path();
        let server = ServerLogin::parse_host(&self.host).0;
        let picked = self.picked.clone().filter(|p| p.is_file());
        self.keys = key_paths(&self.browsed, &config_dir::all_private_keys(), preset.as_deref(), picked.as_deref());
        self.selected_key = choose_key(&self.keys, self.picked.as_deref(), preset.as_deref(), &server);
    }

    /// Another server was chosen or typed: its remembered user, and its password when one is remembered.
    fn server_changed(&mut self) {
        if let Some(user) = self.saved.user(&self.host) {
            self.user = user;
        }
        match self.saved.password(&self.host, &self.user) {
            Some(password) => {
                self.password = password;
                self.remember_password = true;
            }
            None => {
                if self.remember_password {
                    self.password.clear();
                }
                self.remember_password = false;
            }
        }
    }

    fn validate(&self) -> Option<String> {
        if let Some(problem) = target_problem(&self.host, &self.user) {
            return Some(t(problem).into());
        }
        if !self.use_key {
            return self.password.is_empty().then(|| t("Enter the password.").into());
        }
        let Some(key) = self.selected_key.and_then(|i| self.keys.get(i)) else {
            return Some(t("Choose your private key: Import key or Browse.").into());
        };
        // Checked here so a wrong key password is a clear message instead of a refused login.
        match std::fs::read_to_string(key) {
            Err(error) => Some(error.to_string()),
            Ok(text) => ssh::public_key_of(&text, &self.key_password, "").err().map(|e| t(&e.to_string()).to_string()),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, palette: &Palette, picking: bool) -> Option<Action> {
        let mut action = None;
        let enabled = !self.busy && !picking;
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.horizontal(|ui| {
            egui::Frame::new().fill(theme::with_alpha(Fixed::brand(), 0x26)).corner_radius(12).inner_margin(12).show(ui, |ui| {
                icons::show(ui, Icon::Server, 22.0, Fixed::brand());
            });
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(widgets::title_text(t("Log in to your server"), 21.0));
                widgets::muted(ui, palette, t("One login for everything: installing, the addons and the tools. It stays open until you close the program."), 12.5);
            });
        });
        if let Some((kind, heading, message)) = &self.banner {
            widgets::result_banner(ui, palette, *kind, heading, message);
        }

        ui.add_enabled_ui(enabled, |ui| {
            label(ui, palette, t("Server IP / hostname (your servers from Settings in the list, or type another)"));
            let host_id = egui::Id::new("login-host");
            ui.horizontal(|ui| {
                let response = ui.add(
                    field(palette, &mut self.host, t("192.168.1.10 or hotel.example.com (add :2222 for another SSH port)"))
                        .id(host_id)
                        .desired_width(ui.available_width() - 44.0),
                );
                if response.changed() {
                    self.error.clear();
                }
                if response.lost_focus() {
                    self.server_changed();
                    if self.picked.is_none() {
                        self.refresh_keys();
                    }
                }
                let mut chosen = None;
                egui::ComboBox::from_id_salt("login-servers").selected_text("").width(28.0).show_ui(ui, |ui| {
                    if self.servers.is_empty() {
                        ui.label(RichText::new(t("No servers yet: add one below.")).color(palette.subtle));
                    }
                    for server in &self.servers {
                        if ui.selectable_label(server.eq_ignore_ascii_case(&self.host), server).clicked() {
                            chosen = Some(server.clone());
                        }
                    }
                });
                if let Some(server) = chosen {
                    self.host = server;
                    self.error.clear();
                    self.server_changed();
                    // Choosing another server from the list also picks its key.
                    if self.picked.is_none() && self.use_key {
                        self.refresh_keys();
                    }
                }
            });
            label(ui, palette, t("User"));
            if ui.add(field(palette, &mut self.user, t("root or a user with sudo rights")).desired_width(f32::INFINITY)).changed() {
                self.error.clear();
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("Log in with")).size(12.5).strong().color(palette.muted));
                ui.add_space(10.0);
                let before = self.use_key;
                ui.radio_value(&mut self.use_key, false, t("Password"));
                ui.add_space(10.0);
                ui.radio_value(&mut self.use_key, true, t("SSH key"));
                if before != self.use_key {
                    if self.use_key {
                        self.refresh_keys();
                    }
                    self.error.clear();
                }
            });
            let password_id = egui::Id::new("login-password");
            let key_password_id = egui::Id::new("login-key-password");
            if !self.use_key {
                label(ui, palette, t("Password (also for sudo when the account is not root)"));
                if ui.add(field(palette, &mut self.password, t("Password of the account")).password(true).id(password_id).desired_width(f32::INFINITY)).changed() {
                    self.error.clear();
                }
                ui.checkbox(&mut self.remember_password, RichText::new(t("Remember the password on this computer")).size(12.5)).on_hover_text(t(
                    "Kept encrypted in the configuration folder of this computer, readable by your account only, and filled in the next time you log in to this server as this user. Untick it and log in to forget it. SSH key logins never store a password.",
                ));
            } else {
                label(ui, palette, t("Private key"));
                ui.horizontal(|ui| {
                    let selected = self
                        .selected_key
                        .and_then(|i| self.keys.get(i))
                        .map(|p| key_label(p))
                        .unwrap_or_else(|| t("No keys yet: Import key or Browse").into());
                    let width = ui.available_width() - 250.0;
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("login-key").selected_text(selected).width(width.max(160.0)).show_ui(ui, |ui| {
                        for (i, key) in self.keys.iter().enumerate() {
                            if ui.selectable_label(self.selected_key == Some(i), key_label(key)).clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                    if let Some(i) = chosen {
                        self.selected_key = Some(i);
                        self.picked = Some(self.keys[i].clone());
                        self.error.clear();
                    }
                    if ui.add(widgets::secondary_button(t("Browse ..."))).clicked() {
                        action = Some(Action::Browse);
                    }
                    let import = if self.keys.is_empty() { widgets::primary_button(t("Import key ...")) } else { widgets::secondary_button(t("Import key ...")) };
                    if ui.add(import).clicked() {
                        action = Some(Action::Import);
                    }
                });
                if self.keys.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(t("There is no SSH key in configuration/keys yet. Import your private key (OpenSSH, PEM or PuTTY .ppk): it is copied to configuration/keys/<server> and stays on this computer."))
                                .size(12.5)
                                .color(palette.warning_text),
                        )
                        .wrap(),
                    );
                }
                label(ui, palette, t("Password of the key"));
                if ui
                    .add(field(palette, &mut self.key_password, t("Leave empty when the key has no password")).password(true).id(key_password_id).desired_width(f32::INFINITY))
                    .changed()
                {
                    self.error.clear();
                }
            }
            if self.focus_pending {
                self.focus_pending = false;
                let id = if self.host.trim().is_empty() {
                    host_id
                } else if self.use_key {
                    key_password_id
                } else {
                    password_id
                };
                ui.memory_mut(|m| m.request_focus(id));
            }
        });

        if !self.error.is_empty() {
            ui.add(egui::Label::new(RichText::new(&self.error).size(12.5).color(palette.error_text)).wrap());
        }
        if self.busy {
            indeterminate_bar(ui, palette);
        }
        if self.checks_visible {
            let labels = [t("Log in over SSH").to_string(), t("Check operating system").to_string(), t("Switch to root").to_string()];
            widgets::check_list(ui, palette, &labels, &[], self.stage, self.failed && !self.busy);
            if !self.log.is_empty() {
                egui::CollapsingHeader::new(RichText::new(t("Connection log")).size(12.0).strong().color(palette.muted))
                    .id_salt("login-log")
                    .default_open(false)
                    .show(ui, |ui| {
                        log_view::show(ui, "login", &mut self.log, Some(110.0), "");
                    });
            }
        }
        ui.add_space(4.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(enabled, widgets::primary_button(t("Connect"))).clicked() {
                action = Some(Action::Connect);
            }
            if ui.add_enabled(enabled, widgets::secondary_button(t("Not now"))).clicked() {
                action = Some(Action::Later);
            }
        });
        action
    }
}

fn field<'a>(palette: &Palette, text: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).hint_text(RichText::new(hint).color(palette.subtle)).font(egui::FontId::proportional(14.0)).margin(egui::Margin::symmetric(12, 8))
}

fn label(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).strong().color(palette.muted)).wrap());
}

/// The thin running bar while connecting.
fn indeterminate_bar(ui: &mut egui::Ui, palette: &Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 1.5, palette.hover);
    let time = ui.input(|i| i.time) as f32;
    let width = rect.width() * 0.3;
    let x = rect.left() - width + (time * 0.6).fract() * (rect.width() + width);
    let bar = egui::Rect::from_min_max(egui::pos2(x.max(rect.left()), rect.top()), egui::pos2((x + width).min(rect.right()), rect.bottom()));
    ui.painter().rect_filled(bar, 1.5, Fixed::brand());
    ui.ctx().request_repaint();
}

fn stage_index(stage: ConnectStage) -> usize {
    match stage {
        ConnectStage::Login => 0,
        ConnectStage::OperatingSystem => 1,
        ConnectStage::Root => 2,
    }
}

/// A failed login as the dialog shows it: the server's message as is, anything else with its cause.
fn error_text(error: &anyhow::Error) -> String {
    match error.downcast_ref::<ServerError>() {
        Some(server) => server.0.clone(),
        None => format!("{error:#}"),
    }
}

/// "host", "host:port" or "[ipv6]:port", as the login and Settings write a server.
fn host_text(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// What the key list shows: "file   (keys/folder)" for a key in the configuration folder, else the full path.
fn key_label(path: &Path) -> String {
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if path.starts_with(config_dir::directory()) {
        format!("{file}   (keys/{folder})")
    } else {
        path.display().to_string()
    }
}

/// The keys to choose from: the browsed ones, the ones in configuration/keys, the default key and the picked one.
fn key_paths(browsed: &[PathBuf], saved: &[PathBuf], preset: Option<&Path>, picked: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for path in browsed.iter().chain(saved) {
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    if let Some(preset) = preset.filter(|p| !paths.iter().any(|x| x == p)) {
        paths.insert(0, preset.to_path_buf());
    }
    if let Some(picked) = picked.filter(|p| !paths.iter().any(|x| x == p)) {
        paths.insert(0, picked.to_path_buf());
    }
    paths
}

/// The key to select: the picked one, the default one, one in a folder named after the server, else the first.
fn choose_key(paths: &[PathBuf], picked: Option<&Path>, preset: Option<&Path>, server: &str) -> Option<usize> {
    if paths.is_empty() {
        return None;
    }
    let folder = |p: &Path| p.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let server = server.to_lowercase();
    let for_host = if server.is_empty() {
        None
    } else {
        paths.iter().find(|p| folder(p) == server).or_else(|| paths.iter().find(|p| folder(p).contains(&server))).map(PathBuf::as_path)
    };
    let choose = picked.or(preset).or(for_host);
    Some(choose.and_then(|c| paths.iter().position(|p| p == c)).unwrap_or(0))
}

/// The C# Uri.CheckHostName: an IPv4 or IPv6 address, or a DNS name.
fn is_host_name(host: &str) -> bool {
    if host.parse::<std::net::Ipv4Addr>().is_ok() || host.parse::<std::net::Ipv6Addr>().is_ok() {
        return true;
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    !name.is_empty()
        && name.len() <= 255
        && name.split('.').all(|label| {
            !label.is_empty() && label.chars().count() <= 63 && !label.starts_with('-') && label.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        })
}

/// True for "host", "host:port" or "[ipv6]:port" with a usable host and port. (Settings uses it for its list.)
pub(crate) fn is_valid_server(text: &str) -> bool {
    let (host, port) = ServerLogin::parse_host(text);
    !host.is_empty() && !host.chars().any(char::is_whitespace) && is_host_name(&host) && port >= 1
}

/// What is wrong with the server and the account (English, for `t`), or None.
fn target_problem(host: &str, user: &str) -> Option<&'static str> {
    if host.trim().is_empty() {
        return Some(n("Enter the server IP address or hostname."));
    }
    let (server, port) = ServerLogin::parse_host(host);
    if server.is_empty() || server.chars().any(char::is_whitespace) || !is_host_name(&server) {
        return Some(n("That is not a valid IP address or hostname."));
    }
    if port < 1 {
        return Some(n("The SSH port must be between 1 and 65535."));
    }
    let account = user.trim();
    if account.is_empty() {
        return Some(n("Enter the user account to log in with."));
    }
    if account.chars().any(|c| c.is_whitespace() || c == ':' || c == '@') {
        return Some(n("A user name cannot contain spaces, ':' or '@'."));
    }
    None
}

/// Asks for a key file (in ~/.ssh when it exists) and reads it. None when cancelled. (SshKeyImport.PickFileAsync)
pub(crate) async fn pick_key_file(title: &str) -> Result<Option<(String, String)>, String> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    if let Some(ssh_folder) = home.map(|h| h.join(".ssh")).filter(|f| f.is_dir()) {
        dialog = dialog.set_directory(ssh_folder);
    }
    let Some(file) = dialog.pick_file().await else { return Ok(None) };
    let name = file.file_name();
    let path = file.path().to_path_buf();
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 64 * 1024 {
        return Err(tf("{name} is too large to be an SSH key.", &[("name", &name)]));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(Some((name, String::from_utf8_lossy(&bytes).into_owned())))
}

/// Stores a private key as it is (still protected by its password) with its public key next to it, in
/// configuration/keys/<folder>. The password is asked once to read the public key. None when that question is
/// cancelled; Err when it is not a usable private key. (SshKeyImport.ImportPrivateKeyAsync)
pub(crate) async fn import_private_key(dialogs: &Dialogs, text: &str, source: &str, folder: &str) -> Result<Option<(PathBuf, AuthorizedKey)>, String> {
    let mut passphrase = String::new();
    loop {
        match ssh::public_key_of(text, &passphrase, &format!("imported {source}")) {
            Ok(key) => {
                let path = config_dir::save_imported_key(folder, text, &key).map_err(|e| e.to_string())?;
                return Ok(Some((path, key)));
            }
            Err(error @ (KeyReadError::NeedsPassword | KeyReadError::WrongPassword)) => {
                let message = if passphrase.is_empty() {
                    t("Enter it once to read the public key. The key is stored as it is: still protected by its password.").to_string()
                } else {
                    tf("{error} Try again.", &[("error", t(&error.to_string()))])
                };
                let check: Box<dyn Fn(&str) -> Option<String> + Send> =
                    Box::new(|text: &str| text.is_empty().then(|| tf("Use at least {count} characters.", &[("count", "1")])));
                let entered = dialogs
                    .input(t("Key password"), t("This key has a password"), &message, t("Password"), "", true, t("OK"), Some(check))
                    .await;
                let Some(entered) = entered else { return Ok(None) };
                passphrase = entered;
            }
            Err(error) => return Err(t(&error.to_string()).to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_server_and_account() {
        assert_eq!(target_problem(" ", "root"), Some("Enter the server IP address or hostname."));
        assert_eq!(target_problem("bad host", "root"), Some("That is not a valid IP address or hostname."));
        assert_eq!(target_problem("hotel.example.com:0", "root"), Some("The SSH port must be between 1 and 65535."));
        assert_eq!(target_problem("hotel.example.com", " "), Some("Enter the user account to log in with."));
        assert_eq!(target_problem("10.0.0.2:2222", "a@b"), Some("A user name cannot contain spaces, ':' or '@'."));
        assert_eq!(target_problem("[::1]:2222", "simone"), None);
        assert!(is_valid_server("hotel.example.com"));
        assert!(is_valid_server("fe80::1"));
        assert!(!is_valid_server("host:99999"));
        assert!(!is_valid_server("-bad.example.com"));
        assert!(!is_valid_server("a..b"));
    }

    #[test]
    fn formats_the_host_like_the_window() {
        assert_eq!(host_text("hotel.example.com", 22), "hotel.example.com");
        assert_eq!(host_text("hotel.example.com", 2222), "hotel.example.com:2222");
        assert_eq!(host_text("::1", 2222), "[::1]:2222");
        assert_eq!(ServerLogin::parse_host(&host_text("::1", 2222)), ("::1".into(), 2222));
    }

    #[test]
    fn picks_the_key_for_the_server() {
        let a = PathBuf::from("/c/keys/10.0.0.1/id_ed25519");
        let b = PathBuf::from("/c/keys/hotel.example.com/id_ed25519");
        let browsed = PathBuf::from("/home/me/.ssh/id_rsa");
        let paths = key_paths(&[browsed.clone()], &[a.clone(), b.clone(), browsed.clone()], None, None);
        assert_eq!(paths, vec![browsed.clone(), a.clone(), b.clone()]);
        assert_eq!(choose_key(&paths, None, None, "hotel.example.com"), Some(2));
        assert_eq!(choose_key(&paths, None, None, "example"), Some(2));
        assert_eq!(choose_key(&paths, Some(&a), None, "hotel.example.com"), Some(1));
        assert_eq!(choose_key(&paths, None, Some(&a), "hotel.example.com"), Some(1));
        assert_eq!(choose_key(&paths, None, None, ""), Some(0));
        assert_eq!(choose_key(&[], None, None, "x"), None);
        let preset = PathBuf::from("/c/keys/x/default");
        assert_eq!(key_paths(&[], &[a.clone()], Some(&preset), None), vec![preset, a]);
    }

    #[test]
    fn server_errors_are_shown_as_is() {
        let error: anyhow::Error = ServerError("Permission denied.".into()).into();
        assert_eq!(error_text(&error), "Permission denied.");
        let other = anyhow::anyhow!("timed out").context("Could not connect");
        assert_eq!(error_text(&other), "Could not connect: timed out");
    }
}
