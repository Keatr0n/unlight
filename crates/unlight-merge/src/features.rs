//! Finds points in a frame that can be recognised in another. Corners come from the structure
//! tensor over a small pyramid, each described by a histogram of gradient directions in the
//! manner of SIFT (Lowe 2004, whose patent has expired). All of this runs on log luma, where a
//! change of exposure is only an offset and gradients don't see it, so frames shot at different
//! brightness still match.

use crate::{Image, Plane, image::luma};
use rayon::prelude::*;

const OCTAVES: usize = 3;
const STRUCTURE_BLUR: f32 = 1.6;
const CELLS_ACROSS: usize = 4;
/// Pixels per descriptor cell, at the feature's own scale.
const CELL_PIXELS: f32 = 4.0;
const DIRECTIONS: usize = 8;
pub const DESCRIPTOR: usize = CELLS_ACROSS * CELLS_ACROSS * DIRECTIONS;
/// No single gradient may hold more than this share of a descriptor (Lowe's 0.2). Lighting
/// changes the size of gradients far more than their direction.
const DESCRIPTOR_CLIP: f32 = 0.2;
const ORIENTATION_BINS: usize = 36;
const ORIENTATION_RADIUS: isize = 8;
const ORIENTATION_SIGMA: f32 = 4.5;
/// The grid features are spread over, so that one busy corner of a frame can't use up the
/// budget and leave the overlap with a neighbour empty.
const GRID_COLUMNS: usize = 24;
const GRID_ROWS: usize = 16;
/// Corners weaker than this share of the strongest one are noise.
const WEAKEST: f32 = 1e-3;
/// Added so log luma stays finite. Below this a frame holds only noise.
const LOG_FLOOR: f32 = 2e-4;

#[derive(Clone, Debug)]
pub struct Feature {
    /// Position in pixels of the base image.
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub angle: f32,
    pub descriptor: Box<[f32; DESCRIPTOR]>,
}

struct Octave {
    log: Plane,
    gradient_x: Plane,
    gradient_y: Plane,
    scale: f32,
}

impl Octave {
    fn new(log: Plane, scale: f32) -> Self {
        let (width, height) = (log.width, log.height);
        let gradient_x = Plane::from_fn(width, height, |x, y| {
            (log.at((x + 1).min(width - 1), y) - log.at(x.saturating_sub(1), y)) * 0.5
        });
        let gradient_y = Plane::from_fn(width, height, |x, y| {
            (log.at(x, (y + 1).min(height - 1)) - log.at(x, y.saturating_sub(1))) * 0.5
        });
        Self { log, gradient_x, gradient_y, scale }
    }

    /// Smaller eigenvalue of the structure tensor, which is large only at a true corner.
    fn corners(&self) -> Plane {
        let (width, height) = (self.log.width, self.log.height);
        let (gx, gy) = (&self.gradient_x, &self.gradient_y);
        let xx = Plane::from_fn(width, height, |x, y| gx.at(x, y).powi(2)).blurred(STRUCTURE_BLUR);
        let yy = Plane::from_fn(width, height, |x, y| gy.at(x, y).powi(2)).blurred(STRUCTURE_BLUR);
        let xy = Plane::from_fn(width, height, |x, y| gx.at(x, y) * gy.at(x, y)).blurred(STRUCTURE_BLUR);
        Plane::from_fn(width, height, |x, y| {
            let (a, b, c) = (xx.at(x, y), yy.at(x, y), xy.at(x, y));
            0.5 * (a + b - ((a - b).powi(2) + 4.0 * c * c).sqrt())
        })
    }

    fn orientation(&self, x: f32, y: f32) -> f32 {
        let mut bins = [0.0f32; ORIENTATION_BINS];
        let (width, height) = (self.log.width as isize, self.log.height as isize);
        let (cx, cy) = (x.round() as isize, y.round() as isize);
        for dy in -ORIENTATION_RADIUS..=ORIENTATION_RADIUS {
            for dx in -ORIENTATION_RADIUS..=ORIENTATION_RADIUS {
                let (px, py) = (cx + dx, cy + dy);
                if px < 0 || py < 0 || px >= width || py >= height {
                    continue;
                }
                let (gx, gy) = (self.gradient_x.at(px as usize, py as usize), self.gradient_y.at(px as usize, py as usize));
                let distance = (dx * dx + dy * dy) as f32;
                let weight = (-distance / (2.0 * ORIENTATION_SIGMA * ORIENTATION_SIGMA)).exp() * gx.hypot(gy);
                let turn = gy.atan2(gx) / std::f32::consts::TAU + 1.0;
                let bin = (turn * ORIENTATION_BINS as f32) as usize % ORIENTATION_BINS;
                bins[bin] += weight;
            }
        }
        let peak = (0..ORIENTATION_BINS).max_by(|&a, &b| bins[a].total_cmp(&bins[b])).unwrap_or(0);
        let (before, at, after) = (
            bins[(peak + ORIENTATION_BINS - 1) % ORIENTATION_BINS],
            bins[peak],
            bins[(peak + 1) % ORIENTATION_BINS],
        );
        let curve = before - 2.0 * at + after;
        let offset = if curve.abs() > 1e-9 { 0.5 * (before - after) / curve } else { 0.0 };
        (peak as f32 + 0.5 + offset) / ORIENTATION_BINS as f32 * std::f32::consts::TAU
    }

    fn describe(&self, x: f32, y: f32, angle: f32) -> Option<Box<[f32; DESCRIPTOR]>> {
        let half = CELLS_ACROSS as f32 * CELL_PIXELS * 0.5;
        let reach = half * std::f32::consts::SQRT_2 + 1.0;
        let (width, height) = (self.log.width as f32, self.log.height as f32);
        if x < reach || y < reach || x >= width - reach || y >= height - reach {
            return None;
        }
        let (sin, cos) = angle.sin_cos();
        let mut descriptor = Box::new([0.0f32; DESCRIPTOR]);
        let steps = (half * 2.0) as isize;
        for row in 0..steps {
            for column in 0..steps {
                let (local_x, local_y) = (column as f32 + 0.5 - half, row as f32 + 0.5 - half);
                let (sample_x, sample_y) = (x + cos * local_x - sin * local_y, y + sin * local_x + cos * local_y);
                let (gx, gy) = (self.gradient_x.bilinear(sample_x, sample_y), self.gradient_y.bilinear(sample_x, sample_y));
                let magnitude = gx.hypot(gy);
                if magnitude <= 0.0 {
                    continue;
                }
                let falloff = (-(local_x * local_x + local_y * local_y) / (2.0 * half * half)).exp();
                let direction = (gy.atan2(gx) - angle).rem_euclid(std::f32::consts::TAU);
                let bin_float = direction / std::f32::consts::TAU * DIRECTIONS as f32;
                let cell_x = ((column as f32 + 0.5) / CELL_PIXELS - 0.5).clamp(0.0, (CELLS_ACROSS - 1) as f32);
                let cell_y = ((row as f32 + 0.5) / CELL_PIXELS - 0.5).clamp(0.0, (CELLS_ACROSS - 1) as f32);
                let (left, top) = (cell_x as usize, cell_y as usize);
                let (along, down) = (cell_x - left as f32, cell_y - top as f32);
                let (right, bottom) = ((left + 1).min(CELLS_ACROSS - 1), (top + 1).min(CELLS_ACROSS - 1));
                let lower = bin_float as usize % DIRECTIONS;
                let upper = (lower + 1) % DIRECTIONS;
                let turn = bin_float.fract();
                let weight = magnitude * falloff;
                for (cell_row, row_weight) in [(top, 1.0 - down), (bottom, down)] {
                    for (cell_column, column_weight) in [(left, 1.0 - along), (right, along)] {
                        let cell = (cell_row * CELLS_ACROSS + cell_column) * DIRECTIONS;
                        let share = weight * row_weight * column_weight;
                        descriptor[cell + lower] += share * (1.0 - turn);
                        descriptor[cell + upper] += share * turn;
                    }
                }
            }
        }
        normalize(&mut descriptor)?;
        descriptor.iter_mut().for_each(|value| *value = value.min(DESCRIPTOR_CLIP));
        normalize(&mut descriptor)?;
        Some(descriptor)
    }
}

fn normalize(values: &mut [f32; DESCRIPTOR]) -> Option<()> {
    let length = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if length < 1e-9 {
        return None;
    }
    values.iter_mut().for_each(|value| *value /= length);
    Some(())
}

/// Fits a parabola through three samples and returns where its peak lies, relative to the
/// middle one. This is what gets feature positions below a pixel.
fn subpixel_peak(before: f32, at: f32, after: f32) -> f32 {
    let curve = before - 2.0 * at + after;
    if curve >= 0.0 { 0.0 } else { (0.5 * (before - after) / curve).clamp(-0.5, 0.5) }
}

struct Candidate {
    x: f32,
    y: f32,
    response: f32,
}

fn local_peaks(corners: &Plane) -> Vec<Candidate> {
    let (width, height) = (corners.width, corners.height);
    let rows: Vec<usize> = (2..height.saturating_sub(2)).collect();
    rows.par_iter()
        .flat_map_iter(|&y| {
            (2..width.saturating_sub(2)).filter_map(move |x| {
                let value = corners.at(x, y);
                if value <= 0.0 {
                    return None;
                }
                for dy in 0..5 {
                    for dx in 0..5 {
                        if (dx, dy) != (2, 2) && corners.at(x + dx - 2, y + dy - 2) >= value {
                            return None;
                        }
                    }
                }
                let along_x = subpixel_peak(corners.at(x - 1, y), value, corners.at(x + 1, y));
                let along_y = subpixel_peak(corners.at(x, y - 1), value, corners.at(x, y + 1));
                Some(Candidate { x: x as f32 + along_x, y: y as f32 + along_y, response: value })
            })
        })
        .collect()
}

/// Keeps the strongest candidates of each grid cell, up to `budget` in all.
fn spread(mut candidates: Vec<Candidate>, width: usize, height: usize, budget: usize) -> Vec<Candidate> {
    let strongest = candidates.iter().map(|candidate| candidate.response).fold(0.0, f32::max);
    candidates.retain(|candidate| candidate.response >= strongest * WEAKEST);
    candidates.sort_by(|a, b| b.response.total_cmp(&a.response));
    let mut used = vec![0usize; GRID_COLUMNS * GRID_ROWS];
    let mut ranked: Vec<(usize, Candidate)> = candidates
        .into_iter()
        .map(|candidate| {
            let column = ((candidate.x / width as f32 * GRID_COLUMNS as f32) as usize).min(GRID_COLUMNS - 1);
            let row = ((candidate.y / height as f32 * GRID_ROWS as f32) as usize).min(GRID_ROWS - 1);
            let cell = row * GRID_COLUMNS + column;
            used[cell] += 1;
            (used[cell], candidate)
        })
        .collect();
    // Every cell's best, then every cell's second best, and so on until the budget runs out.
    ranked.sort_by_key(|entry| entry.0);
    ranked.truncate(budget);
    ranked.into_iter().map(|(_, candidate)| candidate).collect()
}

/// Features of a linear image, in its pixels, at most about `budget` of them. `top` is the
/// level treated as white, which fixes where log luma bottoms out.
pub fn detect_features(image: &Image, top: f32, budget: usize) -> Vec<Feature> {
    let top = top.max(1e-6);
    let log = image.plane(|pixel| (luma(pixel).max(0.0) / top + LOG_FLOOR).ln());
    let mut octaves = vec![Octave::new(log.blurred(0.8), 1.0)];
    for octave in 1..OCTAVES {
        let next = octaves[octave - 1].log.halved().blurred(0.8);
        if next.width.min(next.height) < 48 {
            break;
        }
        octaves.push(Octave::new(next, (1 << octave) as f32));
    }
    let share = budget.div_ceil(octaves.len());
    let mut features: Vec<Feature> = Vec::with_capacity(budget);
    for octave in &octaves {
        let peaks = spread(local_peaks(&octave.corners()), octave.log.width, octave.log.height, share);
        let found: Vec<Feature> = peaks
            .par_iter()
            .filter_map(|candidate| {
                let angle = octave.orientation(candidate.x, candidate.y);
                let descriptor = octave.describe(candidate.x, candidate.y, angle)?;
                let to_base = |value: f32| (value + 0.5) * octave.scale - 0.5;
                Some(Feature { x: to_base(candidate.x), y: to_base(candidate.y), scale: octave.scale, angle, descriptor })
            })
            .collect();
        features.extend(found);
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(x: f32, y: f32) -> f32 {
        let cell = |value: f32, size: f32| (value / size).floor() as i32;
        let block = (cell(x, 37.0) * 7 + cell(y, 53.0) * 13).rem_euclid(5) as f32;
        let fine = (cell(x, 11.0) * 3 + cell(y, 13.0) * 5).rem_euclid(3) as f32;
        0.02 + 0.08 * block + 0.03 * fine
    }

    fn view(width: usize, height: usize, gain: f32, shift: [f32; 2], degrees: f32) -> Image {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (half_x, half_y) = (width as f32 * 0.5, height as f32 * 0.5);
        Image::from_fn(width, height, |x, y| {
            let (cx, cy) = (x as f32 - half_x, y as f32 - half_y);
            let sx = cos * cx - sin * cy + shift[0] + 500.0;
            let sy = sin * cx + cos * cy + shift[1] + 500.0;
            [pattern(sx, sy) * gain; 3]
        })
    }

    #[test]
    fn a_flat_image_has_no_features() {
        let flat = Image::from_fn(128, 128, |_, _| [0.2; 3]);
        assert!(detect_features(&flat, 1.0, 500).is_empty());
    }

    #[test]
    fn descriptors_are_unit_length() {
        let image = view(320, 240, 1.0, [0.0, 0.0], 0.0);
        let features = detect_features(&image, 1.0, 400);
        assert!(features.len() > 100, "{}", features.len());
        for feature in features.iter().take(50) {
            let length: f32 = feature.descriptor.iter().map(|value| value * value).sum();
            assert!((length - 1.0).abs() < 1e-3, "{length}");
        }
    }

    #[test]
    fn features_spread_across_the_frame() {
        let image = view(320, 240, 1.0, [0.0, 0.0], 0.0);
        let features = detect_features(&image, 1.0, 400);
        let (mut left, mut right) = (0, 0);
        for feature in &features {
            if feature.x < 160.0 { left += 1 } else { right += 1 }
        }
        assert!(left * 2 > right && right * 2 > left, "{left} {right}");
    }

    #[test]
    fn corners_are_found_in_the_same_place_at_another_exposure() {
        let bright = detect_features(&view(320, 240, 2.0, [0.0, 0.0], 0.0), 1.0, 400);
        let dark = detect_features(&view(320, 240, 0.5, [0.0, 0.0], 0.0), 1.0, 400);
        let repeated = bright
            .iter()
            .filter(|a| dark.iter().any(|b| (a.x - b.x).hypot(a.y - b.y) < 1.0))
            .count();
        assert!(repeated * 10 > bright.len() * 8, "{repeated} of {}", bright.len());
    }

    #[test]
    fn parabola_finds_a_peak_between_samples() {
        assert!((subpixel_peak(1.0, 2.0, 1.5) - 0.1666).abs() < 1e-3);
        assert_eq!(subpixel_peak(1.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn the_budget_is_respected() {
        let image = view(640, 480, 1.0, [0.0, 0.0], 0.0);
        let features = detect_features(&image, 1.0, 200);
        assert!(features.len() <= 200, "{}", features.len());
    }
}
