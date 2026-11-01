//! Lens corrections for one frame, on the CPU. A panorama needs them before stitching, since
//! frames won't line up with distortion in them, so this does what `source_position` and
//! `sample_camera` in develop.wgsl do. The corrections go into the pixels at full strength, and
//! the merged file records that so they can't be applied a second time.

use crate::{Frame, Image, image::Rgb, resample::bilinear_rgb};
use std::sync::Arc;
use unlight_lens::{LensProfile, PhotoLens};
use unlight_raw::{GainMap, Opcode};

/// Position in a frame, centred, with the image height as the unit. This is the coordinate
/// system lens profiles are written in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Centred(pub [f32; 2]);

#[derive(Clone, Debug, Default)]
pub struct FrameLens {
    profile: Option<Arc<LensProfile>>,
    gain_maps: Vec<GainMap>,
}

impl FrameLens {
    /// Takes whatever correction the file, the camera or the lens database has for a frame.
    /// A frame nothing is known about simply isn't corrected.
    pub fn for_frame(frame: &Frame) -> Self {
        let lens = PhotoLens::from_parts(
            frame.metadata.clone(),
            frame.width as u32,
            frame.height as u32,
            frame.embedded_lens.clone(),
            frame.opcodes.clone(),
        );
        Self::new(lens.resolve().ok().map(Arc::new), frame)
    }

    fn new(profile: Option<Arc<LensProfile>>, frame: &Frame) -> Self {
        let gain_maps = frame
            .opcodes
            .list3
            .iter()
            .filter_map(|opcode| match opcode {
                Opcode::GainMap(map) => Some(map.clone()),
                _ => None,
            })
            .collect();
        Self { profile, gain_maps }
    }

    #[cfg(test)]
    pub(crate) fn with_profile(profile: LensProfile) -> Self {
        Self { profile: Some(Arc::new(profile)), gain_maps: Vec::new() }
    }

    /// Whether anything has been found to correct, for reports.
    pub fn is_known(&self) -> bool {
        self.profile.is_some() || !self.gain_maps.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn corrects_vignette(&self) -> bool {
        self.profile.as_ref().is_some_and(|profile| profile.vignette.is_some()) || !self.gain_maps.is_empty()
    }

    /// Takes an undistorted position to where green sits in the source. The lens step of
    /// `source_position`.
    pub(crate) fn source_position(&self, ideal: Centred) -> Centred {
        let Some(profile) = self.profile.as_deref().filter(|profile| profile.distortion.is_some()) else {
            return ideal;
        };
        let [center_x, center_y] = profile.center;
        let (dx, dy) = (ideal.0[0] - center_x, ideal.0[1] - center_y);
        let r2 = dx * dx + dy * dy;
        let scale = profile.distortion_scale(r2.sqrt());
        let [t0, t1] = profile.tangential;
        Centred([
            center_x + dx * scale + t1 * (r2 + 2.0 * dx * dx) + 2.0 * t0 * dx * dy,
            center_y + dy * scale + t0 * (r2 + 2.0 * dy * dy) + 2.0 * t1 * dx * dy,
        ])
    }

    /// Inverts `source_position`, for finding the outline of a corrected frame.
    pub(crate) fn ideal_position(&self, source: Centred) -> Centred {
        const ITERATIONS: usize = 16;
        (0..ITERATIONS).fold(source, |estimate, _| {
            let warped = self.source_position(estimate);
            Centred([
                estimate.0[0] + source.0[0] - warped.0[0],
                estimate.0[1] + source.0[1] - warped.0[1],
            ])
        })
    }

    /// Where red and blue sit, given where green does. These are `None` if the lens has no
    /// chromatic aberration data, so a frame without any only pays for one lookup.
    pub(crate) fn channel_positions(&self, green: Centred) -> Option<[Centred; 2]> {
        let profile = self.profile.as_deref().filter(|profile| profile.chromatic.is_some())?;
        let [center_x, center_y] = profile.center;
        let (dx, dy) = (green.0[0] - center_x, green.0[1] - center_y);
        let [red, blue] = profile.chromatic_scale(dx.hypot(dy));
        Some([
            Centred([center_x + dx * red, center_y + dy * red]),
            Centred([center_x + dx * blue, center_y + dy * blue]),
        ])
    }

    /// What a pixel at a source position has to be multiplied by, per channel, to undo the
    /// lens's falloff.
    pub(crate) fn gain_at(&self, position: Centred, aspect: f32) -> Rgb {
        let stops = self.profile.as_deref().filter(|profile| profile.vignette.is_some()).map_or(0.0, |profile| {
            let [center_x, center_y] = profile.vignette_center;
            profile.vignette_stops((position.0[0] - center_x).hypot(position.0[1] - center_y))
        });
        let mut gain = [2f32.powf(stops); 3];
        if !self.gain_maps.is_empty() {
            let uv = [position.0[0] / aspect + 0.5, position.0[1] + 0.5];
            for map in &self.gain_maps {
                apply_gain_map(map, uv, &mut gain);
            }
        }
        gain
    }

    /// A copy resampled through the lens correction, with its falloff removed, so that lines
    /// are straight and features can be matched. Bilinear, since copies are only analysed.
    pub(crate) fn corrected_copy(&self, image: &Image) -> Image {
        if !self.is_known() {
            return image.clone();
        }
        let (width, height) = (image.width as f32, image.height as f32);
        let aspect = width / height;
        Image::from_fn(image.width, image.height, |x, y| {
            let ideal = Centred([((x as f32 + 0.5) / width - 0.5) * aspect, (y as f32 + 0.5) / height - 0.5]);
            let green = self.source_position(ideal);
            let at = |position: Centred| {
                let sx = (position.0[0] / aspect + 0.5) * width - 0.5;
                let sy = (position.0[1] + 0.5) * height - 0.5;
                bilinear_rgb(image, sx, sy)
            };
            let mut pixel = at(green);
            if let Some([red, blue]) = self.channel_positions(green) {
                pixel[0] = at(red)[0];
                pixel[2] = at(blue)[2];
            }
            let gain = self.gain_at(green, aspect);
            std::array::from_fn(|c| pixel[c] * gain[c])
        })
    }
}

/// Multiplies in one gain map at `uv`, the way the plane gain texture does in the shader.
fn apply_gain_map(map: &GainMap, uv: [f32; 2], gain: &mut Rgb) {
    let (columns, rows) = (map.points_h.max(1) as usize, map.points_v.max(1) as usize);
    let stride = map.map_planes.max(1) as usize;
    let place = |value: f32, origin: f64, spacing: f64, count: usize| -> (usize, f32) {
        let spacing = if spacing > 0.0 { spacing } else { 1.0 };
        let position = ((f64::from(value) - origin) / spacing).clamp(0.0, (count - 1) as f64) as f32;
        let lower = (position as usize).min(count.saturating_sub(2));
        (lower, position - lower as f32)
    };
    let (column, along) = place(uv[0], map.origin_h, map.spacing_h, columns);
    let (row, down) = place(uv[1], map.origin_v, map.spacing_v, rows);
    for plane in map.plane..(map.plane + map.planes.max(1)).min(3) {
        let layer = ((plane - map.plane) as usize).min(stride - 1);
        let value = |c: usize, r: usize| {
            let index = (r.min(rows - 1) * columns + c.min(columns - 1)) * stride + layer;
            map.gains.get(index).copied().unwrap_or(1.0)
        };
        let top = value(column, row) * (1.0 - along) + value(column + 1, row) * along;
        let bottom = value(column, row + 1) * (1.0 - along) + value(column + 1, row + 1) * along;
        gain[plane as usize] *= top * (1.0 - down) + bottom * down;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::plain_frame;
    use unlight_lens::{Frame as LensFrame, Source};

    fn barrel_profile() -> LensProfile {
        let frame = LensFrame::new(400, 300);
        let mut profile = LensProfile::empty(frame);
        profile.set_distortion(Source::Lensfun, |radius| 1.0 - 0.1 * radius * radius);
        profile.set_vignette(Source::Lensfun, |radius| 1.5 * radius * radius);
        profile
    }

    #[test]
    fn an_unknown_lens_changes_nothing() {
        let image = Image::from_fn(16, 16, |x, y| [x as f32, y as f32, 1.0]);
        let lens = FrameLens::default();
        assert!(!lens.is_known());
        assert_eq!(lens.corrected_copy(&image), image);
        let position = lens.source_position(Centred([0.25, -0.1]));
        assert_eq!(position.0, [0.25, -0.1]);
    }

    #[test]
    fn distortion_matches_the_profile_curve() {
        let lens = FrameLens::with_profile(barrel_profile());
        let Centred([x, y]) = lens.source_position(Centred([0.5, 0.0]));
        assert!((x - 0.5 * (1.0 - 0.1 * 0.25)).abs() < 1e-3, "{x}");
        assert_eq!(y, 0.0);
    }

    #[test]
    fn undoing_the_correction_finds_the_original_position() {
        let lens = FrameLens::with_profile(barrel_profile());
        for point in [[0.6, 0.4], [-0.65, 0.5], [0.0, -0.45]] {
            let source = lens.source_position(Centred(point));
            let back = lens.ideal_position(source);
            assert!((back.0[0] - point[0]).abs() < 1e-4 && (back.0[1] - point[1]).abs() < 1e-4, "{back:?}");
        }
    }

    #[test]
    fn chromatic_aberration_moves_red_and_blue_apart_from_green() {
        let mut profile = barrel_profile();
        profile.set_chromatic(Source::Lensfun, |_| [1.01, 0.98]);
        let lens = FrameLens::with_profile(profile);
        let [red, blue] = lens.channel_positions(Centred([0.4, 0.0])).unwrap();
        assert!((red.0[0] - 0.404).abs() < 1e-3, "{red:?}");
        assert!((blue.0[0] - 0.392).abs() < 1e-3, "{blue:?}");
        assert!(FrameLens::default().channel_positions(Centred([0.4, 0.0])).is_none());
    }

    #[test]
    fn a_corrected_copy_brings_fringed_channels_back_into_line() {
        let mut profile = LensProfile::empty(LensFrame::new(400, 300));
        profile.set_chromatic(Source::Lensfun, |_| [1.02, 0.98]);
        let lens = FrameLens::with_profile(profile);
        // A step at a radius, with red and blue reaching it further out and further in. The
        // correction reads red further out, so all three should fall in the same place.
        let (center_x, center_y) = (199.5f32, 149.5f32);
        let ring = |x: usize, y: usize, stretch: f32| {
            let radius = (x as f32 - center_x).hypot(y as f32 - center_y) / stretch;
            if radius < 90.0 { 0.8 } else { 0.1 }
        };
        let fringed = Image::from_fn(400, 300, |x, y| [ring(x, y, 1.02), ring(x, y, 1.0), ring(x, y, 0.98)]);
        let flat = lens.corrected_copy(&fringed);
        let fringe = |image: &Image| -> f32 {
            (0..200).map(|x| image.at(x, 150)).map(|[r, g, b]| (r - g).abs() + (b - g).abs()).sum()
        };
        assert!(fringe(&flat) < fringe(&fringed) * 0.35, "{} against {}", fringe(&flat), fringe(&fringed));
    }

    #[test]
    fn vignette_gain_follows_the_radius_in_stops() {
        let lens = FrameLens::with_profile(barrel_profile());
        let gain = lens.gain_at(Centred([0.5, 0.0]), 4.0 / 3.0);
        let expected = 2f32.powf(1.5 * 0.25);
        assert!((gain[0] - expected).abs() < 1e-2, "{gain:?}");
        assert_eq!(lens.gain_at(Centred([0.0, 0.0]), 4.0 / 3.0), [1.0; 3]);
    }

    #[test]
    fn a_corrected_copy_flattens_a_shaded_field() {
        let lens = FrameLens::with_profile(barrel_profile());
        let (width, height) = (400usize, 300usize);
        let shaded = Image::from_fn(width, height, |x, y| {
            let dx = (x as f32 + 0.5) / 300.0 - 2.0 / 3.0;
            let dy = (y as f32 + 0.5) / 300.0 - 0.5;
            [0.5 * 2f32.powf(-1.5 * (dx * dx + dy * dy)); 3]
        });
        let flat = lens.corrected_copy(&shaded);
        let corner = flat.at(10, 10)[0];
        let middle = flat.at(200, 150)[0];
        assert!((corner / middle - 1.0).abs() < 0.03, "{corner} {middle}");
    }

    #[test]
    fn gain_maps_interpolate_and_stay_on_their_plane() {
        let map = GainMap {
            top: 0,
            left: 0,
            bottom: 100,
            right: 100,
            plane: 1,
            planes: 1,
            row_pitch: 1,
            col_pitch: 1,
            points_v: 1,
            points_h: 2,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            map_planes: 1,
            gains: vec![1.0, 3.0],
        };
        let mut gain = [1.0; 3];
        apply_gain_map(&map, [0.5, 0.5], &mut gain);
        assert_eq!(gain, [1.0, 2.0, 1.0]);
    }

    #[test]
    fn frames_with_gain_maps_count_as_corrected() {
        let mut frame = plain_frame(&Image::new(8, 8), 8);
        assert!(!FrameLens::new(None, &frame).corrects_vignette());
        frame.opcodes.list3.push(Opcode::Unsupported { id: 1, optional: true });
        assert!(!FrameLens::new(None, &frame).is_known());
    }
}
