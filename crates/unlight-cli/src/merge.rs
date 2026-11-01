//! `unlight-cli merge`: HDR and panorama merges from the command line.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
use unlight_engine::{read_reduced, read_scene};
use unlight_merge::{
    ANALYSIS_EDGE, DEFAULT_MAX_EDGE, Deghost, ExposureSource, HdrOptions, HdrReport, Merged, PanoramaOptions,
    PanoramaReport, Progress, Projection, RawFrames, SceneReader, merge_hdr, merge_panorama, merged_path, write_merged,
};
use unlight_raw::{MergeKind, RawImage};
use unlight_recipe::Recipe;

#[derive(Clone, Copy, ValueEnum)]
pub enum DeghostArg {
    None,
    Low,
    Medium,
    High,
}

impl From<DeghostArg> for Deghost {
    fn from(value: DeghostArg) -> Self {
        match value {
            DeghostArg::None => Self::None,
            DeghostArg::Low => Self::Low,
            DeghostArg::Medium => Self::Medium,
            DeghostArg::High => Self::High,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ProjectionArg {
    Spherical,
    Cylindrical,
    Perspective,
}

impl From<ProjectionArg> for Projection {
    fn from(value: ProjectionArg) -> Self {
        match value {
            ProjectionArg::Spherical => Self::Spherical,
            ProjectionArg::Cylindrical => Self::Cylindrical,
            ProjectionArg::Perspective => Self::Perspective,
        }
    }
}

#[derive(Subcommand)]
pub enum MergeCommand {
    /// Merge a bracket of exposures into one linear photo.
    Hdr(HdrArgs),
    /// Stitch overlapping photos into one linear photo.
    Pano(PanoArgs),
}

#[derive(Args)]
pub struct HdrArgs {
    #[arg(required = true, num_args = 2..)]
    files: Vec<PathBuf>,
    /// Defaults to a new name beside the first photo.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Leave the frames as they are, for a bracket shot on a tripod.
    #[arg(long)]
    no_align: bool,
    #[arg(long, value_enum, default_value = "none")]
    deghost: DeghostArg,
}

#[derive(Args)]
pub struct PanoArgs {
    #[arg(required = true, num_args = 2..)]
    files: Vec<PathBuf>,
    /// Defaults to a new name beside the first photo.
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "spherical")]
    projection: ProjectionArg,
    /// From 0 to 100.
    #[arg(long, default_value_t = 0.0)]
    boundary_warp: f32,
    /// Keep the whole outline rather than cropping to the largest clean rectangle.
    #[arg(long)]
    no_auto_crop: bool,
    #[arg(long, default_value_t = DEFAULT_MAX_EDGE)]
    max_edge: u32,
}

struct EngineScenes;

impl SceneReader for EngineScenes {
    fn full(&self, raw: &RawImage) -> std::result::Result<Vec<[f32; 4]>, String> {
        read_scene(raw, &Recipe::default()).map_err(|error| error.to_string())
    }

    fn reduced(&self, raw: &RawImage, max_edge: u32) -> std::result::Result<(u32, u32, Vec<[f32; 3]>), String> {
        let scene = read_reduced(raw, max_edge).map_err(|error| error.to_string())?;
        Ok((scene.width, scene.height, scene.pixels))
    }
}

fn peak_memory_gib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / (1024.0 * 1024.0))
}

fn write_result(merged: &Merged, output: Option<&Path>, files: &[PathBuf], kind: MergeKind) -> Result<()> {
    let path = match output {
        Some(path) => path.to_owned(),
        None => merged_path(files, kind).context("no name to save the merge under")?,
    };
    if path.extension().is_none_or(|extension| !extension.eq_ignore_ascii_case("ulr")) {
        bail!("{} has to end in .ulr", path.display());
    }
    write_merged(&path, merged)?;
    let size = std::fs::metadata(&path)?.len() as f64 / (1 << 20) as f64;
    println!("wrote {} ({} × {}, {size:.0} MiB)", path.display(), merged.image.width, merged.image.height);
    Ok(())
}

fn report_hdr(report: &HdrReport, files: &[PathBuf]) {
    for (index, file) in files.iter().enumerate() {
        let estimate = &report.exposures[index];
        let source = match estimate.source {
            ExposureSource::Reference => "reference",
            ExposureSource::Measured => "measured",
            ExposureSource::Metadata => "from the file",
        };
        let stated = estimate.stated.map_or_else(|| "none".to_owned(), |stops| format!("{stops:+.2}"));
        let warning = if estimate.disagrees() { "  file disagrees with pixels" } else { "" };
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        println!("{name}: {:+.3} EV ({source}), file says {stated}{warning}", estimate.used);
    }
    println!("deghosted {:.1}% of the image", report.ghost_share * 100.0);
    println!("clipped in every frame: {:.2}%", report.clipped_share * 100.0);
    println!("headroom above the reference's white: {:.2} EV", report.headroom);
}

fn report_panorama(report: &PanoramaReport, files: &[PathBuf]) {
    println!("used {} of {} frames", report.used.len(), files.len());
    for &index in &report.dropped {
        println!("left out, no overlap found: {}", files[index].display());
    }
    println!("focal length {:.0} px, reprojection error {:.2} px rms", report.focal, report.rms);
    println!("closed a full circle: {}", report.closed_loop);
    println!("lens data for every frame: {}", report.lens_known);
    println!("output is {:.0}% of source scale, {:.2}% empty", report.scale * 100.0, report.empty_share * 100.0);
    println!("vignette fitted from overlaps: {:?}", report.vignette);
}

pub fn run(command: &MergeCommand) -> Result<()> {
    let start = Instant::now();
    let progress = Progress::default();
    match command {
        MergeCommand::Hdr(args) => {
            let frames = RawFrames::open(&args.files, ANALYSIS_EDGE as u32, &EngineScenes, &progress)?;
            let options = HdrOptions { auto_align: !args.no_align, deghost: args.deghost.into() };
            let (merged, report) = merge_hdr(&frames, &options, &progress)?;
            report_hdr(&report, &args.files);
            write_result(&merged, args.output.as_deref(), &args.files, MergeKind::Hdr)?;
        }
        MergeCommand::Pano(args) => {
            let frames = RawFrames::open(&args.files, ANALYSIS_EDGE as u32, &EngineScenes, &progress)?;
            let options = PanoramaOptions {
                projection: args.projection.into(),
                boundary_warp: (args.boundary_warp / 100.0).clamp(0.0, 1.0),
                auto_crop: !args.no_auto_crop,
                max_edge: args.max_edge,
            };
            let (merged, report) = merge_panorama(&frames, &options, &progress)?;
            report_panorama(&report, &args.files);
            write_result(&merged, args.output.as_deref(), &args.files, MergeKind::Panorama)?;
        }
    }
    println!("took {:.1} s", start.elapsed().as_secs_f32());
    if let Some(peak) = peak_memory_gib() {
        println!("peak memory {peak:.2} GiB");
    }
    Ok(())
}
