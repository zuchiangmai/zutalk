//! Lock-free update checker and installer on the shell's supervision tree (`P1.md` §3.9, `P1-23`).
//!
//! One task (`updates`) owns the downloaded `(Update, Vec<u8>)` exclusively with no mutex.
//! The core's blocking `Install` port sends a oneshot reply channel through a 1-slot bounded
//! channel (`shell.updates`, `Full::Reject`) and waits for the reply; if installation succeeds,
//! `app.restart()` ends the process.

use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::oneshot;
use zutalk_core::app::hub::{Core, Install};
use zutalk_core::rt::queue::{self, Full, Rx, Tx};
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::Tree;
use zutalk_log::{info, warn};

/// How long between looks for a newer version, after the one at start-up: six hours
/// (DESIGN.md, updates). Origin to be traced.
pub const LOOK_MS: u64 = 6 * 60 * 60 * 1_000;

/// At most one update install request in flight; the core refuses a second while `Boot::Installing` (`P1-23`).
const UPDATE_INSTALL_MSGS: usize = 1;

/// Sender handed to [`installer`] once `Core::start` has created the shell's `Stats`.
pub type InstallTx = Tx<oneshot::Sender<()>>;

/// Builds the core's blocking `Install` port backed by a lock-free `OnceLock` bound right after
/// `Core::start`.
pub fn installer(tx: Arc<OnceLock<InstallTx>>) -> Install {
    Arc::new(move || {
        let Some(tx) = tx.get() else { return };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.offer(reply_tx).is_ok() {
            let _ = reply_rx.blocking_recv(); // dropped: app restarts on success or task ended
        }
    })
}

/// Creates the 1-slot bounded install channel tracked under `stats`.
pub fn channel(stats: &Stats) -> (InstallTx, Rx<oneshot::Sender<()>>) {
    queue::bounded::<oneshot::Sender<()>>(stats, "shell.updates", UPDATE_INSTALL_MSGS, Full::Reject)
}

/// Starts the `updates` task on `shell`. When `check_enabled` is false (dev builds), no remote
/// check is made and any install request replies immediately.
pub fn start(shell: &Tree, app: AppHandle, rx: Rx<oneshot::Sender<()>>, check_enabled: bool) {
    shell.spawn("updates", run(app, rx, check_enabled));
}

async fn run(app: AppHandle, mut rx: Rx<oneshot::Sender<()>>, check_enabled: bool) {
    let mut ready: Option<(Update, Vec<u8>)> = None;
    let mut told = false;

    if !check_enabled {
        while let Some(reply) = rx.recv().await {
            let _ = reply.send(()); // dropped: caller was cancelled
        }
        return;
    }

    loop {
        if ready.is_none() {
            match fetch(&app).await {
                Ok(Some((update, bytes))) => {
                    let version = update.version.clone();
                    ready = Some((update, bytes));
                    info!("update.look", code = "ready");
                    app.state::<Core>().looked(Ok(version)).await;
                }
                Ok(None) => {
                    told = false;
                    info!("update.look", code = "none");
                }
                Err(_) => {
                    warn!("update.look", code = "update");
                    if !told {
                        told = true;
                        app.state::<Core>().looked(Err("update")).await;
                    }
                }
            }
        }

        if ready.is_some() {
            let Some(reply) = rx.recv().await else { return };
            try_install(&app, ready.as_ref(), reply);
        } else {
            let sleep = tokio::time::sleep(Duration::from_millis(LOOK_MS));
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    () = &mut sleep => break,
                    req = rx.recv() => match req {
                        Some(reply) => try_install(&app, ready.as_ref(), reply),
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

async fn fetch(app: &AppHandle) -> tauri_plugin_updater::Result<Option<(Update, Vec<u8>)>> {
    let Some(update) = app.updater()?.check().await? else { return Ok(None) };
    let bytes = update.download(|_, _| {}, || {}).await?;
    Ok(Some((update, bytes)))
}
