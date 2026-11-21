//! Rooms: the one container for what leaves a device, whether live captions or a recording for
//! whoever has the link. An append-only log of frames, and a slot that holds only the latest.
//! Frames are sealed by their writer; here they are bytes. Each write is a `talk/room` record
//! its owner signed for this server, now: the room's file is the pack of the records that made
//! it and filled its log, so a write counts once and the room outlives a restart. The slot is
//! never on the disk. Whoever knows a room's id reads it, which without the key is no use.

use crate::{Answer, Delivery, Refusal, Server, field, lock, now};
use axum::extract::{Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, convert::Infallible, fs, sync::Arc};
use tokio::sync::broadcast;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};
use zu::{Cid, Pack, Record, Value};

const NO_ROOM: Refusal = Refusal(StatusCode::NOT_FOUND, "room");

pub struct Room {
    pack: Pack,
    owner: String,
    kept: Vec<Cid>,      // the records that made it and filled its log
    frames: Vec<String>, // in base64, the form they go out in
    bytes: usize,
    slot: (i64, String), // the latest frame, and the time of the write that put it there
    last: i64,           // the last write, for the sweep
    live: broadcast::Sender<Event>,
}

/// An event always has data, or a browser drops it: `-` stands for none.
fn event(name: &str, data: &str) -> Event {
    Event::default().event(name).data(if data.is_empty() { "-" } else { data })
}

/// A reader this far behind is hung up on. It comes back with the last id it saw.
fn channel() -> broadcast::Sender<Event> {
    broadcast::channel(256).0
}

impl Room {
    /// What the pack at `path` holds, as it was left. A restart counts as a write: a room is
    /// not swept for the time the server was not watching.
    fn open(path: &std::path::Path) -> Result<Room, zu::Error> {
        let (pack, held) = Pack::open(path)?;
        let owner = held.records.first().map(|record| record.path.by.clone()).unwrap_or_default();
        let (kept, frames, slot) = (Vec::new(), Vec::new(), (0, String::new()));
        let mut room =
            Room { pack, owner, kept, frames, bytes: 0, slot, last: now(), live: channel() };
        for record in held.records {
            let body = record.body.and_then(|cid| held.bodies.get(&cid));
            if field(body, "do").is_some_and(|what| what == "log") {
                room.add(field(body, "frame").cloned().unwrap_or_default());
            }
            room.kept.push(record.cid());
        }
        Ok(room)
    }

    /// A record and its body, on the disk before they count.
    fn keep(&mut self, record: &Record, body: &Value) -> Result<(), zu::Error> {
        self.pack.keep(body)?;
        self.pack.append(record)?;
        self.pack.sync()?;
        self.kept.push(record.cid());
        Ok(())
    }

    /// Whoever is reading hears each frame; an error to send is only nobody listening.
    fn add(&mut self, frame: String) {
        self.bytes += frame.len();
        _ = self.live.send(event("log", &frame).id((self.frames.len() + 1).to_string()));
        self.frames.push(frame);
    }
}

/// The rooms on disk, as they were left.
pub fn open_all(dir: &std::path::Path) -> Result<HashMap<String, Room>, zu::Error> {
    let mut rooms = HashMap::new();
    for file in fs::read_dir(dir)? {
        let path = file?.path();
        let id = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_owned();
        rooms.insert(id, Room::open(&path)?);
    }
    Ok(rooms)
}

/// Takes a room and its file away, and tells whoever is reading.
fn remove(server: &Server, rooms: &mut HashMap<String, Room>, id: &str) -> std::io::Result<()> {
    fs::remove_file(server.config.data.join("rooms").join(id))?;
    if let Some(room) = rooms.remove(id) {
        _ = room.live.send(event("gone", ""));
    }
    Ok(())
}

/// Removes the rooms not written to for longer than the operator keeps them. A file that
/// does not go is tried again at the next sweep.
pub fn sweep(server: &Server, now: i64) {
    let mut rooms = lock(&server.rooms);
    let idle = rooms.iter().filter(|(_, room)| now > room.last + server.config.room.3);
    for id in idle.map(|(id, _)| id.clone()).collect::<Vec<_>>() {
        _ = remove(server, &mut rooms, &id);
    }
}

/// `POST /rooms`: a `talk/room` record `{ room, do, frame }`. `make` makes a room of the
/// signer's; then only they write to it: `log` adds a frame for good, `slot` puts one in place
/// of the last (empty: none), `gone` takes the room away. A write counts once, and a slot is
/// never put back to an older frame.
pub async fn write(State(server): State<Arc<Server>>, Json(sent): Json<Delivery>) -> Answer {
    let (record, body) = server.check(sent, "talk/room", None).await?;
    let body = body.unwrap_or(Value::Null);
    let said = |name| field(Some(&body), name).cloned().unwrap_or_default();
    let (id, what, frame) = (said("room"), said("do"), said("frame"));
    if id.len() != 32 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Refusal(StatusCode::BAD_REQUEST, "room"));
    }
    let (did, (most_bytes, most_rooms, ..)) = (record.path.by.clone(), server.config.room);
    let mut rooms = lock(&server.rooms);
    if what == "make" {
        if rooms.contains_key(&id) {
            return Err(Refusal(StatusCode::CONFLICT, "taken"));
        } else if rooms.values().filter(|room| room.owner == did).count() >= most_rooms {
            return Err(Refusal(StatusCode::TOO_MANY_REQUESTS, "rooms"));
        }
        let mut room = Room::open(&server.config.data.join("rooms").join(&id))?;
        room.owner = did;
        room.keep(&record, &body)?;
        rooms.insert(id, room);
        return Ok(Json(json!({ "viewers": 0 })));
    }
    let room = rooms.get_mut(&id).ok_or(NO_ROOM)?;
    if room.owner != did {
        return Err(Refusal(StatusCode::FORBIDDEN, "owner"));
    } else if room.kept.contains(&record.cid()) || (what == "slot" && record.at <= room.slot.0) {
        return Err(Refusal(StatusCode::CONFLICT, "replay"));
    }
    room.last = now();
    match what.as_str() {
        "log" if frame.is_empty() => return Err(Refusal(StatusCode::BAD_REQUEST, "frame")),
        "log" if room.bytes + frame.len() > most_bytes => {
            return Err(Refusal(StatusCode::PAYLOAD_TOO_LARGE, "full"));
        }
        "log" => {
            room.keep(&record, &body)?;
            room.add(frame);
        }
        "slot" => {
            room.slot = (record.at, frame);
            _ = room.live.send(event("slot", &room.slot.1));
        }
        "gone" => {
            remove(&server, &mut rooms, &id).map_err(zu::Error::from)?;
            return Ok(Json(json!({})));
        }
        _ => return Err(Refusal(StatusCode::BAD_REQUEST, "do")),
    }
    Ok(Json(json!({ "viewers": room.live.receiver_count() })))
}

#[derive(Deserialize)]
pub struct After {
    #[serde(default)]
    after: usize,
}

/// `GET /rooms/{id}/events?after=n`: every frame of the log after the `n`th, its index as the
/// event id, so that `Last-Event-ID` takes up where a connection broke; then the slot, and
/// whatever is written from then on; `gone` at the end.
pub async fn events(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    Query(from): Query<After>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Refusal> {
    let last = headers.get("last-event-id").and_then(|id| id.to_str().ok()?.parse().ok());
    let rooms = lock(&server.rooms);
    let room = rooms.get(&id).ok_or(NO_ROOM)?;
    if room.live.receiver_count() >= server.config.room.2 {
        return Err(Refusal(StatusCode::TOO_MANY_REQUESTS, "viewers"));
    }
    let frames = room.frames.iter().enumerate().skip(last.unwrap_or(from.after));
    let mut past: Vec<Event> =
        frames.map(|(i, f)| event("log", f).id((i + 1).to_string())).collect();
    past.push(event("slot", &room.slot.1));
    // Subscribed under the lock writers take: nothing falls between the past and the live.
    let live = BroadcastStream::new(room.live.subscribe()).map_while(Result::ok);
    Ok(Sse::new(tokio_stream::iter(past).chain(live).map(Ok)).keep_alive(KeepAlive::default()))
}
