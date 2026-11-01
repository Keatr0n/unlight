use crate::error::{Error, Result};
use std::sync::{Arc, Mutex, PoisonError, atomic::{AtomicBool, Ordering}};

type Abort = Box<dyn FnOnce() + Send>;

/// Shared between a job and whoever may want it stopped. Cloning gives another handle on the
/// same flag.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<Shared>);

#[derive(Default)]
struct Shared {
    cancelled: AtomicBool,
    abort: Mutex<Option<Abort>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        if let Some(abort) = self.take_abort() {
            abort();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() { Err(Error::Cancelled) } else { Ok(()) }
    }

    /// Runs `abort` if cancellation arrives before `disarm`, so work that can't poll the flag
    /// can still be stopped part way through.
    pub(crate) fn arm(&self, abort: impl FnOnce() + Send + 'static) {
        *self.0.abort.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(abort));
        // The flag may have been set before the hook was in place.
        if self.is_cancelled() {
            self.cancel();
        }
    }

    pub(crate) fn disarm(&self) {
        self.take_abort();
    }

    fn take_abort(&self) -> Option<Abort> {
        self.0.abort.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn counting_abort(token: &CancelToken) -> Arc<AtomicU32> {
        let count = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&count);
        token.arm(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        count
    }

    #[test]
    fn cancelling_runs_the_abort_once() {
        let token = CancelToken::new();
        let count = counting_abort(&token);
        token.cancel();
        token.cancel();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(matches!(token.check(), Err(Error::Cancelled)));
    }

    #[test]
    fn arming_after_cancellation_aborts_at_once() {
        let token = CancelToken::new();
        token.cancel();
        assert_eq!(counting_abort(&token).load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_disarmed_abort_never_runs() {
        let token = CancelToken::new();
        let count = counting_abort(&token);
        token.disarm();
        token.cancel();
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
