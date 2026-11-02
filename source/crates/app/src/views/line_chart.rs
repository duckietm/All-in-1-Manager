//! The rolling line graph of the live pages (LineChart.cs): the last `capacity` values per series, newest on the
//! right, drawn with egui's painter.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, Pos2, Stroke};

pub struct Series {
    pub color: Color32,
    pub values: VecDeque<f64>,
}

pub struct LineChart {
    pub capacity: usize,
    /// Fixed top of the graph, or 0 to scale to the data.
    pub maximum: f64,
    pub lines: Vec<Series>,
}

impl LineChart {
    pub fn new(colors: &[Color32]) -> Self {
        Self { capacity: 60, maximum: 0.0, lines: colors.iter().map(|&color| Series { color, values: VecDeque::new() }).collect() }
    }

    pub fn with_maximum(mut self, maximum: f64) -> Self {
        self.maximum = maximum;
        self
    }

    pub fn add(&mut self, values: &[f64]) {
        let capacity = self.capacity;
        for (line, value) in self.lines.iter_mut().zip(values) {
            line.values.push_back(*value);
            if line.values.len() > capacity {
                line.values.pop_front();
            }
        }
    }

    pub fn clear(&mut self) {
        for line in &mut self.lines {
            line.values.clear();
        }
    }

    /// The value at the top of the graph.
    pub fn top(&self) -> f64 {
        if self.maximum > 0.0 {
            return self.maximum;
        }
        let max = self.lines.iter().flat_map(|l| l.values.iter().copied()).fold(0.0_f64, f64::max);
        if max > 0.0 {
            max * 1.15
        } else {
            1.0
        }
    }

    /// The points of a series in a `w` x `h` box whose top left is the origin.
    pub fn points(&self, line: &Series, w: f32, h: f32) -> Vec<Pos2> {
        let top = self.top();
        let step = w / (self.capacity.max(2) - 1) as f32;
        let x0 = w - line.values.len().saturating_sub(1) as f32 * step;
        line.values
            .iter()
            .enumerate()
            .map(|(i, v)| egui::pos2(x0 + i as f32 * step, h - (v / top).clamp(0.0, 1.0) as f32 * (h - 2.0) - 1.0))
            .collect()
    }

    /// Draws the graph across the available width.
    pub fn show(&self, ui: &mut egui::Ui, height: f32, grid: Color32) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
        let (w, h) = (rect.width(), rect.height());
        if w <= 2.0 || h <= 2.0 {
            return response;
        }
        let painter = ui.painter_at(rect);
        for i in 1..4 {
            let y = rect.top() + (h * i as f32 / 4.0).round() + 0.5;
            painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], Stroke::new(1.0_f32, grid));
        }
        for line in &self.lines {
            if line.values.len() < 2 {
                continue;
            }
            let points: Vec<Pos2> = self.points(line, w, h).into_iter().map(|p| p + rect.min.to_vec2()).collect();
            // The area under the line, one trapezoid per segment, so the shape need not be convex.
            let fill = Color32::from_rgba_unmultiplied(line.color.r(), line.color.g(), line.color.b(), 36);
            let bottom = rect.bottom();
            let mut mesh = egui::Mesh::default();
            for pair in points.windows(2) {
                let base = mesh.vertices.len() as u32;
                for p in [pair[0], pair[1], egui::pos2(pair[1].x, bottom), egui::pos2(pair[0].x, bottom)] {
                    mesh.colored_vertex(p, fill);
                }
                mesh.add_triangle(base, base + 1, base + 2);
                mesh.add_triangle(base, base + 2, base + 3);
            }
            painter.add(egui::Shape::mesh(mesh));
            painter.add(egui::Shape::line(points, Stroke::new(2.0_f32, line.color)));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_and_top() {
        let mut chart = LineChart::new(&[Color32::RED, Color32::BLUE]);
        assert_eq!(chart.top(), 1.0);
        for i in 0..70 {
            chart.add(&[i as f64, 1.0]);
        }
        assert_eq!(chart.lines[0].values.len(), 60);
        assert_eq!(chart.lines[0].values[0], 10.0);
        assert!((chart.top() - 69.0 * 1.15).abs() < 1e-9);
        assert_eq!(LineChart::new(&[Color32::RED]).with_maximum(100.0).top(), 100.0);
        chart.clear();
        assert!(chart.lines.iter().all(|l| l.values.is_empty()));
    }

    #[test]
    fn newest_on_the_right() {
        let mut chart = LineChart::new(&[Color32::RED]).with_maximum(100.0);
        chart.add(&[0.0]);
        chart.add(&[100.0]);
        let points = chart.points(&chart.lines[0], 590.0, 102.0);
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].x, 590.0);
        assert_eq!(points[0].x, 580.0);
        assert_eq!(points[0].y, 101.0);
        assert_eq!(points[1].y, 1.0);
    }
}
