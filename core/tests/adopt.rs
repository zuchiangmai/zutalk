//! P4-03 tests: adopting device-local (`did:key`) records & glossary into the first signed-in
//! person's `did:zu` shelf, single-person binding in `adopted.json`, post-seal adoption of an
//! in-progress local recording, explicit `Command::DiscardLocal`, and zero privacy leakage.

mod hub;

use hub::{App, ME, NOWHERE, STORE, config_on, local_did, local_shelf, store, tape};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zu::{Pack, SecretKey};
use zutalk_core::app::hub::Config;
use zutalk_core::app::proto::{Command, LocalShelf, Query, QueryValue};
use zutalk_core::capture::clock::wall_ms;
use zutalk_core::capture::tape::{Pass, kept as tape_kept};
use zutalk_core::id::keys;
use zutalk_core::knowledge::glossary::{self, Term};
use zutalk_core::knowledge::topic::{self, ALWAYS_TOPIC_ID, Topic, TopicFact, TranslationPair};
use zutalk_core::live::lines::Line;
use zutalk_core::record::adopt::{self, AdoptPlan, Adopted};
use zutalk_core::record::notes;
use zutalk_core::record::shelf::{About, LINE, NOTE, RECORDING, SPEAKER, Said, Shelf, Words};
use zutalk_core::record::takes;

fn dir_snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(cur) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(bytes) = fs::read(&p) {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.insert(rel, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn adopt_plan_table() {
    let local = "did:key:z6Mklocalsample";
    let person_a = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa";
    let person_b = "did:zu:bbbbbbbbbbbbbbbbbbbbbbbb";

    // No marker: adopts iff local data exists.
    assert_eq!(adopt::plan(None, local, person_a, true), AdoptPlan::Adopt);
    assert_eq!(adopt::plan(None, local, person_a, false), AdoptPlan::Nothing);

    // Marker bound to person A: same person adopts (idempotent catch-up) when local data exists;
    // second distinct person is always refused with OtherPerson.
    let marker_a = Adopted {
        local: local.into(),
        to: person_a.into(),
        at: 1000,
        recordings: 2,
        terms: 1,
        topics: 0,
    };
    assert_eq!(adopt::plan(Some(&marker_a), local, person_a, true), AdoptPlan::Adopt);
    assert_eq!(adopt::plan(Some(&marker_a), local, person_a, false), AdoptPlan::Nothing);
    assert_eq!(adopt::plan(Some(&marker_a), local, person_b, true), AdoptPlan::OtherPerson);
    assert_eq!(adopt::plan(Some(&marker_a), local, person_b, false), AdoptPlan::OtherPerson);

    // Marker for a different local DID does not block a brand-new local DID.
    assert_eq!(adopt::plan(Some(&marker_a), "did:key:z6Mkother", person_b, true), AdoptPlan::Adopt);
}

#[test]
fn can_discard_table() {
    let local = "did:key:z6Mklocalsample";
    let person_a = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa";
    let person_b = "did:zu:bbbbbbbbbbbbbbbbbbbbbbbb";
    let marker_a = Adopted {
        local: local.into(),
        to: person_a.into(),
        at: 1000,
        recordings: 1,
        terms: 0,
        topics: 0,
    };
    let marker_other_local = Adopted {
        local: "did:key:z6Mkother".into(),
        to: person_a.into(),
        at: 1000,
        recordings: 1,
        terms: 0,
        topics: 0,
    };

    // Idempotent cleanup when no local recordings or terms remain on disk.
    assert!(adopt::can_discard(None, local, person_a, false));
    assert!(adopt::can_discard(Some(&marker_a), local, person_a, false));
    assert!(adopt::can_discard(Some(&marker_a), local, person_b, false));
    assert!(adopt::can_discard(Some(&marker_other_local), local, person_a, false));

    // When local data exists, only the person bound to this exact local DID may discard it.
    assert!(!adopt::can_discard(None, local, person_a, true));
    assert!(adopt::can_discard(Some(&marker_a), local, person_a, true));
    assert!(!adopt::can_discard(Some(&marker_a), local, person_b, true));
    assert!(!adopt::can_discard(Some(&marker_other_local), local, person_a, true));
}

#[tokio::test(flavor = "multi_thread")]
async fn first_sign_in_adopts_all_takes_notes_speakers_terms_and_keeps_audio_and_local_intact() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("tempdir");
    let now = 1_764_000_000_000i64;

    let local = local_shelf(dir.path());
    let rec_id = "19a00000001aabbccdd";
    let about = About {
        started: now,
        length_ms: 4_000,
        title: "Secret Local Meeting".into(),
        preview: "refined line".into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        source: "mic".into(),
        take: 1,
        moved: false,
    };

    // Write take 0 (live draft) and take 1 (refined) lines.
    let said0 = Said {
        line: Line {
            t0: 0,
            t1: 1_000,
            speaker: "1".into(),
            language: "en".into(),
            text: "draft hello".into(),
            ..Line::default()
        },
        via: BTreeMap::from([("text".into(), "stt-rt-v6".into())]),
    };
    let said1 = Said {
        line: Line {
            t0: 0,
            t1: 1_200,
            speaker: "1".into(),
            language: "en".into(),
            text: "refined hello".into(),
            ..Line::default()
        },
        via: BTreeMap::from([("text".into(), "stt-async-v6".into())]),
    };
    local.put(rec_id, LINE, &[(format!("{rec_id}.0.00000"), Some(said0))], now).unwrap();
    local.put(rec_id, LINE, &[(format!("{rec_id}.1.00000"), Some(said1))], now + 1).unwrap();

    // Speakers, legacy single-string note, timestamped note, and bare anchored mark.
    local
        .put(rec_id, SPEAKER, &[(format!("{rec_id}.1"), Some(Words { text: "Host".into() }))], now)
        .unwrap();
    local
        .put(rec_id, NOTE, &[(rec_id.into(), Some(Words { text: "Legacy summary".into() }))], now)
        .unwrap();
    notes::add(
        &local,
        rec_id,
        500,
        "Follow up note".into(),
        Some(format!("{rec_id}.1.00000")),
        0x11,
        now + 2,
    )
    .unwrap();
    notes::add(
        &local,
        rec_id,
        900,
        String::new(),
        Some(format!("{rec_id}.1.00000")),
        0x22,
        now + 3,
    )
    .unwrap();
    local.put(rec_id, RECORDING, &[(rec_id.into(), Some(about.clone()))], now + 4).unwrap();

    // Glossary terms and encrypted tape.
    glossary::put(&local, "Zuzalu", Some(BTreeMap::from([("zh".into(), "祖扎鲁".into())])), now)
        .unwrap();
    glossary::put(&local, "ZuTalk", Some(BTreeMap::new()), now + 1).unwrap();
    tape(dir.path(), rec_id, 2);

    let local_before = dir_snapshot(local.root());
    assert!(!local_before.is_empty());

    // Adopt into Person A.
    let key_a = Arc::new(SecretKey::from_seed(&[11u8; 32]));
    let did_a = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa";
    let person_a =
        Shelf::new(dir.path().join("records").join(did_a.replace(':', "-")), did_a, key_a.clone());

    let sum = adopt::run(dir.path(), &local, &person_a, &[], now + 10).expect("adopt A");
    assert_eq!(sum.plan, AdoptPlan::Adopt);
    assert_eq!(sum.recordings, 1);
    assert_eq!(sum.terms, 2);
    assert_eq!(sum.adopted_ids, vec![rec_id.to_owned()]);

    // Verify every adopted record is signed by Person A and passes signature verification.
    let pub_a = key_a.public();
    for pack_path in [
        person_a.pack_path(rec_id, RECORDING),
        person_a.pack_path(rec_id, LINE),
        person_a.pack_path("", zutalk_core::record::shelf::TERM),
    ] {
        let held = Pack::read(&pack_path).expect("read pack");
        assert!(!held.records.is_empty());
        for record in &held.records {
            assert_eq!(record.path.by, did_a);
            assert_eq!(record.verify(|by, key, _| by == did_a && *key == pub_a, |_| None), Ok(()));
        }
    }

    // Both take 0 and take 1 lines, speakers, all 3 notes, and both terms arrived intact.
    let rec_a = person_a.read(rec_id).unwrap().expect("adopted recording");
    assert_eq!(rec_a.about.title, "Secret Local Meeting");
    assert_eq!(rec_a.speakers.get("1").map(String::as_str), Some("Host"));
    assert_eq!(rec_a.note, "Legacy summary");
    assert_eq!(rec_a.notes.len(), 3, "legacy + timestamped + bare mark: {:?}", rec_a.notes);
    assert!(rec_a.notes.iter().any(|n| n.text == "Follow up note"));
    assert!(rec_a.notes.iter().any(|n| n.text.is_empty() && n.at_ms == 900));

    let take_list = takes::list(&person_a, rec_id, rec_a.about.take).unwrap();
    assert_eq!(take_list.len(), 2, "both takes 0 and 1 must be adopted");
    assert_eq!(takes::read(&person_a, rec_id, 0).unwrap()[0].1.line.text, "draft hello");
    assert_eq!(takes::read(&person_a, rec_id, 1).unwrap()[0].1.line.text, "refined hello");

    let terms_a = glossary::read(&person_a).unwrap();
    assert_eq!(terms_a.len(), 2);
    assert!(terms_a.iter().any(|t| t.said == "Zuzalu"));
    assert!(terms_a.iter().any(|t| t.said == "ZuTalk"));

    // Encrypted tape is untouched and directly readable via tape::Pass.
    let tape_dir = dir.path().join("tape").join(rec_id);
    assert!(tape_kept(&tape_dir));
    let tape_key = keys::derive("tape", rec_id).unwrap();
    let mut blocks = 0u32;
    let duration_ms = zutalk_core::capture::tape::read(&tape_dir, &tape_key, &Pass::Export, |_| {
        blocks += 1;
    })
    .expect("read audio through Pass");
    assert!(blocks > 0 && duration_ms > 0);

    // Local shelf bytes are 100% untouched.
    assert_eq!(dir_snapshot(local.root()), local_before);

    // Re-running adopt for Person A is idempotent (0 new recordings, 0 new terms).
    let again = adopt::run(dir.path(), &local, &person_a, &[], now + 20).expect("idempotent A");
    assert_eq!(again.recordings, 0);
    assert_eq!(again.terms, 0);

    // Even if Person A deletes the recording, re-running adopt must NOT resurrect it.
    person_a.put::<About>(rec_id, RECORDING, &[(rec_id.into(), None)], now + 25).unwrap();
    assert!(person_a.about(rec_id).unwrap().is_none());
    let after_delete =
        adopt::run(dir.path(), &local, &person_a, &[], now + 30).expect("after delete");
    assert_eq!(after_delete.recordings, 0);
    assert!(person_a.about(rec_id).unwrap().is_none(), "deleted recording must not resurrect");

    // Second distinct Person B is refused by adopted.json binding.
    let key_b = Arc::new(SecretKey::from_seed(&[12u8; 32]));
    let did_b = "did:zu:bbbbbbbbbbbbbbbbbbbbbbbb";
    let person_b =
        Shelf::new(dir.path().join("records").join(did_b.replace(':', "-")), did_b, key_b);
    let sum_b = adopt::run(dir.path(), &local, &person_b, &[], now + 40).expect("refuse B");
    assert_eq!(sum_b.plan, AdoptPlan::OtherPerson);
    assert_eq!(sum_b.recordings, 0);
    assert_eq!(sum_b.terms, 0);
    assert!(person_b.list().unwrap().is_empty());
    assert!(glossary::read(&person_b).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn discard_local_clears_backup_without_touching_adopted_records_or_tape() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("tempdir");
    let now = wall_ms() as i64;
    let rec_id = "19a00000002feedbeef";
    let local = local_shelf(dir.path());
    let about = About {
        started: now,
        length_ms: 2_000,
        title: "Keep Me Private Title".into(),
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    local.put(rec_id, RECORDING, &[(rec_id.into(), Some(about))], now).unwrap();
    notes::add(&local, rec_id, 100, "Private note body".into(), None, 1, now + 1).unwrap();
    tape(dir.path(), rec_id, 1);

    // Boot once unsigned so the local index exists on disk too.
    let app_local = App::start(dir.path());
    let local_rows = app_local.wait_library(|r| r.len() == 1).await;
    assert_eq!(local_rows[0].id, rec_id);
    drop(app_local);

    let local_dir_name = local_did().replace(':', "-");
    let local_records = dir.path().join("records").join(&local_dir_name);
    let local_index = dir.path().join("index").join(format!("{local_dir_name}.sqlite3"));
    assert!(local_records.is_dir());
    assert!(local_index.is_file());

    // Sign in as ME -> adopts rec_id and exposes local_shelf { recordings: 1, adopted: true }.
    let kept = [&[5u8; 32][..], ME.as_bytes()].concat();
    keys::set("identity", Some(&kept)).unwrap();
    let mut app = App::start(dir.path());
    let state = app.until(|s| s.me.as_deref() == Some(ME)).await;
    assert_eq!(state.local_shelf, LocalShelf { recordings: 1, adopted: true });
    let rows = app.wait_library(|r| r.len() == 1).await;
    assert_eq!(rows[0].id, rec_id);

    // DiscardLocal is rejected with discard.busy while recording.
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    app.send(Command::DiscardLocal);
    app.notice("discard.busy").await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    // DiscardLocal removes local shelf and index, keeps adopted.json, tape/, and Person A's record.
    app.send(Command::DiscardLocal);
    app.notice("local.discarded").await;
    let cleared = app.until(|s| s.local_shelf.recordings == 0).await;
    assert_eq!(cleared.local_shelf, LocalShelf { recordings: 0, adopted: true });
    assert!(!local_records.exists(), "local shelf directory must be removed");
    assert!(!local_index.exists(), "local sqlite index must be removed");
    assert!(dir.path().join("adopted.json").is_file(), "adopted.json binding must remain");
    assert!(tape_kept(&dir.path().join("tape").join(rec_id)), "tape must remain");
    assert!(app.recording(rec_id).await.is_some(), "adopted recording must remain intact");

    // Structured observability and privacy checks.
    assert!(
        app.seen
            .of("identity.adopt")
            .iter()
            .any(|l| l.u64("recordings") == Some(1) && l.text("code") == Some("ok"))
    );
    assert!(app.seen.of("identity.discard_local").iter().any(|l| l.text("code") == Some("ok")));
    let dumped = format!("{:?}", app.seen.all());
    assert!(!dumped.contains("did:key:"), "did:key leaked in logs");
    assert!(!dumped.contains("did:zu:"), "did:zu leaked in logs");
    assert!(!dumped.contains("Keep Me Private Title"), "recording title leaked in logs");
    assert!(!dumped.contains("Private note body"), "note body leaked in logs");
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn in_progress_local_recording_finishes_under_local_then_adopts_on_seal() {
    let _store = STORE.lock().await;
    store(false);
    let directory = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&directory).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut app =
        App::with(Config { directory: directory.uri(), ..config_on(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    // Start recording while signed out (local did:key).
    app.send(Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("session").id;
    let local_me = keys::local_kept().unwrap().expect("local identity exists");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sign in mid-recording by creating a new did:zu.
    app.send(Command::CreateIdentity { handle: None });
    let signed = app.until(|s| s.me.is_some()).await;
    let person_did = signed.me.clone().expect("person did");
    assert!(person_did.starts_with("did:zu:"));
    assert!(
        app.library().await.is_empty(),
        "in-progress local recording must wait until seal before adopting"
    );

    tokio::time::sleep(Duration::from_millis(250)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    // Once sealed, the recording finishes under local did:key AND is auto-adopted into Person A.
    let rows = app.wait_library(|r| r.iter().any(|x| x.id == rec_id && x.length_ms > 0)).await;
    assert_eq!(rows[0].id, rec_id);

    // Verify local shelf holds the full recording signed by local_me (P1-06 invariant).
    let local_about_pack = dir
        .path()
        .join("records")
        .join(local_me.did.replace(':', "-"))
        .join(&rec_id)
        .join("about.jsonl");
    let held_local = Pack::read(&local_about_pack).expect("local about pack");
    assert_eq!(held_local.records.len(), 2, "start + sealed length on local shelf");
    for record in &held_local.records {
        assert_eq!(record.path.by, local_me.did);
    }

    // Query::Transcript works on Person A's adopted recording and audio is still on disk.
    let reply = app
        .core
        .query(Query::Transcript { id: rec_id.clone(), take: None, chunk: 0 })
        .await
        .expect("transcript query");
    assert!(matches!(reply.value, QueryValue::Transcript(_)));
    assert!(tape_kept(&dir.path().join("tape").join(&rec_id)));

    // Observability and privacy check.
    assert!(
        app.seen
            .of("identity.adopt")
            .iter()
            .any(|l| l.u64("recordings") == Some(1) && l.text("code") == Some("ok"))
    );
    let dumped = format!("{:?}", app.seen.all());
    assert!(!dumped.contains("did:key:"), "did:key leaked in logs");
    assert!(!dumped.contains("did:zu:"), "did:zu leaked in logs");
    assert_eq!(app.seen.refused(), 0);
}

async fn mock_soniox_with_words(words: &[&str]) -> MockServer {
    let server = MockServer::start().await;
    let reply = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    Mock::given(method("POST"))
        .and(path("/v1/files"))
        .respond_with(reply(json!({ "id": "file-adopt-1" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/transcriptions"))
        .respond_with(reply(json!({ "id": "tr-adopt-1" })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions/tr-adopt-1"))
        .respond_with(reply(json!({ "status": "completed" })))
        .mount(&server)
        .await;
    let tokens: Vec<Value> = words
        .iter()
        .enumerate()
        .map(|(i, text)| {
            json!({
                "text": text,
                "start_ms": i * 200,
                "end_ms": i * 200 + 180,
                "language": "en",
                "speaker": 1
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions/tr-adopt-1/transcript"))
        .respond_with(reply(json!({ "id": "tr-adopt-1", "tokens": tokens })))
        .mount(&server)
        .await;
    for kind in ["files", "transcriptions"] {
        Mock::given(method("GET"))
            .and(path(format!("/v1/{kind}")))
            .respond_with(reply(json!({ kind: [], "next_page_cursor": null })))
            .mount(&server)
            .await;
    }
    Mock::given(method("DELETE"))
        .and(path_regex("^/v1/"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn in_progress_local_recording_adopted_on_seal_refines_into_person_shelf() {
    let _store = STORE.lock().await;
    hub::nobody();
    let directory = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&directory).await;
    let soniox = mock_soniox_with_words(&[" Refined", " adopted", " transcript", "."]).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::with(Config {
        directory: directory.uri(),
        soniox: format!("{}/v1", soniox.uri()),
        ..config_on(dir.path(), NOWHERE)
    });
    app.until(|_| true).await;

    // Start recording while signed out (local did:key, own Soniox key configured).
    app.send(Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("session").id;
    let local_me = keys::local_kept().unwrap().expect("local identity exists");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sign in mid-recording as Person A.
    app.send(Command::CreateIdentity { handle: None });
    let signed = app.until(|s| s.me.is_some()).await;
    let person_did = signed.me.clone().expect("person did");
    assert!(person_did.starts_with("did:zu:"));

    tokio::time::sleep(Duration::from_millis(250)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    // Recording seals under local did:key, adopts into Person A, and refines directly onto
    // Person A's shelf (take == 1, draft == false). Wait for SQLite library sync so both
    // `recordings` and `take_lines` reflect take 1 atomically before reading `Query::Transcript`.
    app.wait_library(|rows| rows.iter().any(|r| r.id == rec_id && !r.draft)).await;
    let view = app
        .wait_recording(&rec_id, |v| v.is_some_and(|r| r.take == 1 && !r.row.draft))
        .await
        .expect("refined recording view");
    assert_eq!(view.take, 1);
    assert!(!view.row.draft);

    let chunk = app.transcript(&rec_id, None, 0).await;
    let joined: String =
        chunk.lines.iter().map(|(_, s)| s.line.text.as_str()).collect::<Vec<_>>().join(" ");
    assert!(
        joined.contains("Refined adopted transcript"),
        "expected refined transcript on Person A's shelf, got: {joined:?}"
    );

    // Local did:key shelf must retain the original sealed draft (take == 0, signed by local_did)
    // and never be overwritten by Person A's refinement.
    let local_dir = dir.path().join("records").join(local_me.did.replace(':', "-"));
    let local_about_pack = local_dir.join(&rec_id).join("about.jsonl");
    let held_local = Pack::read(&local_about_pack).expect("local about pack");
    assert_eq!(held_local.records.len(), 2, "only start + sealed draft on local shelf");
    for record in &held_local.records {
        assert_eq!(record.path.by, local_me.did);
    }
    let local_read = local_shelf(dir.path()).about(&rec_id).unwrap().expect("local about");
    assert_eq!(local_read.about.take, 0, "local shelf recording must stay at take == 0");

    // Person A's shelf holds the refined record signed by person_did.
    let person_about_pack = dir
        .path()
        .join("records")
        .join(person_did.replace(':', "-"))
        .join(&rec_id)
        .join("about.jsonl");
    let held_person = Pack::read(&person_about_pack).expect("person about pack");
    for record in &held_person.records {
        assert_eq!(record.path.by, person_did);
    }

    assert!(
        app.seen
            .of("identity.adopt")
            .iter()
            .any(|l| l.u64("recordings") == Some(1) && l.text("code") == Some("ok"))
    );
    assert!(
        app.seen
            .of("refine.state")
            .iter()
            .any(|l| l.text("to") == Some("Refined") && l.u64("take") == Some(1))
    );
    let dumped = format!("{:?}", app.seen.all());
    assert!(!dumped.contains("Refined adopted transcript"), "transcript leaked in logs");
    assert!(!dumped.contains("did:key:"), "did:key leaked in logs");
    assert!(!dumped.contains("did:zu:"), "did:zu leaked in logs");
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn second_person_cannot_discard_first_persons_local_backup() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("tempdir");
    let now = wall_ms() as i64;
    let rec_id = "19a00000003cafebabe";
    let local = local_shelf(dir.path());
    let about = About {
        started: now,
        length_ms: 2_000,
        title: "First Person Local Backup".into(),
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    local.put(rec_id, RECORDING, &[(rec_id.into(), Some(about))], now).unwrap();
    tape(dir.path(), rec_id, 1);

    // Boot unsigned so the local index exists on disk too.
    let app_local = App::start(dir.path());
    assert_eq!(app_local.wait_library(|r| r.len() == 1).await[0].id, rec_id);
    drop(app_local);

    let local_dir_name = local_did().replace(':', "-");
    let local_records = dir.path().join("records").join(&local_dir_name);
    let local_index = dir.path().join("index").join(format!("{local_dir_name}.sqlite3"));
    assert!(local_records.is_dir());
    assert!(local_index.is_file());

    // Person A signs in -> adopts rec_id and keeps the local backup.
    let kept_a = [&[5u8; 32][..], ME.as_bytes()].concat();
    keys::set("identity", Some(&kept_a)).unwrap();
    let mut app_a = App::start(dir.path());
    let state_a = app_a.until(|s| s.me.as_deref() == Some(ME)).await;
    assert_eq!(state_a.local_shelf, LocalShelf { recordings: 1, adopted: true });
    app_a.send(Command::SignOut);
    app_a.until(|s| s.me.is_none()).await;
    drop(app_a);

    // Person B signs in -> adopted.json belongs to Person A, so adopted == false.
    let person_b = "did:zu:bbbbbbbbbbbbbbbbbbbbbbbb";
    let kept_b = [&[9u8; 32][..], person_b.as_bytes()].concat();
    keys::set("identity", Some(&kept_b)).unwrap();
    let mut app_b = App::start(dir.path());
    let state_b = app_b.until(|s| s.me.as_deref() == Some(person_b)).await;
    assert_eq!(state_b.local_shelf, LocalShelf { recordings: 1, adopted: false });
    assert!(app_b.library().await.is_empty());

    // Person B attempting Command::DiscardLocal must be refused with adopt.failed and leave
    // Person A's local backup and index completely untouched.
    app_b.send(Command::DiscardLocal);
    app_b.notice("adopt.failed").await;

    assert!(
        app_b
            .seen
            .of("identity.discard_local")
            .iter()
            .any(|l| l.text("code") == Some("adopt.failed"))
    );
    assert!(local_records.is_dir(), "local shelf directory must remain intact");
    assert!(local_index.is_file(), "local sqlite index must remain intact");
    assert_eq!(local_shelf(dir.path()).list().unwrap().len(), 1);
    assert_eq!(app_b.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn adopt_resigns_local_topics_and_merges_always_without_losing_person_terms() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("tempdir");
    let now = 1_765_000_000_000i64;

    let local = local_shelf(dir.path());
    // Local device shelf has an `always` topic and a custom topic.
    topic::save(
        &local,
        Topic {
            id: ALWAYS_TOPIC_ID.into(),
            pinned: true,
            facts: vec![TopicFact { key: "City".into(), value: "Chiangmai".into() }],
            terms: vec![
                Term {
                    said: "LocalOnlyTerm".into(),
                    written: BTreeMap::from([("zh".into(), "本地词".into())]),
                },
                Term {
                    said: "SharedTerm".into(),
                    written: BTreeMap::from([("zh".into(), "本地版本".into())]),
                },
            ],
            translation_terms: vec![TranslationPair {
                source: "Agora".into(),
                target: "广场".into(),
            }],
            ..Topic::default()
        },
        now,
    )
    .expect("save local always");

    let local_custom = topic::save(
        &local,
        Topic {
            name: "Book Club".into(),
            domain: "Philosophy".into(),
            background: "Third night notes".into(),
            terms: vec![Term { said: "Polis".into(), written: BTreeMap::new() }],
            ..Topic::default()
        },
        now + 1,
    )
    .expect("save local custom");

    // Person A already has their own `always` topic with their own term + SharedTerm.
    let key_a = Arc::new(SecretKey::from_seed(&[21u8; 32]));
    let did_a = "did:zu:aaaaaaaaaaaaaaaaaaaaaaaa";
    let person_a =
        Shelf::new(dir.path().join("records").join(did_a.replace(':', "-")), did_a, key_a.clone());
    topic::save(
        &person_a,
        Topic {
            id: ALWAYS_TOPIC_ID.into(),
            pinned: true,
            terms: vec![
                Term {
                    said: "PersonOnlyTerm".into(),
                    written: BTreeMap::from([("zh".into(), "本人词".into())]),
                },
                Term {
                    said: "SharedTerm".into(),
                    written: BTreeMap::from([("zh".into(), "本人版本".into())]),
                },
            ],
            ..Topic::default()
        },
        now + 2,
    )
    .expect("save person always");

    // Run adoption: custom topic is re-signed onto Person A, and `always` is losslessly merged.
    let sum = adopt::run(dir.path(), &local, &person_a, &[], now + 10).expect("adopt topics");
    assert_eq!(sum.plan, AdoptPlan::Adopt);
    assert_eq!(sum.topics, 2);

    // Verify re-signed `topics.jsonl` records are signed by Person A.
    let pub_a = key_a.public();
    let held = Pack::read(person_a.pack_path("", zutalk_core::record::shelf::TOPIC))
        .expect("read person topics pack");
    assert!(!held.records.is_empty());
    for record in &held.records {
        assert_eq!(record.path.by, did_a);
        assert_eq!(record.verify(|by, key, _| by == did_a && *key == pub_a, |_| None), Ok(()));
    }

    let always_a = topic::get(&person_a, ALWAYS_TOPIC_ID).unwrap().expect("person always");
    assert!(always_a.terms.iter().any(|t| t.said == "PersonOnlyTerm"));
    assert!(always_a.terms.iter().any(|t| t.said == "LocalOnlyTerm"));
    // Person's existing SharedTerm is preserved (not overwritten by local).
    let shared = always_a.terms.iter().find(|t| t.said == "SharedTerm").expect("SharedTerm");
    assert_eq!(shared.written.get("zh").map(String::as_str), Some("本人版本"));
    assert!(always_a.facts.iter().any(|f| f.key == "City" && f.value == "Chiangmai"));
    assert!(always_a.translation_terms.iter().any(|p| p.source == "Agora" && p.target == "广场"));

    let custom_a = topic::get(&person_a, &local_custom.id).unwrap().expect("adopted custom topic");
    assert_eq!(custom_a.name, "Book Club");
    assert_eq!(custom_a.background, "Third night notes");

    // Second run for Person A is idempotent (0 new topics).
    let again = adopt::run(dir.path(), &local, &person_a, &[], now + 20).expect("idempotent");
    assert_eq!(again.topics, 0);

    // Second distinct Person B is refused with OtherPerson and receives no topics.
    let key_b = Arc::new(SecretKey::from_seed(&[22u8; 32]));
    let did_b = "did:zu:bbbbbbbbbbbbbbbbbbbbbbbb";
    let person_b =
        Shelf::new(dir.path().join("records").join(did_b.replace(':', "-")), did_b, key_b);
    let sum_b = adopt::run(dir.path(), &local, &person_b, &[], now + 30).expect("refuse B");
    assert_eq!(sum_b.plan, AdoptPlan::OtherPerson);
    assert_eq!(sum_b.topics, 0);
    assert!(topic::get(&person_b, &local_custom.id).unwrap().is_none());
}
