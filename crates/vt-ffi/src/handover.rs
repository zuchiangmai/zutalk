//! The last thing the 0.8 line does for its library: it hands every recording over to the new
//! ZuTalk, as the bodies of the Zu records each recording becomes there, and says where its
//! sealed audio is. The format is handover v1, specified in the new repository's
//! `docs/migrate.md`; the new app signs what it finds here and seals the audio again.
//!
//! What moves is exactly what the reading page shows: the live transcript with the person's
//! corrections (SQLite), the whole-session transcript with theirs (the Async Transcript Loro
//! document), the speakers' names, the session note and the notes of its marks. Nothing here
//! changes the library; the only writes are under `<data_dir>/handover/`, each file written
//! whole and renamed into place, `index.json` last.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{json, Value};
use vt_store::{
    AsyncTaskState, BuiltinNotebookTab, NotebookCaptureProfile, NotebookCaptureRun,
    RealtimeUtterance, SessionQuery, SessionRecord, UtteranceCompletion, UtteranceVariantRole,
    UtteranceVariantState,
};

use crate::block_document_api::{block_document_path, session_note_document_id};
use crate::{CoreError, ZuTalkCore};

/// Speaker id -> handover label, and handover label -> display name.
type HandoverSpeakers = (HashMap<String, String>, BTreeMap<String, String>);

/// The handover this build writes.
const VERSION: u32 = 1;

fn internal(message: impl std::fmt::Display) -> CoreError {
    CoreError::InternalError {
        message: message.to_string(),
    }
}

#[uniffi::export]
impl ZuTalkCore {
    /// Hands over every recording the library shows, the trash excepted, and returns how
    /// many. Each call writes the whole handover again from what the library holds now.
    ///
    /// A recording that cannot be read is still listed in the index, without a file of its
    /// own: the new app says it could not move it, rather than not knowing it existed.
    pub fn write_handover(&self) -> Result<u32, CoreError> {
        let dir = self.data_dir.join("handover");
        let recordings = dir.join("recordings");
        std::fs::create_dir_all(&recordings).map_err(internal)?;
        let query = SessionQuery {
            limit: Some(u32::MAX),
            ..SessionQuery::default()
        };
        let sessions = self
            .session_store
            .query_sessions(&query)
            .map_err(internal)?
            .sessions;
        // One whole-session document per notebook, read once.
        let mut documents: HashMap<String, Value> = HashMap::new();
        let mut listed = Vec::new();
        for session in &sessions {
            if vt_pipeline::require_session_id_path_component(&session.id).is_err() {
                continue;
            }
            let Some(run) = self
                .notebook_capture_store
                .get_run_for_session(&session.id)
                .map_err(internal)?
            else {
                continue; // not in any notebook: the library does not show it
            };
            let file = recordings.join(format!("{}.json", session.id));
            match self.handed(session, &run, &mut documents) {
                Ok(Some(handed)) => write_whole(&file, &handed)?,
                Ok(None) => continue, // not shown: its notebook is gone, or it is being purged
                Err(error) => {
                    tracing::warn!(session_id = %session.id, error = ?error, "handover: skipped");
                    remove_if_there(&file)?;
                }
            }
            listed.push(session.id.clone());
        }
        let index = json!({ "version": VERSION, "recordings": listed });
        write_whole(&dir.join("index.json"), &index)?;
        Ok(listed.len() as u32)
    }
}

impl ZuTalkCore {
    fn handed(
        &self,
        session: &SessionRecord,
        run: &NotebookCaptureRun,
        documents: &mut HashMap<String, Value>,
    ) -> Result<Option<Value>, CoreError> {
        let utterances = match self
            .notebook_capture_store
            .list_visible_notebook_utterances(&run.notebook_id, &session.id)
        {
            Ok(utterances) => utterances,
            Err(vt_store::NotebookCaptureStoreError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(internal(error)),
        };
        let profile: NotebookCaptureProfile =
            serde_json::from_str(&run.profile_snapshot_json).map_err(internal)?;
        let (spoken, shown) = languages(&profile);
        let (labels, speakers) = self.speakers(&session.id)?;
        let model = run.realtime_model_id.clone().unwrap_or_default();
        let live: Vec<Value> = utterances
            .iter()
            .filter_map(|utterance| live_line(utterance, &labels, &model))
            .collect();
        let mut takes = vec![live];
        if run.async_task_state == AsyncTaskState::Completed {
            let model = run.post_stop_model_id.clone().unwrap_or_default();
            let whole = segments(
                self.whole_session_document(run, documents)?,
                &session.id,
                &model,
            );
            if !whole.is_empty() {
                takes.push(whole);
            }
        }
        let source = if session.session_type == "import" {
            "file"
        } else {
            ""
        };
        let (started, note, audio) = (
            started_ms(&session.created_at)?,
            self.note(&session.id)?,
            self.audio(run)?,
        );
        Ok(Some(json!({
            "version": VERSION,
            "id": session.id,
            "about": {
                "started": started,
                "length_ms": session.duration_ms,
                "title": session.title,
                "spoken": spoken,
                "shown": shown,
                "source": source,
            },
            "takes": takes,
            "speakers": speakers,
            "note": note,
            "audio": audio,
        })))
    }

    /// Each speaker's label in the handover, by its id here, and the names the person gave.
    /// A label is the service's own; one heard again after a reconnect is told apart by the
    /// connection it came on.
    fn speakers(&self, session_id: &str) -> Result<HandoverSpeakers, CoreError> {
        let participants: HashMap<String, String> = self
            .notebook_capture_store
            .list_participants()
            .map_err(internal)?
            .into_iter()
            .map(|participant| (participant.id, participant.display_name))
            .collect();
        let (mut labels, mut names) = (HashMap::new(), BTreeMap::new());
        for speaker in self
            .notebook_capture_store
            .list_session_speakers(session_id)
            .map_err(internal)?
        {
            let mut label = key_safe(&speaker.provider_label);
            if speaker.provider_session_epoch > 0 {
                label = format!("{label}-{}", speaker.provider_session_epoch);
            }
            let name = speaker
                .local_display_name
                .clone()
                .or_else(|| {
                    speaker
                        .participant_id
                        .and_then(|id| participants.get(&id).cloned())
                })
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty());
            if let Some(name) = name {
                names.insert(label.clone(), name);
            }
            labels.insert(speaker.id, label);
        }
        Ok((labels, names))
    }

    /// The notebook's Async Transcript document as the editor reads it, with every mark.
    fn whole_session_document<'a>(
        &self,
        run: &NotebookCaptureRun,
        documents: &'a mut HashMap<String, Value>,
    ) -> Result<&'a Value, CoreError> {
        if !documents.contains_key(&run.notebook_id) {
            let tab = self
                .notebook_store
                .list_tabs(&run.notebook_id)
                .map_err(internal)?
                .into_iter()
                .find(|tab| {
                    tab.builtin_kind == BuiltinNotebookTab::AsyncTranscript
                        && tab.deleted_at.is_none()
                });
            let delta = match tab {
                Some(tab) => {
                    let (notebook, tab) = (run.notebook_id.clone(), tab.id);
                    self.open_editor(notebook.clone(), tab.clone())?;
                    let delta = self.get_editor_delta(notebook.clone(), tab.clone());
                    self.close_editor(notebook, tab)?;
                    serde_json::from_str(&delta?).map_err(internal)?
                }
                None => Value::Array(Vec::new()),
            };
            documents.insert(run.notebook_id.clone(), delta);
        }
        documents
            .get(&run.notebook_id)
            .ok_or_else(|| internal("handover: document"))
    }

    /// The session note as plain lines, then the note of each mark at its time.
    fn note(&self, session_id: &str) -> Result<String, CoreError> {
        let mut parts = Vec::new();
        let doc_id = session_note_document_id(session_id)?;
        if block_document_path(&self.data_dir, &doc_id)?.exists() {
            let open = self.block_documents.lock().unwrap().contains_key(&doc_id);
            self.session_note_block_document_open(session_id.to_string())?;
            let rows = self.note_outline_rows(doc_id.clone());
            if !open {
                self.block_document_close(doc_id)?;
            }
            let rows: Vec<String> = rows?
                .into_iter()
                .map(|row| format!("{}{}", "  ".repeat(row.depth as usize), row.text))
                .collect();
            let text = rows.join("\n");
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
        let marks = self.session_marks.list(session_id).map_err(internal)?;
        let marks: Vec<String> = marks
            .into_iter()
            .filter(|mark| !mark.note.trim().is_empty())
            .map(|mark| format!("[{}] {}", clock(mark.at_ms), mark.note.trim()))
            .collect();
        if !marks.is_empty() {
            parts.push(marks.join("\n"));
        }
        Ok(parts.join("\n\n"))
    }

    /// Where the recording's sealed audio is, and how it was stored; null when this Mac has
    /// none of it any more: destroyed, left to its retention, or recorded on another Mac.
    fn audio(&self, run: &NotebookCaptureRun) -> Result<Value, CoreError> {
        let (Some(key), Some(rate), Some(channels)) =
            (run.audio_key_ref.as_deref(), run.sample_rate, run.channels)
        else {
            return Ok(Value::Null);
        };
        if self.recorded_elsewhere(&run.session_id).is_some() || !self.key_store.key_exists(key) {
            return Ok(Value::Null);
        }
        let mut chunks = Vec::new();
        for chunk in self
            .session_meta
            .list_audio_retention_chunks(&run.session_id)
            .map_err(internal)?
            .into_iter()
            .filter(|chunk| !chunk.deleted)
        {
            let path = Path::new(&chunk.local_path)
                .strip_prefix(&self.data_dir)
                .map_err(|_| internal("handover: audio outside the data folder"))?;
            let path = path
                .to_str()
                .ok_or_else(|| internal("handover: audio path"))?;
            chunks.push(json!({ "path": path, "t_ms": chunk.start_ms }));
        }
        if chunks.is_empty() {
            return Ok(Value::Null);
        }
        Ok(json!({
            "key": key,
            "rate": rate,
            "channels": channels,
            "format": run.sample_format,
            "chunks": chunks,
        }))
    }
}

/// The languages spoken, and those shown: the ones a recording's profile selected, the
/// subtitle-only ones shown but not spoken.
fn languages(profile: &NotebookCaptureProfile) -> (Vec<String>, Vec<String>) {
    let shown = if profile.selected_languages.is_empty() {
        vec![
            profile.left_language.clone(),
            profile.right_language.clone(),
        ]
    } else {
        profile.selected_languages.clone()
    };
    let spoken = shown
        .iter()
        .filter(|language| !profile.subtitle_only_languages.contains(language))
        .cloned()
        .collect();
    (spoken, shown)
}

/// One live line as the reading page shows it, corrections in place. A line with no words
/// in any language is not a line.
fn live_line(
    utterance: &RealtimeUtterance,
    labels: &HashMap<String, String>,
    model: &str,
) -> Option<Value> {
    let source = utterance
        .variants
        .iter()
        .find(|variant| variant.role == UtteranceVariantRole::Source);
    let language = source.map_or(utterance.source_language.clone(), |v| v.language.clone());
    let text = source
        .and_then(|variant| variant.text.clone())
        .unwrap_or_else(|| utterance.source_text.clone());
    let by = |edited: bool| (if edited { "hand" } else { model }).to_string();
    let mut via = BTreeMap::from([("text".to_string(), by(utterance.source_edit_revision > 0))]);
    let mut translations = BTreeMap::new();
    for variant in utterance.variants.iter().filter(|variant| {
        variant.role == UtteranceVariantRole::Translation
            && variant.state == UtteranceVariantState::Ready
    }) {
        if let Some(text) = variant.text.clone().filter(|text| !text.trim().is_empty()) {
            translations.insert(variant.language.clone(), text);
            via.insert(variant.language.clone(), by(variant.edit_revision > 0));
        }
    }
    if let (true, Some(language), Some(text)) = (
        translations.is_empty(),
        utterance.translated_language.clone(),
        utterance.translated_text.clone(),
    ) {
        via.insert(language.clone(), model.to_string());
        translations.insert(language, text);
    }
    if text.trim().is_empty() && translations.is_empty() {
        return None;
    }
    let t0 = utterance.source_start_ms.unwrap_or(0);
    let t1 = utterance.source_end_ms.unwrap_or(t0).max(t0);
    let speaker = utterance
        .session_speaker_id
        .as_ref()
        .and_then(|id| labels.get(id).cloned())
        .unwrap_or_default();
    Some(json!({
        "t0": t0,
        "t1": t1,
        "speaker": speaker,
        "language": language,
        "text": text,
        "translations": translations,
        "incomplete": utterance.completion != UtteranceCompletion::Complete,
        "via": via,
    }))
}

/// The lines of one session in the whole-session document, as the reading page cuts them
/// (`NotebookTranscriptProjectionStore.parse`): runs marked with the same segment, one after
/// another, are one line; a rendered time header before it is not part of it.
fn segments(delta: &Value, session_id: &str, model: &str) -> Vec<Value> {
    struct Segment {
        id: String,
        start: Option<u64>,
        end: Option<u64>,
        language: Option<String>,
        speaker: Option<String>,
        text: String,
        user: bool,
        until: usize,
    }
    let number = |value: &Value| value.as_u64().or_else(|| value.as_str()?.parse().ok());
    let word = |value: &Value| {
        let word = value.as_str()?.trim();
        (!word.is_empty()).then(|| word.to_string())
    };
    let (mut found, mut at): (Vec<Segment>, usize) = (Vec::new(), 0);
    for op in delta.as_array().into_iter().flatten() {
        let insert = op["insert"].as_str().unwrap_or_default();
        let (from, attributes) = (at, &op["attributes"]);
        at += insert.chars().count();
        let id = match &attributes["segment_id"] {
            Value::String(id) => id.clone(),
            Value::Number(id) => id.to_string(),
            _ => continue,
        };
        if attributes["session_id"].as_str() != Some(session_id) {
            continue;
        }
        let start = number(&attributes["timestamp_ms"]).or_else(|| number(&attributes["start_ms"]));
        let end = number(&attributes["end_ms"]);
        let user = attributes["content_owner"].as_str() == Some("user");
        match found
            .last_mut()
            .filter(|last| last.id == id && last.until == from)
        {
            Some(last) => {
                last.text.push_str(insert);
                last.until = at;
                last.start = last.start.or(start);
                last.end = match (last.end, end) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                last.language = last
                    .language
                    .take()
                    .or_else(|| word(&attributes["source_language"]));
                last.speaker = last
                    .speaker
                    .take()
                    .or_else(|| word(&attributes["provider_speaker_label"]));
                last.user |= user;
            }
            None => found.push(Segment {
                id,
                start,
                end,
                language: word(&attributes["source_language"]),
                speaker: word(&attributes["provider_speaker_label"]),
                text: insert.to_string(),
                user,
                until: at,
            }),
        }
    }
    found
        .into_iter()
        .filter_map(|segment| {
            let mut text = segment.text.as_str();
            if let Some(start) = segment.start {
                text = text
                    .strip_prefix(&format!("[{}]\n", clock(start)))
                    .unwrap_or(text);
            }
            let text = text.trim();
            let t0 = segment.start.unwrap_or(0);
            let by = if segment.user { "hand" } else { model };
            (!text.is_empty()).then(|| {
                json!({
                    "t0": t0,
                    "t1": segment.end.unwrap_or(t0).max(t0),
                    "speaker": segment.speaker.as_deref().map(key_safe).unwrap_or_default(),
                    "language": segment.language.unwrap_or_else(|| "und".to_string()),
                    "text": text,
                    "translations": {},
                    "incomplete": false,
                    "via": { "text": by },
                })
            })
        })
        .collect()
}

/// `[mm:ss]`, or `[hh:mm:ss]` past an hour, as the reading page writes a time.
fn clock(ms: u64) -> String {
    let seconds = ms / 1_000;
    let (hours, minutes, seconds) = (seconds / 3_600, seconds % 3_600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// A label the new app can put in a record's key: letters, digits, `-` and `_`.
fn key_safe(label: &str) -> String {
    let safe: String = label
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        "unknown".to_string()
    } else {
        safe
    }
}

/// A session's `created_at` (SQLite's `YYYY-MM-DD HH:MM:SS`, UTC, or RFC 3339) in ms.
fn started_ms(created_at: &str) -> Result<i64, CoreError> {
    chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .map(|time| time.and_utc().timestamp_millis())
        .or_else(|_| {
            chrono::DateTime::parse_from_rfc3339(created_at).map(|time| time.timestamp_millis())
        })
        .map_err(|_| internal(format!("handover: created_at {created_at:?}")))
}

/// Written beside, made durable, then renamed into place: a reader finds the old file or the
/// new one, never half of one.
fn write_whole(path: &Path, value: &Value) -> Result<(), CoreError> {
    use std::io::Write;
    let new = path.with_extension("json.new");
    let bytes = serde_json::to_vec_pretty(value).map_err(internal)?;
    let mut file = std::fs::File::create(&new).map_err(internal)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(internal)?;
    std::fs::rename(&new, path).map_err(internal)
}

fn remove_if_there(path: &Path) -> Result<(), CoreError> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(internal(error)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn write_handover_exports_imported_session_and_audio_non_destructively() {
        let tmp = TempDir::new().unwrap();
        let core = ZuTalkCore::new(tmp.path().to_str().unwrap().to_string()).unwrap();

        assert_eq!(core.write_handover().unwrap(), 0);
        let index: Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("handover/index.json")).unwrap())
                .unwrap();
        assert_eq!(index["version"], 1);
        assert_eq!(index["recordings"].as_array().unwrap().len(), 0);

        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../vt-audio/tests/fixtures/test_16k_mono.wav");
        assert!(fixture.exists(), "checked-in import fixture is required");
        let notebook = core.create_notebook(Some("Handover Test".into())).unwrap();
        let imported = core
            .import_audio_into_notebook(fixture.to_str().unwrap().to_string(), notebook.id)
            .unwrap();

        assert_eq!(core.write_handover().unwrap(), 1);
        let index: Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("handover/index.json")).unwrap())
                .unwrap();
        assert_eq!(index["version"], 1);
        assert_eq!(index["recordings"], json!([imported.session_id]));

        let rec_path = tmp
            .path()
            .join("handover/recordings")
            .join(format!("{}.json", imported.session_id));
        let rec: Value = serde_json::from_slice(&std::fs::read(&rec_path).unwrap()).unwrap();
        assert_eq!(rec["version"], 1);
        assert_eq!(rec["id"], imported.session_id);
        assert_eq!(rec["about"]["source"], "file");
        assert!(rec["audio"]["key"].as_str().is_some_and(|k| !k.is_empty()));
        let chunks = rec["audio"]["chunks"].as_array().unwrap();
        assert!(!chunks.is_empty());
        for chunk in chunks {
            let rel = chunk["path"].as_str().unwrap();
            assert!(
                tmp.path().join(rel).is_file(),
                "handover audio chunk missing: {rel}"
            );
        }

        // Original 0.8 library remains intact.
        let sessions = core
            .query_sessions(Some("import".to_string()), None, None, None, None)
            .unwrap();
        assert_eq!(sessions.total_count, 1);
        assert_eq!(sessions.sessions[0].id, imported.session_id);
    }
}
