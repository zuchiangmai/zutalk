//! Ephemeral slot-only live rooms: pure in-memory ciphertext slots for live audience captions.
//! Frames are sealed by their host; here they are opaque base64 bytes. Each write is a signed
//! `talk/room` record (`make`, `slot`, or `gone`). Nothing touches disk and idle rooms expire
//! automatically after `ROOM_IDLE_TTL_MS` (`PLAN.md` §5.5).

use crate::{Answer, Delivery, Refusal, Server, field, lock, now};
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde_json::json;
use std::{convert::Infallible, sync::Arc};
use tokio::sync::watch;
use tokio_stream::{Stream, StreamExt, wrappers::WatchStream};
use zu::Value;

/// 2-minute inactivity TTL for ephemeral live rooms (`PLAN.md` §5.5).
pub const ROOM_IDLE_TTL_MS: i64 = 120_000;
/// Max base64 slot payload per write, well under the 64 KiB body limit (`PLAN.md` §5.2).
pub const ROOM_MAX_SLOT_BYTES: usize = 32 << 10;
/// Max concurrent live rooms held by one owner.
pub const ROOM_MAX_PER_OWNER: usize = 4;
/// Max simultaneous SSE viewers per room.
pub const ROOM_MAX_VIEWERS: usize = 500;

const NO_ROOM: Refusal = Refusal(StatusCode::NOT_FOUND, "room");

/// Lifecycle state carried by a room's watch channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    Slot { seq: u64, frame: String },
    Gone,
}

/// Pure expiration check: a live room expires strictly after `ttl_ms` without writes (`PLAN.md` §5.5).
pub fn room_expired(last_ms: i64, now_ms: i64, ttl_ms: i64) -> bool {
    now_ms > last_ms.saturating_add(ttl_ms)
}

pub struct Room {
    owner: String,
    slot_at: i64,
    last: i64,
    seq: u64,
    live: watch::Sender<SlotState>,
}

/// An SSE event always carries non-empty data, or a browser drops it: `-` stands for none.
fn event(name: &str, data: &str) -> Event {
    Event::default().event(name).data(if data.is_empty() { "-" } else { data })
}

/// Removes rooms not written to for longer than `server.config.room.3` ms and notifies online
/// viewers with `SlotState::Gone`.
pub fn sweep(server: &Server, now_ms: i64) -> usize {
    let mut rooms = lock(&server.rooms);
    let ttl_ms = server.config.room.3;
    let expired: Vec<String> = rooms
        .iter()
        .filter(|(_, room)| room_expired(room.last, now_ms, ttl_ms))
        .map(|(id, _)| id.clone())
        .collect();
    let removed = expired.len();
    for id in expired {
        if let Some(room) = rooms.remove(&id) {
            let _ = room.live.send_replace(SlotState::Gone); // dropped: previous slot value is replaced
        }
    }
    removed
}

/// `POST /rooms`: a signed `talk/room` record `{ room, do, frame }`. `make` opens an ephemeral
/// slot room for its signer; `slot` replaces the latest ciphertext frame (monotonic `record.at`)
/// and returns live `viewers`; `gone` removes the room and closes all viewer streams.
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
        let (live, _) = watch::channel(SlotState::Slot { seq: 0, frame: String::new() });
        rooms.insert(id, Room { owner: did, slot_at: 0, last: now(), seq: 0, live });
        return Ok(Json(json!({ "viewers": 0 })));
    }
    let room = rooms.get_mut(&id).ok_or(NO_ROOM)?;
    if room.owner != did {
        return Err(Refusal(StatusCode::FORBIDDEN, "owner"));
    }
    match what.as_str() {
        "slot" => {
            if record.at <= room.slot_at {
                return Err(Refusal(StatusCode::CONFLICT, "replay"));
            }
            if frame.len() > most_bytes {
                return Err(Refusal(StatusCode::PAYLOAD_TOO_LARGE, "full"));
            }
            room.slot_at = record.at;
            room.last = now();
            room.seq = room.seq.saturating_add(1);
            let _ = room.live.send_replace(SlotState::Slot { seq: room.seq, frame }); // dropped: previous slot value is replaced
            Ok(Json(json!({ "viewers": room.live.receiver_count() })))
        }
        "gone" => {
            if let Some(room) = rooms.remove(&id) {
                let _ = room.live.send_replace(SlotState::Gone); // dropped: previous slot value is replaced
            }
            Ok(Json(json!({})))
        }
        _ => Err(Refusal(StatusCode::BAD_REQUEST, "do")),
    }
}

/// `GET /rooms/{id}/events`: streams the latest ciphertext slot via `WatchStream` and closes
/// cleanly right after delivering `gone`.
pub async fn events(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Refusal> {
    let rx = {
        let rooms = lock(&server.rooms);
        let room = rooms.get(&id).ok_or(NO_ROOM)?;
        if room.live.receiver_count() >= server.config.room.2 {
            return Err(Refusal(StatusCode::TOO_MANY_REQUESTS, "viewers"));
        }
        room.live.subscribe()
    };
    let live = WatchStream::new(rx).map(|state| match state {
        SlotState::Slot { frame, .. } => Ok(event("slot", &frame)),
        SlotState::Gone => Ok(event("gone", "")),
    });
    Ok(Sse::new(live).keep_alive(KeepAlive::default()))
}
