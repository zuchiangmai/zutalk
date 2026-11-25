状态：完成

## 改动与文件

完成 4seas / ZuTalk 设计系统、状态穷举映射、两栏主外壳、全局浮层与共享 jsdom 测试辅助：

1. **设计系统（`ui/src/ds/`）**：
   - `ui/src/ds/tokens.css`：移植 4seas 色板与 `.zt` / `.zt[data-theme="dark"]` 语义变量（白纸=只在本机、色带=对外、橙=仅正在收音、红=仅不可撤销），使用离线安全 Inter / LXGW WenKai / Oxanium 字体回退栈（零外网 Google Fonts `@import`），遵守 `prefers-reduced-motion: reduce`。
   - `ui/src/ds/controls.css`：移植 `zt.css` 控件、两栏外壳、实时字幕、边录边记与浮层样式，补充 `CommandPalette` 与 `HealthOverlay` 紧凑样式，并将电平条 `.meter i` 改为支持实时信号驱动的 `scaleY`。
   - `ui/src/ds/icons.tsx`：纯 SVG 图标组件集（零外部依赖）。
   - `ui/src/ds/primitives.tsx`：纯 props 控件（`Button`、`Pill`、`Seg`、`Tag`、`Card`、`InlineConfirm`），实现「原处再按一次」显式状态机 `ConfirmState` + `confirmStep` 与具名常量 `CONFIRM_ARM_TIMEOUT_MS = 4_000`。
   - `ui/src/ds/stateView.ts`：通过 TypeScript `never` 穷举检查实现 `sessionStatusView`（5 态）、`healthView`（4 态含 `Dead` 重连标记）、`refineStatusView`（4 态含无音频与 `keys.async_refused`）与 `keyFromView`（3 态）。
2. **两栏外壳与全局浮层（`ui/src/shell/`）**：
   - `ui/src/shell/Sidebar.tsx`：含纯函数 `groupRecentByDay` 与具名常量 `LIBRARY_SIDEBAR_RECENT_ROWS = 12`；支持新录音、⌘K 搜索入口、进行中录音置顶行（橙色 `.dot.live`）、今天/昨天/本周/更早分组、按月折叠（`LibrarySummary`）、全部录音计数入口与底部身份/设置。
   - `ui/src/shell/TopBar.tsx`：无共享房时显示白纸「只在本机」标志，有活动共享房时显示对外色带；非录音态有更新时提供「更新」按钮。
   - `ui/src/shell/NoticeToast.tsx`：普通通知 5 秒自动消隐，带 `undo` 命令通知 8 秒消隐并附「撤销」按钮。
   - `ui/src/shell/CommandPalette.tsx`：`⌘K` / `Ctrl+K` 全局搜索浮层，防抖 `SEARCH_DEBOUNCE_MS = 120` 毫秒请求 `Query::Search`（上限 `SEARCH_PALETTE_MAX_HITS = 50`），支持 `ArrowUp` / `ArrowDown` / `Enter` / `Escape` 键盘导航并按命中类型跳至阅读页对应标签与行。
   - `ui/src/shell/HealthOverlay.tsx`：开发态运行状况浮层（`Alt+H` 切换），可视化 `Water`（阶段位置、静默毫秒、`Health` 徽章与原因码）与 `Stats`（队列深度/上限/最深/满/丢弃、延迟 p50/p99/max、计数器）并提供一键导出诊断。
   - `ui/src/shell/Shell.tsx`、`ui/src/main.tsx`：引入设计系统样式表并装配完整两栏外壳与快捷键监听。
3. **测试与 8 语字典同步**：
   - `ui/tests/dom.ts`：提供确定性 `ResizeObserver`（800×600）与 `matchMedia` 桩供全部 DOM 测试复用。
   - `ui/tests/shell.test.tsx`：新增 10 条 DOM 与纯函数测试。
   - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`：8 份字典同步补齐 35 个外壳、分组、健康态、精修态、搜索与浮层词条。

## 验证证据

- 全部 10 个规则脚本通过（含 `scripts/build/rules/ui.sh` dsh 边界校验）。
- `cd ui && npm run check && npm test && npm run build` 全绿。
- UI 测试 7 个文件 50 条全部通过：
  - 新增 `ui/tests/shell.test.tsx`（10 条）：`stateView` 四组穷举（5 项 `Status`、4 项 `Health`、4 项 `RefineStatus`、3 项 `From`）、`confirmStep` 状态转换表与 `InlineConfirm` 原处双点确认、`Sidebar` 按日分组 + 进行中橙点 + 月份折叠跳转、`Shell` 内 `⌘K` 防抖搜索 + 键盘 `ArrowDown`/`Enter`/`Escape`、`NoticeToast` 5 秒消隐与撤销派发 `Restore`、`HealthOverlay` 渲染 `Stalled` 阶段与非零队列满计数及诊断按钮。
  - `ui/tests/words.test.ts`（12 条）验证 8 语键集与占位符完全一致。
- 本任务零 Rust 改动。

## 遗留与需负责人确认

- 业务页面（首页、录音中、录后精修、阅读页、全部录音、设置页）按计划在 P3-03 ～ P3-05 依次接入 `Shell.tsx`。
