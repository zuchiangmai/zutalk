//! The shell: thin wiring between Tauri, the supervised `shell` tree, and `zutalk-core`
//! (`P1.md` §3.9, `P1-23`, `P1-24`). All commands are async, nothing holds a lock, and nothing
//! waits on the main thread.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::{Manager, RunEvent, State, WebviewWindow, WindowEvent, ipc::Channel};
use tauri_plugin_clipboard_manager::ClipboardExt;
use zutalk::pump::{self, Joins, SHELL_JOINS_MSGS};
use zutalk::{beat, updates, windows};
use zutalk_core::app::hub::{Clip, Config, Core};
use zutalk_core::app::proto::{Command, Event, Query, QueryReply};
use zutalk_core::id::keys;
use zutalk_core::out::host::HOST_EXIT_WAIT_MS;
use zutalk_core::rt::queue::{self, Full};
use zutalk_core::rt::stats::{Counter, Spread};
use zutalk_core::rt::supervise::Tree;
use zutalk_log::{LOG_FLUSH_WAIT_MS, info};

struct UiVitals {
    gap_ms: Spread,
    gaps: Counter,
}

#[tauri::command]
async fn dispatch(command: Command, core: State<'_, Core>) -> Result<(), String> {
    core.send(command).map_err(str::to_owned)
}

#[tauri::command]
async fn query(query: Query, core: State<'_, Core>) -> Result<QueryReply, String> {
    core.query(query).await.map_err(str::to_owned)
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
    beat::note_frame_gap(window.label(), gap_ms, gaps, &vitals.gap_ms, &vitals.gaps);
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
            let state_reading = outlet.state.clone();
            let sleep_reading = outlet.state.clone();
            let handle_pushed = app.handle().clone();
            let handle_captions = app.handle().clone();
            let _kept = pump::start(
                &shell,
                outlet,
                joins_rx,
                move |seq| handle_pushed.state::<Core>().pushed(seq),
                move |ops| windows::apply_captions(&handle_captions, ops),
            );
            if let Some(cap_win) = app.get_webview_window("captions") {
                let (geo_tx, geo_rx) =
                    tokio::sync::watch::channel::<Option<windows::GeometrySample>>(None);
                let close_handle = app.handle().clone();
                cap_win.on_window_event(move |event| match event {
                    WindowEvent::Moved(pos) => {
                        geo_tx.send_modify(|slot| {
                            slot.get_or_insert_with(Default::default).merge_move(pos.x, pos.y);
                        });
                    }
                    WindowEvent::Resized(size) => {
                        geo_tx.send_modify(|slot| {
                            slot.get_or_insert_with(Default::default)
                                .merge_resize(size.width, size.height);
                        });
                    }
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let sent =
                            close_handle.state::<Core>().send(Command::Captions { show: false });
                        let _ = sent; // dropped: core closed on exit
                    }
                    _ => {}
                });
                let geo_handle = app.handle().clone();
                shell.spawn(
                    "shell.captions_geo",
                    windows::run_geometry_saver(geo_rx, state_reading, move |cmd| {
                        let _ = geo_handle.state::<Core>().send(cmd); // dropped: core closed on exit
                    }),
                );
            }
            let beat_handle = app.handle().clone();
            shell.spawn("shell.captions_sleep", windows::run_display_sleep(sleep_reading));
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
        .invoke_handler(tauri::generate_handler![dispatch, subscribe, drawn, query])
        .build(tauri::generate_context!());
    match built {
        // On clean exit while hosting, stop outbound asynchronously before exiting; always flush logs.
        Ok(app) => {
            let exiting = Arc::new(AtomicBool::new(false));
            app.run(move |handle, event| match event {
                RunEvent::ExitRequested { api, .. } => {
                    if !exiting.swap(true, Ordering::AcqRel) {
                        let core = handle.state::<Core>();
                        if core.is_hosting() {
                            api.prevent_exit();
                            let _ = core.send(Command::StopOutbound); // dropped: core closed on exit
                            let exit_handle = handle.clone();
                            handle.state::<Tree>().spawn("shell.exit_host", async move {
                                exit_handle.state::<Core>().wait_host_idle(HOST_EXIT_WAIT_MS).await;
                                exit_handle.exit(0);
                            });
                        }
                    }
                }
                RunEvent::Exit => {
                    let _ = zutalk_log::flush(LOG_FLUSH_WAIT_MS); // dropped: quitting goes on anyway
                }
                _ => {}
            });
        }
        Err(error) => {
            eprintln!("ZuTalk could not start: {error}"); // before the log: it may not be there
            std::process::exit(1);
        }
    }
}
