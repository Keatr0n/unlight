//! A denoiser outside the engine reads the demosaiced photo and hands back a replacement. With
//! nothing changed in between, that round trip must not change the render at all.

mod support;

use std::{env, path::Path};
use support::{bayer, mosaic};
use unlight_engine::{ExportOptions, export, export_with_scene, read_scene};
use unlight_raw::RawImage;
use unlight_recipe::Recipe;

fn no_detail_tools() -> Recipe {
    let mut recipe = Recipe::default();
    recipe.detail.sharpening.amount = 0.0;
    recipe.detail.noise_reduction.luminance = 0.0;
    recipe.detail.noise_reduction.color = 0.0;
    recipe
}

fn largest_difference(raw: &RawImage, recipe: &Recipe) -> f32 {
    let options = ExportOptions::default();
    let plain = export(raw, recipe, &options).unwrap();
    let scene = read_scene(raw, recipe).unwrap();
    assert_eq!(scene.len(), raw.width as usize * raw.height as usize);
    let replaced = export_with_scene(raw, recipe, &options, &scene).unwrap();
    plain.pixels.iter().zip(&replaced.pixels).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
}

#[test]
fn a_scene_put_back_unchanged_renders_identically() {
    let raw = mosaic(bayer(), 320, 160, |x, y| [x / 320.0, y / 160.0, 1.0 - x / 320.0]);
    let difference = largest_difference(&raw, &no_detail_tools());
    assert!(difference < 1e-4, "the render moved by {difference}");
}

#[test]
fn a_real_frame_put_back_unchanged_renders_identically() {
    let Some(directory) = env::var_os("UNLIGHT_SAMPLES") else { return };
    let path = Path::new(&directory).join("sony-a7c-iso12800.ARW");
    if !path.exists() {
        return;
    }
    let raw = unlight_raw::load(&path).unwrap();
    let difference = largest_difference(&raw, &no_detail_tools());
    assert!(difference < 1e-4, "the render moved by {difference}");
}
