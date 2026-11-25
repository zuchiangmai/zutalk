//! Named, tracked server tasks: ledger mutations outlive a dropped HTTP connection while still
//! reporting their lifecycle and draining cleanly on shutdown.

use std::future::Future;
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use zutalk_log::info;

#[derive(Default)]
pub struct Tasks {
    tracker: TaskTracker,
}

impl Tasks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a named server task inside the tracker, carrying the active log capture and recording
    /// its start and completion.
    pub fn spawn<F>(&self, name: &'static str, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(zutalk_log::carry(async move {
            let started = Instant::now();
            info!("task.start", tree = "server", task = name);
            let out = fut.await;
            let ran_ms = started.elapsed().as_millis() as u64;
            info!("task.end", tree = "server", task = name, code = "done", ran_ms = ran_ms);
            out
        }))
    }

    /// Close the tracker and wait for all in-flight ledger tasks to finish.
    pub async fn wait(&self) {
        self.tracker.close();
        self.tracker.wait().await;
    }
}
