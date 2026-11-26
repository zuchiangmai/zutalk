# P6-01：`talk/topic` 知识库模型、旧 `talk/term` 迁移与本机领回

## 目标

建成按主题组织的本机知识库事实模型 `talk/topic`（`topics.jsonl`）与纯函数规则模块 `core/src/knowledge/topic.rs`：含固定不可删的「每场都用」主题（`id = "always"`）、概况/背景/词/译法/来源文件/建议词完整字段、显式 10,000 字与条目预算、零新增 crate 的 UTF-8 文本/Markdown 文件提取，以及将既有 `talk/term` 无损迁移至固定主题并同步兼容 `record/adopt.rs` 本机记录领回（严守 `layers.sh` 零 `crate::knowledge` 引用）；接入 `Query::Topics` / `Query::Topic { id }`、`ViewKind::Knowledge`、`Command::{SaveTopic, RemoveTopic, ImportTopicFile, RememberTerm}` 与结构化事件 `topic.saved`，同步最小 TS 绑定与 8 语错误码，使全仓 `scripts/build/check.sh` 一次通过。

## 背景

- 计划依据：`docs/PLAN.md` §2.1、§2.5、§2.7、§5.1；`docs/build/phases/P6.md` §3.1、§3.2、§3.5；`docs/ui-2026-11-21/Knowledge.dc.html`。
- 涉及文件：
  - `core/src/record/shelf.rs`（新增 `TOPIC` 与 `topics.jsonl` 路径映射）
  - `core/src/record/adopt.rs`（同步领回 `talk/topic` 与合并 `"always"`，零 `crate::knowledge` 依赖）
  - `core/src/knowledge.rs`、`core/src/knowledge/topic.rs`（新建）、`core/src/knowledge/glossary.rs`（保留兼容）
  - `core/src/view/query.rs`（新增 `ViewKind::Knowledge`、`QuerySlot::Knowledge`、`Query::Topics`、`Query::Topic`、`QueryValue::{Topics, Topic}`）
  - `core/src/app/{proto.rs,hub.rs,work.rs}`（新增 4 条命令、主题缓存、自动迁移与 `DiscardLocal` 检查）
  - `docs/EVENTS.md`（登记 `topic.saved` 与新 `job` 种类）
  - `ui/src/bus/query.ts`、`ui/dev/mock.ts`、`ui/lang/*.json`（最小编译与 `words.test.ts` 同步）

## 做法

1. **记录类型与 `Shelf` 落盘（`core/src/record/shelf.rs`）**：
   - 新增 `pub const TOPIC: &str = "talk/topic"; // key: <topic_id> ("always" for pinned topic); kept apart in topics.jsonl`。
   - `Shelf::pack` 增加 `TOPIC => self.root.join("topics.jsonl"),`，保留 `TERM => self.root.join("terms.jsonl"),` 以向后读取与迁移。

2. **新建 `core/src/knowledge/topic.rs`（并在 `core/src/knowledge.rs` 导出 `pub mod topic;`）**：
   - 严格守住 `layers.sh`：`knowledge` 层只能用 `rt`、`live`、`record`（及 `serde`/`serde_json`/`ts_rs`/`getrandom`/`zutalk-log`）。
   - 定义 `P6.md` §3.2 规定的全部类型（均 `#[ts(export)]`）与命名常量：
     - `ALWAYS_TOPIC_ID = "always"`、`TOPIC_MAX_CHARS = 10_000`、`TOPIC_MAX_BACKGROUND_CHARS = 6_000`、`TOPIC_MAX_FACTS = 16`、`TOPIC_MAX_TERMS = 200`、`TOPIC_MAX_TRANSLATION_PAIRS = 100`、`TOPIC_MAX_SUGGESTED_TERMS = 40`、`TOPIC_IMPORT_MAX_BYTES = 512 * 1024`、`TOPIC_IMPORT_BACKGROUND_CHARS = 1_200`、`MAX_SELECTED_TOPICS = 8`。
     - `TopicFact { key, value }`、`TranslationPair { source, target }`、`Topic { id, name, domain, facts, background, terms, translation_terms, source_file, suggested_terms, pinned, updated_at }`、`TopicSummary { id, name, domain, pinned, terms_count, size_chars, updated_at }`、`ExtractedTopic { source_file, background, suggested_terms }`。
   - **纯规则函数**：
     - `pub fn size_chars(topic: &Topic) -> usize`：统计 `name`、`domain`、每条 `fact` 的 `key`/`value`、`background`、每条 `term` 的 `said` 与 `written` 值、每条 `translation_terms` 的 `source`/`target` 字符总数。
     - `pub fn normalize(topic: Topic, now: i64) -> Result<Topic, &'static str>`：
       - 去除空白；若 `id.trim().is_empty()` 用 `getrandom` 生成 32 位小写十六进制随机 id（拒绝非法字符或过长 id > 64 B）；
       - `pinned = (id == ALWAYS_TOPIC_ID)`（强制单点不变量：只有 `"always"` 是固定主题，其余主题不可伪造 `pinned = true`）；
       - 非固定主题要求 `name` 非空（否则返回 `Err("topic.empty")`）；固定主题 `name` 允许为空（由界面显示本地化默认名，或保留用户自定义名）；
       - `facts` 过滤空键值并去重截断至 `TOPIC_MAX_FACTS`；`terms` 按 `profile("").key(&said)` 去重合并 `written` 并过滤空串，截断至 `TOPIC_MAX_TERMS`；`translation_terms` 过滤空串去重截断至 `TOPIC_MAX_TRANSLATION_PAIRS`；`suggested_terms` 剔除已在 `terms` 中出现的词并去重截断至 `TOPIC_MAX_SUGGESTED_TERMS`；`source_file` 若非空仅保留文件基名（剥除 `/` 与 `\` 前缀，绝不落盘路径）；
       - 若 `background.chars().count() > TOPIC_MAX_BACKGROUND_CHARS` 或 `size_chars(&topic) > TOPIC_MAX_CHARS`，返回 `Err("topic.limit")`；
       - 设置 `updated_at = now`。
     - `pub fn extract_from_text(source_file: &str, content: &str) -> ExtractedTopic`（纯函数，零 IO）：
       - 剥离 Markdown 标题/粗体/斜体/行内代码/链接包装，生成最多 `TOPIC_IMPORT_BACKGROUND_CHARS` 字符的干净背景摘要；
       - 提取候选词（`《…》`、`「…」`、`『…』`、Markdown 标题短语、`A: B` / `A · B` / `A -> B` 术语行、重复出现的专有英文大写词组），按 `profile("").key(...)` 去重并封顶 `TOPIC_MAX_SUGGESTED_TERMS`，**绝不自动写入 `terms`**。
     - `pub fn extract_from_file(path: &Path) -> Result<ExtractedTopic, &'static str>`：
       - 校验扩展名（忽略大小写仅收 `txt` / `md` / `markdown` / `csv` / `tsv`，其余返回 `Err("topic.file.format")`）；
       - 校验文件大小（`metadata.len() > TOPIC_IMPORT_MAX_BYTES as u64` 返回 `Err("topic.file.big")`），并用 `std::io::Read::take(TOPIC_IMPORT_MAX_BYTES as u64 + 1)` 防止竞态超大读取；
       - 校验 UTF-8（无效返回 `Err("topic.file.format")`），只取 `path.file_name()` 基名传给 `extract_from_text`。
   - **书架读写与无损迁移**：
     - `pub fn ensure_migrated(shelf: &Shelf, now: i64) -> Result<bool, &'static str>`：若 `topics.jsonl` 尚无 `ALWAYS_TOPIC_ID` 且 `terms.jsonl` 非空，把旧 `glossary::read(shelf)` 全部迁入 `id = "always", pinned = true` 的 `talk/topic` 落盘，返回 `Ok(true)`；否则无副作用返回 `Ok(false)`。
     - `pub fn list(shelf: &Shelf) -> Result<Vec<Topic>, &'static str>`：读取全部有效 `talk/topic`，确保 `ALWAYS_TOPIC_ID` 永远排在首位（若盘上尚无则合并旧 `terms.jsonl` 合成默认 `Topic { id: "always", pinned: true, .. }`），其余主题按 `updated_at` 降序再按 `id` 升序排列。
     - `pub fn summaries(shelf: &Shelf) -> Result<Vec<TopicSummary>, &'static str>` 与 `pub fn get(shelf: &Shelf, id: &str) -> Result<Option<Topic>, &'static str>`。
     - `pub fn save(shelf: &Shelf, topic: Topic, now: i64) -> Result<Topic, &'static str>`：执行 `normalize` 后写入 `shelf.put("", TOPIC, &[(saved.id.clone(), Some(saved.clone()))], now)`。
     - `pub fn remove(shelf: &Shelf, id: &str, now: i64) -> Result<(), &'static str>`：若 `id == ALWAYS_TOPIC_ID` 返回 `Err("topic.pinned")`；若不存在返回 `Err("topic.missing")`；写 `None` 墓碑删除。
     - `pub fn remember_term(shelf: &Shelf, topic_id: &str, said: &str, written: BTreeMap<String, String>, now: i64) -> Result<Topic, &'static str>`：把词合并入目标主题（`topic_id` 为空时默认 `ALWAYS_TOPIC_ID`），若该词在 `suggested_terms` 中则顺带移出，保存并返回更新后的主题。
     - `pub fn put_always_term(shelf: &Shelf, said: &str, written: Option<BTreeMap<String, String>>, now: i64) -> Result<(), &'static str>`：供既有 `Command::Term` 同步维护 `always` 主题（同时保留旧 `glossary::put` 兼容）。

3. **领回 `talk/topic` 且零跨层依赖（`core/src/record/adopt.rs`）**：
   - **严禁 `use crate::knowledge`**（守住 `layers.sh`）；仅新增 `use crate::record::shelf::TOPIC;`。
   - `Adopted` 与 `AdoptSummary` 增补 `#[serde(default)] pub topics: u32`。
   - `adopt::run` 读取 `local_topics = local.kept::<serde_json::Value>(TOPIC)`，更新 `has_local_data = !recs.is_empty() || !local_terms.is_empty() || !local_topics.is_empty()`：
     - 非 `"always"` key：若本人书架尚无该 key，则重签写入；
     - `"always"` key：若本人书架尚无则直接写入；若双方均有，则用纯 `serde_json::Value` 辅助函数合并本机 `"terms"`（按 `said` 小写去重）、`"facts"`（按 `(key, value)` 去重）与 `"translation_terms"`（按 `(source, target)` 去重）中缺失的条目，避免覆盖本人已有词表。
   - 同步更新 `core/src/app/work.rs` 的 `Job::DiscardLocal` 检查 `has_recs || has_terms || has_topics`。

4. **视图与命令接入（`core/src/view/query.rs`、`core/src/app/{proto,hub,work}.rs`）**：
   - `ViewKind` 新增 `Knowledge`（更新 `ViewKind::ALL` 为 8 项与 `code()` 返回 `"knowledge"`）；`QuerySlot` 新增 `Knowledge`；`Query` 新增 `Topics`（`slot = Other`, `code = "topics"`）与 `Topic { id: String }`（`slot = Knowledge`, `code = "topic"`）；`QueryValue` 新增 `Topics(Vec<TopicSummary>)` 与 `Topic(Option<Topic>)`；`Query::Glossary` 从 `topic::get(shelf, ALWAYS_TOPIC_ID)` 读取词表（100% 兼容既有 `core/tests/glossary.rs`）。
   - `Command` 新增（全部 `needs_person = false`，更新 `Debug`、`kind()`、`needs_person()`）：
     - `SaveTopic { topic: Topic }`
     - `RemoveTopic { id: String }`
     - `ImportTopicFile { id: String, path: String }`
     - `RememberTerm { topic_id: String, said: String, written: BTreeMap<String, String> }`
   - `Work` 新增缓存 `topics: Arc<Mutex<Vec<Topic>>>`（取代/同步维护原 `terms` 缓存），在 `run_sign` 打开书架后调 `topic::ensure_migrated` 并刷新缓存；`Job::{SaveTopic, RemoveTopic, ImportTopicFile, RememberTerm}` 在 `desk` 上执行、更新缓存、失效 `ViewKind::Knowledge`（若修改了 `"always"` 同时失效 `ViewKind::Glossary`）；`RememberTerm` 成功后发提示 `"topic.remembered"`。
   - **重要兼容细节**：`RemoveTopic` 成功后同步通知（或由 hub 顺带）从 `Setup.topics` 剔除被删 id（在 P6-02 加入 `Setup.topics` 后无缝衔接）。

5. **最小 TS 绑定与 8 语词条同步（保证本任务结束后 `check.sh` 全绿）**：
   - 运行 `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib` 生成 `core/bindings/{Topic,TopicFact,TranslationPair,TopicSummary}.ts` 及更新后的 `Command.ts` / `Query.ts` / `QueryValue.ts` / `ViewKind.ts`。
   - 在 `ui/src/bus/query.ts` 补齐 `Topics` / `Topic` 与 `Knowledge` 的类型和 `switch` 分支；在 `ui/dev/mock.ts` 补齐最小 `Topics` / `Topic` 与 4 条命令的 mock 分支；在全部 8 份 `ui/lang/*.json` 补齐本任务新增的 `code.topic.{empty,pinned,missing,limit,file.format,file.big,file.read,remembered}`，确保 `words.test.ts` 与 `npm run check` 全绿。

## 显式与可观测

- **不变量与纯函数**：`topic::normalize` 强守 `pinned == (id == "always")` 与 `size_chars <= TOPIC_MAX_CHARS`；`topic::extract_from_text` 纯值无 IO，只产建议词不自动改词表；`adopt` 合并逻辑纯函数操作 `serde_json::Value`。
- **具名带单位常量**：`TOPIC_MAX_CHARS`、`TOPIC_MAX_BACKGROUND_CHARS`、`TOPIC_MAX_FACTS`、`TOPIC_MAX_TERMS`、`TOPIC_MAX_TRANSLATION_PAIRS`、`TOPIC_MAX_SUGGESTED_TERMS`、`TOPIC_IMPORT_MAX_BYTES`、`TOPIC_IMPORT_BACKGROUND_CHARS`、`MAX_SELECTED_TOPICS` 全部带单位与理由注释（过 `numbers.sh`）。
- **队列与任务**：全部知识库磁盘读写与文件读取走既有有界 64 槽顺序 `desk` 队列（`Full::Reject`，满时回 `"busy"`），不新增无界队列、不新增裸任务（过 `bounded.sh` / `spawn.sh` / `hub.sh`）。
- **零新 `Stats` 行与结构化事件**：不新增任何 `Stats` 行；在 `docs/EVENTS.md` 登记新事件 `topic.saved`（字段：`op`、`pinned`、`terms`、`code`）并在 `job.start`/`job.done` 的 `kind` 列补充 `topic.save`、`topic.remove`、`topic.import`、`topic.remember`；**严禁记录主题 id、名称、领域、背景、词、译法或文件名/路径**（过 `events.sh`）。
- **故障可见性测试**：验证删固定主题返回 `"topic.pinned"` 并记 `topic.saved(code = "topic.pinned")`、超 10,000 字返回 `"topic.limit"`、超大（> 512 KiB）与非文本（`.pdf` / 非 UTF-8）文件返回 `"topic.file.big"` / `"topic.file.format"` 并有对应事件且日志不含任何主题词或文件名。

## 先写的测试

在 `core/tests/topic.rs`（新建）与 `core/tests/adopt.rs` 中先写以下失败测试，再实现代码使其通过：

1. `always_topic_is_always_present_cannot_be_removed_and_migrates_legacy_terms`：写入旧 `talk/term` 记录后调用 `ensure_migrated`，断言全部词与多语 `written` 无损迁入 `"always"` 主题；尝试删除 `"always"` 返回 `"topic.pinned"`。
2. `topic_normalize_enforces_budgets_deduplication_and_basename_only`：验证超 10,000 字被拒 `"topic.limit"`、空名非固定主题被拒 `"topic.empty"`、同名术语合并去重、`source_file` 剥除完整目录仅留基名。
3. `extract_from_file_handles_utf8_markdown_and_rejects_oversized_or_binary`：对合法 `.md`/`.txt` 提取背景摘要与去重建议词（且不自动进入 `terms`）；对 > 512 KiB 文件返回 `"topic.file.big"`；对 `.pdf` 或无效 UTF-8 返回 `"topic.file.format"`。
4. `adopt_resigns_local_topics_and_merges_always_without_losing_person_terms`（`core/tests/adopt.rs`）：本机 `did:key` 书架建有自定义 `talk/topic` 与 `"always"` 词条，登录 `did:zu` 后全部领回且与本人既有 `"always"` 词条无损合并；第二位 DID 仍被 `OtherPerson` 拒绝。
5. `topic_commands_and_queries_work_without_sign_in_and_emit_safe_events`：未登录本地身份下经 `App` 跑通 `SaveTopic` → `Query::Topics` / `Query::Topic` → `ImportTopicFile` → `RememberTerm` → `RemoveTopic`，断言 `topic.saved` 事件发出且事件捕获中绝不含主题名、词、背景或文件名。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `layers.sh` 零欠账、`events.sh`、`numbers.sh`、`words.test.ts`、`ui.sh`、全部 Rust 与 UI 测试）。
- 既有 `core/tests/glossary.rs`、`core/tests/adopt.rs`、`core/tests/view.rs` 零退化全通过。
- `Cargo.lock` 与 `ui/package-lock.json` 零新增外部依赖。

## 不做

- 本任务不改 `live/lane.rs` 或 `ai/write.rs` 的上下文注入（归 P6-02）。
- 本任务不改 `About` 标题/话题字段、不写 `ai/titles.rs`、不升级 SQLite schema（归 P6-03）。
- 本任务不画 `KnowledgePage` 等界面组件（归 P6-04）。
