mod hub;

use hub::{App, Asked, STORE, config, shelf, signed_in, soniox, tape, wav};
use std::fs;
use std::path::Path;
use zutalk_core::ai::write::Journal;
use zutalk_core::app::proto::{Command, Row, Setup, State};
use zutalk_core::capture::clock::{BLOCK, Block, wall_ms};
use zutalk_core::capture::tape::{self as tapes, Pass, Writer};
use zutalk_core::live::lines::Line;
use zutalk_core::record::retain::goes;
use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

const DAY: i64 = 86_400_000;
const NOW: i64 = 1_800_000_000_000;

#[test]
fn audio_stays_until_the_record_is_written_then_as_long_as_the_person_chose() {
    #[rustfmt::skip]
    let table = [
        // keep,   written, age,           goes
        (None,     true,    4_000 * DAY,   false), // until the person destroys it
        (None,     false,   4_000 * DAY,   false),
        (Some(0),  true,    0,             true),  // once its record is written
        (Some(0),  false,   4_000 * DAY,   false), // never before
        (Some(7),  true,    7 * DAY - 1,   false),
        (Some(7),  true,    7 * DAY,       true),
        (Some(7),  false,   8 * DAY,       false),
        (Some(30), true,    29 * DAY,      false),
        (Some(30), true,    31 * DAY,      true),
        (Some(0),  true,    -DAY,          true),  // a clock set back: once written is still now
        (Some(7),  true,    -DAY,          false), // and a start in the future is no age at all
    ];
    for (keep, written, age, expected) in table {
        assert_eq!(goes(keep, written, NOW - age, NOW), expected, "{keep:?} {written} {age}");
    }
    // Whatever the clock says, nothing overflows.
    assert!(goes(Some(u32::MAX), true, i64::MIN, i64::MAX));
    assert!(!goes(Some(u32::MAX), true, i64::MAX, i64::MIN));
}

#[test]
fn destroying_a_tape_takes_every_minute_and_leaves_what_the_service_is_still_owed() {
    let (dir, key) = (tempfile::tempdir().expect("a folder"), [7; 32]);
    let tape = dir.path().join("kx7q2m5v4tbn3hpw");
    let fill = |tape: &Path| {
        let mut writer = Writer::create(tape, &key).expect("a tape");
        for t_ms in (0..61_000).step_by(100) {
            writer.append(&Block { t_ms, pcm: vec![300; BLOCK] }).expect("a block");
        }
    };
    fill(&tape);
    assert!(tapes::kept(&tape));
    tapes::destroy(&tape).expect("it is destroyed");
    assert!(!tapes::kept(&tape) && !tape.exists(), "two minutes, and the folder, gone");
    tapes::destroy(&tape).expect("gone is gone");
    // A write that left something at the service keeps its journal: the start-up sweep reads it.
    fill(&tape);
    let owed = vec!["files/file-1".to_string()];
    let journal =
        Journal { name: Some("zutalk-kx7q2m5v4tbn3hpw.wav".into()), owed, take: 0, failed: None };
    journal.save(&tape).expect("the journal is kept");
    tapes::destroy(&tape).expect("it is destroyed");
    assert!(!tapes::kept(&tape));
    assert_eq!(Journal::load(&tape).expect("it reads"), journal);
    assert_eq!(tapes::read(&tape, &key, &Pass::Write, |_| {}).expect("nothing to read"), 0);
}

/// A recording as the hub would have kept it: its record, a line of `take`, and its audio.
fn kept(data: &Path, id: &str, started: i64, take: u32) {
    let shelf = shelf(data);
    let line = Line { language: "en".into(), text: format!("Said in {id}."), ..Line::default() };
    let said = Said { line, via: [("text".to_string(), "hand".to_string())].into() };
    shelf.put(id, LINE, &[(format!("{id}.{take}.00000"), Some(said))], started).expect("a line");
    let about =
        About { started, length_ms: 1_000, spoken: vec!["en".into()], take, ..About::default() };
    shelf.put(id, RECORDING, &[(id.to_string(), Some(about))], started).expect("its record");
    tape(data, id, 1);
}

fn audio(state: &State, id: &str) -> Option<bool> {
    state.library.iter().find(|row| row.id == id).map(|row| row.audio)
}

#[tokio::test(flavor = "multi_thread")]
async fn at_start_up_audio_the_person_let_go_is_destroyed_and_audio_not_yet_written_from_stays() {
    let _store = STORE.lock().await;
    signed_in();
    let dir = tempfile::tempdir().expect("a folder");
    let (data, now) = (dir.path(), wall_ms() as i64);
    kept(data, "oldwritten", now - 40 * DAY, 1);
    kept(data, "newwritten", now - 2 * DAY, 1);
    kept(data, "olddraft", now - 400 * DAY, 0);
    // Kept until destroyed, as the 0.8 app kept it: nothing goes.
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.len() == 3).await;
    assert!(state.library.iter().all(|row| row.audio), "{:?}", state.library);
    assert_eq!(state.setup.keep, None);
    app.send(Command::Setup { setup: Setup { keep: Some(30), ..Setup::default() } });
    app.until(|state| state.setup.keep == Some(30)).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !app
        .seen
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("settings") && l.text("code") == Some("ok"))
    {
        assert!(tokio::time::Instant::now() < deadline, "settings job did not finish");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    drop(app);
    // Thirty days: at the next start the old written one's audio goes, and its text stays.
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.len() == 3).await;
    assert_eq!(audio(&state, "oldwritten"), Some(false));
    assert_eq!(audio(&state, "newwritten"), Some(true));
    assert_eq!(audio(&state, "olddraft"), Some(true), "its record is not written yet");
    assert!(!data.join("tape/oldwritten").exists() && data.join("tape/olddraft").exists());
    app.send(Command::Open { id: "oldwritten".into() });
    let open = app.until(|state| state.open.is_some()).await.open.expect("it opens");
    assert_eq!(open.lines[0].1.line.text, "Said in oldwritten.");
    assert!(!open.row.audio && !open.row.draft);
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_destroyed_at_the_persons_word_is_gone_for_good_and_its_text_stays() {
    let _store = STORE.lock().await;
    signed_in();
    let dir = tempfile::tempdir().expect("a folder");
    let (data, now) = (dir.path(), wall_ms() as i64);
    kept(data, "written", now, 1);
    kept(data, "draft", now, 0);
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.len() == 2).await;
    assert_eq!((audio(&state, "written"), audio(&state, "draft")), (Some(true), Some(true)));
    app.send(Command::DestroyAudio { id: "written".into() });
    app.until(|state| audio(state, "written") == Some(false)).await;
    assert!(!data.join("tape/written").exists());
    // A draft whose write left something at the service: the journal stays for the sweep.
    let owed = vec!["files/file-1".to_string()];
    let journal = Journal { name: Some("zutalk-draft.wav".into()), owed, take: 0, failed: None };
    journal.save(&data.join("tape/draft")).expect("the journal is kept");
    app.send(Command::DestroyAudio { id: "draft".into() });
    let state = app.until(|state| audio(state, "draft") == Some(false)).await;
    assert!(state.library.iter().all(|row| !row.audio));
    app.send(Command::DestroyAudio { id: "nowhere".into() });
    app.notice("recording.gone").await;
    // Not while it is being recorded.
    app.send(Command::Start);
    let id = app.session(|s| s.is_some()).await.expect("one").id;
    app.send(Command::DestroyAudio { id });
    app.notice("recording.under_way").await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    drop(app);
    // Started again, the draft without its audio is not written again: its text stays.
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.iter().any(|row| row.id == "draft")).await;
    let draft = state.library.iter().find(|row| row.id == "draft").expect("still there");
    assert!(draft.draft && !draft.audio);
    assert!(state.writing.iter().all(|w| w.id != "draft"), "{:?}", state.writing);
    app.send(Command::Open { id: "draft".into() });
    let open = app.until(|state| state.open.is_some()).await.open.expect("it opens");
    assert_eq!(open.lines[0].1.line.text, "Said in draft.");
}

#[tokio::test(flavor = "multi_thread")]
async fn once_the_record_is_written_audio_kept_only_until_then_goes() {
    let _store = STORE.lock().await;
    signed_in();
    let (dir, asked) = (tempfile::tempdir().expect("a folder"), Asked::default());
    let stt = soniox(&asked).await;
    let data = dir.path();
    let mut app = App::with(zutalk_core::app::hub::Config {
        soniox: format!("{}/v1", stt.uri()),
        ..config(data)
    });
    app.until(|_| true).await;
    let setup = Setup { spoken: vec!["en".into()], keep: Some(0), ..Setup::default() };
    app.send(Command::Setup { setup });
    let file = data.join("Planning.wav");
    wav(&file, 1_500);
    app.send(Command::Import { path: file.to_string_lossy().into() });
    // Once its tape is sealed the audio stays until it is written; then it goes, and the
    // transcript is the text.
    let gone = |state: &State| {
        let kept = |row: &Row| row.audio || !row.draft || row.length_ms == 0;
        assert!(state.library.iter().all(kept), "{:?}", state.library);
        state.library.iter().any(|row| !row.draft && !row.audio)
    };
    let row = app.until(gone).await.library[0].clone();
    assert!(!data.join("tape").join(&row.id).exists());
    app.send(Command::Open { id: row.id.clone() });
    let open = app.until(|state| state.open.is_some()).await.open.expect("it opens");
    assert_eq!(open.lines[0].1.line.text, "Zuzalu is in Chiang Mai.");
    assert_eq!(asked.lock().expect("one at a time").len(), 1);
    assert!(fs::read_dir(data.join("tape")).expect("the folder").next().is_none());
}

/// Seeds an owed draft recording (already sealed with `length_ms > 0`, `take == 0`) on the local
/// shelf with 1 s of tape so startup recovery sends it straight to `job.write`.
fn owed_local_draft(data: &Path, id: &str, started: i64) {
    let shelf = hub::local_shelf(data);
    let about = About {
        started,
        length_ms: 1_000,
        title: id.into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        take: 0,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.to_string(), Some(about))], started).expect("seed draft");
    tape(data, id, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn transcripts_are_written_one_at_a_time() {
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let data = dir.path();
    let now = wall_ms() as i64;
    let ids = ["draft21one01", "draft21two02", "draft21thr03"];
    for (i, id) in ids.iter().enumerate() {
        owed_local_draft(data, id, now + i as i64);
    }

    let server = MockServer::start().await;
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let next_file = Arc::new(AtomicUsize::new(0));
    let started_at: Arc<Mutex<BTreeMap<String, Instant>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let spans: Arc<Mutex<Vec<(Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));

    {
        let active = active.clone();
        let peak = peak.clone();
        let next_file = next_file.clone();
        let started_at = started_at.clone();
        Mock::given(method("POST"))
            .and(path("/v1/files"))
            .respond_with(move |_req: &Request| {
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now_active, Ordering::SeqCst);
                let n = next_file.fetch_add(1, Ordering::SeqCst) + 1;
                let file_id = format!("file-{n}");
                started_at.lock().unwrap().insert(file_id.clone(), Instant::now());
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "id": file_id }))
                    .set_delay(Duration::from_millis(200))
            })
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/transcriptions"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "tr-1" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/v1/transcriptions/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "completed" })))
        .mount(&server)
        .await;
    let tokens = json!({
        "id": "tr-1",
        "tokens": [{ "text": "Hello.", "start_ms": 0, "end_ms": 500, "language": "en", "speaker": 1 }]
    });
    Mock::given(method("GET"))
        .and(path_regex("^/v1/transcriptions/[^/]+/transcript$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
        .mount(&server)
        .await;
    for kind in ["files", "transcriptions"] {
        Mock::given(method("GET"))
            .and(path(format!("/v1/{kind}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ kind: [], "next_page_cursor": null })),
            )
            .mount(&server)
            .await;
    }
    {
        let active = active.clone();
        let started_at = started_at.clone();
        let spans = spans.clone();
        Mock::given(method("DELETE"))
            .and(path_regex("^/v1/files/"))
            .respond_with(move |req: &Request| {
                let id = req.url.path().trim_start_matches("/v1/files/").to_string();
                if let Some(t0) = started_at.lock().unwrap().remove(&id) {
                    spans.lock().unwrap().push((t0, Instant::now()));
                }
                active.fetch_sub(1, Ordering::SeqCst);
                ResponseTemplate::new(204)
            })
            .mount(&server)
            .await;
    }
    Mock::given(method("DELETE"))
        .and(path_regex("^/v1/transcriptions/"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let mut app = App::with(zutalk_core::app::hub::Config {
        soniox: format!("{}/v1", server.uri()),
        ..config(data)
    });
    app.until(|s| ids.iter().all(|id| s.library.iter().any(|r| r.id == *id && !r.draft))).await;

    assert_eq!(peak.load(Ordering::SeqCst), 1, "writes must never overlap");
    let spans = spans.lock().unwrap().clone();
    assert_eq!(spans.len(), 3, "expected 3 upload spans, got {spans:?}");
    for pair in spans.windows(2) {
        assert!(pair[1].0 >= pair[0].1, "spans overlapped: {pair:?}");
    }

    let write_events: Vec<_> = app
        .seen
        .all()
        .into_iter()
        .filter(|l| matches!(l.code, "job.start" | "job.done") && l.text("kind") == Some("write"))
        .collect();
    assert_eq!(write_events.len(), 6, "expected 3 start/done pairs, got {write_events:?}");
    for pair in write_events.chunks_exact(2) {
        assert_eq!(pair[0].code, "job.start", "{write_events:?}");
        assert_eq!(pair[1].code, "job.done", "{write_events:?}");
        assert_eq!(pair[1].text("code"), Some("ok"), "{write_events:?}");
        assert_eq!(pair[0].u64("job"), pair[1].u64("job"), "{write_events:?}");
    }
    assert!(
        write_events[2].u64("waited_ms").unwrap_or(0) > 0,
        "2nd write should have waited for the 1st: {:?}",
        write_events[2]
    );
    assert!(
        write_events[4].u64("waited_ms").unwrap_or(0) > 0,
        "3rd write should have waited for the 2nd: {:?}",
        write_events[4]
    );
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_that_waits_to_try_again_does_not_hold_up_the_next() {
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};
    use zutalk_core::rt::supervise::AGAIN_MS;

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let data = dir.path();
    let now = wall_ms() as i64;
    let ids = ["draft21retry1", "draft21pass02"];
    for (i, id) in ids.iter().enumerate() {
        owed_local_draft(data, id, now + i as i64);
    }

    let server = MockServer::start().await;
    let uploads = Arc::new(AtomicUsize::new(0));
    let failed_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let uploads = uploads.clone();
        let failed_id = failed_id.clone();
        Mock::given(method("POST"))
            .and(path("/v1/files"))
            .respond_with(move |req: &Request| {
                let n = uploads.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    let body = String::from_utf8_lossy(&req.body);
                    let which = ids
                        .iter()
                        .find(|id| body.contains(&format!("zutalk-{id}.wav")))
                        .map(|s| (*s).to_string());
                    *failed_id.lock().unwrap() = which;
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(201).set_body_json(json!({ "id": "file-ok" }))
                }
            })
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/transcriptions"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "tr-ok" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/v1/transcriptions/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "completed" })))
        .mount(&server)
        .await;
    let tokens = json!({
        "id": "tr-ok",
        "tokens": [{ "text": "Second passes.", "start_ms": 0, "end_ms": 500, "language": "en", "speaker": 1 }]
    });
    Mock::given(method("GET"))
        .and(path_regex("^/v1/transcriptions/[^/]+/transcript$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens))
        .mount(&server)
        .await;
    for kind in ["files", "transcriptions"] {
        Mock::given(method("GET"))
            .and(path(format!("/v1/{kind}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ kind: [], "next_page_cursor": null })),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("DELETE"))
        .and(path_regex("^/v1/(files|transcriptions)/"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let started = Instant::now();
    let mut app = App::with(zutalk_core::app::hub::Config {
        soniox: format!("{}/v1", server.uri()),
        ..config(data)
    });

    // Wait until one draft is written (!draft) and the other shows its retry code in State.writing.
    let state = app
        .until(|s| {
            let one_written = s.library.iter().any(|r| !r.draft);
            let one_retrying = s.writing.iter().any(|w| w.step == "service-busy");
            one_written && one_retrying
        })
        .await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(AGAIN_MS / 2),
        "second draft should finish long before AGAIN_MS ({AGAIN_MS} ms), took {elapsed:?}"
    );
    let retried_id = failed_id.lock().unwrap().clone().expect("first upload recorded failed id");
    assert!(
        state.writing.iter().any(|w| w.id == retried_id && w.step == "service-busy"),
        "expected {retried_id} retrying with service-busy in {:?}",
        state.writing
    );

    let events: Vec<_> = app
        .seen
        .all()
        .into_iter()
        .filter(|l| {
            matches!(l.code, "job.start" | "job.done" | "job.again")
                && l.text("kind") == Some("write")
        })
        .collect();
    for line in &events {
        eprintln!("P1-21 retry log: {line:?}");
    }
    let done_busy = events
        .iter()
        .position(|l| l.code == "job.done" && l.text("code") == Some("service-busy"))
        .unwrap_or_else(|| panic!("expected job.done(write, service-busy) in {events:?}"));
    let again_idx = events
        .iter()
        .position(|l| {
            l.code == "job.again"
                && l.text("why") == Some("service-busy")
                && l.u64("n") == Some(1)
                && l.u64("wait_ms") == Some(AGAIN_MS)
        })
        .unwrap_or_else(|| {
            panic!("expected job.again(write, n=1, wait_ms={AGAIN_MS}) in {events:?}")
        });
    let second_start = events
        .iter()
        .enumerate()
        .find(|(i, l)| *i > again_idx && l.code == "job.start")
        .map(|(i, _)| i)
        .unwrap_or_else(|| panic!("expected second job.start after job.again in {events:?}"));
    assert!(done_busy < again_idx && again_idx < second_start, "{events:?}");
    assert!(
        events[second_start].u64("waited_ms").unwrap_or(AGAIN_MS) < AGAIN_MS / 2,
        "second write must not wait AGAIN_MS: {:?}",
        events[second_start]
    );
    assert_eq!(app.seen.refused(), 0);
}
