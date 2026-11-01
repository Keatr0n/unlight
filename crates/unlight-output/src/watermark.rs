use crate::resize::resample;
use crate::{Error, Image, Result};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use fast_image_resize::PixelType;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use unlight_color::matrix::{self, Mat3};
use unlight_color::output::{OutputSpace, TransferFunction};

const MEASURING_PX: f32 = 100.0;
const RGBA: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkSource {
    /// No font ships with Unlight, so text needs a TTF or OTF file. `color` is sRGB.
    Text {
        text: String,
        font: PathBuf,
        color: [u8; 3],
    },
    /// A PNG, treated as sRGB.
    Image { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WatermarkSettings {
    pub source: WatermarkSource,
    pub anchor: Anchor,
    /// 0 is invisible and 1 is fully opaque.
    pub opacity: f32,
    /// Width of the watermark as a fraction of the image width.
    pub scale: f32,
    /// Distance from the image edge as a fraction of its short edge.
    pub inset: f32,
}

impl WatermarkSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        let valid = (0.0..=1.0).contains(&self.opacity)
            && self.scale > 0.0
            && self.scale <= 1.0
            && (0.0..0.5).contains(&self.inset);
        valid.then_some(()).ok_or_else(|| {
            Error::Settings(format!(
                "watermark opacity {}, scale {} or inset {} is out of range",
                self.opacity, self.scale, self.inset
            ))
        })
    }
}

/// Linear-light RGBA in sRGB primaries, with alpha premultiplied.
struct Overlay {
    width: u32,
    height: u32,
    pixels: Vec<f32>,
}

fn watermark_error(path: &Path, detail: impl std::fmt::Display) -> Error {
    Error::Watermark(format!("{}: {detail}", path.display()))
}

fn measured_width(font: &FontVec, text: &str) -> f32 {
    let scaled = font.as_scaled(PxScale::from(MEASURING_PX));
    let mut width = 0.0;
    let mut previous = None;
    for c in text.chars() {
        let glyph = scaled.glyph_id(c);
        if let Some(previous) = previous {
            width += scaled.kern(previous, glyph);
        }
        width += scaled.h_advance(glyph);
        previous = Some(glyph);
    }
    width
}

fn render_coverage(font: &FontVec, text: &str, px: f32) -> (u32, u32, Vec<f32>) {
    let scaled = font.as_scaled(PxScale::from(px));
    let width = measured_width(font, text) * px / MEASURING_PX;
    let (width, height) = (
        width.ceil().max(1.0) as u32,
        scaled.height().ceil().max(1.0) as u32,
    );
    let mut coverage = vec![0.0f32; width as usize * height as usize];

    let mut caret = 0.0;
    let mut previous = None;
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(previous) = previous {
            caret += scaled.kern(previous, id);
        }
        let glyph = id.with_scale_and_position(px, point(caret, scaled.ascent()));
        caret += scaled.h_advance(id);
        previous = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|x, y, amount| {
            let column = bounds.min.x as i64 + i64::from(x);
            let row = bounds.min.y as i64 + i64::from(y);
            if (0..i64::from(width)).contains(&column) && (0..i64::from(height)).contains(&row) {
                let cell = &mut coverage[row as usize * width as usize + column as usize];
                *cell = cell.max(amount);
            }
        });
    }
    (width, height, coverage)
}

fn text_overlay(
    text: &str,
    font_path: &Path,
    color: [u8; 3],
    target_width: u32,
) -> Result<Overlay> {
    let bytes = fs::read(font_path).map_err(|error| watermark_error(font_path, error))?;
    let font = FontVec::try_from_vec(bytes).map_err(|error| watermark_error(font_path, error))?;
    let natural_width = measured_width(&font, text);
    if natural_width <= 0.0 {
        return Err(Error::Watermark("the text has no visible width".to_owned()));
    }
    let px = MEASURING_PX * target_width as f32 / natural_width;
    let (width, height, coverage) = render_coverage(&font, text, px);
    let linear = color.map(|channel| TransferFunction::Srgb.decode(f32::from(channel) / 255.0));
    let pixels = coverage
        .iter()
        .flat_map(|&alpha| {
            [
                linear[0] * alpha,
                linear[1] * alpha,
                linear[2] * alpha,
                alpha,
            ]
        })
        .collect();
    Ok(Overlay {
        width,
        height,
        pixels,
    })
}

fn decode_png(path: &Path) -> Result<Overlay> {
    let file = fs::File::open(path).map_err(|error| watermark_error(path, error))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|error| watermark_error(path, error))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| watermark_error(path, "image too large"))?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| watermark_error(path, error))?;
    let samples = &buffer[..info.buffer_size()];

    let srgb = |value: u8| TransferFunction::Srgb.decode(f32::from(value) / 255.0);
    let premultiplied = |[r, g, b, a]: [u8; 4]| {
        let alpha = f32::from(a) / 255.0;
        [srgb(r) * alpha, srgb(g) * alpha, srgb(b) * alpha, alpha]
    };
    let pixels: Vec<f32> = match info.color_type {
        png::ColorType::Rgba => samples
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| premultiplied(*p))
            .collect(),
        png::ColorType::Rgb => samples
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| premultiplied([r, g, b, 255]))
            .collect(),
        png::ColorType::GrayscaleAlpha => samples
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[gray, alpha]| premultiplied([gray, gray, gray, alpha]))
            .collect(),
        png::ColorType::Grayscale => samples
            .iter()
            .flat_map(|&v| premultiplied([v, v, v, 255]))
            .collect(),
        png::ColorType::Indexed => return Err(watermark_error(path, "unexpected palette output")),
    };
    Ok(Overlay {
        width: info.width,
        height: info.height,
        pixels,
    })
}

fn image_overlay(path: &Path, target_width: u32) -> Result<Overlay> {
    let decoded = decode_png(path)?;
    let scale = f64::from(target_width) / f64::from(decoded.width);
    let height = (f64::from(decoded.height) * scale).round().max(1.0) as u32;
    let mut pixels = resample(
        &decoded.pixels,
        [decoded.width, decoded.height],
        [target_width, height],
        PixelType::F32x4,
        None,
    )?;
    pixels.par_chunks_exact_mut(RGBA).for_each(|pixel| {
        let alpha = pixel[3].clamp(0.0, 1.0);
        pixel[3] = alpha;
        pixel[..3]
            .iter_mut()
            .for_each(|sample| *sample = sample.clamp(0.0, alpha));
    });
    Ok(Overlay {
        width: target_width,
        height,
        pixels,
    })
}

fn position(anchor: Anchor, image: [u32; 2], overlay: [u32; 2], inset: u32) -> (u32, u32) {
    let along = |start: bool, end: bool, size: u32, overlay: u32| {
        let free = size.saturating_sub(overlay);
        if start {
            inset.min(free)
        } else if end {
            free.saturating_sub(inset)
        } else {
            free / 2
        }
    };
    let left = matches!(anchor, Anchor::TopLeft | Anchor::Left | Anchor::BottomLeft);
    let right = matches!(
        anchor,
        Anchor::TopRight | Anchor::Right | Anchor::BottomRight
    );
    let top = matches!(anchor, Anchor::TopLeft | Anchor::Top | Anchor::TopRight);
    let bottom = matches!(
        anchor,
        Anchor::BottomLeft | Anchor::Bottom | Anchor::BottomRight
    );
    (
        along(left, right, image[0], overlay[0]),
        along(top, bottom, image[1], overlay[1]),
    )
}

fn srgb_to(space: OutputSpace) -> Mat3 {
    let to_prophoto = matrix::inverse(&OutputSpace::Srgb.matrix_from_prophoto())
        .expect("sRGB matrix is invertible");
    matrix::mul(&space.matrix_from_prophoto(), &to_prophoto)
}

/// Blends in linear light, so soft overlay edges don't darken.
fn composite(image: &mut Image<'_>, overlay: &Overlay, origin: (u32, u32), opacity: f32) {
    let space = image.space();
    let transfer = space.transfer();
    let to_space = srgb_to(space).map(|row| row.map(|value| value as f32));
    let row_samples = image.row_samples();
    let (left, top) = (origin.0 as usize, origin.1 as usize);
    let overlay_width = overlay.width as usize;

    let target_rows = image
        .pixels_mut()
        .par_chunks_exact_mut(row_samples)
        .skip(top);
    let source_rows = overlay.pixels.par_chunks_exact(overlay_width * RGBA);
    target_rows.zip(source_rows).for_each(|(row, source)| {
        let (pixels, _) = row[left * 3..].as_chunks_mut::<3>();
        let (stamps, _) = source.as_chunks::<RGBA>();
        for (pixel, stamp) in pixels.iter_mut().zip(stamps) {
            let alpha = stamp[3] * opacity;
            if alpha <= 0.0 {
                continue;
            }
            for (channel, sample) in pixel.iter_mut().enumerate() {
                let [r, g, b] = to_space[channel];
                let added = (r * stamp[0] + g * stamp[1] + b * stamp[2]) * opacity;
                *sample = transfer.encode(added + transfer.decode(*sample) * (1.0 - alpha));
            }
        }
    });
}

pub(crate) fn apply(image: &mut Image<'_>, settings: &WatermarkSettings) -> Result<()> {
    let target_width = ((image.width() as f32 * settings.scale).round() as u32).max(1);
    let overlay = match &settings.source {
        WatermarkSource::Text { text, font, color } => {
            text_overlay(text, font, *color, target_width)?
        }
        WatermarkSource::Image { path } => image_overlay(path, target_width)?,
    };
    let size = [image.width(), image.height()];
    let inset = (size[0].min(size[1]) as f32 * settings.inset).round() as u32;
    let origin = position(
        settings.anchor,
        size,
        [overlay.width, overlay.height],
        inset,
    );
    composite(image, &overlay, origin, settings.opacity);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufWriter;

    const BLACK: f32 = 0.0;

    fn black(width: u32, height: u32, space: OutputSpace) -> Image<'static> {
        Image::new(
            width,
            height,
            space,
            vec![BLACK; width as usize * height as usize * 3],
        )
        .unwrap()
    }

    fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
        let file = BufWriter::new(fs::File::create(path).unwrap());
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(rgba)
            .unwrap();
    }

    fn solid_logo(directory: &Path, rgba: [u8; 4]) -> PathBuf {
        let path = directory.join("logo.png");
        write_png(&path, 8, 8, &rgba.repeat(64));
        path
    }

    fn settings(source: WatermarkSource) -> WatermarkSettings {
        WatermarkSettings {
            source,
            anchor: Anchor::BottomRight,
            opacity: 1.0,
            scale: 0.5,
            inset: 0.0,
        }
    }

    fn pixel(image: &Image<'_>, x: usize, y: usize) -> [f32; 3] {
        let index = (y * image.width() as usize + x) * 3;
        [
            image.pixels()[index],
            image.pixels()[index + 1],
            image.pixels()[index + 2],
        ]
    }

    #[test]
    fn every_anchor_lands_where_it_says() {
        let at = |anchor| position(anchor, [100, 80], [20, 10], 5);
        assert_eq!(at(Anchor::TopLeft), (5, 5));
        assert_eq!(at(Anchor::Top), (40, 5));
        assert_eq!(at(Anchor::TopRight), (75, 5));
        assert_eq!(at(Anchor::Left), (5, 35));
        assert_eq!(at(Anchor::Center), (40, 35));
        assert_eq!(at(Anchor::Right), (75, 35));
        assert_eq!(at(Anchor::BottomLeft), (5, 65));
        assert_eq!(at(Anchor::Bottom), (40, 65));
        assert_eq!(at(Anchor::BottomRight), (75, 65));
    }

    #[test]
    fn an_oversized_overlay_stays_on_the_image() {
        assert_eq!(
            position(Anchor::BottomRight, [100, 80], [100, 200], 5),
            (0, 0)
        );
        assert_eq!(position(Anchor::TopLeft, [100, 80], [100, 200], 5), (0, 0));
    }

    #[test]
    fn opaque_white_logo_replaces_the_pixels_under_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = solid_logo(directory.path(), [255, 255, 255, 255]);
        let mut image = black(64, 64, OutputSpace::Srgb);
        apply(&mut image, &settings(WatermarkSource::Image { path })).unwrap();
        assert!(
            pixel(&image, 60, 60).iter().all(|v| (v - 1.0).abs() < 1e-3),
            "{:?}",
            pixel(&image, 60, 60)
        );
        assert_eq!(pixel(&image, 10, 10), [BLACK; 3]);
    }

    #[test]
    fn opacity_blends_in_linear_light() {
        let directory = tempfile::tempdir().unwrap();
        let path = solid_logo(directory.path(), [255, 255, 255, 255]);
        let mut image = black(64, 64, OutputSpace::Srgb);
        let half = WatermarkSettings {
            opacity: 0.5,
            ..settings(WatermarkSource::Image { path })
        };
        apply(&mut image, &half).unwrap();
        let expected = TransferFunction::Srgb.encode(0.5);
        assert!((pixel(&image, 60, 60)[0] - expected).abs() < 1e-3);
    }

    #[test]
    fn alpha_in_the_logo_counts() {
        let directory = tempfile::tempdir().unwrap();
        let path = solid_logo(directory.path(), [255, 255, 255, 0]);
        let mut image = black(64, 64, OutputSpace::Srgb);
        apply(&mut image, &settings(WatermarkSource::Image { path })).unwrap();
        assert!(image.pixels().iter().all(|value| *value == BLACK));
    }

    #[test]
    fn zero_opacity_changes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let path = solid_logo(directory.path(), [255, 255, 255, 255]);
        let mut image = black(64, 64, OutputSpace::Srgb);
        let hidden = WatermarkSettings {
            opacity: 0.0,
            ..settings(WatermarkSource::Image { path })
        };
        apply(&mut image, &hidden).unwrap();
        assert!(image.pixels().iter().all(|value| *value == BLACK));
    }

    #[test]
    fn scale_sets_the_width_on_the_image() {
        let directory = tempfile::tempdir().unwrap();
        let path = solid_logo(directory.path(), [255, 255, 255, 255]);
        let mut image = black(100, 100, OutputSpace::Srgb);
        apply(&mut image, &settings(WatermarkSource::Image { path })).unwrap();
        assert!(pixel(&image, 55, 99)[0] > 0.9);
        assert_eq!(pixel(&image, 45, 99)[0], BLACK);
    }

    #[test]
    fn logo_colours_are_converted_to_the_output_space() {
        let directory = tempfile::tempdir().unwrap();
        let path = solid_logo(directory.path(), [255, 0, 0, 255]);
        let mut srgb = black(64, 64, OutputSpace::Srgb);
        let mut wide = black(64, 64, OutputSpace::ProPhoto);
        let source = WatermarkSource::Image { path };
        apply(&mut srgb, &settings(source.clone())).unwrap();
        apply(&mut wide, &settings(source)).unwrap();

        assert!((pixel(&srgb, 60, 60)[0] - 1.0).abs() < 1e-3);
        let expected = matrix::apply(&srgb_to(OutputSpace::ProPhoto), &[1.0, 0.0, 0.0]);
        let transfer = OutputSpace::ProPhoto.transfer();
        for (expected, actual) in expected.iter().zip(pixel(&wide, 60, 60)) {
            let wanted = transfer.encode(*expected as f32);
            assert!((actual - wanted).abs() < 2e-3, "{actual} vs {wanted}");
        }
        assert!(
            pixel(&wide, 60, 60)[0] < 0.9,
            "pure sRGB red isn't the edge of ProPhoto"
        );
    }

    #[test]
    fn missing_files_are_reported_with_their_path() {
        let source = WatermarkSource::Image {
            path: PathBuf::from("/nonexistent/logo.png"),
        };
        let mut image = black(16, 16, OutputSpace::Srgb);
        let error = apply(&mut image, &settings(source)).unwrap_err();
        assert!(
            error.to_string().contains("/nonexistent/logo.png"),
            "{error}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_font_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("font.ttf");
        fs::write(&path, b"not a font").unwrap();
        let source = WatermarkSource::Text {
            text: "x".into(),
            font: path,
            color: [255; 3],
        };
        let mut image = black(16, 16, OutputSpace::Srgb);
        assert!(matches!(
            apply(&mut image, &settings(source)),
            Err(Error::Watermark(_))
        ));
    }

    #[test]
    fn out_of_range_settings_are_refused() {
        let base = settings(WatermarkSource::Image {
            path: PathBuf::new(),
        });
        assert!(
            WatermarkSettings {
                opacity: 1.5,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            WatermarkSettings {
                scale: 0.0,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            WatermarkSettings {
                inset: 0.5,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            WatermarkSettings {
                opacity: f32::NAN,
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(base.validate().is_ok());
    }

    #[test]
    fn text_renders_in_the_requested_colour_and_width() {
        let directory = tempfile::tempdir().unwrap();
        let font = directory.path().join("font.ttf");
        fs::write(&font, epaint_default_fonts::UBUNTU_LIGHT).unwrap();
        let source = WatermarkSource::Text {
            text: "Unlight".into(),
            font,
            color: [255, 255, 255],
        };
        let mut image = black(400, 200, OutputSpace::Srgb);
        apply(&mut image, &settings(source)).unwrap();

        let mut min_x = usize::MAX;
        let mut max_x = 0;
        for y in 0..200 {
            for x in 0..400 {
                if pixel(&image, x, y)[0] > 0.5 {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                }
            }
        }
        assert!(max_x > min_x, "nothing was drawn");
        assert!(min_x >= 200, "drew outside the right half: {min_x}");
        assert!(max_x - min_x > 150, "text spans only {} px", max_x - min_x);
    }

    #[test]
    fn text_colour_is_converted_to_the_output_space() {
        let directory = tempfile::tempdir().unwrap();
        let font = directory.path().join("font.ttf");
        fs::write(&font, epaint_default_fonts::UBUNTU_LIGHT).unwrap();
        let source = WatermarkSource::Text {
            text: "M".into(),
            font,
            color: [255, 0, 0],
        };
        let mut image = black(200, 200, OutputSpace::ProPhoto);
        apply(&mut image, &settings(source)).unwrap();
        let (pixels, _) = image.pixels().as_chunks::<3>();
        let brightest =
            pixels.iter().fold(
                [0.0f32; 3],
                |best, pixel| if pixel[0] > best[0] { *pixel } else { best },
            );
        assert!(brightest[0] > 0.5 && brightest[0] < 0.9, "{brightest:?}");
        assert!(
            brightest[1] > 0.1,
            "sRGB red has green in ProPhoto: {brightest:?}"
        );
    }

    #[test]
    fn empty_text_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let font = directory.path().join("font.ttf");
        fs::write(&font, epaint_default_fonts::UBUNTU_LIGHT).unwrap();
        let source = WatermarkSource::Text {
            text: String::new(),
            font,
            color: [255; 3],
        };
        let mut image = black(16, 16, OutputSpace::Srgb);
        assert!(matches!(
            apply(&mut image, &settings(source)),
            Err(Error::Watermark(_))
        ));
    }
}
