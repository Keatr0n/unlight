//! Image quality tools on synthetic raws with real-looking sensor noise: highlight
//! reconstruction, hot pixels, noise reduction, sharpening, Texture and Dehaze.

mod support;

use std::time::Duration;
use support::{BLACK, WHITE, bayer, luma, mosaic, pixel, render};
use unlight_engine::{
    Engine, ExportOptions, ExportedImage, GpuContext, Precision, SchedulerConfig, Session,
    PreviewPair, ViewRequest, export, preview_difference, preview_pair,
};
use unlight_raw::{Cfa, GainMap, Opcode, RawImage};
use unlight_recipe::Recipe;

fn recipe(change: impl FnOnce(&mut Recipe)) -> Recipe {
    let mut recipe = Recipe::default();
    change(&mut recipe);
    recipe
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

struct Random(u64);

impl Random {
    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn gaussian(&mut self) -> f32 {
        let radius = (-2.0 * self.uniform().max(1e-7).ln()).sqrt();
        radius * (self.uniform() * std::f32::consts::TAU).cos()
    }
}

/// Adds the noise of a sensor that collects `full_well` electrons at white.
fn noisy(mut raw: RawImage, full_well: f32) -> RawImage {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let span = WHITE - BLACK;
    for sample in &mut raw.samples {
        let signal = (f32::from(*sample) - BLACK) / span;
        let variance = signal.max(0.0) / full_well + 0.003 / full_well;
        let value = signal + random.gaussian() * variance.sqrt();
        *sample = (BLACK + value.clamp(0.0, 1.0) * span).round() as u16;
    }
    raw
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

fn worst_difference(a: &ExportedImage, b: &ExportedImage) -> f32 {
    a.pixels.iter().zip(&b.pixels).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Mean and standard deviation of a rectangle, per channel, in the rendered encoding.
fn region_stats(image: &ExportedImage, [x0, y0, x1, y1]: [u32; 4]) -> ([f32; 3], [f32; 3]) {
    let mut sum = [0.0f64; 3];
    let mut squares = [0.0f64; 3];
    let count = f64::from((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        for x in x0..x1 {
            let value = pixel(image, x, y);
            for channel in 0..3 {
                sum[channel] += f64::from(value[channel]);
                squares[channel] += f64::from(value[channel]).powi(2);
            }
        }
    }
    let mean = sum.map(|value| value / count);
    let deviation = std::array::from_fn(|c| (squares[c] / count - mean[c] * mean[c]).max(0.0).sqrt() as f32);
    (mean.map(|value| value as f32), deviation)
}

fn opponent_noise(deviation: [f32; 3]) -> f32 {
    deviation.iter().sum::<f32>() / 3.0
}

fn off() -> Recipe {
    recipe(|r| {
        r.detail.noise_reduction.color = 0.0;
        r.detail.sharpening.amount = 0.0;
    })
}

mod hot_pixels {
    use super::*;

    #[test]
    fn a_lone_hot_pixel_is_repaired() {
        for cfa in [bayer(), xtrans()] {
            let mut raw = mosaic(cfa, 128, 96, |_, _| [0.05; 3]);
            let spot = (61 * 128 + 73) as usize;
            raw.samples[spot] = WHITE as u16;
            let image = render(&raw, &off());
            let around = pixel(&image, 20, 20);
            let repaired = pixel(&image, 73, 61);
            assert!((repaired[1] - around[1]).abs() < 0.02, "{repaired:?} vs {around:?}");
        }
    }

    #[test]
    fn a_point_of_light_spread_by_the_lens_survives() {
        let star = |x: f32, y: f32| {
            let distance = (x - 64.0).powi(2) + (y - 48.0).powi(2);
            [0.01 + 0.6 * (-distance / 2.0).exp(); 3]
        };
        let image = render(&mosaic(bayer(), 128, 96, star), &off());
        assert!(luma(pixel(&image, 64, 48)) > 0.4, "{:?}", pixel(&image, 64, 48));
    }

    #[test]
    fn a_bright_line_of_detail_survives() {
        let wire = |x: f32, _: f32| [if (x - 64.0).abs() < 0.6 { 0.7 } else { 0.03 }; 3];
        let image = render(&mosaic(bayer(), 128, 96, wire), &off());
        assert!(luma(pixel(&image, 64, 48)) > luma(pixel(&image, 40, 48)) + 0.2);
    }
}

mod highlights {
    use super::*;

    const MULTIPLIERS: [f32; 3] = [2.0, 1.0, 1.5];
    const WIDTH: u32 = 320;

    /// A neutral ramp whose brightness passes green's clip point, then blue's, then red's.
    /// Scene values are as balanced, so green clips at 1, blue at 1.5 and red at 2.
    fn neutral_ramp(level: impl Fn(f32) -> f32) -> RawImage {
        let width = WIDTH as f32;
        let mut raw = mosaic(bayer(), WIDTH, 96, |x, _| {
            let value = level(x / width);
            [value / MULTIPLIERS[0], value / MULTIPLIERS[1], value / MULTIPLIERS[2]]
        });
        raw.color.as_shot_multipliers = MULTIPLIERS;
        raw
    }

    fn pulled_down() -> Recipe {
        recipe(|r| {
            r.basic.exposure = -1.5;
            r.detail.noise_reduction.color = 0.0;
            r.detail.sharpening.amount = 0.0;
        })
    }

    #[test]
    fn a_clipped_channel_is_rebuilt_from_the_others() {
        let image = render(&neutral_ramp(|t| 0.3 + t * 1.15), &pulled_down());
        for x in [130, 170, 210, 250] {
            let [r, g, b] = pixel(&image, x, 48);
            assert!((r - g).abs() < 0.015 && (g - b).abs() < 0.015, "at {x}: {r} {g} {b}");
        }
    }

    #[test]
    fn data_above_the_lowest_clip_point_gives_recoverable_range() {
        let image = render(&neutral_ramp(|t| 0.8 + t * 0.65), &pulled_down());
        let at_clip = luma(pixel(&image, 70, 48));
        let above = luma(pixel(&image, 300, 48));
        assert!(above > at_clip + 0.04, "{at_clip} vs {above}");
    }

    #[test]
    fn brightness_keeps_rising_smoothly_through_the_clip_points() {
        let image = render(&neutral_ramp(|t| 0.5 + t * 1.5), &pulled_down());
        let mut previous = 0.0;
        for x in (8..WIDTH - 8).step_by(4) {
            let value = luma(pixel(&image, x, 48));
            assert!(value >= previous - 0.012, "dip at {x}: {previous} then {value}");
            previous = value;
        }
    }

    #[test]
    fn a_blown_area_fades_to_neutral_white() {
        let scene = |x: f32, y: f32| {
            let distance = ((x - 160.0).powi(2) + (y - 120.0).powi(2)).sqrt();
            let glow = 0.5 + 2.5 * (1.0 - distance / 110.0).max(0.0);
            [0.55 * glow, 0.75 * glow, 1.0 * glow]
        };
        let image = render(&mosaic(bayer(), 320, 240, scene), &pulled_down());
        let [r, g, b] = pixel(&image, 160, 120);
        assert!((r - g).abs() < 0.02 && (g - b).abs() < 0.02, "{r} {g} {b}");
    }

    #[test]
    fn a_partly_clipped_colour_matches_what_an_unclipped_exposure_shows() {
        let colour = |x: f32, scale: f32| {
            let level = (0.5 + x / 256.0) * scale;
            [0.9 * level, 0.45 * level, 0.3 * level]
        };
        let clipped = mosaic(bayer(), 256, 96, |x, _| colour(x, 1.0));
        let intact = mosaic(bayer(), 256, 96, |x, _| colour(x, 0.5));
        let image = render(&clipped, &pulled_down());
        let reference = render(&intact, &recipe(|r| {
            r.basic.exposure = -0.5;
            r.detail.noise_reduction.color = 0.0;
            r.detail.sharpening.amount = 0.0;
        }));
        for x in [200, 225, 245] {
            let (seen, expected) = (pixel(&image, x, 48), pixel(&reference, x, 48));
            for channel in 0..3 {
                assert!((seen[channel] - expected[channel]).abs() < 0.03, "at {x}: {seen:?} vs {expected:?}");
            }
        }
    }

    #[test]
    fn the_clip_mask_comes_from_before_the_gain_map() {
        let mut raw = mosaic(bayer(), 128, 96, |_, _| [1.0; 3]);
        raw.color.as_shot_multipliers = MULTIPLIERS;
        raw.opcodes.list2 = (0..2)
            .flat_map(|top| (0..2).map(move |left| (top, left)))
            .map(|(top, left)| {
                Opcode::GainMap(GainMap {
                    top,
                    left,
                    bottom: 96,
                    right: 128,
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
                    gains: vec![0.7; 4],
                })
            })
            .collect();
        let [r, g, b] = pixel(&render(&raw, &pulled_down()), 64, 48);
        assert!((r - g).abs() < 0.015 && (g - b).abs() < 0.015, "{r} {g} {b}");
    }
}

mod noise_reduction {
    use super::*;

    const FULL_WELL: f32 = 900.0;
    /// Big enough for a preview pyramid several levels deep, which is where chroma noise goes.
    const SIZE: [u32; 2] = [2048, 1024];
    const MIDDLE: [u32; 4] = [500, 250, 1500, 750];

    fn with_noise(luminance: f32, color: f32) -> Recipe {
        recipe(|r| {
            r.detail.noise_reduction.luminance = luminance;
            r.detail.noise_reduction.color = color;
            r.detail.sharpening.amount = 0.0;
        })
    }

    fn grey_card() -> RawImage {
        noisy(mosaic(bayer(), SIZE[0], SIZE[1], |_, _| [0.18; 3]), FULL_WELL)
    }

    fn stripes(x: f32, y: f32) -> [f32; 3] {
        let level = if ((x + y * 0.5) / 9.0).floor() as i32 % 2 == 0 { 0.5 } else { 0.1 };
        [level, level * 0.9, level * 0.8]
    }

    #[test]
    fn output_does_not_depend_on_tile_size() {
        for cfa in [bayer(), xtrans()] {
            let raw = noisy(mosaic(cfa, 610, 430, stripes), FULL_WELL);
            let settings = with_noise(60.0, 50.0);
            let small = export(&raw, &settings, &fixed_tiles(97)).unwrap();
            let large = export(&raw, &settings, &fixed_tiles(1024)).unwrap();
            assert!(small.tiles > large.tiles);
            let worst = worst_difference(&small, &large);
            assert!(worst < 1e-4, "tilings differ by {worst}");
        }
    }

    #[test]
    fn luminance_reduces_noise_and_keeps_the_mean() {
        let raw = grey_card();
        let (before_mean, before) = region_stats(&render(&raw, &with_noise(0.0, 0.0)), MIDDLE);
        let (after_mean, after) = region_stats(&render(&raw, &with_noise(60.0, 25.0)), MIDDLE);
        assert!(opponent_noise(after) < opponent_noise(before) * 0.3, "{before:?} → {after:?}");
        for channel in 0..3 {
            assert!((after_mean[channel] - before_mean[channel]).abs() < 0.006, "{before_mean:?} → {after_mean:?}");
        }
    }

    #[test]
    fn the_slider_fades_in_rather_than_jumping() {
        let raw = grey_card();
        let noise_at = |luminance| opponent_noise(region_stats(&render(&raw, &with_noise(luminance, 25.0)), MIDDLE).1);
        let (off, low, middle, high) = (noise_at(0.0), noise_at(10.0), noise_at(30.0), noise_at(80.0));
        assert!(off > low && low > middle && middle > high, "{off} {low} {middle} {high}");
        assert!(low > off * 0.5, "{off} → {low}");
    }

    #[test]
    fn color_removes_colour_noise_without_touching_luminance_noise() {
        let raw = noisy(mosaic(bayer(), SIZE[0], SIZE[1], |_, _| [0.3, 0.25, 0.2]), FULL_WELL);
        let spread = |image: &ExportedImage| {
            let mut sum = 0.0f64;
            let mut squares = 0.0f64;
            let mut count = 0.0f64;
            for y in MIDDLE[1]..MIDDLE[3] {
                for x in MIDDLE[0]..MIDDLE[2] {
                    let [r, _, b] = pixel(image, x, y);
                    let difference = f64::from(r - b);
                    sum += difference;
                    squares += difference * difference;
                    count += 1.0;
                }
            }
            (squares / count - (sum / count) * (sum / count)).sqrt()
        };
        let before = spread(&render(&raw, &with_noise(0.0, 0.0)));
        let after = spread(&render(&raw, &with_noise(0.0, 25.0)));
        assert!(after < before * 0.75, "{before} → {after}");
    }

    #[test]
    fn edges_survive() {
        let scene = |x: f32, _: f32| [if x < 256.0 { 0.08 } else { 0.5 }; 3];
        let raw = noisy(mosaic(bayer(), 512, 192, scene), FULL_WELL);
        let image = render(&raw, &with_noise(60.0, 25.0));
        let average = |x: u32| (40..150).map(|y| luma(pixel(&image, x, y))).sum::<f32>() / 110.0;
        let (dark, bright) = (average(230), average(282));
        assert!(average(252) < dark + (bright - dark) * 0.2, "{dark} {} {bright}", average(252));
        assert!(average(259) > bright - (bright - dark) * 0.2, "{dark} {} {bright}", average(259));
    }

    #[test]
    fn low_contrast_texture_outlives_moderate_settings() {
        let weave = |x: f32, y: f32| {
            let level = 0.2 + 0.05 * ((x / 3.0).sin() * (y / 3.0).sin());
            [level; 3]
        };
        let raw = noisy(mosaic(bayer(), SIZE[0], SIZE[1], weave), FULL_WELL * 4.0);
        let clean = render(&mosaic(bayer(), SIZE[0], SIZE[1], weave), &with_noise(0.0, 0.0));
        let image = render(&raw, &with_noise(25.0, 25.0));
        let (_, kept) = region_stats(&image, MIDDLE);
        let (_, original) = region_stats(&clean, MIDDLE);
        assert!(kept[1] > original[1] * 0.75, "{original:?} → {kept:?}");
    }

    #[test]
    fn a_noiseless_photo_is_left_exactly_alone() {
        let raw = mosaic(bayer(), 256, 192, stripes);
        let off = render(&raw, &with_noise(0.0, 0.0));
        let on = render(&raw, &with_noise(80.0, 80.0));
        assert_eq!(off.pixels, on.pixels);
    }

    #[test]
    fn detail_settings_trade_noise_for_texture() {
        let raw = grey_card();
        let with = |change: fn(&mut Recipe)| {
            let mut settings = with_noise(40.0, 40.0);
            change(&mut settings);
            opponent_noise(region_stats(&render(&raw, &settings), MIDDLE).1)
        };
        let base = with(|r| r.detail.noise_reduction.luminance_detail = 0.0);
        let more_detail = with(|r| r.detail.noise_reduction.luminance_detail = 100.0);
        assert!(more_detail > base, "{base} vs {more_detail}");
        let low_contrast = with(|r| r.detail.noise_reduction.luminance_contrast = 0.0);
        let high_contrast = with(|r| r.detail.noise_reduction.luminance_contrast = 100.0);
        assert!(high_contrast > low_contrast, "{low_contrast} vs {high_contrast}");
    }

    #[test]
    fn smoothness_reaches_coarser_colour_blotches() {
        let raw = noisy(mosaic(bayer(), SIZE[0], SIZE[1], |_, _| [0.2, 0.17, 0.15]), FULL_WELL * 0.25);
        let blotches = |smoothness: f32| {
            let settings = recipe(|r| {
                r.detail.noise_reduction.color = 40.0;
                r.detail.noise_reduction.color_smoothness = smoothness;
                r.detail.sharpening.amount = 0.0;
            });
            let image = render(&raw, &settings);
            let mut cells = Vec::new();
            for cy in 0..10 {
                for cx in 0..20 {
                    let mut sum = 0.0f64;
                    for y in 0..12 {
                        for x in 0..12 {
                            let [r, _, b] = pixel(&image, 200 + cx * 80 + x, 100 + cy * 80 + y);
                            sum += f64::from(r - b);
                        }
                    }
                    cells.push(sum / 144.0);
                }
            }
            let mean = cells.iter().sum::<f64>() / cells.len() as f64;
            cells.iter().map(|value| (value - mean).powi(2)).sum::<f64>()
        };
        let (mottled, smooth) = (blotches(0.0), blotches(100.0));
        assert!(smooth < mottled * 0.8, "{mottled} → {smooth}");
    }

    #[test]
    fn a_tone_slider_never_re_solves() {
        let raw = grey_card();
        let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
        let settings = with_noise(40.0, 40.0);
        let mut session = Session::open(engine, &raw, &settings).unwrap();
        let target = session.view_target(256, 192);
        session.render(&settings, &ViewRequest::default(), &target, None).unwrap();
        let settled = session.denoise_solves();
        assert!(settled > 0);

        let mut edited = settings.clone();
        edited.basic.exposure = 1.2;
        edited.basic.contrast = 30.0;
        edited.basic.texture = 40.0;
        edited.white_balance = unlight_recipe::WhiteBalance::Custom { temperature: 4000.0, tint: 10.0 };
        session.render(&edited, &ViewRequest::default(), &target, None).unwrap();
        assert_eq!(session.denoise_solves(), settled);

        edited.detail.noise_reduction.color = 70.0;
        session.render(&edited, &ViewRequest::default(), &target, None).unwrap();
        assert!(session.denoise_solves() > settled);
    }

    fn zoomed_view(session: &mut Session, settings: &Recipe, raw: &RawImage) -> Vec<u8> {
        let target = session.view_target(128, 96);
        let step = [128.0 / raw.width as f32, 96.0 / raw.height as f32];
        let region = [0.5, 0.5, step[0], step[1]];
        session.render(settings, &ViewRequest { region, ..ViewRequest::default() }, &target, None).unwrap();
        session.read_view(&target).unwrap()
    }

    #[test]
    fn a_zoomed_view_shows_what_the_export_will() {
        let raw = noisy(mosaic(bayer(), 1024, 768, stripes), FULL_WELL);
        let settings = with_noise(50.0, 40.0);
        let exported = render(&raw, &settings);
        let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
        let mut session = Session::open(engine, &raw, &settings).unwrap();
        let view = zoomed_view(&mut session, &settings, &raw);
        let mut total = 0.0;
        for y in 0..96u32 {
            for x in 0..128u32 {
                let start = ((y * 128 + x) * 4) as usize;
                let shown = f32::from(view[start + 1]) / 255.0;
                total += (shown - pixel(&exported, 512 + x, 384 + y)[1]).abs();
            }
        }
        let mean = total / (128.0 * 96.0);
        assert!(mean < 0.01, "mean difference {mean}");
    }

    #[test]
    fn changing_a_setting_refreshes_zoomed_tiles() {
        let raw = noisy(mosaic(bayer(), SIZE[0], SIZE[1], |_, _| [0.18; 3]), FULL_WELL);
        let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
        let (light, heavy) = (with_noise(5.0, 25.0), with_noise(90.0, 25.0));
        let mut session = Session::open(engine, &raw, &light).unwrap();
        let first = zoomed_view(&mut session, &light, &raw);
        let second = zoomed_view(&mut session, &heavy, &raw);
        assert_ne!(first, second);
    }
}

mod sharpening {
    use super::*;

    fn step(x: f32, _: f32) -> [f32; 3] {
        [if x < 128.0 { 0.1 } else { 0.5 }; 3]
    }

    fn sharpened(change: impl FnOnce(&mut Recipe)) -> Recipe {
        recipe(|r| {
            r.detail.noise_reduction.color = 0.0;
            r.detail.sharpening = unlight_recipe::Sharpening { amount: 120.0, radius: 1.5, detail: 50.0, masking: 0.0 };
            change(r);
        })
    }

    fn profile(image: &ExportedImage) -> Vec<f32> {
        (100..156).map(|x| luma(pixel(image, x, 48))).collect()
    }

    #[test]
    fn zero_amount_is_a_no_op_at_any_setting() {
        let raw = mosaic(bayer(), 256, 96, step);
        let plain = render(&raw, &off());
        let idle = render(&raw, &sharpened(|r| r.detail.sharpening.amount = 0.0));
        assert_eq!(plain.pixels, idle.pixels);
    }

    #[test]
    fn edges_gain_contrast_and_flat_areas_stay_flat() {
        let raw = mosaic(bayer(), 256, 96, step);
        let (base, sharp) = (render(&raw, &off()), render(&raw, &sharpened(|_| {})));
        let slope = |image: &ExportedImage| {
            let line = profile(image);
            line.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max)
        };
        assert!(slope(&sharp) > slope(&base) * 1.2, "{} → {}", slope(&base), slope(&sharp));
        for channel in 0..3 {
            let (a, b) = (pixel(&base, 30, 48)[channel], pixel(&sharp, 30, 48)[channel]);
            assert!((a - b).abs() < 1e-3);
        }
    }

    #[test]
    fn low_detail_holds_back_halos() {
        let raw = mosaic(bayer(), 256, 96, step);
        let overshoot = |detail: f32| {
            let image = render(&raw, &sharpened(|r| r.detail.sharpening.detail = detail));
            let line = profile(&image);
            let (bright, top) = (luma(pixel(&image, 220, 48)), line.iter().copied().fold(0.0, f32::max));
            top - bright
        };
        let (low, high) = (overshoot(0.0), overshoot(100.0));
        assert!(low < 0.02 && high > low * 2.0, "{low} vs {high}");
    }

    #[test]
    fn masking_spares_noise_in_flat_areas_and_keeps_edges() {
        let raw = noisy(mosaic(bayer(), 512, 192, |x, y| step(x * 0.5, y)), 4000.0);
        let flat_noise = |masking: f32| {
            let image = render(&raw, &sharpened(|r| r.detail.sharpening.masking = masking));
            opponent_noise(region_stats(&image, [300, 40, 480, 150]).1)
        };
        assert!(flat_noise(100.0) < flat_noise(0.0) * 0.85, "{} vs {}", flat_noise(100.0), flat_noise(0.0));
    }

    #[test]
    fn the_mask_preview_shows_edges_white_and_flat_areas_black() {
        let raw = mosaic(bayer(), 256, 96, step);
        let settings = sharpened(|r| r.detail.sharpening.masking = 60.0);
        let engine = Engine::new(GpuContext::new().unwrap(), Precision::F16).unwrap();
        let mut session = Session::open(engine, &raw, &settings).unwrap();
        session.show_sharpen_mask = true;
        let target = session.view_target(256, 96);
        session.render(&settings, &ViewRequest::default(), &target, None).unwrap();
        let view = session.read_view(&target).unwrap();
        let at = |x: usize| view[(48 * 256 + x) * 4];
        assert!(at(128) > 200 && at(30) == 0, "edge {} flat {}", at(128), at(30));
    }
}

mod texture {
    use super::*;

    /// Texture's band is relative to the photo, so this needs a photo wide enough for a weave
    /// of a dozen pixels to fall inside it.
    const WIDE: [u32; 2] = [2048, 256];

    fn textured(x: f32, y: f32) -> [f32; 3] {
        let weave = ((x / 2.0).sin() + (y / 2.0).sin()) * 0.25;
        [0.18 * 2f32.powf(weave); 3]
    }

    fn with_texture(amount: f32) -> Recipe {
        recipe(|r| {
            r.basic.texture = amount;
            r.detail.noise_reduction.color = 0.0;
            r.detail.sharpening.amount = 0.0;
        })
    }

    #[test]
    fn it_lifts_mid_frequency_contrast_and_negative_amounts_lower_it() {
        let raw = mosaic(bayer(), WIDE[0], WIDE[1], textured);
        let contrast = |amount: f32| region_stats(&render(&raw, &with_texture(amount)), [400, 40, 1600, 216]).1[1];
        let (flat, up, down) = (contrast(0.0), contrast(80.0), contrast(-80.0));
        assert!(up > flat * 1.1 && down < flat * 0.9, "{down} {flat} {up}");
    }

    #[test]
    fn it_leaves_flat_colour_alone() {
        let raw = mosaic(bayer(), 256, 192, |_, _| [0.2, 0.15, 0.1]);
        let base = render(&raw, &with_texture(0.0));
        let edited = render(&raw, &with_texture(100.0));
        assert!(worst_difference(&base, &edited) < 0.004);
    }

    #[test]
    fn strong_edges_do_not_grow_halos() {
        let raw = mosaic(bayer(), 512, 192, |x, _| [if x < 256.0 { 0.05 } else { 0.6 }; 3]);
        let base = render(&raw, &with_texture(0.0));
        let edited = render(&raw, &with_texture(100.0));
        for x in [226, 236, 246, 266, 276, 286] {
            let (a, b) = (luma(pixel(&base, x, 96)), luma(pixel(&edited, x, 96)));
            assert!((a - b).abs() < 0.02, "at {x}: {a} → {b}");
        }
    }

    #[test]
    fn output_does_not_depend_on_tile_size() {
        let raw = noisy(mosaic(bayer(), 610, 430, textured), 4000.0);
        let settings = recipe(|r| {
            r.basic.texture = 60.0;
            r.basic.clarity = 30.0;
            r.basic.dehaze = 40.0;
            r.detail.noise_reduction.luminance = 30.0;
            r.detail.sharpening = unlight_recipe::Sharpening { amount: 120.0, radius: 3.0, detail: 40.0, masking: 30.0 };
        });
        let small = export(&raw, &settings, &fixed_tiles(97)).unwrap();
        let large = export(&raw, &settings, &fixed_tiles(1024)).unwrap();
        assert!(worst_difference(&small, &large) < 1e-4);
    }
}

mod dehaze {
    use super::*;

    const MAX_DEHAZE: fn() -> Recipe = || {
        recipe(|r| {
            r.basic.dehaze = 100.0;
            r.detail.noise_reduction.color = 0.0;
            r.detail.sharpening.amount = 0.0;
        })
    };

    /// A hazy backdrop with something clear in front of it on the right half.
    fn hazy_scene(front: f32) -> RawImage {
        mosaic(bayer(), 512, 256, move |x, y| {
            let hazy = 0.55 + 0.1 * (y / 256.0);
            [if x > 256.0 { front } else { hazy }; 3]
        })
    }

    #[test]
    fn dark_objects_do_not_turn_into_black_holes() {
        let raw = hazy_scene(0.03);
        let (base, edited) = (render(&raw, &off()), render(&raw, &MAX_DEHAZE()));
        let (before, after) = (luma(pixel(&base, 384, 128)), luma(pixel(&edited, 384, 128)));
        // The floor is 0.3 of the scene light, which the default curve's contrast deepens.
        assert!(after > before * 0.35, "{before} → {after}");
    }

    #[test]
    fn bright_objects_do_not_blow_out() {
        let raw = hazy_scene(0.95);
        let edited = render(&raw, &MAX_DEHAZE());
        let [r, g, b] = pixel(&edited, 384, 128);
        assert!(r.max(g).max(b) < 0.999, "{r} {g} {b}");
    }

    #[test]
    fn haze_removal_stops_at_edges() {
        let raw = hazy_scene(0.04);
        let edited = render(&raw, &MAX_DEHAZE());
        let (near, far) = (luma(pixel(&edited, 244, 128)), luma(pixel(&edited, 120, 128)));
        assert!((near - far).abs() < 0.05, "near the edge {near}, away from it {far}");
    }
}

/// The plan's rule: a tool only ships if the preview shows what the export will.
mod agreement {
    use super::*;

    const LIMIT: f64 = 1.5;

    fn scene(x: f32, y: f32) -> [f32; 3] {
        let tiles = ((x / 37.0).floor() as i32 + (y / 37.0).floor() as i32) % 3;
        let base = [0.08, 0.2, 0.45][tiles as usize];
        let fine = 1.0 + 0.1 * ((x / 3.0).sin() * (y / 4.0).cos());
        [base * fine * (0.8 + x / 2000.0), base * fine, base * fine * (1.1 - y / 1500.0)]
    }

    fn tools() -> Vec<(&'static str, Recipe)> {
        vec![
            ("default", Recipe::default()),
            ("exposure", recipe(|r| r.basic.exposure = 1.0)),
            ("contrast", recipe(|r| r.basic.contrast = 50.0)),
            ("highlights", recipe(|r| r.basic.highlights = -80.0)),
            ("shadows", recipe(|r| r.basic.shadows = 80.0)),
            ("clarity", recipe(|r| r.basic.clarity = 60.0)),
            ("texture", recipe(|r| r.basic.texture = 60.0)),
            ("dehaze", recipe(|r| r.basic.dehaze = 60.0)),
            ("vibrance", recipe(|r| r.basic.vibrance = 60.0)),
            ("sharpening", recipe(|r| r.detail.sharpening.amount = 100.0)),
            ("luminance noise", recipe(|r| r.detail.noise_reduction.luminance = 60.0)),
            ("colour noise", recipe(|r| r.detail.noise_reduction.color = 80.0)),
        ]
    }

    /// How far `moved` sits from `fixed` in pixels, by least squares on the gradient of `fixed`.
    fn shift(pair: &PreviewPair, channel: usize) -> [f32; 2] {
        let (width, height) = (pair.width as usize, pair.height as usize);
        let at = |image: &[f32], x: usize, y: usize| image[(y * width + x) * 3 + channel];
        let (mut xx, mut xy, mut yy, mut xd, mut yd) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for y in 8..height - 8 {
            for x in 8..width - 8 {
                let gx = f64::from(at(&pair.shown, x + 1, y) - at(&pair.shown, x - 1, y)) * 0.5;
                let gy = f64::from(at(&pair.shown, x, y + 1) - at(&pair.shown, x, y - 1)) * 0.5;
                let change = f64::from(at(&pair.exported, x, y) - at(&pair.shown, x, y));
                xx += gx * gx;
                xy += gx * gy;
                yy += gy * gy;
                xd += gx * change;
                yd += gy * change;
            }
        }
        let determinant = xx * yy - xy * xy;
        [(-(yy * xd - xy * yd) / determinant) as f32, (-(xx * yd - xy * xd) / determinant) as f32]
    }

    /// Bayer with the red site at any of the four positions a crop can leave it.
    fn bayer_from(left: u32, top: u32) -> Cfa {
        let plain = bayer();
        let mut pattern = plain.pattern;
        for (row, sites) in pattern.iter_mut().enumerate() {
            for (col, site) in sites.iter_mut().enumerate() {
                *site = plain.color_at(col as u32 + left, row as u32 + top);
            }
        }
        Cfa { period: 2, pattern }
    }

    fn ripples(x: f32, y: f32) -> [f32; 3] {
        let value = 0.25 + 0.15 * ((x + y * 0.4) / 9.0).sin() * ((y - x * 0.3) / 11.0).cos();
        [value; 3]
    }

    /// An odd-sized level has no exact half and a Bayer block's colours aren't at its centre,
    /// so a careless preview shows every colour plane in a slightly different place.
    #[test]
    fn colour_planes_sit_where_the_export_puts_them() {
        const TOLERANCE: f32 = 0.08;
        for (width, height) in [(1030, 774), (1032, 776)] {
            for (left, top) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let raw = mosaic(bayer_from(left, top), width, height, ripples);
                let pair = preview_pair(&raw, &off(), width / 4).unwrap();
                for channel in 0..3 {
                    let [dx, dy] = shift(&pair, channel);
                    let case = format!("{width}x{height} phase {left},{top} channel {channel}: {dx}, {dy}");
                    assert!(dx.abs() < TOLERANCE && dy.abs() < TOLERANCE, "{case}");
                }
            }
        }
    }

    #[test]
    fn preview_matches_a_scaled_export_for_each_tool() {
        let raw = noisy(mosaic(bayer(), 1024, 768, scene), 3000.0);
        for (name, settings) in tools() {
            let difference = preview_difference(&raw, &settings, 512).unwrap();
            assert!(difference.mean < LIMIT, "{name}: {difference:?}");
        }
    }

    #[test]
    fn preview_matches_a_scaled_export_on_real_photos() {
        let Ok(samples) = std::env::var("UNLIGHT_SAMPLES") else {
            return;
        };
        for name in ["sony-a7rm4.ARW", "sony-a7c-iso12800.ARW", "fuji-xt4.RAF"] {
            let raw = unlight_raw::load(&std::path::Path::new(&samples).join(name)).unwrap();
            for (tool, settings) in tools() {
                let difference = preview_difference(&raw, &settings, 1600).unwrap();
                assert!(difference.mean < LIMIT, "{name} {tool}: {difference:?}");
            }
        }
    }
}
