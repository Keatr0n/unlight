//! Compares the default render with the JPEG the camera embedded in the raw. The camera's
//! own rendering is the best reference there is for what a raw should look like when opened.

use anyhow::{Context, Result, bail};
use std::path::Path;
use unlight_color::{delta_e_2000, srgb_to_lab};
use unlight_engine::{Engine, GpuContext, Precision, Session, ViewRequest};
use unlight_raw::Preview;
use unlight_recipe::Recipe;
use zune_jpeg::JpegDecoder;

const BINS: usize = 12;

pub struct Reference {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Reference {
    /// Box-filtered down to a render's size, so detail doesn't count against it.
    pub fn resized(&self, width: u32, height: u32) -> Self {
        let (from_width, from_height) = (self.width as usize, self.height as usize);
        let edge = |index: u32, of: u32, to: usize| (index as usize * to / of as usize).min(to - 1);
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height {
            let rows = edge(y, height, from_height)..edge(y + 1, height, from_height).max(edge(y, height, from_height) + 1);
            for x in 0..width {
                let columns = edge(x, width, from_width)..edge(x + 1, width, from_width).max(edge(x, width, from_width) + 1);
                let mut sum = [0u32; 3];
                for row in rows.clone() {
                    for column in columns.clone() {
                        let start = (row * from_width + column) * 3;
                        for (total, value) in sum.iter_mut().zip(&self.rgb[start..start + 3]) {
                            *total += u32::from(*value);
                        }
                    }
                }
                let area = (rows.len() * columns.len()) as u32;
                rgb.extend(sum.map(|total| (total / area) as u8));
            }
        }
        Self { width, height, rgb }
    }
}

/// The camera's JPEG of a raw, if it has one the same shape as the photo.
pub fn reference(file: &Path, raw: &unlight_raw::RawImage) -> Option<Reference> {
    let reference = decode(unlight_raw::load_preview(file).ok()?).ok()?;
    let expected = raw.height as f32 * reference.width as f32 / raw.width as f32;
    ((expected - reference.height as f32).abs() < expected * 0.08).then_some(reference)
}

fn decode(preview: Preview) -> Result<Reference> {
    match preview {
        Preview::Rgb8 { width, height, data } => Ok(Reference { width, height, rgb: data }),
        Preview::Jpeg(bytes) => {
            let mut decoder = JpegDecoder::new(std::io::Cursor::new(&bytes));
            decoder.decode_headers().context("reading JPEG headers")?;
            let (width, height) = decoder.dimensions().context("JPEG has no dimensions")?;
            let rgb = decoder.decode().context("decoding JPEG")?;
            Ok(Reference { width: width as u32, height: height as u32, rgb })
        }
    }
}

pub fn luma(rgb: &[u8]) -> f32 {
    (0.2126 * f32::from(rgb[0]) + 0.7152 * f32::from(rgb[1]) + 0.0722 * f32::from(rgb[2])) / 255.0
}

pub fn compare(files: &[impl AsRef<Path>]) -> Result<()> {
    let context = GpuContext::new()?;
    let mut totals = [(0.0f64, 0u64); BINS];
    println!("{:<22} {:>9} {:>9} {:>7}", "file", "ours", "camera", "ΔE");
    for file in files {
        let file = file.as_ref();
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        let Ok(raw) = unlight_raw::load(file) else {
            println!("{name:<22} skipped, can't decode");
            continue;
        };
        let reference = decode(unlight_raw::load_preview(file)?)?;
        if reference.width < 64 {
            bail!("{name}: preview too small");
        }
        let scale = raw.width as f32 / reference.width as f32;
        let expected_height = raw.height as f32 / scale;
        if (expected_height - reference.height as f32).abs() > expected_height * 0.08 {
            println!("{name:<22} skipped, preview shape differs");
            continue;
        }

        let recipe = Recipe::default();
        let engine = Engine::new(context.clone(), Precision::F16)?;
        let mut session = Session::open(engine, &raw, &recipe)?;
        let target = session.view_target(reference.width, reference.height);
        session.render(&recipe, &ViewRequest::default(), &target, None)?;
        let ours = session.read_view(&target)?;

        let (mut ours_sum, mut camera_sum, mut difference_sum) = (0.0f64, 0.0f64, 0.0f64);
        let mut samples = 0u64;
        let pixels = ours.as_chunks::<4>().0.iter().zip(reference.rgb.as_chunks::<3>().0);
        for (a, b) in pixels.step_by(53) {
            let (luma_ours, luma_camera) = (luma(a), luma(b));
            ours_sum += f64::from(luma_ours);
            camera_sum += f64::from(luma_camera);
            let color = |p: &[u8]| [p[0], p[1], p[2]].map(|v| f32::from(v) / 255.0);
            difference_sum += delta_e_2000(srgb_to_lab(color(a)), srgb_to_lab(color(b)));
            let bin = ((luma_ours * BINS as f32) as usize).min(BINS - 1);
            totals[bin].0 += f64::from(luma_camera);
            totals[bin].1 += 1;
            samples += 1;
        }
        let count = samples.max(1) as f64;
        println!(
            "{name:<22} {:>9.3} {:>9.3} {:>7.2}",
            ours_sum / count,
            camera_sum / count,
            difference_sum / count
        );
    }
    println!("\nour luma → camera luma");
    for (bin, (sum, count)) in totals.iter().enumerate() {
        if *count > 0 {
            let center = (bin as f32 + 0.5) / BINS as f32;
            println!("{center:.3} → {:.3}  ({count})", sum / *count as f64);
        }
    }
    Ok(())
}
