//! login.md § The Relay: one sealed message per room, in memory, for five minutes, open to
//! every origin.

mod common;

use axum::http::{StatusCode, header};
use common::*;
use http_body_util::BodyExt;

const ROOM: &str = "/.well-known/zu-relay/0123456789abcdef0123456789abcdef";

async fn relay(bench: &Bench, method: &str, path: &str, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
    let answer = send(&bench.app, method, path, None, body).await;
    let cors = answer.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).cloned();
    assert_eq!(cors.as_ref().map(|v| v.to_str().unwrap()), Some("*"), "{method} {path}");
    let status = answer.status();
    (status, answer.into_body().collect().await.unwrap().to_bytes().to_vec())
}

#[tokio::test]
async fn a_room_takes_one_message() {
    let bench = bench();
    assert_eq!(relay(&bench, "GET", ROOM, vec![]).await.0, StatusCode::NOT_FOUND);
    assert!(relay(&bench, "POST", ROOM, b"sealed".to_vec()).await.0.is_success());
    assert_eq!(relay(&bench, "POST", ROOM, b"another".to_vec()).await.0, StatusCode::CONFLICT);
    for _ in 0..2 {
        assert_eq!(relay(&bench, "GET", ROOM, vec![]).await, (StatusCode::OK, b"sealed".to_vec()));
    }
    // The browser asks before it posts bytes from another origin.
    assert!(relay(&bench, "OPTIONS", ROOM, vec![]).await.0.is_success());
}

#[tokio::test]
async fn a_room_is_forgotten_after_its_life() {
    let bench = bench_with(|config| config.relay.1 = 300);
    assert!(relay(&bench, "POST", ROOM, b"sealed".to_vec()).await.0.is_success());
    assert_eq!(relay(&bench, "GET", ROOM, vec![]).await.0, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    assert_eq!(relay(&bench, "GET", ROOM, vec![]).await.0, StatusCode::NOT_FOUND);
    // And the room is free again.
    assert!(relay(&bench, "POST", ROOM, b"again".to_vec()).await.0.is_success());
    assert!(!String::from_utf8_lossy(&disk(&bench)).contains("sealed"), "memory only");
}

#[tokio::test]
async fn a_message_is_64_kib_at_most() {
    let bench = bench();
    let other = ROOM.replace("0123", "4567");
    assert_eq!(relay(&bench, "POST", ROOM, vec![1; 65537]).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(relay(&bench, "POST", &other, vec![1; 65536]).await.0.is_success());
    assert_eq!(relay(&bench, "GET", &other, vec![]).await.1.len(), 65536);
}

#[tokio::test]
async fn the_rooms_held_are_limited_and_named_by_32_hex_digits() {
    let bench = bench_with(|config| config.relay.0 = 2);
    let room = |n: u8| format!("/.well-known/zu-relay/{:032x}", n);
    relay(&bench, "POST", &room(1), b"m".to_vec()).await;
    relay(&bench, "POST", &room(2), b"m".to_vec()).await;
    let full = relay(&bench, "POST", &room(3), b"m".to_vec()).await.0;
    assert_eq!(full, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(relay(&bench, "GET", &room(2), vec![]).await.0, StatusCode::OK);
    for name in ["0123456789ABCDEF0123456789ABCDEF", "0123", "0123456789abcdef0123456789abcdeg"] {
        let path = format!("/.well-known/zu-relay/{name}");
        assert_eq!(relay(&bench, "GET", &path, vec![]).await.0, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(relay(&bench, "POST", &path, b"m".to_vec()).await.0, StatusCode::NOT_FOUND);
    }
    // No credential is needed, nor does one change anything.
    let answer = send(&bench.app, "GET", &room(1), Some("Bearer x"), vec![]).await;
    assert_eq!(answer.status(), StatusCode::OK);
}
