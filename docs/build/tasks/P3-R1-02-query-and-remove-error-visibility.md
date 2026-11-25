# P3-R1-02 修复删除失败误跳转覆盖错误与查询错误码界面可见性

## 目标

做完以后：

1. **`removeWithUndo` 不再在 `Remove` 被拒绝时误跳转或掩盖错误码**：
   - `ui/src/bus/act.ts` 的 `send(command)` 返回 `Promise<boolean>`（命令成功返回 `true`，无总线或总线拒绝时写入 `say(code)` 并返回 `false`）。
   - `removeWithUndo(id)` 先 `const ok = await send({ do: 'Remove', id })`；仅当 `ok === true` 时才执行 `navigate({ is: 'RecordingRemoved', id })` 与 `say('remove', { do: 'Restore', id })`。命令被拒绝（如 `'busy'`、`'starting'`）时，用户留在当前页，底部 `NoticeToast` 清晰保留真实的拒绝错误码。
2. **核心查询页面显式展示 `QueryState.error`，绝不吞错误报**：
   - `ui/src/pages/TranscriptList.tsx`：
     - chunk 0 查询失败（`status === 'error'` 且无已有数据）时，以 `role="status"` 渲染 `word('code', error)`，不再误报「没有文字稿」；
     - 非 0 块查询失败且该块无数据时，行占位显式渲染 `word('code', error)`（而非永久「正在加载…」）；
     - 区分 `idle`/`loading`（加载中）与真正的 `ready && totalLines === 0`（无文字稿）。
   - `ui/src/pages/ResultPage.tsx` 与 `ui/src/pages/ReaderPage.tsx`：
     - 区分 `recQuery.status === 'idle' || recQuery.status === 'loading'`（显示 `t('reader.loading')`）、`recQuery.status === 'error'`（以 `role="status"` 显示 `word('code', recQuery.error ?? 'busy')`）以及 `recQuery.status === 'ready' && !recording`（显示 `t('code.recording.gone')`）。
   - `ui/src/pages/LibraryPage.tsx`：
     - 将查询错误码（搜索态取 `searchQuery.error`，浏览态取 `monthQuery.error ?? summaryQuery.error`）传入 `LibraryView`；出错时以 `role="status"` 显式展示 `word('code', error)`，不再误报 `t('nomatch')`。

## 背景

- 审查来源：`docs/build/reviews/P3-r1.md` **必须改 #2**。
- 原则依据：`CLAUDE.md` 原则 5（失败是事实，什么都不吞，给出稳定错误码）、原则 10（故障必须可见）、`docs/build/phases/P3.md` §3.3（「其他错误码写入 `entry.error` 并在界面可见」）。
- 涉及文件：
  - `ui/src/bus/act.ts`
  - `ui/src/pages/TranscriptList.tsx`
  - `ui/src/pages/ResultPage.tsx`
  - `ui/src/pages/ReaderPage.tsx`
  - `ui/src/pages/LibraryPage.tsx`
  - `ui/tests/bus.test.ts`、`ui/tests/result_reader.test.tsx`、`ui/tests/library_settings.test.tsx`（或 `ui/tests/scale_i8.test.tsx`）

## 做法

1. `ui/src/bus/act.ts`：
   - 修改 `send(command: Command): Promise<boolean>`，成功返回 `true`，失败/无总线 `say(code)` 后返回 `false`；
   - 修改 `removeWithUndo(id: string): Promise<boolean>`：
     ```ts
     export async function removeWithUndo(id: string): Promise<boolean> {
       const ok = await send({ do: 'Remove', id });
       if (!ok) return false;
       navigate({ is: 'RecordingRemoved', id });
       say('remove', { do: 'Restore', id });
       return true;
     }
     ```
   - `diagnose()` 保持透传 `send({ do: 'Diagnose' })` 返回 `Promise<boolean>`（兼容所有现有 `void diagnose()` 调用方）。
2. `ui/src/pages/TranscriptList.tsx`：
   - 从 `transcriptStates[0]` / `compareStates[0]` 提取首块状态与错误码；若首块无数据且 `status === 'error'`，返回 `<p className="k" role="status">{word('code', initialError)}</p>`；若首块 `status === 'idle' || status === 'loading'` 且无数据，返回 `<p className="k">{t('reader.loading')}</p>`。
   - 构建 `chunkErrorMap: Map<number, string>` 记录当前订阅分块的 `error`；渲染非 0 块缺失行时，若 `chunkErrorMap.has(chunkIdx)` 则显示 `word('code', chunkError)`，否则显示 `t('reader.loading')`。
3. `ui/src/pages/ResultPage.tsx` 与 `ui/src/pages/ReaderPage.tsx`：
   - 在 `!recording || !recording.row` 分支显式按 `recQuery.status` 区分：`'error'` → `word('code', recQuery.error ?? 'busy')`（带 `role="status"`）；`'idle' | 'loading'` → `t('reader.loading')`；`'ready'` → `t('code.recording.gone')`。
4. `ui/src/pages/LibraryPage.tsx`：
   - 给 `LibraryViewProps` 新增可选属性 `error?: string | null`（默认 `null`，保持现有单测调用向后兼容）；
   - `LibraryPage` 传入 `error={isSearching ? searchQuery.error : (monthQuery.error ?? summaryQuery.error)}`；
   - `LibraryView` 在存在 `error` 且当前结果为空时，以 `role="status"` 渲染 `word('code', error)`（而不是 `t('nomatch')`）。

## 显式与可观测

- **显式分支**：每个页面按 `QueryStatus`（`'idle' | 'loading' | 'ready' | 'error'`）显式分支，不再把 `'error'` 落入默认空态文案。
- **失败是事实**：所有命令拒绝与查询失败均直达界面 `role="status"` 或 `NoticeToast`，以 `word('code', code)` 展示稳定错误词条，绝不静默吞掉或被伪成功提示覆盖。
- **故障可见性测试**：分别注入 `Remove` 拒绝（`'busy'`）与 `Query` 失败（`'index'`），断言页面与通知均清晰可见错误文案且不产生错误导航。

## 先写的测试

1. `ui/tests/bus.test.ts`：新增测试——当总线对 `Command::Remove` 返回拒绝 `'busy'` 时，调用 `removeWithUndo('r1')` 返回 `false`，当前 `routeSource`（`{ page: 'reader', id: 'r1' }`）**保持不变**，且 `noticesSource` 最后一条为 `'busy'`（绝不出现 `code: 'remove'` 撤销通知）。
2. `ui/tests/result_reader.test.tsx`：新增测试——分别模拟 `Query::Recording` 与 `Query::Transcript` 返回拒绝 `'index'`，断言页面呈现 `Local index could not be read or updated`（`word('code', 'index')`），绝不误报 `This recording is gone` 或 `No transcript`。
3. `ui/tests/library_settings.test.tsx`：新增测试——模拟 `Query::LibraryMonth` 或 `Query::Search` 返回拒绝 `'index'`，断言 `LibraryPage` 显示 `Local index could not be read or updated`（`role="status"`），绝不误报 `No matching recordings`。

## 验收

- `RUSTC_WRAPPER="" bash scripts/build/check.sh` 全绿（零 Rust 改动，全部 10 个规则脚本、707 Rust 测试、全部 UI 测试、`tsc` 与三入口 `vite build` 通过）。

## 不做

- 不修改任何 Rust 代码或语言字典（`'index'`、`'busy'` 等错误码在 8 语字典中均已存在）。
