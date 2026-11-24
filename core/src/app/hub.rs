//! The hub. Commands, and reports from the work under way, come into one mailbox; after each,
//! the state is sent whole if it changed. All data IO runs off the hub on `Work` (`P1-19`);
//! identity, settings, devices, updates, and boot move off in `P1-20`.

pub(crate) use crate::app::core::Voice;
pub use crate::app::core::{Clip, Config, Core, Install, Outlet};
pub use crate::app::proto::Notices;
use crate::app::proto::{
    Change, Command, From, Level as SoundLevel, Room, Row, Session, Sound, State, Status, Writing,
};
pub use crate::app::recording::{DRAFT_QUEUE_LINES, TAPE_QUEUE_BLOCKS};
pub use crate::app::work::DESK_QUEUE_JOBS;
use crate::app::work::{OwnKeyOut, Refreshed, SignedOut, Survey, Work};
use crate::capture::audio::{self, Source};
use crate::capture::clock::wall_ms;
use crate::id::keys::{self, Me, Secret, Service, SignIn, Who};
use crate::live::frame::{Frame, MAX_COLUMNS};
use crate::live::lane::{self, Mode};
use crate::out::city::{self, Calendar, Filing};
use crate::out::export::EXPORTERS;
use crate::record::shelf::About;
use crate::rt::queue::{Rx, Tx};
use crate::rt::signal::Register;
use crate::rt::stats::{Counter, Spread};
use crate::rt::supervise::{End, Tree, again};
use crate::rt::water::{Gauge, Water};
use std::sync::atomic::AtomicU64;
use std::{fs, sync::Arc};
use tokio::sync::watch;
use tokio::task::AbortHandle;
use zutalk_log::{info, warn};

/// The hub's mail. 256: a burst of clicks is a few dozen; the hub answers each in under a millisecond.
pub const HUB_MAIL_MSGS: usize = 256;
/// One turn of the hub (PLAN.md §4.1): a command should take under a millisecond.
pub const HUB_TURN_BUDGET_US: u64 = 1_000;

/// Pure coalescing decision for library refreshes: returns `Some(dirty)` when no refresh is in
/// flight and `dirty` has advanced past `synced`, else `None`.
pub fn next_refresh(dirty: u64, in_flight: Option<u64>, synced: u64) -> Option<u64> {
    if in_flight.is_none() && dirty > synced { Some(dirty) } else { None }
}

pub(crate) enum Msg {
    Do(Command),
    RecordingOpened,
    Sealed(Row),
    Ended(String),
    Writing(Writing),
    Wrote(String),
    SignedIn(Result<Me, End>),
    Signed(Result<SignedOut, &'static str>),
    Refreshed(Result<Refreshed, &'static str>),
    DeskChanged,
    DeskFailed(&'static str),
    Searched {
        seq: u64,
        search_seq: u64,
        ms: u64,
        result: Result<Vec<(String, String)>, &'static str>,
    },
    Rooms(Vec<Room>),
    Shared(Result<(), &'static str>),
    Looked(Result<String, &'static str>),
    City(Result<Calendar, End>),
    FilingReady(String, Filing),
    Filing(Writing),
    Filed(String, &'static str),
    Claimed(Result<(), &'static str>),
    Moving(Writing),
    Moved(Result<Vec<(String, String)>, &'static str>),
    OwnKeyDone(Result<OwnKeyOut, &'static str>),
}

impl Msg {
    fn kind(&self) -> &'static str {
        match self {
            Msg::Do(command) => command.kind(),
            Msg::RecordingOpened => "RecordingOpened",
            Msg::Sealed(_) => "Sealed",
            Msg::Ended(_) => "Ended",
            Msg::Writing(_) => "Writing",
            Msg::Wrote(_) => "Wrote",
            Msg::SignedIn(_) => "SignedIn",
            Msg::Signed(_) => "Signed",
            Msg::Refreshed(_) => "Refreshed",
            Msg::DeskChanged => "DeskChanged",
            Msg::DeskFailed(_) => "DeskFailed",
            Msg::Searched { .. } => "Searched",
            Msg::Rooms(_) => "Rooms",
            Msg::Shared(_) => "Shared",
            Msg::Looked(_) => "Looked",
            Msg::City(_) => "City",
            Msg::FilingReady(_, _) => "FilingReady",
            Msg::Filing(_) => "Filing",
            Msg::Filed(_, _) => "Filed",
            Msg::Claimed(_) => "Claimed",
            Msg::Moving(_) => "Moving",
            Msg::Moved(_) => "Moved",
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

fn writing(id: &str, step: &str) -> Writing {
    Writing { id: id.into(), step: step.into(), done: 0, of: 0 }
}

pub(crate) struct Hub {
    config: Arc<Config>,
    http: reqwest::Client,
    results: Tx<Msg>,
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
    state: State,
    sent: Option<State>,
    recording: Option<(String, watch::Sender<Mode>, watch::Sender<u64>)>,
    opened: Option<String>,
    signing: Option<AbortHandle>, // a sign-in, an identity being made, or an invite carried
    invite: Option<Secret>,       // a 0.8 community invite not carried over yet
    moving: bool,                 // the 0.8 library is being moved in
    dirty: u64,
    refresh_in_flight: Option<u64>,
    refresh_synced: u64,
    need_retain: bool,
    need_rooms: bool,
    search_seq: u64,
}

impl Hub {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: Arc<Config>,
        http: reqwest::Client,
        results: Tx<Msg>,
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
        Self {
            config,
            http,
            results,
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
            state: State::default(),
            sent: None,
            recording: None,
            opened: None,
            signing: None,
            invite: None,
            moving: false,
            dirty: 0,
            refresh_in_flight: None,
            refresh_synced: 0,
            need_retain: false,
            need_rooms: false,
            search_seq: 0,
        }
    }

    pub(crate) async fn run(mut self, mut mail: Rx<Msg>) {
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
        if let Ok(rx) = self.work.sign_boot(Who::of(kept, local), made, self.state.setup.keep)
            && let Ok(Ok(out)) = rx.await
        {
            self.apply_signed(out);
        }
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
            let include_rooms = self.need_rooms;
            if self.work.refresh(rev, self.opened.clone(), retain, include_rooms).is_ok() {
                self.refresh_in_flight = Some(rev);
                self.need_retain = false;
                self.need_rooms = false;
            }
        }
    }

    fn apply_refreshed(&mut self, refreshed: Refreshed) {
        self.state.keys = refreshed.keys;
        self.state.left = refreshed.left;
        self.state.library = refreshed.library;
        self.state.terms = refreshed.terms;
        if refreshed.clear_hits {
            self.state.hits.clear();
        }
        if refreshed.opened == self.opened {
            self.state.open = refreshed.open;
        }
        if let Some(rooms) = refreshed.rooms {
            self.state.rooms = rooms;
        }
    }

    fn apply_survey(&mut self, survey: Survey) {
        let mut owed = Vec::new();
        for id in survey.writes {
            if !self.busy(&id) {
                self.state.writing.push(writing(&id, "closing"));
                owed.push(id);
            }
        }
        if survey.sweep {
            self.work.write_recovered(owed, self.state.terms.clone());
        }
        for (id, filing) in survey.filings {
            if let Err(c) = self.start_filing(id, filing) {
                self.said.say(c);
            }
        }
    }

    fn apply_signed(&mut self, out: SignedOut) {
        for c in out.say {
            self.said.say(c);
        }
        self.state.me = out.me;
        self.opened = None;
        self.state.open = None;
        self.state.rotation_unsaved = out.rotation_unsaved;
        self.apply_refreshed(out.refreshed);
        self.state.rooms = out.rooms;
        let live = self.state.rooms.iter().find(|r| r.recording.is_none()).map(|r| r.id.clone());
        self.work.sync_slot(live);
        // An invite not carried over yet goes to whoever is signed in now.
        if let (Some(_), Some(token)) = (&self.state.me, self.invite.clone()) {
            let (chain, results) = (self.work.keys_snapshot(), self.results.clone());
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
        self.apply_survey(out.survey);
        self.migrate();
    }

    fn take(&mut self, msg: Msg) -> Result<(), &'static str> {
        match msg {
            Msg::Do(command) => self.command(command)?,
            Msg::RecordingOpened => self.mark_dirty(),
            Msg::Sealed(sealed_row) => {
                if let Some(existing) =
                    self.state.library.iter_mut().find(|r| r.id == sealed_row.id)
                {
                    *existing = sealed_row;
                } else {
                    self.state.library.insert(0, sealed_row);
                }
                self.mark_dirty();
            }
            Msg::Ended(id) if self.under_way(&id) => {
                self.stop();
                self.mark_dirty();
            }
            Msg::Ended(_) => {} // from a recording that has stopped
            Msg::Writing(writing) => {
                self.state.writing.retain(|w| w.id != writing.id);
                self.state.writing.push(writing);
            }
            Msg::Wrote(id) => {
                self.state.writing.retain(|w| w.id != id);
                self.need_retain = true;
                self.mark_dirty();
            }
            Msg::SignedIn(me) => {
                self.state.login = None;
                let me = me.map_err(code)?;
                self.work.sign(
                    Who::Person(me),
                    "sign_in",
                    false,
                    self.state.setup.keep,
                    self.busy_ids(),
                )?;
            }
            Msg::Signed(out) => {
                self.apply_signed(out?);
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
            Msg::DeskChanged => self.mark_dirty(),
            Msg::DeskFailed(c) => {
                self.maybe_refresh();
                return Err(c);
            }
            Msg::Searched { seq, search_seq, ms, result } => {
                if search_seq != self.search_seq {
                    self.work.note_search_stale();
                    warn!("job.done", kind = "search", job = seq, code = "stale", ms = ms);
                } else {
                    match result {
                        Ok(hits) => {
                            self.state.hits = hits;
                            info!("job.done", kind = "search", job = seq, code = "ok", ms = ms);
                            self.work.note_job_done();
                        }
                        Err(c) => {
                            warn!("job.done", kind = "search", job = seq, code = c, ms = ms);
                            self.work.note_job_failed();
                            return Err(c);
                        }
                    }
                }
            }
            Msg::Rooms(rooms) => self.state.rooms = rooms,
            Msg::Shared(done) => {
                self.need_rooms = true;
                self.mark_dirty();
                done?;
            }
            Msg::Looked(found) => self.state.update = Some(found?),
            Msg::City(calendar) => self.state.city = Some(calendar.map_err(code)?),
            Msg::FilingReady(id, filing) => self.start_filing(id, filing)?,
            Msg::Filing(writing) => {
                self.state.filing.retain(|w| w.id != writing.id);
                self.state.filing.push(writing);
            }
            Msg::Filed(id, said) => {
                self.state.filing.retain(|w| w.id != id);
                self.said.say(said);
                self.mark_dirty();
            }
            Msg::Claimed(done) => {
                self.invite = None;
                done?;
                self.mark_dirty();
            }
            Msg::Moving(writing) => self.state.moving = Some(writing),
            Msg::Moved(skipped) => {
                (self.moving, self.state.moving) = (false, None);
                self.state.unmoved = skipped?;
                self.mark_dirty();
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
            Command::Open { id } => {
                self.opened = Some(id);
                self.mark_dirty();
            }
            Command::Close => {
                self.opened = None;
                self.state.open = None;
            }
            // A line is corrected on the record, not on a draft about to be replaced.
            Command::Change { id, change: Change::Line { .. } } if self.busy(&id) => {
                return Err("recording.writing");
            }
            Command::Change { id, change } => self.work.change(id, change)?,
            Command::Remove { id } => {
                if self.under_way(&id) {
                    self.stop();
                }
                self.work.remove(id)?;
            }
            Command::Restore { id } => self.work.restore(id)?,
            Command::Search { query } => {
                self.search_seq += 1;
                self.work.search(self.search_seq, query)?;
            }
            Command::Export { id, exporter, languages, to } => {
                let exporter = EXPORTERS.iter().find(|e| e.id == exporter).ok_or("export.which")?;
                self.work.export(id, exporter.id, languages, to)?;
            }
            Command::Share { recording } => {
                if self.state.rooms.iter().any(|room| room.recording == recording) {
                    return Ok(());
                }
                self.work.share(recording)?;
            }
            Command::Unshare { room } => {
                let existing = self.state.rooms.iter().find(|r| r.id == room).ok_or("room.gone")?;
                let recording = existing.recording.clone();
                self.work.unshare(room, recording)?;
            }
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
                self.work.sign(
                    Who::of(None, local),
                    "sign_out",
                    false,
                    self.state.setup.keep,
                    self.busy_ids(),
                )?;
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
            Command::DestroyAudio { id } => {
                if self.under_way(&id) {
                    return Err("recording.under_way");
                }
                let under_way = self.recording.as_ref().map(|(rec, _, _)| rec.clone());
                self.work.destroy_audio(id, under_way)?;
            }
            Command::Term { said, written } => self.work.term(said, written)?,
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
                let calendar = self.state.city.clone().ok_or("city.none")?;
                self.work.begin_file(id, calendar, event, audio)?;
            }
        }
        Ok(())
    }

    fn start_filing(&mut self, id: String, filing: Filing) -> Result<(), &'static str> {
        if self.state.filing.iter().any(|w| w.id == id) {
            return Ok(()); // it is on its way
        }
        self.work.file(id.clone(), filing)?;
        self.state.filing.push(writing(&id, "filing"));
        Ok(())
    }

    fn under_way(&self, id: &str) -> bool {
        self.recording.as_ref().is_some_and(|(under_way, _, _)| under_way == id)
    }

    fn busy(&self, id: &str) -> bool {
        self.under_way(id) || self.state.writing.iter().any(|w| w.id == id)
    }

    fn busy_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.state.writing.iter().map(|w| w.id.clone()).collect();
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
        let (spoken, shown) = (setup.spoken, setup.shown);
        let captions = lane::captions(file, &shown, self.has_key(Service::Soniox));
        let initial = match &captions {
            Ok(_) => Status::Connecting,
            Err(why) => Status::AudioOnly { why: (*why).into() },
        };
        let about =
            About { started, title, spoken, shown, source: source_name, ..About::default() };
        let (mode, watching) = watch::channel(Mode::Recording);
        let (reconnect, reconnect_rx) = watch::channel(0u64);
        self.work.start_recording(
            id.clone(),
            about,
            source,
            captions,
            watching,
            reconnect_rx,
            self.state.terms.clone(),
            self.frame_reg.clone(),
            self.session_reg.clone(),
            self.level_reg.clone(),
            self.water_reg.clone(),
            self.shell.clone(),
            self.screen.clone(),
            self.next_snap.clone(),
        )?;
        self.session_reg.set(Some(Session { id: id.clone(), started, status: initial }));
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
            self.state.writing.push(writing(&id, "closing"));
        }
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
        let Some(me) = self.state.me.clone() else { return };
        if self.config.old.is_none() || self.moving {
            return;
        }
        if self.work.migrate(me) {
            self.moving = true;
        }
    }
}
