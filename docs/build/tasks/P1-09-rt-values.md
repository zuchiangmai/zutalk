# P1-09 `rt/`：寄存器、水位线与判断、监督的状态机和重启上限

## 目标

做完以后，`core/src/rt/` 里又多了运行时的三样东西，都不认识任何业务，都能用值来测：

1. `rt/signal.rs`：`Register<T>` / `Reading<T>`，只留最新值，带序号。
2. `rt/water.rs`：`Gauge`、`Stage`、`Water`、预算表、`sample`、`judge`、`due`。
3. `rt/supervise.rs`：监督写成一个状态机；重启上限 `Restarts`；`supervise` 把 panic 当作一次中断；每次转换是一条事件；`Tree::keep` 起一个被守着的部件。

这一件只写值和它们的测试，**不接线**：没有任何现有的模块开始用它们（`lane` 用的 `supervise` 除外）。接线是 P1-11（寄存器）、P1-16（水位表）、P1-17（看守）。

## 背景

- `docs/PLAN.md` §2.3（信号）、§4.3（水位线与判断）、§4.4（恢复）、§4.2 最后一行（重启次数上限）、§4.5.1 第一行（状态是枚举，转换是函数）。
- 类型、规则、预算表的数字在 `docs/build/phases/P1.md` §3.3、§3.5、§3.6；状态机的写法在 §3.13.11；事件在 §3.13.3。照那里的名字和数字写。
- `core/src/rt/supervise.rs`：`End`、`Part`、`Backoff`、`supervise`、`again`，和 P1-05 加的 `Tree`、`thread`。`supervise` 现在封顶 8 秒后无限重试，是一个 `loop`；唯一的调用者是 `core/src/live/lane.rs` 的 `run`。
- `core/tests/supervise.rs`：原有 5 条加 P1-05 的 9 条，用暂停的时钟。
- 0.8 的事故（`PLAN.md` 附表）：取钥匙卡了 14 分钟、静默重试 39 次，录音键一直亮着。`judge` 的测试里要有这一条。

## 做法

### `rt/signal.rs`

照 `P1.md` §3.3 的签名。里面是 `tokio::sync::watch`，值是 `(u64, Arc<T>)`。

- `set`：序号加一，换掉整个值。`change(with)`：`with` 看着现在的值，返回 `Some(新值)` 才换、才加序号（用 `send_if_modified`）。两者都不阻塞，从任何线程都能调，写的人可以有好几个。
- `Reading::now()` 立刻给现在的值；`next()` 等下一次变化，写的人都没了就给 `None`。新来的读者第一次 `next()` 之前先 `now()`，不会漏掉已经在那里的值。
- 不做序列化，不带大小检查：上限由各信号类型自己的构造保证。
- **不发事件、不数数**：寄存器在现场的路上，每秒被写几十次。谁写它、因为什么写，由写的人去说。

### `rt/water.rs`

照 `P1.md` §3.5。

- `Gauge`：一个部件报告进展用的东西。三样：位置（`AtomicU64`，`set(at)` 只做一次原子写）；现在为什么不动（`because(why)`，一个 `&'static str`，可以是空的，比如 lane 正在重连的原因）；是不是已经结束了、为什么（`ended(code)`）。后两样很少变，但同样不许加锁、不许分配。`read()` 给出 `(at, why, ended)`，正好是 `sample` 要的一行。
- `Stage`、`Health`、`Mark`、`Water` 要能 `Serialize` 并由 ts-rs 导出（`Water` 是一种信号，P1-16 起发布）；`Action`、`Verdict`、`Budget` 不上线。
- `Stage`、`Health`、`Action` 各有 `code() -> &'static str`（`Stage::Heard(2)` 的代码是 `Heard`，序号由 `Stage::lane() -> Option<u8>` 给）：事件的字段要用。
- `BUDGETS`：一张 `const` 表，一格一行，数字照 `P1.md` §3.5 的表。`Budget` 的字段名带单位（`slow_lag_ms`、`slow_idle_ms`、`stalled_ms`）；表的上面一段注释说每个数的来历（`LESSONS.md` 的哪一条，或者「临时，`P1.md` §4 第 3 条」）。`Sent`、`Heard`、`Final` 的那一行对每条 lane 都适用。
- `sample(before, positions, now_ms, clock_ms, paused) -> Water`：纯函数。位置没变的格保留 `moved_ms`，变了的记成 `now_ms`；第一次见到的格记成 `now_ms`。
- `judge(water, budgets) -> Vec<Verdict>`：纯函数，规则在 `P1.md` §3.5「规则」一段。`Capture` 的上游是 `water.clock_ms`。上游那一格不在表里时这一格不落后：没有字幕的一场（`P1.md` §3.14.5）没有 lane，`Draft` 没有上游。
- `due(before, after, verdicts) -> Vec<Due>`：纯函数，说这一秒看守该做什么。「只在刚变成停滞或停了时行动一次；同一格同一个代码的 `Tell` 不说第二遍；`Reopen` 每隔一个停滞预算再来一次」都在这里；把「上次对这一格行动是什么时候」记在 `Water` 里。`Due` 除了动作，还有两样给日志的：`Changed { stage, from, to }`（这一格的健康变了：看守要发一条 `judge.verdict`）和 `Snapshot { stage }`（这一格因为这个原因第一次变成停滞或停了：看守要留一组 `stall.snapshot`；同一格同一个原因只有一次）。形状可以调整，但必须是值进值出、能用表格测。
- **规则不发事件**：`water.rs` 里没有 `zutalk_log`。发事件的是 P1-17 的看守。
- 代码字面量如果写在 `BUDGETS` 里，`ui/tests/words.test.ts` 的 `coreCodes` 现在认不出这张表：把它的正则里 `CODES|REFUSED` 扩成也认 `BUDGETS`，并在 `ui/lang/*.json` 八个文件里给 `tape.slow`、`draft.behind`、`silent`、`shell` 加词（`audio.stalled` 已有）。

### `rt/supervise.rs`

- **监督是一个状态机**（`P1.md` §3.13.11）。把现在的 `loop` 拆成值和壳：

  ```rust
  /// Where a supervised part is. `n`: how many times it has been opened.
  pub enum Watch { Opening { n: u32 }, Running { n: u32, since_ms: u64 }, Waiting { n: u32, until_ms: u64, why: &'static str }, Ended(End) }
  pub enum Seen { Opened, Stopped(End), Waited }          // what happened: it opened; `open` or `run` ended this way; the wait is over
  pub enum Effect { Open, Run, Wait(u64), Broke(&'static str), GaveUp(&'static str) }
  /// Pure: the next state, and what to do. `backoff` and `restarts` are values it steps.
  pub fn step(watch: Watch, seen: Seen, now_ms: u64, backoff: &mut Backoff, restarts: &mut Restarts) -> (Watch, Vec<Effect>);
  ```

  名字可以调整；要有：一个写出全部状态的枚举、每个状态的 `code()`、一个纯的转换函数、时间是参数。`supervise` 本身只剩一层壳：做 `step` 说的事，并在状态变了的时候发事件。
- **重启上限**：`Restarts { most_times: 8, window_ms: 60_000, patience_ms: 120_000 }`，`Default` 给这三个数；三个数是带单位的常量（`RESTARTS_MOST_TIMES`、`RESTARTS_WINDOW_MS`、`RESTARTS_PATIENCE_MS`），注释写「临时，`P1.md` §4 第 3 条」。`note(&mut self, now_ms, ran_ms) -> bool`：记下一次运行；返回 `true` 表示到上限了。到上限的两种情况：`window_ms` 之内有 `most_times` 次没撑到 `Backoff::HEALTHY_MS` 的运行；或者从开始（或从上一次健康的运行结束）起已经 `patience_ms` 没有一次健康的运行。一次健康的运行把记录清空。时间是参数。
- `supervise` 多收一个名字和一个 `Restarts`：`supervise(part: &'static str, open, broke, restarts)`。到上限时返回 `End::Fatal(最后一次的原因)`。
- `supervise` 把 `open()` 和 `part.run()` 里的 panic 接住（`futures_util` 的 `catch_unwind` 加 `AssertUnwindSafe`），当作 `End::Retry("panic")`：告诉 `broke("panic")`，照 `Backoff` 重开。`ui/lang` 加 `code.panic` 的词。
- **事件**（`P1.md` §3.13.3）：每次转换一条 `part.state`（`part`、`from`、`to`、`why`、`wait_ms`、`n`）；到 `Waiting` 的那一条用 `warn!`，其余 `info!`。到了上限另有一条 `error!("part.gave_up", part, why, n)`。被要求停下（`End::Done`）是到 `Ended` 的一条普通转换，`why = "done"`。
- **统计**：`supervise` 没有 `Stats` 可用（它是一个自由函数）；计数由 `Tree::keep` 来数：每次重开 `part.restarts` 加一，放弃时 `part.gave_up` 加一（`keep` 把自己的 `broke` 包在调用者的外面）。
- **`Tree::keep(part, open, broke, restarts)`**：在树上起一个由 `supervise` 守着的部件。它是一个任务（表里一行，种类是「被守着的」，`task.start`、`task.end` 照发）；部件结束的方式（`End`）可以被等到（返回一个能 `await` 的把手，或者收一个回调：选一种，P1-15、P1-16 都要用）。树取消时，正在跑的部件被丢掉，等待中的重开不再发生。
- `lane::run` 调 `supervise` 的地方传名字和 `Restarts::default()`。名字来自一张表 `LANE_NAMES: [&str; 3] = ["lane.0", "lane.1", "lane.2"]`（放在 `live/lane.rs`）。lane 在 `broke` 里收到 `panic` 时把断句器换成空的（崩在半路的状态不可信）；`Lane` 本身在锁里，不随连接走，所以空缺照常从 `edge` 算。lane 这时还是从裸的 `tokio::spawn` 起的（欠账，P1-16 还）：它直接调 `supervise`，不经 `Tree::keep`，所以这一件之后 `part.restarts` 对 lane 还不计数；台架那条测试的计数断言等 P1-16。
- `again`（会过去的失败越等越久地再试）这一件不动；它的事件 `job.again` 是 P1-21 的。

## 显式与可观测

- **事件**：`part.state`、`part.gave_up`。写进 `docs/EVENTS.md`。
- **统计**：计数 `part.restarts`、`part.gave_up`（由 `Tree::keep` 数）。
- **状态机**：`Watch`，有转换表测试（下面第 13 条）。`Health` 是 `judge` 的输出，`due` 说它变没变。
- **数**：`Backoff` 的三个（P1-04 已经补了注释）；`RESTARTS_*` 三个；`BUDGETS` 表。
- **队列、任务**：这一件不造队列。`Tree::keep` 起的任务名字就是部件的名字。
- **规则不发事件**：`signal.rs`、`water.rs` 里没有 `zutalk_log`、没有时钟。

## 先写的测试

`core/tests/signal.rs`：

1. `a_register_keeps_only_the_latest_and_counts`：连着 `set` 三次再读，`next()` 给第三个值，序号是 3。
2. `a_reader_that_comes_late_gets_the_latest_at_once`。
3. `a_change_that_changes_nothing_is_not_a_new_value`：`change` 返回 `None`，序号不变，等着的读者不被叫醒（暂停的时钟下用 `timeout` 证明）。
4. `two_writers_never_share_a_number`：两个线程各 `set` 一千次，最后的序号是 2000。
5. `when_the_writers_are_gone_the_reader_is_told`。

`core/tests/water.rs`（都是表格）：

6. `paused_nothing_is_behind`；`before_the_first_block_the_capture_is_not_late`：`clock_ms` 是 0、`Capture` 在 0，过了 60 秒仍是 `Live`。`a_mark_whose_upstream_is_not_there_is_not_behind`：表里只有 `Capture`、`Tape`、`Draft`、`Frame` 四格（一场只录音的），`Capture` 一直走：过了 10 分钟，`Draft`、`Frame` 仍是 `Live`，`due` 什么也不给。
7. `each_budget_at_its_edges`：对 `BUDGETS` 的每一行，在「慢」和「停滞」的界限两侧各取一点，断言健康和动作。
8. `a_lane_that_cannot_connect_for_fourteen_minutes_is_seen_within_its_budget`：`Sent(0)` 从第 0 秒起不动、`Capture` 一直走。第 19 秒是 `Slow`；第 20 秒是 `Stalled{why: "keys"}`，动作 `Tell("keys")`；之后每一秒都还是停滞，但 `due` 只在第 20 秒给出一次 `Tell`、一次 `Snapshot`。到第 840 秒，这一格的 `ended` 是 `Some("keys")`：`Dead{why: "keys"}`，`due` 给一次 `Changed`（到 `Dead`），不再给 `Tell`，**也不再给 `Snapshot`**：同一格、同一个原因只有一次。
9. `a_service_that_takes_audio_and_says_nothing_is_reopened`：`Sent(0)` 走、`Heard(0)` 不动。第 10 秒 `Stalled{why: "silent"}`、`Reopen(Heard(0))`；`due` 在第 10、20 秒各给一次 `Reopen`，`Snapshot` 只在第 10 秒。`Sent` 也不走（没有声音要送）时是 `Live`。
10. `a_part_that_ended_because_the_recording_did_is_not_dead`：`ended == Some("done")`。
11. `sampling_remembers_when_each_mark_last_moved`；`every_change_of_health_is_due_once`：一格 `Live → Slow → Stalled → Live`，`due` 正好给三次 `Changed`，`from`、`to` 都对；不变的那些秒一次也不给。
12. `the_water_of_four_columns_is_small`：4 栏（3 条 lane）的 `Water` 序列化后不超过 2048 字节。每个 `Stage`、`Health`、`Action` 的 `code()` 过得了 `zutalk_log::is_plain`。

`core/tests/supervise.rs`（加在现有的后面；装捕获）：

13. `the_watch_steps_as_its_table_says`：`step` 的转换表，对每个（状态，发生的事）断言下一个状态和效果：`Opening` 遇 `Opened` → `Running`、`[Run]`；`Opening` 遇 `Stopped(Retry(why))` → `Waiting`、`[Broke(why), Wait(退避)]`；`Running` 遇 `Stopped(Done)` → `Ended(Done)`；遇 `Stopped(Fatal(why))` → `Ended(Fatal(why))`；到了上限的 `Stopped(Retry(why))` → `Ended(Fatal(why))`、`[Broke(why), GaveUp(why)]`；`Waiting` 遇 `Waited` → `Opening`（`n` 加一）、`[Open]`。表里没有的组合有一行明说「不变」。
14. `eight_quick_failures_in_a_minute_are_the_last`：第 8 次以后 `supervise` 返回 `End::Fatal`，带着最后的原因；`open` 正好被叫了 8 次。**看得见**：`part.state` 里到 `Waiting` 的有 7 条、到 `Ended` 的 1 条；`part.gave_up` 正好一条，`n = 8`，级别 `error`。
15. `failures_a_minute_apart_never_reach_the_cap`、`a_healthy_run_clears_the_count`：没有 `part.gave_up`。
16. `two_minutes_without_a_healthy_run_are_the_last`：每次 `open` 都花 10 秒才失败（像拨不通），一分钟内到不了 8 次；第 120 秒以后结束，`part.gave_up` 一条。
17. `a_part_that_panics_is_opened_again`：脚本里第一个部件在 `run` 里 panic，第二个正常结束。`broke` 收到 `panic`，`open` 被叫了两次，间隔是 `Backoff` 的第一档。**看得见**：`part.state`（`to = "Waiting"`、`why = "panic"`、`wait_ms = 250`、`n = 1`）一条。
18. `a_kept_part_is_a_task_of_the_tree_and_its_restarts_are_counted`：`Tree::keep` 起一个会断两次再正常结束的部件：`task.start`、`task.end` 各一条，名字是部件的名字；计数 `part.restarts` 是 2；等得到它的结束方式（`Done`）。取消树时一个正在等重开的部件不再被打开。

现有的 `supervise` 测试除了多传参数以外不改，必须照旧通过。

## 验收

- `scripts/build/check.sh` 全绿。
- `core/tests/liveness.rs` 的 `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed` 如果被 P1-08 挂起了，现在拿掉 `ignore`，它通过（空缺的原因是 `panic`），并且「看得见」里这一件管的部分通过：`part.state`（`part = "lane.0"`、`to = "Waiting"`、`why = "panic"`）一条。`lane.link`、`lane.gap` 两项和计数的断言留一行注释指给 P1-10、P1-16（照 `P1.md` §2 第二张表）。
- 隐私测试里被 P1-08 留了注释的「lane 崩一次」，现在加回去。
- `core/src/rt/` 下没有 `use crate::`（`layers.sh` 守着）。
- `rt/water.rs`、`rt/signal.rs` 里没有读时钟的地方（`Instant::now`、`SystemTime`、`wall_ms`），也没有 `zutalk_log`：时间都是参数，事件由调用的人发。

## 不做

- 不把寄存器、水位表接到 hub、lane、采集上（`lane` 传名字、`Restarts::default()` 和收 `panic` 除外）。
- 不写看守，不发 `judge.verdict`、`stall.snapshot`（P1-17），不加 `Command::Reconnect`，不改取钥匙的时限（P1-18）。
- 不把 lane 搬到树上（P1-16）。
- 不加 `hdrhistogram`、`arc-swap`、`loom`、`statig`。
