//! Healing, cloning and red eye. Repairs run before everything else, so the checks here are as
//! much about what comes after them as about the repair itself.

mod support;

use std::time::Duration;
use support::{bayer, luma, mosaic, pixel, render};
use unlight_engine::{
    Engine, ExportOptions, ExportedImage, GpuContext, Precision, SchedulerConfig, Session, ViewRequest, export,
    preview_difference,
};
use unlight_raw::RawImage;
use unlight_recipe::{HealMode, HealSpot, Recipe, RedEye};

const WIDTH: u32 = 400;
const HEIGHT: u32 = 300;

/// Light rising across the frame, as it does across a real sky.
fn ramp(x: f32) -> f32 {
    0.08 + 0.5 * x / WIDTH as f32
}

fn inside(x: f32, y: f32, center: [f32; 2], radius: f32) -> bool {
    (x - center[0]).hypot(y - center[1]) < radius
}

const BLEMISH: [f32; 2] = [120.0, 150.0];
const BLEMISH_RADIUS: f32 = 12.0;

fn clean_raw() -> RawImage {
    mosaic(bayer(), WIDTH, HEIGHT, |x, _| [ramp(x), ramp(x) * 0.95, ramp(x) * 0.85])
}

fn blemished_raw() -> RawImage {
    mosaic(bayer(), WIDTH, HEIGHT, |x, y| {
        let level = if inside(x, y, BLEMISH, BLEMISH_RADIUS) { 0.01 } else { ramp(x) };
        [level, level * 0.95, level * 0.85]
    })
}

fn spot(mode: HealMode, target: [f32; 2], source: [f32; 2], radius: f32) -> HealSpot {
    HealSpot {
        mode,
        target: [target[0] / WIDTH as f32, target[1] / HEIGHT as f32],
        source: [source[0] / WIDTH as f32, source[1] / HEIGHT as f32],
        radius: radius / WIDTH as f32,
        feather: 0.4,
        opacity: 1.0,
    }
}

fn healed(mode: HealMode) -> Recipe {
    Recipe { healing: vec![spot(mode, BLEMISH, [280.0, 150.0], 22.0)], ..Recipe::default() }
}

fn brightness_at(image: &ExportedImage, point: [f32; 2]) -> f32 {
    luma(pixel(image, point[0] as u32, point[1] as u32))
}

#[test]
fn a_heal_takes_its_tone_from_around_the_spot_not_from_the_patch() {
    let image = render(&blemished_raw(), &healed(HealMode::Heal));
    let clean = render(&clean_raw(), &Recipe::default());
    let difference = (brightness_at(&image, BLEMISH) - brightness_at(&clean, BLEMISH)).abs();
    assert!(difference < 0.015, "{difference}");
}

#[test]
fn a_heal_leaves_no_seam() {
    let image = render(&blemished_raw(), &healed(HealMode::Heal));
    let clean = render(&clean_raw(), &Recipe::default());
    let row = BLEMISH[1] as u32;
    let steps = |source: &ExportedImage| {
        (80..160).map(|x| (luma(pixel(source, x + 1, row)) - luma(pixel(source, x, row))).abs()).fold(0.0, f32::max)
    };
    assert!(steps(&image) < steps(&clean) + 0.006, "{} vs {}", steps(&image), steps(&clean));
}

#[test]
fn a_clone_copies_the_patch_as_it_is() {
    let image = render(&blemished_raw(), &healed(HealMode::Clone));
    let clean = render(&clean_raw(), &Recipe::default());
    let copied = brightness_at(&image, BLEMISH);
    assert!((copied - brightness_at(&clean, [280.0, 150.0])).abs() < 0.015, "{copied}");
    assert!(copied > brightness_at(&clean, BLEMISH) + 0.1, "a clone doesn't match the surroundings");
}

#[test]
fn a_clone_has_a_feathered_edge() {
    let image = render(&blemished_raw(), &healed(HealMode::Clone));
    let at = |x: f32| brightness_at(&image, [x, 150.0]);
    let center = at(BLEMISH[0]);
    let outside = at(BLEMISH[0] + 23.0);
    let halfway = at(BLEMISH[0] + 20.0);
    assert!(halfway < center && halfway > outside, "{outside} {halfway} {center}");
}

#[test]
fn opacity_leaves_part_of_what_was_there() {
    let mut recipe = healed(HealMode::Clone);
    recipe.healing[0].opacity = 0.5;
    let raw = blemished_raw();
    let (half, full, plain) = (render(&raw, &recipe), render(&raw, &healed(HealMode::Clone)), render(&raw, &Recipe::default()));
    let values = [&plain, &half, &full].map(|image| brightness_at(image, BLEMISH));
    assert!(values[0] < values[1] && values[1] < values[2], "{values:?}");
}

#[test]
fn a_repair_touches_nothing_outside_its_circle() {
    let raw = blemished_raw();
    let (image, plain) = (render(&raw, &healed(HealMode::Heal)), render(&raw, &Recipe::default()));
    for point in [[60.0, 40.0], [280.0, 150.0], [120.0, 260.0], [350.0, 280.0]] {
        assert_eq!(pixel(&image, point[0] as u32, point[1] as u32), pixel(&plain, point[0] as u32, point[1] as u32));
    }
}

#[test]
fn a_spot_with_no_opacity_changes_nothing() {
    let mut recipe = healed(HealMode::Heal);
    recipe.healing[0].opacity = 0.0;
    let raw = blemished_raw();
    assert_eq!(render(&raw, &recipe).pixels, render(&raw, &Recipe::default()).pixels);
}

const PUPIL: [f32; 2] = [100.0, 100.0];
const PUPIL_RADIUS: f32 = 10.0;

fn face(x: f32, y: f32) -> [f32; 3] {
    if inside(x, y, [PUPIL[0] + 3.0, PUPIL[1] - 3.0], 2.0) {
        [0.9, 0.9, 0.9]
    } else if inside(x, y, PUPIL, PUPIL_RADIUS) || inside(x, y, [170.0, 100.0], PUPIL_RADIUS) {
        [0.55, 0.05, 0.05]
    } else {
        [0.5, 0.3, 0.22]
    }
}

fn eye() -> RedEye {
    RedEye { center: [PUPIL[0] / 240.0, PUPIL[1] / 200.0], radius: 14.0 / 240.0, pupil_size: 0.9, darken: 0.6 }
}

fn redness([red, green, blue]: [f32; 3]) -> f32 {
    red - green.max(blue)
}

#[test]
fn red_eye_takes_the_red_out_of_the_pupil() {
    let raw = mosaic(bayer(), 240, 200, face);
    let recipe = Recipe { red_eye: vec![eye()], ..Recipe::default() };
    let (fixed, plain) = (render(&raw, &recipe), render(&raw, &Recipe::default()));
    let at = [PUPIL[0] as u32 - 4, PUPIL[1] as u32 + 4];
    assert!(redness(pixel(&plain, at[0], at[1])) > 0.3);
    assert!(redness(pixel(&fixed, at[0], at[1])) < 0.05, "{:?}", pixel(&fixed, at[0], at[1]));
}

#[test]
fn red_eye_darkens_more_the_further_its_slider_goes() {
    let raw = mosaic(bayer(), 240, 200, face);
    let with = |darken: f32| {
        let recipe = Recipe { red_eye: vec![RedEye { darken, ..eye() }], ..Recipe::default() };
        luma(pixel(&render(&raw, &recipe), PUPIL[0] as u32 - 4, PUPIL[1] as u32 + 4))
    };
    assert!(with(1.0) < with(0.0) - 0.02);
}

#[test]
fn red_eye_leaves_skin_catchlights_and_other_red_alone() {
    let raw = mosaic(bayer(), 240, 200, face);
    let recipe = Recipe { red_eye: vec![eye()], ..Recipe::default() };
    let (fixed, plain) = (render(&raw, &recipe), render(&raw, &Recipe::default()));
    let same = |x: u32, y: u32| (0..3).all(|c| (pixel(&fixed, x, y)[c] - pixel(&plain, x, y)[c]).abs() < 0.01);
    assert!(same(170, 100), "red outside the circle is someone's jumper");
    assert!(same(60, 100), "skin");
    assert!(same(PUPIL[0] as u32 + 3, PUPIL[1] as u32 - 3), "catchlight");
}

#[test]
fn later_tools_see_the_repaired_photo() {
    let size = 180.0;
    let center = [200.0, 150.0];
    let dark = mosaic(bayer(), WIDTH, HEIGHT, |x, y| {
        let level = if inside(x, y, center, 45.0) { 0.01 } else { 0.25 };
        [level; 3]
    });
    let flat = mosaic(bayer(), WIDTH, HEIGHT, |_, _| [0.25; 3]);
    let mut recipe = Recipe { healing: vec![spot(HealMode::Clone, center, [70.0, 80.0], 60.0)], ..Recipe::default() };
    recipe.basic.shadows = 80.0;
    recipe.basic.highlights = -60.0;
    recipe.healing[0].feather = 0.1;
    let mut plain = Recipe::default();
    plain.basic.shadows = 80.0;
    plain.basic.highlights = -60.0;
    let (fixed, reference) = (render(&dark, &recipe), render(&flat, &plain));
    let unrepaired = render(&dark, &plain);
    let near = [center[0] + size * 0.5, center[1]];
    let error = (brightness_at(&fixed, near) - brightness_at(&reference, near)).abs();
    let leak = (brightness_at(&unrepaired, near) - brightness_at(&reference, near)).abs();
    assert!(error < leak * 0.5 || error < 0.003, "{error} vs {leak}");
}

fn open(raw: &RawImage) -> Session {
    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F32).unwrap();
    Session::open(engine, raw, &Recipe::default()).unwrap()
}

/// A view at 100%, so it comes from the lazily filled full-resolution image.
fn zoomed(session: &mut Session, recipe: &Recipe, origin: [u32; 2], size: [u32; 2]) -> Vec<f32> {
    let [width, height] = [session.source.width as f32, session.source.height as f32];
    let region = [
        origin[0] as f32 / width,
        origin[1] as f32 / height,
        size[0] as f32 / width,
        size[1] as f32 / height,
    ];
    let target = session.view_target(size[0], size[1]);
    session.render(recipe, &ViewRequest { region, ..ViewRequest::default() }, &target, None).unwrap();
    session.read_view(&target).unwrap().iter().map(|value| f32::from(*value) / 255.0).collect()
}

fn export_crop(image: &ExportedImage, origin: [u32; 2], size: [u32; 2]) -> Vec<f32> {
    let mut values = Vec::with_capacity((size[0] * size[1] * 4) as usize);
    for y in 0..size[1] {
        for x in 0..size[0] {
            let [r, g, b] = pixel(image, origin[0] + x, origin[1] + y);
            values.extend([r, g, b, 1.0]);
        }
    }
    values
}

fn mean_gap(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

/// Spans a cell boundary of the lazy image, with its patch far enough off to be in another.
fn large_scene() -> (RawImage, Recipe) {
    let raw = mosaic(bayer(), 1400, 1000, |x, y| {
        let level = if inside(x, y, [512.0, 512.0], 18.0) {
            0.01
        } else {
            0.1 + 0.4 * x / 1400.0 + 0.05 * (x * 0.13).sin() * (y * 0.11).sin()
        };
        [level, level, level * 0.9]
    });
    let spot = HealSpot {
        mode: HealMode::Heal,
        target: [512.0 / 1400.0, 512.0 / 1000.0],
        source: [1100.0 / 1400.0, 200.0 / 1000.0],
        radius: 34.0 / 1400.0,
        feather: 0.4,
        opacity: 1.0,
    };
    (raw, Recipe { healing: vec![spot], ..Recipe::default() })
}

#[test]
fn a_zoomed_view_heals_as_the_export_does_across_a_cell_edge_from_a_cell_not_yet_made() {
    let (raw, recipe) = large_scene();
    let exported = render(&raw, &recipe);
    let mut session = open(&raw);
    let (origin, size) = ([440, 440], [144, 144]);
    let shown = zoomed(&mut session, &recipe, origin, size);
    let gap = mean_gap(&shown, &export_crop(&exported, origin, size));
    assert!(gap < 0.01, "{gap}");
}

#[test]
fn moving_a_spot_on_a_zoomed_view_leaves_nothing_behind() {
    let (raw, recipe) = large_scene();
    let (origin, size) = ([440, 440], [144, 144]);
    let mut session = open(&raw);
    zoomed(&mut session, &recipe, origin, size);
    let mut moved = recipe.clone();
    moved.healing[0].target = [470.0 / 1400.0, 470.0 / 1000.0];
    let shown = zoomed(&mut session, &moved, origin, size);
    let mut fresh = open(&raw);
    assert_eq!(shown, zoomed(&mut fresh, &moved, origin, size));
}

#[test]
fn deleting_every_spot_on_a_zoomed_view_restores_the_photo() {
    let (raw, recipe) = large_scene();
    let (origin, size) = ([440, 440], [144, 144]);
    let mut session = open(&raw);
    let plain = zoomed(&mut session, &Recipe::default(), origin, size);
    assert_ne!(plain, zoomed(&mut session, &recipe, origin, size));
    assert_eq!(plain, zoomed(&mut session, &Recipe::default(), origin, size));
}

#[test]
fn moving_a_spot_on_a_fit_view_leaves_nothing_behind() {
    let raw = blemished_raw();
    let mut session = open(&raw);
    let view = |session: &mut Session, recipe: &Recipe| {
        let target = session.view_target(200, 150);
        session.render(recipe, &ViewRequest::default(), &target, None).unwrap();
        session.read_view(&target).unwrap()
    };
    view(&mut session, &healed(HealMode::Heal));
    let mut moved = healed(HealMode::Heal);
    moved.healing[0].target = [0.7, 0.3];
    let shown = view(&mut session, &moved);
    let mut fresh = open(&raw);
    assert_eq!(shown, view(&mut fresh, &moved));
}

/// Odd sizes at more than one pyramid level, which is where a mip's texels stop lining up with
/// half of the level above.
fn odd_pyramid() -> RawImage {
    mosaic(bayer(), 2102, 1398, |x, y| {
        let level = 0.2 + 0.25 * ((x * 0.031).sin() * (y * 0.027).cos()).abs();
        [level, level * 0.9, level * 0.8]
    })
}

fn hard_clone(target: [f32; 2], source: [f32; 2]) -> HealSpot {
    HealSpot { mode: HealMode::Clone, target, source, radius: 0.03, feather: 0.0, opacity: 1.0 }
}

#[test]
fn moving_a_spot_leaves_nothing_behind_in_any_level_of_an_odd_sized_pyramid() {
    let raw = odd_pyramid();
    let spot = |target| Recipe { healing: vec![hard_clone(target, [0.2, 0.2])], ..Recipe::default() };
    let view = |session: &mut Session, recipe: &Recipe, width: u32, height: u32| {
        let target = session.view_target(width, height);
        session.render(recipe, &ViewRequest::default(), &target, None).unwrap();
        session.read_view(&target).unwrap()
    };
    let moved = spot([0.93, 0.9]);
    for (width, height) in [(520, 346), (260, 173)] {
        let mut session = open(&raw);
        view(&mut session, &spot([0.8, 0.8]), width, height);
        let shown = view(&mut session, &moved, width, height);
        let mut fresh = open(&raw);
        assert_eq!(shown, view(&mut fresh, &moved, width, height), "at {width}×{height}");
    }
}

#[test]
fn a_healed_photo_previews_like_it_exports() {
    let (raw, recipe) = large_scene();
    let difference = preview_difference(&raw, &recipe, 700).unwrap();
    assert!(difference.mean < 1.5, "{difference:?}");
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

#[test]
fn tile_size_never_shows_through_repairs() {
    let (raw, mut recipe) = large_scene();
    recipe.healing.push(HealSpot { mode: HealMode::Clone, target: [0.2, 0.2], source: [0.3, 0.7], ..recipe.healing[0] });
    recipe.red_eye.push(RedEye { center: [0.5, 0.5], radius: 0.05, pupil_size: 0.8, darken: 0.5 });
    let small = export(&raw, &recipe, &fixed_tiles(128)).unwrap();
    let large = export(&raw, &recipe, &fixed_tiles(1024)).unwrap();
    assert_eq!(small.pixels, large.pixels);
}

#[test]
fn a_found_patch_heals_about_as_well_as_a_chosen_one() {
    let raw = blemished_raw();
    let mut session = open(&raw);
    let (target, radius) = ([BLEMISH[0] / WIDTH as f32, BLEMISH[1] / HEIGHT as f32], 22.0 / WIDTH as f32);
    let source = session.find_heal_source(&Recipe::default(), target, radius).unwrap();
    let apart = ((source[0] - target[0]) * WIDTH as f32).hypot((source[1] - target[1]) * HEIGHT as f32);
    assert!(apart > 44.0, "{source:?}");
    let spot = HealSpot { mode: HealMode::Heal, target, source, radius, feather: 0.4, opacity: 1.0 };
    let image = render(&raw, &Recipe { healing: vec![spot], ..Recipe::default() });
    let clean = render(&clean_raw(), &Recipe::default());
    assert!((brightness_at(&image, BLEMISH) - brightness_at(&clean, BLEMISH)).abs() < 0.015);
}

#[test]
fn a_found_patch_stays_on_the_photo_near_a_corner() {
    let raw = blemished_raw();
    let mut session = open(&raw);
    let source = session.find_heal_source(&Recipe::default(), [0.03, 0.04], 0.02).unwrap();
    assert!(source.iter().all(|value| (0.0..=1.0).contains(value)), "{source:?}");
}
