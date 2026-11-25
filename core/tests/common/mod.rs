//! Our server, for real, on a port of this machine; the `did:zu` directory, standing in; a room
//! read the way a viewer reads it; and the folder a 0.8 app left behind.
#![allow(dead_code)]

use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD, Engine};
use reqwest::StatusCode;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use wiremock::MockServer;
use zu::{Cid, Codec, Doc, SecretKey, Value};
use zutalk_core::out::host;
use zutalk_server::{BoxFuture, Config, Directory, Invites, Minted, Refusal, Server, Upstream};

/// `did:zu:made-up-n` has the key `[n; 32]`; any other DID has the key `[5; 32]`.
struct Keys;

impl Directory for Keys {
    fn doc<'a>(&'a self, did: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>> {
        Box::pin(async move {
            let n = did.strip_prefix("did:zu:made-up-").and_then(|n| n.parse().ok()).unwrap_or(5);
            let keys = vec![(SecretKey::from_seed(&[n; 32]).public(), None)];
            Ok(Doc { did: did.into(), keys, handles: vec![], services: BTreeMap::new() })
        })
    }
}

/// The audit log of a DID that one operation made, as the directory serves it.
pub fn audit(did: &str, op: &Json) -> Vec<u8> {
    let cid = Cid::of(Codec::Drisl, &Value::from_json(&op.to_string()).unwrap().encode());
    let entry = json!({ "did": did, "operation": op, "cid": cid.to_string(), "nullified": false,
        "createdAt": "2026-11-16T00:00:00.000Z" });
    serde_json::to_vec(&[entry]).unwrap()
}

/// The `did:zu` directory: it takes whatever is posted to it, and a DID made here resolves to
/// the log of the operation that made it.
pub struct Plc(pub Arc<MockServer>);

impl Directory for Plc {
    fn doc<'a>(&'a self, did: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>> {
        Box::pin(async move {
            let posted = self.0.received_requests().await.unwrap_or_default();
            let unknown = Refusal(StatusCode::FORBIDDEN, "signer");
            let op = posted.iter().find(|r| r.url.path() == format!("/{did}")).ok_or(unknown)?;
            let op: Json = serde_json::from_slice(&op.body).map_err(|_| unknown)?;
            zu::did::resolve(did, &audit(did, &op)).map_err(|_| unknown)
        })
    }
}

/// Rooms need no upstream. A second of live captions is worth 50 µ$, as in the server's tests.
struct Nowhere;

impl Upstream for Nowhere {
    fn slice(&self) -> u64 {
        0
    }
    fn per_second(&self) -> u64 {
        50
    }
    fn mint<'a>(&'a self, _: &'a str, _: u64) -> BoxFuture<'a, Result<Minted, Refusal>> {
        Box::pin(async { Err(Refusal(reqwest::StatusCode::BAD_GATEWAY, "upstream")) })
    }
}

/// The server, its origin, and the folder it keeps its data in.
pub async fn serve() -> (String, tempfile::TempDir) {
    serve_with(Arc::new(Keys), None).await
}

/// The server, with this directory and the 0.8 line's invite service there.
pub async fn serve_with(
    directory: Arc<dyn Directory>,
    invites: Option<Invites>,
) -> (String, tempfile::TempDir) {
    _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let config = Config { invites, ..Config::new(&origin, dir.path(), vec![]) };
    let server = Server::open(config, directory, [Arc::new(Nowhere), Arc::new(Nowhere)]);
    tokio::spawn(zutalk_server::serve(listener, server.unwrap()));
    (origin, dir)
}

/// The room a link names, and its key: all a viewer has.
pub fn from_link(link: &str) -> (String, String, [u8; 32]) {
    let (url, key) = link.split_once('#').unwrap();
    let (base, id) = url.rsplit_once("/r/").unwrap();
    let key = BASE64_URL_SAFE_NO_PAD.decode(key).unwrap().try_into().unwrap();
    (base.to_owned(), id.to_owned(), key)
}

/// A room as a viewer reads it: each event, `log`, `slot` or `gone`, with its frame opened.
pub struct Viewer {
    answer: reqwest::Response,
    said: String,
    key: [u8; 32],
}

impl Viewer {
    pub async fn open(link: &str) -> Viewer {
        let (base, id, key) = from_link(link);
        let answer = reqwest::get(format!("{base}/rooms/{id}/events")).await.unwrap();
        assert!(answer.status().is_success(), "{}", answer.status());
        Viewer { answer, said: String::new(), key }
    }

    /// The next event, and what its frame holds (none for `-`); `None` when the server hung
    /// up. A frame that does not open with the key is `Err`.
    pub async fn next(&mut self) -> Option<(String, Result<Option<Vec<u8>>, ()>)> {
        loop {
            if let Some((block, rest)) = self.said.split_once("\n\n") {
                let field = |name: &str| {
                    block.lines().find_map(|line| line.strip_prefix(name)).unwrap_or("").trim()
                };
                let (event, data) = (field("event:").to_owned(), field("data:").to_owned());
                self.said = rest.to_owned();
                if event.is_empty() {
                    continue;
                }
                let frame = (data != "-").then(|| BASE64_STANDARD.decode(data).unwrap());
                let plain = frame.map(|frame| host::open(&self.key, &frame)).transpose();
                return Some((event, plain.map_err(drop)));
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10), self.answer.chunk());
            let chunk = chunk.await.expect("an event within ten seconds").ok()??;
            self.said.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// The next event that `wanted` takes.
    pub async fn until<T>(
        &mut self,
        mut wanted: impl FnMut(&str, Option<&[u8]>) -> Option<T>,
    ) -> T {
        loop {
            let (event, plain) = self.next().await.expect("the room is read to its end");
            if let Some(found) = wanted(&event, plain.unwrap().as_deref()) {
                return found;
            }
        }
    }
}

/// The folder the 0.8 app kept its data in, as it was handed over (tests/fixtures/old), copied
/// where a test may change it.
pub fn old_library() -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap().map(Result::unwrap) {
            let to = to.join(entry.file_name());
            match entry.file_type().unwrap().is_dir() {
                true => copy(&entry.path(), &to),
                false => drop(fs::copy(entry.path(), to).unwrap()),
            }
        }
    }
    let old = tempfile::tempdir().unwrap();
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/old/ZuTalk"), old.path());
    old
}
