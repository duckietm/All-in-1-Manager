//! Live logs of the server: the emulator, NGINX, Traefik, the firewall, Fail2Ban and CrowdSec, followed over the login
//! while the page is on screen. Leaving the page stops the follower on the server. (DebugView.cs)
//!
//! egui has no "detached from the visual tree" event, so the page notices both ends itself: `show` records the pass
//! in which it was last drawn; a pass without it means the page was left. A small watchdog task cancels the follower
//! then, and the next `show` after such a gap follows the log again from its last lines (like the C# attach).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aio_remote::server_tools::{debug_log_sources, DebugLogSource};
use eframe::egui::{self, RichText};
use tokio_util::sync::CancellationToken;

use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets;
use crate::gui::{Shared, View};
use crate::i18n::{t, tf};

const MAX_LINES: usize = 5000;

/// What the follower task hands to the page.
#[derive(Default)]
struct Incoming {
    lines: Vec<String>,
    /// The follower of this generation ended by itself, with this text.
    ended: Option<(u64, String)>,
}

pub struct DebugView {
    sources: Vec<DebugLogSource>,
    source: usize,
    /// Every line received (at most 5,000), also while paused or filtered.
    all: VecDeque<String>,
    shown: LogLines,
    incoming: Arc<Mutex<Incoming>>,
    filter: String,
    paused: bool,
    following: Option<CancellationToken>,
    generation: u64,
    state: String,
    state_error: bool,
    /// The pass in which `show` ran last; read by the watchdog.
    last_pass: Arc<AtomicU64>,
    /// `show` has never run, or the login changed: the next `show` follows again.
    restart_on_show: bool,
}

impl DebugView {
    pub fn new(_shared: &Shared) -> Self {
        Self {
            sources: debug_log_sources(),
            source: 0,
            all: VecDeque::new(),
            shown: LogLines::with_limit(MAX_LINES),
            incoming: Arc::new(Mutex::new(Incoming::default())),
            filter: String::new(),
            paused: false,
            following: None,
            generation: 0,
            state: String::new(),
            state_error: false,
            last_pass: Arc::new(AtomicU64::new(0)),
            restart_on_show: true,
        }
    }

    fn show_state(&mut self, text: String, error: bool) {
        self.state = text;
        self.state_error = error;
    }

    fn follow(&mut self, index: usize, shared: &Shared) {
        self.stop();
        self.source = index.min(self.sources.len().saturating_sub(1));
        self.all.clear();
        self.shown.clear();
        *self.incoming.lock().unwrap() = Incoming::default();
        self.set_paused(false);
        let Some(server) = shared.session() else {
            self.show_state(t("Not connected: log in to follow the logs.").to_string(), true);
            return;
        };
        let source = self.sources[self.source].clone();
        let token = CancellationToken::new();
        self.following = Some(token.clone());
        self.generation += 1;
        let generation = self.generation;
        self.show_state(
            tf("Live: {follows} on {host}, the newest lines at the bottom.", &[("follows", &source.follows), ("host", &server.login().host)]),
            false,
        );

        // The watchdog: a pass without the page means it was left, so the follower on the server stops.
        let ctx = shared.ctx.clone();
        let last_pass = self.last_pass.clone();
        let watch = token.clone();
        shared.runtime.spawn(async move {
            loop {
                tokio::select! {
                    _ = watch.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if left_page(ctx.cumulative_pass_nr(), last_pass.load(Ordering::Relaxed)) {
                            watch.cancel();
                            break;
                        }
                    }
                }
            }
        });

        let incoming = self.incoming.clone();
        let ctx = shared.ctx.clone();
        shared.spawn(async move {
            let script = source.follow_script();
            let mut on_line = {
                let incoming = incoming.clone();
                // Lines arrive on the SSH task; the page adds them in batches, five times a second.
                move |line: &str| {
                    incoming.lock().unwrap().lines.push(line.to_string());
                    ctx.request_repaint_after(Duration::from_millis(200));
                }
            };
            let result = server.follow_as_root(&script, &mut on_line, token.clone()).await;
            if token.is_cancelled() {
                return;
            }
            let ended = match result {
                Ok(()) => t("The log stopped on the server.").to_string(),
                Err(error) => tf("Stopped: {message}", &[("message", &error.to_string())]),
            };
            incoming.lock().unwrap().ended = Some((generation, ended));
        });
    }

    /// Stops following (the follower on the server ends with the SSH channel).
    fn stop(&mut self) {
        if let Some(token) = self.following.take() {
            token.cancel();
        }
    }

    fn flush(&mut self) {
        let (batch, ended) = {
            let mut incoming = self.incoming.lock().unwrap();
            (std::mem::take(&mut incoming.lines), incoming.ended.take())
        };
        if !batch.is_empty() {
            push_capped(&mut self.all, batch.iter().cloned());
            if !self.paused {
                for line in batch.into_iter().filter(|l| matches_filter(l, &self.filter)) {
                    self.shown.push(line);
                }
            }
        }
        if let Some((generation, ended)) = ended {
            if generation == self.generation && self.following.is_some() {
                self.following = None;
                self.show_state(format!("{ended} {}", t("Restart follows it again.")), true);
            }
        }
    }

    fn refilter(&mut self) {
        self.shown.clear();
        for line in self.all.iter().filter(|l| matches_filter(l, &self.filter)) {
            self.shown.push(line.clone());
        }
    }

    /// Paused: lines are still collected, but the view holds still until Resume.
    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !paused {
            self.refilter();
        }
    }
}

/// True when a whole pass went by without the page (`shown` is the pass in which `show` ran last).
fn left_page(current: u64, shown: u64) -> bool {
    current > shown + 1
}

fn matches_filter(line: &str, filter: &str) -> bool {
    filter.is_empty() || line.to_lowercase().contains(&filter.to_lowercase())
}

fn push_capped(all: &mut VecDeque<String>, lines: impl IntoIterator<Item = String>) {
    all.extend(lines);
    while all.len() > MAX_LINES {
        all.pop_front();
    }
}

impl View for DebugView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let pass = ui.ctx().cumulative_pass_nr();
        let previous = self.last_pass.swap(pass, Ordering::Relaxed);
        // Back on the page after a pass without it (or for the first time): follow the log again.
        if self.restart_on_show || left_page(pass, previous) {
            self.restart_on_show = false;
            self.follow(self.source, shared);
        }
        self.flush();
        if self.following.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }

        // The sources.
        let mut clicked = None;
        ui.horizontal_wrapped(|ui| {
            for (index, source) in self.sources.iter().enumerate() {
                let button = if index == self.source { widgets::primary_button(&source.name) } else { widgets::secondary_button(&source.name) };
                if ui.add(button).clicked() {
                    clicked = Some(index);
                }
            }
        });
        if let Some(index) = clicked {
            self.follow(index, shared);
        }
        ui.add_space(10.0);

        // The title and the tools.
        let mut restart = false;
        ui.horizontal(|ui| {
            let tools_width = 260.0 + 3.0 * 90.0;
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - tools_width).max(120.0));
                let source = &self.sources[self.source];
                ui.add(egui::Label::new(RichText::new(&source.name).size(13.0).strong()).truncate());
                ui.add(egui::Label::new(RichText::new(&source.description).size(12.0).color(palette.muted)).wrap());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(widgets::secondary_button(t("Restart")))
                    .on_hover_text(t("Follow this log again from its last lines, for example after the connection was lost."))
                    .clicked()
                {
                    restart = true;
                }
                if ui.add(widgets::secondary_button(t("Clear"))).clicked() {
                    self.all.clear();
                    self.shown.clear();
                }
                if ui.add(widgets::secondary_button(if self.paused { t("Resume") } else { t("Pause") })).clicked() {
                    self.set_paused(!self.paused);
                }
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text(t("Filter: only lines with this text"))
                        .desired_width(260.0)
                        .min_size(egui::vec2(0.0, 30.0))
                        .vertical_align(egui::Align::Center)
                        .font(egui::FontId::proportional(12.5)),
                );
                if filter.changed() {
                    self.refilter();
                }
            });
        });
        if restart {
            self.follow(self.source, shared);
        }
        ui.add_space(10.0);

        let state_height = 34.0;
        let height = (ui.available_height() - state_height - 18.0).max(160.0);
        log_view::show(ui, "debug", &mut self.shown, Some(height), "");
        ui.add_space(10.0);
        let color = if self.state_error { palette.warning_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&self.state).size(12.0).color(color)).wrap());
    }

    fn session_changed(&mut self, _shared: &Shared) {
        // A new login (or none): the follower of the old one stops, and the page follows again when it is shown.
        self.stop();
        self.restart_on_show = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaving_is_a_pass_without_the_page() {
        assert!(!left_page(10, 10));
        assert!(!left_page(11, 10));
        assert!(left_page(12, 10));
    }

    #[test]
    fn filter_ignores_case() {
        assert!(matches_filter("anything", ""));
        assert!(matches_filter("2026 [ERROR] Wired", "error"));
        assert!(!matches_filter("info", "ERROR"));
    }

    #[test]
    fn keeps_the_last_lines() {
        let mut all = VecDeque::new();
        push_capped(&mut all, (0..MAX_LINES + 7).map(|i| i.to_string()));
        assert_eq!(all.len(), MAX_LINES);
        assert_eq!(all.front().unwrap(), "7");
    }
}
