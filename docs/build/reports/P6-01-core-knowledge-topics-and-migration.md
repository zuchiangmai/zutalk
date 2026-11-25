状态：完成

# P6-01：`talk/topic` 知识库模型、旧 `talk/term` 迁移与本机领回

## 改动与文件

1. **记录类型与 `Shelf` 落盘**（`core/src/record/shelf.rs`）：
   - 新增常量 `TOPIC = "talk/topic"`，`Shelf::pack` 将其映射至 `topics.jsonl`，保留 `TERM`（`terms.jsonl`）向后读取与迁移。
2. **本机主题知识库与文件提取**（`core/src/knowledge.rs`、`core/src/knowledge/topic.rs`）：
   - 导出 `knowledge::topic`，定义 `ALWAYS_TOPIC_ID = "always"` 与全部带单位命名预算常量（`TOPIC_MAX_CHARS = 10_000`、`TOPIC_MAX_BACKGROUND_CHARS = 6_000`、`TOPIC_MAX_FACTS = 16`、`TOPIC_MAX_TERMS = 200`、`TOPIC_MAX_TRANSLATION_PAIRS = 100`、`TOPIC_MAX_SUGGESTED_TERMS = 40`、`TOPIC_IMPORT_MAX_BYTES = 512 * 1024`、`TOPIC_IMPORT_BACKGROUND_CHARS = 1_200`、`MAX_SELECTED_TOPICS = 8`、`TOPIC_ID_MAX_BYTES = 64`）。
   - 定义 `#[ts(export)]` 结构 `TopicFact`、`TranslationPair`、`Topic`、`TopicSummary` 及 `ExtractedTopic`。
   - 实现纯函数 `size_chars`、`normalize`（强制 `pinned == (id == "always")`、剥除完整路径仅留文件基名、去重合并 `written` 并守住 10,000 字上限）与纯文本提取器 `extract_from_text`，以及零新增 crate 的有界 UTF-8 文件读取 `extract_from_file`（仅收 `.txt`/`.md`/`.markdown`/`.csv`/`.tsv`，硬限 `<= 512 KiB`，提取候选词仅写入 `suggested_terms`，绝不自动加入 `terms`）。
   - 实现 `ensure_migrated`（旧 `talk/term` 无损迁入 `"always"` 主题）、`list`、`summaries`、`get`、`save`、`remove`、`import_file`、`remember_term` 与 `put_always_term`。
3. **本机记录领回兼容 `talk/topic` 且守住 `layers.sh`**（`core/src/record/adopt.rs`、`core/src/app/work.rs`）：
   - 零 `use crate::knowledge`，在纯 `serde_json::Value` 层重签领回自定义 `talk/topic` 并无损合并双方 `"always"` 主题的 `terms`/`facts`/`translation_terms`。
   - `Adopted` 与 `AdoptSummary` 增补 `#[serde(default)] pub topics: u32`；`Job::DiscardLocal` 同步纳入 `has_topics` 检查。
4. **视图、命令、结构化事件与最小界面同步**（`core/src/view/query.rs`、`core/src/app/{proto,hub,work}.rs`、`docs/EVENTS.md`、`core/bindings/*.ts`、`ui/src/bus/query.ts`、`ui/dev/mock.ts`、`ui/lang/*.json`）：
   - 新增 `ViewKind::Knowledge`（`ALL` 扩至 8 项）、`QuerySlot::Knowledge`、`Query::{Topics, Topic}`、`QueryValue::{Topics, Topic}`，`Query::Glossary` 从 `"always"` 主题读取词表并排序保持 100% 向后兼容。
   - 新增 4 条免登录命令 `Command::{SaveTopic, RemoveTopic, ImportTopicFile, RememberTerm}`（更新脱敏 `Debug`、`kind()`、`needs_person() = false`），接入顺序 `desk` 队列与主题缓存并在 `run_sign` 自动调用 `ensure_migrated`。
   - 零新增 `Stats` 行；在 `docs/EVENTS.md` 登记 `topic.saved` 与 `topic.save`/`topic.remove`/`topic.import`/`topic.remember` 四种 `job` 种类。
   - 重新生成 `core/bindings/*.ts`，同步 `ui/src/bus/query.ts`、`ui/dev/mock.ts` 及 8 份 `ui/lang/*.json` 中的 `code.topic.*` 词条。

## 证据

- **新增测试（5 项全部通过）**：
  - `core/tests/topic.rs`（4/4 通过）：
    - `always_topic_is_always_present_cannot_be_removed_and_migrates_legacy_terms`
    - `topic_normalize_enforces_budgets_deduplication_and_basename_only`
    - `extract_from_file_handles_utf8_markdown_and_rejects_oversized_or_binary`
    - `topic_commands_and_queries_work_without_sign_in_and_emit_safe_events`
  - `core/tests/adopt.rs`（新增 1 项 + 既有非 TCP 5 项共 6/6 通过）：
    - `adopt_resigns_local_topics_and_merges_always_without_losing_person_terms`
- **既有回归与门禁（沙箱内全绿）**：
  - `cargo fmt --all` 与 `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 零告警通过。
  - 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh` 零欠账、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）全绿。
  - `cargo test -p zutalk-core --lib export_bindings`：52/52 通过。
  - `core/tests/glossary.rs`（非 TCP 2/2 通过）、`core/tests/view.rs`（11/11 通过）、`core/tests/liveness.rs::every_event_in_the_catalogue_can_be_told_apart`（1/1 通过）。
  - `cd ui && npm run check && npm test && npm run build`：14 个测试文件 109/109 通过（含 `words.test.ts` 12/12），构建通过。
  - `Cargo.lock` 与 `ui/package-lock.json` 无任何改动。

## 遗留 / 需注意

- 沙箱禁止本地 TCP 监听，含 `wiremock`/`TcpListener` 的既有测试将在批次收尾后由协调者在沙箱外执行完整 `scripts/build/check.sh` 复核。
- 本任务按计划不涉及双路上下文注入（归 P6-02）、AI 标题/话题与索引 v2（归 P6-03）及知识库 UI 页面（归 P6-04）。
