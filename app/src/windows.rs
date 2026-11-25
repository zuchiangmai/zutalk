//! Non-blocking caption window state machine, lock-free geometry debouncer, and Tauri adapter
//! (`P1.md` §3.9, `P5.md` §3.4, `P5-03`).
//!
//! Never holds locks, never calls `block_on`, and never queries main-thread window properties
//! (`inner_size`, `outer_position`, `scale_factor`, etc. — enforced by `shell.sh`).

use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::watch;
use zutalk_core::app::proto::{CaptionScene, Command, State, WindowRect};
use zutalk_core::rt::signal::Reading;

/// Wait after the last OS move/resize event before persisting caption window geometry (`P5-03`).
pub const CAPTION_GEO_DEBOUNCE_MS: u64 = 400;

/// Outcome of applying caption window operations without waiting for a main-thread reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionWindow {
    Ok,
    Missing,
    Failed,
}

impl CaptionWindow {
    /// Stable plain code recorded in `shell.captions` and `shell.captions.style`.
    pub const fn code(self) -> &'static str {
        match self {
            CaptionWindow::Ok => "ok",
            CaptionWindow::Missing => "missing",
            CaptionWindow::Failed => "failed",
        }
    }
}

/// Desired OS-level state of the standalone captions window derived from bounded `State`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionWindowSpec {
    pub shown: bool,
    pub scene: CaptionScene,
    pub always_on_top: bool,
    pub click_through: bool,
    pub fullscreen: bool,
    pub geometry: Option<WindowRect>,
}

impl CaptionWindowSpec {
    pub fn from_state(state: &State) -> Self {
        let scene = state.setup.caption.scene;
        let style = state.setup.caption.active();
        Self {
            shown: state.captions,
            scene,
            always_on_top: style.always_on_top,
            click_through: style.click_through,
            fullscreen: style.fullscreen,
            geometry: style.geometry,
        }
    }
}

/// Atomic OS window operation emitted in deterministic order by [`CaptionWindowTracker::diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptionWindowOp {
    SetFullscreen(bool),
    SetSize { width: u32, height: u32 },
    SetPosition { x: i32, y: i32 },
    SetAlwaysOnTop(bool),
    SetIgnoreCursorEvents(bool),
    Show,
    Hide,
}

/// Pure diff tracker for the captions window: emits only changed properties in safe order and
/// suppresses duplicate geometry writes so OS `Moved`/`Resized` events never feedback-loop.
#[derive(Debug, Default, Clone)]
pub struct CaptionWindowTracker {
    applied: Option<CaptionWindowSpec>,
}

impl CaptionWindowTracker {
    pub fn new() -> Self {
        Self { applied: None }
    }

    /// Computes the ordered window operations needed to move from the last applied spec to `next`.
    pub fn diff(&mut self, next: CaptionWindowSpec) -> Vec<CaptionWindowOp> {
        let mut ops = Vec::new();
        let Some(prev) = self.applied.as_ref() else {
            if !next.shown {
                self.applied = Some(next);
                return ops;
            }
            push_show_ops(&mut ops, &next);
            self.applied = Some(next);
            return ops;
        };

        if !next.shown {
            if prev.shown {
                if prev.fullscreen {
                    ops.push(CaptionWindowOp::SetFullscreen(false));
                }
                ops.push(CaptionWindowOp::Hide);
            }
            self.applied = Some(next);
            return ops;
        }

        if !prev.shown {
            push_show_ops(&mut ops, &next);
            self.applied = Some(next);
            return ops;
        }

        // Window is already visible: diff fullscreen, geometry, always-on-top, and click-through.
        match (prev.fullscreen, next.fullscreen) {
            (false, true) => {
                ops.push(CaptionWindowOp::SetFullscreen(true));
            }
            (true, false) => {
                ops.push(CaptionWindowOp::SetFullscreen(false));
                if let Some(rect) = next.geometry {
                    ops.push(CaptionWindowOp::SetSize { width: rect.width, height: rect.height });
                    ops.push(CaptionWindowOp::SetPosition { x: rect.x, y: rect.y });
                }
            }
            (false, false) => {
                if next.geometry != prev.geometry
                    && let Some(rect) = next.geometry
                {
                    let prev_size = prev.geometry.map(|g| (g.width, g.height));
                    let prev_pos = prev.geometry.map(|g| (g.x, g.y));
                    if prev_size != Some((rect.width, rect.height)) {
                        ops.push(CaptionWindowOp::SetSize {
                            width: rect.width,
                            height: rect.height,
                        });
                    }
                    if prev_pos != Some((rect.x, rect.y)) {
                        ops.push(CaptionWindowOp::SetPosition { x: rect.x, y: rect.y });
                    }
                }
            }
            (true, true) => {
                // Ignore geometry updates while staying in fullscreen.
            }
        }

        if next.always_on_top != prev.always_on_top {
            ops.push(CaptionWindowOp::SetAlwaysOnTop(next.always_on_top));
        }
        if next.click_through != prev.click_through {
            ops.push(CaptionWindowOp::SetIgnoreCursorEvents(next.click_through));
        }

        self.applied = Some(next);
        ops
    }
}

fn push_show_ops(ops: &mut Vec<CaptionWindowOp>, next: &CaptionWindowSpec) {
    if next.fullscreen {
        ops.push(CaptionWindowOp::SetFullscreen(true));
    } else if let Some(rect) = next.geometry {
        ops.push(CaptionWindowOp::SetSize { width: rect.width, height: rect.height });
        ops.push(CaptionWindowOp::SetPosition { x: rect.x, y: rect.y });
    }
    ops.push(CaptionWindowOp::SetAlwaysOnTop(next.always_on_top));
    ops.push(CaptionWindowOp::SetIgnoreCursorEvents(next.click_through));
    ops.push(CaptionWindowOp::Show);
}

/// Coalesced physical-pixel window coordinates captured from OS `Moved`/`Resized` events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GeometrySample {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl GeometrySample {
    pub fn merge_move(&mut self, x: i32, y: i32) {
        self.x = Some(x);
        self.y = Some(y);
    }

    pub fn merge_resize(&mut self, width: u32, height: u32) {
        self.width = Some(width);
        self.height = Some(height);
    }

    /// Returns a clamped [`WindowRect`] once both position and size have been observed.
    pub fn to_rect(&self) -> Option<WindowRect> {
        Some(
            WindowRect { x: self.x?, y: self.y?, width: self.width?, height: self.height? }.clamp(),
        )
    }
}

/// Lock-free debouncer for OS caption-window `Moved`/`Resized` events: waits
/// `CAPTION_GEO_DEBOUNCE_MS` after the latest event, then persists the merged `WindowRect` for
/// the active scene iff the captions window is shown, not fullscreen, and actually changed.
pub async fn run_geometry_saver<F>(
    mut samples: watch::Receiver<Option<GeometrySample>>,
    state: Reading<State>,
    send_cmd: F,
) where
    F: Fn(Command) + Send + 'static,
{
    while samples.changed().await.is_ok() {
        loop {
            let sleep = tokio::time::sleep(Duration::from_millis(CAPTION_GEO_DEBOUNCE_MS));
            tokio::pin!(sleep);
            tokio::select! {
                changed = samples.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = &mut sleep => {
                    let sample = *samples.borrow_and_update();
                    if let Some(rect) = sample.and_then(|s| s.to_rect()) {
                        let current = state.now().1;
                        let scene = current.setup.caption.scene;
                        let active = current.setup.caption.active();
                        if current.captions && !active.fullscreen && active.geometry != Some(rect) {
                            send_cmd(Command::CaptionGeometry { scene, rect });
                        }
                    }
                    break;
                }
            }
        }
    }
}

/// Applies ordered [`CaptionWindowOp`]s to the `captions` window using physical coordinates
/// directly (matching `WindowEvent::Moved`/`Resized` without ever calling `scale_factor()`).
pub fn apply_captions(app: &AppHandle, ops: &[CaptionWindowOp]) -> CaptionWindow {
    let Some(window) = app.get_webview_window("captions") else {
        return CaptionWindow::Missing;
    };
    let mut ok = true;
    for op in ops {
        let res = match *op {
            CaptionWindowOp::SetFullscreen(full) => window.set_fullscreen(full),
            CaptionWindowOp::SetSize { width, height } => {
                window.set_size(tauri::PhysicalSize::new(width, height))
            }
            CaptionWindowOp::SetPosition { x, y } => {
                window.set_position(tauri::PhysicalPosition::new(x, y))
            }
            CaptionWindowOp::SetAlwaysOnTop(top) => window.set_always_on_top(top),
            CaptionWindowOp::SetIgnoreCursorEvents(pass) => window.set_ignore_cursor_events(pass),
            CaptionWindowOp::Show => window.show(),
            CaptionWindowOp::Hide => window.hide(),
        };
        if res.is_err() {
            ok = false;
        }
    }
    if ok { CaptionWindow::Ok } else { CaptionWindow::Failed }
}
