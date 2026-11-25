# P5-04 界面：主持流（开场前 / 对外中）、按住交档案馆与设置 · 对外

## 目标

做完以后：

1. **主持 · 开场前（`ui/src/pages/HostPage.tsx`，对照 `HostArm.dc.html`）**：
   - 从首页次级入口「主持一场」进入（未登录时触发按需身份纸 `IdentitySheet`，绝不在首页设登录墙）；
   - 顶部根据观众字幕开关切换：**开启时展示色带 `.band`**（「这一场对外 · 观众字幕 · {活动名} · 取消主持」），关闭时展示白纸 `.top`（「只在本机 · 录完可交到：{活动名} · 取消主持」）；
   - 单一主按钮「开始主持」（`Command::Start`）+ 主持单卡片（`.sheet`）：
     - 活动选择：输入/记忆城地址（`Setup.city`）加载日历（`Command::City` → `Query::Calendar`），默认选中离当前时间最近的活动，点击可循环或下拉切换；
     - 观众字幕开关（`Command::HostArm { event, captions }`）：开启时展示实时等待人数（`{N} 人在等`）、`uqr` 离线生成二维码 SVG、截断链接、「复制链接」与「放大二维码」弹窗；
     - 档案馆一行写明「录完再定」；下方回显本场「说 / 字幕」语言；
   - **离开即结束**：从 `{ page: 'host' }` 导航至非 `'live'` 的任意页面（或点「取消主持」），自动派发 `Command::HostCancel` 销毁房间并清 `host.lock`。
2. **主持 · 对外中（`ui/src/shell/TopBar.tsx`、`ui/src/pages/LivePage.tsx`、`ui/src/shell/Sidebar.tsx`，对照 `HostLive.dc.html`）**：
   - 当 `session?.outbound`（或主持中且对外）时，顶栏显示彩色 `.band`：「对外 · `{viewers} 人在看` · 观众字幕 · {活动名} · 复制链接/二维码 · **停止对外**」；
   - 点「停止对外」一键发 `Command::StopOutbound`：本机录音丝毫不停，顶栏立即切回白纸「只在本机」，并显示「已停止对外」提示；
   - 侧栏正在录音行在 `session.outbound == true` 时显示对外小箭头图标。
3. **主持 · 按住 1.4 秒交档案馆与 8 秒可撤销（`ui/src/bus/filing.ts`、`ui/src/pages/FileArchiveCard.tsx`，对照 `HostFile.dc.html`）**：
   - 显式状态机 `FilePhase`（`'review' | 'holding' | 'grace' | 'submitting' | 'filed' | 'failed' | 'kept'`）与纯函数 `filePhaseStep`，具名常量 `HOLD_TO_FILE_MS = 1_400`、`FILE_GRACE_MS = 8_000`；
   - 停止一场主持录音落到 `ResultPage` 时自动带入 `hostEvent` 展开「交到档案馆」卡片（`ReaderPage` 也可随时打开同一张卡片）；
   - 显示目标活动选择（无日历时支持填入城地址加载）、将公开的转写语言与说话人标签、全程录音开关（`一起交` / `不交` + 按每小时 ~115 MB 估算体积；音频已销毁时隐藏/禁用开关）、公开提示横幅「交上去是公开的。全程录音交了删不掉。」；
   - 「按住交出」支持鼠标/触屏指针按住与键盘 `Space`/`Enter` 按住 1.4 秒填充进度条（提前松手立即取消），按满进入 8 秒倒数环「8 秒后交出」与「撤销」按钮（**8 秒内完全不发 `Command::File`**）；点「撤销」立刻退回检查态；8 秒到期才发 `Command::File { id, event, audio }`；随后跟随 `Progress.file` 与 `recording.filed` 显示上传进度、已交或失败重交。
4. **设置 · 对外（`ui/src/pages/OutboundPage.tsx`、`ui/src/pages/SettingsPage.tsx`，对照 `Outbound.dc.html`）**：
   - 设置页新增「对外」入口行跳至 `{ page: 'outbound' }`；
   - 「现场字幕」组展示当前是否正在对外（若对外中显示人数与「停止对外」按钮，否则显示「现在没有在对外」）；
   - 「档案馆」组通过 `Query::Outbound` 拉取全部交城记录，逐条展示录音标题、城与活动、状态（`已交` + 文字/录音摘要、上传进度条 + 正在交全程录音、`未发送` + 原因码），点击可打开对应阅读页。
5. **样式、mock、8 语字典与 `ui.sh` 守卫**：
   - `ui/src/ds/controls.css` 补齐 `.sheet`、`.sheet-row`、`.hold`、`.hold .fill`、`.ring`、`@keyframes zt-grace` 与二维码放大弹窗样式（全走离线 `tokens.css` 令牌，零外网 `@import`）；
   - `ui/dev/mock.ts` 实现交互式 `HostArm`/`HostCancel`/`StopOutbound`/`City`/`File` 模拟；全部 8 语 `ui/lang/*.json` 同步补齐所有新键。

## 背景

- 设计：`docs/build/phases/P5.md` §3.6；`docs/PLAN.md` §3、§5.5、§6.3；`docs/ui-2026-11-21/{HostArm,HostLive,HostFile,Outbound}.dc.html` 与 `zt.css`。
- 涉及文件：
  - `ui/src/bus/route.ts`、新增 `ui/src/bus/filing.ts`、`ui/src/bus/index.ts`、`ui/src/bus/channel.ts`
  - `ui/src/ds/controls.css`、`ui/src/ds/icons.tsx`（补齐 `broadcast`/`qrcode`/`undo`/`archive` 等所需 SVG 路径）
  - 新增 `ui/src/pages/HostPage.tsx`、`ui/src/pages/FileArchiveCard.tsx`、`ui/src/pages/OutboundPage.tsx`
  - 更新 `ui/src/shell/{Shell,TopBar,Sidebar}.tsx`、`ui/src/pages/{HomePage,LivePage,ResultPage,ReaderPage,SettingsPage}.tsx`
  - `ui/dev/mock.ts`、`ui/lang/*.json`（8 语）
  - 新增 `ui/tests/host_outbound.test.tsx`，更新受影响的现有 UI 测试

## 做法

### 1. 路由扩展与按住交城状态机（`ui/src/bus/route.ts`、`ui/src/bus/filing.ts`）

- `ui/src/bus/route.ts`：
  - `Route` 增补 `{ page: 'host' }`、`{ page: 'outbound' }`，并在 `{ page: 'result'; id: string; hostEvent?: HostEvent }` 携带可选 `hostEvent`；
  - `routeStep` 处理 `SessionStopped { id, hostEvent? }` 与离开 `'host'` 的判定；
  - 新增纯辅助 `leavingHostArm(prev: Route, next: Route): boolean`：`prev.page === 'host' && next.page !== 'host' && next.page !== 'live'`，在 `Shell` 或 `route.ts` 的 `go()` 检测到 `leavingHostArm` 且当前 `state?.host.is !== 'Idle'` 时自动调用 `send({ do: 'HostCancel' })`。
- `ui/src/bus/filing.ts`：
  - 命名常量：
    - `export const HOLD_TO_FILE_MS = 1_400;` // required press duration before entering grace period (`HostFile.dc.html`)
    - `export const FILE_GRACE_MS = 8_000;` // undoable grace window before dispatching Command::File (`HostFile.dc.html`)
    - `export const ESTIMATED_WAV_MB_PER_HOUR = 115;` // uncompressed 16 kHz mono WAV archive estimate (`DESIGN.md` §5.11)
  - `FilePhase = 'review' | 'holding' | 'grace' | 'submitting' | 'filed' | 'failed' | 'kept'`；
  - `FilePhaseEvent`：`HoldStart`、`HoldCancel`、`HoldComplete`、`GraceUndo`、`GraceExpire`、`Keep`、`Reopen`、`ServerFiled`、`ServerFailed`；
  - 纯函数 `export function filePhaseStep(phase: FilePhase, event: FilePhaseEvent): FilePhase`；
  - 纯辅助 `export function pickNearestEvent(events: readonly CityEvent[], nowMs: number): CityEvent | null`（找 `start` 离 `nowMs` 最近的活动，无 `start` 时退回第一条）；
  - 纯辅助 `export function estimateAudioMb(lengthMs: number): number`（`Math.max(1, Math.round((lengthMs / 3_600_000) * ESTIMATED_WAV_MB_PER_HOUR))`）。

### 2. 纯 props 视图 + 薄总线容器（严格守 `ui.sh` dsh 边界）

- `ui/src/pages/HostPage.tsx`（`HostView` + `HostPage`）：
  - 用 `uqr` 的 `renderSVG` 或 `encode` 把 `host.link` 画成 `<svg><path d="..." /></svg>`（**绝不用 `dangerouslySetInnerHTML`**！照 `IdentitySheet.tsx` 用 `encode()` 生成 `<path d={...} />` 过 `ui.sh`！）；
  - 复制链接经 `navigator.clipboard.writeText(link)` 并提示 `copied`；「放大二维码」开本地受控弹窗；
  - `HostPage` 进入时若已有 `state.setup.city` 且未加载日历，自动发 `Command::City { address }`；若尚未 `HostArm` 且已登录，自动发 `Command::HostArm { event: selectedEvent, captions: true }`。
- `ui/src/shell/TopBar.tsx` 与 `ui/src/pages/LivePage.tsx`：
  - `TopBarProps` 接收 `host: HostState`、`session: Session | null`、`onStopOutbound?: () => void`、`onCancelHost?: () => void`、`onCopyHostLink?: () => void`；
  - 只有 `host.is === 'Armed' && host.captions` 或 `session?.outbound` 时显示彩色 `.band`，否则一律白纸 `.top`（原则：白纸只在本机，色带表示离开设备）；
  - `LiveView` 在 `session.outbound` 为真时在 `.band` 展示人数与「停止对外」按钮，点击调 `onStopOutbound`（发 `Command::StopOutbound`）。
- `ui/src/pages/FileArchiveCard.tsx`（`FileArchiveView` + `FileArchiveCard`）：
  - 受控 `phase` 通过 `filePhaseStep` 驱动，`useEffect` 管理 `HOLD_TO_FILE_MS` 与 `FILE_GRACE_MS` 计时器并在 unmount 时清理（不泄漏定时器）；
  - 8 秒到期时才调 `onConfirmFile(eventPath, audio)` → `send({ do: 'File', id, event, audio })`；
  - 未登录点击「按住交出」前若 `me == null`，触发 `openIdentitySheet('ask')` 不进入计时；
  - 挂载在 `ResultPage.tsx`（有 `hostEvent` 时默认展开，其余也可通过「交到档案馆」按钮展开）与 `ReaderPage.tsx`（已有 `filed` 记录时展示状态卡，未交时提供「交到档案馆」展开入口）。
- `ui/src/pages/OutboundPage.tsx`（`OutboundView` + `OutboundPage`）：
  - 订阅 `stateSource`、`sessionSource`、`progressSource` 与 `useQuery({ view: 'Outbound' })`；
  - 展示现场对外状态 + 全部 `OutboundRow` 列表（`filed` / `going` / `failed` 三态），点击每条跳 `go({ page: 'reader', id })`。
- 同步 `ui/src/pages/HomePage.tsx`（`.stage-foot` 增补次级按钮「主持一场」）、`ui/src/pages/SettingsPage.tsx`（增补「对外」导航行）、`ui/src/shell/Shell.tsx`（接入 `host` 与 `outbound` 路由）、`ui/dev/mock.ts`、8 语 `ui/lang/*.json`。

## 显式与可观测

- **纯状态机与纯规则**：`filePhaseStep`、`leavingHostArm`、`pickNearestEvent`、`estimateAudioMb` 均带单元测试覆盖全部转换与边界。
- **具名带单位常量**：`HOLD_TO_FILE_MS`、`FILE_GRACE_MS`、`ESTIMATED_WAV_MB_PER_HOUR`。
- **dsh 与安全守卫**：二维码用 `uqr::encode` 渲染纯 JSX `<path>`（零 `dangerouslySetInnerHTML`）；零 `useSyncExternalStore`/IPC 越界、零 `console.log`、零外网 CSS（全过 `ui.sh`）。
- **状态可见**：对外色带与白纸严格互斥；等待人数/观看人数、按住进度、8 秒倒数环与取消、交城上传进度与失败代码均以 `role="status"` 明确呈现。

## 先写的测试

1. 新建 `ui/tests/host_outbound.test.tsx`（`// @vitest-environment jsdom`）：
   - `filePhaseStep_transition_table`：穷举 `FilePhase` × `FilePhaseEvent` 转换表；
   - `leavingHostArm_triggers_cancel_except_entering_live`：验证从 `'host'` 跳 `'home'`/`'settings'`/`'reader'` 返回 `true`，跳 `'live'` 返回 `false`；
   - `HostView_shows_band_qr_waiting_viewers_and_toggles_quiet_paper`：断言开启观众字幕显示 `.band`、等待人数与 SVG 二维码；关掉字幕切换回白纸「只在本机」；
   - `LiveView_outbound_band_shows_viewers_and_stop_outbound_keeps_recording`：断言 `.band` 显示人数与「停止对外」，点击后发 `StopOutbound` 而未发 `Stop`；
   - `FileArchiveView_requires_1400ms_hold_and_8s_undo_cancels_before_dispatch`：用 `vi.useFakeTimers()` 测：
     - 按住 800 ms 松手 → 回到 `'review'`，未发 `File`；
     - 按满 1,400 ms → 进入 `'grace'` 展示「8 秒后交出」与「撤销」，此时仍**未发** `File`；
     - 在第 5 秒点「撤销」→ 退回 `'review'`，再推进 10 秒仍**零** `File` 调用；
     - 再次按满 1,400 ms 并推进 8,000 ms → 精确发出 1 次 `File(event, audio)`；
   - `OutboundView_renders_idle_or_live_and_all_filing_states`：断言「现在没有在对外」、`已交`、`正在交全程录音`、`未发送` 三态行及点击跳转阅读页。
2. `ui/tests/words.test.ts` 全量 8 语一致性核对通过。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `scripts/build/rules/ui.sh`、`npm run check`、`npm test`）；
- `npm --prefix ui run build` 产出的三个入口（`index.html`、`captions.html`、`viewer.html`）全部正常打包。

## 不做

- 不做 `CaptionScenesPanel`、4 场合 `CaptionsWindow` 升级与新版手机 `ViewerPage`（留 P5-05）；
- 不修改 Rust 代码（全部协议与命令已在 P5-01–P5-03 就位）。
