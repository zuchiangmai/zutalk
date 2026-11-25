//! Non-blocking window helpers: never query the main thread for state (`P1.md` §3.9, `P1-23`).

use tauri::{AppHandle, Manager};

/// Outcome of asking the OS to show or hide the captions window without waiting for a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionWindow {
    Ok,
    Missing,
    Failed,
}

impl CaptionWindow {
    /// Stable plain code recorded in `shell.captions`.
    pub const fn code(self) -> &'static str {
        match self {
            CaptionWindow::Ok => "ok",
            CaptionWindow::Missing => "missing",
            CaptionWindow::Failed => "failed",
        }
    }
}

/// Posts a show or hide request to the `captions` window without querying visibility on the main thread.
pub fn set_captions(app: &AppHandle, show: bool) -> CaptionWindow {
    let Some(window) = app.get_webview_window("captions") else {
        return CaptionWindow::Missing;
    };
    let result = if show { window.show() } else { window.hide() };
    if result.is_ok() { CaptionWindow::Ok } else { CaptionWindow::Failed }
}
