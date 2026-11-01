//! The surface a panorama is laid out on. World rays have y pointing down and z forward, so
//! `(0, 0, 1)` is the middle of the view.

use crate::{BoundaryWarp, linalg::normalized};
use std::sync::Arc;
use unlight_color::matrix::Vec3;

/// A ray closer to perpendicular than this can't land on a perspective plane.
const NEAREST_PERSPECTIVE_DEPTH: f64 = 0.05;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Projection {
    /// Longitude and latitude. Handles any field of view.
    #[default]
    Spherical,
    /// Longitude and height, which keeps verticals straight.
    Cylindrical,
    /// A flat plane, which keeps every straight line straight but can't go wide.
    Perspective,
}

impl Projection {
    /// Where a ray lands, in units of the focal length.
    pub fn project(self, ray: &Vec3) -> Option<[f64; 2]> {
        let [x, y, z] = *ray;
        match self {
            Self::Spherical => {
                let length = (x * x + y * y + z * z).sqrt();
                Some([x.atan2(z), (y / length).clamp(-1.0, 1.0).asin()])
            }
            Self::Cylindrical => {
                let across = x.hypot(z);
                (across > 1e-9).then(|| [x.atan2(z), y / across])
            }
            Self::Perspective => {
                let depth = z / (x * x + y * y + z * z).sqrt();
                (depth > NEAREST_PERSPECTIVE_DEPTH).then(|| [x / z, y / z])
            }
        }
    }

    /// The unit ray through a position, the inverse of `project`.
    pub fn unproject(self, point: [f64; 2]) -> Vec3 {
        let [a, b] = point;
        match self {
            Self::Spherical => {
                let (sin_lat, cos_lat) = b.sin_cos();
                let (sin_lon, cos_lon) = a.sin_cos();
                [sin_lon * cos_lat, sin_lat, cos_lon * cos_lat]
            }
            Self::Cylindrical => {
                let (sin_lon, cos_lon) = a.sin_cos();
                normalized(&[sin_lon, b, cos_lon])
            }
            Self::Perspective => normalized(&[a, b, 1.0]),
        }
    }
}

/// The rectangle of a projection that makes up the output, independent of its pixel size, so
/// the same canvas serves the small working copies and the full image. Positions run from 0 to
/// 1 across each axis of the final picture.
#[derive(Clone, Debug)]
pub struct Canvas {
    pub projection: Projection,
    /// Extent in projection units.
    pub min: [f64; 2],
    pub max: [f64; 2],
    /// The output window within the warped extent, as `[left, top, right, bottom]` from 0 to 1.
    pub window: [f64; 4],
    pub warp: Option<Arc<BoundaryWarp>>,
}

impl Canvas {
    pub fn new(projection: Projection, min: [f64; 2], max: [f64; 2]) -> Self {
        Self { projection, min, max, window: [0.0, 0.0, 1.0, 1.0], warp: None }
    }

    /// Extent of the visible window in projection units, before any boundary warp stretches it.
    pub fn extent(&self) -> [f64; 2] {
        [
            (self.max[0] - self.min[0]) * (self.window[2] - self.window[0]),
            (self.max[1] - self.min[1]) * (self.window[3] - self.window[1]),
        ]
    }

    /// Output size at a scale in pixels per unit, never less than one pixel.
    pub fn size(&self, scale: f64) -> [usize; 2] {
        self.extent().map(|length| ((length * scale).round() as usize).max(1))
    }

    /// The ray through a position in the picture.
    pub fn ray(&self, u: f64, v: f64) -> Vec3 {
        let [left, top, right, bottom] = self.window;
        let (windowed_u, windowed_v) = (left + u * (right - left), top + v * (bottom - top));
        let (x, y) = match &self.warp {
            Some(warp) => warp.source(windowed_u, windowed_v),
            None => (windowed_u, windowed_v),
        };
        self.projection.unproject([
            self.min[0] + x * (self.max[0] - self.min[0]),
            self.min[1] + y * (self.max[1] - self.min[1]),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linalg::dot;

    const ALL: [Projection; 3] = [Projection::Spherical, Projection::Cylindrical, Projection::Perspective];

    #[test]
    fn projecting_and_unprojecting_are_inverses() {
        for projection in ALL {
            for ray in [[0.0, 0.0, 1.0], [0.3, -0.2, 1.0], [-0.5, 0.4, 0.8]] {
                let unit = normalized(&ray);
                let point = projection.project(&unit).unwrap();
                let back = projection.unproject(point);
                assert!(dot(&unit, &back) > 1.0 - 1e-12, "{projection:?} {ray:?}");
            }
        }
    }

    #[test]
    fn perspective_refuses_rays_it_cant_reach() {
        assert!(Projection::Perspective.project(&[1.0, 0.0, 0.01]).is_none());
        assert!(Projection::Perspective.project(&[0.0, 0.0, -1.0]).is_none());
    }

    #[test]
    fn wide_projections_go_all_the_way_round() {
        for projection in [Projection::Spherical, Projection::Cylindrical] {
            let behind = projection.project(&[0.1, 0.0, -1.0]).unwrap();
            assert!(behind[0] > 3.0, "{projection:?} {behind:?}");
        }
    }

    #[test]
    fn verticals_stay_vertical_on_a_cylinder() {
        let high = Projection::Cylindrical.project(&normalized(&[0.5, -0.8, 1.0])).unwrap();
        let low = Projection::Cylindrical.project(&normalized(&[0.5, 0.8, 1.0])).unwrap();
        assert!((high[0] - low[0]).abs() < 1e-12);
    }

    #[test]
    fn the_middle_of_a_symmetric_canvas_looks_forward() {
        let canvas = Canvas::new(Projection::Spherical, [-1.0, -0.5], [1.0, 0.5]);
        let ray = canvas.ray(0.5, 0.5);
        assert!(dot(&ray, &[0.0, 0.0, 1.0]) > 1.0 - 1e-12);
        assert_eq!(canvas.size(100.0), [200, 100]);
    }

    #[test]
    fn a_window_selects_part_of_the_extent() {
        let mut canvas = Canvas::new(Projection::Spherical, [-1.0, -1.0], [1.0, 1.0]);
        let middle = canvas.ray(0.75, 0.5);
        canvas.window = [0.5, 0.0, 1.0, 1.0];
        let same = canvas.ray(0.5, 0.5);
        assert!(dot(&middle, &same) > 1.0 - 1e-12);
        assert_eq!(canvas.size(10.0), [10, 20]);
    }
}
