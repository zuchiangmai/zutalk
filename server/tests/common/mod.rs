//! Shared by the tests: made-up identities, a directory and two upstreams that do what a
//! test says, signed records to send, and the router in-process on a data directory of its own.
#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use zu::{Cid, Codec, Doc, Path, PublicKey, Record, SecretKey, Value};
use zutalk_server::{BoxFuture, Config, Directory, Minted, Refusal, Server, Upstream, app, now};

pub const ORIGIN: &str = "https://talk.example";
/// The coordinator the server trusts is `did(9)`.
pub const COORDINATOR: u8 = 9;
pub const DAY: i64 = 24 * 60 * 60 * 1000;

pub fn secret(n: u8) -> SecretKey {
    SecretKey::from_seed(&[n; 32])
}

pub fn did(n: u8) -> String {
    format!("did:zu:made-up-{n}")
}

/// `did(n)` has the key `secret(n)`, unless a test says otherwise.
#[derive(Default)]
pub struct Keys(pub Mutex<HashMap<String, PublicKey>>);

impl Directory for Keys {
    fn doc<'a>(&'a self, did_: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>> {
        Box::pin(async move {
            let set = self.0.lock().unwrap().get(did_).copied();
            let made = (0..=20).find(|n| did(*n) == did_).map(|n| secret(n).public());
            let keys = set.or(made).map(|key| (key, None)).into_iter().collect();
            Ok(Doc { did: did_.into(), keys, handles: vec![], services: BTreeMap::new() })
        })
    }
}

/// An upstream that does what the test says, and remembers what it was asked.
#[derive(Default)]
pub struct Script {
    pub slice: u64,
    pub down: AtomicBool,
    /// A key good for this many µ$ a second lets a connection last `micros / per_second`.
    pub per_second: u64,
    pub minted: Mutex<Vec<(String, u64)>>,
}

pub const SECRET_KEY: &str = "sk-the-key-that-must-not-be-kept-";

impl Upstream for Script {
    fn slice(&self) -> u64 {
        self.slice
    }
    fn per_second(&self) -> u64 {
        self.per_second
    }
    fn mint<'a>(&'a self, name: &'a str, micros: u64) -> BoxFuture<'a, Result<Minted, Refusal>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            if self.down.load(Ordering::SeqCst) {
                return Err(Refusal(StatusCode::BAD_GATEWAY, "upstream"));
            }
            self.minted.lock().unwrap().push((name.to_owned(), micros));
            let session_cap_s = (self.per_second > 0).then(|| micros / self.per_second);
            Ok(Minted { key: format!("{SECRET_KEY}{name}"), session_cap_s })
        })
    }
}

pub struct Bench {
    pub dir: Arc<tempfile::TempDir>,
    pub server: Arc<Server>,
    pub app: Router,
    pub keys: Arc<Keys>,
    /// A key costs 180,000 µ$: an hour at 50 µ$ a second.
    pub soniox: Arc<Script>,
    /// A key costs 500,000 µ$.
    pub openrouter: Arc<Script>,
}

pub fn bench() -> Bench {
    open(Arc::new(tempfile::tempdir().unwrap()), |_| {})
}

/// A bench of its own, with another `Config`.
pub fn bench_with(change: impl FnOnce(&mut Config)) -> Bench {
    open(Arc::new(tempfile::tempdir().unwrap()), change)
}

/// The same data directory after the process ended and started again.
pub fn restart(bench: Bench) -> Bench {
    let dir = bench.dir.clone();
    drop(bench);
    open(dir, |_| {})
}

fn open(dir: Arc<tempfile::TempDir>, change: impl FnOnce(&mut Config)) -> Bench {
    let mut config = Config::new(ORIGIN, dir.path(), vec![did(COORDINATOR)]);
    change(&mut config);
    let keys = Arc::new(Keys::default());
    let soniox = Arc::new(Script { slice: 180_000, per_second: 50, ..Script::default() });
    let openrouter = Arc::new(Script { slice: 500_000, ..Script::default() });
    let upstreams: [Arc<dyn Upstream>; 2] = [soniox.clone(), openrouter.clone()];
    let server = Server::open(config, keys.clone(), upstreams).unwrap();
    Bench { dir, app: app(server.clone()), server, keys, soniox, openrouter }
}

/// A request, with an `Authorization` header if one is given.
pub async fn send(
    app: &Router,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    body: impl Into<Body>,
) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(authorization) = authorization {
        request = request.header(header::AUTHORIZATION, authorization);
    }
    // A body that is a JSON object is sent as JSON.
    let bytes = body.into().collect().await.unwrap().to_bytes();
    if bytes.starts_with(b"{") {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    app.clone().oneshot(request.body(Body::from(bytes)).unwrap()).await.unwrap()
}

/// The status and the JSON of an answer to a `POST`. `null` when the body is not JSON.
pub async fn post(app: &Router, path: &str, body: impl Into<Body>) -> (StatusCode, Json) {
    let answer = send(app, "POST", path, None, body).await;
    let status = answer.status();
    let bytes = answer.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
}

/// The status and the stable code of a refusal.
pub async fn code(app: &Router, path: &str, body: impl Into<Body>) -> (StatusCode, String) {
    let (status, answer) = post(app, path, body).await;
    (status, answer["error"].as_str().unwrap_or("").to_owned())
}

pub fn json(value: &Value) -> Json {
    serde_json::from_str(&value.to_json().unwrap()).unwrap()
}

/// A body of the data model from its members.
pub fn map(members: &[(&str, Value)]) -> Value {
    Value::Map(members.iter().map(|(name, value)| (name.to_string(), value.clone())).collect())
}

/// What the server takes: a record and its body.
pub fn delivery(record: &Record, body: Option<&Value>) -> String {
    serde_json::json!({ "record": json(&record.header()), "body": body.map(json) }).to_string()
}

static KEY: AtomicU64 = AtomicU64::new(1);

/// A record of `did(by)`, signed with `secret(signer)`, at a key no other record has.
pub fn signed(signer: u8, by: u8, ty: &str, body: &Value, at: i64) -> Record {
    let key = format!("r{}", KEY.fetch_add(1, Ordering::SeqCst));
    let path = Path::new(&did(by), ty, &key).unwrap();
    let cid = Cid::of(Codec::Drisl, &body.encode());
    Record::sign(&secret(signer), path, Some(cid), [], at, None).unwrap()
}

/// A fresh request of `did(n)`'s for a key of `service`, made for this server.
pub fn ask(n: u8, service: &str) -> String {
    let body = map(&[("service", Value::Str(service.into())), ("aud", Value::Str(ORIGIN.into()))]);
    delivery(&signed(n, n, "talk/lease", &body, now()), Some(&body))
}

/// `did(n)` asks for a key of `service`.
pub async fn lease(bench: &Bench, n: u8, service: &str) -> (StatusCode, Json) {
    post(&bench.app, "/lease", ask(n, service)).await
}

/// The body of a grant, and a `talk/grant` record for it signed by `secret(by)` as `did(by)`.
pub fn grant(
    by: u8,
    key: &str,
    to: &str,
    cents: i64,
    until: i64,
    prev: &[&Record],
) -> (Record, Value) {
    let body = map(&[
        ("to", Value::Str(to.into())),
        ("cents", Value::Int(cents)),
        ("until", Value::Int(until)),
    ]);
    let cid = Cid::of(Codec::Drisl, &body.encode());
    let path = Path::new(&did(by), "talk/grant", key).unwrap();
    let at = prev.iter().map(|record| record.at + 1).max().unwrap_or(now());
    let prev = prev.iter().map(|record| record.cid());
    (Record::sign(&secret(by), path, Some(cid), prev, at, None).unwrap(), body)
}

/// Grant `cents` to `did(to)` until `until`, and deliver it.
pub async fn give_until(bench: &Bench, to: u8, cents: i64, until: i64) -> Record {
    let key = format!("k{}", KEY.fetch_add(1, Ordering::SeqCst));
    let (record, body) = grant(COORDINATOR, &key, &did(to), cents, until, &[]);
    let delivered = post(&bench.app, "/grants", delivery(&record, Some(&body)));
    assert_eq!(delivered.await.0, StatusCode::OK);
    record
}

/// Grant `cents` to `did(to)` for a day, and deliver it.
pub async fn give(bench: &Bench, to: u8, cents: i64) -> Record {
    give_until(bench, to, cents, now() + DAY).await
}

/// What `did(n)` has left, in µ$.
pub fn left(bench: &Bench, n: u8) -> u64 {
    bench.server.left(&did(n), now())
}

pub fn ledger(bench: &Bench) -> Vec<u8> {
    std::fs::read(bench.dir.path().join("ledger")).unwrap()
}

/// Every file under the data directory, as bytes.
pub fn disk(bench: &Bench) -> Vec<u8> {
    fn walk(dir: &std::path::Path, into: &mut Vec<u8>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, into)
            } else {
                into.extend(std::fs::read(&path).unwrap())
            }
        }
    }
    let mut bytes = Vec::new();
    walk(bench.dir.path(), &mut bytes);
    bytes
}

/// What `did(n)` signs to do something to a room: `{ room, do, frame }`, for this server, now.
pub fn act(n: u8, room: &str, what: &str, frame: &[u8]) -> String {
    use base64::{Engine, prelude::BASE64_STANDARD};
    let body = map(&[
        ("room", Value::Str(room.into())),
        ("do", Value::Str(what.into())),
        ("frame", Value::Str(BASE64_STANDARD.encode(frame))),
        ("aud", Value::Str(ORIGIN.into())),
    ]);
    delivery(&signed(n, n, "talk/room", &body, now()), Some(&body))
}

/// `did(n)` does `what` to `room`: the status and the JSON of the answer.
pub async fn room_do(
    bench: &Bench,
    n: u8,
    room: &str,
    what: &str,
    frame: &[u8],
) -> (StatusCode, Json) {
    post(&bench.app, "/rooms", act(n, room, what, frame)).await
}

/// A new room of `did(n)`'s.
pub async fn room(bench: &Bench, n: u8) -> String {
    let id = format!("{:032x}", KEY.fetch_add(1, Ordering::SeqCst) + (u64::from(n) << 40));
    assert_eq!(room_do(bench, n, &id, "make", b"").await.0, StatusCode::OK);
    id
}

/// The events of a room as they arrive, `(event, id, data)`; and everything said on the wire.
pub struct Events(Body, String, String);

pub async fn events(app: &Router, path: &str, last: Option<&str>) -> Events {
    let mut request = Request::builder().uri(path);
    if let Some(last) = last {
        request = request.header("last-event-id", last);
    }
    let answer = app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(answer.status(), StatusCode::OK, "{path}");
    assert_eq!(answer.headers()[header::CONTENT_TYPE], "text/event-stream");
    Events(answer.into_body(), String::new(), String::new())
}

impl Events {
    /// The next event, or `None` when the server hung up. Waits two seconds at most.
    pub async fn next(&mut self) -> Option<(String, String, String)> {
        loop {
            if let Some((block, rest)) = self.1.split_once("\n\n") {
                let field = |name: &str| {
                    let found = block.lines().find_map(|line| line.strip_prefix(name));
                    found.unwrap_or("").trim_start().to_owned()
                };
                let event = (field("event:"), field("id:"), field("data:"));
                self.1 = rest.to_owned();
                if !event.0.is_empty() {
                    return Some(event);
                }
                continue;
            }
            let frame = tokio::time::timeout(std::time::Duration::from_secs(2), self.0.frame());
            let frame = frame.await.expect("an event within two seconds")?.unwrap();
            let said = std::str::from_utf8(frame.data_ref().unwrap()).unwrap();
            self.1.push_str(said);
            self.2.push_str(said);
        }
    }

    /// The next `n` events.
    pub async fn take(&mut self, n: usize) -> Vec<(String, String, String)> {
        let mut taken = Vec::new();
        for _ in 0..n {
            taken.push(self.next().await.expect("the stream goes on"));
        }
        taken
    }

    /// Everything said on the wire so far, as it was said.
    pub fn wire(&self) -> &str {
        &self.2
    }
}
