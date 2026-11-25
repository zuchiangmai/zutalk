# P5-03 桌面字幕窗按场合持久化与无锁外壳管理

## 目标

做完以后：

1. **按场合字幕窗样式与默认城市地址落入 `Setup`（`settings.json`）**：
   - `core/src/app/proto.rs` 定义定界类型 `CaptionScene`（`SelfView` / `Band` / `Stage` / `Facing`）、`CaptionSize`（`Small` / `Auto` / `Large`）、`CaptionBg`（`Clear` / `Half` / `Solid`）、`WindowRect`、`SceneStyle`、`CaptionScenes`（固定 4 字段，无动态 `Map`，I8 恒定）与 `CaptionPrefs`；
   - `Setup` 增补 `#[serde(default)] pub caption: CaptionPrefs` 与 `#[serde(default)] pub city: Option<String>`（旧 `settings.json` 向后兼容）；
   - `Command` 增补 `CaptionGeometry { scene: CaptionScene, rect: WindowRect }`（`needs_person = false`），在 `hub` 中校验尺寸范围（`240..=16384` × `80..=16384`）并仅更新对应场合的 `geometry`，经 `desk` 持久化到 `settings.json`（不覆盖用户并发修改的其他字段）。
2. **外壳无锁字幕窗状态机与几何持久（`app/src/windows.rs`、`app/src/pump.rs`、`app/src/main.rs`）**：
   - `app/src/windows.rs` 实现纯结构 `CaptionWindowSpec`、`CaptionWindowOp` 与纯状态机 `CaptionWindowTracker::diff(&mut self, next: CaptionWindowSpec) -> Vec<CaptionWindowOp>`：
     - 支持置顶（`set_always_on_top`）、穿透点击（`set_ignore_cursor_events`）、舞台全屏（`set_fullscreen`）、非全屏位置与尺寸恢复（`set_size` / `set_position`）与显隐（`show` / `hide`）；
     - 退出全屏时先关全屏再恢复矩形，进入全屏时跳过矩形设置，置顶与穿透点击总在 `Show` 之前执行；
     - 同一规格不重复下发，防止 OS `Moved`/`Resized` 事件回环；
   - `app/src/pump.rs` 将 `on_captions` 升级为驱动 `CaptionWindowTracker::diff` + `apply_caption_ops`，按变化发出既有 `shell.captions` 与新增 `shell.captions.style` 事件；
   - `app/src/main.rs` 在 `captions` 窗口挂载 `on_window_event`：
     - 收到 `WindowEvent::Moved(pos)` / `WindowEvent::Resized(size)` 时通过**无锁** `tokio::sync::watch::Sender<Option<GeometrySample>>` 送往监督任务 `shell.captions_geo`，经 `CAPTION_GEO_DEBOUNCE_MS = 400` ms 防抖且确认当前非全屏后派发 `Command::CaptionGeometry`；
     - 收到 `WindowEvent::CloseRequested` 时调 `api.prevent_close()` 并派发 `Command::Captions { show: false }`（只隐藏不销毁字幕 WebView）；
   - **干净退出发 `gone`（零 `block_on`）**：`app/src/main.rs` 在 `RunEvent::ExitRequested { api, .. }` 用 `AtomicBool` 防重入，若 `core.is_hosting()` 则 `api.prevent_exit()`、发 `Command::StopOutbound`、在 `shell` 监督树起任务 `shell.exit_host` 异步等 `core.wait_host_idle(HOST_EXIT_WAIT_MS)` 后调 `app_handle.exit(0)`；
   - **严格遵守 `shell.sh`**：`app/src` 全部 `#[tauri::command]` 保持 `async fn`，零 `Mutex`/`RwLock`/`.lock()`/`block_on`，零阻塞主线程窗口查询（不调 `inner_size`/`outer_position`/`scale_factor` 等）。
3. **绑定、事件与最小前端同步**：
   - 更新 `docs/EVENTS.md`（登记 `shell.captions.style`）；
   - 重新生成 `core/bindings/*.ts`（导出 `CaptionPrefs.ts`、`CaptionScene.ts`、`CaptionScenes.ts`、`SceneStyle.ts`、`WindowRect.ts` 等），同步更新 `ui/dev/mock.ts` 与 `ui/tests/` 中的 `Setup` 夹具，确保 `scripts/build/check.sh` 全绿。

## 背景

- 设计：`docs/build/phases/P5.md` §3.4、§3.5；`docs/PLAN.md` §2.2、§3、§4.1、§5.1、§5.5；`docs/ui-2026-11-21/{CaptionScenes,CaptionsFacing}.dc.html`。
- 涉及文件：
  - `core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`
  - `app/src/windows.rs`、`app/src/pump.rs`、`app/src/main.rs`
  - `docs/EVENTS.md`
  - `core/bindings/*.ts`、`ui/dev/mock.ts`、`ui/tests/`（`Setup` 夹具补齐 `caption` 与 `city`）
  - 新增 `app/tests/windows.rs`，更新 `app/tests/pump.rs`

## 做法

### 1. `core/src/app/proto.rs` 与 `hub.rs`

- 具名常量（带单位与理由注释）：
  - `pub const CAPTION_MIN_WIDTH_PX: u32 = 240;` // smallest readable caption window width
  - `pub const CAPTION_MIN_HEIGHT_PX: u32 = 80;` // smallest readable caption window height
  - `pub const CAPTION_MAX_EXTENT_PX: u32 = 16_384;` // upper bound guarding corrupted window geometry
  - `pub const CAPTION_LINES_MIN: u8 = 1;` // minimum caption lines per language (`CaptionScenes.dc.html`)
  - `pub const CAPTION_LINES_MAX: u8 = 3;` // maximum caption lines per language (`CaptionScenes.dc.html`)
- 类型定义（全 `ts-rs` 导出，带 `#[serde(default)]` 保兼容）：
  - `CaptionScene`: `SelfView`、`Band`、`Stage`、`Facing`，提供 `pub const fn code(self) -> &'static str`（`"self"` / `"band"` / `"stage"` / `"facing"`）；
  - `CaptionSize`: `Small`、`Auto`（默认）、`Large`；
  - `CaptionBg`: `Clear`、`Half`、`Solid`（默认）；
  - `WindowRect { pub x: i32, pub y: i32, pub width: u32, pub height: u32 }`（含 `pub fn clamp(self) -> Self`）；
  - `SceneStyle` + `CaptionScenes`（4 个默认预设精确对齐 `CaptionScenes.dc.html` 与 `CaptionsFacing.dc.html`：`self_view` 2 行半透非穿透、`band` 2 行实底穿透置顶、`stage` 3 行实底全屏、`facing` 2 行实底非全屏）；
  - `CaptionPrefs` 提供 `pub fn active(&self) -> &SceneStyle`、`pub fn active_mut(&mut self) -> &mut SceneStyle`、`pub fn style_mut(&mut self, scene: CaptionScene) -> &mut SceneStyle`、`pub fn normalized(mut self) -> Self`（把每场合 `lines` 钳制到 `1..=3`、`geometry` 经 `clamp()`）。
- `Setup` 增补 `pub caption: CaptionPrefs` 与 `pub city: Option<String>`；`Command::Setup` 与 `Command::CaptionGeometry` 在 `hub.rs` 中先 `normalized()` / `clamp()` 再落内存并交 `desk` 写 `settings.json`；当 `caption.scene == CaptionScene::Facing` 时同步令 `setup.mode = Mode::Conversation`，其余场合 `Mode::Audience`（兼容旧代码读 `mode`）。

### 2. `app/src/windows.rs`

- 具名带单位常量（过 `numbers.sh`）：
  - `pub const CAPTION_GEO_DEBOUNCE_MS: u64 = 400;` // wait after last OS move/resize before persisting geometry
- 纯数据结构与状态机（零 Tauri 耦合，便于 `app/tests/windows.rs` 穷举测试）：
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct CaptionWindowSpec {
      pub shown: bool,
      pub scene: CaptionScene,
      pub always_on_top: bool,
      pub click_through: bool,
      pub fullscreen: bool,
      pub geometry: Option<WindowRect>,
  }

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
  ```
- `CaptionWindowTracker`：
  - 持有 `applied: Option<CaptionWindowSpec>`；
  - `pub fn diff(&mut self, next: CaptionWindowSpec) -> Vec<CaptionWindowOp>`：
    - 对比 `applied` 与 `next`，仅发射变化项；
    - 首次构造时若 `shown == false` 只记录状态不发 `Hide`（与现有 `app/tests/pump.rs` 行为一致）；
    - 当 `shown == true` 或刚从 `false -> true` 时：若刚退出全屏先发 `SetFullscreen(false)` 再恢复 `geometry`；若进入全屏只发 `SetFullscreen(true)` 不发 `SetSize`/`SetPosition`；发 `SetAlwaysOnTop` 与 `SetIgnoreCursorEvents`；最后发 `Show`；
    - 当 `shown` 从 `true -> false` 时：若之前全屏先 `SetFullscreen(false)`，再 `Hide`；
    - 若 `next` 与 `applied` 仅 `geometry` 变化且当前处于 `fullscreen == true`，忽略该 `geometry` 变更避免干扰全屏。
- 几何防抖合并纯结构 `GeometrySample { x: Option<i32>, y: Option<i32>, width: Option<u32>, height: Option<u32> }`：
  - `pub fn merge_move(&mut self, x: i32, y: i32)`、`pub fn merge_resize(&mut self, width: u32, height: u32)`、`pub fn to_rect(&self) -> Option<WindowRect>`；
  - `pub async fn run_geometry_saver(...)`：监听 `watch::Receiver<Option<GeometrySample>>`，在每次变化后 `tokio::time::sleep(Duration::from_millis(CAPTION_GEO_DEBOUNCE_MS))`（来新事件重置防抖），防抖到期时从 `Reading<State>` 读当前 `caption.scene` 与 `active().fullscreen`；仅在 `state.captions && !fullscreen` 且与已保存 `geometry` 不同时派发 `Command::CaptionGeometry { scene, rect }`！
- Tauri 适配函数 `pub fn apply_captions(app: &AppHandle, ops: &[CaptionWindowOp]) -> CaptionWindow`：
  - 取 `app.get_webview_window("captions")`（缺失返 `CaptionWindow::Missing`）；
  - 顺序执行每个 `CaptionWindowOp`：使用 `tauri::PhysicalSize::new(width, height)` 与 `tauri::PhysicalPosition::new(x, y)`（与 `WindowEvent::Moved`/`Resized` 坐标系天然一致，绝不调用被 `shell.sh` 禁止的 `scale_factor()`！）；任一调用失败记 `CaptionWindow::Failed`，全部成功返 `CaptionWindow::Ok`。

### 3. `app/src/pump.rs`、`app/src/main.rs` 与 `docs/EVENTS.md`

- `app/src/pump.rs`：
  - `PumpState` 持有 `caption_tracker: CaptionWindowTracker`；
  - `handle_state` 计算 `CaptionWindowSpec::from_state(&state)` → `ops = self.caption_tracker.diff(spec.clone())` → 非空时调用回调 `on_captions(&ops)`：
    - 若 `ops` 含 `Show`/`Hide` 发出 `shell.captions`（保留 P1 契约）；
    - 若 `ops` 含样式/几何变更发出 `shell.captions.style`（`scene = spec.scene.code(), top = spec.always_on_top, pass = spec.click_through, full = spec.fullscreen, code = outcome.code()`）；
  - 同步更新 `app/tests/pump.rs` 回调签名（保持 P1 全部交错/重载/崩溃自愈测试不变）。
- `app/src/main.rs`：
  - 在 `.setup` 中获取 `captions` 窗口（若存在），创建无锁 `watch::channel::<Option<GeometrySample>>(None)`，挂 `on_window_event`：
    - `WindowEvent::Moved(pos)` → `geo_tx.send_modify(...)`
    - `WindowEvent::Resized(size)` → `geo_tx.send_modify(...)`
    - `WindowEvent::CloseRequested { api, .. }` → `api.prevent_close(); let _ = core.send(Command::Captions { show: false }); // dropped: core closed on exit`
  - 在 `shell` 树起 `shell.spawn("shell.captions_geo", windows::run_geometry_saver(...))`；
  - 在 `app.run` 回调里处理 `RunEvent::ExitRequested { api, .. }`：用 `AtomicBool` 防重入，若 `core.is_hosting()` 则 `api.prevent_exit()`，派发 `Command::StopOutbound`，并在 `shell` 树起 `shell.exit_host` 异步等 `core.wait_host_idle(HOST_EXIT_WAIT_MS)` 后调 `handle.exit(0)`（**零 `block_on`**）。
- 更新 `docs/EVENTS.md`：在 `### 外壳与界面` 增补 `shell.captions.style` 行。
- 刷新 `core/bindings/*.ts`，更新 `ui/dev/mock.ts` 和 `ui/tests/` 中的 `Setup` 夹具。

## 显式与可观测

- **纯状态机**：`CaptionWindowTracker::diff`、`GeometrySample` 与 `CaptionPrefs::normalized` 均为无 IO 纯函数，配转换表与防抖测试。
- **具名带单位常量**：`CAPTION_MIN_WIDTH_PX`、`CAPTION_MIN_HEIGHT_PX`、`CAPTION_MAX_EXTENT_PX`、`CAPTION_LINES_MIN`、`CAPTION_LINES_MAX`、`CAPTION_GEO_DEBOUNCE_MS` 全带单位与理由注释（过 `numbers.sh`）。
- **外壳零阻塞零锁**：无 `Mutex`/`RwLock`/`.lock()`/`block_on`/阻塞窗口查询，通过 `shell.sh` 静态守卫；几何回传经无锁 `watch`（不占 `Stats` 队列槽位，`stats.full == 0`）。
- **结构化事件**：`shell.captions` 与 `shell.captions.style` 记清晰代码与布尔开关，绝不记位置坐标之外的敏感信息或字幕文本。

## 先写的测试

1. 新建 `app/tests/windows.rs`：
   - `tracker_diff_orders_fullscreen_geometry_and_show_cleanly`：验证隐藏态不误发、首次显示按 `SetSize -> SetPosition -> SetAlwaysOnTop -> SetIgnoreCursorEvents -> Show` 顺序、切 `Stage` 进全屏跳过几何、切回 `Band` 先 `SetFullscreen(false)` 再恢复 `Band` 专属几何、重复相同规格输出空列表（防回环）；
   - `geometry_saver_debounces_and_skips_while_fullscreen`：在暂停的 tokio 时钟下连发 5 次移动/缩放，验证 400 ms 内仅发 1 次合并后的 `WindowRect`，且当 `fullscreen == true` 或 `captions == false` 时不派发；
   - `window_rect_clamping_rejects_degenerate_dimensions`：验证极小/超大尺寸被安全钳制。
2. `core/tests/app.rs` 新增测试：
   - `setup_caption_prefs_and_geometry_persist_per_scene_in_settings_json`：切换不同场合样式、发 `Command::CaptionGeometry` 仅改某场合矩形，重启 `Core` 后从 `settings.json` 完整读回，且旧版无 `caption` 字段的 `settings.json` 平滑加载默认值；
   - 验证 `State` 字节数在 5 条 vs 100 条录音下比值仍为 `1.00`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿；
- `scripts/build/rules/shell.sh`、`bounded.sh`、`spawn.sh`、`layers.sh`、`events.sh`、`numbers.sh` 全绿；
- `app/tests/windows.rs` 与 `app/tests/pump.rs` 全绿。

## 不做

- 不做 `CaptionScenesPanel`、`CaptionsWindow` 新渲染逻辑与手机观众页重构（留 P5-05）；
- 不做 `HostPage` / `FileArchiveCard` / `OutboundPage`（留 P5-04）。
