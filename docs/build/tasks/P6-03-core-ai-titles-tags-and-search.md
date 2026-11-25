# P6-03：AI 标题与话题（`ai/titles.rs`）、第 4 槽 `Progress.title` 与 SQLite 话题索引搜索

## 目标

新增后台 AI 标题与话题生成模块 `core/src/ai/titles.rs`，引入 `Progress` 第 4 槽 `title: Option<Writing>` 与独立单并发信号量（绝不堵住录音、精修或交城），在 `talk/recording` 持久化 `title_by`、`tags`（1–4 条含时间戳的话题词）与 `title_candidates`，以显式状态规则与提交前复核保证**用户手改标题（`TitleBy::Hand`）永远优先**且尊重 `Command::Remove` 与钥匙/额度可用性；同步将本机 SQLite 投影索引升级至 `INDEX_SCHEMA_VERSION = 2`（自动重建），支持 `LibraryRow.tags`、`LibraryFilter.tag` 过滤与 FTS5 `SearchHitKind::Tag` 搜索，新增 `Command::{GenerateTitle, PickTitle}` 与结构化事件 `titles.done`，全仓门禁全绿。

## 背景

- 计划依据：`docs/PLAN.md` §2.1、§2.3、§2.5、§3（标题与话题画板）、§5.1；`docs/build/DECISIONS.md` 第 4 条；`docs/build/phases/P6.md` §3.2、§3.4、§3.5；`docs/ui-2026-11-21/{Titles,Library}.dc.html`。
- 涉及文件：
  - `core/src/record/shelf.rs`（新增 `TitleBy`、`ContentTag`、`TitleCandidate` 并补齐 `About` 字段；更新 `Source::File` 与 `Change::Title` 的 `title_by` 赋值）
  - `core/src/ai.rs`、`core/src/ai/titles.rs`（新建）
  - `core/src/view/index.rs`（升级 `INDEX_SCHEMA_VERSION = 2`，增补 `title_by`/`tags_json`/`topics_json`、`SearchHitKind::Tag`、`LibraryRow.{title_by, tags, topics}`、`LibraryFilter.tag`）
  - `core/src/view/query.rs`（`RecordingView` 补齐 `topics` 与 `title_candidates`）
  - `core/src/app/{proto.rs,hub.rs,work.rs}`（`Progress.title`、独立 `TITLE_AT_ONCE_JOBS = 1` 信号量、`Command::{GenerateTitle, PickTitle}`、自动与手动触发、写前核对与 `titles.done`）
  - `docs/EVENTS.md`（登记 `titles.done` 与 `job` 种类 `titles`、`title.pick`）
  - `ui/src/bus/channel.ts`、`ui/src/shell/{Shell,CommandPalette}.tsx`、`ui/src/pages/LibraryPage.tsx`、`ui/dev/mock.ts`、`ui/tests/words.test.ts`、`ui/lang/*.json`（最小绑定与 8 语词条同步，确保本步 `check.sh` 全绿）

## 做法

1. **记录元数据扩展（`core/src/record/shelf.rs`）**：
   - 定义 `TitleBy`（`None` / `Hand` / `Event` / `File` / `Ai` / `Opening`）、`ContentTag { label: String, at_ms: u32 }`、`TitleCandidate { by: TitleBy, text: String }`，均加 `#[ts(export)]`。
   - `About` 追加带 `#[serde(default)]` 的 `pub tags: Vec<ContentTag>`、`pub title_by: TitleBy`、`pub title_candidates: Vec<TitleCandidate>`（向后兼容全部既有录音 pack）。
   - `Hub::start()` 导入音频文件（`Source::File`）时设 `title_by = TitleBy::File`；主持活动（`self.state.host.event()` 非空且非文件导入）时将 `event.title` 保留为首个事件候选（不覆盖已有测试对空 `title` 的预期，或仅在 `build_candidates` 中可选展示）；`work.rs::apply` 处理 `Change::Title { text }` 时设 `title_by = if text.trim().is_empty() { TitleBy::None } else { TitleBy::Hand }`。

2. **新建 `core/src/ai/titles.rs`（并在 `core/src/ai.rs` 导出 `pub mod titles;`）**：
   - 严格守住 `layers.sh`（`ai` 可用 `rt`/`capture`/`live`/`id`/`record`/`knowledge`），零新增 crate。
   - 定义 `P6.md` §3.4 规定的全部带单位命名常量：`TITLES_SAMPLE_HEAD_LINES = 40`、`TITLES_SAMPLE_MID_LINES = 20`、`TITLES_SAMPLE_TAIL_LINES = 20`、`TITLES_MAX_INPUT_CHARS = 6_000`、`TITLES_MAX_TITLE_CHARS = 80`、`TITLES_MAX_CANDIDATES = 3`、`TITLES_MAX_TAGS = 4`、`TITLES_MAX_TAG_CHARS = 24`、`TITLES_TIMEOUT_MS = 30_000`。
   - **纯决策状态机**：
     ```rust
     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
     pub enum TitleAction {
         Generate { overwrite_title: bool },
         Skip(&'static str),
     }
     pub fn title_plan(
         has_lines: bool,
         has_openrouter_key: bool,
         title_by: TitleBy,
         title_empty: bool,
         tags_empty: bool,
         manual: bool,
     ) -> TitleAction
     ```
     按 `P6.md` §3.4 实现全组合决策表：无行→`Skip("titles.empty")`；无 OpenRouter 钥匙→`Skip("keys.none")`；自动触发且手写标题与话题皆已存在→`Skip("titles.manual")`；其余 `Generate { overwrite_title }` 且 **`TitleBy::Hand`、非空 `TitleBy::File`、非空 `TitleBy::Event` 绝不设 `overwrite_title = true`**。
   - **`O(1)` 头中尾采样（I8）**：
     - `pub fn sample_transcript(lines: &[&Line]) -> String`：短录音按顺序取全行；长录音取前 40 + 中段 20 + 末 20 行，格式 `[mm:ss] speaker: text`，严格按 UTF-8 字符边界封顶 `TITLES_MAX_INPUT_CHARS`（6,000）。
   - **OpenRouter 严格 JSON Schema 请求与清洗**：
     - `pub fn request(model: &write::Model, spoken: &[String], topic_hint: &str, excerpt: &str) -> Value`：复用 `write::MODELS`、强带 `provider: { zdr: true, data_collection: "deny", require_parameters: true, only: model.only }`、`reasoning` 与严格 JSON Schema `{ titles: string[], tags: { label: string, at_ms: integer }[] }`；要求使用录音主要语言产出简洁像人写的标题与 1–4 个短话题词。
     - `pub fn parse_reply(reply: &Value, max_t_ms: u32) -> Result<GeneratedMeta, End>`：检查 HTTP 200 内嵌 `error` / `finish_reason == "error"`（`LESSONS.md`）；清洗标题（去首尾空白/引号/换行、截断 `TITLES_MAX_TITLE_CHARS`、去重、封顶 `TITLES_MAX_CANDIDATES`）与话题词（去空白、截断 `TITLES_MAX_TAG_CHARS`、按 `profile("").key` 去重、`at_ms.min(max_t_ms)`、封顶 `TITLES_MAX_TAGS`）。
     - `pub fn build_candidates(ai_titles: &[String], first_line: Option<&str>, file_or_event: Option<(TitleBy, &str)>) -> Vec<TitleCandidate>`：组合 AI 候选 + 首句 `TitleBy::Opening`（截断至 `TITLES_MAX_TITLE_CHARS`）+ 可选文件/活动标题，按文本去重封顶 4 条（对照 `Titles.dc.html`）。
     - `pub async fn generate(http: &reqwest::Client, base: &str, keys: &impl Keys, input: &TitleInput) -> Result<GeneratedMeta, End>`：按 `write::MODELS` 顺序请求，每次调用设 `Duration::from_millis(TITLES_TIMEOUT_MS)` 超时，遇 `401/403` 调用 `keys.refused(Service::OpenRouter)`；**绝不记录任何提示词、标题、候选或话题**。

3. **第 4 槽 `Progress.title` 与不阻塞调度（`core/src/app/{proto,hub,work}.rs`）**：
   - `Progress` 增补 `pub title: Option<Writing>`（`app/src/pump.rs` 与 `Outlet.progress` 无需改逻辑即自动透传）。
   - `Command` 新增（`needs_person = false`）：
     - `GenerateTitle { id: String }`（录音进行中拒以 `"recording.under_way"`）
     - `PickTitle { id: String, title: String, by: TitleBy }`（允许从弹窗选择 `Ai`/`Opening`/`Event`/`File` 或 `Hand` 候选，同步写入 `about.title` 与 `about.title_by` 并更新 SQLite 与失效 `Library`/`Search`/`Recording`）
   - `Work` 新增独立信号量 `title_bg: Arc<Semaphore>`（`pub const TITLE_AT_ONCE_JOBS: usize = 1`），**绝不与 `self.bg`（精修/交城/搬运）共用许可**，另维护 `titling: Arc<Mutex<BTreeSet<String>>>` 防止同一录音并发重复跑标题。
   - 自动触发点（均为最佳努力后台任务，失败仅记日志不弹窗打扰）：
     1. 录音封盘后若精修处于 `Owed::Waits` / `Owed::No`（例如无 Soniox 钥匙或草稿留存），在草稿有行且有 OpenRouter 钥匙时触发 `spawn_titles(id, false)`；
     2. 整场精修成功（`Refined`）后立即用最新高准确度 `take` 触发 `spawn_titles(id, false)`；
     3. 整场精修终态失败（含额度键 `keys.async_refused`）后，若草稿有行且有 OpenRouter 钥匙，触发 `spawn_titles(id, false)`（让额度用户也能自动获得标题与话题！）。
   - **提交前二次核对（走 `desk` 顺序 `Job::ApplyTitles`）**：
     - 重读 `shelf.about(&id)`：若已删除（`None`）直接丢弃并发 `titles.done(rec, code = "gone", applied = false, tags = 0, ms)`；
     - 若最新 `about.title_by == TitleBy::Hand && !about.title.trim().is_empty()`：绝不改写 `about.title` 与 `about.title_by`，只落盘 `tags`（若非空）与 `title_candidates`，`applied = false`；
     - 否则写入首条 AI 标题（`title_by = TitleBy::Ai`）、`tags` 与 `title_candidates`，`applied = true`；
     - 同步调用 `idx.upsert_recording(&shelf, &tapes, &id)` 并发 `Invalidate`（`Library`、`Search`、`Recording(id)`），最后清 `Progress.title` 槽。

4. **SQLite 索引升级至 v2 与视图扩展（`core/src/view/{index,query}.rs`）**：
   - `INDEX_SCHEMA_VERSION` 从 `1` 升至 `2`（老库自动走 `IndexHealth::VersionMismatch -> Rebuild("version")` 重建）。
   - `recordings` 表新增 `title_by TEXT NOT NULL`、`tags_json TEXT NOT NULL`、`topics_json TEXT NOT NULL`；`upsert_recording_tx` 写入三列，并对 `rec.about.tags` 中每个非空 `tag.label` 往 `search_docs` 插入一条 `kind = "tag"`（使用 `profile("").key(&tag.label)` 归一化，保证 FTS5 trigram 与 1–2 字回退均可命中）。
   - `SearchHitKind` 新增 `Tag`；`LibraryRow` 新增 `title_by: TitleBy`、`tags: Vec<ContentTag>`、`topics: Vec<String>`；`LibraryFilter` 新增 `#[serde(default)] pub tag: Option<String>`（用 `EXISTS (SELECT 1 FROM json_each(recordings.tags_json) WHERE json_extract(json_each.value, '$.label') = ?)` 过滤）；`RecordingView` 新增 `topics: Vec<String>`、`title_candidates: Vec<TitleCandidate>`。

5. **最小 TS 绑定与 8 语同步**：
   - 重生成 `core/bindings/`；同步 `ui/src/bus/channel.ts`（`EMPTY_PROGRESS` 加 `title: null`）、`ui/src/shell/Shell.tsx` & `ui/src/pages/LibraryPage.tsx`（`EMPTY_FILTER` 加 `tag: null`）、`CommandPalette.tsx` & `LibraryPage.tsx`（处理 `SearchHitKind === 'Tag'`）、`ui/dev/mock.ts`、UI 测试夹具、`ui/tests/words.test.ts`（`STEPS` 增加 `'titling'`）与 8 份 `ui/lang/*.json`（补 `step.titling`、`code.titles.empty`、`code.titles.manual`）。

## 显式与可观测

- **状态与纯函数**：`TitleBy` 枚举 + `TitleAction` + `title_plan` 完整转换表测试；`sample_transcript`、`parse_reply`、`build_candidates` 均为纯值函数。
- **具名带单位常量**：`TITLES_SAMPLE_HEAD_LINES`、`TITLES_SAMPLE_MID_LINES`、`TITLES_SAMPLE_TAIL_LINES`、`TITLES_MAX_INPUT_CHARS`、`TITLES_MAX_TITLE_CHARS`、`TITLES_MAX_CANDIDATES`、`TITLES_MAX_TAGS`、`TITLES_MAX_TAG_CHARS`、`TITLES_TIMEOUT_MS`、`TITLE_AT_ONCE_JOBS` 全带单位与理由注释（过 `numbers.sh`）。
- **监督与队列**：`job.titles` 挂在 `work` 监督树（`self.tree.spawn("job.titles", ...)`），独立单并发信号量不占 `Work.bg`；结果经 `desk` 顺序写盘与同步 SQLite，零无界队列（过 `spawn.sh` / `bounded.sh`）。
- **零新 `Stats` 行与结构化事件**：不新增任何 `Stats` 行；在 `docs/EVENTS.md` 登记 `titles.done`（`rec`、`code`、`applied`、`tags`、`ms`）并在 `job.start`/`job.done` 的 `kind` 列补充 `titles`、`title.pick`；严禁记录任何提示词、标题、候选或话题（过 `events.sh`）。
- **故障可见性测试**：验证无 OpenRouter 钥匙静默跳过、OpenRouter 返回 HTTP 200 错误正文按故障处理并清进度槽、标题进行中被手改标题或删除录音时不覆写/不复活并产出准确 `titles.done` 字段。

## 先写的测试

在 `core/tests/titles.rs`（新建）与 `core/tests/view.rs` 中先写以下失败测试，再实现代码：

1. `title_plan_covers_every_provenance_and_key_combination`：遍历 `TitleBy` 全部 6 变体 × `manual` × `has_lines` × `has_openrouter_key` × `title_empty` × `tags_empty`，断言手写标题永不被设 `overwrite_title = true`。
2. `sample_transcript_bounds_four_hour_recording_in_o1_chars`：构造 2,560 行（4 小时）转写，断言采样输出包含头、中、尾关键行且总字符数 `<= TITLES_MAX_INPUT_CHARS`（6,000）。
3. `parse_reply_rejects_http_200_error_body_and_sanitizes_titles_and_tags`：验证 OpenRouter 200 内嵌 `error` / `finish_reason: "error"` 返回重试错误；合法响应正确裁剪超长标题与超过 4 条的话题词并钳制 `at_ms`。
4. `background_titling_never_blocks_refining_protects_hand_title_and_respects_remove`：
   - 场景 A：OpenRouter 标题请求故意挂起期间，验证 `Query::Topics` 与录音/精修均能正常推进、不被标题信号量卡死；
   - 场景 B：标题请求在途时发送 `Command::Change { Title }` 手改标题，待 OpenRouter 返回后断言用户手写标题与 `TitleBy::Hand` 完好保留，而 `tags` 与 `title_candidates` 已入库，`titles.done(applied = false)` 可见；
   - 场景 C：标题请求在途时发送 `Command::Remove`，待返回后断言录音未复活且 `titles.done(code = "gone")` 可见。
5. `index_v2_rebuilds_from_v1_and_searches_and_filters_tags`（`core/tests/view.rs`）：旧 v1 SQLite 打开自动重建到 v2；写入带中文 2 字话题（如 `"押金"`）与 ≥ 3 字话题（如 `"读书会讨论"`）的录音，验证 `Query::Search` 命中 `SearchHitKind::Tag` 且 `LibraryFilter { tag: Some("押金"), .. }` 精确过滤。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `events.sh`、`numbers.sh`、`layers.sh`、`words.test.ts`）。
- 验证 `Progress.title` 在生成期间经 `Outlet.progress` 推送并在结束后清为 `None`。
- `Cargo.lock` 与 `ui/package-lock.json` 零新增外部依赖。

## 不做

- 本任务不画 `KnowledgePage`、换标题弹窗、话题胶囊或阅读页「记进知识库」UI（归 P6-04）。
- 本任务不升 `pending.sh` 与写收尾报告（归 P6-05）。
