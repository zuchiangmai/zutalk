//! Register and Reading (`rt/signal.rs`): latest-value signal with monotonic sequence numbers.

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::time::timeout;
use zutalk_core::rt::signal::Register;

#[tokio::test(start_paused = true)]
async fn a_register_keeps_only_the_latest_and_counts() {
    let reg = Register::new("zero");
    let mut reader = reg.read();

    reg.set("one");
    reg.set("two");
    reg.set("three");

    let (seq, value) = reader.next().await.expect("writer still alive");
    assert_eq!(seq, 3);
    assert_eq!(*value, "three");
}

#[tokio::test(start_paused = true)]
async fn a_reader_that_comes_late_gets_the_latest_at_once() {
    let reg = Register::new(10u32);
    reg.set(20);
    reg.set(30);

    let mut late = reg.read();
    let (seq, value) = late.now();
    assert_eq!(seq, 2);
    assert_eq!(*value, 30);

    // Waiting for `next()` after `now()` waits for a new change rather than repeating `30`.
    let pending = timeout(Duration::from_millis(500), late.next()).await;
    assert!(pending.is_err(), "late reader must not see stale update as a new change");

    reg.set(40);
    let (seq, value) = late.next().await.expect("writer still alive");
    assert_eq!(seq, 3);
    assert_eq!(*value, 40);
}

#[tokio::test(start_paused = true)]
async fn a_change_that_changes_nothing_is_not_a_new_value() {
    let reg = Register::new(vec![1, 2, 3]);
    let mut reader = reg.read();
    let (seq_before, val_before) = reader.now();
    assert_eq!(seq_before, 0);

    reg.change(|current| {
        assert_eq!(current.as_slice(), &[1, 2, 3]);
        None
    });

    let (seq_after, val_after) = reader.now();
    assert_eq!(seq_after, 0);
    assert!(Arc::ptr_eq(&val_before, &val_after));

    let woke = timeout(Duration::from_millis(1_000), reader.next()).await;
    assert!(woke.is_err(), "change returning None must not wake waiting readers");

    reg.change(|current| {
        let mut next = current.clone();
        next.push(4);
        Some(next)
    });

    let (seq, val) = reader.next().await.expect("woken by real change");
    assert_eq!(seq, 1);
    assert_eq!(*val, vec![1, 2, 3, 4]);
}

#[test]
fn two_writers_never_share_a_number() {
    let reg = Register::new(0usize);
    let reader = reg.read();

    let handles: Vec<_> = (0..2)
        .map(|worker| {
            let writer = reg.clone();
            thread::spawn(move || {
                for i in 0..1_000 {
                    writer.set(worker * 1_000 + i + 1);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("writer thread finished");
    }

    let (seq, _) = reader.now();
    assert_eq!(seq, 2_000);
}

#[tokio::test(start_paused = true)]
async fn when_the_writers_are_gone_the_reader_is_told() {
    let reg = Register::new("initial");
    let clone = reg.clone();
    let mut reader = reg.read();

    let (seq, val) = reader.now();
    assert_eq!(seq, 0);
    assert_eq!(*val, "initial");

    drop(reg);
    let still_open = timeout(Duration::from_millis(200), reader.next()).await;
    assert!(still_open.is_err(), "one writer clone remains alive");

    drop(clone);
    assert_eq!(reader.next().await, None);
}
