//! Non-blocking main-thread heartbeat: pure `Beat` state machine plus `shell.beat` task
//! (`P1.md` §3.13.7, §3.13.11, `P1-24`).
//!
//! Posts at most one in-flight closure to the main thread every `BEAT_EVERY_MS` and never waits
//! on the main thread. If a beat does not come back within `BEAT_STALLED_MS`, logs
//! `shell.main_thread.stalled` once and increments `shell.beats.stalled`; when it returns, logs
//! `shell.main_thread.back` with the full stall duration and records the latency in
//! `shell.beat_ms`.

use std::time::Duration;
use zutalk_core::rt::signal::Register;
use zutalk_core::rt::stats::{Counter, Spread, Stats};
use zutalk_log::{Id, info, warn};

/// How often a beat is posted to the main thread (`PLAN.md` §4.5.2).
pub const BEAT_EVERY_MS: u64 = 1_000;
/// How long one beat may stay out before the main thread counts as stalled (`PLAN.md` §4.5.2).
pub const BEAT_STALLED_MS: u64 = 2_000;

/// A beat posted to the main thread, and whether it has come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    Idle,
    Out { seq: u64, sent_ms: u64 },
    Late { seq: u64, sent_ms: u64 },
}

impl Beat {
    /// Stable plain code for this state.
    pub const fn code(self) -> &'static str {
        match self {
            Beat::Idle => "Idle",
            Beat::Out { .. } => "Out",
            Beat::Late { .. } => "Late",
        }
    }
}

/// Input to the [`Beat`] state machine: the periodic timer ticked, or the main thread ran beat `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    Time,
    Back(u64),
}

/// Side effect requested by [`step`]: post beat `seq`, note latency, warn stalled, or report return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Post(u64),
    Took(u64),
    Stalled(u64),
    Back(u64),
}

/// Pure transition function for [`Beat`]: at most one beat is in flight at a time, `Stalled` is
/// emitted once per stall, and mismatched `Back` sequences are ignored (`P1-24`).
pub fn step(beat: Beat, tick: Tick, now_ms: u64) -> (Beat, Vec<Effect>) {
    match (beat, tick) {
        (Beat::Idle, Tick::Time) => {
            let seq = now_ms.saturating_add(1);
            (Beat::Out { seq, sent_ms: now_ms }, vec![Effect::Post(seq)])
        }
        (Beat::Idle, Tick::Back(_)) => (Beat::Idle, Vec::new()),
        (Beat::Out { seq, sent_ms }, Tick::Back(got)) if got == seq => {
            (Beat::Idle, vec![Effect::Took(now_ms.saturating_sub(sent_ms))])
        }
        (Beat::Out { seq, sent_ms }, Tick::Back(_)) => (Beat::Out { seq, sent_ms }, Vec::new()),
        (Beat::Out { seq, sent_ms }, Tick::Time) => {
            let waited_ms = now_ms.saturating_sub(sent_ms);
            if waited_ms >= BEAT_STALLED_MS {
                (Beat::Late { seq, sent_ms }, vec![Effect::Stalled(waited_ms)])
            } else {
                (Beat::Out { seq, sent_ms }, Vec::new())
            }
        }
        (Beat::Late { seq, sent_ms }, Tick::Time) => (Beat::Late { seq, sent_ms }, Vec::new()),
        (Beat::Late { seq, sent_ms }, Tick::Back(got)) if got == seq => {
            (Beat::Idle, vec![Effect::Back(now_ms.saturating_sub(sent_ms))])
        }
        (Beat::Late { seq, sent_ms }, Tick::Back(_)) => (Beat::Late { seq, sent_ms }, Vec::new()),
    }
}

/// Runs the shell's main-thread heartbeat loop until `post` returns `false` (app shutting down).
pub async fn run(post: impl Fn(Box<dyn FnOnce() + Send>) -> bool, stats: Stats) {
    let beat_ms = stats.spread("shell.beat_ms");
    let stalled = stats.counter("shell.beats.stalled");
    let reply = Register::new(0u64);
    let mut heard = reply.read();
    let origin = tokio::time::Instant::now();
    let mut beat = Beat::Idle;
    let mut interval = tokio::time::interval(Duration::from_millis(BEAT_EVERY_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let tick = tokio::select! {
            _ = interval.tick() => Tick::Time,
            next = heard.next() => match next {
                Some((_, seq)) => Tick::Back(*seq),
                None => return,
            },
        };
        let now_ms = origin.elapsed().as_millis() as u64;
        let (next_beat, effects) = step(beat, tick, now_ms);
        beat = next_beat;
        for effect in effects {
            match effect {
                Effect::Post(seq) => {
                    let reply = reply.clone();
                    if !post(Box::new(move || reply.set(seq))) {
                        return;
                    }
                }
                Effect::Took(ms) => beat_ms.note(ms),
                Effect::Stalled(waited_ms) => {
                    warn!("shell.main_thread.stalled", waited_ms = waited_ms);
                    stalled.add(1);
                }
                Effect::Back(stalled_ms) => {
                    info!("shell.main_thread.back", stalled_ms = stalled_ms);
                    beat_ms.note(stalled_ms);
                }
            }
        }
    }
}

/// Records an interface frame-gap report from `drawn` (`P1.md` §3.13.8, `P1-24`).
/// Emits `ui.frame.gap` and updates `ui.gap_ms` and `ui.gaps` only when `gaps > 0`.
pub fn note_frame_gap(window: &str, gap_ms: u32, gaps: u32, spread: &Spread, count: &Counter) {
    if gaps > 0 {
        warn!("ui.frame.gap", window = Id(window), gap_ms = gap_ms, n = gaps);
        spread.note(u64::from(gap_ms));
        count.add(u64::from(gaps));
    }
}
