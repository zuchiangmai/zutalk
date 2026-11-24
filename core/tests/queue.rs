//! Named bounded queues: overflow policies, stats rows, and edge-triggered events (`rt/queue.rs`).

use std::time::Duration;
use zutalk_core::rt::queue::{Back, Full, RecvError, bounded, fanout, threads};
use zutalk_core::rt::stats::{QueueRow, Stats};
use zutalk_log::{Level, capture};

fn queue_row(stats: &Stats, name: &str) -> QueueRow {
    stats
        .tally()
        .queues
        .into_iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("no queue row named {name}"))
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_hands_back_says_so_once_and_counts() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let (tx, mut rx) = bounded::<u32>(&stats, "hub.mail", 2, Full::Reject);

    assert_eq!(tx.offer(1), Ok(()));
    assert_eq!(tx.offer(2), Ok(()));
    assert_eq!(tx.offer(3), Err(Back::Full(3)));
    assert_eq!(tx.offer(4), Err(Back::Full(4)));
    assert_eq!(tx.offer(5), Err(Back::Full(5)));

    let full = seen.of("queue.full");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].level, Level::Warn);
    assert_eq!(full[0].text("queue"), Some("hub.mail"));
    assert_eq!(full[0].text("policy"), Some("reject"));
    assert_eq!(full[0].u64("capacity"), Some(2));

    let row = queue_row(&stats, "hub.mail");
    assert_eq!(row.sent, 2);
    assert_eq!(row.full, 3);
    assert_eq!(row.lost, 3);
    assert_eq!(row.deepest, 2);
    assert_eq!(row.depth, 2);

    // Drain one, offer again: succeeds and emits queue.clear with lost = 3.
    assert_eq!(rx.recv().await, Some(1));
    assert_eq!(tx.offer(6), Ok(()));
    let clear = seen.of("queue.clear");
    assert_eq!(clear.len(), 1);
    assert_eq!(clear[0].level, Level::Info);
    assert_eq!(clear[0].text("queue"), Some("hub.mail"));
    assert_eq!(clear[0].u64("lost"), Some(3));

    // Fill again: emits a second queue.full.
    assert_eq!(tx.offer(7), Err(Back::Full(7)));
    assert_eq!(seen.count("queue.full"), 2);
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_sender_that_waits_gets_everything_through_in_order() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let (tx, mut rx) = bounded::<u32>(&stats, "lane.events", 2, Full::Wait);

    let sender = tokio::spawn(async move {
        for i in 0..100 {
            tx.send(i).await.unwrap();
        }
    });

    let mut got = Vec::with_capacity(100);
    while let Some(v) = rx.recv().await {
        got.push(v);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    sender.await.unwrap();

    assert_eq!(got, (0..100).collect::<Vec<_>>());
    assert_eq!(seen.count("queue.full"), 0);
    assert_eq!(seen.count("queue.clear"), 0);
    let row = queue_row(&stats, "lane.events");
    assert_eq!(row.sent, 100);
    assert_eq!(row.lost, 0);
    assert_eq!(row.depth, 0);
    assert!(row.full > 0, "waiting sender should count full waits");
}

#[tokio::test(start_paused = true)]
async fn another_kind_of_sender_has_its_own_answer() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let (reject_tx, _rx) = bounded::<u32>(&stats, "hub.mail", 1, Full::Reject);
    let drop_tx = reject_tx.with(Full::Drop);

    assert_eq!(reject_tx.offer(1), Ok(()));
    assert_eq!(drop_tx.offer(2), Err(Back::Full(2)));

    let full = seen.of("queue.full");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].text("queue"), Some("hub.mail"));
    assert_eq!(full[0].text("policy"), Some("drop"));

    assert_eq!(stats.tally().queues.len(), 1);
    let row = queue_row(&stats, "hub.mail");
    assert_eq!(row.sent, 1);
    assert_eq!(row.full, 1);
    assert_eq!(row.lost, 1);
}

#[tokio::test(start_paused = true)]
async fn nobody_reading_is_not_full() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let (tx, rx) = bounded::<u32>(&stats, "desk", 2, Full::Reject);
    drop(rx);

    assert_eq!(tx.offer(42), Err(Back::Closed(42)));
    assert_eq!(tx.send(99).await, Err(99));
    assert_eq!(seen.count("queue.full"), 0);
    let row = queue_row(&stats, "desk");
    assert_eq!(row.full, 0);
    assert_eq!(row.lost, 0);
}

#[test]
fn a_queue_read_by_a_thread_is_the_same_queue() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let (tx, rx) = threads::<u32>(&stats, "capture.pump", 2, Full::Drop);

    // Empty timeout returns Timeout.
    assert!(rx.recv_timeout(Duration::from_millis(1)).is_err());

    assert_eq!(tx.offer(10), Ok(()));
    assert_eq!(tx.offer(20), Ok(()));
    assert_eq!(tx.offer(30), Err(Back::Full(30)));
    assert_eq!(tx.offer(40), Err(Back::Full(40)));

    let full = seen.of("queue.full");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].text("queue"), Some("capture.pump"));
    assert_eq!(full[0].text("policy"), Some("drop"));
    assert_eq!(full[0].u64("capacity"), Some(2));

    assert_eq!(rx.recv_timeout(Duration::from_millis(10)).unwrap(), 10);
    assert_eq!(tx.offer(50), Ok(()));

    let clear = seen.of("queue.clear");
    assert_eq!(clear.len(), 1);
    assert_eq!(clear[0].u64("lost"), Some(2));

    let row = queue_row(&stats, "capture.pump");
    assert_eq!(row.sent, 3);
    assert_eq!(row.full, 2);
    assert_eq!(row.lost, 2);
    assert_eq!(row.deepest, 2);
    assert_eq!(row.depth, 2);
}

#[tokio::test(start_paused = true)]
async fn a_reader_that_lags_skips_ahead_and_the_loss_is_counted() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let bus = fanout::<u32>(&stats, "audio", 4);
    let mut slow = bus.subscribe();
    let mut fast = bus.subscribe();

    let mut got_fast = Vec::new();
    for i in 0..10 {
        bus.send(i);
        got_fast.push(fast.recv().await.unwrap());
    }
    assert_eq!(got_fast, (0..10).collect::<Vec<_>>());
    assert_eq!(slow.len(), 10);

    assert_eq!(slow.recv().await, Err(RecvError::Lagged(6)));
    let full = seen.of("queue.full");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].text("queue"), Some("audio"));
    assert_eq!(full[0].u64("capacity"), Some(4));

    let row = queue_row(&stats, "audio");
    assert_eq!(row.lost, 6);
    assert_eq!(row.full, 1);

    // Next receive succeeds with the live edge and clears the full flag.
    assert_eq!(slow.recv().await, Ok(6));
    assert_eq!(seen.count("queue.clear"), 1);
}

#[test]
fn the_same_name_is_the_same_row() {
    let stats = Stats::new();
    {
        let (tx, mut rx) = bounded::<u32>(&stats, "tape", 4, Full::End);
        tx.offer(1).unwrap();
        assert_eq!(rx.try_recv().unwrap(), 1);
    }
    {
        let (tx, mut rx) = bounded::<u32>(&stats, "tape", 4, Full::End);
        tx.offer(2).unwrap();
        assert_eq!(rx.try_recv().unwrap(), 2);
    }
    assert_eq!(stats.tally().queues.len(), 1);
    let row = queue_row(&stats, "tape");
    assert_eq!(row.sent, 2);
    assert_eq!(row.depth, 0);
}
