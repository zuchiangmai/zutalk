//! The hub, run as the shell runs it: a signed-in person, a microphone that hums, a folder of
//! its own, and every service out of reach unless a test stands one in.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zu::SecretKey;
use zutalk_core::ai::write::wav_header;
use zutalk_core::app::hub::{Config, Core};
use zutalk_core::app::proto::{Command, Event, State};
use zutalk_core::capture::audio::{Capture, Feed, Keep, Notice, Open, Source, offer};
use zutalk_core::capture::clock::{BLOCK, Block, Gap, wall_ms};
use zutalk_core::capture::tape;
use zutalk_core::id::keys;
use zutalk_core::record::shelf::Shelf;
use zutalk_core::rt::supervise::End;

pub const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
/// Nothing listens here: every service is out of reach.
pub const NOWHERE: &str = "https://127.0.0.1:9";

/// The key store is the process's: one test at a time has it.
pub static STORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A key store with someone signed in, and their own Soniox key.
pub fn signed_in() {
    keyring_core::set_default_store(keyring_core::mock::Store::new().expect("a mock store"));
    let kept = [&[5; 32][..], ME.as_bytes()].concat();
    keys::set("identity", Some(&kept)).expect("the identity is kept");
    keys::set("soniox", Some(b"sk-own-test-key")).expect("the key is kept");
}

/// The shelf the hub keeps this person's records on, under `data`.
pub fn shelf(data: &Path) -> Shelf {
    let root = data.join("records").join(ME.replace(':', "-"));
    Shelf::new(root, ME, Arc::new(SecretKey::from_seed(&[5; 32])))
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
fn hum(
    source: Source,
    sink: Box<dyn FnMut(Block) + Send>,
    notice: Box<dyn FnMut(Notice) + Send>,
) -> Result<Capture, End> {
    if let Source::File(_) = source {
        return Capture::start(source, |_| Gap::Keep, sink, notice);
    }
    let open: Open = Box::new(|_| {
        let (mut ring, feed) = Feed::new(16_000, 1)?;
        thread::spawn(move || {
            while !ring.is_abandoned() {
                offer(&mut ring, (0..320).map(|i| (i as f32 * 0.17).sin() * 0.3));
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok((feed, Box::new(|| None) as Keep))
    });
    Ok(Capture::with(vec![open], |_| Gap::Keep, wall_ms, sink, notice))
}

/// With nothing in reach, and a microphone that hums.
pub fn config(data: &Path) -> Config {
    let (soniox, openrouter, directory) = (NOWHERE.into(), NOWHERE.into(), NOWHERE.into());
    let realtime = "ws://127.0.0.1:9".into();
    Config {
        soniox,
        openrouter,
        directory,
        realtime,
        hear: hum,
        ..Config::new(data.into(), NOWHERE)
    }
}

pub struct App {
    core: Core,
    events: UnboundedReceiver<Event>,
}

impl App {
    pub fn with(config: Config) -> App {
        let (tx, events) = unbounded_channel();
        let core = Core::start(config, move |event| drop(tx.send(event))).expect("the core starts");
        App { core, events }
    }

    pub fn send(&self, command: Command) {
        self.core.send(command);
    }

    /// Waits for an event that `wanted` takes.
    pub async fn next<T>(&mut self, mut wanted: impl FnMut(Event) -> Option<T>) -> T {
        let wait = async {
            loop {
                if let Some(found) = wanted(self.events.recv().await.expect("the core runs")) {
                    return found;
                }
            }
        };
        timeout(Duration::from_secs(20), wait).await.expect("it came in time")
    }

    pub async fn until(&mut self, holds: impl Fn(&State) -> bool) -> State {
        self.next(|event| match event {
            Event::State(state) if holds(&state) => Some(*state),
            _ => None,
        })
        .await
    }

    pub async fn notice(&mut self, code: &str) {
        self.next(|event| matches!(event, Event::Notice(said) if said == code).then_some(())).await
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
