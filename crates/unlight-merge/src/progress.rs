use crate::{MergeError, Result};
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq)]
pub struct ProgressState {
    pub stage: String,
    /// How far along the whole merge is, from 0 to 1.
    pub fraction: f32,
}

/// Links a running merge with whoever is watching it. Clones share the same state, so the UI
/// keeps one to read from and to cancel with.
#[derive(Clone, Debug)]
pub struct Progress {
    cancelled: Arc<AtomicBool>,
    state: Arc<Mutex<ProgressState>>,
    /// The stretch of the overall bar this handle reports on.
    range: [f32; 2],
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(ProgressState { stage: String::new(), fraction: 0.0 })),
            range: [0.0, 1.0],
        }
    }
}

impl Progress {
    /// A handle for one part of a longer job, whose own 0 to 1 covers `start..end` of this one.
    pub fn within(&self, start: f32, end: f32) -> Self {
        let [low, high] = self.range;
        let at = |value: f32| low + value * (high - low);
        Self { range: [at(start), at(end)], ..self.clone() }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn state(&self) -> ProgressState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub(crate) fn report(&self, stage: &str, fraction: f32) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stage != stage {
            state.stage = stage.to_owned();
        }
        let [low, high] = self.range;
        state.fraction = low + fraction.clamp(0.0, 1.0) * (high - low);
    }

    /// Reports a stage and stops the merge if someone asked it to.
    pub(crate) fn step(&self, stage: &str, fraction: f32) -> Result<()> {
        self.report(stage, fraction);
        self.check()
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() { Err(MergeError::Cancelled) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_sees_progress_and_can_cancel() {
        let progress = Progress::default();
        let watcher = progress.clone();
        progress.step("Aligning", 0.25).unwrap();
        assert_eq!(watcher.state(), ProgressState { stage: "Aligning".into(), fraction: 0.25 });
        watcher.cancel();
        assert!(matches!(progress.step("Merging", 0.5), Err(MergeError::Cancelled)));
    }

    #[test]
    fn a_part_reports_on_its_stretch_of_the_bar() {
        let progress = Progress::default();
        let second_half = progress.within(0.5, 1.0);
        second_half.report("Blending", 0.5);
        assert_eq!(progress.state().fraction, 0.75);
        let last_quarter = second_half.within(0.5, 1.0);
        last_quarter.report("Finishing", 0.0);
        assert_eq!(progress.state().fraction, 0.75);
    }

    #[test]
    fn fractions_stay_in_range() {
        let progress = Progress::default();
        progress.report("Finishing", 1.5);
        assert_eq!(progress.state().fraction, 1.0);
    }
}
