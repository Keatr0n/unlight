//! Relative exposure between frames. The file's shutter and aperture give a first answer, but
//! those are nominal values: cameras round to third stops, and a file may say nothing at all.
//! Raw data is linear, so two frames of the same scene differ by one gain, and that can be read
//! straight off the pixels both of them exposed properly. The measured ratio wins whenever
//! there is enough to measure.

use crate::{
    Frame, Image, MergeError, Result,
    image::{luma, median},
};

/// Pixels needed to trust a measured ratio.
const MIN_MEASURED: usize = 500;
/// Only well-exposed pixels count, as a share of the clip level. Above the top of this range
/// demosaicing and highlight repair have touched them, below the bottom the sensor's offset
/// and noise bias the ratio.
const USABLE_TOP: f32 = 0.85;
const USABLE_BOTTOM: f32 = 0.012;
/// Read this far apart in stops, measurement and file disagree enough to be worth reporting.
pub(crate) const DISAGREEMENT_STOPS: f32 = 0.25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposureSource {
    Measured,
    /// The frames shared too little to measure, so this is what the file said.
    Metadata,
    Reference,
}

/// A frame's exposure relative to the reference, in stops.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExposureEstimate {
    pub stated: Option<f32>,
    pub measured: Option<f32>,
    pub used: f32,
    pub source: ExposureSource,
}

impl ExposureEstimate {
    /// Whether the file and the pixels tell different stories about this frame.
    pub fn disagrees(&self) -> bool {
        match (self.stated, self.measured) {
            (Some(stated), Some(measured)) => (stated - measured).abs() > DISAGREEMENT_STOPS,
            _ => false,
        }
    }
}

/// Stops by which `longer` out-exposes `shorter`, from the pixels both exposed properly.
/// Both images must be aligned and share one clip level.
pub(crate) fn measure_stops(longer: &Image, shorter: &Image, clip: [f32; 3]) -> Option<f32> {
    let top = clip.map(|level| level * USABLE_TOP);
    let bottom = clip[1] * USABLE_BOTTOM;
    let usable = |pixel: [f32; 3]| (0..3).all(|c| pixel[c] < top[c]);
    let mut ratios: Vec<f32> = longer
        .pixels
        .iter()
        .zip(&shorter.pixels)
        .filter(|&(&long, &short)| usable(long) && usable(short) && luma(short) > bottom && luma(long) > bottom)
        .map(|(&long, &short)| (luma(long) / luma(short)).log2())
        .collect();
    if ratios.len() < MIN_MEASURED {
        return None;
    }
    median(&mut ratios)
}

/// How to order frames when a file has no shutter speed: by the brightness of the small copy.
pub(crate) fn brightness(frame: &Frame) -> f32 {
    let clip = frame.clip_levels();
    let mut levels: Vec<f32> = frame
        .reduced
        .pixels
        .iter()
        .map(|&pixel| luma(pixel) / clip[1].max(1e-6))
        .collect();
    median(&mut levels).unwrap_or(0.0)
}

/// Indices of `frames`, darkest exposure first.
pub(crate) fn exposure_order(frames: &[Frame]) -> Vec<usize> {
    let stated: Option<Vec<f64>> = frames.iter().map(Frame::stated_exposure).collect();
    let mut order: Vec<usize> = (0..frames.len()).collect();
    match stated {
        Some(values) => order.sort_by(|&a, &b| values[a].total_cmp(&values[b])),
        None => {
            let values: Vec<f32> = frames.iter().map(brightness).collect();
            order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
        }
    }
    order
}

/// Every frame's exposure relative to `reference`, for images that are aligned, balanced alike
/// and in order of exposure. `frames` is in that same order. Neighbours are measured, then the
/// results are chained, since frames far apart may share no usable pixels.
pub(crate) fn resolve_exposures(
    frames: &[&Frame],
    images: &[Image],
    clip: [f32; 3],
    reference: usize,
) -> Result<Vec<ExposureEstimate>> {
    let stated = |index: usize| -> Option<f32> {
        let frame = frames[index].stated_exposure()?;
        let base = frames[reference].stated_exposure()?;
        Some((frame / base).log2() as f32)
    };
    let steps: Vec<(Option<f32>, Option<f32>)> = (1..frames.len())
        .map(|index| {
            let measured = measure_stops(&images[index], &images[index - 1], clip);
            let file = match (frames[index].stated_exposure(), frames[index - 1].stated_exposure()) {
                (Some(long), Some(short)) => Some((long / short).log2() as f32),
                _ => None,
            };
            (measured, file)
        })
        .collect();

    let mut used = vec![0.0f32; frames.len()];
    let mut measured = vec![None; frames.len()];
    let mut sources = vec![ExposureSource::Reference; frames.len()];
    let mut settle = |index: usize, neighbour: usize, step: (Option<f32>, Option<f32>), direction: f32| {
        let previous_measured = if neighbour == reference { Some(0.0) } else { measured[neighbour] };
        let Some(value) = step.0.or(step.1) else {
            return Err(MergeError::UnknownExposure(frames[index].path.clone()));
        };
        used[index] = used[neighbour] + direction * value;
        measured[index] = match (previous_measured, step.0) {
            (Some(known), Some(found)) => Some(known + direction * found),
            _ => None,
        };
        let was_estimated = neighbour != reference && sources[neighbour] == ExposureSource::Metadata;
        sources[index] = if step.0.is_some() && !was_estimated {
            ExposureSource::Measured
        } else {
            ExposureSource::Metadata
        };
        Ok(())
    };
    for index in reference + 1..frames.len() {
        settle(index, index - 1, steps[index - 1], 1.0)?;
    }
    for index in (0..reference).rev() {
        settle(index, index + 1, steps[index], -1.0)?;
    }
    Ok((0..frames.len())
        .map(|index| ExposureEstimate {
            stated: stated(index),
            measured: measured[index],
            used: used[index],
            source: sources[index],
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::plain_frame;

    const CLIP: [f32; 3] = [1.0; 3];

    fn scene(gain: f32) -> Image {
        Image::from_fn(256, 256, |x, y| {
            let level = (0.003 * 1.03f32.powi((x as i32 + y as i32) / 2)).min(8.0);
            [(level * gain).min(1.0), (level * gain * 0.8).min(1.0), (level * gain * 0.6).min(1.0)]
        })
    }

    fn frame_of(image: &Image, shutter: f32) -> Frame {
        let mut frame = plain_frame(image, 128);
        frame.metadata.shutter = shutter;
        frame
    }

    #[test]
    fn the_ratio_comes_from_the_pixels() {
        let stops = measure_stops(&scene(4.0), &scene(1.0), CLIP).unwrap();
        assert!((stops - 2.0).abs() < 1e-3, "{stops}");
    }

    #[test]
    fn nothing_in_common_measures_nothing() {
        let blown = Image::from_fn(64, 64, |_, _| [1.0; 3]);
        let dark = Image::from_fn(64, 64, |_, _| [0.001; 3]);
        assert_eq!(measure_stops(&blown, &dark, CLIP), None);
    }

    #[test]
    fn the_pixels_overrule_a_wrong_file() {
        let images = [scene(0.25), scene(1.0), scene(4.0)];
        let frames = [frame_of(&images[0], 0.0025), frame_of(&images[1], 0.01), frame_of(&images[2], 0.02)];
        let ordered: Vec<&Frame> = frames.iter().collect();
        let found = resolve_exposures(&ordered, &images, CLIP, 1).unwrap();
        assert!((found[2].used - 2.0).abs() < 1e-3, "{:?}", found[2]);
        assert_eq!(found[2].stated, Some(1.0));
        assert_eq!(found[2].source, ExposureSource::Measured);
        assert!(found[2].disagrees());
        assert!((found[0].used + 2.0).abs() < 1e-3, "{:?}", found[0]);
        assert!(!found[0].disagrees());
        assert_eq!(found[1].used, 0.0);
    }

    #[test]
    fn a_file_with_no_shutter_speed_is_measured() {
        let images = [scene(0.25), scene(1.0)];
        let frames = [frame_of(&images[0], 0.0), frame_of(&images[1], 0.0)];
        let ordered: Vec<&Frame> = frames.iter().collect();
        let found = resolve_exposures(&ordered, &images, CLIP, 1).unwrap();
        assert_eq!(found[0].stated, None);
        assert!((found[0].used + 2.0).abs() < 1e-3);
    }

    #[test]
    fn frames_too_far_apart_fall_back_to_the_file() {
        let images = [Image::from_fn(64, 64, |_, _| [0.0005; 3]), Image::from_fn(64, 64, |_, _| [1.0; 3])];
        let frames = [frame_of(&images[0], 0.001), frame_of(&images[1], 0.064)];
        let ordered: Vec<&Frame> = frames.iter().collect();
        let found = resolve_exposures(&ordered, &images, CLIP, 1).unwrap();
        assert_eq!(found[0].source, ExposureSource::Metadata);
        assert!((found[0].used + 6.0).abs() < 1e-3);
    }

    #[test]
    fn no_file_and_no_overlap_is_an_error() {
        let images = [Image::from_fn(64, 64, |_, _| [0.0005; 3]), Image::from_fn(64, 64, |_, _| [1.0; 3])];
        let frames = [frame_of(&images[0], 0.0), frame_of(&images[1], 0.0)];
        let ordered: Vec<&Frame> = frames.iter().collect();
        assert!(matches!(resolve_exposures(&ordered, &images, CLIP, 1), Err(MergeError::UnknownExposure(_))));
    }

    #[test]
    fn order_follows_the_file_then_the_pixels() {
        let dark = scene(0.1);
        let bright = scene(2.0);
        let frames = [frame_of(&bright, 0.1), frame_of(&dark, 0.01)];
        assert_eq!(exposure_order(&frames), [1, 0]);
        let unknown = [frame_of(&bright, 0.0), frame_of(&dark, 0.0)];
        assert_eq!(exposure_order(&unknown), [1, 0]);
    }
}
