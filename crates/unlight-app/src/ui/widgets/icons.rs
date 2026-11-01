//! Painted icons. Drawing them keeps them crisp at any fractional scale, and keeps one
//! typeface in the interface.

use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use std::f32::consts::{FRAC_PI_2, TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Crop,
    Heal,
    Masks,
    RedEye,
    Reset,
    Tick,
    Picker,
    Lock,
    Unlock,
    Flip,
    Plus,
    Close,
    Split,
    SideBySide,
    Clipping,
    Proof,
    Open,
    Star,
    Flag,
    PanelLeft,
    PanelRight,
    Export,
    Grid,
    Loupe,
    Compare,
    Survey,
    Warning,
    SortUp,
    SortDown,
}

const WEIGHT: f32 = 1.25;

/// A disclosure triangle, turning from pointing right to pointing down.
pub fn chevron(painter: &Painter, center: Pos2, openness: f32, color: Color32) {
    let angle = openness * FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    let turn = |x: f32, y: f32| center + vec2(x * cos - y * sin, x * sin + y * cos);
    let points = vec![turn(-1.75, -3.5), turn(1.75, 0.0), turn(-1.75, 3.5)];
    painter.line(points, Stroke::new(WEIGHT, color));
}

fn circle_points(center: Pos2, radius: f32, from: f32, to: f32, count: usize) -> Vec<Pos2> {
    (0..=count)
        .map(|index| {
            let angle = from + (to - from) * index as f32 / count as f32;
            center + radius * vec2(angle.cos(), angle.sin())
        })
        .collect()
}

pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let stroke = Stroke::new(WEIGHT, color);
    let c = rect.center();
    let at = |x: f32, y: f32| c + vec2(x, y);
    match icon {
        Icon::Crop => {
            painter.line(vec![at(-4.0, -7.0), at(-4.0, 4.0), at(7.0, 4.0)], stroke);
            painter.line(vec![at(-7.0, -4.0), at(4.0, -4.0), at(4.0, 7.0)], stroke);
        }
        Icon::Heal => {
            painter.circle_stroke(c, 6.5, stroke);
            painter.line_segment([at(0.0, -3.5), at(0.0, 3.5)], stroke);
            painter.line_segment([at(-3.5, 0.0), at(3.5, 0.0)], stroke);
        }
        Icon::Masks => {
            painter.circle_stroke(at(-2.0, 0.0), 5.5, stroke);
            painter.circle_filled(at(2.5, 0.0), 4.0, color.gamma_multiply(0.45));
            painter.circle_stroke(at(2.5, 0.0), 4.5, stroke);
        }
        Icon::RedEye => {
            painter.circle_filled(c, 2.5, color);
            painter.line(circle_points(at(0.0, 5.0), 9.0, -2.45, -0.69, 12), stroke);
            painter.line(circle_points(at(0.0, -5.0), 9.0, 0.69, 2.45, 12), stroke);
        }
        Icon::Reset => {
            let points = circle_points(c, 5.0, -2.2, 3.6, 18);
            let tip = points[points.len() - 1];
            painter.line(points, stroke);
            painter.line_segment([tip, tip + vec2(-2.5, -2.0)], stroke);
            painter.line_segment([tip, tip + vec2(2.5, -1.75)], stroke);
        }
        Icon::Tick => {
            painter.line(
                vec![at(-3.5, 0.0), at(-1.0, 2.5), at(3.5, -2.5)],
                Stroke::new(1.75, color),
            );
        }
        Icon::Picker => {
            painter.line_segment([at(-5.5, 5.5), at(2.0, -2.0)], stroke);
            painter.line_segment([at(-1.0, -4.5), at(4.5, 1.0)], Stroke::new(1.75, color));
            painter.circle_filled(at(3.5, -3.5), 2.75, color);
        }
        Icon::Lock | Icon::Unlock => {
            let body = Rect::from_center_size(at(0.0, 2.0), vec2(9.0, 7.0));
            painter.rect_stroke(body, 1.5, stroke, StrokeKind::Middle);
            let lift = if icon == Icon::Unlock { -1.5 } else { 0.0 };
            painter.line(
                circle_points(at(0.0, -2.0 + lift), 2.75, -TAU / 2.0, 0.0, 10),
                stroke,
            );
        }
        Icon::Flip => {
            painter.rect_stroke(
                Rect::from_center_size(at(-1.5, 1.5), vec2(7.0, 10.0)),
                1.0,
                stroke,
                StrokeKind::Middle,
            );
            painter.line(circle_points(at(-1.5, 1.5), 8.0, -1.35, -0.2, 8), stroke);
        }
        Icon::Plus => {
            painter.line_segment([at(0.0, -4.5), at(0.0, 4.5)], stroke);
            painter.line_segment([at(-4.5, 0.0), at(4.5, 0.0)], stroke);
        }
        Icon::Close => {
            painter.line_segment([at(-4.0, -4.0), at(4.0, 4.0)], stroke);
            painter.line_segment([at(-4.0, 4.0), at(4.0, -4.0)], stroke);
        }
        Icon::Split => {
            let frame = Rect::from_center_size(c, vec2(15.0, 11.0));
            painter.rect_stroke(frame, 1.5, stroke, StrokeKind::Middle);
            painter.rect_filled(
                Rect::from_min_max(frame.min, pos2(c.x, frame.max.y)),
                1.5,
                color.gamma_multiply(0.45),
            );
            painter.line_segment([pos2(c.x, frame.top()), pos2(c.x, frame.bottom())], stroke);
        }
        Icon::SideBySide => {
            for x in [-4.25, 4.25] {
                let frame = Rect::from_center_size(at(x, 0.0), vec2(6.5, 11.0));
                painter.rect_stroke(frame, 1.5, stroke, StrokeKind::Middle);
            }
        }
        Icon::Clipping => {
            let frame = Rect::from_center_size(c, vec2(13.0, 13.0));
            painter.rect_stroke(frame, 1.5, stroke, StrokeKind::Middle);
            super::paint::triangle(
                painter,
                [frame.left_top(), frame.right_top(), frame.left_bottom()],
                color.gamma_multiply(0.55),
            );
        }
        Icon::Proof => {
            let sheet = Rect::from_center_size(c, vec2(11.0, 14.0));
            painter.rect_stroke(sheet, 1.0, stroke, StrokeKind::Middle);
            painter.circle_filled(c + vec2(0.0, -1.5), 2.75, color);
        }
        Icon::Open => {
            painter.line(
                vec![
                    at(-6.5, 5.0),
                    at(-6.5, -5.0),
                    at(-2.5, -5.0),
                    at(-0.5, -3.0),
                    at(6.0, -3.0),
                    at(6.0, 5.0),
                    at(-6.5, 5.0),
                ],
                stroke,
            );
        }
        Icon::Star => star(painter, c, 3.75, color),
        Icon::Export => {
            painter.line(
                vec![at(-6.0, 1.0), at(-6.0, 6.0), at(6.0, 6.0), at(6.0, 1.0)],
                stroke,
            );
            painter.line_segment([at(0.0, 3.0), at(0.0, -6.0)], stroke);
            painter.line(vec![at(-3.5, -2.5), at(0.0, -6.0), at(3.5, -2.5)], stroke);
        }
        Icon::Grid => {
            for x in [-4.5, 0.0, 4.5] {
                for y in [-4.5, 0.0, 4.5] {
                    painter.rect_filled(Rect::from_center_size(at(x, y), vec2(3.5, 3.5)), 0.5, color);
                }
            }
        }
        Icon::Loupe => {
            let frame = Rect::from_center_size(c, vec2(14.0, 10.0));
            painter.rect_filled(frame, 1.0, color.gamma_multiply(0.5));
            painter.rect_stroke(frame, 1.0, stroke, StrokeKind::Middle);
        }
        Icon::Compare => {
            for x in [-4.25, 4.25] {
                let frame = Rect::from_center_size(at(x, 0.0), vec2(7.0, 9.0));
                painter.rect_filled(frame, 1.0, color.gamma_multiply(0.5));
            }
            painter.rect_stroke(
                Rect::from_center_size(at(4.25, 0.0), vec2(7.0, 9.0)),
                1.0,
                stroke,
                StrokeKind::Middle,
            );
        }
        Icon::Survey => {
            for (x, y) in [(-4.0, -3.0), (4.0, -3.0), (-4.0, 3.0), (4.0, 3.0)] {
                painter.rect_stroke(
                    Rect::from_center_size(at(x, y), vec2(6.5, 4.5)),
                    0.5,
                    stroke,
                    StrokeKind::Middle,
                );
            }
        }
        Icon::Warning => {
            let thin = Stroke::new(1.5, color);
            painter.line(
                vec![at(0.0, -6.0), at(6.5, 5.5), at(-6.5, 5.5), at(0.0, -6.0)],
                thin,
            );
            painter.line_segment([at(0.0, -2.0), at(0.0, 1.5)], thin);
            painter.circle_filled(at(0.0, 3.5), 0.9, color);
        }
        Icon::SortUp | Icon::SortDown => {
            let flip = if icon == Icon::SortUp { 1.0 } else { -1.0 };
            painter.line_segment([at(0.0, 5.0 * flip), at(0.0, -5.0 * flip)], stroke);
            painter.line(
                vec![at(-3.5, -1.5 * flip), at(0.0, -5.0 * flip), at(3.5, -1.5 * flip)],
                stroke,
            );
        }
        Icon::PanelLeft | Icon::PanelRight => {
            let frame = Rect::from_center_size(c, vec2(15.0, 11.0));
            painter.rect_stroke(frame, 1.5, stroke, StrokeKind::Middle);
            let x = if icon == Icon::PanelLeft {
                frame.left() + 5.0
            } else {
                frame.right() - 5.0
            };
            painter.line_segment([pos2(x, frame.top()), pos2(x, frame.bottom())], stroke);
        }
        Icon::Flag => {
            painter.line_segment([at(-3.5, -5.5), at(-3.5, 5.5)], stroke);
            painter.line(vec![at(-3.5, -5.0), at(4.0, -3.0), at(-3.5, 0.5)], stroke);
        }
    }
}

/// A filled five-point star.
pub fn star(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let point = |index: usize, reach: f32| {
        let angle = -FRAC_PI_2 + TAU * index as f32 / 10.0;
        center + reach * vec2(angle.cos(), angle.sin())
    };
    for index in 0..5 {
        let outer = index * 2;
        super::paint::triangle(
            painter,
            [
                point((outer + 9) % 10, radius * 0.42),
                point(outer, radius),
                point(outer + 1, radius * 0.42),
            ],
            color,
        );
        super::paint::triangle(
            painter,
            [
                center,
                point(outer + 1, radius * 0.42),
                point((outer + 9) % 10, radius * 0.42),
            ],
            color,
        );
    }
}
