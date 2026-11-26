//! Composing bounded live caption frames from lane events (`P1.md` §3.4).
//!
//! Values in, values out: no IO, no clock, no log events. Finished lines carry no reveal units
//! and are converted once when they arrive; only a changed `tail` runs display-unit segmentation.

use crate::live::lane::{self, Link, Stream};
use crate::live::lang::profile;
use crate::live::lines::Line;
use crate::rt::water::Health;
use serde::Serialize;
use ts_rs::TS;

/// Finished lines a column keeps: what the captions window draws (`P1.md` §3.4).
pub const KEEP_LINES: usize = 6;

/// Most caption columns one session shows at once, matching the design (`P1.md` §3.4).
pub const MAX_COLUMNS: usize = 4;

/// Maximum serialized JSON size of a frame: Tauri channels inject up to 8 KB directly (`P1.md` §3.4).
pub const MAX_BYTES: usize = 8 * 1024;

/// Fixed JSON wrapper overhead for `Frame` (`{"t_audio":4294967295,"columns":[]}` is 35 B).
const FRAME_OVERHEAD_BYTES: usize = 40;
/// Fixed JSON wrapper overhead per `Column` (keys, punctuation, finite `f32`, `null`, `Health` tag <= 93 B).
const COLUMN_OVERHEAD_BYTES: usize = 128;
/// Fixed JSON wrapper overhead per `Caption` (`,{"speaker":,"text":,"units":[]}` is 32 B).
const CAPTION_OVERHEAD_BYTES: usize = 36;
/// Worst-case JSON bytes per `u16` entry in `Caption.units` (5 digits + comma).
const UNIT_MOST_BYTES: usize = 6;
/// UTF-8 and JSON byte width of the leading ellipsis `"…"` (`U+2026`) placed on a truncated tail.
const ELLIPSIS_BYTES: usize = "…".len();

/// The one sentence the interface says about the recording under way. Audio only: sound is
/// kept, there is no text, for the reason given as a code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "is")]
pub enum Status {
    Recording,
    Paused,
    AudioOnly { why: String },
    Connecting,
    Behind,
}

impl Status {
    /// Stable plain code for structured log events (`P1.md` §3.13.11).
    pub const fn code(&self) -> &'static str {
        match self {
            Status::Recording => "Recording",
            Status::Paused => "Paused",
            Status::AudioOnly { .. } => "AudioOnly",
            Status::Connecting => "Connecting",
            Status::Behind => "Behind",
        }
    }

    /// Reason code carried by `AudioOnly`, or `""` for every other status.
    pub fn why(&self) -> &str {
        match self {
            Status::AudioOnly { why } => why.as_str(),
            Status::Recording | Status::Paused | Status::Connecting | Status::Behind => "",
        }
    }
}

/// One caption, cut where it may be revealed: `units` are where each one ends, as UTF-16
/// offsets in `text` (only on a tail).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Caption {
    pub speaker: String,
    pub text: String,
    pub units: Vec<u16>,
}

/// One language's captions: the last lines said, the one still being said (revealed at
/// `rate` units a second), and how the column is doing.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Column {
    pub language: String,
    pub rate: f32,
    pub lines: Vec<Caption>,
    pub tail: Option<Caption>,
    pub health: Health,
}

/// One bounded snapshot of live captions up to `t_audio` ms on the capture clock.
#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Frame {
    pub t_audio: u32,
    pub columns: Vec<Column>,
}

/// What had to give way for a frame to fit within [`MAX_BYTES`] (`P1.md` §3.4, §3.13.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Whole,
    Lines,
    Units,
    Tail,
}

impl Fit {
    /// Stable plain code for structured log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Fit::Whole => "Whole",
            Fit::Lines => "Lines",
            Fit::Units => "Units",
            Fit::Tail => "Tail",
        }
    }
}

/// The sentence the interface says about a recording under way (`P1.md` §3.4, `P1-17`).
pub fn status(paused: bool, columns: &[Health], links: &[Link], quiet: &str) -> Status {
    if paused {
        return Status::Paused;
    }
    if columns.is_empty()
        || columns.iter().all(|h| matches!(h, Health::Stalled { .. } | Health::Dead { .. }))
    {
        let why = columns
            .iter()
            .find_map(|h| match h {
                Health::Stalled { why } | Health::Dead { why } if !why.is_empty() => {
                    Some(why.as_str())
                }
                _ => None,
            })
            .unwrap_or(quiet);
        return Status::AudioOnly { why: why.into() };
    }
    if links.contains(&Link::Connecting) {
        return Status::Connecting;
    }
    if columns.iter().any(|h| matches!(h, Health::Slow)) {
        return Status::Behind;
    }
    Status::Recording
}

#[derive(Clone)]
struct ColState {
    language: String,
    stream: usize,
    own: bool,
    rate: f32,
    said: Vec<Caption>,
    open_said: Vec<Caption>,
    tail: Option<Caption>,
    tail_written: String,
}

impl ColState {
    fn new(language: String, stream: usize, own: bool) -> Self {
        let rate = profile(&language).reveal_per_s;
        Self {
            language,
            stream,
            own,
            rate,
            said: Vec::new(),
            open_said: Vec::new(),
            tail: None,
            tail_written: String::new(),
        }
    }

    fn take_lines(&mut self, lines: &[Line]) {
        let start = lines.len().saturating_sub(KEEP_LINES);
        for line in &lines[start..] {
            let text = match line.translations.get(&self.language) {
                Some(translated) => translated.clone(),
                None => line.text.clone(),
            };
            self.said.push(Caption { speaker: line.speaker.clone(), text, units: Vec::new() });
        }
        if self.said.len() > KEEP_LINES {
            self.said.drain(..self.said.len() - KEEP_LINES);
        }
    }

    fn take_tail(&mut self, open: &[Line]) {
        let mut visible: Vec<(&Line, &str, &str)> = open
            .iter()
            .filter_map(|line| match line.translations.get(&self.language) {
                Some(text) => Some((line, text.as_str(), self.language.as_str())),
                None if self.own || line.language == self.language => {
                    Some((line, line.text.as_str(), line.language.as_str()))
                }
                None => None,
            })
            .collect();

        let last = visible.pop();
        let skip = visible.len().saturating_sub(KEEP_LINES);
        self.open_said = visible[skip..]
            .iter()
            .map(|(line, text, _)| Caption {
                speaker: line.speaker.clone(),
                text: (*text).to_owned(),
                units: Vec::new(),
            })
            .collect();

        match last {
            Some((line, text, written)) => {
                let same_units = self
                    .tail
                    .as_ref()
                    .is_some_and(|prev| prev.text == text && self.tail_written == written);
                if same_units {
                    if let Some(prev) = &mut self.tail
                        && prev.speaker != line.speaker
                    {
                        prev.speaker.clone_from(&line.speaker);
                    }
                } else {
                    let units = profile(written).units(text);
                    self.tail = Some(Caption {
                        speaker: line.speaker.clone(),
                        text: text.to_owned(),
                        units,
                    });
                    self.tail_written.clear();
                    self.tail_written.push_str(written);
                }
            }
            None => {
                self.tail = None;
                self.tail_written.clear();
            }
        }
    }

    fn build(&self, health: Health) -> Column {
        let total = self.said.len() + self.open_said.len();
        let drop_count = total.saturating_sub(KEEP_LINES);
        let lines: Vec<Caption> =
            self.said.iter().chain(self.open_said.iter()).skip(drop_count).cloned().collect();
        Column {
            language: self.language.clone(),
            rate: self.rate,
            lines,
            tail: self.tail.clone(),
            health,
        }
    }
}

/// Composes bounded [`Frame`]s incrementally: cost depends only on what changed since the last frame.
pub struct Composer {
    cols: Vec<ColState>,
    links: Vec<Link>,
    judged: Vec<Health>,
    t_audio: u32,
    paused: bool,
    dirty: bool,
    bound_bytes: usize,
}

impl Composer {
    pub fn new(shown: &[String], streams: Vec<Stream>) -> Self {
        let n = streams.len();
        let cols: Vec<ColState> = if streams.is_empty() {
            Vec::new()
        } else {
            shown
                .iter()
                .take(MAX_COLUMNS)
                .enumerate()
                .filter_map(|(j, lang)| {
                    let s = j.saturating_sub(1);
                    let kind = streams.get(s)?;
                    Some(ColState::new(lang.clone(), s, *kind == Stream::Transcribe))
                })
                .collect()
        };
        let col_count = cols.len();
        Self {
            cols,
            links: vec![Link::Connecting; n],
            judged: vec![Health::Live; col_count],
            t_audio: 0,
            paused: false,
            dirty: true,
            bound_bytes: 0,
        }
    }

    pub fn take(&mut self, event: lane::Event) {
        match event {
            lane::Event::Lines { stream, lines } => {
                if stream >= self.links.len() {
                    return;
                }
                for line in &lines {
                    self.t_audio = self.t_audio.max(line.t1);
                }
                for col in &mut self.cols {
                    if col.stream == stream {
                        col.take_lines(&lines);
                    }
                }
            }
            lane::Event::Tail { stream, tail } => {
                if stream >= self.links.len() {
                    return;
                }
                for line in &tail {
                    self.t_audio = self.t_audio.max(line.t1);
                }
                for col in &mut self.cols {
                    if col.stream == stream {
                        col.take_tail(&tail);
                    }
                }
            }
            lane::Event::Link { stream, state } => {
                if stream >= self.links.len() {
                    return;
                }
                self.links[stream] = state;
            }
            lane::Event::Gap { stream, t1, .. } => {
                if stream >= self.links.len() {
                    return;
                }
                self.t_audio = self.t_audio.max(t1);
            }
        }
        self.dirty = true;
    }

    pub fn pause(&mut self, paused: bool) {
        if self.paused != paused {
            self.paused = paused;
            self.dirty = true;
        }
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Supplies per-column health judged by the watcher (`P1-17`).
    pub fn judged(&mut self, health: Vec<Health>) {
        if self.judged != health {
            self.judged = health;
            self.dirty = true;
        }
    }

    /// Current per-column health as judged by the watcher.
    pub fn health(&self) -> &[Health] {
        &self.judged
    }

    /// Emits the next bounded [`Frame`] and its [`Fit`], or `None` when nothing changed.
    pub fn frame(&mut self) -> Option<(Frame, Fit)> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let columns = self
            .cols
            .iter()
            .enumerate()
            .map(|(j, col)| {
                let health = self.judged.get(j).cloned().unwrap_or(Health::Live);
                col.build(health)
            })
            .collect();
        let raw = Frame { t_audio: self.t_audio, columns };
        self.bound_bytes = frame_bound_bytes(&raw);
        Some(fit_with_bound(raw, self.bound_bytes))
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    /// Cheap byte-size upper bound of the most recently composed frame before [`fit`] trimmed it.
    pub fn bound_bytes(&self) -> usize {
        self.bound_bytes
    }
}

/// Trims `frame` in priority order so its JSON encoding never exceeds [`MAX_BYTES`] (`P1.md` §3.4).
pub fn fit(frame: Frame) -> (Frame, Fit) {
    let bound = frame_bound_bytes(&frame);
    fit_with_bound(frame, bound)
}

fn fit_with_bound(mut frame: Frame, bound: usize) -> (Frame, Fit) {
    if bound <= MAX_BYTES || json_bytes(&frame) <= MAX_BYTES {
        return (frame, Fit::Whole);
    }

    // 1. Drop the oldest finished line from whichever column currently takes the most space.
    while frame.columns.iter().any(|c| !c.lines.is_empty()) {
        let fullest = frame
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.lines.is_empty())
            .max_by_key(|(_, c)| (column_bound_bytes(c), c.lines.len()))
            .map(|(idx, _)| idx);
        let Some(idx) = fullest else { break };
        frame.columns[idx].lines.remove(0);
        if json_bytes(&frame) <= MAX_BYTES {
            return (frame, Fit::Lines);
        }
    }

    // 2. Strip reveal units from all tails while keeping their text whole.
    for col in &mut frame.columns {
        if let Some(tail) = &mut col.tail {
            tail.units.clear();
        }
    }
    if json_bytes(&frame) <= MAX_BYTES {
        return (frame, Fit::Units);
    }

    // 3. Truncate the longest tail texts from the front, prefixing `"…"`, keeping latest words.
    let originals: Vec<Option<String>> =
        frame.columns.iter().map(|c| c.tail.as_ref().map(|t| t.text.clone())).collect();
    let max_orig = originals
        .iter()
        .flatten()
        .map(|s| json_content_bytes(s))
        .max()
        .unwrap_or(ELLIPSIS_BYTES)
        .max(ELLIPSIS_BYTES);

    let mut lo = ELLIPSIS_BYTES;
    let mut hi = max_orig.min(MAX_BYTES);
    let mut best = ELLIPSIS_BYTES;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        apply_tail_budget(&mut frame, &originals, mid);
        if json_bytes(&frame) <= MAX_BYTES {
            best = mid;
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    apply_tail_budget(&mut frame, &originals, best);

    // Pathological caller safety net (excessive columns or non-text metadata).
    while json_bytes(&frame) > MAX_BYTES {
        if frame.columns.pop().is_none() {
            break;
        }
    }

    (frame, Fit::Tail)
}

fn apply_tail_budget(frame: &mut Frame, originals: &[Option<String>], budget_bytes: usize) {
    for (col, orig) in frame.columns.iter_mut().zip(originals.iter()) {
        if let (Some(tail), Some(orig_text)) = (&mut col.tail, orig) {
            tail.text = truncate_front_json(orig_text, budget_bytes);
        }
    }
}

fn truncate_front_json(orig: &str, max_json_bytes: usize) -> String {
    if json_content_bytes(orig) <= max_json_bytes {
        return orig.to_owned();
    }
    if max_json_bytes <= ELLIPSIS_BYTES {
        return "…".to_owned();
    }
    let room = max_json_bytes - ELLIPSIS_BYTES;
    let mut used = 0usize;
    let mut start = orig.len();
    for (idx, ch) in orig.char_indices().rev() {
        let cost = char_json_bytes(ch);
        if used + cost > room {
            break;
        }
        used += cost;
        start = idx;
    }
    format!("…{}", &orig[start..])
}

fn json_bytes(frame: &Frame) -> usize {
    serde_json::to_vec(frame).map_or(usize::MAX, |v| v.len())
}

fn frame_bound_bytes(frame: &Frame) -> usize {
    FRAME_OVERHEAD_BYTES + frame.columns.iter().map(column_bound_bytes).sum::<usize>()
}

fn column_bound_bytes(col: &Column) -> usize {
    let captions: usize = col.lines.iter().chain(col.tail.iter()).map(caption_bound_bytes).sum();
    COLUMN_OVERHEAD_BYTES
        + json_str_bytes(&col.language)
        + json_str_bytes(col.health.why())
        + captions
}

fn caption_bound_bytes(cap: &Caption) -> usize {
    CAPTION_OVERHEAD_BYTES
        + json_str_bytes(&cap.speaker)
        + json_str_bytes(&cap.text)
        + cap.units.len() * UNIT_MOST_BYTES
}

fn json_str_bytes(s: &str) -> usize {
    2 + json_content_bytes(s)
}

fn json_content_bytes(s: &str) -> usize {
    let extra: usize = s
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 1,
            0x00..=0x1f => 5,
            _ => 0,
        })
        .sum();
    s.len() + extra
}

fn char_json_bytes(ch: char) -> usize {
    let b = ch as u32;
    match b {
        0x22 | 0x5c | 0x08 | 0x0c | 0x0a | 0x0d | 0x09 => 2,
        0x00..=0x1f => 6,
        _ => ch.len_utf8(),
    }
}
