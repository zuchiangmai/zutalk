//! Hosted ephemeral live rooms: explicit `HostState` transition table, AES-256-GCM frame crypto
//! with fragment-only key delivery, atomic `host.lock` (room id only), stop-outbound keeping
//! local recording alive, cancellation/stop/sign-out teardown, crash recovery on next boot, and
//! unauthenticated refusal without network traffic (`P5.md` §3.3, `P5-02`).

mod common;
mod hub;

use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD, Engine};
use common::{Viewer, from_link, serve, serve_handle};
use futures_util::{SinkExt, StreamExt};
use hub::{App, ME, STORE, config_on as config, store};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use zu::{Cid, Codec, Doc, Held, PublicKey, Query as ZuQuery, Record, SecretKey, Value as ZuValue};
use zutalk_core::app::hub::{Config as CoreConfig, Core, Outlet};
use zutalk_core::app::proto::{
    CaptionBg, CaptionPrefs, CaptionScene, CaptionSize, Command, HostEvent, HostState, Query,
    QueryValue, RecordingView, Session, Setup, State, WindowRect,
};
use zutalk_core::capture::tape;
use zutalk_core::id::keys;
use zutalk_core::out::host::{
    HOST_LOCK_FILE, HostStepEvent, clear_lock, clear_lock_if, host_step, is_room_id, link, open,
    read_lock, seal, write_lock,
};
use zutalk_core::rt::supervise::End;
use zutalk_server::{
    BoxFuture, Config as ServerConfig, Directory, Minted, ROOM_IDLE_TTL_MS, Refusal, Server,
    Upstream,
};

const KEY: [u8; 32] = [7; 32];

fn sample_event(title: &str) -> HostEvent {
    HostEvent {
        city: "city.example".into(),
        event: "did:zu:org/ev/event/e1".into(),
        title: title.into(),
        start: Some(1_800_000_000_000),
    }
}

#[test]
fn host_state_transition_table() {
    let ev = Some(sample_event("Opening"));
    let ev2 = Some(sample_event("Panel"));

    // Idle
    let arming =
        host_step(&HostState::Idle, HostStepEvent::Arm { event: ev.clone(), captions: true });
    assert_eq!(arming, HostState::Arming { event: ev.clone(), captions: true });
    assert_eq!(arming.code(), "Arming");
    assert!(arming.is_active());
    assert!(!arming.is_outbound());

    let armed_no_cap =
        host_step(&HostState::Idle, HostStepEvent::Arm { event: ev.clone(), captions: false });
    assert_eq!(
        armed_no_cap,
        HostState::Armed { event: ev.clone(), captions: false, link: None, viewers: 0 }
    );
    assert!(!armed_no_cap.is_outbound());
    assert_eq!(host_step(&HostState::Idle, HostStepEvent::Stopped), HostState::Idle);

    // Arming -> Armed / Idle / Stopping
    let armed = host_step(&arming, HostStepEvent::RoomMade { link: "https://t/r/id#k".into() });
    assert_eq!(
        armed,
        HostState::Armed {
            event: ev.clone(),
            captions: true,
            link: Some("https://t/r/id#k".into()),
            viewers: 0,
        }
    );
    assert!(armed.is_outbound());
    assert_eq!(host_step(&arming, HostStepEvent::RoomFailed), HostState::Idle);
    assert_eq!(host_step(&arming, HostStepEvent::Cancel { wait_gone: true }), HostState::Stopping);
    assert_eq!(host_step(&arming, HostStepEvent::Cancel { wait_gone: false }), HostState::Idle);
    assert_eq!(host_step(&arming, HostStepEvent::Stopped), arming);

    // Armed: viewer count, event swap, captions toggle, start recording
    let armed_2 = host_step(&armed, HostStepEvent::Viewers(2));
    assert_eq!(armed_2.viewers(), 2);

    let armed_ev2 = host_step(&armed_2, HostStepEvent::Arm { event: ev2.clone(), captions: true });
    assert_eq!(
        armed_ev2,
        HostState::Armed {
            event: ev2.clone(),
            captions: true,
            link: Some("https://t/r/id#k".into()),
            viewers: 2,
        }
    );

    let toggled_off =
        host_step(&armed_ev2, HostStepEvent::Arm { event: ev2.clone(), captions: false });
    assert_eq!(
        toggled_off,
        HostState::Armed { event: ev2.clone(), captions: false, link: None, viewers: 0 }
    );
    let toggled_back =
        host_step(&toggled_off, HostStepEvent::Arm { event: ev2.clone(), captions: true });
    assert_eq!(toggled_back, HostState::Arming { event: ev2.clone(), captions: true });

    let live = host_step(&armed_2, HostStepEvent::RecordingStarted);
    assert_eq!(
        live,
        HostState::Live {
            event: ev.clone(),
            outbound: true,
            link: Some("https://t/r/id#k".into()),
            viewers: 2,
        }
    );
    assert!(live.is_outbound());

    // Live: viewer update, StopOutbound (keeps event & Live), RecordingStopped -> Stopping -> Idle
    let live_3 = host_step(&live, HostStepEvent::Viewers(3));
    assert_eq!(live_3.viewers(), 3);

    let live_local = host_step(&live_3, HostStepEvent::StopOutbound);
    assert_eq!(
        live_local,
        HostState::Live { event: ev.clone(), outbound: false, link: None, viewers: 0 }
    );
    assert!(!live_local.is_outbound());
    assert!(live_local.is_active());
    assert_eq!(host_step(&live_local, HostStepEvent::Viewers(5)).viewers(), 0);

    let stopping = host_step(&live_3, HostStepEvent::RecordingStopped { wait_gone: true });
    assert_eq!(stopping, HostState::Stopping);
    assert_eq!(host_step(&stopping, HostStepEvent::Viewers(1)), HostState::Stopping);
    assert_eq!(host_step(&stopping, HostStepEvent::Stopped), HostState::Idle);
    assert_eq!(
        host_step(&live_local, HostStepEvent::RecordingStopped { wait_gone: false }),
        HostState::Idle
    );
}

#[test]
fn frame_crypto_and_link_fragment_only() {
    let frame = seal(&KEY, &[1; 12], b"said in the room").unwrap();
    assert_eq!(frame.len(), 12 + 16 + 16);
    assert_eq!(&frame[..12], &[1; 12]);
    assert_eq!(open(&KEY, &frame).unwrap(), b"said in the room");
    assert_ne!(seal(&KEY, &[2; 12], b"said in the room").unwrap(), frame);
    assert_eq!(open(&[8; 32], &frame), Err(End::Fatal("room.frame")));
    for at in [0, 12, frame.len() - 1] {
        let mut changed = frame.clone();
        changed[at] ^= 1;
        assert_eq!(open(&KEY, &changed), Err(End::Fatal("room.frame")), "byte {at}");
    }
    assert_eq!(open(&KEY, &frame[..11]), Err(End::Fatal("room.frame")));
    assert_eq!(open(&KEY, &seal(&KEY, &[3; 12], b"").unwrap()).unwrap(), b"");

    let id = "a".repeat(32);
    let shared = link("https://talk.example", &id, &KEY);
    let (url, fragment) = shared.split_once('#').unwrap();
    assert_eq!(url, format!("https://talk.example/r/{id}"));
    assert_eq!(fragment.len(), 43);
    assert!(!url.contains(fragment));
    assert_eq!(from_link(&shared), ("https://talk.example".into(), id, KEY));
}

#[test]
fn host_lock_stores_only_room_id_and_never_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    let key_frag = BASE64_URL_SAFE_NO_PAD.encode(KEY);
    assert!(is_room_id(id));
    assert!(!is_room_id("not-a-room-id"));
    assert_eq!(write_lock(dir.path(), "bad"), Err("room.bad"));
    assert_eq!(read_lock(dir.path()), None);

    write_lock(dir.path(), id).unwrap();
    let raw = fs::read_to_string(dir.path().join(HOST_LOCK_FILE)).unwrap();
    assert_eq!(raw, id);
    assert!(!raw.contains(&key_frag));
    assert_eq!(read_lock(dir.path()).as_deref(), Some(id));

    // Corrupt lock file returns None safely.
    fs::write(dir.path().join(HOST_LOCK_FILE), "corrupt-not-hex").unwrap();
    assert_eq!(read_lock(dir.path()), None);

    clear_lock(dir.path());
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());
    // Second clear is idempotent.
    clear_lock(dir.path());
}

#[test]
fn host_state_and_step_debug_redact_room_key() {
    let id = "a".repeat(32);
    let live_link = link("https://talk.example", &id, &KEY);
    let key_frag = BASE64_URL_SAFE_NO_PAD.encode(KEY);
    assert!(live_link.ends_with(&format!("#{key_frag}")));

    let armed = HostState::Armed {
        event: Some(sample_event("Stage")),
        captions: true,
        link: Some(live_link.clone()),
        viewers: 2,
    };
    let live = HostState::Live {
        event: Some(sample_event("Stage")),
        outbound: true,
        link: Some(live_link.clone()),
        viewers: 3,
    };
    let state = State { host: armed.clone(), ..State::default() };
    let made = HostStepEvent::RoomMade { link: live_link };

    for rendered in
        [format!("{armed:?}"), format!("{live:?}"), format!("{state:?}"), format!("{made:?}")]
    {
        assert!(!rendered.contains(&key_frag), "Debug leaked room key: {rendered}");
        assert!(rendered.contains(&format!("https://talk.example/r/{id}#…")), "{rendered}");
    }

    // Links without `#` must still be redacted to `"…"`.
    let opaque = HostStepEvent::RoomMade { link: "opaque-secret-token".into() };
    let opaque_dbg = format!("{opaque:?}");
    assert!(!opaque_dbg.contains("opaque-secret-token"), "Debug leaked opaque link: {opaque_dbg}");
    assert!(opaque_dbg.contains("…"), "{opaque_dbg}");
}

#[test]
fn host_lock_clear_if_preserves_newer_room_lock() {
    let dir = tempfile::tempdir().unwrap();
    let id_a = "0123456789abcdef0123456789abcdef";
    let id_b = "fedcba9876543210fedcba9876543210";

    write_lock(dir.path(), id_a).unwrap();
    write_lock(dir.path(), id_b).unwrap();

    // Clearing the older room id must leave the newer room's lock intact.
    clear_lock_if(dir.path(), id_a);
    assert_eq!(read_lock(dir.path()).as_deref(), Some(id_b));

    // Clearing the matching room id removes the lock file.
    clear_lock_if(dir.path(), id_b);
    assert_eq!(read_lock(dir.path()), None);
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());

    // Calling again when absent is a safe no-op.
    clear_lock_if(dir.path(), id_b);
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn host_arm_live_stop_outbound_keeps_local_recording_and_ends_room() {
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

    // 1. HostArm opens an ephemeral room, writes room-id-only host.lock, and exposes link.
    app.send(Command::HostArm { event: Some(sample_event("Town hall")), captions: true });
    let state = app
        .until(|s| matches!(s.host, HostState::Armed { captions: true, link: Some(_), .. }))
        .await;
    let HostState::Armed { link: Some(live_link), .. } = state.host else {
        panic!("expected Armed with link");
    };
    let (_, room_id, key_bytes) = from_link(&live_link);
    let key_frag = BASE64_URL_SAFE_NO_PAD.encode(key_bytes);
    let lock_body = fs::read_to_string(dir.path().join(HOST_LOCK_FILE)).expect("host.lock exists");
    assert_eq!(lock_body, room_id);
    assert!(!lock_body.contains(&key_frag));
    assert!(app.core.is_hosting());

    // 2. Viewer joins SSE; within the 3 s arm poll interval, State.host reports viewers == 1.
    let mut viewer = Viewer::open(&live_link).await;
    viewer.until(|event, plain| (event == "slot" && plain.is_none()).then_some(())).await;
    app.until(|s| s.host.viewers() == 1).await;

    // 3. Start recording -> Session carries outbound = true, viewers = 1, event_title = "Town hall".
    app.send(Command::Start);
    let session = app
        .session(|s| s.is_some_and(|s| s.outbound && s.viewers == 1))
        .await
        .expect("live hosted session");
    assert_eq!(session.event_title.as_deref(), Some("Town hall"));

    // Emit one caption word from fake Soniox and verify viewer decrypts it.
    let (stream, _) = soniox.accept().await.expect("a connection");
    let mut ws = accept_async(stream).await.expect("a websocket");
    let _ = ws.next().await;
    let hello = json!({"text": "Welcome", "start_ms": 100, "end_ms": 400, "is_final": true,
                       "language": "en", "speaker": "1"});
    let said =
        json!({"tokens": [hello, {"text": "<end>", "is_final": true}], "total_audio_proc_ms": 500});
    ws.send(Message::text(said.to_string())).await.expect("it is said");
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                let _ = ws
                    .send(Message::text(json!({"tokens": [], "finished": true}).to_string()))
                    .await;
            }
        }
    });
    viewer
        .until(|event, plain| {
            let columns: serde_json::Value = serde_json::from_slice(plain?).ok()?;
            (event == "slot" && columns[0]["lines"][0]["text"] == "Welcome").then_some(())
        })
        .await;

    // 4. StopOutbound -> viewer gets `gone`, room returns 404, host.lock is deleted,
    //    Session flips outbound=false / viewers=0 while local recording continues until Stop.
    app.send(Command::StopOutbound);
    app.notice("host.stopped").await;
    viewer.until(|event, _| (event == "gone").then_some(())).await;
    app.core.wait_host_idle(2_000).await;
    assert!(!app.core.is_hosting());
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());

    let after = reqwest::get(format!("{server}/rooms/{room_id}/events")).await.unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::NOT_FOUND);

    let still_recording = app
        .session(|s| s.is_some_and(|s| !s.outbound && s.viewers == 0))
        .await
        .expect("still live");
    assert_eq!(still_recording.id, session.id);

    // Finish local recording cleanly.
    tokio::time::sleep(Duration::from_millis(500)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    app.until(|s| s.host == HostState::Idle).await;
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_cancel_stop_recording_and_sign_out_all_send_gone_and_clear_lock() {
    let _store = STORE.lock().await;
    store(true);
    let (server, _kept) = serve().await;
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(config(dir.path(), &server));
    app.until(|_| true).await;

    // Path A: HostCancel while armed.
    app.send(Command::HostArm { event: None, captions: true });
    let link_a =
        match app.until(|s| matches!(s.host, HostState::Armed { link: Some(_), .. })).await.host {
            HostState::Armed { link: Some(l), .. } => l,
            other => panic!("unexpected host state: {other:?}"),
        };
    let mut viewer_a = Viewer::open(&link_a).await;
    viewer_a.until(|event, _| (event == "slot").then_some(())).await;
    app.send(Command::HostCancel);
    viewer_a.until(|event, _| (event == "gone").then_some(())).await;
    app.until(|s| s.host == HostState::Idle).await;
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());

    // Path B: Stop recording while hosting live.
    app.send(Command::HostArm { event: None, captions: true });
    let link_b =
        match app.until(|s| matches!(s.host, HostState::Armed { link: Some(_), .. })).await.host {
            HostState::Armed { link: Some(l), .. } => l,
            other => panic!("unexpected host state: {other:?}"),
        };
    let mut viewer_b = Viewer::open(&link_b).await;
    viewer_b.until(|event, _| (event == "slot").then_some(())).await;
    app.send(Command::Start);
    app.session(|s| s.is_some_and(|s| s.outbound)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.send(Command::Stop);
    viewer_b.until(|event, _| (event == "gone").then_some(())).await;
    app.session(|s| s.is_none()).await;
    app.until(|s| s.host == HostState::Idle).await;
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());

    // Path C: SignOut while armed (uses original Ours snapshot before identity clears).
    app.send(Command::HostArm { event: None, captions: true });
    let link_c =
        match app.until(|s| matches!(s.host, HostState::Armed { link: Some(_), .. })).await.host {
            HostState::Armed { link: Some(l), .. } => l,
            other => panic!("unexpected host state: {other:?}"),
        };
    let mut viewer_c = Viewer::open(&link_c).await;
    viewer_c.until(|event, _| (event == "slot").then_some(())).await;
    app.send(Command::SignOut);
    viewer_c.until(|event, _| (event == "gone").then_some(())).await;
    app.until(|s| s.me.is_none() && s.host == HostState::Idle).await;
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_recovery_reads_host_lock_on_next_boot_and_sends_gone() {
    let _store = STORE.lock().await;
    store(true);
    let (server, _kept) = serve().await;
    let dir = tempfile::tempdir().expect("a folder");

    // First process: arm a room and then drop Core without stopping (simulated crash).
    let (live_link, room_id) = {
        let mut app = App::with(config(dir.path(), &server));
        app.until(|_| true).await;
        app.send(Command::HostArm { event: None, captions: true });
        let link = match app
            .until(|s| matches!(s.host, HostState::Armed { link: Some(_), .. }))
            .await
            .host
        {
            HostState::Armed { link: Some(l), .. } => l,
            other => panic!("unexpected host state: {other:?}"),
        };
        let (_, id, _) = from_link(&link);
        assert_eq!(read_lock(dir.path()).as_deref(), Some(id.as_str()));
        (link, id)
    };

    // Viewer is still connected to the orphaned room on the server after the crash.
    let mut viewer = Viewer::open(&live_link).await;
    viewer.until(|event, _| (event == "slot").then_some(())).await;

    // Second process: restart Core on the same data dir -> automatically sends `gone`, clears
    // `host.lock`, notifies viewer, and emits `host.recover(code = "ok")`.
    let mut restarted = App::with(config(dir.path(), &server));
    restarted.until(|s| s.me.is_some()).await;
    viewer.until(|event, _| (event == "gone").then_some(())).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while dir.path().join(HOST_LOCK_FILE).exists() || restarted.seen.of("host.recover").is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "host.recover did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let after = reqwest::get(format!("{server}/rooms/{room_id}/events")).await.unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::NOT_FOUND);
    let recovers = restarted.seen.of("host.recover");
    assert_eq!(recovers.len(), 1, "{recovers:?}");
    assert_eq!(recovers[0].text("code"), Some("ok"));
    assert_eq!(restarted.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_host_arm_is_refused_without_touching_server() {
    let _store = STORE.lock().await;
    store(false);
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await;

    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(config(dir.path(), &server.uri()));
    assert!(app.until(|_| true).await.me.is_none());

    app.send(Command::HostArm { event: Some(sample_event("Private")), captions: true });
    app.notice("sign_in.first").await;
    assert_eq!(app.outlet.state.now().1.host, HostState::Idle);
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());
    server.verify().await;
}

/// Core wrapper that inherits the caller's active `zutalk_log::files` scope instead of installing
/// an in-memory `capture()`, so events reach real `<data>/logs/zutalk.jsonl` on disk (`LESSONS.md`).
struct FileApp {
    core: Core,
    outlet: Outlet,
    said_at: u64,
}

impl FileApp {
    fn start(scope: &zutalk_log::Scope, cfg: CoreConfig) -> Self {
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
                {
                    let view = view.map(|b| *b);
                    if holds(view.as_ref()) {
                        return view;
                    }
                }
                let _ = timeout(Duration::from_millis(50), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("recording condition came in time")
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

const CITY_DID: &str = "did:zu:cityaudit4seas";
const ORG_DID: &str = "did:zu:orgaudit4seas";

/// Minimal in-memory Zu city host accepting `/.well-known/zu-did` and `/.well-known/zu-recs`.
#[derive(Clone, Default)]
struct AuditCity(Arc<Mutex<Held>>);

impl AuditCity {
    fn with_event(event_key: &str, title: &str) -> Self {
        let signer = SecretKey::from_seed(&[7; 32]);
        let body = ZuValue::from_json(
            &json!({ "title": title, "start": 1_800_000_000_000i64 }).to_string(),
        )
        .unwrap();
        let body_cid = Cid::of(Codec::Drisl, &body.encode());
        let rec = Record::sign(
            &signer,
            zu::Path { by: ORG_DID.into(), ty: "ev/event".into(), key: event_key.into() },
            Some(body_cid),
            [],
            1_800_000_000_000,
            Some(CITY_DID),
        )
        .unwrap();
        let mut held = Held::default();
        held.bodies.insert(body_cid, body);
        held.records.push(rec);
        Self(Arc::new(Mutex::new(held)))
    }
}

impl Respond for AuditCity {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut held = self.0.lock().unwrap();
        let param = |name: &str| {
            request.url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
        };
        match (request.method.as_str(), request.url.path()) {
            ("GET", "/.well-known/zu-did") => ResponseTemplate::new(200).set_body_string(CITY_DID),
            ("GET", "/.well-known/zu-recs") => {
                let query = ZuQuery {
                    by: param("by"),
                    to: param("to"),
                    ty: param("type"),
                    key: param("key"),
                    ..ZuQuery::default()
                };
                let records: Vec<Record> =
                    held.records.iter().filter(|r| query.matches(r)).cloned().collect();
                let bodies = records
                    .iter()
                    .filter_map(|r| Some((r.body?, held.bodies.get(&r.body?)?.clone())))
                    .collect();
                let view = Held { records, bodies };
                ResponseTemplate::new(200).set_body_bytes(zu::query::answer(&view, None))
            }
            ("POST", "/.well-known/zu-recs") => {
                let Ok((given, _)) = ZuQuery::default().read(&request.body) else {
                    return ResponseTemplate::new(400);
                };
                let mut stat = BTreeMap::new();
                for rec in given.records {
                    let key_seed = if rec.path.by == ME { [5; 32] } else { [7; 32] };
                    let signer = SecretKey::from_seed(&key_seed).public();
                    let signed = rec.verify(
                        |by, pubkey: &PublicKey, _| by == ME && *pubkey == signer,
                        |_| None,
                    );
                    let word = if signed.is_ok() && rec.to.as_deref() == Some(CITY_DID) {
                        "ok"
                    } else {
                        "bad"
                    };
                    stat.insert(rec.cid().to_string(), ZuValue::Str(word.into()));
                    if word == "ok"
                        && let Some(cid) = rec.body
                        && let Some(val) = given.bodies.get(&cid)
                    {
                        held.bodies.insert(cid, val.clone());
                        held.records.push(rec);
                    }
                }
                let answer = ZuValue::Map([("stat".to_owned(), ZuValue::Map(stat))].into());
                ResponseTemplate::new(200).set_body_bytes(answer.encode())
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn host_crash_recovery_double_insurance_leaves_nothing_outside() {
    let _store = STORE.lock().await;
    store(true);
    keys::set("soniox", Some(b"sk-own-test-key")).expect("soniox key kept");
    let (origin, srv, srv_dir) = serve_handle().await;
    let soniox = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("ws port");
    let realtime = format!("ws://{}", soniox.local_addr().expect("ws addr"));

    // --- Insurance 1: client crash leaves host.lock -> reboot sends `gone`, clears lock ---
    let dir1 = tempfile::tempdir().expect("dir1");
    let (link1, room_id_1, mut viewer1) = {
        let mut app = App::on(dir1.path(), &origin, &realtime);
        app.until(|_| true).await;
        let setup =
            Setup { spoken: vec!["en".into()], shown: vec!["en".into()], ..Setup::default() };
        app.send(Command::Setup { setup });

        app.send(Command::HostArm { event: Some(sample_event("InsuranceOne")), captions: true });
        let state = app
            .until(|s| matches!(s.host, HostState::Armed { captions: true, link: Some(_), .. }))
            .await;
        let HostState::Armed { link: Some(live_link), .. } = state.host else {
            panic!("expected Armed");
        };
        let (_, room_id, key_bytes) = from_link(&live_link);
        let key_frag = BASE64_URL_SAFE_NO_PAD.encode(key_bytes);
        let lock_body = fs::read_to_string(dir1.path().join(HOST_LOCK_FILE)).expect("lock");
        assert_eq!(lock_body, room_id);
        assert!(!lock_body.contains(&key_frag));

        let mut viewer = Viewer::open(&live_link).await;
        viewer.until(|ev, plain| (ev == "slot" && plain.is_none()).then_some(())).await;
        app.until(|s| s.host.viewers() == 1).await;

        app.send(Command::Start);
        app.session(|s| s.is_some_and(|s| s.outbound && s.viewers == 1)).await;

        let (stream, _) = soniox.accept().await.expect("soniox connect");
        let mut ws = accept_async(stream).await.expect("ws");
        let _ = ws.next().await;
        let payload = json!({
            "tokens": [
                {"text": "InsuranceLive", "start_ms": 100, "end_ms": 400, "is_final": true, "language": "en", "speaker": "1"},
                {"text": "<end>", "is_final": true}
            ],
            "total_audio_proc_ms": 500
        });
        ws.send(Message::text(payload.to_string())).await.expect("sent caption");
        viewer
            .until(|ev, plain| {
                let cols: Json = serde_json::from_slice(plain?).ok()?;
                (ev == "slot" && cols[0]["lines"][0]["text"] == "InsuranceLive").then_some(())
            })
            .await;

        // Drop Core directly without Stop / HostCancel to simulate a process crash.
        (live_link, room_id, viewer)
    };
    let _ = &link1;
    assert_eq!(read_lock(dir1.path()).as_deref(), Some(room_id_1.as_str()));

    let mut restarted = App::with(config(dir1.path(), &origin));
    restarted.until(|s| s.me.is_some()).await;
    viewer1.until(|ev, _| (ev == "gone").then_some(())).await;
    assert!(viewer1.next().await.is_none(), "viewer stream must close after gone");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while dir1.path().join(HOST_LOCK_FILE).exists() || restarted.seen.of("host.recover").is_empty()
    {
        assert!(tokio::time::Instant::now() < deadline, "host.recover timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let status1 = reqwest::get(format!("{origin}/rooms/{room_id_1}/events")).await.unwrap();
    assert_eq!(status1.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(restarted.seen.of("host.recover").iter().any(|l| l.text("code") == Some("ok")));
    assert_eq!(restarted.seen.refused(), 0);
    drop(restarted);

    // --- Insurance 2: client crash WITHOUT reboot -> server 2-minute TTL sweep cleans room ---
    let dir2 = tempfile::tempdir().expect("dir2");
    let (room_id_2, mut viewer2) = {
        let mut app = App::with(config(dir2.path(), &origin));
        app.until(|_| true).await;
        app.send(Command::HostArm { event: None, captions: true });
        let state = app
            .until(|s| matches!(s.host, HostState::Armed { captions: true, link: Some(_), .. }))
            .await;
        let HostState::Armed { link: Some(live_link), .. } = state.host else {
            panic!("expected Armed");
        };
        let (_, room_id, _) = from_link(&live_link);
        let mut viewer = Viewer::open(&live_link).await;
        viewer.until(|ev, _| (ev == "slot").then_some(())).await;
        (room_id, viewer)
    };
    // Client is NOT restarted; advance server sweep past ROOM_IDLE_TTL_MS.
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let (removed, sweep_events) = {
        let sweep_seen = zutalk_log::capture(zutalk_log::Level::Debug);
        let removed = srv.sweep(now_ms + ROOM_IDLE_TTL_MS + 1);
        (removed, sweep_seen.of("server.sweep"))
    };
    assert_eq!(removed, 1);
    assert!(
        sweep_events.iter().any(|l| l.u64("removed") == Some(1)),
        "expected server.sweep(removed = 1), saw {sweep_events:?}",
    );
    viewer2.until(|ev, _| (ev == "gone").then_some(())).await;
    assert!(viewer2.next().await.is_none(), "viewer2 stream must close after TTL sweep");
    let status2 = reqwest::get(format!("{origin}/rooms/{room_id_2}/events")).await.unwrap();
    assert_eq!(status2.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(
        !srv_dir.path().join("rooms").exists(),
        "ephemeral live rooms must never create a rooms/ directory on server disk"
    );
    srv.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_city_and_caption_secrets_never_reach_logs_diagnose_lock_or_settings() {
    let _store = STORE.lock().await;
    store(true);
    keys::set("soniox", Some(b"sk-own-privacy-probe-99")).expect("soniox key kept");
    keys::set("openrouter", Some(b"sk-own-openrouter-probe-99")).expect("openrouter key kept");
    let refused_before = zutalk_log::refused();

    let transcript_sentinel = "SecretTranscriptPhrase88";
    let translation_sentinel = "SecretTranslationPhrase99";
    let event_title_sentinel = "SecretCityEventTitle77";
    let handle_sentinel = "privacyhostprobe.zuzalu.ink";

    let (origin, srv, srv_dir) = serve_handle().await;
    let city_server = MockServer::start().await;
    Mock::given(any())
        .respond_with(AuditCity::with_event("ev-audit-1", event_title_sentinel))
        .mount(&city_server)
        .await;
    let city_addr = city_server.uri();

    let soniox = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("ws port");
    let realtime = format!("ws://{}", soniox.local_addr().expect("ws addr"));

    let data_dir = tempfile::tempdir().expect("data dir");
    let log_dir = data_dir.path().join("logs");
    let (scope, guard) = zutalk_log::files(
        &log_dir,
        "zutalk",
        zutalk_log::Level::Debug,
        zutalk_log::Limits::default(),
    )
    .expect("open client file log");

    let asked = hub::Asked::default();
    let soniox_http = hub::soniox(&asked).await;
    let openrouter_http = MockServer::start().await;
    let mt_body = json!({
        "lines": [{ "id": 0, "unclear": false, "facts": [], "to": { "zh": "Zuzalu 位于清迈。" } }]
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "choices": [{ "message": { "content": mt_body } }] })),
        )
        .mount(&openrouter_http)
        .await;

    let mut cfg = config(data_dir.path(), &origin);
    cfg.realtime = realtime;
    cfg.soniox = format!("{}/v1", soniox_http.uri());
    cfg.openrouter = format!("{}/api/v1", openrouter_http.uri());
    let mut app = FileApp::start(&scope, cfg);
    app.until(|s| s.me.is_some()).await;

    // 1. Update Setup (4-scene CaptionPrefs + remembered city) and CaptionGeometry.
    let mut caption = CaptionPrefs { scene: CaptionScene::Band, ..CaptionPrefs::default() };
    caption.scenes.band.bg = CaptionBg::Solid;
    caption.scenes.band.size = CaptionSize::Large;
    caption.scenes.band.auto_hide = true;
    app.send(
        &scope,
        Command::Setup {
            setup: Setup {
                spoken: vec!["en".into()],
                shown: vec!["en".into(), "zh".into()],
                caption,
                city: Some(city_addr.clone()),
                ..Setup::default()
            },
        },
    );
    app.send(
        &scope,
        Command::CaptionGeometry {
            scene: CaptionScene::Band,
            rect: WindowRect { x: 48, y: 96, width: 820, height: 180 },
        },
    );
    app.until(|s| s.setup.caption.scenes.band.geometry.is_some()).await;

    // 2. Load city calendar and arm hosting with sentinel event title.
    app.send(&scope, Command::City { address: city_addr.clone() });
    let cal_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let event_path = loop {
        if let Ok(reply) = app.core.query(Query::Calendar).await
            && let QueryValue::Calendar(Some(cal)) = reply.value
            && let Some(ev) = cal.events.first()
        {
            assert_eq!(ev.title, event_title_sentinel);
            break ev.path.clone();
        }
        assert!(tokio::time::Instant::now() < cal_deadline, "city calendar did not load");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };

    let host_event = HostEvent {
        city: city_addr.clone(),
        event: event_path.clone(),
        title: event_title_sentinel.into(),
        start: Some(1_800_000_000_000),
    };
    app.send(&scope, Command::HostArm { event: Some(host_event), captions: true });
    let armed = app
        .until(|s| matches!(s.host, HostState::Armed { captions: true, link: Some(_), .. }))
        .await;
    let HostState::Armed { link: Some(live_link), .. } = armed.host else {
        panic!("expected Armed");
    };
    let (_, room_id, key_bytes) = from_link(&live_link);
    let key_frag = BASE64_URL_SAFE_NO_PAD.encode(key_bytes);
    let key_std = BASE64_STANDARD.encode(key_bytes);
    let lock_snapshot = fs::read_to_string(data_dir.path().join(HOST_LOCK_FILE)).expect("lock");
    assert_eq!(lock_snapshot, room_id);

    // 3. Start recording, stream encrypted transcript + translation sentinel, stop outbound, stop rec.
    let mut viewer = Viewer::open(&live_link).await;
    viewer.until(|ev, plain| (ev == "slot" && plain.is_none()).then_some(())).await;

    app.send(&scope, Command::Start);
    let rec_id = app.session(|s| s.is_some_and(|s| s.outbound)).await.expect("live session").id;

    let (stream, _) = soniox.accept().await.expect("ws accept");
    let mut ws = accept_async(stream).await.expect("ws");
    let _ = ws.next().await;
    let tokens = json!({
        "tokens": [
            {"text": transcript_sentinel, "start_ms": 100, "end_ms": 500, "is_final": true, "language": "en", "speaker": "1"},
            {"text": translation_sentinel, "is_final": true, "translation_status": "translation", "language": "zh"},
            {"text": "<end>", "is_final": true}
        ],
        "total_audio_proc_ms": 600
    });
    ws.send(Message::text(tokens.to_string())).await.expect("send tokens");
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                let _ = ws
                    .send(Message::text(json!({"tokens": [], "finished": true}).to_string()))
                    .await;
            }
        }
    });

    viewer
        .until(|ev, plain| {
            let cols: Json = serde_json::from_slice(plain?).ok()?;
            (ev == "slot" && cols[0]["lines"][0]["text"] == transcript_sentinel).then_some(())
        })
        .await;

    // Wait for audio tape persistence + 200 ms so seal() never discards as zero-length under load.
    let tape_dir = data_dir.path().join("tape").join(&rec_id);
    for _ in 0..100 {
        if tape::kept(&tape_dir) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    app.send(&scope, Command::StopOutbound);
    app.notice("host.stopped").await;
    viewer.until(|ev, _| (ev == "gone").then_some(())).await;

    app.send(&scope, Command::Stop);
    app.session(|s| s.is_none()).await;
    app.wait_recording(&rec_id, |v| {
        v.is_some_and(|r| r.row.length_ms > 0 && r.take == 1 && !r.row.draft)
    })
    .await;
    let progress_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while app.outlet.progress.now().1.refine.is_some() {
        assert!(tokio::time::Instant::now() < progress_deadline, "refine progress did not clear");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 4. File to city archive and write Diagnose bundle.
    app.send(&scope, Command::File { id: rec_id.clone(), event: event_path, audio: false });
    app.notice("city.filed").await;

    std::thread::sleep(Duration::from_millis(50));
    app.send(&scope, Command::Diagnose);
    app.notice("diagnose.done").await;

    drop(app);
    drop(guard);
    srv.shutdown().await;

    // 5. Byte-scan logs/*.jsonl, diagnose/**/*, host.lock snapshot, settings.json, and server disk.
    let mut log_lines = Vec::new();
    for entry in fs::read_dir(&log_dir).expect("read logs dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let content = fs::read_to_string(&path).expect("read jsonl");
            for line in content.lines().filter(|l| !l.trim().is_empty()) {
                log_lines.push(line.to_owned());
            }
        }
    }
    assert!(!log_lines.is_empty(), "client logs must not be empty");

    let forbidden_everywhere: Vec<String> = vec![
        key_frag.clone(),
        key_std.clone(),
        format!("#{key_frag}"),
        transcript_sentinel.into(),
        translation_sentinel.into(),
        event_title_sentinel.into(),
        ME.into(),
        "did:zu:".into(),
        handle_sentinel.into(),
        "sk-own-privacy-probe-99".into(),
        "sk-own-openrouter-probe-99".into(),
    ];
    let mut forbidden_no_room_id = forbidden_everywhere.clone();
    forbidden_no_room_id.push(room_id.clone());

    // host.lock snapshot: holds ONLY room_id, never key/fragment/title/DID.
    assert_eq!(lock_snapshot, room_id);
    for needle in &forbidden_everywhere {
        assert!(!lock_snapshot.contains(needle), "host.lock leaked `{needle}`");
    }

    // settings.json: holds Setup + CaptionPrefs + city, never room id/key/text/DID.
    let settings_raw =
        fs::read_to_string(data_dir.path().join("settings.json")).expect("settings.json exists");
    for needle in &forbidden_no_room_id {
        assert!(!settings_raw.contains(needle), "settings.json leaked `{needle}`");
    }

    // Client logs/*.jsonl: flat JSON, zero "!" refused fields, zero secrets or room_id.
    for (idx, line) in log_lines.iter().enumerate() {
        let parsed: Json = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("log line {idx} invalid json: {e}: {line}"));
        let obj = parsed.as_object().expect("flat json object");
        for (k, v) in obj {
            assert!(!v.is_object() && !v.is_array(), "nested value in log line {idx} key `{k}`");
            assert!(v.as_str() != Some("!"), "refused field `!` in log line {idx} key `{k}`");
        }
        for needle in &forbidden_no_room_id {
            assert!(!line.contains(needle), "log line {idx} leaked `{needle}`: {line}");
        }
    }

    // Diagnose bundle: zero secrets/room_id, Tally <= 8192 B.
    let diagnose_root = data_dir.path().join("diagnose");
    let mut bundle_dirs: Vec<PathBuf> =
        fs::read_dir(&diagnose_root).expect("diagnose dir").flatten().map(|e| e.path()).collect();
    bundle_dirs.sort();
    assert_eq!(bundle_dirs.len(), 1, "expected 1 diagnose bundle");
    let tally_raw = fs::read_to_string(bundle_dirs[0].join("stats.json")).expect("stats.json");
    let tally: Json = serde_json::from_str(&tally_raw).expect("parse Tally JSON");
    let tally_bytes = serde_json::to_vec(&tally).expect("compact Tally JSON").len();
    assert!(tally_bytes <= 8192, "Tally must stay <= 8192 B, got {tally_bytes} B");

    let mut bundle_files = Vec::new();
    collect_files(&diagnose_root, &mut bundle_files);
    assert!(!bundle_files.is_empty());
    for file in &bundle_files {
        let content = fs::read_to_string(file).expect("read diagnose file");
        for needle in &forbidden_no_room_id {
            assert!(
                !content.contains(needle),
                "diagnose file {} leaked `{needle}`",
                file.display()
            );
        }
    }
    assert!(!srv_dir.path().join("rooms").exists(), "server disk must have no rooms/ files");

    // Structured observability assertions.
    let has_event =
        |code: &str| log_lines.iter().any(|l| l.contains(&format!(r#""event":"{code}""#)));
    for required in ["host.state", "host.room", "diagnose.wrote"] {
        assert!(has_event(required), "expected `{required}` in client logs");
    }
    assert!(!has_event("stats.full"), "stats.full must never trigger");
    assert_eq!(zutalk_log::refused(), refused_before, "zutalk_log::refused() must not increase");
}

struct ProxyKeys;

impl Directory for ProxyKeys {
    fn doc<'a>(&'a self, did: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>> {
        Box::pin(async move {
            let n = did.strip_prefix("did:zu:made-up-").and_then(|n| n.parse().ok()).unwrap_or(5);
            let keys = vec![(SecretKey::from_seed(&[n; 32]).public(), None)];
            Ok(Doc { did: did.into(), keys, handles: vec![], services: BTreeMap::new() })
        })
    }
}

struct ProxyNowhere;

impl Upstream for ProxyNowhere {
    fn slice(&self) -> u64 {
        0
    }
    fn per_second(&self) -> u64 {
        50
    }
    fn mint<'a>(&'a self, _: &'a str, _: u64) -> BoxFuture<'a, Result<Minted, Refusal>> {
        Box::pin(async { Err(Refusal(reqwest::StatusCode::BAD_GATEWAY, "upstream")) })
    }
}

/// Deterministic one-shot pause gate for `POST /rooms` requests carrying `"do":"make"`.
struct MakeGate {
    armed: AtomicBool,
    arrived: Notify,
    release: Notify,
}

impl MakeGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            armed: AtomicBool::new(false),
            arrived: Notify::new(),
            release: Notify::new(),
        })
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines() {
        if let Some((name, val)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
            && let Ok(n) = val.trim().parse::<usize>()
        {
            return n;
        }
    }
    0
}

async fn write_all_stream(stream: &TcpStream, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        stream.writable().await?;
        match stream.try_write(data) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

async fn read_some_stream(stream: &TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        stream.readable().await?;
        match stream.try_read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
}

async fn relay_connection(client: TcpStream, backend: SocketAddr, gate: Arc<MakeGate>) {
    let Ok(upstream) = TcpStream::connect(backend).await else { return };
    let client = Arc::new(client);
    let upstream = Arc::new(upstream);

    let c2u_client = Arc::clone(&client);
    let c2u_up = Arc::clone(&upstream);
    let c2u = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        let mut pending = Vec::new();
        loop {
            let Ok(n) = read_some_stream(&c2u_client, &mut buf).await else { break };
            if n == 0 {
                break;
            }
            if gate.armed.load(Ordering::Acquire)
                && (pending.is_empty() && buf[..n].starts_with(b"POST /rooms ")
                    || !pending.is_empty())
            {
                pending.extend_from_slice(&buf[..n]);
                let Some(header_end) = find_bytes(&pending, b"\r\n\r\n") else {
                    continue;
                };
                let body_start = header_end + 4;
                let content_len = parse_content_length(&pending[..header_end]);
                if pending.len() < body_start + content_len {
                    continue;
                }
                let is_make = find_bytes(&pending[body_start..], br#""do":"make""#).is_some();
                if is_make && gate.armed.swap(false, Ordering::AcqRel) {
                    gate.arrived.notify_one();
                    gate.release.notified().await;
                }
                if write_all_stream(&c2u_up, &pending).await.is_err() {
                    break;
                }
                pending.clear();
            } else if write_all_stream(&c2u_up, &buf[..n]).await.is_err() {
                break;
            }
        }
    });

    let u2c_up = Arc::clone(&upstream);
    let u2c_client = Arc::clone(&client);
    let u2c = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let Ok(n) = read_some_stream(&u2c_up, &mut buf).await else { break };
            if n == 0 {
                break;
            }
            if write_all_stream(&u2c_client, &buf[..n]).await.is_err() {
                break;
            }
        }
    });

    let _ = tokio::join!(c2u, u2c);
}

async fn serve_gated() -> (String, String, Arc<MakeGate>, Arc<Server>, tempfile::TempDir) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_origin = format!("http://{}", proxy_listener.local_addr().unwrap());

    let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = backend_listener.local_addr().unwrap();
    let backend_origin = format!("http://{backend_addr}");

    let server_cfg = ServerConfig::new(&proxy_origin, dir.path(), vec![]);
    let server = Server::open(
        server_cfg,
        Arc::new(ProxyKeys),
        [Arc::new(ProxyNowhere), Arc::new(ProxyNowhere)],
    )
    .unwrap();
    let running = server.clone();
    tokio::spawn(zutalk_server::serve(backend_listener, running));

    let gate = MakeGate::new();
    let accept_gate = Arc::clone(&gate);
    tokio::spawn(async move {
        while let Ok((client, _)) = proxy_listener.accept().await {
            tokio::spawn(relay_connection(client, backend_addr, Arc::clone(&accept_gate)));
        }
    });

    (proxy_origin, backend_origin, gate, server, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn rapid_captions_toggle_does_not_orphan_new_room_or_delete_new_lock() {
    let _store = STORE.lock().await;
    store(true);
    let (proxy_origin, backend_origin, gate, server, _srv_dir) = serve_gated().await;
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(config(dir.path(), &proxy_origin));
    app.until(|s| s.me.is_some()).await;

    // 1. Arm room R1.
    app.send(Command::HostArm { event: None, captions: true });
    let link1 = match app
        .until(|s| matches!(s.host, HostState::Armed { captions: true, link: Some(_), .. }))
        .await
        .host
    {
        HostState::Armed { link: Some(l), .. } => l,
        other => panic!("unexpected host state: {other:?}"),
    };
    let (_, room_id_1, _) = from_link(&link1);
    assert_eq!(read_lock(dir.path()).as_deref(), Some(room_id_1.as_str()));

    // 2. Pause the next `make` (R2) so R1's `gone` finishes while Hub is Arming R2 and after
    //    R2 has already written `host.lock = R2`.
    gate.arm();
    app.send(Command::HostArm { event: None, captions: false });
    app.send(Command::HostArm { event: None, captions: true });

    timeout(Duration::from_secs(10), gate.arrived.notified()).await.expect("R2 make reached gate");
    app.until(|s| matches!(s.host, HostState::Arming { captions: true, .. })).await;
    let room_id_2 = read_lock(dir.path()).expect("R2 wrote host.lock");
    assert_ne!(room_id_2, room_id_1);

    // Wait until R1's `net.host_gone` finishes on the server and sends stale `Msg::HostStopped`.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let gone_done = app.seen.of("job.done").iter().any(|l| l.text("kind") == Some("host.gone"));
        let r1_gone = reqwest::get(format!("{backend_origin}/rooms/{room_id_1}/events"))
            .await
            .unwrap()
            .status()
            == reqwest::StatusCode::NOT_FOUND;
        if gone_done && r1_gone {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "R1 teardown did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Give Hub time to process the stale `Msg::HostStopped` and confirm it did not clobber Arming.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        matches!(app.outlet.state.now().1.host, HostState::Arming { captions: true, .. }),
        "stale HostStopped must not reset Arming to Idle: {:?}",
        app.outlet.state.now().1.host
    );
    assert_eq!(
        read_lock(dir.path()).as_deref(),
        Some(room_id_2.as_str()),
        "R1 teardown must not delete R2's host.lock"
    );

    // 3. Release R2's `make` -> Hub transitions cleanly to Armed with R2's link.
    gate.release.notify_one();
    let link2 = match app
        .until(|s| {
            matches!(
                &s.host,
                HostState::Armed { captions: true, link: Some(l), .. } if l != &link1
            )
        })
        .await
        .host
    {
        HostState::Armed { link: Some(l), .. } => l,
        other => panic!("unexpected host state: {other:?}"),
    };
    let (_, armed_id_2, _) = from_link(&link2);
    assert_eq!(armed_id_2, room_id_2);
    assert_eq!(read_lock(dir.path()).as_deref(), Some(room_id_2.as_str()));

    // 4. R2 works end-to-end with a viewer and tears down cleanly on HostCancel.
    let mut viewer = Viewer::open(&link2).await;
    viewer.until(|ev, _| (ev == "slot").then_some(())).await;
    app.until(|s| s.host.viewers() == 1).await;

    app.send(Command::HostCancel);
    viewer.until(|ev, _| (ev == "gone").then_some(())).await;
    app.until(|s| s.host == HostState::Idle).await;
    assert!(!dir.path().join(HOST_LOCK_FILE).exists());
    let r2_after =
        reqwest::get(format!("{backend_origin}/rooms/{room_id_2}/events")).await.unwrap();
    assert_eq!(r2_after.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(app.seen.refused(), 0);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_recording_while_arming_cancels_in_flight_room_without_leak() {
    let _store = STORE.lock().await;
    store(true);
    let (proxy_origin, backend_origin, gate, server, _srv_dir) = serve_gated().await;
    let dir = tempfile::tempdir().expect("a folder");
    let mut app = App::with(config(dir.path(), &proxy_origin));
    app.until(|s| s.me.is_some()).await;

    // 1. Arm hosting while pausing the in-flight `make` request so Hub stays in `Arming`.
    gate.arm();
    app.send(Command::HostArm { event: Some(sample_event("ArmRace")), captions: true });
    timeout(Duration::from_secs(10), gate.arrived.notified()).await.expect("make reached gate");
    app.until(|s| matches!(s.host, HostState::Arming { captions: true, .. })).await;
    let room_id = read_lock(dir.path()).expect("host.lock written before make");

    // 2. Start recording while still Arming -> local-only Live session, in-flight make cancelled.
    app.send(Command::Start);
    let session =
        app.session(|s| s.is_some_and(|s| !s.outbound)).await.expect("local-only session started");
    assert!(!session.outbound);
    assert_eq!(session.viewers, 0);
    assert_eq!(session.event_title.as_deref(), Some("ArmRace"));
    app.until(|s| {
        matches!(s.host, HostState::Live { outbound: false, link: None, viewers: 0, .. })
    })
    .await;

    // 3. Release the in-flight `make` -> cancelled task immediately sends `gone` and clears lock.
    gate.release.notify_one();
    let lock_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while dir.path().join(HOST_LOCK_FILE).exists() {
        assert!(
            tokio::time::Instant::now() < lock_deadline,
            "host.lock did not clear after cancel"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let after = reqwest::get(format!("{backend_origin}/rooms/{room_id}/events")).await.unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(matches!(
        app.outlet.state.now().1.host,
        HostState::Live { outbound: false, link: None, viewers: 0, .. }
    ));

    // 4. Stop local recording cleanly.
    tokio::time::sleep(Duration::from_millis(200)).await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    app.until(|s| s.host == HostState::Idle).await;
    assert!(!app.core.is_hosting());
    assert_eq!(app.seen.refused(), 0);
    server.shutdown().await;
}
