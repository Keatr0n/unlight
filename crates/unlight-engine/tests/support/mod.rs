//! Synthetic raws for GPU tests. Scenes are given in linear sRGB-primaries camera space, with
//! an sRGB camera matrix, so an expected colour is easy to reason about.

#![allow(dead_code)]

use unlight_engine::{ExportOptions, ExportedImage, export};
use unlight_raw::{Cfa, ColorData, Metadata, Opcodes, RawImage};
use unlight_recipe::Recipe;

const XYZ_TO_SRGB_CAMERA: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.969266, 1.8760108, 0.041556],
    [0.0556434, -0.2040259, 1.0572252],
];

pub const BLACK: f32 = 64.0;
pub const WHITE: f32 = 16383.0;

pub fn bayer() -> Cfa {
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

pub fn mosaic(
    cfa: Cfa,
    width: u32,
    height: u32,
    scene: impl Fn(f32, f32) -> [f32; 3],
) -> RawImage {
    let samples = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let rgb = scene(x as f32, y as f32);
            let value = rgb[cfa.color_at(x, y) as usize];
            (BLACK + value.clamp(0.0, 1.0) * (WHITE - BLACK)).round() as u16
        })
        .collect();
    RawImage {
        width,
        height,
        samples,
        cfa,
        black: [[BLACK; 6]; 6],
        white: WHITE,
        color: ColorData {
            xyz_to_camera: XYZ_TO_SRGB_CAMERA,
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

pub fn flat(rgb: [f32; 3]) -> RawImage {
    mosaic(bayer(), 96, 96, move |_, _| rgb)
}

pub fn render(raw: &RawImage, recipe: &Recipe) -> ExportedImage {
    export(raw, recipe, &ExportOptions::default()).expect("export")
}

pub fn pixel(image: &ExportedImage, x: u32, y: u32) -> [f32; 3] {
    let index = (y * image.width + x) as usize * 3;
    [image.pixels[index], image.pixels[index + 1], image.pixels[index + 2]]
}

pub fn center(image: &ExportedImage) -> [f32; 3] {
    pixel(image, image.width / 2, image.height / 2)
}

pub fn luma([r, g, b]: [f32; 3]) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

pub fn chroma([r, g, b]: [f32; 3]) -> f32 {
    r.max(g).max(b) - r.min(g).min(b)
}

/// Renders a flat colour under a recipe and returns the result.
pub fn flat_render(rgb: [f32; 3], recipe: &Recipe) -> [f32; 3] {
    center(&render(&flat(rgb), recipe))
}
