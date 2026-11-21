//! The shell: the windows, the bridge between the interface and the core, and the app's own
//! updates. It decides nothing: it carries commands in and events out, fetches a newer version
//! for the core to offer, and installs it when the core says so.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, RunEvent, State, ipc::Channel};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use zutalk_core::app::hub::{Clip, Config, Core, Install};
use zutalk_core::app::proto::{Command, Event};
use zutalk_core::id::keys;
use zutalk_log::{LOG_FLUSH_WAIT_MS, info};

/// The windows listening, and the last state: a window that opens late is sent it first.
type Windows = Arc<Mutex<(Vec<Channel<Event>>, Option<Event>)>>;
/// A newer version, downloaded, its signature checked against the key in the config.
type Ready = Arc<Mutex<Option<(Update, Vec<u8>)>>>;
/// How long between looks for a newer version, after the one at start-up.
const LOOK: Duration = Duration::from_secs(6 * 60 * 60);

#[tauri::command]
fn dispatch(command: Command, core: State<Core>) {
    core.send(command);
}

#[tauri::command]
fn subscribe(channel: Channel<Event>, windows: State<Windows>) {
    let Ok(mut windows) = windows.lock() else { return };
    windows.1.iter().for_each(|last| drop(channel.send(last.clone())));
    windows.0.push(channel);
}

/// Every event to every window. The captions window is on screen while the state says so.
fn fan(app: &AppHandle, windows: &Windows, event: Event) {
    let Ok(mut windows) = windows.lock() else { return };
    if let Event::State(state) = &event {
        let captions = app.get_webview_window("captions");
        if let Some(window) = captions.filter(|w| w.is_visible().ok() != Some(state.captions)) {
            let _ = if state.captions { window.show() } else { window.hide() };
        }
        windows.1 = Some(event.clone());
    }
    windows.0.retain(|channel| channel.send(event.clone()).is_ok());
}

/// Looks for a newer version at start-up and every `LOOK` until one is downloaded, then tells
/// the core. Trouble is told once, until a look goes well.
async fn look(app: AppHandle, ready: Ready) {
    let mut told = false;
    loop {
        match fetch(&app).await {
            Ok(Some((update, bytes))) => {
                let version = update.version.clone();
                if let Ok(mut ready) = ready.lock() {
                    *ready = Some((update, bytes));
                }
                return app.state::<Core>().looked(Ok(version));
            }
            Ok(None) => told = false,
            Err(_) if told => {}
            Err(_) => {
                told = true;
                app.state::<Core>().looked(Err("update"));
            }
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// A newer version, if there is one, downloaded and its signature checked.
async fn fetch(app: &AppHandle) -> tauri_plugin_updater::Result<Option<(Update, Vec<u8>)>> {
    let Some(update) = app.updater()?.check().await? else { return Ok(None) };
    let bytes = update.download(|_, _| {}, || {}).await?;
    Ok(Some((update, bytes)))
}

/// Puts what is ready in place of the app and relaunches it. A version that failed to install
/// stays ready, to be tried again.
fn installer(app: AppHandle, ready: Ready) -> Install {
    Arc::new(move || {
        let Ok(ready) = ready.lock() else { return };
        if let Some((update, bytes)) = ready.as_ref()
            && update.install(bytes).is_ok()
        {
            app.restart();
        }
    })
}

fn main() {
    let built = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init()) // the paths of an import and an export
        // For the core alone: the page has no permission to the clipboard through it.
        .plugin(tauri_plugin_clipboard_manager::init())
        // For the shell alone: the page can neither look for a version nor install one.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            // Our server, which relays sign-ins, issues keys and keeps rooms. Set when building.
            let server = option_env!("ZUTALK_SERVER").unwrap_or("https://localhost:8443");
            let base = Config::new(data.clone(), server).from_env();
            // The log first, so that all after it is said; without one the app still starts.
            match zutalk_log::install(&base.logs(), "zutalk", base.log, cfg!(debug_assertions)) {
                Ok(guard) => {
                    let _ = app.manage(guard); // dropped: `false` only if one was managed already
                }
                Err(error) => eprintln!("ZuTalk has no log: {error}"), // before the log: none
            }
            info!(
                "app.start",
                version = env!("CARGO_PKG_VERSION"),
                os = std::env::consts::OS,
                arch = std::env::consts::ARCH,
                level = base.log.code()
            );
            keys::open_store(&data).map_err(|end| format!("{end:?}"))?;
            // Where the 0.8 app, which ran on macOS alone, kept its data.
            let old = cfg!(target_os = "macos").then_some(app.path().data_dir()?.join("ZuTalk"));
            let clipboard = app.handle().clone();
            let clip: Clip = Arc::new(move |text| clipboard.clipboard().write_text(text).is_ok());
            let ready = Ready::default();
            let install = installer(app.handle().clone(), ready.clone());
            let config = Config { old, clip, install, ..base };
            let (handle, windows) = (app.handle().clone(), Windows::default());
            let theirs = windows.clone();
            let out = move |event| fan(&handle, &theirs, event);
            // The core runs on the shell's runtime.
            let core = tauri::async_runtime::block_on(async { Core::start(config, out) });
            app.manage(core.map_err(|end| format!("{end:?}"))?);
            app.manage(windows);
            // A build that is not a release has no installed app to put a version in place of.
            if !tauri::is_dev() {
                tauri::async_runtime::spawn(look(app.handle().clone(), ready));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![dispatch, subscribe])
        .build(tauri::generate_context!());
    match built {
        // What the log has not written yet is written before the process ends.
        Ok(app) => app.run(|_, event| {
            if let RunEvent::Exit = event {
                let _ = zutalk_log::flush(LOG_FLUSH_WAIT_MS); // dropped: quitting goes on anyway
            }
        }),
        Err(error) => {
            eprintln!("ZuTalk could not start: {error}"); // before the log: it may not be there
            std::process::exit(1);
        }
    }
}
