//! Lock-free update checker and installer on the shell's supervision tree (`P1.md` §3.9, `P1-23`).
//!
//! One task (`updates`) owns the downloaded `(Update, Vec<u8>)` exclusively with no mutex.
//! The core's non-blocking `CheckUpdate` port wakes the task immediately, and the blocking
//! `Install` port sends a oneshot reply channel through a bounded channel (`shell.updates`,
//! `Full::Reject`) and waits for the reply; if installation succeeds, `app.restart()` ends the
//! process.

use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::oneshot;
use zutalk_core::app::hub::{CheckUpdate, Core, Install, UpdateReport};
use zutalk_core::rt::queue::{self, Full, Rx, Tx};
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::Tree;
use zutalk_log::{info, warn};

/// How long between looks for a newer version, after the one at start-up: six hours
/// (DESIGN.md, updates). Origin to be traced.
pub const LOOK_MS: u64 = 6 * 60 * 60 * 1_000;

/// Bounded shell update queue: holds manual check wake-ups plus at most one install request (`P1-23`).
const UPDATE_REQ_MSGS: usize = 4;

/// Request sent from the core's `CheckUpdate` or `Install` port to the `updates` task.
pub enum UpdateReq {
    Check,
    Install(oneshot::Sender<()>),
}

/// Sender handed to [`checker`] and [`installer`] once `Core::start` has created the shell's `Stats`.
pub type UpdateTx = Tx<UpdateReq>;

/// Builds the core's non-blocking `CheckUpdate` port backed by a lock-free `OnceLock`.
pub fn checker(tx: Arc<OnceLock<UpdateTx>>) -> CheckUpdate {
    Arc::new(move || {
        let Some(tx) = tx.get() else { return };
        let _ = tx.offer(UpdateReq::Check); // dropped: check already queued or shell shutting down
    })
}

/// Builds the core's blocking `Install` port backed by a lock-free `OnceLock` bound right after
/// `Core::start`.
pub fn installer(tx: Arc<OnceLock<UpdateTx>>) -> Install {
    Arc::new(move || {
        let Some(tx) = tx.get() else { return };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.offer(UpdateReq::Install(reply_tx)).is_ok() {
            let _ = reply_rx.blocking_recv(); // dropped: app restarts on success or task ended
        }
    })
}

/// Creates the bounded update request channel tracked under `stats`.
pub fn channel(stats: &Stats) -> (UpdateTx, Rx<UpdateReq>) {
    queue::bounded::<UpdateReq>(stats, "shell.updates", UPDATE_REQ_MSGS, Full::Reject)
}

/// Outcome of one check for a newer release, emitted as `update.look`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Ready,
    None,
    Failed,
}

/// Emits the `update.look` event for one update check (`P1.md` §3.13.3).
pub fn note_look(look: Look) {
    match look {
        Look::Ready => info!("update.look", code = "ready"),
        Look::None => info!("update.look", code = "none"),
        Look::Failed => warn!("update.look", code = "update"),
    }
}

/// Starts the `updates` task on `shell`. When `check_enabled` is false (dev builds), no automatic
/// check is made; manual checks report up-to-date and install requests reply immediately.
pub fn start(shell: &Tree, app: AppHandle, rx: Rx<UpdateReq>, check_enabled: bool) {
    shell.spawn("updates", run(app, rx, check_enabled));
}

async fn run(app: AppHandle, mut rx: Rx<UpdateReq>, check_enabled: bool) {
    let mut ready: Option<(Update, Vec<u8>)> = None;

    if !check_enabled {
        while let Some(req) = rx.recv().await {
            match req {
                UpdateReq::Check => {
                    note_look(Look::None);
                    app.state::<Core>().report_update(UpdateReport::UpToDate).await;
                }
                UpdateReq::Install(reply) => {
                    let _ = reply.send(()); // dropped: caller was cancelled
                }
            }
        }
        return;
    }

    loop {
        if ready.is_none() {
            ready = check_and_download(&app).await;
        }

        if let Some((ref update, _)) = ready {
            let version = update.version.clone();
            match rx.recv().await {
                Some(UpdateReq::Install(reply)) => try_install(&app, ready.as_ref(), reply),
                Some(UpdateReq::Check) => {
                    app.state::<Core>().report_update(UpdateReport::Ready(version)).await;
                }
                None => return,
            }
        } else {
            let sleep = tokio::time::sleep(Duration::from_millis(LOOK_MS));
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    () = &mut sleep => break,
                    req = rx.recv() => match req {
                        Some(UpdateReq::Check) => break,
                        Some(UpdateReq::Install(reply)) => try_install(&app, ready.as_ref(), reply),
                        None => return,
                    },
                }
            }
        }
    }
}

fn try_install(app: &AppHandle, ready: Option<&(Update, Vec<u8>)>, reply: oneshot::Sender<()>) {
    if let Some((update, bytes)) = ready
        && update.install(bytes).is_ok()
    {
        app.restart();
    }
    let _ = reply.send(()); // dropped: caller was cancelled
}

async fn check_and_download(app: &AppHandle) -> Option<(Update, Vec<u8>)> {
    app.state::<Core>().report_update(UpdateReport::Checking).await;
    let checked = match app.updater().map_err(|_| "update") {
        Ok(updater) => updater.check().await.map_err(|_| "update"),
        Err(code) => Err(code),
    };
    let update = match checked {
        Ok(Some(update)) => update,
        Ok(None) => {
            note_look(Look::None);
            app.state::<Core>().report_update(UpdateReport::UpToDate).await;
            return None;
        }
        Err(code) => {
            note_look(Look::Failed);
            app.state::<Core>().report_update(UpdateReport::Failed(code)).await;
            return None;
        }
    };

    let version = update.version.clone();
    app.state::<Core>().report_update(UpdateReport::Downloading(version.clone())).await;
    match update.download(|_, _| {}, || {}).await.map_err(|_| "update") {
        Ok(bytes) => {
            note_look(Look::Ready);
            app.state::<Core>().report_update(UpdateReport::Ready(version)).await;
            Some((update, bytes))
        }
        Err(code) => {
            note_look(Look::Failed);
            app.state::<Core>().report_update(UpdateReport::Failed(code)).await;
            None
        }
    }
}
