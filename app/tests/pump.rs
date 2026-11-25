//! Exhaustive interleaving and supervised-pump tests for the shell's lock-free `Fan` and
//! `shell.pump` (`P1.md` §3.9, `P1-23`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zutalk::pump::{self, Fan, SHELL_JOINS_MSGS, Sink};
use zutalk::windows::{CaptionWindow, CaptionWindowOp};
use zutalk_core::app::hub::Outlet;
use zutalk_core::app::proto::{
    Event, Frame, Invalidate, Invalidations, Level as SoundLevel, Notices, Progress, Session,
    Signal, Stamped, State, Status, ViewKind, Writing,
};
use zutalk_core::rt::queue::{self, Full};
use zutalk_core::rt::signal::Register;
use zutalk_core::rt::stats::{Stats, Tally};
use zutalk_core::rt::supervise::{Backoff, Tree};
use zutalk_core::rt::water::Water;
use zutalk_core::view::query::KEEP_INVALIDATIONS_EVENTS;
use zutalk_log::Level;

#[derive(Debug, Clone)]
struct RecordingSink {
    events: Arc<Mutex<Vec<Event>>>,
    open: Arc<AtomicBool>,
}

impl RecordingSink {
    fn new() -> Self {
        Self { events: Arc::new(Mutex::new(Vec::new())), open: Arc::new(AtomicBool::new(true)) }
    }

    fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
    }

    fn take(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

impl Sink for RecordingSink {
    fn send(&self, event: Event) -> bool {
        if !self.open.load(Ordering::SeqCst) {
            return false;
        }
        self.events.lock().unwrap().push(event);
        true
    }
}

struct PanickingSink;

impl Sink for PanickingSink {
    fn send(&self, _event: Event) -> bool {
        panic!("test injected sink panic");
    }
}

struct Registers {
    state: Register<State>,
    frame: Register<Frame>,
    session: Register<Option<Session>>,
    level: Register<SoundLevel>,
    progress: Register<Progress>,
    invalidated: Register<Invalidations>,
    water: Register<Water>,
    stats: Register<Tally>,
    said: Register<Notices>,
}

impl Registers {
    fn new() -> (Self, Outlet) {
        let state = Register::new(State::default());
        let frame = Register::new(Frame::default());
        let session = Register::new(None);
        let level = Register::new(SoundLevel::default());
        let progress = Register::new(Progress::default());
        let invalidated = Register::new(Invalidations::default());
        let water = Register::new(Water::default());
        let stats = Register::new(Tally::default());
        let said = Register::new(Notices::default());
        let outlet = Outlet {
            state: state.read(),
            frame: frame.read(),
            session: session.read(),
            level: level.read(),
            progress: progress.read(),
            invalidated: invalidated.read(),
            water: water.read(),
            stats: stats.read(),
            said: said.read(),
        };
        (Self { state, frame, session, level, progress, invalidated, water, stats, said }, outlet)
    }

    fn invalidate(&self, view: ViewKind, key: Option<String>) {
        self.invalidated.change(|cur| {
            let mut next = cur.clone();
            let v = next.next + 1;
            next.push(view, key.clone(), v);
            Some(next)
        });
    }

    fn say(&self, code: &str) {
        self.said.change(|notices| {
            let mut codes = notices.codes.clone();
            codes.push(code.to_owned());
            if codes.len() > Notices::KEEP_CODES {
                codes.drain(..codes.len() - Notices::KEEP_CODES);
            }
            Some(Notices { next: notices.next + 1, codes })
        });
    }
}

fn frame_event(seq: u64) -> Event {
    Event::Signal(Stamped {
        seq,
        signal: Signal::Frame(Arc::new(Frame { t_audio: (seq * 100) as u32, columns: Vec::new() })),
    })
}

fn session_event(seq: u64, id: &str) -> Event {
    Event::Signal(Stamped {
        seq,
        signal: Signal::Session(Arc::new(Some(Session {
            id: id.into(),
            started: 1_000,
            status: Status::Recording,
            outbound: false,
            viewers: 0,
            event_title: None,
        }))),
    })
}

fn frame_seqs(events: &[Event]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Signal(Stamped { seq, signal: Signal::Frame(_) }) => Some(*seq),
            _ => None,
        })
        .collect()
}

fn last_state(events: &[Event]) -> Option<Arc<State>> {
    events.iter().rev().find_map(|e| match e {
        Event::State(s) => Some(s.clone()),
        _ => None,
    })
}

fn notice_count(events: &[Event], code: &str) -> usize {
    events.iter().filter(|e| matches!(e, Event::Notice(c) if c == code)).count()
}

// ---- 1. Exhaustive 120-interleaving test ----

#[test]
fn every_interleaving_ends_with_each_window_holding_the_latest() {
    let state1 = Arc::new(State { captions: false, ..State::default() });
    let state2 = Arc::new(State { captions: true, ..State::default() });
    let pushes = [
        Event::State(state1),
        frame_event(1),
        session_event(1, "rec-1"),
        frame_event(2),
        Event::Notice("x".into()),
        Event::State(state2.clone()),
        frame_event(3),
    ];

    let total = 3 + pushes.len(); // 10 actions -> C(10, 3) = 120 interleavings
    let mut interleavings = 0usize;

    for mask in 0u32..(1u32 << total) {
        if mask.count_ones() != 3 {
            continue;
        }
        interleavings += 1;

        let mut fan = Fan::new();
        let a = RecordingSink::new();
        let b = RecordingSink::new();
        let c = RecordingSink::new();

        let mut join_idx = 0usize;
        let mut push_idx = 0usize;
        let mut a_len_at_replace = None;
        let mut a_active = false;
        let mut b_active = false;
        let mut c_active = false;
        let mut notice_expected = (0usize, 0usize, 0usize);

        for bit in 0..total {
            if (mask >> bit) & 1 == 1 {
                match join_idx {
                    0 => {
                        let replaced = fan.join("main", a.clone());
                        assert!(!replaced, "first main join must not report replaced");
                        a_active = true;
                    }
                    1 => {
                        let replaced = fan.join("captions", b.clone());
                        assert!(!replaced, "first captions join must not report replaced");
                        b_active = true;
                    }
                    2 => {
                        let replaced = fan.join("main", c.clone());
                        assert!(replaced, "reloaded main join must report replaced=true");
                        a_len_at_replace = Some(a.len());
                        a_active = false;
                        c_active = true;
                    }
                    _ => unreachable!(),
                }
                join_idx += 1;
            } else {
                let event = pushes[push_idx].clone();
                if matches!(&event, Event::Notice(code) if code == "x") {
                    notice_expected =
                        (usize::from(a_active), usize::from(b_active), usize::from(c_active));
                }
                let gone = fan.push(event);
                assert!(gone.is_empty());
                push_idx += 1;
            }
        }

        let a_ev = a.take();
        let b_ev = b.take();
        let c_ev = c.take();

        // b and c both hold State₂ and Frame 3 as their final values.
        assert_eq!(last_state(&b_ev).as_deref(), Some(&*state2), "mask={mask:010b}");
        assert_eq!(last_state(&c_ev).as_deref(), Some(&*state2), "mask={mask:010b}");
        assert_eq!(frame_seqs(&b_ev).last().copied(), Some(3), "mask={mask:010b}");
        assert_eq!(frame_seqs(&c_ev).last().copied(), Some(3), "mask={mask:010b}");

        // Every channel sees strictly increasing Frame seqs.
        for (name, ev) in [("a", &a_ev), ("b", &b_ev), ("c", &c_ev)] {
            let seqs = frame_seqs(ev);
            assert!(
                seqs.windows(2).all(|w| w[0] < w[1]),
                "non-monotonic frame seqs on {name} (mask={mask:010b}): {seqs:?}"
            );
        }

        // After c joined, a received nothing more.
        assert_eq!(Some(a_ev.len()), a_len_at_replace, "old sink received events after reload");

        // Notice x went only to channels subscribed at the moment it was pushed, once each.
        assert_eq!(
            notice_count(&a_ev, "x"),
            notice_expected.0,
            "a notice count (mask={mask:010b})"
        );
        assert_eq!(
            notice_count(&b_ev, "x"),
            notice_expected.1,
            "b notice count (mask={mask:010b})"
        );
        assert_eq!(
            notice_count(&c_ev, "x"),
            notice_expected.2,
            "c notice count (mask={mask:010b})"
        );
    }

    assert_eq!(interleavings, 120);
}

// ---- 2. Closed sink removed ----

#[test]
fn a_page_that_is_gone_is_let_go() {
    let mut fan = Fan::new();
    let gone_sink = RecordingSink::new();
    let live_sink = RecordingSink::new();
    fan.join("main", gone_sink.clone());
    fan.join("captions", live_sink.clone());

    assert!(fan.push(frame_event(1)).is_empty());
    assert_eq!(gone_sink.len(), 1);

    gone_sink.close();
    let dropped = fan.push(frame_event(2));
    assert_eq!(dropped, vec![("main".to_owned(), "closed")]);

    // Subsequent pushes do not touch the dropped sink and keep reaching live sinks.
    assert!(fan.push(frame_event(3)).is_empty());
    assert_eq!(gone_sink.len(), 1);
    assert_eq!(frame_seqs(&live_sink.take()), vec![1, 2, 3]);
}

// ---- 3. Monotonic signal seq ----

#[test]
fn a_signal_older_than_the_last_one_sent_is_not_sent() {
    let mut fan = Fan::new();
    let sink = RecordingSink::new();
    fan.join("main", sink.clone());

    assert!(fan.push(frame_event(5)).is_empty());
    assert!(fan.push(frame_event(5)).is_empty());
    assert!(fan.push(frame_event(4)).is_empty());
    assert!(fan.push(session_event(2, "rec-2")).is_empty());
    assert!(fan.push(session_event(1, "rec-1")).is_empty());
    assert!(fan.push(frame_event(6)).is_empty());

    let ev = sink.take();
    assert_eq!(frame_seqs(&ev), vec![5, 6]);
    let sessions: Vec<u64> = ev
        .iter()
        .filter_map(|e| match e {
            Event::Signal(Stamped { seq, signal: Signal::Session(_) }) => Some(*seq),
            _ => None,
        })
        .collect();
    assert_eq!(sessions, vec![2]);
}

// ---- 4. Late join replays State + every signal's latest ----

#[tokio::test(start_paused = true)]
async fn a_window_that_joins_late_gets_the_state_and_every_signals_latest() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    // Pre-populate State and all 5 signals before the window joins.
    regs.state.set(State { captions: false, ..State::default() });
    regs.frame.set(Frame { t_audio: 100, columns: Vec::new() });
    regs.session.set(Some(Session {
        id: "s1".into(),
        started: 10,
        status: Status::Recording,
        outbound: false,
        viewers: 0,
        event_title: None,
    }));
    regs.level.set(SoundLevel { peak: 500, rms: 200 });
    let mut water = Water::default();
    water.now_ms = 1000;
    regs.water.set(water);
    regs.stats.set(Tally::default());

    let pushed_seqs = Arc::new(Mutex::new(Vec::<u64>::new()));
    let pushed_rec = pushed_seqs.clone();
    let run_stats = stats.clone();
    let task = tokio::spawn(zutalk_log::carry(pump::run(
        outlet,
        joins_rx,
        run_stats,
        move |seq| pushed_rec.lock().unwrap().push(seq),
        |_| CaptionWindow::Ok,
    )));

    // Let pump seed initial register values.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let sink = RecordingSink::new();
    joins_tx.send(("main".into(), sink.clone())).await.expect("join sent");
    tokio::time::sleep(Duration::from_millis(10)).await;

    let first = sink.take();
    assert_eq!(first.len(), 6, "expected State + 5 signals, got {first:?}");
    assert!(matches!(first[0], Event::State(_)));
    assert!(matches!(first[1], Event::Signal(Stamped { seq: 1, signal: Signal::Frame(_) })));
    assert!(matches!(first[2], Event::Signal(Stamped { seq: 1, signal: Signal::Session(_) })));
    assert!(matches!(first[3], Event::Signal(Stamped { seq: 1, signal: Signal::Level(_) })));
    assert!(matches!(first[4], Event::Signal(Stamped { seq: 1, signal: Signal::Water(_) })));
    assert!(matches!(first[5], Event::Signal(Stamped { seq: 1, signal: Signal::Stats(_) })));

    // Subsequent updates arrive one per set and report pushed frame seq.
    regs.frame.set(Frame { t_audio: 200, columns: Vec::new() });
    regs.level.set(SoundLevel { peak: 900, rms: 300 });
    tokio::time::sleep(Duration::from_millis(10)).await;

    let all = sink.take();
    assert_eq!(frame_seqs(&all), vec![1, 2]);
    assert_eq!(*pushed_seqs.lock().unwrap(), vec![1, 2]);

    let joins_log = seen.of("shell.join");
    assert_eq!(joins_log.len(), 1);
    assert_eq!(joins_log[0].text("window"), Some("main"));
    assert_eq!(joins_log[0].bool("replaced"), Some(false));

    let pushed_count =
        stats.tally().counts.into_iter().find(|c| c.name == "shell.pushed").map(|c| c.n);
    assert_eq!(pushed_count, Some(2));
    assert_eq!(seen.refused(), 0);

    drop(joins_tx);
    task.await.expect("pump exits cleanly");
}

// ---- 5. Reload mid-stream ----

#[tokio::test(start_paused = true)]
async fn a_reload_while_frames_are_coming_loses_nothing_and_the_old_channel_goes_quiet() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    let task = tokio::spawn(zutalk_log::carry(pump::run(
        outlet,
        joins_rx,
        stats,
        |_| {},
        |_| CaptionWindow::Ok,
    )));

    let old = RecordingSink::new();
    joins_tx.send(("main".into(), old.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    for i in 1..=5u32 {
        regs.frame.set(Frame { t_audio: i * 100, columns: Vec::new() });
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let reloaded = RecordingSink::new();
    joins_tx.send(("main".into(), reloaded.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let old_count_after_reload = old.len();

    for i in 6..=10u32 {
        regs.frame.set(Frame { t_audio: i * 100, columns: Vec::new() });
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(old.len(), old_count_after_reload, "old channel must stay silent after reload");
    assert_eq!(frame_seqs(&reloaded.take()), vec![5, 6, 7, 8, 9, 10]);

    let joins_log = seen.of("shell.join");
    assert_eq!(joins_log.len(), 2);
    assert_eq!(joins_log[0].bool("replaced"), Some(false));
    assert_eq!(joins_log[1].bool("replaced"), Some(true));
    assert_eq!(seen.refused(), 0);

    drop(joins_tx);
    task.await.unwrap();
}

// ---- 6. Captions window follows State without querying ----

#[tokio::test(start_paused = true)]
async fn the_captions_window_follows_the_state_and_is_never_asked() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    let calls = Arc::new(Mutex::new(Vec::<bool>::new()));
    let calls_cb = calls.clone();
    let task = tokio::spawn(zutalk_log::carry(pump::run(
        outlet,
        joins_rx,
        stats,
        |_| {},
        move |ops| {
            if ops.contains(&CaptionWindowOp::Show) {
                calls_cb.lock().unwrap().push(true);
            } else if ops.contains(&CaptionWindowOp::Hide) {
                calls_cb.lock().unwrap().push(false);
            }
            CaptionWindow::Ok
        },
    )));

    for wanted in [false, true, true, false] {
        regs.state.set(State { captions: wanted, ..State::default() });
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(*calls.lock().unwrap(), vec![true, false]);
    let cap_events = seen.of("shell.captions");
    assert_eq!(cap_events.len(), 2);
    assert_eq!(cap_events[0].bool("shown"), Some(true));
    assert_eq!(cap_events[1].bool("shown"), Some(false));
    assert_eq!(seen.refused(), 0);

    drop(joins_tx);
    task.await.unwrap();
}

// ---- 7. Notice cursor & ring overflow ----

#[tokio::test(start_paused = true)]
async fn codes_are_said_once_each() {
    let stats = Stats::new();
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    let task = tokio::spawn(pump::run(outlet, joins_rx, stats, |_| {}, |_| CaptionWindow::Ok));
    let sink = RecordingSink::new();
    joins_tx.send(("main".into(), sink.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    for code in ["n1", "n2", "n3"] {
        regs.say(code);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let first: Vec<String> = sink
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Event::Notice(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(first, vec!["n1", "n2", "n3"]);

    // Burst 20 notices synchronously before yielding (more than Notices::KEEP_CODES = 16).
    for i in 1..=20 {
        regs.say(&format!("b{i}"));
    }
    tokio::time::sleep(Duration::from_millis(10)).await;

    let all: Vec<String> = sink
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Event::Notice(c) => Some(c),
            _ => None,
        })
        .collect();
    // No duplicates, and the final delivered code is the 20th.
    let mut dedup = all.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), all.len(), "notices must never duplicate: {all:?}");
    assert_eq!(all.last().map(String::as_str), Some("b20"));
    assert_eq!(all.len(), 3 + Notices::KEEP_CODES);

    drop(joins_tx);
    task.await.unwrap();
}

// ---- 8. Panicking sink isolated ----

#[tokio::test(start_paused = true)]
async fn a_channel_that_panics_does_not_end_the_pump() {
    struct Boxed(Box<dyn Sink>);
    impl Sink for Boxed {
        fn send(&self, event: Event) -> bool {
            self.0.send(event)
        }
    }

    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let shell = Tree::root("shell", stats.clone());
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) =
        queue::bounded::<(String, Boxed)>(&stats, "shell.joins", SHELL_JOINS_MSGS, Full::Wait);

    let _kept = pump::start(&shell, outlet, joins_rx, |_| {}, |_| CaptionWindow::Ok);
    tokio::time::sleep(Duration::from_millis(10)).await;
    let states_after_open = seen.of("part.state").len();

    let good = RecordingSink::new();
    assert!(joins_tx.send(("good".into(), Boxed(Box::new(good.clone())))).await.is_ok());
    assert!(joins_tx.send(("bad".into(), Boxed(Box::new(PanickingSink)))).await.is_ok());
    tokio::time::sleep(Duration::from_millis(10)).await;

    regs.frame.set(Frame { t_audio: 100, columns: Vec::new() });
    regs.frame.set(Frame { t_audio: 200, columns: Vec::new() });
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert!(frame_seqs(&good.take()).contains(&2), "healthy window must keep receiving frames");

    let gone = seen.of("shell.sink.gone");
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].text("window"), Some("bad"));
    assert_eq!(gone[0].text("why"), Some("panic"));

    let gone_count =
        stats.tally().counts.into_iter().find(|c| c.name == "shell.sinks.gone").map(|c| c.n);
    assert_eq!(gone_count, Some(1));
    // Pump itself did not crash or transition state after startup.
    assert_eq!(
        seen.of("part.state").len(),
        states_after_open,
        "pump must not transition when a sink panics: {:?}",
        seen.of("part.state")
    );
    assert_eq!(seen.refused(), 0);

    shell.cancel();
    shell.wait().await;
}

// ---- 9. Supervised pump restarts without losing subscriptions ----

#[tokio::test(start_paused = true)]
async fn a_pump_that_dies_is_started_again_and_no_window_has_to_join_again() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let shell = Tree::root("shell", stats.clone());
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    let panic_once = Arc::new(AtomicBool::new(true));
    let panic_flag = panic_once.clone();
    let _kept = pump::start(
        &shell,
        outlet,
        joins_rx,
        |_| {},
        move |_show| {
            if panic_flag.swap(false, Ordering::SeqCst) {
                panic!("test injected pump panic in captions callback");
            }
            CaptionWindow::Ok
        },
    );

    let main = RecordingSink::new();
    joins_tx.send(("main".into(), main.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Trigger the one-shot panic inside the inner pump loop via State.captions = true.
    regs.state.set(State { captions: true, ..State::default() });
    // Advance past Backoff::FIRST_MS so supervise restarts shell.pump.
    tokio::time::sleep(Duration::from_millis(Backoff::FIRST_MS + 100)).await;

    // Without re-subscribing, the existing main sink receives subsequent frames.
    regs.frame.set(Frame { t_audio: 500, columns: Vec::new() });
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert!(
        frame_seqs(&main.take()).contains(&1),
        "subscribed window must keep receiving frames after pump restart: {:?}",
        main.take()
    );

    let waiting: Vec<_> = seen
        .of("part.state")
        .into_iter()
        .filter(|l| {
            l.text("part") == Some("shell.pump")
                && l.text("to") == Some("Waiting")
                && l.text("why") == Some("panic")
        })
        .collect();
    assert_eq!(waiting.len(), 1, "expected one Waiting/panic transition for shell.pump");
    assert_eq!(seen.refused(), 0);

    shell.cancel();
    shell.wait().await;
}

// ---- 10. Progress replays to late joiners; invalidations broadcast without replay ----

#[tokio::test(start_paused = true)]
async fn progress_signal_is_replayed_to_late_joiners_and_invalidations_are_not() {
    let stats = Stats::new();
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    let task = tokio::spawn(pump::run(outlet, joins_rx, stats, |_| {}, |_| CaptionWindow::Ok));
    tokio::time::sleep(Duration::from_millis(10)).await;

    let early = RecordingSink::new();
    joins_tx.send(("early".into(), early.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let prog = Progress {
        refine: Some(Writing { id: "rec-1".into(), step: "translating".into(), done: 2, of: 5 }),
        file: None,
        move_old: None,
        title: None,
    };
    regs.progress.set(prog.clone());
    regs.invalidate(ViewKind::Recording, Some("rec-1".into()));
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Early subscriber saw both Signal::Progress and Event::Invalidate.
    let early_ev = early.take();
    assert!(early_ev.iter().any(|e| matches!(
        e,
        Event::Signal(Stamped { seq: 1, signal: Signal::Progress(p) }) if **p == prog
    )));
    assert!(early_ev.iter().any(|e| matches!(
        e,
        Event::Invalidate(Invalidate { view: ViewKind::Recording, key: Some(k), version: 1 })
            if k == "rec-1"
    )));

    // Late joiner gets Signal::Progress replayed, never past Event::Invalidate.
    let late = RecordingSink::new();
    joins_tx.send(("late".into(), late.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let late_ev = late.take();
    assert!(
        late_ev.iter().any(|e| matches!(
            e,
            Event::Signal(Stamped { seq: 1, signal: Signal::Progress(p) }) if **p == prog
        )),
        "late joiner must receive replayed Progress: {late_ev:?}"
    );
    assert!(
        !late_ev.iter().any(|e| matches!(e, Event::Invalidate(_))),
        "late joiner must not receive replayed Invalidations: {late_ev:?}"
    );

    drop(joins_tx);
    task.await.unwrap();
}

// ---- 11. Invalidation ring overflow falls back to coarse ViewKind::ALL invalidations ----

#[tokio::test(start_paused = true)]
async fn invalidation_overflow_falls_back_to_coarse_view_invalidations() {
    let stats = Stats::new();
    let (regs, outlet) = Registers::new();
    let (joins_tx, joins_rx) = queue::bounded::<(String, RecordingSink)>(
        &stats,
        "shell.joins",
        SHELL_JOINS_MSGS,
        Full::Wait,
    );

    let task = tokio::spawn(pump::run(outlet, joins_rx, stats, |_| {}, |_| CaptionWindow::Ok));
    let sink = RecordingSink::new();
    joins_tx.send(("main".into(), sink.clone())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Synchronously burst more distinct keys than KEEP_INVALIDATIONS_EVENTS (64) before yielding.
    let burst = KEEP_INVALIDATIONS_EVENTS + 12;
    for i in 0..burst {
        regs.invalidate(ViewKind::Recording, Some(format!("rec-{i}")));
    }
    tokio::time::sleep(Duration::from_millis(10)).await;

    let invs: Vec<Invalidate> = sink
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Event::Invalidate(inv) => Some(inv),
            _ => None,
        })
        .collect();
    assert_eq!(invs.len(), ViewKind::ALL.len(), "expected one coarse invalidation per ViewKind");
    for (got, expected_view) in invs.iter().zip(ViewKind::ALL) {
        assert_eq!(got.view, expected_view);
        assert_eq!(got.key, None);
        assert_eq!(got.version, burst as u64);
    }

    drop(joins_tx);
    task.await.unwrap();
}
