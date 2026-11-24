//! Liveness, isolation, recovery, and log-privacy tests over the full-core simulation bench
//! (`docs/build/phases/P1.md` §2, §3.10, §3.13.13–§3.13.14, `P1-08-bench.md`).

mod bench;
mod common;
mod hub;

use bench::mic::encode_block_index;
use bench::service::{Delays, FINAL_DELAY_MS, Fault, GUESS_DELAY_MS, SILENCE_PROGRESS_MS, Service};
use bench::speech::{BLOCK_MS, SPEAKERS, Speech, SpeechSpec};
use bench::{
    Bench, BenchOptions, MAX_FRAME_BYTES, STORE, TapeMode, check_log_lines, summarize_latencies,
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
use zutalk_core::app::proto::{Change, Command, Status};
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
/// Recovery budget for a silent service connection (`P1-08-bench.md` I7).
const I7_SILENT_RECOVER_MS: u64 = 20_000;
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
    while elapsed < total_ms {
        let chunk = (total_ms - elapsed).min(step);
        tokio::time::sleep(Duration::from_millis(chunk)).await;
        elapsed += chunk;
    }

    let seen_words = bench.word_seen();
    let max_frame_bytes = bench.max_frame_bytes();
    let col_active = bench.column_active();
    let captured_events = bench.seen().all().len();
    let fit_events = bench.seen().count("frame.fit");
    let frame_cut = bench.count("frame.cut");
    let frame_bound_most = bench.peak("frame.bound_bytes").map_or(0, |p| p.most);

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
        "i1_eight_hours total: missing={} max_frame={}B bound_most={}B events={} wall={:.2}s",
        run.missing,
        run.max_frame_bytes,
        run.frame_bound_most,
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
    assert_eq!(
        all_rec1,
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
#[ignore = "P1-19：hub 不阻塞在导出上"]
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
    assert!(
        bench.seen().of("job.start").iter().any(|l| l.text("kind") == Some("export.write")),
        "expected job.start(export.write) while blocked"
    );
    assert!(
        !bench.seen().of("job.done").iter().any(|l| l.text("kind") == Some("export.write")),
        "export.write should still be running while FIFO is unopened"
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
    while !bench
        .seen()
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("export.write") && l.text("code") == Some("ok"))
    {
        assert!(Instant::now() < deadline, "expected job.done(export.write, ok)");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    bench.stop_recording().await;
    bench.finish();
}

async fn run_i2_background(check_observability: bool) {
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

    if check_observability {
        let done = bench.seen().of("job.done");
        for kind in ["search", "job.move", "job.write"] {
            assert!(
                done.iter().any(|l| l.text("kind") == Some(kind) && l.text("code") == Some("ok")),
                "expected job.done({kind}, ok)"
            );
        }
        assert!(
            !bench.seen().of("judge.verdict").iter().any(|l| l.text("to") == Some("Stalled")),
            "no stage should be judged Stalled during healthy background work"
        );
    }

    bench.finish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i2_background_work_runs_beside_a_recording() {
    let _store = STORE.lock().await;
    run_i2_background(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "P1-21：后台任务的 job.done 事件"]
async fn i2_background_work_runs_beside_a_recording_and_it_shows() {
    let _store = STORE.lock().await;
    run_i2_background(true).await;
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
    // P1-16: `part.restarts` counter once lanes start on the supervision tree:
    // assert_eq!(bench.count("part.restarts"), 1);

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

// ---- 5. I7: stalls are seen and acted on ----

async fn run_i7_silent_service(check_observability: bool) {
    let speech = Speech::build(SpeechSpec::standard(35_000));
    let mut opts = BenchOptions::new(speech);
    opts.tape = TapeMode::Gated;
    opts.fault = Fault::Silent { connections: 1 };

    let bench = Bench::start(opts).await;
    bench.setup(&["en", "zh"], &["en", "zh"]).await;
    let rec_id = bench.start_recording().await;
    let started_at = bench.mic.started_at().expect("mic started");

    tokio::time::sleep(Duration::from_millis(30_000)).await;

    assert!(
        bench.service.dials() >= 2,
        "lane must abandon the silent connection and dial a fresh one (dials={})",
        bench.service.dials()
    );

    let first_seen = bench
        .word_seen()
        .into_iter()
        .flatten()
        .min()
        .expect("words must appear after switching off the silent connection");
    let elapsed_ms = first_seen.saturating_duration_since(started_at).as_millis() as u64;
    assert!(
        elapsed_ms <= I7_SILENT_RECOVER_MS,
        "first word after silent service arrived at {elapsed_ms} ms > {I7_SILENT_RECOVER_MS} ms"
    );

    bench.stop_recording().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let recorded = bench.shelf().read(&rec_id).expect("read shelf").expect("recording on shelf");
    let has_gap = recorded.lines.iter().any(|(_, s)| s.line.text == "…" && s.line.incomplete);
    assert!(has_gap, "draft must contain a gap line covering the silent connection");

    if check_observability {
        let verdicts = bench.seen().of("judge.verdict");
        let heard_states: Vec<&str> = verdicts
            .iter()
            .filter(|l| l.text("stage") == Some("Heard") && l.u64("lane") == Some(0))
            .filter_map(|l| l.text("to"))
            .collect();
        assert!(
            heard_states.contains(&"Slow")
                && heard_states.contains(&"Stalled")
                && heard_states.contains(&"Live"),
            "expected Heard(0) verdicts Slow -> Stalled -> Live, saw {heard_states:?}"
        );
        assert!(
            verdicts.iter().any(|l| {
                l.text("stage") == Some("Heard")
                    && l.text("to") == Some("Stalled")
                    && l.text("why") == Some("silent")
                    && l.text("action") == Some("reopen")
            }),
            "expected Stalled(why=silent, action=reopen)"
        );
        assert_eq!(bench.seen().count("stall.snapshot"), 1, "expected exactly one stall.snapshot");
        assert_eq!(bench.seen().count("lane.kick"), 1, "expected exactly one lane.kick");
        assert!(
            bench.seen().of("part.state").iter().any(|l| l.text("why") == Some("silent")),
            "expected part.state(why=silent)"
        );
    }

    bench.finish();
}

#[tokio::test(start_paused = true)]
async fn i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection() {
    let _store = STORE.lock().await;
    run_i7_silent_service(false).await;
}

#[tokio::test(start_paused = true)]
#[ignore = "P1-17：看守判断停滞并发出事件与快照"]
async fn i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection_and_it_shows()
{
    let _store = STORE.lock().await;
    run_i7_silent_service(true).await;
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
