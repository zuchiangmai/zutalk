状态：完成

## 改动与文件

完成停止以后与精修四态页（`ResultPage`）、64 行按需分块虚拟化正文与对比引擎（`TranscriptList`）以及阅读页（`ReaderPage`），接入 `Shell` 并同步全部 8 语字典：

1. **总线多查询聚合（`ui/src/bus/query.ts`）**：
   - 新增 `useQueries(queries)`，把动态可见块集合（`Query::Transcript` / `Query::Compare`）组合为单一 `Source` 并继续收口在 `ui/src/bus/` 内，保持 `ui.sh` dsh 边界零违规，同时仅订阅视口邻近块，离开视口即自动退订。
2. **64 行按需分块虚拟列表（`ui/src/pages/TranscriptList.tsx`）**：
   - 具名常量：`TRANSCRIPT_CHUNK_LINES = 64`、`TRANSCRIPT_PREFETCH_CHUNKS = 1`、`TRANSCRIPT_ROW_ESTIMATE_PX = 84`。
   - 纯函数 `chunksForRange(startIndex, endIndex, totalChunks)` 将 `VList` 视口区间映射为有界分块集合；`VList` 接收 `{ length: totalLines }` 轻量类数组，绝不循环预拉全部块。
   - 支持原文与译文行内编辑（严格 IME 保护：`e.nativeEvent.isComposing || e.keyCode === 229` 不误提交；Enter 提交 `Change::Line`、Escape 取消）、说话人单行点击改名（`Change::Speaker`）、`via.hand` / `via.unclear` / `via.incomplete` 标注、行下锚定笔记展示与行内新增笔记（`NoteOp::Add` 带 `anchor` 与 `at_ms`），以及 `Compare` 字符级 `<del>` / `<mark class="find">` 高亮。
3. **停止以后与精修页（`ui/src/pages/ResultPage.tsx`）**：
   - 具名常量 `REFINE_STEPS` 与纯函数 `refineStepIndex(step)`。
   - 拆出纯 props `ResultView` 与连总线容器 `ResultPage`，完整实现 4 种精修态：`Refining`（5 步进度条 + 微光步骤词 + `done / of`）、`Draft`（有音频给「精修」与原因提示，无音频显示「音频已销毁 · 不能再精修」）、`Failed`（含 `keys.async_refused` 稳定错误词 +「再试」+「打开设置」）、`Refined`（打开阅读与一键对比）。正文区复用 `TranscriptList`（原则 3）。
4. **阅读页（`ui/src/pages/ReaderPage.tsx`）**：
   - 拆出纯 props `ReaderView` 与连总线容器 `ReaderPage`，支持可编辑标题（`Change::Title`）、文字稿/笔记双标签切换、语言胶囊过滤、版本分段器与 `Compare` 开关、纯文本复制（`words.text`）与动态遍历 `State.exporters` 的导出菜单、原处两步确认 `InlineConfirm` 销毁音频（`Command::DestroyAudio`）、软删撤销（`removeWithUndo`），以及笔记标签页的时间戳跳回文字稿与增改删（`NoteOp::Add`/`Edit`/`Remove`）。
5. **外壳、样式、测试辅助与 8 语字典**：
   - `ui/src/shell/Shell.tsx`：接入 `ResultPage` 与 `ReaderPage`。
   - `ui/src/ds/controls.css`、`ui/src/ds/icons.tsx`：补充阅读/精修/对比/资料库/字幕窗样式与 `copy` 图标。
   - `ui/tests/dom.ts`：使 `DeterministicResizeObserver` 走微任务异步回调并提供 `offsetParent` 与项高度估计，确保 `virtua` `VList` 在 jsdom 下按真实浏览器时序测量视口并执行窗口裁剪。
   - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`：8 份字典同步补齐 21 个新键。

## 验证证据

- `cd ui && npm run check && npm test` 全部通过（`tsc` 零错误，三入口 `vite build` 成功）。
- 全部 10 个规则脚本通过（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）。
- UI 测试 9 个文件 64 条全部通过，其中新增 `ui/tests/result_reader.test.tsx`（8 条）：
  - `computes bounded prefetch chunks for visible ranges`
  - `maps refine step codes to 0..4 and unknown codes to -1`
  - `renders Refining progress steps, Draft with/without audio, Failed async_refused, and Refined`
  - `virtualizes a 4-hour 2,560-line recording and fetches only visible chunks (I8)`（4 小时 2,560 行仅挂载视口内 ≤ 40 个 `<li data-row-index>`，仅拉取块 0/1，绝未拉取块 39）
  - `switches into Compare mode and highlights Delete and Insert diffs`
  - `edits lines with IME protection, edits title, and renames speakers`
  - `adds anchored row notes and edits/removes notes in the Notes tab`
  - `requires two-step confirmation for DestroyAudio and offers Undo on Delete`
- 本任务零 Rust 代码改动。

## 遗留与需负责人确认

- 「记进知识库」（P6）与「交到档案馆按住 1.4 秒」（P5）按计划留待后续阶段。
- 全部录音页（`LibraryPage`）、设置壳（`SettingsPage`）、独立字幕窗与观众页按计划在 P3-05、P3-06 继续完成。
