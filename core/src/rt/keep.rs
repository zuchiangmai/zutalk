//! Generic bounded-queue batch writer (`P1.md` §3.6).
//!
//! Reads batches of up to [`KEEP_BATCH_ITEMS`] from a bounded queue and runs each batch in one
//! [`Tree::blocking`] call. Errors or panics discard the broken writer, account for the lost
//! batch (`keep.lost`, `keep.lost_batches`), and let [`Tree::keep`] reopen it under `Restarts`.

use crate::rt::queue::Rx;
use crate::rt::stats::{Counter, Spread};
use crate::rt::supervise::{End, Kept, Part, Restarts, Tree};
use std::sync::Arc;
use tokio::sync::Mutex;
use zutalk_log::warn;

/// The most items written in one blocking call: 50 audio blocks or draft lines finish in under a
/// millisecond while keeping each blocking call bounded (`P1.md` §3.6).
pub const KEEP_BATCH_ITEMS: usize = 50;

struct Keeper<T, W, F, S> {
    tree: Tree,
    part: &'static str,
    batch_task: &'static str,
    rx: Arc<Mutex<Rx<T>>>,
    writer: Option<W>,
    write: Arc<F>,
    finish: Arc<S>,
    batch_ms: Spread,
    lost_batches: Counter,
}

impl<T, W, F, S> Part for Keeper<T, W, F, S>
where
    T: Send + 'static,
    W: Send + 'static,
    F: Fn(&mut W, &[T]) -> Result<(), &'static str> + Send + Sync + 'static,
    S: Fn(&mut W) -> Result<(), &'static str> + Send + Sync + 'static,
{
    async fn run(&mut self) -> End {
        let mut rx = self.rx.lock().await;
        let mut batch = Vec::with_capacity(KEEP_BATCH_ITEMS);
        loop {
            batch.clear();
            let n = rx.recv_many(&mut batch, KEEP_BATCH_ITEMS).await;
            if n == 0 {
                // Every sender dropped and all queued items were written: flush and exit cleanly.
                let Some(mut writer) = self.writer.take() else {
                    return End::Done;
                };
                let finish = self.finish.clone();
                return match self
                    .tree
                    .blocking(self.batch_task, move || (finish)(&mut writer))
                    .await
                {
                    Ok(Ok(())) => End::Done,
                    Ok(Err(why)) => End::Retry(why),
                    Err(_) => End::Retry("panic"),
                };
            }
            let Some(mut writer) = self.writer.take() else {
                return End::Done;
            };
            let items = std::mem::take(&mut batch);
            let write = self.write.clone();
            let started = std::time::Instant::now();
            let res = self
                .tree
                .blocking(self.batch_task, move || match (write)(&mut writer, &items) {
                    Ok(()) => Ok((writer, items)),
                    Err(why) => Err(why),
                })
                .await;
            let took_ms = started.elapsed().as_millis() as u64;
            self.batch_ms.note(took_ms);
            match res {
                Ok(Ok((writer, mut recycled))) => {
                    self.writer = Some(writer);
                    recycled.clear();
                    batch = recycled;
                }
                Ok(Err(why)) => {
                    warn!("keep.lost", part = self.part, n = n, why = why);
                    self.lost_batches.add(1);
                    return End::Retry(why);
                }
                Err(_) => {
                    warn!("keep.lost", part = self.part, n = n, why = "panic");
                    self.lost_batches.add(1);
                    return End::Retry("panic");
                }
            }
        }
    }
}

/// Starts a supervised batch-writing part named `part` on `tree`.
///
/// The caller creates the queue via `rt::queue::bounded` (so its capacity constant and `Full`
/// policy live at the call site) and passes its receiver `rx` here. Each open, batch write, and
/// final sync runs as a separate [`Tree::blocking`] call named `batch_task`.
#[allow(clippy::too_many_arguments)]
pub fn start<T, W, O, F, S>(
    tree: &Tree,
    part: &'static str,
    batch_task: &'static str,
    rx: Rx<T>,
    batch_ms: Spread,
    restarts: Restarts,
    open: O,
    write: F,
    finish: S,
) -> Kept
where
    T: Send + 'static,
    W: Send + 'static,
    O: Fn() -> Result<W, &'static str> + Send + Sync + 'static,
    F: Fn(&mut W, &[T]) -> Result<(), &'static str> + Send + Sync + 'static,
    S: Fn(&mut W) -> Result<(), &'static str> + Send + Sync + 'static,
{
    let rx = Arc::new(Mutex::new(rx));
    let open = Arc::new(open);
    let write = Arc::new(write);
    let finish = Arc::new(finish);
    let lost_batches = tree.stats().counter("keep.lost_batches");
    let worker_tree = tree.clone();

    tree.keep(
        part,
        move || {
            let tree = worker_tree.clone();
            let rx = rx.clone();
            let open = open.clone();
            let write = write.clone();
            let finish = finish.clone();
            let batch_ms = batch_ms.clone();
            let lost_batches = lost_batches.clone();
            async move {
                let writer = match tree.blocking(batch_task, move || (open)()).await {
                    Ok(Ok(writer)) => writer,
                    Ok(Err(why)) => return Err(End::Retry(why)),
                    Err(_) => return Err(End::Retry("panic")),
                };
                Ok(Keeper {
                    tree,
                    part,
                    batch_task,
                    rx,
                    writer: Some(writer),
                    write,
                    finish,
                    batch_ms,
                    lost_batches,
                })
            }
        },
        |_| {},
        restarts,
    )
}
