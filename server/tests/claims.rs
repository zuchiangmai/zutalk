//! Claims: a person carries the hours of live captions left on a community invite of the 0.8
//! line into this ledger. The old service retires the invite and says what was left, once; the
//! seconds are worth what live captions cost here. The token is kept nowhere.

mod common;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::{Json, Router, routing};
use common::*;
use serde_json::{Value as Said, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use zu::{Cid, Codec, Value};
use zutalk_server::{Invites, now};

const ADMIN: &str = "the-old-services-admin-token";
const TOKEN: &str = "zl-access-token-of-an-invite";

/// The 0.8 line's invite service, on a port of this machine: each token with the seconds it
/// has left and whether it was retired. It answers only to its admin token.
#[derive(Default)]
struct Old {
    invites: Mutex<HashMap<String, (u32, bool)>>,
    down: AtomicBool,
    asked: AtomicUsize,
}

async fn retire(
    State(old): State<Arc<Old>>,
    headers: HeaderMap,
    Json(body): Json<Said>,
) -> (StatusCode, Json<Said>) {
    old.asked.fetch_add(1, Ordering::SeqCst);
    if headers.get(AUTHORIZATION).is_none_or(|said| said != &format!("Bearer {ADMIN}")) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })));
    } else if old.down.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    let mut invites = old.invites.lock().unwrap();
    match invites.get_mut(body["access_token"].as_str().unwrap_or("")) {
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "invalid_token" }))),
        Some((left, true)) => {
            let said = json!({ "status": "already_retired", "remaining_seconds": *left });
            (StatusCode::OK, Json(said))
        }
        Some((left, retired)) => {
            *retired = true;
            (StatusCode::OK, Json(json!({ "status": "retired", "remaining_seconds": *left })))
        }
    }
}

/// A bench whose server knows the old service by `admin`, and the old service, with `TOKEN`
/// holding `seconds`.
async fn with_old(seconds: u32, admin: &str) -> (Bench, Arc<Old>) {
    _ = rustls::crypto::ring::default_provider().install_default();
    let old = Arc::new(Old::default());
    old.invites.lock().unwrap().insert(TOKEN.into(), (seconds, false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route("/v1/retire", routing::post(retire)).with_state(old.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let (http, key) = (reqwest::Client::new(), admin.to_owned());
    (bench_with(|config| config.invites = Some(Invites { http, url, key })), old)
}

/// `did(n)` claims what `token` has left, signed now and for this server.
async fn claim(bench: &Bench, n: u8, token: &str) -> (StatusCode, Said) {
    let body = map(&[("token", Value::Str(token.into())), ("aud", Value::Str(ORIGIN.into()))]);
    post(&bench.app, "/claim", delivery(&signed(n, n, "talk/claim", &body, now()), Some(&body)))
        .await
}

#[tokio::test]
async fn the_hours_left_on_an_invite_become_the_claimers_at_the_price_of_live_captions() {
    let (bench, old) = with_old(7200, ADMIN).await;
    let (status, said) = claim(&bench, 1, TOKEN).await;
    assert_eq!(status, StatusCode::OK, "{said}");
    // Two hours at 50 µ$ a second: two keys of an hour each.
    assert_eq!((said["seconds"].clone(), said["left"].clone()), (json!(7200), json!(360_000)));
    assert_eq!(left(&bench, 1), 360_000);
    assert_eq!(left(&bench, 2), 0);
    assert!(old.invites.lock().unwrap()[TOKEN].1, "the invite is retired at the old service");
    assert_eq!(lease(&bench, 1, "soniox").await.1["left"], 180_000);
    // The ledger is a fold: it reads the same after a restart.
    let bench = restart(bench);
    assert_eq!(left(&bench, 1), 180_000);
    assert_eq!(lease(&bench, 1, "soniox").await.1["left"], 0);
}

#[tokio::test]
async fn a_token_is_claimed_once() {
    let (bench, _old) = with_old(3600, ADMIN).await;
    assert_eq!(claim(&bench, 1, TOKEN).await.0, StatusCode::OK);
    let written = ledger(&bench);
    let refused = (StatusCode::CONFLICT, "claimed".to_owned());
    for n in [1, 2] {
        let (status, said) = claim(&bench, n, TOKEN).await;
        assert_eq!((status, said["error"].as_str().unwrap().to_owned()), refused);
    }
    assert_eq!(ledger(&bench), written);
    assert_eq!((left(&bench, 1), left(&bench, 2)), (180_000, 0));
}

#[tokio::test]
async fn a_claim_cut_short_after_the_old_service_retired_it_is_credited_when_sent_again() {
    let (bench, old) = with_old(3600, ADMIN).await;
    // As if the server stopped between the old service's answer and its own ledger.
    old.invites.lock().unwrap().insert(TOKEN.into(), (3600, true));
    assert_eq!(claim(&bench, 1, TOKEN).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 180_000);
}

#[tokio::test]
async fn a_token_the_old_service_does_not_know_is_refused_and_nothing_is_kept() {
    let (bench, _old) = with_old(3600, ADMIN).await;
    let (status, said) = claim(&bench, 1, "not-a-token-it-gave").await;
    assert_eq!((status, said["error"].as_str()), (StatusCode::NOT_FOUND, Some("invite")));
    assert!(ledger(&bench).is_empty());
}

#[tokio::test]
async fn while_the_old_service_cannot_answer_nothing_is_spent_and_a_later_claim_counts() {
    // Away, without its admin token, or not set up at all: the claim waits.
    let (bench, old) = with_old(3600, "not-the-admin-token").await;
    assert_eq!(
        claim(&bench, 1, TOKEN).await,
        (StatusCode::BAD_GATEWAY, json!({ "error": "upstream" }))
    );
    assert!(!old.invites.lock().unwrap()[TOKEN].1);
    let (bench, old) = with_old(3600, ADMIN).await;
    old.down.store(true, Ordering::SeqCst);
    assert_eq!(claim(&bench, 1, TOKEN).await.1["error"], "upstream");
    assert_eq!(claim(&bench_with(|_| {}), 1, TOKEN).await.1["error"], "upstream");
    assert!(ledger(&bench).is_empty());
    old.down.store(false, Ordering::SeqCst);
    assert_eq!(claim(&bench, 1, TOKEN).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 180_000);
}

#[tokio::test]
async fn a_claim_is_a_signed_request_like_any_other() {
    let (bench, old) = with_old(3600, ADMIN).await;
    let body = map(&[("token", Value::Str(TOKEN.into())), ("aud", Value::Str(ORIGIN.into()))]);
    let elsewhere = map(&[
        ("token", Value::Str(TOKEN.into())),
        ("aud", Value::Str("https://x.example".into())),
    ]);
    let without = map(&[("aud", Value::Str(ORIGIN.into()))]);
    let cases = [
        (signed(1, 1, "talk/claim", &elsewhere, now()), elsewhere, "audience"),
        (signed(2, 1, "talk/claim", &body, now()), body.clone(), "signer"),
        (signed(1, 1, "talk/claim", &body, now() - 10 * 60_000), body.clone(), "time"),
        (signed(1, 1, "talk/lease", &body, now()), body, "record"),
        (signed(1, 1, "talk/claim", &without, now()), without, "body"),
    ];
    for (record, body, refused) in cases {
        let said = post(&bench.app, "/claim", delivery(&record, Some(&body))).await;
        assert_eq!(said.1["error"], refused);
    }
    assert_eq!(old.asked.load(Ordering::SeqCst), 0, "nothing reached the old service");
    assert_eq!(left(&bench, 1), 0);
}

#[tokio::test]
async fn the_token_is_kept_nowhere_and_nobody_else_can_write_what_a_claim_carried() {
    let (bench, _old) = with_old(3600, ADMIN).await;
    assert_eq!(claim(&bench, 1, TOKEN).await.0, StatusCode::OK);
    let disk = disk(&bench);
    for secret in [TOKEN, ADMIN] {
        assert!(!disk.windows(secret.len()).any(|w| w == secret.as_bytes()), "{secret}");
    }
    // A coordinator's body shaped like what a claim carried is no grant, and not kept.
    let link = Value::Link(Cid::of(Codec::Drisl, b"a claim"));
    let shaped = map(&[("claim", link), ("to", Value::Str(did(2))), ("seconds", Value::Int(1))]);
    let record = signed(COORDINATOR, COORDINATOR, "talk/grant", &shaped, now());
    let said = post(&bench.app, "/grants", delivery(&record, Some(&shaped))).await;
    assert_eq!(said.1["error"], "body");
    assert_eq!(left(&bench, 2), 0);
}

#[tokio::test]
async fn what_a_claim_carried_is_drawn_after_every_grant_that_ends() {
    let (bench, _old) = with_old(3600, ADMIN).await;
    let soon = give_until(&bench, 1, 100, now() + 60_000).await;
    assert_eq!(claim(&bench, 1, TOKEN).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 1_000_000 + 180_000);
    assert_eq!(lease(&bench, 1, "soniox").await.0, StatusCode::OK);
    // The key was drawn on the grant: once it ends, the claim's hours are all there.
    assert_eq!(bench.server.left(&did(1), soon.at + 60_001), 180_000);
}
