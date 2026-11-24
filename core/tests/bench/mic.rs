//! Fake microphone (`Hear` port) driven by `tokio::time` (`P1.md` §3.10).
//!
//! Each 100 ms block encodes its 0-based sequence number in base-64 across its first 6 samples
//! (`0..=63`), whose RMS (< 4) never triggers speech detection (`lane::quiet` threshold is 100).
//! Loud and silent templates are built once; each block clones a template and stamps its header.

use super::speech::{BLOCK_MS, Speech};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;
use zutalk_core::capture::audio::{Capture, Hear, Hearing, Notice, Sink, Source, Tell};
use zutalk_core::capture::clock::{BLOCK, Block, Gap};

/// Interval between consecutive 100 ms audio blocks delivered by the fake microphone.
pub const MIC_TICK_MS: u64 = BLOCK_MS as u64;
/// Half-block phase shift so block and service events land midway between the core's 100 ms
/// frame ticks, eliminating same-millisecond `select!` tie-breaking on a paused clock.
const PHASE_OFFSET_MS: u64 = MIC_TICK_MS / 2;
/// Number of leading samples carrying the base-64 block sequence number (6 * 6 = 36 bits).
const HEADER_SAMPLES: usize = 6;
/// Amplitude of loud speech samples: RMS ~ 8000, well above `lane::quiet`'s 100.0 threshold.
const LOUD_AMPLITUDE: i16 = 8_000;

/// Encodes `block_idx` in base-64 into the first 6 samples of `pcm`.
pub fn encode_block_index(pcm: &mut [i16], block_idx: u32) {
    let mut n = block_idx;
    for sample in pcm.iter_mut().take(HEADER_SAMPLES) {
        *sample = (n & 63) as i16;
        n >>= 6;
    }
}

/// Decodes the base-64 block sequence number from the first 12 bytes (6 little-endian `i16`s).
pub fn decode_block_index(pcm_le: &[u8]) -> u32 {
    let mut idx = 0u32;
    for k in 0..HEADER_SAMPLES {
        let sample = i16::from_le_bytes([pcm_le[2 * k], pcm_le[2 * k + 1]]) as u32;
        idx |= (sample & 63) << (6 * k);
    }
    idx
}

enum MicCmd {
    Burst(usize),
    End,
}

struct MicState {
    speech: Arc<Speech>,
    paused: AtomicBool,
    stalled: AtomicBool,
    blocks: AtomicU64,
    started_at: Mutex<Option<Instant>>,
    commands: Mutex<VecDeque<MicCmd>>,
    wake: Notify,
}

/// Controller held by a test to inspect or perturb the fake microphone.
#[derive(Clone)]
pub struct Mic {
    state: Arc<MicState>,
}

impl Mic {
    pub fn new(speech: Arc<Speech>) -> Self {
        Self {
            state: Arc::new(MicState {
                speech,
                paused: AtomicBool::new(false),
                stalled: AtomicBool::new(false),
                blocks: AtomicU64::new(0),
                started_at: Mutex::new(None),
                commands: Mutex::new(VecDeque::new()),
                wake: Notify::new(),
            }),
        }
    }

    /// Virtual instant when the latest capture started.
    pub fn started_at(&self) -> Option<Instant> {
        *self.state.started_at.lock().expect("mic started_at lock")
    }

    /// Total 100 ms blocks delivered by the microphone so far.
    pub fn blocks(&self) -> u64 {
        self.state.blocks.load(Ordering::Acquire)
    }

    /// Resets per-recording counters before a second recording on the same `Bench`.
    pub fn reset_for_next(&self) {
        self.state.blocks.store(0, Ordering::Release);
        self.state.paused.store(false, Ordering::Release);
        self.state.stalled.store(false, Ordering::Release);
        *self.state.started_at.lock().expect("mic started_at lock") = None;
        self.state.commands.lock().expect("mic cmds lock").clear();
    }

    /// Delivers `n` blocks immediately in one burst on the capture clock.
    pub fn burst(&self, n: usize) {
        self.state.commands.lock().expect("mic cmds lock").push_back(MicCmd::Burst(n));
        self.state.wake.notify_waiters();
    }

    /// Holds or releases block delivery without pausing the recording (for capture stall tests).
    #[allow(dead_code)]
    pub fn stall(&self, stalled: bool) {
        self.state.stalled.store(stalled, Ordering::Release);
        self.state.wake.notify_waiters();
    }

    /// Ends the current capture as if the gap policy ended it (`Notice::Ended`).
    #[allow(dead_code)]
    pub fn end(&self) {
        self.state.commands.lock().expect("mic cmds lock").push_back(MicCmd::End);
        self.state.wake.notify_waiters();
    }

    /// Builds the `Hear` port closure to install in `Config.hear`.
    pub fn hear(&self) -> Hear {
        let state = self.state.clone();
        Arc::new(move |source: Source, sink: Sink, tell: Tell| {
            if let Source::File(_) = source {
                let capture = Capture::start(source, |_| Gap::Keep, sink, tell)?;
                return Ok(Box::new(capture));
            }
            state.paused.store(false, Ordering::Release);
            state.blocks.store(0, Ordering::Release);

            let stopped = Arc::new(AtomicBool::new(false));
            let stop_wake = Arc::new(Notify::new());
            let shared_sink = Arc::new(Mutex::new(Some(sink)));
            let shared_tell = Arc::new(Mutex::new(Some(tell)));

            let driver_state = state.clone();
            let driver_stopped = stopped.clone();
            let driver_stop_wake = stop_wake.clone();
            let driver_sink = shared_sink.clone();
            let driver_tell = shared_tell.clone();

            tokio::spawn(async move {
                run_mic(driver_state, driver_stopped, driver_stop_wake, driver_sink, driver_tell)
                    .await;
            });

            Ok(Box::new(FakeHearing {
                state: state.clone(),
                stopped,
                stop_wake,
                sink: shared_sink,
                tell: shared_tell,
            }))
        })
    }
}

struct FakeHearing {
    state: Arc<MicState>,
    stopped: Arc<AtomicBool>,
    stop_wake: Arc<Notify>,
    sink: Arc<Mutex<Option<Sink>>>,
    tell: Arc<Mutex<Option<Tell>>>,
}

impl Hearing for FakeHearing {
    fn pause(&self) {
        self.state.paused.store(true, Ordering::Release);
        self.state.wake.notify_waiters();
    }

    fn resume(&self) {
        self.state.paused.store(false, Ordering::Release);
        self.state.wake.notify_waiters();
    }
}

impl Drop for FakeHearing {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.stop_wake.notify_waiters();
        // Drop the sink synchronously (matching real Capture::drop joining its pump thread) so
        // the tape Writer is closed and flushed before `Task::finish` runs `tape::recover`.
        let _ = self.sink.lock().map(|mut guard| guard.take());
        let _ = self.tell.lock().map(|mut guard| guard.take());
    }
}

async fn run_mic(
    state: Arc<MicState>,
    stopped: Arc<AtomicBool>,
    stop_wake: Arc<Notify>,
    sink: Arc<Mutex<Option<Sink>>>,
    tell: Arc<Mutex<Option<Tell>>>,
) {
    // Precompute templates once: each block only clones a template and stamps its 6-sample header.
    let silent_template = vec![0i16; BLOCK];
    let mut loud_template = vec![0i16; BLOCK];
    for (i, sample) in loud_template.iter_mut().enumerate().skip(HEADER_SAMPLES) {
        *sample = if (i / 16) % 2 == 0 { LOUD_AMPLITUDE } else { -LOUD_AMPLITUDE };
    }

    let mut next_block: u32 = 0;

    // Offset start by half a frame tick so audio/service events never tie with the core's 100 ms
    // `frame.tick()` on a paused clock.
    tokio::select! {
        biased;
        _ = stop_wake.notified() => return,
        _ = tokio::time::sleep(Duration::from_millis(PHASE_OFFSET_MS)) => {}
    }
    *state.started_at.lock().expect("mic started_at lock") = Some(Instant::now());

    let emit_one = |idx: u32| -> bool {
        let mut pcm = if state.speech.is_loud(idx as usize) {
            loud_template.clone()
        } else {
            silent_template.clone()
        };
        encode_block_index(&mut pcm, idx);
        let block = Block { t_ms: idx * BLOCK_MS, pcm };
        let mut guard = sink.lock().expect("mic sink lock");
        let Some(ref mut cb) = *guard else {
            return false;
        };
        cb(block);
        state.blocks.fetch_add(1, Ordering::Release);
        true
    };

    while !stopped.load(Ordering::Acquire) {
        // Process any queued control commands first.
        while let Some(cmd) = state.commands.lock().expect("mic cmds lock").pop_front() {
            match cmd {
                MicCmd::Burst(n) => {
                    for _ in 0..n {
                        if stopped.load(Ordering::Acquire) || !emit_one(next_block) {
                            return;
                        }
                        next_block += 1;
                    }
                }
                MicCmd::End => {
                    if let Some(ref mut cb) = *tell.lock().expect("mic tell lock") {
                        cb(Notice::Ended);
                    }
                    return;
                }
            }
        }

        if state.paused.load(Ordering::Acquire) || state.stalled.load(Ordering::Acquire) {
            tokio::select! {
                _ = stop_wake.notified() => break,
                _ = state.wake.notified() => continue,
            }
        }

        let next_tick = tokio::time::sleep(Duration::from_millis(MIC_TICK_MS));
        tokio::select! {
            biased;
            _ = stop_wake.notified() => break,
            _ = state.wake.notified() => continue,
            _ = next_tick => {
                if stopped.load(Ordering::Acquire)
                    || state.paused.load(Ordering::Acquire)
                    || state.stalled.load(Ordering::Acquire)
                {
                    continue;
                }
                if !emit_one(next_block) {
                    break;
                }
                next_block += 1;
            }
        }
    }
}
