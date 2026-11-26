//! Local background helpers (`local_bg` adoption, async export file writes, and progress setters).

use super::{IndexSync, now};
use crate::app::hub::Msg;
use crate::app::proto::{Progress, Writing};
use crate::record::adopt;
use crate::record::shelf::Shelf;
use crate::rt::queue::Tx;
use crate::rt::signal::Register;
use crate::rt::stats::Counter;
use crate::rt::supervise::Tree;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::Semaphore;
use zutalk_log::{info, warn};

pub(super) struct DeferredAdopt {
    pub local: Arc<Shelf>,
    pub person: Arc<Shelf>,
    pub exclude: Vec<String>,
}

pub(super) fn writing(id: &str, step: &str) -> Writing {
    Writing { id: id.into(), step: step.into(), done: 0, of: 0 }
}

pub(super) fn set_refine_progress(
    reg: &Register<Progress>,
    value: Option<Writing>,
    only_id: Option<&str>,
) {
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

pub(super) fn set_file_progress(
    reg: &Register<Progress>,
    value: Option<Writing>,
    only_id: Option<&str>,
) {
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

pub(super) fn set_move_progress(reg: &Register<Progress>, value: Option<Writing>) {
    reg.change(|cur| {
        if cur.move_old == value {
            return None;
        }
        Some(Progress { move_old: value.clone(), ..cur.clone() })
    });
}

pub(super) fn set_title_progress(
    reg: &Register<Progress>,
    value: Option<Writing>,
    only_id: Option<&str>,
) {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_adopt(
    tree: &Tree,
    local_bg: Arc<Semaphore>,
    local_lock: Arc<Mutex<()>>,
    next_seq: &Arc<AtomicU64>,
    jobs_done: &Counter,
    jobs_failed: &Counter,
    data: PathBuf,
    task: DeferredAdopt,
    progress_reg: Register<Progress>,
    index_sync: IndexSync,
    results: Tx<Msg>,
) {
    let seq = next_seq.fetch_add(1, Ordering::Relaxed);
    let tree_run = tree.clone();
    let jobs_done = jobs_done.clone();
    let jobs_failed = jobs_failed.clone();
    tree.spawn("job.adopt", async move {
        let wait_at = tokio::time::Instant::now();
        let Ok(_permit) = local_bg.acquire().await else {
            return;
        };
        let waited_ms = wait_at.elapsed().as_millis() as u64;
        info!("job.start", kind = "adopt", job = seq, waited_ms = waited_ms);
        let started = std::time::Instant::now();
        let reg_cb = progress_reg.clone();
        let data_run = data.clone();
        let local_run = task.local.clone();
        let person_run = task.person.clone();
        let exclude = task.exclude;
        let outcome = tree_run
            .blocking("job.adopt", move || {
                // Serialize against `Job::DiscardLocal` and post-seal `run_one` on the local shelf.
                let _guard = local_lock.lock().unwrap_or_else(PoisonError::into_inner);
                let res = adopt::run_with_progress(
                    &data_run,
                    &local_run,
                    &person_run,
                    &exclude,
                    now(),
                    |done, of, id| {
                        let writing =
                            Writing { id: id.to_owned(), step: "moving".into(), done, of };
                        set_move_progress(&reg_cb, Some(writing));
                    },
                );
                let status = adopt::status(&data_run, Some(&local_run), Some(person_run.by()));
                (res, status)
            })
            .await;
        set_move_progress(&progress_reg, None);
        let ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok((Ok(sum), status)) => {
                info!("job.done", kind = "adopt", job = seq, code = "ok", ms = ms);
                jobs_done.add(1);
                let changed = sum.plan == adopt::AdoptPlan::Adopt
                    && (sum.recordings > 0 || sum.terms > 0 || sum.topics > 0);
                if changed {
                    info!(
                        "identity.adopt",
                        recordings = sum.recordings,
                        terms = sum.terms,
                        code = "ok"
                    );
                    index_sync.adopted(task.person.by().to_owned()).await;
                }
                if sum.plan == adopt::AdoptPlan::Adopt {
                    let _ = results.send(Msg::LocalShelf(status)).await; // dropped: the hub has ended
                }
            }
            Ok((Err(c), _)) => {
                warn!("job.done", kind = "adopt", job = seq, code = c, ms = ms);
                warn!("identity.adopt", recordings = 0u32, terms = 0u32, code = c);
                jobs_failed.add(1);
            }
            Err(_) => {
                if tree_run.is_cancelled() {
                    return;
                }
                warn!("job.done", kind = "adopt", job = seq, code = "panic", ms = ms);
                jobs_failed.add(1);
            }
        }
    });
}

pub(super) fn spawn_export_write(
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
