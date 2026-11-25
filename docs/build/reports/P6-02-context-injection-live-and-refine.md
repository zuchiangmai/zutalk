状态：完成

# P6-02：现场字幕与整场精修双路主题上下文注入（`DECISIONS.md` #7）

## 改动与文件

1. **主题上下文预算、`TopicContext` 与纯装配/投影函数**（`core/src/knowledge/topic.rs`）：
   - 新增带单位命名预算常量：`CONTEXT_MAX_GENERAL_ITEMS = 24`、`CONTEXT_MAX_TEXT_CHARS = 8_000`、`CONTEXT_MAX_TERMS = 300`、`CONTEXT_MAX_TRANSLATION_TERMS = 300`、`CONTEXT_PROMPT_BACKGROUND_CHARS = 2_400`。
   - 新增 `TopicContext` 并手写脱敏 `fmt::Debug`（只打印 `topic_count`、`general_len`、`text_chars`、`terms_len`、`pairs_len`，绝不输出任何词或背景文本）。
   - 新增纯函数 `normalize_selected_topics`（剔除 `"always"`/空白/非法或超 64 字节项、去重并封顶 `MAX_SELECTED_TOPICS = 8`）、`assemble_context`（只收录固定 `"always"` 主题 + 本场选中主题，未选非固定主题绝不外泄，缺失/已删 id 静默忽略，按 UTF-8 字符边界截断并守住全部上限）与 `apply_live`（向所有流注入 `general`/`text`/`terms`，仅 `TwoWay`/`OneWay` 展开多语 `written` 与显式 `translation_pairs`）。
2. **现场实时路 Soniox 配置扩展**（`core/src/live/lane.rs`）：
   - `lane::Setup` 增补 `general: Vec<(String, String)>` 与 `text: String`。
   - `lane::config` 将非空 `general`、`text`、`terms`、`translation_terms` 经 `stated(...)` 装入 `context`；四项全空时省略整个 `context`（兼容既有 `glossary.rs` 断言）。
3. **整场精修路 Soniox 异步 + OpenRouter 提示词扩展**（`core/src/ai/write.rs`）：
   - 为 `write::Plan` 派生 `Default` 并增补 `general`、`background`、`translation_pairs`。
   - `write::transcription` 写入有界的 `context.{general, text, terms, translation_terms}`。
   - `write::request` 在 OpenRouter 系统提示词中按非空条件追加有界 `Facts:`、`Background:`（封顶 `CONTEXT_PROMPT_BACKGROUND_CHARS = 2_400`）、`Glossary:` 与 `Pairs:`，严格保持 `zdr: true`、`data_collection: "deny"` 与严格 JSON Schema。
4. **枢纽、录音与后台精修接线及事件**（`core/src/record/shelf.rs`、`core/src/app/{proto,hub,recording,work}.rs`、`docs/EVENTS.md`、`core/bindings/Setup.ts`、`ui/dev/mock.ts`）：
   - `About` 与 `Setup` 均增补 `#[serde(default)] pub topics: Vec<String>`；`Hub` 在 `Booted`、`Command::Setup` 与 `run_boot` 规整 `Setup.topics` 封顶 8 个，并在 `Msg::TopicRemoved` 时同步从 `Setup.topics` 清理已删主题 id 并落盘 `settings.json`。
   - `Hub::start` 将本场选中的 `setup.topics` 冻结进 `About.topics`；`Work::start_recording` 装配 `TopicContext` 传给 `recording::Start`，`recording::run` 经 `topic::apply_live` 配置每条流并发 `topic.context(where = "live", ...)`。
   - `WriteCtx::write_job` 每次精修起动时按书架最新主题列表与录音 `about.topics` 重新调用 `topic::assemble_context` 并发 `topic.context(where = "refine", ...)`。
   - 零新增 `Stats` 行；在 `docs/EVENTS.md` 登记 `topic.context`；重新生成 `core/bindings/Setup.ts` 并同步 `ui/dev/mock.ts`。

## 证据

- **新增测试（4 项；2 项纯单测沙箱实跑通过，2 项含本地 TCP/wiremock 测试沙箱编译通过）**：
  - `core/tests/topic.rs`：
    - `assemble_context_includes_pinned_and_selected_only_and_enforces_budgets`（通过）
    - `selected_topic_reaches_live_and_refine_while_unselected_topic_stays_local`（`--no-run` 编译通过，待沙箱外实跑）
    - `granted_key_async_refusal_keeps_draft_with_clear_code_after_topic_context`（`--no-run` 编译通过，待沙箱外实跑）
  - `core/tests/write.rs`：
    - `live_and_refine_requests_carry_general_text_terms_and_translation_terms`（通过）
- **既有回归与门禁（沙箱内全绿）**：
  - `cargo fmt --all` 与 `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 零告警通过。
  - 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）全绿。
  - `core/tests/topic.rs`（非 TCP 5/5 通过）、`core/tests/write.rs`（2/2 目标测试通过）、`core/tests/glossary.rs`（非 TCP 2/2 通过）、`core/tests/liveness.rs`（`every_event_in_the_catalogue_can_be_told_apart` 与 `i8_what_one_change_sends_does_not_grow_with_the_library` 均通过）。
  - `cd ui && npm run check && npm test && npm run build`：14 个测试文件 109/109 通过，构建通过。
  - `Cargo.lock` 与 `ui/package-lock.json` 无任何改动。

## 遗留 / 需注意

- 含本地 WebSocket/HTTP 替身服务器的 2 项端到端测试（`selected_topic_reaches_live_and_refine_while_unselected_topic_stays_local` 与 `granted_key_async_refusal_keeps_draft_with_clear_code_after_topic_context`）已通过编译与 clippy，待批次结束后由协调者在沙箱外执行完整 `scripts/build/check.sh` 实跑验收。
- AI 标题与话题生成（`ai/titles.rs`）、第 4 槽 `Progress.title` 与 SQLite 索引 v2 归 P6-03。
