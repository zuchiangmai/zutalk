//! Liveness, isolation, recovery, and log-privacy tests over the full-core simulation bench
//! (`docs/build/phases/P1.md` §2, §3.10, §3.13.13–§3.13.14, `P1-08-bench.md`).

mod bench;
mod common;
mod hub;

use bench::mic::encode_block_index;
use bench::service::{Delays, FINAL_DELAY_MS, Fault, GUESS_DELAY_MS, SILENCE_PROGRESS_MS, Service};
use bench::speech::{BLOCK_MS, SPEAKERS, Speech, SpeechSpec};
use bench::{
    Bench, BenchOptions, MAX_FRAME_BYTES, STORE, TapeMode, check_bundle_dir, check_log_lines,
    summarize_latencies,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as WallInstant};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use zutalk_core::app::proto::{Change, Command, Device, Setup, Status};
use zutalk_core::app::work::{DIAGNOSE_KEPT_DIRS, Facts};
use zutalk_core::capture::clock::BLOCK;
use zutalk_core::capture::tape::{self, Pass};
use zutalk_core::id::keys;
use zutalk_core::live::lane::{self, Setup as LaneSetup, Stream};
use zutalk_core::live::lines::Line;
use zutalk_core::live::wire::{Dial, Wire};
use zutalk_core::record::shelf::{
    About, DraftGap, DraftStep, GapSpan, LINE, Missed, RECORDING, Said, Shelf,
};
use zutalk_core::rt::supervise::End;
use zutalk_core::rt::water::{Health, Stage};
use zutalk_log::{Level, Limits};

/// 8 virtual hours in milliseconds for I1.
const EIGHT_HOURS_MS: u32 = 8 * 60 * 60 * 1_000;
/// 30 virtual minutes in milliseconds for 3-language I1.
const THIRTY_MINUTES_MS: u32 = 30 * 60 * 1_000;
/// 1 virtual hour in milliseconds for bucketing I1 latencies.
const ONE_HOUR_MS: u32 = 60 * 60 * 1_000;
/// Maximum allowed speech-to-screen p99 latency under I1 (`PLAN.md` §7).
const I1_P99_LIMIT_MS: u64 = 1_000;
/// Maximum allowed p99 drift between hour 1 and hour 8 under I1 (one 100 ms frame tick).
const I1_DRIFT_LIMIT_MS: u64 = 100;
/// Recovery budget for a killed caption lane (`P1.md` §2 I6).
const I6_LANE_RECOVER_MS: u64 = 5_000;
/// Recovery budget for a silent service connection (`P1-17-watch.md` I7: tightened to 12 s).
const I7_SILENT_RECOVER_MS: u64 = 12_000;
/// Burst size exceeding the 600-block tape queue capacity (`P1.md` §3.7).
const TAPE_OVERFLOW_BLOCKS: usize = 700;

/// Seeds a recording with `line_count` lines on `shelf`, optionally placing `needle` in one line.
fn seed_recording(shelf: &Shelf, id: &str, title: &str, line_count: usize, needle: Option<&str>) {
    let about = About {
        started: 1_760_000_000_000,
        length_ms: 10_000,
        title: title.into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        source: "mic".into(),
        take: 1,
        ..About::default()
    };
    shelf
        .put(id, RECORDING, &[(id.to_owned(), Some(about))], 1_760_000_000_000)
        .expect("seed recording");
    if line_count > 0 {
        let mut batch = Vec::with_capacity(line_count);
        for i in 0..line_count {
            let text = if i == line_count / 2
                && let Some(n) = needle
            {
                format!("line {i} contains {n} here")
            } else {
                format!("synthetic line number {i} for search")
            };
            let said = Said {
                line: Line {
                    t0: (i as u32) * 100,
                    t1: (i as u32) * 100 + 80,
                    speaker: "1".into(),
                    language: "en".into(),
                    text,
                    translations: BTreeMap::new(),
                    incomplete: false,
                },
                via: BTreeMap::from([("text".into(), "seed".into())]),
            };
            batch.push((format!("{id}.1.{i:05}"), Some(said)));
        }
        shelf.put(id, LINE, &batch, 1_760_000_000_000).expect("seed lines");
    }
}

// ---- 0. bench self-tests ----

#[test]
fn speech_schedule_is_deterministic() {
    let a = Speech::build(SpeechSpec::three_languages(5 * 60 * 1_000));
    let b = Speech::build(SpeechSpec::three_languages(5 * 60 * 1_000));
    assert_eq!(a, b, "same spec must yield byte-for-byte identical schedule");
    assert!(!a.words.is_empty());

    // Every 10th sentence is a continuous 24-second utterance with no internal silence.
    let long_24s = a.sentences.iter().find(|s| s.end_ms - s.start_ms >= 24_000);
    assert!(long_24s.is_some(), "schedule must include a 24-second continuous sentence");
    let long = long_24s.unwrap();
    for block in (long.start_ms / BLOCK_MS)..(long.end_ms / BLOCK_MS) {
        assert!(a.is_loud(block as usize), "24 s sentence must stay loud at block {block}");
    }

    // Three-language spec includes 150-character Chinese lines.
    let has_150_zh = a.sentences.iter().any(|s| {
        if s.language != "zh" {
            return false;
        }
        let chars: usize = a.words[s.first_word..s.first_word + s.word_count]
            .iter()
            .map(|w| w.token.chars().count())
            .sum();
        chars >= 150
    });
    assert!(has_150_zh, "three-language schedule must include 150-character Chinese lines");
}

#[tokio::test(start_paused = true)]
async fn fake_service_frames_parse_as_expected_tokens() {
    let speech = Arc::new(Speech::build(SpeechSpec::standard(10_000)));
    let first_word = speech.words[0].clone();
    let first_block = first_word.start_ms / BLOCK_MS;

    let service = Service::new(speech.clone(), Delays::default(), Fault::None);
    let dial = service.dial();
    let mut ws = dial("ws://bench.local").await.expect("duplex dial succeeds");

    let setup = LaneSetup {
        endpoint: "ws://bench.local".into(),
        model: "stt-rt-v6".into(),
        spoken: vec!["en".into(), "zh".into()],
        ..LaneSetup::default()
    };
    let hello = lane::config("sk-test", &Stream::TwoWay("en".into(), "zh".into()), &setup);
    ws.send(Message::text(hello.to_string())).await.expect("send config");

    // Send the block containing word 0.
    let mut pcm = vec![0i16; BLOCK];
    encode_block_index(&mut pcm, first_block);
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    ws.send(Message::binary(bytes)).await.expect("send audio block");

    // After GUESS_DELAY_MS: non-final guess token arrives and parses cleanly via `lane::reply`.
    tokio::time::advance(Duration::from_millis(GUESS_DELAY_MS)).await;
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        panic!("expected guess frame");
    };
    let guess = lane::reply(&text).expect("guess reply parses");
    assert_eq!(guess.tokens.len(), 1);
    assert!(!guess.tokens[0].is_final);
    assert_eq!(guess.tokens[0].text, first_word.token);
    assert_eq!(guess.tokens[0].end_ms - guess.tokens[0].start_ms, 60);

    // After FINAL_DELAY_MS: final spoken + final translation tokens arrive.
    tokio::time::advance(Duration::from_millis(FINAL_DELAY_MS - GUESS_DELAY_MS)).await;
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        panic!("expected final frame");
    };
    let settled = lane::reply(&text).expect("final reply parses");
    assert!(
        settled.tokens.iter().any(|t| t.is_final && t.into.is_none() && t.text == first_word.token)
    );
    assert!(settled.tokens.iter().any(|t| t.is_final && t.into.is_some()));

    // Send a silent block, wait SILENCE_PROGRESS_MS: silence progress frame advances clock_ms.
    let mut silent = vec![0i16; BLOCK];
    encode_block_index(&mut silent, 0);
    let bytes: Vec<u8> = silent.iter().flat_map(|s| s.to_le_bytes()).collect();
    ws.send(Message::binary(bytes)).await.expect("send silent block");
    tokio::time::advance(Duration::from_millis(SILENCE_PROGRESS_MS)).await;
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        panic!("expected silence progress frame");
    };
    let progress = lane::reply(&text).expect("progress reply parses");
    assert_eq!(progress.clock_ms, 200);

    // Finalize command returns `<fin>` (parsed as `ended: true`).
    ws.send(Message::text(r#"{"type":"finalize"}"#)).await.expect("send finalize");
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        panic!("expected finalize frame");
    };
    let fin = lane::reply(&text).expect("fin reply parses");
    assert!(fin.ended);

    // Empty frame returns `finished: true`.
    ws.send(Message::text("")).await.expect("send end of stream");
    let Some(Ok(Message::Text(text))) = ws.next().await else {
        panic!("expected finished frame");
    };
    let done = lane::reply(&text).expect("finished reply parses");
    assert!(done.finished);
}

// ---- 1. Privacy & audio-only ----

#[test]
fn the_search_for_leaks_finds_one() {
    let forbidden = vec!["w000042".into(), "sk-own-test-key".into()];

    let clean = r#"{"t_ms":1760000000100,"seq":1,"level":"info","component":"zutalk_core::app::hub","event":"identity.who","from":"nobody","to":"local","why":"boot","made":false}"#;
    assert!(check_log_lines(&[clean.into()], &forbidden).is_ok());

    let with_word = r#"{"t_ms":1760000000100,"seq":2,"level":"info","component":"zutalk_core::live","event":"lane.said","word":"w000042"}"#;
    assert!(check_log_lines(&[with_word.into()], &forbidden).is_err());

    let with_key = r#"{"t_ms":1760000000100,"seq":3,"level":"info","component":"zutalk_core::id","event":"keys.use","key":"sk-own-test-key"}"#;
    assert!(check_log_lines(&[with_key.into()], &forbidden).is_err());

    let with_refused = r#"{"t_ms":1760000000100,"seq":4,"level":"warn","component":"zutalk_core::app::hub","event":"task.start","name":"!"}"#;
    assert!(check_log_lines(&[with_refused.into()], &forbidden).is_err());

    let with_nested = r#"{"t_ms":1760000000100,"seq":5,"level":"info","component":"zutalk_core::app::hub","event":"task.start","nested":{"a":1}}"#;
    assert!(check_log_lines(&[with_nested.into()], &forbidden).is_err());
}

#[tokio::test(start_paused = true)]
async fn without_a_key_the_bench_records_sound_and_no_caption() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(35_000));
    let mut opts = BenchOptions::new(speech);
    opts.with_key = false;
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(30_000)).await;

    let expected = Status::AudioOnly { why: "captions.no_key".into() };
    assert_eq!(bench.status(), Some(expected.clone()));
    assert!(
        bench.statuses().iter().all(|s| *s == expected),
        "status must remain AudioOnly(captions.no_key), saw {:?}",
        bench.statuses()
    );
    assert_eq!(bench.non_empty_column_frames(), 0, "no frame may carry caption columns");
    assert_eq!(bench.service.dials(), 0, "realtime service must never be dialed without a key");

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mic_blocks = bench.mic.blocks();
    let tape_blocks = bench.counting_tape.blocks();
    assert!(mic_blocks >= 290, "expected ~300 blocks in 30 s, got {mic_blocks}");
    assert_eq!(tape_blocks, mic_blocks, "tape must keep every block the mic delivered");

    let starts = bench.seen().of("recording.start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].text("captions"), Some("captions.no_key"));
    assert_eq!(starts[0].u64("lanes"), Some(0));
    let statuses = bench.seen().of("session.status");
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].text("to"), Some("AudioOnly"));
    assert_eq!(statuses[0].text("why"), Some("captions.no_key"));
    let ends = bench.seen().of("recording.end");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].text("code"), Some("done"));

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn nothing_said_or_secret_reaches_the_log() {
    let _store = STORE.lock().await;
    let log_dir = tempfile::tempdir().expect("log tempdir");
    let (scope, guard) =
        zutalk_log::files(log_dir.path(), "zutalk", Level::Debug, Limits::default())
            .expect("open file log");
    let entered = scope.enter();

    let speech = Speech::build(SpeechSpec::standard(125_000));
    let forbidden_words = speech.forbidden_words();
    let mut opts = BenchOptions::new(speech);
    opts.capture_log = false;
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::panic_at(20_000);

    let bench = Bench::start(opts).await;
    let data_path = bench.data().to_string_lossy().into_owned();
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    let secret_title = "SecretTitleZeta99";
    let secret_query = "QueryNeedleOmega88";

    tokio::time::sleep(Duration::from_millis(60_000)).await;
    bench.send(Command::Change {
        id: rec_id.clone(),
        change: Change::Title { text: secret_title.into() },
    });
    bench.send(Command::Search { query: secret_query.into() });
    tokio::time::sleep(Duration::from_millis(60_000)).await;

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();

    drop(entered);
    drop(guard);

    let mut lines = Vec::new();
    for entry in fs::read_dir(log_dir.path()).expect("read log dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let content = fs::read_to_string(&path).expect("read jsonl");
            for line in content.lines().filter(|l| !l.trim().is_empty()) {
                lines.push(line.to_owned());
            }
        }
    }

    assert!(!lines.is_empty(), "log files must not be empty");
    let has_code = |code: &str| lines.iter().any(|l| l.contains(&format!(r#""event":"{code}""#)));
    assert!(has_code("recording.start"), "expected recording.start in log");
    assert!(has_code("recording.end"), "expected recording.end in log");
    assert!(has_code("identity.who"), "expected identity.who in log");

    let mut forbidden = forbidden_words;
    forbidden.push(secret_title.into());
    forbidden.push(secret_query.into());
    for spk in SPEAKERS {
        forbidden.push(spk.into());
    }
    forbidden.push(hub::local_did());
    forbidden.push("sk-own-test-key".into());
    forbidden.push(data_path);

    if let Err(leak) = check_log_lines(&lines, &forbidden) {
        panic!("privacy log check failed: {leak}");
    }
    let mut covered: Vec<String> = lines
        .iter()
        .filter_map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v.get("event")?.as_str().map(str::to_owned))
        })
        .collect();
    covered.sort();
    covered.dedup();
    eprintln!("privacy_log_events ({}): {:?}", covered.len(), covered);
}

// ---- 2. I1: speech-to-screen latency and bounded frames ----

struct I1Run {
    summaries: Vec<bench::LatencySummary>,
    missing: usize,
    max_frame_bytes: usize,
    captured_events: usize,
    fit_events: usize,
    frame_cut: u64,
    frame_bound_most: u64,
    caption_lag_n: u64,
    caption_lag_p50: u64,
    caption_lag_p99: u64,
    caption_lag_most: u64,
    judge_verdicts: usize,
    stall_snapshots: usize,
    h1_running: Option<usize>,
    h8_running: Option<usize>,
    h1_events: BTreeMap<String, usize>,
    h8_events: BTreeMap<String, usize>,
    wall: Duration,
}

async fn run_i1(spec: SpeechSpec, shown: &[&str], hours: usize) -> I1Run {
    let wall_start = WallInstant::now();
    let duration_ms = spec.duration_ms;
    let speech = Speech::build(spec);
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], shown).await;
    let _id = bench.start_recording().await;
    let started_at = bench.mic.started_at().expect("mic started");

    // Advance virtual time in 60-second chunks plus a 3-second drain tail.
    let total_ms = u64::from(duration_ms) + 3_000;
    let mut elapsed = 0u64;
    let step = 60_000u64;
    let mut h1_running = None;
    let mut h8_running = None;
    let mut h1_events = BTreeMap::new();
    let mut h7_end_count = 0usize;
    let mut h8_events = BTreeMap::new();
    let count_codes = |lines: &[zutalk_log::Line]| -> BTreeMap<String, usize> {
        let mut map = BTreeMap::new();
        for l in lines {
            *map.entry(l.code.to_owned()).or_insert(0) += 1;
        }
        map
    };
    while elapsed < total_ms {
        let chunk = (total_ms - elapsed).min(step);
        tokio::time::sleep(Duration::from_millis(chunk)).await;
        elapsed += chunk;
        if hours >= 8 {
            if elapsed == u64::from(ONE_HOUR_MS) {
                h1_running = Some(bench.running().len());
                h1_events = count_codes(&bench.seen().all());
            } else if elapsed == 7 * u64::from(ONE_HOUR_MS) {
                h7_end_count = bench.seen().all().len();
            } else if elapsed == 8 * u64::from(ONE_HOUR_MS) {
                h8_running = Some(bench.running().len());
                let all = bench.seen().all();
                h8_events = count_codes(&all[h7_end_count.min(all.len())..]);
            }
        }
    }

    let seen_words = bench.word_seen();
    let max_frame_bytes = bench.max_frame_bytes();
    let col_active = bench.column_active();
    let captured_events = bench.seen().all().len();
    let fit_events = bench.seen().count("frame.fit");
    let frame_cut = bench.count("frame.cut");
    let frame_bound_most = bench.peak("frame.bound_bytes").map_or(0, |p| p.most);
    let lag = bench.spread("caption.lag_ms");
    let caption_lag_n = lag.as_ref().map_or(0, |s| s.n);
    let caption_lag_p50 = lag.as_ref().map_or(0, |s| s.p50);
    let caption_lag_p99 = lag.as_ref().map_or(0, |s| s.p99);
    let caption_lag_most = lag.as_ref().map_or(0, |s| s.most);
    let judge_verdicts = bench.seen().count("judge.verdict");
    let stall_snapshots = bench.seen().count("stall.snapshot");

    let mut buckets: Vec<Vec<u64>> = vec![Vec::new(); hours.max(1)];
    let mut missing = 0usize;
    for (idx, word) in bench.speech.words.iter().enumerate() {
        let Some(first_seen) = seen_words[idx] else {
            missing += 1;
            continue;
        };
        let word_end = started_at + Duration::from_millis(u64::from(word.end_ms));
        let lag_ms = first_seen.saturating_duration_since(word_end).as_millis() as u64;
        let hour = ((word.end_ms / ONE_HOUR_MS) as usize).min(buckets.len() - 1);
        buckets[hour].push(lag_ms);
    }

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(col_active.len(), shown.len(), "expected {} columns", shown.len());
    assert!(col_active.iter().all(|&a| a), "every column must receive captions: {col_active:?}");
    bench.finish();

    let summaries = buckets.into_iter().map(summarize_latencies).collect();
    I1Run {
        summaries,
        missing,
        max_frame_bytes,
        captured_events,
        fit_events,
        frame_cut,
        frame_bound_most,
        caption_lag_n,
        caption_lag_p50,
        caption_lag_p99,
        caption_lag_most,
        judge_verdicts,
        stall_snapshots,
        h1_running,
        h8_running,
        h1_events,
        h8_events,
        wall: wall_start.elapsed(),
    }
}

#[tokio::test(start_paused = true)]
async fn i1_three_languages_fit_a_frame() {
    let _store = STORE.lock().await;
    let run = run_i1(SpeechSpec::three_languages(THIRTY_MINUTES_MS), &["en", "zh", "es"], 1).await;
    let s = run.summaries[0];
    eprintln!(
        "i1_three_languages: words={} missing={} p50={}ms p99={}ms max={}ms max_frame={}B bound_most={}B cut={} fit_events={} events={} wall={:.2}s",
        s.count,
        run.missing,
        s.p50_ms,
        s.p99_ms,
        s.max_ms,
        run.max_frame_bytes,
        run.frame_bound_most,
        run.frame_cut,
        run.fit_events,
        run.captured_events,
        run.wall.as_secs_f64()
    );
    assert_eq!(run.missing, 0, "no words may be lost");
    assert!(s.p99_ms <= I1_P99_LIMIT_MS, "p99 {} ms > {I1_P99_LIMIT_MS} ms", s.p99_ms);
    assert!(
        run.max_frame_bytes <= MAX_FRAME_BYTES,
        "max frame {} bytes > {MAX_FRAME_BYTES} bytes",
        run.max_frame_bytes
    );
    assert!(run.fit_events >= 1, "expected at least one frame.fit event");
    assert!(run.frame_cut > 0, "expected frame.cut > 0");
    assert!(run.frame_bound_most > 0, "expected frame.bound_bytes peak > 0");
}

#[tokio::test(start_paused = true)]
async fn i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first() {
    let _store = STORE.lock().await;
    let run = run_i1(SpeechSpec::standard(EIGHT_HOURS_MS), &["en", "zh"], 8).await;
    for (h, s) in run.summaries.iter().enumerate() {
        eprintln!(
            "i1_eight_hours h{}: words={} p50={}ms p99={}ms max={}ms",
            h + 1,
            s.count,
            s.p50_ms,
            s.p99_ms,
            s.max_ms
        );
    }
    eprintln!(
        "i1_eight_hours total: missing={} max_frame={}B bound_most={}B caption.lag_ms=(n={} p50={}ms p99={}ms most={}ms) judge.verdict={} stall.snapshot={} events={} wall={:.2}s",
        run.missing,
        run.max_frame_bytes,
        run.frame_bound_most,
        run.caption_lag_n,
        run.caption_lag_p50,
        run.caption_lag_p99,
        run.caption_lag_most,
        run.judge_verdicts,
        run.stall_snapshots,
        run.captured_events,
        run.wall.as_secs_f64()
    );
    assert_eq!(run.missing, 0, "no words may be lost across 8 hours");
    for (h, s) in run.summaries.iter().enumerate() {
        assert!(s.count > 0, "hour {} had no words", h + 1);
        assert!(
            s.p99_ms <= I1_P99_LIMIT_MS,
            "hour {} p99 {} ms > {I1_P99_LIMIT_MS} ms",
            h + 1,
            s.p99_ms
        );
    }
    assert!(
        run.summaries[7].p99_ms <= run.summaries[0].p99_ms + I1_DRIFT_LIMIT_MS,
        "hour 8 p99 {} ms drifted > {I1_DRIFT_LIMIT_MS} ms past hour 1 p99 {} ms",
        run.summaries[7].p99_ms,
        run.summaries[0].p99_ms
    );
    assert!(
        run.max_frame_bytes <= MAX_FRAME_BYTES,
        "max frame {} bytes > {MAX_FRAME_BYTES} bytes",
        run.max_frame_bytes
    );
    assert_eq!(
        run.h1_running, run.h8_running,
        "Tree::running() count drifted between hour 1 ({:?}) and hour 8 ({:?})",
        run.h1_running, run.h8_running
    );
    eprintln!(
        "i1_eight_hours cost: h1_running={:?} h8_running={:?} h1_events={:?} h8_events={:?}",
        run.h1_running, run.h8_running, run.h1_events, run.h8_events
    );
}

#[tokio::test(start_paused = true)]
async fn i1_captions_do_not_resend_the_state() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(70_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _id = bench.start_recording().await;

    let (state_seq_before, _) = bench.outlet().state.now();
    let (frame_seq_before, _) = bench.outlet().frame.now();

    tokio::time::sleep(Duration::from_millis(60_000)).await;

    let (state_seq_after, _) = bench.outlet().state.now();
    let (frame_seq_after, _) = bench.outlet().frame.now();
    let frame_delta = frame_seq_after.saturating_sub(frame_seq_before);

    assert_eq!(
        state_seq_after, state_seq_before,
        "steady captions must not resend State (seq {state_seq_before} -> {state_seq_after})"
    );
    assert!(
        frame_delta >= 150,
        "expected well over a hundred frames in 60 s, got {frame_delta} ({frame_seq_before} -> {frame_seq_after})"
    );

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_stop_clears_the_session_and_no_late_status_brings_it_back() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(20_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;
    // Delay service replies by 3 s so the lane and recording task are still wrapping up after Stop.
    opts.fault = Fault::Slow { extra_ms: 3_000 };

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(6_000)).await;
    assert!(
        bench.outlet().session.now().1.is_some(),
        "expected active session in outlet.session before Stop"
    );
    assert!(
        !bench.outlet().frame.now().1.columns.is_empty(),
        "expected non-empty frame columns before Stop"
    );

    bench.stop_recording().await;

    let (session_seq_stop, session_at_stop) = bench.outlet().session.now();
    let (frame_seq_stop, frame_at_stop) = bench.outlet().frame.now();
    assert!(session_at_stop.is_none(), "outlet.session must be None immediately after Stop");
    assert!(frame_at_stop.columns.is_empty(), "outlet.frame must be empty immediately after Stop");

    // Advance 10 virtual seconds while the recording task finishes closing its slow lane.
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_, s) = bench.outlet().session.now();
        let (_, f) = bench.outlet().frame.now();
        assert!(s.is_none(), "late status brought session back after Stop: {s:?}");
        assert!(f.columns.is_empty(), "late frame repopulated outlet.frame after Stop: {f:?}");
    }

    let (session_seq_after, _) = bench.outlet().session.now();
    let (frame_seq_after, _) = bench.outlet().frame.now();
    assert_eq!(session_seq_after, session_seq_stop, "outlet.session seq moved after Stop");
    assert_eq!(frame_seq_after, frame_seq_stop, "outlet.frame seq moved after Stop");

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn the_level_follows_the_sound_and_falls_to_nothing_when_paused() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(30_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _id = bench.start_recording().await;

    // First sentence begins at 500 ms (`LEAD_SILENCE_MS`); at ~1,000 ms blocks are loud (~8000).
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let (seq_loud, loud) = bench.outlet().level.now();
    assert!(seq_loud > 0, "outlet.level must be written per block");
    assert!(loud.peak >= 7_000 && loud.rms >= 6_000, "expected loud level, got {loud:?}");

    // Advance into the inter-sentence silence after sentence 0 (`end_ms` .. `sentence[1].start_ms`).
    let s0_end = bench.speech.sentences[0].end_ms;
    let s1_start = bench.speech.sentences[1].start_ms;
    assert!(s1_start >= s0_end + 300, "expected silence gap between sentences 0 and 1");
    let target_quiet_ms = u64::from(s0_end) + 200;
    tokio::time::sleep(Duration::from_millis(target_quiet_ms.saturating_sub(1_000))).await;
    let (_, quiet) = bench.outlet().level.now();
    assert!(quiet.peak < 100 && quiet.rms < 20, "expected quiet level in pause gap, got {quiet:?}");

    // Advance into sentence 1 so sound rises again, then pause and verify level drops to zero.
    let target_loud2_ms = u64::from(s1_start) + 300;
    tokio::time::sleep(Duration::from_millis(target_loud2_ms - target_quiet_ms)).await;
    let (_, loud_again) = bench.outlet().level.now();
    assert!(loud_again.peak >= 7_000, "expected level to rise again on speech, got {loud_again:?}");

    bench.send(Command::Pause);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (seq_paused, paused_lvl) = bench.outlet().level.now();
    assert_eq!(*paused_lvl, zutalk_core::app::proto::Level { peak: 0, rms: 0 });

    tokio::time::sleep(Duration::from_millis(3_000)).await;
    let (seq_paused_after, paused_after) = bench.outlet().level.now();
    assert_eq!(seq_paused_after, seq_paused, "level seq must not advance while paused");
    assert_eq!(*paused_after, zutalk_core::app::proto::Level { peak: 0, rms: 0 });

    bench.stop_recording().await;
    assert_eq!(*bench.outlet().level.now().1, zutalk_core::app::proto::Level { peak: 0, rms: 0 });
    tokio::time::sleep(Duration::from_millis(200)).await;
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_recording_the_capture_ends_is_ended_by_the_recording_task() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(30_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    // Speak long enough for at least one sentence to settle into the draft.
    tokio::time::sleep(Duration::from_millis(10_000)).await;
    assert!(bench.outlet().session.now().1.is_some());

    // Capture ends on its own (as on lid close).
    bench.mic.end();

    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.outlet().session.now().1.is_some() {
        assert!(Instant::now() < deadline, "session did not clear after capture ended");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(bench.outlet().session.now().1.is_none());
    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("draft must remain");
    assert!(!recorded.lines.is_empty(), "draft lines spoken before capture ended must remain");
    let first_word = &bench.speech.words[bench.speech.sentences[0].first_word];
    let draft_text: String =
        recorded.lines.iter().map(|(_, s)| s.line.text.as_str()).collect::<Vec<_>>().join(" ");
    assert!(draft_text.contains(&first_word.needle), "draft missing first sentence");

    // Observability: recording.end at level error with the capture code, and notice.said with same code.
    let ends = bench.seen().of("recording.end");
    assert_eq!(ends.len(), 1, "expected one recording.end, got {ends:?}");
    assert_eq!(ends[0].level, Level::Error);
    assert_eq!(ends[0].text("rec"), Some(rec_id.as_str()));
    let end_code = ends[0].text("code").expect("recording.end code");
    assert_eq!(end_code, "audio.closed");
    assert!(
        bench.seen().of("notice.said").iter().any(|l| l.text("code") == Some(end_code)),
        "expected notice.said with `{end_code}`"
    );
    assert!(bench.notices().iter().any(|c| c == end_code));

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_recording_says_when_it_starts_how_it_goes_and_when_it_ends() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(60_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;

    // First recording: 30 s, Pause, Resume, Stop.
    let rec1 = bench.start_recording().await;
    tokio::time::sleep(Duration::from_millis(30_000)).await;

    bench.send(Command::Pause);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(bench.status(), Some(Status::Paused));
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    bench.send(Command::Resume);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(bench.status(), Some(Status::Recording));
    tokio::time::sleep(Duration::from_millis(5_000)).await;

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let starts = bench.seen().of("recording.start");
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(starts[0].level, Level::Info);
    assert_eq!(starts[0].text("rec"), Some(rec1.as_str()));
    assert_eq!(starts[0].text("source"), Some("mic"));
    assert_eq!(starts[0].u64("lanes"), Some(1));
    assert_eq!(starts[0].u64("columns"), Some(2));
    assert_eq!(starts[0].text("captions"), Some("live"));

    let statuses: Vec<_> = bench
        .seen()
        .of("session.status")
        .into_iter()
        .filter(|l| l.text("rec") == Some(rec1.as_str()))
        .collect();
    let transitions: Vec<(&str, &str, &str)> = statuses
        .iter()
        .map(|l| {
            assert_eq!(l.level, Level::Info);
            (
                l.text("from").unwrap_or("?"),
                l.text("to").unwrap_or("?"),
                l.text("why").unwrap_or("?"),
            )
        })
        .collect();
    assert_eq!(
        transitions,
        [("Connecting", "Recording", ""), ("Recording", "Paused", ""), ("Paused", "Recording", ""),]
    );

    let ends = bench.seen().of("recording.end");
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].level, Level::Info);
    assert_eq!(ends[0].text("rec"), Some(rec1.as_str()));
    assert_eq!(ends[0].text("code"), Some("done"));
    let all_rec1: Vec<&str> = bench
        .seen()
        .all()
        .into_iter()
        .filter(|l| l.text("rec") == Some(rec1.as_str()))
        .map(|l| l.code)
        .collect();
    assert_eq!(all_rec1.first(), Some(&"recording.start"));
    assert_eq!(all_rec1.last(), Some(&"recording.end"));
    let non_phase: Vec<&str> =
        all_rec1.into_iter().filter(|&code| code != "recording.phase").collect();
    assert_eq!(
        non_phase,
        ["recording.start", "session.status", "session.status", "session.status", "recording.end"]
    );

    // Second recording back-to-back: carries its own id.
    let rec2 = bench.start_recording().await;
    assert_ne!(rec2, rec1);
    tokio::time::sleep(Duration::from_millis(5_000)).await;
    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let starts2: Vec<_> = bench
        .seen()
        .of("recording.start")
        .into_iter()
        .filter(|l| l.text("rec") == Some(rec2.as_str()))
        .collect();
    assert_eq!(starts2.len(), 1);
    let statuses2: Vec<_> = bench
        .seen()
        .of("session.status")
        .into_iter()
        .filter(|l| l.text("rec") == Some(rec2.as_str()))
        .collect();
    assert_eq!(statuses2.len(), 1);
    assert_eq!(statuses2[0].text("from"), Some("Connecting"));
    assert_eq!(statuses2[0].text("to"), Some("Recording"));
    let ends2: Vec<_> = bench
        .seen()
        .of("recording.end")
        .into_iter()
        .filter(|l| l.text("rec") == Some(rec2.as_str()))
        .collect();
    assert_eq!(ends2.len(), 1);
    assert_eq!(ends2[0].text("code"), Some("done"));

    bench.finish();
}

// ---- 3. I2: slow/blocked paths never stall captions or commands ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i2_a_tape_that_stands_still_holds_no_caption_back() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(60_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;

    // Part 1: close tape gate after 1 s; captions must still deliver 15 frames and new words.
    let rec1 = bench.start_recording().await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let seen_before_gate = bench.word_seen().iter().filter(|w| w.is_some()).count();

    {
        let _gate = bench.gated_tape.close_gate();
        // Wait for 15 more frames while the tape writer is blocked.
        bench
            .wait_frames(15, Duration::from_secs(6))
            .await
            .expect("captions stalled while tape gate was closed");
        let seen_while_gated = bench.word_seen().iter().filter(|w| w.is_some()).count();
        assert!(
            seen_while_gated > seen_before_gate,
            "words spoken while tape was blocked did not reach screen ({seen_before_gate} -> {seen_while_gated})"
        );
        // Observability (P1.md §2): no queue.full yet, and tape queue depth > 0.
        assert_eq!(
            bench
                .seen()
                .of("queue.full")
                .iter()
                .filter(|l| l.text("queue") == Some("tape"))
                .count(),
            0
        );
        let q = bench.queue("tape").expect("tape queue stats row");
        assert!(q.deepest > 0, "expected tape queue to buffer while gated");
    }

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mic_blocks = bench.mic.blocks();
    let key = keys::derive("tape", &rec1).expect("tape key");
    let mut read_blocks = 0u64;
    let len_ms = tape::read(&bench.data().join("tape").join(&rec1), &key, &Pass::Export, |_| {
        read_blocks += 1;
    })
    .expect("tape reads back intact");
    assert_eq!(read_blocks, mic_blocks, "no block may be lost after gate re-opens");
    assert_eq!(len_ms as u64, mic_blocks * u64::from(BLOCK_MS));

    // Part 2: burst 700 blocks (> 600 capacity) while gated -> recording ends with `tape`, draft kept.
    let rec2 = bench.start_recording().await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    {
        let _gate = bench.gated_tape.close_gate();
        // Wait briefly for the writer to hit the closed gate.
        let wait_until = Instant::now() + Duration::from_secs(2);
        while !bench.gated_tape.has_waiter() && Instant::now() < wait_until {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        bench.mic.burst(TAPE_OVERFLOW_BLOCKS);
        bench
            .wait_notice("tape", Duration::from_secs(6))
            .await
            .expect("overflowing tape queue must end recording with code `tape`");
        bench
            .wait_session(|s| s.is_none(), Duration::from_secs(6))
            .await
            .expect("recording must end after tape overflow");
    }

    assert!(bench.shelf().read(&rec2).expect("read shelf").is_some(), "draft must remain on shelf");
    assert!(
        bench.notices().iter().any(|c| c == "tape"),
        "notices must include `tape`, got {:?}",
        bench.notices()
    );
    // Wait for `Task::finish` to emit `recording.end(code = "tape")` after `session` cleared.
    let end_deadline = Instant::now() + Duration::from_secs(4);
    while bench.seen().of("recording.end").iter().filter(|l| l.text("code") == Some("tape")).count()
        == 0
    {
        assert!(Instant::now() < end_deadline, "expected recording.end(code = tape)");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Observability (P1.md §2).
    assert_eq!(
        bench
            .seen()
            .of("queue.full")
            .iter()
            .filter(|l| l.text("queue") == Some("tape") && l.text("policy") == Some("end"))
            .count(),
        1
    );
    assert_eq!(
        bench.seen().of("recording.end").iter().filter(|l| l.text("code") == Some("tape")).count(),
        1
    );
    bench.finish();
}

#[cfg(unix)]
struct FifoGuard {
    path: PathBuf,
    released: Arc<AtomicBool>,
}

#[cfg(unix)]
impl FifoGuard {
    fn create(dir: &std::path::Path, name: &str) -> Self {
        let path = dir.join(name);
        let status = std::process::Command::new("mkfifo").arg(&path).status().expect("mkfifo runs");
        assert!(status.success(), "mkfifo failed for {}", path.display());
        Self { path, released: Arc::new(AtomicBool::new(false)) }
    }

    fn drain(&self) -> String {
        self.released.store(true, Ordering::Release);
        fs::read_to_string(&self.path).expect("read exported markdown from fifo")
    }
}

#[cfg(unix)]
impl Drop for FifoGuard {
    fn drop(&mut self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            // Opening a FIFO RDWR never blocks if no writer is attached, and immediately unblocks
            // any writer stuck in `open(O_WRONLY)` so `Hub` can shut down on test failure.
            let _ = fs::OpenOptions::new().read(true).write(true).open(&self.path);
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands() {
    let _store = STORE.lock().await;
    let data = tempfile::tempdir().expect("data tempdir");
    let fifo_dir = tempfile::tempdir().expect("fifo tempdir");
    let fifo = FifoGuard::create(fifo_dir.path(), "out.md");

    hub::nobody();
    let existing_id = "19a0000000100000001";
    seed_recording(&hub::local_shelf(data.path()), existing_id, "initial-title", 5, None);

    let speech = Speech::build(SpeechSpec::standard(60_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;
    opts.data = Some(data.path().to_path_buf());

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec = bench.start_recording().await;
    bench.wait_frames(3, Duration::from_secs(5)).await.expect("initial live frames");

    // Start an export into a FIFO with no reader: this blocks until `fifo.drain()`.
    bench.send(Command::Export {
        id: existing_id.into(),
        exporter: "markdown".into(),
        languages: vec![],
        to: fifo.path.to_string_lossy().into_owned(),
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // While export is stuck: 20 more frames must arrive.
    bench.wait_frames(20, Duration::from_secs(6)).await.expect("export blocked live captions");

    // Pause and Resume must respond.
    bench.send(Command::Pause);
    bench
        .wait_session(
            |s| matches!(s, Some(sess) if sess.status == Status::Paused),
            Duration::from_secs(4),
        )
        .await
        .expect("Pause did not take effect while export was blocked");
    bench.send(Command::Resume);
    bench
        .wait_session(
            |s| matches!(s, Some(sess) if sess.status != Status::Paused),
            Duration::from_secs(4),
        )
        .await
        .expect("Resume did not take effect while export was blocked");

    // Change title and Open must return the updated title.
    bench.send(Command::Change {
        id: existing_id.into(),
        change: Change::Title { text: "updated-title".into() },
    });
    bench.send(Command::Open { id: existing_id.into() });
    bench
        .wait_state(
            |s| s.open.as_ref().is_some_and(|o| o.row.title == "updated-title"),
            Duration::from_secs(4),
        )
        .await
        .expect("Change/Open did not complete while export was blocked");

    // Observability while blocked (P1.md §2).
    let export_starts: Vec<_> = bench
        .seen()
        .of("job.start")
        .into_iter()
        .filter(|l| l.text("kind") == Some("export.write"))
        .collect();
    assert_eq!(export_starts.len(), 1, "expected one job.start(export.write) while blocked");
    let export_seq = export_starts[0].u64("job").expect("export.write job");
    assert!(
        !bench
            .seen()
            .of("job.done")
            .iter()
            .any(|l| l.text("kind") == Some("export.write") && l.u64("job") == Some(export_seq)),
        "export.write should still be running while FIFO is unopened"
    );
    let running = bench.running();
    assert!(
        running.iter().any(|r| r.task == "export.write"),
        "expected export.write in Tree::running(), got {running:?}"
    );
    for kind in ["Pause", "Resume", "Change"] {
        assert!(
            bench
                .seen()
                .of("command.done")
                .iter()
                .any(|l| l.text("kind") == Some(kind) && l.text("code") == Some("ok")),
            "expected command.done({kind}, ok)"
        );
    }

    // Open the read end of the FIFO so export finishes.
    let exported = tokio::task::spawn_blocking(move || fifo.drain()).await.expect("fifo read");
    assert!(exported.contains("initial-title"), "exported markdown should contain title");

    // Observability after unblocking.
    let deadline = Instant::now() + Duration::from_secs(4);
    while !bench.seen().of("job.done").iter().any(|l| {
        l.text("kind") == Some("export.write")
            && l.u64("job") == Some(export_seq)
            && l.text("code") == Some("ok")
    }) {
        assert!(Instant::now() < deadline, "expected job.done(export.write, seq={export_seq}, ok)");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    bench.stop_recording().await;
    bench.finish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i2_background_work_runs_beside_a_recording() {
    let _store = STORE.lock().await;
    let data = tempfile::tempdir().expect("data tempdir");
    let old = common::old_library();

    // 0.8 migration runs only for a signed-in person (`P1.md` §3.14.8); our server stays NOWHERE.
    hub::signed_in();
    let shelf = hub::shelf(data.path());

    // 1. Recording with 2,000 lines to search.
    let search_id = "19a0000000200000001";
    let needle = "needle98765";
    seed_recording(&shelf, search_id, "searchable-big", 2_000, Some(needle));

    // 2. Draft recording with audio owed a whole-session transcript.
    let draft_id = "19a0000000200000002";
    let draft_about = About {
        started: 1_760_000_000_001,
        length_ms: 2_000,
        title: "owed-draft".into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        source: "mic".into(),
        take: 0,
        ..About::default()
    };
    shelf
        .put(draft_id, RECORDING, &[(draft_id.into(), Some(draft_about))], 1_760_000_000_001)
        .expect("seed owed draft");
    hub::tape(data.path(), draft_id, 2);

    let asked = Arc::new(Mutex::new(Vec::new()));
    let stt = hub::soniox(&asked).await;

    let speech = Speech::build(SpeechSpec::standard(15_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.signed_in = true;
    opts.keep_store = true;
    opts.soniox = Some(format!("{}/v1", stt.uri()));
    opts.old = Some(old.path().to_path_buf());
    opts.data = Some(data.path().to_path_buf());

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;

    // Start recording while migration & whole-session write are running, and also trigger search.
    let rec_id = bench.start_recording().await;
    bench.send(Command::Search { query: needle.into() });

    // Record for ~6 real seconds so multiple sentences complete while background work finishes.
    tokio::time::sleep(Duration::from_millis(6_000)).await;

    // Wait for search hit, 3 migrated recordings, and the owed draft to become non-draft.
    bench
        .wait_state(
            |s| {
                let searched = s.hits.iter().any(|(id, _)| id == search_id);
                let moved = s.moving.is_none()
                    && s.library.iter().any(|r| r.title == "interview")
                    && s.library.iter().any(|r| r.title == "Weekly sync");
                let written = s.library.iter().any(|r| r.id == draft_id && !r.draft);
                searched && moved && written
            },
            Duration::from_secs(20),
        )
        .await
        .expect("search, migration, and whole-session transcription must all complete");

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Verify the recording's draft kept every finished sentence and tape length matches mic blocks.
    let mic_blocks = bench.mic.blocks();
    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording exists");
    let draft_text: String =
        recorded.lines.iter().map(|(_, s)| s.line.text.as_str()).collect::<Vec<_>>().join(" ");
    let rec_ms = (mic_blocks * u64::from(BLOCK_MS)) as u32;
    for sentence in &bench.speech.sentences {
        if sentence.end_ms + 2_000 < rec_ms {
            let word = &bench.speech.words[sentence.first_word];
            assert!(
                draft_text.contains(&word.needle),
                "sentence {} (word {}) missing from draft",
                sentence.index,
                word.needle
            );
        }
    }

    let key = keys::derive("tape", &rec_id).expect("tape key");
    let len_ms = tape::read(&bench.data().join("tape").join(&rec_id), &key, &Pass::Export, |_| {})
        .expect("tape reads back");
    assert_eq!(len_ms as u64, mic_blocks * u64::from(BLOCK_MS), "tape length mismatch");

    let done = bench.seen().of("job.done");
    for kind in ["search", "move", "write"] {
        assert!(
            done.iter().any(|l| l.text("kind") == Some(kind) && l.text("code") == Some("ok")),
            "expected job.done({kind}, ok) in {done:?}"
        );
    }
    assert!(
        !bench.seen().of("judge.verdict").iter().any(|l| l.text("to") == Some("Stalled")),
        "no stage should be judged Stalled during healthy background work"
    );

    bench.finish();
}

// ---- 4. I6: any killed part self-heals and reports what it missed ----

#[tokio::test(start_paused = true)]
async fn i6_a_lane_that_dies_comes_back_and_marks_what_it_missed() {
    let _store = STORE.lock().await;
    let panic_at_ms: u32 = 20_000;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::panic_at(panic_at_ms);

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;
    let started_at = bench.mic.started_at().expect("mic started");

    tokio::time::sleep(Duration::from_millis(36_000)).await;

    let seen_words = bench.word_seen();
    let panic_instant = started_at + Duration::from_millis(u64::from(panic_at_ms));
    let recovered_word = bench
        .speech
        .words
        .iter()
        .enumerate()
        .filter(|(_, w)| w.start_ms > panic_at_ms)
        .find_map(|(i, w)| seen_words[i].map(|at| (w, at)));
    let (word, seen_at) =
        recovered_word.expect("no word appeared after injected lane panic at 20 s");
    let recover_ms = seen_at.saturating_duration_since(panic_instant).as_millis() as u64;
    assert!(
        recover_ms <= I6_LANE_RECOVER_MS,
        "first word after panic ({}) took {recover_ms} ms > {I6_LANE_RECOVER_MS} ms",
        word.needle
    );

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording on shelf");
    let has_gap = recorded.lines.iter().any(|(_, s)| s.line.text == "…" && s.line.incomplete);
    assert!(has_gap, "draft must mark missed speech with an incomplete `…` gap line");

    // Every sentence finished before the 20 s panic must be preserved in the draft.
    let draft_text: String =
        recorded.lines.iter().map(|(_, s)| s.line.text.as_str()).collect::<Vec<_>>().join(" ");
    for sentence in &bench.speech.sentences {
        if sentence.end_ms + 2_000 < panic_at_ms {
            let w = &bench.speech.words[sentence.first_word];
            assert!(
                draft_text.contains(&w.needle),
                "pre-panic sentence {} (word {}) missing from draft",
                sentence.index,
                w.needle
            );
        }
    }

    // Observability (P1.md §2).
    assert_eq!(
        bench
            .seen()
            .of("part.state")
            .iter()
            .filter(|l| {
                l.text("part") == Some("lane.0")
                    && l.text("to") == Some("Waiting")
                    && l.text("why") == Some("panic")
            })
            .count(),
        1
    );
    assert!(
        bench
            .seen()
            .of("lane.link")
            .iter()
            .any(|l| l.text("from") == Some("Connecting") && l.text("to") == Some("Live")),
        "expected lane.link Connecting -> Live"
    );
    assert!(
        bench.seen().of("lane.link").iter().filter(|l| l.text("to") == Some("Live")).count() >= 2,
        "expected lane.link back to Live after panic"
    );
    assert_eq!(
        bench.seen().of("lane.gap").iter().filter(|l| l.text("why") == Some("panic")).count(),
        1
    );
    assert_eq!(bench.count("part.restarts"), 1);

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence() {
    let _store = STORE.lock().await;
    let panic_at_ms: u32 = 20_000;
    let speech = Speech::build(SpeechSpec::standard(35_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.gated_tape.panic_at(panic_at_ms);
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(30_000)).await;

    assert!(bench.status().is_some(), "recording must not end when tape writer panics once");
    assert!(
        !bench.notices().iter().any(|c| c == "tape"),
        "single tape panic must recover without ending the recording"
    );

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mic_blocks = bench.mic.blocks();
    assert!(mic_blocks >= 290, "mic must keep delivering across 30 s, got {mic_blocks} blocks");
    let expected_ms = mic_blocks * u64::from(BLOCK_MS);
    let key = keys::derive("tape", &rec_id).expect("tape key");
    let mut blocks = Vec::new();
    let len_ms = tape::read(&bench.data().join("tape").join(&rec_id), &key, &Pass::Export, |b| {
        blocks.push(b);
    })
    .expect("tape reads back");

    assert_eq!(len_ms as u64, expected_ms, "tape length must equal full recording length");
    let hole = blocks.iter().find(|b| b.t_ms == panic_at_ms).expect("padded block at 20 s");
    assert!(hole.pcm.iter().all(|&s| s == 0), "lost block at 20 s must be padded with silence");
    let before = blocks.iter().find(|b| b.t_ms == panic_at_ms - BLOCK_MS).expect("block before");
    let after = blocks.iter().find(|b| b.t_ms == panic_at_ms + BLOCK_MS).expect("block after");
    assert!(!before.pcm.iter().all(|&s| s == 0), "block before hole must retain recorded audio");
    assert!(!after.pcm.iter().all(|&s| s == 0), "block after hole must retain recorded audio");

    // Observability (P1.md §2, P1-15-writers.md #11).
    assert_eq!(
        bench
            .seen()
            .of("part.state")
            .iter()
            .filter(|l| {
                l.text("part") == Some("tape")
                    && l.text("to") == Some("Waiting")
                    && l.text("why") == Some("panic")
            })
            .count(),
        1,
        "expected one part.state(tape, Waiting, panic)"
    );
    let lost: Vec<_> = bench
        .seen()
        .of("keep.lost")
        .into_iter()
        .filter(|l| l.text("part") == Some("tape"))
        .collect();
    assert_eq!(lost.len(), 1, "expected one keep.lost(tape), got {lost:?}");
    let lost_n = lost[0].u64("n").expect("keep.lost n");
    assert!(lost_n >= 1);

    let pads = bench.seen().of("tape.pad");
    assert_eq!(pads.len(), 1, "expected one tape.pad event, got {pads:?}");
    assert_eq!(pads[0].u64("blocks"), Some(lost_n), "padded blocks must equal lost batch size");
    assert_eq!(bench.count("tape.padded_blocks"), lost_n);
    assert_eq!(bench.count("keep.lost_batches"), 1);

    let ends = bench.seen().of("recording.end");
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].text("code"), Some("done"));

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn the_draft_is_whole_after_a_stop() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(60_000));
    // Pick sentence 2 to interrupt mid-sentence while a non-final guess token is open
    // (after GUESS_DELAY_MS, before FINAL_DELAY_MS) so `lane::seam` saves it as `incomplete`.
    let target = speech.sentences[2].clone();
    assert!(target.word_count >= 3, "sentence 2 must have multiple words");
    let mid_word = &speech.words[target.first_word + target.word_count / 2];
    let stop_at_ms = u64::from(mid_word.end_ms) + GUESS_DELAY_MS + 100;
    assert!(
        stop_at_ms < u64::from(mid_word.end_ms) + FINAL_DELAY_MS,
        "stop must land while mid_word is still an unsettled guess"
    );

    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::SilentOnFinish;
    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(stop_at_ms)).await;
    bench.stop_recording().await;

    // Wait for `recording.end` (listen drained and synced draft, seal completed).
    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.seen().of("recording.end").is_empty() {
        assert!(Instant::now() < deadline, "recording.end did not arrive after stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording on shelf");
    assert!(
        recorded.lines.len() >= 3,
        "expected sentences 0, 1, and incomplete 2, got {:?}",
        recorded.lines
    );

    let draft_text: String =
        recorded.lines.iter().map(|(_, s)| s.line.text.as_str()).collect::<Vec<_>>().join(" ");
    for s_idx in 0..2 {
        let s = &bench.speech.sentences[s_idx];
        let first = &bench.speech.words[s.first_word];
        assert!(
            draft_text.contains(&first.needle),
            "finished sentence {s_idx} ({}) missing from draft",
            first.needle
        );
    }

    let (_, last) = recorded.lines.last().expect("last draft line");
    assert!(
        last.line.incomplete,
        "last line stopped mid-sentence must be marked incomplete: {last:?}"
    );
    let first_of_target = &bench.speech.words[target.first_word];
    assert!(
        last.line.text.contains(&first_of_target.needle),
        "last incomplete line must hold words of sentence 2 ({}), got {:?}",
        first_of_target.needle,
        last.line.text
    );
    assert!(bench.count("draft.lines") >= recorded.lines.len() as u64);
    bench.finish();
}

#[test]
fn draft_gap_table_covers_streaks_and_recovery() {
    let mut gap = DraftGap::new();
    assert_eq!(gap.next(), DraftStep::Line);
    assert_eq!(gap.pending(), None);

    // First dropped line opens a streak (`first = true`).
    assert_eq!(gap.missed(1_000, 1_500), Missed { first: true });
    let span1 = GapSpan { t0_ms: 1_000, t1_ms: 1_500, lines: 1 };
    assert_eq!(gap.next(), DraftStep::GapFirst(span1));

    // Subsequent drops merge into the same streak (`first = false`) and widen [t0, t1].
    assert_eq!(gap.missed(1_600, 2_300), Missed { first: false });
    assert_eq!(gap.missed(2_400, 4_000), Missed { first: false });
    let span3 = GapSpan { t0_ms: 1_000, t1_ms: 4_000, lines: 3 };
    assert_eq!(gap.next(), DraftStep::GapFirst(span3));

    let line = DraftGap::gap_line(span3);
    assert_eq!((line.t0, line.t1, line.text.as_str(), line.incomplete), (1_000, 4_000, "…", true));

    // Once the gap line is accepted, the tracker returns to normal (`Line`).
    assert_eq!(gap.gap_sent(), Some(span3));
    assert_eq!(gap.next(), DraftStep::Line);
    assert_eq!(gap.gap_sent(), None);

    // A later drop starts a fresh streak (`first = true`).
    assert_eq!(gap.missed(9_000, 9_500), Missed { first: true });
    assert_eq!(gap.pending(), Some(GapSpan { t0_ms: 9_000, t1_ms: 9_500, lines: 1 }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lines_the_draft_could_not_take_become_one_gap_line() {
    use bench::GatedDraft;
    use tokio::io::duplex;
    use tokio_tungstenite::{accept_async, client_async};
    use zutalk_core::app::hub::{Config, DRAFT_QUEUE_LINES};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("draft gap tempdir");
    let gated_draft = GatedDraft::new();
    let gate_for_dial = gated_draft.clone();
    let release_after_burst = Arc::new(tokio::sync::Notify::new());
    let release_for_dial = release_after_burst.clone();
    let burst_done = Arc::new(tokio::sync::Notify::new());
    let burst_done_for_dial = burst_done.clone();

    // Fast in-memory Soniox service over duplex: sends 1 line to park the gated draft writer,
    // then floods > DRAFT_QUEUE_LINES finalized sentences, waits for gate release, then sends
    // 2 recovery sentences.
    let dial: Dial = Arc::new(move |_url: &str| {
        let gated_draft = gate_for_dial.clone();
        let release_after_burst = release_for_dial.clone();
        let burst_done = burst_done_for_dial.clone();
        Box::pin(async move {
            let (client_io, server_io) = duplex(128 * 1024);
            let server = tokio::spawn(async move {
                let mut ws = accept_async(server_io).await.expect("accept duplex ws");
                // Read hello config.
                let _ = ws.next().await;

                let send_sentence = |idx: u32| {
                    let t0 = idx * 100;
                    let t1 = t0 + 80;
                    let body = serde_json::json!({
                        "tokens": [
                            {
                                "text": format!(" line{idx:04}."),
                                "start_ms": t0,
                                "end_ms": t1,
                                "is_final": true,
                                "language": "en",
                                "speaker": "1"
                            },
                            { "text": "<end>", "is_final": true }
                        ],
                        "final_audio_proc_ms": t1,
                        "total_audio_proc_ms": t1
                    });
                    Message::text(body.to_string())
                };

                // Line 0 enters the gated draft writer and parks it on the closed gate.
                ws.send(send_sentence(0)).await.expect("send line 0");
                let wait_until = Instant::now() + Duration::from_secs(4);
                while !gated_draft.has_waiter() && Instant::now() < wait_until {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert!(gated_draft.has_waiter(), "draft writer never hit the closed gate");

                // Flood DRAFT_QUEUE_LINES + 15 finalized lines while the draft writer is blocked.
                let total_burst = (DRAFT_QUEUE_LINES + 15) as u32;
                for i in 1..=total_burst {
                    ws.send(send_sentence(i)).await.expect("send burst line");
                }
                burst_done.notify_one();

                // Wait until test observes queue.full + draft.behind and opens the gate.
                release_after_burst.notified().await;

                // Send 2 more lines so DraftGap emits its single gap line followed by recovery lines.
                ws.send(send_sentence(total_burst + 1)).await.expect("send recovery line 1");
                ws.send(send_sentence(total_burst + 2)).await.expect("send recovery line 2");

                // Drain until client closes cleanly on Stop.
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Text(t) = msg
                        && t.is_empty()
                    {
                        let _ = ws.send(Message::text(r#"{"tokens":[],"finished":true}"#)).await;
                        break;
                    }
                }
            });
            let (ws, _) = client_async("ws://bench.local/draft-gap", client_io)
                .await
                .map_err(|_| End::Retry("connect"))?;
            drop(server);
            Ok(Box::new(ws) as Wire)
        })
    });

    let config = Config {
        realtime: "ws://bench.local/draft-gap".into(),
        dial,
        drafts: gated_draft.drafts(),
        ..hub::config(dir.path())
    };
    let mut app = hub::App::with(config);
    app.until(|_| true).await;
    app.send(Command::Setup {
        setup: zutalk_core::app::proto::Setup {
            spoken: vec!["en".into()],
            shown: vec!["en".into()],
            ..Default::default()
        },
    });
    app.until(|s| s.setup.shown == ["en"]).await;

    let gate = gated_draft.close_gate();
    app.send(Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("session").id;

    burst_done.notified().await;
    app.notice("draft.behind").await;

    // Release draft gate and wait for queued lines to drain so room opens before recovery lines.
    drop(gate);
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let depth = app
            .core
            .stats()
            .tally()
            .queues
            .into_iter()
            .find(|r| r.name == "draft")
            .map_or(0, |r| r.depth);
        if depth == 0 {
            break;
        }
        assert!(Instant::now() < drain_deadline, "draft queue did not drain after gate opened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    release_after_burst.notify_one();

    // Wait for `draft.gap` to be emitted once recovery lines are offered, and for at least one
    // audio block on tape so `seal()` keeps the recording on the shelf.
    let gap_deadline = Instant::now() + Duration::from_secs(5);
    while app.seen.of("draft.gap").is_empty()
        || app
            .core
            .stats()
            .tally()
            .counts
            .iter()
            .find(|r| r.name == "tape.blocks")
            .map_or(0, |r| r.n)
            == 0
    {
        assert!(Instant::now() < gap_deadline, "expected draft.gap event and tape audio block");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let end_deadline = Instant::now() + Duration::from_secs(5);
    while app.seen.of("recording.end").is_empty() {
        assert!(Instant::now() < end_deadline, "expected recording.end after stop");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Observability assertions (P1-15-writers.md #13).
    assert_eq!(
        app.seen
            .of("queue.full")
            .iter()
            .filter(|l| l.text("queue") == Some("draft") && l.text("policy") == Some("drop"))
            .count(),
        1,
        "expected exactly one queue.full(draft, drop)"
    );
    assert_eq!(
        app.seen
            .of("notice.said")
            .iter()
            .filter(|l| l.text("code") == Some("draft.behind"))
            .count(),
        1,
        "expected draft.behind notice exactly once"
    );

    let gaps = app.seen.of("draft.gap");
    assert_eq!(gaps.len(), 1, "expected one draft.gap event, got {gaps:?}");
    let gap_t0 = gaps[0].u64("t0_ms").expect("draft.gap t0_ms") as u32;
    let gap_t1 = gaps[0].u64("t1_ms").expect("draft.gap t1_ms") as u32;
    let gap_lines = gaps[0].u64("lines").expect("draft.gap lines");
    let lost_count = app
        .core
        .stats()
        .tally()
        .counts
        .iter()
        .find(|r| r.name == "draft.lost_lines")
        .map_or(0, |r| r.n);
    assert!(gap_lines > 0 && gap_lines == lost_count, "gap_lines={gap_lines} vs lost={lost_count}");

    // Verify the persisted draft on disk contains the single gap line matching [t0_ms, t1_ms]
    // and continuous line numbering.
    let recorded = hub::local_shelf(dir.path())
        .read(&rec_id)
        .expect("read shelf")
        .expect("recording on shelf");
    let gap_entries: Vec<_> =
        recorded.lines.iter().filter(|(_, s)| s.line.text == "…" && s.line.incomplete).collect();
    assert_eq!(gap_entries.len(), 1, "expected one gap line in draft, got {gap_entries:?}");
    assert_eq!(gap_entries[0].1.line.t0, gap_t0);
    assert_eq!(gap_entries[0].1.line.t1, gap_t1);

    for (idx, (key, _)) in recorded.lines.iter().enumerate() {
        assert_eq!(key, &format!("{rec_id}.0.{idx:05}"), "draft line keys must stay continuous");
    }
    assert_eq!(app.seen.refused(), 0);
}

// ---- 4b. P1-16: recording tree, Phase state machine, and water gauges ----

#[tokio::test(start_paused = true)]
async fn every_part_reports_how_far_it_has_got() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(30_000)).await;

    let (_, water) = bench.outlet().water.now();

    let stages: Vec<Stage> = water.marks.iter().map(|m| m.stage).collect();
    assert_eq!(
        stages,
        vec![
            Stage::Capture,
            Stage::Tape,
            Stage::Draft,
            Stage::Frame,
            Stage::Sent(0),
            Stage::Heard(0),
            Stage::Final(0),
        ]
    );
    let mark = |s: Stage| water.marks.iter().find(|m| m.stage == s).expect("mark present");
    let cap = mark(Stage::Capture);
    let tape = mark(Stage::Tape);
    let sent = mark(Stage::Sent(0));
    let heard = mark(Stage::Heard(0));
    let final_m = mark(Stage::Final(0));
    let draft = mark(Stage::Draft);
    let frame = mark(Stage::Frame);

    assert!(
        (29_900..=30_100).contains(&cap.at),
        "expected Capture in 29900..=30100, got {}",
        cap.at
    );
    assert!(
        cap.at.saturating_sub(tape.at) < 1_000,
        "Tape lagged Capture: {} vs {}",
        tape.at,
        cap.at
    );
    assert!(
        cap.at.saturating_sub(sent.at) < 300,
        "Sent(0) lagged Capture: {} vs {}",
        sent.at,
        cap.at
    );
    assert!(
        cap.at.saturating_sub(heard.at) < 1_500,
        "Heard(0) lagged Capture: {} vs {}",
        heard.at,
        cap.at
    );
    assert!(final_m.at > 0, "Final(0) must have advanced");
    assert!(draft.at > 0, "Draft must have advanced");
    assert!(frame.at > 0, "Frame must have advanced");

    for m in &water.marks {
        assert!(
            water.now_ms.saturating_sub(m.moved_ms) <= 2_000,
            "stage {:?} moved_ms {} too old for now_ms {}",
            m.stage,
            m.moved_ms,
            water.now_ms
        );
    }

    let lag = bench.spread("caption.lag_ms").expect("caption.lag_ms spread present");
    assert!(lag.n > 0, "caption.lag_ms must have samples");
    assert!(lag.p99 <= 1_000, "caption.lag_ms p99 {} > 1000 (spread={lag:?})", lag.p99);

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn paused_the_water_says_so_and_nothing_moves() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(5_000)).await;
    bench.send(Command::Pause);
    // Let in-flight replies/finalize settle before taking the paused baseline.
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    let (_, paused_start) = bench.outlet().water.now();
    assert!(paused_start.paused, "water.paused must be true while paused");
    let baseline: Vec<(Stage, u64)> = paused_start.marks.iter().map(|m| (m.stage, m.at)).collect();

    tokio::time::sleep(Duration::from_millis(10_000)).await;
    let (_, paused_end) = bench.outlet().water.now();
    assert!(paused_end.paused, "water.paused must stay true during pause");
    let after_pause: Vec<(Stage, u64)> = paused_end.marks.iter().map(|m| (m.stage, m.at)).collect();
    assert_eq!(after_pause, baseline, "no gauge position may move during 10 s pause");

    bench.send(Command::Resume);
    tokio::time::sleep(Duration::from_millis(5_000)).await;
    let (_, resumed) = bench.outlet().water.now();
    assert!(!resumed.paused, "water.paused must clear after resume");
    let cap_before = baseline.iter().find(|(s, _)| *s == Stage::Capture).unwrap().1;
    let cap_after = resumed.marks.iter().find(|m| m.stage == Stage::Capture).unwrap().at;
    assert!(cap_after > cap_before + 3_000, "Capture must advance again after resume");

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn the_water_comes_once_a_second_and_ends_with_the_recording() {
    let _store = STORE.lock().await;

    // Part 1: Debug capture sees ~7 water.mark lines per second (~420 in 60 s).
    let speech = Speech::build(SpeechSpec::standard(70_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;
    opts.capture_level = Level::Debug;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let (seq_start, _) = bench.outlet().water.now();
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(60_000)).await;
    let (seq_60s, water_60s) = bench.outlet().water.now();
    let delta = seq_60s - seq_start;
    assert!((59..=61).contains(&delta), "expected 59..=61 water ticks in 60 s, got {delta}");
    assert_eq!(water_60s.marks.len(), 7);

    let debug_marks = bench.seen().of("water.mark");
    assert!(
        (413..=427).contains(&debug_marks.len()),
        "expected ~420 water.mark events at Debug, got {}",
        debug_marks.len()
    );
    assert!(debug_marks.iter().all(|l| l.level == Level::Debug && l.u64("snap") == Some(0)));

    bench.stop_recording().await;
    let (seq_stopped, water_stopped) = bench.outlet().water.now();
    assert!(water_stopped.marks.is_empty(), "water must clear after stop: {water_stopped:?}");

    tokio::time::sleep(Duration::from_millis(10_000)).await;
    let (seq_later, water_later) = bench.outlet().water.now();
    assert_eq!(seq_later, seq_stopped, "water seq must not move after stop");
    assert!(water_later.marks.is_empty());
    bench.finish();

    // Part 2: Info capture sees zero water.mark lines.
    let speech2 = Speech::build(SpeechSpec::standard(10_000));
    let mut opts2 = BenchOptions::new(speech2);
    opts2.tape = TapeMode::Counting;
    opts2.capture_level = Level::Info;

    let bench2 = Bench::start(opts2).await;
    bench2.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec2 = bench2.start_recording().await;
    tokio::time::sleep(Duration::from_millis(5_000)).await;
    bench2.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(bench2.seen().count("water.mark"), 0, "Info level must emit zero water.mark lines");
    bench2.finish();
}

#[tokio::test(start_paused = true)]
async fn a_stop_keeps_everything_said_and_recorded() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(60_000));
    let target = speech.sentences[2].clone();
    let mid_word = &speech.words[target.first_word + target.word_count / 2];
    let stop_at_ms = u64::from(mid_word.end_ms) + GUESS_DELAY_MS + 100;

    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::SilentOnFinish;
    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(stop_at_ms)).await;
    bench.stop_recording().await;

    // Wait for `recording.end` (full closing + seal).
    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.seen().of("recording.end").is_empty() {
        assert!(Instant::now() < deadline, "recording.end did not arrive after stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(bench.outlet().session.now().1.is_none());
    let mic_blocks = bench.mic.blocks();
    let key = keys::derive("tape", &rec_id).expect("tape key");
    let len_ms = tape::read(&bench.data().join("tape").join(&rec_id), &key, &Pass::Export, |_| {})
        .expect("tape reads back");
    assert_eq!(len_ms as u64, mic_blocks * u64::from(BLOCK_MS));

    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording on shelf");
    let (_, last) = recorded.lines.last().expect("last draft line");
    assert!(last.line.incomplete, "mid-sentence stop must keep incomplete half-line: {last:?}");

    // Wait briefly so `Msg::Wrote` clears `state.writing` past `closing`.
    let write_deadline = Instant::now() + Duration::from_secs(5);
    while bench
        .state()
        .is_some_and(|s| s.writing.iter().any(|w| w.id == rec_id && w.step == "closing"))
    {
        assert!(Instant::now() < write_deadline, "state.writing stayed on closing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Observability: exact phase order, recording.end after last phase, 5 recording task.end events.
    let phases = bench.seen().of("recording.phase");
    let to_seq: Vec<&str> = phases.iter().filter_map(|l| l.text("to")).collect();
    assert_eq!(
        to_seq,
        [
            "Recording",
            "Closing.Capture",
            "Closing.Tape",
            "Closing.Lanes",
            "Closing.Frame",
            "Closing.Draft",
            "Sealing",
            "Ended",
        ]
    );

    let all = bench.seen().all();
    let last_phase_pos =
        all.iter().rposition(|l| l.code == "recording.phase").expect("recording.phase");
    let end_pos = all.iter().position(|l| l.code == "recording.end").expect("recording.end");
    assert!(end_pos > last_phase_pos, "recording.end must come after final recording.phase");
    assert_eq!(all[end_pos].text("code"), Some("done"));

    let rec_ends: Vec<_> = bench
        .seen()
        .of("task.end")
        .into_iter()
        .filter(|l| l.text("tree") == Some("recording"))
        .collect();
    assert_eq!(rec_ends.len(), 5, "expected 5 recording task.end events, got {rec_ends:?}");
    let mut task_names: Vec<&str> = rec_ends.iter().filter_map(|l| l.text("task")).collect();
    task_names.sort_unstable();
    assert_eq!(task_names, ["draft", "frame", "lane.0", "tape", "watch"]);
    assert!(
        rec_ends.iter().all(|l| l.text("code") != Some("panic")),
        "no recording task may end in panic: {rec_ends:?}"
    );

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_recording_whose_capture_ends_is_closed_the_same_way() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(30_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(8_000)).await;
    bench.mic.end();

    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.seen().of("recording.end").is_empty() {
        assert!(Instant::now() < deadline, "recording.end did not arrive after capture end");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(bench.notices().iter().any(|c| c == "audio.closed"));
    let phases = bench.seen().of("recording.phase");
    let to_seq: Vec<&str> = phases.iter().filter_map(|l| l.text("to")).collect();
    assert_eq!(
        to_seq,
        [
            "Recording",
            "Closing.Capture",
            "Closing.Tape",
            "Closing.Lanes",
            "Closing.Frame",
            "Closing.Draft",
            "Sealing",
            "Ended",
        ]
    );
    let last_phase = phases.last().expect("Ended phase");
    assert_eq!(last_phase.text("why"), Some("audio.closed"));
    assert_eq!(last_phase.level, Level::Error);

    let ends = bench.seen().of("recording.end");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].text("code"), Some("audio.closed"));
    assert_eq!(ends[0].level, Level::Error);

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_recording_without_a_key_has_no_lanes_and_stops_the_same_way() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.with_key = false;
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(20_000)).await;
    let (_, water) = bench.outlet().water.now();
    let stages: Vec<Stage> = water.marks.iter().map(|m| m.stage).collect();
    assert_eq!(stages, vec![Stage::Capture, Stage::Tape, Stage::Draft, Stage::Frame]);
    let cap_at = water.marks.iter().find(|m| m.stage == Stage::Capture).unwrap().at;
    let tape_at = water.marks.iter().find(|m| m.stage == Stage::Tape).unwrap().at;
    assert!(cap_at >= 19_000 && tape_at >= 18_000, "cap={cap_at} tape={tape_at}");
    assert_eq!(bench.status(), Some(Status::AudioOnly { why: "captions.no_key".into() }));

    // Pause -> Paused, Resume -> AudioOnly(captions.no_key), handled by the frame task.
    bench.send(Command::Pause);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bench.status(), Some(Status::Paused));
    bench.send(Command::Resume);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bench.status(), Some(Status::AudioOnly { why: "captions.no_key".into() }));

    tokio::time::sleep(Duration::from_millis(10_000)).await;
    bench.stop_recording().await;

    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.seen().of("recording.end").is_empty() {
        assert!(Instant::now() < deadline, "recording.end did not arrive");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let mic_blocks = bench.mic.blocks();
    let key = keys::derive("tape", &rec_id).expect("tape key");
    let len_ms = tape::read(&bench.data().join("tape").join(&rec_id), &key, &Pass::Export, |_| {})
        .expect("tape reads back");
    assert_eq!(len_ms as u64, mic_blocks * u64::from(BLOCK_MS));

    let phases = bench.seen().of("recording.phase");
    let to_seq: Vec<&str> = phases.iter().filter_map(|l| l.text("to")).collect();
    assert_eq!(
        to_seq,
        [
            "Recording",
            "Paused",
            "Recording",
            "Closing.Capture",
            "Closing.Tape",
            "Closing.Lanes",
            "Closing.Frame",
            "Closing.Draft",
            "Sealing",
            "Ended",
        ]
    );

    let rec_ends: Vec<_> = bench
        .seen()
        .of("task.end")
        .into_iter()
        .filter(|l| l.text("tree") == Some("recording"))
        .collect();
    let mut task_names: Vec<&str> = rec_ends.iter().filter_map(|l| l.text("task")).collect();
    task_names.sort_unstable();
    assert_eq!(task_names, ["draft", "frame", "tape", "watch"]);

    let part_states = bench.seen().of("part.state");
    assert!(
        !part_states.iter().any(|l| l.text("part").is_some_and(|p| p.starts_with("lane."))),
        "no lane part.state without key"
    );
    assert!(
        !part_states.iter().any(|l| l.text("to") == Some("Waiting")),
        "no part restarts without key: {part_states:?}"
    );
    assert_eq!(bench.seen().count("notice.said"), 0, "no notices without key");

    bench.finish();
}

// ---- 5. I7: stalls are seen and acted on ----

#[tokio::test(start_paused = true)]
async fn i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(35_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::Silent { connections: 1 };

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;
    let started_at = bench.mic.started_at().expect("mic started");

    // Within 12 virtual seconds, the watcher kicks the silent connection and a new one dials.
    let dial_deadline = Instant::now() + Duration::from_millis(I7_SILENT_RECOVER_MS);
    while bench.service.dials() < 2 {
        assert!(
            Instant::now() < dial_deadline,
            "lane did not switch off silent connection within {I7_SILENT_RECOVER_MS} ms (dials={})",
            bench.service.dials()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::sleep(Duration::from_millis(10_000)).await;

    let first_seen = bench
        .word_seen()
        .into_iter()
        .flatten()
        .min()
        .expect("words must appear after switching off the silent connection");
    let elapsed_ms = first_seen.saturating_duration_since(started_at).as_millis() as u64;
    assert!(
        elapsed_ms <= I7_SILENT_RECOVER_MS + 1_500,
        "first word after silent service arrived too late: {elapsed_ms} ms"
    );

    // Column health must have progressed Slow -> Stalled(silent) -> Live.
    let col_healths = bench.col0_healths();
    let idx_slow = col_healths.iter().position(|h| *h == Health::Slow);
    let idx_stalled =
        col_healths.iter().position(|h| *h == Health::Stalled { why: "silent".into() });
    let idx_back_live = idx_stalled.and_then(|s| {
        col_healths[s + 1..].iter().position(|h| *h == Health::Live).map(|o| s + 1 + o)
    });
    assert!(
        idx_slow.is_some() && idx_stalled > idx_slow && idx_back_live > idx_stalled,
        "expected column health Slow -> Stalled(silent) -> Live, got {col_healths:?}"
    );

    let mark_count_at_snap = bench.outlet().water.now().1.marks.len();
    assert_eq!(mark_count_at_snap, 7);

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording on shelf");
    let has_gap = recorded.lines.iter().any(|(_, s)| s.line.text == "…" && s.line.incomplete);
    assert!(has_gap, "draft must contain a gap line covering the silent connection");
    assert!(
        bench.seen().of("lane.gap").iter().any(|l| l.text("why") == Some("silent")),
        "expected lane.gap(why = silent)"
    );

    // Observability (`P1.md` §2 table 2, `P1-17-watch.md` #1).
    let verdicts = bench.seen().of("judge.verdict");
    let heard_verdicts: Vec<_> = verdicts
        .iter()
        .filter(|l| l.text("stage") == Some("Heard") && l.u64("lane") == Some(0))
        .collect();
    let heard_to: Vec<&str> = heard_verdicts.iter().filter_map(|l| l.text("to")).collect();
    assert_eq!(
        heard_to,
        ["Slow", "Stalled", "Live"],
        "unexpected Heard(0) verdicts: {heard_verdicts:?}"
    );
    assert_eq!(heard_verdicts[1].text("why"), Some("silent"));
    assert_eq!(heard_verdicts[1].text("action"), Some("reopen"));
    assert_eq!(heard_verdicts[1].level, Level::Warn);
    assert_eq!(heard_verdicts[2].level, Level::Info);

    let snaps = bench.seen().of("stall.snapshot");
    assert_eq!(snaps.len(), 1, "expected exactly one stall.snapshot, got {snaps:?}");
    let snap_id = snaps[0].u64("snap").expect("stall.snapshot snap");
    assert!(snap_id >= 1);
    assert_eq!(snaps[0].text("stage"), Some("Heard"));
    assert_eq!(snaps[0].u64("lane"), Some(0));
    assert_eq!(snaps[0].text("why"), Some("silent"));

    let snap_lines: Vec<_> =
        bench.seen().all().into_iter().filter(|l| l.u64("snap") == Some(snap_id)).collect();
    let snap_marks = snap_lines.iter().filter(|l| l.code == "water.mark").count();
    let snap_queues = snap_lines.iter().filter(|l| l.code == "stat.queue").count();
    let snap_tasks: Vec<_> = snap_lines.iter().filter(|l| l.code == "task.running").collect();
    let snap_end =
        snap_lines.iter().find(|l| l.code == "stall.snapshot.end").expect("stall.snapshot.end");

    assert_eq!(snap_marks, mark_count_at_snap);
    assert!(snap_queues >= 1, "expected at least one stat.queue in snapshot");
    assert!(
        snap_tasks.len() >= 5,
        "expected at least 5 task.running lines, got {}",
        snap_tasks.len()
    );
    assert!(snap_tasks.iter().any(|l| l.text("task") == Some("lane.0")));
    assert!(snap_tasks.iter().any(|l| l.text("task") == Some("watch")));
    assert_eq!(snap_end.u64("lines"), Some(snap_lines.len() as u64));

    assert_eq!(bench.seen().count("lane.kick"), 1);
    assert_eq!(
        bench
            .seen()
            .of("part.state")
            .iter()
            .filter(|l| l.text("part") == Some("lane.0") && l.text("why") == Some("silent"))
            .count(),
        1
    );
    assert_eq!(bench.count("judge.stalled"), 1);
    assert_eq!(bench.count("lane.kicks"), 1);

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn i7_a_capture_that_stops_delivering_ends_the_recording_out_loud() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    // Speak for 5 s so draft lines and tape blocks are written, then stall capture.
    tokio::time::sleep(Duration::from_millis(5_000)).await;
    let stalled_at = Instant::now();
    bench.mic.stall(true);

    bench
        .wait_session(|s| s.is_none(), Duration::from_secs(11))
        .await
        .expect("stalled capture must end recording within 11 virtual seconds");
    assert!(
        stalled_at.elapsed() <= Duration::from_secs(11),
        "took {:?} > 11 s to end stalled capture",
        stalled_at.elapsed()
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.seen().of("recording.end").is_empty() {
        assert!(Instant::now() < deadline, "recording.end did not arrive after audio.stalled");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(bench.notices().iter().any(|c| c == "audio.stalled"), "notices: {:?}", bench.notices());
    assert!(bench.outlet().session.now().1.is_none());

    // Tape and draft spoken before the stall must remain.
    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("draft must remain");
    assert!(!recorded.lines.is_empty(), "draft lines before stall must remain");
    let key = keys::derive("tape", &rec_id).expect("tape key");
    let len_ms = tape::read(&bench.data().join("tape").join(&rec_id), &key, &Pass::Export, |_| {})
        .expect("tape reads back");
    assert!(len_ms >= 4_000, "tape length before stall too short: {len_ms} ms");

    // Observability (`P1-17-watch.md` #2).
    let cap_verdicts: Vec<_> = bench
        .seen()
        .of("judge.verdict")
        .into_iter()
        .filter(|l| l.text("stage") == Some("Capture"))
        .collect();
    let cap_to: Vec<&str> = cap_verdicts.iter().filter_map(|l| l.text("to")).collect();
    assert_eq!(cap_to, ["Slow", "Stalled"], "unexpected Capture verdicts: {cap_verdicts:?}");
    assert_eq!(cap_verdicts[1].text("why"), Some("audio.stalled"));
    assert_eq!(cap_verdicts[1].text("action"), Some("end"));

    assert_eq!(bench.seen().count("stall.snapshot"), 1);
    assert!(
        bench.seen().of("recording.phase").iter().any(|l| {
            l.text("from") == Some("Recording")
                && l.text("to") == Some("Closing.Capture")
                && l.text("why") == Some("audio.stalled")
        }),
        "expected Recording -> Closing.Capture (why = audio.stalled)"
    );
    let ends = bench.seen().of("recording.end");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].text("code"), Some("audio.stalled"));
    assert_eq!(ends[0].level, Level::Error);

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_service_that_lags_is_behind_not_dead() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    // Steady healthy recording first.
    tokio::time::sleep(Duration::from_millis(5_000)).await;
    assert_eq!(bench.status(), Some(Status::Recording));

    // Inject 4 s service lag: column becomes Slow, status becomes Behind, no reconnect.
    bench.service.set_fault(Fault::Slow { extra_ms: 4_000 });
    tokio::time::sleep(Duration::from_millis(7_000)).await;
    assert_eq!(bench.status(), Some(Status::Behind));
    let (_, frame) = bench.outlet().frame.now();
    assert_eq!(frame.columns[0].health, Health::Slow);

    // Clear lag: column returns to Live, status returns to Recording, still 1 dial.
    bench.service.set_fault(Fault::None);
    tokio::time::sleep(Duration::from_millis(6_000)).await;
    assert_eq!(bench.status(), Some(Status::Recording));
    let (_, frame) = bench.outlet().frame.now();
    assert_eq!(frame.columns[0].health, Health::Live);
    assert_eq!(bench.service.dials(), 1, "lagging service must not be reconnected");

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let verdicts = bench.seen().of("judge.verdict");
    let heard_to: Vec<&str> = verdicts
        .iter()
        .filter(|l| l.text("stage") == Some("Heard") && l.u64("lane") == Some(0))
        .filter_map(|l| l.text("to"))
        .collect();
    assert_eq!(heard_to, ["Slow", "Live"]);
    assert_eq!(bench.seen().count("stall.snapshot"), 0);
    assert_eq!(bench.seen().count("lane.kick"), 0);

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn nothing_is_done_twice_for_one_stall() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(45_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;
    opts.fault = Fault::Silent { connections: 100 };

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(35_000)).await;

    // Reopen fires around 10 s, 20 s, 30 s (3 kicks total, not once per second).
    assert_eq!(bench.seen().count("lane.kick"), 3, "expected 3 kicks in 35 s");
    assert_eq!(bench.count("lane.kicks"), 3);
    assert!(
        !bench.notices().iter().any(|c| c == "silent"),
        "Reopen must not emit a `silent` notice: {:?}",
        bench.notices()
    );
    assert_eq!(bench.seen().count("stall.snapshot"), 1, "same stage + reason snaps only once");

    let heard_stalled = bench
        .seen()
        .of("judge.verdict")
        .into_iter()
        .filter(|l| l.text("stage") == Some("Heard") && l.text("to") == Some("Stalled"))
        .count();
    assert_eq!(
        heard_stalled, 1,
        "judge.verdict to Stalled fires on transition only, not every 1 s"
    );

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_recording_with_nothing_wrong_leaves_no_verdict_behind() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(5 * 60 * 1_000 + 5_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(60_000)).await;
    }

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let verdicts = bench.seen().of("judge.verdict");
    assert!(
        !verdicts.iter().any(|l| matches!(l.text("to"), Some("Stalled" | "Dead"))),
        "healthy 5 min recording must have no Stalled/Dead verdicts: {verdicts:?}"
    );
    assert_eq!(bench.seen().count("stall.snapshot"), 0);
    assert_eq!(bench.count("judge.stalled"), 0);

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_recording_without_a_key_is_quiet_from_start_to_stop() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(5 * 60 * 1_000 + 5_000));
    let mut opts = BenchOptions::new(speech);
    opts.with_key = false;
    opts.tape = TapeMode::Counting;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(60_000)).await;
    }

    let (_, water) = bench.outlet().water.now();
    assert_eq!(water.health.len(), 4, "expected 4 health entries without key: {:?}", water.health);
    assert!(
        water.health.iter().all(|(_, h)| *h == Health::Live),
        "all 4 marks must be Live without key: {:?}",
        water.health
    );
    let expected = Status::AudioOnly { why: "captions.no_key".into() };
    assert!(
        bench.statuses().iter().all(|s| *s == expected),
        "status must stay AudioOnly(captions.no_key): {:?}",
        bench.statuses()
    );

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(bench.seen().count("judge.verdict"), 0, "no verdicts without key");
    assert_eq!(bench.seen().count("stall.snapshot"), 0, "no snapshots without key");
    assert_eq!(bench.seen().count("lane.kick"), 0, "no kicks without key");
    assert_eq!(bench.seen().count("notice.said"), 0, "no notices without key");

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn i7_offline_is_said_and_captions_stop_trying_until_asked() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(6 * 60 * 1_000 + 30_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::Offline;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;
    let started_at = Instant::now();

    // Before 21 s: `connect` is said once, column is Stalled{why: "connect"}, status is
    // AudioOnly{why: "connect"}, and audio capture + tape are advancing.
    let deadline_stalled = started_at + Duration::from_secs(21);
    loop {
        if bench.notices().iter().any(|c| c == "connect")
            && bench.outlet().frame.now().1.columns.first().map(|c| &c.health)
                == Some(&Health::Stalled { why: "connect".into() })
        {
            break;
        }
        assert!(
            Instant::now() < deadline_stalled,
            "expected Stalled(connect) + notice before 21 s; elapsed={:?}, notices={:?}, frame={:?}",
            started_at.elapsed(),
            bench.notices(),
            bench.outlet().frame.now().1.columns.first().map(|c| &c.health)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(bench.notices().iter().filter(|c| *c == "connect").count(), 1);
    assert_eq!(bench.status(), Some(Status::AudioOnly { why: "connect".into() }));
    let (_, water_stalled) = bench.outlet().water.now();
    let cap_at =
        water_stalled.marks.iter().find(|m| m.stage == Stage::Capture).expect("Capture mark").at;
    let tape_at =
        water_stalled.marks.iter().find(|m| m.stage == Stage::Tape).expect("Tape mark").at;
    assert!(
        cap_at >= 15_000 && tape_at >= 15_000,
        "audio must keep recording: cap={cap_at} tape={tape_at}"
    );

    // Before 30 s (8th dial fails at ~23.75 s): column becomes Dead{why: "connect"}, `connect` is
    // not repeated.
    let deadline_dead = started_at + Duration::from_secs(30);
    loop {
        if bench.outlet().frame.now().1.columns.first().map(|c| &c.health)
            == Some(&Health::Dead { why: "connect".into() })
        {
            break;
        }
        assert!(
            Instant::now() < deadline_dead,
            "expected Dead(connect) before 30 s; elapsed={:?}, frame={:?}",
            started_at.elapsed(),
            bench.outlet().frame.now().1.columns.first().map(|c| &c.health)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(bench.service.dials(), 8, "expected 8 dials before giving up");
    assert_eq!(bench.notices().iter().filter(|c| *c == "connect").count(), 1);

    let opens_before_idle = bench.seen().count("lane.open");
    let states_before_idle = bench.seen().count("part.state");
    assert_eq!(opens_before_idle, 8);

    // Next 5 virtual minutes: no further dials, notices, `lane.open`, or `part.state`.
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(60_000)).await;
    }
    assert_eq!(bench.service.dials(), 8, "lane must not dial again once Dead");
    assert_eq!(bench.notices().iter().filter(|c| *c == "connect").count(), 1);
    assert_eq!(bench.seen().count("lane.open"), opens_before_idle);
    assert_eq!(bench.seen().count("part.state"), states_before_idle);

    // Network comes back and the user asks to Reconnect: words appear on screen within 5 s.
    bench.service.set_fault(Fault::None);
    let reconnect_at = Instant::now();
    bench.send(Command::Reconnect);
    let deadline_live = reconnect_at + Duration::from_secs(5);
    loop {
        let words_seen = bench.word_seen().iter().filter(|w| w.is_some()).count();
        let col_live =
            bench.outlet().frame.now().1.columns.first().map(|c| &c.health) == Some(&Health::Live);
        if words_seen > 0 && col_live {
            break;
        }
        assert!(
            Instant::now() < deadline_live,
            "captions did not resume within 5 s of Reconnect: words={words_seen}, health={:?}",
            bench.outlet().frame.now().1.columns.first().map(|c| &c.health)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Let a couple of seconds settle into the draft, then stop cleanly.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Draft contains a gap line covering the offline stretch.
    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording on shelf");
    let gap_line = recorded
        .lines
        .iter()
        .map(|(_, s)| &s.line)
        .find(|l| l.text == "…" && l.incomplete)
        .expect("draft must contain an incomplete gap line covering the offline stretch");
    assert_eq!(gap_line.t0, 0, "offline gap starts at recording start: {gap_line:?}");
    assert!(gap_line.t1 >= 300_000, "offline gap must span the 5+ min outage: {gap_line:?}");

    // Observability (`P1.md` §2 table 2, `P1-18-reconnect.md` #1).
    let opens = bench.seen().of("lane.open");
    let connect_opens: Vec<_> =
        opens.iter().filter(|l| l.text("code") == Some("connect")).collect();
    let ok_opens: Vec<_> = opens.iter().filter(|l| l.text("code") == Some("ok")).collect();
    assert_eq!(connect_opens.len(), 8, "expected 8 connect lane.open events: {opens:?}");
    assert_eq!(ok_opens.len(), 1, "expected 1 ok lane.open after reconnect: {opens:?}");
    assert_eq!(ok_opens[0].level, Level::Info);

    let gave_up = bench.seen().of("part.gave_up");
    assert_eq!(gave_up.len(), 1, "{gave_up:?}");
    assert_eq!(gave_up[0].text("part"), Some("lane.0"));
    assert_eq!(gave_up[0].text("why"), Some("connect"));
    assert_eq!(gave_up[0].u64("n"), Some(8));
    assert_eq!(gave_up[0].level, Level::Error);

    let sent_verdicts: Vec<String> = bench
        .seen()
        .of("judge.verdict")
        .into_iter()
        .filter(|l| l.text("stage") == Some("Sent") && l.u64("lane") == Some(0))
        .filter_map(|l| l.text("to").map(str::to_owned))
        .collect();
    assert_eq!(sent_verdicts, ["Slow", "Stalled", "Dead", "Live"], "{sent_verdicts:?}");

    assert_eq!(bench.seen().count("stall.snapshot"), 1, "only one snapshot for the outage");
    let connect_notices: Vec<_> = bench
        .seen()
        .of("notice.said")
        .into_iter()
        .filter(|l| l.text("code") == Some("connect"))
        .collect();
    assert_eq!(connect_notices.len(), 1, "{connect_notices:?}");

    let reconnect_cmd: Vec<_> = bench
        .seen()
        .of("command.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("Reconnect"))
        .collect();
    assert_eq!(reconnect_cmd.len(), 1, "{reconnect_cmd:?}");
    assert_eq!(reconnect_cmd[0].text("code"), Some("ok"));

    let reconnects = bench.seen().of("lane.reconnect");
    assert_eq!(reconnects.len(), 1, "{reconnects:?}");
    assert_eq!(reconnects[0].u64("lane"), Some(0));

    let links = bench.seen().of("lane.link");
    assert!(
        links.iter().any(|l| l.text("to") == Some("Live")),
        "expected lane.link to Live after reconnect: {links:?}"
    );
    let gaps = bench.seen().of("lane.gap");
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].text("why"), Some("connect"));

    assert_eq!(bench.count("judge.dead"), 1);

    bench.finish();
}

// ---- 6. I8: cost of one update does not grow with library size (P2) ----

async fn measure_state_bytes_for_library(count: usize) -> usize {
    let data = tempfile::tempdir().expect("data tempdir");
    hub::nobody();
    let shelf = hub::local_shelf(data.path());
    for i in 0..count {
        let id = format!("19a00000003{:08x}", i);
        seed_recording(&shelf, &id, &format!("recording {i}"), 3, None);
    }
    let speech = Speech::build(SpeechSpec::standard(10_000));
    let mut opts = BenchOptions::new(speech);
    opts.keep_store = true;
    opts.data = Some(data.path().to_path_buf());
    let bench = Bench::start(opts).await;
    bench
        .wait_state(|s| s.library.len() == count, Duration::from_secs(10))
        .await
        .expect("library loaded");

    let target = format!("19a00000003{:08x}", 0);
    bench.send(Command::Change {
        id: target.clone(),
        change: Change::Title { text: "renamed-once".into() },
    });
    bench
        .wait_state(
            |s| s.library.iter().any(|r| r.id == target && r.title == "renamed-once"),
            Duration::from_secs(10),
        )
        .await
        .expect("title change reflected in State");
    let bytes = bench.latest_state_bytes();
    bench.finish();
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "P2：状态瘦身"]
async fn i8_what_one_change_sends_does_not_grow_with_the_library() {
    let _store = STORE.lock().await;
    let bytes_5 = measure_state_bytes_for_library(5).await;
    let bytes_100 = measure_state_bytes_for_library(100).await;
    let ratio = bytes_100 as f64 / bytes_5.max(1) as f64;
    eprintln!("i8: bytes_5={bytes_5} bytes_100={bytes_100} ratio={ratio:.2}");
    assert!(
        ratio <= 1.2,
        "State size grew with library: 5 recs = {bytes_5} B, 100 recs = {bytes_100} B (ratio {ratio:.2} > 1.2)"
    );
}

#[tokio::test(start_paused = true)]
async fn five_minute_recording_with_twenty_commands_measures_hub_turns() {
    let _store = STORE.lock().await;
    let data = tempfile::tempdir().expect("data tempdir");
    hub::nobody();
    let existing = "19a0000000400000001";
    seed_recording(&hub::local_shelf(data.path()), existing, "existing", 5, Some("findme"));

    let speech = Speech::build(SpeechSpec::standard(5 * 60 * 1_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Counting;
    opts.data = Some(data.path().to_path_buf());

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    // 20 varied commands spread across 5 virtual minutes of recording.
    for i in 0..20usize {
        tokio::time::sleep(Duration::from_millis(15_000)).await;
        match i % 5 {
            0 => bench.send(Command::Change {
                id: existing.into(),
                change: Change::Title { text: format!("Renamed {i}") },
            }),
            1 => bench.send(Command::Search { query: "findme".into() }),
            2 => bench.send(Command::Open { id: existing.into() }),
            3 => bench.send(Command::Captions { show: i % 2 == 0 }),
            _ => bench
                .send(Command::Term { said: format!("Term{i}"), written: Some(BTreeMap::new()) }),
        }
    }
    bench.send(Command::Close);
    let _ = rec_id;
    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let spread = bench.spread("hub.turn_us");
    let slow = bench.seen().of("hub.turn.slow");
    eprintln!("P1-20 5-min bench: hub.turn_us={spread:?} hub.turn.slow={} ({slow:?})", slow.len());
    assert!(spread.as_ref().is_some_and(|s| s.n >= 20), "expected >= 20 hub turns: {spread:?}");
    bench.finish();
}

// ---- 7. P1-22: Command::Diagnose bundle ----

fn diagnose_dirs(data: &std::path::Path) -> Vec<PathBuf> {
    let base = data.join("diagnose");
    let Ok(entries) = fs::read_dir(base) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    dirs
}

fn count_files(dir: &std::path::Path) -> usize {
    let mut n = 0;
    for entry in fs::read_dir(dir).expect("read bundle dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            n += count_files(&path);
        } else {
            n += 1;
        }
    }
    n
}

#[tokio::test(start_paused = true)]
async fn a_diagnosis_says_how_things_are() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(40_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::panic_at(10_000);

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(30_000)).await;
    bench.send(Command::Diagnose);
    bench.wait_notice("diagnose.done", Duration::from_secs(10)).await.expect("diagnose.done");

    let entries = diagnose_dirs(bench.data());
    assert_eq!(entries.len(), 1, "expected exactly one bundle dir, got {entries:?}");
    let bundle = &entries[0];
    assert!(
        !bundle.file_name().unwrap().to_string_lossy().ends_with(".tmp"),
        "temp dir must be renamed: {bundle:?}"
    );

    let about_raw = fs::read_to_string(bundle.join("about.json")).expect("read about.json");
    let about: serde_json::Value = serde_json::from_str(&about_raw).expect("parse about.json");
    assert_eq!(about["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(about["who"].as_str(), Some("local"));
    assert_eq!(about["keys"]["soniox"].as_str(), Some("Own"));
    assert_eq!(about["shown"], serde_json::json!(["en", "zh"]));

    let water_raw = fs::read_to_string(bundle.join("water.json")).expect("read water.json");
    let water: serde_json::Value = serde_json::from_str(&water_raw).expect("parse water.json");
    let marks = water["marks"].as_array().expect("water marks array");
    let capture_mark = marks.iter().find(|m| m["stage"] == "Capture").expect("Capture mark");
    let capture_at = capture_mark["at"].as_u64().expect("Capture at");
    assert!(
        (25_000..=35_000).contains(&capture_at),
        "expected Capture around 30_000 ms, got {capture_at}"
    );
    assert!(marks.iter().any(|m| m["stage"] == "Tape"), "missing Tape mark");
    let sent0 = marks
        .iter()
        .find(|m| m["stage"] == serde_json::json!({ "Sent": 0 }))
        .expect("Sent(0) mark");

    let stats_raw = fs::read_to_string(bundle.join("stats.json")).expect("read stats.json");
    let stats: serde_json::Value = serde_json::from_str(&stats_raw).expect("parse stats.json");
    let queues = stats["queues"].as_array().expect("stats queues");
    for qname in ["hub.mail", "tape", "draft", "audio", "lane.events", "desk"] {
        let row = queues.iter().find(|q| q["name"].as_str() == Some(qname)).unwrap_or_else(|| {
            panic!("missing queue `{qname}` in stats.json: {queues:?}");
        });
        assert!(row["capacity"].as_u64().unwrap_or(0) > 0, "queue `{qname}` capacity");
        assert!(row["depth"].is_u64(), "queue `{qname}` depth");
        assert!(row["deepest"].is_u64(), "queue `{qname}` deepest");
    }
    let counts = stats["counts"].as_array().expect("stats counts");
    let restarts = counts
        .iter()
        .find(|c| c["name"].as_str() == Some("part.restarts"))
        .and_then(|c| c["n"].as_u64());
    assert_eq!(restarts, Some(1), "expected part.restarts == 1, counts={counts:?}");

    let tasks_raw = fs::read_to_string(bundle.join("tasks.json")).expect("read tasks.json");
    let tasks: serde_json::Value = serde_json::from_str(&tasks_raw).expect("parse tasks.json");
    let task_list = tasks.as_array().expect("tasks array");
    for tname in ["hub", "stats", "desk", "recording", "lane.0", "frame", "watch"] {
        assert!(
            task_list.iter().any(|t| t["task"].as_str() == Some(tname)),
            "missing task `{tname}` in tasks.json: {task_list:?}"
        );
    }
    assert!(
        task_list
            .iter()
            .any(|t| t["kind"].as_str() == Some("blocking")
                && t["task"].as_str() == Some("desk.job")),
        "missing running desk.job blocking call in tasks.json: {task_list:?}"
    );

    // Observable events: command.done, job.done, diagnose.wrote.
    let cmd_done: Vec<_> = bench
        .seen()
        .of("command.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("Diagnose") && l.text("code") == Some("ok"))
        .collect();
    assert_eq!(cmd_done.len(), 1, "expected one command.done for Diagnose");

    let job_done: Vec<_> = bench
        .seen()
        .of("job.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("diagnose") && l.text("code") == Some("ok"))
        .collect();
    assert_eq!(job_done.len(), 1, "expected one job.done for diagnose");

    let wrote = bench.seen().of("diagnose.wrote");
    assert_eq!(wrote.len(), 1, "expected one diagnose.wrote event");
    let actual_files = count_files(bundle) as u64;
    assert_eq!(wrote[0].u64("files"), Some(actual_files));
    assert!(wrote[0].u64("bytes").unwrap_or(0) > 0);

    // Emit acceptance evidence when run with --nocapture.
    eprintln!("P1-22 bundle dir: {}", bundle.file_name().unwrap().to_string_lossy());
    for entry in fs::read_dir(bundle).expect("list bundle") {
        let p = entry.expect("entry").path();
        let meta = fs::metadata(&p).expect("meta");
        eprintln!("  {} ({} bytes)", p.file_name().unwrap().to_string_lossy(), meta.len());
    }
    eprintln!("P1-22 about.json:\n{about_raw}");
    eprintln!("P1-22 water.json Sent(0):\n{}", serde_json::to_string_pretty(sent0).unwrap());

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_diagnosis_holds_nothing_that_was_said_and_no_secret() {
    let _store = STORE.lock().await;
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let log_dir = data_dir.path().join("logs");
    let (scope, guard) = zutalk_log::files(&log_dir, "zutalk", Level::Debug, Limits::default())
        .expect("open file log");
    let entered = scope.enter();

    let speech = Speech::build(SpeechSpec::standard(125_000));
    let forbidden_words = speech.forbidden_words();
    let fake_device_name = "StudioMicDeviceName99";
    let mut opts = BenchOptions::new(speech);
    opts.capture_log = false;
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::panic_at(20_000);
    opts.data = Some(data_dir.path().to_path_buf());
    opts.devices =
        vec![Device { id: "studio-mic".into(), name: fake_device_name.into(), default: true }];

    let bench = Bench::start(opts).await;
    let data_path = bench.data().to_string_lossy().into_owned();
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    let secret_title = "SecretTitleZeta99";
    let secret_query = "QueryNeedleOmega88";

    tokio::time::sleep(Duration::from_millis(60_000)).await;
    bench.send(Command::Change {
        id: rec_id.clone(),
        change: Change::Title { text: secret_title.into() },
    });
    bench.send(Command::Search { query: secret_query.into() });
    tokio::time::sleep(Duration::from_millis(60_000)).await;

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Let the file log writer thread drain queued lines before Diagnose copies `logs/`.
    std::thread::sleep(Duration::from_millis(50));

    bench.send(Command::Diagnose);
    bench.wait_notice("diagnose.done", Duration::from_secs(10)).await.expect("diagnose.done");
    bench.finish();

    drop(entered);
    drop(guard);

    let dirs = diagnose_dirs(data_dir.path());
    assert_eq!(dirs.len(), 1, "expected one diagnose bundle");
    let bundle = &dirs[0];
    let copied_logs = bundle.join("logs");
    assert!(copied_logs.is_dir(), "expected logs/ subdir copied into bundle");
    let copied_jsonl: Vec<_> = fs::read_dir(&copied_logs)
        .expect("read bundle logs")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    assert!(!copied_jsonl.is_empty(), "expected at least one .jsonl file in bundle/logs");

    let mut forbidden = forbidden_words;
    forbidden.push(secret_title.into());
    forbidden.push(secret_query.into());
    for spk in SPEAKERS {
        forbidden.push(spk.into());
    }
    forbidden.push(hub::local_did());
    forbidden.push("sk-own-test-key".into());
    forbidden.push(data_path);
    forbidden.push(fake_device_name.into());

    if let Err(leak) = check_bundle_dir(bundle, &forbidden) {
        panic!("diagnose bundle privacy check failed: {leak}");
    }
}

#[tokio::test(start_paused = true)]
async fn what_goes_in_the_bundle_is_written_down_field_by_field() {
    let _store = STORE.lock().await;
    // Compile-time + serialization check on the exact key set of `Facts` (P1.md §3.13.6).
    let expected_keys: Vec<&str> = vec![
        "arch", "captions", "keys", "level", "os", "shown", "sound", "spoken", "up_ms", "version",
        "who",
    ];
    let sample = Facts {
        version: "0.9.0",
        os: "macos",
        arch: "aarch64",
        level: "info",
        up_ms: 42,
        who: "local",
        spoken: vec!["en".into()],
        shown: vec!["zh".into()],
        sound: "mic",
        captions: false,
        keys: BTreeMap::new(),
    };
    let sample_val = serde_json::to_value(&sample).expect("serialize Facts");
    let mut sample_keys: Vec<&str> =
        sample_val.as_object().unwrap().keys().map(String::as_str).collect();
    sample_keys.sort();
    assert_eq!(sample_keys, expected_keys);

    // End-to-end: a language string containing spaces is sanitized to "!" in about.json.
    let speech = Speech::build(SpeechSpec::standard(10_000));
    let bench = Bench::start(BenchOptions::new(speech)).await;
    let setup = Setup {
        spoken: vec!["en".into(), "not a plain language code".into()],
        shown: vec!["zh".into()],
        ..Setup::default()
    };
    bench.send(Command::Setup { setup: setup.clone() });
    bench
        .wait_state(|s| s.setup.spoken == setup.spoken, Duration::from_secs(10))
        .await
        .expect("setup applied");

    bench.send(Command::Diagnose);
    bench.wait_notice("diagnose.done", Duration::from_secs(10)).await.expect("diagnose.done");

    let bundle = &diagnose_dirs(bench.data())[0];
    let about: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(bundle.join("about.json")).unwrap()).unwrap();
    let mut actual_keys: Vec<&str> =
        about.as_object().unwrap().keys().map(String::as_str).collect();
    actual_keys.sort();
    assert_eq!(actual_keys, expected_keys);
    assert_eq!(about["spoken"], serde_json::json!(["en", "!"]));

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn only_the_newest_three_are_kept() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(10_000));
    let bench = Bench::start(BenchOptions::new(speech)).await;

    let mut all_seen: Vec<u64> = Vec::new();
    for i in 1..=5usize {
        bench.send(Command::Diagnose);
        bench.wait_notice_count("diagnose.done", i, Duration::from_secs(10)).await.unwrap();
        let current: Vec<u64> = diagnose_dirs(bench.data())
            .into_iter()
            .filter_map(|p| p.file_name()?.to_str()?.parse::<u64>().ok())
            .collect();
        let newest = *current.iter().max().expect("at least one bundle");
        all_seen.push(newest);
    }

    let kept: Vec<u64> = diagnose_dirs(bench.data())
        .into_iter()
        .filter_map(|p| p.file_name()?.to_str()?.parse::<u64>().ok())
        .collect();
    assert_eq!(kept.len(), DIAGNOSE_KEPT_DIRS);
    assert_eq!(kept, all_seen[all_seen.len() - DIAGNOSE_KEPT_DIRS..]);
    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn a_diagnosis_that_cannot_be_written_says_so() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(10_000));
    let bench = Bench::start(BenchOptions::new(speech)).await;

    // Occupy `<data>/diagnose` with a regular file so bundle directory creation fails.
    fs::write(bench.data().join("diagnose"), b"not a dir").expect("block diagnose path");

    bench.send(Command::Diagnose);
    bench.wait_notice("diagnose", Duration::from_secs(10)).await.expect("diagnose failure notice");

    let failed: Vec<_> = bench
        .seen()
        .of("job.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("diagnose") && l.text("code") == Some("diagnose"))
        .collect();
    assert_eq!(failed.len(), 1, "expected one failed diagnose job.done");
    assert_eq!(failed[0].level, Level::Warn);
    assert!(
        bench.seen().of("diagnose.wrote").is_empty(),
        "diagnose.wrote must not be emitted on failure"
    );

    bench.finish();
}

// ---- 8. P1-24: Shell/Screen watermarks and untrusted seq clamp ----

#[tokio::test(start_paused = true)]
async fn the_screen_mark_follows_what_the_interface_says_it_drew() {
    let _store = STORE.lock().await;
    let speech = Speech::build(SpeechSpec::standard(30_000));
    let bench = Bench::start(BenchOptions::new(speech)).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _id = bench.start_recording().await;

    // Report Shell and Screen once per virtual second while recording: both advance and stay Live.
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let (frame_seq, _) = bench.outlet().frame.now();
        if frame_seq > 0 {
            bench.pushed(frame_seq);
            bench.drawn(frame_seq);
        }
    }
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let water = bench.last_water().expect("water sampled");
    let shell_mark = water.marks.iter().find(|m| m.stage == Stage::Shell).expect("Shell mark");
    let screen_mark = water.marks.iter().find(|m| m.stage == Stage::Screen).expect("Screen mark");
    assert!(shell_mark.at > 0);
    assert!(screen_mark.at > 0);
    assert_eq!(
        water.health.iter().find(|(s, _)| *s == Stage::Shell).map(|(_, h)| h),
        Some(&Health::Live)
    );
    assert_eq!(
        water.health.iter().find(|(s, _)| *s == Stage::Screen).map(|(_, h)| h),
        Some(&Health::Live)
    );

    // Keep advancing Shell but stop calling `drawn` for 6 virtual seconds: Screen turns Slow,
    // emits one judge.verdict(Screen -> Slow), zero stall.snapshot, and takes no action.
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let (frame_seq, _) = bench.outlet().frame.now();
        bench.pushed(frame_seq);
    }
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let slow_water = bench.last_water().expect("water sampled while screen stopped");
    assert_eq!(
        slow_water.health.iter().find(|(s, _)| *s == Stage::Screen).map(|(_, h)| h),
        Some(&Health::Slow),
        "expected Screen to be Slow after 6 s without drawn(): {:?}",
        slow_water.health
    );

    let screen_verdicts: Vec<_> = bench
        .seen()
        .of("judge.verdict")
        .into_iter()
        .filter(|l| l.text("stage") == Some("Screen") && l.text("to") == Some("Slow"))
        .collect();
    assert_eq!(screen_verdicts.len(), 1, "expected one judge.verdict for Screen -> Slow");
    assert_eq!(screen_verdicts[0].text("action"), Some("none"));
    assert!(
        bench.seen().of("stall.snapshot").is_empty(),
        "Screen Slow must not trigger a stall.snapshot"
    );

    // Untrusted `drawn` seq larger than any emitted Frame must never push Screen past Frame.
    let (frame_seq, _) = bench.outlet().frame.now();
    bench.drawn(frame_seq + 10_000);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let clamped_water = bench.last_water().expect("water after untrusted drawn");
    let frame_at = clamped_water.marks.iter().find(|m| m.stage == Stage::Frame).unwrap().at;
    let screen_at = clamped_water.marks.iter().find(|m| m.stage == Stage::Screen).unwrap().at;
    assert!(screen_at <= frame_at, "Screen ({screen_at}) must never exceed Frame ({frame_at})");

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    bench.finish();
}

// ---- 9. P1-25: E10 event catalogue, read-the-log sequences, and stats audit ----

/// Parses every event code listed under `## 事件` in `docs/EVENTS.md` in document order.
fn parse_events_catalogue(doc: &str) -> Vec<String> {
    let mut in_table = false;
    let mut codes = Vec::new();
    for line in doc.lines() {
        if line.starts_with("## ") {
            in_table = line.trim() == "## 事件";
            continue;
        }
        if !in_table || !line.starts_with("| `") {
            continue;
        }
        let Some(cell) = line.split('|').nth(1) else { continue };
        let mut rest = cell;
        while let Some(start) = rest.find('`') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('`') else { break };
            codes.push(after[..end].to_owned());
            rest = &after[end + 1..];
        }
    }
    codes
}

#[test]
fn every_event_in_the_catalogue_can_be_told_apart() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/EVENTS.md");
    let doc = fs::read_to_string(&path).expect("read docs/EVENTS.md");
    let codes = parse_events_catalogue(&doc);
    assert!(!codes.is_empty(), "docs/EVENTS.md has no event rows under `## 事件`");

    let mut seen = std::collections::BTreeSet::new();
    for code in &codes {
        assert!(
            zutalk_log::is_code(code),
            "event code `{code}` in docs/EVENTS.md fails zutalk_log::is_code"
        );
        assert!(seen.insert(code.clone()), "duplicate event code `{code}` in docs/EVENTS.md");
    }
}

/// Reads all JSONL lines written to `dir` in chronological order and parses each as a JSON value.
fn read_jsonl(dir: &std::path::Path) -> Vec<(String, serde_json::Value)> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .expect("read log dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let content = fs::read_to_string(&path).expect("read jsonl");
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let val: serde_json::Value = serde_json::from_str(line).expect("valid json line");
            out.push((line.to_owned(), val));
        }
    }
    out
}

type LogStep = (&'static str, fn(&serde_json::Value) -> bool);

/// Asserts that `steps` appear in order as a subsequence of `entries`, returning matched indices.
fn assert_log_sequence(entries: &[(String, serde_json::Value)], steps: &[LogStep]) -> Vec<usize> {
    let mut cursor = 0usize;
    let mut matched = Vec::with_capacity(steps.len());
    for (label, pred) in steps {
        let found = entries[cursor..].iter().position(|(_, v)| pred(v)).map(|off| cursor + off);
        let Some(idx) = found else {
            let summary: Vec<&str> =
                entries.iter().map(|(_, v)| v["event"].as_str().unwrap_or("?")).collect();
            panic!("missing step `{label}` after index {cursor}; events={summary:?}");
        };
        matched.push(idx);
        cursor = idx + 1;
    }
    matched
}

#[tokio::test(start_paused = true)]
async fn log_tells_the_story_of_a_silent_service() {
    let _store = STORE.lock().await;
    let log_dir = tempfile::tempdir().expect("log tempdir");
    let (scope, guard) =
        zutalk_log::files(log_dir.path(), "zutalk", Level::Info, Limits::default())
            .expect("open file log");
    let entered = scope.enter();

    let speech = Speech::build(SpeechSpec::standard(35_000));
    let mut opts = BenchOptions::new(speech);
    opts.capture_log = false;
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::Silent { connections: 1 };

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(22_000)).await;
    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();

    drop(entered);
    drop(guard);

    let entries = read_jsonl(log_dir.path());
    let steps: &[LogStep] = &[
        ("recording.start", |v| v["event"] == "recording.start"),
        ("lane.open(ok)", |v| v["event"] == "lane.open" && v["code"] == "ok"),
        ("judge.verdict(Heard->Slow)", |v| {
            v["event"] == "judge.verdict" && v["stage"] == "Heard" && v["to"] == "Slow"
        }),
        ("judge.verdict(Heard->Stalled,silent,reopen)", |v| {
            v["event"] == "judge.verdict"
                && v["stage"] == "Heard"
                && v["to"] == "Stalled"
                && v["why"] == "silent"
                && v["action"] == "reopen"
        }),
        ("stall.snapshot(Heard,silent)", |v| {
            v["event"] == "stall.snapshot" && v["stage"] == "Heard" && v["why"] == "silent"
        }),
        ("stall.snapshot.end", |v| v["event"] == "stall.snapshot.end"),
        ("lane.kick(silent)", |v| v["event"] == "lane.kick" && v["why"] == "silent"),
        ("part.state(lane.0->Waiting,silent)", |v| {
            v["event"] == "part.state"
                && v["part"] == "lane.0"
                && v["to"] == "Waiting"
                && v["why"] == "silent"
        }),
        ("lane.open(ok) after kick", |v| v["event"] == "lane.open" && v["code"] == "ok"),
        ("lane.gap(silent)", |v| v["event"] == "lane.gap" && v["why"] == "silent"),
        ("judge.verdict(Heard->Live)", |v| {
            v["event"] == "judge.verdict" && v["stage"] == "Heard" && v["to"] == "Live"
        }),
        ("recording.end(done)", |v| v["event"] == "recording.end" && v["code"] == "done"),
    ];
    let matched = assert_log_sequence(&entries, steps);
    eprintln!("=== STORY 1: SILENT SERVICE ===");
    for idx in matched {
        eprintln!("{}", entries[idx].0);
    }
}

#[tokio::test(start_paused = true)]
async fn log_tells_the_story_of_going_offline_and_reconnecting() {
    let _store = STORE.lock().await;
    let log_dir = tempfile::tempdir().expect("log tempdir");
    let (scope, guard) =
        zutalk_log::files(log_dir.path(), "zutalk", Level::Info, Limits::default())
            .expect("open file log");
    let entered = scope.enter();

    let speech = Speech::build(SpeechSpec::standard(60_000));
    let mut opts = BenchOptions::new(speech);
    opts.capture_log = false;
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::Offline;

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    // 30 virtual seconds suffices for 8 failed dials, Stalled, gave_up, and Dead.
    tokio::time::sleep(Duration::from_millis(30_000)).await;
    bench.service.set_fault(Fault::None);
    bench.send(Command::Reconnect);
    tokio::time::sleep(Duration::from_millis(6_000)).await;

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();

    drop(entered);
    drop(guard);

    let entries = read_jsonl(log_dir.path());
    let connect_opens =
        entries.iter().filter(|(_, v)| v["event"] == "lane.open" && v["code"] == "connect").count();
    assert_eq!(connect_opens, 8, "expected 8 failed lane.open(connect) lines");
    let steps: &[LogStep] = &[
        ("recording.start", |v| v["event"] == "recording.start"),
        ("lane.open(connect)", |v| v["event"] == "lane.open" && v["code"] == "connect"),
        ("judge.verdict(Sent->Stalled,connect)", |v| {
            v["event"] == "judge.verdict"
                && v["stage"] == "Sent"
                && v["to"] == "Stalled"
                && v["why"] == "connect"
        }),
        ("stall.snapshot(Sent,connect)", |v| {
            v["event"] == "stall.snapshot" && v["stage"] == "Sent" && v["why"] == "connect"
        }),
        ("stall.snapshot.end", |v| v["event"] == "stall.snapshot.end"),
        ("part.gave_up(lane.0,connect)", |v| {
            v["event"] == "part.gave_up" && v["part"] == "lane.0" && v["why"] == "connect"
        }),
        ("judge.verdict(Sent->Dead,connect)", |v| {
            v["event"] == "judge.verdict"
                && v["stage"] == "Sent"
                && v["to"] == "Dead"
                && v["why"] == "connect"
        }),
        ("command.done(Reconnect,ok)", |v| {
            v["event"] == "command.done" && v["kind"] == "Reconnect" && v["code"] == "ok"
        }),
        ("lane.reconnect", |v| v["event"] == "lane.reconnect" && v["lane"] == 0),
        ("lane.open(ok)", |v| v["event"] == "lane.open" && v["code"] == "ok"),
        ("lane.link(Live)", |v| v["event"] == "lane.link" && v["to"] == "Live"),
        ("judge.verdict(Sent->Live)", |v| {
            v["event"] == "judge.verdict" && v["stage"] == "Sent" && v["to"] == "Live"
        }),
        ("recording.end(done)", |v| v["event"] == "recording.end" && v["code"] == "done"),
    ];
    let matched = assert_log_sequence(&entries, steps);
    eprintln!("=== STORY 2: OFFLINE TO GAVE_UP AND RECONNECT ===");
    for idx in matched {
        eprintln!("{}", entries[idx].0);
    }
}

#[tokio::test(start_paused = true)]
async fn log_tells_the_story_of_a_tape_writer_panic() {
    let _store = STORE.lock().await;
    let log_dir = tempfile::tempdir().expect("log tempdir");
    let (scope, guard) =
        zutalk_log::files(log_dir.path(), "zutalk", Level::Info, Limits::default())
            .expect("open file log");
    let entered = scope.enter();

    let speech = Speech::build(SpeechSpec::standard(25_000));
    let mut opts = BenchOptions::new(speech);
    opts.capture_log = false;
    opts.tape = TapeMode::Gated;

    let bench = Bench::start(opts).await;
    bench.gated_tape.panic_at(10_000);
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let _rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(20_000)).await;
    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    bench.finish();

    drop(entered);
    drop(guard);

    let entries = read_jsonl(log_dir.path());
    let steps: &[LogStep] = &[
        ("recording.start", |v| v["event"] == "recording.start"),
        ("keep.lost(tape,panic)", |v| {
            v["event"] == "keep.lost" && v["part"] == "tape" && v["why"] == "panic"
        }),
        ("part.state(tape->Waiting,panic)", |v| {
            v["event"] == "part.state"
                && v["part"] == "tape"
                && v["to"] == "Waiting"
                && v["why"] == "panic"
        }),
        ("part.state(tape->Opening)", |v| {
            v["event"] == "part.state" && v["part"] == "tape" && v["to"] == "Opening"
        }),
        ("part.state(tape->Running)", |v| {
            v["event"] == "part.state" && v["part"] == "tape" && v["to"] == "Running"
        }),
        ("tape.pad", |v| v["event"] == "tape.pad"),
        ("recording.end(done)", |v| v["event"] == "recording.end" && v["code"] == "done"),
    ];
    let matched = assert_log_sequence(&entries, steps);
    eprintln!("=== STORY 3: TAPE WRITER PANIC ===");
    for idx in matched {
        eprintln!("{}", entries[idx].0);
    }
}

#[tokio::test(start_paused = true)]
async fn two_minute_recording_tally_matches_the_table() {
    let _store = STORE.lock().await;
    let export_dir = tempfile::tempdir().expect("export tempdir");
    let export_path = export_dir.path().join("out.md");

    let speech = Speech::build(SpeechSpec::standard(130_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::panic_at(30_000);

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;

    tokio::time::sleep(Duration::from_millis(60_000)).await;
    bench.send(Command::Search { query: "w000".into() });
    bench.send(Command::Export {
        id: rec_id.clone(),
        exporter: "markdown".into(),
        languages: vec![],
        to: export_path.to_string_lossy().into_owned(),
    });
    tokio::time::sleep(Duration::from_millis(60_000)).await;

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let tally = bench.tally();
    let count_names: Vec<&str> = tally.counts.iter().map(|r| r.name).collect();
    let peak_names: Vec<&str> = tally.peaks.iter().map(|r| r.name).collect();
    let spread_names: Vec<&str> = tally.spreads.iter().map(|r| r.name).collect();
    let queue_names: Vec<&str> = tally.queues.iter().map(|r| r.name).collect();

    eprintln!("=== 2-MIN TALLY AUDIT ===");
    eprintln!("counts ({}): {count_names:?}", count_names.len());
    eprintln!("peaks ({}): {peak_names:?}", peak_names.len());
    eprintln!("spreads ({}): {spread_names:?}", spread_names.len());
    eprintln!("queues ({}): {queue_names:?}", queue_names.len());

    // Every name produced must belong to the documented P1.md §3.13.4 table.
    let known_counts = [
        "log.lost",
        "log.refused",
        "task.started",
        "task.panics",
        "part.restarts",
        "part.gave_up",
        "frame.made",
        "frame.cut",
        "lane.gaps",
        "state.sent",
        "notices.said",
        "commands.done",
        "commands.refused",
        "hub.turns",
        "hub.turns.slow",
        "capture.blocks",
        "capture.padded_ms",
        "capture.ring.lost_samples",
        "capture.reopens",
        "tape.blocks",
        "tape.padded_blocks",
        "draft.lines",
        "draft.lost_lines",
        "keep.lost_batches",
        "judge.stalled",
        "judge.dead",
        "lane.kicks",
        "jobs.done",
        "jobs.failed",
        "search.stale",
        "refresh.merged",
        "shell.pushed",
        "shell.sinks.gone",
        "shell.beats.stalled",
        "ui.gaps",
    ];
    let known_peaks = ["tree.tasks", "frame.bound_bytes"];
    let known_spreads = [
        "hub.turn_us",
        "tape.batch_ms",
        "draft.batch_ms",
        "caption.lag_ms",
        "lane.keys_ms",
        "lane.dial_ms",
        "desk.wait_ms",
        "desk.job_ms",
        "shell.beat_ms",
        "ui.gap_ms",
    ];
    let known_queues = [
        "capture.pump",
        "capture.ended",
        "capture.file",
        "audio",
        "tape",
        "lane.events",
        "draft",
        "hub.mail",
        "desk",
        "shell.joins",
        "shell.updates",
        "write.wav",
        "city.audio",
    ];
    for name in &count_names {
        assert!(known_counts.contains(name), "undocumented counter `{name}`");
    }
    for name in &peak_names {
        assert!(known_peaks.contains(name), "undocumented peak `{name}`");
    }
    for name in &spread_names {
        assert!(known_spreads.contains(name), "undocumented spread `{name}`");
    }
    for name in &queue_names {
        assert!(known_queues.contains(name), "undocumented queue `{name}`");
    }
    assert!(count_names.contains(&"part.restarts"));
    assert!(count_names.contains(&"lane.gaps"));
    assert!(count_names.contains(&"jobs.done"));
    assert!(spread_names.contains(&"caption.lag_ms"));
    assert!(spread_names.contains(&"hub.turn_us"));
    assert!(queue_names.contains(&"desk"));
    assert!(queue_names.contains(&"tape"));
    assert!(queue_names.contains(&"draft"));

    bench.finish();
}
