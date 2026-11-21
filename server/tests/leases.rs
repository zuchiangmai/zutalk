//! Leases: a short-lived key costs what it could spend, drawn on what a resident has left.
//! Nobody can draw more than was granted, and no key is kept anywhere.

mod common;

use axum::http::StatusCode;
use common::*;
use std::sync::atomic::Ordering;
use zutalk_server::now;

#[tokio::test]
async fn a_key_costs_its_slice_and_the_answer_says_what_is_left() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    // Soniox: one connection's worth, an hour at 50 µ$ a second.
    let (status, key) = lease(&bench, 1, "soniox").await;
    assert_eq!(status, StatusCode::OK);
    assert!(key["key"].as_str().unwrap().starts_with(SECRET_KEY));
    assert_eq!(key["session_cap_s"], 3600);
    assert_eq!(key["left"], 10_000_000 - 180_000);
    assert_eq!(left(&bench, 1), 10_000_000 - 180_000);
    // OpenRouter: no cap on a connection.
    let (_, key) = lease(&bench, 1, "openrouter").await;
    assert_eq!(key["session_cap_s"], serde_json::Value::Null);
    assert_eq!(key["left"], 10_000_000 - 180_000 - 500_000);
    // The key is named for the request that asked for it.
    let minted = bench.openrouter.minted.lock().unwrap();
    assert!(minted[0].0.starts_with('r') && minted[0].1 == 500_000, "{minted:?}");
    drop(minted);
    // The ledger is a fold: it reads the same after a restart.
    let bench = restart(bench);
    assert_eq!(left(&bench, 1), 10_000_000 - 180_000 - 500_000);
}

#[tokio::test]
async fn a_key_cannot_hold_more_than_is_left() {
    let bench = bench();
    give(&bench, 1, 25).await;
    // 250,000 µ$: one whole Soniox key, then what remains, then nothing.
    let mut leases = Vec::new();
    for _ in 0..3 {
        leases.push(lease(&bench, 1, "soniox").await);
    }
    let held: Vec<u64> =
        bench.soniox.minted.lock().unwrap().iter().map(|(_, micros)| *micros).collect();
    assert_eq!(held, [180_000, 70_000]);
    assert_eq!(leases.iter().filter(|(s, _)| *s == StatusCode::OK).count(), 2);
    assert_eq!(leases[1].1["session_cap_s"], 1400);
    assert_eq!(leases[2], (StatusCode::PAYMENT_REQUIRED, serde_json::json!({ "error": "quota" })));
    assert_eq!(left(&bench, 1), 0);
}

#[tokio::test]
async fn a_key_is_drawn_on_the_grant_that_ends_first() {
    let bench = bench();
    give(&bench, 1, 100).await;
    let soon = give_until(&bench, 1, 100, now() + 60_000).await;
    assert_eq!(lease(&bench, 1, "soniox").await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 2_000_000 - 180_000);
    // Once the grant that ended first is over, the other is whole.
    assert_eq!(bench.server.left(&did(1), soon.at + 60_001), 1_000_000);
}

#[tokio::test]
async fn keys_asked_for_at_once_cannot_draw_the_same_money() {
    let bench = bench();
    give(&bench, 1, 30).await;
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let (app, sent) = (bench.app.clone(), ask(1, "soniox"));
            tokio::spawn(async move { post(&app, "/lease", sent).await })
        })
        .collect();
    let mut granted = 0;
    for task in tasks {
        granted += u64::from(task.await.unwrap().0 == StatusCode::OK);
    }
    let held: u64 = bench.soniox.minted.lock().unwrap().iter().map(|(_, micros)| micros).sum();
    assert_eq!(held, 300_000);
    assert_eq!(granted, 2);
    assert_eq!(left(&bench, 1), 0);
}

#[tokio::test]
async fn an_upstream_that_fails_holds_nothing() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    let before = ledger(&bench);
    bench.openrouter.down.store(true, Ordering::SeqCst);
    let sent = ask(1, "openrouter");
    assert_eq!(post(&bench.app, "/lease", sent.clone()).await.1["error"], "upstream");
    assert_eq!(ledger(&bench), before);
    assert_eq!(left(&bench, 1), 10_000_000);
    // The request was not kept, so it may be sent again.
    bench.openrouter.down.store(false, Ordering::SeqCst);
    assert_eq!(post(&bench.app, "/lease", sent).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 10_000_000 - 500_000);
}

#[tokio::test]
async fn no_key_is_kept_anywhere() {
    let bench = bench();
    give(&bench, 1, 1000).await;
    let (_, soniox) = lease(&bench, 1, "soniox").await;
    let (_, openrouter) = lease(&bench, 1, "openrouter").await;
    let disk = String::from_utf8_lossy(&disk(&bench)).into_owned();
    for key in [&soniox, &openrouter] {
        assert!(key["key"].as_str().unwrap().starts_with(SECRET_KEY));
    }
    assert!(!disk.contains(SECRET_KEY), "a key on disk");
    // What is kept is the request, signed, as the proof of what was drawn.
    let name = bench.soniox.minted.lock().unwrap()[0].0.clone();
    assert!(disk.contains(&format!("\"{name}\"")) && disk.contains("talk/lease"), "{disk}");
}

#[tokio::test]
async fn nothing_is_leased_without_a_grant_or_a_service() {
    let bench = bench();
    assert_eq!(
        lease(&bench, 1, "soniox").await,
        (StatusCode::PAYMENT_REQUIRED, serde_json::json!({ "error": "quota" }))
    );
    give(&bench, 1, 1000).await;
    let other = (StatusCode::BAD_REQUEST, serde_json::json!({ "error": "service" }));
    assert_eq!(lease(&bench, 1, "elsewhere").await, other);
    assert!(bench.soniox.minted.lock().unwrap().is_empty());
    assert_eq!(left(&bench, 1), 10_000_000);
}
