//! Boundary Warp: stretches a panorama's ragged outline out to its bounding rectangle, so less
//! has to be cropped away. Each column is stretched between its own top and bottom edges, and
//! then each row between its left and right. The edges are smoothed first, because any kink in
//! them would turn up as a kink in straight lines all the way across the picture. The warp is
//! applied to canvas rays before frames are sampled, so it adds no second resampling.

use crate::Plane;

/// Width of the smoothing along an edge, as a share of that edge's length.
const SMOOTHING: f32 = 0.04;
/// Rounds of pulling the smoothed edge back inside the real one and smoothing again. Without
/// these, smoothing drifts past the corners of the outline and leaves them empty.
const ENVELOPE_ROUNDS: usize = 6;
/// Width of the last smoothing, as a share of `SMOOTHING`.
const FINAL_SMOOTHING: f32 = 0.5;

/// One edge's position along an axis, from 0 to 1, as a function of position along the other.
#[derive(Clone, Debug, PartialEq)]
struct Curve(Vec<f32>);

impl Curve {
    #[inline]
    fn at(&self, position: f64) -> f64 {
        let last = self.0.len() - 1;
        let place = (position.clamp(0.0, 1.0) * last as f64) as f32;
        let lower = (place as usize).min(last.saturating_sub(1));
        let along = f64::from(place - lower as f32);
        let upper = (lower + 1).min(last);
        f64::from(self.0[lower]) * (1.0 - along) + f64::from(self.0[upper]) * along
    }
}

fn blur_line(values: &[f32], sigma: f32) -> Vec<f32> {
    let width = values.len();
    let as_plane = Plane { width, height: 1, values: values.to_vec() };
    as_plane.blurred(sigma).values
}

/// Smooths an edge while keeping it on the inner side of `limit`. `sign` is 1 for an edge that
/// has to stay at or above its limit, -1 for one that has to stay at or below it.
fn inner_envelope(limit: &[f32], sigma: f32, sign: f32) -> Vec<f32> {
    let mut edge = limit.to_vec();
    for _ in 0..ENVELOPE_ROUNDS {
        edge = blur_line(&edge, sigma);
        edge.iter_mut().zip(limit).for_each(|(value, bound)| {
            *value = if sign > 0.0 { value.max(*bound) } else { value.min(*bound) };
        });
    }
    // That last clamp leaves corners, and a corner in the edge would bend every straight line
    // in the picture. Auto Crop takes care of the sliver this can let back in.
    blur_line(&edge, sigma * FINAL_SMOOTHING)
}

/// Fills unmarked entries from their nearest marked neighbours.
fn fill_gaps(values: &mut [Option<f32>]) -> Vec<f32> {
    let mut last = values.iter().flatten().next().copied().unwrap_or(0.0);
    for value in values.iter_mut() {
        match value {
            Some(known) => last = *known,
            None => *value = Some(last),
        }
    }
    values.iter().map(|value| value.unwrap_or(0.0)).collect()
}

/// The first and last covered positions of each line through a mask, from 0 to 1.
fn extents(lines: usize, length: usize, covered: impl Fn(usize, usize) -> bool) -> (Vec<f32>, Vec<f32>) {
    let mut low: Vec<Option<f32>> = Vec::with_capacity(lines);
    let mut high: Vec<Option<f32>> = Vec::with_capacity(lines);
    for line in 0..lines {
        let first = (0..length).find(|&at| covered(line, at));
        let last = (0..length).rev().find(|&at| covered(line, at));
        low.push(first.map(|at| at as f32 / length as f32));
        high.push(last.map(|at| (at + 1) as f32 / length as f32));
    }
    (fill_gaps(&mut low), fill_gaps(&mut high))
}

fn eased(edge: Vec<f32>, resting: f32, amount: f32) -> Curve {
    Curve(edge.into_iter().map(|value| resting + (value - resting) * amount).collect())
}

/// A warp fitted to one panorama's outline.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryWarp {
    top: Curve,
    bottom: Curve,
    left: Curve,
    right: Curve,
}

impl BoundaryWarp {
    /// Fits a warp to a coverage mask. `amount` runs from 0, which changes nothing, to 1, which
    /// fills the rectangle.
    pub fn fit(mask: &[bool], width: usize, height: usize, amount: f32) -> Self {
        let sigma_across = (width as f32 * SMOOTHING).max(1.0);
        let (raw_top, raw_bottom) = extents(width, height, |x, y| mask[y * width + x]);
        let top = eased(inner_envelope(&raw_top, sigma_across, 1.0), 0.0, amount);
        let bottom = eased(inner_envelope(&raw_bottom, sigma_across, -1.0), 1.0, amount);

        // Rows are measured on the picture after the column stretch, which is the order the
        // warp undoes them in.
        let stretched = |x: usize, y: usize| {
            let u = (x as f64 + 0.5) / width as f64;
            let v = (y as f64 + 0.5) / height as f64;
            let (low, high) = (top.at(u), bottom.at(u));
            let source_y = (low + v * (high - low)) * height as f64;
            let row = (source_y as usize).min(height - 1);
            mask[row * width + x]
        };
        let sigma_down = (height as f32 * SMOOTHING).max(1.0);
        let (raw_left, raw_right) = extents(height, width, |y, x| stretched(x, y));
        let left = eased(inner_envelope(&raw_left, sigma_down, 1.0), 0.0, amount);
        let right = eased(inner_envelope(&raw_right, sigma_down, -1.0), 1.0, amount);
        Self { top, bottom, left, right }
    }

    /// Where a position in the warped picture came from in the plain one, both from 0 to 1.
    #[inline]
    pub fn source(&self, u: f64, v: f64) -> (f64, f64) {
        let across = self.left.at(v) + u * (self.right.at(v) - self.left.at(v));
        let down = self.top.at(across) + v * (self.bottom.at(across) - self.top.at(across));
        (across, down)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An outline like a stitched row of frames: a sagging arc with a jagged edge.
    fn outline(width: usize, height: usize) -> Vec<bool> {
        let (w, h) = (width as f32, height as f32);
        (0..width * height)
            .map(|index| {
                let (x, y) = ((index % width) as f32, (index / width) as f32);
                let arc = ((x / w - 0.5) * 2.0).powi(2) * h * 0.18;
                let notch = if (x / (w / 5.0)).fract() < 0.1 { h * 0.03 } else { 0.0 };
                let top = h * 0.05 + arc + notch;
                let bottom = h * 0.77 + arc - notch;
                y >= top && y <= bottom
            })
            .collect()
    }

    fn filled_share(warp: &BoundaryWarp, mask: &[bool], width: usize, height: usize) -> f32 {
        let inside = (0..width * height)
            .filter(|index| {
                let (u, v) = (
                    ((index % width) as f64 + 0.5) / width as f64,
                    ((index / width) as f64 + 0.5) / height as f64,
                );
                let (x, y) = warp.source(u, v);
                let at = ((y * height as f64) as usize).min(height - 1) * width
                    + ((x * width as f64) as usize).min(width - 1);
                mask[at]
            })
            .count();
        inside as f32 / (width * height) as f32
    }

    #[test]
    fn nothing_moves_at_zero() {
        let mask = outline(200, 100);
        let warp = BoundaryWarp::fit(&mask, 200, 100, 0.0);
        let (x, y) = warp.source(0.3, 0.7);
        assert!((x - 0.3).abs() < 1e-6 && (y - 0.7).abs() < 1e-6, "{x} {y}");
    }

    #[test]
    fn a_rectangle_is_left_alone() {
        let warp = BoundaryWarp::fit(&[true; 20000], 200, 100, 1.0);
        let (x, y) = warp.source(0.25, 0.8);
        assert!((x - 0.25).abs() < 1e-3 && (y - 0.8).abs() < 1e-3, "{x} {y}");
    }

    #[test]
    fn full_strength_fills_the_rectangle() {
        let (width, height) = (400, 200);
        let mask = outline(width, height);
        let plain = mask.iter().filter(|value| **value).count() as f32 / mask.len() as f32;
        let warp = BoundaryWarp::fit(&mask, width, height, 1.0);
        let filled = filled_share(&warp, &mask, width, height);
        assert!(plain < 0.75, "{plain}");
        assert!(filled > 0.97, "{filled}");
    }

    #[test]
    fn half_strength_lands_between() {
        let (width, height) = (400, 200);
        let mask = outline(width, height);
        let plain = mask.iter().filter(|value| **value).count() as f32 / mask.len() as f32;
        let half = filled_share(&BoundaryWarp::fit(&mask, width, height, 0.5), &mask, width, height);
        let full = filled_share(&BoundaryWarp::fit(&mask, width, height, 1.0), &mask, width, height);
        assert!(plain < half && half < full, "{plain} {half} {full}");
    }

    #[test]
    fn straight_columns_only_bend_gently() {
        let (width, height) = (400, 200);
        let mask = outline(width, height);
        let warp = BoundaryWarp::fit(&mask, width, height, 1.0);
        let bends: Vec<f64> = (1..399).map(|x| x as f64 / 400.0).map(|u| warp.source(u, 0.1).1).collect();
        let sharpest = bends.windows(3).map(|w| (w[2] - 2.0 * w[1] + w[0]).abs()).fold(0.0, f64::max);
        assert!(sharpest < 2e-3, "{sharpest}");
    }

    #[test]
    fn gaps_fill_from_their_neighbours() {
        let mut values = [None, Some(0.5), None, None, Some(0.25), None];
        assert_eq!(fill_gaps(&mut values), [0.5, 0.5, 0.5, 0.5, 0.25, 0.25]);
    }
}
