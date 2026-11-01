use anyhow::{Result, bail};
use clap::Args;
use std::{path::PathBuf, sync::Arc, time::{Duration, Instant}};
use unlight_engine::{
    Engine, GpuContext, GpuProfiler, OutputSpace, Precision, Proof, ProofIntent, ProofTarget, Session, ViewRequest,
};
use unlight_output::bake_proof;
use unlight_recipe::{BrushDab, GradeWheel, LocalAdjustments, Mask, MaskComponent, MaskShape, Recipe};

/// Dabs in each brush stroke of `--masks`, as many as a long, careful stroke lays down.
const STROKE_DABS: usize = 4000;

#[derive(Args)]
pub struct BenchArgs {
    file: PathBuf,
    #[arg(long, default_value_t = 2560)]
    width: u32,
    #[arg(long, default_value_t = 1440)]
    height: u32,
    #[arg(long, default_value_t = 600)]
    frames: usize,
    /// Run for this long first so the numbers reflect a heat-soaked chip.
    #[arg(long, default_value_t = 0)]
    soak_seconds: u64,
    /// Turn on every tool, to measure the slowest slider path instead of the fastest.
    #[arg(long)]
    all_tools: bool,
    /// Drag the noise reduction sliders instead of exposure, which also re-solves denoising.
    #[arg(long)]
    noise_slider: bool,
    /// Measure with this many masks, against the same frames without them.
    #[arg(long, default_value_t = 0)]
    masks: usize,
    /// Measure soft proofing against the same frames without it: `srgb`, `p3`, `adobe`, or the
    /// path of an ICC profile.
    #[arg(long)]
    proof: Option<String>,
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * fraction) as usize]
}

/// A slider sweeping back and forth, as a drag does.
fn slider_position(frame: usize) -> f32 {
    const PERIOD: usize = 120;
    let phase = (frame % PERIOD) as f32 / PERIOD as f32;
    (1.0 - (phase * 2.0 - 1.0).abs()) * 4.0 - 2.0
}

fn all_tools_recipe() -> Recipe {
    let mut recipe = Recipe::default();
    let basic = &mut recipe.basic;
    (basic.contrast, basic.highlights, basic.shadows) = (20.0, -40.0, 35.0);
    (basic.whites, basic.blacks, basic.clarity) = (15.0, -10.0, 25.0);
    (basic.dehaze, basic.vibrance, basic.saturation) = (15.0, 20.0, 5.0);
    recipe.tone_curve.parametric.lights = 15.0;
    recipe.color_mixer.saturation[3] = 20.0;
    recipe.color_mixer.hue[1] = -10.0;
    recipe.color_grading.shadows = GradeWheel { hue: 220.0, saturation: 20.0, luminance: 0.0 };
    recipe.effects.vignette.amount = -20.0;
    recipe.effects.grain.amount = 15.0;
    recipe.lens.profile_enabled = true;
    recipe.lens.remove_chromatic_aberration = true;
    recipe.lens.manual_distortion = 5.0;
    recipe.lens.manual_vignette = 15.0;
    recipe.lens.defringe.purple_amount = 50.0;
    recipe.lens.defringe.green_amount = 50.0;
    recipe
}

/// One of each kind of mask in turn, in the proportions a heavily edited photo has them. These
/// use the adjustments that only read a field. Texture, sharpness and noise switch on extra work
/// of their own, which a mask doesn't add over a global slider.
fn bench_mask(index: usize) -> Mask {
    let t = index as f32 / 16.0;
    let shape = match index % 8 {
        0 | 4 => MaskShape::Linear { start: [t, 0.0], end: [1.0 - t, 1.0] },
        1 | 5 => MaskShape::Radial {
            center: [0.2 + 0.6 * t, 0.8 - 0.6 * t],
            radii: [0.3, 0.2],
            angle: 40.0 * t,
            feather: 0.6,
            inverted: index % 8 == 5,
        },
        2 | 6 => MaskShape::Brush {
            dabs: (0..STROKE_DABS)
                .map(|dab| {
                    let along = dab as f32 / STROKE_DABS as f32;
                    let angle = along * 12.0 + t;
                    let reach = 0.1 + 0.3 * along;
                    BrushDab {
                        position: [0.5 + reach * angle.cos(), 0.5 + reach * angle.sin()],
                        radius: 0.02,
                        feather: 0.6,
                        flow: 0.5,
                        erase: false,
                        auto_mask: false,
                    }
                })
                .collect(),
        },
        3 => MaskShape::LuminanceRange { low: 0.1 + 0.3 * t, high: 0.7, smoothness: 0.3 },
        _ => MaskShape::ColorRange { samples: vec![[0.2 + t * 0.4, 0.2, 0.1], [0.1, 0.15, 0.3]], tolerance: 0.5 },
    };
    let sign = if index.is_multiple_of(2) { 1.0 } else { -1.0 };
    Mask {
        name: format!("Mask {index}"),
        components: vec![MaskComponent { shape, subtract: false }],
        adjustments: LocalAdjustments {
            exposure: 0.3 * sign,
            contrast: 15.0 * sign,
            highlights: -20.0,
            shadows: 20.0,
            clarity: 15.0 * sign,
            hue: 5.0 * sign,
            saturation: 10.0 * sign,
            temperature: 10.0 * sign,
            tint: 5.0 * sign,
            ..LocalAdjustments::default()
        },
        ..Mask::default()
    }
}

fn proof_target(name: &str) -> Result<ProofTarget> {
    Ok(match name {
        "srgb" => ProofTarget::Space(OutputSpace::Srgb),
        "p3" => ProofTarget::Space(OutputSpace::DisplayP3),
        "adobe" => ProofTarget::Space(OutputSpace::AdobeRgb),
        path => {
            let start = Instant::now();
            let table = bake_proof(&std::fs::read(path)?, ProofIntent::Relative)?;
            println!("baked {path} in {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
            ProofTarget::Table(Arc::new(table))
        }
    })
}

#[derive(Default)]
struct Measured {
    gpu_ms: Vec<f64>,
    wall_ms: Vec<f64>,
}

impl Measured {
    fn report(mut self, heading: &str) -> (f64, f64) {
        self.gpu_ms.sort_by(f64::total_cmp);
        self.wall_ms.sort_by(f64::total_cmp);
        let line = |name: &str, values: &[f64]| {
            println!(
                "{name}  p50 {:.2} ms  p95 {:.2} ms  p99 {:.2} ms  max {:.2} ms",
                percentile(values, 0.5),
                percentile(values, 0.95),
                percentile(values, 0.99),
                values[values.len() - 1],
            );
        };
        println!("{heading}");
        line("gpu pass   ", &self.gpu_ms);
        line("submit→done", &self.wall_ms);
        (percentile(&self.gpu_ms, 0.5), percentile(&self.gpu_ms, 0.95))
    }
}

fn measure(
    context: &GpuContext,
    frames: usize,
    mut render: impl FnMut(usize, &mut GpuProfiler) -> Result<()>,
) -> Result<Measured> {
    let Some(mut profiler) = GpuProfiler::new(context) else {
        bail!("this adapter has no timestamp queries");
    };
    let mut measured = Measured::default();
    for index in 0..frames {
        let start = Instant::now();
        render(index, &mut profiler)?;
        measured.wall_ms.push(start.elapsed().as_secs_f64() * 1e3);
        measured.gpu_ms.push(profiler.collect(context)?.total_ms());
    }
    Ok(measured)
}

pub fn run(args: &BenchArgs) -> Result<()> {
    let raw = unlight_raw::load(&args.file)?;
    let context = GpuContext::new()?;
    println!("adapter      {}", context.info.name);
    println!("photo        {} {:.1} MP", raw.metadata.camera(), f64::from(raw.width * raw.height) / 1e6);

    let engine = Engine::new(context.clone(), Precision::F16)?;
    let load_start = Instant::now();
    let mut session = Session::open(engine, &raw, &Recipe::default())?;
    println!("to editable  {:.1} ms", load_start.elapsed().as_secs_f64() * 1e3);

    let aspect = f64::from(raw.width) / f64::from(raw.height);
    let width = args.width.min((f64::from(args.height) * aspect) as u32);
    let height = (f64::from(width) / aspect) as u32;
    let target = session.view_target(width, height);
    println!("viewport     {width}×{height}");
    let base = if args.all_tools { all_tools_recipe() } else { Recipe::default() };

    let view = ViewRequest::default();
    let slide = |base: &Recipe, frame: usize| {
        let mut recipe = base.clone();
        if args.noise_slider {
            let noise = &mut recipe.detail.noise_reduction;
            noise.color = 40.0 + slider_position(frame) * 15.0;
            noise.luminance = 40.0 + slider_position(frame) * 15.0;
        } else {
            recipe.basic.exposure = slider_position(frame);
        }
        recipe
    };

    let soak_until = Instant::now() + Duration::from_secs(args.soak_seconds);
    let mut frame = 0;
    while Instant::now() < soak_until {
        session.render(&slide(&base, frame), &view, &target, None)?;
        frame += 1;
    }

    let plain = measure(&context, args.frames, |index, profiler| {
        Ok(session.render(&slide(&base, frame + index), &view, &target, Some(profiler))?)
    })?;
    if let Some(name) = &args.proof {
        plain.report("slider, proofing off");
        let against = proof_target(name)?;
        let proofed = ViewRequest { proof: Some(Proof { target: against, gamut_warning: true }), ..ViewRequest::default() };
        let first = Instant::now();
        session.render(&slide(&base, frame), &proofed, &target, None)?;
        println!("first proof  {:.1} ms, which compiles its shader", first.elapsed().as_secs_f64() * 1e3);
        let Some(mut profiler) = GpuProfiler::new(&context) else {
            bail!("this adapter has no timestamp queries");
        };
        let mut finish_pass = |recipe: &Recipe, request: &ViewRequest| -> Result<f64> {
            session.render(recipe, request, &target, Some(&mut profiler))?;
            let timings = profiler.collect(&context)?;
            Ok(timings.passes.iter().filter(|(pass, _)| *pass == "finish").map(|(_, ms)| ms).sum())
        };
        let mut paired = |label: &str, plain: &ViewRequest, other: &ViewRequest| -> Result<()> {
            let mut differences = Vec::with_capacity(args.frames);
            let mut baseline = Vec::with_capacity(args.frames);
            for index in 0..args.frames {
                let recipe = slide(&base, frame + index);
                // Which goes first swaps each time, so a warm cache favours neither.
                let (base_ms, other_ms) = if index % 2 == 0 {
                    let base_ms = finish_pass(&recipe, plain)?;
                    (base_ms, finish_pass(&recipe, other)?)
                } else {
                    let other_ms = finish_pass(&recipe, other)?;
                    (finish_pass(&recipe, plain)?, other_ms)
                };
                differences.push(other_ms - base_ms);
                baseline.push(base_ms);
            }
            differences.sort_by(f64::total_cmp);
            baseline.sort_by(f64::total_cmp);
            println!(
                "{label:<26} finish pass {:.3} ms, paired difference {:+.3} ms (middle half {:+.3} to {:+.3})",
                percentile(&baseline, 0.5),
                percentile(&differences, 0.5),
                percentile(&differences, 0.25),
                percentile(&differences, 0.75),
            );
            Ok(())
        };
        paired("control, off against off", &view, &view)?;
        paired("proofing on against off", &view, &proofed)?;
        return Ok(());
    }
    if args.masks == 0 {
        plain.report("slider");
        return Ok(());
    }
    let (plain_p50, plain_p95) = plain.report("slider, no masks");

    let mut masked = base.clone();
    masked.masks = (0..args.masks).map(bench_mask).collect();
    let build_start = Instant::now();
    session.render(&masked, &view, &target, None)?;
    println!("first build  {:.1} ms for {} masks", build_start.elapsed().as_secs_f64() * 1e3, args.masks);

    let rebuilds = session.mask_rebuilds();
    let with_masks = measure(&context, args.frames, |index, profiler| {
        Ok(session.render(&slide(&masked, frame + index), &view, &target, Some(profiler))?)
    })?;
    println!("field rebuilds during that slider: {}", session.mask_rebuilds() - rebuilds);
    let (masked_p50, masked_p95) = with_masks.report(&format!("slider, {} masks", args.masks));
    println!("masks add    p50 {:+.2} ms  p95 {:+.2} ms", masked_p50 - plain_p50, masked_p95 - plain_p95);

    // Taken in turn, so that anything else on the GPU lands on both sides of the difference.
    let mut paired = Vec::with_capacity(args.frames);
    let mut last = 0.0;
    let turns = measure(&context, args.frames * 2, |index, profiler| {
        let recipe = if index.is_multiple_of(2) { &base } else { &masked };
        session.render(&slide(recipe, frame + index / 2), &view, &target, Some(profiler))?;
        Ok(())
    });
    let turns = turns?;
    for (index, frame_ms) in turns.gpu_ms.iter().enumerate() {
        if !index.is_multiple_of(2) {
            paired.push(frame_ms - last);
        }
        last = *frame_ms;
    }
    paired.sort_by(f64::total_cmp);
    println!(
        "paired frames  masks add p50 {:+.2} ms  (p10 {:+.2}, p90 {:+.2})",
        percentile(&paired, 0.5),
        percentile(&paired, 0.1),
        percentile(&paired, 0.9),
    );

    let dragged = measure(&context, args.frames, |index, profiler| {
        masked.masks[0].adjustments.exposure = slider_position(index);
        Ok(session.render(&masked, &view, &target, Some(profiler))?)
    })?;
    dragged.report("dragging a mask's slider, which rebuilds the fields");

    let last = masked.masks.len() - 1;
    let mut painted = masked.clone();
    let stroke = (0..args.frames).map(|index| {
        let along = index as f32 / args.frames as f32;
        BrushDab {
            position: [0.1 + 0.8 * along, 0.5 + 0.3 * (along * 20.0).sin()],
            radius: 0.03,
            feather: 0.6,
            flow: 0.5,
            erase: false,
            auto_mask: false,
        }
    });
    painted.masks[last].components.push(MaskComponent { shape: MaskShape::Brush { dabs: Vec::new() }, subtract: false });
    let mut dabs = Vec::new();
    let mut stroke = stroke;
    let painting = measure(&context, args.frames, |_, profiler| {
        dabs.extend(stroke.next());
        if let Some(MaskComponent { shape: MaskShape::Brush { dabs: live }, .. }) = painted.masks[last].components.last_mut() {
            live.clone_from(&dabs);
        }
        Ok(session.render(&painted, &view, &target, Some(profiler))?)
    })?;
    painting.report("painting one dab a frame");
    Ok(())
}
