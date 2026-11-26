//! The hub: pure state machine over one bounded mailbox (`P1.md` §3.8, `P1-19`, `P1-20`).
//!
//! No disk IO, no network IO, no key-store calls, no device queries, and no tasks started here: every
//! external action is handed to [`Work`] and its result arrives back as a [`Msg`]. The only
//! `.await` in this file receives the next message from the mailbox.

pub(crate) use crate::app::core::Voice;
pub use crate::app::core::{Clip, Config, Core, Devices, Install, Outlet};
pub use crate::app::proto::Notices;
use crate::app::proto::{
    AuthFlow, Change, Command, Device, From, HostState, Level as SoundLevel, LocalShelf, Quota,
    Session, Sound, State, Status,
};
pub use crate::app::recording::{DRAFT_QUEUE_LINES, TAPE_QUEUE_BLOCKS};
pub use crate::app::work::DESK_QUEUE_JOBS;
use crate::app::work::{BootOut, OwnKeyOut, Refreshed, SignedOut, Survey, Work};
use crate::capture::audio::Source;
use crate::capture::clock::wall_ms;
use crate::id::identity::{self, AuthEvent, auth_step};
use crate::id::keys::Service;
use crate::knowledge::topic;
use crate::live::frame::{Frame, MAX_COLUMNS};
use crate::live::lane::{self, Mode};
use crate::out::city::{Calendar, Filing};
use crate::out::export::EXPORTERS;
use crate::out::host::{HostStepEvent, host_step};
use crate::record::migrate;
use crate::record::shelf::{About, TitleBy, TitleCandidate};
use crate::record::takes::RefineStatus;
use crate::rt::queue::Rx;
use crate::rt::signal::Register;
use crate::rt::stats::{Counter, Spread};
use crate::rt::supervise::{End, Tree};
use crate::rt::water::{Gauge, Water};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::watch;
use zutalk_log::{info, warn};

/// The hub's mail. 256: a burst of clicks is a few dozen; the hub answers each in under a millisecond.
pub const HUB_MAIL_MSGS: usize = 256;
/// One turn of the hub (PLAN.md §4.1): a command should take under a millisecond.
pub const HUB_TURN_BUDGET_US: u64 = 1_000;

/// Lifecycle phase of the hub (`P1.md` §3.13.11, `P1-20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    Starting,
    Ready,
    Installing,
}

impl Boot {
    /// Stable plain code for `hub.boot` log events.
    pub const fn code(self) -> &'static str {
        match self {
            Boot::Starting => "Starting",
            Boot::Ready => "Ready",
            Boot::Installing => "Installing",
        }
    }

    /// Whether `command` may be accepted in this boot phase (`P1-20`).
    pub fn accepts(self, command: &Command) -> Result<(), &'static str> {
        match self {
            Boot::Starting => Err("starting"),
            Boot::Ready => Ok(()),
            Boot::Installing => match command {
                Command::Start | Command::Import { .. } => Err("update.installing"),
                _ => Ok(()),
            },
        }
    }
}

/// Transition input for [`boot_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootEvent {
    Ready,
    Install,
    InstallFailed,
}

/// Pure transition table for [`Boot`] (`P1.md` §3.13.11).
pub fn boot_step(boot: Boot, event: BootEvent) -> Boot {
    match (boot, event) {
        (Boot::Starting, BootEvent::Ready) => Boot::Ready,
        (Boot::Ready, BootEvent::Install) => Boot::Installing,
        (Boot::Installing, BootEvent::InstallFailed) => Boot::Ready,
        (same, _) => same,
    }
}

/// Pure coalescing decision for library refreshes: returns `Some(dirty)` when no refresh is in
/// flight and `dirty` has advanced past `synced`, else `None`.
pub fn next_refresh(dirty: u64, in_flight: Option<u64>, synced: u64) -> Option<u64> {
    if in_flight.is_none() && dirty > synced { Some(dirty) } else { None }
}

pub(crate) enum Msg {
    Do(Command),
    Booted(Box<BootOut>),
    Devices(Vec<Device>),
    Ended(String),
    Wrote(String),
    RefineReady { id: String, from: RefineStatus },
    SignInPending { auth_gen: u64, did: String, handle: Option<String>, code: Option<String> },
    SignInFailed { auth_gen: u64, code: &'static str },
    Signed(Result<SignedOut, &'static str>),
    QuotaUpdated { left: u64, quota: Quota, notify: bool },
    QuotaFailed(&'static str),
    HandleResolved(Option<String>),
    LocalShelf(LocalShelf),
    LocalDiscarded(Result<LocalShelf, &'static str>),
    Copied,
    Diagnosed,
    RotationSaved,
    TopicRemembered,
    TopicRemoved(String),
    InstallFailed(&'static str),
    Refreshed(Result<Refreshed, &'static str>),
    DeskFailed(&'static str),
    HostReady { host_gen: u64, link: String },
    HostFailed { host_gen: u64, code: &'static str },
    HostViewers { host_gen: u64, viewers: u32 },
    HostStopped { host_gen: u64 },
    Looked(Result<String, &'static str>),
    City(Result<Calendar, End>),
    FilingReady(String, Filing),
    Filed(String, &'static str),
    Claimed(Result<(), &'static str>),
    Moved(String, Result<Vec<(String, String)>, &'static str>),
    OwnKeyDone(Result<OwnKeyOut, &'static str>),
}

impl Msg {
    fn kind(&self) -> &'static str {
        match self {
            Msg::Do(command) => command.kind(),
            Msg::Booted(_) => "Booted",
            Msg::Devices(_) => "Devices",
            Msg::Ended(_) => "Ended",
            Msg::Wrote(_) => "Wrote",
            Msg::RefineReady { .. } => "RefineReady",
            Msg::SignInPending { .. } => "SignInPending",
            Msg::SignInFailed { .. } => "SignInFailed",
            Msg::Signed(_) => "Signed",
            Msg::QuotaUpdated { .. } => "QuotaUpdated",
            Msg::QuotaFailed(_) => "QuotaFailed",
            Msg::HandleResolved(_) => "HandleResolved",
            Msg::LocalShelf(_) => "LocalShelf",
            Msg::LocalDiscarded(_) => "LocalDiscarded",
            Msg::Copied => "Copied",
            Msg::Diagnosed => "Diagnosed",
            Msg::RotationSaved => "RotationSaved",
            Msg::TopicRemembered => "TopicRemembered",
            Msg::TopicRemoved(_) => "TopicRemoved",
            Msg::InstallFailed(_) => "InstallFailed",
            Msg::Refreshed(_) => "Refreshed",
            Msg::DeskFailed(_) => "DeskFailed",
            Msg::HostReady { .. } => "HostReady",
            Msg::HostFailed { .. } => "HostFailed",
            Msg::HostViewers { .. } => "HostViewers",
            Msg::HostStopped { .. } => "HostStopped",
            Msg::Looked(_) => "Looked",
            Msg::City(_) => "City",
            Msg::FilingReady(_, _) => "FilingReady",
            Msg::Filed(_, _) => "Filed",
            Msg::Claimed(_) => "Claimed",
            Msg::Moved(_, _) => "Moved",
            Msg::OwnKeyDone(_) => "OwnKeyDone",
        }
    }
}

fn code(end: End) -> &'static str {
    if let End::Retry(code) | End::Fatal(code) = end { code } else { "done" }
}

fn now() -> i64 {
    wall_ms() as i64
}

pub(crate) struct Hub {
    work: Work,
    tree: Tree,
    state_reg: Register<State>,
    state_sent: Counter,
    commands_done: Counter,
    hub_turns: Counter,
    hub_turns_slow: Counter,
    hub_turn_us: Spread,
    refresh_merged: Counter,
    frame_reg: Register<Frame>,
    session_reg: Register<Option<Session>>,
    level_reg: Register<SoundLevel>,
    water_reg: Register<Water>,
    shell: Arc<Gauge>,
    screen: Arc<Gauge>,
    next_snap: Arc<AtomicU64>,
    said: Voice,
    boot: Boot,
    state: State,
    sent: Option<State>,
    recording: Option<(String, watch::Sender<Mode>, watch::Sender<u64>)>,
    refining: BTreeSet<String>,
    filing: BTreeSet<String>,
    moving: bool,             // the 0.8 library is being moved in
    shelf_by: Option<String>, // active shelf DID (did:zu when signed in, did:key when local)
    dirty: u64,
    refresh_in_flight: Option<u64>,
    refresh_synced: u64,
    need_retain: bool,
    auth_gen: u64,
    host_gen: u64,
}

impl Hub {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        work: Work,
        tree: Tree,
        state_reg: Register<State>,
        frame_reg: Register<Frame>,
        session_reg: Register<Option<Session>>,
        level_reg: Register<SoundLevel>,
        water_reg: Register<Water>,
        shell: Arc<Gauge>,
        screen: Arc<Gauge>,
        said: Voice,
    ) -> Self {
        let stats = tree.stats();
        let state_sent = stats.counter("state.sent");
        let commands_done = stats.counter("commands.done");
        let hub_turns = stats.counter("hub.turns");
        let hub_turns_slow = stats.counter("hub.turns.slow");
        let hub_turn_us = stats.spread("hub.turn_us");
        let refresh_merged = stats.counter("refresh.merged");
        let state = State {
            languages: crate::live::lang::ALL.iter().map(|profile| profile.lang.into()).collect(),
            exporters: EXPORTERS.iter().map(|exporter| exporter.id.into()).collect(),
            ..State::default()
        };
        Self {
            work,
            tree,
            state_reg,
            state_sent,
            commands_done,
            hub_turns,
            hub_turns_slow,
            hub_turn_us,
            refresh_merged,
            frame_reg,
            session_reg,
            level_reg,
            water_reg,
            shell,
            screen,
            next_snap: Arc::new(AtomicU64::new(1)),
            said,
            boot: Boot::Starting,
            state,
            sent: None,
            recording: None,
            refining: BTreeSet::new(),
            filing: BTreeSet::new(),
            moving: false,
            shelf_by: None,
            dirty: 0,
            refresh_in_flight: None,
            refresh_synced: 0,
            need_retain: false,
            auth_gen: 0,
            host_gen: 0,
        }
    }

    fn next_auth_gen(&self) -> u64 {
        let next = self.auth_gen.wrapping_add(1);
        if next == 0 { 1 } else { next }
    }

    fn bump_auth_gen(&mut self) -> u64 {
        self.auth_gen = self.next_auth_gen();
        self.auth_gen
    }

    fn bump_host_gen(&mut self) -> u64 {
        let next = self.host_gen.wrapping_add(1);
        self.host_gen = if next == 0 { 1 } else { next };
        self.host_gen
    }

    pub(crate) async fn run(mut self, mut mail: Rx<Msg>) {
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

    fn set_boot(&mut self, event: BootEvent, why: &'static str) {
        let from = self.boot;
        let to = boot_step(from, event);
        if to != from {
            info!("hub.boot", from = from.code(), to = to.code(), why = why);
            self.boot = to;
        }
    }

    fn set_auth(&mut self, event: AuthEvent, why: &'static str) {
        let is_fail = matches!(event, AuthEvent::Failed);
        let from = self.state.auth.code();
        let next = auth_step(&self.state.auth, event);
        let to = next.code();
        if next != self.state.auth {
            if is_fail {
                warn!("identity.auth", from = from, to = to, why = why);
            } else {
                info!("identity.auth", from = from, to = to, why = why);
            }
            self.state.auth = next;
        }
    }

    fn set_host(&mut self, event: HostStepEvent, why: &'static str) {
        let is_fail = matches!(event, HostStepEvent::RoomFailed);
        let from_code = self.state.host.code();
        let from_outbound = self.state.host.is_outbound();
        let next = host_step(&self.state.host, event);
        let to_code = next.code();
        let to_outbound = next.is_outbound();
        let viewers = next.viewers();
        if from_code != to_code || from_outbound != to_outbound {
            if is_fail {
                warn!(
                    "host.state",
                    from = from_code,
                    to = to_code,
                    why = why,
                    outbound = to_outbound,
                    viewers = viewers,
                );
            } else {
                info!(
                    "host.state",
                    from = from_code,
                    to = to_code,
                    why = why,
                    outbound = to_outbound,
                    viewers = viewers,
                );
            }
        }
        self.state.host = next;
    }

    fn publish(&mut self) {
        if self.boot == Boot::Starting {
            return;
        }
        if self.sent.as_ref() != Some(&self.state) {
            self.sent = Some(self.state.clone());
            self.state_reg.set(self.state.clone());
            self.state_sent.add(1);
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty += 1;
        if self.refresh_in_flight.is_some() {
            self.refresh_merged.add(1);
        } else {
            self.maybe_refresh();
        }
    }

    fn maybe_refresh(&mut self) {
        if let Some(rev) = next_refresh(self.dirty, self.refresh_in_flight, self.refresh_synced) {
            let retain = self.need_retain.then_some(self.state.setup.keep);
            if self.work.refresh(rev, retain).is_ok() {
                self.refresh_in_flight = Some(rev);
                self.need_retain = false;
            }
        }
    }

    fn apply_refreshed(&mut self, refreshed: Refreshed) {
        self.state.keys = refreshed.keys;
        self.state.left = refreshed.left;
        if self.state.me.is_some() {
            if let Some(quota) = refreshed.quota {
                self.state.quota = Some(quota);
            }
        } else {
            self.state.quota = None;
        }
    }

    fn apply_survey(&mut self, survey: Survey) {
        let mut owed = Vec::new();
        for id in survey.writes {
            if !self.busy(&id) {
                self.refining.insert(id.clone());
                owed.push(id);
            }
        }
        if survey.sweep {
            self.work.write_recovered(owed);
        }
        for (id, filing) in survey.filings {
            if let Err(c) = self.start_filing(id, filing) {
                self.said.say(c);
            }
        }
    }

    fn apply_signed(&mut self, out: SignedOut) {
        self.state.unmoved.clear();
        for c in out.say {
            self.said.say(c);
        }
        self.set_auth(AuthEvent::Completed, "signed");
        self.state.me = out.me;
        self.shelf_by = out.shelf_by;
        self.state.handle = out.handle;
        if self.state.me.is_none() {
            self.state.quota = None;
        }
        self.state.rotation_unsaved = out.rotation_unsaved;
        self.state.local_shelf = out.local_shelf;
        self.apply_refreshed(out.refreshed);
        self.apply_survey(out.survey);
        self.migrate();
    }

    fn take(&mut self, msg: Msg) -> Result<(), &'static str> {
        match msg {
            Msg::Do(command) => self.command(command)?,
            Msg::Booted(out) => {
                let BootOut { mut setup, signed } = *out;
                setup.topics = topic::normalize_selected_topics(setup.topics);
                self.state.setup = setup;
                self.apply_signed(signed);
                self.set_boot(BootEvent::Ready, "boot");
            }
            Msg::Devices(devices) => self.state.devices = devices,
            Msg::Ended(id) if self.under_way(&id) => {
                self.stop();
            }
            Msg::Ended(_) => {} // from a recording that has stopped
            Msg::Wrote(id) => {
                self.refining.remove(&id);
                self.need_retain = true;
                self.mark_dirty();
            }
            Msg::RefineReady { id, from } => {
                if self.under_way(&id) {
                    return Err("recording.under_way");
                }
                if self.busy(&id) {
                    return Err("recording.writing");
                }
                self.refining.insert(id.clone());
                self.work.write_manual(id, from);
            }
            Msg::SignInPending { auth_gen, did, handle, code } => {
                if auth_gen != self.auth_gen {
                    let _ = (did, handle, code); // dropped: stale auth attempt superseded by newer sign-in or cancel
                } else {
                    self.set_auth(AuthEvent::Received { did, handle, code }, "received");
                }
            }
            Msg::SignInFailed { auth_gen, code } => {
                if auth_gen != self.auth_gen {
                    let _ = code; // dropped: stale auth attempt superseded by newer sign-in or cancel
                } else {
                    self.set_auth(AuthEvent::Failed, code);
                    return Err(code);
                }
            }
            Msg::Signed(out) => self.apply_signed(out?),
            Msg::QuotaUpdated { left, quota, notify } => {
                if self.state.me.is_some() {
                    self.state.left = Some(left);
                    self.state.quota = Some(quota);
                    if notify {
                        self.said.say("quota.refreshed");
                    }
                }
            }
            Msg::QuotaFailed(c) => return Err(c),
            Msg::HandleResolved(handle) => {
                if self.state.me.is_some() {
                    self.state.handle = handle;
                }
            }
            Msg::LocalShelf(local_shelf) => self.state.local_shelf = local_shelf,
            Msg::LocalDiscarded(res) => match res {
                Ok(local_shelf) => {
                    self.state.local_shelf = local_shelf;
                    self.said.say("local.discarded");
                }
                Err(c) => return Err(c),
            },
            Msg::Copied => self.said.say("copied"),
            Msg::Diagnosed => self.said.say("diagnose.done"),
            Msg::RotationSaved => self.state.rotation_unsaved = false,
            Msg::TopicRemembered => self.said.say("topic.remembered"),
            Msg::TopicRemoved(id) => {
                if self.state.setup.topics.iter().any(|t| t == &id) {
                    self.state.setup.topics.retain(|t| t != &id);
                    let _ = self.work.save_setup(self.state.setup.clone()); // dropped: desk queue busy; next setup save persists cleaned topics
                }
            }
            Msg::InstallFailed(c) => {
                self.set_boot(BootEvent::InstallFailed, "update.install");
                return Err(c);
            }
            Msg::Refreshed(res) => {
                self.refresh_in_flight = None;
                match res {
                    Ok(refreshed) => {
                        self.refresh_synced = self.refresh_synced.max(refreshed.rev);
                        self.apply_refreshed(refreshed);
                        self.maybe_refresh();
                    }
                    Err(c) => {
                        self.maybe_refresh();
                        return Err(c);
                    }
                }
            }
            Msg::DeskFailed(c) => {
                self.maybe_refresh();
                return Err(c);
            }
            Msg::HostReady { host_gen, link } => {
                if host_gen != self.host_gen {
                    let _ = link; // dropped: stale host room superseded or cancelled
                } else {
                    self.set_host(HostStepEvent::RoomMade { link }, "ready");
                }
            }
            Msg::HostFailed { host_gen, code } => {
                if host_gen != self.host_gen {
                    let _ = code; // dropped: stale host room superseded or cancelled
                } else {
                    self.set_host(HostStepEvent::RoomFailed, code);
                    return Err(code);
                }
            }
            Msg::HostViewers { host_gen, viewers } => {
                if host_gen != self.host_gen {
                    let _ = viewers; // dropped: stale host room superseded or cancelled
                } else {
                    self.set_host(HostStepEvent::Viewers(viewers), "viewers");
                    if self.recording.is_some() && self.state.host.is_outbound() {
                        self.session_reg.change(|cur| {
                            let s = cur.as_ref()?;
                            (s.viewers != viewers).then(|| Some(Session { viewers, ..s.clone() }))
                        });
                    }
                }
            }
            Msg::HostStopped { host_gen } => {
                if host_gen != self.host_gen {
                    // dropped: stale room teardown superseded by a newer host session
                } else {
                    self.set_host(HostStepEvent::Stopped, "stopped");
                }
            }
            Msg::Looked(found) => self.state.update = Some(found?),
            Msg::City(Ok(_)) => {}
            Msg::City(Err(end)) => return Err(code(end)),
            Msg::FilingReady(id, filing) => self.start_filing(id, filing)?,
            Msg::Filed(id, said) => {
                self.filing.remove(&id);
                self.said.say(said);
            }
            Msg::Claimed(done) => {
                done?;
                self.mark_dirty();
            }
            Msg::Moved(by, skipped) => {
                self.moving = false;
                if self.shelf_by.as_deref() != Some(by.as_str()) {
                    let _ = skipped; // dropped: stale migration result from previous identity
                    self.migrate();
                } else {
                    self.state.unmoved = migrate::cap_unmoved(skipped?);
                }
            }
            Msg::OwnKeyDone(res) => match res {
                Ok(out) => {
                    self.state.keys = out.keys;
                    self.state.left = out.left;
                    if let Some(survey) = out.survey {
                        self.apply_survey(survey);
                    }
                }
                Err(c) => {
                    self.mark_dirty();
                    return Err(c);
                }
            },
        }
        Ok(())
    }

    fn command(&mut self, command: Command) -> Result<(), &'static str> {
        self.boot.accepts(&command)?;
        if command.needs_person() && self.state.me.is_none() {
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
            Command::Reconnect => self.reconnect()?,
            Command::Setup { mut setup } => {
                if setup.shown.len() > MAX_COLUMNS {
                    return Err("captions.columns");
                }
                setup.caption = setup.caption.normalized();
                setup.topics = topic::normalize_selected_topics(setup.topics);
                // Apply immediately in memory so the next Start uses it; desk persists it.
                self.state.setup = setup.clone();
                self.work.save_setup(setup)?;
            }
            // A line is corrected on the record, not on a draft about to be replaced.
            Command::Change { id, change: Change::Line { .. } } if self.busy(&id) => {
                return Err("recording.writing");
            }
            Command::Change { id, change } => self.work.change(id, change)?,
            Command::Note { id, op } => self.work.note(id, op)?,
            Command::Refine { id } => {
                if self.under_way(&id) {
                    return Err("recording.under_way");
                }
                if self.busy(&id) {
                    return Err("recording.writing");
                }
                self.work.refine(id)?;
            }
            Command::Remove { id } => {
                if self.under_way(&id) {
                    self.stop();
                }
                self.work.remove(id)?;
            }
            Command::Restore { id } => self.work.restore(id)?,
            Command::Export { id, exporter, languages, to } => {
                let exporter = EXPORTERS.iter().find(|e| e.id == exporter).ok_or("export.which")?;
                self.work.export(id, exporter.id, languages, to)?;
            }
            Command::HostArm { event, captions } => {
                if self.recording.is_some() {
                    return Err("recording.under_way");
                }
                if matches!(self.state.host, HostState::Stopping) {
                    return Err("host.busy");
                }
                let had_room = matches!(
                    self.state.host,
                    HostState::Arming { .. } | HostState::Armed { captions: true, .. }
                );
                if !had_room && captions {
                    let host_gen = self.bump_host_gen();
                    self.work.host_make(host_gen)?;
                    self.set_host(HostStepEvent::Arm { event, captions: true }, "arm");
                } else if had_room && !captions {
                    let stop_gen = self.bump_host_gen();
                    let _ = self.work.host_stop_room(stop_gen); // dropped: room teardown runs asynchronously while staying armed without captions
                    self.set_host(HostStepEvent::Arm { event, captions: false }, "captions.off");
                } else {
                    self.set_host(HostStepEvent::Arm { event, captions }, "arm");
                }
            }
            Command::HostCancel => {
                let stop_gen = self.bump_host_gen();
                let wait_gone = self.work.host_stop_room(stop_gen);
                self.set_host(HostStepEvent::Cancel { wait_gone }, "host.cancelled");
                if self.recording.is_some() {
                    self.session_reg.change(|cur| {
                        let s = cur.as_ref()?;
                        (s.outbound || s.viewers != 0)
                            .then(|| Some(Session { outbound: false, viewers: 0, ..s.clone() }))
                    });
                }
            }
            Command::StopOutbound => {
                if self.state.host.is_outbound()
                    || matches!(self.state.host, HostState::Arming { .. })
                {
                    let stop_gen = self.bump_host_gen();
                    let _ = self.work.host_stop_room(stop_gen); // dropped: room teardown runs asynchronously while recording continues
                    self.set_host(HostStepEvent::StopOutbound, "host.stopped");
                    if self.recording.is_some() {
                        self.session_reg.change(|cur| {
                            let s = cur.as_ref()?;
                            (s.outbound || s.viewers != 0)
                                .then(|| Some(Session { outbound: false, viewers: 0, ..s.clone() }))
                        });
                    }
                    self.said.say("host.stopped");
                }
            }
            Command::SignIn => {
                if matches!(self.state.auth, AuthFlow::Creating | AuthFlow::Recovering) {
                    return Err("busy");
                }
                let next_gen = self.next_auth_gen();
                let (url, expires_at) = self.work.sign_in(next_gen)?;
                self.auth_gen = next_gen;
                self.set_auth(AuthEvent::BeginScan { url, expires_at }, "sign_in");
            }
            Command::ConfirmSignIn => {
                if !matches!(self.state.auth, AuthFlow::Confirm { .. }) {
                    return Err("id.no_pending");
                }
                self.work.confirm_sign_in(self.auth_gen, self.state.setup.keep, self.busy_ids())?;
            }
            Command::CancelSignIn => {
                self.bump_auth_gen();
                self.work.cancel_sign_in();
                self.set_auth(AuthEvent::Cancelled, "id.cancelled");
            }
            Command::CreateIdentity { handle } => {
                if self.state.me.is_some() {
                    return Err("id.signed_in");
                }
                if matches!(self.state.auth, AuthFlow::Creating | AuthFlow::Recovering) {
                    return Err("busy");
                }
                let normalized = match handle {
                    Some(raw) => identity::normalize_handle(&raw)?,
                    None => None,
                };
                let auth_gen = self.bump_auth_gen();
                self.work.create_identity(normalized, auth_gen);
                self.set_auth(AuthEvent::BeginCreate, "create");
            }
            Command::RecoverIdentity { did, key } => {
                if self.state.me.is_some() {
                    return Err("id.signed_in");
                }
                if matches!(self.state.auth, AuthFlow::Creating | AuthFlow::Recovering) {
                    return Err("busy");
                }
                let auth_gen = self.bump_auth_gen();
                self.work.recover_identity(did, key, auth_gen);
                self.set_auth(AuthEvent::BeginRecover, "recover");
            }
            Command::RefreshQuota => self.work.refresh_quota()?,
            Command::DiscardLocal => {
                if self.recording.is_some() {
                    return Err("discard.busy");
                }
                self.work.discard_local()?;
            }
            Command::SignOut => {
                if self.state.host.is_active() {
                    let stop_gen = self.bump_host_gen();
                    let wait_gone = self.work.host_stop_room(stop_gen);
                    self.set_host(HostStepEvent::Cancel { wait_gone }, "sign_out");
                    if self.recording.is_some() {
                        self.session_reg.change(|cur| {
                            let s = cur.as_ref()?;
                            (s.outbound || s.viewers != 0)
                                .then(|| Some(Session { outbound: false, viewers: 0, ..s.clone() }))
                        });
                    }
                }
                self.bump_auth_gen();
                self.set_auth(AuthEvent::Cancelled, "sign_out");
                self.state.handle = None;
                self.state.quota = None;
                self.state.unmoved.clear();
                self.work.sign_out(self.state.setup.keep, self.busy_ids())?;
            }
            Command::CopyRotation => self.work.copy_rotation()?,
            Command::RotationSaved => self.work.rotation_saved()?,
            Command::OwnKey { service, key } => {
                let service = Service::ALL.into_iter().find(|s| s.name() == service);
                let service = service.ok_or("keys.which")?;
                let next_from = From::of(key.is_some(), self.state.me.is_some());
                if let Some((_, from)) =
                    self.state.keys.iter_mut().find(|(name, _)| name == service.name())
                {
                    *from = next_from;
                } else {
                    self.state.keys.push((service.name().into(), next_from));
                }
                self.work.own_key(service, key, self.busy_ids())?;
            }
            Command::Captions { show } => self.state.captions = show,
            Command::CaptionGeometry { scene, rect } => {
                let clamped = rect.clamp();
                let style = self.state.setup.caption.style_mut(scene);
                if style.geometry != Some(clamped) {
                    style.geometry = Some(clamped);
                    self.work.save_setup(self.state.setup.clone())?;
                }
            }
            Command::DestroyAudio { id } => {
                if self.under_way(&id) {
                    return Err("recording.under_way");
                }
                let under_way = self.recording.as_ref().map(|(rec, _, _)| rec.clone());
                self.work.destroy_audio(id, under_way)?;
            }
            Command::Term { said, written } => self.work.term(said, written)?,
            Command::SaveTopic { topic } => self.work.save_topic(topic)?,
            Command::RemoveTopic { id } => self.work.remove_topic(id)?,
            Command::ImportTopicFile { id, path } => self.work.import_topic_file(id, path)?,
            Command::RememberTerm { topic_id, said, written } => {
                self.work.remember_term(topic_id, said, written)?
            }
            Command::GenerateTitle { id } => {
                if self.under_way(&id) {
                    return Err("recording.under_way");
                }
                self.work.generate_title(id)?;
            }
            Command::PickTitle { id, title, by } => self.work.pick_title(id, title, by)?,
            // Installing relaunches the app, so never while recording.
            Command::Update if self.recording.is_some() => return Err("update.recording"),
            Command::Update if self.boot == Boot::Installing => return Err("update.installing"),
            Command::Update => {
                self.set_boot(BootEvent::Install, "update");
                self.work.install();
            }
            Command::City { address } => self.work.city(address),
            // What is still being written is filed once it is written.
            Command::File { id, .. } if self.busy(&id) => return Err("recording.writing"),
            Command::File { id, event, audio } => {
                self.work.begin_file(id, event, audio)?;
            }
            Command::Diagnose => self.work.diagnose(
                self.state.setup.spoken.clone(),
                self.state.setup.shown.clone(),
                self.state.setup.sound,
                self.state.captions,
            )?,
        }
        Ok(())
    }

    fn start_filing(&mut self, id: String, filing: Filing) -> Result<(), &'static str> {
        if self.filing.contains(&id) {
            return Ok(()); // it is on its way
        }
        self.work.file(id.clone(), filing)?;
        self.filing.insert(id);
        Ok(())
    }

    fn under_way(&self, id: &str) -> bool {
        self.recording.as_ref().is_some_and(|(under_way, _, _)| under_way == id)
    }

    fn busy(&self, id: &str) -> bool {
        self.under_way(id) || self.refining.contains(id)
    }

    fn busy_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.refining.iter().cloned().collect();
        if let Some((id, _, _)) = &self.recording {
            ids.push(id.clone());
        }
        ids
    }

    fn has_key(&self, service: Service) -> bool {
        self.state
            .keys
            .iter()
            .find(|(name, _)| name == service.name())
            .is_some_and(|(_, from)| *from != From::Nowhere)
    }

    /// A recording from the sound the person chose, or from a file: a file has no live
    /// captions, and its name is the recording's title.
    fn start(&mut self, source: Source) -> Result<(), &'static str> {
        if self.recording.is_some() {
            return Err("recording.under_way");
        }
        let (started, setup) = (now(), self.state.setup.clone());
        let id = format!("{started:011x}{:08x}", getrandom::u32().map_err(|_| "random")?);
        let file = matches!(source, Source::File(_));
        let source_name = source.code().into();
        let title = match &source {
            Source::File(path) => {
                path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
            }
            _ => String::new(),
        };
        let title_by = if file && !title.trim().is_empty() { TitleBy::File } else { TitleBy::None };
        let title_candidates = if !file
            && let Some(ev) = self.state.host.event()
            && !ev.title.trim().is_empty()
        {
            vec![TitleCandidate { by: TitleBy::Event, text: ev.title.clone() }]
        } else {
            Vec::new()
        };
        let (spoken, shown, topics) = (setup.spoken, setup.shown, setup.topics);
        let captions = lane::captions(file, &shown, self.has_key(Service::Soniox));
        let initial = match &captions {
            Ok(_) => Status::Connecting,
            Err(why) => Status::AudioOnly { why: (*why).into() },
        };
        let about = About {
            started,
            title,
            title_by,
            title_candidates,
            spoken,
            shown,
            source: source_name,
            topics,
            ..About::default()
        };
        let (mode, watching) = watch::channel(Mode::Recording);
        let (reconnect, reconnect_rx) = watch::channel(0u64);
        self.work.start_recording(
            id.clone(),
            about,
            source,
            captions,
            watching,
            reconnect_rx,
            self.frame_reg.clone(),
            self.session_reg.clone(),
            self.level_reg.clone(),
            self.water_reg.clone(),
            self.shell.clone(),
            self.screen.clone(),
            self.next_snap.clone(),
        )?;
        let (outbound, viewers, event_title) = if !file && self.state.host.is_active() {
            if matches!(self.state.host, HostState::Arming { .. }) {
                let stop_gen = self.bump_host_gen();
                let _ = self.work.host_stop_room(stop_gen); // dropped: cancel in-flight room make before starting local-only recording
            }
            self.set_host(HostStepEvent::RecordingStarted, "start");
            let outbound = self.state.host.is_outbound();
            if outbound {
                self.work.host_live(true);
            }
            (outbound, self.state.host.viewers(), self.state.host.event().map(|e| e.title.clone()))
        } else {
            (false, 0, None)
        };
        self.session_reg.set(Some(Session {
            id: id.clone(),
            started,
            status: initial,
            outbound,
            viewers,
            event_title,
        }));
        self.recording = Some((id, mode, reconnect));
        Ok(())
    }

    fn mode(&self, mode: Mode) -> Result<(), &'static str> {
        self.recording.as_ref().ok_or("recording.none")?.1.send_replace(mode);
        Ok(())
    }

    fn reconnect(&self) -> Result<(), &'static str> {
        self.recording.as_ref().ok_or("recording.none")?.2.send_modify(|n| *n += 1);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some((id, mode, _)) = self.recording.take() {
            mode.send_replace(Mode::Stopped);
            self.session_reg.set(None);
            self.frame_reg.set(Frame::default());
            self.level_reg.set(SoundLevel::default());
            self.water_reg.change(|current| (*current != Water::default()).then(Water::default));
            self.refining.insert(id);
            if self.state.host.is_active() {
                let stop_gen = self.bump_host_gen();
                let wait_gone = self.work.host_stop_room(stop_gen);
                self.set_host(HostStepEvent::RecordingStopped { wait_gone }, "stop");
            }
        }
    }

    /// The library the 0.8 app left here moves in on first boot without requiring `did:zu`
    /// sign-in: onto the active local `did:key` shelf (later adopted cleanly on sign-in) or
    /// directly onto the signed-in `did:zu` shelf. `moved.json` records the single first recipient.
    fn migrate(&mut self) {
        let Some(by) = self.shelf_by.clone() else { return };
        if self.moving {
            return;
        }
        if self.work.migrate(by) {
            self.moving = true;
        }
    }
}
