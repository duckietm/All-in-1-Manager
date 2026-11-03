//! The filled buttons of the Hotel look: every button gets its own colour (a soft gradient), with the white title and
//! description straight on it, the icon in a white badge and a large faint copy of the icon as a watermark. The
//! colours are deep enough for white text, by day and by night.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Shape, Stroke};

use super::icons::{self, Icon};
use super::theme::{self, hex, with_alpha, Fixed, Palette};

/// The colour of a button (the names are what the colours were picked from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scene {
    Room,
    Lido,
    Disco,
    Hotel,
    Sky,
    Garden,
    Snow,
    Bricks,
    Beach,
    Space,
    Hearts,
    Parquet,
}

pub const ALL: [Scene; 12] =
    [Scene::Room, Scene::Lido, Scene::Disco, Scene::Hotel, Scene::Sky, Scene::Garden, Scene::Snow, Scene::Bricks, Scene::Beach, Scene::Space, Scene::Hearts, Scene::Parquet];

/// A colour that always belongs to the same name (for the many tool buttons).
pub fn for_key(key: &str) -> Scene {
    // FNV-1a: the same on every computer and every start.
    let hash = key.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    ALL[hash as usize % ALL.len()]
}

/// The top and bottom colour of a button. By night a little deeper, so it does not glare on the dark page.
pub fn fill(scene: Scene, dark: bool) -> (Color32, Color32) {
    let (top, bottom) = match scene {
        Scene::Room => ("#D97039", "#B44A18"),
        Scene::Lido => ("#1C9BAC", "#0B7080"),
        Scene::Disco => ("#BE4EA3", "#8E2C7A"),
        Scene::Hotel => ("#2F8BD0", "#1A62A0"),
        Scene::Sky => ("#4A8FE0", "#2B63B8"),
        Scene::Garden => ("#439E51", "#287233"),
        Scene::Snow => ("#5F84AE", "#3C5D86"),
        Scene::Bricks => ("#D9503F", "#A92F24"),
        Scene::Beach => ("#BE801F", "#93580A"),
        Scene::Space => ("#6553CF", "#41329C"),
        Scene::Hearts => ("#DE5C86", "#B13A62"),
        Scene::Parquet => ("#A8774A", "#7A522C"),
    };
    let deepen = |c: Color32| {
        if !dark {
            return c;
        }
        let f = |v: u8| (f32::from(v) * 0.78) as u8;
        Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
    };
    (deepen(hex(top)), deepen(hex(bottom)))
}

/// White at `alpha`, premultiplied: `from_rgba_unmultiplied` goes through linear light here, which makes a faint white
/// several times stronger than asked.
fn white(alpha: u8) -> Color32 {
    Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha)
}

fn gradient(p: &egui::Painter, rect: Rect, (top, bottom): (Color32, Color32)) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(Shape::mesh(mesh));
}

/// the round corners of the card it fills.
pub fn round_corners(painter: &egui::Painter, rect: Rect, radius: f32, outside: Color32) {
    let steps = 6;
    for (corner, center, start) in [
        (rect.left_top(), rect.left_top() + vec2(radius, radius), std::f32::consts::PI),
        (rect.right_top(), rect.right_top() + vec2(-radius, radius), 1.5 * std::f32::consts::PI),
        (rect.right_bottom(), rect.right_bottom() + vec2(-radius, -radius), 0.0),
        (rect.left_bottom(), rect.left_bottom() + vec2(radius, -radius), 0.5 * std::f32::consts::PI),
    ] {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(corner, outside);
        for i in 0..=steps {
            let angle = start + (i as f32 / steps as f32) * std::f32::consts::FRAC_PI_2;
            mesh.colored_vertex(center + vec2(angle.cos(), angle.sin()) * radius, outside);
        }
        for i in 1..=steps as u32 {
            mesh.add_triangle(0, i, i + 1);
        }
        painter.add(Shape::mesh(mesh));
    }
}


/// How a filled button lays out its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// The text at the bottom, the icon at the top left (a card in a grid).
    Card,
    /// The icon on the left, the text next to it (a wide or a small button).
    Side,
}

/// A filled button: the colour of `scene`, the icon in a white badge, the title and description in white. `outside` is
/// the colour around the button, for its round corners. `_accent` is kept for the callers; the colour comes from the
/// scene.
#[allow(clippy::too_many_arguments)]
pub fn button(
    ui: &mut egui::Ui,
    palette: &Palette,
    size: egui::Vec2,
    scene: Scene,
    _accent: Color32,
    icon: Icon,
    title: &str,
    description: &str,
    layout: Layout,
    outside: Color32,
    enabled: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = enabled && response.hovered();
    let dark = ui.visuals().dark_mode;
    let radius = f32::from(theme::card_corners()).max(6.0);
    let edge = theme::card_edge().max(1.5);
    let painter = ui.painter();
    let (top, bottom) = fill(scene, dark);
    // Under the mouse a little lighter.
    let lift = |c: Color32| if hovered { Color32::from_rgb(c.r().saturating_add(14), c.g().saturating_add(14), c.b().saturating_add(14)) } else { c };

    // The hard shadow under it, as under the cards, then the colour and a soft shine over the top half.
    painter.rect_filled(rect.translate(vec2(0.0, 3.0)), radius, with_alpha(palette.outline, 0x40));
    gradient(painter, rect, (lift(top), lift(bottom)));
    gradient(painter, Rect::from_min_max(rect.min, pos2(rect.right(), rect.center().y)), (white(0x16), Color32::TRANSPARENT));

    // Two large soft circles at the right, as bubbles in the colour, kept inside the button.
    let bubbles = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let r = rect.height();
    bubbles.circle_filled(pos2(rect.right() - r * 0.25, rect.top() + r * 0.15), r * 0.75, white(0x18));
    bubbles.circle_filled(pos2(rect.right() - r * 0.95, rect.bottom() + r * 0.2), r * 0.55, white(0x10));
    round_corners(painter, rect, radius, outside);

    // The icon in a white badge.
    let badge_size = if layout == Layout::Card { 40.0 } else { (rect.height() - 22.0).clamp(28.0, 44.0) };
    let badge = match layout {
        Layout::Card => Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(badge_size, badge_size)),
        Layout::Side => Rect::from_min_size(pos2(rect.left() + 12.0, rect.center().y - badge_size / 2.0), vec2(badge_size, badge_size)),
    };
    painter.rect_filled(badge.translate(vec2(0.0, 2.0)), 9.0, with_alpha(Color32::BLACK, 0x30));
    painter.rect_filled(badge, 9.0, Color32::WHITE);
    let icon_size = badge_size * 0.55;
    icons::image(icon, icon_size, bottom).paint_at(ui, Rect::from_center_size(badge.center(), vec2(icon_size, icon_size)));

    // The text in white, straight on the colour.
    let title_font = egui::FontId::new(if layout == Layout::Card { 16.0 } else { 13.5 }, theme::title_family_or_bold());
    let text_font = egui::FontId::proportional(if layout == Layout::Card { 12.5 } else { 11.5 });
    let chevron = 16.0;
    let text_left = match layout {
        Layout::Card => rect.left() + 16.0,
        Layout::Side => badge.right() + 12.0,
    };
    let text_right = match layout {
        Layout::Card => rect.right() - 12.0,
        Layout::Side => (rect.right() - 10.0).min(text_left + 640.0),
    };
    let text_width = text_right - text_left - 8.0 - chevron;
    let rows = match layout {
        Layout::Card => 2,
        Layout::Side => if rect.height() > 80.0 { 3 } else { 2 },
    };
    let description_color = with_alpha(Color32::WHITE, 0xE6);
    let title_galley = galley(ui, title, title_font, Color32::WHITE, text_width, 1);
    let text_galley = galley(ui, description, text_font, description_color, text_width, rows);
    let text_height = title_galley.size().y + 3.0 + text_galley.size().y;
    let top_y = match layout {
        Layout::Card => rect.bottom() - 14.0 - text_height,
        Layout::Side => rect.center().y - text_height / 2.0,
    };
    let elided = title_galley.elided || text_galley.elided;
    let title_height = title_galley.size().y;
    // A faint dark copy under the text keeps it crisp on the lighter colours.
    let shadow = with_alpha(Color32::BLACK, 0x40);
    painter.galley_with_override_text_color(pos2(text_left, top_y + 1.0), title_galley.clone(), shadow);
    painter.galley(pos2(text_left, top_y), title_galley, Color32::WHITE);
    painter.galley(pos2(text_left, top_y + title_height + 3.0), text_galley, description_color);
    if enabled {
        let chevron_rect = Rect::from_center_size(pos2(rect.right() - 10.0 - chevron / 2.0, if layout == Layout::Card { top_y + text_height / 2.0 } else { rect.center().y }), vec2(chevron, chevron));
        icons::image(Icon::ChevronRight, chevron, Color32::WHITE).paint_at(ui, chevron_rect);
    }
    if !enabled {
        painter.rect_filled(rect, radius, with_alpha(palette.surface, 150));
    }

    // The edge: the dark outline, yellow when the mouse is on it.
    let stroke = if hovered { Stroke::new(edge + 1.0, if theme::hotel() { Fixed::hotel_yellow() } else { Fixed::brand() }) } else { Stroke::new(edge, palette.outline) };
    painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
    let response = if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response };
    if elided {
        response.on_hover_text(description)
    } else {
        response
    }
}

fn galley(ui: &egui::Ui, text: &str, font: egui::FontId, color: Color32, width: f32, rows: usize) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, width.max(20.0));
    job.wrap.max_rows = rows;
    job.wrap.overflow_character = Some('…');
    ui.fonts(|fonts| fonts.layout_job(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_keeps_its_colour() {
        assert_eq!(for_key("Restart emulator"), for_key("Restart emulator"));
        let scenes: std::collections::HashSet<_> = ["Health check", "Server status", "Restart MariaDB", "Update system", "Import full database", "SSH terminal", "Enable firewall", "Furniture language"].iter().map(|k| for_key(k)).collect();
        assert!(scenes.len() >= 4, "the tool buttons get several different colours");
    }

    // White text needs a dark enough colour: a contrast of at least 3 : 1 (large text) at the lighter top.
    #[test]
    fn white_text_reads_on_every_colour() {
        fn luminance(c: Color32) -> f32 {
            let l = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * l(c.r()) + 0.7152 * l(c.g()) + 0.0722 * l(c.b())
        }
        for scene in ALL {
            for dark in [false, true] {
                let (top, _) = fill(scene, dark);
                let contrast = 1.05 / (luminance(top) + 0.05);
                assert!(contrast >= 3.0, "{scene:?} (dark {dark}): {contrast:.2}");
            }
        }
    }
}
