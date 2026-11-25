状态：完成

## 改动与文件

完成从 Svelte 5 到 React 19 + TypeScript + `virtua` 的框架迁移、dsh 式总线层、全功能开发替身与静态架构守卫规则：

1. **构建配置与三入口切换**：
   - `ui/package.json`：移除 `svelte`、`svelte-check`、`@sveltejs/vite-plugin-svelte`，将 `check` 设为 `tsc -p tsconfig.json && vite build --emptyOutDir --logLevel warn`，`build` 指向 `npm run check`。
   - `ui/tsconfig.json`：启用 `"jsx": "react-jsx"`，更新 `types` 为 `["vite/client", "node"]`。
   - `ui/vite.config.ts`：切到 `@vitejs/plugin-react`，保留 `rooms()` 与三入口（`index.html`、`captions.html`、`viewer.html`），`test.include` 扩展为 `tests/**/*.test.{ts,tsx}` 且默认维持 `node` 环境。
   - `ui/index.html`、`ui/captions.html`、`ui/viewer.html`、`ui/src/main.tsx`：三入口均以 `<div id="root"></div>` 挂载 React 19 根壳；删除 `ui/src/lib/`、`ui/src/ui/`、`ui/src/app.css`、`ui/src/main.ts`、`ui/tests/store.test.ts`。
   - `ui/src/shell/Shell.tsx`、`ui/src/captions/CaptionsWindow.tsx`、`ui/src/viewer/ViewerPage.tsx`：提供最小可编译 React 挂载壳（由后续任务替换）。
2. **dsh 式总线层（`ui/src/bus/`）**：
   - `ui/src/bus/source.ts`：定义 `Source<T>`、`Cell<T>`、`createCell<T>` 与全仓唯一调用 `useSyncExternalStore` 的 `useSource<T>`。
   - `ui/src/bus/channel.ts`：实现 `startBus`、`take`、按信号类别单调序去重与 `requestAnimationFrame` 每帧合并（`flushPendingSignals`），保留 `vitals` 1 Hz `drawn` 上报与 `visibilitychange` 归零，管理上限 `NOTICE_MAX_ITEMS = 8` 的通知队列、`themeSource`/`resolvedThemeSource` 与 `overlaySource`。
   - `ui/src/bus/query.ts`：实现版本化有界 `QueryCache`（上限 `QUERY_CACHE_MAX_ENTRIES = 128`），按活跃订阅者决定 `Event::Invalidate` 重拉，拒绝过期/乱序回执，静默忽略 `query.cancelled` 与 `starting`，驱逐最旧非活跃项且绝不淘汰当前订阅项。
   - `ui/src/bus/route.ts`：实现显式导航状态机 `Route`、`RouteEvent` 与纯转换函数 `routeStep`。
   - `ui/src/bus/act.ts`、`ui/src/bus/index.ts`：集中封装 `tauri` IPC（`Channel`/`invoke`）、文件对话框 `pick`、`send`、`removeWithUndo`、`diagnose`。
   - `ui/src/bus/words.ts`、`ui/src/bus/vitals.ts`、`ui/src/bus/reveal.ts`：去除 `$state` 迁入总线层，新增 `formatMonth`。
3. **观众房间源与全功能 Mock**：
   - `ui/src/viewer/room.ts`：去除 `$state`，保留 `roomKey`、`openFrame` 与 `read`，改以 `roomOpenSource`/`roomLiveSource` 暴露。
   - `ui/dev/mock.ts`：构造跨 3 个月 5 条样本录音（精修双版本、现场稿、已交城、`keys.async_refused` 失败态、已销毁音频），完整实现全部 10 种 `Query` 与全部 `Command`（含 `Note` 增改删、`Refine` 进度与双版本写入、`Diagnose`）及录音中 `Frame`/`Level`/`Water`/`Stats` 信号流。
4. **架构守卫脚本**：
   - `scripts/build/rules/ui.sh`（可执行）：静态检查 `ui/src/bus/` 外禁止 `useSyncExternalStore` 与 Tauri IPC、禁止 Svelte 文件与依赖残留、禁止 `console.log(`。

## 验证证据

- 全部 10 个规则脚本通过（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）。
- `cd ui && npm run check && npm test && npm run build` 全部通过（`tsc` 零报错，三入口 `vite build` 成功）。
- UI 测试 6 个文件 40 条全部通过：
  - `ui/tests/bus.test.ts`（13 条）：整份替换 `State`、独立信号单调序守卫、`State` 不触碰 `Session`/`Progress`/`Stats`、通知上限 `NOTICE_MAX_ITEMS`、语言跟随、命令拒绝出 `Notice`、15 帧 `Frame`+`Level` rAF 单帧合并、`visibilitychange` 归零不误报 `ui.frame.gap`、`QueryCache` 仅重拉活跃订阅并丢弃乱序回执、`query.cancelled` 静默与真错可见、140 项写入上限 `128` 淘汰且保留活跃订阅、`routeStep` 全转换表、`mockBus` 全 10 种 `Query` 与 `Note`/`Remove`/`Restore`/`Diagnose` 契约。
  - `ui/tests/words.test.ts`（12 条）、`ui/tests/reveal.test.ts`（6 条）、`ui/tests/vitals.test.ts`（4 条）、`ui/tests/room.test.ts`（3 条）、`ui/tests/text.test.ts`（2 条）。
- 本任务零 Rust 改动。

## 遗留与需负责人确认

- 无阻塞；正式设计系统、两栏外壳与各业务页按计划在 P3-02 ～ P3-06 依次补齐。
