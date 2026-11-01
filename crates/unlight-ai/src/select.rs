//! Select Subject and Select Sky: a model's coarse mask, refined against the full picture.

use crate::{
    cancel::CancelToken,
    error::{Error, Result},
    guided::guided_filter,
    image::{RgbImage, Turn, resize_coverage},
    inference::{LoadedModel, Tensor},
    manifest::{OutputScale, SegmentationSpec},
    mask::MaskRaster,
};
use rayon::prelude::*;

/// How far the guided filter reaches for an edge, in the model's own pixels. This wide, it can
/// find the sky showing through gaps in a tree that the model was too coarse to see.
const EDGE_REACH: f32 = 4.0;
/// Low enough to follow a strand of hair, high enough to ignore sensor noise in the guide.
const EDGE_EPSILON: f32 = 1e-4;
/// The filter leaves a wide glow of nearly-full coverage around dark objects on a bright sky,
/// which would show as a pale rim when the sky is darkened. Coverage outside this range counts
/// as fully in or out.
const FIRM_RANGE: [f32; 2] = [0.1, 0.9];

fn input_tensor(image: &RgbImage, spec: &SegmentationSpec) -> Vec<f32> {
    let pixels = image.pixels.len() / 3;
    let mut tensor = vec![0.0; pixels * 3];
    let (red, rest) = tensor.split_at_mut(pixels);
    let (green, blue) = rest.split_at_mut(pixels);
    let planes = [red, green, blue];
    for (channel, plane) in planes.into_iter().enumerate() {
        let (mean, spread) = (spec.mean[channel], spec.std[channel]);
        plane.par_iter_mut().enumerate().for_each(|(pixel, value)| {
            *value = (f32::from(image.pixels[pixel * 3 + channel]) / 255.0 - mean) / spread;
        });
    }
    tensor
}

/// The model's mask at its own resolution, as 0..=1 coverage.
fn coverage(output: Tensor, spec: &SegmentationSpec) -> Result<Vec<f32>> {
    let side = i64::from(spec.input_size);
    if output.shape != [1, 1, side, side] {
        return Err(Error::UnexpectedOutput(output.shape));
    }
    Ok(match spec.output {
        OutputScale::Probability => output.values,
        OutputScale::Logits => output.values.par_iter().map(|score| 1.0 / (1.0 + (-score).exp())).collect(),
    })
}

fn refine(image: &RgbImage, coarse: &[f32], spec: &SegmentationSpec) -> Result<Vec<f32>> {
    let side = spec.input_size;
    let upscaled = resize_coverage(coarse, [side, side], [image.width, image.height])?;
    let working_per_model = image.width.max(image.height) as f32 / side as f32;
    let radius = (working_per_model * EDGE_REACH).ceil() as usize;
    let mut refined = guided_filter(image, &upscaled, radius, EDGE_EPSILON);
    refined.par_iter_mut().for_each(|value| *value = firm_up(*value));
    Ok(refined)
}

fn firm_up(value: f32) -> f32 {
    let [low, high] = FIRM_RANGE;
    ((value - low) / (high - low)).clamp(0.0, 1.0)
}

/// Produces a mask for a rendered photo in sensor orientation, in that same orientation.
pub(crate) fn select(
    model: &mut LoadedModel,
    spec: &SegmentationSpec,
    image: &RgbImage,
    turn: Turn,
    cancel: &CancelToken,
) -> Result<MaskRaster> {
    let upright = turn.upright(image);
    let side = spec.input_size;
    let input = input_tensor(&upright.resized(side, side)?, spec);
    let shape = [1, 3, side as usize, side as usize];
    let output = model.run(shape, &input, cancel)?;
    let refined = refine(&upright, &coverage(output, spec)?, spec)?;
    cancel.check()?;
    Ok(turn.to_sensor(&MaskRaster::from_unit(upright.width, upright.height, &refined)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(output: OutputScale) -> SegmentationSpec {
        SegmentationSpec { input_size: 2, mean: [0.5, 0.25, 0.0], std: [0.5, 0.25, 1.0], output }
    }

    #[test]
    fn the_input_is_planar_and_normalised_per_channel() {
        let image = RgbImage { width: 2, height: 1, pixels: vec![255, 0, 51, 0, 255, 102] };
        let tensor = input_tensor(&image, &spec(OutputScale::Logits));
        assert_eq!(tensor.len(), 6);
        assert!((tensor[0] - 1.0).abs() < 1e-6, "red of the first pixel");
        assert!((tensor[1] + 1.0).abs() < 1e-6, "red of the second pixel");
        assert!((tensor[2] + 1.0).abs() < 1e-6, "green of the first pixel");
        assert!((tensor[3] - 3.0).abs() < 1e-6, "green of the second pixel");
        assert!((tensor[4] - 0.2).abs() < 1e-6, "blue of the first pixel");
    }

    #[test]
    fn logits_go_through_a_sigmoid_and_probabilities_do_not() {
        let tensor = || Tensor { shape: vec![1, 1, 2, 2], values: vec![0.0, 1.0, -20.0, 20.0] };
        let scored = coverage(tensor(), &spec(OutputScale::Logits)).unwrap();
        assert!((scored[0] - 0.5).abs() < 1e-6 && scored[2] < 1e-6 && scored[3] > 0.999);
        assert_eq!(coverage(tensor(), &spec(OutputScale::Probability)).unwrap()[1], 1.0);
    }

    #[test]
    fn near_full_coverage_snaps_to_full_and_the_middle_is_kept() {
        assert_eq!(firm_up(0.95), 1.0);
        assert_eq!(firm_up(0.05), 0.0);
        assert!((firm_up(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_tensor_of_the_wrong_shape_is_refused() {
        let tensor = Tensor { shape: vec![1, 3, 2, 2], values: vec![0.0; 12] };
        let result = coverage(tensor, &spec(OutputScale::Logits));
        assert!(matches!(result, Err(Error::UnexpectedOutput(_))));
    }
}
