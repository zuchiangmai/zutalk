//! P6-01 tests: `talk/topic` knowledge model, pinned `always` topic, legacy `talk/term` lossless
//! migration, bounded UTF-8 file extraction, and sign-in-free commands/queries with safe events.

mod hub;

use hub::{App, STORE, nobody};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use zu::SecretKey;
use zutalk_core::app::proto::{Command, Query, QueryValue};
use zutalk_core::knowledge::glossary::{self, Term};
use zutalk_core::knowledge::topic::{
    self, ALWAYS_TOPIC_ID, TOPIC_IMPORT_BACKGROUND_CHARS, TOPIC_IMPORT_MAX_BYTES, TOPIC_MAX_CHARS,
    TOPIC_MAX_SUGGESTED_TERMS, Topic, TopicFact, TranslationPair,
};
use zutalk_core::record::shelf::Shelf;

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
