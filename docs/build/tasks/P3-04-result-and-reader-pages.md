# P3-04 停止以后、精修四态与虚拟化分块阅读页（virtua + Compare + 笔记）

## 目标

做完以后：

1. **停止以后与精修卡片（`ui/src/pages/ResultPage.tsx`，对照 `Result.dc.html` + `Refine.dc.html`）**：
   - 录完即落在此页（`route.page === 'result'`），顶部可编辑标题、元数据（时间、时长、语言、现场稿/音频徽章）；
   - 「录好了」卡片展示时长、复制与导出快捷按钮，以及实时精修状态区：
     - **精修中**（`Progress.refine?.id === id` 或 `refine.is === 'Refining'`）：5 步进度条（`closing` → `uploading` → `transcribing` → `translating` → `signing`）+ 微光步骤词 + 进度 `done / of`；
     - **没精修**（`refine.is === 'Draft'`）：若音频还在，显示「精修」按钮（派发 `Command::Refine { id }`）与原因提示（如 `keys.none` 引导去设置）；若音频已销毁（`!row.audio`），显示「音频已销毁 · 不能再精修」；
     - **没写成**（`refine.is === 'Failed'`）：显示红色警示徽章「精修没写成」+ 具体错误码词（如 `keys.async_refused`：「没精修 · 需要能整场转写的钥匙」）+「再试」按钮（`Command::Refine { id }`）+ 当错误与钥匙有关时的「打开设置」跳转按钮；
     - **写好**（`refine.is === 'Refined'`）：允许直接在此阅读或切到阅读页完整功能（实际上 `ResultPage` 复用 `ReaderPage` 的分块正文引擎，避免重复两套正文列表实现，践行原则 3「同类只有一个实现」）。
2. **虚拟化分块阅读页（`ui/src/pages/ReaderPage.tsx`，对照 `Reader.dc.html` + `Refine.dc.html`）**：
   - **64 行按需分块 + `virtua` `VList` 虚拟列表（I8 核心要求）**：
     - 根据总行数 `total_lines`（来自 chunk 0 的 `TranscriptChunk`）把整场录音按 `TRANSCRIPT_CHUNK_LINES = 64` 映射到虚拟列表项索引；
     - 仅当某个 `chunk` 进入当前视口（经 `VList` 的 `onRangeChange` 触发，默认预取当前块与相邻 1 块）时，才通过 `bus/query` 订阅/拉取对应的 `Query::Transcript { id, take, chunk }`；
     - DOM 中同时挂载的 `<li>` 行节点数严格受视口高度约束，与 1 分钟或 4 小时（2,560 行）总时长无关！
   - **版本切换与对比（`Query::Compare`）**：
     - 当 `takes.length > 1` 或已精修时，工具栏显示版本分段器（「精修稿」/「现场稿」，多于两版时列出「第 N 版」）与「对比」开关；
     - 打开「对比」后，同样按 64 行块虚拟化拉取 `Query::Compare { id, take_a: 0, take_b: selectedTake, chunk }`，使用 `CompareRow.ops`（`Equal` / `Delete` / `Insert`）高亮字符级改动（删除线标旧字、薄荷底 `<mark className="find">` 标精修新字）。
   - **行内编辑与记录变更**：
     - 标题框失焦或回车发 `Command::Change { id, change: { what: 'Title', text } }`；
     - 点击说话人名改名（`Change::Speaker`），点击原文或译文行内编辑（`Change::Line`），严格处理 IME 组合输入（`e.nativeEvent.isComposing || e.keyCode === 229` 不误提交），标注 `via === 'hand'`（「已改」）、`via === 'unclear'`（虚线红底 +「听不清」）与 `incomplete`（「不完整」）；
     - 语言显示切换胶囊控制哪几种译文展开显示。
   - **笔记标签页与行内锚定笔记**：
     - 顶栏分段器切换「文字稿」/「笔记」；
     - 锚定到具体行（`note.anchor === lineKey`）的笔记直接显示在该行下方（空正文显示为小旗「标记」），并提供该行「+ 笔记 / 标记」入口（发 `Command::Note { id, op: { op: 'Add', at_ms: line.t0, text, anchor: lineKey } }`）；
     - 「笔记」标签页列出该场全部带时间笔记（含边录边记留下的无行键笔记），支持新增、行内编辑（`NoteOp::Edit`）、删除（`NoteOp::Remove`）以及点击时间戳跳转定位到文字稿对应行。
   - **复制、数据驱动导出、销毁音频与软删除**：
     - 复制：把当前选中语言文本经 `words.text` 写入剪贴板并提示 `copied`；
     - 导出菜单：动态遍历 `state.exporters`（`markdown` / `srt` / `vtt`），通过 `pick()` 选路径并派发 `Command::Export`；
     - 更多菜单内含：使用 `InlineConfirm`（「原处再按一次」，红底确认）的「销毁音频」（`Command::DestroyAudio`）与「删除」（`removeWithUndo(id)`，弹出 8 秒撤销通知并回首页）。
3. **接入 `Shell.tsx`** 并补齐 8 语字典新增键。

## 背景

- 设计：`docs/build/phases/P3.md` §3.2、§3.4–§3.5；`docs/ui-2026-11-21/{Result.dc.html,Refine.dc.html,Reader.dc.html,States.dc.html}`；`docs/PLAN.md` §6.3、§7（I8）。
- 涉及文件：
  - 新增 `ui/src/pages/ResultPage.tsx`、`ui/src/pages/ReaderPage.tsx`（可拆内部子模块 `ui/src/pages/TranscriptList.tsx` 供 `ResultPage` 与 `ReaderPage` 共用）
  - 更新 `ui/src/shell/Shell.tsx`（接入 `ResultPage` 与 `ReaderPage`）
  - 新增 `ui/tests/result_reader.test.tsx`
  - 更新 `ui/lang/*.json`（8 份文件）

## 做法

### 1. 64 行按需分块虚拟列表（`TranscriptList.tsx`）

- 命名常量：
  - `TRANSCRIPT_CHUNK_LINES = 64`（与 `core/src/view/transcript.rs` 一致）
  - `TRANSCRIPT_PREFETCH_CHUNKS = 1`（预取可见范围前后各 1 块，滚动不闪白）
  - `TRANSCRIPT_ROW_ESTIMATE_PX = 84`（给 `VList` 的 `itemSize` 初值）
- 纯函数 `chunksForRange(startIndex: number, endIndex: number, totalChunks: number): number[]`：把 `VList` 报告的可见行区间 `[startIndex, endIndex]` 转换为需要拉取的块号集合（夹紧在 `[0, totalChunks - 1]`，前后扩 `TRANSCRIPT_PREFETCH_CHUNKS`）。
- 对总行数 `total_lines` 渲染 `VList`（来自 `virtua`），按索引 `i` 计算 `chunkIdx = Math.floor(i / TRANSCRIPT_CHUNK_LINES)` 与 `offset = i % TRANSCRIPT_CHUNK_LINES`：
  - 已到达的块直接渲染该行；尚未到达的块渲染等高轻量占位行，避免撑爆滚动条；
  - Compare 模式下同理用 `CompareChunk` 按块索引渲染 `CompareRow`，把 `DiffOp` 渲染为纯语义元素（`Equal` 普通文本、`Delete` `<del>`、`Insert` `<mark className="find">`）。
- 严禁在任何地方循环拉取全部 chunks 或拼接全量数组！

### 2. 精修步骤映射与 `ResultPage.tsx`

- 纯函数 `refineStepIndex(step: string): number` 把 `['closing', 'uploading', 'transcribing', 'translating', 'signing']` 映射到 0..4（未知重试码如 `'offline'` 时保持当前态并展示 `word('step code', step)`）。
- 当 `ResultPage` 收到 `Invalidate('Recording', id)` 显示 `refine.is === 'Refined'` 时，卡片平滑切换为「已写好精修稿」操作条，可直接切版或点「对比」。

### 3. `ReaderPage.tsx` 细节

- **单主按钮纪律**：工具栏里仅「复制」（或未精修时「精修」）一个主按钮 `.btn`，其余语言筛选、版本、对比、导出均为 `.seg` / `.pill`。
- **行内编辑安全**：保存前检查 `draft.trim() && draft.trim() !== original`；`Escape` 取消；`isComposing` 期间忽略 Enter；录音进行中（`session?.id === id`）隐藏删除与销毁音频按钮。
- **锚定与跳转**：支持从 `CommandPalette`（`route.lineKey`）或「笔记」标签页点击时间戳跳回文字稿并高亮对应行（若目标行不在当前块，算出目标行号调 `vlistRef.current?.scrollToIndex(...)` 并高亮）。

## 显式与可观测

- **纯函数规则**：`chunksForRange`、`refineStepIndex`、`stateView.refineStatusView` 均独立可测。
- **具名常量**：`TRANSCRIPT_CHUNK_LINES = 64`、`TRANSCRIPT_PREFETCH_CHUNKS = 1`、`TRANSCRIPT_ROW_ESTIMATE_PX = 84`。
- **有界请求**：同一时刻只拉取视口所覆盖的少量分块，且所有查询走 `QueryCache`（上限 128 项），不随录音时长线性增长。
- **故障可见性**：精修失败（含 `keys.async_refused`、`timeout`）与行级 `unclear` / `incomplete` 均有显式标签与恢复路径（再试 / 打开设置）。

## 先写的测试

新建 `ui/tests/result_reader.test.tsx`（顶部声明 `// @vitest-environment jsdom`，引入 `./dom`）：

1. **`ResultPage` 精修四态测试**：
   - `Refining`：渲染 5 步步骤条与 `translating` 步骤词、`done / of`；
   - `Draft`（有音频）：点击「精修」派发 `Command::Refine { id }`；
   - `Draft`（无音频）：显示「音频已销毁 · 不能再精修」，无精修按钮；
   - `Failed('keys.async_refused')`：展示错误文案、「再试」按钮（派发 `Command::Refine`）与「打开设置」按钮。
2. **`ReaderPage` 4 小时分块虚拟化测试（I8）**：
   - 构造模拟 2,560 行（40 个 64 行块）的录音查询替身，挂载 `ReaderPage` 在 600 px 模拟视口下；
   - 断言实际渲染的 `<li data-row-index>` 数量远小于 2,560（例如 ≤ 40），且 `Query::Transcript` 只拉取了前几块（chunk 0 / 1），绝未拉取 chunk 39！
3. **`Compare` 对比测试**：
   - 点击「对比」触发 `Query::Compare`，渲染 `Delete` 与 `Insert`（`<mark class="find">`）字符级高亮。
4. **行内编辑与 IME 保护测试**：
   - 点击正文进入编辑框，发 `Enter` + `isComposing: true` 断言**不提交**；发普通 `Enter` 断言派发 `Command::Change { what: 'Line' }`；改标题派发 `Change::Title`；改说话人派发 `Change::Speaker`。
5. **笔记增改删测试**：
   - 在行上点记笔记派发 `NoteOp::Add`（带 `anchor` 与 `at_ms`）；在「笔记」标签页编辑与删除分别派发 `NoteOp::Edit` 与 `NoteOp::Remove`。
6. **不可逆与可撤销动作测试**：
   - 销毁音频首次点击仅进入确认态不发命令，二次点击派发 `Command::DestroyAudio`；
   - 删除派发 `Command::Remove` 并触发撤销通知。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 8 语 `words.test.ts` 与 `ui.sh`）。

## 不做

- 不在本任务实现「记进知识库」（P6）与「交到档案馆按住 1.4 秒」（P5）。
- 不修改 Rust 导出器列表（完全按 `State.exporters` 数据驱动）。
