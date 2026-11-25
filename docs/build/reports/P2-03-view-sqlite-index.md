状态：完成

## 改动与文件

1. **可丢弃本机 SQLite + FTS5 索引（`core/src/view/index.rs`、`core/src/view.rs`）**
   - 定义命名常量（均附单位与原因）：`INDEX_SCHEMA_VERSION = 1`、`INDEX_BUSY_TIMEOUT_MS = 200`、`LIBRARY_PAGE_DEFAULT_ROWS = 50`、`LIBRARY_PAGE_MAX_ROWS = 200`、`SEARCH_DEFAULT_HITS = 100`、`SEARCH_MAX_HITS = 200`、`SEARCH_SNIPPET_MAX_BYTES = 240`。
   - 实现打开状态机 `IndexHealth`（`Ready` / `Missing` / `VersionMismatch` / `OwnerMismatch` / `Corrupt`）与纯函数 `open_plan`；缺失、版本不符、作者不符或损坏时自动删库（含 `-wal`/`-shm`）并逐条流式重建（`O(1)` 内存，I3），发出 `index.open` 事件并更新 `index.rebuilds` 与 `index.rebuild_ms`。
   - 实现启动快速对账 `reconcile`：按 `(about_sig, lines_sig, audio, filed_state, refine_error)` 仅重读新增、删除或改动的录音目录；零变动时不解析 pack。
   - 建表 `meta`、`recordings`（含 `idx_recordings_started`）、`take_lines`（`WITHOUT ROWID` 主键 `(recording_id, take, seq)` + `idx_take_lines_time`）、`search_docs`（含 `idx_search_docs_rec`）与外部内容 FTS5 `search_fts`（`tokenize='trigram'` + 增删改同步触发器）。
   - 提供只读并发连接 `Reader`（`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` + `interrupt_handle()`），实现 `library_summary`（支持正负 `tz_offset_min`）、`library_month`、`library_recent`（含 `draft_only` / `audio_only` / `filed_only` / `language` 过滤）、`search`（≥ 3 字符走 FTS5 `trigram`，1–2 字符回退 `instr(norm_text, ?1) > 0`，经 `live::lang::profile` 统一大小写、全角与泰语 SARA AM）、`take_line_count`、`take_lines_slice`、`take_lines_in_time`、`recording_row`。
   - 提供单写手增量同步：`upsert_recording`、`remove_recording`、`update_audio`、`update_filing`、`update_refine_error`、`rebuild`（每次发出 `index.sync` 事件，不记录任何正文或搜索词）。
   - 导出 `ts-rs` 绑定：`LibraryRow`、`MonthCount`、`LibrarySummary`、`LibraryFilter`、`LibraryPage`、`SearchHitKind`、`SearchHit`、`SearchResult`。
2. **解耦读取精修失败记录（`core/src/record/takes.rs`）**
   - 新增 `takes::read_failure(tape_dir)` 供 `view` 层读取 `write.json` 中的 `failed` 而不向上或跨层依赖 `ai`。
3. **事件、词表与测试（`docs/EVENTS.md`、`ui/lang/*.json`、`core/tests/view.rs`）**
   - `docs/EVENTS.md` 新增 `index.open` 与 `index.sync`；8 个 `ui/lang/*.json` 补齐 `code.index`。
   - 新建 `core/tests/view.rs`（5 条测试，含 `EXPLAIN QUERY PLAN` 断言）。

## 证据

- `cargo fmt --all -- --check` 通过。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`（22/22 通过，导出 8 个新 TS 类型）。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 9 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`）全绿。
- `core/tests/view.rs`（5/5 通过，0.25s）：
  - `index_open_plan_table`
  - `index_builds_from_shelf_and_rebuilds_on_delete_version_owner_or_corruption`（验证损坏库重建恢复 + `warn` 事件 `why = "corrupt"` + `index.rebuilds == 1` + 离线追加对账）
  - `index_incremental_sync_tracks_upsert_remove_audio_filing_and_refine_failure`
  - `index_library_months_recent_and_filters`
  - `index_search_covers_title_lines_translations_notes_cjk_thai_fullwidth_and_short_queries`（验证 `EXPLAIN QUERY PLAN`：`search_fts VIRTUAL TABLE INDEX`、`take_lines PRIMARY KEY`、`idx_recordings_started`）
- `liveness::every_event_in_the_catalogue_can_be_told_apart`（1/1）与 `ui` `svelte-check`（0 errors）+ `npm test`（35/35）全绿。

## 遗留 / 待负责人确认

- `rusqlite = { version = "0.37", features = ["bundled"] }` 依赖已由前序提交就位；1–2 字短词走归一化子串扫描、3+ 字走 FTS5 `trigram`（`P2.md` §4 第 3、5 条待确认）。
- 分块正文对比与 L3 `ViewEngine` 在 P2-04 实现；接进 `Work` / `Core` 在 P2-05 完成。
