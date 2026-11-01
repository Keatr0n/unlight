//! Upright guides live on the source image, but are drawn on the corrected view, which moves
//! as soon as a second guide goes down. So they're mapped through the same geometry the engine
//! renders with, in both directions.

use super::view::Orientation;
use unlight_engine::Geometry;

const INVERSE_STEPS: usize = 32;
const MIN_LENGTH: f32 = 0.02;

/// Where a point of the view lands on the source.
pub fn view_to_source(
    geometry: &Geometry,
    view: [f32; 2],
    orientation: Orientation,
    aspect: f32,
) -> [f32; 2] {
    geometry.source_uv(orientation.to_sensor(view), aspect)
}

/// Where a point of the source shows in the view. There's no closed form through a warp, but
/// corrections move points by a small fraction of the frame, so stepping by the remaining
/// error settles quickly.
pub fn source_to_view(
    geometry: &Geometry,
    source: [f32; 2],
    orientation: Orientation,
    aspect: f32,
) -> [f32; 2] {
    let mut view = source;
    for _ in 0..INVERSE_STEPS {
        let landed = geometry.source_uv(view, aspect);
        view = [view[0] + source[0] - landed[0], view[1] + source[1] - landed[1]];
    }
    orientation.to_display(view)
}

/// A drag short enough to be a slip of the hand isn't a guide.
pub fn long_enough(start: [f32; 2], end: [f32; 2]) -> bool {
    (start[0] - end[0]).hypot(start[1] - end[1]) >= MIN_LENGTH
}

/// How far a point is from a segment, in the same units.
pub fn distance_to_segment(point: [f32; 2], start: [f32; 2], end: [f32; 2]) -> f32 {
    let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 {
        (((point[0] - start[0]) * dx + (point[1] - start[1]) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (point[0] - start[0] - t * dx).hypot(point[1] - start[1] - t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_engine::Optics;
    use unlight_recipe::{Crop, LensCorrections, Transform};

    const ASPECT: f32 = 1.5;

    fn warped() -> Geometry {
        let transform = Transform {
            vertical: 30.0,
            rotate: 3.0,
            ..Transform::default()
        };
        Geometry::new(
            &Crop::default(),
            &transform,
            &LensCorrections::default(),
            ASPECT,
            &Optics::default(),
        )
    }

    #[test]
    fn view_and_source_round_trip_through_a_warp() {
        let geometry = warped();
        for orientation in Orientation::ALL {
            for view in [[0.2, 0.3], [0.5, 0.5], [0.8, 0.7]] {
                let source = view_to_source(&geometry, view, orientation, ASPECT);
                let back = source_to_view(&geometry, source, orientation, ASPECT);
                assert!(
                    (back[0] - view[0]).abs() < 1e-3 && (back[1] - view[1]).abs() < 1e-3,
                    "{back:?}"
                );
            }
        }
    }

    #[test]
    fn a_slip_of_the_hand_is_not_a_guide() {
        assert!(!long_enough([0.5, 0.5], [0.505, 0.5]));
        assert!(long_enough([0.2, 0.5], [0.6, 0.5]));
    }

    #[test]
    fn distance_stops_at_the_ends_of_a_segment() {
        assert_eq!(distance_to_segment([0.5, 0.2], [0.0, 0.0], [1.0, 0.0]), 0.2);
        assert_eq!(distance_to_segment([2.0, 0.0], [0.0, 0.0], [1.0, 0.0]), 1.0);
    }
}
