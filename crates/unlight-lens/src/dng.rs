//! WarpRectilinear and FixVignetteRadial from a DNG's opcode lists. The spec normalises
//! radii so that 1 is the distance from the optical centre to the farthest image corner, and
//! works in a corner-origin coordinate system, so both are converted here.

use crate::profile::{Frame, LensProfile, Source};
use unlight_raw::{FixVignetteRadial, Opcode, Opcodes, WarpPlane, WarpRectilinear};

fn centred(frame: Frame, x: f64, y: f64) -> [f64; 2] {
    [(x - 0.5) * frame.aspect(), y - 0.5]
}

/// Distance from a point to the farthest corner of the image.
fn farthest_corner(frame: Frame, center: [f64; 2]) -> f64 {
    let (half_width, half_height) = (frame.aspect() * 0.5, 0.5);
    let x = center[0].abs() + half_width;
    let y = center[1].abs() + half_height;
    x.hypot(y)
}

/// The spec clamps the squared radius at 1, but that only applies beyond the farthest corner,
/// and a kink there would be blurred back into the image by the curve's interpolation.
fn radial_ratio(plane: &WarpPlane, normalised_radius: f64) -> f64 {
    let squared = normalised_radius * normalised_radius;
    let [k0, k1, k2, k3] = plane.radial;
    k0 + squared * (k1 + squared * (k2 + squared * k3))
}

fn green_plane(warp: &WarpRectilinear) -> Option<&WarpPlane> {
    match warp.planes.as_slice() {
        [_, green, _] => Some(green),
        [plane, ..] => Some(plane),
        [] => None,
    }
}

fn apply_warp(profile: &mut LensProfile, warp: &WarpRectilinear, frame: Frame) -> Option<()> {
    let center = centred(frame, warp.center_x, warp.center_y);
    let norm = farthest_corner(frame, center);
    let green = green_plane(warp)?;
    profile.center = center.map(|value| value as f32);
    profile.tangential = green.tangential.map(|value| (value / norm) as f32);
    profile.set_distortion(Source::DngOpcodes, |radius| radial_ratio(green, radius / norm));
    if let [red, _, blue] = warp.planes.as_slice() {
        profile.set_chromatic_by_output(Source::DngOpcodes, |radius| {
            let base = radial_ratio(green, radius / norm);
            [red, blue].map(|plane| radial_ratio(plane, radius / norm) / base)
        });
    }
    Some(())
}

fn apply_vignette(profile: &mut LensProfile, vignette: &FixVignetteRadial, frame: Frame) {
    let center = centred(frame, vignette.center_x, vignette.center_y);
    let norm = farthest_corner(frame, center);
    profile.vignette_center = center.map(|value| value as f32);
    profile.set_vignette(Source::DngOpcodes, |radius| {
        let squared = (radius / norm).powi(2);
        let gain = 1.0 + vignette.k.iter().rev().fold(0.0, |sum, k| (sum + k) * squared);
        gain.max(f64::MIN_POSITIVE).log2()
    });
}

pub(crate) fn profile(opcodes: &Opcodes, frame: Frame) -> Option<LensProfile> {
    let mut profile = LensProfile::empty(frame);
    let all = || opcodes.list1.iter().chain(&opcodes.list2).chain(&opcodes.list3);
    let warp = all().find_map(|opcode| match opcode {
        Opcode::WarpRectilinear(warp) => Some(warp),
        _ => None,
    });
    let vignette = all().find_map(|opcode| match opcode {
        Opcode::FixVignetteRadial(vignette) => Some(vignette),
        _ => None,
    });
    if let Some(warp) = warp {
        apply_warp(&mut profile, warp, frame)?;
    }
    if let Some(vignette) = vignette {
        apply_vignette(&mut profile, vignette, frame);
    }
    (warp.is_some() || vignette.is_some()).then_some(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Frame = Frame { width: 4000.0, height: 3000.0 };

    fn warp(planes: Vec<WarpPlane>, center_x: f64, center_y: f64) -> Opcodes {
        Opcodes {
            list3: vec![Opcode::WarpRectilinear(WarpRectilinear { planes, center_x, center_y })],
            ..Opcodes::default()
        }
    }

    fn plane(radial: [f64; 4], tangential: [f64; 2]) -> WarpPlane {
        WarpPlane { radial, tangential }
    }

    #[test]
    fn nothing_to_apply_means_no_profile() {
        assert_eq!(profile(&Opcodes::default(), FRAME), None);
    }

    #[test]
    fn radial_terms_use_the_spec_normalisation() {
        let opcodes = warp(vec![plane([1.0, -0.1, 0.0, 0.0], [0.0; 2])], 0.5, 0.5);
        let profile = profile(&opcodes, FRAME).unwrap();
        let corner = FRAME.half_diagonal() as f32;
        assert!((profile.distortion_scale(corner) - 0.9).abs() < 1e-5);
        assert!((profile.distortion_scale(corner * 0.5) - 0.975).abs() < 1e-5);
    }

    #[test]
    fn an_off_centre_axis_lengthens_the_normalising_radius() {
        let opcodes = warp(vec![plane([1.0, -0.1, 0.0, 0.0], [0.0; 2])], 0.25, 0.5);
        let profile = profile(&opcodes, FRAME).unwrap();
        assert!((profile.center[0] + 1.0 / 3.0).abs() < 1e-6);
        let norm = 1.0f32.hypot(0.5);
        assert!((profile.distortion_scale(norm) - 0.9).abs() < 1e-5);
    }

    #[test]
    fn tangential_terms_are_rescaled_out_of_spec_units() {
        let opcodes = warp(vec![plane([1.0, 0.0, 0.0, 0.0], [0.002, -0.004])], 0.5, 0.5);
        let profile = profile(&opcodes, FRAME).unwrap();
        let norm = FRAME.half_diagonal() as f32;
        assert!((profile.tangential[0] - 0.002 / norm).abs() < 1e-7);
        assert!((profile.tangential[1] + 0.004 / norm).abs() < 1e-7);
    }

    #[test]
    fn three_planes_give_chromatic_aberration_relative_to_green() {
        let planes = vec![
            plane([1.001, 0.0, 0.0, 0.0], [0.0; 2]),
            plane([1.0, 0.0, 0.0, 0.0], [0.0; 2]),
            plane([0.998, 0.0, 0.0, 0.0], [0.0; 2]),
        ];
        let profile = profile(&warp(planes, 0.5, 0.5), FRAME).unwrap();
        assert_eq!(profile.chromatic, Some(Source::DngOpcodes));
        let [red, blue] = profile.chromatic_scale(0.4);
        assert!((red - 1.001).abs() < 1e-6 && (blue - 0.998).abs() < 1e-6);
    }

    #[test]
    fn vignette_follows_the_even_polynomial_in_stops() {
        let opcodes = Opcodes {
            list2: vec![Opcode::FixVignetteRadial(FixVignetteRadial {
                k: [1.0, 1.0, 0.0, 0.0, 1.0],
                center_x: 0.5,
                center_y: 0.5,
            })],
            ..Opcodes::default()
        };
        let profile = profile(&opcodes, FRAME).unwrap();
        let corner = FRAME.half_diagonal() as f32;
        assert!((profile.vignette_stops(corner) - 2.0).abs() < 1e-3, "{}", profile.vignette_stops(corner));
        assert!(profile.vignette_stops(0.0).abs() < 1e-6);
        assert_eq!(profile.distortion, None);
    }
}
