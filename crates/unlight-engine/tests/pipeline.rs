//! GPU tests on synthetic mosaics. They run on any Vulkan device, including llvmpipe, so
//! `nix flake check` needs neither hardware nor sample files.

use unlight_engine::{
    Engine, EngineError, ExportOptions, FaultInjection, GpuContext, Precision, SchedulerConfig,
    Session, ViewRequest, export,
};
use unlight_raw::{Cfa, ColorData, Metadata, Opcode, GainMap, Opcodes, RawImage};
use unlight_recipe::Recipe;
use std::time::Duration;

const SRGB_XYZ_TO_CAMERA: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.969266, 1.8760108, 0.041556],
    [0.0556434, -0.2040259, 1.0572252],
];

fn bayer() -> Cfa {
    let mut pattern = [[0u8; 6]; 6];
    for (row, sites) in pattern.iter_mut().enumerate() {
        for (col, site) in sites.iter_mut().enumerate() {
            *site = match (row % 2, col % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
        }
    }
    Cfa { period: 2, pattern }
}

fn xtrans() -> Cfa {
    let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
    let mut pattern = [[0u8; 6]; 6];
    for (row, text) in rows.iter().enumerate() {
        for (col, letter) in text.bytes().enumerate() {
            pattern[row][col] = match letter {
                b'R' => 0,
                b'G' => 1,
                _ => 2,
            };
        }
    }
    Cfa { period: 6, pattern }
}

/// Builds a neutral-balanced raw by sampling `scene` (linear camera RGB in 0..1) through a CFA.
fn mosaic(
    cfa: Cfa,
    width: u32,
    height: u32,
    black: f32,
    white: f32,
    scene: impl Fn(f32, f32) -> [f32; 3],
) -> RawImage {
    let samples = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let rgb = scene(x as f32, y as f32);
            let value = rgb[cfa.color_at(x, y) as usize];
            (black + value.clamp(0.0, 1.0) * (white - black)).round() as u16
        })
        .collect();
    RawImage {
        width,
        height,
        samples,
        cfa,
        black: [[black; 6]; 6],
        white,
        color: ColorData {
            xyz_to_camera: SRGB_XYZ_TO_CAMERA,
            as_shot_multipliers: [1.0; 3],
            daylight_multipliers: [1.0; 3],
            baseline_exposure: 0.0,
            dng: None,
        },
        opcodes: Opcodes::default(),
        metadata: Metadata::default(),
        embedded_lens: None,
    }
}

fn ctx() -> std::sync::Arc<GpuContext> {
    GpuContext::new().expect("a Vulkan adapter (set VK_DRIVER_FILES to lavapipe if there is no GPU)")
}

fn fixed_tiles(edge: u32) -> SchedulerConfig {
    SchedulerConfig {
        budget: Duration::from_secs(3600),
        min_edge: edge,
        max_edge: edge,
        initial_edge: edge,
    }
}

fn options(schedule: SchedulerConfig) -> ExportOptions {
    ExportOptions { schedule, ..ExportOptions::default() }
}

fn pixel(image: &unlight_engine::ExportedImage, x: u32, y: u32) -> [f32; 3] {
    let index = (y * image.width + x) as usize * 3;
    [image.pixels[index], image.pixels[index + 1], image.pixels[index + 2]]
}

/// Sharp diagonal and vertical edges with smooth colour between, the hard case for a demosaic.
fn detailed_scene(x: f32, y: f32) -> [f32; 3] {
    let stripes = if ((x + y * 0.5) / 7.0).floor() as i32 % 2 == 0 { 0.8 } else { 0.25 };
    [stripes, stripes * (0.6 + 0.4 * (y / 90.0).sin().abs()), stripes * (0.5 + x / 500.0)]
}

fn assert_identical(a: &unlight_engine::ExportedImage, b: &unlight_engine::ExportedImage) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let worst = a
        .pixels
        .iter()
        .zip(&b.pixels)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "tilings differ by {worst}");
}

#[test]
fn bayer_output_does_not_depend_on_tile_size() {
    let raw = mosaic(bayer(), 610, 430, 64.0, 16383.0, detailed_scene);
    let small = export(&raw, &Recipe::default(), &options(fixed_tiles(97))).unwrap();
    let large = export(&raw, &Recipe::default(), &options(fixed_tiles(1024))).unwrap();
    assert!(small.tiles > large.tiles);
    assert_identical(&small, &large);
}

#[test]
fn xtrans_output_does_not_depend_on_tile_size() {
    let raw = mosaic(xtrans(), 610, 430, 64.0, 16383.0, detailed_scene);
    let small = export(&raw, &Recipe::default(), &options(fixed_tiles(97))).unwrap();
    let large = export(&raw, &Recipe::default(), &options(fixed_tiles(1024))).unwrap();
    assert_identical(&small, &large);
}

fn assert_flat(raw: &RawImage, expected: [f32; 3]) {
    let image = export(raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    for (y, x) in [(0, 0), (37, 91), (120, 200), (image.height - 1, image.width - 1)] {
        let value = pixel(&image, x, y);
        for channel in 0..3 {
            let difference = (value[channel] - expected[channel]).abs();
            assert!(difference < 0.02, "at {x},{y}: {value:?} vs {expected:?}");
        }
    }
}

#[test]
fn flat_colour_stays_flat_through_bayer() {
    let raw = mosaic(bayer(), 256, 180, 64.0, 16383.0, |_, _| [0.2, 0.2, 0.2]);
    let reference = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    assert_flat(&raw, pixel(&reference, 128, 90));
}

#[test]
fn flat_colour_stays_flat_through_xtrans() {
    let raw = mosaic(xtrans(), 256, 180, 64.0, 16383.0, |_, _| [0.3, 0.2, 0.1]);
    let reference = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    assert_flat(&raw, pixel(&reference, 128, 90));
}

#[test]
fn neutral_scene_renders_neutral() {
    for cfa in [bayer(), xtrans()] {
        let raw = mosaic(cfa, 200, 150, 64.0, 16383.0, |_, _| [0.18, 0.18, 0.18]);
        let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
        let [r, g, b] = pixel(&image, 100, 75);
        assert!((r - g).abs() < 0.005 && (g - b).abs() < 0.005, "{r} {g} {b}");
    }
}

/// Mean absolute error against what the same scene renders to without a mosaic in the way.
fn demosaic_error(cfa: Cfa) -> f32 {
    let raw = mosaic(cfa, 400, 300, 64.0, 16383.0, detailed_scene);
    let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    // A solid render of each stripe colour would need the full colour path, so the check
    // uses edge-free pixels: every pixel must be close to one of the two stripe levels.
    let mut error = 0.0;
    let mut count = 0;
    for y in (20..280).step_by(3) {
        for x in (20..380).step_by(3) {
            let stripe_here = ((x as f32 + y as f32 * 0.5) / 7.0).floor() as i32;
            let same_stripe = |dx: f32| ((x as f32 + dx + y as f32 * 0.5) / 7.0).floor() as i32 == stripe_here;
            if !(same_stripe(-3.0) && same_stripe(3.0)) {
                continue;
            }
            let value = pixel(&image, x, y);
            let [r, _, _] = detailed_scene(x as f32, y as f32);
            let level = if r > 0.5 { 0.8f32 } else { 0.25 };
            let reference = export_level(level);
            error += (value[0] - reference).abs();
            count += 1;
        }
    }
    error / count as f32
}

/// The tone-mapped red level for a stripe, taken from a flat render at that level.
fn export_level(level: f32) -> f32 {
    use std::{collections::HashMap, sync::{LazyLock, Mutex}};
    static CACHE: LazyLock<Mutex<HashMap<u32, f32>>> = LazyLock::new(Default::default);
    *CACHE.lock().unwrap().entry(level.to_bits()).or_insert_with(|| {
        let raw = mosaic(bayer(), 64, 64, 64.0, 16383.0, |_, _| [level, level, level]);
        let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
        pixel(&image, 32, 32)[0]
    })
}

#[test]
fn bayer_demosaic_recovers_stripe_levels() {
    let error = demosaic_error(bayer());
    assert!(error < 0.06, "mean error {error}");
}

#[test]
fn xtrans_demosaic_recovers_stripe_levels() {
    let error = demosaic_error(xtrans());
    assert!(error < 0.08, "mean error {error}");
}

#[test]
fn black_and_white_levels_normalise_the_range() {
    let low = mosaic(bayer(), 128, 96, 1000.0, 5000.0, |_, _| [0.25; 3]);
    let high = mosaic(bayer(), 128, 96, 0.0, 16000.0, |_, _| [0.25; 3]);
    let low = export(&low, &Recipe::default(), &ExportOptions::default()).unwrap();
    let high = export(&high, &Recipe::default(), &ExportOptions::default()).unwrap();
    let (a, b) = (pixel(&low, 64, 48), pixel(&high, 64, 48));
    for channel in 0..3 {
        assert!((a[channel] - b[channel]).abs() < 0.01, "{a:?} vs {b:?}");
    }
}

#[test]
fn exposure_brightens_the_render() {
    let raw = mosaic(bayer(), 128, 96, 64.0, 16383.0, |_, _| [0.1; 3]);
    let base = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    let mut recipe = Recipe::default();
    recipe.basic.exposure = 1.0;
    let bright = export(&raw, &recipe, &ExportOptions::default()).unwrap();
    assert!(pixel(&bright, 64, 48)[1] > pixel(&base, 64, 48)[1] + 0.05);
}

#[test]
fn white_balance_equalises_channels() {
    let mut raw = mosaic(bayer(), 128, 96, 64.0, 16383.0, |_, _| [0.1, 0.2, 0.15]);
    raw.color.as_shot_multipliers = [2.0, 1.0, 4.0 / 3.0];
    let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    let [r, g, b] = pixel(&image, 64, 48);
    assert!((r - g).abs() < 0.02 && (g - b).abs() < 0.02, "{r} {g} {b}");
}

fn gain_map(top: u32, left: u32, width: u32, height: u32) -> Opcode {
    Opcode::GainMap(GainMap {
        top,
        left,
        bottom: height,
        right: width,
        plane: 0,
        planes: 1,
        row_pitch: 2,
        col_pitch: 2,
        points_v: 2,
        points_h: 2,
        spacing_v: 1.0,
        spacing_h: 1.0,
        origin_v: 0.0,
        origin_h: 0.0,
        map_planes: 1,
        gains: vec![1.0, 2.0, 1.0, 2.0],
    })
}

#[test]
fn gain_map_undoes_lens_shading() {
    let (width, height) = (256.0, 128.0);
    let mut raw = mosaic(bayer(), 256, 128, 0.0, 16383.0, |x, _| {
        let shading = 1.0 / (1.0 + (x + 0.5) / width);
        [0.3 * shading; 3]
    });
    raw.opcodes.list2 = (0..2)
        .flat_map(|top| (0..2).map(move |left| gain_map(top, left, 256, height as u32)))
        .collect();
    let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    let left = pixel(&image, 12, 64)[1];
    let right = pixel(&image, 244, 64)[1];
    assert!((left - right).abs() < 0.01, "{left} vs {right}");
}

#[test]
fn clipped_highlights_stay_neutral() {
    let mut raw = mosaic(bayer(), 128, 96, 64.0, 16383.0, |_, _| [1.0; 3]);
    raw.color.as_shot_multipliers = [2.2, 1.0, 1.6];
    let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    let [r, g, b] = pixel(&image, 64, 48);
    assert!((r - g).abs() < 0.01 && (g - b).abs() < 0.01, "{r} {g} {b}");
}

#[test]
fn export_survives_a_lost_device_and_matches_a_clean_run() {
    let raw = mosaic(bayer(), 512, 384, 64.0, 16383.0, detailed_scene);
    let clean = export(&raw, &Recipe::default(), &options(fixed_tiles(128))).unwrap();
    let recovered = export(
        &raw,
        &Recipe::default(),
        &ExportOptions {
            schedule: fixed_tiles(128),
            fault: FaultInjection::LoseDeviceOnceAfter(3),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(recovered.device_losses, 1);
    assert_identical(&clean, &recovered);
}

#[test]
fn export_gives_up_after_repeated_device_loss() {
    let raw = mosaic(bayer(), 256, 192, 64.0, 16383.0, detailed_scene);
    let result = export(
        &raw,
        &Recipe::default(),
        &ExportOptions {
            schedule: fixed_tiles(64),
            fault: FaultInjection::LoseDeviceAlwaysAfter(1),
            max_recoveries: 2,
            ..ExportOptions::default()
        },
    );
    assert!(matches!(result, Err(EngineError::RecoveryExhausted(3))), "{result:?}");
}

#[test]
fn preview_path_agrees_with_export_on_a_smooth_image() {
    let scene = |x: f32, y: f32| [0.1 + x / 900.0, 0.15 + y / 800.0, 0.2];
    let raw = mosaic(bayer(), 512, 384, 64.0, 16383.0, scene);
    let full = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();

    let engine = Engine::new(ctx(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &Recipe::default()).unwrap();
    let target = session.view_target(256, 192);
    session.render(&Recipe::default(), &ViewRequest::default(), &target, None).unwrap();
    let view = session.read_view(&target).unwrap();

    for (x, y) in [(64u32, 48u32), (128, 96), (200, 150)] {
        let start = ((y * 256 + x) * 4) as usize;
        let shown = [view[start], view[start + 1], view[start + 2]].map(|v| f32::from(v) / 255.0);
        let exported = pixel(&full, x * 2, y * 2);
        for channel in 0..3 {
            assert!((shown[channel] - exported[channel]).abs() < 0.015, "{shown:?} vs {exported:?}");
        }
    }
}

#[test]
fn two_devices_can_work_side_by_side() {
    let raw = mosaic(bayer(), 256, 192, 64.0, 16383.0, detailed_scene);
    let editor = Engine::new(ctx(), Precision::F16).unwrap();
    let source = editor.load(&raw, SchedulerConfig::BACKGROUND).unwrap();
    let preview = editor.build_preview(&source, SchedulerConfig::BACKGROUND).unwrap();
    let image = export(&raw, &Recipe::default(), &ExportOptions::default()).unwrap();
    assert_eq!(image.width, 256);
    assert_eq!(preview.image_width, 256);
    editor.ctx.check().unwrap();
}

#[test]
fn oversized_images_are_refused_cleanly() {
    let mut raw = mosaic(bayer(), 16, 16, 64.0, 16383.0, |_, _| [0.5; 3]);
    let engine = Engine::new(ctx(), Precision::F16).unwrap();
    raw.width = engine.ctx.device.limits().max_texture_dimension_2d;
    let result = engine.load(&raw, SchedulerConfig::BACKGROUND);
    assert!(matches!(result, Err(EngineError::ImageTooLarge { .. })));
}
