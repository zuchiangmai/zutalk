//! Core entry point: `Config`, `Core`, `Outlet`, and `Voice` (`P1.md` §3.2–§3.3).
//!
//! Separated from `app/hub.rs` so `hub.rs` holds only the pure hub state machine with no IO,
//! HTTP client construction, or task spawning (`P1-20` `hub.sh`).

use crate::app::hub::{HUB_MAIL_MSGS, Hub, Msg};
use crate::app::proto::{Device, Level as SoundLevel, Notices, Session, State};
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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zutalk_log::{Level, info, warn};

/// Connection timeout for outbound HTTP requests: matches lane `PATIENCE_MS` (`P1-18`).
const CONNECT_TIMEOUT_MS: u64 = 10_000;
/// Idle read timeout for outbound HTTP requests: 30 s without a byte means a dead connection,
/// while long whole-recording uploads keep streaming without a whole-request timeout (`P1-18`).
const READ_TIMEOUT_MS: u64 = 30_000;

/// Lists the system's audio input devices: real OS query in production, fake in tests.
pub type Devices = Arc<dyn Fn() -> Vec<Device> + Send + Sync>;

/// Puts text on the system clipboard: the shell's to give. `false`: it could not.
pub type Clip = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Installs the newer version the shell has ready and relaunches the app: the shell's to give.
/// It returns only if it could not.
pub type Install = Arc<dyn Fn() + Send + Sync>;

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
            devices,
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
    pub(crate) fn tapes(&self) -> PathBuf {
        self.data.join("tape")
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
    shell: Arc<Gauge>,
    screen: Arc<Gauge>,
}

impl Core {
    /// Starts on the current runtime. The key store must be open (`keys::open_store`).
    pub fn start(config: Config) -> Result<(Core, Outlet), End> {
        let _ = rustls::crypto::ring::default_provider().install_default(); // dropped: installing the same TLS provider again is harmless
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(CONNECT_TIMEOUT_MS))
            .read_timeout(Duration::from_millis(READ_TIMEOUT_MS))
            .build()
            .map_err(|_| End::Fatal("tls"))?;
        let stats = Stats::new();
        let (commands, mail) = queue::bounded(&stats, "hub.mail", HUB_MAIL_MSGS, Full::Reject); // the interface: never waits, is told `busy`
        let results = commands.with(Full::Wait); // work that is done: must arrive; whoever did it can wait
        let progress = commands.with(Full::Drop); // how far a job has got: only the latest matters
        let root = Tree::root("core", stats.clone());
        let config = Arc::new(config);
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
        let commands_refused = stats.counter("commands.refused");
        let work = Work::new(
            config,
            http,
            results.clone(),
            progress,
            said.clone(),
            root.clone(),
            frame_reg.read(),
            water_reg.read(),
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
                shell,
                screen,
            },
            outlet,
        ))
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

    /// The shell's report from looking for a newer version of the app: one downloaded and
    /// ready to install, or what kept it from being had.
    pub async fn looked(&self, found: Result<String, &'static str>) {
        let _ = self.results.send(Msg::Looked(found)).await; // dropped: the hub has ended
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
}
