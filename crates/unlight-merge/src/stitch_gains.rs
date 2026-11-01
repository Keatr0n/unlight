//! Makes frames agree on brightness before they're blended. Wherever two frames overlap they
//! saw the same light, so `log a + gain(a) + falloff(r_a) = log b + gain(b) + falloff(r_b)`,
//! which is linear in the unknowns. That gives each frame's exposure and also the lens's
//! vignetting, which lens profiles often lack or only partly cover (Goldman & Chen 2005). It's
//! fitted as `a·r² + b·r⁴ + c·r⁶` in log light, on a radius of 1 at the frame's corner.

use crate::linalg::solve_spd;

const TERMS: usize = 3;
/// Cauchy scale in log light, for scoring samples. Parallax and moving people make outliers.
const ROBUST_SCALE: f64 = 0.08;
const ROUNDS: usize = 8;
/// A pull towards no vignette and no gain, small enough to matter only when the data can't
/// decide. Overlaps at the same radius in both frames say nothing about falloff, for instance.
const RIDGE: f64 = 1e-3;
/// Holds the reference frame's gain at zero.
const ANCHOR: f64 = 1e4;
/// Too few samples to tell a falloff from a gain.
const MIN_VIGNETTE_SAMPLES: usize = 400;

/// Two frames' view of one point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlapSample {
    pub first: usize,
    pub second: usize,
    /// Distance from each frame's centre, with the corner at 1.
    pub radius: [f32; 2],
    /// Log light in each frame, per channel.
    pub log: [[f32; 3]; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct StitchGains {
    /// Per frame, what to multiply each channel by.
    pub gains: Vec<[f32; 3]>,
    /// Log-light correction terms, in powers of r².
    pub vignette: [f32; TERMS],
}

impl StitchGains {
    pub fn unity(count: usize) -> Self {
        Self { gains: vec![[1.0; 3]; count], vignette: [0.0; TERMS] }
    }

    /// What vignetting takes a pixel at this radius has to be multiplied by.
    #[inline]
    pub fn falloff(&self, radius: f32) -> f32 {
        let r2 = radius * radius;
        let [a, b, c] = self.vignette;
        (r2 * (a + r2 * (b + r2 * c))).exp()
    }
}

fn terms(radius: f32) -> [f64; TERMS] {
    let r2 = f64::from(radius).powi(2);
    [r2, r2 * r2, r2 * r2 * r2]
}

/// Weighted least squares over the samples, with Cauchy weights from the previous round.
fn solve(
    samples: &[OverlapSample],
    frames: usize,
    reference: usize,
    channel: usize,
    fixed_vignette: Option<[f32; TERMS]>,
) -> (Vec<f64>, [f64; TERMS]) {
    let unknowns = frames + if fixed_vignette.is_some() { 0 } else { TERMS };
    let fixed = fixed_vignette.map(|values| values.map(f64::from));
    let mut solution = vec![0.0; unknowns];
    let correction = |solution: &[f64], radius: f32| -> f64 {
        let powers = terms(radius);
        let coefficients: [f64; TERMS] =
            fixed.unwrap_or_else(|| std::array::from_fn(|term| solution[frames + term]));
        (0..TERMS).map(|term| coefficients[term] * powers[term]).sum()
    };
    for _ in 0..ROUNDS {
        let mut system = vec![0.0; unknowns * unknowns];
        let mut vector = vec![0.0; unknowns];
        for sample in samples {
            let (a, b) = (sample.first, sample.second);
            let mut target = f64::from(sample.log[1][channel] - sample.log[0][channel]);
            let mut row = vec![0.0; unknowns];
            row[a] += 1.0;
            row[b] -= 1.0;
            if fixed.is_some() {
                target += correction(&solution, sample.radius[1]) - correction(&solution, sample.radius[0]);
            } else {
                let (inner, outer) = (terms(sample.radius[0]), terms(sample.radius[1]));
                (0..TERMS).for_each(|term| row[frames + term] = inner[term] - outer[term]);
            }
            let predicted: f64 = row.iter().zip(&solution).map(|(x, y)| x * y).sum();
            let error = (target - predicted) / ROBUST_SCALE;
            let weight = 1.0 / (1.0 + error * error);
            let active: Vec<usize> = (0..unknowns).filter(|&index| row[index] != 0.0).collect();
            for &i in &active {
                for &j in &active {
                    system[i * unknowns + j] += weight * row[i] * row[j];
                }
                vector[i] += weight * row[i] * target;
            }
        }
        (0..unknowns).for_each(|index| system[index * unknowns + index] += RIDGE);
        system[reference * unknowns + reference] += ANCHOR;
        if solve_spd(&mut system, &mut vector).is_none() {
            break;
        }
        solution = vector;
    }
    let coefficients = fixed.unwrap_or_else(|| std::array::from_fn(|term| solution[frames + term]));
    solution.truncate(frames);
    (solution, coefficients)
}

/// Solves for every frame's gain, relative to `reference`, and optionally for vignetting.
pub fn solve_gains(samples: &[OverlapSample], frames: usize, reference: usize, fit_vignette: bool) -> StitchGains {
    if samples.is_empty() {
        return StitchGains::unity(frames);
    }
    let fit = fit_vignette && samples.len() >= MIN_VIGNETTE_SAMPLES;
    let fixed = (!fit).then_some([0.0; TERMS]);
    let (green, coefficients) = solve(samples, frames, reference, 1, fixed);
    let vignette = coefficients.map(|value| value as f32);
    let (red, _) = solve(samples, frames, reference, 0, Some(vignette));
    let (blue, _) = solve(samples, frames, reference, 2, Some(vignette));
    let gains = (0..frames)
        .map(|frame| [red[frame].exp() as f32, green[frame].exp() as f32, blue[frame].exp() as f32])
        .collect();
    StitchGains { gains, vignette }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOPS: [f32; 3] = [0.0, 0.6, -0.4];

    fn light(index: usize) -> f32 {
        (index as f32 * 0.37).sin() * 1.5 - 2.0
    }

    fn samples(falloff: impl Fn(f32) -> f32) -> Vec<OverlapSample> {
        let mut samples = Vec::new();
        for (first, second) in [(0, 1), (1, 2)] {
            for index in 0..2000 {
                let along = (index % 97) as f32 / 97.0;
                let radius = [0.15 + 0.7 * along, 0.9 - 0.7 * along];
                let scene = light(index);
                let seen = |frame: usize, r: f32| scene - STOPS[frame] * std::f32::consts::LN_2 - falloff(r);
                let value = |frame: usize, r: f32| [seen(frame, r); 3];
                samples.push(OverlapSample {
                    first,
                    second,
                    radius,
                    log: [value(first, radius[0]), value(second, radius[1])],
                });
            }
        }
        samples
    }

    #[test]
    fn exposure_offsets_are_recovered() {
        let found = solve_gains(&samples(|_| 0.0), 3, 0, false);
        assert_eq!(found.gains[0], [1.0; 3]);
        for (gains, expected) in found.gains.iter().zip(STOPS).skip(1) {
            let stops = gains[1].log2();
            assert!((stops - expected).abs() < 0.01, "{expected}: {stops}");
        }
    }

    #[test]
    fn vignetting_is_told_apart_from_exposure() {
        let falloff = |radius: f32| 0.7 * radius * radius;
        let found = solve_gains(&samples(falloff), 3, 0, true);
        for radius in [0.2f32, 0.5, 0.8] {
            let recovered = found.falloff(radius).ln();
            let relative = recovered - found.falloff(0.0).ln();
            assert!((relative - falloff(radius)).abs() < 0.02, "at {radius}: {relative}");
        }
        for (gains, expected) in found.gains.iter().zip(STOPS).skip(1) {
            let stops = gains[1].log2();
            assert!((stops - expected).abs() < 0.02, "{expected}: {stops}");
        }
    }

    #[test]
    fn outliers_barely_matter() {
        let mut data = samples(|_| 0.0);
        for sample in data.iter_mut().step_by(12) {
            sample.log[1][1] += 2.0;
        }
        let found = solve_gains(&data, 3, 0, false);
        let stops = found.gains[2][1].log2();
        assert!((stops - STOPS[2]).abs() < 0.03, "{stops}");
    }

    #[test]
    fn each_channel_gets_its_own_gain() {
        let mut data = samples(|_| 0.0);
        for sample in &mut data {
            if sample.second == 1 {
                sample.log[1][0] += 0.2;
            }
            if sample.first == 1 {
                sample.log[0][0] += 0.2;
            }
        }
        let found = solve_gains(&data, 3, 0, false);
        let ratio = (found.gains[1][0] / found.gains[1][1]).ln();
        assert!((ratio + 0.2).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn no_samples_change_nothing() {
        let found = solve_gains(&[], 2, 0, true);
        assert_eq!(found, StitchGains::unity(2));
        assert_eq!(found.falloff(0.7), 1.0);
    }
}
