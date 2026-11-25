# P6-R1-02 修复知识库表单竞态、⌘K 话题跳转、多块话题高亮与已删主题记词默认值

## 目标

修复 P6 第 1 轮审查中的 **必须改 #2、#3、#4**：

1. **修复 `KnowledgeView` 草稿与内联表单三处缺陷（必须改 #2）**：
   - 仅在切换主题（`topic?.id` 变化）时重置 `addingFact`/`addingTerm`/`addingPair`，避免编辑名称/领域/背景后点击 `+` 被 `onBlur` 保存引发的 `Invalidate(Knowledge)` 重拉瞬间关掉表单；
   - 在 `commitDraftText` 与 `applyAndSave` 中显式守卫 `draft.id === topic.id`（及 `draft.id === selectedId`），消除切换主题瞬间跨主题比对或误保存；
   - 新增与核心一致的纯校验函数 `validateTopicDraft(topic: Topic): string | null`（检查非固定主题空名 `topic.empty`、背景超 `TOPIC_MAX_BACKGROUND_CHARS = 6_000` 或总字数超 `TOPIC_MAX_CHARS = 10_000` 返 `topic.limit`）；`applyAndSave` 与 `commitDraftText` 若命中已知限制，不把脏改动留在乐观 `draft` 中（文本失焦恢复为已保存 `topic`，并经 `onSaveTopic` 或错误回调明确提示稳定错误码），避免 UI 与磁盘事实永久分叉。
2. **修复 `CommandPalette` 话题命中误跳录音末尾与 `ReaderPage` 多块话题跳转高亮丢失（必须改 #3）**：
   - `ui/src/shell/CommandPalette.tsx:181` 仅在 `hit.kind === 'Line' || hit.kind === 'Translation'` 时传 `lineKey: hit.key`，否则传 `undefined`（与 `LibraryPage.tsx:295` 一致），彻底杜绝把 `Tag` 的 `at_ms` 毫秒数当行号解析跳到录音末尾；
   - `ui/src/pages/ReaderPage.tsx:262-311` 将 `pendingTagAtMs` 改为 `useRef<number | null>(null)`（不再放进 `handleLoadedLines` 的 `useCallback` 依赖数组），且在 `handleLoadedLines` 收到新块时仅当命中行时间与目标 `at_ms` 距离在容差内（或全部块已就绪）才高亮并清空 `pendingTagAtMsRef.current`，确保 4 小时多块录音点击远端话题后目标块加载完成能准确高亮对应行。
3. **修复 `TranscriptList` 记词表单对已删主题的默认回退（必须改 #4）**：
   - `ui/src/pages/TranscriptList.tsx:121` 计算 `defaultRememberTopic` 时校验 `recordingTopics` 中的 id 确实存在于 `topicOptions` 中，否则回退为 `ALWAYS_TOPIC_ID`，避免录音原选主题被删后界面显示「我的词」实际却提交已删 id 触发 `code.topic.missing`。

## 背景

- 审查报告：`docs/build/reviews/P6-r1.md`（必须改 #2、#3、#4）。
- 涉及文件：
  - `ui/src/pages/KnowledgePage.tsx`
  - `ui/src/shell/CommandPalette.tsx`
  - `ui/src/pages/ReaderPage.tsx`
  - `ui/src/pages/TranscriptList.tsx`
  - `ui/tests/knowledge.test.tsx`
  - `ui/tests/titles.test.tsx`
  - `ui/tests/shell.test.tsx`

## 做法

### 1. `KnowledgeView`（`ui/src/pages/KnowledgePage.tsx`）

- 补充命名常量 `export const TOPIC_MAX_BACKGROUND_CHARS = 6_000;`（与 `core/src/knowledge/topic.rs::TOPIC_MAX_BACKGROUND_CHARS` 对齐）。
- 新增纯校验函数：
  ```ts
  /** Pure validation mirroring core/src/knowledge/topic.rs::normalize error codes. */
  export function validateTopicDraft(topic: Topic): 'topic.empty' | 'topic.limit' | null {
    if (!topic.pinned && !topic.name.trim()) return 'topic.empty';
    if (charLen(topic.background.trim()) > TOPIC_MAX_BACKGROUND_CHARS) return 'topic.limit';
    if (topicSizeChars(topic) > TOPIC_MAX_CHARS) return 'topic.limit';
    return null;
  }
  ```
- 将原来的单一 `useEffect([topic])` 拆分为两个职责清晰的效果：
  1. 监听 `[topic?.id]`：仅在切换主题时关闭内联表单（`setAddingFact(false); setAddingTerm(false); setAddingPair(false);`）；
  2. 监听 `[topic]`：同步 `setDraft(topic)`（不关闭正在填写的内联表单）。
- 在 `applyAndSave(action)` 中：
  - 校验 `if (!draft || !topic || draft.id !== topic.id || draft.id !== selectedId) return;`
  - 计算 `const next = topicDraftStep(draft, action);`
  - 若 `validateTopicDraft(next)` 返回错误码：不更新乐观 `draft`，直接调用 `onSaveTopic(next)`（或由容器发给核心弹出稳定错误码，或通过 `say(err)` 提示）并保持 `draft` 与 `topic` 一致；通过校验才 `setDraft(next); onSaveTopic(next);`。
- 在 `commitDraftText()` 中：
  - 校验 `if (!draft || !topic || draft.id !== topic.id || draft.id !== selectedId) return;`
  - 若未变化直接返回；
  - 若 `validateTopicDraft(draft)` 返回错误码：调用 `onSaveTopic(draft)` 让后端记录并回显稳定错误码，同时 `setDraft(topic)` 将界面输入框恢复到已保存值（不再残留未保存的空名或超限文本）；否则调用 `onSaveTopic(draft)`。

### 2. `CommandPalette`（`ui/src/shell/CommandPalette.tsx`）

- 修改 `CommandPalette` 的 `onSelect`（第 176–184 行）：
  ```tsx
  onSelect={(hit) => {
    go({
      page: 'reader',
      id: hit.recording_id,
      tab: hit.kind === 'Note' ? 'notes' : 'transcript',
      lineKey: hit.kind === 'Line' || hit.kind === 'Translation' ? hit.key : undefined,
    });
    onClose();
  }}
  ```

### 3. `ReaderPage` 多块话题跳转（`ui/src/pages/ReaderPage.tsx`）

- 定义命名常量 `const TAG_JUMP_TOLERANCE_MS = 30_000;`（替换原来散落的魔法数字 `30_000`）。
- 将 `pendingTagAtMs` 从 `useState` 改为 `const pendingTagAtMsRef = useRef<number | null>(null);`，并在 `useEffect([row.id])` 中重置为 `null`。
- `handleLoadedLines` 依赖数组收窄为 `[onLoadedLinesChange, recording.takes, selectedTake]`（不再依赖 `pendingTagAtMs`）：
  - 当 `pendingTagAtMsRef.current !== null && lines.length > 0` 时，查找 `idx = findLineIndexForTime(lines, targetAtMs)`；
  - 计算当前 `totalTakeLines = recording.takes.find((tk) => tk.take === selectedTake)?.lines ?? lines.length`；
  - 仅当 `Math.abs(lines[idx][1].t0 - targetAtMs) <= TAG_JUMP_TOLERANCE_MS || lines.length >= totalTakeLines` 时，才调用 `setFocusLineKey(lines[idx][0])` 并清空 `pendingTagAtMsRef.current = null`。
- `handleTagClick` 使用 `TAG_JUMP_TOLERANCE_MS` 与 `pendingTagAtMsRef.current = tag.at_ms`。

### 4. `TranscriptList` 默认主题回退（`ui/src/pages/TranscriptList.tsx`）

- 将第 121 行的 `defaultRememberTopic` 改为在 `topicOptions` 之后计算：
  ```ts
  const defaultRememberTopic = useMemo(
    () => recordingTopics.find((tid) => topicOptions.some((opt) => opt.id === tid)) ?? ALWAYS_TOPIC_ID,
    [recordingTopics, topicOptions],
  );
  ```

## 显式与可观测

- **显式规则与命名常量**：`validateTopicDraft` 复用与核心一致的稳定错误码（`topic.empty`、`topic.limit`）和命名常量 `TOPIC_MAX_CHARS`、`TOPIC_MAX_BACKGROUND_CHARS`、`TAG_JUMP_TOLERANCE_MS`，消除魔法数字与静默分叉。
- **dsh 边界与性能不变量**：不新增任何 `useSyncExternalStore` 或 Tauri IPC 调用，保持 `ui.sh` 全绿；`ReaderPage` 远端话题跳转仍仅靠按需比例定位 + `VList` 加载邻近块，不破坏 I8 虚拟化与 `scale_i8.test.tsx`。
- **故障可见性**：超限或空名保存仍稳定展示 `code.topic.limit` / `code.topic.empty` 且不伪造成功态；已删主题回退到 `always` 并可在测试中断言精确提交 id。

## 先写的测试

1. `ui/tests/knowledge.test.tsx` 新增测试：
   - **同主题保存刷新不关闭内联表单**：编辑 `background` → 点击 `+` 打开「添加词」表单 → `rerender` 传入同 `id` 的新 `topic` 对象（模拟 `onBlur` 保存触发的 `Invalidate` 刷新）→ 断言「添加词」输入框仍处于打开状态；再切换 `selectedId` 与 `topic` 到另一主题 → 断言表单自动关闭。
   - **超限 / 空名拦截与恢复**：测试 `validateTopicDraft` 全分支，并在 `KnowledgeView` 中验证向接近 10,000 字主题添加超限词时，乐观 `draft` 不保留被拒收词条且调用 `onSaveTopic` 报告错误；清空自定义主题名失焦后输入框恢复为原名。
2. `ui/tests/titles.test.tsx` 新增测试：
   - **多块长录音话题跳转**：模拟 2 块（chunk 0 覆盖 `0..64` 行 `0..64s`，chunk 2 覆盖远端行 `7,200,000 ms`）异步分步返回，点击 `2:00:00` 话题胶囊 → 验证 chunk 0 不会提前清掉高亮目标，待 chunk 2 加载后精确高亮 `7,200,000 ms` 所在行。
   - **已删主题记词默认值回退**：`recording.topics = ['deleted_topic_id']` 而 `topics` 中仅含 `always` 与其他存活主题 → 点「+ 记进知识库」直接提交 → 断言 `onRememberTerm` 收到 `topicId === 'always'` 而非 `'deleted_topic_id'`。
3. `ui/tests/shell.test.tsx` 新增/扩充测试：
   - 在 `CommandPalette` 中点击 `kind: 'Tag'`（`key: '40000'`）搜索命中，断言路由跳转到 `{ page: 'reader', id, tab: 'transcript', lineKey: undefined }`（不再把 `'40000'` 传给 `lineKey`）。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `ui.sh`、`tsc`、`vite build` 与全部 Vitest / Rust 测试）。

## 不做

- 不修改 Rust 后端文件（归 `P6-R1-01`）；
- 不新增 npm 依赖或 i18n 键。
