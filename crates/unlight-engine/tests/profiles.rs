//! Camera profiles through the whole render. The expected values come from a CPU copy of the
//! DNG table lookup, so these pin the shader to the spec rather than to itself.

mod support;

use std::path::Path;
use support::{bayer, center, chroma, flat_render, luma, mosaic, render};
use unlight_engine::{ExportOptions, OutputSpace, export};
use unlight_raw::{DngColor, DngIlluminant, RawImage};
use unlight_recipe::{Recipe, WhiteBalance};

const TAG_ILLUMINANT_1: u16 = 50778;
const TAG_ILLUMINANT_2: u16 = 50779;
const TAG_HUE_SAT_DIMS: u16 = 50937;
const TAG_HUE_SAT_1: u16 = 50938;
const TAG_HUE_SAT_2: u16 = 50939;
const TAG_TONE_CURVE: u16 = 50940;
const TAG_LOOK_DIMS: u16 = 50981;
const TAG_LOOK_DATA: u16 = 50982;
const TAG_HUE_SAT_ENCODING: u16 = 51107;
const TAG_LOOK_ENCODING: u16 = 51108;
const TAG_EXPOSURE_OFFSET: u16 = 51109;
const TAG_BLACK_RENDER: u16 = 51110;

const KIND_SHORT: u16 = 3;
const KIND_LONG: u16 = 4;
const KIND_FLOAT: u16 = 11;

const IDENTITY: [f32; 3] = [0.0, 1.0, 1.0];
const LINEAR_CURVE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const NO_AUTO_BLACK: u32 = 1;

/// ProPhoto's transfer exponent. Rendering to the working space means nothing clips to sRGB,
/// so output is the table's result itself.
const PROPHOTO_GAMMA: f32 = 461.0 / 256.0;

const XYZ_TO_SRGB_CAMERA: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.969266, 1.8760108, 0.041556],
    [0.0556434, -0.2040259, 1.0572252],
];

#[derive(Default)]
struct Profile(Vec<(u16, u16, Vec<u8>)>);

impl Profile {
    fn tag(mut self, tag: u16, kind: u16, payload: Vec<u8>) -> Self {
        self.0.push((tag, kind, payload));
        self
    }

    fn longs(self, tag: u16, values: &[u32]) -> Self {
        self.tag(tag, KIND_LONG, values.iter().flat_map(|value| value.to_le_bytes()).collect())
    }

    fn floats(self, tag: u16, values: &[f32]) -> Self {
        self.tag(tag, KIND_FLOAT, values.iter().flat_map(|value| value.to_le_bytes()).collect())
    }

    fn table(self, dims_tag: u16, data_tag: u16, dims: [u32; 3], entries: &[[f32; 3]]) -> Self {
        self.longs(dims_tag, &dims).floats(data_tag, entries.as_flattened())
    }

    /// No default tone or black, so output light is what the tables left.
    fn linear(self) -> Self {
        self.floats(TAG_TONE_CURVE, &LINEAR_CURVE).longs(TAG_BLACK_RENDER, &[NO_AUTO_BLACK])
    }

    /// Changes nothing, but having a table turns off the built-in saturation, which makes
    /// this the photo as a table would receive it.
    fn untouched() -> Self {
        Self::default().table(TAG_HUE_SAT_DIMS, TAG_HUE_SAT_1, [1, 2, 1], &[IDENTITY; 2]).linear()
    }

    fn build(mut self) -> Vec<u8> {
        self.0.sort_by_key(|entry| entry.0);
        let mut out = b"IIRC".to_vec();
        out.extend(8u32.to_le_bytes());
        out.extend((self.0.len() as u16).to_le_bytes());
        let mut data_at = out.len() + self.0.len() * 12 + 4;
        let mut data: Vec<u8> = Vec::new();
        for (tag, kind, payload) in &self.0 {
            let size = if *kind == KIND_SHORT { 2 } else { 4 };
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(((payload.len() / size) as u32).to_le_bytes());
            if payload.len() <= 4 {
                let mut field = payload.clone();
                field.resize(4, 0);
                out.extend(field);
            } else {
                out.extend((data_at as u32).to_le_bytes());
                data_at += payload.len();
                data.extend(payload);
            }
        }
        out.extend([0; 4]);
        out.extend(data);
        out
    }
}

fn install(profile: Profile) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), profile.build()).unwrap();
    file
}

/// A DNG-shaped raw, whose profile tags are read from `file` at render time.
fn dng(color: [f32; 3], file: &Path) -> RawImage {
    let mut raw = mosaic(bayer(), 96, 96, move |_, _| color);
    let illuminant = |code| DngIlluminant {
        illuminant: code,
        color_matrix: XYZ_TO_SRGB_CAMERA,
        forward_matrix: None,
        calibration: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };
    raw.color.dng = Some(DngColor {
        illuminants: vec![illuminant(17), illuminant(21)],
        analog_balance: [1.0; 3],
        as_shot_neutral: Some([1.0; 3]),
    });
    raw.metadata.path = file.to_owned();
    raw
}

fn output_light(color: [f32; 3], profile: Profile, recipe: &Recipe) -> [f32; 3] {
    let installed = install(profile);
    let options = ExportOptions { output_space: OutputSpace::ProPhoto, ..ExportOptions::default() };
    let image = export(&dng(color, installed.path()), recipe, &options).expect("export");
    center(&image).map(|value| value.powf(PROPHOTO_GAMMA))
}

fn assert_close(actual: [f32; 3], expected: [f32; 3], tolerance: f32) {
    for channel in 0..3 {
        assert!((actual[channel] - expected[channel]).abs() < tolerance, "{actual:?} vs {expected:?}");
    }
}

fn rgb_to_hsv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let value = r.max(g).max(b);
    let gap = value - r.min(g).min(b);
    if gap <= 0.0 {
        return [0.0, 0.0, value];
    }
    let hue = if r == value {
        let hue = (g - b) / gap;
        if hue < 0.0 { hue + 6.0 } else { hue }
    } else if g == value {
        2.0 + (b - r) / gap
    } else {
        4.0 + (r - g) / gap
    };
    [hue, gap / value, value]
}

fn hsv_to_rgb([hue, saturation, value]: [f32; 3]) -> [f32; 3] {
    let hue = hue.rem_euclid(6.0);
    let fraction = hue - hue.floor();
    let low = value * (1.0 - saturation);
    let falling = value * (1.0 - saturation * fraction);
    let rising = value * (1.0 - saturation * (1.0 - fraction));
    match hue as u32 {
        0 => [value, rising, low],
        1 => [falling, value, low],
        2 => [low, value, rising],
        3 => [low, falling, value],
        4 => [rising, low, value],
        _ => [value, low, falling],
    }
}

fn srgb_encode(value: f32) -> f32 {
    if value <= 0.0031308 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_decode(value: f32) -> f32 {
    if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

/// The DNG table lookup, trilinear with hue wrapping.
fn table_lookup(rgb: [f32; 3], dims: [usize; 3], entries: &[[f32; 3]], encoded: bool) -> [f32; 3] {
    let [hue, saturation, value] = rgb_to_hsv(rgb);
    let [hue_cells, sat_cells, value_cells] = dims;
    let lookup_value = if encoded { srgb_encode(value) } else { value };
    let hue_at = hue / 6.0 * hue_cells as f32;
    let sat_at = saturation * (sat_cells - 1) as f32;
    let value_at = lookup_value.min(1.0) * (value_cells - 1) as f32;
    let (hue_low, sat_low, value_low) = (hue_at.floor(), sat_at.floor(), value_at.floor());
    let (hue_part, sat_part, value_part) = (hue_at - hue_low, sat_at - sat_low, value_at - value_low);
    let mut change = [0.0; 3];
    for (value_step, value_weight) in [(0, 1.0 - value_part), (1, value_part)] {
        for (hue_step, hue_weight) in [(0, 1.0 - hue_part), (1, hue_part)] {
            for (sat_step, sat_weight) in [(0, 1.0 - sat_part), (1, sat_part)] {
                let hue_index = (hue_low as usize + hue_step) % hue_cells;
                let sat_index = (sat_low as usize + sat_step).min(sat_cells - 1);
                let value_index = (value_low as usize + value_step).min(value_cells - 1);
                let index = (value_index * hue_cells + hue_index) * sat_cells + sat_index;
                let weight = value_weight * hue_weight * sat_weight;
                for lane in 0..3 {
                    change[lane] += entries[index][lane] * weight;
                }
            }
        }
    }
    let scaled = lookup_value * change[2];
    hsv_to_rgb([
        hue + change[0] / 60.0,
        (saturation * change[1]).min(1.0),
        if encoded { srgb_decode(scaled) } else { scaled },
    ])
}

/// Every grid point differs, in a way that would show up if an axis were swapped. Zero
/// saturation stays the identity, as the spec requires.
fn varied_table(dims: [usize; 3]) -> Vec<[f32; 3]> {
    let [hue_cells, sat_cells, value_cells] = dims;
    let mut entries = Vec::new();
    for value in 0..value_cells {
        for hue in 0..hue_cells {
            for saturation in 0..sat_cells {
                if saturation == 0 {
                    entries.push(IDENTITY);
                    continue;
                }
                let phase = (hue as f32 / hue_cells as f32) * std::f32::consts::TAU;
                entries.push([
                    25.0 * phase.sin(),
                    1.0 - 0.3 * (phase + value as f32).cos() * saturation as f32 / (sat_cells - 1) as f32,
                    1.0 + 0.2 * (value as f32 + 1.0) / value_cells as f32 * phase.cos(),
                ]);
            }
        }
    }
    entries
}

const COLOURS: [[f32; 3]; 6] = [
    [0.40, 0.10, 0.08],
    [0.12, 0.35, 0.10],
    [0.08, 0.12, 0.45],
    [0.30, 0.28, 0.06],
    [0.06, 0.25, 0.30],
    [0.33, 0.07, 0.30],
];

fn matches_the_spec(dims: [usize; 3], encoded: bool) {
    let entries = varied_table(dims);
    let u32_dims = dims.map(|count| count as u32);
    for color in COLOURS {
        let plain = output_light(color, Profile::untouched(), &Recipe::default());
        let expected = table_lookup(plain, dims, &entries, encoded);
        let profile = Profile::default()
            .table(TAG_HUE_SAT_DIMS, TAG_HUE_SAT_1, u32_dims, &entries)
            .longs(TAG_HUE_SAT_ENCODING, &[u32::from(encoded)])
            .linear();
        assert_close(output_light(color, profile, &Recipe::default()), expected, 0.012);
    }
}

#[test]
fn hue_sat_map_matches_the_spec_in_two_and_a_half_dimensions() {
    matches_the_spec([12, 5, 1], false);
}

#[test]
fn hue_sat_map_matches_the_spec_in_three_dimensions() {
    matches_the_spec([12, 5, 4], false);
}

#[test]
fn srgb_value_encoding_matches_the_spec() {
    matches_the_spec([12, 5, 4], true);
}

#[test]
fn srgb_encoding_changes_the_answer() {
    let dims = [6, 3, 5];
    let entries = varied_table(dims);
    let render_with = |encoded: u32| {
        let profile = Profile::default()
            .table(TAG_LOOK_DIMS, TAG_LOOK_DATA, [6, 3, 5], &entries)
            .longs(TAG_LOOK_ENCODING, &[encoded])
            .linear();
        output_light([0.1, 0.03, 0.02], profile, &Recipe::default())
    };
    let (linear, encoded) = (render_with(0), render_with(1));
    assert!((linear[0] - encoded[0]).abs() > 2e-3, "{linear:?} vs {encoded:?}");
}

#[test]
fn hue_wraps_across_red() {
    let shifts = [-30.0, 0.0, 0.0, 30.0];
    let entries: Vec<[f32; 3]> =
        shifts.iter().flat_map(|&shift| [IDENTITY, IDENTITY, [shift, 1.0, 1.0]]).collect();
    let rose = [0.45, 0.05, 0.2];
    let plain = output_light(rose, Profile::untouched(), &Recipe::default());
    assert!(rgb_to_hsv(plain)[0] > 4.5, "the colour has to sit between the last cell and the first");
    let expected = table_lookup(plain, [4, 3, 1], &entries, false);
    let profile = Profile::default().table(TAG_HUE_SAT_DIMS, TAG_HUE_SAT_1, [4, 3, 1], &entries).linear();
    assert_close(output_light(rose, profile, &Recipe::default()), expected, 0.012);
}

#[test]
fn greys_pass_through_a_table_untouched() {
    let dims = [12, 5, 4];
    let entries = varied_table(dims);
    let profile = || {
        Profile::default()
            .table(TAG_HUE_SAT_DIMS, TAG_HUE_SAT_1, [12, 5, 4], &entries)
            .table(TAG_LOOK_DIMS, TAG_LOOK_DATA, [12, 5, 4], &entries)
            .linear()
    };
    for grey in [0.02, 0.18, 0.6] {
        let light = output_light([grey; 3], profile(), &Recipe::default());
        assert_close(light, [grey; 3], 0.012);
    }
}

#[test]
fn the_hue_sat_map_comes_before_exposure_and_the_look_table_after() {
    let scale_with_value = [
        IDENTITY, [0.0, 1.0, 1.0],
        IDENTITY, [0.0, 1.0, 2.0],
    ];
    let color = [0.1, 0.02, 0.02];
    let brighter = Recipe { basic: unlight_recipe::Basic { exposure: 1.0, ..Default::default() }, ..Recipe::default() };
    let ratio = |profile: fn(&[[f32; 3]; 4]) -> Profile| {
        let dark = output_light(color, profile(&scale_with_value), &Recipe::default());
        let bright = output_light(color, profile(&scale_with_value), &brighter);
        bright[0] / dark[0]
    };
    let hue_sat_ratio = ratio(|entries| {
        Profile::default().table(TAG_HUE_SAT_DIMS, TAG_HUE_SAT_1, [1, 2, 2], entries).linear()
    });
    let look_ratio = ratio(|entries| {
        Profile::default().table(TAG_LOOK_DIMS, TAG_LOOK_DATA, [1, 2, 2], entries).linear()
    });
    assert!((hue_sat_ratio - 2.0).abs() < 0.03, "exposure scales a finished lookup: {hue_sat_ratio}");
    assert!(look_ratio > hue_sat_ratio + 0.03, "the look sees the exposed value: {look_ratio}");
}

#[test]
fn a_profile_tone_curve_replaces_the_default_one() {
    let default_light = output_light(
        [0.18; 3],
        Profile::default().longs(TAG_BLACK_RENDER, &[NO_AUTO_BLACK]),
        &Recipe::default(),
    );
    let linear_light = output_light([0.18; 3], Profile::untouched(), &Recipe::default());
    assert!(default_light[1] > 0.3, "{default_light:?}");
    assert_close(linear_light, [0.18; 3], 0.01);
}

#[test]
fn a_tone_curve_stays_hue_preserving() {
    let curved = Profile::default().floats(TAG_TONE_CURVE, &[0.0, 0.0, 0.25, 0.5, 1.0, 1.0]);
    let color = [0.3, 0.15, 0.05];
    let plain = output_light(color, Profile::untouched(), &Recipe::default());
    let toned = output_light(
        color,
        curved
            .table(TAG_HUE_SAT_DIMS, TAG_HUE_SAT_1, [1, 2, 1], &[IDENTITY; 2])
            .longs(TAG_BLACK_RENDER, &[NO_AUTO_BLACK]),
        &Recipe::default(),
    );
    let hue = |rgb| rgb_to_hsv(rgb)[0];
    assert!((hue(plain) - hue(toned)).abs() < 0.02, "{plain:?} vs {toned:?}");
}

#[test]
fn the_exposure_offset_is_applied() {
    let offset = Profile::default().linear().floats(TAG_EXPOSURE_OFFSET, &[1.0]);
    let doubled = output_light([0.1; 3], offset, &Recipe::default());
    assert_close(doubled, [0.2; 3], 0.01);
}

#[test]
fn hue_sat_maps_follow_the_white_balance() {
    let flat = |shift: f32| vec![[shift, 1.0, 1.0]; 2];
    let profile = || {
        Profile::default()
            .longs(TAG_HUE_SAT_DIMS, &[1, 2, 1])
            .floats(TAG_HUE_SAT_1, flat(0.0).as_flattened())
            .floats(TAG_HUE_SAT_2, flat(40.0).as_flattened())
            .tag(TAG_ILLUMINANT_1, KIND_SHORT, 17u16.to_le_bytes().to_vec())
            .tag(TAG_ILLUMINANT_2, KIND_SHORT, 21u16.to_le_bytes().to_vec())
            .linear()
    };
    let shot_at = |temperature: f32| Recipe {
        white_balance: WhiteBalance::Custom { temperature, tint: 0.0 },
        ..Recipe::default()
    };
    let hue_at = |temperature: f32| {
        let color = [0.3, 0.1, 0.1];
        let shifted = output_light(color, profile(), &shot_at(temperature));
        let unshifted = output_light(color, Profile::untouched(), &shot_at(temperature));
        (rgb_to_hsv(shifted)[0] - rgb_to_hsv(unshifted)[0]) * 60.0
    };
    assert!(hue_at(2856.0).abs() < 4.0, "tungsten uses the first map: {}", hue_at(2856.0));
    assert!((hue_at(6504.0) - 40.0).abs() < 4.0, "daylight the second: {}", hue_at(6504.0));
}

#[test]
fn a_dng_with_no_rendering_keeps_the_default_look() {
    let empty = install(Profile::default());
    let color = [0.3, 0.1, 0.1];
    let embedded = center(&render(&dng(color, empty.path()), &Recipe::default()));
    assert!(chroma(embedded) > 0.05);
    assert!(luma(embedded) > 0.1);
    assert_close(embedded, flat_render(color, &Recipe::default()), 0.01);
}
