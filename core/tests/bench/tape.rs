//! Two `Reels` ports for the simulation bench (`P1.md` §3.10):
//!
//! 1. [`CountingTape`]: counts blocks without disk I/O or encryption (used by 8-hour and no-key tests).
//! 2. [`GatedTape`]: wraps the real `tape::Writer` with a controllable blocking gate (with a 10 s
//!    real-time safety timeout so a stuck caller fails instead of hanging) and one-shot panic injection.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zutalk_core::capture::clock::Block;
use zutalk_core::capture::tape::{Reel, Reels, Writer};

/// Real-time safety cap on waiting at a closed tape gate (P1-08-bench.md): if the gate stays
/// closed this long, `append` panics so a blocking implementation fails instead of hanging forever.
pub const GATE_TIMEOUT_MS: u64 = 10_000;

/// Counting-only reel: creates the empty tape folder so `tape::recover` succeeds with 0 ms,
/// and counts every block appended with zero disk I/O.
#[derive(Clone, Default)]
pub struct CountingTape {
    blocks: Arc<AtomicU64>,
}

impl CountingTape {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Acquire)
    }

    pub fn reels(&self) -> Reels {
        let blocks = self.blocks.clone();
        Arc::new(move |dir: &Path, _key: &[u8; 32]| {
            fs::create_dir_all(dir)?;
            Ok(Box::new(CountingReel { blocks: blocks.clone() }) as Box<dyn Reel>)
        })
    }
}

struct CountingReel {
    blocks: Arc<AtomicU64>,
}

impl Reel for CountingReel {
    fn append(&mut self, _block: &Block) -> io::Result<()> {
        self.blocks.fetch_add(1, Ordering::Release);
        Ok(())
    }
}

struct GateInner {
    gate: Mutex<GateState>,
    changed: Condvar,
    blocks: AtomicU64,
    panic_at_ms: AtomicU32,
    panicked: AtomicBool,
}

struct GateState {
    closed: bool,
    waiters: usize,
}

/// Real `tape::Writer` behind a gate and one-shot panic injector.
#[derive(Clone)]
pub struct GatedTape {
    inner: Arc<GateInner>,
}

impl Default for GatedTape {
    fn default() -> Self {
        Self::new()
    }
}

impl GatedTape {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(GateInner {
                gate: Mutex::new(GateState { closed: false, waiters: 0 }),
                changed: Condvar::new(),
                blocks: AtomicU64::new(0),
                panic_at_ms: AtomicU32::new(u32::MAX),
                panicked: AtomicBool::new(false),
            }),
        }
    }

    /// Blocks appended so far (excluding a block that panicked).
    #[allow(dead_code)]
    pub fn blocks(&self) -> u64 {
        self.inner.blocks.load(Ordering::Acquire)
    }

    /// Arms a one-shot panic on the first `append` whose `block.t_ms >= at_ms`.
    pub fn panic_at(&self, at_ms: u32) {
        self.inner.panicked.store(false, Ordering::Release);
        self.inner.panic_at_ms.store(at_ms, Ordering::Release);
    }

    /// Closes the gate so `append` blocks until the returned [`GateGuard`] is dropped.
    pub fn close_gate(&self) -> GateGuard {
        let mut state = self.inner.gate.lock().expect("tape gate lock");
        state.closed = true;
        GateGuard { inner: self.inner.clone() }
    }

    /// Returns whether at least one writer is currently parked on the closed gate.
    pub fn has_waiter(&self) -> bool {
        self.inner.gate.lock().expect("tape gate lock").waiters > 0
    }

    pub fn reels(&self) -> Reels {
        let inner = self.inner.clone();
        Arc::new(move |dir: &Path, key: &[u8; 32]| {
            let writer = Writer::create(dir, key)?;
            Ok(Box::new(GatedReel { writer, inner: inner.clone() }) as Box<dyn Reel>)
        })
    }
}

/// Keeps the tape gate closed; dropping it always re-opens the gate so panics cannot deadlock.
pub struct GateGuard {
    inner: Arc<GateInner>,
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        let mut state = self.inner.gate.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = false;
        self.inner.changed.notify_all();
    }
}

struct GatedReel {
    writer: Writer,
    inner: Arc<GateInner>,
}

impl Reel for GatedReel {
    fn append(&mut self, block: &Block) -> io::Result<()> {
        let at_ms = self.inner.panic_at_ms.load(Ordering::Acquire);
        if at_ms != u32::MAX
            && block.t_ms >= at_ms
            && !self.inner.panicked.swap(true, Ordering::AcqRel)
        {
            panic!("bench: injected tape Writer::append panic at {} ms", block.t_ms);
        }

        let mut state = self.inner.gate.lock().expect("tape gate lock");
        if state.closed {
            state.waiters += 1;
            let result = self
                .inner
                .changed
                .wait_timeout_while(state, Duration::from_millis(GATE_TIMEOUT_MS), |s| s.closed)
                .expect("tape gate condvar");
            let (mut state, timed_out) = result;
            state.waiters = state.waiters.saturating_sub(1);
            if timed_out.timed_out() && state.closed {
                drop(state);
                panic!("bench: tape gate stayed closed past {GATE_TIMEOUT_MS} ms");
            }
        }

        self.writer.append(block)?;
        self.inner.blocks.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.writer.sync()
    }

    fn take_padded(&mut self) -> u64 {
        self.writer.take_padded()
    }
}
