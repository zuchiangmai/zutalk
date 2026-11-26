//! The 0.8 library, moved in once and on its own. Only the 0.8 app can read its database and
//! its documents, so its last update hands each recording over as the bodies of the records it
//! becomes, unsigned, and says where its sealed audio is (docs/migrate.md). Here the person
//! signs them, and the audio, opened with its 0.8 key, is sealed again on the tape.
//!
//! A recording moves whole or not at all: its `talk/recording` record is written last, and its
//! id here is the same every time. So a move cut short is done again from the start, and one
//! that finished, even if the recording was removed since, is never done twice.

use crate::capture::audio::Resampler;
use crate::capture::clock::{BLOCK, Clock, Gap, RATE};
use crate::capture::tape;
use crate::id::keys;
use crate::record::shelf::{self, About, LINE, NOTE, RECORDING, SPEAKER, Said, Shelf, Words};
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use zeroize::{Zeroize, Zeroizing};
use zutalk_log::{info, warn};

/// The handover this version reads.
const VERSION: u32 = 1;
/// Executable name of the optional standalone 0.8 handover helper bundled with macOS builds.
pub const HANDOVER_HELPER_NAME: &str = "zutalk-handover-08";

/// 12-hour cap per migrated recording prevents corrupt `t_ms` values from triggering OOM (`P7.md` §3.2).
pub const MIGRATE_MAX_AUDIO_MS: u64 = 12 * 60 * 60 * 1_000;
/// 10 s at 16 kHz: stream silence gaps in bounded chunks instead of one giant `Vec` (`P7.md` §3.2).
const MIGRATE_SILENCE_CHUNK_SAMPLES: usize = 160_000;
/// Bounds `State.unmoved` under I8 even when many handover entries fail (`P7.md` §3.2).
pub const STATE_UNMOVED_MAX_ROWS: usize = 32;

/// Pure check: a handover recording name must be a single relative path component with no slashes.
pub fn safe_recording_name(name: &str) -> bool {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return false;
    }
    let mut comps = Path::new(name).components();
    matches!((comps.next(), comps.next()), (Some(Component::Normal(_)), None))
}

/// Pure check: a handover audio chunk path must stay strictly inside `old` with normal segments.
pub fn safe_chunk_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.ends_with('/') {
        return false;
    }
    if path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
        return false;
    }
    let mut comps = Path::new(path).components();
    let Some(first) = comps.next() else {
        return false;
    };
    matches!(first, Component::Normal(_)) && comps.all(|c| matches!(c, Component::Normal(_)))
}

/// Bounds the failure list placed into `State.unmoved` so small-state size stays O(1) (`I8`).
pub fn cap_unmoved(mut skipped: Vec<(String, String)>) -> Vec<(String, String)> {
    skipped.truncate(STATE_UNMOVED_MAX_ROWS);
    skipped
}

/// Summary returned by [`run_report`] so callers can log counts without leaking titles or ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveReport {
    pub moved: u32,
    pub already: u32,
    pub skipped: Vec<(String, String)>,
}

/// What one stored sample is worth.
type Sample = fn(&[u8]) -> f32;

/// How 0.8 stored a sample: by the name its handover gives, its size, and what it is worth.
#[rustfmt::skip]
const FORMATS: [(&str, usize, Sample); 2] = [
    ("s16", 2, |b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0),
    ("f32", 4, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
];

/// The recordings handed over, in the order 0.8 listed them. Written last, so it is whole.
#[derive(Deserialize)]
struct Index {
    version: u32,
    recordings: Vec<String>,
}

/// One recording as 0.8 handed it over: what it is, its takes (the live text first, then the
/// whole-session transcript if it had one), its speakers' names, its note, and its audio.
#[derive(Debug, Clone, Deserialize)]
pub struct Handed {
    pub version: u32,
    pub id: String,
    pub about: About,
    #[serde(default)]
    pub takes: Vec<Vec<Said>>,
    #[serde(default)]
    pub speakers: BTreeMap<String, String>,
    #[serde(default)]
    pub note: String,
    pub audio: Option<Audio>,
}

/// Where a recording's audio is, under the 0.8 folder, and how it was stored. None: 0.8 had
/// no audio for it any more.
#[derive(Debug, Clone, Deserialize)]
pub struct Audio {
    pub key: String, // its name in the 0.8 key file
    pub rate: u32,
    pub channels: u16,
    pub format: String,
    pub chunks: Vec<Chunk>, // in order; a stretch between two is silence
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chunk {
    pub path: String,
    pub t_ms: u32, // where it starts on the recording's clock
}

impl Handed {
    /// Its id here: shaped like one made here, and the same every time it is moved.
    pub fn id(&self) -> String {
        let tag = Sha256::digest(self.id.as_bytes());
        let tag = u32::from_be_bytes([tag[0], tag[1], tag[2], tag[3]]);
        format!("{:011x}{tag:08x}", self.about.started.max(0))
    }

    /// The lines of every take, keyed as the takes written here are.
    pub fn lines(&self, id: &str) -> Vec<(String, Option<Said>)> {
        let takes = self.takes.iter().enumerate();
        let keyed = takes.flat_map(|(take, said)| {
            said.iter().enumerate().map(move |(n, said)| (format!("{id}.{take}.{n:05}"), said))
        });
        keyed.map(|(key, said)| (key, Some(said.clone()))).collect()
    }

    /// What it is: its text the last take, its length its tape's when it has one. Nothing
    /// writes a transcript for it on its own: 0.8 wrote one when it was asked to.
    pub fn about(&self, length_ms: Option<u32>) -> About {
        let take = self.takes.len().saturating_sub(1);
        let preview = shelf::preview(self.takes.get(take).map_or(&[], Vec::as_slice));
        let length_ms = length_ms.unwrap_or(self.about.length_ms);
        About { take: take as u32, preview, length_ms, moved: true, ..self.about.clone() }
    }
}

/// Who the 0.8 library went to: the first person signed in here once it was handed over.
#[derive(Serialize, Deserialize)]
struct Owner {
    to: String,
}

/// One progress report while moving recordings from ZuTalk 0.8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub id: String,
    pub step: &'static str,
    pub done: u32,
    pub of: u32,
}

/// Atomically writes `<data>/moved.json` via temporary sibling file + `sync_all` + `rename`.
fn write_owner(data: &Path, me: &str) -> Result<(), &'static str> {
    fs::create_dir_all(data).map_err(|_| "shelf")?;
    let json = serde_json::to_vec(&Owner { to: me.into() }).map_err(|_| "shelf")?;
    let tmp = data.join("moved.json.tmp");
    let mut file = fs::File::create(&tmp).map_err(|_| "shelf")?;
    file.write_all(&json).and_then(|()| file.sync_all()).map_err(|_| "shelf")?;
    fs::rename(tmp, data.join("moved.json")).map_err(|_| "shelf")
}

/// If `old` contains a 0.8 database without `handover/index.json` (e.g. a direct one-hop upgrade
/// from 0.8.5 or 0.8.6) and a bundled `zutalk-handover-08` helper binary is available, invokes the
/// helper once to materialize `handover/` non-destructively before reading it.
fn ensure_handover(old: &Path, helper: Option<&Path>) -> Result<(), &'static str> {
    let index_path = old.join("handover").join("index.json");
    if index_path.is_file() || !old.join("zutalk.db").exists() {
        return Ok(());
    }
    let Some(bin) = helper.filter(|path| path.is_file()) else {
        return Err("migrate.missing");
    };
    let status = Command::new(bin)
        .arg(old)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(exit) if exit.success() && index_path.is_file() => {
            info!("migrate.helper.done", code = "ok");
            Ok(())
        }
        Ok(exit) => {
            let code = exit.code().unwrap_or(-1);
            warn!("migrate.helper.done", code = "migrate.missing", exit = code);
            Err("migrate.missing")
        }
        Err(_) => {
            warn!("migrate.helper.done", code = "migrate.missing", exit = -1i32);
            Err("migrate.missing")
        }
    }
}

/// Moves into `shelf`, the shelf of `me`, each recording 0.8 handed over in `old` that is not
/// here yet, telling `progress` of each. When `helper` is provided and `old` has a 0.8 database
/// without `handover/index.json`, runs `helper` first so 0.8.5/0.8.6 users migrate in one hop.
pub fn run_report_with_helper(
    (old, data): (&Path, &Path),
    helper: Option<&Path>,
    shelf: &Shelf,
    me: &str,
    now: impl Fn() -> i64,
    mut progress: impl FnMut(Progress),
) -> Result<MoveReport, &'static str> {
    ensure_handover(old, helper)?;
    let handover = old.join("handover");
    let index = match fs::read(handover.join("index.json")) {
        // A library that was never handed over is said; none at all is nothing to move.
        Err(e) if e.kind() == ErrorKind::NotFound && old.join("zutalk.db").exists() => {
            return Err("migrate.missing");
        }
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(MoveReport::default()),
        read => read.map_err(|_| "migrate.handover")?,
    };
    let index: Index = serde_json::from_slice(&index).map_err(|_| "migrate.handover")?;
    if index.version != VERSION {
        return Err("migrate.handover");
    }
    // It goes to one person: whoever signs in here later finds nothing to move.
    let owner = data.join("moved.json");
    match fs::read(&owner).map(|json| serde_json::from_slice::<Owner>(&json)) {
        Ok(Ok(owner)) if owner.to != me => return Ok(MoveReport::default()),
        Ok(Ok(_)) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => write_owner(data, me)?,
        _ => return Err("shelf"),
    }
    let of = index.recordings.len() as u32;
    let mut report = MoveReport::default();
    for (done, name) in index.recordings.iter().enumerate() {
        if !safe_recording_name(name) {
            report.skipped.push((String::new(), "migrate.handover".into()));
            continue;
        }
        let file = handover.join("recordings").join(format!("{name}.json"));
        let handed = fs::read(file).ok().and_then(|json| serde_json::from_slice(&json).ok());
        let Some(handed) = handed.filter(|handed: &Handed| handed.version == VERSION) else {
            report.skipped.push((String::new(), "migrate.handover".into()));
            continue;
        };
        let id = handed.id();
        if shelf.known(&id).map_err(|_| "shelf")? {
            report.already += 1;
            continue;
        }
        progress(Progress { id: id.clone(), step: "moving", done: done as u32, of });
        match shelve((old, data), shelf, &handed, &id, &now) {
            Ok(()) => report.moved += 1,
            Err(code) => {
                // Its sound stays where 0.8 has it; a tape of it here would belong to nothing.
                // One left behind is cleared by the next attempt, before anything else.
                let _ = fs::remove_dir_all(data.join("tape").join(&id)); // dropped: if removal fails, the next migration attempt clears it first
                report.skipped.push((handed.about.title.clone(), code.into()));
            }
        }
    }
    Ok(report)
}

/// Moves into `shelf`, the shelf of `me`, each recording 0.8 handed over in `old` that is not
/// here yet, telling `progress` of each. Returns counts plus those it could not move, each by
/// its title there and a code. `Err`: none could be.
pub fn run_report(
    (old, data): (&Path, &Path),
    shelf: &Shelf,
    me: &str,
    now: impl Fn() -> i64,
    progress: impl FnMut(Progress),
) -> Result<MoveReport, &'static str> {
    run_report_with_helper((old, data), None, shelf, me, now, progress)
}

/// Moves into `shelf`, the shelf of `me`, each recording 0.8 handed over in `old` that is not
/// here yet, telling `progress` of each. Returns those it could not move, each by its title
/// there and a code. `Err`: none could be.
pub fn run(
    (old, data): (&Path, &Path),
    shelf: &Shelf,
    me: &str,
    now: impl Fn() -> i64,
    progress: impl FnMut(Progress),
) -> Result<Vec<(String, String)>, &'static str> {
    run_report((old, data), shelf, me, now, progress).map(|report| report.skipped)
}

/// One recording: its audio sealed on its tape, then its records, the last one last.
fn shelve(
    (old, data): (&Path, &Path),
    shelf: &Shelf,
    handed: &Handed,
    id: &str,
    now: &impl Fn() -> i64,
) -> Result<(), &'static str> {
    // What a move cut short left is not the recording: it is done again from the start.
    let dir = data.join("tape").join(id);
    match fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err("tape"),
        _ => {}
    }
    let length_ms = match &handed.audio {
        Some(audio) => {
            let key = keys::derive("tape", id).map_err(|_| "tape")?;
            Some(reseal(old, audio, &dir, &key)?)
        }
        None => None,
    };
    // A key or a body the records cannot hold is the handover's fault, not the disk's.
    let fail = |e| if let zu::Error::Io(_) = e { "shelf" } else { "migrate.handover" };
    let names = handed
        .speakers
        .iter()
        .map(|(label, name)| (format!("{id}.{label}"), Some(Words { text: name.clone() })));
    let note = Words { text: handed.note.clone() };
    let note = (!note.text.is_empty()).then(|| (id.to_string(), Some(note)));
    shelf.put(id, LINE, &handed.lines(id), now()).map_err(fail)?;
    shelf.put(id, SPEAKER, &names.collect::<Vec<_>>(), now()).map_err(fail)?;
    shelf.put(id, NOTE, &Vec::from_iter(note), now()).map_err(fail)?;
    shelf.put(id, RECORDING, &[(id.into(), Some(handed.about(length_ms)))], now()).map_err(fail)
}

/// The key 0.8 sealed a recording's audio with, from the one file it kept them in.
fn old_key(old: &Path, name: &str) -> Result<Zeroizing<[u8; 32]>, &'static str> {
    #[derive(Deserialize)]
    struct Kept {
        keys: BTreeMap<String, String>, // in hex
    }
    impl Drop for Kept {
        fn drop(&mut self) {
            self.keys.values_mut().for_each(Zeroize::zeroize);
        }
    }
    let json = Zeroizing::new(fs::read(old.join("Secrets/content-keys.json")).ok());
    let kept: Option<Kept> = json.as_deref().and_then(|json| serde_json::from_slice(json).ok());
    let hex = kept.as_ref().and_then(|kept| kept.keys.get(name)).filter(|hex| hex.len() == 64);
    let mut key = Zeroizing::new([0; 32]);
    let pairs = hex.ok_or("migrate.key")?.as_bytes().chunks_exact(2);
    for (byte, pair) in key.iter_mut().zip(pairs) {
        let pair = str::from_utf8(pair).ok().and_then(|pair| u8::from_str_radix(pair, 16).ok());
        *byte = pair.ok_or("migrate.key")?;
    }
    Ok(key)
}

/// Opens each chunk with the 0.8 key and seals it again on the tape at `dir`, 100 ms at a time:
/// the plain audio is nowhere but here, in memory. Returns the tape's length in ms.
fn reseal(old: &Path, audio: &Audio, dir: &Path, key: &[u8; 32]) -> Result<u32, &'static str> {
    if audio.chunks.is_empty() {
        return Err("migrate.audio");
    }
    let cipher = Aes256Gcm::new((&*old_key(old, &audio.key)?).into());
    let format = FORMATS.iter().find(|(name, ..)| *name == audio.format);
    let (&(_, size, value), channels) = (format.ok_or("migrate.audio")?, audio.channels as usize);
    let resampler = Resampler::new(audio.rate).filter(|_| channels > 0);
    let (mut resampler, frame) = (resampler.ok_or("migrate.audio")?, size * channels);
    let (mut tape, mut clock) = (tape::Writer::create(dir, key), Clock::new(|_| Gap::Keep));
    let (mut at, mut blocks, mut made) = (0, 0, Vec::new());
    let (mut pcm, mut mono, mut carry): (Vec<i16>, Vec<f32>, Vec<u8>) = Default::default();
    let mut seal = |pcm: &mut Vec<i16>, at: &mut usize| -> Result<(), &'static str> {
        clock.push(pcm, 0, &mut made);
        (*at, blocks) = (*at + pcm.len(), blocks + made.len() as u32);
        pcm.clear();
        let tape = tape.as_mut().map_err(|_| "tape")?;
        made.drain(..).try_for_each(|block| tape.append(&block)).map_err(|_| "tape")
    };
    for chunk in &audio.chunks {
        if !safe_chunk_path(&chunk.path) || u64::from(chunk.t_ms) > MIGRATE_MAX_AUDIO_MS {
            return Err("migrate.audio");
        }
        let bytes = fs::read(old.join(&chunk.path)).map_err(|_| "migrate.audio")?;
        // A stretch 0.8 no longer had is silence, streamed in bounded chunks to avoid OOM.
        let from = (u64::from(chunk.t_ms) * RATE / 1_000) as usize;
        let mut gap = from.saturating_sub(at);
        while gap > 0 {
            let step = gap.min(MIGRATE_SILENCE_CHUNK_SAMPLES);
            pcm.resize(step, 0);
            seal(&mut pcm, &mut at)?;
            gap -= step;
        }
        let mut rest = bytes.as_slice();
        // Records of a length, then a nonce and what it seals.
        while let Some((length, tail)) = rest.split_first_chunk::<4>() {
            let length = u32::from_le_bytes(*length) as usize;
            let (sealed, tail) = tail.split_at_checked(length).ok_or("migrate.audio")?;
            let (nonce, sealed) = sealed.split_first_chunk::<12>().ok_or("migrate.audio")?;
            let plain = cipher.decrypt(&(*nonce).into(), sealed).map_err(|_| "migrate.audio")?;
            carry.extend(plain);
            let whole = carry.len() / frame * frame;
            let frames = carry[..whole].chunks_exact(frame);
            let sum = |f: &[u8]| f.chunks_exact(size).map(value).sum::<f32>() / channels as f32;
            mono.clear();
            mono.extend(frames.map(sum));
            carry.drain(..whole);
            resampler.push(&mono, &mut pcm);
            seal(&mut pcm, &mut at)?;
            rest = tail;
        }
        if !rest.is_empty() || !carry.is_empty() {
            return Err("migrate.audio"); // a torn record, or half a sample
        }
        seal(&mut pcm, &mut at)?;
    }
    // The last block is filled up with silence rather than dropped.
    pcm.resize(BLOCK - 1, 0);
    seal(&mut pcm, &mut at)?;
    tape.and_then(|mut tape| tape.sync()).map_err(|_| "tape")?;
    Ok(blocks * 100)
}
