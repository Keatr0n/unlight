//! Photo Merge on a worker. A merge can take minutes, and reads frames through the engine on a
//! device of its own, so it can't hold up the editor or be taken down with it.

use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    thread,
};
use unlight_engine::{read_reduced, read_scene};
use unlight_merge::{
    ANALYSIS_EDGE, HdrOptions, MergeError, PanoramaOptions, Progress, ProgressState, RawFrames, SceneReader, merge_hdr,
    merge_panorama, merged_path, write_merged,
};
use unlight_raw::{MergeKind, RawImage};
use unlight_recipe::Recipe;

/// Share of the progress bar that goes to reading each photo's small copy.
const READING_SHARE: f32 = 0.2;

pub enum MergeJob {
    Hdr { sources: Vec<PathBuf>, options: HdrOptions },
    Panorama { sources: Vec<PathBuf>, options: PanoramaOptions },
}

#[derive(Debug, PartialEq)]
pub enum MergeOutcome {
    Finished(PathBuf),
    Cancelled,
    Failed(String),
}

/// A running merge. Dropping it cancels the merge and waits for it to stop.
pub struct Merger {
    progress: Progress,
    outcome: Receiver<MergeOutcome>,
    thread: Option<thread::JoinHandle<()>>,
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

fn merge(job: &MergeJob, progress: &Progress) -> unlight_merge::Result<PathBuf> {
    let (sources, kind) = match job {
        MergeJob::Hdr { sources, .. } => (sources, MergeKind::Hdr),
        MergeJob::Panorama { sources, .. } => (sources, MergeKind::Panorama),
    };
    let reading = progress.within(0.0, READING_SHARE);
    let frames = RawFrames::open(sources, ANALYSIS_EDGE as u32, &EngineScenes, &reading)?;
    let merging = progress.within(READING_SHARE, 1.0);
    let merged = match job {
        MergeJob::Hdr { options, .. } => merge_hdr(&frames, options, &merging)?.0,
        MergeJob::Panorama { options, .. } => merge_panorama(&frames, options, &merging)?.0,
    };
    let path = merged_path(sources, kind).ok_or(MergeError::TooFewFrames)?;
    write_merged(&path, &merged)?;
    Ok(path)
}

impl Merger {
    pub fn start(job: MergeJob, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let progress = Progress::default();
        let watcher = progress.clone();
        let (sender, outcome) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("unlight-merge".into())
            .spawn(move || {
                let result = match merge(&job, &watcher) {
                    Ok(path) => MergeOutcome::Finished(path),
                    Err(MergeError::Cancelled) => MergeOutcome::Cancelled,
                    Err(error) => MergeOutcome::Failed(error.to_string()),
                };
                let _ = sender.send(result);
                wake();
            })
            .context("starting the merge thread")?;
        Ok(Self { progress, outcome, thread: Some(thread) })
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }

    pub fn state(&self) -> ProgressState {
        self.progress.state()
    }

    pub fn try_finish(&self) -> Option<MergeOutcome> {
        self.outcome.try_recv().ok()
    }
}

impl Drop for Merger {
    fn drop(&mut self) {
        self.progress.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
