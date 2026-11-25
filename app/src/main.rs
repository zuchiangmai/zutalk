//! The shell: thin wiring between Tauri, the supervised `shell` tree, and `zutalk-core`
//! (`P1.md` §3.9, `P1-23`, `P1-24`). All commands are async, nothing holds a lock, and nothing
//! waits on the main thread.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, OnceLock};
use tauri::{Manager, RunEvent, State, WebviewWindow, ipc::Channel};
use tauri_plugin_clipboard_manager::ClipboardExt;
use zutalk::pump::{self, Joins, SHELL_JOINS_MSGS};
use zutalk::{beat, updates, windows};
use zutalk_core::app::hub::{Clip, Config, Core};
use zutalk_core::app::proto::{Command, Event};
use zutalk_core::id::keys;
use zutalk_core::rt::queue::{self, Full};
use zutalk_core::rt::stats::{Counter, Spread};
use zutalk_core::rt::supervise::Tree;
use zutalk_log::{Id, LOG_FLUSH_WAIT_MS, info, warn};

struct UiVitals {
    gap_ms: Spread,
    gaps: Counter,
}

#[tauri::command]
async fn dispatch(command: Command, core: State<'_, Core>) -> Result<(), String> {
    core.send(command).map_err(str::to_owned)
}

#[tauri::command]
async fn subscribe(
    window: WebviewWindow,
    channel: Channel<Event>,
    joins: State<'_, Joins<Channel<Event>>>,
) -> Result<(), String> {
    joins.send((window.label().to_owned(), channel)).await.map_err(|_| "busy".to_owned())
}

#[tauri::command(rename_all = "snake_case")]
async fn drawn(
    window: WebviewWindow,
    seq: u64,
    gap_ms: u32,
    gaps: u32,
    core: State<'_, Core>,
    vitals: State<'_, UiVitals>,
) -> Result<(), String> {
    core.drawn(seq);
    if gaps > 0 {
        warn!("ui.frame.gap", window = Id(window.label()), gap_ms = gap_ms, n = gaps);
        vitals.gap_ms.note(u64::from(gap_ms));
        vitals.gaps.add(u64::from(gaps));
    }
    Ok(())
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

            // Enter Tauri's Tokio runtime synchronously: Core::start and Tree only spawn tasks.
            let runtime = tauri::async_runtime::handle();
            let _entered = runtime.inner().enter();

            let install_slot = Arc::new(OnceLock::new());
            let install = updates::installer(install_slot.clone());
            let config = Config { old, clip, install, ..base };
            let (core, outlet) = Core::start(config).map_err(|end| format!("{end:?}"))?;
            let shell = Tree::root("shell", core.stats());
            let ui_vitals = UiVitals {
                gap_ms: shell.stats().spread("ui.gap_ms"),
                gaps: shell.stats().counter("ui.gaps"),
            };
            let (update_tx, update_rx) = updates::channel(shell.stats());
            let _ = install_slot.set(update_tx); // dropped: initialized once before any Install call
            let (joins_tx, joins_rx) = queue::bounded::<(String, Channel<Event>)>(
                shell.stats(),
                "shell.joins",
                SHELL_JOINS_MSGS,
                Full::Wait,
            );
            // Manage Core before starting shell tasks so handle.state::<Core>() is always ready.
            app.manage(core);
            app.manage(ui_vitals);
            let handle_pushed = app.handle().clone();
            let handle_captions = app.handle().clone();
            let _kept = pump::start(
                &shell,
                outlet,
                joins_rx,
                move |seq| handle_pushed.state::<Core>().pushed(seq),
                move |show| windows::set_captions(&handle_captions, show),
            );
            let beat_handle = app.handle().clone();
            shell.spawn(
                "shell.beat",
                beat::run(
                    move |work| beat_handle.run_on_main_thread(work).is_ok(),
                    shell.stats().clone(),
                ),
            );
            updates::start(&shell, app.handle().clone(), update_rx, !tauri::is_dev());
            let _ = app.manage(shell); // dropped: `false` only if one was managed already
            app.manage(joins_tx);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![dispatch, subscribe, drawn])
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
