mod hub;

use hub::{
    App, Asked, ME, NOWHERE, STORE, config, config_on, shelf, signed_in, soniox, store, tape,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zu::{Pack, SecretKey};
use zutalk_core::ai::write::{self, Journal};
use zutalk_core::app::hub::Config;
use zutalk_core::app::proto::Command;
use zutalk_core::id::keys::From;
use zutalk_core::live::lines::Line;
use zutalk_core::record::shelf::{About, Draft, DraftWriter, LINE, RECORDING, Said, Shelf};
use zutalk_core::record::takes::{
    self, RefineEvent, RefineFailure, RefineStatus, TakeInfo, initial_refine_status, refine_step,
};

const ID: &str = "kx7q2m5v4tbn3hpw";
const NOW: i64 = 1_800_000_000_000;

fn make_shelf(dir: &Path) -> Shelf {
    Shelf::new(dir, ME, Arc::new(SecretKey::from_seed(&[5; 32])))
}

fn said(t0: u32, text: &str) -> Said {
    Said {
        line: Line {
            t0,
            t1: t0 + 1_000,
            speaker: "1".into(),
            language: "en".into(),
            text: text.into(),
            translations: BTreeMap::new(),
            incomplete: false,
        },
        via: BTreeMap::from([("text".into(), "stt".into())]),
    }
}

fn write_take(shelf: &Shelf, id: &str, take: u32, texts: &[&str], at: i64) {
    let entries: Vec<(String, Option<Said>)> = texts
        .iter()
        .enumerate()
        .map(|(n, text)| (format!("{id}.{take}.{n:05}"), Some(said(n as u32 * 1_000, text))))
        .collect();
    shelf.put(id, LINE, &entries, at).expect("write take lines");
}

#[test]
fn takes_list_and_read_preserves_draft_alongside_refined_takes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = make_shelf(dir.path());
    shelf
        .put(
            ID,
            RECORDING,
            &[(ID.to_string(), Some(About { started: NOW, take: 2, ..About::default() }))],
            NOW,
        )
        .expect("write about");

    write_take(&shelf, ID, 0, &["live one", "live two"], NOW + 1_000);
    write_take(&shelf, ID, 1, &["refined once"], NOW + 2_000);
    write_take(
        &shelf,
        ID,
        2,
        &["refined twice a", "refined twice b", "refined twice c"],
        NOW + 3_000,
    );

    assert_eq!(takes::parse_key(ID, &format!("{ID}.0.00001")), Some((0, 1)));
    assert_eq!(takes::parse_key(ID, &format!("{ID}.12.00042")), Some((12, 42)));
    assert_eq!(takes::parse_key(ID, "other.0.00001"), None);
    assert_eq!(takes::parse_key(ID, &format!("{ID}.bad.00001")), None);

    let list = takes::list(&shelf, ID, 2).expect("list takes");
    assert_eq!(
        list,
        vec![
            TakeInfo {
                take: 0,
                lines: 2,
                is_live: true,
                is_current: false,
                written_at: Some(NOW + 1_000),
            },
            TakeInfo {
                take: 1,
                lines: 1,
                is_live: false,
                is_current: false,
                written_at: Some(NOW + 2_000),
            },
            TakeInfo {
                take: 2,
                lines: 3,
                is_live: false,
                is_current: true,
                written_at: Some(NOW + 3_000),
            },
        ]
    );

    let t0: Vec<String> = takes::read(&shelf, ID, 0)
        .expect("read take 0")
        .into_iter()
        .map(|(_, s)| s.line.text)
        .collect();
    let t1: Vec<String> = takes::read(&shelf, ID, 1)
        .expect("read take 1")
        .into_iter()
        .map(|(_, s)| s.line.text)
        .collect();
    let t2: Vec<String> = takes::read(&shelf, ID, 2)
        .expect("read take 2")
        .into_iter()
        .map(|(_, s)| s.line.text)
        .collect();
    assert_eq!(t0, vec!["live one", "live two"]);
    assert_eq!(t1, vec!["refined once"]);
    assert_eq!(t2, vec!["refined twice a", "refined twice b", "refined twice c"]);
    assert!(takes::read(&shelf, ID, 9).expect("missing take").is_empty());

    let all = takes::read_all(&shelf, ID, 2).expect("read all takes in one pass");
    assert_eq!(all.infos, list);
    assert_eq!(
        all.by_take.get(&0).unwrap().iter().map(|(_, s)| s.line.text.clone()).collect::<Vec<_>>(),
        t0
    );
    assert_eq!(
        all.by_take.get(&1).unwrap().iter().map(|(_, s)| s.line.text.clone()).collect::<Vec<_>>(),
        t1
    );
    assert_eq!(
        all.by_take.get(&2).unwrap().iter().map(|(_, s)| s.line.text.clone()).collect::<Vec<_>>(),
        t2
    );
}

#[test]
fn refine_status_transition_table() {
    let draft = RefineStatus::Draft { why: Some("keys.none".into()) };
    let refining = RefineStatus::Refining { step: "closing".into(), done: 0, of: 0 };
    let refined = RefineStatus::Refined { take: 1 };
    let failed = RefineStatus::Failed { code: "keys.async_refused".into(), at: NOW };

    assert_eq!(draft.code(), "Draft");
    assert_eq!(refining.code(), "Refining");
    assert_eq!(refined.code(), "Refined");
    assert_eq!(failed.code(), "Failed");

    // Start enters Refining from every state (including re-refine from Refined and retry from Failed).
    for state in [&draft, &refining, &refined, &failed] {
        assert_eq!(refine_step(state, RefineEvent::Start, NOW + 1), refining);
    }

    // Progress updates only Refining.
    let prog = RefineEvent::Progress { step: "translating", done: 3, of: 10 };
    assert_eq!(
        refine_step(&refining, prog, NOW + 2),
        RefineStatus::Refining { step: "translating".into(), done: 3, of: 10 }
    );
    assert_eq!(refine_step(&draft, prog, NOW + 2), draft);
    assert_eq!(refine_step(&refined, prog, NOW + 2), refined);
    assert_eq!(refine_step(&failed, prog, NOW + 2), failed);

    // Succeeded transitions Refining -> Refined; ignored elsewhere.
    let ok = RefineEvent::Succeeded { take: 2 };
    assert_eq!(refine_step(&refining, ok, NOW + 3), RefineStatus::Refined { take: 2 });
    assert_eq!(refine_step(&draft, ok, NOW + 3), draft);
    assert_eq!(refine_step(&refined, ok, NOW + 3), refined);
    assert_eq!(refine_step(&failed, ok, NOW + 3), failed);

    // Failed transitions Refining -> Failed; ignored elsewhere.
    let err = RefineEvent::Failed { code: "keys.async_refused" };
    assert_eq!(
        refine_step(&refining, err, NOW + 4),
        RefineStatus::Failed { code: "keys.async_refused".into(), at: NOW + 4 }
    );
    assert_eq!(refine_step(&draft, err, NOW + 4), draft);
    assert_eq!(refine_step(&refined, err, NOW + 4), refined);
    assert_eq!(refine_step(&failed, err, NOW + 4), failed);

    // Cleared resets Failed or Draft -> Draft { why }; ignored for Refining and Refined.
    let cleared = RefineEvent::Cleared { why: None };
    assert_eq!(refine_step(&failed, cleared, NOW + 5), RefineStatus::Draft { why: None });
    assert_eq!(refine_step(&draft, cleared, NOW + 5), RefineStatus::Draft { why: None });
    assert_eq!(refine_step(&refining, cleared, NOW + 5), refining);
    assert_eq!(refine_step(&refined, cleared, NOW + 5), refined);

    // initial_refine_status table.
    let fail_rec = RefineFailure { code: "keys.async_refused".into(), at: NOW };
    assert_eq!(
        initial_refine_status(1, false, false, Some(&fail_rec)),
        RefineStatus::Refined { take: 1 }
    );
    assert_eq!(
        initial_refine_status(0, true, true, Some(&fail_rec)),
        RefineStatus::Failed { code: "keys.async_refused".into(), at: NOW }
    );
    assert_eq!(
        initial_refine_status(0, false, true, None),
        RefineStatus::Draft { why: Some("tape.none".into()) }
    );
    assert_eq!(
        initial_refine_status(0, true, false, None),
        RefineStatus::Draft { why: Some("keys.none".into()) }
    );
    assert_eq!(initial_refine_status(0, true, true, None), RefineStatus::Draft { why: None });

    // DECISIONS.md #4 error mapping.
    assert_eq!(write::refine_error_code("key-refused", From::Granted), "keys.async_refused");
    assert_eq!(write::refine_error_code("key-refused", From::Own), "key-refused");
    assert_eq!(write::refine_error_code("out-of-credit", From::Granted), "out-of-credit");
}

#[test]
fn draft_writer_keeps_o1_state_over_ten_thousand_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = make_shelf(dir.path());
    let mut writer = shelf.open_draft(ID).expect("open draft writer");
    // Struct holds only Pack + DID + key + last_at (no per-path heads BTreeMap).
    assert!(std::mem::size_of::<DraftWriter>() < 256);

    for n in 0..10_000u32 {
        let key = format!("{ID}.0.{n:05}");
        let line = said(n * 100, "draft line");
        // Even with a stationary clock, `last_at` guarantees monotonic record timestamps.
        writer.put(&key, &line, NOW).expect("write draft line");
    }
    writer.sync().expect("sync draft");
    drop(writer);

    let lines = takes::read(&shelf, ID, 0).expect("read 10k draft lines");
    assert_eq!(lines.len(), 10_000);
    assert_eq!(lines[0].0, format!("{ID}.0.00000"));
    assert_eq!(lines[9_999].0, format!("{ID}.0.09999"));

    let held = Pack::read(dir.path().join(ID).join("lines.jsonl")).expect("read raw pack");
    assert_eq!(held.records.len(), 10_000);
    assert!(held.records.iter().all(|r| r.prev.is_empty()));
    assert!(held.records.windows(2).all(|w| w[1].at > w[0].at));
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_refine_rejects_missing_audio_key_and_live_recording() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    // Unknown recording -> recording.gone.
    app.send(Command::Refine { id: "does-not-exist".into() });
    app.notice("recording.gone").await;

    // Live recording -> recording.under_way.
    app.send(Command::Start);
    let session = app.session(|s| s.is_some()).await.expect("session started");
    let live_id = session.id.clone();
    app.wait_library(|rows| rows.iter().any(|r| r.id == live_id)).await;
    app.send(Command::Refine { id: live_id.clone() });
    app.notice("recording.under_way").await;

    // Wait for at least one audio block so sealing keeps the recording and its audio.
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Stop recording: without a key it stays draft with audio -> Refine returns keys.none.
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    app.wait_library(|rows| rows.iter().any(|r| r.id == live_id && r.audio && r.length_ms > 0))
        .await;
    app.wait_progress(|p| p.refine.is_none()).await;
    app.send(Command::Refine { id: live_id.clone() });
    app.notice("keys.none").await;

    // Destroy audio -> Refine returns tape.none (checked before key check).
    app.send(Command::DestroyAudio { id: live_id.clone() });
    app.wait_library(|rows| rows.iter().any(|r| r.id == live_id && !r.audio)).await;
    app.send(Command::Refine { id: live_id });
    app.notice("tape.none").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_refine_triggers_on_draft_retries_after_failure_and_re_refines() {
    let _store = STORE.lock().await;
    // Signed in with NO own Soniox key => Soniox key comes from granted quota (`From::Granted`).
    store(true);
    let dir = tempfile::tempdir().expect("tempdir");

    // Seed a draft recording with take 0 lines and real encrypted tape.
    let s = shelf(dir.path());
    let about = About {
        started: NOW,
        length_ms: 2_000,
        title: "Refine test".into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        take: 0,
        ..About::default()
    };
    s.put(ID, RECORDING, &[(ID.to_string(), Some(about))], NOW).expect("seed about");
    write_take(&s, ID, 0, &["live draft line"], NOW + 1);
    tape(dir.path(), ID, 2);

    // Stand-in ZuTalk server granting a short-lived Soniox key.
    let our_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/lease"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "key": "sk-granted", "left": 5_000_000 })),
        )
        .mount(&our_server)
        .await;

    // Stand-in Soniox server rejecting the granted key with 401 on async upload.
    let refusing_soniox = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/files"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&refusing_soniox)
        .await;

    let cfg = Config {
        soniox: format!("{}/v1", refusing_soniox.uri()),
        openrouter: NOWHERE.into(),
        ..config_on(dir.path(), &our_server.uri())
    };
    let mut app = App::with(cfg);

    // Boot survey auto-attempts refine, hits 401 on granted key -> persists keys.async_refused.
    app.notice("keys.async_refused").await;
    app.wait_progress(|p| p.refine.is_none()).await;
    app.wait_recording(ID, |r| {
        r.is_some_and(|v| {
            matches!(v.refine, RefineStatus::Failed { ref code, .. } if code == "keys.async_refused")
        })
    })
    .await;

    let tape_dir = dir.path().join("tape").join(ID);
    let journal = Journal::load(&tape_dir).expect("load journal");
    assert_eq!(journal.failed.as_ref().map(|f| f.code.as_str()), Some("keys.async_refused"));
    assert!(
        app.seen.of("refine.state").iter().any(|l| {
            l.text("to") == Some("Failed") && l.text("why") == Some("keys.async_refused")
        }),
        "expected refine.state Failed(keys.async_refused) event, got {:?}",
        app.seen.of("refine.state")
    );
    drop(app);

    // Restarting with the failed journal must NOT blindly retry the refused draft on boot.
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let good_soniox = soniox(&asked).await;
    let cfg2 = Config {
        soniox: format!("{}/v1", good_soniox.uri()),
        openrouter: NOWHERE.into(),
        ..config_on(dir.path(), &our_server.uri())
    };
    let mut app2 = App::with(cfg2);
    app2.wait_library(|rows| rows.iter().any(|r| r.id == ID)).await;
    assert!(app2.outlet.progress.now().1.refine.is_none());
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(asked.lock().expect("asked").is_empty(), "boot survey should skip failed draft");

    // Entering an own Soniox key clears keys.async_refused and triggers automatic refine -> take 1.
    app2.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-own-working".into()) });
    app2.wait_library(|rows| rows.iter().any(|r| r.id == ID && !r.draft)).await;
    app2.wait_progress(|p| p.refine.is_none()).await;

    assert!(!tape_dir.join(Journal::FILE).exists(), "write.json cleared on success");
    assert_eq!(s.about(ID).expect("about").expect("exists").about.take, 1);
    assert!(
        app2.seen
            .of("refine.state")
            .iter()
            .any(|l| { l.text("to") == Some("Refined") && l.u64("take") == Some(1) }),
        "expected refine.state Refined(take=1), got {:?}",
        app2.seen.of("refine.state")
    );

    // Manual re-refine on already-refined take 1 produces take 2; takes 0, 1, 2 all readable.
    app2.send(Command::Refine { id: ID.into() });
    app2.wait_recording(ID, |r| r.is_some_and(|v| v.take == 2)).await;
    app2.wait_progress(|p| p.refine.is_none()).await;
    assert_eq!(s.about(ID).expect("about").expect("exists").about.take, 2);

    let takes_list = takes::list(&s, ID, 2).expect("list takes");
    assert_eq!(takes_list.iter().map(|t| t.take).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(takes::read(&s, ID, 0).expect("take 0").len(), 1);
    assert_eq!(takes::read(&s, ID, 1).expect("take 1").len(), 1);
    assert_eq!(takes::read(&s, ID, 2).expect("take 2").len(), 1);

    // Clean up helper import warning if unused.
    let _ = (config, signed_in);
}
