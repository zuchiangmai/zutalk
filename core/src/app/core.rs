//! Core entry point: `Config`, `Core`, `Outlet`, and `Voice` (`P1.md` §3.2–§3.3).
//!
//! Separated from `app/hub.rs` so `hub.rs` holds only the pure hub state machine with no IO,
//! HTTP client construction, or task spawning (`P1-20` `hub.sh`).

use crate::app::hub::{HUB_MAIL_MSGS, Hub, Msg};
use crate::app::proto::{
    Device, HostState, Invalidations, Level as SoundLevel, Notices, Progress, Query, QueryReply,
    Session, State,
};
use crate::app::work::Work;
use crate::capture::audio::{self, Capture, Hear, Hearing};
use crate::capture::clock::wall_clock_policy;
use crate::capture::tape::{self, Reels};
use crate::live::frame::Frame;
use crate::live::wire::{self, Dial};
use crate::record::shelf::{self, Drafts};
use crate::rt::queue::{self, Back, Full, Tx};
use crate::rt::signal::{Reading, Register};
use crate::rt::stats::{self, Counter, Stats, Tally};
use crate::rt::supervise::{End, Running, Tree};
use crate::rt::water::{Gauge, Water};
use crate::view::query::ViewEngine;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zutalk_log::{Level, info, warn};

/// Lists the system's audio input devices: real OS query in production, fake in tests.
pub type Devices = Arc<dyn Fn() -> Vec<Device> + Send + Sync>;

/// Puts text on the system clipboard: the shell's to give. `false`: it could not.
pub type Clip = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Signals the shell to wake up and check for a newer version of the app: non-blocking.
pub type CheckUpdate = Arc<dyn Fn() + Send + Sync>;

/// Installs the newer version the shell has ready and relaunches the app: the shell's to give.
/// It returns only if it could not.
pub type Install = Arc<dyn Fn() + Send + Sync>;

/// Progress or outcome reported by the shell's update task to the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateReport {
    Checking,
    Downloading(String),
    Ready(String),
    UpToDate,
    Failed(&'static str),
}

/// What the core is started with: where things are, the ports the shell and the tests give,
/// and how much it says.
pub struct Config {
    /// The app's own folder: settings, records, tapes, secrets, logs.
    pub data: PathBuf,
    /// Ours: relays the sign-in and serves city `zu-svc` quota, transcription, and AI calls.
    pub server: String,
    /// Ephemeral ciphertext caption-sharing room server (`/rooms`).
    pub rooms: String,
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
    /// Optional bundled 0.8 handover helper binary (`zutalk-handover-08`) used when a user
    /// upgrades directly from 0.8.5/0.8.6 to 0.9 without passing through 0.8.7.
    pub handover_helper: Option<PathBuf>,
    /// How sound is heard: real audio capture in production, fake microphone in tests.
    pub hear: Hear,
    /// How the system's audio input devices are listed.
    pub devices: Devices,
    /// How a live caption lane connects to the realtime service.
    pub dial: Dial,
    /// How a recording's tape is opened for writing.
    pub reels: Reels,
    /// How a recording's live draft is opened for writing.
    pub drafts: Drafts,
    /// Puts text on the clipboard.
    pub clip: Clip,
    /// Triggers an immediate update check on the shell.
    pub check_update: CheckUpdate,
    /// Installs a newer version.
    pub install: Install,
    /// How much the log says: `ZUTALK_LOG` through `from_env`, else `info`.
    pub log: Level,
}

impl Config {
    pub fn new(data: PathBuf, server: &str) -> Self {
        let realtime = "wss://stt-rt.soniox.com/transcribe-websocket".into();
        let (soniox, openrouter) =
            ("https://api.soniox.com/v1".into(), "https://openrouter.ai/api/v1".into());
        let hear: Hear = Arc::new(|source, sink, notice, stats| {
            let capture = Capture::start(source, wall_clock_policy, stats, sink, notice)?;
            Ok(Box::new(capture) as Box<dyn Hearing>)
        });
        let devices: Devices = Arc::new(audio::devices);
        let (dial, reels, drafts) = (wire::dial(), tape::reels(), shelf::drafts());
        let (server, rooms, directory, old) =
            (server.into(), server.into(), zu::did::DIRECTORY.into(), None);
        // Without a shell to give them, nothing can be put on the clipboard, checked, or installed.
        let (clip, check_update, install): (Clip, CheckUpdate, Install) =
            (Arc::new(|_| false), Arc::new(|| {}), Arc::new(|| {}));
        let log = Level::Info;
        Self {
            data,
            server,
            rooms,
            realtime,
            soniox,
            openrouter,
            directory,
            old,
            handover_helper: None,
            hear,
            devices,
            dial,
            reels,
            drafts,
            clip,
            check_update,
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
    pub(crate) fn tapes(&self) -> PathBuf {
        self.data.join("tape")
    }
}

/// Everything the core says outward: latest-value registers readable at once or on change,
/// cloneable across the shell's pump, a live room's slot, and tests (`P1.md` §3.3, `P2.md` §3.6).
#[derive(Clone, Debug)]
pub struct Outlet {
    pub state: Reading<State>,
    pub frame: Reading<Frame>,
    pub session: Reading<Option<Session>>,
    pub level: Reading<SoundLevel>,
    pub progress: Reading<Progress>,
    pub invalidated: Reading<Invalidations>,
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
    pub(crate) fn new(stats: &Stats) -> (Self, Reading<Notices>) {
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
    root: Tree,
    frame: Register<Frame>,
    state_reading: Reading<State>,
    host_busy: Reading<bool>,
    shell: Arc<Gauge>,
    screen: Arc<Gauge>,
    views: Arc<ViewEngine>,
}

impl Core {
    /// Starts on the current runtime. The key store must be open (`keys::open_store`).
    pub fn start(config: Config) -> Result<(Core, Outlet), End> {
        let http = crate::rt::http::client()?;
        let stats = Stats::new();
        let (commands, mail) = queue::bounded(&stats, "hub.mail", HUB_MAIL_MSGS, Full::Reject); // the interface: never waits, is told `busy`
        let results = commands.with(Full::Wait); // work that is done: must arrive; whoever did it can wait
        let root = Tree::root("core", stats.clone());
        let work_tree = root.child("work");
        let config = Arc::new(config);
        let state_reg = Register::new(State::default());
        let host_busy_reg = Register::new(false);
        let frame_reg = Register::new(Frame::default());
        let session_reg = Register::new(None);
        let level_reg = Register::new(SoundLevel::default());
        let progress_reg = Register::new(Progress::default());
        let water_reg = Register::new(Water::default());
        let stats_reg = Register::new(Tally::default());
        let (views, invalidated_reading) = ViewEngine::new(work_tree.clone(), config.tapes());
        let views = Arc::new(views);
        let shell = Arc::new(Gauge::new());
        let screen = Arc::new(Gauge::new());
        let (said, said_reading) = Voice::new(&stats);
        let state_reading = state_reg.read();
        let host_busy = host_busy_reg.read();
        let outlet = Outlet {
            state: state_reading.clone(),
            frame: frame_reg.read(),
            session: session_reg.read(),
            level: level_reg.read(),
            progress: progress_reg.read(),
            invalidated: invalidated_reading,
            water: water_reg.read(),
            stats: stats_reg.read(),
            said: said_reading,
        };
        let commands_refused = stats.counter("commands.refused");
        let work = Work::new(
            config,
            http,
            results.clone(),
            said.clone(),
            root.clone(),
            work_tree,
            views.clone(),
            progress_reg,
            frame_reg.read(),
            water_reg.read(),
            host_busy_reg,
        );
        work.boot();
        let hub = Hub::new(
            work,
            root.clone(),
            state_reg,
            frame_reg.clone(),
            session_reg,
            level_reg,
            water_reg,
            shell.clone(),
            screen.clone(),
            said,
        );
        root.spawn("hub", hub.run(mail));
        root.spawn("stats", stats::publish(stats.clone(), stats_reg));
        Ok((
            Core {
                commands,
                results,
                commands_refused,
                stats,
                root,
                frame: frame_reg,
                state_reading,
                host_busy,
                shell,
                screen,
                views,
            },
            outlet,
        ))
    }

    /// Executes an L3 view query off-hub via [`ViewEngine`] (`P2.md` §3.6.2).
    pub async fn query(&self, query: Query) -> Result<QueryReply, &'static str> {
        self.views.query(query).await
    }

    pub fn send(&self, command: crate::app::proto::Command) -> Result<(), &'static str> {
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

    /// Reports progress or outcome from the shell's update checker to the hub.
    pub async fn report_update(&self, report: UpdateReport) {
        let _ = self.results.send(Msg::UpdateReported(report)).await; // dropped: the hub has ended
    }

    /// The shell's report from looking for a newer version of the app: one downloaded and
    /// ready to install, or what kept it from being had.
    pub async fn looked(&self, found: Result<String, &'static str>) {
        let report = match found {
            Ok(version) => UpdateReport::Ready(version),
            Err(code) => UpdateReport::Failed(code),
        };
        self.report_update(report).await;
    }

    /// Reports the latest frame sequence pushed by the shell (`P1.md` §3.3, §3.5).
    pub fn pushed(&self, seq: u64) {
        self.shell.set(seq);
    }

    /// Reports the latest frame sequence drawn by the interface (`P1.md` §3.3, §3.5, `P1-24`).
    /// Untrusted input beyond the latest emitted `Frame` sequence is ignored so `Screen` never
    /// exceeds `Frame`.
    pub fn drawn(&self, seq: u64) {
        if seq > self.frame.seq() {
            return;
        }
        if seq > self.screen.read().0 {
            self.screen.set(seq);
        }
    }

    /// Live statistics for this core: counters, peaks, spreads, and queues.
    pub fn stats(&self) -> Stats {
        self.stats.clone()
    }

    /// Snapshot of all tasks and blocking calls currently running on this core's tree.
    pub fn running(&self) -> Vec<Running> {
        self.root.running()
    }

    /// Whether an ephemeral hosted room is active or being torn down (lock-free, safe from shell).
    pub fn is_hosting(&self) -> bool {
        let state = self.state_reading.now().1;
        state.host.is_outbound()
            || matches!(state.host, HostState::Arming { .. } | HostState::Stopping)
            || *self.host_busy.now().1
    }

    /// Waits up to `max_ms` for any active outbound room to finish sending `gone` and clear
    /// `host.lock` (`P5.md` §3.4).
    pub async fn wait_host_idle(&self, max_ms: u64) {
        if !self.is_hosting() {
            return;
        }
        let mut state_rx = self.state_reading.clone();
        let mut busy_rx = self.host_busy.clone();
        let wait_ms = max_ms;
        let waited = tokio::time::timeout(Duration::from_millis(wait_ms), async move {
            loop {
                let hosting = {
                    let s = state_rx.now().1;
                    s.host.is_outbound()
                        || matches!(s.host, HostState::Arming { .. } | HostState::Stopping)
                        || *busy_rx.now().1
                };
                if !hosting {
                    break;
                }
                tokio::select! {
                    res = state_rx.next() => {
                        if res.is_none() {
                            break;
                        }
                    }
                    res = busy_rx.next() => {
                        if res.is_none() {
                            break;
                        }
                    }
                }
            }
        })
        .await;
        let _ = waited; // dropped: clean-exit wait budget elapsed; server TTL and host.lock cover the rest
    }
}
