//! Multi-band blending in linear light (Burt & Adelson 1983). Fine detail changes over at the
//! seam and broad tone changes over slowly, so a seam neither ghosts nor shows as a step. The
//! running sums live on the canvas and each frame is folded in and dropped, which is what keeps
//! memory tied to the size of the panorama rather than the number of frames in it.

use crate::{Image, Plane, Rgb};
use rayon::prelude::*;

const KERNEL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
/// Coverage below this is rounding left over from empty pixels.
const HOLDS_DATA: f32 = 1e-9;

fn level_size(size: usize, level: usize) -> usize {
    size.div_ceil(1 << level).max(1)
}

/// Halves premultiplied colour and coverage together. Positions outside the image count as
/// empty, which leaves averages over covered pixels only, so a frame's edge doesn't fade.
fn reduced(pixels: &[[f32; 4]], width: usize, height: usize) -> (Vec<[f32; 4]>, usize, usize) {
    let (out_width, out_height) = (width.div_ceil(2).max(1), height.div_ceil(2).max(1));
    let mut across = vec![[0.0f32; 4]; out_width * height];
    across.par_chunks_mut(out_width).enumerate().for_each(|(y, row)| {
        let line = &pixels[y * width..(y + 1) * width];
        for (x, out) in row.iter_mut().enumerate() {
            let middle = x as isize * 2;
            for (offset, weight) in KERNEL.iter().enumerate() {
                let at = middle + offset as isize - 2;
                if (0..width as isize).contains(&at) {
                    let value = line[at as usize];
                    (0..4).for_each(|c| out[c] += value[c] * weight);
                }
            }
        }
    });
    let mut down = vec![[0.0f32; 4]; out_width * out_height];
    down.par_chunks_mut(out_width).enumerate().for_each(|(y, row)| {
        let middle = y as isize * 2;
        for (offset, weight) in KERNEL.iter().enumerate() {
            let at = middle + offset as isize - 2;
            if (0..height as isize).contains(&at) {
                let line = &across[at as usize * out_width..(at as usize + 1) * out_width];
                for (out, value) in row.iter_mut().zip(line) {
                    (0..4).for_each(|c| out[c] += value[c] * weight);
                }
            }
        }
    });
    (down, out_width, out_height)
}

/// Doubles a level to the size of the one below it. Nodes marked invalid hold no data, and the
/// result is averaged over those that do. Without that, a frame's outer edge would pick up the
/// empty canvas beyond it and darken.
fn expanded(coarse: &[Rgb], valid: &[bool], size: [usize; 2], out_width: usize, out_height: usize) -> Vec<Rgb> {
    let [width, height] = size;
    let taps = |position: usize| -> [(isize, f32); 3] {
        let node = (position / 2) as isize;
        if position.is_multiple_of(2) {
            [(node - 1, 0.125), (node, 0.75), (node + 1, 0.125)]
        } else {
            [(node, 0.5), (node + 1, 0.5), (0, 0.0)]
        }
    };
    let mut fine = vec![[0.0f32; 3]; out_width * out_height];
    fine.par_chunks_mut(out_width).enumerate().for_each(|(y, row)| {
        let rows = taps(y);
        for (x, out) in row.iter_mut().enumerate() {
            let columns = taps(x);
            let mut total = 0.0f32;
            let mut sum = [0.0f32; 3];
            for &(node_y, weight_y) in &rows {
                if weight_y == 0.0 || node_y < 0 || node_y as usize >= height {
                    continue;
                }
                for &(node_x, weight_x) in &columns {
                    if weight_x == 0.0 || node_x < 0 || node_x as usize >= width {
                        continue;
                    }
                    let at = node_y as usize * width + node_x as usize;
                    if valid[at] {
                        let weight = weight_x * weight_y;
                        (0..3).for_each(|c| sum[c] += coarse[at][c] * weight);
                        total += weight;
                    }
                }
            }
            if total > 0.0 {
                *out = sum.map(|value| value / total);
            }
        }
    });
    fine
}

fn validity(level: &[[f32; 4]]) -> Vec<bool> {
    level.par_iter().map(|value| value[3] > HOLDS_DATA).collect()
}

fn unpremultiplied(level: &[[f32; 4]]) -> Vec<Rgb> {
    level
        .par_iter()
        .map(|&[r, g, b, weight]| if weight > HOLDS_DATA { [r / weight, g / weight, b / weight] } else { [0.0; 3] })
        .collect()
}

/// Halves a mask. Unlike colour its edge should spread, because that's what widens the
/// transition at each coarser band.
fn reduced_mask(mask: &[f32], width: usize, height: usize) -> (Vec<f32>, usize, usize) {
    let as_colour: Vec<[f32; 4]> = mask.par_iter().map(|&value| [0.0, 0.0, 0.0, value]).collect();
    let (down, out_width, out_height) = reduced(&as_colour, width, height);
    (down.into_par_iter().map(|value| value[3]).collect(), out_width, out_height)
}

/// The running blend of a panorama.
pub struct Blender {
    width: usize,
    height: usize,
    /// Weighted bands and total weight, finest first.
    bands: Vec<Vec<[f32; 4]>>,
}

impl Blender {
    /// Asks for `levels` bands, or as many as the canvas has room for.
    pub fn new(width: usize, height: usize, levels: usize) -> Self {
        let room = (usize::BITS - width.min(height).max(1).leading_zeros()) as usize;
        let levels = levels.min(room.saturating_sub(3)).max(1);
        let bands = (0..levels)
            .map(|level| vec![[0.0f32; 4]; level_size(width, level) * level_size(height, level)])
            .collect();
        Self { width, height, bands }
    }

    /// Tile origins have to fall on this grid, so that every level of a tile lines up with the
    /// same level of the canvas.
    pub fn grid(&self) -> usize {
        1 << (self.bands.len() - 1)
    }

    /// Folds in one frame's tile. `covered` is where the tile has real pixels, and `mask` how
    /// much of the blend it should have, both from 0 to 1.
    pub fn add(&mut self, origin: [usize; 2], image: &Image, covered: &Plane, mask: &Plane) {
        let grid = self.grid();
        assert!(origin[0].is_multiple_of(grid) && origin[1].is_multiple_of(grid), "tile {origin:?} off a {grid} grid");
        let levels = self.bands.len();
        let (width, height) = (image.width, image.height);
        let mut gaussian: Vec<[f32; 4]> = image
            .pixels
            .par_iter()
            .zip(&covered.values)
            .map(|(pixel, &cover)| [pixel[0] * cover, pixel[1] * cover, pixel[2] * cover, cover])
            .collect();
        let mut weights = mask.values.clone();
        let (mut level_width, mut level_height) = (width, height);
        for level in 0..levels {
            let last = level + 1 == levels;
            let colour = unpremultiplied(&gaussian);
            let (next, next_width, next_height) = if last {
                (Vec::new(), 0, 0)
            } else {
                reduced(&gaussian, level_width, level_height)
            };
            let expansion = (!last).then(|| {
                let coarse = unpremultiplied(&next);
                expanded(&coarse, &validity(&next), [next_width, next_height], level_width, level_height)
            });
            let (canvas_width, canvas_height) = (level_size(self.width, level), level_size(self.height, level));
            let (left, top) = (origin[0] >> level, origin[1] >> level);
            let band = &mut self.bands[level];
            for y in 0..level_height.min(canvas_height.saturating_sub(top)) {
                let start = (top + y) * canvas_width + left;
                let row_width = level_width.min(canvas_width.saturating_sub(left));
                let row = &mut band[start..start + row_width];
                let source = y * level_width;
                row.iter_mut().enumerate().for_each(|(x, total)| {
                    let at = source + x;
                    let weight = weights[at];
                    if weight <= 0.0 || gaussian[at][3] <= 0.0 {
                        return;
                    }
                    for c in 0..3 {
                        let lower = expansion.as_ref().map_or(0.0, |lower| lower[at][c]);
                        total[c] += (colour[at][c] - lower) * weight;
                    }
                    total[3] += weight;
                });
            }
            if last {
                break;
            }
            let (next_weights, _, _) = reduced_mask(&weights, level_width, level_height);
            weights = next_weights;
            gaussian = next;
            level_width = next_width;
            level_height = next_height;
        }
    }

    /// Collapses the bands. Returns the picture and which of its pixels anything covered.
    pub fn finish(self) -> (Image, Vec<bool>) {
        let levels = self.bands.len();
        let (width, height) = (self.width, self.height);
        let mut result = unpremultiplied(&self.bands[levels - 1]);
        for level in (0..levels - 1).rev() {
            let coarse_size = [level_size(width, level + 1), level_size(height, level + 1)];
            let (fine_width, fine_height) = (level_size(width, level), level_size(height, level));
            let valid = validity(&self.bands[level + 1]);
            let mut fine = expanded(&result, &valid, coarse_size, fine_width, fine_height);
            let band = &self.bands[level];
            fine.par_iter_mut().zip(band).for_each(|(pixel, total)| {
                if total[3] > HOLDS_DATA {
                    (0..3).for_each(|c| pixel[c] += total[c] / total[3]);
                }
            });
            result = fine;
        }
        let covered: Vec<bool> = self.bands[0].par_iter().map(|total| total[3] > 0.0).collect();
        let pixels = result
            .into_par_iter()
            .zip(&covered)
            .map(|(pixel, &covered)| if covered { pixel.map(|value| value.max(0.0)) } else { [0.0; 3] })
            .collect();
        (Image { width, height, pixels }, covered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(width: usize, height: usize, value: impl Fn(usize, usize) -> Rgb + Sync) -> Image {
        Image::from_fn(width, height, value)
    }

    fn step_masks(width: usize, height: usize, split: usize) -> (Plane, Plane) {
        (
            Plane::from_fn(width, height, |x, _| f32::from(u8::from(x < split))),
            Plane::from_fn(width, height, |x, _| f32::from(u8::from(x >= split))),
        )
    }

    #[test]
    fn one_frame_comes_back_unchanged() {
        let image = tile(96, 64, |x, y| [x as f32 * 0.01, y as f32 * 0.02, ((x + y) % 7) as f32 * 0.1]);
        let mut blender = Blender::new(96, 64, 5);
        let everywhere = Plane::filled(96, 64, 1.0);
        blender.add([0, 0], &image, &everywhere, &everywhere);
        let (result, covered) = blender.finish();
        assert!(covered.iter().all(|value| *value));
        for (found, expected) in result.pixels.iter().zip(&image.pixels) {
            for c in 0..3 {
                assert!((found[c] - expected[c]).abs() < 1e-3, "{found:?} {expected:?}");
            }
        }
    }

    #[test]
    fn a_frame_edge_doesnt_darken_towards_the_empty_canvas() {
        let image = tile(64, 64, |_, _| [0.5; 3]);
        let mut blender = Blender::new(128, 128, 5);
        let everywhere = Plane::filled(64, 64, 1.0);
        blender.add([32, 32], &image, &everywhere, &everywhere);
        let (result, covered) = blender.finish();
        assert!(!covered[0]);
        for y in 32..96 {
            for x in 32..96 {
                let value = result.at(x, y)[0];
                assert!((value - 0.5).abs() < 1e-3, "{value} at {x},{y}");
            }
        }
        assert_eq!(result.at(5, 5), [0.0; 3]);
    }

    #[test]
    fn a_step_in_exposure_is_spread_over_the_broad_bands() {
        let (width, height) = (256, 64);
        let (left_mask, right_mask) = step_masks(width, height, 128);
        let everywhere = Plane::filled(width, height, 1.0);
        let dark = tile(width, height, |_, _| [0.4; 3]);
        let bright = tile(width, height, |_, _| [0.6; 3]);
        let mut blender = Blender::new(width, height, 6);
        blender.add([0, 0], &dark, &everywhere, &left_mask);
        blender.add([0, 0], &bright, &everywhere, &right_mask);
        let (result, _) = blender.finish();
        let row: Vec<f32> = (0..width).map(|x| result.at(x, 32)[0]).collect();
        let largest = row.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max);
        assert!(largest < 0.03, "largest step {largest} of a 0.2 change");
        assert!((row[2] - 0.4).abs() < 0.01 && (row[width - 3] - 0.6).abs() < 0.01, "{} {}", row[2], row[width - 3]);
    }

    #[test]
    fn fine_detail_stays_sharp_at_the_seam() {
        let (width, height) = (128, 64);
        let (left_mask, right_mask) = step_masks(width, height, 64);
        let everywhere = Plane::filled(width, height, 1.0);
        let stripes = |x: usize, _: usize| [if x.is_multiple_of(2) { 0.2 } else { 0.8 }; 3];
        let mut blender = Blender::new(width, height, 5);
        blender.add([0, 0], &tile(width, height, stripes), &everywhere, &left_mask);
        blender.add([0, 0], &tile(width, height, stripes), &everywhere, &right_mask);
        let (result, _) = blender.finish();
        for x in 60..68 {
            let expected = stripes(x, 0)[0];
            assert!((result.at(x, 32)[0] - expected).abs() < 1e-3, "at {x}");
        }
    }

    #[test]
    fn linear_blending_never_goes_negative() {
        let (width, height) = (128, 32);
        let (left_mask, right_mask) = step_masks(width, height, 64);
        let everywhere = Plane::filled(width, height, 1.0);
        let dark = tile(width, height, |_, _| [0.001; 3]);
        let hot = tile(width, height, |_, _| [32.0; 3]);
        let mut blender = Blender::new(width, height, 5);
        blender.add([0, 0], &dark, &everywhere, &left_mask);
        blender.add([0, 0], &hot, &everywhere, &right_mask);
        let (result, _) = blender.finish();
        assert!(result.pixels.iter().flatten().all(|value| *value >= 0.0));
    }

    #[test]
    fn levels_are_limited_by_the_canvas() {
        let blender = Blender::new(32, 16, 10);
        assert!(blender.bands.len() < 10);
        assert_eq!(blender.grid(), 1 << (blender.bands.len() - 1));
    }

    #[test]
    fn tiles_keep_their_place_on_the_canvas() {
        let grid = Blender::new(128, 128, 4).grid();
        let image = tile(32, 32, |_, _| [1.0; 3]);
        let mut blender = Blender::new(128, 128, 4);
        let everywhere = Plane::filled(32, 32, 1.0);
        blender.add([grid * 4, grid * 2], &image, &everywhere, &everywhere);
        let (result, covered) = blender.finish();
        assert!(covered[(grid * 2) * 128 + grid * 4]);
        assert!((result.at(grid * 4 + 4, grid * 2 + 4)[0] - 1.0).abs() < 1e-3);
    }
}
