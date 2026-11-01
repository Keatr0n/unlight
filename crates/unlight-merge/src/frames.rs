//! What a merge is given: one record per photo with a small copy already in memory, and the
//! full frame only on request. A merge asks for one full frame at a time, so its memory use
//! follows the output size rather than the number of photos.

use crate::{Image, Result};
use std::path::PathBuf;
use unlight_raw::{ColorData, EmbeddedLens, Metadata, Opcodes};

/// Sensor sites this close to the white level count as clipped. Same value as the engine's.
pub(crate) const SENSOR_CLIP: f32 = 0.985;

#[derive(Clone, Debug)]
pub struct Frame {
    pub path: PathBuf,
    /// Size of the full frame.
    pub width: usize,
    pub height: usize,
    pub metadata: Metadata,
    pub color: ColorData,
    pub opcodes: Opcodes,
    pub embedded_lens: Option<EmbeddedLens>,
    /// What each channel was multiplied by to white balance this frame. The engine hands frames
    /// over like this, and frames with different as-shot balances have to agree before merging.
    pub balance: [f32; 3],
    /// A small copy of the frame, the same colour as the full one.
    pub reduced: Image,
}

impl Frame {
    /// The level each channel saturates at in this frame's own values.
    pub(crate) fn clip_levels(&self) -> [f32; 3] {
        self.balance.map(|gain| gain * SENSOR_CLIP)
    }

    /// Relative light captured, according to the file: time times sensitivity, over the
    /// aperture's area. `None` if the file doesn't record a shutter speed.
    pub(crate) fn stated_exposure(&self) -> Option<f64> {
        let shutter = f64::from(self.metadata.shutter);
        if !shutter.is_finite() || shutter <= 0.0 {
            return None;
        }
        let iso = f64::from(self.metadata.iso);
        let aperture = f64::from(self.metadata.aperture);
        let iso = if iso > 0.0 { iso / 100.0 } else { 1.0 };
        let area = if aperture > 0.0 { aperture * aperture } else { 1.0 };
        Some(shutter * iso / area)
    }
}

pub trait FrameSource: Sync {
    fn frames(&self) -> &[Frame];

    /// A whole frame, in the same colour as its `reduced` copy. Only called for one frame at a
    /// time, and the merge drops it before asking for the next.
    fn full(&self, index: usize) -> Result<Image>;
}

/// Frames that are already in memory, which is what the tests and synthetic sets use.
#[cfg(test)]
pub(crate) struct MemoryFrames {
    pub frames: Vec<Frame>,
    pub images: Vec<Image>,
}

#[cfg(test)]
impl FrameSource for MemoryFrames {
    fn frames(&self) -> &[Frame] {
        &self.frames
    }

    fn full(&self, index: usize) -> Result<Image> {
        Ok(self.images[index].clone())
    }
}

#[cfg(test)]
pub(crate) fn plain_frame(image: &Image, reduced_edge: usize) -> Frame {
    Frame {
        path: PathBuf::from("frame"),
        width: image.width,
        height: image.height,
        metadata: Metadata::default(),
        color: ColorData {
            xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            as_shot_multipliers: [1.0; 3],
            daylight_multipliers: [1.0; 3],
            baseline_exposure: 0.0,
            dng: None,
        },
        opcodes: Opcodes::default(),
        embedded_lens: None,
        balance: [1.0; 3],
        reduced: image.fitted(reduced_edge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(shutter: f32, iso: f32, aperture: f32) -> Frame {
        let mut frame = plain_frame(&Image::new(8, 8), 8);
        frame.metadata.shutter = shutter;
        frame.metadata.iso = iso;
        frame.metadata.aperture = aperture;
        frame
    }

    #[test]
    fn a_stop_of_anything_is_a_stop_of_exposure() {
        let base = frame(0.01, 100.0, 4.0).stated_exposure().unwrap();
        let ratio = |other: &Frame| (other.stated_exposure().unwrap() / base).log2();
        assert!((ratio(&frame(0.02, 100.0, 4.0)) - 1.0).abs() < 1e-5);
        assert!((ratio(&frame(0.01, 200.0, 4.0)) - 1.0).abs() < 1e-5);
        assert!((ratio(&frame(0.01, 100.0, 2.8284)) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn missing_sensitivity_and_aperture_dont_hide_the_shutter() {
        assert!(frame(0.01, 0.0, 0.0).stated_exposure().is_some());
        assert!(frame(0.0, 100.0, 4.0).stated_exposure().is_none());
    }

    #[test]
    fn clip_levels_follow_the_white_balance() {
        let mut frame = frame(0.01, 100.0, 4.0);
        frame.balance = [2.0, 1.0, 1.5];
        assert_eq!(frame.clip_levels(), [2.0 * SENSOR_CLIP, SENSOR_CLIP, 1.5 * SENSOR_CLIP]);
    }
}
