状态：完成

# P1-09 `rt/`：寄存器、水位线与判断、监督的状态机和重启上限：报告

## 改了什么

1. **最新值寄存器（`core/src/rt/signal.rs`，新文件）**：
   - 新增 `Register<T>` 与 `Reading<T>`（底层封装 `tokio::sync::watch` 的 `(u64, Arc<T>)`，手动实现无 `T: Clone` 约束的 `Clone`）。
   - `Register::new` 从序号 `0` 起步；`set`（`send_modify`）原子递增序号并替换最新值；`change`（`send_if_modified`）仅在闭包返回 `Some` 时递增序号并唤醒读者；`read` 订阅当前位置。
   - `Reading::now` 立即返回当前 `(seq, Arc<T>)`；`next` 等待下一次变化，在全部 `Register` 写端丢弃后返回 `None`。
   - 零日志事件、零时钟调用、零核心业务依赖。

2. **水位线、预算表与纯判断函数（`core/src/rt/water.rs`，新文件）**：
   - 新增无锁、零堆分配的 `Gauge`（`set(at)` 单次原子写；`because(why)` 与 `ended(code)` 经固定静态槽位索引原子存取；`read()` 返回 `(u64, &'static str, Option<&'static str>)`）。
   - 新增可 `Serialize` 且带 `ts_rs::TS`（`u64` 字段均标 `#[ts(type = "number")]`，`Health` 标 `#[serde(tag = "is")]`）的 `Stage`、`Health`、`Mark`、`Water`，以及纯值类型 `Action`、`Verdict`、`Upstream`、`OnStall`、`Budget`、`Due`。
   - `Stage`、`Health`、`Action` 均提供满足 `zutalk_log::is_plain` 的 `code() -> &'static str`（`Stage::lane() -> Option<u8>` 给出 lane 序号）。
   - 新增 `BUDGETS` 常量表，严格对照 `P1.md` §3.5 九行预算（带 `_ms` 单位字段与来源注释）。
   - 新增纯函数 `sample`、`judge`、`due`：
     - `sample`：位置未变保留 `moved_ms`，位置前进或首次出现记为 `now_ms`，延续 `Water` 内的动作与快照历史；
     - `judge`：暂停时全为 `Live`；首块前 `clock_ms == 0` 时 `Capture` 为 `Live`；上游缺席（如无字幕场次的 `Draft`）时 `behind = 0`；`ended == Some("done")` 为 `Live`，其余 `Some(why)` 为 `Dead { why }` + `Action::Tell(why)`；按 `stalled_ms`、`slow_lag_ms`、`slow_idle_ms` 给出 `Stalled`、`Slow`、`Live`；
     - `due`：每次健康状态变化给一条 `Due::Changed`；同一格同一原因首次进入 `Stalled` 或 `Dead` 给一条 `Due::Snapshot`（从 `Stalled` 再进入同因 `Dead` 不重复）；同一格同一代码 `Action::Tell` 只给一次；`Action::Reopen` 在持续停滞期间每隔该格 `stalled_ms` 给一次。
   - 更新 `ui/tests/words.test.ts` 的 `coreCodes`，使表扫描覆盖 `BUDGETS`（含无点号的 `why` 字段），并在 `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json` 八个语言文件中补齐 `code.tape.slow`、`code.draft.behind`、`code.silent`、`code.shell`、`code.panic`。

3. **监督状态机、重启上限、接住 panic 与 `Tree::keep`（`core/src/rt/supervise.rs`、`core/src/live/lane.rs`、`core/src/app/hub.rs`）**：
   - 新增纯状态机 `Watch`（`Opening`、`Running`、`Waiting`、`Ended`）、`Seen`、`Effect` 与纯转换函数 `step(watch, seen, now_ms, backoff, restarts)`。
   - 新增重启上限 `Restarts`（`RESTARTS_MOST_TIMES = 8`、`RESTARTS_WINDOW_MS = 60_000`、`RESTARTS_PATIENCE_MS = 120_000`），`note(&mut self, now_ms, ran_ms) -> bool` 在窗口内满 8 次不健康运行或连续 120 秒无健康运行时触发上限，一次达到 `Backoff::HEALTHY_MS`（30 秒）的运行清空计数。
   - `supervise(part, open, broke, restarts)` 用 `AssertUnwindSafe` + `FutureExt::catch_unwind` 接住 `open()` 与 `Part::run()` 的 panic，记为 `End::Retry("panic")`；每次状态转换发 `part.state`（到 `Waiting` 为 `warn!`，其余为 `info!`），触达重启上限时发 `error!("part.gave_up", part, why, n)` 并以 `End::Fatal(why)` 结束。
   - 新增 `Tree::keep` 与可 `await` 的 `Kept` 把手：在树上以 `kind = "kept"` 起受监督部件，自动统计 `part.restarts` 与 `part.gave_up`，树取消时丢弃正在运行或等待重开的部件。
   - `core/src/live/lane.rs`：新增 `LANE_NAMES: [&str; 3]`，向 `supervise` 传入部件名与 `Restarts::default()`；`broke("panic")` 重置断句器 `Assembler` 并清空 `tail`；将 `Conn` 更新 `lane.edge` 收进 `Drop` 以确保连接因 panic 展开时也按 `feed.heard` 回退 `edge`，重连后准确标出 `why = "panic"` 的空缺；`core/src/app/hub.rs` 起 `lane::run` 时包上 `zutalk_log::carry` 使日志捕获收到 `part.state`。
   - `core/src/rt.rs` 导出 `signal` 与 `water`；`docs/EVENTS.md` 补入 `part.state` 与 `part.gave_up`。

文件：`core/src/rt.rs`、`core/src/rt/signal.rs`、`core/src/rt/water.rs`、`core/src/rt/supervise.rs`、`core/src/live/lane.rs`、`core/src/app/hub.rs`、`docs/EVENTS.md`、`ui/tests/words.test.ts`、`ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`、`core/tests/signal.rs`、`core/tests/water.rs`、`core/tests/supervise.rs`、`core/tests/liveness.rs`、本报告。

## 证据

### 1. 新增与解除挂起的测试

- `core/tests/signal.rs`（5 条全绿）：
  1. `a_register_keeps_only_the_latest_and_counts`
  2. `a_reader_that_comes_late_gets_the_latest_at_once`
  3. `a_change_that_changes_nothing_is_not_a_new_value`
  4. `two_writers_never_share_a_number`
  5. `when_the_writers_are_gone_the_reader_is_told`
- `core/tests/water.rs`（10 条全绿）：
  6. `paused_nothing_is_behind`、`before_the_first_block_the_capture_is_not_late`、`a_mark_whose_upstream_is_not_there_is_not_behind`
  7. `each_budget_at_its_edges`
  8. `a_lane_that_cannot_connect_for_fourteen_minutes_is_seen_within_its_budget`
  9. `a_service_that_takes_audio_and_says_nothing_is_reopened`
  10. `a_part_that_ended_because_the_recording_did_is_not_dead`
  11. `sampling_remembers_when_each_mark_last_moved`、`every_change_of_health_is_due_once`
  12. `the_water_of_four_columns_is_small`
- `core/tests/supervise.rs`（新增 7 条 + 原有 14 条，共 21 条全绿）：
  13. `the_watch_steps_as_its_table_says`
  14. `eight_quick_failures_in_a_minute_are_the_last`
  15. `failures_a_minute_apart_never_reach_the_cap`、`a_healthy_run_clears_the_count`
  16. `two_minutes_without_a_healthy_run_are_the_last`
  17. `a_part_that_panics_is_opened_again`
  18. `a_kept_part_is_a_task_of_the_tree_and_its_restarts_are_counted`
- `core/tests/liveness.rs`：
  - `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed`：拿掉 `#[ignore]` 并通过（第 20 秒注入 `Wire::poll_next` panic 后 5 秒内恢复出字、草稿保留崩前全部已完成句子并留下不完整 `…` 空缺行、捕获到 `part.state(part = "lane.0", to = "Waiting", why = "panic")` 1 条；`lane.link`/`lane.gap` 与 `part.restarts` 按任务说明留注释分别交接给 `P1-10` 与 `P1-16`）。
  - `nothing_said_or_secret_reaches_the_log`：补回 `opts.fault = Fault::panic_at(20_000)`，真实落盘日志下含 lane 崩溃重开的 `part.state` 事件，零内容与秘密泄露。

### 2. 纯度、规则脚本与界面词表检查

- `grep -rnE 'Instant::now|SystemTime|wall_ms|zutalk_log' core/src/rt/signal.rs core/src/rt/water.rs`：无匹配。
- 六个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）全部通过，欠账表未增加。
- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、全量门禁 `scripts/build/check.sh` 全部通过（Rust `passed 525, failed 0, ignored 14`，`ui` 0 errors / `27 passed`）。

## 留下的，要负责人的

- 本任务按范围只写 `rt/signal.rs`、`rt/water.rs`、`rt/supervise.rs` 的值与纯函数（以及 `lane` 的 `supervise` 调用），向核心其余模块的接线按计划分别在 `P1-11`（`Outlet` 寄存器）、`P1-15`（`rt::keep` 落盘部件）、`P1-16`（录音子树与水位采样）、`P1-17`（看守判断与快照）、`P1-18`（取钥匙时限与手动重连）完成。
- `BUDGETS` 表中的临时阈值与 `RESTARTS_*` 的三个重启上限常量已集中定义并标注出处，供负责人在 `P1.md` §4 第 3 条统一审阅。
