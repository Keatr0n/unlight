//! Panorama merge. Everything about where frames go is worked out on small copies. Only then is
//! each full frame read, warped onto the canvas, folded into the blend and dropped, so memory
//! follows the size of the panorama and one frame, whatever the number of photos.

use crate::{
    BoundaryWarp, Frame, FrameSource, FrameLens, Image, MergeError, Merged, Plane, Progress, Result,
    blend::Blender,
    bundle::{Correspondence, PairMatches, adjust, estimate_focal, initial_cameras, leveled, pair_error},
    crop::largest_rectangle,
    detect_features,
    features::Feature,
    fit_homography,
    frames::SENSOR_CLIP,
    image::{Rgb, luma},
    lens::Centred,
    linalg::{normalized, transpose, yaw_rotation},
    match_features,
    output::panorama_metadata,
    projection::{Canvas, Projection},
    refine::polish_pair,
    resample::{bilinear_rgb, lanczos_channel},
    seam::{Layer, NO_FRAME, select_seams},
    stitch_gains::{OverlapSample, StitchGains, solve_gains},
};
use rayon::prelude::*;
use std::sync::Arc;
use unlight_color::{
    CameraProfile,
    matrix::{Mat3, Vec3, apply, mul},
};
use unlight_raw::Opcodes;

/// The largest edge a texture has on the GPUs this runs on, and so on a merged photo. The
/// engine holds a photo in one texture, so this is where panoramas have to stop.
pub const DEFAULT_MAX_EDGE: u32 = 16384;

const FEATURE_EDGE: usize = 1600;
const FEATURE_BUDGET: usize = 2500;
/// These three are in pixels of the image features were found on.
const RANSAC_PIXELS: f64 = 2.5;
/// After adjustment, a pair whose matches sit further off than this was never really a pair.
const FALSE_PAIR_PIXELS: f64 = 4.0;
/// If the fit is worse than this, also try the focal length homographies imply.
const GOOD_FIT_PIXELS: f64 = 1.0;
const MIN_INLIERS: usize = 12;
const MIN_INLIER_SHARE: f64 = 0.2;
const FULL_FRAME_DIAGONAL_MM: f64 = 43.27;
/// Assumed when a file gives no focal length and the geometry can't supply one either.
const FALLBACK_FOCAL_35MM: f64 = 35.0;
const BORDER_SAMPLES: usize = 32;
const POLISH_LOG_FLOOR: f32 = 2e-4;
const POLISH_BLUR: f32 = 0.7;
/// Long edge of the canvas that seams, exposure and masks are worked out on.
const WORKING_EDGE: usize = 2048;
/// The broadest blending band spans this share of the panorama's long edge, so that a brightness
/// difference across a seam fades out over a distance that suits the picture. Fewer bands leave
/// a soft stripe down an evenly lit sky or sheet of water.
const WIDEST_BAND_SHARE: usize = 16;
const FEWEST_BANDS: u32 = 5;
const MOST_BANDS: u32 = 9;
/// A perspective projection stretches without limit as it nears 180°.
const WIDEST_PERSPECTIVE: f64 = 150.0;
/// Neighbouring frames that leave less than this share of their combined reach as overlap
/// don't close a circle. Stitching across a gap that thin would be guesswork.
const LOOP_OVERLAP: f64 = 0.95;
const USABLE_BOTTOM: f32 = 0.012;
const USABLE_TOP: f32 = 0.85;
const SAMPLES_PER_PAIR: usize = 1500;
const MIN_MASK: f32 = 1e-3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanoramaOptions {
    pub projection: Projection,
    /// How far to stretch the outline to fill its rectangle, from 0 to 1.
    pub boundary_warp: f32,
    pub auto_crop: bool,
    /// The largest long edge to output.
    pub max_edge: u32,
}

impl Default for PanoramaOptions {
    fn default() -> Self {
        Self { projection: Projection::Spherical, boundary_warp: 0.0, auto_crop: true, max_edge: DEFAULT_MAX_EDGE }
    }
}

#[derive(Clone, Debug)]
pub struct PanoramaReport {
    /// Input frames that made it in, in the order given.
    pub used: Vec<usize>,
    /// Frames nothing connected to the rest.
    pub dropped: Vec<usize>,
    /// The frame whose exposure the panorama took.
    pub reference: usize,
    /// In pixels of the full frame.
    pub focal: f64,
    /// Root-mean-square reprojection error, in pixels of the full frame.
    pub rms: f64,
    pub closed_loop: bool,
    /// Per used frame: the rotation found.
    pub cameras: Vec<Mat3>,
    /// Per used frame.
    pub gains: Vec<[f32; 3]>,
    pub vignette: [f32; 3],
    /// Whether every used frame had lens data.
    pub lens_known: bool,
    /// Output pixels per source pixel. Under 1 means the size limit reduced the panorama.
    pub scale: f64,
    /// Share of the output that has no photo in it.
    pub empty_share: f32,
    /// The surface the output was laid out on.
    pub canvas: Canvas,
    /// Which used frame each pixel of a small version of the output came from.
    pub seams: Vec<u16>,
    pub seam_size: [usize; 2],
}

/// A frame's place in the world.
#[derive(Clone)]
struct Placed {
    /// Index into the source's frames.
    index: usize,
    camera: Mat3,
    /// Full-size pixels.
    focal: f64,
    width: f64,
    height: f64,
    lens: FrameLens,
}

impl Placed {
    /// Where a world ray lands, in full-size pixels from the middle of the frame.
    #[inline]
    fn ideal(&self, ray: &Vec3) -> Option<[f64; 2]> {
        let [x, y, z] = apply(&self.camera, ray);
        (z > 1e-6).then(|| [self.focal * x / z, self.focal * y / z])
    }

    fn aspect(&self) -> f32 {
        (self.width / self.height) as f32
    }

    /// Whether an undistorted position has a pixel behind it. Correcting a lens changes the
    /// frame's outline, so this has to go through the lens.
    #[inline]
    fn contains(&self, ideal: [f64; 2]) -> bool {
        let position = Centred([(ideal[0] / self.height) as f32, (ideal[1] / self.height) as f32]);
        let Centred([x, y]) = self.lens.source_position(position);
        x.abs() <= self.aspect() * 0.5 && y.abs() <= 0.5
    }

    fn half_diagonal(&self) -> f64 {
        0.5 * self.width.hypot(self.height)
    }

    fn world_ray(&self, ideal: [f64; 2]) -> Vec3 {
        apply(&transpose(&self.camera), &normalized(&[ideal[0] / self.focal, ideal[1] / self.focal, 1.0]))
    }

    /// World rays around the edge of the frame, once its lens is corrected.
    fn border(&self) -> Vec<Vec3> {
        let (half_x, half_y) = (self.aspect() * 0.5, 0.5);
        (0..BORDER_SAMPLES)
            .flat_map(|step| {
                let t = step as f32 / (BORDER_SAMPLES - 1) as f32 * 2.0 - 1.0;
                [[t * half_x, -half_y], [t * half_x, half_y], [-half_x, t * half_y], [half_x, t * half_y]]
            })
            .map(|edge| {
                let Centred([x, y]) = self.lens.ideal_position(Centred(edge));
                self.world_ray([f64::from(x) * self.height, f64::from(y) * self.height])
            })
            .collect()
    }

    /// Longitude of the view's centre, around the vertical axis.
    fn yaw(&self) -> f64 {
        self.camera[2][0].atan2(self.camera[2][2])
    }
}

fn check_compatible(frames: &[Frame]) -> Result<()> {
    if frames.len() < 2 {
        return Err(MergeError::TooFewFrames);
    }
    if let Some(frame) = frames.iter().find(|frame| frame.metadata.merge.is_some()) {
        return Err(MergeError::AlreadyMerged(frame.path.clone()));
    }
    let first = &frames[0];
    if let Some(frame) = frames.iter().find(|frame| (frame.width, frame.height) != (first.width, first.height)) {
        return Err(MergeError::SizeMismatch(frame.path.clone()));
    }
    Ok(())
}

fn balance_gain(frame: &Frame, reference: &Frame) -> Rgb {
    std::array::from_fn(|c| reference.balance[c] / frame.balance[c])
}

/// A frame's small copy with its lens corrected and its white balance matched to `reference`.
fn working_copy(frame: &Frame, lens: &FrameLens, reference: &Frame) -> Image {
    let gain = balance_gain(frame, reference);
    let mut copy = lens.corrected_copy(&frame.reduced);
    if gain != [1.0; 3] {
        copy.pixels.par_iter_mut().for_each(|pixel| *pixel = std::array::from_fn(|c| pixel[c] * gain[c]));
    }
    copy
}

/// A feature's place on the fine copy it was reduced from by `factor`, from the middle.
fn centred(feature: &Feature, factor: f64, fine: &Image) -> [f64; 2] {
    [
        (f64::from(feature.x) + 0.5) * factor - fine.width as f64 * 0.5,
        (f64::from(feature.y) + 0.5) * factor - fine.height as f64 * 0.5,
    ]
}

struct VerifiedPair {
    matches: PairMatches,
    focal: Option<f64>,
}

/// Matches two frames, keeping the result only if it holds up as a single homography. Points
/// come back in pixels of the fine copies, which are `factor` times the size features were
/// found at.
fn verify_pair(first: usize, second: usize, features: &[Vec<Feature>], fine: &[Image], factor: f64) -> Option<VerifiedPair> {
    let matches = match_features(&features[first], &features[second]);
    if matches.len() < MIN_INLIERS {
        return None;
    }
    let from: Vec<[f64; 2]> = matches.iter().map(|&(a, _)| centred(&features[first][a as usize], factor, &fine[first])).collect();
    let to: Vec<[f64; 2]> = matches.iter().map(|&(_, b)| centred(&features[second][b as usize], factor, &fine[second])).collect();
    let fit = fit_homography(&from, &to, RANSAC_PIXELS * factor)?;
    let enough = fit.inliers.len() >= MIN_INLIERS
        && fit.inliers.len() as f64 >= matches.len() as f64 * MIN_INLIER_SHARE;
    if !enough {
        return None;
    }
    let points = fit.inliers.iter().map(|&index| Correspondence { first: from[index], second: to[index] }).collect();
    Some(VerifiedPair { matches: PairMatches { first, second, points }, focal: estimate_focal(&fit.matrix) })
}

/// The focal length the file implies, in pixels of a frame of this diagonal.
fn stated_focal(frame: &Frame) -> Option<f64> {
    let equivalent = f64::from(frame.metadata.focal_length_35mm);
    (equivalent > 0.0).then(|| equivalent / FULL_FRAME_DIAGONAL_MM * (frame.width as f64).hypot(frame.height as f64))
}

/// Where each frame points and the focal length, on working-copy pixels, along with the frames
/// that could be placed.
struct Solution {
    used: Vec<usize>,
    cameras: Vec<Mat3>,
    focal: f64,
    rms: f64,
    pairs: Vec<PairMatches>,
}

fn strongest_frame(pairs: &[PairMatches], count: usize) -> usize {
    let mut totals = vec![0usize; count];
    for pair in pairs {
        totals[pair.first] += pair.points.len();
        totals[pair.second] += pair.points.len();
    }
    (0..count).max_by_key(|&index| totals[index]).unwrap_or(0)
}

/// Bundle adjusts from one starting focal length, throwing out any pair that doesn't fit by
/// `tolerance` pixels.
fn solve_from(count: usize, pairs: &[PairMatches], start: f64, tolerance: f64) -> Option<Solution> {
    let mut pairs = pairs.to_vec();
    loop {
        let root = strongest_frame(&pairs, count);
        let placed = initial_cameras(count, root, &pairs, start);
        let used: Vec<usize> = (0..count).filter(|&index| placed[index].is_some()).collect();
        if used.len() < 2 {
            return None;
        }
        let mut position = vec![usize::MAX; count];
        used.iter().enumerate().for_each(|(at, &index)| position[index] = at);
        let local: Vec<PairMatches> = pairs
            .iter()
            .filter(|pair| position[pair.first] != usize::MAX && position[pair.second] != usize::MAX)
            .map(|pair| PairMatches { first: position[pair.first], second: position[pair.second], points: pair.points.clone() })
            .collect();
        let mut cameras: Vec<Mat3> = used.iter().filter_map(|&index| placed[index]).collect();
        let mut focal = start;
        let rms = adjust(&mut cameras, &mut focal, &local, position[root]);

        let bad: Vec<(usize, usize)> = local
            .iter()
            .filter(|pair| pair_error(pair, &cameras, focal) > tolerance)
            .map(|pair| (used[pair.first], used[pair.second]))
            .collect();
        // A loose set can leave everything equally bad. Only single out the odd ones.
        if bad.is_empty() || bad.len() >= local.len() {
            return Some(Solution { used, cameras, focal, rms, pairs: local });
        }
        pairs.retain(|pair| !bad.contains(&(pair.first, pair.second)));
    }
}

/// Re-places every match by comparing pixels on the fine copies, then adjusts again.
fn polish_solution(solution: &mut Solution, fine: &[Image], clip: f32) {
    let top = clip.max(1e-6);
    let planes: Vec<Plane> = solution
        .used
        .iter()
        .map(|&index| fine[index].plane(|pixel| (luma(pixel).max(0.0) / top + POLISH_LOG_FLOOR).ln()).blurred(POLISH_BLUR))
        .collect();
    let focal = solution.focal;
    let scale = [[focal, 0.0, 0.0], [0.0, focal, 0.0], [0.0, 0.0, 1.0]];
    let unscale = [[1.0 / focal, 0.0, 0.0], [0.0, 1.0 / focal, 0.0], [0.0, 0.0, 1.0]];
    let polished: Vec<PairMatches> = solution
        .pairs
        .iter()
        .map(|pair| {
            let relative = mul(&solution.cameras[pair.second], &transpose(&solution.cameras[pair.first]));
            let between = mul(&scale, &mul(&relative, &unscale));
            polish_pair(pair, &planes[pair.first], &planes[pair.second], &between)
        })
        .collect();
    let root = strongest_frame(&polished, solution.cameras.len());
    solution.rms = adjust(&mut solution.cameras, &mut solution.focal, &polished, root);
    solution.pairs = polished;
}

/// Rotates the world about the vertical so the panorama faces forward and the back of the
/// sphere falls in the widest gap between frames. Returns whether frames close the circle.
fn face_forward(placed: &mut [Placed]) -> bool {
    let mut yaws: Vec<(f64, usize)> = placed.iter().enumerate().map(|(index, frame)| (frame.yaw(), index)).collect();
    yaws.sort_by(|a, b| a.0.total_cmp(&b.0));
    let tau = std::f64::consts::TAU;
    let (widest, after) = (0..yaws.len())
        .map(|at| {
            let next = (at + 1) % yaws.len();
            let gap = (yaws[next].0 - yaws[at].0).rem_euclid(tau);
            (if yaws.len() == 1 { tau } else { gap }, at)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap_or((tau, 0));
    let cut = yaws[after].0 + widest * 0.5;
    let front = cut + std::f64::consts::PI;
    let spin = yaw_rotation(front);
    placed.iter_mut().for_each(|frame| frame.camera = mul(&frame.camera, &spin));

    let reach = |frame: &Placed| {
        let middle = frame.yaw();
        frame
            .border()
            .iter()
            .map(|ray| (ray[0].atan2(ray[2]) - middle + std::f64::consts::PI).rem_euclid(tau) - std::f64::consts::PI)
            .map(f64::abs)
            .fold(0.0, f64::max)
    };
    let (a, b) = (yaws[after].1, yaws[(after + 1) % yaws.len()].1);
    widest < (reach(&placed[a]) + reach(&placed[b])) * LOOP_OVERLAP
}

/// The extent of every frame's border in a projection.
fn extent(placed: &[Placed], projection: Projection, closed: bool) -> Result<([f64; 2], [f64; 2])> {
    let mut min = [f64::MAX; 2];
    let mut max = [f64::MIN; 2];
    let mut widest: f64 = 0.0;
    let mut unreachable = false;
    for frame in placed {
        for ray in frame.border() {
            widest = widest.max(ray[2].clamp(-1.0, 1.0).acos());
            match projection.project(&ray) {
                Some(point) => {
                    (0..2).for_each(|axis| {
                        min[axis] = min[axis].min(point[axis]);
                        max[axis] = max[axis].max(point[axis]);
                    });
                }
                None => unreachable = true,
            }
        }
    }
    let degrees = widest.to_degrees() * 2.0;
    if projection == Projection::Perspective && (unreachable || closed || degrees > WIDEST_PERSPECTIVE) {
        return Err(MergeError::FieldOfView { degrees: if closed { 360.0 } else { degrees } });
    }
    if unreachable {
        return Err(MergeError::NoOverlap);
    }
    if closed {
        min[0] = -std::f64::consts::PI;
        max[0] = std::f64::consts::PI;
    }
    Ok((min, max))
}

fn pixel_rays(canvas: &Canvas, width: usize, height: usize) -> Vec<Vec3> {
    (0..width * height)
        .into_par_iter()
        .map(|at| {
            let u = ((at % width) as f64 + 0.5) / width as f64;
            let v = ((at / width) as f64 + 0.5) / height as f64;
            canvas.ray(u, v)
        })
        .collect()
}

fn covered_anywhere(placed: &[Placed], rays: &[Vec3]) -> Vec<bool> {
    rays.par_iter()
        .map(|ray| placed.iter().any(|frame| frame.ideal(ray).is_some_and(|ideal| frame.contains(ideal))))
        .collect()
}

/// Shaves a pixel off the outline, because a coarse mask's edge can't be trusted to the pixel.
fn shrunk(mask: &[bool], width: usize, height: usize) -> Vec<bool> {
    let plane = Plane::from_fn(width, height, |x, y| f32::from(u8::from(mask[y * width + x])));
    plane.eroded(1).values.iter().map(|value| *value > 0.5).collect()
}

/// A working canvas of `long_edge` pixels on its longest side.
fn working_size(canvas: &Canvas, long_edge: usize) -> ([usize; 2], f64) {
    let extent = canvas.extent();
    let scale = long_edge as f64 / extent[0].max(extent[1]).max(1e-9);
    (canvas.size(scale), scale)
}

/// One frame on the working canvas, and how far each of its pixels is from the frame's middle.
struct Laid {
    layer: Layer,
    radius: Vec<f32>,
}

fn lay_out(frame: &Placed, copy: &Image, rays: &[Vec3], [width, height]: [usize; 2], scale: f64) -> Laid {
    let reduction = ((frame.focal / (frame.width / copy.width as f64) / scale) as usize).max(1);
    let copy = if reduction > 1 { copy.reduced(reduction) } else { copy.clone() };
    let to_copy = copy.width as f64 / frame.width;
    let half = [copy.width as f64 * 0.5, copy.height as f64 * 0.5];
    let reach = frame.half_diagonal();
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0usize, 0usize);
    let inside: Vec<Option<[f64; 2]>> = rays
        .par_iter()
        .map(|ray| frame.ideal(ray).filter(|ideal| frame.contains(*ideal)))
        .collect();
    for (at, place) in inside.iter().enumerate() {
        if place.is_some() {
            let (x, y) = (at % width, at / width);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if left >= right || top >= bottom {
        return Laid { layer: Layer::default(), radius: Vec::new() };
    }
    let (tile_width, tile_height) = (right - left, bottom - top);
    let mut layer = Layer {
        left,
        top,
        width: tile_width,
        height: tile_height,
        pixels: vec![[0.0; 3]; tile_width * tile_height],
        covered: vec![false; tile_width * tile_height],
    };
    let mut radius = vec![0.0f32; tile_width * tile_height];
    for y in 0..tile_height {
        for x in 0..tile_width {
            let at = y * tile_width + x;
            if let Some(ideal) = inside[(y + top) * width + x + left] {
                let sx = (ideal[0] * to_copy + half[0] - 0.5) as f32;
                let sy = (ideal[1] * to_copy + half[1] - 0.5) as f32;
                layer.pixels[at] = bilinear_rgb(&copy, sx, sy);
                layer.covered[at] = true;
                radius[at] = (ideal[0].hypot(ideal[1]) / reach) as f32;
            }
        }
    }
    Laid { layer, radius }
}

/// Reads every overlap for how frames differ at the same point of the scene.
fn overlap_samples(laid: &[Laid], clip: Rgb) -> Vec<OverlapSample> {
    let usable = |pixel: Rgb| {
        (0..3).all(|c| pixel[c] < clip[c] * USABLE_TOP && pixel[c] > 0.0) && luma(pixel) > clip[1] * USABLE_BOTTOM
    };
    let log = |pixel: Rgb| pixel.map(f32::ln);
    let mut pairs = Vec::new();
    for first in 0..laid.len() {
        for second in first + 1..laid.len() {
            pairs.push((first, second));
        }
    }
    pairs
        .par_iter()
        .flat_map_iter(|&(first, second)| {
            let (a, b) = (&laid[first], &laid[second]);
            let (la, lb) = (&a.layer, &b.layer);
            let left = la.left.max(lb.left);
            let right = (la.left + la.width).min(lb.left + lb.width);
            let top = la.top.max(lb.top);
            let bottom = (la.top + la.height).min(lb.top + lb.height);
            let mut samples = Vec::new();
            if left >= right || top >= bottom {
                return samples.into_iter();
            }
            let area = (right - left) * (bottom - top);
            let stride = ((area as f64 / SAMPLES_PER_PAIR as f64).sqrt() as usize).max(1);
            for y in (top..bottom).step_by(stride) {
                for x in (left..right).step_by(stride) {
                    let (at_a, at_b) = (
                        (y - la.top) * la.width + x - la.left,
                        (y - lb.top) * lb.width + x - lb.left,
                    );
                    if !la.covered[at_a] || !lb.covered[at_b] {
                        continue;
                    }
                    let (pixel_a, pixel_b) = (la.pixels[at_a], lb.pixels[at_b]);
                    if usable(pixel_a) && usable(pixel_b) {
                        samples.push(OverlapSample {
                            first,
                            second,
                            radius: [a.radius[at_a], b.radius[at_b]],
                            log: [log(pixel_a), log(pixel_b)],
                        });
                    }
                }
            }
            samples.into_iter()
        })
        .collect()
}

fn compensate(laid: &mut [Laid], gains: &StitchGains) {
    laid.iter_mut().enumerate().for_each(|(index, frame)| {
        let gain = gains.gains[index];
        let radius = &frame.radius;
        frame.layer.pixels.par_iter_mut().zip(radius).for_each(|(pixel, radius)| {
            let falloff = gains.falloff(*radius);
            *pixel = std::array::from_fn(|c| pixel[c] * gain[c] * falloff);
        });
    });
}

/// A full-size tile of one frame on the canvas.
struct Tile {
    origin: [usize; 2],
    image: Image,
    covered: Plane,
    mask: Plane,
}

struct Target<'a> {
    canvas: &'a Canvas,
    size: [usize; 2],
    labels: &'a [u16],
    label_size: [usize; 2],
}

/// Warps a frame into its tile, undoing the lens as it goes. The frame was resampled only here.
fn warp_tile(frame: &Placed, image: &Image, label: u16, gain: Rgb, gains: &StitchGains, region: [usize; 4], target: &Target<'_>) -> Tile {
    let [left, top, tile_width, tile_height] = region;
    let [canvas_width, canvas_height] = target.size;
    let aspect = frame.aspect();
    let (source_width, source_height) = (image.width as f32, image.height as f32);
    let reach = frame.half_diagonal();
    let [label_width, label_height] = target.label_size;
    let mine = Plane::from_fn(label_width, label_height, |x, y| {
        f32::from(u8::from(target.labels[y * label_width + x] == label))
    });
    let to_label = [label_width as f32 / canvas_width as f32, label_height as f32 / canvas_height as f32];
    let mut pixels = vec![[0.0f32; 3]; tile_width * tile_height];
    let mut covered = vec![0.0f32; tile_width * tile_height];
    let mut mask = vec![0.0f32; tile_width * tile_height];
    pixels
        .par_chunks_mut(tile_width)
        .zip(covered.par_chunks_mut(tile_width))
        .zip(mask.par_chunks_mut(tile_width))
        .enumerate()
        .for_each(|(row, ((pixels, covered), mask))| {
            let y = row + top;
            for x in 0..tile_width {
                let canvas_x = x + left;
                let u = (canvas_x as f64 + 0.5) / canvas_width as f64;
                let v = (y as f64 + 0.5) / canvas_height as f64;
                let ray = target.canvas.ray(u, v);
                let Some(ideal) = frame.ideal(&ray) else {
                    continue;
                };
                let position = Centred([(ideal[0] / frame.height) as f32, (ideal[1] / frame.height) as f32]);
                let green = frame.lens.source_position(position);
                let place = |at: Centred| {
                    [(at.0[0] / aspect + 0.5) * source_width - 0.5, (at.0[1] + 0.5) * source_height - 0.5]
                };
                let [sx, sy] = place(green);
                if sx < 0.0 || sy < 0.0 || sx > source_width - 1.0 || sy > source_height - 1.0 {
                    continue;
                }
                let lens_gain = frame.lens.gain_at(green, aspect);
                let radius = (ideal[0].hypot(ideal[1]) / reach) as f32;
                let falloff = gains.falloff(radius);
                let [red_at, blue_at] = frame.lens.channel_positions(green).map_or([[sx, sy]; 2], |far| far.map(place));
                let sampled = [
                    lanczos_channel(image, red_at[0], red_at[1], 0),
                    lanczos_channel(image, sx, sy, 1),
                    lanczos_channel(image, blue_at[0], blue_at[1], 2),
                ];
                pixels[x] = std::array::from_fn(|c| sampled[c] * lens_gain[c] * gain[c] * falloff);
                covered[x] = 1.0;
                let label_x = (canvas_x as f32 + 0.5) * to_label[0] - 0.5;
                let label_y = (y as f32 + 0.5) * to_label[1] - 0.5;
                mask[x] = mine.bilinear(label_x, label_y).max(MIN_MASK);
            }
        });
    Tile {
        origin: [left, top],
        image: Image { width: tile_width, height: tile_height, pixels },
        covered: Plane { width: tile_width, height: tile_height, values: covered },
        mask: Plane { width: tile_width, height: tile_height, values: mask },
    }
}

fn bands_for(size: [usize; 2]) -> usize {
    let widest = (size[0].max(size[1]) / WIDEST_BAND_SHARE).max(1);
    widest.ilog2().clamp(FEWEST_BANDS, MOST_BANDS) as usize
}

/// The canvas rectangle a frame touches, snapped to the blender's grid.
fn tile_region(layer: &Layer, working: [usize; 2], size: [usize; 2], grid: usize) -> [usize; 4] {
    let (scale_x, scale_y) = (size[0] as f64 / working[0] as f64, size[1] as f64 / working[1] as f64);
    let snap_down = |value: usize| value / grid * grid;
    let left = snap_down(((layer.left as f64 - 1.0).max(0.0) * scale_x) as usize);
    let top = snap_down(((layer.top as f64 - 1.0).max(0.0) * scale_y) as usize);
    let right = (((layer.left + layer.width + 1) as f64 * scale_x).ceil() as usize).min(size[0]);
    let bottom = (((layer.top + layer.height + 1) as f64 * scale_y).ceil() as usize).min(size[1]);
    [left, top, right.saturating_sub(left).max(1), bottom.saturating_sub(top).max(1)]
}

/// Stitches a set of overlapping frames into one image. Lens corrections go into the pixels
/// here, and the merged file says so.
pub fn merge_panorama(
    source: &dyn FrameSource,
    options: &PanoramaOptions,
    progress: &Progress,
) -> Result<(Merged, PanoramaReport)> {
    let frames = source.frames();
    check_compatible(frames)?;
    let colour_reference = &frames[frames.len() / 2];
    let clip = colour_reference.balance.map(|gain| gain * SENSOR_CLIP);

    progress.step("Correcting lenses", 0.02)?;
    let lenses: Vec<FrameLens> = frames.iter().map(FrameLens::for_frame).collect();
    let fine: Vec<Image> = frames
        .iter()
        .zip(&lenses)
        .map(|(frame, lens)| working_copy(frame, lens, colour_reference))
        .collect();
    let copies: Vec<Image> = fine.iter().map(|image| image.fitted(FEATURE_EDGE)).collect();
    let factor = fine[0].width as f64 / copies[0].width as f64;

    progress.step("Finding features", 0.05)?;
    let features: Vec<Vec<Feature>> = copies.iter().map(|copy| detect_features(copy, clip[1], FEATURE_BUDGET)).collect();

    progress.step("Matching frames", 0.1)?;
    let candidates: Vec<(usize, usize)> =
        (0..frames.len()).flat_map(|first| (first + 1..frames.len()).map(move |second| (first, second))).collect();
    let verified: Vec<VerifiedPair> = candidates
        .par_iter()
        .filter_map(|&(first, second)| verify_pair(first, second, &features, &fine, factor))
        .collect();
    drop(features);
    progress.check()?;
    if verified.is_empty() {
        return Err(MergeError::NoOverlap);
    }
    let to_fine = fine[0].width as f64 / frames[0].width as f64;
    let mut implied: Vec<f64> = verified.iter().filter_map(|pair| pair.focal).collect();
    implied.sort_by(f64::total_cmp);
    let implied = implied.get(implied.len() / 2).copied();
    let pairs: Vec<PairMatches> = verified.into_iter().map(|pair| pair.matches).collect();

    progress.step("Solving camera positions", 0.15)?;
    let stated = stated_focal(colour_reference).map(|focal| focal * to_fine);
    let fallback = FALLBACK_FOCAL_35MM / FULL_FRAME_DIAGONAL_MM * (fine[0].width as f64).hypot(fine[0].height as f64);
    let start = stated.or(implied).unwrap_or(fallback);
    let tolerance = FALSE_PAIR_PIXELS * factor;
    let mut solution = solve_from(frames.len(), &pairs, start, tolerance).ok_or(MergeError::NoOverlap)?;
    if solution.rms > GOOD_FIT_PIXELS * factor
        && let (Some(_), Some(other)) = (stated, implied)
        && let Some(retried) = solve_from(frames.len(), &pairs, other, tolerance)
        && retried.rms < solution.rms
        && retried.used.len() >= solution.used.len()
    {
        solution = retried;
    }
    progress.step("Refining matches", 0.18)?;
    polish_solution(&mut solution, &fine, clip[1]);
    drop(fine);
    let focal_full = solution.focal / to_fine;
    let rms_full = solution.rms / to_fine;

    let flips: Vec<i32> = solution.used.iter().map(|&index| frames[index].metadata.orientation).collect();
    let cameras = leveled(&solution.cameras, &flips);
    let mut placed: Vec<Placed> = solution
        .used
        .iter()
        .zip(&cameras)
        .map(|(&index, camera)| Placed {
            index,
            camera: *camera,
            focal: focal_full,
            width: frames[index].width as f64,
            height: frames[index].height as f64,
            lens: lenses[index].clone(),
        })
        .collect();
    let closed_loop = face_forward(&mut placed);

    progress.step("Laying out the canvas", 0.2)?;
    let (min, max) = extent(&placed, options.projection, closed_loop)?;
    let mut canvas = Canvas::new(options.projection, min, max);
    let ([mask_width, mask_height], _) = working_size(&canvas, WORKING_EDGE);
    if options.boundary_warp > 0.0 {
        let covered = covered_anywhere(&placed, &pixel_rays(&canvas, mask_width, mask_height));
        let warp = BoundaryWarp::fit(&covered, mask_width, mask_height, options.boundary_warp.clamp(0.0, 1.0));
        canvas.warp = Some(Arc::new(warp));
    }
    if options.auto_crop {
        let covered = covered_anywhere(&placed, &pixel_rays(&canvas, mask_width, mask_height));
        let covered = shrunk(&covered, mask_width, mask_height);
        if let Some([left, top, width, height]) = largest_rectangle(&covered, mask_width, mask_height) {
            canvas.window = [
                left as f64 / mask_width as f64,
                top as f64 / mask_height as f64,
                (left + width) as f64 / mask_width as f64,
                (top + height) as f64 / mask_height as f64,
            ];
        }
    }

    let spans = canvas.extent();
    let natural = spans[0].max(spans[1]) * focal_full;
    let scale = if natural > f64::from(options.max_edge) { f64::from(options.max_edge) / natural } else { 1.0 };
    let size = canvas.size(focal_full * scale);

    progress.step("Matching exposure", 0.25)?;
    let to_copy = copies[0].width as f64 / frames[0].width as f64;
    let (working, working_scale) = working_size(&canvas, WORKING_EDGE.min((natural * to_copy).ceil().max(1.0) as usize));
    let rays = pixel_rays(&canvas, working[0], working[1]);
    let mut laid: Vec<Laid> = placed
        .iter()
        .map(|frame| lay_out(frame, &copies[frame.index], &rays, working, working_scale))
        .collect();
    drop(rays);
    let samples = overlap_samples(&laid, clip);
    let reference = (0..placed.len())
        .min_by(|&a, &b| placed[a].yaw().abs().total_cmp(&placed[b].yaw().abs()))
        .unwrap_or(0);
    let gains = solve_gains(&samples, placed.len(), reference, true);
    drop(samples);
    compensate(&mut laid, &gains);

    progress.step("Choosing seams", 0.3)?;
    let layers: Vec<Layer> = laid.into_iter().map(|frame| frame.layer).collect();
    let labels = select_seams(&layers, working[0], working[1]);
    progress.check()?;

    let mut blender = Blender::new(size[0], size[1], bands_for(size));
    let target = Target { canvas: &canvas, size, labels: &labels, label_size: working };
    for (position, frame) in placed.iter().enumerate() {
        let fraction = 0.35 + 0.55 * position as f32 / placed.len() as f32;
        progress.step("Blending", fraction)?;
        if layers[position].width == 0 {
            continue;
        }
        let mut image = source.full(frame.index)?;
        let gain = balance_gain(&frames[frame.index], colour_reference);
        if gain != [1.0; 3] {
            image.pixels.par_iter_mut().for_each(|pixel| *pixel = std::array::from_fn(|c| pixel[c] * gain[c]));
        }
        let shrink = (1.0 / scale).floor().max(1.0) as usize;
        if shrink > 1 {
            image = image.reduced(shrink);
        }
        let region = tile_region(&layers[position], working, size, blender.grid());
        let tile = warp_tile(frame, &image, position as u16, gains.gains[position], &gains, region, &target);
        drop(image);
        blender.add(tile.origin, &tile.image, &tile.covered, &tile.mask);
    }

    progress.step("Finishing", 0.92)?;
    let (image, covered) = blender.finish();
    let empty = covered.par_iter().filter(|value| !**value).count();
    let empty_share = empty as f32 / covered.len().max(1) as f32;
    let any_label = labels.iter().any(|label| *label != NO_FRAME);
    if !any_label {
        return Err(MergeError::NoOverlap);
    }
    let reference_frame = &frames[placed[reference].index];
    let used: Vec<usize> = placed.iter().map(|frame| frame.index).collect();
    let dropped: Vec<usize> = (0..frames.len()).filter(|index| !used.contains(index)).collect();
    let report = PanoramaReport {
        reference: placed[reference].index,
        dropped,
        focal: focal_full,
        rms: rms_full,
        closed_loop,
        cameras: placed.iter().map(|frame| frame.camera).collect(),
        gains: gains.gains.clone(),
        vignette: gains.vignette,
        lens_known: placed.iter().all(|frame| frame.lens.is_known()),
        scale,
        empty_share,
        used,
        seam_size: working,
        canvas,
        seams: labels,
    };
    let sources = placed.iter().map(|frame| frames[frame.index].path.clone()).collect();
    let profile = CameraProfile::new(&colour_reference.color);
    let merged = Merged {
        metadata: panorama_metadata(reference_frame, sources),
        color: colour_reference.color.clone(),
        opcodes: Opcodes::default(),
        embedded_lens: None,
        neutral: profile.as_shot_neutral(),
        image,
    };
    Ok((merged, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frames::{MemoryFrames, plain_frame},
        linalg::{rotation_distance, rotation_from_vector},
    };

    const WIDTH: usize = 480;
    const HEIGHT: usize = 360;
    /// About a 53° wide view.
    const FOCAL: f64 = 480.0;

    /// A scene painted on the inside of a sphere: blocks of different brightness at several
    /// sizes, so it has corners at every scale in every direction.
    fn environment(ray: &Vec3) -> Rgb {
        let longitude = ray[0].atan2(ray[2]) as f32 + 4.0;
        let latitude = ray[1].clamp(-1.0, 1.0).asin() as f32 + 2.0;
        let cell = |size: f32| {
            let (a, b) = ((longitude * size).floor() as i64, (latitude * size).floor() as i64);
            ((a.wrapping_mul(73_856_093) ^ b.wrapping_mul(19_349_663)) & 0xff) as f32 / 255.0
        };
        let level = 0.05 + 0.3 * cell(5.0) + 0.2 * cell(13.0) + 0.1 * cell(31.0);
        [level * 1.05, level, level * 0.9]
    }

    fn view(camera: &Mat3, gain: f32) -> Image {
        let back = transpose(camera);
        let (half_x, half_y) = (WIDTH as f64 * 0.5, HEIGHT as f64 * 0.5);
        Image::from_fn(WIDTH, HEIGHT, |x, y| {
            let camera_ray = normalized(&[(x as f64 + 0.5 - half_x) / FOCAL, (y as f64 + 0.5 - half_y) / FOCAL, 1.0]);
            environment(&apply(&back, &camera_ray)).map(|value| value * gain)
        })
    }

    fn set(cameras: &[Mat3], gains: &[f32]) -> MemoryFrames {
        let images: Vec<Image> = cameras.iter().zip(gains).map(|(camera, gain)| view(camera, *gain)).collect();
        let frames = images.iter().map(|image| plain_frame(image, 1600)).collect();
        MemoryFrames { frames, images }
    }

    fn row(count: usize, step: f64) -> Vec<Mat3> {
        (0..count).map(|index| yaw_rotation((index as f64 - (count - 1) as f64 * 0.5) * step)).collect()
    }

    #[test]
    fn a_row_of_frames_stitches_with_the_right_geometry() {
        let truth = row(4, 0.5);
        let source = set(&truth, &[1.0; 4]);
        let options = PanoramaOptions { auto_crop: false, ..PanoramaOptions::default() };
        let (merged, report) = merge_panorama(&source, &options, &Progress::default()).unwrap();
        assert_eq!(report.used.len(), 4);
        assert!(report.rms < 0.5, "rms {}", report.rms);
        assert!((report.focal / FOCAL - 1.0).abs() < 0.02, "focal {}", report.focal);
        let first = mul(&report.cameras[3], &transpose(&report.cameras[0]));
        let expected = mul(&truth[3], &transpose(&truth[0]));
        assert!(rotation_distance(&first, &expected).to_degrees() < 0.2);
        assert!(merged.image.width > WIDTH * 2, "{}", merged.image.width);
        assert!(!report.closed_loop);
    }

    #[test]
    fn the_stitched_picture_shows_the_scene_in_the_right_place() {
        let source = set(&row(3, 0.5), &[1.0; 3]);
        let options = PanoramaOptions { auto_crop: true, ..PanoramaOptions::default() };
        let (merged, report) = merge_panorama(&source, &options, &Progress::default()).unwrap();
        assert!(report.empty_share < 0.002, "{}", report.empty_share);
        let image = &merged.image;
        let (mut total, mut count) = (0.0f32, 0);
        let (half_x, half_y) = (image.width as f64 * 0.5, image.height as f64 * 0.5);
        for y in (8..image.height - 8).step_by(3) {
            for x in (8..image.width - 8).step_by(3) {
                let longitude = (x as f64 + 0.5 - half_x) / report.focal;
                let latitude = (y as f64 + 0.5 - half_y) / report.focal;
                let world = Projection::Spherical.unproject([longitude, latitude]);
                let found = image.at(x, y)[1];
                let expected = environment(&world)[1];
                total += (found - expected).abs() / expected;
                count += 1;
            }
        }
        let mean = total / count as f32;
        assert!(mean < 0.08, "mean relative error {mean}");
    }

    #[test]
    fn different_exposures_are_evened_out() {
        let source = set(&row(3, 0.5), &[1.0, 1.5, 0.7]);
        let (_, report) = merge_panorama(&source, &PanoramaOptions::default(), &Progress::default()).unwrap();
        let middle = report.gains[report.used.iter().position(|&index| index == 1).unwrap()][1];
        let left = report.gains[report.used.iter().position(|&index| index == 0).unwrap()][1];
        let right = report.gains[report.used.iter().position(|&index| index == 2).unwrap()][1];
        assert!(((left * 1.0) / (middle * 1.5) - 1.0).abs() < 0.03, "{left} {middle}");
        assert!(((right * 0.7) / (middle * 1.5) - 1.0).abs() < 0.03, "{right} {middle}");
    }

    #[test]
    fn a_full_circle_closes_and_wraps_the_canvas() {
        let truth: Vec<Mat3> = (0..9).map(|index| yaw_rotation(index as f64 * std::f64::consts::TAU / 9.0)).collect();
        let source = set(&truth, &[1.0; 9]);
        let options = PanoramaOptions { auto_crop: false, ..PanoramaOptions::default() };
        let (merged, report) = merge_panorama(&source, &options, &Progress::default()).unwrap();
        assert_eq!(report.used.len(), 9);
        assert!(report.closed_loop);
        let expected_width = std::f64::consts::TAU * report.focal;
        assert!((merged.image.width as f64 / expected_width - 1.0).abs() < 0.01, "{}", merged.image.width);
        let middle = merged.image.height / 2;
        for x in [0, merged.image.width / 4, merged.image.width / 2, merged.image.width - 1] {
            assert!(luma(merged.image.at(x, middle)) > 0.0, "empty at {x}");
        }
        assert!(report.rms < 0.5, "rms {}", report.rms);
    }

    #[test]
    fn a_full_circle_is_refused_on_a_flat_plane() {
        let truth: Vec<Mat3> = (0..9).map(|index| yaw_rotation(index as f64 * std::f64::consts::TAU / 9.0)).collect();
        let source = set(&truth, &[1.0; 9]);
        let options = PanoramaOptions { projection: Projection::Perspective, ..PanoramaOptions::default() };
        assert!(matches!(merge_panorama(&source, &options, &Progress::default()), Err(MergeError::FieldOfView { .. })));
    }

    #[test]
    fn frames_shot_in_portrait_stitch_into_an_upright_row() {
        // The camera on its side, so the sensor's rows run up the scene.
        let on_its_side = [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let cameras: Vec<Mat3> = row(4, 0.38).iter().map(|camera| mul(&on_its_side, camera)).collect();
        let mut source = set(&cameras, &[1.0; 4]);
        source.frames.iter_mut().for_each(|frame| frame.metadata.orientation = 6);
        let (merged, report) = merge_panorama(&source, &PanoramaOptions::default(), &Progress::default()).unwrap();
        assert_eq!(report.used.len(), 4);
        assert_eq!(merged.metadata.orientation, 0);
        let image = &merged.image;
        assert!(image.width as f32 > image.height as f32 * 1.2, "{}×{}", image.width, image.height);
        let (half_x, half_y) = (image.width as f64 * 0.5, image.height as f64 * 0.5);
        let (mut total, mut count) = (0.0f32, 0);
        for y in (8..image.height - 8).step_by(3) {
            for x in (8..image.width - 8).step_by(3) {
                let world = [(x as f64 + 0.5 - half_x) / report.focal, (y as f64 + 0.5 - half_y) / report.focal];
                let expected = environment(&Projection::Spherical.unproject(world))[1];
                total += (image.at(x, y)[1] - expected).abs() / expected;
                count += 1;
            }
        }
        let mean = total / count as f32;
        assert!(mean < 0.1, "not upright: mean relative error {mean}");
    }

    #[test]
    fn every_projection_stitches_a_gentle_row() {
        for projection in [Projection::Spherical, Projection::Cylindrical, Projection::Perspective] {
            let source = set(&row(3, 0.4), &[1.0; 3]);
            let options = PanoramaOptions { projection, ..PanoramaOptions::default() };
            let (merged, report) = merge_panorama(&source, &options, &Progress::default()).unwrap();
            assert!(merged.image.width > WIDTH, "{projection:?} {}", merged.image.width);
            assert!(report.empty_share < 0.002, "{projection:?} {}", report.empty_share);
        }
    }

    #[test]
    fn frames_that_dont_overlap_are_reported() {
        let source = set(&[yaw_rotation(0.0), yaw_rotation(2.5)], &[1.0; 2]);
        assert!(matches!(merge_panorama(&source, &PanoramaOptions::default(), &Progress::default()), Err(MergeError::NoOverlap)));
    }

    #[test]
    fn a_stranger_in_the_set_is_left_out() {
        let mut cameras = row(3, 0.5);
        cameras.push(yaw_rotation(3.0));
        let source = set(&cameras, &[1.0; 4]);
        let (_, report) = merge_panorama(&source, &PanoramaOptions::default(), &Progress::default()).unwrap();
        assert_eq!(report.dropped, [3]);
        assert_eq!(report.used.len(), 3);
    }

    #[test]
    fn the_size_limit_reduces_the_output() {
        let source = set(&row(3, 0.5), &[1.0; 3]);
        let options = PanoramaOptions { max_edge: 400, ..PanoramaOptions::default() };
        let (merged, report) = merge_panorama(&source, &options, &Progress::default()).unwrap();
        assert!(merged.image.width.max(merged.image.height) <= 400, "{}", merged.image.width);
        assert!(report.scale < 1.0);
    }

    #[test]
    fn boundary_warp_fills_more_of_the_frame() {
        let tilted: Vec<Mat3> = (0..4)
            .map(|index| {
                let wave = 0.25 * (index as f64 * 2.0).sin();
                mul(&rotation_from_vector(&[wave, 0.0, 0.0]), &yaw_rotation(index as f64 * 0.5 - 0.75))
            })
            .collect();
        let source = set(&tilted, &[1.0; 4]);
        let plain = PanoramaOptions { auto_crop: false, boundary_warp: 0.0, ..PanoramaOptions::default() };
        let warped = PanoramaOptions { boundary_warp: 1.0, ..plain };
        let (_, without) = merge_panorama(&source, &plain, &Progress::default()).unwrap();
        let (_, with) = merge_panorama(&source, &warped, &Progress::default()).unwrap();
        assert!(with.empty_share < without.empty_share * 0.5, "{} {}", with.empty_share, without.empty_share);
    }

    #[test]
    fn cancelling_stops_the_merge() {
        let source = set(&row(2, 0.5), &[1.0; 2]);
        let progress = Progress::default();
        progress.cancel();
        assert!(matches!(merge_panorama(&source, &PanoramaOptions::default(), &progress), Err(MergeError::Cancelled)));
    }

    #[test]
    fn bigger_panoramas_blend_over_more_bands() {
        assert_eq!(bands_for([600, 400]), 5);
        assert_eq!(bands_for([10779, 2663]), 9);
        assert_eq!(bands_for([3000, 2000]), 7);
    }

    #[test]
    fn the_same_photos_give_the_same_picture() {
        let source = set(&row(3, 0.5), &[1.0; 3]);
        let (first, _) = merge_panorama(&source, &PanoramaOptions::default(), &Progress::default()).unwrap();
        let (second, _) = merge_panorama(&source, &PanoramaOptions::default(), &Progress::default()).unwrap();
        assert_eq!(first.image, second.image);
    }
}
