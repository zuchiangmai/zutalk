//! Disposable local SQLite + FTS5 projection index (`P2.md` §3.4).
//!
//! Zu packs under `<data>/records/<did>/` remain the sole source of truth (Principle 1). This
//! index is an accelerator that can be deleted or corrupted at any time and will automatically
//! rebuild from `Shelf` one recording at a time (`O(1)` peak memory, I3).

use crate::capture::clock::wall_ms;
use crate::capture::tape;
use crate::live::lang::profile;
use crate::live::lines::Line;
use crate::out::city::Filing;
use crate::record::shelf::{FILING, LINE, RECORDING, Said, Shelf};
use crate::record::takes::{self, RefineFailure};
use crate::rt::stats::Stats;
use rusqlite::{Connection, InterruptHandle, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};
use ts_rs::TS;
use zutalk_log::{debug, info, warn};

/// Schema version stored in `PRAGMA user_version`; bump to trigger automatic rebuild.
pub const INDEX_SCHEMA_VERSION: u32 = 1;
/// Short WAL busy wait for concurrent readers and the single writer (`P2.md` §3.4).
pub const INDEX_BUSY_TIMEOUT_MS: u64 = 200;
/// Default page size for monthly and recent library queries (`P2.md` §3.4).
pub const LIBRARY_PAGE_DEFAULT_ROWS: u32 = 50;
/// Hard ceiling on rows returned by any single library page query (`P2.md` §3.4, I3).
pub const LIBRARY_PAGE_MAX_ROWS: u32 = 200;
/// Default number of hits returned by a full-text search (`P2.md` §3.4).
pub const SEARCH_DEFAULT_HITS: u32 = 100;
/// Hard ceiling on hits returned by a single search query (`P2.md` §3.4, I3).
pub const SEARCH_MAX_HITS: u32 = 200;
/// Maximum UTF-8 byte length kept for each stored search snippet (`P2.md` §3.4).
pub const SEARCH_SNIPPET_MAX_BYTES: usize = 240;

const SCHEMA_SQL: &str = r#"
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE recordings (
    id TEXT PRIMARY KEY,
    started INTEGER NOT NULL,
    length_ms INTEGER NOT NULL,
    title TEXT NOT NULL,
    preview TEXT NOT NULL,
    spoken TEXT NOT NULL,
    shown TEXT NOT NULL,
    source TEXT NOT NULL,
    take INTEGER NOT NULL,
    moved INTEGER NOT NULL,
    audio INTEGER NOT NULL,
    filed_state TEXT NOT NULL,
    refine_error TEXT,
    refine_error_at INTEGER,
    notes_count INTEGER NOT NULL,
    about_sig TEXT NOT NULL,
    lines_sig TEXT NOT NULL
);

CREATE INDEX idx_recordings_started ON recordings(started DESC, id);

CREATE TABLE take_lines (
    recording_id TEXT NOT NULL,
    take INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    line_key TEXT NOT NULL,
    t0 INTEGER NOT NULL,
    t1 INTEGER NOT NULL,
    speaker TEXT NOT NULL,
    language TEXT NOT NULL,
    text TEXT NOT NULL,
    translations_json TEXT NOT NULL,
    via_json TEXT NOT NULL,
    incomplete INTEGER NOT NULL,
    PRIMARY KEY (recording_id, take, seq)
) WITHOUT ROWID;

CREATE INDEX idx_take_lines_time ON take_lines(recording_id, take, t0, t1);

CREATE TABLE search_docs (
    doc_id INTEGER PRIMARY KEY,
    recording_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    item_key TEXT NOT NULL,
    lang TEXT NOT NULL,
    norm_text TEXT NOT NULL,
    snippet TEXT NOT NULL
);

CREATE INDEX idx_search_docs_rec ON search_docs(recording_id);

CREATE VIRTUAL TABLE search_fts USING fts5(
    norm_text,
    content='search_docs',
    content_rowid='doc_id',
    tokenize='trigram'
);

CREATE TRIGGER search_docs_ai AFTER INSERT ON search_docs BEGIN
    INSERT INTO search_fts(rowid, norm_text) VALUES (new.doc_id, new.norm_text);
END;

CREATE TRIGGER search_docs_ad AFTER DELETE ON search_docs BEGIN
    INSERT INTO search_fts(search_fts, rowid, norm_text) VALUES('delete', old.doc_id, old.norm_text);
END;

CREATE TRIGGER search_docs_au AFTER UPDATE ON search_docs BEGIN
    INSERT INTO search_fts(search_fts, rowid, norm_text) VALUES('delete', old.doc_id, old.norm_text);
    INSERT INTO search_fts(rowid, norm_text) VALUES (new.doc_id, new.norm_text);
END;
"#;

/// Probed health state of an on-disk SQLite index before opening (`P2.md` §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexHealth {
    Ready,
    Missing,
    VersionMismatch,
    OwnerMismatch,
    Corrupt,
}

/// Pure decision produced by [`open_plan`] from [`IndexHealth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPlan {
    Reuse,
    Rebuild(&'static str),
}

/// Pure transition decision for opening an on-disk SQLite projection index (`P2.md` §3.4).
pub fn open_plan(health: IndexHealth) -> OpenPlan {
    match health {
        IndexHealth::Ready => OpenPlan::Reuse,
        IndexHealth::Missing => OpenPlan::Rebuild("missing"),
        IndexHealth::VersionMismatch => OpenPlan::Rebuild("version"),
        IndexHealth::OwnerMismatch => OpenPlan::Rebuild("owner"),
        IndexHealth::Corrupt => OpenPlan::Rebuild("corrupt"),
    }
}

/// One recording row projected for library lists and detail headers (`P2.md` §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LibraryRow {
    pub id: String,
    #[ts(type = "number")]
    pub started: i64,
    pub length_ms: u32,
    pub title: String,
    pub preview: String,
    pub spoken: Vec<String>,
    pub draft: bool,
    pub audio: bool,
    pub filed: bool,
    pub refine_failed: Option<String>,
}

/// Recording count for one local calendar month (`YYYY-MM`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MonthCount {
    pub month: String,
    pub count: u32,
}

/// Total recording count plus monthly breakdown, newest month first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LibrarySummary {
    pub total: u32,
    pub months: Vec<MonthCount>,
}

/// Optional filters applied to monthly and recent library queries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct LibraryFilter {
    pub draft_only: bool,
    pub audio_only: bool,
    pub filed_only: bool,
    pub language: Option<String>,
}

/// One bounded page of library rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LibraryPage {
    pub rows: Vec<LibraryRow>,
    pub total: u32,
    pub has_more: bool,
}

/// Where a search hit matched within a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum SearchHitKind {
    Title,
    Line,
    Translation,
    Note,
}

/// One bounded search hit pointing to a recording and matching item key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchHit {
    pub recording_id: String,
    pub kind: SearchHitKind,
    pub key: String,
    pub lang: String,
    pub snippet: String,
}

/// Bounded full-text search reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
}

/// Read-only SQLite connection used by concurrent L3 queries (`P2.md` §3.4).
pub struct Reader(Connection);

impl Reader {
    /// Opens an existing projection index read-only for concurrent WAL snapshots.
    pub fn open(path: &Path) -> Result<Self, &'static str> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(|_| "index")?;
        conn.busy_timeout(Duration::from_millis(INDEX_BUSY_TIMEOUT_MS)).map_err(|_| "index")?;
        Ok(Self(conn))
    }

    /// Returns a handle that cancels an in-flight SQLite query on another thread.
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.0.get_interrupt_handle()
    }

    /// Monthly recording counts in the caller's timezone (`tz_offset_min`, positive east of UTC).
    pub fn library_summary(&self, tz_offset_min: i32) -> Result<LibrarySummary, &'static str> {
        query_library_summary(&self.0, tz_offset_min)
    }

    /// One bounded page of recordings started in `month` (`YYYY-MM`, local to `tz_offset_min`).
    pub fn library_month(
        &self,
        month: &str,
        tz_offset_min: i32,
        filter: &LibraryFilter,
        limit: u32,
        offset: u32,
    ) -> Result<LibraryPage, &'static str> {
        query_library_month(&self.0, month, tz_offset_min, filter, limit, offset)
    }

    /// One bounded page of the most recent recordings across all months.
    pub fn library_recent(
        &self,
        filter: &LibraryFilter,
        limit: u32,
        offset: u32,
    ) -> Result<LibraryPage, &'static str> {
        query_library_recent(&self.0, filter, limit, offset)
    }

    /// Full-text search across titles, current-take lines, translations, and notes.
    pub fn search(&self, query: &str, limit: u32) -> Result<SearchResult, &'static str> {
        query_search(&self.0, query, limit)
    }

    /// Total number of indexed lines for `(id, take)`.
    pub fn take_line_count(&self, id: &str, take: u32) -> Result<u32, &'static str> {
        query_take_line_count(&self.0, id, take)
    }

    /// Fetches a bounded slice of lines for `(id, take)` ordered by `seq`.
    pub fn take_lines_slice(
        &self,
        id: &str,
        take: u32,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<(String, Said)>, &'static str> {
        query_take_lines_slice(&self.0, id, take, offset, limit)
    }

    /// Fetches lines of `(id, take)` overlapping `[t0_ms, t1_ms]` using `idx_take_lines_time`.
    pub fn take_lines_in_time(
        &self,
        id: &str,
        take: u32,
        t0_ms: u32,
        t1_ms: u32,
    ) -> Result<Vec<(String, Said)>, &'static str> {
        query_take_lines_in_time(&self.0, id, take, t0_ms, t1_ms)
    }

    /// Looks up one recording's projected summary row.
    pub fn recording_row(&self, id: &str) -> Result<Option<LibraryRow>, &'static str> {
        query_recording_row(&self.0, id)
    }
}

/// Single-writer SQLite projection index held by `desk` (`P2.md` §3.4).
pub struct Index {
    path: PathBuf,
    conn: Connection,
}

impl Index {
    /// Opens or rebuilds the SQLite projection at `path` for `shelf`, reconciling any offline
    /// pack changes when reusing an existing healthy database.
    pub fn open(
        path: &Path,
        shelf: &Shelf,
        tapes: &Path,
        stats: &Stats,
    ) -> Result<Self, &'static str> {
        let started = Instant::now();
        let rebuilds = stats.counter("index.rebuilds");
        let rebuild_ms = stats.spread("index.rebuild_ms");

        let health = probe(path, shelf.by());
        match open_plan(health) {
            OpenPlan::Reuse => match Self::open_existing(path, shelf, tapes) {
                Ok((index, rows)) => {
                    let ms = started.elapsed().as_millis() as u64;
                    info!("index.open", mode = "existing", why = "ok", rows = rows, ms = ms);
                    Ok(index)
                }
                Err(_) => {
                    // Existing DB broke during reconcile (e.g. corrupt page): fall back to rebuild.
                    let (index, rows) = Self::build_fresh(path, shelf, tapes)?;
                    let ms = started.elapsed().as_millis() as u64;
                    rebuilds.add(1);
                    rebuild_ms.note(ms);
                    warn!("index.open", mode = "rebuilt", why = "corrupt", rows = rows, ms = ms);
                    Ok(index)
                }
            },
            OpenPlan::Rebuild(why) => {
                let (index, rows) = Self::build_fresh(path, shelf, tapes)?;
                let ms = started.elapsed().as_millis() as u64;
                rebuilds.add(1);
                rebuild_ms.note(ms);
                if why == "corrupt" {
                    warn!("index.open", mode = "rebuilt", why = why, rows = rows, ms = ms);
                } else {
                    info!("index.open", mode = "rebuilt", why = why, rows = rows, ms = ms);
                }
                Ok(index)
            }
        }
    }

    /// Path of the underlying SQLite database file (for opening concurrent [`Reader`]s).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open_existing(
        path: &Path,
        shelf: &Shelf,
        tapes: &Path,
    ) -> Result<(Self, usize), &'static str> {
        let conn = open_rw(path)?;
        let mut index = Self { path: path.to_path_buf(), conn };
        let rows = index.reconcile(shelf, tapes)?;
        Ok((index, rows))
    }

    fn build_fresh(
        path: &Path,
        shelf: &Shelf,
        tapes: &Path,
    ) -> Result<(Self, usize), &'static str> {
        remove_db_files(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| "index")?;
        }
        let conn = open_rw(path)?;
        conn.execute_batch(SCHEMA_SQL).map_err(|_| "index")?;
        conn.pragma_update(None, "user_version", INDEX_SCHEMA_VERSION).map_err(|_| "index")?;
        let mut index = Self { path: path.to_path_buf(), conn };
        let rows = index.rebuild(shelf, tapes)?;
        Ok((index, rows))
    }

    /// Rebuilds the entire index from `shelf` one recording at a time (`O(1)` memory, I3).
    pub fn rebuild(&mut self, shelf: &Shelf, tapes: &Path) -> Result<usize, &'static str> {
        let filings = load_filings(shelf);
        let ids = recording_dirs(shelf);
        let tx = self.conn.transaction().map_err(|_| "index")?;
        tx.execute("DELETE FROM search_docs", []).map_err(|_| "index")?;
        tx.execute("DELETE FROM take_lines", []).map_err(|_| "index")?;
        tx.execute("DELETE FROM recordings", []).map_err(|_| "index")?;
        tx.execute("DELETE FROM meta", []).map_err(|_| "index")?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('by', ?1), ('rebuilt_at', ?2)",
            params![shelf.by(), wall_ms().to_string()],
        )
        .map_err(|_| "index")?;

        let mut rows = 0usize;
        for id in ids {
            let filed_state = filings.get(&id).map(String::as_str).unwrap_or("");
            if upsert_recording_tx(&tx, shelf, tapes, &id, filed_state)? {
                rows += 1;
            }
        }
        tx.commit().map_err(|_| "index")?;
        Ok(rows)
    }

    /// Reconciles an existing index with disk packs, re-reading only added, removed, or modified
    /// recording directories.
    fn reconcile(&mut self, shelf: &Shelf, tapes: &Path) -> Result<usize, &'static str> {
        let filings = load_filings(shelf);
        let disk_ids = recording_dirs(shelf);
        let tx = self.conn.transaction().map_err(|_| "index")?;

        let mut existing: HashMap<String, StoredSig> = HashMap::new();
        {
            let mut stmt = tx
                .prepare(
                    "SELECT id, about_sig, lines_sig, audio, filed_state, refine_error, refine_error_at \
                     FROM recordings",
                )
                .map_err(|_| "index")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        StoredSig {
                            about_sig: r.get(1)?,
                            lines_sig: r.get(2)?,
                            audio: r.get::<_, i64>(3)? != 0,
                            filed_state: r.get(4)?,
                            refine_error: r.get(5)?,
                            refine_error_at: r.get(6)?,
                        },
                    ))
                })
                .map_err(|_| "index")?;
            for row in rows {
                let (id, sig) = row.map_err(|_| "index")?;
                existing.insert(id, sig);
            }
        }

        for id in &disk_ids {
            let about_sig = file_sig(&shelf.pack_path(id, RECORDING));
            let lines_sig = file_sig(&shelf.pack_path(id, LINE));
            let audio = tape::kept(&tapes.join(id));
            let filed_state = filings.get(id).map(String::as_str).unwrap_or("");
            let failure = takes::read_failure(&tapes.join(id));
            let err_code = failure.as_ref().map(|f| f.code.as_str());
            let err_at = failure.as_ref().map(|f| f.at);

            match existing.remove(id) {
                Some(prev) if prev.about_sig == about_sig && prev.lines_sig == lines_sig => {
                    if prev.audio != audio
                        || prev.filed_state != filed_state
                        || prev.refine_error.as_deref() != err_code
                        || prev.refine_error_at != err_at
                    {
                        tx.execute(
                            "UPDATE recordings SET audio = ?1, filed_state = ?2, \
                             refine_error = ?3, refine_error_at = ?4 WHERE id = ?5",
                            params![i64::from(audio), filed_state, err_code, err_at, id],
                        )
                        .map_err(|_| "index")?;
                    }
                }
                _ => {
                    let _ = upsert_recording_tx(&tx, shelf, tapes, id, filed_state)?; // dropped: boolean only counts active recordings during rebuild
                }
            }
        }

        for gone in existing.keys() {
            remove_recording_tx(&tx, gone)?;
        }

        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM recordings", [], |r| r.get(0))
            .map_err(|_| "index")?;
        tx.commit().map_err(|_| "index")?;
        Ok(count.max(0) as usize)
    }

    /// Incrementally re-indexes one recording from `shelf` + `tapes` in a single transaction, or
    /// removes it if it has been tombstoned on `shelf`.
    pub fn upsert_recording(
        &mut self,
        shelf: &Shelf,
        tapes: &Path,
        id: &str,
    ) -> Result<(), &'static str> {
        let started = Instant::now();
        let res = (|| -> Result<(), &'static str> {
            let filings = load_filings(shelf);
            let filed_state = filings.get(id).map(String::as_str).unwrap_or("");
            let tx = self.conn.transaction().map_err(|_| "index")?;
            let _ = upsert_recording_tx(&tx, shelf, tapes, id, filed_state)?; // dropped: upsert handles removal when recording is gone
            tx.commit().map_err(|_| "index")?;
            Ok(())
        })();
        log_sync("upsert", res, started);
        res
    }

    /// Removes one recording from all projection tables in a single transaction.
    pub fn remove_recording(&mut self, id: &str) -> Result<(), &'static str> {
        let started = Instant::now();
        let res = (|| -> Result<(), &'static str> {
            let tx = self.conn.transaction().map_err(|_| "index")?;
            remove_recording_tx(&tx, id)?;
            tx.commit().map_err(|_| "index")?;
            Ok(())
        })();
        log_sync("remove", res, started);
        res
    }

    /// Updates only the `audio` flag of one recording.
    pub fn update_audio(&mut self, id: &str, audio: bool) -> Result<(), &'static str> {
        let started = Instant::now();
        let res = self
            .conn
            .execute(
                "UPDATE recordings SET audio = ?1 WHERE id = ?2",
                params![i64::from(audio), id],
            )
            .map(|_| ())
            .map_err(|_| "index");
        log_sync("audio", res, started);
        res
    }

    /// Updates only the `filed_state` of one recording.
    pub fn update_filing(&mut self, id: &str, state: &str) -> Result<(), &'static str> {
        let started = Instant::now();
        let res = self
            .conn
            .execute("UPDATE recordings SET filed_state = ?1 WHERE id = ?2", params![state, id])
            .map(|_| ())
            .map_err(|_| "index");
        log_sync("filing", res, started);
        res
    }

    /// Updates or clears the persisted refinement failure metadata of one recording.
    pub fn update_refine_error(
        &mut self,
        id: &str,
        failure: Option<&RefineFailure>,
    ) -> Result<(), &'static str> {
        let started = Instant::now();
        let code = failure.map(|f| f.code.as_str());
        let at = failure.map(|f| f.at);
        let res = self
            .conn
            .execute(
                "UPDATE recordings SET refine_error = ?1, refine_error_at = ?2 WHERE id = ?3",
                params![code, at, id],
            )
            .map(|_| ())
            .map_err(|_| "index");
        log_sync("refine", res, started);
        res
    }

    pub fn library_summary(&self, tz_offset_min: i32) -> Result<LibrarySummary, &'static str> {
        query_library_summary(&self.conn, tz_offset_min)
    }

    pub fn library_month(
        &self,
        month: &str,
        tz_offset_min: i32,
        filter: &LibraryFilter,
        limit: u32,
        offset: u32,
    ) -> Result<LibraryPage, &'static str> {
        query_library_month(&self.conn, month, tz_offset_min, filter, limit, offset)
    }

    pub fn library_recent(
        &self,
        filter: &LibraryFilter,
        limit: u32,
        offset: u32,
    ) -> Result<LibraryPage, &'static str> {
        query_library_recent(&self.conn, filter, limit, offset)
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<SearchResult, &'static str> {
        query_search(&self.conn, query, limit)
    }

    pub fn take_line_count(&self, id: &str, take: u32) -> Result<u32, &'static str> {
        query_take_line_count(&self.conn, id, take)
    }

    pub fn take_lines_slice(
        &self,
        id: &str,
        take: u32,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<(String, Said)>, &'static str> {
        query_take_lines_slice(&self.conn, id, take, offset, limit)
    }

    pub fn take_lines_in_time(
        &self,
        id: &str,
        take: u32,
        t0_ms: u32,
        t1_ms: u32,
    ) -> Result<Vec<(String, Said)>, &'static str> {
        query_take_lines_in_time(&self.conn, id, take, t0_ms, t1_ms)
    }

    pub fn recording_row(&self, id: &str) -> Result<Option<LibraryRow>, &'static str> {
        query_recording_row(&self.conn, id)
    }
}

struct StoredSig {
    about_sig: String,
    lines_sig: String,
    audio: bool,
    filed_state: String,
    refine_error: Option<String>,
    refine_error_at: Option<i64>,
}

fn log_sync(op: &'static str, res: Result<(), &'static str>, started: Instant) {
    let ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(()) => debug!("index.sync", op = op, code = "ok", ms = ms),
        Err(code) => warn!("index.sync", op = op, code = code, ms = ms),
    }
}

fn open_rw(path: &Path) -> Result<Connection, &'static str> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|_| "index")?;
    conn.busy_timeout(Duration::from_millis(INDEX_BUSY_TIMEOUT_MS)).map_err(|_| "index")?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY;",
    )
    .map_err(|_| "index")?;
    Ok(conn)
}

fn probe(path: &Path, expected_by: &str) -> IndexHealth {
    if !path.exists() {
        return IndexHealth::Missing;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = Connection::open_with_flags(path, flags) else {
        return IndexHealth::Corrupt;
    };
    if conn.busy_timeout(Duration::from_millis(INDEX_BUSY_TIMEOUT_MS)).is_err() {
        return IndexHealth::Corrupt;
    }
    let check: Result<String, _> = conn.query_row("PRAGMA quick_check", [], |r| r.get(0));
    if check.as_deref() != Ok("ok") {
        return IndexHealth::Corrupt;
    }
    let version: Result<u32, _> = conn.query_row("PRAGMA user_version", [], |r| r.get(0));
    match version {
        Ok(v) if v != INDEX_SCHEMA_VERSION => return IndexHealth::VersionMismatch,
        Ok(_) => {}
        Err(_) => return IndexHealth::Corrupt,
    }
    let by: Result<Option<String>, _> =
        conn.query_row("SELECT value FROM meta WHERE key = 'by'", [], |r| r.get(0)).optional();
    match by {
        Ok(Some(owner)) if owner == expected_by => {}
        Ok(Some(_)) => return IndexHealth::OwnerMismatch,
        Ok(None) | Err(_) => return IndexHealth::Corrupt,
    }
    let tables: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
         ('meta', 'recordings', 'take_lines', 'search_docs', 'search_fts')",
        [],
        |r| r.get(0),
    );
    if tables != Ok(5) {
        return IndexHealth::Corrupt;
    }
    IndexHealth::Ready
}

fn remove_db_files(path: &Path) {
    let base = path.as_os_str();
    let mut wal = base.to_os_string();
    wal.push("-wal");
    let mut shm = base.to_os_string();
    shm.push("-shm");
    for file in [path.to_path_buf(), PathBuf::from(wal), PathBuf::from(shm)] {
        let _ = fs::remove_file(file); // dropped: absent WAL/SHM or missing DB is expected before fresh creation
    }
}

fn file_sig(path: &Path) -> String {
    let Ok(meta) = fs::metadata(path) else {
        return "0:0".to_owned();
    };
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{size}:{mtime_ns}")
}

fn recording_dirs(shelf: &Shelf) -> Vec<String> {
    let entries = match fs::read_dir(shelf.root()) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    let mut ids = Vec::new();
    for entry in entries.flatten() {
        if let (Ok(ft), Some(name)) = (entry.file_type(), entry.file_name().to_str())
            && ft.is_dir()
        {
            ids.push(name.to_owned());
        }
    }
    ids.sort();
    ids
}

fn load_filings(shelf: &Shelf) -> HashMap<String, String> {
    shelf
        .kept::<Filing>(FILING)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, f)| (id, f.state))
        .collect()
}

fn remove_recording_tx(tx: &Transaction<'_>, id: &str) -> Result<(), &'static str> {
    tx.execute("DELETE FROM search_docs WHERE recording_id = ?1", params![id])
        .map_err(|_| "index")?;
    tx.execute("DELETE FROM take_lines WHERE recording_id = ?1", params![id])
        .map_err(|_| "index")?;
    tx.execute("DELETE FROM recordings WHERE id = ?1", params![id]).map_err(|_| "index")?;
    Ok(())
}

fn upsert_recording_tx(
    tx: &Transaction<'_>,
    shelf: &Shelf,
    tapes: &Path,
    id: &str,
    filed_state: &str,
) -> Result<bool, &'static str> {
    let about = shelf.about(id).map_err(|_| "index")?;
    let Some(rec) = about else {
        remove_recording_tx(tx, id)?;
        return Ok(false);
    };

    remove_recording_tx(tx, id)?;

    let about_sig = file_sig(&shelf.pack_path(id, RECORDING));
    let lines_sig = file_sig(&shelf.pack_path(id, LINE));
    let audio = tape::kept(&tapes.join(id));
    let failure = takes::read_failure(&tapes.join(id));
    let spoken_json = serde_json::to_string(&rec.about.spoken).map_err(|_| "index")?;
    let shown_json = serde_json::to_string(&rec.about.shown).map_err(|_| "index")?;

    tx.execute(
        "INSERT INTO recordings (\
            id, started, length_ms, title, preview, spoken, shown, source, take, moved, audio, \
            filed_state, refine_error, refine_error_at, notes_count, about_sig, lines_sig\
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            id,
            rec.about.started,
            i64::from(rec.about.length_ms),
            &rec.about.title,
            &rec.about.preview,
            spoken_json,
            shown_json,
            &rec.about.source,
            i64::from(rec.about.take),
            i64::from(rec.about.moved),
            i64::from(audio),
            filed_state,
            failure.as_ref().map(|f| f.code.as_str()),
            failure.as_ref().map(|f| f.at),
            rec.notes.len() as i64,
            about_sig,
            lines_sig,
        ],
    )
    .map_err(|_| "index")?;

    let mut line_stmt = tx
        .prepare_cached(
            "INSERT INTO take_lines (\
                recording_id, take, seq, line_key, t0, t1, speaker, language, text, \
                translations_json, via_json, incomplete\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .map_err(|_| "index")?;
    let mut doc_stmt = tx
        .prepare_cached(
            "INSERT INTO search_docs (recording_id, kind, item_key, lang, norm_text, snippet) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(|_| "index")?;

    // Index title if present.
    if !rec.about.title.trim().is_empty() {
        let norm = profile("").key(&rec.about.title);
        let snippet = truncate_utf8(&rec.about.title, SEARCH_SNIPPET_MAX_BYTES);
        doc_stmt.execute(params![id, "title", id, "", norm, snippet]).map_err(|_| "index")?;
    }

    // Index all takes into `take_lines`, and current take's spoken + translated text into `search_docs`.
    let take_list = takes::list(shelf, id, rec.about.take).map_err(|_| "index")?;
    for info in take_list {
        let lines = takes::read(shelf, id, info.take).map_err(|_| "index")?;
        for (seq, (line_key, said)) in lines.into_iter().enumerate() {
            let tr_json = serde_json::to_string(&said.line.translations).map_err(|_| "index")?;
            let via_json = serde_json::to_string(&said.via).map_err(|_| "index")?;
            line_stmt
                .execute(params![
                    id,
                    i64::from(info.take),
                    seq as i64,
                    &line_key,
                    i64::from(said.line.t0),
                    i64::from(said.line.t1),
                    &said.line.speaker,
                    &said.line.language,
                    &said.line.text,
                    tr_json,
                    via_json,
                    i64::from(said.line.incomplete),
                ])
                .map_err(|_| "index")?;

            if info.take == rec.about.take {
                if !said.line.text.trim().is_empty() {
                    let norm = profile(&said.line.language).key(&said.line.text);
                    let snippet = truncate_utf8(&said.line.text, SEARCH_SNIPPET_MAX_BYTES);
                    doc_stmt
                        .execute(params![id, "line", &line_key, &said.line.language, norm, snippet])
                        .map_err(|_| "index")?;
                }
                for (tr_lang, tr_text) in &said.line.translations {
                    if !tr_text.trim().is_empty() {
                        let norm = profile(tr_lang).key(tr_text);
                        let snippet = truncate_utf8(tr_text, SEARCH_SNIPPET_MAX_BYTES);
                        doc_stmt
                            .execute(params![id, "translation", &line_key, tr_lang, norm, snippet])
                            .map_err(|_| "index")?;
                    }
                }
            }
        }
    }

    // Index all notes with non-empty text.
    for note in &rec.notes {
        if !note.text.trim().is_empty() {
            let norm = profile("").key(&note.text);
            let snippet = truncate_utf8(&note.text, SEARCH_SNIPPET_MAX_BYTES);
            doc_stmt
                .execute(params![id, "note", &note.key, "", norm, snippet])
                .map_err(|_| "index")?;
        }
    }

    Ok(true)
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ---- Civil calendar math (Howard Hinnant's exact O(1) epoch-day formulas) ----

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

const MS_PER_MIN: i64 = 60_000; // milliseconds in one minute for timezone offsets
const MS_PER_DAY: i64 = 86_400_000; // milliseconds in one UTC day

fn year_month_of(started_ms: i64, tz_offset_min: i32) -> (i64, u32) {
    let local_ms = started_ms.saturating_add(i64::from(tz_offset_min) * MS_PER_MIN);
    let days = local_ms.div_euclid(MS_PER_DAY);
    let (year, month, _) = civil_from_days(days);
    (year, month)
}

fn month_bounds_utc_ms(month: &str, tz_offset_min: i32) -> Option<(i64, i64)> {
    let (y_str, m_str) = month.split_once('-')?;
    let year: i64 = y_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    let (next_year, next_month) = if m == 12 { (year + 1, 1) } else { (year, m + 1) };
    let tz_ms = i64::from(tz_offset_min) * MS_PER_MIN;
    let start = days_from_civil(year, m, 1).saturating_mul(MS_PER_DAY).saturating_sub(tz_ms);
    let end =
        days_from_civil(next_year, next_month, 1).saturating_mul(MS_PER_DAY).saturating_sub(tz_ms);
    Some((start, end))
}

// ---- Read query implementations shared by `Index` and `Reader` ----

fn row_from_sqlite(r: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRow> {
    let spoken_json: String = r.get(5)?;
    let spoken: Vec<String> = serde_json::from_str(&spoken_json).unwrap_or_default();
    let take: i64 = r.get(6)?;
    let audio: i64 = r.get(7)?;
    let filed_state: String = r.get(8)?;
    let refine_failed: Option<String> = r.get(9)?;
    Ok(LibraryRow {
        id: r.get(0)?,
        started: r.get(1)?,
        length_ms: r.get::<_, i64>(2)?.max(0) as u32,
        title: r.get(3)?,
        preview: r.get(4)?,
        spoken,
        draft: take == 0,
        audio: audio != 0,
        filed: filed_state == "city.filed",
        refine_failed,
    })
}

fn query_recording_row(conn: &Connection, id: &str) -> Result<Option<LibraryRow>, &'static str> {
    conn.query_row(
        "SELECT id, started, length_ms, title, preview, spoken, take, audio, filed_state, refine_error \
         FROM recordings WHERE id = ?1",
        params![id],
        row_from_sqlite,
    )
    .optional()
    .map_err(|_| "index")
}

fn query_library_summary(
    conn: &Connection,
    tz_offset_min: i32,
) -> Result<LibrarySummary, &'static str> {
    let mut stmt = conn
        .prepare_cached("SELECT started FROM recordings ORDER BY started DESC")
        .map_err(|_| "index")?;
    let mut rows = stmt.query([]).map_err(|_| "index")?;
    let mut counts: BTreeMap<(i64, u32), u32> = BTreeMap::new();
    let mut total = 0u32;
    while let Some(row) = rows.next().map_err(|_| "index")? {
        let started: i64 = row.get(0).map_err(|_| "index")?;
        let ym = year_month_of(started, tz_offset_min);
        *counts.entry(ym).or_insert(0) += 1;
        total = total.saturating_add(1);
    }
    let months = counts
        .into_iter()
        .rev()
        .map(|((y, m), count)| MonthCount { month: format!("{y:04}-{m:02}"), count })
        .collect();
    Ok(LibrarySummary { total, months })
}

fn build_filter_where(
    range: Option<(i64, i64)>,
    filter: &LibraryFilter,
) -> (String, Vec<rusqlite::types::Value>) {
    let mut clauses = Vec::new();
    let mut vals: Vec<rusqlite::types::Value> = Vec::new();
    if let Some((start, end)) = range {
        clauses.push("started >= ?".to_owned());
        vals.push(rusqlite::types::Value::Integer(start));
        clauses.push("started < ?".to_owned());
        vals.push(rusqlite::types::Value::Integer(end));
    }
    if filter.draft_only {
        clauses.push("take = 0".to_owned());
    }
    if filter.audio_only {
        clauses.push("audio = 1".to_owned());
    }
    if filter.filed_only {
        clauses.push("filed_state = 'city.filed'".to_owned());
    }
    if let Some(ref lang) = filter.language {
        clauses.push(
            "EXISTS (SELECT 1 FROM json_each(recordings.spoken) WHERE json_each.value = ?)"
                .to_owned(),
        );
        vals.push(rusqlite::types::Value::Text(lang.clone()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, vals)
}

fn query_library_page(
    conn: &Connection,
    range: Option<(i64, i64)>,
    filter: &LibraryFilter,
    limit: u32,
    offset: u32,
) -> Result<LibraryPage, &'static str> {
    let limit =
        if limit == 0 { LIBRARY_PAGE_DEFAULT_ROWS } else { limit.min(LIBRARY_PAGE_MAX_ROWS) };
    let (where_sql, base_params) = build_filter_where(range, filter);

    let count_sql = format!("SELECT COUNT(*) FROM recordings{where_sql}");
    let total: u32 = conn
        .query_row(&count_sql, rusqlite::params_from_iter(base_params.iter()), |r| {
            Ok(r.get::<_, i64>(0)?.max(0) as u32)
        })
        .map_err(|_| "index")?;

    let select_sql = format!(
        "SELECT id, started, length_ms, title, preview, spoken, take, audio, filed_state, refine_error \
         FROM recordings{where_sql} ORDER BY started DESC, id ASC LIMIT ? OFFSET ?"
    );
    let mut select_params = base_params;
    select_params.push(rusqlite::types::Value::Integer(i64::from(limit)));
    select_params.push(rusqlite::types::Value::Integer(i64::from(offset)));

    let mut stmt = conn.prepare(&select_sql).map_err(|_| "index")?;
    let rows_iter = stmt
        .query_map(rusqlite::params_from_iter(select_params.iter()), row_from_sqlite)
        .map_err(|_| "index")?;
    let mut rows = Vec::new();
    for r in rows_iter {
        rows.push(r.map_err(|_| "index")?);
    }
    let has_more = offset.saturating_add(rows.len() as u32) < total;
    Ok(LibraryPage { rows, total, has_more })
}

fn query_library_month(
    conn: &Connection,
    month: &str,
    tz_offset_min: i32,
    filter: &LibraryFilter,
    limit: u32,
    offset: u32,
) -> Result<LibraryPage, &'static str> {
    let Some(bounds) = month_bounds_utc_ms(month, tz_offset_min) else {
        return Ok(LibraryPage::default());
    };
    query_library_page(conn, Some(bounds), filter, limit, offset)
}

fn query_library_recent(
    conn: &Connection,
    filter: &LibraryFilter,
    limit: u32,
    offset: u32,
) -> Result<LibraryPage, &'static str> {
    query_library_page(conn, None, filter, limit, offset)
}

fn hit_kind_from_str(s: &str) -> SearchHitKind {
    match s {
        "title" => SearchHitKind::Title,
        "translation" => SearchHitKind::Translation,
        "note" => SearchHitKind::Note,
        _ => SearchHitKind::Line,
    }
}

fn query_search(conn: &Connection, query: &str, limit: u32) -> Result<SearchResult, &'static str> {
    let norm = profile("").key(query);
    let norm = norm.trim();
    if norm.is_empty() {
        return Ok(SearchResult::default());
    }
    let cap = if limit == 0 { SEARCH_DEFAULT_HITS } else { limit.min(SEARCH_MAX_HITS) };
    let fetch = i64::from(cap) + 1;

    let mut hits = Vec::new();
    if norm.chars().count() >= 3 {
        let fts_match = format!("\"{}\"", norm.replace('"', "\"\""));
        let mut stmt = conn
            .prepare_cached(
                "SELECT d.recording_id, d.kind, d.item_key, d.lang, d.snippet \
                 FROM search_fts \
                 JOIN search_docs d ON d.doc_id = search_fts.rowid \
                 JOIN recordings r ON r.id = d.recording_id \
                 WHERE search_fts MATCH ?1 \
                 ORDER BY r.started DESC, d.doc_id ASC \
                 LIMIT ?2",
            )
            .map_err(|_| "index")?;
        let rows = stmt
            .query_map(params![fts_match, fetch], |r| {
                let kind: String = r.get(1)?;
                Ok(SearchHit {
                    recording_id: r.get(0)?,
                    kind: hit_kind_from_str(&kind),
                    key: r.get(2)?,
                    lang: r.get(3)?,
                    snippet: r.get(4)?,
                })
            })
            .map_err(|_| "index")?;
        for row in rows {
            hits.push(row.map_err(|_| "index")?);
        }
    } else {
        // FTS5 trigram indexes >= 3-char grams; fall back to normalized substring scan for 1-2 chars.
        let mut stmt = conn
            .prepare_cached(
                "SELECT d.recording_id, d.kind, d.item_key, d.lang, d.snippet \
                 FROM search_docs d \
                 JOIN recordings r ON r.id = d.recording_id \
                 WHERE instr(d.norm_text, ?1) > 0 \
                 ORDER BY r.started DESC, d.doc_id ASC \
                 LIMIT ?2",
            )
            .map_err(|_| "index")?;
        let rows = stmt
            .query_map(params![norm, fetch], |r| {
                let kind: String = r.get(1)?;
                Ok(SearchHit {
                    recording_id: r.get(0)?,
                    kind: hit_kind_from_str(&kind),
                    key: r.get(2)?,
                    lang: r.get(3)?,
                    snippet: r.get(4)?,
                })
            })
            .map_err(|_| "index")?;
        for row in rows {
            hits.push(row.map_err(|_| "index")?);
        }
    }

    let truncated = hits.len() > cap as usize;
    hits.truncate(cap as usize);
    Ok(SearchResult { hits, truncated })
}

fn said_from_sqlite(r: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Said)> {
    let key: String = r.get(0)?;
    let t0: i64 = r.get(1)?;
    let t1: i64 = r.get(2)?;
    let speaker: String = r.get(3)?;
    let language: String = r.get(4)?;
    let text: String = r.get(5)?;
    let tr_json: String = r.get(6)?;
    let via_json: String = r.get(7)?;
    let incomplete: i64 = r.get(8)?;
    let translations: BTreeMap<String, String> = serde_json::from_str(&tr_json).unwrap_or_default();
    let via: BTreeMap<String, String> = serde_json::from_str(&via_json).unwrap_or_default();
    let line = Line {
        t0: t0.max(0) as u32,
        t1: t1.max(0) as u32,
        speaker,
        language,
        text,
        translations,
        incomplete: incomplete != 0,
    };
    Ok((key, Said { line, via }))
}

fn query_take_line_count(conn: &Connection, id: &str, take: u32) -> Result<u32, &'static str> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM take_lines WHERE recording_id = ?1 AND take = ?2",
            params![id, i64::from(take)],
            |r| r.get(0),
        )
        .map_err(|_| "index")?;
    Ok(count.max(0) as u32)
}

fn query_take_lines_slice(
    conn: &Connection,
    id: &str,
    take: u32,
    offset: u32,
    limit: u32,
) -> Result<Vec<(String, Said)>, &'static str> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT line_key, t0, t1, speaker, language, text, translations_json, via_json, incomplete \
             FROM take_lines \
             WHERE recording_id = ?1 AND take = ?2 \
             ORDER BY seq ASC \
             LIMIT ?3 OFFSET ?4",
        )
        .map_err(|_| "index")?;
    let rows = stmt
        .query_map(
            params![id, i64::from(take), i64::from(limit), i64::from(offset)],
            said_from_sqlite,
        )
        .map_err(|_| "index")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|_| "index")?);
    }
    Ok(out)
}

fn query_take_lines_in_time(
    conn: &Connection,
    id: &str,
    take: u32,
    t0_ms: u32,
    t1_ms: u32,
) -> Result<Vec<(String, Said)>, &'static str> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT line_key, t0, t1, speaker, language, text, translations_json, via_json, incomplete \
             FROM take_lines \
             WHERE recording_id = ?1 AND take = ?2 AND t0 <= ?3 AND t1 >= ?4 \
             ORDER BY seq ASC",
        )
        .map_err(|_| "index")?;
    let rows = stmt
        .query_map(
            params![id, i64::from(take), i64::from(t1_ms), i64::from(t0_ms)],
            said_from_sqlite,
        )
        .map_err(|_| "index")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|_| "index")?);
    }
    Ok(out)
}
