//! Measures how far the interactive preview strays from a full export, which is what each tool
//! is gated on. The two take different paths through the pipeline at different resolutions, so
//! nothing but a measurement can show they still agree.

use crate::{
    error::Result,
    export::{ExportOptions, export},
    gpu::GpuContext,
    pipelines::{Engine, Precision},
    session::{Session, ViewRequest},
};
use rayon::prelude::*;
use unlight_color::{delta_e_2000, srgb_to_lab};
use unlight_raw::RawImage;
use unlight_recipe::Recipe;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewDifference {
    pub mean: f64,
    pub p95: f64,
    pub max: f64,
}

fn decode(value: f32) -> f32 {
    if value <= 0.040_45 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

fn encode(value: f32) -> f32 {
    if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 }
}

/// Box-filters gamma-encoded interleaved RGB down to `width`×`height`, averaging in linear
/// light, which is what a downscale of the exported file ought to look like.
fn downscale(pixels: &[f32], size: [u32; 2], width: u32, height: u32) -> Vec<f32> {
    let (source_width, source_height) = (size[0] as usize, size[1] as usize);
    let (width, height) = (width as usize, height as usize);
    let columns: Vec<Vec<(usize, f32)>> = (0..width).map(|x| coverage(x, source_width, width)).collect();
    let mut output = vec![0.0f32; width * height * 3];
    output.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
        let rows = coverage(y, source_height, height);
        for (pixel, column) in row.as_chunks_mut::<3>().0.iter_mut().zip(&columns) {
            let mut sum = [0.0f32; 3];
            let mut area = 0.0;
            for &(source_y, height_share) in &rows {
                for &(source_x, width_share) in column {
                    let start = (source_y * source_width + source_x) * 3;
                    let share = height_share * width_share;
                    area += share;
                    for (total, value) in sum.iter_mut().zip(&pixels[start..start + 3]) {
                        *total += decode(*value) * share;
                    }
                }
            }
            for (value, total) in pixel.iter_mut().zip(sum) {
                *value = encode(total / area);
            }
        }
    });
    output
}

/// The source pixels under one output pixel, with how much of each it covers. Rounding these
/// to whole pixels instead would shift the result by up to half a source pixel.
fn coverage(index: usize, from: usize, to: usize) -> Vec<(usize, f32)> {
    let ratio = from as f64 / to as f64;
    let (start, end) = (index as f64 * ratio, (index + 1) as f64 * ratio);
    (start.floor() as usize..(end.ceil() as usize).min(from))
        .map(|source| {
            let share = end.min((source + 1) as f64) - start.max(source as f64);
            (source, share.max(0.0) as f32)
        })
        .collect()
}

/// ΔE2000 over matching interleaved sRGB images, leaving out a margin where the downscale and
/// the view sample the frame edge differently.
fn difference(a: &[f32], b: &[f32], width: usize, height: usize) -> PreviewDifference {
    const MARGIN: usize = 4;
    let mut values: Vec<f64> = (MARGIN..height.saturating_sub(MARGIN))
        .into_par_iter()
        .flat_map_iter(|y| {
            (MARGIN..width.saturating_sub(MARGIN)).map(move |x| {
                let start = (y * width + x) * 3;
                let pixel = |image: &[f32]| [image[start], image[start + 1], image[start + 2]];
                delta_e_2000(srgb_to_lab(pixel(a)), srgb_to_lab(pixel(b)))
            })
        })
        .collect();
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return PreviewDifference { mean: 0.0, p95: 0.0, max: 0.0 };
    }
    PreviewDifference {
        mean: values.iter().sum::<f64>() / values.len() as f64,
        p95: values[(values.len() - 1) * 95 / 100],
        max: values[values.len() - 1],
    }
}

/// A photo rendered both ways at the preview's size, as interleaved sRGB.
pub struct PreviewPair {
    pub width: u32,
    pub height: u32,
    pub shown: Vec<f32>,
    pub exported: Vec<f32>,
}

impl PreviewPair {
    pub fn difference(&self) -> PreviewDifference {
        difference(&self.shown, &self.exported, self.width as usize, self.height as usize)
    }
}

/// Renders a photo both ways and compares them at the preview's size.
pub fn preview_difference(raw: &RawImage, recipe: &Recipe, view_width: u32) -> Result<PreviewDifference> {
    Ok(preview_pair(raw, recipe, view_width)?.difference())
}

/// Renders a photo as the preview and as a scaled export.
pub fn preview_pair(raw: &RawImage, recipe: &Recipe, view_width: u32) -> Result<PreviewPair> {
    let exported = export(raw, recipe, &ExportOptions::default())?;
    let engine = Engine::new(GpuContext::new()?, Precision::F16)?;
    let mut session = Session::open(engine, raw, recipe)?;
    let [output_width, output_height] = session.output_size(recipe);
    let view_height = (u64::from(view_width) * u64::from(output_height) / u64::from(output_width)).max(1) as u32;
    let target = session.view_target(view_width, view_height);
    session.render(recipe, &ViewRequest::default(), &target, None)?;
    let view = session.read_view(&target)?;
    let shown: Vec<f32> = view
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|texel| [texel[0], texel[1], texel[2]])
        .map(|value| f32::from(value) / 255.0)
        .collect();
    let scaled = downscale(&exported.pixels, [exported.width, exported.height], view_width, view_height);
    Ok(PreviewPair { width: view_width, height: view_height, shown, exported: scaled })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscaling_averages_in_linear_light() {
        let pixels = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let scaled = downscale(&pixels, [2, 1], 1, 1);
        assert!((scaled[0] - encode(0.5)).abs() < 1e-5, "{scaled:?}");
    }

    #[test]
    fn a_fractional_downscale_keeps_a_ramp_in_place() {
        let linear = |position: f32| position / 7.0;
        let pixels: Vec<f32> = (0..7).flat_map(|x| [encode(linear(x as f32 + 0.5)); 3]).collect();
        let scaled = downscale(&pixels, [7, 1], 3, 1);
        for (x, pixel) in scaled.as_chunks::<3>().0.iter().enumerate() {
            let expected = linear((x as f32 + 0.5) * 7.0 / 3.0);
            // A stepped ramp lands within this. Whole-pixel spans miss by up to five times it.
            assert!((decode(pixel[0]) - expected).abs() < 0.01, "{x}: {pixel:?}");
        }
    }

    #[test]
    fn identical_images_do_not_differ() {
        let image = vec![0.4f32; 64 * 64 * 3];
        assert_eq!(difference(&image, &image, 64, 64).max, 0.0);
    }

    #[test]
    fn a_visible_shift_registers() {
        let (a, b) = (vec![0.4f32; 64 * 64 * 3], vec![0.5f32; 64 * 64 * 3]);
        assert!(difference(&a, &b, 64, 64).mean > 5.0);
    }
}
