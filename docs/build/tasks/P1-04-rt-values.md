# P1-04 `rt/`：寄存器、水位线与判断、重启上限

## 目标

做完以后，`core/src/rt/` 里有运行时的三样东西，都不认识任何业务，都能用值来测：

1. `rt/signal.rs`：`Register<T>` / `Reading<T>`，只留最新值，带序号。
2. `rt/water.rs`：`Gauge`、`Stage`、`Water`、预算表、`sample`、`judge`。
3. `rt/supervise.rs`：重启上限 `Restarts`；`supervise` 把 panic 当作一次中断；`Tree`。

这一件只写值和它们的测试，**不接线**：没有任何现有的模块开始用它们（`lane` 用的 `supervise` 除外）。接线是 P1-06（寄存器）、P1-10（`Tree`、水位表）、P1-11（看守）。

## 背景

- `docs/PLAN.md` §2.3（信号）、§4.3（水位线与判断）、§4.4（恢复）、§4.2 最后一行（重启次数上限）。
- 类型、规则、预算表的数字在 `docs/build/phases/P1.md` §3.3、§3.5、§3.6。照那里的名字和数字写。
- `core/src/rt/supervise.rs`：`End`、`Part`、`Backoff`、`supervise`、`again`。`supervise` 现在封顶 8 秒后无限重试，唯一的调用者是 `core/src/live/lane.rs` 的 `run`。
- `core/tests/supervise.rs`：现有 5 条，用暂停的时钟。
- 0.8 的事故（`PLAN.md` 附表）：取钥匙卡了 14 分钟、静默重试 39 次，录音键一直亮着。`judge` 的测试里要有这一条。

## 做法

### `rt/signal.rs`

照 `P1.md` §3.3 的签名。里面是 `tokio::sync::watch`，值是 `(u64, Arc<T>)`。

- `set`：序号加一，换掉整个值。`change(with)`：`with` 看着现在的值，返回 `Some(新值)` 才换、才加序号（用 `send_if_modified`）。两者都不阻塞，从任何线程都能调，写的人可以有好几个。
- `Reading::now()` 立刻给现在的值；`next()` 等下一次变化，写的人都没了就给 `None`。新来的读者第一次 `next()` 之前先 `now()`，不会漏掉已经在那里的值。
- 不做序列化，不带大小检查：上限由各信号类型自己的构造保证。

### `rt/water.rs`

照 `P1.md` §3.5。

- `Gauge`：一个部件报告进展用的东西。三样：位置（`AtomicU64`，`set(at)` 只做一次原子写）；现在为什么不动（`because(why)`，一个 `&'static str`，可以是空的，比如 lane 正在重连的原因）；是不是已经结束了、为什么（`ended(code)`）。后两样很少变，但同样不许加锁、不许分配。`read()` 给出 `(at, why, ended)`，正好是 `sample` 要的一行。
- `Stage`、`Health`、`Mark`、`Water` 要能 `Serialize` 并由 ts-rs 导出（`Water` 是一种信号，P1-10 起发布）；`Action`、`Verdict`、`Budget` 不上线。
- `BUDGETS`：一张 `const` 表，一格一行，数字照 `P1.md` §3.5 的表。`Sent`、`Heard`、`Final` 的那一行对每条 lane 都适用。
- `sample(before, positions, now_ms, clock_ms, paused) -> Water`：纯函数。位置没变的格保留 `moved_ms`，变了的记成 `now_ms`；第一次见到的格记成 `now_ms`。
- `judge(water, budgets) -> Vec<Verdict>`：纯函数，规则在 `P1.md` §3.5「规则」一段。`Capture` 的上游是 `water.clock_ms`。
- 「只在刚变成停滞或停了时行动一次；同一格同一个代码的 `Tell` 不说第二遍；`Reopen` 每隔一个停滞预算再来一次」也写成一个纯函数（比如 `due(before: &Water, after: &mut Water, verdicts: &[Verdict]) -> Vec<Action>`，把「上次对这一格行动是什么时候」记在 `Water` 里）。形状可以调整，但必须是值进值出、能用表格测。
- 代码字面量如果写在 `BUDGETS` 里，`ui/tests/words.test.ts` 的 `coreCodes` 现在认不出这张表：把它的正则里 `CODES|REFUSED` 扩成也认 `BUDGETS`，并在 `ui/lang/*.json` 八个文件里给 `tape.slow`、`draft.behind`、`silent`、`shell` 加词（`audio.stalled` 已有）。

### `rt/supervise.rs`

- `Restarts { most: 8, window_ms: 60_000, patience_ms: 120_000 }`，`Default` 给这三个数。`note(&mut self, now_ms, ran_ms) -> bool`：记下一次运行；返回 `true` 表示到上限了。到上限的两种情况：`window_ms` 之内有 `most` 次没撑到 `Backoff::HEALTHY_MS` 的运行；或者从开始（或从上一次健康的运行结束）起已经 `patience_ms` 没有一次健康的运行。一次健康的运行把记录清空。时间是参数。
- `supervise` 多收一个 `Restarts`。到上限时返回 `End::Fatal(最后一次的原因)`。
- `supervise` 把 `open()` 和 `part.run()` 里的 panic 接住（`futures_util` 的 `catch_unwind` 加 `AssertUnwindSafe`），当作 `End::Retry("panic")`：告诉 `broke("panic")`，照 `Backoff` 重开。`ui/lang` 加 `code.panic` 的词。
- `Tree`：`tokio_util::task::TaskTracker` 加 `CancellationToken` 的薄包装。要有：起一个任务（`spawn`）；起一个由 `supervise` 守着的部件（`keep(open, broke, restarts)`）；取消全部；等全部结束。P1-10 用它管一场录音。
- `lane::run` 调 `supervise` 的地方传 `Restarts::default()`。lane 在 `broke` 里收到 `panic` 时把断句器换成空的（崩在半路的状态不可信）；`Lane` 本身在锁里，不随连接走，所以空缺照常从 `edge` 算。
- `core/Cargo.toml` 加 `tokio-util`（只要 `rt` 特性）。

## 先写的测试

`core/tests/signal.rs`：

1. `a_register_keeps_only_the_latest_and_counts`：连着 `set` 三次再读，`next()` 给第三个值，序号是 3。
2. `a_reader_that_comes_late_gets_the_latest_at_once`。
3. `a_change_that_changes_nothing_is_not_a_new_value`：`change` 返回 `None`，序号不变，等着的读者不被叫醒（暂停的时钟下用 `timeout` 证明）。
4. `two_writers_never_share_a_number`：两个线程各 `set` 一千次，最后的序号是 2000。
5. `when_the_writers_are_gone_the_reader_is_told`。

`core/tests/water.rs`（都是表格）：

6. `paused_nothing_is_behind`；`before_the_first_block_the_capture_is_not_late`：`clock_ms` 是 0、`Capture` 在 0，过了 60 秒仍是 `Live`。
7. `each_budget_at_its_edges`：对 `BUDGETS` 的每一行，在「慢」和「停滞」的界限两侧各取一点，断言健康和动作。
8. `a_lane_that_cannot_connect_for_fourteen_minutes_is_seen_within_its_budget`：`Sent(0)` 从第 0 秒起不动、`Capture` 一直走。第 19 秒是 `Slow`；第 20 秒是 `Stalled{why: "keys"}`，动作 `Tell("keys")`；之后每一秒都还是停滞，但 `due` 只在第 20 秒给出一次 `Tell`。到第 840 秒，这一格的 `ended` 是 `Some("keys")`：`Dead{why: "keys"}`，而 `due` 不再给 `Tell`：同一格、同一个代码只说一次。
9. `a_service_that_takes_audio_and_says_nothing_is_reopened`：`Sent(0)` 走、`Heard(0)` 不动。第 10 秒 `Stalled{why: "silent"}`、`Reopen(Heard(0))`；`due` 在第 10、20 秒各给一次。`Sent` 也不走（没有声音要送）时是 `Live`。
10. `a_part_that_ended_because_the_recording_did_is_not_dead`：`ended == Some("done")`。
11. `sampling_remembers_when_each_mark_last_moved`。
12. `the_water_of_four_columns_is_small`：4 栏（3 条 lane）的 `Water` 序列化后不超过 2048 字节。

`core/tests/supervise.rs`（加在现有的后面）：

13. `eight_quick_failures_in_a_minute_are_the_last`：第 8 次以后 `supervise` 返回 `End::Fatal`，带着最后的原因；`open` 正好被叫了 8 次。
14. `failures_a_minute_apart_never_reach_the_cap`、`a_healthy_run_clears_the_count`。
15. `two_minutes_without_a_healthy_run_are_the_last`：每次 `open` 都花 10 秒才失败（像拨不通），一分钟内到不了 8 次；第 120 秒以后结束。
16. `a_part_that_panics_is_opened_again`：脚本里第一个部件在 `run` 里 panic，第二个正常结束。`broke` 收到 `panic`，`open` 被叫了两次，间隔是 `Backoff` 的第一档。
17. `a_tree_cancels_and_waits_for_all_it_started`。

现有的 5 条 `supervise` 测试除了多传一个参数以外不改，必须照旧通过。

## 验收

- `scripts/build/check.sh` 全绿。
- `core/tests/liveness.rs` 的 `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed` 如果被 P1-03 挂起了，现在拿掉 `ignore`，它通过（空缺的原因是 `panic`）。
- `core/src/rt/` 下没有 `use crate::`（`layers.sh` 守着）。
- `rt/water.rs`、`rt/signal.rs` 里没有读时钟的地方（`Instant::now`、`SystemTime`、`wall_ms`）：时间都是参数。

## 不做

- 不把寄存器、水位表、`Tree` 接到 hub、lane、采集上（`lane` 传 `Restarts::default()` 和收 `panic` 除外）。
- 不写看守（P1-11），不加 `Command::Reconnect`，不改取钥匙的时限。
- 不加 `hdrhistogram`、`arc-swap`、`loom`。
