//! Interpolation of colour at sub-pixel positions. Merging warps a frame exactly once, so that
//! one resampling decides how sharp the result is.

use crate::image::{Image, Rgb};

const LANCZOS_LOBES: isize = 3;

#[inline]
fn sinc(value: f32) -> f32 {
    if value.abs() < 1e-5 {
        return 1.0;
    }
    let scaled = value * std::f32::consts::PI;
    scaled.sin() / scaled
}

#[inline]
fn lanczos(value: f32) -> f32 {
    if value.abs() >= LANCZOS_LOBES as f32 {
        return 0.0;
    }
    sinc(value) * sinc(value / LANCZOS_LOBES as f32)
}

/// Weights for the six samples around a fractional offset.
#[inline]
fn lanczos_weights(fraction: f32) -> [f32; 6] {
    let mut weights = std::array::from_fn(|index| lanczos(index as f32 - 2.0 - fraction));
    let total: f32 = weights.iter().sum();
    weights.iter_mut().for_each(|weight| *weight /= total);
    weights
}

/// Lanczos-3 of one channel, with coordinates held to the edge of the image. Its negative lobes
/// are limited to the range of the four nearest pixels, because a linear image has edges many
/// stops tall and an unlimited lobe would ring far below black beside them.
#[inline]
pub(crate) fn lanczos_channel(image: &Image, x: f32, y: f32, channel: usize) -> f32 {
    let (left, top) = (x.floor(), y.floor());
    let (along, down) = (x - left, y - top);
    let (left, top) = (left as isize, top as isize);
    let (width, height) = (image.width as isize, image.height as isize);
    let (across, upward) = (lanczos_weights(along), lanczos_weights(down));
    let mut sum = 0.0;
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for (row, row_weight) in upward.iter().enumerate() {
        let at_y = (top + row as isize - 2).clamp(0, height - 1);
        let line = &image.pixels[(at_y * width) as usize..((at_y + 1) * width) as usize];
        let mut line_sum = 0.0;
        for (column, column_weight) in across.iter().enumerate() {
            let at_x = (left + column as isize - 2).clamp(0, width - 1);
            let value = line[at_x as usize][channel];
            line_sum += value * column_weight;
            if (2..=3).contains(&row) && (2..=3).contains(&column) {
                low = low.min(value);
                high = high.max(value);
            }
        }
        sum += line_sum * row_weight;
    }
    sum.clamp(low, high)
}

#[inline]
pub(crate) fn lanczos_rgb(image: &Image, x: f32, y: f32) -> Rgb {
    std::array::from_fn(|channel| lanczos_channel(image, x, y, channel))
}

/// Bilinear colour, with coordinates held to the edge of the image.
#[inline]
pub(crate) fn bilinear_rgb(image: &Image, x: f32, y: f32) -> Rgb {
    let x = x.clamp(0.0, (image.width - 1) as f32);
    let y = y.clamp(0.0, (image.height - 1) as f32);
    let (left, top) = (x as usize, y as usize);
    let (right, bottom) = ((left + 1).min(image.width - 1), (top + 1).min(image.height - 1));
    let (along, down) = (x - left as f32, y - top as f32);
    let (a, b) = (image.at(left, top), image.at(right, top));
    let (c, d) = (image.at(left, bottom), image.at(right, bottom));
    std::array::from_fn(|channel| {
        let upper = a[channel] * (1.0 - along) + b[channel] * along;
        let lower = c[channel] * (1.0 - along) + d[channel] * along;
        upper * (1.0 - down) + lower * down
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Image {
        Image::from_fn(32, 32, |x, y| [x as f32, y as f32, (x + y) as f32])
    }

    #[test]
    fn whole_coordinates_return_the_pixel() {
        let image = ramp();
        assert_eq!(lanczos_rgb(&image, 10.0, 20.0), image.at(10, 20));
    }

    #[test]
    fn a_ramp_stays_a_ramp_between_pixels() {
        let [x, y, sum] = lanczos_rgb(&ramp(), 10.25, 20.75);
        assert!((x - 10.25).abs() < 0.03, "{x}");
        assert!((y - 20.75).abs() < 0.03, "{y}");
        assert!((sum - 31.0).abs() < 0.06, "{sum}");
    }

    #[test]
    fn a_tall_edge_doesnt_ring_below_black() {
        let image = Image::from_fn(16, 16, |x, _| [if x < 8 { 0.0 } else { 64.0 }; 3]);
        for tenth in 0..50 {
            let x = 5.0 + tenth as f32 * 0.1;
            let [value, ..] = lanczos_rgb(&image, x, 8.0);
            assert!((0.0..=64.0).contains(&value), "{value} at {x}");
        }
    }

    #[test]
    fn positions_past_the_edge_are_held() {
        let image = ramp();
        assert_eq!(bilinear_rgb(&image, -3.0, 40.0), [0.0, 31.0, 31.0]);
        assert!(lanczos_channel(&image, -3.0, 5.0, 0).abs() < 1e-3);
    }
}
