//! The language button at the top right: the flag of the window's language, and a click lists every language with
//! its flag. A choice applies at once and is kept in settings.json (`Language`), like Settings → Language.

use eframe::egui::{self, pos2, vec2, Color32, Stroke};

use super::theme::{hex, Fixed, Palette};
use crate::i18n::{self, t, LANGUAGES};

const POPUP: &str = "language_picker";

/// Opens the list (for screenshots).
pub fn open(ctx: &egui::Context) {
    ctx.memory_mut(|m| m.open_popup(egui::Id::new(POPUP)));
}

/// The button and its list.
pub fn picker(ui: &mut egui::Ui, palette: &Palette) {
    let current = i18n::current();
    let dark = ui.visuals().dark_mode;
    let code = current.code.to_uppercase();
    let galley = ui.painter().layout_no_wrap(code, egui::FontId::new(12.5, egui::FontFamily::Name("bold".into())), palette.text);
    let size = vec2(10.0 + 24.0 + 6.0 + galley.size().x + 6.0 + 8.0 + 10.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(t("Language"));

    // A chunky outlined pill with a hard shadow, like the day / night switch next to it.
    let painter = ui.painter_at(rect.expand(4.0));
    let outline = if dark { hex("#5B87A6") } else { Fixed::outline() };
    let radius = rect.height() / 2.0;
    painter.rect_filled(rect.translate(vec2(0.0, 3.0)), radius, Color32::from_black_alpha(0x40));
    painter.rect_filled(rect, radius, if response.hovered() { palette.hover } else { palette.card });
    painter.rect_stroke(rect, radius, Stroke::new(2.0_f32, outline), egui::StrokeKind::Inside);
    let flag = egui::Rect::from_min_size(pos2(rect.left() + 10.0, rect.center().y - 8.0), vec2(24.0, 16.0));
    super::flags::paint(&painter, flag, current.hotel_domain);
    let text_left = flag.right() + 6.0;
    painter.galley(pos2(text_left, rect.center().y - galley.size().y / 2.0), galley.clone(), palette.text);
    // ▾
    let tip = pos2(text_left + galley.size().x + 6.0 + 4.0, rect.center().y + 2.5);
    painter.add(egui::Shape::convex_polygon(vec![tip + vec2(-4.0, -5.0), tip + vec2(4.0, -5.0), tip], palette.muted, Stroke::NONE));

    let popup = egui::Id::new(POPUP);
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    let mut chosen = None;
    egui::popup::popup_below_widget(ui, popup, &response, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(190.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        for lang in LANGUAGES {
            if row(ui, palette, lang.native_name, lang.hotel_domain, lang.code == current.code).clicked() {
                chosen = Some(*lang);
            }
        }
    });
    if let Some(lang) = chosen.filter(|l| l.code != current.code) {
        i18n::set(lang.code);
        let mut settings = aio_core::InstallerSettings::load();
        settings.language = Some(lang.code.to_string());
        // A read-only configuration folder only means the choice is not remembered.
        let _ = settings.save();
        ui.ctx().request_repaint();
    }
}

// One language in the list: its flag and its own name, the current one marked.
fn row(ui: &mut egui::Ui, palette: &Palette, name: &str, domain: &str, selected: bool) -> egui::Response {
    let width = ui.available_width().max(190.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), egui::Sense::click());
    let painter = ui.painter();
    if selected || response.hovered() {
        painter.rect_filled(rect, 6.0, if selected { palette.selected } else { palette.hover });
    }
    let flag = egui::Rect::from_min_size(pos2(rect.left() + 8.0, rect.center().y - 8.0), vec2(24.0, 16.0));
    super::flags::paint(painter, flag, domain);
    let font = egui::FontId::new(13.0, if selected { egui::FontFamily::Name("bold".into()) } else { egui::FontFamily::Proportional });
    let galley = painter.layout_no_wrap(name.to_string(), font, palette.text);
    painter.galley(pos2(flag.right() + 10.0, rect.center().y - galley.size().y / 2.0), galley, palette.text);
    if selected {
        // ✓ at the right.
        let at = pos2(rect.right() - 16.0, rect.center().y);
        let stroke = Stroke::new(2.0_f32, palette.accent_text);
        painter.line_segment([at + vec2(-5.0, 0.0), at + vec2(-1.5, 3.5)], stroke);
        painter.line_segment([at + vec2(-1.5, 3.5), at + vec2(5.0, -4.0)], stroke);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
