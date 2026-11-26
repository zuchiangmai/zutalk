//! Timestamped, optionally line-anchored notes (`talk/note`) kept inside a recording's
//! `about.jsonl` pack (`P2.md` §3.3.1, `DECISIONS.md` #2).
//!
//! Backward-compatible with legacy single-string notes stored at key `<id>` with body
//! `Words { text }`: `NoteBody` defaults `at` to `0` and `anchor` to `None`.

use crate::record::shelf::{NOTE, Shelf};
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use ts_rs::TS;
use zu::{Error, Held, Pack, Record, record};

/// Record body for `talk/note`. Empty `text` on a timestamped key marks a moment (`DECISIONS.md` #2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteBody {
    pub at: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

/// One resolved note read from a recording's `about.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Note {
    pub key: String,
    pub at_ms: u32,
    pub text: String,
    pub anchor: Option<String>,
}

/// Builds a lexicographically time-ordered note key `<id>.<at_ms:010>.<rand:08x>`.
pub fn make_key(id: &str, at_ms: u32, rand: u32) -> String {
    format!("{id}.{at_ms:010}.{rand:08x}")
}

/// Whether `key` belongs to recording `id` (either the legacy `<id>` key or `<id>.*`).
pub fn belongs_to(id: &str, key: &str) -> bool {
    key == id || key.strip_prefix(id).is_some_and(|rest| rest.starts_with('.'))
}

/// Parses current `talk/note` records for `id` out of an already-loaded `about.jsonl` [`Held`].
/// Returns `(legacy_single_string, sorted_notes)` so `Shelf::about` avoids reading the pack twice.
pub(crate) fn from_held(id: &str, held: &Held) -> Result<(String, Vec<Note>), Error> {
    let mut legacy = String::new();
    let mut notes = Vec::new();
    for (path, current) in record::paths(&held.records) {
        if path.ty != NOTE || !belongs_to(id, &path.key) {
            continue;
        }
        let Some(body) = decode_body(held, &current)? else {
            continue;
        };
        if path.key == id {
            legacy = body.text.clone();
            if body.text.is_empty() {
                continue;
            }
        }
        notes.push(Note {
            key: path.key.clone(),
            at_ms: body.at,
            text: body.text,
            anchor: body.anchor,
        });
    }
    notes.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.key.cmp(&b.key)));
    Ok((legacy, notes))
}

fn decode_body(held: &Held, current: &[&Record]) -> Result<Option<NoteBody>, Error> {
    let Some(body) = current.first().and_then(|r| r.body).and_then(|cid| held.bodies.get(&cid))
    else {
        return Ok(None);
    };
    serde_json::from_str(&body.to_json()?).map(Some).map_err(|_| Error::Json)
}

/// Reads all live notes for recording `id`, sorted by `(at_ms, key)`.
pub fn read(shelf: &Shelf, id: &str) -> Result<Vec<Note>, Error> {
    let held = match Pack::read(shelf.pack_path(id, NOTE)) {
        Err(Error::Io(ErrorKind::NotFound)) => Held::default(),
        res => res?,
    };
    Ok(from_held(id, &held)?.1)
}

/// Appends a new timestamped note (or bare mark when `text` is empty) to recording `id`.
pub fn add(
    shelf: &Shelf,
    id: &str,
    at_ms: u32,
    text: String,
    anchor: Option<String>,
    rand: u32,
    now: i64,
) -> Result<Note, Error> {
    let key = make_key(id, at_ms, rand);
    let body = NoteBody { at: at_ms, text: text.clone(), anchor: anchor.clone() };
    shelf.put(id, NOTE, &[(key.clone(), Some(body))], now)?;
    Ok(Note { key, at_ms, text, anchor })
}

/// Replaces the `text` of an existing note `key` on recording `id`, keeping its `at_ms` and `anchor`.
pub fn edit(
    shelf: &Shelf,
    id: &str,
    key: &str,
    text: String,
    now: i64,
) -> Result<Note, &'static str> {
    if !belongs_to(id, key) {
        return Err("note.gone");
    }
    let notes = read(shelf, id).map_err(|_| "shelf")?;
    let existing = notes.into_iter().find(|n| n.key == key).ok_or("note.gone")?;
    let body = NoteBody { at: existing.at_ms, text: text.clone(), anchor: existing.anchor.clone() };
    shelf.put(id, NOTE, &[(key.to_string(), Some(body))], now).map_err(|_| "shelf")?;
    Ok(Note { key: key.to_string(), at_ms: existing.at_ms, text, anchor: existing.anchor })
}

/// Tombstones an existing note `key` on recording `id`.
pub fn remove(shelf: &Shelf, id: &str, key: &str, now: i64) -> Result<(), &'static str> {
    if !belongs_to(id, key) {
        return Err("note.gone");
    }
    let notes = read(shelf, id).map_err(|_| "shelf")?;
    if !notes.iter().any(|n| n.key == key) {
        return Err("note.gone");
    }
    shelf.put::<NoteBody>(id, NOTE, &[(key.to_string(), None)], now).map_err(|_| "shelf")?;
    Ok(())
}
