//! Background work for the open photos: previews from the cache, and raw decodes. The UI
//! thread only queues jobs and picks up results, so opening a photo can't stall a frame.

use anyhow::Result;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};
use unlight_catalog::{Fingerprint, LibraryRow, PreviewCache, PreviewSize, PreviewSource, RgbaImage};
use unlight_engine::{Engine, EngineError, GpuContext, Precision, Session};
use unlight_raw::Metadata;
use unlight_recipe::Recipe;

const WORKERS: usize = 3;

/// A row's preview identity, owned so it can cross threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewJob {
    pub path: PathBuf,
    pub fingerprint: Fingerprint,
    pub orientation: i32,
}

impl PreviewJob {
    pub fn of(row: &LibraryRow) -> Self {
        Self {
            path: row.path.clone(),
            fingerprint: row.fingerprint,
            orientation: row.orientation,
        }
    }

    fn fetch(&self, cache: &PreviewCache, size: PreviewSize) -> unlight_catalog::Result<RgbaImage> {
        let source = PreviewSource {
            fingerprint: self.fingerprint,
            path: &self.path,
            orientation: self.orientation,
        };
        cache.get(&source, size)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Thumbnail(PreviewJob),
    Preview(PreviewJob),
    Session { path: PathBuf, recipe: Box<Recipe> },
}

pub enum Loaded {
    Thumbnail {
        path: PathBuf,
        image: RgbaImage,
    },
    ThumbnailFailed {
        path: PathBuf,
    },
    Preview {
        path: PathBuf,
        image: RgbaImage,
    },
    PreviewFailed {
        path: PathBuf,
    },
    Session {
        path: PathBuf,
        session: Box<Session>,
        metadata: Metadata,
    },
    SessionFailed {
        path: PathBuf,
        message: String,
        device_lost: bool,
    },
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    stopping: AtomicBool,
    engines: Mutex<Vec<Engine>>,
    context: Arc<GpuContext>,
    previews: Arc<PreviewCache>,
}

impl Shared {
    fn next_job(&self) -> Option<Job> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.stopping.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(job) = queue.pop_front() {
                return Some(job);
            }
            queue = self.wake.wait(queue).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn take_engine(&self) -> Result<Engine, EngineError> {
        let spare = self.engines.lock().unwrap_or_else(PoisonError::into_inner).pop();
        match spare {
            Some(engine) => Ok(engine),
            None => Engine::new(self.context.clone(), Precision::F16),
        }
    }

    fn return_engine(&self, engine: Engine) {
        self.engines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(engine);
    }
}

pub struct Loader {
    shared: Arc<Shared>,
    results: Receiver<Loaded>,
    workers: Vec<thread::JoinHandle<()>>,
}

fn message_for(error: &unlight_raw::Error) -> String {
    match error {
        unlight_raw::Error::UnsupportedCompression { camera } => {
            format!("{camera} files with this compression can't be edited yet")
        }
        other => other.to_string(),
    }
}

fn open_session(shared: &Shared, path: &Path, recipe: &Recipe) -> Option<Loaded> {
    let failed = |message: String, device_lost: bool| {
        Some(Loaded::SessionFailed {
            path: path.to_path_buf(),
            message,
            device_lost,
        })
    };
    let raw = match unlight_raw::load(path) {
        Ok(raw) => raw,
        Err(error) => return failed(message_for(&error), false),
    };
    if shared.stopping.load(Ordering::SeqCst) {
        return None;
    }
    let engine = match shared.take_engine() {
        Ok(engine) => engine,
        Err(error) => return failed(error.to_string(), true),
    };
    match Session::open(engine, &raw, recipe) {
        Ok(mut session) => {
            session.set_mask_rasters(crate::mask_rasters());
            Some(Loaded::Session {
                path: path.to_path_buf(),
                session: Box::new(session),
                metadata: raw.metadata,
            })
        }
        Err(error) => {
            let device_lost = matches!(error, EngineError::DeviceLost(_));
            failed(error.to_string(), device_lost)
        }
    }
}

fn load(shared: &Shared, job: &Job) -> Option<Loaded> {
    match job {
        Job::Thumbnail(preview) => {
            let path = preview.path.clone();
            Some(match preview.fetch(&shared.previews, PreviewSize::Grid) {
                Ok(image) => Loaded::Thumbnail { path, image },
                Err(_) => Loaded::ThumbnailFailed { path },
            })
        }
        Job::Preview(preview) => {
            let path = preview.path.clone();
            Some(match preview.fetch(&shared.previews, PreviewSize::Loupe) {
                Ok(image) => Loaded::Preview { path, image },
                Err(_) => Loaded::PreviewFailed { path },
            })
        }
        Job::Session { path, recipe } => open_session(shared, path, recipe),
    }
}

fn work(shared: &Shared, results: &Sender<Loaded>, repaint: &(dyn Fn() + Send + Sync)) {
    while let Some(job) = shared.next_job() {
        if let Some(loaded) = load(shared, &job) {
            if results.send(loaded).is_err() {
                return;
            }
            repaint();
        }
    }
}

impl Loader {
    /// `repaint` wakes the UI when a result arrives, since an idle app isn't polling.
    pub fn new(
        context: Arc<GpuContext>,
        previews: Arc<PreviewCache>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
            engines: Mutex::new(Vec::new()),
            context,
            previews,
        });
        let (sender, results) = mpsc::channel();
        let workers = (0..WORKERS)
            .map(|index| {
                let (shared, sender, repaint) = (shared.clone(), sender.clone(), repaint.clone());
                thread::Builder::new()
                    .name(format!("unlight-load-{index}"))
                    .spawn(move || work(&shared, &sender, repaint.as_ref()))
            })
            .filter_map(Result::ok)
            .collect();
        Self {
            shared,
            results,
            workers,
        }
    }

    /// Takes back the queued photo jobs, leaving the thumbnails.
    pub fn cancel_photo_jobs(&self) -> Vec<Job> {
        let mut queue = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let (thumbnails, cancelled): (Vec<Job>, Vec<Job>) =
            queue.drain(..).partition(|job| matches!(job, Job::Thumbnail(_)));
        queue.extend(thumbnails);
        cancelled
    }

    /// Queues a job ahead of the thumbnails.
    pub fn push(&self, job: Job) {
        let mut queue = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let position = queue
            .iter()
            .position(|queued| matches!(queued, Job::Thumbnail(_)));
        let position = position.unwrap_or(queue.len());
        queue.insert(position, job);
        self.shared.wake.notify_all();
    }

    /// Replaces the queued thumbnails with these. Returns the ones dropped unstarted.
    pub fn set_thumbnails(&self, wanted: impl IntoIterator<Item = PreviewJob>) -> Vec<PreviewJob> {
        let mut queue = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let (thumbnails, others): (Vec<Job>, Vec<Job>) =
            queue.drain(..).partition(|job| matches!(job, Job::Thumbnail(_)));
        queue.extend(others);
        queue.extend(wanted.into_iter().map(Job::Thumbnail));
        self.shared.wake.notify_all();
        thumbnails
            .into_iter()
            .filter_map(|job| match job {
                Job::Thumbnail(preview) => Some(preview),
                _ => None,
            })
            .collect()
    }

    pub fn try_next(&self) -> Option<Loaded> {
        self.results.try_recv().ok()
    }

    /// Takes back the engine of a session nobody needs, so the next decode can reuse it.
    pub fn recycle(&self, session: Session) {
        let Session { engine, .. } = session;
        self.shared.return_engine(engine);
    }
}

/// Workers have to be gone before the process exits: a thread still inside the driver while
/// its libraries unload is a crash on the way out.
impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
