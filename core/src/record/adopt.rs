//! Adoption of device-local (`did:key`) recordings and glossary terms into a person's `did:zu`
//! shelf on first sign-in (`PLAN.md` §2.7, `P4.md` §3.4, `DECISIONS.md` #9–#11).
//!
//! Strictly stays inside the `record` layer (never imports `crate::knowledge`), re-signs every
//! take of `talk/line`, `talk/speaker`, `talk/note`, `talk/recording` (last, for crash atomicity),
//! and `talk/term`, leaves `tape/` and the local shelf untouched until explicit `DiscardLocal`,
//! and binds `local -> first person` atomically in `<data>/adopted.json`.

use crate::record::notes::NoteBody;
use crate::record::shelf::{LINE, NOTE, RECORDING, Recording, SPEAKER, Said, Shelf, TERM, Words};
use crate::record::takes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
use ts_rs::TS;

/// Bounded summary of the device-local shelf exposed in `State`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct LocalShelf {
    pub recordings: u32,
    pub adopted: bool,
}

/// Persistent single-person adoption binding stored atomically in `<data>/adopted.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adopted {
    pub local: String,
    pub to: String,
    pub at: i64,
    pub recordings: u32,
    pub terms: u32,
}

/// Pure adoption decision produced by [`plan`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AdoptPlan {
    Adopt,
    OtherPerson,
    #[default]
    Nothing,
}

/// Outcome of one [`run`] or [`run_one`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdoptSummary {
    pub plan: AdoptPlan,
    pub recordings: u32,
    pub terms: u32,
    pub adopted_ids: Vec<String>,
}

/// Pure decision rule: bind `local_did` to at most one `person_did`; refuse a second distinct
/// person even if local data still exists on disk.
pub fn plan(
    marker: Option<&Adopted>,
    local_did: &str,
    person_did: &str,
    has_local_data: bool,
) -> AdoptPlan {
    if let Some(m) = marker
        && m.local == local_did
        && m.to != person_did
    {
        return AdoptPlan::OtherPerson;
    }
    if !has_local_data {
        return AdoptPlan::Nothing;
    }
    AdoptPlan::Adopt
}

/// Reads `<data>/adopted.json` if present.
pub fn read_marker(data: &Path) -> Result<Option<Adopted>, &'static str> {
    match fs::read(data.join("adopted.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| "adopt.failed"),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err("adopt.failed"),
    }
}

/// Atomically writes `<data>/adopted.json` via temporary sibling file + `sync_all` + `rename`.
pub fn write_marker(data: &Path, marker: &Adopted) -> Result<(), &'static str> {
    fs::create_dir_all(data).map_err(|_| "adopt.failed")?;
    let bytes = serde_json::to_vec_pretty(marker).map_err(|_| "adopt.failed")?;
    let tmp = data.join("adopted.json.tmp");
    let mut file = fs::File::create(&tmp).map_err(|_| "adopt.failed")?;
    file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|_| "adopt.failed")?;
    fs::rename(tmp, data.join("adopted.json")).map_err(|_| "adopt.failed")
}

/// Re-signs a single local recording (all takes of `talk/line`, `talk/speaker`, `talk/note`, and
/// `talk/recording` last) onto `person`. Returns `Ok(false)` if `person` already knows `rec.id`
/// (including recordings previously removed by `person`, so removals are never resurrected).
fn adopt_one_recording(
    local: &Shelf,
    person: &Shelf,
    rec: &Recording,
    now: i64,
) -> Result<bool, &'static str> {
    if person.known(&rec.id).map_err(|_| "adopt.failed")? {
        return Ok(false);
    }

    // 1. All live takes of `talk/line`.
    let take_infos = takes::list(local, &rec.id, rec.about.take).map_err(|_| "adopt.failed")?;
    let mut all_lines: Vec<(String, Option<Said>)> = Vec::new();
    for info in take_infos {
        let lines = takes::read(local, &rec.id, info.take).map_err(|_| "adopt.failed")?;
        for (key, said) in lines {
            all_lines.push((key, Some(said)));
        }
    }
    if !all_lines.is_empty() {
        person.put(&rec.id, LINE, &all_lines, now).map_err(|_| "adopt.failed")?;
    }

    // 2. All `talk/speaker` names.
    if !rec.speakers.is_empty() {
        let speakers: Vec<(String, Option<Words>)> = rec
            .speakers
            .iter()
            .map(|(label, name)| {
                (format!("{}.{label}", rec.id), Some(Words { text: name.clone() }))
            })
            .collect();
        person.put(&rec.id, SPEAKER, &speakers, now).map_err(|_| "adopt.failed")?;
    }

    // 3. All `talk/note` records (both timestamped/anchored marks and legacy single-string key).
    if !rec.notes.is_empty() {
        let note_pairs: Vec<(String, Option<NoteBody>)> = rec
            .notes
            .iter()
            .map(|n| {
                (
                    n.key.clone(),
                    Some(NoteBody { at: n.at_ms, text: n.text.clone(), anchor: n.anchor.clone() }),
                )
            })
            .collect();
        person.put(&rec.id, NOTE, &note_pairs, now).map_err(|_| "adopt.failed")?;
    }

    // 4. Write `talk/recording` LAST so a crash midway never leaves a half-adopted recording.
    person
        .put(&rec.id, RECORDING, &[(rec.id.clone(), Some(rec.about.clone()))], now)
        .map_err(|_| "adopt.failed")?;

    Ok(true)
}

/// Adopts all eligible local recordings (excluding `exclude_ids`) and glossary terms from `local`
/// into `person`, leaving `local` and `tape/` untouched and recording the binding in `adopted.json`.
pub fn run(
    data: &Path,
    local: &Shelf,
    person: &Shelf,
    exclude_ids: &[String],
    now: i64,
) -> Result<AdoptSummary, &'static str> {
    let recs: Vec<Recording> = local
        .list()
        .map_err(|_| "adopt.failed")?
        .into_iter()
        .filter(|r| !exclude_ids.iter().any(|ex| ex == &r.id))
        .collect();
    let local_terms = local.kept::<serde_json::Value>(TERM).map_err(|_| "adopt.failed")?;
    let has_local_data = !recs.is_empty() || !local_terms.is_empty();

    let marker = read_marker(data)?;
    let decision = plan(marker.as_ref(), local.by(), person.by(), has_local_data);
    if decision != AdoptPlan::Adopt {
        return Ok(AdoptSummary { plan: decision, ..AdoptSummary::default() });
    }

    let mut adopted_ids = Vec::new();
    for rec in &recs {
        if adopt_one_recording(local, person, rec, now)? {
            adopted_ids.push(rec.id.clone());
        }
    }

    // Adopt missing `talk/term` records without importing `crate::knowledge`.
    let existing_terms: BTreeSet<String> = person
        .kept::<serde_json::Value>(TERM)
        .map_err(|_| "adopt.failed")?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let missing_terms: Vec<(String, Option<serde_json::Value>)> = local_terms
        .into_iter()
        .filter(|(k, _)| !existing_terms.contains(k))
        .map(|(k, val)| (k, Some(val)))
        .collect();
    let terms_count = u32::try_from(missing_terms.len()).unwrap_or(u32::MAX);
    if !missing_terms.is_empty() {
        person.put("", TERM, &missing_terms, now).map_err(|_| "adopt.failed")?;
    }

    let recs_count = u32::try_from(adopted_ids.len()).unwrap_or(u32::MAX);
    let next_marker = match marker {
        Some(prev) if prev.local == local.by() && prev.to == person.by() => Adopted {
            local: prev.local,
            to: prev.to,
            at: prev.at,
            recordings: prev.recordings.saturating_add(recs_count),
            terms: prev.terms.saturating_add(terms_count),
        },
        _ => Adopted {
            local: local.by().to_owned(),
            to: person.by().to_owned(),
            at: now,
            recordings: recs_count,
            terms: terms_count,
        },
    };
    if recs_count > 0 || terms_count > 0 || read_marker(data)?.is_none() {
        write_marker(data, &next_marker)?;
    }

    Ok(AdoptSummary {
        plan: AdoptPlan::Adopt,
        recordings: recs_count,
        terms: terms_count,
        adopted_ids,
    })
}

/// Adopts one specific recording `id` from `local` into `person` (used when an in-progress local
/// recording finishes sealing after the person has already signed in).
pub fn run_one(
    data: &Path,
    local: &Shelf,
    person: &Shelf,
    id: &str,
    now: i64,
) -> Result<AdoptSummary, &'static str> {
    let Some(rec) = local.about(id).map_err(|_| "adopt.failed")? else {
        return Ok(AdoptSummary { plan: AdoptPlan::Nothing, ..AdoptSummary::default() });
    };
    let marker = read_marker(data)?;
    let decision = plan(marker.as_ref(), local.by(), person.by(), true);
    if decision != AdoptPlan::Adopt {
        return Ok(AdoptSummary { plan: decision, ..AdoptSummary::default() });
    }
    if !adopt_one_recording(local, person, &rec, now)? {
        return Ok(AdoptSummary { plan: AdoptPlan::Adopt, ..AdoptSummary::default() });
    }
    let next_marker = match marker {
        Some(prev) if prev.local == local.by() && prev.to == person.by() => Adopted {
            local: prev.local,
            to: prev.to,
            at: prev.at,
            recordings: prev.recordings.saturating_add(1),
            terms: prev.terms,
        },
        _ => Adopted {
            local: local.by().to_owned(),
            to: person.by().to_owned(),
            at: now,
            recordings: 1,
            terms: 0,
        },
    };
    write_marker(data, &next_marker)?;
    Ok(AdoptSummary {
        plan: AdoptPlan::Adopt,
        recordings: 1,
        terms: 0,
        adopted_ids: vec![id.to_owned()],
    })
}

/// Computes the bounded [`LocalShelf`] summary for `State`.
pub fn status(data: &Path, local: Option<&Shelf>, person_did: Option<&str>) -> LocalShelf {
    let Some(shelf) = local else {
        return LocalShelf::default();
    };
    let recordings =
        shelf.list().map(|list| u32::try_from(list.len()).unwrap_or(u32::MAX)).unwrap_or(0);
    let adopted = match (person_did, read_marker(data)) {
        (Some(person), Ok(Some(marker))) => marker.local == shelf.by() && marker.to == person,
        _ => false,
    };
    LocalShelf { recordings, adopted }
}

/// Removes the device-local shelf directory and SQLite index while preserving `adopted.json`
/// and `tape/` untouched.
pub fn discard_local(data: &Path, local_did: &str) -> Result<(), &'static str> {
    let dir_name = local_did.replace(':', "-");
    let records_dir = data.join("records").join(&dir_name);
    if let Err(e) = fs::remove_dir_all(&records_dir)
        && e.kind() != ErrorKind::NotFound
    {
        return Err("disk");
    }
    let index_dir = data.join("index");
    for suffix in ["", "-wal", "-shm"] {
        let idx = index_dir.join(format!("{dir_name}.sqlite3{suffix}"));
        if let Err(e) = fs::remove_file(&idx)
            && e.kind() != ErrorKind::NotFound
        {
            return Err("disk");
        }
    }
    Ok(())
}
