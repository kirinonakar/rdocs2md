//! Cooperative time limits.
//!
//! Parsers that walk many pages or sections call [`expired`] and stop early.
//! A hard limit cannot be enforced from the same thread, so this is a budget
//! that bounds the work a single document may perform; a runaway loop inside a
//! third-party decoder is still only bounded by the caller's process timeout.
use crate::model::Result;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

thread_local! {
    static CURRENT: RefCell<Budget> = const { RefCell::new(Budget { deadline: None, cancelled: None }) };
}

/// A request budget that can be installed on every thread working on the call.
#[derive(Clone)]
pub(crate) struct Budget {
    deadline: Option<Instant>,
    cancelled: Option<Arc<AtomicBool>>,
}

impl Budget {
    fn new(seconds: Option<u64>, cancelled: Option<Arc<AtomicBool>>) -> Self {
        let deadline = seconds
            .filter(|s| *s > 0)
            .and_then(|s| Instant::now().checked_add(Duration::from_secs(s)));
        Self {
            deadline,
            cancelled,
        }
    }

    pub(crate) fn request(seconds: u64) -> Self {
        Self::new(Some(seconds), Some(Arc::new(AtomicBool::new(false))))
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn cancel(&self) {
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn error(&self) -> Option<&'static str> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            Some("request cancelled")
        } else if self.deadline.is_some_and(|t| Instant::now() >= t) {
            Some("time limit exceeded")
        } else {
            None
        }
    }

    pub(crate) fn enter(&self) -> Guard {
        Guard(
            CURRENT.with(|current| current.replace(self.clone())),
            std::marker::PhantomData,
        )
    }
}

/// Restores the calling thread's budget on drop.
pub struct Guard(Budget, std::marker::PhantomData<std::rc::Rc<()>>);

impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            c.replace(self.0.clone());
        });
    }
}

/// Starts a budget of `seconds` on this thread. Zero or `None` removes it.
pub fn start(seconds: Option<u64>) -> Guard {
    Budget::new(seconds, None).enter()
}

pub fn expired() -> bool {
    CURRENT.with(|c| c.borrow().error().is_some())
}

pub fn check() -> Result<()> {
    CURRENT.with(|c| c.borrow().error().map_or(Ok(()), |error| Err(error.into())))
}

/// MCP requests abandon interrupted work. CLI parsing keeps its existing timeout behavior.
pub(crate) fn request_interrupted() -> bool {
    CURRENT.with(|c| {
        let budget = c.borrow();
        budget.cancelled.is_some() && budget.error().is_some()
    })
}

pub(crate) fn check_request() -> Result<()> {
    if request_interrupted() {
        check()
    } else {
        Ok(())
    }
}

/// Test-only: moves this thread's budget into the past without waiting.
#[cfg(test)]
pub(crate) fn expire() {
    CURRENT.with(|c| c.borrow_mut().deadline = Some(Instant::now() - Duration::from_secs(1)));
}
