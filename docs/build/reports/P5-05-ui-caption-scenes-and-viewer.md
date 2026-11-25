状态：完成

## 改了什么

1. **字幕窗纯样式规则与自动隐去（`ui/src/captions/style.ts`）**：
   - 新增具名带单位常量 `CAPTION_AUTO_HIDE_MS = 10_000`、`CAPTION_AUTO_HIDE_TICK_MS = 1_000`、`CAPTION_LINES_MIN = 1`、`CAPTION_LINES_MAX = 3`；
   - 实现纯函数 `isCaptionQuiet`、`bgAlpha`（`Clear: 0.42` / `Half: 0.72` / `Solid: 0.94`）、`sizeScale`（`Small: 0.85` / `Auto: 1` / `Large: 1.22`）、`sceneKey`、`activeStyle`、`sceneSummary`、`updateActiveStyle`（不可变更新当前场合不串扰其余场合）与 `sceneClassName`。
2. **字幕窗场合与设置面板（`ui/src/pages/CaptionScenesPanel.tsx`）**：
   - 新增 `{ page: 'captions' }` 路由并在 `Shell.tsx` 挂载 `CaptionScenesPage`，由 `SettingsPage.tsx`（「字幕窗 · 场合与设置」）与 `LivePage.tsx`（字幕窗旁快捷设置按钮）均可直达；
   - 左侧 16:10 `.screen` + `.capwin` 实时桌面预览，即时响应 4 场合（`SelfView`/`Band`/`Stage`/`Facing`）、语言、行数、字号、背景透明度、深/浅底色、楷/黑字体与上/下位置；
   - 右侧提供完整场合切换、语言开关（保护至少保留 1 种语言不可删空）、行数/字号/背景/底色/字体/位置（仅 `Band` 显示）、窗口开关（置顶、锁定穿透点击 + 主窗口解除锁定提示、自动隐去、舞台全屏）、开关字幕窗与重置窗口位置（清空当前场合 `geometry`）。
3. **4 场合独立字幕窗升级（`ui/src/captions/CaptionsWindow.tsx`）**：
   - 保持严格**零 `Query` 隔离**（仅订阅 `frameSource`、`sessionSource`、`stateSource`、`resolvedThemeSource`、`langSource`）；
   - 支持 `SelfView`/`Band`/`Stage` 样式与行数裁剪，以及 `Facing` 对照分栏布局（上方语区 `data-rotated="180"` 180° 翻转、中间 44 px 控制条含只在本机/对外中标签与 `最新一句`/`对照` 切换、下方正向）；
   - 接入 `auto_hide` 10 秒静默自动隐去（`data-quiet="true"`）与新帧到达即时唤醒。
4. **只留槽手机观众页与开发替身升级（`ui/src/viewer/{room.ts,ViewerPage.tsx}`、`ui/dev/rooms.ts`）**：
   - `ui/src/viewer/room.ts`：新增显式 `ViewerStatus`（`connecting`/`waiting`/`live`/`ended`/`error`）与纯转换函数 `viewerStatusStep`；空槽进 `waiting`、非空进 `live`、收到 `gone` 立即清空实时字幕切 `ended`、按 HTTP 404/429/解密失败/网络异常区分 `room.gone`/`room.viewers`/`room.bad`/`offline`；
   - `ui/src/viewer/ViewerPage.tsx`：对齐移动端 `ViewerLive.dc.html` 与 `ViewerEnded.dc.html`，支持等待开场态、语言大药丸切换单语底部对齐渐隐视图（`mask-image` + 最多保留 3 句历史 + 大号当前句）、结束锁屏页（「字幕结束了 · 这个链接已失效」）与清晰故障态；
   - `ui/dev/rooms.ts`：清理旧单条录音 `rec`/`log` 分支，改为只留槽 `/r/live`、`/r/waiting`、`/r/gone`；
   - 更新 `ui/src/ds/controls.css` 与全部 8 语 `ui/lang/*.json`（新增 40 个键）。

## 证据

- `bash scripts/build/rules/ui.sh`：通过（零违规）。
- `cd ui && npm run check && npm run build`：通过，`ui/dist/{index,captions,viewer}.html` 三入口全量构建通过。
- `cd ui && npm test`：**14/14 文件、106/106 测试通过**（含 `words.test.ts` 8 语全量校验、`ui/tests/room.test.ts` 4/4 与 `ui/tests/captions_viewer.test.tsx` 全部 5 条测试）：
  - `caption_style_and_auto_hide_rules`
  - `CaptionScenesView_switches_scenes_guards_last_language_and_updates_window_flags`
  - `CaptionsWindow_renders_facing_rotated_180_and_makes_zero_queries`
  - `CaptionsWindow_auto_hides_after_10s_silence_and_wakes_on_new_frame`
  - `ViewerView_renders_waiting_live_language_focus_and_clears_on_gone`
  - `room.test.ts::viewerStatusStep covers all lifecycle transitions cleanly`
- `ui/package-lock.json` 与 `Cargo.lock` 零改动。

## 遗留 / 需协调人注意

- 端到端崩溃双保险测试、真实落盘隐私字节审计、升级 `pending.sh` 与 P5 收尾文档留给下一件任务 `P5-06`。
