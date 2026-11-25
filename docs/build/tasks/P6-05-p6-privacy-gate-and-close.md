# P6-05：读书会端到端验收、落盘隐私字节审计、升级 `pending.sh` 与 P6 收尾

## 目标

以一场完整的「读书会」端到端集成测试和真实落盘逐字节隐私审计验证 Phase P6 全部出口（E1–E6）：证明知识库主题与文件提取、双路 `context` 注入与未选主题隔离、AI 标题与话题生成（含手写保护、额度键草稿兜底与不阻塞精修/查询）、话题搜索与过滤全部协同成立；确认所有主题名/领域/背景/词表/译法/文件名/AI 标题/候选/话题词在真实落盘 `zutalk.jsonl`、`diagnose/` 与 `settings.json` 中零字节泄漏且 `zutalk_log::refused() == 0`；复测 I8 `State` 比值 `1.00`、`stats.full == 0` 与 `Tally ≤ 8192 B`；升级 `scripts/build/rules/pending.sh` 同禁 `P6`，同步文档并写出收尾报告 `docs/build/reports/P6-05-close.md`。

## 背景

- 计划依据：`docs/PLAN.md` §7、§8（P6 出口：一场读书会——专有名词识别对、标题像人写的）；`docs/build/phases/P6.md` §1 E1–E6、§2、§4；`docs/LESSONS.md`（P4 多核文件日志测试规范）。
- 涉及文件：
  - `core/tests/knowledge.rs`（新建：P6 读书会端到端验收与真实落盘隐私逐字节审计）
  - `core/tests/liveness.rs`（确认 I8 `i8_what_one_change_sends_does_not_grow_with_the_library` 含 `Setup.topics` 后比值仍恒为 `1.00`）
  - `scripts/build/rules/pending.sh`（升级正则为 `ignore = "P(1-|2|3|4|5|6)`）
  - `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`、`docs/EVENTS.md`（同步 P6 实际落地状态与经验）
  - `docs/build/reports/P6-05-close.md`（新建：P6 实测数据与收尾清单）

## 做法

1. **端到端读书会综合测试（`core/tests/knowledge.rs::book_club_end_to_end_topics_context_titles_tags_and_search`）**：
   - 使用本地替身 WebSocket（Soniox realtime）、本地替身 Soniox Async HTTP 与本地替身 OpenRouter HTTP，构造一场完整读书会场景（虚构样例，零真实人名）：
     1. 写入两条旧版 `talk/term` 确认启动时自动无损迁入固定 `"always"` 主题；
     2. 新建选中主题「读书会 · 看不见的城」（含领域、概况、背景、专有名词与译法）及未选主题「私人财务」（含独立哨兵词），并对选中主题执行 `Command::ImportTopicFile` 导入临时 `.md` 讲义，验证建议词出现在 `suggested_terms` 而未自动进 `terms`，随后经 `Command::RememberTerm` 采纳；
     3. 经 `Command::Setup` 选中读书会主题并开始录音；断言现场 WebSocket 首帧 `context` 收到 `always` + 读书会主题的 `general`/`text`/`terms`/`translation_terms`，**完全不含「私人财务」的任何内容**；
     4. 停止录音触发整场精修，断言 Soniox 异步 `POST /transcriptions` 与 OpenRouter 翻译 `POST /chat/completions` 同样只含 `always` + 读书会上下文，精修完成后自动触发 `job.titles`（经 `Outlet.progress` 观测 `progress.title` 槽或结果落盘）产出 AI 标题（`TitleBy::Ai`）、候选列表与 1–4 个话题词；
     5. 验证 `Query::Search` 搜 2 字与 ≥ 3 字话题词均命中 `SearchHitKind::Tag`，且 `Query::LibraryRecent` 配合 `LibraryFilter { tag: Some(...), .. }` 精确命中该场录音；
     6. 用户手动 `Command::Change { Title }` 改为手写标题（`TitleBy::Hand`），再次触发 `Command::GenerateTitle`，断言手写标题完好不变而 `title_candidates` 与 `tags` 刷新；
     7. 追加一条额度键场景验证（或独立测试函数）：当 Soniox 异步返回 403 令精修落入 `Failed(keys.async_refused)` 时，只要 OpenRouter 钥匙可用，草稿仍能自动拿到 AI 标题与话题。

2. **真实落盘逐字节隐私与可观测审计（`core/tests/knowledge.rs::knowledge_and_ai_titles_never_leak_to_logs_diagnose_or_settings`）**：
   - 按 `LESSONS.md` P4 经验：在 `#[tokio::test(flavor = "multi_thread")]` 中直接用 `zutalk_log::files`（不套 `App::with` 的内存捕获器），仅在 `Core::start(cfg)` 同步启动一瞬 `scope.enter()`，使全树事件真实落盘到 `<data>/logs/zutalk.jsonl`；
   - 注入高辨识度哨兵串：主题名、领域、概况键值、背景密句、词条原词与多语写法、显式译法源/目标词、导入讲义文件名与目录段、AI 生成标题、AI 候选标题、AI 话题词、以及说话原文；
   - 完整跑完建主题 → 文件导入 → 选主题开录 → 精修 → AI 标题生成 → `Command::Diagnose`；
   - 刷新日志后，逐字节扫描 `<data>/logs/*.jsonl`、`<data>/diagnose/<stamp>/**/*` 与 `<data>/settings.json`：
     - 断言上述全部哨兵串（含讲义目录/文件名、主题名、背景、词、标题、话题词）**在全部日志、诊断包与 `settings.json` 中零字节命中**；
     - 断言 `logs/zutalk.jsonl` 明确包含 `topic.saved`、`topic.context`（`live` 与 `refine`）与 `titles.done` 结构化事件；
     - 断言 `zutalk_log::refused() == 0`、`stats.full` 事件出现 `0` 次、且此刻 `Tally` JSON 字节数 `<= 8192`。

3. **升级规则脚本 `scripts/build/rules/pending.sh`**：
   - 将检查正则从 `ignore = "P(1-|2|3|4|5)` 升级为 `ignore = "P(1-|2|3|4|5|6)`，更新注释与错误提示包含 `P6`。

4. **复测全量门禁与同步文档**：
   - 跑通全量 `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh`，记录实测数据（I8 5 条 vs 100 条 `State` 字节数及比值、`Tally` 字节数、`Stats` 各表行数、Rust/UI 测试总数、10 个规则脚本状态、零新增依赖确认）。
   - 同步更新：
     - `docs/PLAN.md`（§2.1 `talk/topic` 与 `talk/recording`、§2.3 `Progress` 第 4 槽、§2.4 `State` I8 复测、§2.5 `knowledge` 视图、§5.3 P6 零新增依赖、§7 I8/I9 实测、§8 P6 状态）
     - `docs/DESIGN.md`（§3 概念表、§4 模块表、§5.9 知识库与双路上下文/AI 标题升级）
     - `docs/LESSONS.md`（追加「P6 里学到的」3–5 条真实工程收获）
     - `docs/EVENTS.md`（核对全部 83 个事件与「出了事先看什么」知识库/标题排查行）
   - 撰写收尾报告 `docs/build/reports/P6-05-close.md`（逐项核对 `P6.md` §1 E1–E6 与实测指标，并列明 `P6.md` §4 待负责人确认/真机验证清单）。

## 显式与可观测

- **真实落盘验证**：用真实文件日志（`zutalk_log::files`）而非内存替身验证 `logs/zutalk.jsonl`、`diagnose/` 与 `settings.json` 的隐私零泄漏与事件完备性。
- **统计硬预算守护**：断言 `stats.full == 0`、计数器仍 ≤ 40（实际 38/40，P6 零新增行）、`Tally` JSON `<= 8192 B`、`zutalk_log::refused() == 0`。
- **防挂起门禁**：升级后的 `pending.sh` 永久禁止 `P1–P6` 阶段留存任何 `#[ignore = "P6..."]` 测试。

## 先写的测试

1. `core/tests/knowledge.rs::book_club_end_to_end_topics_context_titles_tags_and_search`
2. `core/tests/knowledge.rs::quota_async_refusal_still_titles_draft_with_openrouter`
3. `core/tests/knowledge.rs::knowledge_and_ai_titles_never_leak_to_logs_diagnose_or_settings`

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含升级后的 `pending.sh` 及全部 10 个规则脚本、全部 Rust 测试与全部 UI 测试与构建）。
- `git diff` 核对 `Cargo.lock` 与 `ui/package-lock.json` 零新增外部依赖。
- `docs/build/reports/P6-05-close.md` 写清 E1–E6 实测数字、I8 `State` 比值 `1.00`、`stats.full == 0`、`Tally ≤ 8192 B` 与负责人待办清单。

## 不做

- 不修改 `server/`（服务器按服务每日额度与无盘直播房间已在 P4/P5 闭环，P6 无需改动服务器）。
- 不引入任何 PDF/DOCX 解析 crate 或新 npm 包。
- 不推送、不打标签、不发布。
