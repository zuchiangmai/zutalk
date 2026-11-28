mod common;
mod hub;

use futures_util::{SinkExt, StreamExt};
use hub::{App, ME, STORE, config_on, store, wav};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zu::{SecretKey, Value, auth};
use zutalk_core::app::hub::Config;
use zutalk_core::app::proto::{Command, HostState, RefineStatus, Setup};
use zutalk_core::record::shelf::TitleBy;

const DRISL_CT: &str = "application/vnd.ipld.drisl";
const NOW_RESET_MS: i64 = 4_102_444_800_000;

#[derive(Default)]
struct PolCalls {
    quota: usize,
    live: Vec<(Option<String>, i64, i64)>,
    settled: Vec<(String, i64)>,
    uploads: Vec<(usize, bool)>,
    jobs: usize,
    titles: usize,
    translates: usize,
}

fn verify_zu_auth(req: &Request, origin: &str) {
    let hdr = req
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .expect("Authorization: Zu header present");
    let pubkey = SecretKey::from_seed(&[5; 32]).public();
    let now_ms = zutalk_core::capture::clock::wall_ms() as i64;
    let verified =
        auth::verify(hdr, origin, now_ms, |did, key| did == ME && *key == pubkey, |_, _| true)
            .expect("valid zu::auth signature");
    assert_eq!(verified, ME);
}

fn drisl_reply(val: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).insert_header("content-type", DRISL_CT).set_body_bytes(val.encode())
}

fn quota_fields(left: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("left".into(), Value::Int(left)),
        ("sonioxUsed".into(), Value::Int(1)),
        ("sonioxLimit".into(), Value::Int(40)),
        ("aiUsed".into(), Value::Int(1)),
        ("aiLimit".into(), Value::Int(120)),
        ("resetsAt".into(), Value::Int(NOW_RESET_MS)),
        ("sttReady".into(), Value::Bool(true)),
        ("aiReady".into(), Value::Bool(true)),
    ])
}

async fn mock_pol_host(calls: Arc<Mutex<PolCalls>>) -> MockServer {
    let server = MockServer::start().await;
    let origin = server.uri();

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/quota"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                calls.lock().expect("lock").quota += 1;
                drisl_reply(Value::Map(quota_fields(720_000)))
            })
            .mount(&server)
            .await;
    }

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/live"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                let Value::Map(args) = Value::decode(&req.body).expect("DRISL live args") else {
                    panic!("expected map");
                };
                let session = match args.get("session") {
                    Some(Value::Str(s)) => Some(s.clone()),
                    _ => None,
                };
                let lanes = match args.get("lanes") {
                    Some(Value::Int(n)) => *n,
                    _ => 1,
                };
                let count = match args.get("count") {
                    Some(Value::Int(n)) => *n,
                    _ => 1,
                };
                calls.lock().expect("lock").live.push((session, lanes, count));
                let keys: Vec<Value> =
                    (0..count).map(|i| Value::Str(format!("temp:ws-{}", i + 1))).collect();
                let mut out = quota_fields(500_000);
                out.insert("session".into(), Value::Str("sess-pol-1".into()));
                out.insert("keys".into(), Value::List(keys));
                out.insert("sessionCapS".into(), Value::Int(10_800));
                drisl_reply(Value::Map(out))
            })
            .mount(&server)
            .await;
    }

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/settle"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                let Value::Map(args) = Value::decode(&req.body).expect("DRISL settle args") else {
                    panic!("expected map");
                };
                let Some(Value::Str(sess)) = args.get("session") else {
                    panic!("missing session");
                };
                let Some(Value::Int(used)) = args.get("usedSeconds") else {
                    panic!("missing usedSeconds");
                };
                calls.lock().expect("lock").settled.push((sess.clone(), *used));
                drisl_reply(Value::Map(BTreeMap::from([
                    ("left".into(), Value::Int(719_800)),
                    ("alreadyClosed".into(), Value::Bool(false)),
                ])))
            })
            .mount(&server)
            .await;
    }

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/titles"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                let Value::Map(args) = Value::decode(&req.body).expect("DRISL titles args") else {
                    panic!("expected map");
                };
                let Some(Value::Str(excerpt)) = args.get("excerpt") else {
                    panic!("missing excerpt");
                };
                assert!(!excerpt.trim().is_empty());
                calls.lock().expect("lock").titles += 1;
                let tag = Value::Map(BTreeMap::from([
                    ("label".into(), Value::Str("清迈".into())),
                    ("atMs".into(), Value::Int(200)),
                ]));
                drisl_reply(Value::Map(BTreeMap::from([
                    (
                        "titles".into(),
                        Value::List(vec![
                            Value::Str("清迈 Zuzalu 讨论".into()),
                            Value::Str("Zuzalu Chiang Mai".into()),
                        ]),
                    ),
                    ("tags".into(), Value::List(vec![tag])),
                    ("model".into(), Value::Str("deepseek-flash".into())),
                ])))
            })
            .mount(&server)
            .await;
    }

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/upload"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                let Value::Map(args) = Value::decode(&req.body).expect("DRISL upload args") else {
                    panic!("expected map");
                };
                let Some(Value::Bytes(chunk)) = args.get("chunk") else {
                    panic!("missing chunk bytes");
                };
                let done = matches!(args.get("done"), Some(Value::Bool(true)));
                let mut guard = calls.lock().expect("lock");
                if guard.uploads.is_empty() {
                    assert!(chunk.starts_with(b"RIFF") && &chunk[8..12] == b"WAVE");
                }
                guard.uploads.push((chunk.len(), done));
                drisl_reply(Value::Map(BTreeMap::from([(
                    "job".into(),
                    Value::Str("job-pol-1".into()),
                )])))
            })
            .mount(&server)
            .await;
    }

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/job"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                calls.lock().expect("lock").jobs += 1;
                let words = ["Zuzalu", " is", " in", " Chiang", " Mai", "."];
                let tokens: Vec<Value> = words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        Value::Map(BTreeMap::from([
                            ("text".into(), Value::Str((*w).into())),
                            ("startMs".into(), Value::Int((i * 200) as i64)),
                            ("endMs".into(), Value::Int((i * 200 + 180) as i64)),
                            ("speaker".into(), Value::Str("1".into())),
                            ("language".into(), Value::Str("en".into())),
                        ]))
                    })
                    .collect();
                drisl_reply(Value::Map(BTreeMap::from([
                    ("done".into(), Value::Bool(true)),
                    ("model".into(), Value::Str("stt-async-v6".into())),
                    ("tokens".into(), Value::List(tokens)),
                ])))
            })
            .mount(&server)
            .await;
    }

    {
        let calls = calls.clone();
        let origin = origin.clone();
        Mock::given(method("POST"))
            .and(path("/.well-known/zu-svc/zutalk/translate"))
            .respond_with(move |req: &Request| {
                verify_zu_auth(req, &origin);
                let Value::Map(args) = Value::decode(&req.body).expect("DRISL translate args")
                else {
                    panic!("expected map");
                };
                let Some(Value::List(lines)) = args.get("lines") else {
                    panic!("missing lines");
                };
                calls.lock().expect("lock").translates += 1;
                let answers: Vec<Value> = lines
                    .iter()
                    .map(|item| {
                        let Value::Map(m) = item else { panic!("expected line map") };
                        let Some(Value::Int(id)) = m.get("id") else { panic!("missing id") };
                        Value::Map(BTreeMap::from([
                            ("id".into(), Value::Int(*id)),
                            ("unclear".into(), Value::Bool(false)),
                            ("facts".into(), Value::List(Vec::new())),
                            (
                                "to".into(),
                                Value::Map(BTreeMap::from([(
                                    "zh".into(),
                                    Value::Str("Zuzalu 在清迈。".into()),
                                )])),
                            ),
                        ]))
                    })
                    .collect();
                drisl_reply(Value::Map(BTreeMap::from([
                    ("model".into(), Value::Str("deepseek-flash".into())),
                    ("answers".into(), Value::List(answers)),
                ])))
            })
            .mount(&server)
            .await;
    }

    server
}

#[tokio::test(flavor = "multi_thread")]
async fn pol_host_live_recording_settles_draft_and_titles_then_manual_refine_writes_take_one() {
    let _store = STORE.lock().await;
    store(true);
    let calls = Arc::new(Mutex::new(PolCalls::default()));
    let pol = mock_pol_host(calls.clone()).await;
    let (rooms_origin, _rooms_dir) = common::serve().await;

    let soniox = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("ws port");
    let realtime = format!("ws://{}", soniox.local_addr().expect("ws addr"));
    let ws_task = tokio::spawn(async move {
        for _ in 0..2 {
            let Ok((stream, _)) = soniox.accept().await else { break };
            tokio::spawn(async move {
                let mut ws = accept_async(stream).await.expect("ws handshake");
                let Some(Ok(Message::Text(cfg))) = ws.next().await else {
                    panic!("missing ws setup");
                };
                assert!(cfg.contains("temp:ws-"), "expected temp ws key, got {cfg}");
                let is_translating = cfg.contains("\"translation\"");
                let tokens = if is_translating {
                    json!([
                        {"text": "Zuzalu lives in Chiang Mai.", "start_ms": 100, "end_ms": 800,
                         "is_final": true, "language": "en", "speaker": "1"},
                        {"text": "Zuzalu 在清迈生活。", "is_final": true,
                         "language": "zh", "speaker": "1", "translation_status": "translation"},
                        {"text": "<end>", "is_final": true}
                    ])
                } else {
                    json!([
                        {"text": "Zuzalu lives in Chiang Mai.", "start_ms": 100, "end_ms": 800,
                         "is_final": true, "language": "en", "speaker": "1"},
                        {"text": "<end>", "is_final": true}
                    ])
                };
                let reply = json!({ "tokens": tokens, "total_audio_proc_ms": 800 });
                let _ = ws.send(Message::text(reply.to_string())).await;
                while let Some(Ok(msg)) = ws.next().await {
                    if matches!(&msg, Message::Text(t) if t.is_empty()) {
                        let _ = ws
                            .send(Message::text(
                                json!({"tokens": [], "finished": true}).to_string(),
                            ))
                            .await;
                        break;
                    }
                }
            });
        }
    });

    let dir = tempfile::tempdir().expect("data dir");
    let cfg = Config {
        realtime: realtime.clone(),
        rooms: rooms_origin.clone(),
        ..config_on(dir.path(), &pol.uri())
    };
    let mut app = App::with(cfg);
    app.until(|s| s.left == Some(720_000)).await;

    let setup = Setup {
        spoken: vec!["en".into()],
        shown: vec!["en".into(), "zh".into()],
        ..Setup::default()
    };
    app.send(Command::Setup { setup });

    app.send(Command::HostArm { event: None, captions: true });
    let armed = app.until(|s| matches!(s.host, HostState::Armed { link: Some(_), .. })).await;
    if let HostState::Armed { link: Some(link), .. } = armed.host {
        assert!(link.starts_with(&format!("{rooms_origin}/r/")));
    }
    app.send(Command::HostCancel);
    app.until(|s| s.host == HostState::default()).await;

    app.send(Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("session").id;
    app.frame(|f| f.columns.iter().any(|c| !c.lines.is_empty())).await;
    let draft_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if !app.transcript(&rec_id, Some(0), 0).await.lines.is_empty() {
            break;
        }
        assert!(tokio::time::Instant::now() < draft_deadline, "draft line did not land");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    let settled_view = app
        .wait_recording(&rec_id, |v| {
            v.is_some_and(|r| r.row.title_by == TitleBy::Ai && !r.row.tags.is_empty())
        })
        .await
        .expect("settled recording view");

    assert!(settled_view.row.draft);
    assert_eq!(settled_view.take, 0);
    assert_eq!(settled_view.refine, RefineStatus::Draft { why: None });
    assert_eq!(settled_view.row.title, "清迈 Zuzalu 讨论");
    assert!(settled_view.row.tags.iter().any(|t| t.label == "清迈"));

    {
        let snap = calls.lock().expect("lock");
        assert_eq!(snap.live.len(), 1, "lanes batched into one zutalk/live call");
        assert_eq!(snap.live[0].0, None);
        assert_eq!(snap.live[0].1, snap.live[0].2);
        assert_eq!(snap.settled.len(), 1, "live session settled on stop");
        assert_eq!(snap.settled[0].0, "sess-pol-1");
        assert!(snap.settled[0].1 >= 1);
        assert!(snap.uploads.is_empty(), "live draft on pol host must NOT auto-upload");
        assert!(snap.titles >= 1);
    }

    ws_task.abort();
    drop(app);

    let app2 = App::with(Config { rooms: rooms_origin, ..config_on(dir.path(), &pol.uri()) });
    app2.wait_library(|rows| rows.iter().any(|r| r.id == rec_id)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        calls.lock().expect("lock").uploads.is_empty(),
        "boot survey must keep pol host live draft at take 0 without auto-uploading"
    );

    app2.send(Command::Refine { id: rec_id.clone() });
    let refined_view = app2
        .wait_recording(&rec_id, |v| {
            v.is_some_and(|r| r.refine == RefineStatus::Refined { take: 1 } && !r.row.draft)
        })
        .await
        .expect("manual refine completed");
    assert_eq!(refined_view.take, 1);

    let chunk = app2.transcript(&rec_id, Some(1), 0).await;
    assert_eq!(chunk.lines.len(), 1);
    assert_eq!(chunk.lines[0].1.line.text, "Zuzalu is in Chiang Mai.");
    assert_eq!(
        chunk.lines[0].1.line.translations.get("zh").map(String::as_str),
        Some("Zuzalu 在清迈。")
    );

    let snap = calls.lock().expect("lock");
    assert!(!snap.uploads.is_empty());
    assert!(snap.jobs >= 1);
    assert!(snap.translates >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn pol_host_imported_audio_file_transcribes_and_translates_automatically() {
    let _store = STORE.lock().await;
    store(true);
    let calls = Arc::new(Mutex::new(PolCalls::default()));
    let pol = mock_pol_host(calls.clone()).await;

    let dir = tempfile::tempdir().expect("data dir");
    let mut app = App::with(config_on(dir.path(), &pol.uri()));
    app.until(|s| s.left == Some(720_000)).await;

    let setup = Setup {
        spoken: vec!["en".into()],
        shown: vec!["en".into(), "zh".into()],
        ..Setup::default()
    };
    app.send(Command::Setup { setup });

    let audio_file = dir.path().join("imported.wav");
    wav(&audio_file, 1_200);
    app.send(Command::Import { path: audio_file.to_string_lossy().into() });

    let rows = app.wait_library(|rows| rows.iter().any(|r| !r.draft && !r.tags.is_empty())).await;
    let rec_id = rows[0].id.clone();
    let imported_view = app.recording(&rec_id).await.expect("imported recording view");
    assert_eq!(imported_view.row.title_by, TitleBy::File);
    assert!(imported_view.title_candidates.iter().any(|c| c.by == TitleBy::Ai));

    let chunk = app.transcript(&rec_id, Some(1), 0).await;
    assert_eq!(chunk.lines.len(), 1);
    assert_eq!(chunk.lines[0].1.line.text, "Zuzalu is in Chiang Mai.");
    assert_eq!(
        chunk.lines[0].1.line.translations.get("zh").map(String::as_str),
        Some("Zuzalu 在清迈。")
    );

    let snap = calls.lock().expect("lock");
    assert!(!snap.uploads.is_empty());
    assert!(snap.jobs >= 1);
    assert!(snap.translates >= 1);
    assert!(snap.titles >= 1);
}
