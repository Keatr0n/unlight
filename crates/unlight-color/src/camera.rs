//! Camera calibration following the DNG specification's colour model. Non-DNG cameras are
//! treated as a one-illuminant profile, so there is a single code path.

use crate::{
    dcp::{Dcp, HueSatTable, ProfileCalibration},
    matrix::{
        IDENTITY, Mat3, Vec3, WHITE_D50, XYZ_D50_TO_PROPHOTO, apply, bradford, cct_from_xy,
        diagonal, inverse, lerp, mul, scale, xy_to_xyz, xyz_to_xy,
    },
    temperature::{temperature_from_xy, xy_from_temperature},
};
use std::{borrow::Cow, sync::Arc};
use unlight_raw::{ColorData, DngColor, DngIlluminant};

const WHITE_SEARCH_ITERATIONS: usize = 30;
const WHITE_SEARCH_TOLERANCE: f64 = 1e-7;
const D65_CCT: f64 = 6504.0;

#[derive(Clone, Copy, Debug)]
struct Calibration {
    cct: f64,
    xyz_to_camera: Mat3,
    forward: Option<Mat3>,
    camera_calibration: Mat3,
}

/// Converts camera space to the working space for a given scene white.
#[derive(Clone, Debug)]
pub struct CameraProfile {
    calibrations: Vec<Calibration>,
    analog_balance: Mat3,
    as_shot_neutral: Vec3,
    rendering: Option<Arc<Dcp>>,
}

/// Colour temperature of an EXIF LightSource code.
fn illuminant_cct(code: u16) -> f64 {
    match code {
        1 | 4 | 20 => 5503.0,
        2 | 14 => 4230.0,
        3 | 17 => 2856.0,
        9 | 18 => 5500.0,
        10 | 22 => 7504.0,
        11 | 21 => 6504.0,
        12 => 6430.0,
        13 => 5000.0,
        15 => 3450.0,
        19 => 6774.0,
        23 => 5003.0,
        24 => 3200.0,
        _ => D65_CCT,
    }
}

fn to_f64(matrix: &[[f32; 3]; 3]) -> Mat3 {
    matrix.map(|row| row.map(f64::from))
}

fn from_dng(illuminant: &DngIlluminant) -> Calibration {
    Calibration {
        cct: illuminant_cct(illuminant.illuminant),
        xyz_to_camera: to_f64(&illuminant.color_matrix),
        forward: illuminant.forward_matrix.as_ref().map(to_f64),
        camera_calibration: to_f64(&illuminant.calibration),
    }
}

fn from_profile(calibration: &ProfileCalibration) -> Option<Calibration> {
    Some(Calibration {
        cct: illuminant_cct(calibration.illuminant),
        xyz_to_camera: calibration.color_matrix?,
        forward: calibration.forward_matrix,
        camera_calibration: IDENTITY,
    })
}

fn neutral_from_multipliers(multipliers: &[f32; 3]) -> Vec3 {
    multipliers.map(|gain| 1.0 / f64::from(gain))
}

/// How much of the lower of two temperatures a scene gets. Colour responds evenly to mired
/// rather than to kelvin.
fn mired_weight(cct: f64, low: f64, high: f64) -> f64 {
    if low == high {
        return 1.0;
    }
    ((1.0 / cct - 1.0 / high) / (1.0 / low - 1.0 / high)).clamp(0.0, 1.0)
}

fn blend_tables(low: &HueSatTable, high: &HueSatTable, low_weight: f32) -> HueSatTable {
    let blend = |a: &[f32; 3], b: &[f32; 3]| {
        std::array::from_fn(|index| a[index] * low_weight + b[index] * (1.0 - low_weight))
    };
    HueSatTable {
        entries: low.entries.iter().zip(&high.entries).map(|(a, b)| blend(a, b)).collect(),
        ..low.clone()
    }
}

impl CameraProfile {
    pub fn new(color: &ColorData) -> Self {
        let as_shot_neutral = neutral_from_multipliers(&color.as_shot_multipliers);
        match &color.dng {
            Some(DngColor { illuminants, analog_balance, as_shot_neutral: dng_neutral }) => {
                let mut calibrations: Vec<_> = illuminants.iter().map(from_dng).collect();
                calibrations.sort_by(|a, b| a.cct.total_cmp(&b.cct));
                Self {
                    calibrations,
                    analog_balance: diagonal(&analog_balance.map(f64::from)),
                    as_shot_neutral: dng_neutral.map_or(as_shot_neutral, |n| n.map(f64::from)),
                    rendering: None,
                }
            }
            None => Self {
                calibrations: vec![Calibration {
                    cct: D65_CCT,
                    xyz_to_camera: to_f64(&color.xyz_to_camera),
                    forward: None,
                    camera_calibration: IDENTITY,
                }],
                analog_balance: IDENTITY,
                as_shot_neutral,
                rendering: None,
            },
        }
    }

    /// A profile from outside the file: its matrices replace the file's, and CameraCalibration
    /// is dropped because that describes this unit against the file's own profile.
    pub fn with_dcp(color: &ColorData, profile: Arc<Dcp>) -> Self {
        let mut calibrations: Vec<_> = profile.calibrations.iter().filter_map(from_profile).collect();
        calibrations.sort_by(|a, b| a.cct.total_cmp(&b.cct));
        let mut camera = Self::with_embedded(color, profile);
        if !calibrations.is_empty() {
            camera.calibrations = calibrations;
        }
        camera
    }

    /// A profile stored in the raw file, whose matrices the decoder has already supplied.
    pub fn with_embedded(color: &ColorData, profile: Arc<Dcp>) -> Self {
        Self { rendering: Some(profile), ..Self::new(color) }
    }

    /// The profile's tables and tone curve, if one is in use.
    pub fn rendering(&self) -> Option<&Dcp> {
        self.rendering.as_deref()
    }

    pub fn as_shot_neutral(&self) -> Vec3 {
        self.as_shot_neutral
    }

    /// Mired-weighted blend of the calibrations bracketing `cct`.
    fn calibration_at(&self, cct: f64) -> Calibration {
        let [low, high] = match self.calibrations.as_slice() {
            [only] => return *only,
            [low, .., high] => [low, high],
            [] => unreachable!("a profile always has a calibration"),
        };
        let weight = mired_weight(cct, low.cct, high.cct);
        let blend = |a: &Mat3, b: &Mat3| lerp(b, a, weight);
        Calibration {
            cct,
            xyz_to_camera: blend(&low.xyz_to_camera, &high.xyz_to_camera),
            forward: low.forward.zip(high.forward).map(|(a, b)| blend(&a, &b)),
            camera_calibration: blend(&low.camera_calibration, &high.camera_calibration),
        }
    }

    fn balance(&self, calibration: &Calibration) -> Mat3 {
        mul(&self.analog_balance, &calibration.camera_calibration)
    }

    /// The DNG white search: a neutral's colour depends on the matrix, and the matrix on
    /// its colour temperature, so the two are iterated together.
    fn scene_white(&self, neutral: &Vec3) -> Option<(Calibration, Vec3)> {
        let mut xy = xyz_to_xy(&WHITE_D50);
        let mut calibration = self.calibration_at(cct_from_xy(&xy));
        let mut white = xy_to_xyz(&xy);
        for _ in 0..WHITE_SEARCH_ITERATIONS {
            calibration = self.calibration_at(cct_from_xy(&xy));
            let xyz_to_camera = mul(&self.balance(&calibration), &calibration.xyz_to_camera);
            white = apply(&inverse(&xyz_to_camera)?, neutral);
            let next = xyz_to_xy(&white);
            let moved = (next[0] - xy[0]).abs() + (next[1] - xy[1]).abs();
            xy = [(xy[0] + next[0]) * 0.5, (xy[1] + next[1]) * 0.5];
            if moved < WHITE_SEARCH_TOLERANCE {
                break;
            }
        }
        Some((calibration, white))
    }

    fn camera_to_xyz_d50(&self, neutral: &Vec3) -> Option<Mat3> {
        let (calibration, white) = self.scene_white(neutral)?;
        let balance = self.balance(&calibration);
        match calibration.forward {
            Some(forward) => {
                let unbalance = inverse(&balance)?;
                let reference = apply(&unbalance, neutral);
                let whitening = diagonal(&reference.map(|value| 1.0 / value));
                Some(mul(&forward, &mul(&whitening, &unbalance)))
            }
            None => {
                let xyz_to_camera = mul(&balance, &calibration.xyz_to_camera);
                Some(mul(&bradford(&white, &WHITE_D50), &inverse(&xyz_to_camera)?))
            }
        }
    }

    /// The profile's HueSatMap for a scene white, by the same mired weight as the matrices.
    pub fn hue_sat_map(&self, neutral: &Vec3) -> Option<Cow<'_, HueSatTable>> {
        let maps = self.rendering()?.calibrations.iter().filter_map(|calibration| {
            calibration.hue_sat_map.as_ref().map(|map| (illuminant_cct(calibration.illuminant), map))
        });
        let mut maps: Vec<_> = maps.collect();
        maps.sort_by(|a, b| a.0.total_cmp(&b.0));
        match maps.as_slice() {
            [] => None,
            [(_, only)] => Some(Cow::Borrowed(*only)),
            [(low_cct, low), .., (high_cct, high)] => {
                let cct = self.scene_white(neutral).map_or(D65_CCT, |(_, white)| cct_from_xy(&xyz_to_xy(&white)));
                let weight = mired_weight(cct, *low_cct, *high_cct) as f32;
                Some(Cow::Owned(blend_tables(low, high, weight)))
            }
        }
    }

    /// The camera neutral that makes a light of this temperature and tint look white.
    pub fn neutral_for_temperature(&self, temperature: f64, tint: f64) -> Vec3 {
        let xy = xy_from_temperature(temperature, tint);
        let calibration = self.calibration_at(temperature);
        let xyz_to_camera = mul(&self.balance(&calibration), &calibration.xyz_to_camera);
        let neutral = apply(&xyz_to_camera, &xy_to_xyz(&xy));
        let green = neutral[1].max(1e-9);
        neutral.map(|value| (value / green).max(1e-6))
    }

    /// The temperature and tint a camera neutral corresponds to, for showing in the UI.
    pub fn temperature_for_neutral(&self, neutral: &Vec3) -> (f64, f64) {
        match self.scene_white(neutral) {
            Some((_, white)) => temperature_from_xy(&xyz_to_xy(&white)),
            None => (D65_CCT, 0.0),
        }
    }

    /// Camera to linear ProPhoto for a scene white, scaled so `neutral` maps to (1, 1, 1).
    /// Falls back to a plain channel gain if the file's matrices are unusable.
    pub fn camera_to_prophoto(&self, neutral: &Vec3) -> Mat3 {
        let Some(camera_to_xyz) = self.camera_to_xyz_d50(neutral) else {
            return diagonal(&neutral.map(|value| 1.0 / value));
        };
        let matrix = mul(&XYZ_D50_TO_PROPHOTO, &camera_to_xyz);
        let white = apply(&matrix, neutral);
        scale(&matrix, 3.0 / (white[0] + white[1] + white[2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dcp::testing::complete,
        matrix::{WHITE_D65, XYZ_D65_TO_SRGB},
    };

    fn color_data(xyz_to_camera: [[f32; 3]; 3], multipliers: [f32; 3]) -> ColorData {
        ColorData {
            xyz_to_camera,
            as_shot_multipliers: multipliers,
            daylight_multipliers: multipliers,
            baseline_exposure: 0.0,
            dng: None,
        }
    }

    fn to_f32(matrix: &Mat3) -> [[f32; 3]; 3] {
        matrix.map(|row| row.map(|value| value as f32))
    }

    fn sample_matrix() -> [[f32; 3]; 3] {
        [[0.8293, -0.1789, -0.1094], [-0.5025, 1.2926, 0.2436], [-0.0894, 0.1886, 0.6084]]
    }

    fn assert_neutral_within(value: &Vec3, tolerance: f64) {
        for channel in value {
            assert!((channel - 1.0).abs() < tolerance, "{value:?} is not neutral");
        }
    }

    fn assert_neutral(value: &Vec3) {
        assert_neutral_within(value, 1e-6);
    }

    #[test]
    fn as_shot_neutral_maps_to_prophoto_white() {
        let profile = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        let neutral = profile.as_shot_neutral();
        assert_neutral(&apply(&profile.camera_to_prophoto(&neutral), &neutral));
    }

    #[test]
    fn any_neutral_maps_to_prophoto_white() {
        let profile = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        for neutral in [[0.4, 1.0, 0.9], [0.7, 1.0, 0.5], [0.33, 1.0, 1.2]] {
            assert_neutral(&apply(&profile.camera_to_prophoto(&neutral), &neutral));
        }
    }

    #[test]
    fn srgb_like_camera_reproduces_primaries() {
        let xyz_to_camera = to_f32(&XYZ_D65_TO_SRGB);
        let profile = CameraProfile::new(&color_data(xyz_to_camera, [1.0, 1.0, 1.0]));
        let to_prophoto = profile.camera_to_prophoto(&[1.0, 1.0, 1.0]);
        let to_srgb = mul(&crate::matrix::prophoto_to_srgb(), &to_prophoto);
        for row in 0..3 {
            for col in 0..3 {
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!((to_srgb[row][col] - expected).abs() < 5e-3, "{to_srgb:?}");
            }
        }
    }

    fn dng_data(forward: Option<[[f32; 3]; 3]>) -> ColorData {
        let warm = [[1.0, -0.2, -0.1], [-0.4, 1.2, 0.2], [-0.05, 0.1, 0.5]];
        let illuminant = |code, matrix| DngIlluminant {
            illuminant: code,
            color_matrix: matrix,
            forward_matrix: forward,
            calibration: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        ColorData {
            dng: Some(DngColor {
                illuminants: vec![illuminant(21, sample_matrix()), illuminant(17, warm)],
                analog_balance: [1.0; 3],
                as_shot_neutral: Some([0.52, 1.0, 0.59]),
            }),
            ..color_data([[0.0; 3]; 3], [1.9, 1.0, 1.7])
        }
    }

    #[test]
    fn dng_prefers_the_as_shot_neutral_tag() {
        let profile = CameraProfile::new(&dng_data(None));
        assert_eq!(profile.as_shot_neutral(), [0.52f32 as f64, 1.0, 0.59f32 as f64]);
    }

    #[test]
    fn dng_interpolates_between_illuminants_by_mired() {
        let profile = CameraProfile::new(&dng_data(None));
        let warm = profile.calibration_at(2856.0).xyz_to_camera;
        let cool = profile.calibration_at(6504.0).xyz_to_camera;
        let cooler = profile.calibration_at(9000.0).xyz_to_camera;
        let middle = profile.calibration_at(3970.0).xyz_to_camera;
        assert_eq!(cool, cooler, "outside the bracket the weight clamps");
        assert!((warm[0][0] - 1.0).abs() < 1e-5);
        assert!((cool[0][0] - 0.8293).abs() < 1e-5);
        assert!((middle[0][0] - 0.9146).abs() < 1e-2);
    }

    #[test]
    fn dng_neutral_maps_to_white_with_and_without_forward_matrix() {
        let d50_srgb: [[f32; 3]; 3] =
            [[0.4360, 0.3851, 0.1431], [0.2225, 0.7169, 0.0606], [0.0139, 0.0971, 0.7139]];
        // A forward matrix only reaches D50 to its own printed precision.
        for (data, tolerance) in [(dng_data(None), 1e-6), (dng_data(Some(d50_srgb)), 1e-3)] {
            let profile = CameraProfile::new(&data);
            let neutral = profile.as_shot_neutral();
            let white = apply(&profile.camera_to_prophoto(&neutral), &neutral);
            assert_neutral_within(&white, tolerance);
        }
    }

    #[test]
    fn temperature_round_trips_through_the_camera_matrix() {
        let profile = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        for (temperature, tint) in [(3200.0, 10.0), (5500.0, 0.0), (7500.0, -15.0)] {
            let neutral = profile.neutral_for_temperature(temperature, tint);
            let (found, found_tint) = profile.temperature_for_neutral(&neutral);
            assert!((found - temperature).abs() < temperature * 0.01, "{temperature} → {found}");
            assert!((found_tint - tint).abs() < 2.0, "{tint} → {found_tint}");
        }
    }

    #[test]
    fn warmer_white_balance_setting_means_a_bluer_neutral() {
        let profile = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        let tungsten = profile.neutral_for_temperature(3000.0, 0.0);
        let shade = profile.neutral_for_temperature(8000.0, 0.0);
        assert!(tungsten[0] / tungsten[2] > shade[0] / shade[2]);
    }

    #[test]
    fn unusable_matrix_falls_back_to_channel_gains() {
        let profile = CameraProfile::new(&color_data([[0.0; 3]; 3], [2.0, 1.0, 1.5]));
        let neutral = profile.as_shot_neutral();
        assert_eq!(apply(&profile.camera_to_prophoto(&neutral), &neutral), [1.0, 1.0, 1.0]);
    }

    fn with_test_profile() -> CameraProfile {
        let profile = Dcp::parse(&complete(b"IIRC")).unwrap();
        CameraProfile::with_dcp(&color_data(sample_matrix(), [2.0, 1.0, 1.5]), Arc::new(profile))
    }

    fn table(value: f32, srgb_values: bool) -> HueSatTable {
        HueSatTable { divisions: [1, 2, 1], srgb_values, entries: vec![[value, 1.0, 1.0]; 2] }
    }

    #[test]
    fn a_dcp_replaces_the_file_matrices() {
        let plain = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        let neutral = plain.as_shot_neutral();
        let profiled = with_test_profile();
        assert_ne!(plain.camera_to_prophoto(&neutral), profiled.camera_to_prophoto(&neutral));
        assert_neutral_within(&apply(&profiled.camera_to_prophoto(&neutral), &neutral), 1e-3);
    }

    #[test]
    fn a_dcp_with_no_matrices_keeps_the_file_matrices() {
        let color = color_data(sample_matrix(), [2.0, 1.0, 1.5]);
        let mut profile = Dcp::parse(&complete(b"IIRC")).unwrap();
        for calibration in &mut profile.calibrations {
            calibration.color_matrix = None;
        }
        let plain = CameraProfile::new(&color);
        let neutral = plain.as_shot_neutral();
        let profiled = CameraProfile::with_dcp(&color, Arc::new(profile));
        assert_eq!(plain.camera_to_prophoto(&neutral), profiled.camera_to_prophoto(&neutral));
    }

    #[test]
    fn an_embedded_profile_keeps_the_camera_calibration() {
        let mut data = dng_data(None);
        if let Some(dng) = &mut data.dng {
            dng.illuminants[0].calibration[0][0] = 1.2;
            dng.illuminants[1].calibration[0][0] = 1.2;
        }
        let plain = CameraProfile::new(&data);
        let neutral = plain.as_shot_neutral();
        let profile = Arc::new(Dcp::parse(&complete(b"IIRC")).unwrap());
        let embedded = CameraProfile::with_embedded(&data, profile);
        assert_eq!(plain.camera_to_prophoto(&neutral), embedded.camera_to_prophoto(&neutral));
    }

    #[test]
    fn hue_sat_maps_blend_by_the_same_mired_weight_as_the_matrices() {
        let mut profile = Dcp::parse(&complete(b"IIRC")).unwrap();
        profile.calibrations[0].hue_sat_map = Some(table(10.0, false));
        profile.calibrations[1].hue_sat_map = Some(table(30.0, false));
        let color = color_data(sample_matrix(), [2.0, 1.0, 1.5]);
        let camera = CameraProfile::with_dcp(&color, Arc::new(profile));

        let shift_at = |temperature: f64| {
            let neutral = camera.neutral_for_temperature(temperature, 0.0);
            camera.hue_sat_map(&neutral).unwrap().entries[0][0]
        };
        assert!((shift_at(2856.0) - 10.0).abs() < 0.5, "{}", shift_at(2856.0));
        assert!((shift_at(6504.0) - 30.0).abs() < 0.5, "{}", shift_at(6504.0));
        let halfway = 2.0 / (1.0 / 2856.0 + 1.0 / 6504.0);
        assert!((shift_at(halfway) - 20.0).abs() < 1.0, "{}", shift_at(halfway));
    }

    #[test]
    fn a_single_hue_sat_map_needs_no_blending() {
        let mut profile = Dcp::parse(&complete(b"IIRC")).unwrap();
        profile.calibrations[1].hue_sat_map = None;
        let color = color_data(sample_matrix(), [2.0, 1.0, 1.5]);
        let camera = CameraProfile::with_dcp(&color, Arc::new(profile));
        let map = camera.hue_sat_map(&camera.as_shot_neutral()).unwrap();
        assert!(matches!(map, Cow::Borrowed(_)));
    }

    #[test]
    fn two_illuminants_at_one_temperature_do_not_divide_by_zero() {
        assert_eq!(mired_weight(5000.0, 6504.0, 6504.0), 1.0);
    }

    #[test]
    fn no_profile_means_no_tables() {
        let camera = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        assert!(camera.rendering().is_none());
        assert!(camera.hue_sat_map(&camera.as_shot_neutral()).is_none());
    }

    #[test]
    fn d65_scene_white_matches_d65() {
        let profile = CameraProfile::new(&color_data(sample_matrix(), [2.0, 1.0, 1.5]));
        let camera = apply(&to_f64(&sample_matrix()), &WHITE_D65);
        let (_, white) = profile.scene_white(&camera).unwrap();
        let xy = xyz_to_xy(&white);
        let expected = xyz_to_xy(&WHITE_D65);
        assert!((xy[0] - expected[0]).abs() < 1e-5 && (xy[1] - expected[1]).abs() < 1e-5);
    }
}
