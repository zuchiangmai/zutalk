状态：完成

## 改了什么

1. **读书会端到端验收、额度草稿起标题与真实落盘逐字节隐私审计（`core/tests/knowledge.rs`）**：
   - 新增 3 条全链路验收测试：
     1. `book_club_end_to_end_topics_context_titles_tags_and_search`：
        - 启动前写入两条旧版 `talk/term`，验证启动时自动无损迁入固定 `"always"` 主题；
        - 新建选中主题「读书会 · 看不见的城」（领域、概况、背景、专有名词、译法对）与未选主题「私人财务」（含独立哨兵）；对选中主题执行 `Command::ImportTopicFile` 导入临时 `.md` 讲义，验证候选词只入 `suggested_terms` 不自动进 `terms`，随后经 `Command::RememberTerm` 采纳；
        - 选中读书会主题开录，断言现场 WebSocket 首帧、整场精修 Soniox 异步 `POST /transcriptions` 与 OpenRouter 翻译 `POST /chat/completions` 均收到 `always` + 读书会上下文，且**零字节含「私人财务」**；验证 OpenRouter 标题请求带 `provider.zdr = true` 与 `provider.data_collection = "deny"`；
        - 验证自动 `job.titles` 产出 AI 标题（`TitleBy::Ai`）、候选列表与 1–4 个话题词；`Query::Search` 搜 2 字中文话题「押金」与 ≥ 3 字话题「读书会讨论」均命中 `SearchHitKind::Tag`，`Query::LibraryRecent` 配合 `LibraryFilter.tag` 精确命中与排异；
        - 手动 `Command::Change { Title }` 改为手写标题（`TitleBy::Hand`）后再次触发 `Command::GenerateTitle`，断言手写标题完好不变、候选与话题词刷新。
     2. `quota_async_refusal_still_titles_draft_with_openrouter`：
        - 社区额度服务器同时授予 `soniox` 与 `openrouter` 临时钥匙（`From::Granted`），Soniox 异步返回 403 触发 `keys.async_refused` 持久化失败并完整保留现场草稿（`DECISIONS.md` #4）；验证后台 OpenRouter 仍自动为草稿生成 `TitleBy::Ai` 标题与话题词并记 `titles.done(code = "ok", applied = true)`。
     3. `knowledge_and_ai_titles_never_leak_to_logs_diagnose_or_settings`：
        - 按 `LESSONS.md` P4 规范在 `multi_thread` 运行时安装真实文件日志 `zutalk_log::files`（仅在同步 `Core::start` / `core.send` 短时进入 `scope.enter()`，绝不跨 `.await` 持有 `Entered`），等待 `tape::kept(&tape_dir)` + 200 ms 后停录，完整跑通建主题 → 嵌套目录 `.md` 导入 → 记词 → 选主题开录 → 精修 → 自动 AI 标题 → `Command::Diagnose`；
        - 逐字节扫描 `<data>/logs/*.jsonl`、`<data>/diagnose/**/*` 与 `<data>/settings.json`，断言主题名/领域/概况/背景/词表/多语译写/译法对/导入目录与文件名/建议词/AI 标题/候选/话题词/说话原文/API 密钥/DID 全部**零字节泄漏**；
        - 断言日志含 `topic.saved`、`topic.context`（`live` 与 `refine`）、`titles.done(code = "ok", applied = true)`、`diagnose.wrote`，`stats.full` 为 `0`，`zutalk_log::refused()` 零增量，且诊断包 `stats.json` 紧凑 JSON `≤ 8192 B`。
2. **规则升级与全仓文档收尾**：
   - `scripts/build/rules/pending.sh`：升级为 `ignore = "P(1-|2|3|4|5|6)'`，同禁 P1–P6 挂起测试；
   - 更新 `docs/PLAN.md`（同步 §2.1 `talk/recording`/`talk/topic`、§2.3 `Progress` 第 4 槽、§2.4 `State` I8 `1332 B / 1334 B`、§2.5 `knowledge`/`search`、§5.3 P6 零新增依赖、§7 I8/I9 实测与 §8 P6 已做状态）；
   - 更新 `docs/DESIGN.md`（同步 §3 概念表、§4 模块表、§5.9 知识库/双路上下文/AI 标题话题与 §5.14 全仓 83 事件及 10 规则脚本）；
   - 更新 `docs/LESSONS.md`（追加「P6 里学到的」4 条工程经验）；
   - 更新 `docs/EVENTS.md`（核对全部 83 个事件并补充「知识库词表没生效、文件提词失败或 AI 标题没生成」排查行）。

## P6 出口逐项核对（E1–E6）

| 出口 | 状态 | 实测证据 |
|---|---|---|
| **E1** `talk/topic` 知识库、旧 `talk/term` 无损迁移、文件提取与 `adopt` | 通过 | `core/src/knowledge/topic.rs` 支持固定 `always` 主题、10,000 字及条目硬上限、零新 crate UTF-8 `.txt`/`.md`/`.csv`/`.tsv` 提取（≤ 512 KiB、仅留基名、只入 `suggested_terms`）与 `ensure_migrated` 自动无损迁移；`record/adopt.rs` 零 `crate::knowledge` 引用并在纯 JSON 层重签领回与合并 `always`；`layers.sh` 保持零欠账全绿 |
| **E2** 现场字幕与整场精修双路 `context` 注入（`DECISIONS.md` #7） | 通过 | `TopicContext` 手写脱敏 `Debug`、`assemble_context` 严守未选主题隔离与全部预算；`live/lane.rs` 与 `ai/write.rs`（Soniox 异步 + OpenRouter 翻译提示词）双路注入 `general`/`text`/`terms`/`translation_terms`；`Setup.topics` 封顶 8 个且只存随机十六进制 id；`topic.context` 只记计数与字符数 |
| **E3** AI 标题与话题后台任务、第 4 槽 `Progress.title` 与 SQLite v2 话题检索 | 通过 | `core/src/ai/titles.rs` 实现 `title_plan`、`O(1)` 4 小时采样（≤ 6,000 字符）、OpenRouter 严格 JSON 零留存与 HTTP 200 错误正文拦截；独立信号量 `TITLE_AT_ONCE_JOBS = 1` 绝不阻塞精修/交城/查询；`ApplyTitles` 写前重读复核守住 `TitleBy::Hand` 与 `Command::Remove`；额度键 403 拒精修后草稿仍自动起标题；SQLite `INDEX_SCHEMA_VERSION = 2` 自动重建并索引 `SearchHitKind::Tag`（含 2 字中文短词）与 `LibraryFilter.tag` |
| **E4** 界面知识库页、首页主题选择器、阅读页标题/话题/「记进知识库」与全部录音过滤 | 通过 | `KnowledgePage`（对照 `Knowledge.dc.html`）、`SettingsPage` 知识库入口卡片、`HomePage` 主题选择器（封顶 8）、`ReaderPage`/`TranscriptList`/`ResultPage`（`.title` vs `.title.auto`、换标题弹窗与第 4 槽加载、话题胶囊跳转不破 `VList`、手改行「+ 记进知识库」）、`LibraryPage` 话题副标题与过滤、`mock.ts` 与 8 语字典同步、`ui.sh` 全绿 |
| **E5** 读书会端到端、I8 / I9 / 隐私红线 | 通过 | `core/tests/knowledge.rs` 3 项端到端与真实落盘隐私审计测试覆盖全流程与零泄漏；`i8_what_one_change_sends_does_not_grow_with_the_library` 实测 `bytes_5 = 1332 B`、`bytes_100 = 1334 B`、**比值 `1.00`**、`inv_bytes = (60, 60)`；`two_minute_recording_tally_matches_the_table` 与全仓 0 新增 `Stats` 行守住 `stats.full == 0`（38/40 计数）与 `Tally ≤ 8192 B`；`ui/tests/scale_i8.test.tsx` 全绿 |
| **E6** 门禁全绿、零新增外部依赖、无 P6 挂起测试 | 通过 | `pending.sh` 升级禁 P1–P6；全部 10 个规则脚本零违规零欠账；`git diff 6025501 -- Cargo.lock ui/package-lock.json` 输出为空（零新增外部 crate 与 npm 包） |

## 证据

### 沙箱内实测

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（零警告）。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：**10/10 全绿，全仓零欠账**；`docs/EVENTS.md` 精确登记 **83 个**结构化事件并经 `events.sh` 双向核对通过。
- 零新增依赖核验：`git diff 6025501 -- Cargo.lock ui/package-lock.json` 为空。
- Rust 沙箱内编译与测试：
  - `RUSTC_WRAPPER="" cargo test --workspace --no-run`：全仓编译通过（含 `core/tests/knowledge.rs` 新增 3 项测试）；
  - `core/tests/liveness.rs`：
    - `i8_what_one_change_sends_does_not_grow_with_the_library`：`i8: bytes_5=1332 bytes_100=1334 ratio=1.00 state_pushes=(0,0) inv_bytes=(60,60)`；
    - `every_event_in_the_catalogue_can_be_told_apart`：通过；
    - `two_minute_recording_tally_matches_the_table`：通过（`counts = 30`、`peaks = 2`、`spreads = 10`、`queues = 6`，全仓注册 38/40 计数零新增，`stats.full == 0`）。
- 前端校验与测试（`ui/`）：
  - `npm run check && npm run build`：通过（`ui/dist/{index,captions,viewer}.html` 三入口正常输出）；
  - `npm test`：**16/16 文件、118/118 测试通过**（含新增 `knowledge.test.tsx` 4/4、`titles.test.tsx` 5/5，以及 `words.test.ts` 12/12、`scale_i8.test.tsx` 4/4）。

### 沙箱外全量门禁（`RUSTC_WRAPPER="" scripts/build/check.sh`）

- `rustfmt` + `clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：**10/10 全绿，全仓零欠账**。
- Rust 全量测试：**779 passed, 0 failed, 6 ignored**（含 `core/tests/knowledge.rs` 全部 3 项端到端与真实落盘隐私字节审计测试一次通过：`book_club_end_to_end_topics_context_titles_tags_and_search`、`quota_async_refusal_still_titles_draft_with_openrouter`、`knowledge_and_ai_titles_never_leak_to_logs_diagnose_or_settings`）。
- UI 全量测试与构建：`tsc` + `vite build`（3 入口）+ Vitest **16 files / 118 passed**。
- 总结论：**`check: all green`**。

## 遗留 / 需负责人确认或动手的事

1. **只用额度的人如何做整场精修（`DECISIONS.md` #4）**：当前继续按临时决定执行——Soniox 异步接口拒绝临时钥匙时停在现场稿并给 `keys.async_refused` 引导填自有钥匙，同时 AI 标题与话题已可借助 OpenRouter 额度在草稿上自动产出；若未来和 Soniox 确认可发异步临时钥匙或由城邦提供转写服务，再切 `server/src/ledger.rs` 即可。
2. **PDF / DOCX 讲义提取**：P6 在零新增 crate 约束下支持 UTF-8 `.txt`/`.md`/`.markdown`/`.csv`/`.tsv` 文件导入提取候选词与背景（上限 512 KiB）；若负责人希望支持直接读取 `.pdf` 或 `.docx`，需拍板引入对应解析 crate。
3. **真机真钥匙读书会实测**：P6 所有自动化测试用本地替身 Soniox / OpenRouter 服务器验证协议、预算与防泄漏；真实 Soniox `stt-rt-v6`/`stt-async-v6` 与 OpenRouter 模型的专有名词识别率及中文标题文风，需负责人用真钥匙录一场真实读书会验收。
