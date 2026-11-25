//! Ephemeral slot-only live rooms: pure in-memory ciphertext slots delivered over `WatchStream`.
//! Every write is a signed `talk/room` record (`make`/`slot`/`gone`); `log` is rejected, nothing
//! touches disk, idle rooms expire after 2 minutes, and the sweeper runs on `Tasks`.

mod common;

use aes_gcm::aead::{Aead, KeyInit};
use axum::body::Body;
use axum::http::StatusCode;
use base64::{Engine, prelude::BASE64_STANDARD};
use common::*;
use std::time::Duration;
use zu::Value;
use zutalk_log::{Level, capture};
use zutalk_server::{ROOM_IDLE_TTL_MS, now, room_expired};

fn b64(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

fn slot(frame: &[u8]) -> (String, String, String) {
    let data = if frame.is_empty() { "-".to_owned() } else { b64(frame) };
    ("slot".into(), String::new(), data)
}

fn gone() -> (String, String, String) {
    ("gone".into(), String::new(), "-".into())
}

fn refused(status: StatusCode, code: &str) -> (StatusCode, serde_json::Value) {
    (status, serde_json::json!({ "error": code }))
}

#[test]
fn room_expired_boundary_table() {
    let ttl = ROOM_IDLE_TTL_MS;
    assert_eq!(ttl, 120_000);
    assert!(!room_expired(1_000, 1_000, ttl));
    assert!(!room_expired(1_000, 1_000 + ttl - 1, ttl));
    assert!(!room_expired(1_000, 1_000 + ttl, ttl));
    assert!(room_expired(1_000, 1_000 + ttl + 1, ttl));
    assert!(!room_expired(i64::MAX - 10, i64::MAX, ttl));
    assert!(!room_expired(0, 0, 0));
    assert!(room_expired(0, 1, 0));
}

#[tokio::test]
async fn only_the_owner_writes_and_log_is_rejected() {
    let bench = bench();
    let id = room(&bench, 1).await;
    // `do: "log"` is retired and rejected with 400 "do".
    assert_eq!(
        room_do(&bench, 1, &id, "log", b"one").await,
        refused(StatusCode::BAD_REQUEST, "do")
    );
    assert_eq!(room_do(&bench, 1, &id, "seal", b"").await, refused(StatusCode::BAD_REQUEST, "do"));
    assert_eq!(room_do(&bench, 1, &id, "slot", b"one").await.0, StatusCode::OK);
    // Non-owner cannot write or close.
    let not_yours = refused(StatusCode::FORBIDDEN, "owner");
    for what in ["slot", "gone", "log"] {
        assert_eq!(room_do(&bench, 2, &id, what, b"x").await, not_yours, "{what}");
    }
    // Duplicate id, missing room, and malformed id are rejected.
    assert_eq!(room_do(&bench, 2, &id, "make", b"").await, refused(StatusCode::CONFLICT, "taken"));
    let nowhere = "0".repeat(32);
    assert_eq!(
        room_do(&bench, 1, &nowhere, "slot", b"x").await,
        refused(StatusCode::NOT_FOUND, "room")
    );
    assert_eq!(
        room_do(&bench, 1, "../ledger", "make", b"").await,
        refused(StatusCode::BAD_REQUEST, "room")
    );
    // Signature, audience, and clock checks still apply.
    let write = |aud: &str, at: i64| {
        let body = map(&[
            ("room", Value::Str(id.clone())),
            ("do", Value::Str("slot".into())),
            ("frame", Value::Str(b64(b"two"))),
            ("aud", Value::Str(aud.into())),
        ]);
        delivery(&signed(1, 1, "talk/room", &body, at), Some(&body))
    };
    assert_eq!(
        code(&bench.app, "/rooms", write("https://other.example", now())).await.1,
        "audience"
    );
    assert_eq!(code(&bench.app, "/rooms", write(ORIGIN, now() - 2 * DAY)).await.1, "time");
    let mut changed: serde_json::Value = serde_json::from_str(&write(ORIGIN, now())).unwrap();
    changed["body"]["frame"] = b64(b"else").into();
    assert_eq!(code(&bench.app, "/rooms", changed.to_string()).await.1, "record");
    bench.keys.0.lock().unwrap().insert(did(1), secret(2).public());
    assert_eq!(code(&bench.app, "/rooms", act(1, &id, "slot", b"two")).await.1, "signer");
    // No `rooms/` directory is ever created on disk.
    assert!(!bench.dir.path().join("rooms").exists());
    // Nothing anyone else tried replaced the owner's slot.
    let mut watching = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(watching.take(1).await, [slot(b"one")]);
}

#[tokio::test]
async fn slot_only_holds_latest_reports_accurate_viewers_and_rejects_replay() {
    let bench = bench();
    let id = room(&bench, 1).await;
    let path = format!("/rooms/{id}/events");

    // Two concurrent viewers connect and receive the initial empty slot.
    let mut v1 = events(&bench.app, &path, None).await;
    let mut v2 = events(&bench.app, &path, None).await;
    assert_eq!(v1.take(1).await, [slot(b"")]);
    assert_eq!(v2.take(1).await, [slot(b"")]);

    let older = act(1, &id, "slot", b"older");
    tokio::time::sleep(Duration::from_millis(2)).await;
    let (status, body) = room_do(&bench, 1, &id, "slot", b"first").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["viewers"], 2);
    assert_eq!(v1.take(1).await, [slot(b"first")]);
    assert_eq!(v2.take(1).await, [slot(b"first")]);

    // Older timestamp is rejected as replay and does not overwrite the slot.
    assert_eq!(post(&bench.app, "/rooms", older).await, refused(StatusCode::CONFLICT, "replay"));

    // Dropping one viewer updates the reported viewer count immediately.
    drop(v2);
    tokio::time::sleep(Duration::from_millis(2)).await;
    let (status, body) = room_do(&bench, 1, &id, "slot", b"second").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["viewers"], 1);
    assert_eq!(v1.take(1).await, [slot(b"second")]);

    // A late viewer sees only the latest slot.
    let mut late = events(&bench.app, &path, None).await;
    assert_eq!(late.take(1).await, [slot(b"second")]);

    // Clearing the slot sends empty slot to both connected viewers.
    tokio::time::sleep(Duration::from_millis(2)).await;
    let (status, body) = room_do(&bench, 1, &id, "slot", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["viewers"], 2);
    assert_eq!(v1.take(1).await, [slot(b"")]);
    assert_eq!(late.take(1).await, [slot(b"")]);
}

#[tokio::test]
async fn gone_and_two_minute_idle_ttl_notify_viewers_and_close_stream() {
    let bench = bench();
    let seen = capture(Level::Debug);

    // 1. Explicit `gone` notifies online viewer and closes the stream cleanly.
    let id1 = room(&bench, 1).await;
    let mut w1 = events(&bench.app, &format!("/rooms/{id1}/events"), None).await;
    assert_eq!(w1.take(1).await, [slot(b"")]);
    assert_eq!(room_do(&bench, 1, &id1, "gone", b"").await.0, StatusCode::OK);
    assert_eq!(w1.next().await, Some(gone()));
    assert_eq!(w1.next().await, None);
    let after = send(&bench.app, "GET", &format!("/rooms/{id1}/events"), None, Body::empty()).await;
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        room_do(&bench, 1, &id1, "slot", b"x").await,
        refused(StatusCode::NOT_FOUND, "room")
    );

    // 2. 2-minute idle TTL: alive at TTL - 1, refreshed on slot write, swept at TTL + 1.
    let id2 = room(&bench, 1).await;
    let mut w2 = events(&bench.app, &format!("/rooms/{id2}/events"), None).await;
    assert_eq!(w2.take(1).await, [slot(b"")]);
    assert_eq!(bench.server.sweep(now() + ROOM_IDLE_TTL_MS - 1), 0);
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(room_do(&bench, 1, &id2, "slot", b"alive").await.0, StatusCode::OK);
    assert_eq!(w2.take(1).await, [slot(b"alive")]);

    let removed = bench.server.sweep(now() + ROOM_IDLE_TTL_MS + 1);
    assert_eq!(removed, 1);
    assert_eq!(w2.next().await, Some(gone()));
    assert_eq!(w2.next().await, None);
    let after2 =
        send(&bench.app, "GET", &format!("/rooms/{id2}/events"), None, Body::empty()).await;
    assert_eq!(after2.status(), StatusCode::NOT_FOUND);

    let sweeps = seen.of("server.sweep");
    assert_eq!(sweeps.len(), 2, "{sweeps:?}");
    assert_eq!(sweeps[0].u64("removed"), Some(0));
    assert_eq!(sweeps[1].u64("removed"), Some(1));
    assert_eq!(seen.refused(), 0);
}

#[tokio::test]
async fn ephemeral_rooms_do_not_persist_across_restart_and_hold_no_plaintext() {
    let bench = bench();
    let said = "the words said in the room, in plain text";
    let cipher = aes_gcm::Aes256Gcm::new(&[7; 32].into());
    let nonce = [1; 12];
    let frame = [&nonce[..], &cipher.encrypt(&nonce.into(), said.as_bytes()).unwrap()].concat();
    let id = room(&bench, 1).await;
    assert_eq!(room_do(&bench, 1, &id, "slot", &frame).await.0, StatusCode::OK);

    let mut reading = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(reading.take(1).await, [slot(&frame)]);
    let on_disk = String::from_utf8_lossy(&disk(&bench)).into_owned();
    assert!(!bench.dir.path().join("rooms").exists());
    for place in [&on_disk, reading.wire()] {
        assert!(!place.contains(said) && !place.contains(&b64(said.as_bytes())));
    }
    assert!(!on_disk.contains(&b64(&frame)), "ephemeral slot must never touch disk");

    // Restarting the server drops all in-memory rooms cleanly.
    let bench = restart(bench);
    let after = send(&bench.app, "GET", &format!("/rooms/{id}/events"), None, Body::empty()).await;
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        room_do(&bench, 1, &id, "slot", &frame).await,
        refused(StatusCode::NOT_FOUND, "room")
    );
}

#[tokio::test]
async fn rooms_enforce_slot_size_owner_count_and_viewer_caps() {
    let bench = bench_with(|config| (config.room.0, config.room.1, config.room.2) = (12, 2, 2));
    let id = room(&bench, 1).await;
    let _second = room(&bench, 1).await;
    let third = format!("{:032x}", 3);
    assert_eq!(
        room_do(&bench, 1, &third, "make", b"").await,
        refused(StatusCode::TOO_MANY_REQUESTS, "rooms")
    );
    // Another owner still has their own quota.
    let _other = room(&bench, 2).await;

    // 12 bytes of base64 hold at most 9 raw bytes; 10 raw bytes encode to 16 base64 bytes -> 413 full.
    assert_eq!(room_do(&bench, 1, &id, "slot", b"123456789").await.0, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(
        room_do(&bench, 1, &id, "slot", b"1234567890").await,
        refused(StatusCode::PAYLOAD_TOO_LARGE, "full")
    );

    let path = format!("/rooms/{id}/events");
    let (_one, _two) =
        (events(&bench.app, &path, None).await, events(&bench.app, &path, None).await);
    let third_viewer = send(&bench.app, "GET", &path, None, Body::empty()).await;
    assert_eq!(third_viewer.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn server_shutdown_cancels_supervised_sweeper() {
    let bench = bench();
    let seen = capture(Level::Info);
    bench.server.start_sweeper();
    // Give the spawned task a tick to emit `task.start`.
    tokio::task::yield_now().await;
    tokio::time::timeout(Duration::from_secs(2), bench.server.shutdown())
        .await
        .expect("shutdown must cancel and drain server.sweep promptly");

    let starts: Vec<_> = seen
        .of("task.start")
        .into_iter()
        .filter(|l| l.text("tree") == Some("server") && l.text("task") == Some("server.sweep"))
        .collect();
    let ends: Vec<_> = seen
        .of("task.end")
        .into_iter()
        .filter(|l| l.text("tree") == Some("server") && l.text("task") == Some("server.sweep"))
        .collect();
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].text("code"), Some("done"));
    assert_eq!(seen.refused(), 0);
}
