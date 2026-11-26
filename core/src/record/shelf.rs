//! The library. A recording is a folder of Zu records, and nothing in it is ever rewritten:
//! whatever a person changes — a word, the title, a speaker's name, the recording's removal —
//! is one more record at the same path, pointing at the one it replaces.

use crate::live::lang::profile;
use crate::live::lines::Line;
use crate::record::notes::{self, Note};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use ts_rs::TS;
use zu::{Cid, Error, Held, Pack, Path, Record, SecretKey, Value, record};

// The types ZuTalk writes. A key is the recording's id, then what it names within it.
pub const RECORDING: &str = "talk/recording"; // key: <id>
pub const LINE: &str = "talk/line"; // key: <id>.<take>.<number>
pub const SPEAKER: &str = "talk/speaker"; // key: <id>.<label>
pub const NOTE: &str = "talk/note"; // key: <id>
pub const TERM: &str = "talk/term"; // key: drawn from how the term is said; kept apart too
pub const TOPIC: &str = "talk/topic"; // key: <topic_id> ("always" for pinned topic); kept apart in topics.jsonl
pub const FILING: &str = "talk/filing"; // key: <id>; where it went to a city, kept apart too

/// Where a recording's current title came from (`P6.md` §3.2, `Titles.dc.html`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum TitleBy {
    #[default]
    None,
    Hand,
    Event,
    File,
    Ai,
    Opening,
}

/// One concise topic tag with a jump timestamp into the transcript (`P6.md` §3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct ContentTag {
    pub label: String,
    pub at_ms: u32,
}

/// One selectable title candidate stored alongside a recording (`P6.md` §3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct TitleCandidate {
    pub by: TitleBy,
    pub text: String,
}

/// What a recording is, apart from what was said in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct About {
    pub started: i64, // wall clock, in ms since 1970
    pub length_ms: u32,
    pub title: String,
    pub preview: String, // its first words, for a list
    pub spoken: Vec<String>,
    pub shown: Vec<String>, // the languages its text is given in
    pub source: String,
    /// Which transcript is the recording's text: 0 is the live draft, each later take a
    /// whole-session transcription. Lines of other takes stay in the pack, unshown.
    pub take: u32,
    pub moved: bool, // from the 0.8 app, with the transcript it had there
    pub topics: Vec<String>,
    pub tags: Vec<ContentTag>,
    pub title_by: TitleBy,
    pub title_candidates: Vec<TitleCandidate>,
}

/// A recording's first words, for a list.
pub fn preview(lines: &[Said]) -> String {
    let words: Vec<&str> = lines.iter().map(|said| said.line.text.as_str()).collect();
    words.join(" ").chars().take(80).collect()
}

/// A line as it is kept, with where each piece of its text came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct Said {
    #[serde(flatten)]
    pub line: Line,
    /// `text` and each translation's language, to a service, a model, or `hand`.
    pub via: BTreeMap<String, String>,
}

/// A speaker's name, or a note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Words {
    pub text: String,
}

/// A recording as it reads now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recording {
    pub id: String,
    pub about: About,
    pub lines: Vec<(String, Said)>, // each with its key, in the order they were said
    pub speakers: BTreeMap<String, String>,
    pub note: String,
    pub notes: Vec<Note>,
}

pub struct Shelf {
    root: PathBuf,
    by: String,
    key: Arc<SecretKey>,
}

/// One pack open for adding records of one type. What each path says now is known, so a
/// record added follows the ones it replaces without the pack being read again: the live
/// draft adds its lines one by one, however long the recording runs.
pub struct Writer {
    pack: Pack,
    by: String,
    ty: String,
    key: Arc<SecretKey>,
    heads: BTreeMap<Path, (Vec<Cid>, i64)>, // a path's current records, and the soonest after them
}

impl Writer {
    /// One more record at `key`: `body` becomes what the path says, and `None` removes it.
    pub fn put<T: Serialize>(
        &mut self,
        key: &str,
        body: Option<&T>,
        now: i64,
    ) -> Result<(), Error> {
        let path = Path::new(&self.by, &self.ty, key)?;
        // record.md § Time: a record is later than every record it follows.
        let (prev, after) = self.heads.remove(&path).unwrap_or((Vec::new(), i64::MIN));
        let at = now.max(after);
        let json = body.map(serde_json::to_string).transpose().map_err(|_| Error::Json)?;
        let body = json.map(|json| self.pack.keep(&Value::from_json(&json)?)).transpose()?;
        let record = Record::sign(&self.key, path.clone(), body, prev, at, None)?;
        self.pack.append(&record)?;
        self.heads.insert(path, (vec![record.cid()], at + 1));
        Ok(())
    }

    /// Waits until every record so far is on the disk.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.pack.sync()
    }
}

/// A draft pack being written, line by line.
pub trait Draft: Send {
    fn put(&mut self, key: &str, said: &Said, now: i64) -> Result<(), Error>;
    fn sync(&mut self) -> Result<(), Error>;
}

impl Draft for Writer {
    fn put(&mut self, key: &str, said: &Said, now: i64) -> Result<(), Error> {
        Writer::put(self, key, Some(said), now)
    }

    fn sync(&mut self) -> Result<(), Error> {
        Writer::sync(self)
    }
}

/// Append-only writer for live draft lines (`<id>.0.<seq>`). Keeps O(1) memory across hours of
/// recording by omitting per-path head maps since draft line keys are strictly unique (`P2.md` §3.3.3).
pub struct DraftWriter {
    pack: Pack,
    by: String,
    key: Arc<SecretKey>,
    last_at: i64,
}

impl Draft for DraftWriter {
    fn put(&mut self, key: &str, said: &Said, now: i64) -> Result<(), Error> {
        let path = Path::new(&self.by, LINE, key)?;
        let at = now.max(self.last_at);
        let json = serde_json::to_string(said).map_err(|_| Error::Json)?;
        let body = self.pack.keep(&Value::from_json(&json)?)?;
        let record = Record::sign(&self.key, path, Some(body), Vec::new(), at, None)?;
        self.pack.append(&record)?;
        self.last_at = at + 1;
        Ok(())
    }

    fn sync(&mut self) -> Result<(), Error> {
        self.pack.sync()
    }
}

/// Opens a recording's live draft for writing.
pub type Drafts = Arc<dyn Fn(&Shelf, &str) -> Result<Box<dyn Draft>, Error> + Send + Sync>;

/// The real draft port: writes `talk/line` records directly onto the shelf with O(1) writer state.
pub fn drafts() -> Drafts {
    Arc::new(|shelf, id| Ok(Box::new(shelf.open_draft(id)?) as Box<dyn Draft>))
}

/// The range and count of consecutive draft lines dropped while the draft queue was full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapSpan {
    pub t0_ms: u32,
    pub t1_ms: u32,
    pub lines: u64,
}

/// What the caller should try to enqueue next when a finalized draft line arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftStep {
    /// No pending gap: offer the incoming line directly.
    Line,
    /// A drop streak is open: offer a gap line covering `GapSpan` before the incoming line.
    GapFirst(GapSpan),
}

/// Outcome of recording a dropped draft line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Missed {
    /// `true` on the first dropped line of a new streak (caller notifies `draft.behind` once).
    pub first: bool,
}

/// Pure value type tracking consecutive draft lines dropped on a full queue and collapsing them
/// into one incomplete gap line (`…`) once the queue accepts again (`P1.md` §3.7).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DraftGap {
    span: Option<GapSpan>,
}

impl DraftGap {
    pub fn new() -> Self {
        Self { span: None }
    }

    /// Pending gap span, if a drop streak is open.
    pub fn pending(&self) -> Option<GapSpan> {
        self.span
    }

    /// Decides whether a gap line must be offered before the incoming line.
    pub fn next(&self) -> DraftStep {
        match self.span {
            Some(span) => DraftStep::GapFirst(span),
            None => DraftStep::Line,
        }
    }

    /// Clears and returns the pending gap once its gap line has been accepted by the queue.
    pub fn gap_sent(&mut self) -> Option<GapSpan> {
        self.span.take()
    }

    /// Records a line (`t0_ms..t1_ms`) that could not enter the draft queue.
    pub fn missed(&mut self, t0_ms: u32, t1_ms: u32) -> Missed {
        match &mut self.span {
            None => {
                self.span = Some(GapSpan { t0_ms, t1_ms, lines: 1 });
                Missed { first: true }
            }
            Some(span) => {
                span.t0_ms = span.t0_ms.min(t0_ms);
                span.t1_ms = span.t1_ms.max(t1_ms);
                span.lines += 1;
                Missed { first: false }
            }
        }
    }

    /// Builds the incomplete gap line (`…`) representing `span`.
    pub fn gap_line(span: GapSpan) -> Line {
        Line {
            t0: span.t0_ms,
            t1: span.t1_ms,
            text: "…".into(),
            incomplete: true,
            ..Line::default()
        }
    }
}

/// A pack that is not there holds nothing.
fn held(path: PathBuf) -> Result<Held, Error> {
    match Pack::read(path) {
        Err(Error::Io(ErrorKind::NotFound)) => Ok(Held::default()),
        held => held,
    }
}

/// What a path says now: the body of the record it refers to, read as its type.
fn says<T: DeserializeOwned>(held: &Held, current: &[&Record]) -> Result<Option<T>, Error> {
    let Some(body) = current.first().and_then(|r| r.body).and_then(|cid| held.bodies.get(&cid))
    else {
        return Ok(None);
    };
    serde_json::from_str(&body.to_json()?).map(Some).map_err(|_| Error::Json)
}

impl Shelf {
    /// `by` is the DID whose records these are, and `key` one of its signing keys.
    pub fn new(root: impl Into<PathBuf>, by: &str, key: Arc<SecretKey>) -> Self {
        Self { root: root.into(), by: by.into(), key }
    }

    /// The DID signing records on this shelf.
    pub fn by(&self) -> &str {
        &self.by
    }

    /// The root directory holding this shelf's packs.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Path to the pack file of recording `id` for record type `ty`.
    pub fn pack_path(&self, id: &str, ty: &str) -> PathBuf {
        self.pack(id, ty)
    }

    /// Lines are many and the rest is little, so each has a pack: a list reads the little.
    /// What is the person's, not a recording's (their glossary, filings), has its own.
    fn pack(&self, id: &str, ty: &str) -> PathBuf {
        match ty {
            LINE => self.root.join(id).join("lines.jsonl"),
            TERM => self.root.join("terms.jsonl"),
            TOPIC => self.root.join("topics.jsonl"),
            FILING => self.root.join("filings.jsonl"),
            _ => self.root.join(id).join("about.jsonl"),
        }
    }

    /// Opens `<id>/lines.jsonl` for append-only live draft writing without retaining per-path heads.
    pub fn open_draft(&self, id: &str) -> Result<DraftWriter, Error> {
        let pack = self.pack(id, LINE);
        fs::create_dir_all(pack.parent().unwrap_or(&self.root))?;
        let (pack, held) = Pack::open(pack)?;
        let last_at = held.records.iter().map(|record| record.at + 1).max().unwrap_or(i64::MIN);
        drop(held);
        Ok(DraftWriter { pack, by: self.by.clone(), key: self.key.clone(), last_at })
    }

    /// The pack of recording `id` that holds type `ty`, open for adding records.
    pub fn open(&self, id: &str, ty: &str) -> Result<Writer, Error> {
        let pack = self.pack(id, ty);
        fs::create_dir_all(pack.parent().unwrap_or(&self.root))?;
        let (pack, held) = Pack::open(pack)?;
        let heads = record::paths(&held.records).into_iter().map(|(path, records)| {
            let after = records.iter().map(|record| record.at + 1).max().unwrap_or(i64::MIN);
            (path.clone(), (records.iter().map(|record| record.cid()).collect(), after))
        });
        let (by, ty, key) = (self.by.clone(), ty.to_string(), self.key.clone());
        Ok(Writer { pack, by, ty, key, heads: heads.collect() })
    }

    /// One more record at each of these paths of one type: the body becomes what the path
    /// says, and `None` removes it. This is every change there is.
    pub fn put<T: Serialize>(
        &self,
        id: &str,
        ty: &str,
        bodies: &[(String, Option<T>)],
        now: i64,
    ) -> Result<(), Error> {
        let pack_path = self.pack(id, ty);
        fs::create_dir_all(pack_path.parent().unwrap_or(&self.root))?;
        let key_set: std::collections::BTreeSet<&str> =
            bodies.iter().map(|(k, _)| k.as_str()).collect();
        let (pack, matching_records) = Pack::open_for_keys(&pack_path, &key_set)?;
        let heads = record::paths(&matching_records).into_iter().map(|(path, records)| {
            let after = records.iter().map(|record| record.at + 1).max().unwrap_or(i64::MIN);
            (path.clone(), (records.iter().map(|record| record.cid()).collect(), after))
        });
        let mut writer = Writer {
            pack,
            by: self.by.clone(),
            ty: ty.to_string(),
            key: self.key.clone(),
            heads: heads.collect(),
        };
        bodies.iter().try_for_each(|(key, body)| writer.put(key, body.as_ref(), now))?;
        writer.sync()?;
        drop(writer);
        if ty != LINE && fs::metadata(&pack_path).is_ok_and(|m| m.len() > 32 * 1024) {
            let _ = Pack::compact(&pack_path); // dropped: opportunistic body GC; headers and live bodies stay intact
        }
        Ok(())
    }

    /// What the person keeps apart from their recordings, by key: the rooms they have and what
    /// each carries (`SHARE`), their glossary (`TERM`), where their recordings went to a city
    /// (`FILING`).
    pub fn kept<T: DeserializeOwned>(&self, ty: &str) -> Result<Vec<(String, T)>, Error> {
        let held = held(self.pack("", ty))?;
        let mut kept = Vec::new();
        for (path, current) in record::paths(&held.records) {
            kept.extend(says::<T>(&held, &current)?.map(|said| (path.key.clone(), said)));
        }
        Ok(kept)
    }

    /// Undoes a removal: the recording says again what it said last.
    pub fn restore(&self, id: &str, now: i64) -> Result<(), Error> {
        let held = held(self.pack(id, RECORDING))?;
        let path = Path::new(&self.by, RECORDING, id)?;
        let said = held.records.iter().filter(|r| r.path == path && r.body.is_some());
        let last: Vec<&Record> = said.max_by_key(|record| record.at).into_iter().collect();
        let about = says::<About>(&held, &last)?.ok_or(Error::Field("body"))?;
        self.put(id, RECORDING, &[(id.to_string(), Some(about))], now)
    }

    /// Every key ever written for person-level record type `ty`, removed since or not.
    pub fn known_kept(&self, ty: &str) -> Result<BTreeSet<String>, Error> {
        let held = held(self.pack("", ty))?;
        Ok(held.records.into_iter().filter(|r| r.path.ty == ty).map(|r| r.path.key).collect())
    }

    /// Whether recording `id` was ever written here, removed since or not.
    pub fn known(&self, id: &str) -> Result<bool, Error> {
        let held = held(self.pack(id, RECORDING))?;
        Ok(held.records.iter().any(|r| r.path.ty == RECORDING && r.path.key == id))
    }

    /// The recording without its lines, which is what a list shows. `None` if it was
    /// removed, or never written.
    pub fn about(&self, id: &str) -> Result<Option<Recording>, Error> {
        let held = held(self.pack(id, RECORDING))?;
        let mut recording = None;
        let mut speakers = BTreeMap::new();
        for (path, current) in record::paths(&held.records) {
            let label = path.key.strip_prefix(id).and_then(|rest| rest.strip_prefix('.'));
            match (path.ty.as_str(), label) {
                (RECORDING, _) => recording = says::<About>(&held, &current)?,
                (SPEAKER, Some(label)) => {
                    let name = says::<Words>(&held, &current)?;
                    speakers.extend(name.map(|name| (label.to_string(), name.text)));
                }
                _ => {} // notes are parsed below; unknown types are kept, not shown
            }
        }
        let (note, notes) = notes::from_held(id, &held)?;
        let id = id.to_string();
        Ok(recording.map(|about| Recording { id, about, lines: Vec::new(), speakers, note, notes }))
    }

    /// The recording as it reads now.
    pub fn read(&self, id: &str) -> Result<Option<Recording>, Error> {
        let Some(mut recording) = self.about(id)? else { return Ok(None) };
        let held = held(self.pack(id, LINE))?;
        let take = format!("{id}.{}.", recording.about.take);
        for (path, current) in record::paths(&held.records) {
            // Keys of one take sort in the order the lines were said.
            let said = if path.key.starts_with(&take) { says(&held, &current)? } else { None };
            recording.lines.extend(said.map(|said| (path.key.clone(), said)));
        }
        Ok(Some(recording))
    }

    /// Every recording that has not been removed, the newest first.
    pub fn list(&self) -> Result<Vec<Recording>, Error> {
        let mut all = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(all), // nothing kept yet
            entries => entries?,
        };
        for entry in entries {
            let entry = entry?;
            if let (true, Some(id)) = (entry.file_type()?.is_dir(), entry.file_name().to_str()) {
                all.extend(self.about(id)?);
            }
        }
        all.sort_by_key(|recording| Reverse(recording.about.started));
        Ok(all)
    }

    /// The lines that hold `query`, in what was said or in any translation: each as the
    /// recording's id and the line's key.
    pub fn search(&self, query: &str) -> Result<Vec<(String, String)>, Error> {
        let query = profile("").key(query);
        let mut hits = Vec::new();
        for recording in self.list()? {
            let lines = self.read(&recording.id)?.map(|r| r.lines).unwrap_or_default();
            for (key, said) in lines {
                let spoken = [(&said.line.language, &said.line.text)];
                let mut texts = spoken.into_iter().chain(&said.line.translations);
                if texts.any(|(lang, text)| profile(lang).key(text).contains(&query)) {
                    hits.push((recording.id.clone(), key));
                }
            }
        }
        Ok(hits)
    }
}
