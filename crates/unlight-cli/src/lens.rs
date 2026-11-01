//! Lens correction reports: what a photo resolves to, and how straight its lines come out.

use anyhow::Result;
use std::path::Path;
use unlight_engine::{ExportOptions, LumaImage, PhotoLens, Straightness, export, measure_long_lines};
use unlight_recipe::Recipe;

/// Lines shorter than this share of the image's long edge say little about distortion, whose
/// bow grows with the square of a line's length.
const MIN_LINE_SHARE: f32 = 0.15;
const RADII: [f32; 5] = [0.2, 0.4, 0.6, 0.8, 1.0];

fn describe(source: Option<unlight_engine::LensSource>) -> &'static str {
    source.map_or("none", |source| source.label())
}

pub fn report(file: &Path) -> Result<()> {
    let raw = unlight_raw::load(file)?;
    let meta = &raw.metadata;
    println!("camera      {}", meta.camera());
    println!("lens        {} at {} mm f/{}", meta.lens, meta.focal_length, meta.aperture);
    let lens = PhotoLens::new(&raw);
    let profile = match lens.resolve() {
        Ok(profile) => profile,
        Err(error) => {
            println!("no correction: {error}");
            return Ok(());
        }
    };
    println!("distortion  {}", describe(profile.distortion));
    println!("chromatic   {}", describe(profile.chromatic));
    println!("vignette    {}", describe(profile.vignette));
    if let Some(name) = &profile.lens {
        println!("database    {name}");
    }
    println!("axis        {:?}", profile.center);
    let corner = (f64::from(raw.width).hypot(f64::from(raw.height)) * 0.5 / f64::from(raw.height)) as f32;
    println!("radius   scale      red        blue       stops");
    for share in RADII {
        let radius = corner * share;
        let [red, blue] = profile.chromatic_scale(radius);
        println!(
            "{share:.1}      {:.5}    {red:.5}    {blue:.5}    {:+.3}",
            profile.distortion_scale(radius),
            profile.vignette_stops(radius),
        );
    }
    Ok(())
}

fn to_luma(pixels: &[f32], width: u32, height: u32) -> LumaImage {
    let values = pixels
        .as_chunks::<3>()
        .0
        .iter()
        .map(|rgb| (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]) * 255.0)
        .collect();
    LumaImage { width: width as usize, height: height as usize, values }
}

fn percentile(sorted: &[f32], share: f32) -> f32 {
    sorted[((sorted.len() - 1) as f32 * share) as usize]
}

/// Renders a photo at full size and reports how far its long lines stray from straight.
pub fn measure_straightness(file: &Path, recipe: &Recipe, count: usize) -> Result<()> {
    let raw = unlight_raw::load(file)?;
    let image = export(&raw, recipe, &ExportOptions::default())?;
    let luma = to_luma(&image.pixels, image.width, image.height);
    let shortest = image.width.max(image.height) as f32 * MIN_LINE_SHARE;
    let mut lines = measure_long_lines(&luma, shortest);
    lines.truncate(count);
    if lines.is_empty() {
        println!("no lines of {shortest:.0} px or longer to measure");
        return Ok(());
    }
    for line in &lines {
        println!(
            "line {:7.0} px   rms {:5.2} px   bow {:5.2} px   sagitta {:+6.2} px",
            line.length, line.rms, line.bow, line.sagitta
        );
    }
    let sorted = |pick: fn(&Straightness) -> f32| {
        let mut values: Vec<f32> = lines.iter().map(pick).collect();
        values.sort_by(f32::total_cmp);
        values
    };
    let (rms, bow) = (sorted(|line| line.rms), sorted(|line| line.bow));
    println!(
        "{} lines: rms median {:.2} px max {:.2} px, bow median {:.2} px max {:.2} px",
        lines.len(),
        percentile(&rms, 0.5),
        percentile(&rms, 1.0),
        percentile(&bow, 0.5),
        percentile(&bow, 1.0),
    );
    Ok(())
}
