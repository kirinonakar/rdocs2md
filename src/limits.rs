//! Cooperative time limits.
//!
//! Parsers that walk many pages or sections call [`expired`] and stop early.
//! A hard limit cannot be enforced from the same thread, so this is a budget
//! that bounds the work a single document may perform; a runaway loop inside a
//! third-party decoder is still only bounded by the caller's process timeout.
use crate::model::Result;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Restores the previous deadline when dropped, so nested conversions keep the
/// budget of the calling request.
pub struct Guard(Option<Instant>);

impl Drop for Guard {
    fn drop(&mut self) {
        DEADLINE.with(|d| d.set(self.0))
    }
}

/// Starts a budget of `seconds` on this thread. Zero or `None` removes it.
pub fn start(seconds: Option<u64>) -> Guard {
    let previous = DEADLINE.with(Cell::get);
    let deadline = seconds
        .filter(|s| *s > 0)
        .and_then(|s| Instant::now().checked_add(Duration::from_secs(s)));
    DEADLINE.with(|d| d.set(deadline));
    Guard(previous)
}

pub fn expired() -> bool {
    DEADLINE.with(|d| d.get().is_some_and(|t| Instant::now() >= t))
}

pub fn check() -> Result<()> {
    if expired() {
        Err("time limit exceeded".into())
    } else {
        Ok(())
    }
}
