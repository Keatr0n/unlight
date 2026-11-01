//! Auto tone through the real render. The CPU model it solves against has to agree with the
//! shader, and on real photos the result has to hold up against the camera's own JPEG.

mod support;

use std::{env, path::PathBuf};
use support::{flat, mosaic, bayer};
use unlight_color::{delta_e_2000, srgb_to_lab};
use unlight_engine::{AutoTone, Engine, GpuContext, Precision, Session, ViewRequest, auto_tone};
use unlight_raw::{Preview, RawImage};
use unlight_recipe::{Crop, Recipe};
use zune_jpeg::JpegDecoder;

/// Mean luma a default or Auto render of an ordinary photo should land in.
const MEAN_BAND: [f32; 2] = [0.27, 0.72];
/// Extra clipped pixels Auto may add, at either end, as a share of the photo.
const CLIPPING_TOLERANCE: f32 = 0.005;
/// How much worse than the default a sample may end up, in mean ΔE2000. That's well under what
/// anyone can see: a just-noticeable difference is about 1.
const DELTA_E_TOLERANCE: f64 = 0.1;

fn session(raw: &RawImage) -> Session {
    let engine = Engine::new(GpuContext::new().expect("a device"), Precision::F16).expect("engine");
    Session::open(engine, raw, &Recipe::default()).expect("session")
}

fn view(session: &mut Session, recipe: &Recipe, width: u32, height: u32) -> Vec<u8> {
    let target = session.view_target(width, height);
    session.render(recipe, &ViewRequest::default(), &target, None).expect("render");
    session.read_view(&target).expect("readback")
}

fn luma(pixel: &[u8]) -> f32 {
    (0.2126 * f32::from(pixel[0]) + 0.7152 * f32::from(pixel[1]) + 0.0722 * f32::from(pixel[2])) / 255.0
}

fn mean_luma(rgba: &[u8]) -> f32 {
    let pixels = rgba.as_chunks::<4>().0;
    pixels.iter().map(|pixel| luma(pixel)).sum::<f32>() / pixels.len() as f32
}

#[test]
fn the_cpu_model_matches_the_shader_for_every_slider() {
    let settings = [
        AutoTone::default(),
        AutoTone { exposure: 0.8, ..AutoTone::default() },
        AutoTone { exposure: -0.7, ..AutoTone::default() },
        AutoTone { contrast: 30.0, ..AutoTone::default() },
        AutoTone { highlights: -60.0, ..AutoTone::default() },
        AutoTone { shadows: 60.0, ..AutoTone::default() },
        AutoTone { whites: 40.0, ..AutoTone::default() },
        AutoTone { blacks: -40.0, ..AutoTone::default() },
        AutoTone { exposure: 0.4, contrast: 10.0, highlights: -25.0, shadows: 20.0, whites: 15.0, blacks: -10.0, ..AutoTone::default() },
    ];
    for level in [0.01f32, 0.05, 0.18, 0.45, 0.8] {
        let raw = flat([level; 3]);
        let mut session = session(&raw);
        let stats = session.tone_stats(&Recipe::default()).unwrap();
        let ev = stats.quantile(0.5);
        for tone in &settings {
            let mut recipe = Recipe::default();
            tone.apply_to(&mut recipe.basic);
            let rendered = mean_luma(&view(&mut session, &recipe, 32, 32));
            let predicted = stats.model.output(ev, tone);
            assert!((rendered - predicted).abs() < 0.015, "{level}, {tone:?}: {rendered} vs {predicted}");
        }
    }
}

#[test]
fn stats_follow_the_crop() {
    let raw = mosaic(bayer(), 256, 256, |x, _| if x < 128.0 { [0.01; 3] } else { [0.5; 3] });
    let mut session = session(&raw);
    let whole = session.tone_stats(&Recipe::default()).unwrap();
    let dark = Recipe { crop: Crop { right: 0.4, ..Crop::default() }, ..Recipe::default() };
    let cropped = session.tone_stats(&dark).unwrap();
    assert!(cropped.quantile(0.9) < whole.quantile(0.9) - 2.0, "{} vs {}", cropped.quantile(0.9), whole.quantile(0.9));
}

#[test]
fn asking_twice_gives_the_same_answer_whatever_the_sliders_say() {
    let raw = mosaic(bayer(), 256, 256, |x, y| [0.02 + x / 900.0 + y / 2000.0; 3]);
    let mut session = session(&raw);
    let first = session.auto_tone(&Recipe::default()).unwrap();
    let mut edited = Recipe::default();
    first.apply_to(&mut edited.basic);
    assert_eq!(session.auto_tone(&edited).unwrap(), first);
}

#[test]
fn a_dark_daylight_frame_comes_up_through_the_real_render() {
    let raw = mosaic(bayer(), 256, 256, |x, _| [0.002 + x * 0.00002; 3]);
    let mut session = session(&raw);
    let plain = Recipe::default();
    let before = mean_luma(&view(&mut session, &plain, 64, 64));
    let tone = session.auto_tone(&plain).unwrap();
    let mut edited = plain.clone();
    tone.apply_to(&mut edited.basic);
    assert!(mean_luma(&view(&mut session, &edited, 64, 64)) > before + 0.1, "{tone:?}");
}

struct Reference {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

fn camera_jpeg(path: &std::path::Path) -> Option<Reference> {
    match unlight_raw::load_preview(path).ok()? {
        Preview::Rgb8 { width, height, data } => Some(Reference { width: width as usize, height: height as usize, rgb: data }),
        Preview::Jpeg(bytes) => {
            let mut decoder = JpegDecoder::new(std::io::Cursor::new(&bytes));
            decoder.decode_headers().ok()?;
            let (width, height) = decoder.dimensions()?;
            Some(Reference { width, height, rgb: decoder.decode().ok()? })
        }
    }
}

/// Mean ΔE2000 against a reference sampled at the render's pixels.
fn difference(rgba: &[u8], width: usize, height: usize, reference: &Reference) -> f64 {
    let color = |pixel: &[u8]| [pixel[0], pixel[1], pixel[2]].map(|value| f32::from(value) / 255.0);
    let (mut sum, mut count) = (0.0, 0u64);
    for y in (0..height).step_by(5) {
        for x in (0..width).step_by(5) {
            let ours = &rgba[(y * width + x) * 4..][..3];
            let (from_x, from_y) = (x * reference.width / width, y * reference.height / height);
            let theirs = &reference.rgb[(from_y * reference.width + from_x) * 3..][..3];
            sum += delta_e_2000(srgb_to_lab(color(ours)), srgb_to_lab(color(theirs)));
            count += 1;
        }
    }
    sum / count as f64
}

fn clipped(rgba: &[u8]) -> (f32, f32) {
    let pixels = rgba.as_chunks::<4>().0;
    let count = pixels.len() as f32;
    let share = |test: &dyn Fn(&u8) -> bool| pixels.iter().filter(|pixel| pixel[..3].iter().all(test)).count() as f32 / count;
    (share(&|value| *value >= 254), share(&|value| *value <= 1))
}

/// Pixel is left out of the camera comparison: its JPEG is HDR+ tone-mapped from a burst.
#[test]
fn real_photos_hold_up_against_the_camera() {
    let Some(samples) = env::var_os("UNLIGHT_SAMPLES").map(PathBuf::from).filter(|path| path.is_dir()) else {
        return;
    };
    const NAMES: [&str; 9] = [
        "canon-5d4.CR2",
        "canon-eosr.CR3",
        "canon-m6ii-craw.CR3",
        "fuji-xt4.RAF",
        "gh5.RW2",
        "hasselblad-h6d.fff",
        "nikon-z6.NEF",
        "sony-a7c-iso12800.ARW",
        "sony-a7rm4.ARW",
    ];
    let (mut before_total, mut after_total) = (0.0, 0.0);
    for name in NAMES {
        let path = samples.join(name);
        let raw = unlight_raw::load(&path).expect("decode");
        let mut session = session(&raw);
        let plain = Recipe::default();
        let tone = session.auto_tone(&plain).unwrap();
        let mut edited = plain.clone();
        tone.apply_to(&mut edited.basic);
        let [full_width, full_height] = session.output_size(&plain);
        let (width, height) = (960, (960 * u64::from(full_height) / u64::from(full_width)) as usize);
        let before = view(&mut session, &plain, width, height as u32);
        let after = view(&mut session, &edited, width, height as u32);
        let reference = camera_jpeg(&path).expect("an embedded JPEG");

        let mean = mean_luma(&after);
        assert!((MEAN_BAND[0]..=MEAN_BAND[1]).contains(&mean), "{name}: mean {mean}, {tone:?}");
        let ((white_before, black_before), (white_after, black_after)) = (clipped(&before), clipped(&after));
        assert!(white_after <= white_before + CLIPPING_TOLERANCE, "{name}: {white_before} to {white_after}");
        assert!(black_after <= black_before + CLIPPING_TOLERANCE, "{name}: {black_before} to {black_after}");
        let (was, is) = (
            difference(&before, width as usize, height, &reference),
            difference(&after, width as usize, height, &reference),
        );
        assert!(is <= was + DELTA_E_TOLERANCE, "{name}: ΔE {was} to {is}, {tone:?}");
        before_total += was;
        after_total += is;
    }
    assert!(after_total <= before_total, "ΔE summed {before_total} to {after_total}");
}

/// What three stops less exposure does to a file: an eighth of the signal, from a shutter open
/// an eighth as long. The scene was as bright as ever, so the exposure settings still say so.
fn underexposed(mut raw: RawImage) -> RawImage {
    const STOPS_DOWN: f32 = 8.0;
    let width = raw.width as usize;
    let black = raw.black;
    for (index, sample) in raw.samples.iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let floor = black[y % 6][x % 6];
        *sample = (floor + (f32::from(*sample) - floor).max(0.0) / STOPS_DOWN).round() as u16;
    }
    raw.metadata.shutter /= STOPS_DOWN;
    raw
}

#[test]
fn a_real_photo_three_stops_under_comes_back_up() {
    let Some(samples) = env::var_os("UNLIGHT_SAMPLES").map(PathBuf::from).filter(|path| path.is_dir()) else {
        return;
    };
    for name in ["gh5.RW2", "canon-eosr.CR3"] {
        let raw = underexposed(unlight_raw::load(&samples.join(name)).expect("decode"));
        let mut session = session(&raw);
        let plain = Recipe::default();
        let dark = mean_luma(&view(&mut session, &plain, 480, 320));
        let tone = session.auto_tone(&plain).unwrap();
        let mut edited = plain.clone();
        tone.apply_to(&mut edited.basic);
        let lifted = mean_luma(&view(&mut session, &edited, 480, 320));
        assert!(dark < 0.25, "{name} should start dark: {dark}");
        assert!(tone.exposure > 1.0, "{name}: {tone:?}");
        assert!(lifted >= MEAN_BAND[0] - 0.03, "{name}: {dark} came up to {lifted}, {tone:?}");
    }
}

#[test]
fn the_pure_function_is_what_the_session_uses() {
    let raw = flat([0.2; 3]);
    let mut session = session(&raw);
    let stats = session.tone_stats(&Recipe::default()).unwrap();
    assert_eq!(session.auto_tone(&Recipe::default()).unwrap(), auto_tone(&stats));
}
