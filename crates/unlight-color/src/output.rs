//! Colour spaces a finished image can be delivered in.

use crate::matrix::{self, Mat3, Vec3};

const D65_XY: [f64; 2] = [0.312_7, 0.329_0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransferFunction {
    /// The IEC 61966-2-1 curve, also used by Display P3.
    Srgb,
    /// A pure power law: `encoded = linear^(1 / gamma)`.
    Gamma(f64),
}

impl TransferFunction {
    /// Linear light to encoded. Negative input clips to zero.
    pub fn encode(self, linear: f32) -> f32 {
        let linear = linear.max(0.0);
        match self {
            Self::Srgb if linear <= 0.003_130_8 => linear * 12.92,
            Self::Srgb => 1.055 * linear.powf(1.0 / 2.4) - 0.055,
            Self::Gamma(gamma) => linear.powf(1.0 / gamma as f32),
        }
    }

    /// Encoded to linear light. Negative input clips to zero.
    pub fn decode(self, encoded: f32) -> f32 {
        let encoded = encoded.max(0.0);
        match self {
            Self::Srgb if encoded <= 0.040_45 => encoded / 12.92,
            Self::Srgb => ((encoded + 0.055) / 1.055).powf(2.4),
            Self::Gamma(gamma) => encoded.powf(gamma as f32),
        }
    }
}

/// CIE xy chromaticities of the three primaries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primaries {
    pub red: [f64; 2],
    pub green: [f64; 2],
    pub blue: [f64; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputSpace {
    #[default]
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
}

impl OutputSpace {
    pub const ALL: [Self; 4] = [Self::Srgb, Self::DisplayP3, Self::AdobeRgb, Self::ProPhoto];

    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::DisplayP3 => "Display P3",
            Self::AdobeRgb => "Adobe RGB (1998)",
            Self::ProPhoto => "ProPhoto RGB",
        }
    }

    pub fn primaries(self) -> Primaries {
        match self {
            Self::Srgb => Primaries {
                red: [0.64, 0.33],
                green: [0.30, 0.60],
                blue: [0.15, 0.06],
            },
            Self::DisplayP3 => Primaries {
                red: [0.68, 0.32],
                green: [0.265, 0.69],
                blue: [0.15, 0.06],
            },
            Self::AdobeRgb => Primaries {
                red: [0.64, 0.33],
                green: [0.21, 0.71],
                blue: [0.15, 0.06],
            },
            Self::ProPhoto => Primaries {
                red: [0.734_7, 0.265_3],
                green: [0.159_6, 0.840_4],
                blue: [0.036_6, 0.000_1],
            },
        }
    }

    pub fn white_xy(self) -> [f64; 2] {
        match self {
            Self::ProPhoto => matrix::xyz_to_xy(&matrix::WHITE_D50),
            _ => D65_XY,
        }
    }

    pub fn transfer(self) -> TransferFunction {
        match self {
            Self::Srgb | Self::DisplayP3 => TransferFunction::Srgb,
            Self::AdobeRgb => TransferFunction::Gamma(563.0 / 256.0),
            // 1.8 as the ICC v2 fixed-point field stores it, which is the curve readers of an
            // embedded profile apply. A pure power, because those profiles have no ROMM toe.
            Self::ProPhoto => TransferFunction::Gamma(461.0 / 256.0),
        }
    }

    /// Linear RGB in this space to XYZ relative to its own white.
    pub fn matrix_to_xyz(self) -> Mat3 {
        let Primaries { red, green, blue } = self.primaries();
        let columns = [red, green, blue].map(|xy| matrix::xy_to_xyz(&xy));
        let unscaled: Mat3 =
            std::array::from_fn(|row| std::array::from_fn(|col| columns[col][row]));
        let inverse = matrix::inverse(&unscaled).expect("primaries are linearly independent");
        let gains = matrix::apply(&inverse, &matrix::xy_to_xyz(&self.white_xy()));
        matrix::mul(&unscaled, &matrix::diagonal(&gains))
    }

    /// Linear ProPhoto (D50) to linear RGB in this space.
    pub fn matrix_from_prophoto(self) -> Mat3 {
        let prophoto_to_xyz =
            matrix::inverse(&matrix::XYZ_D50_TO_PROPHOTO).expect("ProPhoto matrix is invertible");
        let white = matrix::xy_to_xyz(&self.white_xy());
        let adapted = matrix::mul(
            &matrix::bradford(&matrix::WHITE_D50, &white),
            &prophoto_to_xyz,
        );
        let xyz_to_self =
            matrix::inverse(&self.matrix_to_xyz()).expect("primaries are linearly independent");
        matrix::mul(&xyz_to_self, &adapted)
    }

    /// Weights that turn linear RGB in this space into relative luminance.
    pub fn luminance_weights(self) -> Vec3 {
        self.matrix_to_xyz()[1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::{IDENTITY, WHITE_D50, apply, prophoto_to_srgb};

    fn assert_matrix_close(a: &Mat3, b: &Mat3, tolerance: f64) {
        for row in 0..3 {
            for col in 0..3 {
                assert!(
                    (a[row][col] - b[row][col]).abs() < tolerance,
                    "{a:?} != {b:?}"
                );
            }
        }
    }

    fn assert_vec_close(a: &Vec3, b: &Vec3, tolerance: f64) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < tolerance, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn matrices_match_published_rgb_to_xyz() {
        // Rounded to the four places the sources publish.
        let published = [
            (
                OutputSpace::Srgb,
                [
                    [0.412_4, 0.357_6, 0.180_5],
                    [0.212_6, 0.715_2, 0.072_2],
                    [0.019_3, 0.119_2, 0.950_5],
                ],
            ),
            (
                OutputSpace::DisplayP3,
                [
                    [0.486_6, 0.265_7, 0.198_2],
                    [0.229_0, 0.691_7, 0.079_3],
                    [0.0, 0.045_1, 1.043_9],
                ],
            ),
            (
                OutputSpace::AdobeRgb,
                [
                    [0.576_7, 0.185_6, 0.188_2],
                    [0.297_4, 0.627_4, 0.075_3],
                    [0.027_0, 0.070_7, 0.991_2],
                ],
            ),
            (
                OutputSpace::ProPhoto,
                [
                    [0.797_7, 0.135_2, 0.031_3],
                    [0.288_0, 0.711_9, 0.000_1],
                    [0.0, 0.0, 0.825_2],
                ],
            ),
        ];
        for (space, expected) in published {
            assert_matrix_close(&space.matrix_to_xyz(), &expected, 2e-4);
        }
    }

    #[test]
    fn primaries_round_trip_through_the_matrix() {
        for space in OutputSpace::ALL {
            let Primaries { red, green, blue } = space.primaries();
            let to_xyz = space.matrix_to_xyz();
            for (axis, primary) in [red, green, blue].iter().enumerate() {
                let unit = std::array::from_fn(|i| f64::from(u8::from(i == axis)));
                let xy = matrix::xyz_to_xy(&apply(&to_xyz, &unit));
                assert!((xy[0] - primary[0]).abs() < 1e-9, "{space:?} {xy:?}");
                assert!((xy[1] - primary[1]).abs() < 1e-9, "{space:?} {xy:?}");
            }
        }
    }

    #[test]
    fn rgb_white_is_the_space_white() {
        for space in OutputSpace::ALL {
            let expected = matrix::xy_to_xyz(&space.white_xy());
            assert_vec_close(
                &apply(&space.matrix_to_xyz(), &[1.0, 1.0, 1.0]),
                &expected,
                1e-9,
            );
        }
        assert_vec_close(
            &matrix::xy_to_xyz(&OutputSpace::ProPhoto.white_xy()),
            &WHITE_D50,
            1e-9,
        );
    }

    #[test]
    fn prophoto_white_stays_white_everywhere() {
        for space in OutputSpace::ALL {
            let white = apply(&space.matrix_from_prophoto(), &[1.0, 1.0, 1.0]);
            assert_vec_close(&white, &[1.0, 1.0, 1.0], 1e-5);
        }
    }

    #[test]
    fn prophoto_to_prophoto_is_the_identity() {
        assert_matrix_close(
            &OutputSpace::ProPhoto.matrix_from_prophoto(),
            &IDENTITY,
            1e-5,
        );
    }

    #[test]
    fn srgb_matches_the_engine_matrix() {
        assert_matrix_close(
            &OutputSpace::Srgb.matrix_from_prophoto(),
            &prophoto_to_srgb(),
            5e-4,
        );
    }

    #[test]
    fn display_p3_and_srgb_share_a_blue_but_differ_in_red() {
        let p3 = OutputSpace::DisplayP3.primaries();
        let srgb = OutputSpace::Srgb.primaries();
        assert_eq!(p3.blue, srgb.blue);
        assert_ne!(p3.red, srgb.red);
    }

    #[test]
    fn luminance_weights_sum_to_one() {
        for space in OutputSpace::ALL {
            let sum: f64 = space.luminance_weights().iter().sum();
            assert!((sum - 1.0).abs() < 1e-9, "{space:?} summed to {sum}");
        }
    }

    #[test]
    fn transfer_functions_hit_known_values() {
        let srgb = TransferFunction::Srgb;
        assert!((srgb.encode(0.18) - 0.461_356).abs() < 1e-5);
        assert!((srgb.encode(0.003_130_8) - 0.040_45).abs() < 1e-5);
        let adobe = OutputSpace::AdobeRgb.transfer();
        assert!((adobe.encode(0.5) - 0.729_658).abs() < 1e-5);
        let prophoto = OutputSpace::ProPhoto.transfer();
        assert!((prophoto.encode(0.5) - 0.680_509).abs() < 1e-5);
    }

    #[test]
    fn transfer_functions_round_trip() {
        for space in OutputSpace::ALL {
            let transfer = space.transfer();
            for step in 0..=1000 {
                let value = step as f32 / 1000.0;
                let back = transfer.decode(transfer.encode(value));
                assert!(
                    (back - value).abs() < 2e-6,
                    "{space:?} {value} became {back}"
                );
            }
        }
    }

    #[test]
    fn transfer_functions_clip_negative_light() {
        for space in OutputSpace::ALL {
            assert_eq!(space.transfer().encode(-0.1), 0.0);
            assert_eq!(space.transfer().decode(-0.1), 0.0);
        }
    }
}
