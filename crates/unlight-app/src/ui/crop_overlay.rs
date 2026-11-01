//! The crop overlay: shade, rule of thirds and handles, plus the pointer handling that drives
//! `CropGesture`.

use crate::{
    state::{
        AppState,
        crop::{CropGesture, CropGrip, CropRect, Edges, Framing, corners_inside, fit_inside, pick_grip},
        edit::Step,
        view::{Orientation, ViewPlan},
    },
    ui::theme,
};
use eframe::egui::{Color32, CursorIcon, Painter, Pos2, Rect, Response, Stroke, Ui, pos2};
use unlight_engine::Optics;

const GRAB_DISTANCE: f32 = 10.0;
const HANDLE_LENGTH: f32 = 14.0;
const HANDLE_WEIGHT: f32 = 3.0;

/// Maps between the photo's 0..1 square and screen points for one frame.
#[derive(Clone, Copy)]
pub struct PhotoSpace {
    pub image: Rect,
    pub plan: ViewPlan,
}

impl PhotoSpace {
    pub fn to_screen(&self, point: [f32; 2]) -> Pos2 {
        let [rx, ry, rw, rh] = self.plan.region;
        pos2(
            self.image.left() + (point[0] - rx) / rw.max(1e-6) * self.image.width(),
            self.image.top() + (point[1] - ry) / rh.max(1e-6) * self.image.height(),
        )
    }

    pub fn to_photo(&self, pos: Pos2) -> [f32; 2] {
        let [rx, ry, rw, rh] = self.plan.region;
        [
            rx + (pos.x - self.image.left()) / self.image.width().max(1.0) * rw,
            ry + (pos.y - self.image.top()) / self.image.height().max(1.0) * rh,
        ]
    }
}

fn cursor_for(grip: CropGrip) -> CursorIcon {
    match grip {
        CropGrip::Move => CursorIcon::Move,
        CropGrip::Rotate => CursorIcon::Alias,
        CropGrip::Resize(edges) if edges.is_corner() => {
            if edges.left == edges.top {
                CursorIcon::ResizeNwSe
            } else {
                CursorIcon::ResizeNeSw
            }
        }
        CropGrip::Resize(edges) if edges.left || edges.right => CursorIcon::ResizeHorizontal,
        CropGrip::Resize(_) => CursorIcon::ResizeVertical,
    }
}

pub struct CropInput<'a> {
    pub space: PhotoSpace,
    pub orientation: Orientation,
    pub sensor_aspect: f32,
    pub optics: &'a Optics,
}

/// Handles the pointer on the overlay. Returns the rectangle to draw this frame.
pub fn interact(
    ui: &Ui,
    response: &Response,
    state: &mut AppState,
    gesture: &mut Option<CropGesture>,
    input: &CropInput<'_>,
) -> Option<CropRect> {
    let CropInput {
        space,
        orientation,
        sensor_aspect,
        optics,
    } = *input;
    let rect = state.crop_rect(orientation)?;
    let photo_aspect = if orientation.swaps_axes() {
        1.0 / sensor_aspect
    } else {
        sensor_aspect
    };
    let tolerance = [
        GRAB_DISTANCE / space.image.width().max(1.0),
        GRAB_DISTANCE / space.image.height().max(1.0),
    ];

    if let Some(pos) = response.hover_pos()
        && gesture.is_none()
    {
        ui.ctx()
            .set_cursor_icon(cursor_for(pick_grip(&rect, space.to_photo(pos), tolerance)));
    }

    if response.drag_started()
        && let Some(pos) = ui.input(|input| input.pointer.press_origin())
    {
        let point = space.to_photo(pos);
        let angle = state.edit().map_or(0.0, |edit| edit.recipe.crop.angle);
        let grip = pick_grip(&rect, point, tolerance);
        *gesture = Some(CropGesture::new(grip, rect, angle, point, photo_aspect));
    }
    if let Some(active) = *gesture {
        ui.ctx().set_cursor_icon(cursor_for(active.grip));
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let tool = state.crop_tool;
            let lock = tool.lock.ratio(photo_aspect, tool.portrait);
            let (moved, angle) = active.update(space.to_photo(pos), lock, orientation.mirrors());
            if let Some(edit) = state.edit_mut() {
                let mut crop = edit.recipe.crop;
                moved.apply_to(&mut crop, orientation);
                crop.angle = angle;
                let framing = Framing {
                    transform: &edit.recipe.transform,
                    lens: &edit.recipe.lens,
                    optics,
                    aspect: sensor_aspect,
                };
                let settled = if active.grip == CropGrip::Move {
                    corners_inside(&crop, &framing).then_some(crop)
                } else {
                    Some(fit_inside(&crop, &framing))
                };
                if let Some(settled) = settled {
                    edit.recipe.crop = settled;
                    let label = if active.grip == CropGrip::Rotate {
                        "Straighten"
                    } else {
                        "Crop"
                    };
                    edit.note(label, "", Step::New);
                }
            }
        }
        if response.drag_stopped() {
            *gesture = None;
        }
    }
    state.crop_rect(orientation)
}

fn paint_shade(painter: &Painter, image: Rect, crop: Rect) {
    let shade = Color32::from_black_alpha(150);
    let top = Rect::from_min_max(image.min, pos2(image.right(), crop.top()));
    let bottom = Rect::from_min_max(pos2(image.left(), crop.bottom()), image.max);
    let left = Rect::from_min_max(pos2(image.left(), crop.top()), pos2(crop.left(), crop.bottom()));
    let right = Rect::from_min_max(pos2(crop.right(), crop.top()), pos2(image.right(), crop.bottom()));
    for band in [top, bottom, left, right] {
        painter.rect_filled(band, 0.0, shade);
    }
}

fn paint_thirds(painter: &Painter, crop: Rect) {
    let line = Stroke::new(1.0, Color32::from_white_alpha(70));
    for third in [1.0 / 3.0, 2.0 / 3.0] {
        painter.vline(crop.left() + crop.width() * third, crop.y_range(), line);
        painter.hline(crop.x_range(), crop.top() + crop.height() * third, line);
    }
}

fn paint_handle(painter: &Painter, crop: Rect, handle: Edges) {
    let x = if handle.left {
        crop.left()
    } else if handle.right {
        crop.right()
    } else {
        crop.center().x
    };
    let y = if handle.top {
        crop.top()
    } else if handle.bottom {
        crop.bottom()
    } else {
        crop.center().y
    };
    let color = theme::HANDLE_HOVER;
    let inward_x = if handle.left { 1.0 } else { -1.0 };
    let inward_y = if handle.top { 1.0 } else { -1.0 };
    let horizontal = |from: f32, to: f32, y: f32| {
        painter.rect_filled(
            Rect::from_two_pos(
                pos2(from, y - HANDLE_WEIGHT * 0.5),
                pos2(to, y + HANDLE_WEIGHT * 0.5),
            ),
            0.0,
            color,
        );
    };
    let vertical = |from: f32, to: f32, x: f32| {
        painter.rect_filled(
            Rect::from_two_pos(
                pos2(x - HANDLE_WEIGHT * 0.5, from),
                pos2(x + HANDLE_WEIGHT * 0.5, to),
            ),
            0.0,
            color,
        );
    };
    if handle.is_corner() {
        horizontal(x, x + inward_x * HANDLE_LENGTH, y);
        vertical(y, y + inward_y * HANDLE_LENGTH, x);
    } else if handle.top || handle.bottom {
        horizontal(x - HANDLE_LENGTH * 0.5, x + HANDLE_LENGTH * 0.5, y);
    } else {
        vertical(y - HANDLE_LENGTH * 0.5, y + HANDLE_LENGTH * 0.5, x);
    }
}

pub fn paint(ui: &Ui, space: &PhotoSpace, rect: &CropRect, show_grid: bool) {
    let painter = ui.painter_at(space.image.expand(HANDLE_WEIGHT));
    let crop = Rect::from_min_max(
        space.to_screen([rect.left, rect.top]),
        space.to_screen([rect.right, rect.bottom]),
    );
    paint_shade(&painter, space.image, crop);
    if show_grid {
        paint_thirds(&painter, crop);
    }
    painter.rect_stroke(
        crop,
        0.0,
        Stroke::new(1.0, Color32::from_white_alpha(200)),
        eframe::egui::StrokeKind::Middle,
    );
    for handle in Edges::ALL_HANDLES {
        paint_handle(&painter, crop, handle);
    }
}
