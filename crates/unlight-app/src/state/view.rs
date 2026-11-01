//! Zoom and pan arithmetic. The photo is addressed in display orientation everywhere in the
//! UI; `Orientation` converts to the sensor orientation the engine renders in.

pub use unlight_recipe::Orientation;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    Fit,
    Fill,
    /// Device pixels per photo pixel, 1.0 is 100%.
    Ratio(f32),
}

pub const MIN_RATIO: f32 = 0.02;
pub const MAX_RATIO: f32 = 8.0;
const ZOOM_STEP: f32 = 1.25;

/// What the viewport should show of the photo, for one canvas size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewPlan {
    /// Top-left of the image inside the canvas, in device pixels.
    pub offset: [f32; 2],
    /// Size of the image on screen, in device pixels.
    pub size: [u32; 2],
    /// Display-oriented x, y, width, height of the photo that `size` shows.
    pub region: [f32; 4],
    /// Device pixels per photo pixel.
    pub scale: f32,
}

impl ViewPlan {
    pub fn canvas_to_photo(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let [rx, ry, rw, rh] = self.region;
        [
            rx + (x - self.offset[0]) / self.size[0].max(1) as f32 * rw,
            ry + (y - self.offset[1]) / self.size[1].max(1) as f32 * rh,
        ]
    }

    pub fn photo_to_canvas(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let [rx, ry, rw, rh] = self.region;
        [
            self.offset[0] + (x - rx) / rw.max(1e-6) * self.size[0] as f32,
            self.offset[1] + (y - ry) / rh.max(1e-6) * self.size[1] as f32,
        ]
    }

    /// Size of the texture the engine should render into, in sensor orientation.
    pub fn target_size(&self, orientation: Orientation) -> [u32; 2] {
        orientation.display_size(self.size)
    }
}

/// The viewport's zoom and pan, independent of canvas size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom: Zoom,
    /// The zoom a click toggles to, like Lightroom's Navigator.
    pub toggle_ratio: f32,
    /// Photo point at the centre of the canvas.
    pub center: [f32; 2],
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: Zoom::Fit,
            toggle_ratio: 1.0,
            center: [0.5, 0.5],
        }
    }
}

fn fit_scale(canvas: [f32; 2], output: [u32; 2]) -> f32 {
    (canvas[0] / output[0].max(1) as f32).min(canvas[1] / output[1].max(1) as f32)
}

fn fill_scale(canvas: [f32; 2], output: [u32; 2]) -> f32 {
    (canvas[0] / output[0].max(1) as f32).max(canvas[1] / output[1].max(1) as f32)
}

impl ViewState {
    pub fn scale(&self, canvas: [f32; 2], output: [u32; 2]) -> f32 {
        match self.zoom {
            Zoom::Fit => fit_scale(canvas, output),
            Zoom::Fill => fill_scale(canvas, output),
            Zoom::Ratio(ratio) => ratio,
        }
    }

    /// Lays the photo out on a canvas. `output` is the size of the cropped photo in display
    /// orientation, `canvas` is in device pixels.
    pub fn plan(&self, canvas: [f32; 2], output: [u32; 2]) -> ViewPlan {
        let scale = self.scale(canvas, output).max(1e-4);
        let axis = |index: usize| {
            let full = output[index].max(1) as f32 * scale;
            let shown = full.min(canvas[index]).round().max(1.0);
            let span = (shown / full).min(1.0);
            let start = (self.center[index] - span * 0.5).clamp(0.0, 1.0 - span);
            ((canvas[index] - shown) * 0.5, shown as u32, start, span)
        };
        let (offset_x, width, x, region_width) = axis(0);
        let (offset_y, height, y, region_height) = axis(1);
        ViewPlan {
            offset: [offset_x.round(), offset_y.round()],
            size: [width, height],
            region: [x, y, region_width, region_height],
            scale,
        }
    }

    /// Moves the view so the photo follows a pointer drag in device pixels.
    pub fn pan(&mut self, plan: &ViewPlan, delta: [f32; 2]) {
        let [_, _, width, height] = plan.region;
        let span = [width, height];
        let size = plan.size;
        for axis in 0..2 {
            let start = plan.region[axis] + span[axis] * 0.5;
            let moved = start - delta[axis] / size[axis].max(1) as f32 * span[axis];
            self.center[axis] = moved.clamp(span[axis] * 0.5, 1.0 - span[axis] * 0.5);
        }
    }

    /// Changes the zoom ratio, keeping the photo point under `anchor` where it is.
    pub fn zoom_about(&mut self, ratio: f32, anchor: [f32; 2], canvas: [f32; 2], output: [u32; 2]) {
        let before = self.plan(canvas, output);
        let held = before.canvas_to_photo(anchor);
        self.zoom = Zoom::Ratio(ratio.clamp(MIN_RATIO, MAX_RATIO));
        self.center = held;
        let after = self.plan(canvas, output);
        let drift = after.photo_to_canvas(held);
        self.pan(&after, [anchor[0] - drift[0], anchor[1] - drift[1]]);
    }

    pub fn step_ratio(&self, steps: f32, canvas: [f32; 2], output: [u32; 2]) -> f32 {
        self.scale(canvas, output) * ZOOM_STEP.powf(steps)
    }

    /// A click on the photo: zoom in on that point, or back out to fit.
    pub fn toggle_at(&mut self, point: [f32; 2]) {
        match self.zoom {
            Zoom::Ratio(_) => self.zoom = Zoom::Fit,
            Zoom::Fit | Zoom::Fill => {
                self.zoom = Zoom::Ratio(self.toggle_ratio);
                self.center = point;
            }
        }
    }

    /// Remembers the current ratio as the one a click returns to, as Lightroom does.
    pub fn remember_ratio(&mut self) {
        if let Zoom::Ratio(ratio) = self.zoom {
            self.toggle_ratio = ratio;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: [f32; 2] = [1600.0, 1000.0];
    const OUTPUT: [u32; 2] = [6000, 4000];

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn fit_shows_the_whole_photo_centred() {
        let plan = ViewState::default().plan(CANVAS, OUTPUT);
        assert_eq!(plan.region, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(plan.size, [1500, 1000]);
        assert_eq!(plan.offset, [50.0, 0.0]);
    }

    #[test]
    fn fill_covers_the_canvas() {
        let view = ViewState {
            zoom: Zoom::Fill,
            ..ViewState::default()
        };
        let plan = view.plan(CANVAS, OUTPUT);
        assert_eq!(plan.size, [1600, 1000]);
        assert!(close(plan.region[2], 1.0));
        assert!(close(plan.region[3], 1000.0 / (4000.0 * 1600.0 / 6000.0)));
    }

    #[test]
    fn hundred_percent_maps_one_to_one() {
        let view = ViewState {
            zoom: Zoom::Ratio(1.0),
            ..ViewState::default()
        };
        let plan = view.plan(CANVAS, OUTPUT);
        assert_eq!(plan.size, [1600, 1000]);
        assert!(close(plan.region[2], 1600.0 / 6000.0));
        assert!(close(plan.region[3], 1000.0 / 4000.0));
        assert!(close(plan.region[0], 0.5 - 1600.0 / 12000.0));
    }

    #[test]
    fn panning_stops_at_the_photo_edge() {
        let mut view = ViewState {
            zoom: Zoom::Ratio(1.0),
            ..ViewState::default()
        };
        let plan = view.plan(CANVAS, OUTPUT);
        view.pan(&plan, [100_000.0, 100_000.0]);
        let plan = view.plan(CANVAS, OUTPUT);
        assert!(
            close(plan.region[0], 0.0) && close(plan.region[1], 0.0),
            "{plan:?}"
        );
    }

    #[test]
    fn a_pan_moves_the_photo_by_exactly_the_drag() {
        let mut view = ViewState {
            zoom: Zoom::Ratio(1.0),
            ..ViewState::default()
        };
        let plan = view.plan(CANVAS, OUTPUT);
        let grabbed = plan.canvas_to_photo([800.0, 500.0]);
        view.pan(&plan, [-120.0, 40.0]);
        let moved = view.plan(CANVAS, OUTPUT).photo_to_canvas(grabbed);
        assert!(close(moved[0], 680.0) && close(moved[1], 540.0), "{moved:?}");
    }

    #[test]
    fn zooming_holds_the_point_under_the_pointer() {
        let mut view = ViewState {
            zoom: Zoom::Ratio(0.5),
            ..ViewState::default()
        };
        let anchor = [1000.0, 300.0];
        let held = view.plan(CANVAS, OUTPUT).canvas_to_photo(anchor);
        view.zoom_about(1.5, anchor, CANVAS, OUTPUT);
        let after = view.plan(CANVAS, OUTPUT).photo_to_canvas(held);
        assert!(
            close(after[0], anchor[0]) && close(after[1], anchor[1]),
            "{after:?}"
        );
    }

    #[test]
    fn a_click_toggles_to_the_remembered_ratio_and_back() {
        let mut view = ViewState {
            toggle_ratio: 2.0,
            ..ViewState::default()
        };
        view.toggle_at([0.25, 0.75]);
        assert_eq!(view.zoom, Zoom::Ratio(2.0));
        assert_eq!(view.center, [0.25, 0.75]);
        view.toggle_at([0.5, 0.5]);
        assert_eq!(view.zoom, Zoom::Fit);
    }

    #[test]
    fn canvas_and_photo_mapping_invert() {
        let view = ViewState {
            zoom: Zoom::Ratio(0.75),
            center: [0.3, 0.6],
            ..ViewState::default()
        };
        let plan = view.plan(CANVAS, OUTPUT);
        let point = plan.canvas_to_photo(plan.photo_to_canvas([0.31, 0.62]));
        assert!(close(point[0], 0.31) && close(point[1], 0.62));
    }

    #[test]
    fn orientation_round_trips_and_rotates_the_right_way() {
        for orientation in [
            Orientation::Normal,
            Orientation::Rotate180,
            Orientation::RotateCw,
            Orientation::RotateCcw,
        ] {
            let back = orientation.to_sensor(orientation.to_display([0.2, 0.7]));
            assert!(close(back[0], 0.2) && close(back[1], 0.7));
        }
        assert_eq!(Orientation::RotateCw.to_display([0.0, 0.0]), [1.0, 0.0]);
        assert_eq!(Orientation::RotateCcw.to_display([0.0, 0.0]), [0.0, 1.0]);
    }

    #[test]
    fn a_display_region_becomes_a_sensor_rectangle() {
        let region = Orientation::RotateCw.sensor_region([0.1, 0.2, 0.3, 0.4]);
        let expected = [0.2, 1.0 - 0.4, 0.4, 0.3];
        for (a, b) in region.iter().zip(expected) {
            assert!(close(*a, b), "{region:?}");
        }
    }

    #[test]
    fn rotated_targets_swap_their_axes() {
        let plan = ViewState::default().plan(CANVAS, [4000, 6000]);
        assert_eq!(
            plan.target_size(Orientation::RotateCw),
            [plan.size[1], plan.size[0]]
        );
    }
}
