//! HDR merge: several exposures of one scene into a single linear image with more range than
//! any of them. Radiance is a weighted mean of each frame divided by its exposure. A frame's
//! weight is its inverse noise variance, so longer exposures win wherever they aren't clipped,
//! scaled by how far the frame sits from clipping.
//!
//! The clipping term looks at a pixel's brightest channel and applies to all three. Weighting
//! channels apart would mix a red from one frame with a green from another just below clip,
//! where any error in the exposure ratio turns straight into a colour cast.

use crate::{
    Frame, FrameSource, Image, MergeError, Merged, Plane, Progress, Result,
    align::{align_frames, scale_alignment},
    deghost::find_ghosts,
    exposure::{ExposureEstimate, exposure_order, resolve_exposures},
    image::luma,
    output::{hdr_metadata, merged_source_opcodes},
    resample::{bilinear_rgb, lanczos_rgb},
};
use rayon::prelude::*;
use unlight_color::{
    CameraProfile,
    matrix::{IDENTITY, Mat3},
};

/// A frame's weight falls from full to nothing across this share of the clip level. Starting
/// well below clip gives the merge room to hand over smoothly, which is what stops a band
/// showing where one frame gives way to the next.
const CLIP_RAMP_START: f32 = 0.7;
const CLIP_RAMP_END: f32 = 0.96;
/// Pixels this close to a clipped one are out too. Demosaicing pulled their values from
/// clipped neighbours, and the resampling filter reaches about this far.
const CLIP_GUARD: usize = 3;
/// Read noise over shot noise, as a level in a frame normalised to white. Sets where longer
/// exposures stop mattering. Any plausible sensor gives close to the same weights.
const READ_NOISE_LEVEL: f32 = 0.003;
/// Near black a frame only counts for this much, as a share of the dark limit.
const DARK_FLOOR: f32 = 0.05;
const DARK_RAMP: f32 = 0.002;
/// Weight of a clipped frame in an area that moved. Below any real weight.
const CLIPPED_SAY: f32 = 1e-6;
/// Long edge of the copies used to align and to look for motion.
pub const ANALYSIS_EDGE: usize = 3000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Deghost {
    #[default]
    None,
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrOptions {
    pub auto_align: bool,
    pub deghost: Deghost,
}

impl Default for HdrOptions {
    fn default() -> Self {
        Self { auto_align: true, deghost: Deghost::None }
    }
}

#[derive(Clone, Debug)]
pub struct HdrReport {
    /// Position in the input list of the frame everything was aligned and scaled to.
    pub reference: usize,
    /// Per input frame.
    pub exposures: Vec<ExposureEstimate>,
    /// Per input frame: reference to frame, on centred pixels of the full image.
    pub alignments: Vec<Mat3>,
    /// Share of the image left to a single exposure by deghosting.
    pub ghost_share: f32,
    /// Share of pixels clipped even in the shortest exposure.
    pub clipped_share: f32,
    /// Stops of light above the reference frame's clip level the result holds.
    pub headroom: f32,
}

#[inline]
fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much to trust a pixel, from how close its brightest channel is to clipping.
#[inline]
fn exposure_quality(pixel: [f32; 3], clip: [f32; 3]) -> f32 {
    let highest = (0..3).map(|c| pixel[c] / clip[c]).fold(0.0f32, f32::max);
    let top = 1.0 - smoothstep(CLIP_RAMP_START, CLIP_RAMP_END, highest);
    let level = luma(pixel) / clip[1];
    top * (DARK_FLOOR + (1.0 - DARK_FLOOR) * smoothstep(0.0, DARK_RAMP, level))
}

/// Inverse variance of the radiance a pixel implies. Signal is `R·t`, its variance goes as
/// signal plus read noise, and dividing by `t` to get radiance divides the variance by `t²`.
#[inline]
fn noise_weight(level: f32, exposure: f32) -> f32 {
    exposure * exposure / (level.max(0.0) + READ_NOISE_LEVEL)
}

fn warp_copy(image: &Image, matrix: &Mat3) -> Image {
    if *matrix == IDENTITY {
        return image.clone();
    }
    let (half_x, half_y) = (image.width as f32 * 0.5, image.height as f32 * 0.5);
    Image::from_fn(image.width, image.height, |x, y| {
        let (cx, cy) = (f64::from(x as f32 + 0.5 - half_x), f64::from(y as f32 + 0.5 - half_y));
        let w = matrix[2][0] * cx + matrix[2][1] * cy + matrix[2][2];
        let sx = ((matrix[0][0] * cx + matrix[0][1] * cy + matrix[0][2]) / w) as f32 + half_x - 0.5;
        let sy = ((matrix[1][0] * cx + matrix[1][1] * cy + matrix[1][2]) / w) as f32 + half_y - 0.5;
        bilinear_rgb(image, sx, sy)
    })
}

/// What one frame contributes to the running sum.
struct Contribution<'a> {
    matrix: &'a Mat3,
    /// Exposure relative to the reference.
    exposure: f32,
    /// Copy-sized map of this frame's say in the areas that moved, if anything did.
    only: Option<&'a Plane>,
    clip: [f32; 3],
    /// This is the shortest exposure, added last. Any pixel no frame could vouch for is clipped
    /// in all of them, and takes its value from here.
    last_resort: bool,
}

fn balance_gain(frame: &Frame, reference: &Frame) -> [f32; 3] {
    std::array::from_fn(|c| reference.balance[c] / frame.balance[c])
}

fn quality_map(image: &Image, clip: [f32; 3]) -> Plane {
    let quality = image.plane(|pixel| exposure_quality(pixel, clip));
    if quality.values.par_iter().any(|value| *value <= 0.0) { quality.eroded(CLIP_GUARD) } else { quality }
}

/// The running result. `sum` holds weighted radiance and total weight per pixel. `only` holds
/// the same for just the frames deghosting allows, and is empty unless something moved.
struct Totals {
    sum: Vec<[f32; 4]>,
    only: Vec<[f32; 4]>,
    width: usize,
}

/// Folds a whole frame into the totals.
fn accumulate(totals: &mut Totals, frame: &Image, part: &Contribution<'_>) {
    let width = totals.width;
    let quality = quality_map(frame, part.clip);
    let (half_x, half_y) = (frame.width as f32 * 0.5, frame.height as f32 * 0.5);
    let matrix = part.matrix;
    let aligned = *matrix != IDENTITY;
    let divide = 1.0 / part.exposure;
    let (last_x, last_y) = ((frame.width - 1) as f32, (frame.height - 1) as f32);
    let mut claims = totals.only.chunks_mut(width);
    let rows: Vec<_> = totals.sum.chunks_mut(width).map(|row| (row, claims.next())).collect();
    rows.into_par_iter().enumerate().for_each(|(y, (row, mut claim))| {
        let center_y = y as f32 + 0.5 - half_y;
        for (x, total) in row.iter_mut().enumerate() {
            let (sample, trust) = if aligned {
                let center_x = x as f32 + 0.5 - half_x;
                let (hx, hy) = (f64::from(center_x), f64::from(center_y));
                let w = matrix[2][0] * hx + matrix[2][1] * hy + matrix[2][2];
                let sx = ((matrix[0][0] * hx + matrix[0][1] * hy + matrix[0][2]) / w) as f32 + half_x - 0.5;
                let sy = ((matrix[1][0] * hx + matrix[1][1] * hy + matrix[1][2]) / w) as f32 + half_y - 0.5;
                let inside = sx >= 0.0 && sy >= 0.0 && sx <= last_x && sy <= last_y;
                if !inside && !(part.last_resort && total[3] <= 0.0) {
                    continue;
                }
                let trust = if inside { quality.bilinear(sx, sy) } else { 0.0 };
                (lanczos_rgb(frame, sx, sy), trust)
            } else {
                (frame.at(x, y), quality.at(x, y))
            };
            let mut weight = trust * noise_weight(luma(sample), part.exposure);
            if part.last_resort && total[3] + weight <= 0.0 {
                weight = 1.0;
            }
            for channel in 0..3 {
                total[channel] += sample[channel] * divide * weight;
            }
            total[3] += weight;
            if let (Some(only), Some(claim)) = (part.only, claim.as_deref_mut()) {
                let along_x = (x as f32 + 0.5) / frame.width as f32 * only.width as f32 - 0.5;
                let along_y = (y as f32 + 0.5) / frame.height as f32 * only.height as f32 - 0.5;
                // Even clipped, a frame allowed here beats a frame that shows something else,
                // so it keeps a say that only matters when none of them has any weight.
                let share = only.bilinear(along_x, along_y) * weight.max(CLIPPED_SAY);
                if share > 0.0 {
                    for channel in 0..3 {
                        claim[x][channel] += sample[channel] * divide * share;
                    }
                    claim[x][3] += share;
                }
            }
        }
    });
}

/// Turns the totals into pixels: the plain merge, except that where something moved the result
/// comes from the frames allowed there, fading across `reach`.
fn resolved(totals: Totals, height: usize, reach: Option<&Plane>) -> Image {
    let Totals { sum, only, width } = totals;
    let plain = |[r, g, b, weight]: [f32; 4]| if weight > 0.0 { [r / weight, g / weight, b / weight] } else { [0.0; 3] };
    let pixels = match reach {
        None => sum.into_par_iter().map(plain).collect(),
        Some(reach) => sum
            .into_par_iter()
            .zip(only)
            .enumerate()
            .map(|(at, (total, claim))| {
                let merged = plain(total);
                if claim[3] <= 0.0 {
                    return merged;
                }
                let along_x = ((at % width) as f32 + 0.5) / width as f32 * reach.width as f32 - 0.5;
                let along_y = ((at / width) as f32 + 0.5) / height as f32 * reach.height as f32 - 0.5;
                let share = reach.bilinear(along_x, along_y);
                let allowed = plain(claim);
                std::array::from_fn(|c| merged[c] * (1.0 - share) + allowed[c] * share)
            })
            .collect(),
    };
    Image { width, height, pixels }
}

fn check_compatible(frames: &[Frame]) -> Result<()> {
    if frames.len() < 2 {
        return Err(MergeError::TooFewFrames);
    }
    let first = &frames[0];
    if let Some(frame) = frames.iter().find(|frame| frame.metadata.merge.is_some()) {
        return Err(MergeError::AlreadyMerged(frame.path.clone()));
    }
    if let Some(frame) = frames.iter().find(|frame| (frame.width, frame.height) != (first.width, first.height)) {
        return Err(MergeError::SizeMismatch(frame.path.clone()));
    }
    Ok(())
}

fn balanced_copy(frame: &Frame, reference: &Frame) -> Image {
    let gain = balance_gain(frame, reference);
    let reduced = &frame.reduced;
    Image::from_fn(reduced.width, reduced.height, |x, y| {
        let pixel = reduced.at(x, y);
        std::array::from_fn(|c| pixel[c] * gain[c])
    })
}

/// Merges a bracket. The result is in the reference frame's white balance, and its scale is
/// that frame's exposure, so it opens looking like the middle frame with its highlights intact.
pub fn merge_hdr(source: &dyn FrameSource, options: &HdrOptions, progress: &Progress) -> Result<(Merged, HdrReport)> {
    let frames = source.frames();
    check_compatible(frames)?;
    let order = exposure_order(frames);
    let position = order.len() / 2;
    let reference = &frames[order[position]];
    let clip = reference.clip_levels();

    progress.step("Reading exposures", 0.02)?;
    let copies: Vec<Image> = order.iter().map(|&index| balanced_copy(&frames[index], reference)).collect();
    let copy_clips: Vec<[f32; 3]> = vec![clip; order.len()];

    progress.step("Aligning", 0.05)?;
    let copy_matrices = if options.auto_align {
        let images: Vec<&Image> = copies.iter().collect();
        align_frames(&images, &copy_clips, position)
    } else {
        vec![IDENTITY; order.len()]
    };
    let aligned: Vec<Image> = copies.iter().zip(&copy_matrices).map(|(copy, matrix)| warp_copy(copy, matrix)).collect();

    progress.step("Measuring exposures", 0.15)?;
    let ordered: Vec<&Frame> = order.iter().map(|&index| &frames[index]).collect();
    let exposures = resolve_exposures(&ordered, &aligned, clip, position)?;
    let stops: Vec<f32> = exposures.iter().map(|estimate| estimate.used).collect();

    progress.step("Looking for motion", 0.2)?;
    let ghosts = find_ghosts(&aligned, &stops, clip, position, options.deghost);

    let scale = reference.width as f64 / reference.reduced.width as f64;
    let matrices: Vec<Mat3> = copy_matrices.iter().map(|matrix| scale_alignment(matrix, scale)).collect();

    let (width, height) = (reference.width, reference.height);
    let only = if ghosts.is_some() { vec![[0.0f32; 4]; width * height] } else { Vec::new() };
    let mut totals = Totals { sum: vec![[0.0f32; 4]; width * height], only, width };
    for (done, (step, &index)) in order.iter().enumerate().rev().enumerate() {
        let fraction = 0.25 + 0.65 * done as f32 / order.len() as f32;
        progress.step("Merging", fraction)?;
        let frame = source.full(index)?;
        let gain = balance_gain(&frames[index], reference);
        let mut frame = frame;
        if gain != [1.0; 3] {
            frame.pixels.par_iter_mut().for_each(|pixel| *pixel = std::array::from_fn(|c| pixel[c] * gain[c]));
        }
        let part = Contribution {
            matrix: &matrices[step],
            exposure: 2f32.powf(stops[step]),
            only: ghosts.as_ref().map(|maps| &maps.only[step]),
            clip,
            last_resort: step == 0,
        };
        accumulate(&mut totals, &frame, &part);
    }
    progress.step("Finishing", 0.92)?;
    let image = resolved(totals, height, ghosts.as_ref().map(|maps| &maps.reach));

    let clipped_share = {
        let shortest = &aligned[0];
        let clipped = shortest.pixels.par_iter().filter(|pixel| (0..3).any(|c| pixel[c] >= clip[c])).count();
        clipped as f32 / shortest.pixels.len().max(1) as f32
    };
    let brightest = image.pixels.par_iter().map(|pixel| luma(*pixel)).reduce(|| 0.0, f32::max);
    let mut by_input_exposures = vec![exposures[0]; frames.len()];
    let mut by_input_alignments = vec![IDENTITY; frames.len()];
    for (step, &index) in order.iter().enumerate() {
        by_input_exposures[index] = exposures[step];
        by_input_alignments[index] = matrices[step];
    }
    let report = HdrReport {
        reference: order[position],
        exposures: by_input_exposures,
        alignments: by_input_alignments,
        ghost_share: ghosts.as_ref().map_or(0.0, |maps| maps.share),
        clipped_share,
        headroom: (brightest / clip[1]).max(1e-6).log2(),
    };
    let sources: Vec<_> = order.iter().map(|&index| frames[index].path.clone()).collect();
    let profile = CameraProfile::new(&reference.color);
    let merged = Merged {
        metadata: hdr_metadata(reference, sources),
        color: reference.color.clone(),
        opcodes: merged_source_opcodes(reference),
        embedded_lens: reference.embedded_lens.clone(),
        neutral: profile.as_shot_neutral(),
        image,
    };
    Ok((merged, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{MemoryFrames, plain_frame};

    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;

    /// Radiance over twelve stops, from below black at the reference to far above clip.
    fn radiance(x: usize, y: usize) -> [f32; 3] {
        let ramp = (x as f32 / WIDTH as f32 * 12.0 - 7.0).exp2();
        let ripple = 1.0 + 0.2 * ((y as f32) * 0.21).sin();
        let level = ramp * ripple;
        [level * 1.1, level, level * 0.8]
    }

    fn exposed(stops: f32) -> Image {
        let gain = stops.exp2();
        Image::from_fn(WIDTH, HEIGHT, |x, y| radiance(x, y).map(|value| (value * gain).min(1.0)))
    }

    fn bracket(stops: &[f32]) -> MemoryFrames {
        let images: Vec<Image> = stops.iter().map(|&stop| exposed(stop)).collect();
        let frames = images
            .iter()
            .zip(stops)
            .map(|(image, stop)| {
                let mut frame = plain_frame(image, 320);
                frame.metadata.shutter = 0.01 * stop.exp2();
                frame
            })
            .collect();
        MemoryFrames { frames, images }
    }

    fn relative_error(merged: &Image, x: usize, y: usize) -> f32 {
        let truth = radiance(x, y);
        let found = merged.at(x, y);
        (0..3).map(|c| (found[c] / truth[c] - 1.0).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn range_beyond_every_input_is_recovered() {
        let set = bracket(&[-3.0, 0.0, 3.0]);
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, report) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let image = &merged.image;
        assert_eq!(report.reference, 1);
        assert!(report.headroom > 2.5, "headroom {}", report.headroom);
        let reference_clip = (0..WIDTH).find(|&x| radiance(x, 120)[0] >= 1.0).unwrap();
        for x in (20..WIDTH * 4 / 5).step_by(7) {
            let error = relative_error(image, x, 120);
            assert!(error < 0.02, "{error} at {x}, reference clips at {reference_clip}");
        }
    }

    #[test]
    fn the_result_is_scaled_to_the_reference() {
        let set = bracket(&[-2.0, 0.0, 2.0]);
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, report) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let x = (0..WIDTH).find(|&x| radiance(x, 120)[1] > 0.3).unwrap();
        assert!(relative_error(&merged.image, x, 120) < 0.01);
        assert!((report.exposures[0].used + 2.0).abs() < 0.02);
        assert!((report.exposures[2].used - 2.0).abs() < 0.02);
    }

    #[test]
    fn longer_exposures_lower_the_noise() {
        let noise = |x: usize, y: usize, seed: usize| {
            let hash = (x * 73 + y * 179 + seed * 283).wrapping_mul(2_654_435_761) >> 16;
            ((hash & 0xffff) as f32 / 65535.0 - 0.5) * 0.002
        };
        let dark = 0.02;
        let images: Vec<Image> = [(0.0, 0usize), (2.0, 1), (3.0, 2)]
            .iter()
            .map(|&(stops, seed): &(f32, usize)| {
                let gain = stops.exp2();
                Image::from_fn(WIDTH, HEIGHT, |x, y| [dark * gain + noise(x, y, seed); 3])
            })
            .collect();
        let frames = images.iter().map(|image| plain_frame(image, 320)).collect();
        let set = MemoryFrames { frames, images };
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, _) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let spread = |image: &Image| {
            let values: Vec<f32> = image.pixels.iter().map(|pixel| pixel[1]).collect();
            let mean = values.iter().sum::<f32>() / values.len() as f32;
            (values.iter().map(|value| (value - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt()
        };
        let reference = spread(&set.images[1]);
        let result = spread(&merged.image);
        assert!(result < reference * 0.9, "{result} against {reference}");
    }

    #[test]
    fn a_channel_clipped_in_one_frame_doesnt_tint_the_result() {
        let warm = [1.8, 1.0, 0.5];
        let flat = |gain: f32| Image::from_fn(WIDTH, HEIGHT, |_, _| warm.map(|value| (value * 0.4 * gain).min(1.0)));
        let images = vec![flat(0.5), flat(2.0)];
        let mut frames: Vec<Frame> = images.iter().map(|image| plain_frame(image, 320)).collect();
        frames[0].metadata.shutter = 0.0025;
        frames[1].metadata.shutter = 0.01;
        let set = MemoryFrames { frames, images };
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, _) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let [r, g, b] = merged.image.at(160, 120);
        assert!((r / g - 1.8).abs() < 0.01, "{r} {g} {b}");
        assert!((b / g - 0.5).abs() < 0.01, "{r} {g} {b}");
    }

    #[test]
    fn a_pixel_clipped_everywhere_comes_from_the_shortest_frame() {
        let images = vec![Image::from_fn(16, 16, |_, _| [1.0; 3]), Image::from_fn(16, 16, |_, _| [1.0; 3])];
        let mut frames: Vec<Frame> = images.iter().map(|image| plain_frame(image, 16)).collect();
        frames[0].metadata.shutter = 0.01;
        frames[1].metadata.shutter = 0.04;
        let set = MemoryFrames { frames, images };
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, report) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        assert!(report.clipped_share > 0.99);
        let expected = 4.0;
        assert!((merged.image.at(8, 8)[0] - expected).abs() < 0.2, "{:?}", merged.image.at(8, 8));
    }

    #[test]
    fn transitions_between_exposures_dont_band() {
        let set = bracket(&[-2.0, 2.0]);
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, _) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let row = 120;
        let log: Vec<f32> = (0..WIDTH).map(|x| merged.image.at(x, row)[1].max(1e-5).log2()).collect();
        let slope = 12.0 / WIDTH as f32;
        let worst = log[1..].iter().zip(&log).skip(30).take(200).map(|(a, b)| (a - b - slope).abs()).fold(0.0, f32::max);
        assert!(worst < slope * 0.5, "step {worst} on a slope of {slope}");
    }

    #[test]
    fn mixed_white_balance_is_reconciled() {
        let bright = Image::from_fn(WIDTH, HEIGHT, |_, _| [0.4, 0.4, 0.4]);
        let dark = Image::from_fn(WIDTH, HEIGHT, |_, _| [0.1 * 1.5, 0.1, 0.1 * 0.5]);
        let images = vec![bright, dark];
        let mut frames: Vec<Frame> = images.iter().map(|image| plain_frame(image, 320)).collect();
        frames[0].metadata.shutter = 0.04;
        frames[1].metadata.shutter = 0.01;
        frames[1].balance = [1.5, 1.0, 0.5];
        let set = MemoryFrames { frames, images };
        let options = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (merged, report) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        assert_eq!(report.reference, 0);
        let [r, g, b] = merged.image.at(100, 100);
        assert!((r - g).abs() < 0.005 && (b - g).abs() < 0.005, "{r} {g} {b}");
    }

    #[test]
    fn a_frame_that_clipped_part_of_a_moving_area_hands_over_to_one_that_agrees() {
        // Most of the area is dim, which the middle frame shows best, so that's the one chosen.
        // A hot spot in it clipped there, and only the short frame holds it. The long frame has
        // something else there entirely, and it mustn't show through anywhere.
        let scene = |x: usize, y: usize| {
            let (spot, area) = (
                (150..190).contains(&x) && (100..140).contains(&y),
                (80..260).contains(&x) && (60..180).contains(&y),
            );
            if spot { 3.0 } else if area { 0.03 } else { 0.1 + (y as f32 * 0.02).sin().abs() * 0.1 }
        };
        let view = |stops: f32, object: bool| {
            let gain = stops.exp2();
            Image::from_fn(WIDTH, HEIGHT, |x, y| {
                let inside = (80..260).contains(&x) && (60..180).contains(&y);
                [if object && inside { 0.5 } else { (scene(x, y) * gain).min(1.0) }; 3]
            })
        };
        let images = vec![view(-2.0, false), view(0.0, false), view(2.0, true)];
        let mut frames: Vec<Frame> = images.iter().map(|image| plain_frame(image, 320)).collect();
        for (frame, shutter) in frames.iter_mut().zip([0.0025, 0.01, 0.04]) {
            frame.metadata.shutter = shutter;
        }
        let set = MemoryFrames { frames, images };
        let options = HdrOptions { auto_align: false, deghost: Deghost::Medium };
        let (merged, _) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let spot = merged.image.at(170, 120)[1];
        let area = merged.image.at(120, 90)[1];
        assert!((spot - 3.0).abs() < 0.3, "the hot spot came out at {spot}");
        assert!((area - 0.03).abs() < 0.01, "the dim area came out at {area}");
    }

    #[test]
    fn something_that_moved_cant_show_through_where_every_other_frame_clipped() {
        // Far above even the short frame's clip, so nothing that agrees has any weight there.
        let view = |gain: f32, object: bool| {
            Image::from_fn(WIDTH, HEIGHT, |x, y| {
                let inside = (100..200).contains(&x) && (80..160).contains(&y);
                let scene = if inside { 16.0 } else { 0.1 + (y as f32 * 0.02).sin().abs() * 0.1 };
                [if object && inside { 0.05 } else { (scene * gain).min(1.0) }; 3]
            })
        };
        let images = vec![view(0.25, false), view(1.0, false), view(4.0, true)];
        let mut frames: Vec<Frame> = images.iter().map(|image| plain_frame(image, 320)).collect();
        for (frame, shutter) in frames.iter_mut().zip([0.0025, 0.01, 0.04]) {
            frame.metadata.shutter = shutter;
        }
        let set = MemoryFrames { frames, images };
        let options = HdrOptions { auto_align: false, deghost: Deghost::Medium };
        let (merged, _) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        let value = merged.image.at(150, 120)[1];
        assert!(value > 0.9, "the moved object showed through as {value}");
    }

    #[test]
    fn one_frame_isnt_a_merge() {
        let set = bracket(&[0.0]);
        assert!(matches!(merge_hdr(&set, &HdrOptions::default(), &Progress::default()), Err(MergeError::TooFewFrames)));
    }

    #[test]
    fn frames_of_different_sizes_are_refused() {
        let mut set = bracket(&[0.0, 1.0]);
        set.frames[1].width = 100;
        assert!(matches!(merge_hdr(&set, &HdrOptions::default(), &Progress::default()), Err(MergeError::SizeMismatch(_))));
    }

    #[test]
    fn cancelling_stops_the_merge() {
        let set = bracket(&[-1.0, 1.0]);
        let progress = Progress::default();
        progress.cancel();
        assert!(matches!(merge_hdr(&set, &HdrOptions::default(), &progress), Err(MergeError::Cancelled)));
    }

    #[test]
    fn deghosting_keeps_one_version_of_a_moved_object() {
        let mut set = bracket(&[-1.0, 1.0]);
        let object = |image: &mut Image, left: usize, gain: f32| {
            for y in 80..160 {
                for x in left..left + 40 {
                    image.pixels[y * WIDTH + x] = [0.3 * gain; 3];
                }
            }
        };
        object(&mut set.images[0], 60, 0.5);
        object(&mut set.images[1], 200, 2.0);
        set.frames[0].reduced = set.images[0].clone();
        set.frames[1].reduced = set.images[1].clone();
        let options = HdrOptions { auto_align: false, deghost: Deghost::Medium };
        let (merged, report) = merge_hdr(&set, &options, &Progress::default()).unwrap();
        assert!(report.ghost_share > 0.02, "{}", report.ghost_share);
        let plain = HdrOptions { auto_align: false, deghost: Deghost::None };
        let (ghosted, _) = merge_hdr(&set, &plain, &Progress::default()).unwrap();
        // The longer frame is the reference, which puts everything a stop above the scene.
        let consistent = |image: &Image, x: usize| {
            let value = image.at(x, 120)[1];
            let scene = radiance(x, 120)[1] * 2.0;
            (value - 0.6).abs() < 0.06 || (value - scene).abs() / scene < 0.05
        };
        assert!(consistent(&merged.image, 80), "{:?}", merged.image.at(80, 120));
        assert!(consistent(&merged.image, 220), "{:?}", merged.image.at(220, 120));
        assert!(!consistent(&ghosted.image, 80) || !consistent(&ghosted.image, 220));
    }
}
