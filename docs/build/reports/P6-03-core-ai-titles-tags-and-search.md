状态：完成

# P6-03：AI 标题与话题（`ai/titles.rs`）、第 4 槽 `Progress.title` 与 SQLite 话题索引搜索

## 改动与文件

1. **记录元数据扩展**（`core/src/record/shelf.rs`、`core/src/app/{hub,work}.rs`）：
   - 新增 `#[ts(export)]` 类型 `TitleBy`（`None`/`Hand`/`Event`/`File`/`Ai`/`Opening`）、`ContentTag { label, at_ms }` 与 `TitleCandidate { by, text }`。
   - `About` 增补带 `#[serde(default)]` 的 `tags`、`title_by`、`title_candidates`（向后兼容既有 pack）。
   - `Hub::start` 导入音频文件（`Source::File`）时设 `title_by = TitleBy::File`（主持活动时保留活动标题到 `title_candidates`）；`work.rs::apply` 处理 `Change::Title` 时按非空置 `TitleBy::Hand`、置空回 `TitleBy::None`。
2. **后台 AI 标题与话题模块**（`core/src/ai.rs`、`core/src/ai/titles.rs`）：
   - 导出 `ai::titles`，定义全部带单位命名常量：`TITLES_SAMPLE_HEAD_LINES = 40`、`TITLES_SAMPLE_MID_LINES = 20`、`TITLES_SAMPLE_TAIL_LINES = 20`、`TITLES_MAX_INPUT_CHARS = 6_000`、`TITLES_MAX_TITLE_CHARS = 80`、`TITLES_MAX_CANDIDATES = 3`、`TITLES_MAX_TOTAL_CANDIDATES = 4`、`TITLES_MAX_TAGS = 4`、`TITLES_MAX_TAG_CHARS = 24`、`TITLES_TIMEOUT_MS = 30_000`。
   - 实现纯决策状态机 `TitleAction` + `title_plan`（`TitleBy::Hand` 及非空 `File`/`Event` 绝不设 `overwrite_title = true`）、`O(1)` 头中尾分段采样 `sample_transcript`（在超长 4 小时录音上仍保头/中/尾关键句且总字符 `<= 6_000`）、OpenRouter 严格 JSON Schema + `zdr: true` + `data_collection: "deny"` 请求构造 `request`、拒绝 HTTP 200 内嵌 `error`/`finish_reason == "error"` 并清洗截断与钳制 `at_ms` 的 `parse_reply`、`build_candidates` 以及多模型回退且零内容日志的 `generate`。
3. **第 4 槽 `Progress.title` 与不阻塞调度**（`core/src/app/{proto,hub,work}.rs`、`docs/EVENTS.md`）：
   - `Progress` 增补第 4 槽 `title: Option<Writing>`；新增免登录命令 `Command::{GenerateTitle, PickTitle}`（更新脱敏 `Debug`、`kind()`、`needs_person() = false`，录音进行中拒 `GenerateTitle` 以 `"recording.under_way"`）。
   - `Work` 新增独立信号量 `title_bg`（`TITLE_AT_ONCE_JOBS = 1`，与 `Work.bg` 彻底隔离，绝不堵住录音、精修或交城）与去重集合 `titling`（配合 RAII `TitlingGuard` 确保取消或异常也必清 `Progress.title`）。
   - 接通 3 处自动触发点（封盘后精修 `Owed::Waits`/`Owed::No` 时、精修成功 `Refined` 后、精修终态失败如 `keys.async_refused` 后在草稿上自动产出标题与话题）。
   - 结果通过 `desk` 顺序任务 `Job::ApplyTitles` 提交前二次核对 `shelf.about(&id)`：若录音已删则放弃并记 `titles.done(code = "gone")`；若最新 `title_by == TitleBy::Hand` 则保留用户手写标题不动仅落盘 `tags` 与 `title_candidates` 并记 `titles.done(code = "ok", applied = false)`；否则应用首条 AI 标题并提交 SQLite 索引与 `Library`/`Search`/`Recording` 失效。
   - 零新增 `Stats` 行；在 `docs/EVENTS.md` 登记 `titles.done` 与 `job` 种类 `titles`/`title.pick`。
4. **SQLite 投影索引升级至 v2 与视图扩展**（`core/src/view/{index,query}.rs`）：
   - `INDEX_SCHEMA_VERSION` 升为 `2`（老库打开自动按 `VersionMismatch -> Rebuild("version")` 重建）。
   - `recordings` 表增补 `title_by`、`tags_json`、`topics_json`；`search_docs` 对每条非空 `tag.label` 写入 `kind = "tag"`（同步支持 FTS5 trigram 与 1–2 字中文回退检索）；`SearchHitKind` 新增 `Tag`；`LibraryRow` 增补 `title_by`/`tags`/`topics`；`LibraryFilter` 增补 `tag: Option<String>`；`RecordingView` 增补 `topics`/`title_candidates`。
5. **TS 绑定、最小界面与 8 语词条同步**（`core/bindings/*.ts`、`ui/src/bus/channel.ts`、`ui/src/shell/{Shell,CommandPalette}.tsx`、`ui/src/pages/LibraryPage.tsx`、`ui/dev/mock.ts`、`ui/tests/*`、`ui/lang/*.json`、`app/tests/pump.rs`）：
   - 重生成全部 55 个 TS 绑定，同步 `EMPTY_PROGRESS`、`EMPTY_FILTER`、`SearchHitKind::Tag`、`mock.ts`、UI 测试夹具、`words.test.ts`（`STEPS` 增加 `'titling'`）与 8 份 `ui/lang/*.json`（`step.titling`、`code.titles.empty`、`code.titles.manual`、`search.kind.Tag`）。

## 证据

- **新增测试（5 项；4 项非 TCP 测试沙箱实跑通过，1 项含 wiremock 测试沙箱编译通过）**：
  - `core/tests/titles.rs`：
    - `title_plan_covers_every_provenance_and_key_combination`（通过）
    - `sample_transcript_bounds_four_hour_recording_in_o1_chars`（通过）
    - `parse_reply_rejects_http_200_error_body_and_sanitizes_titles_and_tags`（通过）
    - `background_titling_never_blocks_refining_protects_hand_title_and_respects_remove`（`cargo test --workspace --no-run` 编译通过，待沙箱外实跑）
  - `core/tests/view.rs`：
    - `index_v2_rebuilds_from_v1_and_searches_and_filters_tags`（通过，验证 v1→v2 自动重建、2 字中文话题 `"押金"` 与 ≥ 3 字话题 `"读书会讨论"` 均命中 `SearchHitKind::Tag`、`LibraryFilter.tag` 精确过滤）
- **既有回归与门禁（沙箱内全绿）**：
  - `cargo fmt --all` 与 `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 零告警通过。
  - 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）全绿。
  - `RUSTC_WRAPPER="" cargo test --workspace --no-run` 全仓所有 crate 与测试二进制全部编译通过。
  - `export_bindings`（55/55）、`titles`（非 TCP 3/3）、`view`（12/12）、`topic`（非 TCP 5/5）、`adopt`（非 TCP 6/6）、`glossary`（非 TCP 2/2）、`stats`（11/11）、`liveness`（`every_event_in_the_catalogue_can_be_told_apart` 与 `i8_what_one_change_sends_does_not_grow_with_the_library` 均通过）、`app/tests/pump.rs`（11/11）全部通过。
  - `cd ui && npm run check && npm test && npm run build`：14 个测试文件 109/109 通过（含 `words.test.ts` 12/12），构建通过。
  - `Cargo.lock` 与 `ui/package-lock.json` 无任何改动；零新增 `Stats` 行。

- **沙箱外全量门禁 `RUSTC_WRAPPER="" scripts/build/check.sh`**：`check: all green`（`rustfmt` + `clippy -D warnings` + 10/10 规则零欠账全绿 + Rust **776 passed, 0 failed, 6 ignored** + UI **14 文件 / 109 passed**，含全部 3 项新增 TCP 测试 `selected_topic_reaches_live_and_refine_while_unselected_topic_stays_local`、`granted_key_async_refusal_keeps_draft_with_clear_code_after_topic_context`、`background_titling_never_blocks_refining_protects_hand_title_and_respects_remove`，同步更新 `liveness.rs` 与 `view_scale.rs` 以匹配 `topic.context` 事件及 SQLite schema v2 新增列）。

## 遗留 / 需注意

- 知识库页、首页主题选择器、换标题候选弹窗、话题胶囊与阅读页「记进知识库」UI 归 P6-04。
