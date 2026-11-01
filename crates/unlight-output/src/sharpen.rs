use crate::Image;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

const KERNEL_REACH_IN_SIGMAS: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharpenMedia {
    Screen,
    Matte,
    Glossy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharpenAmount {
    Low,
    Standard,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSharpening {
    pub media: SharpenMedia,
    pub amount: SharpenAmount,
}

impl OutputSharpening {
    /// Print loses fine detail, matte paper most of all, so print settings use a wider
    /// radius and a stronger mask than a screen needs.
    fn unsharp_mask(self) -> UnsharpMask {
        let (sigma, standard_amount) = match self.media {
            SharpenMedia::Screen => (0.5, 0.6),
            SharpenMedia::Glossy => (0.8, 0.9),
            SharpenMedia::Matte => (1.0, 1.2),
        };
        let strength = match self.amount {
            SharpenAmount::Low => 0.6,
            SharpenAmount::Standard => 1.0,
            SharpenAmount::High => 1.5,
        };
        UnsharpMask {
            sigma,
            amount: standard_amount * strength,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UnsharpMask {
    sigma: f32,
    amount: f32,
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let reach = (sigma * KERNEL_REACH_IN_SIGMAS).ceil() as i32;
    let weights: Vec<f32> = (-reach..=reach)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    weights.into_iter().map(|weight| weight / total).collect()
}

fn blur_rows(source: &[f32], target: &mut [f32], width: usize, kernel: &[f32]) {
    let reach = kernel.len() / 2;
    target
        .par_chunks_mut(width)
        .zip(source.par_chunks(width))
        .for_each(|(out, row)| {
            for (x, value) in out.iter_mut().enumerate() {
                *value = kernel
                    .iter()
                    .enumerate()
                    .map(|(i, weight)| weight * row[(x + i).saturating_sub(reach).min(width - 1)])
                    .sum();
            }
        });
}

fn blur_columns(source: &[f32], target: &mut [f32], width: usize, kernel: &[f32]) {
    let height = source.len() / width;
    let reach = kernel.len() / 2;
    target
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, out)| {
            out.fill(0.0);
            for (i, weight) in kernel.iter().enumerate() {
                let line = (y + i).saturating_sub(reach).min(height - 1);
                let row = &source[line * width..(line + 1) * width];
                for (value, sample) in out.iter_mut().zip(row) {
                    *value += weight * sample;
                }
            }
        });
}

/// Unsharp mask on luma, which keeps edges free of colour fringes. The pixels are still
/// gamma-encoded here, so the mask works on steps the eye sees as equal.
pub(crate) fn sharpen(image: &mut Image<'_>, settings: OutputSharpening) {
    let UnsharpMask { sigma, amount } = settings.unsharp_mask();
    let width = image.width() as usize;
    let [red, green, blue] = image
        .space()
        .luminance_weights()
        .map(|weight| weight as f32);

    let luma: Vec<f32> = image
        .pixels()
        .par_chunks_exact(3)
        .map(|pixel| red * pixel[0] + green * pixel[1] + blue * pixel[2])
        .collect();
    let kernel = gaussian_kernel(sigma);
    let mut horizontal = vec![0.0; luma.len()];
    let mut blurred = vec![0.0; luma.len()];
    blur_rows(&luma, &mut horizontal, width, &kernel);
    blur_columns(&horizontal, &mut blurred, width, &kernel);

    image
        .pixels_mut()
        .par_chunks_exact_mut(3)
        .zip(luma.par_iter().zip(blurred.par_iter()))
        .for_each(|(pixel, (original, smooth))| {
            let boost = amount * (original - smooth);
            for sample in pixel {
                *sample += boost;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_color::output::OutputSpace;

    const MEDIA: [SharpenMedia; 3] = [
        SharpenMedia::Screen,
        SharpenMedia::Glossy,
        SharpenMedia::Matte,
    ];
    const AMOUNTS: [SharpenAmount; 3] = [
        SharpenAmount::Low,
        SharpenAmount::Standard,
        SharpenAmount::High,
    ];

    fn step_edge(width: u32, height: u32) -> Image<'static> {
        let mut pixels = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                pixels.extend([if x < width / 2 { 0.3 } else { 0.6 }; 3]);
            }
        }
        Image::new(width, height, OutputSpace::Srgb, pixels).unwrap()
    }

    fn overshoot(settings: OutputSharpening) -> f32 {
        let mut image = step_edge(32, 16);
        sharpen(&mut image, settings);
        let row = &image.pixels()[8 * 32 * 3..9 * 32 * 3];
        row.iter().copied().fold(f32::MIN, f32::max) - 0.6
    }

    #[test]
    fn stronger_settings_sharpen_more() {
        for media in MEDIA {
            let levels = AMOUNTS.map(|amount| overshoot(OutputSharpening { media, amount }));
            assert!(levels[0] > 0.0, "{media:?} did nothing");
            assert!(
                levels[0] < levels[1] && levels[1] < levels[2],
                "{media:?}: {levels:?}"
            );
        }
    }

    #[test]
    fn matte_needs_more_than_glossy_which_needs_more_than_screen() {
        let at = |media| {
            OutputSharpening {
                media,
                amount: SharpenAmount::Standard,
            }
            .unsharp_mask()
        };
        let [screen, glossy, matte] = MEDIA.map(at);
        assert!(screen.sigma < glossy.sigma && glossy.sigma < matte.sigma);
        assert!(screen.amount < glossy.amount && glossy.amount < matte.amount);
    }

    #[test]
    fn flat_areas_stay_flat() {
        let mut image = Image::new(32, 32, OutputSpace::Srgb, vec![0.42; 32 * 32 * 3]).unwrap();
        sharpen(
            &mut image,
            OutputSharpening {
                media: SharpenMedia::Matte,
                amount: SharpenAmount::High,
            },
        );
        assert!(
            image
                .pixels()
                .iter()
                .all(|value| (value - 0.42).abs() < 1e-5)
        );
    }

    #[test]
    fn edges_are_pushed_apart_on_both_sides() {
        let mut image = step_edge(32, 16);
        let settings = OutputSharpening {
            media: SharpenMedia::Screen,
            amount: SharpenAmount::High,
        };
        sharpen(&mut image, settings);
        let row = &image.pixels()[8 * 32 * 3..9 * 32 * 3];
        assert!(
            row[15 * 3] < 0.3 && row[16 * 3] > 0.6,
            "{} {}",
            row[15 * 3],
            row[16 * 3]
        );
    }

    #[test]
    fn colour_is_left_alone() {
        let pixels: Vec<f32> = (0..16 * 16)
            .flat_map(|i| {
                let shade = if i % 16 < 8 { 0.2 } else { 0.5 };
                [shade + 0.2, shade, shade - 0.1]
            })
            .collect();
        let mut image = Image::new(16, 16, OutputSpace::Srgb, pixels.clone()).unwrap();
        sharpen(
            &mut image,
            OutputSharpening {
                media: SharpenMedia::Matte,
                amount: SharpenAmount::High,
            },
        );
        let (after_pixels, _) = image.pixels().as_chunks::<3>();
        for (before, after) in pixels.as_chunks::<3>().0.iter().zip(after_pixels) {
            let before_chroma = before[0] - before[2];
            let after_chroma = after[0] - after[2];
            assert!((before_chroma - after_chroma).abs() < 1e-5);
        }
    }

    #[test]
    fn kernels_are_normalised_and_symmetric() {
        for sigma in [0.5, 0.8, 1.0] {
            let kernel = gaussian_kernel(sigma);
            assert!((kernel.iter().sum::<f32>() - 1.0).abs() < 1e-6);
            assert!(kernel.len() % 2 == 1);
            assert_eq!(kernel[0], kernel[kernel.len() - 1]);
        }
    }

    #[test]
    fn single_pixel_images_survive() {
        let mut image = Image::new(1, 1, OutputSpace::Srgb, vec![0.5; 3]).unwrap();
        sharpen(
            &mut image,
            OutputSharpening {
                media: SharpenMedia::Screen,
                amount: SharpenAmount::Low,
            },
        );
        assert!((image.pixels()[0] - 0.5).abs() < 1e-6);
    }
}
