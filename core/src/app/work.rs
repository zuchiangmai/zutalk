//! Off-hub work: the sequential `desk` queue for local data/settings/key-store IO, plus dedicated
//! supervised tasks on the `work` subtree with isolated concurrency gates (`local_bg` for local
//! shelf adoption/migration, `cloud_bg` for whole-recording transcription and city filing, and
//! `title_bg` for AI title/tag generation) (`P1.md` §3.6–§3.8, `P1-19`, `P1-20`).

mod cloud;
mod desk;
mod local;

use self::cloud::{
    TitleTask, WriteCtx, spawn_claim_net, spawn_create_net, spawn_host_recover, spawn_host_slot,
    spawn_profile_net, spawn_quota_net, spawn_titles_task,
};
use self::desk::{DeskEnv, DeskOut, Job, run_desk_job};
use self::local::{set_file_progress, set_move_progress, spawn_adopt, spawn_export_write, writing};
use crate::ai::write;
use crate::app::core::{Config, Voice};
use crate::app::hub::Msg;
use crate::app::proto::{Change, From, LocalShelf, NoteOp, Progress, Quota, Setup, Sound, Writing};
use crate::app::recording;
use crate::capture::clock::wall_ms;
use crate::id::identity::{self, PendingSignIn};
use crate::id::keys::{self, Chain, Ours, SIGN_IN_WAIT_MS, Secret, Service, SignIn, Who};
use crate::knowledge::topic::{self, Topic};
use crate::live::frame::Frame;
use crate::out::city::{self, FileGate, Filer, Filing};
use crate::out::host::{self, Write};
use crate::record::migrate;
use crate::record::shelf::{About, Shelf, TitleBy};
use crate::record::takes::{RefineFailure, RefineStatus};
use crate::rt::queue::{self, Full, Tx};
use crate::rt::signal::{Reading, Register};
use crate::rt::stats::Counter;
use crate::rt::supervise::{End, Tree};
use crate::rt::water::Water;
use crate::view::query::{ViewEngine, ViewKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::{Notify, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
use zutalk_log::{info, warn};

/// Jobs buffered for the desk worker: a burst of clicks or edits is dozens of jobs; full means
/// disk is stuck and the person must be told (`P1.md` §3.7).
pub const DESK_QUEUE_JOBS: usize = 64;
/// Cloud background jobs (whole-recording transcription, city filing) run one at a time on their
/// own lane so slow uploads/transcriptions never block local migration or adoption (`K1`).
pub const CLOUD_AT_ONCE_JOBS: usize = 1;
/// Backward-compatible alias for single-concurrency background lane sizing.
pub const BACKGROUND_AT_ONCE_JOBS: usize = CLOUD_AT_ONCE_JOBS;
/// Local disk background jobs (0.8 migration, deferred local-shelf adoption) run one at a time
/// independently of `cloud_bg` (`K1`).
pub const LOCAL_AT_ONCE_JOBS: usize = 1;
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

pub(super) struct Identity {
    pub who: Who,
    pub shelf: Option<Arc<Shelf>>,
    pub ours: Option<Ours>,
    pub keys: Arc<Chain>,
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

    pub(super) fn shelf(&self) -> Result<Arc<Shelf>, &'static str> {
        match &self.shelf {
            Some(shelf) => Ok(shelf.clone()),
            None => Err(self.who.me().err().unwrap_or("keys.store")),
        }
    }
}

#[derive(Clone)]
pub(super) struct Snapshot {
    pub shelf: Result<Arc<Shelf>, &'static str>,
    pub ours: Option<Ours>,
    pub keys: Arc<Chain>,
}

/// Updated write target returned by [`IndexSync::recording_sealed`] so post-seal refinement
/// runs against the active shelf (including a newly adopted person shelf) with fresh keys.
pub(crate) struct SealTarget {
    pub shelf: Arc<Shelf>,
    pub keys: Arc<Chain>,
}

pub(super) enum SyncKind {
    RecordingOpened { id: String },
    RecordingSealed { id: String, started_by: String },
    Refined { id: String, shelf_by: String },
    RefineFailed { id: String, shelf_by: String, failure: RefineFailure },
    FilingUpdated { id: String, state: String },
    Migrated { by: String },
    Adopted { by: String },
}

/// Ordered handle allowing background tasks (recording, refine, file, migrate, adopt) to sync the
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

    pub(crate) fn invalidate_live_draft(&self, id: &str) {
        self.views.invalidate(ViewKind::Transcript, Some(id.to_owned()));
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
        let _ = self.views.clear_active_recording_if(&id); // dropped: returns false if a newer recording already took over
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

    pub(crate) async fn adopted(&self, by: String) {
        self.run(SyncKind::Adopted { by }).await;
    }
}

/// In-memory active hosted room session: the 32-byte key lives only here (`Zeroizing`) and is
/// never written to disk (`PLAN.md` §5.5, `P5.md` §3.3).
pub(super) struct ActiveHost {
    pub host_gen: u64,
    pub room_id: String,
    pub ours: Ours,
    pub live_flag: Arc<AtomicBool>,
    pub live_notify: Arc<Notify>,
    pub cancel: CancellationToken,
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
    cloud_bg: Arc<Semaphore>,
    local_bg: Arc<Semaphore>,
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
        let cloud_bg = Arc::new(Semaphore::new(CLOUD_AT_ONCE_JOBS));
        let local_bg = Arc::new(Semaphore::new(LOCAL_AT_ONCE_JOBS));
        let local_shelf_lock = Arc::new(Mutex::new(()));
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
            local_shelf_lock: local_shelf_lock.clone(),
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
        let deliver_local_bg = local_bg.clone();
        let deliver_local_lock = local_shelf_lock.clone();
        let deliver_progress = progress_reg.clone();
        let deliver_views = views.clone();

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
                let local_bg = deliver_local_bg.clone();
                let local_lock = deliver_local_lock.clone();
                let progress_reg = deliver_progress.clone();
                let views = deliver_views.clone();
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
                        Ok(DeskOut::Booted {
                            out,
                            start_create,
                            start_claim,
                            recover_host,
                            deferred_adopt,
                        }) => {
                            let snap = snapshot(&identity);
                            if let Some(adopt_task) = deferred_adopt {
                                let desk = desk_holder
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(desk) = desk {
                                    spawn_adopt(
                                        &tree,
                                        local_bg.clone(),
                                        local_lock.clone(),
                                        &next_seq,
                                        &jobs_done,
                                        &jobs_failed,
                                        config.data.clone(),
                                        adopt_task,
                                        progress_reg.clone(),
                                        IndexSync { desk, views },
                                        results.clone(),
                                    );
                                }
                            }
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
                                    Ours { server: config.rooms.clone(), ..ours },
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
                        Ok(DeskOut::Signed { out, start_claim, recover_host, deferred_adopt }) => {
                            let snap = snapshot(&identity);
                            if let Some(adopt_task) = deferred_adopt {
                                let desk = desk_holder
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .clone();
                                if let Some(desk) = desk {
                                    spawn_adopt(
                                        &tree,
                                        local_bg.clone(),
                                        local_lock.clone(),
                                        &next_seq,
                                        &jobs_done,
                                        &jobs_failed,
                                        config.data.clone(),
                                        adopt_task,
                                        progress_reg.clone(),
                                        IndexSync { desk, views },
                                        results.clone(),
                                    );
                                }
                            }
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
                                    Ours { server: config.rooms.clone(), ..ours },
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
            cloud_bg,
            local_bg,
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

    pub(crate) fn copy_transcript(
        &self,
        id: String,
        take: Option<u32>,
        languages: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::CopyTranscript { id, take, languages }).map(|_| ())
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

    /// Signals the shell's non-blocking `check_update` port to look for a newer version.
    pub(crate) fn request_update_check(&self) {
        (self.config.check_update)();
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
        let base_ours = snapshot(&self.identity).ours.ok_or("sign_in.first")?;
        let ours = Ours { server: self.config.rooms.clone(), ..base_ours };
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
            cloud_bg: self.cloud_bg.clone(),
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
        let cloud_bg = self.cloud_bg.clone();
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
            let res = write::sweep_with(&http, &tapes, &soniox, &*keys).await;
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
                    cloud_bg: cloud_bg.clone(),
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
            cloud_bg: self.cloud_bg.clone(),
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

    /// Files one recording to a city on the `work` subtree (`job.file`), gated on `cloud_bg`.
    pub(crate) fn file(&self, id: String, filing: Filing) -> Result<(), &'static str> {
        let snap = snapshot(&self.identity);
        let me = snap.ours.ok_or("sign_in.first")?.me;
        let shelf = snap.shelf?;
        let filer = Filer { http: self.http.clone(), me, shelf, tapes: self.config.tapes() };
        let results = self.results.clone();
        let progress_reg = self.progress_reg.clone();
        let index_sync = self.index_sync();
        let tree = self.tree.clone();
        let cloud_bg = self.cloud_bg.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.file", async move {
            set_file_progress(&progress_reg, Some(writing(&id, "filing")), None);
            let gate =
                FileGate { bg: &cloud_bg, seq, jobs_done: &jobs_done, jobs_failed: &jobs_failed };
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

    /// Moves the 0.8 library into the active shelf on the `work` subtree (`job.move`), gated on
    /// `local_bg` so cloud uploads never block local migration (`K1`).
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
        let local_bg = self.local_bg.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.move", async move {
            let wait_at = tokio::time::Instant::now();
            let Ok(_permit) = local_bg.acquire().await else {
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

pub(super) fn replace_signing(cell: &Mutex<Option<CancellationToken>>) -> CancellationToken {
    let next = CancellationToken::new();
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = guard.replace(next.clone()) {
        old.cancel();
    }
    next
}

pub(super) fn clear_pending(cell: &Mutex<Option<PendingSignIn>>) {
    let _ = cell.lock().unwrap_or_else(PoisonError::into_inner).take(); // dropped: zeroize abandoned pending sign-in
}

pub(super) fn cancel_signing(
    cell: &Mutex<Option<CancellationToken>>,
    pending: &Mutex<Option<PendingSignIn>>,
) {
    {
        let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = guard.take() {
            old.cancel();
        }
    }
    clear_pending(pending);
}

pub(super) fn snapshot(identity: &Mutex<Identity>) -> Snapshot {
    let id = identity.lock().unwrap_or_else(PoisonError::into_inner);
    Snapshot { shelf: id.shelf(), ours: id.ours.clone(), keys: id.keys.clone() }
}

pub(super) fn index_path(data: &Path, shelf_by: &str) -> PathBuf {
    data.join("index").join(format!("{}.sqlite3", shelf_by.replace(':', "-")))
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

pub(super) fn code(end: End) -> &'static str {
    if let End::Retry(code) | End::Fatal(code) = end { code } else { "done" }
}

pub(super) fn fail(_: zu::Error) -> &'static str {
    "shelf"
}

pub(super) fn now() -> i64 {
    wall_ms() as i64
}
