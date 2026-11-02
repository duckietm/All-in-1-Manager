//! Sections that fold away with a click on their heading (the sidebar's groups, the Tools page's sections). Which
//! ones are folded is kept in settings.json (`Collapsed`), so the window opens the way it was left.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, Color32};

fn state() -> &'static Mutex<BTreeSet<String>> {
    static STATE: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(aio_core::InstallerSettings::load().collapsed.into_iter().collect()))
}

pub fn is_collapsed(key: &str) -> bool {
    state().lock().unwrap().contains(key)
}

/// Folds or unfolds the section and saves the choice.
pub fn toggle(key: &str) {
    let collapsed: Vec<String> = {
        let mut set = state().lock().unwrap();
        if !set.remove(key) {
            set.insert(key.to_string());
        }
        set.iter().cloned().collect()
    };
    let mut settings = aio_core::InstallerSettings::load();
    settings.collapsed = collapsed;
    // A read-only configuration folder only means the choice is not remembered.
    let _ = settings.save();
}

/// A section heading with ▾ (open) or ▸ (folded) in front of it; a click folds or unfolds it. Returns whether the
/// section is open.
pub fn heading(ui: &mut egui::Ui, key: &str, text: &str, size: f32, color: Color32, indent: f32) -> bool {
    let open = !is_collapsed(key);
    let font = egui::FontId::new(size, egui::FontFamily::Name("bold".into()));
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), color);
    let height = galley.size().y.max(14.0) + 4.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    let hovered = response.hovered();
    let color = if hovered { color.gamma_multiply(1.4) } else { color };
    let arrow = if open { "▾" } else { "▸" };
    let y = rect.center().y;
    ui.painter().text(egui::pos2(rect.left() + indent, y), egui::Align2::LEFT_CENTER, arrow, egui::FontId::proportional(size + 1.0), color);
    ui.painter().galley(egui::pos2(rect.left() + indent + size + 4.0, y - galley.size().y / 2.0), galley, color);
    let tip = if open { crate::i18n::t("Click to fold this section") } else { crate::i18n::t("Click to unfold this section") };
    if response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
        toggle(key);
        ui.ctx().request_repaint();
    }
    open
}
