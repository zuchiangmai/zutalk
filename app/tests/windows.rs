//! Non-blocking caption window state machine, lock-free geometry debouncer, and geometry clamping
//! (`P5.md` §3.4, `P5-03`).

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use zutalk::windows::{
    CAPTION_GEO_DEBOUNCE_MS, CaptionWindowOp, CaptionWindowSpec, CaptionWindowTracker,
    GeometrySample, run_geometry_saver,
};
use zutalk_core::app::proto::{
    CAPTION_MAX_EXTENT_PX, CAPTION_MIN_HEIGHT_PX, CAPTION_MIN_WIDTH_PX, CaptionPrefs, CaptionScene,
    Command, State, WindowRect,
};
use zutalk_core::rt::signal::Register;

#[test]
fn tracker_diff_orders_fullscreen_geometry_and_show_cleanly() {
    let mut tracker = CaptionWindowTracker::new();

    // 1. Initial hidden state never emits a spurious Hide.
    let hidden = CaptionWindowSpec {
        shown: false,
        scene: CaptionScene::SelfView,
        always_on_top: false,
        click_through: false,
        fullscreen: false,
        geometry: None,
    };
    assert!(tracker.diff(hidden.clone()).is_empty());
    assert!(tracker.diff(hidden).is_empty());

    // 2. First show applies SetSize -> SetPosition -> SetAlwaysOnTop -> SetIgnoreCursorEvents -> Show.
    let self_rect = WindowRect { x: 120, y: 80, width: 900, height: 220 };
    let shown_self = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::SelfView,
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
            CaptionWindowOp::SetAlwaysOnTop(false),
            CaptionWindowOp::SetIgnoreCursorEvents(false),
            CaptionWindowOp::Show,
        ]
    );

    // 3. Repeating the exact same spec emits zero ops (prevents OS Moved/Resized feedback loop).
    assert!(tracker.diff(shown_self).is_empty());

    // 4. Switching to Stage fullscreen emits SetFullscreen(true) and skips SetSize/SetPosition.
    let stage_spec = CaptionWindowSpec {
        shown: true,
        scene: CaptionScene::Stage,
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
        always_on_top: false,
        click_through: false,
        fullscreen: true,
        geometry: Some(self_rect),
    };
    assert_eq!(
        tracker.diff(show_stage.clone()),
        vec![
            CaptionWindowOp::SetFullscreen(true),
            CaptionWindowOp::SetAlwaysOnTop(false),
            CaptionWindowOp::SetIgnoreCursorEvents(false),
            CaptionWindowOp::Show,
        ]
    );
    assert_eq!(
        tracker.diff(CaptionWindowSpec { shown: false, ..show_stage }),
        vec![CaptionWindowOp::SetFullscreen(false), CaptionWindowOp::Hide]
    );
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
