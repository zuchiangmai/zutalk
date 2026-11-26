mod common;
mod hub;

use common::{Plc, Viewer, old_library, serve, serve_with};
use futures_util::{SinkExt, StreamExt};
use hub::{App, ME, NOWHERE, STORE, config_on as config, store, wav};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use zutalk_core::app::hub::{Clip, Config, HUB_MAIL_MSGS, Install, Notices};
use zutalk_core::app::proto::{
    CaptionBg, CaptionScene, CaptionSize, Change, Command, Mode, NoteOp, Query, QueryValue,
    Session, Setup, State, Status, WindowRect,
};
use zutalk_core::capture::clock::wall_ms;
use zutalk_core::id::keys;
use zutalk_core::rt::queue::{self, Back, Full};
use zutalk_core::rt::stats::Stats;
use zutalk_server::Invites;

/// Names of identity directories under `data/records`, sorted.
fn record_dirs(data: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = fs::read_dir(data.join("records"))
        .expect("records dir")
        .filter_map(|e| {
            let e = e.ok()?;
            e.file_type().ok()?.is_dir().then(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    dirs.sort();
    dirs
}

/// Every file under `root` as relative path -> exact bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, into: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("a folder") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(base, &path, into);
            } else {
                let rel = path.strip_prefix(base).expect("relative").to_path_buf();
                into.insert(rel, fs::read(&path).expect("bytes"));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

/// Waits for `export.write` to finish (async off-hub export since P1-19).
async fn wait_export(app: &App) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !app
        .seen
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("export.write") && l.text("code") == Some("ok"))
    {
        assert!(tokio::time::Instant::now() < deadline, "export.write did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits for a `State` matching `holds`, failing if `"sign_in.first"` was ever said.
async fn until_no_sign_in_first(app: &mut App, holds: impl Fn(&State) -> bool) -> State {
    let state = app.until(holds).await;
    assert!(
        !app.outlet.said.now().1.codes.iter().any(|c| c == "sign_in.first")
            && !app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("sign_in.first")),
        "unexpected sign_in.first"
    );
    state
}

/// Waits for `Outlet.session` matching `holds`, failing if `"sign_in.first"` was ever said.
async fn session_no_sign_in_first(
    app: &mut App,
    holds: impl Fn(Option<&Session>) -> bool,
) -> Option<Session> {
    let session = app.session(holds).await;
    assert!(
        !app.outlet.said.now().1.codes.iter().any(|c| c == "sign_in.first")
            && !app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("sign_in.first")),
        "unexpected sign_in.first"
    );
    session
}

#[test]
fn commands_that_need_a_person_are_listed_in_one_table() {
    let cases = [
        (Command::Start, false),
        (Command::Import { path: "a.wav".into() }, false),
        (Command::Pause, false),
        (Command::Resume, false),
        (Command::Stop, false),
        (Command::Reconnect, false),
        (Command::Setup { setup: Setup::default() }, false),
        (Command::Change { id: "id".into(), change: Change::Title { text: "t".into() } }, false),
        (
            Command::Note {
                id: "id".into(),
                op: NoteOp::Add { at_ms: 0, text: "n".into(), anchor: None },
            },
            false,
        ),
        (Command::Refine { id: "id".into() }, false),
        (Command::Remove { id: "id".into() }, false),
        (Command::Restore { id: "id".into() }, false),
        (
            Command::Export {
                id: "id".into(),
                exporter: "markdown".into(),
                languages: vec![],
                to: "out.md".into(),
            },
            false,
        ),
        (
            Command::CopyTranscript {
                id: "id".into(),
                take: Some(1),
                languages: vec!["zh".into()],
            },
            false,
        ),
        (Command::HostArm { event: None, captions: true }, true),
        (Command::HostCancel, false),
        (Command::StopOutbound, false),
        (Command::SignIn, false),
        (Command::ConfirmSignIn, false),
        (Command::CancelSignIn, false),
        (Command::CreateIdentity { handle: None }, false),
        (Command::RecoverIdentity { did: "did:zu:made-up-1".into(), key: "z".into() }, false),
        (Command::RefreshQuota, true),
        (Command::DiscardLocal, true),
        (Command::SignOut, false),
        (Command::RotationSaved, false),
        (Command::CopyRotation, false),
        (Command::Update, false),
        (Command::OwnKey { service: "soniox".into(), key: None }, false),
        (Command::Captions { show: true }, false),
        (
            Command::CaptionGeometry {
                scene: CaptionScene::Band,
                rect: WindowRect { x: 10, y: 20, width: 800, height: 160 },
            },
            false,
        ),
        (Command::DestroyAudio { id: "id".into() }, false),
        (Command::Term { said: "ZuTalk".into(), written: None }, false),
        (Command::City { address: "pol.example".into() }, false),
        (Command::File { id: "id".into(), event: "e1".into(), audio: true }, true),
        (Command::Diagnose, false),
    ];
    for (command, expected) in cases {
        assert_eq!(command.needs_person(), expected, "{command:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn whoever_reads_late_gets_what_is_there_now() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let first = app.until(|_| true).await;
    assert!(first.me.is_none());
    assert_eq!(first.languages.len(), 8);

    let late = app.outlet.clone();
    let (seq, now) = late.state.now();
    assert!(seq >= 1, "expected published state seq >= 1, got {seq}");
    assert_eq!(*now, first);

    let state_sent = app
        .core
        .stats()
        .tally()
        .counts
        .into_iter()
        .find(|row| row.name == "state.sent")
        .map(|row| row.n);
    assert_eq!(state_sent, Some(seq));
}

#[tokio::test(flavor = "multi_thread")]
async fn more_codes_than_are_kept_lose_the_oldest() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    for _ in 0..20 {
        app.send(Command::Pause);
    }
    // Fence via the same FIFO mailbox without reading `said` beforehand.
    app.send(Command::Captions { show: true });
    app.until(|s| s.captions).await;

    let (_, said) = app.outlet.said.now();
    assert_eq!(said.next, 20);
    assert_eq!(said.codes.len(), Notices::KEEP_CODES);
    assert!(said.codes.iter().all(|c| c == "recording.none"), "{:?}", said.codes);

    let events = app.seen.of("notice.said");
    assert_eq!(events.len(), 20);
    assert!(
        events.iter().all(
            |l| l.level == zutalk_log::Level::Info && l.text("code") == Some("recording.none")
        ),
        "{events:?}"
    );

    let counted = app
        .core
        .stats()
        .tally()
        .counts
        .into_iter()
        .find(|row| row.name == "notices.said")
        .map(|row| row.n);
    assert_eq!(counted, Some(20));

    // One more distinct code pushes out the oldest and keeps the newest at the tail.
    app.send(Command::CopyRotation);
    app.notice("rotation.none").await;
    let (_, after) = app.outlet.said.now();
    assert_eq!(after.next, 21);
    assert_eq!(after.codes.len(), Notices::KEEP_CODES);
    assert_eq!(after.codes[..15], vec!["recording.none".to_string(); 15]);
    assert_eq!(after.codes[15], "rotation.none");
}

#[tokio::test(start_paused = true)]
async fn the_tally_comes_once_a_second_while_something_moves() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    let (seq_start, _) = app.outlet.stats.now();
    for _ in 0..5 {
        app.send(Command::Pause);
        app.notice("recording.none").await;
        tokio::time::sleep(Duration::from_millis(1_000)).await;
    }
    // Settle past the 5th-second boundary so its tick has run before we snapshot.
    tokio::time::sleep(Duration::from_millis(1)).await;

    let (seq_active, tally) = app.outlet.stats.now();
    let delta = seq_active - seq_start;
    assert!((4..=6).contains(&delta), "expected stats seq delta in 4..=6, got {delta}");
    let notices_said = tally.counts.iter().find(|r| r.name == "notices.said").map(|r| r.n);
    assert_eq!(notices_said, Some(5));

    tokio::time::sleep(Duration::from_millis(5_000)).await;
    let (seq_idle, _) = app.outlet.stats.now();
    assert_eq!(seq_idle, seq_active, "stats seq must not advance while nothing moves");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fifth_column_is_refused() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    let four = Setup {
        shown: vec!["en".into(), "zh".into(), "th".into(), "ja".into()],
        ..Setup::default()
    };
    app.send(Command::Setup { setup: four.clone() });
    app.until(|s| s.setup == four).await;

    let five = Setup {
        shown: vec!["en".into(), "zh".into(), "th".into(), "ja".into(), "es".into()],
        ..Setup::default()
    };
    app.send(Command::Setup { setup: five });
    app.notice("captions.columns").await;

    // Fence via the hub mailbox and verify `state.setup` did not change.
    app.send(Command::Captions { show: true });
    let after = app.until(|s| s.captions).await;
    assert_eq!(after.setup, four);

    let said: Vec<_> = app
        .seen
        .of("notice.said")
        .into_iter()
        .filter(|l| l.text("code") == Some("captions.columns"))
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_needs_nobody_signed_in() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let state = until_no_sign_in_first(&mut app, |_| true).await;
    assert!(state.me.is_none());
    assert!(app.library().await.is_empty());
    assert_eq!(state.languages.len(), 8);
    assert_eq!(state.exporters, ["markdown", "srt", "vtt"]);

    app.send(Command::Start);
    session_no_sign_in_first(&mut app, |s| s.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    session_no_sign_in_first(&mut app, |s| s.is_none()).await;
    let rows = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await;
    app.wait_progress(|p| p.refine.is_none()).await;
    assert_eq!(rows.len(), 1);
    let id = rows[0].id.clone();

    // Exactly one directory under `records/`, named for the device's `did:key`.
    let dirs = record_dirs(dir.path());
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert!(dirs[0].starts_with("did-key-z6Mk"), "{dirs:?}");
    let about_path = dir.path().join("records").join(&dirs[0]).join(&id).join("about.jsonl");
    let held = zu::Pack::read(&about_path).expect("about pack reads");
    assert!(!held.records.is_empty());
    let local_did = held.records[0].path.by.clone();
    assert!(local_did.starts_with("did:key:z6Mk"), "{local_did}");
    for record in &held.records {
        assert_eq!(record.path.by, local_did);
        assert_eq!(
            record.verify(|by, key, _| zu::did::local_key(by) == Some(*key), |_| None),
            Ok(())
        );
    }

    // Rename, search, export, remove and restore all work without signing in.
    app.send(Command::Change {
        id: id.clone(),
        change: Change::Title { text: "Local note".into() },
    });
    app.wait_library(|rows| rows.iter().any(|r| r.title == "Local note")).await;
    let hits = app.search("note").await;
    assert!(hits.hits.iter().any(|h| h.recording_id == id), "{hits:?}");
    let out = dir.path().join("exported");
    app.send(Command::Export {
        id: id.clone(),
        exporter: "markdown".into(),
        languages: vec![],
        to: out.to_string_lossy().into(),
    });
    app.send(Command::Remove { id: id.clone() });
    app.wait_library(|rows| rows.is_empty()).await;
    app.send(Command::Restore { id: id.clone() });
    app.wait_library(|rows| rows.len() == 1).await;
    wait_export(&app).await;
    assert!(
        !app.outlet.said.now().1.codes.iter().any(|c| c == "sign_in.first")
            && !app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("sign_in.first")),
        "unexpected sign_in.first"
    );
    let exported = fs::read_to_string(dir.path().join("exported.md")).expect("exported");
    assert!(exported.starts_with("# Local note"), "{exported}");

    // Observable: one `identity.who` event, and no `did:key` anywhere in the captured log.
    let who = app.seen.of("identity.who");
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].level, zutalk_log::Level::Info);
    assert_eq!(who[0].text("from"), Some("nobody"));
    assert_eq!(who[0].text("to"), Some("local"));
    assert_eq!(who[0].text("why"), Some("boot"));
    assert_eq!(who[0].bool("made"), Some(true));
    let multikey = local_did.strip_prefix("did:key:").expect("did:key prefix");
    for line in app.seen.all() {
        for (_, value) in &line.fields {
            if let zutalk_log::Value::Text(text) = value {
                assert!(
                    !text.contains(&local_did)
                        && !text.contains(multikey)
                        && !text.contains("did:key"),
                    "local DID leaked into log: {line:?}"
                );
            }
        }
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach() {
    let _store = STORE.lock().await;
    store(false);
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let server = format!("https://{}", relay.local_addr().expect("an address"));
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(config(dir.path(), &server));
    app.until(|_| true).await;
    app.send(Command::SignIn);
    let (stream, _) = relay.accept().await.expect("a connection");
    let state = app
        .until(|state| matches!(state.auth, zutalk_core::app::proto::AuthFlow::Scanning { .. }))
        .await;
    let zutalk_core::app::proto::AuthFlow::Scanning { url: code, .. } = state.auth else {
        panic!("expected Scanning auth flow");
    };
    assert!(code.starts_with(&format!("{server}/.well-known/zu-relay/")));
    drop((stream, relay));
    app.notice("offline").await;
    app.until(|state| state.auth == zutalk_core::app::proto::AuthFlow::Idle).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_device_is_the_same_identity_the_next_time() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let id = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await[0].id.clone();
    app.wait_progress(|p| p.refine.is_none()).await;
    drop(app);

    let again = App::start(dir.path());
    let rows = again.wait_library(|rows| rows.len() == 1).await;
    assert_eq!(rows[0].id, id);
    assert_eq!(record_dirs(dir.path()).len(), 1);

    let who = again.seen.of("identity.who");
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].text("from"), Some("nobody"));
    assert_eq!(who[0].text("to"), Some("local"));
    assert_eq!(who[0].text("why"), Some("boot"));
    assert_eq!(who[0].bool("made"), Some(false));
    assert_eq!(again.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let id = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await[0].id.clone();
    app.wait_progress(|p| p.refine.is_none()).await;
    drop(app);

    let local_dirs = record_dirs(dir.path());
    assert_eq!(local_dirs.len(), 1);
    let local_dir = dir.path().join("records").join(&local_dirs[0]);
    let before = snapshot(&local_dir);
    assert!(!before.is_empty());

    // Add a signed-in identity into the same key store so the device identity seed stays intact.
    let kept = [&[5; 32][..], ME.as_bytes()].concat();
    keys::set("identity", Some(&kept)).expect("the identity is kept");

    // P4-03: first sign-in adopts the local recording into the person's shelf while keeping the
    // local shelf bytes untouched.
    let mut app = App::start(dir.path());
    app.until(|s| s.me.as_deref() == Some(ME)).await;
    let adopted = app.wait_library(|rows| rows.len() == 1).await;
    assert_eq!(adopted[0].id, id);
    assert_eq!(snapshot(&local_dir), before, "local shelf bytes must stay intact after adoption");

    app.send(Command::SignOut);
    app.until(|s| s.me.is_none()).await;
    let back = app.wait_library(|rows| rows.len() == 1).await;
    assert_eq!(back[0].id, id);
    assert_eq!(snapshot(&local_dir), before, "local shelf changed across sign-in and sign-out");

    let sign_out: Vec<_> = app
        .seen
        .of("identity.who")
        .into_iter()
        .filter(|line| line.text("why") == Some("sign_out"))
        .collect();
    assert_eq!(sign_out.len(), 1, "{:?}", app.seen.of("identity.who"));
    assert_eq!(sign_out[0].text("from"), Some("person"));
    assert_eq!(sign_out[0].text("to"), Some("local"));
    assert_eq!(sign_out[0].bool("made"), Some(false));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_needs_a_person_says_so_and_writes_nothing() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let id = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await[0].id.clone();
    app.wait_progress(|p| p.refine.is_none()).await;

    for cmd in [
        Command::HostArm { event: None, captions: true },
        Command::File { id: id.clone(), event: "e1".into(), audio: true },
    ] {
        app.send(cmd);
        app.notice("sign_in.first").await;
    }

    let dirs = record_dirs(dir.path());
    assert_eq!(dirs.len(), 1);
    let local_dir = dir.path().join("records").join(&dirs[0]);
    assert!(!dir.path().join("host.lock").exists());
    assert!(!local_dir.join("filings.jsonl").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_is_signed_to_its_end_by_whoever_signed_its_start() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|s| s.me.as_deref() == Some(ME)).await;

    app.send(Command::Start);
    let id = app.session(|s| s.is_some()).await.expect("session").id;
    tokio::time::sleep(Duration::from_millis(500)).await;
    app.send(Command::SignOut);
    app.until(|s| s.me.is_none()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    // The task started under the person's shelf and keys: once `progress.refine` reports "offline",
    // `seal()` has already written the sealed length record to that shelf.
    app.wait_progress(|p| p.refine.as_ref().is_some_and(|w| w.id == id && w.step == "offline"))
        .await;
    assert!(app.library().await.is_empty(), "the local shelf must not hold the person's recording");

    let person_dir = dir.path().join("records").join(ME.replace(':', "-"));
    let held = zu::Pack::read(person_dir.join(&id).join("about.jsonl")).expect("person's about");
    assert_eq!(held.records.len(), 2, "start and sealed length records");
    let person_pub = zu::SecretKey::from_seed(&[5; 32]).public();
    for record in &held.records {
        assert_eq!(record.path.by, ME);
        assert_eq!(record.verify(|by, key, _| by == ME && *key == person_pub, |_| None), Ok(()));
    }
    let person_shelf = zutalk_core::record::shelf::Shelf::new(
        &person_dir,
        ME,
        Arc::new(zu::SecretKey::from_seed(&[5; 32])),
    );
    let about = person_shelf.about(&id).expect("reads").expect("exists").about;
    assert!(about.length_ms > 0, "sealed length must be on the person's shelf");

    let local_dirs: Vec<_> =
        record_dirs(dir.path()).into_iter().filter(|d| d.starts_with("did-key-")).collect();
    for local in local_dirs {
        assert!(!dir.path().join("records").join(local).join(&id).exists());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_signed_in_nothing_is_asked_of_our_server() {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let _store = STORE.lock().await;
    store(false);
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await;

    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(config(dir.path(), &server.uri()));
    assert!(app.until(|_| true).await.me.is_none());

    app.send(Command::Setup { setup: Setup { spoken: vec!["en".into()], ..Setup::default() } });
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let id = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await[0].id.clone();
    app.wait_progress(|p| p.refine.is_none()).await;

    app.send(Command::Change { id: id.clone(), change: Change::Title { text: "Quiet".into() } });
    app.wait_library(|rows| rows.iter().any(|r| r.title == "Quiet")).await;
    let hits = app.search("Quiet").await;
    assert!(hits.hits.iter().any(|h| h.recording_id == id), "{hits:?}");
    let out = dir.path().join("quiet.md");
    app.send(Command::Export {
        id,
        exporter: "markdown".into(),
        languages: vec![],
        to: out.to_string_lossy().into(),
    });
    app.send(Command::HostArm { event: None, captions: true });
    app.notice("sign_in.first").await;
    wait_export(&app).await;
    assert!(out.exists());

    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_that_cannot_be_read_mean_nobody_signs_and_it_is_said() {
    let _store = STORE.lock().await;
    store(false);
    let entry = keyring_core::Entry::new("ZuTalk", "identity.local").expect("mock entry");
    let mock: &keyring_core::mock::Cred = entry.as_any().downcast_ref().expect("mock cred");
    mock.set_error(keyring_core::Error::Invalid("mock".into(), "unreadable".into()));

    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.notice("keys.store").await;
    let state = app.until(|_| true).await;
    assert!(state.me.is_none());
    assert!(app.library().await.is_empty());

    app.send(Command::Start);
    app.notice("keys.store").await;

    let who = app.seen.of("identity.who");
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].level, zutalk_log::Level::Warn);
    assert_eq!(who[0].text("from"), Some("nobody"));
    assert_eq!(who[0].text("to"), Some("nobody"));
    assert_eq!(who[0].text("why"), Some("boot"));
    assert_eq!(who[0].bool("made"), Some(false));
    assert_eq!(app.seen.refused(), 0);
}

/// Someone who had a community invite in the 0.8 app starts the new one and is, with nothing
/// asked of them, someone with a Zu identity and the hours the invite had left. Without an
/// invite nothing changes.
#[tokio::test(flavor = "multi_thread")]
async fn an_invite_the_old_app_left_becomes_an_identity_with_the_hours_it_had_left() {
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let _store = STORE.lock().await;
    store(false);
    let (dir, old) = (tempfile::tempdir().expect("a folder"), tempfile::tempdir().expect("one"));
    let invite = old.path().join("Secrets/community-invite.json");
    let directory = Arc::new(MockServer::start().await);
    let took = Mock::given(method("POST")).respond_with(ResponseTemplate::new(200));
    took.expect(1).mount(&directory).await;
    let invites = MockServer::start().await;
    let retired = json!({ "status": "retired", "remaining_seconds": 3600 });
    Mock::given(method("POST"))
        .and(path("/v1/retire"))
        .and(header("authorization", "Bearer the-admin-token"))
        .and(body_json(json!({ "access_token": "zl-token" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(retired))
        .expect(1)
        .mount(&invites)
        .await;
    _ = rustls::crypto::ring::default_provider().install_default();
    let (http, url, key) = (reqwest::Client::new(), invites.uri(), "the-admin-token".into());
    let invites_there = Some(Invites { http, url, key });
    let (server, _kept) = serve_with(Arc::new(Plc(directory.clone())), invites_there).await;
    let start = || {
        let (old, directory) = (Some(old.path().into()), directory.uri());
        App::with(Config { old, directory, ..config(dir.path(), &server) })
    };
    let mut app = start();
    assert!(app.until(|_| true).await.me.is_none());
    assert!(directory.received_requests().await.expect("recorded").is_empty());
    drop(app);
    let json = br#"{"version":1,"accessToken":"zl-token"}"#;
    fs::create_dir_all(old.path().join("Secrets")).expect("the old app made it");
    fs::write(&invite, json).expect("the old app wrote it");
    let mut app = start();
    // An hour of live captions, at 50 µ$ a second.
    let state = app.until(|state| state.left.is_some()).await;
    assert_eq!(state.left, Some(3600 * 50));
    assert!(state.me.as_ref().is_some_and(|me| me.starts_with("did:zu:")));
    assert!(state.rotation_unsaved, "the person is to be reminded of the rotation key");
    app.send(Command::RotationSaved);
    app.until(|state| !state.rotation_unsaved).await;
    drop(app);
    // Started again: the same identity, and nothing is sent again.
    let mut app = start();
    let again = app.until(|_| true).await;
    assert_eq!((again.me, again.rotation_unsaved), (state.me, false));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fs::read(&invite).expect("it is there"), json, "the old file is left as it is");
    directory.verify().await;
    invites.verify().await;
}

/// Someone who used the 0.8 app finds their recordings here with nothing asked of them, the
/// first time the app starts and never again. One whose whole-session transcript 0.8 never
/// wrote is not sent anywhere to have one written.
#[tokio::test(flavor = "multi_thread")]
async fn the_old_library_moves_in_on_its_own_and_nothing_of_it_leaves() {
    let _store = STORE.lock().await;
    store(true);
    let (dir, old) = (tempfile::tempdir().expect("a folder"), old_library());
    let start =
        || App::with(Config { old: Some(old.path().into()), ..config(dir.path(), NOWHERE) });
    let mut app = start();
    let rows = app.wait_library(|rows| rows.len() == 3).await;
    app.wait_progress(|p| p.move_old.is_none()).await;
    let state = app.until(|_| true).await;
    assert!(state.unmoved.is_empty());
    let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(titles, ["interview", "Weekly sync", ""]);
    assert_eq!(rows.iter().map(|row| row.draft).collect::<Vec<_>>(), [false, true, false]);
    drop(app);
    // Again: nothing moves twice, and the live draft from 0.8, audio and all, is owed nothing.
    let app = start();
    let again_rows = app.wait_library(|rows| !rows.is_empty()).await;
    assert_eq!(again_rows, rows);
    assert!(app.outlet.progress.now().1.refine.is_none());
    let (seq_before, _) = app.outlet.progress.now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (seq_after, latest) = app.outlet.progress.now();
    assert_eq!(seq_after, seq_before, "{latest:?}");
    assert!(latest.refine.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_stops_at_once_and_is_kept_whole() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let state = app.until(|s| s.me.as_deref() == Some(ME)).await;
    assert_eq!(state.me.as_deref(), Some(ME));
    let setup = Setup { spoken: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup: setup.clone() });
    app.send(Command::Start);
    let audio_only = |session: Option<&Session>| {
        session.map(|s| &s.status) == Some(&Status::AudioOnly { why: "captions.off".into() })
    };
    app.session(audio_only).await;
    app.send(Command::Pause);
    app.session(|s| s.is_some_and(|s| s.status == Status::Paused)).await;
    app.send(Command::Resume);
    app.session(audio_only).await;
    app.send(Command::Start);
    app.notice("recording.under_way").await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let prog = app.wait_progress(|p| p.refine.is_some()).await;
    assert!(
        matches!(prog.refine.as_ref().map(|w| w.step.as_str()), Some("closing" | "offline")),
        "{prog:?}"
    );
    // The tape is sealed behind the stop, and the transcript waits for a key.
    app.wait_progress(|p| p.refine.as_ref().is_some_and(|w| w.step == "offline")).await;
    let row = app.wait_library(|rows| !rows.is_empty()).await[0].clone();
    assert!((900..2_000).contains(&row.length_ms) && row.draft, "{row:?}");
    // What the person chose for the next recording is still there after a restart.
    let mut again = App::start(dir.path());
    assert_eq!(again.until(|_| true).await.setup, setup);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_imported_file_is_a_recording_and_a_change_is_one_more_record() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    let file = dir.path().join("Board meeting.wav");
    wav(&file, 1_500);
    app.send(Command::Import { path: file.to_string_lossy().into() });
    let rows = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await;
    app.session(|s| s.is_none()).await;
    let row = rows[0].clone();
    assert_eq!((row.title.as_str(), row.length_ms, row.draft), ("Board meeting", 1_500, true));
    let id = row.id;
    let change = |change| Command::Change { id: id.clone(), change };
    app.send(change(Change::Title { text: "Budget".into() }));
    app.send(change(Change::Speaker { label: "1".into(), name: "Chair".into() }));
    app.send(change(Change::Note { text: "Decide in May".into() }));
    let open = app
        .wait_recording(&id, |r| r.is_some_and(|o| o.note == "Decide in May"))
        .await
        .expect("it is open");
    assert_eq!((open.row.title.as_str(), open.speakers["1"].as_str()), ("Budget", "Chair"));
    // A line is corrected on the record, not on a draft that is about to be replaced.
    let key = format!("{id}.0.00000");
    app.send(change(Change::Line { key, language: "en".into(), text: "Hello".into() }));
    app.notice("recording.writing").await;
    app.send(Command::Remove { id: id.clone() });
    app.wait_library(|rows| rows.is_empty()).await;
    assert!(app.recording(&id).await.is_none());
    app.send(Command::Restore { id: id.clone() });
    app.wait_library(|rows| rows.len() == 1).await;
    // Written out by a row of the exporters, the extension added when the path has none.
    let out = dir.path().join("out");
    let to = out.to_string_lossy().into();
    let export =
        Command::Export { id: id.clone(), exporter: "markdown".into(), languages: vec![], to };
    app.send(export);
    let to = dir.path().join("out.txt").to_string_lossy().into();
    app.send(Command::Export { id: id.clone(), exporter: "pdf".into(), languages: vec![], to });
    app.notice("export.which").await;
    wait_export(&app).await;
    let exported = fs::read_to_string(dir.path().join("out.md")).expect("it was exported");
    assert!(exported.starts_with("# Budget"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_holds_no_sound_leaves_nothing_behind() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    let file = dir.path().join("empty.wav");
    fs::write(&file, b"not a sound").expect("the file is written");
    app.send(Command::Import { path: file.to_string_lossy().into() });
    app.notice("audio.file.format").await;
    app.session(|s| s.is_none()).await;
    app.wait_progress(|p| p.refine.is_none()).await;
    let rows = app.wait_library(|rows| rows.is_empty()).await;
    assert!(rows.is_empty(), "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_room_carries_the_live_captions_or_a_recording_and_the_draft_is_kept_line_by_line() {
    let _store = STORE.lock().await;
    store(true);
    keys::set("soniox", Some(b"sk-own-test-key")).expect("the key is kept");
    let (server, _kept) = serve().await;
    let soniox = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let realtime = format!("ws://{}", soniox.local_addr().expect("an address"));
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::on(dir.path(), &server, &realtime);
    app.until(|_| true).await;
    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup });
    // A hosted live room: a link with the key after `#`, and nothing in it yet.
    app.send(Command::HostArm { event: None, captions: true });
    let state = app
        .until(|state| {
            matches!(state.host, zutalk_core::app::proto::HostState::Armed { link: Some(_), .. })
        })
        .await;
    let zutalk_core::app::proto::HostState::Armed { link: Some(live_link), .. } = state.host else {
        panic!("expected Armed host with link");
    };
    assert!(live_link.starts_with(&format!("{server}/r/")));
    let mut viewer = Viewer::open(&live_link).await;
    viewer.until(|event, plain| (event == "slot" && plain.is_none()).then_some(())).await;
    app.send(Command::Start);
    let id = app.session(|s| s.is_some()).await.map(|s| s.id).unwrap();
    // The realtime service hears the recording, and says a word.
    let (stream, _) = soniox.accept().await.expect("a connection");
    let mut ws = accept_async(stream).await.expect("a websocket");
    let Some(Ok(Message::Text(config))) = ws.next().await else { panic!("no configuration") };
    assert!(config.contains("sk-own-test-key"));
    let hello = json!({"text": "Hello", "start_ms": 120, "end_ms": 480, "is_final": true,
                       "language": "en", "speaker": "1"});
    let said =
        json!({"tokens": [hello, {"text": "<end>", "is_final": true}], "total_audio_proc_ms": 500});
    ws.send(Message::text(said.to_string())).await.expect("it is said");
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                _ = ws
                    .send(Message::text(json!({"tokens": [], "finished": true}).to_string()))
                    .await;
            }
        }
    });
    // The captions show it, and so does the room, sealed.
    let frame = app.frame(|f| f.columns.iter().any(|c| !c.lines.is_empty())).await;
    let column = &frame.columns[0];
    assert_eq!((column.lines[0].text.as_str(), &column.lines[0].units[..]), ("Hello", &[][..]));
    assert_eq!(column.rate, 4.0);
    let shown = |event: &str, plain: Option<&[u8]>| {
        let columns: serde_json::Value = serde_json::from_slice(plain?).ok()?;
        (event == "slot" && columns[0]["lines"][0]["text"] == "Hello").then_some(())
    };
    viewer.until(shown).await;
    // The draft has the line while the recording goes on.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if app.transcript(&id, Some(0), 0).await.lines.iter().any(|(_, s)| s.line.text == "Hello") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "draft line did not land");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Stopping recording automatically closes the hosted live room and notifies viewers.
    app.send(Command::Stop);
    viewer.until(|event, _| (event == "gone").then_some(())).await;
    let state = app.until(|state| state.host == zutalk_core::app::proto::HostState::Idle).await;
    assert_eq!(state.host, zutalk_core::app::proto::HostState::Idle);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_is_one_more_record() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    let rows = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await;
    let id = rows[0].id.clone();
    let change = |change| Command::Change { id: id.clone(), change };
    app.send(change(Change::Title { text: "Budget".into() }));
    app.send(change(Change::Speaker { label: "1".into(), name: "Chair".into() }));
    app.send(change(Change::Note { text: "Decide in May".into() }));
    let open = app
        .wait_recording(&id, |r| r.is_some_and(|o| o.note == "Decide in May"))
        .await
        .expect("it is open");
    assert_eq!((open.row.title.as_str(), open.speakers["1"].as_str()), ("Budget", "Chair"));
    // A line is corrected on the record, not on a draft that is about to be replaced.
    let key = format!("{id}.0.00000");
    app.send(change(Change::Line { key, language: "en".into(), text: "Hello".into() }));
    app.notice("recording.writing").await;
    app.send(Command::Remove { id: id.clone() });
    app.wait_library(|rows| rows.is_empty()).await;
    assert!(app.recording(&id).await.is_none());
    app.send(Command::Restore { id: id.clone() });
    app.wait_library(|rows| rows.len() == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_recovery_key_goes_to_the_clipboard_and_through_nothing_else() {
    let _store = STORE.lock().await;
    store(true);
    keys::set("rotation.unsaved", Some(&[7; 32])).expect("a recovery key is kept");
    let written = zu::did::Rotation::from_bytes(&[7; 32]).expect("a P-256 key").multikey();
    let clipped = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let theirs = clipped.clone();
    let clip: Clip = Arc::new(move |text| {
        theirs.lock().expect("one at a time").push(text.into());
        true
    });
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(Config { clip, ..config(dir.path(), NOWHERE) });
    let first = app.until(|state| state.rotation_unsaved).await;
    let seen = Arc::new(std::sync::Mutex::new(vec![
        serde_json::to_string(&first).expect("state is JSON"),
        serde_json::to_string(&*app.outlet.said.now().1).expect("notices are JSON"),
    ]));
    let into = seen.clone();
    let mut watch_state = app.outlet.state.clone();
    let mut watch_said = app.outlet.said.clone();
    let watcher = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some((_, s)) = watch_state.next() => {
                    into.lock().expect("one at a time")
                        .push(serde_json::to_string(&*s).expect("state is JSON"));
                }
                Some((_, n)) = watch_said.next() => {
                    into.lock().expect("one at a time")
                        .push(serde_json::to_string(&*n).expect("notices are JSON"));
                }
                else => break,
            }
        }
    });
    app.send(Command::CopyRotation);
    app.notice("copied").await;
    assert_eq!(*clipped.lock().expect("one at a time"), [written.to_string()]);
    // Saved, it is no longer asked for, and can still be copied.
    app.send(Command::RotationSaved);
    let saved = app.until(|state| !state.rotation_unsaved).await;
    app.send(Command::CopyRotation);
    app.notice("copied").await;
    watcher.abort();
    let mut checked = seen.lock().expect("one at a time").clone();
    checked.push(serde_json::to_string(&saved).expect("state is JSON"));
    checked.push(serde_json::to_string(&*app.outlet.said.now().1).expect("notices are JSON"));
    assert!(checked.iter().all(|json| !json.contains(&written[1..])), "the key reached the page");
    // Observability privacy check (P1-20 #5): neither the recovery key nor the signed-in DID
    // or handle appears in any captured log event.
    let multikey_tail = &written[1..];
    for line in app.seen.all() {
        let rendered = format!("{line:?}");
        assert!(!rendered.contains(multikey_tail), "recovery key leaked into log: {line:?}");
        assert!(!rendered.contains(ME), "signed-in DID leaked into log: {line:?}");
        assert!(!rendered.contains("did:zu:"), "DID leaked into log: {line:?}");
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_recovery_key_or_a_clipboard_it_says_so() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::CopyRotation);
    app.notice("rotation.none").await;
    keys::set("rotation.unsaved", Some(&[7; 32])).expect("a recovery key is kept");
    app.send(Command::CopyRotation); // the default clipboard is none
    app.notice("clip").await;
}

/// A newer version the shell has ready is offered, and installed only while nothing is
/// recording, since installing relaunches the app. Trouble finding one is said.
#[tokio::test(flavor = "multi_thread")]
async fn an_update_is_installed_only_while_nothing_is_recording() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let _store = STORE.lock().await;
    store(true);
    let (installs, dir) = (Arc::new(AtomicUsize::new(0)), tempfile::tempdir().expect("a folder"));
    let counted = installs.clone();
    let install: Install = Arc::new(move || _ = counted.fetch_add(1, SeqCst));
    let mut app = App::with(Config { install, ..config(dir.path(), NOWHERE) });
    app.until(|_| true).await;
    app.core.looked(Err("update")).await;
    app.notice("update").await;
    app.core.looked(Ok("0.9.1".into())).await;
    app.until(|state| state.update.as_deref() == Some("0.9.1")).await;
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    app.send(Command::Update);
    app.notice("update.recording").await;
    assert_eq!(installs.load(SeqCst), 0, "the recording was not interrupted");
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    app.send(Command::Update);
    app.notice("update.install").await; // this port returns, as one that could not install
    assert_eq!(installs.load(SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_core_runs_on_a_tree_with_a_name() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let app = App::with_log(hub::config(dir.path()), zutalk_log::Level::Debug);

    let starts = app.seen.of("task.start");
    assert_eq!(starts.len(), 4, "{starts:?}");
    assert!(
        starts
            .iter()
            .any(|line| line.text("tree") == Some("core") && line.text("task") == Some("hub")),
        "expected hub task.start, got {starts:?}"
    );
    assert!(
        starts
            .iter()
            .any(|line| line.text("tree") == Some("core") && line.text("task") == Some("stats")),
        "expected stats task.start, got {starts:?}"
    );
    assert!(
        starts
            .iter()
            .any(|line| line.text("tree") == Some("work") && line.text("task") == Some("desk")),
        "expected desk task.start, got {starts:?}"
    );
    assert!(
        starts
            .iter()
            .any(|line| line.text("tree") == Some("work") && line.text("task") == Some("devices")),
        "expected devices task.start, got {starts:?}"
    );

    let tally = app.core.stats().tally();
    let started = tally.counts.iter().find(|row| row.name == "task.started").map(|row| row.n);
    assert_eq!(started, Some(4));
    assert_eq!(app.seen.refused(), 0);
}

/// Waits for a `State` matching `holds`, failing if `"keys.none"` was ever said.
async fn until_no_keys_none(app: &mut App, holds: impl Fn(&State) -> bool) -> State {
    let state = app.until(holds).await;
    assert!(
        !app.outlet.said.now().1.codes.iter().any(|c| c == "keys.none")
            && !app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("keys.none")),
        "unexpected keys.none"
    );
    state
}

/// Waits for `Outlet.session` matching `holds`, failing if `"keys.none"` was ever said.
async fn session_no_keys_none(
    app: &mut App,
    holds: impl Fn(Option<&Session>) -> bool,
) -> Option<Session> {
    let session = app.session(holds).await;
    assert!(
        !app.outlet.said.now().1.codes.iter().any(|c| c == "keys.none")
            && !app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("keys.none")),
        "unexpected keys.none"
    );
    session
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_key_sound_is_kept_and_the_status_says_why() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _store = STORE.lock().await;
    store(false);

    // Local port that only counts connections: no live lane may connect without a key.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let realtime = format!("ws://{}", listener.local_addr().expect("an address"));
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = connections.clone();
    let accept_task = tokio::spawn(async move {
        while let Ok((_stream, _)) = listener.accept().await {
            counted.fetch_add(1, SeqCst);
        }
    });

    // Whole-transcript stub expecting zero requests.
    let soniox = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).expect(0).mount(&soniox).await;

    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(Config {
        realtime,
        soniox: format!("{}/v1", soniox.uri()),
        ..config(dir.path(), NOWHERE)
    });
    until_no_keys_none(&mut app, |s| s.me.is_none()).await;

    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup });
    until_no_keys_none(&mut app, |s| s.setup.shown == ["en"]).await;

    let no_key = |session: Option<&Session>| {
        session.map(|s| &s.status) == Some(&Status::AudioOnly { why: "captions.no_key".into() })
    };
    app.send(Command::Start);
    session_no_keys_none(&mut app, no_key).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Observable (P1-12): recording.start says captions.no_key and lanes = 0; only one
    // session.status emitted (to AudioOnly, why = captions.no_key) across steady ticks.
    let starts = app.seen.of("recording.start");
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(starts[0].text("captions"), Some("captions.no_key"));
    assert_eq!(starts[0].u64("lanes"), Some(0));
    let statuses = app.seen.of("session.status");
    assert_eq!(statuses.len(), 1, "{statuses:?}");
    assert_eq!(statuses[0].text("to"), Some("AudioOnly"));
    assert_eq!(statuses[0].text("why"), Some("captions.no_key"));

    app.send(Command::Pause);
    session_no_keys_none(&mut app, |s| s.is_some_and(|session| session.status == Status::Paused))
        .await;
    app.send(Command::Resume);
    session_no_keys_none(&mut app, no_key).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    app.send(Command::Stop);
    session_no_keys_none(&mut app, |s| s.is_none()).await;

    let rows = app.wait_library(|rows| rows.len() == 1 && rows[0].length_ms > 0).await;
    app.wait_progress(|p| p.refine.is_none()).await;
    let row = &rows[0];
    assert!(row.draft && row.audio && row.length_ms > 0, "{row:?}");
    assert!(app.outlet.progress.now().1.refine.is_none());

    // Ensure `keys.none` was never said in either `Outlet.said` or `notice.said`.
    assert!(
        !app.outlet.said.now().1.codes.iter().any(|c| c == "keys.none")
            && !app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("keys.none")),
        "unexpected keys.none"
    );

    accept_task.abort();
    assert_eq!(connections.load(SeqCst), 0, "no realtime lane may dial without a key");
    soniox.verify().await;

    let skipped = app.seen.of("write.skipped");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].level, zutalk_log::Level::Info);
    assert_eq!(skipped[0].u64("n"), Some(1));
    assert_eq!(skipped[0].text("why"), Some("keys.none"));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_their_own_key_captions_come_without_signing_in() {
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _store = STORE.lock().await;
    hub::nobody();

    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await;

    let asked = hub::Asked::default();
    let stt = hub::soniox(&asked).await;

    let soniox_rt = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let realtime = format!("ws://{}", soniox_rt.local_addr().expect("an address"));

    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(Config {
        realtime,
        soniox: format!("{}/v1", stt.uri()),
        ..config(dir.path(), &server.uri())
    });
    let initial = app.until(|_| true).await;
    assert!(initial.me.is_none());

    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup });
    app.until(|s| s.setup.shown == ["en"]).await;

    app.send(Command::Start);
    let id = app.session(|s| s.is_some()).await.expect("session").id;

    // Scripted realtime service verifies the own key and sends one line.
    let (stream, _) = soniox_rt.accept().await.expect("a connection");
    let mut ws = accept_async(stream).await.expect("a websocket");
    let Some(Ok(Message::Text(cfg))) = ws.next().await else { panic!("no configuration") };
    assert!(cfg.contains("sk-own-test-key"), "{cfg}");
    let hello = json!({"text": "Hello", "start_ms": 120, "end_ms": 480, "is_final": true,
                       "language": "en", "speaker": "1"});
    let said =
        json!({"tokens": [hello, {"text": "<end>", "is_final": true}], "total_audio_proc_ms": 500});
    ws.send(Message::text(said.to_string())).await.expect("it is said");
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                _ = ws
                    .send(Message::text(json!({"tokens": [], "finished": true}).to_string()))
                    .await;
            }
        }
    });

    // Live captions show the word.
    let frame = app.frame(|f| f.columns.iter().any(|c| !c.lines.is_empty())).await;
    assert_eq!(frame.columns[0].lines[0].text, "Hello");

    // Draft has the line before whole-transcript replaces it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if app.transcript(&id, Some(0), 0).await.lines.iter().any(|(_, l)| l.line.text == "Hello") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "draft line did not land");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    // Whole transcript runs with the own key and turns the draft into a written record.
    app.wait_library(|rows| rows.iter().any(|r| r.id == id && !r.draft)).await;
    app.wait_progress(|p| p.refine.is_none()).await;
    let state = app.until(|_| true).await;
    assert!(state.me.is_none());
    assert_eq!(asked.lock().expect("one at a time").len(), 1);
    let requests = stt.received_requests().await.expect("requests");
    assert!(!requests.is_empty());
    for req in &requests {
        assert_eq!(
            req.headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer sk-own-test-key")
        );
    }

    // All records live on the device's local identity shelf, including the draft take and take 1.
    let expected_dir = hub::local_did().replace(':', "-");
    assert_eq!(record_dirs(dir.path()), [expected_dir]);
    let lines_pack = zu::Pack::read(
        dir.path()
            .join("records")
            .join(hub::local_did().replace(':', "-"))
            .join(&id)
            .join("lines.jsonl"),
    )
    .expect("lines pack reads");
    assert!(
        lines_pack.records.iter().any(|r| r.path.key == format!("{id}.0.00000")),
        "draft line must be kept in lines.jsonl"
    );
    let written = hub::local_shelf(dir.path()).read(&id).expect("reads").expect("exists");
    assert_eq!(written.about.take, 1);
    assert_eq!(written.lines[0].1.line.text, "Zuzalu is in Chiang Mai.");

    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn drafts_without_a_key_wait_quietly_and_are_written_once_there_is_one() {
    use zutalk_core::live::lines::Line;
    use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

    let _store = STORE.lock().await;
    hub::nobody();
    keys::set("soniox", None).expect("key cleared");

    let dir = tempfile::tempdir().expect("a folder");
    let data = dir.path();
    let now = wall_ms() as i64;
    let shelf = hub::local_shelf(data);
    for id in ["draft1", "draft2"] {
        let line =
            Line { language: "en".into(), text: format!("Said in {id}."), ..Line::default() };
        let said = Said { line, via: [("text".to_string(), "stt-rt-v6".to_string())].into() };
        shelf.put(id, LINE, &[(format!("{id}.0.00000"), Some(said))], now).expect("draft line");
        let about =
            About { started: now, length_ms: 1_000, spoken: vec!["en".into()], ..About::default() };
        shelf.put(id, RECORDING, &[(id.to_string(), Some(about))], now).expect("draft record");
        hub::tape(data, id, 1);
    }

    let asked = hub::Asked::default();
    let stt = hub::soniox(&asked).await;
    let mut app =
        App::with(Config { soniox: format!("{}/v1", stt.uri()), ..config(data, NOWHERE) });

    let state = app.until(|_| true).await;
    let rows = app.wait_library(|rows| rows.len() == 2).await;
    assert!(state.me.is_none());
    assert!(app.outlet.progress.now().1.refine.is_none());
    assert!(rows.iter().all(|r| r.draft && r.audio));

    let (prog_seq, _) = app.outlet.progress.now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(app.outlet.said.now().1.next, 0, "unexpected notice: {:?}", app.outlet.said.now().1);
    let (prog_seq_after, latest) = app.outlet.progress.now();
    assert_eq!(prog_seq_after, prog_seq, "unexpected refine progress: {latest:?}");
    assert!(latest.refine.is_none());
    assert!(stt.received_requests().await.expect("requests").is_empty());

    let skipped = app.seen.of("write.skipped");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].level, zutalk_log::Level::Info);
    assert_eq!(skipped[0].u64("n"), Some(2));
    assert_eq!(skipped[0].text("why"), Some("keys.none"));

    // Setting an own Soniox key re-runs recover and writes both waiting drafts.
    app.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-own-test-key".into()) });
    let rows = app.wait_library(|rows| rows.len() == 2 && rows.iter().all(|r| !r.draft)).await;
    app.wait_progress(|p| p.refine.is_none()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(asked.lock().expect("one at a time").len(), 2);
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn a_hub_that_cannot_keep_up_refuses_commands_and_says_busy() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    let before = app.outlet.said.now().1.next;
    let (mut accepted, mut refused) = (0usize, 0usize);
    for _ in 0..300 {
        match app.core.send(Command::Pause) {
            Ok(()) => accepted += 1,
            Err(code) => {
                assert_eq!(code, "busy");
                refused += 1;
            }
        }
    }
    assert!(accepted > 0 && accepted <= HUB_MAIL_MSGS, "accepted={accepted}");
    assert_eq!(accepted + refused, 300);
    assert!(refused > 0, "expected at least one refused command");

    let target = before + accepted as u64;
    tokio::time::timeout(Duration::from_secs(20), async {
        while app.outlet.said.now().1.next < target {
            app.outlet.said.next().await.expect("the core runs");
        }
    })
    .await
    .expect("all accepted Pause commands were handled");

    let (_, said) = app.outlet.said.now();
    assert_eq!(said.next, target);
    assert!(said.codes.iter().all(|c| c == "recording.none"), "{:?}", said.codes);
    let said_events = app
        .seen
        .of("notice.said")
        .into_iter()
        .filter(|l| l.text("code") == Some("recording.none"))
        .count();
    assert_eq!(said_events, accepted);

    let full = app.seen.of("queue.full");
    assert_eq!(full.len(), 1, "{full:?}");
    assert_eq!(full[0].level, zutalk_log::Level::Warn);
    assert_eq!(full[0].text("queue"), Some("hub.mail"));
    assert_eq!(full[0].text("policy"), Some("reject"));
    assert_eq!(full[0].u64("capacity"), Some(HUB_MAIL_MSGS as u64));

    let refused_events = app.seen.of("command.refused");
    assert_eq!(refused_events.len(), refused, "{refused_events:?}");
    assert!(
        refused_events.iter().all(|l| {
            l.level == zutalk_log::Level::Warn
                && l.text("kind") == Some("Pause")
                && l.text("code") == Some("busy")
        }),
        "{refused_events:?}"
    );

    let done_events = app.seen.of("command.done");
    assert_eq!(done_events.len(), accepted, "{done_events:?}");
    assert!(
        done_events.iter().all(|l| {
            l.level == zutalk_log::Level::Warn
                && l.text("kind") == Some("Pause")
                && l.text("code") == Some("recording.none")
                && l.u64("took_us").is_some()
        }),
        "{done_events:?}"
    );

    let tally = app.core.stats().tally();
    let row = tally.queues.iter().find(|r| r.name == "hub.mail").expect("hub.mail queue row");
    assert_eq!(row.capacity, HUB_MAIL_MSGS as u64);
    assert_eq!(row.full, refused as u64);
    assert_eq!(row.deepest, HUB_MAIL_MSGS as u64);
    assert_eq!(
        tally.counts.iter().find(|r| r.name == "commands.refused").map(|r| r.n),
        Some(refused as u64)
    );
    assert_eq!(
        tally.counts.iter().find(|r| r.name == "commands.done").map(|r| r.n),
        Some(accepted as u64)
    );
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn what_the_shell_found_is_never_dropped() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    let mut refused = 0usize;
    for _ in 0..300 {
        if app.core.send(Command::Pause).is_err() {
            refused += 1;
        }
    }
    assert!(refused > 0, "mailbox must be full before calling looked");

    app.core.looked(Ok("0.9.1".into())).await;
    let state = app.until(|s| s.update.as_deref() == Some("0.9.1")).await;
    assert_eq!(state.update.as_deref(), Some("0.9.1"));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn every_command_leaves_its_kind_its_answer_and_nothing_it_carried() {
    use zutalk_core::live::lines::Line;
    use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let id = "rec130001";
    let secret_title = "KestrelTitleMarker";
    let secret_query = "AlbatrossSearchNeedle";

    let shelf = hub::local_shelf(dir.path());
    let line = Line {
        language: "en".into(),
        text: format!("Contains {secret_query} here."),
        ..Line::default()
    };
    let said = Said { line, via: [("text".to_string(), "stt-rt-v6".to_string())].into() };
    shelf.put(id, LINE, &[(format!("{id}.0.00000"), Some(said))], now).expect("line");
    let about = About {
        started: now,
        length_ms: 1_000,
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.to_string(), Some(about))], now).expect("recording");

    let mut app = App::start(dir.path());
    app.wait_library(|rows| rows.iter().any(|r| r.id == id)).await;

    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup });
    app.send(Command::Change {
        id: id.into(),
        change: Change::Title { text: secret_title.into() },
    });
    let _ = app.query(Query::Search { query: secret_query.into(), limit: 20 }).await;
    app.send(Command::Pause);
    app.notice("recording.none").await;

    let done = app.seen.of("command.done");
    assert_eq!(done.len(), 3, "{done:?}");
    let expected = [
        ("Setup", "ok", zutalk_log::Level::Info),
        ("Change", "ok", zutalk_log::Level::Info),
        ("Pause", "recording.none", zutalk_log::Level::Warn),
    ];
    for (line, (kind, code, level)) in done.iter().zip(expected) {
        assert_eq!(line.level, level, "{line:?}");
        assert_eq!(line.text("kind"), Some(kind), "{line:?}");
        assert_eq!(line.text("code"), Some(code), "{line:?}");
        assert!(line.u64("took_us").is_some(), "{line:?}");
        let mut names = line.names();
        names.sort_unstable();
        assert_eq!(names, ["code", "kind", "took_us"], "{line:?}");
    }

    for line in app.seen.all() {
        let rendered = format!("{line:?}");
        assert!(!rendered.contains(secret_title), "title leaked into log: {line:?}");
        assert!(!rendered.contains(secret_query), "query leaked into log: {line:?}");
    }
    let tally = app.core.stats().tally();
    assert_eq!(tally.counts.iter().find(|r| r.name == "commands.done").map(|r| r.n), Some(3));
    assert!(
        tally.spreads.iter().find(|r| r.name == "hub.turn_us").is_some_and(|r| r.n >= 3),
        "{:?}",
        tally.spreads
    );
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn progress_that_finds_the_mailbox_full_is_let_go() {
    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let stats = Stats::new();
    let (commands, mut mail) =
        queue::bounded::<&'static str>(&stats, "hub.mail", HUB_MAIL_MSGS, Full::Reject);
    let results = commands.with(Full::Wait);
    let progress = commands.with(Full::Drop);

    for _ in 0..HUB_MAIL_MSGS {
        commands.offer("cmd").expect("command fits until capacity");
    }

    // Progress offered while the mailbox is full is handed back at once without waiting.
    assert_eq!(progress.offer("progress"), Err(Back::Full("progress")));
    let full = seen.of("queue.full");
    assert_eq!(full.len(), 1, "{full:?}");
    assert_eq!(full[0].text("queue"), Some("hub.mail"));
    assert_eq!(full[0].text("policy"), Some("drop"));
    assert_eq!(full[0].u64("capacity"), Some(HUB_MAIL_MSGS as u64));

    // A result sent on the Full::Wait handle waits for room and arrives once the hub drains.
    let waiter = tokio::spawn(zutalk_log::carry(async move {
        results.send("result").await.expect("result arrives once room opens");
    }));
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished(), "Full::Wait sender must wait while mailbox is full");

    let mut drained = Vec::with_capacity(HUB_MAIL_MSGS + 1);
    for _ in 0..HUB_MAIL_MSGS {
        drained.push(mail.recv().await.expect("queued command"));
    }
    waiter.await.expect("result sender completes");
    drained.push(mail.recv().await.expect("result arrives after room opens"));

    assert_eq!(drained.len(), HUB_MAIL_MSGS + 1);
    assert_eq!(drained.last().copied(), Some("result"));
    let clear = seen.of("queue.clear");
    assert_eq!(clear.len(), 1, "{clear:?}");
    assert_eq!(clear[0].text("queue"), Some("hub.mail"));
    assert_eq!(clear[0].u64("lost"), Some(1));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_file_waits_for_its_tape() {
    use std::sync::{Condvar, Mutex};
    use zutalk_core::capture::clock::Block;
    use zutalk_core::capture::tape::{Reel, Reels, Writer};

    struct Gate {
        lock: Mutex<(bool, usize)>,
        cv: Condvar,
    }

    struct GatedFileReel {
        writer: Writer,
        gate: Arc<Gate>,
    }

    impl Reel for GatedFileReel {
        fn append(&mut self, block: &Block) -> std::io::Result<()> {
            {
                let mut guard = self.gate.lock.lock().expect("gate lock");
                if guard.0 {
                    guard.1 += 1;
                    let (mut guard, timed_out) = self
                        .gate
                        .cv
                        .wait_timeout_while(guard, Duration::from_secs(10), |s| s.0)
                        .expect("gate cv");
                    guard.1 = guard.1.saturating_sub(1);
                    assert!(!timed_out.timed_out() || !guard.0, "tape gate stayed closed");
                }
            }
            self.writer.append(block)
        }

        fn sync(&mut self) -> std::io::Result<()> {
            self.writer.sync()
        }
    }

    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let file = dir.path().join("Two minutes.wav");
    // 120 seconds = 1,200 blocks (double the 600-block tape queue capacity).
    wav(&file, 120_000);

    let gate = Arc::new(Gate { lock: Mutex::new((true, 0)), cv: Condvar::new() });
    let gate_cb = gate.clone();
    let reels: Reels = Arc::new(move |dir, key| {
        let writer = Writer::create(dir, key)?;
        Ok(Box::new(GatedFileReel { writer, gate: gate_cb.clone() }) as Box<dyn Reel>)
    });

    let mut app = App::with(Config { reels, ..config(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    app.send(Command::Import { path: file.to_string_lossy().into() });

    // Wait until the tape writer is blocked on the gate and the file reader has filled the queue
    // and waited for room (`Full::Wait`).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let waiting = gate.lock.lock().expect("gate lock").1 > 0;
        let q = app.core.stats().tally().queues.into_iter().find(|r| r.name == "tape");
        if waiting && q.as_ref().is_some_and(|r| r.full > 0) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "file reader did not block on full tape queue"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // No `tape` error notice or `queue.full` event while waiting.
    assert!(!app.outlet.said.now().1.codes.iter().any(|c| c == "tape"));
    assert!(app.seen.of("queue.full").iter().all(|l| l.text("queue") != Some("tape")));

    // Release the tape gate: the file import finishes cleanly with all 120,000 ms intact.
    {
        let mut guard = gate.lock.lock().expect("gate lock");
        guard.0 = false;
        gate.cv.notify_all();
    }

    let rows = app.wait_library(|rows| rows.iter().any(|r| r.length_ms > 0)).await;
    app.wait_progress(|p| p.refine.is_none()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].length_ms, 120_000);
    assert!(!app.outlet.said.now().1.codes.iter().any(|c| c == "tape"));
    assert!(
        app.seen.of("recording.end").iter().any(|l| l.text("code") == Some("done")),
        "{:?}",
        app.seen.of("recording.end")
    );
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_with_nothing_recording_says_so() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    app.send(Command::Reconnect);
    app.notice("recording.none").await;

    let done: Vec<_> = app
        .seen
        .of("command.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("Reconnect"))
        .collect();
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].text("code"), Some("recording.none"));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_starts_nothing_for_a_recording_that_never_had_captions() {
    use zutalk_core::live::wire::Dial;
    use zutalk_core::rt::supervise::End;

    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let offline_dial: Dial = Arc::new(|_| Box::pin(async { Err(End::Fatal("connect")) }));
    let mut app = App::with(Config { dial: offline_dial, ..config(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup: setup.clone() });
    app.until(|s| s.setup == setup).await;

    // Start without a key: recording is audio-only with `captions.no_key`.
    app.send(Command::Start);
    let no_key = |s: Option<&Session>| {
        s.map(|s| &s.status) == Some(&Status::AudioOnly { why: "captions.no_key".into() })
    };
    app.session(no_key).await;

    // Adding an own key mid-recording and asking to Reconnect starts no lane.
    app.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-own-test-key".into()) });
    app.until(|s| s.keys.iter().any(|(svc, from)| svc == "soniox" && *from == keys::From::Own))
        .await;
    app.send(Command::Reconnect);
    // Fence via the same mailbox so Reconnect has been handled before asserting.
    app.send(Command::Captions { show: true });
    app.until(|s| s.captions).await;

    let reconnect_done: Vec<_> = app
        .seen
        .of("command.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("Reconnect"))
        .collect();
    assert_eq!(reconnect_done.len(), 1, "{reconnect_done:?}");
    assert_eq!(reconnect_done[0].text("code"), Some("ok"));
    assert_eq!(app.seen.count("lane.reconnect"), 0);
    assert_eq!(app.seen.count("lane.open"), 0);
    assert_eq!(
        (*app.outlet.session.now().1).as_ref().map(|s| &s.status),
        Some(&Status::AudioOnly { why: "captions.no_key".into() })
    );

    // Stop and start a fresh recording: now it starts with live captions.
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while app.seen.of("recording.start").len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "second recording.start did not arrive");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let starts = app.seen.of("recording.start");
    assert_eq!(starts[0].text("captions"), Some("captions.no_key"));
    assert_eq!(starts[1].text("captions"), Some("live"));

    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    assert_eq!(app.seen.refused(), 0);
}

#[test]
fn refresh_coalescing_table_holds() {
    use zutalk_core::app::hub::next_refresh;
    assert_eq!(next_refresh(0, None, 0), None);
    assert_eq!(next_refresh(1, None, 0), Some(1));
    assert_eq!(next_refresh(5, Some(1), 0), None);
    assert_eq!(next_refresh(5, None, 1), Some(5));
    assert_eq!(next_refresh(5, None, 5), None);
}

#[tokio::test]
async fn a_burst_of_changes_is_read_back_once_or_twice() {
    use zutalk_core::record::shelf::{About, RECORDING};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let id = "rec19burst01";
    let shelf = hub::local_shelf(dir.path());
    let about = About {
        started: now,
        length_ms: 1_000,
        title: "start".into(),
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.into(), Some(about))], now).expect("seed");

    let app = App::start(dir.path());
    app.wait_library(|rows| rows.len() == 1).await;

    let (seq_before, _) = app.outlet.state.now();
    let refreshes_before =
        app.seen.of("job.done").iter().filter(|l| l.text("kind") == Some("refresh")).count();

    for i in 0..50 {
        app.send(Command::Change {
            id: id.into(),
            change: Change::Title { text: format!("title-{i}") },
        });
    }

    let rows = app.wait_library(|rows| rows.iter().any(|r| r.title == "title-49")).await;
    assert_eq!(rows[0].title, "title-49");

    // Wait for all 50 change jobs to finish.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while app.seen.of("job.done").iter().filter(|l| l.text("kind") == Some("change")).count() < 50 {
        assert!(tokio::time::Instant::now() < deadline, "50 change jobs did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (seq_after, _) = app.outlet.state.now();
    let latest_rec = app.recording(id).await.expect("recording readable");
    assert_eq!(latest_rec.row.title, "title-49");
    let state_delta = seq_after - seq_before;
    assert_eq!(state_delta, 0, "50 title edits must push 0 State updates, got {state_delta}");

    let changes: Vec<_> =
        app.seen.of("job.done").into_iter().filter(|l| l.text("kind") == Some("change")).collect();
    assert_eq!(changes.len(), 50);
    let mut prev_seq = 0u64;
    for line in &changes {
        assert_eq!(line.text("code"), Some("ok"), "{line:?}");
        let seq = line.u64("job").expect("change job seq");
        assert!(seq > prev_seq, "change seq not strictly increasing: {prev_seq} -> {seq}");
        prev_seq = seq;
    }

    let refreshes_after =
        app.seen.of("job.done").iter().filter(|l| l.text("kind") == Some("refresh")).count();
    let refresh_delta = refreshes_after - refreshes_before;
    assert_eq!(refresh_delta, 0, "expected 0 refreshes for 50 title edits, got {refresh_delta}");

    let (_, inv) = app.outlet.invalidated.now();
    assert!(
        inv.items.len() <= zutalk_core::view::query::ViewKind::ALL.len() + 1,
        "coalesced invalidation ring stayed tiny: {:?}",
        inv.items
    );

    let tally = app.core.stats().tally();
    let turn_spread = tally.spreads.iter().find(|r| r.name == "hub.turn_us");
    let slow_count = app.seen.of("hub.turn.slow").len();
    eprintln!("P1-19 burst: hub.turn_us={turn_spread:?} hub.turn.slow={slow_count}");
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn an_export_writes_what_was_changed_just_before_it() {
    use zutalk_core::record::shelf::{About, RECORDING};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let id = "rec19exp01";
    let shelf = hub::local_shelf(dir.path());
    let about = About {
        started: now,
        length_ms: 1_000,
        title: "Old Title".into(),
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.into(), Some(about))], now).expect("seed");

    let app = App::start(dir.path());
    app.wait_library(|rows| rows.len() == 1).await;

    let out = dir.path().join("fresh");
    app.send(Command::Change {
        id: id.into(),
        change: Change::Title { text: "Fresh Title".into() },
    });
    app.send(Command::Export {
        id: id.into(),
        exporter: "markdown".into(),
        languages: vec![],
        to: out.to_string_lossy().into_owned(),
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !app
        .seen
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("export.write") && l.text("code") == Some("ok"))
    {
        assert!(tokio::time::Instant::now() < deadline, "export.write did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let exported = fs::read_to_string(dir.path().join("fresh.md")).expect("exported file");
    assert!(exported.starts_with("# Fresh Title"), "expected new title, got: {exported}");
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn a_search_asked_again_answers_only_the_last() {
    use zutalk_core::live::lines::Line;
    use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let shelf = hub::local_shelf(dir.path());
    for (id, word) in [("rec19alpha", "alpha"), ("rec19beta", "beta")] {
        let line = Line { language: "en".into(), text: format!("word {word}"), ..Line::default() };
        let said = Said { line, via: [("text".to_string(), "hand".to_string())].into() };
        shelf.put(id, LINE, &[(format!("{id}.1.00000"), Some(said))], now).expect("line");
        let about = About {
            started: now,
            length_ms: 1_000,
            spoken: vec!["en".into()],
            take: 1,
            ..About::default()
        };
        shelf.put(id, RECORDING, &[(id.into(), Some(about))], now).expect("recording");
    }

    let app = App::start(dir.path());
    app.wait_library(|rows| rows.len() == 2).await;

    // Issue two searches back-to-back on the Search slot: the latest ("beta") always succeeds
    // with rec19beta, while an in-flight older search is either already done or cancelled.
    let (first, second) = tokio::join!(
        app.query(Query::Search { query: "alpha".into(), limit: 50 }),
        app.query(Query::Search { query: "beta".into(), limit: 50 }),
    );
    let second = second.expect("latest search succeeds");
    match second.value {
        QueryValue::Search(res) => {
            assert_eq!(res.hits.len(), 1, "{res:?}");
            assert_eq!(res.hits[0].recording_id, "rec19beta");
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    match first {
        Ok(reply) => {
            assert!(matches!(
                reply.value,
                QueryValue::Search(ref res) if res.hits.len() == 1 && res.hits[0].recording_id == "rec19alpha"
            ));
        }
        Err("query.cancelled") => {}
        Err(other) => panic!("unexpected first search error: {other}"),
    }
    assert!(
        app.seen
            .of("query.done")
            .iter()
            .any(|l| l.text("view") == Some("search") && l.text("code") == Some("ok")),
        "expected query.done(search, ok)"
    );
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn a_desk_that_is_full_turns_commands_away_out_loud() {
    use zutalk_core::record::shelf::{About, RECORDING};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let id = "rec19full01";
    let shelf = hub::local_shelf(dir.path());
    let about = About {
        started: now,
        length_ms: 1_000,
        title: "init".into(),
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.into(), Some(about))], now).expect("seed");

    let app = App::start(dir.path());
    app.wait_library(|rows| rows.len() == 1).await;

    for i in 0..200 {
        app.send(Command::Change {
            id: id.into(),
            change: Change::Title { text: format!("v-{i}") },
        });
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let done_changes = app
            .seen
            .of("job.done")
            .iter()
            .filter(|l| l.text("kind") == Some("change") && l.text("code") == Some("ok"))
            .count();
        let busy_cmds = app
            .seen
            .of("command.done")
            .iter()
            .filter(|l| l.text("kind") == Some("Change") && l.text("code") == Some("busy"))
            .count();
        if done_changes + busy_cmds == 200 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "desk burst did not settle: done={done_changes} busy={busy_cmds}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let about_path = dir
        .path()
        .join("records")
        .join(hub::local_did().replace(':', "-"))
        .join(id)
        .join("about.jsonl");
    let written = zu::Pack::read(&about_path).expect("about pack").records.len() - 1;
    let busy_said =
        app.seen.of("notice.said").iter().filter(|l| l.text("code") == Some("busy")).count();
    let busy_cmds = app
        .seen
        .of("command.done")
        .iter()
        .filter(|l| l.text("kind") == Some("Change") && l.text("code") == Some("busy"))
        .count();
    let done_changes = app
        .seen
        .of("job.done")
        .iter()
        .filter(|l| l.text("kind") == Some("change") && l.text("code") == Some("ok"))
        .count();

    assert!(busy_said > 0, "expected at least one busy rejection on desk");
    assert_eq!(busy_said + written, 200);
    assert_eq!(done_changes, written);
    assert_eq!(busy_cmds, busy_said);

    let full_events: Vec<_> = app
        .seen
        .of("queue.full")
        .into_iter()
        .filter(|l| l.text("queue") == Some("desk") && l.text("policy") == Some("reject"))
        .collect();
    assert!(!full_events.is_empty(), "expected queue.full(desk, reject)");

    let tally = app.core.stats().tally();
    let desk_row = tally.queues.iter().find(|r| r.name == "desk").expect("desk queue row");
    assert_eq!(desk_row.full, busy_said as u64);
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_whose_record_cannot_be_written_ends_saying_so() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    // Replace the local identity's records folder with a regular file so shelf.put fails.
    let records_dir = dir.path().join("records");
    fs::create_dir_all(&records_dir).expect("records dir");
    let local_dir = records_dir.join(hub::local_did().replace(':', "-"));
    fs::write(&local_dir, b"not-a-directory").expect("block records dir");

    app.send(Command::Start);
    app.notice("shelf").await;
    app.session(|s| s.is_none()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while app.seen.of("recording.end").is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "recording.end did not arrive");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let tape_dir = dir.path().join("tape");
    assert!(
        !tape_dir.exists() || fs::read_dir(&tape_dir).expect("tape dir").next().is_none(),
        "no tape should be left when initial record cannot be written"
    );

    let ends = app.seen.of("recording.end");
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].level, zutalk_log::Level::Error);
    assert_eq!(ends[0].text("code"), Some("shelf"));

    let phases = app.seen.of("recording.phase");
    assert!(!phases.is_empty(), "expected recording.phase events");
    assert_eq!(phases.first().and_then(|l| l.text("from")), Some("Opening"));
    assert_eq!(phases.last().and_then(|l| l.text("to")), Some("Ended"));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test]
async fn a_job_that_panics_is_one_failed_job_and_the_desk_goes_on() {
    use std::sync::atomic::AtomicU64;
    use zutalk_core::app::work::{Desk, DeskJob};
    use zutalk_core::rt::supervise::Tree;

    enum FakeJob {
        Boom,
        After,
    }
    impl DeskJob for FakeJob {
        fn kind(&self) -> &'static str {
            match self {
                FakeJob::Boom => "boom",
                FakeJob::After => "after",
            }
        }
    }

    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let stats = Stats::new();
    let root = Tree::root("test", stats.clone());
    let work_tree = root.child("work");
    let (out_tx, mut out_rx) = queue::bounded::<(&'static str, Result<(), &'static str>)>(
        &stats,
        "test.desk.out",
        8,
        Full::Wait,
    );
    let desk = Desk::start(
        &work_tree,
        Arc::new(AtomicU64::new(1)),
        |job: FakeJob| match job {
            FakeJob::Boom => panic!("simulated desk panic"),
            FakeJob::After => Ok(()),
        },
        move |kind, res| {
            let out_tx = out_tx.clone();
            async move {
                let _ = out_tx.send((kind, res)).await;
            }
        },
    );

    desk.offer(FakeJob::Boom).expect("boom queued");
    desk.offer(FakeJob::After).expect("after queued");

    let first = out_rx.recv().await.expect("first result");
    let second = out_rx.recv().await.expect("second result");
    assert_eq!(first, ("boom", Err("panic")));
    assert_eq!(second, ("after", Ok(())));

    let done = seen.of("job.done");
    assert_eq!(done.len(), 2, "{done:?}");
    assert_eq!(done[0].level, zutalk_log::Level::Warn);
    assert_eq!(done[0].text("kind"), Some("boom"));
    assert_eq!(done[0].text("code"), Some("panic"));
    assert_eq!(done[1].level, zutalk_log::Level::Info);
    assert_eq!(done[1].text("kind"), Some("after"));
    assert_eq!(done[1].text("code"), Some("ok"));

    let tally = stats.tally();
    assert_eq!(tally.counts.iter().find(|r| r.name == "jobs.failed").map(|r| r.n), Some(1));
    assert_eq!(tally.counts.iter().find(|r| r.name == "jobs.done").map(|r| r.n), Some(1));
    assert_eq!(seen.refused(), 0);
    root.cancel();
    root.wait().await;
}

#[tokio::test]
async fn no_job_says_what_it_was_about() {
    use zutalk_core::live::lines::Line;
    use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let id = "rec19priv01";
    let secret_title = "QuartzTitleMarker77";
    let secret_query = "ObsidianNeedle88";
    let secret_path = "TopazPath99";

    let shelf = hub::local_shelf(dir.path());
    let line = Line {
        language: "en".into(),
        text: format!("Has {secret_query} inside."),
        ..Line::default()
    };
    let said = Said { line, via: [("text".to_string(), "hand".to_string())].into() };
    shelf.put(id, LINE, &[(format!("{id}.1.00000"), Some(said))], now).expect("line");
    let about = About {
        started: now,
        length_ms: 1_000,
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.into(), Some(about))], now).expect("recording");

    let app = App::start(dir.path());
    app.wait_library(|rows| rows.len() == 1).await;

    let out = dir.path().join(secret_path);
    app.send(Command::Change {
        id: id.into(),
        change: Change::Title { text: secret_title.into() },
    });
    let _ = app.query(Query::Search { query: secret_query.into(), limit: 20 }).await;
    app.send(Command::Export {
        id: id.into(),
        exporter: "markdown".into(),
        languages: vec![],
        to: out.to_string_lossy().into_owned(),
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let done = app.seen.of("job.done");
        let has = |k: &str| {
            done.iter().any(|l| l.text("kind") == Some(k) && l.text("code") == Some("ok"))
        };
        if has("change") && has("export.write") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "jobs did not finish: {done:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for line in app.seen.all() {
        let rendered = format!("{line:?}");
        assert!(!rendered.contains(secret_title), "title leaked: {line:?}");
        assert!(!rendered.contains(secret_query), "query leaked: {line:?}");
        assert!(!rendered.contains(secret_path), "export path leaked: {line:?}");
    }
    assert_eq!(app.seen.refused(), 0);
}

#[test]
fn the_hub_boots_as_its_table_says() {
    use zutalk_core::app::hub::{Boot, BootEvent, boot_step};

    assert_eq!(Boot::Starting.code(), "Starting");
    assert_eq!(Boot::Ready.code(), "Ready");
    assert_eq!(Boot::Installing.code(), "Installing");

    let transitions = [
        (Boot::Starting, BootEvent::Ready, Boot::Ready),
        (Boot::Starting, BootEvent::Install, Boot::Starting),
        (Boot::Starting, BootEvent::InstallFailed, Boot::Starting),
        (Boot::Ready, BootEvent::Ready, Boot::Ready),
        (Boot::Ready, BootEvent::Install, Boot::Installing),
        (Boot::Ready, BootEvent::InstallFailed, Boot::Ready),
        (Boot::Installing, BootEvent::Ready, Boot::Installing),
        (Boot::Installing, BootEvent::Install, Boot::Installing),
        (Boot::Installing, BootEvent::InstallFailed, Boot::Ready),
    ];
    for (from, event, expected) in transitions {
        assert_eq!(boot_step(from, event), expected, "{from:?} + {event:?}");
    }

    let commands = [
        ("Start", Command::Start),
        ("Import", Command::Import { path: "a.wav".into() }),
        ("Pause", Command::Pause),
        ("Setup", Command::Setup { setup: Setup::default() }),
    ];
    for (name, cmd) in &commands {
        assert_eq!(Boot::Starting.accepts(cmd), Err("starting"), "Starting × {name}");
        assert_eq!(Boot::Ready.accepts(cmd), Ok(()), "Ready × {name}");
        let expected_installing = match cmd {
            Command::Start | Command::Import { .. } => Err("update.installing"),
            _ => Ok(()),
        };
        assert_eq!(Boot::Installing.accepts(cmd), expected_installing, "Installing × {name}");
    }
}

#[tokio::test]
async fn a_command_before_the_core_is_ready_is_turned_away() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");

    let mut app = App::start(dir.path());
    // Immediately send Start before yielding to the runtime: hub is still Starting.
    app.send(Command::Start);
    app.notice("starting").await;

    // Wait for first State (hub transitioned Starting -> Ready).
    app.until(|_| true).await;

    // Now Start succeeds.
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    let all = app.seen.all();
    for line in &all {
        if matches!(line.code, "task.start" | "boot.step" | "hub.boot" | "identity.who") {
            eprintln!("P1-20 boot log: {line:?}");
        }
    }

    let start_cmds: Vec<_> = app
        .seen
        .of("command.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("Start"))
        .collect();
    assert_eq!(start_cmds.len(), 2, "{start_cmds:?}");
    assert_eq!(start_cmds[0].level, zutalk_log::Level::Warn);
    assert_eq!(start_cmds[0].text("code"), Some("starting"));
    assert_eq!(start_cmds[1].level, zutalk_log::Level::Info);
    assert_eq!(start_cmds[1].text("code"), Some("ok"));

    let boot_events = app.seen.of("hub.boot");
    assert_eq!(boot_events.len(), 1, "{boot_events:?}");
    assert_eq!(boot_events[0].text("from"), Some("Starting"));
    assert_eq!(boot_events[0].text("to"), Some("Ready"));
    assert_eq!(boot_events[0].text("why"), Some("boot"));

    // Ordering: Starting->Ready comes before the `ok` Start command.
    let boot_idx = all.iter().position(|l| l.code == "hub.boot").expect("hub.boot index");
    let ok_start_idx = all
        .iter()
        .position(|l| {
            l.code == "command.done"
                && l.text("kind") == Some("Start")
                && l.text("code") == Some("ok")
        })
        .expect("ok Start index");
    assert!(
        boot_idx < ok_start_idx,
        "hub.boot ({boot_idx}) must precede ok Start ({ok_start_idx})"
    );

    let steps = app.seen.of("boot.step");
    for step in ["settings", "identity", "invites"] {
        assert!(
            steps.iter().any(|l| l.text("step") == Some(step) && l.text("code").is_some()),
            "missing boot.step({step}) in {steps:?}"
        );
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_list_that_never_comes_holds_up_nothing() {
    use std::sync::{Condvar, Mutex};
    use zutalk_core::app::hub::Devices;
    use zutalk_core::app::proto::Device;

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_cb = gate.clone();
    let secret_device = "SecretMicName42";
    let devices: Devices = Arc::new(move || {
        let (lock, cv) = &*gate_cb;
        let mut open = lock.lock().expect("gate lock");
        while !*open {
            let (guard, timed) = cv.wait_timeout(open, Duration::from_secs(10)).expect("gate cv");
            open = guard;
            assert!(!timed.timed_out() || *open, "devices gate stayed closed");
        }
        vec![Device { id: "dev-1".into(), name: secret_device.into(), default: true }]
    });

    let mut app = App::with(Config { devices, ..config(dir.path(), NOWHERE) });

    // Hub reaches Ready and answers commands even while `devices` is blocked.
    let first = app.until(|_| true).await;
    assert!(first.devices.is_empty());
    app.send(Command::Captions { show: true });
    app.until(|s| s.captions).await;

    assert!(
        app.seen.of("hub.boot").iter().any(|l| l.text("to") == Some("Ready")),
        "hub must reach Ready while devices is blocked"
    );
    assert!(
        !app.seen.of("boot.step").iter().any(|l| l.text("step") == Some("devices")),
        "boot.step(devices) must not appear while blocked"
    );
    let running = app.core.running();
    assert!(
        running.iter().any(|r| r.task == "devices"),
        "Tree::running() must show blocked devices task: {running:?}"
    );

    // Release gate: `boot.step(devices)` fires and `state.devices` populates; no device name in logs.
    {
        let (lock, cv) = &*gate;
        *lock.lock().expect("gate lock") = true;
        cv.notify_all();
    }
    let with_devs = app.until(|s| s.devices.len() == 1).await;
    assert_eq!(with_devs.devices[0].name, secret_device);
    let dev_steps: Vec<_> = app
        .seen
        .of("boot.step")
        .into_iter()
        .filter(|l| l.text("step") == Some("devices"))
        .collect();
    assert_eq!(dev_steps.len(), 1, "{dev_steps:?}");
    assert_eq!(dev_steps[0].text("code"), Some("ok"));
    assert_eq!(dev_steps[0].u64("n"), Some(1));

    for line in app.seen.all() {
        let rendered = format!("{line:?}");
        assert!(!rendered.contains(secret_device), "device name leaked into log: {line:?}");
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_setting_takes_effect_at_once_and_is_kept() {
    use tokio::io::duplex;
    use tokio_tungstenite::{accept_async, client_async};
    use zutalk_core::live::wire::{Dial, Wire};
    use zutalk_core::rt::supervise::End;

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");

    // In-memory duplex Soniox server that replies with one word in en and zh.
    let dial: Dial = Arc::new(|_| {
        Box::pin(async move {
            let (client_io, server_io) = duplex(64 * 1024);
            let server = tokio::spawn(async move {
                let mut ws = accept_async(server_io).await.expect("accept duplex");
                let _ = ws.next().await;
                let msg = json!({
                    "tokens": [
                        { "text": "Hello", "start_ms": 100, "end_ms": 300, "is_final": true, "language": "en", "speaker": "1" },
                        { "text": "你好", "start_ms": 100, "end_ms": 300, "is_final": true, "language": "zh", "source_language": "en", "translation_status": "translation", "speaker": "1" },
                        { "text": "<end>", "is_final": true }
                    ],
                    "final_audio_proc_ms": 400,
                    "total_audio_proc_ms": 400
                });
                let _ = ws.send(Message::text(msg.to_string())).await;
                while let Some(Ok(m)) = ws.next().await {
                    if matches!(&m, Message::Text(t) if t.is_empty()) {
                        let _ = ws.send(Message::text(r#"{"tokens":[],"finished":true}"#)).await;
                        break;
                    }
                }
            });
            let (ws, _) = client_async("ws://bench.local/setup", client_io)
                .await
                .map_err(|_| End::Retry("connect"))?;
            drop(server);
            Ok(Box::new(ws) as Wire)
        })
    });

    let mut app = App::with(Config { dial, ..config(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    let setup = Setup {
        spoken: vec!["en".into(), "zh".into()],
        shown: vec!["en".into(), "zh".into()],
        ..Setup::default()
    };
    // Send Setup and Start back-to-back without awaiting in between.
    app.send(Command::Setup { setup: setup.clone() });
    app.send(Command::Start);

    let frame = app.frame(|f| f.columns.len() == 2).await;
    assert_eq!(frame.columns.len(), 2);

    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !app
        .seen
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("settings") && l.text("code") == Some("ok"))
    {
        assert!(tokio::time::Instant::now() < deadline, "settings job did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(app);

    let mut again = App::start(dir.path());
    let restored = again.until(|_| true).await;
    assert_eq!(restored.setup, setup);
}

#[tokio::test]
async fn settings_that_cannot_be_saved_say_so_and_still_hold() {
    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");

    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    // Occupy `settings.json` with a directory so writing it fails.
    fs::create_dir_all(dir.path().join("settings.json")).expect("block settings.json");

    let chosen = Setup {
        spoken: vec!["en".into()],
        shown: vec!["en".into(), "zh".into()],
        ..Setup::default()
    };
    app.send(Command::Setup { setup: chosen.clone() });
    app.notice("settings").await;

    let (_, state) = app.outlet.state.now();
    assert_eq!(state.setup, chosen, "in-memory setup must still hold when disk save fails");

    let settings_jobs: Vec<_> = app
        .seen
        .of("job.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("settings"))
        .collect();
    assert_eq!(settings_jobs.len(), 1, "{settings_jobs:?}");
    assert_eq!(settings_jobs[0].level, zutalk_log::Level::Warn);
    assert_eq!(settings_jobs[0].text("code"), Some("settings"));
    assert!(
        app.seen.of("notice.said").iter().any(|l| l.text("code") == Some("settings")),
        "expected notice.said(settings)"
    );
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_recorded_while_an_update_is_being_put_in_place() {
    use std::sync::{Condvar, Mutex};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");

    let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let gate_cb = gate.clone();
    let install: Install = Arc::new(move || {
        let (lock, cv) = &*gate_cb;
        let mut state = lock.lock().expect("install gate");
        state.0 = true; // entered
        cv.notify_all();
        while !state.1 {
            let (guard, timed) =
                cv.wait_timeout(state, Duration::from_secs(10)).expect("install cv");
            state = guard;
            assert!(!timed.timed_out() || state.1, "install gate stayed closed");
        }
    });

    let mut app = App::with(Config { install, ..config(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    app.send(Command::Update);
    // Wait until installer blocking call has entered.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !gate.0.lock().expect("install gate").0 {
        assert!(tokio::time::Instant::now() < deadline, "install did not start");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // While installing: Start & Import are rejected with `update.installing`, while Pause still
    // responds immediately (`recording.none`), proving the hub is not blocked.
    app.send(Command::Start);
    app.notice("update.installing").await;
    app.send(Command::Import { path: "dummy.wav".into() });
    app.notice("update.installing").await;
    app.send(Command::Pause);
    app.notice("recording.none").await;

    assert!(
        app.seen.of("job.start").iter().any(|l| l.text("kind") == Some("install")),
        "expected job.start(install) while blocked"
    );
    assert!(
        !app.seen.of("job.done").iter().any(|l| l.text("kind") == Some("install")),
        "job.done(install) must not appear while blocked"
    );
    assert!(
        app.seen.of("command.done").iter().any(|l| {
            l.text("kind") == Some("Start") && l.text("code") == Some("update.installing")
        }),
        "expected command.done(Start, update.installing)"
    );

    // Release gate: installer returns (could not relaunch) -> Ready + `update.install`.
    {
        let (lock, cv) = &*gate;
        lock.lock().expect("install gate").1 = true;
        cv.notify_all();
    }
    app.notice("update.install").await;

    let boot_lines = app.seen.of("hub.boot");
    let boot_seq: Vec<(&str, &str)> = boot_lines
        .iter()
        .map(|l| (l.text("from").unwrap_or("?"), l.text("to").unwrap_or("?")))
        .collect();
    assert_eq!(boot_seq, [("Starting", "Ready"), ("Ready", "Installing"), ("Installing", "Ready")]);

    // Start works again now that Boot is back to Ready.
    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_just_set_counts_for_the_next_recording() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");

    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup });
    app.until(|s| s.setup.shown == ["en"]).await;

    // Set own Soniox key: state.keys reflects Own immediately before desk refresh.
    app.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-own-test-key".into()) });
    app.until(|s| s.keys.iter().any(|(svc, from)| svc == "soniox" && *from == keys::From::Own))
        .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !app
        .seen
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("own_key") && l.text("code") == Some("ok"))
    {
        assert!(tokio::time::Instant::now() < deadline, "own_key job did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while app.seen.of("recording.start").is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "first recording.start did not arrive");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(app.seen.of("recording.start")[0].text("captions"), Some("live"));
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    // Clear key: state.keys immediately returns to Nowhere, and next recording is audio-only.
    app.send(Command::OwnKey { service: "soniox".into(), key: None });
    app.until(|s| s.keys.iter().any(|(svc, from)| svc == "soniox" && *from == keys::From::Nowhere))
        .await;

    app.send(Command::Start);
    app.session(|s| s.is_some()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while app.seen.of("recording.start").len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "second recording.start did not arrive");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(app.seen.of("recording.start")[1].text("captions"), Some("captions.no_key"));
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_out_goes_back_to_the_device_off_the_hub() {
    use zutalk_core::record::shelf::{About, RECORDING};

    let _store = STORE.lock().await;
    hub::nobody();
    let kept = [&[5; 32][..], ME.as_bytes()].concat();
    keys::set("identity", Some(&kept)).expect("sign in person");

    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let local_id = "localrec2001";
    let local_about = About {
        started: now,
        length_ms: 1_000,
        title: "Device Note".into(),
        spoken: vec!["en".into()],
        take: 1,
        ..About::default()
    };
    hub::local_shelf(dir.path())
        .put(local_id, RECORDING, &[(local_id.into(), Some(local_about))], now)
        .expect("seed local");

    let mut app = App::start(dir.path());
    app.until(|s| s.me.as_deref() == Some(ME)).await;
    // P4-03: first sign-in adopts `local_id` into `ME`'s shelf while leaving local shelf intact.
    let adopted = app.wait_library(|rows| rows.iter().any(|r| r.id == local_id)).await;
    assert_eq!(adopted.len(), 1);

    app.send(Command::SignOut);
    app.until(|s| s.me.is_none()).await;
    let back = app.wait_library(|rows| rows.iter().any(|r| r.id == local_id)).await;
    assert_eq!(back.len(), 1);

    let who: Vec<_> = app
        .seen
        .of("identity.who")
        .into_iter()
        .filter(|l| l.text("why") == Some("sign_out"))
        .collect();
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].text("from"), Some("person"));
    assert_eq!(who[0].text("to"), Some("local"));

    let signout_jobs: Vec<_> = app
        .seen
        .of("job.done")
        .into_iter()
        .filter(|l| l.text("kind") == Some("signout") && l.text("code") == Some("ok"))
        .collect();
    assert_eq!(signout_jobs.len(), 1, "{signout_jobs:?}");

    let local_did = hub::local_did();
    for line in app.seen.all() {
        let rendered = format!("{line:?}");
        assert!(!rendered.contains(ME), "person DID leaked into log: {line:?}");
        assert!(!rendered.contains(&local_did), "local DID leaked into log: {line:?}");
        assert!(!rendered.contains("did:zu:"), "did:zu leaked into log: {line:?}");
        assert!(!rendered.contains("did:key:"), "did:key leaked into log: {line:?}");
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tape_that_cannot_be_read_fails_the_job_with_a_code() {
    use zutalk_core::record::shelf::{About, RECORDING};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("a folder");
    let now = wall_ms() as i64;
    let id = "rec21badtape";
    let shelf = hub::local_shelf(dir.path());
    let about = About {
        started: now,
        length_ms: 2_000,
        title: "Corrupt tape".into(),
        spoken: vec!["en".into()],
        shown: vec!["en".into()],
        take: 0,
        ..About::default()
    };
    shelf.put(id, RECORDING, &[(id.into(), Some(about))], now).expect("seed draft");
    hub::tape(dir.path(), id, 2);

    // Corrupt ciphertext inside the tape so decryption fails deterministically without touching the network.
    let tape_file = std::fs::read_dir(dir.path().join("tape").join(id))
        .expect("read tape dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("tape"))
        .expect("at least one .tape file");
    let mut bytes = std::fs::read(&tape_file).expect("read tape");
    assert!(!bytes.is_empty());
    bytes[0] ^= 0xff;
    std::fs::write(&tape_file, &bytes).expect("corrupt tape");

    let mut app = App::start(dir.path());
    app.notice("tape").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !app
        .seen
        .of("job.done")
        .iter()
        .any(|l| l.text("kind") == Some("write") && l.text("code") == Some("tape"))
    {
        assert!(tokio::time::Instant::now() < deadline, "expected job.done(write, tape)");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diagnosis_says_who_signs_and_never_which_did() {
    let _store = STORE.lock().await;
    hub::signed_in();
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let state = app.until(|s| s.me.is_some()).await;
    let person_did = state.me.expect("signed-in person DID");
    let local_did = hub::local_did();

    app.send(Command::Diagnose);
    app.notice("diagnose.done").await;

    let base = dir.path().join("diagnose");
    let bundles: Vec<_> =
        std::fs::read_dir(&base).expect("read diagnose dir").flatten().map(|e| e.path()).collect();
    assert_eq!(bundles.len(), 1, "expected one diagnose bundle");
    let bundle = &bundles[0];

    let about: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(bundle.join("about.json")).expect("read about.json"),
    )
    .expect("parse about.json");
    assert_eq!(about["who"].as_str(), Some("person"));

    fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk_files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk_files(bundle, &mut files);
    assert!(!files.is_empty());
    for file in files {
        let raw = std::fs::read_to_string(&file).expect("read bundle file");
        assert!(!raw.contains(&person_did), "person DID leaked into {}", file.display());
        assert!(!raw.contains(&local_did), "local DID leaked into {}", file.display());
        assert!(!raw.contains("did:zu:"), "did:zu leaked into {}", file.display());
        assert!(!raw.contains("did:key:"), "did:key leaked into {}", file.display());
    }
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_caption_prefs_and_geometry_persist_per_scene_in_settings_json() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");

    // 1. Legacy settings.json without `caption` or `city` loads defaults cleanly.
    fs::write(
        dir.path().join("settings.json"),
        r#"{"spoken":["en"],"shown":["en","zh"],"sound":"Mic","device":null,"mode":"Audience","language":null,"keep":null}"#,
    )
    .expect("write legacy settings");
    let mut app = App::start(dir.path());
    let booted = app.until(|_| true).await;
    assert_eq!(booted.setup.spoken, vec!["en"]);
    assert_eq!(booted.setup.caption.scene, CaptionScene::SelfView);
    assert!(booted.setup.caption.scenes.band.always_on_top);
    assert!(booted.setup.caption.scenes.band.top);
    assert!(!booted.setup.caption.scenes.band.click_through);
    assert!(booted.setup.caption.scenes.stage.fullscreen);
    assert_eq!(booted.setup.city, None);

    // 2. Switching scene and mode independently clamps lines, font_px, opacity, and stores city.
    let mut next = booted.setup.clone();
    next.mode = Mode::Conversation;
    next.caption.scene = CaptionScene::Band;
    next.caption.scenes.band.lines = 99;
    next.caption.scenes.band.font_px = Some(999);
    next.caption.scenes.band.opacity = Some(5);
    next.caption.scenes.band.size = CaptionSize::Large;
    next.caption.scenes.band.bg = CaptionBg::Clear;
    next.city = Some("zuchiangmai.com".into());
    app.send(Command::Setup { setup: next });
    let after_setup = app.until(|s| s.setup.caption.scene == CaptionScene::Band).await;
    assert_eq!(after_setup.setup.mode, Mode::Conversation);
    assert_eq!(after_setup.setup.caption.scenes.band.lines, 8);
    assert_eq!(after_setup.setup.caption.scenes.band.font_px, Some(160));
    assert_eq!(after_setup.setup.caption.scenes.band.opacity, Some(20));
    assert_eq!(after_setup.setup.caption.scenes.band.size, CaptionSize::Large);
    assert_eq!(after_setup.setup.caption.scenes.band.bg, CaptionBg::Clear);
    assert_eq!(after_setup.setup.city.as_deref(), Some("zuchiangmai.com"));

    // 3. CaptionGeometry updates only the named scene's geometry (clamping degenerate values)
    //    without overwriting other scenes or concurrent settings fields.
    app.send(Command::CaptionGeometry {
        scene: CaptionScene::Band,
        rect: WindowRect { x: 40, y: 820, width: 1200, height: 140 },
    });
    app.send(Command::CaptionGeometry {
        scene: CaptionScene::SelfView,
        rect: WindowRect { x: 10, y: 20, width: 50, height: 20 },
    });
    let after_geo = app
        .until(|s| {
            s.setup.caption.scenes.band.geometry.is_some()
                && s.setup.caption.scenes.self_view.geometry.is_some()
        })
        .await;
    assert_eq!(
        after_geo.setup.caption.scenes.band.geometry,
        Some(WindowRect { x: 40, y: 820, width: 1200, height: 140 })
    );
    assert_eq!(
        after_geo.setup.caption.scenes.self_view.geometry,
        Some(WindowRect { x: 10, y: 20, width: 240, height: 80 })
    );
    assert_eq!(after_geo.setup.caption.scenes.stage.geometry, None);
    assert_eq!(after_geo.setup.city.as_deref(), Some("zuchiangmai.com"));

    // Wait for desk to finish flushing settings.json before restarting.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while app
        .seen
        .of("job.done")
        .iter()
        .filter(|l| l.text("kind") == Some("settings") && l.text("code") == Some("ok"))
        .count()
        < 3
    {
        assert!(tokio::time::Instant::now() < deadline, "settings jobs did not flush");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(app);

    // 4. Restart Core and verify every scene's style, geometry, mode, and city survive reload.
    let mut again = App::start(dir.path());
    let reloaded = again.until(|_| true).await;
    assert_eq!(reloaded.setup, after_geo.setup);
    assert_eq!(reloaded.setup.mode, Mode::Conversation);
    assert_eq!(reloaded.setup.city.as_deref(), Some("zuchiangmai.com"));
    assert_eq!(again.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_transcript_copies_entire_multichunk_take_off_hub() {
    use zutalk_core::live::lines::Line;
    use zutalk_core::record::shelf::{About, LINE, RECORDING, SPEAKER, Said, Words};

    let _store = STORE.lock().await;
    hub::nobody();
    let dir = tempfile::tempdir().expect("tempdir");
    let now = wall_ms() as i64;
    let local = hub::local_shelf(dir.path());
    let rec_id = "19c00000001copyfull";

    // Seed 200 lines (> 3 transcript chunks of 64) on take 1 and 1 draft line on take 0.
    local
        .put(
            rec_id,
            LINE,
            &[(
                format!("{rec_id}.0.00000"),
                Some(Said {
                    line: Line {
                        t0: 0,
                        t1: 500,
                        speaker: "1".into(),
                        language: "en".into(),
                        text: "draft only line".into(),
                        ..Line::default()
                    },
                    via: BTreeMap::new(),
                }),
            )],
            now,
        )
        .unwrap();
    let mut take1 = Vec::with_capacity(200);
    for i in 0..200u32 {
        let mut translations = BTreeMap::new();
        translations.insert("zh".into(), format!("第{i}行译文"));
        take1.push((
            format!("{rec_id}.1.{i:05}"),
            Some(Said {
                line: Line {
                    t0: i * 1_000,
                    t1: i * 1_000 + 800,
                    speaker: "1".into(),
                    language: "en".into(),
                    text: format!("Line number {i}"),
                    translations,
                    ..Line::default()
                },
                via: BTreeMap::new(),
            }),
        ));
    }
    local.put(rec_id, LINE, &take1, now + 1).unwrap();
    local
        .put(rec_id, SPEAKER, &[(format!("{rec_id}.1"), Some(Words { text: "Alice".into() }))], now)
        .unwrap();
    local
        .put(
            rec_id,
            RECORDING,
            &[(
                rec_id.into(),
                Some(About {
                    started: now,
                    length_ms: 200_000,
                    title: "Long Session".into(),
                    spoken: vec!["en".into()],
                    shown: vec!["en".into(), "zh".into()],
                    take: 1,
                    ..About::default()
                }),
            )],
            now + 2,
        )
        .unwrap();

    let copied = Arc::new(std::sync::Mutex::new(String::new()));
    let copied_out = copied.clone();
    let mut cfg = hub::config(dir.path());
    cfg.clip = Arc::new(move |text| {
        *copied_out.lock().unwrap() = text.to_owned();
        true
    });

    let mut app = App::with(cfg);
    app.until(|_| true).await;
    app.wait_library(|r| r.len() == 1).await;

    app.send(Command::CopyTranscript {
        id: rec_id.into(),
        take: Some(1),
        languages: vec!["zh".into()],
    });
    app.notice("copied").await;

    let got = copied.lock().unwrap().clone();
    assert!(got.contains("Alice 0:00\nLine number 0\n第0行译文"), "missing head: {got}");
    assert!(got.contains("Alice 3:19\nLine number 199\n第199行译文"), "missing tail #199: {got}");
    assert!(!got.contains("draft only line"));

    // Selecting take 0 explicitly copies take 0.
    app.send(Command::CopyTranscript { id: rec_id.into(), take: Some(0), languages: vec![] });
    app.notice("copied").await;
    assert!(copied.lock().unwrap().contains("draft only line"));
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_recording_while_lane_dial_hangs_seals_immediately_and_preserves_newer_active() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let _store = STORE.lock().await;
    hub::signed_in();
    let soniox = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("ws port");
    let ws_url = format!("ws://{}", soniox.local_addr().expect("ws addr"));

    tokio::spawn(async move {
        while let Ok((stream, _)) = soniox.accept().await {
            let Ok(mut ws) = accept_async(stream).await else { continue };
            let _ = ws.next().await;
            let tok = json!({
                "text": "live rec2 line",
                "start_ms": 100,
                "end_ms": 450,
                "is_final": true,
                "language": "en",
                "speaker": "1",
            });
            let said = json!({
                "tokens": [tok, { "text": "<end>", "is_final": true }],
                "total_audio_proc_ms": 500,
            });
            let _ = ws.send(Message::text(said.to_string())).await;
            tokio::spawn(async move {
                while let Some(Ok(msg)) = ws.next().await {
                    if matches!(&msg, Message::Text(t) if t.is_empty()) {
                        let _ = ws.send(Message::text(r#"{"tokens":[],"finished":true}"#)).await;
                        break;
                    }
                }
            });
        }
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = hub::config(dir.path());
    cfg.realtime = ws_url;
    let default_dial = cfg.dial.clone();
    let dials = Arc::new(AtomicUsize::new(0));
    let dials_cb = dials.clone();
    // Recording 1's WebSocket dial stalls for 8 seconds (hung Wi-Fi); Recording 2 connects normally.
    cfg.dial = Arc::new(move |endpoint| {
        let n = dials_cb.fetch_add(1, Ordering::AcqRel);
        let default_dial = default_dial.clone();
        let endpoint = endpoint.to_owned();
        Box::pin(async move {
            if n == 0 {
                tokio::time::sleep(Duration::from_millis(8_000)).await;
                Err(zutalk_core::rt::supervise::End::Retry("connect"))
            } else {
                (default_dial)(&endpoint).await
            }
        })
    });

    let mut app = App::with(cfg);
    app.until(|s| s.me.is_some()).await;
    app.send(Command::Setup {
        setup: Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() },
    });
    app.until(|s| s.setup.shown == vec!["en".to_string()]).await;

    app.send(Command::Start);
    let rec1 = app.session(|s| s.is_some()).await.expect("rec1").id;
    let dial_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while dials.load(Ordering::Acquire) == 0 {
        assert!(tokio::time::Instant::now() < dial_deadline, "rec1 did not enter dial");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(350)).await;

    let stop_at = tokio::time::Instant::now();
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;

    // Immediately start Recording 2 while Recording 1 is sealing.
    app.send(Command::Start);
    let rec2 = app.session(|s| s.is_some()).await.expect("rec2").id;
    assert_ne!(rec1, rec2);

    // Recording 1 must abort the hung lane dial immediately on Stop and seal in < 1.5 s (not 8 s!).
    let sealed_rows = tokio::time::timeout(
        Duration::from_millis(1_500),
        app.wait_library(|rows| rows.iter().any(|r| r.id == rec1 && r.length_ms > 0)),
    )
    .await
    .expect("Stop must cancel in-flight lane dial and seal promptly within 1.5 s");
    assert!(stop_at.elapsed() < Duration::from_millis(1_500));
    assert!(sealed_rows.iter().any(|r| r.id == rec1 && r.length_ms > 0));

    // Recording 1 sealing must NOT clobber Recording 2's active live-draft status: Query::Transcript
    // must stream Recording 2's live draft lines written after Recording 1 sealed.
    let live_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let rec2_chunk = loop {
        let chunk = app.transcript(&rec2, Some(0), 0).await;
        if chunk.total_lines >= 1 {
            break chunk;
        }
        assert!(
            tokio::time::Instant::now() < live_deadline,
            "sealing rec1 must not wipe rec2 active live-draft mode"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(rec2_chunk.lines[0].1.line.text, "live rec2 line");

    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_reconnect_uses_heard_watermark_not_sent_for_gap_origin() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _store = STORE.lock().await;
    hub::signed_in();

    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("ws port");
    let ws_addr = ws_listener.local_addr().expect("ws addr");
    let healthy = Arc::new(AtomicBool::new(false));
    let healthy_srv = healthy.clone();

    tokio::spawn(async move {
        let mut conn_num = 0usize;
        while let Ok((stream, _)) = ws_listener.accept().await {
            let Ok(mut ws) = accept_async(stream).await else { continue };
            conn_num += 1;
            // Read initial config frame.
            let _ = ws.next().await;
            if conn_num == 1 {
                // Read 12 binary blocks (`sent >= 1_200`), acknowledge only 400 ms (`heard = 400`),
                // then return a fatal key error so lane 0 stops with `ended = Some("key")`.
                let mut blocks = 0usize;
                while blocks < 12 {
                    match ws.next().await {
                        Some(Ok(Message::Binary(_))) => blocks += 1,
                        Some(Ok(_)) => {}
                        _ => break,
                    }
                }
                let _ = ws
                    .send(Message::Text(
                        json!({ "tokens": [], "total_audio_proc_ms": 400 }).to_string().into(),
                    ))
                    .await;
                let _ = ws
                    .send(Message::Text(
                        json!({ "error_code": 401, "error_type": "invalid_api_key" })
                            .to_string()
                            .into(),
                    ))
                    .await;
                let _ = ws.close(None).await;
            } else if !healthy_srv.load(Ordering::Acquire) {
                let _ = ws
                    .send(Message::Text(
                        json!({ "error_code": 401, "error_type": "invalid_api_key" })
                            .to_string()
                            .into(),
                    ))
                    .await;
                let _ = ws.close(None).await;
            } else {
                while let Some(Ok(msg)) = ws.next().await {
                    if let Message::Binary(_) = msg {
                        let _ = ws
                            .send(Message::Text(
                                json!({ "tokens": [], "total_audio_proc_ms": 2_000 })
                                    .to_string()
                                    .into(),
                            ))
                            .await;
                    }
                }
            }
        }
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = hub::config(dir.path());
    cfg.realtime = format!("ws://{ws_addr}");
    let mut app = App::with(cfg);
    app.until(|s| s.me.is_some()).await;
    // Use 1 column (`Transcribe`) so there is a single deterministic lane 0.
    app.send(Command::Setup {
        setup: Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() },
    });
    app.until(|s| s.setup.shown == vec!["en".to_string()]).await;

    app.send(Command::Start);
    app.session(|s| s.is_some()).await;

    // Wait until lane 0 stops with `why = "key"` after sending >= 1_200 ms while `heard == 400`.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !app
        .seen
        .of("lane.link")
        .iter()
        .any(|l| l.text("to") == Some("Stopped") && l.text("why") == Some("key"))
    {
        assert!(tokio::time::Instant::now() < deadline, "lane 0 did not stop on key error");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    healthy.store(true, Ordering::Release);
    app.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-fresh".into()) });
    app.send(Command::Reconnect);

    // Wait for `lane.gap` on reconnect and verify `t0_ms == 400` (`heard`), NOT `>= 1_200` (`sent`)!
    let gap_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let gap_t0 = loop {
        if let Some(gap) = app.seen.of("lane.gap").last()
            && let Some(t0) = gap.u64("t0_ms")
        {
            break t0;
        }
        assert!(tokio::time::Instant::now() < gap_deadline, "expected lane.gap after Reconnect");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(gap_t0, 400, "manual Reconnect must start gap at heard_at (400), got {gap_t0}");

    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
}
