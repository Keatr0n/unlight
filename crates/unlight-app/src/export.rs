//! Batch export on a worker. Each photo takes a few seconds, so this runs beside the UI, on
//! the separate device `unlight_engine::export` opens for itself.

use crate::state::transfer::ExportFailure;
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, Local, Timelike};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};
use unlight_catalog::{Catalog, VariantId};
use unlight_engine::{ExportOptions, export};
use unlight_output::{CaptureTime, ExportSettings, Image, ImageMetadata, write_image};
use unlight_raw::Metadata;
use unlight_recipe::Orientation;

pub enum ExportEvent {
    Started { name: String },
    Finished { skipped: bool, device_losses: u32 },
    Failed(ExportFailure),
    Done { cancelled: bool },
}

pub struct ExportJob {
    pub variants: Vec<VariantId>,
    pub settings: ExportSettings,
    pub directory: PathBuf,
}

/// A running export. Dropping it cancels what's left and waits for the file in progress.
pub struct Exporter {
    cancel: Arc<AtomicBool>,
    events: Receiver<ExportEvent>,
    thread: Option<thread::JoinHandle<()>>,
}

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

fn image_metadata(file: &Path, meta: &Metadata, sequence: u32, rating: u8) -> ImageMetadata {
    ImageMetadata {
        source_name: file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        sequence,
        rating: (rating > 0).then_some(rating),
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

enum Outcome {
    Written { device_losses: u32 },
    Skipped { device_losses: u32 },
}

fn export_one(
    catalog: &Catalog,
    id: VariantId,
    sequence: u32,
    settings: &ExportSettings,
    directory: &Path,
) -> Result<Outcome> {
    let variant = catalog.variant(id)?;
    let photo = catalog.photo(variant.photo)?;
    if !photo.path.is_file() {
        bail!("the file is missing: {}", photo.path.display());
    }
    let raw = unlight_raw::load(&photo.path).context("couldn't decode the raw file")?;
    let options = ExportOptions {
        output_space: settings.color_space,
        mask_rasters: crate::mask_rasters(),
        ..ExportOptions::default()
    };
    let rendered = export(&raw, &variant.recipe.effective(), &options)?;
    let device_losses = rendered.device_losses;
    let orientation = Orientation::from_libraw(raw.metadata.orientation).then(variant.recipe.orientation);
    let (pixels, width, height) = orientation.apply(rendered.pixels, rendered.width, rendered.height);
    let image = Image::new(width, height, settings.color_space, pixels)?;
    let metadata = image_metadata(&photo.path, &raw.metadata, sequence, variant.rating);
    match write_image(&image, settings, &metadata, directory) {
        Ok(_) => Ok(Outcome::Written { device_losses }),
        Err(unlight_output::Error::Skipped { .. }) => Ok(Outcome::Skipped { device_losses }),
        Err(error) => Err(anyhow!(error)),
    }
}

fn file_name_of(catalog: &Catalog, id: VariantId) -> String {
    let path = catalog
        .variant(id)
        .and_then(|variant| Ok(catalog.photo(variant.photo)?.path));
    path.ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| format!("photo {}", id.0))
}

fn run(
    catalog: &Catalog,
    job: &ExportJob,
    cancel: &AtomicBool,
    events: &Sender<ExportEvent>,
    wake: &dyn Fn(),
) {
    let send = |event: ExportEvent| {
        let _ = events.send(event);
        wake();
    };
    if let Err(error) = std::fs::create_dir_all(&job.directory) {
        send(ExportEvent::Failed(ExportFailure {
            name: job.directory.display().to_string(),
            message: error.to_string(),
        }));
        send(ExportEvent::Done { cancelled: false });
        return;
    }
    for (index, id) in job.variants.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            send(ExportEvent::Done { cancelled: true });
            return;
        }
        let name = file_name_of(catalog, *id);
        send(ExportEvent::Started { name: name.clone() });
        match export_one(catalog, *id, index as u32 + 1, &job.settings, &job.directory) {
            Ok(Outcome::Written { device_losses }) => {
                send(ExportEvent::Finished { skipped: false, device_losses });
            }
            Ok(Outcome::Skipped { device_losses }) => {
                send(ExportEvent::Finished { skipped: true, device_losses });
            }
            Err(error) => send(ExportEvent::Failed(ExportFailure {
                name,
                message: format!("{error:#}"),
            })),
        }
    }
    send(ExportEvent::Done { cancelled: false });
}

impl Exporter {
    pub fn start(catalog: Arc<Catalog>, job: ExportJob, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, events) = mpsc::channel();
        let flag = cancel.clone();
        let thread = thread::Builder::new()
            .name("unlight-export".into())
            .spawn(move || run(&catalog, &job, &flag, &sender, wake.as_ref()))
            .context("starting the export thread")?;
        Ok(Self {
            cancel,
            events,
            thread: Some(thread),
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn try_next(&self) -> Option<ExportEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Exporter {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_leaves_out_what_the_file_lacks() {
        let meta = Metadata {
            make: "Acme".into(),
            iso: 400.0,
            ..Metadata::default()
        };
        let converted = image_metadata(Path::new("/a/IMG_1.ARW"), &meta, 3, 0);
        assert_eq!(converted.source_name, "IMG_1");
        assert_eq!(converted.sequence, 3);
        assert_eq!(converted.camera_make.as_deref(), Some("Acme"));
        assert_eq!(converted.camera_model, None);
        assert_eq!(converted.iso, Some(400));
        assert_eq!(converted.rating, None);
    }
}
