use std::collections::VecDeque;
use std::future::pending;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::{Instant, advance, sleep};
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::{
    Backoff, Broke, Effect, End, Part, RESTARTS_MOST_TIMES, RESTARTS_PATIENCE_MS,
    RESTARTS_WINDOW_MS, Restarts, Seen, TREE_MOST_TASKS, Tree, Watch, step, supervise,
    thread as named_thread,
};
use zutalk_log::{Level, capture, info};

/// A part that runs for a while and then ends the way the script says.
struct Scripted {
    runs_for: Duration,
    end: End,
}

impl Part for Scripted {
    async fn run(&mut self) -> End {
        sleep(self.runs_for).await;
        self.end
    }
}

fn quick(end: End) -> Result<Scripted, End> {
    Ok(Scripted { runs_for: Duration::ZERO, end })
}

/// Runs a script under `supervise`. Returns how it ended, when each open happened
/// (ms since the start), and the reasons that were reported.
async fn play(script: Vec<Result<Scripted, End>>) -> (End, Vec<u64>, Vec<&'static str>) {
    let (mut script, mut opens, mut reasons) = (VecDeque::from(script), Vec::new(), Vec::new());
    let start = Instant::now();
    let end = supervise(
        "test",
        async || {
            opens.push(start.elapsed().as_millis() as u64);
            script.pop_front().expect("the script ran out")
        },
        |why| reasons.push(why),
        Restarts::default(),
    )
    .await;
    (end, opens, reasons)
}

#[tokio::test(start_paused = true)]
async fn it_reopens_until_asked_to_stop() {
    let script =
        vec![quick(End::Retry("unplugged")), quick(End::Retry("format")), quick(End::Done)];
    let (end, opens, reasons) = play(script).await;
    assert_eq!(end, End::Done);
    assert_eq!(opens, [0, 250, 750]);
    assert_eq!(reasons, ["unplugged", "format"]);
}

#[tokio::test(start_paused = true)]
async fn failing_to_open_is_retried_like_any_other_break() {
    let script = vec![Err(End::Retry("busy")), Err(End::Retry("busy")), quick(End::Done)];
    let (end, opens, reasons) = play(script).await;
    assert_eq!(end, End::Done);
    assert_eq!(opens.len(), 3);
    assert_eq!(reasons, ["busy", "busy"]);
}

#[tokio::test(start_paused = true)]
async fn what_cannot_be_fixed_by_reopening_stops_at_once() {
    let (end, opens, reasons) = play(vec![Err(End::Fatal("no permission"))]).await;
    assert_eq!(end, End::Fatal("no permission"));
    assert_eq!(opens.len(), 1);
    assert!(reasons.is_empty());
}

#[tokio::test(start_paused = true)]
async fn waits_double_up_to_a_ceiling() {
    let mut script: Vec<_> = (0..7).map(|_| quick(End::Retry("down"))).collect();
    script.push(quick(End::Done));
    let (_, opens, _) = play(script).await;
    let waits: Vec<u64> = opens.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(waits, [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000]);
}

#[tokio::test(start_paused = true)]
async fn a_healthy_run_starts_the_waits_over() {
    let long = Ok(Scripted { runs_for: Duration::from_secs(60), end: End::Retry("dropped") });
    let script = vec![quick(End::Retry("down")), quick(End::Retry("down")), long, quick(End::Done)];
    let (_, opens, _) = play(script).await;
    // 250 and 500 after the two quick failures; then a minute of running; then 250 again.
    assert_eq!(opens, [0, 250, 750, 60_750 + 250]);
}

#[tokio::test(start_paused = true)]
async fn a_tree_cancels_and_waits_for_all_it_started() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    let child = root.child("recording");

    root.spawn("hub", pending());
    root.spawn("stats", pending());
    child.spawn("lane", pending());

    advance(Duration::from_millis(50)).await;
    let running = root.running();
    assert_eq!(running.len(), 3);
    assert_eq!(
        running.iter().map(|r| (r.tree, r.task, r.kind, r.for_ms)).collect::<Vec<_>>(),
        vec![
            ("core", "hub", "task", 50),
            ("core", "stats", "task", 50),
            ("recording", "lane", "task", 50),
        ]
    );

    root.cancel();
    root.wait().await;

    assert!(root.running().is_empty());
    let starts = seen.of("task.start");
    let ends = seen.of("task.end");
    assert_eq!(starts.len(), 3);
    assert_eq!(ends.len(), 3);
    for line in &ends {
        assert_eq!(line.level, Level::Info);
        assert_eq!(line.text("code"), Some("cancelled"));
        assert_eq!(line.u64("ran_ms"), Some(50));
    }
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_task_that_ends_on_its_own_says_done() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    root.spawn("short", async {
        sleep(Duration::from_millis(120)).await;
    });

    root.wait().await;

    assert!(root.running().is_empty());
    assert_eq!(seen.count("task.start"), 1);
    let ends = seen.of("task.end");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].level, Level::Info);
    assert_eq!(ends[0].text("tree"), Some("core"));
    assert_eq!(ends[0].text("task"), Some("short"));
    assert_eq!(ends[0].text("code"), Some("done"));
    assert_eq!(ends[0].u64("ran_ms"), Some(120));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_task_that_panics_is_seen_and_takes_nothing_with_it() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let root = Tree::root("core", stats.clone());
    let steady_done = Arc::new(AtomicBool::new(false));
    let flag = steady_done.clone();

    root.spawn("broken", async {
        panic!("boom");
    });
    root.spawn("steady", async move {
        sleep(Duration::from_millis(200)).await;
        flag.store(true, Ordering::SeqCst);
    });

    root.wait().await;

    assert!(steady_done.load(Ordering::SeqCst));
    let ends = seen.of("task.end");
    assert_eq!(ends.len(), 2);
    let broken = ends.iter().find(|l| l.text("task") == Some("broken")).expect("broken ended");
    assert_eq!(broken.level, Level::Error);
    assert_eq!(broken.text("code"), Some("panic"));
    let steady = ends.iter().find(|l| l.text("task") == Some("steady")).expect("steady ended");
    assert_eq!(steady.level, Level::Info);
    assert_eq!(steady.text("code"), Some("done"));

    let tally = stats.tally();
    let panics = tally.counts.iter().find(|c| c.name == "task.panics").map(|c| c.n);
    assert_eq!(panics, Some(1));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_child_can_be_cancelled_alone() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    let child = root.child("recording");

    root.spawn("parent", pending());
    child.spawn("kid", pending());

    child.cancel();
    child.wait().await;

    let running = root.running();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].tree, "core");
    assert_eq!(running[0].task, "parent");

    let ends = seen.of("task.end");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].text("tree"), Some("recording"));
    assert_eq!(ends[0].text("task"), Some("kid"));
    assert_eq!(ends[0].text("code"), Some("cancelled"));

    root.cancel();
    root.wait().await;
    assert!(root.running().is_empty());
    assert_eq!(seen.count("task.end"), 2);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn nothing_starts_on_a_tree_that_has_closed() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    root.wait().await;

    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    root.spawn("late", async move {
        flag.store(true, Ordering::SeqCst);
    });
    tokio::task::yield_now().await;

    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(seen.count("task.start"), 0);
    let refused = seen.of("task.refused");
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].level, Level::Warn);
    assert_eq!(refused[0].text("tree"), Some("core"));
    assert_eq!(refused[0].text("task"), Some("late"));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_blocking_call_gives_back_what_it_made_and_a_panic_as_broke() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);

    let (result, observed) = tokio::join!(
        root.blocking("disk", move || {
            entered_tx.send(()).expect("notify test");
            release_rx.recv().expect("released");
            42u32
        }),
        async {
            entered_rx.await.expect("entered blocking");
            let running = root.running();
            release_tx.send(()).expect("release blocking");
            running
        }
    );

    assert_eq!(result, Ok(42));
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].tree, "core");
    assert_eq!(observed[0].task, "disk");
    assert_eq!(observed[0].kind, "blocking");

    let broke: Result<u32, Broke> = root.blocking("boom", || panic!("bad")).await;
    assert_eq!(broke, Err(Broke));
    assert!(root.running().is_empty());
    assert_eq!(seen.count("task.start"), 0);
    assert_eq!(seen.count("task.end"), 0);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_task_says_is_heard_by_whoever_started_the_tree() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    let work_tree = root.clone();

    root.spawn("worker", async move {
        info!("test.spawn", n = 1u64);
        work_tree
            .blocking("work", || {
                info!("test.blocking", n = 2u64);
            })
            .await
            .expect("blocking succeeded");
        named_thread("bg.thread", || {
            info!("test.thread", n = 3u64);
        })
        .join()
        .expect("thread joined");
    });

    root.wait().await;

    assert_eq!(seen.of("test.spawn").len(), 1);
    assert_eq!(seen.of("test.spawn")[0].u64("n"), Some(1));
    assert_eq!(seen.of("test.blocking").len(), 1);
    assert_eq!(seen.of("test.blocking")[0].u64("n"), Some(2));
    assert_eq!(seen.of("test.thread").len(), 1);
    assert_eq!(seen.of("test.thread")[0].u64("n"), Some(3));
    assert_eq!(seen.refused(), 0);
}

#[test]
fn a_named_thread_says_when_it_starts_and_ends() {
    let seen = capture(Level::Debug);
    let name_seen = Arc::new(Mutex::new(None));
    let out = name_seen.clone();

    named_thread("capture.pump", move || {
        *out.lock().expect("lock") = std::thread::current().name().map(str::to_owned);
    })
    .join()
    .expect("thread joined");
    assert_eq!(name_seen.lock().expect("lock").as_deref(), Some("capture.pump"));

    let _ = named_thread("broken.thread", || {
        panic!("thread boom");
    })
    .join();

    let starts = seen.of("thread.start");
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].level, Level::Info);
    assert_eq!(starts[0].text("thread"), Some("capture.pump"));
    assert_eq!(starts[1].level, Level::Info);
    assert_eq!(starts[1].text("thread"), Some("broken.thread"));

    let ends = seen.of("thread.end");
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0].level, Level::Info);
    assert_eq!(ends[0].text("thread"), Some("capture.pump"));
    assert_eq!(ends[0].text("code"), Some("done"));
    assert!(ends[0].u64("ran_ms").is_some());

    assert_eq!(ends[1].level, Level::Error);
    assert_eq!(ends[1].text("thread"), Some("broken.thread"));
    assert_eq!(ends[1].text("code"), Some("panic"));
    assert!(ends[1].u64("ran_ms").is_some());
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn more_tasks_than_the_table_holds_still_run() {
    let seen = capture(Level::Debug);
    let root = Tree::root("core", Stats::new());
    let ran = Arc::new(AtomicUsize::new(0));
    let total = TREE_MOST_TASKS + 2;

    for _ in 0..total {
        let count = ran.clone();
        root.spawn("extra", async move {
            count.fetch_add(1, Ordering::SeqCst);
            pending::<()>().await;
        });
    }
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    assert_eq!(ran.load(Ordering::SeqCst), total);
    assert_eq!(root.running().len(), TREE_MOST_TASKS);
    let full = seen.of("stats.full");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].level, Level::Warn);
    assert_eq!(full[0].text("kind"), Some("tasks"));

    root.cancel();
    root.wait().await;
    assert!(root.running().is_empty());
    assert_eq!(seen.refused(), 0);
}

#[test]
fn the_watch_steps_as_its_table_says() {
    for state in [
        Watch::Opening { n: 1 },
        Watch::Running { n: 1, since_ms: 0 },
        Watch::Waiting { n: 1, until_ms: 250, why: "down" },
        Watch::Ended(End::Done),
    ] {
        assert!(zutalk_log::is_plain(state.code()));
    }

    // Opening -> Opened -> Running + [Run].
    let (mut backoff, mut restarts) = (Backoff::default(), Restarts::default());
    assert_eq!(
        step(Watch::Opening { n: 1 }, Seen::Opened, 100, &mut backoff, &mut restarts),
        (Watch::Running { n: 1, since_ms: 100 }, vec![Effect::Run])
    );

    // Opening -> Stopped(Done / Fatal) -> Ended + [].
    assert_eq!(
        step(Watch::Opening { n: 1 }, Seen::Stopped(End::Done), 100, &mut backoff, &mut restarts),
        (Watch::Ended(End::Done), vec![])
    );
    assert_eq!(
        step(
            Watch::Opening { n: 1 },
            Seen::Stopped(End::Fatal("key")),
            100,
            &mut backoff,
            &mut restarts
        ),
        (Watch::Ended(End::Fatal("key")), vec![])
    );

    // Opening -> Stopped(Retry(why)) -> Waiting + [Broke(why), Wait(250)].
    let (mut backoff, mut restarts) = (Backoff::default(), Restarts::default());
    assert_eq!(
        step(
            Watch::Opening { n: 1 },
            Seen::Stopped(End::Retry("connect")),
            100,
            &mut backoff,
            &mut restarts
        ),
        (
            Watch::Waiting { n: 1, until_ms: 350, why: "connect" },
            vec![Effect::Broke("connect"), Effect::Wait(250)]
        )
    );

    // Running -> Stopped(Done) -> Ended(Done) + [].
    assert_eq!(
        step(
            Watch::Running { n: 1, since_ms: 100 },
            Seen::Stopped(End::Done),
            500,
            &mut backoff,
            &mut restarts
        ),
        (Watch::Ended(End::Done), vec![])
    );

    // Running -> Stopped(Fatal(why)) -> Ended(Fatal(why)) + [].
    assert_eq!(
        step(
            Watch::Running { n: 1, since_ms: 100 },
            Seen::Stopped(End::Fatal("quota")),
            500,
            &mut backoff,
            &mut restarts
        ),
        (Watch::Ended(End::Fatal("quota")), vec![])
    );

    // Running -> Stopped(Retry(why)) below cap -> Waiting + [Broke(why), Wait(500)].
    assert_eq!(
        step(
            Watch::Running { n: 2, since_ms: 350 },
            Seen::Stopped(End::Retry("socket")),
            600,
            &mut backoff,
            &mut restarts
        ),
        (
            Watch::Waiting { n: 2, until_ms: 1_100, why: "socket" },
            vec![Effect::Broke("socket"), Effect::Wait(500)]
        )
    );

    // Stopped(Retry(why)) reaching cap -> Ended(Fatal(why)) + [Broke(why), GaveUp(why)].
    let mut capped = Restarts::new(2, RESTARTS_WINDOW_MS, RESTARTS_PATIENCE_MS);
    let mut b = Backoff::default();
    let _ = step(
        Watch::Running { n: 1, since_ms: 0 },
        Seen::Stopped(End::Retry("socket")),
        100,
        &mut b,
        &mut capped,
    );
    assert_eq!(
        step(
            Watch::Running { n: 2, since_ms: 350 },
            Seen::Stopped(End::Retry("socket")),
            450,
            &mut b,
            &mut capped
        ),
        (
            Watch::Ended(End::Fatal("socket")),
            vec![Effect::Broke("socket"), Effect::GaveUp("socket")]
        )
    );

    // Waiting -> Waited -> Opening { n + 1 } + [Open].
    assert_eq!(
        step(
            Watch::Waiting { n: 2, until_ms: 1_100, why: "socket" },
            Seen::Waited,
            1_100,
            &mut backoff,
            &mut restarts
        ),
        (Watch::Opening { n: 3 }, vec![Effect::Open])
    );

    // Combinations not in the transition table stay unchanged with no effects.
    let unchanged = [
        (Watch::Opening { n: 1 }, Seen::Waited),
        (Watch::Running { n: 1, since_ms: 0 }, Seen::Opened),
        (Watch::Running { n: 1, since_ms: 0 }, Seen::Waited),
        (Watch::Waiting { n: 1, until_ms: 250, why: "down" }, Seen::Opened),
        (Watch::Waiting { n: 1, until_ms: 250, why: "down" }, Seen::Stopped(End::Done)),
        (Watch::Ended(End::Done), Seen::Opened),
        (Watch::Ended(End::Done), Seen::Waited),
        (Watch::Ended(End::Done), Seen::Stopped(End::Retry("down"))),
    ];
    for (state, ev) in unchanged {
        let (mut b, mut r) = (Backoff::default(), Restarts::default());
        assert_eq!(step(state, ev, 1_000, &mut b, &mut r), (state, vec![]));
    }
}

#[tokio::test(start_paused = true)]
async fn eight_quick_failures_in_a_minute_are_the_last() {
    let seen = capture(Level::Debug);
    let script: Vec<_> = (0..10).map(|_| quick(End::Retry("socket"))).collect();
    let (end, opens, reasons) = play(script).await;

    assert_eq!(end, End::Fatal("socket"));
    assert_eq!(opens.len(), RESTARTS_MOST_TIMES);
    assert_eq!(reasons.len(), RESTARTS_MOST_TIMES);

    let states = seen.of("part.state");
    let to_waiting = states.iter().filter(|l| l.text("to") == Some("Waiting")).count();
    let to_ended = states.iter().filter(|l| l.text("to") == Some("Ended")).count();
    assert_eq!(to_waiting, 7);
    assert_eq!(to_ended, 1);
    for w in states.iter().filter(|l| l.text("to") == Some("Waiting")) {
        assert_eq!(w.level, Level::Warn);
    }

    let gave_up = seen.of("part.gave_up");
    assert_eq!(gave_up.len(), 1);
    assert_eq!(gave_up[0].level, Level::Error);
    assert_eq!(gave_up[0].text("part"), Some("test"));
    assert_eq!(gave_up[0].text("why"), Some("socket"));
    assert_eq!(gave_up[0].u64("n"), Some(8));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn failures_a_minute_apart_never_reach_the_cap() {
    let seen = capture(Level::Debug);
    let mut script: Vec<_> = (0..15)
        .map(|_| Ok(Scripted { runs_for: Duration::from_millis(60_000), end: End::Retry("blip") }))
        .collect();
    script.push(quick(End::Done));

    let (end, opens, _) = play(script).await;
    assert_eq!(end, End::Done);
    assert_eq!(opens.len(), 16);
    assert_eq!(seen.count("part.gave_up"), 0);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_healthy_run_clears_the_count() {
    let seen = capture(Level::Debug);
    let mut script: Vec<_> = (0..7).map(|_| quick(End::Retry("down"))).collect();
    script.push(Ok(Scripted {
        runs_for: Duration::from_millis(Backoff::HEALTHY_MS),
        end: End::Retry("dropped"),
    }));
    script.extend((0..7).map(|_| quick(End::Retry("down"))));
    script.push(quick(End::Done));

    let (end, opens, _) = play(script).await;
    assert_eq!(end, End::Done);
    assert_eq!(opens.len(), 16);
    assert_eq!(seen.count("part.gave_up"), 0);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn two_minutes_without_a_healthy_run_are_the_last() {
    let seen = capture(Level::Debug);
    let mut opens = 0usize;
    let start = Instant::now();
    let end = supervise(
        "dialer",
        async || -> Result<Scripted, End> {
            opens += 1;
            sleep(Duration::from_millis(10_000)).await;
            Err(End::Retry("connect"))
        },
        |_| {},
        Restarts::default(),
    )
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    assert_eq!(end, End::Fatal("connect"));
    assert!(elapsed_ms >= RESTARTS_PATIENCE_MS, "ended at {elapsed_ms} ms < 120_000 ms");
    assert!(opens > 8 && opens <= 12, "expected >8 and <=12 opens, got {opens}");

    let gave_up = seen.of("part.gave_up");
    assert_eq!(gave_up.len(), 1);
    assert_eq!(gave_up[0].level, Level::Error);
    assert_eq!(gave_up[0].text("part"), Some("dialer"));
    assert_eq!(gave_up[0].text("why"), Some("connect"));
    assert_eq!(seen.refused(), 0);
}

struct MaybePanics {
    panic_on_run: bool,
}

impl Part for MaybePanics {
    async fn run(&mut self) -> End {
        if self.panic_on_run {
            panic!("part blew up in run");
        }
        End::Done
    }
}

#[tokio::test(start_paused = true)]
async fn a_part_that_panics_is_opened_again() {
    let seen = capture(Level::Debug);
    let start = Instant::now();
    let mut opens = Vec::new();
    let mut reasons = Vec::new();
    let mut attempt = 0usize;

    let end = supervise(
        "frame",
        async || {
            opens.push(start.elapsed().as_millis() as u64);
            attempt += 1;
            Ok(MaybePanics { panic_on_run: attempt == 1 })
        },
        |why| reasons.push(why),
        Restarts::default(),
    )
    .await;

    assert_eq!(end, End::Done);
    assert_eq!(opens, [0, Backoff::FIRST_MS]);
    assert_eq!(reasons, ["panic"]);

    let waiting: Vec<_> =
        seen.of("part.state").into_iter().filter(|l| l.text("to") == Some("Waiting")).collect();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].level, Level::Warn);
    assert_eq!(waiting[0].text("part"), Some("frame"));
    assert_eq!(waiting[0].text("from"), Some("Running"));
    assert_eq!(waiting[0].text("why"), Some("panic"));
    assert_eq!(waiting[0].u64("wait_ms"), Some(250));
    assert_eq!(waiting[0].u64("n"), Some(1));

    // A panic inside `open` itself is also caught as `End::Retry("panic")` and reopened.
    let mut open_attempt = 0usize;
    let mut open_reasons = Vec::new();
    let end_open = supervise(
        "opener",
        async || {
            open_attempt += 1;
            if open_attempt == 1 {
                panic!("blew up in open");
            }
            Ok(MaybePanics { panic_on_run: false })
        },
        |why| open_reasons.push(why),
        Restarts::default(),
    )
    .await;
    assert_eq!(end_open, End::Done);
    assert_eq!(open_reasons, ["panic"]);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_kept_part_is_a_task_of_the_tree_and_its_restarts_are_counted() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let root = Tree::root("core", stats.clone());

    let mut script = VecDeque::from(vec![
        quick(End::Retry("socket")),
        quick(End::Retry("closed")),
        quick(End::Done),
    ]);
    let kept = root.keep(
        "tape",
        move || {
            let next = script.pop_front().expect("script item");
            async move { next }
        },
        |_| {},
        Restarts::default(),
    );

    let end = kept.await;
    assert_eq!(end, Some(End::Done));
    root.wait().await;

    let starts = seen.of("task.start");
    let ends = seen.of("task.end");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].text("tree"), Some("core"));
    assert_eq!(starts[0].text("task"), Some("tape"));
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].text("tree"), Some("core"));
    assert_eq!(ends[0].text("task"), Some("tape"));
    assert_eq!(ends[0].text("code"), Some("done"));

    let tally = stats.tally();
    let restarts = tally.counts.iter().find(|c| c.name == "part.restarts").map(|c| c.n);
    assert_eq!(restarts, Some(2));

    // Cancelling a tree while a kept part is waiting to reopen prevents it from opening again.
    let child = Tree::root("recording", Stats::new());
    let opens = Arc::new(AtomicUsize::new(0));
    let opens_in = opens.clone();
    let cancelled_kept = child.keep(
        "lane.0",
        move || {
            let n = opens_in.fetch_add(1, Ordering::SeqCst);
            async move { if n == 0 { Err(End::Retry("connect")) } else { quick(End::Done) } }
        },
        |_| {},
        Restarts::default(),
    );

    advance(Duration::from_millis(100)).await;
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(child.running()[0].kind, "kept");

    child.cancel();
    assert_eq!(cancelled_kept.await, None);
    child.wait().await;
    advance(Duration::from_millis(5_000)).await;
    assert_eq!(opens.load(Ordering::SeqCst), 1, "must not reopen after tree cancellation");

    // Reaching the restart cap on a kept part increments `part.gave_up` and yields `End::Fatal`.
    let cap_stats = Stats::new();
    let cap_tree = Tree::root("capped", cap_stats.clone());
    let doomed = cap_tree.keep(
        "doomed",
        || async { Err::<Scripted, End>(End::Retry("connect")) },
        |_| {},
        Restarts::new(2, RESTARTS_WINDOW_MS, RESTARTS_PATIENCE_MS),
    );
    assert_eq!(doomed.await, Some(End::Fatal("connect")));
    cap_tree.wait().await;
    let cap_tally = cap_stats.tally();
    assert_eq!(cap_tally.counts.iter().find(|c| c.name == "part.restarts").map(|c| c.n), Some(1));
    assert_eq!(cap_tally.counts.iter().find(|c| c.name == "part.gave_up").map(|c| c.n), Some(1));
    assert_eq!(seen.refused(), 0);
}
