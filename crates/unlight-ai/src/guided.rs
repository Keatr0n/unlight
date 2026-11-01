//! The colour guided filter of He, Sun and Tang. Models see a small image, so their masks are
//! soft and misplaced by a few pixels. This snaps a mask onto the edges of the full picture,
//! which is what brings back hair and tree lines.

use crate::image::RgbImage;
use rayon::prelude::*;

/// A window-mean of every product the local linear model needs, at one pixel.
const GUIDE_MOMENTS: usize = 13;

/// Mean over a square of radius `radius` around each pixel, shrinking at the borders.
pub(crate) fn box_mean(values: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let mut rows = vec![0.0; values.len()];
    for (input, output) in values.chunks_exact(width).zip(rows.chunks_exact_mut(width)) {
        let mut sum: f64 = input[..(radius + 1).min(width)].iter().map(|&value| f64::from(value)).sum();
        for x in 0..width {
            let count = (x + radius + 1).min(width) - x.saturating_sub(radius);
            output[x] = (sum / count as f64) as f32;
            if x + radius + 1 < width {
                sum += f64::from(input[x + radius + 1]);
            }
            if x >= radius {
                sum -= f64::from(input[x - radius]);
            }
        }
    }

    let mut columns = vec![0.0f64; width];
    for row in rows.chunks_exact(width).take(radius + 1) {
        columns.iter_mut().zip(row).for_each(|(sum, &value)| *sum += f64::from(value));
    }
    let mut result = vec![0.0; values.len()];
    for y in 0..height {
        let count = ((y + radius + 1).min(height) - y.saturating_sub(radius)) as f64;
        let output = &mut result[y * width..(y + 1) * width];
        output.iter_mut().zip(&columns).for_each(|(value, sum)| *value = (sum / count) as f32);
        if y + radius + 1 < height {
            let entering = &rows[(y + radius + 1) * width..(y + radius + 2) * width];
            columns.iter_mut().zip(entering).for_each(|(sum, &value)| *sum += f64::from(value));
        }
        if y >= radius {
            let leaving = &rows[(y - radius) * width..(y - radius + 1) * width];
            columns.iter_mut().zip(leaving).for_each(|(sum, &value)| *sum -= f64::from(value));
        }
    }
    result
}

/// Solves (covariance + epsilon·I)·a = correlation for a symmetric 3×3 covariance.
fn solve([xx, xy, xz, yy, yz, zz]: [f32; 6], [p, q, r]: [f32; 3], epsilon: f32) -> [f32; 3] {
    let (xx, yy, zz) = (xx + epsilon, yy + epsilon, zz + epsilon);
    let (c00, c01, c02) = (yy * zz - yz * yz, xz * yz - xy * zz, xy * yz - xz * yy);
    let (c11, c12, c22) = (xx * zz - xz * xz, xy * xz - xx * yz, xx * yy - xy * xy);
    let scale = 1.0 / (xx * c00 + xy * c01 + xz * c02);
    [
        (c00 * p + c01 * q + c02 * r) * scale,
        (c01 * p + c11 * q + c12 * r) * scale,
        (c02 * p + c12 * q + c22 * r) * scale,
    ]
}

/// Refines `input` so its edges follow `guide`. A larger `radius` reaches further for an edge;
/// a smaller `epsilon` follows weaker ones.
pub fn guided_filter(guide: &RgbImage, input: &[f32], radius: usize, epsilon: f32) -> Vec<f32> {
    let (width, height) = (guide.width as usize, guide.height as usize);
    let channel = |index: usize| -> Vec<f32> {
        guide.pixels.iter().skip(index).step_by(3).map(|&value| f32::from(value) / 255.0).collect()
    };
    let (red, green, blue) = (channel(0), channel(1), channel(2));
    let product = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(x, y)| x * y).collect() };

    let fields: [Vec<f32>; GUIDE_MOMENTS] = [
        red.clone(),
        green.clone(),
        blue.clone(),
        input.to_vec(),
        product(&red, &red),
        product(&red, &green),
        product(&red, &blue),
        product(&green, &green),
        product(&green, &blue),
        product(&blue, &blue),
        product(&red, input),
        product(&green, input),
        product(&blue, input),
    ];
    let means: Vec<Vec<f32>> =
        fields.par_iter().map(|field| box_mean(field, width, height, radius)).collect();
    let mean = |index: usize, pixel: usize| means[index][pixel];

    let mut coefficients = vec![[0.0f32; 4]; width * height];
    coefficients.par_iter_mut().enumerate().for_each(|(pixel, out)| {
        let [mr, mg, mb, mp] = [0, 1, 2, 3].map(|index| mean(index, pixel));
        let covariance = [
            mean(4, pixel) - mr * mr,
            mean(5, pixel) - mr * mg,
            mean(6, pixel) - mr * mb,
            mean(7, pixel) - mg * mg,
            mean(8, pixel) - mg * mb,
            mean(9, pixel) - mb * mb,
        ];
        let correlation = [mean(10, pixel) - mr * mp, mean(11, pixel) - mg * mp, mean(12, pixel) - mb * mp];
        let [a, b, c] = solve(covariance, correlation, epsilon);
        *out = [a, b, c, mp - a * mr - b * mg - c * mb];
    });

    let plane = |index: usize| -> Vec<f32> { coefficients.iter().map(|value| value[index]).collect() };
    let smoothed: Vec<Vec<f32>> =
        (0..4).into_par_iter().map(|index| box_mean(&plane(index), width, height, radius)).collect();
    (0..width * height)
        .into_par_iter()
        .map(|pixel| {
            smoothed[0][pixel] * red[pixel]
                + smoothed[1][pixel] * green[pixel]
                + smoothed[2][pixel] * blue[pixel]
                + smoothed[3][pixel]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step_guide(width: u32, height: u32, edge: u32) -> RgbImage {
        let pixels = (0..height)
            .flat_map(|_| (0..width).flat_map(move |x| [if x < edge { 30 } else { 220 }; 3]))
            .collect();
        RgbImage { width, height, pixels }
    }

    #[test]
    fn box_mean_averages_a_window_and_shrinks_at_borders() {
        let result = box_mean(&[1.0, 2.0, 3.0, 4.0, 5.0], 5, 1, 1);
        assert_eq!(result, [1.5, 2.0, 3.0, 4.0, 4.5]);
        let tall = box_mean(&[1.0, 2.0, 3.0, 4.0, 5.0], 1, 5, 1);
        assert_eq!(tall, [1.5, 2.0, 3.0, 4.0, 4.5]);
    }

    #[test]
    fn box_mean_matches_brute_force_on_a_noisy_field() {
        let (width, height, radius) = (23usize, 17usize, 4usize);
        let values: Vec<f32> = (0..width * height).map(|i| ((i * 2654435761) % 1000) as f32 / 1000.0).collect();
        let fast = box_mean(&values, width, height, radius);
        for y in 0..height {
            for x in 0..width {
                let (mut sum, mut count) = (0.0, 0.0);
                for v in y.saturating_sub(radius)..(y + radius + 1).min(height) {
                    for u in x.saturating_sub(radius)..(x + radius + 1).min(width) {
                        sum += values[v * width + u];
                        count += 1.0;
                    }
                }
                assert!((fast[y * width + x] - sum / count).abs() < 1e-4, "at {x},{y}");
            }
        }
    }

    #[test]
    fn a_blurry_edge_snaps_onto_the_edge_in_the_guide() {
        let (width, height, edge) = (64u32, 16u32, 32u32);
        let guide = step_guide(width, height, edge);
        let blurry: Vec<f32> = (0..height)
            .flat_map(|_| (0..width).map(|x| ((x as f32 - 24.0) / 16.0).clamp(0.0, 1.0)))
            .collect();
        let sharp = guided_filter(&guide, &blurry, 8, 1e-4);
        let row = |values: &[f32], x: u32| values[(8 * width + x) as usize];
        assert!(row(&sharp, edge - 2) < row(&blurry, edge - 2), "left of the edge should fall");
        assert!(row(&sharp, edge + 2) > row(&blurry, edge + 2), "right of the edge should rise");
        let input_slope = row(&blurry, edge) - row(&blurry, edge - 1);
        let step = row(&sharp, edge) - row(&sharp, edge - 1);
        assert!(step > 7.0 * input_slope, "the transition should land on the edge pixel, got {step}");
    }

    #[test]
    fn a_flat_mask_stays_flat_whatever_the_guide_shows() {
        let guide = step_guide(32, 8, 16);
        let result = guided_filter(&guide, &[0.4; 256], 4, 1e-4);
        assert!(result.iter().all(|value| (value - 0.4).abs() < 1e-4));
    }

    #[test]
    fn a_featureless_guide_only_smooths() {
        let guide = RgbImage { width: 16, height: 16, pixels: vec![128; 16 * 16 * 3] };
        let input: Vec<f32> = (0..256).map(|i| if i % 2 == 0 { 0.0 } else { 1.0 }).collect();
        let result = guided_filter(&guide, &input, 3, 1e-4);
        assert!(result.iter().all(|value| (value - 0.5).abs() < 0.2));
    }
}
