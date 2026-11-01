//! A merged photo has to behave like a raw in every Develop tool. These run synthetic merged
//! sources through the real pipeline, with no sample files and no merge step: what's under test
//! is the engine's side of the contract.

mod support;

use support::{bayer, center, luma, mosaic, pixel, render};
use unlight_engine::{Engine, GpuContext, LensError, Precision, Session, ViewRequest, preview_difference, read_reduced, read_scene};
use unlight_raw::{
    Cfa, FixVignetteRadial, MergeInfo, MergeKind, Metadata, Opcode, RawImage,
    merged::{MergedPhoto, half_from_f32, write},
};
use unlight_recipe::{
    HealMode, HealSpot, LocalAdjustments, Mask, MaskComponent, MaskShape, Recipe, WhiteBalance,
};

const OPAQUE: u16 = 0x3c00;

fn info(lens_applied: bool) -> MergeInfo {
    MergeInfo { kind: MergeKind::Panorama, sources: vec!["a.ARW".into(), "b.ARW".into()], lens_applied }
}

fn merged(width: u32, height: u32, scene: impl Fn(f32, f32) -> [f32; 3]) -> RawImage {
    let mut samples = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b] = scene(x as f32, y as f32);
            samples.extend([half_from_f32(r), half_from_f32(g), half_from_f32(b), OPAQUE]);
        }
    }
    RawImage {
        width,
        height,
        samples,
        cfa: Cfa::FULL_COLOUR,
        black: [[0.0; 6]; 6],
        white: 1.0,
        color: support::flat([0.5; 3]).color,
        opcodes: Default::default(),
        metadata: Metadata { merge: Some(info(false)), ..Metadata::default() },
        embedded_lens: None,
    }
}

fn scene(x: f32, y: f32) -> [f32; 3] {
    let wave = 0.3 + 0.2 * ((x * 0.02).sin() * (y * 0.03).cos());
    [wave * 1.1, wave, wave * 0.8]
}

fn recipe(change: impl FnOnce(&mut Recipe)) -> Recipe {
    let mut recipe = Recipe::default();
    change(&mut recipe);
    recipe
}

fn mean_luma(image: &unlight_engine::ExportedImage, columns: std::ops::Range<u32>) -> f32 {
    let mut total = 0.0;
    let mut count = 0.0;
    for y in 8..image.height - 8 {
        for x in columns.clone() {
            total += luma(pixel(image, x, y));
            count += 1.0;
        }
    }
    total / count
}

fn is_finite(image: &unlight_engine::ExportedImage) -> bool {
    image.pixels.iter().all(|value| value.is_finite())
}

/// Noise that's the same on every run, as `sigma` around each value.
fn grainy(value: f32, x: f32, y: f32, sigma: f32) -> f32 {
    let hash = ((x as u32).wrapping_mul(73_856_093) ^ (y as u32).wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
    let unit = (hash >> 16) as f32 / 65535.0;
    value + (unit - 0.5) * sigma * 3.46
}

fn ctx() -> std::sync::Arc<GpuContext> {
    GpuContext::new().unwrap()
}

#[test]
fn a_merged_photo_renders_like_a_raw_of_the_same_scene() {
    let raw = mosaic(bayer(), 256, 192, scene);
    let full = merged(256, 192, scene);
    let (from_raw, from_merge) = (render(&raw, &Recipe::default()), render(&full, &Recipe::default()));
    assert_eq!((from_merge.width, from_merge.height), (256, 192));
    let mut worst = 0.0f32;
    for y in (16..176).step_by(8) {
        for x in (16..240).step_by(8) {
            let (a, b) = (pixel(&from_raw, x, y), pixel(&from_merge, x, y));
            (0..3).for_each(|c| worst = worst.max((a[c] - b[c]).abs()));
        }
    }
    assert!(worst < 0.02, "worst channel difference {worst}");
}

#[test]
fn light_above_white_comes_back_with_exposure() {
    let raw = merged(96, 96, |_, _| [4.0; 3]);
    let blown = center(&render(&raw, &Recipe::default()));
    let lowered = center(&render(&raw, &recipe(|r| r.basic.exposure = -4.0)));
    let matching = center(&render(&merged(96, 96, |_, _| [0.25; 3]), &Recipe::default()));
    assert!(blown[1] > 0.95, "{blown:?}");
    assert!((lowered[1] - matching[1]).abs() < 0.03, "{lowered:?} against {matching:?}");
}

#[test]
fn highlights_recover_detail_above_white() {
    // Two levels above white, about a stop apart, which is within the slider's reach. They
    // render as one flat white until Highlights brings them down.
    let bar = |y: f32| if (y / 16.0).floor() % 2.0 == 0.0 { 1.5 } else { 2.8 };
    let raw = merged(256, 128, |x, y| if x < 128.0 { [0.05; 3] } else { [bar(y); 3] });
    let plain = render(&raw, &Recipe::default());
    let recovered = render(&raw, &recipe(|r| r.basic.highlights = -100.0));
    let contrast = |image: &unlight_engine::ExportedImage| {
        (luma(pixel(image, 200, 24)) - luma(pixel(image, 200, 8))).abs()
    };
    assert!(contrast(&plain) < 0.02, "already visible: {}", contrast(&plain));
    assert!(contrast(&recovered) > 0.05, "{} to {}", contrast(&plain), contrast(&recovered));
    assert!(mean_luma(&recovered, 180..240) < mean_luma(&plain, 180..240) - 0.03);
    let shadows = render(&raw, &recipe(|r| r.basic.shadows = 100.0));
    assert!(mean_luma(&shadows, 20..100) > mean_luma(&plain, 20..100) + 0.02);
}

#[test]
fn white_balance_moves_it_the_way_it_moves_a_raw() {
    let raw = merged(96, 96, |_, _| [0.3; 3]);
    let warm = center(&render(&raw, &recipe(|r| {
        r.white_balance = WhiteBalance::Custom { temperature: 9000.0, tint: 0.0 };
    })));
    let cool = center(&render(&raw, &recipe(|r| {
        r.white_balance = WhiteBalance::Custom { temperature: 4000.0, tint: 0.0 };
    })));
    assert!(warm[0] / warm[2] > cool[0] / cool[2] * 1.2, "{warm:?} {cool:?}");
}

#[test]
fn noise_reduction_leaves_a_clean_photo_alone() {
    let raw = merged(320, 240, scene);
    let settings = recipe(|r| {
        r.detail.noise_reduction.luminance = 80.0;
        r.detail.noise_reduction.color = 80.0;
    });
    let (plain, denoised) = (render(&raw, &Recipe::default()), render(&raw, &settings));
    assert!(is_finite(&denoised));
    let worst = plain.pixels.iter().zip(&denoised.pixels).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(worst < 0.03, "{worst}");
}

#[test]
fn noise_reduction_works_on_a_noisy_merged_photo() {
    let raw = merged(512, 384, |x, y| {
        [grainy(0.2, x, y, 0.02), grainy(0.2, x + 311.0, y, 0.02), grainy(0.2, x, y + 197.0, 0.02)]
    });
    let settings = recipe(|r| {
        r.detail.noise_reduction.luminance = 90.0;
        r.detail.noise_reduction.color = 90.0;
    });
    let (plain, denoised) = (render(&raw, &Recipe::default()), render(&raw, &settings));
    assert!(is_finite(&denoised));
    let spread = |image: &unlight_engine::ExportedImage| {
        let values: Vec<f32> = image.pixels.as_chunks::<3>().0.iter().map(|pixel| pixel[1]).collect();
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        (values.iter().map(|value| (value - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt()
    };
    assert!(spread(&denoised) < spread(&plain) * 0.8, "{} against {}", spread(&denoised), spread(&plain));
    assert!(spread(&denoised) > 0.0);
}

#[test]
fn a_lens_correction_cant_be_applied_twice() {
    let mut raw = merged(256, 192, scene);
    raw.metadata.merge = Some(info(true));
    raw.metadata.make = "SONY".into();
    raw.metadata.model = "ILCE-7RM4".into();
    raw.metadata.lens = "FE 24-70mm F2.8 GM".into();
    raw.metadata.focal_length = 24.0;
    let on = recipe(|r| {
        r.lens.profile_enabled = true;
        r.lens.profile_distortion = 100.0;
        r.lens.profile_vignette = 100.0;
        r.lens.remove_chromatic_aberration = true;
    });
    let (plain, corrected) = (render(&raw, &Recipe::default()), render(&raw, &on));
    assert_eq!(plain.pixels, corrected.pixels);

    let engine = Engine::new(ctx(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &on).unwrap();
    let target = session.view_target(64, 48);
    session.render(&on, &ViewRequest::default(), &target, None).unwrap();
    assert!(session.lens_profile().is_none());
    assert_eq!(session.lens_error(), Some(&LensError::AlreadyApplied));
}

#[test]
fn an_hdr_keeps_its_lens_corrections() {
    let mut raw = merged(256, 192, |_, _| [0.2; 3]);
    raw.opcodes.list3.push(Opcode::FixVignetteRadial(FixVignetteRadial {
        k: [1.0, 0.0, 0.0, 0.0, 0.0],
        center_x: 0.5,
        center_y: 0.5,
    }));
    let plain = render(&raw, &Recipe::default());
    let (corner, middle) = (luma(pixel(&plain, 6, 6)), luma(pixel(&plain, 128, 96)));
    assert!(corner > middle + 0.05, "the file's own correction should brighten corners: {corner} {middle}");

    raw.metadata.merge = Some(info(true));
    let baked = render(&raw, &Recipe::default());
    let (corner, middle) = (luma(pixel(&baked, 6, 6)), luma(pixel(&baked, 128, 96)));
    assert!((corner - middle).abs() < 0.01, "already in the pixels, so never again: {corner} {middle}");
}

#[test]
fn masks_and_healing_work_on_a_merged_photo() {
    let raw = merged(320, 240, scene);
    let brighten = Mask {
        components: vec![MaskComponent {
            shape: MaskShape::Linear { start: [0.45, 0.5], end: [0.55, 0.5] },
            subtract: false,
        }],
        adjustments: LocalAdjustments { exposure: 1.0, ..LocalAdjustments::default() },
        ..Mask::default()
    };
    let plain = render(&raw, &Recipe::default());
    let masked = render(&raw, &Recipe { masks: vec![brighten], ..Recipe::default() });
    assert!(mean_luma(&masked, 20..120) > mean_luma(&plain, 20..120) + 0.05);
    assert!((mean_luma(&masked, 220..300) - mean_luma(&plain, 220..300)).abs() < 0.005);

    let spot = HealSpot {
        mode: HealMode::Clone,
        target: [0.3, 0.5],
        source: [0.7, 0.5],
        radius: 0.1,
        feather: 0.1,
        opacity: 1.0,
    };
    let healed = render(&raw, &Recipe { healing: vec![spot], ..Recipe::default() });
    let (a, b) = (pixel(&healed, 96, 120), pixel(&plain, 224, 120));
    assert!((a[1] - b[1]).abs() < 0.02, "{a:?} {b:?}");
    assert!(is_finite(&healed));
}

#[test]
fn the_preview_matches_the_export() {
    let raw = merged(1024, 768, scene);
    let difference = preview_difference(&raw, &Recipe::default(), 512).unwrap();
    assert!(difference.p95 < 1.5, "{difference:?}");
}

#[test]
fn zooming_in_shows_full_resolution_pixels() {
    // Stripes a pixel wide, which the half-size preview can only show as flat grey.
    let raw = merged(1600, 1200, |x, _| [if x % 2.0 == 0.0 { 0.1 } else { 0.5 }; 3]);
    let engine = Engine::new(ctx(), Precision::F16).unwrap();
    let mut session = Session::open(engine, &raw, &Recipe::default()).unwrap();
    let target = session.view_target(256, 192);
    let request = ViewRequest { region: [0.4, 0.4, 0.16, 0.16], ..ViewRequest::default() };
    session.render(&Recipe::default(), &request, &target, None).unwrap();
    let view = session.read_view(&target).unwrap();
    let green: Vec<u8> = view.as_chunks::<4>().0.iter().map(|texel| texel[1]).collect();
    let (darkest, brightest) = (green.iter().min().unwrap(), green.iter().max().unwrap());
    assert!(brightest - darkest > 40, "{darkest} to {brightest}");
}

#[test]
fn a_merged_file_opens_and_renders_from_disk() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("merged.ulr");
    let reference = merged(128, 96, scene);
    let pixels: Vec<[f32; 3]> = (0..96).flat_map(|y| (0..128).map(move |x| scene(x as f32, y as f32))).collect();
    let metadata = reference.metadata.clone();
    let photo = MergedPhoto {
        width: 128,
        height: 96,
        pixels: &pixels,
        color: &reference.color,
        metadata: &metadata,
        opcodes: &reference.opcodes,
        embedded_lens: None,
        preview_jpeg: b"",
    };
    write(&path, &photo).unwrap();
    let opened = unlight_raw::load(&path).unwrap();
    let (from_file, in_memory) = (render(&opened, &Recipe::default()), render(&reference, &Recipe::default()));
    assert_eq!(from_file.pixels, in_memory.pixels);
}

#[test]
fn a_small_copy_of_a_raw_is_on_the_same_scale_as_the_scene() {
    let raw = mosaic(bayer(), 512, 384, |x, _| [0.1 + x * 5e-4, 0.3, 0.2]);
    let scene_pixels = read_scene(&raw, &Recipe::default()).unwrap();
    let reduced = read_reduced(&raw, 256).unwrap();
    assert!(reduced.width <= 256 && reduced.height <= 256);
    let scale = raw.width as usize / reduced.width as usize;
    let (x, y) = (reduced.width as usize / 2, reduced.height as usize / 2);
    let full = scene_pixels[(y * scale + scale / 2) * 512 + x * scale + scale / 2];
    let small = reduced.pixels[y * reduced.width as usize + x];
    for channel in 0..3 {
        assert!((full[channel] / small[channel] - 1.0).abs() < 0.03, "{full:?} {small:?}");
    }
}

#[test]
fn a_small_copy_of_a_merged_photo_is_available_too() {
    let raw = merged(512, 384, |_, _| [0.4, 0.5, 0.6]);
    let reduced = read_reduced(&raw, 128).unwrap();
    let [r, g, b] = reduced.pixels[reduced.pixels.len() / 2];
    assert!((r - 0.4).abs() < 1e-2 && (g - 0.5).abs() < 1e-2 && (b - 0.6).abs() < 1e-2, "{r} {g} {b}");
}
