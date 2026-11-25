# P3-01 迁移到 React 19 + dsh 总线、rAF 信号合并、有界视图缓存与全功能 Mock

## 目标

做完以后：

1. **框架彻底切换**：`ui/` 从 Svelte 5 切换为 **React 19 + TypeScript + `virtua`**；删掉全部 `*.svelte` / `*.svelte.ts` 文件，从 `ui/package.json` 移除 `svelte`、`svelte-check`、`@sveltejs/vite-plugin-svelte`，`ui/vite.config.ts` 改用 `@vitejs/plugin-react`，`ui/tsconfig.json` 开启 `"jsx": "react-jsx"`，`npm run check` 执行 `tsc -p tsconfig.json && vite build --emptyOutDir --logLevel warn`。
2. **dsh 式总线层（`ui/src/bus/`）**：
   - `source.ts`：定义 `Source<T>`（`getSnapshot` + `subscribe`）、`createCell<T>` 与 `useSource<T>`（**全仓唯一**调用 `useSyncExternalStore` 的地方）；
   - `channel.ts`：订阅 Tauri `Channel<Event>`（或 dev mock）；对 `Event::Signal` 按信号类别做单调序去重，并缓冲到每 `requestAnimationFrame` 仅提交一次最新值（真正的 P3 信号合并）；保留 `vitals.ts` 1 Hz `drawn` 上报与 `visibilitychange` 归零不误报 `ui.frame.gap`；管理有上限的 `notices` 队列、本地外观主题源 `themeSource` 与运行状况浮层开关源 `overlaySource`；
   - `query.ts`：版本化有界视图缓存 `QueryCache`（上限 `QUERY_CACHE_MAX_ENTRIES = 128`），收到 `Event::Invalidate` 时**只重新拉取有活跃订阅者的匹配视图**（无订阅者仅标脏，不刷屏），拒绝过期乱序回执，静默忽略 `query.cancelled` / `starting`；
   - `act.ts`：命令发送（`send`）、文件对话框（`pick`）、软删撤销辅助（`removeWithUndo`）、导出诊断（`diagnose`）；
   - `route.ts`：显式导航状态机 `Route` 与纯转换函数 `routeStep`；
   - `words.ts`、`vitals.ts`、`reveal.ts`：由原 `ui/src/lib/` 迁入并去除所有 Svelte `$state` 依赖（`words.ts` 提供 `langSource` 与兼容 `ui.lang` 读写器）。
3. **观众房间源迁移（`ui/src/viewer/room.ts`）**：去除 `$state`，保留 `roomKey`、`openFrame` 与 `read`（以 `Source` 暴露 `roomOpenSource` 与 `roomLiveSource`），让 `ui/tests/room.test.ts` 全绿。
4. **全功能开发替身（`ui/dev/mock.ts`）**：完整支持全部 10 种 `Query`（`LibrarySummary` 按月统计、`LibraryMonth` / `LibraryRecent` 筛选与分页、`Search` 覆盖标题/正文/译文/笔记、`Recording` 含多版本 `takes` + `notes` + `refine`、`Transcript` 64 行分块、`Compare` 逐行 `DiffOp`、`Glossary`、`Calendar`、`Outbound`）和全部 `Command`（含 `Note` 增改删、`Refine` 模拟进度与失败态、`Diagnose`），并在模拟录音中同时发送 `Frame`、`Session`、`Level`、`Progress`、`Water`、`Stats` 信号。
5. **三入口 React 最小可编译壳**：`index.html`、`captions.html`、`viewer.html` 与 `ui/src/main.tsx` 挂载极简 React 根壳（保证 `vite build` 打包三个入口无误，P3-02 起替换为正式界面）。
6. **dsh 架构规则脚本**：新增 `scripts/build/rules/ui.sh` 并纳入 `check.sh` 自动执行。

## 背景

- 设计：`docs/build/phases/P3.md` §3.1–§3.4；`docs/PLAN.md` §5.2、§6.1–§6.2；`docs/build/DECISIONS.md` 第 1 条。
- 涉及文件：
  - `ui/package.json`、`ui/tsconfig.json`、`ui/vite.config.ts`
  - `ui/index.html`、`ui/captions.html`、`ui/viewer.html`
  - `ui/src/main.tsx`（取代 `ui/src/main.ts`）
  - 新增 `ui/src/bus/{source,channel,query,act,words,vitals,reveal,route,index}.ts`
  - 新增 `ui/src/viewer/room.ts`、`ui/src/viewer/ViewerPage.tsx`（最小壳）
  - 新增 `ui/src/shell/Shell.tsx`（最小壳）、`ui/src/captions/CaptionsWindow.tsx`（最小壳）
  - 改造 `ui/dev/mock.ts`
  - 删除 `ui/src/lib/`、`ui/src/ui/`、`ui/src/app.css`、`ui/src/main.ts`
  - 迁移并扩展 `ui/tests/{words,reveal,vitals,room,text}.test.ts` 与 `ui/tests/bus.test.ts`（取代 `store.test.ts`）
  - 新增 `scripts/build/rules/ui.sh`

## 做法

### 1. 构建配置与依赖清理

- 前提：协调者已在沙箱外预装 `react`、`react-dom`、`virtua`、`@vitejs/plugin-react`、`@types/react`、`@types/react-dom`、`@testing-library/react`、`jsdom`。
- 更新 `ui/package.json`：删除 `devDependencies` 中的 `svelte`、`svelte-check`、`@sveltejs/vite-plugin-svelte`；把 `scripts.check` 设为 `"tsc -p tsconfig.json && vite build --emptyOutDir --logLevel warn"`，`scripts.build` 设为 `"npm run check"`。
- 更新 `ui/tsconfig.json`：添加 `"jsx": "react-jsx"`，`"types": ["vite/client", "node"]`。
- 更新 `ui/vite.config.ts`：用 `@vitejs/plugin-react` 取代 Svelte 插件；`test: { include: ['tests/**/*.test.{ts,tsx}'] }`（默认 `node` 环境）。
- 更新 `index.html`、`captions.html`、`viewer.html`：在 `<body>` 内放 `<div id="root"></div>` 并指向 `<script type="module" src="/src/main.tsx"></script>`（保留 `data-window="captions"` 和 `data-window="viewer"`）。

### 2. dsh 式 `ui/src/bus/`

- **`source.ts`**：
  ```ts
  export type Listener = () => void;
  export type Source<T> = {
    getSnapshot(): T;
    subscribe(listener: Listener): () => void;
  };
  export type Cell<T> = Source<T> & { set(next: T): void; update(fn: (prev: T) => T): void };
  ```
  - `createCell<T>(initial: T): Cell<T>`：使用 `Object.is` 去重，变化时通知订阅者。
  - `useSource<T>(source: Source<T>): T`：封装 `useSyncExternalStore(source.subscribe, source.getSnapshot, source.getSnapshot)`。
- **`channel.ts`**：
  - 命名常量：
    - `NOTICE_AUTO_DISMISS_MS = 5_000`
    - `NOTICE_UNDO_DISMISS_MS = 8_000`
    - `NOTICE_MAX_ITEMS = 8`
    - `THEME_STORAGE_KEY = 'zutalk.theme'`
  - 导出源：`stateSource`、`frameSource`（完整 `Frame | null`，保留 `t_audio` 与 `columns`）、`sessionSource`、`levelSource`、`progressSource`、`waterSource`、`statsSource`、`noticesSource`、`themeSource`（`'system' | 'light' | 'dark'`）、`resolvedThemeSource`（`'light' | 'dark'`）、`overlaySource`（`boolean`）。
  - `Bus` 类型：
    ```ts
    export type Bus = {
      send(command: Command): Promise<void>;
      query?(query: Query): Promise<QueryReply>;
      listen(on: (event: Event) => void): () => void;
      drawn?: (seq: number, gap_ms: number, gaps: number) => void;
    };
    ```
  - **rAF 信号合并**：
    - `ChannelController` 内部维护 `seenSeq: Partial<Record<Signal['of'], number>>` 与 `pending: Partial<Record<Signal['of'], Signal>>`。
    - 收到 `Event::Signal({ seq, signal })`：`if (seq <= (seenSeq[signal.of] ?? 0)) return; seenSeq[signal.of] = seq;`。若环境存在 `requestAnimationFrame`（或测试注入的 `scheduleRaf`），存入 `pending[signal.of] = signal` 并只挂起一次待冲刷回调；在帧回调里把 `pending` 全部刷入对应 cell 再清空。若无 `requestAnimationFrame`（纯 Node 同步测试），立即提交。
    - 收到 `Event::State`：立即更新 `stateSource` 并调用 `setLang(e.is.setup.language ?? system)`。
    - 收到 `Event::Invalidate`：立即转交 `queryCache.invalidate(e.is)`。
    - 收到 `Event::Notice`：立即追加到 `noticesSource`（超出 `NOTICE_MAX_ITEMS` 时丢弃最旧一条）。
    - 收到 `Session` 信号切换时驱动 `routeSource`（通过 `routeStep`：空→有切到 `live`，有→空切到 `result(prevId)`）。
  - **Vitals 与 `visibilitychange` 归零**：严格保持原 `P1-R1-01` 行为——注册 `visibilitychange` 监听，当 `document.hidden` 变 `true` 时立刻 `vitals = frame(vitals, 0, true)`；每 rAF 推进 `frame(vitals, now, hidden)` 并经 `report(vitals, seenSeq.Frame ?? 0, now)` 至多每秒报一次 `drawn`；`stop()` 时注销监听与取消 rAF。
- **`query.ts`**：
  - 命名常量：`QUERY_CACHE_MAX_ENTRIES = 128`。
  - 提供 `queryKey(q: Query): string`（确定性稳定键）与 `queryViewKind(q: Query): ViewKind`（`LibrarySummary`/`LibraryMonth`/`LibraryRecent` → `'Library'`；`Compare` → `'Transcript'`；其余同名）与 `queryRecordingId(q: Query): string | null`。
  - `QueryState<T> = { status: 'idle' | 'loading' | 'ready' | 'error'; data: T | null; version: number; error: string | null }`。
  - `QueryCache`：
    - `sourceFor<V extends QueryValue['view']>(query: Query | null): Source<QueryState<Extract<QueryValue, { view: V }>['data']>>`：首次被订阅时自动拉取（若无数据或已标脏）；退订时计数归零；
    - `invalidate(inv: Invalidate)`：只对 `subscribers > 0` 且匹配 `(view, key)` 且 `entry.version < inv.version` 的项触发重拉；其余匹配项仅标 `stale = true`；
    - `evictIfNeeded()`：条目超过 `QUERY_CACHE_MAX_ENTRIES` 时按 `lastUsed` 淘汰最旧的 `subscribers === 0` 条目；
    - `fetchOne`：带 `reqId` 与 `reply.version >= entry.version` 守卫，拒收乱序旧回执；`query.cancelled` 与 `starting` 不覆盖为 error 也不弹 notice；
    - `useQuery<V>(query: Query | null)`：仅在 `ui/src/bus/query.ts` 内基于 `useSource` 暴露给上层容器。
- **`route.ts`**：
  - 定义 `Route` 与 `RouteEvent` 枚举联合类型及纯函数 `routeStep(current, event): Route`；导出 `routeSource` 与 `navigate(event)`。
- **`act.ts`**：
  - 导出 `send(command)`（拒绝时 `say(code)`）、`pick(name?)`（仅在 `ui/src/bus/` 引用 `@tauri-apps/plugin-dialog`）、`removeWithUndo(id)`（发 `Remove` 并弹出带 `{ do: 'Restore', id }` 的撤销通知）、`diagnose()`（发 `Diagnose`）。
- **`words.ts`、`vitals.ts`、`reveal.ts`**：
  - 原 `lib/vitals.ts`、`lib/reveal.ts` 原样迁入 `bus/`；
  - `words.ts` 移除 `$state`，使用 `createCell(system)` 作为 `langSource`，保留 `LANGS`、`pick`、`system`、`ui`（getter/setter 包装 `langSource`）、`t`、`word`、`langName`、`clock`、`when`、`text`，并新增纯函数 `formatMonth(ym: string, lang?: string)`。

### 3. 全功能 `ui/dev/mock.ts`

- 构造跨 3 个月的丰富示例录音（含精修稿、现场稿、精修失败 `keys.async_refused`、有/无音频、已交城、多条带时间与锚点的笔记、2 个版本 `take 0` 现场稿与 `take 1` 精修稿）。
- 完整实现全部 10 种 `Query`：
  - `LibrarySummary`：真实按 `tz_offset_min` 汇总各 `YYYY-MM` 计数；
  - `LibraryMonth` & `LibraryRecent`：真实应用 `LibraryFilter`（`draft_only`/`audio_only`/`filed_only`/`language`）与 `limit`/`offset`/`has_more`；
  - `Search`：真实搜索标题、正文、译文、笔记，返回对应 `SearchHitKind`；
  - `Recording`：返回完整 `takes`、`notes`、`refine` 状态；
  - `Transcript`：按 64 行分块切片并返回指定 `take`；
  - `Compare`：对 `take 0` 与 `take 1` 生成真实带 `Equal`/`Delete`/`Insert` 的 `CompareRow[]`；
  - `Glossary`、`Calendar`、`Outbound` 均返回真实结构。
- 完整实现全部 `Command`：含 `Note`（`Add`/`Edit`/`Remove` 触发 `Recording` 与 `Search` 失效）、`Refine`（推进 `Progress.refine` 并写出新 `take`）、`Diagnose`（回 `Notice('diagnose.done')`）。
- 模拟录音中同时以合理频率发出 `Frame`、`Level`（正弦/脉冲波形）、`Water`（各阶段健康水位）、`Stats`（示例计数与队列深度）。

### 4. 规则脚本 `scripts/build/rules/ui.sh`

新增可执行脚本 `scripts/build/rules/ui.sh`（被 `scripts/build/check.sh` 自动发现执行），检查：
1. `ui/src` 下除 `ui/src/bus/` 外，禁止出现 `useSyncExternalStore`、`@tauri-apps/api`、`@tauri-apps/plugin-dialog`、`invoke(`、`new Channel`；
2. `ui/src`、`ui/tests`、`ui/dev` 不存在任何 `*.svelte` 或 `*.svelte.ts` 文件；
3. `ui/package.json` 不含 `"svelte"`、`"svelte-check"`、`"@sveltejs/vite-plugin-svelte"`；
4. `ui/src` 不含 `console.log(`。

## 显式与可观测

- **状态机**：`Route` + `routeStep`（配转换表测试）；`QueryStatus`（`idle`/`loading`/`ready`/`error`）；`ChannelController` 显式缓冲与冲刷周期。
- **具名常量**：`NOTICE_AUTO_DISMISS_MS`、`NOTICE_UNDO_DISMISS_MS`、`NOTICE_MAX_ITEMS`、`QUERY_CACHE_MAX_ENTRIES`、`FRAME_GAP_MS`、`REPORT_EVERY_MS`、`THEME_STORAGE_KEY`，全带单位或原因注释。
- **有界缓存与队列**：`QueryCache` 封顶 `128` 项（驱逐最旧非活跃项）；`notices` 封顶 `8` 条（丢弃最旧）；信号缓冲每类仅 1 槽（最新覆盖）。
- **可观测**：保留 `vitals` 1 Hz `drawn` 上报与 `visibilitychange` 归零，避免后台误报 `ui.frame.gap`；命令拒绝与视图错误显式进入 `noticesSource` / `QueryState.error`，绝不静默吞掉。
- **故障可见性测试**：总线拒绝命令出 `Notice`；查询失败（非取消）进入 `QueryState.error`；乱序回执不污染视图。

## 先写的测试

1. 迁移并保持原有 5 份单测全绿（更新导入路径到 `../src/bus/*` 与 `../src/viewer/room`）：
   - `ui/tests/words.test.ts`
   - `ui/tests/reveal.test.ts`
   - `ui/tests/vitals.test.ts`
   - `ui/tests/room.test.ts`
   - `ui/tests/text.test.ts`
2. 新建 `ui/tests/bus.test.ts`（取代 `store.test.ts`）：
   - 保留原 `store.test.ts` 全部核心语义断言（整份替换 `State`、独立信号单调 `seq` 守卫、`State` 不触碰 `Session`/`Progress`/`Stats`/`Notices`、语言跟随 `setup.language`、命令拒绝进 `notices`、`visibilitychange` 归零不误报 `ui.frame.gap`）；
   - **rAF 信号合并测试**：在同一帧内连推 15 次 `Frame` 与 15 次 `Level`，断言帧前不刷、帧回调仅通知订阅者 1 次且拿到最后 1 个 `seq`；
   - **活跃订阅失效测试**：订阅 `LibraryRecent` 而未订阅 `Glossary`，分别推 `Invalidate('Library')` 与 `Invalidate('Glossary')`，断言只重拉 `LibraryRecent` 不拉 `Glossary`；后发先至乱序回执被丢弃；
   - **缓存上限淘汰测试**：写入 140 个独立键（其中 2 个处于订阅态），断言缓存数 ≤ `QUERY_CACHE_MAX_ENTRIES = 128` 且 2 个活跃订阅项绝不被淘汰；
   - **`routeStep` 转换表测试**：覆盖 `home` → `live` → `result` → `reader` → `library` → `settings` 及删除当前录音回到 `home`；
   - **`mockBus` 契约测试**：验证全 10 种 `Query` 与 `Note` / `Refine` / `Remove` / `Restore` 在 `mockBus` 上跑通。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含新规则 `scripts/build/rules/ui.sh`、全部 707 Rust 测试、全部 UI 测试、`tsc` 与三入口 `vite build`）。

## 不做

- 不在本任务里写完整页面 UI 与设计系统样式（`Shell`/`CaptionsWindow`/`ViewerPage` 只留最小可编译挂载壳，由 P3-02 ～ P3-06 实现）。
- 不修改任何 Rust 代码或 `core/bindings/*.ts`。
