//! Network and cloud background tasks (`cloud_bg` refinement, `title_bg` AI titling, quota/profile
//! refresh, invite claim, identity creation, and hosted room slot publishing).

use super::desk::Job;
use super::local::{set_refine_progress, set_title_progress, writing};
use super::{Desk, IndexSync, code, now, replace_signing};
use crate::ai::titles::{self, TitleAction, TitleInput};
use crate::ai::write::{self, MAX_TRANSCRIBE_WAIT_MS};
use crate::app::core::{Config, Voice};
use crate::app::hub::Msg;
use crate::app::proto::{From, Progress, Writing};
use crate::app::recording;
use crate::capture::tape;
use crate::id::identity;
use crate::id::keys::{self, Chain, Ours, Secret, Service, Who};
use crate::id::quota;
use crate::knowledge::topic;
use crate::live::frame::Frame;
use crate::live::lines::Line;
use crate::out::host::{self, HOST_ARM_POLL_MS, HOST_KEEPALIVE_MS, HOST_SLOT_MS, Write};
use crate::record::shelf::{About, Shelf, TitleBy};
use crate::record::takes::{RefineEvent, RefineFailure, RefineStatus, refine_step};
use crate::rt::queue::Tx;
use crate::rt::signal::{Reading, Register};
use crate::rt::stats::Counter;
use crate::rt::supervise::{End, Tree, again};
use crate::view::query::ViewEngine;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
use zutalk_log::{Id, info, warn};

const WHOLE: &str = "stt-async-v6";

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_create_net(
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
pub(super) fn spawn_quota_net(
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
pub(super) fn spawn_profile_net(
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
pub(super) fn spawn_claim_net(
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

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_host_recover(
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
pub(super) fn spawn_host_slot(
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

pub(super) struct TitleTask {
    pub id: String,
    pub manual: bool,
    pub shelf: Arc<Shelf>,
    pub keys: Arc<Chain>,
    pub http: reqwest::Client,
    pub openrouter: String,
    pub work_tree: Tree,
    pub desk: Desk<Job>,
    pub progress_reg: Register<Progress>,
    pub said: Voice,
    pub title_bg: Arc<Semaphore>,
    pub titling: Arc<Mutex<BTreeSet<String>>>,
    pub next_seq: Arc<AtomicU64>,
    pub jobs_done: Counter,
    pub jobs_failed: Counter,
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

pub(super) fn spawn_titles_task(task: TitleTask) {
    {
        let mut set = task.titling.lock().unwrap_or_else(PoisonError::into_inner);
        if !set.insert(task.id.clone()) {
            return;
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

pub(super) struct WriteCtx {
    pub id: String,
    pub started_by: String,
    pub shelf: Arc<Shelf>,
    pub keys: Arc<Chain>,
    pub config: Arc<Config>,
    pub results: Tx<Msg>,
    pub progress_reg: Register<Progress>,
    pub views: Arc<ViewEngine>,
    pub index_sync: IndexSync,
    pub said: Voice,
    pub work_tree: Tree,
    pub root: Tree,
    pub cloud_bg: Arc<Semaphore>,
    pub http: reqwest::Client,
    pub desk: Desk<Job>,
    pub title_bg: Arc<Semaphore>,
    pub titling: Arc<Mutex<BTreeSet<String>>>,
    pub next_seq: Arc<AtomicU64>,
    pub jobs_done: Counter,
    pub jobs_failed: Counter,
    pub manual: bool,
    pub from_state: RefineStatus,
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

    pub(super) async fn finish_recovered(self) {
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

    pub(super) async fn after_seal(self, sealed: Result<Option<About>, &'static str>) {
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
            }
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

    pub(super) async fn write_job(&self) {
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
                let Ok(permit) = self.cloud_bg.clone().acquire_owned().await else {
                    return Err(End::Done);
                };
                let waited_ms = wait_at.elapsed().as_millis() as u64;
                info!("job.start", kind = "write", job = seq, waited_ms = waited_ms);
                let started = std::time::Instant::now();
                let outcome = async {
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
                    match write::write_with(
                        &self.http,
                        MAX_TRANSCRIBE_WAIT_MS,
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
