//! Measures a photo's noise from its own raw data, so denoising needs no per-camera profile.
//! Sensor noise follows `variance = shot · signal + read`. Flat areas show that relationship
//! directly, and texture only ever adds to it, so the quietest blocks at each brightness trace
//! the curve.

use rayon::prelude::*;
use unlight_raw::{RawImage, merged::f32_from_half};

const BLOCK_EDGE: usize = 24;
const TARGET_BLOCKS: usize = 5000;
const BINS: usize = 40;
const MIN_BIN_BLOCKS: usize = 5;
/// A block with any site this close to white says nothing, because clipping hides its noise.
const NEAR_CLIP: f32 = 0.96;
const QUIET_PERCENTILE: f64 = 0.2;
/// How far above the rough fit a bin can sit and still count as noise rather than texture.
const OUTLIER: f32 = 1.6;
/// The quiet percentile reads a block's variance low by this much, even on a flat image.
const QUIET_BIAS: f32 = 0.8;
/// A second difference `s - (a + b) / 2` of white noise has 1.5 times its variance.
const SECOND_DIFFERENCE_GAIN: f32 = 1.5;
/// Photos without deep shadows can't reveal their read noise. Assuming none would switch
/// denoising off in exactly the areas that need it most, so it's tied to the shot noise.
const READ_FLOOR: f32 = 0.003;
/// Median absolute value of a unit Gaussian.
const GAUSSIAN_MEDIAN: f32 = 0.6745;

/// Noise variance at a signal, both in raw units where black is 0 and white is 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NoiseModel {
    pub shot: f32,
    pub read: f32,
}

impl NoiseModel {
    pub fn variance(&self, signal: f32) -> f32 {
        self.shot * signal.max(0.0) + self.read
    }
}

#[derive(Clone, Copy)]
struct Sample {
    signal: f32,
    variance: f32,
}

fn median_absolute(values: &mut [f32]) -> f32 {
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f32::total_cmp).1
}

fn robust_variance(residuals: &mut [f32]) -> f32 {
    let spread = median_absolute(residuals) / GAUSSIAN_MEDIAN;
    spread * spread / SECOND_DIFFERENCE_GAIN
}

/// One sample per colour for a block. Residuals are taken across one CFA period, along rows
/// and along columns, and the quieter direction wins so that a straight edge doesn't count.
fn measure_block(raw: &RawImage, left: usize, top: usize, buffers: &mut [Vec<f32>; 6], samples: &mut Vec<Sample>) {
    let width = raw.width as usize;
    let period = raw.cfa.period as usize;
    let step = [1, width];
    let site = |x: usize, y: usize| f32::from(raw.samples[y * width + x]);
    let scale = |x: usize, y: usize| {
        let black = raw.black[y % 6][x % 6];
        (black, 1.0 / (raw.white - black))
    };
    let mut sums = [0.0f32; 3];
    let mut counts = [0u32; 3];
    buffers.iter_mut().for_each(Vec::clear);
    for y in top..top + BLOCK_EDGE {
        for x in left..left + BLOCK_EDGE {
            let channel = raw.cfa.color_at(x as u32, y as u32) as usize;
            let (black, gain) = scale(x, y);
            let value = (site(x, y) - black) * gain;
            if value >= NEAR_CLIP {
                return;
            }
            sums[channel] += value;
            counts[channel] += 1;
            let index = y * width + x;
            for (direction, offset) in step.iter().map(|stride| stride * period).enumerate() {
                let neighbours = f32::from(raw.samples[index - offset]) + f32::from(raw.samples[index + offset]);
                let residual = (site(x, y) - neighbours * 0.5) * gain;
                buffers[channel * 2 + direction].push(residual.abs());
            }
        }
    }
    for channel in 0..3 {
        if counts[channel] == 0 {
            continue;
        }
        let (rows, columns) = {
            let (head, tail) = buffers.split_at_mut(channel * 2 + 1);
            (&mut head[channel * 2], &mut tail[0])
        };
        let variance = robust_variance(rows).min(robust_variance(columns));
        samples.push(Sample { signal: sums[channel] / counts[channel] as f32, variance });
    }
}

fn collect_samples(raw: &RawImage) -> Vec<Sample> {
    let (width, height) = (raw.width as usize, raw.height as usize);
    let margin = raw.cfa.period as usize;
    if width < BLOCK_EDGE + 2 * margin || height < BLOCK_EDGE + 2 * margin {
        return Vec::new();
    }
    let usable = (width - 2 * margin - BLOCK_EDGE, height - 2 * margin - BLOCK_EDGE);
    let area = (usable.0 * usable.1) as f64;
    let stride = ((area / TARGET_BLOCKS as f64).sqrt() as usize).max(BLOCK_EDGE);
    let tops: Vec<usize> = (margin..margin + usable.1).step_by(stride).collect();
    tops.par_iter()
        .flat_map_iter(|&top| {
            let mut buffers: [Vec<f32>; 6] = Default::default();
            let mut samples = Vec::new();
            for left in (margin..margin + usable.0).step_by(stride) {
                measure_block(raw, left, top, &mut buffers, &mut samples);
            }
            samples
        })
        .collect()
}

/// Noise in one block of a merged photo's green channel, which has no sites to keep apart.
/// Residuals span two pixels because demosaicing has smoothed neighbouring ones together.
fn measure_green_block(raw: &RawImage, left: usize, top: usize, buffers: &mut [Vec<f32>; 2]) -> Option<Sample> {
    const STEP: usize = 2;
    const GREEN: usize = 1;
    let width = raw.width as usize;
    let at = |x: usize, y: usize| f32_from_half(raw.samples[(y * width + x) * 4 + GREEN]);
    let mut sum = 0.0;
    buffers.iter_mut().for_each(Vec::clear);
    for y in top..top + BLOCK_EDGE {
        for x in left..left + BLOCK_EDGE {
            let value = at(x, y);
            // Near clipping hides noise, and zero is where a panorama has no photo at all.
            if value >= NEAR_CLIP || value <= 0.0 {
                return None;
            }
            sum += value;
            buffers[0].push((value - (at(x - STEP, y) + at(x + STEP, y)) * 0.5).abs());
            buffers[1].push((value - (at(x, y - STEP) + at(x, y + STEP)) * 0.5).abs());
        }
    }
    let [rows, columns] = buffers;
    let variance = robust_variance(rows).min(robust_variance(columns));
    Some(Sample { signal: sum / (BLOCK_EDGE * BLOCK_EDGE) as f32, variance })
}

fn collect_green_samples(raw: &RawImage) -> Vec<Sample> {
    const MARGIN: usize = 2;
    let (width, height) = (raw.width as usize, raw.height as usize);
    if width < BLOCK_EDGE + 2 * MARGIN || height < BLOCK_EDGE + 2 * MARGIN {
        return Vec::new();
    }
    let usable = (width - 2 * MARGIN - BLOCK_EDGE, height - 2 * MARGIN - BLOCK_EDGE);
    let area = (usable.0 * usable.1) as f64;
    let stride = ((area / TARGET_BLOCKS as f64).sqrt() as usize).max(BLOCK_EDGE);
    let tops: Vec<usize> = (MARGIN..MARGIN + usable.1).step_by(stride).collect();
    tops.par_iter()
        .flat_map_iter(|&top| {
            let mut buffers: [Vec<f32>; 2] = Default::default();
            (MARGIN..MARGIN + usable.0)
                .step_by(stride)
                .filter_map(|left| measure_green_block(raw, left, top, &mut buffers))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The quiet end of each brightness bin, as (signal, variance) points.
fn noise_floor(samples: &[Sample]) -> Vec<(f32, f32)> {
    let mut bins: Vec<Vec<Sample>> = vec![Vec::new(); BINS];
    for sample in samples {
        let position = sample.signal.max(0.0).sqrt() * BINS as f32;
        bins[(position as usize).min(BINS - 1)].push(*sample);
    }
    bins.into_iter()
        .filter(|bin| bin.len() >= MIN_BIN_BLOCKS)
        .map(|mut bin| {
            let rank = ((bin.len() - 1) as f64 * QUIET_PERCENTILE) as usize;
            bin.select_nth_unstable_by(rank, |a, b| a.variance.total_cmp(&b.variance));
            let quiet = bin[rank].variance / QUIET_BIAS;
            let signal = bin.iter().map(|sample| sample.signal).sum::<f32>() / bin.len() as f32;
            (signal, quiet)
        })
        .collect()
}

fn median(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f32::total_cmp).1
}

/// The median of every pairwise slope, which stays on the curve even with several bins of pure
/// texture. A bright bin in a busy photo can easily have no flat blocks at all.
fn robust_line(points: &[(f32, f32)]) -> NoiseModel {
    let mut slopes = Vec::with_capacity(points.len() * points.len() / 2);
    for (index, a) in points.iter().enumerate() {
        for b in &points[index + 1..] {
            if (b.0 - a.0).abs() > 1e-4 {
                slopes.push((b.1 - a.1) / (b.0 - a.0));
            }
        }
    }
    let shot = median(slopes).max(0.0);
    let read = median(points.iter().map(|point| point.1 - shot * point.0).collect()).max(0.0);
    NoiseModel { shot, read }
}

/// Least squares on relative error, so dark bins, where the variance is small, count as much
/// as bright ones. Starts from the robust line and leaves out what's far above it.
fn fit_line(points: &[(f32, f32)]) -> NoiseModel {
    let rough = robust_line(points);
    let inliers: Vec<(f32, f32)> =
        points.iter().copied().filter(|&(signal, variance)| variance <= rough.variance(signal) * OUTLIER).collect();
    let floor = points.iter().map(|point| point.1).fold(f32::MAX, f32::min).max(1e-12);
    let mut model = rough;
    for _ in 0..3 {
        let (mut ss, mut s, mut w, mut sv, mut v) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for &(signal, variance) in &inliers {
            let weight = 1.0 / f64::from(model.variance(signal).max(floor)).powi(2);
            let (signal, variance) = (f64::from(signal), f64::from(variance));
            ss += weight * signal * signal;
            s += weight * signal;
            w += weight;
            sv += weight * signal * variance;
            v += weight * variance;
        }
        let determinant = ss * w - s * s;
        if determinant.abs() < 1e-30 {
            return model;
        }
        let shot = ((sv * w - s * v) / determinant).max(0.0);
        let read = ((v - shot * s) / w).max(0.0);
        model = NoiseModel { shot: shot as f32, read: read as f32 };
    }
    model
}

/// Fits the photo's noise curve. A noiseless image gives a zero model, which disables denoising.
pub fn estimate(raw: &RawImage) -> NoiseModel {
    let samples = if raw.cfa.is_full_colour() { collect_green_samples(raw) } else { collect_samples(raw) };
    let points = noise_floor(&samples);
    if points.is_empty() {
        return NoiseModel::default();
    }
    let model = fit_line(&points);
    NoiseModel { read: model.read.max(model.shot * READ_FLOOR), ..model }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_raw::{Cfa, ColorData, Metadata, Opcodes};

    struct Random(u64);

    impl Random {
        fn uniform(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }

        fn gaussian(&mut self) -> f32 {
            let radius = (-2.0 * self.uniform().max(1e-7).ln()).sqrt();
            radius * (self.uniform() * std::f32::consts::TAU).cos()
        }
    }

    fn bayer() -> Cfa {
        let mut pattern = [[1u8; 6]; 6];
        for (row, sites) in pattern.iter_mut().enumerate() {
            for (col, site) in sites.iter_mut().enumerate() {
                *site = match (row % 2, col % 2) {
                    (0, 0) => 0,
                    (1, 1) => 2,
                    _ => 1,
                };
            }
        }
        Cfa { period: 2, pattern }
    }

    fn raw_with(model: NoiseModel, scene: impl Fn(usize, usize) -> f32) -> RawImage {
        let (width, height) = (1600usize, 1200usize);
        let (black, white) = (64.0, 16383.0);
        let mut random = Random(0x2545_f491_4f6c_dd1d);
        let mut samples = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let signal = scene(x, y);
                let noisy = signal + random.gaussian() * model.variance(signal).sqrt();
                samples.push((black + noisy.clamp(0.0, 1.0) * (white - black)).round() as u16);
            }
        }
        RawImage {
            width: width as u32,
            height: height as u32,
            samples,
            cfa: bayer(),
            black: [[black; 6]; 6],
            white,
            color: ColorData {
                xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                as_shot_multipliers: [1.0; 3],
                daylight_multipliers: [1.0; 3],
                baseline_exposure: 0.0,
                dng: None,
            },
            opcodes: Opcodes::default(),
            metadata: Metadata::default(),
            embedded_lens: None,
        }
    }

    fn ramp(x: usize, _: usize) -> f32 {
        0.01 + x as f32 / 1600.0 * 0.85
    }

    #[test]
    fn recovers_a_known_noise_curve() {
        let truth = NoiseModel { shot: 4e-4, read: 4e-6 };
        let found = estimate(&raw_with(truth, ramp));
        assert!((found.shot / truth.shot - 1.0).abs() < 0.2, "{found:?}");
        assert!((found.read / truth.read - 1.0).abs() < 0.5, "{found:?}");
    }

    #[test]
    fn finds_nothing_in_a_clean_image() {
        let found = estimate(&raw_with(NoiseModel::default(), ramp));
        assert!(found.variance(0.5) < 1e-7, "{found:?}");
    }

    #[test]
    fn texture_barely_raises_the_estimate() {
        let truth = NoiseModel { shot: 4e-4, read: 4e-6 };
        let textured = |x: usize, y: usize| {
            let busy = (x / 256 + y / 256).is_multiple_of(3);
            let stripes = if busy && (x / 3 + y / 3).is_multiple_of(2) { 0.2 } else { 0.0 };
            0.1 + x as f32 / 1600.0 * 0.6 + stripes
        };
        let found = estimate(&raw_with(truth, textured));
        assert!(found.variance(0.5) / truth.variance(0.5) < 1.5, "{found:?}");
    }

    #[test]
    fn a_tiny_image_is_treated_as_clean() {
        let mut raw = raw_with(NoiseModel::default(), ramp);
        raw.width = 16;
        raw.height = 16;
        assert_eq!(estimate(&raw), NoiseModel::default());
    }
}
