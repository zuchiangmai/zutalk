//! Full-core simulation bench (`docs/build/phases/P1.md` §3.10, `P1-08-bench.md`).
//!
//! Runs the public [`Core`] against in-memory ports (`Mic`, `Service` over `tokio::io::duplex`,
//! `CountingTape` / `GatedTape`), signed out by default (`hub::nobody()`), with structured log
//! capture (`zutalk_log::capture`) and live stats (`core.stats().tally()`).
//!
//! All reads of protocol [`Event`] are confined behind [`Bench`]'s probe methods so P1-11 can
//! switch the bench to `Outlet` without touching the tests in `core/tests/liveness.rs`.
#![allow(dead_code)]

pub mod mic;
pub mod service;
pub mod speech;
pub mod tape;

use crate::hub;
use mic::Mic;
use serde_json::Value;
use service::{Delays, Fault, Service};
use speech::Speech;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tape::{CountingTape, GATE_TIMEOUT_MS, GatedTape};
use tokio::sync::Notify;
use tokio::time::Instant;
use zutalk_core::app::hub::{Config, Core, Outlet};
use zutalk_core::app::proto::{Column, Command, Frame, Session, Setup, State, Status};
use zutalk_core::id::keys;
use zutalk_core::record::shelf::{Draft, Drafts, LINE, Said, Shelf, Writer as ShelfWriter};
use zutalk_core::rt::stats::{PeakRow, QueueRow, SpreadRow, Tally};
use zutalk_core::rt::water::{Health, Water};
use zutalk_log::{Level, Seen};

/// Maximum JSON size of one live caption frame (`P1.md` §3.4).
pub const MAX_FRAME_BYTES: usize = 8 * 1024;
/// Fixed per-frame JSON overhead used in the cheap byte-size upper bound.
const FRAME_OVERHEAD_BYTES: usize = 64;
/// Fixed per-column JSON overhead used in the cheap byte-size upper bound.
const COLUMN_OVERHEAD_BYTES: usize = 64;
/// Fixed per-caption JSON overhead used in the cheap byte-size upper bound.
const CAPTION_OVERHEAD_BYTES: usize = 64;
/// Worst-case JSON bytes per `u16` entry in `Caption.units` (5 digits + comma).
const UNIT_MOST_BYTES: usize = 6;
/// Nothing listens here: bench tests never touch external servers unless configured.
pub const NOWHERE: &str = "https://127.0.0.1:9";
/// Fake WebSocket URL passed to `Dial` (intercepted by the in-memory `Service`).
const BENCH_WS: &str = "ws://bench.local/transcribe-websocket";

/// Serializes tests that share the process-wide mock key store.
pub static STORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Which tape port to install on a `Bench` run.
#[derive(Clone, Copy, Debug)]
pub enum TapeMode {
    /// Counts blocks without touching disk or encrypting (fast for 8-hour runs).
    Counting,
    /// Real encrypted `tape::Writer` behind a gate and one-shot panic injector.
    Gated,
}

/// Configuration for one simulation bench run.
pub struct BenchOptions {
    pub speech: Speech,
    pub delays: Delays,
    pub fault: Fault,
    pub tape: TapeMode,
    /// When `false`, no Soniox key is installed so the recording runs audio-only (`captions.no_key`).
    pub with_key: bool,
    /// Default `false` (`hub::nobody()`). Only set `true` when exercising signed-in-only 0.8 migration.
    pub signed_in: bool,
    /// When `true`, preserves the mock key store already configured by the caller before `start`.
    pub keep_store: bool,
    /// Default `true` (`zutalk_log::capture(capture_level)`). Set `false` when entering `zutalk_log::files`.
    pub capture_log: bool,
    /// Log level for in-memory capture (`Level::Info` by default).
    pub capture_level: Level,
    /// Optional whole-session Soniox endpoint override (default: unreachable `NOWHERE`).
    pub soniox: Option<String>,
    /// Optional 0.8 app data folder for migration testing.
    pub old: Option<PathBuf>,
    /// Optional pre-seeded data folder (otherwise a fresh tempdir is created).
    pub data: Option<PathBuf>,
}

impl BenchOptions {
    pub fn new(speech: Speech) -> Self {
        Self {
            speech,
            delays: Delays::default(),
            fault: Fault::None,
            tape: TapeMode::Counting,
            with_key: true,
            signed_in: false,
            keep_store: false,
            capture_log: true,
            capture_level: Level::Info,
            soniox: None,
            old: None,
            data: None,
        }
    }
}

struct ProbeState {
    /// First virtual instant each spoken word (`id - 1`) appeared in a live caption frame.
    word_seen: Vec<Option<Instant>>,
    /// Total live `Frame` updates received.
    frames: u64,
    /// Live `Frame` updates carrying at least one column.
    non_empty_column_frames: u64,
    /// Whether column `i` ever carried non-empty caption text.
    column_active: Vec<bool>,
    /// Highest cheap byte-size upper bound observed so far.
    max_bound_bytes: usize,
    /// Highest serialized JSON frame byte size observed so far.
    max_frame_bytes: usize,
    /// Latest whole `State` snapshot from the core.
    state: Option<State>,
    /// Latest `Session` signal from the core (`None` when idle).
    session: Option<Session>,
    /// Serialized JSON size of the latest `State` snapshot.
    last_state_bytes: usize,
    /// Distinct session statuses seen since the last reset.
    statuses: Vec<Status>,
    /// Distinct column-0 health values seen in live frames since the last reset.
    col0_healths: Vec<Health>,
    /// Last non-empty `Water` sample observed before the recording cleared it.
    last_water: Option<Water>,
    /// All notice codes received in order.
    notices: Vec<String>,
}

impl ProbeState {
    fn new(word_count: usize) -> Self {
        Self {
            word_seen: vec![None; word_count],
            frames: 0,
            non_empty_column_frames: 0,
            column_active: Vec::new(),
            max_bound_bytes: 0,
            max_frame_bytes: 0,
            state: None,
            session: None,
            last_state_bytes: 0,
            statuses: Vec::new(),
            col0_healths: Vec::new(),
            last_water: None,
            notices: Vec::new(),
        }
    }

    fn take_live(&mut self, at: Instant, frame: &Frame) {
        let columns = &frame.columns;
        self.frames += 1;
        if !columns.is_empty() {
            self.non_empty_column_frames += 1;
        }
        if let Some(first) = columns.first()
            && self.col0_healths.last() != Some(&first.health)
        {
            self.col0_healths.push(first.health.clone());
        }
        if self.column_active.len() < columns.len() {
            self.column_active.resize(columns.len(), false);
        }

        let bound = cheap_frame_bound(columns);
        if bound > self.max_bound_bytes {
            self.max_bound_bytes = bound;
            let actual = serde_json::to_vec(frame).expect("serialize frame").len();
            self.max_frame_bytes = self.max_frame_bytes.max(actual);
        }

        for (i, col) in columns.iter().enumerate() {
            if let Some(tail) = &col.tail
                && !tail.text.is_empty()
            {
                self.column_active[i] = true;
                scan_word_ids(&tail.text, at, &mut self.word_seen);
            }
            // Also scan the last 2 finished lines so a word that finished between two 100 ms
            // ticks is still recorded on the first frame carrying it.
            let start = col.lines.len().saturating_sub(2);
            for line in &col.lines[start..] {
                if !line.text.is_empty() {
                    self.column_active[i] = true;
                    scan_word_ids(&line.text, at, &mut self.word_seen);
                }
            }
        }
    }

    fn take_state(&mut self, state: State) {
        self.state = Some(state);
    }

    fn take_session(&mut self, session: Option<Session>) {
        if let Some(ref s) = session
            && self.statuses.last() != Some(&s.status)
        {
            self.statuses.push(s.status.clone());
        }
        self.session = session;
    }

    fn take_notice(&mut self, code: String) {
        self.notices.push(code);
    }
}

/// Cheap byte-size upper bound for a live caption frame (`P1-08-bench.md`).
fn cheap_frame_bound(columns: &[Column]) -> usize {
    let mut bound = FRAME_OVERHEAD_BYTES;
    for col in columns {
        bound += COLUMN_OVERHEAD_BYTES + col.language.len() + col.health.why().len() + 32;
        for cap in col.lines.iter().chain(col.tail.iter()) {
            bound += CAPTION_OVERHEAD_BYTES
                + cap.speaker.len()
                + cap.text.len()
                + cap.units.len() * UNIT_MOST_BYTES;
        }
    }
    bound
}

/// Scans `text` for spoken word markers (`w000001` or `词000001`) and records `at` on first sight.
fn scan_word_ids(text: &str, at: Instant, seen: &mut [Option<Instant>]) {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Match ASCII 'w' followed by 6 ASCII digits, or UTF-8 '词' (0xE8 0xAF 0x8D) + 6 digits.
        let digits_at = if bytes[i] == b'w' && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            Some(i + 1)
        } else if i + 3 <= len && bytes[i] == 0xE8 && bytes[i + 1] == 0xAF && bytes[i + 2] == 0x8D {
            Some(i + 3)
        } else {
            None
        };
        if let Some(start) = digits_at
            && start + 6 <= len
            && bytes[start..start + 6].iter().all(u8::is_ascii_digit)
        {
            let mut id = 0usize;
            for &b in &bytes[start..start + 6] {
                id = id * 10 + (b - b'0') as usize;
            }
            if id > 0
                && let Some(slot) = seen.get_mut(id - 1)
                && slot.is_none()
            {
                *slot = Some(at);
            }
            i = start + 6;
            continue;
        }
        i += 1;
    }
}

struct DraftGateInner {
    closed: AtomicBool,
    gate: Mutex<DraftGateState>,
    changed: Condvar,
}

struct DraftGateState {
    closed: bool,
    waiters: usize,
}

/// Real `shelf::Writer` behind a controllable blocking gate for testing draft backpressure.
#[derive(Clone)]
pub struct GatedDraft {
    inner: Arc<DraftGateInner>,
}

impl Default for GatedDraft {
    fn default() -> Self {
        Self::new()
    }
}

impl GatedDraft {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DraftGateInner {
                closed: AtomicBool::new(false),
                gate: Mutex::new(DraftGateState { closed: false, waiters: 0 }),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn close_gate(&self) -> DraftGateGuard {
        let mut state = self.inner.gate.lock().expect("draft gate lock");
        state.closed = true;
        self.inner.closed.store(true, Ordering::Release);
        DraftGateGuard { inner: self.inner.clone() }
    }

    pub fn has_waiter(&self) -> bool {
        self.inner.gate.lock().expect("draft gate lock").waiters > 0
    }

    pub fn drafts(&self) -> Drafts {
        let inner = self.inner.clone();
        Arc::new(move |shelf: &Shelf, id: &str| {
            let writer = shelf.open(id, LINE)?;
            Ok(Box::new(GatedDraftWriter { writer, inner: inner.clone() }) as Box<dyn Draft>)
        })
    }
}

/// Releases the draft gate on drop so test failures cannot deadlock.
pub struct DraftGateGuard {
    inner: Arc<DraftGateInner>,
}

impl Drop for DraftGateGuard {
    fn drop(&mut self) {
        let mut state = self.inner.gate.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = false;
        self.inner.closed.store(false, Ordering::Release);
        self.inner.changed.notify_all();
    }
}

struct GatedDraftWriter {
    writer: ShelfWriter,
    inner: Arc<DraftGateInner>,
}

impl Draft for GatedDraftWriter {
    fn put(&mut self, key: &str, said: &Said, now: i64) -> Result<(), zu::Error> {
        if self.inner.closed.load(Ordering::Acquire) {
            let mut state = self.inner.gate.lock().expect("draft gate lock");
            if state.closed {
                state.waiters += 1;
                let (mut state, timed_out) = self
                    .inner
                    .changed
                    .wait_timeout_while(state, Duration::from_millis(GATE_TIMEOUT_MS), |s| s.closed)
                    .expect("draft gate condvar");
                state.waiters = state.waiters.saturating_sub(1);
                if timed_out.timed_out() && state.closed {
                    drop(state);
                    panic!("bench: draft gate stayed closed past {GATE_TIMEOUT_MS} ms");
                }
            }
        }
        self.writer.put(key, Some(said), now)
    }

    fn sync(&mut self) -> Result<(), zu::Error> {
        self.writer.sync()
    }
}

/// One running simulation bench holding `Core`, `Outlet`, fake ports, probe, and log capture.
pub struct Bench {
    _tempdir: Option<tempfile::TempDir>,
    data: PathBuf,
    signed_in: bool,
    pub speech: Arc<Speech>,
    pub mic: Mic,
    pub service: Service,
    pub counting_tape: CountingTape,
    pub gated_tape: GatedTape,
    pub gated_draft: GatedDraft,
    core: Core,
    outlet: Outlet,
    probe: Arc<Mutex<ProbeState>>,
    notify: Arc<Notify>,
    drain_task: tokio::task::JoinHandle<()>,
    /// Log capture guard stays on the test thread (`!Send`).
    pub seen: Option<Seen>,
}

impl Bench {
    /// Sets up the mock key store, installs log capture before `Core::start`, and waits for the
    /// core's initial `State` snapshot.
    pub async fn start(opts: BenchOptions) -> Self {
        if !opts.keep_store {
            if opts.signed_in {
                hub::signed_in();
            } else {
                hub::nobody();
            }
        }
        if !opts.with_key {
            keys::set("soniox", None).expect("clear soniox key");
        }

        let (tempdir, data) = match opts.data {
            Some(path) => (None, path),
            None => {
                let td = tempfile::tempdir().expect("bench tempdir");
                let path = td.path().to_path_buf();
                (Some(td), path)
            }
        };

        let speech = Arc::new(opts.speech);
        let mic = Mic::new(speech.clone());
        let service = Service::new(speech.clone(), opts.delays, opts.fault);
        let counting_tape = CountingTape::new();
        let gated_tape = GatedTape::new();
        let gated_draft = GatedDraft::new();
        let reels = match opts.tape {
            TapeMode::Counting => counting_tape.reels(),
            TapeMode::Gated => gated_tape.reels(),
        };

        let config = Config {
            realtime: BENCH_WS.into(),
            soniox: opts.soniox.unwrap_or_else(|| NOWHERE.into()),
            openrouter: NOWHERE.into(),
            directory: NOWHERE.into(),
            old: opts.old,
            hear: mic.hear(),
            dial: service.dial(),
            reels,
            drafts: gated_draft.drafts(),
            ..Config::new(data.clone(), NOWHERE)
        };

        let seen = opts.capture_log.then(|| zutalk_log::capture(opts.capture_level));
        let (core, outlet) = Core::start(config).expect("core starts");

        let probe = Arc::new(Mutex::new(ProbeState::new(speech.words.len())));
        let notify = Arc::new(Notify::new());
        let drain_probe = probe.clone();
        let drain_notify = notify.clone();
        let mut drain_outlet = outlet.clone();
        let drain_task = tokio::spawn(async move {
            let (mut state_seq, initial_state) = drain_outlet.state.now();
            let (mut frame_seq, initial_frame) = drain_outlet.frame.now();
            let (mut session_seq, initial_session) = drain_outlet.session.now();
            let (mut water_seq, _) = drain_outlet.water.now();
            let (_, initial_said) = drain_outlet.said.now();
            let mut said_at = initial_said.next;
            if state_seq > 0 || frame_seq > 0 || session_seq > 0 || initial_said.next > 0 {
                let mut state = drain_probe.lock().expect("probe lock");
                if state_seq > 0 {
                    state.take_state((*initial_state).clone());
                }
                if frame_seq > 0 {
                    state.take_live(Instant::now(), &initial_frame);
                }
                if session_seq > 0 {
                    state.take_session((*initial_session).clone());
                }
                for code in &initial_said.codes {
                    state.take_notice(code.clone());
                }
                drop(state);
                drain_notify.notify_waiters();
            }
            loop {
                tokio::select! {
                    next = drain_outlet.frame.next() => match next {
                        Some((seq, frame)) => {
                            if seq > frame_seq {
                                frame_seq = seq;
                                let at = Instant::now();
                                drain_probe.lock().expect("probe lock").take_live(at, &frame);
                                drain_notify.notify_waiters();
                            }
                        }
                        None => break,
                    },
                    next = drain_outlet.session.next() => match next {
                        Some((seq, s)) => {
                            if seq > session_seq {
                                session_seq = seq;
                                drain_probe.lock().expect("probe lock").take_session((*s).clone());
                                drain_notify.notify_waiters();
                            }
                        }
                        None => break,
                    },
                    next = drain_outlet.state.next() => match next {
                        Some((seq, s)) => {
                            if seq > state_seq {
                                state_seq = seq;
                                drain_probe.lock().expect("probe lock").take_state((*s).clone());
                                drain_notify.notify_waiters();
                            }
                        }
                        None => break,
                    },
                    next = drain_outlet.water.next() => match next {
                        Some((seq, w)) => {
                            if seq > water_seq {
                                water_seq = seq;
                                if !w.marks.is_empty() {
                                    drain_probe.lock().expect("probe lock").last_water =
                                        Some((*w).clone());
                                }
                                drain_notify.notify_waiters();
                            }
                        }
                        None => break,
                    },
                    next = drain_outlet.said.next() => match next {
                        Some((_, notices)) => {
                            let first = notices.next.saturating_sub(notices.codes.len() as u64);
                            let start = said_at.max(first);
                            if notices.next > start {
                                let mut state = drain_probe.lock().expect("probe lock");
                                for code in &notices.codes[(start - first) as usize..] {
                                    state.take_notice(code.clone());
                                }
                                drop(state);
                                said_at = notices.next;
                                drain_notify.notify_waiters();
                            }
                        }
                        None => break,
                    },
                }
            }
        });

        let bench = Self {
            _tempdir: tempdir,
            data,
            signed_in: opts.signed_in,
            speech,
            mic,
            service,
            counting_tape,
            gated_tape,
            gated_draft,
            core,
            outlet,
            probe,
            notify,
            drain_task,
            seen,
        };

        bench
            .wait_state(|_| true, Duration::from_secs(10))
            .await
            .expect("initial core state arrives");
        bench
    }

    pub fn outlet(&self) -> &Outlet {
        &self.outlet
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    /// Shelf matching whichever identity this bench is running under (local by default).
    pub fn shelf(&self) -> Shelf {
        if self.signed_in { hub::shelf(&self.data) } else { hub::local_shelf(&self.data) }
    }

    pub fn send(&self, command: Command) {
        self.core.send(command).expect("the hub's mailbox accepts the command");
    }

    pub fn seen(&self) -> &Seen {
        self.seen.as_ref().expect("bench was started with capture_log = true")
    }

    pub fn tally(&self) -> Tally {
        self.core.stats().tally()
    }

    pub fn running(&self) -> Vec<zutalk_core::rt::supervise::Running> {
        self.core.running()
    }

    pub fn count(&self, name: &str) -> u64 {
        self.tally().counts.iter().find(|r| r.name == name).map_or(0, |r| r.n)
    }

    pub fn peak(&self, name: &str) -> Option<PeakRow> {
        self.tally().peaks.into_iter().find(|r| r.name == name)
    }

    pub fn queue(&self, name: &str) -> Option<QueueRow> {
        self.tally().queues.into_iter().find(|r| r.name == name)
    }

    pub fn spread(&self, name: &str) -> Option<SpreadRow> {
        self.tally().spreads.into_iter().find(|r| r.name == name)
    }

    /// Configures `spoken` and `shown` languages and waits for `Core` to acknowledge them.
    pub async fn setup(&self, spoken: &[&str], shown: &[&str]) {
        let setup = Setup {
            spoken: spoken.iter().map(|s| (*s).to_owned()).collect(),
            shown: shown.iter().map(|s| (*s).to_owned()).collect(),
            ..Setup::default()
        };
        let want_spoken = setup.spoken.clone();
        let want_shown = setup.shown.clone();
        self.send(Command::Setup { setup });
        self.wait_state(
            |s| s.setup.spoken == want_spoken && s.setup.shown == want_shown,
            Duration::from_secs(10),
        )
        .await
        .expect("setup applied");
    }

    /// Starts a recording and waits until the session and microphone are both active.
    pub async fn start_recording(&self) -> String {
        {
            let mut probe = self.probe.lock().expect("probe lock");
            probe.statuses.clear();
            probe.col0_healths.clear();
            probe.last_water = None;
        }
        self.mic.reset_for_next();
        self.send(Command::Start);
        let session = self
            .wait_session(|s| s.is_some(), Duration::from_secs(10))
            .await
            .expect("recording session started")
            .expect("session");
        let id = session.id;
        // Wait until `Task::listen` has invoked `config.hear`.
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.mic.started_at().is_none() {
            assert!(Instant::now() < deadline, "mic did not start in time");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        id
    }

    /// Stops the active recording and waits until `Outlet.session` clears.
    pub async fn stop_recording(&self) {
        self.send(Command::Stop);
        self.wait_session(|s| s.is_none(), Duration::from_secs(15))
            .await
            .expect("recording session stopped");
    }

    /// Waits until `pred` holds on `Outlet.session`, or returns `Err` on timeout.
    pub async fn wait_session(
        &self,
        pred: impl Fn(Option<&Session>) -> bool,
        limit: Duration,
    ) -> Result<Option<Session>, String> {
        let deadline = Instant::now() + limit;
        loop {
            let notified = self.notify.notified();
            let current = (*self.outlet.session.now().1).clone();
            if pred(current.as_ref()) {
                return Ok(current);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out waiting for session signal".into());
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                let latest = (*self.outlet.session.now().1).clone();
                if pred(latest.as_ref()) {
                    return Ok(latest);
                }
                return Err("timed out waiting for session signal".into());
            }
        }
    }

    /// Waits until `pred` holds on the latest `State`, or returns `Err` on timeout.
    pub async fn wait_state(
        &self,
        pred: impl Fn(&State) -> bool,
        limit: Duration,
    ) -> Result<State, String> {
        let deadline = Instant::now() + limit;
        loop {
            let notified = self.notify.notified();
            if let Some(state) = self.probe.lock().expect("probe lock").state.as_ref()
                && pred(state)
            {
                return Ok(state.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out waiting for core state".into());
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return Err("timed out waiting for core state".into());
            }
        }
    }

    /// Waits until `delta` additional live caption frames arrive.
    pub async fn wait_frames(&self, delta: u64, limit: Duration) -> Result<u64, String> {
        let start = self.frames();
        let target = start + delta;
        let deadline = Instant::now() + limit;
        loop {
            let notified = self.notify.notified();
            let now_frames = self.frames();
            if now_frames >= target {
                return Ok(now_frames);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "timed out waiting for {delta} frames (got {} of {delta})",
                    now_frames - start
                ));
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                let got = self.frames() - start;
                return Err(format!("timed out waiting for {delta} frames (got {got} of {delta})"));
            }
        }
    }

    /// Waits until `code` appears in the received notice list.
    pub async fn wait_notice(&self, code: &str, limit: Duration) -> Result<(), String> {
        let deadline = Instant::now() + limit;
        loop {
            let notified = self.notify.notified();
            if self.probe.lock().expect("probe lock").notices.iter().any(|c| c == code) {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("timed out waiting for notice `{code}`"));
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return Err(format!("timed out waiting for notice `{code}`"));
            }
        }
    }

    pub fn frames(&self) -> u64 {
        self.probe.lock().expect("probe lock").frames
    }

    pub fn non_empty_column_frames(&self) -> u64 {
        self.probe.lock().expect("probe lock").non_empty_column_frames
    }

    pub fn column_active(&self) -> Vec<bool> {
        self.probe.lock().expect("probe lock").column_active.clone()
    }

    pub fn max_frame_bytes(&self) -> usize {
        self.probe.lock().expect("probe lock").max_frame_bytes
    }

    pub fn word_seen(&self) -> Vec<Option<Instant>> {
        self.probe.lock().expect("probe lock").word_seen.clone()
    }

    pub fn status(&self) -> Option<Status> {
        (*self.outlet.session.now().1).as_ref().map(|s| s.status.clone())
    }

    pub fn statuses(&self) -> Vec<Status> {
        self.probe.lock().expect("probe lock").statuses.clone()
    }

    pub fn col0_healths(&self) -> Vec<Health> {
        self.probe.lock().expect("probe lock").col0_healths.clone()
    }

    pub fn last_water(&self) -> Option<Water> {
        self.probe.lock().expect("probe lock").last_water.clone()
    }

    pub fn notices(&self) -> Vec<String> {
        self.probe.lock().expect("probe lock").notices.clone()
    }

    pub fn state(&self) -> Option<State> {
        self.probe.lock().expect("probe lock").state.clone()
    }

    /// Serialized byte size of the latest `State` snapshot (used by `i8`).
    pub fn latest_state_bytes(&self) -> usize {
        self.probe
            .lock()
            .expect("probe lock")
            .state
            .as_ref()
            .map_or(0, |s| serde_json::to_vec(s).expect("serialize state").len())
    }

    /// Standard bench teardown: verifies signed-out invariant (`State.me` empty unless
    /// `signed_in` was explicitly opted into) and zero refused/lost log events.
    pub fn finish(self) {
        let state = self.state().expect("core emitted a state");
        if self.signed_in {
            assert!(state.me.is_some(), "signed-in bench expected State.me to be set");
        } else {
            assert!(
                state.me.is_none(),
                "bench must run signed out (State.me should be None), got {:?}",
                state.me
            );
        }
        if let Some(ref seen) = self.seen {
            assert_eq!(seen.refused(), 0, "log refused non-plain fields: {:?}", seen.all());
            assert_eq!(seen.lost(), 0, "log capture overflowed and lost events");
        }
        self.drain_task.abort();
        drop(self.core);
    }
}

/// Summary percentiles computed by sorting latency samples (no external crate).
#[derive(Clone, Copy, Debug)]
pub struct LatencySummary {
    pub count: usize,
    pub p50_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
}

pub fn summarize_latencies(mut samples: Vec<u64>) -> LatencySummary {
    if samples.is_empty() {
        return LatencySummary { count: 0, p50_ms: 0, p99_ms: 0, max_ms: 0 };
    }
    samples.sort_unstable();
    let count = samples.len();
    let at = |pct: usize| -> u64 {
        let rank = (count * pct).div_ceil(100).saturating_sub(1).min(count - 1);
        samples[rank]
    };
    LatencySummary { count, p50_ms: at(50), p99_ms: at(99), max_ms: samples[count - 1] }
}

/// Validates every JSONL log line for structure, plainness, absence of `"!"`, and absence of any
/// forbidden string (`P1.md` §3.13.14).
pub fn check_log_lines(lines: &[String], forbidden: &[String]) -> Result<(), String> {
    for (idx, raw) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let parsed: Value =
            serde_json::from_str(raw).map_err(|e| format!("line {line_no}: invalid JSON: {e}"))?;
        let Some(obj) = parsed.as_object() else {
            return Err(format!("line {line_no}: expected JSON object"));
        };
        for (key, value) in obj {
            match value {
                Value::Number(_) | Value::Bool(_) => {}
                Value::String(text) => {
                    if text == "!" {
                        return Err(format!("line {line_no}: field `{key}` was refused (\"!\")"));
                    }
                    if !zutalk_log::is_plain(text) {
                        return Err(format!(
                            "line {line_no}: field `{key}` failed is_plain: {text:?}"
                        ));
                    }
                }
                Value::Object(_) | Value::Array(_) | Value::Null => {
                    return Err(format!(
                        "line {line_no}: field `{key}` is not flat scalar: {value}"
                    ));
                }
            }
        }
        for secret in forbidden {
            if !secret.is_empty() && raw.contains(secret.as_str()) {
                return Err(format!("line {line_no}: leaked forbidden text `{secret}`"));
            }
        }
    }
    Ok(())
}
