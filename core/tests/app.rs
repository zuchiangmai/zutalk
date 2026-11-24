mod common;
mod hub;

use common::{Plc, Viewer, old_library, serve, serve_with};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use zutalk_core::ai::write::wav_header;
use zutalk_core::app::hub::{Clip, Config, Core, Install, status};
use zutalk_core::app::proto::{Change, Command, Event, Setup, State, Status};
use zutalk_core::capture::audio::{Capture, Feed, Keep, Notice, Open, Source, offer};
use zutalk_core::capture::clock::{Block, Gap, wall_ms};
use zutalk_core::id::keys;
use zutalk_core::live::lane::Health;
use zutalk_core::rt::supervise::End;
use zutalk_server::Invites;

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
/// Nothing listens here: every service is out of reach.
const NOWHERE: &str = "https://127.0.0.1:9";

/// The key store is the process's: one test at a time has it.
static STORE: Mutex<()> = Mutex::const_new(());

fn store(signed_in: bool) {
    keyring_core::set_default_store(keyring_core::mock::Store::new().expect("a mock store"));
    if signed_in {
        let kept = [&[5; 32][..], ME.as_bytes()].concat();
        keys::set("identity", Some(&kept)).expect("the identity is kept");
    }
}

/// A microphone that hums, in real time, until it is closed. A file is read as it is.
fn hum(
    source: Source,
    sink: Box<dyn FnMut(Block) + Send>,
    notice: Box<dyn FnMut(Notice) + Send>,
) -> Result<Capture, End> {
    if let Source::File(_) = source {
        return Capture::start(source, |_| Gap::Keep, sink, notice);
    }
    let open: Open = Box::new(|_| {
        let (mut ring, feed) = Feed::new(16_000, 1)?;
        thread::spawn(move || {
            while !ring.is_abandoned() {
                offer(&mut ring, (0..320).map(|i| (i as f32 * 0.17).sin() * 0.3));
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok((feed, Box::new(|| None) as Keep))
    });
    Ok(Capture::with(vec![open], |_| Gap::Keep, wall_ms, sink, notice))
}

struct App {
    core: Core,
    events: UnboundedReceiver<Event>,
}

impl App {
    fn start(data: &Path) -> App {
        App::on(data, NOWHERE, "ws://127.0.0.1:9")
    }

    /// With our server at `server`, and the realtime service at `realtime`.
    fn on(data: &Path, server: &str, realtime: &str) -> App {
        App::with(Config { realtime: realtime.into(), ..config(data, server) })
    }

    fn with(config: Config) -> App {
        let (tx, events) = unbounded_channel();
        let core = Core::start(config, move |event| drop(tx.send(event))).expect("the core starts");
        App { core, events }
    }

    fn send(&self, command: Command) {
        self.core.send(command);
    }

    /// Waits for an event that `wanted` takes.
    async fn next<T>(&mut self, mut wanted: impl FnMut(Event) -> Option<T>) -> T {
        let wait = async {
            loop {
                if let Some(found) = wanted(self.events.recv().await.expect("the core runs")) {
                    return found;
                }
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("it came in time")
    }

    async fn until(&mut self, holds: impl Fn(&State) -> bool) -> State {
        self.next(|event| match event {
            Event::State(state) if holds(&state) => Some(*state),
            _ => None,
        })
        .await
    }

    async fn notice(&mut self, code: &str) {
        self.next(|event| matches!(event, Event::Notice(said) if said == code).then_some(())).await
    }
}

/// With our server at `server`, nothing else in reach, and a microphone that hums.
fn config(data: &Path, server: &str) -> Config {
    let (soniox, openrouter, directory) = (NOWHERE.into(), NOWHERE.into(), NOWHERE.into());
    Config { soniox, openrouter, directory, hear: hum, ..Config::new(data.into(), server) }
}

fn wav(path: &Path, ms: u32) {
    let samples = ms * 16;
    let pcm = (0..samples).flat_map(|i| (((i as f32 * 0.05).sin() * 8_000.0) as i16).to_le_bytes());
    fs::write(path, [wav_header(samples), pcm.collect()].concat()).expect("the file is written");
}

#[test]
fn the_status_line_says_what_most_needs_saying() {
    use Health::{Behind, Connecting, Live, Stopped};
    let only = |why: &str| Status::AudioOnly { why: why.into() };
    assert_eq!(status(true, &[Connecting], "captions.off"), Status::Paused);
    assert_eq!(status(false, &[], "captions.off"), only("captions.off"));
    assert_eq!(status(false, &[], "captions.no_key"), only("captions.no_key"));
    assert_eq!(status(false, &[Stopped("keys.none"), Stopped("done")], ""), only("keys.none"));
    assert_eq!(status(false, &[Live, Connecting], ""), Status::Connecting);
    assert_eq!(status(false, &[Behind, Live], ""), Status::Behind);
    assert_eq!(status(false, &[Live, Stopped("key")], ""), Status::Recording);
}

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

/// Waits for a `State` matching `holds`, failing if `"sign_in.first"` is noticed along the way.
async fn until_no_sign_in_first(app: &mut App, holds: impl Fn(&State) -> bool) -> State {
    app.next(|event| match event {
        Event::Notice(code) if code == "sign_in.first" => panic!("unexpected sign_in.first"),
        Event::State(state) if holds(&state) => Some(*state),
        _ => None,
    })
    .await
}

#[test]
fn commands_that_need_a_person_are_listed_in_one_table() {
    let cases = [
        (Command::Start, false),
        (Command::Import { path: "a.wav".into() }, false),
        (Command::Pause, false),
        (Command::Resume, false),
        (Command::Stop, false),
        (Command::Setup { setup: Setup::default() }, false),
        (Command::Open { id: "id".into() }, false),
        (Command::Close, false),
        (Command::Change { id: "id".into(), change: Change::Title { text: "t".into() } }, false),
        (Command::Remove { id: "id".into() }, false),
        (Command::Restore { id: "id".into() }, false),
        (Command::Search { query: "q".into() }, false),
        (
            Command::Export {
                id: "id".into(),
                exporter: "markdown".into(),
                languages: vec![],
                to: "out.md".into(),
            },
            false,
        ),
        (Command::Share { recording: None }, true),
        (Command::Share { recording: Some("id".into()) }, true),
        (Command::Unshare { room: "r1".into() }, true),
        (Command::SignIn, false),
        (Command::SignOut, false),
        (Command::RotationSaved, false),
        (Command::CopyRotation, false),
        (Command::Update, false),
        (Command::OwnKey { service: "soniox".into(), key: None }, false),
        (Command::Captions { show: true }, false),
        (Command::DestroyAudio { id: "id".into() }, false),
        (Command::Term { said: "ZuTalk".into(), written: None }, false),
        (Command::City { address: "pol.example".into() }, false),
        (Command::File { id: "id".into(), event: "e1".into(), audio: true }, true),
    ];
    for (command, expected) in cases {
        assert_eq!(command.needs_person(), expected, "{command:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_needs_nobody_signed_in() {
    let _store = STORE.lock().await;
    store(false);
    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let state = until_no_sign_in_first(&mut app, |_| true).await;
    assert!(state.me.is_none() && state.library.is_empty());
    assert_eq!(state.languages.len(), 8);
    assert_eq!(state.exporters, ["markdown", "srt", "vtt"]);

    app.send(Command::Start);
    until_no_sign_in_first(&mut app, |s| s.session.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    let state = until_no_sign_in_first(&mut app, |s| {
        s.session.is_none() && s.writing.is_empty() && s.library.iter().any(|r| r.length_ms > 0)
    })
    .await;
    assert_eq!(state.library.len(), 1);
    let id = state.library[0].id.clone();

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
    until_no_sign_in_first(&mut app, |s| s.library.iter().any(|r| r.title == "Local note")).await;
    app.send(Command::Search { query: "note".into() });
    let out = dir.path().join("exported");
    app.send(Command::Export {
        id: id.clone(),
        exporter: "markdown".into(),
        languages: vec![],
        to: out.to_string_lossy().into(),
    });
    app.send(Command::Remove { id: id.clone() });
    until_no_sign_in_first(&mut app, |s| s.library.is_empty()).await;
    app.send(Command::Restore { id: id.clone() });
    until_no_sign_in_first(&mut app, |s| s.library.len() == 1).await;
    let exported = fs::read_to_string(dir.path().join("exported.md")).expect("exported");
    assert!(exported.starts_with("# Local note"), "{exported}");

    // Observable: one `identity.who` event, and no `did:key` anywhere in the captured log.
    let who = seen.of("identity.who");
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].level, zutalk_log::Level::Info);
    assert_eq!(who[0].text("from"), Some("nobody"));
    assert_eq!(who[0].text("to"), Some("local"));
    assert_eq!(who[0].text("why"), Some("boot"));
    assert_eq!(who[0].bool("made"), Some(true));
    let multikey = local_did.strip_prefix("did:key:").expect("did:key prefix");
    for line in seen.all() {
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
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::SignIn);
    app.until(|state| state.login.is_some()).await;
    app.notice("offline").await;
    app.until(|state| state.login.is_none()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_device_is_the_same_identity_the_next_time() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.until(|s| s.session.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    let id = app
        .until(|s| {
            s.session.is_none() && s.writing.is_empty() && s.library.iter().any(|r| r.length_ms > 0)
        })
        .await
        .library[0]
        .id
        .clone();
    drop(app);

    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let mut again = App::start(dir.path());
    let state = again.until(|s| s.library.len() == 1).await;
    assert_eq!(state.library[0].id, id);
    assert_eq!(record_dirs(dir.path()).len(), 1);

    let who = seen.of("identity.who");
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].text("from"), Some("nobody"));
    assert_eq!(who[0].text("to"), Some("local"));
    assert_eq!(who[0].text("why"), Some("boot"));
    assert_eq!(who[0].bool("made"), Some(false));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.until(|s| s.session.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    let id = app
        .until(|s| {
            s.session.is_none() && s.writing.is_empty() && s.library.iter().any(|r| r.length_ms > 0)
        })
        .await
        .library[0]
        .id
        .clone();
    drop(app);

    let local_dirs = record_dirs(dir.path());
    assert_eq!(local_dirs.len(), 1);
    let local_dir = dir.path().join("records").join(&local_dirs[0]);
    let before = snapshot(&local_dir);
    assert!(!before.is_empty());

    // Add a signed-in identity into the same key store so the device identity seed stays intact.
    let kept = [&[5; 32][..], ME.as_bytes()].concat();
    keys::set("identity", Some(&kept)).expect("the identity is kept");

    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let mut app = App::start(dir.path());
    let signed = app.until(|s| s.me.as_deref() == Some(ME)).await;
    assert!(signed.library.is_empty());

    app.send(Command::SignOut);
    let back = app.until(|s| s.me.is_none() && s.library.len() == 1 && s.writing.is_empty()).await;
    assert_eq!(back.library[0].id, id);
    assert_eq!(snapshot(&local_dir), before, "local shelf changed across sign-in and sign-out");

    let sign_out: Vec<_> = seen
        .of("identity.who")
        .into_iter()
        .filter(|line| line.text("why") == Some("sign_out"))
        .collect();
    assert_eq!(sign_out.len(), 1, "{:?}", seen.of("identity.who"));
    assert_eq!(sign_out[0].text("from"), Some("person"));
    assert_eq!(sign_out[0].text("to"), Some("local"));
    assert_eq!(sign_out[0].bool("made"), Some(false));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_needs_a_person_says_so_and_writes_nothing() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;
    app.send(Command::Start);
    app.until(|s| s.session.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    let id = app
        .until(|s| {
            s.session.is_none() && s.writing.is_empty() && s.library.iter().any(|r| r.length_ms > 0)
        })
        .await
        .library[0]
        .id
        .clone();

    for cmd in [
        Command::Share { recording: None },
        Command::Share { recording: Some(id.clone()) },
        Command::Unshare { room: "r1".into() },
        Command::File { id: id.clone(), event: "e1".into(), audio: true },
    ] {
        app.send(cmd);
        app.notice("sign_in.first").await;
    }

    let dirs = record_dirs(dir.path());
    assert_eq!(dirs.len(), 1);
    let local_dir = dir.path().join("records").join(&dirs[0]);
    assert!(!local_dir.join("shares.jsonl").exists());
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
    let id = app.until(|s| s.session.is_some()).await.session.expect("session").id;
    tokio::time::sleep(Duration::from_millis(500)).await;
    app.send(Command::SignOut);
    app.until(|s| s.me.is_none()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    app.send(Command::Stop);
    // The task started under the person's shelf and keys: once `writing` reports "offline",
    // `seal()` has already written the sealed length record to that shelf.
    let state = app
        .until(|s| {
            s.session.is_none() && s.writing.iter().any(|w| w.id == id && w.step == "offline")
        })
        .await;
    assert!(state.library.is_empty(), "the local shelf must not hold the person's recording");

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
    app.until(|s| s.session.is_some()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    app.send(Command::Stop);
    let id = app
        .until(|s| {
            s.session.is_none() && s.writing.is_empty() && s.library.iter().any(|r| r.length_ms > 0)
        })
        .await
        .library[0]
        .id
        .clone();

    app.send(Command::Change { id: id.clone(), change: Change::Title { text: "Quiet".into() } });
    app.until(|s| s.library.iter().any(|r| r.title == "Quiet")).await;
    app.send(Command::Search { query: "Quiet".into() });
    let out = dir.path().join("quiet.md");
    app.send(Command::Export {
        id,
        exporter: "markdown".into(),
        languages: vec![],
        to: out.to_string_lossy().into(),
    });
    app.send(Command::Share { recording: None });
    app.notice("sign_in.first").await;
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

    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.notice("keys.store").await;
    let state = app.until(|_| true).await;
    assert!(state.me.is_none() && state.library.is_empty());

    app.send(Command::Start);
    app.notice("keys.store").await;

    let who = seen.of("identity.who");
    assert_eq!(who.len(), 1, "{who:?}");
    assert_eq!(who[0].level, zutalk_log::Level::Warn);
    assert_eq!(who[0].text("from"), Some("nobody"));
    assert_eq!(who[0].text("to"), Some("nobody"));
    assert_eq!(who[0].text("why"), Some("boot"));
    assert_eq!(who[0].bool("made"), Some(false));
    assert_eq!(seen.refused(), 0);
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
    let state = app.until(|state| state.library.len() == 3 && state.moving.is_none()).await;
    assert!(state.unmoved.is_empty());
    let titles: Vec<&str> = state.library.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(titles, ["interview", "Weekly sync", ""]);
    assert_eq!(state.library.iter().map(|row| row.draft).collect::<Vec<_>>(), [false, true, false]);
    drop(app);
    // Again: nothing moves twice, and the live draft from 0.8, audio and all, is owed nothing.
    let mut app = start();
    let again = app.until(|state| !state.library.is_empty()).await;
    assert_eq!((again.library, again.writing), (state.library, vec![]));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rest: Vec<Event> = std::iter::from_fn(|| app.events.try_recv().ok()).collect();
    let writes = |event: &Event| matches!(event, Event::State(s) if !s.writing.is_empty());
    assert!(!rest.iter().any(writes), "{rest:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_stops_at_once_and_is_kept_whole() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let setup = Setup { spoken: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup: setup.clone() });
    app.send(Command::Start);
    let audio_only = |state: &State| {
        let status = state.session.as_ref().map(|session| &session.status);
        status == Some(&Status::AudioOnly { why: "captions.off".into() })
    };
    let state = app.until(audio_only).await;
    assert_eq!(state.me.as_deref(), Some(ME));
    app.send(Command::Pause);
    app.until(|state| state.session.as_ref().is_some_and(|s| s.status == Status::Paused)).await;
    app.send(Command::Resume);
    app.until(audio_only).await;
    app.send(Command::Start);
    app.notice("recording.under_way").await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    app.send(Command::Stop);
    let state = app.until(|state| state.session.is_none()).await;
    assert_eq!(state.writing.first().map(|w| w.step.as_str()), Some("closing"));
    // The tape is sealed behind the stop, and the transcript waits for a key.
    let waits = |state: &State| state.writing.iter().any(|w| w.step == "offline");
    let row = app.until(waits).await.library[0].clone();
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
    let file = dir.path().join("Board meeting.wav");
    wav(&file, 1_500);
    app.send(Command::Import { path: file.to_string_lossy().into() });
    let state =
        app.until(|s| s.session.is_none() && s.library.iter().any(|r| r.length_ms > 0)).await;
    let row = state.library[0].clone();
    assert_eq!((row.title.as_str(), row.length_ms, row.draft), ("Board meeting", 1_500, true));
    let id = row.id;
    let change = |change| Command::Change { id: id.clone(), change };
    app.send(change(Change::Title { text: "Budget".into() }));
    app.send(change(Change::Speaker { label: "1".into(), name: "Chair".into() }));
    app.send(change(Change::Note { text: "Decide in May".into() }));
    app.send(Command::Open { id: id.clone() });
    let open = app.until(|s| s.open.as_ref().is_some_and(|o| o.note == "Decide in May")).await.open;
    let open = open.expect("it is open");
    assert_eq!((open.row.title.as_str(), open.speakers["1"].as_str()), ("Budget", "Chair"));
    // A line is corrected on the record, not on a draft that is about to be replaced.
    let key = format!("{id}.0.00000");
    app.send(change(Change::Line { key, language: "en".into(), text: "Hello".into() }));
    app.notice("recording.writing").await;
    app.send(Command::Remove { id: id.clone() });
    app.until(|state| state.library.is_empty() && state.open.is_none()).await;
    app.send(Command::Restore { id: id.clone() });
    app.until(|state| state.library.len() == 1).await;
    // Written out by a row of the exporters, the extension added when the path has none.
    let out = dir.path().join("out");
    let to = out.to_string_lossy().into();
    let export =
        Command::Export { id: id.clone(), exporter: "markdown".into(), languages: vec![], to };
    app.send(export);
    let to = dir.path().join("out.txt").to_string_lossy().into();
    app.send(Command::Export { id: id.clone(), exporter: "pdf".into(), languages: vec![], to });
    app.notice("export.which").await;
    let exported = fs::read_to_string(dir.path().join("out.md")).expect("it was exported");
    assert!(exported.starts_with("# Budget"));
    app.send(Command::Close);
    app.until(|state| state.open.is_none()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_holds_no_sound_leaves_nothing_behind() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    let file = dir.path().join("empty.wav");
    fs::write(&file, b"not a sound").expect("the file is written");
    app.send(Command::Import { path: file.to_string_lossy().into() });
    app.notice("audio.file.format").await;
    let state = app.until(|state| state.session.is_none() && state.writing.is_empty()).await;
    assert!(state.library.is_empty(), "{:?}", state.library);
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
    let setup = Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
    app.send(Command::Setup { setup });
    // A room for the live captions: a link with the key after `#`, and nothing in it yet.
    app.send(Command::Share { recording: None });
    let live = app.until(|state| state.rooms.len() == 1).await.rooms[0].clone();
    assert!(live.recording.is_none() && live.link.starts_with(&format!("{server}/r/")));
    let mut viewer = Viewer::open(&live.link).await;
    viewer.until(|event, plain| (event == "slot" && plain.is_none()).then_some(())).await;
    app.send(Command::Start);
    let id = app.until(|state| state.session.is_some()).await.session.map(|s| s.id).unwrap();
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
    // The captions show it, cut where each unit ends, and so does the room, sealed.
    let columns = app.next(|event| match event {
        Event::Live(columns) if columns.iter().any(|c| !c.lines.is_empty()) => Some(columns),
        _ => None,
    });
    let column = columns.await.remove(0);
    assert_eq!((column.lines[0].text.as_str(), &column.lines[0].units[..]), ("Hello", &[5u16][..]));
    assert_eq!(column.rate, 4.0);
    let shown = |event: &str, plain: Option<&[u8]>| {
        let columns: serde_json::Value = serde_json::from_slice(plain?).ok()?;
        (event == "slot" && columns[0]["lines"][0]["text"] == "Hello").then_some(())
    };
    viewer.until(shown).await;
    // The draft has the line while the recording goes on.
    app.send(Command::Open { id: id.clone() });
    let has = |state: &State| {
        state.open.as_ref().is_some_and(|o| o.lines.iter().any(|(_, s)| s.line.text == "Hello"))
    };
    app.until(has).await;
    app.send(Command::Stop);
    viewer.until(|event, plain| (event == "slot" && plain.is_none()).then_some(())).await;
    // A recording's room holds its text in the log.
    app.send(Command::Share { recording: Some(id.clone()) });
    let state = app.until(|state| state.rooms.len() == 2).await;
    let room = state.rooms.iter().find(|room| room.recording.as_ref() == Some(&id)).cloned();
    let mut reader = Viewer::open(&room.expect("a room for it").link).await;
    let page = reader.until(|event, plain| {
        (event == "log").then(|| serde_json::from_slice::<serde_json::Value>(plain?).ok())?
    });
    let page = page.await;
    assert_eq!(
        (page["row"]["id"].as_str(), page["lines"][0][1]["text"].as_str()),
        (Some(&*id), Some("Hello"))
    );
    // One room for a recording is enough: shared again, it is the same one.
    app.send(Command::Share { recording: Some(id.clone()) });
    // Taken away: whoever was watching is told, and the list keeps the other room.
    app.send(Command::Unshare { room: live.id.clone() });
    viewer.until(|event, _| (event == "gone").then_some(())).await;
    let state = app.until(|state| state.rooms.len() == 1).await;
    assert_eq!(state.rooms[0].recording.as_ref(), Some(&id));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_is_one_more_record() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
    app.send(Command::Start);
    app.until(|s| s.session.is_some()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    let state =
        app.until(|s| s.session.is_none() && s.library.iter().any(|r| r.length_ms > 0)).await;
    let id = state.library[0].id.clone();
    let change = |change| Command::Change { id: id.clone(), change };
    app.send(change(Change::Title { text: "Budget".into() }));
    app.send(change(Change::Speaker { label: "1".into(), name: "Chair".into() }));
    app.send(change(Change::Note { text: "Decide in May".into() }));
    app.send(Command::Open { id: id.clone() });
    let open = app.until(|s| s.open.as_ref().is_some_and(|o| o.note == "Decide in May")).await.open;
    let open = open.expect("it is open");
    assert_eq!((open.row.title.as_str(), open.speakers["1"].as_str()), ("Budget", "Chair"));
    // A line is corrected on the record, not on a draft that is about to be replaced.
    let key = format!("{id}.0.00000");
    app.send(change(Change::Line { key, language: "en".into(), text: "Hello".into() }));
    app.notice("recording.writing").await;
    app.send(Command::Remove { id: id.clone() });
    app.until(|state| state.library.is_empty() && state.open.is_none()).await;
    app.send(Command::Restore { id: id.clone() });
    app.until(|state| state.library.len() == 1).await;
    app.send(Command::Close);
    app.until(|state| state.open.is_none()).await;
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
    app.until(|state| state.rotation_unsaved).await;
    app.send(Command::CopyRotation);
    let mut seen = Vec::new();
    app.next(|event| {
        seen.push(serde_json::to_string(&event).expect("an event is JSON"));
        matches!(event, Event::Notice(ref said) if said == "copied").then_some(())
    })
    .await;
    assert_eq!(*clipped.lock().expect("one at a time"), [written.to_string()]);
    assert!(seen.iter().all(|json| !json.contains(&written[1..])), "the key reached the page");
    // Saved, it is no longer asked for, and can still be copied.
    app.send(Command::RotationSaved);
    app.until(|state| !state.rotation_unsaved).await;
    app.send(Command::CopyRotation);
    app.notice("copied").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_recovery_key_or_a_clipboard_it_says_so() {
    let _store = STORE.lock().await;
    store(true);
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::start(dir.path());
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
    app.core.looked(Err("update"));
    app.notice("update").await;
    app.core.looked(Ok("0.9.1".into()));
    app.until(|state| state.update.as_deref() == Some("0.9.1")).await;
    app.send(Command::Start);
    app.until(|state| state.session.is_some()).await;
    app.send(Command::Update);
    app.notice("update.recording").await;
    assert_eq!(installs.load(SeqCst), 0, "the recording was not interrupted");
    app.send(Command::Stop);
    app.until(|state| state.session.is_none()).await;
    app.send(Command::Update);
    app.notice("update.install").await; // this port returns, as one that could not install
    assert_eq!(installs.load(SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_core_runs_on_a_tree_with_a_name() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("a folder");
    let seen = zutalk_log::capture(zutalk_log::Level::Debug);
    let app = App::start(dir.path());

    let starts = seen.of("task.start");
    assert_eq!(starts.len(), 2);
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

    let tally = app.core.stats().tally();
    let started = tally.counts.iter().find(|row| row.name == "task.started").map(|row| row.n);
    assert_eq!(started, Some(2));
    assert_eq!(seen.refused(), 0);
}

/// Waits for a `State` matching `holds`, failing if `"keys.none"` is ever noticed.
async fn until_no_keys_none(app: &mut App, holds: impl Fn(&State) -> bool) -> State {
    app.next(|event| match event {
        Event::Notice(code) if code == "keys.none" => panic!("unexpected keys.none"),
        Event::State(state) if holds(&state) => Some(*state),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_key_sound_is_kept_and_the_status_says_why() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _store = STORE.lock().await;
    store(false);
    let seen = zutalk_log::capture(zutalk_log::Level::Info);

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

    let no_key = |state: &State| {
        state.session.as_ref().map(|session| &session.status)
            == Some(&Status::AudioOnly { why: "captions.no_key".into() })
    };
    app.send(Command::Start);
    until_no_keys_none(&mut app, no_key).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    app.send(Command::Pause);
    until_no_keys_none(&mut app, |s| {
        s.session.as_ref().is_some_and(|session| session.status == Status::Paused)
    })
    .await;
    app.send(Command::Resume);
    until_no_keys_none(&mut app, no_key).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    app.send(Command::Stop);

    let state = until_no_keys_none(&mut app, |s| {
        s.session.is_none()
            && s.writing.is_empty()
            && s.library.len() == 1
            && s.library[0].length_ms > 0
    })
    .await;
    let row = &state.library[0];
    assert!(row.draft && row.audio && row.length_ms > 0, "{row:?}");
    assert!(state.writing.is_empty());

    // Drain any queued events and ensure `keys.none` was never noticed.
    while let Ok(event) = app.events.try_recv() {
        assert!(
            !matches!(&event, Event::Notice(code) if code == "keys.none"),
            "unexpected keys.none: {event:?}"
        );
    }

    accept_task.abort();
    assert_eq!(connections.load(SeqCst), 0, "no realtime lane may dial without a key");
    soniox.verify().await;

    let skipped = seen.of("write.skipped");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].level, zutalk_log::Level::Info);
    assert_eq!(skipped[0].u64("n"), Some(1));
    assert_eq!(skipped[0].text("why"), Some("keys.none"));
    assert_eq!(seen.refused(), 0);
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
    let id = app.until(|s| s.session.is_some()).await.session.expect("session").id;

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
    let columns = app
        .next(|event| match event {
            Event::Live(columns) if columns.iter().any(|c| !c.lines.is_empty()) => Some(columns),
            _ => None,
        })
        .await;
    assert_eq!(columns[0].lines[0].text, "Hello");

    // Draft has the line before whole-transcript replaces it.
    app.send(Command::Open { id: id.clone() });
    app.until(|s| {
        s.open.as_ref().is_some_and(|o| o.lines.iter().any(|(_, l)| l.line.text == "Hello"))
    })
    .await;

    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);

    // Whole transcript runs with the own key and turns the draft into a written record.
    let state = app
        .until(|s| {
            s.session.is_none()
                && s.writing.is_empty()
                && s.library.iter().any(|r| r.id == id && !r.draft)
        })
        .await;
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
    let seen = zutalk_log::capture(zutalk_log::Level::Info);
    let mut app =
        App::with(Config { soniox: format!("{}/v1", stt.uri()), ..config(data, NOWHERE) });

    let state = app.until(|s| s.library.len() == 2).await;
    assert!(state.me.is_none());
    assert!(state.writing.is_empty());
    assert!(state.library.iter().all(|r| r.draft && r.audio));

    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(event) = app.events.try_recv() {
        match event {
            Event::Notice(code) => panic!("unexpected notice while waiting for key: {code}"),
            Event::State(s) => assert!(s.writing.is_empty(), "unexpected writing: {:?}", s.writing),
            Event::Live(_) => {}
        }
    }
    assert!(stt.received_requests().await.expect("requests").is_empty());

    let skipped = seen.of("write.skipped");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].level, zutalk_log::Level::Info);
    assert_eq!(skipped[0].u64("n"), Some(2));
    assert_eq!(skipped[0].text("why"), Some("keys.none"));

    // Setting an own Soniox key re-runs recover and writes both waiting drafts.
    app.send(Command::OwnKey { service: "soniox".into(), key: Some("sk-own-test-key".into()) });
    let state = app
        .until(|s| {
            s.writing.is_empty() && s.library.len() == 2 && s.library.iter().all(|r| !r.draft)
        })
        .await;
    assert_eq!(state.library.len(), 2);
    assert_eq!(asked.lock().expect("one at a time").len(), 2);
    assert_eq!(seen.refused(), 0);
}
