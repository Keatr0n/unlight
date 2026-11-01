//! Runs inference and downloads away from the UI thread. Every job hands back a `Ticket` that
//! can report progress, be polled, and be cancelled.

use crate::{
    cancel::CancelToken,
    download::Progress,
    error::{Error, Result},
    image::{RgbImage, Turn},
    inference::LoadedModel,
    denoise::{LinearImage, denoise},
    manifest::Model,
    mask::MaskStore,
    select::select,
    store::ModelStore,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

type Notify = Arc<dyn Fn() + Send + Sync>;
type Job = Box<dyn FnOnce(&mut Slot) + Send>;

#[derive(Default)]
struct Meter {
    done: AtomicU64,
    total: AtomicU64,
}

impl Meter {
    fn record(&self, step: Progress) {
        self.total.store(step.total, Ordering::Relaxed);
        self.done.store(step.done, Ordering::Relaxed);
    }

    fn read(&self) -> Progress {
        Progress { done: self.done.load(Ordering::Relaxed), total: self.total.load(Ordering::Relaxed) }
    }
}

/// A job in progress. Dropping it does not stop the job; `cancel` does.
pub struct Ticket<T> {
    cancel: CancelToken,
    meter: Arc<Meter>,
    outcome: Receiver<Result<T>>,
}

impl<T> Ticket<T> {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn progress(&self) -> Progress {
        self.meter.read()
    }

    /// The result, once there is one.
    pub fn poll(&self) -> Option<Result<T>> {
        match self.outcome.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(Error::Inference("the worker stopped".to_owned()))),
        }
    }

    pub fn wait(self) -> Result<T> {
        self.outcome.recv().unwrap_or_else(|_| Err(Error::Inference("the worker stopped".to_owned())))
    }
}

/// What a finished selection produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// For `MaskShape::Raster`.
    pub mask_id: String,
    /// Reading, verifying and starting the model, if it wasn't already in memory.
    pub load_time: Duration,
    pub run_time: Duration,
}

/// The model in memory. Only one is kept, because each costs hundreds of megabytes.
#[derive(Default)]
struct Slot {
    held: Option<(String, LoadedModel)>,
}

impl Slot {
    fn model(&mut self, store: &ModelStore, model: &Model) -> Result<&mut LoadedModel> {
        if self.held.as_ref().is_none_or(|(hash, _)| *hash != model.sha256) {
            self.held = None;
            let bytes = store.load(model)?;
            self.held = Some((model.sha256.clone(), LoadedModel::load(&bytes)?));
        }
        match &mut self.held {
            Some((_, loaded)) => Ok(loaded),
            None => Err(Error::ModelMissing(model.name.clone())),
        }
    }
}

pub struct Worker {
    jobs: Sender<Job>,
    models: ModelStore,
    masks: MaskStore,
    notify: Notify,
}

impl Worker {
    /// `notify` is called from the worker whenever a job progresses or ends, so a UI can wake.
    pub fn new(models: ModelStore, masks: MaskStore, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        // If the thread can't start, the queue is gone and every ticket says the worker stopped.
        let _ = thread::Builder::new().name("unlight-ai".to_owned()).spawn(move || {
            let mut slot = Slot::default();
            for job in queue {
                job(&mut slot);
            }
        });
        Self { jobs, models, masks, notify: Arc::new(notify) }
    }

    /// Downloads on a thread of its own, so a slow network never holds up inference.
    pub fn download(&self, model: &Model) -> Ticket<PathBuf> {
        let (ticket, cancel, meter, outcome) = ticket();
        let (store, model, notify) = (self.models.clone(), model.clone(), Arc::clone(&self.notify));
        thread::spawn(move || {
            let mut on_progress = |step| {
                meter.record(step);
                notify();
            };
            let _ = outcome.send(store.download(&model, &mut on_progress, &cancel));
            notify();
        });
        ticket
    }

    /// Denoises a demosaiced photo. `strength` runs from 0, unchanged, to 1, the model's result.
    pub fn denoise(&self, model: &Model, image: LinearImage, strength: f32) -> Ticket<LinearImage> {
        let (ticket, cancel, meter, outcome) = ticket();
        let (store, model, notify) = (self.models.clone(), model.clone(), Arc::clone(&self.notify));
        let job: Job = Box::new(move |slot| {
            let mut on_progress = |step| {
                meter.record(step);
                notify();
            };
            let result = cancel.check().and_then(|()| {
                let spec = model.restoration()?;
                denoise(slot.model(&store, &model)?, spec, &image, strength, &mut on_progress, &cancel)
            });
            let _ = outcome.send(result);
            notify();
        });
        let _ = self.jobs.send(job);
        ticket
    }

    /// Makes a mask from a rendered photo in sensor orientation and stores it.
    pub fn select(&self, model: &Model, image: RgbImage, turn: Turn) -> Ticket<Selection> {
        let (ticket, cancel, _, outcome) = ticket();
        let (store, masks, model, notify) =
            (self.models.clone(), self.masks.clone(), model.clone(), Arc::clone(&self.notify));
        let job: Job = Box::new(move |slot| {
            let result = run_selection(slot, &store, &masks, &model, &image, turn, &cancel);
            let _ = outcome.send(result);
            notify();
        });
        // A send only fails once the worker is gone, and the ticket then reports that itself.
        let _ = self.jobs.send(job);
        ticket
    }
}

fn ticket<T>() -> (Ticket<T>, CancelToken, Arc<Meter>, Sender<Result<T>>) {
    let (cancel, meter) = (CancelToken::new(), Arc::new(Meter::default()));
    let (sender, outcome) = mpsc::channel();
    (Ticket { cancel: cancel.clone(), meter: Arc::clone(&meter), outcome }, cancel, meter, sender)
}

fn run_selection(
    slot: &mut Slot,
    store: &ModelStore,
    masks: &MaskStore,
    model: &Model,
    image: &RgbImage,
    turn: Turn,
    cancel: &CancelToken,
) -> Result<Selection> {
    cancel.check()?;
    let started = Instant::now();
    let loaded = slot.model(store, model)?;
    let load_time = started.elapsed();
    let started = Instant::now();
    let mask = select(loaded, model.segmentation()?, image, turn, cancel)?;
    let run_time = started.elapsed();
    Ok(Selection { mask_id: masks.save(&mask)?, load_time, run_time })
}
