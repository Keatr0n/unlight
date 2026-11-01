use crate::{Error, Result};
use rayon::prelude::*;
use std::borrow::Cow;
use unlight_color::output::OutputSpace;

const CHANNELS: usize = 3;

/// A finished image: interleaved RGB, gamma-encoded in `space`. Values outside 0..=1 clip
/// when written.
#[derive(Clone, Debug)]
pub struct Image<'a> {
    width: u32,
    height: u32,
    space: OutputSpace,
    pixels: Cow<'a, [f32]>,
}

impl<'a> Image<'a> {
    pub fn new(
        width: u32,
        height: u32,
        space: OutputSpace,
        pixels: impl Into<Cow<'a, [f32]>>,
    ) -> Result<Self> {
        let pixels = pixels.into();
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(CHANNELS));
        match expected {
            Some(expected) if expected == pixels.len() && expected > 0 => Ok(Self {
                width,
                height,
                space,
                pixels,
            }),
            _ => Err(Error::ImageSize {
                width,
                height,
                expected: expected.unwrap_or(usize::MAX),
                actual: pixels.len(),
            }),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn space(&self) -> OutputSpace {
        self.space
    }

    pub fn pixels(&self) -> &[f32] {
        &self.pixels
    }

    pub(crate) fn pixels_mut(&mut self) -> &mut [f32] {
        self.pixels.to_mut()
    }

    pub(crate) fn row_samples(&self) -> usize {
        self.width as usize * CHANNELS
    }

    pub(crate) fn view(&self) -> Image<'_> {
        Image {
            pixels: Cow::Borrowed(&self.pixels),
            ..*self
        }
    }
}

fn quantize(value: f32, max: f32) -> u16 {
    (value.clamp(0.0, 1.0) * max + 0.5) as u16
}

pub(crate) fn quantize_u8(pixels: &[f32]) -> Vec<u8> {
    pixels
        .par_iter()
        .map(|&value| quantize(value, 255.0) as u8)
        .collect()
}

pub(crate) fn quantize_u16(pixels: &[f32], max: u16) -> Vec<u16> {
    let max = f32::from(max);
    pixels
        .par_iter()
        .map(|&value| quantize(value, max))
        .collect()
}

/// 16-bit samples as bytes in the order `to_bytes` gives them.
pub(crate) fn quantize_u16_bytes(pixels: &[f32], to_bytes: fn(u16) -> [u8; 2]) -> Vec<u8> {
    let mut bytes = vec![0; pixels.len() * 2];
    bytes
        .par_chunks_exact_mut(2)
        .zip(pixels.par_iter())
        .for_each(|(target, &value)| {
            target.copy_from_slice(&to_bytes(quantize(value, 65535.0)));
        });
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_buffer_of_the_wrong_length() {
        let pixels = vec![0.0; 11];
        assert!(matches!(
            Image::new(2, 2, OutputSpace::Srgb, pixels.as_slice()),
            Err(Error::ImageSize {
                expected: 12,
                actual: 11,
                ..
            })
        ));
    }

    #[test]
    fn rejects_an_empty_image() {
        assert!(Image::new(0, 0, OutputSpace::Srgb, vec![]).is_err());
    }

    #[test]
    fn dimensions_that_overflow_are_rejected_rather_than_wrapping() {
        assert!(Image::new(u32::MAX, u32::MAX, OutputSpace::Srgb, vec![]).is_err());
    }

    #[test]
    fn every_8_bit_code_survives_a_float_round_trip() {
        let pixels: Vec<f32> = (0..=255).map(|code| code as f32 / 255.0).collect();
        let expected: Vec<u8> = (0..=255).collect();
        assert_eq!(quantize_u8(&pixels), expected);
    }

    #[test]
    fn every_16_bit_code_survives_a_float_round_trip() {
        let pixels: Vec<f32> = (0..=65535).map(|code| code as f32 / 65535.0).collect();
        let expected: Vec<u16> = (0..=65535).collect();
        assert_eq!(quantize_u16(&pixels, u16::MAX), expected);
    }

    #[test]
    fn rounds_to_nearest_rather_than_truncating() {
        let just_below_one_code = 0.9 / 255.0;
        let just_below_half_code = 0.4 / 255.0;
        assert_eq!(
            quantize_u8(&[just_below_one_code, just_below_half_code]),
            [1, 0]
        );
    }

    #[test]
    fn out_of_range_and_nan_samples_clip() {
        assert_eq!(
            quantize_u8(&[-1.0, 2.0, f32::NAN, f32::INFINITY]),
            [0, 255, 0, 255]
        );
    }

    #[test]
    fn scales_to_10_and_12_bit_ranges() {
        assert_eq!(quantize_u16(&[1.0, 0.5], 1023), [1023, 512]);
        assert_eq!(quantize_u16(&[1.0, 0.5], 4095), [4095, 2048]);
    }

    #[test]
    fn byte_order_is_the_callers_choice() {
        let sample = [1.0 / 256.0 + 1e-6];
        let expected = (65535.0 / 256.0 + 0.5) as u16;
        assert_eq!(
            quantize_u16_bytes(&sample, u16::to_be_bytes),
            expected.to_be_bytes()
        );
        assert_eq!(
            quantize_u16_bytes(&sample, u16::to_le_bytes),
            expected.to_le_bytes()
        );
    }
}
