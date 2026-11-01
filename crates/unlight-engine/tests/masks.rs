//! Local adjustments: what each one touches, what it leaves alone, and how masks combine. All
//! of it is pinned on synthetic raws, so it runs without sample files.

mod support;

use support::{bayer, center, chroma, luma, mosaic, pixel, render};
use std::time::Duration;
use unlight_engine::{
    Engine, ExportOptions, ExportedImage, Geometry, GpuContext, MaskRaster, MaskRasterSource, Optics, Precision,
    SchedulerConfig, Session, ViewRequest, export, preview_difference,
};
use unlight_raw::RawImage;
use unlight_recipe::{BrushDab, Crop, LocalAdjustments, Mask, MaskComponent, MaskShape, Recipe, Transform};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 160;

/// Blocks of three tones with fine detail on top, so every control finds something to act on.
fn busy(x: f32, y: f32) -> [f32; 3] {
    let block = ((x / 40.0).floor() + (y / 40.0).floor()).rem_euclid(3.0);
    let tone = [0.015, 0.14, 0.75][block as usize];
    let detail = 1.0 + 0.18 * (x * 1.1).sin() * (y * 0.9).sin();
    [tone * detail * 1.05, tone * detail, tone * detail * 0.75]
}

fn busy_raw() -> RawImage {
    mosaic(bayer(), WIDTH, HEIGHT, busy)
}

fn flat_raw(width: u32, height: u32) -> RawImage {
    mosaic(bayer(), width, height, |_, _| [0.15, 0.15, 0.15])
}

fn add(shape: MaskShape) -> MaskComponent {
    MaskComponent { shape, subtract: false }
}

fn take(shape: MaskShape) -> MaskComponent {
    MaskComponent { shape, subtract: true }
}

/// All of the left, none of the right, with a short ramp between.
fn left_half() -> MaskShape {
    MaskShape::Linear { start: [0.45, 0.5], end: [0.55, 0.5] }
}

fn disc(center: [f32; 2], radius: f32) -> MaskShape {
    MaskShape::Radial { center, radii: [radius; 2], angle: 0.0, feather: 0.1, inverted: false }
}

fn mask(components: Vec<MaskComponent>, adjustments: LocalAdjustments) -> Mask {
    Mask { components, adjustments, ..Mask::default() }
}

fn exposure(stops: f32) -> LocalAdjustments {
    LocalAdjustments { exposure: stops, ..LocalAdjustments::default() }
}

fn masked(masks: Vec<Mask>) -> Recipe {
    Recipe { masks, ..Recipe::default() }
}

fn mean_change(a: &ExportedImage, b: &ExportedImage, columns: std::ops::Range<u32>) -> f32 {
    let mut total = 0.0;
    let mut count = 0.0;
    for y in 8..a.height - 8 {
        for x in columns.clone() {
            let (one, two) = (pixel(a, x, y), pixel(b, x, y));
            total += (0..3).map(|channel| (one[channel] - two[channel]).abs()).sum::<f32>();
            count += 3.0;
        }
    }
    total / count
}

fn left_right_change(adjustments: LocalAdjustments, raw: &RawImage) -> (f32, f32) {
    let edited = render(raw, &masked(vec![mask(vec![add(left_half())], adjustments)]));
    let plain = render(raw, &Recipe::default());
    let width = plain.width;
    (mean_change(&plain, &edited, 8..width * 2 / 5), mean_change(&plain, &edited, width * 3 / 5..width - 8))
}

fn assert_only_left_changes(name: &str, adjustments: LocalAdjustments) {
    let (left, right) = left_right_change(adjustments, &busy_raw());
    assert!(left > 5e-4, "{name} did nothing inside its mask: {left}");
    assert_eq!(right, 0.0, "{name} reached past its mask");
}

macro_rules! confined {
    ($($name:ident: $field:ident = $value:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let adjustments = LocalAdjustments { $field: $value, ..LocalAdjustments::default() };
                assert_only_left_changes(stringify!($field), adjustments);
            }
        )*
    };
}

confined! {
    temperature_stays_inside_its_mask: temperature = 80.0;
    tint_stays_inside_its_mask: tint = 80.0;
    exposure_stays_inside_its_mask: exposure = 1.5;
    contrast_stays_inside_its_mask: contrast = 80.0;
    highlights_stay_inside_their_mask: highlights = -80.0;
    shadows_stay_inside_their_mask: shadows = 80.0;
    whites_stay_inside_their_mask: whites = 80.0;
    blacks_stay_inside_their_mask: blacks = 80.0;
    texture_stays_inside_its_mask: texture = 80.0;
    clarity_stays_inside_its_mask: clarity = 80.0;
    dehaze_stays_inside_its_mask: dehaze = 80.0;
    hue_stays_inside_its_mask: hue = 60.0;
    saturation_stays_inside_its_mask: saturation = 80.0;
    sharpness_stays_inside_its_mask: sharpness = 80.0;
}

/// Adds repeatable sensor noise, enough for the photo's noise model to find.
fn grainy(mut raw: RawImage) -> RawImage {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for sample in &mut raw.samples {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let noise = ((state >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 400.0;
        *sample = (f32::from(*sample) + noise).clamp(0.0, 16383.0) as u16;
    }
    raw
}

fn row_roughness(image: &ExportedImage, columns: std::ops::Range<u32>) -> f32 {
    let mut total = 0.0;
    let mut count = 0.0;
    for y in 8..image.height - 8 {
        for x in columns.clone() {
            total += (luma(pixel(image, x + 1, y)) - luma(pixel(image, x, y))).abs();
            count += 1.0;
        }
    }
    total / count
}

#[test]
fn noise_reduction_stays_inside_its_mask() {
    let raw = grainy(mosaic(bayer(), 512, 256, |_, _| [0.2, 0.2, 0.2]));
    let adjustments = LocalAdjustments { noise: 100.0, ..LocalAdjustments::default() };
    let edited = render(&raw, &masked(vec![mask(vec![add(left_half())], adjustments)]));
    let plain = render(&raw, &Recipe::default());
    let (inside, outside) = (40..200, 312..472);
    assert!(
        row_roughness(&edited, inside.clone()) < row_roughness(&plain, inside.clone()) * 0.8,
        "{} vs {}",
        row_roughness(&edited, inside.clone()),
        row_roughness(&plain, inside)
    );
    assert_eq!(mean_change(&plain, &edited, outside), 0.0);
}

#[test]
fn a_mask_can_give_back_noise_the_global_slider_took() {
    let raw = grainy(mosaic(bayer(), 512, 256, |_, _| [0.2, 0.2, 0.2]));
    let mut quiet = Recipe::default();
    quiet.detail.noise_reduction.luminance = 80.0;
    let adjustments = LocalAdjustments { noise: -100.0, ..LocalAdjustments::default() };
    let mut edited = quiet.clone();
    edited.masks.push(mask(vec![add(left_half())], adjustments));
    let (smooth, rough) = (render(&raw, &quiet), render(&raw, &edited));
    let inside = 40..200;
    assert!(row_roughness(&rough, inside.clone()) > row_roughness(&smooth, inside) * 1.2);
    assert_eq!(mean_change(&smooth, &rough, 312..472), 0.0);
}

/// True luminance, since weighing gamma-encoded channels misjudges a saturated colour.
fn linear_luminance(encoded: [f32; 3]) -> f32 {
    luma(encoded.map(|value| if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }))
}

#[test]
fn temperature_warms_without_brightening() {
    let raw = flat_raw(96, 96);
    let warm = LocalAdjustments { temperature: 100.0, ..LocalAdjustments::default() };
    let edited = render(&raw, &masked(vec![mask(vec![add(disc([0.5, 0.5], 0.4))], warm)]));
    let plain = render(&raw, &Recipe::default());
    let [red, _, blue] = center(&edited);
    assert!(red > blue + 0.05, "{:?}", center(&edited));
    let (warmed, original) = (linear_luminance(center(&edited)), linear_luminance(center(&plain)));
    assert!((warmed / original - 1.0).abs() < 0.05, "{original} -> {warmed}");
}

/// Everything the camera saw, so a mask can be compared with the slider it mirrors.
fn everywhere() -> Vec<MaskComponent> {
    vec![add(disc([0.5, 0.5], 2.0))]
}

fn assert_same_as_global(name: &str, global: impl FnOnce(&mut Recipe), local: LocalAdjustments) {
    let raw = busy_raw();
    let mut slider = Recipe::default();
    global(&mut slider);
    let (by_slider, by_mask) = (render(&raw, &slider), render(&raw, &masked(vec![mask(everywhere(), local)])));
    let worst = by_slider.pixels.iter().zip(&by_mask.pixels).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(worst < 2e-3, "{name} differs from its global slider by {worst}");
}

macro_rules! mirrors_global {
    ($($name:ident: $field:ident = $value:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let local = LocalAdjustments { $field: $value, ..LocalAdjustments::default() };
                assert_same_as_global(stringify!($field), |recipe| recipe.basic.$field = $value, local);
            }
        )*
    };
}

mirrors_global! {
    a_full_mask_of_exposure_matches_the_slider: exposure = 1.2;
    a_full_mask_of_contrast_matches_the_slider: contrast = 60.0;
    a_full_mask_of_highlights_matches_the_slider: highlights = -70.0;
    a_full_mask_of_shadows_matches_the_slider: shadows = 70.0;
    a_full_mask_of_whites_matches_the_slider: whites = 50.0;
    a_full_mask_of_blacks_matches_the_slider: blacks = -50.0;
    a_full_mask_of_texture_matches_the_slider: texture = 80.0;
    a_full_mask_of_clarity_matches_the_slider: clarity = 80.0;
    a_full_mask_of_dehaze_matches_the_slider: dehaze = 60.0;
    a_full_mask_of_saturation_matches_the_slider: saturation = 50.0;
}

#[test]
fn local_sharpness_adds_to_global_sharpening() {
    let raw = busy_raw();
    let mut slider = Recipe::default();
    slider.detail.sharpening.amount += 60.0;
    let local = LocalAdjustments { sharpness: 60.0, ..LocalAdjustments::default() };
    let (by_slider, by_mask) = (render(&raw, &slider), render(&raw, &masked(vec![mask(everywhere(), local)])));
    let worst = by_slider.pixels.iter().zip(&by_mask.pixels).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(worst < 2e-3, "{worst}");
}

#[test]
fn negative_sharpness_softens() {
    let raw = busy_raw();
    let local = LocalAdjustments { sharpness: -100.0, ..LocalAdjustments::default() };
    let edited = render(&raw, &masked(vec![mask(vec![add(left_half())], local)]));
    let plain = render(&raw, &Recipe::default());
    assert!(row_roughness(&edited, 8..120) < row_roughness(&plain, 8..120));
}

#[test]
fn offsets_add_to_the_global_setting() {
    let raw = flat_raw(96, 96);
    let mut both = masked(vec![mask(vec![add(disc([0.5, 0.5], 0.4))], exposure(1.0))]);
    both.basic.exposure = 1.0;
    let mut global = Recipe::default();
    global.basic.exposure = 2.0;
    let (a, b) = (center(&render(&raw, &both)), center(&render(&raw, &global)));
    assert!((a[1] - b[1]).abs() < 1e-3, "{a:?} {b:?}");
}

#[test]
fn overlapping_masks_add_up() {
    let raw = flat_raw(96, 96);
    let shape = || vec![add(disc([0.5, 0.5], 0.4))];
    let two = masked(vec![mask(shape(), exposure(0.5)), mask(shape(), exposure(0.5))]);
    let one = masked(vec![mask(shape(), exposure(1.0))]);
    let (a, b) = (center(&render(&raw, &two)), center(&render(&raw, &one)));
    assert!((a[1] - b[1]).abs() < 1e-3, "{a:?} {b:?}");
}

fn brightened_at(recipe: &Recipe, raw: &RawImage, point: [u32; 2]) -> f32 {
    let plain = render(raw, &Recipe::default());
    let edited = render(raw, recipe);
    luma(pixel(&edited, point[0], point[1])) - luma(pixel(&plain, point[0], point[1]))
}

#[test]
fn subtracting_cuts_a_hole_in_a_mask() {
    let raw = flat_raw(200, 100);
    let recipe = masked(vec![mask(vec![add(left_half()), take(disc([0.2, 0.5], 0.1))], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [40, 50]).abs() < 1e-3);
    assert!(brightened_at(&recipe, &raw, [40, 10]) > 0.1);
}

#[test]
fn adding_widens_a_mask() {
    let raw = flat_raw(200, 100);
    let recipe = masked(vec![mask(vec![add(left_half()), add(disc([0.8, 0.5], 0.1))], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [160, 50]) > 0.1);
    assert!(brightened_at(&recipe, &raw, [40, 50]) > 0.1);
    assert!(brightened_at(&recipe, &raw, [120, 50]).abs() < 1e-3);
}

#[test]
fn a_mask_of_only_subtractions_selects_nothing() {
    let raw = flat_raw(96, 96);
    let recipe = masked(vec![mask(vec![take(disc([0.5, 0.5], 0.4))], exposure(2.0))]);
    assert_eq!(render(&raw, &recipe).pixels, render(&raw, &Recipe::default()).pixels);
}

#[test]
fn an_empty_mask_is_a_no_op() {
    let raw = busy_raw();
    let recipe = masked(vec![mask(Vec::new(), exposure(2.0))]);
    assert_eq!(render(&raw, &recipe).pixels, render(&raw, &Recipe::default()).pixels);
}

#[test]
fn a_neutral_mask_is_a_no_op() {
    let raw = busy_raw();
    let recipe = masked(vec![mask(vec![add(left_half())], LocalAdjustments::default())]);
    assert_eq!(render(&raw, &recipe).pixels, render(&raw, &Recipe::default()).pixels);
}

#[test]
fn a_disabled_mask_does_nothing() {
    let raw = busy_raw();
    let mut off = mask(vec![add(left_half())], exposure(2.0));
    off.enabled = false;
    assert_eq!(render(&raw, &masked(vec![off])).pixels, render(&raw, &Recipe::default()).pixels);
}

#[test]
fn a_radial_mask_inverts() {
    let raw = flat_raw(120, 120);
    let shape = MaskShape::Radial { center: [0.5, 0.5], radii: [0.2; 2], angle: 0.0, feather: 0.1, inverted: true };
    let recipe = masked(vec![mask(vec![add(shape)], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [60, 60]).abs() < 1e-3);
    assert!(brightened_at(&recipe, &raw, [8, 8]) > 0.1);
}

#[test]
fn a_rotated_ellipse_lies_along_its_angle() {
    let raw = flat_raw(200, 200);
    let shape = MaskShape::Radial { center: [0.5, 0.5], radii: [0.4, 0.08], angle: 90.0, feather: 0.1, inverted: false };
    let recipe = masked(vec![mask(vec![add(shape)], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [100, 40]) > 0.1);
    assert!(brightened_at(&recipe, &raw, [40, 100]).abs() < 1e-3);
}

#[test]
fn a_circle_stays_round_on_a_wide_frame() {
    let raw = flat_raw(400, 100);
    let recipe = masked(vec![mask(vec![add(disc([0.5, 0.5], 0.1))], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [230, 50]) > 0.1, "30 px right of a 40 px circle");
    assert!(brightened_at(&recipe, &raw, [260, 50]).abs() < 1e-3, "60 px right of it");
}

#[test]
fn a_gradient_is_measured_square_on_a_wide_frame() {
    let raw = flat_raw(400, 100);
    let shape = MaskShape::Linear { start: [0.5, 0.5], end: [0.625, 1.0] };
    let recipe = masked(vec![mask(vec![add(shape)], exposure(2.0))]);
    let edited = render(&raw, &recipe);
    let along = |x: u32, y: u32| luma(pixel(&edited, x, y));
    assert!((along(250, 25) - along(200, 75)).abs() < 0.01, "the isolines run across the gradient's axis");
}

#[test]
fn a_luminance_range_picks_tones_from_the_scene() {
    let raw = mosaic(bayer(), 200, 100, |x, _| if x < 100.0 { [0.02; 3] } else { [0.5; 3] });
    let shape = MaskShape::LuminanceRange { low: 0.6, high: 1.0, smoothness: 0.2 };
    let recipe = masked(vec![mask(vec![add(shape)], exposure(-1.0))]);
    assert!(brightened_at(&recipe, &raw, [150, 50]) < -0.05);
    assert!(brightened_at(&recipe, &raw, [50, 50]).abs() < 1e-3);
}

#[test]
fn a_colour_range_picks_the_sampled_colour() {
    const RED: [f32; 3] = [0.5, 0.05, 0.05];
    const BLUE: [f32; 3] = [0.05, 0.05, 0.5];
    let raw = mosaic(bayer(), 200, 100, |x, _| if x < 100.0 { RED } else { BLUE });
    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F32).unwrap();
    let mut session = Session::open(engine, &raw, &Recipe::default()).unwrap();
    let sample = session.scene_color(&Recipe::default(), [0.25, 0.5]).unwrap();
    let shape = MaskShape::ColorRange { samples: vec![sample], tolerance: 0.4 };
    let recipe = masked(vec![mask(vec![add(shape)], exposure(1.5))]);
    assert!(brightened_at(&recipe, &raw, [50, 50]) > 0.05);
    assert!(brightened_at(&recipe, &raw, [150, 50]).abs() < 1e-3);
}

#[test]
fn a_range_mask_refines_a_shape_by_subtracting_the_tones_it_should_not_touch() {
    let raw = mosaic(bayer(), 200, 100, |x, _| if x < 50.0 { [0.02; 3] } else { [0.5; 3] });
    let darks = MaskShape::LuminanceRange { low: 0.0, high: 0.5, smoothness: 0.1 };
    let recipe = masked(vec![mask(vec![add(left_half()), take(darks)], exposure(-1.0))]);
    assert!(brightened_at(&recipe, &raw, [25, 50]).abs() < 1e-3);
    assert!(brightened_at(&recipe, &raw, [70, 50]) < -0.05);
    assert!(brightened_at(&recipe, &raw, [170, 50]).abs() < 1e-3);
}

fn dab(position: [f32; 2], radius: f32) -> BrushDab {
    BrushDab { position, radius, feather: 0.2, flow: 1.0, erase: false, auto_mask: false }
}

#[test]
fn a_brush_paints_where_it_went_and_an_eraser_takes_it_back() {
    let raw = flat_raw(200, 100);
    let stroke: Vec<BrushDab> = (0..40).map(|step| dab([0.1 + step as f32 * 0.02, 0.5], 0.04)).collect();
    let mut erased = stroke.clone();
    erased.push(BrushDab { erase: true, ..dab([0.5, 0.5], 0.06) });
    let recipe = masked(vec![mask(vec![add(MaskShape::Brush { dabs: erased })], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [40, 50]) > 0.1);
    assert!(brightened_at(&recipe, &raw, [100, 50]).abs() < 1e-3);
    assert!(brightened_at(&recipe, &raw, [40, 10]).abs() < 1e-3);
}

#[test]
fn flow_builds_up_with_each_pass() {
    let raw = flat_raw(120, 120);
    let light = BrushDab { flow: 0.2, ..dab([0.5, 0.5], 0.2) };
    let strength = |passes: usize| {
        let recipe = masked(vec![mask(vec![add(MaskShape::Brush { dabs: vec![light; passes] })], exposure(2.0))]);
        brightened_at(&recipe, &raw, [60, 60])
    };
    let (one, four) = (strength(1), strength(4));
    assert!(one > 0.0 && four > one * 2.0, "{one} {four}");
}

#[test]
fn auto_mask_keeps_a_dab_to_the_colour_under_its_centre() {
    let raw = mosaic(bayer(), 200, 100, |x, _| if x < 100.0 { [0.5, 0.05, 0.05] } else { [0.05, 0.05, 0.5] });
    let stroke = BrushDab { auto_mask: true, ..dab([0.45, 0.5], 0.3) };
    let recipe = masked(vec![mask(vec![add(MaskShape::Brush { dabs: vec![stroke] })], exposure(2.0))]);
    assert!(brightened_at(&recipe, &raw, [70, 50]) > 0.05);
    assert!(brightened_at(&recipe, &raw, [130, 50]).abs() < 1e-3);
}

fn open(raw: &RawImage) -> Session {
    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F32).unwrap();
    Session::open(engine, raw, &Recipe::default()).unwrap()
}

fn view(session: &mut Session, recipe: &Recipe, size: [u32; 2]) -> Vec<u8> {
    let target = session.view_target(size[0], size[1]);
    session.render(recipe, &ViewRequest::default(), &target, None).unwrap();
    session.read_view(&target).unwrap()
}

#[test]
fn a_stroke_painted_in_steps_matches_one_painted_at_once() {
    let raw = flat_raw(256, 128);
    let dabs: Vec<BrushDab> = (0..60)
        .map(|step| {
            let t = step as f32 / 60.0;
            BrushDab { flow: 0.3, erase: step % 7 == 6, ..dab([0.1 + 0.8 * t, 0.3 + 0.4 * t], 0.03) }
        })
        .collect();
    let stroke = |count: usize| {
        let shape = MaskShape::Brush { dabs: dabs[..count].to_vec() };
        masked(vec![mask(vec![add(shape)], exposure(1.0))])
    };
    let mut stepped = open(&raw);
    let mut last = Vec::new();
    for count in [1, 10, 11, 40, 60] {
        last = view(&mut stepped, &stroke(count), [256, 128]);
    }
    let mut whole = open(&raw);
    assert_eq!(last, view(&mut whole, &stroke(60), [256, 128]));
}

#[test]
fn only_a_mask_rebuilds_the_fields() {
    let raw = busy_raw();
    let mut session = open(&raw);
    let mut recipe = masked(vec![mask(vec![add(left_half())], exposure(1.0))]);
    view(&mut session, &recipe, [160, 80]);
    let settled = session.mask_rebuilds();
    assert!(settled > 0);
    for step in 0..4 {
        recipe.basic.exposure = step as f32 * 0.3;
        recipe.basic.contrast = step as f32 * 10.0;
        recipe.white_balance = unlight_recipe::WhiteBalance::Custom { temperature: 4000.0 + step as f32 * 500.0, tint: 5.0 };
        view(&mut session, &recipe, [160, 80]);
    }
    assert_eq!(session.mask_rebuilds(), settled);
    recipe.masks[0].adjustments.exposure = 2.0;
    view(&mut session, &recipe, [160, 80]);
    assert_eq!(session.mask_rebuilds(), settled + 1);
}

#[test]
fn a_recipe_without_masks_never_builds_a_field() {
    let raw = busy_raw();
    let mut session = open(&raw);
    view(&mut session, &Recipe::default(), [160, 80]);
    assert_eq!(session.mask_rebuilds(), 0);
}

#[test]
fn a_deleted_mask_stops_applying() {
    let raw = busy_raw();
    let mut session = open(&raw);
    let plain = view(&mut session, &Recipe::default(), [160, 80]);
    let edited = view(&mut session, &masked(vec![mask(vec![add(left_half())], exposure(1.0))]), [160, 80]);
    assert_ne!(plain, edited);
    assert_eq!(plain, view(&mut session, &Recipe::default(), [160, 80]));
}

/// The output pixel a source position lands on under a recipe's geometry.
fn output_pixel(recipe: &Recipe, raw: &RawImage, image: &ExportedImage, uv: [f32; 2]) -> [u32; 2] {
    let aspect = raw.width as f32 / raw.height as f32;
    let geometry = Geometry::new(&recipe.crop, &recipe.transform, &recipe.lens, aspect, &Optics::default());
    let view = geometry.view_from_source(uv, aspect);
    [(view[0] * image.width as f32) as u32, (view[1] * image.height as f32) as u32]
}

fn assert_follows(mut recipe: Recipe) {
    let raw = flat_raw(320, 240);
    let target = [0.62, 0.44];
    recipe.masks = vec![mask(vec![add(disc(target, 0.06))], exposure(2.0))];
    let mut unmasked = recipe.clone();
    unmasked.masks.clear();
    let (edited, plain) = (render(&raw, &recipe), render(&raw, &unmasked));
    let [x, y] = output_pixel(&recipe, &raw, &edited, target);
    assert!(luma(pixel(&edited, x, y)) > luma(pixel(&plain, x, y)) + 0.1, "not at {x},{y}");
    let away = output_pixel(&recipe, &raw, &edited, [0.42, 0.62]);
    let (at, was) = (pixel(&edited, away[0], away[1]), pixel(&plain, away[0], away[1]));
    assert!((luma(at) - luma(was)).abs() < 1e-3, "spread to {away:?}");
}

#[test]
fn a_mask_follows_the_crop() {
    let crop = Crop { left: 0.3, top: 0.2, right: 0.9, bottom: 0.8, angle: 0.0 };
    assert_follows(Recipe { crop, ..Recipe::default() });
}

#[test]
fn a_mask_follows_straightening() {
    let crop = Crop { left: 0.2, top: 0.15, right: 0.85, bottom: 0.85, angle: 12.0 };
    assert_follows(Recipe { crop, ..Recipe::default() });
}

#[test]
fn a_mask_follows_perspective() {
    let transform = Transform { vertical: 30.0, rotate: 4.0, ..Transform::default() };
    assert_follows(Recipe { transform, ..Recipe::default() });
}

#[test]
fn a_mask_follows_lens_distortion() {
    let mut recipe = Recipe::default();
    recipe.lens.manual_distortion = 40.0;
    assert_follows(recipe);
}

#[derive(Debug)]
struct OneRaster;

impl MaskRasterSource for OneRaster {
    fn load(&self, id: &str) -> Option<MaskRaster> {
        (id == "left").then(|| MaskRaster { width: 2, height: 1, coverage: vec![1.0, 0.0] })
    }
}

#[test]
fn a_raster_mask_loads_by_id() {
    let raw = flat_raw(200, 100);
    let named = |id: &str| {
        let shape = MaskShape::Raster { id: id.to_owned(), inverted: false };
        masked(vec![mask(vec![add(shape)], exposure(2.0))])
    };
    let options = ExportOptions { mask_rasters: Some(&OneRaster), ..ExportOptions::default() };
    let found = export(&raw, &named("left"), &options).unwrap();
    let plain = render(&raw, &Recipe::default());
    assert!(luma(pixel(&found, 10, 50)) > luma(pixel(&plain, 10, 50)) + 0.1);
    assert!((luma(pixel(&found, 190, 50)) - luma(pixel(&plain, 190, 50))).abs() < 1e-3);
    assert_eq!(export(&raw, &named("gone"), &options).unwrap().pixels, plain.pixels);
}

#[test]
fn the_overlay_shows_the_mask_in_red_without_changing_the_edit() {
    let raw = flat_raw(200, 100);
    let mut session = open(&raw);
    let recipe = masked(vec![mask(vec![add(left_half())], LocalAdjustments::default())]);
    let target = session.view_target(200, 100);
    let request = ViewRequest { mask_overlay: Some(0), ..ViewRequest::default() };
    session.render(&recipe, &request, &target, None).unwrap();
    let shown = session.read_view(&target).unwrap();
    let at = |x: usize, y: usize| &shown[(y * 200 + x) * 4..(y * 200 + x) * 4 + 3];
    assert!(at(40, 50)[0] > at(40, 50)[1].saturating_add(60), "{:?}", at(40, 50));
    assert!(at(160, 50)[0].abs_diff(at(160, 50)[1]) < 4, "{:?}", at(160, 50));
    session.render(&recipe, &ViewRequest::default(), &target, None).unwrap();
    let plain = session.read_view(&target).unwrap();
    assert!(plain[(50 * 200 + 40) * 4].abs_diff(plain[(50 * 200 + 40) * 4 + 1]) < 4);
}

fn fixed_tiles(edge: u32) -> ExportOptions {
    let schedule = SchedulerConfig {
        budget: Duration::from_secs(3600),
        min_edge: edge,
        max_edge: edge,
        initial_edge: edge,
    };
    ExportOptions { schedule, ..ExportOptions::default() }
}

fn every_kind_of_mask() -> Recipe {
    let everything = LocalAdjustments {
        exposure: 0.8,
        contrast: 30.0,
        clarity: 40.0,
        texture: 40.0,
        sharpness: 50.0,
        saturation: 30.0,
        temperature: 20.0,
        ..LocalAdjustments::default()
    };
    let stroke: Vec<BrushDab> = (0..30).map(|step| dab([0.2 + step as f32 * 0.02, 0.3], 0.05)).collect();
    masked(vec![
        mask(vec![add(left_half())], everything),
        mask(vec![add(MaskShape::Brush { dabs: stroke })], everything),
        mask(vec![add(disc([0.7, 0.6], 0.2)), take(disc([0.7, 0.6], 0.08))], everything),
        mask(vec![add(MaskShape::LuminanceRange { low: 0.5, high: 1.0, smoothness: 0.2 })], everything),
    ])
}

#[test]
fn tile_size_never_shows_through_masks() {
    let raw = mosaic(bayer(), 640, 400, busy);
    let recipe = every_kind_of_mask();
    let small = export(&raw, &recipe, &fixed_tiles(128)).unwrap();
    let large = export(&raw, &recipe, &fixed_tiles(1024)).unwrap();
    assert_eq!(small.pixels, large.pixels);
}

#[test]
fn a_masked_edit_previews_like_it_exports() {
    let raw = mosaic(bayer(), 1280, 800, busy);
    let plain = preview_difference(&raw, &Recipe::default(), 640).unwrap();
    let masked = preview_difference(&raw, &every_kind_of_mask(), 640).unwrap();
    assert!(masked.mean < plain.mean + 0.3, "{masked:?} against {plain:?} without masks");
}

#[test]
fn sixteen_masks_render_like_their_sum() {
    let raw = flat_raw(96, 96);
    let shape = || vec![add(disc([0.5, 0.5], 0.4))];
    let many = masked((0..16).map(|_| mask(shape(), exposure(0.125))).collect());
    let one = masked(vec![mask(shape(), exposure(2.0))]);
    let (a, b) = (center(&render(&raw, &many)), center(&render(&raw, &one)));
    assert!((a[1] - b[1]).abs() < 2e-3, "{a:?} {b:?}");
    assert!(chroma(a) < 0.02);
}

#[test]
fn outgrowing_the_coverage_stack_keeps_every_mask() {
    let raw = flat_raw(96, 96);
    let mut session = open(&raw);
    let shape = || vec![add(disc([0.5, 0.5], 0.4))];
    view(&mut session, &masked(vec![mask(shape(), exposure(0.2))]), [96, 96]);
    let grown = masked((0..9).map(|_| mask(shape(), exposure(0.2))).collect());
    let shown = view(&mut session, &grown, [96, 96]);
    let mut fresh = open(&raw);
    assert_eq!(shown, view(&mut fresh, &grown, [96, 96]));
}
