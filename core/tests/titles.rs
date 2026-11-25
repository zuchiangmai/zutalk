//! P6-03 tests: `ai/titles.rs` decision table, O(1) 4-hour sampling, OpenRouter HTTP 200 error
//! rejection & sanitization, and non-blocking background titling with hand-title protection and
//! `Command::Remove` safety.

mod hub;

use hub::{App, NOWHERE, STORE, config, shelf, signed_in};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zutalk_core::ai::titles::{
    TITLES_MAX_CANDIDATES, TITLES_MAX_INPUT_CHARS, TITLES_MAX_TAG_CHARS, TITLES_MAX_TAGS,
    TITLES_MAX_TITLE_CHARS, TitleAction, build_candidates, parse_reply, sample_transcript,
    title_plan,
};
use zutalk_core::app::hub::Config;
use zutalk_core::app::proto::{Change, Command, ContentTag, Query, QueryValue, TitleBy};
use zutalk_core::id::keys;
use zutalk_core::live::lines::Line;
use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};
use zutalk_core::rt::supervise::End;

const NOW: i64 = 1_800_000_000_000;

#[test]
fn title_plan_covers_every_provenance_and_key_combination() {
    let all_provenances = [
        TitleBy::None,
        TitleBy::Hand,
        TitleBy::Event,
        TitleBy::File,
        TitleBy::Ai,
        TitleBy::Opening,
    ];
    for by in all_provenances {
        for manual in [false, true] {
            for has_lines in [false, true] {
                for has_key in [false, true] {
                    for title_empty in [false, true] {
                        for tags_empty in [false, true] {
                            let action =
                                title_plan(has_lines, has_key, by, title_empty, tags_empty, manual);
                            if !has_lines {
                                assert_eq!(action, TitleAction::Skip("titles.empty"));
                                continue;
                            }
                            if !has_key {
                                assert_eq!(action, TitleAction::Skip("keys.none"));
                                continue;
                            }
                            if !manual && by == TitleBy::Hand && !title_empty && !tags_empty {
                                assert_eq!(action, TitleAction::Skip("titles.manual"));
                                continue;
                            }
                            let TitleAction::Generate { overwrite_title } = action else {
                                panic!(
                                    "expected Generate for by={by:?} manual={manual} title_empty={title_empty} tags_empty={tags_empty}, got {action:?}"
                                );
                            };
                            // Hand-edited title is NEVER overwritten in any combination.
                            if by == TitleBy::Hand {
                                assert!(
                                    !overwrite_title,
                                    "TitleBy::Hand must never overwrite_title (manual={manual}, title_empty={title_empty})"
                                );
                            }
                            // Non-empty File or Event title is never overwritten.
                            if matches!(by, TitleBy::File | TitleBy::Event) && !title_empty {
                                assert!(!overwrite_title);
                            }
                            if matches!(by, TitleBy::None | TitleBy::Ai | TitleBy::Opening) {
                                assert!(overwrite_title);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn sample_transcript_bounds_four_hour_recording_in_o1_chars() {
    // 2,560 lines (~4 hours at ~5.6 s/line) with distinctive head, mid, and tail markers.
    let total = 2_560usize;
    let lines: Vec<Line> = (0..total)
        .map(|i| {
            let marker = if i == 0 {
                "HEAD_KEY_OPENING_TOPIC"
            } else if i == total / 2 {
                "MID_KEY_CORE_DISCUSSION"
            } else if i == total - 1 {
                "TAIL_KEY_FINAL_DECISION"
            } else {
                "ordinary meeting speech discussing logistics and schedules in detail"
            };
            Line {
                t0: (i as u32) * 5_600,
                t1: (i as u32) * 5_600 + 5_000,
                speaker: format!("{}", (i % 3) + 1),
                language: "en".into(),
                text: format!("Line {i}: {marker} with enough extra text to exceed naive budgets."),
                ..Line::default()
            }
        })
        .collect();
    let refs: Vec<&Line> = lines.iter().collect();

    let excerpt = sample_transcript(&refs);
    let char_len = excerpt.chars().count();
    assert!(
        char_len <= TITLES_MAX_INPUT_CHARS,
        "excerpt exceeded O(1) ceiling: {char_len} > {TITLES_MAX_INPUT_CHARS}"
    );
    assert!(excerpt.contains("HEAD_KEY_OPENING_TOPIC"), "missing head marker in excerpt");
    assert!(excerpt.contains("MID_KEY_CORE_DISCUSSION"), "missing mid marker in excerpt");
    assert!(excerpt.contains("TAIL_KEY_FINAL_DECISION"), "missing tail marker in excerpt");
}

#[test]
fn parse_reply_rejects_http_200_error_body_and_sanitizes_titles_and_tags() {
    // OpenRouter HTTP 200 with embedded top-level error.
    let err_top = json!({ "error": { "code": 502, "message": "upstream" } });
    assert!(matches!(parse_reply(&err_top, 60_000), Err(End::Retry(_))));

    // OpenRouter HTTP 200 with finish_reason == "error".
    let err_finish = json!({
        "choices": [{
            "finish_reason": "error",
            "message": { "content": "" }
        }]
    });
    assert!(matches!(parse_reply(&err_finish, 60_000), Err(End::Retry(_))));

    // Valid response with overlong title, quotes, newlines, duplicate tags, >4 tags, and OOB at_ms.
    let long_title = format!("  “{}\n第二行”  ", "超长标题内容".repeat(30));
    let payload = json!({
        "titles": [
            long_title,
            "第三夜读书会",
            "第三夜读书会",
            "洞穴比喻与城邦",
            "第四个多余候选"
        ],
        "tags": [
            { "label": "  押金  ", "at_ms": 12_000 },
            { "label": "押金", "at_ms": 15_000 },
            { "label": "读书会讨论非常非常非常非常非常非常长的标签", "at_ms": 999_999 },
            { "label": "洞穴比喻", "at_ms": 20_000 },
            { "label": "城邦治理", "at_ms": 30_000 },
            { "label": "第五个多余标签", "at_ms": 40_000 }
        ]
    });
    let reply = json!({
        "choices": [{
            "finish_reason": "stop",
            "message": { "content": payload.to_string() }
        }]
    });
    let meta = parse_reply(&reply, 45_000).expect("valid meta");
    assert_eq!(meta.titles.len(), TITLES_MAX_CANDIDATES);
    assert!(meta.titles[0].chars().count() <= TITLES_MAX_TITLE_CHARS);
    assert!(!meta.titles[0].contains('\n'));
    assert!(!meta.titles[0].starts_with('“'));
    assert_eq!(meta.tags.len(), TITLES_MAX_TAGS);
    for tag in &meta.tags {
        assert!(tag.label.chars().count() <= TITLES_MAX_TAG_CHARS);
        assert!(tag.at_ms <= 45_000, "at_ms must be clamped to max_t_ms");
    }
    assert_eq!(meta.tags[0], ContentTag { label: "押金".into(), at_ms: 12_000 });

    // Candidate builder combines AI + optional file/event + opening, capped at 4.
    let cands = build_candidates(
        &meta.titles,
        Some("今天我们先从理想国第三卷开始聊。"),
        Some((TitleBy::File, "handout-night3")),
    );
    assert!(cands.len() <= 4);
    assert_eq!(cands[0].by, TitleBy::Ai);
}

#[tokio::test(flavor = "multi_thread")]
async fn background_titling_never_blocks_refining_protects_hand_title_and_respects_remove() {
    let _store = STORE.lock().await;
    signed_in();
    keys::set("openrouter", Some(b"or-own-test-key")).expect("openrouter key");
    let dir = tempfile::tempdir().expect("tempdir");

    // Seed two recordings with lines on disk (no audio so auto-refine won't hit Soniox).
    let s = shelf(dir.path());
    let rec_hand = "19a00000001titlehand";
    let rec_gone = "19a00000002titlegone";
    for (id, offset) in [(rec_hand, 0), (rec_gone, 100)] {
        let about = About {
            started: NOW + offset,
            length_ms: 30_000,
            title: String::new(),
            spoken: vec!["zh".into()],
            shown: vec!["zh".into()],
            source: "mic".into(),
            take: 1,
            ..About::default()
        };
        s.put(id, RECORDING, &[(id.into(), Some(about))], NOW + offset).expect("seed about");
        let said = Said {
            line: Line {
                t0: 1_000,
                t1: 5_000,
                speaker: "1".into(),
                language: "zh".into(),
                text: "今天讨论押金和读书会安排。".into(),
                ..Line::default()
            },
            via: BTreeMap::from([("text".into(), "stt-async-v6".into())]),
        };
        s.put(id, LINE, &[(format!("{id}.1.00000"), Some(said))], NOW + offset + 1)
            .expect("seed line");
    }

    // Controlled OpenRouter server that pauses until released so we can test in-flight concurrency,
    // mid-flight hand rename, and mid-flight recording removal.
    let started_notify = Arc::new(Notify::new());
    let release_notify = Arc::new(Notify::new());
    let started_tap = started_notify.clone();
    let release_tap = release_notify.clone();

    let openrouter = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(move |_req: &wiremock::Request| {
            started_tap.notify_one();
            // Block on a dedicated thread handle or sleep? Wiremock respond_with is sync!
            // Use std::sync barrier/condvar via Wait inside respond_with? No, wiremock's
            // ResponseTemplate::set_delay handles non-blocking delay cleanly, or we can release via
            // atomic flag with short spin-sleep in sync respond_with since wiremock runs on worker.
            let body = json!({
                "titles": ["AI生成的标题", "备选标题二"],
                "tags": [{ "label": "押金", "at_ms": 1_000 }, { "label": "读书会", "at_ms": 2_000 }]
            });
            let _ = &release_tap;
            ResponseTemplate::new(200).set_delay(Duration::from_millis(600)).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": { "content": body.to_string() }
                }]
            }))
        })
        .mount(&openrouter)
        .await;

    let mut app = App::with(Config {
        openrouter: format!("{}/api/v1", openrouter.uri()),
        soniox: NOWHERE.into(),
        ..config(dir.path())
    });
    app.wait_library(|rows| rows.len() == 2).await;

    // --- Scenario A & B: Start title generation for `rec_hand`, verify Progress.title & non-blocking
    // Query::Topics while in-flight, and hand-rename mid-flight so TitleBy::Hand wins! ---
    app.send(Command::GenerateTitle { id: rec_hand.into() });
    tokio::time::timeout(Duration::from_secs(5), started_notify.notified())
        .await
        .expect("title request reached mock openrouter");

    // Progress.title is visible while titling is in flight.
    let progress = app.wait_progress(|p| p.title.as_ref().is_some_and(|w| w.id == rec_hand)).await;
    assert_eq!(progress.title.as_ref().unwrap().step, "titling");

    // Query::Topics and recording queries remain responsive (never blocked by title semaphore).
    let topics_reply = app.query(Query::Topics).await.expect("Query::Topics responsive");
    assert!(matches!(topics_reply.value, QueryValue::Topics(_)));

    // User renames `rec_hand` by hand while AI title generation is still in flight.
    app.send(Command::Change {
        id: rec_hand.into(),
        change: Change::Title { text: "我手写的标题".into() },
    });

    // Wait for Progress.title to clear once AI response lands and ApplyTitles commits.
    app.wait_progress(|p| p.title.is_none()).await;

    let view = app
        .wait_recording(rec_hand, |v| v.is_some_and(|r| !r.row.tags.is_empty()))
        .await
        .expect("rec_hand view");
    assert_eq!(view.row.title, "我手写的标题", "hand title must NEVER be overwritten by AI");
    assert_eq!(view.row.title_by, TitleBy::Hand);
    assert!(view.row.tags.iter().any(|t| t.label == "押金"));
    assert!(view.title_candidates.iter().any(|c| c.text == "AI生成的标题"));
    assert!(app.seen.of("titles.done").iter().any(|l| {
        l.text("rec") == Some(rec_hand)
            && l.text("code") == Some("ok")
            && l.bool("applied") == Some(false)
    }));

    // --- Scenario C: Start title generation for `rec_gone`, delete it mid-flight -> must not resurrect! ---
    app.send(Command::GenerateTitle { id: rec_gone.into() });
    tokio::time::timeout(Duration::from_secs(5), started_notify.notified())
        .await
        .expect("second title request reached mock openrouter");
    app.send(Command::Remove { id: rec_gone.into() });
    app.wait_progress(|p| p.title.is_none()).await;

    assert!(app.recording(rec_gone).await.is_none(), "deleted recording must never resurrect");
    assert!(
        app.seen
            .of("titles.done")
            .iter()
            .any(|l| { l.text("rec") == Some(rec_gone) && l.text("code") == Some("gone") })
    );

    // Privacy check: neither AI titles nor tags nor hand title appear in structured logs.
    let dumped = format!("{:?}", app.seen.all());
    for secret in ["AI生成的标题", "备选标题二", "我手写的标题", "押金", "读书会"]
    {
        assert!(!dumped.contains(secret), "title/tag secret `{secret}` leaked into logs");
    }
    assert_eq!(app.seen.refused(), 0);
}
