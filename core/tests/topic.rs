//! P6-01 tests: `talk/topic` knowledge model, pinned `always` topic, legacy `talk/term` lossless
//! migration, bounded UTF-8 file extraction, and sign-in-free commands/queries with safe events.

mod hub;

use futures_util::{SinkExt, StreamExt};
use hub::{
    App, Asked, NOWHERE, STORE, config, config_on, nobody, shelf, signed_in, soniox, store, tape,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zu::SecretKey;
use zutalk_core::app::hub::Config;
use zutalk_core::app::proto::{Command, Query, QueryValue, RefineStatus, Setup as AppSetup};
use zutalk_core::id::keys;
use zutalk_core::knowledge::glossary::{self, Term};
use zutalk_core::knowledge::topic::{
    self, ALWAYS_TOPIC_ID, CONTEXT_MAX_GENERAL_ITEMS, CONTEXT_MAX_TERMS, CONTEXT_MAX_TEXT_CHARS,
    CONTEXT_MAX_TRANSLATION_TERMS, MAX_SELECTED_TOPICS, TOPIC_IMPORT_BACKGROUND_CHARS,
    TOPIC_IMPORT_MAX_BYTES, TOPIC_MAX_CHARS, TOPIC_MAX_SUGGESTED_TERMS, Topic, TopicFact,
    TranslationPair,
};
use zutalk_core::live::lines::Line;
use zutalk_core::record::shelf::{About, LINE, RECORDING, Said, Shelf};

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const NOW: i64 = 1_800_000_000_000;

fn owned(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn always_topic_is_always_present_cannot_be_removed_and_migrates_legacy_terms() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = Shelf::new(dir.path(), ME, Arc::new(SecretKey::from_seed(&[5; 32])));

    // Empty shelf still lists synthesized pinned `always` topic without writing to disk.
    let initial = topic::list(&shelf).expect("list empty");
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].id, ALWAYS_TOPIC_ID);
    assert!(initial[0].pinned);
    assert!(initial[0].terms.is_empty());
    assert!(!shelf.pack_path("", zutalk_core::record::shelf::TOPIC).exists());

    // Legacy `talk/term` records migrate losslessly into `always` with all multilingual `written`.
    glossary::put(&shelf, "Zuzalu", Some(owned(&[("zh", "祖扎卢"), ("th", "ซูซาลู")])), NOW)
        .expect("put zuzalu");
    glossary::put(&shelf, "ZuTalk", Some(BTreeMap::new()), NOW + 1).expect("put zutalk");

    let migrated = topic::ensure_migrated(&shelf, NOW + 10).expect("migrate");
    assert!(migrated, "first migration should write always topic");
    let second = topic::ensure_migrated(&shelf, NOW + 11).expect("migrate idempotent");
    assert!(!second, "second migration must be a no-op");

    let always = topic::get(&shelf, ALWAYS_TOPIC_ID).expect("get always").expect("always exists");
    assert!(always.pinned);
    assert_eq!(always.terms.len(), 2);
    let zuzalu = always.terms.iter().find(|t| t.said == "Zuzalu").expect("Zuzalu migrated");
    assert_eq!(zuzalu.written, owned(&[("zh", "祖扎卢"), ("th", "ซูซาลู")]));
    assert!(always.terms.iter().any(|t| t.said == "ZuTalk"));

    // Pinned `always` topic can never be removed.
    assert_eq!(topic::remove(&shelf, ALWAYS_TOPIC_ID, NOW + 20), Err("topic.pinned"));
    assert!(topic::get(&shelf, ALWAYS_TOPIC_ID).expect("still there").is_some());
}

#[test]
fn topic_normalize_enforces_budgets_deduplication_and_basename_only() {
    // Empty name on non-pinned topic is rejected.
    let empty_name = Topic { name: "   ".into(), ..Topic::default() };
    assert_eq!(topic::normalize(empty_name, NOW), Err("topic.empty"));

    // Over 10,000 chars total is rejected with `topic.limit`.
    let mut big_terms = Vec::new();
    for i in 0..120 {
        big_terms.push(Term {
            said: format!("term-{i:03}-{}", "x".repeat(90)),
            written: BTreeMap::new(),
        });
    }
    let over_limit = Topic { name: "Too Big".into(), terms: big_terms, ..Topic::default() };
    assert!(topic::size_chars(&over_limit) > TOPIC_MAX_CHARS);
    assert_eq!(topic::normalize(over_limit, NOW), Err("topic.limit"));

    // Deduplicates terms (merging `written`), strips full paths to basename, and forces pinned=false
    // on non-always ids.
    let raw = Topic {
        id: String::new(),
        name: "  Reading Club  ".into(),
        domain: "  Philosophy ".into(),
        facts: vec![
            TopicFact { key: " Host ".into(), value: " Room A ".into() },
            TopicFact { key: "host".into(), value: "room a".into() },
            TopicFact { key: " ".into(), value: "ignored".into() },
        ],
        background: "  Notes about the third evening.  ".into(),
        terms: vec![
            Term { said: "Zuzalu".into(), written: owned(&[("zh", "祖扎卢")]) },
            Term {
                said: "ＺＵＺＡＬＵ".into(), written: owned(&[("th", "ซูซาลู")])
            },
            Term { said: " ".into(), written: BTreeMap::new() },
        ],
        translation_terms: vec![
            TranslationPair { source: "Agora".into(), target: "广场".into() },
            TranslationPair { source: "agora".into(), target: "广场".into() },
            TranslationPair { source: "Same".into(), target: "Same".into() },
        ],
        source_file: Some("/Users/secret/path/to/handout-night3.md".into()),
        suggested_terms: vec!["Zuzalu".into(), "Polis".into(), "polis".into()],
        pinned: true, // non-always topic cannot forge pinned=true
        updated_at: 0,
    };
    let clean = topic::normalize(raw, NOW).expect("normalized");
    assert_eq!(clean.id.len(), 32);
    assert!(clean.id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert!(!clean.pinned);
    assert_eq!(clean.name, "Reading Club");
    assert_eq!(clean.domain, "Philosophy");
    assert_eq!(clean.facts.len(), 1);
    assert_eq!(clean.terms.len(), 1);
    assert_eq!(clean.terms[0].written, owned(&[("zh", "祖扎卢"), ("th", "ซูซาลู")]));
    assert_eq!(clean.translation_terms.len(), 1);
    assert_eq!(clean.source_file.as_deref(), Some("handout-night3.md"));
    assert_eq!(clean.suggested_terms, vec!["Polis".to_string()]);
    assert_eq!(clean.updated_at, NOW);

    // Windows-style path is also stripped to basename only.
    let win = Topic {
        name: "Win Path".into(),
        source_file: Some(r"C:\Users\private\docs\notes.txt".into()),
        ..Topic::default()
    };
    let win_clean = topic::normalize(win, NOW).expect("win normalized");
    assert_eq!(win_clean.source_file.as_deref(), Some("notes.txt"));
}

#[test]
fn extract_from_file_handles_utf8_markdown_and_rejects_oversized_or_binary() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Valid UTF-8 Markdown: extracts bounded background and deduplicated suggested_terms.
    let md_path = dir.path().join("night3-handout.md");
    let md = format!(
        "# 第三夜讲义\n\n讨论《理想国》与「洞穴比喻」。\n\n- **Agora**: 城邦公共广场\n- **Polis** -> 城邦\n- Soniox 与 OpenRouter 会在会议中多次提到。Soniox 和 OpenRouter 的配置见下文。\n\n{}\n",
        "背景正文段落。".repeat(250)
    );
    fs::write(&md_path, &md).expect("write md");
    let extracted = topic::extract_from_file(&md_path).expect("extract md");
    assert_eq!(extracted.source_file, "night3-handout.md");
    assert!(extracted.background.chars().count() <= TOPIC_IMPORT_BACKGROUND_CHARS);
    assert!(!extracted.background.is_empty());
    assert!(extracted.suggested_terms.len() <= TOPIC_MAX_SUGGESTED_TERMS);
    assert!(
        extracted.suggested_terms.iter().any(|t| t == "理想国"),
        "expected 《理想国》 in {:?}",
        extracted.suggested_terms
    );
    assert!(
        extracted.suggested_terms.iter().any(|t| t == "Agora"),
        "expected Agora in {:?}",
        extracted.suggested_terms
    );

    // Importing into a shelf topic puts candidates ONLY in suggested_terms, never auto-adds to terms.
    let shelf = Shelf::new(dir.path().join("shelf"), ME, Arc::new(SecretKey::from_seed(&[6; 32])));
    let saved = topic::save(&shelf, Topic { name: "Night 3".into(), ..Topic::default() }, NOW)
        .expect("save topic");
    let imported = topic::import_file(&shelf, &saved.id, &md_path, NOW + 1).expect("import md");
    assert_eq!(imported.source_file.as_deref(), Some("night3-handout.md"));
    assert!(imported.terms.is_empty(), "suggestions must never be auto-added to terms");
    assert!(!imported.suggested_terms.is_empty());
    assert!(!imported.background.is_empty());

    // Unsupported extension (.pdf) returns topic.file.format.
    let pdf_path = dir.path().join("handout.pdf");
    fs::write(&pdf_path, b"%PDF-1.4 fake").expect("write pdf");
    assert_eq!(topic::extract_from_file(&pdf_path), Err("topic.file.format"));

    // Invalid UTF-8 bytes in .txt returns topic.file.format.
    let bad_utf8 = dir.path().join("broken.txt");
    fs::write(&bad_utf8, [0xff, 0xfe, 0x00, 0x80]).expect("write bad utf8");
    assert_eq!(topic::extract_from_file(&bad_utf8), Err("topic.file.format"));

    // Oversized file (> 512 KiB) returns topic.file.big.
    let big_path = dir.path().join("huge.txt");
    fs::write(&big_path, vec![b'a'; TOPIC_IMPORT_MAX_BYTES + 1]).expect("write huge");
    assert_eq!(topic::extract_from_file(&big_path), Err("topic.file.big"));
}

#[tokio::test(flavor = "multi_thread")]
async fn topic_commands_and_queries_work_without_sign_in_and_emit_safe_events() {
    let _store = STORE.lock().await;
    nobody();
    let dir = tempfile::tempdir().expect("tempdir");

    let mut app = App::start(dir.path());
    let state = app.until(|s| s.me.is_none()).await;
    assert!(state.me.is_none(), "must run signed out on local identity");

    let sentinel_name = "SecretBookClubSentinel";
    let sentinel_domain = "SecretDomainSentinel";
    let sentinel_bg = "SecretBackgroundNarrativeSentinel";
    let sentinel_term = "SecretTermSentinel";
    let sentinel_file = "SecretHandoutFilenameSentinel.md";
    let handout_path = dir.path().join(sentinel_file);
    fs::write(
        &handout_path,
        format!("# {sentinel_name}\n\n- {sentinel_term}: 秘密术语\n\n{sentinel_bg}\n"),
    )
    .expect("write handout");

    // 1. SaveTopic without signing in.
    app.send(Command::SaveTopic {
        topic: Topic {
            name: sentinel_name.into(),
            domain: sentinel_domain.into(),
            ..Topic::default()
        },
    });
    let summaries = loop {
        if let Ok(reply) = app.query(Query::Topics).await
            && let QueryValue::Topics(list) = reply.value
            && list.iter().any(|t| t.name == sentinel_name)
        {
            break list;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(summaries[0].id, ALWAYS_TOPIC_ID);
    assert!(summaries[0].pinned);
    let created_id = summaries.iter().find(|t| t.name == sentinel_name).unwrap().id.clone();

    // 2. Query::Topic returns full detail.
    let detail = match app.query(Query::Topic { id: created_id.clone() }).await.unwrap().value {
        QueryValue::Topic(Some(t)) => t,
        other => panic!("unexpected topic reply: {other:?}"),
    };
    assert_eq!(detail.name, sentinel_name);
    assert_eq!(detail.domain, sentinel_domain);

    // 3. ImportTopicFile attaches basename + suggestions without auto-adding to terms.
    app.send(Command::ImportTopicFile {
        id: created_id.clone(),
        path: handout_path.to_string_lossy().into_owned(),
    });
    let imported = loop {
        if let Ok(reply) = app.query(Query::Topic { id: created_id.clone() }).await
            && let QueryValue::Topic(Some(t)) = reply.value
            && t.source_file.as_deref() == Some(sentinel_file)
        {
            break t;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert!(imported.terms.is_empty());
    assert!(imported.suggested_terms.iter().any(|s| s == sentinel_term));

    // 4. RememberTerm adds term and removes it from suggested_terms, emitting topic.remembered.
    app.send(Command::RememberTerm {
        topic_id: created_id.clone(),
        said: sentinel_term.into(),
        written: owned(&[("zh", "秘密术语")]),
    });
    app.notice("topic.remembered").await;
    let remembered = match app.query(Query::Topic { id: created_id.clone() }).await.unwrap().value {
        QueryValue::Topic(Some(t)) => t,
        other => panic!("unexpected topic reply: {other:?}"),
    };
    assert!(remembered.terms.iter().any(|t| t.said == sentinel_term));
    assert!(!remembered.suggested_terms.iter().any(|s| s == sentinel_term));

    // 5. Removing pinned `always` fails with topic.pinned and logs topic.saved(code="topic.pinned").
    app.send(Command::RemoveTopic { id: ALWAYS_TOPIC_ID.into() });
    app.notice("topic.pinned").await;

    // 6. RemoveTopic deletes custom topic cleanly.
    app.send(Command::RemoveTopic { id: created_id.clone() });
    loop {
        if let Ok(reply) = app.query(Query::Topic { id: created_id.clone() }).await
            && let QueryValue::Topic(None) = reply.value
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // 7. Observability & strict zero-leak privacy audit.
    let saved_events = app.seen.of("topic.saved");
    assert!(
        saved_events.iter().any(|l| l.text("op") == Some("save") && l.text("code") == Some("ok"))
    );
    assert!(
        saved_events.iter().any(|l| l.text("op") == Some("import") && l.text("code") == Some("ok"))
    );
    assert!(
        saved_events
            .iter()
            .any(|l| l.text("op") == Some("remember") && l.text("code") == Some("ok"))
    );
    assert!(
        saved_events
            .iter()
            .any(|l| l.text("op") == Some("remove") && l.text("code") == Some("topic.pinned"))
    );
    assert!(
        saved_events.iter().any(|l| l.text("op") == Some("remove") && l.text("code") == Some("ok"))
    );

    let dumped = format!("{:?}", app.seen.all());
    for secret in
        [sentinel_name, sentinel_domain, sentinel_bg, sentinel_term, sentinel_file, &created_id]
    {
        assert!(!dumped.contains(secret), "topic secret `{secret}` leaked into logs");
    }
    assert_eq!(app.seen.refused(), 0);
}

#[test]
fn assemble_context_includes_pinned_and_selected_only_and_enforces_budgets() {
    let always = Topic {
        id: ALWAYS_TOPIC_ID.into(),
        pinned: true,
        domain: "AlwaysDomain".into(),
        facts: vec![TopicFact { key: "Org".into(), value: "AlwaysOrg".into() }],
        background: "Always background notes.".into(),
        terms: vec![Term { said: "AlwaysTerm".into(), written: owned(&[("zh", "常驻词")]) }],
        translation_terms: vec![TranslationPair {
            source: "AlwaysSrc".into(),
            target: "AlwaysDst".into(),
        }],
        ..Topic::default()
    };

    // Selected Topic A has huge background + many facts/terms/pairs to exercise every ceiling.
    let mut facts_a = Vec::new();
    for i in 0..30 {
        facts_a.push(TopicFact { key: format!("k{i}"), value: format!("SelectedFact{i}") });
    }
    let mut terms_a = Vec::new();
    for i in 0..320 {
        terms_a.push(Term { said: format!("SelectedTerm{i}"), written: BTreeMap::new() });
    }
    let mut pairs_a = Vec::new();
    for i in 0..320 {
        pairs_a.push(TranslationPair { source: format!("SrcA{i}"), target: format!("DstA{i}") });
    }
    let topic_a = Topic {
        id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        name: "Topic A".into(),
        domain: "SelectedDomainA".into(),
        facts: facts_a,
        background: "精选背景中文。".repeat(2_000),
        terms: terms_a,
        translation_terms: pairs_a,
        ..Topic::default()
    };

    // Unselected Topic B must NEVER leak into assembled context.
    let topic_b = Topic {
        id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        name: "Secret Topic B".into(),
        domain: "UNSELECTED_DOMAIN_B".into(),
        facts: vec![TopicFact { key: "Secret".into(), value: "UNSELECTED_FACT_B".into() }],
        background: "UNSELECTED_BACKGROUND_B".into(),
        terms: vec![Term {
            said: "UNSELECTED_TERM_B".into(),
            written: owned(&[("zh", "UNSELECTED_ZH_B")]),
        }],
        translation_terms: vec![TranslationPair {
            source: "UNSELECTED_SRC_B".into(),
            target: "UNSELECTED_DST_B".into(),
        }],
        ..Topic::default()
    };

    // Also include a missing/deleted topic id to confirm it is ignored cleanly without error.
    let selected = vec![topic_a.id.clone(), "deadbeefdeadbeefdeadbeefdeadbeef".into()];
    let ctx = topic::assemble_context(&[always, topic_a, topic_b], &selected, "Session Title");

    assert_eq!(ctx.topic_count, 2);
    assert!(ctx.general.len() <= CONTEXT_MAX_GENERAL_ITEMS);
    assert!(ctx.text.chars().count() <= CONTEXT_MAX_TEXT_CHARS);
    assert!(ctx.terms.len() <= CONTEXT_MAX_TERMS);
    assert!(ctx.translation_pairs.len() <= CONTEXT_MAX_TRANSLATION_TERMS);

    // Pinned `always` + selected Topic A + recording title are present.
    assert!(ctx.general.iter().any(|(k, v)| k == "topic" && v == "Session Title"));
    assert!(ctx.general.iter().any(|(k, v)| k == "domain" && v == "AlwaysDomain"));
    assert!(ctx.general.iter().any(|(k, v)| k == "domain" && v == "SelectedDomainA"));
    assert!(ctx.text.contains("Always background notes."));
    assert!(ctx.text.contains("精选背景中文。"));
    assert!(ctx.terms.iter().any(|t| t.said == "AlwaysTerm"));
    assert!(ctx.terms.iter().any(|t| t.said == "SelectedTerm0"));
    assert!(ctx.translation_pairs.iter().any(|p| p.source == "AlwaysSrc"));
    assert!(ctx.translation_pairs.iter().any(|p| p.source == "SrcA0"));

    // Unselected Topic B is 100% absent from every field.
    let serialized =
        format!("{:?} {} {:?} {:?}", ctx.general, ctx.text, ctx.terms, ctx.translation_pairs);
    for forbidden in [
        "UNSELECTED_DOMAIN_B",
        "UNSELECTED_FACT_B",
        "UNSELECTED_BACKGROUND_B",
        "UNSELECTED_TERM_B",
        "UNSELECTED_ZH_B",
        "UNSELECTED_SRC_B",
        "UNSELECTED_DST_B",
    ] {
        assert!(!serialized.contains(forbidden), "unselected `{forbidden}` leaked into context");
    }

    // TopicContext Debug is strictly redacted (prints only counts/lengths, zero content).
    let debug = format!("{ctx:?}");
    assert!(debug.contains("topic_count"));
    assert!(debug.contains("text_chars"));
    for secret in
        ["AlwaysTerm", "AlwaysDomain", "Always background", "SelectedTerm0", "Session Title"]
    {
        assert!(!debug.contains(secret), "TopicContext Debug leaked `{secret}`: {debug}");
    }

    // normalize_selected_topics drops always, blanks, invalid/oversized ids, deduplicates, caps at 8.
    let mut many: Vec<String> = (0..12).map(|i| format!("{i:032x}")).collect();
    many.insert(0, ALWAYS_TOPIC_ID.into());
    many.insert(1, "  ".into());
    many.insert(2, "x".repeat(70));
    many.insert(3, format!("{:032x}", 0));
    let clean = topic::normalize_selected_topics(many);
    assert_eq!(clean.len(), MAX_SELECTED_TOPICS);
    assert!(!clean.iter().any(|id| id == ALWAYS_TOPIC_ID));
    assert_eq!(clean[0], format!("{:032x}", 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_topic_reaches_live_and_refine_while_unselected_topic_stays_local() {
    let _store = STORE.lock().await;
    signed_in();
    keys::set("openrouter", Some(b"or-own-test-key")).expect("openrouter key");
    let (dir, asked_soniox) = (tempfile::tempdir().expect("tempdir"), Asked::default());
    let stt = soniox(&asked_soniox).await;

    // Stand-in OpenRouter capturing translation request body.
    let asked_or: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let asked_or_tap = asked_or.clone();
    let openrouter = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            asked_or_tap.lock().unwrap().push(body);
            let reply = json!({
                "lines": [{ "id": 0, "unclear": false, "facts": [], "to": { "zh": "在祖扎鲁讨论雅典城邦。" } }]
            });
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": { "content": reply.to_string() }
                }]
            }))
        })
        .mount(&openrouter)
        .await;

    let realtime = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind ws");
    let config = Config {
        soniox: format!("{}/v1", stt.uri()),
        openrouter: format!("{}/api/v1", openrouter.uri()),
        realtime: format!("ws://{}", realtime.local_addr().expect("addr")),
        ..config(dir.path())
    };
    let mut app = App::with(config);
    app.until(|_| true).await;

    // Seed pinned `always`, selected topic A, and unselected topic B.
    app.send(Command::SaveTopic {
        topic: Topic {
            id: ALWAYS_TOPIC_ID.into(),
            pinned: true,
            terms: vec![Term {
                said: "AlwaysAlpha".into(), written: owned(&[("zh", "常驻词甲")])
            }],
            ..Topic::default()
        },
    });
    app.send(Command::SaveTopic {
        topic: Topic {
            name: "Book Club A".into(),
            domain: "SelectedDomainA".into(),
            facts: vec![TopicFact { key: "Book".into(), value: "RepublicA".into() }],
            background: "SelectedBackgroundA explains the cave allegory.".into(),
            terms: vec![Term { said: "PolisAlpha".into(), written: owned(&[("zh", "城邦甲")]) }],
            translation_terms: vec![TranslationPair {
                source: "AgoraAlpha".into(),
                target: "广场甲".into(),
            }],
            ..Topic::default()
        },
    });
    app.send(Command::SaveTopic {
        topic: Topic {
            name: "Private Topic B".into(),
            domain: "UnselectedDomainB".into(),
            facts: vec![TopicFact { key: "Secret".into(), value: "UnselectedFactB".into() }],
            background: "UnselectedBackgroundB must never leave this device.".into(),
            terms: vec![Term {
                said: "SecretBetaTerm".into(),
                written: owned(&[("zh", "秘密词乙")]),
            }],
            translation_terms: vec![TranslationPair {
                source: "SecretBetaSrc".into(),
                target: "秘密乙".into(),
            }],
            ..Topic::default()
        },
    });

    let summaries = loop {
        if let Ok(reply) = app.query(Query::Topics).await
            && let QueryValue::Topics(list) = reply.value
            && list.iter().any(|t| t.name == "Book Club A")
            && list.iter().any(|t| t.name == "Private Topic B")
        {
            break list;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let id_a = summaries.iter().find(|t| t.name == "Book Club A").unwrap().id.clone();

    // Select ONLY Topic A in Setup, then start recording.
    app.send(Command::Setup {
        setup: AppSetup {
            spoken: vec!["en".into(), "zh".into()],
            shown: vec!["en".into(), "zh".into()],
            topics: vec![id_a.clone()],
            ..AppSetup::default()
        },
    });
    app.until(|s| s.setup.topics == vec![id_a.clone()]).await;
    app.send(Command::Start);

    // 1. Live WebSocket first config frame carries always + A, zero B.
    let (stream, _) = realtime.accept().await.expect("ws accept");
    let mut ws = accept_async(stream).await.expect("ws handshake");
    let Some(Ok(Message::Text(first_raw))) = ws.next().await else {
        panic!("missing live config frame");
    };
    let first: Value = serde_json::from_str(&first_raw).expect("ws json");
    let first_dump = first.to_string();
    assert!(first_dump.contains("AlwaysAlpha"), "live missing always term: {first_dump}");
    assert!(first_dump.contains("SelectedDomainA"), "live missing A domain: {first_dump}");
    assert!(first_dump.contains("SelectedBackgroundA"), "live missing A background: {first_dump}");
    assert!(first_dump.contains("PolisAlpha"), "live missing A term: {first_dump}");
    assert!(first_dump.contains("AgoraAlpha"), "live missing A pair: {first_dump}");
    for forbidden in [
        "UnselectedDomainB",
        "UnselectedFactB",
        "UnselectedBackgroundB",
        "SecretBetaTerm",
        "SecretBetaSrc",
    ] {
        assert!(!first_dump.contains(forbidden), "live leaked unselected `{forbidden}`");
    }

    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                let finished = json!({ "tokens": [], "finished": true }).to_string();
                let _ = ws.send(Message::text(finished)).await;
            }
        }
    });

    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);

    // 2. Refine Soniox async POST /transcriptions + OpenRouter POST /chat/completions.
    let wait = async {
        while asked_soniox.lock().unwrap().is_empty() || asked_or.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait).await.expect("refine requests sent");

    let soniox_dump = asked_soniox.lock().unwrap()[0].to_string();
    assert!(soniox_dump.contains("AlwaysAlpha"));
    assert!(soniox_dump.contains("SelectedDomainA"));
    assert!(soniox_dump.contains("SelectedBackgroundA"));
    assert!(soniox_dump.contains("PolisAlpha"));
    assert!(soniox_dump.contains("AgoraAlpha"));
    for forbidden in [
        "UnselectedDomainB",
        "UnselectedFactB",
        "UnselectedBackgroundB",
        "SecretBetaTerm",
        "SecretBetaSrc",
    ] {
        assert!(!soniox_dump.contains(forbidden), "soniox leaked unselected `{forbidden}`");
    }

    let or_dump = asked_or.lock().unwrap()[0].to_string();
    assert!(or_dump.contains("SelectedDomainA"));
    assert!(or_dump.contains("SelectedBackgroundA"));
    assert!(or_dump.contains("PolisAlpha"));
    assert!(or_dump.contains("AgoraAlpha"));
    for forbidden in [
        "UnselectedDomainB",
        "UnselectedFactB",
        "UnselectedBackgroundB",
        "SecretBetaTerm",
        "SecretBetaSrc",
    ] {
        assert!(!or_dump.contains(forbidden), "openrouter leaked unselected `{forbidden}`");
    }

    // 3. Structured `topic.context` logs emitted for live & refine with zero content leak.
    let ctx_events = app.seen.of("topic.context");
    assert!(
        ctx_events.iter().any(|l| l.text("where") == Some("live") && l.u64("topics") == Some(2))
    );
    assert!(
        ctx_events.iter().any(|l| l.text("where") == Some("refine") && l.u64("topics") == Some(2))
    );
    let log_dump = format!("{:?}", app.seen.all());
    for secret in [
        "AlwaysAlpha",
        "SelectedDomainA",
        "SelectedBackgroundA",
        "PolisAlpha",
        "AgoraAlpha",
        "UnselectedBackgroundB",
        "SecretBetaTerm",
    ] {
        assert!(!log_dump.contains(secret), "logs leaked `{secret}`");
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn granted_key_async_refusal_keeps_draft_with_clear_code_after_topic_context() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("tempdir");

    let s = shelf(dir.path());
    let saved_topic = topic::save(
        &s,
        Topic {
            name: "Quota Context Topic".into(),
            background: "Background kept local when quota key is refused.".into(),
            terms: vec![Term { said: "QuotaTerm".into(), written: BTreeMap::new() }],
            ..Topic::default()
        },
        NOW,
    )
    .expect("save topic");

    let rec_id = "19a00000009quota403";
    let about = About {
        started: NOW,
        length_ms: 2_000,
        title: "Quota Refine Test".into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        take: 0,
        topics: vec![saved_topic.id.clone()],
        ..About::default()
    };
    s.put(rec_id, RECORDING, &[(rec_id.into(), Some(about))], NOW).expect("seed about");
    let draft_line = Said {
        line: Line {
            t0: 0,
            t1: 1_000,
            speaker: "1".into(),
            language: "en".into(),
            text: "draft survives refusal".into(),
            ..Line::default()
        },
        via: BTreeMap::from([("text".into(), "stt-rt-v6".into())]),
    };
    s.put(rec_id, LINE, &[(format!("{rec_id}.0.00000"), Some(draft_line))], NOW + 1)
        .expect("seed draft");
    tape(dir.path(), rec_id, 2);

    // Granted Soniox key server + 403 refusing Soniox async endpoint.
    let our_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/lease"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "key": "sk-granted-temp", "left": 5_000_000 })),
        )
        .mount(&our_server)
        .await;

    let refusing_soniox = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/files"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&refusing_soniox)
        .await;

    let mut app = App::with(Config {
        soniox: format!("{}/v1", refusing_soniox.uri()),
        openrouter: NOWHERE.into(),
        ..config_on(dir.path(), &our_server.uri())
    });

    app.notice("keys.async_refused").await;
    app.wait_recording(rec_id, |r| {
        r.is_some_and(|v| {
            v.row.draft
                && matches!(v.refine, RefineStatus::Failed { ref code, .. } if code == "keys.async_refused")
        })
    })
    .await;

    // Draft line is intact.
    let chunk = app.transcript(rec_id, Some(0), 0).await;
    assert_eq!(chunk.lines.len(), 1);
    assert_eq!(chunk.lines[0].1.line.text, "draft survives refusal");

    // Observability: topic.context(where="refine") + refine.state(to="Failed", why="keys.async_refused").
    assert!(
        app.seen
            .of("topic.context")
            .iter()
            .any(|l| l.text("where") == Some("refine") && l.u64("topics") == Some(2))
    );
    assert!(app.seen.of("refine.state").iter().any(|l| {
        l.text("to") == Some("Failed") && l.text("why") == Some("keys.async_refused")
    }));
    assert_eq!(app.seen.refused(), 0);
}
