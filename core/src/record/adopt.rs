//! Adoption of device-local (`did:key`) recordings and glossary terms into a person's `did:zu`
//! shelf on first sign-in (`PLAN.md` §2.7, `P4.md` §3.4, `DECISIONS.md` #9–#11).
//!
//! Strictly stays inside the `record` layer (never imports `crate::knowledge`), re-signs every
//! take of `talk/line`, `talk/speaker`, `talk/note`, `talk/recording` (last, for crash atomicity),
//! and `talk/term`, leaves `tape/` and the local shelf untouched until explicit `DiscardLocal`,
//! and binds `local -> first person` atomically in `<data>/adopted.json`.

use crate::record::notes::NoteBody;
use crate::record::shelf::{
    LINE, NOTE, RECORDING, Recording, SPEAKER, Said, Shelf, TERM, TOPIC, Words,
};
use crate::record::takes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
use ts_rs::TS;

/// Key of the pinned "used in every recording" topic (`P6.md` §3.2); kept here as a literal so
/// `record/adopt.rs` stays strictly inside the `record` layer without importing `crate::knowledge`.
const ALWAYS_TOPIC_KEY: &str = "always";

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
    #[serde(default)]
    pub topics: u32,
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
    pub topics: u32,
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

/// Whether `person_did` may discard `local_did`'s backup given `marker` and whether any local
/// recordings or glossary terms still exist on disk.
pub fn can_discard(
    marker: Option<&Adopted>,
    local_did: &str,
    person_did: &str,
    has_local_data: bool,
) -> bool {
    if !has_local_data {
        return true;
    }
    marker.is_some_and(|m| m.local == local_did && m.to == person_did)
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

/// Pure `serde_json::Value` merge of missing `terms`, `facts`, and `translation_terms` from a
/// local `"always"` topic into the person's existing `"always"` topic without importing
/// `crate::knowledge` (`P6.md` §3.2). Returns `Some(merged)` only when at least one entry is added.
fn merge_always_topic_value(
    person: &serde_json::Value,
    local: &serde_json::Value,
) -> Option<serde_json::Value> {
    let mut merged = person.as_object()?.clone();
    let local_obj = local.as_object()?;
    let mut changed = false;

    // Merge missing `terms` by lowercased trimmed `said`.
    if let Some(local_terms) = local_obj.get("terms").and_then(serde_json::Value::as_array) {
        let mut out_terms =
            merged.get("terms").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        let mut seen: BTreeSet<String> = out_terms
            .iter()
            .filter_map(|v| v.get("said").and_then(serde_json::Value::as_str))
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        for item in local_terms {
            let Some(said) = item.get("said").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let key = said.trim().to_lowercase();
            if !key.is_empty() && seen.insert(key) {
                out_terms.push(item.clone());
                changed = true;
            }
        }
        if changed {
            merged.insert("terms".into(), serde_json::Value::Array(out_terms));
        }
    }

    // Merge missing `facts` by lowercased trimmed `(key, value)`.
    if let Some(local_facts) = local_obj.get("facts").and_then(serde_json::Value::as_array) {
        let mut out_facts =
            merged.get("facts").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        let mut seen: BTreeSet<(String, String)> = out_facts
            .iter()
            .filter_map(|v| {
                let k = v.get("key").and_then(serde_json::Value::as_str)?.trim().to_lowercase();
                let val = v.get("value").and_then(serde_json::Value::as_str)?.trim().to_lowercase();
                (!k.is_empty() && !val.is_empty()).then_some((k, val))
            })
            .collect();
        let mut facts_changed = false;
        for item in local_facts {
            let (Some(k), Some(val)) = (
                item.get("key").and_then(serde_json::Value::as_str),
                item.get("value").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            let pair = (k.trim().to_lowercase(), val.trim().to_lowercase());
            if !pair.0.is_empty() && !pair.1.is_empty() && seen.insert(pair) {
                out_facts.push(item.clone());
                facts_changed = true;
            }
        }
        if facts_changed {
            merged.insert("facts".into(), serde_json::Value::Array(out_facts));
            changed = true;
        }
    }

    // Merge missing `translation_terms` by lowercased trimmed `(source, target)`.
    if let Some(local_pairs) =
        local_obj.get("translation_terms").and_then(serde_json::Value::as_array)
    {
        let mut out_pairs = merged
            .get("translation_terms")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut seen: BTreeSet<(String, String)> = out_pairs
            .iter()
            .filter_map(|v| {
                let s = v.get("source").and_then(serde_json::Value::as_str)?.trim().to_lowercase();
                let t = v.get("target").and_then(serde_json::Value::as_str)?.trim().to_lowercase();
                (!s.is_empty() && !t.is_empty()).then_some((s, t))
            })
            .collect();
        let mut pairs_changed = false;
        for item in local_pairs {
            let (Some(s), Some(t)) = (
                item.get("source").and_then(serde_json::Value::as_str),
                item.get("target").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            let pair = (s.trim().to_lowercase(), t.trim().to_lowercase());
            if !pair.0.is_empty() && !pair.1.is_empty() && seen.insert(pair) {
                out_pairs.push(item.clone());
                pairs_changed = true;
            }
        }
        if pairs_changed {
            merged.insert("translation_terms".into(), serde_json::Value::Array(out_pairs));
            changed = true;
        }
    }

    changed.then_some(serde_json::Value::Object(merged))
}

/// Fast directory-entry count of recording folders on a local shelf (zero pack reads).
pub fn pending_recordings_count(local: &Shelf) -> usize {
    let Ok(entries) = fs::read_dir(local.root()) else {
        return 0;
    };
    entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).count()
}

/// Adopts all eligible local recordings (excluding `exclude_ids`), glossary terms, and knowledge
/// topics from `local` into `person`, leaving `local` and `tape/` untouched and recording the
/// binding in `adopted.json`.
pub fn run(
    data: &Path,
    local: &Shelf,
    person: &Shelf,
    exclude_ids: &[String],
    now: i64,
) -> Result<AdoptSummary, &'static str> {
    run_with_progress(data, local, person, exclude_ids, now, |_, _, _| {})
}

/// Same as [`run`], calling `progress(done, total, id)` before each recording is adopted.
pub fn run_with_progress(
    data: &Path,
    local: &Shelf,
    person: &Shelf,
    exclude_ids: &[String],
    now: i64,
    mut progress: impl FnMut(u32, u32, &str),
) -> Result<AdoptSummary, &'static str> {
    let recs: Vec<Recording> = local
        .list()
        .map_err(|_| "adopt.failed")?
        .into_iter()
        .filter(|r| !exclude_ids.iter().any(|ex| ex == &r.id))
        .collect();
    let local_terms = local.kept::<serde_json::Value>(TERM).map_err(|_| "adopt.failed")?;
    let local_topics = local.kept::<serde_json::Value>(TOPIC).map_err(|_| "adopt.failed")?;
    let has_local_data = !recs.is_empty() || !local_terms.is_empty() || !local_topics.is_empty();

    let marker = read_marker(data)?;
    let decision = plan(marker.as_ref(), local.by(), person.by(), has_local_data);
    if decision != AdoptPlan::Adopt {
        return Ok(AdoptSummary { plan: decision, ..AdoptSummary::default() });
    }

    let total = u32::try_from(recs.len()).unwrap_or(u32::MAX);
    let mut adopted_ids = Vec::new();
    for (idx, rec) in recs.iter().enumerate() {
        if person.known(&rec.id).map_err(|_| "adopt.failed")? {
            continue;
        }
        progress(u32::try_from(idx).unwrap_or(u32::MAX), total, &rec.id);
        if adopt_one_recording(local, person, rec, now)? {
            adopted_ids.push(rec.id.clone());
        }
    }

    // Snapshot legacy local `talk/term` entries before consuming `local_terms` so that when the
    // local shelf has no `"always"` topic yet, they can still merge into the person's existing
    // `"always"` topic purely at the `serde_json::Value` layer (zero `crate::knowledge` import).
    let local_has_always = local_topics.iter().any(|(k, _)| k == ALWAYS_TOPIC_KEY);
    let legacy_as_always = (!local_has_always && !local_terms.is_empty()).then(|| {
        serde_json::json!({
            "terms": local_terms.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
        })
    });

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

    // Adopt `talk/topic` records without importing `crate::knowledge`, merging `"always"` losslessly
    // and checking tombstones via `known_kept` so person-deleted custom topics never resurrect.
    let existing_topics: BTreeMap<String, serde_json::Value> =
        person.kept::<serde_json::Value>(TOPIC).map_err(|_| "adopt.failed")?.into_iter().collect();
    let known_topics = person.known_kept(TOPIC).map_err(|_| "adopt.failed")?;
    let mut adopted_topics: Vec<(String, Option<serde_json::Value>)> = Vec::new();
    for (k, val) in local_topics {
        if k == ALWAYS_TOPIC_KEY {
            match existing_topics.get(ALWAYS_TOPIC_KEY) {
                None => adopted_topics.push((k, Some(val))),
                Some(person_always) => {
                    if let Some(merged) = merge_always_topic_value(person_always, &val) {
                        adopted_topics.push((k, Some(merged)));
                    }
                }
            }
        } else if !known_topics.contains(&k) {
            adopted_topics.push((k, Some(val)));
        }
    }
    if let (Some(legacy), Some(person_always)) =
        (legacy_as_always.as_ref(), existing_topics.get(ALWAYS_TOPIC_KEY))
        && let Some(merged) = merge_always_topic_value(person_always, legacy)
    {
        adopted_topics.push((ALWAYS_TOPIC_KEY.to_owned(), Some(merged)));
    }
    let topics_count = u32::try_from(adopted_topics.len()).unwrap_or(u32::MAX);
    if !adopted_topics.is_empty() {
        person.put("", TOPIC, &adopted_topics, now).map_err(|_| "adopt.failed")?;
    }

    let recs_count = u32::try_from(adopted_ids.len()).unwrap_or(u32::MAX);
    let next_marker = match marker {
        Some(prev) if prev.local == local.by() && prev.to == person.by() => Adopted {
            local: prev.local,
            to: prev.to,
            at: prev.at,
            recordings: prev.recordings.saturating_add(recs_count),
            terms: prev.terms.saturating_add(terms_count),
            topics: prev.topics.saturating_add(topics_count),
        },
        _ => Adopted {
            local: local.by().to_owned(),
            to: person.by().to_owned(),
            at: now,
            recordings: recs_count,
            terms: terms_count,
            topics: topics_count,
        },
    };
    if recs_count > 0 || terms_count > 0 || topics_count > 0 || read_marker(data)?.is_none() {
        write_marker(data, &next_marker)?;
    }

    Ok(AdoptSummary {
        plan: AdoptPlan::Adopt,
        recordings: recs_count,
        terms: terms_count,
        topics: topics_count,
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
            topics: prev.topics,
        },
        _ => Adopted {
            local: local.by().to_owned(),
            to: person.by().to_owned(),
            at: now,
            recordings: 1,
            terms: 0,
            topics: 0,
        },
    };
    write_marker(data, &next_marker)?;
    Ok(AdoptSummary {
        plan: AdoptPlan::Adopt,
        recordings: 1,
        terms: 0,
        topics: 0,
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
