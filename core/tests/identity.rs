//! P4-02 tests: AuthFlow state machine, handle normalization, daily quota parsing/reset,
//! two-step QR sign-in confirmation, identity creation with handle, rotation-key recovery,
//! and daily quota exhaustion observability.

mod hub;

use hub::{App, NOWHERE, STORE, config_on, store};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use zu::{Cid, Codec, Doc, Path as ZuPath, Record, SecretKey, Value, did, login};
use zutalk_core::app::hub::{Config, Core, Outlet};
use zutalk_core::app::proto::{
    AuthFlow, Change, Command, NoteOp, Query, QueryValue, RecordingView, Session, State,
};
use zutalk_core::id::identity::{
    self, AuthEvent, DEFAULT_HANDLE_DOMAIN, auth_step, did_doc_url, normalize_handle,
    read_cached_handle, write_cached_handle,
};
use zutalk_core::id::keys::{self, Keys, Ours, Service};
use zutalk_core::id::quota::{self, Quota, ServiceQuota, is_exhausted, remaining_for};
use zutalk_core::knowledge::glossary::Term;
use zutalk_core::rt::supervise::End;
use zutalk_server::{
    BoxFuture, Config as ServerConfig, Directory, Minted, Refusal, Server, Upstream,
};

#[test]
fn auth_flow_transition_table() {
    let idle = AuthFlow::Idle;
    let scanning = auth_step(
        &idle,
        AuthEvent::BeginScan {
            url: "https://s.example/.well-known/zu-relay/00".into(),
            expires_at: 1000,
        },
    );
    assert_eq!(scanning.code(), "Scanning");

    // Re-beginning scan from Scanning replaces the code.
    let scanning2 = auth_step(
        &scanning,
        AuthEvent::BeginScan {
            url: "https://s.example/.well-known/zu-relay/11".into(),
            expires_at: 2000,
        },
    );
    assert_eq!(
        scanning2,
        AuthFlow::Scanning {
            url: "https://s.example/.well-known/zu-relay/11".into(),
            expires_at: 2000,
        }
    );

    // Receiving a key moves Scanning -> Confirm (holding reserved `code: None`).
    let confirm = auth_step(
        &scanning2,
        AuthEvent::Received {
            did: "did:zu:ewvi7nxzyoun6zhxrhs64oiz".into(),
            handle: Some("demo.zuzalu.ink".into()),
            code: None,
        },
    );
    assert_eq!(confirm.code(), "Confirm");
    assert_eq!(
        confirm,
        AuthFlow::Confirm {
            did: "did:zu:ewvi7nxzyoun6zhxrhs64oiz".into(),
            handle: Some("demo.zuzalu.ink".into()),
            code: None,
        }
    );

    // Received in Idle is ignored.
    assert_eq!(
        auth_step(&idle, AuthEvent::Received { did: "did:zu:x".into(), handle: None, code: None }),
        AuthFlow::Idle
    );

    // Confirm -> Completed / Cancelled / Failed all return to Idle.
    assert_eq!(auth_step(&confirm, AuthEvent::Completed), AuthFlow::Idle);
    assert_eq!(auth_step(&confirm, AuthEvent::Cancelled), AuthFlow::Idle);
    assert_eq!(auth_step(&confirm, AuthEvent::Failed), AuthFlow::Idle);

    // Creating and Recovering ignore BeginScan / Received and settle to Idle on terminal events.
    let creating = auth_step(&idle, AuthEvent::BeginCreate);
    assert_eq!(creating, AuthFlow::Creating);
    assert_eq!(
        auth_step(&creating, AuthEvent::BeginScan { url: "x".into(), expires_at: 1 }),
        AuthFlow::Creating
    );
    assert_eq!(auth_step(&creating, AuthEvent::Completed), AuthFlow::Idle);
    assert_eq!(auth_step(&creating, AuthEvent::Failed), AuthFlow::Idle);

    let recovering = auth_step(&idle, AuthEvent::BeginRecover);
    assert_eq!(recovering, AuthFlow::Recovering);
    assert_eq!(auth_step(&recovering, AuthEvent::BeginCreate), AuthFlow::Recovering);
    assert_eq!(auth_step(&recovering, AuthEvent::Cancelled), AuthFlow::Idle);
}

#[test]
fn handle_normalization_table() {
    assert_eq!(normalize_handle(""), Ok(None));
    assert_eq!(normalize_handle("   "), Ok(None));
    assert_eq!(normalize_handle("lin"), Ok(Some(format!("lin.{DEFAULT_HANDLE_DOMAIN}"))));
    assert_eq!(normalize_handle(" @Lin-01 "), Ok(Some(format!("lin-01.{DEFAULT_HANDLE_DOMAIN}"))));
    assert_eq!(normalize_handle("Demo.Zuzalu.INK"), Ok(Some("demo.zuzalu.ink".into())));
    assert_eq!(normalize_handle("@alpha.example.org"), Ok(Some("alpha.example.org".into())));

    let max_short = "a".repeat(identity::HANDLE_MAX_BYTES);
    assert_eq!(
        normalize_handle(&max_short),
        Ok(Some(format!("{max_short}.{DEFAULT_HANDLE_DOMAIN}")))
    );

    let too_long = "a".repeat(identity::HANDLE_MAX_BYTES + 1);
    for bad in ["a", "-ab", "ab-", "bad name", "bad_name", "a..b", "@", "a.0", &too_long] {
        assert_eq!(normalize_handle(bad), Err("id.handle"), "expected id.handle for {bad:?}");
    }
}

#[test]
fn quota_parsing_and_midnight_reset_rules() {
    let raw = json!({
        "left": 500_000,
        "resets_at": 1_764_000_000_000i64,
        "services": {
            "soniox": { "used": 40, "limit": 40, "remaining": 0 },
            "openrouter": { "used": 5, "limit": 30, "remaining": 25 }
        }
    });
    let (left, parsed) = quota::parse(&raw).expect("valid quota payload");
    assert_eq!(left, 500_000);
    assert_eq!(
        parsed,
        Quota {
            resets_at: 1_764_000_000_000,
            services: vec![
                ServiceQuota { service: "soniox".into(), used: 40, limit: 40, remaining: 0 },
                ServiceQuota { service: "openrouter".into(), used: 5, limit: 30, remaining: 25 },
            ],
        }
    );

    let before_midnight = parsed.resets_at - 1;
    assert_eq!(remaining_for(&parsed, Service::Soniox, before_midnight), Some(0));
    assert_eq!(remaining_for(&parsed, Service::OpenRouter, before_midnight), Some(25));
    assert!(is_exhausted(&parsed, Service::Soniox, before_midnight));
    assert!(!is_exhausted(&parsed, Service::OpenRouter, before_midnight));

    // At or after UTC midnight reset, full daily limit is available and nothing is exhausted.
    let at_midnight = parsed.resets_at;
    assert_eq!(remaining_for(&parsed, Service::Soniox, at_midnight), Some(40));
    assert_eq!(remaining_for(&parsed, Service::OpenRouter, at_midnight), Some(30));
    assert!(!is_exhausted(&parsed, Service::Soniox, at_midnight));
    assert!(!is_exhausted(&parsed, Service::OpenRouter, at_midnight));

    assert!(quota::parse(&json!({ "left": 10 })).is_none());
    assert!(quota::parse_quota(&json!({ "resets_at": 10, "services": {} })).is_none());
}

#[test]
fn cached_handle_and_did_doc_url_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(read_cached_handle(dir.path()), None);
    write_cached_handle(dir.path(), Some("demo.zuzalu.ink")).expect("write handle");
    assert_eq!(read_cached_handle(dir.path()), Some("demo.zuzalu.ink".into()));
    write_cached_handle(dir.path(), None).expect("clear handle");
    assert_eq!(read_cached_handle(dir.path()), None);

    let did_zu = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
    assert_eq!(
        did_doc_url(did_zu, "http://127.0.0.1:8080/").unwrap(),
        format!("http://127.0.0.1:8080/{did_zu}/log/audit")
    );
    assert_eq!(
        did_doc_url("did:web:user.example.org", "http://127.0.0.1:8080").unwrap(),
        "https://user.example.org/.well-known/did.json"
    );
    assert_eq!(
        did_doc_url("did:key:z6MkInvalid", "http://127.0.0.1:8080"),
        Err(End::Fatal("id.did"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn create_and_recover_hub_guards_without_network() {
    let _store = STORE.lock().await;
    store(false);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    // ConfirmSignIn without a pending sign-in returns id.no_pending.
    app.send(Command::ConfirmSignIn);
    app.notice("id.no_pending").await;

    // Invalid handle is rejected synchronously on the hub with id.handle.
    app.send(Command::CreateIdentity { handle: Some("x".into()) });
    app.notice("id.handle").await;
    assert_eq!(app.until(|_| true).await.auth, AuthFlow::Idle);

    // RefreshQuota while signed out returns sign_in.first.
    app.send(Command::RefreshQuota);
    app.notice("sign_in.first").await;

    // When already signed in, CreateIdentity and RecoverIdentity are refused with id.signed_in.
    drop(app);
    store(true);
    let mut app = App::start(dir.path());
    app.until(|s| s.me.is_some()).await;
    app.send(Command::CreateIdentity { handle: None });
    app.notice("id.signed_in").await;
    app.send(Command::RecoverIdentity {
        did: "did:zu:ewvi7nxzyoun6zhxrhs64oiz".into(),
        key: "z".into(),
    });
    app.notice("id.signed_in").await;
}

/// Stand-in directory storing posted PLC operations per DID and serving their audit logs.
#[derive(Default)]
struct MemoryPlc {
    logs: Mutex<BTreeMap<String, Vec<Json>>>,
}

impl MemoryPlc {
    fn push_op(&self, did_str: &str, op: Json) -> Vec<u8> {
        let mut logs = self.logs.lock().unwrap();
        let entries = logs.entry(did_str.to_owned()).or_default();
        let idx = entries.len();
        let cid = Cid::of(Codec::Drisl, &Value::from_json(&op.to_string()).unwrap().encode());
        let created_at = format!("2026-11-16T00:{idx:02}:00.000Z");
        entries.push(json!({
            "did": did_str,
            "operation": op,
            "cid": cid.to_string(),
            "nullified": false,
            "createdAt": created_at,
        }));
        serde_json::to_vec(entries).unwrap()
    }

    fn audit_bytes(&self, did_str: &str) -> Option<Vec<u8>> {
        let logs = self.logs.lock().unwrap();
        logs.get(did_str).map(|entries| serde_json::to_vec(entries).unwrap())
    }
}

impl Directory for MemoryPlc {
    fn doc<'a>(&'a self, did_str: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>> {
        Box::pin(async move {
            let unknown = Refusal(reqwest::StatusCode::FORBIDDEN, "signer");
            let bytes = self.audit_bytes(did_str).ok_or(unknown)?;
            did::resolve(did_str, &bytes).map_err(|_| unknown)
        })
    }
}

struct PlcResponder(Arc<MemoryPlc>);

impl Respond for PlcResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let p = request.url.path();
        if request.method == "POST" {
            let did_str = p.trim_start_matches('/');
            let Ok(op) = serde_json::from_slice::<Json>(&request.body) else {
                return ResponseTemplate::new(400);
            };
            self.0.push_op(did_str, op);
            return ResponseTemplate::new(200);
        }
        if let Some(did_str) = p.strip_prefix('/').and_then(|rest| rest.strip_suffix("/log/audit"))
        {
            if let Some(bytes) = self.0.audit_bytes(did_str) {
                return ResponseTemplate::new(200).set_body_bytes(bytes);
            }
            return ResponseTemplate::new(404);
        }
        ResponseTemplate::new(404)
    }
}

struct FakeUpstream {
    slice: u64,
}

impl Upstream for FakeUpstream {
    fn slice(&self) -> u64 {
        self.slice
    }
    fn mint<'a>(&'a self, name: &'a str, _: u64) -> BoxFuture<'a, Result<Minted, Refusal>> {
        Box::pin(async move {
            Ok(Minted { key: format!("sk-minted-{name}"), session_cap_s: Some(300) })
        })
    }
}

async fn start_local_server(
    plc: Arc<dyn Directory>,
    daily: [u32; 2],
    coordinator: Option<String>,
) -> (String, Arc<Server>, tempfile::TempDir) {
    _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut cfg = ServerConfig::new(&origin, dir.path(), coordinator.into_iter().collect());
    cfg.daily = daily;
    let server = Server::open(
        cfg,
        plc,
        [Arc::new(FakeUpstream { slice: 100_000 }), Arc::new(FakeUpstream { slice: 100_000 })],
    )
    .unwrap();
    let running = server.clone();
    tokio::spawn(async move {
        let _ = zutalk_server::serve(listener, running).await;
    });
    (origin, server, dir)
}

async fn post_grant(
    http: &reqwest::Client,
    origin: &str,
    coord_did: &str,
    coord_key: &SecretKey,
    to_did: &str,
    cents: i64,
) {
    let body = Value::Map(BTreeMap::from([
        ("to".into(), Value::Str(to_did.into())),
        ("cents".into(), Value::Int(cents)),
        ("until".into(), Value::Int(zutalk_server::now() + zutalk_server::DAY_MS)),
    ]));
    let cid = Cid::of(Codec::Drisl, &body.encode());
    let path = ZuPath::new(coord_did, "talk/grant", "g1").unwrap();
    let record = Record::sign(coord_key, path, Some(cid), [], zutalk_server::now(), None).unwrap();
    let header: Json = serde_json::from_str(&record.header().to_json().unwrap()).unwrap();
    let body_json: Json = serde_json::from_str(&body.to_json().unwrap()).unwrap();
    let res = http
        .post(format!("{origin}/grants"))
        .json(&json!({ "record": header, "body": body_json }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn qr_sign_in_waits_for_confirmation_before_keeping_identity() {
    let _store = STORE.lock().await;
    store(false);
    let plc = Arc::new(MemoryPlc::default());
    let directory = MockServer::start().await;
    Mock::given(method("GET")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;

    // Register a resident DID with handle and a coordinator DID in our stand-in PLC.
    let resident_seed = [21u8; 32];
    let resident_key = SecretKey::from_seed(&resident_seed);
    let resident_rot = did::Rotation::from_bytes(&[22u8; 32]).unwrap();
    let (resident_did, resident_op) = did::create_with_aka(
        &resident_rot,
        &resident_key.public(),
        &["zu://demo.zuzalu.ink".into()],
    )
    .unwrap();
    plc.push_op(&resident_did, serde_json::from_str(&resident_op).unwrap());

    let coord_key = SecretKey::from_seed(&[23u8; 32]);
    let coord_rot = did::Rotation::from_bytes(&[24u8; 32]).unwrap();
    let (coord_did, coord_op) = did::create(&coord_rot, &coord_key.public()).unwrap();
    plc.push_op(&coord_did, serde_json::from_str(&coord_op).unwrap());

    let (server_url, _server, _srv_dir) =
        start_local_server(plc.clone(), [40, 30], Some(coord_did.clone())).await;
    let http = reqwest::Client::new();
    post_grant(&http, &server_url, &coord_did, &coord_key, &resident_did, 500).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut app =
        App::with(Config { directory: directory.uri(), ..config_on(dir.path(), &server_url) });
    app.until(|_| true).await;

    // Attempt 1: scan QR -> relay seals key -> enters Confirm WITHOUT writing to key store -> CancelSignIn.
    app.send(Command::SignIn);
    let scanning = app.until(|s| matches!(s.auth, AuthFlow::Scanning { .. })).await;
    let AuthFlow::Scanning { url, .. } = scanning.auth else { panic!("expected Scanning") };
    let scanned = login::scan(&url).expect("valid login code");
    let sealed = scanned
        .seal(&login::key(&resident_did, &resident_seed), &[31u8; 32], &[32u8; 12])
        .expect("seal");
    let relay_post = scanned.room.replacen("https://", "http://", 1);
    let resp = http.post(relay_post).body(sealed).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    let confirm = app.until(|s| matches!(s.auth, AuthFlow::Confirm { .. })).await;
    assert_eq!(
        confirm.auth,
        AuthFlow::Confirm {
            did: resident_did.clone(),
            handle: Some("demo.zuzalu.ink".into()),
            code: None,
        }
    );
    assert!(keys::kept().unwrap().is_none(), "identity must NOT be kept before ConfirmSignIn");

    app.send(Command::CancelSignIn);
    app.until(|s| s.auth == AuthFlow::Idle).await;
    assert!(keys::kept().unwrap().is_none(), "cancelled sign-in must leave store empty");

    // Attempt 2: scan QR again -> ConfirmSignIn persists identity, resolves handle, fetches quota.
    app.send(Command::SignIn);
    let scanning = app.until(|s| matches!(s.auth, AuthFlow::Scanning { .. })).await;
    let AuthFlow::Scanning { url, .. } = scanning.auth else { panic!("expected Scanning") };
    let scanned = login::scan(&url).expect("valid login code");
    let sealed = scanned
        .seal(&login::key(&resident_did, &resident_seed), &[33u8; 32], &[34u8; 12])
        .expect("seal");
    let relay_post = scanned.room.replacen("https://", "http://", 1);
    assert_eq!(
        http.post(relay_post).body(sealed).send().await.unwrap().status(),
        reqwest::StatusCode::NO_CONTENT
    );

    app.until(|s| matches!(s.auth, AuthFlow::Confirm { .. })).await;
    app.send(Command::ConfirmSignIn);
    let signed =
        app.until(|s| s.me.as_deref() == Some(resident_did.as_str()) && s.quota.is_some()).await;
    assert_eq!(signed.auth, AuthFlow::Idle);
    assert_eq!(signed.handle.as_deref(), Some("demo.zuzalu.ink"));
    assert_eq!(signed.left, Some(5_000_000));
    assert!(keys::kept().unwrap().is_some());

    let auth_events = app.seen.of("identity.auth");
    assert!(auth_events.iter().any(|l| l.text("to") == Some("Scanning")));
    assert!(auth_events.iter().any(|l| l.text("to") == Some("Confirm")));
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_identity_with_handle_holds_unsaved_rotation_key() {
    let _store = STORE.lock().await;
    store(false);
    let plc = Arc::new(MemoryPlc::default());
    let directory = MockServer::start().await;
    Mock::given(method("POST")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;
    Mock::given(method("GET")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;

    let copied = Arc::new(Mutex::new(String::new()));
    let copied_cb = copied.clone();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = Config { directory: directory.uri(), ..config_on(dir.path(), NOWHERE) };
    cfg.clip = Arc::new(move |text| {
        *copied_cb.lock().unwrap() = text.to_owned();
        true
    });

    let mut app = App::with(cfg);
    app.until(|_| true).await;

    // Invalid handle fails immediately with id.handle.
    app.send(Command::CreateIdentity { handle: Some("-bad".into()) });
    app.notice("id.handle").await;

    // Valid short handle creates did:zu with lin.zuzalu.ink and unsaved rotation reminder.
    app.send(Command::CreateIdentity { handle: Some("lin".into()) });
    let state = app.until(|s| s.me.is_some()).await;
    let created_did = state.me.clone().expect("created did");
    assert!(created_did.starts_with("did:zu:"));
    assert_eq!(state.handle.as_deref(), Some("lin.zuzalu.ink"));
    assert!(state.rotation_unsaved);
    assert_eq!(state.auth, AuthFlow::Idle);

    // CopyRotation copies valid multikey to clipboard without putting it in State or logs.
    app.send(Command::CopyRotation);
    app.notice("copied").await;
    let rot_key = copied.lock().unwrap().clone();
    assert!(rot_key.starts_with('z'));
    assert!(did::Rotation::from_multikey(&rot_key).is_ok());

    let state_json = serde_json::to_string(&state).unwrap();
    assert!(!state_json.contains(&rot_key), "rotation key must never appear in State");
    let dumped_logs = format!("{:?}", app.seen.all());
    assert!(!dumped_logs.contains(&rot_key), "rotation key leaked into logs");
    assert!(!dumped_logs.contains(&created_did), "DID leaked into logs");
    assert!(!dumped_logs.contains("lin.zuzalu.ink"), "handle leaked into logs");
    assert_eq!(app.seen.refused(), 0);

    app.send(Command::RotationSaved);
    app.until(|s| !s.rotation_unsaved).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_identity_adds_new_signing_key_and_rejects_wrong_key() {
    let _store = STORE.lock().await;
    store(false);
    let plc = Arc::new(MemoryPlc::default());
    let directory = MockServer::start().await;
    Mock::given(method("POST")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;
    Mock::given(method("GET")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;

    // Seed an existing identity with handle in our stand-in PLC.
    let orig_key = SecretKey::from_seed(&[41u8; 32]);
    let rot = did::Rotation::from_bytes(&[42u8; 32]).unwrap();
    let wrong_rot = did::Rotation::from_bytes(&[43u8; 32]).unwrap();
    let (target_did, gen_op) =
        did::create_with_aka(&rot, &orig_key.public(), &["zu://river.zuzalu.ink".into()]).unwrap();
    plc.push_op(&target_did, serde_json::from_str(&gen_op).unwrap());

    let dir = tempfile::tempdir().expect("tempdir");
    let mut app =
        App::with(Config { directory: directory.uri(), ..config_on(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    // Wrong rotation key is rejected with id.recovery_key and never leaks to logs.
    let bad_key = wrong_rot.multikey().to_string();
    app.send(Command::RecoverIdentity { did: target_did.clone(), key: bad_key.clone() });
    app.notice("id.recovery_key").await;
    app.until(|s| s.auth == AuthFlow::Idle).await;
    assert!(keys::kept().unwrap().is_none());

    // Valid rotation key appends a fresh device signing key and signs in.
    let good_key = rot.multikey().to_string();
    app.send(Command::RecoverIdentity { did: target_did.clone(), key: good_key.clone() });
    let signed = app.until(|s| s.me.as_deref() == Some(target_did.as_str())).await;
    assert_eq!(signed.handle.as_deref(), Some("river.zuzalu.ink"));
    assert!(!signed.rotation_unsaved);
    assert_eq!(signed.auth, AuthFlow::Idle);

    // The directory now resolves with both the original key and the newly persisted device key.
    let audit_bytes = plc.audit_bytes(&target_did).expect("audit log");
    let doc = did::resolve(&target_did, &audit_bytes).expect("valid audit log");
    let kept = keys::kept().unwrap().expect("kept identity");
    assert!(doc.key_now(&orig_key.public()));
    assert!(doc.key_now(&kept.key.public()));

    let dumped_logs = format!("{:?}", app.seen.all());
    assert!(!dumped_logs.contains(&bad_key), "wrong recovery key leaked in logs");
    assert!(!dumped_logs.contains(&good_key), "recovery key leaked in logs");
    assert!(!dumped_logs.contains(&target_did), "DID leaked in logs");
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_quota_and_daily_exhaustion_are_visible() {
    let _store = STORE.lock().await;
    store(false);
    let plc = Arc::new(MemoryPlc::default());
    let directory = MockServer::start().await;
    Mock::given(method("GET")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;

    let user_seed = [51u8; 32];
    let user_key = SecretKey::from_seed(&user_seed);
    let user_rot = did::Rotation::from_bytes(&[52u8; 32]).unwrap();
    let (user_did, user_op) = did::create(&user_rot, &user_key.public()).unwrap();
    plc.push_op(&user_did, serde_json::from_str(&user_op).unwrap());

    let coord_key = SecretKey::from_seed(&[53u8; 32]);
    let coord_rot = did::Rotation::from_bytes(&[54u8; 32]).unwrap();
    let (coord_did, coord_op) = did::create(&coord_rot, &coord_key.public()).unwrap();
    plc.push_op(&coord_did, serde_json::from_str(&coord_op).unwrap());

    let (server_url, _server, _srv_dir) =
        start_local_server(plc.clone(), [1, 1], Some(coord_did.clone())).await;
    let http = reqwest::Client::new();
    post_grant(&http, &server_url, &coord_did, &coord_key, &user_did, 500).await;

    // Keep user_did signed in in the mock key store.
    keys::set("identity", Some(&[&user_seed[..], user_did.as_bytes()].concat())).unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app =
        App::with(Config { directory: directory.uri(), ..config_on(dir.path(), &server_url) });
    let booted = app.until(|s| s.quota.is_some()).await;
    let q = booted.quota.expect("boot quota");
    assert_eq!(
        q.services[0],
        ServiceQuota { service: "soniox".into(), used: 0, limit: 1, remaining: 1 }
    );

    // Lease once directly via Chain, then hit daily cap -> keys.quota.daily.
    let ours =
        Ours { server: server_url.clone(), me: keys::kept().unwrap().unwrap(), http: http.clone() };
    let chain = keys::Chain::new(Some(ours));
    assert!(chain.lease(Service::Soniox).await.is_ok());
    assert_eq!(chain.lease(Service::Soniox).await.err(), Some(End::Fatal("keys.quota.daily")));

    // Explicit Command::RefreshQuota refreshes State.quota and emits quota.updated + notice.
    app.send(Command::RefreshQuota);
    app.notice("quota.refreshed").await;
    let refreshed = app
        .until(|s| {
            s.quota
                .as_ref()
                .is_some_and(|q| q.services.iter().any(|x| x.service == "soniox" && x.used == 1))
        })
        .await;
    let q2 = refreshed.quota.expect("refreshed quota");
    assert_eq!(
        q2.services[0],
        ServiceQuota { service: "soniox".into(), used: 1, limit: 1, remaining: 0 }
    );

    let q_events = app.seen.of("quota.updated");
    assert!(
        q_events.iter().any(|l| l.u64("soniox_left") == Some(0) && l.text("code") == Some("ok"))
    );
    assert_eq!(app.seen.refused(), 0);
}

/// Core wrapper that inherits the caller's active `zutalk_log::files` scope instead of installing
/// an in-memory `capture()`, so events reach real `<data>/logs/zutalk.jsonl` on disk.
struct FileApp {
    core: Core,
    outlet: Outlet,
    said_at: u64,
}

impl FileApp {
    fn start(scope: &zutalk_log::Scope, cfg: Config) -> Self {
        let (core, outlet) = {
            let _entered = scope.enter();
            Core::start(cfg).expect("core starts")
        };
        Self { core, outlet, said_at: 0 }
    }

    fn send(&self, scope: &zutalk_log::Scope, command: Command) {
        let _entered = scope.enter();
        self.core.send(command).expect("hub accepts command");
    }

    async fn until(&mut self, holds: impl Fn(&State) -> bool) -> State {
        let wait = async {
            let (seq, state) = self.outlet.state.now();
            if seq > 0 && holds(&state) {
                return (*state).clone();
            }
            while let Some((_, state)) = self.outlet.state.next().await {
                if holds(&state) {
                    return (*state).clone();
                }
            }
            panic!("the core stopped");
        };
        timeout(Duration::from_secs(20), wait).await.expect("state came in time")
    }

    async fn session(&mut self, holds: impl Fn(Option<&Session>) -> bool) -> Option<Session> {
        let wait = async {
            loop {
                let (_, current) = self.outlet.session.now();
                if holds((*current).as_ref()) {
                    return (*current).clone();
                }
                self.outlet.session.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("session came in time")
    }

    async fn notice(&mut self, code: &str) {
        let wait = async {
            loop {
                let (_, notices) = self.outlet.said.now();
                let first = notices.next.saturating_sub(notices.codes.len() as u64);
                if self.said_at < first {
                    self.said_at = first;
                }
                while self.said_at < notices.next {
                    let idx = (self.said_at - first) as usize;
                    self.said_at += 1;
                    if notices.codes[idx] == code {
                        return;
                    }
                }
                self.outlet.said.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("notice came in time")
    }

    async fn wait_recording(
        &self,
        id: &str,
        holds: impl Fn(Option<&RecordingView>) -> bool,
    ) -> Option<RecordingView> {
        let mut inv = self.outlet.invalidated.clone();
        let wait = async {
            loop {
                if let Ok(reply) = self.core.query(Query::Recording { id: id.into() }).await
                    && let QueryValue::Recording(view) = reply.value
                {
                    let view = view.map(|b| *b);
                    if holds(view.as_ref()) {
                        return view;
                    }
                }
                let _ = timeout(Duration::from_millis(50), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("recording condition came in time")
    }

    async fn wait_glossary(&self, holds: impl Fn(&[Term]) -> bool) -> Vec<Term> {
        let mut inv = self.outlet.invalidated.clone();
        let wait = async {
            loop {
                if let Ok(reply) = self.core.query(Query::Glossary).await
                    && let QueryValue::Glossary(terms) = reply.value
                    && holds(&terms)
                {
                    return terms;
                }
                let _ = timeout(Duration::from_millis(50), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("glossary condition came in time")
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_secrets_dids_and_handles_never_reach_logs_or_diagnose() {
    let _store = STORE.lock().await;
    store(false);
    keys::set("identity.local", Some(&hub::LOCAL_SEED)).expect("local identity seed");
    let local_did = hub::local_did();
    let refused_before = zutalk_log::refused();

    // Set up stand-in PLC directory and local server before opening the file log scope.
    let plc = Arc::new(MemoryPlc::default());
    let directory = MockServer::start().await;
    Mock::given(method("POST")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;
    Mock::given(method("GET")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;

    let coord_key = SecretKey::from_seed(&[61u8; 32]);
    let coord_rot = did::Rotation::from_bytes(&[62u8; 32]).unwrap();
    let (coord_did, coord_op) = did::create(&coord_rot, &coord_key.public()).unwrap();
    plc.push_op(&coord_did, serde_json::from_str(&coord_op).unwrap());

    let (server_url, _server, _srv_dir) =
        start_local_server(plc.clone(), [40, 30], Some(coord_did.clone())).await;
    let http = reqwest::Client::new();

    let data_dir = tempfile::tempdir().expect("data tempdir");
    let data_path = data_dir.path().to_string_lossy().into_owned();
    let log_dir = data_dir.path().join("logs");
    let (scope, guard) = zutalk_log::files(
        &log_dir,
        "zutalk",
        zutalk_log::Level::Debug,
        zutalk_log::Limits::default(),
    )
    .expect("open file log");

    let copied = Arc::new(Mutex::new(String::new()));
    let copied_cb = copied.clone();
    let mut cfg = Config { directory: directory.uri(), ..config_on(data_dir.path(), &server_url) };
    cfg.clip = Arc::new(move |text| {
        *copied_cb.lock().unwrap() = text.to_owned();
        true
    });

    let mut app = FileApp::start(&scope, cfg);
    app.until(|_| true).await;

    // 1. Offline local recording with sentinel title, note, and glossary term.
    let title_sentinel = "AdoptTitleSentinel44";
    let note_sentinel = "AdoptNoteSentinel55";
    let term_sentinel = "AdoptTermSentinel66";
    let handle_sentinel = "handleprobe44";
    let wrong_recovery_sentinel = "zWrongRecoveryProbe99";

    app.send(&scope, Command::Start);
    let rec_id = app.session(|s| s.is_some()).await.expect("live session").id;
    let tape_dir = data_dir.path().join("tape").join(&rec_id);
    for _ in 0..100 {
        if zutalk_core::capture::tape::kept(&tape_dir) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    app.send(&scope, Command::Stop);
    app.session(|s| s.is_none()).await;
    app.wait_recording(&rec_id, |v| v.is_some_and(|r| r.row.length_ms > 0)).await;

    app.send(
        &scope,
        Command::Change {
            id: rec_id.clone(),
            change: Change::Title { text: title_sentinel.into() },
        },
    );
    app.send(
        &scope,
        Command::Note {
            id: rec_id.clone(),
            op: NoteOp::Add { at_ms: 100, anchor: None, text: note_sentinel.into() },
        },
    );
    app.send(&scope, Command::Term { said: term_sentinel.into(), written: Some(BTreeMap::new()) });
    app.wait_recording(&rec_id, |v| {
        v.is_some_and(|r| {
            r.row.title == title_sentinel && r.notes.iter().any(|n| n.text == note_sentinel)
        })
    })
    .await;
    app.wait_glossary(|terms| terms.iter().any(|t| t.said == term_sentinel)).await;

    // 2. CreateIdentity with handle -> triggers first-login adoption of local recording & glossary.
    app.send(&scope, Command::CreateIdentity { handle: Some(handle_sentinel.into()) });
    let created = app
        .until(|s| s.me.is_some() && s.local_shelf.adopted && s.local_shelf.recordings >= 1)
        .await;
    let person_did = created.me.clone().expect("created did:zu");
    assert!(person_did.starts_with("did:zu:"));
    assert_eq!(created.handle.as_deref(), Some("handleprobe44.zuzalu.ink"));
    assert!(created.rotation_unsaved);

    // 3. CopyRotation & RotationSaved -> capture real rotation multikey sentinel.
    app.send(&scope, Command::CopyRotation);
    app.notice("copied").await;
    let rot_key = copied.lock().unwrap().clone();
    assert!(rot_key.starts_with('z'));
    assert!(did::Rotation::from_multikey(&rot_key).is_ok());

    app.send(&scope, Command::RotationSaved);
    app.until(|s| !s.rotation_unsaved).await;

    // 4. Grant quota on local server & RefreshQuota.
    post_grant(&http, &server_url, &coord_did, &coord_key, &person_did, 500).await;
    app.send(&scope, Command::RefreshQuota);
    app.notice("quota.refreshed").await;
    app.until(|s| {
        s.quota
            .as_ref()
            .is_some_and(|q| q.services.iter().any(|x| x.service == "soniox" && x.limit == 40))
    })
    .await;

    // 5. SignOut -> RecoverIdentity with wrong key, then with real rotation key.
    app.send(&scope, Command::SignOut);
    app.until(|s| s.me.is_none()).await;

    app.send(
        &scope,
        Command::RecoverIdentity { did: person_did.clone(), key: wrong_recovery_sentinel.into() },
    );
    app.notice("id.recovery_key").await;
    app.until(|s| s.auth == AuthFlow::Idle).await;

    app.send(&scope, Command::RecoverIdentity { did: person_did.clone(), key: rot_key.clone() });
    let recovered = app.until(|s| s.me.as_deref() == Some(person_did.as_str())).await;
    assert_eq!(recovered.handle.as_deref(), Some("handleprobe44.zuzalu.ink"));

    // 6. DiscardLocal removes local backup cleanly.
    app.send(&scope, Command::DiscardLocal);
    app.notice("local.discarded").await;
    let after_discard = app.until(|s| s.local_shelf.recordings == 0).await;
    let state_json = serde_json::to_string(&after_discard).expect("serialize final state");

    // 7. Drain file log briefly, then generate Diagnose bundle.
    std::thread::sleep(Duration::from_millis(50));
    app.send(&scope, Command::Diagnose);
    app.notice("diagnose.done").await;

    // Shut down core and flush file log writer completely.
    drop(app);
    drop(guard);

    // State must never carry rotation or recovery secret material.
    assert!(!state_json.contains(&rot_key), "rotation key appeared in State JSON");
    assert!(
        !state_json.contains(wrong_recovery_sentinel),
        "wrong recovery key sentinel appeared in State JSON"
    );

    // Collect every line from <data>/logs/*.jsonl.
    let mut log_lines = Vec::new();
    for entry in fs::read_dir(&log_dir).expect("read logs dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let content = fs::read_to_string(&path).expect("read jsonl");
            for line in content.lines().filter(|l| !l.trim().is_empty()) {
                log_lines.push(line.to_owned());
            }
        }
    }
    assert!(!log_lines.is_empty(), "log files must not be empty");

    // Verify diagnose bundle structure, copied logs, and Tally <= 8192 B.
    let diagnose_root = data_dir.path().join("diagnose");
    let mut bundle_dirs: Vec<PathBuf> = fs::read_dir(&diagnose_root)
        .expect("read diagnose dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    bundle_dirs.sort();
    assert_eq!(bundle_dirs.len(), 1, "expected exactly one diagnose bundle: {bundle_dirs:?}");
    let bundle = &bundle_dirs[0];
    assert!(bundle.join("logs").is_dir(), "expected logs/ copied inside diagnose bundle");
    let tally_raw = fs::read_to_string(bundle.join("stats.json")).expect("read stats.json");
    let tally: Json = serde_json::from_str(&tally_raw).expect("parse Tally JSON from stats.json");
    assert!(tally["counts"].is_array(), "stats.json must contain counts array");
    assert!(tally["peaks"].is_array(), "stats.json must contain peaks array");
    assert!(tally["spreads"].is_array(), "stats.json must contain spreads array");
    assert!(tally["queues"].is_array(), "stats.json must contain queues array");
    let tally_compact_bytes = serde_json::to_vec(&tally).expect("serialize Tally").len();
    assert!(
        tally_compact_bytes <= 8192,
        "Tally serialized size must stay <= 8192 B, got {tally_compact_bytes} B"
    );

    // Forbidden sensitive strings that must have ZERO occurrences in logs or diagnose bundle.
    let forbidden: Vec<String> = vec![
        rot_key,
        wrong_recovery_sentinel.into(),
        person_did,
        local_did,
        "did:zu:".into(),
        "did:key:".into(),
        handle_sentinel.into(),
        title_sentinel.into(),
        note_sentinel.into(),
        term_sentinel.into(),
        "sk-minted-soniox".into(),
        data_path,
    ];

    // Check every log line: valid flat JSON, zero refused "!" values, zero sensitive hits.
    for (idx, line) in log_lines.iter().enumerate() {
        let parsed: Json = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("log line {idx} invalid json: {e}: {line}"));
        let obj = parsed.as_object().expect("log line must be JSON object");
        for (k, v) in obj {
            assert!(
                !v.is_object() && !v.is_array(),
                "nested value in log line {idx} key `{k}`: {line}"
            );
            assert!(v.as_str() != Some("!"), "refused field `!` in log line {idx} key `{k}`");
        }
        for needle in &forbidden {
            assert!(!line.contains(needle), "log line {idx} leaked forbidden `{needle}`: {line}");
        }
    }

    // Check every file under diagnose/ recursively for zero sensitive hits.
    let mut bundle_files = Vec::new();
    collect_files(&diagnose_root, &mut bundle_files);
    assert!(!bundle_files.is_empty(), "diagnose bundle must contain files");
    for file in &bundle_files {
        let content = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        for needle in &forbidden {
            assert!(
                !content.contains(needle),
                "diagnose file {} leaked forbidden `{needle}`",
                file.display()
            );
        }
    }

    // Verify all 4 P4 structured events + diagnose.wrote appeared, zero stats.full, zero refused.
    let has_event =
        |code: &str| log_lines.iter().any(|l| l.contains(&format!(r#""event":"{code}""#)));
    for required in [
        "identity.auth",
        "quota.updated",
        "identity.adopt",
        "identity.discard_local",
        "diagnose.wrote",
    ] {
        assert!(has_event(required), "expected structured event `{required}` in logs");
    }
    assert!(!has_event("stats.full"), "stats.full must never trigger (Stats table stayed bounded)");
    assert_eq!(zutalk_log::refused(), refused_before, "zutalk_log::refused() must not increase");
}

#[test]
fn command_debug_redacts_recovery_and_own_keys() {
    let recover_secret = "zSecretRecoveryKeyProbe99";
    let recover_cmd = Command::RecoverIdentity {
        did: "did:zu:ewvi7nxzyoun6zhxrhs64oiz".into(),
        key: recover_secret.into(),
    };
    let recover_dbg = format!("{recover_cmd:?}");
    assert!(
        !recover_dbg.contains(recover_secret),
        "Command::RecoverIdentity Debug leaked recovery key: {recover_dbg}"
    );
    assert!(recover_dbg.contains("RecoverIdentity"));
    assert!(recover_dbg.contains("did:zu:ewvi7nxzyoun6zhxrhs64oiz"));
    assert!(recover_dbg.contains('…'));

    let api_secret = "sk-secret-own-key-probe-99";
    let own_some = Command::OwnKey { service: "soniox".into(), key: Some(api_secret.into()) };
    let own_some_dbg = format!("{own_some:?}");
    assert!(
        !own_some_dbg.contains(api_secret),
        "Command::OwnKey Debug leaked API key: {own_some_dbg}"
    );
    assert!(own_some_dbg.contains("OwnKey"));
    assert!(own_some_dbg.contains("soniox"));
    assert!(own_some_dbg.contains('…'));

    let own_none = Command::OwnKey { service: "openrouter".into(), key: None };
    let own_none_dbg = format!("{own_none:?}");
    assert!(own_none_dbg.contains("OwnKey"));
    assert!(own_none_dbg.contains("openrouter"));
    assert!(own_none_dbg.contains("None"));
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_flow_refuses_overlapping_commands_while_creating_and_allows_cancel() {
    let _store = STORE.lock().await;
    store(false);
    let directory = MockServer::start().await;
    // Delay POST long enough so CreateIdentity stays in AuthFlow::Creating.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&directory)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut app =
        App::with(Config { directory: directory.uri(), ..config_on(dir.path(), NOWHERE) });
    app.until(|_| true).await;

    app.send(Command::CreateIdentity { handle: None });
    app.until(|s| s.auth == AuthFlow::Creating).await;

    // Overlapping SignIn / CreateIdentity / RecoverIdentity while Creating must be refused with
    // "busy" without cancelling or deadlocking AuthFlow::Creating.
    app.send(Command::SignIn);
    app.notice("busy").await;
    assert_eq!(app.outlet.state.now().1.auth, AuthFlow::Creating);

    app.send(Command::CreateIdentity { handle: None });
    app.notice("busy").await;
    assert_eq!(app.outlet.state.now().1.auth, AuthFlow::Creating);

    app.send(Command::RecoverIdentity {
        did: "did:zu:ewvi7nxzyoun6zhxrhs64oiz".into(),
        key: "zFakeRecoveryKey".into(),
    });
    app.notice("busy").await;
    assert_eq!(app.outlet.state.now().1.auth, AuthFlow::Creating);

    assert!(
        app.seen
            .of("command.done")
            .iter()
            .any(|l| l.text("kind") == Some("SignIn") && l.text("code") == Some("busy"))
    );

    // Explicit CancelSignIn cancels the in-flight creation and restores Idle so a new SignIn works.
    app.send(Command::CancelSignIn);
    app.until(|s| s.auth == AuthFlow::Idle).await;

    app.send(Command::SignIn);
    app.until(|s| matches!(s.auth, AuthFlow::Scanning { .. })).await;
    app.send(Command::CancelSignIn);
    app.until(|s| s.auth == AuthFlow::Idle).await;
    assert_eq!(app.seen.refused(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn unsaved_rotation_key_is_scoped_to_creating_did_and_never_leaks_across_accounts() {
    let _store = STORE.lock().await;
    store(false);
    let plc = Arc::new(MemoryPlc::default());
    let directory = MockServer::start().await;
    Mock::given(method("POST")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;
    Mock::given(method("GET")).respond_with(PlcResponder(plc.clone())).mount(&directory).await;

    // Seed account B in the stand-in directory.
    let key_b = SecretKey::from_seed(&[71u8; 32]);
    let rot_b = did::Rotation::from_bytes(&[72u8; 32]).unwrap();
    let (did_b, op_b) =
        did::create_with_aka(&rot_b, &key_b.public(), &["zu://bravo.zuzalu.ink".into()]).unwrap();
    plc.push_op(&did_b, serde_json::from_str(&op_b).unwrap());

    let copied = Arc::new(Mutex::new(String::new()));
    let copied_cb = copied.clone();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = Config { directory: directory.uri(), ..config_on(dir.path(), NOWHERE) };
    cfg.clip = Arc::new(move |text| {
        *copied_cb.lock().unwrap() = text.to_owned();
        true
    });

    let mut app = App::with(cfg);
    app.until(|_| true).await;

    // 1. Create account A -> rotation_unsaved is true and CopyRotation copies A's recovery key.
    app.send(Command::CreateIdentity { handle: Some("alpha".into()) });
    let state_a = app.until(|s| s.me.is_some()).await;
    let did_a = state_a.me.clone().expect("account A DID");
    assert!(state_a.rotation_unsaved);
    app.send(Command::CopyRotation);
    app.notice("copied").await;
    let rot_a = copied.lock().unwrap().clone();
    assert!(rot_a.starts_with('z'));

    // 2. Sign out without saving -> signed-out state must show rotation_unsaved == false and
    //    CopyRotation must refuse with rotation.none.
    app.send(Command::SignOut);
    let signed_out = app.until(|s| s.me.is_none()).await;
    assert!(!signed_out.rotation_unsaved);
    copied.lock().unwrap().clear();
    app.send(Command::CopyRotation);
    app.notice("rotation.none").await;
    assert!(copied.lock().unwrap().is_empty(), "signed-out CopyRotation must not copy A's key");

    // 3. Sign in as account B -> rotation_unsaved must be false and CopyRotation must refuse with
    //    rotation.none (never leaking account A's recovery key to account B).
    let rot_b_key = rot_b.multikey().to_string();
    app.send(Command::RecoverIdentity { did: did_b.clone(), key: rot_b_key.clone() });
    let state_b = app.until(|s| s.me.as_deref() == Some(did_b.as_str())).await;
    assert!(!state_b.rotation_unsaved);
    app.send(Command::CopyRotation);
    app.notice("rotation.none").await;
    assert!(copied.lock().unwrap().is_empty(), "account B must never receive account A's key");

    // RotationSaved by B must not wipe A's unsaved reminder.
    app.send(Command::RotationSaved);
    app.send(Command::SignOut);
    app.until(|s| s.me.is_none()).await;

    // 4. Sign back in as account A -> A's unsaved reminder is still scoped to A.
    app.send(Command::RecoverIdentity { did: did_a.clone(), key: rot_a.clone() });
    let state_a_again = app.until(|s| s.me.as_deref() == Some(did_a.as_str())).await;
    assert!(state_a_again.rotation_unsaved);
    app.send(Command::CopyRotation);
    app.notice("copied").await;
    assert_eq!(copied.lock().unwrap().as_str(), rot_a.as_str());

    let dumped = format!("{:?}", app.seen.all());
    assert!(!dumped.contains(&rot_a), "account A rotation key leaked in logs");
    assert!(!dumped.contains(&rot_b_key), "account B rotation key leaked in logs");
    assert!(!dumped.contains(&did_a), "account A DID leaked in logs");
    assert!(!dumped.contains(&did_b), "account B DID leaked in logs");
    assert_eq!(app.seen.refused(), 0);
}
