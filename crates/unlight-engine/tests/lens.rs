//! Lens and perspective tools on synthetic raws, where the true distortion is known exactly.
//! Scenes are warped by the CPU model and then corrected by the shader, so these also pin down
//! that the two agree.

mod support;

use std::{sync::Arc, time::Duration};
use support::{bayer, center, chroma, luma, mosaic, pixel, render};
use unlight_engine::{
    Engine, ExportOptions, ExportedImage, Geometry, GpuContext, LensProfile, LumaImage, Optics,
    PhotoLens, Precision, SchedulerConfig, Segment, Session, ViewRequest, detect_segments, export,
    join_collinear, measure_long_lines, straightness,
};
use unlight_raw::{
    EmbeddedLens, FixVignetteRadial, GainMap, Metadata, NikonLens, Opcode, Opcodes, PanasonicLens,
    RawImage, WarpPlane, WarpRectilinear,
};
use unlight_recipe::{Crop, GuideLine, LensCorrections, Recipe, Transform, Upright};

/// The Pixel 4 XL sample's WarpRectilinear.
const PIXEL_RADIAL: [f64; 4] = [
    0.980_540_573_596_954_3,
    0.052_180_830_389_261_25,
    -0.052_247_390_151_023_87,
    0.018_468_147_143_721_58,
];
const PIXEL_CENTER: [f64; 2] = [0.465_702_333_192_567_54, 0.502_597_316_530_168_5];
/// The GH5 sample's DistortionInfo, with its radius halved for half-size test images.
const GH5_LENS: PanasonicLens = PanasonicLens {
    scale: 1.004_906_771_344_455_3,
    a: 0.002_624_511_718_75,
    b: -0.009_521_484_375,
    c: 0.003_265_380_859_375,
    radius: 1620.0,
};

fn recipe(change: impl FnOnce(&mut Recipe)) -> Recipe {
    let mut recipe = Recipe::default();
    change(&mut recipe);
    recipe
}

fn warp_opcodes(radial: [f64; 4], tangential: [f64; 2], center: [f64; 2]) -> Opcodes {
    Opcodes {
        list3: vec![Opcode::WarpRectilinear(WarpRectilinear {
            planes: vec![WarpPlane { radial, tangential }],
            center_x: center[0],
            center_y: center[1],
        })],
        ..Opcodes::default()
    }
}

fn blank(width: u32, height: u32) -> RawImage {
    mosaic(bayer(), width, height, |_, _| [0.0; 3])
}

/// A lens's geometry on its own: no crop, transform or frame fit.
fn lens_geometry(profile: LensProfile, lens: &LensCorrections, aspect: f32) -> Geometry {
    let optics = Optics { profile: Some(Arc::new(profile)), correction: None };
    Geometry::new(&Crop::default(), &Transform::default(), lens, aspect, &optics)
}

/// Pushes a scene through a lens: what the sensor would record for a scene given in undistorted
/// view coordinates.
fn through_lens(
    template: &RawImage,
    geometry: &Geometry,
    scene: impl Fn(f32, f32) -> [f32; 3],
) -> RawImage {
    let (width, height) = (template.width, template.height);
    let aspect = width as f32 / height as f32;
    let mut raw = mosaic(bayer(), width, height, |x, y| {
        let source = [((x + 0.5) / width as f32 - 0.5) * aspect, (y + 0.5) / height as f32 - 0.5];
        let [ux, uy] = geometry.unwarp(source);
        scene(ux / aspect + 0.5, uy + 0.5)
    });
    raw.opcodes = template.opcodes.clone();
    raw.embedded_lens = template.embedded_lens.clone();
    raw.metadata = template.metadata.clone();
    raw
}

fn smooth_scene(u: f32, v: f32) -> [f32; 3] {
    let wave = |x: f32| 0.5 + 0.5 * (x * std::f32::consts::TAU).sin();
    [0.1 + 0.35 * wave(u * 3.0), 0.1 + 0.35 * wave(v * 2.5), 0.15 + 0.25 * wave((u + v) * 2.0)]
}

fn plain(width: u32, height: u32, scene: impl Fn(f32, f32) -> [f32; 3]) -> RawImage {
    mosaic(bayer(), width, height, |x, y| scene((x + 0.5) / width as f32, (y + 0.5) / height as f32))
}

fn worst_difference(a: &ExportedImage, b: &ExportedImage, margin: u32) -> f32 {
    let mut worst = 0.0f32;
    for y in margin..a.height - margin {
        for x in margin..a.width - margin {
            let (first, second) = (pixel(a, x, y), pixel(b, x, y));
            for channel in 0..3 {
                worst = worst.max((first[channel] - second[channel]).abs());
            }
        }
    }
    worst
}

#[test]
fn the_shader_undoes_exactly_what_the_cpu_model_says_a_lens_did() {
    let (width, height) = (1200, 800);
    let mut template = blank(width, height);
    template.opcodes = warp_opcodes(PIXEL_RADIAL, [0.004, -0.003], PIXEL_CENTER);
    let profile = PhotoLens::new(&template).file_profile().unwrap();
    let geometry = lens_geometry(profile, &LensCorrections::default(), 1.5);

    let warped = through_lens(&template, &geometry, smooth_scene);
    let corrected = render(&warped, &Recipe::default());
    let reference = render(&plain(width, height, smooth_scene), &Recipe::default());
    let worst = worst_difference(&corrected, &reference, 40);
    assert!(worst < 0.012, "worst difference {worst}");
}

#[test]
fn a_dng_warp_changes_the_render_only_when_present() {
    let (width, height) = (600, 400);
    let mut warped = plain(width, height, smooth_scene);
    let straight = render(&warped, &Recipe::default());
    warped.opcodes = warp_opcodes([0.95, 0.1, 0.0, 0.0], [0.0; 2], [0.5, 0.5]);
    let bent = render(&warped, &Recipe::default());
    assert!(worst_difference(&straight, &bent, 10) > 0.02);
}

fn panasonic_template(width: u32, height: u32) -> RawImage {
    let mut template = blank(width, height);
    template.embedded_lens = Some(EmbeddedLens::Panasonic(GH5_LENS));
    template
}

#[test]
fn corrections_left_off_change_nothing() {
    let (width, height) = (480, 360);
    let without = plain(width, height, smooth_scene);
    let mut with = without.clone();
    with.embedded_lens = Some(EmbeddedLens::Nikon(NikonLens { distortion: [0.0, 0.03, 0.03], vignette: [0.0; 3] }));
    let (first, second) = (render(&without, &Recipe::default()), render(&with, &Recipe::default()));
    assert_eq!(first.pixels, second.pixels);
}

#[test]
fn a_lens_nobody_knows_renders_uncorrected_and_says_why() {
    let mut raw = plain(320, 240, smooth_scene);
    raw.metadata = Metadata {
        make: "Canon".into(),
        model: "EOS 5D Mark IV".into(),
        lens: "24-70mm".into(),
        focal_length: 40.0,
        aperture: 2.8,
        ..Metadata::default()
    };
    let enabled = recipe(|r| r.lens.profile_enabled = true);
    assert_eq!(render(&raw, &enabled).pixels, render(&raw, &Recipe::default()).pixels);

    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &enabled).unwrap();
    let target = session.view_target(160, 120);
    session.render(&enabled, &ViewRequest::default(), &target, None).unwrap();
    assert!(session.lens_profile().is_none());
    assert!(session.lens_error().is_some_and(|error| error.to_string().contains("24-70mm")));
}

/// Dark stripes on a light ground. Each edge keeps one polarity for the whole width of the
/// frame, which is what the line measurement follows.
fn grid(u: f32, v: f32, width: f32, height: f32) -> [f32; 3] {
    const CELLS: f32 = 6.0;
    const HALF_STRIPE: f32 = 5.0;
    let distance = |t: f32, pixels: f32| ((t * CELLS).round() - t * CELLS).abs() * pixels / CELLS;
    let stripe = |t: f32, pixels: f32| (distance(t, pixels) - HALF_STRIPE + 0.75).clamp(0.0, 1.5) / 1.5;
    [0.08 + 0.5 * stripe(u, width).min(stripe(v, height)); 3]
}

fn line_residuals(image: &ExportedImage) -> Vec<(f32, f32, f32)> {
    let values = image.pixels.as_chunks::<3>().0.iter().map(|rgb| rgb[1] * 255.0).collect();
    let luma = LumaImage { width: image.width as usize, height: image.height as usize, values };
    let long_edge = image.width.max(image.height) as f32;
    let segments = join_collinear(&detect_segments(&luma, long_edge * 0.1), 8.0);
    segments
        .iter()
        .filter(|segment| segment.length() > long_edge * 0.4)
        .filter_map(|segment| straightness(&luma, segment))
        .map(|line| (line.length, line.rms, line.bow))
        .collect()
}

fn assert_straightens(template: &RawImage, profile: LensProfile, on: &Recipe, off: &Recipe, least_bend: f32) {
    let (width, height) = (template.width as f32, template.height as f32);
    let geometry = lens_geometry(profile, &on.lens, width / height);
    let raw = through_lens(template, &geometry, |u, v| grid(u, v, width, height));

    let corrected = line_residuals(&render(&raw, on));
    assert!(corrected.len() >= 6, "{corrected:?}");
    let worst_rms = corrected.iter().map(|line| line.1).fold(0.0, f32::max);
    let worst_bow = corrected.iter().map(|line| line.2).fold(0.0, f32::max);
    assert!(worst_rms < 0.25 && worst_bow < 1.0, "{corrected:?}");

    let bent = line_residuals(&render(&raw, off));
    let bent_bow = bent.iter().map(|line| line.2).fold(0.0, f32::max);
    assert!(bent_bow > least_bend, "{bent:?}");
    println!("corrected rms {worst_rms:.3} bow {worst_bow:.3}, uncorrected bow {bent_bow:.2}");
}

#[test]
fn pixel_warp_leaves_lines_straight_to_within_a_pixel() {
    let mut template = blank(1850, 1387);
    template.opcodes = warp_opcodes(PIXEL_RADIAL, [0.0; 2], PIXEL_CENTER);
    let profile = PhotoLens::new(&template).file_profile().unwrap();
    let off = recipe(|r| {
        r.lens.profile_enabled = true;
        r.lens.profile_distortion = 0.0;
    });
    assert_straightens(&template, profile, &Recipe::default(), &off, 1.5);
}

#[test]
fn gh5_tag_leaves_lines_straight_to_within_a_pixel() {
    let template = panasonic_template(2604, 1952);
    let profile = PhotoLens::new(&template).resolve().unwrap();
    let on = recipe(|r| {
        r.lens.profile_enabled = true;
        r.transform.allow_empty_corners = true;
    });
    let off = recipe(|r| {
        r.lens.profile_enabled = true;
        r.lens.profile_distortion = 0.0;
    });
    assert_straightens(&template, profile, &on, &off, 0.8);
}

fn nikon_pincushion(width: u32, height: u32) -> RawImage {
    let mut raw = plain(width, height, |_, _| [0.4; 3]);
    raw.embedded_lens = Some(EmbeddedLens::Nikon(NikonLens {
        distortion: [0.0, 0.03, 0.03],
        vignette: [0.0; 3],
    }));
    raw
}

fn corner_is_empty(image: &ExportedImage) -> bool {
    luma(pixel(image, 1, 1)) < 0.01
}

#[test]
fn correcting_pincushion_scales_up_to_hide_the_corners() {
    let raw = nikon_pincushion(600, 400);
    let fitted = recipe(|r| r.lens.profile_enabled = true);
    assert!(!corner_is_empty(&render(&raw, &fitted)));
}

#[test]
fn empty_corners_stay_when_the_user_allows_them() {
    let raw = nikon_pincushion(600, 400);
    let allowed = recipe(|r| {
        r.lens.profile_enabled = true;
        r.transform.allow_empty_corners = true;
    });
    assert!(corner_is_empty(&render(&raw, &allowed)));
}

#[test]
fn constrain_crop_removes_empty_corners() {
    let raw = nikon_pincushion(600, 400);
    let mut edit = recipe(|r| {
        r.lens.profile_enabled = true;
        r.transform.allow_empty_corners = true;
    });
    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &edit).unwrap();
    edit.crop = session.largest_crop(&edit, 1.5).unwrap();
    assert!(edit.crop.right - edit.crop.left < 1.0);
    let cropped = render(&raw, &edit);
    let corners = [(1, 1), (cropped.width - 2, 1), (1, cropped.height - 2), (cropped.width - 2, cropped.height - 2)];
    for (x, y) in corners {
        assert!(luma(pixel(&cropped, x, y)) > 0.1, "{x},{y}");
    }
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

fn three_plane_warp(red: f64, blue: f64) -> Opcodes {
    let plane = |scale: f64| WarpPlane { radial: [scale, 0.0, 0.0, 0.0], tangential: [0.0; 2] };
    Opcodes {
        list3: vec![Opcode::WarpRectilinear(WarpRectilinear {
            planes: vec![plane(red), plane(1.0), plane(blue)],
            center_x: 0.5,
            center_y: 0.5,
        })],
        ..Opcodes::default()
    }
}

fn every_lens_tool() -> Recipe {
    recipe(|r| {
        r.lens.profile_enabled = true;
        r.lens.remove_chromatic_aberration = true;
        r.lens.manual_vignette = 30.0;
        r.lens.manual_distortion = 10.0;
        r.lens.defringe.purple_amount = 80.0;
        r.lens.defringe.green_amount = 80.0;
    })
}

#[test]
fn lens_tools_do_not_depend_on_tile_size() {
    let mut raw = plain(610, 430, |u, v| grid(u, v, 610.0, 430.0));
    raw.opcodes = three_plane_warp(1.002, 0.998);
    let edit = every_lens_tool();
    let small = export(&raw, &edit, &fixed_tiles(97)).unwrap();
    let large = export(&raw, &edit, &fixed_tiles(1024)).unwrap();
    let worst = worst_difference(&small, &large, 0);
    assert!(worst < 1e-4, "{worst}");
}

#[test]
fn preview_agrees_with_export_through_the_lens_tools() {
    let mut raw = plain(1024, 768, smooth_scene);
    raw.opcodes = warp_opcodes(PIXEL_RADIAL, [0.002, 0.0], PIXEL_CENTER);
    let edit = recipe(|r| {
        r.lens.manual_vignette = 25.0;
        r.lens.manual_distortion = 8.0;
    });
    let full = render(&raw, &edit);

    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &edit).unwrap();
    let target = session.view_target(512, 384);
    session.render(&edit, &ViewRequest::default(), &target, None).unwrap();
    let view = session.read_view(&target).unwrap();
    for (x, y) in [(40u32, 40u32), (256, 192), (470, 340), (60, 330), (400, 60)] {
        let start = ((y * 512 + x) * 4) as usize;
        let shown = [view[start], view[start + 1], view[start + 2]].map(|value| f32::from(value) / 255.0);
        let exported = pixel(&full, x * 2, y * 2);
        for channel in 0..3 {
            assert!((shown[channel] - exported[channel]).abs() < 0.02, "at {x},{y}: {shown:?} vs {exported:?}");
        }
    }
}

/// Red and blue sit on rings of slightly different size around the centre, as lateral chromatic
/// aberration leaves them. Green's rings are where the scene really is.
fn aberrated_rings(width: u32, height: u32, red: f32, blue: f32) -> RawImage {
    let (w, h) = (width as f32, height as f32);
    let ring = |x: f32, y: f32, scale: f32| {
        let radius = ((x - w * 0.5).hypot(y - h * 0.5) / scale) / h;
        0.45 + 0.35 * (radius * 42.0).sin()
    };
    mosaic(bayer(), width, height, |x, y| [ring(x, y, red), ring(x, y, 1.0), ring(x, y, blue)])
}

fn colour_error(image: &ExportedImage) -> f32 {
    let (mut total, mut count) = (0.0, 0);
    for y in (10..image.height - 10).step_by(3) {
        for x in (10..image.width - 10).step_by(3) {
            total += chroma(pixel(image, x, y));
            count += 1;
        }
    }
    total / count as f32
}

#[test]
fn lateral_chromatic_aberration_loses_its_colour_fringes() {
    let (red, blue) = (1.012, 0.988);
    let mut raw = aberrated_rings(900, 600, red, blue);
    raw.opcodes = three_plane_warp(f64::from(red), f64::from(blue));
    let corrected = colour_error(&render(&raw, &recipe(|r| r.lens.remove_chromatic_aberration = true)));
    let left = colour_error(&render(&raw, &Recipe::default()));
    assert!(corrected < left * 0.7, "{corrected} vs {left}");
}

#[test]
fn chromatic_aberration_stays_put_unless_asked_for() {
    let mut raw = aberrated_rings(480, 320, 1.01, 0.99);
    let plain_render = render(&raw, &Recipe::default());
    raw.opcodes = three_plane_warp(1.01, 0.99);
    let with_data = render(&raw, &Recipe::default());
    assert!(worst_difference(&plain_render, &with_data, 4) < 1e-5);
}

fn shaded(width: u32, height: u32, strength: f32) -> RawImage {
    let half = (width as f32).hypot(height as f32) * 0.5;
    let mut raw = mosaic(bayer(), width, height, |x, y| {
        let radius = (x - width as f32 * 0.5).hypot(y - height as f32 * 0.5) / half;
        [0.3 / (1.0 + strength * radius * radius); 3]
    });
    raw.opcodes.list3 = vec![Opcode::FixVignetteRadial(FixVignetteRadial {
        k: [f64::from(strength), 0.0, 0.0, 0.0, 0.0],
        center_x: 0.5,
        center_y: 0.5,
    })];
    raw
}

fn corner_falloff(image: &ExportedImage) -> f32 {
    luma(center(image)) - luma(pixel(image, 8, 8))
}

#[test]
fn fix_vignette_radial_flattens_a_shaded_frame() {
    let raw = shaded(480, 320, 1.0);
    let corrected = corner_falloff(&render(&raw, &Recipe::default()));
    assert!(corrected.abs() < 0.015, "{corrected}");
}

#[test]
fn profile_vignette_slider_scales_the_correction() {
    let raw = shaded(480, 320, 1.0);
    let at = |slider| {
        corner_falloff(&render(&raw, &recipe(|r| {
            r.lens.profile_enabled = true;
            r.lens.profile_vignette = slider;
        })))
    };
    assert!(at(0.0) > 0.1, "{}", at(0.0));
    assert!(at(100.0).abs() < 0.015);
    assert!(at(200.0) < -0.08, "{}", at(200.0));
}

#[test]
fn manual_vignette_works_on_the_corners_and_spares_the_centre() {
    let raw = plain(480, 320, |_, _| [0.2; 3]);
    let base = render(&raw, &Recipe::default());
    let lifted = render(&raw, &recipe(|r| r.lens.manual_vignette = 60.0));
    let darkened = render(&raw, &recipe(|r| r.lens.manual_vignette = -60.0));
    let corner = |image: &ExportedImage| luma(pixel(image, 4, 4));
    assert!(corner(&lifted) > corner(&base) + 0.05);
    assert!(corner(&darkened) < corner(&base) - 0.05);
    assert!((luma(center(&lifted)) - luma(center(&base))).abs() < 0.002);
}

#[test]
fn manual_vignette_midpoint_confines_it_to_the_corners() {
    let raw = plain(480, 320, |_, _| [0.2; 3]);
    let at = |midpoint| {
        luma(pixel(&render(&raw, &recipe(|r| {
            r.lens.manual_vignette = 80.0;
            r.lens.manual_vignette_midpoint = midpoint;
        })), 120, 80))
    };
    assert!(at(20.0) > at(80.0) + 0.02, "{} {}", at(20.0), at(80.0));
}

#[test]
fn vignette_gain_reaches_shadow_and_highlight_tools_consistently() {
    let raw = plain(480, 320, |_, _| [0.05; 3]);
    let edited = render(&raw, &recipe(|r| {
        r.lens.manual_vignette = 100.0;
        r.basic.shadows = 80.0;
    }));
    let rings: Vec<f32> = [20u32, 60, 100, 140, 180, 220]
        .iter()
        .map(|&x| luma(pixel(&edited, x, x * 320 / 480)))
        .collect();
    assert!(rings.windows(2).all(|pair| pair[0] > pair[1] - 0.002), "{rings:?}");
}

#[test]
fn a_post_demosaic_gain_map_shades_one_plane() {
    let mut raw = plain(320, 160, |_, _| [0.3; 3]);
    let before = render(&raw, &Recipe::default());
    raw.opcodes.list3 = vec![Opcode::GainMap(GainMap {
        top: 0,
        left: 0,
        bottom: 160,
        right: 320,
        plane: 0,
        planes: 1,
        row_pitch: 1,
        col_pitch: 1,
        points_v: 1,
        points_h: 2,
        spacing_v: 1.0,
        spacing_h: 1.0,
        origin_v: 0.0,
        origin_h: 0.0,
        map_planes: 1,
        gains: vec![1.0, 2.0],
    })];
    let after = render(&raw, &Recipe::default());
    let (left, right) = (pixel(&after, 4, 80), pixel(&after, 316, 80));
    assert!((left[0] - pixel(&before, 4, 80)[0]).abs() < 0.015, "{left:?}");
    assert!(right[0] > left[0] + 0.1, "{left:?} {right:?}");
    assert!((right[1] - left[1]).abs() < 0.03 || right[1] < left[1], "{left:?} {right:?}");
}

const PURPLE: [f32; 3] = [0.42, 0.12, 0.72];

/// Dark on the left, a thin purple halo, then bright sky; a big purple patch sits far away.
fn fringed_scene(x: f32, y: f32) -> [f32; 3] {
    match (x, y) {
        (_, y) if y >= 120.0 && (120.0..200.0).contains(&x) => PURPLE,
        (x, _) if x < 150.0 => [0.02; 3],
        (x, _) if x < 153.0 => PURPLE,
        _ => [0.95; 3],
    }
}

#[test]
fn defringe_greys_a_halo_beside_an_edge() {
    let raw = mosaic(bayer(), 320, 200, fringed_scene);
    let base = render(&raw, &Recipe::default());
    let cleaned = render(&raw, &recipe(|r| r.lens.defringe.purple_amount = 100.0));
    assert!(chroma(pixel(&cleaned, 151, 40)) < chroma(pixel(&base, 151, 40)) * 0.6);
}

#[test]
fn defringe_leaves_a_purple_object_alone() {
    let raw = mosaic(bayer(), 320, 200, fringed_scene);
    let base = render(&raw, &Recipe::default());
    let cleaned = render(&raw, &recipe(|r| r.lens.defringe.purple_amount = 100.0));
    let (before, after) = (pixel(&base, 160, 170), pixel(&cleaned, 160, 170));
    assert!((chroma(before) - chroma(after)).abs() < 0.01, "{before:?} {after:?}");
}

#[test]
fn defringe_ignores_colours_outside_its_hue_range() {
    let raw = mosaic(bayer(), 320, 200, fringed_scene);
    let base = render(&raw, &Recipe::default());
    let cleaned = render(&raw, &recipe(|r| r.lens.defringe.green_amount = 100.0));
    assert!(worst_difference(&base, &cleaned, 0) < 0.01);
}

/// Vertical stripes on a wall, photographed tilted upward: they lean inward towards the top.
fn keystoned(width: u32, height: u32, lean: f32) -> RawImage {
    let (w, h) = (width as f32, height as f32);
    mosaic(bayer(), width, height, move |x, y| {
        let (px, py) = ((x - w * 0.5) / h, (y - h * 0.5) / h);
        let depth = 1.0 + lean * py;
        let (wall_x, wall_y) = (px / depth, py / depth);
        let stripe = (wall_x * 7.0).rem_euclid(1.0) < 0.5;
        let band = (wall_y * 5.0).rem_euclid(1.0) < 0.5;
        [if stripe != band { 0.6 } else { 0.08 }; 3]
    })
}

fn steepest_lean(image: &ExportedImage) -> f32 {
    let values = image.pixels.as_chunks::<3>().0.iter().map(|rgb| rgb[1] * 255.0).collect();
    let luma = LumaImage { width: image.width as usize, height: image.height as usize, values };
    let segments = detect_segments(&luma, image.height as f32 * 0.12);
    let lean = |segment: &Segment| {
        ((segment.end[0] - segment.start[0]) / (segment.end[1] - segment.start[1])).abs()
    };
    segments
        .iter()
        .filter(|segment| {
            (segment.end[1] - segment.start[1]).abs() > (segment.end[0] - segment.start[0]).abs()
        })
        .map(lean)
        .fold(0.0, f32::max)
}

#[test]
fn upright_vertical_stands_converging_lines_up() {
    let raw = keystoned(900, 600, 0.35);
    let before = steepest_lean(&render(&raw, &Recipe::default()));
    let after = steepest_lean(&render(&raw, &recipe(|r| r.transform.upright = Upright::Vertical)));
    assert!(before > 0.05, "{before}");
    assert!(after < before * 0.25, "{after} from {before}");
}

#[test]
fn upright_leaves_no_empty_corners() {
    let raw = keystoned(900, 600, 0.35);
    let image = render(&raw, &recipe(|r| r.transform.upright = Upright::Vertical));
    let edges = (0..image.width).step_by(16).flat_map(|x| [(x, 0), (x, image.height - 1)]);
    let brightest_is_lit = edges.map(|(x, y)| luma(pixel(&image, x, y))).fold(0.0, f32::max);
    assert!(brightest_is_lit > 0.2);
    for (x, y) in [(0, 0), (image.width - 1, 0), (0, image.height - 1), (image.width - 1, image.height - 1)] {
        let around = (0..6).map(|step| luma(pixel(&image, x.saturating_sub(step).max(x.min(step)), y)));
        assert!(around.fold(0.0, f32::max) > 0.01, "{x},{y}");
    }
}

#[test]
fn guides_straighten_the_lines_they_are_drawn_on() {
    let raw = keystoned(900, 600, 0.35);
    let (w, h) = (900.0f32, 600.0f32);
    let on_wall = |wall_x: f32, y: f32| {
        let py = y / h - 0.5;
        [(wall_x * (1.0 + 0.35 * py) * h + w * 0.5) / w, y / h]
    };
    let guide = |wall_x: f32| GuideLine { start: on_wall(wall_x, 60.0), end: on_wall(wall_x, 540.0) };
    let edit = recipe(|r| {
        r.transform.upright = Upright::Guided;
        r.transform.guides = vec![guide(-0.5), guide(0.5)];
    });
    let after = steepest_lean(&render(&raw, &edit));
    assert!(after < 0.02, "{after}");
}

#[test]
fn upright_finds_the_same_answer_for_preview_and_export() {
    let raw = keystoned(900, 600, 0.35);
    let edit = recipe(|r| r.transform.upright = Upright::Full);
    let full = render(&raw, &edit);
    let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &edit).unwrap();
    let target = session.view_target(900, 600);
    session.render(&edit, &ViewRequest::default(), &target, None).unwrap();
    let view = session.read_view(&target).unwrap();
    let mut mismatched = 0;
    for y in (20..580).step_by(11) {
        for x in (20..880).step_by(13) {
            let start = ((y * 900 + x) * 4) as usize;
            if (f32::from(view[start + 1]) / 255.0 - pixel(&full, x, y)[1]).abs() > 0.2 {
                mismatched += 1;
            }
        }
    }
    assert!(mismatched < 120, "{mismatched} of {} samples differ", 51 * 66);
}

/// Wooden planks wander on their own, so the typical line says more here than the worst one.
#[test]
fn real_gh5_lines_end_up_within_a_pixel_of_straight() {
    let Ok(samples) = std::env::var("UNLIGHT_SAMPLES") else {
        return;
    };
    let raw = unlight_raw::load(&std::path::Path::new(&samples).join("gh5.RW2")).unwrap();
    let image = render(&raw, &recipe(|r| r.lens.profile_enabled = true));
    let values = image.pixels.as_chunks::<3>().0.iter().map(|rgb| rgb[1] * 255.0).collect();
    let luma = LumaImage { width: image.width as usize, height: image.height as usize, values };
    let lines = measure_long_lines(&luma, image.width as f32 * 0.15);
    assert!(lines.len() >= 6, "{lines:?}");
    let mut rms: Vec<f32> = lines.iter().map(|line| line.rms).collect();
    rms.sort_by(f32::total_cmp);
    assert!(rms[rms.len() / 2] < 1.0, "{lines:?}");
}

/// Micro Four Thirds lenses are designed around this correction, so it applies without asking.
#[test]
fn real_gh5_distortion_is_corrected_by_default() {
    let Ok(samples) = std::env::var("UNLIGHT_SAMPLES") else {
        return;
    };
    let raw = unlight_raw::load(&std::path::Path::new(&samples).join("gh5.RW2")).unwrap();
    let plain = render(&raw, &Recipe::default());
    let switched_off = recipe(|r| {
        r.lens.profile_enabled = true;
        r.lens.profile_distortion = 0.0;
    });
    let uncorrected = render(&raw, &switched_off);
    let moved = plain.pixels.iter().zip(&uncorrected.pixels).filter(|(a, b)| (*a - *b).abs() > 0.02).count();
    assert!(moved > plain.pixels.len() / 100, "{moved} of {}", plain.pixels.len());
}
