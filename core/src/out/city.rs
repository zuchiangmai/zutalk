//! A city's archive: the one place a recording goes in the open, to stay, and the one place
//! besides its transcription that its audio may go (`tape::Pass::City`). A city is a Zu host
//! (record.md, query.md, blob.md), and ZuTalk is no app of the city's: what goes is the person's
//! own records, signed with the identity they already have and addressed to the city in `to`.
//! That is the recording, its lines in every language, its speakers' names and, unless they
//! said no, its audio as files; never its note. No request needs another proof: records are
//! signed, and a file is taken only when a record names it.

use crate::ai::write::wav_header;
use crate::app::proto::Writing;
use crate::capture::clock::wall_ms;
use crate::capture::tape::{self, Pass};
use crate::id::keys::{self, Me};
use crate::record::shelf::{FILING, LINE, RECORDING, Recording, SPEAKER, Shelf};
use crate::rt::queue::{self, Full};
use crate::rt::stats::Counter;
use crate::rt::supervise::{End, Tree, again};
use reqwest::{Client, Response, StatusCode, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};
use ts_rs::TS;
use zu::{Blob, Cid, Codec, Held, Path, Query, Record, Value};
use zutalk_log::{info, warn};

const EVENT: &str = "ev/event"; // a city's events: records of this type meant for it
/// Audio goes as WAV files of this many ms each, well under what a host takes as one file.
const PART: u32 = 5 * 60_000;
const BATCH: usize = 200; // records given to a host in one request
/// WAV parts buffered between the tape reader and the city uploader: one part at a time so audio
/// never sits whole in memory (`P1.md` §3.8).
pub const CITY_QUEUE_PARTS: usize = 1;

/// What a host's word for a record means here (record.md § Delivering). A word not known, or
/// none, is `deny`. `want`: it was given the body, and may take it next time.
const WORDS: [(&str, End); 6] = [
    ("ok", End::Done),
    ("want", End::Retry("city.busy")),
    ("busy", End::Retry("city.busy")),
    ("bad", End::Fatal("city.bad")),
    ("big", End::Fatal("city.big")),
    ("full", End::Fatal("city.full")),
];

/// A city's calendar, as its host serves it: the events a recording can be filed to.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Calendar {
    pub address: String, // as the person gave it
    pub did: String,     // the one its domain confirms
    pub events: Vec<CityEvent>,
}

/// An event of a city: the path of its record, its title, and when it starts, if it says.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CityEvent {
    pub path: String,
    pub title: String,
    #[ts(type = "number | null")]
    pub start: Option<i64>,
}

/// Where a recording was filed and how that went, as the person keeps it (`talk/filing`, under
/// the recording's id; a change is one more record). `state` is empty while it goes, then
/// `city.filed`, or the code of what stopped it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct Filing {
    pub city: String,  // its address
    pub did: String,   // `to` of all that goes
    pub event: String, // the path of the event's record
    pub title: String, // the event's, as it read then
    pub audio: bool,   // the audio goes with the text
    pub key: String,   // the recording's at the city: random, as path.md asks
    pub state: String,
}

/// The origin of the city at `address`: a domain, over HTTPS; or, to try a city on this
/// machine, `http://localhost` or `http://127.0.0.1` with a port.
pub fn origin(address: &str) -> Result<String, &'static str> {
    let address = address.trim().trim_end_matches('/');
    let local =
        ["http://localhost:", "http://127.0.0.1:"].iter().find_map(|o| address.strip_prefix(o));
    match local {
        Some(port) => port.parse::<u16>().map(|_| address.into()).map_err(|_| "city.address"),
        None => zu::did::origin(address.trim_start_matches("https://"), false)
            .map_err(|_| "city.address"),
    }
}

/// What goes to the city for `filing`, signed by `me`: the recording as it reads, under the
/// filing's key in place of its own id, saying the event it is filed to and its audio's parts,
/// each with where it starts (none when no audio goes, so that none filed before stays named).
/// Records are signed a millisecond apart from `at` on, as one signer's clock must run
/// (record.md § Time).
pub fn records(
    me: &Me,
    filing: &Filing,
    recording: &Recording,
    audio: &[(u32, Blob)],
    at: i64,
) -> Result<Held, zu::Error> {
    let key = |own: &str| own.replacen(&recording.id, &filing.key, 1);
    let value = |json: Json| Value::from_json(&json.to_string());
    let part = |(t0, file): &(u32, Blob)| {
        let fields = [("t0".to_owned(), Value::Int(i64::from(*t0))), ("file".into(), file.value())];
        Value::Map(fields.into())
    };
    let mut about = serde_json::to_value(&recording.about).map_err(|_| zu::Error::Json)?;
    about["event"] = json!(filing.event);
    let Value::Map(mut head) = value(about)? else { return Err(zu::Error::Json) };
    head.insert("audio".into(), Value::List(audio.iter().map(part).collect()));
    let mut bodies = vec![(RECORDING, filing.key.clone(), Value::Map(head))];
    for (own, said) in &recording.lines {
        let said = serde_json::to_value(said).map_err(|_| zu::Error::Json)?;
        bodies.push((LINE, key(own), value(said)?));
    }
    for (label, name) in &recording.speakers {
        let own = format!("{}.{label}", recording.id);
        bodies.push((SPEAKER, key(&own), value(json!({ "text": name }))?));
    }
    let mut held = Held::default();
    for (n, (ty, key, body)) in (0..).zip(bodies) {
        let (path, cid) = (Path::new(&me.did, ty, &key)?, Cid::of(Codec::Drisl, &body.encode()));
        held.records.push(Record::sign(&me.key, path, Some(cid), [], at + n, Some(&filing.did))?);
        held.bodies.insert(cid, body);
    }
    Ok(held)
}

/// A host's answer if it is a success, else what its status means here.
fn status(reply: Response) -> Result<Response, End> {
    match reply.status() {
        status if status.is_success() => Ok(reply),
        StatusCode::NOT_FOUND => Err(End::Fatal("city.unknown")), // no Zu host there
        StatusCode::PAYLOAD_TOO_LARGE => Err(End::Fatal("city.big")),
        status if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
            Err(End::Retry("city.busy"))
        }
        _ => Err(End::Fatal("city.deny")),
    }
}

async fn get(http: &Client, url: &str) -> Result<Vec<u8>, End> {
    let reply = status(http.get(url).send().await.map_err(|_| End::Retry("city.unreachable"))?)?;
    Ok(reply.bytes().await.map_err(|_| End::Retry("city.unreachable"))?.to_vec())
}

/// The calendar of the city at `address`: the DID its domain confirms (map.md § HTTPS
/// well-known Method), and the events its host holds that are meant for it. An event without
/// its body or a title cannot be chosen, and is left out.
pub async fn calendar(http: &Client, address: &str) -> Result<Calendar, End> {
    let origin = origin(address).map_err(End::Fatal)?;
    let did = get(http, &format!("{origin}/.well-known/zu-did")).await?;
    let did = String::from_utf8_lossy(&did).trim().to_owned();
    let (mut query, mut events) = (Query::across(EVENT, Some(&did)), Vec::new());
    for _ in 0..50 {
        let url = query.url(&origin).map_err(|_| End::Fatal("city.unknown"))?;
        let (held, next) =
            query.read(&get(http, &url).await?).map_err(|_| End::Retry("city.busy"))?;
        for record in &held.records {
            let body = record.body.and_then(|cid| held.bodies.get(&cid));
            let Some(Value::Map(body)) = body else { continue };
            let (Some(Value::Str(title)), start) = (body.get("title"), body.get("start")) else {
                continue;
            };
            let start = if let Some(Value::Int(start)) = start { Some(*start) } else { None };
            events.push(CityEvent { path: record.path.to_string(), title: title.clone(), start });
        }
        let Some(next) = next else { break };
        query.from = Some(next);
    }
    events.sort_by_key(|event| event.start);
    Ok(Calendar { address: address.trim().into(), did, events })
}

/// A recording about to be filed to `event` in `calendar`, kept before anything goes, so that
/// it goes on after a restart. Filed to that city before, it keeps its key there, and what goes
/// now replaces what went then.
pub fn begin(
    shelf: &Shelf,
    id: &str,
    calendar: &Calendar,
    event: &str,
    audio: bool,
) -> Result<Filing, &'static str> {
    let event = calendar.events.iter().find(|e| e.path == event).ok_or("city.event.gone")?;
    let mut before = shelf.kept::<Filing>(FILING).map_err(|_| "shelf")?.into_iter();
    let before = before.find(|(of, before)| of == id && before.did == calendar.did);
    let key = match before {
        Some((_, before)) => before.key,
        None => keys::fresh().map_err(|_| "random")?,
    };
    let (city, did) = (calendar.address.clone(), calendar.did.clone());
    let (path, title, state) = (event.path.clone(), event.title.clone(), String::new());
    let filing = Filing { city, did, event: path, title, audio, key, state };
    let kept = shelf.put("", FILING, &[(id.to_owned(), Some(filing.clone()))], wall_ms() as i64);
    kept.map(|()| filing).map_err(|_| "shelf")
}

/// What a filing needs: the network, who signs, their records, and where audio is kept.
pub struct Filer {
    pub http: Client,
    pub me: Me,
    pub shelf: Arc<Shelf>,
    pub tapes: PathBuf,
}

impl Filer {
    /// Where a recording's audio is, and its key.
    fn tape(&self, id: &str) -> Result<(PathBuf, [u8; 32]), End> {
        Ok((self.tapes.join(id), keys::derive("tape", id)?))
    }
}

/// Background-gate and observability context passed by `Work` when filing on the background lane.
#[derive(Clone, Copy)]
pub(crate) struct FileGate<'a> {
    pub bg: &'a Arc<Semaphore>,
    pub seq: u64,
    pub jobs_done: &'a Counter,
    pub jobs_failed: &'a Counter,
}

/// Files recording `id` as `filing` says, until the city holds all of it or refuses it for good;
/// what may pass is tried again, ever more patiently. The records go first, then the audio's
/// files, so a refusal of the audio says that the text is there. How it went is one more
/// record, and is returned as its code. `tell` hears each step.
pub async fn file(
    tree: &Tree,
    filer: &Filer,
    id: &str,
    filing: Filing,
    tell: impl Fn(Writing) + Sync,
) -> &'static str {
    file_inner(tree, filer, id, filing, None, tell).await
}

/// Like [`file`], acquiring the shared background semaphore per attempt (never held across retry
/// sleeps) and emitting `job.start` / `job.done` per attempt with the same `seq`.
pub(crate) async fn file_job(
    tree: &Tree,
    filer: &Filer,
    id: &str,
    filing: Filing,
    gate: FileGate<'_>,
    tell: impl Fn(Writing) + Sync,
) -> &'static str {
    file_inner(tree, filer, id, filing, Some(gate), tell).await
}

async fn file_inner(
    tree: &Tree,
    filer: &Filer,
    id: &str,
    mut filing: Filing,
    gate: Option<FileGate<'_>>,
    tell: impl Fn(Writing) + Sync,
) -> &'static str {
    let (signed, sent) = (OnceCell::new(), AtomicUsize::new(0));
    let step = |step: &str, done: usize, of: usize| {
        tell(Writing { id: id.into(), step: step.into(), done: done as u32, of: of as u32 })
    };
    let slot: Mutex<Option<OwnedSemaphorePermit>> = Mutex::new(None);
    let (signed, sent, step, wanted, slot_ref) = (&signed, &sent, &step, &filing, &slot);
    let filed = again(
        move || async move {
            let held_permit = if let Some(gate) = gate {
                let wait_at = tokio::time::Instant::now();
                let Ok(permit) = gate.bg.clone().acquire_owned().await else {
                    return Err(End::Done);
                };
                let waited_ms = wait_at.elapsed().as_millis() as u64;
                info!("job.start", kind = "file", job = gate.seq, waited_ms = waited_ms);
                Some((permit, std::time::Instant::now()))
            } else {
                None
            };
            let attempt = async {
                let origin = origin(&wanted.city).map_err(End::Fatal)?;
                let (held, named) = signed
                    .get_or_try_init(|| async {
                        step("filing", 0, 0);
                        present(&filer.http, &origin, &wanted.event).await?;
                        sign(tree, filer, id, wanted).await
                    })
                    .await?;
                deliver(&filer.http, &origin, held).await?;
                if named.is_empty() {
                    return Ok(()); // no audio goes
                }
                let tell = |n| step("audio", n, named.len());
                upload(tree, &filer.http, &origin, filer.tape(id)?, named, sent, &tell).await
            };
            let attempt = attempt.await;
            if let Err(End::Retry(code)) = attempt {
                step(code, 0, 0); // what it waits for
            }
            if let (Some(gate), Some((permit, started))) = (gate, held_permit) {
                let ms = started.elapsed().as_millis() as u64;
                match &attempt {
                    Ok(()) => {
                        info!("job.done", kind = "file", job = gate.seq, code = "ok", ms = ms);
                        gate.jobs_done.add(1);
                    }
                    Err(end) => {
                        let c = match *end {
                            End::Retry(c) | End::Fatal(c) => c,
                            End::Done => "done",
                        };
                        warn!("job.done", kind = "file", job = gate.seq, code = c, ms = ms);
                        gate.jobs_failed.add(1);
                    }
                }
                if matches!(attempt, Err(End::Retry(_))) {
                    *slot_ref.lock().unwrap_or_else(PoisonError::into_inner) = Some(permit);
                }
            }
            attempt
        },
        |why, wait_ms, n| {
            warn!("job.again", kind = "file", why = why, wait_ms = wait_ms, n = n);
            let _ = slot_ref.lock().unwrap_or_else(PoisonError::into_inner).take(); // dropped: release gate before sleeping
        },
    );
    let said = match filed.await {
        Ok(()) | Err(End::Done) => "city.filed",
        Err(End::Retry(code) | End::Fatal(code)) => code,
    };
    // Audio went only if there was some to go.
    filing.audio &= signed.get().is_none_or(|(_, named)| !named.is_empty());
    filing.state = said.into();
    let kept = filer.shelf.put("", FILING, &[(id.to_owned(), Some(filing))], wall_ms() as i64);
    if kept.is_ok() { said } else { "shelf" }
}

/// Whether the event at `path` is still in the calendar: its record is there, not removed.
async fn present(http: &Client, origin: &str, path: &str) -> Result<(), End> {
    let query = Query::one(&path.parse().map_err(|_| End::Fatal("city.event.gone"))?);
    let url = query.url(origin).map_err(|_| End::Fatal("city.event.gone"))?;
    let (held, _) = query.read(&get(http, &url).await?).map_err(|_| End::Retry("city.busy"))?;
    let there = held.records.iter().any(|record| record.body.is_some());
    if there { Ok(()) } else { Err(End::Fatal("city.event.gone")) }
}

/// The records that go, signed once for all attempts so that each gives the same, and the
/// audio's parts as they are named in them.
async fn sign(
    tree: &Tree,
    filer: &Filer,
    id: &str,
    filing: &Filing,
) -> Result<(Held, Vec<(u32, Blob)>), End> {
    let recording = filer.shelf.read(id).map_err(|_| End::Fatal("shelf"))?;
    let recording = recording.ok_or(End::Fatal("recording.gone"))?;
    // Audio destroyed since it was asked for, by the person or by their choice of how long it
    // stays, leaves the text to go alone.
    let tape = filer.tape(id)?;
    let named =
        if filing.audio && tape::kept(&tape.0) { named(tree, tape).await? } else { Vec::new() };
    let held = records(&filer.me, filing, &recording, &named, wall_ms() as i64);
    Ok((held.map_err(|_| End::Fatal("id.sign"))?, named))
}

/// Gives the records to the host at `origin`, with their bodies, a batch at a time (record.md
/// § Delivering), and says what the host made of them.
async fn deliver(http: &Client, origin: &str, held: &Held) -> Result<(), End> {
    let url = Query::default().url(origin).map_err(|_| End::Fatal("city.address"))?;
    for batch in held.records.chunks(BATCH) {
        let body = |record: &Record| Some((record.body?, held.bodies.get(&record.body?)?.clone()));
        let batch_held =
            Held { records: batch.to_vec(), bodies: batch.iter().filter_map(body).collect() };
        let sent = http.post(&url).header(CONTENT_TYPE, "application/cbor");
        let sent = sent.body(zu::query::answer(&batch_held, None)).send().await;
        let reply = status(sent.map_err(|_| End::Retry("city.unreachable"))?)?;
        let said = reply.bytes().await.map_err(|_| End::Retry("city.unreachable"))?;
        let Ok(Value::Map(mut said)) = Value::decode(&said) else {
            return Err(End::Retry("city.busy"));
        };
        let Some(Value::Map(stat)) = said.remove("stat") else {
            return Err(End::Retry("city.busy"));
        };
        for record in batch {
            let said = stat.get(&record.cid().to_string());
            let word = WORDS.iter().find(|(w, _)| matches!(said, Some(Value::Str(s)) if s == w));
            match word.map_or(End::Fatal("city.deny"), |(_, end)| *end) {
                End::Done => {}
                end => return Err(end),
            }
        }
    }
    Ok(())
}

/// What stops the audio once the text is there says so.
fn audio(end: End) -> End {
    match end {
        End::Fatal("city.big") => End::Fatal("city.audio.big"),
        End::Fatal(_) => End::Fatal("city.audio.deny"),
        end => end,
    }
}

/// The recording's audio as WAV files of `PART` each, read on a supervised blocking task as they
/// are wanted, each with where it starts and how it is named: one part in memory at a time,
/// none on disk in the clear.
fn parts(
    tree: &Tree,
    (dir, key): (PathBuf, [u8; 32]),
) -> queue::Rx<io::Result<(u32, Blob, Vec<u8>)>> {
    let (tx, rx) = queue::bounded(tree.stats(), "city.audio", CITY_QUEUE_PARTS, Full::Wait);
    let blocking_tree = tree.clone();
    tree.spawn("city.audio", async move {
        let tx_block = tx.clone();
        let outcome = blocking_tree
            .blocking("city.audio", move || {
                let mut part = (0, Vec::new());
                // A send fails only once nobody is listening any more.
                let send = |(t0, pcm): (u32, Vec<u8>)| {
                    let wav = [wav_header(pcm.len() as u32 / 2), pcm].concat();
                    let file = Blob {
                        cid: Cid::of(Codec::Raw, &wav),
                        mime: "audio/wav".into(),
                        size: wav.len() as u64,
                    };
                    let _ = tx_block.send_blocking(Ok((t0, file, wav))); // dropped: send fails only after the filing task stopped listening
                };
                let read = tape::read(&dir, &key, &Pass::City, |block| {
                    if block.t_ms / PART != part.0 / PART {
                        send(std::mem::replace(&mut part, (block.t_ms, Vec::new())));
                    }
                    part.1.extend(block.pcm.iter().flat_map(|sample| sample.to_le_bytes()));
                });
                match read {
                    Ok(0) => {}
                    Ok(_) => send(part),
                    Err(error) => {
                        let _ = tx_block.send_blocking(Err(error)); // dropped: send fails only after the filing task stopped listening
                    }
                }
            })
            .await;
        if outcome.is_err() {
            let _ = tx.send(Err(io::Error::other("tape read broke"))).await; // dropped: send fails only after the filing task stopped listening
        }
    });
    rx
}

/// Each part of the audio, read once to name it before anything goes.
async fn named(tree: &Tree, tape: (PathBuf, [u8; 32])) -> Result<Vec<(u32, Blob)>, End> {
    let (mut parts, mut named) = (parts(tree, tape), Vec::new());
    while let Some(part) = parts.recv().await {
        let (t0, file, _) = part.map_err(|_| End::Fatal("tape"))?;
        named.push((t0, file));
    }
    Ok(named)
}

/// Gives the host each part it does not have yet (blob.md § Uploading), read again from the
/// tape; `sent` counts the parts it has. `tell` hears how many it has.
async fn upload(
    tree: &Tree,
    http: &Client,
    origin: &str,
    tape: (PathBuf, [u8; 32]),
    named: &[(u32, Blob)],
    sent: &AtomicUsize,
    tell: &(dyn Fn(usize) + Sync),
) -> Result<(), End> {
    let mut parts = parts(tree, tape);
    for (n, (_, file)) in named.iter().enumerate() {
        let part = parts.recv().await.ok_or(End::Fatal("tape"))?;
        let (_, read, wav) =
            part.ok().filter(|(_, read, _)| read == file).ok_or(End::Fatal("tape"))?;
        if n >= sent.load(Relaxed) {
            let url = zu::blob::upload(origin, &read.cid).map_err(|_| End::Fatal("tape"))?;
            let put = http.put(url).header(CONTENT_TYPE, "application/octet-stream").body(wav);
            status(put.send().await.map_err(|_| End::Retry("city.unreachable"))?).map_err(audio)?;
            sent.store(n + 1, Relaxed);
        }
        tell(n + 1);
    }
    Ok(())
}
