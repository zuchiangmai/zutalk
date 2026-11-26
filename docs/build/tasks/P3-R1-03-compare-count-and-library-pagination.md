# P3-R1-03 修复 Compare 行数计算、搜索 lineKey 跳转定位与资料库多页失效刷新

## 目标

做完以后：

1. **修复 `TranscriptList` Compare 模式行数计算（消除幽灵加载行与末尾截断）**：
   - `ui/src/pages/TranscriptList.tsx` 在 Compare 模式下，一旦 `chunk0Compare` 返回，以真实对比块数据确定 `totalLines`：
     - `chunk0Compare.total_chunks === 0` → `0`；
     - `chunk0Compare.total_chunks === 1` → 精确等于 `chunk0Compare.rows.length`；
     - `chunk0Compare.total_chunks > 1` → 若末块 `total_chunks - 1` 已加载，精确等于 `(total_chunks - 1) * TRANSCRIPT_CHUNK_LINES + lastChunk.rows.length`；若末块尚未加载，按 `(total_chunks - 1) * TRANSCRIPT_CHUNK_LINES + TRANSCRIPT_CHUNK_LINES`（或将 `expectedCompareLines` 钳制在合法区间 `[(total_chunks - 1) * 64 + 1, total_chunks * 64]`）估算，绝不用过期的 `expectedCompareLines` 覆盖已确定的单块/末块真实行数。
   - 同步清理 `TranscriptList.tsx:217` 的未命名回退视口高度，收进命名常量 `DEFAULT_VIEWPORT_FALLBACK_PX = 600`。
2. **补齐从搜索 `lineKey` 跳入阅读页的自动滚动定位（`P3-04` §3.3）**：
   - `ui/src/pages/ReaderPage.tsx` 抽出纯函数 `parseLineIndex(lineKey: string | null | undefined): number | null`，同时服务「笔记跳转（`jumpToNote`）」与「从 `⌘K` / 全部录音搜索携带 `lineKey` 进入阅读页」，使搜索命中后半段行时自动滚动并拉取对应分块、高亮目标行。
3. **修复 `LibraryPage` 多页加载后的 `Invalidate('Library')` 刷新与 `initialMonth` 归零重置**：
   - `ui/src/pages/LibraryPage.tsx` 放弃用本地 `accumulatedRows` 手工拼接分页的做法，改为维护已展开页数 `pageCount`（初始 `1`，点加载更多递增 `1`），通过 `useQueries` 同时订阅 `[0, LIBRARY_PAGE_ROWS, ..., (pageCount - 1) * LIBRARY_PAGE_ROWS]` 各页（配合 `P3-R1-01` 的 `Library` 槽串行化），再经 `useMemo` 合并去重衍生 `rows`：
     - 多页状态下发生删除录音、改标题或精修完成（`Invalidate('Library')`）时，全部活跃页自动从 `QueryCache` 刷新，已删除行立即消失、标题与徽章实时更新，不再残留旧行。
   - 修复 `initialMonth` 同步逻辑：无论 `initialMonth` 变为某个月份还是回到 `null`（例如从侧栏某个月切回「全部录音」），都重置 `selectedMonth` 与 `pageCount = 1`。
   - 顺手把触底预加载阈值 `3` 收进命名常量 `LOAD_MORE_THRESHOLD_ROWS = 3`。

## 背景

- 审查来源：`docs/build/reviews/P3-r1.md` **必须改 #3**。
- 依赖：`P3-R1-01`（`LibraryPage` 使用 `useQueries` 订阅多页 `LibraryMonth` 依赖 `QueryCache` 对 `Library` 槽的串行调度）。
- 涉及文件：
  - `ui/src/pages/TranscriptList.tsx`
  - `ui/src/pages/ReaderPage.tsx`
  - `ui/src/pages/LibraryPage.tsx`
  - `ui/tests/result_reader.test.tsx`
  - `ui/tests/library_settings.test.tsx`

## 做法

1. **`ui/src/pages/TranscriptList.tsx`**：
   - 新增命名常量 `DEFAULT_VIEWPORT_FALLBACK_PX = 600`（`VList` 尚未完成首次测量时的回退视口高度）。
   - 重写 `totalLines` 的 Compare 分支：
     ```ts
     if (compare) {
       if (!chunk0Compare) return 0;
       if (chunk0Compare.total_chunks <= 0) return 0;
       if (chunk0Compare.total_chunks === 1) return chunk0Compare.rows.length;
       const lastIdx = chunk0Compare.total_chunks - 1;
       const lastChunk = compareMap.get(lastIdx);
       if (lastChunk) {
         return lastIdx * TRANSCRIPT_CHUNK_LINES + lastChunk.rows.length;
       }
       const minTotal = lastIdx * TRANSCRIPT_CHUNK_LINES + 1;
       const maxTotal = chunk0Compare.total_chunks * TRANSCRIPT_CHUNK_LINES;
       return expectedCompareLines > 0
         ? Math.min(maxTotal, Math.max(minTotal, expectedCompareLines))
         : maxTotal;
     }
     ```
2. **`ui/src/pages/ReaderPage.tsx`**：
   - 导出纯函数 `parseLineIndex(key: string | null | undefined): number | null`：匹配行键尾段数字（兼容 `k12` 与 `<id>.<take>.<n>`，例如 `/(?:^|\.|k)(\d+)$/`，不匹配 `'title'` 或空串）。
   - 在 `useEffect([row.id, highlightLineKey])` 中同步 `setFocusLineKey(highlightLineKey)` 与 `setScrollToIndex(parseLineIndex(highlightLineKey))`；`jumpToNote` 复用 `parseLineIndex(note.anchor)`。
3. **`ui/src/pages/LibraryPage.tsx`**：
   - 新增命名常量 `LOAD_MORE_THRESHOLD_ROWS = 3`（距列表底部剩余不足 3 行高时自动触发加载下一页）。
   - 用 `pageCount`（`useState(1)`）替代 `offset` + `accumulatedRows`：
     - `pageQueries = useMemo(...)` 生成 `pageCount` 条 `Query::LibraryMonth`（`offset = i * LIBRARY_PAGE_ROWS`）；
     - `pageStates = useQueries(pageQueries)`；
     - `rows = useMemo(...)` 按页序合并 `pageStates` 的 `data.rows`（按 `id` 去重防跨页插入导致重复键）；
     - `hasMore = Boolean(pageStates[pageStates.length - 1]?.data?.has_more)`；
     - `loading = pageStates.length > 0 && pageStates[0].status === 'loading' && rows.length === 0`。
   - `useEffect` 监听 `initialMonth` 变化：直接 `setSelectedMonth(initialMonth); setPageCount(1);`（包括 `initialMonth` 回到 `null`）。

## 显式与可观测

- **纯函数规则**：`parseLineIndex` 独立可测；`LibraryPage` 列表行完全由 `QueryCache` 响应纯衍生，彻底消灭双份状态（原则 3「同类只有一个实现」）。
- **具名常量**：新增 `DEFAULT_VIEWPORT_FALLBACK_PX = 600`、`LOAD_MORE_THRESHOLD_ROWS = 3`。
- **有界成本**：`LibraryPage` 仅订阅用户已主动展开的页，不预拉未展开页；`ReaderPage` 按 `lineKey` 跳转仍只拉取目标行所在块及相邻预取块，保持 I8 成立。

## 先写的测试

1. `ui/tests/result_reader.test.tsx`：
   - **Compare 精确行数测试**：构造 `takes` 各 8 行但 `Compare` 返回 1 行合并结果的场景，断言渲染出的 `<li data-row-index>` 精确为 `1` 行（零「正在加载…」幽灵行）；再构造 `takes` 各 2 行但 `Compare` 返回 4 行的场景，断言 `4` 行全部渲染无截断。
   - **搜索 `lineKey` 跳转测试**：以 `lineKey="rec-1.1.130"` 挂载 4 小时（2,560 行）`ReaderPage`，断言自动拉取第 2 块（`Math.floor(130 / 64) === 2`）并高亮 `data-line-key="rec-1.1.130"`（若使用 `k130` 键名同样验证）。
2. `ui/tests/library_settings.test.tsx`：
   - **多页失效同步刷新测试**：构造 60 条月度录音（第 1 页 50 条、第 2 页 10 条），点击「加载更多」加载第 2 页后，从数据源删除第 0 条并修改第 1 条标题，推送 `Event::Invalidate { view: 'Library', key: null, version: 2 }`，断言第 0 条已从列表消失、第 1 条呈现新标题且第 2 页仍完整保留。
   - **`initialMonth` 归零重置测试**：从 `initialMonth="2026-10"` 重渲染为 `initialMonth={null}`，断言自动切回最新月份（`2026-11`）。

## 验收

- `RUSTC_WRAPPER="" bash scripts/build/check.sh` 全绿（零 Rust 改动，全部 10 个规则脚本、707 Rust 测试、全部 UI 测试、`tsc` 与三入口 `vite build` 通过）。

## 不做

- 不修改任何 Rust 代码或 `core/bindings/*.ts`。
