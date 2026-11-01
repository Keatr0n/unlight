//! The Masks, Healing and Red Eye tools on the photo: gradient handles, the brush cursor and
//! the spot circles, plus the pointer handling behind them.

use crate::{
    darkroom::LivePhoto,
    state::{AppState, Tool, edit::Step, guides::view_to_source, local::dabs_between, view::Orientation},
    ui::{crop_overlay::PhotoSpace, theme},
};
use eframe::egui::{Color32, CursorIcon, Painter, Pos2, Response, Stroke, Ui};
use std::f32::consts::TAU;
use unlight_engine::{EngineError, Geometry};
use unlight_recipe::{HealSpot, MaskShape, Recipe, RedEye};

const GRAB_DISTANCE: f32 = 10.0;
const HANDLE_RADIUS: f32 = 5.0;
const ELLIPSE_POINTS: usize = 64;
/// How far past an ellipse's edge its rotation handle sits, as a share of the radius.
const ROTATE_REACH: f32 = 1.25;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Start,
    End,
    Whole { grabbed: [f32; 2] },
    Center,
    EdgeX,
    EdgeY,
    Rotate,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LocalGesture {
    /// Painting. `kept` is how many dabs came before this stroke, `start` where it began.
    Stroke {
        component: usize,
        kept: usize,
        start: [f32; 2],
        last: [f32; 2],
        straight: bool,
    },
    Mask {
        component: usize,
        handle: Handle,
    },
    SpotTarget(usize),
    SpotSource(usize),
    Eye(usize),
}

pub struct LocalInput {
    pub space: PhotoSpace,
    pub orientation: Orientation,
    pub sensor_aspect: f32,
}

/// Between the screen and the source, through the geometry on screen.
struct Mapping<'a> {
    geometry: Geometry,
    input: &'a LocalInput,
}

impl Mapping<'_> {
    fn to_source(&self, pos: Pos2) -> [f32; 2] {
        let input = self.input;
        view_to_source(
            &self.geometry,
            input.space.to_photo(pos),
            input.orientation,
            input.sensor_aspect,
        )
    }

    fn to_screen(&self, source: [f32; 2]) -> Pos2 {
        let input = self.input;
        let view = self.geometry.view_from_source(source, input.sensor_aspect);
        input.space.to_screen(input.orientation.to_display(view))
    }

    /// What one normalised unit along each axis is as a share of the long edge.
    fn axes(&self) -> [f32; 2] {
        let aspect = self.input.sensor_aspect;
        if aspect >= 1.0 {
            [1.0, 1.0 / aspect]
        } else {
            [aspect, 1.0]
        }
    }

    /// A radius given as a share of the long edge, in screen points.
    fn screen_radius(&self, center: [f32; 2], radius: f32) -> f32 {
        let across = [center[0] + radius / self.axes()[0], center[1]];
        self.to_screen(center).distance(self.to_screen(across))
    }

    fn in_long_edges(&self, from: [f32; 2], to: [f32; 2]) -> [f32; 2] {
        let axes = self.axes();
        [(to[0] - from[0]) * axes[0], (to[1] - from[1]) * axes[1]]
    }

    fn offset_in_long_edges(&self, center: [f32; 2], offset: [f32; 2]) -> [f32; 2] {
        let axes = self.axes();
        [center[0] + offset[0] / axes[0], center[1] + offset[1] / axes[1]]
    }

    /// A point around an ellipse, at `angle` radians from its own x axis.
    fn on_ellipse(&self, ellipse: &Ellipse, angle: f32, scale: f32) -> [f32; 2] {
        let (x, y) = (
            ellipse.radii[0] * angle.cos() * scale,
            ellipse.radii[1] * angle.sin() * scale,
        );
        let (sine, cosine) = ellipse.angle.to_radians().sin_cos();
        self.offset_in_long_edges(ellipse.center, [cosine * x - sine * y, sine * x + cosine * y])
    }
}

#[derive(Clone, Copy)]
struct Ellipse {
    center: [f32; 2],
    radii: [f32; 2],
    angle: f32,
}

fn handle_ring(painter: &Painter, center: Pos2, selected: bool) {
    let fill = if selected {
        theme::ACCENT
    } else {
        theme::PANEL_DEEP
    };
    painter.circle(center, HANDLE_RADIUS, fill, Stroke::new(1.5, Color32::WHITE));
}

fn outlined_circle(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    painter.circle_stroke(center, radius, Stroke::new(3.0, Color32::from_black_alpha(110)));
    painter.circle_stroke(center, radius, Stroke::new(1.25, color));
}

fn outlined_line(painter: &Painter, points: [Pos2; 2]) {
    painter.line_segment(points, Stroke::new(3.0, Color32::from_black_alpha(110)));
    painter.line_segment(points, Stroke::new(1.25, Color32::WHITE));
}

fn paint_linear(painter: &Painter, mapping: &Mapping<'_>, start: [f32; 2], end: [f32; 2]) {
    let (from, to) = (mapping.to_screen(start), mapping.to_screen(end));
    let along = to - from;
    let across = along.rot90().normalized() * along.length().max(40.0) * 0.6;
    outlined_line(painter, [from, to]);
    outlined_line(painter, [from - across, from + across]);
    outlined_line(painter, [to - across, to + across]);
    handle_ring(painter, from, true);
    handle_ring(painter, to, false);
}

fn paint_radial(painter: &Painter, mapping: &Mapping<'_>, ellipse: &Ellipse) {
    let outline: Vec<Pos2> = (0..=ELLIPSE_POINTS)
        .map(|step| {
            mapping.to_screen(mapping.on_ellipse(ellipse, step as f32 / ELLIPSE_POINTS as f32 * TAU, 1.0))
        })
        .collect();
    let shadow = Stroke::new(3.0, Color32::from_black_alpha(110));
    painter.add(eframe::egui::Shape::line(outline.clone(), shadow));
    painter.add(eframe::egui::Shape::line(
        outline,
        Stroke::new(1.25, Color32::WHITE),
    ));
    handle_ring(painter, mapping.to_screen(ellipse.center), true);
    handle_ring(
        painter,
        mapping.to_screen(mapping.on_ellipse(ellipse, 0.0, 1.0)),
        false,
    );
    handle_ring(
        painter,
        mapping.to_screen(mapping.on_ellipse(ellipse, TAU * 0.25, 1.0)),
        false,
    );
    let edge = mapping.to_screen(mapping.on_ellipse(ellipse, 0.0, 1.0));
    let turn = mapping.to_screen(mapping.on_ellipse(ellipse, 0.0, ROTATE_REACH));
    outlined_line(painter, [edge, turn]);
    handle_ring(painter, turn, false);
}

fn pick_linear(mapping: &Mapping<'_>, start: [f32; 2], end: [f32; 2], pos: Pos2) -> Option<Handle> {
    let near = |point: Pos2| point.distance(pos) <= GRAB_DISTANCE;
    let (from, to) = (mapping.to_screen(start), mapping.to_screen(end));
    if near(from) {
        Some(Handle::Start)
    } else if near(to) {
        Some(Handle::End)
    } else {
        let along = to - from;
        let t = ((pos - from).dot(along) / along.length_sq().max(1.0)).clamp(0.0, 1.0);
        ((from + along * t).distance(pos) <= GRAB_DISTANCE).then(|| Handle::Whole {
            grabbed: mapping.to_source(pos),
        })
    }
}

fn pick_radial(mapping: &Mapping<'_>, ellipse: &Ellipse, pos: Pos2) -> Option<Handle> {
    let near = |source: [f32; 2]| mapping.to_screen(source).distance(pos) <= GRAB_DISTANCE;
    if near(ellipse.center) {
        Some(Handle::Center)
    } else if near(mapping.on_ellipse(ellipse, 0.0, ROTATE_REACH)) {
        Some(Handle::Rotate)
    } else if near(mapping.on_ellipse(ellipse, 0.0, 1.0)) {
        Some(Handle::EdgeX)
    } else if near(mapping.on_ellipse(ellipse, TAU * 0.25, 1.0)) {
        Some(Handle::EdgeY)
    } else {
        None
    }
}

fn pick_handle(recipe: &Recipe, index: usize, mapping: &Mapping<'_>, pos: Pos2) -> Option<(usize, Handle)> {
    let components = &recipe.masks.get(index)?.components;
    components
        .iter()
        .enumerate()
        .rev()
        .find_map(|(position, component)| {
            let handle = match component.shape {
                MaskShape::Linear { start, end } => pick_linear(mapping, start, end, pos),
                MaskShape::Radial {
                    center, radii, angle, ..
                } => pick_radial(mapping, &Ellipse { center, radii, angle }, pos),
                _ => None,
            };
            handle.map(|handle| (position, handle))
        })
}

fn move_linear(
    handle: Handle,
    start: &mut [f32; 2],
    end: &mut [f32; 2],
    point: [f32; 2],
    gesture: &mut Handle,
) {
    match handle {
        Handle::Start => *start = point,
        Handle::End => *end = point,
        Handle::Whole { grabbed } => {
            let shift = [point[0] - grabbed[0], point[1] - grabbed[1]];
            for end_point in [start, end] {
                *end_point = [end_point[0] + shift[0], end_point[1] + shift[1]];
            }
            *gesture = Handle::Whole { grabbed: point };
        }
        _ => {}
    }
}

fn move_radial(handle: Handle, ellipse: &mut Ellipse, point: [f32; 2], mapping: &Mapping<'_>) {
    let [x, y] = mapping.in_long_edges(ellipse.center, point);
    let (sine, cosine) = ellipse.angle.to_radians().sin_cos();
    match handle {
        Handle::Center => ellipse.center = point,
        Handle::EdgeX => ellipse.radii[0] = (cosine * x + sine * y).abs().max(1e-3),
        Handle::EdgeY => ellipse.radii[1] = (cosine * y - sine * x).abs().max(1e-3),
        Handle::Rotate => ellipse.angle = y.atan2(x).to_degrees(),
        _ => {}
    }
}

fn apply_handle(state: &mut AppState, gesture: &mut LocalGesture, mapping: &Mapping<'_>, point: [f32; 2]) {
    let LocalGesture::Mask { component, handle } = gesture else {
        return;
    };
    let Some(index) = state.current_mask_index() else {
        return;
    };
    let Some(edit) = state.edit_mut() else {
        return;
    };
    let Some(part) = edit.recipe.masks[index].components.get_mut(*component) else {
        return;
    };
    match &mut part.shape {
        MaskShape::Linear { start, end } => {
            let current = *handle;
            move_linear(current, start, end, point, handle);
        }
        MaskShape::Radial {
            center, radii, angle, ..
        } => {
            let mut ellipse = Ellipse {
                center: *center,
                radii: *radii,
                angle: *angle,
            };
            move_radial(*handle, &mut ellipse, point, mapping);
            (*center, *radii, *angle) = (ellipse.center, ellipse.radii, ellipse.angle);
        }
        _ => {}
    }
    edit.note("Move Mask", "", Step::New);
}

/// The last brush component of the selected mask, which is where a stroke goes.
fn brush_component(recipe: &Recipe, index: usize) -> Option<(usize, usize)> {
    let components = &recipe.masks.get(index)?.components;
    components
        .iter()
        .enumerate()
        .rev()
        .find_map(|(position, component)| match &component.shape {
            MaskShape::Brush { dabs } => Some((position, dabs.len())),
            _ => None,
        })
}

fn extend_stroke(
    state: &mut AppState,
    gesture: &mut LocalGesture,
    mapping: &Mapping<'_>,
    point: [f32; 2],
    erasing: bool,
) {
    let LocalGesture::Stroke {
        component,
        kept,
        start,
        last,
        straight,
    } = gesture
    else {
        return;
    };
    let brush = state.local.brush;
    let Some(index) = state.current_mask_index() else {
        return;
    };
    let Some(edit) = state.edit_mut() else {
        return;
    };
    let Some(part) = edit.recipe.masks[index].components.get_mut(*component) else {
        return;
    };
    let MaskShape::Brush { dabs } = &mut part.shape else {
        return;
    };
    let axes = mapping.axes();
    if *straight {
        dabs.truncate(*kept);
        dabs.push(brush.dab(*start, erasing));
        dabs.extend(
            dabs_between(*start, point, axes, brush.radius)
                .into_iter()
                .map(|at| brush.dab(at, erasing)),
        );
    } else {
        let fresh = dabs_between(*last, point, axes, brush.radius);
        if let Some(reached) = fresh.last() {
            *last = *reached;
        }
        dabs.extend(fresh.into_iter().map(|at| brush.dab(at, erasing)));
    }
    edit.note("Brush", "", Step::New);
}

fn begin_stroke(
    state: &mut AppState,
    gesture: &mut Option<LocalGesture>,
    point: [f32; 2],
    straight: bool,
    erasing: bool,
) {
    let brush = state.local.brush;
    let Some(index) = state.current_mask_index() else {
        return;
    };
    let Some(edit) = state.edit_mut() else {
        return;
    };
    let Some((component, kept)) = brush_component(&edit.recipe, index) else {
        return;
    };
    if let MaskShape::Brush { dabs } = &mut edit.recipe.masks[index].components[component].shape {
        dabs.push(brush.dab(point, erasing));
    }
    edit.note("Brush", "", Step::New);
    *gesture = Some(LocalGesture::Stroke {
        component,
        kept,
        start: point,
        last: point,
        straight,
    });
}

fn pick_colour(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    live: &mut LivePhoto,
    mapping: &Mapping<'_>,
) {
    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
    if !response.clicked() {
        return;
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return;
    };
    let Some(recipe) = state.edit().map(|edit| edit.effective()) else {
        return;
    };
    match live.session.scene_color(&recipe, mapping.to_source(pos)) {
        Ok(colour) => state.add_colour_sample(colour),
        Err(error) => report(state, &error),
    }
}

fn report(state: &mut AppState, error: &EngineError) {
    match error {
        EngineError::DeviceLost(_) => state.device_lost = true,
        other => state.report_error(format!("Couldn't read the photo: {other}")),
    }
}

fn masks_tool(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    gesture: &mut Option<LocalGesture>,
    live: &mut LivePhoto,
    mapping: &Mapping<'_>,
) {
    if state.local.picking_colour {
        pick_colour(ui, response, state, live, mapping);
    }
    let Some(index) = state.current_mask_index() else {
        return;
    };
    let hover = response.hover_pos();
    if response.drag_started()
        && !state.local.picking_colour
        && let Some(pos) = ui.input(|input| input.pointer.press_origin())
        && let Some(recipe) = state.edit().map(|edit| edit.recipe.clone())
    {
        let modifiers = ui.input(|input| input.modifiers);
        let erasing = state.local.brush.erase || modifiers.alt;
        match pick_handle(&recipe, index, mapping, pos) {
            Some((component, handle)) => *gesture = Some(LocalGesture::Mask { component, handle }),
            None => begin_stroke(state, gesture, mapping.to_source(pos), modifiers.shift, erasing),
        }
    }
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(active) = gesture
    {
        let point = mapping.to_source(pos);
        let erasing = state.local.brush.erase || ui.input(|input| input.modifiers.alt);
        match active {
            LocalGesture::Stroke { .. } => extend_stroke(state, active, mapping, point, erasing),
            LocalGesture::Mask { .. } => apply_handle(state, active, mapping, point),
            _ => {}
        }
    }
    let Some(recipe) = state.edit().map(|edit| &edit.recipe) else {
        return;
    };
    let painter = ui.painter_at(mapping.input.space.image);
    let paints = brush_component(recipe, index).is_some();
    if let Some(mask) = recipe.masks.get(index) {
        for component in &mask.components {
            match component.shape {
                MaskShape::Linear { start, end } => paint_linear(&painter, mapping, start, end),
                MaskShape::Radial {
                    center, radii, angle, ..
                } => {
                    paint_radial(&painter, mapping, &Ellipse { center, radii, angle });
                }
                _ => {}
            }
        }
    }
    if let (true, Some(pos)) = (paints, hover) {
        brush_cursor(ui, &painter, state, mapping, pos);
    }
}

fn brush_cursor(ui: &Ui, painter: &Painter, state: &AppState, mapping: &Mapping<'_>, pos: Pos2) {
    ui.ctx().set_cursor_icon(CursorIcon::None);
    let brush = state.local.brush;
    let radius = mapping.screen_radius(mapping.to_source(pos), brush.radius);
    outlined_circle(painter, pos, radius, Color32::WHITE);
    outlined_circle(
        painter,
        pos,
        radius * (1.0 - brush.feather),
        Color32::from_white_alpha(120),
    );
}

fn spot_at(spots: &[HealSpot], mapping: &Mapping<'_>, pos: Pos2) -> Option<LocalGesture> {
    spots.iter().enumerate().rev().find_map(|(index, spot)| {
        let reach = |center: [f32; 2]| mapping.screen_radius(center, spot.radius).max(GRAB_DISTANCE);
        if mapping.to_screen(spot.target).distance(pos) <= reach(spot.target) {
            Some(LocalGesture::SpotTarget(index))
        } else if mapping.to_screen(spot.source).distance(pos) <= reach(spot.source) {
            Some(LocalGesture::SpotSource(index))
        } else {
            None
        }
    })
}

fn paint_spots(painter: &Painter, spots: &[HealSpot], selected: Option<usize>, mapping: &Mapping<'_>) {
    for (index, spot) in spots.iter().enumerate() {
        let target = mapping.to_screen(spot.target);
        let radius = mapping.screen_radius(spot.target, spot.radius);
        if selected == Some(index) {
            let source = mapping.to_screen(spot.source);
            outlined_line(painter, [source, target]);
            outlined_circle(painter, source, radius, Color32::from_white_alpha(170));
            outlined_circle(painter, target, radius, theme::ACCENT);
        } else {
            outlined_circle(painter, target, radius, Color32::WHITE);
        }
    }
}

fn add_spot(state: &mut AppState, live: &mut LivePhoto, target: [f32; 2]) {
    let radius = state.local.heal.radius;
    let Some(recipe) = state.edit().map(|edit| edit.effective()) else {
        return;
    };
    match live.session.find_heal_source(&recipe, target, radius) {
        Ok(source) => state.add_heal_spot(target, source),
        Err(error) => report(state, &error),
    }
}

fn heal_tool(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    gesture: &mut Option<LocalGesture>,
    live: &mut LivePhoto,
    mapping: &Mapping<'_>,
) {
    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
    let spots = state
        .edit()
        .map(|edit| edit.recipe.healing.clone())
        .unwrap_or_default();
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(LocalGesture::SpotTarget(index) | LocalGesture::SpotSource(index)) =
            spot_at(&spots, mapping, pos)
    {
        state.delete_spot(index);
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        match spot_at(&spots, mapping, pos) {
            Some(LocalGesture::SpotTarget(index) | LocalGesture::SpotSource(index)) => {
                state.local.spot = Some(index)
            }
            _ => add_spot(state, live, mapping.to_source(pos)),
        }
    }
    if response.drag_started()
        && let Some(pos) = ui.input(|input| input.pointer.press_origin())
    {
        *gesture = spot_at(&spots, mapping, pos);
        if let Some(LocalGesture::SpotTarget(index) | LocalGesture::SpotSource(index)) = *gesture {
            state.local.spot = Some(index);
        }
    }
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(edit) = state.edit_mut()
    {
        let point = mapping.to_source(pos);
        match *gesture {
            Some(LocalGesture::SpotTarget(index)) => {
                if let Some(spot) = edit.recipe.healing.get_mut(index) {
                    spot.target = point;
                }
                edit.note("Move Spot", "", Step::New);
            }
            Some(LocalGesture::SpotSource(index)) => {
                if let Some(spot) = edit.recipe.healing.get_mut(index) {
                    spot.source = point;
                }
                edit.note("Move Spot Source", "", Step::New);
            }
            _ => {}
        }
    }
    let selected = state.local.spot;
    if let Some(edit) = state.edit() {
        let painter = ui.painter_at(mapping.input.space.image);
        paint_spots(&painter, &edit.recipe.healing, selected, mapping);
    }
}

fn eye_at(eyes: &[RedEye], mapping: &Mapping<'_>, pos: Pos2) -> Option<usize> {
    eyes.iter().rposition(|eye| {
        let reach = mapping.screen_radius(eye.center, eye.radius).max(GRAB_DISTANCE);
        mapping.to_screen(eye.center).distance(pos) <= reach
    })
}

fn red_eye_tool(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    gesture: &mut Option<LocalGesture>,
    mapping: &Mapping<'_>,
) {
    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
    let eyes = state
        .edit()
        .map(|edit| edit.recipe.red_eye.clone())
        .unwrap_or_default();
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(index) = eye_at(&eyes, mapping, pos)
    {
        state.delete_eye(index);
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        match eye_at(&eyes, mapping, pos) {
            Some(index) => state.local.eye = Some(index),
            None => state.add_red_eye(mapping.to_source(pos)),
        }
    }
    if response.drag_started()
        && let Some(pos) = ui.input(|input| input.pointer.press_origin())
    {
        *gesture = eye_at(&eyes, mapping, pos).map(LocalGesture::Eye);
        state.local.eye = eye_at(&eyes, mapping, pos).or(state.local.eye);
    }
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(LocalGesture::Eye(index)) = *gesture
        && let Some(edit) = state.edit_mut()
        && let Some(eye) = edit.recipe.red_eye.get_mut(index)
    {
        eye.center = mapping.to_source(pos);
        edit.note("Move Red Eye", "", Step::New);
    }
    if let Some(edit) = state.edit() {
        let painter = ui.painter_at(mapping.input.space.image);
        for (index, eye) in edit.recipe.red_eye.iter().enumerate() {
            let color = if state.local.eye == Some(index) {
                theme::ACCENT
            } else {
                Color32::WHITE
            };
            let center = mapping.to_screen(eye.center);
            outlined_circle(
                &painter,
                center,
                mapping.screen_radius(eye.center, eye.radius),
                color,
            );
        }
    }
}

/// Handles the pointer for whichever local tool is open, then draws its handles.
pub fn show(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    gesture: &mut Option<LocalGesture>,
    live: &mut LivePhoto,
    input: &LocalInput,
) {
    let Some(edit) = state.edit() else {
        return;
    };
    let recipe = edit.effective();
    let geometry = Geometry::new(
        &recipe.crop,
        &recipe.transform,
        &recipe.lens,
        input.sensor_aspect,
        live.session.optics(),
    );
    let mapping = Mapping { geometry, input };
    match state.tool {
        Tool::Masks => masks_tool(ui, response, state, gesture, live, &mapping),
        Tool::Heal => heal_tool(ui, response, state, gesture, live, &mapping),
        Tool::RedEye => red_eye_tool(ui, response, state, gesture, &mapping),
        _ => {}
    }
    if response.drag_stopped() {
        *gesture = None;
    }
}
