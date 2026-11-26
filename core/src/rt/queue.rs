//! Named bounded queues (`P1.md` §3.13.9). Every channel in the core and shell is constructed
//! here so its capacity, overflow policy, stats row and edge-triggered `queue.full` /
//! `queue.clear` events live in one place.

use super::stats::{QueueStat, Stats};
use std::sync::mpsc as std_mpsc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use zutalk_log::{info, warn};

pub use std::sync::mpsc::{RecvTimeoutError, TryRecvError as ThreadTryRecvError};
pub use tokio::sync::broadcast::error::{RecvError, TryRecvError as FanoutTryRecvError};
pub use tokio::sync::mpsc::error::TryRecvError;

/// What a sender does when the queue is full, stated where the queue is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Full {
    /// Waits for room: only where the sender is on the same path as the reader, or a slower one.
    Wait,
    /// This item is dropped and counted.
    Drop,
    /// Whoever asked is told at once with a code.
    Reject,
    /// The owner ends with a code.
    End,
}

impl Full {
    pub fn code(self) -> &'static str {
        match self {
            Full::Wait => "wait",
            Full::Drop => "drop",
            Full::Reject => "reject",
            Full::End => "end",
        }
    }
}

/// Why `offer` did not accept a value; hands the value back.
#[derive(Debug, PartialEq, Eq)]
pub enum Back<T> {
    Full(T),
    Closed(T),
}

/// A bounded queue read by one async task (`tokio::sync::mpsc`).
pub fn bounded<T>(
    stats: &Stats,
    name: &'static str,
    capacity: usize,
    full: Full,
) -> (Tx<T>, Rx<T>) {
    let (tx, rx) = mpsc::channel(capacity);
    let stat = stats.queue(name, capacity);
    (Tx { tx, stat: stat.clone(), full }, Rx { rx, stat })
}

/// A bounded queue read by a dedicated OS thread (`std::sync::mpsc::sync_channel`).
pub fn threads<T>(
    stats: &Stats,
    name: &'static str,
    capacity: usize,
    full: Full,
) -> (ThreadTx<T>, ThreadRx<T>) {
    let (tx, rx) = std_mpsc::sync_channel(capacity);
    let stat = stats.queue(name, capacity);
    (ThreadTx { tx, stat: stat.clone(), full }, ThreadRx { rx, stat })
}

/// A bounded broadcast bus for many readers; any reader that falls behind skips ahead.
pub fn fanout<T: Clone>(stats: &Stats, name: &'static str, capacity: usize) -> Fanout<T> {
    let (tx, _) = broadcast::channel(capacity);
    let stat = stats.queue(name, capacity);
    Fanout { tx, stat }
}

/// Async sender end of `bounded`.
pub struct Tx<T> {
    tx: mpsc::Sender<T>,
    stat: QueueStat,
    full: Full,
}

impl<T> Clone for Tx<T> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), stat: self.stat.clone(), full: self.full }
    }
}

impl<T> Tx<T> {
    /// Same queue for another kind of sender, with its own overflow policy.
    pub fn with(&self, full: Full) -> Tx<T> {
        Tx { tx: self.tx.clone(), stat: self.stat.clone(), full }
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Never waits. Counts and emits `queue.full` / `queue.clear` on edge transitions.
    pub fn offer(&self, value: T) -> Result<(), Back<T>> {
        match self.tx.try_send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(value)) => {
                if self.stat.note_rejected() {
                    warn!(
                        "queue.full",
                        queue = self.stat.name(),
                        policy = self.full.code(),
                        capacity = self.stat.capacity(),
                    );
                }
                Err(Back::Full(value))
            }
            Err(mpsc::error::TrySendError::Closed(value)) => Err(Back::Closed(value)),
        }
    }

    /// Waits for room. Waiting counts toward `full` but does not emit `queue.full`.
    pub async fn send(&self, value: T) -> Result<(), T> {
        let value = match self.tx.try_send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                return Ok(());
            }
            Err(mpsc::error::TrySendError::Closed(value)) => return Err(value),
            Err(mpsc::error::TrySendError::Full(value)) => {
                self.stat.note_waited();
                value
            }
        };
        match self.tx.send(value).await {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(())
            }
            Err(mpsc::error::SendError(value)) => Err(value),
        }
    }

    /// Blocking wait for room from a non-async thread.
    pub fn send_blocking(&self, value: T) -> Result<(), T> {
        let value = match self.tx.try_send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                return Ok(());
            }
            Err(mpsc::error::TrySendError::Closed(value)) => return Err(value),
            Err(mpsc::error::TrySendError::Full(value)) => {
                self.stat.note_waited();
                value
            }
        };
        match self.tx.blocking_send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(())
            }
            Err(mpsc::error::SendError(value)) => Err(value),
        }
    }
}

/// Async receiver end of `bounded`.
pub struct Rx<T> {
    rx: mpsc::Receiver<T>,
    stat: QueueStat,
}

impl<T> Rx<T> {
    pub async fn recv(&mut self) -> Option<T> {
        let item = self.rx.recv().await?;
        self.stat.note_recv(1);
        Some(item)
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let item = self.rx.try_recv()?;
        self.stat.note_recv(1);
        Ok(item)
    }

    pub async fn recv_many(&mut self, buf: &mut Vec<T>, limit: usize) -> usize {
        let n = self.rx.recv_many(buf, limit).await;
        if n > 0 {
            self.stat.note_recv(n as u64);
        }
        n
    }

    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(item)) => {
                self.stat.note_recv(1);
                Poll::Ready(Some(item))
            }
            other => other,
        }
    }

    pub fn close(&mut self) {
        self.rx.close();
    }
}

impl<T> Drop for Rx<T> {
    fn drop(&mut self) {
        self.rx.close();
        let mut drained = 0u64;
        while self.rx.try_recv().is_ok() {
            drained += 1;
        }
        if drained > 0 {
            self.stat.note_recv(drained);
        }
    }
}

/// Sender end of `threads`.
pub struct ThreadTx<T> {
    tx: std_mpsc::SyncSender<T>,
    stat: QueueStat,
    full: Full,
}

impl<T> Clone for ThreadTx<T> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), stat: self.stat.clone(), full: self.full }
    }
}

impl<T> ThreadTx<T> {
    /// Same queue for another kind of sender, with its own overflow policy.
    pub fn with(&self, full: Full) -> ThreadTx<T> {
        ThreadTx { tx: self.tx.clone(), stat: self.stat.clone(), full }
    }

    /// Never waits. Counts and emits `queue.full` / `queue.clear` on edge transitions.
    pub fn offer(&self, value: T) -> Result<(), Back<T>> {
        match self.tx.try_send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(())
            }
            Err(std_mpsc::TrySendError::Full(value)) => {
                if self.stat.note_rejected() {
                    warn!(
                        "queue.full",
                        queue = self.stat.name(),
                        policy = self.full.code(),
                        capacity = self.stat.capacity(),
                    );
                }
                Err(Back::Full(value))
            }
            Err(std_mpsc::TrySendError::Disconnected(value)) => Err(Back::Closed(value)),
        }
    }

    /// Waits for room on an OS thread.
    pub fn send_blocking(&self, value: T) -> Result<(), T> {
        let value = match self.tx.try_send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                return Ok(());
            }
            Err(std_mpsc::TrySendError::Disconnected(value)) => return Err(value),
            Err(std_mpsc::TrySendError::Full(value)) => {
                self.stat.note_waited();
                value
            }
        };
        match self.tx.send(value) {
            Ok(()) => {
                if let Some(lost) = self.stat.note_sent() {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(())
            }
            Err(std_mpsc::SendError(value)) => Err(value),
        }
    }
}

/// Receiver end of `threads`.
pub struct ThreadRx<T> {
    rx: std_mpsc::Receiver<T>,
    stat: QueueStat,
}

impl<T> ThreadRx<T> {
    pub fn recv(&self) -> Option<T> {
        let item = self.rx.recv().ok()?;
        self.stat.note_recv(1);
        Some(item)
    }

    pub fn recv_timeout(&self, wait: Duration) -> Result<T, RecvTimeoutError> {
        let item = self.rx.recv_timeout(wait)?;
        self.stat.note_recv(1);
        Ok(item)
    }

    pub fn try_recv(&self) -> Result<T, ThreadTryRecvError> {
        let item = self.rx.try_recv()?;
        self.stat.note_recv(1);
        Ok(item)
    }
}

impl<T> Drop for ThreadRx<T> {
    fn drop(&mut self) {
        let mut drained = 0u64;
        while self.rx.try_recv().is_ok() {
            drained += 1;
        }
        if drained > 0 {
            self.stat.note_recv(drained);
        }
    }
}

/// Sender end of `fanout`.
pub struct Fanout<T> {
    tx: broadcast::Sender<T>,
    stat: QueueStat,
}

impl<T> Clone for Fanout<T> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), stat: self.stat.clone() }
    }
}

impl<T: Clone> Fanout<T> {
    pub fn subscribe(&self) -> FanoutRx<T> {
        FanoutRx { rx: self.tx.subscribe(), stat: self.stat.clone() }
    }

    /// Never waits; returns how many readers received the item (0 if none).
    pub fn send(&self, value: T) -> usize {
        match self.tx.send(value) {
            Ok(readers) => {
                self.stat.note_fanout_sent(self.tx.len() as u64);
                readers
            }
            Err(_) => 0,
        }
    }
}

/// Receiver end of `fanout`.
pub struct FanoutRx<T> {
    rx: broadcast::Receiver<T>,
    stat: QueueStat,
}

impl<T: Clone> FanoutRx<T> {
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        match self.rx.recv().await {
            Ok(item) => {
                if let Some(lost) = self.stat.note_fanout_recv(self.rx.len() as u64) {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(item)
            }
            Err(RecvError::Lagged(skipped)) => {
                if self.stat.note_lagged(skipped) {
                    warn!(
                        "queue.full",
                        queue = self.stat.name(),
                        policy = Full::Drop.code(),
                        capacity = self.stat.capacity(),
                    );
                }
                Err(RecvError::Lagged(skipped))
            }
            Err(RecvError::Closed) => Err(RecvError::Closed),
        }
    }

    pub fn try_recv(&mut self) -> Result<T, FanoutTryRecvError> {
        match self.rx.try_recv() {
            Ok(item) => {
                if let Some(lost) = self.stat.note_fanout_recv(self.rx.len() as u64) {
                    info!("queue.clear", queue = self.stat.name(), lost = lost);
                }
                Ok(item)
            }
            Err(FanoutTryRecvError::Lagged(skipped)) => {
                if self.stat.note_lagged(skipped) {
                    warn!(
                        "queue.full",
                        queue = self.stat.name(),
                        policy = Full::Drop.code(),
                        capacity = self.stat.capacity(),
                    );
                }
                Err(FanoutTryRecvError::Lagged(skipped))
            }
            other => other,
        }
    }

    pub fn len(&self) -> usize {
        self.rx.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }

    pub fn resubscribe(&self) -> FanoutRx<T> {
        FanoutRx { rx: self.rx.resubscribe(), stat: self.stat.clone() }
    }
}
