//! The hub. Commands, and reports from the work under way, come into one mailbox; after each,
//! the state is sent whole if it changed. A recording is a task of its own, from its first
//! block to its written transcript, so a stop hands the app back at once; so is whatever goes
//! to a room or to a city.

use crate::ai::write;
use crate::app::proto::{Caption, Change, Column, Command, Event, From, Open, Room, Row, Session};
use crate::app::proto::{Sound, State, Status, Writing};
use crate::capture::audio::{self, Capture, Notice, Source};
use crate::capture::clock::{Block, wall_clock_policy, wall_ms};
use crate::capture::tape;
use crate::id::keys::{self, Chain, Me, Ours, Secret, Service, SignIn, Who};
use crate::knowledge::glossary::{self, Term};
use crate::live::lane::{self, Health, Mode, Stream};
use crate::live::lang::profile;
use crate::live::lines::Line;
use crate::out::city::{self, Calendar, Filer, Filing};
use crate::out::export::{Doc, EXPORTERS};
use crate::out::room::{self, Write};
use crate::record::shelf::{
    About, FILING, LINE, NOTE, RECORDING, Recording, SHARE, SPEAKER, Said, Share,
};
use crate::record::shelf::{Shelf, Words};
use crate::record::{migrate, retain};
use crate::rt::stats::{self, Stats};
use crate::rt::supervise::{End, Tree, again};
use std::path::{Path, PathBuf};
use std::{fs, sync::Arc, time::Duration};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::AbortHandle;
use zutalk_log::{Level, info, warn};

const REALTIME: &str = "stt-rt-v6";
const WHOLE: &str = "stt-async-v6";
/// Captions are drawn at most this often: once a 100 ms block, as new words come no faster.
const FRAME_MS: u64 = 100;
const KEEP: usize = 12; // the finished lines a column keeps on screen
/// A live room is written at most this often (DESIGN.md, rooms). Origin to be traced.
const SLOT_MS: u64 = 250;

/// How sound is heard: the one port the tests replace.
pub type Hear =
    fn(Source, Box<dyn FnMut(Block) + Send>, Box<dyn FnMut(Notice) + Send>) -> Result<Capture, End>;

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
    /// How sound is heard.
    pub hear: Hear,
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
        let hear: Hear =
            |source, sink, notice| Capture::start(source, wall_clock_policy, sink, notice);
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

enum Msg {
    Do(Command),
    Live(String, Vec<Column>),
    Status(String, Status),
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
    Say(&'static str),
}

/// The core, running. Commands go in; events come out through the function it was given.
pub struct Core {
    tx: mpsc::UnboundedSender<Msg>,
    stats: Stats,
}

impl Core {
    /// Starts on the current runtime. The key store must be open (`keys::open_store`).
    pub fn start(config: Config, out: impl Fn(Event) + Send + 'static) -> Result<Core, End> {
        let _ = rustls::crypto::ring::default_provider().install_default(); // dropped: installing the same TLS provider again is harmless
        let http = reqwest::Client::builder().build().map_err(|_| End::Fatal("tls"))?;
        let (tx, mail) = mpsc::unbounded_channel();
        let stats = Stats::new();
        let root = Tree::root("core", stats.clone());
        let (config, out, keys) = (Arc::new(config), Box::new(out), Arc::new(Chain::new(None)));
        let (state, sent, ours, shelf, recording, opened, signing, slot, invite, moving) =
            Default::default();
        let live = watch::Sender::new(Vec::new());
        let hub = Hub {
            config,
            tx: tx.clone(),
            tree: root.clone(),
            out,
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
            live,
            slot,
            moving,
        };
        root.spawn("hub", hub.run(mail));
        root.spawn("stats", stats::publish(stats.clone()));
        Ok(Core { tx, stats })
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Msg::Do(command)); // dropped: unbounded mailbox; fails only after the hub has stopped
    }

    /// The shell's report from looking for a newer version of the app: one downloaded and
    /// ready to install, or what kept it from being had.
    pub fn looked(&self, found: Result<String, &'static str>) {
        let _ = self.tx.send(Msg::Looked(found)); // dropped: unbounded mailbox; fails only after the hub has stopped
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
    tx: Sender,
    tree: Tree,
    out: Box<dyn Fn(Event) + Send>,
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
    live: watch::Sender<Vec<Column>>, // the captions under way; none between recordings
    slot: Option<(String, AbortHandle)>, // the live room, and what keeps its slot
    moving: bool,                 // the 0.8 library is being moved in
}

impl Hub {
    async fn run(mut self, mut mail: mpsc::UnboundedReceiver<Msg>) {
        // None kept yet is the default; one that does not read is said, then replaced.
        if let Ok(json) = fs::read(self.config.data.join("settings.json")) {
            match serde_json::from_slice(&json) {
                Ok(setup) => self.state.setup = setup,
                Err(_) => (self.out)(Event::Notice("settings".into())),
            }
        }
        self.state.languages =
            crate::live::lang::ALL.iter().map(|profile| profile.lang.into()).collect();
        self.state.exporters = EXPORTERS.iter().map(|exporter| exporter.id.into()).collect();
        self.state.devices = audio::devices();
        let kept = keys::kept();
        // An identity that does not read is said, not replaced by one made for an invite.
        if let Err(code) = self.carry(matches!(kept, Ok(None))) {
            (self.out)(Event::Notice(code.into()));
        }
        let kept = kept.unwrap_or_else(|end| {
            (self.out)(Event::Notice(code(end).into()));
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
            if let Err(code) = self.take(msg) {
                (self.out)(Event::Notice(code.into()));
            }
            self.publish();
        }
        self.tree.cancel();
    }

    fn publish(&mut self) {
        if self.sent.as_ref() != Some(&self.state) {
            self.sent = Some(self.state.clone());
            (self.out)(Event::State(Box::new(self.state.clone())));
        }
    }

    fn take(&mut self, msg: Msg) -> Result<(), &'static str> {
        let reread = !matches!(msg, Msg::Live(..) | Msg::Status(..));
        match msg {
            Msg::Do(command) => self.command(command)?,
            Msg::Live(id, columns) if self.under_way(&id) => {
                self.live.send_replace(columns.clone());
                (self.out)(Event::Live(columns));
            }
            Msg::Status(id, status) => {
                let session = self.state.session.as_mut().filter(|s| s.id == id);
                session.into_iter().for_each(|session| session.status = status.clone());
            }
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
                (self.out)(Event::Notice(said.into()));
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
            Msg::Say(code) => return Err(code),
            Msg::Live(..) | Msg::Ended(..) => {} // from a recording that has stopped
        }
        if reread { self.refresh() } else { Ok(()) }
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
                let (tx, http) = (self.tx.clone(), self.http.clone());
                let task = tokio::spawn(async move {
                    let _ = tx.send(Msg::SignedIn(sign_in.finish(&http).await)); // dropped: unbounded mailbox; fails only after the hub has stopped
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
                (self.out)(Event::Notice("copied".into()));
            }
            Command::RotationSaved => {
                keys::saved().map_err(code)?;
                self.state.rotation_unsaved = false;
            }
            Command::OwnKey { service, key } => {
                let service = Service::ALL.into_iter().find(|s| s.name() == service);
                self.keys.own(service.ok_or("keys.which")?, key.as_deref()).map_err(code)?;
            }
            Command::Captions { show } => self.state.captions = show,
            Command::DestroyAudio { id } => {
                retain::destroy(&self.config.tapes(), &self.state, &id)?
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
                let (http, tx) = (self.http.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let _ = tx.send(Msg::City(city::calendar(&http, &address).await)); // dropped: unbounded mailbox; fails only after the hub has stopped
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
        let (filer, tx) = (Filer { http, me, shelf, tapes }, self.tx.clone());
        self.state.filing.push(writing(&id, "filing"));
        tokio::spawn(async move {
            let said = city::file(&filer, &id, filing, |w| drop(tx.send(Msg::Filing(w)))).await; // dropped: unbounded mailbox; fails only after the hub has stopped
            let _ = tx.send(Msg::Filed(id, said)); // dropped: unbounded mailbox; fails only after the hub has stopped
        });
        Ok(())
    }

    fn under_way(&self, id: &str) -> bool {
        self.recording.as_ref().is_some_and(|(under_way, _)| under_way == id)
    }

    fn busy(&self, id: &str) -> bool {
        self.under_way(id) || self.state.writing.iter().any(|w| w.id == id)
    }

    fn task(&self, id: &str, shelf: &Arc<Shelf>) -> Task {
        let (config, tx, keys, shelf) =
            (self.config.clone(), self.tx.clone(), self.keys.clone(), shelf.clone());
        Task { id: id.into(), shelf, keys, config, tx, terms: self.state.terms.clone() }
    }

    /// A recording from the sound the person chose, or from a file: a file has no live
    /// captions, and its name is the recording's title.
    fn start(&mut self, source: Source) -> Result<(), &'static str> {
        if self.recording.is_some() {
            return Err("recording.under_way");
        }
        let (shelf, started, setup) = (self.shelf()?, now(), self.state.setup.clone());
        let id = format!("{started:011x}{:08x}", getrandom::u32().map_err(|_| "random")?);
        let (source_name, title) = match &source {
            Source::File(path) => {
                let name = path.file_stem().map(|name| name.to_string_lossy().into_owned());
                ("file".into(), name.unwrap_or_default())
            }
            _ => (format!("{:?}", setup.sound).to_lowercase(), String::new()),
        };
        let (spoken, shown) = (setup.spoken, setup.shown);
        let about =
            About { started, title, spoken, shown, source: source_name, ..About::default() };
        shelf.put(&id, RECORDING, &[(id.clone(), Some(about.clone()))], started).map_err(fail)?;
        let (mode, watching) = watch::channel(Mode::Recording);
        tokio::spawn(self.task(&id, &shelf).record(source, about, watching));
        self.state.session = Some(Session { id: id.clone(), started, status: Status::Connecting });
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
            self.live.send_replace(Vec::new());
            self.state.session = None;
            self.state.writing.push(writing(&id, "closing"));
        }
    }

    /// Who signs decides whose records these are, and where keys come from. Each identity has a
    /// shelf of its own on a shared device; only a signed-in person reaches our server or `State.me`.
    fn sign(&mut self, who: Who, why: &'static str, made: bool) {
        let (from, to) = (self.who.code(), who.code());
        if let Who::Nobody(code) = &who {
            warn!("identity.who", from = from, to = to, why = why, made = made);
            (self.out)(Event::Notice((*code).into()));
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
            let (chain, tx) = (self.keys.clone(), self.tx.clone());
            let claim = tokio::spawn(async move {
                let claimed = again(|| chain.claim(&token)).await;
                // Gone over, or refused for good: either way it is not sent again.
                let _ = tx.send(Msg::Claimed(keys::carried(&token).and(claimed).map_err(code))); // dropped: unbounded mailbox; fails only after the hub has stopped
            });
            if let Some(earlier) = self.signing.replace(claim.abort_handle()) {
                earlier.abort();
            }
        }
        let swept = self.refresh().and_then(|()| self.retain());
        if let Err(code) = swept.and(self.rooms()).and(self.resume()) {
            (self.out)(Event::Notice(code.into()));
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
            let tx = self.tx.clone();
            let made = tokio::spawn(async move {
                let me = again(|| keys::create(&http, &directory)).await;
                let _ = tx.send(Msg::SignedIn(me)); // dropped: unbounded mailbox; fails only after the hub has stopped
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
            let (data, tx) = (self.config.data.clone(), self.tx.clone());
            tokio::task::spawn_blocking(move || {
                let progress = |writing| drop(tx.send(Msg::Moving(writing))); // dropped: unbounded mailbox; fails only after the hub has stopped
                let moved = migrate::run((&old, &data), &shelf, &me, now, progress);
                let _ = tx.send(Msg::Moved(moved)); // dropped: unbounded mailbox; fails only after the hub has stopped
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
                    tokio::spawn(slot(ours, id.clone(), self.live.subscribe(), self.tx.clone()));
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
        let (pages, tx) = (pages.map_err(code)?.unwrap_or_default(), self.tx.clone());
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
            let _ = tx.send(Msg::Shared(shared.await)); // dropped: unbounded mailbox; fails only after the hub has stopped
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
        let (key, tx) = (key.map_err(code)?, self.tx.clone());
        tokio::spawn(async move {
            let gone = match room::write(&ours, &id, &key, Write::Gone).await {
                Ok(()) | Err(End::Fatal("room.gone")) => Ok(()),
                Err(end) => {
                    let back = shelf.put("", SHARE, &[(id, Some(share))], now()).map_err(fail);
                    back.and(Err(code(end)))
                }
            };
            let _ = tx.send(Msg::Shared(gone)); // dropped: unbounded mailbox; fails only after the hub has stopped
        });
        self.rooms()
    }

    /// The library and the open recording, read again after anything that may change them.
    fn refresh(&mut self) -> Result<(), &'static str> {
        let from = |own| {
            if own {
                From::Own
            } else if self.ours.is_some() {
                From::Granted
            } else {
                From::Nowhere
            }
        };
        self.state.keys =
            Service::ALL.iter().map(|s| (s.name().into(), from(self.keys.has_own(*s)))).collect();
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
        let owing = self.state.library.iter().filter(|row| row.draft && !self.busy(&row.id));
        // Nothing is written again without its audio, nor for one moved from 0.8: it keeps the
        // transcript it had there, or the lack of one, and its sound is not sent anywhere.
        let owed = |id: &str| shelf.about(id).ok().flatten().is_some_and(|r| !r.about.moved);
        let owing = owing.filter(|row| row.audio && owed(&row.id));
        let owing: Vec<Task> = owing.map(|row| self.task(&row.id, shelf)).collect();
        self.state.writing.extend(owing.iter().map(|task| writing(&task.id, "closing")));
        let (soniox, keys, tx) = (self.config.soniox.clone(), self.keys.clone(), self.tx.clone());
        tokio::spawn(async move {
            if let End::Retry(code) | End::Fatal(code) = write::sweep(&tapes, &soniox, &*keys).await
            {
                let _ = tx.send(Msg::Say(code)); // dropped: unbounded mailbox; fails only after the hub has stopped
            }
            owing.into_iter().for_each(|task| drop(tokio::spawn(task.finish()))); // dropped: detached handle until P1-21 moves finishing onto the tree
        });
    }
}

/// A recording as it is read.
fn open(recording: Recording, tapes: &Path) -> Open {
    let (row, lines) = (row(&recording, tapes), recording.lines);
    Open { row, lines, speakers: recording.speakers, note: recording.note, filed: None }
}

/// Keeps a live room's slot showing what the captions show: the latest, at most every
/// `SLOT_MS`, and nothing between recordings. Its trouble is said once, when it changes.
async fn slot(ours: Ours, id: String, mut live: watch::Receiver<Vec<Column>>, tx: Sender) {
    let key = match keys::derive("room", &id) {
        Ok(key) => key,
        Err(end) => return drop(tx.send(Msg::Say(code(end)))), // dropped: unbounded mailbox; fails only after the hub has stopped
    };
    let mut said = None;
    live.mark_changed(); // whatever the slot held before is replaced
    while live.changed().await.is_ok() {
        let columns = live.borrow_and_update().clone();
        let frame = serde_json::to_vec(&columns).ok().filter(|_| !columns.is_empty());
        let wrote = room::write(&ours, &id, &key, Write::Slot(frame.as_deref())).await.err();
        if wrote != said {
            wrote.into_iter().for_each(|end| drop(tx.send(Msg::Say(code(end))))); // dropped: unbounded mailbox; fails only after the hub has stopped
            said = wrote;
        }
        tokio::time::sleep(Duration::from_millis(SLOT_MS)).await;
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

type Sender = mpsc::UnboundedSender<Msg>;

/// One recording, from its first block to its written transcript.
struct Task {
    id: String,
    shelf: Arc<Shelf>,
    keys: Arc<Chain>,
    config: Arc<Config>,
    tx: Sender,
    terms: Vec<Term>, // the glossary as it was when the task began
}

impl Task {
    fn tell(&self, msg: Msg) {
        let _ = self.tx.send(msg); // dropped: unbounded mailbox; fails only after the hub has stopped
    }

    fn tape(&self) -> PathBuf {
        self.config.tapes().join(&self.id)
    }

    async fn record(self, source: Source, about: About, mode: watch::Receiver<Mode>) {
        let live = !matches!(source, Source::File(_));
        if let Err(code) = self.listen(source, &about, live, mode).await {
            // Whatever ends the listening ends the recording; what was kept is kept.
            self.tell(Msg::Say(code));
            self.tell(Msg::Ended(self.id.clone()));
        }
        self.finish().await;
    }

    /// Hears until the recording stops: the tape, the captions, and the live draft, which is
    /// the recording's text until its transcript is written. The draft is kept line by line,
    /// so a crash keeps what was said before it.
    async fn listen(
        &self,
        source: Source,
        about: &About,
        live: bool,
        mut mode: watch::Receiver<Mode>,
    ) -> Result<(), &'static str> {
        let key = keys::derive("tape", &self.id).map_err(code)?;
        let mut tape = tape::Writer::create(&self.tape(), &key).map_err(|_| "tape")?;
        let streams = if live { lane::plan(&about.shown) } else { Vec::new() };
        let ((audio, _), (lanes, mut heard)) = (broadcast::channel(64), mpsc::channel(64));
        let (endpoint, model, spoken) =
            (self.config.realtime.clone(), REALTIME.into(), about.spoken.clone());
        let setup = lane::Setup { endpoint, model, spoken, ..lane::Setup::default() };
        for (n, stream) in streams.iter().cloned().enumerate() {
            let setup = glossary::live(&self.terms, &stream, setup.clone());
            let (keys, lanes, audio, mode) =
                (self.keys.clone(), lanes.clone(), audio.subscribe(), mode.clone());
            tokio::spawn(
                async move { lane::run(n, stream, &setup, keys, audio, mode, lanes).await },
            );
        }
        drop(lanes);
        let (tx, id, mut kept) = (self.tx.clone(), self.id.clone(), true);
        let sink = move |block: Block| {
            // Without the tape there is no recording: say so, and end it.
            if kept && tape.append(&block).is_err() {
                kept = false;
                let _ = tx.send(Msg::Say("tape")); // dropped: unbounded mailbox; fails only after the hub has stopped
                let _ = tx.send(Msg::Ended(id.clone())); // dropped: unbounded mailbox; fails only after the hub has stopped
            }
            let _ = audio.send(Arc::new(block)); // dropped: broadcast fails only when no caption lanes are listening
        };
        let (tx, id) = (self.tx.clone(), self.id.clone());
        let notice = move |notice| {
            let msg = match notice {
                Notice::Ended => Msg::Ended(id.clone()),
                Notice::Restarted(code) | Notice::Fatal(code) => Msg::Say(code),
            };
            let _ = tx.send(msg); // dropped: unbounded mailbox; fails only after the hub has stopped
        };
        let mut capture =
            Some((self.config.hear)(source, Box::new(sink), Box::new(notice)).map_err(code)?);
        let (mut draft, mut n) = (self.shelf.open(&self.id, LINE).map_err(fail)?, 0);
        let (mut view, mut frame) = (View::new(&about.shown, streams), tick());
        let (mut open, quiet) =
            (!view.streams.is_empty(), if live { "captions.off" } else { "file" });
        while capture.is_some() || open {
            tokio::select! {
                changed = mode.changed(), if capture.is_some() => {
                    let now = changed.map_or(Mode::Stopped, |()| *mode.borrow_and_update());
                    match (now, &capture) {
                        (Mode::Paused, Some(capture)) => capture.pause(),
                        (Mode::Recording, Some(capture)) => capture.resume(),
                        _ => capture = None, // the last blocks are delivered, the tape closed
                    }
                    (view.paused, view.dirty) = (now == Mode::Paused, true);
                }
                event = heard.recv(), if open => match event {
                    None => open = false,
                    Some(event) => {
                        for said in drafted(&event) {
                            draft.put(&format!("{}.0.{n:05}", self.id), Some(&said), now())
                                .map_err(fail)?;
                            n += 1;
                        }
                        view.take(event);
                    }
                },
                _ = frame.tick(), if view.dirty => {
                    self.tell(Msg::Live(self.id.clone(), view.columns()));
                    let status = status(view.paused, &view.health, quiet);
                    if view.told.replace(status.clone()) != Some(status.clone()) {
                        self.tell(Msg::Status(self.id.clone(), status));
                    }
                    view.dirty = false;
                }
            }
        }
        draft.sync().map_err(fail)
    }

    /// The tape is closed: a torn tail is cut off, its length goes on the record, and its
    /// transcript is written. A recording that kept no sound is nothing, and is removed.
    async fn finish(self) {
        match self.seal().await {
            Ok(Some(0)) => drop(fs::remove_dir_all(self.tape())),
            Ok(Some(_)) => self.write().await,
            Ok(None) => {} // removed meanwhile: the tape stays for a restore
            Err(code) => self.tell(Msg::Say(code)),
        }
        self.tell(Msg::Wrote(self.id.clone()));
    }

    async fn seal(&self) -> Result<Option<u32>, &'static str> {
        let (dir, key) = (self.tape(), keys::derive("tape", &self.id).map_err(code)?);
        let length = tokio::task::spawn_blocking(move || tape::recover(&dir, &key)).await;
        let length_ms = length.map_err(|_| "tape")?.map_err(|_| "tape")?;
        let Some(recording) = self.shelf.about(&self.id).map_err(fail)? else { return Ok(None) };
        if length_ms == 0 || recording.about.length_ms != length_ms {
            let about = (length_ms > 0).then_some(About { length_ms, ..recording.about });
            self.shelf
                .put(&self.id, RECORDING, &[(self.id.clone(), about)], now())
                .map_err(fail)?;
        }
        Ok(Some(length_ms))
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
                self.tell(Msg::Writing(Writing { done, of, ..writing(&self.id, step) }))
            };
            let tape = (self.tape(), key);
            match write::write(&self.shelf, (&tape.0, &tape.1), &plan, &*self.keys, now, progress)
                .await
            {
                End::Done => Ok(()),
                End::Retry(code) => {
                    self.tell(Msg::Writing(writing(&self.id, code)));
                    Err(End::Retry(code))
                }
                end => Err(end),
            }
        });
        if let Err(end) = written.await {
            self.tell(Msg::Say(code(end)));
        }
    }
}

fn tick() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick
}

/// The draft's lines from the first stream, with where their words came from. A stretch the
/// captions missed is a line of its own.
fn drafted(event: &lane::Event) -> Vec<Said> {
    let said = |line: Line| {
        let via = line.translations.keys().map(String::as_str).chain(["text"]);
        Said { via: via.map(|piece| (piece.into(), REALTIME.into())).collect(), line }
    };
    match event {
        lane::Event::Lines { stream: 0, lines } => lines.iter().cloned().map(said).collect(),
        &lane::Event::Gap { stream: 0, t0, t1, .. } => {
            vec![said(Line { t0, t1, text: "…".into(), incomplete: true, ..Line::default() })]
        }
        _ => Vec::new(),
    }
}

/// The sentence the interface says about a recording under way.
pub fn status(paused: bool, health: &[Health], quiet: &str) -> Status {
    let why =
        health.iter().find_map(|h| if let Health::Stopped(why) = h { Some(*why) } else { None });
    if paused {
        Status::Paused
    } else if health.iter().all(|h| matches!(h, Health::Stopped(_))) {
        Status::AudioOnly { why: why.unwrap_or(quiet).into() }
    } else if health.contains(&Health::Connecting) {
        Status::Connecting
    } else if health.contains(&Health::Behind) {
        Status::Behind
    } else {
        Status::Recording
    }
}

/// What the captions show: for each stream, the last lines it finished, the ones still open,
/// and how it is doing.
struct View {
    shown: Vec<String>,
    streams: Vec<Stream>,
    said: Vec<Vec<Line>>,
    open: Vec<Vec<Line>>,
    health: Vec<Health>,
    paused: bool,
    dirty: bool,
    told: Option<Status>, // what the interface was last told
}

impl View {
    fn new(shown: &[String], streams: Vec<Stream>) -> Self {
        let n = streams.len();
        let (said, open, health) =
            (vec![Vec::new(); n], vec![Vec::new(); n], vec![Health::Connecting; n]);
        Self {
            shown: shown.to_vec(),
            streams,
            said,
            open,
            health,
            paused: false,
            dirty: true,
            told: None,
        }
    }

    fn take(&mut self, event: lane::Event) {
        match event {
            lane::Event::Lines { stream, lines } => {
                let said = &mut self.said[stream];
                said.extend(lines);
                said.drain(..said.len().saturating_sub(KEEP));
            }
            lane::Event::Tail { stream, tail } => self.open[stream] = tail,
            lane::Event::Health { stream, state } => self.health[stream] = state,
            lane::Event::Gap { .. } => {}
        }
        self.dirty = true;
    }

    /// The first two languages share a stream, which translates both ways. A line shows in a
    /// column in its language if it has been translated, else as it was said when it is
    /// finished or the stream only transcribes, else not yet.
    fn columns(&self) -> Vec<Column> {
        let column = |(j, language): (usize, &String)| {
            let n = j.saturating_sub(1);
            let caption = |line: &Line, as_said: bool| {
                let (text, written) = match line.translations.get(language) {
                    Some(text) => (text, language),
                    None if as_said || line.language == *language => (&line.text, &line.language),
                    None => return None,
                };
                let units = profile(written).units(text);
                Some(Caption { speaker: line.speaker.clone(), text: text.clone(), units })
            };
            let own = self.streams[n] == Stream::Transcribe;
            let mut open: Vec<Caption> =
                self.open[n].iter().filter_map(|l| caption(l, own)).collect();
            let tail = open.pop();
            let lines = self.said[n].iter().filter_map(|l| caption(l, true)).chain(open).collect();
            let rate = profile(language).reveal_per_s;
            Column { language: language.clone(), rate, lines, tail }
        };
        if self.streams.is_empty() {
            Vec::new()
        } else {
            self.shown.iter().enumerate().map(column).collect()
        }
    }
}
