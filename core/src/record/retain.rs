//! How long a recording's audio stays on this device. The person chooses it among the device's
//! preferences; whatever they choose, the audio stays until the recording's whole-session
//! record is written from it, unless they destroy it themselves. The rule is a function of
//! values; the sweep applies it at start-up and after each transcript is written.

use crate::capture::tape;
use std::path::Path;

const DAY_MS: i64 = 86_400_000; // a day: `keep` is counted in days

/// What retention needs to know about one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub started: i64,
    pub draft: bool,
    pub audio: bool,
}

/// Whether a recording's audio goes now: once its record is `written`, when it is `keep` days
/// old (none: never, until the person destroys it). A start in the future is no age at all.
pub fn goes(keep: Option<u32>, written: bool, started: i64, now: i64) -> bool {
    let age = now.saturating_sub(started).max(0);
    written && keep.is_some_and(|days| age >= i64::from(days) * DAY_MS)
}

/// Destroys the audio of each entry that goes now, marks `audio = false`, and returns the ids
/// whose audio was destroyed.
pub fn sweep(
    tapes: &Path,
    keep: Option<u32>,
    entries: &mut [Entry],
    now: i64,
) -> Result<Vec<String>, &'static str> {
    let mut destroyed = Vec::new();
    for entry in entries.iter_mut() {
        if entry.audio && goes(keep, !entry.draft, entry.started, now) {
            tape::destroy(&tapes.join(&entry.id)).map_err(|_| "tape.destroy")?;
            entry.audio = false;
            destroyed.push(entry.id.clone());
        }
    }
    Ok(destroyed)
}

/// Destroys a recording's audio at the person's word: one of theirs, and not while it is taken.
pub fn destroy(
    tapes: &Path,
    entries: &[Entry],
    under_way: Option<&str>,
    id: &str,
) -> Result<(), &'static str> {
    if under_way == Some(id) {
        return Err("recording.under_way");
    }
    let entry = entries.iter().find(|entry| entry.id == id).ok_or("recording.gone")?;
    tape::destroy(&tapes.join(&entry.id)).map_err(|_| "tape.destroy")
}
