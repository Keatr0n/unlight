//! Shows what Auto picks for photos, and measures the result against the default render and
//! the camera's own JPEG.

use crate::{look, output};
use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};
use unlight_color::{delta_e_2000, srgb_to_lab};
use unlight_engine::{AutoTone, Engine, GpuContext, Precision, Session, ViewRequest, ViewTarget};
use unlight_recipe::Recipe;

/// Every this many pixels is compared, which is plenty for a mean.
const STRIDE: usize = 7;

#[derive(Args)]
pub struct AutoArgs {
    files: Vec<PathBuf>,
    /// Width of the renders that get measured.
    #[arg(long, default_value_t = 1280)]
    width: u32,
    /// Write `<name>-default.png` and `<name>-auto.png` here, to look at.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Measures {
    mean_luma: f32,
    white_clip: f32,
    black_clip: f32,
}

fn measure(rgba: &[u8]) -> Measures {
    let pixels = rgba.as_chunks::<4>().0;
    let count = pixels.len().max(1) as f32;
    let share = |test: &dyn Fn(&[u8; 4]) -> bool| pixels.iter().filter(|pixel| test(pixel)).count() as f32 / count;
    Measures {
        mean_luma: pixels.iter().map(|pixel| look::luma(&pixel[..3])).sum::<f32>() / count,
        white_clip: share(&|pixel| pixel[..3].iter().all(|value| *value >= 254)),
        black_clip: share(&|pixel| pixel[..3].iter().all(|value| *value <= 1)),
    }
}

fn difference(rgba: &[u8], reference: &[u8]) -> f64 {
    let color = |pixel: &[u8]| [pixel[0], pixel[1], pixel[2]].map(|value| f32::from(value) / 255.0);
    let pairs = rgba.as_chunks::<4>().0.iter().zip(reference.as_chunks::<3>().0).step_by(STRIDE);
    let (sum, count) = pairs.fold((0.0, 0u64), |(sum, count), (ours, theirs)| {
        (sum + delta_e_2000(srgb_to_lab(color(ours)), srgb_to_lab(color(theirs))), count + 1)
    });
    sum / count.max(1) as f64
}

fn rendered(session: &mut Session, recipe: &Recipe, target: &ViewTarget) -> Result<Vec<u8>> {
    session.render(recipe, &ViewRequest::default(), target, None)?;
    Ok(session.read_view(target)?)
}

fn show(tone: &AutoTone) -> String {
    format!(
        "exp {:+.2} con {:+3.0} hi {:+4.0} sh {:+3.0} wh {:+3.0} bl {:+3.0} vib {:+3.0} sat {:+2.0}",
        tone.exposure, tone.contrast, tone.highlights, tone.shadows, tone.whites, tone.blacks, tone.vibrance, tone.saturation,
    )
}

fn report(args: &AutoArgs, context: &std::sync::Arc<GpuContext>, file: &Path) -> Result<()> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let raw = unlight_raw::load(file)?;
    let engine = Engine::new(context.clone(), Precision::F16)?;
    let plain = Recipe::default();
    let mut session = Session::open(engine, &raw, &plain)?;
    let tone = session.auto_tone(&plain)?;
    let mut edited = plain.clone();
    tone.apply_to(&mut edited.basic);

    let [output_width, output_height] = session.output_size(&plain);
    let height = (u64::from(args.width) * u64::from(output_height) / u64::from(output_width)) as u32;
    let target = session.view_target(args.width, height);
    let before = rendered(&mut session, &plain, &target)?;
    let after = rendered(&mut session, &edited, &target)?;
    if let Some(directory) = &args.output {
        std::fs::create_dir_all(directory)?;
        output::save_rgba8(&directory.join(format!("{name}-default.png")), &before, args.width, height)?;
        output::save_rgba8(&directory.join(format!("{name}-auto.png")), &after, args.width, height)?;
    }

    let reference = look::reference(file, &raw).map(|reference| reference.resized(args.width, height));
    let compare = reference.as_ref().map_or(String::from("no camera JPEG"), |reference| {
        format!(
            "camera {:.3}  ΔE {:5.2} → {:5.2}",
            measure_rgb(&reference.rgb),
            difference(&before, &reference.rgb),
            difference(&after, &reference.rgb),
        )
    });
    let (plain_measures, auto_measures) = (measure(&before), measure(&after));
    let stats = session.tone_stats(&plain)?;
    println!(
        "{name:<34} EV {:>5} med {:6.2} satur {:.2} {:.2} model {:.3} {:.3}  {}  mean {:.3} → {:.3}  white {:.2}% → {:.2}%  black {:.2}% → {:.2}%  {compare}",
        stats.exposure_value.map_or("?".to_owned(), |value| format!("{value:.1}")),
        stats.quantile(0.5),
        stats.saturation,
        stats.vivid,
        stats.mean_output(&AutoTone::default()),
        stats.mean_output(&tone),
        show(&tone),
        plain_measures.mean_luma,
        auto_measures.mean_luma,
        plain_measures.white_clip * 100.0,
        auto_measures.white_clip * 100.0,
        plain_measures.black_clip * 100.0,
        auto_measures.black_clip * 100.0,
    );
    Ok(())
}

fn measure_rgb(rgb: &[u8]) -> f32 {
    let pixels = rgb.as_chunks::<3>().0;
    pixels.iter().map(|pixel| look::luma(pixel)).sum::<f32>() / pixels.len().max(1) as f32
}

pub fn run(args: &AutoArgs) -> Result<()> {
    let context = GpuContext::new()?;
    for file in &args.files {
        if let Err(error) = report(args, &context, file) {
            println!("{}: {error:#}", file.display());
        }
    }
    Ok(())
}
