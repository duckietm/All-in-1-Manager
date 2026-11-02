//! Light or dark, like the hotel by day and by night: the switch at the top right (a sky with the sun, or the night
//! sky with the moon and stars, in a chunky outlined pill) and the choice in Settings → Look. Following the system is
//! the default; a choice is kept in settings.json (`Theme`) and applies at once.

use eframe::egui::{self, pos2, vec2, Color32, Stroke};

use super::theme::{hex, Fixed};
use crate::i18n::t;

pub const LIGHT: &str = "light";
pub const DARK: &str = "dark";

/// The saved choice as egui's preference: light, dark, or the system's.
pub fn preference(theme: Option<&str>) -> egui::ThemePreference {
    match theme {
        Some(LIGHT) => egui::ThemePreference::Light,
        Some(DARK) => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    }
}

/// The saved choice (None: follow the system).
pub fn saved() -> Option<String> {
    aio_core::InstallerSettings::load().theme
}

/// Applies the choice (None: follow the system) and keeps it in settings.json.
pub fn choose(ctx: &egui::Context, theme: Option<&str>) {
    ctx.set_theme(preference(theme));
    let mut settings = aio_core::InstallerSettings::load();
    settings.theme = theme.map(str::to_string);
    // A read-only configuration folder only means the choice is not remembered.
    let _ = settings.save();
}

/// The day / night switch. A click turns the window to the other one.
pub fn switch(ui: &mut egui::Ui) {
    let dark = ui.visuals().dark_mode;
    let size = vec2(62.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect.expand(4.0));
    // By night the dark edge would vanish on the dark page: a lighter one there.
    let outline = if dark { hex("#5B87A6") } else { Fixed::outline() };
    let radius = rect.height() / 2.0;

    // The hard shadow under it, and the sky.
    painter.rect_filled(rect.translate(vec2(0.0, 3.0)), radius, Color32::from_black_alpha(0x40));
    painter.rect_filled(rect, radius, if dark { hex("#0B2A45") } else { hex("#6FB0E3") });
    if dark {
        for (x, y, s) in [(0.20, 0.30, 1.6), (0.33, 0.62, 1.2), (0.46, 0.28, 1.0), (0.27, 0.80, 1.0), (0.52, 0.70, 1.4)] {
            let at = pos2(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
            painter.rect_filled(egui::Rect::from_center_size(at, vec2(s, s)), 0.0, Color32::from_rgb(255, 255, 230));
        }
    } else {
        // A small outlined cloud on the right side of the sky.
        let at = pos2(rect.right() - 20.0, rect.center().y + 3.0);
        for (dx, dy, r) in [(-6.0, 1.0, 4.5), (0.0, -2.0, 6.0), (6.0, 1.0, 4.0)] {
            painter.circle_filled(at + vec2(dx, dy), r + 1.5, outline);
        }
        for (dx, dy, r) in [(-6.0, 1.0, 4.5), (0.0, -2.0, 6.0), (6.0, 1.0, 4.0)] {
            painter.circle_filled(at + vec2(dx, dy), r, Color32::WHITE);
        }
    }

    // The knob: the sun on the left by day, the moon on the right by night, sliding between them.
    let progress = ui.ctx().animate_bool_with_time(response.id, dark, 0.18);
    let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), progress);
    let knob = pos2(knob_x, rect.center().y);
    let knob_radius = radius - 4.0;
    if progress < 0.5 {
        for i in 0..8 {
            let angle = i as f32 * std::f32::consts::TAU / 8.0;
            let direction = vec2(angle.cos(), angle.sin());
            painter.line_segment([knob + direction * (knob_radius + 1.0), knob + direction * (knob_radius + 4.0)], Stroke::new(2.0_f32, hex("#FFE27A")));
        }
        painter.circle_filled(knob, knob_radius, Fixed::hotel_yellow());
        painter.circle_stroke(knob, knob_radius, Stroke::new(2.0_f32, outline));
    } else {
        painter.circle_filled(knob, knob_radius, hex("#F4EFD9"));
        painter.circle_stroke(knob, knob_radius, Stroke::new(2.0_f32, outline));
        // The shadow of the crescent and two craters.
        painter.circle_filled(knob + vec2(4.0, -3.0), knob_radius * 0.75, hex("#0B2A45"));
        painter.circle_filled(knob + vec2(-4.0, 3.0), 1.6, hex("#D5CDAE"));
        painter.circle_filled(knob + vec2(-2.0, -4.5), 1.1, hex("#D5CDAE"));
    }
    painter.rect_stroke(rect, radius, Stroke::new(2.0_f32, outline), egui::StrokeKind::Inside);

    let tip = if dark { t("Night: switch to the day (light)") } else { t("Day: switch to the night (dark)") };
    if response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
        choose(ui.ctx(), Some(if dark { LIGHT } else { DARK }));
    }
}
