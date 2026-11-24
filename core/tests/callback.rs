//! The device audio callback and lock-free stat/watermark updates must never allocate.
//! Kept in its own test binary because it installs a counting `GlobalAlloc`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use zutalk_core::capture::audio::{Feed, offer};
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::water::Gauge;

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, SeqCst);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, SeqCst);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn assert_zero_allocs(what: &str, f: impl FnOnce()) {
    let before = ALLOCS.load(SeqCst);
    f();
    let added = ALLOCS.load(SeqCst) - before;
    assert_eq!(added, 0, "{what} allocated {added} time(s)");
}

#[test]
fn the_device_callback_allocates_nothing() {
    let before = ALLOCS.load(SeqCst);
    let probe = std::hint::black_box(vec![1u8; 64]);
    assert!(ALLOCS.load(SeqCst) > before, "counting allocator must observe heap allocations");
    drop(probe);

    let stats = Stats::new();
    let lost = stats.counter("capture.ring.lost_samples");
    let peak = stats.peak("tree.tasks");
    let spread = stats.spread("hub.turn_us");
    let gauge = Gauge::new();

    // 16 kHz mono: ring holds 32,000 samples (RING_MS = 2_000).
    let (mut ring, _feed) = Feed::new(16_000, 1, lost.clone()).expect("feed opens");
    let burst = [0.25f32; 320];

    // Normal path: callback fits into the ring without allocating.
    assert_zero_allocs("offer (normal)", || {
        offer(&mut ring, burst.iter().copied());
    });

    // Fill the remaining ring capacity (32,000 - 320 = 31,680 samples = 99 bursts).
    for _ in 0..99 {
        offer(&mut ring, burst.iter().copied());
    }

    // Full-ring path: callback drops samples and counts them atomically without allocating.
    assert_zero_allocs("offer (ring full)", || {
        offer(&mut ring, burst.iter().copied());
    });
    let counted = stats
        .tally()
        .counts
        .into_iter()
        .find(|r| r.name == "capture.ring.lost_samples")
        .map(|r| r.n);
    assert_eq!(counted, Some(320));

    // Lock-free stat and watermark updates must allocate nothing.
    assert_zero_allocs("Counter::add", || lost.add(1));
    assert_zero_allocs("Peak::set", || peak.set(7));
    assert_zero_allocs("Spread::note", || spread.note(42));
    assert_zero_allocs("Gauge::set", || gauge.set(1_200));
}
