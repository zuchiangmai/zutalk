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
use zutalk_core::rt::supervise::Tree;

/// Wait after the last OS move/resize event before persisting caption window geometry (`P5-03`).
pub const CAPTION_GEO_DEBOUNCE_MS: u64 = 400;
/// Maximum physical-pixel delta treated as OS/Retina clamping jitter rather than a user move/resize.
const GEOMETRY_JITTER_PX: u32 = 2;
/// Default floating `SelfView` width in logical/Cocoa points (`tauri.conf.json`).
pub const DEFAULT_SELF_VIEW_WIDTH_PT: f64 = 900.0;
/// Default floating `SelfView` height in logical/Cocoa points (`tauri.conf.json`).
pub const DEFAULT_SELF_VIEW_HEIGHT_PT: f64 = 220.0;
/// Bottom margin above Dock / visible frame edge for default `SelfView` placement in points.
const DEFAULT_SELF_VIEW_BOTTOM_MARGIN_PT: f64 = 48.0;
/// Minimum height of the `Band` banner strip in points.
const MIN_BAND_HEIGHT_PT: f64 = 120.0;

/// Usable screen rectangle in macOS Cocoa points (bottom-left origin) or logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPoints {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Pure Cocoa-point frame layout for unplaced scenes (`geometry == None`), handling negative-origin
/// secondary monitors and mixed-DPI setups without WebView `devicePixelRatio` coordinate drift.
pub fn default_scene_frame_cocoa(
    visible: ScreenPoints,
    scene: CaptionScene,
    top: bool,
) -> ScreenPoints {
    match scene {
        CaptionScene::Stage => visible,
        CaptionScene::SelfView => {
            let width = DEFAULT_SELF_VIEW_WIDTH_PT.min(visible.width).max(320.0);
            let height = DEFAULT_SELF_VIEW_HEIGHT_PT.min(visible.height).max(96.0);
            let x = visible.x + (visible.width - width) * 0.5;
            let margin = DEFAULT_SELF_VIEW_BOTTOM_MARGIN_PT.min((visible.height - height).max(0.0));
            let y = visible.y + margin;
            ScreenPoints { x, y, width, height }
        }
        CaptionScene::Band => {
            let width = visible.width.max(320.0);
            let min_h = MIN_BAND_HEIGHT_PT.min(visible.height);
            let max_h = (visible.height * 0.5).max(min_h);
            let height = (visible.height * 0.25).clamp(min_h, max_h);
            let x = visible.x;
            let y = if top { visible.y + visible.height - height } else { visible.y };
            ScreenPoints { x, y, width, height }
        }
    }
}

fn rect_close(a: WindowRect, b: WindowRect) -> bool {
    a.x.abs_diff(b.x) <= GEOMETRY_JITTER_PX
        && a.y.abs_diff(b.y) <= GEOMETRY_JITTER_PX
        && a.width.abs_diff(b.width) <= GEOMETRY_JITTER_PX
        && a.height.abs_diff(b.height) <= GEOMETRY_JITTER_PX
}

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
    pub top: bool,
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
            top: style.top,
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
    PlaceDefault { scene: CaptionScene, top: bool },
    SetSize { width: u32, height: u32 },
    SetPosition { x: i32, y: i32 },
    SetAlwaysOnTop(bool),
    SetIgnoreCursorEvents(bool),
    Show,
    Hide,
}

/// Idempotent operation sequence for explicit user/OS close actions (`Command::Captions { show: false }`
/// and `WindowEvent::CloseRequested`), guaranteeing the OS window exits fullscreen and hides even
/// if `State.captions` was already `false`.
pub const FORCE_HIDE_OPS: &[CaptionWindowOp] =
    &[CaptionWindowOp::SetFullscreen(false), CaptionWindowOp::Hide];

/// macOS `NSWindowLevel` target computed purely from caption window state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacWindowLevel {
    Normal,
    Floating,
    Status,
}

/// macOS Spaces & Cmd+` cycle behavior computed purely from caption window state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacCollectionMode {
    /// Moves with the active Space and joins fullscreen Spaces as an auxiliary overlay.
    ActiveSpaceAuxiliary,
    /// Appears across all Spaces, joins fullscreen Spaces, and is excluded from Cmd+` cycling.
    AllSpacesPinned,
}

/// Pure macOS AppKit window policy (`CLAUDE.md` Principle 4: rules are values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacCaptionPolicy {
    pub level: MacWindowLevel,
    pub collection: MacCollectionMode,
    pub maximize_to_visible_frame: bool,
    pub order_front: bool,
}

/// Computes the AppKit window policy for the standalone captions window.
///
/// `order_front` is strictly gated on `currently_shown`: calling `-[NSWindow orderFrontRegardless]`
/// on an ordered-out (`"visible": false`) window forces macOS WindowServer to map it onto the
/// screen immediately while `State.captions` is still `false`.
pub const fn macos_caption_policy(
    currently_shown: bool,
    pinned: bool,
    fullscreen: bool,
) -> MacCaptionPolicy {
    if fullscreen {
        MacCaptionPolicy {
            level: MacWindowLevel::Floating,
            collection: MacCollectionMode::ActiveSpaceAuxiliary,
            maximize_to_visible_frame: true,
            order_front: currently_shown,
        }
    } else if pinned {
        MacCaptionPolicy {
            level: MacWindowLevel::Status,
            collection: MacCollectionMode::AllSpacesPinned,
            maximize_to_visible_frame: false,
            order_front: currently_shown,
        }
    } else {
        MacCaptionPolicy {
            level: MacWindowLevel::Normal,
            collection: MacCollectionMode::ActiveSpaceAuxiliary,
            maximize_to_visible_frame: false,
            order_front: false,
        }
    }
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
        let mut emitted_top = false;
        match (prev.fullscreen, next.fullscreen) {
            (false, true) => {
                ops.push(CaptionWindowOp::SetFullscreen(true));
            }
            (true, false) => {
                ops.push(CaptionWindowOp::SetFullscreen(false));
                if let Some(rect) = next.geometry {
                    ops.push(CaptionWindowOp::SetSize { width: rect.width, height: rect.height });
                    ops.push(CaptionWindowOp::SetPosition { x: rect.x, y: rect.y });
                } else {
                    ops.push(CaptionWindowOp::PlaceDefault { scene: next.scene, top: next.top });
                }
                // Always re-apply `always_on_top` after leaving macOS fullscreen so both pinned
                // (`NSStatusWindowLevel`) and unpinned (`NSNormalWindowLevel`) levels are restored.
                ops.push(CaptionWindowOp::SetAlwaysOnTop(next.always_on_top));
                emitted_top = true;
            }
            (false, false) => {
                if let Some(rect) = next.geometry {
                    if next.geometry != prev.geometry
                        && !prev.geometry.is_some_and(|g| rect_close(g, rect))
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
                } else if prev.geometry.is_some()
                    || next.scene != prev.scene
                    || (next.scene == CaptionScene::Band && next.top != prev.top)
                {
                    ops.push(CaptionWindowOp::PlaceDefault { scene: next.scene, top: next.top });
                }
            }
            (true, true) => {
                // Ignore geometry updates while staying in fullscreen.
            }
        }

        if !emitted_top && !next.fullscreen && next.always_on_top != prev.always_on_top {
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
    if !next.fullscreen {
        if let Some(rect) = next.geometry {
            ops.push(CaptionWindowOp::SetSize { width: rect.width, height: rect.height });
            ops.push(CaptionWindowOp::SetPosition { x: rect.x, y: rect.y });
        } else {
            ops.push(CaptionWindowOp::PlaceDefault { scene: next.scene, top: next.top });
        }
    }
    ops.push(CaptionWindowOp::SetIgnoreCursorEvents(next.click_through));
    ops.push(CaptionWindowOp::Show);
    if next.fullscreen {
        ops.push(CaptionWindowOp::SetFullscreen(true));
    } else {
        // Apply always-on-top after Show so macOS preserves the floating window level, all-Spaces
        // behavior, and front ordering above other apps and fullscreen Spaces.
        ops.push(CaptionWindowOp::SetAlwaysOnTop(next.always_on_top));
    }
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

/// Configures static creation-time invariants on the standalone captions window at startup without
/// touching dynamic visibility or window ordering (which belong exclusively to `CaptionWindowTracker`).
pub fn prepare_captions_window(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let win = window.clone();
        let dispatch = window.run_on_main_thread(move || {
            use objc2_app_kit::NSWindow;

            let Ok(raw) = win.ns_window() else {
                return;
            };
            if raw.is_null() {
                return;
            }
            // SAFETY: `tauri::WebviewWindow::ns_window` returns a valid `NSWindow` pointer owned by
            // Tauri for the lifetime of the app, and this closure runs strictly on the main thread.
            let ns_win = unsafe { &*(raw as *const NSWindow) };
            ns_win.setHidesOnDeactivate(false);
            let _: () = unsafe { objc2::msg_send![ns_win, setRestorable: false] };
            ns_win.orderOut(None);
        });
        let _ = dispatch; // dropped: fire-and-forget startup invariant setup fails only during shutdown
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window; // dropped: non-macOS platforms need no AppKit startup setup
}

#[cfg(target_os = "macos")]
fn apply_macos_caption_policy_on_main(
    window: &tauri::WebviewWindow,
    pinned: bool,
    fullscreen: bool,
) {
    use objc2_app_kit::{
        NSFloatingWindowLevel, NSNormalWindowLevel, NSScreen, NSStatusWindowLevel, NSWindow,
        NSWindowCollectionBehavior,
    };

    let Ok(raw) = window.ns_window() else {
        return;
    };
    if raw.is_null() {
        return;
    }
    // SAFETY: `tauri::WebviewWindow::ns_window` returns a valid `NSWindow` pointer owned by
    // Tauri for the lifetime of the app, and this helper runs strictly on the macOS main thread.
    let ns_win = unsafe { &*(raw as *const NSWindow) };
    ns_win.setHidesOnDeactivate(false);
    let shown_now = ns_win.isVisible();
    let policy = macos_caption_policy(shown_now, pinned, fullscreen);
    let level = match policy.level {
        MacWindowLevel::Normal => NSNormalWindowLevel,
        MacWindowLevel::Floating => NSFloatingWindowLevel,
        MacWindowLevel::Status => NSStatusWindowLevel,
    };
    let collection = match policy.collection {
        MacCollectionMode::ActiveSpaceAuxiliary => {
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenAuxiliary
        }
        MacCollectionMode::AllSpacesPinned => {
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle
        }
    };
    ns_win.setLevel(level);
    ns_win.setCollectionBehavior(collection);
    if policy.maximize_to_visible_frame
        && let Some(screen) = ns_win.screen().or_else(|| {
            // SAFETY: called strictly on the macOS main thread.
            let mtm = unsafe { objc2::MainThreadMarker::new_unchecked() };
            NSScreen::mainScreen(mtm)
        })
    {
        ns_win.setFrame_display_animate(screen.visibleFrame(), true, false);
    }
    if policy.order_front {
        ns_win.orderFrontRegardless();
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_default_frame_on_main(
    window: &tauri::WebviewWindow,
    scene: CaptionScene,
    top: bool,
) {
    use objc2_app_kit::{NSScreen, NSWindow};

    let Ok(raw) = window.ns_window() else {
        return;
    };
    if raw.is_null() {
        return;
    }
    // SAFETY: `tauri::WebviewWindow::ns_window` returns a valid `NSWindow` pointer owned by
    // Tauri for the lifetime of the app, and this helper runs strictly on the macOS main thread.
    let ns_win = unsafe { &*(raw as *const NSWindow) };
    let Some(screen) = ns_win.screen().or_else(|| {
        // SAFETY: called strictly on the macOS main thread.
        let mtm = unsafe { objc2::MainThreadMarker::new_unchecked() };
        NSScreen::mainScreen(mtm)
    }) else {
        return;
    };
    let mut frame = screen.visibleFrame();
    let vis = ScreenPoints {
        x: frame.origin.x,
        y: frame.origin.y,
        width: frame.size.width,
        height: frame.size.height,
    };
    let target = default_scene_frame_cocoa(vis, scene, top);
    frame.origin.x = target.x;
    frame.origin.y = target.y;
    frame.size.width = target.width;
    frame.size.height = target.height;
    ns_win.setFrame_display_animate(frame, true, false);
}

#[cfg(target_os = "macos")]
fn apply_macos_visibility_on_main(window: &tauri::WebviewWindow, show: bool) {
    use objc2_app_kit::NSWindow;

    let Ok(raw) = window.ns_window() else {
        return;
    };
    if raw.is_null() {
        return;
    }
    // SAFETY: `tauri::WebviewWindow::ns_window` returns a valid `NSWindow` pointer owned by
    // Tauri for the lifetime of the app, and this helper runs strictly on the macOS main thread.
    let ns_win = unsafe { &*(raw as *const NSWindow) };
    if show {
        ns_win.orderFrontRegardless();
    } else {
        ns_win.orderOut(None);
    }
}

/// Immediately exits fullscreen and orders out the `captions` window on the OS main thread,
/// reconciling any visibility drift even when `State.captions` is already `false`.
pub fn hide_captions(app: &AppHandle) -> CaptionWindow {
    apply_captions(app, FORCE_HIDE_OPS)
}

/// Applies ordered [`CaptionWindowOp`]s to the `captions` window asynchronously on the OS main
/// thread so the Tokio pump task never blocks on WindowServer or AppKit animations.
pub fn apply_captions(app: &AppHandle, ops: &[CaptionWindowOp]) -> CaptionWindow {
    let Some(window) = app.get_webview_window("captions") else {
        return CaptionWindow::Missing;
    };
    let ops = ops.to_vec();
    let win = window.clone();
    match window.run_on_main_thread(move || {
        for op in ops {
            let res = match op {
                CaptionWindowOp::SetFullscreen(full) => {
                    #[cfg(target_os = "macos")]
                    {
                        apply_macos_caption_policy_on_main(&win, false, full);
                        Ok(())
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        win.set_fullscreen(full)
                    }
                }
                CaptionWindowOp::PlaceDefault { scene, top } => {
                    #[cfg(target_os = "macos")]
                    {
                        apply_macos_default_frame_on_main(&win, scene, top);
                        Ok(())
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = top; // dropped: non-macOS default fallback uses window-manager placement
                        if scene == CaptionScene::SelfView {
                            win.set_size(tauri::LogicalSize::new(
                                DEFAULT_SELF_VIEW_WIDTH_PT,
                                DEFAULT_SELF_VIEW_HEIGHT_PT,
                            ))
                        } else {
                            Ok(())
                        }
                    }
                }
                CaptionWindowOp::SetSize { width, height } => {
                    win.set_size(tauri::PhysicalSize::new(width, height))
                }
                CaptionWindowOp::SetPosition { x, y } => {
                    win.set_position(tauri::PhysicalPosition::new(x, y))
                }
                CaptionWindowOp::SetAlwaysOnTop(top) => {
                    let top_res = win.set_always_on_top(top);
                    let _ = win.set_visible_on_all_workspaces(top); // dropped: best-effort cross-space pin alongside AppKit policy
                    #[cfg(target_os = "macos")]
                    apply_macos_caption_policy_on_main(&win, top, false);
                    top_res
                }
                CaptionWindowOp::SetIgnoreCursorEvents(pass) => win.set_ignore_cursor_events(pass),
                CaptionWindowOp::Show => {
                    let show_res = win.show();
                    #[cfg(target_os = "macos")]
                    apply_macos_visibility_on_main(&win, true);
                    show_res
                }
                CaptionWindowOp::Hide => {
                    let hide_res = win.hide();
                    #[cfg(target_os = "macos")]
                    apply_macos_visibility_on_main(&win, false);
                    hide_res
                }
            };
            let _ = res; // dropped: best-effort main-thread OS window mutation; failures are non-fatal
        }
    }) {
        Ok(()) => CaptionWindow::Ok,
        Err(_) => CaptionWindow::Failed,
    }
}

/// Prevents the display from sleeping while the standalone captions window is open (matching 0.8's
/// `SubtitleDisplaySleepActivity`). Owned by a single supervised shell task: zero locks, and on
/// macOS `caffeinate -d -w <pid>` automatically terminates if the app ever crashes.
#[derive(Default)]
pub struct DisplaySleep {
    #[cfg(target_os = "macos")]
    child: Option<std::process::Child>,
}

impl DisplaySleep {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(target_os = "macos")]
    pub fn set_active(&mut self, active: bool) {
        use std::process::{Command as ProcessCommand, Stdio};
        if active {
            if self.child.as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_none()) {
                return;
            }
            let pid = std::process::id().to_string();
            self.child = ProcessCommand::new("/usr/bin/caffeinate")
                .args(["-d", "-w", &pid])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok(); // dropped: caffeinate is best-effort display-sleep guard on macOS
        } else if let Some(mut child) = self.child.take() {
            let _ = child.kill(); // dropped: child may already have exited
            let _ = child.wait(); // dropped: best-effort reap on hide
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn set_active(&mut self, _active: bool) {}
}

impl Drop for DisplaySleep {
    fn drop(&mut self) {
        self.set_active(false);
    }
}

/// Watches `state.captions` and toggles display-sleep prevention on the blocking pool without
/// holding any lock or stalling Tokio async workers on process spawn/wait syscalls.
pub async fn run_display_sleep(tree: Tree, mut state: Reading<State>) {
    let mut guard = Some(DisplaySleep::new());
    let mut active = false;

    let initial = state.now().1.captions;
    if initial {
        active = true;
        if let Some(mut current) = guard.take()
            && let Ok(returned) = tree
                .blocking("shell.sleep", move || {
                    current.set_active(true);
                    current
                })
                .await
        {
            guard = Some(returned);
        }
    }
    while let Some((_, next)) = state.next().await {
        let want = next.captions;
        if want == active {
            continue;
        }
        active = want;
        if let Some(mut current) = guard.take()
            && let Ok(returned) = tree
                .blocking("shell.sleep", move || {
                    current.set_active(want);
                    current
                })
                .await
        {
            guard = Some(returned);
        }
    }
    if active && let Some(mut current) = guard.take() {
        let _ = tree.blocking("shell.sleep", move || current.set_active(false)).await; // dropped: best-effort reap on shutdown
    }
}
