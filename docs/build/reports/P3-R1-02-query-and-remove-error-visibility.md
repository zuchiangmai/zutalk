状态：完成

## 改动与文件

修复第 1 轮审查（`docs/build/reviews/P3-r1.md`）**必须改 #2**：确保 `Command::Remove` 拒绝时不误跳转也不覆盖错误通知，并在阅读页、停止结果页、分块正文与资料库页显式呈现 `QueryState.error`（原则 5、10）：

1. **`ui/src/bus/act.ts`**：
   - `send(command)` 改为返回 `Promise<boolean>`（成功返回 `true`，无总线或总线拒绝时 `say(code)` 后返回 `false`），`diagnose()` 透传该返回值。
   - `removeWithUndo(id)` 改为先 `await send({ do: 'Remove', id })`；仅当返回 `true` 时才执行 `navigate({ is: 'RecordingRemoved', id })` 与 `say('remove', { do: 'Restore', id })`，拒绝时保留在当前页并在 `NoticeToast` 清晰展示真实拒绝码。
2. **`ui/src/pages/TranscriptList.tsx`**：
   - 首块无数据时显式区分 `status === 'error'`（以 `role="status"` 渲染 `word('code', error)`）、`idle`/`loading`（显示 `t('reader.loading')`）与真正的 `ready && totalLines === 0`（显示 `t('notranscript')`）。
   - 维护 `chunkErrorMap` 记录非 0 块查询错误，缺失块行占位在出错时显式展示 `word('code', errCode)`（`role="status"`），不再永久显示「正在加载…」。
3. **`ui/src/pages/ResultPage.tsx` 与 `ui/src/pages/ReaderPage.tsx`**：
   - 录音详情为空时按 `recQuery.status` 显式分支：`error` → `role="status"` 渲染 `word('code', recQuery.error ?? 'busy')`；`idle`/`loading` → `t('reader.loading')`；`ready` → `t('code.recording.gone')`。
4. **`ui/src/pages/LibraryPage.tsx`**：
   - `LibraryViewProps` 新增可选属性 `error?: string | null`，`LibraryPage` 分别传入搜索态 `searchQuery.error` 或浏览态 `monthQuery.error ?? summaryQuery.error`；出错时以 `role="status"` 呈现 `word('code', error)`，不再误报 `t('nomatch')`。
5. **回归测试（先红后绿）**：
   - `ui/tests/bus.test.ts`：`keeps current route and preserves refusal notice when removeWithUndo is rejected`
   - `ui/tests/result_reader.test.tsx`：`surfaces Recording and Transcript query errors instead of reporting gone or empty transcript`
   - `ui/tests/library_settings.test.tsx`：`surfaces LibraryMonth and Search query errors instead of showing No matching recordings`

## 验证证据

- TDD 实测：修复前 3 条新增回归测试全部复现原缺陷并失败，修复后全部转绿。
- `cd ui && npm run check && npm test` 全部通过（`tsc` 零报错，三入口 `vite build` 成功）。
- 全部 10 个规则脚本通过（含 `ui.sh` 与 `pending.sh`）。
- UI 测试 12 个文件 **82 条全部通过**（`bus` 17、`result_reader` 9、`library_settings` 5 等）。
- 本任务零 Rust 代码改动，零语言字典改动。

## 遗留与需负责人确认

- 无；继续执行 `P3-R1-03`。
