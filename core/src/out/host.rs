//! Ephemeral live audience hosting (`PLAN.md` §2.1 & §5.5, `P5.md` §3.3).
//!
//! A hosted room lives only in memory during a session: its 32-byte AES-256-GCM key is generated
//! randomly per room, stays solely in RAM (`Zeroizing`), and leaves the device only after `#` in
//! the viewer link or QR code. On disk, `<data>/host.lock` holds **only** the 32-char lowercase
//! hex room id so a crash leaves enough to send `gone` on the next launch without ever persisting
//! a key or transcript.

use crate::id::keys::{Ours, random};
use crate::rt::supervise::End;
use aes_gcm::{Aes256Gcm, aead::Aead, aead::KeyInit};
use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Instant;
use ts_rs::TS;
use zutalk_log::{info, warn};

/// Max live caption slot publish interval (`DESIGN.md` §5.3).
pub const HOST_SLOT_MS: u64 = 250;
/// Waiting viewer count refresh and keepalive interval while armed (well under server 120 s TTL).
pub const HOST_ARM_POLL_MS: u64 = 3_000;
/// Live keepalive interval during silence or pause (well under server 120 s TTL).
pub const HOST_KEEPALIVE_MS: u64 = 30_000;
/// Max wait for `gone` during clean desktop exit (`P5.md` §3.4).
pub const HOST_EXIT_WAIT_MS: u64 = 1_500;
/// Crash-recovery lock file under `<data>/` storing only the 32-char hex room id (never a key).
pub const HOST_LOCK_FILE: &str = "host.lock";

const FRAME: End = End::Fatal("room.frame");

/// City event selected before or during a hosted session (`P5.md` §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HostEvent {
    pub city: String,
    pub event: String,
    pub title: String,
    #[ts(type = "number | null")]
    pub start: Option<i64>,
}

/// Redacts the `#<key>` fragment of a viewer link in `Debug` output so 32-byte room keys never
/// appear in assertion messages, error wrappers, or diagnostics (Principle 6).
struct RedactedLink<'a>(&'a str);

impl std::fmt::Debug for RedactedLink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.split_once('#') {
            Some((prefix, _)) => std::fmt::Debug::fmt(&format!("{prefix}#…"), f),
            None => std::fmt::Debug::fmt("…", f),
        }
    }
}

/// Bounded in-memory hosting state machine (`P5.md` §3.3).
/// At most one hosted session exists at a time; nothing about it persists in records or settings.
#[derive(Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "is")]
pub enum HostState {
    #[default]
    Idle,
    Arming {
        event: Option<HostEvent>,
        captions: bool,
    },
    Armed {
        event: Option<HostEvent>,
        captions: bool,
        link: Option<String>,
        viewers: u32,
    },
    Live {
        event: Option<HostEvent>,
        outbound: bool,
        link: Option<String>,
        viewers: u32,
    },
    Stopping,
}

impl std::fmt::Debug for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostState::Idle => f.write_str("Idle"),
            HostState::Arming { event, captions } => {
                f.debug_struct("Arming").field("event", event).field("captions", captions).finish()
            }
            HostState::Armed { event, captions, link, viewers } => f
                .debug_struct("Armed")
                .field("event", event)
                .field("captions", captions)
                .field("link", &link.as_deref().map(RedactedLink))
                .field("viewers", viewers)
                .finish(),
            HostState::Live { event, outbound, link, viewers } => f
                .debug_struct("Live")
                .field("event", event)
                .field("outbound", outbound)
                .field("link", &link.as_deref().map(RedactedLink))
                .field("viewers", viewers)
                .finish(),
            HostState::Stopping => f.write_str("Stopping"),
        }
    }
}

impl HostState {
    /// Stable plain state code for `host.state` log events.
    pub const fn code(&self) -> &'static str {
        match self {
            HostState::Idle => "Idle",
            HostState::Arming { .. } => "Arming",
            HostState::Armed { .. } => "Armed",
            HostState::Live { .. } => "Live",
            HostState::Stopping => "Stopping",
        }
    }

    /// True while audience live captions are armed or actively leaving the device.
    pub fn is_outbound(&self) -> bool {
        matches!(
            self,
            HostState::Armed { captions: true, .. } | HostState::Live { outbound: true, .. }
        )
    }

    /// True while any hosted flow (arming, armed, live, or stopping) is in progress.
    pub fn is_active(&self) -> bool {
        !matches!(self, HostState::Idle)
    }

    /// Current live viewer count reported by the server, or `0` when not outbound.
    pub fn viewers(&self) -> u32 {
        match self {
            HostState::Armed { viewers, .. } | HostState::Live { viewers, .. } => *viewers,
            HostState::Idle | HostState::Arming { .. } | HostState::Stopping => 0,
        }
    }

    /// Selected city event carried by the current hosted flow, if any.
    pub fn event(&self) -> Option<&HostEvent> {
        match self {
            HostState::Arming { event, .. }
            | HostState::Armed { event, .. }
            | HostState::Live { event, .. } => event.as_ref(),
            HostState::Idle | HostState::Stopping => None,
        }
    }
}

/// Pure transition input for [`host_step`].
#[derive(Clone, PartialEq, Eq)]
pub enum HostStepEvent {
    Arm { event: Option<HostEvent>, captions: bool },
    RoomMade { link: String },
    RoomFailed,
    Viewers(u32),
    RecordingStarted,
    StopOutbound,
    Cancel { wait_gone: bool },
    RecordingStopped { wait_gone: bool },
    Stopped,
}

impl std::fmt::Debug for HostStepEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostStepEvent::Arm { event, captions } => {
                f.debug_struct("Arm").field("event", event).field("captions", captions).finish()
            }
            HostStepEvent::RoomMade { link } => {
                f.debug_struct("RoomMade").field("link", &RedactedLink(link)).finish()
            }
            HostStepEvent::RoomFailed => f.write_str("RoomFailed"),
            HostStepEvent::Viewers(n) => f.debug_tuple("Viewers").field(n).finish(),
            HostStepEvent::RecordingStarted => f.write_str("RecordingStarted"),
            HostStepEvent::StopOutbound => f.write_str("StopOutbound"),
            HostStepEvent::Cancel { wait_gone } => {
                f.debug_struct("Cancel").field("wait_gone", wait_gone).finish()
            }
            HostStepEvent::RecordingStopped { wait_gone } => {
                f.debug_struct("RecordingStopped").field("wait_gone", wait_gone).finish()
            }
            HostStepEvent::Stopped => f.write_str("Stopped"),
        }
    }
}

/// Pure state transition function for [`HostState`] (`P5.md` §3.3).
pub fn host_step(state: &HostState, event: HostStepEvent) -> HostState {
    match (state, event) {
        (HostState::Idle, HostStepEvent::Arm { event, captions: true }) => {
            HostState::Arming { event, captions: true }
        }
        (HostState::Idle, HostStepEvent::Arm { event, captions: false }) => {
            HostState::Armed { event, captions: false, link: None, viewers: 0 }
        }
        (HostState::Idle, _) => HostState::Idle,

        (HostState::Arming { .. }, HostStepEvent::Arm { event, captions: true }) => {
            HostState::Arming { event, captions: true }
        }
        (HostState::Arming { .. }, HostStepEvent::Arm { event, captions: false }) => {
            HostState::Armed { event, captions: false, link: None, viewers: 0 }
        }
        (HostState::Arming { event, captions }, HostStepEvent::RoomMade { link }) => {
            HostState::Armed {
                event: event.clone(),
                captions: *captions,
                link: Some(link),
                viewers: 0,
            }
        }
        (HostState::Arming { .. }, HostStepEvent::RoomFailed) => HostState::Idle,
        (HostState::Arming { event, .. }, HostStepEvent::RecordingStarted) => {
            HostState::Live { event: event.clone(), outbound: false, link: None, viewers: 0 }
        }
        (HostState::Arming { event, .. }, HostStepEvent::StopOutbound) => {
            HostState::Armed { event: event.clone(), captions: false, link: None, viewers: 0 }
        }
        (
            HostState::Arming { .. },
            HostStepEvent::Cancel { wait_gone: true }
            | HostStepEvent::RecordingStopped { wait_gone: true },
        ) => HostState::Stopping,
        (
            HostState::Arming { .. },
            HostStepEvent::Cancel { wait_gone: false }
            | HostStepEvent::RecordingStopped { wait_gone: false },
        ) => HostState::Idle,
        (HostState::Arming { .. }, HostStepEvent::Viewers(_) | HostStepEvent::Stopped) => {
            state.clone()
        }

        (
            HostState::Armed { captions, link, viewers, .. },
            HostStepEvent::Arm { event, captions: next_cap },
        ) => match (*captions, next_cap) {
            (false, true) => HostState::Arming { event, captions: true },
            (true, false) => HostState::Armed { event, captions: false, link: None, viewers: 0 },
            (same, _) => {
                HostState::Armed { event, captions: same, link: link.clone(), viewers: *viewers }
            }
        },
        (HostState::Armed { event, captions: true, link, .. }, HostStepEvent::Viewers(n)) => {
            HostState::Armed {
                event: event.clone(),
                captions: true,
                link: link.clone(),
                viewers: n,
            }
        }
        (HostState::Armed { event, captions, link, viewers }, HostStepEvent::RecordingStarted) => {
            let outbound = *captions && link.is_some();
            HostState::Live {
                event: event.clone(),
                outbound,
                link: outbound.then(|| link.clone()).flatten(),
                viewers: if outbound { *viewers } else { 0 },
            }
        }
        (HostState::Armed { event, .. }, HostStepEvent::StopOutbound) => {
            HostState::Armed { event: event.clone(), captions: false, link: None, viewers: 0 }
        }
        (
            HostState::Armed { .. },
            HostStepEvent::Cancel { wait_gone: true }
            | HostStepEvent::RecordingStopped { wait_gone: true },
        ) => HostState::Stopping,
        (
            HostState::Armed { .. },
            HostStepEvent::Cancel { wait_gone: false }
            | HostStepEvent::RecordingStopped { wait_gone: false },
        ) => HostState::Idle,
        (HostState::Armed { .. }, _) => state.clone(),

        (HostState::Live { event, outbound: true, link, .. }, HostStepEvent::Viewers(n)) => {
            HostState::Live { event: event.clone(), outbound: true, link: link.clone(), viewers: n }
        }
        (HostState::Live { event, .. }, HostStepEvent::StopOutbound) => {
            HostState::Live { event: event.clone(), outbound: false, link: None, viewers: 0 }
        }
        (
            HostState::Live { .. },
            HostStepEvent::Cancel { wait_gone: true }
            | HostStepEvent::RecordingStopped { wait_gone: true },
        ) => HostState::Stopping,
        (
            HostState::Live { .. },
            HostStepEvent::Cancel { wait_gone: false }
            | HostStepEvent::RecordingStopped { wait_gone: false },
        ) => HostState::Idle,
        (HostState::Live { .. }, _) => state.clone(),

        (HostState::Stopping, HostStepEvent::Stopped) => HostState::Idle,
        (HostState::Stopping, _) => HostState::Stopping,
    }
}

/// Whether `id` is a valid 32-character lowercase hexadecimal room id.
pub fn is_room_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// AES-256-GCM: `nonce(12) ‖ ciphertext ‖ tag(16)`.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], plain: &[u8]) -> Result<Vec<u8>, End> {
    let sealed = Aes256Gcm::new(key.into()).encrypt(nonce.into(), plain).map_err(|_| FRAME)?;
    Ok([&nonce[..], &sealed].concat())
}

/// Opens a sealed frame using `key`, failing on wrong key or any tampered byte.
pub fn open(key: &[u8; 32], frame: &[u8]) -> Result<Vec<u8>, End> {
    let (nonce, sealed) = frame.split_first_chunk::<12>().ok_or(FRAME)?;
    Aes256Gcm::new(key.into()).decrypt(nonce.into(), sealed).map_err(|_| FRAME)
}

/// Builds `{server}/r/{id}#{key}`: the key travels only in the URL fragment, never to the server.
pub fn link(server: &str, id: &str, key: &[u8; 32]) -> String {
    format!("{server}/r/{id}#{}", BASE64_URL_SAFE_NO_PAD.encode(key))
}

/// Atomically writes only the 32-char hex room id to `<data>/host.lock` (never a key).
pub fn write_lock(data: &Path, room_id: &str) -> Result<(), &'static str> {
    if !is_room_id(room_id) {
        return Err("room.bad");
    }
    fs::create_dir_all(data).map_err(|_| "disk")?;
    let tmp = data.join("host.lock.tmp");
    let dest = data.join(HOST_LOCK_FILE);
    fs::write(&tmp, room_id.as_bytes()).map_err(|_| "disk")?;
    fs::rename(&tmp, &dest).map_err(|_| "disk")?;
    Ok(())
}

/// Reads a valid 32-char hex room id from `<data>/host.lock`, returning `None` if absent or corrupt.
pub fn read_lock(data: &Path) -> Option<String> {
    let raw = fs::read_to_string(data.join(HOST_LOCK_FILE)).ok()?;
    let trimmed = raw.trim();
    is_room_id(trimmed).then(|| trimmed.to_owned())
}

/// Removes `<data>/host.lock` if present (idempotent).
pub fn clear_lock(data: &Path) {
    let _ = fs::remove_file(data.join(HOST_LOCK_FILE)); // dropped: missing lock file is already clean
}

/// Removes `<data>/host.lock` only if it still names `room_id`, preventing a slow teardown or
/// crash-recovery task from deleting a newer room's lock file.
pub fn clear_lock_if(data: &Path, room_id: &str) {
    if read_lock(data).as_deref() == Some(room_id) {
        clear_lock(data);
    }
}

/// Slot-only room mutations accepted by the ephemeral live server (`P5-01`).
pub enum Write<'a> {
    Make,
    Slot(Option<&'a [u8]>),
    Gone,
}

/// Sends a signed `talk/room` mutation (`make`, `slot`, or `gone`) and returns the server's live
/// viewer count. `Make` and `Gone` require no room key so crash recovery needs only `host.lock`.
pub async fn write(
    ours: &Ours,
    room: &str,
    key: Option<&[u8; 32]>,
    write: Write<'_>,
) -> Result<u32, End> {
    let action = match write {
        Write::Make => "make",
        Write::Slot(_) => "slot",
        Write::Gone => "gone",
    };
    let started = Instant::now();
    let body = match write {
        Write::Make => json!({ "room": room, "do": "make" }),
        Write::Slot(plain) => {
            let frame = match plain {
                Some(bytes) => {
                    let key = key.ok_or(FRAME)?;
                    BASE64_STANDARD.encode(seal(key, &random()?, bytes)?)
                }
                None => String::new(),
            };
            json!({ "room": room, "do": "slot", "frame": frame })
        }
        Write::Gone => json!({ "room": room, "do": "gone" }),
    };
    let outcome = ours.ask("rooms", "talk/room", body).await;
    let ms = started.elapsed().as_millis() as u64;
    match outcome {
        Ok(val) => {
            let viewers =
                val.get("viewers").and_then(|v| v.as_u64()).unwrap_or(0).min(u64::from(u32::MAX))
                    as u32;
            info!("host.room", action = action, code = "ok", viewers = viewers, ms = ms);
            Ok(viewers)
        }
        Err(end) => {
            let c = match end {
                End::Retry(c) | End::Fatal(c) => c,
                End::Done => "done",
            };
            warn!("host.room", action = action, code = c, viewers = 0u32, ms = ms);
            Err(end)
        }
    }
}
