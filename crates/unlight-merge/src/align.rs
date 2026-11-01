//! Aligns handheld brackets. Median threshold bitmaps find the rough shift, since they don't
//! care about exposure at all (Ward 2003). Gauss-Newton on log luma then refines it: in linear
//! light two exposures differ by a constant gain, so in log they differ by a constant offset,
//! which is fitted alongside the motion. The motion model grows as the pyramid gets finer,
//! ending in a full homography, because a camera turning in the hand makes one. On a 24 mm lens
//! half a degree of yaw leaves tens of pixels at the frame edge that a shift can't explain.

use crate::{
    image::{Image, Plane, median},
    linalg::solve_spd,
};
use rayon::prelude::*;
use unlight_color::matrix::{IDENTITY, Mat3, mul};

/// Levels below the reduced frame. Each doubles the shift the bitmaps can find, so this reaches
/// about 4% of the frame's width either way.
const PYRAMID_LEVELS: usize = 4;
/// Pixels this far from the median in log luma don't vote, since noise decides their bit.
const BITMAP_EXCLUSION: f32 = 0.08;
const GAUSS_NEWTON_STEPS: usize = 24;
const CONVERGED_PIXELS: f64 = 0.002;
/// A Tukey biweight cuts off at this many robust sigmas, which is what makes it ignore a person
/// walking through the frame.
const TUKEY_CUTOFF: f32 = 4.685;
const MIN_RESIDUAL_SCALE: f32 = 0.01;
const MEDIAN_TO_SIGMA: f32 = 1.4826;
const DAMPING: f64 = 1e-3;
/// Below this level of the top of the range, luma says more about noise than the scene.
const DARK_LIMIT: f32 = 0.006;
const BRIGHT_LIMIT: f32 = 0.9;
const LOG_FLOOR: f32 = 1e-4;
/// Each pyramid level pre-blurs log luma by this much, which keeps demosaicing's fine pattern
/// and sensor noise out of the gradients.
const LEVEL_BLUR: f32 = 0.8;
/// Fewer usable pixels than this and a level can't constrain a motion.
const MIN_SAMPLES: usize = 400;
/// Largest move an alignment can claim, as a share of the frame's width. Beyond it the frames
/// aren't a bracket, and leaving them unaligned is the honest outcome.
const MAX_MOVE: f64 = 0.2;

/// Where each pixel of the reference lands in a frame, as a homography on centred pixel
/// coordinates: the origin is the middle of the image.
pub type Alignment = Mat3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Translation,
    Rigid,
    Projective,
}

impl Motion {
    fn parameters(self) -> usize {
        match self {
            Self::Translation => 2,
            Self::Rigid => 3,
            Self::Projective => 8,
        }
    }

    /// How a pixel at normalised `(x, y)` moves per unit of each parameter. Parameters are in
    /// pixels at the frame edge, so they're all of similar size and the system stays well
    /// conditioned.
    fn basis(self, x: f32, y: f32) -> [[f32; 2]; 8] {
        match self {
            Self::Translation => {
                let mut basis = [[0.0; 2]; 8];
                basis[0] = [1.0, 0.0];
                basis[1] = [0.0, 1.0];
                basis
            }
            Self::Rigid => {
                let mut basis = [[0.0; 2]; 8];
                basis[0] = [1.0, 0.0];
                basis[1] = [0.0, 1.0];
                basis[2] = [-y, x];
                basis
            }
            Self::Projective => [
                [1.0, 0.0],
                [0.0, 1.0],
                [x, 0.0],
                [y, 0.0],
                [0.0, x],
                [0.0, y],
                [-x * x, -x * y],
                [-x * y, -y * y],
            ],
        }
    }

    /// The small warp an update stands for.
    fn update(self, step: &[f64], scale: f64) -> Mat3 {
        let (mut a, mut b, mut c, mut d, mut g, mut h) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        match self {
            Self::Translation => {}
            Self::Rigid => {
                b = -step[2];
                c = step[2];
            }
            Self::Projective => {
                (a, b, c, d, g, h) = (step[2], step[3], step[4], step[5], step[6], step[7]);
            }
        }
        [
            [1.0 + a / scale, b / scale, step[0]],
            [c / scale, 1.0 + d / scale, step[1]],
            [g / (scale * scale), h / (scale * scale), 1.0],
        ]
    }
}

/// One frame ready to compare: log luma, and where it can be trusted.
#[derive(Clone)]
struct Level {
    log: Plane,
    usable: Plane,
}

impl Level {
    fn from_image(image: &Image, clip: [f32; 3]) -> Self {
        let top = clip[1].max(1e-6);
        let log = image.plane(|pixel| (crate::image::luma(pixel).max(0.0) / top + LOG_FLOOR).ln());
        let usable = image.plane(|pixel| {
            let highest = (0..3).map(|c| pixel[c] / clip[c].max(1e-6)).fold(0.0, f32::max);
            let level = crate::image::luma(pixel) / top;
            f32::from(u8::from(highest < BRIGHT_LIMIT && level > DARK_LIMIT))
        });
        Self { log: log.blurred(LEVEL_BLUR), usable }
    }

    fn halved(&self) -> Self {
        Self { log: self.log.halved().blurred(LEVEL_BLUR), usable: self.usable.halved() }
    }
}

fn pyramid(image: &Image, clip: [f32; 3]) -> Vec<Level> {
    let mut levels = vec![Level::from_image(image, clip)];
    for _ in 0..PYRAMID_LEVELS {
        let next = levels[levels.len() - 1].halved();
        if next.log.width.min(next.log.height) < 16 {
            break;
        }
        levels.push(next);
    }
    levels
}

struct Bitmap {
    width: usize,
    height: usize,
    bits: Vec<bool>,
    counted: Vec<bool>,
}

fn bitmap(level: &Level) -> Bitmap {
    let mut sample: Vec<f32> = level
        .log
        .values
        .iter()
        .zip(&level.usable.values)
        .filter(|(_, usable)| **usable > 0.5)
        .map(|(value, _)| *value)
        .collect();
    if sample.is_empty() {
        sample = level.log.values.clone();
    }
    let threshold = median(&mut sample).unwrap_or(0.0);
    Bitmap {
        width: level.log.width,
        height: level.log.height,
        bits: level.log.values.iter().map(|value| *value > threshold).collect(),
        counted: level.log.values.iter().map(|value| (*value - threshold).abs() > BITMAP_EXCLUSION).collect(),
    }
}

/// Counts differing bits with `moving` shifted by `(dx, dy)`.
fn bitmap_difference(reference: &Bitmap, moving: &Bitmap, dx: isize, dy: isize) -> f32 {
    let (width, height) = (reference.width.min(moving.width) as isize, reference.height.min(moving.height) as isize);
    let rows: Vec<isize> = (0..height).collect();
    let (different, compared): (u64, u64) = rows
        .par_iter()
        .map(|&y| {
            let (mut different, mut compared) = (0u64, 0u64);
            let shifted_y = y + dy;
            if !(0..height).contains(&shifted_y) {
                return (0, 0);
            }
            for x in 0..width {
                let shifted_x = x + dx;
                if !(0..width).contains(&shifted_x) {
                    continue;
                }
                let at = (y * reference.width as isize + x) as usize;
                let other = (shifted_y * moving.width as isize + shifted_x) as usize;
                if reference.counted[at] && moving.counted[other] {
                    compared += 1;
                    different += u64::from(reference.bits[at] != moving.bits[other]);
                }
            }
            (different, compared)
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    if compared == 0 { 1.0 } else { different as f32 / compared as f32 }
}

/// The shift of `moving` against `reference`, in pixels of the base level.
fn bitmap_shift(reference: &[Level], moving: &[Level]) -> [isize; 2] {
    let mut shift = [0isize; 2];
    for level in (0..reference.len().min(moving.len())).rev() {
        let (a, b) = (bitmap(&reference[level]), bitmap(&moving[level]));
        shift = shift.map(|value| value * 2);
        let reach = if level + 1 == reference.len() { 3 } else { 1 };
        let mut best = (f32::MAX, shift);
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let candidate = [shift[0] + dx, shift[1] + dy];
                let difference = bitmap_difference(&a, &b, candidate[0], candidate[1]);
                let tie = (dx.abs() + dy.abs()) as f32 * 1e-6;
                if difference + tie < best.0 {
                    best = (difference + tie, candidate);
                }
            }
        }
        shift = best.1;
    }
    shift
}

#[inline]
fn warp_point(matrix: &Mat3, x: f32, y: f32) -> [f32; 2] {
    let (hx, hy) = (f64::from(x), f64::from(y));
    let w = matrix[2][0] * hx + matrix[2][1] * hy + matrix[2][2];
    [
        ((matrix[0][0] * hx + matrix[0][1] * hy + matrix[0][2]) / w) as f32,
        ((matrix[1][0] * hx + matrix[1][1] * hy + matrix[1][2]) / w) as f32,
    ]
}

#[derive(Clone)]
struct Normal {
    matrix: Vec<f64>,
    vector: Vec<f64>,
    weight: f64,
    used: usize,
}

impl Normal {
    fn new(size: usize) -> Self {
        Self { matrix: vec![0.0; size * size], vector: vec![0.0; size], weight: 0.0, used: 0 }
    }

    fn merged(mut self, other: Self) -> Self {
        self.matrix.iter_mut().zip(&other.matrix).for_each(|(a, b)| *a += b);
        self.vector.iter_mut().zip(&other.vector).for_each(|(a, b)| *a += b);
        self.weight += other.weight;
        self.used += other.used;
        self
    }
}

/// The moving frame resampled through `matrix`, plus where that's usable.
fn warped(moving: &Level, reference: &Level, matrix: &Mat3) -> Level {
    let (width, height) = (reference.log.width, reference.log.height);
    let (center_x, center_y) = (width as f32 * 0.5, height as f32 * 0.5);
    let (moving_x, moving_y) = (moving.log.width as f32 * 0.5, moving.log.height as f32 * 0.5);
    let position = |x: usize, y: usize| {
        let [mapped_x, mapped_y] = warp_point(matrix, x as f32 + 0.5 - center_x, y as f32 + 0.5 - center_y);
        [mapped_x + moving_x - 0.5, mapped_y + moving_y - 0.5]
    };
    let inside = |x: f32, y: f32| x >= 0.0 && y >= 0.0 && x <= (moving.log.width - 1) as f32 && y <= (moving.log.height - 1) as f32;
    Level {
        log: Plane::from_fn(width, height, |x, y| {
            let [sx, sy] = position(x, y);
            moving.log.bilinear(sx, sy)
        }),
        usable: Plane::from_fn(width, height, |x, y| {
            let [sx, sy] = position(x, y);
            if inside(sx, sy) { moving.usable.bilinear(sx, sy) } else { 0.0 }
        }),
    }
}

/// Median absolute residual over pixels both frames can use, after removing the offset between
/// them. Also the measure that decides whether an alignment helped.
fn residual_stats(reference: &Level, moved: &Level) -> Option<(f32, f32)> {
    let mut offsets: Vec<f32> = Vec::new();
    for (index, (a, b)) in reference.log.values.iter().zip(&moved.log.values).enumerate() {
        if reference.usable.values[index] > 0.99 && moved.usable.values[index] > 0.99 {
            offsets.push(a - b);
        }
    }
    if offsets.len() < MIN_SAMPLES {
        return None;
    }
    let offset = median(&mut offsets)?;
    let mut absolute: Vec<f32> = offsets.iter().map(|value| (value - offset).abs()).collect();
    Some((offset, median(&mut absolute)?))
}

/// Refines `matrix`, which is in this level's centred pixels. Forward compositional: each step
/// warps the moving frame by what's known so far and solves for a small correction to that.
fn refine(reference: &Level, moving: &Level, matrix: &mut Mat3, motion: Motion) {
    let (width, height) = (reference.log.width, reference.log.height);
    let scale = width as f32 * 0.5;
    let size = motion.parameters() + 1;
    let (center_x, center_y) = (width as f32 * 0.5, height as f32 * 0.5);
    for _ in 0..GAUSS_NEWTON_STEPS {
        let moved = warped(moving, reference, matrix);
        let Some((offset, spread)) = residual_stats(reference, &moved) else {
            return;
        };
        let sigma = (spread * MEDIAN_TO_SIGMA).max(MIN_RESIDUAL_SCALE);
        let cutoff = sigma * TUKEY_CUTOFF;
        let rows: Vec<usize> = (1..height - 1).collect();
        let normal = rows
            .par_iter()
            .fold(
                || Normal::new(size),
                |mut normal, &y| {
                    for x in 1..width - 1 {
                        let at = y * width + x;
                        if reference.usable.values[at] < 0.99 || moved.usable.values[at] < 0.99 {
                            continue;
                        }
                        let residual = reference.log.values[at] - moved.log.values[at] - offset;
                        if residual.abs() >= cutoff {
                            continue;
                        }
                        let tukey = 1.0 - (residual / cutoff).powi(2);
                        let weight = f64::from(tukey * tukey);
                        let gradient_x = (moved.log.values[at + 1] - moved.log.values[at - 1]) * 0.5;
                        let gradient_y = (moved.log.values[at + width] - moved.log.values[at - width]) * 0.5;
                        let normal_x = (x as f32 + 0.5 - center_x) / scale;
                        let normal_y = (y as f32 + 0.5 - center_y) / scale;
                        let basis = motion.basis(normal_x, normal_y);
                        let mut jacobian = [0.0f64; 9];
                        for (slot, direction) in jacobian.iter_mut().zip(&basis).take(size - 1) {
                            *slot = f64::from(gradient_x * direction[0] + gradient_y * direction[1]);
                        }
                        jacobian[size - 1] = 1.0;
                        for row in 0..size {
                            for column in 0..=row {
                                normal.matrix[row * size + column] += weight * jacobian[row] * jacobian[column];
                            }
                            normal.vector[row] += weight * jacobian[row] * f64::from(residual);
                        }
                        normal.weight += weight;
                        normal.used += 1;
                    }
                    normal
                },
            )
            .reduce(|| Normal::new(size), Normal::merged);
        if normal.used < MIN_SAMPLES {
            return;
        }
        let Normal { matrix: mut system, mut vector, .. } = normal;
        for row in 0..size {
            for column in row + 1..size {
                system[row * size + column] = system[column * size + row];
            }
            system[row * size + row] *= 1.0 + DAMPING;
            system[row * size + row] += 1e-9;
        }
        if solve_spd(&mut system, &mut vector).is_none() {
            return;
        }
        let step = &vector[..size - 1];
        if step.iter().any(|value| !value.is_finite()) {
            return;
        }
        let update = motion.update(step, f64::from(scale));
        *matrix = mul(matrix, &update);
        let largest = step.iter().fold(0.0f64, |peak, value| peak.max(value.abs()));
        if largest < CONVERGED_PIXELS {
            break;
        }
    }
}

fn rescaled(matrix: &Mat3, factor: f64) -> Mat3 {
    let mut scaled = *matrix;
    scaled[0][2] *= factor;
    scaled[1][2] *= factor;
    scaled[2][0] /= factor;
    scaled[2][1] /= factor;
    scaled
}

fn is_plausible(matrix: &Mat3, width: usize, height: usize) -> bool {
    let limit = MAX_MOVE * width as f64;
    let (half_x, half_y) = (width as f32 * 0.5, height as f32 * 0.5);
    [[-half_x, -half_y], [half_x, -half_y], [-half_x, half_y], [half_x, half_y]].iter().all(|corner| {
        let [x, y] = warp_point(matrix, corner[0], corner[1]);
        f64::from(x - corner[0]).hypot(f64::from(y - corner[1])) <= limit
    })
}

fn align_levels(reference: &[Level], moving: &[Level]) -> Mat3 {
    let [dx, dy] = bitmap_shift(reference, moving);
    let top = reference.len().min(moving.len()) - 1;
    let factor = (1usize << top) as f64;
    let mut matrix = IDENTITY;
    matrix[0][2] = dx as f64 / factor;
    matrix[1][2] = dy as f64 / factor;
    for level in (0..=top).rev() {
        let motion = match level {
            0 | 1 => Motion::Projective,
            2 => Motion::Rigid,
            _ => Motion::Translation,
        };
        refine(&reference[level], &moving[level], &mut matrix, Motion::Translation);
        if motion != Motion::Translation {
            refine(&reference[level], &moving[level], &mut matrix, motion);
        }
        if level > 0 {
            matrix = rescaled(&matrix, 2.0);
        }
    }
    let base = (&reference[0], &moving[0]);
    let cost = |candidate: &Mat3| residual_stats(base.0, &warped(base.1, base.0, candidate)).map_or(f32::MAX, |stats| stats.1);
    let plausible = is_plausible(&matrix, reference[0].log.width, reference[0].log.height);
    if plausible && cost(&matrix) <= cost(&IDENTITY) { matrix } else { IDENTITY }
}

/// Alignment of `moving` to `reference`: a homography on centred pixels, reference to moving.
pub fn measure_alignment(reference: &Image, moving: &Image, clip: [[f32; 3]; 2]) -> Alignment {
    align_levels(&pyramid(reference, clip[0]), &pyramid(moving, clip[1]))
}

/// Aligns every frame to `reference`, a position in `images`. Frames are compared with their
/// neighbour on the way to the reference rather than with the reference itself, because two
/// frames many stops apart can have almost no pixels both of them exposed properly. `images`
/// must be in order of exposure.
pub fn align_frames(images: &[&Image], clips: &[[f32; 3]], reference: usize) -> Vec<Alignment> {
    let pyramids: Vec<Vec<Level>> =
        images.iter().zip(clips).map(|(image, clip)| pyramid(image, *clip)).collect();
    let mut matrices = vec![IDENTITY; images.len()];
    for index in reference + 1..images.len() {
        let step = align_levels(&pyramids[index - 1], &pyramids[index]);
        matrices[index] = mul(&step, &matrices[index - 1]);
    }
    for index in (0..reference).rev() {
        let step = align_levels(&pyramids[index + 1], &pyramids[index]);
        matrices[index] = mul(&step, &matrices[index + 1]);
    }
    matrices
}

/// Brings an alignment measured on a small copy up to a frame `factor` times bigger.
pub(crate) fn scale_alignment(matrix: &Alignment, factor: f64) -> Alignment {
    rescaled(matrix, factor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{image::Rgb, resample::lanczos_rgb};
    use unlight_color::matrix::inverse;

    const CLIP: [f32; 3] = [1.0; 3];

    /// Smooth value noise, so a shifted copy can be computed exactly at any position.
    fn scene(x: f32, y: f32) -> f32 {
        let wave = |fx: f32, fy: f32, phase: f32| ((x * fx + y * fy) * 0.1 + phase).sin();
        let broad = wave(0.31, 0.17, 0.3) + wave(-0.13, 0.41, 1.1) + wave(0.71, -0.23, 2.0);
        let fine = wave(1.7, 0.9, 0.7) * wave(-0.8, 1.9, 0.2);
        0.02 * (2.2 + broad + 0.8 * fine).exp()
    }

    /// The scene as a frame sees it, when `matrix` carries reference pixels to this frame's.
    fn view(width: usize, height: usize, exposure: f32, matrix: &Mat3) -> Image {
        let (half_x, half_y) = (width as f32 * 0.5, height as f32 * 0.5);
        let back = inverse(matrix).unwrap();
        Image::from_fn(width, height, |x, y| {
            let [sx, sy] = warp_point(&back, x as f32 + 0.5 - half_x, y as f32 + 0.5 - half_y);
            let value = (scene(sx, sy) * exposure).min(1.0);
            [value, value, value]
        })
    }

    fn corner_error(found: &Mat3, truth: &Mat3, width: usize, height: usize) -> f32 {
        let (half_x, half_y) = (width as f32 * 0.5, height as f32 * 0.5);
        [[-half_x, -half_y], [half_x, -half_y], [-half_x, half_y], [half_x, half_y]]
            .iter()
            .map(|corner| {
                let a = warp_point(found, corner[0], corner[1]);
                let b = warp_point(truth, corner[0], corner[1]);
                (a[0] - b[0]).hypot(a[1] - b[1])
            })
            .fold(0.0, f32::max)
    }

    fn rigid(dx: f64, dy: f64, degrees: f64) -> Mat3 {
        let (sin, cos) = degrees.to_radians().sin_cos();
        [[cos, -sin, dx], [sin, cos, dy], [0.0, 0.0, 1.0]]
    }

    #[test]
    fn finds_a_large_whole_shift_through_two_stops() {
        let truth = rigid(13.0, -8.0, 0.0);
        let reference = view(512, 384, 1.0, &IDENTITY);
        let moving = view(512, 384, 0.25, &truth);
        let found = measure_alignment(&reference, &moving, [CLIP, CLIP]);
        let error = corner_error(&found, &truth, 512, 384);
        assert!(error < 0.15, "corner error {error}");
    }

    #[test]
    fn finds_a_subpixel_shift_and_rotation_through_two_stops() {
        let truth = rigid(3.37, -1.64, 0.31);
        let reference = view(640, 480, 1.0, &IDENTITY);
        let moving = view(640, 480, 4.0, &truth);
        let found = measure_alignment(&reference, &moving, [CLIP, CLIP]);
        let error = corner_error(&found, &truth, 640, 480);
        assert!(error < 0.15, "corner error {error}");
    }

    #[test]
    fn finds_the_keystone_of_a_turning_camera() {
        let mut truth = rigid(2.2, 1.3, 0.1);
        truth[2][0] = 1.2e-5;
        truth[2][1] = -0.8e-5;
        let reference = view(640, 480, 1.0, &IDENTITY);
        let moving = view(640, 480, 0.5, &truth);
        let found = measure_alignment(&reference, &moving, [CLIP, CLIP]);
        let error = corner_error(&found, &truth, 640, 480);
        assert!(error < 0.3, "corner error {error}");
    }

    #[test]
    fn a_moving_object_doesnt_drag_the_alignment() {
        let truth = rigid(2.4, 1.8, 0.2);
        let reference = view(640, 480, 1.0, &IDENTITY);
        let mut moving = view(640, 480, 0.25, &truth);
        for y in 200..320 {
            for x in 260..420 {
                moving.pixels[y * 640 + x] = [0.6; 3];
            }
        }
        let found = measure_alignment(&reference, &moving, [CLIP, CLIP]);
        let error = corner_error(&found, &truth, 640, 480);
        assert!(error < 0.25, "corner error {error}");
    }

    #[test]
    fn identical_frames_stay_put() {
        let image = view(320, 240, 1.0, &IDENTITY);
        let found = measure_alignment(&image, &image, [CLIP, CLIP]);
        assert!(corner_error(&found, &IDENTITY, 320, 240) < 0.02);
    }

    #[test]
    fn featureless_frames_stay_put() {
        let flat = Image::from_fn(320, 240, |_, _| [0.2; 3]);
        assert_eq!(measure_alignment(&flat, &flat, [CLIP, CLIP]), IDENTITY);
    }

    #[test]
    fn a_chain_reaches_a_frame_that_shares_nothing_with_the_reference() {
        let first = rigid(1.5, -1.0, 0.0);
        let second = rigid(3.0, -2.0, 0.0);
        let frames = [view(512, 384, 16.0, &second), view(512, 384, 1.0, &first), view(512, 384, 1.0 / 16.0, &IDENTITY)];
        let images: Vec<&Image> = frames.iter().collect();
        let found = align_frames(&images, &[CLIP; 3], 2);
        assert!(corner_error(&found[1], &first, 512, 384) < 0.3);
        assert!(corner_error(&found[0], &second, 512, 384) < 0.5);
    }

    #[test]
    fn a_small_copys_alignment_scales_to_the_full_frame() {
        let truth = rigid(1.2, 0.7, 0.15);
        let doubled = scale_alignment(&truth, 2.0);
        let (x, y) = (100.0, -60.0);
        let small = warp_point(&truth, x, y);
        let large = warp_point(&doubled, x * 2.0, y * 2.0);
        assert!((large[0] - small[0] * 2.0).abs() < 1e-3);
        assert!((large[1] - small[1] * 2.0).abs() < 1e-3);
    }

    #[test]
    fn resampling_by_the_found_alignment_matches_the_reference() {
        let truth = rigid(2.6, -1.3, 0.2);
        let reference = view(480, 360, 1.0, &IDENTITY);
        let moving = view(480, 360, 1.0, &truth);
        let found = measure_alignment(&reference, &moving, [CLIP, CLIP]);
        let (half_x, half_y) = (240.0, 180.0);
        let sample = |x: usize, y: usize| -> Rgb {
            let [sx, sy] = warp_point(&found, x as f32 + 0.5 - half_x, y as f32 + 0.5 - half_y);
            lanczos_rgb(&moving, sx + half_x - 0.5, sy + half_y - 0.5)
        };
        let mut total = 0.0;
        let mut count = 0;
        for y in (40..320).step_by(4) {
            for x in (40..440).step_by(4) {
                let expected = reference.at(x, y)[0];
                if expected < 0.9 {
                    total += ((sample(x, y)[0] + 1e-3).ln() - (expected + 1e-3).ln()).abs();
                    count += 1;
                }
            }
        }
        let mean = total / count as f32;
        assert!(mean < 0.01, "mean log error {mean}");
    }
}
