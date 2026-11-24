//! The hub. Commands, and reports from the work under way, come into one mailbox; after each,
//! the state is sent whole if it changed. A recording is a task of its own, from its first
//! block to its written transcript, so a stop hands the app back at once; so is whatever goes
//! to a room or to a city.

use crate::ai::write;
pub use crate::app::proto::Notices;
use crate::app::proto::{
    Change, Command, From, Level as SoundLevel, Open, Room, Row, Session, Sound, State, Status,
    Writing,
};
use crate::app::recording;
pub use crate::app::recording::{DRAFT_QUEUE_LINES, TAPE_QUEUE_BLOCKS};
use crate::capture::audio::{self, Capture, Hear, Hearing, Source};
use crate::capture::clock::{wall_clock_policy, wall_ms};
use crate::capture::tape::{self, Reels};
use crate::id::keys::{self, Chain, Me, Ours, Secret, Service, SignIn, Who};
use crate::knowledge::glossary::{self, Term};
use crate::live::frame::{Frame, MAX_COLUMNS};
use crate::live::lane::{self, Mode};
use crate::live::lines::Line;
use crate::live::wire::{self, Dial};
use crate::out::city::{self, Calendar, Filer, Filing};
use crate::out::export::{Doc, EXPORTERS};
use crate::out::room::{self, Write};
use crate::record::shelf::{
    self, About, Drafts, FILING, LINE, NOTE, RECORDING, Recording, SHARE, SPEAKER, Share, Shelf,
    Words,
};
use crate::record::{migrate, retain};
use crate::rt::queue::{self, Back, Full, Rx, Tx};
use crate::rt::signal::{Reading, Register};
use crate::rt::stats::{self, Counter, Spread, Stats, Tally};
use crate::rt::supervise::{End, Tree, again};
use crate::rt::water::{Gauge, Water};
use std::path::{Path, PathBuf};
use std::{fs, sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio::task::AbortHandle;
use zutalk_log::{Level, info, warn};

const WHOLE: &str = "stt-async-v6";
/// A live room is written at most this often (DESIGN.md, rooms). Origin to be traced.
const SLOT_MS: u64 = 250;
/// The hub's mail. 256: a burst of clicks is a few dozen; the hub answers each in under a millisecond.
pub const HUB_MAIL_MSGS: usize = 256;
/// One turn of the hub (PLAN.md §4.1): a command should take under a millisecond.
pub const HUB_TURN_BUDGET_US: u64 = 1_000;

/// What the core is started with: where things are, the ports the shell and the tests give,
/// and how much it says.
pub struct Config {
    /// The app's own folder: settings, records, tapes, secrets, logs.
    pub data: PathBuf,
    /// Ours: relays the sign-in, issues keys, keeps rooms.
    pub server: String,
    /// Soniox's real-time endpoint, a WebSocket.
    pub realtime: String,
    /// Soniox's API: whole-recording transcripts.
    pub soniox: String,
    /// OpenRouter's API: whole-recording translation and checks.
    pub openrouter: String,
    /// Where a `did:zu` made here is registered.
    pub directory: String,
    /// Where the 0.8 app kept its data, if it ran here.
    pub old: Option<PathBuf>,
    /// How sound is heard: real audio capture in production, fake microphone in tests.
    pub hear: Hear,
    /// How a live caption lane connects to the realtime service.
    pub dial: Dial,
    /// How a recording's tape is opened for writing.
    pub reels: Reels,
    /// How a recording's live draft is opened for writing.
    pub drafts: Drafts,
    /// Puts text on the clipboard.
    pub clip: Clip,
    /// Installs a newer version.
    pub install: Install,
    /// How much the log says: `ZUTALK_LOG` through `from_env`, else `info`.
    pub log: Level,
}

/// Puts text on the system clipboard: the shell's to give. `false`: it could not.
pub type Clip = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Installs the newer version the shell has ready and relaunches the app: the shell's to give.
/// It returns only if it could not.
pub type Install = Arc<dyn Fn() + Send + Sync>;

impl Config {
    pub fn new(data: PathBuf, server: &str) -> Self {
        let realtime = "wss://stt-rt.soniox.com/transcribe-websocket".into();
        let (soniox, openrouter) =
            ("https://api.soniox.com/v1".into(), "https://openrouter.ai/api/v1".into());
        let hear: Hear = Arc::new(|source, sink, notice, stats| {
            let capture = Capture::start(source, wall_clock_policy, stats, sink, notice)?;
            Ok(Box::new(capture) as Box<dyn Hearing>)
        });
        let (dial, reels, drafts) = (wire::dial(), tape::reels(), shelf::drafts());
        let (server, directory, old) = (server.into(), zu::did::DIRECTORY.into(), None);
        // Without a shell to give them, nothing can be put on the clipboard, nor installed.
        let (clip, install): (Clip, Install) = (Arc::new(|_| false), Arc::new(|| {}));
        let log = Level::Info;
        Self {
            data,
            server,
            realtime,
            soniox,
            openrouter,
            directory,
            old,
            hear,
            dial,
            reels,
            drafts,
            clip,
            install,
            log,
        }
    }

    /// What the environment says: `ZUTALK_LOG` (`debug`, else `info`). The one place the core
    /// reads the environment; the shell calls it once, before the log is installed.
    pub fn from_env(self) -> Self {
        let log = Level::from_setting(std::env::var("ZUTALK_LOG").ok().as_deref());
        Self { log, ..self }
    }

    /// Where the log's files are.
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    /// Where each recording's audio is, in a folder of its own.
    fn tapes(&self) -> PathBuf {
        self.data.join("tape")
    }
}

pub(crate) enum Msg {
    Do(Command),
    Ended(String),
    Writing(Writing),
    Wrote(String),
    SignedIn(Result<Me, End>),
    Claimed(Result<(), &'static str>),
    Shared(Result<(), &'static str>),
    Looked(Result<String, &'static str>),
    City(Result<Calendar, End>),
    Filing(Writing),
    Filed(String, &'static str),
    Moving(Writing),
    Moved(Result<Vec<(String, String)>, &'static str>),
}

impl Msg {
    fn kind(&self) -> &'static str {
        match self {
            Msg::Do(command) => command.kind(),
            Msg::Ended(_) => "Ended",
            Msg::Writing(_) => "Writing",
            Msg::Wrote(_) => "Wrote",
            Msg::SignedIn(_) => "SignedIn",
            Msg::Claimed(_) => "Claimed",
            Msg::Shared(_) => "Shared",
            Msg::Looked(_) => "Looked",
            Msg::City(_) => "City",
            Msg::Filing(_) => "Filing",
            Msg::Filed(_, _) => "Filed",
            Msg::Moving(_) => "Moving",
            Msg::Moved(_) => "Moved",
        }
    }
}

/// Everything the core says outward: latest-value registers readable at once or on change,
/// cloneable across the shell's pump, a live room's slot, and tests (`P1.md` §3.3).
#[derive(Clone, Debug)]
pub struct Outlet {
    pub state: Reading<State>,
    pub frame: Reading<Frame>,
    pub session: Reading<Option<Session>>,
    pub level: Reading<SoundLevel>,
    pub water: Reading<Water>,
    pub stats: Reading<Tally>,
    pub said: Reading<Notices>,
}

/// Writer half for [`Notices`]: appends a code, trims past [`Notices::KEEP_CODES`], emits
/// `notice.said`, and increments `notices.said`.
#[derive(Clone)]
pub(crate) struct Voice {
    reg: Register<Notices>,
    counted: Counter,
}

impl Voice {
    fn new(stats: &Stats) -> (Self, Reading<Notices>) {
        let reg = Register::new(Notices::default());
        let reading = reg.read();
        (Self { reg, counted: stats.counter("notices.said") }, reading)
    }

    pub(crate) fn say(&self, code: &'static str) {
        self.reg.change(|notices| {
            let mut codes = notices.codes.clone();
            codes.push(code.into());
            if codes.len() > Notices::KEEP_CODES {
                codes.drain(..codes.len() - Notices::KEEP_CODES);
            }
            Some(Notices { next: notices.next + 1, codes })
        });
        self.counted.add(1);
        info!("notice.said", code = code);
    }
}

/// The core, running. Commands go in; latest values are read from [`Outlet`].
pub struct Core {
    commands: Tx<Msg>,
    results: Tx<Msg>,
    commands_refused: Counter,
    stats: Stats,
    shell: Arc<Gauge>,
    screen: Arc<Gauge>,
}

impl Core {
    /// Starts on the current runtime. The key store must be open (`keys::open_store`).
    pub fn start(config: Config) -> Result<(Core, Outlet), End> {
        let _ = rustls::crypto::ring::default_provider().install_default(); // dropped: installing the same TLS provider again is harmless
        let http = reqwest::Client::builder().build().map_err(|_| End::Fatal("tls"))?;
        let stats = Stats::new();
        let (commands, mail) = queue::bounded(&stats, "hub.mail", HUB_MAIL_MSGS, Full::Reject); // the interface: never waits, is told `busy`
        let results = commands.with(Full::Wait); // work that is done: must arrive; whoever did it can wait
        let progress = commands.with(Full::Drop); // how far a job has got: only the latest matters
        let root = Tree::root("core", stats.clone());
        let (config, keys) = (Arc::new(config), Arc::new(Chain::new(None)));
        let state_reg = Register::new(State::default());
        let frame_reg = Register::new(Frame::default());
        let session_reg = Register::new(None);
        let level_reg = Register::new(SoundLevel::default());
        let water_reg = Register::new(Water::default());
        let stats_reg = Register::new(Tally::default());
        let shell = Arc::new(Gauge::new());
        let screen = Arc::new(Gauge::new());
        let (said, said_reading) = Voice::new(&stats);
        let outlet = Outlet {
            state: state_reg.read(),
            frame: frame_reg.read(),
            session: session_reg.read(),
            level: level_reg.read(),
            water: water_reg.read(),
            stats: stats_reg.read(),
            said: said_reading,
        };
        let state_sent = stats.counter("state.sent");
        let commands_refused = stats.counter("commands.refused");
        let commands_done = stats.counter("commands.done");
        let hub_turns = stats.counter("hub.turns");
        let hub_turns_slow = stats.counter("hub.turns.slow");
        let hub_turn_us = stats.spread("hub.turn_us");
        let (state, sent, ours, shelf, recording, opened, signing, slot, invite, moving) =
            Default::default();
        let hub = Hub {
            config,
            results: results.clone(),
            progress,
            tree: root.clone(),
            state_reg,
            state_sent,
            commands_done,
            hub_turns,
            hub_turns_slow,
            hub_turn_us,
            frame_reg,
            session_reg,
            level_reg,
            water_reg,
            shell: shell.clone(),
            screen: screen.clone(),
            said,
            http,
            who: Who::Nobody("starting"),
            keys,
            ours,
            state,
            sent,
            shelf,
            recording,
            opened,
            signing,
            invite,
            slot,
            moving,
        };
        root.spawn("hub", hub.run(mail));
        root.spawn("stats", stats::publish(stats.clone(), stats_reg));
        Ok((Core { commands, results, commands_refused, stats, shell, screen }, outlet))
    }

    pub fn send(&self, command: Command) -> Result<(), &'static str> {
        let kind = command.kind();
        match self.commands.offer(Msg::Do(command)) {
            Ok(()) => Ok(()),
            Err(Back::Full(_) | Back::Closed(_)) => {
                warn!("command.refused", kind = kind, code = "busy");
                self.commands_refused.add(1);
                Err("busy")
            }
        }
    }

    /// The shell's report from looking for a newer version of the app: one downloaded and
    /// ready to install, or what kept it from being had.
    pub async fn looked(&self, found: Result<String, &'static str>) {
        let _ = self.results.send(Msg::Looked(found)).await; // dropped: the hub has ended
    }

    /// Reports the latest frame sequence pushed by the shell (`P1.md` §3.3, §3.5).
    pub fn pushed(&self, seq: u64) {
        self.shell.set(seq);
    }

    /// Reports the latest frame sequence drawn by the interface (`P1.md` §3.3, §3.5).
    pub fn drawn(&self, seq: u64) {
        self.screen.set(seq);
    }

    /// Live statistics for this core: counters, peaks, spreads, and queues.
    pub fn stats(&self) -> Stats {
        self.stats.clone()
    }
}

fn code(end: End) -> &'static str {
    if let End::Retry(code) | End::Fatal(code) = end { code } else { "done" }
}

fn fail(_: zu::Error) -> &'static str {
    "shelf"
}

fn now() -> i64 {
    wall_ms() as i64
}

fn writing(id: &str, step: &str) -> Writing {
    Writing { id: id.into(), step: step.into(), done: 0, of: 0 }
}

fn row(recording: &Recording, tapes: &Path) -> Row {
    let About { started, length_ms, title, preview, spoken, take, .. } = recording.about.clone();
    let (id, audio) = (recording.id.clone(), tape::kept(&tapes.join(&recording.id)));
    Row { id, started, length_ms, title, preview, spoken, draft: take == 0, audio }
}

struct Hub {
    config: Arc<Config>,
    results: Tx<Msg>,
    progress: Tx<Msg>,
    tree: Tree,
    state_reg: Register<State>,
    state_sent: Counter,
    commands_done: Counter,
    hub_turns: Counter,
    hub_turns_slow: Counter,
    hub_turn_us: Spread,
    frame_reg: Register<Frame>,
    session_reg: Register<Option<Session>>,
    level_reg: Register<SoundLevel>,
    water_reg: Register<Water>,
    shell: Arc<Gauge>,
    screen: Arc<Gauge>,
    said: Voice,
    http: reqwest::Client,
    who: Who,
    keys: Arc<Chain>,
    ours: Option<Ours>,
    state: State,
    sent: Option<State>,
    shelf: Option<Arc<Shelf>>,
    recording: Option<(String, watch::Sender<Mode>)>,
    opened: Option<String>,
    signing: Option<AbortHandle>, // a sign-in, an identity being made, or an invite carried
    invite: Option<Secret>,       // a 0.8 community invite not carried over yet
    slot: Option<(String, AbortHandle)>, // the live room, and what keeps its slot
    moving: bool,                 // the 0.8 library is being moved in
}

impl Hub {
    async fn run(mut self, mut mail: Rx<Msg>) {
        // None kept yet is the default; one that does not read is said, then replaced.
        if let Ok(json) = fs::read(self.config.data.join("settings.json")) {
            match serde_json::from_slice(&json) {
                Ok(setup) => self.state.setup = setup,
                Err(_) => self.said.say("settings"),
            }
        }
        self.state.languages =
            crate::live::lang::ALL.iter().map(|profile| profile.lang.into()).collect();
        self.state.exporters = EXPORTERS.iter().map(|exporter| exporter.id.into()).collect();
        self.state.devices = audio::devices();
        let kept = keys::kept();
        // An identity that does not read is said, not replaced by one made for an invite.
        if let Err(code) = self.carry(matches!(kept, Ok(None))) {
            self.said.say(code);
        }
        let kept = kept.unwrap_or_else(|end| {
            self.said.say(code(end));
            None
        });
        // Always read or make the device's own identity at boot so SignOut can return to it.
        let (local, made) = match keys::local() {
            Ok((me, made)) => (Ok(me), made),
            Err(end) => (Err(end), false),
        };
        self.sign(Who::of(kept, local), "boot", made);
        self.publish();
        while let Some(msg) = mail.recv().await {
            let is_command = matches!(&msg, Msg::Do(_));
            let kind = msg.kind();
            let started = std::time::Instant::now();
            let result = self.take(msg);
            let took_us = started.elapsed().as_micros() as u64;
            self.hub_turn_us.note(took_us);
            self.hub_turns.add(1);
            if is_command {
                match result {
                    Ok(()) => info!("command.done", kind = kind, code = "ok", took_us = took_us),
                    Err(code) => warn!("command.done", kind = kind, code = code, took_us = took_us),
                }
                self.commands_done.add(1);
            }
            if took_us > HUB_TURN_BUDGET_US {
                warn!("hub.turn.slow", kind = kind, took_us = took_us);
                self.hub_turns_slow.add(1);
            }
            if let Err(code) = result {
                self.said.say(code);
            }
            self.publish();
        }
        self.tree.cancel();
    }

    fn publish(&mut self) {
        if self.sent.as_ref() != Some(&self.state) {
            self.sent = Some(self.state.clone());
            self.state_reg.set(self.state.clone());
            self.state_sent.add(1);
        }
    }

    fn take(&mut self, msg: Msg) -> Result<(), &'static str> {
        match msg {
            Msg::Do(command) => self.command(command)?,
            Msg::Ended(id) if self.under_way(&id) => self.stop(),
            Msg::Writing(writing) => {
                self.state.writing.retain(|w| w.id != writing.id);
                self.state.writing.push(writing);
            }
            Msg::Wrote(id) => {
                self.state.writing.retain(|w| w.id != id);
                self.refresh()?; // the rule asks whether it is written now
                self.retain()?
            }
            Msg::SignedIn(me) => {
                self.state.login = None;
                self.sign(Who::Person(me.map_err(code)?), "sign_in", false);
            }
            Msg::Shared(done) => self.rooms().and(done)?,
            Msg::Looked(found) => self.state.update = Some(found?),
            Msg::City(calendar) => self.state.city = Some(calendar.map_err(code)?),
            Msg::Filing(writing) => {
                self.state.filing.retain(|w| w.id != writing.id);
                self.state.filing.push(writing);
            }
            Msg::Filed(id, said) => {
                self.state.filing.retain(|w| w.id != id);
                self.said.say(said);
            }
            Msg::Claimed(done) => {
                self.invite = None;
                done?
            }
            Msg::Moving(writing) => self.state.moving = Some(writing),
            Msg::Moved(skipped) => {
                (self.moving, self.state.moving) = (false, None);
                self.state.unmoved = skipped?;
            }
            Msg::Ended(_) => {} // from a recording that has stopped
        }
        self.refresh()
    }

    fn shelf(&self) -> Result<Arc<Shelf>, &'static str> {
        match &self.shelf {
            Some(shelf) => Ok(shelf.clone()),
            None => Err(self.who.me().err().unwrap_or("keys.store")),
        }
    }

    fn command(&mut self, command: Command) -> Result<(), &'static str> {
        if command.needs_person() && self.who.person().is_none() {
            return Err("sign_in.first");
        }
        match command {
            Command::Start => {
                let device = self.state.setup.device.clone();
                self.start(match self.state.setup.sound {
                    Sound::Mic => Source::Mic { device },
                    Sound::System => Source::System,
                    Sound::Both => Source::Both { device },
                })?
            }
            Command::Import { path } => self.start(Source::File(path.into()))?,
            Command::Pause => self.mode(Mode::Paused)?,
            Command::Resume => self.mode(Mode::Recording)?,
            Command::Stop => self.stop(),
            Command::Setup { setup } => {
                if setup.shown.len() > MAX_COLUMNS {
                    return Err("captions.columns");
                }
                let json = serde_json::to_vec(&setup).map_err(|_| "settings")?;
                let saved = fs::create_dir_all(&self.config.data)
                    .and_then(|()| fs::write(self.config.data.join("settings.json"), json));
                saved.map_err(|_| "settings")?;
                self.state.setup = setup;
            }
            Command::Open { id } => self.opened = Some(id),
            Command::Close => self.opened = None,
            // A line is corrected on the record, not on a draft about to be replaced.
            Command::Change { id, change: Change::Line { .. } } if self.busy(&id) => {
                return Err("recording.writing");
            }
            Command::Change { id, change } => apply(&*self.shelf()?, &id, change)?,
            Command::Remove { id } => {
                if self.under_way(&id) {
                    self.stop();
                }
                self.shelf()?
                    .put::<About>(&id, RECORDING, &[(id.clone(), None)], now())
                    .map_err(fail)?
            }
            Command::Restore { id } => self.shelf()?.restore(&id, now()).map_err(fail)?,
            Command::Search { query } => {
                self.state.hits = self.shelf()?.search(&query).map_err(fail)?
            }
            Command::Export { id, exporter, languages, to } => {
                let recording = self.shelf()?.read(&id).map_err(fail)?.ok_or("recording.gone")?;
                let exporter = EXPORTERS.iter().find(|e| e.id == exporter).ok_or("export.which")?;
                let lines: Vec<Line> = recording.lines.into_iter().map(|(_, s)| s.line).collect();
                let (title, speakers) = (&recording.about.title, &recording.speakers);
                let text = (exporter.render)(&Doc { title, lines: &lines, speakers }, &languages);
                let mut to = PathBuf::from(to);
                if to.extension().is_none() {
                    to.set_extension(exporter.ext);
                }
                fs::write(to, text).map_err(|_| "export.write")?;
            }
            Command::Share { recording } => self.share(recording)?,
            Command::Unshare { room } => self.unshare(room)?,
            Command::SignIn => {
                let sign_in = SignIn::begin(&self.config.server).map_err(code)?;
                self.state.login = Some(sign_in.code().into());
                let (results, http) = (self.results.clone(), self.http.clone());
                let task = tokio::spawn(async move {
                    let _ = results.send(Msg::SignedIn(sign_in.finish(&http).await)).await; // dropped: the hub has ended
                });
                if let Some(earlier) = self.signing.replace(task.abort_handle()) {
                    earlier.abort();
                }
            }
            Command::SignOut => {
                keys::set("identity", None).map_err(code)?;
                if let Some(signing) = self.signing.take() {
                    signing.abort();
                }
                self.state.login = None;
                let local = keys::local().map(|(me, _)| me);
                self.sign(Who::of(None, local), "sign_out", false);
            }
            Command::CopyRotation => {
                let key = keys::rotation().map_err(code)?.ok_or("rotation.none")?;
                if !(self.config.clip)(&key) {
                    return Err("clip");
                }
                self.said.say("copied");
            }
            Command::RotationSaved => {
                keys::saved().map_err(code)?;
                self.state.rotation_unsaved = false;
            }
            Command::OwnKey { service, key } => {
                let service = Service::ALL.into_iter().find(|s| s.name() == service);
                let service = service.ok_or("keys.which")?;
                self.keys.own(service, key.as_deref()).map_err(code)?;
                self.refresh()?;
                if service == Service::Soniox && self.has_key(Service::Soniox) {
                    self.recover();
                }
            }
            Command::Captions { show } => self.state.captions = show,
            Command::DestroyAudio { id } => {
                let under_way = self.recording.as_ref().map(|(rec, _)| rec.as_str());
                retain::destroy(&self.config.tapes(), &self.state.library, under_way, &id)?
            }
            Command::Term { said, written } => {
                glossary::put(&*self.shelf()?, &said, written, now())?
            }
            // Installing relaunches the app, so never while recording.
            Command::Update if self.recording.is_some() => return Err("update.recording"),
            Command::Update => {
                (self.config.install)();
                return Err("update.install"); // it returned: it could not
            }
            Command::City { address } => {
                let (http, results) = (self.http.clone(), self.results.clone());
                tokio::spawn(async move {
                    let _ = results.send(Msg::City(city::calendar(&http, &address).await)).await; // dropped: the hub has ended
                });
            }
            // What is still being written is filed once it is written.
            Command::File { id, .. } if self.busy(&id) => return Err("recording.writing"),
            Command::File { id, event, audio } => {
                let calendar = self.state.city.as_ref().ok_or("city.none")?;
                let filing = city::begin(&*self.shelf()?, &id, calendar, &event, audio)?;
                self.file(id, filing)?
            }
        }
        Ok(())
    }

    /// Files a recording to a city in a task of its own, which says how it went.
    fn file(&mut self, id: String, filing: Filing) -> Result<(), &'static str> {
        if self.state.filing.iter().any(|w| w.id == id) {
            return Ok(()); // it is on its way
        }
        let me = self.ours.as_ref().ok_or("sign_in.first")?.me.clone();
        let (http, shelf, tapes) = (self.http.clone(), self.shelf()?, self.config.tapes());
        let (filer, results, progress) =
            (Filer { http, me, shelf, tapes }, self.results.clone(), self.progress.clone());
        self.state.filing.push(writing(&id, "filing"));
        tokio::spawn(async move {
            let said = city::file(&filer, &id, filing, |w| {
                let _ = progress.offer(Msg::Filing(w)); // dropped: the next one replaces it
            })
            .await;
            let _ = results.send(Msg::Filed(id, said)).await; // dropped: the hub has ended
        });
        Ok(())
    }

    fn under_way(&self, id: &str) -> bool {
        self.recording.as_ref().is_some_and(|(under_way, _)| under_way == id)
    }

    fn busy(&self, id: &str) -> bool {
        self.under_way(id) || self.state.writing.iter().any(|w| w.id == id)
    }

    fn has_key(&self, service: Service) -> bool {
        self.state
            .keys
            .iter()
            .find(|(name, _)| name == service.name())
            .is_some_and(|(_, from)| *from != From::Nowhere)
    }

    fn task(&self, id: &str, shelf: &Arc<Shelf>) -> Task {
        Task {
            id: id.into(),
            shelf: shelf.clone(),
            keys: self.keys.clone(),
            config: self.config.clone(),
            results: self.results.clone(),
            progress: self.progress.clone(),
            said: self.said.clone(),
            tree: self.tree.clone(),
            terms: self.state.terms.clone(),
        }
    }

    /// A recording from the sound the person chose, or from a file: a file has no live
    /// captions, and its name is the recording's title.
    fn start(&mut self, source: Source) -> Result<(), &'static str> {
        if self.recording.is_some() {
            return Err("recording.under_way");
        }
        let (shelf, started, setup) = (self.shelf()?, now(), self.state.setup.clone());
        let id = format!("{started:011x}{:08x}", getrandom::u32().map_err(|_| "random")?);
        let file = matches!(source, Source::File(_));
        let source_name = source.code().into();
        let title = match &source {
            Source::File(path) => {
                path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
            }
            _ => String::new(),
        };
        let (spoken, shown) = (setup.spoken, setup.shown);
        let captions = lane::captions(file, &shown, self.has_key(Service::Soniox));
        let initial = match &captions {
            Ok(_) => Status::Connecting,
            Err(why) => Status::AudioOnly { why: (*why).into() },
        };
        let about =
            About { started, title, spoken, shown, source: source_name, ..About::default() };
        shelf.put(&id, RECORDING, &[(id.clone(), Some(about.clone()))], started).map_err(fail)?;
        self.session_reg.set(Some(Session { id: id.clone(), started, status: initial }));
        let (mode, watching) = watch::channel(Mode::Recording);
        let rec_start = recording::Start {
            id: id.clone(),
            about,
            source,
            captions,
            mode: watching,
            shelf: shelf.clone(),
            keys: self.keys.clone(),
            config: self.config.clone(),
            terms: self.state.terms.clone(),
            frame: self.frame_reg.clone(),
            session: self.session_reg.clone(),
            level: self.level_reg.clone(),
            water: self.water_reg.clone(),
            said: self.said.clone(),
            shell: self.shell.clone(),
            screen: self.screen.clone(),
            root: self.tree.clone(),
            results: self.results.clone(),
        };
        let post = self.task(&id, &shelf);
        self.tree.spawn("recording", async move {
            let done = recording::run(rec_start).await;
            post.after_seal(done.sealed).await;
        });
        self.recording = Some((id, mode));
        Ok(())
    }

    fn mode(&self, mode: Mode) -> Result<(), &'static str> {
        self.recording.as_ref().ok_or("recording.none")?.1.send_replace(mode);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some((id, mode)) = self.recording.take() {
            mode.send_replace(Mode::Stopped);
            self.session_reg.set(None);
            self.frame_reg.set(Frame::default());
            self.level_reg.set(SoundLevel::default());
            self.water_reg.change(|current| (*current != Water::default()).then(Water::default));
            self.state.writing.push(writing(&id, "closing"));
        }
    }

    /// Who signs decides whose records these are, and where keys come from. Each identity has a
    /// shelf of its own on a shared device; only a signed-in person reaches our server or `State.me`.
    fn sign(&mut self, who: Who, why: &'static str, made: bool) {
        let (from, to) = (self.who.code(), who.code());
        if let Who::Nobody(code) = &who {
            warn!("identity.who", from = from, to = to, why = why, made = made);
            self.said.say(code);
        } else {
            info!("identity.who", from = from, to = to, why = why, made = made);
        }
        self.who = who;
        let records = self.config.data.join("records");
        let shelf = |me: &Me| {
            Arc::new(Shelf::new(records.join(me.did.replace(':', "-")), &me.did, me.key.clone()))
        };
        self.shelf = self.who.me().ok().map(shelf);
        let (server, http) = (self.config.server.clone(), self.http.clone());
        self.ours = self.who.person().cloned().map(|me| Ours { server, me, http });
        self.keys = Arc::new(Chain::new(self.ours.clone()));
        // Never expose the device's `did:key` in `State.me`: it is only for a signed-in person.
        (self.state.me, self.opened) = (self.who.person().map(|me| me.did.clone()), None);
        self.state.rotation_unsaved = keys::unsaved();
        // An invite not carried over yet goes to whoever is signed in now.
        if let (Some(_), Some(token)) = (&self.ours, self.invite.clone()) {
            let (chain, results) = (self.keys.clone(), self.results.clone());
            let claim = tokio::spawn(async move {
                let claimed = again(|| chain.claim(&token)).await;
                // Gone over, or refused for good: either way it is not sent again.
                let done = keys::carried(&token).and(claimed).map_err(code);
                let _ = results.send(Msg::Claimed(done)).await; // dropped: the hub has ended
            });
            if let Some(earlier) = self.signing.replace(claim.abort_handle()) {
                earlier.abort();
            }
        }
        let swept = self.refresh().and_then(|()| self.retain());
        if let Err(code) = swept.and(self.rooms()).and(self.resume()) {
            self.said.say(code);
        }
        self.recover();
        self.migrate();
    }

    /// Lets go of the audio the person's choice says goes now.
    fn retain(&mut self) -> Result<(), &'static str> {
        retain::sweep(&self.config.tapes(), &mut self.state, now())
    }

    /// Filings a restart or a sign-out left on their way go on.
    fn resume(&mut self) -> Result<(), &'static str> {
        let owed = self.shelf.as_ref().map(|shelf| shelf.kept::<Filing>(FILING)).transpose();
        let owed = owed.map_err(fail)?.into_iter().flatten().filter(|(_, f)| f.state.is_empty());
        owed.collect::<Vec<_>>().into_iter().try_for_each(|(id, filing)| self.file(id, filing))
    }

    /// A community invite the 0.8 app left here is carried over once, and nothing is asked of
    /// its holder: to the identity kept here, or, with `make`, to one made for them now. Its
    /// token is read where the old app kept it and copied nowhere. Kept as-is rather than moving
    /// invites to the device's local identity (`docs/build/phases/P1.md` §3.14.8).
    fn carry(&mut self, make: bool) -> Result<(), &'static str> {
        let path = self.config.old.as_ref().map(|old| old.join("Secrets/community-invite.json"));
        self.invite = path.as_deref().map(keys::invite).transpose().map_err(code)?.flatten();
        if make && self.invite.is_some() {
            let (http, directory) = (self.http.clone(), self.config.directory.clone());
            let results = self.results.clone();
            let made = tokio::spawn(async move {
                let me = again(|| keys::create(&http, &directory)).await;
                let _ = results.send(Msg::SignedIn(me)).await; // dropped: the hub has ended
            });
            self.signing = Some(made.abort_handle());
        }
        Ok(())
    }

    /// The library the 0.8 app left here comes to whoever is signed in: once, on its own.
    /// Never moved to the device's local identity because `moved.json` records one recipient
    /// once (`docs/build/phases/P1.md` §3.14.8).
    fn migrate(&mut self) {
        let (Some(shelf), Some(old), Some(me)) =
            (self.shelf.clone(), self.config.old.clone(), self.state.me.clone())
        else {
            return;
        };
        if !std::mem::replace(&mut self.moving, true) {
            let (data, results, progress) =
                (self.config.data.clone(), self.results.clone(), self.progress.clone());
            tokio::task::spawn_blocking(move || {
                let progress = |writing| {
                    let _ = progress.offer(Msg::Moving(writing)); // dropped: the next one replaces it
                };
                let moved = migrate::run((&old, &data), &shelf, &me, now, progress);
                let _ = results.send_blocking(Msg::Moved(moved)); // dropped: the hub has ended
            });
        }
    }

    /// The person's rooms, with their links. The live one has a task that keeps its slot
    /// showing the captions.
    fn rooms(&mut self) -> Result<(), &'static str> {
        let shares =
            self.shelf.as_ref().map(|shelf| shelf.kept(SHARE)).transpose().map_err(fail)?;
        let server = &self.config.server;
        let room = |(id, share): (String, Share)| -> Result<Room, &'static str> {
            let link = room::link(server, &id, &keys::derive("room", &id).map_err(code)?);
            Ok(Room { id, recording: share.recording, link })
        };
        self.state.rooms = shares.into_iter().flatten().map(room).collect::<Result<_, _>>()?;
        let live = self.state.rooms.iter().find(|room| room.recording.is_none());
        let live = live.map(|room| room.id.clone());
        if self.slot.as_ref().map(|(id, _)| id) != live.as_ref() {
            self.slot.take().into_iter().for_each(|(_, task)| task.abort());
            if let (Some(id), Some(ours)) = (live, self.ours.clone()) {
                let task =
                    tokio::spawn(slot(ours, id.clone(), self.frame_reg.read(), self.said.clone()));
                self.slot = Some((id, task.abort_handle()));
            }
        }
        Ok(())
    }

    /// A room for one recording, or for the live captions; one each is enough. It is made on
    /// our server, kept as a record, then filled.
    fn share(&mut self, recording: Option<String>) -> Result<(), &'static str> {
        let (ours, shelf) = (self.ours.clone().ok_or("sign_in.first")?, self.shelf()?);
        if self.state.rooms.iter().any(|room| room.recording == recording) {
            return Ok(());
        }
        let opened = recording.as_deref().map(|id| shelf.read(id)).transpose().map_err(fail)?;
        let opened = opened.map(|read| read.ok_or("recording.gone")).transpose()?;
        let tapes = self.config.tapes();
        let pages = opened.map(|recording| room::pages(&open(recording, &tapes))).transpose();
        let (pages, results) = (pages.map_err(code)?.unwrap_or_default(), self.results.clone());
        let id = keys::fresh().map_err(code)?;
        let key = keys::derive("room", &id).map_err(code)?;
        tokio::spawn(async move {
            let shared = async {
                room::write(&ours, &id, &key, Write::Make).await.map_err(code)?;
                let share = [(id.clone(), Some(Share { recording }))];
                shelf.put("", SHARE, &share, now()).map_err(fail)?;
                for page in pages {
                    room::write(&ours, &id, &key, Write::Log(&page)).await.map_err(code)?;
                }
                Ok(())
            };
            let _ = results.send(Msg::Shared(shared.await)).await; // dropped: the hub has ended
        });
        Ok(())
    }

    /// Takes a room off the list at once, so that nothing writes to it any more, then away
    /// from our server; if it is still there, it is back on the list.
    fn unshare(&mut self, id: String) -> Result<(), &'static str> {
        let (ours, shelf) = (self.ours.clone().ok_or("sign_in.first")?, self.shelf()?);
        let room = self.state.rooms.iter().find(|room| room.id == id).ok_or("room.gone")?;
        let (share, key) = (Share { recording: room.recording.clone() }, keys::derive("room", &id));
        shelf.put::<Share>("", SHARE, &[(id.clone(), None)], now()).map_err(fail)?;
        let (key, results) = (key.map_err(code)?, self.results.clone());
        tokio::spawn(async move {
            let gone = match room::write(&ours, &id, &key, Write::Gone).await {
                Ok(()) | Err(End::Fatal("room.gone")) => Ok(()),
                Err(end) => {
                    let back = shelf.put("", SHARE, &[(id, Some(share))], now()).map_err(fail);
                    back.and(Err(code(end)))
                }
            };
            let _ = results.send(Msg::Shared(gone)).await; // dropped: the hub has ended
        });
        self.rooms()
    }

    /// The library and the open recording, read again after anything that may change them.
    fn refresh(&mut self) -> Result<(), &'static str> {
        self.state.keys =
            Service::ALL.iter().map(|s| (s.name().into(), self.keys.from(*s))).collect();
        self.state.left = self.keys.left();
        let Some(shelf) = &self.shelf else {
            (self.state.library, self.state.open, self.state.hits) = Default::default();
            self.state.terms.clear();
            return Ok(());
        };
        let tapes = self.config.tapes();
        self.state.library = shelf.list().map_err(fail)?.iter().map(|r| row(r, &tapes)).collect();
        let read = self.opened.as_deref().map(|id| shelf.read(id)).transpose().map_err(fail)?;
        self.state.open = read.flatten().map(|recording| open(recording, &tapes));
        self.state.terms = glossary::read(shelf)?;
        if let Some(open) = &mut self.state.open {
            let mut filed = shelf.kept::<Filing>(FILING).map_err(fail)?.into_iter();
            open.filed = filed.find_map(|(id, filing)| (id == open.row.id).then_some(filing));
        }
        Ok(())
    }

    /// What a stop, a crash or a failure left unwritten is written now, once whatever an
    /// earlier attempt left at the service has been deleted.
    fn recover(&mut self) {
        let (Some(shelf), tapes) = (&self.shelf, self.config.tapes()) else { return };
        let key = self.has_key(Service::Soniox);
        let (mut owing, mut skipped, mut why) = (Vec::new(), 0usize, "");
        for row in &self.state.library {
            if self.busy(&row.id) {
                continue;
            }
            // Nothing is written again without its audio, nor for one moved from 0.8: it keeps the
            // transcript it had there, or the lack of one, and its sound is not sent anywhere.
            let moved = row.draft
                && row.audio
                && shelf.about(&row.id).ok().flatten().is_none_or(|r| r.about.moved);
            match write::owed(row.draft, row.audio, moved, key) {
                write::Owed::Yes => owing.push(self.task(&row.id, shelf)),
                write::Owed::Waits(reason) => {
                    skipped += 1;
                    why = reason;
                }
                write::Owed::No => {}
            }
        }
        if skipped > 0 {
            info!("write.skipped", n = skipped, why = why);
        }
        if !key {
            return;
        }
        self.state.writing.extend(owing.iter().map(|task| writing(&task.id, "closing")));
        let (soniox, keys, said) =
            (self.config.soniox.clone(), self.keys.clone(), self.said.clone());
        tokio::spawn(zutalk_log::carry(async move {
            if let End::Retry(code) | End::Fatal(code) = write::sweep(&tapes, &soniox, &*keys).await
            {
                said.say(code);
            }
            for task in owing {
                drop(tokio::spawn(zutalk_log::carry(task.finish_recovered()))); // dropped: detached handle until P1-21 moves finishing onto the tree
            }
        }));
    }
}

/// A recording as it is read.
fn open(recording: Recording, tapes: &Path) -> Open {
    let (row, lines) = (row(&recording, tapes), recording.lines);
    Open { row, lines, speakers: recording.speakers, note: recording.note, filed: None }
}

/// Keeps a live room's slot showing what the captions show: the latest, at most every
/// `SLOT_MS`, and nothing between recordings. Its trouble is said once, when it changes.
async fn slot(ours: Ours, id: String, mut live: Reading<Frame>, voice: Voice) {
    let key = match keys::derive("room", &id) {
        Ok(key) => key,
        Err(end) => return voice.say(code(end)),
    };
    let mut said = None;
    let (_, mut current) = live.now();
    loop {
        let bytes =
            serde_json::to_vec(&current.columns).ok().filter(|_| !current.columns.is_empty());
        let wrote = room::write(&ours, &id, &key, Write::Slot(bytes.as_deref())).await.err();
        if wrote != said {
            wrote.into_iter().for_each(|end| voice.say(code(end)));
            said = wrote;
        }
        tokio::time::sleep(Duration::from_millis(SLOT_MS)).await;
        let Some((_, next)) = live.next().await else { break };
        current = next;
    }
}

/// One change a person makes: one more record at the path it changes.
fn apply(shelf: &Shelf, id: &str, change: Change) -> Result<(), &'static str> {
    let done = match change {
        Change::Title { text } => {
            let about = shelf.about(id).map_err(fail)?.ok_or("recording.gone")?.about;
            shelf.put(id, RECORDING, &[(id.into(), Some(About { title: text, ..about }))], now())
        }
        Change::Line { key, language, text } => {
            let lines = shelf.read(id).map_err(fail)?.ok_or("recording.gone")?.lines;
            let (key, mut said) = lines.into_iter().find(|(k, _)| *k == key).ok_or("line.gone")?;
            let piece =
                if language == said.line.language { "text".into() } else { language.clone() };
            match piece.as_str() {
                "text" => said.line.text = text,
                _ => _ = said.line.translations.insert(language, text),
            }
            said.via.insert(piece, "hand".into());
            shelf.put(id, LINE, &[(key, Some(said))], now())
        }
        Change::Speaker { label, name } => {
            shelf.put(id, SPEAKER, &[(format!("{id}.{label}"), Some(Words { text: name }))], now())
        }
        Change::Note { text } => shelf.put(id, NOTE, &[(id.into(), Some(Words { text }))], now()),
    };
    done.map_err(fail)
}

/// Whole-recording transcription task run after a recording seals its tape (`P1-19` moves this to `work.rs`).
struct Task {
    id: String,
    shelf: Arc<Shelf>,
    keys: Arc<Chain>,
    config: Arc<Config>,
    results: Tx<Msg>,
    progress: Tx<Msg>,
    said: Voice,
    tree: Tree,
    terms: Vec<Term>, // the glossary as it was when the task began
}

impl Task {
    fn tape(&self) -> PathBuf {
        self.config.tapes().join(&self.id)
    }

    async fn finish_recovered(self) {
        let sealed = recording::seal(&self.tree, &self.shelf, self.tape(), &self.id).await;
        if let Err(code) = sealed {
            self.said.say(code);
        }
        self.after_seal(sealed).await;
    }

    async fn after_seal(self, sealed: Result<Option<About>, &'static str>) {
        match sealed {
            Ok(Some(about)) if about.length_ms == 0 => drop(fs::remove_dir_all(self.tape())),
            Ok(Some(about)) => {
                let audio = tape::kept(&self.tape());
                let key = self.keys.from(Service::Soniox) != From::Nowhere;
                match write::owed(about.take == 0, audio, about.moved, key) {
                    write::Owed::Yes => self.write().await,
                    write::Owed::Waits(why) => info!("write.skipped", n = 1usize, why = why),
                    write::Owed::No => {}
                }
            }
            Ok(None) => {} // removed meanwhile: the tape stays for a restore
            Err(_) => {}
        }
        let _ = self.results.send(Msg::Wrote(self.id.clone())).await; // dropped: the hub has ended
    }

    async fn write(&self) {
        let terms: Vec<_> = self.terms.iter().cloned().map(|t| (t.said, t.written)).collect();
        let written = again(|| async {
            // Removed meanwhile, written, or its audio destroyed: nothing to do.
            let about = match self.shelf.about(&self.id) {
                Ok(Some(read)) if read.about.take == 0 && tape::kept(&self.tape()) => read.about,
                _ => return Ok(()),
            };
            let key = keys::derive("tape", &self.id)?;
            let (soniox, openrouter) = (self.config.soniox.clone(), self.config.openrouter.clone());
            let (id, spoken, into, topic) =
                (self.id.clone(), about.spoken, about.shown, about.title);
            let (model, terms) = (WHOLE.into(), terms.clone());
            let plan = write::Plan { id, spoken, into, terms, topic, model, soniox, openrouter };
            let progress = |step: &str, done: usize, of: usize| {
                let (done, of) = (done as u32, of as u32);
                let msg = Msg::Writing(Writing { done, of, ..writing(&self.id, step) });
                let _ = self.progress.offer(msg); // dropped: the next one replaces it
            };
            let tape = (self.tape(), key);
            match write::write(&self.shelf, (&tape.0, &tape.1), &plan, &*self.keys, now, progress)
                .await
            {
                End::Done => Ok(()),
                End::Retry(code) => {
                    let _ = self.progress.offer(Msg::Writing(writing(&self.id, code))); // dropped: the next one replaces it
                    Err(End::Retry(code))
                }
                end => Err(end),
            }
        });
        if let Err(end) = written.await {
            self.said.say(code(end));
        }
    }
}
