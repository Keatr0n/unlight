//! `unlight-cli ai`, the local AI tools. Only `download` uses the network.

use crate::{load_recipe, mask_rasters, output};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use unlight_ai::{
    CancelToken, LinearImage, Manifest, MaskStore, Model, ModelStatus, ModelStore, Progress, RgbImage, Task, Turn,
    Worker, working_size,
};
use unlight_engine::{Engine, ExportOptions, GpuContext, Precision, Session, ViewRequest, export_with_scene, read_scene};
use unlight_recipe::{Orientation, Recipe};

const REPORT_EVERY_PERCENT: u64 = 10;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Subcommand)]
pub enum AiCommand {
    /// List the models Unlight can download, with their sizes and licences.
    List,
    /// Download a model. This is the only thing in Unlight that uses the network.
    Download { name: String },
    /// Make a mask for a photo. It is stored for recipes to use and copied to the output.
    Mask(MaskArgs),
    /// Render a photo with AI Denoise in place of the engine's noise reduction.
    Denoise(DenoiseArgs),
}

#[derive(Args)]
pub struct MaskArgs {
    file: PathBuf,
    #[arg(long, conflicts_with = "sky", required_unless_present = "sky")]
    subject: bool,
    #[arg(long)]
    sky: bool,
    /// 16-bit grayscale PNG, in the sensor orientation of the raw file.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub struct DenoiseArgs {
    file: PathBuf,
    /// .tiff (16-bit) or .png (8-bit).
    #[arg(short, long)]
    output: PathBuf,
    /// How much of the change to apply, from 0 to 1.
    #[arg(long, default_value_t = 1.0)]
    strength: f32,
    /// Edit recipe as JSON. Its noise reduction settings are not used.
    #[arg(long)]
    recipe: Option<PathBuf>,
}

fn status_text(status: ModelStatus) -> String {
    match status {
        ModelStatus::Ready => "installed".to_owned(),
        ModelStatus::Partial { received } => format!("partly downloaded, {} MB", received / 1_000_000),
        ModelStatus::Missing => "not installed".to_owned(),
    }
}

fn list() -> Result<()> {
    let store = ModelStore::open_default()?;
    for model in Manifest::bundled()?.models {
        println!(
            "{:<14} {:>4} MB  {:<12} {}: {}",
            model.name,
            model.size_in_megabytes(),
            model.licence,
            model.title,
            status_text(store.status(&model)),
        );
    }
    Ok(())
}

fn report_progress(step: Progress, last_reported: &mut u64) {
    let percent = step.done * 100 / step.total.max(1);
    if percent >= *last_reported + REPORT_EVERY_PERCENT {
        *last_reported = percent;
        eprintln!("{percent}%");
    }
}

fn download(name: &str) -> Result<()> {
    let manifest = Manifest::bundled()?;
    let model = manifest.find(name)?;
    println!("{}: {} MB, {} licence ({})", model.title, model.size_in_megabytes(), model.licence, model.licence_url);
    println!("from {}", model.url);
    let mut last_reported = 0;
    let store = ModelStore::open_default()?;
    let path = store.download(model, &mut |step| report_progress(step, &mut last_reported), &CancelToken::new())?;
    println!("verified and saved to {}", path.display());
    Ok(())
}

fn require_installed(store: &ModelStore, model: &Model) -> Result<()> {
    if store.status(model) != ModelStatus::Ready {
        bail!("{} Run `unlight-cli ai download {}`.", model.consent_prompt(), model.name);
    }
    Ok(())
}

/// Renders the photo with no edits at all: the mask must cover the whole frame, and shouldn't
/// change because a slider moved.
fn render_for_selection(file: &Path) -> Result<(RgbImage, Turn)> {
    let raw = unlight_raw::load(file)?;
    let recipe = Recipe::default();
    let engine = Engine::new(GpuContext::new()?, Precision::F16)?;
    let mut session = Session::open(engine, &raw, &recipe)?;
    let [width, height] = session.output_size(&recipe);
    let [width, height] = working_size(width, height);
    let target = session.view_target(width, height);
    session.render(&recipe, &ViewRequest::default(), &target, None)?;
    let image = RgbImage::from_rgba(width, height, &session.read_view(&target)?);
    Ok((image, Turn::from_libraw(raw.metadata.orientation)))
}

fn mask(args: &MaskArgs) -> Result<()> {
    let task = if args.sky { Task::Sky } else { Task::Subject };
    let manifest = Manifest::bundled()?;
    let model = manifest.for_task(task).context("the manifest has no model for this tool")?;
    let store = ModelStore::open_default()?;
    require_installed(&store, model)?;

    let (image, turn) = render_for_selection(&args.file)?;
    let (width, height) = (image.width, image.height);
    let masks = MaskStore::open_default()?;
    let worker = Worker::new(store, masks.clone(), || {});
    let selection = worker.select(model, image, turn).wait()?;
    masks.load(&selection.mask_id)?.write_png(&args.output)?;
    println!(
        "{width}×{height}: load {:.2} s, inference {:.2} s, mask {}",
        selection.load_time.as_secs_f64(),
        selection.run_time.as_secs_f64(),
        selection.mask_id,
    );
    Ok(())
}

fn denoise(args: &DenoiseArgs) -> Result<()> {
    let manifest = Manifest::bundled()?;
    let model = manifest.for_task(Task::Denoise).context("the manifest has no model for this tool")?;
    let store = ModelStore::open_default()?;
    require_installed(&store, model)?;

    let recipe = load_recipe(args.recipe.as_deref())?;
    let raw = unlight_raw::load(&args.file)?;
    let scene = read_scene(&raw, &recipe)?;
    let image = LinearImage { width: raw.width, height: raw.height, pixels: scene };

    let worker = Worker::new(store, MaskStore::open_default()?, || {});
    let started = Instant::now();
    let ticket = worker.denoise(model, image, args.strength);
    let mut last_reported = 0;
    let cleaned = loop {
        if let Some(result) = ticket.poll() {
            break result?;
        }
        report_progress(ticket.progress(), &mut last_reported);
        thread::sleep(POLL_INTERVAL);
    };
    let tiles = ticket.progress().total;
    println!("denoised {tiles} tiles in {:.1} s", started.elapsed().as_secs_f64());

    let options = ExportOptions { mask_rasters: mask_rasters(), ..ExportOptions::default() };
    let rendered = export_with_scene(&raw, &recipe, &options, &cleaned.pixels)?;
    let orientation = Orientation::from_libraw(raw.metadata.orientation).then(recipe.orientation);
    let (pixels, width, height) = orientation.apply(rendered.pixels, rendered.width, rendered.height);
    output::save(&args.output, &pixels, width, height)
}

pub fn run(command: &AiCommand) -> Result<()> {
    match command {
        AiCommand::List => list(),
        AiCommand::Download { name } => download(name),
        AiCommand::Mask(args) => mask(args),
        AiCommand::Denoise(args) => denoise(args),
    }
}
