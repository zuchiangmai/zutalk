//! Rooms: the one container for what leaves this device, whether live captions for an
//! audience or one recording for whoever has the link. A room on our server is a log of frames
//! and a slot for the latest one. Each frame is sealed here with the room's key, which travels
//! only after `#` in a link or a QR code: the server keeps ciphertext.

use crate::app::proto::Open;
use crate::id::keys::{Ours, random};
use crate::rt::supervise::End;
use aes_gcm::{Aes256Gcm, aead::Aead, aead::KeyInit};
use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD, Engine};
use serde_json::json;

const FRAME: End = End::Fatal("room.frame");
/// The most a frame holds, so that its request stays well under the server's 64 KiB.
pub const PAGE: usize = 32 << 10;

/// AES-256-GCM: `nonce(12) ‖ ciphertext ‖ tag(16)`.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], plain: &[u8]) -> Result<Vec<u8>, End> {
    let sealed = Aes256Gcm::new(key.into()).encrypt(nonce.into(), plain).map_err(|_| FRAME)?;
    Ok([&nonce[..], &sealed].concat())
}

/// What a frame holds, if it was sealed with `key` and not altered since.
pub fn open(key: &[u8; 32], frame: &[u8]) -> Result<Vec<u8>, End> {
    let (nonce, sealed) = frame.split_first_chunk::<12>().ok_or(FRAME)?;
    Aes256Gcm::new(key.into()).decrypt(nonce.into(), sealed).map_err(|_| FRAME)
}

/// What to give out: `{server}/r/{id}#{key}`. The viewer reads the room with that alone.
pub fn link(server: &str, id: &str, key: &[u8; 32]) -> String {
    format!("{server}/r/{id}#{}", BASE64_URL_SAFE_NO_PAD.encode(key))
}

/// A recording as frames of its log, each with what the recording is and as many of its
/// lines as fit in a page. The viewer puts the lines back together.
pub fn pages(open: &Open) -> Result<Vec<Vec<u8>>, End> {
    let json = |page: &Open| serde_json::to_vec(page).map_err(|_| FRAME);
    let head = Open { lines: Vec::new(), ..open.clone() };
    let empty = PAGE.saturating_sub(json(&head)?.len());
    let (mut pages, mut room) = (vec![head.clone()], empty);
    for line in &open.lines {
        let size = serde_json::to_vec(line).map_err(|_| FRAME)?.len() + 1;
        if size > room && pages.last().is_some_and(|page| !page.lines.is_empty()) {
            pages.push(head.clone());
            room = empty;
        }
        pages.last_mut().into_iter().for_each(|page| page.lines.push(line.clone()));
        room = room.saturating_sub(size);
    }
    pages.iter().map(json).collect()
}

/// What its owner does to a room: makes it, adds a frame to its log, puts a frame in its slot
/// (none: empties it), or takes it away. Frames go in plain and leave sealed.
pub enum Write<'a> {
    Make,
    Log(&'a [u8]),
    Slot(Option<&'a [u8]>),
    Gone,
}

pub async fn write(ours: &Ours, room: &str, key: &[u8; 32], write: Write<'_>) -> Result<(), End> {
    let sealed = |plain| Ok::<_, End>(BASE64_STANDARD.encode(seal(key, &random()?, plain)?));
    let body = match write {
        Write::Make => json!({ "room": room, "do": "make" }),
        Write::Log(plain) => json!({ "room": room, "do": "log", "frame": sealed(plain)? }),
        Write::Slot(plain) => {
            let frame = plain.map(sealed).transpose()?.unwrap_or_default();
            json!({ "room": room, "do": "slot", "frame": frame })
        }
        Write::Gone => json!({ "room": room, "do": "gone" }),
    };
    ours.ask("rooms", "talk/room", body).await.map(drop)
}
