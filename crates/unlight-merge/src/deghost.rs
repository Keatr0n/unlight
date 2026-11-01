//! Finds what moved between frames and picks one exposure for each moving area, so a person
//! walking through a bracket shows up once, whole, instead of as a translucent double. Frames
//! are compared by the radiance they imply: where the scene held still, neighbours agree once
//! their exposure is divided out. This runs on the small copies, which are quiet enough for
//! that comparison, and the result is a smooth map per frame, of where that frame alone decides.

use crate::{Image, Plane, hdr::Deghost, image::median};
#[cfg(test)]
use crate::image::luma;

/// Below this share of clip, a pixel has more noise than scene.
const DARK: f32 = 0.012;
/// Above this share of clip, a pixel might be clipped in some channel.
const BRIGHT: f32 = 0.9;
/// A frame that's clipped where a neighbour implies less than this share of clip saw something
/// else there. It has to be well under the clip level, because the neighbour's own value is
/// uncertain close to it.
const CLIP_DOUBT: f32 = 0.6;
/// Likewise a frame near black where a neighbour implies at least this many times the dark limit.
const DARK_DOUBT: f32 = 4.0;
const NOISE_SIGMAS: f32 = 5.0;
const MEDIAN_TO_SIGMA: f32 = 1.4826;
/// Radii below are for a copy with this long edge, and scale with it.
const REFERENCE_EDGE: f32 = 1500.0;
const COMPARE_BLUR: f32 = 1.5;
const OPENING_RADIUS: f32 = 1.5;
const GROWTH_RADIUS: f32 = 6.0;
/// Three of these fit inside the growth, so the soft edge falls entirely outside what moved.
const FEATHER: f32 = GROWTH_RADIUS / 3.0;
/// A frame agreeing with the chosen one over this share of an area can fill in for it there.
const AGREEMENT: f32 = 0.6;
/// How a stand-in counts against the chosen frame. Far below what merge weights can differ by, so
/// it only matters where the chosen frame clipped and has no weight at all.
const STAND_IN: f32 = 1e-7;
/// Slight favour for frames near the reference, which breaks ties predictably.
const NEARNESS: f32 = 0.02;
/// How much more a frame agreeing with another counts than a frame merely being well exposed.
const VOTE_WEIGHT: f32 = 4.0;

fn threshold_stops(level: Deghost) -> Option<f32> {
    match level {
        Deghost::None => None,
        Deghost::Low => Some(0.55),
        Deghost::Medium => Some(0.32),
        Deghost::High => Some(0.18),
    }
}

/// Where moving areas were found, on a copy-sized grid. The result there is blended with the
/// finished merge rather than made by scaling frames' weights, because weights run over a range
/// of ten thousand to one between exposures, and a long frame scaled down by even a soft edge of
/// 7% would still overrule everything else.
pub(crate) struct GhostMaps {
    /// How far each frame gets a say in a moving area: fully for the one chosen, and barely at
    /// all for frames that agree with it, which only fill in what it clipped.
    pub only: Vec<Plane>,
    /// How much of the result comes from moving areas.
    pub reach: Plane,
    /// Share of the image that's been restricted to one exposure.
    pub share: f32,
}

/// The exposure a moving area comes from, and the frames that can stand in for it.
struct AreaChoice {
    chosen: usize,
    agrees: Vec<bool>,
}

struct Radiance {
    log: Plane,
    /// Luma as a share of clip, which is how dark a pixel is.
    level: Plane,
    /// The brightest channel as a share of its clip. Clipping happens a channel at a time, so
    /// this rather than luma is what says how near it a pixel is. A saturated orange can have
    /// its red at clip with a luma of 0.2.
    highest: Plane,
}

impl Radiance {
    #[inline]
    fn is_clipped(&self, at: usize) -> bool {
        self.highest.values[at] >= BRIGHT
    }
}

fn scaled(radius: f32, long_edge: usize) -> f32 {
    radius * long_edge as f32 / REFERENCE_EDGE
}

fn radiance(image: &Image, stops: f32, clip: [f32; 3], blur: f32) -> Radiance {
    let top = clip[1].max(1e-6);
    let quiet = image.luma().blurred(blur);
    let divide = 2f32.powf(-stops);
    Radiance {
        log: Plane::from_fn(image.width, image.height, |x, y| (quiet.at(x, y).max(1e-6) * divide).log2()),
        level: Plane::from_fn(image.width, image.height, |x, y| quiet.at(x, y) / top),
        highest: image
            .plane(|pixel| (0..3).map(|c| pixel[c] / clip[c].max(1e-6)).fold(0.0, f32::max))
            .blurred(blur),
    }
}

#[inline]
fn is_exposed(frame: &Radiance, at: usize) -> bool {
    frame.level.values[at] > DARK && !frame.is_clipped(at)
}

/// Whether a pixel of two frames, `gain` apart in exposure, can be the same light. A frame that
/// clipped or went black still says something: that the light was at least, or at most, so much.
/// So a shorter exposure clipped where a longer one isn't can't be a still scene, whatever the
/// values are, and neither can a longer exposure black where a shorter one isn't.
#[inline]
fn consistent(shorter: &Radiance, longer: &Radiance, at: usize, gain: f32, limit: f32) -> bool {
    let (short_ok, long_ok) = (is_exposed(shorter, at), is_exposed(longer, at));
    let (short_clipped, long_clipped) = (shorter.is_clipped(at), longer.is_clipped(at));
    let (short_dark, long_dark) = (shorter.level.values[at] <= DARK, longer.level.values[at] <= DARK);
    if short_ok && long_ok {
        (shorter.log.values[at] - longer.log.values[at]).abs() <= limit
    } else if short_ok && long_clipped {
        shorter.highest.values[at] * gain >= CLIP_DOUBT
    } else if long_ok && short_dark {
        longer.level.values[at] / gain <= DARK * DARK_DOUBT
    } else {
        !(short_clipped && !long_clipped) && !(long_dark && !short_dark)
    }
}

/// Marks where a pair of neighbouring frames can't both be showing the same scene.
fn mark_motion(motion: &mut Plane, shorter: &Radiance, longer: &Radiance, gain: f32, threshold: f32) {
    let mut differences: Vec<f32> = (0..motion.values.len())
        .step_by(7)
        .filter(|&at| is_exposed(shorter, at) && is_exposed(longer, at))
        .map(|at| (shorter.log.values[at] - longer.log.values[at]).abs())
        .collect();
    let spread = median(&mut differences).unwrap_or(0.0) * MEDIAN_TO_SIGMA;
    let limit = threshold.max(spread * NOISE_SIGMAS);
    for at in 0..motion.values.len() {
        if !consistent(shorter, longer, at, gain, limit) {
            motion.values[at] = 1.0;
        }
    }
}

/// Labels connected areas, returning how many pixels each has. Label 0 is the background.
fn label_regions(mask: &Plane) -> (Vec<u32>, Vec<usize>) {
    let (width, height) = (mask.width, mask.height);
    let mut labels = vec![0u32; width * height];
    let mut sizes = vec![0usize];
    let mut stack = Vec::new();
    for start in 0..labels.len() {
        if labels[start] != 0 || mask.values[start] < 0.5 {
            continue;
        }
        let label = sizes.len() as u32;
        let mut size = 0;
        stack.push(start);
        labels[start] = label;
        while let Some(at) = stack.pop() {
            size += 1;
            let (x, y) = (at % width, at / width);
            let mut visit = |next: usize| {
                if labels[next] == 0 && mask.values[next] >= 0.5 {
                    labels[next] = label;
                    stack.push(next);
                }
            };
            if x > 0 {
                visit(at - 1);
            }
            if x + 1 < width {
                visit(at + 1);
            }
            if y > 0 {
                visit(at - width);
            }
            if y + 1 < height {
                visit(at + width);
            }
        }
        sizes.push(size);
    }
    (labels, sizes)
}

/// Fills holes, meaning anything unmarked that doesn't reach the edge of the image. A moving
/// object's own shading can match the background in places, and it still moved as a whole.
fn filled(mask: &Plane) -> Plane {
    let inverse = Plane { values: mask.values.iter().map(|value| 1.0 - value).collect(), ..*mask };
    let (labels, sizes) = label_regions(&inverse);
    let (width, height) = (mask.width, mask.height);
    let mut reaches_edge = vec![false; sizes.len()];
    for x in 0..width {
        reaches_edge[labels[x] as usize] = true;
        reaches_edge[labels[(height - 1) * width + x] as usize] = true;
    }
    for y in 0..height {
        reaches_edge[labels[y * width] as usize] = true;
        reaches_edge[labels[y * width + width - 1] as usize] = true;
    }
    Plane {
        values: labels.iter().map(|&label| f32::from(u8::from(label == 0 || !reaches_edge[label as usize]))).collect(),
        width,
        height,
    }
}

fn opened(mask: &Plane, radius: usize) -> Plane {
    let shrunk = mask.eroded(radius);
    grown(&shrunk, radius)
}

fn grown(mask: &Plane, radius: usize) -> Plane {
    let inverse = Plane { values: mask.values.iter().map(|value| 1.0 - value).collect(), ..*mask };
    let eroded = inverse.eroded(radius);
    Plane { values: eroded.values.iter().map(|value| 1.0 - value).collect(), ..eroded }
}

/// Picks the frame each moving area should come from. The one that agrees with the others wins,
/// because what moved is usually in only one of them, and that frame can be the best exposed
/// there: a dark passer-by in the long exposure, say. Votes only stop being decisive when frames
/// have nothing to agree on, and then it's whichever frame saw most of the area, nearest the
/// reference if that's still level.
fn choose_frames(
    frames: &[Radiance],
    stops: &[f32],
    (labels, sizes): (&[u32], &[usize]),
    reference: usize,
    threshold: f32,
) -> Vec<AreaChoice> {
    let count = frames.len();
    let mut exposure = vec![vec![0.0f32; count]; sizes.len()];
    let mut agreement = vec![vec![0.0f32; count * count]; sizes.len()];
    for (at, &label) in labels.iter().enumerate() {
        let label = label as usize;
        if label == 0 {
            continue;
        }
        for first in 0..count {
            exposure[label][first] += f32::from(u8::from(is_exposed(&frames[first], at)));
            for second in first + 1..count {
                let gain = 2f32.powf(stops[second] - stops[first]);
                if consistent(&frames[first], &frames[second], at, gain, threshold) {
                    agreement[label][first * count + second] += 1.0;
                    agreement[label][second * count + first] += 1.0;
                }
            }
        }
    }
    (0..sizes.len())
        .map(|label| {
            let size = sizes[label].max(1) as f32;
            let votes = |index: usize| -> f32 { (0..count).map(|other| agreement[label][index * count + other]).sum() };
            let score = |index: usize| {
                votes(index) / size * VOTE_WEIGHT + exposure[label][index] / size
                    - NEARNESS * index.abs_diff(reference) as f32
            };
            let chosen = (0..count).max_by(|&a, &b| score(a).total_cmp(&score(b))).unwrap_or(reference);
            let agrees = (0..count).map(|other| agreement[label][chosen * count + other] / size >= AGREEMENT).collect();
            AreaChoice { chosen, agrees }
        })
        .collect()
}

/// For aligned frames in order of exposure. `stops` are relative to `reference`.
pub(crate) fn find_ghosts(
    images: &[Image],
    stops: &[f32],
    clip: [f32; 3],
    reference: usize,
    level: Deghost,
) -> Option<GhostMaps> {
    let threshold = threshold_stops(level)?;
    let (width, height) = (images[0].width, images[0].height);
    let long_edge = width.max(height);
    let blur = scaled(COMPARE_BLUR, long_edge).max(0.7);
    let frames: Vec<Radiance> =
        images.iter().zip(stops).map(|(image, stop)| radiance(image, *stop, clip, blur)).collect();

    let mut motion = Plane::new(width, height);
    for pair in 0..frames.len() - 1 {
        let gain = 2f32.powf(stops[pair + 1] - stops[pair]);
        mark_motion(&mut motion, &frames[pair], &frames[pair + 1], gain, threshold);
    }
    let opening = scaled(OPENING_RADIUS, long_edge).round().max(1.0) as usize;
    let growth = scaled(GROWTH_RADIUS, long_edge).round().max(1.0) as usize;
    let cleaned = filled(&grown(&opened(&motion, opening), growth));
    let (labels, sizes) = label_regions(&cleaned);
    if sizes.len() <= 1 {
        return None;
    }

    let chosen = choose_frames(&frames, stops, (&labels, &sizes), reference, threshold);

    let feather = scaled(FEATHER, long_edge).max(0.7);
    let moved = labels.iter().filter(|label| **label != 0).count() as f32;
    let plane_of = |value: &(dyn Fn(u32) -> f32 + Sync)| Plane {
        values: labels.iter().map(|&label| value(label)).collect(),
        width,
        height,
    };
    let only = (0..images.len())
        .map(|index| {
            let say = |label: u32| {
                let choice = &chosen[label as usize];
                match label {
                    0 => 0.0,
                    _ if choice.chosen == index => 1.0,
                    _ if choice.agrees[index] => STAND_IN,
                    _ => 0.0,
                }
            };
            plane_of(&say).blurred(feather)
        })
        .collect();
    let reach = plane_of(&|label| f32::from(u8::from(label != 0))).blurred(feather);
    Some(GhostMaps { only, reach, share: moved / labels.len() as f32 })
}

/// How much of an area a frame alone provides, for tests.
#[cfg(test)]
pub(crate) fn mean_share(maps: &GhostMaps, frame: usize, x: std::ops::Range<usize>, y: std::ops::Range<usize>) -> f32 {
    let plane = &maps.only[frame];
    let mut sum = 0.0;
    let mut count = 0;
    for row in y {
        for column in x.clone() {
            sum += plane.at(column, row);
            count += 1;
        }
    }
    sum / count as f32
}

/// Fraction of a frame's pixels that would count as usable in a merge.
#[cfg(test)]
pub(crate) fn usable_share(image: &Image, clip: [f32; 3]) -> f32 {
    let usable = image.pixels.iter().filter(|pixel| {
        (0..3).all(|c| pixel[c] < clip[c] * BRIGHT) && luma(**pixel) > clip[1] * DARK
    });
    usable.count() as f32 / image.pixels.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: [f32; 3] = [1.0; 3];

    /// Stays clear of the dark limit in every frame. Below it nothing can prove that anything
    /// moved, and in a merge the longer frames outweigh those pixels regardless.
    fn backdrop(x: usize, y: usize) -> f32 {
        0.12 + 0.3 * (x as f32 * 0.03).sin().abs() * (y as f32 * 0.02).cos().abs()
    }

    fn scene(stops: f32, object_at: Option<usize>) -> Image {
        let gain = 2f32.powf(stops);
        Image::from_fn(240, 180, |x, y| {
            let on_object = object_at.is_some_and(|left| (left..left + 40).contains(&x) && (60..120).contains(&y));
            let value = if on_object { 0.02 } else { backdrop(x, y) };
            [(value * gain).min(1.0); 3]
        })
    }

    #[test]
    fn a_still_bracket_has_no_ghosts() {
        let images = [scene(-2.0, None), scene(0.0, None), scene(2.0, None)];
        assert!(find_ghosts(&images, &[-2.0, 0.0, 2.0], CLIP, 1, Deghost::High).is_none());
    }

    #[test]
    fn switched_off_finds_nothing() {
        let images = [scene(-2.0, Some(40)), scene(0.0, None)];
        assert!(find_ghosts(&images, &[-2.0, 0.0], CLIP, 1, Deghost::None).is_none());
    }

    #[test]
    fn a_moved_object_is_left_to_a_single_frame() {
        let images = [scene(-1.0, Some(40)), scene(0.0, Some(150)), scene(1.0, Some(150))];
        let maps = find_ghosts(&images, &[-1.0, 0.0, 1.0], CLIP, 1, Deghost::Medium).unwrap();
        assert!(maps.share > 0.03, "share {}", maps.share);
        let kept: Vec<f32> = (0..3).map(|frame| mean_share(&maps, frame, 50..70, 75..105)).collect();
        let winners = kept.iter().filter(|&&keep| keep > 0.9).count();
        let losers = kept.iter().filter(|&&keep| keep < 0.1).count();
        assert_eq!((winners, losers), (1, 2), "{kept:?}");
        for frame in 0..3 {
            assert!(mean_share(&maps, frame, 200..220, 20..40) < 0.01);
        }
    }

    #[test]
    fn the_frame_that_disagrees_loses_even_when_its_the_best_exposed() {
        // A dark object appears in the long frame only, exactly where the other two are bright
        // enough to sit near clipping, so a choice by exposure alone would pick it.
        let scene = |stops: f32, object: bool| {
            let gain = 2f32.powf(stops);
            Image::from_fn(240, 180, |x, y| {
                let on_object = (90..150).contains(&x) && (60..120).contains(&y);
                let value = if on_object && object { 0.12 / gain } else { 0.1 + (x as f32 * 0.02).sin().abs() * 0.1 };
                [(value * gain).min(1.0); 3]
            })
        };
        let images = [scene(-1.0, false), scene(0.0, false), scene(2.0, true)];
        let maps = find_ghosts(&images, &[-1.0, 0.0, 2.0], CLIP, 1, Deghost::Medium).unwrap();
        let kept: Vec<f32> = (0..3).map(|frame| mean_share(&maps, frame, 110..130, 80..100)).collect();
        assert!(kept[2] < 0.1, "the odd frame kept {kept:?}");
        assert!(kept[0] > 0.9 || kept[1] > 0.9, "{kept:?}");
    }

    #[test]
    fn a_short_exposure_clipped_where_a_long_one_isnt_counts_as_motion() {
        // Bright enough to clip the reference, and the long frame shows something dark in it.
        // The frames disagreeing here are never both exposed, so no ratio can show it.
        let scene = |stops: f32, object: bool| {
            let gain = 2f32.powf(stops);
            Image::from_fn(240, 180, |x, y| {
                let on_object = (90..150).contains(&x) && (60..120).contains(&y);
                let value = if on_object && object { 0.1 / gain } else { 2.0 };
                [(value * gain).min(1.0); 3]
            })
        };
        let images = [scene(-2.0, false), scene(0.0, false), scene(2.0, true)];
        let maps = find_ghosts(&images, &[-2.0, 0.0, 2.0], CLIP, 1, Deghost::Medium).unwrap();
        let kept: Vec<f32> = (0..3).map(|frame| mean_share(&maps, frame, 110..130, 80..100)).collect();
        assert!(kept[2] < 0.1, "the frame with the object kept {kept:?}");
        assert!(kept[0] > 0.9, "the frame that exposed the light properly should win: {kept:?}");
    }

    #[test]
    fn a_saturated_colour_clipping_in_one_channel_isnt_motion() {
        // Red clips in the middle frame while luma stays low, the way a vivid orange does.
        let scene = |stops: f32| {
            let gain = 2f32.powf(stops);
            Image::from_fn(240, 180, |x, y| {
                let on_colour = (60..180).contains(&x) && (40..140).contains(&y);
                let base = if on_colour { [1.6, 0.05, 0.02] } else { [0.2, 0.2, 0.2] };
                base.map(|value| (value * gain).min(1.0))
            })
        };
        let images = [scene(-2.0), scene(0.0), scene(2.0)];
        assert!(find_ghosts(&images, &[-2.0, 0.0, 2.0], CLIP, 1, Deghost::High).is_none());
    }

    #[test]
    fn something_clipped_in_one_frame_only_is_found() {
        let object = |stops: f32, bright: bool| {
            let gain = 2f32.powf(stops);
            Image::from_fn(240, 180, |x, y| {
                let on_object = (80..140).contains(&x) && (50..130).contains(&y);
                let value = if bright && on_object { 8.0 } else { 0.05 + (x as f32 * 0.02).sin().abs() * 0.1 };
                [(value * gain).min(1.0); 3]
            })
        };
        let images = [object(-1.0, false), object(1.0, true)];
        assert!(usable_share(&images[0], CLIP) > 0.5);
        let maps = find_ghosts(&images, &[-1.0, 1.0], CLIP, 0, Deghost::Medium).unwrap();
        assert!(maps.share > 0.05, "share {}", maps.share);
    }

    #[test]
    fn labels_separate_areas_that_dont_touch() {
        let mask = Plane::from_fn(10, 3, |x, _| f32::from(u8::from(!(3..=6).contains(&x))));
        let (labels, sizes) = label_regions(&mask);
        assert_eq!(sizes.len(), 3);
        assert_ne!(labels[0], labels[9]);
        assert_eq!(labels[4], 0);
    }

    #[test]
    fn holes_fill_but_outside_stays_outside() {
        let ring = Plane::from_fn(11, 11, |x, y| {
            let edge = |value: usize| value == 2 || value == 8;
            let within = |value: usize| (2..=8).contains(&value);
            f32::from(u8::from((edge(x) && within(y)) || (edge(y) && within(x))))
        });
        let solid = filled(&ring);
        assert_eq!(solid.at(5, 5), 1.0);
        assert_eq!(solid.at(0, 0), 0.0);
        assert_eq!(solid.at(9, 5), 0.0);
    }

    #[test]
    fn opening_drops_specks_and_keeps_areas() {
        let mut mask = Plane::new(32, 32);
        mask.values[5 * 32 + 5] = 1.0;
        for y in 12..28 {
            for x in 12..28 {
                mask.values[y * 32 + x] = 1.0;
            }
        }
        let cleaned = opened(&mask, 1);
        assert_eq!(cleaned.at(5, 5), 0.0);
        assert_eq!(cleaned.at(20, 20), 1.0);
    }
}
