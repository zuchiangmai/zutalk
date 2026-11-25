# P2-04 分块正文、纯函数对比与 L3 查询引擎（`view/transcript.rs`、`view/query.rs`）

## 目标

做完以后：

1. **`core/src/view/transcript.rs`**：
   - 64 行一块的分块数据结构 `TranscriptChunk` 与纯切片函数 `slice_chunk`；
   - 纯函数 `compare_lines(a, b) -> Vec<CompareRow>`：按时间区间重叠对齐两版（无时间戳时退回位置对齐），带前缀/后缀剥离与有界 LCS 的字符级 `DiffOp`，时间复杂度严格有界。
2. **`core/src/view/query.rs`**：
   - 完整导出协议类型（`ts-rs`）：`ViewKind`、`Invalidate`、`Invalidations`（有界合并环形缓冲，上限 64 条）、`Query`、`QueryReply`、`QueryValue`、`RecordingView`、`OutboundRow`；
   - `ViewEngine`：在 `work` 子树上经 `Semaphore(QUERY_CONCURRENT_MAX_JOBS = 8)` 与 `Tree::blocking("view.query", …)` 执行全部 10 种查询；
   - 同槽位新查询到达时，通过 `CancellationToken` + SQLite `InterruptHandle` 立刻打断旧查询，旧查询返回 `Err("query.cancelled")`；
   - 单次查询记入 `query.ms` 分布与 `query.done` / `query.cancelled` 计数，超 `QUERY_BUDGET_MS = 100` 发 `query.slow`。

## 背景

- 设计：`docs/build/phases/P2.md` §3.5、§3.6、§3.7；`docs/PLAN.md` §2.5、§4.1（L3 查询）、§5.2。
- 涉及文件：
  - `core/src/view.rs`
  - 新增 `core/src/view/transcript.rs`、`core/src/view/query.rs`
  - `ui/lang/*.json`（8 种语言新增 `code.query.cancelled`）
  - `docs/EVENTS.md`（登记 `query.done`、`query.slow`、`view.invalidate`）

## 做法

### 1. 分块与对比（`core/src/view/transcript.rs`）

- 命名常量：
  - `pub const TRANSCRIPT_CHUNK_LINES: usize = 64;`（每块 64 行，`PLAN.md` §2.5）
  - `pub const DIFF_LCS_MAX_BYTES: usize = 1_024;`（单行字符级 LCS 上限，避免极端长行二次耗时）
- 导出类型（全部 `#[ts(export)]`）：
  - `TranscriptChunk { pub id: String, pub take: u32, pub chunk: u32, pub total_lines: u32, pub total_chunks: u32, pub lines: Vec<(String, Said)> }`
  - `DiffOp`：`#[serde(tag = "kind")]`：`Equal { text: String } | Delete { text: String } | Insert { text: String }`
  - `CompareRow { pub key_a: Option<String>, pub key_b: Option<String>, pub t0: u32, pub t1: u32, pub speaker: String, pub language: String, pub changed: bool, pub text_a: String, pub text_b: String, pub ops: Vec<DiffOp> }`
  - `CompareChunk { pub id: String, pub take_a: u32, pub take_b: u32, pub chunk: u32, pub total_chunks: u32, pub rows: Vec<CompareRow> }`
- 纯函数：
  - `pub fn total_chunks(total_lines: u32) -> u32`（`total_lines.div_ceil(TRANSCRIPT_CHUNK_LINES as u32)`）
  - `pub fn slice_chunk(id: &str, take: u32, all: &[(String, Said)], chunk: u32) -> TranscriptChunk`
  - `pub fn diff_text(a: &str, b: &str) -> Vec<DiffOp>`：先剥离相同字符前缀与后缀；剩余部分若 `a_rem.len() + b_rem.len() <= DIFF_LCS_MAX_BYTES` 跑经典字符级 LCS 合并相邻同种操作；否则输出 `Delete(a_rem)` + `Insert(b_rem)`。保证把 `ops` 里 `Equal+Delete` 拼回去严格等于 `a`，把 `Equal+Insert` 拼回去严格等于 `b`！
  - `pub fn compare_lines(a: &[(String, Said)], b: &[(String, Said)]) -> Vec<CompareRow>`：
    - 若两版都有非零时间区间：为每条 `b` 行找时间重叠（`a.t0 < b.t1 && b.t0 < a.t1`，或点时刻相等）的 `a` 行并按序拼成 `text_a`（记 `key_a` 为首条重叠行键），算出 `changed = text_a != text_b` 与 `ops = diff_text(&text_a, &text_b)`；若某条 `a` 行不与任何 `b` 行重叠，也按其 `t0` 位置插入为 `key_b: None` 的删行。
    - 若时间戳全为 0：按位置一一配对。

### 2. 协议与 L3 引擎（`core/src/view/query.rs`）

- 命名常量：
  - `pub const QUERY_BUDGET_MS: u64 = 100;`（L3 查询预算，`PLAN.md` §4.1）
  - `pub const QUERY_CONCURRENT_MAX_JOBS: usize = 8;`（L3 并发阻塞查询上限）
  - `pub const KEEP_INVALIDATIONS_EVENTS: usize = 64;`（`Outlet.invalidated` 环形缓存上限）
- 协议类型（全部 `#[ts(export)]`）：
  - `ViewKind`：`Library | Search | Recording | Transcript | Glossary | Calendar | Outbound`（含 `pub const ALL: [ViewKind; 7]` 与 `pub fn code(self) -> &'static str`）
  - `Invalidate { pub view: ViewKind, pub key: Option<String>, #[ts(type = "number")] pub version: u64 }`
  - `Invalidations { pub next: u64, pub items: Vec<Invalidate> }`（提供纯合并方法 `push(&mut self, view, key, version)`：移除同 `(view, key)` 的旧项再追加新项，超出 `KEEP_INVALIDATIONS_EVENTS` 裁掉最旧项，`next = version`）
  - `RecordingView { pub row: LibraryRow, pub shown: Vec<String>, pub source: String, pub take: u32, pub takes: Vec<TakeInfo>, pub speakers: BTreeMap<String, String>, pub notes: Vec<Note>, pub note: String, pub filed: Option<Filing>, pub refine: RefineStatus }`
  - `OutboundRow { pub id: String, pub title: String, #[ts(type = "number")] pub started: i64, pub filing: Filing }`
  - `Query`（`#[serde(tag = "view")]`）：`LibrarySummary`、`LibraryMonth`、`LibraryRecent`、`Search`、`Recording`、`Transcript`、`Compare`、`Glossary`、`Calendar`、`Outbound`（字段照 `P2.md` §3.6.2，提供 `pub fn code(&self) -> &'static str` 和 `slot(&self) -> QuerySlot`）
  - `QueryReply { #[ts(type = "number")] pub version: u64, pub value: QueryValue }`
  - `QueryValue`（`#[serde(tag = "view", content = "data")]`）：`LibrarySummary`、`LibraryPage`、`Search`、`Recording`、`Transcript`、`Compare`、`Glossary`、`Calendar`、`Outbound`
- `ViewEngine`：
  - 持有：
    - `tree: Tree`（L3 归属 `work` 子树）；
    - `slots: Mutex<BTreeMap<QuerySlot, ActiveSlot>>`（每个槽位存最新代号 `gen: u64`、`CancellationToken`、`Arc<Mutex<Option<InterruptHandle>>>`；锁极短，绝不跨越 `.await`）；
    - `permits: Arc<Semaphore>`（容量 `QUERY_CONCURRENT_MAX_JOBS = 8`）；
    - `version: Arc<AtomicU64>`（单调递增视图版本号，初值 0）；
    - `invalidated: Register<Invalidations>`；
    - `source: Arc<Mutex<ViewSource>>`（由 `Work` 更新：当前 `shelf: Option<Arc<Shelf>>`、`index_path: Option<PathBuf>`、`tapes: PathBuf`、`has_soniox_key: bool`、`active_recording: Option<String>`、`active_refines: BTreeMap<String, (String, u32, u32)>`、`calendar: Option<Calendar>`、`ready: bool`）；
    - 统计：`query_done`（Counter）、`query_cancelled`（Counter）、`view_invalidated`（Counter）、`query_ms`（Spread）。
  - 公开方法：
    - `pub fn new(tree: Tree, tapes: PathBuf) -> (Self, Reading<Invalidations>)`
    - `pub fn set_source(...)` / `pub fn set_calendar(...)` / `pub fn set_active_recording(...)` / `pub fn set_active_refine(...)`
    - `pub fn invalidate(&self, view: ViewKind, key: Option<String>) -> u64`：原子递增 `version`，更新 `invalidated` 寄存器，`view_invalidated.add(1)`，发 `debug!("view.invalidate", view = view.code(), version = version)`。
    - `pub fn invalidate_all(&self) -> u64`：换身份或全量重建时按每个 `ViewKind` 推一次 `key: None` 的失效。
    - `pub async fn query(&self, query: Query) -> Result<QueryReply, &'static str>`：
      1. 查 `ready`：未就绪直接回 `Err("starting")`（发 `query.done(code = "starting")`）。
      2. 占槽位：递增该 `QuerySlot` 代号，取消旧槽位的 `CancellationToken` 并调用旧 `InterruptHandle::interrupt()`。
      3. 抢 `Semaphore` 许可（`tokio::select!` 监听 `cancel.cancelled()`：等待期间被取消立刻回 `Err("query.cancelled")`）。
      4. 记录当前 `version` 快照，通过 `tree.blocking("view.query", …)` 执行只读查询（进入时把 SQLite `Reader::interrupt_handle()` 注册进当前槽位，若此刻 `cancel.is_cancelled()` 立刻退出）；
         - `Transcript`：若 `active_recording == Some(id)` 且查询的是 `take 0`（或当前 `take`），直接用 `takes::read(&shelf, &id, 0)` + `slice_chunk` 读最新现场草稿；否则从 SQLite `Reader::take_lines_slice` 亚毫秒级读单块！
         - `Compare`：先用 `Reader::take_lines_slice` 取 `take_b` 的第 `chunk` 块（至多 64 行），再按该块 `[min_t0, max_t1]` 用 `Reader::take_lines_in_time` 取 `take_a` 重叠行，交给纯函数 `compare_lines`！
         - `Recording`：从 `Shelf::about` + `takes::list` + `FILING` + `Journal::load` + `active_refines` 组装完整 `RecordingView`。
      5. 若返回前已被新查询顶掉或 SQLite 返回中断错误 → 发 `warn!("query.done", view, code = "cancelled", ms, rows = 0usize)`，`query_cancelled.add(1)`，回 `Err("query.cancelled")`；
      6. 成功 → `query_done.add(1)`，`query_ms.note(ms)`，发 `info!("query.done", view, code = "ok", ms, rows)`，若 `ms > QUERY_BUDGET_MS` 额外发 `warn!("query.slow", view, ms)`，返回 `Ok(QueryReply { version, value })`。
  - **禁止直接暴露 `rusqlite` 错误字符串**：一律映射为 `"index"` 或 `"query.cancelled"`。

## 显式与可观测

- **命名常量**：`TRANSCRIPT_CHUNK_LINES`、`DIFF_LCS_MAX_BYTES`、`QUERY_BUDGET_MS`、`QUERY_CONCURRENT_MAX_JOBS`、`KEEP_INVALIDATIONS_EVENTS`，全部通过 `numbers.sh`。
- **任务与并发**：所有阻塞查询经 `tree.blocking("view.query", …)`，并发受 `QUERY_CONCURRENT_MAX_JOBS = 8` 信号量封顶；不新增裸 `spawn` 或无界通道。
- **事件**（同步登记进 `docs/EVENTS.md`）：
  - `query.done`（`info`；非 `ok` 时 `warn`）：`view`、`code`、`ms`、`rows`
  - `query.slow`（`warn`）：`view`、`ms`
  - `view.invalidate`（`debug`）：`view`、`version`
- **统计**：计数 `query.done`、`query.cancelled`、`view.invalidated`；分布 `query.ms`。
- **故障可见性**：取消测试断言返回 `Err("query.cancelled")`、事件里出现 `query.done(code = "cancelled")` 且 `query.cancelled >= 1`。

## 先写的测试

在 `core/tests/view.rs` 补充以下测试：

1. `transcript_chunking_splits_at_64_lines_and_handles_empty_and_out_of_range`：测试 0 行、1 行、64 行、65 行、200 行分块与越界块返回空 `lines` 及正确 `total_lines` / `total_chunks`。
2. `compare_lines_aligns_merged_sentences_and_roundtrips_diff_ops`：
   - 构造 take 0（两句短现场稿）与 take 1（一句合并并改错字的精修稿），断言对齐合并正确、`changed == true`、`DiffOp` 重建 `Equal+Delete == text_a` 且 `Equal+Insert == text_b`；
   - 测试相同行（`changed == false`）、删除行、纯零时间戳位置对齐、以及超过 `DIFF_LCS_MAX_BYTES` 的超长行不会变慢且仍满足重建不变量。
3. `invalidations_ring_coalesces_duplicates_and_caps_at_64`：连推 100 次同 `(Recording, Some("r1"))` 只留 1 条；连推 100 个不同 key 最多留 64 条且 `next` 单调递增。
4. `view_engine_executes_all_queries_and_cancels_superseded_search`：
   - 未就绪前调 `query` 返回 `Err("starting")`；
   - 就绪后依次测 `LibrarySummary`、`LibraryMonth`、`LibraryRecent`、`Search`、`Recording`、`Transcript`、`Compare`、`Glossary`、`Calendar`、`Outbound` 全部返回正确数据与递增 `version`；
   - 通过测试用阻塞钩子或连续并发多个 `Search` 验证旧查询被新查询取消（返回 `Err("query.cancelled")`、`query.cancelled` 计数增加、日志出现 `query.done(code = "cancelled")`）。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `layers.sh`、`numbers.sh`、`spawn.sh`、`bounded.sh`、`events.sh`、`words.test.ts`）。
- `core/tests/view.rs` 全部通过。

## 不做

- 不把 `ViewEngine` 接进 `Core` 或改 `app/src`（P2-05）。
- 不删 `State` 里的旧字段（P2-06）。
