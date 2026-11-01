use anyhow::{Context, Result, bail};
use std::{fs::File, io::BufWriter, path::Path};
use tiff::encoder::{TiffEncoder, colortype::RGB16};
use unlight_color::{delta_e_2000, srgb_to_lab};

/// Copies a rectangle out of interleaved RGB.
pub fn crop(pixels: &[f32], width: u32, [x, y, crop_width, crop_height]: [u32; 4]) -> Vec<f32> {
    let (width, x, crop_width) = (width as usize, x as usize, crop_width as usize);
    pixels
        .chunks_exact(width * 3)
        .skip(y as usize)
        .take(crop_height as usize)
        .flat_map(|row| row[x * 3..(x + crop_width) * 3].iter().copied())
        .collect()
}

pub fn save(path: &Path, pixels: &[f32], width: u32, height: u32) -> Result<()> {
    match path.extension().and_then(|ext| ext.to_str()).map(str::to_lowercase).as_deref() {
        Some("tif" | "tiff") => save_tiff(path, pixels, width, height),
        Some("png") => {
            let rgb: Vec<u8> = pixels.iter().map(|value| (value * 255.0 + 0.5) as u8).collect();
            save_png(path, &rgb, png::ColorType::Rgb, width, height)
        }
        other => bail!("can't write {other:?}, use .tiff or .png"),
    }
}

fn save_tiff(path: &Path, pixels: &[f32], width: u32, height: u32) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = TiffEncoder::new(BufWriter::new(file))?;
    let data: Vec<u16> = pixels.iter().map(|value| (value * 65535.0 + 0.5) as u16).collect();
    encoder.write_image::<RGB16>(width, height, &data)?;
    Ok(())
}

pub fn save_rgba8(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    save_png(path, rgba, png::ColorType::Rgba, width, height)
}

fn save_png(path: &Path, data: &[u8], color: png::ColorType, width: u32, height: u32) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(data)?;
    Ok(())
}

pub struct Difference {
    pub mean: f64,
    pub p99: f64,
    pub max: f64,
}

/// ΔE2000 between two sRGB images, sampled on a sparse grid to keep it quick.
pub fn difference(a: &[f32], b: &[f32]) -> Difference {
    const STEP: usize = 97;
    let mut values: Vec<f64> = a
        .as_chunks::<3>()
        .0
        .iter()
        .zip(b.as_chunks::<3>().0)
        .step_by(STEP)
        .map(|(x, y)| delta_e_2000(srgb_to_lab(*x), srgb_to_lab(*y)))
        .collect();
    values.sort_by(f64::total_cmp);
    Difference {
        mean: values.iter().sum::<f64>() / values.len() as f64,
        p99: values[values.len() * 99 / 100],
        max: *values.last().unwrap_or(&0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(width: usize, height: usize) -> Vec<f32> {
        (0..width * height).flat_map(|i| [i as f32; 3]).collect()
    }

    fn at(pixels: &[f32], width: usize, x: usize, y: usize) -> f32 {
        pixels[(y * width + x) * 3]
    }

    #[test]
    fn crop_keeps_the_requested_rectangle() {
        let cropped = crop(&numbered(3, 2), 3, [1, 0, 2, 2]);
        assert_eq!(cropped.len(), 12);
        assert_eq!(at(&cropped, 2, 0, 0), 1.0);
        assert_eq!(at(&cropped, 2, 1, 1), 5.0);
    }

    #[test]
    fn identical_images_have_no_difference() {
        let pixels = vec![0.25; 300];
        let stats = difference(&pixels, &pixels);
        assert_eq!((stats.mean, stats.max), (0.0, 0.0));
    }
}
