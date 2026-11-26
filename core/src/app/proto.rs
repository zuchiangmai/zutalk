//! What crosses between the core and the interface: one state, sent whole whenever it changes;
//! live captions, sent as they come; commands, going the other way. The interface holds
//! nothing of its own but what is being typed. The TypeScript is written by `cargo test`.

pub use crate::capture::audio::Device;
pub use crate::id::identity::AuthFlow;
pub use crate::id::keys::From;
pub use crate::id::quota::{Quota, ServiceQuota};
pub use crate::knowledge::topic::{Topic, TopicFact, TopicSummary, TranslationPair};
pub use crate::live::frame::{Caption, Column, Frame, Status};
pub use crate::out::host::{HostEvent, HostState};
pub use crate::record::adopt::LocalShelf;
pub use crate::record::notes::Note;
pub use crate::record::shelf::{ContentTag, TitleBy, TitleCandidate};
pub use crate::record::takes::{RefineFailure, RefineStatus, TakeInfo};
use crate::rt::stats::Tally;
use crate::rt::water::Water;
pub use crate::view::index::{
    LibraryFilter, LibraryPage, LibraryRow, LibrarySummary, MonthCount, SearchHit, SearchHitKind,
    SearchResult,
};
pub use crate::view::query::{
    Invalidate, Invalidations, OutboundRow, Query, QueryReply, QueryValue, RecordingView, ViewKind,
};
pub use crate::view::transcript::TranscriptChunk;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use ts_rs::TS;

/// Bounded app state sent whole whenever it changes (`PLAN.md` §2.4, `P2.md` §3.6.1).
/// Anything that grows with recordings or history is fetched through [`Query`], never held here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct State {
    pub me: Option<String>,     // the DID signed in on this device
    pub auth: AuthFlow,         // explicit sign-in / create / recover state machine
    pub handle: Option<String>, // primary Zu handle for `me`, cached offline and refreshed on sign-in
    pub rotation_unsaved: bool, // the identity made here has a rotation key not saved elsewhere
    pub update: Option<String>, // a newer version of the app, downloaded and ready to install
    pub keys: Vec<(String, From)>,
    #[ts(type = "number | null")]
    pub left: Option<u64>, // µ$ of granted quota left, as last heard
    pub quota: Option<Quota>, // per-service UTC daily community quota (bounded to 2 services)
    pub setup: Setup,
    pub devices: Vec<Device>,
    pub languages: Vec<String>, // the ones there are rules for
    pub exporters: Vec<String>,
    pub host: HostState, // bounded in-memory hosting state (at most one live room)
    pub unmoved: Vec<(String, String)>, // what 0.8 handed over that did not move: title, why
    pub local_shelf: LocalShelf, // bounded summary of the device-local shelf
    pub captions: bool,  // the captions window is on screen
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Writing {
    pub id: String,
    pub step: String,
    pub done: u32,
    pub of: u32,
}

/// Latest progress of each bounded background job class (`P2.md` §3.6.1, `PLAN.md` §2.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct Progress {
    pub refine: Option<Writing>,
    pub file: Option<Writing>,
    pub move_old: Option<Writing>,
    pub title: Option<Writing>,
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

/// Smallest readable caption window width in physical pixels (`P5-03`).
pub const CAPTION_MIN_WIDTH_PX: u32 = 240;
/// Smallest readable caption window height in physical pixels (`P5-03`).
pub const CAPTION_MIN_HEIGHT_PX: u32 = 80;
/// Upper bound guarding against corrupted window geometry (`P5-03`).
pub const CAPTION_MAX_EXTENT_PX: u32 = 16_384;
/// Minimum caption lines per language.
pub const CAPTION_LINES_MIN: u8 = 1;
/// Maximum caption lines per language (matching 0.8's audience row ceiling).
pub const CAPTION_LINES_MAX: u8 = 8;
/// Minimum manual caption font size in px/pt (0.8 `SubtitleOverlayFontPolicy`).
pub const CAPTION_FONT_MIN_PX: u16 = 16;
/// Maximum manual caption font size in px/pt for projector reading.
pub const CAPTION_FONT_MAX_PX: u16 = 160;
/// Minimum continuous caption canvas opacity percentage (20%).
pub const CAPTION_OPACITY_MIN_PCT: u8 = 20;
/// Maximum continuous caption canvas opacity percentage (100%).
pub const CAPTION_OPACITY_MAX_PCT: u8 = 100;

/// Fixed caption window presentation scenes (`P5.md` §3.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum CaptionScene {
    #[default]
    SelfView,
    Band,
    Stage,
}

impl CaptionScene {
    /// Stable plain code recorded in `shell.captions.style`.
    pub const fn code(self) -> &'static str {
        match self {
            CaptionScene::SelfView => "self",
            CaptionScene::Band => "band",
            CaptionScene::Stage => "stage",
        }
    }
}

/// Caption font scale preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum CaptionSize {
    Small,
    #[default]
    Auto,
    Large,
}

/// Caption window surface opacity preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum CaptionBg {
    Clear,
    Half,
    #[default]
    Solid,
}

/// Persisted physical-pixel window rectangle for a non-fullscreen caption scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl WindowRect {
    /// Clamps degenerate or out-of-range coordinates and dimensions to safe physical pixel bounds.
    pub fn clamp(self) -> Self {
        let bound = CAPTION_MAX_EXTENT_PX as i32;
        Self {
            x: self.x.clamp(-bound, bound),
            y: self.y.clamp(-bound, bound),
            width: self.width.clamp(CAPTION_MIN_WIDTH_PX, CAPTION_MAX_EXTENT_PX),
            height: self.height.clamp(CAPTION_MIN_HEIGHT_PX, CAPTION_MAX_EXTENT_PX),
        }
    }
}

/// Per-scene visual and window behavior settings (`P5.md` §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct SceneStyle {
    pub lines: u8,
    pub size: CaptionSize,
    /// Manual font size in px/pt (`16..=160`); `None` follows window dimensions automatically.
    #[serde(default)]
    pub font_px: Option<u16>,
    pub bg: CaptionBg,
    /// Continuous canvas backdrop opacity percent (`20..=100`); `None` uses the `bg` preset.
    #[serde(default)]
    pub opacity: Option<u8>,
    pub light: bool,
    pub sans: bool,
    pub top: bool,
    pub always_on_top: bool,
    pub click_through: bool,
    pub auto_hide: bool,
    pub fullscreen: bool,
    pub geometry: Option<WindowRect>,
}

impl SceneStyle {
    pub fn self_view() -> Self {
        Self {
            lines: 2,
            size: CaptionSize::Auto,
            font_px: None,
            bg: CaptionBg::Half,
            opacity: None,
            light: false,
            sans: false,
            top: false,
            always_on_top: true,
            click_through: false,
            auto_hide: false,
            fullscreen: false,
            geometry: None,
        }
    }

    pub fn band() -> Self {
        Self {
            lines: 2,
            size: CaptionSize::Auto,
            font_px: None,
            bg: CaptionBg::Solid,
            opacity: None,
            light: false,
            sans: false,
            top: true,
            always_on_top: true,
            click_through: false,
            auto_hide: false,
            fullscreen: false,
            geometry: None,
        }
    }

    pub fn stage() -> Self {
        Self {
            lines: 3,
            size: CaptionSize::Auto,
            font_px: None,
            bg: CaptionBg::Solid,
            opacity: None,
            light: false,
            sans: false,
            top: false,
            always_on_top: false,
            click_through: false,
            auto_hide: false,
            fullscreen: true,
            geometry: None,
        }
    }

    /// Clamps `lines`, `font_px`, `opacity`, and any saved `geometry` to safe bounds.
    pub fn normalized(mut self) -> Self {
        self.lines = self.lines.clamp(CAPTION_LINES_MIN, CAPTION_LINES_MAX);
        self.font_px = self.font_px.map(|px| px.clamp(CAPTION_FONT_MIN_PX, CAPTION_FONT_MAX_PX));
        self.opacity =
            self.opacity.map(|pct| pct.clamp(CAPTION_OPACITY_MIN_PCT, CAPTION_OPACITY_MAX_PCT));
        self.geometry = self.geometry.map(WindowRect::clamp);
        self
    }
}

impl Default for SceneStyle {
    fn default() -> Self {
        Self::self_view()
    }
}

/// Fixed 3-scene caption style table (bounded struct, zero dynamic map for I8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct CaptionScenes {
    pub self_view: SceneStyle,
    pub band: SceneStyle,
    pub stage: SceneStyle,
}

impl Default for CaptionScenes {
    fn default() -> Self {
        Self {
            self_view: SceneStyle::self_view(),
            band: SceneStyle::band(),
            stage: SceneStyle::stage(),
        }
    }
}

/// Active caption scene plus per-scene styles persisted inside `Setup` (`settings.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct CaptionPrefs {
    pub scene: CaptionScene,
    pub scenes: CaptionScenes,
}

impl CaptionPrefs {
    pub fn style(&self, scene: CaptionScene) -> &SceneStyle {
        match scene {
            CaptionScene::SelfView => &self.scenes.self_view,
            CaptionScene::Band => &self.scenes.band,
            CaptionScene::Stage => &self.scenes.stage,
        }
    }

    pub fn style_mut(&mut self, scene: CaptionScene) -> &mut SceneStyle {
        match scene {
            CaptionScene::SelfView => &mut self.scenes.self_view,
            CaptionScene::Band => &mut self.scenes.band,
            CaptionScene::Stage => &mut self.scenes.stage,
        }
    }

    pub fn active(&self) -> &SceneStyle {
        self.style(self.scene)
    }

    pub fn active_mut(&mut self) -> &mut SceneStyle {
        self.style_mut(self.scene)
    }

    /// Normalizes all 3 scenes' line counts, font/opacity bounds, and saved window rectangles.
    pub fn normalized(mut self) -> Self {
        self.scenes.self_view = self.scenes.self_view.normalized();
        self.scenes.band = self.scenes.band.normalized();
        self.scenes.stage = self.scenes.stage.normalized();
        self
    }
}

/// Current defaults schema version persisted in `settings.json`.
/// `0` denotes pre-0.9.1 files (`self_view.always_on_top` false); `1` denotes 0.9.1 files that
/// could retain a stuck `click_through: true` trap on `band`/`stage`.
pub const SETUP_DEFAULTS_VERSION: u8 = 2;

fn legacy_setup_version() -> u8 {
    0
}

/// What the next recording will be, which a recording freezes; the interface's language; and
/// how long audio stays on this device once a recording's record is written (`retain`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct Setup {
    pub spoken: Vec<String>,
    pub shown: Vec<String>, // each a caption column
    pub sound: Sound,
    pub device: Option<String>,
    pub mode: Mode,
    pub language: Option<String>, // the interface's; none: the system's
    pub keep: Option<u32>,        // days from a recording's start; none: until destroyed
    #[serde(default)]
    pub caption: CaptionPrefs,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default = "legacy_setup_version")]
    #[ts(skip)]
    pub defaults_v: u8,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            spoken: Vec::new(),
            shown: Vec::new(),
            sound: Sound::default(),
            device: None,
            mode: Mode::default(),
            language: None,
            keep: None,
            caption: CaptionPrefs::default(),
            city: None,
            topics: Vec::new(),
            defaults_v: SETUP_DEFAULTS_VERSION,
        }
    }
}

impl Setup {
    /// First-run defaults when no `settings.json` exists yet: bilingual Chinese + English.
    pub fn initial() -> Self {
        let langs = vec!["zh".into(), "en".into()];
        Self { spoken: langs.clone(), shown: langs, ..Self::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Session {
    pub id: String,
    #[ts(type = "number")]
    pub started: i64,
    pub status: Status,
    pub outbound: bool,
    pub viewers: u32,
    pub event_title: Option<String>,
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

/// One signal value stamped with the monotonic sequence number of its own signal kind (`P1.md` §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct Stamped {
    #[ts(type = "number")]
    pub seq: u64,
    pub signal: Signal,
}

/// Latest-value signals pushed from the core to the interface (`P1.md` §3.3, `P2.md` §3.6.1).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "of", content = "is")]
pub enum Signal {
    Frame(Arc<Frame>),
    Session(Arc<Option<Session>>),
    Level(Arc<Level>),
    Progress(Arc<Progress>),
    Water(Arc<Water>),
    Stats(Arc<Tally>),
}

/// Bounded state replaced whole, latest-value signals stamped with per-kind sequence numbers,
/// view invalidations, or one-shot notice codes (`P1.md` §3.3, `P2.md` §3.6.2).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "kind", content = "is")]
pub enum Event {
    State(Arc<State>),
    Signal(Stamped),
    Invalidate(Invalidate),
    Notice(String),
}

/// One operation on a recording's timestamped notes (`P2.md` §3.3.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "op")]
pub enum NoteOp {
    Add { at_ms: u32, text: String, anchor: Option<String> },
    Edit { key: String, text: String },
    Remove { key: String },
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
#[derive(Clone, PartialEq, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "do")]
pub enum Command {
    Start,
    Import { path: String },
    Pause,
    Resume,
    Stop,
    Reconnect,
    Setup { setup: Setup },
    Change { id: String, change: Change },
    Note { id: String, op: NoteOp },
    Refine { id: String },
    Remove { id: String },
    Restore { id: String },
    Export { id: String, exporter: String, languages: Vec<String>, to: String },
    HostArm { event: Option<HostEvent>, captions: bool },
    HostCancel,
    StopOutbound,
    SignIn,
    ConfirmSignIn,
    CancelSignIn,
    CreateIdentity { handle: Option<String> },
    RecoverIdentity { did: String, key: String },
    RefreshQuota,
    DiscardLocal,
    SignOut,
    RotationSaved,
    CopyRotation,
    Update,
    OwnKey { service: String, key: Option<String> },
    Captions { show: bool },
    CaptionGeometry { scene: CaptionScene, rect: WindowRect },
    DestroyAudio { id: String },
    Term { said: String, written: Option<BTreeMap<String, String>> },
    SaveTopic { topic: Topic },
    RemoveTopic { id: String },
    ImportTopicFile { id: String, path: String },
    RememberTerm { topic_id: String, said: String, written: BTreeMap<String, String> },
    GenerateTitle { id: String },
    PickTitle { id: String, title: String, by: TitleBy },
    City { address: String },
    File { id: String, event: String, audio: bool },
    Diagnose,
}

/// Redacts secret fields (`RecoverIdentity.key`, `OwnKey.key`) so secrets never reach `Debug`
/// output (`CLAUDE.md` Principle 6).
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Start => f.write_str("Start"),
            Command::Import { path } => f.debug_struct("Import").field("path", path).finish(),
            Command::Pause => f.write_str("Pause"),
            Command::Resume => f.write_str("Resume"),
            Command::Stop => f.write_str("Stop"),
            Command::Reconnect => f.write_str("Reconnect"),
            Command::Setup { setup } => f.debug_struct("Setup").field("setup", setup).finish(),
            Command::Change { id, change } => {
                f.debug_struct("Change").field("id", id).field("change", change).finish()
            }
            Command::Note { id, op } => {
                f.debug_struct("Note").field("id", id).field("op", op).finish()
            }
            Command::Refine { id } => f.debug_struct("Refine").field("id", id).finish(),
            Command::Remove { id } => f.debug_struct("Remove").field("id", id).finish(),
            Command::Restore { id } => f.debug_struct("Restore").field("id", id).finish(),
            Command::Export { id, exporter, languages, to } => f
                .debug_struct("Export")
                .field("id", id)
                .field("exporter", exporter)
                .field("languages", languages)
                .field("to", to)
                .finish(),
            Command::HostArm { event, captions } => {
                f.debug_struct("HostArm").field("event", event).field("captions", captions).finish()
            }
            Command::HostCancel => f.write_str("HostCancel"),
            Command::StopOutbound => f.write_str("StopOutbound"),
            Command::SignIn => f.write_str("SignIn"),
            Command::ConfirmSignIn => f.write_str("ConfirmSignIn"),
            Command::CancelSignIn => f.write_str("CancelSignIn"),
            Command::CreateIdentity { handle } => {
                f.debug_struct("CreateIdentity").field("handle", handle).finish()
            }
            Command::RecoverIdentity { did, key: _ } => {
                f.debug_struct("RecoverIdentity").field("did", did).field("key", &"…").finish()
            }
            Command::RefreshQuota => f.write_str("RefreshQuota"),
            Command::DiscardLocal => f.write_str("DiscardLocal"),
            Command::SignOut => f.write_str("SignOut"),
            Command::RotationSaved => f.write_str("RotationSaved"),
            Command::CopyRotation => f.write_str("CopyRotation"),
            Command::Update => f.write_str("Update"),
            Command::OwnKey { service, key } => f
                .debug_struct("OwnKey")
                .field("service", service)
                .field("key", &key.as_ref().map(|_| "…"))
                .finish(),
            Command::Captions { show } => f.debug_struct("Captions").field("show", show).finish(),
            Command::CaptionGeometry { scene, rect } => {
                f.debug_struct("CaptionGeometry").field("scene", scene).field("rect", rect).finish()
            }
            Command::DestroyAudio { id } => f.debug_struct("DestroyAudio").field("id", id).finish(),
            Command::Term { said, written } => {
                f.debug_struct("Term").field("said", said).field("written", written).finish()
            }
            Command::SaveTopic { topic } => f
                .debug_struct("SaveTopic")
                .field("id", &topic.id)
                .field("pinned", &topic.pinned)
                .field("terms", &topic.terms.len())
                .finish(),
            Command::RemoveTopic { id } => f.debug_struct("RemoveTopic").field("id", id).finish(),
            Command::ImportTopicFile { id, path: _ } => {
                f.debug_struct("ImportTopicFile").field("id", id).field("path", &"…").finish()
            }
            Command::RememberTerm { topic_id, said: _, written } => f
                .debug_struct("RememberTerm")
                .field("topic_id", topic_id)
                .field("written_len", &written.len())
                .finish(),
            Command::GenerateTitle { id } => {
                f.debug_struct("GenerateTitle").field("id", id).finish()
            }
            Command::PickTitle { id, title: _, by } => f
                .debug_struct("PickTitle")
                .field("id", id)
                .field("title", &"…")
                .field("by", by)
                .finish(),
            Command::City { address } => f.debug_struct("City").field("address", address).finish(),
            Command::File { id, event, audio } => f
                .debug_struct("File")
                .field("id", id)
                .field("event", event)
                .field("audio", audio)
                .finish(),
            Command::Diagnose => f.write_str("Diagnose"),
        }
    }
}

impl Command {
    /// Variant name for structured logs (`command.done`, `command.refused`, `hub.turn.slow`).
    /// Never carries any argument of the command.
    pub fn kind(&self) -> &'static str {
        match self {
            Command::Start => "Start",
            Command::Import { .. } => "Import",
            Command::Pause => "Pause",
            Command::Resume => "Resume",
            Command::Stop => "Stop",
            Command::Reconnect => "Reconnect",
            Command::Setup { .. } => "Setup",
            Command::Change { .. } => "Change",
            Command::Note { .. } => "Note",
            Command::Refine { .. } => "Refine",
            Command::Remove { .. } => "Remove",
            Command::Restore { .. } => "Restore",
            Command::Export { .. } => "Export",
            Command::HostArm { .. } => "HostArm",
            Command::HostCancel => "HostCancel",
            Command::StopOutbound => "StopOutbound",
            Command::SignIn => "SignIn",
            Command::ConfirmSignIn => "ConfirmSignIn",
            Command::CancelSignIn => "CancelSignIn",
            Command::CreateIdentity { .. } => "CreateIdentity",
            Command::RecoverIdentity { .. } => "RecoverIdentity",
            Command::RefreshQuota => "RefreshQuota",
            Command::DiscardLocal => "DiscardLocal",
            Command::SignOut => "SignOut",
            Command::RotationSaved => "RotationSaved",
            Command::CopyRotation => "CopyRotation",
            Command::Update => "Update",
            Command::OwnKey { .. } => "OwnKey",
            Command::Captions { .. } => "Captions",
            Command::CaptionGeometry { .. } => "CaptionGeometry",
            Command::DestroyAudio { .. } => "DestroyAudio",
            Command::Term { .. } => "Term",
            Command::SaveTopic { .. } => "SaveTopic",
            Command::RemoveTopic { .. } => "RemoveTopic",
            Command::ImportTopicFile { .. } => "ImportTopicFile",
            Command::RememberTerm { .. } => "RememberTerm",
            Command::GenerateTitle { .. } => "GenerateTitle",
            Command::PickTitle { .. } => "PickTitle",
            Command::City { .. } => "City",
            Command::File { .. } => "File",
            Command::Diagnose => "Diagnose",
        }
    }

    /// What only a person signed in may ask (`docs/PLAN.md` §2.7). Every variant answers here
    /// explicitly so any future command must state whether it requires a signed-in person.
    pub fn needs_person(&self) -> bool {
        match self {
            Command::HostArm { .. }
            | Command::File { .. }
            | Command::RefreshQuota
            | Command::DiscardLocal => true,
            Command::Start
            | Command::Import { .. }
            | Command::Pause
            | Command::Resume
            | Command::Stop
            | Command::Reconnect
            | Command::Setup { .. }
            | Command::Change { .. }
            | Command::Note { .. }
            | Command::Refine { .. }
            | Command::Remove { .. }
            | Command::Restore { .. }
            | Command::Export { .. }
            | Command::HostCancel
            | Command::StopOutbound
            | Command::SignIn
            | Command::ConfirmSignIn
            | Command::CancelSignIn
            | Command::CreateIdentity { .. }
            | Command::RecoverIdentity { .. }
            | Command::SignOut
            | Command::RotationSaved
            | Command::CopyRotation
            | Command::Update
            | Command::OwnKey { .. }
            | Command::Captions { .. }
            | Command::CaptionGeometry { .. }
            | Command::DestroyAudio { .. }
            | Command::Term { .. }
            | Command::SaveTopic { .. }
            | Command::RemoveTopic { .. }
            | Command::ImportTopicFile { .. }
            | Command::RememberTerm { .. }
            | Command::GenerateTitle { .. }
            | Command::PickTitle { .. }
            | Command::City { .. }
            | Command::Diagnose => false,
        }
    }
}
