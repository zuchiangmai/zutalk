//! Rooms: an append-only log of sealed frames and a slot for the latest one. Every write is a
//! record its owner signed; whoever has the id reads, without the key to open anything.

mod common;

use aes_gcm::aead::{Aead, KeyInit};
use axum::body::Body;
use axum::http::StatusCode;
use base64::{Engine, prelude::BASE64_STANDARD};
use common::*;
use zu::Value;
use zutalk_server::now;

fn b64(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

fn log(i: usize, frame: &[u8]) -> (String, String, String) {
    ("log".into(), i.to_string(), b64(frame))
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

#[tokio::test]
async fn only_the_owner_writes() {
    let bench = bench();
    let id = room(&bench, 1).await;
    assert_eq!(room_do(&bench, 1, &id, "log", b"one").await.0, StatusCode::OK);
    let not_yours = refused(StatusCode::FORBIDDEN, "owner");
    for what in ["log", "slot", "gone"] {
        assert_eq!(room_do(&bench, 2, &id, what, b"x").await, not_yours, "{what}");
    }
    // A room is made once; one that is not there takes nothing; a write must say what it is.
    let taken = refused(StatusCode::CONFLICT, "taken");
    assert_eq!(room_do(&bench, 2, &id, "make", b"").await, taken);
    let nowhere = "0".repeat(32);
    assert_eq!(
        room_do(&bench, 1, &nowhere, "log", b"x").await,
        refused(StatusCode::NOT_FOUND, "room")
    );
    let bad = refused(StatusCode::BAD_REQUEST, "room");
    assert_eq!(room_do(&bench, 1, "../ledger", "make", b"").await, bad);
    assert_eq!(room_do(&bench, 1, &id, "seal", b"").await, refused(StatusCode::BAD_REQUEST, "do"));
    // Nothing anyone else tried reached the log.
    let mut events = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(events.take(2).await, [log(1, b"one"), slot(b"")]);
}

#[tokio::test]
async fn a_write_is_the_owners_record_for_this_server_now_and_counts_once() {
    let bench = bench();
    let id = room(&bench, 1).await;
    let sent = act(1, &id, "log", b"one");
    assert_eq!(post(&bench.app, "/rooms", sent.clone()).await.0, StatusCode::OK);
    assert_eq!(
        post(&bench.app, "/rooms", sent.clone()).await,
        refused(StatusCode::CONFLICT, "replay")
    );
    // Kept with the room: refused after a restart too.
    let bench = restart(bench);
    assert_eq!(post(&bench.app, "/rooms", sent).await, refused(StatusCode::CONFLICT, "replay"));
    // The one way anyone acts here: a write made for another server, made long ago, changed
    // since it was signed, or signed with a key that is not the owner's, does nothing.
    let write = |aud: &str, at: i64| {
        let body = map(&[
            ("room", Value::Str(id.clone())),
            ("do", Value::Str("log".into())),
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
    assert_eq!(code(&bench.app, "/rooms", act(1, &id, "log", b"two")).await.1, "signer");
    let mut events = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(events.take(2).await, [log(1, b"one"), slot(b"")]);
}

#[tokio::test]
async fn whoever_has_the_id_reads_and_resumes_without_gap_or_repeat() {
    let bench = bench();
    let id = room(&bench, 1).await;
    let frames: Vec<Vec<u8>> = (1..=5).map(|i| vec![i; 3]).collect();
    for frame in &frames[..3] {
        room_do(&bench, 1, &id, "log", frame).await;
    }
    let path = format!("/rooms/{id}/events");
    let mut all = events(&bench.app, &path, None).await;
    assert_eq!(
        all.take(4).await,
        [log(1, &frames[0]), log(2, &frames[1]), log(3, &frames[2]), slot(b"")]
    );
    // Written while it listens: it hears each once, in order.
    for frame in &frames[3..] {
        assert_eq!(room_do(&bench, 1, &id, "log", frame).await.1["viewers"], 1);
    }
    assert_eq!(all.take(2).await, [log(4, &frames[3]), log(5, &frames[4])]);
    // From a position, by query or by the id of the last event seen.
    let mut after = events(&bench.app, &format!("{path}?after=3"), None).await;
    assert_eq!(after.take(3).await, [log(4, &frames[3]), log(5, &frames[4]), slot(b"")]);
    let mut resumed = events(&bench.app, &format!("{path}?after=1"), Some("4")).await;
    assert_eq!(resumed.take(2).await, [log(5, &frames[4]), slot(b"")]);
    let mut beyond = events(&bench.app, &path, Some("9")).await;
    assert_eq!(beyond.take(1).await, [slot(b"")]);
    let (status, _) = post(&bench.app, "/rooms/nowhere/events", Body::empty()).await;
    assert!(status.is_client_error());
    let nowhere = send(&bench.app, "GET", "/rooms/nowhere/events", None, Body::empty()).await;
    assert_eq!(nowhere.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_slot_holds_only_the_latest_frame() {
    let bench = bench();
    let id = room(&bench, 1).await;
    let path = format!("/rooms/{id}/events");
    let mut watching = events(&bench.app, &path, None).await;
    assert_eq!(watching.take(1).await, [slot(b"")]);
    assert_eq!(room_do(&bench, 1, &id, "slot", b"first").await.1["viewers"], 1);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    room_do(&bench, 1, &id, "slot", b"second").await;
    assert_eq!(watching.take(2).await, [slot(b"first"), slot(b"second")]);
    // Who comes later sees only the latest; nothing of it went into the log, or onto the disk.
    let mut later = events(&bench.app, &path, None).await;
    assert_eq!(later.take(1).await, [slot(b"second")]);
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    room_do(&bench, 1, &id, "slot", b"").await;
    assert_eq!(watching.take(1).await, [slot(b"")]);
    assert_eq!(later.take(1).await, [slot(b"")]);
    assert!(!String::from_utf8_lossy(&disk(&bench)).contains(&b64(b"second")));
}

#[tokio::test]
async fn a_slot_is_never_put_back_to_an_older_frame() {
    let bench = bench();
    let id = room(&bench, 1).await;
    let older = act(1, &id, "slot", b"older");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert_eq!(room_do(&bench, 1, &id, "slot", b"newer").await.0, StatusCode::OK);
    // Sent late, or sent again: refused, and the slot keeps what it has.
    assert_eq!(post(&bench.app, "/rooms", older).await, refused(StatusCode::CONFLICT, "replay"));
    let mut watching = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(watching.take(1).await, [slot(b"newer")]);
}

#[tokio::test]
async fn whoever_is_reading_hears_that_a_room_is_gone() {
    let bench = bench();
    let id = room(&bench, 1).await;
    room_do(&bench, 1, &id, "log", b"one").await;
    let mut watching = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(watching.take(2).await, [log(1, b"one"), slot(b"")]);
    assert_eq!(room_do(&bench, 1, &id, "gone", b"").await.0, StatusCode::OK);
    assert_eq!(watching.next().await, Some(gone()));
    assert_eq!(watching.next().await, None);
    let after = send(&bench.app, "GET", &format!("/rooms/{id}/events"), None, Body::empty()).await;
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
    assert!(!bench.dir.path().join("rooms").join(&id).exists());
    assert_eq!(room_do(&bench, 1, &id, "log", b"x").await, refused(StatusCode::NOT_FOUND, "room"));
}

#[tokio::test]
async fn a_room_survives_a_restart_and_its_slot_does_not() {
    let bench = bench();
    let id = room(&bench, 1).await;
    room_do(&bench, 1, &id, "log", b"one").await;
    room_do(&bench, 1, &id, "log", b"two").await;
    room_do(&bench, 1, &id, "slot", b"now").await;
    let bench = restart(bench);
    let mut reading = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(reading.take(3).await, [log(1, b"one"), log(2, b"two"), slot(b"")]);
    assert_eq!(room_do(&bench, 1, &id, "log", b"three").await.0, StatusCode::OK);
    assert_eq!(room_do(&bench, 2, &id, "log", b"three").await.1["error"], "owner");
    assert_eq!(reading.take(1).await, [log(3, b"three")]);
}

#[tokio::test]
async fn no_word_of_a_sealed_frame_is_on_the_disk_or_the_wire() {
    let bench = bench();
    let said = "the words said in the room, in plain text";
    let cipher = aes_gcm::Aes256Gcm::new(&[7; 32].into());
    let nonce = [1; 12];
    let frame = [&nonce[..], &cipher.encrypt(&nonce.into(), said.as_bytes()).unwrap()].concat();
    let id = room(&bench, 1).await;
    room_do(&bench, 1, &id, "log", &frame).await;
    room_do(&bench, 1, &id, "slot", &frame).await;
    let mut reading = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(reading.take(2).await, [log(1, &frame), slot(&frame)]);
    let disk = String::from_utf8_lossy(&disk(&bench)).into_owned();
    for place in [&disk, reading.wire()] {
        assert!(!place.contains(said) && !place.contains(&b64(said.as_bytes())));
    }
    assert!(disk.contains(&b64(&frame)), "the ciphertext is kept as it came");
}

#[tokio::test]
async fn rooms_have_limits() {
    let bench = bench_with(|config| (config.room.0, config.room.1, config.room.2) = (12, 2, 2));
    let id = room(&bench, 1).await;
    room(&bench, 1).await;
    let third = format!("{:032x}", 3);
    assert_eq!(
        room_do(&bench, 1, &third, "make", b"").await,
        refused(StatusCode::TOO_MANY_REQUESTS, "rooms")
    );
    room(&bench, 2).await;
    assert_eq!(room_do(&bench, 1, &id, "log", b"").await.1["error"], "frame");
    // Twelve bytes of base64 hold nine of frame.
    assert_eq!(room_do(&bench, 1, &id, "log", b"123456").await.0, StatusCode::OK);
    assert_eq!(room_do(&bench, 1, &id, "log", b"1234").await.1["error"], "full");
    assert_eq!(room_do(&bench, 1, &id, "log", b"123").await.0, StatusCode::OK);
    let path = format!("/rooms/{id}/events");
    let (_one, _two) =
        (events(&bench.app, &path, None).await, events(&bench.app, &path, None).await);
    let third = send(&bench.app, "GET", &path, None, Body::empty()).await;
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_room_not_written_to_for_long_is_removed() {
    let bench = bench();
    let id = room(&bench, 1).await;
    let mut watching = events(&bench.app, &format!("/rooms/{id}/events"), None).await;
    assert_eq!(watching.take(1).await, [slot(b"")]);
    bench.server.sweep(now() + 29 * DAY);
    room_do(&bench, 1, &id, "slot", b"now").await;
    assert_eq!(watching.take(1).await, [slot(b"now")]);
    bench.server.sweep(now() + 31 * DAY);
    assert_eq!(watching.next().await, Some(gone()));
    assert_eq!(room_do(&bench, 1, &id, "log", b"x").await.1["error"], "room");
}
