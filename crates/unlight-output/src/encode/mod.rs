mod avif;
mod jpeg;
mod jxl;
mod png;
mod tiff;

use crate::metadata::Embedded;
use crate::settings::FormatSettings;
use crate::{Error, Image, Result};
use std::fmt::Display;
use std::io::Write;

/// A finished image and everything that travels with it.
pub(crate) struct Input<'a> {
    pub image: &'a Image<'a>,
    pub icc: &'a [u8],
    pub metadata: &'a Embedded,
}

pub(crate) fn encode(
    input: &Input<'_>,
    format: &FormatSettings,
    out: &mut dyn Write,
) -> Result<()> {
    match format {
        FormatSettings::Jpeg(settings) => jpeg::encode(input, settings, out),
        FormatSettings::Tiff(settings) => tiff::encode(input, settings, out),
        FormatSettings::Png(settings) => png::encode(input, settings, out),
        FormatSettings::JpegXl(settings) => jxl::encode(input, settings, out),
        FormatSettings::Avif(settings) => avif::encode(input, settings, out),
    }
}

fn failure(format: &'static str) -> impl Fn(&dyn Display) -> Error {
    move |detail| Error::Encode {
        format,
        message: detail.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::Image;
    use crate::metadata::{Embedded, ImageMetadata, MetadataSettings};
    use unlight_color::output::OutputSpace;

    /// Smooth gradients in each channel, with a band of fine detail along the bottom.
    pub fn test_image(width: u32, height: u32, space: OutputSpace) -> Image<'static> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height {
            for x in 0..width {
                let u = x as f32 / (width - 1).max(1) as f32;
                let v = y as f32 / (height - 1).max(1) as f32;
                let detail = if v > 0.75 {
                    ((x * 7 + y * 3) % 5) as f32 / 40.0
                } else {
                    0.0
                };
                pixels.extend([u * 0.9 + detail, v * 0.9 + detail, (1.0 - u) * 0.5 + detail]);
            }
        }
        Image::new(width, height, space, pixels).unwrap()
    }

    /// Every 16-bit code once, on all three channels of a 256×256 image.
    pub fn all_u16_codes(space: OutputSpace) -> Image<'static> {
        let pixels: Vec<f32> = (0..=65535u16)
            .flat_map(|code| [f32::from(code) / 65535.0; 3])
            .collect();
        Image::new(256, 256, space, pixels).unwrap()
    }

    pub fn sample_metadata() -> ImageMetadata {
        ImageMetadata {
            camera_make: Some("SONY".into()),
            camera_model: Some("ILCE-7RM5".into()),
            iso: Some(400),
            copyright: Some("© 2026 Jane Doe".into()),
            ..Default::default()
        }
    }

    pub fn embedded(image: &Image<'_>) -> Embedded {
        Embedded::new(
            &sample_metadata(),
            MetadataSettings::default(),
            [image.width(), image.height()],
            image.space(),
        )
    }

    pub fn psnr(expected: &[f32], actual: &[f32]) -> f64 {
        assert_eq!(expected.len(), actual.len());
        let squared: f64 = expected
            .iter()
            .zip(actual)
            .map(|(a, b)| f64::from(a.clamp(0.0, 1.0) - b.clamp(0.0, 1.0)).powi(2))
            .sum();
        let mse = squared / expected.len() as f64;
        if mse == 0.0 {
            f64::INFINITY
        } else {
            -10.0 * mse.log10()
        }
    }
}
