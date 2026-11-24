//! One recovery rule for everything that can break mid-recording: an audio source, a
//! connection to the transcription service.
//!
//! When a part stops, say why, then open it again from scratch. Nothing is repaired in
//! place. The time that was lost is accounted for by whoever owns the timeline: audio
//! is padded with silence by the capture clock, and text marks a gap.

use crate::rt::stats::{Counter, Peak, Stats};
use futures_util::FutureExt;
use serde::Serialize;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use zutalk_log::{error, info, warn};

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
pub trait Part: Send {
    fn run(&mut self) -> impl Future<Output = End> + Send;
}

/// The wait before opening again, as a value: the shortest after a run that counts as
/// healthy, else twice the last, up to the longest. Time is handed in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Backoff(u64);

impl Backoff {
    /// A blip costs no more than a quarter second of reopening. Origin to be traced.
    pub const FIRST_MS: u64 = 250;
    /// The cap 0.8's Soniox stream doubled up to (1, 2, 4, then 8 s for ever).
    pub const LONGEST_MS: u64 = 8_000;
    /// A run this long was healthy, and the next wait starts short again. Origin to be traced.
    pub const HEALTHY_MS: u64 = 30_000;

    /// The wait in ms after a part that ran for `ran_ms`.
    pub fn next(&mut self, ran_ms: u64) -> u64 {
        let (first, longest) = (Self::FIRST_MS, Self::LONGEST_MS);
        self.0 =
            if ran_ms >= Self::HEALTHY_MS { first } else { (self.0 * 2).clamp(first, longest) };
        self.0
    }
}

/// How many unhealthy runs within `RESTARTS_WINDOW_MS` make `supervise` give up (`P1.md` §3.6,
/// provisional `P1.md` §4 item 3).
pub const RESTARTS_MOST_TIMES: usize = 8;
/// Sliding window in ms for counting unhealthy runs (`P1.md` §3.6, provisional `P1.md` §4 item 3).
pub const RESTARTS_WINDOW_MS: u64 = 60_000;
/// Longest stretch in ms without a healthy run before `supervise` gives up (`P1.md` §3.6,
/// provisional `P1.md` §4 item 3).
pub const RESTARTS_PATIENCE_MS: u64 = 120_000;

/// Cap on repeated unhealthy runs (`P1.md` §3.6). Time is handed in as a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restarts {
    pub most_times: usize,
    pub window_ms: u64,
    pub patience_ms: u64,
    recent_ms: VecDeque<u64>,
    since_ms: u64,
}

impl Restarts {
    /// Creates a restart policy with explicit limits.
    pub const fn new(most_times: usize, window_ms: u64, patience_ms: u64) -> Self {
        Self { most_times, window_ms, patience_ms, recent_ms: VecDeque::new(), since_ms: 0 }
    }

    /// Records a run that ended at `now_ms` after running for `ran_ms`. Returns `true` when the
    /// restart cap has been reached. A run of at least `Backoff::HEALTHY_MS` clears the history.
    pub fn note(&mut self, now_ms: u64, ran_ms: u64) -> bool {
        if ran_ms >= Backoff::HEALTHY_MS {
            self.recent_ms.clear();
            self.since_ms = now_ms;
            return false;
        }
        while self.recent_ms.front().is_some_and(|&t| now_ms.saturating_sub(t) >= self.window_ms) {
            self.recent_ms.pop_front();
        }
        self.recent_ms.push_back(now_ms);
        self.recent_ms.len() >= self.most_times
            || now_ms.saturating_sub(self.since_ms) >= self.patience_ms
    }
}

impl Default for Restarts {
    fn default() -> Self {
        Self::new(RESTARTS_MOST_TIMES, RESTARTS_WINDOW_MS, RESTARTS_PATIENCE_MS)
    }
}

/// Where a supervised part stands (`P1.md` §3.6, §3.13.11). `n` counts how many times opening it
/// has begun (`1..`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    Opening { n: u32 },
    Running { n: u32, since_ms: u64 },
    Waiting { n: u32, until_ms: u64, why: &'static str },
    Ended(End),
}

impl Watch {
    /// Stable plain code of this state for structured log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Watch::Opening { .. } => "Opening",
            Watch::Running { .. } => "Running",
            Watch::Waiting { .. } => "Waiting",
            Watch::Ended(_) => "Ended",
        }
    }

    /// Which open attempt this state belongs to (`0` once `Ended`).
    pub const fn n(&self) -> u32 {
        match *self {
            Watch::Opening { n } | Watch::Running { n, .. } | Watch::Waiting { n, .. } => n,
            Watch::Ended(_) => 0,
        }
    }
}

/// What happened to a supervised part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Opened,
    Stopped(End),
    Waited,
}

/// Side effect requested by one pure transition of [`Watch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Open,
    Run,
    Wait(u64),
    Broke(&'static str),
    GaveUp(&'static str),
}

/// Pure transition of [`Watch`]: computes the next state and effects to carry out.
pub fn step(
    watch: Watch,
    seen: Seen,
    now_ms: u64,
    backoff: &mut Backoff,
    restarts: &mut Restarts,
) -> (Watch, Vec<Effect>) {
    match (watch, seen) {
        (Watch::Opening { n }, Seen::Opened) => {
            (Watch::Running { n, since_ms: now_ms }, vec![Effect::Run])
        }
        (Watch::Opening { .. } | Watch::Running { .. }, Seen::Stopped(End::Done)) => {
            (Watch::Ended(End::Done), Vec::new())
        }
        (Watch::Opening { .. } | Watch::Running { .. }, Seen::Stopped(End::Fatal(why))) => {
            (Watch::Ended(End::Fatal(why)), Vec::new())
        }
        (Watch::Opening { n }, Seen::Stopped(End::Retry(why))) => {
            if restarts.note(now_ms, 0) {
                (Watch::Ended(End::Fatal(why)), vec![Effect::Broke(why), Effect::GaveUp(why)])
            } else {
                let wait_ms = backoff.next(0);
                (
                    Watch::Waiting { n, until_ms: now_ms + wait_ms, why },
                    vec![Effect::Broke(why), Effect::Wait(wait_ms)],
                )
            }
        }
        (Watch::Running { n, since_ms }, Seen::Stopped(End::Retry(why))) => {
            let ran_ms = now_ms.saturating_sub(since_ms);
            if restarts.note(now_ms, ran_ms) {
                (Watch::Ended(End::Fatal(why)), vec![Effect::Broke(why), Effect::GaveUp(why)])
            } else {
                let wait_ms = backoff.next(ran_ms);
                (
                    Watch::Waiting { n, until_ms: now_ms + wait_ms, why },
                    vec![Effect::Broke(why), Effect::Wait(wait_ms)],
                )
            }
        }
        (Watch::Waiting { n, .. }, Seen::Waited) => {
            (Watch::Opening { n: n + 1 }, vec![Effect::Open])
        }
        (same, _) => (same, Vec::new()),
    }
}

fn emit_state(part: &'static str, from: Watch, to: Watch, wait_ms: u64) {
    if from == to {
        return;
    }
    let n = match to {
        Watch::Ended(_) => from.n(),
        _ => to.n(),
    };
    let why = match to {
        Watch::Waiting { why, .. } => why,
        Watch::Ended(End::Done) => "done",
        Watch::Ended(End::Fatal(why) | End::Retry(why)) => why,
        Watch::Opening { .. } | Watch::Running { .. } => "",
    };
    if matches!(to, Watch::Waiting { .. }) {
        warn!(
            "part.state",
            part = part,
            from = from.code(),
            to = to.code(),
            why = why,
            wait_ms = wait_ms,
            n = n,
        );
    } else {
        info!(
            "part.state",
            part = part,
            from = from.code(),
            to = to.code(),
            why = why,
            wait_ms = wait_ms,
            n = n,
        );
    }
}

async fn supervise_with<P: Part>(
    part: &'static str,
    mut open: impl AsyncFnMut() -> Result<P, End>,
    mut broke: impl FnMut(&'static str),
    mut restarts: Restarts,
    counts: Option<(&Counter, &Counter)>,
) -> End {
    let origin = Instant::now();
    let now_ms = || origin.elapsed().as_millis() as u64;
    let mut backoff = Backoff::default();
    let mut watch = Watch::Opening { n: 1 };

    loop {
        if let Watch::Ended(end) = watch {
            return end;
        }

        let attempt = AssertUnwindSafe(open()).catch_unwind().await;
        let end = match attempt {
            Ok(Ok(mut instance)) => {
                let prev = watch;
                let (next, _) = step(watch, Seen::Opened, now_ms(), &mut backoff, &mut restarts);
                emit_state(part, prev, next, 0);
                watch = next;

                let res = AssertUnwindSafe(instance.run()).catch_unwind().await;
                drop(instance);
                res.unwrap_or(End::Retry("panic"))
            }
            Ok(Err(end)) => end,
            Err(_) => End::Retry("panic"),
        };

        let prev = watch;
        let n = prev.n();
        let (next, effects) =
            step(watch, Seen::Stopped(end), now_ms(), &mut backoff, &mut restarts);
        let wait_ms = effects
            .iter()
            .find_map(|e| if let Effect::Wait(ms) = *e { Some(ms) } else { None })
            .unwrap_or(0);
        emit_state(part, prev, next, wait_ms);
        watch = next;

        for effect in effects {
            match effect {
                Effect::Broke(why) => broke(why),
                Effect::GaveUp(why) => {
                    if let Some((_, gave_up)) = counts {
                        gave_up.add(1);
                    }
                    error!("part.gave_up", part = part, why = why, n = n);
                }
                Effect::Wait(wait_ms) => {
                    if let Some((reopened, _)) = counts {
                        reopened.add(1);
                    }
                    sleep(Duration::from_millis(wait_ms)).await;
                    let prev_wait = watch;
                    let (opened, _) =
                        step(watch, Seen::Waited, now_ms(), &mut backoff, &mut restarts);
                    emit_state(part, prev_wait, opened, 0);
                    watch = opened;
                }
                Effect::Open | Effect::Run => {}
            }
        }
    }
}

/// Keeps a named part running up to `restarts`, catching panics in both `open` and `Part::run` as
/// `End::Retry("panic")`. `broke` hears the reason each time an attempt breaks.
pub async fn supervise<P: Part>(
    part: &'static str,
    open: impl AsyncFnMut() -> Result<P, End>,
    broke: impl FnMut(&'static str),
    restarts: Restarts,
) -> End {
    supervise_with(part, open, broke, restarts, None).await
}

/// Like [`supervise`], and records restarts and give-ups into `stats` (`part.restarts`,
/// `part.gave_up`).
pub async fn supervise_counted<P: Part>(
    part: &'static str,
    open: impl AsyncFnMut() -> Result<P, End>,
    broke: impl FnMut(&'static str),
    restarts: Restarts,
    stats: &Stats,
) -> End {
    let reopened = stats.counter("part.restarts");
    let gave_up = stats.counter("part.gave_up");
    supervise_with(part, open, broke, restarts, Some((&reopened, &gave_up))).await
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

/// The most running entries one tree tracks: P1 runs under twenty tasks at once, so 64 leaves
/// room while bounding snapshots and diagnosis files.
pub const TREE_MOST_TASKS: usize = 64;

/// One task or blocking call currently running on a `Tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Running {
    pub tree: &'static str,
    pub task: &'static str,
    pub kind: &'static str,
    pub for_ms: u64,
}

/// A blocking call did not finish: it panicked, or its tree was cancelled while waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Broke;

/// Awaitable handle to a part started with [`Tree::keep`]. Resolves to `Some(End)` when the part
/// finishes on its own, or `None` if the tree was closed or cancelled first. Dropping `Kept`
/// detaches without cancelling the part.
pub struct Kept(oneshot::Receiver<End>);

impl Future for Kept {
    type Output = Option<End>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(Result::ok)
    }
}

enum Outcome {
    Done,
    Cancelled,
    Panic,
}

struct Entry {
    id: u64,
    tree: &'static str,
    task: &'static str,
    kind: &'static str,
    started: Instant,
}

#[derive(Default)]
struct TaskTable {
    next_id: u64,
    live: u64,
    rows: Vec<Entry>,
    warned: bool,
}

struct TaskGuard {
    table: Arc<Mutex<TaskTable>>,
    tasks: Peak,
    id: Option<u64>,
    finished: bool,
}

impl TaskGuard {
    fn leave(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = self.id {
            table.rows.retain(|row| row.id != id);
        }
        table.live = table.live.saturating_sub(1);
        let live = table.live;
        drop(table);
        self.tasks.set(live);
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.leave();
    }
}

/// A named tree of owned tasks (`P1.md` §3.13.10). Cancelling a tree cancels its children too;
/// `wait` closes it against new tasks and waits for all tasks started on it and its children.
#[derive(Clone)]
pub struct Tree {
    name: &'static str,
    tracker: TaskTracker,
    ancestors: Vec<TaskTracker>,
    token: CancellationToken,
    stats: Stats,
    table: Arc<Mutex<TaskTable>>,
    started: Counter,
    panics: Counter,
    tasks: Peak,
}

impl Tree {
    /// Creates a root tree with its own running table and counters on `stats`.
    pub fn root(name: &'static str, stats: Stats) -> Tree {
        let started = stats.counter("task.started");
        let panics = stats.counter("task.panics");
        let tasks = stats.peak("tree.tasks");
        Tree {
            name,
            tracker: TaskTracker::new(),
            ancestors: Vec::new(),
            token: CancellationToken::new(),
            stats,
            table: Arc::new(Mutex::new(TaskTable::default())),
            started,
            panics,
            tasks,
        }
    }

    /// Creates a child tree: cancelled with its parent, with its own `cancel` and `wait`.
    pub fn child(&self, name: &'static str) -> Tree {
        let mut ancestors = Vec::with_capacity(self.ancestors.len() + 1);
        ancestors.extend(self.ancestors.iter().cloned());
        ancestors.push(self.tracker.clone());
        Tree {
            name,
            tracker: TaskTracker::new(),
            ancestors,
            token: self.token.child_token(),
            stats: self.stats.clone(),
            table: self.table.clone(),
            started: self.started.clone(),
            panics: self.panics.clone(),
            tasks: self.tasks.clone(),
        }
    }

    fn is_closed(&self) -> bool {
        self.tracker.is_closed() || self.ancestors.iter().any(TaskTracker::is_closed)
    }

    fn enter(&self, task: &'static str, kind: &'static str, started: Instant) -> TaskGuard {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        table.live += 1;
        let live = table.live;
        let (id, emit_full) = if table.rows.len() < TREE_MOST_TASKS {
            table.next_id += 1;
            let id = table.next_id;
            table.rows.push(Entry { id, tree: self.name, task, kind, started });
            (Some(id), false)
        } else {
            let emit = !table.warned;
            table.warned = true;
            (None, emit)
        };
        drop(table);
        self.tasks.set(live);
        if emit_full {
            warn!("stats.full", kind = "tasks");
        }
        TaskGuard { table: self.table.clone(), tasks: self.tasks.clone(), id, finished: false }
    }

    /// Starts a named async task on this tree, carrying the caller's log receiver.
    pub fn spawn(&self, task: &'static str, work: impl Future<Output = ()> + Send + 'static) {
        let mut tokens = Vec::with_capacity(1 + self.ancestors.len());
        tokens.push(self.tracker.token());
        for ancestor in &self.ancestors {
            tokens.push(ancestor.token());
        }
        if self.is_closed() {
            drop(tokens);
            warn!("task.refused", tree = self.name, task = task);
            return;
        }
        let started = Instant::now();
        let mut guard = self.enter(task, "task", started);
        self.started.add(1);
        info!("task.start", tree = self.name, task = task);
        let tree_name = self.name;
        let token = self.token.clone();
        let panics = self.panics.clone();
        let tracked = async move {
            let _tokens = tokens;
            let outcome = tokio::select! {
                biased;
                () = token.cancelled() => Outcome::Cancelled,
                res = AssertUnwindSafe(work).catch_unwind() => match res {
                    Ok(()) => Outcome::Done,
                    Err(_) => Outcome::Panic,
                },
            };
            let ran_ms = started.elapsed().as_millis() as u64;
            guard.leave();
            match outcome {
                Outcome::Done => {
                    info!(
                        "task.end",
                        tree = tree_name,
                        task = task,
                        code = "done",
                        ran_ms = ran_ms
                    );
                }
                Outcome::Cancelled => {
                    info!(
                        "task.end",
                        tree = tree_name,
                        task = task,
                        code = "cancelled",
                        ran_ms = ran_ms,
                    );
                }
                Outcome::Panic => {
                    panics.add(1);
                    error!(
                        "task.end",
                        tree = tree_name,
                        task = task,
                        code = "panic",
                        ran_ms = ran_ms,
                    );
                }
            }
            drop(_tokens);
        };
        tokio::spawn(zutalk_log::carry(tracked));
    }

    /// Starts a named supervised part on this tree (`kind = "kept"`), counting `part.restarts`
    /// and `part.gave_up` on the tree's `Stats` and returning an awaitable [`Kept`] handle.
    pub fn keep<P, F, Fut, B>(
        &self,
        part: &'static str,
        mut open: F,
        broke: B,
        restarts: Restarts,
    ) -> Kept
    where
        P: Part + 'static,
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<P, End>> + Send,
        B: FnMut(&'static str) + Send + 'static,
    {
        let (done_tx, done_rx) = oneshot::channel();
        let mut tokens = Vec::with_capacity(1 + self.ancestors.len());
        tokens.push(self.tracker.token());
        for ancestor in &self.ancestors {
            tokens.push(ancestor.token());
        }
        if self.is_closed() {
            drop(tokens);
            drop(done_tx);
            warn!("task.refused", tree = self.name, task = part);
            return Kept(done_rx);
        }

        let started = Instant::now();
        let mut guard = self.enter(part, "kept", started);
        self.started.add(1);
        info!("task.start", tree = self.name, task = part);

        let tree_name = self.name;
        let token = self.token.clone();
        let reopened = self.stats.counter("part.restarts");
        let gave_up = self.stats.counter("part.gave_up");

        let tracked = async move {
            let _tokens = tokens;
            let ended = tokio::select! {
                biased;
                () = token.cancelled() => None,
                end = supervise_with(
                    part,
                    async move || open().await,
                    broke,
                    restarts,
                    Some((&reopened, &gave_up)),
                ) => Some(end),
            };
            let ran_ms = started.elapsed().as_millis() as u64;
            guard.leave();
            match ended {
                Some(end) => {
                    info!(
                        "task.end",
                        tree = tree_name,
                        task = part,
                        code = "done",
                        ran_ms = ran_ms,
                    );
                    let _ = done_tx.send(end); // dropped: caller may detach Kept when only the tree waits
                }
                None => {
                    info!(
                        "task.end",
                        tree = tree_name,
                        task = part,
                        code = "cancelled",
                        ran_ms = ran_ms,
                    );
                }
            }
            drop(_tokens);
        };
        tokio::spawn(zutalk_log::carry(tracked));
        Kept(done_rx)
    }

    /// Runs a named blocking call on Tokio's blocking pool, carrying the caller's log receiver.
    /// Does not emit `task.start`/`task.end`; panics and tree cancellation return `Err(Broke)`.
    pub async fn blocking<R: Send + 'static>(
        &self,
        task: &'static str,
        work: impl FnOnce() -> R + Send + 'static,
    ) -> Result<R, Broke> {
        let carrier = zutalk_log::carrier();
        let guard = self.enter(task, "blocking", Instant::now());
        let handle = tokio::task::spawn_blocking(move || {
            let _entered = carrier.enter();
            let _guard = guard;
            work()
        });
        tokio::select! {
            res = handle => res.map_err(|_| Broke),
            () = self.token.cancelled() => Err(Broke),
        }
    }

    /// Cancels this tree and every child tree under it.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Resolves once this tree (or an ancestor) has been cancelled.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Closes this tree against new tasks and waits for all tasks on it and its children to end.
    pub async fn wait(&self) {
        self.tracker.close();
        self.tracker.wait().await;
    }

    /// The `Stats` attached to this tree.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Snapshot of everything currently running on this tree and its relatives, in start order.
    pub fn running(&self) -> Vec<Running> {
        let table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        table
            .rows
            .iter()
            .map(|row| Running {
                tree: row.tree,
                task: row.task,
                kind: row.kind,
                for_ms: row.started.elapsed().as_millis() as u64,
            })
            .collect()
    }
}

/// Starts a named OS thread carrying the caller's log receiver. Whoever holds the handle owns it.
pub fn thread(
    name: &'static str,
    work: impl FnOnce() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let carrier = zutalk_log::carrier();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _entered = carrier.enter();
            let started = std::time::Instant::now();
            info!("thread.start", thread = name);
            let ended = std::panic::catch_unwind(AssertUnwindSafe(work));
            let ran_ms = started.elapsed().as_millis() as u64;
            match ended {
                Ok(()) => info!("thread.end", thread = name, code = "done", ran_ms = ran_ms),
                Err(_) => error!("thread.end", thread = name, code = "panic", ran_ms = ran_ms),
            }
        })
        .expect("a named thread starts")
}
