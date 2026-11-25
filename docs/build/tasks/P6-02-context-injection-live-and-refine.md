# P6-02：现场字幕与整场精修双路主题上下文注入（`DECISIONS.md` #7）

## 目标

将固定「每场都用」主题与用户本场选中的主题，按显式字符与条目预算组装为脱敏的 `TopicContext`，同时注入**现场实时字幕**（`core/src/live/lane.rs` 的 Soniox `context.{general, text, terms, translation_terms}`）与**整场精修**（`core/src/ai/write.rs` 的 Soniox 异步转写 `context` 与 OpenRouter 翻译系统提示词），并严守 `DECISIONS.md` #7（未选中的非固定主题绝不外发）；在 `Setup` 增补定界 `topics: Vec<String>`（封顶 8 个随机 id，守住 I8），在 `About` 冻结本场 `topics`，发射结构化事件 `topic.context`（只记数字、零内容泄漏）。

## 背景

- 计划依据：`docs/PLAN.md` §2.1、§2.2、§2.6；`docs/build/DECISIONS.md` 第 4 条与第 7 条；`docs/build/phases/P6.md` §3.3。
- 涉及文件：
  - `core/src/knowledge/topic.rs`（新增 `TopicContext`、`assemble_context`、`apply_live` 与上下文预算常量）
  - `core/src/live/lane.rs`（扩展 `Setup.{general, text}` 与 `lane::config` 的 `context` 构造）
  - `core/src/ai/write.rs`（扩展 `Plan.{general, background, translation_pairs}`、`transcription()` 与 `request()`）
  - `core/src/record/shelf.rs`（在 `About` 增补 `#[serde(default)] pub topics: Vec<String>`）
  - `core/src/app/{proto.rs,hub.rs,recording.rs,work.rs}`（`Setup.topics` 定界规整、`RemoveTopic` 联动清理、开录与精修双路注入、`topic.context` 事件）
  - `docs/EVENTS.md`（登记 `topic.context`）
  - `ui/dev/mock.ts` 及受 `Setup` 字段影响的 UI 测试夹具（最小类型同步保持 `check.sh` 全绿）

## 做法

1. **`TopicContext` 与纯组装/投影函数（`core/src/knowledge/topic.rs`）**：
   - 定义具名带单位常量：
     - `CONTEXT_MAX_GENERAL_ITEMS = 24`
     - `CONTEXT_MAX_TEXT_CHARS = 8_000`
     - `CONTEXT_MAX_TERMS = 300`
     - `CONTEXT_MAX_TRANSLATION_TERMS = 300`
     - `CONTEXT_PROMPT_BACKGROUND_CHARS = 2_400`
   - 定义 `TopicContext { pub topic_count: usize, pub general: Vec<(String, String)>, pub text: String, pub terms: Vec<Term>, pub translation_pairs: Vec<TranslationPair> }`，并**手写脱敏 `std::fmt::Debug`**（只打印 `topic_count`、`general_len`、`text_chars`、`terms_len`、`pairs_len`，绝不输出任何词或背景文本，守住原则 6）。
   - 纯函数 `pub fn assemble_context(all_topics: &[Topic], selected_ids: &[String], recording_title: &str) -> TopicContext`：
     - 只挑 `pinned == true`（即 `"always"`）和 `selected_ids` 中命中的主题（去重保序），**严禁包含任何未选中的非固定主题**；
     - `general`：若 `recording_title` 非空先加 `("topic", recording_title)`；再依次收集活跃主题的非空 `("domain", topic.domain)` 与全部 `facts`，按 `(key, value)` 去重并封顶 `CONTEXT_MAX_GENERAL_ITEMS`；
     - `text`：将活跃主题的非空 `background` 以 `"\n\n"` 拼接，按 UTF-8 字符边界安全截断至 `CONTEXT_MAX_TEXT_CHARS`；
     - `terms`：按 `profile("").key(&said)` 合并去重（合并 `written` 映射），封顶 `CONTEXT_MAX_TERMS`；
     - `translation_pairs`：去重封顶 `CONTEXT_MAX_TRANSLATION_TERMS`。
   - 纯函数 `pub fn apply_live(ctx: &TopicContext, stream: &Stream, setup: Setup) -> Setup`：
     - 填入 `general`（克隆 `ctx.general`）、`text`（克隆 `ctx.text`）；
     - 收集所有 `Term` 的 `said` + `written.values()` 以及 `ctx.translation_pairs` 的 `source`/`target` 去重封顶 `CONTEXT_MAX_TERMS` 进 `terms`；
     - 仅在 `Stream::TwoWay` / `Stream::OneWay` 时展开目标语言对应的 `Term` 译法对（保持与 `glossary::live` 语义 100% 一致）并追加 `ctx.translation_pairs`，去重封顶 `CONTEXT_MAX_TRANSLATION_TERMS` 进 `translation_terms`；`Stream::Transcribe` 时 `translation_terms` 为空，但 `general`/`text`/`terms` 照送。
   - 纯函数 `pub fn normalize_selected_topics(ids: Vec<String>) -> Vec<String>`：去空白、去 `"always"`（已固定自动包含）、过滤超过 64 字节项、去重并截断至 `MAX_SELECTED_TOPICS`（8）。

2. **现场路 Soniox 配置扩展（`core/src/live/lane.rs`）**：
   - `lane::Setup` 新增 `pub general: Vec<(String, String)>` 与 `pub text: String`（均受益于既有 `#[derive(Default)]`）。
   - `lane::config` 构建 `context`：
     - `general`：映射非空 `(key, value)` 为 `{"key": key, "value": value}`；
     - `text`：非空则 `json!(setup.text)`，空则 `Value::Null`；
     - `terms`：`setup.terms`；
     - `translation_terms`：`pairs`；
     - 经 `stated(...)` 清理空字段——四项全空时整个 `context` 省略，保持 `core/tests/glossary.rs` 断言 100% 成立。

3. **精修路 Soniox 异步 + OpenRouter 提示词扩展（`core/src/ai/write.rs`）**：
   - `write::Plan` 派生 `Default` 并增补 `pub general: Vec<(String, String)>`、`pub background: String`、`pub translation_pairs: Vec<(String, String)>`。
   - `write::transcription(plan, name, file)`：
     - 组装 `general`（`plan.general` + 若尚未包含且 `!plan.topic.is_empty()` 追加 `("topic", plan.topic)`，封顶 `CONTEXT_MAX_GENERAL_ITEMS`），非空写入 `body["context"]["general"]`；
     - 若 `!plan.background.trim().is_empty()`，按 UTF-8 字符边界截断至 `CONTEXT_MAX_TEXT_CHARS` 写入 `body["context"]["text"]`；
     - `terms` 汇总 `plan.terms` 全部写法与 `plan.translation_pairs` 的 `source`/`target`，去重封顶 `CONTEXT_MAX_TERMS` 写入 `body["context"]["terms"]`；
     - 若 `!plan.translation_pairs.is_empty()`，映射为 `[{"source": s, "target": t}]`（封顶 `CONTEXT_MAX_TRANSLATION_TERMS`）写入 `body["context"]["translation_terms"]`。
   - `write::request(model, plan, before, batch)`：
     - 在 `system` 提示词中按非空条件追加有界的 `Facts: ...`、`Background: ...`（按 UTF-8 边界截断至 `CONTEXT_PROMPT_BACKGROUND_CHARS`）、`Glossary: ...` 与 `Pairs: ...`；
     - 严格保留 `provider: { zdr: true, data_collection: "deny", require_parameters: true, only: model.only }`、`reasoning` 与严格 JSON Schema。

4. **枢纽、录音与后台精修接线（`core/src/record/shelf.rs`、`core/src/app/{proto,hub,recording,work}.rs`）**：
   - `About` 新增 `#[serde(default)] pub topics: Vec<String>`。
   - `Setup` 新增 `#[serde(default)] pub topics: Vec<String>`；`Hub` 在 `Command::Setup` 与 `Booted` 时用 `topic::normalize_selected_topics` 规整，并在 `RemoveTopic` 成功时顺带清理 `self.state.setup.topics` 中的该 id 并落盘 `settings.json`。
   - `Hub::start()` 把 `setup.topics` 冻结进 `about.topics`。
   - `Work::start_recording()` 按 `about.topics` 装配 `TopicContext` 传给 `recording::Start`（取代 `terms: Vec<Term>`）；`recording::run` 在开 lane 前用 `topic::apply_live` 配置每条流，并在有流开启时发一次 `info!("topic.context", rec = Id(&start.id), where = "live", topics = ctx.topic_count, terms = ctx.terms.len(), pairs = ctx.translation_pairs.len(), text_chars = ctx.text.chars().count())`。
   - `WriteCtx::write_job()` 在构造 `write::Plan` 前，直接按当前书架 `topic::list(&self.shelf)` + 该录音最新的 `about.topics` + `about.title` 重新调用 `topic::assemble_context`（确保重启补写或编辑主题后再精修都用最新主题包），填入 `Plan.{general, background, terms, translation_pairs}` 并发 `info!("topic.context", rec = Id(&self.id), where = "refine", ...)`。
   - 最小 TS 同步：重生成 `core/bindings/Setup.ts`，补齐 `ui/dev/mock.ts` 与 UI 测试夹具中的 `topics: []`。

## 显式与可观测

- **纯函数与确定预算**：`assemble_context`、`apply_live`、`normalize_selected_topics` 均为无 IO 纯函数，带显式字符/条目预算常量（过 `numbers.sh`），且 `TopicContext::fmt` 强制脱敏。
- **不变量与隐私**：未选中的主题绝不进入 `TopicContext`、`lane::config` 或 `write::Plan`；`Setup.topics` 封顶 8 个随机 id（I8 恒定）；`Facts`（自检包 `about.json`）不暴露任何主题名或内容。
- **零新 `Stats` 行与结构化事件**：不新增任何 `Stats` 行；在 `docs/EVENTS.md` 登记 `topic.context`（字段：`rec`、`where`、`topics`、`terms`、`pairs`、`text_chars`），过 `events.sh`。
- **故障与边界可见性测试**：验证未选主题绝不外泄、超预算文本按字符边界截断、已删主题 id 在 `assemble_context` 中静默忽略不崩溃、以及额度键精修遇 401/403 仍稳定落为 `keys.async_refused` 并在日志中可见 `topic.context(where = "refine")` 与 `refine.state(to = "Failed", why = "keys.async_refused")`。

## 先写的测试

在 `core/tests/topic.rs` 与 `core/tests/write.rs` 中先写以下失败测试，再实现代码：

1. `assemble_context_includes_pinned_and_selected_only_and_enforces_budgets`：构造 3 个主题（`always` + 选中主题 A + 未选中主题 B，含超长背景与超额词条），断言 `assemble_context` 含 `always` 与 A、**完全不含 B 的任何词/背景/概况**，且 `text` 截断在 `CONTEXT_MAX_TEXT_CHARS`（合法 UTF-8 边界）、`general`/`terms`/`translation_pairs` 不越上限，且 `format!("{ctx:?}")` 不含任何敏感词。
2. `live_and_refine_requests_carry_general_text_terms_and_translation_terms`：单元测试 `lane::config`、`write::transcription`、`write::request` 均包含 `general`、`text`/`Background`、`terms`、`translation_terms`；空上下文时 `lane::config` 仍省略 `context`。
3. `selected_topic_reaches_live_and_refine_while_unselected_topic_stays_local`（端到端本地替身 WebSocket + Soniox HTTP + OpenRouter HTTP）：建选中主题 A 和未选主题 B，选 A 录音并停止精修；断言 WebSocket 首帧、Soniox 异步 `POST /transcriptions` 与 OpenRouter `POST /chat/completions` 全部含 `always` 与 A 的背景/概况/词/译法，**三处均不含 B 的任何哨兵词**，且捕获日志含 `topic.context(where = "live")` 与 `topic.context(where = "refine")` 且零哨兵词泄漏。
4. `granted_key_async_refusal_keeps_draft_with_clear_code_after_topic_context`：模拟额度 Soniox 钥匙（`From::Granted`）在异步转写被 403 拒绝，断言 `refine.state` 进入 `Failed(keys.async_refused)`、草稿保留、且 `topic.context(where = "refine")` 可见。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿。
- 既有 `core/tests/glossary.rs`、`core/tests/write.rs`、`core/tests/live.rs`、`core/tests/liveness.rs`（含 I8 `State` 比值 `1.00`）全部通过。

## 不做

- 本任务不实现 `ai/titles.rs`、第 4 槽 `Progress.title` 或 SQLite `tags` 索引（归 P6-03）。
- 本任务不画首页主题选择器或知识库页 UI（归 P6-04）。
