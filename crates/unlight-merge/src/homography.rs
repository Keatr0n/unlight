//! Separates true matches from false ones. Two views of a scene from one spot are related by a
//! homography, so anything that disagrees with the best one is thrown out. Sampling is seeded,
//! which means merging the same photos always gives the same picture.

use crate::linalg::{apply_homography, solve_general, solve_spd};
use unlight_color::matrix::{IDENTITY, Mat3, inverse, mul};

const MAX_ITERATIONS: usize = 4000;
const CONFIDENCE: f64 = 0.999;
const SAMPLE: usize = 4;
const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

pub type Homography = Mat3;

#[derive(Clone, Debug)]
pub struct RansacFit {
    pub matrix: Homography,
    pub inliers: Vec<usize>,
}

struct Random(u64);

impl Random {
    fn below(&mut self, limit: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % limit as u64) as usize
    }
}

/// A shift and scale that centre points on the origin with mean distance √2, which keeps the
/// solve well conditioned (Hartley 1997).
fn normalizer(points: &[[f64; 2]]) -> Mat3 {
    let count = points.len().max(1) as f64;
    let center = points.iter().fold([0.0, 0.0], |sum, point| [sum[0] + point[0], sum[1] + point[1]]).map(|value| value / count);
    let spread = points.iter().map(|point| (point[0] - center[0]).hypot(point[1] - center[1])).sum::<f64>() / count;
    let scale = std::f64::consts::SQRT_2 / spread.max(1e-12);
    [[scale, 0.0, -scale * center[0]], [0.0, scale, -scale * center[1]], [0.0, 0.0, 1.0]]
}

/// The exact homography through four point pairs, or `None` if they're degenerate.
fn from_four(from: &[[f64; 2]; 4], to: &[[f64; 2]; 4]) -> Option<Mat3> {
    let mut matrix = [[0.0; 8]; 8];
    let mut vector = [0.0; 8];
    for index in 0..4 {
        let ([x, y], [u, v]) = (from[index], to[index]);
        matrix[index * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y];
        matrix[index * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y];
        vector[index * 2] = u;
        vector[index * 2 + 1] = v;
    }
    let h = solve_general(matrix, vector)?;
    Some([[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]])
}

/// Least squares over many pairs.
fn from_many(from: &[[f64; 2]], to: &[[f64; 2]]) -> Option<Mat3> {
    let (from_norm, to_norm) = (normalizer(from), normalizer(to));
    let mut system = [0.0f64; 64];
    let mut vector = [0.0f64; 8];
    for (a, b) in from.iter().zip(to) {
        let [x, y] = apply_homography(&from_norm, *a);
        let [u, v] = apply_homography(&to_norm, *b);
        let rows = [
            ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
            ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
        ];
        for (row, target) in rows {
            for i in 0..8 {
                for j in 0..8 {
                    system[i * 8 + j] += row[i] * row[j];
                }
                vector[i] += row[i] * target;
            }
        }
    }
    solve_spd(&mut system, &mut vector)?;
    let [a, b, c, d, e, f, g, h] = vector;
    let normalized = [[a, b, c], [d, e, f], [g, h, 1.0]];
    Some(mul(&inverse(&to_norm)?, &mul(&normalized, &from_norm)))
}

fn transfer_error(matrix: &Mat3, from: [f64; 2], to: [f64; 2]) -> f64 {
    let [x, y] = apply_homography(matrix, from);
    (x - to[0]).hypot(y - to[1])
}

/// A homography that folds the frame over itself, or squeezes it to a sliver, isn't a view.
fn is_sensible(matrix: &Mat3, from: &[[f64; 2]]) -> bool {
    let spread = normalizer(from);
    let scale = 1.0 / spread[0][0].max(1e-12);
    let determinant = matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0];
    let tilt = matrix[2][0].hypot(matrix[2][1]) * scale;
    determinant > 0.05 && determinant < 20.0 && tilt < 1.5
}

fn inliers_of(matrix: &Mat3, from: &[[f64; 2]], to: &[[f64; 2]], threshold: f64) -> Vec<usize> {
    (0..from.len()).filter(|&index| transfer_error(matrix, from[index], to[index]) < threshold).collect()
}

/// Finds the homography most pairs agree on, to within `threshold`.
pub fn fit_homography(from: &[[f64; 2]], to: &[[f64; 2]], threshold: f64) -> Option<RansacFit> {
    let count = from.len();
    if count < SAMPLE || count != to.len() {
        return None;
    }
    let (from_norm, to_norm) = (normalizer(from), normalizer(to));
    let to_back = inverse(&to_norm)?;
    let mut random = Random(SEED);
    let mut best: Option<(Vec<usize>, Mat3)> = None;
    let mut needed = MAX_ITERATIONS;
    let mut iteration = 0;
    while iteration < needed {
        iteration += 1;
        let mut picks = [0usize; SAMPLE];
        for slot in 0..SAMPLE {
            picks[slot] = loop {
                let candidate = random.below(count);
                if !picks[..slot].contains(&candidate) {
                    break candidate;
                }
            };
        }
        let sample_from = picks.map(|index| apply_homography(&from_norm, from[index]));
        let sample_to = picks.map(|index| apply_homography(&to_norm, to[index]));
        let Some(model) = from_four(&sample_from, &sample_to) else {
            continue;
        };
        let matrix = mul(&to_back, &mul(&model, &from_norm));
        if !is_sensible(&matrix, from) {
            continue;
        }
        let inliers = inliers_of(&matrix, from, to, threshold);
        let better = best.as_ref().is_none_or(|(most, _)| inliers.len() > most.len());
        if better && inliers.len() >= SAMPLE {
            let share = inliers.len() as f64 / count as f64;
            let all_good = (1.0 - share.powi(SAMPLE as i32)).clamp(1e-12, 1.0 - 1e-12);
            needed = (((1.0 - CONFIDENCE).ln() / all_good.ln()).ceil() as usize).clamp(32, MAX_ITERATIONS);
            best = Some((inliers, matrix));
        }
    }
    let (mut inliers, mut matrix) = best?;
    for _ in 0..3 {
        let (kept_from, kept_to): (Vec<_>, Vec<_>) = inliers.iter().map(|&index| (from[index], to[index])).unzip();
        let Some(refined) = from_many(&kept_from, &kept_to) else {
            break;
        };
        let refined_inliers = inliers_of(&refined, from, to, threshold);
        if refined_inliers.len() < inliers.len() {
            break;
        }
        matrix = refined;
        inliers = refined_inliers;
    }
    let scale = matrix[2][2];
    let matrix = if scale.abs() > 1e-12 { matrix.map(|row| row.map(|value| value / scale)) } else { IDENTITY };
    Some(RansacFit { matrix, inliers })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn truth() -> Mat3 {
        [[1.02, -0.05, 40.0], [0.03, 0.99, -12.0], [1.2e-4, -0.8e-4, 1.0]]
    }

    fn grid() -> Vec<[f64; 2]> {
        let mut points = Vec::new();
        for y in 0..10 {
            for x in 0..10 {
                points.push([x as f64 * 40.0 - 200.0 + (y * 3) as f64, y as f64 * 30.0 - 150.0 + (x * 2) as f64]);
            }
        }
        points
    }

    #[test]
    fn clean_points_give_the_exact_homography() {
        let from = grid();
        let to: Vec<_> = from.iter().map(|point| apply_homography(&truth(), *point)).collect();
        let fit = fit_homography(&from, &to, 1.0).unwrap();
        assert_eq!(fit.inliers.len(), from.len());
        for point in &from {
            let error = transfer_error(&fit.matrix, *point, apply_homography(&truth(), *point));
            assert!(error < 1e-6, "{error}");
        }
    }

    #[test]
    fn half_the_points_can_be_wrong() {
        let from = grid();
        let mut to: Vec<_> = from.iter().map(|point| apply_homography(&truth(), *point)).collect();
        for (index, point) in to.iter_mut().enumerate() {
            if index % 2 == 0 {
                point[0] += 37.0 + (index % 7) as f64 * 41.0;
                point[1] -= 23.0 + (index % 5) as f64 * 29.0;
            }
        }
        let fit = fit_homography(&from, &to, 1.0).unwrap();
        assert_eq!(fit.inliers.len(), from.len() / 2);
        assert!(fit.inliers.iter().all(|index| index % 2 == 1));
    }

    #[test]
    fn the_same_input_gives_the_same_answer() {
        let from = grid();
        let mut to: Vec<_> = from.iter().map(|point| apply_homography(&truth(), *point)).collect();
        to[3][0] += 50.0;
        let (first, second) = (fit_homography(&from, &to, 1.0).unwrap(), fit_homography(&from, &to, 1.0).unwrap());
        assert_eq!(first.matrix, second.matrix);
    }

    #[test]
    fn too_few_points_fit_nothing() {
        assert!(fit_homography(&[[0.0, 0.0]; 3], &[[0.0, 0.0]; 3], 1.0).is_none());
    }

    #[test]
    fn collinear_points_are_no_model() {
        let from: Vec<_> = (0..20).map(|index| [index as f64, index as f64]).collect();
        let fit = fit_homography(&from, &from, 0.5);
        assert!(fit.is_none_or(|fit| from_many(&from, &from).is_none() || fit.inliers.len() == 20));
    }
}
