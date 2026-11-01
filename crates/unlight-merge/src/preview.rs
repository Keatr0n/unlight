//! The small JPEG a merged file carries for the Library, in place of a camera's embedded one.
//! A camera's JPEG doesn't match a default render exactly either, so this only has to be
//! recognisably the same picture, and it costs no trip through the GPU.

use crate::{Image, Merged};
use jpeg_encoder::{ColorType, Encoder};
use rayon::prelude::*;
use unlight_color::{
    CameraProfile,
    matrix::{apply, prophoto_to_srgb},
};

const LONG_EDGE: usize = 2048;
const QUALITY: u8 = 85;
/// Where the tone curve starts compressing, to keep highlights above 1 from all going white.
const SHOULDER: f32 = 0.75;

fn encode_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 };
    (encoded * 255.0 + 0.5) as u8
}

/// Passes values through up to the shoulder, then rolls them off towards 1.
fn tone(value: f32) -> f32 {
    if value <= SHOULDER {
        return value;
    }
    let over = (value - SHOULDER) / (1.0 - SHOULDER);
    SHOULDER + (1.0 - SHOULDER) * over / (1.0 + over)
}

fn rendered(merged: &Merged, image: &Image) -> Vec<u8> {
    let profile = CameraProfile::new(&merged.color);
    let to_prophoto = profile.camera_to_prophoto(&merged.neutral);
    let to_srgb = prophoto_to_srgb();
    let neutral = merged.neutral;
    let gain = 2f32.powf(merged.color.baseline_exposure);
    image
        .pixels
        .par_iter()
        .flat_map_iter(|pixel| {
            let camera = std::array::from_fn(|c| f64::from(pixel[c]) * neutral[c]);
            let linear = apply(&to_srgb, &apply(&to_prophoto, &camera));
            linear.map(|value| encode_srgb(tone(value as f32 * gain)))
        })
        .collect()
}

pub(crate) fn preview_jpeg(merged: &Merged) -> Vec<u8> {
    let small = merged.image.fitted(LONG_EDGE);
    let pixels = rendered(merged, &small);
    let mut bytes = Vec::new();
    let encoded = Encoder::new(&mut bytes, QUALITY).encode(
        &pixels,
        small.width as u16,
        small.height as u16,
        ColorType::Rgb,
    );
    if encoded.is_ok() { bytes } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tone_curve_is_continuous_and_stays_in_range() {
        assert_eq!(tone(0.5), 0.5);
        assert!((tone(SHOULDER + 1e-5) - SHOULDER).abs() < 1e-4);
        assert!(tone(1.0) < 1.0);
        assert!(tone(1000.0) <= 1.0);
        assert!(tone(4.0) > tone(2.0));
    }

    #[test]
    fn srgb_encoding_hits_its_landmarks() {
        assert_eq!(encode_srgb(0.0), 0);
        assert_eq!(encode_srgb(1.0), 255);
        assert_eq!(encode_srgb(0.214), 127);
    }
}
