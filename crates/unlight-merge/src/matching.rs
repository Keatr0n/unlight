//! Pairs up features between two frames. Lowe's ratio test throws away anything ambiguous, and
//! a match has to be mutual, which removes most false matches before RANSAC ever sees them.

use crate::features::{DESCRIPTOR, Feature};
use rayon::prelude::*;

/// Largest ratio of nearest to second-nearest squared distance.
const RATIO: f32 = 0.64;

#[inline]
fn similarity(a: &[f32; DESCRIPTOR], b: &[f32; DESCRIPTOR]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// For each of `from`, the index of its nearest in `to`, if it passes the ratio test.
fn nearest(from: &[Feature], to: &[Feature]) -> Vec<Option<u32>> {
    from.par_iter()
        .map(|feature| {
            let (mut best, mut second, mut index) = (-1.0f32, -1.0f32, 0u32);
            for (candidate, other) in to.iter().enumerate() {
                let score = similarity(&feature.descriptor, &other.descriptor);
                if score > best {
                    second = best;
                    best = score;
                    index = candidate as u32;
                } else if score > second {
                    second = score;
                }
            }
            // Unit vectors, so a squared distance is 2 - 2·cos.
            let (nearest, runner_up) = (2.0 - 2.0 * best, 2.0 - 2.0 * second);
            (best >= -0.5 && nearest <= runner_up * RATIO).then_some(index)
        })
        .collect()
}

/// Index pairs of features that are each other's clear nearest neighbour.
pub fn match_features(a: &[Feature], b: &[Feature]) -> Vec<(u32, u32)> {
    if a.len() < 2 || b.len() < 2 {
        return Vec::new();
    }
    let (forward, backward) = rayon::join(|| nearest(a, b), || nearest(b, a));
    forward
        .iter()
        .enumerate()
        .filter_map(|(index, partner)| {
            let partner = (*partner)?;
            (backward[partner as usize] == Some(index as u32)).then_some((index as u32, partner))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(direction: usize, mix: f32) -> Feature {
        let mut descriptor = Box::new([0.0f32; DESCRIPTOR]);
        descriptor[direction] = 1.0;
        descriptor[(direction + 1) % DESCRIPTOR] = mix;
        let length = (1.0 + mix * mix).sqrt();
        descriptor.iter_mut().for_each(|value| *value /= length);
        Feature { x: 0.0, y: 0.0, scale: 1.0, angle: 0.0, descriptor }
    }

    #[test]
    fn identical_features_match_in_order() {
        let a = vec![feature(0, 0.1), feature(10, 0.1), feature(20, 0.1)];
        assert_eq!(match_features(&a, &a), vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn an_ambiguous_feature_is_dropped() {
        let a = vec![feature(0, 0.0), feature(30, 0.0)];
        let b = vec![feature(0, 0.05), feature(0, 0.06), feature(30, 0.0), feature(50, 0.0)];
        let matches = match_features(&a, &b);
        assert_eq!(matches, vec![(1, 2)]);
    }

    #[test]
    fn too_few_features_match_nothing() {
        assert!(match_features(&[feature(0, 0.0)], &[feature(0, 0.0)]).is_empty());
    }
}
