//! Per-service UTC daily quota tracking, enforcement, POST /quota, and supervised ledger tasks.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use zu::{Cid, Codec, Path, Record, Value};
use zutalk_log::{Level, capture};
use zutalk_server::{
    DAY_MS, DEFAULT_DAILY_OPENROUTER_TIMES, DEFAULT_DAILY_SONIOX_TIMES, ServiceDaily,
    daily_service, day_bounds, now,
};

/// `did(n)` asks for its daily quota report, signed now and for this server.
fn quota_ask(n: u8) -> String {
    let body = map(&[("aud", Value::Str(ORIGIN.into()))]);
    delivery(&signed(n, n, "talk/quota", &body, now()), Some(&body))
}

#[test]
fn day_bounds_and_daily_service_rules() {
    // UTC midnight boundaries and negative timestamps.
    assert_eq!(day_bounds(0), (0, DAY_MS));
    assert_eq!(day_bounds(DAY_MS - 1), (0, DAY_MS));
    assert_eq!(day_bounds(DAY_MS), (DAY_MS, 2 * DAY_MS));
    assert_eq!(day_bounds(-1), (-DAY_MS, 0));

    // Default daily caps match Settings.dc.html.
    assert_eq!(DEFAULT_DAILY_SONIOX_TIMES, 40);
    assert_eq!(DEFAULT_DAILY_OPENROUTER_TIMES, 30);

    // No grant: limit and remaining are both 0.
    assert_eq!(
        daily_service(0, 40, false, false),
        ServiceDaily { used: 0, limit: 0, remaining: 0 }
    );
    // Grant exists, balance spent: limit reflects cap, remaining is 0.
    assert_eq!(
        daily_service(5, 40, true, false),
        ServiceDaily { used: 5, limit: 40, remaining: 0 }
    );
    // Grant and balance exist, under cap.
    assert_eq!(
        daily_service(3, 40, true, true),
        ServiceDaily { used: 3, limit: 40, remaining: 37 }
    );
    // Grant and balance exist, at or above cap.
    assert_eq!(
        daily_service(40, 40, true, true),
        ServiceDaily { used: 40, limit: 40, remaining: 0 }
    );
    assert_eq!(
        daily_service(45, 40, true, true),
        ServiceDaily { used: 45, limit: 40, remaining: 0 }
    );
}

#[tokio::test]
async fn quota_tracks_per_service_daily_usage_and_resets_next_day() {
    let bench = bench_with(|config| config.daily = [2, 1]);
    give(&bench, 1, 1000).await;

    let (status, initial) = post(&bench.app, "/quota", quota_ask(1)).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["left"], 10_000_000);
    assert_eq!(initial["services"]["soniox"], json!({ "used": 0, "limit": 2, "remaining": 2 }));
    assert_eq!(initial["services"]["openrouter"], json!({ "used": 0, "limit": 1, "remaining": 1 }));
    let resets_at = initial["resets_at"].as_i64().unwrap();
    assert!(resets_at > now() && resets_at <= now() + DAY_MS);

    // Two Soniox leases succeed and carry updated additive quota payloads.
    let (s1, b1) = lease(&bench, 1, "soniox").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1["quota"]["services"]["soniox"], json!({ "used": 1, "limit": 2, "remaining": 1 }));
    let (s2, b2) = lease(&bench, 1, "soniox").await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["quota"]["services"]["soniox"], json!({ "used": 2, "limit": 2, "remaining": 0 }));

    // Third Soniox lease hits the daily cap with 402 quota.daily (balance is still non-zero).
    let (s3, b3) = lease(&bench, 1, "soniox").await;
    assert_eq!((s3, b3), (StatusCode::PAYMENT_REQUIRED, json!({ "error": "quota.daily" })));

    // OpenRouter still has its own 1/day budget.
    let (o1, ob1) = lease(&bench, 1, "openrouter").await;
    assert_eq!(o1, StatusCode::OK);
    assert_eq!(
        ob1["quota"]["services"]["openrouter"],
        json!({ "used": 1, "limit": 1, "remaining": 0 })
    );
    let (o2, ob2) = lease(&bench, 1, "openrouter").await;
    assert_eq!((o2, ob2), (StatusCode::PAYMENT_REQUIRED, json!({ "error": "quota.daily" })));

    // At resets_at (next UTC day), daily usage resets while balance remains.
    let tomorrow = bench.server.quota(&did(1), resets_at);
    assert_eq!(tomorrow.soniox, ServiceDaily { used: 0, limit: 2, remaining: 2 });
    assert_eq!(tomorrow.openrouter, ServiceDaily { used: 0, limit: 1, remaining: 1 });
    assert!(tomorrow.left > 0);

    // Restarting the server replays the exact same daily usage from disk.
    let dir = bench.dir.clone();
    bench.server.shutdown().await;
    drop(bench);
    let mut cfg = zutalk_server::Config::new(ORIGIN, dir.path(), vec![did(COORDINATOR)]);
    cfg.daily = [2, 1];
    let keys = std::sync::Arc::new(Keys::default());
    let soniox =
        std::sync::Arc::new(Script { slice: 180_000, per_second: 50, ..Script::default() });
    let openrouter = std::sync::Arc::new(Script { slice: 500_000, ..Script::default() });
    let reopened = zutalk_server::Server::open(
        cfg,
        keys,
        std::sync::Arc::new(Hosting::default()),
        [soniox, openrouter],
    )
    .unwrap();
    let replayed = reopened.quota(&did(1), now());
    assert_eq!(replayed.soniox, ServiceDaily { used: 2, limit: 2, remaining: 0 });
    assert_eq!(replayed.openrouter, ServiceDaily { used: 1, limit: 1, remaining: 0 });
}

#[tokio::test]
async fn concurrent_leases_cannot_exceed_daily_cap() {
    let bench = bench_with(|config| config.daily = [2, 2]);
    give(&bench, 1, 1000).await;

    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let (app, sent) = (bench.app.clone(), ask(1, "soniox"));
            tokio::spawn(async move { post(&app, "/lease", sent).await })
        })
        .collect();
    let mut ok = 0;
    let mut daily = 0;
    for task in tasks {
        let (status, body) = task.await.unwrap();
        if status == StatusCode::OK {
            ok += 1;
        } else if status == StatusCode::PAYMENT_REQUIRED && body["error"] == "quota.daily" {
            daily += 1;
        } else {
            panic!("unexpected response: {status} {body}");
        }
    }
    assert_eq!(ok, 2);
    assert_eq!(daily, 3);
    assert_eq!(bench.soniox.minted.lock().unwrap().len(), 2);
    bench.server.shutdown().await;
}

#[tokio::test]
async fn quota_endpoint_requires_signed_person_record_and_logs_route() {
    let bench = bench_with(|config| config.daily = [1, 1]);
    give(&bench, 1, 1000).await;
    let seen = capture(Level::Info);

    // Valid POST /quota succeeds.
    let (status, _) = post(&bench.app, "/quota", quota_ask(1)).await;
    assert_eq!(status, StatusCode::OK);

    // Expired timestamp, wrong aud, wrong record type are refused.
    let good_body = map(&[("aud", Value::Str(ORIGIN.into()))]);
    let wrong_aud = map(&[("aud", Value::Str("https://other.example".into()))]);
    let cases = [
        (signed(1, 1, "talk/quota", &good_body, now() - 10 * 60_000), good_body.clone(), "time"),
        (signed(1, 1, "talk/quota", &wrong_aud, now()), wrong_aud, "audience"),
        (signed(1, 1, "talk/lease", &good_body, now()), good_body.clone(), "record"),
    ];
    for (record, body, expected) in cases {
        let (st, err) = code(&bench.app, "/quota", delivery(&record, Some(&body))).await;
        assert!(st.is_client_error(), "expected 4xx, got {st}");
        assert_eq!(err, expected);
    }

    // Local did:key is refused with 403 signer.
    let key = secret(1);
    let local = zu::did::local(&key.public());
    let cid = Cid::of(Codec::Drisl, &good_body.encode());
    let path = Path::new(&local, "talk/quota", "q-local-1").unwrap();
    let local_record = Record::sign(&key, path, Some(cid), [], now(), None).unwrap();
    let (st, err) = code(&bench.app, "/quota", delivery(&local_record, Some(&good_body))).await;
    assert_eq!((st, err.as_str()), (StatusCode::FORBIDDEN, "signer"));

    // A lease triggers supervised task.start / task.end and daily limit refusal is observable.
    assert_eq!(lease(&bench, 1, "soniox").await.0, StatusCode::OK);
    let (st, body) = lease(&bench, 1, "soniox").await;
    assert_eq!((st, body["error"].as_str()), (StatusCode::PAYMENT_REQUIRED, Some("quota.daily")));
    bench.server.shutdown().await;

    let requests = seen.of("server.request");
    assert!(
        requests.iter().any(|l| l.text("route") == Some("quota") && l.u64("status") == Some(200)),
        "expected 200 quota request in logs: {requests:?}"
    );
    assert!(
        requests.iter().any(|l| l.text("route") == Some("quota") && l.u64("status") == Some(403)),
        "expected 403 quota refusal in logs: {requests:?}"
    );
    assert!(
        requests.iter().any(|l| l.text("route") == Some("lease") && l.u64("status") == Some(402)),
        "expected 402 lease refusal in logs: {requests:?}"
    );

    let starts = seen.of("task.start");
    let ends = seen.of("task.end");
    assert!(
        starts.iter().any(|l| {
            l.text("tree") == Some("server") && l.text("task") == Some("ledger.lease")
        }),
        "expected task.start for server/ledger.lease: {starts:?}"
    );
    assert!(
        ends.iter().any(|l| {
            l.text("tree") == Some("server")
                && l.text("task") == Some("ledger.lease")
                && l.text("code") == Some("done")
        }),
        "expected task.end for server/ledger.lease: {ends:?}"
    );

    // Privacy: no DID or secret key appears in any logged line.
    let dumped = format!("{:?}", seen.all());
    assert!(!dumped.contains(&did(1)), "DID leaked in logs");
    assert!(!dumped.contains(&local), "local did:key leaked in logs");
    assert!(!dumped.contains(SECRET_KEY), "secret key leaked in logs");
    assert_eq!(seen.refused(), 0);
}
