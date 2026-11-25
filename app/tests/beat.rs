//! Table and paused-clock tests for the non-blocking main-thread heartbeat (`P1.md` §3.13.7,
//! §3.13.11, `P1-24`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zutalk::beat::{BEAT_EVERY_MS, BEAT_STALLED_MS, Beat, Effect, Tick, run, step};
use zutalk_core::rt::stats::Stats;
use zutalk_log::Level;

// ---- 1. Pure state-machine transition table ----

#[test]
fn the_beat_steps_as_its_table_says() {
    let cases = [
        // Idle + Time -> Out, Post(seq)
        (Beat::Idle, Tick::Time, 0, Beat::Out { seq: 1, sent_ms: 0 }, vec![Effect::Post(1)]),
        (
            Beat::Idle,
            Tick::Time,
            5_000,
            Beat::Out { seq: 5_001, sent_ms: 5_000 },
            vec![Effect::Post(5_001)],
        ),
        // Idle + Back(_) -> unchanged
        (Beat::Idle, Tick::Back(1), 100, Beat::Idle, vec![]),
        // Out + matching Back -> Idle, Took(latency)
        (
            Beat::Out { seq: 7, sent_ms: 1_000 },
            Tick::Back(7),
            1_042,
            Beat::Idle,
            vec![Effect::Took(42)],
        ),
        // Out + mismatched Back -> unchanged
        (
            Beat::Out { seq: 7, sent_ms: 1_000 },
            Tick::Back(6),
            1_042,
            Beat::Out { seq: 7, sent_ms: 1_000 },
            vec![],
        ),
        // Out + Time at boundary: 1999 ms (< BEAT_STALLED_MS) -> unchanged, no second Post
        (
            Beat::Out { seq: 1, sent_ms: 0 },
            Tick::Time,
            BEAT_STALLED_MS - 1,
            Beat::Out { seq: 1, sent_ms: 0 },
            vec![],
        ),
        // Out + Time at boundary: 2000 ms (== BEAT_STALLED_MS) -> Late, Stalled(2000)
        (
            Beat::Out { seq: 1, sent_ms: 0 },
            Tick::Time,
            BEAT_STALLED_MS,
            Beat::Late { seq: 1, sent_ms: 0 },
            vec![Effect::Stalled(BEAT_STALLED_MS)],
        ),
        // Late + Time -> unchanged (stalled is emitted only once)
        (
            Beat::Late { seq: 1, sent_ms: 0 },
            Tick::Time,
            30_000,
            Beat::Late { seq: 1, sent_ms: 0 },
            vec![],
        ),
        // Late + mismatched Back -> unchanged
        (
            Beat::Late { seq: 1, sent_ms: 0 },
            Tick::Back(99),
            5_000,
            Beat::Late { seq: 1, sent_ms: 0 },
            vec![],
        ),
        // Late + matching Back -> Idle, Back(total stalled_ms)
        (
            Beat::Late { seq: 1, sent_ms: 0 },
            Tick::Back(1),
            32_000,
            Beat::Idle,
            vec![Effect::Back(32_000)],
        ),
    ];

    for (state, tick, now_ms, want_state, want_effects) in cases {
        let (got_state, got_effects) = step(state, tick, now_ms);
        assert_eq!(got_state, want_state, "state={state:?} tick={tick:?} now_ms={now_ms}");
        assert_eq!(got_effects, want_effects, "state={state:?} tick={tick:?} now_ms={now_ms}");
    }
}

// ---- 2. Healthy main thread ----

#[tokio::test(start_paused = true)]
async fn a_main_thread_that_answers_is_timed_and_nothing_is_said() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = calls.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_cb = alive.clone();

    let run_stats = stats.clone();
    let task = tokio::spawn(zutalk_log::carry(run(
        move |work| {
            if !alive_cb.load(Ordering::SeqCst) {
                return false;
            }
            calls_cb.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move { work() });
            true
        },
        run_stats,
    )));

    // Advance 10 virtual seconds (interval ticks at 0, 1, ..., 10 s).
    tokio::time::sleep(Duration::from_millis(BEAT_EVERY_MS * 10 + 50)).await;
    alive.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(BEAT_EVERY_MS)).await;
    task.await.expect("beat task exits cleanly");

    let posted = calls.load(Ordering::SeqCst) as u64;
    assert!((10..=11).contains(&posted), "expected ~10-11 beats in 10 s, got {posted}");

    let beat_spread = stats
        .tally()
        .spreads
        .into_iter()
        .find(|s| s.name == "shell.beat_ms")
        .expect("shell.beat_ms spread recorded");
    assert_eq!(beat_spread.n, posted, "every posted beat was timed");
    assert!(seen.of("shell.main_thread.stalled").is_empty(), "healthy main thread must not stall");
    assert_eq!(seen.refused(), 0);
}

// ---- 3. Stalled main thread reports once, holds single in-flight beat, then reports back ----

#[tokio::test(start_paused = true)]
async fn a_main_thread_that_stands_still_is_said_once_and_its_return_says_how_long() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let calls = Arc::new(AtomicUsize::new(0));
    type Work = Box<dyn FnOnce() + Send>;
    let held: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
    let hold_mode = Arc::new(AtomicBool::new(true));
    let alive = Arc::new(AtomicBool::new(true));

    let calls_cb = calls.clone();
    let held_cb = held.clone();
    let hold_cb = hold_mode.clone();
    let alive_cb = alive.clone();
    let run_stats = stats.clone();

    let task = tokio::spawn(zutalk_log::carry(run(
        move |work| {
            if !alive_cb.load(Ordering::SeqCst) {
                return false;
            }
            calls_cb.fetch_add(1, Ordering::SeqCst);
            if hold_cb.load(Ordering::SeqCst) {
                held_cb.lock().unwrap().push(work);
            } else {
                tokio::spawn(async move { work() });
            }
            true
        },
        run_stats,
    )));

    // First beat posted at t = 0; at t = 2000 ms it becomes stalled.
    tokio::time::sleep(Duration::from_millis(BEAT_STALLED_MS + 100)).await;
    let stalled_events = seen.of("shell.main_thread.stalled");
    assert_eq!(stalled_events.len(), 1, "expected exactly one stalled event");
    assert_eq!(stalled_events[0].u64("waited_ms"), Some(BEAT_STALLED_MS));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "at most one beat may be in flight");

    // Another 30 virtual seconds pass while still stalled: still 1 stalled event and 1 posted beat.
    tokio::time::sleep(Duration::from_millis(30_000)).await;
    assert_eq!(seen.of("shell.main_thread.stalled").len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let stalled_count =
        stats.tally().counts.into_iter().find(|c| c.name == "shell.beats.stalled").map(|c| c.n);
    assert_eq!(stalled_count, Some(1));

    // Release the held closure at t = 32_100 ms and switch back to responsive main thread.
    hold_mode.store(false, Ordering::SeqCst);
    let pending: Vec<_> = held.lock().unwrap().drain(..).collect();
    assert_eq!(pending.len(), 1);
    for work in pending {
        work();
    }
    tokio::time::sleep(Duration::from_millis(10)).await;

    let back_events = seen.of("shell.main_thread.back");
    assert_eq!(back_events.len(), 1, "expected one shell.main_thread.back event");
    assert_eq!(back_events[0].u64("stalled_ms"), Some(32_100));

    // Normal beats resume afterwards.
    tokio::time::sleep(Duration::from_millis(BEAT_EVERY_MS * 3)).await;
    assert!(calls.load(Ordering::SeqCst) >= 3, "beats must resume after main thread returns");

    alive.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(BEAT_EVERY_MS * 2)).await;
    task.await.expect("beat task exits cleanly");
    assert_eq!(seen.refused(), 0);
}

// ---- 4. Stops cleanly when post returns false ----

#[tokio::test(start_paused = true)]
async fn the_beat_ends_when_the_app_is_going() {
    let stats = Stats::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = calls.clone();

    run(
        move |_work| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            false
        },
        stats,
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
