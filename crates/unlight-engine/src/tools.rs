//! Maps recipe sliders onto the shader's internal scales.

use crate::{
    curves,
    geometry::{Geometry, vignette_strength},
    optics::OpticsState,
    params::{DevelopParams, FinishParams, wgsl_mat3},
    rendering,
    source::SourceImage,
};
use unlight_color::{
    Mat3, Vec3,
    matrix::{
        IDENTITY, WHITE_D50, WHITE_D65, XYZ_D50_TO_PROPHOTO, apply, bradford, diagonal, inverse,
        mul, prophoto_to_srgb,
    },
};
use unlight_lens::CURVE_SIZE;
use unlight_color::output::OutputSpace;
use unlight_recipe::{Calibration, Defringe, GradeWheel, HSL_BANDS, Recipe, WhiteBalance};

pub const LUMA_WEIGHTS: [f32; 4] = [0.2880, 0.7119, 0.0001, 0.0];

const XYZ_D65_TO_LMS: Mat3 = [
    [0.818_933_010_1, 0.361_866_742_4, -0.128_859_713_7],
    [0.032_984_543_6, 0.929_311_871_5, 0.036_145_638_7],
    [0.048_200_301_8, 0.264_366_269_1, 0.633_851_707_0],
];

const MIXER_HUE_RANGE: f32 = 0.52;
const MIXER_LUMINANCE_RANGE: f32 = 0.5;
const MONOCHROME_RANGE: f32 = 0.6;
const GRADE_CHROMA_RANGE: f32 = 0.12;
const GRADE_LUMINANCE_RANGE: f32 = 0.2;
const CALIBRATION_RANGE: f64 = 0.5;
/// Below this many output pixels a radius has no visible effect.
const MIN_SHARPEN_RADIUS: f32 = 0.2;

/// How much of a lens's lateral chromatic aberration to undo. Lens data states all of it, but
/// demosaicing has already drawn red and blue edges partway to green, so the rest is what's
/// left to fix here. Sweeping this on Bayer and X-Trans raws, edge fringing bottoms out near
/// this value on all of them, while the full amount is little better than none.
const CHROMATIC_STRENGTH: f32 = 0.55;
/// Brightening of the corners at full Lens Vignetting, in stops.
const MANUAL_VIGNETTE_RANGE: f32 = 2.0;
/// Fringes reach this far from an edge at full resolution, in source pixels.
const FRINGE_REACH: f32 = 3.5;
/// The arcs of Oklab hue the defringe hue sliders run over, in degrees.
const PURPLE_HUES: [f32; 2] = [250.0, 350.0];
const GREEN_HUES: [f32; 2] = [90.0, 190.0];

pub(crate) fn working_to_lms() -> Mat3 {
    let working_to_xyz = inverse(&XYZ_D50_TO_PROPHOTO).unwrap_or(IDENTITY);
    let to_d65 = bradford(&WHITE_D50, &WHITE_D65);
    mul(&XYZ_D65_TO_LMS, &mul(&to_d65, &working_to_xyz))
}

fn split_bands(values: &[f32; HSL_BANDS], scale: f32) -> [[f32; 4]; 2] {
    std::array::from_fn(|entry| std::array::from_fn(|lane| values[entry * 4 + lane] / 100.0 * scale))
}

fn wheel(values: &GradeWheel) -> [f32; 4] {
    [
        values.hue.to_radians(),
        values.luminance / 100.0 * GRADE_LUMINANCE_RANGE,
        0.0,
        values.saturation / 100.0 * GRADE_CHROMA_RANGE,
    ]
}

/// Moves the working-space primaries. Columns are the new primaries, normalised so white
/// stays white.
fn calibration_matrix(calibration: &Calibration) -> Mat3 {
    let shift = |value: f32| f64::from(value) / 100.0 * CALIBRATION_RANGE;
    let primary = |own: usize, next: usize, last: usize, hue: f32, saturation: f32| {
        let mut column = [0.0; 3];
        let hue = shift(hue);
        column[own] = 1.0 + shift(saturation);
        column[next] = hue.max(0.0);
        column[last] = (-hue).max(0.0);
        let desaturate = (-shift(saturation)).max(0.0);
        column.map(|value| value + desaturate * 0.33)
    };
    let red = primary(0, 1, 2, calibration.red_hue, calibration.red_saturation);
    let green = primary(1, 2, 0, calibration.green_hue, calibration.green_saturation);
    let blue = primary(2, 0, 1, calibration.blue_hue, calibration.blue_saturation);
    let matrix: Mat3 = std::array::from_fn(|row| [red[row], green[row], blue[row]]);
    let white = apply(&matrix, &[1.0, 1.0, 1.0]);
    let normalise = diagonal(&white.map(|value| 1.0 / value.max(1e-6)));
    mul(&normalise, &matrix)
}

/// Amount, then the low and high ends of the hue range in radians.
fn defringe_band(amount: f32, low: f32, high: f32, arc: [f32; 2]) -> [f32; 4] {
    let hue = |slider: f32| (arc[0] + (arc[1] - arc[0]) * slider / 100.0).to_radians();
    [amount / 100.0, hue(low), hue(high), 0.0]
}

fn purple_band(defringe: &Defringe) -> [f32; 4] {
    defringe_band(defringe.purple_amount, defringe.purple_hue_low, defringe.purple_hue_high, PURPLE_HUES)
}

fn green_band(defringe: &Defringe) -> [f32; 4] {
    defringe_band(defringe.green_amount, defringe.green_hue_low, defringe.green_hue_high, GREEN_HUES)
}

/// Global statistics measured from the guide image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageStats {
    pub white_ev: f32,
    pub black_ev: f32,
    pub atmosphere: f32,
}

impl Default for ImageStats {
    fn default() -> Self {
        Self { white_ev: 0.0, black_ev: -9.0, atmosphere: 0.9 }
    }
}

/// The camera neutral a recipe's white balance asks for.
pub fn resolve_neutral(source: &SourceImage, recipe: &Recipe) -> Vec3 {
    match recipe.white_balance {
        WhiteBalance::AsShot => source.profile.as_shot_neutral(),
        WhiteBalance::Custom { temperature, tint } => {
            source.profile.neutral_for_temperature(f64::from(temperature), f64::from(tint))
        }
    }
}

/// Camera-to-working matrix for a recipe, on top of the mosaic's reference balance.
pub fn camera_to_working(source: &SourceImage, recipe: &Recipe) -> Mat3 {
    let neutral = resolve_neutral(source, recipe);
    let to_working = source.profile.camera_to_prophoto(&neutral);
    let balanced = mul(&to_working, &diagonal(&source.reference_neutral));
    mul(&calibration_matrix(&recipe.calibration), &balanced)
}

pub fn develop_params(
    source: &SourceImage,
    recipe: &Recipe,
    stats: &ImageStats,
    optics: &OpticsState,
) -> DevelopParams {
    let aspect = source.width as f32 / source.height as f32;
    let lens = &recipe.lens;
    let geometry = Geometry::new(&recipe.crop, &recipe.transform, lens, aspect, optics.current());
    let profile = optics.current().profile.as_deref();
    let plane_gain = optics.plane_gain;
    let to_lms = working_to_lms();
    let basic = &recipe.basic;
    let percent = |value: f32| value / 100.0;
    let mixer = &recipe.color_mixer;
    let grading = &recipe.color_grading;
    let any = |values: &[f32; HSL_BANDS]| values.iter().any(|&value| value != 0.0);
    let mixer_active = any(&mixer.hue) || any(&mixer.saturation) || any(&mixer.luminance);
    let is_graded = [&grading.shadows, &grading.midtones, &grading.highlights, &grading.global]
        .iter()
        .any(|wheel| wheel.saturation != 0.0 || wheel.luminance != 0.0);

    DevelopParams {
        camera_to_working: wgsl_mat3(&camera_to_working(source, recipe)),
        working_to_lms: wgsl_mat3(&to_lms),
        lms_to_working: wgsl_mat3(&inverse(&to_lms).unwrap_or(IDENTITY)),
        inverse_homography: wgsl_mat3(&geometry.inverse_homography),
        view_origin: [0.0; 2],
        view_step: [0.0; 2],
        crop_origin: geometry.crop_origin,
        crop_size: geometry.crop_size,
        rotation: geometry.rotation,
        aspect,
        lod: 0.0,
        lens: geometry.lens,
        luma: LUMA_WEIGHTS,

        exposure: basic.exposure + source.baseline_exposure + rendering::exposure_offset(&source.profile),
        contrast: percent(basic.contrast),
        highlights: percent(basic.highlights),
        shadows: percent(basic.shadows),
        whites: percent(basic.whites),
        blacks: percent(basic.blacks),
        clarity: percent(basic.clarity),
        texture_amount: percent(basic.texture),
        dehaze: percent(basic.dehaze),
        vibrance: percent(basic.vibrance),
        saturation: percent(basic.saturation),
        white_ev: stats.white_ev,
        black_ev: stats.black_ev,
        look_chroma: rendering::look_chroma(&source.profile),
        tone_padding: 0.0,
        curves_enabled: f32::from(u8::from(!curves::is_identity(&recipe.tone_curve))),
        atmosphere: stats.atmosphere,
        shadow_tint: percent(recipe.calibration.shadow_tint) * 0.5,
        mixer_enabled: f32::from(u8::from(mixer_active)),
        monochrome: f32::from(u8::from(recipe.black_and_white.enabled)),

        mixer_hue: split_bands(&mixer.hue, MIXER_HUE_RANGE),
        mixer_saturation: split_bands(&mixer.saturation, 1.0),
        mixer_luminance: split_bands(&mixer.luminance, MIXER_LUMINANCE_RANGE),
        bw_mix: split_bands(&recipe.black_and_white.mix, MONOCHROME_RANGE),

        grade_shadows: wheel(&grading.shadows),
        grade_midtones: wheel(&grading.midtones),
        grade_highlights: wheel(&grading.highlights),
        grade_global: wheel(&grading.global),
        grade_blending: percent(grading.blending),
        grade_balance: percent(grading.balance),
        grade_enabled: f32::from(u8::from(is_graded)),
        lens_curve_scale: profile.map_or(0.0, |profile| (CURVE_SIZE - 1) as f32 / profile.reach),

        lens_center: geometry.lens_center,
        lens_tangential: geometry.tangential,
        vignette_center: profile.map_or([0.0; 2], |profile| profile.vignette_center),
        profile_distortion: geometry.profile_distortion,
        profile_chromatic: if lens.remove_chromatic_aberration && profile.is_some_and(|profile| profile.chromatic.is_some()) {
            CHROMATIC_STRENGTH
        } else {
            0.0
        },
        profile_vignette: profile.map_or(0.0, |profile| vignette_strength(lens, profile)),
        manual_vignette: percent(lens.manual_vignette) * MANUAL_VIGNETTE_RANGE,
        manual_vignette_start: percent(lens.manual_vignette_midpoint) * 0.9,
        manual_vignette_norm: 2.0 / aspect.hypot(1.0),
        defringe_purple: purple_band(&lens.defringe),
        defringe_green: green_band(&lens.defringe),
        fringe_reach: [FRINGE_REACH / source.width as f32, FRINGE_REACH / source.height as f32],
        plane_gain_enabled: f32::from(u8::from(plane_gain.enabled)),
        local_flags: 0,
        plane_gain_scale: plane_gain.scale,
        plane_gain_offset: plane_gain.offset,
        hue_sat_cells: rendering::hue_sat_cells(&source.profile),
        look_cells: rendering::look_cells(&source.profile),
    }
}

/// Targets a finished image at an output colour space. The encodings must stay in step with
/// `encode` in finish.wgsl and with `OutputSpace::transfer`, which the embedded ICC describes.
pub fn apply_output_space(finish: &mut FinishParams, space: OutputSpace) {
    finish.working_to_output = wgsl_mat3(&space.matrix_from_prophoto());
    finish.encoding = match space {
        OutputSpace::Srgb | OutputSpace::DisplayP3 => 0,
        OutputSpace::AdobeRgb => 1,
        OutputSpace::ProPhoto => 2,
    };
}

/// Whether sharpening would show at all at this many source pixels per output pixel.
pub(crate) fn sharpens_at(recipe: &Recipe, pixel_scale: f32) -> bool {
    recipe.detail.sharpening.radius / pixel_scale.max(1.0) >= MIN_SHARPEN_RADIUS
}

/// `pixel_scale` is source pixels per output pixel, which keeps sharpening and grain the same
/// size relative to the photo at every zoom.
pub fn finish_params(recipe: &Recipe, aspect: f32, pixel_scale: f32) -> FinishParams {
    let sharpening = &recipe.detail.sharpening;
    let vignette = &recipe.effects.vignette;
    let grain = &recipe.effects.grain;
    let percent = |value: f32| value / 100.0;
    let radius = sharpening.radius / pixel_scale.max(1.0);
    let sharpens = sharpens_at(recipe, pixel_scale);
    FinishParams {
        working_to_output: wgsl_mat3(&prophoto_to_srgb()),
        luma: LUMA_WEIGHTS,
        view_origin: [0.0; 2],
        view_step: [0.0; 2],
        sharpen_amount: if sharpens { percent(sharpening.amount) } else { 0.0 },
        sharpen_radius: radius.max(MIN_SHARPEN_RADIUS),
        sharpen_detail: percent(sharpening.detail),
        sharpen_masking: percent(sharpening.masking),
        vignette_amount: percent(vignette.amount),
        vignette_midpoint: percent(vignette.midpoint),
        vignette_roundness: (percent(vignette.roundness) + 1.0) * 0.5,
        vignette_feather: percent(vignette.feather),
        vignette_highlights: percent(vignette.highlights),
        grain_amount: percent(grain.amount) * 0.25,
        grain_scale: 900.0 / (1.0 + percent(grain.size) * 3.0),
        grain_roughness: percent(grain.roughness),
        aspect,
        encoding: 0,
        dither: 1.0 / 255.0,
        show_clipping: 0.0,
        show_sharpen_mask: 0.0,
        local_detail: 0.0,
        mask_overlay: 0.0,
        padding: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_calibration_is_the_identity() {
        let matrix = calibration_matrix(&Calibration::default());
        for row in 0..3 {
            for col in 0..3 {
                let expected = if row == col { 1.0 } else { 0.0 };
                assert!((matrix[row][col] - expected).abs() < 1e-9, "{matrix:?}");
            }
        }
    }

    #[test]
    fn calibration_keeps_white_neutral_at_any_setting() {
        let calibration = Calibration {
            shadow_tint: 0.0,
            red_hue: 60.0,
            red_saturation: -40.0,
            green_hue: -30.0,
            green_saturation: 80.0,
            blue_hue: 100.0,
            blue_saturation: -100.0,
        };
        let white = apply(&calibration_matrix(&calibration), &[1.0, 1.0, 1.0]);
        for channel in white {
            assert!((channel - 1.0).abs() < 1e-9, "{white:?}");
        }
    }

    #[test]
    fn lms_matrix_sends_the_working_white_near_the_oklab_axis() {
        let lms = apply(&working_to_lms(), &[1.0, 1.0, 1.0]);
        assert!((lms[0] - lms[1]).abs() < 0.02 && (lms[1] - lms[2]).abs() < 0.02, "{lms:?}");
    }

    #[test]
    fn band_values_land_in_vec4_lanes_in_order() {
        let mut values = [0.0; HSL_BANDS];
        values[0] = 100.0;
        values[5] = 50.0;
        let packed = split_bands(&values, 1.0);
        assert_eq!(packed[0][0], 1.0);
        assert_eq!(packed[1][1], 0.5);
    }

    #[test]
    fn sharpening_turns_off_when_too_small_to_see() {
        let recipe = Recipe::default();
        assert!(finish_params(&recipe, 1.5, 1.0).sharpen_amount > 0.0);
        assert_eq!(finish_params(&recipe, 1.5, 6.0).sharpen_amount, 0.0);
    }

    #[test]
    fn defringe_hue_sliders_span_their_arc() {
        let band = defringe_band(50.0, 0.0, 100.0, [90.0, 190.0]);
        assert_eq!(band[0], 0.5);
        assert!((band[1] - 90f32.to_radians()).abs() < 1e-6);
        assert!((band[2] - 190f32.to_radians()).abs() < 1e-6);
    }

    #[test]
    fn uniform_struct_sizes_match_the_shaders() {
        assert_eq!(size_of::<DevelopParams>(), 704);
        assert_eq!(size_of::<FinishParams>(), 160);
    }
}
