//! Off-hub work: the sequential `desk` queue for local data IO, plus dedicated supervised tasks
//! on the `work` subtree for search, export file writes, rooms/slots, whole-recording
//! transcription, city filing, and 0.8 library migration (`P1.md` §3.6–§3.8, `P1-19`).

use crate::ai::write;
use crate::app::core::{Config, Voice};
use crate::app::hub::Msg;
use crate::app::proto::{Change, From, Open, Room, Row, State, Writing};
use crate::app::recording;
use crate::capture::clock::wall_ms;
use crate::capture::tape;
use crate::id::keys::{self, Chain, Me, Ours, Service, Who};
use crate::knowledge::glossary::{self, Term};
use crate::live::frame::Frame;
use crate::live::lines::Line;
use crate::out::city::{self, Calendar, Filer, Filing};
use crate::out::export::{Doc, EXPORTERS};
use crate::out::room::{self, Write};
use crate::record::shelf::{
    About, FILING, LINE, NOTE, RECORDING, Recording, SHARE, SPEAKER, Share, Shelf, Words,
};
use crate::record::{migrate, retain};
use crate::rt::queue::{self, Full, Tx};
use crate::rt::signal::Reading;
use crate::rt::stats::Counter;
use crate::rt::supervise::{End, Tree, again};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use zutalk_log::{info, warn};

const WHOLE: &str = "stt-async-v6";
/// A live room is written at most this often (DESIGN.md, rooms). Origin to be traced.
const SLOT_MS: u64 = 250;
/// Jobs buffered for the desk worker: a burst of clicks or edits is dozens of jobs; full means
/// disk is stuck and the person must be told (`P1.md` §3.7).
pub const DESK_QUEUE_JOBS: usize = 64;

/// One unit of sequential disk work run by a [`Desk`].
pub trait DeskJob: Send + 'static {
    fn kind(&self) -> &'static str;
}

struct Queued<J> {
    seq: u64,
    queued_at: tokio::time::Instant,
    job: J,
}

/// Sequential blocking worker over a bounded `desk` queue (`Full::Reject`), emitting `job.start`
/// and `job.done` with queue wait and run durations and catching panics as `code = "panic"`.
pub struct Desk<J> {
    tx: Tx<Queued<J>>,
    next_seq: Arc<AtomicU64>,
}

impl<J> Clone for Desk<J> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), next_seq: self.next_seq.clone() }
    }
}

impl<J: DeskJob> Desk<J> {
    /// Starts the `desk` task on `tree`, running each queued job sequentially in
    /// `tree.blocking("desk.job", ...)` and forwarding each outcome to `deliver`.
    pub fn start<R, F, Fut>(
        tree: &Tree,
        next_seq: Arc<AtomicU64>,
        run: impl Fn(J) -> Result<R, &'static str> + Send + Sync + 'static,
        mut deliver: F,
    ) -> Self
    where
        R: Send + 'static,
        F: FnMut(&'static str, Result<R, &'static str>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let stats = tree.stats();
        let (tx, mut rx) =
            queue::bounded::<Queued<J>>(stats, "desk", DESK_QUEUE_JOBS, Full::Reject);
        let jobs_done = stats.counter("jobs.done");
        let jobs_failed = stats.counter("jobs.failed");
        let wait_ms_spread = stats.spread("desk.wait_ms");
        let job_ms_spread = stats.spread("desk.job_ms");
        let run = Arc::new(run);
        let worker_tree = tree.clone();
        tree.spawn("desk", async move {
            while let Some(Queued { seq, queued_at, job }) = rx.recv().await {
                let kind = job.kind();
                let waited_ms = queued_at.elapsed().as_millis() as u64;
                wait_ms_spread.note(waited_ms);
                info!("job.start", kind = kind, job = seq, waited_ms = waited_ms);
                let started = std::time::Instant::now();
                let run = run.clone();
                let outcome = worker_tree.blocking("desk.job", move || run(job)).await;
                let ms = started.elapsed().as_millis() as u64;
                job_ms_spread.note(ms);
                let result = match outcome {
                    Ok(Ok(val)) => {
                        info!("job.done", kind = kind, job = seq, code = "ok", ms = ms);
                        jobs_done.add(1);
                        Ok(val)
                    }
                    Ok(Err(code)) => {
                        warn!("job.done", kind = kind, job = seq, code = code, ms = ms);
                        jobs_failed.add(1);
                        Err(code)
                    }
                    Err(_) => {
                        if worker_tree.is_cancelled() {
                            break;
                        }
                        warn!("job.done", kind = kind, job = seq, code = "panic", ms = ms);
                        jobs_failed.add(1);
                        Err("panic")
                    }
                };
                deliver(kind, result).await;
            }
        });
        Self { tx, next_seq }
    }

    /// Offers a job without waiting. Returns `"busy"` when `desk` is full or closed.
    pub fn offer(&self, job: J) -> Result<u64, &'static str> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let queued = Queued { seq, queued_at: tokio::time::Instant::now(), job };
        self.tx.offer(queued).map(|()| seq).map_err(|_| "busy")
    }

    /// Waits for room on `desk` (used by internal background tasks committing a shelf record).
    pub async fn send(&self, job: J) -> Result<u64, &'static str> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let queued = Queued { seq, queued_at: tokio::time::Instant::now(), job };
        self.tx.with(Full::Wait).send(queued).await.map(|()| seq).map_err(|_| "busy")
    }
}

/// Snapshot of library, opened recording, glossary, keys, and rooms read on `desk`.
pub(crate) struct Refreshed {
    pub rev: u64,
    pub opened: Option<String>,
    pub keys: Vec<(String, From)>,
    pub left: Option<u64>,
    pub library: Vec<Row>,
    pub open: Option<Open>,
    pub terms: Vec<Term>,
    pub rooms: Option<Vec<Room>>,
    pub clear_hits: bool,
}

/// Unfinished background work surveyed on `desk` (`recover` + `resume`).
pub(crate) struct Survey {
    pub writes: Vec<String>,
    pub sweep: bool,
    pub filings: Vec<(String, Filing)>,
}

/// Complete result of switching the signing identity on `desk`.
pub(crate) struct SignedOut {
    pub me: Option<String>,
    pub rotation_unsaved: bool,
    pub refreshed: Refreshed,
    pub rooms: Vec<Room>,
    pub survey: Survey,
    pub say: Vec<&'static str>,
}

/// Result of updating an own service key on `desk`.
pub(crate) struct OwnKeyOut {
    pub keys: Vec<(String, From)>,
    pub left: Option<u64>,
    pub survey: Option<Survey>,
}

struct Identity {
    who: Who,
    shelf: Option<Arc<Shelf>>,
    ours: Option<Ours>,
    keys: Arc<Chain>,
}

impl Identity {
    fn starting() -> Self {
        Self {
            who: Who::Nobody("starting"),
            shelf: None,
            ours: None,
            keys: Arc::new(Chain::new(None)),
        }
    }

    fn shelf(&self) -> Result<Arc<Shelf>, &'static str> {
        match &self.shelf {
            Some(shelf) => Ok(shelf.clone()),
            None => Err(self.who.me().err().unwrap_or("keys.store")),
        }
    }
}

#[derive(Clone)]
struct Snapshot {
    shelf: Result<Arc<Shelf>, &'static str>,
    ours: Option<Ours>,
    keys: Arc<Chain>,
}

enum Job {
    Sign {
        who: Who,
        why: &'static str,
        made: bool,
        keep: Option<u32>,
        busy: Vec<String>,
        boot: Option<oneshot::Sender<Result<SignedOut, &'static str>>>,
    },
    Refresh {
        rev: u64,
        opened: Option<String>,
        retain: Option<Option<u32>>,
        include_rooms: bool,
    },
    Change {
        id: String,
        change: Change,
    },
    Remove {
        id: String,
    },
    Restore {
        id: String,
    },
    Term {
        said: String,
        written: Option<BTreeMap<String, String>>,
    },
    DestroyAudio {
        id: String,
        under_way: Option<String>,
    },
    Export {
        id: String,
        exporter: &'static str,
        languages: Vec<String>,
        to: String,
    },
    SharePrepare {
        recording: Option<String>,
    },
    PutShare {
        id: String,
        share: Option<Share>,
        reply: oneshot::Sender<Result<(), &'static str>>,
    },
    UnsharePrepare {
        id: String,
        recording: Option<String>,
    },
    BeginFile {
        id: String,
        calendar: Calendar,
        event: String,
        audio: bool,
    },
    OwnKey {
        service: Service,
        key: Option<String>,
        busy: Vec<String>,
    },
}

impl DeskJob for Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::Sign { .. } => "sign",
            Job::Refresh { .. } => "refresh",
            Job::Change { .. } => "change",
            Job::Remove { .. } => "remove",
            Job::Restore { .. } => "restore",
            Job::Term { .. } => "term",
            Job::DestroyAudio { .. } => "destroy",
            Job::Export { .. } => "export",
            Job::SharePrepare { .. } => "share.prepare",
            Job::PutShare { .. } => "share.put",
            Job::UnsharePrepare { .. } => "unshare.prepare",
            Job::BeginFile { .. } => "file.begin",
            Job::OwnKey { .. } => "own_key",
        }
    }
}

enum DeskOut {
    Changed,
    Silent,
    BootReplied,
    Signed(SignedOut),
    Refreshed(Refreshed),
    ExportWrite {
        to: PathBuf,
        text: String,
    },
    ShareNet {
        ours: Ours,
        id: String,
        key: [u8; 32],
        recording: Option<String>,
        pages: Vec<Vec<u8>>,
    },
    UnshareNet {
        ours: Ours,
        id: String,
        key: [u8; 32],
        share: Share,
        rooms: Vec<Room>,
    },
    FileReady {
        id: String,
        filing: Filing,
    },
    OwnKey(OwnKeyOut),
}

/// Everything the hub hands off: sequential `desk` jobs and separate supervised tasks on the
/// `work` subtree.
pub(crate) struct Work {
    config: Arc<Config>,
    http: reqwest::Client,
    results: Tx<Msg>,
    progress: Tx<Msg>,
    said: Voice,
    root: Tree,
    tree: Tree,
    frame: Reading<Frame>,
    identity: Arc<Mutex<Identity>>,
    desk: Desk<Job>,
    next_seq: Arc<AtomicU64>,
    slot: Arc<Mutex<Option<(String, CancellationToken)>>>,
    jobs_done: Counter,
    jobs_failed: Counter,
    search_stale: Counter,
}

impl Work {
    pub(crate) fn new(
        config: Arc<Config>,
        http: reqwest::Client,
        results: Tx<Msg>,
        progress: Tx<Msg>,
        said: Voice,
        root: Tree,
        frame: Reading<Frame>,
    ) -> Self {
        let tree = root.child("work");
        let stats = tree.stats();
        let jobs_done = stats.counter("jobs.done");
        let jobs_failed = stats.counter("jobs.failed");
        let search_stale = stats.counter("search.stale");
        let next_seq = Arc::new(AtomicU64::new(1));
        let identity = Arc::new(Mutex::new(Identity::starting()));
        let slot: Arc<Mutex<Option<(String, CancellationToken)>>> = Arc::new(Mutex::new(None));

        let run_cfg = config.clone();
        let run_http = http.clone();
        let run_id = identity.clone();

        let deliver_results = results.clone();
        let deliver_tree = tree.clone();
        let deliver_seq = next_seq.clone();
        let deliver_done = jobs_done.clone();
        let deliver_failed = jobs_failed.clone();
        let deliver_slot = slot.clone();
        let deliver_frame = frame.clone();
        let deliver_said = said.clone();
        let deliver_id = identity.clone();

        let desk_tx_holder: Arc<Mutex<Option<Desk<Job>>>> = Arc::new(Mutex::new(None));
        let deliver_desk = desk_tx_holder.clone();

        let desk = Desk::start(
            &tree,
            next_seq.clone(),
            move |job| run_desk_job(job, &run_cfg, &run_http, &run_id),
            move |kind, outcome| {
                let results = deliver_results.clone();
                let tree = deliver_tree.clone();
                let next_seq = deliver_seq.clone();
                let jobs_done = deliver_done.clone();
                let jobs_failed = deliver_failed.clone();
                let slot = deliver_slot.clone();
                let frame = deliver_frame.clone();
                let said = deliver_said.clone();
                let identity = deliver_id.clone();
                let desk_holder = deliver_desk.clone();
                async move {
                    match outcome {
                        Ok(DeskOut::Changed) => {
                            let _ = results.send(Msg::DeskChanged).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Silent) | Ok(DeskOut::BootReplied) => {}
                        Ok(DeskOut::Signed(out)) => {
                            let live = out
                                .rooms
                                .iter()
                                .find(|r| r.recording.is_none())
                                .map(|r| r.id.clone());
                            let ours = snapshot(&identity).ours;
                            sync_slot_inner(
                                &tree, &slot, live, ours, &frame, &said, &next_seq, &jobs_done,
                            );
                            let _ = results.send(Msg::Signed(Ok(out))).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Refreshed(refreshed)) => {
                            if let Some(ref rooms) = refreshed.rooms {
                                let live = rooms
                                    .iter()
                                    .find(|r| r.recording.is_none())
                                    .map(|r| r.id.clone());
                                let ours = snapshot(&identity).ours;
                                sync_slot_inner(
                                    &tree, &slot, live, ours, &frame, &said, &next_seq, &jobs_done,
                                );
                            }
                            let _ = results.send(Msg::Refreshed(Ok(refreshed))).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::ExportWrite { to, text }) => {
                            spawn_export_write(
                                &tree,
                                &next_seq,
                                &jobs_done,
                                &jobs_failed,
                                results,
                                to,
                                text,
                            );
                        }
                        Ok(DeskOut::ShareNet { ours, id, key, recording, pages }) => {
                            let desk =
                                desk_holder.lock().unwrap_or_else(PoisonError::into_inner).clone();
                            if let Some(desk) = desk {
                                spawn_share_net(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    desk,
                                    results,
                                    ours,
                                    id,
                                    key,
                                    recording,
                                    pages,
                                );
                            }
                        }
                        Ok(DeskOut::UnshareNet { ours, id, key, share, rooms }) => {
                            let live =
                                rooms.iter().find(|r| r.recording.is_none()).map(|r| r.id.clone());
                            sync_slot_inner(
                                &tree,
                                &slot,
                                live,
                                Some(ours.clone()),
                                &frame,
                                &said,
                                &next_seq,
                                &jobs_done,
                            );
                            let _ = results.send(Msg::Rooms(rooms)).await; // dropped: the hub has ended
                            let desk =
                                desk_holder.lock().unwrap_or_else(PoisonError::into_inner).clone();
                            if let Some(desk) = desk {
                                spawn_unshare_net(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    desk,
                                    results,
                                    ours,
                                    id,
                                    key,
                                    share,
                                );
                            }
                        }
                        Ok(DeskOut::FileReady { id, filing }) => {
                            let _ = results.send(Msg::FilingReady(id, filing)).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::OwnKey(out)) => {
                            let _ = results.send(Msg::OwnKeyDone(Ok(out))).await; // dropped: the hub has ended
                        }
                        Err(code) => {
                            let msg = match kind {
                                "refresh" => Msg::Refreshed(Err(code)),
                                "sign" => Msg::Signed(Err(code)),
                                "own_key" => Msg::OwnKeyDone(Err(code)),
                                _ => Msg::DeskFailed(code),
                            };
                            let _ = results.send(msg).await; // dropped: the hub has ended
                        }
                    }
                }
            },
        );
        *desk_tx_holder.lock().unwrap_or_else(PoisonError::into_inner) = Some(desk.clone());

        Self {
            config,
            http,
            results,
            progress,
            said,
            root,
            tree,
            frame,
            identity,
            desk,
            next_seq,
            slot,
            jobs_done,
            jobs_failed,
            search_stale,
        }
    }

    fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Snapshot of the current key chain (used by `hub.rs` for invite claim until `P1-20`).
    pub(crate) fn keys_snapshot(&self) -> Arc<Chain> {
        snapshot(&self.identity).keys
    }

    /// Initial boot sign on `desk`, returning a oneshot awaited before `Hub` enters its loop
    /// (moved into async boot in `P1-20`).
    pub(crate) fn sign_boot(
        &self,
        who: Who,
        made: bool,
        keep: Option<u32>,
    ) -> Result<oneshot::Receiver<Result<SignedOut, &'static str>>, &'static str> {
        let (tx, rx) = oneshot::channel();
        self.desk.offer(Job::Sign {
            who,
            why: "boot",
            made,
            keep,
            busy: Vec::new(),
            boot: Some(tx),
        })?;
        Ok(rx)
    }

    pub(crate) fn sign(
        &self,
        who: Who,
        why: &'static str,
        made: bool,
        keep: Option<u32>,
        busy: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Sign { who, why, made, keep, busy, boot: None }).map(|_| ())
    }

    pub(crate) fn refresh(
        &self,
        rev: u64,
        opened: Option<String>,
        retain: Option<Option<u32>>,
        include_rooms: bool,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Refresh { rev, opened, retain, include_rooms }).map(|_| ())
    }

    pub(crate) fn change(&self, id: String, change: Change) -> Result<(), &'static str> {
        self.desk.offer(Job::Change { id, change }).map(|_| ())
    }

    pub(crate) fn remove(&self, id: String) -> Result<(), &'static str> {
        self.desk.offer(Job::Remove { id }).map(|_| ())
    }

    pub(crate) fn restore(&self, id: String) -> Result<(), &'static str> {
        self.desk.offer(Job::Restore { id }).map(|_| ())
    }

    pub(crate) fn term(
        &self,
        said: String,
        written: Option<BTreeMap<String, String>>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Term { said, written }).map(|_| ())
    }

    pub(crate) fn destroy_audio(
        &self,
        id: String,
        under_way: Option<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::DestroyAudio { id, under_way }).map(|_| ())
    }

    pub(crate) fn export(
        &self,
        id: String,
        exporter: &'static str,
        languages: Vec<String>,
        to: String,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Export { id, exporter, languages, to }).map(|_| ())
    }

    pub(crate) fn share(&self, recording: Option<String>) -> Result<(), &'static str> {
        self.desk.offer(Job::SharePrepare { recording }).map(|_| ())
    }

    pub(crate) fn unshare(
        &self,
        id: String,
        recording: Option<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::UnsharePrepare { id, recording }).map(|_| ())
    }

    pub(crate) fn begin_file(
        &self,
        id: String,
        calendar: Calendar,
        event: String,
        audio: bool,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::BeginFile { id, calendar, event, audio }).map(|_| ())
    }

    pub(crate) fn own_key(
        &self,
        service: Service,
        key: Option<String>,
        busy: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::OwnKey { service, key, busy }).map(|_| ())
    }

    /// Starts a live room's slot task or stops the previous one on the `work` subtree.
    pub(crate) fn sync_slot(&self, live: Option<String>) {
        let ours = snapshot(&self.identity).ours;
        sync_slot_inner(
            &self.tree,
            &self.slot,
            live,
            ours,
            &self.frame,
            &self.said,
            &self.next_seq,
            &self.jobs_done,
        );
    }

    /// Runs a search off the `desk` in its own `search` task on the `work` subtree.
    pub(crate) fn search(&self, search_seq: u64, query: String) -> Result<(), &'static str> {
        let shelf = snapshot(&self.identity).shelf?;
        let seq = self.alloc_seq();
        let tree = self.tree.clone();
        let results = self.results.clone();
        self.tree.spawn("search", async move {
            info!("job.start", kind = "search", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tree.blocking("search", move || shelf.search(&query).map_err(fail)).await;
            let ms = started.elapsed().as_millis() as u64;
            let result = match outcome {
                Ok(res) => res,
                Err(_) => {
                    if tree.is_cancelled() {
                        return;
                    }
                    Err("panic")
                }
            };
            let _ = results.send(Msg::Searched { seq, search_seq, ms, result }).await; // dropped: the hub has ended
        });
        Ok(())
    }

    pub(crate) fn note_job_done(&self) {
        self.jobs_done.add(1);
    }

    pub(crate) fn note_job_failed(&self) {
        self.jobs_failed.add(1);
    }

    pub(crate) fn note_search_stale(&self) {
        self.search_stale.add(1);
    }

    /// Starts a recording task on the root tree with `Work`'s current shelf and key chain,
    /// followed by post-seal transcription on the `work` subtree.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_recording(
        &self,
        id: String,
        about: About,
        source: crate::capture::audio::Source,
        captions: Result<Vec<crate::live::lane::Stream>, &'static str>,
        mode: tokio::sync::watch::Receiver<crate::live::lane::Mode>,
        reconnect: tokio::sync::watch::Receiver<u64>,
        terms: Vec<Term>,
        frame: crate::rt::signal::Register<Frame>,
        session: crate::rt::signal::Register<Option<crate::app::proto::Session>>,
        level: crate::rt::signal::Register<crate::app::proto::Level>,
        water: crate::rt::signal::Register<crate::rt::water::Water>,
        shell: Arc<crate::rt::water::Gauge>,
        screen: Arc<crate::rt::water::Gauge>,
        next_snap: Arc<AtomicU64>,
    ) -> Result<(), &'static str> {
        let snap = snapshot(&self.identity);
        let shelf = snap.shelf?;
        let keys = snap.keys;
        let rec_start = recording::Start {
            id: id.clone(),
            about,
            source,
            captions,
            mode,
            reconnect,
            shelf: shelf.clone(),
            keys: keys.clone(),
            config: self.config.clone(),
            terms: terms.clone(),
            frame,
            session,
            level,
            water,
            said: self.said.clone(),
            shell,
            screen,
            next_snap,
            root: self.root.clone(),
            results: self.results.clone(),
        };
        let post = WriteCtx {
            id,
            shelf,
            keys,
            config: self.config.clone(),
            results: self.results.clone(),
            progress: self.progress.clone(),
            said: self.said.clone(),
            work_tree: self.tree.clone(),
            root: self.root.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
            terms,
        };
        self.root.spawn("recording", async move {
            let done = recording::run(rec_start).await;
            post.after_seal(done.sealed).await;
        });
        Ok(())
    }

    /// Sweeps any orphaned remote files and starts `job.write` tasks for owed drafts.
    pub(crate) fn write_recovered(&self, ids: Vec<String>, terms: Vec<Term>) {
        let snap = snapshot(&self.identity);
        let Ok(shelf) = snap.shelf else { return };
        let keys = snap.keys;
        let tapes = self.config.tapes();
        let soniox = self.config.soniox.clone();
        let said = self.said.clone();
        let work_tree = self.tree.clone();
        let root = self.root.clone();
        let config = self.config.clone();
        let results = self.results.clone();
        let progress = self.progress.clone();
        let next_seq = self.next_seq.clone();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        let sweep_seq = self.alloc_seq();
        self.tree.spawn("job.sweep", async move {
            info!("job.start", kind = "sweep", job = sweep_seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let res = write::sweep(&tapes, &soniox, &*keys).await;
            let ms = started.elapsed().as_millis() as u64;
            match res {
                End::Done => {
                    info!("job.done", kind = "sweep", job = sweep_seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                }
                End::Retry(c) | End::Fatal(c) => {
                    warn!("job.done", kind = "sweep", job = sweep_seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    said.say(c);
                }
            }
            for id in ids {
                let ctx = WriteCtx {
                    id,
                    shelf: shelf.clone(),
                    keys: keys.clone(),
                    config: config.clone(),
                    results: results.clone(),
                    progress: progress.clone(),
                    said: said.clone(),
                    work_tree: work_tree.clone(),
                    root: root.clone(),
                    next_seq: next_seq.clone(),
                    jobs_done: jobs_done.clone(),
                    jobs_failed: jobs_failed.clone(),
                    terms: terms.clone(),
                };
                work_tree.spawn("job.write", async move {
                    ctx.finish_recovered().await;
                });
            }
        });
    }

    /// Files one recording to a city on the `work` subtree (`job.file`).
    pub(crate) fn file(&self, id: String, filing: Filing) -> Result<(), &'static str> {
        let snap = snapshot(&self.identity);
        let me = snap.ours.ok_or("sign_in.first")?.me;
        let shelf = snap.shelf?;
        let filer = Filer { http: self.http.clone(), me, shelf, tapes: self.config.tapes() };
        let results = self.results.clone();
        let progress = self.progress.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.file", async move {
            info!("job.start", kind = "file", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let said = city::file(&filer, &id, filing, |w| {
                let _ = progress.offer(Msg::Filing(w)); // dropped: the next one replaces it
            })
            .await;
            let ms = started.elapsed().as_millis() as u64;
            if said == "city.filed" {
                info!("job.done", kind = "file", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            } else {
                warn!("job.done", kind = "file", job = seq, code = said, ms = ms);
                jobs_failed.add(1);
            }
            let _ = results.send(Msg::Filed(id, said)).await; // dropped: the hub has ended
        });
        Ok(())
    }

    /// Moves the 0.8 library into the signed-in person's shelf on the `work` subtree (`job.move`).
    pub(crate) fn migrate(&self, me: String) -> bool {
        let Some(old) = self.config.old.clone() else { return false };
        let Ok(shelf) = snapshot(&self.identity).shelf else { return false };
        let data = self.config.data.clone();
        let results = self.results.clone();
        let progress = self.progress.clone();
        let tree = self.tree.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.move", async move {
            info!("job.start", kind = "move", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tree
                .blocking("job.move", move || {
                    let progress = |writing| {
                        let _ = progress.offer(Msg::Moving(writing)); // dropped: the next one replaces it
                    };
                    migrate::run((&old, &data), &shelf, &me, now, progress)
                })
                .await;
            let ms = started.elapsed().as_millis() as u64;
            let moved = match outcome {
                Ok(Ok(skipped)) => {
                    info!("job.done", kind = "move", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    Ok(skipped)
                }
                Ok(Err(c)) => {
                    warn!("job.done", kind = "move", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    Err(c)
                }
                Err(_) => {
                    if tree.is_cancelled() {
                        return;
                    }
                    warn!("job.done", kind = "move", job = seq, code = "panic", ms = ms);
                    jobs_failed.add(1);
                    Err("panic")
                }
            };
            let _ = results.send(Msg::Moved(moved)).await; // dropped: the hub has ended
        });
        true
    }
}

fn snapshot(identity: &Mutex<Identity>) -> Snapshot {
    let id = identity.lock().unwrap_or_else(PoisonError::into_inner);
    Snapshot { shelf: id.shelf(), ours: id.ours.clone(), keys: id.keys.clone() }
}

fn run_desk_job(
    job: Job,
    config: &Config,
    http: &reqwest::Client,
    identity: &Mutex<Identity>,
) -> Result<DeskOut, &'static str> {
    match job {
        Job::Sign { who, why, made, keep, busy, boot } => {
            let res = run_sign(config, http, identity, who, why, made, keep, &busy);
            if let Some(reply) = boot {
                let _ = reply.send(res); // dropped: caller timed out or shut down
                return Ok(DeskOut::BootReplied);
            }
            res.map(DeskOut::Signed)
        }
        Job::Refresh { rev, opened, retain, include_rooms } => {
            let snap = snapshot(identity);
            run_refresh(config, &snap, rev, opened, retain, include_rooms).map(DeskOut::Refreshed)
        }
        Job::Change { id, change } => {
            let shelf = snapshot(identity).shelf?;
            apply(&shelf, &id, change)?;
            Ok(DeskOut::Changed)
        }
        Job::Remove { id } => {
            let shelf = snapshot(identity).shelf?;
            shelf.put::<About>(&id, RECORDING, &[(id.clone(), None)], now()).map_err(fail)?;
            Ok(DeskOut::Changed)
        }
        Job::Restore { id } => {
            let shelf = snapshot(identity).shelf?;
            shelf.restore(&id, now()).map_err(fail)?;
            Ok(DeskOut::Changed)
        }
        Job::Term { said, written } => {
            let shelf = snapshot(identity).shelf?;
            glossary::put(&shelf, &said, written, now())?;
            Ok(DeskOut::Changed)
        }
        Job::DestroyAudio { id, under_way } => {
            let shelf = snapshot(identity).shelf?;
            let tapes = config.tapes();
            let library: Vec<Row> =
                shelf.list().map_err(fail)?.iter().map(|r| row(r, &tapes)).collect();
            retain::destroy(&tapes, &library, under_way.as_deref(), &id)?;
            Ok(DeskOut::Changed)
        }
        Job::Export { id, exporter, languages, to } => {
            let shelf = snapshot(identity).shelf?;
            let recording = shelf.read(&id).map_err(fail)?.ok_or("recording.gone")?;
            let exporter = EXPORTERS.iter().find(|e| e.id == exporter).ok_or("export.which")?;
            let lines: Vec<Line> = recording.lines.into_iter().map(|(_, s)| s.line).collect();
            let (title, speakers) = (&recording.about.title, &recording.speakers);
            let text = (exporter.render)(&Doc { title, lines: &lines, speakers }, &languages);
            let mut to = PathBuf::from(to);
            if to.extension().is_none() {
                to.set_extension(exporter.ext);
            }
            Ok(DeskOut::ExportWrite { to, text })
        }
        Job::SharePrepare { recording } => {
            let snap = snapshot(identity);
            let ours = snap.ours.ok_or("sign_in.first")?;
            let shelf = snap.shelf?;
            let opened = recording.as_deref().map(|id| shelf.read(id)).transpose().map_err(fail)?;
            let opened = opened.map(|read| read.ok_or("recording.gone")).transpose()?;
            let tapes = config.tapes();
            let pages = opened.map(|rec| room::pages(&open(rec, &tapes))).transpose();
            let pages = pages.map_err(code)?.unwrap_or_default();
            let id = keys::fresh().map_err(code)?;
            let key = keys::derive("room", &id).map_err(code)?;
            Ok(DeskOut::ShareNet { ours, id, key, recording, pages })
        }
        Job::PutShare { id, share, reply } => {
            let res = snapshot(identity)
                .shelf
                .and_then(|shelf| shelf.put("", SHARE, &[(id, share)], now()).map_err(fail));
            let _ = reply.send(res); // dropped: net task was cancelled
            res.map(|()| DeskOut::Silent)
        }
        Job::UnsharePrepare { id, recording } => {
            let snap = snapshot(identity);
            let ours = snap.ours.ok_or("sign_in.first")?;
            let shelf = snap.shelf?;
            let share = Share { recording };
            let key = keys::derive("room", &id).map_err(code)?;
            shelf.put::<Share>("", SHARE, &[(id.clone(), None)], now()).map_err(fail)?;
            let rooms = read_rooms(config, Some(&shelf))?;
            Ok(DeskOut::UnshareNet { ours, id, key, share, rooms })
        }
        Job::BeginFile { id, calendar, event, audio } => {
            let shelf = snapshot(identity).shelf?;
            let filing = city::begin(&shelf, &id, &calendar, &event, audio)?;
            Ok(DeskOut::FileReady { id, filing })
        }
        Job::OwnKey { service, key, busy } => {
            let snap = snapshot(identity);
            snap.keys.own(service, key.as_deref()).map_err(code)?;
            let keys_from: Vec<(String, From)> =
                Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
            let left = snap.keys.left();
            let has_soniox = snap.keys.from(Service::Soniox) != From::Nowhere;
            let survey = if service == Service::Soniox && has_soniox {
                Some(run_survey(config, &snap, &busy, false)?)
            } else {
                None
            };
            Ok(DeskOut::OwnKey(OwnKeyOut { keys: keys_from, left, survey }))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_sign(
    config: &Config,
    http: &reqwest::Client,
    identity: &Mutex<Identity>,
    who: Who,
    why: &'static str,
    made: bool,
    keep: Option<u32>,
    busy: &[String],
) -> Result<SignedOut, &'static str> {
    let mut say = Vec::new();
    let records = config.data.join("records");
    let make_shelf = |me: &Me| {
        Arc::new(Shelf::new(records.join(me.did.replace(':', "-")), &me.did, me.key.clone()))
    };
    let shelf = who.me().ok().map(make_shelf);
    let ours = who.person().cloned().map(|me| Ours {
        server: config.server.clone(),
        me,
        http: http.clone(),
    });
    let keys = Arc::new(Chain::new(ours.clone()));
    let me = who.person().map(|me| me.did.clone());
    let rotation_unsaved = keys::unsaved();

    {
        let mut guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
        let (from, to) = (guard.who.code(), who.code());
        if let Who::Nobody(code) = &who {
            warn!("identity.who", from = from, to = to, why = why, made = made);
            say.push(*code);
        } else {
            info!("identity.who", from = from, to = to, why = why, made = made);
        }
        guard.who = who;
        guard.shelf = shelf;
        guard.ours = ours;
        guard.keys = keys;
    }

    let snap = snapshot(identity);
    let refreshed = match run_refresh(config, &snap, 0, None, Some(keep), true) {
        Ok(refreshed) => refreshed,
        Err(c) => {
            say.push(c);
            Refreshed {
                rev: 0,
                opened: None,
                keys: Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect(),
                left: snap.keys.left(),
                library: Vec::new(),
                open: None,
                terms: Vec::new(),
                rooms: Some(Vec::new()),
                clear_hits: true,
            }
        }
    };
    let rooms = refreshed.rooms.clone().unwrap_or_default();
    let survey = match run_survey(config, &snap, busy, true) {
        Ok(survey) => survey,
        Err(c) => {
            say.push(c);
            Survey { writes: Vec::new(), sweep: false, filings: Vec::new() }
        }
    };

    Ok(SignedOut { me, rotation_unsaved, refreshed, rooms, survey, say })
}

fn run_refresh(
    config: &Config,
    snap: &Snapshot,
    rev: u64,
    opened: Option<String>,
    retain: Option<Option<u32>>,
    include_rooms: bool,
) -> Result<Refreshed, &'static str> {
    let keys: Vec<(String, From)> =
        Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
    let left = snap.keys.left();
    let Ok(shelf) = &snap.shelf else {
        return Ok(Refreshed {
            rev,
            opened,
            keys,
            left,
            library: Vec::new(),
            open: None,
            terms: Vec::new(),
            rooms: include_rooms.then_some(Vec::new()),
            clear_hits: true,
        });
    };
    let tapes = config.tapes();
    let mut library: Vec<Row> =
        shelf.list().map_err(fail)?.iter().map(|r| row(r, &tapes)).collect();
    if let Some(keep) = retain {
        let mut tmp = State { library, ..State::default() };
        tmp.setup.keep = keep;
        retain::sweep(&tapes, &mut tmp, now())?;
        library = tmp.library;
    }
    let read = opened.as_deref().map(|id| shelf.read(id)).transpose().map_err(fail)?;
    let mut open_rec = read.flatten().map(|recording| open(recording, &tapes));
    let terms = glossary::read(shelf)?;
    if let Some(ref mut open_rec) = open_rec {
        let mut filed = shelf.kept::<Filing>(FILING).map_err(fail)?.into_iter();
        open_rec.filed = filed.find_map(|(id, filing)| (id == open_rec.row.id).then_some(filing));
    }
    let rooms = if include_rooms { Some(read_rooms(config, Some(shelf))?) } else { None };
    Ok(Refreshed {
        rev,
        opened,
        keys,
        left,
        library,
        open: open_rec,
        terms,
        rooms,
        clear_hits: false,
    })
}

fn read_rooms(config: &Config, shelf: Option<&Shelf>) -> Result<Vec<Room>, &'static str> {
    let shares = shelf.map(|shelf| shelf.kept(SHARE)).transpose().map_err(fail)?;
    let server = &config.server;
    let to_room = |(id, share): (String, Share)| -> Result<Room, &'static str> {
        let link = room::link(server, &id, &keys::derive("room", &id).map_err(code)?);
        Ok(Room { id, recording: share.recording, link })
    };
    shares.into_iter().flatten().map(to_room).collect()
}

fn run_survey(
    config: &Config,
    snap: &Snapshot,
    busy: &[String],
    include_filings: bool,
) -> Result<Survey, &'static str> {
    let Ok(shelf) = &snap.shelf else {
        return Ok(Survey { writes: Vec::new(), sweep: false, filings: Vec::new() });
    };
    let tapes = config.tapes();
    let has_key = snap.keys.from(Service::Soniox) != From::Nowhere;
    let rows: Vec<Row> = shelf.list().map_err(fail)?.iter().map(|r| row(r, &tapes)).collect();
    let (mut writes, mut skipped, mut why) = (Vec::new(), 0usize, "");
    for r in &rows {
        if busy.iter().any(|id| id == &r.id) {
            continue;
        }
        // Nothing is written again without its audio, nor for one moved from 0.8: it keeps the
        // transcript it had there, or the lack of one, and its sound is not sent anywhere.
        let moved = r.draft
            && r.audio
            && shelf.about(&r.id).ok().flatten().is_none_or(|rec| rec.about.moved);
        match write::owed(r.draft, r.audio, moved, has_key) {
            write::Owed::Yes => writes.push(r.id.clone()),
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
    let filings = if include_filings && snap.ours.is_some() {
        shelf
            .kept::<Filing>(FILING)
            .map_err(fail)?
            .into_iter()
            .filter(|(_, f)| f.state.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    Ok(Survey { writes, sweep: has_key, filings })
}

fn spawn_export_write(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    results: Tx<Msg>,
    to: PathBuf,
    text: String,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let run_tree = tree.clone();
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("export.write", async move {
        info!("job.start", kind = "export.write", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let outcome = run_tree
            .blocking("export.write", move || fs::write(to, text).map_err(|_| "export.write"))
            .await;
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(Ok(())) => {
                info!("job.done", kind = "export.write", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            }
            Ok(Err(c)) => {
                warn!("job.done", kind = "export.write", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
                let _ = results.send(Msg::DeskFailed(c)).await; // dropped: the hub has ended
            }
            Err(_) => {
                if run_tree.is_cancelled() {
                    return;
                }
                warn!("job.done", kind = "export.write", job = seq, code = "panic", ms = ms);
                jobs_failed.add(1);
                let _ = results.send(Msg::DeskFailed("panic")).await; // dropped: the hub has ended
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_share_net(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    desk: Desk<Job>,
    results: Tx<Msg>,
    ours: Ours,
    id: String,
    key: [u8; 32],
    recording: Option<String>,
    pages: Vec<Vec<u8>>,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.share", async move {
        info!("job.start", kind = "share", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let shared: Result<(), &'static str> = async {
            room::write(&ours, &id, &key, Write::Make).await.map_err(code)?;
            let (tx, rx) = oneshot::channel();
            desk.send(Job::PutShare {
                id: id.clone(),
                share: Some(Share { recording }),
                reply: tx,
            })
            .await?;
            rx.await.map_err(|_| "shelf")??;
            for page in pages {
                room::write(&ours, &id, &key, Write::Log(&page)).await.map_err(code)?;
            }
            Ok(())
        }
        .await;
        let ms = started.elapsed().as_millis() as u64;
        match shared {
            Ok(()) => {
                info!("job.done", kind = "share", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            }
            Err(c) => {
                warn!("job.done", kind = "share", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
            }
        }
        let _ = results.send(Msg::Shared(shared)).await; // dropped: the hub has ended
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_unshare_net(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    desk: Desk<Job>,
    results: Tx<Msg>,
    ours: Ours,
    id: String,
    key: [u8; 32],
    share: Share,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.unshare", async move {
        info!("job.start", kind = "unshare", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let gone = match room::write(&ours, &id, &key, Write::Gone).await {
            Ok(()) | Err(End::Fatal("room.gone")) => Ok(()),
            Err(end) => {
                let (tx, rx) = oneshot::channel();
                let sent = desk.send(Job::PutShare { id, share: Some(share), reply: tx }).await;
                let back = match sent {
                    Ok(_) => rx.await.map_err(|_| "shelf").and_then(|r| r),
                    Err(c) => Err(c),
                };
                back.and(Err(code(end)))
            }
        };
        let ms = started.elapsed().as_millis() as u64;
        match gone {
            Ok(()) => {
                info!("job.done", kind = "unshare", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            }
            Err(c) => {
                warn!("job.done", kind = "unshare", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
            }
        }
        let _ = results.send(Msg::Shared(gone)).await; // dropped: the hub has ended
    });
}

#[allow(clippy::too_many_arguments)]
fn sync_slot_inner(
    tree: &Tree,
    slot_cell: &Mutex<Option<(String, CancellationToken)>>,
    live: Option<String>,
    ours: Option<Ours>,
    frame: &Reading<Frame>,
    voice: &Voice,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
) {
    let mut guard = slot_cell.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.as_ref().map(|(id, _)| id) == live.as_ref() {
        return;
    }
    if let Some((_, old_cancel)) = guard.take() {
        old_cancel.cancel();
    }
    if let (Some(id), Some(ours)) = (live, ours) {
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let seq = next_seq.fetch_add(1, Ordering::Relaxed);
        let live_rx = frame.clone();
        let voice = voice.clone();
        let jobs_done = jobs_done.clone();
        let slot_id = id.clone();
        tree.spawn("slot", async move {
            info!("job.start", kind = "slot", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            tokio::select! {
                () = run_cancel.cancelled() => {}
                () = slot_loop(ours, slot_id, live_rx, voice) => {}
            }
            let ms = started.elapsed().as_millis() as u64;
            info!("job.done", kind = "slot", job = seq, code = "ok", ms = ms);
            jobs_done.add(1);
        });
        *guard = Some((id, cancel));
    }
}

/// Keeps a live room's slot showing what the captions show: the latest, at most every
/// `SLOT_MS`, and nothing between recordings. Its trouble is said once, when it changes.
async fn slot_loop(ours: Ours, id: String, mut live: Reading<Frame>, voice: Voice) {
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

struct WriteCtx {
    id: String,
    shelf: Arc<Shelf>,
    keys: Arc<Chain>,
    config: Arc<Config>,
    results: Tx<Msg>,
    progress: Tx<Msg>,
    said: Voice,
    work_tree: Tree,
    root: Tree,
    next_seq: Arc<AtomicU64>,
    jobs_done: Counter,
    jobs_failed: Counter,
    terms: Vec<Term>,
}

impl WriteCtx {
    fn tape(&self) -> PathBuf {
        self.config.tapes().join(&self.id)
    }

    async fn finish_recovered(self) {
        let sealed = recording::seal(&self.root, self.shelf.clone(), self.tape(), &self.id).await;
        if let Err(c) = sealed {
            self.said.say(c);
        }
        self.after_seal_inner(sealed, true).await;
    }

    async fn after_seal(self, sealed: Result<Option<About>, &'static str>) {
        self.after_seal_inner(sealed, false).await;
    }

    async fn after_seal_inner(
        self,
        sealed: Result<Option<About>, &'static str>,
        on_write_task: bool,
    ) {
        match sealed {
            Ok(Some(about)) if about.length_ms == 0 => {
                let tape_dir = self.tape();
                let destroyed =
                    self.work_tree.blocking("tape.destroy", move || tape::destroy(&tape_dir)).await;
                if matches!(destroyed, Ok(Err(_))) {
                    self.said.say("tape.destroy");
                }
            }
            Ok(Some(about)) => {
                let tape_dir = self.tape();
                let audio = self
                    .work_tree
                    .blocking("tape.kept", move || tape::kept(&tape_dir))
                    .await
                    .unwrap_or(false);
                let sealed_row = Row {
                    id: self.id.clone(),
                    started: about.started,
                    length_ms: about.length_ms,
                    title: about.title.clone(),
                    preview: about.preview.clone(),
                    spoken: about.spoken.clone(),
                    draft: about.take == 0,
                    audio,
                };
                let _ = self.results.send(Msg::Sealed(sealed_row)).await; // dropped: the hub has ended
                let key = self.keys.from(Service::Soniox) != From::Nowhere;
                match write::owed(about.take == 0, audio, about.moved, key) {
                    write::Owed::Yes => {
                        if on_write_task {
                            self.write_job().await;
                        } else {
                            let work_tree = self.work_tree.clone();
                            work_tree.spawn("job.write", async move {
                                self.write_job().await;
                                let _ = self.results.send(Msg::Wrote(self.id.clone())).await; // dropped: the hub has ended
                            });
                            return;
                        }
                    }
                    write::Owed::Waits(why) => info!("write.skipped", n = 1usize, why = why),
                    write::Owed::No => {}
                }
            }
            Ok(None) => {} // removed meanwhile: the tape stays for a restore
            Err(_) => {}
        }
        let _ = self.results.send(Msg::Wrote(self.id.clone())).await; // dropped: the hub has ended
    }

    async fn write_job(&self) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        info!("job.start", kind = "write", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
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
                End::Retry(c) => {
                    let _ = self.progress.offer(Msg::Writing(writing(&self.id, c))); // dropped: the next one replaces it
                    Err(End::Retry(c))
                }
                end => Err(end),
            }
        })
        .await;
        let ms = started.elapsed().as_millis() as u64;
        match written {
            Ok(()) => {
                info!("job.done", kind = "write", job = seq, code = "ok", ms = ms);
                self.jobs_done.add(1);
            }
            Err(end) => {
                let c = code(end);
                warn!("job.done", kind = "write", job = seq, code = c, ms = ms);
                self.jobs_failed.add(1);
                self.said.say(c);
            }
        }
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

fn open(recording: Recording, tapes: &Path) -> Open {
    let (row, lines) = (row(&recording, tapes), recording.lines);
    Open { row, lines, speakers: recording.speakers, note: recording.note, filed: None }
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
