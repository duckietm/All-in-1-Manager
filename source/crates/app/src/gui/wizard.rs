//! Runs install steps one after the other on the server of the login at startup (ServerWizard.cs). "Install New
//! Server" starts with the SSH key step (an SSH key for root); the addons start with their own steps. Back returns to
//! the dashboard and keeps both the login and where the wizard was.

use std::path::PathBuf;
use std::sync::Arc;

use aio_remote::web_access::WebAccessSettings;
use aio_remote::Server;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::dialogs::Dialogs;
use super::icons::{self, Icon};
use super::step::{self, InstallStep, StepOutcome, StepUi};
use super::theme::{Fixed, Palette};
use super::widgets;
use crate::i18n::{t, tf};

/// How a step ended, for the finish page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepSummary {
    Done,
    AlreadyInstalled,
    Skipped,
    Failed,
    NotRun,
}

/// What the finish page is built from.
pub struct FinishInfo {
    pub host: String,
    pub ssh_port: u16,
    pub site: Option<WebAccessSettings>,
    pub putty_key: Option<PathBuf>,
    pub steps: Vec<(String, StepSummary)>,
}

/// The last wizard page.
pub trait FinishPage: Send {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette);
}

pub type FinishBuilder = Arc<dyn Fn(FinishInfo) -> Box<dyn FinishPage> + Send + Sync>;

/// A page of plain text (a failure while collecting the summary).
struct TextPage(String);

impl FinishPage for TextPage {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.label(RichText::new(&self.0).size(13.0).color(palette.error_text));
    }
}

enum Event {
    Detected { index: usize, found: bool },
    Ran { index: usize, outcome: StepOutcome },
    Finished { page: Box<dyn FinishPage>, site_url: Option<String> },
}

/// What the window does after a click in the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardAction {
    /// Back, or Abort after an aborted run: show the dashboard.
    Exit,
    /// "Log in" while there is no login.
    Login,
}

struct Entry {
    step: Arc<dyn InstallStep>,
    ui: StepUi,
}

pub struct Wizard {
    entries: Vec<Entry>,
    has_key_step: bool,
    index: usize,
    outcomes: Vec<Option<StepOutcome>>,
    // Failed this session and skipped afterwards, not fixed; for the summary.
    failed: Vec<bool>,
    already_done: Vec<bool>,
    busy: bool,
    // The login this wizard's progress belongs to; a new login starts it over.
    started_for: Option<usize>,
    finish_builder: FinishBuilder,
    finish_title: String,
    finish_caption: String,
    finish_page: Option<Box<dyn FinishPage>>,
    site_url: Option<String>,
    /// The address the finish page's Open button opens; without it, the hotel's.
    pub open_url: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    validation: String,
    status: Arc<Mutex<String>>,
    events: (mpsc::UnboundedSender<Event>, mpsc::UnboundedReceiver<Event>),
    runtime: tokio::runtime::Handle,
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl Wizard {
    /// `key_step`: the first step is the SSH key step (Install New Server).
    pub fn new(
        runtime: tokio::runtime::Handle,
        ctx: &egui::Context,
        dialogs: &Dialogs,
        steps: Vec<Arc<dyn InstallStep>>,
        key_step: bool,
        finish_builder: FinishBuilder,
        finish_caption: &str,
        finish_title: &str,
    ) -> Self {
        let status = Arc::new(Mutex::new(t("Ready.").to_string()));
        let entries: Vec<Entry> = steps
            .into_iter()
            .map(|step| {
                let ui = StepUi::new(ctx.clone(), dialogs.clone(), status.clone(), &step.title(), step.checks());
                Entry { step, ui }
            })
            .collect();
        let count = entries.len();
        Self {
            entries,
            has_key_step: key_step,
            index: 0,
            outcomes: vec![None; count],
            failed: vec![false; count],
            already_done: vec![false; count],
            busy: false,
            started_for: None,
            finish_builder,
            finish_title: finish_title.into(),
            finish_caption: finish_caption.into(),
            finish_page: None,
            site_url: None,
            open_url: None,
            validation: String::new(),
            status,
            events: mpsc::unbounded_channel(),
            runtime,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn status(&self) -> String {
        self.status.lock().clone()
    }

    fn set_status(&self, message: &str) {
        *self.status.lock() = message.to_string();
    }

    fn finish_index(&self) -> usize {
        self.entries.len()
    }

    fn on_key_step(&self) -> bool {
        self.has_key_step && self.index == 0
    }

    fn on_finish(&self) -> bool {
        self.index == self.finish_index()
    }

    /// Called each time the page is shown: starts the first step for a new login, or shows the step where the
    /// wizard was. (OpenAsync)
    pub fn open(&mut self, server: Option<&Arc<dyn Server>>) {
        if self.busy {
            return;
        }
        let Some(server) = server else { return };
        // Back from another page in the middle of a run continues it; a finished run starts over.
        if self.started_for == Some(server_id(server)) && !self.on_finish() {
            return;
        }
        self.started_for = Some(server_id(server));
        self.finish_page = None;
        self.outcomes.iter_mut().for_each(|o| *o = None);
        self.failed.iter_mut().for_each(|f| *f = false);
        self.already_done.iter_mut().for_each(|d| *d = false);
        self.start(server);
    }

    fn start(&mut self, server: &Arc<dyn Server>) {
        if self.entries.is_empty() {
            self.enter_finish(server);
        } else {
            self.enter_step(0, server);
        }
    }

    /// Opens step `index` (0-based) at once, for screenshots of the later steps.
    pub fn jump_to(&mut self, index: usize, server: &Arc<dyn Server>) {
        if index < self.entries.len() {
            self.started_for = Some(server_id(server));
            self.enter_step(index, server);
        }
    }

    fn enter_step(&mut self, index: usize, server: &Arc<dyn Server>) {
        self.index = index;
        self.outcomes[index] = None;
        let entry = &self.entries[index];
        entry.ui.show_intro();
        entry.step.enter(server.as_ref());
        let title = entry.step.title();
        self.set_status(&if self.on_key_step() {
            t("Checking the installed SSH keys ...").to_string()
        } else {
            tf("Checking {title} on the server ...", &[("title", &title)])
        });
        self.busy = true;
        let (step, ui, server, events) = (entry.step.clone(), entry.ui.clone(), server.clone(), self.events.0.clone());
        self.runtime.spawn(async move {
            let found = step::detect(step, server, ui.clone()).await;
            let _ = events.send(Event::Detected { index, found });
            ui.wake();
        });
    }

    fn run_step(&mut self, server: &Arc<dyn Server>) {
        let index = self.index;
        if let Some(problem) = self.entries[index].step.validate() {
            self.validation = problem;
            return;
        }
        self.validation.clear();
        self.busy = true;
        let entry = &self.entries[index];
        let (step, ui, server, events) = (entry.step.clone(), entry.ui.clone(), server.clone(), self.events.0.clone());
        self.runtime.spawn(async move {
            let outcome = step::run(step, server, ui.clone()).await;
            let _ = events.send(Event::Ran { index, outcome });
            ui.set_busy(false);
        });
    }

    // Title plus caption when another step shares the title (Octane clone / build).
    fn step_name(&self, i: usize) -> String {
        let title = self.entries[i].step.title();
        if self.entries.iter().filter(|e| e.step.title() == title).count() > 1 {
            format!("{title} ({})", self.entries[i].step.caption())
        } else {
            title
        }
    }

    fn enter_finish(&mut self, server: &Arc<dyn Server>) {
        self.index = self.finish_index();
        self.finish_page = None;
        self.busy = true;
        self.set_status(t("Collecting the summary ..."));
        let mut steps = Vec::new();
        for i in 0..self.entries.len() {
            let summary = if self.has_key_step && i == 0 {
                match self.outcomes[0] {
                    Some(StepOutcome::Done) => StepSummary::Done,
                    _ if self.already_done[0] => StepSummary::AlreadyInstalled,
                    Some(StepOutcome::Failed) => StepSummary::Failed,
                    _ => StepSummary::Skipped,
                }
            } else {
                match self.outcomes[i] {
                    Some(StepOutcome::Done) => StepSummary::Done,
                    _ if self.failed[i] => StepSummary::Failed,
                    _ if self.already_done[i] => StepSummary::AlreadyInstalled,
                    Some(StepOutcome::Skipped) => StepSummary::Skipped,
                    _ => StepSummary::NotRun,
                }
            };
            steps.push((if self.has_key_step && i == 0 { t("SSH key").to_string() } else { self.step_name(i) }, summary));
        }
        let (builder, server, events, open_url) = (self.finish_builder.clone(), server.clone(), self.events.0.clone(), self.open_url.clone());
        let status = self.status.clone();
        self.runtime.spawn(async move {
            let host = server.login().host.clone();
            let collected = aio_remote::web_access::inspect(server.as_ref()).await;
            let (page, site_url): (Box<dyn FinishPage>, Option<String>) = match collected {
                Ok(web) => {
                    let site_url = open_url.and_then(|f| f()).or_else(|| web.configured.as_ref().map(|s| s.url()));
                    let keys = aio_core::config_dir::keys_directory(&host);
                    let putty_key = newest_ppk(&keys);
                    let info = FinishInfo { host, ssh_port: server.login().port, site: web.configured, putty_key, steps };
                    *status.lock() = t("Installation completed.").to_string();
                    (builder(info), site_url)
                }
                Err(error) => {
                    *status.lock() = t("Collecting the summary failed.").to_string();
                    (Box::new(TextPage(tf("Collecting the summary failed: {error}", &[("error", &step::error_message(&error))]))), None)
                }
            };
            let _ = events.send(Event::Finished { page, site_url });
        });
    }

    /// Applies what the steps' tasks reported.
    fn poll(&mut self) {
        while let Ok(event) = self.events.1.try_recv() {
            match event {
                Event::Detected { index, found } => {
                    self.already_done[index] = found;
                    self.busy = false;
                    self.set_status(t("Ready."));
                }
                Event::Ran { index, outcome } => {
                    self.outcomes[index] = Some(outcome);
                    if !(self.has_key_step && index == 0) {
                        self.failed[index] = outcome == StepOutcome::Failed;
                    }
                    if outcome == StepOutcome::Aborted {
                        self.set_status(t("Installation aborted. Nothing was changed on the server."));
                    }
                    self.busy = false;
                }
                Event::Finished { page, site_url } => {
                    self.finish_page = Some(page);
                    self.site_url = site_url;
                    self.busy = false;
                }
            }
        }
    }

    /// Draws the wizard card; returns what the window should do.
    pub fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, server: Option<&Arc<dyn Server>>) -> Option<WizardAction> {
        self.poll();
        let connected = server.is_some();
        let mut action = None;
        widgets::card(ui, palette, egui::Margin::symmetric(28, 24), |ui| {
            self.stepper(ui, palette, connected);
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(14.0);

            if !connected && !self.busy {
                not_connected(ui, palette);
            } else if self.on_finish() {
                match &mut self.finish_page {
                    Some(page) => page.show(ui, palette),
                    None => {
                        ui.spinner();
                    }
                }
            } else {
                let entry = &self.entries[self.index];
                step::show(ui, palette, entry.step.as_ref(), &entry.ui);
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.validation).size(12.5).color(palette.error_text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right to left: the buttons are added in reverse.
                    let buttons = self.buttons(connected);
                    for button in buttons.into_iter().rev() {
                        let widget = if button.primary { widgets::primary_button(&button.label) } else { widgets::secondary_button(&button.label) };
                        if ui.add(widget.min_size(egui::vec2(0.0, 36.0))).clicked() {
                            action = self.click(button.kind, server);
                        }
                    }
                });
            });
        });
        action
    }

    fn stepper(&self, ui: &mut egui::Ui, palette: &Palette, connected: bool) {
        let mut titles: Vec<(String, String)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| if self.has_key_step && i == 0 { (t("SSH key").into(), "authorized_keys".into()) } else { (e.step.title(), e.step.caption()) })
            .collect();
        titles.push((self.finish_title.clone(), self.finish_caption.clone()));
        // Without a login nothing has started yet.
        let index = if !connected && !self.busy { None } else { Some(self.index) };
        // When the captions of all steps do not fit (about 115 px each), only the current step keeps its caption.
        let compact = ui.available_width() < titles.len() as f32 * 115.0;
        ui.horizontal_wrapped(|ui| {
            for (i, (title, caption)) in titles.iter().enumerate() {
                let done = index.is_some_and(|x| i < x);
                let current = index == Some(i);
                let (rect, response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                let fill = if current {
                    Fixed::brand()
                } else if done {
                    palette.success_back
                } else {
                    palette.hover
                };
                ui.painter().circle_filled(rect.center(), 15.0, fill);
                if done {
                    icons::image(Icon::Check, 15.0, palette.success_text).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(15.0, 15.0)));
                } else {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        (i + 1).to_string(),
                        egui::FontId::proportional(13.0),
                        if current { egui::Color32::WHITE } else { palette.muted },
                    );
                }
                if compact && !current {
                    response.on_hover_text(title);
                } else {
                    ui.vertical(|ui| {
                        let mut text = RichText::new(title).size(13.0).color(if current { palette.text } else { palette.muted });
                        if current {
                            text = text.strong();
                        }
                        ui.label(text);
                        if current || !compact {
                            ui.label(RichText::new(caption).size(11.5).color(palette.subtle));
                        }
                    });
                }
                if i + 1 < titles.len() {
                    let (line, _) = ui.allocate_exact_size(egui::vec2(if compact { 16.0 } else { 28.0 }, 30.0), egui::Sense::hover());
                    ui.painter().line_segment(
                        [line.left_center() + egui::vec2(4.0, 0.0), line.right_center() - egui::vec2(4.0, 0.0)],
                        egui::Stroke::new(2.0_f32, if done { palette.success_text } else { palette.card_border }),
                    );
                }
            }
        });
    }

    fn buttons(&self, connected: bool) -> Vec<Button> {
        let mut list = Vec::new();
        if self.busy {
            return list;
        }
        if !connected {
            list.push(Button::primary(t("Log in"), Kind::Login));
            return list;
        }
        if self.on_finish() {
            list.push(Button::secondary(t("Back"), Kind::Leave));
            if let Some(url) = &self.site_url {
                list.push(Button::primary(&tf("Open {url}", &[("url", url)]), Kind::OpenSite));
            }
            return list;
        }
        let i = self.index;
        let step = &self.entries[i].step;
        let has_next = i + 1 < self.entries.len();
        let next_title = if has_next { tf("Next: {title}", &[("title", &self.entries[i + 1].step.title())]) } else { t("Finish").into() };
        let install_or_skip = |list: &mut Vec<Button>, install: String, already: bool| {
            let skip = if already { t("Skip (already installed)") } else { t("Skip") };
            if already {
                list.push(Button { label: install, primary: false, kind: Kind::Next });
                list.push(Button::primary(skip, Kind::Skip));
            } else {
                list.push(Button::secondary(skip, Kind::Skip));
                list.push(Button { label: install, primary: true, kind: Kind::Next });
            }
        };
        if self.on_key_step() {
            match self.outcomes[0] {
                None => {
                    list.push(Button::secondary(t("Back"), Kind::Leave));
                    let already = self.already_done[0];
                    install_or_skip(&mut list, if already { t("Install another key").into() } else { t("Install key").into() }, already);
                }
                Some(StepOutcome::Done) => {
                    list.push(Button::secondary(t("Back"), Kind::Leave));
                    list.push(Button::secondary(t("Install another key"), Kind::AnotherKey));
                    list.push(Button::primary(&next_title, Kind::Continue));
                }
                Some(StepOutcome::Failed) | Some(StepOutcome::Skipped) => {
                    list.push(Button::secondary(t("Back"), Kind::Leave));
                    list.push(Button::primary(t("Try again"), Kind::Next));
                }
                Some(StepOutcome::Aborted) => {
                    list.push(Button::secondary(t("Abort"), Kind::Abort));
                    list.push(Button::primary(t("Start over"), Kind::Next));
                }
            }
            return list;
        }
        list.push(if self.outcomes[i] == Some(StepOutcome::Aborted) { Button::secondary(t("Abort"), Kind::Abort) } else { Button::secondary(t("Back"), Kind::Leave) });
        match self.outcomes[i] {
            None => {
                let already = self.already_done[i];
                install_or_skip(&mut list, if already { step.rerun_label() } else { step.install_label() }, already);
            }
            Some(StepOutcome::Skipped) => {
                // Only the last step stays on screen when skipped; the user can still change their mind.
                list.push(Button { label: tf("{label} anyway", &[("label", &step.install_label())]), primary: false, kind: Kind::Next });
            }
            Some(StepOutcome::Done) => list.push(Button::primary(&next_title, Kind::Continue)),
            Some(StepOutcome::Failed) => {
                list.push(Button::secondary(t("Skip"), Kind::Skip));
                list.push(Button::primary(t("Try again"), Kind::Next));
            }
            Some(StepOutcome::Aborted) => list.push(Button::primary(t("Start over"), Kind::Next)),
        }
        list
    }

    fn click(&mut self, kind: Kind, server: Option<&Arc<dyn Server>>) -> Option<WizardAction> {
        if self.busy {
            return None;
        }
        match kind {
            Kind::Login => return Some(WizardAction::Login),
            Kind::Leave => {
                self.started_for = None;
                return Some(WizardAction::Exit);
            }
            Kind::Abort => {
                // Start over next time; the login stays.
                self.started_for = None;
                self.set_status(t("Installation aborted."));
                return Some(WizardAction::Exit);
            }
            Kind::OpenSite => {
                if let Some(url) = &self.site_url {
                    super::open_url(url);
                }
                return None;
            }
            _ => {}
        }
        let server = server?.clone();
        let i = self.index;
        match kind {
            Kind::Next => match self.outcomes[i] {
                Some(StepOutcome::Aborted) => self.start(&server),
                // "Try again" on the key step: back to the choices, with a fresh look at the server.
                Some(StepOutcome::Failed) if self.on_key_step() => self.enter_step(0, &server),
                _ => self.run_step(&server),
            },
            Kind::Skip => {
                if self.on_key_step() {
                    self.set_status(t("SSH key step skipped."));
                } else {
                    self.outcomes[i] = Some(StepOutcome::Skipped);
                    self.set_status(&tf("{title} step skipped.", &[("title", &self.entries[i].step.title())]));
                }
                self.continue_after(i, &server);
            }
            Kind::Continue => self.continue_after(i, &server),
            Kind::AnotherKey => self.enter_step(0, &server),
            _ => {}
        }
        None
    }

    fn continue_after(&mut self, i: usize, server: &Arc<dyn Server>) {
        if i + 1 < self.entries.len() {
            self.enter_step(i + 1, server);
        } else {
            self.enter_finish(server);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Next,
    Skip,
    Continue,
    Leave,
    Abort,
    AnotherKey,
    Login,
    OpenSite,
}

struct Button {
    label: String,
    primary: bool,
    kind: Kind,
}

impl Button {
    fn primary(label: &str, kind: Kind) -> Self {
        Self { label: label.into(), primary: true, kind }
    }

    fn secondary(label: &str, kind: Kind) -> Self {
        Self { label: label.into(), primary: false, kind }
    }
}

fn newest_ppk(directory: &std::path::Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ppk"))
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

pub fn not_connected(ui: &mut egui::Ui, palette: &Palette) {
    ui.horizontal_top(|ui| {
        widgets::icon_badge(ui, Icon::Server, 46.0);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.set_max_width(620.0);
            ui.label(RichText::new(t("Not connected")).size(19.0).strong());
            widgets::muted(
                ui,
                palette,
                t("Log in to the server first. The login is used for everything (installing, the addons and the tools) and stays open until you close the program."),
                13.0,
            );
        });
    });
}
