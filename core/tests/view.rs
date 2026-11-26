mod hub;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use zu::SecretKey;
use zutalk_core::ai::write;
use zutalk_core::app::proto::{Change, Command, NoteOp};
use zutalk_core::capture::clock::{BLOCK, Block};
use zutalk_core::capture::tape;
use zutalk_core::knowledge::glossary;
use zutalk_core::live::lines::Line;
use zutalk_core::out::city::{Calendar, CityEvent, Filing};
use zutalk_core::record::notes;
use zutalk_core::record::shelf::{About, FILING, LINE, RECORDING, Said, Shelf};
use zutalk_core::record::takes::{RefineFailure, RefineStatus};
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::Tree;
use zutalk_core::view::index::{
    INDEX_SCHEMA_VERSION, Index, IndexHealth, LibraryFilter, OpenPlan, Reader, SearchHitKind,
    open_plan,
};
use zutalk_core::view::query::{
    Invalidations, KEEP_INVALIDATIONS_EVENTS, Query, QueryValue, ViewEngine, ViewKind,
};
use zutalk_core::view::transcript::{TRANSCRIPT_CHUNK_LINES, slice_chunk, total_chunks};

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const OTHER_ME: &str = "did:zu:qq7hq3xbd2lvf5jy6tkmzbxe";
const NOW: i64 = 1_742_000_000_000; // 2025-03-15T01:46:40Z

fn shelf_for(dir: &Path, did: &str, seed: u8) -> Shelf {
    Shelf::new(dir, did, Arc::new(SecretKey::from_seed(&[seed; 32])))
}

fn said(t0: u32, t1: u32, language: &str, text: &str, translations: &[(&str, &str)]) -> Said {
    let translations: BTreeMap<String, String> =
        translations.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let mut via = BTreeMap::from([("text".to_owned(), "stt-async-v6".to_owned())]);
    for lang in translations.keys() {
        via.insert(lang.clone(), "model".to_owned());
    }
    Said {
        line: Line {
            t0,
            t1,
            speaker: "1".into(),
            language: language.into(),
            text: text.into(),
            translations,
            incomplete: false,
        },
        via,
    }
}

fn write_recording(
    shelf: &Shelf,
    id: &str,
    started: i64,
    title: &str,
    spoken: &[&str],
    take: u32,
    takes_lines: &[(u32, Vec<Said>)],
) {
    let about = About {
        started,
        length_ms: 60_000,
        title: title.into(),
        preview: takes_lines
            .iter()
            .find(|(t, _)| *t == take)
            .and_then(|(_, lines)| lines.first())
            .map(|s| s.line.text.clone())
            .unwrap_or_default(),
        spoken: spoken.iter().map(|s| (*s).to_owned()).collect(),
        shown: spoken.iter().map(|s| (*s).to_owned()).collect(),
        source: "mic".into(),
        take,
        moved: false,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.to_owned(), Some(about))], started).expect("write about");
    for (t, lines) in takes_lines {
        let entries: Vec<(String, Option<Said>)> = lines
            .iter()
            .enumerate()
            .map(|(n, s)| (format!("{id}.{t}.{n:05}"), Some(s.clone())))
            .collect();
        shelf.put(id, LINE, &entries, started + i64::from(*t) + 1).expect("write lines");
    }
}

fn write_tape(tapes_dir: &Path, id: &str) {
    let key = [7u8; 32];
    let mut writer = tape::Writer::create(&tapes_dir.join(id), &key).expect("create tape");
    writer.append(&Block { t_ms: 0, pcm: vec![100; BLOCK] }).expect("append block");
    writer.sync().expect("sync tape");
}

#[test]
fn index_open_plan_table() {
    assert_eq!(open_plan(IndexHealth::Ready), OpenPlan::Reuse);
    assert_eq!(open_plan(IndexHealth::Missing), OpenPlan::Rebuild("missing"));
    assert_eq!(open_plan(IndexHealth::VersionMismatch), OpenPlan::Rebuild("version"));
    assert_eq!(open_plan(IndexHealth::OwnerMismatch), OpenPlan::Rebuild("owner"));
    assert_eq!(open_plan(IndexHealth::Corrupt), OpenPlan::Rebuild("corrupt"));
}

#[test]
fn index_builds_from_shelf_and_rebuilds_on_delete_version_owner_or_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");
    let shelf = shelf_for(&records, ME, 5);

    write_recording(
        &shelf,
        "rec1",
        NOW,
        "Quarterly Budget",
        &["en", "zh"],
        1,
        &[
            (0, vec![said(0, 1_000, "en", "draft hello", &[])]),
            (
                1,
                vec![said(
                    0,
                    2_000,
                    "en",
                    "The first shipment leaves in March.",
                    &[("zh", "第一批货物三月发出。")],
                )],
            ),
        ],
    );
    notes::add(&shelf, "rec1", 500, "Remember supplier deadline".into(), None, 0x11, NOW + 10)
        .expect("add note");
    write_tape(&tapes, "rec1");

    // 1. Missing DB -> full rebuild with mode="rebuilt", why="missing".
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let stats = Stats::new();
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("open missing");
    assert_eq!(idx.recording_row("rec1").expect("row").expect("present").title, "Quarterly Budget");
    assert_eq!(idx.take_line_count("rec1", 0).expect("t0 count"), 1);
    assert_eq!(idx.take_line_count("rec1", 1).expect("t1 count"), 1);
    assert!(
        seen.of("index.open")
            .iter()
            .any(|l| l.text("mode") == Some("rebuilt") && l.text("why") == Some("missing"))
    );
    drop(idx);

    // 2. Re-open existing DB -> reuse with mode="existing", why="ok".
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("reopen existing");
    assert!(
        seen.of("index.open")
            .iter()
            .any(|l| l.text("mode") == Some("existing") && l.text("why") == Some("ok"))
    );
    drop(idx);

    // 3. Corrupt file -> delete & rebuild, data 100% restored, warn why="corrupt", index.rebuilds += 1.
    fs::write(&db, b"not a valid sqlite database at all!").expect("corrupt db");
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let corrupt_stats = Stats::new();
    let idx = Index::open(&db, &shelf, &tapes, &corrupt_stats).expect("rebuild corrupt");
    assert_eq!(
        idx.recording_row("rec1").expect("row").expect("restored").title,
        "Quarterly Budget"
    );
    assert!(seen.of("index.open").iter().any(|l| {
        l.level == zutalk_log::Level::Warn
            && l.text("mode") == Some("rebuilt")
            && l.text("why") == Some("corrupt")
    }));
    let rebuilds =
        corrupt_stats.tally().counts.into_iter().find(|c| c.name == "index.rebuilds").map(|c| c.n);
    assert_eq!(rebuilds, Some(1));
    drop(idx);

    // 4. Schema version mismatch -> rebuild with why="version".
    {
        let conn = rusqlite::Connection::open(&db).expect("raw sqlite");
        conn.pragma_update(None, "user_version", INDEX_SCHEMA_VERSION + 999).expect("bump version");
    }
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("rebuild version");
    assert!(
        seen.of("index.open")
            .iter()
            .any(|l| l.text("mode") == Some("rebuilt") && l.text("why") == Some("version"))
    );
    drop(idx);

    // 5. Owner mismatch (different DID pointing at same path) -> rebuild with why="owner".
    let other_records = dir.path().join("other_records");
    let other_shelf = shelf_for(&other_records, OTHER_ME, 9);
    write_recording(&other_shelf, "other1", NOW, "Other person", &["en"], 0, &[]);
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let idx = Index::open(&db, &other_shelf, &tapes, &stats).expect("rebuild owner");
    assert!(idx.recording_row("rec1").expect("old gone").is_none());
    assert!(idx.recording_row("other1").expect("new present").is_some());
    assert!(
        seen.of("index.open")
            .iter()
            .any(|l| l.text("mode") == Some("rebuilt") && l.text("why") == Some("owner"))
    );
    drop(idx);

    // Rebuild back for `shelf`, then append a new recording offline and verify `reconcile` picks it up.
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("rebuild back");
    drop(idx);
    write_recording(&shelf, "rec2", NOW + 60_000, "Added offline", &["en"], 0, &[]);
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("reopen reconcile");
    assert!(
        seen.of("index.open")
            .iter()
            .any(|l| l.text("mode") == Some("existing") && l.text("why") == Some("ok"))
    );
    assert_eq!(idx.recording_row("rec2").expect("row").expect("reconciled").title, "Added offline");
}

#[test]
fn index_incremental_sync_tracks_upsert_remove_audio_filing_and_refine_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");
    let shelf = shelf_for(&records, ME, 5);
    let stats = Stats::new();

    let mut idx = Index::open(&db, &shelf, &tapes, &stats).expect("open empty");
    let reader = Reader::open(&db).expect("open reader");

    write_recording(
        &shelf,
        "r1",
        NOW,
        "Initial title",
        &["en"],
        0,
        &[(0, vec![said(0, 1_000, "en", "First words", &[])])],
    );
    write_tape(&tapes, "r1");
    idx.upsert_recording(&shelf, &tapes, "r1").expect("upsert");

    let row = reader.recording_row("r1").expect("read").expect("exists");
    assert_eq!(row.title, "Initial title");
    assert!(row.draft);
    assert!(row.audio);
    assert!(!row.filed);
    assert_eq!(row.refine_failed, None);

    // Update refine error.
    let failure = RefineFailure { code: "keys.async_refused".into(), at: NOW + 100 };
    write::record_failure(&tapes.join("r1"), &failure.code, failure.at).expect("save failure");
    idx.update_refine_error("r1", Some(&failure)).expect("sync refine error");
    assert_eq!(
        reader.recording_row("r1").expect("read").expect("exists").refine_failed.as_deref(),
        Some("keys.async_refused")
    );

    // Clear refine error.
    idx.update_refine_error("r1", None).expect("clear refine error");
    assert_eq!(reader.recording_row("r1").expect("read").expect("exists").refine_failed, None);

    // Update filing state.
    let filing = Filing {
        city: "city.example".into(),
        did: "did:zu:city".into(),
        event: "zu://did:zu:org/ev/event/1".into(),
        title: "Town hall".into(),
        audio: false,
        key: "k1".into(),
        state: "city.filed".into(),
    };
    shelf.put("", FILING, &[("r1".into(), Some(filing))], NOW + 200).expect("put filing");
    idx.update_filing("r1", "city.filed").expect("sync filing");
    assert!(reader.recording_row("r1").expect("read").expect("exists").filed);

    // Update audio flag.
    idx.update_audio("r1", false).expect("sync audio");
    assert!(!reader.recording_row("r1").expect("read").expect("exists").audio);

    // Remove recording.
    idx.remove_recording("r1").expect("remove");
    assert!(reader.recording_row("r1").expect("read").is_none());
    assert!(reader.take_lines_slice("r1", 0, 0, 10).expect("lines gone").is_empty());
    assert!(reader.search("Initial", 10).expect("search gone").hits.is_empty());
}

#[test]
fn index_library_months_recent_and_filters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");
    let shelf = shelf_for(&records, ME, 5);
    let stats = Stats::new();

    // Timestamps spanning 4 months in UTC, including a month-boundary timestamp:
    // 2025-01-31T22:00:00Z = 1_738_360_800_000 (Jan in UTC, Feb in UTC+7 / +420 min!)
    let jan_edge = 1_738_360_800_000i64;
    let feb_mid = 1_739_577_600_000i64; // 2025-02-15T00:00:00Z
    let mar_mid = 1_741_996_800_000i64; // 2025-03-15T00:00:00Z
    let apr_mid = 1_744_675_200_000i64; // 2025-04-15T00:00:00Z

    write_recording(&shelf, "m1", jan_edge, "Jan edge", &["en"], 0, &[]);
    write_recording(&shelf, "m2", feb_mid, "Feb talk", &["zh", "en"], 1, &[]);
    write_recording(&shelf, "m3", mar_mid, "Mar sync", &["th"], 1, &[]);
    write_recording(&shelf, "m4", apr_mid, "Apr review", &["zh"], 0, &[]);
    write_recording(&shelf, "m5", apr_mid + 3_600_000, "Apr later", &["en"], 1, &[]);
    write_tape(&tapes, "m1");
    write_tape(&tapes, "m3");

    let filing = Filing { state: "city.filed".into(), ..Filing::default() };
    shelf.put("", FILING, &[("m3".into(), Some(filing))], mar_mid).expect("put filing");

    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("open");

    // Summary at UTC (offset 0): 2025-04 (2), 2025-03 (1), 2025-02 (1), 2025-01 (1).
    let utc = idx.library_summary(0).expect("utc summary");
    assert_eq!(utc.total, 5);
    assert_eq!(
        utc.months.iter().map(|m| (m.month.as_str(), m.count)).collect::<Vec<_>>(),
        vec![("2025-04", 2), ("2025-03", 1), ("2025-02", 1), ("2025-01", 1)]
    );

    // Summary at UTC+7 (+420 min): jan_edge rolls into 2025-02!
    let bkk = idx.library_summary(420).expect("bkk summary");
    assert_eq!(bkk.total, 5);
    assert_eq!(
        bkk.months.iter().map(|m| (m.month.as_str(), m.count)).collect::<Vec<_>>(),
        vec![("2025-04", 2), ("2025-03", 1), ("2025-02", 2)]
    );

    // Monthly pagination (2025-04 has 2 rows: m5 then m4).
    let p1 = idx.library_month("2025-04", 0, &LibraryFilter::default(), 1, 0).expect("p1");
    assert_eq!(p1.total, 2);
    assert!(p1.has_more);
    assert_eq!(p1.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["m5"]);

    let p2 = idx.library_month("2025-04", 0, &LibraryFilter::default(), 1, 1).expect("p2");
    assert_eq!(p2.total, 2);
    assert!(!p2.has_more);
    assert_eq!(p2.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["m4"]);

    // Recent ordering & filters.
    let recent = idx.library_recent(&LibraryFilter::default(), 3, 0).expect("recent");
    assert_eq!(recent.total, 5);
    assert!(recent.has_more);
    assert_eq!(
        recent.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["m5", "m4", "m3"]
    );

    let drafts = idx
        .library_recent(&LibraryFilter { draft_only: true, ..Default::default() }, 10, 0)
        .expect("drafts");
    assert_eq!(drafts.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["m4", "m1"]);

    let with_audio = idx
        .library_recent(&LibraryFilter { audio_only: true, ..Default::default() }, 10, 0)
        .expect("audio");
    assert_eq!(with_audio.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["m3", "m1"]);

    let filed = idx
        .library_recent(&LibraryFilter { filed_only: true, ..Default::default() }, 10, 0)
        .expect("filed");
    assert_eq!(filed.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["m3"]);

    let zh = idx
        .library_recent(&LibraryFilter { language: Some("zh".into()), ..Default::default() }, 10, 0)
        .expect("zh");
    assert_eq!(zh.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["m4", "m2"]);
}

#[test]
fn index_search_covers_title_lines_translations_notes_cjk_thai_fullwidth_and_short_queries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");
    let shelf = shelf_for(&records, ME, 5);
    let stats = Stats::new();

    write_recording(
        &shelf,
        "s1",
        NOW,
        "Quarterly Roadmap",
        &["en", "zh", "th"],
        1,
        &[(
            1,
            vec![
                said(
                    0,
                    2_000,
                    "en",
                    "The first batch ships in March.",
                    &[("zh", "第一批预算三月发货。")],
                ),
                said(2_000, 4_000, "th", "ราคานี้รวมภาษีแล้ว น\u{e49}\u{e33}หนัก 5 กิโล", &[]),
            ],
        )],
    );
    let note =
        notes::add(&shelf, "s1", 1_200, "Check customs tariff rate".into(), None, 0x77, NOW + 5)
            .expect("add note");

    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("open");

    // Title hit.
    let title_hits = idx.search("roadmap", 20).expect("title");
    assert!(
        title_hits.hits.iter().any(|h| h.recording_id == "s1" && h.kind == SearchHitKind::Title)
    );

    // Line hit (case-insensitive & full-width).
    let en_hits = idx.search("SHIPS IN march", 20).expect("en");
    assert!(en_hits.hits.iter().any(|h| h.key == "s1.1.00000" && h.kind == SearchHitKind::Line));
    let fw_hits = idx.search("Ｍａｒｃｈ", 20).expect("fullwidth");
    assert!(fw_hits.hits.iter().any(|h| h.key == "s1.1.00000" && h.kind == SearchHitKind::Line));

    // Translation hit: 4-char CJK (trigram FTS5), 2-char CJK and 1-char CJK (short fallback).
    let zh4 = idx.search("三月发货", 20).expect("zh4");
    assert!(zh4.hits.iter().any(|h| h.key == "s1.1.00000" && h.kind == SearchHitKind::Translation));
    let zh2 = idx.search("预算", 20).expect("zh2");
    assert!(zh2.hits.iter().any(|h| h.key == "s1.1.00000" && h.kind == SearchHitKind::Translation));
    let zh1 = idx.search("月", 20).expect("zh1");
    assert!(zh1.hits.iter().any(|h| h.key == "s1.1.00000" && h.kind == SearchHitKind::Translation));

    // Thai SARA AM alternate spelling matches.
    let thai = idx.search("น\u{e4d}\u{e49}\u{e32}หนัก", 20).expect("thai");
    assert!(thai.hits.iter().any(|h| h.key == "s1.1.00001" && h.kind == SearchHitKind::Line));

    // Timestamped note hit.
    let note_hits = idx.search("tariff", 20).expect("note");
    assert!(
        note_hits
            .hits
            .iter()
            .any(|h| h.recording_id == "s1" && h.kind == SearchHitKind::Note && h.key == note.key)
    );

    // Empty / whitespace query returns empty set.
    assert!(idx.search("   ", 20).expect("empty").hits.is_empty());

    // EXPLAIN QUERY PLAN assertions (I8: no full scans for search >= 3, chunked lines, or monthly list).
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("raw ro");
    let plan_of = |sql: &str| -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).expect("prepare eqp");
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .expect("query eqp")
            .filter_map(Result::ok)
            .collect();
        rows.join(" | ")
    };

    let search_plan = plan_of(
        "SELECT d.recording_id FROM search_fts \
         JOIN search_docs d ON d.doc_id = search_fts.rowid \
         JOIN recordings r ON r.id = d.recording_id \
         WHERE search_fts MATCH '\"march\"' \
         ORDER BY r.started DESC, d.doc_id ASC LIMIT 100",
    );
    assert!(
        search_plan.contains("search_fts") && search_plan.contains("VIRTUAL TABLE INDEX"),
        "expected search_fts virtual index in plan: {search_plan}"
    );

    let chunk_plan = plan_of(
        "SELECT line_key FROM take_lines \
         WHERE recording_id = 's1' AND take = 1 \
         ORDER BY seq ASC LIMIT 64 OFFSET 0",
    );
    assert!(
        chunk_plan.contains("PRIMARY KEY") && chunk_plan.contains("take_lines"),
        "expected take_lines PRIMARY KEY in plan: {chunk_plan}"
    );

    let month_plan = plan_of(
        "SELECT id FROM recordings \
         WHERE started >= 1740787200000 AND started < 1743465600000 \
         ORDER BY started DESC, id ASC LIMIT 50 OFFSET 0",
    );
    assert!(
        month_plan.contains("idx_recordings_started"),
        "expected idx_recordings_started in plan: {month_plan}"
    );
}

// ---- P2-04: chunking, invalidations ring, and ViewEngine ----

#[test]
fn transcript_chunking_splits_at_64_lines_and_handles_empty_and_out_of_range() {
    assert_eq!(TRANSCRIPT_CHUNK_LINES, 64);
    assert_eq!(total_chunks(0), 0);
    assert_eq!(total_chunks(1), 1);
    assert_eq!(total_chunks(64), 1);
    assert_eq!(total_chunks(65), 2);
    assert_eq!(total_chunks(200), 4);

    // 0 lines
    let empty = slice_chunk("r0", 0, &[], 0);
    assert_eq!(empty.total_lines, 0);
    assert_eq!(empty.total_chunks, 0);
    assert!(empty.lines.is_empty());

    // 1, 64, 65, and 200 lines
    let make = |n: usize| -> Vec<(String, Said)> {
        (0..n)
            .map(|i| {
                (
                    format!("r.1.{i:05}"),
                    said(i as u32 * 1_000, i as u32 * 1_000 + 800, "en", &format!("line {i}"), &[]),
                )
            })
            .collect()
    };

    let c1 = slice_chunk("r", 1, &make(1), 0);
    assert_eq!((c1.total_lines, c1.total_chunks, c1.lines.len()), (1, 1, 1));

    let c64 = slice_chunk("r", 1, &make(64), 0);
    assert_eq!((c64.total_lines, c64.total_chunks, c64.lines.len()), (64, 1, 64));
    assert!(slice_chunk("r", 1, &make(64), 1).lines.is_empty());

    let lines65 = make(65);
    let c65_0 = slice_chunk("r", 1, &lines65, 0);
    let c65_1 = slice_chunk("r", 1, &lines65, 1);
    assert_eq!((c65_0.total_lines, c65_0.total_chunks, c65_0.lines.len()), (65, 2, 64));
    assert_eq!((c65_1.total_lines, c65_1.total_chunks, c65_1.lines.len()), (65, 2, 1));
    assert_eq!(c65_1.lines[0].0, "r.1.00064");

    let lines200 = make(200);
    let c200_3 = slice_chunk("r", 1, &lines200, 3);
    assert_eq!((c200_3.total_lines, c200_3.total_chunks, c200_3.lines.len()), (200, 4, 8));
    assert_eq!(c200_3.lines[0].0, "r.1.00192");

    // Out of range chunk returns empty lines with accurate totals.
    let oob = slice_chunk("r", 1, &lines200, 99);
    assert_eq!((oob.total_lines, oob.total_chunks, oob.lines.len()), (200, 4, 0));
}

#[test]
fn invalidations_ring_coalesces_duplicates_and_caps_at_64() {
    let mut ring = Invalidations::default();
    for v in 1..=100u64 {
        ring.push(ViewKind::Recording, Some("r1".into()), v);
    }
    assert_eq!(ring.next, 100);
    assert_eq!(ring.items.len(), 1);
    assert_eq!(ring.items[0].version, 100);
    assert_eq!(ring.items[0].key.as_deref(), Some("r1"));

    let mut distinct = Invalidations::default();
    for v in 1..=100u64 {
        distinct.push(ViewKind::Recording, Some(format!("r{v}")), v);
    }
    assert_eq!(distinct.next, 100);
    assert_eq!(distinct.items.len(), KEEP_INVALIDATIONS_EVENTS);
    assert_eq!(distinct.items.first().map(|i| i.version), Some(37));
    assert_eq!(distinct.items.last().map(|i| i.version), Some(100));
}

#[tokio::test(flavor = "multi_thread")]
async fn view_engine_executes_all_queries_and_cancels_superseded_search() {
    let dir = tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");
    let shelf = Arc::new(shelf_for(&records, ME, 5));

    write_recording(
        &shelf,
        "rec1",
        NOW,
        "Quarterly Budget",
        &["en", "zh"],
        1,
        &[
            (
                0,
                vec![
                    said(0, 1_000, "en", "The budget", &[]),
                    said(1_100, 2_000, "en", "is five thousand.", &[]),
                ],
            ),
            (1, vec![said(0, 2_100, "en", "The budget is 5,000.", &[("zh", "预算是五千。")])]),
        ],
    );
    notes::add(
        &shelf,
        "rec1",
        500,
        "Confirm budget".into(),
        Some("rec1.1.00000".into()),
        0x12,
        NOW + 5,
    )
    .expect("add note");
    write_tape(&tapes, "rec1");
    glossary::put(
        &shelf,
        "ZuTalk",
        Some(BTreeMap::from([("zh".into(), "ZuTalk".into())])),
        NOW + 6,
    )
    .expect("add term");
    let filing = Filing {
        city: "city.example".into(),
        did: "did:zu:city".into(),
        event: "zu://did:zu:org/ev/event/1".into(),
        title: "Town hall".into(),
        audio: false,
        key: "filek1".into(),
        state: "city.filed".into(),
    };
    shelf.put("", FILING, &[("rec1".into(), Some(filing.clone()))], NOW + 7).expect("put filing");

    let stats = Stats::new();
    let _index = Index::open(&db, &shelf, &tapes, &stats).expect("build index");
    let tree = Tree::root("core", stats.clone()).child("work");
    let (engine, invalidations) = ViewEngine::new(tree.clone(), tapes.clone());

    // 1. Before ready, queries return Err("starting").
    let err = engine.query(Query::LibrarySummary { tz_offset_min: 0 }).await.unwrap_err();
    assert_eq!(err, "starting");

    // 2. Wire source + calendar + invalidate so version advances.
    engine.set_source(Some(shelf.clone()), Some(db.clone()), true, true);
    engine.set_calendar(Some(Calendar {
        address: "city.example".into(),
        did: "did:zu:city".into(),
        events: vec![CityEvent {
            path: "zu://did:zu:org/ev/event/1".into(),
            title: "Town hall".into(),
            start: Some(NOW),
        }],
    }));
    let v1 = engine.invalidate(ViewKind::Library, None);
    assert_eq!(v1, 1);
    assert_eq!(invalidations.now().1.next, 1);

    // 3. Execute all 10 query variants.
    let r = engine.query(Query::LibrarySummary { tz_offset_min: 0 }).await.expect("summary");
    assert_eq!(r.version, 1);
    assert!(matches!(r.value, QueryValue::LibrarySummary(ref s) if s.total == 1));

    let r = engine
        .query(Query::LibraryMonth {
            month: "2025-03".into(),
            tz_offset_min: 0,
            filter: LibraryFilter::default(),
            limit: 50,
            offset: 0,
        })
        .await
        .expect("month");
    assert!(matches!(r.value, QueryValue::LibraryPage(ref p) if p.total == 1));

    let r = engine
        .query(Query::LibraryRecent { filter: LibraryFilter::default(), limit: 10, offset: 0 })
        .await
        .expect("recent");
    assert!(matches!(r.value, QueryValue::LibraryPage(ref p) if p.rows.len() == 1));

    let r =
        engine.query(Query::Search { query: "budget".into(), limit: 20 }).await.expect("search");
    assert!(matches!(r.value, QueryValue::Search(ref s) if !s.hits.is_empty()));

    let r = engine.query(Query::Recording { id: "rec1".into() }).await.expect("recording");
    match r.value {
        QueryValue::Recording(Some(view)) => {
            assert_eq!(view.row.title, "Quarterly Budget");
            assert_eq!(view.takes.len(), 2);
            assert_eq!(view.notes.len(), 1);
            assert_eq!(view.filed, Some(filing));
            assert_eq!(view.refine, RefineStatus::Refined { take: 1 });
        }
        other => panic!("unexpected Recording reply: {other:?}"),
    }

    let r = engine
        .query(Query::Transcript { id: "rec1".into(), take: None, chunk: 0 })
        .await
        .expect("transcript");
    assert!(matches!(r.value, QueryValue::Transcript(ref c) if c.take == 1 && c.lines.len() == 1));

    let r = engine.query(Query::Glossary).await.expect("glossary");
    assert!(matches!(r.value, QueryValue::Glossary(ref terms) if terms.len() == 1));

    let r = engine.query(Query::Calendar).await.expect("calendar");
    assert!(matches!(r.value, QueryValue::Calendar(Some(ref cal)) if cal.events.len() == 1));

    let r = engine.query(Query::Outbound).await.expect("outbound");
    assert!(
        matches!(r.value, QueryValue::Outbound(ref rows) if rows.len() == 1 && rows[0].id == "rec1")
    );

    // 4. Deterministic cancellation: block the first Search inside the worker until a second Search
    //    supersedes its slot, then verify Err("query.cancelled"), counter, and log event.
    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let entered_hook = entered.clone();
    let armed = Arc::new(AtomicBool::new(true));
    let armed_hook = armed.clone();
    engine.set_hook(Some(Arc::new(move |view, cancel| {
        if view == "search" && armed_hook.swap(false, Ordering::AcqRel) {
            {
                let (lock, cvar) = &*entered_hook;
                *lock.lock().expect("lock") = true;
                cvar.notify_all();
            }
            // Wait deterministically for the superseding query to cancel our token.
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    })));

    let eng1 = engine.clone();
    let first = tokio::spawn(zutalk_log::carry(async move {
        eng1.query(Query::Search { query: "budget".into(), limit: 20 }).await
    }));

    // Wait until the first Search is inside the blocking worker with its slot active.
    tokio::task::spawn_blocking(move || {
        let (lock, cvar) = &*entered;
        let mut started = lock.lock().expect("lock");
        while !*started {
            started = cvar.wait(started).expect("wait");
        }
    })
    .await
    .expect("barrier");

    let v2 = engine.invalidate(ViewKind::Search, None);
    assert!(v2 > v1);

    // Second Search on the same slot cancels the first and completes cleanly.
    let second = engine
        .query(Query::Search { query: "budget".into(), limit: 20 })
        .await
        .expect("second search");
    assert_eq!(second.version, v2);

    let first_res = first.await.expect("join first");
    assert_eq!(first_res, Err("query.cancelled"));

    let cancelled_count =
        stats.tally().counts.into_iter().find(|c| c.name == "query.cancelled").map(|c| c.n);
    assert!(cancelled_count.unwrap_or(0) >= 1, "expected query.cancelled >= 1");
    assert!(
        seen.of("query.done")
            .iter()
            .any(|l| l.text("view") == Some("search") && l.text("code") == Some("cancelled")),
        "expected query.done(code = cancelled) event, got {:?}",
        seen.of("query.done")
    );

    engine.set_hook(None);
    tree.cancel();
    tree.wait().await;
}

#[tokio::test]
async fn core_query_reflects_recording_notes_changes_search_takes_and_invalidates() {
    let _guard = hub::STORE.lock().await;
    hub::store(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path();

    // Seed two recordings on the signed-in shelf:
    // 1. `rec_cmp` with take 0 + take 1 and valid audio (for Compare + DestroyAudio).
    // 2. `rec_broken` with take 0 and a corrupt tape file (triggers local Refine failure without network).
    let shelf = hub::shelf(data);
    write_recording(
        &shelf,
        "rec_cmp",
        NOW,
        "Original Compare Title",
        &["en"],
        1,
        &[
            (0, vec![said(0, 2_000, "en", "draft wording here", &[])]),
            (1, vec![said(0, 2_000, "en", "refined wording here", &[])]),
        ],
    );
    hub::tape(data, "rec_cmp", 1);

    // Seed `rec_broken` with `moved = true` so OwnKey's auto-survey leaves it alone and our
    // explicit Command::Refine triggers `write_job` deterministically.
    shelf
        .put(
            "rec_broken",
            RECORDING,
            &[(
                "rec_broken".to_owned(),
                Some(About {
                    started: NOW + 10_000,
                    length_ms: 1_000,
                    title: "Broken Tape Draft".into(),
                    preview: "broken tape line".into(),
                    spoken: vec!["en".into()],
                    shown: vec!["en".into()],
                    source: "mic".into(),
                    take: 0,
                    moved: true,
                    ..About::default()
                }),
            )],
            NOW + 10_000,
        )
        .expect("write broken about");
    let broken_dir = data.join("tape").join("rec_broken");
    fs::create_dir_all(&broken_dir).expect("mkdir broken tape");
    fs::write(broken_dir.join("00000.tape"), b"corrupt-audio-bytes").expect("write broken tape");

    let mut app = hub::App::start(data);
    app.until(|s| s.me.as_deref() == Some(hub::ME)).await;

    // Import a short local WAV file -> sealed draft lands in index via IndexSync.
    let wav_path = data.join("clip.wav");
    hub::wav(&wav_path, 300);
    let v_before_import = app.outlet.invalidated.now().1.next;
    app.send(Command::Import { path: wav_path.to_string_lossy().into_owned() });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let imported_id = loop {
        if let Ok(reply) = app
            .core
            .query(Query::LibraryRecent { filter: LibraryFilter::default(), limit: 10, offset: 0 })
            .await
            && let QueryValue::LibraryPage(page) = reply.value
            && page.total == 3
            && let Some(row) = page.rows.iter().find(|r| r.id != "rec_cmp" && r.id != "rec_broken")
            && row.length_ms > 0
        {
            break row.id.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for imported recording");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let v_after_import = app.outlet.invalidated.now().1.next;
    assert!(v_after_import > v_before_import, "import must advance invalidation version");

    // LibrarySummary reflects all 3 recordings.
    let summary =
        app.core.query(Query::LibrarySummary { tz_offset_min: 0 }).await.expect("summary");
    assert!(matches!(summary.value, QueryValue::LibrarySummary(ref s) if s.total == 3));

    // Command::Note -> invalidates Recording + Search and is queryable immediately after sync.
    let v_before_note = app.outlet.invalidated.now().1.next;
    app.send(Command::Note {
        id: imported_id.clone(),
        op: NoteOp::Add { at_ms: 250, text: "AlphaNoteToken".into(), anchor: None },
    });
    let mut inv_rx = app.outlet.invalidated.clone();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let v_after_note = loop {
        let (_, inv) = inv_rx.now();
        if inv.next > v_before_note
            && inv.items.iter().any(|i| {
                i.version > v_before_note
                    && i.view == ViewKind::Recording
                    && i.key.as_deref() == Some(imported_id.as_str())
            })
            && inv.items.iter().any(|i| i.version > v_before_note && i.view == ViewKind::Search)
        {
            break inv.next;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for note invalidation");
        let _ = tokio::time::timeout(Duration::from_millis(200), inv_rx.next()).await;
    };
    assert!(v_after_note > v_before_note);

    let rec_reply =
        app.core.query(Query::Recording { id: imported_id.clone() }).await.expect("recording");
    assert!(matches!(
        rec_reply.value,
        QueryValue::Recording(Some(ref view))
            if view.notes.iter().any(|n| n.text == "AlphaNoteToken")
    ));
    let search_note = app
        .core
        .query(Query::Search { query: "AlphaNoteToken".into(), limit: 20 })
        .await
        .expect("search note");
    assert!(matches!(
        search_note.value,
        QueryValue::Search(ref res)
            if res.hits.iter().any(|h| h.recording_id == imported_id && h.kind == SearchHitKind::Note)
    ));

    // Command::Change(Title) -> invalidates Library + Search + Recording.
    let v_before_title = app.outlet.invalidated.now().1.next;
    app.send(Command::Change {
        id: imported_id.clone(),
        change: Change::Title { text: "RenamedQueryTitle".into() },
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, inv) = inv_rx.now();
        if inv.next > v_before_title
            && inv.items.iter().any(|i| i.version > v_before_title && i.view == ViewKind::Library)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for title invalidation");
        let _ = tokio::time::timeout(Duration::from_millis(200), inv_rx.next()).await;
    }
    let search_title = app
        .core
        .query(Query::Search { query: "RenamedQueryTitle".into(), limit: 20 })
        .await
        .expect("search title");
    assert!(matches!(
        search_title.value,
        QueryValue::Search(ref res)
            if res.hits.iter().any(|h| h.recording_id == imported_id && h.kind == SearchHitKind::Title)
    ));

    // Command::Term -> invalidates Glossary and returns term via Query::Glossary.
    let v_before_term = app.outlet.invalidated.now().1.next;
    app.send(Command::Term {
        said: "Zuzalu".into(),
        written: Some(BTreeMap::from([("zh".into(), "祖扎鲁".into())])),
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, inv) = inv_rx.now();
        if inv.next > v_before_term
            && inv.items.iter().any(|i| i.version > v_before_term && i.view == ViewKind::Glossary)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for term invalidation");
        let _ = tokio::time::timeout(Duration::from_millis(200), inv_rx.next()).await;
    }
    let glossary_reply = app.core.query(Query::Glossary).await.expect("glossary");
    assert!(matches!(
        glossary_reply.value,
        QueryValue::Glossary(ref terms) if terms.iter().any(|t| t.said == "Zuzalu")
    ));

    // Query::Transcript on the 2-take recording.
    let tr = app
        .core
        .query(Query::Transcript { id: "rec_cmp".into(), take: Some(1), chunk: 0 })
        .await
        .expect("transcript");
    assert!(matches!(
        tr.value,
        QueryValue::Transcript(ref c)
            if c.total_lines == 1 && c.lines[0].1.line.text == "refined wording here"
    ));

    // Command::DestroyAudio on `imported_id` and `rec_cmp` -> flips `row.audio` to false and
    // keeps `imported_id` out of OwnKey's auto-survey.
    app.send(Command::DestroyAudio { id: imported_id.clone() });
    app.send(Command::DestroyAudio { id: "rec_cmp".into() });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(r1) = app.core.query(Query::Recording { id: imported_id.clone() }).await
            && let Ok(r2) = app.core.query(Query::Recording { id: "rec_cmp".into() }).await
            && matches!(r1.value, QueryValue::Recording(Some(ref v)) if !v.row.audio)
            && matches!(r2.value, QueryValue::Recording(Some(ref v)) if !v.row.audio)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for DestroyAudio");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Command::Refine on `rec_broken`: watch Outlet.progress for active refine progress, then
    // verify the fatal local tape error persists into Query::Recording.
    let seen_refine_active = Arc::new(AtomicBool::new(false));
    let seen_refine_flag = seen_refine_active.clone();
    let mut prog_rx = app.outlet.progress.clone();
    let watch_progress = tokio::spawn(async move {
        while let Some((_, prog)) = prog_rx.next().await {
            if prog.refine.as_ref().is_some_and(|w| w.id == "rec_broken") {
                seen_refine_flag.store(true, Ordering::Release);
            }
        }
    });

    app.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-local-test".into()) });
    app.until(|s| {
        s.keys
            .iter()
            .any(|(name, from)| name == "soniox" && *from == zutalk_core::id::keys::From::Own)
    })
    .await;
    app.send(Command::Refine { id: "rec_broken".into() });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(reply) = app.core.query(Query::Recording { id: "rec_broken".into() }).await
            && let QueryValue::Recording(Some(ref view)) = reply.value
            && matches!(view.refine, RefineStatus::Failed { ref code, .. } if code == "tape")
            && app.outlet.progress.now().1.refine.is_none()
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for refine failure");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    watch_progress.abort();
    assert!(
        seen_refine_active.load(Ordering::Acquire),
        "Outlet.progress must observe refine progress for rec_broken"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn view_engine_never_stamps_stale_source_with_newer_version() {
    use std::sync::{Condvar, Mutex};
    use zutalk_core::out::city::{Calendar, CityEvent};
    use zutalk_core::rt::stats::Stats;
    use zutalk_core::rt::supervise::Tree;
    use zutalk_core::view::query::{
        QUERY_CONCURRENT_MAX_JOBS, Query, QueryValue, ViewEngine, ViewKind,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = Arc::new(shelf_for(&dir.path().join("records"), ME, 5));
    let tapes = dir.path().join("tapes");
    write_recording(
        &shelf,
        "rec1",
        NOW,
        "Snapshot Order Test",
        &["en"],
        0,
        &[(0, vec![said(0, 1_000, "en", "hello world", &[])])],
    );

    let stats = Stats::new();
    let tree = Tree::root("core", stats.clone()).child("work");
    let db = dir.path().join("index.sqlite3");
    let _idx = Index::open(&db, &shelf, &tapes, &stats).expect("open");

    let (engine, _inv) = ViewEngine::new(tree.clone(), tapes);
    engine.set_source(Some(shelf), Some(db), false, true);
    engine.set_calendar(None);

    // Block all QUERY_CONCURRENT_MAX_JOBS (8) permits inside the Search worker hook until released.
    let state = Arc::new((Mutex::new((0usize, false)), Condvar::new()));
    let hook_state = state.clone();
    engine.set_hook(Some(Arc::new(move |view, _cancel| {
        if view == "search" {
            let (lock, cvar) = &*hook_state;
            let mut guard = lock.lock().expect("lock");
            guard.0 += 1;
            cvar.notify_all();
            while !guard.1 {
                guard = cvar.wait(guard).expect("wait");
            }
        }
    })));

    let mut blockers = Vec::with_capacity(QUERY_CONCURRENT_MAX_JOBS);
    for i in 0..QUERY_CONCURRENT_MAX_JOBS {
        let eng = engine.clone();
        blockers.push(tokio::spawn(async move {
            eng.query(Query::Search { query: "hello".into(), limit: 10 }).await
        }));
        // Wait for blocker `i` to hold its semaphore permit inside the hook before the next
        // Search supersedes its slot, so all 8 permits are deterministically occupied.
        let wait_state = state.clone();
        let target = i + 1;
        tokio::task::spawn_blocking(move || {
            let (lock, cvar) = &*wait_state;
            let mut guard = lock.lock().expect("lock");
            while guard.0 < target {
                guard = cvar.wait(guard).expect("wait");
            }
        })
        .await
        .expect("barrier");
    }

    // Start a Calendar query (Other slot, not cancelled by Search) while all 8 permits are busy.
    let eng_cal = engine.clone();
    let queued = tokio::spawn(async move { eng_cal.query(Query::Calendar).await });
    // Let the queued task reach `permits.acquire_owned().await`.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Update calendar and bump version while the Calendar query is queued on the semaphore.
    let fresh = Calendar {
        address: "city.test".into(),
        did: "did:zu:city".into(),
        events: vec![CityEvent {
            path: "zu://did:zu:org/ev/event/e1".into(),
            title: "Fresh".into(),
            start: Some(NOW),
        }],
    };
    engine.set_calendar(Some(fresh.clone()));
    let v_new = engine.invalidate(ViewKind::Calendar, None);

    // Release all 8 blocking Search workers so the queued Calendar query acquires a permit.
    {
        let (lock, cvar) = &*state;
        let mut guard = lock.lock().expect("lock");
        guard.1 = true;
        cvar.notify_all();
    }

    let reply = queued.await.expect("join queued").expect("calendar reply");
    for h in blockers {
        let _ = h.await;
    }

    assert!(
        reply.version < v_new
            || matches!(&reply.value, QueryValue::Calendar(Some(cal)) if cal.did == fresh.did && cal.events.len() == 1),
        "stale Calendar(None) must never be stamped with version {} >= v_new {}: reply={reply:?}",
        reply.version,
        v_new
    );
    assert_eq!(reply.version, v_new);
    assert!(
        matches!(&reply.value, QueryValue::Calendar(Some(cal)) if cal.did == fresh.did && cal.events.len() == 1),
        "queued Calendar query must observe fresh calendar for v_new={v_new}, got {:?}",
        reply.value
    );
    let cancelled = stats
        .tally()
        .counts
        .into_iter()
        .find(|c| c.name == "query.cancelled")
        .map(|c| c.n)
        .unwrap_or(0);
    assert!(
        cancelled >= (QUERY_CONCURRENT_MAX_JOBS - 1) as u64,
        "expected superseded search cancellations, got {cancelled}"
    );

    engine.set_hook(None);
    tree.cancel();
    tree.wait().await;
}

#[test]
fn index_v2_rebuilds_from_v1_and_searches_and_filters_tags() {
    use zutalk_core::record::shelf::{ContentTag, TitleBy, TitleCandidate};

    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = shelf_for(&dir.path().join("shelf"), ME, 5);
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");

    // Seed two recordings: one with 2-char Chinese tag ("押金") and >=3-char tag ("读书会讨论"),
    // and one with a different tag ("旅行").
    let about_a = About {
        started: NOW,
        length_ms: 45_000,
        title: "租房沟通".into(),
        title_by: TitleBy::Ai,
        tags: vec![
            ContentTag { label: "押金".into(), at_ms: 3_000 },
            ContentTag { label: "读书会讨论".into(), at_ms: 12_000 },
        ],
        topics: vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()],
        title_candidates: vec![TitleCandidate { by: TitleBy::Ai, text: "租房沟通".into() }],
        spoken: vec!["zh".into()],
        shown: vec!["zh".into()],
        source: "mic".into(),
        take: 1,
        ..About::default()
    };
    shelf.put("rec_tag_a", RECORDING, &[("rec_tag_a".into(), Some(about_a))], NOW).unwrap();
    let about_b = About {
        started: NOW + 1_000,
        length_ms: 20_000,
        title: "周末出行".into(),
        title_by: TitleBy::Hand,
        tags: vec![ContentTag { label: "旅行".into(), at_ms: 1_000 }],
        spoken: vec!["zh".into()],
        shown: vec!["zh".into()],
        source: "mic".into(),
        take: 1,
        ..About::default()
    };
    shelf.put("rec_tag_b", RECORDING, &[("rec_tag_b".into(), Some(about_b))], NOW + 1_000).unwrap();

    // Create a legacy v1 SQLite database on disk (user_version = 1) to verify automatic rebuild to v2.
    {
        let conn = rusqlite::Connection::open(&db).expect("create v1 db");
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta(key, value) VALUES ('by', 'did:zu:ewvi7nxzyoun6zhxrhs64oiz');
             PRAGMA user_version = 1;",
        )
        .expect("init v1");
    }

    assert_eq!(INDEX_SCHEMA_VERSION, 2);
    let stats = Stats::new();
    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("open & rebuild v1->v2");
    assert!(
        seen.of("index.open")
            .iter()
            .any(|l| l.text("mode") == Some("rebuilt") && l.text("why") == Some("version")),
        "expected v1->v2 automatic rebuild event"
    );

    // Search hits 2-char Chinese tag ("押金") via fallback AND >=3-char tag ("读书会讨论") via FTS5.
    let short_res = idx.search("押金", 50).expect("search short tag");
    assert!(
        short_res.hits.iter().any(|h| h.recording_id == "rec_tag_a"
            && h.kind == SearchHitKind::Tag
            && h.snippet == "押金"),
        "expected SearchHitKind::Tag for 2-char tag, got {:?}",
        short_res.hits
    );
    let long_res = idx.search("读书会讨论", 50).expect("search long tag");
    assert!(
        long_res.hits.iter().any(|h| h.recording_id == "rec_tag_a"
            && h.kind == SearchHitKind::Tag
            && h.snippet == "读书会讨论"),
        "expected SearchHitKind::Tag for >=3-char tag, got {:?}",
        long_res.hits
    );

    // LibraryFilter { tag: Some("押金") } filters strictly to rec_tag_a and preserves title_by/tags/topics.
    let filtered = idx
        .library_recent(
            &LibraryFilter { tag: Some("押金".into()), ..LibraryFilter::default() },
            50,
            0,
        )
        .expect("filter by tag");
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.rows.len(), 1);
    assert_eq!(filtered.rows[0].id, "rec_tag_a");
    assert_eq!(filtered.rows[0].title_by, TitleBy::Ai);
    assert_eq!(filtered.rows[0].tags.len(), 2);
    assert_eq!(filtered.rows[0].topics, vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()]);
}
