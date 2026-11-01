//! Solves for where the camera pointed. Every frame gets a rotation and the whole set shares one
//! focal length, refined together so that errors spread evenly instead of piling up at the far
//! end of the panorama (Brown & Lowe 2007). A set that closes a full circle closes it properly,
//! because the first and last frames are matched like any others.

use crate::linalg::{best_rotation, cross, dot, normalized, rotation_from_vector, solve_spd, symmetric_eigen, transpose};
use rayon::prelude::*;
use unlight_color::matrix::{IDENTITY, Mat3, Vec3, apply, mul};

/// Beyond this residual in pixels a match counts for less and less, so that a few wrong ones
/// can't pull the solution.
const HUBER: f64 = 2.5;
const ITERATIONS: usize = 100;
const INITIAL_DAMPING: f64 = 1e-3;
const CONVERGED: f64 = 1e-9;
/// How firmly the focal length stays near its starting value, in pixels of error per doubling.
/// Weak enough that real evidence overrides it at once. It's only there because a single row
/// of frames, turning about one axis, barely constrains the focal length at all.
const FOCAL_PRIOR: f64 = 0.25;

/// One point seen in two frames, in pixels from the middle of the frame, on an image with no
/// lens distortion left in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Correspondence {
    pub first: [f64; 2],
    pub second: [f64; 2],
}

/// The verified matches between two frames.
#[derive(Clone, Debug)]
pub struct PairMatches {
    pub first: usize,
    pub second: usize,
    pub points: Vec<Correspondence>,
}

/// A frame's orientation: takes rays in the world to rays in the camera.
pub type Camera = Mat3;

fn ray(point: [f64; 2], focal: f64) -> Vec3 {
    [point[0] / focal, point[1] / focal, 1.0]
}

/// The rotation from one frame to another that best explains a pair's matches.
fn relative_rotation(pair: &PairMatches, focal: f64) -> Mat3 {
    best_rotation(pair.points.iter().map(|point| {
        (normalized(&ray(point.first, focal)), normalized(&ray(point.second, focal)))
    }))
}

/// Starting orientations, walking outwards from `root` along the strongest pair each time.
/// `None` for frames that nothing connects to the root.
pub(crate) fn initial_cameras(count: usize, root: usize, pairs: &[PairMatches], focal: f64) -> Vec<Option<Camera>> {
    let mut cameras = vec![None; count];
    cameras[root] = Some(IDENTITY);
    loop {
        let step = pairs
            .iter()
            .filter(|pair| cameras[pair.first].is_some() != cameras[pair.second].is_some())
            .max_by_key(|pair| pair.points.len());
        let Some(pair) = step else {
            return cameras;
        };
        let relative = relative_rotation(pair, focal);
        match (cameras[pair.first], cameras[pair.second]) {
            (Some(known), None) => cameras[pair.second] = Some(mul(&relative, &known)),
            (None, Some(known)) => cameras[pair.first] = Some(mul(&transpose(&relative), &known)),
            _ => {}
        }
    }
}

struct Residual {
    value: [f64; 2],
    /// Of each component against the rotation of the camera the point came from, the rotation
    /// of the camera it landed in, and the log of the focal length.
    from: [[f64; 3]; 2],
    to: [[f64; 3]; 2],
    focal: [f64; 2],
}

/// Reprojects a point from one frame into another.
fn reproject(from: [f64; 2], to: [f64; 2], between: &Mat3, focal: f64) -> Option<Residual> {
    let source = ray(from, focal);
    let [x, y, z] = apply(between, &source);
    if z <= 1e-6 {
        return None;
    }
    let (depth, along_x, along_y) = (focal / z, x / z, y / z);
    let value = [focal * along_x - to[0], focal * along_y - to[1]];
    let projection = [[depth, 0.0, -depth * along_x], [0.0, depth, -depth * along_y]];
    let landed = [x, y, z];

    // A turn ω of the destination moves the point by ω × X, which is -[X]× ω.
    let moved_by_to: [Vec3; 3] = std::array::from_fn(|axis| {
        let mut unit = [0.0; 3];
        unit[axis] = 1.0;
        cross(&unit, &landed)
    });
    // A turn ω of the source moves the ray by -ω × r, seen through the relative rotation.
    let moved_by_from: [Vec3; 3] = std::array::from_fn(|axis| {
        let mut unit = [0.0; 3];
        unit[axis] = 1.0;
        apply(between, &cross(&source, &unit))
    });
    let along = |row: usize, moved: &[Vec3; 3]| -> [f64; 3] {
        std::array::from_fn(|axis| dot(&projection[row], &moved[axis]))
    };
    let shifted = apply(between, &[-from[0] / focal, -from[1] / focal, 0.0]);
    let focal_term = |row: usize, along: f64| along * focal + dot(&projection[row], &shifted);
    Some(Residual {
        value,
        from: [along(0, &moved_by_from), along(1, &moved_by_from)],
        to: [along(0, &moved_by_to), along(1, &moved_by_to)],
        focal: [focal_term(0, along_x), focal_term(1, along_y)],
    })
}

/// Every residual of a pair, in both directions.
fn pair_residuals<'a>(
    pair: &'a PairMatches,
    cameras: &'a [Camera],
    focal: f64,
) -> impl Iterator<Item = (usize, usize, Residual)> + 'a {
    let forward = mul(&cameras[pair.second], &transpose(&cameras[pair.first]));
    let backward = transpose(&forward);
    pair.points.iter().flat_map(move |point| {
        let there = reproject(point.first, point.second, &forward, focal)
            .map(|residual| (pair.first, pair.second, residual));
        let back = reproject(point.second, point.first, &backward, focal)
            .map(|residual| (pair.second, pair.first, residual));
        [there, back].into_iter().flatten()
    })
}

#[inline]
fn huber_weight(size: f64) -> f64 {
    if size <= HUBER { 1.0 } else { HUBER / size }
}

fn huber_cost(size: f64) -> f64 {
    if size <= HUBER { 0.5 * size * size } else { HUBER * (size - 0.5 * HUBER) }
}

fn total_cost(pairs: &[PairMatches], cameras: &[Camera], focal: f64, start: f64) -> f64 {
    let matches: f64 = pairs
        .par_iter()
        .map(|pair| {
            pair_residuals(pair, cameras, focal)
                .map(|(_, _, residual)| huber_cost(residual.value[0].hypot(residual.value[1])))
                .sum::<f64>()
        })
        .sum();
    matches + 0.5 * focal_prior_residual(pairs, focal, start).powi(2)
}

fn focal_prior_weight(pairs: &[PairMatches]) -> f64 {
    let count: usize = pairs.iter().map(|pair| pair.points.len() * 2).sum();
    FOCAL_PRIOR * (count as f64).sqrt() / std::f64::consts::LN_2
}

fn focal_prior_residual(pairs: &[PairMatches], focal: f64, start: f64) -> f64 {
    focal_prior_weight(pairs) * (focal / start).ln()
}

/// One Jacobian's worth of normal equations for a pair.
fn accumulate(pair: &PairMatches, cameras: &[Camera], focal: f64, size: usize) -> (Vec<f64>, Vec<f64>) {
    let mut system = vec![0.0; size * size];
    let mut gradient = vec![0.0; size];
    let focal_slot = size - 1;
    for (from, to, residual) in pair_residuals(pair, cameras, focal) {
        let weight = huber_weight(residual.value[0].hypot(residual.value[1]));
        for component in 0..2 {
            let slots: [(usize, f64); 7] = [
                (from * 3, residual.from[component][0]),
                (from * 3 + 1, residual.from[component][1]),
                (from * 3 + 2, residual.from[component][2]),
                (to * 3, residual.to[component][0]),
                (to * 3 + 1, residual.to[component][1]),
                (to * 3 + 2, residual.to[component][2]),
                (focal_slot, residual.focal[component]),
            ];
            for (row, row_value) in slots {
                for (column, column_value) in slots {
                    system[row * size + column] += weight * row_value * column_value;
                }
                gradient[row] += weight * row_value * residual.value[component];
            }
        }
    }
    (system, gradient)
}

fn retract(cameras: &[Camera], focal: f64, step: &[f64]) -> (Vec<Camera>, f64) {
    let moved = cameras
        .iter()
        .enumerate()
        .map(|(index, camera)| {
            let turn = [step[index * 3], step[index * 3 + 1], step[index * 3 + 2]];
            mul(&rotation_from_vector(&turn), camera)
        })
        .collect();
    (moved, focal * step[cameras.len() * 3].exp())
}

/// Refines cameras and focal length by Levenberg-Marquardt, with `root` held still. Returns the
/// root-mean-square reprojection error in pixels.
pub fn adjust(cameras: &mut Vec<Camera>, focal: &mut f64, pairs: &[PairMatches], root: usize) -> f64 {
    let size = cameras.len() * 3 + 1;
    let start = *focal;
    let mut damping = INITIAL_DAMPING;
    let mut cost = total_cost(pairs, cameras, *focal, start);
    for _ in 0..ITERATIONS {
        let (mut system, mut gradient) = pairs
            .par_iter()
            .map(|pair| accumulate(pair, cameras, *focal, size))
            .reduce(
                || (vec![0.0; size * size], vec![0.0; size]),
                |mut total, part| {
                    total.0.iter_mut().zip(&part.0).for_each(|(a, b)| *a += b);
                    total.1.iter_mut().zip(&part.1).for_each(|(a, b)| *a += b);
                    total
                },
            );
        let prior = focal_prior_weight(pairs);
        system[size * size - 1] += prior * prior;
        gradient[size - 1] += prior * focal_prior_residual(pairs, *focal, start);
        for axis in 0..3 {
            let slot = root * 3 + axis;
            for other in 0..size {
                system[slot * size + other] = 0.0;
                system[other * size + slot] = 0.0;
            }
            system[slot * size + slot] = 1.0;
            gradient[slot] = 0.0;
        }

        let mut improved = false;
        for _ in 0..12 {
            let mut damped = system.clone();
            for slot in 0..size {
                damped[slot * size + slot] += damping * damped[slot * size + slot].max(1e-9);
            }
            let mut step: Vec<f64> = gradient.iter().map(|value| -value).collect();
            if solve_spd(&mut damped, &mut step).is_none() || step.iter().any(|value| !value.is_finite()) {
                damping *= 4.0;
                continue;
            }
            let (moved, moved_focal) = retract(cameras, *focal, &step);
            let moved_cost = total_cost(pairs, &moved, moved_focal, start);
            if moved_cost < cost {
                let change = (cost - moved_cost) / cost.max(1e-12);
                *cameras = moved;
                *focal = moved_focal;
                cost = moved_cost;
                damping = (damping / 3.0).max(1e-9);
                improved = change > CONVERGED;
                break;
            }
            damping *= 4.0;
        }
        if !improved {
            break;
        }
    }
    reprojection_rms(pairs, cameras, *focal)
}

/// Root-mean-square distance between where matches are and where the cameras put them.
pub(crate) fn reprojection_rms(pairs: &[PairMatches], cameras: &[Camera], focal: f64) -> f64 {
    let (sum, count) = pairs
        .iter()
        .flat_map(|pair| pair_residuals(pair, cameras, focal))
        .fold((0.0, 0usize), |(sum, count), (_, _, residual)| {
            (sum + residual.value[0].powi(2) + residual.value[1].powi(2), count + 1)
        });
    if count == 0 { 0.0 } else { (sum / count as f64).sqrt() }
}

/// The median residual of one pair, which shows a false pair up after adjustment.
pub(crate) fn pair_error(pair: &PairMatches, cameras: &[Camera], focal: f64) -> f64 {
    let mut sizes: Vec<f64> = pair_residuals(pair, cameras, focal)
        .map(|(_, _, residual)| residual.value[0].hypot(residual.value[1]))
        .collect();
    if sizes.is_empty() {
        return f64::MAX;
    }
    let middle = sizes.len() / 2;
    *sizes.select_nth_unstable_by(middle, f64::total_cmp).1
}

/// The focal length a homography between two frames implies, in its pixels, if it implies one
/// (Szeliski & Shum 1997). Used when a file doesn't say.
pub fn estimate_focal(homography: &Mat3) -> Option<f64> {
    let h = homography;
    let (a, b, c, d, e, f, g, i) = (h[0][0], h[0][1], h[0][2], h[1][0], h[1][1], h[1][2], h[2][0], h[2][1]);
    let solve = |d1: f64, d2: f64, v1: f64, v2: f64| -> Option<f64> {
        let (first, second) = (v1 / d1, v2 / d2);
        let (large, small) = if first < second { (second, first) } else { (first, second) };
        if large > 0.0 && small > 0.0 {
            Some(if d1.abs() > d2.abs() { first.sqrt() } else { second.sqrt() })
        } else if large > 0.0 {
            Some(large.sqrt())
        } else {
            None
        }
    };
    let one = solve(g * i, (i - g) * (i + g), -(a * b + d * e), a * a + d * d - b * b - e * e);
    let two = solve(a * d + b * e, a * a + b * b - d * d - e * e, -c * f, f * f - c * c);
    match (one, two) {
        (Some(one), Some(two)) => Some((one * two).sqrt()),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// How a frame was turned on the sensor, from its LibRaw flip code: the axes of a camera that
/// point right and down in the picture as it's meant to be seen.
fn picture_axes(camera: &Camera, flip: i32) -> (Vec3, Vec3) {
    let [right, down, _] = *camera;
    let negative = |axis: Vec3| axis.map(|value| -value);
    match flip {
        3 => (negative(right), negative(down)),
        5 => (down, negative(right)),
        6 => (negative(down), right),
        _ => (right, down),
    }
}

/// Levels the set. Cameras panning across a scene all keep their horizontal axes in one plane,
/// and that plane's normal is the true vertical. Leaving this out lets a panorama curve into a
/// wave, however well its frames line up (Brown & Lowe 2007, section 5).
pub(crate) fn leveled(cameras: &[Camera], flips: &[i32]) -> Vec<Camera> {
    let mut scatter = [[0.0f64; 3]; 3];
    let mut mean_down = [0.0f64; 3];
    let mut mean_forward = [0.0f64; 3];
    for (camera, flip) in cameras.iter().zip(flips) {
        let (right, down) = picture_axes(camera, *flip);
        for row in 0..3 {
            for column in 0..3 {
                scatter[row][column] += right[row] * right[column];
            }
            mean_down[row] += down[row];
            mean_forward[row] += camera[2][row];
        }
    }
    let (_, vectors) = symmetric_eigen(scatter);
    let mut down = vectors[2];
    if dot(&down, &mean_down) < 0.0 {
        down = down.map(|value| -value);
    }
    let mut forward = if dot(&mean_forward, &mean_forward).sqrt() > 0.1 * cameras.len() as f64 {
        mean_forward
    } else {
        cameras[0][2]
    };
    let along = dot(&forward, &down);
    forward = normalized(&std::array::from_fn(|axis| forward[axis] - down[axis] * along));
    let right = cross(&down, &forward);
    let world = [right, down, forward];
    let back = transpose(&world);
    cameras.iter().map(|camera| mul(camera, &back)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linalg::{is_rotation, rotation_distance, yaw_rotation};

    const FOCAL: f64 = 1000.0;

    fn project(camera: &Camera, world: &Vec3, focal: f64) -> Option<[f64; 2]> {
        let [x, y, z] = apply(camera, world);
        (z > 0.2).then(|| [focal * x / z, focal * y / z])
    }

    fn scene_rays() -> Vec<Vec3> {
        let mut rays = Vec::new();
        for row in 0..30 {
            for column in 0..120 {
                let yaw = column as f64 * 0.06 - 1.8 + (row % 3) as f64 * 0.01;
                let pitch = row as f64 * 0.05 - 0.7;
                rays.push([yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos()]);
            }
        }
        rays
    }

    fn pairs_of(cameras: &[Camera], focal: f64) -> Vec<PairMatches> {
        let rays = scene_rays();
        let visible = |point: [f64; 2]| point[0].abs() < 600.0 && point[1].abs() < 400.0;
        let mut pairs = Vec::new();
        for first in 0..cameras.len() {
            for second in first + 1..cameras.len() {
                let points: Vec<_> = rays
                    .iter()
                    .filter_map(|ray| {
                        let (a, b) = (project(&cameras[first], ray, focal)?, project(&cameras[second], ray, focal)?);
                        (visible(a) && visible(b)).then_some(Correspondence { first: a, second: b })
                    })
                    .collect();
                if points.len() >= 8 {
                    pairs.push(PairMatches { first, second, points });
                }
            }
        }
        pairs
    }

    fn fan(count: usize, step: f64) -> Vec<Camera> {
        (0..count)
            .map(|index| {
                let wobble = rotation_from_vector(&[0.03 * (index as f64 * 1.7).sin(), 0.0, 0.02 * (index as f64).cos()]);
                mul(&wobble, &yaw_rotation(index as f64 * step))
            })
            .collect()
    }

    #[test]
    fn jacobian_matches_finite_differences() {
        let between = rotation_from_vector(&[0.05, -0.2, 0.03]);
        let (from, to) = ([120.0, -80.0], [0.0, 0.0]);
        let exact = reproject(from, to, &between, FOCAL).unwrap();
        let delta = 1e-6;
        for axis in 0..3 {
            let mut turn = [0.0; 3];
            turn[axis] = delta;
            let moved = reproject(from, to, &mul(&rotation_from_vector(&turn), &between), FOCAL).unwrap();
            let numeric = (moved.value[0] - exact.value[0]) / delta;
            assert!((numeric - exact.to[0][axis]).abs() < 1e-3, "to {axis}: {numeric} {:?}", exact.to[0]);
            let unwound = mul(&between, &transpose(&rotation_from_vector(&turn)));
            let moved = reproject(from, to, &unwound, FOCAL).unwrap();
            let numeric = (moved.value[1] - exact.value[1]) / delta;
            assert!((numeric - exact.from[1][axis]).abs() < 1e-3, "from {axis}: {numeric} {:?}", exact.from[1]);
        }
        let moved = reproject(from, to, &between, FOCAL * (1.0 + delta)).unwrap();
        let numeric = (moved.value[0] - exact.value[0]) / delta;
        assert!((numeric - exact.focal[0]).abs() < 1e-2, "{numeric} {:?}", exact.focal);
    }

    #[test]
    fn a_bad_focal_guess_is_corrected() {
        let truth = fan(6, 0.45);
        let pairs = pairs_of(&truth, FOCAL);
        let mut focal = FOCAL * 1.25;
        let mut cameras: Vec<Camera> = initial_cameras(6, 0, &pairs, focal).into_iter().map(Option::unwrap).collect();
        let rms = adjust(&mut cameras, &mut focal, &pairs, 0);
        assert!(rms < 0.05, "rms {rms}");
        assert!((focal / FOCAL - 1.0).abs() < 0.01, "focal {focal}");
        for index in 1..6 {
            let found = mul(&cameras[index], &transpose(&cameras[0]));
            let expected = mul(&truth[index], &transpose(&truth[0]));
            assert!(rotation_distance(&found, &expected) < 1e-3);
            assert!(is_rotation(&cameras[index], 1e-9));
        }
    }

    #[test]
    fn a_closed_circle_closes() {
        let truth = fan(12, std::f64::consts::TAU / 12.0);
        let pairs = pairs_of(&truth, FOCAL);
        assert!(pairs.iter().any(|pair| pair.first == 0 && pair.second == 11) || pairs.len() >= 10);
        let mut focal = FOCAL * 1.05;
        let mut cameras: Vec<Camera> = initial_cameras(12, 0, &pairs, focal).into_iter().flatten().collect();
        assert_eq!(cameras.len(), 12);
        let rms = adjust(&mut cameras, &mut focal, &pairs, 0);
        assert!(rms < 0.1, "rms {rms}");
    }

    #[test]
    fn wrong_matches_dont_move_the_solution_much() {
        let truth = fan(4, 0.5);
        let mut pairs = pairs_of(&truth, FOCAL);
        for pair in &mut pairs {
            for index in (0..pair.points.len()).step_by(20) {
                pair.points[index].second[0] += 80.0;
            }
        }
        let mut focal = FOCAL;
        let mut cameras: Vec<Camera> = initial_cameras(4, 0, &pairs, focal).into_iter().flatten().collect();
        adjust(&mut cameras, &mut focal, &pairs, 0);
        let found = mul(&cameras[3], &transpose(&cameras[0]));
        let expected = mul(&truth[3], &transpose(&truth[0]));
        assert!(rotation_distance(&found, &expected) < 2e-3, "{}", rotation_distance(&found, &expected));
    }

    #[test]
    fn an_unconnected_frame_gets_no_camera() {
        let truth = fan(3, 0.4);
        let mut pairs = pairs_of(&truth, FOCAL);
        pairs.retain(|pair| pair.second != 2);
        let cameras = initial_cameras(3, 0, &pairs, FOCAL);
        assert!(cameras[2].is_none());
        assert!(cameras[1].is_some());
    }

    #[test]
    fn a_homography_gives_back_its_focal_length() {
        let rotation = mul(&rotation_from_vector(&[0.1, 0.0, 0.0]), &yaw_rotation(0.3));
        let scale = [[FOCAL, 0.0, 0.0], [0.0, FOCAL, 0.0], [0.0, 0.0, 1.0]];
        let inverse = [[1.0 / FOCAL, 0.0, 0.0], [0.0, 1.0 / FOCAL, 0.0], [0.0, 0.0, 1.0]];
        let homography = mul(&scale, &mul(&rotation, &inverse));
        let found = estimate_focal(&homography).unwrap();
        assert!((found / FOCAL - 1.0).abs() < 0.02, "{found}");
    }

    #[test]
    fn leveling_finds_the_horizon_of_a_tilted_set() {
        let tilt = rotation_from_vector(&[0.2, 0.0, 0.3]);
        let cameras: Vec<Camera> = (0..5).map(|index| mul(&yaw_rotation(index as f64 * 0.4 - 0.8), &tilt)).collect();
        let flat = leveled(&cameras, &[0; 5]);
        for camera in &flat {
            assert!(camera[0][1].abs() < 1e-6, "{camera:?}");
        }
        for (camera, was) in flat.iter().zip(&cameras) {
            assert!(is_rotation(camera, 1e-9));
            assert!(was[0][0].is_finite());
        }
    }

    #[test]
    fn leveling_works_for_frames_shot_in_portrait() {
        let turned = [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let cameras: Vec<Camera> = (0..5).map(|index| mul(&turned, &yaw_rotation(index as f64 * 0.3 - 0.6))).collect();
        let flat = leveled(&cameras, &[6; 5]);
        for camera in &flat {
            assert!(camera[1][1].abs() < 1e-6, "{camera:?}");
        }
    }
}
