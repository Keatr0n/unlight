//! Upright: reads a photo's perspective from its straight lines and solves the rotation of the
//! camera that would have shot it square. A homography `K·R·K⁻¹` is a real camera rotation, so
//! unlike four-corner keystone fitting it can't shear the picture or change its proportions.
//! Everything here is in image-centred coordinates with the image height as the unit, after
//! lens correction, because a line isn't straight before that.

use crate::lines::Segment;
use unlight_color::{
    Mat3,
    matrix::{apply, inverse, mul},
};
use unlight_recipe::Upright;

type Vec3 = [f64; 3];

/// Lines this close to an axis count as meaning that axis.
const AXIS_WINDOW_DEGREES: f64 = 35.0;
/// Level trusts only lines nearly on an axis, because anything else is mostly perspective.
const LEVEL_WINDOW_DEGREES: f64 = 12.0;
/// How far a line may miss a vanishing point and still support it.
const AGREEMENT_DEGREES: f64 = 1.0;
/// Hypotheses come from every pair of this many longest lines.
const HYPOTHESIS_LINES: usize = 48;
/// What a vanishing point needs behind it: supporting length in image heights, a share of all
/// the length on that axis, and a count of lines. On real photos, scenes with genuine
/// perspective clear these easily and foliage or clutter falls well short.
const MIN_SUPPORT: f64 = 1.0;
const MIN_SUPPORT_SHARE: f64 = 0.4;
const MIN_SUPPORTING_LINES: usize = 4;
/// A vanishing point this close to the centre is a corridor, not a tilted camera.
const MIN_VANISHING_DISTANCE: f64 = 1.5;
/// Beyond this a solution is a misreading of the scene rather than a camera angle.
const MAX_TILT_DEGREES: f64 = 45.0;
/// Tilts this close to the median count as agreeing with it.
const LEVEL_AGREEMENT_DEGREES: f64 = 1.0;
/// Length that has to agree before Level acts, in image heights. Foliage and hillsides yield
/// plenty of short near-level segments that agree on nothing, and they must not turn a photo.
const LEVEL_MIN_SUPPORT: f64 = 1.5;
/// The share of near-axis length that has to agree.
const LEVEL_MIN_SHARE: f64 = 0.5;
/// Auto keeps half of a sideways correction, since Full's complete fix can look unnatural.
const AUTO_YAW: f64 = 0.5;
const POWER_ITERATIONS: usize = 256;

fn dot(a: &Vec3, b: &Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalized(v: Vec3) -> Vec3 {
    let length = dot(&v, &v).sqrt();
    if length > 0.0 { v.map(|value| value / length) } else { v }
}

#[derive(Clone, Copy)]
struct Line {
    /// `a·x + b·y + c = 0` with `a² + b² = 1`.
    equation: Vec3,
    middle: [f64; 2],
    /// Unit direction, which is good enough unsigned.
    direction: [f64; 2],
    length: f64,
}

impl Line {
    fn new(segment: &Segment) -> Self {
        let [x0, y0] = segment.start.map(f64::from);
        let [x1, y1] = segment.end.map(f64::from);
        let length = (x1 - x0).hypot(y1 - y0);
        let direction = [(x1 - x0) / length, (y1 - y0) / length];
        let normal = [-direction[1], direction[0]];
        let equation = [normal[0], normal[1], -(normal[0] * x0 + normal[1] * y0)];
        Self { equation, middle: [(x0 + x1) * 0.5, (y0 + y1) * 0.5], direction, length }
    }

    /// Sine of the angle between this line and the way to a vanishing point.
    fn miss(&self, point: &Vec3) -> f64 {
        let to_point = [point[0] - self.middle[0] * point[2], point[1] - self.middle[1] * point[2]];
        let distance = to_point[0].hypot(to_point[1]);
        if distance == 0.0 {
            return 1.0;
        }
        ((to_point[0] * self.direction[1] - to_point[1] * self.direction[0]) / distance).abs()
    }

    fn is_near(&self, axis: Axis, window_degrees: f64) -> bool {
        let along = match axis {
            Axis::Vertical => self.direction[1],
            Axis::Horizontal => self.direction[0],
        };
        along.abs() >= window_degrees.to_radians().cos()
    }

    /// Clockwise turn from the axis, which is the turn Level has to undo.
    fn tilt(&self, axis: Axis) -> f64 {
        let [dx, dy] = self.direction;
        let (dx, dy) = match axis {
            Axis::Horizontal if dx < 0.0 => (-dx, -dy),
            Axis::Vertical if dy < 0.0 => (-dx, -dy),
            _ => (dx, dy),
        };
        match axis {
            Axis::Horizontal => dy.atan2(dx),
            Axis::Vertical => (-dx).atan2(dy),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Vertical,
    Horizontal,
}

/// The eigenvector of a symmetric matrix with the smallest eigenvalue.
fn smallest_eigenvector(matrix: &Mat3) -> Vec3 {
    let shift = matrix[0][0] + matrix[1][1] + matrix[2][2];
    let shifted: Mat3 = std::array::from_fn(|row| {
        std::array::from_fn(|col| if row == col { shift - matrix[row][col] } else { -matrix[row][col] })
    });
    (0..POWER_ITERATIONS).fold(normalized([1.0, 0.7, 0.4]), |vector, _| normalized(apply(&shifted, &vector)))
}

/// The point that minimises the squared distance to every line, weighted by length.
fn least_squares_point(lines: &[&Line]) -> Vec3 {
    let scatter: Mat3 = std::array::from_fn(|row| {
        std::array::from_fn(|col| {
            lines.iter().map(|line| line.length * line.equation[row] * line.equation[col]).sum()
        })
    });
    smallest_eigenvector(&scatter)
}

/// Finds the vanishing point most lines agree on. Trying every pair of long lines rather than
/// sampling at random means a photo always solves the same way, so preview matches export.
fn vanishing_point(lines: &[Line]) -> Option<Vec3> {
    let mut longest: Vec<&Line> = lines.iter().collect();
    longest.sort_by(|a, b| b.length.total_cmp(&a.length));
    longest.truncate(HYPOTHESIS_LINES);

    let tolerance = AGREEMENT_DEGREES.to_radians().sin();
    let supporters = |point: &Vec3| -> Vec<&Line> {
        lines.iter().filter(|line| line.miss(point) <= tolerance).collect()
    };
    let support = |point: &Vec3| lines.iter().filter(|line| line.miss(point) <= tolerance).map(|line| line.length).sum::<f64>();

    let mut best: Option<(f64, Vec3)> = None;
    for (index, first) in longest.iter().enumerate() {
        for second in &longest[index + 1..] {
            let candidate = cross(&first.equation, &second.equation);
            if dot(&candidate, &candidate) < 1e-12 {
                continue;
            }
            let score = support(&candidate);
            if best.is_none_or(|(top, _)| score > top) {
                best = Some((score, candidate));
            }
        }
    }
    let (score, candidate) = best?;
    let inliers = supporters(&candidate);
    let total: f64 = lines.iter().map(|line| line.length).sum();
    if score < MIN_SUPPORT || score < total * MIN_SUPPORT_SHARE || inliers.len() < MIN_SUPPORTING_LINES {
        return None;
    }
    Some(least_squares_point(&inliers))
}

fn is_beyond_frame(point: &Vec3) -> bool {
    point[0].hypot(point[1]) >= MIN_VANISHING_DISTANCE * point[2].abs()
}

/// Length-weighted median, so one long edge outvotes a scatter of short ones.
fn weighted_median(mut tilts: Vec<(f64, f64)>) -> Option<f64> {
    tilts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = tilts.iter().map(|tilt| tilt.1).sum::<f64>() * 0.5;
    let mut running = 0.0;
    tilts.into_iter().find_map(|(tilt, weight)| {
        running += weight;
        (running >= half).then_some(tilt)
    })
}

fn axis_tilts(lines: &[Line], window_degrees: f64) -> Vec<(f64, f64)> {
    lines
        .iter()
        .flat_map(|line| [Axis::Vertical, Axis::Horizontal].map(|axis| (line, axis)))
        .filter(|(line, axis)| line.is_near(*axis, window_degrees))
        .map(|(line, axis)| (line.tilt(axis), line.length))
        .collect()
}

fn rotation_about_view(angle: f64) -> Mat3 {
    let (sin, cos) = angle.sin_cos();
    [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]]
}

fn rotation_about_vertical(angle: f64) -> Mat3 {
    let (sin, cos) = angle.sin_cos();
    [[cos, 0.0, sin], [0.0, 1.0, 0.0], [-sin, 0.0, cos]]
}

/// The smallest rotation taking one unit vector to another, by Rodrigues' formula.
fn align(from: &Vec3, to: &Vec3) -> Mat3 {
    let axis = cross(from, to);
    let cosine = dot(from, to);
    let scale = 1.0 / (1.0 + cosine).max(1e-9);
    let [x, y, z] = axis;
    let cross_matrix = [[0.0, -z, y], [z, 0.0, -x], [-y, x, 0.0]];
    std::array::from_fn(|row| {
        std::array::from_fn(|col| {
            let identity = if row == col { 1.0 } else { 0.0 };
            identity + cross_matrix[row][col] + scale * axis[row] * axis[col] - if row == col { scale * dot(&axis, &axis) } else { 0.0 }
        })
    })
}

/// Sets a vanishing point's camera direction pointing along the positive axis.
fn view_direction(point: &Vec3, focal: f64, axis: Axis) -> Vec3 {
    let direction = normalized([point[0] / focal, point[1] / focal, point[2]]);
    let component = if axis == Axis::Vertical { direction[1] } else { direction[0] };
    if component < 0.0 { direction.map(|value| -value) } else { direction }
}

/// A rotation standing the vertical vanishing direction upright, then turning some share of the
/// way towards facing the horizontal one.
fn squared_up(vertical: &Vec3, horizontal: Option<&Vec3>, yaw_share: f64) -> Mat3 {
    let upright = align(vertical, &[0.0, 1.0, 0.0]);
    let Some(horizontal) = horizontal else {
        return upright;
    };
    let turned = apply(&upright, horizontal);
    let yaw = turned[2].atan2(turned[0]) * yaw_share;
    mul(&rotation_about_vertical(yaw), &upright)
}

/// `K·R·K⁻¹`, shifted so the image centre stays where it was.
fn homography(rotation: &Mat3, focal: f64) -> Option<Mat3> {
    let intrinsics = [[focal, 0.0, 0.0], [0.0, focal, 0.0], [0.0, 0.0, 1.0]];
    let rotated = mul(&mul(&intrinsics, rotation), &inverse(&intrinsics)?);
    let center = apply(&rotated, &[0.0, 0.0, 1.0]);
    let shift = [[1.0, 0.0, -center[0] / center[2]], [0.0, 1.0, -center[1] / center[2]], [0.0, 0.0, 1.0]];
    Some(mul(&shift, &rotated))
}

fn is_reasonable(rotation: &Mat3) -> bool {
    rotation[2][2].abs().acos().to_degrees() <= MAX_TILT_DEGREES
        && rotation[1][1].abs().acos().to_degrees() <= MAX_TILT_DEGREES
}

fn level(lines: &[Line]) -> Option<Mat3> {
    let tilts = axis_tilts(lines, LEVEL_WINDOW_DEGREES);
    let tilt = weighted_median(tilts.clone())?;
    let total: f64 = tilts.iter().map(|entry| entry.1).sum();
    let agreeing: f64 = tilts
        .iter()
        .filter(|entry| (entry.0 - tilt).abs() <= LEVEL_AGREEMENT_DEGREES.to_radians())
        .map(|entry| entry.1)
        .sum();
    (agreeing >= LEVEL_MIN_SUPPORT && agreeing >= total * LEVEL_MIN_SHARE)
        .then(|| rotation_about_view(-tilt))
}

fn lines_near(lines: &[Line], axis: Axis) -> Vec<Line> {
    lines.iter().filter(|line| line.is_near(axis, AXIS_WINDOW_DEGREES)).copied().collect()
}

fn scene_vanishing_point(lines: &[Line], axis: Axis) -> Option<Vec3> {
    vanishing_point(&lines_near(lines, axis)).filter(is_beyond_frame)
}

/// Rotation for the automatic modes: `yaw_share` of 0 only stands verticals upright.
fn perspective(lines: &[Line], focal: f64, yaw_share: f64) -> Option<Mat3> {
    let vertical = scene_vanishing_point(lines, Axis::Vertical);
    let horizontal = scene_vanishing_point(lines, Axis::Horizontal).filter(|_| yaw_share > 0.0);
    let vertical = vertical.map(|point| view_direction(&point, focal, Axis::Vertical));
    let horizontal = horizontal.map(|point| view_direction(&point, focal, Axis::Horizontal));
    let rotation = match (&vertical, &horizontal) {
        (Some(vertical), horizontal) => squared_up(vertical, horizontal.as_ref(), yaw_share),
        (None, Some(horizontal)) => align(horizontal, &[1.0, 0.0, 0.0]),
        (None, None) => return level(lines),
    };
    if is_reasonable(&rotation) {
        homography(&rotation, focal)
    } else {
        level(lines)
    }
}

/// Guides are the user's word, so they skip the checks that guard against misreading a scene.
fn guided(guides: &[Line], focal: f64) -> Option<Mat3> {
    let (upright, flat): (Vec<Line>, Vec<Line>) =
        guides.iter().partition(|line| line.direction[1].abs() > line.direction[0].abs());
    let direction = |lines: &[Line], axis: Axis| {
        (lines.len() >= 2).then(|| {
            let lines: Vec<&Line> = lines.iter().collect();
            view_direction(&least_squares_point(&lines), focal, axis)
        })
    };
    let vertical = direction(&upright, Axis::Vertical);
    let horizontal = direction(&flat, Axis::Horizontal);
    let rotation = match (&vertical, &horizontal) {
        (Some(vertical), horizontal) => squared_up(vertical, horizontal.as_ref(), 1.0),
        (None, Some(horizontal)) => align(horizontal, &[1.0, 0.0, 0.0]),
        (None, None) => {
            let tilt = |lines: &[Line], axis| lines.iter().map(|line| (line.tilt(axis), line.length)).collect::<Vec<_>>();
            let mut tilts = tilt(&upright, Axis::Vertical);
            tilts.extend(tilt(&flat, Axis::Horizontal));
            return weighted_median(tilts).map(|angle| rotation_about_view(-angle));
        }
    };
    homography(&rotation, focal)
}

/// Solves an Upright mode. Returns the transform from undistorted to output coordinates, or
/// `None` when the photo gives nothing to go on, which leaves it as shot.
pub fn solve(mode: Upright, segments: &[Segment], guides: &[Segment], focal: f64) -> Option<Mat3> {
    let lines: Vec<Line> = segments.iter().map(Line::new).collect();
    match mode {
        Upright::Off => None,
        Upright::Level => level(&lines),
        Upright::Vertical => perspective(&lines, focal, 0.0),
        Upright::Auto => perspective(&lines, focal, AUTO_YAW),
        Upright::Full => perspective(&lines, focal, 1.0),
        Upright::Guided => guided(&guides.iter().map(Line::new).collect::<Vec<_>>(), focal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOCAL: f64 = 1.5;

    fn project(matrix: &Mat3, point: [f64; 2]) -> [f32; 2] {
        let [x, y, w] = apply(matrix, &[point[0], point[1], 1.0]);
        [(x / w) as f32, (y / w) as f32]
    }

    fn segment(matrix: &Mat3, start: [f64; 2], end: [f64; 2]) -> Segment {
        Segment { start: project(matrix, start), end: project(matrix, end) }
    }

    /// A facade of verticals and horizontals seen through a known camera rotation.
    fn facade(camera: &Mat3) -> Vec<Segment> {
        let mut segments = Vec::new();
        for column in -4..=4 {
            let x = f64::from(column) * 0.12;
            segments.push(segment(camera, [x, -0.4], [x, 0.4]));
        }
        for row in -3..=3 {
            let y = f64::from(row) * 0.12;
            segments.push(segment(camera, [-0.55, y], [0.55, y]));
        }
        segments
    }

    fn pitch(degrees: f64) -> Mat3 {
        let (sin, cos) = degrees.to_radians().sin_cos();
        [[1.0, 0.0, 0.0], [0.0, cos, -sin], [0.0, sin, cos]]
    }

    fn yaw(degrees: f64) -> Mat3 {
        rotation_about_vertical(degrees.to_radians())
    }

    fn roll(degrees: f64) -> Mat3 {
        rotation_about_view(degrees.to_radians())
    }

    /// A real camera turned about its own centre, so its principal point stays at the origin.
    fn camera(rotation: &Mat3) -> Mat3 {
        let intrinsics = [[FOCAL, 0.0, 0.0], [0.0, FOCAL, 0.0], [0.0, 0.0, 1.0]];
        mul(&mul(&intrinsics, rotation), &inverse(&intrinsics).unwrap())
    }

    /// Worst angle of any line from its nearest image axis, once corrected.
    fn axis_error(correction: &Mat3, segments: &[Segment]) -> f64 {
        segments
            .iter()
            .map(|segment| {
                let [x0, y0] = segment.start.map(f64::from);
                let [x1, y1] = segment.end.map(f64::from);
                let moved = |x, y| project(correction, [x, y]);
                let (a, b) = (moved(x0, y0), moved(x1, y1));
                let (dx, dy) = (f64::from(b[0] - a[0]).abs(), f64::from(b[1] - a[1]).abs());
                dx.min(dy).atan2(dx.max(dy)).to_degrees()
            })
            .fold(0.0, f64::max)
    }

    fn converging_verticals(correction: &Mat3, segments: &[Segment]) -> f64 {
        segments
            .iter()
            .filter(|segment| (segment.end[1] - segment.start[1]).abs() > (segment.end[0] - segment.start[0]).abs())
            .map(|segment| {
                let [x0, y0] = segment.start.map(f64::from);
                let [x1, y1] = segment.end.map(f64::from);
                let a = project(correction, [x0, y0]);
                let b = project(correction, [x1, y1]);
                f64::from(b[0] - a[0]).atan2(f64::from(b[1] - a[1])).to_degrees().abs()
            })
            .fold(0.0, f64::max)
    }

    #[test]
    fn off_changes_nothing() {
        let segments = facade(&camera(&pitch(10.0)));
        assert_eq!(solve(Upright::Off, &segments, &[], FOCAL), None);
    }

    #[test]
    fn no_lines_means_no_correction() {
        for mode in [Upright::Auto, Upright::Level, Upright::Vertical, Upright::Full, Upright::Guided] {
            assert_eq!(solve(mode, &[], &[], FOCAL), None);
        }
    }

    #[test]
    fn level_removes_a_rolled_camera() {
        let segments = facade(&roll(4.0));
        let correction = solve(Upright::Level, &segments, &[], FOCAL).unwrap();
        assert!(axis_error(&correction, &segments) < 0.05);
    }

    #[test]
    fn scattered_lines_are_no_reason_to_turn_a_photo() {
        let segments: Vec<Segment> = (0..60)
            .map(|index| {
                let tilt = ((index * 37) % 21) as f32 * 1.1 - 11.0;
                let (sin, cos) = tilt.to_radians().sin_cos();
                let (x, y) = ((index % 10) as f32 * 0.1 - 0.5, (index % 7) as f32 * 0.1 - 0.3);
                Segment { start: [x, y], end: [x + 0.12 * cos, y + 0.12 * sin] }
            })
            .collect();
        assert_eq!(solve(Upright::Level, &segments, &[], FOCAL), None);
        assert_eq!(solve(Upright::Auto, &segments, &[], FOCAL), None);
    }

    #[test]
    fn a_few_short_lines_are_not_enough_to_level_on() {
        let short = [0.0f32, 0.1, 0.2].map(|y| Segment { start: [-0.1, y], end: [0.1, y + 0.01] });
        assert_eq!(solve(Upright::Level, &short, &[], FOCAL), None);
    }

    #[test]
    fn level_leaves_keystone_alone() {
        let segments = facade(&camera(&pitch(12.0)));
        let correction = solve(Upright::Level, &segments, &[], FOCAL).unwrap();
        assert!(correction[2][0] == 0.0 && correction[2][1] == 0.0);
        assert!(converging_verticals(&correction, &segments) > 2.0);
    }

    #[test]
    fn vertical_makes_verticals_parallel_and_keeps_some_convergence_elsewhere() {
        let segments = facade(&camera(&mul(&pitch(14.0), &yaw(10.0))));
        let correction = solve(Upright::Vertical, &segments, &[], FOCAL).unwrap();
        assert!(converging_verticals(&correction, &segments) < 0.05);
        assert!(axis_error(&correction, &segments) > 1.0);
    }

    #[test]
    fn full_squares_both_axes() {
        let segments = facade(&camera(&mul(&roll(3.0), &mul(&pitch(12.0), &yaw(9.0)))));
        let correction = solve(Upright::Full, &segments, &[], FOCAL).unwrap();
        assert!(axis_error(&correction, &segments) < 0.05, "{}", axis_error(&correction, &segments));
    }

    #[test]
    fn auto_fixes_verticals_and_only_part_of_the_sideways_turn() {
        let segments = facade(&camera(&mul(&pitch(12.0), &yaw(10.0))));
        let auto = solve(Upright::Auto, &segments, &[], FOCAL).unwrap();
        assert!(converging_verticals(&auto, &segments) < 0.05);
        let full = solve(Upright::Full, &segments, &[], FOCAL).unwrap();
        assert!(axis_error(&auto, &segments) > axis_error(&full, &segments));
    }

    #[test]
    fn the_image_centre_stays_put() {
        let segments = facade(&camera(&pitch(15.0)));
        let correction = solve(Upright::Full, &segments, &[], FOCAL).unwrap();
        let center = project(&correction, [0.0, 0.0]);
        assert!(center[0].abs() < 1e-6 && center[1].abs() < 1e-6);
    }

    #[test]
    fn a_wrong_focal_length_still_straightens() {
        let segments = facade(&camera(&mul(&pitch(12.0), &yaw(9.0))));
        let correction = solve(Upright::Vertical, &segments, &[], 3.0).unwrap();
        assert!(converging_verticals(&correction, &segments) < 0.05);
    }

    #[test]
    fn a_corridor_is_not_mistaken_for_tilt() {
        let rays = (0..8).map(|index| {
            let angle = index as f32 * 0.25 - 0.9;
            Segment {
                start: [0.2 * angle.sin(), 0.2 * angle.cos()],
                end: [0.5 * angle.sin(), 0.5 * angle.cos()],
            }
        });
        let segments: Vec<Segment> = rays.collect();
        let correction = solve(Upright::Vertical, &segments, &[], FOCAL);
        assert!(correction.is_none_or(|matrix| matrix[2][0] == 0.0 && matrix[2][1] == 0.0));
    }

    #[test]
    fn stray_lines_do_not_upset_the_solution() {
        let mut segments = facade(&camera(&pitch(12.0)));
        for index in 0..6 {
            let offset = index as f32 * 0.08;
            segments.push(Segment { start: [-0.5 + offset, 0.1], end: [-0.2 + offset, 0.42] });
        }
        let correction = solve(Upright::Vertical, &segments, &[], FOCAL).unwrap();
        assert!(converging_verticals(&correction, &segments[..16]) < 0.05);
    }

    #[test]
    fn two_guides_straighten_their_lines() {
        let view = camera(&pitch(13.0));
        let guides = [segment(&view, [-0.4, -0.4], [-0.4, 0.4]), segment(&view, [0.4, -0.4], [0.4, 0.4])];
        let correction = solve(Upright::Guided, &[], &guides, FOCAL).unwrap();
        assert!(converging_verticals(&correction, &guides) < 0.05);
    }

    #[test]
    fn four_guides_square_the_view() {
        let view = camera(&mul(&pitch(10.0), &yaw(8.0)));
        let guides = [
            segment(&view, [-0.4, -0.4], [-0.4, 0.4]),
            segment(&view, [0.4, -0.4], [0.4, 0.4]),
            segment(&view, [-0.5, -0.3], [0.5, -0.3]),
            segment(&view, [-0.5, 0.3], [0.5, 0.3]),
        ];
        let correction = solve(Upright::Guided, &[], &guides, FOCAL).unwrap();
        assert!(axis_error(&correction, &guides) < 0.05);
    }

    #[test]
    fn a_single_guide_levels_the_photo() {
        let rolled = roll(3.0);
        let guides = [segment(&rolled, [-0.5, 0.0], [0.5, 0.0])];
        let correction = solve(Upright::Guided, &[], &guides, FOCAL).unwrap();
        assert!(axis_error(&correction, &guides) < 0.01);
    }
}
