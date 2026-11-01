//! Correlated colour temperature and tint, by Robertson's isotherm method. Tint is measured
//! along the isotherm using the DNG convention: these describe the light, so a positive tint
//! means a greener light, and correcting for it renders the photo more magenta.

use crate::matrix::Vec3;

const TINT_SCALE: f64 = -3000.0;

struct Isotherm {
    mired: f64,
    u: f64,
    v: f64,
    slope: f64,
}

const fn isotherm(mired: f64, u: f64, v: f64, slope: f64) -> Isotherm {
    Isotherm { mired, u, v, slope }
}

/// Wyszecki and Stiles' Robertson table, in CIE 1960 UCS coordinates.
const ISOTHERMS: [Isotherm; 31] = [
    isotherm(0.0, 0.18006, 0.26352, -0.24341),
    isotherm(10.0, 0.18066, 0.26589, -0.25479),
    isotherm(20.0, 0.18133, 0.26846, -0.26876),
    isotherm(30.0, 0.18208, 0.27119, -0.28539),
    isotherm(40.0, 0.18293, 0.27407, -0.30470),
    isotherm(50.0, 0.18388, 0.27709, -0.32675),
    isotherm(60.0, 0.18494, 0.28021, -0.35156),
    isotherm(70.0, 0.18611, 0.28342, -0.37915),
    isotherm(80.0, 0.18740, 0.28668, -0.40955),
    isotherm(90.0, 0.18880, 0.28997, -0.44278),
    isotherm(100.0, 0.19032, 0.29326, -0.47888),
    isotherm(125.0, 0.19462, 0.30141, -0.58204),
    isotherm(150.0, 0.19962, 0.30921, -0.70471),
    isotherm(175.0, 0.20525, 0.31647, -0.84901),
    isotherm(200.0, 0.21142, 0.32312, -1.0182),
    isotherm(225.0, 0.21807, 0.32909, -1.2168),
    isotherm(250.0, 0.22511, 0.33439, -1.4512),
    isotherm(275.0, 0.23247, 0.33904, -1.7298),
    isotherm(300.0, 0.24010, 0.34308, -2.0637),
    isotherm(325.0, 0.24702, 0.34655, -2.4681),
    isotherm(350.0, 0.25591, 0.34951, -2.9641),
    isotherm(375.0, 0.26400, 0.35200, -3.5814),
    isotherm(400.0, 0.27218, 0.35407, -4.3633),
    isotherm(425.0, 0.28039, 0.35577, -5.3762),
    isotherm(450.0, 0.28863, 0.35714, -6.7262),
    isotherm(475.0, 0.29685, 0.35823, -8.5955),
    isotherm(500.0, 0.30505, 0.35907, -11.324),
    isotherm(525.0, 0.31320, 0.35968, -15.628),
    isotherm(550.0, 0.32129, 0.36011, -23.325),
    isotherm(575.0, 0.32931, 0.36038, -40.770),
    isotherm(600.0, 0.33724, 0.36051, -116.45),
];

fn unit_slope(slope: f64) -> [f64; 2] {
    let length = (1.0 + slope * slope).sqrt();
    [1.0 / length, slope / length]
}

/// Chromaticity of a temperature in kelvin and a tint.
pub fn xy_from_temperature(temperature: f64, tint: f64) -> [f64; 2] {
    let mired = 1e6 / temperature;
    let offset = tint / TINT_SCALE;
    let index = (0..ISOTHERMS.len() - 2)
        .find(|&i| mired < ISOTHERMS[i + 1].mired)
        .unwrap_or(ISOTHERMS.len() - 2);
    let (a, b) = (&ISOTHERMS[index], &ISOTHERMS[index + 1]);
    let weight = (b.mired - mired) / (b.mired - a.mired);
    let mut u = a.u * weight + b.u * (1.0 - weight);
    let mut v = a.v * weight + b.v * (1.0 - weight);

    let (slope_a, slope_b) = (unit_slope(a.slope), unit_slope(b.slope));
    let along = [
        slope_a[0] * weight + slope_b[0] * (1.0 - weight),
        slope_a[1] * weight + slope_b[1] * (1.0 - weight),
    ];
    let length = along[0].hypot(along[1]);
    u += along[0] / length * offset;
    v += along[1] / length * offset;

    let denominator = u - 4.0 * v + 2.0;
    [1.5 * u / denominator, v / denominator]
}

/// The temperature and tint of a chromaticity.
pub fn temperature_from_xy(xy: &[f64; 2]) -> (f64, f64) {
    let denominator = 1.5 - xy[0] + 6.0 * xy[1];
    let (u, v) = (2.0 * xy[0] / denominator, 3.0 * xy[1] / denominator);

    let mut last_distance = 0.0;
    let mut last_direction = [0.0; 2];
    for (index, line) in ISOTHERMS.iter().enumerate() {
        let direction = unit_slope(line.slope);
        let (du, dv) = (u - line.u, v - line.v);
        let distance = dv * direction[0] - du * direction[1];
        if (distance <= 0.0 && index > 0) || index == ISOTHERMS.len() - 1 {
            let previous = &ISOTHERMS[index - 1];
            let span = last_distance - distance;
            let weight = if span.abs() < 1e-12 { 0.0 } else { last_distance / span };
            let mired = previous.mired + (line.mired - previous.mired) * weight;

            let along = [
                last_direction[0] * (1.0 - weight) + direction[0] * weight,
                last_direction[1] * (1.0 - weight) + direction[1] * weight,
            ];
            let length = along[0].hypot(along[1]);
            let locus_u = previous.u + (line.u - previous.u) * weight;
            let locus_v = previous.v + (line.v - previous.v) * weight;
            let offset = (u - locus_u) * along[0] / length + (v - locus_v) * along[1] / length;
            return (1e6 / mired.max(1e-3), offset * TINT_SCALE);
        }
        last_distance = distance;
        last_direction = direction;
    }
    unreachable!("the loop returns on its final isotherm")
}

/// The chromaticity as XYZ with Y = 1.
pub fn xyz_from_temperature(temperature: f64, tint: f64) -> Vec3 {
    crate::matrix::xy_to_xyz(&xy_from_temperature(temperature, tint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{WHITE_D50, WHITE_D65, xyz_to_xy};

    #[test]
    fn known_illuminants_have_their_published_temperatures() {
        let (d65, tint_d65) = temperature_from_xy(&xyz_to_xy(&WHITE_D65));
        let (d50, tint_d50) = temperature_from_xy(&xyz_to_xy(&WHITE_D50));
        assert!((d65 - 6504.0).abs() < 25.0, "D65 gave {d65}");
        assert!((d50 - 5003.0).abs() < 25.0, "D50 gave {d50}");
        assert!(tint_d65.abs() < 15.0 && tint_d50.abs() < 15.0, "{tint_d65} {tint_d50}");
    }

    #[test]
    fn temperature_and_tint_round_trip() {
        for temperature in [2300.0, 3200.0, 4100.0, 5500.0, 6500.0, 9000.0, 15000.0] {
            for tint in [-100.0, -20.0, 0.0, 35.0, 120.0] {
                let (found, found_tint) = temperature_from_xy(&xy_from_temperature(temperature, tint));
                assert!((found - temperature).abs() < temperature * 0.003, "{temperature} → {found}");
                assert!((found_tint - tint).abs() < 1.0, "{tint} → {found_tint}");
            }
        }
    }

    #[test]
    fn positive_tint_assumes_a_greener_light_which_renders_magenta() {
        let neutral = xy_from_temperature(5000.0, 0.0);
        let tinted = xy_from_temperature(5000.0, 50.0);
        assert!(tinted[1] > neutral[1]);
    }

    #[test]
    fn warmer_light_has_a_lower_temperature_and_redder_white() {
        let warm = xy_from_temperature(3000.0, 0.0);
        let cool = xy_from_temperature(8000.0, 0.0);
        assert!(warm[0] > cool[0]);
    }
}
