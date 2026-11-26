//! What the server's log keeps of a request: the route's code, the status, how long. Nothing
//! the request carried.

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::*;
use zu::Value;
use zutalk_log::{Level, capture};
use zutalk_server::now;

#[tokio::test]
async fn a_request_leaves_its_route_its_status_and_no_more() {
    let bench = bench();
    let seen = capture(Level::Debug);
    let answer = send(&bench.app, "GET", "/healthz", None, Body::empty()).await;
    assert_eq!(answer.status(), StatusCode::OK);
    // A request for a key of `did(1)`'s, signed with another's key.
    let body = map(&[("service", Value::Str("soniox".into())), ("aud", Value::Str(ORIGIN.into()))]);
    let forged = delivery(&signed(2, 1, "talk/lease", &body, now()), Some(&body));
    let refused = post(&bench.app, "/lease", forged).await.0;
    assert_eq!(refused, StatusCode::FORBIDDEN);
    // A room's id in the path is not in the log: its route's code is.
    let room = "r0123456789abcdef";
    send(&bench.app, "GET", &format!("/rooms/{room}/events?after=3"), None, Body::empty()).await;
    send(&bench.app, "GET", "/nowhere", None, Body::empty()).await;

    let requests = seen.of("server.request");
    let said: Vec<(&str, u64)> = requests
        .iter()
        .map(|line| (line.text("route").unwrap(), line.u64("status").unwrap()))
        .collect();
    assert_eq!(said[..2], [("healthz", 200), ("lease", u64::from(refused.as_u16()))]);
    assert_eq!(said[2].0, "rooms.events");
    assert_eq!(said[3], ("none", 404));
    assert_eq!(said.len(), 4);
    for line in &requests {
        assert_eq!(line.names(), ["route", "status", "ms"]);
        assert_eq!(line.level, Level::Info);
    }
    assert_eq!(seen.all().len(), 4, "the server says nothing else of a request");
    assert_eq!(seen.refused(), 0);
}

#[test]
fn a_sweep_is_noted_at_debug() {
    let bench = bench();
    let seen = capture(Level::Debug);
    let removed = bench.server.sweep(now());
    assert_eq!(removed, 0);
    let sweeps = seen.of("server.sweep");
    assert_eq!(sweeps.len(), 1, "expected one server.sweep event: {sweeps:?}");
    assert_eq!(sweeps[0].level, Level::Debug);
    assert_eq!(sweeps[0].names(), ["ms", "removed"]);
    assert_eq!(sweeps[0].u64("removed"), Some(0));
    assert_eq!(seen.refused(), 0);
}
