//! Small drawing helpers shared by the widgets.

use eframe::egui::{Color32, Mesh, Painter, Pos2, Rect, Shape, pos2};

/// Fills a rectangle with evenly spaced colour stops.
pub fn horizontal_gradient(painter: &Painter, rect: Rect, stops: &[Color32]) {
    let mut mesh = Mesh::default();
    let segments = stops.len().saturating_sub(1).max(1);
    for (index, color) in stops.iter().enumerate() {
        let x = rect.left() + rect.width() * index as f32 / segments as f32;
        mesh.colored_vertex(pos2(x, rect.top()), *color);
        mesh.colored_vertex(pos2(x, rect.bottom()), *color);
    }
    for index in 0..segments as u32 {
        let first = index * 2;
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 1, first + 2, first + 3);
    }
    painter.add(Shape::mesh(mesh));
}

/// A closed filled triangle.
pub fn triangle(painter: &Painter, points: [Pos2; 3], color: Color32) {
    let mut mesh = Mesh::default();
    for point in points {
        mesh.colored_vertex(point, color);
    }
    mesh.add_triangle(0, 1, 2);
    painter.add(Shape::mesh(mesh));
}
