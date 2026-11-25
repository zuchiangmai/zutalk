//! P6-05 acceptance & privacy gate:
//! 1. Book-club end-to-end flow: legacy `talk/term` migration -> topic CRUD & `.md` handout
//!    extraction -> selected vs unselected topic context isolation across live WS, async Soniox,
//!    and OpenRouter -> auto AI titles & tags -> 2-char & >=3-char `SearchHitKind::Tag` +
//!    `LibraryFilter.tag` -> hand title protection across subsequent `GenerateTitle`.
//! 2. Quota-only async 403 refusal (`keys.async_refused`, `DECISIONS.md` #4) preserves the live
//!    draft and still automatically titles the draft via OpenRouter.
//! 3. Real-disk `zutalk_log::files` byte-level privacy & observability audit across
//!    `<data>/logs/*.jsonl`, `<data>/diagnose/**/*`, and `<data>/settings.json` verifying zero
//!    leaks, required structured events, `stats.full == 0`, `refused == 0`, and `Tally <= 8192 B`.

mod hub;

use futures_util::{SinkExt, StreamExt};
use hub::{App, Asked, STORE, config, config_on, shelf, signed_in, soniox, store, tape};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zutalk_core::app::hub::{Config, Core, Outlet};
use zutalk_core::app::proto::{
    Change, Command, LibraryFilter, Query, QueryValue, RecordingView, RefineStatus, SearchHitKind,
    Session, Setup as AppSetup, State, TitleBy,
};
use zutalk_core::capture::tape as audio_tape;
use zutalk_core::id::keys;
use zutalk_core::knowledge::glossary::{self, Term};
use zutalk_core::knowledge::topic::{ALWAYS_TOPIC_ID, Topic, TopicFact, TranslationPair};
use zutalk_core::live::lines::Line;
use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

const NOW: i64 = 1_800_000_000_000;

fn owned(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Wait for tape persistence + 200 ms so `seal()` never discards a recording as zero-length under
/// parallel CPU load (`LESSONS.md`).
async fn wait_for_tape(data_dir: &Path, rec_id: &str) {
    let tape_dir = data_dir.join("tape").join(rec_id);
    for _ in 0..100 {
        if audio_tape::kept(&tape_dir) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// Core wrapper that inherits the caller's active `zutalk_log::files` scope instead of installing
/// an in-memory `capture()`, entering `scope.enter()` only synchronously (`LESSONS.md` P4).
struct FileApp {
    core: Core,
    outlet: Outlet,
    said_at: u64,
}

impl FileApp {
    fn start(scope: &zutalk_log::Scope, cfg: Config) -> Self {
        let (core, outlet) = {
            let _entered = scope.enter();
            Core::start(cfg).expect("core starts")
        };
        Self { core, outlet, said_at: 0 }
    }

    fn send(&self, scope: &zutalk_log::Scope, command: Command) {
        let _entered = scope.enter();
        self.core.send(command).expect("hub accepts command");
    }

    async fn until(&mut self, holds: impl Fn(&State) -> bool) -> State {
        let wait = async {
            let (seq, state) = self.outlet.state.now();
            if seq > 0 && holds(&state) {
                return (*state).clone();
            }
            while let Some((_, state)) = self.outlet.state.next().await {
                if holds(&state) {
                    return (*state).clone();
                }
            }
            panic!("the core stopped");
        };
        timeout(Duration::from_secs(20), wait).await.expect("state came in time")
    }

    async fn session(&mut self, holds: impl Fn(Option<&Session>) -> bool) -> Option<Session> {
        let wait = async {
            loop {
                let (_, current) = self.outlet.session.now();
                if holds((*current).as_ref()) {
                    return (*current).clone();
                }
                self.outlet.session.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("session came in time")
    }

    async fn notice(&mut self, code: &str) {
        let wait = async {
            loop {
                let (_, notices) = self.outlet.said.now();
                let first = notices.next.saturating_sub(notices.codes.len() as u64);
                if self.said_at < first {
                    self.said_at = first;
                }
                while self.said_at < notices.next {
                    let idx = (self.said_at - first) as usize;
                    self.said_at += 1;
                    if notices.codes[idx] == code {
                        return;
                    }
                }
                self.outlet.said.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("notice came in time")
    }

    async fn wait_recording(
        &self,
        id: &str,
        holds: impl Fn(Option<&RecordingView>) -> bool,
    ) -> Option<RecordingView> {
        let mut inv = self.outlet.invalidated.clone();
        let wait = async {
            loop {
                if let Ok(reply) = self.core.query(Query::Recording { id: id.into() }).await
                    && let QueryValue::Recording(view) = reply.value
                    && holds(view.as_deref())
                {
                    return view.map(|v| *v);
                }
                let _ = timeout(Duration::from_millis(100), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("recording matched in time")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn book_club_end_to_end_topics_context_titles_tags_and_search() {
    let _store = STORE.lock().await;
    signed_in();
    keys::set("openrouter", Some(b"or-own-bookclub-key")).expect("openrouter key");
    let dir = tempfile::tempdir().expect("tempdir");

    // 1. Seed legacy `talk/term` records before startup to verify lossless migration into `always`.
    let legacy_shelf = shelf(dir.path());
    glossary::put(
        &legacy_shelf,
        "ChiangMaiAlways",
        Some(owned(&[("zh", "清迈常驻"), ("th", "เชียงใหม่")])),
        NOW,
    )
    .expect("seed legacy term 1");
    glossary::put(
        &legacy_shelf,
        "SharedKitchenAlways",
        Some(owned(&[("zh", "共享厨房")])),
        NOW + 1,
    )
    .expect("seed legacy term 2");

    // Stand-in Soniox Async HTTP + OpenRouter HTTP (dispatching translations vs titles).
    let asked_soniox = Asked::default();
    let stt = soniox(&asked_soniox).await;

    let asked_translate: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let asked_titles: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let title_calls = Arc::new(AtomicUsize::new(0));
    let translate_tap = asked_translate.clone();
    let titles_tap = asked_titles.clone();
    let title_calls_tap = title_calls.clone();

    let openrouter = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let schema_name = body
                .pointer("/response_format/json_schema/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if schema_name == "recording_titles_and_tags" {
                titles_tap.lock().unwrap().push(body);
                let call = title_calls_tap.fetch_add(1, Ordering::SeqCst);
                let payload = if call == 0 {
                    json!({
                        "titles": [
                            "读书会：看不见的城第三夜",
                            "瓦尔德拉达与记忆里的城"
                        ],
                        "tags": [
                            { "label": "押金", "at_ms": 200 },
                            { "label": "读书会讨论", "at_ms": 600 },
                            { "label": "瓦尔德拉达", "at_ms": 900 }
                        ]
                    })
                } else {
                    json!({
                        "titles": [
                            "重生成的读书会候选标题",
                            "湖中倒影之城讨论"
                        ],
                        "tags": [
                            { "label": "押金", "at_ms": 200 },
                            { "label": "读书会讨论", "at_ms": 600 },
                            { "label": "湖中倒影", "at_ms": 900 }
                        ]
                    })
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{
                        "finish_reason": "stop",
                        "message": { "content": payload.to_string() }
                    }]
                }))
            } else {
                translate_tap.lock().unwrap().push(body);
                let reply = json!({
                    "lines": [{
                        "id": 0,
                        "unclear": false,
                        "facts": [],
                        "to": { "zh": "今晚读书会聊《看不见的城》与瓦尔德拉达。" }
                    }]
                });
                ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{
                        "finish_reason": "stop",
                        "message": { "content": reply.to_string() }
                    }]
                }))
            }
        })
        .mount(&openrouter)
        .await;

    let realtime = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind ws");
    let cfg = Config {
        soniox: format!("{}/v1", stt.uri()),
        openrouter: format!("{}/api/v1", openrouter.uri()),
        realtime: format!("ws://{}", realtime.local_addr().expect("ws addr")),
        ..config(dir.path())
    };
    let mut app = App::with(cfg);
    app.until(|s| s.me.is_some()).await;

    // Verify legacy `talk/term` migrated automatically into pinned `always` topic.
    let always = match app.query(Query::Topic { id: ALWAYS_TOPIC_ID.into() }).await.unwrap().value {
        QueryValue::Topic(Some(t)) => t,
        other => panic!("expected migrated always topic, got {other:?}"),
    };
    assert!(always.pinned);
    assert!(always.terms.iter().any(|t| t.said == "ChiangMaiAlways"));
    assert!(always.terms.iter().any(|t| t.said == "SharedKitchenAlways"));

    // 2. Create selected book-club topic and unselected private-finance topic.
    app.send(Command::SaveTopic {
        topic: Topic {
            name: "读书会 · 看不见的城".into(),
            domain: "文学读书会".into(),
            facts: vec![TopicFact { key: "作品".into(), value: "看不见的城".into() }],
            background: "每周共读一章，中英混说讨论想象中的城市。".into(),
            terms: vec![Term { said: "KublaiKhan".into(), written: owned(&[("zh", "忽必烈")]) }],
            translation_terms: vec![TranslationPair {
                source: "InvisibleCities".into(),
                target: "看不见的城".into(),
            }],
            ..Topic::default()
        },
    });
    app.send(Command::SaveTopic {
        topic: Topic {
            name: "私人财务".into(),
            domain: "PRIVATE_FINANCE_DOMAIN_99".into(),
            facts: vec![TopicFact {
                key: "Account".into(),
                value: "PRIVATE_FINANCE_FACT_99".into(),
            }],
            background: "PRIVATE_FINANCE_BG_99 must never leave this device.".into(),
            terms: vec![Term {
                said: "PRIVATE_FINANCE_TERM_99".into(),
                written: owned(&[("zh", "私密账目九九")]),
            }],
            translation_terms: vec![TranslationPair {
                source: "PRIVATE_FINANCE_PAIR_99".into(),
                target: "私密译法九九".into(),
            }],
            ..Topic::default()
        },
    });

    let summaries = loop {
        if let Ok(reply) = app.query(Query::Topics).await
            && let QueryValue::Topics(list) = reply.value
            && list.iter().any(|t| t.name == "读书会 · 看不见的城")
            && list.iter().any(|t| t.name == "私人财务")
        {
            break list;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let book_id = summaries.iter().find(|t| t.name == "读书会 · 看不见的城").unwrap().id.clone();

    // Import `.md` handout -> candidates go ONLY to suggested_terms -> adopt via RememberTerm.
    let handout = dir.path().join("night-three-handout.md");
    fs::write(
        &handout,
        "# 第三夜讲义\n\n- **Valdrada**: 湖中倒影之城\n- **Isidora**: 记忆中的城\n",
    )
    .expect("write handout");
    app.send(Command::ImportTopicFile {
        id: book_id.clone(),
        path: handout.to_string_lossy().into_owned(),
    });
    let imported = loop {
        if let Ok(reply) = app.query(Query::Topic { id: book_id.clone() }).await
            && let QueryValue::Topic(Some(t)) = reply.value
            && t.source_file.as_deref() == Some("night-three-handout.md")
        {
            break t;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(imported.suggested_terms.iter().any(|s| s == "Valdrada"));
    assert!(
        !imported.terms.iter().any(|t| t.said == "Valdrada"),
        "ImportTopicFile must not auto-add suggestions to terms"
    );

    app.send(Command::RememberTerm {
        topic_id: book_id.clone(),
        said: "Valdrada".into(),
        written: owned(&[("zh", "瓦尔德拉达")]),
    });
    app.notice("topic.remembered").await;

    // 3. Select ONLY the book-club topic in Setup, start recording, and verify live WS config.
    app.send(Command::Setup {
        setup: AppSetup {
            spoken: vec!["en".into(), "zh".into()],
            shown: vec!["en".into(), "zh".into()],
            topics: vec![book_id.clone()],
            ..AppSetup::default()
        },
    });
    app.until(|s| s.setup.topics == vec![book_id.clone()]).await;
    app.send(Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("live session").id;

    let (stream, _) = realtime.accept().await.expect("ws accept");
    let mut ws = accept_async(stream).await.expect("ws handshake");
    let Some(Ok(Message::Text(first_raw))) = ws.next().await else {
        panic!("missing live ws config frame");
    };
    let forbidden_private = [
        "PRIVATE_FINANCE_DOMAIN_99",
        "PRIVATE_FINANCE_FACT_99",
        "PRIVATE_FINANCE_BG_99",
        "PRIVATE_FINANCE_TERM_99",
        "PRIVATE_FINANCE_PAIR_99",
        "私密账目九九",
    ];
    for expected in [
        "ChiangMaiAlways",
        "SharedKitchenAlways",
        "文学读书会",
        "每周共读一章",
        "KublaiKhan",
        "Valdrada",
        "InvisibleCities",
    ] {
        assert!(first_raw.contains(expected), "live WS missing `{expected}`: {first_raw}");
    }
    for secret in forbidden_private {
        assert!(!first_raw.contains(secret), "live WS leaked unselected `{secret}`");
    }

    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                let finished = json!({ "tokens": [], "finished": true }).to_string();
                let _ = ws.send(Message::text(finished)).await;
            }
        }
    });

    // 4. Wait for tape persistence, stop recording, and wait for refine + automatic AI titles/tags.
    wait_for_tape(dir.path(), &rec_id).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    let titled = app
        .wait_recording(&rec_id, |v| {
            v.is_some_and(|r| {
                !r.row.draft
                    && r.take == 1
                    && r.row.title_by == TitleBy::Ai
                    && !r.row.tags.is_empty()
                    && !r.title_candidates.is_empty()
            })
        })
        .await
        .expect("refined & AI titled recording");

    assert_eq!(titled.row.title, "读书会：看不见的城第三夜");
    assert_eq!(titled.row.title_by, TitleBy::Ai);
    assert_eq!(titled.topics, vec![book_id.clone()]);
    assert!((1..=4).contains(&titled.row.tags.len()));
    assert!(titled.title_candidates.iter().any(|c| c.text == "瓦尔德拉达与记忆里的城"));

    // Verify Soniox async + OpenRouter translation both carried always + selected, ZERO private.
    let soniox_dump = asked_soniox.lock().unwrap()[0].to_string();
    let translate_dump = asked_translate.lock().unwrap()[0].to_string();
    for expected in ["ChiangMaiAlways", "文学读书会", "每周共读一章", "Valdrada", "InvisibleCities"]
    {
        assert!(soniox_dump.contains(expected), "Soniox async missing `{expected}`");
    }
    for expected in ["文学读书会", "每周共读一章", "Valdrada", "InvisibleCities"] {
        assert!(translate_dump.contains(expected), "OpenRouter translate missing `{expected}`");
    }
    for secret in forbidden_private {
        assert!(!soniox_dump.contains(secret), "Soniox async leaked `{secret}`");
        assert!(!translate_dump.contains(secret), "OpenRouter translate leaked `{secret}`");
    }

    // Verify OpenRouter titles request enforced zero data retention.
    let title_req = asked_titles.lock().unwrap()[0].clone();
    assert_eq!(title_req.pointer("/provider/zdr"), Some(&json!(true)));
    assert_eq!(title_req.pointer("/provider/data_collection"), Some(&json!("deny")));

    // 5. Verify Query::Search hits both 2-char ("押金") and >=3-char ("读书会讨论") Tag hits,
    //    and Query::LibraryRecent filters accurately by LibraryFilter.tag.
    let hit_short = app.search("押金").await;
    assert!(
        hit_short.hits.iter().any(|h| h.recording_id == rec_id
            && h.kind == SearchHitKind::Tag
            && h.snippet == "押金"),
        "expected 2-char Tag search hit, got {:?}",
        hit_short.hits
    );
    let hit_long = app.search("读书会讨论").await;
    assert!(
        hit_long.hits.iter().any(|h| {
            h.recording_id == rec_id && h.kind == SearchHitKind::Tag && h.snippet == "读书会讨论"
        }),
        "expected >=3-char Tag search hit, got {:?}",
        hit_long.hits
    );

    let filtered_match = match app
        .query(Query::LibraryRecent {
            filter: LibraryFilter { tag: Some("押金".into()), ..LibraryFilter::default() },
            limit: 20,
            offset: 0,
        })
        .await
        .unwrap()
        .value
    {
        QueryValue::LibraryPage(p) => p,
        other => panic!("unexpected library reply: {other:?}"),
    };
    assert_eq!(filtered_match.rows.len(), 1);
    assert_eq!(filtered_match.rows[0].id, rec_id);

    let filtered_miss = match app
        .query(Query::LibraryRecent {
            filter: LibraryFilter {
                tag: Some("完全无关的话题".into()),
                ..LibraryFilter::default()
            },
            limit: 20,
            offset: 0,
        })
        .await
        .unwrap()
        .value
    {
        QueryValue::LibraryPage(p) => p,
        other => panic!("unexpected library reply: {other:?}"),
    };
    assert!(filtered_miss.rows.is_empty());

    // 6. Hand-rename title -> TitleBy::Hand -> GenerateTitle refreshes candidates & tags but
    //    NEVER overwrites the user's hand-written title!
    app.send(Command::Change {
        id: rec_id.clone(),
        change: Change::Title { text: "我手写的读书会标题".into() },
    });
    app.wait_recording(&rec_id, |v| {
        v.is_some_and(|r| r.row.title == "我手写的读书会标题" && r.row.title_by == TitleBy::Hand)
    })
    .await;

    app.send(Command::GenerateTitle { id: rec_id.clone() });
    let after_regen = app
        .wait_recording(&rec_id, |v| {
            v.is_some_and(|r| r.row.tags.iter().any(|t| t.label == "湖中倒影"))
        })
        .await
        .expect("regenerated candidates & tags");
    assert_eq!(after_regen.row.title, "我手写的读书会标题");
    assert_eq!(after_regen.row.title_by, TitleBy::Hand);
    assert!(after_regen.title_candidates.iter().any(|c| c.text == "重生成的读书会候选标题"));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn quota_async_refusal_still_titles_draft_with_openrouter() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("tempdir");

    // Seed a draft recording with audio tape and empty title.
    let s = shelf(dir.path());
    let rec_id = "19a00000009drafttitle";
    let about = About {
        started: NOW,
        length_ms: 4_000,
        title: String::new(),
        spoken: vec!["zh".into()],
        shown: vec!["zh".into()],
        take: 0,
        ..About::default()
    };
    s.put(rec_id, RECORDING, &[(rec_id.into(), Some(about))], NOW).expect("seed about");
    let draft_line = Said {
        line: Line {
            t0: 500,
            t1: 3_000,
            speaker: "1".into(),
            language: "zh".into(),
            text: "今天读书会先从第三夜开始聊，押金问题稍后确认。".into(),
            ..Line::default()
        },
        via: BTreeMap::from([("text".into(), "stt-rt-v6".into())]),
    };
    s.put(rec_id, LINE, &[(format!("{rec_id}.0.00000"), Some(draft_line))], NOW + 1)
        .expect("seed draft line");
    tape(dir.path(), rec_id, 2);

    // Community server grants temporary keys for both Soniox and OpenRouter (From::Granted).
    let our_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/lease"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "key": "sk-granted-temp-key", "left": 5_000_000 })),
        )
        .mount(&our_server)
        .await;

    // Soniox async refuses granted temporary key with 403 -> keys.async_refused.
    let refusing_soniox = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/files"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&refusing_soniox)
        .await;

    // OpenRouter accepts granted key and titles the live draft automatically!
    let openrouter = MockServer::start().await;
    let payload = json!({
        "titles": ["第三夜读书会草稿标题", "押金与读书会安排"],
        "tags": [{ "label": "第三夜", "at_ms": 500 }, { "label": "押金", "at_ms": 1_500 }]
    });
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": { "content": payload.to_string() }
            }]
        })))
        .mount(&openrouter)
        .await;

    let mut app = App::with(Config {
        soniox: format!("{}/v1", refusing_soniox.uri()),
        openrouter: format!("{}/api/v1", openrouter.uri()),
        ..config_on(dir.path(), &our_server.uri())
    });

    app.notice("keys.async_refused").await;
    let view = app
        .wait_recording(rec_id, |v| {
            v.is_some_and(|r| {
                r.row.draft
                    && matches!(r.refine, RefineStatus::Failed { ref code, .. } if code == "keys.async_refused")
                    && r.row.title_by == TitleBy::Ai
                    && !r.row.tags.is_empty()
            })
        })
        .await
        .expect("draft titled despite Soniox async 403 refusal");

    assert_eq!(view.row.title, "第三夜读书会草稿标题");
    assert_eq!(view.row.title_by, TitleBy::Ai);
    assert!(view.row.tags.iter().any(|t| t.label == "押金"));

    // Draft line remains completely intact.
    let chunk = app.transcript(rec_id, Some(0), 0).await;
    assert_eq!(chunk.lines.len(), 1);
    assert_eq!(chunk.lines[0].1.line.text, "今天读书会先从第三夜开始聊，押金问题稍后确认。");

    assert!(app.seen.of("refine.state").iter().any(|l| {
        l.text("to") == Some("Failed") && l.text("why") == Some("keys.async_refused")
    }));
    assert!(app.seen.of("titles.done").iter().any(|l| {
        l.text("rec") == Some(rec_id)
            && l.text("code") == Some("ok")
            && l.bool("applied") == Some(true)
    }));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn knowledge_and_ai_titles_never_leak_to_logs_diagnose_or_settings() {
    let _store = STORE.lock().await;
    store(true);
    let soniox_key = "sk-soniox-p6-privacy-probe-77";
    let openrouter_key = "sk-openrouter-p6-privacy-probe-88";
    keys::set("soniox", Some(soniox_key.as_bytes())).expect("soniox key");
    keys::set("openrouter", Some(openrouter_key.as_bytes())).expect("openrouter key");

    // Distinct, high-contrast fictional sentinels (zero real person names/handles).
    let topic_name_sentinel = "SENTINEL_TOPIC_NAME_QX7";
    let topic_domain_sentinel = "SENTINEL_TOPIC_DOMAIN_QX7";
    let topic_fact_k_sentinel = "SENTINEL_FACT_KEY_QX7";
    let topic_fact_v_sentinel = "SENTINEL_FACT_VAL_QX7";
    let topic_bg_sentinel = "SENTINEL_BACKGROUND_NARRATIVE_QX7";
    let topic_term_said_sentinel = "SENTINEL_TERM_SAID_QX7";
    let topic_term_zh_sentinel = "哨兵译写词QX7";
    let topic_pair_src_sentinel = "SENTINEL_PAIR_SRC_QX7";
    let topic_pair_dst_sentinel = "哨兵译法目标QX7";
    let import_dir_sentinel = "SENTINEL_IMPORT_DIR_QX7";
    let import_file_sentinel = "SENTINEL_HANDOUT_FILE_QX7.md";
    let import_suggested_sentinel = "SENTINEL_SUGGESTED_WORD_QX7";
    let ai_title_sentinel = "SENTINEL_AI_TITLE_QX7";
    let ai_cand_sentinel = "SENTINEL_AI_CANDIDATE_QX7";
    let ai_tag_sentinel = "SENTINEL_AI_TAG_QX7";
    let transcript_sentinel = "SENTINEL_TRANSCRIPT_UTTERANCE_QX7";

    let data_dir = tempfile::tempdir().expect("data dir");
    let log_dir = data_dir.path().join("logs");
    let refused_before = zutalk_log::refused();
    let (scope, guard) = zutalk_log::files(
        &log_dir,
        "zutalk",
        zutalk_log::Level::Debug,
        zutalk_log::Limits::default(),
    )
    .expect("open real client file log");

    let asked_soniox = Asked::default();
    let stt = soniox(&asked_soniox).await;

    let ai_title_for_mock = ai_title_sentinel.to_string();
    let ai_cand_for_mock = ai_cand_sentinel.to_string();
    let ai_tag_for_mock = ai_tag_sentinel.to_string();
    let openrouter = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let schema_name = body
                .pointer("/response_format/json_schema/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if schema_name == "recording_titles_and_tags" {
                let payload = json!({
                    "titles": [ai_title_for_mock, ai_cand_for_mock],
                    "tags": [{ "label": ai_tag_for_mock, "at_ms": 200 }]
                });
                ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{
                        "finish_reason": "stop",
                        "message": { "content": payload.to_string() }
                    }]
                }))
            } else {
                let reply = json!({
                    "lines": [{ "id": 0, "unclear": false, "facts": [], "to": { "zh": "翻译测试句。" } }]
                });
                ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{
                        "finish_reason": "stop",
                        "message": { "content": reply.to_string() }
                    }]
                }))
            }
        })
        .mount(&openrouter)
        .await;

    let realtime = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind ws");
    let cfg = Config {
        soniox: format!("{}/v1", stt.uri()),
        openrouter: format!("{}/api/v1", openrouter.uri()),
        realtime: format!("ws://{}", realtime.local_addr().expect("ws addr")),
        ..config(data_dir.path())
    };
    let mut app = FileApp::start(&scope, cfg);
    app.until(|s| s.me.is_some()).await;

    // 1. Save topic, import `.md` file from nested sentinel dir, and remember term.
    app.send(
        &scope,
        Command::SaveTopic {
            topic: Topic {
                name: topic_name_sentinel.into(),
                domain: topic_domain_sentinel.into(),
                facts: vec![TopicFact {
                    key: topic_fact_k_sentinel.into(),
                    value: topic_fact_v_sentinel.into(),
                }],
                background: topic_bg_sentinel.into(),
                terms: vec![Term {
                    said: topic_term_said_sentinel.into(),
                    written: owned(&[("zh", topic_term_zh_sentinel)]),
                }],
                translation_terms: vec![TranslationPair {
                    source: topic_pair_src_sentinel.into(),
                    target: topic_pair_dst_sentinel.into(),
                }],
                ..Topic::default()
            },
        },
    );

    let topic_id = loop {
        if let Ok(reply) = app.core.query(Query::Topics).await
            && let QueryValue::Topics(list) = reply.value
            && let Some(found) = list.iter().find(|t| t.name == topic_name_sentinel)
        {
            break found.id.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let import_dir = data_dir.path().join(import_dir_sentinel);
    fs::create_dir_all(&import_dir).expect("mkdir import dir");
    let handout_path = import_dir.join(import_file_sentinel);
    fs::write(
        &handout_path,
        format!("# {topic_name_sentinel}\n\n- **{import_suggested_sentinel}**: 讲义词\n"),
    )
    .expect("write sentinel handout");
    app.send(
        &scope,
        Command::ImportTopicFile {
            id: topic_id.clone(),
            path: handout_path.to_string_lossy().into_owned(),
        },
    );
    loop {
        if let Ok(reply) = app.core.query(Query::Topic { id: topic_id.clone() }).await
            && let QueryValue::Topic(Some(t)) = reply.value
            && t.source_file.as_deref() == Some(import_file_sentinel)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    app.send(
        &scope,
        Command::RememberTerm {
            topic_id: topic_id.clone(),
            said: import_suggested_sentinel.into(),
            written: BTreeMap::new(),
        },
    );
    app.notice("topic.remembered").await;

    // 2. Select topic in Setup, start recording, stream live token, wait for tape, stop.
    app.send(
        &scope,
        Command::Setup {
            setup: AppSetup {
                spoken: vec!["en".into(), "zh".into()],
                shown: vec!["en".into(), "zh".into()],
                topics: vec![topic_id.clone()],
                ..AppSetup::default()
            },
        },
    );
    app.until(|s| s.setup.topics == vec![topic_id.clone()]).await;

    app.send(&scope, Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("live session").id;

    let (stream, _) = realtime.accept().await.expect("ws accept");
    let mut ws = accept_async(stream).await.expect("ws handshake");
    let _ = ws.next().await;
    let tokens = json!({
        "tokens": [
            { "text": transcript_sentinel, "start_ms": 100, "end_ms": 500, "is_final": true, "language": "en", "speaker": "1" },
            { "text": "<end>", "is_final": true }
        ],
        "total_audio_proc_ms": 600
    });
    ws.send(Message::text(tokens.to_string())).await.expect("send live tokens");
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                let _ = ws
                    .send(Message::text(json!({ "tokens": [], "finished": true }).to_string()))
                    .await;
            }
        }
    });

    wait_for_tape(data_dir.path(), &rec_id).await;
    app.send(&scope, Command::Stop);
    app.session(|s| s.is_none()).await;

    // 3. Wait for refine AND automatic AI title generation to complete, then export Diagnose.
    app.wait_recording(&rec_id, |v| {
        v.is_some_and(|r| {
            !r.row.draft
                && r.take == 1
                && r.row.title == ai_title_sentinel
                && r.row.title_by == TitleBy::Ai
                && r.row.tags.iter().any(|t| t.label == ai_tag_sentinel)
        })
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while app.outlet.progress.now().1.refine.is_some()
        || app.outlet.progress.now().1.title.is_some()
    {
        assert!(tokio::time::Instant::now() < deadline, "background progress did not clear");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    app.send(&scope, Command::Diagnose);
    app.notice("diagnose.done").await;

    // Drop core and file-log guard so every event flushes cleanly to disk.
    drop(app);
    drop(guard);

    // 4. Byte-scan `<data>/logs/*.jsonl`, `<data>/diagnose/**/*`, and `<data>/settings.json`.
    let sentinels = [
        topic_name_sentinel,
        topic_domain_sentinel,
        topic_fact_k_sentinel,
        topic_fact_v_sentinel,
        topic_bg_sentinel,
        topic_term_said_sentinel,
        topic_term_zh_sentinel,
        topic_pair_src_sentinel,
        topic_pair_dst_sentinel,
        import_dir_sentinel,
        import_file_sentinel,
        import_suggested_sentinel,
        ai_title_sentinel,
        ai_cand_sentinel,
        ai_tag_sentinel,
        transcript_sentinel,
        soniox_key,
        openrouter_key,
        hub::ME,
        "did:zu:",
    ];

    // settings.json stores ONLY random hex topic id, never any topic name/content/secrets.
    let settings_raw =
        fs::read_to_string(data_dir.path().join("settings.json")).expect("settings.json exists");
    assert!(settings_raw.contains(&topic_id), "settings.json should retain selected hex topic id");
    for needle in sentinels {
        assert!(!settings_raw.contains(needle), "settings.json leaked `{needle}`");
    }

    // logs/*.jsonl: flat JSON, zero refused `!`, zero sentinels, required P6 events present,
    // stats.full == 0.
    let mut log_lines = Vec::new();
    for entry in fs::read_dir(&log_dir).expect("read logs dir") {
        let p = entry.expect("entry").path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let content = fs::read_to_string(&p).expect("read jsonl");
            for line in content.lines().filter(|l| !l.trim().is_empty()) {
                log_lines.push(line.to_owned());
            }
        }
    }
    assert!(!log_lines.is_empty(), "real logs/zutalk.jsonl must not be empty");

    let mut seen_topic_saved = false;
    let mut seen_ctx_live = false;
    let mut seen_ctx_refine = false;
    let mut seen_titles_done = false;
    let mut seen_diagnose_wrote = false;
    let mut stats_full_count = 0usize;

    for (idx, line) in log_lines.iter().enumerate() {
        let parsed: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("log line {idx} invalid JSON: {e}: {line}"));
        let obj = parsed.as_object().expect("flat json object");
        for (k, v) in obj {
            assert!(!v.is_object() && !v.is_array(), "nested log value at line {idx} key `{k}`");
            assert!(v.as_str() != Some("!"), "refused field `!` at line {idx} key `{k}`");
        }
        for needle in sentinels {
            assert!(!line.contains(needle), "log line {idx} leaked `{needle}`: {line}");
        }
        match obj.get("event").and_then(Value::as_str) {
            Some("topic.saved") => seen_topic_saved = true,
            Some("topic.context") => match obj.get("where").and_then(Value::as_str) {
                Some("live") => seen_ctx_live = true,
                Some("refine") => seen_ctx_refine = true,
                _ => {}
            },
            Some("titles.done") => {
                if obj.get("code").and_then(Value::as_str) == Some("ok")
                    && obj.get("applied").and_then(Value::as_bool) == Some(true)
                {
                    seen_titles_done = true;
                }
            }
            Some("diagnose.wrote") => seen_diagnose_wrote = true,
            Some("stats.full") => stats_full_count += 1,
            _ => {}
        }
    }

    assert!(seen_topic_saved, "logs missing topic.saved");
    assert!(seen_ctx_live, "logs missing topic.context(where=live)");
    assert!(seen_ctx_refine, "logs missing topic.context(where=refine)");
    assert!(seen_titles_done, "logs missing titles.done(code=ok, applied=true)");
    assert!(seen_diagnose_wrote, "logs missing diagnose.wrote");
    assert_eq!(stats_full_count, 0, "stats.full must never fire");
    assert_eq!(zutalk_log::refused(), refused_before, "zutalk_log::refused() must not increase");

    // Diagnose bundle: zero sentinels across every file + Tally JSON <= 8192 B.
    let diagnose_root = data_dir.path().join("diagnose");
    let mut bundle_dirs: Vec<PathBuf> =
        fs::read_dir(&diagnose_root).expect("diagnose dir").flatten().map(|e| e.path()).collect();
    bundle_dirs.sort();
    assert_eq!(bundle_dirs.len(), 1, "expected 1 diagnose bundle");

    let tally_raw = fs::read_to_string(bundle_dirs[0].join("stats.json")).expect("stats.json");
    let tally: Value = serde_json::from_str(&tally_raw).expect("parse Tally JSON");
    let tally_bytes = serde_json::to_vec(&tally).expect("compact Tally JSON").len();
    assert!(tally_bytes <= 8192, "Tally must stay <= 8192 B, got {tally_bytes} B");

    let mut bundle_files = Vec::new();
    collect_files(&diagnose_root, &mut bundle_files);
    assert!(!bundle_files.is_empty());
    for file in &bundle_files {
        let bytes = fs::read(file).expect("read diagnose file");
        let text = String::from_utf8_lossy(&bytes);
        for needle in sentinels {
            assert!(!text.contains(needle), "diagnose file `{}` leaked `{needle}`", file.display());
        }
    }
}
