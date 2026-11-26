状态：完成

# P1-04 地基二：统计和有名字的队列；数都有名字和单位：报告

## 改了什么

本任务由两次提交组成：前置提交 `0a77cc1` 在额度中断前完成了 `numbers.sh` 与全仓时限常量的单位化改名，本次提交补完 `rt/stats.rs`、`rt/queue.rs`、`bounded.sh`、`docs/EVENTS.md` 及全部测试。

1. **`core/src/rt/stats.rs`**：
   - `Stats`（可克隆、每核一份，两个 `Stats::new()` 互不可见）：`counter(name)`、`peak(name)`、`spread(name)` 以及 crate 内的 `queue(name, capacity)`。取把手时加锁一次，之后 `Counter::add`、`Peak::set`、`Spread::note` 均为无锁无分配原子操作。
   - 固定 13 桶分布：`SPREAD_EDGES = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 2_000, 5_000]` 加溢出桶；公开纯函数 `bucket(value)` 与 `quantile(buckets, most, pct)`（返回所在桶上沿，溢出桶返回 `most`）。
   - 行数与名称上限：`STATS_MOST_COUNTS = 40`、`STATS_MOST_PEAKS = 4`、`STATS_MOST_SPREADS = 12`、`STATS_MOST_QUEUES = 16`、`STATS_NAME_MOST_BYTES = 32`；超限或超长按种类发一次 `warn!("stats.full", kind = …)`，仍返回可正常写入但不入表的独立把手。
   - `tally() -> Tally` 按名称排序输出确定快照；`Tally` 及四类行结构体派生 `Serialize`、`TS`、`PartialEq`、`Clone`、`Default`；公开纯函数 `moved(before, after) -> Tally` 只留新增或变化的行。
2. **`core/src/rt/queue.rs`**：
   - 三个通道构造函数：`bounded`（`tokio::sync::mpsc`）、`threads`（`std::sync::mpsc::sync_channel`）、`fanout`（`tokio::sync::broadcast`）；导出 `Full`（`Wait`/`Drop`/`Reject`/`End` 及其 `code()`）和 `Back`（`Full`/`Closed`）。
   - 每条队列按名称在 `Stats` 登记一行（同名队列共用同一行）；`Tx`/`ThreadTx` 提供 `offer`、`send`/`send_blocking`、`with(full)`；`Rx` 提供 `recv`、`try_recv`、`recv_many`、`poll_recv`、`close`；`ThreadRx` 提供 `recv`、`recv_timeout`、`try_recv`；`Fanout`/`FanoutRx` 提供 `send`、`subscribe`、`recv`、`try_recv`、`len`、`is_empty`、`resubscribe`。
   - 原子边沿触发事件：从可入队首次变为满时发一条 `warn!("queue.full", queue, policy, capacity)`，连续满不重复发；下一次成功入队（或 fanout 恢复读取）时发一条 `info!("queue.clear", queue, lost)`。`send` 等待空位只计 `full`，不计 `lost`、不发 `queue.full`。
3. **`core/src/rt.rs`**：导出 `pub mod queue;`、`pub mod stats;`。
4. **`docs/EVENTS.md`**：新增 `queue.full`、`queue.clear`、`stats.full` 三行。
5. **`scripts/build/rules/numbers.sh` 与全仓时限常量改名**（前置提交 `0a77cc1`）：覆盖 `core/src`、`app/src`、`server/src`、`log/src`，消除全部 `Duration::from_secs` 与 `Duration` 常量，为带单位常量补齐注释；行为零变化。
6. **`scripts/build/rules/bounded.sh`**：守护 `core/src`、`app/src`、`server/src`，除 `core/src/rt/queue.rs` 外禁止直接构造通道、除 `core/src/capture/audio.rs` 外禁止 `RingBuffer::new`，用精确匹配欠账表和「以后」表防新增、催还清。

文件：`core/src/rt.rs`、`core/src/rt/stats.rs`、`core/src/rt/queue.rs`、`core/tests/stats.rs`、`core/tests/queue.rs`、`scripts/build/rules/bounded.sh`、`docs/EVENTS.md`、本报告（以及前置提交 `0a77cc1` 中的 `scripts/build/rules/numbers.sh` 和 12 个源码文件）。

## 改了名字与补齐常量的清单（旧名 → 新名）

| 文件 | 旧写法 | 新常量 | 值（ms） |
|---|---|---|---|
| `app/src/main.rs` | `LOOK: Duration` | `LOOK_MS: u64` | `21_600_000` |
| `core/src/ai/write.rs` | `POLL: Duration` | `POLL_MS: u64` | `2_000` |
| `core/src/app/hub.rs` | `FRAME: Duration` | `FRAME_MS: u64` | `100` |
| `core/src/app/hub.rs` | `SLOT: Duration` | `SLOT_MS: u64` | `250` |
| `core/src/capture/audio.rs` | `TICK: Duration` | `TICK_MS: u64` | `20` |
| `core/src/id/keys.rs` | `from_secs(30)` | `CREATE_WAIT_MS: u64` | `30_000` |
| `core/src/id/keys.rs` | `from_secs(1)` | `SIGN_IN_POLL_MS: u64` | `1_000` |
| `core/src/id/keys.rs` | `300_000` | `SIGN_IN_WAIT_MS: u64` | `300_000` |
| `core/src/id/keys.rs` | `map(Duration::from_secs)` | `MS_PER_S: u64`（换算后 `from_millis`） | `1_000` |
| `core/src/live/lane.rs` | `PATIENCE: Duration` | `PATIENCE_MS: u64` | `10_000` |
| `core/src/live/lane.rs` | `KEEPALIVE: Duration` | `KEEPALIVE_MS: u64` | `10_000` |
| `core/src/live/lane.rs` | `TAIL: Duration` | `TAIL_MS: u64` | `5_000` |
| `core/src/live/lane.rs` | `from_secs(60)` | `HAND_OVER_LEAD_MS: u64` | `60_000` |
| `core/src/live/lane.rs` | `from_secs(15)` | `HAND_OVER_MARGIN_MS: u64` | `15_000` |
| `core/src/rt/supervise.rs` | `AGAIN: Duration` | `AGAIN_MS: u64` | `60_000` |
| `core/src/rt/supervise.rs` | `from_secs(3600)` | `AGAIN_LONGEST_MS: u64` | `3_600_000` |
| `server/src/ledger.rs` | `from_secs(10)`（两处） | `UPSTREAM_WAIT_MS: u64` | `10_000` |
| `server/src/ledger.rs` | `from_secs(3600)` | `OPENROUTER_KEY_LIFE_MS: u64` | `3_600_000` |
| `server/src/lib.rs` | `from_secs(60)` | `SWEEP_EVERY_MS: u64` | `60_000` |
| `server/src/main.rs` | `from_secs(10)` | `DIRECTORY_WAIT_MS: u64` | `10_000` |

另有既有带单位常量补齐注释：`core/src/capture/audio.rs` 的 `ZERO_RUN_MS`、`core/src/live/lane.rs` 的 `BLOCK_MS` 和 `BEHIND_MS`、`core/src/record/retain.rs` 的 `DAY_MS`、`core/src/rt/supervise.rs` 的 `FIRST_MS`/`LONGEST_MS`/`HEALTHY_MS`。

### 注释写为「来历待查」（`Origin to be traced`）的常量

- `app/src/main.rs`：`LOOK_MS`（6 小时）
- `core/src/app/hub.rs`：`SLOT_MS`（250 ms）
- `core/src/capture/audio.rs`：`ZERO_RUN_MS`（10 000 ms）
- `core/src/id/keys.rs`：`CREATE_WAIT_MS`（30 000 ms）、`SIGN_IN_POLL_MS`（1 000 ms）、`SIGN_IN_WAIT_MS`（300 000 ms）
- `core/src/rt/supervise.rs`：`Backoff::FIRST_MS`（250 ms）、`Backoff::HEALTHY_MS`（30 000 ms）
- `server/src/ledger.rs`：`UPSTREAM_WAIT_MS`（10 000 ms）、`OPENROUTER_KEY_LIFE_MS`（3 600 000 ms）
- `server/src/lib.rs`：`SWEEP_EVERY_MS`（60 000 ms）
- `server/src/main.rs`：`DIRECTORY_WAIT_MS`（10 000 ms）

## `bounded.sh` 欠账表

| 类别 | 文件 | 处数 | 谁还 |
|---|---|---|---|
| 欠账 | `core/src/app/hub.rs` | 6 | `P1-13`（邮箱 4 处）、`P1-16`（音频 `broadcast` 与 lane `mpsc` 2 处） |
| 欠账 | `core/src/capture/audio.rs` | 3 | `P1-14` |
| 欠账 | `core/src/ai/write.rs` | 1 | `P1-21` |
| 欠账 | `core/src/out/city.rs` | 1 | `P1-21` |
| 以后 | `server/src/room.rs` | 1 | `P5` |

## 证据

### 新测试（16 条全过）

- `core/tests/stats.rs`（9 条）：
  - `a_name_is_one_row_however_often_it_is_asked_for`
  - `a_peak_keeps_the_most_it_has_been`
  - `a_spread_puts_each_value_in_its_bucket`
  - `the_quantile_is_the_upper_edge_of_its_bucket`
  - `counting_takes_no_lock_and_no_memory`
  - `more_rows_than_there_is_room_for_are_said_once_and_left_out`
  - `the_fullest_tally_fits_one_channel_message`
  - `only_what_moved_is_in_the_difference`
  - `two_stats_do_not_see_each_other`
- `core/tests/queue.rs`（7 条）：
  - `a_full_queue_hands_back_says_so_once_and_counts`
  - `a_sender_that_waits_gets_everything_through_in_order`
  - `another_kind_of_sender_has_its_own_answer`
  - `nobody_reading_is_not_full`
  - `a_queue_read_by_a_thread_is_the_same_queue`
  - `a_reader_that_lags_skips_ahead_and_the_loss_is_counted`
  - `the_same_name_is_the_same_row`

### `numbers.sh` 改代码之前的输出（基线 `48779f9`）

```text
app/src/main.rs:22: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
app/src/main.rs:22: LOOK is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/ai/write.rs:59: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/ai/write.rs:59: POLL is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/app/hub.rs:34: Duration::from_millis takes a constant ending in _MS or a value named …_ms, not a number or a sum
core/src/app/hub.rs:36: Duration::from_millis takes a constant ending in _MS or a value named …_ms, not a number or a sum
core/src/app/hub.rs:34: FRAME is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/app/hub.rs:36: SLOT is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/capture/audio.rs:23: Duration::from_millis takes a constant ending in _MS or a value named …_ms, not a number or a sum
core/src/capture/audio.rs:23: TICK is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/capture/audio.rs:29: ZERO_RUN_MS has a unit and no reason: say on its line or the one above why it is that number
core/src/id/keys.rs:222: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/id/keys.rs:309: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/id/keys.rs:374: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/live/lane.rs:24: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/live/lane.rs:26: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/live/lane.rs:28: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/live/lane.rs:179: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/live/lane.rs:179: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/live/lane.rs:24: PATIENCE is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/live/lane.rs:26: KEEPALIVE is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/live/lane.rs:28: TAIL is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/live/lane.rs:22: BLOCK_MS has a unit and no reason: say on its line or the one above why it is that number
core/src/live/lane.rs:33: BEHIND_MS has a unit and no reason: say on its line or the one above why it is that number
core/src/record/retain.rs:10: DAY_MS has a unit and no reason: say on its line or the one above why it is that number
core/src/rt/supervise.rs:66: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/rt/supervise.rs:80: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/rt/supervise.rs:60: Duration::from_millis takes a constant ending in _MS or a value named …_ms, not a number or a sum
core/src/rt/supervise.rs:66: AGAIN is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/rt/supervise.rs:33: FIRST_MS has a unit and no reason: say on its line or the one above why it is that number
core/src/rt/supervise.rs:34: LONGEST_MS has a unit and no reason: say on its line or the one above why it is that number
core/src/rt/supervise.rs:35: HEALTHY_MS has a unit and no reason: say on its line or the one above why it is that number
server/src/ledger.rs:258: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
server/src/ledger.rs:274: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
server/src/ledger.rs:331: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
server/src/lib.rs:213: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
server/src/main.rs:22: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
```

### 规则脚本证明（试完均已还原）

```text
=== numbers.sh violation proof ===
core/src/lib.rs:17: Duration::from_secs: time is in ms; write Duration::from_millis(<NAME>_MS)
core/src/lib.rs:18: Duration::from_millis takes a constant ending in _MS or a value named …_ms, not a number or a sum
core/src/lib.rs:18: X is a Duration constant: make it a number with its unit (<NAME>_MS: u64)
core/src/lib.rs:19: Y_MS has a unit and no reason: say on its line or the one above why it is that number
exit=1
=== bounded.sh new-channel proof ===
core/src/lib.rs:17: channel built outside core/src/rt/queue.rs (mpsc::channel::<u8>()
exit=1
=== bounded.sh inflated-debt proof ===
core/src/out/city.rs: 1 channel uses, debt says 2: lower its row in scripts/build/rules/bounded.sh to 1
exit=1
```

### 其他验收项

- `git grep -n 'Duration::from_secs\|: Duration =' core/src app/src server/src log/src`：无输出。
- `layers.sh`、`events.sh`、`numbers.sh`、`bounded.sh` 全部退出码 0。
- `scripts/build/check.sh` 全绿（Rust `passed 464, failed 0, ignored 6`，界面 `27 passed`）。

## 留下的，要负责人的

- 上述 12 个标为 `Origin to be traced` 的既有常量数值，待负责人按 `P1.md` §4 第 3 条一并确认。
- 现有通道的替换按计划留给 `P1-13` 至 `P1-21`，`clippy.toml` 的 `disallowed-methods` 留给 `P1-14`。
