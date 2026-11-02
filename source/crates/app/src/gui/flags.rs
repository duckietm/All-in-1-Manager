//! Country flags of the Habbo hotels, drawn as shapes (Windows has no flag emoji), next to the languages in Settings.
//! The same drawing as the All-in-1 Converter's.

use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Shape, Stroke};

use super::theme::hex;

const W: f32 = 30.0;
const H: f32 = 20.0;

/// Paints the flag of the hotel at `domain` into `rect`.
pub fn paint(painter: &egui::Painter, rect: Rect, domain: &str) {
    let scale = rect.width() / W;
    let at = |x: f32, y: f32| pos2(rect.left() + x * scale, rect.top() + y * scale);
    let fill = |x: f32, y: f32, w: f32, h: f32, color: &str| {
        painter.rect_filled(Rect::from_min_size(at(x, y), vec2(w * scale, h * scale)), 0.0, hex(color));
    };
    let circle = |x: f32, y: f32, r: f32, color: &str| painter.circle_filled(at(x, y), r * scale, hex(color));

    let clip = painter.with_clip_rect(rect);
    let _ = &clip;
    match Some(domain) {
        Some("habbo.nl") => horizontal(&fill, "#AE1C28", "#FFFFFF", "#21468B"),
        Some("habbo.de") => horizontal(&fill, "#000000", "#DD0000", "#FFCE00"),
        Some("habbo.fr") => vertical(&fill, "#0055A4", "#FFFFFF", "#EF4135"),
        Some("habbo.it") => vertical(&fill, "#009246", "#FFFFFF", "#CE2B37"),
        Some("habbo.es") => {
            fill(0.0, 0.0, W, H, "#AA151B");
            fill(0.0, H / 4.0, W, H / 2.0, "#F1BF00");
        }
        Some("habbo.fi") => {
            fill(0.0, 0.0, W, H, "#FFFFFF");
            fill(W * 5.0 / 18.0, 0.0, W * 3.0 / 18.0, H, "#002F6C");
            fill(0.0, H * 4.0 / 11.0, W, H * 3.0 / 11.0, "#002F6C");
        }
        Some("habbo.com.tr") => {
            fill(0.0, 0.0, W, H, "#E30A17");
            circle(11.0, 10.0, 5.0, "#FFFFFF");
            circle(12.25, 10.0, 4.0, "#E30A17");
            star(painter, &at, 16.6, 10.0, 2.5, scale, hex("#FFFFFF"));
        }
        Some("habbo.com.br") => {
            fill(0.0, 0.0, W, H, "#009C3B");
            let points = vec![at(2.6, 10.0), at(15.0, 1.7), at(27.4, 10.0), at(15.0, 18.3)];
            painter.add(Shape::convex_polygon(points, hex("#FFDF00"), Stroke::NONE));
            circle(15.0, 10.0, 5.1, "#002776");
        }
        _ => {
            // Habbo.com is international.
            fill(0.0, 0.0, W, H, "#2F6FDE");
            let center = at(15.0, 10.0);
            let r = 6.5 * scale;
            painter.circle_stroke(center, r, Stroke::new(1.3 * scale.max(0.6), Color32::WHITE));
            painter.line_segment([pos2(center.x - r, center.y), pos2(center.x + r, center.y)], Stroke::new(1.0 * scale.max(0.6), Color32::WHITE));
            painter.add(Shape::ellipse_stroke(center, vec2(r * 0.45, r), Stroke::new(1.0 * scale.max(0.6), Color32::WHITE)));
        }
    }
    painter.rect_stroke(rect, egui::CornerRadius::same((rect.width() / 12.0).max(2.0) as u8), Stroke::new(1.0_f32, Color32::from_black_alpha(0x26)), egui::StrokeKind::Inside);
}

fn horizontal(fill: &dyn Fn(f32, f32, f32, f32, &str), top: &str, middle: &str, bottom: &str) {
    fill(0.0, 0.0, W, H / 3.0, top);
    fill(0.0, H / 3.0, W, H / 3.0, middle);
    fill(0.0, H * 2.0 / 3.0, W, H / 3.0 + 0.01, bottom);
}

fn vertical(fill: &dyn Fn(f32, f32, f32, f32, &str), left: &str, middle: &str, right: &str) {
    fill(0.0, 0.0, W / 3.0, H, left);
    fill(W / 3.0, 0.0, W / 3.0, H, middle);
    fill(W * 2.0 / 3.0, 0.0, W / 3.0 + 0.01, H, right);
}

/// A five-pointed star with one point towards the hoist, as on the Turkish flag.
fn star(painter: &egui::Painter, at: &dyn Fn(f32, f32) -> Pos2, cx: f32, cy: f32, radius: f32, _scale: f32, color: Color32) {
    let points: Vec<Pos2> = (0..10)
        .map(|i| {
            let r = if i % 2 == 0 { radius } else { radius * 0.4 };
            let angle = std::f32::consts::PI + i as f32 * std::f32::consts::PI / 5.0;
            at(cx + r * angle.cos(), cy + r * angle.sin())
        })
        .collect();
    // Concave: fan of triangles from the centre.
    let center = at(cx, cy);
    for i in 0..10 {
        let a = points[i];
        let b = points[(i + 1) % 10];
        painter.add(Shape::convex_polygon(vec![center, a, b], color, Stroke::NONE));
    }
}
