//! Straight line detection, after the region growing of LSD (von Gioi et al.): pixels whose
//! gradients agree in direction gather into a thin region, and a thin enough region is a line.
//! Upright reads perspective from these, and `straightness` measures how straight they are,
//! which is how lens correction is verified.

use rayon::prelude::*;
use std::f32::consts::{PI, TAU};
use unlight_color::matrix::{apply, inverse};

/// How far a pixel's gradient may turn from its region's and still belong to it.
const ANGLE_TOLERANCE: f32 = 22.5 * PI / 180.0;
/// LSD's threshold for values on a 0..255 scale: below it, a gradient can't be told from
/// quantisation error at this tolerance.
const MIN_GRADIENT: f32 = 5.2;
/// Fraction of a segment's bounding rectangle its region must fill.
const MIN_DENSITY: f32 = 0.5;
/// Allowed RMS spread across a segment, as a constant plus a share of its length.
const WIDTH_BASE: f32 = 1.6;
const WIDTH_PER_LENGTH: f32 = 0.0015;
/// Sine of the largest angle between two pieces of one line.
const JOIN_MAX_TURN: f32 = 0.02;

/// Half the window searched across a line for its edge, in pixels.
const EDGE_SEARCH: f32 = 8.0;
/// How far an edge may wander from where it sits on average along its line.
const EDGE_TRACK: f32 = 5.0;
const EDGE_STEP: f32 = 0.25;
/// How far either side of a slope's peak still counts towards its centroid, in pixels.
const EDGE_CENTROID_REACH: f32 = 2.5;
/// Pixels along a line that share one measurement, which averages out grain and texture.
const CHUNK: usize = 12;
/// Lines with fewer measurable chunks than this can't be judged.
const MIN_CHUNKS: usize = 6;
const OUTLIER_LIMIT: f64 = 3.0;
const FIT_ROUNDS: usize = 4;
/// Trimmed from each end, where a line usually runs into something else.
const END_TRIM: f32 = 0.04;

/// A single-channel image, gamma-encoded on a 0..255 scale so thresholds mean what LSD's do.
#[derive(Clone, Debug, PartialEq)]
pub struct LumaImage {
    pub width: usize,
    pub height: usize,
    pub values: Vec<f32>,
}

impl LumaImage {
    /// Bilinear lookup at pixel-centre coordinates, repeating the edge.
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (x, y) = (x.clamp(0.0, (self.width - 1) as f32), y.clamp(0.0, (self.height - 1) as f32));
        let (left, top) = (x as usize, y as usize);
        let (right, bottom) = ((left + 1).min(self.width - 1), (top + 1).min(self.height - 1));
        let (fx, fy) = (x - left as f32, y - top as f32);
        let at = |column: usize, row: usize| self.values[row * self.width + column];
        let upper = at(left, top) + (at(right, top) - at(left, top)) * fx;
        let lower = at(left, bottom) + (at(right, bottom) - at(left, bottom)) * fx;
        upper + (lower - upper) * fy
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub start: [f32; 2],
    pub end: [f32; 2],
}

impl Segment {
    pub fn length(&self) -> f32 {
        (self.end[0] - self.start[0]).hypot(self.end[1] - self.start[1])
    }
}

struct Gradients {
    magnitude: Vec<f32>,
    /// Direction along the level line, so an edge has the same angle along its whole length.
    angle: Vec<f32>,
}

/// LSD's 2×2 mask. Its smaller support than Sobel's keeps neighbouring edges apart.
fn gradients(image: &LumaImage) -> Gradients {
    let (width, height) = (image.width, image.height);
    let mut magnitude = vec![0.0; width * height];
    let mut angle = vec![0.0; width * height];
    let rows = magnitude.par_chunks_mut(width).zip(angle.par_chunks_mut(width));
    rows.take(height - 1).enumerate().for_each(|(y, (magnitudes, angles))| {
        for x in 0..width - 1 {
            let at = |column: usize, row: usize| image.values[row * width + column];
            let (a, b, c, d) = (at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1));
            let (gx, gy) = ((b + d - a - c) * 0.5, (c + d - a - b) * 0.5);
            magnitudes[x] = gx.hypot(gy);
            angles[x] = gx.atan2(-gy);
        }
    });
    Gradients { magnitude, angle }
}

fn angle_difference(a: f32, b: f32) -> f32 {
    let difference = (a - b).abs() % TAU;
    difference.min(TAU - difference)
}

/// Gathers the connected pixels whose direction stays close to the region's running mean.
fn grow_region(seed: usize, width: usize, field: &Gradients, used: &mut [bool]) -> Vec<usize> {
    let mut region = vec![seed];
    used[seed] = true;
    let (mut sum_x, mut sum_y) = (field.angle[seed].cos(), field.angle[seed].sin());
    let mut next = 0;
    while next < region.len() {
        let pixel = region[next];
        next += 1;
        let (x, y) = ((pixel % width) as isize, (pixel / width) as isize);
        let mean = sum_y.atan2(sum_x);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= width as isize {
                continue;
            }
            let neighbour = ny as usize * width + nx as usize;
            if neighbour >= used.len()
                || used[neighbour]
                || field.magnitude[neighbour] < MIN_GRADIENT
                || angle_difference(field.angle[neighbour], mean) > ANGLE_TOLERANCE
            {
                continue;
            }
            used[neighbour] = true;
            region.push(neighbour);
            sum_x += field.angle[neighbour].cos();
            sum_y += field.angle[neighbour].sin();
        }
    }
    region
}

struct RegionFit {
    start: [f32; 2],
    end: [f32; 2],
    along: Vec<f32>,
    spread: f32,
}

/// Fits a line through a region by its principal axis, weighting pixels by gradient.
fn fit_region(region: &[usize], width: usize, field: &Gradients) -> RegionFit {
    let (mut total, mut mean_x, mut mean_y) = (0.0, 0.0, 0.0);
    let position = |pixel: usize| ((pixel % width) as f32 + 0.5, (pixel / width) as f32 + 0.5);
    for &pixel in region {
        let (x, y) = position(pixel);
        let weight = field.magnitude[pixel];
        total += weight;
        mean_x += weight * x;
        mean_y += weight * y;
    }
    (mean_x, mean_y) = (mean_x / total, mean_y / total);
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for &pixel in region {
        let (x, y) = position(pixel);
        let weight = field.magnitude[pixel];
        xx += weight * (x - mean_x) * (x - mean_x);
        yy += weight * (y - mean_y) * (y - mean_y);
        xy += weight * (x - mean_x) * (y - mean_y);
    }
    let axis = 0.5 * (2.0 * xy).atan2(xx - yy);
    let (sin, cos) = axis.sin_cos();
    let along: Vec<f32> = region
        .iter()
        .map(|&pixel| {
            let (x, y) = position(pixel);
            (x - mean_x) * cos + (y - mean_y) * sin
        })
        .collect();
    let spread: f32 = region
        .iter()
        .map(|&pixel| {
            let (x, y) = position(pixel);
            let across = -(x - mean_x) * sin + (y - mean_y) * cos;
            field.magnitude[pixel] * across * across
        })
        .sum();
    let low = along.iter().copied().fold(f32::MAX, f32::min);
    let high = along.iter().copied().fold(f32::MIN, f32::max);
    RegionFit {
        start: [mean_x + low * cos, mean_y + low * sin],
        end: [mean_x + high * cos, mean_y + high * sin],
        along,
        spread: (spread / total).sqrt(),
    }
}

/// Keeps a region that is straight, and halves one that bends, so a curved edge still gives
/// its straight stretches instead of being lost.
fn collect_segments(
    region: &[usize],
    width: usize,
    field: &Gradients,
    min_length: f32,
    segments: &mut Vec<Segment>,
) {
    if region.len() < 4 {
        return;
    }
    let fit = fit_region(region, width, field);
    let segment = Segment { start: fit.start, end: fit.end };
    let length = segment.length();
    if length < min_length {
        return;
    }
    if fit.spread <= WIDTH_BASE + WIDTH_PER_LENGTH * length {
        let rectangle = length * (fit.spread * 2.0 * 3f32.sqrt()).max(1.0);
        if region.len() as f32 / rectangle >= MIN_DENSITY {
            segments.push(segment);
        }
        return;
    }
    let (before, after): (Vec<_>, Vec<_>) = region
        .iter()
        .zip(&fit.along)
        .partition(|&(_, &position)| position < 0.0);
    let pixels = |side: Vec<(&usize, &f32)>| side.into_iter().map(|(&pixel, _)| pixel).collect::<Vec<_>>();
    collect_segments(&pixels(before), width, field, min_length, segments);
    collect_segments(&pixels(after), width, field, min_length, segments);
}

/// Finds line segments at least `min_length` pixels long, longest first.
pub fn detect_segments(image: &LumaImage, min_length: f32) -> Vec<Segment> {
    let (width, height) = (image.width, image.height);
    if width < 2 || height < 2 {
        return Vec::new();
    }
    let field = gradients(image);
    let mut seeds: Vec<usize> = (0..width * height)
        .filter(|&pixel| field.magnitude[pixel] >= MIN_GRADIENT)
        .collect();
    seeds.sort_unstable_by(|&a, &b| field.magnitude[b].total_cmp(&field.magnitude[a]).then(a.cmp(&b)));

    let mut used = vec![false; width * height];
    let mut segments = Vec::new();
    for seed in seeds {
        if !used[seed] {
            let region = grow_region(seed, width, &field, &mut used);
            collect_segments(&region, width, &field, min_length, &mut segments);
        }
    }
    segments.sort_by(|a, b| b.length().total_cmp(&a.length()));
    segments
}

/// Whether `other` continues `line`: nearly parallel, and both ends close to its extension.
fn continues(line: &Segment, other: &Segment, tolerance: f32) -> bool {
    let length = line.length();
    let direction = [(line.end[0] - line.start[0]) / length, (line.end[1] - line.start[1]) / length];
    let across = |point: [f32; 2]| {
        (point[0] - line.start[0]) * -direction[1] + (point[1] - line.start[1]) * direction[0]
    };
    let turn = ((other.end[0] - other.start[0]) * -direction[1] + (other.end[1] - other.start[1]) * direction[0])
        / other.length();
    turn.abs() <= JOIN_MAX_TURN && across(other.start).abs() <= tolerance && across(other.end).abs() <= tolerance
}

/// The span of two collinear segments, keeping the first one's direction.
fn joined(line: &Segment, other: &Segment) -> Segment {
    let direction = [line.end[0] - line.start[0], line.end[1] - line.start[1]];
    let along = |point: &[f32; 2]| point[0] * direction[0] + point[1] * direction[1];
    let points = [line.start, line.end, other.start, other.end];
    let pick = |ordering: std::cmp::Ordering| {
        points.iter().copied().reduce(|best, point| {
            if along(&point).total_cmp(&along(&best)) == ordering { point } else { best }
        })
    };
    let start = pick(std::cmp::Ordering::Less).unwrap_or(line.start);
    let end = pick(std::cmp::Ordering::Greater).unwrap_or(line.end);
    Segment { start, end }
}

/// Joins pieces of one interrupted line, longest first. Bow grows with the square of length, so
/// a line left in pieces hides nearly all the distortion along it.
pub fn join_collinear(segments: &[Segment], tolerance: f32) -> Vec<Segment> {
    let mut sorted = segments.to_vec();
    sorted.sort_by(|a, b| b.length().total_cmp(&a.length()));
    let mut used = vec![false; sorted.len()];
    let mut lines = Vec::new();
    for index in 0..sorted.len() {
        if used[index] {
            continue;
        }
        let mut line = sorted[index];
        for other in index + 1..sorted.len() {
            if !used[other] && continues(&line, &sorted[other], tolerance) {
                line = joined(&line, &sorted[other]);
                used[other] = true;
            }
        }
        lines.push(line);
    }
    lines.sort_by(|a, b| b.length().total_cmp(&a.length()));
    lines
}

/// How far a detected line strays from straight, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Straightness {
    pub length: f32,
    /// RMS distance from the best straight line.
    pub rms: f32,
    /// Peak-to-peak distance from it, which is the bow of a curved line.
    pub bow: f32,
    /// Signed bow: how far the middle sits from the chord through the ends, along the segment's
    /// left-hand normal. Unlike `bow`, the lens's part and the object's part of this simply add.
    pub sagitta: f32,
}

/// Signed brightness slope across a line at a point, at each step of the search window.
fn slope_profile(image: &LumaImage, point: [f32; 2], normal: [f32; 2]) -> Vec<f32> {
    let steps = (2.0 * EDGE_SEARCH / EDGE_STEP) as usize;
    let at = |offset: f32| image.sample(point[0] + normal[0] * offset, point[1] + normal[1] * offset);
    (0..=steps)
        .map(|step| {
            let offset = step as f32 * EDGE_STEP - EDGE_SEARCH;
            at(offset + 0.5) - at(offset - 0.5)
        })
        .collect()
}

/// The edge a whole line runs along: which way it steps, and where on average. Tracking this
/// one edge keeps the measurement from hopping between the two sides of a painted stripe.
struct EdgeLock {
    polarity: f32,
    center: usize,
}

fn lock_edge(profiles: &[Vec<f32>]) -> Option<EdgeLock> {
    let mut mean = vec![0.0; profiles.first()?.len()];
    for profile in profiles {
        mean.iter_mut().zip(profile).for_each(|(total, value)| *total += value);
    }
    let (center, &peak) = mean.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
    Some(EdgeLock { polarity: peak.signum(), center })
}

/// The sub-pixel position of the locked edge, and how strong it is. A sharp edge's slope has a
/// flat top, so a peak-fitting parabola wanders by a fraction of a pixel with the edge's
/// phase. The centroid of the top half doesn't, for any symmetric edge.
fn edge_offset(profile: &[f32], lock: &EdgeLock) -> Option<(f32, f32)> {
    let last = profile.len() - 1;
    let track = (EDGE_TRACK / EDGE_STEP) as usize;
    let span = lock.center.saturating_sub(track)..=(lock.center + track).min(last);
    let signed = |index: usize| profile[index] * lock.polarity;
    let peak = span.max_by(|&a, &b| signed(a).total_cmp(&signed(b)))?;
    let strength = signed(peak);
    if peak == 0 || peak == last || strength <= 0.0 {
        return None;
    }
    let reach = (EDGE_CENTROID_REACH / EDGE_STEP) as usize;
    let (mut weight, mut moment) = (0.0, 0.0);
    for index in peak.saturating_sub(reach)..=(peak + reach).min(last) {
        let excess = (signed(index) - strength * 0.5).max(0.0);
        weight += excess;
        moment += excess * index as f32;
    }
    Some((moment / weight * EDGE_STEP - EDGE_SEARCH, strength))
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let middle = values.len() / 2;
    let (_, value, _) = values.select_nth_unstable_by(middle, f32::total_cmp);
    Some(*value)
}

/// Least-squares polynomial of `degree` 1 or 2 through points, in `[1, x, x² - ½]` with `x`
/// spanning -1..1, so the last coefficient is the sagitta. Returns it with the residuals.
fn polynomial_fit(points: &[(f32, f32)], degree: usize) -> ([f64; 3], Vec<f64>) {
    let span = points.iter().map(|point| f64::from(point.0)).fold(1.0, f64::max);
    let terms = |t: f32| {
        let x = f64::from(t) / span * 2.0 - 1.0;
        [1.0, x, if degree == 2 { x * x - 0.5 } else { 0.0 }]
    };
    let mut normal = [[0.0; 3]; 3];
    let mut moment = [0.0; 3];
    for &(t, s) in points {
        let row = terms(t);
        for (index, value) in row.iter().enumerate() {
            moment[index] += value * f64::from(s);
            for (other, cross) in row.iter().enumerate() {
                normal[index][other] += value * cross;
            }
        }
    }
    if degree < 2 {
        normal[2][2] = 1.0;
    }
    let solved = inverse(&normal).map_or([0.0; 3], |solution| apply(&solution, &moment));
    let residuals = points
        .iter()
        .map(|&(t, s)| {
            let row = terms(t);
            f64::from(s) - row[0] * solved[0] - row[1] * solved[1] - row[2] * solved[2]
        })
        .collect();
    (solved, residuals)
}

/// Halves an image, which keeps grain from breaking up the lines being detected.
fn binned(image: &LumaImage) -> LumaImage {
    let (width, height) = (image.width / 2, image.height / 2);
    let at = |x: usize, y: usize| image.values[y * image.width + x];
    let values = (0..width * height)
        .map(|index| {
            let (x, y) = ((index % width) * 2, (index / width) * 2);
            (at(x, y) + at(x + 1, y) + at(x, y + 1) + at(x + 1, y + 1)) * 0.25
        })
        .collect();
    LumaImage { width, height, values }
}

/// How straight a photo's lines of at least `shortest` pixels are, longest first. Real edges
/// only join up into long lines at half size, so they're found there and measured at full size.
pub fn measure_long_lines(image: &LumaImage, shortest: f32) -> Vec<Straightness> {
    // How far pieces can stray and still be one line, in pixels of the binned image.
    const JOIN_TOLERANCE: f32 = 4.0;
    let doubled = |segment: &Segment| Segment {
        start: segment.start.map(|value| value * 2.0),
        end: segment.end.map(|value| value * 2.0),
    };
    join_collinear(&detect_segments(&binned(image), shortest * 0.25), JOIN_TOLERANCE)
        .iter()
        .filter(|segment| segment.length() >= shortest * 0.5)
        .filter_map(|segment| straightness(image, &doubled(segment)))
        .collect()
}

/// Measures how straight the edge under a segment really is, from sub-pixel edge positions.
pub fn straightness(image: &LumaImage, segment: &Segment) -> Option<Straightness> {
    let length = segment.length();
    let direction = [(segment.end[0] - segment.start[0]) / length, (segment.end[1] - segment.start[1]) / length];
    let normal = [-direction[1], direction[0]];
    let trimmed = length * (1.0 - 2.0 * END_TRIM);
    let chunks = trimmed as usize / CHUNK;

    let profiles: Vec<Vec<f32>> = (0..chunks * CHUNK)
        .map(|index| {
            let t = length * END_TRIM + index as f32;
            slope_profile(image, [segment.start[0] + direction[0] * t, segment.start[1] + direction[1] * t], normal)
        })
        .collect();
    let lock = lock_edge(&profiles)?;

    let mut points = Vec::new();
    for (chunk, group) in profiles.as_chunks::<CHUNK>().0.iter().enumerate() {
        let mut offsets: Vec<f32> = group
            .iter()
            .filter_map(|profile| edge_offset(profile, &lock))
            .filter(|&(_, strength)| strength >= MIN_GRADIENT)
            .map(|(offset, _)| offset)
            .collect();
        if offsets.len() >= CHUNK / 2
            && let Some(offset) = median(&mut offsets)
        {
            points.push(((chunk * CHUNK) as f32, offset));
        }
    }

    // Rejecting outliers against a parabola rather than a line, so that genuine bow survives.
    for _ in 0..FIT_ROUNDS {
        if points.len() < MIN_CHUNKS {
            return None;
        }
        let (_, residuals) = polynomial_fit(&points, 2);
        let mut sizes: Vec<f32> = residuals.iter().map(|value| value.abs() as f32).collect();
        let typical = f64::from(median(&mut sizes)?).max(0.05);
        let kept: Vec<(f32, f32)> = points
            .iter()
            .zip(&residuals)
            .filter(|&(_, residual)| residual.abs() <= OUTLIER_LIMIT * typical)
            .map(|(&point, _)| point)
            .collect();
        if kept.len() == points.len() {
            break;
        }
        points = kept;
    }
    if points.len() < MIN_CHUNKS {
        return None;
    }
    let (curve, _) = polynomial_fit(&points, 2);
    let (_, residuals) = polynomial_fit(&points, 1);
    let rms = (residuals.iter().map(|value| value * value).sum::<f64>() / residuals.len() as f64).sqrt();
    let low = residuals.iter().copied().fold(f64::MAX, f64::min);
    let high = residuals.iter().copied().fold(f64::MIN, f64::max);
    Some(Straightness { length, rms: rms as f32, bow: (high - low) as f32, sagitta: -curve[2] as f32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anti-aliased half-plane: bright where `distance` is positive.
    fn image_of(width: usize, height: usize, distance: impl Fn(f32, f32) -> f32 + Sync) -> LumaImage {
        let values = (0..width * height)
            .map(|index| {
                let (x, y) = ((index % width) as f32 + 0.5, (index / width) as f32 + 0.5);
                (distance(x, y) + 0.5).clamp(0.0, 1.0) * 180.0 + 30.0
            })
            .collect();
        LumaImage { width, height, values }
    }

    #[test]
    fn finds_a_diagonal_edge_end_to_end() {
        let image = image_of(400, 300, |x, y| 0.8 * x - y + 40.0);
        let segments = detect_segments(&image, 50.0);
        let longest = segments.first().unwrap();
        assert!(longest.length() > 400.0, "{longest:?}");
        let slope = (longest.end[1] - longest.start[1]) / (longest.end[0] - longest.start[0]);
        assert!((slope - 0.8).abs() < 0.01, "{slope}");
    }

    #[test]
    fn finds_nothing_in_a_flat_image() {
        let image = image_of(200, 200, |_, _| 0.0);
        assert!(detect_segments(&image, 20.0).is_empty());
    }

    #[test]
    fn short_lines_fall_below_the_minimum() {
        let image = image_of(200, 200, |x, y| 15.0 - (x - 100.0).abs().max((y - 100.0).abs()));
        assert!(detect_segments(&image, 60.0).is_empty());
        assert!(!detect_segments(&image, 20.0).is_empty());
    }

    #[test]
    fn a_circle_is_not_one_long_line() {
        let image = image_of(300, 300, |x, y| 110.0 - (x - 150.0).hypot(y - 150.0));
        let longest = detect_segments(&image, 30.0).first().map_or(0.0, Segment::length);
        assert!(longest < 120.0, "{longest}");
    }

    #[test]
    fn pieces_of_one_line_join_up() {
        let piece = |x0: f32, x1: f32| Segment { start: [x0, 0.1 * x0], end: [x1, 0.1 * x1] };
        let lines = join_collinear(&[piece(0.0, 100.0), piece(300.0, 500.0), piece(120.0, 280.0)], 2.0);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!((lines[0].length() - 500.0 * 1.01f32.sqrt()).abs() < 1e-2);
    }

    #[test]
    fn parallel_and_crossing_lines_stay_apart() {
        let flat = |y: f32| Segment { start: [0.0, y], end: [200.0, y] };
        let upright = Segment { start: [100.0, 0.0], end: [100.0, 100.0] };
        assert_eq!(join_collinear(&[flat(10.0), flat(30.0), upright], 2.0).len(), 3);
    }

    #[test]
    fn the_other_side_of_a_stripe_cannot_steal_the_measurement() {
        // A stripe with a dark edge that strengthens along its length: the strongest slope
        // anywhere changes sides, but the line's own edge stays put.
        let width = 500;
        let values = (0..width * 200)
            .map(|index| {
                let (x, y) = ((index % width) as f32, (index / width) as f32);
                let far_side = 60.0 + x * 0.3;
                if y < 100.0 {
                    20.0
                } else if y < 104.0 {
                    120.0
                } else {
                    120.0 - far_side
                }
            })
            .collect();
        let image = LumaImage { width, height: 200, values };
        let segment = Segment { start: [0.0, 100.0], end: [500.0, 100.0] };
        let measured = straightness(&image, &segment).unwrap();
        assert!(measured.bow < 0.3 && measured.sagitta.abs() < 0.2, "{measured:?}");
    }

    #[test]
    fn a_straight_edge_measures_as_straight() {
        let image = image_of(600, 300, |x, y| 0.1 * x - y + 120.0);
        let segment = detect_segments(&image, 100.0)[0];
        let measured = straightness(&image, &segment).unwrap();
        assert!(measured.bow < 0.3 && measured.rms < 0.1, "{measured:?}");
    }

    #[test]
    fn a_bowed_edge_measures_its_sagitta() {
        // A parabola dropping 3 pixels at the centre of a 600 pixel span.
        let sag = 3.0;
        let bowed = move |x: f32, y: f32| y - 150.0 - sag * (1.0 - ((x - 300.0) / 300.0).powi(2));
        let image = image_of(600, 300, bowed);
        let segment = detect_segments(&image, 100.0)[0];
        let measured = straightness(&image, &segment).unwrap();
        let measured_span = measured.length / 600.0 * (1.0 - 2.0 * END_TRIM);
        let expected = sag * measured_span * measured_span;
        assert!((measured.bow - expected).abs() < 0.3, "{measured:?} expected {expected}");
        assert!((measured.sagitta.abs() - expected).abs() < 0.3, "{measured:?} expected {expected}");
    }
}
