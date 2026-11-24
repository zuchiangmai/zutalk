use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use zutalk_core::rt::keep::{self, KEEP_BATCH_ITEMS};
use zutalk_core::rt::queue::{self, Back, Full};
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::{End, Restarts, Tree};
use zutalk_log::Level;

const GATE_TIMEOUT_MS: u64 = 10_000;

struct Gate {
    lock: Mutex<(bool, usize)>,
    cv: Condvar,
}

impl Gate {
    fn closed() -> Arc<Self> {
        Arc::new(Self { lock: Mutex::new((true, 0)), cv: Condvar::new() })
    }

    fn wait(&self) {
        let mut guard = self.lock.lock().expect("gate lock");
        if guard.0 {
            guard.1 += 1;
            let (mut guard, timed_out) = self
                .cv
                .wait_timeout_while(guard, Duration::from_millis(GATE_TIMEOUT_MS), |s| s.0)
                .expect("gate cv");
            guard.1 = guard.1.saturating_sub(1);
            assert!(!timed_out.timed_out() || !guard.0, "gate stayed closed past timeout");
        }
    }

    fn has_waiter(&self) -> bool {
        self.lock.lock().expect("gate lock").1 > 0
    }

    fn open(&self) {
        let mut guard = self.lock.lock().expect("gate lock");
        guard.0 = false;
        self.cv.notify_all();
    }
}

#[tokio::test(start_paused = true)]
async fn what_is_queued_is_written_in_order_and_off_the_caller() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let tree = Tree::root("test", stats.clone());
    const CAP: usize = 1_024;
    let (tx, rx) = queue::bounded::<u32>(tree.stats(), "items", CAP, Full::Drop);

    let written = Arc::new(Mutex::new(Vec::new()));
    let written_cb = written.clone();
    let kept = keep::start(
        &tree,
        "writer",
        "writer.batch",
        rx,
        tree.stats().spread("writer.batch_ms"),
        Restarts::default(),
        || Ok(()),
        move |(), batch: &[u32]| {
            written_cb.lock().expect("written").extend_from_slice(batch);
            Ok(())
        },
        |()| Ok(()),
    );

    // Enqueue 1,000 items synchronously without ever waiting.
    for i in 0..1_000u32 {
        assert_eq!(tx.offer(i), Ok(()), "caller must never wait on offer {i}");
    }
    drop(tx);

    assert_eq!(kept.await, Some(End::Done));
    tree.wait().await;

    let got = written.lock().expect("written").clone();
    assert_eq!(got, (0..1_000u32).collect::<Vec<_>>());

    let tally = stats.tally();
    let q = tally.queues.iter().find(|r| r.name == "items").expect("items queue row");
    assert_eq!(q.sent, 1_000);
    assert_eq!(q.full, 0);

    let spread = tally.spreads.iter().find(|r| r.name == "writer.batch_ms").expect("batch spread");
    assert!(
        spread.n >= (1_000 / KEEP_BATCH_ITEMS) as u64,
        "expected at least {} batches, got {}",
        1_000 / KEEP_BATCH_ITEMS,
        spread.n
    );
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_says_so_at_once() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let tree = Tree::root("test", stats.clone());
    const CAP: usize = 16;
    let (tx, rx) = queue::bounded::<u32>(tree.stats(), "items", CAP, Full::End);

    let gate = Gate::closed();
    let gate_cb = gate.clone();
    let kept = keep::start(
        &tree,
        "writer",
        "writer.batch",
        rx,
        tree.stats().spread("writer.batch_ms"),
        Restarts::default(),
        || Ok(()),
        move |(), _batch: &[u32]| {
            gate_cb.wait();
            Ok(())
        },
        |()| Ok(()),
    );

    // Send 1 item so the writer enters its first blocking batch and parks on the gate.
    assert_eq!(tx.offer(0), Ok(()));
    let mut spins = 0;
    while !gate.has_waiter() {
        spins += 1;
        assert!(spins < 200, "writer never entered blocking gate");
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(5));
    }

    // Fill all CAP slots in the queue.
    for i in 1..=(CAP as u32) {
        assert_eq!(tx.offer(i), Ok(()), "slot {i} of {CAP} must accept");
    }

    // Next offer is rejected immediately without waiting.
    assert_eq!(tx.offer(999), Err(Back::Full(999)));

    let full_events = seen.of("queue.full");
    assert_eq!(full_events.len(), 1, "{full_events:?}");
    assert_eq!(full_events[0].text("queue"), Some("items"));
    assert_eq!(full_events[0].text("policy"), Some("end"));

    gate.open();
    drop(tx);
    assert_eq!(kept.await, Some(End::Done));
    tree.wait().await;
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_writer_that_breaks_is_opened_again() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let tree = Tree::root("test", stats.clone());
    const CAP: usize = 64;
    let (tx, rx) = queue::bounded::<u32>(tree.stats(), "items", CAP, Full::Drop);

    let opens = Arc::new(AtomicUsize::new(0));
    let batch_no = Arc::new(AtomicUsize::new(0));
    let written = Arc::new(Mutex::new(Vec::<Vec<u32>>::new()));
    let done_one = Arc::new(Notify::new());

    let opens_cb = opens.clone();
    let batch_no_cb = batch_no.clone();
    let written_cb = written.clone();
    let done_one_cb = done_one.clone();

    let kept = keep::start(
        &tree,
        "writer",
        "writer.batch",
        rx,
        tree.stats().spread("writer.batch_ms"),
        Restarts::default(),
        move || {
            opens_cb.fetch_add(1, Ordering::AcqRel);
            Ok(())
        },
        move |(), batch: &[u32]| {
            let n = batch_no_cb.fetch_add(1, Ordering::AcqRel) + 1;
            match n {
                3 => {
                    done_one_cb.notify_one();
                    Err("disk")
                }
                5 => {
                    done_one_cb.notify_one();
                    panic!("injected write panic on batch 5");
                }
                _ => {
                    written_cb.lock().expect("written").push(batch.to_vec());
                    done_one_cb.notify_one();
                    Ok(())
                }
            }
        },
        |()| Ok(()),
    );

    // Send 6 distinct batches sequentially so each batch size is deterministic.
    let batches: [&[u32]; 6] = [
        &[1, 2],
        &[3, 4, 5],
        &[6, 7, 8, 9],         // batch 3: returns Err("disk"), 4 items lost
        &[10, 11],             // batch 4: succeeds after reopen #2
        &[12, 13, 14, 15, 16], // batch 5: panics, 5 items lost
        &[17, 18, 19],         // batch 6: succeeds after reopen #3
    ];

    for (idx, items) in batches.iter().enumerate() {
        let notified = done_one.notified();
        for &v in *items {
            assert_eq!(tx.offer(v), Ok(()));
        }
        notified.await;
        // Let supervision sleep through backoff and reopen after broken batches 3 and 5.
        if idx == 2 || idx == 4 {
            tokio::time::sleep(Duration::from_millis(1_000)).await;
        }
    }

    drop(tx);
    assert_eq!(kept.await, Some(End::Done));
    tree.wait().await;

    assert_eq!(opens.load(Ordering::Acquire), 3, "open must be called 3 times");
    assert_eq!(
        *written.lock().expect("written"),
        vec![vec![1, 2], vec![3, 4, 5], vec![10, 11], vec![17, 18, 19]],
        "broken batches 3 and 5 must not be rewritten; subsequent batches must succeed"
    );

    // Observability: 2 keep.lost events (4 items disk, 5 items panic).
    let lost = seen.of("keep.lost");
    assert_eq!(lost.len(), 2, "{lost:?}");
    assert_eq!(lost[0].level, Level::Warn);
    assert_eq!(lost[0].text("part"), Some("writer"));
    assert_eq!(lost[0].u64("n"), Some(4));
    assert_eq!(lost[0].text("why"), Some("disk"));
    assert_eq!(lost[1].level, Level::Warn);
    assert_eq!(lost[1].text("part"), Some("writer"));
    assert_eq!(lost[1].u64("n"), Some(5));
    assert_eq!(lost[1].text("why"), Some("panic"));

    // Observability: 2 part.state transitions to Waiting with matching reasons.
    let waiting: Vec<_> = seen
        .of("part.state")
        .into_iter()
        .filter(|l| l.text("part") == Some("writer") && l.text("to") == Some("Waiting"))
        .collect();
    assert_eq!(waiting.len(), 2, "{waiting:?}");
    assert_eq!(waiting[0].text("why"), Some("disk"));
    assert_eq!(waiting[1].text("why"), Some("panic"));

    // Counters: keep.lost_batches == 2, part.restarts == 2.
    let tally = stats.tally();
    let count = |name: &str| tally.counts.iter().find(|r| r.name == name).map_or(0, |r| r.n);
    assert_eq!(count("keep.lost_batches"), 2);
    assert_eq!(count("part.restarts"), 2);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_writer_that_never_opens_gives_up_and_says_why() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let tree = Tree::root("test", stats.clone());
    const CAP: usize = 16;
    let (_tx, rx) = queue::bounded::<u32>(tree.stats(), "items", CAP, Full::Drop);

    let kept = keep::start(
        &tree,
        "broken",
        "broken.batch",
        rx,
        tree.stats().spread("broken.batch_ms"),
        Restarts::default(),
        || -> Result<(), &'static str> { Err("disk") },
        |(), _batch: &[u32]| Ok(()),
        |()| Ok(()),
    );

    assert_eq!(kept.await, Some(End::Fatal("disk")));
    tree.wait().await;

    let gave_up = seen.of("part.gave_up");
    assert_eq!(gave_up.len(), 1, "{gave_up:?}");
    assert_eq!(gave_up[0].level, Level::Error);
    assert_eq!(gave_up[0].text("part"), Some("broken"));
    assert_eq!(gave_up[0].text("why"), Some("disk"));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn closing_writes_what_is_left() {
    let seen = zutalk_log::capture(Level::Info);
    let stats = Stats::new();
    let tree = Tree::root("test", stats.clone());
    const CAP: usize = 128;
    let (tx, rx) = queue::bounded::<u32>(tree.stats(), "items", CAP, Full::Drop);

    let written = Arc::new(Mutex::new(Vec::new()));
    let finished = Arc::new(AtomicUsize::new(0));
    let written_cb = written.clone();
    let finished_cb = finished.clone();

    let kept = keep::start(
        &tree,
        "writer",
        "writer.batch",
        rx,
        tree.stats().spread("writer.batch_ms"),
        Restarts::default(),
        || Ok(()),
        move |(), batch: &[u32]| {
            written_cb.lock().expect("written").extend_from_slice(batch);
            Ok(())
        },
        move |()| {
            finished_cb.fetch_add(1, Ordering::AcqRel);
            Ok(())
        },
    );

    for i in 0..100u32 {
        assert_eq!(tx.offer(i), Ok(()));
    }
    drop(tx);

    assert_eq!(kept.await, Some(End::Done));
    tree.wait().await;

    assert_eq!(*written.lock().expect("written"), (0..100u32).collect::<Vec<_>>());
    assert_eq!(finished.load(Ordering::Acquire), 1, "finish/sync must be called once on close");
    assert_eq!(seen.refused(), 0);
}
