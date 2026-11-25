//! L3 view query protocol, bounded invalidation ring, and concurrent cancellable `ViewEngine`
//! (`P2.md` §3.6, `PLAN.md` §2.5 & §4.1).
//!
//! Queries never enter the hub mailbox: they execute off-hub via `Tree::blocking("view.query", …)`
//! under a fixed semaphore (`QUERY_CONCURRENT_MAX_JOBS`), and a newer query on the same slot
//! cancels and interrupts any superseded in-flight query.

use crate::capture::tape;
use crate::knowledge::glossary::Term;
use crate::knowledge::topic::{self, ALWAYS_TOPIC_ID, Topic, TopicSummary};
use crate::out::city::{Calendar, Filing};
use crate::record::notes::Note;
use crate::record::shelf::{FILING, Shelf};
use crate::record::takes::{self, RefineStatus, TakeInfo};
use crate::rt::signal::{Reading, Register};
use crate::rt::stats::{Counter, Spread};
use crate::rt::supervise::Tree;
use crate::view::index::{
    LibraryFilter, LibraryPage, LibraryRow, LibrarySummary, Reader, SearchResult,
};
use crate::view::transcript::{
    self, CompareChunk, TRANSCRIPT_CHUNK_LINES, TranscriptChunk, compare_lines, slice_chunk,
};
use rusqlite::InterruptHandle;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use zutalk_log::{debug, info, warn};

/// Latency budget for a single L3 view query before emitting `query.slow` (`PLAN.md` §4.1).
pub const QUERY_BUDGET_MS: u64 = 100;
/// Maximum concurrent blocking L3 queries allowed on the `work` subtree (`P2.md` §3.6.2).
pub const QUERY_CONCURRENT_MAX_JOBS: usize = 8;
/// Maximum coalesced invalidation events kept in `Outlet.invalidated` (`P2.md` §3.6.2).
pub const KEEP_INVALIDATIONS_EVENTS: usize = 64;

/// Which view family a change invalidated (`P2.md` §3.6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ViewKind {
    Library,
    Search,
    Recording,
    Transcript,
    Glossary,
    Knowledge,
    Calendar,
    Outbound,
}

impl ViewKind {
    /// Every view family, used for full invalidation on identity switch or index rebuild.
    pub const ALL: [ViewKind; 8] = [
        ViewKind::Library,
        ViewKind::Search,
        ViewKind::Recording,
        ViewKind::Transcript,
        ViewKind::Glossary,
        ViewKind::Knowledge,
        ViewKind::Calendar,
        ViewKind::Outbound,
    ];

    /// Stable code emitted in `view.invalidate` log events.
    pub const fn code(self) -> &'static str {
        match self {
            ViewKind::Library => "library",
            ViewKind::Search => "search",
            ViewKind::Recording => "recording",
            ViewKind::Transcript => "transcript",
            ViewKind::Glossary => "glossary",
            ViewKind::Knowledge => "knowledge",
            ViewKind::Calendar => "calendar",
            ViewKind::Outbound => "outbound",
        }
    }
}

/// One view invalidation event telling the interface which view (and optional recording key) moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Invalidate {
    pub view: ViewKind,
    pub key: Option<String>,
    #[ts(type = "number")]
    pub version: u64,
}

/// Bounded ring of recent coalesced invalidations carried by `Outlet.invalidated`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Invalidations {
    #[ts(type = "number")]
    pub next: u64,
    pub items: Vec<Invalidate>,
}

impl Invalidations {
    /// Coalesces any previous entry with the same `(view, key)`, appends the new entry, caps at
    /// [`KEEP_INVALIDATIONS_EVENTS`], and advances `next`.
    pub fn push(&mut self, view: ViewKind, key: Option<String>, version: u64) {
        self.items.retain(|item| !(item.view == view && item.key == key));
        self.items.push(Invalidate { view, key, version });
        if self.items.len() > KEEP_INVALIDATIONS_EVENTS {
            let excess = self.items.len() - KEEP_INVALIDATIONS_EVENTS;
            self.items.drain(0..excess);
        }
        self.next = self.next.max(version);
    }
}

/// Complete detail header of one recording returned by [`Query::Recording`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct RecordingView {
    pub row: LibraryRow,
    pub shown: Vec<String>,
    pub source: String,
    pub take: u32,
    pub takes: Vec<TakeInfo>,
    pub speakers: BTreeMap<String, String>,
    pub notes: Vec<Note>,
    pub note: String,
    pub filed: Option<Filing>,
    pub refine: RefineStatus,
}

/// One recording's outbound city-filing record returned by [`Query::Outbound`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct OutboundRow {
    pub id: String,
    pub title: String,
    #[ts(type = "number")]
    pub started: i64,
    pub filing: Filing,
}

/// Slot used to coalesce/cancel superseded queries of the same interaction class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuerySlot {
    Search,
    Library,
    Recording,
    Transcript,
    Compare,
    Knowledge,
    Other,
}

/// All L3 view queries supported by [`ViewEngine`] (`P2.md` §3.6.2, `P6.md` §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "view")]
pub enum Query {
    LibrarySummary {
        #[serde(default)]
        tz_offset_min: i32,
    },
    LibraryMonth {
        month: String,
        #[serde(default)]
        tz_offset_min: i32,
        #[serde(default)]
        filter: LibraryFilter,
        #[serde(default)]
        limit: u32,
        #[serde(default)]
        offset: u32,
    },
    LibraryRecent {
        #[serde(default)]
        filter: LibraryFilter,
        #[serde(default)]
        limit: u32,
        #[serde(default)]
        offset: u32,
    },
    Search {
        query: String,
        #[serde(default)]
        limit: u32,
    },
    Recording {
        id: String,
    },
    Transcript {
        id: String,
        take: Option<u32>,
        chunk: u32,
    },
    Compare {
        id: String,
        take_a: u32,
        take_b: u32,
        chunk: u32,
    },
    Glossary,
    Topics,
    Topic {
        id: String,
    },
    Calendar,
    Outbound,
}

impl Query {
    /// Stable plain code for `query.done` and `query.slow` log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Query::LibrarySummary { .. } => "library.summary",
            Query::LibraryMonth { .. } => "library.month",
            Query::LibraryRecent { .. } => "library.recent",
            Query::Search { .. } => "search",
            Query::Recording { .. } => "recording",
            Query::Transcript { .. } => "transcript",
            Query::Compare { .. } => "compare",
            Query::Glossary => "glossary",
            Query::Topics => "topics",
            Query::Topic { .. } => "topic",
            Query::Calendar => "calendar",
            Query::Outbound => "outbound",
        }
    }

    /// Slot in which a newer query cancels any in-flight older query.
    pub const fn slot(&self) -> QuerySlot {
        match self {
            Query::LibrarySummary { .. }
            | Query::LibraryMonth { .. }
            | Query::LibraryRecent { .. } => QuerySlot::Library,
            Query::Search { .. } => QuerySlot::Search,
            Query::Recording { .. } => QuerySlot::Recording,
            Query::Transcript { .. } => QuerySlot::Transcript,
            Query::Compare { .. } => QuerySlot::Compare,
            Query::Topic { .. } => QuerySlot::Knowledge,
            Query::Glossary | Query::Topics | Query::Calendar | Query::Outbound => QuerySlot::Other,
        }
    }
}

/// Typed payload inside [`QueryReply`].
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "view", content = "data")]
pub enum QueryValue {
    LibrarySummary(LibrarySummary),
    LibraryPage(LibraryPage),
    Search(SearchResult),
    Recording(Option<Box<RecordingView>>),
    Transcript(TranscriptChunk),
    Compare(CompareChunk),
    Glossary(Vec<Term>),
    Topics(Vec<TopicSummary>),
    Topic(Option<Topic>),
    Calendar(Option<Calendar>),
    Outbound(Vec<OutboundRow>),
}

/// Version-stamped L3 query answer (`P2.md` §3.6.2).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct QueryReply {
    #[ts(type = "number")]
    pub version: u64,
    pub value: QueryValue,
}

struct ActiveSlot {
    generation: u64,
    cancel: CancellationToken,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
}

/// Optional deterministic test hook run inside the blocking query worker once the SQLite interrupt
/// handle has been registered.
pub type QueryHook = Arc<dyn Fn(&'static str, &CancellationToken) + Send + Sync>;

#[derive(Clone)]
struct ViewSource {
    shelf: Option<Arc<Shelf>>,
    index_path: Option<PathBuf>,
    tapes: PathBuf,
    has_soniox_key: bool,
    active_recording: Option<String>,
    active_refines: BTreeMap<String, (String, u32, u32)>,
    calendar: Option<Calendar>,
    ready: bool,
    hook: Option<QueryHook>,
}

struct EngineInner {
    tree: Tree,
    slots: Mutex<BTreeMap<QuerySlot, ActiveSlot>>,
    permits: Arc<Semaphore>,
    version: Arc<AtomicU64>,
    invalidated: Register<Invalidations>,
    source: Mutex<ViewSource>,
    query_done: Counter,
    query_cancelled: Counter,
    view_invalidated: Counter,
    query_ms: Spread,
}

/// Concurrent, cancellable L3 view query engine (`P2.md` §3.6.2).
#[derive(Clone)]
pub struct ViewEngine(Arc<EngineInner>);

impl ViewEngine {
    /// Creates a new `ViewEngine` running blocking queries on `tree` and publishing invalidations
    /// to the returned [`Reading<Invalidations>`].
    pub fn new(tree: Tree, tapes: PathBuf) -> (Self, Reading<Invalidations>) {
        let stats = tree.stats();
        let query_done = stats.counter("query.done");
        let query_cancelled = stats.counter("query.cancelled");
        let view_invalidated = stats.counter("view.invalidated");
        let query_ms = stats.spread("query.ms");
        let invalidated = Register::new(Invalidations::default());
        let reading = invalidated.read();
        let source = ViewSource {
            shelf: None,
            index_path: None,
            tapes,
            has_soniox_key: false,
            active_recording: None,
            active_refines: BTreeMap::new(),
            calendar: None,
            ready: false,
            hook: None,
        };
        let inner = EngineInner {
            tree,
            slots: Mutex::new(BTreeMap::new()),
            permits: Arc::new(Semaphore::new(QUERY_CONCURRENT_MAX_JOBS)),
            version: Arc::new(AtomicU64::new(0)),
            invalidated,
            source: Mutex::new(source),
            query_done,
            query_cancelled,
            view_invalidated,
            query_ms,
        };
        (Self(Arc::new(inner)), reading)
    }

    /// Updates the current shelf, SQLite index path, key availability, and readiness flag.
    pub fn set_source(
        &self,
        shelf: Option<Arc<Shelf>>,
        index_path: Option<PathBuf>,
        has_soniox_key: bool,
        ready: bool,
    ) {
        let mut src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        src.shelf = shelf;
        src.index_path = index_path;
        src.has_soniox_key = has_soniox_key;
        src.ready = ready;
    }

    /// Updates whether a Soniox key is currently available (affects `RecordingView.refine`).
    pub fn set_has_soniox_key(&self, has_soniox_key: bool) {
        let mut src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        src.has_soniox_key = has_soniox_key;
    }

    /// Updates the cached city calendar returned by [`Query::Calendar`].
    pub fn set_calendar(&self, calendar: Option<Calendar>) {
        let mut src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        src.calendar = calendar;
    }

    /// Reads the cached city calendar (used by `desk` when beginning a city filing).
    pub fn calendar(&self) -> Option<Calendar> {
        self.0.source.lock().unwrap_or_else(PoisonError::into_inner).calendar.clone()
    }

    /// Tracks the currently active live recording id so `Query::Transcript` reads take 0 directly
    /// from the live draft pack.
    pub fn set_active_recording(&self, id: Option<String>) {
        let mut src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        src.active_recording = id;
    }

    pub fn active_recording(&self) -> Option<String> {
        let src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        src.active_recording.clone()
    }

    /// Tracks active refinement progress for `id` (`None` clears it once refinement ends).
    pub fn set_active_refine(&self, id: String, progress: Option<(String, u32, u32)>) {
        let mut src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        match progress {
            Some(p) => {
                src.active_refines.insert(id, p);
            }
            None => {
                src.active_refines.remove(&id);
            }
        }
    }

    /// Installs an optional synchronization hook run inside the blocking worker (for deterministic
    /// cancellation tests).
    pub fn set_hook(&self, hook: Option<QueryHook>) {
        let mut src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner);
        src.hook = hook;
    }

    /// Current monotonic view version.
    pub fn version(&self) -> u64 {
        self.0.version.load(Ordering::Acquire)
    }

    /// Bumps the monotonic view version, records one coalesced invalidation, and emits
    /// `view.invalidate`.
    pub fn invalidate(&self, view: ViewKind, key: Option<String>) -> u64 {
        let version = self.0.version.fetch_add(1, Ordering::AcqRel) + 1;
        self.0.invalidated.change(|current| {
            let mut next = current.clone();
            next.push(view, key.clone(), version);
            Some(next)
        });
        self.0.view_invalidated.add(1);
        debug!("view.invalidate", view = view.code(), version = version);
        version
    }

    /// Invalidates every [`ViewKind`] with `key = None` (used on identity switch or full rebuild).
    pub fn invalidate_all(&self) -> u64 {
        let mut latest = self.version();
        for view in ViewKind::ALL {
            latest = self.invalidate(view, None);
        }
        latest
    }

    /// Executes `query` off-hub on the `work` subtree under `QUERY_CONCURRENT_MAX_JOBS`.
    /// Cancels and interrupts any earlier in-flight query occupying the same [`QuerySlot`].
    pub async fn query(&self, query: Query) -> Result<QueryReply, &'static str> {
        let started = Instant::now();
        let view_code = query.code();
        let ready = self.0.source.lock().unwrap_or_else(PoisonError::into_inner).ready;
        if !ready {
            let ms = started.elapsed().as_millis() as u64;
            warn!("query.done", view = view_code, code = "starting", ms = ms, rows = 0usize);
            return Err("starting");
        }

        let slot = query.slot();
        let (my_gen, cancel, interrupt) = self.claim_slot(slot);

        let _permit = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Err(self.finish_cancelled(slot, my_gen, view_code, started));
            }
            permit = self.0.permits.clone().acquire_owned() => {
                match permit {
                    Ok(p) => p,
                    Err(_) => return Err(self.finish_cancelled(slot, my_gen, view_code, started)),
                }
            }
        };

        if cancel.is_cancelled() {
            return Err(self.finish_cancelled(slot, my_gen, view_code, started));
        }

        // Load `version` (Acquire) *before* cloning `ViewSource`: writers always update `source`
        // (and commit SQLite) before bumping `version` (AcqRel) in `invalidate`, so reading
        // `version` first guarantees the cloned snapshot is at least as fresh as `version`.
        let version = self.0.version.load(Ordering::Acquire);
        let src = self.0.source.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if !src.ready {
            self.clear_slot(slot, my_gen);
            let ms = started.elapsed().as_millis() as u64;
            warn!("query.done", view = view_code, code = "starting", ms = ms, rows = 0usize);
            return Err("starting");
        }
        let run_cancel = cancel.clone();
        let run_interrupt = interrupt.clone();
        let outcome = self
            .0
            .tree
            .blocking("view.query", move || execute_blocking(query, src, run_cancel, run_interrupt))
            .await;

        let ms = started.elapsed().as_millis() as u64;
        if cancel.is_cancelled() {
            return Err(self.finish_cancelled(slot, my_gen, view_code, started));
        }
        self.clear_slot(slot, my_gen);

        match outcome {
            Ok(Ok((value, rows))) => {
                self.0.query_done.add(1);
                self.0.query_ms.note(ms);
                info!("query.done", view = view_code, code = "ok", ms = ms, rows = rows);
                if ms > QUERY_BUDGET_MS {
                    warn!("query.slow", view = view_code, ms = ms);
                }
                Ok(QueryReply { version, value })
            }
            Ok(Err("query.cancelled")) => {
                Err(self.finish_cancelled(slot, my_gen, view_code, started))
            }
            Ok(Err(code)) => {
                warn!("query.done", view = view_code, code = code, ms = ms, rows = 0usize);
                Err(code)
            }
            Err(_) => {
                if self.0.tree.is_cancelled() {
                    Err(self.finish_cancelled(slot, my_gen, view_code, started))
                } else {
                    warn!("query.done", view = view_code, code = "panic", ms = ms, rows = 0usize);
                    Err("panic")
                }
            }
        }
    }

    fn claim_slot(
        &self,
        slot: QuerySlot,
    ) -> (u64, CancellationToken, Arc<Mutex<Option<InterruptHandle>>>) {
        let cancel = CancellationToken::new();
        let interrupt = Arc::new(Mutex::new(None));
        let prev = {
            let mut map = self.0.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let next_gen = map.get(&slot).map_or(1, |s| s.generation.saturating_add(1));
            let old = map.insert(
                slot,
                ActiveSlot {
                    generation: next_gen,
                    cancel: cancel.clone(),
                    interrupt: interrupt.clone(),
                },
            );
            (next_gen, old)
        };
        if let Some(old) = prev.1 {
            old.cancel.cancel();
            if let Some(handle) =
                old.interrupt.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            {
                handle.interrupt();
            }
        }
        (prev.0, cancel, interrupt)
    }

    fn clear_slot(&self, slot: QuerySlot, generation: u64) {
        let mut map = self.0.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if map.get(&slot).is_some_and(|s| s.generation == generation) {
            map.remove(&slot);
        }
    }

    fn finish_cancelled(
        &self,
        slot: QuerySlot,
        generation: u64,
        view_code: &'static str,
        started: Instant,
    ) -> &'static str {
        self.clear_slot(slot, generation);
        let ms = started.elapsed().as_millis() as u64;
        self.0.query_cancelled.add(1);
        warn!("query.done", view = view_code, code = "cancelled", ms = ms, rows = 0usize);
        "query.cancelled"
    }
}

fn open_reader(
    src: &ViewSource,
    cancel: &CancellationToken,
    interrupt: &Mutex<Option<InterruptHandle>>,
) -> Result<Option<Reader>, &'static str> {
    let Some(ref path) = src.index_path else {
        return Ok(None);
    };
    if cancel.is_cancelled() {
        return Err("query.cancelled");
    }
    let reader = Reader::open(path)?;
    let handle = reader.interrupt_handle();
    *interrupt.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    if cancel.is_cancelled() {
        if let Some(h) = interrupt.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            h.interrupt();
        }
        return Err("query.cancelled");
    }
    Ok(Some(reader))
}

fn execute_blocking(
    query: Query,
    src: ViewSource,
    cancel: CancellationToken,
    interrupt: Arc<Mutex<Option<InterruptHandle>>>,
) -> Result<(QueryValue, usize), &'static str> {
    let view_code = query.code();
    if cancel.is_cancelled() {
        return Err("query.cancelled");
    }

    let needs_reader = matches!(
        query,
        Query::LibrarySummary { .. }
            | Query::LibraryMonth { .. }
            | Query::LibraryRecent { .. }
            | Query::Search { .. }
            | Query::Transcript { .. }
            | Query::Compare { .. }
            | Query::Outbound
    );
    let reader = if needs_reader { open_reader(&src, &cancel, &interrupt)? } else { None };

    if let Some(ref hook) = src.hook {
        hook(view_code, &cancel);
    }
    if cancel.is_cancelled() {
        return Err("query.cancelled");
    }

    let result = match query {
        Query::LibrarySummary { tz_offset_min } => {
            let summary = match reader {
                Some(r) => r.library_summary(tz_offset_min)?,
                None => LibrarySummary::default(),
            };
            let rows = summary.months.len();
            Ok((QueryValue::LibrarySummary(summary), rows))
        }
        Query::LibraryMonth { month, tz_offset_min, filter, limit, offset } => {
            let page = match reader {
                Some(r) => r.library_month(&month, tz_offset_min, &filter, limit, offset)?,
                None => LibraryPage::default(),
            };
            let rows = page.rows.len();
            Ok((QueryValue::LibraryPage(page), rows))
        }
        Query::LibraryRecent { filter, limit, offset } => {
            let page = match reader {
                Some(r) => r.library_recent(&filter, limit, offset)?,
                None => LibraryPage::default(),
            };
            let rows = page.rows.len();
            Ok((QueryValue::LibraryPage(page), rows))
        }
        Query::Search { query: q, limit } => {
            let res = match reader {
                Some(r) => r.search(&q, limit)?,
                None => SearchResult::default(),
            };
            let rows = res.hits.len();
            Ok((QueryValue::Search(res), rows))
        }
        Query::Recording { id } => {
            let Some(ref shelf) = src.shelf else {
                return Ok((QueryValue::Recording(None), 0));
            };
            let Some(rec) = shelf.about(&id).map_err(|_| "shelf")? else {
                return Ok((QueryValue::Recording(None), 0));
            };
            let takes_list = takes::list(shelf, &id, rec.about.take).map_err(|_| "shelf")?;
            let filed = shelf
                .kept::<Filing>(FILING)
                .map_err(|_| "shelf")?
                .into_iter()
                .find_map(|(fid, f)| (fid == id).then_some(f));
            let tape_dir = src.tapes.join(&id);
            let audio = tape::kept(&tape_dir);
            let failure = takes::read_failure(&tape_dir);
            let refine = match src.active_refines.get(&id) {
                Some((step, done, of)) => {
                    RefineStatus::Refining { step: step.clone(), done: *done, of: *of }
                }
                None => takes::initial_refine_status(
                    rec.about.take,
                    audio,
                    src.has_soniox_key,
                    failure.as_ref(),
                ),
            };
            let row = LibraryRow {
                id: rec.id.clone(),
                started: rec.about.started,
                length_ms: rec.about.length_ms,
                title: rec.about.title.clone(),
                preview: rec.about.preview.clone(),
                spoken: rec.about.spoken.clone(),
                draft: rec.about.take == 0,
                audio,
                filed: filed.as_ref().is_some_and(|f| f.state == "city.filed"),
                refine_failed: failure.map(|f| f.code),
            };
            let view = RecordingView {
                row,
                shown: rec.about.shown,
                source: rec.about.source,
                take: rec.about.take,
                takes: takes_list,
                speakers: rec.speakers,
                notes: rec.notes,
                note: rec.note,
                filed,
                refine,
            };
            Ok((QueryValue::Recording(Some(Box::new(view))), 1))
        }
        Query::Transcript { id, take, chunk } => {
            let target_take = match take {
                Some(t) => t,
                None => src
                    .shelf
                    .as_ref()
                    .and_then(|s| s.about(&id).ok().flatten())
                    .map_or(0, |r| r.about.take),
            };
            let is_live_draft =
                src.active_recording.as_deref() == Some(id.as_str()) && target_take == 0;
            let reply = if is_live_draft {
                if let Some(ref shelf) = src.shelf {
                    let all = takes::read(shelf, &id, 0).map_err(|_| "shelf")?;
                    slice_chunk(&id, 0, &all, chunk)
                } else {
                    slice_chunk(&id, 0, &[], chunk)
                }
            } else if let Some(r) = reader {
                let total_lines = r.take_line_count(&id, target_take)?;
                let total_chunks = transcript::total_chunks(total_lines);
                let offset = chunk.saturating_mul(TRANSCRIPT_CHUNK_LINES as u32);
                let lines =
                    r.take_lines_slice(&id, target_take, offset, TRANSCRIPT_CHUNK_LINES as u32)?;
                TranscriptChunk { id, take: target_take, chunk, total_lines, total_chunks, lines }
            } else {
                slice_chunk(&id, target_take, &[], chunk)
            };
            let rows = reply.lines.len();
            Ok((QueryValue::Transcript(reply), rows))
        }
        Query::Compare { id, take_a, take_b, chunk } => {
            let reply = if let Some(r) = reader {
                let total_b = r.take_line_count(&id, take_b)?;
                let offset = chunk.saturating_mul(TRANSCRIPT_CHUNK_LINES as u32);
                let lines_b =
                    r.take_lines_slice(&id, take_b, offset, TRANSCRIPT_CHUNK_LINES as u32)?;
                let lines_a = if lines_b.is_empty() {
                    if chunk == 0 {
                        r.take_lines_slice(&id, take_a, 0, TRANSCRIPT_CHUNK_LINES as u32)?
                    } else {
                        Vec::new()
                    }
                } else {
                    let has_time = lines_b.iter().any(|(_, s)| s.line.t0 > 0 || s.line.t1 > 0);
                    if has_time {
                        let min_t0 = lines_b.iter().map(|(_, s)| s.line.t0).min().unwrap_or(0);
                        let max_t1 = lines_b.iter().map(|(_, s)| s.line.t1).max().unwrap_or(0);
                        r.take_lines_in_time(&id, take_a, min_t0, max_t1)?
                    } else {
                        r.take_lines_slice(&id, take_a, offset, TRANSCRIPT_CHUNK_LINES as u32)?
                    }
                };
                let rows = compare_lines(&lines_a, &lines_b);
                let total_chunks =
                    transcript::total_chunks(total_b).max(u32::from(!rows.is_empty()));
                CompareChunk { id, take_a, take_b, chunk, total_chunks, rows }
            } else {
                CompareChunk { id, take_a, take_b, chunk, total_chunks: 0, rows: Vec::new() }
            };
            let rows = reply.rows.len();
            Ok((QueryValue::Compare(reply), rows))
        }
        Query::Glossary => {
            let mut terms = match &src.shelf {
                Some(shelf) => {
                    topic::get(shelf, ALWAYS_TOPIC_ID)?.map(|t| t.terms).unwrap_or_default()
                }
                None => Vec::new(),
            };
            terms.sort_by(|a, b| a.said.cmp(&b.said));
            let rows = terms.len();
            Ok((QueryValue::Glossary(terms), rows))
        }
        Query::Topics => {
            let list = match &src.shelf {
                Some(shelf) => topic::summaries(shelf)?,
                None => Vec::new(),
            };
            let rows = list.len();
            Ok((QueryValue::Topics(list), rows))
        }
        Query::Topic { id } => {
            let found = match &src.shelf {
                Some(shelf) => topic::get(shelf, &id)?,
                None => None,
            };
            let rows = usize::from(found.is_some());
            Ok((QueryValue::Topic(found), rows))
        }
        Query::Calendar => {
            let rows = src.calendar.as_ref().map_or(0, |c| c.events.len());
            Ok((QueryValue::Calendar(src.calendar), rows))
        }
        Query::Outbound => {
            let Some(ref shelf) = src.shelf else {
                return Ok((QueryValue::Outbound(Vec::new()), 0));
            };
            let filings = shelf.kept::<Filing>(FILING).map_err(|_| "shelf")?;
            let mut rows = Vec::with_capacity(filings.len());
            for (id, filing) in filings {
                let (title, started) = if let Some(Some(r)) =
                    reader.as_ref().map(|rd| rd.recording_row(&id)).transpose()?
                {
                    (r.title, r.started)
                } else if let Some(rec) = shelf.about(&id).map_err(|_| "shelf")? {
                    (rec.about.title, rec.about.started)
                } else {
                    (String::new(), 0)
                };
                rows.push(OutboundRow { id, title, started, filing });
            }
            rows.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| a.id.cmp(&b.id)));
            let n = rows.len();
            Ok((QueryValue::Outbound(rows), n))
        }
    };

    if cancel.is_cancelled() {
        return Err("query.cancelled");
    }
    result
}
