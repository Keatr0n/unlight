//! Turns crop, straighten, perspective and lens settings into the UV transform the develop
//! pass evaluates per pixel.

use std::sync::Arc;
use unlight_color::matrix::{IDENTITY, Mat3, inverse, mul};
use unlight_lens::{LensProfile, Source};
use unlight_recipe::{Crop, LensCorrections, Transform};

/// Maximum keystone from the Vertical and Horizontal sliders, in image heights.
const PERSPECTIVE_RANGE: f64 = 0.6;
const DISTORTION_RANGE: f64 = 0.25;

/// Cells along a rectangle's sides when searching for the largest one that fits.
const SEARCH_GRID: usize = 200;
/// Points checked along each edge when testing whether a rectangle stays on the photo.
const EDGE_SAMPLES: usize = 32;
const BISECTION_STEPS: usize = 24;
/// Zoomed out this far, a rectangle is twice the frame, which no lens correction needs.
const LARGEST_FRAME: f32 = 2.0;
/// Undoing a warp converges within a few rounds, since a lens moves points by a few percent.
const UNWARP_ITERATIONS: usize = 12;

/// What a photo adds to its recipe's geometry. It is derived from the file and cached on the
/// session, so a recipe never carries it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Optics {
    pub profile: Option<Arc<LensProfile>>,
    /// Upright and frame fitting, as a transform from undistorted to output coordinates.
    pub correction: Option<Mat3>,
}

/// A rectangle in view coordinates, where 0..1 spans the uncropped frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewRect {
    pub center: [f32; 2],
    pub half_size: [f32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub inverse_homography: Mat3,
    pub crop_origin: [f32; 2],
    pub crop_size: [f32; 2],
    pub rotation: [f32; 2],
    /// Radial polynomial `k1, k2, k3` over squared radius, and a scale.
    pub lens: [f32; 4],
    pub lens_center: [f32; 2],
    /// Decentering terms, already scaled by the profile slider.
    pub tangential: [f32; 2],
    /// How much of the profile's radial curve to apply.
    pub profile_distortion: f32,
    profile: Option<Arc<LensProfile>>,
}

/// How far a slider's correction applies. What's part of how a file renders holds at full
/// strength until a profile switched on takes over its scaling.
fn slider_strength(source: Option<Source>, enabled: bool, slider: f32) -> f32 {
    match source {
        None => 0.0,
        Some(_) if enabled => slider / 100.0,
        Some(Source::DngOpcodes | Source::Panasonic) => 1.0,
        Some(_) => 0.0,
    }
}

pub fn distortion_strength(lens: &LensCorrections, profile: &LensProfile) -> f32 {
    slider_strength(profile.distortion, lens.profile_enabled, lens.profile_distortion)
}

pub fn vignette_strength(lens: &LensCorrections, profile: &LensProfile) -> f32 {
    slider_strength(profile.vignette, lens.profile_enabled, lens.profile_vignette)
}

fn translation(x: f64, y: f64) -> Mat3 {
    [[1.0, 0.0, x], [0.0, 1.0, y], [0.0, 0.0, 1.0]]
}

fn rotation(degrees: f64) -> Mat3 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]]
}

fn scaling(x: f64, y: f64) -> Mat3 {
    [[x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, 1.0]]
}

fn keystone(horizontal: f64, vertical: f64) -> Mat3 {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [horizontal, vertical, 1.0]]
}

/// Source-to-output transform for the Transform panel, in image-centred coordinates with
/// the image height as the unit.
fn transform_matrix(transform: &Transform) -> Mat3 {
    let scale = f64::from(transform.scale) / 100.0;
    let aspect = 2f64.powf(f64::from(transform.aspect) / 100.0);
    let offset = translation(
        f64::from(transform.offset_x) / 100.0,
        f64::from(transform.offset_y) / 100.0,
    );
    let vertical = f64::from(transform.vertical) / 100.0 * PERSPECTIVE_RANGE;
    let horizontal = f64::from(transform.horizontal) / 100.0 * PERSPECTIVE_RANGE;
    mul(
        &offset,
        &mul(
            &scaling(scale * aspect, scale / aspect),
            &mul(&rotation(f64::from(transform.rotate)), &keystone(horizontal, vertical)),
        ),
    )
}

impl Geometry {
    pub fn new(
        crop: &Crop,
        transform: &Transform,
        lens: &LensCorrections,
        aspect: f32,
        optics: &Optics,
    ) -> Self {
        let forward = mul(&transform_matrix(transform), &optics.correction.unwrap_or(IDENTITY));
        let inverse_homography = inverse(&forward).unwrap_or(IDENTITY);
        let half_diagonal_squared = (f64::from(aspect).powi(2) + 1.0) * 0.25;
        let k1 = -f64::from(lens.manual_distortion) / 100.0 * DISTORTION_RANGE / half_diagonal_squared;
        let (sin, cos) = f32::to_radians(crop.angle).sin_cos();
        let profile = optics.profile.as_deref();
        let strength = profile.map_or(0.0, |profile| distortion_strength(lens, profile));
        Self {
            inverse_homography,
            crop_origin: [crop.left, crop.top],
            crop_size: [crop.right - crop.left, crop.bottom - crop.top],
            rotation: [cos, sin],
            lens: [k1 as f32, 0.0, 0.0, 1.0],
            lens_center: profile.map_or([0.0; 2], |profile| profile.center),
            tangential: profile.map_or([0.0; 2], |profile| profile.tangential.map(|term| term * strength)),
            profile_distortion: strength,
            profile: optics.profile.clone(),
        }
    }

    /// Output coordinates to undistorted image-centred coordinates: everything in
    /// `source_position` from develop.wgsl up to the lens.
    fn undistorted(&self, view: [f32; 2], aspect: f32) -> [f32; 2] {
        let x = self.crop_origin[0] + view[0] * self.crop_size[0];
        let y = self.crop_origin[1] + view[1] * self.crop_size[1];
        let (px, py) = ((x - 0.5) * aspect, y - 0.5);
        let (cos, sin) = (self.rotation[0], self.rotation[1]);
        let (rx, ry) = (cos * px - sin * py, sin * px + cos * py);
        let h = &self.inverse_homography;
        let (hx, hy) = (f64::from(rx), f64::from(ry));
        let w = h[2][0] * hx + h[2][1] * hy + h[2][2];
        [
            ((h[0][0] * hx + h[0][1] * hy + h[0][2]) / w) as f32,
            ((h[1][0] * hx + h[1][1] * hy + h[1][2]) / w) as f32,
        ]
    }

    /// The lens step of `source_position` in develop.wgsl.
    pub fn warp(&self, undistorted: [f32; 2]) -> [f32; 2] {
        let [center_x, center_y] = self.lens_center;
        let (dx, dy) = (undistorted[0] - center_x, undistorted[1] - center_y);
        let r2 = dx * dx + dy * dy;
        let [k1, k2, k3, base] = self.lens;
        let mut scale = base + r2 * (k1 + r2 * (k2 + r2 * k3));
        if let Some(profile) = self.profile.as_deref().filter(|_| self.profile_distortion != 0.0) {
            scale *= 1.0 + self.profile_distortion * (profile.distortion_scale(r2.sqrt()) - 1.0);
        }
        let [t0, t1] = self.tangential;
        [
            center_x + dx * scale + t1 * (r2 + 2.0 * dx * dx) + 2.0 * t0 * dx * dy,
            center_y + dy * scale + t0 * (r2 + 2.0 * dy * dy) + 2.0 * t1 * dx * dy,
        ]
    }

    /// Inverts `warp`, for points known only in the source, such as guides.
    pub fn unwarp(&self, source: [f32; 2]) -> [f32; 2] {
        (0..UNWARP_ITERATIONS).fold(source, |estimate, _| {
            let warped = self.warp(estimate);
            [estimate[0] + source[0] - warped[0], estimate[1] + source[1] - warped[1]]
        })
    }

    /// Inverts `source_uv`, for things stored in source coordinates that are drawn over a view.
    pub fn view_from_source(&self, uv: [f32; 2], aspect: f32) -> [f32; 2] {
        let [x, y] = self.unwarp([(uv[0] - 0.5) * aspect, uv[1] - 0.5]);
        let h = inverse(&self.inverse_homography).unwrap_or(IDENTITY);
        let (hx, hy) = (f64::from(x), f64::from(y));
        let w = h[2][0] * hx + h[2][1] * hy + h[2][2];
        let (rx, ry) = (
            ((h[0][0] * hx + h[0][1] * hy + h[0][2]) / w) as f32,
            ((h[1][0] * hx + h[1][1] * hy + h[1][2]) / w) as f32,
        );
        let (cos, sin) = (self.rotation[0], self.rotation[1]);
        let (px, py) = (cos * rx + sin * ry, cos * ry - sin * rx);
        [
            (px / aspect + 0.5 - self.crop_origin[0]) / self.crop_size[0],
            (py + 0.5 - self.crop_origin[1]) / self.crop_size[1],
        ]
    }

    /// Where an output position lands in the source, in image-centred coordinates.
    pub fn source_position(&self, view: [f32; 2], aspect: f32) -> [f32; 2] {
        self.warp(self.undistorted(view, aspect))
    }

    /// The same mapping as `source_uv` in develop.wgsl, for CPU-side overlays and tests.
    pub fn source_uv(&self, view: [f32; 2], aspect: f32) -> [f32; 2] {
        let [x, y] = self.source_position(view, aspect);
        [x / aspect + 0.5, y + 0.5]
    }

    pub fn is_identity(&self) -> bool {
        let default = Self::new(
            &Crop::default(),
            &Transform::default(),
            &LensCorrections::default(),
            1.0,
            &Optics::default(),
        );
        *self == default
    }

    fn uncropped(&self) -> Self {
        Self {
            crop_origin: [0.0; 2],
            crop_size: [1.0; 2],
            rotation: [1.0, 0.0],
            ..self.clone()
        }
    }

    fn on_photo(&self, view: [f32; 2], aspect: f32) -> bool {
        self.source_uv(view, aspect).iter().all(|value| (0.0..=1.0).contains(value))
    }

    fn rect_fits(&self, rect: ViewRect, aspect: f32) -> bool {
        (0..=EDGE_SAMPLES).all(|step| {
            let along = step as f32 / EDGE_SAMPLES as f32 * 2.0 - 1.0;
            [[along, -1.0], [along, 1.0], [-1.0, along], [1.0, along]].iter().all(|edge| {
                let view = std::array::from_fn(|axis| rect.center[axis] + edge[axis] * rect.half_size[axis]);
                self.on_photo(view, aspect)
            })
        })
    }

    /// The largest scale of `half_size` about `center` that stays on the photo, up to `limit`.
    fn grow(&self, center: [f32; 2], half_size: [f32; 2], aspect: f32, limit: f32) -> ViewRect {
        let at = |scale: f32| ViewRect { center, half_size: half_size.map(|side| side * scale) };
        if self.rect_fits(at(limit), aspect) {
            return at(limit);
        }
        let (mut low, mut high) = (0.0, limit);
        for _ in 0..BISECTION_STEPS {
            let middle = (low + high) * 0.5;
            if self.rect_fits(at(middle), aspect) {
                low = middle;
            } else {
                high = middle;
            }
        }
        at(low)
    }

    /// Finds the largest rectangle of a given shape on the photo. Cells are shaped like the
    /// rectangle, which turns this into the classic largest-square dynamic programme.
    fn largest_rect(&self, half_size: [f32; 2], aspect: f32) -> ViewRect {
        let cell = half_size.map(|side| 2.0 * side / SEARCH_GRID as f32);
        let columns = (1.0 / cell[0]).ceil() as usize;
        let rows = (1.0 / cell[1]).ceil() as usize;
        let mut sides = vec![0u32; columns];
        let (mut best, mut best_end) = (0, [0usize; 2]);
        for row in 0..rows {
            let mut diagonal = 0;
            for column in 0..columns {
                let view = [(column as f32 + 0.5) * cell[0], (row as f32 + 0.5) * cell[1]];
                let previous = sides[column];
                let left = if column == 0 { 0 } else { sides[column - 1] };
                sides[column] = if self.on_photo(view, aspect) { 1 + previous.min(left).min(diagonal) } else { 0 };
                diagonal = previous;
                if sides[column] > best {
                    (best, best_end) = (sides[column], [column, row]);
                }
            }
        }
        let center = std::array::from_fn(|axis| (best_end[axis] as f32 + 1.0 - best as f32 * 0.5) * cell[axis]);
        self.grow(center, half_size, aspect, 1.0)
    }

    /// The largest crop of a given shape holding no empty corners, for Constrain Crop. Shapes
    /// are width over height, so pass the image's aspect ratio to keep its proportions.
    pub fn largest_crop(&self, image_aspect: f32, crop_aspect: f32) -> Crop {
        let shape = crop_aspect / image_aspect;
        let half_size = if shape > 1.0 { [0.5, 0.5 / shape] } else { [0.5 * shape, 0.5] };
        let ViewRect { center, half_size } = self.uncropped().largest_rect(half_size, image_aspect);
        Crop {
            left: center[0] - half_size[0],
            top: center[1] - half_size[1],
            right: center[0] + half_size[0],
            bottom: center[1] + half_size[1],
            angle: 0.0,
        }
    }

    /// The transform that fills the frame with the photo: outward as far as a lens correction
    /// frees up, inward as far as a warp leaves corners empty. Upright usually leaves a
    /// lopsided shape, so `recenter` lets the frame slide to keep more of the picture.
    pub fn frame_fit(&self, aspect: f32, recenter: bool) -> Mat3 {
        let geometry = self.uncropped();
        let half_size = [0.5; 2];
        let center = if recenter { geometry.largest_rect(half_size, aspect).center } else { [0.5; 2] };
        let rect = geometry.grow(center, half_size, aspect, LARGEST_FRAME);
        let zoom = f64::from(0.5 / rect.half_size[0]);
        let shift = [f64::from((rect.center[0] - 0.5) * aspect), f64::from(rect.center[1] - 0.5)];
        mul(&scaling(zoom, zoom), &translation(-shift[0], -shift[1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_lens::PhotoLens;
    use unlight_raw::{Cfa, ColorData, Metadata, Opcode, Opcodes, RawImage, WarpPlane, WarpRectilinear};

    const ASPECT: f32 = 1.5;

    fn geometry(crop: Crop, transform: Transform, lens: LensCorrections) -> Geometry {
        Geometry::new(&crop, &transform, &lens, ASPECT, &Optics::default())
    }

    fn warp_opcodes(radial: [f64; 4], tangential: [f64; 2], center: [f64; 2]) -> Opcodes {
        let warp = WarpRectilinear {
            planes: vec![WarpPlane { radial, tangential }],
            center_x: center[0],
            center_y: center[1],
        };
        Opcodes { list3: vec![Opcode::WarpRectilinear(warp)], ..Opcodes::default() }
    }

    fn profile_from(opcodes: Opcodes) -> Arc<LensProfile> {
        let mut raw = RawImage {
            width: 3000,
            height: 2000,
            samples: Vec::new(),
            cfa: Cfa { period: 2, pattern: [[0; 6]; 6] },
            black: [[0.0; 6]; 6],
            white: 1.0,
            color: ColorData {
                xyz_to_camera: [[0.0; 3]; 3],
                as_shot_multipliers: [1.0; 3],
                daylight_multipliers: [1.0; 3],
                baseline_exposure: 0.0,
                dng: None,
            },
            opcodes: Opcodes::default(),
            metadata: Metadata::default(),
            embedded_lens: None,
        };
        raw.opcodes = opcodes;
        Arc::new(PhotoLens::new(&raw).file_profile().unwrap())
    }

    fn warped(radial: [f64; 4], tangential: [f64; 2], center: [f64; 2], lens: LensCorrections) -> Geometry {
        let optics = Optics {
            profile: Some(profile_from(warp_opcodes(radial, tangential, center))),
            correction: None,
        };
        Geometry::new(&Crop::default(), &Transform::default(), &lens, ASPECT, &optics)
    }

    fn assert_close(a: [f32; 2], b: [f32; 2]) {
        assert!((a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4, "{a:?} vs {b:?}");
    }

    #[test]
    fn default_settings_leave_coordinates_alone() {
        let g = geometry(Crop::default(), Transform::default(), LensCorrections::default());
        for point in [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0], [0.2, 0.9]] {
            assert_close(g.source_uv(point, ASPECT), point);
        }
    }

    #[test]
    fn crop_maps_the_view_onto_the_rectangle() {
        let crop = Crop { left: 0.25, top: 0.5, right: 0.75, bottom: 1.0, angle: 0.0 };
        let g = geometry(crop, Transform::default(), LensCorrections::default());
        assert_close(g.source_uv([0.0, 0.0], ASPECT), [0.25, 0.5]);
        assert_close(g.source_uv([1.0, 1.0], ASPECT), [0.75, 1.0]);
    }

    #[test]
    fn straightening_turns_about_the_centre_without_shearing() {
        let crop = Crop { angle: 90.0, ..Crop::default() };
        let g = geometry(crop, Transform::default(), LensCorrections::default());
        assert_close(g.source_uv([0.5, 0.5], ASPECT), [0.5, 0.5]);
        let moved = g.source_uv([0.5, 0.75], ASPECT);
        assert!((moved[0] - (0.5 - 0.25 / ASPECT)).abs() < 1e-4 && (moved[1] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn scale_zooms_about_the_centre() {
        let transform = Transform { scale: 200.0, ..Transform::default() };
        let g = geometry(Crop::default(), transform, LensCorrections::default());
        assert_close(g.source_uv([1.0, 1.0], ASPECT), [0.75, 0.75]);
    }

    #[test]
    fn vertical_keystone_narrows_one_end_only() {
        let transform = Transform { vertical: 50.0, ..Transform::default() };
        let g = geometry(Crop::default(), transform, LensCorrections::default());
        let top = g.source_uv([0.0, 0.0], ASPECT);
        let bottom = g.source_uv([0.0, 1.0], ASPECT);
        assert!((top[0] - bottom[0]).abs() > 0.05, "{top:?} {bottom:?}");
        assert_close(g.source_uv([0.5, 0.5], ASPECT), [0.5, 0.5]);
    }

    #[test]
    fn distortion_moves_corners_and_spares_the_centre() {
        let lens = LensCorrections { manual_distortion: 50.0, ..LensCorrections::default() };
        let g = geometry(Crop::default(), Transform::default(), lens);
        assert_close(g.source_uv([0.5, 0.5], ASPECT), [0.5, 0.5]);
        let corner = g.source_uv([1.0, 1.0], ASPECT);
        assert!((corner[0] - 1.0).abs() > 0.03);
    }

    #[test]
    fn opposite_distortions_pull_opposite_ways() {
        let bent = |manual_distortion| {
            let lens = LensCorrections { manual_distortion, ..LensCorrections::default() };
            geometry(Crop::default(), Transform::default(), lens).source_uv([1.0, 1.0], ASPECT)[0]
        };
        assert!((bent(40.0) - 1.0) * (bent(-40.0) - 1.0) < 0.0);
    }

    #[test]
    fn dng_warp_matches_the_spec_equations() {
        let (kr, kt, center) = ([0.98, 0.05, -0.05, 0.02], [0.004, -0.002], [0.46, 0.51]);
        let g = warped(kr, kt, center, LensCorrections::default());
        let aspect = f64::from(ASPECT);
        let (cx, cy) = ((center[0] - 0.5) * aspect, center[1] - 0.5);
        let norm = (cx.abs() + aspect * 0.5).hypot(cy.abs() + 0.5);
        for view in [[0.1, 0.2], [0.5, 0.5], [0.95, 0.9], [0.02, 0.98]] {
            let (x, y) = ((f64::from(view[0]) - 0.5) * aspect, f64::from(view[1]) - 0.5);
            let (dx, dy) = ((x - cx) / norm, (y - cy) / norm);
            let r2 = dx * dx + dy * dy;
            let f = kr[0] + r2 * (kr[1] + r2 * (kr[2] + r2 * kr[3]));
            let sx = dx * f + kt[1] * (r2 + 2.0 * dx * dx) + 2.0 * kt[0] * dx * dy;
            let sy = dy * f + kt[0] * (r2 + 2.0 * dy * dy) + 2.0 * kt[1] * dx * dy;
            let expected = [(cx + sx * norm) / aspect + 0.5, cy + sy * norm + 0.5];
            let found = g.source_uv(view, ASPECT);
            assert!(
                (f64::from(found[0]) - expected[0]).abs() < 1e-5
                    && (f64::from(found[1]) - expected[1]).abs() < 1e-5,
                "{found:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn a_dng_warp_applies_without_a_profile_switched_on() {
        let g = warped([0.9, 0.0, 0.0, 0.0], [0.0; 2], [0.5, 0.5], LensCorrections::default());
        assert_eq!(g.profile_distortion, 1.0);
        assert!(!g.is_identity());
    }

    #[test]
    fn profile_slider_scales_a_dng_warp_when_switched_on() {
        let strength = |slider| {
            let lens = LensCorrections {
                profile_enabled: true,
                profile_distortion: slider,
                ..LensCorrections::default()
            };
            warped([0.9, 0.0, 0.0, 0.0], [0.0; 2], [0.5, 0.5], lens).source_uv([1.0, 0.5], ASPECT)[0]
        };
        assert!((strength(0.0) - 1.0).abs() < 1e-5);
        assert!((strength(100.0) - 0.95).abs() < 1e-4);
        assert!((strength(200.0) - 0.9).abs() < 1e-4);
    }

    #[test]
    fn unwarp_inverts_warp() {
        let g = warped([0.98, 0.05, -0.05, 0.02], [0.004, -0.002], [0.46, 0.51], LensCorrections::default());
        for point in [[0.1, -0.2], [0.7, 0.45], [-0.6, 0.3]] {
            let back = g.unwarp(g.warp(point));
            assert_close(back, point);
        }
    }

    #[test]
    fn view_from_source_inverts_source_uv_through_every_stage() {
        let crop = Crop { left: 0.1, top: 0.2, right: 0.8, bottom: 0.9, angle: 7.0 };
        let transform = Transform { vertical: 20.0, rotate: 3.0, ..Transform::default() };
        let lens = LensCorrections { manual_distortion: 15.0, ..LensCorrections::default() };
        let g = geometry(crop, transform, lens);
        for view in [[0.5, 0.5], [0.1, 0.9], [0.8, 0.2]] {
            assert_close(g.view_from_source(g.source_uv(view, ASPECT), ASPECT), view);
        }
    }

    fn pincushion() -> Geometry {
        warped([1.0, 0.1, 0.0, 0.0], [0.0; 2], [0.5, 0.5], LensCorrections::default())
    }

    #[test]
    fn largest_crop_has_no_empty_corners_and_keeps_the_shape() {
        let g = pincushion();
        let crop = g.largest_crop(ASPECT, ASPECT);
        assert!(crop.right - crop.left < 1.0);
        let shape = (crop.right - crop.left) / (crop.bottom - crop.top);
        assert!((shape - 1.0).abs() < 1e-3, "{shape}");
        for corner in [[crop.left, crop.top], [crop.right, crop.top], [crop.left, crop.bottom], [crop.right, crop.bottom]] {
            let uv = g.source_uv(corner, ASPECT);
            assert!(uv.iter().all(|value| (0.0..=1.0001).contains(value)), "{uv:?}");
        }
    }

    #[test]
    fn largest_crop_is_tight_against_the_photo() {
        let g = pincushion();
        let crop = g.largest_crop(ASPECT, ASPECT);
        let uv = g.source_uv([crop.right, crop.bottom], ASPECT);
        assert!(uv[0] > 0.995 || uv[1] > 0.995, "{uv:?}");
    }

    #[test]
    fn largest_crop_of_an_untouched_photo_is_the_whole_frame() {
        let g = geometry(Crop::default(), Transform::default(), LensCorrections::default());
        let crop = g.largest_crop(ASPECT, ASPECT);
        assert!(crop.left.abs() < 1e-3 && (crop.right - 1.0).abs() < 1e-3, "{crop:?}");
    }

    #[test]
    fn largest_crop_honours_a_different_shape() {
        let g = geometry(Crop::default(), Transform::default(), LensCorrections::default());
        let crop = g.largest_crop(ASPECT, 1.0);
        let width = (crop.right - crop.left) * ASPECT;
        assert!((width - (crop.bottom - crop.top)).abs() < 1e-3, "{crop:?}");
    }

    #[test]
    fn frame_fit_closes_the_empty_corners_of_a_pincushion_correction() {
        let g = pincushion();
        assert!(!g.rect_fits(ViewRect { center: [0.5; 2], half_size: [0.5; 2] }, ASPECT));
        let optics = Optics { correction: Some(g.frame_fit(ASPECT, false)), ..Optics::default() };
        let optics = Optics { profile: g.profile.clone(), ..optics };
        let fitted = Geometry::new(&Crop::default(), &Transform::default(), &LensCorrections::default(), ASPECT, &optics);
        assert!(fitted.rect_fits(ViewRect { center: [0.5; 2], half_size: [0.499; 2] }, ASPECT));
        assert_close(fitted.source_uv([0.5, 0.5], ASPECT), [0.5, 0.5]);
    }

    #[test]
    fn frame_fit_zooms_out_to_use_what_a_barrel_correction_frees_up() {
        let g = warped([1.0, -0.1, 0.0, 0.0], [0.0; 2], [0.5, 0.5], LensCorrections::default());
        let fit = g.frame_fit(ASPECT, false);
        assert!(fit[0][0] < 1.0, "{fit:?}");
    }
}
