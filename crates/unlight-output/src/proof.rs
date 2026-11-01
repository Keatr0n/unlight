//! Bakes an ICC profile to the table soft proofing looks colours up in. Printer profiles are
//! usually CMYK with several megabytes of tables, far too slow to run per pixel, and a cube is
//! the same shape whatever the profile was. Every colour goes to the device and back rather
//! than through a single proofing transform, because that clips it to the device's real range,
//! which a floating-point matrix profile otherwise wouldn't.

use crate::{Error, Result, icc::chromaticity};
use bytemuck::Pod;
use lcms2::{
    CIExyY, CIExyYTRIPLE, ColorSpaceSignature, Flags, GlobalContext, InfoType, Intent, Locale, PixelFormat, Profile,
    ToneCurve, Transform,
};
use std::path::Path;
use unlight_color::{
    output::OutputSpace,
    proof::{PROOF_GAMMA, ProofIntent, ProofLut},
};
use xxhash_rust::xxh3::xxh3_64;

/// Grid points along each axis of the cube.
const GRID: u32 = 49;
/// CIE76 distance from the wanted colour past which it is out of gamut, then clearly so.
const GAMUT_NEAR: f32 = 2.0;
const GAMUT_FAR: f32 = 4.0;
const D50: CIExyY = CIExyY { x: 0.3457, y: 0.3585, Y: 1.0 };

type Rgb = [f32; 3];
type Lab = [f32; 3];

fn lcms_intent(intent: ProofIntent) -> Intent {
    match intent {
        ProofIntent::Perceptual => Intent::Perceptual,
        ProofIntent::Relative => Intent::RelativeColorimetric,
    }
}

/// Linear ProPhoto with the table's axis encoding.
fn working_profile() -> Result<Profile> {
    let primaries = OutputSpace::ProPhoto.primaries();
    let curve = ToneCurve::new(f64::from(PROOF_GAMMA));
    Ok(Profile::new_rgb(
        &chromaticity(OutputSpace::ProPhoto.white_xy()),
        &CIExyYTRIPLE {
            Red: chromaticity(primaries.red),
            Green: chromaticity(primaries.green),
            Blue: chromaticity(primaries.blue),
        },
        &[&curve, &curve, &curve],
    )?)
}

fn grid(size: u32) -> Vec<Rgb> {
    let axis = |index: u32| index as f32 / (size - 1) as f32;
    let mut colors = Vec::with_capacity((size * size * size) as usize);
    for blue in 0..size {
        for green in 0..size {
            for red in 0..size {
                colors.push([axis(red), axis(green), axis(blue)]);
            }
        }
    }
    colors
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How far outside the gamut a colour is, from what it wanted to be and what the device made.
fn gamut_distance(wanted: Lab, made: Lab) -> f32 {
    let distance = wanted.iter().zip(&made).map(|(a, b)| (a - b).powi(2)).sum::<f32>().sqrt();
    smoothstep(GAMUT_NEAR, GAMUT_FAR, distance)
}

fn bake_through<Device: Copy + Pod + Default>(
    working: &Profile,
    device: &Profile,
    format: PixelFormat,
    intent: ProofIntent,
    size: u32,
) -> Result<Vec<[f32; 4]>> {
    let colors = grid(size);
    let lab = Profile::new_lab4_context(GlobalContext::new(), &D50)?;
    let display = Profile::new_srgb();
    let compensated = Flags::BLACKPOINT_COMPENSATION;
    let relative = Intent::RelativeColorimetric;

    let mut devices = vec![Device::default(); colors.len()];
    let to_device: Transform<Rgb, Device> =
        Transform::new_flags(working, PixelFormat::RGB_FLT, device, format, lcms_intent(intent), compensated)?;
    to_device.transform_pixels(&colors, &mut devices);
    let mut shown = vec![[0.0f32; 3]; colors.len()];
    let to_display: Transform<Device, Rgb> =
        Transform::new_flags(device, format, &display, PixelFormat::RGB_FLT, relative, compensated)?;
    to_display.transform_pixels(&devices, &mut shown);

    let mut wanted = vec![[0.0f32; 3]; colors.len()];
    let working_to_lab: Transform<Rgb, Lab> =
        Transform::new_flags(working, PixelFormat::RGB_FLT, &lab, PixelFormat::Lab_FLT, relative, compensated)?;
    working_to_lab.transform_pixels(&colors, &mut wanted);
    let mut colorimetric = vec![Device::default(); colors.len()];
    let exact: Transform<Rgb, Device> =
        Transform::new_flags(working, PixelFormat::RGB_FLT, device, format, relative, compensated)?;
    exact.transform_pixels(&colors, &mut colorimetric);
    let mut made = vec![[0.0f32; 3]; colors.len()];
    let device_to_lab: Transform<Device, Lab> =
        Transform::new_flags(device, format, &lab, PixelFormat::Lab_FLT, relative, compensated)?;
    device_to_lab.transform_pixels(&colorimetric, &mut made);

    Ok(shown
        .iter()
        .zip(wanted.iter().zip(&made))
        .map(|(rgb, (wanted, made))| {
            let [red, green, blue] = rgb.map(|value| value.clamp(0.0, 1.0));
            [red, green, blue, gamut_distance(*wanted, *made)]
        })
        .collect())
}

fn table_id(icc: &[u8], intent: ProofIntent) -> u64 {
    xxh3_64(icc) ^ (intent as u64 + 1)
}

/// Bakes ICC profile data for one rendering intent.
pub fn bake_proof(icc: &[u8], intent: ProofIntent) -> Result<ProofLut> {
    let device = Profile::new_icc(icc)?;
    let working = working_profile()?;
    let entries = match device.color_space() {
        ColorSpaceSignature::RgbData => bake_through::<[u16; 3]>(&working, &device, PixelFormat::RGB_16, intent, GRID),
        ColorSpaceSignature::CmykData => bake_through::<[u16; 4]>(&working, &device, PixelFormat::CMYK_16, intent, GRID),
        ColorSpaceSignature::GrayData => bake_through::<[u16; 1]>(&working, &device, PixelFormat::GRAY_16, intent, GRID),
        other => Err(Error::Settings(format!("can't proof to a profile for {other:?} devices"))),
    }?;
    Ok(ProofLut { id: table_id(icc, intent), size: GRID, entries })
}

/// The name a profile gives itself, for lists.
pub fn proof_profile_name(icc: &[u8]) -> Result<String> {
    Ok(Profile::new_icc(icc)?.info(InfoType::Description, Locale::none()).unwrap_or_default())
}

/// Reads a profile from disk.
pub fn read_proof_profile(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::Io { path: path.to_owned(), source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icc_profile;
    use unlight_color::matrix::{apply, prophoto_to_srgb};

    fn srgb_encode(light: f32) -> f32 {
        OutputSpace::Srgb.transfer().encode(light)
    }

    fn grid_light(index: u32, size: u32) -> f32 {
        (index as f32 / (size - 1) as f32).powf(PROOF_GAMMA)
    }

    /// The table's nearest entry to a linear working-space colour.
    fn nearest(table: &ProofLut, rgb: [f32; 3]) -> [f32; 4] {
        let last = (table.size - 1) as f32;
        let [red, green, blue] = rgb.map(|value| (value.powf(1.0 / PROOF_GAMMA) * last).round() as usize);
        let size = table.size as usize;
        table.entries[(blue * size + green) * size + red]
    }

    #[test]
    fn an_in_gamut_colour_passes_through_a_matrix_profile_unchanged() {
        let lut = bake_proof(&icc_profile(OutputSpace::AdobeRgb).unwrap(), ProofIntent::Relative).unwrap();
        for [red, green, blue] in [[20u32, 20, 20], [30, 24, 18], [24, 30, 20], [24, 22, 30], [40, 40, 40]] {
            let index = ((blue * lut.size + green) * lut.size + red) as usize;
            let entry = lut.entries[index];
            let light = [red, green, blue].map(|axis| f64::from(grid_light(axis, lut.size)));
            let expected = apply(&prophoto_to_srgb(), &light).map(|value| srgb_encode(value as f32));
            for channel in 0..3 {
                assert!((entry[channel] - expected[channel]).abs() < 0.004, "{entry:?} vs {expected:?}");
            }
            assert_eq!(entry[3], 0.0, "{entry:?} is in gamut");
        }
    }

    #[test]
    fn a_colour_past_the_target_gamut_is_flagged() {
        let lut = bake_proof(&icc_profile(OutputSpace::Srgb).unwrap(), ProofIntent::Relative).unwrap();
        assert!(nearest(&lut, [0.05, 0.7, 0.05])[3] > 0.9, "ProPhoto green is well outside sRGB");
        assert!(nearest(&lut, [0.5, 0.02, 0.5])[3] > 0.9);
        assert_eq!(nearest(&lut, [0.4, 0.4, 0.4])[3], 0.0, "grey is inside every gamut");
    }

    #[test]
    fn a_wider_target_flags_less_of_the_working_space() {
        let flagged = |space| {
            let lut = bake_proof(&icc_profile(space).unwrap(), ProofIntent::Relative).unwrap();
            lut.entries.iter().filter(|entry| entry[3] > 0.5).count()
        };
        let (srgb, adobe, prophoto) =
            (flagged(OutputSpace::Srgb), flagged(OutputSpace::AdobeRgb), flagged(OutputSpace::ProPhoto));
        assert!(srgb > adobe && adobe > prophoto, "{srgb} {adobe} {prophoto}");
        assert_eq!(prophoto, 0);
    }

    /// Printer profiles are licensed, so none lives in the repo. Point `UNLIGHT_PRINTER_ICC` at
    /// a real CMYK one to run this.
    #[test]
    fn a_printer_profile_bakes_to_something_a_printer_could_do() {
        let Some(path) = std::env::var_os("UNLIGHT_PRINTER_ICC") else {
            return;
        };
        let icc = std::fs::read(path).unwrap();
        let relative = bake_proof(&icc, ProofIntent::Relative).unwrap();
        let perceptual = bake_proof(&icc, ProofIntent::Perceptual).unwrap();

        let white = nearest(&relative, [1.0; 3]);
        assert!(white[..3].iter().all(|channel| *channel > 0.97), "paper shows as white: {white:?}");
        assert!(nearest(&relative, [0.0; 3])[..3].iter().all(|channel| *channel < 0.15), "ink shows as black");
        assert_eq!(nearest(&relative, [0.4; 3])[3], 0.0, "a mid grey prints");
        assert!(nearest(&relative, [0.03, 0.75, 0.03])[3] > 0.9, "no ink makes that green");

        let flagged = |table: &ProofLut| table.entries.iter().filter(|entry| entry[3] > 0.5).count();
        let screen = bake_proof(&icc_profile(OutputSpace::Srgb).unwrap(), ProofIntent::Relative).unwrap();
        assert!(flagged(&relative) > flagged(&screen), "a press holds less than a screen");

        assert_eq!(flagged(&relative), flagged(&perceptual), "the gamut doesn't depend on the intent");

        // Many profiles share one table between intents, so give this one a perceptual table of
        // its own, to show each intent reaches the table it should.
        let Some(split) = with_its_own_perceptual_table(&icc) else {
            return;
        };
        let (split_relative, split_perceptual) = (
            bake_proof(&split, ProofIntent::Relative).unwrap(),
            bake_proof(&split, ProofIntent::Perceptual).unwrap(),
        );
        let moved = |after: &ProofLut, before: &ProofLut| {
            let pairs = after.entries.iter().zip(&before.entries);
            pairs.filter(|(a, b)| (a[0] - b[0]).abs() > 0.02).count()
        };
        // Relative moves a little too: black point compensation finds a CMYK profile's black
        // from its perceptual table, whichever intent is in use.
        let (through_perceptual, through_relative) = (moved(&split_perceptual, &perceptual), moved(&split_relative, &relative));
        assert!(through_perceptual > relative.entries.len() / 4, "perceptual follows its table: {through_perceptual}");
        assert!(through_perceptual > through_relative * 3, "{through_perceptual} vs {through_relative}");
    }

    /// A copy of an 8-bit LUT profile with lighter output curves on its perceptual table only.
    fn with_its_own_perceptual_table(icc: &[u8]) -> Option<Vec<u8>> {
        let long = |at: usize| u32::from_be_bytes(icc[at..at + 4].try_into().unwrap());
        let count = long(128) as usize;
        let entry = (0..count).map(|index| 132 + index * 12).find(|at| &icc[*at..at + 4] == b"B2A0")?;
        let (offset, size) = (long(entry + 4) as usize, long(entry + 8) as usize);
        let mut table = icc[offset..offset + size].to_vec();
        if &table[..4] != b"mft1" {
            return None;
        }
        let curves = table.len() - 256 * usize::from(table[9]);
        table[curves..].iter_mut().for_each(|value| *value = (u16::from(*value) * 3 / 5) as u8);
        let mut file = icc.to_vec();
        file.resize(file.len().next_multiple_of(4), 0);
        let moved_to = (file.len() as u32).to_be_bytes();
        file[entry + 4..entry + 8].copy_from_slice(&moved_to);
        file.extend(table);
        let total = (file.len() as u32).to_be_bytes();
        file[..4].copy_from_slice(&total);
        Some(file)
    }

    #[test]
    fn a_table_can_be_told_apart_from_another() {
        let icc = icc_profile(OutputSpace::Srgb).unwrap();
        assert_ne!(table_id(&icc, ProofIntent::Perceptual), table_id(&icc, ProofIntent::Relative));
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(bake_proof(b"not a profile", ProofIntent::Relative).is_err());
    }
}
