//! One recovery rule for everything that can break mid-recording: an audio source, a
//! connection to the transcription service.
//!
//! When a part stops, say why, then open it again from scratch. Nothing is repaired in
//! place. The time that was lost is accounted for by whoever owns the timeline: audio
//! is padded with silence by the capture clock, and text marks a gap.

use std::time::Duration;
use tokio::time::{Instant, sleep};

/// Why a part stopped running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// It was asked to stop.
    Done,
    /// It broke, or its device or format changed. Opening it again may work.
    Retry(&'static str),
    /// Opening it again cannot help: no permission, a key that was refused.
    Fatal(&'static str),
}

/// Something that runs until it ends.
pub trait Part {
    fn run(&mut self) -> impl Future<Output = End>;
}

/// The wait before opening again, as a value: the shortest after a run that counts as
/// healthy, else twice the last, up to the longest. Time is handed in.
#[derive(Debug, Default, Clone, Copy)]
pub struct Backoff(u64);

impl Backoff {
    const FIRST_MS: u64 = 250;
    const LONGEST_MS: u64 = 8_000;
    const HEALTHY_MS: u64 = 30_000;

    /// The wait in ms after a part that ran for `ran_ms`.
    pub fn next(&mut self, ran_ms: u64) -> u64 {
        let (first, longest) = (Self::FIRST_MS, Self::LONGEST_MS);
        self.0 =
            if ran_ms >= Self::HEALTHY_MS { first } else { (self.0 * 2).clamp(first, longest) };
        self.0
    }
}

/// Keeps a part running. `broke` hears the reason each time it has to be opened again.
pub async fn supervise<P: Part>(
    mut open: impl AsyncFnMut() -> Result<P, End>,
    mut broke: impl FnMut(&'static str),
) -> End {
    let mut backoff = Backoff::default();
    loop {
        let started = Instant::now();
        let end = match open().await {
            Ok(mut part) => part.run().await,
            Err(end) => end,
        };
        let End::Retry(why) = end else { return end };
        broke(why);
        sleep(Duration::from_millis(backoff.next(started.elapsed().as_millis() as u64))).await;
    }
}
