//! Crop tool geometry. Everything here works on a rectangle in display orientation, on a
//! 0..1 square that stretches to the photo; `Orientation` converts to and from the recipe.

use super::view::Orientation;
use unlight_engine::{Geometry, Optics};
use unlight_recipe::{Crop, LensCorrections, Transform};

pub const MIN_SIZE: f32 = 0.02;
pub const MAX_ANGLE: f32 = 45.0;
const INSIDE_TOLERANCE: f32 = 1e-4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl CropRect {
    pub const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    pub fn from_crop(crop: &Crop, orientation: Orientation) -> Self {
        let a = orientation.to_display([crop.left, crop.top]);
        let b = orientation.to_display([crop.right, crop.bottom]);
        Self {
            left: a[0].min(b[0]),
            top: a[1].min(b[1]),
            right: a[0].max(b[0]),
            bottom: a[1].max(b[1]),
        }
    }

    pub fn apply_to(&self, crop: &mut Crop, orientation: Orientation) {
        let a = orientation.to_sensor([self.left, self.top]);
        let b = orientation.to_sensor([self.right, self.bottom]);
        crop.left = a[0].min(b[0]);
        crop.top = a[1].min(b[1]);
        crop.right = a[0].max(b[0]);
        crop.bottom = a[1].max(b[1]);
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    pub fn center(&self) -> [f32; 2] {
        [(self.left + self.right) * 0.5, (self.top + self.bottom) * 0.5]
    }

    pub fn contains(&self, [x, y]: [f32; 2]) -> bool {
        (self.left..=self.right).contains(&x) && (self.top..=self.bottom).contains(&y)
    }

    fn scaled_about_center(&self, scale: f32) -> Self {
        let [cx, cy] = self.center();
        let (half_width, half_height) = (self.width() * 0.5 * scale, self.height() * 0.5 * scale);
        Self {
            left: cx - half_width,
            top: cy - half_height,
            right: cx + half_width,
            bottom: cy + half_height,
        }
    }
}

/// Which edges a drag moves. Two adjacent edges make a corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl Edges {
    const fn new(left: bool, top: bool, right: bool, bottom: bool) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const ALL_HANDLES: [Self; 8] = [
        Self::new(true, true, false, false),
        Self::new(false, true, false, false),
        Self::new(false, true, true, false),
        Self::new(false, false, true, false),
        Self::new(false, false, true, true),
        Self::new(false, false, false, true),
        Self::new(true, false, false, true),
        Self::new(true, false, false, false),
    ];

    pub fn is_corner(self) -> bool {
        (self.left || self.right) && (self.top || self.bottom)
    }

    /// The point of a rectangle this handle sits on.
    pub fn anchor(self, rect: &CropRect) -> [f32; 2] {
        let [cx, cy] = rect.center();
        let x = if self.left {
            rect.left
        } else if self.right {
            rect.right
        } else {
            cx
        };
        let y = if self.top {
            rect.top
        } else if self.bottom {
            rect.bottom
        } else {
            cy
        };
        [x, y]
    }
}

/// Moves a rectangle, stopping at the photo's edge.
pub fn move_rect(start: &CropRect, delta: [f32; 2]) -> CropRect {
    let dx = delta[0].clamp(-start.left, 1.0 - start.right);
    let dy = delta[1].clamp(-start.top, 1.0 - start.bottom);
    CropRect {
        left: start.left + dx,
        top: start.top + dy,
        right: start.right + dx,
        bottom: start.bottom + dy,
    }
}

/// Drags edges freely, keeping the rectangle on the photo and above the minimum size.
fn drag_free(start: &CropRect, edges: Edges, delta: [f32; 2]) -> CropRect {
    let mut rect = *start;
    if edges.left {
        rect.left = (start.left + delta[0]).clamp(0.0, start.right - MIN_SIZE);
    }
    if edges.right {
        rect.right = (start.right + delta[0]).clamp(start.left + MIN_SIZE, 1.0);
    }
    if edges.top {
        rect.top = (start.top + delta[1]).clamp(0.0, start.bottom - MIN_SIZE);
    }
    if edges.bottom {
        rect.bottom = (start.bottom + delta[1]).clamp(start.top + MIN_SIZE, 1.0);
    }
    rect
}

/// Drags edges with the ratio held. `ratio` is width over height in pixels, `photo_aspect`
/// the same for the whole photo, so a unit-square height is `width * photo_aspect / ratio`.
fn drag_locked(start: &CropRect, edges: Edges, delta: [f32; 2], ratio: f32, photo_aspect: f32) -> CropRect {
    let per_width = photo_aspect / ratio;
    let dragged = drag_free(start, edges, delta);
    let moves_x = edges.left || edges.right;
    let moves_y = edges.top || edges.bottom;

    let wanted_width = if moves_x && moves_y {
        dragged.width().max(dragged.height() / per_width)
    } else if moves_x {
        dragged.width()
    } else {
        dragged.height() / per_width
    };

    let [cx, cy] = start.center();
    let (x_anchor, x_room) = if edges.left {
        (start.right, start.right)
    } else if edges.right {
        (start.left, 1.0 - start.left)
    } else {
        (cx, cx.min(1.0 - cx) * 2.0)
    };
    let (y_anchor, y_room) = if edges.top {
        (start.bottom, start.bottom)
    } else if edges.bottom {
        (start.top, 1.0 - start.top)
    } else {
        (cy, cy.min(1.0 - cy) * 2.0)
    };

    let width = wanted_width.min(x_room).min(y_room / per_width).max(MIN_SIZE);
    let height = width * per_width;
    let (left, right) = if edges.left {
        (x_anchor - width, x_anchor)
    } else if edges.right {
        (x_anchor, x_anchor + width)
    } else {
        (x_anchor - width * 0.5, x_anchor + width * 0.5)
    };
    let (top, bottom) = if edges.top {
        (y_anchor - height, y_anchor)
    } else if edges.bottom {
        (y_anchor, y_anchor + height)
    } else {
        (y_anchor - height * 0.5, y_anchor + height * 0.5)
    };
    CropRect {
        left,
        top,
        right,
        bottom,
    }
}

pub fn drag_edges(
    start: &CropRect,
    edges: Edges,
    delta: [f32; 2],
    lock: Option<f32>,
    photo_aspect: f32,
) -> CropRect {
    match lock {
        Some(ratio) => drag_locked(start, edges, delta, ratio, photo_aspect),
        None => drag_free(start, edges, delta),
    }
}

/// The largest centred rectangle of a pixel ratio that fits inside `rect`.
pub fn fit_ratio(rect: &CropRect, ratio: f32, photo_aspect: f32) -> CropRect {
    let per_width = photo_aspect / ratio;
    let width = rect.width().min(rect.height() / per_width);
    let height = width * per_width;
    let [cx, cy] = rect.center();
    CropRect {
        left: cx - width * 0.5,
        top: cy - height * 0.5,
        right: cx + width * 0.5,
        bottom: cy + height * 0.5,
    }
}

/// What a crop's position on the source depends on, apart from the crop itself.
#[derive(Clone, Copy)]
pub struct Framing<'a> {
    pub transform: &'a Transform,
    pub lens: &'a LensCorrections,
    pub optics: &'a Optics,
    /// Width over height of the sensor image.
    pub aspect: f32,
}

impl Framing<'_> {
    fn geometry(&self, crop: &Crop) -> Geometry {
        Geometry::new(crop, self.transform, self.lens, self.aspect, self.optics)
    }
}

pub fn corners_inside(crop: &Crop, framing: &Framing<'_>) -> bool {
    let (geometry, aspect) = (framing.geometry(crop), framing.aspect);
    [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
        .into_iter()
        .all(|corner| {
            let [u, v] = geometry.source_uv(corner, aspect);
            let range = -INSIDE_TOLERANCE..=1.0 + INSIDE_TOLERANCE;
            range.contains(&u) && range.contains(&v)
        })
}

/// Shrinks a crop about its centre until every corner lands on the photo, so straightening
/// never shows the empty area outside it.
pub fn fit_inside(crop: &Crop, framing: &Framing<'_>) -> Crop {
    if corners_inside(crop, framing) {
        return *crop;
    }
    let rect = CropRect {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    };
    let at_scale = |scale: f32| {
        let scaled = rect.scaled_about_center(scale);
        let mut result = *crop;
        scaled.apply_to(&mut result, Orientation::Normal);
        result
    };
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..16 {
        let middle = (low + high) * 0.5;
        if corners_inside(&at_scale(middle), framing) {
            low = middle;
        } else {
            high = middle;
        }
    }
    at_scale(low)
}

/// What a press on the crop overlay grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropGrip {
    Move,
    Resize(Edges),
    Rotate,
}

/// Decides what a press means. `tolerance` is the grab distance on each axis, in the same
/// units as `point`.
pub fn pick_grip(rect: &CropRect, point: [f32; 2], tolerance: [f32; 2]) -> CropGrip {
    let near = |handle: Edges| {
        let [x, y] = handle.anchor(rect);
        (x - point[0]).abs() <= tolerance[0] && (y - point[1]).abs() <= tolerance[1]
    };
    let corners = Edges::ALL_HANDLES
        .iter()
        .copied()
        .filter(|handle| handle.is_corner());
    let sides = Edges::ALL_HANDLES
        .iter()
        .copied()
        .filter(|handle| !handle.is_corner());
    if let Some(handle) = corners.chain(sides).find(|handle| near(*handle)) {
        return CropGrip::Resize(handle);
    }
    let along_x = (rect.left..=rect.right).contains(&point[0]);
    let along_y = (rect.top..=rect.bottom).contains(&point[1]);
    let edge = Edges {
        left: along_y && (point[0] - rect.left).abs() <= tolerance[0],
        top: along_x && (point[1] - rect.top).abs() <= tolerance[1],
        right: along_y && (point[0] - rect.right).abs() <= tolerance[0],
        bottom: along_x && (point[1] - rect.bottom).abs() <= tolerance[1],
    };
    if edge.left || edge.top || edge.right || edge.bottom {
        CropGrip::Resize(edge)
    } else if rect.contains(point) {
        CropGrip::Move
    } else {
        CropGrip::Rotate
    }
}

/// One drag on the overlay, measured from where it started so nothing accumulates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropGesture {
    pub grip: CropGrip,
    start: CropRect,
    start_angle: f32,
    start_point: [f32; 2],
    photo_aspect: f32,
}

impl CropGesture {
    pub fn new(grip: CropGrip, start: CropRect, angle: f32, point: [f32; 2], photo_aspect: f32) -> Self {
        Self {
            grip,
            start,
            start_angle: angle,
            start_point: point,
            photo_aspect,
        }
    }

    /// The rectangle and angle for the pointer's current position.
    pub fn update(&self, point: [f32; 2], lock: Option<f32>) -> (CropRect, f32) {
        let delta = [point[0] - self.start_point[0], point[1] - self.start_point[1]];
        match self.grip {
            CropGrip::Move => (move_rect(&self.start, delta), self.start_angle),
            CropGrip::Resize(edges) => (
                drag_edges(&self.start, edges, delta, lock, self.photo_aspect),
                self.start_angle,
            ),
            CropGrip::Rotate => {
                let center = self.start.center();
                let from = pointer_angle(self.start_point, center, self.photo_aspect);
                let to = pointer_angle(point, center, self.photo_aspect);
                let angle = self.start_angle - angle_delta(from, to);
                (self.start, angle.clamp(-MAX_ANGLE, MAX_ANGLE))
            }
        }
    }
}

/// Pointer angle around a centre, in degrees, in square pixel space.
pub fn pointer_angle(point: [f32; 2], center: [f32; 2], photo_aspect: f32) -> f32 {
    let dx = (point[0] - center[0]) * photo_aspect;
    let dy = point[1] - center[1];
    dy.atan2(dx).to_degrees()
}

/// Angular difference wrapped into -180..=180.
pub fn angle_delta(from: f32, to: f32) -> f32 {
    let delta = (to - from) % 360.0;
    if delta > 180.0 {
        delta - 360.0
    } else if delta < -180.0 {
        delta + 360.0
    } else {
        delta
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AspectLock {
    Free,
    /// The photo's own ratio.
    Original,
    Ratio(u8, u8),
}

impl AspectLock {
    pub const CHOICES: [Self; 8] = [
        Self::Free,
        Self::Original,
        Self::Ratio(1, 1),
        Self::Ratio(5, 4),
        Self::Ratio(4, 3),
        Self::Ratio(3, 2),
        Self::Ratio(7, 5),
        Self::Ratio(16, 9),
    ];

    pub fn label(self) -> String {
        match self {
            Self::Free => "Free".into(),
            Self::Original => "Original".into(),
            Self::Ratio(a, b) => format!("{a} : {b}"),
        }
    }

    /// Width over height in pixels. `portrait` swaps the ratio's long and short sides.
    pub fn ratio(self, photo_aspect: f32, portrait: bool) -> Option<f32> {
        let landscape = match self {
            Self::Free => return None,
            Self::Original => return Some(photo_aspect),
            Self::Ratio(a, b) => f32::from(a) / f32::from(b),
        };
        Some(if portrait { 1.0 / landscape } else { landscape })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropTool {
    pub lock: AspectLock,
    pub portrait: bool,
    pub show_grid: bool,
}

impl Default for CropTool {
    fn default() -> Self {
        Self {
            lock: AspectLock::Free,
            portrait: false,
            show_grid: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASPECT: f32 = 1.5;
    const LEFT_TOP: Edges = Edges::ALL_HANDLES[0];
    const RIGHT: Edges = Edges::ALL_HANDLES[3];
    const RIGHT_BOTTOM: Edges = Edges::ALL_HANDLES[4];

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn neutral() -> (Transform, LensCorrections, Optics) {
        Default::default()
    }

    fn pixel_ratio(rect: &CropRect) -> f32 {
        rect.width() * ASPECT / rect.height()
    }

    #[test]
    fn moving_stops_at_the_edge_without_resizing() {
        let start = CropRect {
            left: 0.2,
            top: 0.2,
            right: 0.6,
            bottom: 0.6,
        };
        let moved = move_rect(&start, [-1.0, 1.0]);
        assert!(close(moved.left, 0.0) && close(moved.bottom, 1.0));
        assert!(close(moved.width(), 0.4) && close(moved.height(), 0.4));
    }

    #[test]
    fn free_corner_drag_moves_two_edges() {
        let moved = drag_edges(&CropRect::FULL, LEFT_TOP, [0.1, 0.2], None, ASPECT);
        assert_eq!(
            moved,
            CropRect {
                left: 0.1,
                top: 0.2,
                right: 1.0,
                bottom: 1.0
            }
        );
    }

    #[test]
    fn edges_cannot_cross() {
        let moved = drag_edges(&CropRect::FULL, RIGHT, [-5.0, 0.0], None, ASPECT);
        assert!(close(moved.width(), MIN_SIZE));
    }

    #[test]
    fn a_locked_corner_keeps_the_ratio_and_its_opposite_corner() {
        let start = fit_ratio(&CropRect::FULL, 1.0, ASPECT);
        let moved = drag_edges(&start, RIGHT_BOTTOM, [-0.1, -0.05], Some(1.0), ASPECT);
        assert!(close(pixel_ratio(&moved), 1.0), "{moved:?}");
        assert!(close(moved.left, start.left) && close(moved.top, start.top));
    }

    #[test]
    fn a_locked_edge_grows_about_the_other_axis() {
        let start = CropRect {
            left: 0.3,
            top: 0.3,
            right: 0.7,
            bottom: 0.7,
        };
        let locked = fit_ratio(&start, 1.0, ASPECT);
        let moved = drag_edges(&locked, RIGHT, [0.05, 0.0], Some(1.0), ASPECT);
        assert!(close(pixel_ratio(&moved), 1.0));
        assert!(close(moved.center()[1], locked.center()[1]));
    }

    #[test]
    fn a_locked_drag_never_leaves_the_photo() {
        let start = fit_ratio(&CropRect::FULL, 1.0, ASPECT);
        let moved = drag_edges(&start, RIGHT_BOTTOM, [2.0, 2.0], Some(1.0), ASPECT);
        assert!(
            moved.right <= 1.0 + 1e-3 && moved.bottom <= 1.0 + 1e-3,
            "{moved:?}"
        );
        assert!(close(pixel_ratio(&moved), 1.0));
    }

    #[test]
    fn straightening_shrinks_the_crop_onto_the_photo() {
        let (transform, lens, optics) = neutral();
        let framing = Framing {
            transform: &transform,
            lens: &lens,
            optics: &optics,
            aspect: ASPECT,
        };
        let crop = Crop {
            angle: 10.0,
            ..Crop::default()
        };
        let fitted = fit_inside(&crop, &framing);
        assert!(fitted.right - fitted.left < 1.0);
        assert!(corners_inside(&fitted, &framing));
        assert!(close((fitted.left + fitted.right) * 0.5, 0.5));
    }

    #[test]
    fn a_crop_already_on_the_photo_is_left_alone() {
        let (transform, lens, optics) = neutral();
        let framing = Framing {
            transform: &transform,
            lens: &lens,
            optics: &optics,
            aspect: ASPECT,
        };
        let crop = Crop {
            left: 0.2,
            top: 0.2,
            right: 0.8,
            bottom: 0.8,
            angle: 2.0,
        };
        assert_eq!(fit_inside(&crop, &framing), crop);
    }

    #[test]
    fn crop_rectangles_round_trip_through_orientation() {
        let crop = Crop {
            left: 0.1,
            top: 0.2,
            right: 0.6,
            bottom: 0.9,
            angle: 1.0,
        };
        for orientation in [
            Orientation::RotateCw,
            Orientation::RotateCcw,
            Orientation::Rotate180,
        ] {
            let mut back = Crop {
                angle: 1.0,
                ..Crop::default()
            };
            CropRect::from_crop(&crop, orientation).apply_to(&mut back, orientation);
            assert!(
                close(back.left, crop.left) && close(back.bottom, crop.bottom),
                "{back:?}"
            );
        }
    }

    #[test]
    fn handles_sit_on_corners_and_edge_midpoints() {
        let rect = CropRect {
            left: 0.2,
            top: 0.4,
            right: 0.6,
            bottom: 0.8,
        };
        assert_eq!(LEFT_TOP.anchor(&rect), [0.2, 0.4]);
        let [x, y] = RIGHT.anchor(&rect);
        assert!(close(x, 0.6) && close(y, 0.6));
    }

    #[test]
    fn angle_differences_wrap_the_short_way() {
        assert!(close(angle_delta(170.0, -170.0), 20.0));
        assert!(close(angle_delta(-170.0, 170.0), -20.0));
        assert!(close(angle_delta(10.0, 30.0), 20.0));
    }

    #[test]
    fn presses_pick_handles_then_edges_then_the_inside() {
        let rect = CropRect {
            left: 0.2,
            top: 0.2,
            right: 0.8,
            bottom: 0.8,
        };
        let tolerance = [0.02, 0.02];
        assert_eq!(
            pick_grip(&rect, [0.21, 0.19], tolerance),
            CropGrip::Resize(LEFT_TOP)
        );
        assert_eq!(pick_grip(&rect, [0.8, 0.5], tolerance), CropGrip::Resize(RIGHT));
        let CropGrip::Resize(edge) = pick_grip(&rect, [0.3, 0.8], tolerance) else {
            panic!("expected an edge");
        };
        assert!(edge.bottom && !edge.left);
        assert_eq!(pick_grip(&rect, [0.5, 0.5], tolerance), CropGrip::Move);
        assert_eq!(pick_grip(&rect, [0.05, 0.5], tolerance), CropGrip::Rotate);
    }

    #[test]
    fn a_gesture_measures_from_its_start() {
        let start = CropRect {
            left: 0.2,
            top: 0.2,
            right: 0.6,
            bottom: 0.6,
        };
        let gesture = CropGesture::new(CropGrip::Move, start, 0.0, [0.4, 0.4], ASPECT);
        gesture.update([0.9, 0.9], None);
        let (rect, _) = gesture.update([0.5, 0.4], None);
        assert!(close(rect.left, 0.3) && close(rect.top, 0.2));
    }

    #[test]
    fn dragging_clockwise_outside_turns_the_photo_clockwise() {
        let gesture = CropGesture::new(CropGrip::Rotate, CropRect::FULL, 0.0, [1.0, 0.5], ASPECT);
        let (_, angle) = gesture.update([1.0, 0.6], None);
        assert!(angle < 0.0, "{angle}");
    }

    #[test]
    fn portrait_inverts_the_lock() {
        assert_eq!(AspectLock::Ratio(3, 2).ratio(1.0, false), Some(1.5));
        assert_eq!(AspectLock::Ratio(2, 1).ratio(1.0, true), Some(0.5));
        assert_eq!(AspectLock::Free.ratio(1.0, true), None);
    }
}
