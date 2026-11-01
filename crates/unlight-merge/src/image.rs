//! Plain float images. Colour is three `f32`s rather than RGBA, which saves a quarter of the
//! memory on frames that are 61 MP each.

use rayon::prelude::*;

pub type Rgb = [f32; 3];

/// The weights demosaiced frames are compared with. Green carries most of the signal, and the
/// same mix is what the engine's noise and tone stages call luma.
pub(crate) const LUMA: Rgb = [0.25, 0.5, 0.25];

pub(crate) fn luma(pixel: Rgb) -> f32 {
    pixel[0] * LUMA[0] + pixel[1] * LUMA[1] + pixel[2] * LUMA[2]
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Rgb>,
}

/// One channel, for analysis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub values: Vec<f32>,
}

impl Image {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, pixels: vec![[0.0; 3]; width * height] }
    }

    pub fn from_fn(width: usize, height: usize, pixel: impl Fn(usize, usize) -> Rgb + Sync) -> Self {
        let mut image = Self::new(width, height);
        image.pixels.par_chunks_mut(width.max(1)).enumerate().for_each(|(y, row)| {
            row.iter_mut().enumerate().for_each(|(x, value)| *value = pixel(x, y));
        });
        image
    }

    /// Drops the alpha channel of what the engine reads back.
    pub fn from_rgba(width: usize, height: usize, pixels: &[[f32; 4]]) -> Self {
        Self { width, height, pixels: pixels.par_iter().map(|&[r, g, b, _]| [r, g, b]).collect() }
    }

    pub fn at(&self, x: usize, y: usize) -> Rgb {
        self.pixels[y * self.width + x]
    }

    pub fn plane(&self, value: impl Fn(Rgb) -> f32 + Sync) -> Plane {
        Plane {
            width: self.width,
            height: self.height,
            values: self.pixels.par_iter().map(|&pixel| value(pixel)).collect(),
        }
    }

    pub fn luma(&self) -> Plane {
        self.plane(luma)
    }

    /// Averages each `factor`-sized square to one pixel. Whatever doesn't fill a whole square
    /// at the right and bottom edges is dropped, which keeps the grid exactly `factor` apart.
    pub fn reduced(&self, factor: usize) -> Self {
        if factor <= 1 {
            return self.clone();
        }
        let (width, height) = ((self.width / factor).max(1), (self.height / factor).max(1));
        let scale = 1.0 / (factor * factor) as f32;
        Self::from_fn(width, height, |x, y| {
            let mut sum = [0.0f32; 3];
            for row in y * factor..((y + 1) * factor).min(self.height) {
                let start = row * self.width + x * factor;
                let end = (start + factor).min((row + 1) * self.width);
                for pixel in &self.pixels[start..end] {
                    (0..3).for_each(|channel| sum[channel] += pixel[channel]);
                }
            }
            sum.map(|value| value * scale)
        })
    }

    /// The smallest whole reduction that brings the long edge to `max_edge` or less.
    pub fn fitted(&self, max_edge: usize) -> Self {
        self.reduced(self.width.max(self.height).div_ceil(max_edge.max(1)))
    }
}

impl Plane {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, values: vec![0.0; width * height] }
    }

    pub fn filled(width: usize, height: usize, value: f32) -> Self {
        Self { width, height, values: vec![value; width * height] }
    }

    pub fn from_fn(width: usize, height: usize, value: impl Fn(usize, usize) -> f32 + Sync) -> Self {
        let mut plane = Self::new(width, height);
        plane.values.par_chunks_mut(width.max(1)).enumerate().for_each(|(y, row)| {
            row.iter_mut().enumerate().for_each(|(x, out)| *out = value(x, y));
        });
        plane
    }

    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.values[y * self.width + x]
    }

    /// Bilinear lookup at pixel coordinates, clamped to the edge.
    pub fn bilinear(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.width - 1) as f32);
        let y = y.clamp(0.0, (self.height - 1) as f32);
        let (left, top) = (x as usize, y as usize);
        let (right, bottom) = ((left + 1).min(self.width - 1), (top + 1).min(self.height - 1));
        let (along, down) = (x - left as f32, y - top as f32);
        let upper = self.at(left, top) * (1.0 - along) + self.at(right, top) * along;
        let lower = self.at(left, bottom) * (1.0 - along) + self.at(right, bottom) * along;
        upper * (1.0 - down) + lower * down
    }

    /// Averages each 2×2 square to one value.
    pub fn halved(&self) -> Self {
        let (width, height) = ((self.width / 2).max(1), (self.height / 2).max(1));
        Self::from_fn(width, height, |x, y| {
            let (left, top) = (x * 2, y * 2);
            let (right, bottom) = ((left + 1).min(self.width - 1), (top + 1).min(self.height - 1));
            (self.at(left, top) + self.at(right, top) + self.at(left, bottom) + self.at(right, bottom)) * 0.25
        })
    }

    /// A Gaussian blur that holds the edge value outside the image.
    pub fn blurred(&self, sigma: f32) -> Self {
        if sigma <= 0.0 {
            return self.clone();
        }
        let radius = (sigma * 3.0).ceil() as isize;
        let kernel: Vec<f32> = (-radius..=radius)
            .map(|offset| (-0.5 * (offset as f32 / sigma).powi(2)).exp())
            .collect();
        let total: f32 = kernel.iter().sum();
        let kernel: Vec<f32> = kernel.iter().map(|weight| weight / total).collect();
        let (width, height) = (self.width, self.height);
        let across = Self::from_fn(width, height, |x, y| {
            let row = &self.values[y * width..(y + 1) * width];
            kernel.iter().enumerate().fold(0.0, |sum, (index, weight)| {
                let at = (x as isize + index as isize - radius).clamp(0, width as isize - 1);
                sum + row[at as usize] * weight
            })
        });
        Self::from_fn(width, height, |x, y| {
            kernel.iter().enumerate().fold(0.0, |sum, (index, weight)| {
                let at = (y as isize + index as isize - radius).clamp(0, height as isize - 1);
                sum + across.values[at as usize * width + x] * weight
            })
        })
    }

    /// The smallest value within `radius` pixels, along rows and then columns, so a square.
    pub fn eroded(&self, radius: usize) -> Self {
        if radius == 0 {
            return self.clone();
        }
        let (width, height) = (self.width, self.height);
        let across = Self::from_fn(width, height, |x, y| {
            let (left, right) = (x.saturating_sub(radius), (x + radius + 1).min(width));
            self.values[y * width + left..y * width + right].iter().copied().fold(f32::MAX, f32::min)
        });
        Self::from_fn(width, height, |x, y| {
            let (top, bottom) = (y.saturating_sub(radius), (y + radius + 1).min(height));
            (top..bottom).map(|row| across.values[row * width + x]).fold(f32::MAX, f32::min)
        })
    }
}

/// Median of a slice, which it reorders.
pub(crate) fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let middle = values.len() / 2;
    Some(*values.select_nth_unstable_by(middle, f32::total_cmp).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reducing_keeps_the_mean_of_each_block() {
        let image = Image::from_fn(4, 2, |x, y| [x as f32, y as f32, 1.0]);
        let half = image.reduced(2);
        assert_eq!((half.width, half.height), (2, 1));
        assert_eq!(half.pixels, vec![[0.5, 0.5, 1.0], [2.5, 0.5, 1.0]]);
    }

    #[test]
    fn fitting_reaches_the_requested_edge() {
        let fitted = Image::new(6000, 4000).fitted(1600);
        assert_eq!((fitted.width, fitted.height), (1500, 1000));
        let untouched = Image::new(600, 400).fitted(1600);
        assert_eq!((untouched.width, untouched.height), (600, 400));
    }

    #[test]
    fn a_blur_keeps_a_constant_and_the_mean() {
        let flat = Plane::filled(32, 32, 0.5).blurred(3.0);
        assert!(flat.values.iter().all(|value| (value - 0.5).abs() < 1e-5));
        let mut spike = Plane::new(65, 65);
        spike.values[32 * 65 + 32] = 1.0;
        let blurred = spike.blurred(2.0);
        let total: f32 = blurred.values.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "{total}");
        assert!(blurred.at(32, 32) > blurred.at(34, 32));
    }

    #[test]
    fn bilinear_lands_between_pixels_and_clamps() {
        let plane = Plane::from_fn(2, 2, |x, y| (x + 2 * y) as f32);
        assert_eq!(plane.bilinear(0.5, 0.0), 0.5);
        assert_eq!(plane.bilinear(0.5, 0.5), 1.5);
        assert_eq!(plane.bilinear(5.0, 5.0), 3.0);
        assert_eq!(plane.bilinear(-5.0, -5.0), 0.0);
    }

    #[test]
    fn erosion_grows_a_dark_pixel_into_a_square() {
        let mut plane = Plane::filled(9, 9, 1.0);
        plane.values[4 * 9 + 4] = 0.0;
        let eroded = plane.eroded(2);
        assert_eq!(eroded.at(2, 2), 0.0);
        assert_eq!(eroded.at(6, 6), 0.0);
        assert_eq!(eroded.at(1, 4), 1.0);
    }

    #[test]
    fn median_of_nothing_is_nothing() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
    }
}
