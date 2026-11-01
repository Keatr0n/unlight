#[cfg(feature = "ai")]
mod ai;
mod auto;
mod bench;
mod deliver;
mod compare;
mod lens;
mod look;
mod merge;
mod output;
mod probe;
mod profile;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use unlight_engine::{
    Engine, ExportOptions, GpuContext, Precision, Session, ViewRequest, export, stall_device,
};
use unlight_recipe::Recipe;

#[derive(Parser)]
#[command(name = "unlight-cli", about = "Render and benchmark raw photos from the command line")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum PrecisionArg {
    F16,
    F32,
}

impl From<PrecisionArg> for Precision {
    fn from(value: PrecisionArg) -> Self {
        match value {
            PrecisionArg::F16 => Self::F16,
            PrecisionArg::F32 => Self::F32,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Print what the decoder sees in a raw file.
    Info { file: PathBuf },
    /// Merge photos into an HDR or a panorama that stays fully editable.
    #[command(subcommand)]
    Merge(merge::MergeCommand),
    /// Render a photo at full resolution to .tiff (16-bit) or .png (8-bit).
    Render {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Edit recipe as JSON.
        #[arg(long)]
        recipe: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "f32")]
        precision: PrecisionArg,
        /// Keep only this region of the result, as X,Y,WIDTH,HEIGHT.
        #[arg(long, value_delimiter = ',')]
        crop: Option<Vec<u32>>,
    },
    /// Deliver finished images using an export preset.
    Export {
        files: Vec<PathBuf>,
        /// Destination directory.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        #[arg(long, default_value = "Full-size JPEG")]
        preset: String,
        #[arg(long)]
        recipe: Option<PathBuf>,
        #[arg(long)]
        list_presets: bool,
    },
    /// Render the fit-to-screen preview path to a PNG.
    Preview {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long)]
        recipe: Option<PathBuf>,
    },
    /// Measure interactive latency with per-pass GPU timestamps.
    Bench(bench::BenchArgs),
    /// Show which lens corrections a photo resolves to.
    Lens { file: PathBuf },
    /// Render a photo and measure how straight its long lines are.
    Straightness {
        file: PathBuf,
        #[arg(long)]
        recipe: Option<PathBuf>,
        /// How many of the longest lines to measure.
        #[arg(long, default_value_t = 20)]
        lines: usize,
    },
    /// Measure the colour difference between 16-bit and 32-bit processing.
    ComparePrecision { file: PathBuf },
    /// Measure how far the preview strays from a scaled-down export, for each tool.
    ComparePreview(compare::CompareArgs),
    /// Show what range masks and healing would pick at a point on a photo.
    Probe(probe::ProbeArgs),
    /// Compare the default render with each camera's embedded JPEG.
    CompareLook { files: Vec<PathBuf> },
    /// Show what Auto tone picks, and how it compares with the default and the camera's JPEG.
    AutoTone(auto::AutoArgs),
    /// Show which camera profile a photo resolves to.
    Profile {
        file: PathBuf,
        #[arg(long)]
        recipe: Option<PathBuf>,
    },
    /// Optional local AI tools: model downloads and masks.
    #[cfg(feature = "ai")]
    #[command(subcommand)]
    Ai(ai::AiCommand),
    /// Hang the GPU on purpose to prove the editor survives. This makes the kernel reset the
    /// GPU, which can crash other apps or your desktop session, so save your work first.
    HangTest {
        file: PathBuf,
        #[arg(long)]
        i_understand_this_may_crash_my_desktop: bool,
    },
}

fn load_recipe(path: Option<&Path>) -> Result<Recipe> {
    let Some(path) = path else {
        return Ok(Recipe::default());
    };
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(feature = "ai")]
fn mask_rasters() -> Option<&'static dyn unlight_engine::MaskRasterSource> {
    unlight_ai::stored_masks()
}

#[cfg(not(feature = "ai"))]
fn mask_rasters() -> Option<&'static dyn unlight_engine::MaskRasterSource> {
    None
}

fn info(file: &Path) -> Result<()> {
    let raw = unlight_raw::load(file)?;
    let meta = &raw.metadata;
    println!("camera       {}", meta.camera());
    println!("lens         {}", meta.lens);
    println!("size         {}×{} ({:.1} MP)", raw.width, raw.height, f64::from(raw.width * raw.height) / 1e6);
    let sensor = match &raw.metadata.merge {
        Some(merge) => format!("{} merged from {} photos", merge.kind.label(), merge.sources.len()),
        None if raw.cfa.is_xtrans() => "X-Trans".to_owned(),
        None => "Bayer".to_owned(),
    };
    println!("sensor       {sensor}");
    println!("white        {}", raw.white);
    println!("as shot      {:?}", raw.color.as_shot_multipliers);
    println!("baseline     {:+.2} EV", raw.color.baseline_exposure);
    println!("iso          {}", meta.iso);
    let opcodes = raw.opcodes.list1.len() + raw.opcodes.list2.len() + raw.opcodes.list3.len();
    println!("dng opcodes  {opcodes}");
    Ok(())
}

fn render(
    file: &Path,
    output: &Path,
    recipe: Option<&Path>,
    precision: Precision,
    crop: Option<&[u32]>,
) -> Result<()> {
    let recipe = load_recipe(recipe)?;
    let decode_start = Instant::now();
    let raw = unlight_raw::load(file)?;
    let decode = decode_start.elapsed();

    let render_start = Instant::now();
    let options = ExportOptions { precision, mask_rasters: mask_rasters(), ..ExportOptions::default() };
    let image = export(&raw, &recipe, &options)?;
    let render = render_start.elapsed();

    let (tiles, device_losses) = (image.tiles, image.device_losses);
    let (pixels, width, height) =
        output::orient(image.pixels, image.width, image.height, raw.metadata.orientation);
    let (pixels, width, height) = match crop {
        Some(&[x, y, crop_width, crop_height]) => {
            (output::crop(&pixels, width, [x, y, crop_width, crop_height]), crop_width, crop_height)
        }
        _ => (pixels, width, height),
    };
    output::save(output, &pixels, width, height)?;
    println!(
        "{} {width}×{height}: decode {:.2} s, render {:.2} s in {tiles} tiles, {device_losses} device losses",
        raw.metadata.camera(),
        decode.as_secs_f64(),
        render.as_secs_f64(),
    );
    Ok(())
}

fn preview(file: &Path, output_path: &Path, width: u32, recipe: Option<&Path>) -> Result<()> {
    let recipe = load_recipe(recipe)?;
    let raw = unlight_raw::load(file)?;
    let engine = Engine::new(GpuContext::new()?, Precision::F16)?;
    let mut session = Session::open(engine, &raw, &recipe)?;
    session.set_mask_rasters(mask_rasters());
    let [output_width, output_height] = session.output_size(&recipe);
    let height = (u64::from(width) * u64::from(output_height) / u64::from(output_width)) as u32;
    let target = session.view_target(width, height);
    session.render(&recipe, &ViewRequest::default(), &target, None)?;
    let rgba = session.read_view(&target)?;
    output::save_rgba8(output_path, &rgba, width, height)
}

fn compare_precision(file: &Path) -> Result<()> {
    let raw = unlight_raw::load(file)?;
    let recipe = Recipe::default();
    let render = |precision| export(&raw, &recipe, &ExportOptions { precision, ..ExportOptions::default() });
    let wide = render(Precision::F32)?;
    let narrow = render(Precision::F16)?;
    let stats = output::difference(&wide.pixels, &narrow.pixels);
    println!(
        "{}: ΔE2000 mean {:.4}, p99 {:.4}, max {:.4}",
        raw.metadata.camera(),
        stats.mean,
        stats.p99,
        stats.max
    );
    Ok(())
}

fn hang_test(file: &Path, confirmed: bool) -> Result<()> {
    if !confirmed {
        bail!("refusing to hang the GPU without --i-understand-this-may-crash-my-desktop");
    }
    let raw = unlight_raw::load(file)?;
    let recipe = Recipe::default();
    let editor = GpuContext::new()?;
    let export_device = GpuContext::new()?;
    let engine = Engine::new(editor.clone(), Precision::F16)?;
    let mut session = Session::open(engine, &raw, &recipe)?;
    let target = session.view_target(640, 426);
    session.render(&recipe, &ViewRequest::default(), &target, None)?;

    let outcome = stall_device(&export_device, &editor);
    println!(
        "after {:.1} s: export device lost: {}, editor device lost: {}",
        outcome.elapsed.as_secs_f64(),
        outcome.stalled_device_lost,
        outcome.editor_device_lost,
    );
    let survived = session.render(&recipe, &ViewRequest::default(), &target, None);
    println!("editor renders after the hang: {}", survived.is_ok());
    let image = export(&raw, &recipe, &ExportOptions::default())?;
    println!("export on a fresh device: {}×{} rendered", image.width, image.height);
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Info { file } => info(&file),
        Command::Merge(command) => merge::run(&command),
        Command::Render { file, output, recipe, precision, crop } => {
            render(&file, &output, recipe.as_deref(), precision.into(), crop.as_deref())
        }
        Command::Export { list_presets: true, .. } => {
            deliver::list_presets();
            Ok(())
        }
        Command::Export { files, output, preset, recipe, .. } => {
            deliver::run(&files, &output, &preset, &load_recipe(recipe.as_deref())?)
        }
        Command::Preview { file, output, width, recipe } => {
            preview(&file, &output, width, recipe.as_deref())
        }
        Command::Bench(args) => bench::run(&args),
        Command::Lens { file } => lens::report(&file),
        Command::Straightness { file, recipe, lines } => {
            lens::measure_straightness(&file, &load_recipe(recipe.as_deref())?, lines)
        }
        Command::ComparePrecision { file } => compare_precision(&file),
        Command::ComparePreview(args) => compare::run(&args),
        Command::Probe(args) => probe::run(&args),
        Command::CompareLook { files } => look::compare(&files),
        Command::AutoTone(args) => auto::run(&args),
        Command::Profile { file, recipe } => profile::report(&file, &load_recipe(recipe.as_deref())?),
        #[cfg(feature = "ai")]
        Command::Ai(command) => ai::run(&command),
        Command::HangTest { file, i_understand_this_may_crash_my_desktop } => {
            hang_test(&file, i_understand_this_may_crash_my_desktop)
        }
    }
}
