状态：完成

## 改了什么

1. **核心定界 4 场合字幕配置与默认城市地址落入 `Setup`（`core/src/app/{proto,hub,work}.rs`）**：
   - 具名带单位常量：`CAPTION_MIN_WIDTH_PX = 240`、`CAPTION_MIN_HEIGHT_PX = 80`、`CAPTION_MAX_EXTENT_PX = 16_384`、`CAPTION_LINES_MIN = 1`、`CAPTION_LINES_MAX = 3`；
   - 新增全导出定界类型：`CaptionScene`（`SelfView` / `Band` / `Stage` / `Facing`，含稳定日志码 `self`/`band`/`stage`/`facing`）、`CaptionSize`、`CaptionBg`、`WindowRect`（含 `clamp()`）、`SceneStyle`、`CaptionScenes`（固定 4 字段，无动态 `Map`，I8 恒定）、`CaptionPrefs`（含 `active()` / `style_mut()` / `normalized()`）；
   - `Setup` 增补 `#[serde(default)] pub caption: CaptionPrefs` 与 `#[serde(default)] pub city: Option<String>`（旧 `settings.json` 向后兼容，启动与写入均 `normalized()`）；`Command` 增补 `CaptionGeometry { scene, rect }`（`needs_person = false`，同步更新脱敏 `fmt::Debug`）；
   - `hub.rs` 中 `Facing` 场合与 `Mode::Conversation` 双向同步，其余场合对应 `Mode::Audience`；`Command::CaptionGeometry` 仅钳制并更新对应场合的 `geometry` 后落盘 `settings.json`；为 `Msg::Booted` / `DeskOut::Booted` 中的 `BootOut` 加 `Box` 以消除 clippy `large_enum_variant`。
2. **桌面外壳无锁字幕窗状态机、防抖几何回写与退出收尾（`app/src/{windows,pump,main}.rs`）**：
   - `app/src/windows.rs`：具名常量 `CAPTION_GEO_DEBOUNCE_MS = 400`，纯结构 `CaptionWindowSpec`、`CaptionWindowOp`、纯状态机 `CaptionWindowTracker::diff`（首次隐藏不误发 `Hide`、置顶/穿透点击总在 `Show` 前、进全屏跳过几何、退全屏先关全屏再恢复矩形、同规格输出空列表防回环）、`GeometrySample` 与无锁防抖任务 `run_geometry_saver`（仅在显示中且非全屏且几何变化时发 `Command::CaptionGeometry`），以及 Tauri 适配器 `apply_captions`（直接使用 `PhysicalSize`/`PhysicalPosition`，零 `scale_factor()` 查询）；
   - `app/src/pump.rs`：接入 `CaptionWindowTracker`，按变更分别发出既有 `shell.captions` 与新增 `shell.captions.style`；
   - `app/src/main.rs`：在 `captions` 窗口挂 `on_window_event` 经无锁 `watch` 采集 `Moved`/`Resized` 给监督任务 `shell.captions_geo`，`CloseRequested` 调 `prevent_close()` 并派发 `Command::Captions { show: false }`；`RunEvent::ExitRequested` 用 `AtomicBool` 防重入，若 `core.is_hosting()` 则 `prevent_exit()`、派发 `Command::StopOutbound`、在 `shell` 树起 `shell.exit_host` 异步等 `core.wait_host_idle(HOST_EXIT_WAIT_MS)` 后调 `exit(0)`（零 `block_on`、零锁）。
3. **事件文档、绑定、测试与最小前端同步**：
   - 更新 `docs/EVENTS.md` 登记 `shell.captions.style`；
   - 重新生成 `core/bindings/*.ts`（新增 `CaptionScene.ts`、`CaptionSize.ts`、`CaptionBg.ts`、`WindowRect.ts`、`SceneStyle.ts`、`CaptionScenes.ts`、`CaptionPrefs.ts`，更新 `Setup.ts` 与 `Command.ts`）；
   - 同步更新 `ui/dev/mock.ts`（`DEFAULT_CAPTION_PREFS` 与 `CaptionGeometry` 处理）与 `ui/tests/home_live.test.tsx` 的 `Setup` 夹具；
   - 新增 `app/tests/windows.rs`（3 条测试）、更新 `app/tests/pump.rs`、在 `core/tests/app.rs` 新增 `setup_caption_prefs_and_geometry_persist_per_scene_in_settings_json`。

## 证据

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（零警告）。
- 全部 10 个规则脚本（`set -e` 逐一执行）：全绿（`bounded.sh`、`spawn.sh`、`layers.sh`、`shell.sh`、`ui.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`numbers.sh`、`pending.sh` 均零违规、零欠账）。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`：48/48 通过。
- 沙箱内 Rust 测试：
  - `zutalk`（桌面外壳，23/23 全绿）：
    - `app/tests/windows.rs`（3/3）：`tracker_diff_orders_fullscreen_geometry_and_show_cleanly`、`geometry_saver_debounces_and_skips_while_fullscreen`、`window_rect_clamping_rejects_degenerate_dimensions`；
    - `app/tests/pump.rs`（11/11）、`app/tests/beat.rs`（5/5）、`app/tests/updates.rs`（4/4）；
  - `core/tests/app.rs`：`commands_that_need_a_person_are_listed_in_one_table`、`setup_caption_prefs_and_geometry_persist_per_scene_in_settings_json` 通过；
  - `core/tests/liveness.rs`：`every_event_in_the_catalogue_can_be_told_apart`、`i8_what_one_change_sends_does_not_grow_with_the_library`、`two_minute_recording_tally_matches_the_table` 通过；
  - `core/tests/host.rs`（非 TCP 3/3）、`core/tests/city.rs`（`archive_types_table_matches_packed_records`）、`zutalk-server`（29/29 非 TCP）全部通过；
  - `cargo test --workspace --no-run`：全工作区编译通过。
- 前端校验与测试（`ui/`）：
  - `npm run check` & `npm run build`：通过；
  - `npm test`：13/13 文件、97/97 测试通过；
  - `Cargo.lock` 与 `ui/package-lock.json` 无变更。

- **沙箱外全量门禁 `RUSTC_WRAPPER="" scripts/build/check.sh`**：`check: all green`（`rustfmt` + `clippy -D warnings` + 10/10 规则脚本零欠账全绿 + Rust **749 passed, 0 failed, 6 ignored** + UI **13 文件 / 97 passed**，含全部本机端口 `server/tests/` 与 `core/tests/host.rs` 集成测试）。

## 遗留 / 需协调人注意

- `CaptionScenesPanel` 界面预览与 4 场合 `CaptionsWindow` 渲染留在 `P5-05`；`HostPage` / `FileArchiveCard` / `OutboundPage` 留在 `P5-04`。
