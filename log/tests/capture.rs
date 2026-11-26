//! A capture hears this thread, and whatever it carries the log to; nothing else.

use std::thread;
use zutalk_log::{Level, capture, carrier, carry, info};

#[test]
fn a_capture_hears_this_thread_and_whatever_it_carries_the_log_to() {
    let seen = capture(Level::Info);
    info!("test.here", n = 1u8);
    let carried = carrier();
    thread::spawn(move || {
        let _entered = carried.enter();
        info!("test.carried", n = 2u8);
    })
    .join()
    .unwrap();
    thread::spawn(|| info!("test.alone", n = 3u8)).join().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).build().unwrap();
    let task = runtime.spawn(carry(async { info!("test.task", n = 4u8) }));
    runtime.block_on(task).unwrap();
    let codes: Vec<&str> = seen.all().iter().map(|line| line.code).collect();
    assert_eq!(codes, ["test.here", "test.carried", "test.task"]);
}

#[test]
fn two_captures_side_by_side_hear_only_their_own() {
    let one = |code: u8| {
        thread::spawn(move || {
            let seen = capture(Level::Info);
            for _ in 0..100 {
                info!("test.side", by = code);
            }
            seen.all().iter().map(|line| line.u64("by").unwrap()).collect::<Vec<u64>>()
        })
    };
    let (a, b) = (one(1), one(2));
    let (a, b) = (a.join().unwrap(), b.join().unwrap());
    assert!(a.len() == 100 && a.iter().all(|by| *by == 1));
    assert!(b.len() == 100 && b.iter().all(|by| *by == 2));
}

#[test]
fn a_capture_that_is_dropped_hears_no_more_and_the_one_before_is_back() {
    let outer = capture(Level::Info);
    let inner = capture(Level::Info);
    info!("test.inner");
    drop(inner);
    info!("test.outer");
    assert_eq!(outer.all().iter().map(|line| line.code).collect::<Vec<_>>(), ["test.outer"]);
}

#[test]
fn a_callsite_first_touched_without_a_capture_is_still_heard_by_a_capture() {
    fn say(by: u8) {
        info!("test.first_elsewhere", by = by);
    }

    let seen = capture(Level::Info);
    // Another thread with no subscriber hits this callsite first while our capture is live.
    thread::spawn(|| say(1)).join().unwrap();
    say(2);
    assert_eq!(seen.of("test.first_elsewhere").len(), 1);
    assert_eq!(seen.of("test.first_elsewhere")[0].u64("by"), Some(2));
}
