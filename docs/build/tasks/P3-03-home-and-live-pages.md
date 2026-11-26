# P3-03 首页与录音中（多栏逐字显现、健康度重连、边录边记与标记 M）

## 目标

做完以后：

1. **首页（`ui/src/pages/HomePage.tsx`，对照 `Main.dc.html`）**：
   - 遵循「一屏一个主按钮」规矩：中央大圆形「开始录音」主按钮（带呼吸涟漪动效，点击派发 `Command::Start`；已有录音进行中时禁用或提示回到录音）；
   - 下一场录音配置网格：说的语言（添加/移除胶囊）、字幕语言（添加/移除胶囊，受核心 4 栏上限提示）、声音来源（麦克风 / 电脑里的声音 / 麦克风和电脑声音）与麦克风设备选择器、当前音频留存摘要（可点去设置修改）；
   - 底部次级动作：「导入」音频文件（经 `bus/act.ts` 的 `pick()` 选文件并派发 `Command::Import`）；
   - 若正在搬运 0.8 资料库（`Progress.move_old`）或存在未搬条目（`State.unmoved`），在下方以克制卡片展示进度条与具体失败原因码（`States.dc.html`）。
2. **可复用多栏逐字显现字幕组件（`ui/src/pages/LiveCaptions.tsx`）**：
   - 使用 `ui/src/bus/reveal.ts` 的 `reveal` / `upTo` / `carry` 做按语速平滑逐字显现，尊重 `prefers-reduced-motion: reduce`；
   - 支持三种版面：`Reading`（录音主屏，保留最近 6 行）、`Audience`（字幕窗最新一句，保留 1 行）、`Conversation`（面对面，保留 2 行且首栏 180° 旋转）；
   - 每栏标题旁展示该栏健康与新鲜度状态（`Live` / `Slow` / `Stalled` / `Dead` 及其原因码）；存在 `Dead` 栏时显示「重连」按钮（`Command::Reconnect`），彻底落实 `PLAN.md` §6.3「字幕停了，界面上看得见」。
3. **录音中与边录边记（`ui/src/pages/LivePage.tsx`，对照 `Live.dc.html` + `LiveNotes.dc.html`）**：
   - 顶部操作栏：笔记抽屉开关、独立字幕窗开关（`Command::Captions`）；
   - 底部悬浮控制胶囊：橙色呼吸点（暂停时静止变灰）、实时计时器（`CLOCK_TICK_MS = 250` ms 刷新）、由真实 `Signal::Level`（`rms` / `peak`）驱动的 5 柱电平表（`LEVEL_BARS_COUNT = 5`）、核心状态句（`Recording` / `Paused` / `AudioOnly · 原因` / `Connecting` / `Behind`）、**标记（`M`）**按钮、暂停/继续按钮（`Pause` / `Resume`）、单一主按钮「停止」（`Stop`，停后跳往 `result(session.id)`）；
   - **边录边记右侧栏（`LiveNotes`）**：
     - 订阅 `Query::Recording { id: session.id }` 读取当前场已存的 `notes`（经 `Invalidate('Recording')` 自动刷新）；
     - 忠实实现 `LiveNotes.dc.html` 首字锁锚点体验：在输入框键入第一个字时锁定当下 `at_ms = frame?.t_audio ?? elapsedMs` 与当前屏幕字幕引文预览；清空输入释放锚点；按 Enter 提交 `Command::Note { id: session.id, op: { op: 'Add', at_ms, text, anchor: null } }`；
     - 按键盘 `M`（当焦点不在输入框/文本域时）或点击「标记」按钮，立即以 `text: ""` 写入一条锚定此刻 `at_ms` 的空笔记（`DECISIONS.md` 第 2 条）；
     - 顶部标明「不对外」私密提示；已记笔记支持行内删除（`NoteOp::Remove`）。
4. **接入 `Shell.tsx`** 并补齐 8 语字典新增键。

## 背景

- 设计：`docs/build/phases/P3.md` §3.2、§3.4；`docs/ui-2026-11-21/{Main.dc.html,Live.dc.html,LiveNotes.dc.html,States.dc.html}`；`docs/build/DECISIONS.md` 第 2 条。
- 涉及文件：
  - 新增 `ui/src/pages/HomePage.tsx`、`ui/src/pages/LiveCaptions.tsx`、`ui/src/pages/LivePage.tsx`
  - 更新 `ui/src/shell/Shell.tsx`（接入 `HomePage` 与 `LivePage`）
  - 新增 `ui/tests/home_live.test.tsx`
  - 更新 `ui/lang/*.json`（8 份文件）

## 做法

### 1. `HomePage.tsx`

- 拆出纯 props 展示组件 `HomeView` 与连 `bus` 的容器 `HomePage`。
- 语言添加/移除：从 `state.languages` 中排除已选语言渲染添加弹层/下拉，每次增删直接发 `Command::Setup { setup: { ...state.setup, spoken / shown } }`；遵循「当前的值写在控件上」。
- 声音来源：`Mic` / `System` / `Both` 切换，非 `System` 时展示 `state.devices` 麦克风列表（含 `device: null` 默认麦克风）。
- 唯一主按钮：只有中央「开始录音」按钮使用主按钮语义，导入与配置均使用次级 `pill` / `quiet` 样式。
- 0.8 迁移提示：`progress.move_old` 存在时显示进度条（`done / of`）与步骤词；`state.unmoved` 非空时列出标题与 `word('code', code)`。

### 2. `LiveCaptions.tsx`

- 封装平滑显现逻辑（纯函数计算 + rAF 推进 `now`，与原 `Captions.svelte` 等价但完全用 React hooks + `reveal.ts` 实现，且不碰 `useSyncExternalStore`）。
- 接收纯 props：`columns: Column[]`、`layout: 'Reading' | 'Audience' | 'Conversation'`、`onReconnect?: () => void`。
- 健康态展示：
  - `Live`：不打扰正文（可选极淡状态点）；
  - `Slow`：栏头显示琥珀/中性「偏慢」徽章；
  - `Stalled`：栏头显示「停滞 · 原因」警告徽章（`role="status"`）；
  - `Dead`：栏头显示「已断开 · 原因」警告徽章，并提供「重连」按钮触发 `onReconnect`。

### 3. `LivePage.tsx`

- 命名常量：
  - `CLOCK_TICK_MS = 250`（计时器刷新间隔，保证秒表跳变不抖）
  - `LEVEL_BARS_COUNT = 5`（胶囊内电平柱数，与 `zt.css` 一致）
- 电平柱高度由纯函数 `levelBarHeights(level: Level, paused: boolean): number[]` 算出（5 根柱按 `rms`/`peak` 加权重映射 `[0.2, 1.0]`，暂停时恒为静息态），彻底取代 CSS 假动画，让电平真实反映麦克风输入！
- 笔记锚点逻辑抽出纯函数 `resolveLiveAnchor(frame: Frame | null, elapsedMs: number): { at_ms: number; quote: string }`，方便单测。
- 键盘 `M` 监听：仅当事件目标不是 `INPUT` / `TEXTAREA` / `contentEditable` 且无修饰键（`!metaKey && !ctrlKey && !altKey`）时触发标记。
- 点击「停止」：先记下 `const id = session.id`，派发 `send({ do: 'Stop' })`，由 `routeStep`（在 `Session` 变空时）或立即切到 `result(id)`，保证停止后用户直达录后页面。

## 显式与可观测

- **纯函数规则**：`levelBarHeights`、`resolveLiveAnchor`、`stateView` 均为无副作用纯函数。
- **具名常量**：`CLOCK_TICK_MS = 250`、`LEVEL_BARS_COUNT = 5`。
- **故障可见性**：
  - `Status::AudioOnly { why }`（如 `captions.no_key` / `offline`）在胶囊 `role="status"` 中直接显示「正在录音 · 只录音 · 原因」；
  - 任意栏 `Health::Stalled` / `Health::Dead` 在该栏顶部显式标出原因并提供「重连」动作。
- **隐私**：绝不向日志或控制台输出任何字幕文字或笔记内容。

## 先写的测试

新建 `ui/tests/home_live.test.tsx`（顶部声明 `// @vitest-environment jsdom`）：

1. **`HomeView` 测试**：
   - 点击主按钮「开始录音」派发 `Command::Start`；
   - 添加/移除语言与切换声源派发正确的 `Command::Setup`；
   - 点击「导入」调用文件选择并派发 `Command::Import`；
   - 存在 `progress.move_old` 与 `state.unmoved` 时可见进度数字与错误码文案。
2. **`LiveCaptions` 与故障可见性测试**：
   - 渲染双语 `Frame` 的历史行与 `tail`；
   - 注入一栏 `Health::Stalled { why: 'silent' }`、另一栏 `Health::Dead { why: 'offline' }`，断言两栏原因均在 DOM 可见，且点击「重连」派发 `Command::Reconnect`。
3. **`LiveView` 状态与胶囊测试**：
   - 分别渲染 `Recording`、`Paused`、`AudioOnly('captions.no_key')`、`Connecting`、`Behind`，断言胶囊状态句与暂停/继续按钮切换正确；
   - `levelBarHeights` 纯函数测试：静音为底值、大信号升高、暂停归静。
4. **边录边记与标记（`M`）测试**：
   - 键入第一个字符时锁定此刻 `frame.t_audio` 为锚点；随后 `frame.t_audio` 前进 10 秒再按 Enter，断言派发的 `Command::Note` 使用**首字时刻**的 `at_ms` 而非提交时刻；
   - 按快捷键 `M`（非输入框焦点）或点「标记」按钮，断言派发 `NoteOp::Add { text: "", at_ms, anchor: null }`；在输入框内输入字母 `m` 不误触发标记；
   - 点击「停止」派发 `Command::Stop`。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 8 语 `words.test.ts` 与 `ui.sh`）。

## 不做

- 不在本任务实现主持开场前 / 对外直播（P5）或首页主题包选择（P6）。
- 不在本任务实现独立字幕窗页面本身（只保留顶栏开关发 `Command::Captions`，窗口页面在 P3-06 实现）。
