//! A value the daemon re-reads while it is running.
//!
//! Settings are resolved once at startup ([`Resolved`] in the binary), which is
//! the right place for the *precedence* - flag, then the user's GSettings, then
//! `snap set`, then the built-in - but the wrong place to stop: the user can
//! change their GSettings while the daemon runs, and hearing that must not cost
//! a restart. So the resolution stays where it is and lands in one of these
//! instead of in a plain field, and the daemon's settings watch re-runs it and
//! writes the answer back.
//!
//! [`Resolved`]: ../../myna_desktop/index.html
//!
//! Deliberately tiny: a lock, not a channel. Every reader here wants "the value
//! right now" at a moment it chooses (the next press, the next event), never a
//! notification, and a lock cannot leave a reader one edge behind the way a
//! missed wakeup can.

use std::sync::{Arc, RwLock};

/// A shared cell holding the current value of one setting.
///
/// Cloning shares the cell; the daemon hands a clone to the controller and
/// keeps one for the settings watch to write through.
pub struct Live<T>(Arc<RwLock<T>>);

impl<T> Live<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(RwLock::new(value)))
    }

    /// The value now. A poisoned lock yields the value anyway: a panic in some
    /// other holder is not a reason to stop dictating, and the worst a stale
    /// read can do here is show a preedit the user turned off a moment ago.
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set(&self, value: T) {
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
    }
}

impl<T> Clone for Live<T> {
    /// Hand-written: `derive` would demand `T: Clone`, but sharing the cell
    /// clones the handle, never the value.
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for Live<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// So a caller with a fixed value - every test, and any build that never
/// watches - can pass it straight in: `.preedit(true)`.
impl<T> From<T> for Live<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Clone + std::fmt::Debug> std::fmt::Debug for Live<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Live({:?})", self.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_shares_the_cell() {
        let a = Live::new(false);
        let b = a.clone();
        b.set(true);
        assert!(
            a.get(),
            "a write through one handle is read through the other"
        );
    }

    #[test]
    fn a_poisoned_lock_still_reads() {
        let live = Live::new(7);
        let other = live.clone();
        std::thread::spawn(move || {
            let _guard = other.0.write().unwrap();
            panic!("poison the lock");
        })
        .join()
        .expect_err("the thread panicked");
        assert_eq!(live.get(), 7);
        live.set(9);
        assert_eq!(live.get(), 9);
    }
}
