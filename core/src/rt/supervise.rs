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
    /// A blip costs no more than a quarter second of reopening. Origin to be traced.
    const FIRST_MS: u64 = 250;
    /// The cap 0.8's Soniox stream doubled up to (1, 2, 4, then 8 s for ever).
    const LONGEST_MS: u64 = 8_000;
    /// A run this long was healthy, and the next wait starts short again. Origin to be traced.
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
        let wait_ms = backoff.next(started.elapsed().as_millis() as u64);
        sleep(Duration::from_millis(wait_ms)).await;
    }
}

/// A transcript that could not be written, for a reason that may pass, is tried again: first
/// after a minute (long enough for a network to come back), then ever more patiently.
pub const AGAIN_MS: u64 = 60_000;
/// The longest wait between two tries: an hour, so a failure that passes overnight is caught
/// before morning, and a process left running does not try more than 24 times a day.
pub const AGAIN_LONGEST_MS: u64 = 3_600_000;

/// Tries until it ends one way or the other: after a failure that may pass, again after
/// `AGAIN_MS`, then ever more patiently, up to `AGAIN_LONGEST_MS`.
pub async fn again<T, F>(mut attempt: impl FnMut() -> F) -> Result<T, End>
where
    F: Future<Output = Result<T, End>>,
{
    let mut wait_ms = AGAIN_MS;
    loop {
        match attempt().await {
            Err(End::Retry(_)) => tokio::time::sleep(Duration::from_millis(wait_ms)).await,
            done => return done,
        }
        wait_ms = (wait_ms * 2).min(AGAIN_LONGEST_MS);
    }
}
