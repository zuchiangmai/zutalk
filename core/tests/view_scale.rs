//! Scale, I2, I3, and I8 tests for the local SQLite + FTS5 view layer (`P2-07`).

mod bench;
mod hub;

use bench::speech::{Speech, SpeechSpec};
use bench::{Bench, BenchOptions, STORE, TapeMode};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use zu::SecretKey;
use zutalk_core::app::proto::{LibraryFilter, Query, QueryValue, SearchHitKind, ViewKind};
use zutalk_core::live::lang::profile;
use zutalk_core::live::lines::Line;
use zutalk_core::record::shelf::{About, Draft, DraftWriter, LINE, RECORDING, Said, Shelf};
use zutalk_core::record::takes;
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::Tree;
use zutalk_core::view::index::{
    Index, LIBRARY_PAGE_MAX_ROWS, SEARCH_MAX_HITS, SEARCH_SNIPPET_MAX_BYTES,
};
use zutalk_core::view::query::{KEEP_INVALIDATIONS_EVENTS, QUERY_BUDGET_MS, ViewEngine};
use zutalk_core::view::transcript::TRANSCRIPT_CHUNK_LINES;
use zutalk_log::Level;

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const MAX_TALLY_BYTES: usize = 8 * 1024;
const MAX_DRAFT_WRITER_BYTES: usize = 256;

fn shelf_for(dir: &Path, did: &str, seed: u8) -> Shelf {
    Shelf::new(dir, did, Arc::new(SecretKey::from_seed(&[seed; 32])))
}

/// Howard Hinnant's exact civil-date to UTC epoch ms helper (12:00:00 UTC).
fn utc_noon_ms(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + 43_200) * 1_000
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

fn init_empty_index(db_path: &Path, shelf: &Shelf, tapes: &Path) {
    let stats = Stats::new();
    let idx = Index::open(db_path, shelf, tapes, &stats).expect("create schema index");
    drop(idx);
}

/// Seeds 1,000 recordings across 12 months (2025-01..2025-12), each with title, 20 multilingual
/// lines + translations, and 2 notes (~20,000 lines, ~43,000 search docs) in one SQLite tx.
///
/// When `touch_disk_dirs` is true, creates minimal recording dirs and records matching `about_sig`
/// and `lines_sig` so `Index::open` -> `reconcile` retains all 1,000 rows without reparsing.
fn seed_1000_recordings(db_path: &Path, shelf: &Shelf, tapes: &Path, touch_disk_dirs: bool) {
    init_empty_index(db_path, shelf, tapes);
    let mut conn = Connection::open(db_path).expect("open seeded sqlite");
    let tx = conn.transaction().expect("begin tx");

    let mut rec_stmt = tx
        .prepare(
            "INSERT INTO recordings (\
                id, started, length_ms, title, preview, spoken, shown, source, take, moved, audio, \
                filed_state, refine_error, refine_error_at, notes_count, about_sig, lines_sig, \
                title_by, tags_json, topics_json\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL, ?13, ?14, ?15, '\"None\"', '[]', '[]')",
        )
        .expect("prepare recordings");
    let mut line_stmt = tx
        .prepare(
            "INSERT INTO take_lines (\
                recording_id, take, seq, line_key, t0, t1, speaker, language, text, \
                translations_json, via_json, incomplete\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
        )
        .expect("prepare take_lines");
    let mut doc_stmt = tx
        .prepare(
            "INSERT INTO search_docs (recording_id, kind, item_key, lang, norm_text, snippet) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .expect("prepare search_docs");

    let via_json = r#"{"text":"stt-async-v6","en":"model"}"#;
    let spoken_sets = [r#"["zh","en"]"#, r#"["th","en"]"#, r#"["en","zh"]"#];

    for i in 0..1_000u32 {
        let id = format!("rec-{i:04}");
        let month = (i % 12) + 1;
        let day = ((i / 12) % 28) + 1;
        let started = utc_noon_ms(2025, month, day) + i64::from(i) * 10;
        let title = if i == 988 {
            "TitleGamma Chiang Mai Sync".to_owned()
        } else {
            format!("Workshop {i:04} Month {month:02}")
        };
        let preview = format!("Opening remarks for session {i:04}");
        let spoken = spoken_sets[((i / 12) % 3) as usize];
        let take = if (i / 12) % 3 == 0 { 0i64 } else { 1i64 };
        let audio = i64::from((i / 12) % 2 == 0);
        let filed_state = if (i / 12) % 5 == 0 { "city.filed" } else { "" };

        let (about_sig, lines_sig) = if touch_disk_dirs {
            let dir = shelf.root().join(&id);
            fs::create_dir_all(&dir).expect("create rec dir");
            let about_file = dir.join("about.jsonl");
            fs::write(&about_file, b"").expect("touch about.jsonl");
            (file_sig(&about_file), file_sig(&dir.join("lines.jsonl")))
        } else {
            ("0:0".to_owned(), "0:0".to_owned())
        };

        rec_stmt
            .execute(params![
                &id,
                started,
                180_000i64,
                &title,
                &preview,
                spoken,
                spoken,
                "mic",
                take,
                0i64,
                audio,
                filed_state,
                2i64,
                about_sig,
                lines_sig,
            ])
            .expect("insert recording");

        let title_norm = profile("").key(&title);
        doc_stmt
            .execute(params![
                &id,
                "title",
                &id,
                "",
                title_norm,
                truncate_utf8(&title, SEARCH_SNIPPET_MAX_BYTES)
            ])
            .expect("insert title doc");

        for seq in 0..20u32 {
            let line_key = format!("{id}.{take}.{seq:05}");
            let (lang, text, tr_lang, tr_text) = if i == 17 && seq == 3 {
                (
                    "zh",
                    "今天在清迈读书会讨论了本地检索和索引架构".to_owned(),
                    "en",
                    "Today the Chiang Mai reading group discussed local search".to_owned(),
                )
            } else if i == 17 && seq == 7 {
                (
                    "zh",
                    "退房时押金会原路退回给租客".to_owned(),
                    "en",
                    "The deposit is refunded upon checkout".to_owned(),
                )
            } else if i == 513 && seq == 4 {
                (
                    "th",
                    "ตรวจน\u{e49}\u{e33}หนักพิเศษก่อนออกเดินทาง".to_owned(),
                    "en",
                    "Check special weight before departure".to_owned(),
                )
            } else if i == 513 && seq == 11 {
                (
                    "en",
                    "Standard multilingual conversation segment".to_owned(),
                    "en",
                    "ZuzaluQuarterly chapter retrospective and budget summary".to_owned(),
                )
            } else {
                match (i + seq) % 3 {
                    0 => (
                        "zh",
                        format!("第{i}场第{seq}句社区讨论与日常记录分享"),
                        "en",
                        format!("Community discussion line {seq} in recording {i}"),
                    ),
                    1 => (
                        "th",
                        format!("บันทึกการประชุมที่ {i} บรรทัดที่ {seq} สำหรับชุมชน"),
                        "en",
                        format!("Meeting note line {seq} of recording {i}"),
                    ),
                    _ => (
                        "en",
                        format!("Session {i} segment {seq} about collaborative software"),
                        "zh",
                        format!("第{i}场第{seq}段协作软件工程讨论"),
                    ),
                }
            };

            let tr_json = format!(r#"{{"{tr_lang}":"{tr_text}"}}"#);
            let t0 = i64::from(seq * 5_000);
            let t1 = t0 + 4_500;
            line_stmt
                .execute(params![
                    &id,
                    take,
                    i64::from(seq),
                    &line_key,
                    t0,
                    t1,
                    "1",
                    lang,
                    &text,
                    &tr_json,
                    via_json,
                ])
                .expect("insert take_line");

            let norm_line = profile(lang).key(&text);
            doc_stmt
                .execute(params![
                    &id,
                    "line",
                    &line_key,
                    lang,
                    norm_line,
                    truncate_utf8(&text, SEARCH_SNIPPET_MAX_BYTES),
                ])
                .expect("insert line doc");

            let norm_tr = profile(tr_lang).key(&tr_text);
            doc_stmt
                .execute(params![
                    &id,
                    "translation",
                    &line_key,
                    tr_lang,
                    norm_tr,
                    truncate_utf8(&tr_text, SEARCH_SNIPPET_MAX_BYTES),
                ])
                .expect("insert translation doc");
        }

        for n in 0..2u32 {
            let note_key = format!("note-{i:04}-{n}");
            let note_text = if i == 988 && n == 0 {
                "Action item: confirm NoteAnchorBeta schedule with everyone".to_owned()
            } else {
                format!("Memo {n} recorded during session {i:04}")
            };
            let norm_note = profile("").key(&note_text);
            doc_stmt
                .execute(params![
                    &id,
                    "note",
                    &note_key,
                    "",
                    norm_note,
                    truncate_utf8(&note_text, SEARCH_SNIPPET_MAX_BYTES),
                ])
                .expect("insert note doc");
        }
    }

    drop(rec_stmt);
    drop(line_stmt);
    drop(doc_stmt);
    tx.commit().expect("commit 1000 recordings");
}

/// Seeds one recording with two takes of `line_count` lines each (every 10th line edited in take 1).
fn seed_two_take_recording(
    conn: &mut Connection,
    id: &str,
    started: i64,
    length_ms: u32,
    line_count: u32,
) {
    let tx = conn.transaction().expect("begin two-take tx");
    tx.execute(
        "INSERT INTO recordings (\
            id, started, length_ms, title, preview, spoken, shown, source, take, moved, audio, \
            filed_state, refine_error, refine_error_at, notes_count, about_sig, lines_sig, \
            title_by, tags_json, topics_json\
         ) VALUES (?1, ?2, ?3, ?4, ?5, '[\"en\",\"zh\"]', '[\"en\",\"zh\"]', 'mic', 1, 0, 1, '', \
                   NULL, NULL, 0, '0:0', '0:0', '\"None\"', '[]', '[]')",
        params![id, started, i64::from(length_ms), format!("Long talk {id}"), "Line 0"],
    )
    .expect("insert long recording");

    let mut line_stmt = tx
        .prepare(
            "INSERT INTO take_lines (\
                recording_id, take, seq, line_key, t0, t1, speaker, language, text, \
                translations_json, via_json, incomplete\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'en', ?8, '{}', '{\"text\":\"stt\"}', 0)",
        )
        .expect("prepare take_lines");

    let step_ms = length_ms.checked_div(line_count).map_or(5_000, |s| s.max(1_000));
    for seq in 0..line_count {
        let t0 = i64::from(seq.saturating_mul(step_ms));
        let t1 = t0 + i64::from(step_ms.saturating_sub(200).max(500));
        let spk = if seq % 2 == 0 { "1" } else { "2" };
        let draft_text =
            format!("Live transcript utterance number {seq:05} discussing architecture.");
        let refined_text = if seq % 10 == 0 {
            format!(
                "Refined transcript utterance number {seq:05} discussing verified architecture!"
            )
        } else {
            draft_text.clone()
        };

        line_stmt
            .execute(params![
                id,
                0i64,
                i64::from(seq),
                format!("{id}.0.{seq:05}"),
                t0,
                t1,
                spk,
                &draft_text,
            ])
            .expect("insert take 0 line");
        line_stmt
            .execute(params![
                id,
                1i64,
                i64::from(seq),
                format!("{id}.1.{seq:05}"),
                t0,
                t1,
                spk,
                &refined_text,
            ])
            .expect("insert take 1 line");
    }

    drop(line_stmt);
    tx.commit().expect("commit two-take recording");
}

fn said(t0: u32, t1: u32, lang: &str, text: &str) -> Said {
    Said {
        line: Line {
            t0,
            t1,
            speaker: "1".into(),
            language: lang.into(),
            text: text.into(),
            translations: BTreeMap::from([("en".into(), format!("en:{text}"))]),
            incomplete: false,
        },
        via: BTreeMap::from([("text".into(), "stt".into())]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scale_1000_recordings_library_and_fts5_search_under_100ms() {
    let refused_before = zutalk_log::refused();
    let seen = zutalk_log::capture(Level::Info);

    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = Arc::new(shelf_for(&dir.path().join("records"), ME, 1));
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");

    let seed_start = Instant::now();
    seed_1000_recordings(&db, &shelf, &tapes, false);
    let seed_ms = seed_start.elapsed().as_millis();

    let stats = Stats::new();
    let tree = Tree::root("scale-1000", stats);
    let (engine, _inv) = ViewEngine::new(tree.clone(), tapes);
    engine.set_source(Some(shelf), Some(db), true, true);

    // 1. LibrarySummary across all 12 months (UTC+8).
    let t0 = Instant::now();
    let summary = match engine
        .query(Query::LibrarySummary { tz_offset_min: 480 })
        .await
        .expect("library summary")
        .value
    {
        QueryValue::LibrarySummary(s) => s,
        other => panic!("unexpected reply: {other:?}"),
    };
    let summary_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(summary.total, 1_000);
    assert_eq!(summary.months.len(), 12);
    assert_eq!(summary.months.iter().map(|m| m.count).sum::<u32>(), 1_000);
    assert!(
        summary_ms <= QUERY_BUDGET_MS as f64,
        "LibrarySummary took {summary_ms:.2} ms > {QUERY_BUDGET_MS} ms"
    );

    // 2. LibraryMonth (page 2 + filtered).
    let t0 = Instant::now();
    let month_p2 = match engine
        .query(Query::LibraryMonth {
            month: "2025-06".into(),
            tz_offset_min: 480,
            filter: LibraryFilter::default(),
            limit: 20,
            offset: 20,
        })
        .await
        .expect("library month p2")
        .value
    {
        QueryValue::LibraryPage(p) => p,
        other => panic!("unexpected reply: {other:?}"),
    };
    let month_p2_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(month_p2.rows.len(), 20);
    assert!(month_p2.total >= 80);
    assert!(month_p2.has_more);
    assert!(
        month_p2_ms <= QUERY_BUDGET_MS as f64,
        "LibraryMonth p2 took {month_p2_ms:.2} ms > {QUERY_BUDGET_MS} ms"
    );

    let t0 = Instant::now();
    let month_filtered = match engine
        .query(Query::LibraryMonth {
            month: "2025-06".into(),
            tz_offset_min: 480,
            filter: LibraryFilter {
                draft_only: true,
                audio_only: true,
                filed_only: false,
                language: None,
                tag: None,
            },
            limit: 50,
            offset: 0,
        })
        .await
        .expect("library month filtered")
        .value
    {
        QueryValue::LibraryPage(p) => p,
        other => panic!("unexpected reply: {other:?}"),
    };
    let month_filt_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    assert!(!month_filtered.rows.is_empty());
    assert!(month_filtered.rows.iter().all(|r| r.draft && r.audio));
    assert!(
        month_filt_ms <= QUERY_BUDGET_MS as f64,
        "LibraryMonth filtered took {month_filt_ms:.2} ms > {QUERY_BUDGET_MS} ms"
    );

    // 3. LibraryRecent (50 newest rows).
    let t0 = Instant::now();
    let recent = match engine
        .query(Query::LibraryRecent { filter: LibraryFilter::default(), limit: 50, offset: 0 })
        .await
        .expect("library recent")
        .value
    {
        QueryValue::LibraryPage(p) => p,
        other => panic!("unexpected reply: {other:?}"),
    };
    let recent_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(recent.rows.len(), 50);
    assert_eq!(recent.total, 1_000);
    assert!(recent.has_more);
    assert!(recent.rows.windows(2).all(|w| w[0].started >= w[1].started));
    assert!(
        recent_ms <= QUERY_BUDGET_MS as f64,
        "LibraryRecent took {recent_ms:.2} ms > {QUERY_BUDGET_MS} ms"
    );

    // 4. Six Search queries covering CJK 4-char FTS5, CJK 2-char short fallback, Thai SARA AM
    // variant, case-insensitive English translation, note, and title.
    let searches: &[(&str, &str, &str, SearchHitKind)] = &[
        ("cjk4", "清迈读书", "rec-0017", SearchHitKind::Line),
        ("cjk2", "押金", "rec-0017", SearchHitKind::Line),
        ("thai_sara_am", "น\u{e4d}\u{e49}\u{e32}หนักพิเศษ", "rec-0513", SearchHitKind::Line),
        ("en_case", "zuzaluquarterly", "rec-0513", SearchHitKind::Translation),
        ("note", "NoteAnchorBeta", "rec-0988", SearchHitKind::Note),
        ("title", "TitleGamma", "rec-0988", SearchHitKind::Title),
    ];

    let mut search_timings = Vec::new();
    for &(label, query_text, expected_rec, expected_kind) in searches {
        let t0 = Instant::now();
        let res = match engine
            .query(Query::Search { query: query_text.into(), limit: 50 })
            .await
            .expect("search")
            .value
        {
            QueryValue::Search(s) => s,
            other => panic!("unexpected reply for {label}: {other:?}"),
        };
        let ms = t0.elapsed().as_secs_f64() * 1_000.0;
        assert!(
            res.hits.iter().any(|h| h.recording_id == expected_rec && h.kind == expected_kind),
            "search {label} missed ({expected_rec}, {expected_kind:?}); got {:?}",
            res.hits
        );
        assert!(
            ms <= QUERY_BUDGET_MS as f64,
            "Search[{label}] took {ms:.2} ms > {QUERY_BUDGET_MS} ms"
        );
        search_timings.push(format!("{label}={ms:.2}ms"));
    }

    assert_eq!(seen.of("query.slow").len(), 0, "no query.slow events expected");
    assert_eq!(zutalk_log::refused(), refused_before, "zero log fields refused");

    eprintln!(
        "scale_1000: seed={seed_ms}ms summary={summary_ms:.2}ms month_p2={month_p2_ms:.2}ms \
         month_filt={month_filt_ms:.2}ms recent={recent_ms:.2}ms searches=[{}]",
        search_timings.join(", ")
    );

    tree.cancel();
    tree.wait().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scale_10_hour_transcript_chunk_and_compare_under_100ms() {
    let refused_before = zutalk_log::refused();
    let seen = zutalk_log::capture(Level::Info);

    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = Arc::new(shelf_for(&dir.path().join("records"), ME, 2));
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");

    init_empty_index(&db, &shelf, &tapes);
    let seed_start = Instant::now();
    {
        let mut conn = Connection::open(&db).expect("open db");
        seed_two_take_recording(&mut conn, "rec-10h", utc_noon_ms(2025, 4, 10), 36_000_000, 6_400);
    }
    let seed_ms = seed_start.elapsed().as_millis();

    let stats = Stats::new();
    let tree = Tree::root("scale-10h", stats);
    let (engine, _inv) = ViewEngine::new(tree.clone(), tapes);
    engine.set_source(Some(shelf), Some(db), true, true);

    let mut tr_timings = Vec::new();

    for chunk in [0u32, 50, 99] {
        let t0 = Instant::now();
        let tr = match engine
            .query(Query::Transcript { id: "rec-10h".into(), take: Some(0), chunk })
            .await
            .expect("transcript chunk")
            .value
        {
            QueryValue::Transcript(c) => c,
            other => panic!("unexpected reply: {other:?}"),
        };
        let tr_ms = t0.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(tr.lines.len(), TRANSCRIPT_CHUNK_LINES);
        assert_eq!(tr.total_lines, 6_400);
        assert_eq!(tr.total_chunks, 100);
        assert!(
            tr_ms <= QUERY_BUDGET_MS as f64,
            "Transcript chunk {chunk} took {tr_ms:.2} ms > {QUERY_BUDGET_MS} ms"
        );
        tr_timings.push(format!("c{chunk}={tr_ms:.2}ms"));
    }

    assert_eq!(seen.of("query.slow").len(), 0, "no query.slow events expected");
    assert_eq!(zutalk_log::refused(), refused_before, "zero log fields refused");

    eprintln!("scale_10h: seed={seed_ms}ms transcript=[{}]", tr_timings.join(", "));

    tree.cancel();
    tree.wait().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = Arc::new(shelf_for(&dir.path().join("records"), ME, 3));
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");

    init_empty_index(&db, &shelf, &tapes);
    {
        let mut conn = Connection::open(&db).expect("open db");
        seed_two_take_recording(&mut conn, "rec-1m", utc_noon_ms(2025, 5, 1), 60_000, 12);
        seed_two_take_recording(&mut conn, "rec-4h", utc_noon_ms(2025, 5, 2), 14_400_000, 2_560);
    }

    let stats = Stats::new();
    let tree = Tree::root("i8-chunk", stats);
    let (engine, _inv) = ViewEngine::new(tree.clone(), tapes);
    engine.set_source(Some(shelf), Some(db), true, true);

    // Measure 1-minute recording (12 lines) vs 4-hour recording (2,560 lines = 40 chunks).
    let t0 = Instant::now();
    let tr_1m = engine
        .query(Query::Transcript { id: "rec-1m".into(), take: Some(0), chunk: 0 })
        .await
        .expect("tr 1m");
    let tr_1m_us = t0.elapsed().as_micros();

    let t0 = Instant::now();
    let tr_4h_first = engine
        .query(Query::Transcript { id: "rec-4h".into(), take: Some(0), chunk: 0 })
        .await
        .expect("tr 4h first");
    let tr_4h_first_us = t0.elapsed().as_micros();

    let t0 = Instant::now();
    let tr_4h_last = engine
        .query(Query::Transcript { id: "rec-4h".into(), take: Some(0), chunk: 39 })
        .await
        .expect("tr 4h last");
    let tr_4h_last_us = t0.elapsed().as_micros();

    let tr_4h_chunk_bytes = serde_json::to_vec(&tr_4h_last).expect("json").len();
    let estimated_full_4h_bytes = tr_4h_chunk_bytes * 40;

    for reply in [&tr_1m, &tr_4h_first, &tr_4h_last] {
        match &reply.value {
            QueryValue::Transcript(c) => assert!(c.lines.len() <= TRANSCRIPT_CHUNK_LINES),
            other => panic!("expected Transcript, got {other:?}"),
        }
    }

    assert!(
        tr_4h_chunk_bytes < estimated_full_4h_bytes / 10,
        "chunk size must stay bounded relative to full 4h transcript"
    );
    for (name, us) in
        [("tr_1m", tr_1m_us), ("tr_4h_first", tr_4h_first_us), ("tr_4h_last", tr_4h_last_us)]
    {
        assert!(
            us <= u128::from(QUERY_BUDGET_MS) * 1_000,
            "{name} took {us}us > {QUERY_BUDGET_MS}ms"
        );
    }

    let tr_ratio = tr_4h_last_us as f64 / tr_1m_us.max(1) as f64;
    eprintln!(
        "i8_chunk: tr_1m={tr_1m_us}us tr_4h_c0={tr_4h_first_us}us tr_4h_c39={tr_4h_last_us}us \
         tr_ratio={tr_ratio:.2} | tr_chunk_bytes={tr_4h_chunk_bytes}"
    );

    tree.cancel();
    tree.wait().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i3_rebuild_queries_and_draft_stay_within_bounds() {
    let refused_before = zutalk_log::refused();
    let seen = zutalk_log::capture(Level::Info);

    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = Arc::new(shelf_for(&dir.path().join("records"), ME, 4));
    let tapes = dir.path().join("tapes");
    let db = dir.path().join("index.sqlite3");

    // 1. Write 100 real signed recordings via Shelf, then corrupt the SQLite DB and verify
    // streaming automatic rebuild (`index.open(mode = "rebuilt", why = "corrupt")`).
    for i in 0..100u32 {
        let id = format!("real-{i:03}");
        let started = utc_noon_ms(2025, (i % 12) + 1, ((i / 12) % 28) + 1) + i64::from(i);
        let about = About {
            started,
            length_ms: 30_000,
            title: format!("Real Recording {i:03}"),
            preview: format!("Preview line {i:03}"),
            spoken: vec!["en".into(), "zh".into()],
            shown: vec!["en".into(), "zh".into()],
            source: "mic".into(),
            take: 0,
            moved: false,
            ..About::default()
        };
        shelf.put(&id, RECORDING, &[(id.clone(), Some(about))], started).expect("write about");
        let lines = vec![
            (format!("{id}.0.00000"), Some(said(0, 2_000, "en", &format!("Hello topic {i}")))),
            (format!("{id}.0.00001"), Some(said(2_000, 4_000, "zh", &format!("中文第{i}条")))),
        ];
        shelf.put(&id, LINE, &lines, started + 1).expect("write lines");
    }

    // Corrupt SQLite file on disk before Index::open.
    fs::write(&db, b"corrupted-not-a-sqlite-header-payload").expect("write corrupt db");

    let stats = Stats::new();
    let rebuild_start = Instant::now();
    let idx = Index::open(&db, &shelf, &tapes, &stats).expect("rebuild corrupt db");
    let rebuild_ms = rebuild_start.elapsed().as_millis();
    drop(idx);

    let opens = seen.of("index.open");
    assert!(
        opens.iter().any(|e| e.text("mode") == Some("rebuilt") && e.text("why") == Some("corrupt")),
        "expected index.open(mode=rebuilt, why=corrupt), got {opens:?}"
    );
    assert_eq!(
        stats.tally().counts.iter().find(|c| c.name == "index.rebuilds").map(|c| c.n),
        Some(1)
    );

    // 2. Run 50 mixed queries (including concurrent overlapping searches) + 200 invalidations.
    let tree = Tree::root("i3-bounds", stats.clone());
    let (engine, inv_reading) = ViewEngine::new(tree.clone(), tapes);
    engine.set_source(Some(shelf.clone()), Some(db), true, true);

    let mut concurrent_searches = Vec::new();
    for i in 0..10u32 {
        let eng = engine.clone();
        concurrent_searches.push(tokio::spawn(async move {
            eng.query(Query::Search { query: format!("topic {i}"), limit: 50 }).await
        }));
    }
    for handle in concurrent_searches {
        match handle.await.expect("join search") {
            Ok(reply) => {
                if let QueryValue::Search(s) = reply.value {
                    assert!(s.hits.len() <= SEARCH_MAX_HITS as usize);
                }
            }
            Err(code) => assert_eq!(code, "query.cancelled"),
        }
    }

    for i in 0..40u32 {
        let q = match i % 4 {
            0 => Query::LibrarySummary { tz_offset_min: 480 },
            1 => Query::LibraryRecent { filter: LibraryFilter::default(), limit: 50, offset: 0 },
            2 => Query::Search { query: format!("中文第{}", i % 10), limit: 50 },
            _ => Query::Transcript { id: format!("real-{:03}", i % 100), take: Some(0), chunk: 0 },
        };
        let reply = engine.query(q).await.expect("sequential query");
        match reply.value {
            QueryValue::LibraryPage(p) => assert!(p.rows.len() <= LIBRARY_PAGE_MAX_ROWS as usize),
            QueryValue::Search(s) => assert!(s.hits.len() <= SEARCH_MAX_HITS as usize),
            QueryValue::Transcript(c) => assert!(c.lines.len() <= TRANSCRIPT_CHUNK_LINES),
            QueryValue::LibrarySummary(s) => assert_eq!(s.total, 100),
            _ => {}
        }
    }

    for i in 0..200u32 {
        engine.invalidate(ViewKind::Recording, Some(format!("real-{i:03}")));
    }

    // Give any cancelled blocking task a brief moment to settle back to 0 running tasks.
    for _ in 0..50 {
        if tree.running().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let (_, inv) = inv_reading.now();
    assert!(
        inv.items.len() <= KEEP_INVALIDATIONS_EVENTS,
        "invalidations ring exceeded cap: {}",
        inv.items.len()
    );
    assert_eq!(inv.next, 200);

    let tally = stats.tally();
    let tally_bytes = serde_json::to_vec(&tally).expect("serialize tally").len();
    assert_eq!(seen.of("stats.full").len(), 0, "stats.full must be 0");
    assert!(
        tally_bytes <= MAX_TALLY_BYTES,
        "Tally JSON size {tally_bytes} B > {MAX_TALLY_BYTES} B"
    );
    assert!(
        tally.queues.iter().all(|q| q.depth == 0),
        "all queue depths must return to 0: {:?}",
        tally.queues
    );
    assert!(tree.running().is_empty(), "tree tasks must return to baseline: {:?}", tree.running());

    // 3. DraftWriter: O(1) writer struct size and 10,000 lines written cleanly & readable back.
    let draft_writer_bytes = std::mem::size_of::<DraftWriter>();
    assert!(
        draft_writer_bytes <= MAX_DRAFT_WRITER_BYTES,
        "DraftWriter size {draft_writer_bytes} B exceeds O(1) budget {MAX_DRAFT_WRITER_BYTES} B"
    );
    let draft_id = "draft-10k";
    let draft_start = Instant::now();
    let base_at = utc_noon_ms(2025, 6, 1);
    let mut draft = shelf.open_draft(draft_id).expect("open draft");
    for n in 0..10_000u32 {
        let entry = said(n * 1_000, n * 1_000 + 800, "en", &format!("draft line {n}"));
        draft
            .put(&format!("{draft_id}.0.{n:05}"), &entry, base_at + i64::from(n))
            .expect("write draft line");
    }
    draft.sync().expect("sync draft");
    drop(draft);
    let draft_ms = draft_start.elapsed().as_millis();

    let read_back = takes::read(&shelf, draft_id, 0).expect("read 10k draft");
    assert_eq!(read_back.len(), 10_000);
    assert_eq!(read_back[0].1.line.text, "draft line 0");
    assert_eq!(read_back[9_999].1.line.text, "draft line 9999");

    assert_eq!(zutalk_log::refused(), refused_before, "zero log fields refused");

    eprintln!(
        "i3_bounds: rebuild_100={rebuild_ms}ms tally_bytes={tally_bytes}B \
         draft_writer={draft_writer_bytes}B draft_10k_write={draft_ms}ms inv_len={}",
        inv.items.len()
    );

    tree.cancel();
    tree.wait().await;
}

#[tokio::test(start_paused = true)]
async fn i2_concurrent_view_queries_do_not_stall_live_captions() {
    let _store = STORE.lock().await;

    // Pre-seed 1,000 recordings on the device's local shelf + matching SQLite index so Bench::start
    // boots with a 1,000-recording library immediately ready to query during live recording.
    let seeded_dir = tempfile::tempdir().expect("seeded data dir");
    let data: PathBuf = seeded_dir.path().to_path_buf();
    let local_shelf = hub::local_shelf(&data);
    let tapes = data.join("tape");
    let db_path =
        data.join("index").join(format!("{}.sqlite3", local_shelf.by().replace(':', "-")));
    seed_1000_recordings(&db_path, &local_shelf, &tapes, true);

    let speech = Speech::build(SpeechSpec::standard(16_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;
    opts.data = Some(data);

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;

    // Confirm the 1,000 pre-seeded recordings are immediately queryable.
    let before = match bench
        .query(Query::LibrarySummary { tz_offset_min: 480 })
        .await
        .expect("initial summary")
        .value
    {
        QueryValue::LibrarySummary(s) => s,
        other => panic!("unexpected summary: {other:?}"),
    };
    assert_eq!(before.total, 1_000);

    let rec_id = bench.start_recording().await;
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let frames_before = bench.frames();

    // Concurrently fire 20 mixed L3 queries across Search, LibraryMonth, LibrarySummary,
    // Transcript, and Recording while the live recording advances virtual time.
    let mut ok_count = 0usize;
    let mut cancelled_count = 0usize;
    for round in 0..4u32 {
        let q_search = bench.query(Query::Search {
            query: if round % 2 == 0 { "清迈读书".into() } else { "zuzaluquarterly".into() },
            limit: 50,
        });
        let q_month = bench.query(Query::LibraryMonth {
            month: format!("2025-{:02}", (round % 12) + 1),
            tz_offset_min: 480,
            filter: LibraryFilter::default(),
            limit: 50,
            offset: 0,
        });
        let q_summary = bench.query(Query::LibrarySummary { tz_offset_min: 480 });
        let q_transcript = bench.query(Query::Transcript {
            id: format!("rec-{:04}", round * 100),
            take: None,
            chunk: 0,
        });
        let q_recording = bench.query(Query::Recording { id: format!("rec-{:04}", round * 100) });

        let (r1, r2, r3, r4, r5, ()) = tokio::join!(
            q_search,
            q_month,
            q_summary,
            q_transcript,
            q_recording,
            tokio::time::sleep(Duration::from_millis(2_000)),
        );
        for res in [r1, r2, r3, r4, r5] {
            match res {
                Ok(_) => ok_count += 1,
                Err("query.cancelled") => cancelled_count += 1,
                Err(other) => panic!("unexpected query error during live recording: {other}"),
            }
        }
    }

    let frames_after = bench.frames();
    assert!(
        frames_after > frames_before + 10,
        "live caption frames must keep advancing during concurrent queries ({frames_before} -> {frames_after})"
    );
    assert!(
        ok_count >= 15,
        "expected most concurrent queries to succeed (ok={ok_count}, cancelled={cancelled_count})"
    );
    let stalled_or_dead = bench
        .seen()
        .of("judge.verdict")
        .into_iter()
        .filter(|e| matches!(e.text("to"), Some("Stalled" | "Dead")))
        .count();
    assert_eq!(stalled_or_dead, 0, "concurrent SQLite queries must never stall live captions");
    assert_eq!(
        bench.seen().count("stall.snapshot"),
        0,
        "concurrent SQLite queries must never trigger a stall snapshot"
    );

    bench.stop_recording().await;

    // Wait briefly for the post-seal index sync to commit, then verify the new recording is
    // immediately present in Core::query.
    let mut found_new = false;
    for _ in 0..40 {
        if let Ok(reply) = bench.query(Query::Recording { id: rec_id.clone() }).await
            && matches!(reply.value, QueryValue::Recording(Some(_)))
        {
            found_new = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found_new, "newly stopped recording {rec_id} must appear in Core::query");

    let after = match bench
        .query(Query::LibrarySummary { tz_offset_min: 480 })
        .await
        .expect("post-stop summary")
        .value
    {
        QueryValue::LibrarySummary(s) => s,
        other => panic!("unexpected summary: {other:?}"),
    };
    assert_eq!(after.total, 1_001);

    eprintln!(
        "i2_live_queries: ok={ok_count}/20 cancelled={cancelled_count} \
         frames={frames_before}->{frames_after} total_after={}",
        after.total
    );

    bench.finish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn surgical_edits_on_real_4_hour_zu_pack_avoid_full_reparse_and_preserve_all_takes() {
    use zutalk_core::app::proto::{Change, Command, NoteOp};

    let _store = hub::STORE.lock().await;
    hub::signed_in();
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = hub::shelf(dir.path());

    // Seed a real 4-hour, 2-take Zu pack (2,560 lines × 2 takes = 5,120 signed records in lines.jsonl).
    let rec_id = "19c00000000fourhour";
    let started = utc_noon_ms(2025, 11, 1);
    let mut take0 = Vec::with_capacity(2_560);
    let mut take1 = Vec::with_capacity(2_560);
    for seq in 0..2_560u32 {
        let t0 = seq * 5_600;
        let t1 = t0 + 4_800;
        take0.push((
            format!("{rec_id}.0.{seq:05}"),
            Some(said(t0, t1, "en", &format!("Draft utterance {seq:05} in long town hall"))),
        ));
        take1.push((
            format!("{rec_id}.1.{seq:05}"),
            Some(said(t0, t1, "en", &format!("Refined utterance {seq:05} in long town hall"))),
        ));
    }
    shelf.put(rec_id, LINE, &take0, started + 1).expect("seed take 0");
    shelf.put(rec_id, LINE, &take1, started + 2).expect("seed take 1");
    shelf
        .put(
            rec_id,
            RECORDING,
            &[(
                rec_id.into(),
                Some(About {
                    started,
                    length_ms: 14_400_000,
                    title: "Initial 4h Town Hall".into(),
                    preview: "Refined utterance 00000".into(),
                    spoken: vec!["en".into()],
                    shown: vec!["en".into(), "zh".into()],
                    source: "mic".into(),
                    take: 1,
                    ..About::default()
                }),
            )],
            started + 3,
        )
        .expect("seed about");

    let mut app = hub::App::start(dir.path());
    app.until(|s| s.me.as_deref() == Some(ME)).await;
    app.wait_recording(rec_id, |v| v.is_some()).await;

    // 1. Add note -> surgical Index::update_notes (never re-reads 5,120 lines.jsonl records).
    let t_note = Instant::now();
    app.send(Command::Note {
        id: rec_id.into(),
        op: NoteOp::Add {
            at_ms: 120_000,
            text: "SurgicalNoteLandmark".into(),
            anchor: Some(format!("{rec_id}.1.00021")),
        },
    });
    app.wait_recording(rec_id, |v| v.is_some_and(|r| r.notes.len() == 1)).await;
    let note_ms = t_note.elapsed().as_millis();

    // 2. Rename title -> surgical Index::update_meta.
    let t_title = Instant::now();
    app.send(Command::Change {
        id: rec_id.into(),
        change: Change::Title { text: "SurgicalTitleLandmark".into() },
    });
    app.wait_library(|rows| {
        rows.iter().any(|r| r.id == rec_id && r.title == "SurgicalTitleLandmark")
    })
    .await;
    let title_ms = t_title.elapsed().as_millis();

    // 3. Rename speaker -> O(1) Recording invalidation without touching SQLite take_lines.
    let t_spk = Instant::now();
    app.send(Command::Change {
        id: rec_id.into(),
        change: Change::Speaker { label: "1".into(), name: "Moderator".into() },
    });
    app.wait_recording(rec_id, |v| {
        v.is_some_and(|r| r.speakers.get("1").map(String::as_str) == Some("Moderator"))
    })
    .await;
    let spk_ms = t_spk.elapsed().as_millis();

    // 4. Edit last line (#2559) -> surgical Index::update_line + FTS5 doc update.
    let last_key = format!("{rec_id}.1.02559");
    let t_line = Instant::now();
    app.send(Command::Change {
        id: rec_id.into(),
        change: Change::Line {
            key: last_key.clone(),
            language: "en".into(),
            text: "SurgicalTailCorrectionLandmark".into(),
        },
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let c39 = app.transcript(rec_id, Some(1), 39).await;
        if c39.lines.last().is_some_and(|(_, s)| s.line.text == "SurgicalTailCorrectionLandmark") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "tail line update did not propagate");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let line_ms = t_line.elapsed().as_millis();

    // All 4 mutations stay sub-100ms and preserve all 2,560 lines across both takes + FTS5 search!
    assert!(note_ms <= 80, "Note took {note_ms} ms on 4h pack");
    assert!(title_ms <= 80, "Title took {title_ms} ms on 4h pack");
    assert!(spk_ms <= 80, "Speaker took {spk_ms} ms on 4h pack");
    assert!(line_ms <= 80, "Line edit took {line_ms} ms on 4h pack");

    let c0_t0 = app.transcript(rec_id, Some(0), 0).await;
    let c0_t1 = app.transcript(rec_id, Some(1), 0).await;
    assert_eq!(c0_t0.total_lines, 2_560);
    assert_eq!(c0_t1.total_lines, 2_560);

    let hits = app.search("SurgicalTailCorrectionLandmark").await.hits;
    assert!(hits.iter().any(|h| h.recording_id == rec_id && h.key == last_key));

    eprintln!(
        "surgical_4h_e2e: note={note_ms}ms title={title_ms}ms speaker={spk_ms}ms line={line_ms}ms"
    );
}
