//! Grants: a coordinator's signed `talk/grant` record says a resident may spend so much until
//! a time. Only a coordinator's counts; the record must be theirs and its body the one signed.

mod common;

use axum::http::StatusCode;
use common::*;
use zu::Value;
use zutalk_server::now;

async fn deliver(bench: &Bench, record: &zu::Record, body: Option<&Value>) -> (StatusCode, String) {
    code(&bench.app, "/grants", delivery(record, body)).await
}

#[tokio::test]
async fn a_coordinators_grant_raises_the_balance() {
    let bench = bench();
    assert_eq!(left(&bench, 1), 0);
    give(&bench, 1, 250).await;
    assert_eq!(left(&bench, 1), 2_500_000);
    give(&bench, 1, 100).await;
    assert_eq!(left(&bench, 1), 3_500_000);
    // It is the resident's alone.
    assert_eq!(left(&bench, 2), 0);
}

#[tokio::test]
async fn a_grant_by_anyone_else_is_refused() {
    let bench = bench();
    let (record, body) = grant(3, "k", &did(1), 250, now() + DAY, &[]);
    let refused = (StatusCode::FORBIDDEN, "coordinator".to_owned());
    assert_eq!(deliver(&bench, &record, Some(&body)).await, refused);
    assert_eq!(left(&bench, 1), 0);
    // Signed in the coordinator's name with a key that is not the coordinator's.
    let (mut record, body) = grant(COORDINATOR, "k", &did(1), 250, now() + DAY, &[]);
    let (forged, _) = grant(3, "k", &did(1), 250, now() + DAY, &[]);
    (record.pubkey, record.sig) = (forged.pubkey, forged.sig);
    assert_eq!(deliver(&bench, &record, Some(&body)).await.0, StatusCode::BAD_REQUEST);
    let signed =
        zu::Record::sign(&secret(3), record.path.clone(), record.body, [], record.at, None);
    let refused = (StatusCode::FORBIDDEN, "signer".to_owned());
    assert_eq!(deliver(&bench, &signed.unwrap(), Some(&body)).await, refused);
    assert_eq!(left(&bench, 1), 0);
}

#[tokio::test]
async fn a_changed_body_is_refused() {
    let bench = bench();
    let (record, _) = grant(COORDINATOR, "k", &did(1), 250, now() + DAY, &[]);
    let (_, more) = grant(COORDINATOR, "k", &did(1), 250_000, now() + DAY, &[]);
    let refused = (StatusCode::BAD_REQUEST, "record".to_owned());
    assert_eq!(deliver(&bench, &record, Some(&more)).await, refused);
    assert_eq!(deliver(&bench, &record, None).await, refused);
    // A body of the wrong shape, though it is the one signed.
    let odd = map(&[("to", Value::Int(1))]);
    let record = signed(COORDINATOR, COORDINATOR, "talk/grant", &odd, now());
    let refused = (StatusCode::BAD_REQUEST, "body".to_owned());
    assert_eq!(deliver(&bench, &record, Some(&odd)).await, refused);
    // Another type of record is no grant.
    let (_, body) = grant(COORDINATOR, "k", &did(1), 250, now() + DAY, &[]);
    let record = signed(COORDINATOR, COORDINATOR, "talk/other", &body, now());
    let refused = (StatusCode::BAD_REQUEST, "record".to_owned());
    assert_eq!(deliver(&bench, &record, Some(&body)).await, refused);
    assert_eq!(left(&bench, 1), 0);
}

#[tokio::test]
async fn a_grant_delivered_again_changes_nothing() {
    let bench = bench();
    let (record, body) = grant(COORDINATOR, "k", &did(1), 250, now() + DAY, &[]);
    assert_eq!(deliver(&bench, &record, Some(&body)).await.0, StatusCode::OK);
    let written = ledger(&bench);
    assert_eq!(deliver(&bench, &record, Some(&body)).await.0, StatusCode::OK);
    assert_eq!(ledger(&bench), written);
    assert_eq!(left(&bench, 1), 2_500_000);
}

#[tokio::test]
async fn a_grant_is_replaced_or_removed_by_its_next_record() {
    let bench = bench();
    let (first, body) = grant(COORDINATOR, "k", &did(1), 250, now() + DAY, &[]);
    deliver(&bench, &first, Some(&body)).await;
    let (second, body) = grant(COORDINATOR, "k", &did(1), 100, now() + DAY, &[&first]);
    assert_eq!(deliver(&bench, &second, Some(&body)).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 1_000_000);
    // What was drawn on a grant stays drawn on what replaced it.
    assert_eq!(lease(&bench, 1, "soniox").await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 1_000_000 - 180_000);
    let path = second.path.clone();
    let gone =
        zu::Record::sign(&secret(COORDINATOR), path, None, [second.cid()], second.at + 1, None);
    assert_eq!(deliver(&bench, &gone.unwrap(), None).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 0);
    // The ledger is a fold: it reads the same after a restart.
    let bench = restart(bench);
    assert_eq!(left(&bench, 1), 0);
    assert_eq!(lease(&bench, 1, "soniox").await.1["error"], "quota");
}

#[tokio::test]
async fn a_grant_counts_until_its_end() {
    let bench = bench();
    let (record, body) = grant(COORDINATOR, "k", &did(1), 250, now() - 1, &[]);
    assert_eq!(deliver(&bench, &record, Some(&body)).await.0, StatusCode::OK);
    assert_eq!(left(&bench, 1), 0);
    assert_eq!(lease(&bench, 1, "soniox").await.1["error"], "quota");
    let soon = give_until(&bench, 1, 100, now() + 60_000).await;
    assert_eq!(left(&bench, 1), 1_000_000);
    assert_eq!(bench.server.left(&did(1), soon.at + 60_001), 0);
}
