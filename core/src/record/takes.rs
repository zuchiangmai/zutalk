//! Multiple transcript takes and the pure refinement state machine (`P2.md` §3.3.2).
//!
//! Take 0 is the live draft; each subsequent take (1, 2, …) is a whole-session transcription.
//! All takes stay in `<id>/lines.jsonl` and can be listed and read independently.

use crate::record::shelf::{LINE, Said, Shelf};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use ts_rs::TS;
use zu::{Error, Held, Pack, Record, record};

/// Summary of one transcript take stored in `<id>/lines.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TakeInfo {
    pub take: u32,
    pub lines: u32,
    pub is_live: bool,
    pub is_current: bool,
    #[ts(type = "number | null")]
    pub written_at: Option<i64>,
}

/// Persistent fatal failure record stored in `<data>/tape/<id>/write.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RefineFailure {
    pub code: String,
    #[ts(type = "number")]
    pub at: i64,
}

/// Explicit refinement lifecycle state for one recording (`P2.md` §3.3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "is")]
pub enum RefineStatus {
    Draft {
        why: Option<String>,
    },
    Refining {
        step: String,
        done: u32,
        of: u32,
    },
    Refined {
        take: u32,
    },
    Failed {
        code: String,
        #[ts(type = "number")]
        at: i64,
    },
}

impl RefineStatus {
    /// Stable state code for structured `refine.state` log events.
    pub fn code(&self) -> &'static str {
        match self {
            RefineStatus::Draft { .. } => "Draft",
            RefineStatus::Refining { .. } => "Refining",
            RefineStatus::Refined { .. } => "Refined",
            RefineStatus::Failed { .. } => "Failed",
        }
    }
}

/// Transition input for [`refine_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefineEvent<'a> {
    Start,
    Progress { step: &'a str, done: u32, of: u32 },
    Succeeded { take: u32 },
    Failed { code: &'a str },
    Cleared { why: Option<&'a str> },
}

/// Pure transition table for [`RefineStatus`] (`P2.md` §3.3.2).
pub fn refine_step(state: &RefineStatus, event: RefineEvent<'_>, now: i64) -> RefineStatus {
    match (state, event) {
        (_, RefineEvent::Start) => {
            RefineStatus::Refining { step: "closing".into(), done: 0, of: 0 }
        }
        (RefineStatus::Refining { .. }, RefineEvent::Progress { step, done, of }) => {
            RefineStatus::Refining { step: step.into(), done, of }
        }
        (RefineStatus::Refining { .. }, RefineEvent::Succeeded { take }) => {
            RefineStatus::Refined { take }
        }
        (RefineStatus::Refining { .. }, RefineEvent::Failed { code }) => {
            RefineStatus::Failed { code: code.into(), at: now }
        }
        (
            RefineStatus::Failed { .. } | RefineStatus::Draft { .. },
            RefineEvent::Cleared { why },
        ) => RefineStatus::Draft { why: why.map(str::to_owned) },
        (same, _) => same.clone(),
    }
}

/// Computes the initial [`RefineStatus`] of a recording at rest (when no refine job is active).
pub fn initial_refine_status(
    take: u32,
    audio: bool,
    has_key: bool,
    failed: Option<&RefineFailure>,
) -> RefineStatus {
    if take > 0 {
        RefineStatus::Refined { take }
    } else if let Some(failure) = failed {
        RefineStatus::Failed { code: failure.code.clone(), at: failure.at }
    } else if !audio {
        RefineStatus::Draft { why: Some("tape.none".into()) }
    } else if !has_key {
        RefineStatus::Draft { why: Some("keys.none".into()) }
    } else {
        RefineStatus::Draft { why: None }
    }
}

/// Parses `(take, line_seq)` from a line key `<id>.<take>.<n>`.
pub fn parse_key(id: &str, key: &str) -> Option<(u32, u32)> {
    let rest = key.strip_prefix(id)?.strip_prefix('.')?;
    let (take, seq) = rest.split_once('.')?;
    if take.is_empty() || seq.is_empty() || seq.contains('.') {
        return None;
    }
    Some((take.parse().ok()?, seq.parse().ok()?))
}

fn read_lines_pack(shelf: &Shelf, id: &str) -> Result<Held, Error> {
    match Pack::read(shelf.pack_path(id, LINE)) {
        Err(Error::Io(ErrorKind::NotFound)) => Ok(Held::default()),
        res => res,
    }
}

fn decode_said(held: &Held, current: &[&Record]) -> Result<Option<Said>, Error> {
    let Some(body) = current.first().and_then(|r| r.body).and_then(|cid| held.bodies.get(&cid))
    else {
        return Ok(None);
    };
    serde_json::from_str(&body.to_json()?).map(Some).map_err(|_| Error::Json)
}

/// Lists all takes in `<id>/lines.jsonl` that still have at least one live line, ascending by `take`.
pub fn list(shelf: &Shelf, id: &str, current_take: u32) -> Result<Vec<TakeInfo>, Error> {
    let held = read_lines_pack(shelf, id)?;
    let mut by_take: BTreeMap<u32, (u32, Option<i64>)> = BTreeMap::new();
    for (path, current) in record::paths(&held.records) {
        if path.ty != LINE {
            continue;
        }
        let Some((take, _)) = parse_key(id, &path.key) else {
            continue;
        };
        let Some(head) = current.first() else {
            continue;
        };
        if head.body.is_none() {
            continue;
        }
        let entry = by_take.entry(take).or_insert((0, None));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = Some(entry.1.map_or(head.at, |prev| prev.max(head.at)));
    }
    Ok(by_take
        .into_iter()
        .map(|(take, (lines, written_at))| TakeInfo {
            take,
            lines,
            is_live: take == 0,
            is_current: take == current_take,
            written_at,
        })
        .collect())
}

/// Reads all live lines for `take` of recording `id`, ordered by line sequence number.
pub fn read(shelf: &Shelf, id: &str, take: u32) -> Result<Vec<(String, Said)>, Error> {
    let held = read_lines_pack(shelf, id)?;
    let mut lines: Vec<(u32, String, Said)> = Vec::new();
    for (path, current) in record::paths(&held.records) {
        if path.ty != LINE {
            continue;
        }
        let Some((line_take, seq)) = parse_key(id, &path.key) else {
            continue;
        };
        if line_take != take {
            continue;
        }
        if let Some(said) = decode_said(&held, &current)? {
            lines.push((seq, path.key.clone(), said));
        }
    }
    lines.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(lines.into_iter().map(|(_, key, said)| (key, said)).collect())
}
