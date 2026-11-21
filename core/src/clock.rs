//! The capture clock, the one clock the app keeps time by. Audio that arrives whenever it
//! likes becomes 100 ms blocks whose position is the recording's time, never behind the wall
//! clock: a source that stalls or restarts leaves a hole, and the hole is silence.

/// Samples per second, every source resampled to it; and in one block, 100 ms of mono.
pub const RATE: u64 = 16_000;
pub const BLOCK: usize = 1_600;

/// 100 ms of audio starting at `t_ms` on the capture clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub t_ms: u32,
    pub pcm: Vec<i16>,
}

/// What to do when the audio is behind the wall clock: nothing, for sources deliver in
/// bursts; pad this many ms of silence before what just arrived; or end the recording here,
/// for the process was not running (the lid was closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    Keep,
    Pad(u32),
    End,
}

/// The wall clock, read here and nowhere else. It keeps counting while the computer sleeps,
/// so a closed lid shows up as a gap, and the gap policy decides what that means.
pub fn wall_ms() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    since.map_or(0, |since| since.as_millis() as u64)
}

/// Decides what a lag of `behind_ms` means. A plain function, so tests can bring their own.
pub type GapPolicy = fn(behind_ms: u64) -> Gap;

/// The policy the app records with.
pub fn wall_clock_policy(behind_ms: u64) -> Gap {
    // TODO(human): decide what a lag of `behind_ms` means — Keep, Pad(ms) or End.
    let _ = behind_ms;
    todo!("the gap policy is the owner's call")
}

pub struct Clock {
    policy: GapPolicy,
    origin_ms: Option<u64>, // wall time of the first sample
    blocks: u32,
    pending: Vec<i16>,
}

impl Clock {
    pub fn new(policy: GapPolicy) -> Self {
        Self { policy, origin_ms: None, blocks: 0, pending: Vec::new() }
    }

    /// Audio that finished arriving at wall time `now_ms`. Completed blocks go to `out`.
    /// Returns `false` when the policy ends the recording; nothing is added in that case.
    pub fn push(&mut self, pcm: &[i16], now_ms: u64, out: &mut Vec<Block>) -> bool {
        let chunk_ms = pcm.len() as u64 * 1000 / RATE;
        let origin = *self.origin_ms.get_or_insert(now_ms.saturating_sub(chunk_ms));
        // Where the wall clock says this audio starts, against how far we have got.
        let starts_at = now_ms.saturating_sub(origin).saturating_sub(chunk_ms);
        let got_to = self.blocks as u64 * 100 + self.pending.len() as u64 * 1000 / RATE;
        match (self.policy)(starts_at.saturating_sub(got_to)) {
            Gap::Keep => {}
            Gap::Pad(ms) => self.take(&vec![0; (ms as u64 * RATE / 1000) as usize], out),
            Gap::End => return false,
        }
        self.take(pcm, out);
        true
    }

    /// No audio arrived. Call this on a timer so that a stalled source is still padded.
    pub fn tick(&mut self, now_ms: u64, out: &mut Vec<Block>) -> bool {
        self.push(&[], now_ms, out)
    }

    fn take(&mut self, pcm: &[i16], out: &mut Vec<Block>) {
        self.pending.extend_from_slice(pcm);
        while self.pending.len() >= BLOCK {
            out.push(Block { t_ms: self.blocks * 100, pcm: self.pending.drain(..BLOCK).collect() });
            self.blocks += 1;
        }
    }
}
