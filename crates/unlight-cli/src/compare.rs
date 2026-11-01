//! Prints the preview-versus-export colour difference for each tool on a real photo.

use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};
use unlight_engine::{PreviewPair, preview_difference, preview_pair};
use unlight_recipe::{
    BrushDab, GradeWheel, HealMode, HealSpot, LocalAdjustments, Mask, MaskComponent, MaskShape, Recipe, RedEye,
};

#[derive(Args)]
pub struct CompareArgs {
    file: PathBuf,
    /// Width of the preview, as on a 1440p display.
    #[arg(long, default_value_t = 2160)]
    width: u32,
    /// Only the tools whose name contains this.
    #[arg(long)]
    only: Option<String>,
    /// Also write `<prefix>-preview.png`, `<prefix>-export.png` and an absolute difference
    /// `<prefix>-difference.png`, amplified eight times, for the first matching tool.
    #[arg(long)]
    save: Option<PathBuf>,
}

type Edit = fn(&mut Recipe);

fn add_mask(recipe: &mut Recipe, shape: MaskShape, adjustments: LocalAdjustments) {
    let components = vec![MaskComponent { shape, subtract: false }];
    recipe.masks.push(Mask { components, adjustments, ..Mask::default() });
}

fn exposure(stops: f32) -> LocalAdjustments {
    LocalAdjustments { exposure: stops, ..LocalAdjustments::default() }
}

fn soft_sky() -> MaskShape {
    MaskShape::Linear { start: [0.5, 0.1], end: [0.5, 0.6] }
}

fn soft_circle() -> MaskShape {
    MaskShape::Radial { center: [0.55, 0.5], radii: [0.25, 0.18], angle: 20.0, feather: 0.7, inverted: false }
}

fn wavy_stroke() -> MaskShape {
    let dab = |step: usize| {
        let along = step as f32 / 300.0;
        BrushDab {
            position: [0.15 + 0.7 * along, 0.5 + 0.2 * (along * 12.0).sin()],
            radius: 0.04,
            feather: 0.6,
            flow: 0.4,
            erase: false,
            auto_mask: false,
        }
    };
    MaskShape::Brush { dabs: (0..300).map(dab).collect() }
}

fn dust_spots(recipe: &mut Recipe) {
    for step in 0..6 {
        let along = step as f32 / 6.0;
        recipe.healing.push(HealSpot {
            mode: if step % 2 == 0 { HealMode::Heal } else { HealMode::Clone },
            target: [0.15 + 0.7 * along, 0.3 + 0.4 * along],
            source: [0.22 + 0.7 * along, 0.3 + 0.4 * along],
            radius: 0.012,
            feather: 0.5,
            opacity: 1.0,
        });
    }
}

const TOOLS: &[(&str, Edit)] = &[
    ("default", |_| {}),
    ("no noise reduction", |r| r.detail.noise_reduction.color = 0.0),
    ("exposure +1", |r| r.basic.exposure = 1.0),
    ("contrast +50", |r| r.basic.contrast = 50.0),
    ("highlights -80", |r| r.basic.highlights = -80.0),
    ("shadows +80", |r| r.basic.shadows = 80.0),
    ("whites +40", |r| r.basic.whites = 40.0),
    ("blacks -40", |r| r.basic.blacks = -40.0),
    ("clarity +60", |r| r.basic.clarity = 60.0),
    ("texture +60", |r| r.basic.texture = 60.0),
    ("dehaze +60", |r| r.basic.dehaze = 60.0),
    ("vibrance +60", |r| r.basic.vibrance = 60.0),
    ("saturation +40", |r| r.basic.saturation = 40.0),
    ("hsl green +60", |r| r.color_mixer.saturation[3] = 60.0),
    ("grade shadows", |r| r.color_grading.shadows = GradeWheel { hue: 220.0, saturation: 40.0, luminance: 0.0 }),
    ("sharpening 100", |r| r.detail.sharpening.amount = 100.0),
    ("sharpening 150 r3", |r| {
        r.detail.sharpening.amount = 150.0;
        r.detail.sharpening.radius = 3.0;
    }),
    ("luminance nr 50", |r| r.detail.noise_reduction.luminance = 50.0),
    ("luminance nr 100", |r| r.detail.noise_reduction.luminance = 100.0),
    ("color nr 100", |r| r.detail.noise_reduction.color = 100.0),
    ("vignette -50", |r| r.effects.vignette.amount = -50.0),
    ("grain 40", |r| r.effects.grain.amount = 40.0),
    ("mask gradient", |r| add_mask(r, soft_sky(), exposure(-1.5))),
    ("mask radial", |r| add_mask(r, soft_circle(), exposure(1.2))),
    ("mask brush", |r| add_mask(r, wavy_stroke(), exposure(1.0))),
    ("mask luminance", |r| {
        let shape = MaskShape::LuminanceRange { low: 0.55, high: 1.0, smoothness: 0.3 };
        add_mask(r, shape, exposure(-0.8));
    }),
    ("mask colour", |r| {
        let shape = MaskShape::ColorRange { samples: vec![[0.25, 0.3, 0.45]], tolerance: 0.6 };
        add_mask(r, shape, LocalAdjustments { saturation: 40.0, hue: 15.0, ..LocalAdjustments::default() });
    }),
    ("mask clarity texture", |r| {
        let adjustments = LocalAdjustments { clarity: 50.0, texture: 50.0, ..LocalAdjustments::default() };
        add_mask(r, soft_circle(), adjustments);
    }),
    ("mask sharpness", |r| {
        add_mask(r, soft_circle(), LocalAdjustments { sharpness: 80.0, ..LocalAdjustments::default() });
    }),
    ("mask noise", |r| {
        add_mask(r, soft_circle(), LocalAdjustments { noise: 80.0, ..LocalAdjustments::default() });
    }),
    ("mask four together", |r| {
        add_mask(r, soft_sky(), exposure(-1.0));
        add_mask(r, soft_circle(), LocalAdjustments { shadows: 40.0, clarity: 30.0, ..LocalAdjustments::default() });
        add_mask(r, wavy_stroke(), LocalAdjustments { temperature: 30.0, ..LocalAdjustments::default() });
        let shape = MaskShape::LuminanceRange { low: 0.0, high: 0.4, smoothness: 0.3 };
        add_mask(r, shape, exposure(0.5));
    }),
    ("repair heal and clone", dust_spots),
    ("repair red eye", |r| {
        r.red_eye.push(RedEye { center: [0.5, 0.5], radius: 0.02, pupil_size: 0.8, darken: 0.5 });
    }),
];

fn save_png(path: &Path, width: u32, height: u32, pixels: impl Iterator<Item = f32>) -> Result<()> {
    let bytes: Vec<u8> = pixels.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.write_header()?.write_image_data(&bytes)?;
    Ok(())
}

fn save_pair(prefix: &Path, pair: &PreviewPair) -> Result<()> {
    const AMPLIFY: f32 = 8.0;
    let name = |suffix: &str| prefix.with_file_name(format!("{}-{suffix}.png", prefix.display().to_string().rsplit('/').next().unwrap_or("compare")));
    save_png(&name("preview"), pair.width, pair.height, pair.shown.iter().copied())?;
    save_png(&name("export"), pair.width, pair.height, pair.exported.iter().copied())?;
    let difference = pair.shown.iter().zip(&pair.exported).map(|(a, b)| (a - b).abs() * AMPLIFY);
    save_png(&name("difference"), pair.width, pair.height, difference)
}

pub fn run(args: &CompareArgs) -> Result<()> {
    let raw = unlight_raw::load(&args.file)?;
    println!("{} at {} px wide: ΔE2000", raw.metadata.camera(), args.width);
    let mut wanted = TOOLS
        .iter()
        .filter(|(name, _)| args.only.as_ref().is_none_or(|only| name.contains(only.as_str())))
        .peekable();
    if let (Some(prefix), Some((_, edit))) = (&args.save, wanted.peek()) {
        let mut recipe = Recipe::default();
        edit(&mut recipe);
        save_pair(prefix, &preview_pair(&raw, &recipe, args.width)?)?;
    }
    for (name, edit) in wanted {
        let mut recipe = Recipe::default();
        edit(&mut recipe);
        let difference = preview_difference(&raw, &recipe, args.width)?;
        println!(
            "{name:<20} mean {:5.2}  p95 {:5.2}  max {:6.2}",
            difference.mean, difference.p95, difference.max
        );
    }
    Ok(())
}
