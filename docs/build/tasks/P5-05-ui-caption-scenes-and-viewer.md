# P5-05 界面：字幕窗场合与设置、4 场合独立字幕窗与手机观众页

## 目标

做完以后：

1. **字幕窗场合与设置面板（`ui/src/pages/CaptionScenesPanel.tsx`，对照 `CaptionScenes.dc.html`）**：
   - 可从设置页「字幕窗 · 场合与设置」进入（同时在 `LivePage` 顶部字幕窗按钮旁提供快捷设置跳转），改动全部经 `Command::Setup` 写入 `Setup.caption`（落盘 `settings.json` 并实时驱动桌面 `app/src/windows.rs`）；
   - 左侧提供 16:10 桌面屏幕实时预览（`.screen` + `.capwin`），即时反映场合、语言、行数、字号、背景透明度、深/浅底色、楷/黑字体、上/下位置与叠放/并排；
   - 右侧提供完整控制：
     - 场合：`自己看`（`SelfView`）/ `投屏`（`Band`）/ `舞台`（`Stage`）/ `对照`（`Facing`）；
     - 语言：切换当前 `setup.shown`（至少保留 1 种语言，不可删空）；
     - 行数：`1` / `2` / `3`；字号：`小` / `自动` / `大`；
     - 背景：`透`（`Clear`）/ `半`（`Half`）/ `实`（`Solid`）；底色：`深` / `浅`；字体：`楷` / `黑`；
     - 位置：`下` / `上`（仅 `Band` 场合显示）；
     - 窗口开关：`置顶`（`always_on_top`）、`锁定 / 穿透点击`（`click_through`，开启时明确提示「在主窗口可解除锁定」）、`自动隐去`（`auto_hide`）、`舞台全屏`（`fullscreen`），另附「打开/关闭字幕窗」与「重置窗口位置」（清空该场合 `geometry: null`）。
2. **独立字幕窗升级（`ui/src/captions/CaptionsWindow.tsx`、`ui/src/captions/style.ts`，对照 `CaptionScenes.dc.html` & `CaptionsFacing.dc.html`）**：
   - **严格保持零 `Query` 隔离**（只订阅 `frameSource`、`sessionSource`、`stateSource`，绝不调 `useQuery`）；
   - 按 `state.setup.caption` 激活场合渲染：
     - `SelfView` / `Band` / `Stage`：按 `lines`（1–3 行裁剪）、`size`（缩放系数）、`bg`（`Clear`/`Half`/`Solid` 透明度）、`light`（深绿 `#012A1F` / 浅白 `#FFFFFF`）、`sans`（楷/黑）、`Stage` 叠放行模式渲染；
     - `Facing`（`CaptionsFacing.dc.html`）：上下两语对照分栏，上方语种 `transform: rotate(180deg)` 供对面人直接阅读，中间工具条含「只在本机 / 对外中」标签、「最新一句 / 对照」切换与关闭按钮，下方语种正向显示；
   - **自动隐去（`auto_hide`）**：纯函数 `isCaptionQuiet(lastFrameAtMs, nowMs, autoHide)` 使用 `CAPTION_AUTO_HIDE_MS = 10_000`；静默满 10 秒柔和淡出字幕面，新帧到达瞬间显现。
3. **手机观众页全面升级（`ui/src/viewer/ViewerPage.tsx`、`ui/src/viewer/room.ts`、`ui/dev/rooms.ts`，对照 `ViewerLive.dc.html` & `ViewerEnded.dc.html`）**：
   - `viewer/room.ts` 保持纯只留槽 AES-256-GCM 解密（`slot` + `gone`），增加显式连接状态 `ViewerStatus = 'connecting' | 'waiting' | 'live' | 'ended' | 'error'` 与纯函数 `viewerStatusStep`；
   - 收到 `gone` 时立即清空实时字幕数组，确保主持人停止对外后观众手机上不留任何残句；
   - `ViewerPage.tsx` 对齐移动端 `ViewerLive.dc.html`（390 px 友好排版）：
     - 顶栏 `ZuTalk` + 实时绿点「现场字幕」标签（空槽时显示「等待开场」）；
     - 顶部语言选择大药丸（`中文` / `English` / `ไทย` / `全部`），默认或选中单语时底部对齐显示历史行与当前正在说的大号行，并带顶部渐隐遮罩（`mask-image: linear-gradient(to bottom, transparent 0, #000 42%)`）；
     - 结束态对齐 `ViewerEnded.dc.html`：居中锁形徽章 + **字幕结束了** + **这个链接已失效**；
     - 区分显示 `room.bad`（链接密钥无效）、`room.viewers`（人数已满）、`offline`（已断线）；
   - 更新 `ui/dev/rooms.ts` 为只留槽开发替身（`/r/live`、`/r/waiting`、`/r/gone`）。
4. **8 语字典同步与 `ui.sh` 洁净**：
   - 补齐全部 8 语 `ui/lang/*.json` 新增键，确保 `words.test.ts` 与 `ui.sh` 全绿。

## 背景

- 设计：`docs/build/phases/P5.md` §3.4、§3.6；`docs/PLAN.md` §2.2、§3、§6.3；`docs/ui-2026-11-21/{CaptionScenes,CaptionsFacing,ViewerLive,ViewerEnded}.dc.html` 与 `zt.css`。
- 涉及文件：
  - 新增 `ui/src/captions/style.ts`、`ui/src/pages/CaptionScenesPanel.tsx`
  - 更新 `ui/src/captions/CaptionsWindow.tsx`、`ui/src/pages/SettingsPage.tsx`、`ui/src/pages/LivePage.tsx`、`ui/src/shell/Shell.tsx`、`ui/src/bus/route.ts`（如需 `{ page: 'captions_settings' }` 或内嵌设置页子区）
  - 更新 `ui/src/viewer/room.ts`、`ui/src/viewer/ViewerPage.tsx`、`ui/dev/rooms.ts`
  - `ui/src/ds/controls.css`、`ui/lang/*.json`（8 语）
  - 更新 `ui/tests/captions_viewer.test.tsx`、`ui/tests/room.test.ts`

## 做法

### 1. 纯样式与自动隐去规则（`ui/src/captions/style.ts`）

- 具名带单位常量：
  - `export const CAPTION_AUTO_HIDE_MS = 10_000;` // fade idle caption window after 10 s of silence (`CaptionScenes.dc.html`)
  - `export const CAPTION_AUTO_HIDE_TICK_MS = 1_000;` // check quiet state once per second
- 纯函数：
  - `export function isCaptionQuiet(lastFrameAtMs: number | null, nowMs: number, autoHide: boolean): boolean`：仅在 `autoHide === true` 且 `lastFrameAtMs !== null && nowMs - lastFrameAtMs >= CAPTION_AUTO_HIDE_MS` 时返回 `true`；
  - `export function bgAlpha(bg: CaptionBg): number`：`Clear => 0.42`、`Half => 0.72`、`Solid => 0.94`；
  - `export function sizeScale(size: CaptionSize): number`：`Small => 0.85`、`Auto => 1`、`Large => 1.22`；
  - `export function sceneSummary(scene: CaptionScene, style: SceneStyle): { stacked: boolean; lines: number }`；
  - `export function updateActiveStyle(prefs: CaptionPrefs, patch: Partial<SceneStyle>): CaptionPrefs`（不可变更新对应场合样式，不碰其他场合）。

### 2. `ui/src/pages/CaptionScenesPanel.tsx`

- 纯 props `CaptionScenesView` + 薄总线容器 `CaptionScenesPage`（从 `SettingsPage` 独立卡片/子页进入，或路由 `{ page: 'captions' }` 从设置页与直播页均可直达）。
- 16:10 `.screen` 预览内嵌 `.capwin`（按 `CaptionScenes.dc.html` CSS 变量 `--a`、`--k`、`--l` 实时驱动，绝不依赖外网字体），并展示当前排布（「叠放」/「并排」）、行数标签与「记在这台设备」私密标。
- 语言开关禁止取消最后一种语言（`if (shown.length <= 1 && isOn) return;`）。
- 当 `style.click_through` 为 `true` 时，在开关旁以清晰提示标明「字幕窗已穿透点击，可在此处解除锁定」。

### 3. `ui/src/captions/CaptionsWindow.tsx`

- 仅订阅 `frameSource`、`sessionSource`、`stateSource`、`langSource`（**绝不引入 `useQuery`**）。
- 记录收到最新非空 `Frame` 的时间戳 `lastFrameAtMs`，在 `style.auto_hide` 开启时按 `CAPTION_AUTO_HIDE_TICK_MS` 检查 `isCaptionQuiet`，安静时给根容器加 `data-quiet="true"`（CSS 平滑降透明度，仍保留顶部控件交互当非穿透时）。
- `scene === 'Facing'` 时渲染 `CaptionsFacing` 布局：
  - 上半区取第二栏（或单栏时复用第一栏）并施加 `data-rotated="180"`（`transform: rotate(180deg)`）；
  - 中间 44 px 控制栏：`只在本机` 或 `对外中` 标签 + `最新一句`（切回 `SelfView`）/`对照`（`Facing`）分段切换 + 关闭按钮；
  - 下半区取第一栏正向渲染最近历史行 + 正在说的大号行。
- `scene !== 'Facing'` 时按 `style` 应用 `.capwin` 样式类（`sc-self`/`sc-band`/`sc-stage`、`rows`、`light`、`sans`、背景透明度与行数上限裁剪每栏可见行数）。

### 4. 手机观众页与只留槽客户端（`ui/src/viewer/room.ts`、`ui/src/viewer/ViewerPage.tsx`）

- `ui/src/viewer/room.ts`：
  - `export type ViewerStatus = 'connecting' | 'waiting' | 'live' | 'ended' | 'error';`
  - 纯函数 `viewerStatusStep(status, event) -> ViewerStatus`；
  - `slot` 收到 `-` 或空数组 → `roomLiveCell.set([])`、状态 `'waiting'`；收到非空 `Column[]` → 更新列、状态 `'live'`；
  - `gone` → `roomLiveCell.set([])`、状态 `'ended'`、`roomEndCell.set('room.gone')`、关闭 `EventSource`；
  - 解密失败 → `roomLiveCell.set([])`、状态 `'error'`、`roomEndCell.set('room.bad')`；
  - HTTP 404 → `'ended'`（`'room.gone'`）；HTTP 429 → `'error'`（`'room.viewers'`）；网络异常 → `'error'`（`'offline'`）。
- `ui/src/viewer/ViewerPage.tsx`：
  - `status === 'ended'`：渲染 `ViewerEnded.dc.html`（居中锁图标 + 「字幕结束了」 + 「这个链接已失效」）；
  - `status === 'waiting' || status === 'connecting'`：显示顶栏与柔和等待提示（「等待开场」）；
  - `status === 'live'`：显示绿点「现场字幕」标签、语言大药丸切换、单语底部对齐渐隐列表（大号当前句 + 淡出前 3 句，多语选「全部」时复用 `LiveCaptions`）。
- 更新 `ui/dev/rooms.ts` 移除旧 `rec` 录音日志分支，改为 `/r/live`、`/r/waiting`、`/r/gone` 三个只留槽演示房。

## 显式与可观测

- **纯函数状态机**：`isCaptionQuiet`、`viewerStatusStep`、`updateActiveStyle`、`bgAlpha`/`sizeScale` 均有独立单元测试。
- **具名带单位常量**：`CAPTION_AUTO_HIDE_MS`、`CAPTION_AUTO_HIDE_TICK_MS`。
- **零 `Query` 隔离与 dsh 边界**：`CaptionsWindow` 零查询测试持续守卫；`ui.sh` 全绿。
- **故障与终态可见**：房间结束、密钥无效、人数满员、离线、空槽等待在手机观众页各有明确文案与 `role="status"`。

## 先写的测试

1. 更新 `ui/tests/captions_viewer.test.tsx`（`// @vitest-environment jsdom`）：
   - `caption_style_and_auto_hide_rules`：测 `isCaptionQuiet` 在 9,999 ms / 10,000 ms 及 `autoHide = false` 下的输出，`updateActiveStyle` 只改当前场合不串扰其他场合；
   - `CaptionScenesView_switches_scenes_guards_last_language_and_updates_window_flags`：测切场合、不可删掉最后一个语言、切置顶/穿透点击/舞台全屏回调；
   - `CaptionsWindow_renders_facing_rotated_180_and_makes_zero_queries`：断言 `Facing` 场合渲染 `data-rotated="180"` 顶部语区且全程 `query` 调用次数为 `0`；
   - `CaptionsWindow_auto_hides_after_10s_silence_and_wakes_on_new_frame`：用 `vi.useFakeTimers()` 验 10 秒静默 `data-quiet="true"`、新帧到来恢复 `"false"`；
   - `ViewerView_renders_waiting_live_language_focus_and_clears_on_gone`：测等待态、多语切换单语、收到 `gone` 后立即清空旧字幕并显示「字幕结束了 · 这个链接已失效」、`room.bad` 与 `room.viewers` 清晰报错。
2. 更新 `ui/tests/room.test.ts`：覆盖 `viewerStatusStep` 转换表、AES-256-GCM 槽解密、错误密钥拒开。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `ui.sh`、`words.test.ts`）；
- `npm --prefix ui run build` 完整构建主窗口、字幕窗、观众页三入口无警告。

## 不做

- 不修改 Rust 服务器或核心逻辑；
- 不升级 `pending.sh` 与写收尾报告（留 P5-06）。
