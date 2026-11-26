# P3-06 独立字幕窗、React 观众页与 8 语字典核齐

## 目标

做完以后：

1. **独立字幕窗（`ui/src/captions/CaptionsWindow.tsx`，对照 `CaptionsLatest.dc.html`）**：
   - 挂载在 `captions.html`（`data-window="captions"`），**只订阅** `frameSource`、`stateSource`（取 `setup.mode` 与 `rooms`）与 `resolvedThemeSource`，绝不触发任何 `Library` / `Transcript` / `Search` 查询（保持字幕窗轻量、低延迟、`PLAN.md` §6.2）；
   - 整窗支持 `data-tauri-drag-region` 拖拽（交互按钮区域不拦点击）；
   - 顶部工具条：若有活动对外房间则显示薄荷/森林色带小标「对外」；右侧提供版面切换按钮（`Audience` 最新一句 ↔ `Conversation` 面对面，首栏 180° 翻转，派发 `Command::Setup`）与关闭按钮（派发 `Command::Captions { show: false }`）；
   - 复用 `LiveCaptions`（`P3-03`）展示多栏逐字显现字幕与每栏健康态（`Slow`/`Stalled`/`Dead` 时窗内可见，不假装正常）。
2. **React 观众页（`ui/src/viewer/ViewerPage.tsx`，对接 `ui/src/viewer/room.ts`）**：
   - 挂载在 `viewer.html`（`data-window="viewer"`），完整保留既有 `Viewer.svelte` 的能力：只读消费 `room.ts` 的 SSE + AES-256-GCM 解密结果、单语过滤按钮、复用 `LiveCaptions` 渲染实时字幕、展示共享录音正文以及错误/结束状态码（`room.bad`、`room.gone`、`room.viewers`、`offline`）；
   - 保持 `ui/tests/room.test.ts` 100% 通过，`ui/dev/rooms.ts` 开发中间件正常工作（完整 P5 手机端观众页视觉重构留 P5）。
3. **8 语字典全面核对与润色**：
   - 核对 `ui/lang/{en,zh-Hans,th,ja,ko,fr,de,es}.json` 中 P3 新增的全部文案键，确保无遗漏、无生硬英文占位、占位符 `{0}` 完全一致，且零未使用孤立键（或明确保留的既有键）。

## 背景

- 设计：`docs/build/phases/P3.md` §3.2、§3.6；`docs/ui-2026-11-21/CaptionsLatest.dc.html`；`docs/PLAN.md` §3、§6.2。
- 涉及文件：
  - 完善 `ui/src/captions/CaptionsWindow.tsx`
  - 完善 `ui/src/viewer/ViewerPage.tsx`（使用 `ui/src/viewer/room.ts`）
  - 核查 `ui/src/main.tsx`（确认 `captions` 与 `viewer` 两个窗口不发多余初始化查询）
  - 新增 `ui/tests/captions_viewer.test.tsx`
  - 复核 `ui/lang/*.json`（8 份文件）

## 做法

### 1. `CaptionsWindow.tsx`

- 拆出纯 props 展示组件 `CaptionsWindowView` 与连 `bus` 容器 `CaptionsWindow`。
- 样式对齐 `CaptionsLatest.dc.html`：深森林绿背景（`#012A1F`）配白字主尾行、柔和次色上一行与橙色呼吸光标；当 `mode === 'Conversation'` 时启用面对面上下排布并旋转首栏 180°（复用 `controls.css` 的 `.sc-facing` / `.conversation` 规则）。
- 仅监听 `frameSource`、`stateSource`、`resolvedThemeSource`；`main.tsx` 启动 `captions` 窗口时只调 `startBus(bus)`，因为 `QueryCache` 是懒订阅，字幕窗不调用 `useQuery`，天然零多余查询！

### 2. `ViewerPage.tsx`

- 拆出纯 props 展示组件 `ViewerView` 与连 `room.ts` 源的容器 `ViewerPage`。
- 当存在 `endCode`（`room.gone` / `room.bad` / `room.viewers` / `offline`）时在顶栏显示明确状态标志（`role="status"`）；当有多栏实时字幕时提供按语言单栏筛选胶囊；复用 `LiveCaptions`（`layout="Audience"`）渲染平滑字幕；若有旧式 `room.open` 日志也照常渲染（兼容 P5 删单场分享前的开发中间件 `/r/rec`）。

### 3. 8 语字典核齐

- 逐个检查 `ui/lang/*.json` 8 份文件，确保每一条 P3 新键在中文、英文、泰文、日文、韩文、法文、德文、西班牙文均自然准确。

## 显式与可观测

- **单一实现**：`LivePage`、`CaptionsWindow`、`ViewerPage` 三处全部共用同一套 `LiveCaptions` + `reveal.ts`，不写第二份逐字显现逻辑（原则 3）。
- **低开销隔离**：字幕窗零 `Query` 订阅，仅接收每帧合并后的最新 `Frame`。
- **故障可见性**：字幕窗内若某栏 `Stalled` 或 `Dead`，直接呈现健康徽章与原因码；观众页断连或密钥错误直接呈现 `room.gone` / `room.bad` / `offline`。

## 先写的测试

新建 `ui/tests/captions_viewer.test.tsx`（顶部声明 `// @vitest-environment jsdom`，引入 `./dom`）：

1. **`CaptionsWindowView` 测试**：
   - 渲染 3 栏 `Frame` 字幕，确认容器带 `data-tauri-drag-region`；
   - 点击版面切换按钮把 `Audience` 切换为 `Conversation`（派发 `Command::Setup`），并验证 `Conversation` 类应用于面对面布局；
   - 点击关闭按钮派发 `Command::Captions { show: false }`；
   - 注入 `Health::Dead { why: 'offline' }`，断言字幕窗内可见断开原因。
2. **字幕窗零查询隔离测试**：
   - 挂载 `CaptionsWindow` 容器连测试 `Bus`，推入 `State`、`Frame` 与 `Invalidate('Library')`，断言 `bus.query` 调用次数严格为 `0`。
3. **`ViewerView` 测试**：
   - 渲染多语 `Column[]`，点击语言胶囊筛选为单栏；
   - 传入 `endCode = 'room.gone'` 断言状态标志可见；
   - 确认既有 `ui/tests/room.test.ts` 原样通过。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `room.test.ts`、`words.test.ts`、`ui.sh` 与三入口 `vite build`）。

## 不做

- 不在本任务实现字幕窗的场合预设、窗口大小/位置记忆、穿透点击与舞台全屏（留 P5，涉及 `app/src/windows.rs`）。
- 不在本任务删除 `rooms.ts` 的 `rec` 演示分支或修改服务器 `server/src/room.rs`（留 P5）。
