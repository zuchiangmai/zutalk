//! How long a recording's audio stays on this device. The person chooses it among the device's
//! preferences; whatever they choose, the audio stays until the recording's whole-session
//! record is written from it, unless they destroy it themselves. The rule is a function of
//! values; the sweep applies it at start-up and after each transcript is written.

use crate::app::proto::{Row, State};
use crate::capture::tape;
use std::path::Path;

const DAY_MS: i64 = 86_400_000; // a day: `keep` is counted in days

/// Whether a recording's audio goes now: once its record is `written`, when it is `keep` days
/// old (none: never, until the person destroys it). A start in the future is no age at all.
pub fn goes(keep: Option<u32>, written: bool, started: i64, now: i64) -> bool {
    let age = now.saturating_sub(started).max(0);
    written && keep.is_some_and(|days| age >= i64::from(days) * DAY_MS)
}

/// Destroys the audio of each recording in the library that goes now, and marks it gone.
pub fn sweep(tapes: &Path, state: &mut State, now: i64) -> Result<(), &'static str> {
    let keep = state.setup.keep;
    let due = |row: &&mut Row| row.audio && goes(keep, !row.draft, row.started, now);
    for row in state.library.iter_mut().filter(due) {
        tape::destroy(&tapes.join(&row.id)).map_err(|_| "tape.destroy")?;
        row.audio = false;
    }
    Ok(())
}

/// Destroys a recording's audio at the person's word: one of theirs, and not while it is taken.
pub fn destroy(tapes: &Path, state: &State, id: &str) -> Result<(), &'static str> {
    if state.session.as_ref().is_some_and(|session| session.id == id) {
        return Err("recording.under_way");
    }
    let row = state.library.iter().find(|row| row.id == id).ok_or("recording.gone")?;
    tape::destroy(&tapes.join(&row.id)).map_err(|_| "tape.destroy")
}
