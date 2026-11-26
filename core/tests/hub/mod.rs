//! The hub, run as the shell runs it: a signed-in person, a microphone that hums, a folder of
//! its own, and every service out of reach unless a test stands one in.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::time::timeout;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zu::SecretKey;
use zutalk_core::ai::write::wav_header;
use zutalk_core::app::hub::{Config, Core, Outlet};
use zutalk_core::app::proto::{
    Command, Frame, LibraryFilter, LibraryRow, Progress, Query, QueryReply, QueryValue,
    RecordingView, SearchResult, Session, State, TranscriptChunk,
};
use zutalk_core::capture::audio::{Capture, Feed, Hearing, Keep, Open, Sink, Source, Tell, offer};
use zutalk_core::capture::clock::{BLOCK, Block, Gap, wall_ms};
use zutalk_core::capture::tape;
use zutalk_core::id::keys;
use zutalk_core::knowledge::glossary::Term;
use zutalk_core::out::city::Calendar;
use zutalk_core::record::shelf::Shelf;
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::End;

pub const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
pub const LOCAL_SEED: [u8; 32] = [7; 32];
/// Nothing listens here: every service is out of reach.
pub const NOWHERE: &str = "https://127.0.0.1:9";

/// The key store is the process's: one test at a time has it.
pub static STORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A mock key store, optionally with `ME` signed in and no service keys pre-installed.
pub fn store(signed_in: bool) {
    keyring_core::set_default_store(keyring_core::mock::Store::new().expect("a mock store"));
    if signed_in {
        let kept = [&[5; 32][..], ME.as_bytes()].concat();
        keys::set("identity", Some(&kept)).expect("the identity is kept");
    }
}

/// A key store with someone signed in, and their own Soniox key.
pub fn signed_in() {
    store(true);
    keys::set("soniox", Some(b"sk-own-test-key")).expect("the key is kept");
}

/// An empty key store with only the device's own identity seed and an own Soniox key.
pub fn nobody() {
    keyring_core::set_default_store(keyring_core::mock::Store::new().expect("a mock store"));
    keys::set("identity.local", Some(&LOCAL_SEED)).expect("the local identity is kept");
    keys::set("soniox", Some(b"sk-own-test-key")).expect("the key is kept");
}

/// The device's own identity for [`nobody`].
pub fn local_did() -> String {
    zu::did::local(&SecretKey::from_seed(&LOCAL_SEED).public())
}

/// The shelf the hub keeps this person's records on, under `data`.
pub fn shelf(data: &Path) -> Shelf {
    let root = data.join("records").join(ME.replace(':', "-"));
    Shelf::new(root, ME, Arc::new(SecretKey::from_seed(&[5; 32])))
}

/// The shelf the hub keeps the device's own records on, under `data`, for [`nobody`].
pub fn local_shelf(data: &Path) -> Shelf {
    let did = local_did();
    let root = data.join("records").join(did.replace(':', "-"));
    Shelf::new(root, &did, Arc::new(SecretKey::from_seed(&LOCAL_SEED)))
}

/// A tape of `seconds`, sealed with the key the hub would seal it with.
pub fn tape(data: &Path, id: &str, seconds: u32) {
    let key = keys::derive("tape", id).expect("a tape key");
    let mut tape = tape::Writer::create(&data.join("tape").join(id), &key).expect("a tape");
    for n in 0..seconds * 10 {
        let pcm = (0..BLOCK).map(|i| ((i as i32 * 37 + n as i32) % 2_000 - 1_000) as i16);
        tape.append(&Block { t_ms: n * 100, pcm: pcm.collect() }).expect("a block");
    }
}

/// A sound file of `ms`.
pub fn wav(file: &Path, ms: u32) {
    let samples = ms * 16;
    let pcm = (0..samples).flat_map(|i| (((i as f32 * 0.05).sin() * 8_000.0) as i16).to_le_bytes());
    std::fs::write(file, [wav_header(samples), pcm.collect()].concat()).expect("it is written");
}

/// A microphone that hums, in real time, until it is closed. A file is read as it is.
fn hum(source: Source, sink: Sink, notice: Tell, stats: &Stats) -> Result<Box<dyn Hearing>, End> {
    if let Source::File(_) = source {
        let capture = Capture::start(source, |_| Gap::Keep, stats, sink, notice)?;
        return Ok(Box::new(capture));
    }
    let open: Open = Box::new(|_, lost| {
        let (mut ring, feed) = Feed::new(16_000, 1, lost.clone())?;
        thread::spawn(move || {
            while !ring.is_abandoned() {
                offer(&mut ring, (0..320).map(|i| (i as f32 * 0.17).sin() * 0.3));
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok((feed, Box::new(|| None) as Keep))
    });
    Ok(Box::new(Capture::with(vec![open], |_| Gap::Keep, stats, wall_ms, sink, notice)))
}

/// With nothing in reach, and a microphone that hums.
pub fn config(data: &Path) -> Config {
    config_on(data, NOWHERE)
}

/// With our server at `server`, nothing else in reach, and a microphone that hums.
pub fn config_on(data: &Path, server: &str) -> Config {
    let (soniox, openrouter, directory) = (NOWHERE.into(), NOWHERE.into(), NOWHERE.into());
    let realtime = "ws://127.0.0.1:9".into();
    Config {
        soniox,
        openrouter,
        directory,
        realtime,
        hear: Arc::new(hum),
        devices: Arc::new(Vec::new),
        ..Config::new(data.into(), server)
    }
}

pub struct App {
    pub core: Core,
    pub outlet: Outlet,
    pub seen: zutalk_log::Seen,
    said_at: u64,
}

impl App {
    pub fn start(data: &Path) -> App {
        App::with(config(data))
    }

    /// With our server at `server`, and the realtime service at `realtime`.
    pub fn on(data: &Path, server: &str, realtime: &str) -> App {
        App::with(Config { realtime: realtime.into(), ..config_on(data, server) })
    }

    pub fn with(config: Config) -> App {
        App::with_log(config, zutalk_log::Level::Info)
    }

    pub fn with_log(config: Config, level: zutalk_log::Level) -> App {
        let seen = zutalk_log::capture(level);
        let (core, outlet) = Core::start(config).expect("the core starts");
        App { core, outlet, seen, said_at: 0 }
    }

    pub fn send(&self, command: Command) {
        self.core.send(command).expect("the hub's mailbox accepts the command");
    }

    pub async fn until(&mut self, holds: impl Fn(&State) -> bool) -> State {
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
        timeout(Duration::from_secs(20), wait).await.expect("it came in time")
    }

    pub async fn frame(&mut self, holds: impl Fn(&Frame) -> bool) -> Frame {
        let wait = async {
            let (seq, frame) = self.outlet.frame.now();
            if seq > 0 && holds(&frame) {
                return (*frame).clone();
            }
            while let Some((_, frame)) = self.outlet.frame.next().await {
                if holds(&frame) {
                    return (*frame).clone();
                }
            }
            panic!("the core stopped");
        };
        timeout(Duration::from_secs(20), wait).await.expect("it came in time")
    }

    pub async fn session(&mut self, holds: impl Fn(Option<&Session>) -> bool) -> Option<Session> {
        let wait = async {
            loop {
                let (_, current) = self.outlet.session.now();
                if holds((*current).as_ref()) {
                    return (*current).clone();
                }
                self.outlet.session.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("it came in time")
    }

    pub async fn notice(&mut self, code: &str) {
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
        timeout(Duration::from_secs(20), wait).await.expect("it came in time")
    }

    pub async fn query(&self, q: Query) -> Result<QueryReply, &'static str> {
        self.core.query(q).await
    }

    pub async fn library(&self) -> Vec<LibraryRow> {
        match self
            .core
            .query(Query::LibraryRecent { filter: LibraryFilter::default(), limit: 200, offset: 0 })
            .await
            .expect("library query")
            .value
        {
            QueryValue::LibraryPage(page) => page.rows,
            other => panic!("unexpected library reply: {other:?}"),
        }
    }

    pub async fn recording(&self, id: &str) -> Option<RecordingView> {
        match self
            .core
            .query(Query::Recording { id: id.into() })
            .await
            .expect("recording query")
            .value
        {
            QueryValue::Recording(view) => view.map(|b| *b),
            other => panic!("unexpected recording reply: {other:?}"),
        }
    }

    pub async fn transcript(&self, id: &str, take: Option<u32>, chunk: u32) -> TranscriptChunk {
        match self
            .core
            .query(Query::Transcript { id: id.into(), take, chunk })
            .await
            .expect("transcript query")
            .value
        {
            QueryValue::Transcript(chunk) => chunk,
            other => panic!("unexpected transcript reply: {other:?}"),
        }
    }

    pub async fn search(&self, q: &str) -> SearchResult {
        match self
            .core
            .query(Query::Search { query: q.into(), limit: 200 })
            .await
            .expect("search query")
            .value
        {
            QueryValue::Search(res) => res,
            other => panic!("unexpected search reply: {other:?}"),
        }
    }

    pub async fn glossary(&self) -> Vec<Term> {
        match self.core.query(Query::Glossary).await.expect("glossary query").value {
            QueryValue::Glossary(terms) => terms,
            other => panic!("unexpected glossary reply: {other:?}"),
        }
    }

    pub async fn calendar(&self) -> Option<Calendar> {
        match self.core.query(Query::Calendar).await.expect("calendar query").value {
            QueryValue::Calendar(cal) => cal,
            other => panic!("unexpected calendar reply: {other:?}"),
        }
    }

    pub async fn wait_library(&self, holds: impl Fn(&[LibraryRow]) -> bool) -> Vec<LibraryRow> {
        let mut inv = self.outlet.invalidated.clone();
        let mut state = self.outlet.state.clone();
        let wait = async {
            while state.now().0 == 0 {
                state.next().await.expect("the core stopped");
            }
            loop {
                if let Ok(reply) = self
                    .core
                    .query(Query::LibraryRecent {
                        filter: LibraryFilter::default(),
                        limit: 200,
                        offset: 0,
                    })
                    .await
                    && let QueryValue::LibraryPage(page) = reply.value
                    && holds(&page.rows)
                {
                    return page.rows;
                }
                let _ = timeout(Duration::from_millis(50), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("library condition came in time")
    }

    pub async fn wait_recording(
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

    pub async fn wait_progress(&mut self, holds: impl Fn(&Progress) -> bool) -> Progress {
        let wait = async {
            loop {
                let (_, prog) = self.outlet.progress.now();
                if holds(&prog) {
                    return (*prog).clone();
                }
                self.outlet.progress.next().await.expect("the core runs");
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("progress came in time")
    }

    pub async fn wait_glossary(&self, holds: impl Fn(&[Term]) -> bool) -> Vec<Term> {
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

    pub async fn wait_calendar(
        &self,
        holds: impl Fn(Option<&Calendar>) -> bool,
    ) -> Option<Calendar> {
        let mut inv = self.outlet.invalidated.clone();
        let wait = async {
            loop {
                if let Ok(reply) = self.core.query(Query::Calendar).await
                    && let QueryValue::Calendar(cal) = reply.value
                    && holds(cal.as_ref())
                {
                    return cal;
                }
                let _ = timeout(Duration::from_millis(50), inv.next()).await;
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("calendar condition came in time")
    }
}

/// What a whole-session service was asked to transcribe, each body as it came.
pub type Asked = Arc<Mutex<Vec<Value>>>;

/// A whole-session transcription service that hears "Zuzalu is in Chiang Mai." in any file, and
/// keeps what it was asked.
pub async fn soniox(asked: &Asked) -> MockServer {
    let server = MockServer::start().await;
    let reply = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    let file = reply(json!({ "id": "file-1" }));
    Mock::given(method("POST")).and(path("/v1/files")).respond_with(file).mount(&server).await;
    let asked = asked.clone();
    let job = move |request: &Request| {
        asked.lock().expect("one at a time").push(request.body_json().expect("JSON"));
        ResponseTemplate::new(200).set_body_json(json!({ "id": "tr-1" }))
    };
    Mock::given(method("POST"))
        .and(path("/v1/transcriptions"))
        .respond_with(job)
        .mount(&server)
        .await;
    let done = reply(json!({ "status": "completed" }));
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions/tr-1"))
        .respond_with(done)
        .mount(&server)
        .await;
    let words = [" Zuzalu", " is", " in", " Chiang", " Mai", "."].iter().enumerate();
    let tokens: Vec<Value> = words
        .map(|(i, text)| {
            json!({ "text": text, "start_ms": i * 200, "end_ms": i * 200 + 180,
                                 "language": "en", "speaker": 1 })
        })
        .collect();
    let transcript = reply(json!({ "id": "tr-1", "tokens": tokens }));
    let at = path("/v1/transcriptions/tr-1/transcript");
    Mock::given(method("GET")).and(at).respond_with(transcript).mount(&server).await;
    for kind in ["files", "transcriptions"] {
        let none = reply(json!({ kind: [], "next_page_cursor": null }));
        Mock::given(method("GET"))
            .and(path(format!("/v1/{kind}")))
            .respond_with(none)
            .mount(&server)
            .await;
    }
    let gone = ResponseTemplate::new(204);
    Mock::given(method("DELETE")).and(path_regex("^/v1/")).respond_with(gone).mount(&server).await;
    server
}
