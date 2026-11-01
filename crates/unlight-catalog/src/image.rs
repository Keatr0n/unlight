use crate::{Error, Result};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use jpeg_encoder::{ColorType, Encoder};
use rayon::prelude::*;
use std::{fmt, io::Cursor};
use unlight_raw::Preview;
use zune_core::{colorspace::ColorSpace, options::DecoderOptions};
use zune_jpeg::JpegDecoder;

/// zune-jpeg stops at 16384 by default, below the largest embedded previews.
const MAX_JPEG_EDGE: usize = 65_500;
const CACHE_JPEG_QUALITY: u8 = 90;
const BYTES_PER_PIXEL: usize = 4;

/// Tightly packed 8-bit RGBA with opaque alpha, ready for a GPU texture.
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl fmt::Debug for RgbaImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RgbaImage({}x{})", self.width, self.height)
    }
}

pub(crate) fn decode_preview(preview: &Preview) -> Result<RgbaImage> {
    match preview {
        Preview::Jpeg(bytes) => decode_jpeg(bytes, false),
        Preview::Rgb8 { width, height, data } => rgb8_to_rgba(*width, *height, data),
    }
}

/// `strict` turns any damage into an error. Otherwise a truncated stream decodes as far as it
/// goes, which suits an embedded preview, where part of an image beats none.
pub(crate) fn decode_jpeg(bytes: &[u8], strict: bool) -> Result<RgbaImage> {
    let options = DecoderOptions::new_fast()
        .set_strict_mode(strict)
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_JPEG_EDGE)
        .set_max_height(MAX_JPEG_EDGE);
    let mut decoder = JpegDecoder::new_with_options(Cursor::new(bytes), options);
    let pixels = decoder.decode().map_err(|error| Error::Decode(error.to_string()))?;
    let info = decoder.info().ok_or_else(|| Error::Decode("no image header".to_owned()))?;
    let (width, height) = (info.width as u32, info.height as u32);
    if pixels.len() != width as usize * height as usize * BYTES_PER_PIXEL {
        return Err(Error::Decode(format!("{} bytes for {width}x{height}", pixels.len())));
    }
    Ok(RgbaImage { width, height, pixels })
}

fn rgb8_to_rgba(width: u32, height: u32, data: &[u8]) -> Result<RgbaImage> {
    if data.len() != width as usize * height as usize * 3 {
        return Err(Error::Decode(format!("{} bytes of RGB for {width}x{height}", data.len())));
    }
    let mut pixels = vec![u8::MAX; width as usize * height as usize * BYTES_PER_PIXEL];
    pixels.par_chunks_exact_mut(BYTES_PER_PIXEL).zip(data.par_chunks_exact(3)).for_each(
        |(out, rgb)| {
            out[..3].copy_from_slice(rgb);
        },
    );
    Ok(RgbaImage { width, height, pixels })
}

/// Shrinks so the long edge is at most `long_edge`. Never enlarges.
pub(crate) fn fit_long_edge(image: RgbaImage, long_edge: u32) -> Result<RgbaImage> {
    let current = image.width.max(image.height);
    if current <= long_edge {
        return Ok(image);
    }
    let scale = long_edge as f64 / current as f64;
    let scaled = |edge: u32| ((edge as f64 * scale).round() as u32).max(1);
    let (width, height) = (scaled(image.width), scaled(image.height));

    let resize_error = |error: &dyn fmt::Display| Error::Decode(format!("resize: {error}"));
    let source = Image::from_vec_u8(image.width, image.height, image.pixels, PixelType::U8x4)
        .map_err(|error| resize_error(&error))?;
    let mut target = Image::new(width, height, PixelType::U8x4);
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Hamming))
        .use_alpha(false);
    Resizer::new().resize(&source, &mut target, &options).map_err(|error| resize_error(&error))?;
    Ok(RgbaImage { width, height, pixels: target.into_vec() })
}

/// Applies a LibRaw flip code using dcraw's `flip_index`: bit 2 swaps the axes, then bits 1
/// and 0 mirror the source rows and columns.
pub(crate) fn apply_orientation(image: RgbaImage, flip: i32) -> RgbaImage {
    if flip <= 0 || flip > 7 {
        return image;
    }
    let (source_width, source_height) = (image.width as usize, image.height as usize);
    let swap_axes = flip & 4 != 0;
    let (width, height) =
        if swap_axes { (source_height, source_width) } else { (source_width, source_height) };
    let source = &image.pixels;
    let mut pixels = vec![0; width * height * BYTES_PER_PIXEL];
    pixels.par_chunks_exact_mut(width * BYTES_PER_PIXEL).enumerate().for_each(|(row, line)| {
        let (line, _) = line.as_chunks_mut::<BYTES_PER_PIXEL>();
        for (col, out) in line.iter_mut().enumerate() {
            let (mut source_row, mut source_col) = if swap_axes { (col, row) } else { (row, col) };
            if flip & 2 != 0 {
                source_row = source_height - 1 - source_row;
            }
            if flip & 1 != 0 {
                source_col = source_width - 1 - source_col;
            }
            let start = (source_row * source_width + source_col) * BYTES_PER_PIXEL;
            out.copy_from_slice(&source[start..start + BYTES_PER_PIXEL]);
        }
    });
    RgbaImage { width: width as u32, height: height as u32, pixels }
}

pub(crate) fn encode_jpeg(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    Encoder::new(&mut bytes, CACHE_JPEG_QUALITY)
        .encode(&image.pixels, image.width as u16, image.height as u16, ColorType::Rgba)
        .map_err(|error| Error::Encode(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        let start = (y * image.width + x) as usize * 4;
        image.pixels[start..start + 4].try_into().unwrap()
    }

    /// A 3x2 image whose red channel identifies each pixel:
    /// ```text
    /// 1 2 3
    /// 4 5 6
    /// ```
    fn numbered() -> RgbaImage {
        let pixels = (1..=6).flat_map(|n| [n, 0, 0, 255]).collect();
        RgbaImage { width: 3, height: 2, pixels }
    }

    fn reds(image: &RgbaImage) -> Vec<u8> {
        image.pixels.as_chunks::<4>().0.iter().map(|pixel| pixel[0]).collect()
    }

    #[test]
    fn flip_0_changes_nothing() {
        assert_eq!(apply_orientation(numbered(), 0), numbered());
    }

    #[test]
    fn unknown_flip_codes_change_nothing() {
        assert_eq!(apply_orientation(numbered(), -1), numbered());
        assert_eq!(apply_orientation(numbered(), 8), numbered());
    }

    #[test]
    fn flip_3_rotates_180() {
        assert_eq!(reds(&apply_orientation(numbered(), 3)), [6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn flip_6_rotates_90_clockwise() {
        let rotated = apply_orientation(numbered(), 6);
        assert_eq!((rotated.width, rotated.height), (2, 3));
        assert_eq!(reds(&rotated), [4, 1, 5, 2, 6, 3]);
    }

    #[test]
    fn flip_5_rotates_90_counter_clockwise() {
        let rotated = apply_orientation(numbered(), 5);
        assert_eq!((rotated.width, rotated.height), (2, 3));
        assert_eq!(reds(&rotated), [3, 6, 2, 5, 1, 4]);
    }

    #[test]
    fn flips_1_and_2_mirror() {
        assert_eq!(reds(&apply_orientation(numbered(), 1)), [3, 2, 1, 6, 5, 4]);
        assert_eq!(reds(&apply_orientation(numbered(), 2)), [4, 5, 6, 1, 2, 3]);
    }

    #[test]
    fn rgb8_gains_opaque_alpha() {
        let image =
            decode_preview(&Preview::Rgb8 { width: 2, height: 1, data: vec![1, 2, 3, 4, 5, 6] })
                .unwrap();
        assert_eq!(image.pixels, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn rgb8_of_the_wrong_length_is_an_error() {
        let preview = Preview::Rgb8 { width: 2, height: 2, data: vec![0; 5] };
        assert!(matches!(decode_preview(&preview), Err(Error::Decode(_))));
    }

    #[test]
    fn shrinking_keeps_the_aspect_ratio() {
        let image = RgbaImage { width: 4000, height: 3000, pixels: vec![200; 4000 * 3000 * 4] };
        let small = fit_long_edge(image, 512).unwrap();
        assert_eq!((small.width, small.height), (512, 384));
        assert_eq!(small.pixels.len(), 512 * 384 * 4);
        assert_eq!(px(&small, 100, 100), [200, 200, 200, 200]);
    }

    #[test]
    fn a_thin_image_keeps_at_least_one_pixel() {
        let image = RgbaImage { width: 5000, height: 2, pixels: vec![0; 5000 * 2 * 4] };
        assert_eq!(fit_long_edge(image, 512).unwrap().height, 1);
    }

    #[test]
    fn small_images_are_not_enlarged() {
        assert_eq!(fit_long_edge(numbered(), 512).unwrap(), numbered());
    }

    #[test]
    fn jpeg_round_trip_keeps_size_and_colour() {
        let image = RgbaImage { width: 64, height: 48, pixels: [200, 40, 40, 255].repeat(64 * 48) };
        let decoded = decode_jpeg(&encode_jpeg(&image).unwrap(), true).unwrap();
        assert_eq!((decoded.width, decoded.height), (64, 48));
        let [red, green, blue, alpha] = px(&decoded, 30, 20);
        assert!(red.abs_diff(200) < 6 && green.abs_diff(40) < 6 && blue.abs_diff(40) < 6);
        assert_eq!(alpha, 255);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode_jpeg(b"not a jpeg", false).is_err());
        assert!(decode_jpeg(&[], true).is_err());
    }
}
