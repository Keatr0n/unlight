//! Sharpens matches once the cameras are roughly known. Feature positions are only good to about
//! a pixel of the small image they were found in, which is several pixels of a 61 MP frame, and
//! that shows up as ghosting in the overlaps. With a first solution in hand, the view of a patch
//! from the other frame can be predicted, foreshortening and all, so each match can be slid into
//! place by comparing pixels directly, on the larger copy.

use crate::{Plane, bundle::{Correspondence, PairMatches}, linalg::apply_homography};
use rayon::prelude::*;
use unlight_color::matrix::Mat3;

const PATCH_RADIUS: isize = 10;
const STEPS: usize = 12;
const CONVERGED: f32 = 0.01;
/// A match that wants to move further than this was probably wrong.
const LONGEST_MOVE: f32 = 6.0;
/// Below this correlation, the two patches aren't showing the same thing.
const MIN_CORRELATION: f32 = 0.85;
const MIN_TEXTURE: f32 = 1e-4;

/// Which frame's log luma a pair of positions refer to, on its centred pixels.
struct Patch<'a> {
    log: &'a Plane,
    half: [f32; 2],
}

impl Patch<'_> {
    #[inline]
    fn at(&self, x: f32, y: f32) -> f32 {
        self.log.bilinear(x + self.half[0] - 0.5, y + self.half[1] - 0.5)
    }

    fn holds(&self, x: f32, y: f32, margin: f32) -> bool {
        x.abs() + margin < self.half[0] - 1.0 && y.abs() + margin < self.half[1] - 1.0
    }
}

fn mean(values: &[f32]) -> f32 {
    values.iter().sum::<f32>() / values.len().max(1) as f32
}

/// Slides one match to where the patches agree best. `None` if they never really do.
fn polish(point: [f64; 2], first: &Patch<'_>, second: &Patch<'_>, between: &Mat3) -> Option<[f64; 2]> {
    let size = (PATCH_RADIUS * 2 + 1) as usize;
    let reach = PATCH_RADIUS as f32 + 2.0;
    let (x, y) = (point[0] as f32, point[1] as f32);
    if !first.holds(x, y, reach) {
        return None;
    }
    let mut template = Vec::with_capacity(size * size);
    let mut places = Vec::with_capacity(size * size);
    for dy in -PATCH_RADIUS..=PATCH_RADIUS {
        for dx in -PATCH_RADIUS..=PATCH_RADIUS {
            let (px, py) = (x + dx as f32, y + dy as f32);
            template.push(first.at(px, py));
            let [qx, qy] = apply_homography(between, [f64::from(px), f64::from(py)]);
            places.push([qx as f32, qy as f32]);
        }
    }
    let template_mean = mean(&template);
    template.iter_mut().for_each(|value| *value -= template_mean);
    let template_energy: f32 = template.iter().map(|value| value * value).sum();
    if template_energy / (size * size) as f32 <= MIN_TEXTURE {
        return None;
    }
    let center = apply_homography(between, point);
    let (mut shift_x, mut shift_y) = (0.0f32, 0.0f32);
    let mut moving = vec![0.0f32; template.len()];
    for _ in 0..STEPS {
        if !second.holds(center[0] as f32 + shift_x, center[1] as f32 + shift_y, reach * 1.5) {
            return None;
        }
        let (mut xx, mut xy, mut yy, mut bx, mut by) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for (value, place) in moving.iter_mut().zip(&places) {
            *value = second.at(place[0] + shift_x, place[1] + shift_y);
        }
        let moving_mean = mean(&moving);
        for (index, place) in places.iter().enumerate() {
            let (px, py) = (place[0] + shift_x, place[1] + shift_y);
            let gradient_x = (second.at(px + 1.0, py) - second.at(px - 1.0, py)) * 0.5;
            let gradient_y = (second.at(px, py + 1.0) - second.at(px, py - 1.0)) * 0.5;
            let error = template[index] - (moving[index] - moving_mean);
            xx += gradient_x * gradient_x;
            xy += gradient_x * gradient_y;
            yy += gradient_y * gradient_y;
            bx += gradient_x * error;
            by += gradient_y * error;
        }
        let determinant = xx * yy - xy * xy;
        if determinant.abs() < 1e-9 {
            return None;
        }
        let (step_x, step_y) = ((yy * bx - xy * by) / determinant, (xx * by - xy * bx) / determinant);
        shift_x += step_x;
        shift_y += step_y;
        if shift_x.hypot(shift_y) > LONGEST_MOVE {
            return None;
        }
        if step_x.hypot(step_y) < CONVERGED {
            break;
        }
    }
    for (value, place) in moving.iter_mut().zip(&places) {
        *value = second.at(place[0] + shift_x, place[1] + shift_y);
    }
    let moving_mean = mean(&moving);
    let (mut cross, mut energy) = (0.0f32, 0.0f32);
    for (a, b) in template.iter().zip(&moving) {
        let b = b - moving_mean;
        cross += a * b;
        energy += b * b;
    }
    let correlation = cross / (template_energy * energy).sqrt().max(1e-9);
    (correlation >= MIN_CORRELATION).then(|| [center[0] + f64::from(shift_x), center[1] + f64::from(shift_y)])
}

/// Re-places a pair's matches on larger images of log luma. `between` carries centred pixels of
/// the first to the second, as far as the cameras know so far. Points that can't be improved
/// are left out, unless so many fail that the pair would go short, when it's returned as it was.
pub(crate) fn polish_pair(pair: &PairMatches, first: &Plane, second: &Plane, between: &Mat3) -> PairMatches {
    let halves = |plane: &Plane| [plane.width as f32 * 0.5, plane.height as f32 * 0.5];
    let (from, to) = (Patch { log: first, half: halves(first) }, Patch { log: second, half: halves(second) });
    let points: Vec<Correspondence> = pair
        .points
        .par_iter()
        .filter_map(|point| {
            let landed = polish(point.first, &from, &to, between)?;
            Some(Correspondence { first: point.first, second: landed })
        })
        .collect();
    if points.len() * 3 < pair.points.len() {
        return pair.clone();
    }
    PairMatches { first: pair.first, second: pair.second, points }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(x: f32, y: f32) -> f32 {
        ((x * 0.31).sin() + (y * 0.23).cos()) * 0.5 + ((x * 0.11 + y * 0.17).sin() * (x * 0.07 - y * 0.13).cos()) * 0.8
    }

    fn plane(width: usize, height: usize, offset: [f32; 2], gain: f32) -> Plane {
        Plane::from_fn(width, height, |x, y| {
            let (cx, cy) = (x as f32 + 0.5 - width as f32 * 0.5, y as f32 + 0.5 - height as f32 * 0.5);
            texture(cx - offset[0], cy - offset[1]) + gain
        })
    }

    fn identity_shift(x: f64, y: f64) -> Mat3 {
        [[1.0, 0.0, x], [0.0, 1.0, y], [0.0, 0.0, 1.0]]
    }

    #[test]
    fn a_match_slides_into_place_regardless_of_exposure() {
        let first = plane(256, 256, [0.0, 0.0], 0.0);
        let second = plane(256, 256, [3.37, -1.64], 1.2);
        let points: Vec<Correspondence> = [[-40.0, 10.0], [25.0, -30.0], [0.0, 45.0], [-60.0, -55.0]]
            .iter()
            .map(|&point| Correspondence { first: point, second: point })
            .collect();
        let pair = PairMatches { first: 0, second: 1, points };
        let polished = polish_pair(&pair, &first, &second, &identity_shift(2.5, -1.0));
        assert_eq!(polished.points.len(), 4);
        for point in &polished.points {
            let (dx, dy) = (point.second[0] - point.first[0], point.second[1] - point.first[1]);
            assert!((dx - 3.37).abs() < 0.03 && (dy + 1.64).abs() < 0.03, "{dx} {dy}");
        }
    }

    #[test]
    fn a_featureless_patch_is_left_out() {
        let flat = Plane::filled(128, 128, 0.2);
        let patches = (Patch { log: &flat, half: [64.0; 2] }, Patch { log: &flat, half: [64.0; 2] });
        assert_eq!(polish([0.0, 0.0], &patches.0, &patches.1, &identity_shift(0.0, 0.0)), None);
    }

    #[test]
    fn unrelated_patches_are_left_out() {
        let first = plane(256, 256, [0.0, 0.0], 0.0);
        let second = Plane::from_fn(256, 256, |x, y| texture(y as f32 * 1.7, x as f32 * 2.3));
        let patches = (Patch { log: &first, half: [128.0; 2] }, Patch { log: &second, half: [128.0; 2] });
        assert_eq!(polish([10.0, 10.0], &patches.0, &patches.1, &identity_shift(0.0, 0.0)), None);
    }

    #[test]
    fn a_pair_that_mostly_fails_stays_as_it_was() {
        let first = plane(128, 128, [0.0, 0.0], 0.0);
        let second = Plane::filled(128, 128, 0.0);
        let points = vec![Correspondence { first: [0.0, 0.0], second: [0.0, 0.0] }; 6];
        let pair = PairMatches { first: 0, second: 1, points };
        assert_eq!(polish_pair(&pair, &first, &second, &identity_shift(0.0, 0.0)).points.len(), 6);
    }
}
