//! CIELAB and CIEDE2000, used to gate preview-versus-export and precision differences.

use std::f64::consts::PI;

const LAB_EPSILON: f64 = 216.0 / 24389.0;
const LAB_KAPPA: f64 = 24389.0 / 27.0;
const WHITE_D65: [f64; 3] = [0.950_47, 1.0, 1.088_83];
const SRGB_TO_XYZ_D65: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];

fn srgb_decode(value: f64) -> f64 {
    if value <= 0.040_45 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

/// Converts gamma-encoded sRGB to CIELAB relative to D65.
pub fn srgb_to_lab(rgb: [f32; 3]) -> [f64; 3] {
    let linear = rgb.map(|value| srgb_decode(f64::from(value)));
    let xyz: [f64; 3] = std::array::from_fn(|row| {
        (0..3).map(|col| SRGB_TO_XYZ_D65[row][col] * linear[col]).sum()
    });
    let f = |index: usize| {
        let ratio = xyz[index] / WHITE_D65[index];
        if ratio > LAB_EPSILON { ratio.cbrt() } else { (LAB_KAPPA * ratio + 16.0) / 116.0 }
    };
    let (fx, fy, fz) = (f(0), f(1), f(2));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn hue_angle(b: f64, a: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        return 0.0;
    }
    let degrees = b.atan2(a).to_degrees();
    if degrees < 0.0 { degrees + 360.0 } else { degrees }
}

pub fn delta_e_2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;
    let chroma_mean = (a1.hypot(b1) + a2.hypot(b2)) * 0.5;
    let g = 0.5 * (1.0 - (chroma_mean.powi(7) / (chroma_mean.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let (h1p, h2p) = (hue_angle(b1, a1p), hue_angle(b2, a2p));

    let delta_l = l2 - l1;
    let delta_c = c2p - c1p;
    let hue_gap = h2p - h1p;
    let delta_h_angle = if c1p * c2p == 0.0 {
        0.0
    } else if hue_gap.abs() <= 180.0 {
        hue_gap
    } else if hue_gap > 180.0 {
        hue_gap - 360.0
    } else {
        hue_gap + 360.0
    };
    let delta_h = 2.0 * (c1p * c2p).sqrt() * (delta_h_angle.to_radians() * 0.5).sin();

    let l_mean = (l1 + l2) * 0.5;
    let c_mean = (c1p + c2p) * 0.5;
    let h_sum = h1p + h2p;
    let h_mean = if c1p * c2p == 0.0 {
        h_sum
    } else if (h1p - h2p).abs() <= 180.0 {
        h_sum * 0.5
    } else if h_sum < 360.0 {
        (h_sum + 360.0) * 0.5
    } else {
        (h_sum - 360.0) * 0.5
    };

    let t = 1.0 - 0.17 * (h_mean - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_mean).to_radians().cos()
        + 0.32 * (3.0 * h_mean + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_mean - 63.0).to_radians().cos();
    let rotation_angle = 30.0 * (-((h_mean - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (c_mean.powi(7) / (c_mean.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_mean - 50.0).powi(2) / (20.0 + (l_mean - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_mean;
    let sh = 1.0 + 0.015 * c_mean * t;
    let rt = -(2.0 * rotation_angle * PI / 180.0).sin() * rc;

    let (dl, dc, dh) = (delta_l / sl, delta_c / sc, delta_h / sh);
    (dl * dl + dc * dc + dh * dh + rt * dc * dh).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference pairs from Sharma, Wu and Dalal's CIEDE2000 test data.
    #[test]
    fn matches_published_reference_values() {
        let pairs = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.5, 0.0], [50.0, 0.0, -2.5], 4.3065),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([22.7233, 20.0904, -46.694], [23.0331, 14.973, -42.5619], 2.0373),
            ([2.0776, 0.0795, -1.135], [0.9033, -0.0636, -0.5514], 0.9082),
        ];
        for (a, b, expected) in pairs {
            let value = delta_e_2000(a, b);
            assert!((value - expected).abs() < 1e-4, "{a:?} vs {b:?}: {value}");
        }
    }

    #[test]
    fn identical_colours_have_no_difference() {
        let lab = srgb_to_lab([0.2, 0.5, 0.8]);
        assert_eq!(delta_e_2000(lab, lab), 0.0);
    }

    #[test]
    fn srgb_white_is_lab_white() {
        let [l, a, b] = srgb_to_lab([1.0, 1.0, 1.0]);
        assert!((l - 100.0).abs() < 1e-2 && a.abs() < 0.02 && b.abs() < 0.02, "{l} {a} {b}");
    }
}
