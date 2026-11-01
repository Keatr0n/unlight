//! Builds the tone curve lookup table from the recipe. Curves work on gamma-encoded values,
//! as Lightroom's do.

use unlight_recipe::{Curve, ParametricCurve, ToneCurve};

pub const LUT_SIZE: usize = 1024;

/// Fritsch–Carlson monotone cubic: smooth through its points, and it can't overshoot.
pub(crate) fn monotone_spline(points: &[(f32, f32)]) -> impl Fn(f32) -> f32 + '_ {
    let count = points.len();
    let secants: Vec<f32> = points
        .windows(2)
        .map(|pair| (pair[1].1 - pair[0].1) / (pair[1].0 - pair[0].0).max(1e-6))
        .collect();
    let mut tangents = vec![0.0; count];
    if count >= 2 {
        tangents[0] = secants[0];
        tangents[count - 1] = secants[count - 2];
    }
    for i in 1..count.saturating_sub(1) {
        tangents[i] = if secants[i - 1] * secants[i] <= 0.0 {
            0.0
        } else {
            (secants[i - 1] + secants[i]) * 0.5
        };
    }
    for i in 0..count.saturating_sub(1) {
        if secants[i].abs() < 1e-6 {
            tangents[i] = 0.0;
            tangents[i + 1] = 0.0;
            continue;
        }
        let (a, b) = (tangents[i] / secants[i], tangents[i + 1] / secants[i]);
        let magnitude = a.hypot(b);
        if magnitude > 3.0 {
            let limit = 3.0 / magnitude;
            tangents[i] = limit * a * secants[i];
            tangents[i + 1] = limit * b * secants[i];
        }
    }

    move |x: f32| {
        let Some(&(first_x, first_y)) = points.first() else {
            return x;
        };
        let &(last_x, last_y) = points.last().unwrap_or(&(first_x, first_y));
        if x <= first_x {
            return first_y;
        }
        if x >= last_x {
            return last_y;
        }
        let index = points.partition_point(|point| point.0 <= x) - 1;
        let (x0, y0) = points[index];
        let (x1, y1) = points[index + 1];
        let span = (x1 - x0).max(1e-6);
        let t = (x - x0) / span;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * y0
            + (t3 - 2.0 * t2 + t) * span * tangents[index]
            + (-2.0 * t3 + 3.0 * t2) * y1
            + (t3 - t2) * span * tangents[index + 1]
    }
}

fn sampled(curve: &Curve) -> Vec<f32> {
    let mut points: Vec<(f32, f32)> = curve.0.iter().map(|point| (point.x, point.y)).collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    points.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-5);
    let spline = monotone_spline(&points);
    (0..LUT_SIZE)
        .map(|index| {
            let x = index as f32 / (LUT_SIZE - 1) as f32;
            if points.len() < 2 { x } else { spline(x).clamp(0.0, 1.0) }
        })
        .collect()
}

/// Lifts or lowers each tonal region. Sliders move the curve, region boundaries come from
/// the split points, and the result is kept monotonic.
fn parametric_points(curve: &ParametricCurve) -> Vec<(f32, f32)> {
    const REACH: f32 = 0.2;
    let [split_low, split_mid, split_high] =
        [curve.split_shadows, curve.split_midtones, curve.split_highlights].map(|v| v / 100.0);
    let knot = |x: f32, slider: f32| (x, (x + slider / 100.0 * REACH).clamp(0.0, 1.0));
    let mut points = vec![
        (0.0, 0.0),
        knot(split_low * 0.5, curve.shadows),
        knot((split_low + split_mid) * 0.5, curve.darks),
        (split_mid, split_mid + (curve.darks + curve.lights) / 200.0 * REACH * 0.5),
        knot((split_mid + split_high) * 0.5, curve.lights),
        knot((split_high + 1.0) * 0.5, curve.highlights),
        (1.0, 1.0),
    ];
    let mut highest = 0.0f32;
    for point in &mut points {
        point.1 = point.1.max(highest);
        highest = point.1;
    }
    points
}

/// RGBA per entry: red, green, blue, then the composite RGB curve in alpha.
pub fn build_lut(tone: &ToneCurve) -> Vec<[f32; 4]> {
    let points = parametric_points(&tone.parametric);
    let parametric = monotone_spline(&points);
    let rgb = sampled(&tone.rgb);
    let (red, green, blue) = (sampled(&tone.red), sampled(&tone.green), sampled(&tone.blue));
    (0..LUT_SIZE)
        .map(|index| {
            let x = index as f32 / (LUT_SIZE - 1) as f32;
            let shaped = parametric(x).clamp(0.0, 1.0);
            let composite = rgb[(shaped * (LUT_SIZE - 1) as f32).round() as usize];
            [red[index], green[index], blue[index], composite]
        })
        .collect()
}

pub fn is_identity(tone: &ToneCurve) -> bool {
    tone.parametric == ParametricCurve::default()
        && tone.rgb.is_identity()
        && tone.red.is_identity()
        && tone.green.is_identity()
        && tone.blue.is_identity()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_recipe::CurvePoint;

    fn curve(points: &[(f32, f32)]) -> Curve {
        Curve(points.iter().map(|&(x, y)| CurvePoint::new(x, y)).collect())
    }

    fn at(lut: &[[f32; 4]], x: f32, channel: usize) -> f32 {
        lut[(x * (LUT_SIZE - 1) as f32).round() as usize][channel]
    }

    #[test]
    fn neutral_curves_are_the_identity() {
        let lut = build_lut(&ToneCurve::default());
        for x in [0.0, 0.1, 0.33, 0.5, 0.8, 1.0] {
            for channel in 0..4 {
                assert!((at(&lut, x, channel) - x).abs() < 0.01, "{x} ch{channel}");
            }
        }
        assert!(is_identity(&ToneCurve::default()));
    }

    #[test]
    fn point_curve_passes_through_its_points() {
        let tone = ToneCurve {
            rgb: curve(&[(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)]),
            ..ToneCurve::default()
        };
        let lut = build_lut(&tone);
        assert!((at(&lut, 0.25, 3) - 0.15).abs() < 0.01);
        assert!((at(&lut, 0.75, 3) - 0.85).abs() < 0.01);
    }

    #[test]
    fn spline_never_overshoots_steep_points() {
        let tone = ToneCurve {
            red: curve(&[(0.0, 0.0), (0.45, 0.02), (0.55, 0.98), (1.0, 1.0)]),
            ..ToneCurve::default()
        };
        let lut = build_lut(&tone);
        let values: Vec<f32> = lut.iter().map(|entry| entry[0]).collect();
        assert!(values.windows(2).all(|pair| pair[1] >= pair[0] - 1e-5), "not monotonic");
    }

    #[test]
    fn channel_curves_only_touch_their_channel() {
        let tone = ToneCurve { blue: curve(&[(0.0, 0.2), (1.0, 0.8)]), ..ToneCurve::default() };
        let lut = build_lut(&tone);
        assert!((at(&lut, 0.0, 2) - 0.2).abs() < 0.01);
        assert!(at(&lut, 0.5, 0) - 0.5 < 0.01);
    }

    #[test]
    fn parametric_regions_move_their_own_tones() {
        let mut tone = ToneCurve::default();
        tone.parametric.shadows = 100.0;
        let lifted = build_lut(&tone);
        assert!(at(&lifted, 0.12, 3) > 0.2);
        assert!((at(&lifted, 0.95, 3) - 0.95).abs() < 0.05);

        let mut tone = ToneCurve::default();
        tone.parametric.highlights = -100.0;
        let lowered = build_lut(&tone);
        assert!(at(&lowered, 0.87, 3) < 0.8);
        assert!((at(&lowered, 0.1, 3) - 0.1).abs() < 0.05);
    }

    #[test]
    fn extreme_parametric_settings_stay_monotonic() {
        let mut tone = ToneCurve::default();
        tone.parametric.shadows = 100.0;
        tone.parametric.darks = -100.0;
        tone.parametric.lights = 100.0;
        tone.parametric.highlights = -100.0;
        let lut = build_lut(&tone);
        let values: Vec<f32> = lut.iter().map(|entry| entry[3]).collect();
        assert!(values.windows(2).all(|pair| pair[1] >= pair[0] - 1e-5));
    }

    #[test]
    fn duplicate_points_do_not_divide_by_zero() {
        let tone = ToneCurve {
            rgb: curve(&[(0.5, 0.2), (0.5, 0.8), (1.0, 1.0)]),
            ..ToneCurve::default()
        };
        assert!(build_lut(&tone).iter().flatten().all(|value| value.is_finite()));
    }
}
