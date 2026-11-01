//! Natural cubic spline. Camera tables are coarse enough that linear interpolation between
//! their knots would put visible kinks in straight lines.

pub(crate) struct Spline {
    knots: Vec<f64>,
    values: Vec<f64>,
    curvature: Vec<f64>,
}

impl Spline {
    /// `knots` must be strictly ascending and at least as long as `values`.
    pub fn new(knots: Vec<f64>, values: Vec<f64>) -> Self {
        let count = knots.len();
        let mut curvature = vec![0.0; count];
        let mut upper = vec![0.0; count];
        for index in 1..count.saturating_sub(1) {
            let (left, right) = (knots[index] - knots[index - 1], knots[index + 1] - knots[index]);
            let pivot = 2.0 * (left + right) - left * upper[index - 1];
            let slope = (values[index + 1] - values[index]) / right
                - (values[index] - values[index - 1]) / left;
            upper[index] = right / pivot;
            curvature[index] = (6.0 * slope - left * curvature[index - 1]) / pivot;
        }
        for index in (1..count.saturating_sub(1)).rev() {
            curvature[index] -= upper[index] * curvature[index + 1];
        }
        Self { knots, values, curvature }
    }

    /// Continues straight past either end. Holding the last value instead would put a kink
    /// right at the image corner, where tables usually stop.
    pub fn at(&self, x: f64) -> f64 {
        let last = self.knots.len() - 1;
        if x < self.knots[0] {
            return self.values[0] + (x - self.knots[0]) * self.end_slope(0);
        }
        if x > self.knots[last] {
            return self.values[last] + (x - self.knots[last]) * self.end_slope(last - 1);
        }
        let upper = self.knots.partition_point(|&knot| knot <= x).min(last);
        let lower = upper - 1;
        let width = self.knots[upper] - self.knots[lower];
        let (a, b) = ((self.knots[upper] - x) / width, (x - self.knots[lower]) / width);
        a * self.values[lower]
            + b * self.values[upper]
            + ((a * a * a - a) * self.curvature[lower] + (b * b * b - b) * self.curvature[upper])
                * width
                * width
                / 6.0
    }

    /// The slope at the outer end of the span starting at `span`.
    fn end_slope(&self, span: usize) -> f64 {
        let width = self.knots[span + 1] - self.knots[span];
        let chord = (self.values[span + 1] - self.values[span]) / width;
        let bend = self.curvature[span + 1] + 2.0 * self.curvature[span];
        if span == 0 {
            chord - width * bend / 6.0
        } else {
            chord + width * (2.0 * self.curvature[span + 1] + self.curvature[span]) / 6.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_its_knots() {
        let spline = Spline::new(vec![0.0, 0.3, 0.5, 1.0], vec![1.0, -2.0, 4.0, 0.5]);
        for (x, y) in [(0.0, 1.0), (0.3, -2.0), (0.5, 4.0), (1.0, 0.5)] {
            assert!((spline.at(x) - y).abs() < 1e-12);
        }
    }

    #[test]
    fn reproduces_a_straight_line_exactly() {
        let knots: Vec<f64> = (0..8).map(|index| f64::from(index) / 7.0).collect();
        let values = knots.iter().map(|x| 3.0 * x - 1.0).collect();
        let spline = Spline::new(knots, values);
        assert!((spline.at(0.42) - (3.0 * 0.42 - 1.0)).abs() < 1e-12);
    }

    #[test]
    fn follows_a_smooth_curve_far_better_than_a_chord() {
        let knots: Vec<f64> = (0..16).map(|index| f64::from(index) / 15.0).collect();
        let curve = |x: f64| -0.05 * x * x + 0.01 * x.powi(4);
        let spline = Spline::new(knots.clone(), knots.iter().map(|&x| curve(x)).collect());
        let x = 0.5 + 0.5 / 15.0;
        assert!((spline.at(x) - curve(x)).abs() < 3e-6);
    }

    #[test]
    fn continues_smoothly_past_the_ends() {
        let spline = Spline::new(vec![0.0, 1.0, 2.0, 3.0], vec![0.0, 2.0, 4.0, 6.0]);
        assert!((spline.at(4.0) - 8.0).abs() < 1e-12);
        assert!((spline.at(-1.0) + 2.0).abs() < 1e-12);
    }

    #[test]
    fn the_slope_does_not_jump_at_the_last_knot() {
        let knots: Vec<f64> = (0..16).map(|index| f64::from(index) / 15.0).collect();
        let spline = Spline::new(knots.clone(), knots.iter().map(|x| -0.05 * x * x).collect());
        let step = 1e-5;
        let inside = (spline.at(1.0) - spline.at(1.0 - step)) / step;
        let outside = (spline.at(1.0 + step) - spline.at(1.0)) / step;
        assert!((inside - outside).abs() < 1e-3, "{inside} vs {outside}");
    }
}
