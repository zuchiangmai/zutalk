//! Who is asking: a request that acts for someone is a record they signed, made for this
//! server, made now, sent once, with a key that is the DID's for its time.

mod common;

use axum::body::Body;
use axum::http::{StatusCode, header};
use common::*;
use zu::{Value, auth::WINDOW};
use zutalk_server::now;

/// A request of `did(by)`'s, signed with `secret(signer)`, made for `aud` at `at`.
fn request(signer: u8, by: u8, aud: &str, at: i64) -> String {
    let body = map(&[("service", Value::Str("soniox".into())), ("aud", Value::Str(aud.into()))]);
    delivery(&signed(signer, by, "talk/lease", &body, at), Some(&body))
}

async fn asked(bench: &Bench, sent: String) -> (StatusCode, String) {
    code(&bench.app, "/lease", sent).await
}

#[tokio::test]
async fn a_signed_record_says_who_is_asking() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    assert_eq!(asked(&bench, request(1, 1, ORIGIN, now())).await.0, StatusCode::OK);
    // No record, or something else in its place, acts for nobody.
    let (status, _) = post(&bench.app, "/lease", Body::empty()).await;
    assert!(status.is_client_error(), "{status}");
    let garbage = serde_json::json!({ "record": "x", "body": null }).to_string();
    assert_eq!(asked(&bench, garbage).await, (StatusCode::BAD_REQUEST, "record".into()));
    // A grant is no request for a key, nor is a request for a key a grant.
    let (grant, body) = grant(COORDINATOR, "g", &did(1), 1, now() + DAY, &[]);
    let refused = (StatusCode::BAD_REQUEST, "record".to_owned());
    assert_eq!(asked(&bench, delivery(&grant, Some(&body))).await, refused);
    assert_eq!(code(&bench.app, "/grants", request(1, 1, ORIGIN, now())).await, refused);
}

#[tokio::test]
async fn a_request_for_another_host_is_refused() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    for aud in ["https://other.example", "http://talk.example", "https://talk.example:8443", ""] {
        let refused = (StatusCode::UNAUTHORIZED, "audience".to_owned());
        assert_eq!(asked(&bench, request(1, 1, aud, now())).await, refused, "{aud}");
    }
    let body = map(&[("service", Value::Str("soniox".into()))]);
    let unnamed = delivery(&signed(1, 1, "talk/lease", &body, now()), Some(&body));
    assert_eq!(asked(&bench, unnamed).await, (StatusCode::UNAUTHORIZED, "audience".into()));
    assert!(bench.soniox.minted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_request_counts_for_five_minutes() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    let margin = 5_000;
    for off in [WINDOW as i64 + margin, -(WINDOW as i64) - margin] {
        let refused = (StatusCode::UNAUTHORIZED, "time".to_owned());
        assert_eq!(asked(&bench, request(1, 1, ORIGIN, now() + off)).await, refused);
        let near = off - 2 * margin * off.signum();
        assert_eq!(asked(&bench, request(1, 1, ORIGIN, now() + near)).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn a_request_counts_once() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    let sent = request(1, 1, ORIGIN, now());
    assert_eq!(asked(&bench, sent.clone()).await.0, StatusCode::OK);
    let replay = (StatusCode::CONFLICT, "replay".to_owned());
    assert_eq!(asked(&bench, sent.clone()).await, replay);
    assert_eq!(asked(&bench, sent.clone()).await, replay);
    assert_eq!(bench.soniox.minted.lock().unwrap().len(), 1);
    // The ledger keeps the request: it is refused after a restart too.
    let bench = restart(bench);
    assert_eq!(asked(&bench, sent).await, replay);
    assert_eq!(left(&bench, 1), 10_000_000 - 180_000);
}

#[tokio::test]
async fn the_key_must_be_the_dids_for_its_time() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    // Signed well, but with a key that is not `did(1)`'s.
    let sent = request(2, 1, ORIGIN, now());
    let signer = (StatusCode::FORBIDDEN, "signer".to_owned());
    assert_eq!(asked(&bench, sent.clone()).await, signer);
    // A request that did not pass is not remembered: once the key is the DID's, it counts.
    bench.keys.0.lock().unwrap().insert(did(1), secret(2).public());
    assert_eq!(asked(&bench, sent).await.0, StatusCode::OK);
    // And the key it had before no longer speaks for it.
    assert_eq!(asked(&bench, request(1, 1, ORIGIN, now())).await, signer);
}

#[tokio::test]
async fn a_changed_request_is_refused() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    let mut sent: serde_json::Value = serde_json::from_str(&request(1, 1, ORIGIN, now())).unwrap();
    // A body other than the one signed.
    let changed =
        map(&[("service", Value::Str("openrouter".into())), ("aud", Value::Str(ORIGIN.into()))]);
    let signed_body = std::mem::replace(&mut sent["body"], json(&changed));
    assert_eq!(asked(&bench, sent.to_string()).await, (StatusCode::BAD_REQUEST, "record".into()));
    // A signature that is not over this record: another time.
    sent["body"] = signed_body;
    sent["record"]["at"] = (sent["record"]["at"].as_i64().unwrap() + 1).into();
    assert_eq!(asked(&bench, sent.to_string()).await, (StatusCode::BAD_REQUEST, "record".into()));
    assert!(bench.soniox.minted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn answers_are_not_to_be_stored_and_health_is_public() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    let health = send(&bench.app, "GET", "/healthz", None, Body::empty()).await;
    assert_eq!(health.status(), StatusCode::OK);
    for answer in [
        health,
        send(&bench.app, "POST", "/lease", None, ask(1, "soniox")).await,
        send(&bench.app, "POST", "/lease", None, Body::empty()).await,
        send(&bench.app, "GET", "/.well-known/zu-relay/none", None, Body::empty()).await,
        send(&bench.app, "GET", "/nowhere", None, Body::empty()).await,
    ] {
        assert_eq!(answer.headers()[header::CACHE_CONTROL], "no-store", "{}", answer.status());
    }
}

#[tokio::test]
async fn a_body_larger_than_64_kib_is_refused() {
    let bench = bench();
    let grant = serde_json::json!({ "record": "x".repeat(64 << 10), "body": {} }).to_string();
    assert_eq!(post(&bench.app, "/grants", grant).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    let lease = serde_json::json!({ "record": "x".repeat(64 << 10), "body": {} }).to_string();
    assert_eq!(post(&bench.app, "/lease", lease).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}
