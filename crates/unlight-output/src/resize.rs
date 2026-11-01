use crate::{Error, Image, Result};
use bytemuck::{cast_slice, cast_slice_mut};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Inside the box, keeping the aspect ratio.
    #[default]
    Contain,
    /// Fills the box exactly, centre-cropping what overflows.
    Cover,
    /// Fills the box exactly, ignoring the aspect ratio.
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeMode {
    LongEdge(u32),
    ShortEdge(u32),
    Megapixels(f64),
    Percent(f64),
    Exact { width: u32, height: u32, fit: Fit },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResizeSettings {
    pub mode: ResizeMode,
    pub allow_upscale: bool,
}

impl ResizeSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        let valid = match self.mode {
            ResizeMode::LongEdge(edge) | ResizeMode::ShortEdge(edge) => edge > 0,
            ResizeMode::Megapixels(amount) | ResizeMode::Percent(amount) => {
                amount.is_finite() && amount > 0.0
            }
            ResizeMode::Exact { width, height, .. } => width > 0 && height > 0,
        };
        valid
            .then_some(())
            .ok_or_else(|| Error::Settings(format!("bad resize {:?}", self.mode)))
    }
}

/// Far past any real export, and a settings file can't make the allocation abort the process.
const MAX_OUTPUT_PIXELS: u64 = 1 << 31;

/// Source rectangle in pixels: left, top, width, height.
type Crop = [f64; 4];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plan {
    pub width: u32,
    pub height: u32,
    crop: Option<Crop>,
}

fn scaled(edge: f64, scale: f64) -> u32 {
    (edge * scale).round().clamp(1.0, f64::from(u32::MAX)) as u32
}

fn uniform(source: [f64; 2], scale: f64, allow_upscale: bool) -> Plan {
    let scale = if allow_upscale { scale } else { scale.min(1.0) };
    Plan {
        width: scaled(source[0], scale),
        height: scaled(source[1], scale),
        crop: None,
    }
}

fn cover(source: [f64; 2], target: [f64; 2], allow_upscale: bool) -> Plan {
    let scale = (target[0] / source[0]).max(target[1] / source[1]);
    let limit = if allow_upscale {
        1.0
    } else {
        (1.0 / scale).min(1.0)
    };
    let crop_width = target[0] / scale;
    let crop_height = target[1] / scale;
    Plan {
        width: scaled(target[0], limit),
        height: scaled(target[1], limit),
        crop: Some([
            (source[0] - crop_width) / 2.0,
            (source[1] - crop_height) / 2.0,
            crop_width,
            crop_height,
        ]),
    }
}

fn stretch(source: [f64; 2], target: [f64; 2], allow_upscale: bool) -> Plan {
    let edge = |from: f64, to: f64| if allow_upscale { to } else { to.min(from) };
    Plan {
        width: scaled(edge(source[0], target[0]), 1.0),
        height: scaled(edge(source[1], target[1]), 1.0),
        crop: None,
    }
}

/// The size to resize to, or `None` when the image can stay as it is.
pub(crate) fn plan(width: u32, height: u32, settings: &ResizeSettings) -> Option<Plan> {
    let source = [f64::from(width), f64::from(height)];
    let long = source[0].max(source[1]);
    let short = source[0].min(source[1]);
    let allow_upscale = settings.allow_upscale;
    let plan = match settings.mode {
        ResizeMode::LongEdge(edge) => uniform(source, f64::from(edge) / long, allow_upscale),
        ResizeMode::ShortEdge(edge) => uniform(source, f64::from(edge) / short, allow_upscale),
        ResizeMode::Megapixels(amount) => uniform(
            source,
            (amount * 1e6 / (long * short)).sqrt(),
            allow_upscale,
        ),
        ResizeMode::Percent(percent) => uniform(source, percent / 100.0, allow_upscale),
        ResizeMode::Exact {
            width: w,
            height: h,
            fit,
        } => {
            let target = [f64::from(w), f64::from(h)];
            match fit {
                Fit::Contain => {
                    let scale = (target[0] / source[0]).min(target[1] / source[1]);
                    uniform(source, scale, allow_upscale)
                }
                Fit::Cover => cover(source, target, allow_upscale),
                Fit::Stretch => stretch(source, target, allow_upscale),
            }
        }
    };
    let unchanged =
        plan.width == width && plan.height == height && !has_visible_crop(&plan, source);
    (!unchanged).then_some(plan)
}

fn has_visible_crop(plan: &Plan, source: [f64; 2]) -> bool {
    plan.crop.is_some_and(|[_, _, width, height]| {
        (width - source[0]).abs() >= 1.0 || (height - source[1]).abs() >= 1.0
    })
}

/// Lanczos3 on interleaved floats of the given layout.
pub(crate) fn resample(
    source: &[f32],
    source_size: [u32; 2],
    target_size: [u32; 2],
    pixel_type: PixelType,
    crop: Option<Crop>,
) -> Result<Vec<f32>> {
    let channels = pixel_type.size() / size_of::<f32>();
    let mut target = vec![0.0f32; target_size[0] as usize * target_size[1] as usize * channels];
    let source_view = images::ImageRef::new(
        source_size[0],
        source_size[1],
        cast_slice(source),
        pixel_type,
    )
    .map_err(|error| Error::Resize(error.to_string()))?;
    let mut target_view = images::Image::from_slice_u8(
        target_size[0],
        target_size[1],
        cast_slice_mut(&mut target),
        pixel_type,
    )
    .map_err(|error| Error::Resize(error.to_string()))?;
    let mut options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .use_alpha(false);
    if let Some([left, top, width, height]) = crop {
        options = options.crop(left, top, width, height);
    }
    Resizer::new()
        .resize(&source_view, &mut target_view, &options)
        .map_err(|error| Error::Resize(error.to_string()))?;
    Ok(target)
}

/// Resizes in linear light so bright and dark detail average the way light does.
pub(crate) fn resize(image: &Image<'_>, plan: &Plan) -> Result<Image<'static>> {
    let (width, height) = (plan.width, plan.height);
    if u64::from(width) * u64::from(height) > MAX_OUTPUT_PIXELS {
        return Err(Error::Resize(format!("{width}×{height} is too large")));
    }
    let transfer = image.space().transfer();
    let linear: Vec<f32> = image
        .pixels()
        .par_iter()
        .map(|&value| transfer.decode(value))
        .collect();
    let mut resized = resample(
        &linear,
        [image.width(), image.height()],
        [plan.width, plan.height],
        PixelType::F32x3,
        plan.crop,
    )?;
    resized
        .par_iter_mut()
        .for_each(|value| *value = transfer.encode(*value));
    Image::new(plan.width, plan.height, image.space(), resized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_color::output::{OutputSpace, TransferFunction};

    fn settings(mode: ResizeMode) -> ResizeSettings {
        ResizeSettings {
            mode,
            allow_upscale: false,
        }
    }

    fn size(width: u32, height: u32, mode: ResizeMode) -> Option<(u32, u32)> {
        plan(width, height, &settings(mode)).map(|plan| (plan.width, plan.height))
    }

    fn upscaled(width: u32, height: u32, mode: ResizeMode) -> Option<(u32, u32)> {
        let settings = ResizeSettings {
            mode,
            allow_upscale: true,
        };
        plan(width, height, &settings).map(|plan| (plan.width, plan.height))
    }

    fn gray(
        width: u32,
        height: u32,
        space: OutputSpace,
        shade: impl Fn(u32, u32) -> f32,
    ) -> Image<'static> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height {
            for x in 0..width {
                pixels.extend([shade(x, y); 3]);
            }
        }
        Image::new(width, height, space, pixels).unwrap()
    }

    #[test]
    fn long_and_short_edge_keep_the_aspect_ratio() {
        assert_eq!(
            size(6000, 4000, ResizeMode::LongEdge(1500)),
            Some((1500, 1000))
        );
        assert_eq!(
            size(4000, 6000, ResizeMode::LongEdge(1500)),
            Some((1000, 1500))
        );
        assert_eq!(
            size(6000, 4000, ResizeMode::ShortEdge(1000)),
            Some((1500, 1000))
        );
    }

    #[test]
    fn megapixels_targets_the_pixel_count() {
        let (width, height) = size(6000, 4000, ResizeMode::Megapixels(6.0)).unwrap();
        assert_eq!((width, height), (3000, 2000));
    }

    #[test]
    fn percent_scales_both_edges() {
        assert_eq!(
            size(6000, 4000, ResizeMode::Percent(25.0)),
            Some((1500, 1000))
        );
    }

    #[test]
    fn contain_fits_inside_the_box() {
        let mode = ResizeMode::Exact {
            width: 1000,
            height: 1000,
            fit: Fit::Contain,
        };
        assert_eq!(size(6000, 4000, mode), Some((1000, 667)));
        assert_eq!(size(4000, 6000, mode), Some((667, 1000)));
    }

    #[test]
    fn cover_fills_the_box_exactly() {
        let mode = ResizeMode::Exact {
            width: 1000,
            height: 1000,
            fit: Fit::Cover,
        };
        let plan = plan(6000, 4000, &settings(mode)).unwrap();
        assert_eq!((plan.width, plan.height), (1000, 1000));
        assert_eq!(plan.crop, Some([1000.0, 0.0, 4000.0, 4000.0]));
    }

    #[test]
    fn stretch_ignores_the_aspect_ratio() {
        let mode = ResizeMode::Exact {
            width: 1000,
            height: 1000,
            fit: Fit::Stretch,
        };
        assert_eq!(size(6000, 4000, mode), Some((1000, 1000)));
    }

    #[test]
    fn never_upscales_unless_asked() {
        assert_eq!(size(800, 600, ResizeMode::LongEdge(1600)), None);
        assert_eq!(size(800, 600, ResizeMode::ShortEdge(1200)), None);
        assert_eq!(size(800, 600, ResizeMode::Megapixels(2.0)), None);
        assert_eq!(size(800, 600, ResizeMode::Percent(200.0)), None);
        let contain = ResizeMode::Exact {
            width: 1600,
            height: 1200,
            fit: Fit::Contain,
        };
        assert_eq!(size(800, 600, contain), None);
    }

    #[test]
    fn upscales_when_asked() {
        assert_eq!(
            upscaled(800, 600, ResizeMode::LongEdge(1600)),
            Some((1600, 1200))
        );
        assert_eq!(
            upscaled(800, 600, ResizeMode::Percent(200.0)),
            Some((1600, 1200))
        );
    }

    #[test]
    fn cover_without_upscaling_keeps_the_box_shape() {
        let mode = ResizeMode::Exact {
            width: 1000,
            height: 1000,
            fit: Fit::Cover,
        };
        let plan = plan(800, 600, &settings(mode)).unwrap();
        assert_eq!((plan.width, plan.height), (600, 600));
        assert_eq!(plan.crop, Some([100.0, 0.0, 600.0, 600.0]));
    }

    #[test]
    fn stretch_without_upscaling_clamps_each_edge() {
        let mode = ResizeMode::Exact {
            width: 1000,
            height: 100,
            fit: Fit::Stretch,
        };
        assert_eq!(size(800, 600, mode), Some((800, 100)));
    }

    #[test]
    fn tiny_targets_keep_at_least_one_pixel() {
        assert_eq!(size(6000, 10, ResizeMode::LongEdge(60)), Some((60, 1)));
    }

    #[test]
    fn same_size_means_no_work() {
        assert_eq!(size(800, 600, ResizeMode::LongEdge(800)), None);
    }

    #[test]
    fn rejects_zero_and_nonsense_targets() {
        assert!(settings(ResizeMode::LongEdge(0)).validate().is_err());
        assert!(settings(ResizeMode::Percent(f64::NAN)).validate().is_err());
        assert!(settings(ResizeMode::Megapixels(-1.0)).validate().is_err());
        let exact = ResizeMode::Exact {
            width: 0,
            height: 10,
            fit: Fit::Contain,
        };
        assert!(settings(exact).validate().is_err());
    }

    #[test]
    fn a_checkerboard_averages_in_linear_light() {
        for space in OutputSpace::ALL {
            let transfer = space.transfer();
            let image = gray(64, 64, space, |x, y| ((x + y) % 2) as f32);
            let plan = plan(64, 64, &settings(ResizeMode::Percent(25.0))).unwrap();
            let resized = resize(&image, &plan).unwrap();
            let expected = transfer.encode(0.5);
            let center = resized.pixels()[(8 * 16 + 8) * 3];
            assert!(
                (center - expected).abs() < 1e-3,
                "{space:?}: {center} vs {expected}"
            );
            assert!(center > 0.6, "{space:?} averaged in gamma: {center}");
        }
    }

    #[test]
    fn a_gradient_keeps_its_brightness() {
        let transfer = TransferFunction::Srgb;
        let (width, height) = (1200, 80);
        let image = gray(width, height, OutputSpace::Srgb, |x, _| {
            x as f32 / (width - 1) as f32
        });
        let plan = plan(width, height, &settings(ResizeMode::Percent(25.0))).unwrap();
        let resized = resize(&image, &plan).unwrap();

        let mean_light = |pixels: &[f32]| {
            pixels
                .iter()
                .map(|&v| f64::from(transfer.decode(v)))
                .sum::<f64>()
                / pixels.len() as f64
        };
        let before = mean_light(image.pixels());
        let after = mean_light(resized.pixels());
        assert!(
            (before - after).abs() < 2e-3,
            "mean light moved from {before} to {after}"
        );

        for x in [100usize, 150, 200, 250] {
            let index = (10 * resized.width() as usize + x) * 3;
            let expected = (x as f32 + 0.5) * 4.0 / (width - 1) as f32;
            let actual = resized.pixels()[index];
            assert!(
                (actual - expected).abs() < 2e-3,
                "x={x}: {actual} vs {expected}"
            );
        }
    }

    #[test]
    fn a_constant_image_stays_constant() {
        let image = gray(300, 200, OutputSpace::ProPhoto, |_, _| 0.37);
        let plan = plan(300, 200, &settings(ResizeMode::LongEdge(90))).unwrap();
        let resized = resize(&image, &plan).unwrap();
        for value in resized.pixels() {
            assert!((value - 0.37).abs() < 1e-4, "{value}");
        }
    }

    #[test]
    fn keeps_more_than_8_bits_of_precision() {
        let (width, height) = (4096, 16);
        let image = gray(width, height, OutputSpace::Srgb, |x, _| {
            x as f32 / 4095.0 / 16.0
        });
        let plan = plan(width, height, &settings(ResizeMode::Percent(50.0))).unwrap();
        let resized = resize(&image, &plan).unwrap();
        let row = &resized.pixels()[(4 * 2048) * 3..(5 * 2048) * 3];
        let levels: std::collections::BTreeSet<u16> = row
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| (p[0] * 65535.0).round() as u16)
            .collect();
        assert!(
            levels.len() > 2000,
            "only {} distinct 16-bit levels",
            levels.len()
        );
    }

    #[test]
    fn bright_overshoot_and_dark_undershoot_clip_on_output() {
        let image = gray(
            60,
            60,
            OutputSpace::Srgb,
            |x, _| if x < 30 { 0.0 } else { 1.0 },
        );
        let plan = plan(60, 60, &settings(ResizeMode::Percent(50.0))).unwrap();
        let resized = resize(&image, &plan).unwrap();
        assert!(
            resized
                .pixels()
                .iter()
                .all(|value| *value >= 0.0 && value.is_finite())
        );
    }

    #[test]
    fn absurd_upscales_are_refused_instead_of_aborting() {
        let image = gray(8, 8, OutputSpace::Srgb, |_, _| 0.5);
        let upscale = ResizeSettings {
            mode: ResizeMode::Percent(1e9),
            allow_upscale: true,
        };
        let error = resize(&image, &plan(8, 8, &upscale).unwrap()).unwrap_err();
        assert!(matches!(error, Error::Resize(_)), "{error}");
    }

    #[test]
    fn cropping_selects_the_centre() {
        let image = gray(300, 100, OutputSpace::Srgb, |x, _| {
            if (100..200).contains(&x) { 1.0 } else { 0.0 }
        });
        let mode = ResizeMode::Exact {
            width: 50,
            height: 50,
            fit: Fit::Cover,
        };
        let plan = plan(300, 100, &settings(mode)).unwrap();
        let resized = resize(&image, &plan).unwrap();
        let center = resized.pixels()[(25 * 50 + 25) * 3];
        assert!(center > 0.99, "{center}");
    }
}
