//! Counters, high-water peaks, fixed-bucket latency spreads, and queue rows: one table per core,
//! readable at any time as a bounded `Tally` (`P1.md` §3.13.4).
//!
//! Acquiring a handle takes a lock once when a part starts; recording on the handle is atomic,
//! lock-free and allocation-free so it is safe even in an audio callback.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use ts_rs::TS;
use zutalk_log::warn;

/// The most counter rows one `Stats` holds: sized so the fullest `Tally` fits in 8192 bytes.
pub const STATS_MOST_COUNTS: usize = 40;
/// The most peak rows one `Stats` holds: sized so the fullest `Tally` fits in 8192 bytes.
pub const STATS_MOST_PEAKS: usize = 4;
/// The most spread rows one `Stats` holds: sized so the fullest `Tally` fits in 8192 bytes.
pub const STATS_MOST_SPREADS: usize = 12;
/// The most queue rows one `Stats` holds: sized so the fullest `Tally` fits in 8192 bytes.
pub const STATS_MOST_QUEUES: usize = 16;
/// The longest stat name kept in the table: bounds the fullest `Tally` to at most 8192 bytes.
pub const STATS_NAME_MOST_BYTES: usize = 32;

/// Upper edges of the 12 finite spread buckets; a 13th bucket holds values above 5000.
/// A 1-2-5 scale from 1 to 5000 separates sub-ms, sub-second and multi-second latencies.
pub const SPREAD_EDGES: [u64; 12] = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 2_000, 5_000];

/// One core's live statistics. Cloneable handle; two `Stats::new()` never share rows.
#[derive(Clone, Default)]
pub struct Stats(Arc<Mutex<Table>>);

#[derive(Default)]
struct Table {
    counts: Slot<CountCell>,
    peaks: Slot<PeakCell>,
    spreads: Slot<SpreadCell>,
    queues: Slot<QueueCell>,
}

struct Slot<C> {
    rows: Vec<(&'static str, Arc<C>)>,
    warned: bool,
}

impl<C> Default for Slot<C> {
    fn default() -> Self {
        Self { rows: Vec::new(), warned: false }
    }
}

impl<C> Slot<C> {
    fn get_or_insert(
        &mut self,
        name: &'static str,
        limit: usize,
        make: impl FnOnce() -> C,
    ) -> (Arc<C>, bool) {
        if let Some((_, cell)) = self.rows.iter().find(|(n, _)| *n == name) {
            return (cell.clone(), false);
        }
        let cell = Arc::new(make());
        if name.len() > STATS_NAME_MOST_BYTES || self.rows.len() >= limit {
            let emit = !self.warned;
            self.warned = true;
            return (cell, emit);
        }
        self.rows.push((name, cell.clone()));
        (cell, false)
    }
}

#[derive(Default)]
struct CountCell(AtomicU64);

#[derive(Default)]
struct PeakCell {
    now: AtomicU64,
    most: AtomicU64,
}

struct SpreadCell {
    buckets: [AtomicU64; SPREAD_EDGES.len() + 1],
    most: AtomicU64,
}

impl Default for SpreadCell {
    fn default() -> Self {
        Self { buckets: std::array::from_fn(|_| AtomicU64::new(0)), most: AtomicU64::new(0) }
    }
}

pub(crate) struct QueueCell {
    name: &'static str,
    capacity: AtomicU64,
    depth: AtomicU64,
    deepest: AtomicU64,
    sent: AtomicU64,
    full: AtomicU64,
    lost: AtomicU64,
    is_full: AtomicBool,
    lost_run: AtomicU64,
}

impl QueueCell {
    fn new(name: &'static str, capacity: usize) -> Self {
        Self {
            name,
            capacity: AtomicU64::new(capacity as u64),
            depth: AtomicU64::new(0),
            deepest: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            full: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            is_full: AtomicBool::new(false),
            lost_run: AtomicU64::new(0),
        }
    }
}

/// Handle to one monotonic counter. `add` is a single atomic fetch-add.
#[derive(Clone)]
pub struct Counter(Arc<CountCell>);

impl Counter {
    pub fn add(&self, n: u64) {
        self.0.0.fetch_add(n, Ordering::Relaxed);
    }
}

/// Handle to one gauge with its high-water mark. `set` is lock-free and allocation-free.
#[derive(Clone)]
pub struct Peak(Arc<PeakCell>);

impl Peak {
    pub fn set(&self, now: u64) {
        self.0.now.store(now, Ordering::Relaxed);
        self.0.most.fetch_max(now, Ordering::Relaxed);
    }
}

/// Handle to one fixed-bucket distribution. `note` is lock-free and allocation-free.
#[derive(Clone)]
pub struct Spread(Arc<SpreadCell>);

impl Spread {
    pub fn note(&self, value: u64) {
        self.0.buckets[bucket(value)].fetch_add(1, Ordering::Relaxed);
        self.0.most.fetch_max(value, Ordering::Relaxed);
    }
}

/// Handle written by `rt::queue` for one named queue row.
#[derive(Clone)]
pub(crate) struct QueueStat(Arc<QueueCell>);

impl QueueStat {
    pub(crate) fn name(&self) -> &'static str {
        self.0.name
    }

    pub(crate) fn capacity(&self) -> u64 {
        self.0.capacity.load(Ordering::Relaxed)
    }

    /// Record one item accepted into a bounded or thread queue; returns the lost count if the
    /// queue was previously full and has now cleared.
    pub(crate) fn note_sent(&self) -> Option<u64> {
        let depth = self.0.depth.fetch_add(1, Ordering::Relaxed) + 1;
        self.0.deepest.fetch_max(depth, Ordering::Relaxed);
        self.0.sent.fetch_add(1, Ordering::Relaxed);
        if self.0.is_full.swap(false, Ordering::AcqRel) {
            Some(self.0.lost_run.swap(0, Ordering::AcqRel))
        } else {
            None
        }
    }

    /// Record `n` items received from a bounded or thread queue.
    pub(crate) fn note_recv(&self, n: u64) {
        // dropped: fetch_update with Some never returns Err
        let _ = self
            .0
            .depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |d| Some(d.saturating_sub(n)));
    }

    /// Record a waiting sender that found the queue full before waiting (not a `queue.full` event).
    pub(crate) fn note_waited(&self) {
        self.0.full.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one item rejected by a full queue; returns `true` on the transition into full.
    pub(crate) fn note_rejected(&self) -> bool {
        self.0.full.fetch_add(1, Ordering::Relaxed);
        self.0.lost.fetch_add(1, Ordering::Relaxed);
        self.0.lost_run.fetch_add(1, Ordering::AcqRel);
        !self.0.is_full.swap(true, Ordering::AcqRel)
    }

    /// Record one item broadcast on a fanout bus with its current buffered depth.
    pub(crate) fn note_fanout_sent(&self, depth: u64) {
        self.0.depth.store(depth, Ordering::Relaxed);
        self.0.deepest.fetch_max(depth, Ordering::Relaxed);
        self.0.sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one reader lagging by `skipped` items on a fanout bus; returns `true` on transition.
    pub(crate) fn note_lagged(&self, skipped: u64) -> bool {
        self.0.full.fetch_add(1, Ordering::Relaxed);
        self.0.lost.fetch_add(skipped, Ordering::Relaxed);
        self.0.lost_run.fetch_add(skipped, Ordering::AcqRel);
        !self.0.is_full.swap(true, Ordering::AcqRel)
    }

    /// Record a fanout reader receiving cleanly; returns the lost count if clearing a lagged run.
    pub(crate) fn note_fanout_recv(&self, depth: u64) -> Option<u64> {
        self.0.depth.store(depth, Ordering::Relaxed);
        if self.0.is_full.swap(false, Ordering::AcqRel) {
            Some(self.0.lost_run.swap(0, Ordering::AcqRel))
        } else {
            None
        }
    }
}

impl Stats {
    pub fn new() -> Stats {
        Stats::default()
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Find or create the counter row for `name`.
    pub fn counter(&self, name: &'static str) -> Counter {
        let (cell, emit) =
            self.lock().counts.get_or_insert(name, STATS_MOST_COUNTS, Default::default);
        if emit {
            warn!("stats.full", kind = "counts");
        }
        Counter(cell)
    }

    /// Find or create the peak row for `name`.
    pub fn peak(&self, name: &'static str) -> Peak {
        let (cell, emit) =
            self.lock().peaks.get_or_insert(name, STATS_MOST_PEAKS, Default::default);
        if emit {
            warn!("stats.full", kind = "peaks");
        }
        Peak(cell)
    }

    /// Find or create the spread row for `name`.
    pub fn spread(&self, name: &'static str) -> Spread {
        let (cell, emit) =
            self.lock().spreads.get_or_insert(name, STATS_MOST_SPREADS, Default::default);
        if emit {
            warn!("stats.full", kind = "spreads");
        }
        Spread(cell)
    }

    /// Find or create the queue row for `name` (used by `rt::queue`).
    pub(crate) fn queue(&self, name: &'static str, capacity: usize) -> QueueStat {
        let (cell, emit) = self
            .lock()
            .queues
            .get_or_insert(name, STATS_MOST_QUEUES, || QueueCell::new(name, capacity));
        cell.capacity.store(capacity as u64, Ordering::Relaxed);
        if emit {
            warn!("stats.full", kind = "queues");
        }
        QueueStat(cell)
    }

    /// A deterministic snapshot of every row, sorted by name. Not for hot paths.
    pub fn tally(&self) -> Tally {
        let table = self.lock();
        let mut counts: Vec<Count> = table
            .counts
            .rows
            .iter()
            .map(|(name, cell)| Count { name, n: cell.0.load(Ordering::Relaxed) })
            .collect();
        counts.sort_by_key(|row| row.name);

        let mut peaks: Vec<PeakRow> = table
            .peaks
            .rows
            .iter()
            .map(|(name, cell)| PeakRow {
                name,
                now: cell.now.load(Ordering::Relaxed),
                most: cell.most.load(Ordering::Relaxed),
            })
            .collect();
        peaks.sort_by_key(|row| row.name);

        let mut spreads: Vec<SpreadRow> = table
            .spreads
            .rows
            .iter()
            .map(|(name, cell)| {
                let buckets: [u64; SPREAD_EDGES.len() + 1] =
                    std::array::from_fn(|i| cell.buckets[i].load(Ordering::Relaxed));
                let n = buckets.iter().sum();
                let most = cell.most.load(Ordering::Relaxed);
                SpreadRow {
                    name,
                    n,
                    p50: quantile(&buckets, most, 50),
                    p99: quantile(&buckets, most, 99),
                    most,
                }
            })
            .collect();
        spreads.sort_by_key(|row| row.name);

        let mut queues: Vec<QueueRow> = table
            .queues
            .rows
            .iter()
            .map(|(name, cell)| QueueRow {
                name,
                capacity: cell.capacity.load(Ordering::Relaxed),
                depth: cell.depth.load(Ordering::Relaxed),
                deepest: cell.deepest.load(Ordering::Relaxed),
                sent: cell.sent.load(Ordering::Relaxed),
                full: cell.full.load(Ordering::Relaxed),
                lost: cell.lost.load(Ordering::Relaxed),
            })
            .collect();
        queues.sort_by_key(|row| row.name);

        Tally { counts, peaks, spreads, queues }
    }
}

/// Which bucket of `SPREAD_EDGES` holds `value`; `SPREAD_EDGES.len()` is the overflow bucket.
pub fn bucket(value: u64) -> usize {
    SPREAD_EDGES.iter().position(|&edge| value <= edge).unwrap_or(SPREAD_EDGES.len())
}

/// The `pct`-th percentile of a bucketed spread: the upper edge of the bucket holding that rank,
/// or `most` when it falls in the overflow bucket above `SPREAD_EDGES`. Returns 0 if empty.
pub fn quantile(buckets: &[u64; SPREAD_EDGES.len() + 1], most: u64, pct: u8) -> u64 {
    let total: u64 = buckets.iter().sum();
    if total == 0 {
        return 0;
    }
    let need = total.saturating_mul(u64::from(pct)).div_ceil(100).max(1);
    let mut seen = 0u64;
    for (i, &count) in buckets.iter().enumerate() {
        seen = seen.saturating_add(count);
        if seen >= need {
            return SPREAD_EDGES.get(i).copied().unwrap_or(most);
        }
    }
    most
}

/// A point-in-time snapshot of one core's stats. Bounded in size so it travels as a signal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
pub struct Tally {
    pub counts: Vec<Count>,
    pub peaks: Vec<PeakRow>,
    pub spreads: Vec<SpreadRow>,
    pub queues: Vec<QueueRow>,
}

impl Tally {
    pub fn len(&self) -> usize {
        self.counts.len() + self.peaks.len() + self.spreads.len() + self.queues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
pub struct Count {
    pub name: &'static str,
    #[ts(type = "number")]
    pub n: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
pub struct PeakRow {
    pub name: &'static str,
    #[ts(type = "number")]
    pub now: u64,
    #[ts(type = "number")]
    pub most: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
pub struct SpreadRow {
    pub name: &'static str,
    #[ts(type = "number")]
    pub n: u64,
    #[ts(type = "number")]
    pub p50: u64,
    #[ts(type = "number")]
    pub p99: u64,
    #[ts(type = "number")]
    pub most: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
pub struct QueueRow {
    pub name: &'static str,
    #[ts(type = "number")]
    pub capacity: u64,
    #[ts(type = "number")]
    pub depth: u64,
    #[ts(type = "number")]
    pub deepest: u64,
    #[ts(type = "number")]
    pub sent: u64,
    #[ts(type = "number")]
    pub full: u64,
    #[ts(type = "number")]
    pub lost: u64,
}

/// Only the rows in `after` that were added or changed compared to `before`.
pub fn moved(before: &Tally, after: &Tally) -> Tally {
    Tally {
        counts: after
            .counts
            .iter()
            .filter(|row| before.counts.iter().find(|prev| prev.name == row.name) != Some(*row))
            .cloned()
            .collect(),
        peaks: after
            .peaks
            .iter()
            .filter(|row| before.peaks.iter().find(|prev| prev.name == row.name) != Some(*row))
            .cloned()
            .collect(),
        spreads: after
            .spreads
            .iter()
            .filter(|row| before.spreads.iter().find(|prev| prev.name == row.name) != Some(*row))
            .cloned()
            .collect(),
        queues: after
            .queues
            .iter()
            .filter(|row| before.queues.iter().find(|prev| prev.name == row.name) != Some(*row))
            .cloned()
            .collect(),
    }
}
