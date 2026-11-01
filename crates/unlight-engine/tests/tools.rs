//! Pins down what each Develop control does, in Lightroom's terms: which way it moves the
//! image and, just as importantly, what it leaves alone.

mod support;

use support::{bayer, center, chroma, flat_render, luma, mosaic, pixel, render};
use unlight_engine::{ExportOptions, ExportedImage, OutputSpace, export};
use unlight_recipe::{Curve, CurvePoint, GradeWheel, Recipe, WhiteBalance};

const SHADOW: [f32; 3] = [0.01, 0.01, 0.01];
const MIDTONE: [f32; 3] = [0.1, 0.1, 0.1];
const BRIGHT: [f32; 3] = [0.7, 0.7, 0.7];
const RED: [f32; 3] = [0.5, 0.05, 0.05];
const BLUE: [f32; 3] = [0.05, 0.05, 0.5];
const GREEN: [f32; 3] = [0.05, 0.4, 0.05];

fn recipe(change: impl FnOnce(&mut Recipe)) -> Recipe {
    let mut recipe = Recipe::default();
    change(&mut recipe);
    recipe
}

fn change_in_luma(color: [f32; 3], edited: &Recipe) -> f32 {
    luma(flat_render(color, edited)) - luma(flat_render(color, &Recipe::default()))
}

/// Dark on the left half, bright on the right, for tools that work on the base layer.
fn split_scene() -> unlight_raw::RawImage {
    mosaic(bayer(), 256, 128, |x, _| if x < 128.0 { SHADOW } else { BRIGHT })
}

fn left_right(image: &unlight_engine::ExportedImage) -> (f32, f32) {
    (luma(pixel(image, 40, 64)), luma(pixel(image, 216, 64)))
}

#[test]
fn exposure_scales_every_tone() {
    let brighter = recipe(|r| r.basic.exposure = 1.0);
    let darker = recipe(|r| r.basic.exposure = -1.0);
    assert!(change_in_luma(MIDTONE, &brighter) > 0.05);
    assert!(change_in_luma(MIDTONE, &darker) < -0.05);
}

#[test]
fn contrast_spreads_tones_around_the_middle() {
    let raised = recipe(|r| r.basic.contrast = 80.0);
    assert!(change_in_luma(SHADOW, &raised) < 0.0);
    assert!(change_in_luma(BRIGHT, &raised) > 0.0);
}

#[test]
fn highlights_move_bright_tones_and_spare_shadows() {
    let base = render(&split_scene(), &Recipe::default());
    let edited = render(&split_scene(), &recipe(|r| r.basic.highlights = -100.0));
    let (dark_before, bright_before) = left_right(&base);
    let (dark_after, bright_after) = left_right(&edited);
    assert!(bright_after < bright_before - 0.05, "{bright_before} → {bright_after}");
    assert!((dark_after - dark_before).abs() < 0.02, "{dark_before} → {dark_after}");
}

#[test]
fn shadows_move_dark_tones_and_spare_highlights() {
    let base = render(&split_scene(), &Recipe::default());
    let edited = render(&split_scene(), &recipe(|r| r.basic.shadows = 100.0));
    let (dark_before, bright_before) = left_right(&base);
    let (dark_after, bright_after) = left_right(&edited);
    assert!(dark_after > dark_before + 0.05, "{dark_before} → {dark_after}");
    assert!((bright_after - bright_before).abs() < 0.02, "{bright_before} → {bright_after}");
}

#[test]
fn whites_act_on_the_brightest_tones() {
    let base = render(&split_scene(), &Recipe::default());
    let edited = render(&split_scene(), &recipe(|r| r.basic.whites = 100.0));
    let (dark_before, bright_before) = left_right(&base);
    let (dark_after, bright_after) = left_right(&edited);
    assert!(bright_after > bright_before + 0.02, "{bright_before} → {bright_after}");
    assert!((dark_after - dark_before).abs() < 0.01, "{dark_before} → {dark_after}");
}

#[test]
fn blacks_act_on_the_darkest_tones() {
    let base = render(&split_scene(), &Recipe::default());
    let edited = render(&split_scene(), &recipe(|r| r.basic.blacks = -100.0));
    let (dark_before, bright_before) = left_right(&base);
    let (dark_after, bright_after) = left_right(&edited);
    assert!(dark_after < dark_before - 0.02, "{dark_before} → {dark_after}");
    assert!((bright_after - bright_before).abs() < 0.01, "{bright_before} → {bright_after}");
}

#[test]
fn clarity_lifts_local_contrast_without_shifting_the_average() {
    let scene = || mosaic(bayer(), 256, 256, |x, y| {
        let stripe = if ((x / 32.0).floor() as i32 + (y / 32.0).floor() as i32) % 2 == 0 { 0.3 } else { 0.1 };
        [stripe; 3]
    });
    let spread = |image: &unlight_engine::ExportedImage| {
        (luma(pixel(image, 16, 16)) - luma(pixel(image, 48, 16))).abs()
    };
    let base = render(&scene(), &Recipe::default());
    let edited = render(&scene(), &recipe(|r| r.basic.clarity = 100.0));
    assert!(spread(&edited) > spread(&base), "{} → {}", spread(&base), spread(&edited));
}

#[test]
fn saturation_scales_colour_and_minus_100_is_grey() {
    let base = chroma(flat_render(RED, &Recipe::default()));
    let raised = chroma(flat_render(RED, &recipe(|r| r.basic.saturation = 50.0)));
    let removed = chroma(flat_render(RED, &recipe(|r| r.basic.saturation = -100.0)));
    assert!(raised > base);
    assert!(removed < 0.01, "{removed}");
}

#[test]
fn vibrance_favours_muted_colours() {
    let muted = [0.2, 0.17, 0.15];
    let vivid = RED;
    let boost = recipe(|r| r.basic.vibrance = 100.0);
    let relative = |color| chroma(flat_render(color, &boost)) / chroma(flat_render(color, &Recipe::default()));
    assert!(relative(muted) > relative(vivid), "{} vs {}", relative(muted), relative(vivid));
}

#[test]
fn neutral_greys_are_untouched_by_colour_tools() {
    let edited = recipe(|r| {
        r.basic.vibrance = 100.0;
        r.basic.saturation = 100.0;
        r.color_mixer.saturation = [100.0; 8];
        r.color_mixer.hue = [60.0; 8];
    });
    assert!(chroma(flat_render(MIDTONE, &edited)) < 0.01);
}

#[test]
fn colour_mixer_band_only_moves_its_own_hue() {
    let red_off = recipe(|r| r.color_mixer.saturation[0] = -100.0);
    assert!(chroma(flat_render(RED, &red_off)) < chroma(flat_render(RED, &Recipe::default())) * 0.4);
    let blue_before = flat_render(BLUE, &Recipe::default());
    let blue_after = flat_render(BLUE, &red_off);
    assert!((chroma(blue_before) - chroma(blue_after)).abs() < 0.01);
}

#[test]
fn colour_mixer_hue_shifts_towards_the_next_band() {
    let shifted = flat_render(GREEN, &recipe(|r| r.color_mixer.hue[3] = 100.0));
    let original = flat_render(GREEN, &Recipe::default());
    assert!(shifted[2] > original[2], "{original:?} → {shifted:?}");
}

#[test]
fn colour_mixer_luminance_changes_brightness_of_its_band() {
    let lifted = recipe(|r| r.color_mixer.luminance[0] = 100.0);
    assert!(change_in_luma(RED, &lifted) > 0.02);
    assert!(change_in_luma(BLUE, &lifted).abs() < 0.01);
}

#[test]
fn black_and_white_removes_colour_and_mix_changes_brightness() {
    let mono = recipe(|r| r.black_and_white.enabled = true);
    assert!(chroma(flat_render(RED, &mono)) < 0.01);
    let bright_reds = recipe(|r| {
        r.black_and_white.enabled = true;
        r.black_and_white.mix[0] = 100.0;
    });
    assert!(luma(flat_render(RED, &bright_reds)) > luma(flat_render(RED, &mono)) + 0.02);
}

#[test]
fn warmer_white_balance_renders_warmer() {
    let cool_setting = flat_render(MIDTONE, &recipe(|r| {
        r.white_balance = WhiteBalance::Custom { temperature: 4000.0, tint: 0.0 };
    }));
    let warm_setting = flat_render(MIDTONE, &recipe(|r| {
        r.white_balance = WhiteBalance::Custom { temperature: 9000.0, tint: 0.0 };
    }));
    assert!(warm_setting[0] > cool_setting[0] && warm_setting[2] < cool_setting[2]);
}

#[test]
fn positive_tint_renders_magenta() {
    let green = flat_render(MIDTONE, &recipe(|r| {
        r.white_balance = WhiteBalance::Custom { temperature: 6500.0, tint: -60.0 };
    }));
    let magenta = flat_render(MIDTONE, &recipe(|r| {
        r.white_balance = WhiteBalance::Custom { temperature: 6500.0, tint: 60.0 };
    }));
    let green_cast = green[1] - (green[0] + green[2]) * 0.5;
    let magenta_cast = magenta[1] - (magenta[0] + magenta[2]) * 0.5;
    assert!(green_cast > 0.03 && magenta_cast < -0.03, "{green:?} vs {magenta:?}");
}

#[test]
fn point_curve_changes_output_and_identity_does_nothing() {
    let lifted = recipe(|r| {
        r.tone_curve.rgb = Curve(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.7),
            CurvePoint::new(1.0, 1.0),
        ]);
    });
    assert!(change_in_luma(MIDTONE, &lifted) > 0.05);
    let identity = recipe(|r| {
        r.tone_curve.rgb = Curve(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]);
    });
    assert!(change_in_luma(MIDTONE, &identity).abs() < 0.005);
}

#[test]
fn channel_curve_tints_the_image() {
    let red_lift = recipe(|r| {
        r.tone_curve.red = Curve(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.7),
            CurvePoint::new(1.0, 1.0),
        ]);
    });
    let [r, g, b] = flat_render(MIDTONE, &red_lift);
    assert!(r > g.max(b) + 0.2, "{r} {g} {b}");
}

#[test]
fn colour_grading_tints_only_its_own_region() {
    let blue_shadows = recipe(|r| {
        r.color_grading.shadows = GradeWheel { hue: 260.0, saturation: 100.0, luminance: 0.0 };
    });
    let dark = flat_render([0.02; 3], &blue_shadows);
    assert!(dark[2] > dark[0] + 0.02, "{dark:?}");
    let light = flat_render(BRIGHT, &blue_shadows);
    assert!(chroma(light) < 0.03, "{light:?}");
}

#[test]
fn colour_grading_global_tints_everything() {
    let warm = recipe(|r| {
        r.color_grading.global = GradeWheel { hue: 40.0, saturation: 100.0, luminance: 0.0 };
    });
    for tone in [[0.02; 3], MIDTONE, BRIGHT] {
        let graded = flat_render(tone, &warm);
        assert!(graded[0] > graded[2] + 0.01, "{tone:?} → {graded:?}");
    }
}

#[test]
fn calibration_moves_primaries_and_keeps_neutrals() {
    let shifted = recipe(|r| r.calibration.red_hue = 80.0);
    let edited = flat_render(RED, &shifted);
    let original = flat_render(RED, &Recipe::default());
    assert!(edited[1] > original[1] + 0.01, "{original:?} → {edited:?}");
    assert!(chroma(flat_render(MIDTONE, &shifted)) < 0.015);
}

#[test]
fn shadow_tint_only_touches_dark_tones() {
    let tinted = recipe(|r| r.calibration.shadow_tint = 100.0);
    let dark = flat_render([0.01; 3], &tinted);
    assert!(dark[1] < dark[0], "{dark:?}");
    assert!(chroma(flat_render(BRIGHT, &tinted)) < 0.02);
}

#[test]
fn post_crop_vignette_darkens_corners_and_spares_the_centre() {
    let raw = mosaic(bayer(), 200, 150, |_, _| [0.2; 3]);
    let base = render(&raw, &Recipe::default());
    let edited = render(&raw, &recipe(|r| r.effects.vignette.amount = -100.0));
    assert!(luma(pixel(&edited, 2, 2)) < luma(pixel(&base, 2, 2)) - 0.1);
    assert!((luma(center(&edited)) - luma(center(&base))).abs() < 0.02);
}

#[test]
fn positive_vignette_lightens_corners() {
    let raw = mosaic(bayer(), 200, 150, |_, _| [0.1; 3]);
    let base = render(&raw, &Recipe::default());
    let edited = render(&raw, &recipe(|r| r.effects.vignette.amount = 100.0));
    assert!(luma(pixel(&edited, 2, 2)) > luma(pixel(&base, 2, 2)) + 0.05);
}

fn deviation(image: &unlight_engine::ExportedImage) -> f32 {
    let values: Vec<f32> = (40..80).flat_map(|y| (40..80).map(move |x| (x, y)))
        .map(|(x, y)| pixel(image, x, y)[1])
        .collect();
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    (values.iter().map(|value| (value - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt()
}

#[test]
fn grain_adds_texture_to_flat_areas() {
    let raw = mosaic(bayer(), 128, 128, |_, _| [0.2; 3]);
    let base = render(&raw, &Recipe::default());
    let edited = render(&raw, &recipe(|r| r.effects.grain.amount = 100.0));
    assert!(deviation(&edited) > deviation(&base) + 0.005);
}

#[test]
fn crop_sets_the_output_size_and_shows_the_chosen_area() {
    let raw = mosaic(bayer(), 200, 100, |x, _| if x < 100.0 { [0.02; 3] } else { [0.5; 3] });
    let edited = recipe(|r| {
        r.crop.left = 0.5;
        r.crop.right = 1.0;
    });
    let image = render(&raw, &edited);
    assert_eq!((image.width, image.height), (100, 100));
    assert!(luma(pixel(&image, 10, 50)) > 0.5, "{:?}", pixel(&image, 10, 50));
}

#[test]
fn straightening_keeps_the_centre_in_place() {
    let raw = mosaic(bayer(), 200, 200, |x, y| {
        if (x - 100.0).abs() < 12.0 && (y - 100.0).abs() < 12.0 { [0.6; 3] } else { [0.03; 3] }
    });
    let image = render(&raw, &recipe(|r| r.crop.angle = 10.0));
    assert!(luma(center(&image)) > 0.5);
}

#[test]
fn dehaze_recovers_contrast_from_a_hazy_scene() {
    let scene = || mosaic(bayer(), 256, 128, |x, _| {
        let clear: f32 = if x < 128.0 { 0.05 } else { 0.25 };
        [clear * 0.5 + 0.45; 3]
    });
    let base = render(&scene(), &Recipe::default());
    let edited = render(&scene(), &recipe(|r| r.basic.dehaze = 100.0));
    let spread = |image: &unlight_engine::ExportedImage| {
        let (left, right) = left_right(image);
        right - left
    };
    assert!(spread(&edited) > spread(&base), "{} → {}", spread(&base), spread(&edited));
}

fn render_in(raw: &unlight_raw::RawImage, space: OutputSpace) -> ExportedImage {
    let options = ExportOptions { output_space: space, ..ExportOptions::default() };
    export(raw, &Recipe::default(), &options).expect("export")
}

/// Greys share a white in every D65 space, and D50 ProPhoto adapts to the same neutral, so
/// any difference between spaces here comes from the transfer curve alone.
#[test]
fn each_output_space_encodes_with_its_own_transfer_function() {
    let raw = support::flat(MIDTONE);
    let reference = center(&render_in(&raw, OutputSpace::Srgb))[1];
    let linear = OutputSpace::Srgb.transfer().decode(reference);
    for space in OutputSpace::ALL {
        let encoded = center(&render_in(&raw, space))[1];
        let expected = space.transfer().encode(linear);
        assert!((encoded - expected).abs() < 0.002, "{}: {encoded} vs {expected}", space.name());
    }
}

#[test]
fn wider_spaces_need_less_signal_for_the_same_saturated_colour() {
    let raw = support::flat(RED);
    let narrow = center(&render_in(&raw, OutputSpace::Srgb));
    let wide = center(&render_in(&raw, OutputSpace::DisplayP3));
    assert!(wide[0] < narrow[0], "{narrow:?} vs {wide:?}");
    assert!(wide[1] > narrow[1], "{narrow:?} vs {wide:?}");
}

#[test]
fn neutral_recipe_is_the_same_as_no_recipe_at_all() {
    let raw = mosaic(bayer(), 128, 96, |x, y| [x / 128.0 * 0.5, y / 96.0 * 0.5, 0.2]);
    let explicit: Recipe = serde_json::from_str("{}").unwrap();
    let a = render(&raw, &Recipe::default());
    let b = render(&raw, &explicit);
    assert_eq!(a.pixels, b.pixels);
}

#[test]
fn rendering_twice_is_deterministic() {
    let raw = mosaic(bayer(), 128, 96, |x, y| [x / 128.0 * 0.5, y / 96.0 * 0.5, 0.2]);
    let edited = recipe(|r| {
        r.basic.clarity = 40.0;
        r.effects.grain.amount = 30.0;
    });
    assert_eq!(render(&raw, &edited).pixels, render(&raw, &edited).pixels);
}
