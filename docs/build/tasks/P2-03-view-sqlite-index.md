# P2-03 可丢弃本机 SQLite + FTS5 索引（`core/src/view/index.rs`）

## 目标

做完以后：

1. `core/Cargo.toml` 引入 `rusqlite = { version = "0.37", features = ["bundled"] }`。
2. `core/src/view/index.rs` 实现完整的本机 SQLite 投影索引（WAL 模式），作为 Zu pack 的可丢弃缓存：
   - **自动自愈**：缺失、schema 版本不符、`by` 作者不符或文件损坏时自动删除并从 `Shelf` 逐条流式重建；
   - **启动对账**（`reconcile`）：已有库打开时仅重索引文件元信息有变化或新增/删除的录音，零变动时几乎不做磁盘读盘；
   - **增量同步**：`upsert_recording`、`remove_recording`、`update_audio`、`update_filing`、`update_refine_error` 均在单事务内更新；
   - **资料库查询**：`library_summary`（按月计数 + 总数，支持 `tz_offset_min`）、`library_month`（某月分页 + 过滤）、`library_recent`（侧栏最近 N 条 + 过滤）；
   - **全文搜索**：FTS5 `trigram` 覆盖标题、当前版正文、全部译文与全部笔记（经 `live::lang::profile` 归一化，全角与泰语 SARA AM 均命中），对 1–2 字符短词自动回退到归一化子串匹配；
   - **分版正文表**（`take_lines`）：索引每个版本的行与时间区间，供 P2-04 亚毫秒级 64 行分块与对比。

## 背景

- 设计：`docs/build/phases/P2.md` §3.2、§3.4、§3.7；`docs/PLAN.md` §2.5、§5.1、§5.3。
- 涉及文件：
  - `core/Cargo.toml`
  - `core/src/view.rs`、新增 `core/src/view/index.rs`
  - `ui/lang/*.json`（8 种语言新增 `code.index`）
  - `docs/EVENTS.md`（登记 `index.open`、`index.sync`）
- 既有规范约束：
  - `layers.sh`：`view` 只能引用 `rt`、`capture`、`live`、`record`、`knowledge`、`out` 与 `zutalk_log`，绝不引用 `app`；
  - `numbers.sh`：全部时限与上限为带单位的命名常量并附注释；
  - `events.sh`：日志只出代码、id 与数字，绝不记录标题、正文、笔记或搜索词。

## 做法

### 1. 依赖与常量

- 在 `core/Cargo.toml` 的 `[dependencies]` 加入 `rusqlite = { version = "0.37", features = ["bundled"] }`。
- 在 `core/src/view/index.rs` 定义带单位与原因注释的公开常量：
  - `pub const INDEX_SCHEMA_VERSION: u32 = 1;`
  - `pub const INDEX_BUSY_TIMEOUT_MS: u64 = 200;`（WAL 读写并发等待）
  - `pub const LIBRARY_PAGE_DEFAULT_ROWS: u32 = 50;`
  - `pub const LIBRARY_PAGE_MAX_ROWS: u32 = 200;`
  - `pub const SEARCH_DEFAULT_HITS: u32 = 100;`
  - `pub const SEARCH_MAX_HITS: u32 = 200;`
  - `pub const SEARCH_SNIPPET_MAX_BYTES: usize = 240;`

### 2. 打开状态机 `IndexHealth` 与纯决策函数 `open_plan`

- 定义枚举：
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum IndexHealth {
      Ready,
      Missing,
      VersionMismatch,
      OwnerMismatch,
      Corrupt,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum OpenPlan {
      Reuse,
      Rebuild(&'static str), // "missing" | "version" | "owner" | "corrupt"
  }

  pub fn open_plan(health: IndexHealth) -> OpenPlan
  ```
- `Index::open(path: &Path, shelf: &Shelf, tapes: &Path, stats: &Stats) -> Result<Index, &'static str>`：
  - 探测库文件是否存在、是否可打开、执行 `PRAGMA quick_check`、核对 `PRAGMA user_version == INDEX_SCHEMA_VERSION` 与 `meta.by == shelf.by()`，得出 `IndexHealth`。
  - 按 `open_plan(health)`：
    - `Rebuild(why)`：删掉 `path` 及同行 `-wal` / `-shm`（忽略 NotFound），建表，流式扫描 `shelf` 写入全部录音、分版行（`takes::list` + `takes::read`）与搜索文档，记 `index.rebuilds` 加 1 与 `index.rebuild_ms`，发 `index.open(mode = "rebuilt", why, rows, ms)`（`why == "corrupt"` 用 `warn!`，其余用 `info!`）。
    - `Reuse`：执行 `reconcile(shelf, tapes)` 仅补齐增删或 `about.jsonl`/`lines.jsonl` 元信息（大小 + 修改时间纳秒）有变动的录音（如对账时遇 SQLite 损坏则立即转入重建），发 `index.open(mode = "existing", why = "ok", rows, ms)`。

### 3. Schema 与数据结构（全部 `ts-rs` 导出）

- 表结构严格照 `P2.md` §3.4：`meta`、`recordings`（含 `idx_recordings_started`）、`take_lines`（含 `PRIMARY KEY (recording_id, take, seq)` 与 `idx_take_lines_time`）、`search_docs`（含 `idx_search_docs_rec`）、`search_fts`（`fts5(norm_text, content='search_docs', content_rowid='doc_id', tokenize='trigram')`）。
- 导出类型（位于 `view/index.rs`，由 `app/proto.rs` 在后续任务统一 re-export）：
  - `IndexRow`（或直接命名为 `Row` 放在 `view/index.rs` 供 P2-06 替换 `proto::Row`！为避免 P2-03 与现存 `proto::Row` 撞 ts-rs 导出名，本件导出 `LibraryRow` 或 `Row`？**决定**：`ts-rs` 按 Rust 类型名写 `core/bindings/<Name>.ts`，因此在 P2-03 至 P2-05 过渡期间 `proto::Row` 仍在 `core/src/app/proto.rs`，**请在本件直接使用 `LibraryRow` 作为 `view/index.rs` 的行类型（字段含 `id`, `started`, `length_ms`, `title`, `preview`, `spoken`, `draft`, `audio`, `filed`, `refine_failed`）**，永不与 `proto::Row` 冲突！）
  - `MonthCount { pub month: String, pub count: u32 }`
  - `LibrarySummary { pub total: u32, pub months: Vec<MonthCount> }`
  - `LibraryFilter { pub draft_only: bool, pub audio_only: bool, pub filed_only: bool, pub language: Option<String> }`
  - `LibraryPage { pub rows: Vec<LibraryRow>, pub total: u32, pub has_more: bool }`
  - `SearchHitKind { Title, Line, Translation, Note }`
  - `SearchHit { pub recording_id: String, pub kind: SearchHitKind, pub key: String, pub lang: String, pub snippet: String }`
  - `SearchResult { pub hits: Vec<SearchHit>, pub truncated: bool }`

### 4. 查询与同步 API（含只读连接支持）

- `Index` 持有可写 `Connection`，并提供只读视图打开口 `pub struct Reader(Connection)`（`Reader::open(path: &Path) -> Result<Reader, &'static str>`，使用 `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` + `busy_timeout(INDEX_BUSY_TIMEOUT_MS)`），以及 `Reader::interrupt_handle(&self) -> rusqlite::InterruptHandle`，供 P2-04 的 L3 `ViewEngine` 并发读与随时打断！
- 读写共享的只读查询方法挂在 `Reader`（`Index` 也可直接委托或共享 helper）：
  - `library_summary(&self, tz_offset_min: i32) -> Result<LibrarySummary, &'static str>`
  - `library_month(&self, month: &str, tz_offset_min: i32, filter: &LibraryFilter, limit: u32, offset: u32) -> Result<LibraryPage, &'static str>`
  - `library_recent(&self, filter: &LibraryFilter, limit: u32, offset: u32) -> Result<LibraryPage, &'static str>`
  - `search(&self, query: &str, limit: u32) -> Result<SearchResult, &'static str>`（对查询先做 `profile("").key(query).trim()`；`chars().count() >= 3` 走 `search_fts MATCH`；`1..=2` 走 `instr(norm_text, ?1) > 0`；结果截断至 `limit.clamp(1, SEARCH_MAX_HITS)` 并设置 `truncated`）
  - `take_line_count(&self, id: &str, take: u32) -> Result<u32, &'static str>`
  - `take_lines_slice(&self, id: &str, take: u32, offset: u32, limit: u32) -> Result<Vec<(String, Said)>, &'static str>`
  - `take_lines_in_time(&self, id: &str, take: u32, t0_ms: u32, t1_ms: u32) -> Result<Vec<(String, Said)>, &'static str>`（走 `idx_take_lines_time`，供 `compare` 按块时间范围取重叠行）
  - `recording_row(&self, id: &str) -> Result<Option<LibraryRow>, &'static str>`
- 增量写方法挂在 `Index`（每次发出 `index.sync` 事件）：
  - `upsert_recording(&mut self, shelf: &Shelf, tapes: &Path, id: &str) -> Result<(), &'static str>`（单事务读取该录音的 `about`、所有 `takes`、全部 `notes`、`FILING` 与 `Journal.failed` 更新四张表；若已被删则等价 `remove_recording`）
  - `remove_recording(&mut self, id: &str) -> Result<(), &'static str>`
  - `update_audio(&mut self, id: &str, audio: bool) -> Result<(), &'static str>`
  - `update_filing(&mut self, id: &str, state: &str) -> Result<(), &'static str>`
  - `update_refine_error(&mut self, id: &str, failure: Option<&RefineFailure>) -> Result<(), &'static str>`
  - `rebuild(&mut self, shelf: &Shelf, tapes: &Path) -> Result<usize, &'static str>`（逐条录音读完即释放，不把所有录音正文同时留在内存）
- 错误统一映射为 `"index"`（不要暴露 SQLite 错误原文进日志或界面；在 `ui/lang/*.json` 8 个文件加 `code.index`）。

## 显式与可观测

- **状态机**：`IndexHealth`（5 态）+ 纯函数 `open_plan`（配完整转换表测试）。
- **命名常量**：`INDEX_SCHEMA_VERSION`、`INDEX_BUSY_TIMEOUT_MS`、`LIBRARY_PAGE_DEFAULT_ROWS`、`LIBRARY_PAGE_MAX_ROWS`、`SEARCH_DEFAULT_HITS`、`SEARCH_MAX_HITS`、`SEARCH_SNIPPET_MAX_BYTES`，全过 `numbers.sh`。
- **事件**：
  - `index.open`（`info`；`why == "corrupt"` 时 `warn`）：`mode`（`"existing"` / `"rebuilt"`）、`why`（`"ok"` / `"missing"` / `"version"` / `"owner"` / `"corrupt"`）、`rows`、`ms`。
  - `index.sync`（`debug`；失败时 `warn`）：`op`（`"upsert"` / `"remove"` / `"audio"` / `"filing"` / `"refine"`）、`code`、`ms`。
  - 登记进 `docs/EVENTS.md`；绝不记录任何标题、话语、笔记或搜索词。
- **统计**：计数 `index.rebuilds`、分布 `index.rebuild_ms`（计入 `Stats`）。
- **故障可见性**：损坏库测试不仅断言重建后数据 100% 恢复，还断言日志里存在 `index.open(mode = "rebuilt", why = "corrupt")` 且 `index.rebuilds == 1`。

## 先写的测试

新建 `core/tests/view.rs`（本件先写索引部分，P2-04 接着扩展分块/对比/查询部分）：

1. `index_open_plan_table`：穷举 `IndexHealth` 五种状态验证 `open_plan`。
2. `index_builds_from_shelf_and_rebuilds_on_delete_version_owner_or_corruption`：
   - 写多条录音（含标题、多版行、译文、带时间笔记）→ 首次 `Index::open` 重建成功（事件 `mode = "rebuilt", why = "missing"`）；
   - 再次打开直接复用（`mode = "existing", why = "ok"`）；
   - 往 `.sqlite3` 写垃圾字节破坏文件 → 再打开自动删库重建，数据 100% 恢复且事件 `why = "corrupt"`、`index.rebuilds` 加 1；
   - 改 `PRAGMA user_version = 999` → 再打开自动重建（`why = "version"`）；
   - 换另一个 DID 的 `Shelf` 指向同路径 → 自动重建（`why = "owner"`）；
   - 离线往 `Shelf` 追加一条新录音再打开 → `reconcile` 自动补齐新录音。
3. `index_incremental_sync_tracks_upsert_remove_audio_filing_and_refine_failure`：验证每一种增量更新无需重建即可被 `Reader` 立刻读到。
4. `index_library_months_recent_and_filters`：跨 4 个月的数据，验证 `library_summary`（含正负 `tz_offset_min` 边界）、`library_month` 分页、`library_recent` 排序与 `draft_only` / `audio_only` / `filed_only` / `language` 过滤。
5. `index_search_covers_title_lines_translations_notes_cjk_thai_fullwidth_and_short_queries`：
   - 搜标题、搜原话、搜译文、搜带时间笔记；
   - 搜中文 4 字词（走 FTS5 `trigram`）、中文 2 字词和 1 字词（走短词回退）、全角 `"Ｍａｒｃｈ"`、泰语 SARA AM 异写 `"น\u{e4d}\u{e49}\u{e32}หนัก"`、英文大小写；
   - 验证 `EXPLAIN QUERY PLAN`：3+ 字搜索走 `search_fts`，分块取行走 `take_lines` 主键，按月查询走 `idx_recordings_started`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `layers.sh`、`numbers.sh`、`events.sh`、`words.test.ts`）。
- `core/tests/view.rs` 全部通过。

## 不做

- 不写分块对比和 `ViewEngine`（P2-04）。
- 不接入 `app/work.rs` 或改 `State`（P2-05、P2-06）。
