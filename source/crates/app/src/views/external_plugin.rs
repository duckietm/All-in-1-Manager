//! The page of an external plugin: the plugin describes it as a `ui-tree` ([`super::plugin_tree`] checks it and
//! draws it with the window's own widgets) and gets what the user did back as `ui-event`s.
//!
//! Lifecycle: one [`PluginInstance`] per open page. It lives in a task on the tokio runtime (never on the window's
//! thread) that loads it, renders, and then handles the page's events one by one, rendering after them. Each start
//! (opening the page, Reload) is a [`Running`] of its own: its state, log, services and approval queue, so an old
//! instance that is still ending can never write into, or reject the requests of, the new one. Leaving the page drops
//! the view and with it the `Running`: the task stops, the instance is dropped and its waiting approvals are rejected.
//! A plugin that traps or runs too long shows "The plugin stopped: …" with Reload. While an approved script or write
//! batch executes, the window counts as busy ([`ExternalPluginView::executing`]), so nothing cuts it off half-way.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aio_plugin_host::approvals::Approvals;
use aio_plugin_host::runtime::{PluginError, PluginInstance, UiEvent};
use aio_plugin_host::services::HostServices;
use aio_plugin_host::{display_safe, Engine};
use eframe::egui::{self, RichText};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::plugin_approval::{self, Outcome};
use super::plugin_tree::{self, Shown};
use crate::gui::log_view;
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::Shared;
use crate::i18n::{t, tf};
use crate::plugins::external::{self, ExternalInfo, LiveGuard};
use crate::plugins::services::{AppServices, PluginOutput};

/// Events waiting for the plugin before further clicks are dropped.
const MAX_QUEUE: usize = 32;

/// The engine every plugin runs on (one per program).
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(aio_plugin_host::runtime::engine)
}

/// What the page's task reports to the window.
#[derive(Default)]
struct State {
    /// A call (loading, rendering, an event) runs or waits.
    busy: bool,
    /// The last page the plugin drew, checked and made safe (or why it cannot be shown).
    page: Option<Arc<Result<Shown, String>>>,
    /// Counts the pages received, so the view knows when to take a new one.
    generation: u64,
    /// Why the instance is gone (trapped, timed out, failed to load, stopped by the user).
    stopped: Option<String>,
    /// The plugin's own error from its last event (the instance stays usable).
    failed: Option<String>,
    /// Events sent and not yet handled.
    queued: usize,
}

/// Loads the plugin, renders, then handles events until the page closes (the sender is dropped) or the plugin stops.
async fn run(
    info: ExternalInfo,
    dir: PathBuf,
    services: Arc<dyn HostServices>,
    approvals: Arc<Approvals>,
    state: Arc<Mutex<State>>,
    mut events: mpsc::UnboundedReceiver<UiEvent>,
    ctx: egui::Context,
) {
    let update = |f: &dyn Fn(&mut State)| {
        f(&mut state.lock());
        ctx.request_repaint();
    };
    let id = info.manifest.id.clone();
    // Held as long as the instance: removing or updating the plugin waits until it is gone.
    let _live = LiveGuard::new(&id);
    let loaded = if info.dev {
        // A developer plugin runs the author's file; `dir` is its private folder, not the author's.
        match external::plugins().dev_wasm(&id) {
            Some(wasm) => PluginInstance::load_from(engine(), &wasm, &dir, info.manifest, services, approvals).await,
            None => Err(anyhow::anyhow!("The developer folder is not in the list any more")),
        }
    } else {
        PluginInstance::load(engine(), &dir, info.manifest, services, approvals).await
    };
    let mut instance = match loaded {
        Ok(instance) => instance,
        Err(error) => {
            let message = format!("{error:#}");
            update(&|s| {
                s.busy = false;
                s.stopped = Some(message.clone());
            });
            return;
        }
    };
    loop {
        let rendered = instance.render().await;
        match rendered {
            Ok(tree) => {
                // Checked and made safe here, once, not on the window's thread every frame.
                let page = Arc::new(plugin_tree::prepare(&tree));
                update(&|s| {
                    s.busy = !events.is_empty();
                    s.page = Some(page.clone());
                    s.generation += 1;
                });
            }
            Err(error) => {
                let message = error.to_string();
                update(&|s| {
                    s.busy = false;
                    s.stopped = Some(message.clone());
                });
                return;
            }
        }
        // Every queued event first, then one render.
        loop {
            let Some(event) = events.recv().await else { return };
            update(&|s| {
                s.busy = true;
                s.queued = s.queued.saturating_sub(1);
            });
            match instance.event(event).await {
                Ok(()) => update(&|s| s.failed = None),
                Err(PluginError::Failed(message)) => update(&|s| s.failed = Some(message.clone())),
                Err(error) => {
                    let message = error.to_string();
                    update(&|s| {
                        s.busy = false;
                        s.stopped = Some(message.clone());
                    });
                    return;
                }
            }
            if events.is_empty() {
                break;
            }
        }
    }
}

/// One start of the plugin: everything its task touches belongs to it alone.
struct Running {
    id: String,
    state: Arc<Mutex<State>>,
    output: Arc<Mutex<PluginOutput>>,
    services: Arc<AppServices>,
    approvals: Arc<Approvals>,
    events: Option<mpsc::UnboundedSender<UiEvent>>,
    task: tokio::task::AbortHandle,
}

impl Running {
    fn start(info: &ExternalInfo, dir: PathBuf, exports: PathBuf, shared: &Shared) -> Self {
        let state = Arc::new(Mutex::new(State { busy: true, ..State::default() }));
        let output = Arc::new(Mutex::new(PluginOutput::new()));
        let services = Arc::new(AppServices::new(shared.clone(), &info.manifest.name, exports, output.clone()));
        let ctx = shared.ctx.clone();
        let approvals = Arc::new(Approvals::with_notifier(move || ctx.request_repaint()));
        // The card's text is built once per request, on the plugin's worker, never per frame.
        approvals.set_formatter(plugin_approval::card_lines);
        let (sender, receiver) = mpsc::unbounded_channel();
        let host: Arc<dyn HostServices> = services.clone();
        let task = shared.runtime.spawn(run(info.clone(), dir, host, approvals.clone(), state.clone(), receiver, shared.ctx.clone())).abort_handle();
        // "Stop this plugin" in one of its dialogs: like "Reject and stop" on the approval card.
        let (stop_task, stop_state, stop_approvals, stop_id, ctx) = (task.clone(), state.clone(), approvals.clone(), info.manifest.id.clone(), shared.ctx.clone());
        services.on_stop(move || {
            stop_task.abort();
            stop_approvals.reject_all_for(&stop_id);
            {
                let mut s = stop_state.lock();
                s.busy = false;
                s.stopped = Some(t("you stopped it").to_string());
            }
            ctx.request_repaint();
        });
        Running { id: info.manifest.id.clone(), state, output, services, approvals, events: Some(sender), task }
    }

    /// Stops the task (at its next await; a running guest call ends within the call limit) and rejects whatever it
    /// still waits for. Its state stays, to show why it stopped.
    fn halt(&mut self) {
        self.events = None;
        self.task.abort();
        self.approvals.reject_all_for(&self.id);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

/// An open external plugin page.
pub struct ExternalPluginView {
    info: ExternalInfo,
    /// The plugin's name, safe to show.
    name: String,
    /// Where the plugin is in the list now (`Page::External`).
    pub index: u16,
    /// The installed folder and the exports folder.
    dir: PathBuf,
    exports: PathBuf,
    running: Running,
    /// The page shown, taken from the state when a new one arrives.
    page: Option<Result<Shown, String>>,
    generation: u64,
    /// What the user typed into the inputs, by input id, until the plugin draws the page again.
    inputs: HashMap<String, String>,
    /// A developer plugin's folder is being read again (off the window's thread): its result, when it is there.
    reloading: Option<Arc<Mutex<Option<anyhow::Result<external::DevReload>>>>>,
}

impl ExternalPluginView {
    /// Opens the page: starts the plugin.
    pub fn open(info: ExternalInfo, index: u16, shared: &Shared) -> Self {
        let plugins = external::plugins();
        let dir = if info.dev { plugins.dev_dir(&info.manifest.id) } else { plugins.dir(&info.manifest.id) };
        let exports = plugins.exports_dir(&info.manifest.id);
        Self::open_in(info, index, dir, exports, shared)
    }

    fn open_in(info: ExternalInfo, index: u16, dir: PathBuf, exports: PathBuf, shared: &Shared) -> Self {
        let running = Running::start(&info, dir.clone(), exports.clone(), shared);
        Self { name: display_safe(&info.manifest.name, 80), info, index, dir, exports, running, page: None, generation: 0, inputs: HashMap::new(), reloading: None }
    }

    pub fn id(&self) -> &str {
        &self.info.manifest.id
    }

    pub fn info(&self) -> &ExternalInfo {
        &self.info
    }

    /// True while an approved script or write batch of this plugin executes: the window waits for it.
    pub fn executing(&self) -> bool {
        if self.running.task.is_finished() {
            // The instance ended: an approval it never got to start is not running.
            self.running.services.forget_approved();
        }
        self.running.services.executing() > 0
    }

    /// Reload: a new instance with a fresh state; the old one is stopped and can no longer touch the page.
    fn restart(&mut self, shared: &Shared) {
        if self.executing() {
            return;
        }
        if self.info.dev {
            // Reload re-reads manifest.json and the wasm path from the author's folder, off the window's thread.
            if self.reloading.is_some() {
                return;
            }
            let slot = Arc::new(Mutex::new(None));
            self.reloading = Some(slot.clone());
            let (id, ctx) = (self.info.manifest.id.clone(), shared.ctx.clone());
            shared.spawn(async move {
                let result = match tokio::task::spawn_blocking(move || external::plugins().reload_dev(&id)).await {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::anyhow!("{error}")),
                };
                *slot.lock() = Some(result);
                ctx.request_repaint();
            });
            return;
        }
        self.start_again(shared);
    }

    /// Finishes a developer plugin's Reload once the folder has been read.
    fn poll_reload(&mut self, shared: &Shared) {
        let Some(slot) = &self.reloading else { return };
        let Some(result) = slot.lock().take() else { return };
        self.reloading = None;
        let plugins = external::plugins();
        match result {
            Ok(external::DevReload::Reloaded) => {
                if let Some(info) = plugins.index_of(&self.info.manifest.id).and_then(|i| plugins.get(i)) {
                    self.name = display_safe(&info.manifest.name, 80);
                    self.info = info;
                }
                self.start_again(shared);
            }
            Ok(external::DevReload::NeedsConsent) => {
                // The plugin leaves the list; the window closes this page and the Plugins page asks.
                shared.set_status(&tf("{plugin} asks for more permissions: review them on the Plugins page.", &[("plugin", &self.name)]));
            }
            Err(error) => shared.set_status(&tf("The developer folder cannot be read: {error}", &[("error", &display_safe(&format!("{error:#}"), 300))])),
        }
    }

    fn start_again(&mut self, shared: &Shared) {
        self.running = Running::start(&self.info, self.dir.clone(), self.exports.clone(), shared);
        self.page = None;
        self.generation = 0;
        self.inputs.clear();
    }

    fn send(&mut self, event: UiEvent, shared: &Shared) {
        let Some(sender) = &self.running.events else { return };
        let mut state = self.running.state.lock();
        if state.queued >= MAX_QUEUE {
            shared.set_status(&tf("{plugin} is busy: wait until it has finished.", &[("plugin", &self.name)]));
            return;
        }
        if sender.send(event).is_ok() {
            state.queued += 1;
            state.busy = true;
        }
    }

    /// The part above the scrolling content: the plugin's name, Reload, and the approval cards, so a request is
    /// always in view.
    pub fn show_pinned(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.poll_reload(shared);
        let (busy, stopped) = {
            let s = self.running.state.lock();
            (s.busy, s.stopped.is_some())
        };
        let executing = self.executing();
        ui.horizontal(|ui| {
            ui.label(widgets::title_text(&self.name, 16.0));
            ui.label(RichText::new(display_safe(&self.info.manifest.version, 40)).size(12.5).color(palette.muted));
            plugin_approval::badges(ui, palette, &self.info);
            if (busy && !stopped) || executing {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hint = if executing { t("Wait until the approved work has finished") } else { t("Start the plugin again") };
                if ui.add_enabled(!executing, widgets::secondary_button(t("Reload"))).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
                    self.restart(shared);
                }
            });
        });
        ui.add_space(10.0);
        let target = plugin_approval::Target {
            host: self.running.services.bound_host(),
            user: self.running.services.bound_user(),
            changed: self.running.services.server_changed(),
        };
        match plugin_approval::show(ui, palette, &self.running.approvals, &self.info, &self.name, &target) {
            Outcome::Approve(id) => {
                // Busy from the click on, so nothing (Reload, leaving the page) can cut the run off as it starts.
                if self.running.approvals.pending().iter().any(|(p, _)| *p == id) {
                    self.running.services.approve();
                    self.running.approvals.decide(id, aio_plugin_host::approvals::Decision::Approved);
                }
            }
            Outcome::Stop => {
                self.running.halt();
                self.running.state.lock().stopped = Some(t("you stopped it").to_string());
            }
            Outcome::Nothing => {}
        }
    }

    /// The scrolling part: the plugin's own content, framed and labelled as the plugin's, the stopped card, the
    /// exported file and the plugin log.
    pub fn show_content(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let (page, generation, stopped, failed) = {
            let s = self.running.state.lock();
            (s.page.clone(), s.generation, s.stopped.clone(), s.failed.clone())
        };
        if generation != self.generation {
            self.generation = generation;
            self.page = page.map(|p| (*p).clone());
            // A new page from the plugin: its values win, except in the field being typed in.
            let focused = ui.ctx().data(|d| d.get_temp::<String>(egui::Id::new(("plugin-focus", self.id()))));
            self.inputs.retain(|id, _| Some(id) == focused.as_ref());
        }

        if let Some(reason) = stopped {
            let mut reload = false;
            widgets::card(ui, palette, egui::Margin::symmetric(24, 18), |ui| {
                widgets::result_banner(ui, palette, ResultKind::Error, &tf("The plugin stopped: {reason}", &[("reason", &display_safe(&reason, 500))]), "");
                ui.add_space(8.0);
                reload = ui.add_enabled(!self.executing(), widgets::primary_button(t("Reload"))).clicked();
            });
            if reload {
                self.restart(shared);
            }
            ui.add_space(10.0);
        } else {
            if let Some(message) = failed {
                widgets::result_banner(ui, palette, ResultKind::Warning, t("The plugin reported an error"), &display_safe(&message, 500));
                ui.add_space(8.0);
            }
            // Everything the plugin draws sits in this frame, under its name: it cannot pass for the Manager's own UI.
            let mut events = Vec::new();
            egui::Frame::new().stroke(egui::Stroke::new(1.0_f32, palette.card_border_strong)).corner_radius(8).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tf("Drawn by the plugin {plugin}", &[("plugin", &self.name)])).size(11.5).color(palette.subtle));
                ui.add_space(6.0);
                match self.page.as_mut() {
                    Some(Ok(shown)) => {
                        let drawn = plugin_tree::show(ui, palette, shown, &mut self.inputs);
                        let key = egui::Id::new(("plugin-focus", self.info.manifest.id.as_str()));
                        ui.ctx().data_mut(|d| match &drawn.focused {
                            Some(id) => d.insert_temp(key, id.clone()),
                            None => d.remove::<String>(key),
                        });
                        events = drawn.events;
                    }
                    Some(Err(reason)) => {
                        widgets::result_banner(ui, palette, ResultKind::Error, t("The plugin's page cannot be shown"), &display_safe(reason, 300));
                    }
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(t("Starting the plugin …"));
                        });
                    }
                }
            });
            for event in events {
                self.send(event, shared);
            }
            ui.add_space(10.0);
        }

        let export = self.running.output.lock().export.clone();
        if let Some(file) = export {
            ui.horizontal(|ui| {
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                widgets::muted(ui, palette, &tf("Saved {name}: open it with Catalogue → Import a file", &[("name", &name)]), 12.5);
                if ui.add(widgets::secondary_button(t("Open folder"))).clicked() {
                    if let Some(folder) = file.parent() {
                        crate::gui::open_url(&folder.display().to_string());
                    }
                }
            });
            ui.add_space(8.0);
        }

        egui::CollapsingHeader::new(RichText::new(t("Plugin log")).size(13.0)).id_salt(("plugin-host-log", self.id())).show(ui, |ui| {
            let mut output = self.running.output.lock();
            log_view::show(ui, &format!("plugin-host-log-{}", self.info.manifest.id), &mut output.log, Some(200.0), t("Nothing logged yet."));
        });
    }
}

#[cfg(test)]
#[path = "external_plugin_e2e.rs"]
mod e2e;
