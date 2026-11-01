use anyhow::{Context, Result, bail};
use std::{fs::File, io::BufWriter, path::Path};
use tiff::encoder::{TiffEncoder, colortype::RGB16};
use unlight_color::{delta_e_2000, srgb_to_lab};

type PixelMap = Box<dyn Fn(usize, usize) -> (usize, usize)>;

/// Applies LibRaw's flip code to interleaved RGB. Returns pixels and the new size.
pub fn orient(pixels: Vec<f32>, width: u32, height: u32, flip: i32) -> (Vec<f32>, u32, u32) {
    let (w, h) = (width as usize, height as usize);
    let (out_w, out_h, map): (usize, usize, PixelMap) = match flip {
        3 => (w, h, Box::new(move |x, y| (w - 1 - x, h - 1 - y))),
        5 => (h, w, Box::new(move |x, y| (y, w - 1 - x))),
        6 => (h, w, Box::new(move |x, y| (h - 1 - y, x))),
        _ => return (pixels, width, height),
    };
    let mut rotated = vec![0.0; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let (tx, ty) = map(x, y);
            let source = (y * w + x) * 3;
            let target = (ty * out_w + tx) * 3;
            rotated[target..target + 3].copy_from_slice(&pixels[source..source + 3]);
        }
    }
    (rotated, out_w as u32, out_h as u32)
}

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
    fn no_flip_leaves_the_image_alone() {
        let pixels = numbered(3, 2);
        assert_eq!(orient(pixels.clone(), 3, 2, 0), (pixels, 3, 2));
    }

    #[test]
    fn flip_3_is_a_half_turn() {
        let (rotated, w, h) = orient(numbered(3, 2), 3, 2, 3);
        assert_eq!((w, h), (3, 2));
        assert_eq!(at(&rotated, 3, 0, 0), 5.0);
        assert_eq!(at(&rotated, 3, 2, 1), 0.0);
    }

    #[test]
    fn flip_6_turns_clockwise() {
        let (rotated, w, h) = orient(numbered(3, 2), 3, 2, 6);
        assert_eq!((w, h), (2, 3));
        assert_eq!(at(&rotated, 2, 1, 0), 0.0);
        assert_eq!(at(&rotated, 2, 0, 2), 5.0);
    }

    #[test]
    fn flip_5_turns_counter_clockwise() {
        let (rotated, w, h) = orient(numbered(3, 2), 3, 2, 5);
        assert_eq!((w, h), (2, 3));
        assert_eq!(at(&rotated, 2, 0, 2), 0.0);
        assert_eq!(at(&rotated, 2, 1, 0), 5.0);
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
