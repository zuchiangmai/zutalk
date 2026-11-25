状态：完成

# P6-04：界面知识库、首页选主题、阅读页「记进知识库」与标题/话题

## 改动与文件

1. **路由与外壳接入**（`ui/src/bus/route.ts`、`ui/src/shell/Shell.tsx`）：
   - `Route` 新增 `{ page: 'knowledge'; topicId?: string }`；`Shell.tsx` 在 `route.page === 'knowledge'` 挂载 `<KnowledgePage initialTopicId={route.topicId} />` 并复用页面自有 `<header className="top">` 返回设置，避免重复顶栏。
2. **新建知识库页**（`ui/src/pages/KnowledgePage.tsx`）：
   - 拆分为纯 props `KnowledgeView` 与薄总线容器 `KnowledgePage`（严守 `ui.sh` dsh 边界）。
   - 定义命名常量 `ALWAYS_TOPIC_ID = 'always'`、`TOPIC_MAX_CHARS = 10_000`、`MAX_SELECTED_TOPICS = 8`，及纯函数 `topicSizeChars`、`topicBudgetPercent` 与 `topicDraftStep`。
   - 对照 `Knowledge.dc.html` 完整实现左轨主题列表（固定「每场都用」置顶不可删 + 自定义主题 + `"+ 新主题"` 行内新建）与右编辑区（10,000 字篇幅条、IME 组合键防护 `isComposing || keyCode === 229`、领域与概况胶囊、背景文本域、词与文件提取虚线建议词采纳胶囊、译法对、`"从文件里提"` 走 `bus/act.ts::pick()` 派发 `ImportTopicFile`、非固定主题红底两步 `InlineConfirm` 删除、以及底栏常驻 `DECISIONS.md` #7 隐私说明）。
3. **设置页与首页升级**（`ui/src/pages/SettingsPage.tsx`、`ui/src/pages/HomePage.tsx`）：
   - `SettingsPage.tsx` 将旧扁平词表区升级为简洁的「知识库」入口卡片，展示主题数量（`Query::Topics`）与 `DECISIONS.md` #7 隐私说明，点击进入 `{ page: 'knowledge' }`。
   - `HomePage.tsx` 对照 `Main.dc.html` 在「字幕」与「声音」之间新增「主题」选择行：已选主题激活胶囊可点 `×` 移除、末尾 `"+"` 弹出选择器封顶 `MAX_SELECTED_TOPICS = 8` 并附 `"管理知识库"` 入口、固定主题非空时给出低调提示。
4. **阅读页、文字稿列表、结果页与全部录音页**（`ui/src/pages/ReaderPage.tsx`、`ui/src/pages/TranscriptList.tsx`、`ui/src/pages/ResultPage.tsx`、`ui/src/pages/LibraryPage.tsx`）：
   - 对照 `Titles.dc.html`：根据 `row.title_by === 'Hand'` 区分粗体 `.title` 与常规字重 `.title.auto`；标题右侧新增「换一个标题」候选弹窗（展示来源标签、勾选当前项、底部 `"重新生成"` 派发 `GenerateTitle`，接第 4 槽 `progress.title` 加载态）。
   - 新增纯函数 `findLineIndexForTime(lines, atMs)`，点击 `.topics` 话题时间胶囊可定位高亮对应行（大录音按比例跳块加载，不破坏 `VList` I8 常量 DOM 上限）；`ResultPage.tsx` 同步展示 `row.tags` 与非阻断 `step.titling` 提示。
   - 对照 `Reader.dc.html`：在 `TranscriptList.tsx` 中对手改行内联显示 `"+ 记进知识库"` 快捷按钮（普通行亦在行操作提供），展开紧凑入库表单默认指向当前录音首个主题（否则 `"always"`），提交派发 `Command::RememberTerm`。
   - 对照 `Library.dc.html`：`LibraryPage.tsx` 每行展示 `row.tags` 可点击话题副标题，点击即切换 `filter.tag` 并在筛栏显示可清除胶囊；搜索命中支持 `SearchHitKind === 'Tag'`。
5. **设计系统、开发替身与 8 语同步**（`ui/src/ds/{icons.tsx,controls.css}`、`ui/dev/mock.ts`、`ui/lang/*.json`）：
   - 新增 `book`、`file-text`、`arrow-right`、`sparkles` 图标与 `.title.auto`、`.topics`、`.topic`、`.kb-*`、`.remember-*`、`.lrow-tags` 样式；零外网 `@import`、零不安全 HTML 注入。
   - `ui/dev/mock.ts` 同步虚构读书会与社区周会样例（零真实人名/句柄）；全部 8 份 `ui/lang/*.json` 同步 53 个新增 UI 文案键并通过 `words.test.ts` 校验。

## 证据

- **新增测试（2 个测试文件共 9 项，先红后绿全部通过）**：
  - `ui/tests/knowledge.test.tsx`（4/4 通过）：
    - `computes topicBudgetPercent clamped to 0..100 and counts topicSizeChars`
    - `transitions topic draft cleanly across facts, terms, suggested adoption, and pairs`
    - `shows pinned always topic without delete, renders custom topic budget/fields/privacy, adopts suggestions, imports file, and requires two-step delete`
    - `adds and removes selected topics, shows always hint, opens knowledge base, and enforces 8-topic cap`
  - `ui/tests/titles.test.tsx`（5/5 通过）：
    - `locates the line containing or closest to a tag timestamp`
    - `distinguishes AI regular title (.title.auto) from Hand bold title (.title), picks candidates, and regenerates`
    - `jumps and highlights the matching transcript line when clicking a tag chip, and remembers terms from hand-edited lines`
    - `renders tag chips and non-blocking titling status`
    - `shows row tags subtitle, filters by clicking a tag chip, clears active tag filter, and displays Tag search hits`
- **全量前端门禁与规则检查**：
  - `cd ui && npm run check && npm test && npm run build`：**16 个测试文件 / 118 项测试全部通过**（含 `words.test.ts` 12/12、`scale_i8.test.tsx` 4/4、`library_settings.test.tsx` 10/10、`result_reader.test.tsx` 12/12、`home_live.test.tsx` 6/6、`shell.test.tsx` 11/11、`bus.test.ts` 17/17），TypeScript 与 Vite 3 入口构建全部通过。
  - `bash scripts/build/rules/ui.sh`：通过（0 违规）。
  - `ui/package.json` 与 `ui/package-lock.json` 零新增外部依赖。

## 遗留 / 需注意

- 读书会端到端验收、真实落盘隐私字节审计、升级 `pending.sh` 与全仓收尾归 `P6-05`。
