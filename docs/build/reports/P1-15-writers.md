状态：完成

# P1-15 落盘离开现场的路：写 tape、写草稿各隔一条有上限的队列：报告

## 改了什么

1. **`core/src/rt/keep.rs` 与 `core/src/rt.rs`**：
   - 新增泛型批量落盘部件 `rt::keep::start`（每批上限 `KEEP_BATCH_ITEMS = 50`），只依赖 `crate::rt::*` 与 `zutalk_log`，无裸 `tokio::spawn`、`spawn_blocking` 或 `thread::spawn`。
   - 调用方在调用处经 `rt::queue::bounded` 建好具名队列（让 `numbers.sh` 直接核对每条队列的带单位上限常量，如 `TAPE_QUEUE_BLOCKS`、`DRAFT_QUEUE_LINES`），把 `Rx<T>` 交给 `keep::start`；部件本身经 `Tree::keep` 挂在给定子树上，每次 `open`、每批 `write` 和收尾 `finish` 各用一次 `Tree::blocking(batch_task, …)`，一批一个阻塞调用，不跨多批。
   - 每批写完把用时（`std::time::Instant`）记进调用方传入的 `Spread`；写出错或 panic（`Tree::blocking` 返回 `Err(Broke)`）时丢弃坏掉的写入口，发 `warn!("keep.lost", part, n, why)`，计数 `keep.lost_batches` 加一，返回 `End::Retry(why)` 由 `supervise` 照 `Backoff`、`Restarts` 重开；到重启上限后由 `Tree::keep` 发 `part.gave_up` 并以 `Some(End::Fatal(why))` 结束返回的 `Kept` 句柄。
   - 发送端全部丢掉后，把队列里剩余项分批写完并执行 `finish`（同步到盘），再以 `End::Done` 结束。

2. **`core/src/capture/tape.rs`**：
   - 新增常量 `BLOCK_MS = 100`、`MINUTE_MS = 60_000`、`SILENT_PLAIN`，以及纯分类类型与函数 `Place::{Before, Next, Ahead { blocks }}`、`place(next_ms, block_ms) -> Place`。
   - `Writer` 跟踪下一块期望时间戳 `next_ms`、`dirty` 标志与待上报的补静音计数 `padded_since`：
     - `Writer::create` 在已有内容的目录上仅按 `fs::metadata().len()` 计算 `next_ms`，校验先前分钟无残块并截掉最后一个分钟文件的残尾，不解密整条 tape；
     - `Writer::append` 按 `place` 处理：落在前面则跳过；正好接上直接写；落在后面则按 `SILENT_PLAIN` 补封好的静音块（跨分钟时照旧同步完成分钟并切新文件），发 `warn!("tape.pad", from_ms, blocks)` 并累加 `padded_since`；
     - 为 `Reel` trait 增加默认方法 `sync(&mut self) -> io::Result<()>` 与 `take_padded(&mut self) -> u64`（`Writer` 转发实现）。
   - 处理 `P1-05` 报告指给本件的 `tape.rs` 丢弃 `sync_data`：正常关闭由 `keep` 的收尾回调显式调用 `Reel::sync`，`fsync` 失败会作为 `tape` 错误上报与重试；`Writer::sync` 成功后清除 `dirty`，`Drop` 仅在异常丢弃且 `dirty` 为真时做兜底同步并保留实话 `// dropped:` 理由。

3. **`core/src/record/shelf.rs`**：
   - 新增 `Draft` trait、`Drafts` 端口与默认实现 `shelf::drafts()`（与 `tape::Reel`/`Reels` 对称，便于台架给草稿写入口加闸）。
   - 新增纯值类型 `DraftGap`（配套 `GapSpan`、`DraftStep`、`Missed`），不读钟、不发事件：记录满队列时连续丢弃行的 `[t0_ms, t1_ms]` 与行数，首次丢弃标 `first = true`（调用方说一次 `draft.behind`），队列恢复后生成单行不完整空缺（`…`，`incomplete: true`）。

4. **`core/src/app/hub.rs`**：
   - 新增常量 `TAPE_QUEUE_BLOCKS = 600`、`DRAFT_QUEUE_LINES = 256`；`Config` 新增 `drafts: Drafts`（默认 `shelf::drafts()`）。
   - `Task::listen` 从根树分出 `recording = self.tree.child("recording")` 子树：
     - `tape` 队列（`TAPE_QUEUE_BLOCKS`，现场 `Full::End`，导入文件 `with(Full::Wait)`）挂 `keep::start(&recording, "tape", "tape.batch", …)`，维护计数 `tape.blocks`、`tape.padded_blocks` 与分布 `tape.batch_ms`。`sink` 闭包不再调用 `append`，只算电平、入队（现场满即 `end_once(&ended, "tape")`；文件 `send_blocking` 等空位）和发给 `audio`。
     - `draft` 队列（`DRAFT_QUEUE_LINES`，`Full::Drop`）挂 `keep::start(&recording, "draft", "draft.batch", …)`，维护计数 `draft.lines`、`draft.lost_lines` 与分布 `draft.batch_ms`。`listen` 循环不再调用 `draft.put`，经 `DraftGap` + `draft_tx.offer` 入队：满时计数并说一次 `draft.behind`，恢复后先入队一行空缺并发 `warn!("draft.gap", t0_ms, t1_ms, lines)`，行号全程连续。
     - 监听 `tape_done`：写 tape 部件到重启上限即以 `tape` 结束录音；停止时先刷出尾部待补空缺并等 `draft` 写完同步；若因 `tape` 满/失败结束则取消 `recording` 立即退出（不等卡住的磁盘），否则等 `tape` 写完同步再返回去 `seal()`。

5. **`docs/EVENTS.md` 与词表**：
   - 在 `docs/EVENTS.md` 新增 `keep.lost`、`tape.pad`、`draft.gap` 三行。
   - 核对全部 8 个 `ui/lang/*.json`：`code.tape.slow` 与 `code.draft.behind` 均已在位。

6. **台架与测试**：
   - `core/tests/bench/mic.rs`：给 `run_mic` 任务套 `zutalk_log::carry`，确保多线程测试里采集回调发的 `queue.full` 进入测试捕获。
   - `core/tests/bench/tape.rs`：`GatedReel` 转发 `sync` 与 `take_padded`。
   - `core/tests/bench/mod.rs`：新增 `GatedDraft`（无闸时单次原子读零锁开销），接入 `Bench`。
   - `core/tests/bench/service.rs`：新增 `Fault::SilentOnFinish`，用于测试停录时未定稿猜测行由 `lane::seam` 落为不完整行。
   - 新增/更新测试文件：`core/tests/keep.rs`、`core/tests/tape.rs`、`core/tests/liveness.rs`、`core/tests/app.rs`。

改动文件：`core/src/rt.rs`、`core/src/rt/keep.rs`、`core/src/capture/tape.rs`、`core/src/record/shelf.rs`、`core/src/app/hub.rs`、`docs/EVENTS.md`、`core/tests/keep.rs`、`core/tests/tape.rs`、`core/tests/liveness.rs`、`core/tests/app.rs`、`core/tests/bench/mic.rs`、`core/tests/bench/mod.rs`、`core/tests/bench/service.rs`、`core/tests/bench/tape.rs`、本报告。

## 证据

### 新增与解除挂起的测试（全部通过）

- `core/tests/keep.rs`（5 条全过，0.01s）：
  1. `what_is_queued_is_written_in_order_and_off_the_caller`
  2. `a_full_queue_says_so_at_once`
  3. `a_writer_that_breaks_is_opened_again`
  4. `a_writer_that_never_opens_gives_up_and_says_why`
  5. `closing_writes_what_is_left`
- `core/tests/tape.rs`（新增 5 条，共 12 条全过，0.69s）：
  6. `place_classifies_before_next_and_ahead`（纯函数三态表格测试）
  7. `a_hole_between_blocks_is_sealed_silence`
  8. `a_hole_across_a_minute_is_silence_in_both_files`
  9. `a_writer_opened_on_a_tape_goes_on_where_it_ends`
  10. `a_block_from_before_the_end_is_skipped`
- `core/tests/liveness.rs`（解除挂起 2 条 + 新增 3 条，快速子集 18 条全过 21.40s，`i1_eight_hours_…` 通过）：
  11. `i2_a_tape_that_stands_still_holds_no_caption_back`（去 `ignore`，含前半 `deepest > 0` / 无 `queue.full`、后半 `queue.full(tape, end)`、`recording.end(tape)`、`tape` 提示断言）
  12. `i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence`（去 `ignore`，含 `part.state(tape, Waiting, panic)`、`keep.lost(tape)`、`tape.pad` 块数等于丢批大小、`tape.padded_blocks`、`keep.lost_batches == 1`、`recording.end(done)` 断言）
  13. `the_draft_is_whole_after_a_stop`
  14. `draft_gap_table_covers_streaks_and_recovery`（`DraftGap` 纯值表格测试）
  15. `lines_the_draft_could_not_take_become_one_gap_line`（经真实 `Core` + `GatedDraft` + `Shelf` 端到端断言 `queue.full(draft, drop)`、`draft.behind` 一次、`draft.gap` 与 `draft.lost_lines` 一致、草稿空缺行起止与行号连续）
- `core/tests/app.rs`（新增 1 条通过，7.09s）：
  16. `a_long_file_waits_for_its_tape`（120 秒 = 1,200 块 > 600 块队列上限，无 `tape` 提示、无 `queue.full`，放闸后长度精确为 120,000 ms）

### `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` 真实耗时对比

```text
i1_eight_hours h1: words=7241 p50=321ms p99=321ms max=321ms
i1_eight_hours h2: words=7183 p50=321ms p99=321ms max=321ms
i1_eight_hours h3: words=7199 p50=321ms p99=321ms max=321ms
i1_eight_hours h4: words=7213 p50=321ms p99=321ms max=321ms
i1_eight_hours h5: words=7173 p50=321ms p99=321ms max=321ms
i1_eight_hours h6: words=7172 p50=321ms p99=321ms max=321ms
i1_eight_hours h7: words=7158 p50=321ms p99=321ms max=321ms
i1_eight_hours h8: words=7148 p50=321ms p99=321ms max=321ms
i1_eight_hours total: missing=0 max_frame=6114B bound_most=7495B events=4352 wall=121.89s
```

- 与 P1-12 的 `57.49s` 相比：增加 `64.40s`（288,000 块，在暂停时钟上每块 100 ms 单独触发一次 `Tree::blocking` 往返，合每块约 224 µs）。
- 全部 8 小时 `p50 = p99 = max = 321 ms`，无漂移，0 丢词，最大帧 `6114 B ≤ 8192 B`。

### 验证结果

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warnings）。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部退出码 0。
- `grep -n "append\|draft\.put" core/src/app/hub.rs`：`sink` 闭包内无 `append`（仅出现在 `keep::start` 的阻塞写回调中），`listen` 循环内无 `draft.put`。
- 定向 Rust 测试：
  - `keep`：`5 passed, 0 failed`（0.01s）
  - `tape`：`12 passed, 0 failed`（0.69s）
  - `liveness`（除 `i1_eight_hours`、`i2_background`）：`18 passed, 0 failed, 3 ignored`（21.40s）
  - `app` (`a_long_file_waits_for_its_tape`)：`1 passed, 0 failed`（7.09s）
  - `liveness` (`i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`)：`1 passed, 0 failed`（121.90s）
- 界面验证（`cd ui && npm run --silent check && npm test --silent`）：`svelte-check` 0 errors 0 warnings；`vitest` 5 files / 29 tests passed（含 `words.test.ts` 12 passed）。
- 完整门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh`：全绿（`rustfmt`、`clippy -D warnings`、全部 6 个规则脚本、`cargo test`：`passed 577, failed 0, ignored 10`、`ui`：`0 errors, 0 warnings`、`29 passed`）。期间由全量门禁捕获并修复：`tape::recover` 对尚无写入目录的 `NotFound` 返回 `Ok(0)`（与 `tape::destroy` 一致），解决损坏文件导入提前退出时 `a_file_that_holds_no_sound_leaves_nothing_behind` 残留空记录的问题。

## 留下的，要负责人的

- `TAPE_QUEUE_BLOCKS`（600）和 `DRAFT_QUEUE_LINES`（256）为临时数字（`P1.md` §4 第 3 条）；录音任务拆到 `app/recording.rs`、把 lane 与组帧搬上 `recording` 树、水位表与看守分别留给 P1-16、P1-17。
