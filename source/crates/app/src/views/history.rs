//! History: what ran on which server, newest first, with each run's log. The runs come from
//! configuration/history (aio_core::history), which every install step and tool writes when it ends.

use std::time::Instant;

use aio_core::history::{self, Entry, Outcome};
use eframe::egui::{self, RichText};

use super::dashboard::chip;
use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::{step, widgets, Shared, View};
use crate::i18n::{t, tf};

/// Which runs the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// The server you are logged in to (all servers without a login).
    Current,
    All,
    Server(String),
}

pub struct HistoryView {
    scope: Scope,
    filter: String,
    entries: Vec<Entry>,
    servers: Vec<String>,
    loaded: Option<Instant>,
    loaded_for: Option<String>,
    selected: Option<usize>,
    log: LogLines,
}

impl HistoryView {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            scope: Scope::Current,
            filter: String::new(),
            entries: Vec::new(),
            servers: Vec::new(),
            loaded: None,
            loaded_for: None,
            selected: None,
            log: LogLines::default(),
        }
    }

    fn server_of_scope(&self, shared: &Shared) -> Option<String> {
        match &self.scope {
            Scope::Current => shared.session().map(|s| step::history_server(s.as_ref())),
            Scope::All => None,
            Scope::Server(server) => Some(server.clone()),
        }
    }

    /// Reads the folder again (when the page opens, every few seconds while it is on screen, and on Refresh).
    fn reload(&mut self, shared: &Shared) {
        let server = self.server_of_scope(shared);
        let selected_path = self.selected.and_then(|i| self.entries.get(i)).map(|e| e.path.clone());
        self.entries = history::list(server.as_deref());
        self.servers = history::servers();
        self.loaded = Some(Instant::now());
        self.loaded_for = server;
        self.selected = selected_path.and_then(|p| self.entries.iter().position(|e| e.path == p));
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.log.clear();
        if let Some(entry) = self.entries.get(index) {
            for line in history::read_log(&entry.path) {
                self.log.push(line);
            }
        }
    }

    fn visible(&self) -> Vec<usize> {
        let words: Vec<String> = self.filter.split_whitespace().map(str::to_lowercase).collect();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let text = format!("{} {} {} {}", e.title, e.result, e.server, e.outcome.as_str()).to_lowercase();
                words.iter().all(|w| text.contains(w))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

fn outcome_chip(ui: &mut egui::Ui, palette: &Palette, outcome: Outcome) {
    let (text, back, fore) = match outcome {
        Outcome::Done => (t("DONE"), palette.success_back, palette.success_text),
        Outcome::Warning => (t("WARNING"), palette.warning_back, palette.warning_text),
        Outcome::Failed => (t("FAILED"), palette.error_back, palette.error_text),
        Outcome::Aborted => (t("ABORTED"), palette.hover, palette.muted),
    };
    chip(ui, text, back, fore);
}

impl View for HistoryView {
    fn session_changed(&mut self, _shared: &Shared) {
        self.loaded = None;
        self.selected = None;
        self.log.clear();
    }

    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let wanted = self.server_of_scope(shared);
        if self.loaded.is_none_or(|at| at.elapsed().as_secs() >= 3) || self.loaded_for != wanted {
            self.reload(shared);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));

        // The toolbar: which server, a filter, Refresh and the folder.
        ui.horizontal(|ui| {
            let label = |scope: &Scope| match scope {
                Scope::Current => t("This server").to_string(),
                Scope::All => t("All servers").to_string(),
                Scope::Server(server) => server.clone(),
            };
            let mut scope = self.scope.clone();
            egui::ComboBox::from_id_salt("history-scope").selected_text(label(&scope)).width(220.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut scope, Scope::Current, label(&Scope::Current));
                ui.selectable_value(&mut scope, Scope::All, label(&Scope::All));
                for server in &self.servers {
                    ui.selectable_value(&mut scope, Scope::Server(server.clone()), server);
                }
            });
            if scope != self.scope {
                self.scope = scope;
                self.selected = None;
                self.log.clear();
                self.reload(shared);
            }
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(t("Filter (title, result, server)")).desired_width(260.0));
            if ui.add(widgets::secondary_button(t("Refresh"))).clicked() {
                self.reload(shared);
            }
            if ui.add(widgets::secondary_button(t("Open folder"))).clicked() {
                let folder = history::folder();
                let _ = std::fs::create_dir_all(&folder);
                crate::gui::open_url(&folder.display().to_string());
            }
        });
        ui.add_space(10.0);

        let visible = self.visible();
        // The newest run is open when the page opens.
        if self.selected.is_none() {
            if let Some(&first) = visible.first() {
                self.select(first);
            }
        }
        let height = ui.available_height();
        let mut clicked = None;
        ui.horizontal_top(|ui| {
            let list_width = (ui.available_width() * 0.42).clamp(320.0, 520.0);
            ui.allocate_ui_with_layout(egui::vec2(list_width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(list_width);
                widgets::card(ui, palette, egui::Margin::same(10), |ui| {
                    ui.set_min_height(height - 24.0);
                    if visible.is_empty() {
                        widgets::muted(
                            ui,
                            palette,
                            if self.entries.is_empty() {
                                t("Nothing ran yet. Every install step and tool you run is kept here, with its log, per server.")
                            } else {
                                t("No run matches the filter.")
                            },
                            12.5,
                        );
                        return;
                    }
                    egui::ScrollArea::vertical().id_salt("history-list").auto_shrink([false, false]).show(ui, |ui| {
                        for index in visible {
                            let entry = &self.entries[index];
                            let selected = self.selected == Some(index);
                            let frame = egui::Frame::new()
                                .fill(if selected { palette.selected } else { egui::Color32::TRANSPARENT })
                                .corner_radius(6)
                                .inner_margin(egui::Margin::symmetric(8, 6));
                            let response = frame
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        outcome_chip(ui, palette, entry.outcome);
                                        ui.add_space(6.0);
                                        ui.label(RichText::new(&entry.title).size(13.0).strong());
                                    });
                                    let when = entry.started.format("%Y-%m-%d %H:%M").to_string();
                                    let seconds = (entry.finished - entry.started).num_seconds().max(0);
                                    let took = if seconds >= 60 { format!("{} min {} s", seconds / 60, seconds % 60) } else { format!("{seconds} s") };
                                    let kind = if entry.kind == "step" { t("step") } else { t("tool") };
                                    ui.label(RichText::new(format!("{when}  ·  {took}  ·  {kind}  ·  {}", entry.server)).size(11.5).color(palette.subtle));
                                    if !entry.result.is_empty() {
                                        ui.add(egui::Label::new(RichText::new(&entry.result).size(12.0).color(palette.muted)).truncate());
                                    }
                                })
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if response.clicked() {
                                clicked = Some(index);
                            }
                        }
                    });
                });
            });
            ui.add_space(12.0);
            ui.vertical(|ui| match self.selected.and_then(|i| self.entries.get(i)) {
                Some(entry) => {
                    ui.horizontal(|ui| {
                        outcome_chip(ui, palette, entry.outcome);
                        ui.add_space(6.0);
                        ui.label(widgets::title_text(&entry.title, 16.0));
                    });
                    ui.label(
                        RichText::new(tf(
                            "{server}, {started} – {finished}",
                            &[
                                ("server", &entry.server),
                                ("started", &entry.started.format("%Y-%m-%d %H:%M:%S").to_string()),
                                ("finished", &entry.finished.format("%H:%M:%S").to_string()),
                            ],
                        ))
                        .size(12.0)
                        .color(palette.muted),
                    );
                    if !entry.result.is_empty() {
                        widgets::text(ui, &entry.result, 12.5);
                    }
                    ui.add_space(6.0);
                    let title = entry.path.display().to_string();
                    log_view::show(ui, &title, &mut self.log, None, t("This run wrote no log."));
                }
                None => widgets::muted(ui, palette, t("Choose a run on the left to see its log."), 13.0),
            });
        });
        if let Some(index) = clicked {
            self.select(index);
        }
    }
}
