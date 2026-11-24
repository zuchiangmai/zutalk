//! What crosses between the core and the interface: one state, sent whole whenever it changes;
//! live captions, sent as they come; commands, going the other way. The interface holds
//! nothing of its own but what is being typed. The TypeScript is written by `cargo test`.

pub use crate::capture::audio::Device;
pub use crate::id::keys::From;
use crate::knowledge::glossary::Term;
pub use crate::live::frame::{Caption, Column, Frame, Status};
use crate::out::city::{Calendar, Filing};
use crate::record::shelf::Said;
use crate::rt::stats::Tally;
use crate::rt::water::Water;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use ts_rs::TS;

/// Everything on screen that is not live text or a signal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct State {
    pub me: Option<String>,     // the DID signed in on this device
    pub login: Option<String>,  // a sign-in under way: the address to show as a code
    pub rotation_unsaved: bool, // the identity made here has a rotation key not saved elsewhere
    pub update: Option<String>, // a newer version of the app, downloaded and ready to install
    pub keys: Vec<(String, From)>,
    #[ts(type = "number | null")]
    pub left: Option<u64>, // µ$ of granted quota left, as last heard
    pub setup: Setup,
    pub devices: Vec<Device>,
    pub languages: Vec<String>, // the ones there are rules for
    pub exporters: Vec<String>,
    pub library: Vec<Row>,
    pub open: Option<Open>,
    pub hits: Vec<(String, String)>, // a recording's id and a line's key
    pub rooms: Vec<Room>,
    pub terms: Vec<Term>,               // the person's glossary
    pub city: Option<Calendar>,         // the calendar of the city last asked for
    pub writing: Vec<Writing>,          // transcripts on their way, after their stop
    pub filing: Vec<Writing>,           // recordings on their way to a city
    pub moving: Option<Writing>,        // the 0.8 library being moved in: which recording, how far
    pub unmoved: Vec<(String, String)>, // what 0.8 handed over that did not move: title, why
    pub captions: bool,                 // the captions window is on screen
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Writing {
    pub id: String,
    pub step: String,
    pub done: u32,
    pub of: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum Sound {
    #[default]
    Mic,
    System,
    Both,
}

/// How the captions window lays its lines out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum Mode {
    #[default]
    Audience,
    Conversation,
}

/// What the next recording will be, which a recording freezes; the interface's language; and
/// how long audio stays on this device once a recording's record is written (`retain`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct Setup {
    pub spoken: Vec<String>,
    pub shown: Vec<String>, // each a caption column
    pub sound: Sound,
    pub device: Option<String>,
    pub mode: Mode,
    pub language: Option<String>, // the interface's; none: the system's
    pub keep: Option<u32>,        // days from a recording's start; none: until destroyed
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Session {
    pub id: String,
    #[ts(type = "number")]
    pub started: i64,
    pub status: Status,
}

/// Peak and RMS amplitude of the latest 100 ms audio block, or zero while paused or idle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct Level {
    pub peak: u16,
    pub rms: u16,
}

impl Level {
    /// Peak and root-mean-square amplitude of a 16-bit PCM slice (`0` when empty).
    pub fn of(pcm: &[i16]) -> Self {
        if pcm.is_empty() {
            return Self::default();
        }
        let mut peak = 0u16;
        let mut sum_sq = 0u64;
        for &sample in pcm {
            let amp = sample.unsigned_abs();
            peak = peak.max(amp);
            sum_sq += u64::from(amp) * u64::from(amp);
        }
        let rms = (sum_sq as f64 / pcm.len() as f64).sqrt().round() as u16;
        Self { peak, rms }
    }
}

/// The codes said lately, oldest first, and the number the next one will have. At most
/// [`Notices::KEEP_CODES`] are kept: a reader that falls further behind misses the oldest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct Notices {
    #[ts(type = "number")]
    pub next: u64,
    pub codes: Vec<String>,
}

impl Notices {
    /// Most notice codes retained at once: the interface shows no more at one time (`P1.md` §3.3).
    pub const KEEP_CODES: usize = 16;
}

/// A recording in a list. A draft's text is still the live one.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Row {
    pub id: String,
    #[ts(type = "number")]
    pub started: i64,
    pub length_ms: u32,
    pub title: String,
    pub preview: String,
    pub spoken: Vec<String>,
    pub draft: bool,
    pub audio: bool, // still on this device
}

/// A recording being read: each line with the key it is corrected by.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Open {
    pub row: Row,
    pub lines: Vec<(String, Said)>,
    pub speakers: BTreeMap<String, String>,
    pub note: String,
    pub filed: Option<Filing>, // where it went to a city, and how that went
}

/// A room others can read: one recording, or the live captions. Its key is after `#`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Room {
    pub id: String,
    pub recording: Option<String>,
    pub link: String,
}

/// One signal value stamped with the monotonic sequence number of its own signal kind (`P1.md` §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Stamped {
    #[ts(type = "number")]
    pub seq: u64,
    pub signal: Signal,
}

/// Latest-value signals pushed from the core to the interface (`P1.md` §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "of", content = "is")]
pub enum Signal {
    Frame(Arc<Frame>),
    Session(Arc<Option<Session>>),
    Level(Arc<Level>),
    Water(Arc<Water>),
    Stats(Arc<Tally>),
}

/// Bounded state replaced whole, latest-value signals stamped with per-kind sequence numbers,
/// or one-shot notice codes (`P1.md` §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "kind", content = "is")]
pub enum Event {
    State(Arc<State>),
    Signal(Stamped),
    Notice(String),
}

/// One change a person makes to a recording; each is one more record. A line's `language`
/// is its own for what was said, or a translation's.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(tag = "what")]
pub enum Change {
    Title { text: String },
    Line { key: String, language: String, text: String },
    Speaker { label: String, name: String },
    Note { text: String },
}

/// What the interface asks. `Import` records a file; `Export` writes a recording out in the
/// languages asked for (none: as spoken); `Share` gives a room to one recording, or (none) to
/// the live captions; `OwnKey` sets or clears the person's own key for a service;
/// `RotationSaved` says the rotation key of the identity made here is saved elsewhere, and
/// `CopyRotation` puts it on the clipboard to be saved: from the secrets, never through here.
/// `Update` installs the newer version that is ready and relaunches the app, never while
/// recording. `DestroyAudio` deletes a recording's audio for good and keeps its text; `Term`
/// puts a term in the glossary, or (none) takes it out. `City` reads the calendar of the city at
/// an address; `File` files a recording to one of its events, with its audio unless `audio` is
/// false or none is left.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "do")]
pub enum Command {
    Start,
    Import { path: String },
    Pause,
    Resume,
    Stop,
    Setup { setup: Setup },
    Open { id: String },
    Close,
    Change { id: String, change: Change },
    Remove { id: String },
    Restore { id: String },
    Search { query: String },
    Export { id: String, exporter: String, languages: Vec<String>, to: String },
    Share { recording: Option<String> },
    Unshare { room: String },
    SignIn,
    SignOut,
    RotationSaved,
    CopyRotation,
    Update,
    OwnKey { service: String, key: Option<String> },
    Captions { show: bool },
    DestroyAudio { id: String },
    Term { said: String, written: Option<BTreeMap<String, String>> },
    City { address: String },
    File { id: String, event: String, audio: bool },
}

impl Command {
    /// What only a person signed in may ask (`docs/PLAN.md` §2.7). Every variant answers here
    /// explicitly so any future command must state whether it requires a signed-in person.
    pub fn needs_person(&self) -> bool {
        match self {
            Command::Share { .. } | Command::Unshare { .. } | Command::File { .. } => true,
            Command::Start
            | Command::Import { .. }
            | Command::Pause
            | Command::Resume
            | Command::Stop
            | Command::Setup { .. }
            | Command::Open { .. }
            | Command::Close
            | Command::Change { .. }
            | Command::Remove { .. }
            | Command::Restore { .. }
            | Command::Search { .. }
            | Command::Export { .. }
            | Command::SignIn
            | Command::SignOut
            | Command::RotationSaved
            | Command::CopyRotation
            | Command::Update
            | Command::OwnKey { .. }
            | Command::Captions { .. }
            | Command::DestroyAudio { .. }
            | Command::Term { .. }
            | Command::City { .. } => false,
        }
    }
}
