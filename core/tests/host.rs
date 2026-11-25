//! Hosted ephemeral live rooms: explicit `HostState` transition table, AES-256-GCM frame crypto
//! with fragment-only key delivery, atomic `host.lock` (room id only), stop-outbound keeping
//! local recording alive, cancellation/stop/sign-out teardown, crash recovery on next boot, and
//! unauthenticated refusal without network traffic (`P5.md` §3.3, `P5-02`).

mod common;
mod hub;

use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine};
use common::{Viewer, from_link, serve};
use futures_util::{SinkExt, StreamExt};
use hub::{App, STORE, config_on as config, store};
use serde_json::json;
use std::fs;
use std::time::Duration;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};
use zutalk_core::app::proto::{Command, HostEvent, HostState, Setup};
use zutalk_core::id::keys;
use zutalk_core::out::host::{
    HOST_LOCK_FILE, HostStepEvent, clear_lock, host_step, is_room_id, link, open, read_lock, seal,
    write_lock,
};
use zutalk_core::rt::supervise::End;

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
