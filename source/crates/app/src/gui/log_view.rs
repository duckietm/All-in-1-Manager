//! The console log of a step or tool (LogView): it follows new output unless the user scrolled up, the lines can
//! be selected, and Copy log in the top right corner puts the selected lines (or all of them) on the clipboard, so a
//! failure can be pasted somewhere instead of screenshotted. Ctrl+C copies the selection too.

use eframe::egui::{self, Color32, RichText};

use super::theme::{hex, Fixed};
use crate::i18n::{n, t, tf};

/// The lines of a log, joined once per change for the view.
#[derive(Default, Clone)]
pub struct LogLines {
    lines: Vec<String>,
    joined: String,
    stale: bool,
    /// Lines dropped from the front (when a limit is set).
    limit: Option<usize>,
}

impl LogLines {
    pub fn with_limit(limit: usize) -> Self {
        Self { limit: Some(limit), ..Default::default() }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
        if let Some(limit) = self.limit {
            if self.lines.len() > limit {
                let excess = self.lines.len() - limit;
                self.lines.drain(..excess);
            }
        }
        self.stale = true;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.joined.clear();
        self.stale = false;
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn text(&mut self) -> &str {
        if self.stale {
            self.joined = self.lines.join("\n");
            self.stale = false;
        }
        &self.joined
    }
}

/// Per log: when "Copied 3 lines" was shown, so the label goes back after 2 seconds.
#[derive(Clone, Copy, Default)]
struct CopyLabel {
    until: f64,
    copied: usize,
}

/// Draws the log `height` points high (None: what is left).
pub fn show(ui: &mut egui::Ui, id: &str, log: &mut LogLines, height: Option<f32>, placeholder: &str) {
    let height = height.unwrap_or_else(|| ui.available_height().max(120.0));
    let id = ui.make_persistent_id(("log", id));
    let frame = egui::Frame::new()
        .fill(Fixed::console_background())
        .stroke(egui::Stroke::new(1.0_f32, Fixed::console_border()))
        .corner_radius(8)
        // The right side stays free for the Copy log button, so it never covers a line.
        .inner_margin(egui::Margin { left: 4, right: 124, top: 6, bottom: 6 });
    let response = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_height(height - 12.0);
        if log.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(placeholder).monospace().size(12.0).color(hex("#6B7890")));
            return None;
        }
        let mut selection = None;
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            let mut text = log.text().to_string();
            let output = egui::TextEdit::multiline(&mut text)
                .id(id.with("text"))
                .font(egui::FontId::monospace(12.0))
                .text_color(Fixed::console_text())
                .frame(false)
                .desired_width(f32::INFINITY)
                .interactive(true)
                .show(ui);
            // Read-only: the text is put back every frame, but the selection stays for copying.
            if let Some(range) = output.cursor_range {
                let [start, end] = range.sorted_cursors();
                if start.ccursor.index != end.ccursor.index {
                    selection = Some((start.ccursor.index, end.ccursor.index));
                }
            }
        });
        selection
    });
    let selection = response.inner;

    // Copy log, floating in the top right corner.
    let rect = response.response.rect;
    if !log.is_empty() {
        let now = ui.input(|i| i.time);
        let mut label: CopyLabel = ui.data(|d| d.get_temp(id.with("copy"))).unwrap_or_default();
        let caption = if now < label.until {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            if label.copied > 0 {
                tf(if label.copied == 1 { n("Copied {n} line") } else { n("Copied {n} lines") }, &[("n", &label.copied.to_string())])
            } else {
                t("Nothing to copy").to_string()
            }
        } else {
            t("Copy log").to_string()
        };
        let button_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - 120.0, rect.top() + 8.0), egui::vec2(100.0, 22.0));
        // In a child of its own: `put` would move this ui's cursor back to the button, and what comes after the log
        // (a page's status line) was then drawn over the log's first lines.
        let mut corner = ui.new_child(egui::UiBuilder::new().max_rect(button_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let button = corner
            .add_sized(button_rect.size(), egui::Button::new(RichText::new(caption).size(11.0)).fill(Color32::from_rgba_unmultiplied(40, 50, 64, 220)))
            .on_hover_text(t("Copies the whole log to the clipboard, or only the lines you selected. Ctrl+C does the same."));
        if button.clicked() {
            let copied = copy(ui.ctx(), log, selection);
            label = CopyLabel { until: now + 2.0, copied };
            ui.data_mut(|d| d.insert_temp(id.with("copy"), label));
        }
    }
}

/// Copies the lines touched by the selection, or all lines, and returns how many.
fn copy(ctx: &egui::Context, log: &mut LogLines, selection: Option<(usize, usize)>) -> usize {
    let lines = log.lines();
    let (first, last) = match selection {
        Some((start, end)) => {
            let text = log.text().to_string();
            let line_of = |char_index: usize| text.chars().take(char_index).filter(|&c| c == '\n').count();
            (line_of(start), line_of(end.saturating_sub(1).max(start)))
        }
        None => (0, lines.len().saturating_sub(1)),
    };
    let lines = log.lines();
    if lines.is_empty() {
        return 0;
    }
    let last = last.min(lines.len() - 1);
    let chosen = &lines[first.min(last)..=last];
    ctx.copy_text(chosen.join(if cfg!(windows) { "\r\n" } else { "\n" }));
    chosen.len()
}

