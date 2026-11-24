//! Stats: counters, peaks, fixed-bucket spreads, limits, and diffs (`rt/stats.rs`).

use std::thread;
use std::time::Duration;
use zutalk_core::rt::signal::Register;
use zutalk_core::rt::stats::{
    Count, PeakRow, QueueRow, SPREAD_EDGES, STATS_MOST_COUNTS, STATS_MOST_PEAKS, STATS_MOST_QUEUES,
    STATS_MOST_SPREADS, STATS_NAME_MOST_BYTES, STATS_SUMMARY_EVERY_MS, SpreadRow, Stats, Tally,
    bucket, moved, publish, quantile,
};
use zutalk_core::rt::supervise::Tree;
use zutalk_log::{Level, capture};

#[test]
fn a_name_is_one_row_however_often_it_is_asked_for() {
    let stats = Stats::new();
    let a = stats.counter("frame.made");
    let b = stats.counter("frame.made");
    a.add(3);
    b.add(4);
    a.add(5);

    let tally = stats.tally();
    assert_eq!(tally.counts, vec![Count { name: "frame.made", n: 12 }]);
}

#[test]
fn a_peak_keeps_the_most_it_has_been() {
    let stats = Stats::new();
    let peak = stats.peak("tree.tasks");
    peak.set(2);
    peak.set(9);
    peak.set(4);

    let tally = stats.tally();
    assert_eq!(tally.peaks, vec![PeakRow { name: "tree.tasks", now: 4, most: 9 }]);
}

#[test]
fn a_spread_puts_each_value_in_its_bucket() {
    let cases = [
        (0, 0),
        (1, 0),
        (2, 1),
        (3, 2),
        (5, 2),
        (6, 3),
        (1_000, 9),
        (5_000, 11),
        (5_001, 12),
        (u64::MAX, 12),
    ];
    for (value, expected) in cases {
        assert_eq!(bucket(value), expected, "value {value}");
    }

    let stats = Stats::new();
    let spread = stats.spread("hub.turn_us");
    for value in [0, 1, 2, 3, 5_000, 5_001] {
        spread.note(value);
    }
    let tally = stats.tally();
    assert_eq!(tally.spreads.len(), 1);
    let row = &tally.spreads[0];
    assert_eq!(row.name, "hub.turn_us");
    assert_eq!(row.n, 6);
    assert_eq!(row.most, 5_001);
    // 6 values: p50 is the 3rd (2 -> edge 2); p99 is the 6th (5001 -> overflow -> most 5001).
    assert_eq!(row.p50, 2);
    assert_eq!(row.p99, 5_001);
}

#[test]
fn the_quantile_is_the_upper_edge_of_its_bucket() {
    let mut empty = [0u64; SPREAD_EDGES.len() + 1];
    assert_eq!(quantile(&empty, 0, 50), 0);
    assert_eq!(quantile(&empty, 0, 99), 0);

    // 100 samples: 50 in bucket 2 (<= 5), 49 in bucket 6 (<= 100), 1 in bucket 10 (<= 2000).
    let mut b = [0u64; SPREAD_EDGES.len() + 1];
    b[2] = 50;
    b[6] = 49;
    b[10] = 1;
    let cases = [
        (b, 1_500, 1, 5),
        (b, 1_500, 50, 5),
        (b, 1_500, 51, 100),
        (b, 1_500, 99, 100),
        (b, 1_500, 100, 2_000),
    ];
    for (buckets, most, pct, want) in cases {
        assert_eq!(quantile(&buckets, most, pct), want, "pct {pct}");
    }

    // Overflow bucket (> 5000) reports `most`.
    empty[SPREAD_EDGES.len()] = 3;
    assert_eq!(quantile(&empty, 9_876, 50), 9_876);
    assert_eq!(quantile(&empty, 9_876, 99), 9_876);
}

#[test]
fn counting_takes_no_lock_and_no_memory() {
    let stats = Stats::new();
    let counter = stats.counter("capture.blocks");
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let c = counter.clone();
            thread::spawn(move || {
                for _ in 0..10_000 {
                    c.add(1);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(stats.tally().counts, vec![Count { name: "capture.blocks", n: 80_000 }]);
}

#[test]
fn more_rows_than_there_is_room_for_are_said_once_and_left_out() {
    const NAMES: [&str; 42] = [
        "c00", "c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09", "c10", "c11", "c12",
        "c13", "c14", "c15", "c16", "c17", "c18", "c19", "c20", "c21", "c22", "c23", "c24", "c25",
        "c26", "c27", "c28", "c29", "c30", "c31", "c32", "c33", "c34", "c35", "c36", "c37", "c38",
        "c39", "c40", "c41",
    ];
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    for name in &NAMES[..STATS_MOST_COUNTS] {
        stats.counter(name).add(1);
    }
    assert_eq!(seen.count("stats.full"), 0);

    let extra_one = stats.counter(NAMES[40]);
    let extra_two = stats.counter(NAMES[41]);
    extra_one.add(7);
    extra_two.add(9);

    let full = seen.of("stats.full");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].level, Level::Warn);
    assert_eq!(full[0].text("kind"), Some("counts"));
    assert_eq!(stats.tally().counts.len(), STATS_MOST_COUNTS);
    assert_eq!(seen.refused(), 0);

    // An overlong name is also refused from the table and emits once for its kind.
    let overlong = stats.peak("this_name_is_longer_than_thirty_two_bytes");
    overlong.set(5);
    assert_eq!(stats.tally().peaks.len(), 0);
    let peak_full: Vec<_> =
        seen.of("stats.full").into_iter().filter(|l| l.text("kind") == Some("peaks")).collect();
    assert_eq!(peak_full.len(), 1);
}

#[test]
fn the_fullest_tally_fits_one_channel_message() {
    let big = 1u64 << 40;
    let name = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_eq!(name.len(), STATS_NAME_MOST_BYTES);

    let tally = Tally {
        counts: (0..STATS_MOST_COUNTS).map(|_| Count { name, n: big }).collect(),
        peaks: (0..STATS_MOST_PEAKS).map(|_| PeakRow { name, now: big, most: big }).collect(),
        spreads: (0..STATS_MOST_SPREADS)
            .map(|_| SpreadRow { name, n: big, p50: big, p99: big, most: big })
            .collect(),
        queues: (0..STATS_MOST_QUEUES)
            .map(|_| QueueRow {
                name,
                capacity: big,
                depth: big,
                deepest: big,
                sent: big,
                full: big,
                lost: big,
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&tally).unwrap();
    assert!(bytes.len() <= 8_192, "fullest Tally is {} bytes (> 8192)", bytes.len());
}

#[test]
fn only_what_moved_is_in_the_difference() {
    let base = Tally {
        counts: vec![Count { name: "a", n: 1 }, Count { name: "b", n: 2 }],
        peaks: vec![PeakRow { name: "p", now: 1, most: 3 }],
        spreads: vec![SpreadRow { name: "s", n: 2, p50: 5, p99: 10, most: 8 }],
        queues: vec![QueueRow {
            name: "q",
            capacity: 4,
            depth: 0,
            deepest: 1,
            sent: 1,
            full: 0,
            lost: 0,
        }],
    };

    // Nothing changed.
    let same = moved(&base, &base);
    assert_eq!(same, Tally::default());
    assert!(same.is_empty());

    // A row was added.
    let mut added = base.clone();
    added.counts.push(Count { name: "c", n: 0 });
    let diff = moved(&base, &added);
    assert_eq!(diff.counts, vec![Count { name: "c", n: 0 }]);
    assert!(diff.peaks.is_empty());
    assert!(diff.spreads.is_empty());
    assert!(diff.queues.is_empty());

    // One count and one queue changed.
    let mut changed = base.clone();
    changed.counts[1].n = 5;
    changed.queues[0].sent = 2;
    let diff = moved(&base, &changed);
    assert_eq!(diff.counts, vec![Count { name: "b", n: 5 }]);
    assert!(diff.peaks.is_empty());
    assert!(diff.spreads.is_empty());
    assert_eq!(diff.queues, vec![changed.queues[0].clone()]);
}

#[test]
fn two_stats_do_not_see_each_other() {
    let a = Stats::new();
    let b = Stats::new();
    a.counter("hub.turns").add(3);
    b.counter("hub.turns").add(10);

    assert_eq!(a.tally().counts, vec![Count { name: "hub.turns", n: 3 }]);
    assert_eq!(b.tally().counts, vec![Count { name: "hub.turns", n: 10 }]);
}

#[tokio::test(start_paused = true)]
async fn once_a_minute_what_moved_is_written() {
    let seen = capture(Level::Debug);
    let stats = Stats::new();
    let out = Register::new(Tally::default());
    let runner = Tree::root("test", Stats::new());
    runner.spawn("stats", publish(stats.clone(), out.clone()));
    tokio::task::yield_now().await;

    stats.counter("frame.made").add(7);
    stats.counter("hub.turns").add(3);
    let spread = stats.spread("hub.turn_us");
    spread.note(4);
    spread.note(12);
    spread.note(80);

    tokio::time::advance(Duration::from_millis(STATS_SUMMARY_EVERY_MS)).await;
    tokio::task::yield_now().await;

    let summaries = seen.of("stats.summary");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].level, Level::Info);
    assert_eq!(summaries[0].u64("snap"), Some(0));
    assert_eq!(summaries[0].u64("up_ms"), Some(STATS_SUMMARY_EVERY_MS));
    assert_eq!(summaries[0].u64("rows"), Some(3));

    let counts = seen.of("stat.count");
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[0].u64("snap"), Some(0));
    assert_eq!(counts[0].text("name"), Some("frame.made"));
    assert_eq!(counts[0].u64("n"), Some(7));
    assert_eq!(counts[1].u64("snap"), Some(0));
    assert_eq!(counts[1].text("name"), Some("hub.turns"));
    assert_eq!(counts[1].u64("n"), Some(3));

    let spreads = seen.of("stat.spread");
    assert_eq!(spreads.len(), 1);
    assert_eq!(spreads[0].u64("snap"), Some(0));
    assert_eq!(spreads[0].text("name"), Some("hub.turn_us"));
    assert_eq!(spreads[0].u64("n"), Some(3));
    assert_eq!(spreads[0].u64("p50"), Some(20));
    assert_eq!(spreads[0].u64("p99"), Some(100));
    assert_eq!(spreads[0].u64("most"), Some(80));

    // Another minute with nothing moved: only stats.summary with rows = 0.
    tokio::time::advance(Duration::from_millis(STATS_SUMMARY_EVERY_MS)).await;
    tokio::task::yield_now().await;

    let summaries = seen.of("stats.summary");
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[1].u64("up_ms"), Some(2 * STATS_SUMMARY_EVERY_MS));
    assert_eq!(summaries[1].u64("rows"), Some(0));
    assert_eq!(seen.count("stat.count"), 2);
    assert_eq!(seen.count("stat.spread"), 1);

    // Move one counter: the next minute writes only that one row.
    stats.counter("frame.made").add(2);
    tokio::time::advance(Duration::from_millis(STATS_SUMMARY_EVERY_MS)).await;
    tokio::task::yield_now().await;

    let summaries = seen.of("stats.summary");
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[2].u64("up_ms"), Some(3 * STATS_SUMMARY_EVERY_MS));
    assert_eq!(summaries[2].u64("rows"), Some(1));
    let counts = seen.of("stat.count");
    assert_eq!(counts.len(), 3);
    assert_eq!(counts[2].text("name"), Some("frame.made"));
    assert_eq!(counts[2].u64("n"), Some(9));

    runner.cancel();
    runner.wait().await;
    assert_eq!(seen.refused(), 0);
}
