//! Off-hub work: the sequential `desk` queue for local data/settings/key-store IO, plus dedicated
//! supervised tasks on the `work` subtree for boot devices, search, export file writes, sign-in,
//! identity creation, invite claiming, city calendar, rooms/slots, whole-recording transcription,
//! city filing, 0.8 migration, and update installation (`P1.md` §3.6–§3.8, `P1-19`, `P1-20`).

use crate::ai::titles::{self, TitleAction, TitleInput};
use crate::ai::write;
use crate::app::core::{Config, Voice};
use crate::app::hub::Msg;
use crate::app::proto::{Change, From, LocalShelf, NoteOp, Progress, Quota, Setup, Sound, Writing};
use crate::app::recording;
use crate::capture::clock::wall_ms;
use crate::capture::tape;
use crate::id::identity::{self, PendingSignIn};
use crate::id::keys::{self, Chain, Me, Ours, SIGN_IN_WAIT_MS, Secret, Service, SignIn, Who};
use crate::id::quota;
use crate::knowledge::glossary;
use crate::knowledge::topic::{self, ALWAYS_TOPIC_ID, Topic};
use crate::live::frame::Frame;
use crate::live::lines::Line;
use crate::out::city::{self, FileGate, Filer, Filing};
use crate::out::export::{Doc, EXPORTERS};
use crate::out::host::{self, HOST_ARM_POLL_MS, HOST_KEEPALIVE_MS, HOST_SLOT_MS, Write};
use crate::record::notes;
use crate::record::shelf::{
    About, ContentTag, FILING, LINE, NOTE, RECORDING, SPEAKER, Shelf, TERM, TOPIC, TitleBy,
    TitleCandidate, Words,
};
use crate::record::takes::{
    RefineEvent, RefineFailure, RefineStatus, initial_refine_status, refine_step,
};
use crate::record::{adopt, migrate, retain};
use crate::rt::queue::{self, Full, Tx};
use crate::rt::signal::{Reading, Register};
use crate::rt::stats::Counter;
use crate::rt::supervise::{End, Tree, again};
use crate::rt::water::Water;
use crate::view::index::Index;
use crate::view::query::{ViewEngine, ViewKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
use zutalk_log::{Id, info, warn};

const WHOLE: &str = "stt-async-v6";
/// Jobs buffered for the desk worker: a burst of clicks or edits is dozens of jobs; full means
/// disk is stuck and the person must be told (`P1.md` §3.7).
pub const DESK_QUEUE_JOBS: usize = 64;
/// Heavy background jobs (whole-recording transcription, city filing, 0.8 migration) run one at a
/// time so they neither fight each other nor starve live captioning (`PLAN.md` §4.1, `P1.md` §3.8).
pub const BACKGROUND_AT_ONCE_JOBS: usize = 1;
/// Independent single-concurrency semaphore for background AI title/tag generation so it never
/// blocks recording, refining, or city filing (`P6.md` §3.4).
pub const TITLE_AT_ONCE_JOBS: usize = 1;
/// Diagnose bundles retained under `<data>/diagnose/`: each copies at most 2 log files (16 MB),
/// so 3 bundles stay under 50 MB (`P1-22-diagnose.md`).
pub const DIAGNOSE_KEPT_DIRS: usize = 3;
/// Most recent rotated `*.jsonl` log files copied into one diagnose bundle (`P1.md` §3.13.6).
pub const DIAGNOSE_LOG_FILES: usize = 2;

/// Safe summary written to `about.json` inside a diagnose bundle (`P1.md` §3.13.6).
/// Constructed field by field from numbers, booleans, and static or plain-checked codes — never
/// serialized from `State` or copied from `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Facts {
    pub version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub level: &'static str,
    pub up_ms: u64,
    pub who: &'static str,
    pub spoken: Vec<String>,
    pub shown: Vec<String>,
    pub sound: &'static str,
    pub captions: bool,
    pub keys: BTreeMap<&'static str, From>,
}

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

/// Lightweight snapshot of bounded small-state fields (`keys`, `left`, `quota`) read on `desk`.
pub(crate) struct Refreshed {
    pub rev: u64,
    pub keys: Vec<(String, From)>,
    pub left: Option<u64>,
    pub quota: Option<Quota>,
}

/// Unfinished background work surveyed on `desk` (`recover` + `resume`).
pub(crate) struct Survey {
    pub writes: Vec<String>,
    pub sweep: bool,
    pub filings: Vec<(String, Filing)>,
}

/// Complete result of switching the signing identity on `desk` (carries no keys).
pub(crate) struct SignedOut {
    pub me: Option<String>,
    pub shelf_by: Option<String>,
    pub handle: Option<String>,
    pub rotation_unsaved: bool,
    pub local_shelf: LocalShelf,
    pub refreshed: Refreshed,
    pub survey: Survey,
    pub say: Vec<&'static str>,
}

/// Complete result of booting the core on `desk` (carries no keys or device names).
pub(crate) struct BootOut {
    pub setup: Setup,
    pub signed: SignedOut,
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

/// Updated write target returned by [`IndexSync::recording_sealed`] so post-seal refinement
/// runs against the active shelf (including a newly adopted person shelf) with fresh keys.
pub(crate) struct SealTarget {
    shelf: Arc<Shelf>,
    keys: Arc<Chain>,
}

enum SyncKind {
    RecordingOpened { id: String },
    RecordingSealed { id: String, started_by: String },
    Refined { id: String, shelf_by: String },
    RefineFailed { id: String, shelf_by: String, failure: RefineFailure },
    FilingUpdated { id: String, state: String },
    Migrated { by: String },
}

/// Ordered handle allowing background tasks (recording, refine, file, migrate) to sync the
/// single-writer SQLite index through `desk` and wait until both index and invalidations commit.
#[derive(Clone)]
pub(crate) struct IndexSync {
    desk: Desk<Job>,
    views: Arc<ViewEngine>,
}

impl IndexSync {
    async fn run(&self, kind: SyncKind) {
        let (tx, rx) = oneshot::channel();
        if self.desk.send(Job::IndexSync { kind, done: tx }).await.is_ok() {
            let _ = rx.await; // dropped: desk ended on shutdown
        }
    }

    pub(crate) async fn recording_opened(&self, id: String) {
        self.views.set_active_recording(Some(id.clone()));
        self.run(SyncKind::RecordingOpened { id }).await;
    }

    pub(crate) async fn recording_sealed(
        &self,
        id: String,
        started_by: String,
    ) -> Option<SealTarget> {
        self.views.set_active_recording(None);
        let (tx, rx) = oneshot::channel();
        let kind = SyncKind::RecordingSealed { id, started_by };
        if self.desk.send(Job::IndexSync { kind, done: tx }).await.is_ok() {
            rx.await.ok().flatten()
        } else {
            None
        }
    }

    pub(crate) async fn refined(&self, id: String, shelf_by: String) {
        self.views.set_active_refine(id.clone(), None);
        self.run(SyncKind::Refined { id, shelf_by }).await;
    }

    pub(crate) async fn refine_failed(&self, id: String, shelf_by: String, failure: RefineFailure) {
        self.views.set_active_refine(id.clone(), None);
        self.run(SyncKind::RefineFailed { id, shelf_by, failure }).await;
    }

    pub(crate) async fn filing_updated(&self, id: String, state: String) {
        self.run(SyncKind::FilingUpdated { id, state }).await;
    }

    pub(crate) async fn migrated(&self, by: String) {
        self.run(SyncKind::Migrated { by }).await;
    }
}

enum Job {
    Boot,
    Settings {
        setup: Setup,
    },
    Sign {
        who: Who,
        why: &'static str,
        made: bool,
        keep: Option<u32>,
        busy: Vec<String>,
        initial_handle: Option<Option<String>>,
    },
    ConfirmSignIn {
        auth_gen: u64,
        keep: Option<u32>,
        busy: Vec<String>,
    },
    SignOut {
        keep: Option<u32>,
        busy: Vec<String>,
    },
    DiscardLocal,
    Clip,
    RotationSaved,
    Refresh {
        rev: u64,
        retain: Option<Option<u32>>,
    },
    Change {
        id: String,
        change: Change,
    },
    Note {
        id: String,
        op: NoteOp,
    },
    RefinePrepare {
        id: String,
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
    SaveTopic {
        topic: Topic,
    },
    RemoveTopic {
        id: String,
    },
    ImportTopicFile {
        id: String,
        path: String,
    },
    RememberTerm {
        topic_id: String,
        said: String,
        written: BTreeMap<String, String>,
    },
    PickTitle {
        id: String,
        title: String,
        by: TitleBy,
    },
    ApplyTitles {
        id: String,
        shelf_by: String,
        titles: Vec<String>,
        tags: Vec<ContentTag>,
        candidates: Vec<TitleCandidate>,
        elapsed_ms: u64,
        done: oneshot::Sender<()>,
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
    BeginFile {
        id: String,
        event: String,
        audio: bool,
    },
    OwnKey {
        service: Service,
        key: Option<String>,
        busy: Vec<String>,
    },
    Diagnose {
        spoken: Vec<String>,
        shown: Vec<String>,
        sound: Sound,
        captions: bool,
    },
    IndexSync {
        kind: SyncKind,
        done: oneshot::Sender<Option<SealTarget>>,
    },
}

impl DeskJob for Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::Boot => "boot",
            Job::Settings { .. } => "settings",
            Job::Sign { .. } => "sign",
            Job::ConfirmSignIn { .. } => "confirm_signin",
            Job::SignOut { .. } => "signout",
            Job::DiscardLocal => "discard_local",
            Job::Clip => "clip",
            Job::RotationSaved => "rotation_saved",
            Job::Refresh { .. } => "refresh",
            Job::Change { .. } => "change",
            Job::Note { .. } => "note",
            Job::RefinePrepare { .. } => "refine.prepare",
            Job::Remove { .. } => "remove",
            Job::Restore { .. } => "restore",
            Job::Term { .. } => "term",
            Job::SaveTopic { .. } => "topic.save",
            Job::RemoveTopic { .. } => "topic.remove",
            Job::ImportTopicFile { .. } => "topic.import",
            Job::RememberTerm { .. } => "topic.remember",
            Job::PickTitle { .. } => "title.pick",
            Job::ApplyTitles { .. } => "titles",
            Job::DestroyAudio { .. } => "destroy",
            Job::Export { .. } => "export",
            Job::BeginFile { .. } => "file.begin",
            Job::OwnKey { .. } => "own_key",
            Job::Diagnose { .. } => "diagnose",
            Job::IndexSync { .. } => "index.sync",
        }
    }
}

enum DeskOut {
    Silent,
    Copied,
    Diagnosed,
    RotationSaved,
    TopicRemembered,
    TopicRemoved(String),
    Booted {
        out: Box<BootOut>,
        start_create: bool,
        start_claim: Option<Secret>,
        recover_host: Option<String>,
    },
    Signed {
        out: SignedOut,
        start_claim: Option<Secret>,
        recover_host: Option<String>,
    },
    Refreshed(Refreshed),
    ExportWrite {
        to: PathBuf,
        text: String,
    },
    FileReady {
        id: String,
        filing: Filing,
    },
    RefineStart {
        id: String,
        from: RefineStatus,
    },
    OwnKey(OwnKeyOut),
    LocalShelf(LocalShelf),
    LocalDiscarded(LocalShelf),
}

struct DeskEnv {
    config: Arc<Config>,
    http: reqwest::Client,
    identity: Arc<Mutex<Identity>>,
    index: Mutex<Option<Index>>,
    views: Arc<ViewEngine>,
    topics: Arc<Mutex<Vec<Topic>>>,
    invite: Arc<Mutex<Option<Secret>>>,
    signing: Arc<Mutex<Option<CancellationToken>>>,
    pending_signin: Arc<Mutex<Option<PendingSignIn>>>,
    confirm_gen: Arc<AtomicU64>,
    keep_hint: Arc<Mutex<Option<u32>>>,
    root: Tree,
    water: Reading<Water>,
    started_at: std::time::Instant,
    last_diagnose_ms: AtomicU64,
}

impl DeskEnv {
    fn with_index(&self, f: impl FnOnce(&mut Index)) {
        let mut guard = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = guard.as_mut() {
            f(index);
        }
    }
}

/// In-memory active hosted room session: the 32-byte key lives only here (`Zeroizing`) and is
/// never written to disk (`PLAN.md` §5.5, `P5.md` §3.3).
struct ActiveHost {
    host_gen: u64,
    room_id: String,
    ours: Ours,
    live_flag: Arc<AtomicBool>,
    live_notify: Arc<Notify>,
    cancel: CancellationToken,
}

/// Everything the hub hands off: sequential `desk` jobs and separate supervised tasks on the
/// `work` subtree.
pub(crate) struct Work {
    config: Arc<Config>,
    http: reqwest::Client,
    results: Tx<Msg>,
    progress_reg: Register<Progress>,
    views: Arc<ViewEngine>,
    topics: Arc<Mutex<Vec<Topic>>>,
    said: Voice,
    root: Tree,
    tree: Tree,
    frame: Reading<Frame>,
    identity: Arc<Mutex<Identity>>,
    signing: Arc<Mutex<Option<CancellationToken>>>,
    pending_signin: Arc<Mutex<Option<PendingSignIn>>>,
    keep_hint: Arc<Mutex<Option<u32>>>,
    desk: Desk<Job>,
    bg: Arc<Semaphore>,
    title_bg: Arc<Semaphore>,
    titling: Arc<Mutex<BTreeSet<String>>>,
    next_seq: Arc<AtomicU64>,
    host_live: Arc<Mutex<Option<ActiveHost>>>,
    host_busy: Register<bool>,
    host_ops: Arc<AtomicUsize>,
    jobs_done: Counter,
    jobs_failed: Counter,
}

impl Work {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: Arc<Config>,
        http: reqwest::Client,
        results: Tx<Msg>,
        said: Voice,
        root: Tree,
        tree: Tree,
        views: Arc<ViewEngine>,
        progress_reg: Register<Progress>,
        frame: Reading<Frame>,
        water: Reading<Water>,
        host_busy: Register<bool>,
    ) -> Self {
        let stats = tree.stats();
        let jobs_done = stats.counter("jobs.done");
        let jobs_failed = stats.counter("jobs.failed");
        let next_seq = Arc::new(AtomicU64::new(1));
        let bg = Arc::new(Semaphore::new(BACKGROUND_AT_ONCE_JOBS));
        let title_bg = Arc::new(Semaphore::new(TITLE_AT_ONCE_JOBS));
        let titling: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
        let identity = Arc::new(Mutex::new(Identity::starting()));
        let topics: Arc<Mutex<Vec<Topic>>> = Arc::new(Mutex::new(Vec::new()));
        let invite: Arc<Mutex<Option<Secret>>> = Arc::new(Mutex::new(None));
        let signing: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
        let pending_signin: Arc<Mutex<Option<PendingSignIn>>> = Arc::new(Mutex::new(None));
        let confirm_gen = Arc::new(AtomicU64::new(0));
        let keep_hint: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let host_live: Arc<Mutex<Option<ActiveHost>>> = Arc::new(Mutex::new(None));
        let host_ops = Arc::new(AtomicUsize::new(0));

        let run_env = Arc::new(DeskEnv {
            config: config.clone(),
            http: http.clone(),
            identity: identity.clone(),
            index: Mutex::new(None),
            views: views.clone(),
            topics: topics.clone(),
            invite: invite.clone(),
            signing: signing.clone(),
            pending_signin: pending_signin.clone(),
            confirm_gen: confirm_gen.clone(),
            keep_hint: keep_hint.clone(),
            root: root.clone(),
            water,
            started_at: std::time::Instant::now(),
            last_diagnose_ms: AtomicU64::new(0),
        });

        let deliver_cfg = config.clone();
        let deliver_http = http.clone();
        let deliver_results = results.clone();
        let deliver_tree = tree.clone();
        let deliver_seq = next_seq.clone();
        let deliver_done = jobs_done.clone();
        let deliver_failed = jobs_failed.clone();
        let deliver_host_live = host_live.clone();
        let deliver_id = identity.clone();
        let deliver_invite = invite.clone();
        let deliver_signing = signing.clone();
        let deliver_confirm_gen = confirm_gen;
        let deliver_keep = keep_hint.clone();

        let desk_tx_holder: Arc<Mutex<Option<Desk<Job>>>> = Arc::new(Mutex::new(None));
        let deliver_desk = desk_tx_holder.clone();

        let desk = Desk::start(
            &tree,
            next_seq.clone(),
            move |job| run_desk_job(job, &run_env),
            move |kind, outcome| {
                let config = deliver_cfg.clone();
                let http = deliver_http.clone();
                let results = deliver_results.clone();
                let tree = deliver_tree.clone();
                let next_seq = deliver_seq.clone();
                let jobs_done = deliver_done.clone();
                let jobs_failed = deliver_failed.clone();
                let host_live = deliver_host_live.clone();
                let identity = deliver_id.clone();
                let invite = deliver_invite.clone();
                let signing = deliver_signing.clone();
                let confirm_gen = deliver_confirm_gen.clone();
                let keep_hint = deliver_keep.clone();
                let desk_holder = deliver_desk.clone();
                async move {
                    match outcome {
                        Ok(DeskOut::Silent) => {}
                        Ok(DeskOut::Copied) => {
                            let _ = results.send(Msg::Copied).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Diagnosed) => {
                            let _ = results.send(Msg::Diagnosed).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::RotationSaved) => {
                            let _ = results.send(Msg::RotationSaved).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::TopicRemembered) => {
                            let _ = results.send(Msg::TopicRemembered).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::TopicRemoved(id)) => {
                            let _ = results.send(Msg::TopicRemoved(id)).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Booted { out, start_create, start_claim, recover_host }) => {
                            let snap = snapshot(&identity);
                            if let (Some(ours), Some(stale_id)) = (snap.ours.clone(), recover_host)
                                && host_live
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .is_none()
                            {
                                spawn_host_recover(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    config.data.clone(),
                                    ours,
                                    stale_id,
                                );
                            }
                            if start_create {
                                let desk = desk_holder
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(desk) = desk {
                                    spawn_create_net(
                                        &tree,
                                        &signing,
                                        &next_seq,
                                        &jobs_done,
                                        &jobs_failed,
                                        http.clone(),
                                        config.directory.clone(),
                                        desk,
                                        keep_hint,
                                        results.clone(),
                                    );
                                }
                            } else if let Some(token) = start_claim {
                                spawn_claim_net(
                                    &tree,
                                    &signing,
                                    &invite,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    snap.keys.clone(),
                                    token,
                                    results.clone(),
                                );
                            } else if let Some(ours) = snap.ours.clone() {
                                spawn_quota_net(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    ours,
                                    snap.keys.clone(),
                                    results.clone(),
                                    false,
                                );
                            }
                            if let (Some(ours), Ok(shelf)) = (snap.ours, snap.shelf) {
                                spawn_profile_net(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    http,
                                    config.directory.clone(),
                                    ours.me.did,
                                    shelf.root().to_path_buf(),
                                    results.clone(),
                                );
                            }
                            let _ = results.send(Msg::Booted(out)).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Signed { out, start_claim, recover_host }) => {
                            let snap = snapshot(&identity);
                            if let (Some(ours), Some(stale_id)) = (snap.ours.clone(), recover_host)
                                && host_live
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .is_none()
                            {
                                spawn_host_recover(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    config.data.clone(),
                                    ours,
                                    stale_id,
                                );
                            }
                            if let Some(token) = start_claim {
                                spawn_claim_net(
                                    &tree,
                                    &signing,
                                    &invite,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    snap.keys.clone(),
                                    token,
                                    results.clone(),
                                );
                            } else if let Some(ours) = snap.ours.clone() {
                                spawn_quota_net(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    ours,
                                    snap.keys.clone(),
                                    results.clone(),
                                    false,
                                );
                            }
                            if let (Some(ours), Ok(shelf)) = (snap.ours, snap.shelf) {
                                spawn_profile_net(
                                    &tree,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    http,
                                    config.directory.clone(),
                                    ours.me.did,
                                    shelf.root().to_path_buf(),
                                    results.clone(),
                                );
                            }
                            let _ = results.send(Msg::Signed(Ok(out))).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Refreshed(refreshed)) => {
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
                        Ok(DeskOut::FileReady { id, filing }) => {
                            let _ = results.send(Msg::FilingReady(id, filing)).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::RefineStart { id, from }) => {
                            let _ = results.send(Msg::RefineReady { id, from }).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::OwnKey(out)) => {
                            let _ = results.send(Msg::OwnKeyDone(Ok(out))).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::LocalShelf(local_shelf)) => {
                            let _ = results.send(Msg::LocalShelf(local_shelf)).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::LocalDiscarded(local_shelf)) => {
                            let _ = results.send(Msg::LocalDiscarded(Ok(local_shelf))).await; // dropped: the hub has ended
                        }
                        Err(code) => {
                            let msg = match kind {
                                "refresh" => Msg::Refreshed(Err(code)),
                                "sign" | "signout" => Msg::Signed(Err(code)),
                                "confirm_signin" => Msg::SignInFailed {
                                    auth_gen: confirm_gen.load(Ordering::Acquire),
                                    code,
                                },
                                "discard_local" => Msg::LocalDiscarded(Err(code)),
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
            progress_reg,
            views,
            topics,
            said,
            root,
            tree,
            frame,
            identity,
            signing,
            pending_signin,
            keep_hint,
            desk,
            bg,
            title_bg,
            titling,
            next_seq,
            host_live,
            host_busy,
            host_ops,
            jobs_done,
            jobs_failed,
        }
    }

    fn current_topics(&self) -> Vec<Topic> {
        self.topics.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn index_sync(&self) -> IndexSync {
        IndexSync { desk: self.desk.clone(), views: self.views.clone() }
    }

    fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Kicks off boot on `desk` (settings, identity, invites) and device enumeration on a
    /// separate `devices` task so slow OS audio queries never delay boot (`P1-20`).
    pub(crate) fn boot(&self) {
        let _ = self.desk.offer(Job::Boot); // dropped: desk was just created with capacity 64
        let devices = self.config.devices.clone();
        let tree = self.tree.clone();
        let results = self.results.clone();
        self.tree.spawn("devices", async move {
            let started = std::time::Instant::now();
            let outcome = tree.blocking("devices", move || (devices)()).await;
            let ms = started.elapsed().as_millis() as u64;
            match outcome {
                Ok(list) => {
                    info!("boot.step", step = "devices", code = "ok", ms = ms, n = list.len());
                    let _ = results.send(Msg::Devices(list)).await; // dropped: the hub has ended
                }
                Err(_) => {
                    if !tree.is_cancelled() {
                        warn!("boot.step", step = "devices", code = "panic", ms = ms, n = 0usize);
                    }
                }
            }
        });
    }

    pub(crate) fn save_setup(&self, setup: Setup) -> Result<(), &'static str> {
        self.desk.offer(Job::Settings { setup }).map(|_| ())
    }

    /// Starts a sign-in relay wait on `net.signin` and returns `(url, expires_at)`.
    /// Keys stay inside `Work::pending_signin` until `confirm_sign_in` commits them.
    pub(crate) fn sign_in(&self, auth_gen: u64) -> Result<(String, i64), &'static str> {
        let sign_in = SignIn::begin(&self.config.server).map_err(code)?;
        let url = sign_in.code().to_owned();
        let expires_at = now() + SIGN_IN_WAIT_MS as i64;
        let cancel = replace_signing(&self.signing);
        clear_pending(&self.pending_signin);
        let http = self.http.clone();
        let directory = self.config.directory.clone();
        let pending_signin = self.pending_signin.clone();
        let results = self.results.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("net.signin", async move {
            info!("job.start", kind = "signin", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tokio::select! {
                () = cancel.cancelled() => None,
                res = identity::receive_sign_in(&sign_in, &http, &directory) => Some(res),
            };
            let ms = started.elapsed().as_millis() as u64;
            match outcome {
                None => {
                    info!("job.done", kind = "signin", job = seq, code = "cancelled", ms = ms);
                }
                Some(Ok(pending)) => {
                    let did = pending.did().to_owned();
                    let handle = pending.handle().map(str::to_owned);
                    let code_str = pending.code().map(str::to_owned);
                    let stored = {
                        let mut guard =
                            pending_signin.lock().unwrap_or_else(PoisonError::into_inner);
                        if cancel.is_cancelled() {
                            false
                        } else {
                            *guard = Some(pending);
                            true
                        }
                    };
                    if !stored {
                        info!("job.done", kind = "signin", job = seq, code = "cancelled", ms = ms);
                        return;
                    }
                    info!("job.done", kind = "signin", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    let msg = Msg::SignInPending { auth_gen, did, handle, code: code_str };
                    let _ = results.send(msg).await; // dropped: the hub has ended
                }
                Some(Err(end)) => {
                    let c = code(end);
                    warn!("job.done", kind = "signin", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    let _ = results.send(Msg::SignInFailed { auth_gen, code: c }).await; // dropped: the hub has ended
                }
            }
        });
        Ok((url, expires_at))
    }

    pub(crate) fn confirm_sign_in(
        &self,
        auth_gen: u64,
        keep: Option<u32>,
        busy: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::ConfirmSignIn { auth_gen, keep, busy }).map(|_| ())
    }

    pub(crate) fn cancel_sign_in(&self) {
        cancel_signing(&self.signing, &self.pending_signin);
    }

    /// Creates a new `did:zu` (with optional normalized handle) on `net.create_manual`.
    pub(crate) fn create_identity(&self, handle: Option<String>, auth_gen: u64) {
        let cancel = replace_signing(&self.signing);
        clear_pending(&self.pending_signin);
        let http = self.http.clone();
        let directory = self.config.directory.clone();
        let desk = self.desk.clone();
        let keep_hint = self.keep_hint.clone();
        let results = self.results.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("net.create_manual", async move {
            info!("job.start", kind = "create", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tokio::select! {
                () = cancel.cancelled() => None,
                res = identity::create_identity(&http, &directory, handle) => Some(res),
            };
            let ms = started.elapsed().as_millis() as u64;
            match outcome {
                None => {
                    info!("job.done", kind = "create", job = seq, code = "cancelled", ms = ms);
                }
                Some(Ok((me, resolved_handle))) => {
                    info!("job.done", kind = "create", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    let keep = *keep_hint.lock().unwrap_or_else(PoisonError::into_inner);
                    let job = Job::Sign {
                        who: Who::Person(me),
                        why: "sign_in",
                        made: false,
                        keep,
                        busy: Vec::new(),
                        initial_handle: Some(resolved_handle),
                    };
                    let _ = desk.send(job).await; // dropped: desk ended on shutdown
                }
                Some(Err(end)) => {
                    let c = code(end);
                    warn!("job.done", kind = "create", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    let _ = results.send(Msg::SignInFailed { auth_gen, code: c }).await; // dropped: the hub has ended
                }
            }
        });
    }

    /// Recovers an existing `did:zu` using its P-256 rotation key on `net.recover`.
    /// Immediately wraps the key string in `Zeroizing` so it is wiped on drop.
    pub(crate) fn recover_identity(&self, did: String, key: String, auth_gen: u64) {
        let key = Zeroizing::new(key);
        let cancel = replace_signing(&self.signing);
        clear_pending(&self.pending_signin);
        let http = self.http.clone();
        let directory = self.config.directory.clone();
        let desk = self.desk.clone();
        let keep_hint = self.keep_hint.clone();
        let results = self.results.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("net.recover", async move {
            info!("job.start", kind = "recover", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tokio::select! {
                () = cancel.cancelled() => None,
                res = identity::recover_identity(&http, &directory, &did, key) => Some(res),
            };
            let ms = started.elapsed().as_millis() as u64;
            match outcome {
                None => {
                    info!("job.done", kind = "recover", job = seq, code = "cancelled", ms = ms);
                }
                Some(Ok((me, resolved_handle))) => {
                    info!("job.done", kind = "recover", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    let keep = *keep_hint.lock().unwrap_or_else(PoisonError::into_inner);
                    let job = Job::Sign {
                        who: Who::Person(me),
                        why: "sign_in",
                        made: false,
                        keep,
                        busy: Vec::new(),
                        initial_handle: Some(resolved_handle),
                    };
                    let _ = desk.send(job).await; // dropped: desk ended on shutdown
                }
                Some(Err(end)) => {
                    let c = code(end);
                    warn!("job.done", kind = "recover", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    let _ = results.send(Msg::SignInFailed { auth_gen, code: c }).await; // dropped: the hub has ended
                }
            }
        });
    }

    /// Manually refreshes the signed-in person's daily quota from `POST /quota`.
    pub(crate) fn refresh_quota(&self) -> Result<(), &'static str> {
        let snap = snapshot(&self.identity);
        let ours = snap.ours.ok_or("sign_in.first")?;
        spawn_quota_net(
            &self.tree,
            &self.next_seq,
            &self.jobs_done,
            &self.jobs_failed,
            ours,
            snap.keys,
            self.results.clone(),
            true,
        );
        Ok(())
    }

    pub(crate) fn sign_out(
        &self,
        keep: Option<u32>,
        busy: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::SignOut { keep, busy }).map(|_| ())
    }

    pub(crate) fn discard_local(&self) -> Result<(), &'static str> {
        self.desk.offer(Job::DiscardLocal).map(|_| ())
    }

    pub(crate) fn copy_rotation(&self) -> Result<(), &'static str> {
        self.desk.offer(Job::Clip).map(|_| ())
    }

    pub(crate) fn rotation_saved(&self) -> Result<(), &'static str> {
        self.desk.offer(Job::RotationSaved).map(|_| ())
    }

    pub(crate) fn city(&self, address: String) {
        let http = self.http.clone();
        let results = self.results.clone();
        let views = self.views.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("net.calendar", async move {
            info!("job.start", kind = "calendar", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let res = city::calendar(&http, &address).await;
            let ms = started.elapsed().as_millis() as u64;
            match &res {
                Ok(calendar) => {
                    info!("job.done", kind = "calendar", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    views.set_calendar(Some(calendar.clone()));
                    views.invalidate(ViewKind::Calendar, None);
                }
                Err(end) => {
                    warn!("job.done", kind = "calendar", job = seq, code = code(*end), ms = ms);
                    jobs_failed.add(1);
                }
            }
            let _ = results.send(Msg::City(res)).await; // dropped: the hub has ended
        });
    }

    /// Runs `config.install` in a blocking call on `work`; if it returns, reports failure to hub.
    pub(crate) fn install(&self) {
        let install = self.config.install.clone();
        let tree = self.tree.clone();
        let results = self.results.clone();
        let seq = self.alloc_seq();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("update.install", async move {
            info!("job.start", kind = "install", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tree.blocking("update.install", move || (install)()).await;
            let ms = started.elapsed().as_millis() as u64;
            if outcome.is_err() && tree.is_cancelled() {
                return;
            }
            let c = if outcome.is_err() { "panic" } else { "update.install" };
            warn!("job.done", kind = "install", job = seq, code = c, ms = ms);
            jobs_failed.add(1);
            let _ = results.send(Msg::InstallFailed(c)).await; // dropped: the hub has ended
        });
    }

    pub(crate) fn refresh(
        &self,
        rev: u64,
        retain: Option<Option<u32>>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Refresh { rev, retain }).map(|_| ())
    }

    pub(crate) fn change(&self, id: String, change: Change) -> Result<(), &'static str> {
        self.desk.offer(Job::Change { id, change }).map(|_| ())
    }

    pub(crate) fn note(&self, id: String, op: NoteOp) -> Result<(), &'static str> {
        self.desk.offer(Job::Note { id, op }).map(|_| ())
    }

    pub(crate) fn refine(&self, id: String) -> Result<(), &'static str> {
        self.desk.offer(Job::RefinePrepare { id }).map(|_| ())
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

    pub(crate) fn save_topic(&self, topic: Topic) -> Result<(), &'static str> {
        self.desk.offer(Job::SaveTopic { topic }).map(|_| ())
    }

    pub(crate) fn remove_topic(&self, id: String) -> Result<(), &'static str> {
        self.desk.offer(Job::RemoveTopic { id }).map(|_| ())
    }

    pub(crate) fn import_topic_file(&self, id: String, path: String) -> Result<(), &'static str> {
        self.desk.offer(Job::ImportTopicFile { id, path }).map(|_| ())
    }

    pub(crate) fn remember_term(
        &self,
        topic_id: String,
        said: String,
        written: BTreeMap<String, String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::RememberTerm { topic_id, said, written }).map(|_| ())
    }

    pub(crate) fn pick_title(
        &self,
        id: String,
        title: String,
        by: TitleBy,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::PickTitle { id, title, by }).map(|_| ())
    }

    pub(crate) fn generate_title(&self, id: String) -> Result<(), &'static str> {
        let snap = snapshot(&self.identity);
        let shelf = snap.shelf?;
        spawn_titles_task(TitleTask {
            id,
            manual: true,
            shelf,
            keys: snap.keys,
            http: self.http.clone(),
            openrouter: self.config.openrouter.clone(),
            work_tree: self.tree.clone(),
            desk: self.desk.clone(),
            progress_reg: self.progress_reg.clone(),
            said: self.said.clone(),
            title_bg: self.title_bg.clone(),
            titling: self.titling.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
        });
        Ok(())
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

    /// Creates an ephemeral live room with a fresh random 32-byte key held only in RAM, writes
    /// only the room id to `<data>/host.lock`, calls `make`, and starts `host.slot` on success.
    pub(crate) fn host_make(&self, host_gen: u64) -> Result<(), &'static str> {
        let ours = snapshot(&self.identity).ours.ok_or("sign_in.first")?;
        let room_id = keys::fresh().map_err(code)?;
        let key = Zeroizing::new(keys::random::<32>().map_err(code)?);
        let cancel = CancellationToken::new();
        let live_flag = Arc::new(AtomicBool::new(false));
        let live_notify = Arc::new(Notify::new());
        let active = ActiveHost {
            host_gen,
            room_id: room_id.clone(),
            ours: ours.clone(),
            live_flag: live_flag.clone(),
            live_notify: live_notify.clone(),
            cancel: cancel.clone(),
        };
        {
            let mut guard = self.host_live.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(prev) = guard.replace(active) {
                prev.cancel.cancel();
            }
        }
        self.host_ops.fetch_add(1, Ordering::AcqRel);
        self.host_busy.set(true);

        let tree = self.tree.clone();
        let data = self.config.data.clone();
        let frame = self.frame.clone();
        let voice = self.said.clone();
        let results = self.results.clone();
        let host_live = self.host_live.clone();
        let host_busy = self.host_busy.clone();
        let host_ops = self.host_ops.clone();
        let next_seq = self.next_seq.clone();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        let seq = self.alloc_seq();

        self.tree.spawn("net.host_make", async move {
            info!("job.start", kind = "host.make", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let lock_data = data.clone();
            let lock_id = room_id.clone();
            let lock_res =
                tree.blocking("host.lock", move || host::write_lock(&lock_data, &lock_id)).await;
            let locked = match lock_res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(c)) => Err(c),
                Err(_) => Err("disk"),
            };
            if let Err(c) = locked {
                let ms = started.elapsed().as_millis() as u64;
                warn!("job.done", kind = "host.make", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
                clear_active_if_gen(&host_live, host_gen);
                finish_host_op(&host_ops, &host_live, &host_busy);
                let _ = results.send(Msg::HostFailed { host_gen, code: c }).await; // dropped: the hub has ended
                return;
            }

            let made = host::write(&ours, &room_id, None, Write::Make).await;
            let ms = started.elapsed().as_millis() as u64;

            if cancel.is_cancelled() {
                if made.is_ok() {
                    let gone = host::write(&ours, &room_id, None, Write::Gone).await;
                    if gone.is_ok() || matches!(gone, Err(End::Fatal("room.gone"))) {
                        let clear_data = data.clone();
                        let clear_id = room_id.clone();
                        let res = tree
                            .blocking("host.lock", move || {
                                host::clear_lock_if(&clear_data, &clear_id)
                            })
                            .await;
                        let _ = res; // dropped: best-effort lock cleanup after cancel
                    }
                } else {
                    let clear_data = data.clone();
                    let clear_id = room_id.clone();
                    let res = tree
                        .blocking("host.lock", move || host::clear_lock_if(&clear_data, &clear_id))
                        .await;
                    let _ = res; // dropped: best-effort lock cleanup after failed make
                }
                info!("job.done", kind = "host.make", job = seq, code = "cancelled", ms = ms);
                clear_active_if_gen(&host_live, host_gen);
                finish_host_op(&host_ops, &host_live, &host_busy);
                return;
            }

            match made {
                Ok(_) => {
                    let link = host::link(&ours.server, &room_id, &key);
                    spawn_host_slot(
                        &tree,
                        &next_seq,
                        &jobs_done,
                        host_gen,
                        ours,
                        room_id,
                        key,
                        live_flag,
                        live_notify,
                        cancel,
                        frame,
                        voice,
                        results.clone(),
                    );
                    info!("job.done", kind = "host.make", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    finish_host_op(&host_ops, &host_live, &host_busy);
                    let _ = results.send(Msg::HostReady { host_gen, link }).await; // dropped: the hub has ended
                }
                Err(end) => {
                    let c = code(end);
                    let clear_data = data.clone();
                    let clear_id = room_id.clone();
                    let res = tree
                        .blocking("host.lock", move || host::clear_lock_if(&clear_data, &clear_id))
                        .await;
                    let _ = res; // dropped: best-effort lock cleanup after failed make
                    warn!("job.done", kind = "host.make", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    clear_active_if_gen(&host_live, host_gen);
                    finish_host_op(&host_ops, &host_live, &host_busy);
                    let _ = results.send(Msg::HostFailed { host_gen, code: c }).await; // dropped: the hub has ended
                }
            }
        });
        Ok(())
    }

    /// Switches the active hosted room between armed polling and live caption publishing.
    pub(crate) fn host_live(&self, live: bool) {
        let guard = self.host_live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(active) = guard.as_ref() {
            active.live_flag.store(live, Ordering::Release);
            if live {
                active.live_notify.notify_one();
            }
        }
    }

    /// Cancels `host.slot`, sends `gone` using the room's original `Ours` snapshot, and clears
    /// `host.lock` if it still names this room once confirmed. Returns `true` if a room teardown
    /// task was started.
    pub(crate) fn host_stop_room(&self, stop_gen: u64) -> bool {
        let active = self.host_live.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(active) = active else {
            return false;
        };
        active.cancel.cancel();
        self.host_ops.fetch_add(1, Ordering::AcqRel);
        self.host_busy.set(true);

        let tree = self.tree.clone();
        let data = self.config.data.clone();
        let results = self.results.clone();
        let host_live = self.host_live.clone();
        let host_busy = self.host_busy.clone();
        let host_ops = self.host_ops.clone();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        let seq = self.alloc_seq();

        self.tree.spawn("net.host_gone", async move {
            info!("job.start", kind = "host.gone", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let gone = host::write(&active.ours, &active.room_id, None, Write::Gone).await;
            let ms = started.elapsed().as_millis() as u64;
            match gone {
                Ok(_) | Err(End::Fatal("room.gone")) => {
                    let clear_data = data.clone();
                    let clear_id = active.room_id.clone();
                    let res = tree
                        .blocking("host.lock", move || host::clear_lock_if(&clear_data, &clear_id))
                        .await;
                    let _ = res; // dropped: lock cleanup after confirmed gone
                    info!("job.done", kind = "host.gone", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                }
                Err(end) => {
                    let c = code(end);
                    warn!("job.done", kind = "host.gone", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                }
            }
            finish_host_op(&host_ops, &host_live, &host_busy);
            let _ = results.send(Msg::HostStopped { host_gen: stop_gen }).await; // dropped: the hub has ended
        });
        true
    }

    pub(crate) fn begin_file(
        &self,
        id: String,
        event: String,
        audio: bool,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::BeginFile { id, event, audio }).map(|_| ())
    }

    pub(crate) fn own_key(
        &self,
        service: Service,
        key: Option<String>,
        busy: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::OwnKey { service, key, busy }).map(|_| ())
    }

    pub(crate) fn diagnose(
        &self,
        spoken: Vec<String>,
        shown: Vec<String>,
        sound: Sound,
        captions: bool,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Diagnose { spoken, shown, sound, captions }).map(|_| ())
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
        let topics = self.current_topics();
        let context = topic::assemble_context(&topics, &about.topics, &about.title);
        let index_sync = self.index_sync();
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
            context,
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
            index_sync: index_sync.clone(),
        };
        let post = WriteCtx {
            id,
            started_by: shelf.by().to_owned(),
            shelf,
            keys,
            config: self.config.clone(),
            results: self.results.clone(),
            progress_reg: self.progress_reg.clone(),
            views: self.views.clone(),
            index_sync,
            said: self.said.clone(),
            work_tree: self.tree.clone(),
            root: self.root.clone(),
            bg: self.bg.clone(),
            http: self.http.clone(),
            desk: self.desk.clone(),
            title_bg: self.title_bg.clone(),
            titling: self.titling.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
            manual: false,
            from_state: RefineStatus::Draft { why: None },
        };
        self.root.spawn("recording", async move {
            let done = recording::run(rec_start).await;
            post.after_seal(done.sealed).await;
        });
        Ok(())
    }

    /// Sweeps any orphaned remote files and starts `job.write` tasks for owed drafts.
    pub(crate) fn write_recovered(&self, ids: Vec<String>) {
        let snap = snapshot(&self.identity);
        let Ok(shelf) = snap.shelf else { return };
        let keys = snap.keys;
        let tapes = self.config.tapes();
        let soniox = self.config.soniox.clone();
        let said = self.said.clone();
        let work_tree = self.tree.clone();
        let root = self.root.clone();
        let bg = self.bg.clone();
        let http = self.http.clone();
        let desk = self.desk.clone();
        let title_bg = self.title_bg.clone();
        let titling = self.titling.clone();
        let config = self.config.clone();
        let results = self.results.clone();
        let progress_reg = self.progress_reg.clone();
        let views = self.views.clone();
        let index_sync = self.index_sync();
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
                    started_by: shelf.by().to_owned(),
                    shelf: shelf.clone(),
                    keys: keys.clone(),
                    config: config.clone(),
                    results: results.clone(),
                    progress_reg: progress_reg.clone(),
                    views: views.clone(),
                    index_sync: index_sync.clone(),
                    said: said.clone(),
                    work_tree: work_tree.clone(),
                    root: root.clone(),
                    bg: bg.clone(),
                    http: http.clone(),
                    desk: desk.clone(),
                    title_bg: title_bg.clone(),
                    titling: titling.clone(),
                    next_seq: next_seq.clone(),
                    jobs_done: jobs_done.clone(),
                    jobs_failed: jobs_failed.clone(),
                    manual: false,
                    from_state: RefineStatus::Draft { why: None },
                };
                work_tree.spawn("job.write", async move {
                    ctx.finish_recovered().await;
                });
            }
        });
    }

    /// Starts a manual whole-recording refinement on the `work` subtree (`Command::Refine`).
    pub(crate) fn write_manual(&self, id: String, from: RefineStatus) {
        let snap = snapshot(&self.identity);
        let Ok(shelf) = snap.shelf else {
            let results = self.results.clone();
            self.tree.spawn("job.write", async move {
                let _ = results.send(Msg::Wrote(id)).await; // dropped: the hub has ended
            });
            return;
        };
        let ctx = WriteCtx {
            id,
            started_by: shelf.by().to_owned(),
            shelf,
            keys: snap.keys,
            config: self.config.clone(),
            results: self.results.clone(),
            progress_reg: self.progress_reg.clone(),
            views: self.views.clone(),
            index_sync: self.index_sync(),
            said: self.said.clone(),
            work_tree: self.tree.clone(),
            root: self.root.clone(),
            bg: self.bg.clone(),
            http: self.http.clone(),
            desk: self.desk.clone(),
            title_bg: self.title_bg.clone(),
            titling: self.titling.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
            manual: true,
            from_state: from,
        };
        self.tree.spawn("job.write", async move {
            ctx.write_job().await;
            let _ = ctx.results.send(Msg::Wrote(ctx.id.clone())).await; // dropped: the hub has ended
        });
    }

    /// Files one recording to a city on the `work` subtree (`job.file`).
    pub(crate) fn file(&self, id: String, filing: Filing) -> Result<(), &'static str> {
        let snap = snapshot(&self.identity);
        let me = snap.ours.ok_or("sign_in.first")?.me;
        let shelf = snap.shelf?;
        let filer = Filer { http: self.http.clone(), me, shelf, tapes: self.config.tapes() };
        let results = self.results.clone();
        let progress_reg = self.progress_reg.clone();
        let index_sync = self.index_sync();
        let tree = self.tree.clone();
        let bg = self.bg.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.file", async move {
            set_file_progress(&progress_reg, Some(writing(&id, "filing")), None);
            let gate = FileGate { bg: &bg, seq, jobs_done: &jobs_done, jobs_failed: &jobs_failed };
            let file_id = id.clone();
            let said = city::file_job(&tree, &filer, &id, filing, gate, |s| {
                let w = Writing { id: file_id.clone(), step: s.step, done: s.done, of: s.of };
                set_file_progress(&progress_reg, Some(w), None);
            })
            .await;
            index_sync.filing_updated(id.clone(), said.into()).await;
            set_file_progress(&progress_reg, None, Some(&id));
            let _ = results.send(Msg::Filed(id, said)).await; // dropped: the hub has ended
        });
        Ok(())
    }

    /// Moves the 0.8 library into the signed-in person's shelf on the `work` subtree (`job.move`).
    pub(crate) fn migrate(&self, me: String) -> bool {
        let Some(old) = self.config.old.clone() else { return false };
        let Ok(shelf) = snapshot(&self.identity).shelf else { return false };
        if shelf.by() != me {
            return false;
        }
        let data = self.config.data.clone();
        let helper = self.config.handover_helper.clone();
        let results = self.results.clone();
        let progress_reg = self.progress_reg.clone();
        let index_sync = self.index_sync();
        let tree = self.tree.clone();
        let bg = self.bg.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.move", async move {
            let wait_at = tokio::time::Instant::now();
            let Ok(_permit) = bg.acquire().await else {
                return;
            };
            let waited_ms = wait_at.elapsed().as_millis() as u64;
            info!("job.start", kind = "move", job = seq, waited_ms = waited_ms);
            let started = std::time::Instant::now();
            let reg_cb = progress_reg.clone();
            let me_run = me.clone();
            let outcome = tree
                .blocking("job.move", move || {
                    let progress = |p: migrate::Progress| {
                        let writing =
                            Writing { id: p.id, step: p.step.into(), done: p.done, of: p.of };
                        set_move_progress(&reg_cb, Some(writing));
                    };
                    migrate::run_report_with_helper(
                        (&old, &data),
                        helper.as_deref(),
                        &shelf,
                        &me_run,
                        now,
                        progress,
                    )
                })
                .await;
            let ms = started.elapsed().as_millis() as u64;
            let moved = match outcome {
                Ok(Ok(report)) => {
                    let skipped_n = report.skipped.len() as u32;
                    info!("job.done", kind = "move", job = seq, code = "ok", ms = ms);
                    if skipped_n == 0 {
                        info!(
                            "migrate.done",
                            moved = report.moved,
                            already = report.already,
                            skipped = skipped_n,
                            code = "ok",
                            ms = ms,
                        );
                    } else {
                        warn!(
                            "migrate.done",
                            moved = report.moved,
                            already = report.already,
                            skipped = skipped_n,
                            code = "ok",
                            ms = ms,
                        );
                    }
                    jobs_done.add(1);
                    Ok(report.skipped)
                }
                Ok(Err(c)) => {
                    warn!("job.done", kind = "move", job = seq, code = c, ms = ms);
                    warn!(
                        "migrate.done",
                        moved = 0u32,
                        already = 0u32,
                        skipped = 0u32,
                        code = c,
                        ms = ms,
                    );
                    jobs_failed.add(1);
                    Err(c)
                }
                Err(_) => {
                    if tree.is_cancelled() {
                        set_move_progress(&progress_reg, None);
                        return;
                    }
                    warn!("job.done", kind = "move", job = seq, code = "panic", ms = ms);
                    warn!(
                        "migrate.done",
                        moved = 0u32,
                        already = 0u32,
                        skipped = 0u32,
                        code = "panic",
                        ms = ms,
                    );
                    jobs_failed.add(1);
                    Err("panic")
                }
            };
            if moved.is_ok() {
                index_sync.migrated(me.clone()).await;
            }
            set_move_progress(&progress_reg, None);
            let _ = results.send(Msg::Moved(me, moved)).await; // dropped: the hub has ended
        });
        true
    }
}

fn replace_signing(cell: &Mutex<Option<CancellationToken>>) -> CancellationToken {
    let next = CancellationToken::new();
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = guard.replace(next.clone()) {
        old.cancel();
    }
    next
}

fn clear_pending(cell: &Mutex<Option<PendingSignIn>>) {
    let _ = cell.lock().unwrap_or_else(PoisonError::into_inner).take(); // dropped: zeroize abandoned pending sign-in
}

fn cancel_signing(cell: &Mutex<Option<CancellationToken>>, pending: &Mutex<Option<PendingSignIn>>) {
    {
        let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = guard.take() {
            old.cancel();
        }
    }
    clear_pending(pending);
}

fn snapshot(identity: &Mutex<Identity>) -> Snapshot {
    let id = identity.lock().unwrap_or_else(PoisonError::into_inner);
    Snapshot { shelf: id.shelf(), ours: id.ours.clone(), keys: id.keys.clone() }
}

fn index_path(data: &Path, shelf_by: &str) -> PathBuf {
    data.join("index").join(format!("{}.sqlite3", shelf_by.replace(':', "-")))
}

fn set_refine_progress(reg: &Register<Progress>, value: Option<Writing>, only_id: Option<&str>) {
    reg.change(|cur| {
        let next = match (&value, only_id) {
            (Some(w), _) => Some(w.clone()),
            (None, Some(target)) => {
                if cur.refine.as_ref().is_some_and(|w| w.id == target) {
                    None
                } else {
                    return None;
                }
            }
            (None, None) => None,
        };
        if cur.refine == next {
            return None;
        }
        Some(Progress { refine: next, ..cur.clone() })
    });
}

fn set_file_progress(reg: &Register<Progress>, value: Option<Writing>, only_id: Option<&str>) {
    reg.change(|cur| {
        let next = match (&value, only_id) {
            (Some(w), _) => Some(w.clone()),
            (None, Some(target)) => {
                if cur.file.as_ref().is_some_and(|w| w.id == target) {
                    None
                } else {
                    return None;
                }
            }
            (None, None) => None,
        };
        if cur.file == next {
            return None;
        }
        Some(Progress { file: next, ..cur.clone() })
    });
}

fn set_move_progress(reg: &Register<Progress>, value: Option<Writing>) {
    reg.change(|cur| {
        if cur.move_old == value {
            return None;
        }
        Some(Progress { move_old: value.clone(), ..cur.clone() })
    });
}

fn set_title_progress(reg: &Register<Progress>, value: Option<Writing>, only_id: Option<&str>) {
    reg.change(|cur| {
        let next = match (&value, only_id) {
            (Some(w), _) => Some(w.clone()),
            (None, Some(target)) => {
                if cur.title.as_ref().is_some_and(|w| w.id == target) {
                    None
                } else {
                    return None;
                }
            }
            (None, None) => None,
        };
        if cur.title == next {
            return None;
        }
        Some(Progress { title: next, ..cur.clone() })
    });
}

fn run_desk_job(job: Job, env: &DeskEnv) -> Result<DeskOut, &'static str> {
    let config = &env.config;
    let identity = &env.identity;
    match job {
        Job::Boot => run_boot(env),
        Job::Settings { setup } => {
            *env.keep_hint.lock().unwrap_or_else(PoisonError::into_inner) = setup.keep;
            let json = serde_json::to_vec(&setup).map_err(|_| "settings")?;
            fs::create_dir_all(&config.data)
                .and_then(|()| fs::write(config.data.join("settings.json"), json))
                .map_err(|_| "settings")?;
            Ok(DeskOut::Silent)
        }
        Job::Sign { who, why, made, keep, busy, initial_handle } => {
            let has_person = who.person().is_some();
            let out = run_sign(env, who, why, made, keep, &busy, initial_handle)?;
            let start_claim = if has_person {
                env.invite.lock().unwrap_or_else(PoisonError::into_inner).clone()
            } else {
                None
            };
            let recover_host = if has_person { host::read_lock(&config.data) } else { None };
            Ok(DeskOut::Signed { out, start_claim, recover_host })
        }
        Job::ConfirmSignIn { auth_gen, keep, busy } => {
            env.confirm_gen.store(auth_gen, Ordering::Release);
            let pending = env
                .pending_signin
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .ok_or("id.no_pending")?;
            let (me, handle) = pending.commit().map_err(code)?;
            let out = run_sign(env, Who::Person(me), "sign_in", false, keep, &busy, Some(handle))?;
            let start_claim = env.invite.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let recover_host = host::read_lock(&config.data);
            Ok(DeskOut::Signed { out, start_claim, recover_host })
        }
        Job::SignOut { keep, busy } => {
            cancel_signing(&env.signing, &env.pending_signin);
            keys::set("identity", None).map_err(code)?;
            let local = keys::local().map(|(me, _)| me);
            let out = run_sign(env, Who::of(None, local), "sign_out", false, keep, &busy, None)?;
            Ok(DeskOut::Signed { out, start_claim: None, recover_host: None })
        }
        Job::DiscardLocal => {
            let person_did = {
                let guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            }
            .ok_or("sign_in.first")?;
            let local_me = keys::local_kept().map_err(code)?;
            let local_shelf = if let Some(local_me) = local_me {
                let shelf = Shelf::new(
                    config.data.join("records").join(local_me.did.replace(':', "-")),
                    &local_me.did,
                    local_me.key,
                );
                let has_local_data = if shelf.root().is_dir() {
                    let has_recs = match shelf.list() {
                        Ok(list) => !list.is_empty(),
                        Err(_) => {
                            warn!("identity.discard_local", code = "adopt.failed");
                            return Err("adopt.failed");
                        }
                    };
                    let has_terms = match shelf.kept::<serde_json::Value>(TERM) {
                        Ok(terms) => !terms.is_empty(),
                        Err(_) => {
                            warn!("identity.discard_local", code = "adopt.failed");
                            return Err("adopt.failed");
                        }
                    };
                    let has_topics = match shelf.kept::<serde_json::Value>(TOPIC) {
                        Ok(topics) => !topics.is_empty(),
                        Err(_) => {
                            warn!("identity.discard_local", code = "adopt.failed");
                            return Err("adopt.failed");
                        }
                    };
                    has_recs || has_terms || has_topics
                } else {
                    false
                };
                let marker = match adopt::read_marker(&config.data) {
                    Ok(marker) => marker,
                    Err(c) => {
                        warn!("identity.discard_local", code = c);
                        return Err(c);
                    }
                };
                if !adopt::can_discard(marker.as_ref(), &local_me.did, &person_did, has_local_data)
                {
                    warn!("identity.discard_local", code = "adopt.failed");
                    return Err("adopt.failed");
                }
                if let Err(c) = adopt::discard_local(&config.data, &local_me.did) {
                    warn!("identity.discard_local", code = c);
                    return Err(c);
                }
                info!("identity.discard_local", code = "ok");
                adopt::status(&config.data, Some(&shelf), Some(&person_did))
            } else {
                info!("identity.discard_local", code = "ok");
                LocalShelf::default()
            };
            Ok(DeskOut::LocalDiscarded(local_shelf))
        }
        Job::Clip => {
            let person_did = {
                let guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            }
            .ok_or("rotation.none")?;
            let key = keys::rotation_for(&person_did).map_err(code)?.ok_or("rotation.none")?;
            if !(config.clip)(&key) {
                return Err("clip");
            }
            Ok(DeskOut::Copied)
        }
        Job::RotationSaved => {
            let person_did = {
                let guard = identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            };
            if let Some(did) = person_did {
                keys::saved_for(&did).map_err(code)?;
            }
            Ok(DeskOut::RotationSaved)
        }
        Job::Refresh { rev, retain } => {
            let snap = snapshot(identity);
            run_refresh(env, &snap, rev, retain).map(DeskOut::Refreshed)
        }
        Job::Change { id, change } => {
            let shelf = snapshot(identity).shelf?;
            let is_title = matches!(change, Change::Title { .. });
            let is_line = matches!(change, Change::Line { .. });
            let is_note = matches!(change, Change::Note { .. });
            apply(&shelf, &id, change)?;
            let tapes = config.tapes();
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
            if is_title {
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
            } else if is_line {
                env.views.invalidate(ViewKind::Transcript, Some(id));
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Library, None);
            } else if is_note {
                env.views.invalidate(ViewKind::Search, None);
            }
            Ok(DeskOut::Silent)
        }
        Job::Note { id, op } => {
            let shelf = snapshot(identity).shelf?;
            if shelf.about(&id).map_err(fail)?.is_none() {
                return Err("recording.gone");
            }
            match op {
                NoteOp::Add { at_ms, text, anchor } => {
                    let rand = getrandom::u32().map_err(|_| "random")?;
                    notes::add(&shelf, &id, at_ms, text, anchor, rand, now()).map_err(fail)?;
                }
                NoteOp::Edit { key, text } => {
                    notes::edit(&shelf, &id, &key, text, now())?;
                }
                NoteOp::Remove { key } => {
                    notes::remove(&shelf, &id, &key, now())?;
                }
            }
            let tapes = config.tapes();
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Recording, Some(id));
            env.views.invalidate(ViewKind::Search, None);
            Ok(DeskOut::Silent)
        }
        Job::RefinePrepare { id } => {
            let snap = snapshot(identity);
            let shelf = snap.shelf?;
            let rec = shelf.about(&id).map_err(fail)?.ok_or("recording.gone")?;
            let tape_dir = config.tapes().join(&id);
            let audio = tape::kept(&tape_dir);
            if !audio {
                return Err("tape.none");
            }
            let has_key = snap.keys.from(Service::Soniox) != From::Nowhere;
            if !has_key {
                return Err("keys.none");
            }
            let journal = write::Journal::load(&tape_dir).unwrap_or_default();
            let from =
                initial_refine_status(rec.about.take, audio, has_key, journal.failed.as_ref());
            let _ = write::clear_failure(&tape_dir); // dropped: write_job also clears failure before signing
            env.with_index(|idx| {
                let _ = idx.update_refine_error(&id, None); // dropped: index.sync logged error
            });
            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
            Ok(DeskOut::RefineStart { id, from })
        }
        Job::Remove { id } => {
            let shelf = snapshot(identity).shelf?;
            shelf.put::<About>(&id, RECORDING, &[(id.clone(), None)], now()).map_err(fail)?;
            env.with_index(|idx| {
                let _ = idx.remove_recording(&id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Search, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::Restore { id } => {
            let shelf = snapshot(identity).shelf?;
            shelf.restore(&id, now()).map_err(fail)?;
            let tapes = config.tapes();
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Search, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::Term { said, written } => {
            let shelf = snapshot(identity).shelf?;
            topic::put_always_term(&shelf, &said, written.clone(), now())?;
            glossary::put(&shelf, &said, written, now())?;
            refresh_topic_caches(env, &shelf);
            env.views.invalidate(ViewKind::Glossary, None);
            env.views.invalidate(ViewKind::Knowledge, None);
            Ok(DeskOut::Silent)
        }
        Job::SaveTopic { topic } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = topic.id.trim() == ALWAYS_TOPIC_ID;
            match topic::save(&shelf, topic, now()) {
                Ok(saved) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    if saved.pinned {
                        env.views.invalidate(ViewKind::Glossary, None);
                    }
                    info!(
                        "topic.saved",
                        op = "save",
                        pinned = saved.pinned,
                        terms = saved.terms.len(),
                        code = "ok"
                    );
                    Ok(DeskOut::Silent)
                }
                Err(c) => {
                    warn!("topic.saved", op = "save", pinned = is_pinned, terms = 0usize, code = c);
                    Err(c)
                }
            }
        }
        Job::RemoveTopic { id } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = id == ALWAYS_TOPIC_ID;
            match topic::remove(&shelf, &id, now()) {
                Ok(()) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    info!(
                        "topic.saved",
                        op = "remove",
                        pinned = false,
                        terms = 0usize,
                        code = "ok"
                    );
                    Ok(DeskOut::TopicRemoved(id))
                }
                Err(c) => {
                    warn!(
                        "topic.saved",
                        op = "remove",
                        pinned = is_pinned,
                        terms = 0usize,
                        code = c
                    );
                    Err(c)
                }
            }
        }
        Job::ImportTopicFile { id, path } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = id.trim().is_empty() || id.trim() == ALWAYS_TOPIC_ID;
            match topic::import_file(&shelf, &id, Path::new(&path), now()) {
                Ok(saved) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    if saved.pinned {
                        env.views.invalidate(ViewKind::Glossary, None);
                    }
                    info!(
                        "topic.saved",
                        op = "import",
                        pinned = saved.pinned,
                        terms = saved.terms.len(),
                        code = "ok"
                    );
                    Ok(DeskOut::Silent)
                }
                Err(c) => {
                    warn!(
                        "topic.saved",
                        op = "import",
                        pinned = is_pinned,
                        terms = 0usize,
                        code = c
                    );
                    Err(c)
                }
            }
        }
        Job::RememberTerm { topic_id, said, written } => {
            let shelf = snapshot(identity).shelf?;
            let is_pinned = topic_id.trim().is_empty() || topic_id.trim() == ALWAYS_TOPIC_ID;
            match topic::remember_term(&shelf, &topic_id, &said, written, now()) {
                Ok(saved) => {
                    refresh_topic_caches(env, &shelf);
                    env.views.invalidate(ViewKind::Knowledge, None);
                    if saved.pinned {
                        env.views.invalidate(ViewKind::Glossary, None);
                    }
                    info!(
                        "topic.saved",
                        op = "remember",
                        pinned = saved.pinned,
                        terms = saved.terms.len(),
                        code = "ok"
                    );
                    Ok(DeskOut::TopicRemembered)
                }
                Err(c) => {
                    warn!(
                        "topic.saved",
                        op = "remember",
                        pinned = is_pinned,
                        terms = 0usize,
                        code = c
                    );
                    Err(c)
                }
            }
        }
        Job::PickTitle { id, title, by } => {
            let shelf = snapshot(identity).shelf?;
            let about = shelf.about(&id).map_err(fail)?.ok_or("recording.gone")?.about;
            let trimmed = title.trim().to_owned();
            let title_by = if trimmed.is_empty() { TitleBy::None } else { by };
            shelf
                .put(
                    &id,
                    RECORDING,
                    &[(id.clone(), Some(About { title: trimmed, title_by, ..about }))],
                    now(),
                )
                .map_err(fail)?;
            let tapes = config.tapes();
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Search, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
        }
        Job::ApplyTitles { id, shelf_by, titles, tags, candidates, elapsed_ms, done } => {
            let commit_res = (|| -> Result<(), &'static str> {
                let snap = snapshot(identity);
                let Ok(shelf) = snap.shelf else {
                    info!(
                        "titles.done",
                        rec = Id(&id),
                        code = "gone",
                        applied = false,
                        tags = 0usize,
                        ms = elapsed_ms
                    );
                    return Ok(());
                };
                if shelf.by() != shelf_by {
                    info!(
                        "titles.done",
                        rec = Id(&id),
                        code = "gone",
                        applied = false,
                        tags = 0usize,
                        ms = elapsed_ms
                    );
                    return Ok(());
                }
                let Some(rec) = shelf.about(&id).map_err(fail)? else {
                    info!(
                        "titles.done",
                        rec = Id(&id),
                        code = "gone",
                        applied = false,
                        tags = 0usize,
                        ms = elapsed_ms
                    );
                    return Ok(());
                };
                let mut next = rec.about;
                let hand_protected = next.title_by == TitleBy::Hand
                    || (matches!(next.title_by, TitleBy::File | TitleBy::Event)
                        && !next.title.trim().is_empty());
                let mut applied = false;
                if !hand_protected && let Some(first) = titles.first() {
                    next.title = first.clone();
                    next.title_by = TitleBy::Ai;
                    applied = true;
                }
                if !tags.is_empty() {
                    next.tags = tags.clone();
                }
                if !candidates.is_empty() {
                    next.title_candidates = candidates;
                }
                shelf.put(&id, RECORDING, &[(id.clone(), Some(next))], now()).map_err(fail)?;
                let tapes = config.tapes();
                env.with_index(|idx| {
                    let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error; next boot reconciles
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                info!(
                    "titles.done",
                    rec = Id(&id),
                    code = "ok",
                    applied = applied,
                    tags = tags.len(),
                    ms = elapsed_ms
                );
                Ok(())
            })();
            let _ = done.send(()); // dropped: background titling caller was cancelled
            commit_res?;
            Ok(DeskOut::Silent)
        }
        Job::DestroyAudio { id, under_way } => {
            let shelf = snapshot(identity).shelf?;
            let tapes = config.tapes();
            let entries: Vec<retain::Entry> = shelf
                .list()
                .map_err(fail)?
                .iter()
                .map(|r| retain::Entry {
                    id: r.id.clone(),
                    started: r.about.started,
                    draft: r.about.take == 0,
                    audio: tape::kept(&tapes.join(&r.id)),
                })
                .collect();
            retain::destroy(&tapes, &entries, under_way.as_deref(), &id)?;
            env.with_index(|idx| {
                let _ = idx.update_audio(&id, false); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            Ok(DeskOut::Silent)
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
        Job::BeginFile { id, event, audio } => {
            let calendar = env.views.calendar().ok_or("city.none")?;
            let shelf = snapshot(identity).shelf?;
            let filing = city::begin(&shelf, &id, &calendar, &event, audio)?;
            env.with_index(|idx| {
                let _ = idx.update_filing(&id, ""); // dropped: index.sync logged error; next boot reconciles
            });
            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
            env.views.invalidate(ViewKind::Outbound, None);
            Ok(DeskOut::FileReady { id, filing })
        }
        Job::OwnKey { service, key, busy } => {
            let snap = snapshot(identity);
            let had_own = key.is_some();
            snap.keys.own(service, key.as_deref()).map_err(code)?;
            let keys_from: Vec<(String, From)> =
                Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
            let left = snap.keys.left();
            let has_soniox = snap.keys.from(Service::Soniox) != From::Nowhere;
            env.views.set_has_soniox_key(has_soniox);
            let mut cleared_any = false;
            if service == Service::Soniox
                && had_own
                && let Ok(shelf) = &snap.shelf
            {
                let tapes = config.tapes();
                if let Ok(recs) = shelf.list() {
                    for rec in recs {
                        if rec.about.take != 0 {
                            continue;
                        }
                        let dir = tapes.join(&rec.id);
                        if let Ok(journal) = write::Journal::load(&dir)
                            && journal.failed.as_ref().is_some_and(|f| {
                                f.code == "keys.async_refused" || f.code == "keys.none"
                            })
                        {
                            let _ = write::clear_failure(&dir); // dropped: best-effort retry unlock after entering own key
                            env.with_index(|idx| {
                                let _ = idx.update_refine_error(&rec.id, None); // dropped: index.sync logged error
                            });
                            cleared_any = true;
                        }
                    }
                }
            }
            env.views.invalidate(ViewKind::Recording, None);
            if cleared_any {
                env.views.invalidate(ViewKind::Library, None);
            }
            let survey = if service == Service::Soniox && has_soniox {
                Some(run_survey(config, &snap, &busy, false)?)
            } else {
                None
            };
            Ok(DeskOut::OwnKey(OwnKeyOut { keys: keys_from, left, survey }))
        }
        Job::Diagnose { spoken, shown, sound, captions } => {
            run_diagnose(env, spoken, shown, sound, captions)
        }
        Job::IndexSync { kind, done } => {
            let (local_shelf, target) = run_index_sync(env, kind);
            let _ = done.send(target); // dropped: background caller was cancelled
            Ok(local_shelf.map_or(DeskOut::Silent, DeskOut::LocalShelf))
        }
    }
}

fn run_index_sync(env: &DeskEnv, kind: SyncKind) -> (Option<LocalShelf>, Option<SealTarget>) {
    let snap = snapshot(&env.identity);
    let Ok(shelf) = snap.shelf else { return (None, None) };
    let tapes = env.config.tapes();
    match kind {
        SyncKind::RecordingOpened { id } => {
            env.with_index(|idx| {
                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            (None, None)
        }
        SyncKind::RecordingSealed { id, started_by } => {
            let person_did = {
                let guard = env.identity.lock().unwrap_or_else(PoisonError::into_inner);
                guard.who.person().map(|me| me.did.clone())
            };
            if started_by.starts_with("did:key:")
                && let Some(person_did) = person_did
            {
                if let Ok(Some(local_me)) = keys::local_kept()
                    && local_me.did == started_by
                {
                    let local_shelf = Shelf::new(
                        env.config.data.join("records").join(local_me.did.replace(':', "-")),
                        &local_me.did,
                        local_me.key,
                    );
                    match adopt::run_one(&env.config.data, &local_shelf, &shelf, &id, now()) {
                        Ok(sum) if sum.recordings > 0 => {
                            info!(
                                "identity.adopt",
                                recordings = sum.recordings,
                                terms = sum.terms,
                                code = "ok"
                            );
                            env.with_index(|idx| {
                                let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
                            });
                            env.views.invalidate(ViewKind::Library, None);
                            env.views.invalidate(ViewKind::Search, None);
                            env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                            env.views.invalidate(ViewKind::Transcript, Some(id));
                            let local_status = adopt::status(
                                &env.config.data,
                                Some(&local_shelf),
                                Some(&person_did),
                            );
                            let target = SealTarget { shelf, keys: snap.keys };
                            return (Some(local_status), Some(target));
                        }
                        Ok(_) => {
                            let local_status = adopt::status(
                                &env.config.data,
                                Some(&local_shelf),
                                Some(&person_did),
                            );
                            return (Some(local_status), None);
                        }
                        Err(c) => {
                            warn!("identity.adopt", recordings = 0u32, terms = 0u32, code = c);
                        }
                    }
                }
                return (None, None);
            }
            if shelf.by() == started_by {
                env.with_index(|idx| {
                    let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                env.views.invalidate(ViewKind::Transcript, Some(id));
                return (None, Some(SealTarget { shelf, keys: snap.keys }));
            }
            (None, None)
        }
        SyncKind::Refined { id, shelf_by } => {
            if shelf.by() == shelf_by {
                env.with_index(|idx| {
                    let _ = idx.upsert_recording(&shelf, &tapes, &id); // dropped: index.sync logged error
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, Some(id.clone()));
                env.views.invalidate(ViewKind::Transcript, Some(id));
            }
            (None, None)
        }
        SyncKind::RefineFailed { id, shelf_by, failure } => {
            if shelf.by() == shelf_by {
                env.with_index(|idx| {
                    let _ = idx.update_refine_error(&id, Some(&failure)); // dropped: index.sync logged error
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Recording, Some(id));
            }
            (None, None)
        }
        SyncKind::FilingUpdated { id, state } => {
            env.with_index(|idx| {
                let _ = idx.update_filing(&id, &state); // dropped: index.sync logged error
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, Some(id));
            env.views.invalidate(ViewKind::Outbound, None);
            (None, None)
        }
        SyncKind::Migrated { by } => {
            if shelf.by() == by {
                env.with_index(|idx| {
                    let _ = idx.reconcile(&shelf, &tapes); // dropped: next boot reconciles if sqlite failed
                });
                env.views.invalidate(ViewKind::Library, None);
                env.views.invalidate(ViewKind::Search, None);
                env.views.invalidate(ViewKind::Recording, None);
                env.views.invalidate(ViewKind::Transcript, None);
            }
            (None, None)
        }
    }
}

fn plain_code(text: &str) -> String {
    if zutalk_log::is_plain(text) { text.to_owned() } else { "!".to_owned() }
}

fn sound_code(sound: Sound) -> &'static str {
    match sound {
        Sound::Mic => "mic",
        Sound::System => "system",
        Sound::Both => "both",
    }
}

fn run_diagnose(
    env: &DeskEnv,
    spoken: Vec<String>,
    shown: Vec<String>,
    sound: Sound,
    captions: bool,
) -> Result<DeskOut, &'static str> {
    let (who, keys) = {
        let id = env.identity.lock().unwrap_or_else(PoisonError::into_inner);
        let keys: BTreeMap<&'static str, From> =
            Service::ALL.iter().map(|s| (s.name(), id.keys.from(*s))).collect();
        (id.who.code(), keys)
    };
    let facts = Facts {
        version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        level: env.config.log.code(),
        up_ms: env.started_at.elapsed().as_millis() as u64,
        who,
        spoken: spoken.iter().map(|s| plain_code(s)).collect(),
        shown: shown.iter().map(|s| plain_code(s)).collect(),
        sound: sound_code(sound),
        captions,
        keys,
    };
    let water = env.water.now().1;
    let stats = env.root.stats().tally();
    let tasks = env.root.running();

    let _ = zutalk_log::flush(zutalk_log::LOG_FLUSH_WAIT_MS); // dropped: tests without an installed process log have no writer to flush

    let base = env.config.data.join("diagnose");
    fs::create_dir_all(&base).map_err(|_| "diagnose")?;
    let prev = env.last_diagnose_ms.load(Ordering::Relaxed);
    let stamp = wall_ms().max(prev.saturating_add(1));
    env.last_diagnose_ms.store(stamp, Ordering::Relaxed);

    let tmp = base.join(format!("{stamp}.tmp"));
    let dest = base.join(format!("{stamp}"));
    if tmp.exists() {
        let _ = fs::remove_dir_all(&tmp); // dropped: stale temp dir from an interrupted diagnose
    }
    fs::create_dir(&tmp).map_err(|_| "diagnose")?;

    let write_res: Result<(usize, u64), &'static str> = (|| {
        let mut files = 0usize;
        let mut bytes = 0u64;
        let mut put = |name: &str, data: Vec<u8>| -> Result<(), &'static str> {
            fs::write(tmp.join(name), &data).map_err(|_| "diagnose")?;
            files += 1;
            bytes += data.len() as u64;
            Ok(())
        };
        put("about.json", serde_json::to_vec_pretty(&facts).map_err(|_| "diagnose")?)?;
        put("water.json", serde_json::to_vec_pretty(&*water).map_err(|_| "diagnose")?)?;
        put("stats.json", serde_json::to_vec_pretty(&stats).map_err(|_| "diagnose")?)?;
        put("tasks.json", serde_json::to_vec_pretty(&tasks).map_err(|_| "diagnose")?)?;

        let logs_src = env.config.logs();
        if logs_src.is_dir() {
            let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            if let Ok(entries) = fs::read_dir(&logs_src) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    {
                        let mtime = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        found.push((mtime, path));
                    }
                }
            }
            found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            found.truncate(DIAGNOSE_LOG_FILES);
            if !found.is_empty() {
                let logs_dst = tmp.join("logs");
                fs::create_dir(&logs_dst).map_err(|_| "diagnose")?;
                for (_, src) in found {
                    let Some(name) = src.file_name() else { continue };
                    let copied = fs::copy(&src, logs_dst.join(name)).map_err(|_| "diagnose")?;
                    files += 1;
                    bytes += copied;
                }
            }
        }
        Ok((files, bytes))
    })();

    let (files, bytes) = match write_res {
        Ok(counts) => counts,
        Err(code) => {
            let _ = fs::remove_dir_all(&tmp); // dropped: best-effort cleanup of half-written bundle
            return Err(code);
        }
    };

    if fs::rename(&tmp, &dest).is_err() {
        let _ = fs::remove_dir_all(&tmp); // dropped: best-effort cleanup when atomic rename fails
        return Err("diagnose");
    }

    prune_diagnose_dirs(&base);
    info!("diagnose.wrote", files = files, bytes = bytes);
    Ok(DeskOut::Diagnosed)
}

fn prune_diagnose_dirs(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else { return };
    let mut dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let stamp = entry.file_name().to_str()?.parse::<u64>().ok()?;
            Some((stamp, path))
        })
        .collect();
    dirs.sort_by_key(|(stamp, _)| *stamp);
    let excess = dirs.len().saturating_sub(DIAGNOSE_KEPT_DIRS);
    for (_, old) in dirs.into_iter().take(excess) {
        let _ = fs::remove_dir_all(&old); // dropped: old bundle will be pruned on the next diagnose
    }
}

fn run_boot(env: &DeskEnv) -> Result<DeskOut, &'static str> {
    let mut boot_say = Vec::new();

    // Step 1: settings.json
    let t = std::time::Instant::now();
    let setup = match fs::read(env.config.data.join("settings.json")) {
        Ok(json) => match serde_json::from_slice::<Setup>(&json) {
            Ok(mut setup) => {
                setup.caption = setup.caption.normalized();
                setup.topics = topic::normalize_selected_topics(setup.topics);
                let ms = t.elapsed().as_millis() as u64;
                info!("boot.step", step = "settings", code = "ok", ms = ms);
                setup
            }
            Err(_) => {
                let ms = t.elapsed().as_millis() as u64;
                warn!("boot.step", step = "settings", code = "settings", ms = ms);
                boot_say.push("settings");
                Setup::initial()
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let ms = t.elapsed().as_millis() as u64;
            info!("boot.step", step = "settings", code = "ok", ms = ms);
            Setup::initial()
        }
        Err(_) => {
            let ms = t.elapsed().as_millis() as u64;
            warn!("boot.step", step = "settings", code = "settings", ms = ms);
            boot_say.push("settings");
            Setup::initial()
        }
    };
    *env.keep_hint.lock().unwrap_or_else(PoisonError::into_inner) = setup.keep;

    // Step 2: identity (kept person + local device identity + Who::of + shelf/refresh/survey)
    let t = std::time::Instant::now();
    let kept = keys::kept();
    let kept_is_none = matches!(kept, Ok(None));
    let kept = kept.unwrap_or_else(|end| {
        boot_say.push(code(end));
        None
    });
    let (local, made) = match keys::local() {
        Ok((me, made)) => (Ok(me), made),
        Err(end) => (Err(end), false),
    };
    let who = Who::of(kept, local);
    let who_err = match &who {
        Who::Nobody(c) => Some(*c),
        Who::Local(_) | Who::Person(_) => None,
    };
    let has_person = who.person().is_some();
    let mut signed = run_sign(env, who, "boot", made, setup.keep, &[], None)?;
    let id_ms = t.elapsed().as_millis() as u64;
    if let Some(c) = who_err {
        warn!("boot.step", step = "identity", code = c, ms = id_ms);
    } else {
        info!("boot.step", step = "identity", code = "ok", ms = id_ms);
    }

    // Step 3: 0.8 community invite (never blocks boot; network work spawns on work subtree)
    let t = std::time::Instant::now();
    let invite_path = env.config.old.as_ref().map(|old| old.join("Secrets/community-invite.json"));
    let invite_res = invite_path.as_deref().map(keys::invite).transpose().map_err(code);
    let (start_create, start_claim) = match invite_res {
        Ok(opt) => {
            let invite = opt.flatten();
            let ms = t.elapsed().as_millis() as u64;
            info!("boot.step", step = "invites", code = "ok", ms = ms);
            *env.invite.lock().unwrap_or_else(PoisonError::into_inner) = invite.clone();
            match invite {
                Some(_) if kept_is_none => (true, None),
                Some(token) if has_person => (false, Some(token)),
                _ => (false, None),
            }
        }
        Err(c) => {
            let ms = t.elapsed().as_millis() as u64;
            warn!("boot.step", step = "invites", code = c, ms = ms);
            boot_say.push(c);
            (false, None)
        }
    };

    boot_say.extend(signed.say);
    signed.say = boot_say;

    let recover_host = if has_person { host::read_lock(&env.config.data) } else { None };
    Ok(DeskOut::Booted {
        out: Box::new(BootOut { setup, signed }),
        start_create,
        start_claim,
        recover_host,
    })
}

fn run_sign(
    env: &DeskEnv,
    who: Who,
    why: &'static str,
    made: bool,
    keep: Option<u32>,
    busy: &[String],
    initial_handle: Option<Option<String>>,
) -> Result<SignedOut, &'static str> {
    let config = &env.config;
    let http = &env.http;
    let identity = &env.identity;
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
    let rotation_unsaved = me.as_deref().is_some_and(keys::unsaved_for);
    if let (Some(h), Some(s)) = (initial_handle.as_ref(), shelf.as_deref()) {
        let _ = identity::write_cached_handle(s.root(), h.as_deref()); // dropped: offline handle cache is best-effort
    }
    let handle = if me.is_some() {
        initial_handle
            .flatten()
            .or_else(|| shelf.as_deref().and_then(|s| identity::read_cached_handle(s.root())))
    } else {
        None
    };

    // Adopt any existing device-local (`did:key`) records and terms into the first signed-in
    // person's shelf before opening the SQLite index and loading the glossary (`P4.md` §3.4).
    let local_shelf = match (&who, shelf.as_deref()) {
        (Who::Person(person), Some(person_shelf)) => {
            let _ = topic::ensure_migrated(person_shelf, now()); // dropped: best-effort legacy talk/term migration before adopt
            if let Ok(Some(local_me)) = keys::local_kept() {
                let local = make_shelf(&local_me);
                if local.root().is_dir() {
                    let mut exclude: Vec<String> = busy.to_vec();
                    if let Some(active) = env.views.active_recording()
                        && !exclude.contains(&active)
                    {
                        exclude.push(active);
                    }
                    match adopt::run(&config.data, &local, person_shelf, &exclude, now()) {
                        Ok(sum) => {
                            if sum.plan == adopt::AdoptPlan::Adopt
                                && (sum.recordings > 0 || sum.terms > 0 || sum.topics > 0)
                            {
                                info!(
                                    "identity.adopt",
                                    recordings = sum.recordings,
                                    terms = sum.terms,
                                    code = "ok"
                                );
                            }
                        }
                        Err(c) => {
                            warn!("identity.adopt", recordings = 0u32, terms = 0u32, code = c);
                            say.push(c);
                        }
                    }
                }
                adopt::status(&config.data, Some(&local), Some(&person.did))
            } else {
                LocalShelf::default()
            }
        }
        (Who::Local(_), Some(local)) => adopt::status(&config.data, Some(local), None),
        _ => LocalShelf::default(),
    };

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

    // Drop any previous identity's index handle before sweeping/reopening.
    *env.index.lock().unwrap_or_else(PoisonError::into_inner) = None;

    let snap = snapshot(identity);
    if let Ok(shelf) = &snap.shelf {
        let _ = topic::ensure_migrated(shelf, now()); // dropped: best-effort legacy talk/term migration on sign-in
        refresh_topic_caches(env, shelf);
    } else {
        *env.topics.lock().unwrap_or_else(PoisonError::into_inner) = Vec::new();
    }

    let refreshed = match run_refresh(env, &snap, 0, Some(keep)) {
        Ok(refreshed) => refreshed,
        Err(c) => {
            say.push(c);
            Refreshed {
                rev: 0,
                keys: Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect(),
                left: snap.keys.left(),
                quota: snap.keys.quota(),
            }
        }
    };

    let has_soniox = snap.keys.from(Service::Soniox) != From::Nowhere;
    if let Ok(shelf) = &snap.shelf {
        let path = index_path(&config.data, shelf.by());
        let tapes = config.tapes();
        let opened_path = match Index::open(&path, shelf, &tapes, env.root.stats()) {
            Ok(idx) => {
                *env.index.lock().unwrap_or_else(PoisonError::into_inner) = Some(idx);
                Some(path)
            }
            Err(c) => {
                say.push(c);
                None
            }
        };
        env.views.set_source(Some(shelf.clone()), opened_path, has_soniox, true);
    } else {
        env.views.set_source(None, None, false, true);
    }
    env.views.invalidate_all();

    let survey = match run_survey(config, &snap, busy, true) {
        Ok(survey) => survey,
        Err(c) => {
            say.push(c);
            Survey { writes: Vec::new(), sweep: false, filings: Vec::new() }
        }
    };

    let shelf_by = snap.shelf.as_ref().ok().map(|s| s.by().to_owned());
    Ok(SignedOut { me, shelf_by, handle, rotation_unsaved, local_shelf, refreshed, survey, say })
}

fn refresh_topic_caches(env: &DeskEnv, shelf: &Shelf) {
    if let Ok(list) = topic::list(shelf) {
        *env.topics.lock().unwrap_or_else(PoisonError::into_inner) = list;
    }
}

fn run_refresh(
    env: &DeskEnv,
    snap: &Snapshot,
    rev: u64,
    retain: Option<Option<u32>>,
) -> Result<Refreshed, &'static str> {
    let config = &env.config;
    let keys: Vec<(String, From)> =
        Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
    let left = snap.keys.left();
    let quota = snap.keys.quota();
    let Ok(shelf) = &snap.shelf else {
        return Ok(Refreshed { rev, keys, left, quota });
    };
    if let Some(keep) = retain {
        let tapes = config.tapes();
        let mut entries: Vec<retain::Entry> = shelf
            .list()
            .map_err(fail)?
            .iter()
            .map(|r| retain::Entry {
                id: r.id.clone(),
                started: r.about.started,
                draft: r.about.take == 0,
                audio: tape::kept(&tapes.join(&r.id)),
            })
            .collect();
        let destroyed = retain::sweep(&tapes, keep, &mut entries, now())?;
        if !destroyed.is_empty() {
            env.with_index(|idx| {
                for id in &destroyed {
                    let _ = idx.update_audio(id, false); // dropped: index.sync logged error
                }
            });
            env.views.invalidate(ViewKind::Library, None);
            env.views.invalidate(ViewKind::Recording, None);
        }
    }
    Ok(Refreshed { rev, keys, left, quota })
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
    let recordings = shelf.list().map_err(fail)?;
    let (mut writes, mut skipped, mut why) = (Vec::new(), 0usize, "");
    for r in &recordings {
        if busy.iter().any(|id| id == &r.id) {
            continue;
        }
        let draft = r.about.take == 0;
        let audio = tape::kept(&tapes.join(&r.id));
        // Skip drafts that stopped with a persistent fatal refinement error until the user
        // supplies a new key or retries manually (`P2.md` §3.3.2).
        if draft
            && audio
            && write::Journal::load(&tapes.join(&r.id)).is_ok_and(|j| j.failed.is_some())
        {
            continue;
        }
        // Nothing is written again without its audio, nor for one moved from 0.8: it keeps the
        // transcript it had there, or the lack of one, and its sound is not sent anywhere.
        match write::owed(draft, audio, r.about.moved, has_key) {
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

#[allow(clippy::too_many_arguments)]
fn spawn_create_net(
    tree: &Tree,
    signing: &Mutex<Option<CancellationToken>>,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    http: reqwest::Client,
    directory: String,
    desk: Desk<Job>,
    keep_hint: Arc<Mutex<Option<u32>>>,
    results: Tx<Msg>,
) {
    let cancel = replace_signing(signing);
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.create", async move {
        info!("job.start", kind = "create", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let outcome = tokio::select! {
            () = cancel.cancelled() => None,
            res = again(|| keys::create(&http, &directory), |why, wait_ms, n| {
                warn!("job.again", kind = "create", why = why, wait_ms = wait_ms, n = n);
            }) => Some(res),
        };
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            None => {
                info!("job.done", kind = "create", job = seq, code = "cancelled", ms = ms);
            }
            Some(Ok(me)) => {
                info!("job.done", kind = "create", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
                let keep = *keep_hint.lock().unwrap_or_else(PoisonError::into_inner);
                let job = Job::Sign {
                    who: Who::Person(me),
                    why: "sign_in",
                    made: false,
                    keep,
                    busy: Vec::new(),
                    initial_handle: None,
                };
                let _ = desk.send(job).await; // dropped: desk ended on shutdown
            }
            Some(Err(end)) => {
                let c = code(end);
                warn!("job.done", kind = "create", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
                let _ = results.send(Msg::SignInFailed { auth_gen: 0, code: c }).await; // dropped: the hub has ended
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_quota_net(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    ours: Ours,
    chain: Arc<Chain>,
    results: Tx<Msg>,
    notify: bool,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.quota", async move {
        info!("job.start", kind = "quota", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let outcome = quota::fetch(&ours).await;
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok((left, quota_info)) => {
                chain.set_quota(left, quota_info.clone());
                let now_ms = now();
                let soniox_left =
                    quota::remaining_for(&quota_info, Service::Soniox, now_ms).unwrap_or(0);
                let openrouter_left =
                    quota::remaining_for(&quota_info, Service::OpenRouter, now_ms).unwrap_or(0);
                info!(
                    "quota.updated",
                    soniox_left = soniox_left,
                    openrouter_left = openrouter_left,
                    code = "ok"
                );
                info!("job.done", kind = "quota", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
                let _ = results.send(Msg::QuotaUpdated { left, quota: quota_info, notify }).await; // dropped: the hub has ended
            }
            Err(end) => {
                let c = code(end);
                warn!("quota.updated", soniox_left = 0u32, openrouter_left = 0u32, code = c);
                warn!("job.done", kind = "quota", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
                if notify {
                    let _ = results.send(Msg::QuotaFailed(c)).await; // dropped: the hub has ended
                }
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_profile_net(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    http: reqwest::Client,
    directory: String,
    did: String,
    shelf_root: PathBuf,
    results: Tx<Msg>,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let run_tree = tree.clone();
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.profile", async move {
        info!("job.start", kind = "profile", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let outcome = identity::fetch_handle(&http, &directory, &did).await;
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(handle) => {
                let to_cache = handle.clone();
                let write_res = run_tree
                    .blocking("profile.write", move || {
                        identity::write_cached_handle(&shelf_root, to_cache.as_deref())
                    })
                    .await;
                let _ = write_res; // dropped: best-effort offline handle cache
                info!("job.done", kind = "profile", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
                let _ = results.send(Msg::HandleResolved(handle)).await; // dropped: the hub has ended
            }
            Err(end) => {
                let c = code(end);
                warn!("job.done", kind = "profile", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_claim_net(
    tree: &Tree,
    signing: &Mutex<Option<CancellationToken>>,
    invite: &Arc<Mutex<Option<Secret>>>,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    chain: Arc<Chain>,
    token: Secret,
    results: Tx<Msg>,
) {
    let cancel = replace_signing(signing);
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let invite = invite.clone();
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.claim", async move {
        info!("job.start", kind = "claim", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let outcome = tokio::select! {
            () = cancel.cancelled() => None,
            res = again(|| chain.claim(&token), |why, wait_ms, n| {
                warn!("job.again", kind = "claim", why = why, wait_ms = wait_ms, n = n);
            }) => Some(res),
        };
        let ms = started.elapsed().as_millis() as u64;
        let Some(claimed) = outcome else {
            info!("job.done", kind = "claim", job = seq, code = "cancelled", ms = ms);
            return;
        };
        // Gone over, or refused for good: either way it is not sent again.
        let done = keys::carried(&token).and(claimed).map_err(code);
        *invite.lock().unwrap_or_else(PoisonError::into_inner) = None;
        match done {
            Ok(()) => {
                info!("job.done", kind = "claim", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            }
            Err(c) => {
                warn!("job.done", kind = "claim", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
            }
        }
        let _ = results.send(Msg::Claimed(done)).await; // dropped: the hub has ended
    });
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

fn clear_active_if_gen(host_live: &Mutex<Option<ActiveHost>>, host_gen: u64) {
    let mut guard = host_live.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.as_ref().is_some_and(|a| a.host_gen == host_gen) {
        *guard = None;
    }
}

fn finish_host_op(
    host_ops: &AtomicUsize,
    host_live: &Mutex<Option<ActiveHost>>,
    host_busy: &Register<bool>,
) {
    if host_ops.fetch_sub(1, Ordering::AcqRel) == 1
        && host_live.lock().unwrap_or_else(PoisonError::into_inner).is_none()
    {
        host_busy.set(false);
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_host_recover(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    data: PathBuf,
    ours: Ours,
    room_id: String,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let run_tree = tree.clone();
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("net.host_recover", async move {
        info!("job.start", kind = "host.recover", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        let outcome = host::write(&ours, &room_id, None, Write::Gone).await;
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(_) => {
                let clear_data = data.clone();
                let clear_id = room_id.clone();
                let res = run_tree
                    .blocking("host.lock", move || host::clear_lock_if(&clear_data, &clear_id))
                    .await;
                let _ = res; // dropped: lock cleared after crash recovery
                info!("host.recover", code = "ok");
                info!("job.done", kind = "host.recover", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            }
            Err(End::Fatal("room.gone")) => {
                let clear_data = data.clone();
                let clear_id = room_id.clone();
                let res = run_tree
                    .blocking("host.lock", move || host::clear_lock_if(&clear_data, &clear_id))
                    .await;
                let _ = res; // dropped: room already gone on server
                info!("host.recover", code = "gone");
                info!("job.done", kind = "host.recover", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
            }
            Err(end) => {
                let c = code(end);
                warn!("host.recover", code = c);
                warn!("job.done", kind = "host.recover", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_host_slot(
    tree: &Tree,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    host_gen: u64,
    ours: Ours,
    room_id: String,
    key: Zeroizing<[u8; 32]>,
    live_flag: Arc<AtomicBool>,
    live_notify: Arc<Notify>,
    cancel: CancellationToken,
    frame: Reading<Frame>,
    voice: Voice,
    results: Tx<Msg>,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let jobs_done = jobs_done.clone();
    tree.spawn("host.slot", async move {
        info!("job.start", kind = "host.slot", job = seq, waited_ms = 0u64);
        let started = std::time::Instant::now();
        host_slot_loop(
            host_gen,
            ours,
            room_id,
            key,
            live_flag,
            live_notify,
            cancel,
            frame,
            voice,
            results,
        )
        .await;
        let ms = started.elapsed().as_millis() as u64;
        info!("job.done", kind = "host.slot", job = seq, code = "ok", ms = ms);
        jobs_done.add(1);
    });
}

/// Publishes the live room slot: polls waiting viewers every `HOST_ARM_POLL_MS` while armed,
/// then pushes sealed `Frame.columns` at most every `HOST_SLOT_MS` while recording with
/// `HOST_KEEPALIVE_MS` keepalives during silence or pause (`P5.md` §3.3).
#[allow(clippy::too_many_arguments)]
async fn host_slot_loop(
    host_gen: u64,
    ours: Ours,
    room_id: String,
    key: Zeroizing<[u8; 32]>,
    live_flag: Arc<AtomicBool>,
    live_notify: Arc<Notify>,
    cancel: CancellationToken,
    mut live: Reading<Frame>,
    voice: Voice,
    results: Tx<Msg>,
) {
    let mut said: Option<End> = None;

    // Phase 1: Armed — poll viewer count & keep room alive under the 120 s server TTL until
    // recording begins or the room is cancelled.
    while !live_flag.load(Ordering::Acquire) {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = live_notify.notified() => break,
            () = tokio::time::sleep(Duration::from_millis(HOST_ARM_POLL_MS)) => {
                match host::write(&ours, &room_id, Some(&key), Write::Slot(None)).await {
                    Ok(viewers) => {
                        said = None;
                        let _ = results.send(Msg::HostViewers { host_gen, viewers }).await; // dropped: the hub has ended
                    }
                    Err(end) => {
                        if said != Some(end) {
                            voice.say(code(end));
                            said = Some(end);
                        }
                    }
                }
            }
        }
    }

    // Phase 2: Live — push sealed caption columns at most every HOST_SLOT_MS, keeping the room
    // alive during silence/pause every HOST_KEEPALIVE_MS.
    let (_, mut current) = live.now();
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let bytes =
            serde_json::to_vec(&current.columns).ok().filter(|_| !current.columns.is_empty());
        match host::write(&ours, &room_id, Some(&key), Write::Slot(bytes.as_deref())).await {
            Ok(viewers) => {
                said = None;
                let _ = results.send(Msg::HostViewers { host_gen, viewers }).await; // dropped: the hub has ended
            }
            Err(end) => {
                if said != Some(end) {
                    voice.say(code(end));
                    said = Some(end);
                }
            }
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(Duration::from_millis(HOST_SLOT_MS)) => {}
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            next = live.next() => {
                let Some((_, next_frame)) = next else { return };
                current = next_frame;
            }
            () = tokio::time::sleep(Duration::from_millis(HOST_KEEPALIVE_MS)) => {}
        }
    }
}

struct TitleTask {
    id: String,
    manual: bool,
    shelf: Arc<Shelf>,
    keys: Arc<Chain>,
    http: reqwest::Client,
    openrouter: String,
    work_tree: Tree,
    desk: Desk<Job>,
    progress_reg: Register<Progress>,
    said: Voice,
    title_bg: Arc<Semaphore>,
    titling: Arc<Mutex<BTreeSet<String>>>,
    next_seq: Arc<AtomicU64>,
    jobs_done: Counter,
    jobs_failed: Counter,
}

struct TitlingGuard {
    id: String,
    titling: Arc<Mutex<BTreeSet<String>>>,
    progress_reg: Register<Progress>,
}

impl Drop for TitlingGuard {
    fn drop(&mut self) {
        self.titling.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.id);
        set_title_progress(&self.progress_reg, None, Some(&self.id));
    }
}

fn spawn_titles_task(task: TitleTask) {
    {
        let mut set = task.titling.lock().unwrap_or_else(PoisonError::into_inner);
        if !set.insert(task.id.clone()) {
            return; // already titling this recording
        }
    }
    let work_tree = task.work_tree.clone();
    work_tree.spawn("job.titles", async move {
        let _guard = TitlingGuard {
            id: task.id.clone(),
            titling: task.titling.clone(),
            progress_reg: task.progress_reg.clone(),
        };
        let wait_at = tokio::time::Instant::now();
        let Ok(_permit) = task.title_bg.clone().acquire_owned().await else {
            return;
        };
        let waited_ms = wait_at.elapsed().as_millis() as u64;
        let read_shelf = task.shelf.clone();
        let read_id = task.id.clone();
        let loaded = task
            .work_tree
            .blocking("titles.read", move || {
                let rec = read_shelf.read(&read_id).map_err(|_| "shelf")?;
                let topics = topic::list(&read_shelf).unwrap_or_default();
                Ok::<_, &'static str>((rec, topics))
            })
            .await;
        let (rec, all_topics) = match loaded {
            Ok(Ok((Some(rec), topics))) => (rec, topics),
            Ok(Ok((None, _))) => {
                info!(
                    "titles.done",
                    rec = Id(&task.id),
                    code = "gone",
                    applied = false,
                    tags = 0usize,
                    ms = 0u64
                );
                return;
            }
            _ => return,
        };

        let has_lines = rec.lines.iter().any(|(_, s)| !s.line.text.trim().is_empty());
        let has_key = task.keys.from(Service::OpenRouter) != From::Nowhere;
        let title_empty = rec.about.title.trim().is_empty();
        let tags_empty = rec.about.tags.is_empty();
        match titles::title_plan(
            has_lines,
            has_key,
            rec.about.title_by,
            title_empty,
            tags_empty,
            task.manual,
        ) {
            TitleAction::Skip(why) => {
                info!(
                    "titles.done",
                    rec = Id(&task.id),
                    code = "skipped",
                    applied = false,
                    tags = 0usize,
                    ms = 0u64
                );
                if task.manual {
                    task.said.say(why);
                }
                return;
            }
            TitleAction::Generate { .. } => {}
        }

        let seq = task.next_seq.fetch_add(1, Ordering::Relaxed);
        info!("job.start", kind = "titles", job = seq, waited_ms = waited_ms);
        set_title_progress(
            &task.progress_reg,
            Some(Writing { id: task.id.clone(), step: "titling".into(), done: 0, of: 1 }),
            None,
        );

        let line_refs: Vec<&Line> = rec.lines.iter().map(|(_, s)| &s.line).collect();
        let excerpt = titles::sample_transcript(&line_refs);
        let max_t_ms = line_refs.iter().map(|l| l.t1).max().unwrap_or(rec.about.length_ms);
        let first_line =
            line_refs.iter().map(|l| l.text.trim()).find(|t| !t.is_empty()).map(str::to_owned);
        let file_or_event: Option<(TitleBy, String)> =
            if matches!(rec.about.title_by, TitleBy::File | TitleBy::Event)
                && !rec.about.title.trim().is_empty()
            {
                Some((rec.about.title_by, rec.about.title.clone()))
            } else {
                rec.about
                    .title_candidates
                    .iter()
                    .find(|c| {
                        matches!(c.by, TitleBy::Event | TitleBy::File) && !c.text.trim().is_empty()
                    })
                    .map(|c| (c.by, c.text.clone()))
            };

        let ctx = topic::assemble_context(&all_topics, &rec.about.topics, "");
        let topic_hint = ctx
            .general
            .iter()
            .filter(|(k, _)| k == "domain")
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let input = TitleInput { spoken: rec.about.spoken.clone(), topic_hint, excerpt, max_t_ms };
        let started = std::time::Instant::now();
        let generated = titles::generate(&task.http, &task.openrouter, &*task.keys, &input).await;
        let ms = started.elapsed().as_millis() as u64;

        match generated {
            Ok(meta) => {
                let fe_ref = file_or_event.as_ref().map(|(by, text)| (*by, text.as_str()));
                let candidates =
                    titles::build_candidates(&meta.titles, first_line.as_deref(), fe_ref);
                let (done_tx, done_rx) = oneshot::channel();
                let apply_job = Job::ApplyTitles {
                    id: task.id.clone(),
                    shelf_by: task.shelf.by().to_owned(),
                    titles: meta.titles,
                    tags: meta.tags,
                    candidates,
                    elapsed_ms: ms,
                    done: done_tx,
                };
                if task.desk.send(apply_job).await.is_ok() {
                    let _ = done_rx.await; // dropped: desk ended on shutdown
                }
                info!("job.done", kind = "titles", job = seq, code = "ok", ms = ms);
                task.jobs_done.add(1);
            }
            Err(end) => {
                let c = code(end);
                warn!(
                    "titles.done",
                    rec = Id(&task.id),
                    code = c,
                    applied = false,
                    tags = 0usize,
                    ms = ms
                );
                warn!("job.done", kind = "titles", job = seq, code = c, ms = ms);
                task.jobs_failed.add(1);
                if task.manual {
                    task.said.say(c);
                }
            }
        }
    });
}

struct WriteCtx {
    id: String,
    started_by: String,
    shelf: Arc<Shelf>,
    keys: Arc<Chain>,
    config: Arc<Config>,
    results: Tx<Msg>,
    progress_reg: Register<Progress>,
    views: Arc<ViewEngine>,
    index_sync: IndexSync,
    said: Voice,
    work_tree: Tree,
    root: Tree,
    bg: Arc<Semaphore>,
    http: reqwest::Client,
    desk: Desk<Job>,
    title_bg: Arc<Semaphore>,
    titling: Arc<Mutex<BTreeSet<String>>>,
    next_seq: Arc<AtomicU64>,
    jobs_done: Counter,
    jobs_failed: Counter,
    manual: bool,
    from_state: RefineStatus,
}

impl WriteCtx {
    fn spawn_auto_titles(&self) {
        if self.keys.from(Service::OpenRouter) == From::Nowhere {
            return;
        }
        spawn_titles_task(TitleTask {
            id: self.id.clone(),
            manual: false,
            shelf: self.shelf.clone(),
            keys: self.keys.clone(),
            http: self.http.clone(),
            openrouter: self.config.openrouter.clone(),
            work_tree: self.work_tree.clone(),
            desk: self.desk.clone(),
            progress_reg: self.progress_reg.clone(),
            said: self.said.clone(),
            title_bg: self.title_bg.clone(),
            titling: self.titling.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
        });
    }
    fn tape(&self) -> PathBuf {
        self.config.tapes().join(&self.id)
    }

    async fn finish_recovered(self) {
        let started = std::time::Instant::now();
        let sealed = recording::seal(&self.root, self.shelf.clone(), self.tape(), &self.id).await;
        if let Err(c) = sealed {
            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            let ms = started.elapsed().as_millis() as u64;
            info!("job.start", kind = "write", job = seq, waited_ms = 0u64);
            warn!("job.done", kind = "write", job = seq, code = c, ms = ms);
            self.jobs_failed.add(1);
            self.said.say(c);
        }
        self.after_seal_inner(sealed, true).await;
    }

    async fn after_seal(self, sealed: Result<Option<About>, &'static str>) {
        self.after_seal_inner(sealed, false).await;
    }

    async fn after_seal_inner(
        mut self,
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
                let target = self
                    .index_sync
                    .recording_sealed(self.id.clone(), self.started_by.clone())
                    .await;
                let _ = target; // dropped: zero-length recording is not refined
            }
            Ok(Some(about)) => {
                let tape_dir = self.tape();
                let audio = self
                    .work_tree
                    .blocking("tape.kept", move || tape::kept(&tape_dir))
                    .await
                    .unwrap_or(false);
                let sealed_target = self
                    .index_sync
                    .recording_sealed(self.id.clone(), self.started_by.clone())
                    .await;
                if let Some(target) = sealed_target {
                    self.started_by = target.shelf.by().to_owned();
                    self.shelf = target.shelf;
                    self.keys = target.keys;
                }
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
                    write::Owed::Waits(why) => {
                        info!("write.skipped", n = 1usize, why = why);
                        self.spawn_auto_titles();
                    }
                    write::Owed::No => {
                        self.spawn_auto_titles();
                    }
                }
            }
            Ok(None) => {
                let target = self
                    .index_sync
                    .recording_sealed(self.id.clone(), self.started_by.clone())
                    .await;
                let _ = target; // dropped: recording removed before seal
            } // removed meanwhile: the tape stays for a restore
            Err(_) => {
                let target = self
                    .index_sync
                    .recording_sealed(self.id.clone(), self.started_by.clone())
                    .await;
                let _ = target; // dropped: seal failed, no refinement will run
            }
        }
        let _ = self.results.send(Msg::Wrote(self.id.clone())).await; // dropped: the hub has ended
    }

    async fn write_job(&self) {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let slot: Mutex<Option<OwnedSemaphorePermit>> = Mutex::new(None);
        let slot_ref = &slot;
        let state: Mutex<RefineStatus> = Mutex::new(self.from_state.clone());
        let state_ref = &state;
        let entered = AtomicBool::new(false);
        let entered_ref = &entered;
        let written = again(
            || async {
                let wait_at = tokio::time::Instant::now();
                let Ok(permit) = self.bg.clone().acquire_owned().await else {
                    return Err(End::Done);
                };
                let waited_ms = wait_at.elapsed().as_millis() as u64;
                info!("job.start", kind = "write", job = seq, waited_ms = waited_ms);
                let started = std::time::Instant::now();
                let outcome = async {
                    // Removed meanwhile, already written (for auto jobs), or audio destroyed: skip.
                    let about = match self.shelf.about(&self.id) {
                        Ok(Some(read))
                            if (self.manual || read.about.take == 0)
                                && tape::kept(&self.tape()) =>
                        {
                            read.about
                        }
                        _ => return Ok(()),
                    };
                    if !entered_ref.swap(true, Ordering::AcqRel) {
                        let mut cur = state_ref.lock().unwrap_or_else(PoisonError::into_inner);
                        let from = cur.code();
                        let next = refine_step(&cur, RefineEvent::Start, now());
                        info!(
                            "refine.state",
                            rec = Id(&self.id),
                            from = from,
                            to = next.code(),
                            why = "",
                            take = about.take
                        );
                        *cur = next;
                        let init = writing(&self.id, "closing");
                        set_refine_progress(&self.progress_reg, Some(init), None);
                        self.views
                            .set_active_refine(self.id.clone(), Some(("closing".into(), 0, 0)));
                    }
                    let all_topics = topic::list(&self.shelf).unwrap_or_default();
                    let ctx = topic::assemble_context(&all_topics, &about.topics, &about.title);
                    info!(
                        "topic.context",
                        rec = Id(&self.id),
                        where = "refine",
                        topics = ctx.topic_count,
                        terms = ctx.terms.len(),
                        pairs = ctx.translation_pairs.len(),
                        text_chars = ctx.text.chars().count(),
                    );
                    let key = keys::derive("tape", &self.id)?;
                    let (soniox, openrouter) =
                        (self.config.soniox.clone(), self.config.openrouter.clone());
                    let terms: Vec<_> =
                        ctx.terms.into_iter().map(|t| (t.said, t.written)).collect();
                    let translation_pairs: Vec<_> =
                        ctx.translation_pairs.into_iter().map(|p| (p.source, p.target)).collect();
                    let plan = write::Plan {
                        id: self.id.clone(),
                        spoken: about.spoken,
                        into: about.shown,
                        terms,
                        topic: about.title,
                        general: ctx.general,
                        background: ctx.text,
                        translation_pairs,
                        model: WHOLE.into(),
                        soniox,
                        openrouter,
                    };
                    let progress = |step: &str, done: usize, of: usize| {
                        let (done, of) = (done as u32, of as u32);
                        let w = Writing { done, of, ..writing(&self.id, step) };
                        set_refine_progress(&self.progress_reg, Some(w.clone()), None);
                        self.views.set_active_refine(self.id.clone(), Some((w.step, done, of)));
                    };
                    let tape = (self.tape(), key);
                    match write::write(
                        &self.work_tree,
                        &self.shelf,
                        (&tape.0, &tape.1),
                        &plan,
                        &*self.keys,
                        now,
                        progress,
                    )
                    .await
                    {
                        End::Done => Ok(()),
                        End::Retry(c) => {
                            let w = writing(&self.id, c);
                            set_refine_progress(&self.progress_reg, Some(w), None);
                            self.views.set_active_refine(self.id.clone(), Some((c.into(), 0, 0)));
                            Err(End::Retry(c))
                        }
                        end => Err(end),
                    }
                }
                .await;
                let ms = started.elapsed().as_millis() as u64;
                match &outcome {
                    Ok(()) => {
                        info!("job.done", kind = "write", job = seq, code = "ok", ms = ms);
                        self.jobs_done.add(1);
                    }
                    Err(end) => {
                        let c = code(*end);
                        warn!("job.done", kind = "write", job = seq, code = c, ms = ms);
                        self.jobs_failed.add(1);
                    }
                }
                if matches!(outcome, Err(End::Retry(_))) {
                    *slot_ref.lock().unwrap_or_else(PoisonError::into_inner) = Some(permit);
                }
                outcome
            },
            |why, wait_ms, n| {
                warn!("job.again", kind = "write", why = why, wait_ms = wait_ms, n = n);
                let _ = slot_ref.lock().unwrap_or_else(PoisonError::into_inner).take(); // dropped: release gate before sleeping
            },
        )
        .await;
        match written {
            Ok(()) if entered.load(Ordering::Acquire) => {
                let new_take =
                    self.shelf.about(&self.id).ok().flatten().map(|r| r.about.take).unwrap_or(0);
                {
                    let mut cur = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let from = cur.code();
                    let next = refine_step(&cur, RefineEvent::Succeeded { take: new_take }, now());
                    info!(
                        "refine.state",
                        rec = Id(&self.id),
                        from = from,
                        to = next.code(),
                        why = "",
                        take = new_take
                    );
                    *cur = next;
                }
                self.index_sync.refined(self.id.clone(), self.shelf.by().to_owned()).await;
                self.spawn_auto_titles();
            }
            Ok(()) => {
                self.views.set_active_refine(self.id.clone(), None);
            }
            Err(end) => {
                let raw = code(end);
                let failed_at = now();
                let mapped = if matches!(end, End::Fatal(_)) {
                    let mapped = write::refine_error_code(raw, self.keys.from(Service::Soniox));
                    let _ = write::record_failure(&self.tape(), mapped, failed_at); // dropped: best-effort failure persistence beside audio
                    mapped
                } else {
                    raw
                };
                if entered.load(Ordering::Acquire) {
                    let mut cur = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let from = cur.code();
                    let next = refine_step(&cur, RefineEvent::Failed { code: mapped }, failed_at);
                    warn!(
                        "refine.state",
                        rec = Id(&self.id),
                        from = from,
                        to = next.code(),
                        why = mapped,
                        take = 0u32
                    );
                    *cur = next;
                }
                if matches!(end, End::Fatal(_)) {
                    self.index_sync
                        .refine_failed(
                            self.id.clone(),
                            self.shelf.by().to_owned(),
                            RefineFailure { code: mapped.into(), at: failed_at },
                        )
                        .await;
                    self.spawn_auto_titles();
                } else {
                    self.views.set_active_refine(self.id.clone(), None);
                }
                self.said.say(mapped);
            }
        }
        set_refine_progress(&self.progress_reg, None, Some(&self.id));
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

/// One change a person makes: one more record at the path it changes.
fn apply(shelf: &Shelf, id: &str, change: Change) -> Result<(), &'static str> {
    let done = match change {
        Change::Title { text } => {
            let about = shelf.about(id).map_err(fail)?.ok_or("recording.gone")?.about;
            let title_by = if text.trim().is_empty() { TitleBy::None } else { TitleBy::Hand };
            shelf.put(
                id,
                RECORDING,
                &[(id.into(), Some(About { title: text, title_by, ..about }))],
                now(),
            )
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
