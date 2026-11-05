//! The install steps (InstallStep.cs, RunPanel.cs): an intro with what is installed already, and a run panel with a
//! check list, the result and the live log. A step only detects and installs; it runs as a task on the runtime and
//! talks to the window through its [`StepUi`].

use std::sync::{Arc, Mutex};

use aio_remote::{Server, ServerError};
use async_trait::async_trait;
use eframe::egui::{self, RichText};

use super::dialogs::{Choice, Dialogs};
use super::icons::{self, Icon};
use super::log_view::{self, LogLines};
use super::theme::Palette;
use super::widgets::{self, ResultKind};
use crate::i18n::{t, tf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Done,
    Skipped,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub kind: ResultKind,
    pub heading: String,
    pub message: String,
}

/// What a finished step adds under its result (an installation key to copy, a link, extra lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extra {
    /// A value with a Copy button.
    Copy { label: String, value: String },
    /// Lines of text.
    Lines(Vec<String>),
}

/// The intro of a step: icon, heading, description, what it does, and a footnote.
pub struct Intro {
    pub icon: Icon,
    pub heading: String,
    pub description: String,
    pub bullets: Vec<String>,
    pub footnote: Option<String>,
}

/// The state of a step's page, shared between its task and the window.
#[derive(Default)]
pub struct StepState {
    pub showing_run: bool,
    pub detected: Option<Banner>,
    pub lines: LogLines,
    pub checks: Vec<String>,
    pub details: Vec<String>,
    pub stage: usize,
    pub failed: bool,
    pub busy: bool,
    pub result: Option<Banner>,
    pub extras: Vec<Extra>,
}

/// A step's handle on the window. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct StepUi {
    state: Arc<Mutex<StepState>>,
    ctx: egui::Context,
    pub dialogs: Dialogs,
    status: Arc<Mutex<String>>,
    title: String,
}

impl StepUi {
    pub fn new(ctx: egui::Context, dialogs: Dialogs, status: Arc<Mutex<String>>, title: &str, checks: Vec<String>) -> Self {
        let state = StepState { details: vec![String::new(); checks.len()], checks, ..Default::default() };
        Self { state: Arc::new(Mutex::new(state)), ctx, dialogs, status, title: title.into() }
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, StepState> {
        self.state.lock().unwrap()
    }

    fn changed(&self) {
        self.ctx.request_repaint();
    }

    /// The window's context (for a thread that has to draw it again when it is done).
    pub fn context(&self) -> egui::Context {
        self.ctx.clone()
    }

    /// Asks the window to draw again (after a task reported something).
    pub fn wake(&self) {
        self.ctx.request_repaint();
    }

    /// Adds a log line.
    pub fn log(&self, line: &str) {
        self.state().lines.push(line);
        self.changed();
    }

    /// The step's log as the `Log` the remote functions take.
    pub fn logger(&self) -> impl Fn(&str) + Send + Sync + '_ {
        move |line: &str| self.log(line)
    }

    /// Marks the running check as done and moves on to the next one.
    pub fn done(&self, detail: &str) {
        let mut state = self.state();
        let stage = state.stage;
        if let Some(d) = state.details.get_mut(stage) {
            *d = detail.to_string();
        }
        state.stage += 1;
        drop(state);
        self.changed();
    }

    pub fn fail(&self) {
        self.state().failed = true;
        self.changed();
    }

    pub fn set_busy(&self, busy: bool) {
        self.state().busy = busy;
        self.changed();
    }

    pub fn detected(&self, kind: ResultKind, heading: &str, message: &str) {
        self.state().detected = Some(Banner { kind, heading: heading.into(), message: message.into() });
        self.changed();
    }

    pub fn result(&self, kind: ResultKind, heading: &str, message: &str) {
        self.state().result = Some(Banner { kind, heading: heading.into(), message: message.into() });
        self.changed();
    }

    pub fn add_extra(&self, extra: Extra) {
        self.state().extras.push(extra);
        self.changed();
    }

    pub fn set_status(&self, message: &str) {
        *self.status.lock().unwrap() = message.to_string();
        self.changed();
    }

    /// Done: the green result and the status. (SucceededAsync)
    pub fn succeeded(&self, heading: &str, message: &str) -> StepOutcome {
        self.result(ResultKind::Success, heading, message);
        self.set_status(&format!("{heading}."));
        StepOutcome::Done
    }

    /// Done, but with something the user should fix (shown as a warning). (SucceededWithWarningAsync)
    pub fn succeeded_with_warning(&self, heading: &str, message: &str) -> StepOutcome {
        self.result(ResultKind::Warning, heading, message);
        self.set_status(&format!("{heading}."));
        StepOutcome::Done
    }

    /// Aborted by the user before anything changed. (Aborted)
    pub fn aborted(&self, host: &str) -> StepOutcome {
        self.log("! Installation aborted by the user. Nothing was changed on the server.");
        self.fail();
        self.result(ResultKind::Warning, t("Installation aborted"), &tf("Nothing was changed on {host}.", &[("host", host)]));
        self.set_status(t("Installation aborted."));
        StepOutcome::Aborted
    }

    /// A question with described answers while the progress bar pauses. (ChoiceDialog with Run.Busy = false)
    pub async fn choose<T: Clone + Send>(&self, window_title: &str, heading: &str, message: &str, cancel: T, options: Vec<Choice<T>>) -> T {
        self.set_busy(false);
        let answer = self.dialogs.choose(window_title, heading, message, cancel, options).await;
        self.set_busy(true);
        answer
    }

    /// Back to the intro, with the check list reset. (ShowIntro)
    pub fn show_intro(&self) {
        self.state().showing_run = false;
        self.changed();
    }

    /// The run panel, emptied. (RunPanel.Start)
    pub fn start(&self) {
        let mut state = self.state();
        state.showing_run = true;
        state.lines.clear();
        state.result = None;
        state.extras.clear();
        state.stage = 0;
        state.failed = false;
        let count = state.checks.len();
        state.details = vec![String::new(); count];
        state.busy = true;
        drop(state);
        self.changed();
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}

/// An install step. The window keeps it as `Arc<dyn InstallStep>`; settings a step shows on its intro live in a
/// `Mutex` of its own, since the step's task reads them while the window draws.
#[async_trait]
pub trait InstallStep: Send + Sync + 'static {
    /// Short name for the stepper and messages, e.g. "NGINX".
    fn title(&self) -> String;
    /// Second line in the stepper, e.g. "Web server".
    fn caption(&self) -> String;
    fn install_label(&self) -> String;
    /// Install button text when the step is already done.
    fn rerun_label(&self) -> String {
        t("Reinstall / upgrade").into()
    }
    fn intro(&self) -> Intro;
    /// Called when the wizard opens the step, before detect (the SSH key step shows its options for the server).
    fn enter(&self, _server: &dyn Server) {}
    /// The check list of the run panel.
    fn checks(&self) -> Vec<String>;
    /// Settings shown under the bullets.
    fn options(&self, _ui: &mut egui::Ui, _palette: &Palette) {}
    /// Checks those settings before the step runs; returns the problem, or None.
    fn validate(&self) -> Option<String> {
        None
    }
    /// Shows what is installed now (with `ui.detected`). True when everything this step sets up is already there.
    async fn detect(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<bool>;
    /// Runs the checks in order; return an error to fail.
    async fn run(&self, server: Arc<dyn Server>, ui: StepUi) -> anyhow::Result<StepOutcome>;
}

/// The message of a failure: a ServerError as it is, anything else with its kind. (C#: ex.GetType().Name + message)
pub fn error_message(error: &anyhow::Error) -> String {
    if let Some(server) = error.downcast_ref::<ServerError>() {
        server.0.clone()
    } else {
        format!("{error:#}")
    }
}

/// In dry-run mode the recorded commands go to this step's log.
pub fn dry_run_sink(server: &dyn Server, ui: &StepUi) {
    if server.is_dry_run() {
        let ui = ui.clone();
        super::dry_run::set_sink(move |line| ui.log(line));
    }
}

/// Detect with the failure shown on the intro. (InstallStep.DetectAsync)
pub async fn detect(step: Arc<dyn InstallStep>, server: Arc<dyn Server>, ui: StepUi) -> bool {
    dry_run_sink(server.as_ref(), &ui);
    match step.detect(server, ui.clone()).await {
        Ok(found) => found,
        Err(error) => {
            ui.detected(ResultKind::Warning, &tf("Could not check {title}", &[("title", &step.title())]), &error_message(&error));
            false
        }
    }
}

/// Runs the step with its failure on the run panel. (InstallStep.RunAsync)
pub async fn run(step: Arc<dyn InstallStep>, server: Arc<dyn Server>, ui: StepUi) -> StepOutcome {
    ui.start();
    dry_run_sink(server.as_ref(), &ui);
    let started = chrono::Local::now();
    let history_server = server.clone();
    let outcome = match step.run(server, ui.clone()).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = error_message(&error);
            let title = step.title();
            ui.log(&format!("! {message}"));
            ui.fail();
            ui.result(ResultKind::Error, &tf("Installing {title} failed", &[("title", &title)]), &message);
            ui.set_status(&tf("Installing {title} failed.", &[("title", &title)]));
            StepOutcome::Failed
        }
    };
    ui.set_busy(false);
    let kept = match outcome {
        StepOutcome::Done => Some(aio_core::history::Outcome::Done),
        StepOutcome::Failed => Some(aio_core::history::Outcome::Failed),
        StepOutcome::Aborted => Some(aio_core::history::Outcome::Aborted),
        StepOutcome::Skipped => None,
    };
    if let Some(kept) = kept {
        save_history(&ui, history_server.as_ref(), "step", &step.title(), kept, started);
    }
    outcome
}

/// The server's name in the history: the host, with the port when it is not 22.
pub fn history_server(server: &dyn Server) -> String {
    let login = server.login();
    match (login.port, login.host.contains(':')) {
        (22, _) => login.host.clone(),
        (port, true) => format!("[{}]:{port}", login.host),
        (port, false) => format!("{}:{port}", login.host),
    }
}

/// Keeps the run's log in configuration/history (not for the dry run, which reaches no server). A warning result counts
/// as a warning.
pub fn save_history(ui: &StepUi, server: &dyn Server, kind: &str, title: &str, outcome: aio_core::history::Outcome, started: chrono::DateTime<chrono::Local>) {
    if server.is_dry_run() {
        return;
    }
    let state = ui.state();
    let outcome = match (&state.result, outcome) {
        (Some(banner), aio_core::history::Outcome::Done) if banner.kind == ResultKind::Warning => aio_core::history::Outcome::Warning,
        _ => outcome,
    };
    let result = state.result.as_ref().map(|b| format!("{}: {}", b.heading, b.message)).unwrap_or_default();
    let host = history_server(server);
    let record = aio_core::history::Record {
        server: &host,
        kind,
        title,
        outcome,
        started,
        finished: chrono::Local::now(),
        result: &result,
        lines: state.lines.lines(),
    };
    // Not being able to write the history never fails the step.
    let _ = aio_core::history::save(&record);
}

/// The step's page: its intro, or the run panel while and after it runs.
pub fn show(ui: &mut egui::Ui, palette: &Palette, step: &dyn InstallStep, step_ui: &StepUi) {
    let showing_run = step_ui.state().showing_run;
    if showing_run {
        show_run(ui, palette, step_ui, 240.0);
    } else {
        show_intro(ui, palette, step, step_ui);
    }
}

fn show_intro(ui: &mut egui::Ui, palette: &Palette, step: &dyn InstallStep, step_ui: &StepUi) {
    let intro = step.intro();
    ui.horizontal_top(|ui| {
        widgets::icon_badge(ui, intro.icon, 46.0);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.set_max_width(760.0_f32.min(ui.available_width()));
            ui.label(RichText::new(&intro.heading).size(19.0).strong());
            widgets::muted(ui, palette, &intro.description, 13.0);
            ui.add_space(4.0);
            let detected = step_ui.state().detected.clone();
            if let Some(banner) = detected {
                widgets::result_banner(ui, palette, banner.kind, &banner.heading, &banner.message);
                ui.add_space(4.0);
            }
            for line in &intro.bullets {
                ui.horizontal_top(|ui| {
                    icons::show(ui, Icon::Check, 15.0, palette.success_text);
                    ui.add_space(4.0);
                    widgets::text(ui, line, 13.0);
                });
            }
            if let Some(footnote) = &intro.footnote {
                ui.add_space(2.0);
                widgets::muted(ui, palette, footnote, 12.0);
            }
            ui.add_space(6.0);
            step.options(ui, palette);
        });
    });
}

/// The run panel: progress, check list, result, extras and the live log. (RunPanel)
pub fn show_run(ui: &mut egui::Ui, palette: &Palette, step_ui: &StepUi, log_height: f32) {
    let mut state = step_ui.state();
    if state.busy {
        ui.add(egui::ProgressBar::new(0.0).animate(true).desired_height(3.0).fill(super::theme::Fixed::brand()));
    } else {
        ui.add_space(3.0);
    }
    ui.add_space(10.0);
    widgets::check_list(ui, palette, &state.checks, &state.details, state.stage, state.failed);
    if let Some(banner) = state.result.clone() {
        ui.add_space(10.0);
        widgets::result_banner(ui, palette, banner.kind, &banner.heading, &banner.message);
    }
    for extra in state.extras.clone() {
        ui.add_space(6.0);
        match extra {
            Extra::Copy { label, value } => widgets::copy_field(ui, palette, &label, &value),
            Extra::Lines(lines) => {
                for line in lines {
                    widgets::text(ui, &line, 12.5);
                }
            }
        }
    }
    ui.add_space(10.0);
    let title = step_ui.title().to_string();
    log_view::show(ui, &title, &mut state.lines, Some(log_height), t("Output appears here when the installer connects."));
}
