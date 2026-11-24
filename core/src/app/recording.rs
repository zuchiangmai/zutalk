//! One recording, from its first audio block to its sealed tape (`P1.md` §3.5–§3.7, §3.13.11).
//!
//! Its lifecycle is an explicit state machine ([`Phase`], [`Step`], [`Happened`], [`step`]) where
//! every transition emits `recording.phase`. Its long-running parts (`tape`, `draft`, `lane.0..2`,
//! `frame`, `watch`) live on one `recording` supervision subtree and report progress through
//! atomic [`Gauge`]s sampled at 1 Hz into `Outlet.water`.

use crate::app::hub::{Config, Msg, Voice};
use crate::app::proto::{Level as SoundLevel, Session, Status};
use crate::capture::audio::{Notice, Source};
use crate::capture::clock::{Block, wall_ms};
use crate::capture::tape;
use crate::id::keys::{self, Chain};
use crate::knowledge::glossary::{self, Term};
use crate::live::frame::{Composer, Fit, Frame, MAX_COLUMNS, status};
use crate::live::lane::{self, LANE_NAMES, Mode, Stream};
use crate::live::lines::Line;
use crate::record::shelf::{About, DraftGap, DraftStep, RECORDING, Said, Shelf};
use crate::rt::keep;
use crate::rt::queue::{self, Full, Rx, Tx};
use crate::rt::signal::Register;
use crate::rt::stats::{self, Counter, Peak, Spread};
use crate::rt::supervise::{End, Kept, Part, Restarts, Tree};
use crate::rt::water::{
    Action, BUDGETS, Due, Gauge, Health, Stage, Verdict, Water, due, judge, sample, verdict_why,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval};
use zutalk_log::{Id, debug, error, info, warn};

const REALTIME: &str = "stt-rt-v6";
/// Captions are drawn at most this often: once a 100 ms block, as new words come no faster.
const FRAME_MS: u64 = 100;
/// Length of one capture audio block in milliseconds (1600 samples at 16 kHz).
const BLOCK_MS: u64 = 100;
/// How often the watcher samples gauges and publishes `Water`: 1 Hz (`P1.md` §3.5).
pub const WATCH_EVERY_MS: u64 = 1_000;
/// Audio blocks buffered on the broadcast bus to caption lanes: 64 × 100 ms = 6.4 s; a lane skips
/// to the live edge at 20 blocks (`P1.md` §3.7).
pub const AUDIO_FANOUT_BLOCKS: usize = 64;
/// Caption events buffered between lanes and the frame composer: 64 events absorbs bursts without
/// stalling lanes on the same path (`P1.md` §3.7).
pub const LANE_QUEUE_EVENTS: usize = 64;
/// Audio blocks buffered for the tape writer: 600 × 100 ms = 60 s; longer means disk is stuck
/// (`P1.md` §3.7, provisional `P1.md` §4 item 3).
pub const TAPE_QUEUE_BLOCKS: usize = 600;
/// Live draft lines buffered for the draft writer: 256 lines covers minutes of speech before
/// collapsing dropped lines into one gap (`P1.md` §3.7, provisional `P1.md` §4 item 3).
pub const DRAFT_QUEUE_LINES: usize = 256;

/// Order of closing steps for one recording (`P1-16`).
///
/// 1. `Capture`: drop capture so the pump delivers its last blocks and `sink` drops.
/// 2. `Tape`: wait for the tape writer to flush and sync remaining blocks.
/// 3. `Lanes`: wait for caption lanes to drain last words (bounded by `TAIL_MS + PATIENCE_MS`).
/// 4. `Frame`: wait for the frame task to process remaining events and close the draft queue.
/// 5. `Draft`: wait for the draft writer to flush and sync remaining lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Capture,
    Tape,
    Lanes,
    Frame,
    Draft,
}

impl Step {
    /// All closing steps in deterministic shutdown order.
    pub const ALL: [Step; 5] = [Step::Capture, Step::Tape, Step::Lanes, Step::Frame, Step::Draft];

    /// The next closing step, or `None` after `Draft` (moving to `Sealing`).
    pub const fn next(self) -> Option<Step> {
        match self {
            Step::Capture => Some(Step::Tape),
            Step::Tape => Some(Step::Lanes),
            Step::Lanes => Some(Step::Frame),
            Step::Frame => Some(Step::Draft),
            Step::Draft => None,
        }
    }
}

/// Where a recording is (`P1.md` §3.6, §3.13.11). Each closing step waits for one thing, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Opening,
    Recording,
    Paused,
    Closing(Step),
    Sealing,
    Ended(&'static str),
}

impl Phase {
    /// Stable plain code for `recording.phase` log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Phase::Opening => "Opening",
            Phase::Recording => "Recording",
            Phase::Paused => "Paused",
            Phase::Closing(Step::Capture) => "Closing.Capture",
            Phase::Closing(Step::Tape) => "Closing.Tape",
            Phase::Closing(Step::Lanes) => "Closing.Lanes",
            Phase::Closing(Step::Frame) => "Closing.Frame",
            Phase::Closing(Step::Draft) => "Closing.Draft",
            Phase::Sealing => "Sealing",
            Phase::Ended(_) => "Ended",
        }
    }
}

/// What the recording task observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Happened {
    Opened,
    Mode(Mode),
    Failed(&'static str),
    StepDone,
}

/// Side effect returned by [`step`] for the recording task to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Pause,
    Resume,
    Say(&'static str),
    Close(Step),
    Seal,
}

/// Pure transition table of a recording (`P1.md` §3.13.11): given the current `phase`, the closing
/// code carried toward `Ended` (initially `"done"`), and what `happened`, returns the next phase,
/// updated closing code, and effects to run.
pub fn step(
    phase: Phase,
    ending: &'static str,
    happened: Happened,
) -> (Phase, &'static str, Vec<Effect>) {
    match (phase, happened) {
        (Phase::Opening, Happened::Opened) => (Phase::Recording, ending, Vec::new()),
        (Phase::Opening, Happened::Mode(Mode::Stopped)) => {
            (Phase::Closing(Step::Capture), "done", vec![Effect::Close(Step::Capture)])
        }
        (Phase::Opening, Happened::Failed(code)) => {
            let mut effects = Vec::new();
            if code != "done" {
                effects.push(Effect::Say(code));
            }
            effects.push(Effect::Close(Step::Capture));
            (Phase::Closing(Step::Capture), code, effects)
        }
        (Phase::Recording, Happened::Mode(Mode::Paused)) => {
            (Phase::Paused, ending, vec![Effect::Pause])
        }
        (Phase::Recording, Happened::Mode(Mode::Stopped)) => {
            (Phase::Closing(Step::Capture), ending, vec![Effect::Close(Step::Capture)])
        }
        (Phase::Recording, Happened::Failed(code)) => {
            let mut effects = Vec::new();
            if code != "done" {
                effects.push(Effect::Say(code));
            }
            effects.push(Effect::Close(Step::Capture));
            (Phase::Closing(Step::Capture), code, effects)
        }
        (Phase::Paused, Happened::Mode(Mode::Recording)) => {
            (Phase::Recording, ending, vec![Effect::Resume])
        }
        (Phase::Paused, Happened::Mode(Mode::Stopped)) => {
            (Phase::Closing(Step::Capture), ending, vec![Effect::Close(Step::Capture)])
        }
        (Phase::Paused, Happened::Failed(code)) => {
            let mut effects = Vec::new();
            if code != "done" {
                effects.push(Effect::Say(code));
            }
            effects.push(Effect::Close(Step::Capture));
            (Phase::Closing(Step::Capture), code, effects)
        }
        (Phase::Closing(current), Happened::StepDone) => match current.next() {
            Some(next) => (Phase::Closing(next), ending, vec![Effect::Close(next)]),
            None => (Phase::Sealing, ending, vec![Effect::Seal]),
        },
        (Phase::Closing(current), Happened::Failed(code)) => {
            if ending == "done" && code != "done" {
                (Phase::Closing(current), code, vec![Effect::Say(code)])
            } else {
                (Phase::Closing(current), ending, Vec::new())
            }
        }
        (Phase::Sealing, Happened::StepDone) => (Phase::Ended(ending), ending, Vec::new()),
        (Phase::Sealing, Happened::Failed(code)) => {
            if ending == "done" && code != "done" {
                (Phase::Ended(code), code, vec![Effect::Say(code)])
            } else {
                (Phase::Ended(ending), ending, Vec::new())
            }
        }
        (Phase::Ended(c), _) => (Phase::Ended(c), c, Vec::new()),
        (Phase::Opening, Happened::Mode(Mode::Recording | Mode::Paused) | Happened::StepDone)
        | (
            Phase::Recording,
            Happened::Opened | Happened::Mode(Mode::Recording) | Happened::StepDone,
        )
        | (Phase::Paused, Happened::Opened | Happened::Mode(Mode::Paused) | Happened::StepDone)
        | (Phase::Closing(_), Happened::Opened | Happened::Mode(_))
        | (Phase::Sealing, Happened::Opened | Happened::Mode(_)) => (phase, ending, Vec::new()),
    }
}

/// Emits one `water.mark` event per stage in `water`: `debug!` for the 1 Hz stream (`snap == 0`),
/// `warn!` when tagged with a stall snapshot (`snap > 0`).
pub fn write_water(water: &Water, snap: u64) {
    for mark in &water.marks {
        let stage = mark.stage.code();
        let lane = mark.stage.lane().map_or(-1i32, i32::from);
        let ended = mark.ended.unwrap_or("");
        if snap == 0 {
            debug!(
                "water.mark",
                snap = snap,
                stage = stage,
                lane = lane,
                at = mark.at,
                moved_ms = mark.moved_ms,
                why = mark.why,
                ended = ended,
            );
        } else {
            warn!(
                "water.mark",
                snap = snap,
                stage = stage,
                lane = lane,
                at = mark.at,
                moved_ms = mark.moved_ms,
                why = mark.why,
                ended = ended,
            );
        }
    }
}

/// Watermark gauges shared across the parts of one recording.
#[derive(Clone)]
struct Gauges {
    capture: Arc<Gauge>,
    tape: Arc<Gauge>,
    draft: Arc<Gauge>,
    frame: Arc<Gauge>,
    lanes: Vec<lane::Gauges>,
    shell: Arc<Gauge>,
    screen: Arc<Gauge>,
}

impl Gauges {
    fn new(lane_count: usize, shell: Arc<Gauge>, screen: Arc<Gauge>) -> Self {
        Self {
            capture: Arc::new(Gauge::new()),
            tape: Arc::new(Gauge::new()),
            draft: Arc::new(Gauge::new()),
            frame: Arc::new(Gauge::new()),
            lanes: (0..lane_count).map(|_| lane::Gauges::default()).collect(),
            shell,
            screen,
        }
    }

    fn read_positions(&self) -> Vec<(Stage, u64, &'static str, Option<&'static str>)> {
        let mut out = Vec::with_capacity(4 + self.lanes.len() * 3 + 2);
        let push = |out: &mut Vec<_>, stage: Stage, g: &Gauge| {
            let (at, why, ended) = g.read();
            out.push((stage, at, why, ended));
        };
        push(&mut out, Stage::Capture, &self.capture);
        push(&mut out, Stage::Tape, &self.tape);
        push(&mut out, Stage::Draft, &self.draft);
        push(&mut out, Stage::Frame, &self.frame);
        for (n, lane) in self.lanes.iter().enumerate() {
            let idx = n as u8;
            push(&mut out, Stage::Sent(idx), &lane.sent);
            push(&mut out, Stage::Heard(idx), &lane.heard);
            push(&mut out, Stage::Final(idx), &lane.final_at);
        }
        if self.shell.read().0 > 0 {
            push(&mut out, Stage::Shell, &self.shell);
        }
        if self.screen.read().0 > 0 {
            push(&mut out, Stage::Screen, &self.screen);
        }
        out
    }
}

/// Everything the hub hands to a recording when it starts.
pub(crate) struct Start {
    pub id: String,
    pub about: About,
    pub source: Source,
    pub captions: Result<Vec<Stream>, &'static str>,
    pub mode: watch::Receiver<Mode>,
    pub shelf: Arc<Shelf>,
    pub keys: Arc<Chain>,
    pub config: Arc<Config>,
    pub terms: Vec<Term>,
    pub frame: Register<Frame>,
    pub session: Register<Option<Session>>,
    pub level: Register<SoundLevel>,
    pub water: Register<Water>,
    pub said: Voice,
    pub shell: Arc<Gauge>,
    pub screen: Arc<Gauge>,
    pub next_snap: Arc<AtomicU64>,
    pub root: Tree,
    pub results: Tx<Msg>,
}

/// Outcome of one recording after sealing its tape.
pub(crate) struct Done {
    pub sealed: Result<Option<About>, &'static str>,
}

fn end_code(end: End) -> &'static str {
    if let End::Retry(code) | End::Fatal(code) = end { code } else { "done" }
}

fn shelf_err(_: zu::Error) -> &'static str {
    "shelf"
}

fn now() -> i64 {
    wall_ms() as i64
}

fn is_active(session: &Register<Option<Session>>, id: &str) -> bool {
    (*session.read().now().1).as_ref().is_some_and(|s| s.id == id)
}

fn report_status(
    session: &Register<Option<Session>>,
    id: &str,
    told: &mut Option<Status>,
    next: Status,
) {
    if told.as_ref() == Some(&next) {
        return;
    }
    let mut active = false;
    session.change(|current| {
        let current_session = current.as_ref().filter(|s| s.id == id)?;
        active = true;
        (current_session.status != next)
            .then(|| Some(Session { status: next.clone(), ..current_session.clone() }))
    });
    if !active {
        return;
    }
    let from = told.as_ref().map_or("", Status::code);
    let to = next.code();
    let why = next.why().to_owned();
    *told = Some(next);
    info!("session.status", rec = Id(id), from = from, to = to, why = Id(&why));
}

fn end_once(ended: &watch::Sender<Option<&'static str>>, why: &'static str) {
    ended.send_if_modified(|current| {
        let first = current.is_none();
        if first {
            *current = Some(why);
        }
        first
    });
}

fn clear_water(reg: &Register<Water>) {
    reg.change(|current| (*current != Water::default()).then(Water::default));
}

fn emit_phase(id: &str, from: Phase, to: Phase, why: &'static str, ms: u64) {
    if from == to {
        return;
    }
    if let Phase::Ended(code) = to
        && code != "done"
    {
        error!(
            "recording.phase",
            rec = Id(id),
            from = from.code(),
            to = to.code(),
            why = code,
            ms = ms,
        );
    } else {
        info!(
            "recording.phase",
            rec = Id(id),
            from = from.code(),
            to = to.code(),
            why = why,
            ms = ms,
        );
    }
}

/// Seals a recording's tape on `tree` via `Tree::blocking`, writes its final length on the record,
/// and removes empty recordings from the shelf.
pub(crate) async fn seal(
    tree: &Tree,
    shelf: &Shelf,
    tape_dir: PathBuf,
    id: &str,
) -> Result<Option<About>, &'static str> {
    let key = keys::derive("tape", id).map_err(end_code)?;
    let length = tree.blocking("recording.seal", move || tape::recover(&tape_dir, &key)).await;
    let length_ms = length.map_err(|_| "tape")?.map_err(|_| "tape")?;
    let Some(recording) = shelf.about(id).map_err(shelf_err)? else { return Ok(None) };
    let updated = About { length_ms, ..recording.about };
    if length_ms == 0 || recording.about.length_ms != length_ms {
        let about = (length_ms > 0).then_some(updated.clone());
        shelf.put(id, RECORDING, &[(id.into(), about)], now()).map_err(shelf_err)?;
    }
    Ok(Some(updated))
}

struct DraftRelay {
    tx: Option<Tx<(String, Said, i64)>>,
    gap: DraftGap,
    next_idx: usize,
    told: Option<Status>,
    last_t1: Vec<u32>,
}

struct FrameShared {
    id: String,
    shown: Vec<String>,
    streams: Vec<Stream>,
    quiet: &'static str,
    events: Arc<Mutex<Rx<lane::Event>>>,
    relay: Arc<Mutex<DraftRelay>>,
    mode: watch::Receiver<Mode>,
    health: watch::Receiver<Vec<Health>>,
    frame_reg: Register<Frame>,
    session: Register<Option<Session>>,
    said: Voice,
    capture_gauge: Arc<Gauge>,
    frame_gauge: Arc<Gauge>,
    frame_made: Counter,
    frame_cut: Counter,
    frame_bound: Peak,
    draft_lost: Counter,
    caption_lag_ms: Spread,
}

struct FramePart {
    shared: Arc<FrameShared>,
    composer: Composer,
    mode: watch::Receiver<Mode>,
    health: watch::Receiver<Vec<Health>>,
    last_fit: Fit,
}

impl Part for FramePart {
    async fn run(&mut self) -> End {
        let mut frame_tick = tick();
        let events = self.shared.events.clone();
        let mut rx = events.lock().await;
        let mut mode_open = true;
        let mut health_open = true;
        loop {
            tokio::select! {
                changed = self.mode.changed(), if mode_open => {
                    match changed {
                        Ok(()) => {
                            let mode = *self.mode.borrow_and_update();
                            self.composer.pause(mode == Mode::Paused);
                        }
                        Err(_) => mode_open = false,
                    }
                }
                changed = self.health.changed(), if health_open => {
                    match changed {
                        Ok(()) => {
                            let judged = self.health.borrow_and_update().clone();
                            self.composer.judged(judged);
                        }
                        Err(_) => health_open = false,
                    }
                }
                event = rx.recv() => match event {
                    None => {
                        self.flush_and_close_draft().await;
                        return End::Done;
                    }
                    Some(event) => {
                        self.note_lag(&event).await;
                        self.forward_draft(&event).await;
                        self.composer.take(event);
                    }
                },
                _ = frame_tick.tick(), if self.composer.dirty() => {
                    self.emit_frame().await;
                }
            }
        }
    }
}

impl FramePart {
    async fn note_lag(&self, event: &lane::Event) {
        let (stream, max_t1) = match event {
            lane::Event::Lines { stream, lines } => {
                let Some(t1) = lines.iter().map(|l| l.t1).max() else { return };
                (*stream, t1)
            }
            lane::Event::Tail { stream, tail } => {
                let Some(t1) = tail.iter().map(|l| l.t1).max() else { return };
                (*stream, t1)
            }
            lane::Event::Gap { .. } | lane::Event::Link { .. } => return,
        };
        let mut relay = self.shared.relay.lock().await;
        if stream >= relay.last_t1.len() {
            relay.last_t1.resize(stream + 1, 0);
        }
        if max_t1 > relay.last_t1[stream] {
            relay.last_t1[stream] = max_t1;
            let cap = self.shared.capture_gauge.read().0;
            self.shared.caption_lag_ms.note(cap.saturating_sub(u64::from(max_t1)));
        }
    }

    async fn forward_draft(&self, event: &lane::Event) {
        let lines = drafted(event);
        if lines.is_empty() {
            return;
        }
        let mut relay = self.shared.relay.lock().await;
        for said in lines {
            let Some(draft_tx) = relay.tx.clone() else {
                return;
            };
            let (t0_ms, t1_ms) = (said.line.t0, said.line.t1);
            if let DraftStep::GapFirst(span) = relay.gap.next() {
                let gap_said = said_line(DraftGap::gap_line(span));
                let key = format!("{}.0.{:05}", self.shared.id, relay.next_idx);
                if draft_tx.offer((key, gap_said, now())).is_ok() {
                    relay.next_idx += 1;
                    if let Some(sent) = relay.gap.gap_sent() {
                        warn!(
                            "draft.gap",
                            t0_ms = sent.t0_ms,
                            t1_ms = sent.t1_ms,
                            lines = sent.lines,
                        );
                    }
                } else {
                    self.shared.draft_lost.add(1);
                    if relay.gap.missed(t0_ms, t1_ms).first {
                        self.shared.said.say("draft.behind");
                    }
                    continue;
                }
            }
            let key = format!("{}.0.{:05}", self.shared.id, relay.next_idx);
            if draft_tx.offer((key, said, now())).is_ok() {
                relay.next_idx += 1;
            } else {
                self.shared.draft_lost.add(1);
                if relay.gap.missed(t0_ms, t1_ms).first {
                    self.shared.said.say("draft.behind");
                }
            }
        }
    }

    async fn flush_and_close_draft(&self) {
        let mut relay = self.shared.relay.lock().await;
        if let Some(span) = relay.gap.pending()
            && let Some(draft_tx) = relay.tx.clone()
        {
            let gap_said = said_line(DraftGap::gap_line(span));
            let key = format!("{}.0.{:05}", self.shared.id, relay.next_idx);
            if draft_tx.send((key, gap_said, now())).await.is_ok() {
                relay.next_idx += 1;
                if let Some(sent) = relay.gap.gap_sent() {
                    warn!("draft.gap", t0_ms = sent.t0_ms, t1_ms = sent.t1_ms, lines = sent.lines);
                }
            }
        }
        // Drop the draft sender so `draft` finishes writing and syncs.
        relay.tx.take();
    }

    async fn emit_frame(&mut self) {
        let Some((frame, fit)) = self.composer.frame() else {
            return;
        };
        self.shared.frame_made.add(1);
        self.shared.frame_bound.set(self.composer.bound_bytes() as u64);
        if fit != Fit::Whole {
            self.shared.frame_cut.add(1);
        }
        if fit != self.last_fit {
            let bytes = serde_json::to_vec(&frame).map_or(0, |v| v.len());
            info!("frame.fit", from = self.last_fit.code(), to = fit.code(), bytes = bytes);
            self.last_fit = fit;
        }
        if *self.mode.borrow() != Mode::Stopped && is_active(&self.shared.session, &self.shared.id)
        {
            self.shared.frame_reg.set(frame);
            self.shared.frame_gauge.set(self.shared.frame_reg.seq());
        }
        let next = status(
            self.composer.paused(),
            self.composer.health(),
            self.composer.links(),
            self.shared.quiet,
        );
        let mut relay = self.shared.relay.lock().await;
        report_status(&self.shared.session, &self.shared.id, &mut relay.told, next);
    }
}

fn start_frame(tree: &Tree, shared: Arc<FrameShared>) -> Kept {
    tree.keep(
        "frame",
        move || {
            let shared = shared.clone();
            async move {
                let mut composer = Composer::new(&shared.shown, shared.streams.clone());
                let mode = shared.mode.clone();
                let health = shared.health.clone();
                composer.pause(*mode.borrow() == Mode::Paused);
                composer.judged(health.borrow().clone());
                Ok(FramePart { shared, composer, mode, health, last_fit: Fit::Whole })
            }
        },
        |_| {},
        Restarts::default(),
    )
}

/// Derives each caption column's health from `verdicts`: column `j` uses lane `max(j, 1) - 1`,
/// taking the worse of its `Sent` and `Heard` verdicts (`P1-17`).
fn column_health(columns: usize, verdicts: &[Verdict]) -> Vec<Health> {
    (0..columns)
        .map(|j| {
            let lane = j.saturating_sub(1) as u8;
            let sent = verdicts
                .iter()
                .find(|v| v.stage == Stage::Sent(lane))
                .map_or(&Health::Live, |v| &v.health);
            let heard = verdicts
                .iter()
                .find(|v| v.stage == Stage::Heard(lane))
                .map_or(&Health::Live, |v| &v.health);
            Health::worst(sent, heard)
        })
        .collect()
}

fn emit_verdict(
    water: &Water,
    verdicts: &[Verdict],
    stage: Stage,
    from: &Health,
    to: &Health,
    judge_stalled: &Counter,
    judge_dead: &Counter,
) {
    let lane = stage.lane().map_or(-1i32, i32::from);
    let why = verdict_why(water, stage, to);
    let action = verdicts.iter().find(|v| v.stage == stage).map_or(Action::None, |v| v.action);
    match to {
        Health::Live => info!(
            "judge.verdict",
            stage = stage.code(),
            lane = lane,
            from = from.code(),
            to = to.code(),
            why = why,
            action = action.code(),
        ),
        Health::Slow => warn!(
            "judge.verdict",
            stage = stage.code(),
            lane = lane,
            from = from.code(),
            to = to.code(),
            why = why,
            action = action.code(),
        ),
        Health::Stalled { .. } => {
            judge_stalled.add(1);
            warn!(
                "judge.verdict",
                stage = stage.code(),
                lane = lane,
                from = from.code(),
                to = to.code(),
                why = why,
                action = action.code(),
            );
        }
        Health::Dead { .. } => {
            judge_dead.add(1);
            error!(
                "judge.verdict",
                stage = stage.code(),
                lane = lane,
                from = from.code(),
                to = to.code(),
                why = why,
                action = action.code(),
            );
        }
    }
}

/// Writes one `stall.snapshot` bundle (`P1.md` §3.13.5): header, every `water.mark`, every
/// `stat.*`, every `task.running` on the root tree, and `stall.snapshot.end` with exact line count.
fn write_snapshot(snap: u64, rec: &str, stage: Stage, water: &Water, root: &Tree) {
    let health = water.health.iter().find(|(s, _)| *s == stage).map_or(&Health::Live, |(_, h)| h);
    let why = verdict_why(water, stage, health);
    let idle_ms = water
        .marks
        .iter()
        .find(|m| m.stage == stage)
        .map_or(0, |m| water.now_ms.saturating_sub(m.moved_ms));
    let lane = stage.lane().map_or(-1i32, i32::from);
    let tally = root.stats().tally();
    let running = root.running();
    let lines = 1 + water.marks.len() + tally.len() + running.len() + 1;

    if matches!(health, Health::Dead { .. }) {
        error!(
            "stall.snapshot",
            snap = snap,
            rec = Id(rec),
            stage = stage.code(),
            lane = lane,
            health = health.code(),
            why = why,
            idle_ms = idle_ms,
        );
    } else {
        warn!(
            "stall.snapshot",
            snap = snap,
            rec = Id(rec),
            stage = stage.code(),
            lane = lane,
            health = health.code(),
            why = why,
            idle_ms = idle_ms,
        );
    }
    write_water(water, snap);
    stats::write(&tally, snap);
    for row in &running {
        warn!(
            "task.running",
            snap = snap,
            tree = row.tree,
            task = row.task,
            kind = row.kind,
            for_ms = row.for_ms,
        );
    }
    if matches!(health, Health::Dead { .. }) {
        error!("stall.snapshot.end", snap = snap, lines = lines);
    } else {
        warn!("stall.snapshot.end", snap = snap, lines = lines);
    }
}

fn advance_clock(
    clock_ms: &mut u64,
    clock_since: &mut Option<u64>,
    first_block_ms: &AtomicU64,
    now_ms: u64,
    was_paused: bool,
) {
    let first = first_block_ms.load(Ordering::Acquire);
    if first == u64::MAX {
        return;
    }
    let from = clock_since.unwrap_or(first);
    if !was_paused {
        *clock_ms = clock_ms.saturating_add(now_ms.saturating_sub(from));
    }
    *clock_since = Some(now_ms);
}

struct WatchArgs {
    id: String,
    origin: Instant,
    columns: usize,
    first_block_ms: Arc<AtomicU64>,
    gauges: Gauges,
    mode: watch::Receiver<Mode>,
    stop: watch::Receiver<bool>,
    health_tx: watch::Sender<Vec<Health>>,
    kicks: Vec<watch::Sender<u64>>,
    ended_tx: watch::Sender<Option<&'static str>>,
    said: Voice,
    session: Register<Option<Session>>,
    water_reg: Register<Water>,
    next_snap: Arc<AtomicU64>,
    root: Tree,
}

async fn watch_task(mut args: WatchArgs) {
    let mut tick = interval(Duration::from_millis(WATCH_EVERY_MS));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await; // first sample is at 1 s

    let judge_stalled = args.root.stats().counter("judge.stalled");
    let judge_dead = args.root.stats().counter("judge.dead");
    let lane_kicks = args.root.stats().counter("lane.kicks");

    let mut paused = *args.mode.borrow() == Mode::Paused;
    let mut clock_ms = 0u64;
    let mut clock_since: Option<u64> = None;
    // Seed initial marks at t = 0 so a stage that never advances reaches its budget on time.
    let mut before = sample(&Water::default(), &args.gauges.read_positions(), 0, 0, paused);

    loop {
        if *args.stop.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed = args.stop.changed() => {
                if changed.is_err() || *args.stop.borrow() {
                    break;
                }
            }
            changed = args.mode.changed() => {
                let next_mode = changed.map_or(Mode::Stopped, |()| *args.mode.borrow_and_update());
                if next_mode == Mode::Stopped {
                    break;
                }
                let now_ms = args.origin.elapsed().as_millis() as u64;
                advance_clock(&mut clock_ms, &mut clock_since, &args.first_block_ms, now_ms, paused);
                paused = next_mode == Mode::Paused;
            }
            _ = tick.tick() => {
                if *args.stop.borrow()
                    || *args.mode.borrow() == Mode::Stopped
                    || !is_active(&args.session, &args.id)
                {
                    break;
                }
                let now_ms = args.origin.elapsed().as_millis() as u64;
                advance_clock(&mut clock_ms, &mut clock_since, &args.first_block_ms, now_ms, paused);
                let positions = args.gauges.read_positions();
                let mut next = sample(&before, &positions, now_ms, clock_ms, paused);
                let verdicts = judge(&next, BUDGETS);
                let due_items = due(&before, &mut next, &verdicts);

                write_water(&next, 0);
                if *args.mode.borrow() != Mode::Stopped && is_active(&args.session, &args.id) {
                    args.water_reg.set(next.clone());
                }

                let cols = column_health(args.columns, &verdicts);
                args.health_tx.send_if_modified(|current| {
                    if *current != cols {
                        *current = cols;
                        true
                    } else {
                        false
                    }
                });

                let mut ending_recording = false;
                for item in due_items {
                    match item {
                        Due::Changed { stage, from, to } => {
                            emit_verdict(
                                &next,
                                &verdicts,
                                stage,
                                &from,
                                &to,
                                &judge_stalled,
                                &judge_dead,
                            );
                        }
                        Due::Snapshot { stage } => {
                            let snap = args.next_snap.fetch_add(1, Ordering::Relaxed);
                            write_snapshot(snap, &args.id, stage, &next, &args.root);
                        }
                        Due::Action(Action::Reopen(Stage::Heard(n))) => {
                            if let Some(kick) = args.kicks.get(usize::from(n)) {
                                kick.send_modify(|c| *c += 1);
                                lane_kicks.add(1);
                                warn!("lane.kick", lane = n, why = "silent");
                            }
                        }
                        Due::Action(Action::Tell(code)) => {
                            args.said.say(code);
                        }
                        Due::Action(Action::End(code)) => {
                            end_once(&args.ended_tx, code);
                            ending_recording = true;
                        }
                        Due::Action(Action::None | Action::Reopen(_)) => {}
                    }
                }

                before = next;
                if ending_recording {
                    // Leave the Stalled Water in `water_reg` until `Closing(Capture)` clears it.
                    return;
                }
            }
        }
    }
    clear_water(&args.water_reg);
}

/// Runs one recording from `recording.start` through `Phase::Opening`, `Recording`/`Paused`,
/// ordered `Closing(Step)`, `Sealing`, and `recording.end`.
pub(crate) async fn run(mut start: Start) -> Done {
    let started_at = Instant::now();
    let source_code = start.source.code();
    let (lane_count, columns, captions_code) = match &start.captions {
        Ok(streams) => (streams.len(), start.about.shown.len().min(MAX_COLUMNS), "live"),
        Err(why) => (0, 0, *why),
    };
    info!(
        "recording.start",
        rec = Id(&start.id),
        source = source_code,
        lanes = lane_count,
        columns = columns,
        captions = captions_code,
    );

    let mut phase = Phase::Opening;
    let mut phase_at = started_at;
    let mut ending: &'static str = "done";
    let mut self_ended = false;
    let mut tape_failed = false;

    let transition = |phase: &mut Phase,
                      phase_at: &mut Instant,
                      ending: &mut &'static str,
                      happened: Happened,
                      why: &'static str|
     -> Vec<Effect> {
        let from = *phase;
        let (next, next_ending, effects) = step(from, ending, happened);
        *ending = next_ending;
        if next != from {
            let ms = phase_at.elapsed().as_millis() as u64;
            let event_why = match next {
                Phase::Ended(code) => code,
                _ => why,
            };
            emit_phase(&start.id, from, next, event_why, ms);
            *phase = next;
            *phase_at = Instant::now();
        }
        effects
    };

    let recording = start.root.child("recording");
    let stats = recording.stats().clone();
    let gauges = Gauges::new(lane_count, start.shell.clone(), start.screen.clone());
    let (internal_mode_tx, internal_mode_rx) = watch::channel(*start.mode.borrow());
    let (watch_stop_tx, watch_stop_rx) = watch::channel(false);
    let (ended_tx, mut ended_rx) = watch::channel(None::<&'static str>);
    let first_block_ms = Arc::new(AtomicU64::new(u64::MAX));

    let mut capture = None;
    let mut audio_fanout = None;
    let mut lane_tx_hold: Option<Tx<lane::Event>> = None;
    let mut lane_dones: Vec<oneshot::Receiver<()>> = Vec::new();
    let mut tape_done: Option<Kept> = None;
    let mut frame_done: Option<Kept> = None;
    let mut draft_done: Option<Kept> = None;
    let mut sealed: Result<Option<About>, &'static str> = Ok(None);

    // Phase::Opening — wire queues, parts, capture, and watch.
    let source = start.source;
    let open_result: Result<(), &'static str> = (|| {
        let key = keys::derive("tape", &start.id).map_err(end_code)?;
        let (tape_tx, tape_rx) = queue::bounded(&stats, "tape", TAPE_QUEUE_BLOCKS, Full::End);
        let (streams, quiet) = match &start.captions {
            Ok(streams) => (streams.clone(), ""),
            Err(why) => (Vec::new(), *why),
        };
        let audio = queue::fanout(&stats, "audio", AUDIO_FANOUT_BLOCKS);
        let (lane_tx, lane_rx) =
            queue::bounded(&stats, "lane.events", LANE_QUEUE_EVENTS, Full::Wait);

        let file = matches!(source, Source::File(_));
        let tape_wait = file.then(|| tape_tx.with(Full::Wait));
        let sink_ended = ended_tx.clone();
        let sink_level = start.level.clone();
        let sink_mode = internal_mode_rx.clone();
        let sink_audio = audio.clone();
        let sink_capture_gauge = gauges.capture.clone();
        let sink_first_block = first_block_ms.clone();
        let mut kept = true;
        let sink = move |block: Block| {
            let block = Arc::new(block);
            let elapsed = started_at.elapsed().as_millis() as u64;
            let stamped = sink_first_block.compare_exchange(
                u64::MAX,
                elapsed,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            let _ = stamped; // dropped: only the first block stamps the recording clock origin
            sink_capture_gauge.set(u64::from(block.t_ms) + BLOCK_MS);
            if *sink_mode.borrow() == Mode::Recording {
                sink_level.set(SoundLevel::of(&block.pcm));
            }
            if kept {
                let ok = match &tape_wait {
                    Some(tx) => tx.send_blocking(block.clone()).is_ok(),
                    None => tape_tx.offer(block.clone()).is_ok(),
                };
                if !ok {
                    kept = false;
                    end_once(&sink_ended, "tape");
                }
            }
            let _ = sink_audio.send(block); // dropped: fanout returns 0 when no caption lanes are listening
        };

        let notice_ended = ended_tx.clone();
        let notice_said = start.said.clone();
        let notice = move |notice| match notice {
            Notice::Ended => end_once(&notice_ended, if file { "done" } else { "audio.closed" }),
            Notice::Fatal(code) if file => end_once(&notice_ended, code),
            Notice::Restarted(code) | Notice::Fatal(code) => notice_said.say(code),
        };

        let hearing = (start.config.hear)(source, Box::new(sink), Box::new(notice), &stats)
            .map_err(end_code)?;
        capture = Some(hearing);

        let tape_dir = start.config.data.join("tape").join(&start.id);
        let reels = start.config.reels.clone();
        let tape_blocks = stats.counter("tape.blocks");
        let tape_padded = stats.counter("tape.padded_blocks");
        let tape_batch_ms = stats.spread("tape.batch_ms");
        let tape_gauge = gauges.tape.clone();
        tape_done = Some(keep::start(
            &recording,
            "tape",
            "tape.batch",
            tape_rx,
            tape_batch_ms,
            Restarts::default(),
            move || (reels)(&tape_dir, &key).map_err(|_| "tape"),
            move |writer, batch: &[Arc<Block>]| {
                for block in batch {
                    let res = writer.append(block);
                    let padded = writer.take_padded();
                    if padded > 0 {
                        tape_padded.add(padded);
                    }
                    res.map_err(|_| "tape")?;
                }
                if let Some(last) = batch.last() {
                    tape_gauge.set(u64::from(last.t_ms) + BLOCK_MS);
                }
                tape_blocks.add(batch.len() as u64);
                Ok(())
            },
            |writer| writer.sync().map_err(|_| "tape"),
        ));

        let (draft_tx, draft_rx) = queue::bounded(&stats, "draft", DRAFT_QUEUE_LINES, Full::Drop);
        let draft_shelf = start.shelf.clone();
        let draft_id = start.id.clone();
        let drafts = start.config.drafts.clone();
        let draft_lines = stats.counter("draft.lines");
        let draft_batch_ms = stats.spread("draft.batch_ms");
        let draft_gauge = gauges.draft.clone();
        draft_done = Some(keep::start(
            &recording,
            "draft",
            "draft.batch",
            draft_rx,
            draft_batch_ms,
            Restarts::default(),
            move || (drafts)(&draft_shelf, &draft_id).map_err(shelf_err),
            move |writer, batch: &[(String, Said, i64)]| {
                for (key, said, at) in batch {
                    writer.put(key, said, *at).map_err(shelf_err)?;
                }
                if let Some((_, last_said, _)) = batch.last() {
                    draft_gauge.set(u64::from(last_said.line.t1));
                }
                draft_lines.add(batch.len() as u64);
                Ok(())
            },
            |writer| writer.sync().map_err(shelf_err),
        ));

        let mut kicks = Vec::with_capacity(streams.len());
        let base_setup = lane::Setup {
            endpoint: start.config.realtime.clone(),
            model: REALTIME.into(),
            spoken: start.about.spoken.clone(),
            ..lane::Setup::default()
        };
        for (n, stream) in streams.iter().cloned().enumerate() {
            let setup = glossary::live(&start.terms, &stream, base_setup.clone());
            let (kick_tx, kick_rx) = watch::channel(0u64);
            kicks.push(kick_tx);
            let (keys, dial, lane_stats, lane_gauges, lane_audio, lane_mode, lane_out) = (
                start.keys.clone(),
                start.config.dial.clone(),
                stats.clone(),
                gauges.lanes[n].clone(),
                audio.subscribe(),
                internal_mode_rx.clone(),
                lane_tx.clone(),
            );
            let (done_tx, done_rx) = oneshot::channel();
            lane_dones.push(done_rx);
            recording.spawn(LANE_NAMES[n], async move {
                let running = lane::run(
                    n,
                    stream,
                    &setup,
                    keys,
                    dial,
                    lane_stats,
                    lane_gauges,
                    lane_audio,
                    lane_mode,
                    kick_rx,
                    lane_out,
                );
                let _ = running.await; // dropped: lane records its own end code in Link and Gauge
                let _ = done_tx.send(()); // dropped: recording task already ended or cancelled
            });
        }
        audio_fanout = Some(audio);
        lane_tx_hold = Some(lane_tx);

        let (health_tx, health_rx) = watch::channel(vec![Health::Live; columns]);
        let frame_shared = Arc::new(FrameShared {
            id: start.id.clone(),
            shown: start.about.shown.clone(),
            streams,
            quiet,
            events: Arc::new(Mutex::new(lane_rx)),
            relay: Arc::new(Mutex::new(DraftRelay {
                tx: Some(draft_tx),
                gap: DraftGap::new(),
                next_idx: 0,
                told: quiet.is_empty().then_some(Status::Connecting),
                last_t1: vec![0; lane_count],
            })),
            mode: internal_mode_rx.clone(),
            health: health_rx,
            frame_reg: start.frame.clone(),
            session: start.session.clone(),
            said: start.said.clone(),
            capture_gauge: gauges.capture.clone(),
            frame_gauge: gauges.frame.clone(),
            frame_made: stats.counter("frame.made"),
            frame_cut: stats.counter("frame.cut"),
            frame_bound: stats.peak("frame.bound_bytes"),
            draft_lost: stats.counter("draft.lost_lines"),
            caption_lag_ms: stats.spread("caption.lag_ms"),
        });
        frame_done = Some(start_frame(&recording, frame_shared));

        recording.spawn(
            "watch",
            watch_task(WatchArgs {
                id: start.id.clone(),
                origin: started_at,
                columns,
                first_block_ms: first_block_ms.clone(),
                gauges: gauges.clone(),
                mode: internal_mode_rx.clone(),
                stop: watch_stop_rx,
                health_tx,
                kicks,
                ended_tx: ended_tx.clone(),
                said: start.said.clone(),
                session: start.session.clone(),
                water_reg: start.water.clone(),
                next_snap: start.next_snap.clone(),
                root: start.root.clone(),
            }),
        );
        Ok(())
    })();

    let mut pending_effects = match open_result {
        Ok(()) => transition(&mut phase, &mut phase_at, &mut ending, Happened::Opened, ""),
        Err(code) => {
            self_ended = true;
            transition(&mut phase, &mut phase_at, &mut ending, Happened::Failed(code), code)
        }
    };

    // Main recording state-machine loop.
    loop {
        // Carry out any effects produced by the latest transition.
        let mut step_done = false;
        let mut seal_failed: Option<&'static str> = None;
        for effect in std::mem::take(&mut pending_effects) {
            match effect {
                Effect::Pause => {
                    if let Some(ref c) = capture {
                        c.pause();
                    }
                    internal_mode_tx.send_replace(Mode::Paused);
                    if is_active(&start.session, &start.id) {
                        start.level.set(SoundLevel::default());
                    }
                }
                Effect::Resume => {
                    if let Some(ref c) = capture {
                        c.resume();
                    }
                    internal_mode_tx.send_replace(Mode::Recording);
                }
                Effect::Say(code) => {
                    start.said.say(code);
                }
                Effect::Close(Step::Capture) => {
                    // Drop capture first so final audio blocks flush and `sink` (with `tape_tx`) drops.
                    capture = None;
                    drop(audio_fanout.take());
                    internal_mode_tx.send_replace(Mode::Stopped);
                    watch_stop_tx.send_replace(true);
                    clear_water(&start.water);
                    if is_active(&start.session, &start.id) {
                        start.level.set(SoundLevel::default());
                    }
                    if self_ended {
                        let _ = start.results.send(Msg::Ended(start.id.clone())).await; // dropped: the hub has ended
                    }
                    step_done = true;
                }
                Effect::Close(Step::Tape) => {
                    let tape_ended = ending == "tape" || tape_failed;
                    if !tape_ended
                        && let Some(done) = tape_done.take()
                        && matches!(done.await, Some(End::Fatal(_)))
                    {
                        tape_failed = true;
                        pending_effects.extend(transition(
                            &mut phase,
                            &mut phase_at,
                            &mut ending,
                            Happened::Failed("tape"),
                            "tape",
                        ));
                    }
                    step_done = true;
                }
                Effect::Close(Step::Lanes) => {
                    for done in std::mem::take(&mut lane_dones) {
                        let _ = done.await; // dropped: lane task cancelled or already ended
                    }
                    // Dropping our clone closes `lane.events`, telling `frame` no more events will come.
                    drop(lane_tx_hold.take());
                    step_done = true;
                }
                Effect::Close(Step::Frame) => {
                    if let Some(done) = frame_done.take() {
                        let _ = done.await; // dropped: frame task cancelled or already ended
                    }
                    step_done = true;
                }
                Effect::Close(Step::Draft) => {
                    if let Some(done) = draft_done.take() {
                        let _ = done.await; // dropped: draft write errors do not invalidate the audio recording
                    }
                    if ending == "tape" || tape_failed {
                        // Do not wait on a stuck tape writer that already overflowed or gave up.
                        recording.cancel();
                    }
                    recording.wait().await;
                    clear_water(&start.water);
                    step_done = true;
                }
                Effect::Seal => {
                    let tape_dir = start.config.data.join("tape").join(&start.id);
                    sealed = seal(&start.root, &start.shelf, tape_dir, &start.id).await;
                    match &sealed {
                        Ok(_) => step_done = true,
                        Err(code) => seal_failed = Some(*code),
                    }
                }
            }
        }

        if let Some(code) = seal_failed {
            pending_effects.extend(transition(
                &mut phase,
                &mut phase_at,
                &mut ending,
                Happened::Failed(code),
                code,
            ));
            continue;
        }
        if step_done {
            pending_effects.extend(transition(
                &mut phase,
                &mut phase_at,
                &mut ending,
                Happened::StepDone,
                "",
            ));
            continue;
        }

        if let Phase::Ended(code) = phase {
            let ms = started_at.elapsed().as_millis() as u64;
            if code == "done" {
                info!("recording.end", rec = Id(&start.id), code = code, ms = ms);
            } else {
                error!("recording.end", rec = Id(&start.id), code = code, ms = ms);
            }
            return Done { sealed };
        }

        // Active recording/paused wait: mode changes, capture end, or fatal tape writer.
        tokio::select! {
            changed = start.mode.changed() => {
                let next_mode = changed.map_or(Mode::Stopped, |()| *start.mode.borrow_and_update());
                pending_effects = transition(
                    &mut phase,
                    &mut phase_at,
                    &mut ending,
                    Happened::Mode(next_mode),
                    "",
                );
            }
            changed = ended_rx.changed() => {
                if changed.is_ok()
                    && let Some(why) = *ended_rx.borrow_and_update()
                {
                    self_ended = true;
                    pending_effects = transition(
                        &mut phase,
                        &mut phase_at,
                        &mut ending,
                        Happened::Failed(why),
                        why,
                    );
                }
            }
            end = async {
                tape_done.as_mut().expect("guarded by is_some").await
            }, if tape_done.is_some() => {
                tape_done = None;
                if let Some(End::Fatal(_)) = end {
                    tape_failed = true;
                    self_ended = true;
                    pending_effects = transition(
                        &mut phase,
                        &mut phase_at,
                        &mut ending,
                        Happened::Failed("tape"),
                        "tape",
                    );
                }
            }
        }
    }
}

fn tick() -> tokio::time::Interval {
    let mut tick = interval(Duration::from_millis(FRAME_MS));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick
}

fn said_line(line: Line) -> Said {
    let via = line.translations.keys().map(String::as_str).chain(["text"]);
    Said { via: via.map(|piece| (piece.into(), REALTIME.into())).collect(), line }
}

/// The draft's lines from the first stream, with where their words came from. A stretch the
/// captions missed is a line of its own.
fn drafted(event: &lane::Event) -> Vec<Said> {
    match event {
        lane::Event::Lines { stream: 0, lines } => lines.iter().cloned().map(said_line).collect(),
        &lane::Event::Gap { stream: 0, t0, t1, .. } => {
            vec![said_line(Line {
                t0, t1, text: "…".into(), incomplete: true, ..Line::default()
            })]
        }
        _ => Vec::new(),
    }
}
