//! Latest-value registers (`P1.md` §3.3): one value kept, numbered monotonically, readable at
//! once or awaited on change without blocking writers.

use std::sync::Arc;
use tokio::sync::watch;

/// The latest value of something, and how many updates there have been. Multiple writers may
/// share a `Register` across threads; neither `set` nor `change` ever blocks or allocates more
/// than the `Arc<T>` holding the new value.
#[derive(Debug)]
pub struct Register<T>(Arc<watch::Sender<(u64, Arc<T>)>>);

impl<T> Clone for Register<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// A reader of a [`Register`]. Late readers get the latest value at once via [`Reading::now`];
/// slow readers skip intermediate values replaced between two [`Reading::next`] calls.
#[derive(Debug)]
pub struct Reading<T>(watch::Receiver<(u64, Arc<T>)>);

impl<T> Clone for Reading<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Register<T> {
    /// Creates a register holding `first` at sequence `0`.
    pub fn new(first: T) -> Self {
        let (tx, _rx) = watch::channel((0, Arc::new(first)));
        Self(Arc::new(tx))
    }

    /// Replaces the current value and advances the sequence number by `1`.
    pub fn set(&self, value: T) {
        let value = Arc::new(value);
        self.0.send_modify(|(seq, current)| {
            *seq += 1;
            *current = value;
        });
    }

    /// Inspects the current value and replaces it—advancing the sequence number by `1` and waking
    /// waiting readers—only when `with` returns `Some`.
    pub fn change(&self, with: impl FnOnce(&T) -> Option<T>) {
        self.0.send_if_modified(|(seq, current)| match with(current) {
            Some(next) => {
                *seq += 1;
                *current = Arc::new(next);
                true
            }
            None => false,
        });
    }

    /// Subscribes a new [`Reading`] positioned at the current value.
    pub fn read(&self) -> Reading<T> {
        Reading(self.0.subscribe())
    }

    /// Current monotonic sequence number of this register.
    pub fn seq(&self) -> u64 {
        self.0.borrow().0
    }
}

impl<T> Reading<T> {
    /// Returns the latest `(seq, value)` immediately.
    pub fn now(&self) -> (u64, Arc<T>) {
        self.0.borrow().clone()
    }

    /// Waits for the next change after the last value observed by this [`Reading`], returning
    /// `None` once every [`Register`] writer has been dropped.
    pub async fn next(&mut self) -> Option<(u64, Arc<T>)> {
        self.0.changed().await.ok()?;
        Some(self.0.borrow_and_update().clone())
    }
}
