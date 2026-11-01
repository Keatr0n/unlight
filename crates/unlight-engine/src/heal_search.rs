//! Finds a patch to heal from when the user only clicks a spot. This analyses the photo once
//! and stores a position in the recipe; rendering stays on the GPU.

/// Ring points compared between target and candidate.
const RING_POINTS: usize = 24;
const RING_REACH: f32 = 1.12;
/// Probes across a patch's radius, finely enough not to step over a fence post.
const INNER_STEPS: i32 = 5;
/// How far a candidate may be, in radii.
pub(crate) const SEARCH_RADII: f32 = 10.0;
/// Candidates per radius along each axis.
const STEPS_PER_RADIUS: f32 = 4.0;
/// How much it counts against a patch to hold something its own ring doesn't predict. That's
/// what an object is, whereas a road running through a patch is predicted by where it crosses.
const SURPRISE_WEIGHT: f32 = 0.5;
/// How much it counts against a patch to be finer or coarser than the ground around the spot.
/// Without this, a featureless patch would win on grass and leave a blurred hole.
const TEXTURE_WEIGHT: f32 = 4.0;
/// How far out fine texture is measured, past the ring, in pixels of the level searched.
const TEXTURE_STEP: f32 = 1.5;
/// How much it counts against a patch to need correcting. A heal multiplies the patch by the
/// difference in light, so a big one amplifies its noise and whatever else it holds.
const LIGHT_WEIGHT: f32 = 0.05;
/// Enough preference for nearby patches to break ties, since light drifts with distance.
const DISTANCE_WEIGHT: f32 = 1e-4;

/// Part of one level of the pyramid, as interleaved RGB.
pub(crate) struct Patchwork {
    log_rgb: Vec<[f32; 3]>,
    width: usize,
    height: usize,
}

/// A point inside a patch, and how much each ring point contributes to what's expected there.
struct InnerPoint {
    offset: [f32; 2],
    weights: [f32; RING_POINTS],
}

/// The same harmonic interpolation the heal itself does, from the Poisson integral for a disc,
/// so that what counts as a surprise here is what the heal would leave visible.
fn inner_points() -> Vec<InnerPoint> {
    let mut points = Vec::new();
    for row in -INNER_STEPS..=INNER_STEPS {
        for column in -INNER_STEPS..=INNER_STEPS {
            let offset = [column as f32 / INNER_STEPS as f32, row as f32 / INNER_STEPS as f32];
            if offset[0].hypot(offset[1]) > 1.0 {
                continue;
            }
            let on_ring = offset.map(|value| value / RING_REACH);
            let mut weights: [f32; RING_POINTS] = std::array::from_fn(|index| {
                let angle = std::f32::consts::TAU * index as f32 / RING_POINTS as f32;
                let gap = [angle.cos() - on_ring[0], angle.sin() - on_ring[1]];
                1.0 / (gap[0] * gap[0] + gap[1] * gap[1]).max(1e-5)
            });
            let total: f32 = weights.iter().sum();
            weights.iter_mut().for_each(|weight| *weight /= total);
            points.push(InnerPoint { offset, weights });
        }
    }
    points
}

/// The photo around a spot, as a patch will be judged against it.
struct Surroundings {
    ring: [[f32; 3]; RING_POINTS],
    roughness: f32,
    inner: Vec<InnerPoint>,
}

/// An area other spots already claimed. Both figures are in this level's pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Claimed {
    pub center: [f32; 2],
    pub radius: f32,
}

impl Patchwork {
    pub(crate) fn new(rgb: &[[f32; 3]], width: usize, height: usize) -> Self {
        let log_rgb = rgb.iter().map(|pixel| pixel.map(|value| value.max(1e-4).ln())).collect();
        Self { log_rgb, width, height }
    }

    fn at(&self, x: f32, y: f32) -> [f32; 3] {
        let column = (x.max(0.0) as usize).min(self.width - 1);
        let row = (y.max(0.0) as usize).min(self.height - 1);
        self.log_rgb[row * self.width + column]
    }

    fn circle(&self, center: [f32; 2], reach: f32) -> [[f32; 3]; RING_POINTS] {
        std::array::from_fn(|index| {
            let angle = std::f32::consts::TAU * index as f32 / RING_POINTS as f32;
            self.at(center[0] + angle.cos() * reach, center[1] + angle.sin() * reach)
        })
    }

    /// How much the photo changes over a pixel or two, just outside a ring.
    fn roughness(&self, ring: &[[f32; 3]; RING_POINTS], center: [f32; 2], reach: f32) -> f32 {
        let outside = self.circle(center, reach + TEXTURE_STEP);
        let change = |(near, far): (&[f32; 3], &[f32; 3])| (0..3).map(|c| (near[c] - far[c]).abs()).sum::<f32>();
        ring.iter().zip(&outside).map(change).sum::<f32>() / RING_POINTS as f32
    }

    /// How far the inside of a patch strays from what its ring predicts.
    fn surprise(&self, ring: &[[f32; 3]; RING_POINTS], inner: &[InnerPoint], center: [f32; 2], radius: f32) -> f32 {
        let total: f32 = inner
            .iter()
            .map(|point| {
                let pixel = self.at(center[0] + point.offset[0] * radius, center[1] + point.offset[1] * radius);
                (0..3)
                    .map(|channel| {
                        let expected: f32 = ring.iter().zip(&point.weights).map(|(on, weight)| on[channel] * weight).sum();
                        (pixel[channel] - expected).powi(2)
                    })
                    .sum::<f32>()
            })
            .sum();
        total / inner.len() as f32
    }

    fn cost(&self, wanted: &Surroundings, candidate: [f32; 2], radius: f32) -> f32 {
        let reach = radius * RING_REACH;
        let found = self.circle(candidate, reach);
        let shift: [f32; 3] = std::array::from_fn(|channel| {
            wanted.ring.iter().zip(&found).map(|(near, far)| near[channel] - far[channel]).sum::<f32>() / RING_POINTS as f32
        });
        let mismatch: f32 = wanted
            .ring
            .iter()
            .zip(&found)
            .map(|(near, far)| (0..3).map(|c| (near[c] - far[c] - shift[c]).powi(2)).sum::<f32>())
            .sum::<f32>()
            / RING_POINTS as f32;
        let surprise = self.surprise(&found, &wanted.inner, candidate, radius);
        let texture = (self.roughness(&found, candidate, reach) - wanted.roughness).powi(2);
        let light: f32 = shift.iter().map(|gap| gap * gap).sum();
        mismatch + SURPRISE_WEIGHT * surprise + TEXTURE_WEIGHT * texture + LIGHT_WEIGHT * light
    }

    fn fits(&self, center: [f32; 2], radius: f32) -> bool {
        let reach = radius * RING_REACH + 1.0;
        center[0] >= reach
            && center[1] >= reach
            && center[0] + reach < self.width as f32
            && center[1] + reach < self.height as f32
    }

    fn surroundings(&self, target: [f32; 2], radius: f32) -> Surroundings {
        let reach = radius * RING_REACH;
        let ring = self.circle(target, reach);
        Surroundings { roughness: self.roughness(&ring, target, reach), ring, inner: inner_points() }
    }

    /// The best centre for a patch to cover `target`, or `None` if nothing here can hold one.
    pub(crate) fn best_source(&self, target: [f32; 2], radius: f32, claimed: &[Claimed]) -> Option<[f32; 2]> {
        let wanted = self.surroundings(target, radius);
        let step = (radius / STEPS_PER_RADIUS).max(1.0);
        let reach = (radius * SEARCH_RADII / step).ceil() as i32;
        let closest = radius * RING_REACH * 2.0 + 1.0;
        let mut best: Option<(f32, [f32; 2])> = None;
        for row in -reach..=reach {
            for column in -reach..=reach {
                let candidate = [target[0] + column as f32 * step, target[1] + row as f32 * step];
                let distance = (candidate[0] - target[0]).hypot(candidate[1] - target[1]);
                let free = claimed.iter().all(|spot| {
                    (candidate[0] - spot.center[0]).hypot(candidate[1] - spot.center[1]) >= radius + spot.radius
                });
                if distance < closest || !free || !self.fits(candidate, radius) {
                    continue;
                }
                let cost = self.cost(&wanted, candidate, radius) + DISTANCE_WEIGHT * distance / radius;
                if best.is_none_or(|(lowest, _)| cost < lowest) {
                    best = Some((cost, candidate));
                }
            }
        }
        best.map(|(_, center)| center)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: usize = 200;

    fn scene(pixel: impl Fn(f32, f32) -> f32) -> Patchwork {
        let rgb: Vec<[f32; 3]> = (0..SIZE * SIZE)
            .map(|index| {
                let value = pixel((index % SIZE) as f32, (index / SIZE) as f32);
                [value; 3]
            })
            .collect();
        Patchwork::new(&rgb, SIZE, SIZE)
    }

    #[test]
    fn a_patch_keeps_clear_of_the_spot_it_will_cover() {
        let patchwork = scene(|_, _| 0.3);
        let found = patchwork.best_source([100.0, 100.0], 10.0, &[]).unwrap();
        let distance = (found[0] - 100.0).hypot(found[1] - 100.0);
        assert!(distance >= 2.0 * 10.0, "{found:?}");
    }

    #[test]
    fn a_patch_lands_on_the_same_side_of_an_edge_as_the_spot() {
        let patchwork = scene(|x, _| if x < 90.0 { 0.1 } else { 0.8 });
        let found = patchwork.best_source([60.0, 100.0], 8.0, &[]).unwrap();
        assert!(found[0] + 8.0 * RING_REACH < 90.0, "{found:?}");
    }

    #[test]
    fn a_patch_avoids_a_dark_blemish_inside_itself() {
        let blemish = |x: f32, y: f32| (x - 120.0).hypot(y - 100.0) < 6.0;
        let patchwork = scene(|x, y| if blemish(x, y) { 0.02 } else { 0.4 });
        let found = patchwork.best_source([70.0, 100.0], 8.0, &[]).unwrap();
        assert!((found[0] - 120.0).hypot(found[1] - 100.0) > 8.0 + 6.0, "{found:?}");
    }

    /// Blotches that don't repeat, so no other patch of it matches a ring closely. That's what
    /// grass and gravel are like, and what lets a featureless patch look like a good fit.
    fn blotches(x: f32, y: f32) -> f32 {
        let (cell_x, cell_y) = ((x / 3.0).floor(), (y / 3.0).floor());
        let hash = (cell_x * 12.9 + cell_y * 78.2).sin() * 4375.8;
        1.0 + 0.7 * hash.fract().abs()
    }

    const SPOT: [f32; 2] = [100.0, 150.0];
    const RADIUS: f32 = 8.0;

    fn cost_at(patchwork: &Patchwork, candidate: [f32; 2]) -> f32 {
        patchwork.cost(&patchwork.surroundings(SPOT, RADIUS), candidate, RADIUS)
    }

    #[test]
    fn a_flat_patch_in_other_light_costs_more_than_imperfect_ground_nearby() {
        let patchwork = scene(|x, y| if y < 110.0 { 0.8 } else { 0.1 * blotches(x, y) });
        let (ground, sky) = (cost_at(&patchwork, [130.0, 150.0]), cost_at(&patchwork, [100.0, 90.0]));
        assert!(ground < sky, "ground {ground} sky {sky}");
    }

    #[test]
    fn a_flat_patch_costs_more_than_texture_when_the_spot_is_on_texture() {
        let patchwork = scene(|x, y| if y < 110.0 { 0.13 } else { 0.1 * blotches(x, y) });
        let (textured, flat) = (cost_at(&patchwork, [130.0, 150.0]), cost_at(&patchwork, [100.0, 90.0]));
        assert!(textured < flat, "textured {textured} flat {flat}");
    }

    #[test]
    fn a_busy_patch_costs_more_than_a_plain_one_when_the_spot_is_plain() {
        let patchwork = scene(|x, y| if y < 110.0 { 0.1 * blotches(x, y) } else { 0.13 });
        let (plain, busy) = (cost_at(&patchwork, [130.0, 150.0]), cost_at(&patchwork, [100.0, 90.0]));
        assert!(plain < busy, "plain {plain} busy {busy}");
    }

    #[test]
    fn a_spot_on_a_strong_edge_does_not_make_an_object_welcome() {
        // The ring spans dark trees and bright grass, so it varies a lot. That's no reason to
        // want a patch with a car in it, however similar the car's contrast.
        let car = |x: f32, y: f32| (x - 150.0).hypot(y - 150.0) < 4.0;
        let patchwork = scene(|x, y| {
            let trees = x < 130.0 && y < 150.0;
            if trees || car(x, y) { 0.02 } else { 0.4 }
        });
        let (open, object) = (cost_at(&patchwork, [150.0, 60.0]), cost_at(&patchwork, [150.0, 150.0]));
        assert!(open < object, "open ground {open} object {object}");
    }

    #[test]
    fn a_road_through_a_patch_is_not_held_against_it() {
        let road = |x: f32, y: f32| (x + y - 250.0).abs() < 4.0;
        let patchwork = scene(|x, y| if road(x, y) { 0.1 } else { 0.4 });
        let (along, plain) = (cost_at(&patchwork, [130.0, 120.0]), cost_at(&patchwork, [40.0, 60.0]));
        assert!(along < plain, "along the road {along} plain ground {plain}");
    }

    #[test]
    fn a_thin_post_in_a_patch_is_not_stepped_over() {
        let patchwork = scene(|x, y| if (x - 150.0).abs() < 1.0 && (y - 150.0).abs() < 7.0 { 0.02 } else { 0.4 });
        let (open, post) = (cost_at(&patchwork, [150.0, 100.0]), cost_at(&patchwork, [150.0, 150.0]));
        assert!(open < post, "open ground {open} post {post}");
    }

    #[test]
    fn a_patch_keeps_off_other_spots() {
        let patchwork = scene(|_, _| 0.3);
        let claimed = [Claimed { center: [125.0, 100.0], radius: 10.0 }];
        let found = patchwork.best_source([100.0, 100.0], 10.0, &claimed).unwrap();
        assert!((found[0] - 125.0).hypot(found[1] - 100.0) >= 20.0, "{found:?}");
    }

    #[test]
    fn nowhere_to_go_finds_nothing() {
        let rgb = vec![[0.3; 3]; 20 * 20];
        let patchwork = Patchwork::new(&rgb, 20, 20);
        assert_eq!(patchwork.best_source([10.0, 10.0], 8.0, &[]), None);
    }
}
