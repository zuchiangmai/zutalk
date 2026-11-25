use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use zu::SecretKey;
use zutalk_core::ai::write;
use zutalk_core::capture::clock::{BLOCK, Block};
use zutalk_core::capture::tape;
use zutalk_core::live::lines::Line;
use zutalk_core::out::city::Filing;
use zutalk_core::record::notes;
use zutalk_core::record::shelf::{About, FILING, LINE, RECORDING, Said, Shelf};
use zutalk_core::record::takes::RefineFailure;
use zutalk_core::rt::stats::Stats;
use zutalk_core::view::index::{
    INDEX_SCHEMA_VERSION, Index, IndexHealth, LibraryFilter, OpenPlan, Reader, SearchHitKind,
    open_plan,
};

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
