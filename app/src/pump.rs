//! Exclusive shell event pump: pure `Fan` state machine plus supervised `shell.pump` loop
//! (`P1.md` §3.9, `P1-23`).
//!
//! No locks, no synchronous window queries, and no Tauri coupling inside `Fan`. A single task
//! owns the window sink map and latest snapshots; if the inner run panics, its `Drop` guard
//! returns the live state to the supervisor via a lock-free oneshot so windows never need to
//! re-subscribe.

use crate::windows::CaptionWindow;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use tokio::sync::oneshot;
use zutalk_core::app::hub::Outlet;
use zutalk_core::app::proto::{Event, Signal, Stamped, State};
use zutalk_core::rt::queue::{Rx, Tx};
use zutalk_core::rt::stats::{Counter, Stats};
use zutalk_core::rt::supervise::{End, Kept, Part, Restarts, Tree};
use zutalk_log::{Id, info, warn};

/// Window subscription requests buffered for the pump: only 2-3 windows exist and `subscribe` is
/// async (`P1.md` §3.7, `P1-23`).
pub const SHELL_JOINS_MSGS: usize = 8;

/// Sender half passed to the async `subscribe` Tauri command.
pub type Joins<S> = Tx<(String, S)>;

/// One window's outbound channel: `false` means the page is gone.
pub trait Sink: Send + 'static {
    fn send(&self, event: Event) -> bool;
}

impl Sink for tauri::ipc::Channel<Event> {
    fn send(&self, event: Event) -> bool {
        tauri::ipc::Channel::send(self, event).is_ok()
    }
}

enum Delivery {
    Ok,
    Closed,
    Panic,
}

impl Delivery {
    fn why(self) -> &'static str {
        match self {
            Delivery::Ok => "",
            Delivery::Closed => "closed",
            Delivery::Panic => "panic",
        }
    }
}

fn deliver<S: Sink>(sink: &S, event: Event) -> Delivery {
    match catch_unwind(AssertUnwindSafe(|| sink.send(event))) {
        Ok(true) => Delivery::Ok,
        Ok(false) => Delivery::Closed,
        Err(_) => Delivery::Panic,
    }
}

/// Pure state machine over window sinks and the latest value of `State` and each `Signal`.
/// Never starts tasks, reads a clock, touches Tauri, or emits log events (`P1-23`).
pub struct Fan<S> {
    sinks: BTreeMap<String, S>,
    state: Option<Arc<State>>,
    frame: Option<Stamped>,
    session: Option<Stamped>,
    level: Option<Stamped>,
    water: Option<Stamped>,
    stats: Option<Stamped>,
}

impl<S> Default for Fan<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Fan<S> {
    pub fn new() -> Self {
        Self {
            sinks: BTreeMap::new(),
            state: None,
            frame: None,
            session: None,
            level: None,
            water: None,
            stats: None,
        }
    }
}

impl<S: Sink> Fan<S> {
    /// Subscribes `window` with `sink`, replacing any previous sink for that window (`true` when
    /// an old sink was replaced). Immediately replays the latest `State` and then each signal's
    /// latest value in fixed order (`Frame`, `Session`, `Level`, `Water`, `Stats`); notices are
    /// never replayed.
    pub fn join(&mut self, window: &str, sink: S) -> bool {
        self.join_checked(window, sink).0
    }

    fn join_checked(&mut self, window: &str, sink: S) -> (bool, Option<&'static str>) {
        let replaced = self.sinks.remove(window).is_some();
        let replay = [
            self.state.clone().map(Event::State),
            self.frame.clone().map(Event::Signal),
            self.session.clone().map(Event::Signal),
            self.level.clone().map(Event::Signal),
            self.water.clone().map(Event::Signal),
            self.stats.clone().map(Event::Signal),
        ];
        for event in replay.into_iter().flatten() {
            match deliver(&sink, event) {
                Delivery::Ok => {}
                gone => return (replaced, Some(gone.why())),
            }
        }
        self.sinks.insert(window.to_owned(), sink);
        (replaced, None)
    }

    /// Records `event` as the latest of its kind (notices are not kept), enforces monotonic
    /// per-signal sequence numbers, pushes to every subscribed sink, drops any closed or
    /// panicking sink, and returns `(window, why)` (`"closed"` or `"panic"`) for each dropped sink.
    pub fn push(&mut self, event: Event) -> Vec<(String, &'static str)> {
        match &event {
            Event::State(state) => self.state = Some(state.clone()),
            Event::Signal(stamped) => {
                let slot = match &stamped.signal {
                    Signal::Frame(_) => &mut self.frame,
                    Signal::Session(_) => &mut self.session,
                    Signal::Level(_) => &mut self.level,
                    Signal::Water(_) => &mut self.water,
                    Signal::Stats(_) => &mut self.stats,
                };
                if slot.as_ref().is_some_and(|prev| stamped.seq <= prev.seq) {
                    return Vec::new();
                }
                *slot = Some(stamped.clone());
            }
            Event::Notice(_) => {}
        }

        let mut gone = Vec::new();
        for (window, sink) in &self.sinks {
            match deliver(sink, event.clone()) {
                Delivery::Ok => {}
                bad => gone.push((window.clone(), bad.why())),
            }
        }
        for (window, _) in &gone {
            self.sinks.remove(window);
        }
        gone
    }
}

struct PumpState<S> {
    fan: Fan<S>,
    joins: Rx<(String, S)>,
    outlet: Outlet,
    seeded: bool,
    state_seq: u64,
    frame_seq: u64,
    session_seq: u64,
    level_seq: u64,
    water_seq: u64,
    stats_seq: u64,
    said_at: u64,
    captions_shown: bool,
    pushed: Counter,
    sinks_gone: Counter,
}

impl<S: Sink> PumpState<S> {
    fn new(outlet: Outlet, joins: Rx<(String, S)>, stats: &Stats) -> Self {
        Self {
            fan: Fan::new(),
            joins,
            outlet,
            seeded: false,
            state_seq: 0,
            frame_seq: 0,
            session_seq: 0,
            level_seq: 0,
            water_seq: 0,
            stats_seq: 0,
            said_at: 0,
            captions_shown: false,
            pushed: stats.counter("shell.pushed"),
            sinks_gone: stats.counter("shell.sinks.gone"),
        }
    }

    fn note_gone(&self, gone: Vec<(String, &'static str)>) {
        for (window, why) in gone {
            warn!("shell.sink.gone", window = Id(&window), why = why);
            self.sinks_gone.add(1);
        }
    }

    fn push_event(&mut self, event: Event) {
        let gone = self.fan.push(event);
        self.note_gone(gone);
    }

    fn handle_state<C: Fn(bool) -> CaptionWindow>(
        &mut self,
        seq: u64,
        state: Arc<State>,
        on_captions: &C,
    ) {
        if seq <= self.state_seq {
            return;
        }
        self.state_seq = seq;
        let wanted = state.captions;
        self.push_event(Event::State(state));
        if wanted != self.captions_shown {
            self.captions_shown = wanted;
            let outcome = on_captions(wanted);
            match outcome {
                CaptionWindow::Ok => info!("shell.captions", shown = wanted, code = "ok"),
                other => warn!("shell.captions", shown = wanted, code = other.code()),
            }
        }
    }

    fn handle_frame<P: Fn(u64)>(
        &mut self,
        seq: u64,
        frame: Arc<zutalk_core::app::proto::Frame>,
        on_pushed: &P,
    ) {
        if seq <= self.frame_seq {
            return;
        }
        self.frame_seq = seq;
        self.push_event(Event::Signal(Stamped { seq, signal: Signal::Frame(frame) }));
        self.pushed.add(1);
        on_pushed(seq);
    }

    fn handle_session(&mut self, seq: u64, session: Arc<Option<zutalk_core::app::proto::Session>>) {
        if seq <= self.session_seq {
            return;
        }
        self.session_seq = seq;
        self.push_event(Event::Signal(Stamped { seq, signal: Signal::Session(session) }));
    }

    fn handle_level(&mut self, seq: u64, level: Arc<zutalk_core::app::proto::Level>) {
        if seq <= self.level_seq {
            return;
        }
        self.level_seq = seq;
        self.push_event(Event::Signal(Stamped { seq, signal: Signal::Level(level) }));
    }

    fn handle_water(&mut self, seq: u64, water: Arc<zutalk_core::rt::water::Water>) {
        if seq <= self.water_seq {
            return;
        }
        self.water_seq = seq;
        self.push_event(Event::Signal(Stamped { seq, signal: Signal::Water(water) }));
    }

    fn handle_stats(&mut self, seq: u64, tally: Arc<zutalk_core::rt::stats::Tally>) {
        if seq <= self.stats_seq {
            return;
        }
        self.stats_seq = seq;
        self.push_event(Event::Signal(Stamped { seq, signal: Signal::Stats(tally) }));
    }

    fn handle_notices(&mut self, notices: &zutalk_core::app::proto::Notices) {
        let first = notices.next.saturating_sub(notices.codes.len() as u64);
        let start = self.said_at.max(first);
        if notices.next > start {
            for code in &notices.codes[(start - first) as usize..] {
                self.push_event(Event::Notice(code.clone()));
            }
            self.said_at = notices.next;
        }
    }

    fn seed<P: Fn(u64), C: Fn(bool) -> CaptionWindow>(&mut self, on_pushed: &P, on_captions: &C) {
        if self.seeded {
            return;
        }
        self.seeded = true;
        let (seq, state) = self.outlet.state.now();
        if seq > 0 {
            self.handle_state(seq, state, on_captions);
        }
        let (seq, frame) = self.outlet.frame.now();
        if seq > 0 {
            self.handle_frame(seq, frame, on_pushed);
        }
        let (seq, session) = self.outlet.session.now();
        if seq > 0 {
            self.handle_session(seq, session);
        }
        let (seq, level) = self.outlet.level.now();
        if seq > 0 {
            self.handle_level(seq, level);
        }
        let (seq, water) = self.outlet.water.now();
        if seq > 0 {
            self.handle_water(seq, water);
        }
        let (seq, stats) = self.outlet.stats.now();
        if seq > 0 {
            self.handle_stats(seq, stats);
        }
        let (_, notices) = self.outlet.said.now();
        self.handle_notices(&notices);
    }

    async fn step_loop<P: Fn(u64), C: Fn(bool) -> CaptionWindow>(
        &mut self,
        on_pushed: &P,
        on_captions: &C,
    ) -> End {
        self.seed(on_pushed, on_captions);
        loop {
            tokio::select! {
                join = self.joins.recv() => match join {
                    Some((window, sink)) => {
                        let (replaced, gone) = self.fan.join_checked(&window, sink);
                        info!("shell.join", window = Id(&window), replaced = replaced);
                        if let Some(why) = gone {
                            warn!("shell.sink.gone", window = Id(&window), why = why);
                            self.sinks_gone.add(1);
                        }
                    }
                    None => return End::Done,
                },
                next = self.outlet.state.next() => match next {
                    Some((seq, state)) => self.handle_state(seq, state, on_captions),
                    None => return End::Done,
                },
                next = self.outlet.frame.next() => match next {
                    Some((seq, frame)) => self.handle_frame(seq, frame, on_pushed),
                    None => return End::Done,
                },
                next = self.outlet.session.next() => match next {
                    Some((seq, session)) => self.handle_session(seq, session),
                    None => return End::Done,
                },
                next = self.outlet.level.next() => match next {
                    Some((seq, level)) => self.handle_level(seq, level),
                    None => return End::Done,
                },
                next = self.outlet.water.next() => match next {
                    Some((seq, water)) => self.handle_water(seq, water),
                    None => return End::Done,
                },
                next = self.outlet.stats.next() => match next {
                    Some((seq, tally)) => self.handle_stats(seq, tally),
                    None => return End::Done,
                },
                next = self.outlet.said.next() => match next {
                    Some((_, notices)) => self.handle_notices(&notices),
                    None => return End::Done,
                },
            }
        }
    }
}

/// Runs the shell pump directly without supervision (used by unit/integration tests and inside
/// the supervised [`start`] part).
pub async fn run<S: Sink>(
    outlet: Outlet,
    joins: Rx<(String, S)>,
    stats: Stats,
    on_pushed: impl Fn(u64) + Send + 'static,
    on_captions: impl Fn(bool) -> CaptionWindow + Send + 'static,
) {
    let mut state = PumpState::new(outlet, joins, &stats);
    let _ = state.step_loop(&on_pushed, &on_captions).await; // dropped: End::Done marks normal shutdown
}

struct PumpPart<S, P, C> {
    state: Option<PumpState<S>>,
    give_back: Option<oneshot::Sender<PumpState<S>>>,
    on_pushed: Arc<P>,
    on_captions: Arc<C>,
}

impl<S, P, C> Drop for PumpPart<S, P, C> {
    fn drop(&mut self) {
        if let (Some(state), Some(tx)) = (self.state.take(), self.give_back.take()) {
            let _ = tx.send(state); // dropped: supervisor ended or tree was cancelled
        }
    }
}

impl<S, P, C> Part for PumpPart<S, P, C>
where
    S: Sink,
    P: Fn(u64) + Send + Sync + 'static,
    C: Fn(bool) -> CaptionWindow + Send + Sync + 'static,
{
    async fn run(&mut self) -> End {
        let Some(state) = self.state.as_mut() else {
            return End::Fatal("panic");
        };
        state.step_loop(self.on_pushed.as_ref(), self.on_captions.as_ref()).await
    }
}

/// Starts `shell.pump` as a supervised part on `shell` (`shell.keep("shell.pump", ...)`).
///
/// `Fan` and the `joins` receiver survive panics inside the inner pump loop via a lock-free
/// oneshot handoff on drop, so subscribed windows keep receiving events after a restart without
/// re-subscribing (`P1-23`).
pub fn start<S, P, C>(
    shell: &Tree,
    outlet: Outlet,
    joins: Rx<(String, S)>,
    on_pushed: P,
    on_captions: C,
) -> Kept
where
    S: Sink,
    P: Fn(u64) + Send + Sync + 'static,
    C: Fn(bool) -> CaptionWindow + Send + Sync + 'static,
{
    let initial = PumpState::new(outlet, joins, shell.stats());
    let mut initial = Some(initial);
    let mut returned: Option<oneshot::Receiver<PumpState<S>>> = None;
    let on_pushed = Arc::new(on_pushed);
    let on_captions = Arc::new(on_captions);

    shell.keep(
        "shell.pump",
        move || {
            let state =
                initial.take().or_else(|| returned.take().and_then(|mut rx| rx.try_recv().ok()));
            let on_pushed = on_pushed.clone();
            let on_captions = on_captions.clone();
            let next = state.map(|state| {
                let (tx, rx) = oneshot::channel();
                returned = Some(rx);
                PumpPart { state: Some(state), give_back: Some(tx), on_pushed, on_captions }
            });
            async move { next.ok_or(End::Fatal("panic")) }
        },
        |_| {},
        Restarts::default(),
    )
}
