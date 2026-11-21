//! The ZuTalk server: a notary. Whoever acts here sends a signed Zu record — a coordinator's
//! grant, a person's request for a key or claim of an old invite, an owner's write to a room —
//! and the server only
//! checks it, keeps what it needs as proof, and answers. Keys it makes it does not keep; rooms
//! it keeps are ciphertext. It relays sign-ins, and sees no word of a conversation. Rate
//! limits, TLS, and the pages it is read with are the edge's.

mod ledger;
mod relay;
mod room;

pub use crate::ledger::{Invites, Minted, OpenRouter, Service, Soniox, Upstream};
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router, middleware::map_response};
use serde::Deserialize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::{collections::HashMap, path::PathBuf, time::SystemTime};
use zu::{Cid, Codec, Doc, Record, Value, auth::WINDOW};

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The DID documents. Resolving them is a port: the server only reads what comes back.
pub trait Directory: Send + Sync {
    fn doc<'a>(&'a self, did: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>>;
}

/// What the operator decides: this server's origin, the `aud` every request names
/// (`https://talk.example`); where it keeps the ledger and the rooms; the DIDs whose grants
/// count; the largest room in bytes, the most rooms of one owner, the most readers of one, and
/// how long one is kept after its last write in ms; the most login rooms held at once, and how
/// long each lives in ms (login.md: 5 minutes); the 0.8 line's invite service, without which
/// claims wait.
pub struct Config {
    pub origin: String,
    pub data: PathBuf,
    pub coordinators: Vec<String>,
    pub room: (usize, usize, usize, i64),
    pub relay: (usize, i64),
    pub invites: Option<Invites>,
}

impl Config {
    pub fn new(origin: &str, data: impl Into<PathBuf>, coordinators: Vec<String>) -> Config {
        let (origin, data, room) =
            (origin.into(), data.into(), (8 << 20, 100, 500, 30 * 86_400_000));
        Config { origin, data, coordinators, room, relay: (1000, 300_000), invites: None }
    }
}

pub struct Server {
    pub config: Config,
    directory: Arc<dyn Directory>,
    /// The upstreams by `Service as usize`: a table, not a branch.
    upstreams: [Arc<dyn Upstream>; 2],
    book: Mutex<ledger::Book>,
    rooms: Mutex<HashMap<String, room::Room>>,
    relay: Mutex<HashMap<String, (i64, axum::body::Bytes)>>,
}

/// Why a request was refused: the HTTP status, and a stable code in the body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Refusal(pub StatusCode, pub &'static str);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<zu::Error> for Refusal {
    fn from(_: zu::Error) -> Refusal {
        Refusal(StatusCode::INTERNAL_SERVER_ERROR, "disk")
    }
}

type Answer = Result<Json<serde_json::Value>, Refusal>;

/// A signed record and its body, as JSON: the one way anyone acts here.
#[derive(Deserialize)]
pub struct Delivery {
    record: serde_json::Value,
    body: Option<serde_json::Value>,
}

impl Server {
    pub fn open(
        config: Config,
        directory: Arc<dyn Directory>,
        upstreams: [Arc<dyn Upstream>; 2],
    ) -> Result<Arc<Server>, zu::Error> {
        std::fs::create_dir_all(config.data.join("rooms"))?;
        let slices = [upstreams[0].slice(), upstreams[1].slice()];
        let second = upstreams[Service::Soniox as usize].per_second();
        let book = Mutex::new(ledger::Book::open(&config.data.join("ledger"), slices, second)?);
        let rooms = Mutex::new(room::open_all(&config.data.join("rooms"))?);
        Ok(Arc::new(Server { config, directory, upstreams, book, rooms, relay: Mutex::default() }))
    }

    /// What time does by itself: rooms not written to for too long are removed.
    pub fn sweep(&self, now: i64) {
        room::sweep(self, now);
    }

    /// The record in `sent`, checked: of type `ty`, its body the one signed, by one of `only`
    /// if they are named, with a key its author's document has for its time. A request is
    /// also made now, for this server: its body says `aud`.
    async fn check(
        &self,
        sent: Delivery,
        ty: &str,
        only: Option<&[String]>,
    ) -> Result<(Record, Option<Value>), Refusal> {
        let (bad, refused) =
            (|c| Refusal(StatusCode::BAD_REQUEST, c), |c| Refusal(StatusCode::UNAUTHORIZED, c));
        let record = Value::from_json(&sent.record.to_string()).and_then(Record::try_from);
        let record = record.map_err(|_| bad("record"))?;
        let body = sent.body.map(|body| Value::from_json(&body.to_string()));
        let body = body.transpose().map_err(|_| bad("body"))?;
        if record.path.ty != ty
            || body.as_ref().map(|b| Cid::of(Codec::Drisl, &b.encode())) != record.body
        {
            return Err(bad("record"));
        }
        if only.is_some_and(|only| !only.contains(&record.path.by)) {
            return Err(Refusal(StatusCode::FORBIDDEN, "coordinator"));
        } else if only.is_none() && field(body.as_ref(), "aud") != Some(&self.config.origin) {
            return Err(refused("audience"));
        } else if only.is_none() && now().abs_diff(record.at) > WINDOW {
            return Err(refused("time"));
        }
        let doc = self.directory.doc(&record.path.by).await?;
        let at_of = |cid: &Cid| lock(&self.book).at_of(cid);
        record.verify(|_, key, at| doc.key_at(key, at), at_of).map_err(|e| match e {
            zu::Error::Signer => Refusal(StatusCode::FORBIDDEN, "signer"),
            _ => bad("record"),
        })?;
        Ok((record, body))
    }

    /// What `did` has left at `now`, in µ$.
    pub fn left(&self, did: &str, now: i64) -> u64 {
        lock(&self.book).free(did, now).iter().map(|(_, free)| free).sum()
    }
}

/// A string member of a body that is a map.
fn field<'a>(body: Option<&'a Value>, name: &str) -> Option<&'a String> {
    let Some(Value::Map(map)) = body else { return None };
    if let Some(Value::Str(text)) = map.get(name) { Some(text) } else { None }
}

pub fn app(server: Arc<Server>) -> Router {
    // An answer may hold a key: nothing on the way keeps it.
    let no_store = |mut response: Response| async {
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    };
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/grants", post(ledger::grant))
        .route("/lease", post(ledger::lease))
        .route("/claim", post(ledger::claim))
        .route("/rooms", post(room::write))
        .route("/rooms/{id}/events", get(room::events))
        .route("/.well-known/zu-relay/{room}", any(relay::relay))
        .layer(DefaultBodyLimit::max(relay::MESSAGE))
        .layer(map_response(no_store))
        .with_state(server)
}

/// Serve on `listener`, and sweep every minute, until the process ends.
pub async fn serve(listener: tokio::net::TcpListener, server: Arc<Server>) -> std::io::Result<()> {
    let sweeper = server.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            sweeper.sweep(now());
        }
    });
    axum::serve(listener, app(server)).await
}

/// Milliseconds since 1970, by this machine's clock.
pub fn now() -> i64 {
    SystemTime::UNIX_EPOCH.elapsed().map_or(0, |since| since.as_millis() as i64)
}

/// A lock that a panic elsewhere does not turn into a panic here.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
