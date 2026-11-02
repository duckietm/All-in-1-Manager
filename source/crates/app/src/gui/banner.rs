//! The top of the dashboard in the Hotel look: a hotel view (blue sky with outlined clouds by day,
//! stars by night) with an isometric hotel drawn on the right. Everything is painted here, no images.

use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Shape, Stroke, Vec2};

use super::theme::{hex, with_alpha, Fixed, Palette};

const HEIGHT: f32 = 170.0;

pub fn hotel_banner(ui: &mut egui::Ui, palette: &Palette, title: &str, text: &str) {
    let dark = ui.visuals().dark_mode;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEIGHT), egui::Sense::hover());
    let outline = Fixed::outline();
    let painter = ui.painter_at(rect);

    // The hard shadow under the banner, as under the cards.
    ui.painter().rect_filled(rect.translate(vec2(0.0, 4.0)), 8.0, with_alpha(palette.outline, 0x40));
    sky(&painter, rect, dark);
    if dark {
        stars(&painter, rect);
    }
    for (x, y, scale) in [(620.0, 22.0, 1.25), (930.0, 96.0, 1.0), (1180.0, 30.0, 0.8), (450.0, 12.0, 0.5), (760.0, 128.0, 0.5), (1420.0, 110.0, 0.6)] {
        // Not behind the hotel on the right.
        if rect.left() + x + 80.0 * scale < rect.right() - 230.0 {
            cloud(&painter, rect.min + vec2(x, y), scale, dark);
        }
    }
    for (x, y) in [(590.0, 92.0), (720.0, 70.0), (880.0, 40.0), (1080.0, 136.0), (1110.0, 62.0), (470.0, 148.0)] {
        speck(&painter, rect.min + vec2(x, y), dark);
    }
    hotel(&painter, pos2(rect.right() - 120.0, rect.bottom() - 14.0), dark);

    // The words: white title with a hard dark shadow, the text under it.
    let left = rect.left() + 28.0;
    let wrap = (rect.width() - 300.0).clamp(220.0, 560.0);
    let title_font = egui::FontId::new(28.0, egui::FontFamily::Name("bold".into()));
    let title_galley = painter.layout(title.to_string(), title_font, Color32::WHITE, wrap);
    let shadow_galley = painter.layout(title.to_string(), egui::FontId::new(28.0, egui::FontFamily::Name("bold".into())), outline, wrap);
    let text_color = if dark { Color32::WHITE } else { outline };
    let text_galley = painter.layout(text.to_string(), egui::FontId::proportional(14.0), text_color, wrap);
    let total = title_galley.size().y + 8.0 + text_galley.size().y;
    let top = rect.center().y - total / 2.0;
    painter.galley(pos2(left, top + 2.0), shadow_galley, outline);
    let title_height = title_galley.size().y;
    painter.galley(pos2(left, top), title_galley, Color32::WHITE);
    painter.galley(pos2(left, top + title_height + 8.0), text_galley, text_color);

    ui.painter().rect_stroke(rect, 8.0, Stroke::new(2.0_f32, palette.outline), egui::StrokeKind::Inside);
}

fn sky(painter: &egui::Painter, rect: Rect, dark: bool) {
    let (top, bottom) = if dark { (hex("#071C30"), hex("#1D4A6E")) } else { (hex("#65A1D7"), hex("#78B0E0")) };
    // A vertical gradient (a mesh) between two rounded ends of the top and bottom colours, so the
    // corners stay round.
    let ends = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 16.0));
    painter.rect_filled(ends, 8.0, top);
    painter.rect_filled(ends.translate(vec2(0.0, rect.height() - 16.0)), 8.0, bottom);
    let band = rect.shrink2(vec2(0.0, 8.0));
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(band.left_top(), top);
    mesh.colored_vertex(band.right_top(), top);
    mesh.colored_vertex(band.right_bottom(), bottom);
    mesh.colored_vertex(band.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

/// Stars at fixed places, so the banner looks the same every time.
fn stars(painter: &egui::Painter, rect: Rect) {
    let mut seed = 7u32;
    let mut next = |limit: u32| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (seed >> 16) % limit.max(1)
    };
    for _ in 0..46 {
        let size = next(2) as f32 + 1.5;
        let alpha = 120 + next(135) as u8;
        let x = next(1400) as f32;
        let y = 4.0 + next(136) as f32;
        // None between the words.
        if x < 600.0 && (40.0..140.0).contains(&y) {
            continue;
        }
        let star = Rect::from_min_size(rect.min + vec2(x, y), vec2(size, size));
        painter.rect_filled(star, 0.0, Color32::from_rgba_unmultiplied(255, 255, 230, alpha));
    }
}

/// A hotel view cloud: round puffs on a flat base, one dark outline around the whole and a pale
/// band underneath. The outline is painted first, a little larger, so no lines show inside.
fn cloud(painter: &egui::Painter, at: Pos2, scale: f32, dark: bool) {
    let white = hex(if dark { "#3A5A78" } else { "#FFFFFF" });
    let under = hex(if dark { "#2B4763" } else { "#D3E3F2" });
    let edge = if dark { hex("#06121D") } else { Fixed::outline() };
    let puffs = [(20.0, 24.0, 11.0), (42.0, 17.0, 17.0), (62.0, 24.0, 10.0)];
    let (base_top, base_bottom, width) = (22.0, 34.0, 80.0);
    let point = |x: f32, y: f32| at + vec2(x, y) * scale;
    let round = |left: f32, top: f32, w: f32, h: f32, grow: f32, color: Color32| {
        let r = Rect::from_min_max(point(left, top), point(left + w, top + h)).expand(grow);
        painter.rect_filled(r, r.height() / 2.0, color);
    };
    round(0.0, base_top, width, base_bottom - base_top, 2.0, edge);
    for (x, y, r) in puffs {
        painter.circle_filled(point(x, y), r * scale + 2.0, edge);
    }
    round(0.0, base_top, width, base_bottom - base_top, 0.0, under);
    for (x, y, r) in puffs {
        painter.circle_filled(point(x, y), r * scale, white);
    }
    let inner = Rect::from_min_max(point(4.0, base_top), point(width - 4.0, base_bottom - 4.0));
    painter.rect_filled(inner, 4.0 * scale, white);
}

/// The tiny outlined dots between the clouds.
fn speck(painter: &egui::Painter, at: Pos2, dark: bool) {
    let rect = Rect::from_min_size(at, vec2(7.0, 4.0));
    painter.rect_filled(rect, 2.0, hex(if dark { "#3A5A78" } else { "#FFFFFF" }));
    let edge = if dark { hex("#06121D") } else { Fixed::outline() };
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0_f32, edge), egui::StrokeKind::Outside);
}

// Isometric axes: right and left along the floor, up.
const RIGHT: Vec2 = vec2(0.866, -0.5);
const LEFT: Vec2 = vec2(-0.866, -0.5);
const UP: Vec2 = vec2(0.0, -1.0);

/// Two outlined isometric buildings with window rows and a red HOTEL sign on the tall one.
fn hotel(painter: &egui::Painter, front: Pos2, dark: bool) {
    let tall = Block { front, width: 56.0, depth: 48.0, height: 72.0 };
    tall.draw(painter, dark, 5);
    sign(painter, tall.top_center() + vec2(0.0, -10.0), dark);
    let low = Block { front: front + vec2(-62.0, 6.0), width: 44.0, depth: 40.0, height: 44.0 };
    low.draw(painter, dark, 3);
}

struct Block {
    /// The bottom corner nearest to the viewer.
    front: Pos2,
    width: f32,
    depth: f32,
    height: f32,
}

impl Block {
    fn top_center(&self) -> Pos2 {
        self.front + UP * self.height + RIGHT * (self.width / 2.0) + LEFT * (self.depth / 2.0)
    }

    fn draw(&self, painter: &egui::Painter, dark: bool, floors: usize) {
        let edge = Stroke::new(2.0_f32, Fixed::outline());
        let (b, r, l, h) = (self.front, RIGHT * self.width, LEFT * self.depth, UP * self.height);
        let (top, left_face, right_face) = if dark {
            (hex("#B9AE8E"), hex("#A39A7C"), hex("#857D63"))
        } else {
            (hex("#F6EDCF"), hex("#E6D7AC"), hex("#CBB884"))
        };
        painter.add(Shape::convex_polygon(vec![b, b + l, b + l + h, b + h], left_face, edge));
        painter.add(Shape::convex_polygon(vec![b, b + r, b + r + h, b + h], right_face, edge));
        painter.add(Shape::convex_polygon(vec![b + h, b + r + h, b + r + l + h, b + l + h], top, edge));

        // Windows: rows of small parallelograms on both faces; some are lit, more by night.
        let lit = hex("#FFD25A");
        let glass = hex(if dark { "#2E5878" } else { "#8EC3EA" });
        let floor = self.height / (floors as f32 + 0.5);
        for (axis, length) in [(RIGHT, self.width), (LEFT, self.depth)] {
            let columns = ((length - 6.0) / 11.0).floor().max(1.0) as usize;
            let step = (length - 6.0) / columns as f32;
            for row in 0..floors {
                for column in 0..columns {
                    let at = b + axis * (5.0 + column as f32 * step) + UP * (floor * 0.45 + row as f32 * floor);
                    let on = (row * 3 + column * 5 + floors).is_multiple_of(if dark { 3 } else { 5 });
                    let w = axis * (step - 4.0);
                    let hgt = UP * (floor * 0.55);
                    painter.add(Shape::convex_polygon(
                        vec![at, at + w, at + w + hgt, at + hgt],
                        if on { lit } else { glass },
                        Stroke::new(1.0_f32, Fixed::outline()),
                    ));
                }
            }
        }
    }
}

fn sign(painter: &egui::Painter, center: Pos2, dark: bool) {
    let rect = Rect::from_center_size(center, vec2(52.0, 18.0));
    painter.line_segment([center + vec2(-14.0, 9.0), center + vec2(-14.0, 18.0)], Stroke::new(2.0_f32, Fixed::outline()));
    painter.line_segment([center + vec2(14.0, 9.0), center + vec2(14.0, 18.0)], Stroke::new(2.0_f32, Fixed::outline()));
    painter.rect_filled(rect, 3.0, hex(if dark { "#E0442E" } else { "#D93A26" }));
    painter.rect_stroke(rect, 3.0, Stroke::new(2.0_f32, Fixed::outline()), egui::StrokeKind::Outside);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "HOTEL",
        egui::FontId::new(12.0, egui::FontFamily::Name("pixel".into())),
        hex("#FFE27A"),
    );
}
