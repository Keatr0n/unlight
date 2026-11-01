//! The colour grading wheel: hue around the rim, saturation outward from the centre.

use crate::ui::theme;
use eframe::egui::{
    Color32, Mesh, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo, ecolor::HsvaGamma,
    vec2,
};
use std::f32::consts::TAU;
use unlight_recipe::GradeWheel;

const SEGMENTS: u32 = 72;
const RINGS: u32 = 8;
const PUCK_RADIUS: f32 = 4.5;
const FINE_DRAG: f32 = 0.1;

/// The disc's colour at a polar position. Value stays low so the wheel never out-shouts
/// the photo.
fn disc_color(angle: f32, reach: f32) -> Color32 {
    HsvaGamma {
        h: angle / TAU,
        s: reach * 0.8,
        v: 0.46 + reach * 0.3,
        a: 1.0,
    }
    .into()
}

/// Puck position within a unit disc for a grade.
pub fn puck_offset(hue: f32, saturation: f32) -> Vec2 {
    let angle = hue.to_radians();
    let reach = (saturation / 100.0).clamp(0.0, 1.0);
    vec2(angle.cos() * reach, -angle.sin() * reach)
}

/// The grade for a point in a unit disc. Beyond the rim it stays on the rim.
pub fn grade_at(offset: Vec2) -> (f32, f32) {
    let saturation = (offset.length() * 100.0).min(100.0);
    let hue = (-offset.y).atan2(offset.x).to_degrees().rem_euclid(360.0);
    (hue, saturation)
}

fn disc_mesh(center: Pos2, radius: f32) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, disc_color(0.0, 0.0));
    for ring in 1..=RINGS {
        let reach = ring as f32 / RINGS as f32;
        for segment in 0..SEGMENTS {
            let angle = TAU * segment as f32 / SEGMENTS as f32;
            let position = center + radius * reach * vec2(angle.cos(), -angle.sin());
            mesh.colored_vertex(position, disc_color(angle, reach));
        }
    }
    let vertex = |ring: u32, segment: u32| 1 + (ring - 1) * SEGMENTS + segment % SEGMENTS;
    for segment in 0..SEGMENTS {
        mesh.add_triangle(0, vertex(1, segment), vertex(1, segment + 1));
        for ring in 1..RINGS {
            let (a, b) = (vertex(ring, segment), vertex(ring, segment + 1));
            let (c, d) = (vertex(ring + 1, segment), vertex(ring + 1, segment + 1));
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, c, d);
        }
    }
    mesh
}

/// A wheel `diameter` across. Returns the response; `changed` means the grade moved.
pub fn show(ui: &mut Ui, grade: &mut GradeWheel, name: &str, diameter: f32) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let id = ui.auto_id_with(name);
    let mut response = ui.interact(rect, id, Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Slider, ui.is_enabled(), name));
    let center = rect.center();
    let radius = diameter * 0.5 - 1.0;

    if response.double_clicked() {
        if grade.hue != 0.0 || grade.saturation != 0.0 {
            grade.hue = 0.0;
            grade.saturation = 0.0;
            response.mark_changed();
        }
    } else if response.is_pointer_button_down_on() {
        let fine = ui.input(|input| input.modifiers.alt);
        let moved = if fine {
            let base = puck_offset(grade.hue, grade.saturation);
            let drag = response.drag_delta() / radius * FINE_DRAG;
            Some(base + drag)
        } else {
            response.interact_pointer_pos().map(|pos| (pos - center) / radius)
        };
        if let Some(offset) = moved {
            let (hue, saturation) = grade_at(offset);
            if hue != grade.hue || saturation != grade.saturation {
                grade.hue = hue;
                grade.saturation = saturation;
                response.mark_changed();
            }
        }
    }

    paint(ui, rect, center, radius, grade, &response);
    response
}

fn paint(ui: &Ui, rect: Rect, center: Pos2, radius: f32, grade: &GradeWheel, response: &Response) {
    let painter = ui.painter_at(rect.expand(2.0));
    painter.add(Shape::mesh(disc_mesh(center, radius)));
    painter.circle_stroke(center, radius, Stroke::new(1.0, Color32::from_black_alpha(90)));

    let line = Stroke::new(1.0, Color32::from_black_alpha(50));
    painter.hline(rect.x_range(), center.y, line);
    painter.vline(center.x, rect.y_range(), line);

    let puck = center + puck_offset(grade.hue, grade.saturation) * radius;
    if grade.saturation > 0.0 {
        painter.line_segment([center, puck], Stroke::new(1.0, Color32::from_white_alpha(120)));
    }
    let active = response.hovered() || response.dragged();
    let fill = disc_color(grade.hue.to_radians(), (grade.saturation / 100.0).min(1.0));
    let outline = if active {
        theme::HANDLE_HOVER
    } else {
        theme::HANDLE
    };
    painter.circle_filled(puck, PUCK_RADIUS + 1.0, Color32::from_black_alpha(120));
    painter.circle(puck, PUCK_RADIUS, fill, Stroke::new(1.5, outline));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_round_trips_through_a_grade() {
        for (hue, saturation) in [(30.0, 40.0), (200.0, 80.0), (359.0, 100.0)] {
            let (back_hue, back_saturation) = grade_at(puck_offset(hue, saturation));
            assert!((back_hue - hue).abs() < 0.1, "{back_hue}");
            assert!((back_saturation - saturation).abs() < 0.1);
        }
    }

    #[test]
    fn dragging_past_the_rim_holds_full_saturation() {
        let (_, saturation) = grade_at(vec2(3.0, 4.0));
        assert_eq!(saturation, 100.0);
    }

    #[test]
    fn hue_zero_points_right_and_ninety_points_up() {
        assert!((puck_offset(0.0, 100.0) - vec2(1.0, 0.0)).length() < 1e-5);
        assert!((puck_offset(90.0, 100.0) - vec2(0.0, -1.0)).length() < 1e-5);
    }
}
