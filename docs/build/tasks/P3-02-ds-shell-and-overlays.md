# P3-02 设计系统、两栏外壳、⌘K 快速搜索与运行状况浮层

## 目标

做完以后：

1. **4seas / ZuTalk 设计系统（`ui/src/ds/`）**：
   - `tokens.css` & `controls.css`：忠实移植 `docs/ui-2026-11-21/zt.css` 与 `ds/ds4seasdesignsystem_af6e46/colors_and_type.css` 的全部色板、字体（Inter / 霞鹜文楷 / Oxanium 离线安全回退栈）、动效时长、亮/暗主题（`[data-theme="dark"]`）与视觉不变量：白纸 = 只在本机，色带 = 对外，橙色（`--live`）只表示正在收音，红色（`--danger`）只表示不可撤销；尊重 `prefers-reduced-motion: reduce`；
   - `icons.tsx`：纯 SVG 图标组件集（零外部图标库依赖）；
   - `primitives.tsx`：纯 props 基础控件（`Button`、`Pill`、`Seg`、`Tag`、`Card`、`InlineConfirm` 原处再按一次状态机组件）；
   - `stateView.ts`：覆盖 `States.dc.html` 的纯函数状态映射表，对 `Status`、`Health`、`RefineStatus`、`From` 做 TypeScript 穷举映射（零硬编码临时提示）。
2. **两栏主外壳（`ui/src/shell/`）**：
   - `Shell.tsx`：连接 `bus` 源，将纯 props 传给侧栏、顶栏、当前路由页面与全局浮层；监听全局快捷键 `⌘K` / `Ctrl+K`（打开搜索浮层）与 `Alt+H`（开发构建切换运行状况浮层）；
   - `Sidebar.tsx`：新录音按钮、带 `⌘K` 提示的搜索入口、进行中录音置顶行（橙点 + 实时计时）、近期录音分组（`LibraryRecent` 按今天/昨天/本周分组）、更早按月折叠项（`LibrarySummary`，点击跳转到 `library` 对应月份）、「全部录音」入口（带总数）、底部身份摘要与设置按钮；
   - `TopBar.tsx`：左侧白纸「只在本机」标志（若有活动共享房则显示色带摘要），右侧页面槽位与非录音时的「更新」主按钮；
   - `NoticeToast.tsx`：底部浮出胶囊，普通通知 5 秒自动消隐，带 `undo` 命令的通知 8 秒消隐并附「撤销」按钮；
   - `CommandPalette.tsx`：`⌘K` / `Ctrl+K` 全局快速搜索弹窗，防抖 120 ms 调用 `Query::Search`，按标题/正文/译文/笔记展示命中片段，支持键盘 `↑`/`↓`/`Enter`/`Esc` 跳转到对应录音与行；
   - `HealthOverlay.tsx`：开发态运行状况浮层（`PLAN.md` §4.5.2），实时渲染 `Water`（各阶段水位、停滞毫秒、`Health` 徽章、`why`）与 `Stats`（计数、高水位、延迟分布 p50/p99、各队列容量/深度/满/丢弃）。
3. **共享 jsdom 测试辅助（`ui/tests/dom.ts`）**：提供确定性 `ResizeObserver`（固定视口高度 600 px）与 `matchMedia` 桩，供 P3-02 ～ P3-07 全部 React DOM 测试复用。
4. **8 语字典同步补全**：把本任务新增的全部壳与状态词条同步写入 `ui/lang/*.json` 全部 8 份文件，`words.test.ts` 全绿。

## 背景

- 设计：`docs/build/phases/P3.md` §3.2、§3.4–§3.6；`docs/ui-2026-11-21/{zt.css,System.dc.html,States.dc.html,Library.dc.html}`；`docs/PLAN.md` §4.5.2、§6.3。
- 涉及文件：
  - 新增 `ui/src/ds/{tokens.css,controls.css,icons.tsx,primitives.tsx,stateView.ts}`
  - 完善 `ui/src/shell/Shell.tsx`，新增 `ui/src/shell/{Sidebar,TopBar,NoticeToast,CommandPalette,HealthOverlay}.tsx`
  - 更新 `ui/src/main.tsx`（引入 `ds/tokens.css` 与 `ds/controls.css`）
  - 新增 `ui/tests/dom.ts`、`ui/tests/shell.test.tsx`
  - 更新 `ui/lang/*.json`（8 份文件）

## 做法

### 1. 设计系统（`ui/src/ds/`）

- `tokens.css`：搬入 `4seas` 调色板变量与 `.zt` / `.zt[data-theme="dark"]` 语义令牌；**禁止**在 CSS 里 `@import url('https://fonts.googleapis.com/...')`（桌面离线环境不能外网阻塞），使用与 `zt.css` 一致的本地字体回退栈（`'Inter', -apple-system, ...`、`'LXGW WenKai', 'LXGW WenKai TC', 'Kaiti SC', ...`、`'Oxanium', 'Inter', ...`）。
- `controls.css`：移植 `zt.css` 全部组件与版面类（`.pill`/`.btn`/`.seg`/`.field`/`.tag`/`.card`/`.side`/`.main`/`.band`/`.stage`/`.rec`/`.caps`/`.capsule`/`.doc`/`.tl`/`.pop`/`.menu`/`.toast`/`.steps`/`.bar`/`.page`/`.lib`/`.rail`/`.scrim`/`.dialog`/`.split`/`.notes`/`.capwin` 等），并补充 `HealthOverlay` 与 `CommandPalette` 紧凑样式。
- `primitives.tsx`：
  - 全部组件仅收纯 props（无 `bus` 导入）。
  - `InlineConfirm`（「销毁音频 · 原处再按一次」，`States.dc.html`）：
    - 具名常量：`CONFIRM_ARM_TIMEOUT_MS = 4_000`；
    - 显式状态：`ConfirmState = { is: 'idle' } | { is: 'armed'; since: number }` + 纯函数 `confirmStep(state, event, now)`（事件：`Press` / `Timeout` / `Cancel`）；
    - 首按从红字 `pill danger` 变成红底白字「确认销毁音频」，再次按下触发 `onConfirm()` 并回到 `idle`，超 `4000 ms` 或失焦自动回 `idle`。
- `stateView.ts`：
  - 纯函数，用 `exhaustive(x: never)` 保证编译期穷举：
    - `sessionStatusView(status: Status)` → `{ tone: 'live' | 'paused' | 'warn' | 'muted', labelKey, detailCode? }`
    - `healthView(health: Health)` → `{ tone: 'ok' | 'warn' | 'danger', labelKey, whyCode?, canReconnect: boolean }`
    - `refineStatusView(refine: RefineStatus, hasAudio: boolean)` → `{ kind: 'draft' | 'refining' | 'refined' | 'failed', labelKey, detailCode?, canRefine: boolean }`
    - `keyFromView(from: From)` → `{ labelKey, tone: 'ok' | 'accent' | 'muted' }`

### 2. 两栏外壳与全局浮层（`ui/src/shell/`）

- 命名常量（集中定义并附单位与理由注释）：
  - `LIBRARY_SIDEBAR_RECENT_ROWS = 12`（侧栏近期最多展示 12 条，更早折叠进月度项）
  - `SEARCH_DEBOUNCE_MS = 120`（⌘K 键入防抖，减少无谓 SQLite 取消）
  - `SEARCH_PALETTE_MAX_HITS = 50`（⌘K 快速弹窗结果上限）
- `Sidebar.tsx`：纯 props 接收 `recent: LibraryRow[]`、`summary: LibrarySummary | null`、`session: Session | null`、`route: Route`、`me: string | null` 与回调。把 `recent` 用纯函数 `groupRecentByDay(rows, nowMs)` 分为「今天 / 昨天 / 本周 / 更早」；下方列出 `summary.months` 的折叠月份与「全部录音（总数）」按钮。
- `CommandPalette.tsx`：打开时聚焦输入框；按 `SEARCH_DEBOUNCE_MS` 防抖后请求 `Query::Search { query, limit: SEARCH_PALETTE_MAX_HITS }`；展示命中类型徽章（`Title`/`Line`/`Translation`/`Note`）与 `snippet`；支持 `ArrowDown` / `ArrowUp` / `Enter`（跳转 `reader` 并传入 `lineKey`）/ `Escape`。
- `HealthOverlay.tsx`：展示 `Water`（每条 `Mark` 对应阶段、位置 `at`、静默时间 `now_ms - moved_ms`、`Health` 判定、`why`）与 `Stats`（`queues` 表的容量/当前深度/最深/满次数/丢弃数、`spreads` 的 p50/p99、`counts`）；支持一键关闭与触发 `diagnose()`。
- `Shell.tsx`：组合 `Sidebar`、`TopBar`、主内容区（P3-02 对尚未实现的页面渲染清晰路由占位，P3-03 ～ P3-05 依次替换为真实页面）、`NoticeToast`、`CommandPalette`、`HealthOverlay`。确保组件内**不直接调用** `useSyncExternalStore`，一律走 `ui/src/bus/` 的 `useSource` / `useQuery`。

### 3. 8 语字典补齐

在 `ui/lang/{en,zh-Hans,th,ja,ko,fr,de,es}.json` 同步补齐壳、分组、⌘K、健康态、外观与运行状况浮层键（见 `P3.md`）。

## 显式与可观测

- **状态机**：`InlineConfirm`（`idle` / `armed`）与 `confirmStep` 纯转换表；`stateView` 四组穷举映射函数。
- **具名常量**：`CONFIRM_ARM_TIMEOUT_MS = 4_000`、`LIBRARY_SIDEBAR_RECENT_ROWS = 12`、`SEARCH_DEBOUNCE_MS = 120`、`SEARCH_PALETTE_MAX_HITS = 50`。
- **可观测**：`HealthOverlay` 将核心 `Water` 与 `Stats` 直接可视化到界面（`PLAN.md` §4.5.2），`NoticeToast` 保证每条核心 `Notice` 与撤销命令在界面可见。
- **故障可见性测试**：验证 `Health::Stalled` / `Health::Dead`、`RefineStatus::Failed('keys.async_refused')`、`Status::AudioOnly` 经 `stateView` 映射到清晰错误词与恢复标记，且 `HealthOverlay` 正确渲染停滞阶段与非零队列满计数。

## 先写的测试

新建 `ui/tests/dom.ts` 与 `ui/tests/shell.test.tsx`（顶部声明 `// @vitest-environment jsdom`）：

1. `stateView` 穷举测试：覆盖全部 `Status`（5 种）、`Health`（4 种）、`RefineStatus`（4 种含 `keys.async_refused` 与无音频）、`From`（3 种）变体。
2. `confirmStep` 转换表测试：`idle` → `armed` → 确认复原、`armed` 超时复原、`armed` 取消复原。
3. `Sidebar` DOM 测试：验证按今天/昨天/本周分组、按月折叠来自 `LibrarySummary`、进行中录音带橙色 `.dot.live` 置顶行、点击月份触发跳转。
4. `CommandPalette` DOM 测试：按 `⌘K` / `Ctrl+K` 打开，输入关键词经防抖触发 `Query::Search`，渲染命中列表，键盘 `ArrowDown` + `Enter` 选中跳转，`Escape` 关闭。
5. `NoticeToast` DOM 测试：普通通知 5 秒消隐；带 `undo` 通知点击撤销按钮派发 `Command::Restore` 并立即关闭。
6. `HealthOverlay` DOM 测试：传入含 `Stalled` 水位与队列满数据的 `Water` + `Stats`，断言页面可见停滞原因、队列深度与满次数。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `words.test.ts` 8 语一致校验与 `ui.sh` 架构校验）。

## 不做

- 不在本任务实现具体业务页（`HomePage`、`LivePage`、`ResultPage`、`ReaderPage`、`LibraryPage`、`SettingsPage` 留给 P3-03 ～ P3-05）。
- 不做 P5 完整对外色带（主持人数/结束对外）。
