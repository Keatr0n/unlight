//! Drawing Upright guides on the photo. A drag on empty ground adds a guide, a drag on an end
//! moves it, and a right-click removes one.

use crate::{
    state::{
        AppState,
        edit::Step,
        guides::{distance_to_segment, long_enough, source_to_view, view_to_source},
        view::Orientation,
    },
    ui::{crop_overlay::PhotoSpace, theme},
};
use eframe::egui::{Color32, CursorIcon, Pos2, Response, Stroke, Ui};
use unlight_engine::{Geometry, Optics};
use unlight_recipe::{Crop, GuideLine};

const GRAB_DISTANCE: f32 = 10.0;
const LINE_HIT: f32 = 8.0;
const HANDLE_RADIUS: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GuideGesture {
    /// A new guide, from where the drag began, in view coordinates.
    Drawing { start: [f32; 2] },
    /// An existing guide's end. `end` picks which.
    Moving { index: usize, end: bool },
}

pub struct GuideInput<'a> {
    pub space: PhotoSpace,
    pub orientation: Orientation,
    pub sensor_aspect: f32,
    pub optics: &'a Optics,
}

struct Mapping<'a> {
    geometry: Geometry,
    input: &'a GuideInput<'a>,
}

impl Mapping<'_> {
    fn to_source(&self, view: [f32; 2]) -> [f32; 2] {
        view_to_source(
            &self.geometry,
            view,
            self.input.orientation,
            self.input.sensor_aspect,
        )
    }

    fn to_screen(&self, source: [f32; 2]) -> Pos2 {
        let view = source_to_view(
            &self.geometry,
            source,
            self.input.orientation,
            self.input.sensor_aspect,
        );
        self.input.space.to_screen(view)
    }
}

fn pick_end(guides: &[GuideLine], pos: Pos2, mapping: &Mapping<'_>) -> Option<GuideGesture> {
    guides.iter().enumerate().find_map(|(index, guide)| {
        let near = |source: [f32; 2]| mapping.to_screen(source).distance(pos) <= GRAB_DISTANCE;
        if near(guide.start) {
            Some(GuideGesture::Moving { index, end: false })
        } else if near(guide.end) {
            Some(GuideGesture::Moving { index, end: true })
        } else {
            None
        }
    })
}

fn guide_at(guides: &[GuideLine], pos: Pos2, mapping: &Mapping<'_>) -> Option<usize> {
    guides.iter().position(|guide| {
        let start = mapping.to_screen(guide.start);
        let end = mapping.to_screen(guide.end);
        distance_to_segment([pos.x, pos.y], [start.x, start.y], [end.x, end.y]) <= LINE_HIT
    })
}

fn apply_move(state: &mut AppState, index: usize, end: bool, source: [f32; 2]) {
    if let Some(edit) = state.edit_mut()
        && let Some(guide) = edit.recipe.transform.guides.get_mut(index)
    {
        if end {
            guide.end = source;
        } else {
            guide.start = source;
        }
        edit.note("Move Guide", "", Step::New);
    }
}

fn finish_drawing(state: &mut AppState, start: [f32; 2], end: [f32; 2], mapping: &Mapping<'_>) {
    if long_enough(start, end) {
        state.add_guide(GuideLine {
            start: mapping.to_source(start),
            end: mapping.to_source(end),
        });
    }
}

fn paint(ui: &Ui, guides: &[GuideLine], drawing: Option<(Pos2, Pos2)>, mapping: &Mapping<'_>) {
    let painter = ui.painter_at(mapping.input.space.image);
    let shadow = Stroke::new(3.0, Color32::from_black_alpha(110));
    let line = Stroke::new(1.5, theme::ACCENT);
    let segment = |start: Pos2, end: Pos2| {
        painter.line_segment([start, end], shadow);
        painter.line_segment([start, end], line);
        for point in [start, end] {
            painter.circle(point, HANDLE_RADIUS, theme::PANEL_DEEP, line);
        }
    };
    for guide in guides {
        segment(mapping.to_screen(guide.start), mapping.to_screen(guide.end));
    }
    if let Some((start, end)) = drawing {
        segment(start, end);
    }
}

/// Handles the pointer, then draws. The geometry is the one on screen: no crop, the crop's
/// angle, and whatever Upright currently makes of the guides.
pub fn show(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    gesture: &mut Option<GuideGesture>,
    input: &GuideInput<'_>,
) {
    let Some(edit) = state.edit() else {
        return;
    };
    let recipe = edit.effective();
    let crop = Crop {
        angle: recipe.crop.angle,
        ..Crop::default()
    };
    let geometry = Geometry::new(
        &crop,
        &recipe.transform,
        &recipe.lens,
        input.sensor_aspect,
        input.optics,
    );
    let mapping = Mapping { geometry, input };
    let guides = recipe.transform.guides;
    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);

    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(index) = guide_at(&guides, pos, &mapping)
    {
        state.remove_guide(index);
    }
    if response.drag_started()
        && let Some(pos) = ui.input(|input| input.pointer.press_origin())
    {
        *gesture = Some(pick_end(&guides, pos, &mapping).unwrap_or(GuideGesture::Drawing {
            start: input.space.to_photo(pos),
        }));
    }
    let pointer = response.interact_pointer_pos();
    if let (Some(GuideGesture::Moving { index, end }), Some(pos), true) =
        (*gesture, pointer, response.dragged())
    {
        apply_move(state, index, end, mapping.to_source(input.space.to_photo(pos)));
    }
    let drawing = match (*gesture, pointer) {
        (Some(GuideGesture::Drawing { start }), Some(pos)) => Some((input.space.to_screen(start), pos)),
        _ => None,
    };
    if response.drag_stopped() {
        if let (Some(GuideGesture::Drawing { start }), Some(pos)) = (*gesture, pointer) {
            finish_drawing(state, start, input.space.to_photo(pos), &mapping);
        }
        *gesture = None;
    }
    let current = state
        .edit()
        .map(|edit| edit.recipe.transform.guides.clone())
        .unwrap_or_default();
    paint(ui, &current, drawing, &mapping);
}
