use crate::Result;
use lcms2::{CIExyY, CIExyYTRIPLE, Locale, MLU, Profile, Tag, TagSignature, ToneCurve};
use unlight_color::output::{OutputSpace, TransferFunction};

/// As many samples as the standard sRGB profile; lcms would otherwise write 4096.
const SRGB_CURVE_ENTRIES: usize = 1024;
/// Where a profile's header keeps its creation date, as six big-endian 16-bit numbers.
const HEADER_DATE: usize = 24;
/// lcms stamps the clock here, which would make two exports of one photo differ. These profiles
/// are built from constants, so a constant date is the true one.
const CREATED: [u16; 6] = [2026, 1, 1, 0, 0, 0];

pub(crate) fn chromaticity([x, y]: [f64; 2]) -> CIExyY {
    CIExyY { x, y, Y: 1.0 }
}

fn tone_curve(transfer: TransferFunction) -> ToneCurve {
    match transfer {
        TransferFunction::Gamma(gamma) => ToneCurve::new(gamma),
        TransferFunction::Srgb => {
            let last = (SRGB_CURVE_ENTRIES - 1) as f32;
            let entries: Vec<u16> = (0..SRGB_CURVE_ENTRIES)
                .map(|i| (f64::from(transfer.decode(i as f32 / last)) * 65535.0).round() as u16)
                .collect();
            ToneCurve::new_tabulated(&entries)
        }
    }
}

fn description(space: OutputSpace) -> &'static str {
    match space {
        // "Adobe RGB (1998)" is a trademark, so open tools label the same primaries this way.
        OutputSpace::AdobeRgb => "Compatible with Adobe RGB (1998)",
        other => other.name(),
    }
}

/// Builds the profile to embed for `space`. It's ICC v2 because exports end up in software
/// of every age, and v2 is what all of it can read.
pub fn icc_profile(space: OutputSpace) -> Result<Vec<u8>> {
    let primaries = space.primaries();
    let curve = tone_curve(space.transfer());
    let mut profile = Profile::new_rgb(
        &chromaticity(space.white_xy()),
        &CIExyYTRIPLE {
            Red: chromaticity(primaries.red),
            Green: chromaticity(primaries.green),
            Blue: chromaticity(primaries.blue),
        },
        &[&curve, &curve, &curve],
    )?;
    profile.set_version(2.4);

    let mut text = MLU::new(1);
    text.set_text_ascii(description(space), Locale::none());
    profile.write_tag(TagSignature::ProfileDescriptionTag, Tag::MLU(&text));
    let mut bytes = profile.icc()?;
    let date = bytes[HEADER_DATE..].as_chunks_mut::<2>().0.iter_mut().zip(CREATED);
    date.for_each(|(field, value)| *field = value.to_be_bytes());
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcms2::{ColorSpaceSignature, Flags, InfoType, Intent, PixelFormat, Transform};
    use unlight_color::matrix::{self, Vec3};

    type Rgb = [f32; 3];

    fn parsed(space: OutputSpace) -> Profile {
        Profile::new_icc(&icc_profile(space).unwrap()).unwrap()
    }

    fn colorant(profile: &Profile, tag: TagSignature) -> Vec3 {
        match profile.read_tag(tag) {
            Tag::CIEXYZ(xyz) => [xyz.X, xyz.Y, xyz.Z],
            other => panic!("expected XYZ for {tag:?}, got {other:?}"),
        }
    }

    fn convert(from: &Profile, to: &Profile, colors: &[Rgb]) -> Vec<Rgb> {
        let transform: Transform<Rgb, Rgb> = Transform::new_flags(
            from,
            PixelFormat::RGB_FLT,
            to,
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
            Flags::NO_OPTIMIZE,
        )
        .unwrap();
        let mut output = vec![[0.0; 3]; colors.len()];
        transform.transform_pixels(colors, &mut output);
        output
    }

    fn color_grid(steps: u32) -> Vec<Rgb> {
        let step = |v: u32| v as f32 / (steps - 1) as f32;
        let mut colors = Vec::new();
        for r in 0..steps {
            for g in 0..steps {
                for b in 0..steps {
                    colors.push([step(r), step(g), step(b)]);
                }
            }
        }
        colors
    }

    #[test]
    fn profiles_parse_as_rgb_matrix_shaper_display_profiles() {
        for space in OutputSpace::ALL {
            let profile = parsed(space);
            assert_eq!(profile.color_space(), ColorSpaceSignature::RgbData);
            assert_eq!(profile.pcs(), ColorSpaceSignature::XYZData);
            assert!(profile.is_matrix_shaper(), "{space:?}");
            assert!(
                profile.version() < 3.0,
                "{space:?} is v{}",
                profile.version()
            );
        }
    }

    #[test]
    fn a_profile_carries_no_trace_of_when_it_was_made() {
        for space in OutputSpace::ALL {
            let bytes = icc_profile(space).unwrap();
            assert_eq!(&bytes[HEADER_DATE..HEADER_DATE + 12], &[7, 234, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
            assert_eq!(parsed(space).version(), Profile::new_icc(&bytes).unwrap().version());
        }
    }

    #[test]
    fn profiles_carry_their_name() {
        for space in OutputSpace::ALL {
            let description = parsed(space)
                .info(InfoType::Description, Locale::none())
                .unwrap();
            assert_eq!(description, super::description(space));
        }
    }

    #[test]
    fn colorants_match_the_published_d50_adapted_values() {
        let published = [
            (
                OutputSpace::Srgb,
                [
                    [0.4360, 0.2225, 0.0139],
                    [0.3851, 0.7169, 0.0971],
                    [0.1431, 0.0606, 0.7139],
                ],
            ),
            (
                OutputSpace::DisplayP3,
                [
                    [0.5151, 0.2412, -0.0011],
                    [0.2920, 0.6922, 0.0419],
                    [0.1571, 0.0666, 0.7841],
                ],
            ),
            (
                OutputSpace::AdobeRgb,
                [
                    [0.6097, 0.3111, 0.0195],
                    [0.2053, 0.6257, 0.0609],
                    [0.1492, 0.0632, 0.7446],
                ],
            ),
            (
                OutputSpace::ProPhoto,
                [
                    [0.7977, 0.2880, 0.0],
                    [0.1352, 0.7119, 0.0],
                    [0.0313, 0.0001, 0.8249],
                ],
            ),
        ];
        let tags = [
            TagSignature::RedColorantTag,
            TagSignature::GreenColorantTag,
            TagSignature::BlueColorantTag,
        ];
        for (space, expected) in published {
            let profile = parsed(space);
            for (tag, expected) in tags.iter().zip(expected) {
                let actual = colorant(&profile, *tag);
                for channel in 0..3 {
                    let difference = (actual[channel] - expected[channel]).abs();
                    assert!(
                        difference < 2.5e-4,
                        "{space:?} {tag:?}: {actual:?} vs {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn device_white_lands_on_the_d50_pcs_white() {
        for space in OutputSpace::ALL {
            let xyz: Transform<Rgb, [f64; 3]> = Transform::new(
                &parsed(space),
                PixelFormat::RGB_FLT,
                &Profile::new_xyz(),
                PixelFormat::XYZ_DBL,
                Intent::RelativeColorimetric,
            )
            .unwrap();
            let mut white = [[0.0; 3]];
            xyz.transform_pixels(&[[1.0, 1.0, 1.0]], &mut white);
            let expected = [0.9642, 1.0, 0.8249];
            for channel in 0..3 {
                assert!(
                    (white[0][channel] - expected[channel]).abs() < 1e-3,
                    "{space:?} {white:?}"
                );
            }
        }
    }

    #[test]
    fn pure_gamma_curves_are_stored_exactly() {
        for (space, stored) in [(OutputSpace::AdobeRgb, 563), (OutputSpace::ProPhoto, 461)] {
            let curve = match parsed(space).read_tag(TagSignature::RedTRCTag) {
                Tag::ToneCurve(curve) => curve.to_owned(),
                other => panic!("{space:?} has no red curve: {other:?}"),
            };
            let TransferFunction::Gamma(gamma) = space.transfer() else {
                panic!("{space:?} isn't a pure gamma");
            };
            assert_eq!(gamma, f64::from(stored) / 256.0, "{space:?}");
            let read_back = curve.estimated_gamma(0.01).unwrap();
            assert!(
                (read_back - gamma).abs() < 1e-4,
                "{space:?}: {read_back} vs {gamma}"
            );
        }
    }

    #[test]
    fn colours_round_trip_through_every_profile() {
        let colors = color_grid(9);
        for space in OutputSpace::ALL {
            let profile = parsed(space);
            let xyz = Profile::new_xyz();
            let forward: Transform<Rgb, [f64; 3]> = Transform::new(
                &profile,
                PixelFormat::RGB_FLT,
                &xyz,
                PixelFormat::XYZ_DBL,
                Intent::RelativeColorimetric,
            )
            .unwrap();
            let backward: Transform<[f64; 3], Rgb> = Transform::new(
                &xyz,
                PixelFormat::XYZ_DBL,
                &profile,
                PixelFormat::RGB_FLT,
                Intent::RelativeColorimetric,
            )
            .unwrap();
            let mut pcs = vec![[0.0; 3]; colors.len()];
            forward.transform_pixels(&colors, &mut pcs);
            let mut back = vec![[0.0; 3]; colors.len()];
            backward.transform_pixels(&pcs, &mut back);
            for (color, result) in colors.iter().zip(&back) {
                for channel in 0..3 {
                    let difference = (color[channel] - result[channel]).abs();
                    assert!(difference < 1e-3, "{space:?} {color:?} became {result:?}");
                }
            }
        }
    }

    #[test]
    fn srgb_profile_agrees_with_the_lcms_builtin() {
        let colors = color_grid(9);
        let identity = convert(&parsed(OutputSpace::Srgb), &Profile::new_srgb(), &colors);
        for (color, result) in colors.iter().zip(&identity) {
            for channel in 0..3 {
                assert!(
                    (color[channel] - result[channel]).abs() < 1e-3,
                    "{color:?} became {result:?}"
                );
            }
        }
    }

    #[test]
    fn profiles_agree_with_the_engine_matrices_and_curves() {
        let colors: Vec<Rgb> = color_grid(7);
        let prophoto = parsed(OutputSpace::ProPhoto);
        let prophoto_curve = OutputSpace::ProPhoto.transfer();
        for space in OutputSpace::ALL {
            let converted = convert(&prophoto, &parsed(space), &colors);
            let matrix = space.matrix_from_prophoto();
            let transfer = space.transfer();
            for (color, through_lcms) in colors.iter().zip(&converted) {
                let linear: Vec3 = color.map(|value| f64::from(prophoto_curve.decode(value)));
                let in_space = matrix::apply(&matrix, &linear);
                if in_space.iter().any(|value| !(0.0..=1.0).contains(value)) {
                    continue;
                }
                let expected = in_space.map(|value| transfer.encode(value as f32));
                for channel in 0..3 {
                    let difference = (expected[channel] - through_lcms[channel]).abs();
                    assert!(
                        difference < 1e-3,
                        "{space:?} {color:?}: {expected:?} vs {through_lcms:?}"
                    );
                }
            }
        }
    }
}
