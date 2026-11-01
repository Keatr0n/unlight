//! The tone curve plot: a point curve you drag, and the parametric view with its split
//! handles. Point logic is kept apart from drawing so it can be tested directly.

use super::paint;
use crate::{renderer::Histogram, ui::theme};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Id, Mesh, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui,
    WidgetInfo, pos2, vec2,
};
use unlight_recipe::{Curve, CurvePoint};

const PLOT_HEIGHT: f32 = 236.0;
const GRAB_RADIUS: f32 = 9.0;
const POINT_RADIUS: f32 = 3.5;
const MIN_SPACING: f32 = 0.012;
const FINE_DRAG: f32 = 0.1;
const SPLIT_BAR_HEIGHT: f32 = 16.0;
const MIN_SPLIT_GAP: f32 = 5.0;

/// The points of a curve, with the implied endpoints spelled out.
pub fn editable_points(curve: &Curve) -> Vec<CurvePoint> {
    if curve.0.len() >= 2 {
        let mut points = curve.0.clone();
        points.sort_by(|a, b| a.x.total_cmp(&b.x));
        points
    } else {
        vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
    }
}

fn nearest_point(points: &[CurvePoint], x: f32, y: f32, tolerance: [f32; 2]) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let (dx, dy) = ((point.x - x) / tolerance[0], (point.y - y) / tolerance[1]);
            (index, dx * dx + dy * dy)
        })
        .filter(|(_, distance)| *distance <= 1.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// Adds a point on the curve at `x`, returning its index, or `None` if one is already too
/// close there.
pub fn insert_point(points: &mut Vec<CurvePoint>, x: f32, y: f32) -> Option<usize> {
    let index = points.partition_point(|point| point.x < x);
    let clear_of = |point: &CurvePoint| (point.x - x).abs() >= MIN_SPACING;
    if !points.iter().all(clear_of) {
        return None;
    }
    points.insert(index, CurvePoint::new(x, y));
    Some(index)
}

/// Moves a point, keeping it between its neighbours.
pub fn move_point(points: &mut [CurvePoint], index: usize, x: f32, y: f32) {
    let low = if index == 0 {
        0.0
    } else {
        points[index - 1].x + MIN_SPACING
    };
    let high = if index + 1 == points.len() {
        1.0
    } else {
        points[index + 1].x - MIN_SPACING
    };
    let point = &mut points[index];
    point.x = x.clamp(low, high.max(low));
    point.y = y.clamp(0.0, 1.0);
}

/// Removes a point. Endpoints stay, a curve always spans the whole range.
pub fn remove_point(points: &mut Vec<CurvePoint>, index: usize) -> bool {
    let inner = index > 0 && index + 1 < points.len();
    if inner {
        points.remove(index);
    }
    inner
}

/// Stores points back, using the empty identity when nothing is left to say.
pub fn store_points(curve: &mut Curve, points: Vec<CurvePoint>) {
    let stored = Curve(points);
    *curve = if stored.is_identity() {
        Curve::default()
    } else {
        stored
    };
}

fn paint_plot_background(ui: &Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, theme::PANEL_DEEP);
    let line = Stroke::new(1.0, Color32::from_gray(0x2c));
    for quarter in 1..4 {
        let t = quarter as f32 / 4.0;
        painter.vline(rect.left() + t * rect.width(), rect.y_range(), line);
        painter.hline(rect.x_range(), rect.top() + t * rect.height(), line);
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        Stroke::new(1.0, Color32::from_gray(0x3a)),
    );
}

fn paint_histogram(ui: &Ui, rect: Rect, histogram: &Histogram) {
    let mut totals = [0f32; 256];
    for channel in 0..3 {
        for (total, count) in totals.iter_mut().zip(histogram.channel(channel)) {
            *total += *count as f32;
        }
    }
    let peak = totals[1..255].iter().copied().fold(1.0f32, f32::max);
    let color = Color32::from_gray(0x36);
    let mut mesh = Mesh::default();
    for (index, total) in totals.iter().enumerate() {
        let t = index as f32 / 255.0;
        let level = (total / peak).min(1.0).powf(0.6);
        let x = rect.left() + t * rect.width();
        mesh.colored_vertex(pos2(x, rect.bottom()), color);
        mesh.colored_vertex(pos2(x, rect.bottom() - level * rect.height() * 0.7), color);
    }
    for index in 0..255u32 {
        let first = index * 2;
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 1, first + 2, first + 3);
    }
    ui.painter().add(Shape::mesh(mesh));
}

fn paint_curve_line(ui: &Ui, rect: Rect, samples: &[f32], color: Color32) {
    let last = samples.len().saturating_sub(1).max(1) as f32;
    let step = (samples.len() / 256).max(1);
    let points: Vec<Pos2> = samples
        .iter()
        .enumerate()
        .step_by(step)
        .map(|(index, value)| {
            pos2(
                rect.left() + index as f32 / last * rect.width(),
                rect.bottom() - value.clamp(0.0, 1.0) * rect.height(),
            )
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.5, color)));
}

fn plot_rect(ui: &mut Ui, height: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    rect
}

fn readout(ui: &Ui, rect: Rect, text: String) {
    ui.painter().text(
        rect.left_top() + vec2(6.0, 5.0),
        Align2::LEFT_TOP,
        text,
        theme::small(),
        theme::TEXT_DIM,
    );
}

/// The point being dragged, kept between frames.
#[derive(Clone, Copy, Default)]
struct PointDrag {
    index: usize,
    exact: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveChange {
    Point,
    Removed,
}

pub struct PointCurve<'a> {
    pub curve: &'a mut Curve,
    /// What the engine makes of a curve, sampled evenly over 0..=1.
    pub sampler: &'a dyn Fn(&Curve) -> Vec<f32>,
    pub color: Color32,
    pub histogram: Option<&'a Histogram>,
    pub label: &'a str,
}

impl PointCurve<'_> {
    pub fn show(self, ui: &mut Ui) -> Option<CurveChange> {
        let Self {
            curve,
            sampler,
            color,
            histogram,
            label,
        } = self;
        let rect = plot_rect(ui, PLOT_HEIGHT);
        let id = ui.auto_id_with(label);
        let response = ui.interact(rect, id, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(Role::Image, ui.is_enabled(), label));
        paint_plot_background(ui, rect);
        if let Some(histogram) = histogram {
            paint_histogram(ui, rect, histogram);
        }

        let to_curve = |pos: Pos2| {
            [
                ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
                ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0),
            ]
        };
        let to_screen = |point: &CurvePoint| {
            pos2(
                rect.left() + point.x * rect.width(),
                rect.bottom() - point.y * rect.height(),
            )
        };

        let mut points = editable_points(curve);
        let samples = sampler(curve);
        let change = interact(ui, &response, id, &mut points, &samples, rect, to_curve);
        match change {
            Some(CurveChange::Point) => curve.0.clone_from(&points),
            Some(CurveChange::Removed) => store_points(curve, points.clone()),
            None if response.drag_stopped() => store_points(curve, points.clone()),
            None => {}
        }
        paint_curve_line(ui, rect, &sampler(curve), color);

        let hovered_point = response.hover_pos().and_then(|pos| {
            let [x, y] = to_curve(pos);
            nearest_point(
                &points,
                x,
                y,
                [GRAB_RADIUS / rect.width(), GRAB_RADIUS / rect.height()],
            )
        });
        let held = ui
            .data(|data| data.get_temp::<PointDrag>(id))
            .map(|drag| drag.index);
        for (index, point) in points.iter().enumerate() {
            let active = hovered_point == Some(index) || held == Some(index);
            let fill = if active {
                theme::HANDLE_HOVER
            } else {
                theme::HANDLE
            };
            ui.painter()
                .circle_filled(to_screen(point), POINT_RADIUS + 1.5, theme::PANEL_DEEP);
            ui.painter().circle_filled(to_screen(point), POINT_RADIUS, fill);
        }

        if hovered_point.is_some() || held.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        let text_point = held
            .or(hovered_point)
            .and_then(|index| points.get(index))
            .map(|point| [point.x, point.y]);
        if let Some(pos) = text_point.or_else(|| response.hover_pos().map(to_curve)) {
            let [x, y] = pos;
            readout(ui, rect, format!("{:.0}  /  {:.0}", x * 100.0, y * 100.0));
        }
        change
    }
}

fn interact(
    ui: &Ui,
    response: &Response,
    id: Id,
    points: &mut Vec<CurvePoint>,
    samples: &[f32],
    rect: Rect,
    to_curve: impl Fn(Pos2) -> [f32; 2],
) -> Option<CurveChange> {
    let tolerance = [GRAB_RADIUS / rect.width(), GRAB_RADIUS / rect.height()];
    let pointer = response.interact_pointer_pos();

    if response.double_clicked()
        && let Some(pos) = pointer
    {
        let [x, y] = to_curve(pos);
        ui.data_mut(|data| data.remove_temp::<PointDrag>(id));
        if let Some(index) = nearest_point(points, x, y, tolerance)
            && remove_point(points, index)
        {
            return Some(CurveChange::Removed);
        }
        return None;
    }
    if response.secondary_clicked()
        && let Some(pos) = pointer
    {
        let [x, y] = to_curve(pos);
        if let Some(index) = nearest_point(points, x, y, tolerance)
            && remove_point(points, index)
        {
            return Some(CurveChange::Removed);
        }
    }

    if !response.is_pointer_button_down_on() {
        ui.data_mut(|data| data.remove_temp::<PointDrag>(id));
        return None;
    }
    let pos = pointer?;
    let [x, y] = to_curve(pos);
    let fine = ui.input(|input| input.modifiers.alt);

    let mut drag = match ui.data(|data| data.get_temp::<PointDrag>(id)) {
        Some(drag) => drag,
        None => {
            let index = nearest_point(points, x, y, tolerance).or_else(|| {
                let sample = (x * (samples.len() - 1) as f32).round() as usize;
                let on_curve = samples.get(sample).copied().unwrap_or(x);
                let near_line = (on_curve - y).abs() <= tolerance[1] * 2.0;
                insert_point(points, x, if near_line { on_curve } else { y })
            })?;
            PointDrag {
                index,
                exact: [points[index].x, points[index].y],
            }
        }
    };
    if fine {
        let delta = response.drag_delta();
        drag.exact[0] += delta.x / rect.width() * FINE_DRAG;
        drag.exact[1] -= delta.y / rect.height() * FINE_DRAG;
    } else {
        drag.exact = [x, y];
    }
    ui.data_mut(|data| data.insert_temp(id, drag));
    let before = points[drag.index];
    move_point(points, drag.index, drag.exact[0], drag.exact[1]);
    (before != points[drag.index] || drag.exact == [x, y]).then_some(CurveChange::Point)
}

/// The parametric view: the curve, a shaded band for the hovered region, split handles below.
pub fn parametric_plot(
    ui: &mut Ui,
    samples: &[f32],
    histogram: Option<&Histogram>,
    splits: &mut [f32; 3],
    region: Option<(f32, f32)>,
) -> bool {
    let rect = plot_rect(ui, PLOT_HEIGHT - 32.0);
    paint_plot_background(ui, rect);
    if let Some(histogram) = histogram {
        paint_histogram(ui, rect, histogram);
    }
    if let Some((low, high)) = region {
        let band = Rect::from_min_max(
            pos2(rect.left() + low * rect.width(), rect.top()),
            pos2(rect.left() + high * rect.width(), rect.bottom()),
        );
        ui.painter().rect_filled(band, 0.0, Color32::from_white_alpha(10));
    }
    for split in *splits {
        let x = rect.left() + split / 100.0 * rect.width();
        ui.painter()
            .vline(x, rect.y_range(), (1.0, Color32::from_gray(0x44)));
    }
    paint_curve_line(ui, rect, samples, theme::HANDLE);
    split_bar(ui, rect, splits)
}

fn split_bar(ui: &mut Ui, plot: Rect, splits: &mut [f32; 3]) -> bool {
    let (bar, _) = ui.allocate_exact_size(vec2(plot.width(), SPLIT_BAR_HEIGHT), Sense::hover());
    let mut changed = false;
    for index in 0..splits.len() {
        let x = bar.left() + splits[index] / 100.0 * bar.width();
        let area = Rect::from_center_size(pos2(x, bar.center().y), vec2(14.0, SPLIT_BAR_HEIGHT));
        let name = ["Shadow split", "Midtone split", "Highlight split"][index];
        let id = ui.auto_id_with(name);
        let response = ui
            .interact(area, id, Sense::drag())
            .on_hover_cursor(CursorIcon::ResizeHorizontal);
        response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), f64::from(splits[index]), name));
        if response.dragged() {
            let moved = response.drag_delta().x / bar.width() * 100.0;
            let low = if index == 0 {
                10.0
            } else {
                splits[index - 1] + MIN_SPLIT_GAP
            };
            let high = if index == 2 {
                90.0
            } else {
                splits[index + 1] - MIN_SPLIT_GAP
            };
            let value = (splits[index] + moved).clamp(low, high);
            changed |= value != splits[index];
            splits[index] = value;
        }
        let color = if response.hovered() || response.dragged() {
            theme::HANDLE_HOVER
        } else {
            theme::TEXT_FAINT
        };
        let top = bar.top() + 3.0;
        paint::triangle(
            ui.painter(),
            [pos2(x, top), pos2(x - 5.0, top + 8.0), pos2(x + 5.0, top + 8.0)],
            color,
        );
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(pairs: &[(f32, f32)]) -> Vec<CurvePoint> {
        pairs.iter().map(|&(x, y)| CurvePoint::new(x, y)).collect()
    }

    #[test]
    fn an_empty_curve_edits_as_the_diagonal() {
        assert_eq!(
            editable_points(&Curve::default()),
            points(&[(0.0, 0.0), (1.0, 1.0)])
        );
    }

    #[test]
    fn inserted_points_stay_sorted() {
        let mut curve = points(&[(0.0, 0.0), (1.0, 1.0)]);
        assert_eq!(insert_point(&mut curve, 0.5, 0.6), Some(1));
        assert_eq!(insert_point(&mut curve, 0.25, 0.2), Some(1));
        let xs: Vec<f32> = curve.iter().map(|point| point.x).collect();
        assert_eq!(xs, [0.0, 0.25, 0.5, 1.0]);
    }

    #[test]
    fn a_point_cant_be_stacked_on_another() {
        let mut curve = points(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        assert_eq!(insert_point(&mut curve, 0.505, 0.6), None);
    }

    #[test]
    fn points_stay_between_their_neighbours() {
        let mut curve = points(&[(0.0, 0.0), (0.4, 0.4), (0.6, 0.6), (1.0, 1.0)]);
        move_point(&mut curve, 1, 0.9, 2.0);
        assert!(curve[1].x < curve[2].x);
        assert_eq!(curve[1].y, 1.0);
    }

    #[test]
    fn endpoints_cant_be_removed() {
        let mut curve = points(&[(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)]);
        assert!(!remove_point(&mut curve, 0));
        assert!(remove_point(&mut curve, 1));
        assert_eq!(curve.len(), 2);
    }

    #[test]
    fn a_curve_back_on_the_diagonal_stores_as_identity() {
        let mut curve = Curve(points(&[(0.0, 0.1), (1.0, 1.0)]));
        store_points(&mut curve, points(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]));
        assert_eq!(curve, Curve::default());
    }

    #[test]
    fn grabbing_prefers_the_nearest_point() {
        let curve = points(&[(0.0, 0.0), (0.5, 0.5), (0.52, 0.5), (1.0, 1.0)]);
        assert_eq!(nearest_point(&curve, 0.515, 0.5, [0.05, 0.05]), Some(2));
        assert_eq!(nearest_point(&curve, 0.8, 0.2, [0.05, 0.05]), None);
    }
}
