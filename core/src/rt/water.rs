//! Watermarks, stage budgets, and pure stall judgment (`P1.md` §3.5).
//!
//! Each running part reports its progress into an atomic [`Gauge`]. A watcher samples gauges at
//! 1 Hz with time handed in as a parameter, asks [`judge`] for each stage's [`Health`] and
//! [`Action`], and asks [`due`] what changed, what needs a snapshot, and what action to take now.

use serde::Serialize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use ts_rs::TS;

/// Maximum distinct static reason codes interned across a process for lock-free gauges; P1 has
/// under eighty codes in total, so 512 leaves wide headroom without heap allocation.
const GAUGE_MOST_CODES: usize = 512;
const ENDED_EMPTY: usize = usize::MAX;

static GAUGE_CODES: [OnceLock<&'static str>; GAUGE_MOST_CODES] =
    [const { OnceLock::new() }; GAUGE_MOST_CODES];

fn intern_code(code: &'static str) -> usize {
    if code.is_empty() {
        return 0;
    }
    for (idx, slot) in GAUGE_CODES.iter().enumerate() {
        if let Some(&existing) = slot.get() {
            if existing == code {
                return idx + 1;
            }
            continue;
        }
        if slot.set(code).is_ok() {
            return idx + 1;
        }
        if slot.get().copied() == Some(code) {
            return idx + 1;
        }
    }
    0
}

fn lookup_code(slot: usize) -> &'static str {
    if slot == 0 || slot > GAUGE_MOST_CODES {
        ""
    } else {
        GAUGE_CODES[slot - 1].get().copied().unwrap_or("")
    }
}

/// Lock-free, allocation-free watermark reported by one running part.
#[derive(Debug, Default)]
pub struct Gauge {
    at: AtomicU64,
    why: AtomicUsize,
    ended: AtomicUsize,
}

impl Gauge {
    /// Creates a gauge starting at position `0`, with an empty reason and not ended.
    pub const fn new() -> Self {
        Self { at: AtomicU64::new(0), why: AtomicUsize::new(0), ended: AtomicUsize::new(0) }
    }

    /// Updates the watermark position with a single atomic store.
    pub fn set(&self, at: u64) {
        self.at.store(at, Ordering::Relaxed);
    }

    /// Records why this part is currently not advancing (`""` clears it).
    pub fn because(&self, why: &'static str) {
        self.why.store(intern_code(why), Ordering::Release);
    }

    /// Marks that this part will not advance again, and why (`"done"` or a failure code).
    pub fn ended(&self, code: &'static str) {
        let encoded = if code.is_empty() { ENDED_EMPTY } else { intern_code(code) };
        self.ended.store(encoded, Ordering::Release);
    }

    /// Clears a prior `ended` marker when a stopped part is started again (e.g., `Reconnect`).
    pub fn clear_ended(&self) {
        self.ended.store(0, Ordering::Release);
    }

    /// Reads `(at, why, ended)` as one row for [`sample`].
    pub fn read(&self) -> (u64, &'static str, Option<&'static str>) {
        let at = self.at.load(Ordering::Relaxed);
        let why = lookup_code(self.why.load(Ordering::Acquire));
        let ended = match self.ended.load(Ordering::Acquire) {
            0 => None,
            ENDED_EMPTY => Some(""),
            slot => Some(lookup_code(slot)),
        };
        (at, why, ended)
    }
}

/// One stage along the recording and caption pipeline (`P1.md` §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
pub enum Stage {
    Capture,
    Tape,
    Draft,
    Sent(u8),
    Heard(u8),
    Final(u8),
    Frame,
    Shell,
    Screen,
}

impl Stage {
    /// Stable plain code of this stage for structured log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Stage::Capture => "Capture",
            Stage::Tape => "Tape",
            Stage::Draft => "Draft",
            Stage::Sent(_) => "Sent",
            Stage::Heard(_) => "Heard",
            Stage::Final(_) => "Final",
            Stage::Frame => "Frame",
            Stage::Shell => "Shell",
            Stage::Screen => "Screen",
        }
    }

    /// Caption lane index (`0..`), or `None` for stages shared across the recording.
    pub const fn lane(&self) -> Option<u8> {
        match *self {
            Stage::Sent(n) | Stage::Heard(n) | Stage::Final(n) => Some(n),
            Stage::Capture
            | Stage::Tape
            | Stage::Draft
            | Stage::Frame
            | Stage::Shell
            | Stage::Screen => None,
        }
    }
}

/// Judged health of one stage (`P1.md` §3.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "is")]
pub enum Health {
    Live,
    Slow,
    Stalled { why: String },
    Dead { why: String },
}

impl Health {
    /// Stable plain code of this health state for structured log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Health::Live => "Live",
            Health::Slow => "Slow",
            Health::Stalled { .. } => "Stalled",
            Health::Dead { .. } => "Dead",
        }
    }

    /// Reason code carried by `Stalled` or `Dead`, or `""` when `Live` or `Slow`.
    pub fn why(&self) -> &str {
        match self {
            Health::Stalled { why } | Health::Dead { why } => why.as_str(),
            Health::Live | Health::Slow => "",
        }
    }

    /// Severity rank (`Live < Slow < Stalled < Dead`) for combining column health (`P1-17`).
    pub const fn rank(&self) -> u8 {
        match self {
            Health::Live => 0,
            Health::Slow => 1,
            Health::Stalled { .. } => 2,
            Health::Dead { .. } => 3,
        }
    }

    /// Returns the worse of two health verdicts (`Dead` > `Stalled` > `Slow` > `Live`).
    pub fn worst(a: &Health, b: &Health) -> Health {
        if b.rank() > a.rank() { b.clone() } else { a.clone() }
    }
}

/// Recovery or reporting action attached to a [`Verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Reopen(Stage),
    Tell(&'static str),
    End(&'static str),
}

impl Action {
    /// Stable plain code of this action kind for structured log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Action::None => "none",
            Action::Reopen(_) => "reopen",
            Action::Tell(_) => "tell",
            Action::End(_) => "end",
        }
    }
}

/// Sampled watermark of one [`Stage`] at `Water.now_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Mark {
    pub stage: Stage,
    #[ts(type = "number")]
    pub at: u64,
    #[ts(type = "number")]
    pub moved_ms: u64,
    pub why: &'static str,
    pub ended: Option<&'static str>,
}

/// Point-in-time watermarks and health verdicts across all active stages (`P1.md` §3.5).
#[derive(Debug, Clone, Default, Serialize, TS)]
pub struct Water {
    #[ts(type = "number")]
    pub now_ms: u64,
    #[ts(type = "number")]
    pub clock_ms: u64,
    pub paused: bool,
    pub marks: Vec<Mark>,
    pub health: Vec<(Stage, Health)>,
    #[serde(skip)]
    #[ts(skip)]
    acted: Vec<(Stage, u64)>,
    #[serde(skip)]
    #[ts(skip)]
    told: Vec<(Stage, &'static str)>,
    #[serde(skip)]
    #[ts(skip)]
    snapped: Vec<(Stage, String)>,
}

impl PartialEq for Water {
    fn eq(&self, other: &Self) -> bool {
        self.now_ms == other.now_ms
            && self.clock_ms == other.clock_ms
            && self.paused == other.paused
            && self.marks == other.marks
            && self.health == other.health
    }
}

impl Eq for Water {}

/// One stage's judged [`Health`] and [`Action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub stage: Stage,
    pub health: Health,
    pub action: Action,
}

/// Where a stage's upstream watermark comes from in [`BUDGETS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// No upstream stage: never behind on its own (`Frame`).
    None,
    /// The active recording clock `Water.clock_ms` (`Capture`).
    Clock,
    /// A specific stage (`Capture`, `Final(0)`, `Frame`, `Shell`).
    Stage(Stage),
    /// `Stage::Sent(n)` of the same caption lane (`Heard(n)`).
    LaneSent,
    /// `Stage::Heard(n)` of the same caption lane (`Final(n)`).
    LaneHeard,
}

/// What a budget row asks the watcher to do when a stage reaches `stalled_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnStall {
    None,
    Reopen,
    Tell,
    End,
}

/// Liveness thresholds and stall response for one kind of [`Stage`] (`P1.md` §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub stage: Stage,
    pub upstream: Upstream,
    pub slow_lag_ms: Option<u64>,
    pub slow_idle_ms: Option<u64>,
    pub stalled_ms: Option<u64>,
    pub on_stall: OnStall,
    pub why: &'static str,
}

/// Per-stage liveness budgets (`P1.md` §3.5).
///
/// Origins of the numbers:
/// - `Heard` `stalled_ms = 10_000`: `docs/LESSONS.md` ("静音时服务仍每 1.0–1.2 秒报一次进度"), so ten
///   seconds without progress while audio was sent means a dead connection.
/// - `Final` `slow_lag_ms = 30_000`, no `stalled_ms`: `docs/LESSONS.md` caps unfinalized dense
///   speech at 25 s (`overlong`), so 30 s marks slow finalization without falsely stalling.
/// - `Screen` has `slow_idle_ms = 5_000` and no `stalled_ms`: a hidden window may stop ticking
///   `requestAnimationFrame`, so it is at most `Slow`.
/// - All remaining thresholds (`Capture` 1 s / 10 s, `Tape` 2 s / 10 s, `Sent` 2 s / 20 s,
///   `Draft` 30 s, `Shell` 2 s / 5 s) are provisional (`P1.md` §4 item 3).
pub const BUDGETS: &[Budget] = &[
    Budget {
        stage: Stage::Capture,
        upstream: Upstream::Clock,
        slow_lag_ms: None,
        slow_idle_ms: Some(1_000),
        stalled_ms: Some(10_000),
        on_stall: OnStall::End,
        why: "audio.stalled",
    },
    Budget {
        stage: Stage::Tape,
        upstream: Upstream::Stage(Stage::Capture),
        slow_lag_ms: Some(2_000),
        slow_idle_ms: None,
        stalled_ms: Some(10_000),
        on_stall: OnStall::Tell,
        why: "tape.slow",
    },
    Budget {
        stage: Stage::Sent(0),
        upstream: Upstream::Stage(Stage::Capture),
        slow_lag_ms: Some(2_000),
        slow_idle_ms: None,
        stalled_ms: Some(20_000),
        on_stall: OnStall::Tell,
        why: "",
    },
    Budget {
        stage: Stage::Heard(0),
        upstream: Upstream::LaneSent,
        slow_lag_ms: Some(3_000),
        slow_idle_ms: None,
        stalled_ms: Some(10_000),
        on_stall: OnStall::Reopen,
        why: "silent",
    },
    Budget {
        stage: Stage::Final(0),
        upstream: Upstream::LaneHeard,
        slow_lag_ms: Some(30_000),
        slow_idle_ms: None,
        stalled_ms: None,
        on_stall: OnStall::None,
        why: "",
    },
    Budget {
        stage: Stage::Draft,
        upstream: Upstream::Stage(Stage::Final(0)),
        slow_lag_ms: None,
        slow_idle_ms: None,
        stalled_ms: Some(30_000),
        on_stall: OnStall::Tell,
        why: "draft.behind",
    },
    Budget {
        stage: Stage::Frame,
        upstream: Upstream::None,
        slow_lag_ms: None,
        slow_idle_ms: None,
        stalled_ms: None,
        on_stall: OnStall::None,
        why: "",
    },
    Budget {
        stage: Stage::Shell,
        upstream: Upstream::Stage(Stage::Frame),
        slow_lag_ms: None,
        slow_idle_ms: Some(2_000),
        stalled_ms: Some(5_000),
        on_stall: OnStall::None,
        why: "shell",
    },
    Budget {
        stage: Stage::Screen,
        upstream: Upstream::Stage(Stage::Shell),
        slow_lag_ms: None,
        slow_idle_ms: Some(5_000),
        stalled_ms: None,
        on_stall: OnStall::None,
        why: "",
    },
];

/// Pure: builds the next [`Water`] from `before` and the newly read positions, preserving
/// `moved_ms` for any stage whose `at` has not advanced and setting `now_ms` otherwise.
pub fn sample(
    before: &Water,
    at: &[(Stage, u64, &'static str, Option<&'static str>)],
    now_ms: u64,
    clock_ms: u64,
    paused: bool,
) -> Water {
    let marks = at
        .iter()
        .map(|&(stage, pos, why, ended)| {
            // A lane restarted via `Reconnect` clears `ended` on `Sent(n)`: reset `moved_ms` for
            // every stage of that lane so offline duration is not counted as post-reconnect idle.
            let reopened_lane = stage.lane().is_some_and(|n| {
                let was_ended = before
                    .marks
                    .iter()
                    .find(|m| m.stage == Stage::Sent(n))
                    .is_some_and(|m| m.ended.is_some());
                let now_live = at
                    .iter()
                    .find(|(s, ..)| *s == Stage::Sent(n))
                    .is_some_and(|(_, _, _, e)| e.is_none());
                was_ended && now_live
            });
            // Resuming from a pause gives each stage a fresh clock so paused duration is never
            // counted as idle time.
            let moved_ms = if (before.paused && !paused) || reopened_lane {
                now_ms
            } else {
                match before.marks.iter().find(|m| m.stage == stage) {
                    Some(prev) if prev.at == pos => prev.moved_ms,
                    _ => now_ms,
                }
            };
            Mark { stage, at: pos, moved_ms, why, ended }
        })
        .collect();
    Water {
        now_ms,
        clock_ms,
        paused,
        marks,
        health: Vec::new(),
        acted: before.acted.clone(),
        told: before.told.clone(),
        snapped: before.snapped.clone(),
    }
}

fn find_budget(stage: Stage, budgets: &[Budget]) -> Option<&Budget> {
    let code = stage.code();
    budgets.iter().find(|b| b.stage.code() == code)
}

/// Static reason code for `stage`'s judged `health`, drawn from `BUDGETS` and `water.marks`.
pub fn verdict_why(water: &Water, stage: Stage, health: &Health) -> &'static str {
    match health {
        Health::Live | Health::Slow => "",
        Health::Dead { .. } => {
            water.marks.iter().find(|m| m.stage == stage).and_then(|m| m.ended).unwrap_or("")
        }
        Health::Stalled { .. } => {
            let budget_why = find_budget(stage, BUDGETS).map_or("", |b| b.why);
            if !budget_why.is_empty() {
                budget_why
            } else {
                water.marks.iter().find(|m| m.stage == stage).map_or("", |m| m.why)
            }
        }
    }
}

fn upstream_at(water: &Water, stage: Stage, upstream: Upstream) -> Option<u64> {
    let mark_at = |target: Stage| water.marks.iter().find(|m| m.stage == target).map(|m| m.at);
    match upstream {
        Upstream::None => None,
        Upstream::Clock => (water.clock_ms > 0).then_some(water.clock_ms),
        Upstream::Stage(target) => mark_at(target),
        Upstream::LaneSent => stage.lane().and_then(|n| mark_at(Stage::Sent(n))),
        Upstream::LaneHeard => stage.lane().and_then(|n| mark_at(Stage::Heard(n))),
    }
}

/// Pure: evaluates every mark in `water` against `budgets`.
pub fn judge(water: &Water, budgets: &[Budget]) -> Vec<Verdict> {
    if water.paused {
        return water
            .marks
            .iter()
            .map(|m| Verdict { stage: m.stage, health: Health::Live, action: Action::None })
            .collect();
    }

    water
        .marks
        .iter()
        .map(|mark| {
            if let Some(code) = mark.ended {
                if code == "done" {
                    return Verdict {
                        stage: mark.stage,
                        health: Health::Live,
                        action: Action::None,
                    };
                }
                return Verdict {
                    stage: mark.stage,
                    health: Health::Dead { why: code.to_owned() },
                    action: Action::Tell(code),
                };
            }

            let Some(budget) = find_budget(mark.stage, budgets) else {
                return Verdict { stage: mark.stage, health: Health::Live, action: Action::None };
            };

            let behind = upstream_at(water, mark.stage, budget.upstream)
                .map_or(0, |up| up.saturating_sub(mark.at));
            let idle = if behind > 0 { water.now_ms.saturating_sub(mark.moved_ms) } else { 0 };

            if let Some(stalled_ms) = budget.stalled_ms
                && idle >= stalled_ms
            {
                let why = if budget.why.is_empty() { mark.why } else { budget.why };
                let action = match budget.on_stall {
                    OnStall::None => Action::None,
                    OnStall::Reopen => Action::Reopen(mark.stage),
                    OnStall::Tell => Action::Tell(why),
                    OnStall::End => Action::End(why),
                };
                return Verdict {
                    stage: mark.stage,
                    health: Health::Stalled { why: why.to_owned() },
                    action,
                };
            }

            if budget.slow_lag_ms.is_some_and(|lag| behind >= lag)
                || budget.slow_idle_ms.is_some_and(|lim| idle >= lim)
            {
                return Verdict { stage: mark.stage, health: Health::Slow, action: Action::None };
            }

            Verdict { stage: mark.stage, health: Health::Live, action: Action::None }
        })
        .collect()
}

/// What the watcher owes on this tick: log a health transition, capture a stall snapshot, or execute
/// a recovery/reporting action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    Changed { stage: Stage, from: Health, to: Health },
    Snapshot { stage: Stage },
    Action(Action),
}

/// Pure: compares `before` and `verdicts`, records health and action history into `after`, and
/// returns what the watcher should emit or execute on this tick.
pub fn due(before: &Water, after: &mut Water, verdicts: &[Verdict]) -> Vec<Due> {
    let mut out = Vec::new();
    after.health = Vec::with_capacity(verdicts.len());
    after.acted.retain(|(s, _)| verdicts.iter().any(|v| v.stage == *s));

    for verdict in verdicts {
        let prev = before
            .health
            .iter()
            .find(|(s, _)| *s == verdict.stage)
            .map_or(Health::Live, |(_, h)| h.clone());

        if prev != verdict.health {
            out.push(Due::Changed { stage: verdict.stage, from: prev, to: verdict.health.clone() });
        }

        match &verdict.health {
            Health::Stalled { why } | Health::Dead { why } => {
                if !after.snapped.iter().any(|(s, w)| *s == verdict.stage && w == why) {
                    after.snapped.push((verdict.stage, why.clone()));
                    out.push(Due::Snapshot { stage: verdict.stage });
                }
            }
            Health::Live | Health::Slow => {
                after.acted.retain(|(s, _)| *s != verdict.stage);
            }
        }

        match verdict.action {
            Action::None => {}
            Action::Tell(code) => {
                if !code.is_empty() && !after.told.contains(&(verdict.stage, code)) {
                    after.told.push((verdict.stage, code));
                    out.push(Due::Action(Action::Tell(code)));
                }
            }
            Action::End(code) => {
                if !after.acted.iter().any(|(s, _)| *s == verdict.stage) {
                    after.acted.push((verdict.stage, after.now_ms));
                    out.push(Due::Action(Action::End(code)));
                }
            }
            Action::Reopen(target) => {
                let repeat_ms =
                    find_budget(verdict.stage, BUDGETS).and_then(|b| b.stalled_ms).unwrap_or(0);
                match after.acted.iter_mut().find(|(s, _)| *s == verdict.stage) {
                    None => {
                        after.acted.push((verdict.stage, after.now_ms));
                        out.push(Due::Action(Action::Reopen(target)));
                    }
                    Some((_, last_ms)) => {
                        if repeat_ms > 0 && after.now_ms.saturating_sub(*last_ms) >= repeat_ms {
                            *last_ms = after.now_ms;
                            out.push(Due::Action(Action::Reopen(target)));
                        }
                    }
                }
            }
        }

        after.health.push((verdict.stage, verdict.health.clone()));
    }

    out
}
