//! Small building blocks shared by the pages: cards, chips, headings and buttons.

use eframe::egui::{self, Color32, RichText};

use super::theme::{self, Fixed, Palette};

pub fn card<R>(ui: &mut egui::Ui, palette: &Palette, margin: egui::Margin, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // The margin and the edge go around the content, inside the width there is: the content gets what is left.
    let content = (ui.available_width() - (margin.left + margin.right) as f32 - 2.0 * theme::card_edge()).max(0.0);
    card_frame(palette)
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(content);
            add(ui)
        })
        .inner
}

/// A card's frame: in the Hotel look a dark 2 px outline with a hard shadow under it.
pub fn card_frame(palette: &Palette) -> egui::Frame {
    let frame = egui::Frame::new().fill(palette.card).stroke(palette.card_stroke()).corner_radius(theme::card_corners());
    if theme::hotel() {
        frame.shadow(egui::Shadow { offset: [0, 3], blur: 0, spread: 0, color: theme::with_alpha(palette.outline, 0x40) })
    } else {
        frame
    }
}

/// A page or card title: Pixelify Sans in the Hotel look.
pub fn title_text(text: &str, size: f32) -> RichText {
    let text = RichText::new(text).size(size).family(theme::title_family());
    if theme::hotel() { text } else { text.strong() }
}

/// The main action: the hotel's green "go" button with a darker edge, or the brand colour.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    let button = egui::Button::new(RichText::new(text.to_string()).strong().color(Color32::WHITE)).min_size(egui::vec2(0.0, 32.0));
    if theme::hotel() {
        button.fill(Fixed::go()).stroke(egui::Stroke::new(2.0_f32, Fixed::go_edge())).corner_radius(6)
    } else {
        button.fill(Fixed::brand()).corner_radius(8)
    }
}

pub fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string())).corner_radius(if theme::hotel() { 6 } else { 8 }).min_size(egui::vec2(0.0, 32.0))
}

/// A wide button with a bold title and a description under it (the answers of a question).
pub fn described_button(ui: &mut egui::Ui, palette: &Palette, title: &str, description: &str, primary: bool) -> egui::Response {
    let width = ui.available_width();
    let (fill, text, stroke) = if primary {
        if theme::hotel() {
            (Fixed::go(), Color32::WHITE, egui::Stroke::new(2.0_f32, Fixed::go_edge()))
        } else {
            (Fixed::brand(), Color32::WHITE, egui::Stroke::NONE)
        }
    } else {
        (palette.card, palette.text, egui::Stroke::new(if theme::hotel() { 2.0_f32 } else { 1.0 }, palette.card_border_strong))
    };
    let response = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(width - 32.0);
            ui.label(RichText::new(title).size(13.5).strong().color(text));
            if !description.is_empty() {
                ui.label(RichText::new(description).size(12.0).color(theme::with_alpha(text, 220)));
            }
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().rect_stroke(response.rect, 8.0, egui::Stroke::new(2.0_f32, if theme::hotel() { Fixed::hotel_yellow() } else { Fixed::brand() }), egui::StrokeKind::Inside);
    }
    ui.add_space(2.0);
    response
}

/// The coloured square with an icon at the left of a step's intro.
pub fn icon_badge(ui: &mut egui::Ui, icon: super::icons::Icon, size: f32) {
    egui::Frame::new().fill(theme::with_alpha(Fixed::brand(), 0x26)).corner_radius(12).inner_margin((size - 22.0) / 2.0).show(ui, |ui| {
        super::icons::show(ui, icon, 22.0, Fixed::brand());
    });
}

/// How a result box is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Success,
    Info,
    Warning,
    Error,
}

/// The coloured outcome box (ResultBanner).
pub fn result_banner(ui: &mut egui::Ui, palette: &Palette, kind: ResultKind, heading: &str, message: &str) {
    use super::icons::Icon;
    let (icon, fore, back) = match kind {
        ResultKind::Success => (Icon::Shield, palette.success_text, palette.success_back),
        ResultKind::Info => (Icon::Info, palette.info_text, palette.info_back),
        ResultKind::Warning => (Icon::Alert, palette.warning_text, palette.warning_back),
        ResultKind::Error => (Icon::Alert, palette.error_text, palette.error_back),
    };
    egui::Frame::new().fill(back).corner_radius(10).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            super::icons::show(ui, icon, 20.0, fore);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(heading).size(14.0).strong().color(fore));
                if !message.is_empty() {
                    ui.add(egui::Label::new(RichText::new(message).size(12.5)).wrap());
                }
            });
        });
    });
}

/// A list of steps ticked off as they finish (CheckList): before `running` done, `running` busy or failed.
pub fn check_list(ui: &mut egui::Ui, palette: &Palette, labels: &[String], details: &[String], running: usize, failed: bool) {
    use super::icons::Icon;
    for (i, label) in labels.iter().enumerate() {
        let done = i < running;
        let current = i == running;
        let (icon, color) = if done {
            (Icon::Check, palette.success_text)
        } else if current && failed {
            (Icon::Close, palette.error_text)
        } else if current {
            (Icon::ChevronRight, palette.accent_text)
        } else {
            (Icon::ChevronRight, palette.subtle)
        };
        ui.horizontal(|ui| {
            super::icons::show(ui, icon, 16.0, color);
            ui.add_space(4.0);
            let mut text = RichText::new(label).size(13.5).color(if done || current { palette.text } else { palette.muted });
            if current && !failed {
                text = text.strong();
            }
            ui.label(text);
            if done {
                if let Some(detail) = details.get(i).filter(|d| !d.is_empty()) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(detail).size(12.5).color(palette.muted));
                    });
                }
            }
        });
    }
}

/// Muted text that wraps.
pub fn muted(ui: &mut egui::Ui, palette: &Palette, text: &str, size: f32) {
    ui.add(egui::Label::new(RichText::new(text).size(size).color(palette.muted)).wrap());
}

/// Text that wraps.
pub fn text(ui: &mut egui::Ui, text: &str, size: f32) {
    ui.add(egui::Label::new(RichText::new(text).size(size)).wrap());
}

/// A text with a Copy button next to it (installation keys, passwords to paste elsewhere).
pub fn copy_field(ui: &mut egui::Ui, palette: &Palette, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());
        egui::Frame::new().fill(palette.hover).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.label(RichText::new(value).monospace());
        });
        if ui.add(secondary_button(crate::i18n::t("Copy"))).clicked() {
            ui.ctx().copy_text(value.to_string());
        }
    });
}
