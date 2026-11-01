//! AI Denoise on the demosaiced photo. The network was trained on gamma-encoded pictures, so
//! each tile is encoded for it and only the change it makes is taken back to linear. That leaves
//! anything the encoding had to clip, such as highlights above white, exactly as it was.

use crate::{
    cancel::CancelToken,
    download::Progress,
    error::{Error, Result},
    guided::box_mean,
    inference::LoadedModel,
    manifest::RestorationSpec,
};
use rayon::prelude::*;

/// The share of pixels allowed to clip when scaling the photo to fill the network's range.
const CLIP_SHARE: f64 = 0.005;
/// Limits how far a nearly black frame is lifted, which would only amplify its noise.
const MAX_GAIN: f32 = 16.0;
/// The network drifts colours a little as it removes noise, by up to 2 levels in 255 on a real
/// ISO 12800 frame. Noise averages to nothing in linear light, so whatever it changes across a
/// window this wide isn't noise, and leaving that out cuts the drift to under 1 level.
const COLOUR_RADIUS: usize = 32;

/// Linear RGBA, row by row, as the engine holds a demosaiced photo.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

fn encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Scales the brightest pixels to white, since raw data sits far below the range of the
/// pictures the network saw in training.
fn exposure_gain(image: &LinearImage) -> f32 {
    let mut brightest: Vec<f32> =
        image.pixels.par_iter().map(|pixel| pixel[0].max(pixel[1]).max(pixel[2])).collect();
    if brightest.is_empty() {
        return 1.0;
    }
    let rank = ((brightest.len() as f64 * (1.0 - CLIP_SHARE)) as usize).min(brightest.len() - 1);
    let (_, level, _) = brightest.select_nth_unstable_by(rank, f32::total_cmp);
    (1.0 / level.max(1.0 / MAX_GAIN)).clamp(1.0, MAX_GAIN)
}

/// Where tiles start along one side: evenly spaced, with the last flush against the far edge.
fn tile_starts(length: u32, tile: u32, overlap: u32) -> Vec<u32> {
    if length <= tile {
        return vec![0];
    }
    let stride = (tile - overlap).max(1);
    let mut starts: Vec<u32> = (0..length - tile).step_by(stride as usize).collect();
    starts.push(length - tile);
    starts
}

/// Weights a tile's pixel so overlapping tiles cross-fade. Never zero, so a pixel that only one
/// tile covers still counts in full once normalised.
fn feather(position: u32, tile: u32, overlap: u32) -> f32 {
    let ramp = overlap as f32 + 1.0;
    let from_edge = (position + 1).min(tile - position) as f32;
    (from_edge / ramp).min(1.0)
}

struct Tile {
    x: u32,
    y: u32,
    side: u32,
}

/// The tile as the network's planar input. A photo smaller than a tile repeats its edge pixels,
/// and those extra pixels are ignored on the way back.
fn tile_input(image: &LinearImage, tile: &Tile, gain: f32) -> Vec<f32> {
    let side = tile.side as usize;
    let mut input = vec![0.0; side * side * 3];
    let (width, height) = (image.width as usize, image.height as usize);
    for row in 0..side {
        let y = (tile.y as usize + row).min(height - 1);
        for column in 0..side {
            let x = (tile.x as usize + column).min(width - 1);
            let pixel = image.pixels[y * width + x];
            for channel in 0..3 {
                let value = (pixel[channel] * gain).clamp(0.0, 1.0);
                input[channel * side * side + row * side + column] = encode(value);
            }
        }
    }
    input
}

/// Adds a tile's weighted change, in linear light, to the running totals.
fn accumulate(
    totals: &mut [[f32; 4]],
    image_size: [u32; 2],
    tile: &Tile,
    input: &[f32],
    output: &[f32],
    (gain, overlap): (f32, u32),
) {
    let side = tile.side as usize;
    let [width, height] = image_size.map(|edge| edge as usize);
    for row in 0..side.min(height - tile.y as usize) {
        let row_weight = feather(row as u32, tile.side, overlap);
        for column in 0..side.min(width - tile.x as usize) {
            let weight = row_weight * feather(column as u32, tile.side, overlap);
            let at = row * side + column;
            let total = &mut totals[(tile.y as usize + row) * width + tile.x as usize + column];
            for (channel, sum) in total.iter_mut().take(3).enumerate() {
                let plane = channel * side * side;
                let cleaned = decode(output[plane + at].clamp(0.0, 1.0));
                *sum += weight * (cleaned - decode(input[plane + at])) / gain;
            }
            total[3] += weight;
        }
    }
}

/// Returns the photo with `strength` of the network's change applied, from 0 for none to 1 for
/// all of it.
pub(crate) fn denoise(
    model: &mut LoadedModel,
    spec: &RestorationSpec,
    image: &LinearImage,
    strength: f32,
    on_progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<LinearImage> {
    let RestorationSpec { tile: side, overlap } = *spec;
    let gain = exposure_gain(image);
    let columns = tile_starts(image.width, side, overlap);
    let rows = tile_starts(image.height, side, overlap);
    let total = (columns.len() * rows.len()) as u64;
    let mut totals = vec![[0.0f32; 4]; image.pixels.len()];

    let mut done = 0;
    on_progress(Progress { done, total });
    for &y in &rows {
        for &x in &columns {
            let tile = Tile { x, y, side };
            let input = tile_input(image, &tile, gain);
            let output = model.run([1, 3, side as usize, side as usize], &input, cancel)?;
            if output.shape != [1, 3, i64::from(side), i64::from(side)] {
                return Err(Error::UnexpectedOutput(output.shape));
            }
            accumulate(&mut totals, [image.width, image.height], &tile, &input, &output.values, (gain, overlap));
            done += 1;
            on_progress(Progress { done, total });
        }
    }
    Ok(blend(image, &totals, strength))
}

/// The fine part of one channel's change, with its local average taken out.
fn fine_change(totals: &[[f32; 4]], channel: usize, width: usize, height: usize) -> Vec<f32> {
    let change: Vec<f32> = totals.par_iter().map(|total| total[channel] / total[3].max(f32::EPSILON)).collect();
    let smooth = box_mean(&box_mean(&change, width, height, COLOUR_RADIUS), width, height, COLOUR_RADIUS);
    change.iter().zip(&smooth).map(|(value, local_average)| value - local_average).collect()
}

fn blend(image: &LinearImage, totals: &[[f32; 4]], strength: f32) -> LinearImage {
    let strength = strength.clamp(0.0, 1.0);
    let (width, height) = (image.width as usize, image.height as usize);
    let fine: Vec<Vec<f32>> =
        (0..3).into_par_iter().map(|channel| fine_change(totals, channel, width, height)).collect();
    let pixels = image
        .pixels
        .par_iter()
        .enumerate()
        .map(|(index, pixel)| {
            let [red, green, blue] = [0, 1, 2].map(|channel| pixel[channel] + strength * fine[channel][index]);
            [red, green, blue, pixel[3]]
        })
        .collect();
    LinearImage { width: image.width, height: image.height, pixels }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encoding_round_trips() {
        for value in [0.0, 0.001, 0.003_130_8, 0.05, 0.18, 0.5, 1.0] {
            assert!((decode(encode(value)) - value).abs() < 1e-5, "{value}");
        }
    }

    #[test]
    fn tiles_cover_the_whole_side_and_end_flush() {
        for length in [512, 513, 900, 959, 960, 961, 6024] {
            let starts = tile_starts(length, 512, 32);
            assert_eq!(starts.first(), Some(&0));
            assert_eq!(starts.last().map(|start| start + 512), Some(length), "length {length}");
            assert!(starts.windows(2).all(|pair| pair[1] > pair[0] && pair[1] - pair[0] <= 480));
        }
    }

    #[test]
    fn a_photo_smaller_than_a_tile_gets_one_tile() {
        assert_eq!(tile_starts(100, 512, 32), [0]);
    }

    #[test]
    fn feathering_fades_towards_the_edges_of_a_tile() {
        assert_eq!(feather(100, 512, 32), 1.0);
        assert!(feather(0, 512, 32) < feather(10, 512, 32));
        assert_eq!(feather(0, 512, 32), feather(511, 512, 32));
        assert!(feather(0, 512, 32) > 0.0);
    }

    fn flat(value: f32) -> LinearImage {
        LinearImage { width: 100, height: 100, pixels: vec![[value; 4]; 10_000] }
    }

    #[test]
    fn gain_brings_the_highlights_up_to_white() {
        assert!((exposure_gain(&flat(0.25)) - 4.0).abs() < 1e-5);
    }

    #[test]
    fn gain_ignores_a_handful_of_hot_pixels() {
        let mut image = flat(0.25);
        image.pixels[0] = [100.0; 4];
        assert!((exposure_gain(&image) - 4.0).abs() < 1e-5);
    }

    #[test]
    fn gain_is_limited_for_a_black_frame_and_never_dims() {
        assert_eq!(exposure_gain(&flat(0.0)), MAX_GAIN);
        assert_eq!(exposure_gain(&flat(3.0)), 1.0);
    }

    /// A change that alternates pixel by pixel, as removing noise does.
    fn alternating_totals() -> Vec<[f32; 4]> {
        (0..10_000).map(|index| if (index + index / 100) % 2 == 0 { [0.2, 0.2, 0.2, 2.0] } else { [-0.2, -0.2, -0.2, 2.0] }).collect()
    }

    #[test]
    fn strength_scales_the_change() {
        let image = flat(0.5);
        let totals = alternating_totals();
        let at = |strength| blend(&image, &totals, strength).pixels[5050][0];
        assert_eq!(at(0.0), 0.5);
        assert!((at(1.0) - 0.6).abs() < 1e-3, "{}", at(1.0));
        assert!((at(0.5) - 0.55).abs() < 1e-3, "{}", at(0.5));
        assert_eq!(at(7.0), at(1.0), "strength is limited to 1");
    }

    #[test]
    fn a_colour_cast_from_the_network_is_left_out() {
        let image = flat(0.5);
        let cast = vec![[0.3, -0.2, 0.1, 2.0]; 10_000];
        let result = blend(&image, &cast, 1.0);
        assert!(result.pixels.iter().flatten().all(|value| (value - 0.5).abs() < 1e-4));
    }

    #[test]
    fn fine_changes_keep_the_local_average_of_the_photo() {
        let image = flat(0.5);
        let mut totals = alternating_totals();
        totals.iter_mut().for_each(|total| total[0] += 0.3);
        let result = blend(&image, &totals, 1.0);
        let block: f32 = (40..60).flat_map(|y| (40..60).map(move |x| y * 100 + x)).map(|at: usize| result.pixels[at][0]).sum();
        assert!((block / 400.0 - 0.5).abs() < 1e-3, "{}", block / 400.0);
    }
}
