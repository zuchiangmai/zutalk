//! Off-hub work: the sequential `desk` queue for local data/settings/key-store IO, plus dedicated
//! supervised tasks on the `work` subtree for boot devices, search, export file writes, sign-in,
//! identity creation, invite claiming, city calendar, rooms/slots, whole-recording transcription,
//! city filing, 0.8 migration, and update installation (`P1.md` §3.6–§3.8, `P1-19`, `P1-20`).

use crate::ai::write;
use crate::app::core::{Config, Voice};
use crate::app::hub::Msg;
use crate::app::proto::{Change, From, NoteOp, Open, Room, Row, Setup, Sound, State, Writing};
use crate::app::recording;
use crate::capture::clock::wall_ms;
use crate::capture::tape;
use crate::id::keys::{self, Chain, Me, Ours, Secret, Service, SignIn, Who};
use crate::knowledge::glossary::{self, Term};
use crate::live::frame::Frame;
use crate::live::lines::Line;
use crate::out::city::{self, Calendar, FileGate, Filer, Filing};
use crate::out::export::{Doc, EXPORTERS};
use crate::out::room::{self, Write};
use crate::record::notes;
use crate::record::shelf::{
    About, FILING, LINE, NOTE, RECORDING, Recording, SHARE, SPEAKER, Share, Shelf, Words,
};
use crate::record::takes::{RefineEvent, RefineStatus, initial_refine_status, refine_step};
use crate::record::{migrate, retain};
use crate::rt::queue::{self, Full, Tx};
use crate::rt::signal::Reading;
use crate::rt::stats::Counter;
use crate::rt::supervise::{End, Tree, again};
use crate::rt::water::Water;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use zutalk_log::{Id, info, warn};

const WHOLE: &str = "stt-async-v6";
/// A live room is written at most this often (DESIGN.md, rooms). Origin to be traced.
const SLOT_MS: u64 = 250;
/// Jobs buffered for the desk worker: a burst of clicks or edits is dozens of jobs; full means
/// disk is stuck and the person must be told (`P1.md` §3.7).
pub const DESK_QUEUE_JOBS: usize = 64;
/// Heavy background jobs (whole-recording transcription, city filing, 0.8 migration) run one at a
/// time so they neither fight each other nor starve live captioning (`PLAN.md` §4.1, `P1.md` §3.8).
pub const BACKGROUND_AT_ONCE_JOBS: usize = 1;
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

/// Complete result of switching the signing identity on `desk` (carries no keys).
pub(crate) struct SignedOut {
    pub me: Option<String>,
    pub rotation_unsaved: bool,
    pub refreshed: Refreshed,
    pub rooms: Vec<Room>,
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

enum Job {
    Boot,
    Settings { setup: Setup },
    Sign { who: Who, why: &'static str, made: bool, keep: Option<u32>, busy: Vec<String> },
    SignOut { keep: Option<u32>, busy: Vec<String> },
    Clip,
    RotationSaved,
    Refresh { rev: u64, opened: Option<String>, retain: Option<Option<u32>>, include_rooms: bool },
    Change { id: String, change: Change },
    Note { id: String, op: NoteOp },
    RefinePrepare { id: String },
    Remove { id: String },
    Restore { id: String },
    Term { said: String, written: Option<BTreeMap<String, String>> },
    DestroyAudio { id: String, under_way: Option<String> },
    Export { id: String, exporter: &'static str, languages: Vec<String>, to: String },
    SharePrepare { recording: Option<String> },
    PutShare { id: String, share: Option<Share>, reply: oneshot::Sender<Result<(), &'static str>> },
    UnsharePrepare { id: String, recording: Option<String> },
    BeginFile { id: String, calendar: Calendar, event: String, audio: bool },
    OwnKey { service: Service, key: Option<String>, busy: Vec<String> },
    Diagnose { spoken: Vec<String>, shown: Vec<String>, sound: Sound, captions: bool },
}

impl DeskJob for Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::Boot => "boot",
            Job::Settings { .. } => "settings",
            Job::Sign { .. } => "sign",
            Job::SignOut { .. } => "signout",
            Job::Clip => "clip",
            Job::RotationSaved => "rotation_saved",
            Job::Refresh { .. } => "refresh",
            Job::Change { .. } => "change",
            Job::Note { .. } => "note",
            Job::RefinePrepare { .. } => "refine.prepare",
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
            Job::Diagnose { .. } => "diagnose",
        }
    }
}

enum DeskOut {
    Changed,
    Silent,
    Copied,
    Diagnosed,
    RotationSaved,
    Booted {
        out: BootOut,
        start_create: bool,
        start_claim: Option<Secret>,
    },
    Signed {
        out: SignedOut,
        start_claim: Option<Secret>,
    },
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
    RefineStart {
        id: String,
        from: RefineStatus,
    },
    OwnKey(OwnKeyOut),
}

struct DeskEnv {
    config: Arc<Config>,
    http: reqwest::Client,
    identity: Arc<Mutex<Identity>>,
    invite: Arc<Mutex<Option<Secret>>>,
    signing: Arc<Mutex<Option<CancellationToken>>>,
    keep_hint: Arc<Mutex<Option<u32>>>,
    root: Tree,
    water: Reading<Water>,
    started_at: std::time::Instant,
    last_diagnose_ms: AtomicU64,
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
    signing: Arc<Mutex<Option<CancellationToken>>>,
    keep_hint: Arc<Mutex<Option<u32>>>,
    desk: Desk<Job>,
    bg: Arc<Semaphore>,
    next_seq: Arc<AtomicU64>,
    slot: Arc<Mutex<Option<(String, CancellationToken)>>>,
    jobs_done: Counter,
    jobs_failed: Counter,
    search_stale: Counter,
}

impl Work {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: Arc<Config>,
        http: reqwest::Client,
        results: Tx<Msg>,
        progress: Tx<Msg>,
        said: Voice,
        root: Tree,
        frame: Reading<Frame>,
        water: Reading<Water>,
    ) -> Self {
        let tree = root.child("work");
        let stats = tree.stats();
        let jobs_done = stats.counter("jobs.done");
        let jobs_failed = stats.counter("jobs.failed");
        let search_stale = stats.counter("search.stale");
        let next_seq = Arc::new(AtomicU64::new(1));
        let bg = Arc::new(Semaphore::new(BACKGROUND_AT_ONCE_JOBS));
        let identity = Arc::new(Mutex::new(Identity::starting()));
        let invite: Arc<Mutex<Option<Secret>>> = Arc::new(Mutex::new(None));
        let signing: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
        let keep_hint: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let slot: Arc<Mutex<Option<(String, CancellationToken)>>> = Arc::new(Mutex::new(None));

        let run_env = Arc::new(DeskEnv {
            config: config.clone(),
            http: http.clone(),
            identity: identity.clone(),
            invite: invite.clone(),
            signing: signing.clone(),
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
        let deliver_slot = slot.clone();
        let deliver_frame = frame.clone();
        let deliver_said = said.clone();
        let deliver_id = identity.clone();
        let deliver_invite = invite.clone();
        let deliver_signing = signing.clone();
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
                let slot = deliver_slot.clone();
                let frame = deliver_frame.clone();
                let said = deliver_said.clone();
                let identity = deliver_id.clone();
                let invite = deliver_invite.clone();
                let signing = deliver_signing.clone();
                let keep_hint = deliver_keep.clone();
                let desk_holder = deliver_desk.clone();
                async move {
                    match outcome {
                        Ok(DeskOut::Changed) => {
                            let _ = results.send(Msg::DeskChanged).await; // dropped: the hub has ended
                        }
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
                        Ok(DeskOut::Booted { out, start_create, start_claim }) => {
                            let live = out
                                .signed
                                .rooms
                                .iter()
                                .find(|r| r.recording.is_none())
                                .map(|r| r.id.clone());
                            let snap = snapshot(&identity);
                            sync_slot_inner(
                                &tree, &slot, live, snap.ours, &frame, &said, &next_seq, &jobs_done,
                            );
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
                                        http,
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
                                    snap.keys,
                                    token,
                                    results.clone(),
                                );
                            }
                            let _ = results.send(Msg::Booted(out)).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::Signed { out, start_claim }) => {
                            let live = out
                                .rooms
                                .iter()
                                .find(|r| r.recording.is_none())
                                .map(|r| r.id.clone());
                            let snap = snapshot(&identity);
                            sync_slot_inner(
                                &tree, &slot, live, snap.ours, &frame, &said, &next_seq, &jobs_done,
                            );
                            if let Some(token) = start_claim {
                                spawn_claim_net(
                                    &tree,
                                    &signing,
                                    &invite,
                                    &next_seq,
                                    &jobs_done,
                                    &jobs_failed,
                                    snap.keys,
                                    token,
                                    results.clone(),
                                );
                            }
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
                        Ok(DeskOut::RefineStart { id, from }) => {
                            let _ = results.send(Msg::RefineReady { id, from }).await; // dropped: the hub has ended
                        }
                        Ok(DeskOut::OwnKey(out)) => {
                            let _ = results.send(Msg::OwnKeyDone(Ok(out))).await; // dropped: the hub has ended
                        }
                        Err(code) => {
                            let msg = match kind {
                                "refresh" => Msg::Refreshed(Err(code)),
                                "sign" | "signout" => Msg::Signed(Err(code)),
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
            signing,
            keep_hint,
            desk,
            bg,
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

    /// Starts a sign-in relay wait on `net.signin` and returns the QR URL to display.
    /// Keys stay inside `Work` and never cross the hub's mailbox (`P1-20`).
    pub(crate) fn sign_in(&self) -> Result<String, &'static str> {
        let sign_in = SignIn::begin(&self.config.server).map_err(code)?;
        let url = sign_in.code().to_owned();
        let cancel = replace_signing(&self.signing);
        let http = self.http.clone();
        let desk = self.desk.clone();
        let keep_hint = self.keep_hint.clone();
        let results = self.results.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("net.signin", async move {
            info!("job.start", kind = "signin", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let outcome = tokio::select! {
                () = cancel.cancelled() => None,
                res = sign_in.finish(&http) => Some(res),
            };
            let ms = started.elapsed().as_millis() as u64;
            match outcome {
                None => {
                    info!("job.done", kind = "signin", job = seq, code = "cancelled", ms = ms);
                }
                Some(Ok(me)) => {
                    info!("job.done", kind = "signin", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
                    let keep = *keep_hint.lock().unwrap_or_else(PoisonError::into_inner);
                    let job = Job::Sign {
                        who: Who::Person(me),
                        why: "sign_in",
                        made: false,
                        keep,
                        busy: Vec::new(),
                    };
                    let _ = desk.send(job).await; // dropped: desk ended on shutdown
                }
                Some(Err(end)) => {
                    let c = code(end);
                    warn!("job.done", kind = "signin", job = seq, code = c, ms = ms);
                    jobs_failed.add(1);
                    let _ = results.send(Msg::SignInFailed(c)).await; // dropped: the hub has ended
                }
            }
        });
        Ok(url)
    }

    pub(crate) fn sign_out(
        &self,
        keep: Option<u32>,
        busy: Vec<String>,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::SignOut { keep, busy }).map(|_| ())
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
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("net.calendar", async move {
            info!("job.start", kind = "calendar", job = seq, waited_ms = 0u64);
            let started = std::time::Instant::now();
            let res = city::calendar(&http, &address).await;
            let ms = started.elapsed().as_millis() as u64;
            match &res {
                Ok(_) => {
                    info!("job.done", kind = "calendar", job = seq, code = "ok", ms = ms);
                    jobs_done.add(1);
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
        opened: Option<String>,
        retain: Option<Option<u32>>,
        include_rooms: bool,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Refresh { rev, opened, retain, include_rooms }).map(|_| ())
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

    pub(crate) fn diagnose(
        &self,
        spoken: Vec<String>,
        shown: Vec<String>,
        sound: Sound,
        captions: bool,
    ) -> Result<(), &'static str> {
        self.desk.offer(Job::Diagnose { spoken, shown, sound, captions }).map(|_| ())
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
            bg: self.bg.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
            terms,
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
    pub(crate) fn write_recovered(&self, ids: Vec<String>, terms: Vec<Term>) {
        let snap = snapshot(&self.identity);
        let Ok(shelf) = snap.shelf else { return };
        let keys = snap.keys;
        let tapes = self.config.tapes();
        let soniox = self.config.soniox.clone();
        let said = self.said.clone();
        let work_tree = self.tree.clone();
        let root = self.root.clone();
        let bg = self.bg.clone();
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
                    bg: bg.clone(),
                    next_seq: next_seq.clone(),
                    jobs_done: jobs_done.clone(),
                    jobs_failed: jobs_failed.clone(),
                    terms: terms.clone(),
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
    pub(crate) fn write_manual(&self, id: String, from: RefineStatus, terms: Vec<Term>) {
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
            shelf,
            keys: snap.keys,
            config: self.config.clone(),
            results: self.results.clone(),
            progress: self.progress.clone(),
            said: self.said.clone(),
            work_tree: self.tree.clone(),
            root: self.root.clone(),
            bg: self.bg.clone(),
            next_seq: self.next_seq.clone(),
            jobs_done: self.jobs_done.clone(),
            jobs_failed: self.jobs_failed.clone(),
            terms,
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
        let progress = self.progress.clone();
        let tree = self.tree.clone();
        let bg = self.bg.clone();
        let seq = self.alloc_seq();
        let jobs_done = self.jobs_done.clone();
        let jobs_failed = self.jobs_failed.clone();
        self.tree.spawn("job.file", async move {
            let gate = FileGate { bg: &bg, seq, jobs_done: &jobs_done, jobs_failed: &jobs_failed };
            let said = city::file_job(&tree, &filer, &id, filing, gate, |w| {
                let _ = progress.offer(Msg::Filing(w)); // dropped: the next one replaces it
            })
            .await;
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

fn replace_signing(cell: &Mutex<Option<CancellationToken>>) -> CancellationToken {
    let next = CancellationToken::new();
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = guard.replace(next.clone()) {
        old.cancel();
    }
    next
}

fn cancel_signing(cell: &Mutex<Option<CancellationToken>>) {
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = guard.take() {
        old.cancel();
    }
}

fn snapshot(identity: &Mutex<Identity>) -> Snapshot {
    let id = identity.lock().unwrap_or_else(PoisonError::into_inner);
    Snapshot { shelf: id.shelf(), ours: id.ours.clone(), keys: id.keys.clone() }
}

fn run_desk_job(job: Job, env: &DeskEnv) -> Result<DeskOut, &'static str> {
    let config = &env.config;
    let http = &env.http;
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
        Job::Sign { who, why, made, keep, busy } => {
            let has_person = who.person().is_some();
            let out = run_sign(config, http, identity, who, why, made, keep, &busy)?;
            let start_claim = if has_person {
                env.invite.lock().unwrap_or_else(PoisonError::into_inner).clone()
            } else {
                None
            };
            Ok(DeskOut::Signed { out, start_claim })
        }
        Job::SignOut { keep, busy } => {
            cancel_signing(&env.signing);
            keys::set("identity", None).map_err(code)?;
            let local = keys::local().map(|(me, _)| me);
            let out = run_sign(
                config,
                http,
                identity,
                Who::of(None, local),
                "sign_out",
                false,
                keep,
                &busy,
            )?;
            Ok(DeskOut::Signed { out, start_claim: None })
        }
        Job::Clip => {
            let key = keys::rotation().map_err(code)?.ok_or("rotation.none")?;
            if !(config.clip)(&key) {
                return Err("clip");
            }
            Ok(DeskOut::Copied)
        }
        Job::RotationSaved => {
            keys::saved().map_err(code)?;
            Ok(DeskOut::RotationSaved)
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
            Ok(DeskOut::Changed)
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
            Ok(DeskOut::RefineStart { id, from })
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
            let had_own = key.is_some();
            snap.keys.own(service, key.as_deref()).map_err(code)?;
            let keys_from: Vec<(String, From)> =
                Service::ALL.iter().map(|s| (s.name().into(), snap.keys.from(*s))).collect();
            let left = snap.keys.left();
            let has_soniox = snap.keys.from(Service::Soniox) != From::Nowhere;
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
                        }
                    }
                }
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
            Ok(setup) => {
                let ms = t.elapsed().as_millis() as u64;
                info!("boot.step", step = "settings", code = "ok", ms = ms);
                setup
            }
            Err(_) => {
                let ms = t.elapsed().as_millis() as u64;
                warn!("boot.step", step = "settings", code = "settings", ms = ms);
                boot_say.push("settings");
                Setup::default()
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let ms = t.elapsed().as_millis() as u64;
            info!("boot.step", step = "settings", code = "ok", ms = ms);
            Setup::default()
        }
        Err(_) => {
            let ms = t.elapsed().as_millis() as u64;
            warn!("boot.step", step = "settings", code = "settings", ms = ms);
            boot_say.push("settings");
            Setup::default()
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
    let mut signed =
        run_sign(&env.config, &env.http, &env.identity, who, "boot", made, setup.keep, &[])?;
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

    Ok(DeskOut::Booted { out: BootOut { setup, signed }, start_create, start_claim })
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
        // Skip drafts that stopped with a persistent fatal refinement error until the user
        // supplies a new key or retries manually (`P2.md` §3.3.2).
        if r.draft
            && r.audio
            && write::Journal::load(&tapes.join(&r.id)).is_ok_and(|j| j.failed.is_some())
        {
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
                };
                let _ = desk.send(job).await; // dropped: desk ended on shutdown
            }
            Some(Err(end)) => {
                let c = code(end);
                warn!("job.done", kind = "create", job = seq, code = c, ms = ms);
                jobs_failed.add(1);
                let _ = results.send(Msg::SignInFailed(c)).await; // dropped: the hub has ended
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
    bg: Arc<Semaphore>,
    next_seq: Arc<AtomicU64>,
    jobs_done: Counter,
    jobs_failed: Counter,
    terms: Vec<Term>,
    manual: bool,
    from_state: RefineStatus,
}

impl WriteCtx {
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
                let owner =
                    (!self.shelf.by().starts_with("did:key:")).then(|| self.shelf.by().to_string());
                let _ = self.results.send(Msg::Sealed(owner, sealed_row)).await; // dropped: the hub has ended
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
        let terms: Vec<_> = self.terms.iter().cloned().map(|t| (t.said, t.written)).collect();
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
                    }
                    let key = keys::derive("tape", &self.id)?;
                    let (soniox, openrouter) =
                        (self.config.soniox.clone(), self.config.openrouter.clone());
                    let (id, spoken, into, topic) =
                        (self.id.clone(), about.spoken, about.shown, about.title);
                    let (model, terms) = (WHOLE.into(), terms.clone());
                    let plan =
                        write::Plan { id, spoken, into, terms, topic, model, soniox, openrouter };
                    let progress = |step: &str, done: usize, of: usize| {
                        let (done, of) = (done as u32, of as u32);
                        let msg = Msg::Writing(Writing { done, of, ..writing(&self.id, step) });
                        let _ = self.progress.offer(msg); // dropped: the next one replaces it
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
                            let _ = self.progress.offer(Msg::Writing(writing(&self.id, c))); // dropped: the next one replaces it
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
            Ok(()) => {}
            Err(end) => {
                let raw = code(end);
                let mapped = if matches!(end, End::Fatal(_)) {
                    let mapped = write::refine_error_code(raw, self.keys.from(Service::Soniox));
                    let _ = write::record_failure(&self.tape(), mapped, now()); // dropped: best-effort failure persistence beside audio
                    mapped
                } else {
                    raw
                };
                if entered.load(Ordering::Acquire) {
                    let mut cur = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let from = cur.code();
                    let next = refine_step(&cur, RefineEvent::Failed { code: mapped }, now());
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
                self.said.say(mapped);
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
