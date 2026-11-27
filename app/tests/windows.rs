//! Non-blocking caption window state machine, lock-free geometry debouncer, and geometry clamping
//! (`P5.md` §3.4, `P5-03`).

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use zutalk::windows::{
    CAPTION_GEO_DEBOUNCE_MS, CaptionWindowOp, CaptionWindowSpec, CaptionWindowTracker,
    DEFAULT_SELF_VIEW_HEIGHT_PT, DEFAULT_SELF_VIEW_WIDTH_PT, DisplaySleep, FORCE_HIDE_OPS,
    GeometrySample, MacCaptionPolicy, MacCollectionMode, MacWindowLevel, ScreenPoints,
    default_scene_frame_cocoa, macos_caption_policy, run_display_sleep, run_geometry_saver,
};
use zutalk_core::app::proto::{
    CAPTION_MAX_EXTENT_PX, CAPTION_MIN_HEIGHT_PX, CAPTION_MIN_WIDTH_PX, CaptionPrefs, CaptionScene,
    Command, State, WindowRect,
};
use zutalk_core::rt::signal::Register;
use zutalk_core::rt::stats::Stats;
use zutalk_core::rt::supervise::Tree;

#[test]
fn tracker_diff_orders_fullscreen_geometry_and_show_cleanly() {
    let mut tracker = CaptionWindowTracker::new();

    // 1. Initial hidden state never emits a spurious Hide.
    let hidden = CaptionWindowSpec {
        shown: false,
        scene: CaptionScene::SelfView,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: false,
        geometry: None,
    };
    assert!(tracker.diff(hidden.clone()).is_empty());
    assert!(tracker.diff(hidden).is_empty());

    // 2. First show applies SetSize -> SetPosition -> SetIgnoreCursorEvents -> Show -> SetAlwaysOnTop.
    let self_rect = WindowRect { x: 120, y: 80, width: 900, height: 220 };
    let shown_self = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::SelfView,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: false,
        geometry: Some(self_rect),
    };
    assert_eq!(
        tracker.diff(shown_self.clone()),
        vec![
            CaptionWindowOp::SetSize { width: 900, height: 220 },
            CaptionWindowOp::SetPosition { x: 120, y: 80 },
            CaptionWindowOp::SetIgnoreCursorEvents(false),
            CaptionWindowOp::Show,
            CaptionWindowOp::SetAlwaysOnTop(false),
        ]
    );

    // 3. Repeating the exact same spec emits zero ops (prevents OS Moved/Resized feedback loop).
    assert!(tracker.diff(shown_self).is_empty());

    // 4. Switching to Stage fullscreen emits SetFullscreen(true) and skips SetSize/SetPosition.
    let stage_spec = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Stage,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: true,
        geometry: Some(WindowRect { x: 10, y: 10, width: 800, height: 600 }),
    };
    assert_eq!(tracker.diff(stage_spec.clone()), vec![CaptionWindowOp::SetFullscreen(true)]);

    // Geometry change while staying fullscreen is ignored.
    let stage_geo_changed = CaptionWindowSpec {
        geometry: Some(WindowRect { x: 50, y: 50, width: 1000, height: 700 }),
        ..stage_spec
    };
    assert!(tracker.diff(stage_geo_changed).is_empty());

    // 5. Switching from Stage fullscreen to Band exits fullscreen first, then restores Band rect,
    //    always_on_top, and click_through.
    let band_rect = WindowRect { x: 0, y: 900, width: 1440, height: 140 };
    let band_spec = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Band,
        top: true,
        always_on_top: true,
        click_through: true,
        fullscreen: false,
        geometry: Some(band_rect),
    };
    assert_eq!(
        tracker.diff(band_spec.clone()),
        vec![
            CaptionWindowOp::SetFullscreen(false),
            CaptionWindowOp::SetSize { width: 1440, height: 140 },
            CaptionWindowOp::SetPosition { x: 0, y: 900 },
            CaptionWindowOp::SetAlwaysOnTop(true),
            CaptionWindowOp::SetIgnoreCursorEvents(true),
        ]
    );

    // 6. Hiding from windowed Band emits only Hide.
    let hide_band = CaptionWindowSpec { shown: false, ..band_spec };
    assert_eq!(tracker.diff(hide_band), vec![CaptionWindowOp::Hide]);

    // 7. Showing directly into fullscreen Stage, then hiding exits fullscreen before Hide.
    let show_stage = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Stage,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: true,
        geometry: Some(self_rect),
    };
    assert_eq!(
        tracker.diff(show_stage.clone()),
        vec![
            CaptionWindowOp::SetIgnoreCursorEvents(false),
            CaptionWindowOp::Show,
            CaptionWindowOp::SetFullscreen(true),
        ]
    );
    assert_eq!(
        tracker.diff(CaptionWindowSpec { shown: false, ..show_stage }),
        vec![CaptionWindowOp::SetFullscreen(false), CaptionWindowOp::Hide]
    );
}

#[test]
fn tracker_restores_default_frame_when_leaving_fullscreen_or_switching_unplaced_scenes() {
    let mut tracker = CaptionWindowTracker::new();

    // 1. First show into Stage fullscreen (`geometry: None`), then switch to SelfView (`geometry: None`):
    //    MUST emit `PlaceDefault { scene: SelfView, top: false }` so macOS shrinks back from maximized!
    let stage = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Stage,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: true,
        geometry: None,
    };
    assert_eq!(
        tracker.diff(stage),
        vec![
            CaptionWindowOp::SetIgnoreCursorEvents(false),
            CaptionWindowOp::Show,
            CaptionWindowOp::SetFullscreen(true),
        ]
    );

    let unplaced_self = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::SelfView,
        top: false,
        always_on_top: true,
        click_through: false,
        fullscreen: false,
        geometry: None,
    };
    assert_eq!(
        tracker.diff(unplaced_self.clone()),
        vec![
            CaptionWindowOp::SetFullscreen(false),
            CaptionWindowOp::PlaceDefault { scene: CaptionScene::SelfView, top: false },
            CaptionWindowOp::SetAlwaysOnTop(true),
        ]
    );

    // 2. Switching from unplaced SelfView to unplaced Band (top: true) places the top banner natively.
    let unplaced_band_top = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Band,
        top: true,
        always_on_top: true,
        click_through: false,
        fullscreen: false,
        geometry: None,
    };
    assert_eq!(
        tracker.diff(unplaced_band_top.clone()),
        vec![CaptionWindowOp::PlaceDefault { scene: CaptionScene::Band, top: true }]
    );

    // 3. Flipping Band edge from top -> bottom with `geometry: None` re-places the banner at bottom.
    let unplaced_band_bottom = CaptionWindowSpec { top: false, ..unplaced_band_top };
    assert_eq!(
        tracker.diff(unplaced_band_bottom.clone()),
        vec![CaptionWindowOp::PlaceDefault { scene: CaptionScene::Band, top: false }]
    );

    // 4. Resetting custom geometry back to `None` while visible re-applies default placement.
    let manual_band = CaptionWindowSpec {
        geometry: Some(WindowRect { x: 40, y: 80, width: 1200, height: 180 }),
        ..unplaced_band_bottom.clone()
    };
    let _ = tracker.diff(manual_band);
    assert_eq!(
        tracker.diff(unplaced_band_bottom),
        vec![CaptionWindowOp::PlaceDefault { scene: CaptionScene::Band, top: false }]
    );
}

#[test]
fn default_scene_frame_cocoa_handles_negative_origin_secondary_monitors_accurately() {
    // Secondary monitor arranged to the left and below primary in Cocoa points (bottom-left origin).
    let secondary = ScreenPoints { x: -1920.0, y: -240.0, width: 1920.0, height: 1055.0 };

    let self_frame = default_scene_frame_cocoa(secondary, CaptionScene::SelfView, false);
    assert_eq!(self_frame.width, DEFAULT_SELF_VIEW_WIDTH_PT);
    assert_eq!(self_frame.height, DEFAULT_SELF_VIEW_HEIGHT_PT);
    assert_eq!(self_frame.x, -1920.0 + (1920.0 - 900.0) * 0.5);
    assert_eq!(self_frame.y, -240.0 + 48.0);

    let band_top = default_scene_frame_cocoa(secondary, CaptionScene::Band, true);
    assert_eq!(band_top.x, -1920.0);
    assert_eq!(band_top.width, 1920.0);
    assert!((band_top.y + band_top.height - (-240.0 + 1055.0)).abs() < 1e-6);

    let band_bottom = default_scene_frame_cocoa(secondary, CaptionScene::Band, false);
    assert_eq!(band_bottom.x, -1920.0);
    assert_eq!(band_bottom.y, -240.0);
    assert_eq!(band_bottom.width, 1920.0);
    assert_eq!(band_bottom.height, band_top.height);
}

#[tokio::test(start_paused = true)]
async fn geometry_saver_debounces_and_skips_while_fullscreen() {
    let state_reg = Register::new({
        let mut s = State { captions: true, ..State::default() };
        s.setup.caption = CaptionPrefs::default();
        s
    });
    let (geo_tx, geo_rx) = watch::channel::<Option<GeometrySample>>(None);
    let sent: Arc<Mutex<Vec<Command>>> = Arc::new(Mutex::new(Vec::new()));
    let sent_cb = sent.clone();

    let saver = tokio::spawn(run_geometry_saver(geo_rx, state_reg.read(), move |cmd| {
        sent_cb.lock().unwrap().push(cmd);
    }));
    tokio::task::yield_now().await;

    // Rapid burst of 5 move/resize events within 400 ms collapses into a single Command.
    for i in 0..5i32 {
        geo_tx.send_modify(|slot| {
            let s = slot.get_or_insert_with(Default::default);
            s.merge_move(100 + i * 20, 200 + i * 10);
            s.merge_resize(800 + (i as u32) * 40, 200 + (i as u32) * 10);
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(sent.lock().unwrap().is_empty(), "must not emit before debounce settles");

    tokio::time::sleep(Duration::from_millis(CAPTION_GEO_DEBOUNCE_MS + 10)).await;
    let expected = WindowRect { x: 180, y: 240, width: 960, height: 240 };
    assert_eq!(
        *sent.lock().unwrap(),
        vec![Command::CaptionGeometry { scene: CaptionScene::SelfView, rect: expected }]
    );

    // While fullscreen (`Stage`), move/resize events must NOT dispatch CaptionGeometry.
    state_reg.change(|cur| {
        let mut next = cur.clone();
        next.setup.caption.scene = CaptionScene::Stage;
        Some(next)
    });
    geo_tx.send_modify(|slot| {
        let s = slot.get_or_insert_with(Default::default);
        s.merge_move(0, 0);
        s.merge_resize(1920, 1080);
    });
    tokio::time::sleep(Duration::from_millis(CAPTION_GEO_DEBOUNCE_MS + 50)).await;
    assert_eq!(sent.lock().unwrap().len(), 1, "fullscreen geometry must not be persisted");

    // While hidden (`captions == false`), move/resize events must NOT dispatch CaptionGeometry.
    state_reg.change(|cur| {
        let mut next = cur.clone();
        next.setup.caption.scene = CaptionScene::Band;
        next.captions = false;
        Some(next)
    });
    geo_tx.send_modify(|slot| {
        let s = slot.get_or_insert_with(Default::default);
        s.merge_move(40, 60);
        s.merge_resize(1000, 160);
    });
    tokio::time::sleep(Duration::from_millis(CAPTION_GEO_DEBOUNCE_MS + 50)).await;
    assert_eq!(sent.lock().unwrap().len(), 1, "hidden window geometry must not be persisted");

    drop(geo_tx);
    saver.await.unwrap();
}

#[test]
fn window_rect_clamping_rejects_degenerate_dimensions() {
    let tiny = WindowRect { x: -999_999, y: 999_999, width: 0, height: 12 }.clamp();
    assert_eq!(
        tiny,
        WindowRect {
            x: -(CAPTION_MAX_EXTENT_PX as i32),
            y: CAPTION_MAX_EXTENT_PX as i32,
            width: CAPTION_MIN_WIDTH_PX,
            height: CAPTION_MIN_HEIGHT_PX,
        }
    );

    let huge = WindowRect { x: 50, y: -40, width: 999_999, height: 500_000 }.clamp();
    assert_eq!(
        huge,
        WindowRect { x: 50, y: -40, width: CAPTION_MAX_EXTENT_PX, height: CAPTION_MAX_EXTENT_PX }
    );

    let mut sample = GeometrySample::default();
    assert_eq!(sample.to_rect(), None);
    sample.merge_move(30, 40);
    assert_eq!(sample.to_rect(), None);
    sample.merge_resize(10, 10);
    assert_eq!(
        sample.to_rect(),
        Some(WindowRect {
            x: 30,
            y: 40,
            width: CAPTION_MIN_WIDTH_PX,
            height: CAPTION_MIN_HEIGHT_PX,
        })
    );
}

#[tokio::test]
async fn display_sleep_watcher_tracks_caption_visibility_and_releases_on_drop() {
    let mut guard = DisplaySleep::new();
    guard.set_active(false);
    drop(guard);

    let stats = Stats::new();
    let tree = Tree::root("shell_sleep_test", stats);
    let state_reg = Register::new(State { captions: false, ..State::default() });
    let handle = tokio::spawn(run_display_sleep(tree.clone(), state_reg.read()));
    tokio::task::yield_now().await;
    state_reg.change(|cur| Some(State { captions: false, ..cur.clone() }));
    state_reg.change(|cur| Some(State { captions: true, ..cur.clone() }));
    tokio::time::sleep(Duration::from_millis(20)).await;
    state_reg.change(|cur| Some(State { captions: false, ..cur.clone() }));
    drop(state_reg);
    handle.await.expect("display sleep watcher exits cleanly");
    tree.cancel();
}

#[test]
fn macos_caption_policy_never_orders_front_when_hidden_and_maps_all_eight_states() {
    // Hidden window (`currently_visible == false`) must NEVER call `orderFrontRegardless`,
    // regardless of `pinned` or `fullscreen` — otherwise AppKit maps a `"visible": false` window
    // onto the screen while `State.captions` remains `false`.
    for pinned in [false, true] {
        for fullscreen in [false, true] {
            let policy = macos_caption_policy(false, pinned, fullscreen);
            assert!(
                !policy.order_front,
                "hidden window must never order_front (pinned={pinned}, fullscreen={fullscreen}): {policy:?}"
            );
        }
    }

    // Visible + fullscreen (`Stage`): floating level on active Space, maximized, ordered front.
    for pinned in [false, true] {
        assert_eq!(
            macos_caption_policy(true, pinned, true),
            MacCaptionPolicy {
                level: MacWindowLevel::Floating,
                collection: MacCollectionMode::ActiveSpaceAuxiliary,
                maximize_to_visible_frame: true,
                order_front: true,
            }
        );
    }

    // Visible + windowed + pinned (`always_on_top == true`): status level across all Spaces.
    assert_eq!(
        macos_caption_policy(true, true, false),
        MacCaptionPolicy {
            level: MacWindowLevel::Status,
            collection: MacCollectionMode::AllSpacesPinned,
            maximize_to_visible_frame: false,
            order_front: true,
        }
    );

    // Visible + windowed + unpinned (`always_on_top == false`): normal window level, no forced front.
    assert_eq!(
        macos_caption_policy(true, false, false),
        MacCaptionPolicy {
            level: MacWindowLevel::Normal,
            collection: MacCollectionMode::ActiveSpaceAuxiliary,
            maximize_to_visible_frame: false,
            order_front: false,
        }
    );
}

#[test]
fn tracker_leaving_fullscreen_always_restores_always_on_top_even_when_unpinned_and_exposes_force_hide()
 {
    let mut tracker = CaptionWindowTracker::new();

    let stage_unpinned = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Stage,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: true,
        geometry: None,
    };
    let _ = tracker.diff(stage_unpinned);

    // Leaving Stage (`always_on_top: false`) into unpinned SelfView (`always_on_top: false`)
    // MUST still emit `SetAlwaysOnTop(false)` so macOS drops from `NSFloatingWindowLevel` back to
    // `NSNormalWindowLevel`.
    let self_unpinned = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::SelfView,
        top: false,
        always_on_top: false,
        click_through: false,
        fullscreen: false,
        geometry: None,
    };
    assert_eq!(
        tracker.diff(self_unpinned),
        vec![
            CaptionWindowOp::SetFullscreen(false),
            CaptionWindowOp::PlaceDefault { scene: CaptionScene::SelfView, top: false },
            CaptionWindowOp::SetAlwaysOnTop(false),
        ]
    );

    // Explicit close (`Command::Captions { show: false }` / `CloseRequested`) always executes
    // idempotent `[SetFullscreen(false), Hide]` even if `State.captions` was already `false`.
    assert_eq!(FORCE_HIDE_OPS, [CaptionWindowOp::SetFullscreen(false), CaptionWindowOp::Hide]);
}
