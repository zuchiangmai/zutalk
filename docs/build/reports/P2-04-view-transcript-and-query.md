状态：完成

## 改动与文件

1. **64 行分块与纯函数对比（`core/src/view/transcript.rs`）**
   - 定义命名常量（带单位与理由）：`TRANSCRIPT_CHUNK_LINES = 64`、`DIFF_LCS_MAX_BYTES = 1_024`。
   - 导出类型（`ts-rs`）：`TranscriptChunk`、`DiffOp`、`CompareRow`、`CompareChunk`。
   - 实现纯函数 `total_chunks`、`slice_chunk`、`diff_text`（前缀/后缀剥离 + `<= 1024` 字节字符级 LCS + 超长行退化为单次删增，严格满足 `Equal+Delete == a`、`Equal+Insert == b`）与 `compare_lines`（按时间区间重叠对齐两版并依 `live::lang::profile(lang).spaced` 拼接多条现场行，未重叠的删行按 `t0` 顺位插入；全零时间戳回退到位置对齐）。
2. **L3 查询协议、有界失效环与并发取消引擎（`core/src/view/query.rs`、`core/src/view.rs`）**
   - 定义命名常量：`QUERY_BUDGET_MS = 100`、`QUERY_CONCURRENT_MAX_JOBS = 8`、`KEEP_INVALIDATIONS_EVENTS = 64`。
   - 导出类型（`ts-rs`）：`ViewKind`、`Invalidate`、`Invalidations`（`push` 合并同 `(view, key)` 且上限 64 条）、`RecordingView`、`OutboundRow`、`Query`（10 种查询）、`QueryValue`（`Recording` 采用 `Option<Box<RecordingView>>` 控制枚举体积）、`QueryReply`。
   - 实现 `ViewEngine`：通过 `Tree::blocking("view.query", …)` 与 `Semaphore(QUERY_CONCURRENT_MAX_JOBS)` 并发执行全部 10 种查询；同 `QuerySlot` 新查询到达时立即取消旧 `CancellationToken` 并调用 SQLite `InterruptHandle::interrupt()`，旧查询返回 `Err("query.cancelled")`、记入 `query.cancelled` 计数并发出 `query.done(code = "cancelled")`；正常完成记入 `query.done` 与 `query.ms` 分布，超 100 ms 发出 `query.slow`；版本失效通过 `Register<Invalidations>` 发出并记入 `view.invalidated` 与 `view.invalidate` 事件。
   - `Transcript` 在命中当前活动录音的 `take 0` 时直接读现场草稿包并分块，其余走 SQLite `take_lines` 主键分块；`Compare` 先按主键取 `take_b` 当前 64 行块，再按该块时间跨度经 `idx_take_lines_time` 只取 `take_a` 重叠行交给纯函数 `compare_lines`。
3. **事件、词表、TS 绑定与测试（`docs/EVENTS.md`、`ui/lang/*.json`、`core/bindings/*.ts`、`core/tests/view.rs`）**
   - `docs/EVENTS.md` 登记 `query.done`、`query.slow`、`view.invalidate`。
   - 8 个 `ui/lang/*.json` 补齐 `code.query.cancelled`。
   - 导出 12 个新增 TS 绑定文件；`core/tests/view.rs` 新增 4 条 P2-04 测试（累计 9/9）。

## 证据

- `cargo fmt --all -- --check` 通过。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`（34/34 通过）。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 9 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`）全绿。
- `core/tests/view.rs`（9/9 通过，0.30s），其中 P2-04 新增 4 条：
  - `transcript_chunking_splits_at_64_lines_and_handles_empty_and_out_of_range`
  - `compare_lines_aligns_merged_sentences_and_roundtrips_diff_ops`（含超长行 `> DIFF_LCS_MAX_BYTES` 退化与往返不变量）
  - `invalidations_ring_coalesces_duplicates_and_caps_at_64`
  - `view_engine_executes_all_queries_and_cancels_superseded_search`（覆盖未就绪 `"starting"`、全部 10 种查询、确定性钩子触发旧 `Search` 取消并断言 `Err("query.cancelled")`、`query.cancelled >= 1` 与 `query.done(code = "cancelled")` 事件）
- `liveness::every_event_in_the_catalogue_can_be_told_apart`（1/1）与 `ui` `svelte-check`（0 errors）+ `npm test`（35/35）全绿。
- 完整门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh`：全绿（全部 9 个规则脚本、`cargo test`：`passed 694, failed 0, ignored 7`、`ui`：`0 errors, 0 warnings`、`35 passed`）。

## 遗留 / 待负责人确认

- `P2-05` 将把 `Index` 与 `ViewEngine` 接进 `Work`、`Core::query` 与外壳 `pump.rs`。
