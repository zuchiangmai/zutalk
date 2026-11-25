状态：完成

## 改动与文件

修复第 1 轮审查（`docs/build/reviews/P3-r1.md`）**必须改 #3**：修正 Compare 模式真实行数计算（消除幽灵加载行与末尾截断）、补齐搜索 `lineKey` 跳入阅读页的自动滚动与分块加载、改造全部录音页多页分页为 `useQueries` 纯衍生并修复 `initialMonth` 归零重置：

1. **`ui/src/pages/TranscriptList.tsx`**：
   - 新增命名常量 `DEFAULT_VIEWPORT_FALLBACK_PX = 600`（取代未命名的视口回退高度字面量）。
   - 重写 Compare 模式 `totalLines` 计算：在 `chunk0Compare` 返回后以真实对比块数据为准（`total_chunks <= 0` → `0`、`total_chunks === 1` → 精确取 `chunk0Compare.rows.length`、多块时若末块已加载精确取 `(total_chunks - 1) * 64 + lastChunk.rows.length`，末块未加载时将 `expectedCompareLines` 钳制在合法区间 `[(total_chunks - 1) * 64 + 1, total_chunks * 64]`），不再被过期估计值覆盖导致幽灵加载行或截断。
2. **`ui/src/pages/ReaderPage.tsx`**：
   - 导出纯函数 `parseLineIndex(key)`，同时兼容 `k130` 与 `<id>.<take>.<n>` 行键格式（拒绝 `'title'` 与空值）。
   - 初始挂载与 `highlightLineKey` 变化时同步驱动 `scrollToIndex`，并与 `jumpToNote` 共用 `parseLineIndex`，使从 `⌘K` 或全部录音搜索进入长录音后半段行时自动滚动并按需拉取目标分块、高亮目标行。
3. **`ui/src/pages/LibraryPage.tsx`**：
   - 新增命名常量 `LOAD_MORE_THRESHOLD_ROWS = 3`。
   - 弃用手工拼接的 `offset` + `accumulatedRows`，改为维护已展开页数 `pageCount` 并通过 `useQueries` 订阅各页 `LibraryMonth`（依赖 `P3-R1-01` 的 `Library` 槽串行调度），经 `useMemo` 合并去重纯衍生 `rows`，确保多页状态下 `Invalidate('Library')` 实时移除已删录音并刷新改名行。
   - `useEffect([initialMonth])` 改为无条件同步 `selectedMonth` 并重置 `pageCount = 1`，修复从侧栏某月切回「全部录音」（`initialMonth === null`）卡在旧月的问题。
4. **测试与 jsdom 滚动辅助**：
   - `ui/tests/dom.ts`：补齐 jsdom 缺失的 `Element.prototype.scrollTo` / `scrollBy` 替身（同步更新 `scrollTop` 并派发 `scroll` 事件），使 `virtua` `VList.scrollToIndex` 在 jsdom 中真实滚动并挂载目标行。
   - `ui/tests/result_reader.test.tsx`：新增 3 条测试（`parseLineIndex` 纯函数、Compare 精确 1 行合并无幽灵行 + 4 行展开无截断、4 小时录音带 `lineKey="rec-1.1.130"` 自动拉取第 2 块并滚动高亮目标行）。
   - `ui/tests/library_settings.test.tsx`：新增 1 条测试（60 条月度录音加载第 2 页后删第 0 条 + 改第 1 条标题 + 推送 `Invalidate('Library')` 实时同步，再验证 `initialMonth` 从 `'2026-10'` 回到 `null` 自动重置为最新月份 `'2026-11'`）。

## 验证证据

- TDD 实测：修复前 3 条新增回归测试全部复现原缺陷并失败，修复后全部转绿。
- `cd ui && npm run check && npm test && npm run build` 全部通过（`tsc` 零报错，三入口 `vite build` 成功）。
- 全部 10 个规则脚本通过（含 `ui.sh` 与 `pending.sh`）。
- UI 测试 12 个文件 **86 条全部通过**（`result_reader` 12、`library_settings` 6、`bus` 17、`words` 12 等）。
- 本任务及整轮 P3 零 Rust 代码改动。

## 遗留与需负责人确认

- 第 1 轮审查的 3 项「必须改」已全部关闭，交由协调者跑完整沙箱外 `scripts/build/check.sh` 与第 2 轮复审。
