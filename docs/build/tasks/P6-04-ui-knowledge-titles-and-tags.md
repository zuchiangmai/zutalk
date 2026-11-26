# P6-04：界面知识库、首页选主题、阅读页「记进知识库」与标题/话题

## 目标

严格对照设计稿 `Knowledge.dc.html`、`Titles.dc.html`、`Main.dc.html`、`Reader.dc.html` 与 `Library.dc.html`，在 React 19 dsh 架构边界（`ui.sh` 全绿、零新增 npm 包）下完整落地 P6 界面：新建 `KnowledgePage.tsx`（主题轨、固定「每场都用」主题、10,000 字篇幅条、概况/背景/词/译法编辑、文件提取虚线建议词采纳、两步红底 `InlineConfirm` 删除与 `DECISIONS.md` #7 隐私提示）、在 `HomePage.tsx` 加入录音前主题选择器、在 `ReaderPage.tsx` / `TranscriptList.tsx` / `ResultPage.tsx` 加入字重区分的 AI 标题、换标题候选弹窗与重新生成按钮（接第 4 槽 `progress.title`）、可点跳对应行的话题时间胶囊、以及行内「+ 记进知识库」入库弹窗；在 `LibraryPage.tsx` 展示话题副标题并支持话题过滤与 `Tag` 搜索命中；同步 `ui/dev/mock.ts`、8 语字典与完整 Vitest 测试。

## 背景

- 计划依据：`docs/PLAN.md` §3、§6；`docs/build/DECISIONS.md` 第 1 条与第 7 条；`docs/build/phases/P6.md` §1 E4、§3.2–§3.5；`docs/ui-2026-11-21/{Knowledge,Titles,Main,Reader,Library}.dc.html`。
- 涉及文件：
  - `ui/src/bus/route.ts`（新增 `{ page: 'knowledge'; topicId?: string }` 路由）
  - `ui/src/shell/Shell.tsx`、`ui/src/shell/CommandPalette.tsx`（接入 `KnowledgePage` 与 `SearchHitKind === 'Tag'` 标签展示）
  - `ui/src/pages/KnowledgePage.tsx`（新建：纯 props `KnowledgeView` + 薄总线容器 `KnowledgePage` + 纯辅助 `topicCharBudget`）
  - `ui/src/pages/SettingsPage.tsx`（将旧内嵌扁平词表区升级为「知识库」入口卡片，显示主题数与直达 `{ page: 'knowledge' }`，同时保留返回路径）
  - `ui/src/pages/HomePage.tsx`（对照 `Main.dc.html` 增加「主题」选择胶囊与弹出选择器 + 「打开知识库」入口）
  - `ui/src/pages/ReaderPage.tsx`、`ui/src/pages/TranscriptList.tsx`、`ui/src/pages/ResultPage.tsx`（对照 `Titles.dc.html` + `Reader.dc.html`：标题字重区分、换标题候选弹窗与重新生成、话题胶囊点击跳转、「+ 记进知识库」一键入库弹窗）
  - `ui/src/pages/LibraryPage.tsx`（对照 `Library.dc.html`：每行显示 `tags` 副标题、支持按话题过滤芯片与 `Tag` 搜索命中）
  - `ui/src/ds/{icons.tsx,zt.css,controls.css}`（补齐 `refresh` / `sparkles` / `book` / `file-text` 等所需图标与知识库/话题胶囊样式）
  - `ui/dev/mock.ts`（完整交互式主题 CRUD、文件提取模拟、记忆词条、标题候选与生成模拟）
  - `ui/lang/*.json`（全部 8 种语言同步新增 UI 键）
  - `ui/tests/knowledge.test.tsx`、`ui/tests/titles.test.tsx`（新建 Vitest 测试）

## 做法

1. **路由与外壳接入（`ui/src/bus/route.ts`、`ui/src/shell/Shell.tsx`）**：
   - `Route` 增补 `{ page: 'knowledge'; topicId?: string }`，`Shell.tsx` 在 `route.page === 'knowledge'` 渲染 `<KnowledgePage initialTopicId={route.topicId} />`（保留统一顶部安全区，不重复渲染无用顶栏）。
   - `SettingsPage.tsx` 将原扁平 `s.terms` 区域替换为简洁的「知识库」卡片：展示已准备的主题数（来自 `Query::Topics`）与 `DECISIONS.md` #7 说明，点按钮 `onOpenKnowledge()` 进入 `{ page: 'knowledge' }`（`Knowledge.dc.html` 顶部带 `< 设置` 返回键）。

2. **新建 `ui/src/pages/KnowledgePage.tsx`（严格对照 `Knowledge.dc.html`）**：
   - 拆分为纯 props `KnowledgeView` 与薄容器 `KnowledgePage`（严守 `ui.sh`：`useSyncExternalStore` 与 IPC 绝不离开 `ui/src/bus/`）。
   - **纯函数与命名常量**：
     - `export const TOPIC_MAX_CHARS = 10_000;`（与核心一致）；
     - `export function topicBudgetPercent(sizeChars: number): number`（`0..=100` 钳制）；
     - `export type KnowledgeDraftAction` + 纯函数 `topicDraftStep`（管理添加/移除概况、词、译法、采纳虚线建议词，保持表单逻辑可单测）。
   - **左轨（`max-width: 220px`）**：
     - 展示 `Query::Topics` 列表：固定 `always` 主题置顶（名称空时显示本地化默认名 `t('knowledge.always_name')`）带词数徽标；其余自定义主题按次序展示；底部 `"+ 新主题"` 按钮打开新建表单或创建并切换至新主题。
   - **右编辑区（`Query::Topic { id }`）**：
     - 顶行：主题名（可编辑输入框，防 IME 组合键 `isComposing || keyCode === 229`）、固定主题显示 `<Tag>每场都用</Tag>`、篇幅进度条与 `{size_chars} / 10,000` 读数；非固定主题提供红底两步 `InlineConfirm` 删除按钮（固定主题不显示删除）。
     - **概况**：`domain` 字段 + 已有 `facts` 胶囊（`key` + `value` + 移除）+ 行内添加概况表单。
     - **背景**：`<textarea>` 编辑主题背景，失焦或点保存调用 `Command::SaveTopic`。
     - **词**：已有 `terms` 胶囊（展示 `said` 及各语 `written` 写法，点 `×` 移出）+ 行内添加词（怎么说 + 可选多语写法）+ 若 `suggested_terms.length > 0` 显示来源文件名（如 `讲义.md 里还有`）与虚线建议词按钮，点一下即采纳进 `terms` 并从 `suggested_terms` 移出（完全还原 `Knowledge.dc.html` 第 94–101 行）。
     - **译法**：`source → target` 胶囊 + 行内添加译法表单。
     - **底栏**：`"从文件里提"` 按钮（经 `bus/act.ts::pick()` 选文件并派发 `Command::ImportTopicFile { id, path }`）+ 右侧常驻隐私提示标签：`t('knowledge.send_notice')`（「用到它的录音，会把这一页随声音发给出字服务」，对照 `Knowledge.dc.html` 第 117 行与 `DECISIONS.md` #7）。

3. **首页主题选择器（`ui/src/pages/HomePage.tsx`，对照 `Main.dc.html` 第 72–76 行）**：
   - 在 `.setup` 的「字幕」与「声音」之间新增「主题」一行：
     - 展示 `state.setup.topics` 中每个已选主题的激活胶囊（按 `Query::Topics` 映射名称，点 `×` 从 `setup.topics` 移除并调用 `Command::Setup`）；
     - 末尾 `"+"` 按钮打开弹出菜单：列出未选的非固定主题供点选加入（最多 `MAX_SELECTED_TOPICS = 8`，到顶给出清晰提示），底部提供 `"管理知识库"` 入口直达 `{ page: 'knowledge' }`；
     - 若固定主题（`always`）非空，以低调提示说明「每场都用」词表会自动带上。

4. **阅读页与结果页：标题来源/候选弹窗、话题跳转与「记进知识库」（`ReaderPage.tsx`、`TranscriptList.tsx`、`ResultPage.tsx`，对照 `Titles.dc.html` + `Reader.dc.html`）**：
   - **标题字重与换标题弹窗（`Titles.dc.html`）**：
     - 根据 `row.title_by === 'Hand'` 切换 `.title`（手写粗体）与 `.title.auto`（机器常规字重）；
     - 标题右侧加「换一个标题」图标按钮（带 `aria-expanded`），生成中（`progress.title?.id === row.id`）显示柔和加载态；
     - 弹窗列出 `recording.title_candidates`（每项带来源标签：`文字稿` / `开头` / `活动` / `文件名` + 当前选中勾号），点击派发 `Command::PickTitle { id, title, by }`；弹窗底部提供 `"重新生成"` 按钮派发 `Command::GenerateTitle { id }`。
   - **话题时间胶囊（`Titles.dc.html` 第 72–76 行）**：
     - 在 `.meta` 下方展示 `.topics` 行（仅当 `row.tags.length > 0`），每枚胶囊显示 `{tag.label}` 与 `{clock(tag.at_ms)}`；
     - 点击某枚话题胶囊时，通过纯函数 `findLineIndexForTime(lines, atMs)` 定位最接近 `at_ms` 的行并滚动高亮（或再次点击取消高亮），同时不破坏虚拟列表 `VList` 的 I8 常量 DOM 上限！
   - **「+ 记进知识库」（`Reader.dc.html` 第 76 行）**：
     - 在 `TranscriptList` 中，对人工修改过的行（`said.via.text === 'hand'` 或任一译文为 `'hand'`）直接显式展示 `已改` 标签 + `"+ 记进知识库"` 快捷按钮，并在普通行的行操作中也可唤出；
     - 点击后在原处展开紧凑表单：预填文本（支持用户裁成专有名词或直接输入词与译法）+ 下拉选择目标主题（默认选当前录音绑定的首个主题，否则 `"always"` 固定主题），提交调用 `Command::RememberTerm { topic_id, said, written }`。
   - **`ResultPage.tsx`**：展示 `row.tags` 话题胶囊，并在 `progress.title?.id === row.id` 时以非阻断微提示显示 `step.titling`。

5. **全部录音页话题副标题与过滤（`ui/src/pages/LibraryPage.tsx`，对照 `Library.dc.html`）**：
   - 每行 `lrow` 在标题下展示 `row.tags.map(x => x.label).join(' · ')` 作为 `.lrow-s` 副标题（与 `Library.dc.html` 第 85 行一致）；
   - 支持点击行内话题或筛栏话题芯片切换 `filter.tag`（透传给 `Query::LibraryMonth` / `Query::LibraryRecent`），筛选栏在 `filter.tag` 激活时显示可点 `×` 清除的激活胶囊；
   - 搜索命中列表与 `CommandPalette.tsx` 均完整支持 `SearchHitKind === 'Tag'` 的标签文案展示。

6. **8 语字典、`mock.ts` 与样本数据隐私**：
   - 全部新增 UI 文案键同步写入全部 8 份 `ui/lang/*.json`（`en`、`zh-Hans`、`th`、`ja`、`ko`、`fr`、`de`、`es`），确保 `words.test.ts` 零缺键、零占位符错配；
   - `ui/dev/mock.ts` 使用虚构的读书会/社区样例（严禁出现任何真实人名、句柄或邮箱，遵守原则 6）。

## 显式与可观测

- **纯状态函数与命名常量**：`TOPIC_MAX_CHARS = 10_000`、`MAX_SELECTED_TOPICS = 8`、`topicBudgetPercent`、`topicDraftStep`、`findLineIndexForTime` 均为无副作用纯函数并配单元测试。
- **架构与安全守卫**：严守 `scripts/build/rules/ui.sh`——总线外零 `useSyncExternalStore` / `@tauri-apps/*` / `invoke(` / `new Channel`、零 `console.log`、零外网 `@import`、零不安全 HTML 注入属性（含注释内）；阅读页与全部录音页保持 `virtua` 虚拟化与 I8 DOM 节点上限（`scale_i8.test.tsx` 持续通过）。
- **状态显式可见**：10,000 字篇幅条、固定「每场都用」标签、`DECISIONS.md` #7 隐私说明、AI 标题生成中进度（`progress.title`）、手写粗体 vs 自动常规字重、删除两步红底确认均由数据直接驱动，无隐藏隐式状态。
- **IME 与焦点防护**：知识库名称/词条/概况编辑、阅读页记进知识库输入框均做 `isComposing || keyCode === 229` 防护，防止中日韩输入法选词误提交（`LESSONS.md` P3 经验）。

## 先写的测试

在 `ui/tests/knowledge.test.tsx` 与 `ui/tests/titles.test.tsx`（标 `// @vitest-environment jsdom`）中先写以下测试，再实现代码：

1. `KnowledgeView` 测试（`ui/tests/knowledge.test.tsx`）：
   - 展示固定「每场都用」主题且不提供删除按钮；切到自定义主题可展示 10,000 字篇幅条、概况/背景/词/译法与 `DECISIONS.md` #7 隐私提示；
   - 点击文件提取的虚线建议词胶囊将其采纳进 `terms` 并触发 `onSaveTopic`；
   - 点击「从文件里提」触发 `onImportFile`；
   - 删除自定义主题必须经过 `InlineConfirm` 两步确认才触发 `onRemoveTopic`。
2. `HomeView` 主题选择测试（`ui/tests/knowledge.test.tsx`）：
   - 打开「主题」弹窗选中一个主题，断言 `onSetup` 携带新 `setup.topics`；点击已选主题胶囊上的 `×` 将其移出。
3. `ReaderView` + `TranscriptList` 标题、话题与「记进知识库」测试（`ui/tests/titles.test.tsx`）：
   - `title_by === 'Ai'` 渲染 `.title.auto`，`title_by === 'Hand'` 渲染粗体 `.title`；
   - 打开「换一个标题」弹窗点选候选触发 `onPickTitle`，点「重新生成」触发 `onGenerateTitle`；
   - 点击话题时间胶囊触发跳转/高亮对应行；
   - 对手改行点击「+ 记进知识库」提交表单触发 `onRememberTerm`。
4. `LibraryView` 话题副标题与过滤测试（`ui/tests/titles.test.tsx`）：
   - 渲染行内 `tags` 副标题；点击话题筛选更新 `filter.tag` 并可一键清除。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `ui.sh`、`words.test.ts`、`scale_i8.test.tsx`、全部 Vitest 测试、Vite 3 入口构建与全部 Rust 测试）。
- `ui/package.json` 与 `ui/package-lock.json` 零新增依赖。

## 不做

- 本任务不改 Rust 核心逻辑与 SQLite schema（P6-01–P6-03 已完成）。
- 本任务不升级 `pending.sh` 与写收尾报告（归 P6-05）。
