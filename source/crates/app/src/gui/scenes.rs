//! Habbo-style pictures behind the buttons of the Hotel look: a room with a checkered floor, the Lido, a disco floor,
//! the hotel's front, clouds, a garden, a snowy night, a brick wall, the beach, space, hearts and a parquet floor.
//! Everything is painted here with plain shapes (no Habbo artwork). The text of a button always sits on a plate in the
//! card colour, so it reads the same on every picture, by day and by night.

use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Shape, Stroke};

use super::icons::{self, Icon};
use super::theme::{self, hex, with_alpha, Fixed, Palette};

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

/// A scene that always belongs to the same name (for the many tool buttons).
pub fn for_key(key: &str) -> Scene {
    // FNV-1a: the same on every computer and every start.
    let hash = key.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    ALL[hash as usize % ALL.len()]
}

// By night a scene is a little darker, so it does not glare on the dark page.
fn shade(color: Color32, dark: bool) -> Color32 {
    if !dark {
        return color;
    }
    let f = |c: u8| (f32::from(c) * 0.72) as u8;
    Color32::from_rgba_unmultiplied(f(color.r()), f(color.g()), f(color.b()), color.a())
}

/// Paints `scene` filling `rect`.
pub fn paint(painter: &egui::Painter, rect: Rect, scene: Scene, dark: bool, accent: Color32) {
    let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let c = |s: &str| shade(hex(s), dark);
    let at = |x: f32, y: f32| pos2(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    let fill = |r: Rect, color: Color32| p.rect_filled(r, 0.0, color);
    let band = |y0: f32, y1: f32, color: Color32| fill(Rect::from_min_max(at(0.0, y0), at(1.0, y1)), color);
    let outline = shade(Fixed::outline(), false);
    match scene {
        Scene::Room => {
            // Wallpaper with stripes in the accent colour, a skirting board, and an isometric checkered floor.
            band(0.0, 0.58, shade(with_alpha(accent, 255), dark).gamma_multiply(0.55).to_opaque());
            let mut x = rect.left();
            while x < rect.right() {
                fill(Rect::from_min_max(pos2(x, rect.top()), pos2(x + 6.0, at(0.0, 0.58).y)), with_alpha(Color32::WHITE, 40));
                x += 18.0;
            }
            band(0.55, 0.6, c("#5B3A1E"));
            band(0.6, 1.0, c("#C9C9B9"));
            iso_checker(&p, Rect::from_min_max(at(0.0, 0.6), at(1.0, 1.0)), c("#E8E8DA"), c("#A9A996"), 26.0);
        }
        Scene::Lido => {
            band(0.0, 0.35, c("#8CCBF2"));
            band(0.35, 0.45, c("#F2F2F2"));
            tiles_row(&p, Rect::from_min_max(at(0.0, 0.35), at(1.0, 0.45)), c("#3A9AD9"), 12.0);
            band(0.45, 1.0, c("#2E8BD4"));
            for row in 0..4 {
                wave(&p, rect, 0.55 + row as f32 * 0.12, with_alpha(Color32::WHITE, 90), row as f32 * 7.0);
            }
            // A diving board.
            let board = Rect::from_min_size(at(0.68, 0.30), vec2(rect.width() * 0.28, 5.0));
            fill(board, c("#FFFFFF"));
            p.rect_stroke(board, 0.0, Stroke::new(1.0_f32, outline), egui::StrokeKind::Outside);
        }
        Scene::Disco => {
            band(0.0, 1.0, c("#1B1030"));
            let colors = [accent, hex("#FF4FA3"), hex("#38D9F5"), hex("#FFD23F"), hex("#7CFC5A")];
            let size = 16.0;
            let mut i = 0usize;
            let mut y = rect.top() + rect.height() * 0.4;
            while y < rect.bottom() {
                let mut x = rect.left();
                while x < rect.right() {
                    let color = colors[(i * 7 + (y as usize / 16) * 3) % colors.len()];
                    fill(Rect::from_min_size(pos2(x + 1.0, y + 1.0), vec2(size - 2.0, size - 2.0)), shade(color, dark).gamma_multiply(0.85));
                    x += size;
                    i += 1;
                }
                y += size;
            }
            // Spotlights from a mirror ball.
            let ball = at(0.5, 0.0);
            for (x, color) in [(0.15, hex("#FF4FA3")), (0.45, hex("#38D9F5")), (0.8, hex("#FFD23F"))] {
                p.add(Shape::convex_polygon(
                    vec![ball, at(x - 0.08, 1.0), at(x + 0.08, 1.0)],
                    with_alpha(color, 40),
                    Stroke::NONE,
                ));
            }
            p.circle_filled(pos2(ball.x, ball.y + 8.0), 7.0, c("#D9D9E3"));
        }
        Scene::Hotel => {
            band(0.0, 1.0, if dark { hex("#0B2340") } else { hex("#7FB7E6") });
            let front = Rect::from_min_max(at(0.08, 0.12), at(0.92, 1.0));
            fill(front, c("#E9D8B4"));
            p.rect_stroke(front, 0.0, Stroke::new(2.0_f32, outline), egui::StrokeKind::Inside);
            let (cols, rows) = (((front.width() / 22.0) as i32).max(3), ((front.height() / 20.0) as i32).max(2));
            for row in 0..rows {
                for col in 0..cols {
                    let lit = (row * 31 + col * 17) % 5 != 0;
                    let w = Rect::from_min_size(
                        pos2(front.left() + 8.0 + col as f32 * 22.0, front.top() + 8.0 + row as f32 * 20.0),
                        vec2(12.0, 11.0),
                    );
                    if w.right() < front.right() - 4.0 && w.bottom() < front.bottom() - 2.0 {
                        let window = if dark && lit { hex("#FFD86B") } else if lit { hex("#BFE3FF") } else { c("#5E7A92") };
                        fill(w, window);
                        p.rect_stroke(w, 0.0, Stroke::new(1.0_f32, outline), egui::StrokeKind::Outside);
                    }
                }
            }
            let sign = Rect::from_center_size(pos2(front.center().x, front.top() + 2.0), vec2(46.0, 13.0));
            p.rect_filled(sign, 2.0, hex("#E33B2E"));
            p.rect_stroke(sign, 2.0, Stroke::new(1.5_f32, outline), egui::StrokeKind::Outside);
        }
        Scene::Sky => {
            gradient(&p, rect, if dark { (hex("#071C30"), hex("#1D4A6E")) } else { (hex("#5E9BD3"), hex("#9CCBF0")) });
            for (x, y, s) in [(0.12, 0.25, 0.7), (0.55, 0.12, 0.9), (0.8, 0.55, 0.6), (0.3, 0.7, 0.5)] {
                cloud(&p, at(x, y), s, dark);
            }
        }
        Scene::Garden => {
            band(0.0, 0.55, c("#9ED8F5"));
            band(0.42, 0.6, c("#2F8A3A"));
            for i in 0..((rect.width() / 16.0) as i32 + 1) {
                p.circle_filled(pos2(rect.left() + i as f32 * 16.0, at(0.0, 0.44).y), 9.0, c("#3FA34D"));
            }
            band(0.6, 1.0, c("#67C24A"));
            for (x, y, color) in [(0.1, 0.75, "#FF5A7A"), (0.25, 0.88, "#FFD23F"), (0.45, 0.72, "#FFFFFF"), (0.62, 0.9, "#FF5A7A"), (0.8, 0.78, "#B07CFF"), (0.93, 0.92, "#FFD23F")] {
                let center = at(x, y);
                for d in [vec2(-3.0, 0.0), vec2(3.0, 0.0), vec2(0.0, -3.0), vec2(0.0, 3.0)] {
                    p.circle_filled(center + d, 2.6, c(color));
                }
                p.circle_filled(center, 2.0, c("#FFB000"));
            }
        }
        Scene::Snow => {
            gradient(&p, rect, (hex("#0E2547"), hex("#3A5E8C")));
            band(0.72, 1.0, c("#F2F7FF"));
            for (x, r) in [(0.15, 26.0), (0.55, 34.0), (0.9, 24.0)] {
                p.circle_filled(at(x, 0.8), r, c("#F2F7FF"));
            }
            for i in 0..40 {
                let x = ((i * 37) % 100) as f32 / 100.0;
                let y = ((i * 53) % 70) as f32 / 100.0;
                p.circle_filled(at(x, y), if i % 3 == 0 { 2.0 } else { 1.3 }, with_alpha(Color32::WHITE, 220));
            }
        }
        Scene::Bricks => {
            band(0.0, 1.0, shade(accent, dark).gamma_multiply(0.7).to_opaque());
            let (w, h) = (26.0, 12.0);
            let mut row = 0;
            let mut y = rect.top();
            while y < rect.bottom() {
                let offset = if row % 2 == 0 { 0.0 } else { w / 2.0 };
                let mut x = rect.left() - offset;
                while x < rect.right() {
                    let brick = Rect::from_min_size(pos2(x + 1.0, y + 1.0), vec2(w - 2.0, h - 2.0));
                    let tint = if (row + (x as i32 / 26)) % 3 == 0 { 0.9 } else { 1.0 };
                    fill(brick, c("#B5533C").gamma_multiply(tint).to_opaque());
                    x += w;
                }
                y += h;
                row += 1;
            }
        }
        Scene::Beach => {
            band(0.0, 0.45, c("#86CDF5"));
            p.circle_filled(at(0.82, 0.18), 13.0, hex("#FFD23F"));
            band(0.45, 0.68, c("#2D9BD8"));
            wave(&p, rect, 0.58, with_alpha(Color32::WHITE, 120), 0.0);
            band(0.68, 1.0, c("#F2D58A"));
            for i in 0..18 {
                p.circle_filled(at(((i * 41) % 100) as f32 / 100.0, 0.74 + ((i * 29) % 25) as f32 / 100.0), 1.2, c("#D8B566"));
            }
        }
        Scene::Space => {
            gradient(&p, rect, (hex("#140A2E"), hex("#3B1E6B")));
            for i in 0..45 {
                p.circle_filled(at(((i * 61) % 100) as f32 / 100.0, ((i * 37) % 100) as f32 / 100.0), if i % 5 == 0 { 1.8 } else { 1.0 }, with_alpha(Color32::WHITE, 210));
            }
            let planet = at(0.78, 0.62);
            p.circle_filled(planet, 18.0, shade(accent, false));
            p.add(Shape::ellipse_stroke(planet, vec2(30.0, 7.0), Stroke::new(2.0_f32, hex("#FFD23F"))));
        }
        Scene::Hearts => {
            band(0.0, 1.0, c("#FFB3CF"));
            for i in 0..14 {
                let center = at(((i * 47) % 100) as f32 / 100.0, ((i * 31) % 100) as f32 / 100.0);
                heart(&p, center, if i % 3 == 0 { 7.0 } else { 4.5 }, c(if i % 2 == 0 { "#FF4F87" } else { "#FFFFFF" }));
            }
        }
        Scene::Parquet => {
            band(0.0, 1.0, c("#B57A44"));
            let (w, h) = (34.0, 10.0);
            let mut row = 0;
            let mut y = rect.top();
            while y < rect.bottom() {
                let mut x = rect.left() - (row % 3) as f32 * 11.0;
                while x < rect.right() {
                    let plank = Rect::from_min_size(pos2(x + 0.5, y + 0.5), vec2(w - 1.0, h - 1.0));
                    let tint = [1.0, 0.92, 1.06, 0.97][((x as i32 / 34) + row) as usize % 4];
                    fill(plank, c("#C68A52").gamma_multiply(tint).to_opaque());
                    x += w;
                }
                y += h;
                row += 1;
            }
        }
    }
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

// An isometric checkered floor: diamonds of two colours.
fn iso_checker(p: &egui::Painter, rect: Rect, light: Color32, dark_tile: Color32, size: f32) {
    p.rect_filled(rect, 0.0, light);
    let half = vec2(size / 2.0, size / 4.0);
    let mut row = 0;
    let mut y = rect.top();
    while y < rect.bottom() + half.y {
        let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { half.x };
        while x < rect.right() + half.x {
            if (row / 2 + (x / size) as i32) % 2 == 0 {
                let c = pos2(x, y);
                p.add(Shape::convex_polygon(vec![c + vec2(0.0, -half.y), c + vec2(half.x, 0.0), c + vec2(0.0, half.y), c + vec2(-half.x, 0.0)], dark_tile, Stroke::NONE));
            }
            x += size;
        }
        y += half.y;
        row += 1;
    }
}

fn tiles_row(p: &egui::Painter, rect: Rect, color: Color32, size: f32) {
    let mut x = rect.left();
    let mut i = 0;
    while x < rect.right() {
        if i % 2 == 0 {
            p.rect_filled(Rect::from_min_size(pos2(x, rect.top()), vec2(size, rect.height())), 0.0, color);
        }
        x += size;
        i += 1;
    }
}

fn wave(p: &egui::Painter, rect: Rect, y: f32, color: Color32, phase: f32) {
    let base = rect.top() + rect.height() * y;
    let points: Vec<Pos2> = (0..=((rect.width() / 4.0) as i32))
        .map(|i| {
            let x = rect.left() + i as f32 * 4.0;
            pos2(x, base + ((x + phase) / 9.0).sin() * 2.0)
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(1.5_f32, color)));
}

fn cloud(p: &egui::Painter, at: Pos2, scale: f32, dark: bool) {
    let (edge, body) = if dark { (hex("#0B2236"), hex("#3E5F7C")) } else { (Fixed::outline(), Color32::WHITE) };
    let puffs = [(-14.0, 2.0, 9.0), (0.0, -4.0, 12.0), (14.0, 2.0, 9.0), (4.0, 5.0, 9.0)];
    for (dx, dy, r) in puffs {
        p.circle_filled(at + vec2(dx, dy) * scale, (r + 1.5) * scale, edge);
    }
    for (dx, dy, r) in puffs {
        p.circle_filled(at + vec2(dx, dy) * scale, r * scale, body);
    }
}

fn heart(p: &egui::Painter, at: Pos2, r: f32, color: Color32) {
    p.circle_filled(at + vec2(-r * 0.5, 0.0), r * 0.55, color);
    p.circle_filled(at + vec2(r * 0.5, 0.0), r * 0.55, color);
    p.add(Shape::convex_polygon(vec![at + vec2(-r * 1.02, r * 0.15), at + vec2(r * 1.02, r * 0.15), at + vec2(0.0, r * 1.2)], color, Stroke::NONE));
}

/// Paints the colour `outside` over the four corners of `rect` outside a rounding of `radius`, so a square picture gets
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

/// How a picture button lays out its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// The plate at the bottom, the icon at the top left (a card in a grid).
    Card,
    /// The icon on the left, the plate filling the rest (a wide or a small button).
    Side,
}

/// A button with a picture behind it: the scene, the icon in a white badge, and the title and description on a plate
/// in the card colour (always readable). `outside` is the colour around the button, for its round corners. True when
/// clicked.
#[allow(clippy::too_many_arguments)]
pub fn button(
    ui: &mut egui::Ui,
    palette: &Palette,
    size: egui::Vec2,
    scene: Scene,
    accent: Color32,
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
    // The hard shadow under it, as under the cards.
    painter.rect_filled(rect.translate(vec2(0.0, 3.0)), radius, with_alpha(palette.outline, 0x40));
    paint(painter, rect, scene, dark, accent);
    round_corners(painter, rect, radius, outside);
    if !enabled {
        painter.rect_filled(rect, radius, with_alpha(palette.surface, 150));
    }

    // The icon in a white badge with the dark edge.
    let badge_size = if layout == Layout::Card { 40.0 } else { (rect.height() - 20.0).clamp(28.0, 44.0) };
    let badge = match layout {
        Layout::Card => Rect::from_min_size(rect.min + vec2(10.0, 10.0), vec2(badge_size, badge_size)),
        Layout::Side => Rect::from_min_size(pos2(rect.left() + 10.0, rect.center().y - badge_size / 2.0), vec2(badge_size, badge_size)),
    };
    painter.rect_filled(badge.translate(vec2(0.0, 2.0)), 8.0, with_alpha(Fixed::outline(), 0x60));
    painter.rect_filled(badge, 8.0, Color32::WHITE);
    painter.rect_stroke(badge, 8.0, Stroke::new(1.5_f32, Fixed::outline()), egui::StrokeKind::Inside);
    let icon_size = badge_size * 0.55;
    icons::image(icon, icon_size, accent).paint_at(ui, Rect::from_center_size(badge.center(), vec2(icon_size, icon_size)));

    // The plate: the card colour, solid, so the text reads the same on every picture.
    let title_font = egui::FontId::new(if layout == Layout::Card { 16.0 } else { 13.5 }, theme::title_family_or_bold());
    let text_font = egui::FontId::proportional(if layout == Layout::Card { 12.0 } else { 11.5 });
    let chevron = 16.0;
    let plate_left = match layout {
        Layout::Card => rect.left() + 8.0,
        Layout::Side => badge.right() + 10.0,
    };
    // A wide side button keeps the right part for the picture.
    let plate_right = match layout {
        Layout::Card => rect.right() - 8.0,
        Layout::Side => (rect.right() - 8.0).min(plate_left + 560.0),
    };
    let text_width = plate_right - plate_left - 20.0 - chevron;
    let rows = match layout {
        Layout::Card => 3,
        Layout::Side => if rect.height() > 80.0 { 3 } else { 2 },
    };
    let title_galley = galley(ui, title, title_font, palette.text, text_width, 1);
    let text_galley = galley(ui, description, text_font, palette.muted, text_width, rows);
    let text_height = title_galley.size().y + 2.0 + text_galley.size().y;
    let plate = match layout {
        Layout::Card => Rect::from_min_max(pos2(plate_left, rect.bottom() - 8.0 - text_height - 16.0), pos2(plate_right, rect.bottom() - 8.0)),
        Layout::Side => Rect::from_min_max(pos2(plate_left, rect.top() + 7.0), pos2(plate_right, rect.bottom() - 7.0)),
    };
    painter.rect_filled(plate, 6.0, palette.card);
    painter.rect_stroke(plate, 6.0, Stroke::new(1.0_f32, with_alpha(palette.outline, 0x55)), egui::StrokeKind::Inside);
    let top = plate.center().y - text_height / 2.0;
    let elided = title_galley.elided || text_galley.elided;
    let title_height = title_galley.size().y;
    painter.galley(pos2(plate.left() + 10.0, top), title_galley, palette.text);
    painter.galley(pos2(plate.left() + 10.0, top + title_height + 2.0), text_galley, palette.muted);
    if enabled {
        let chevron_rect = Rect::from_center_size(pos2(plate.right() - 6.0 - chevron / 2.0, plate.center().y), vec2(chevron, chevron));
        icons::image(Icon::ChevronRight, chevron, palette.subtle).paint_at(ui, chevron_rect);
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
    fn a_name_keeps_its_scene() {
        assert_eq!(for_key("Restart emulator"), for_key("Restart emulator"));
        let scenes: std::collections::HashSet<_> = ["Health check", "Server status", "Restart MariaDB", "Update system", "Import full database", "SSH terminal", "Enable firewall", "Furniture language"].iter().map(|k| for_key(k)).collect();
        assert!(scenes.len() >= 4, "the tool buttons get several different scenes");
    }
}
