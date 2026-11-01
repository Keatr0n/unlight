//! The whole path from a raw file to a finished, tagged image on disk.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Datelike, Local, Timelike};
use std::{path::Path, time::Instant};
use unlight_engine::{ExportOptions, export};
use unlight_output::{CaptureTime, ExportPreset, Image, ImageMetadata, write_image};
use unlight_raw::Metadata;
use unlight_recipe::{Orientation, Recipe};

fn positive(value: f32) -> Option<f64> {
    (value > 0.0).then_some(f64::from(value))
}

fn named(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// LibRaw turns the camera's wall clock into a timestamp using the local zone, so reading it
/// back in that zone recovers what the camera recorded.
fn capture_time(timestamp: i64) -> Option<CaptureTime> {
    let local = DateTime::from_timestamp(timestamp, 0)?.with_timezone(&Local);
    (timestamp > 0).then(|| CaptureTime {
        year: local.year(),
        month: local.month() as u8,
        day: local.day() as u8,
        hour: local.hour() as u8,
        minute: local.minute() as u8,
        second: local.second() as u8,
        utc_offset_minutes: None,
    })
}

fn image_metadata(file: &Path, meta: &Metadata, sequence: u32) -> ImageMetadata {
    ImageMetadata {
        source_name: file.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        sequence,
        camera_make: named(&meta.make),
        camera_model: named(&meta.model),
        lens: named(&meta.lens),
        capture_time: capture_time(meta.timestamp),
        exposure_seconds: positive(meta.shutter),
        f_number: positive(meta.aperture),
        iso: (meta.iso > 0.0).then_some(meta.iso as u32),
        focal_length_mm: positive(meta.focal_length),
        ..ImageMetadata::default()
    }
}

pub fn list_presets() {
    for preset in ExportPreset::builtin() {
        println!("{}", preset.name);
    }
}

pub fn run(files: &[impl AsRef<Path>], directory: &Path, preset: &str, recipe: &Recipe) -> Result<()> {
    let presets = ExportPreset::builtin();
    let settings = &presets
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(preset))
        .ok_or_else(|| anyhow!("no preset named {preset:?}, see --list-presets"))?
        .settings;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;

    let options = ExportOptions { output_space: settings.color_space, ..ExportOptions::default() };
    for (index, file) in files.iter().enumerate() {
        let file = file.as_ref();
        let start = Instant::now();
        let raw = unlight_raw::load(file).with_context(|| format!("decoding {}", file.display()))?;
        let rendered = export(&raw, recipe, &options)?;
        let orientation = Orientation::from_libraw(raw.metadata.orientation).then(recipe.orientation);
        let (pixels, width, height) = orientation.apply(rendered.pixels, rendered.width, rendered.height);
        let image = Image::new(width, height, settings.color_space, pixels)?;
        let metadata = image_metadata(file, &raw.metadata, index as u32 + 1);
        let path = write_image(&image, settings, &metadata, directory)?;
        println!("{} ({:.2} s)", path.display(), start.elapsed().as_secs_f64());
    }
    Ok(())
}
