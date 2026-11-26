# P1-05 地基三：有名字的任务挂在树上；丢弃要写理由

## 目标

做完以后：

1. `core/src/rt/supervise.rs` 里有 `Tree`：起任务、起阻塞调用、起线程都从它这里走，每一个都有名字、有主人；开始、结束、崩了都留事件；随时列得出现在跑着什么。
2. 从树上起的任务，把起它的人的日志接收者带着走：测试装的捕获看得见核心在任何线程上说的话。
3. 核心有一个根（`Tree::root("core", …)`）和一份 `Stats`；一个 `stats` 任务每分钟把动过的统计写进日志。
4. 规则脚本 `spawn.sh`：起任务只许在 `rt/supervise.rs` 里。现有的各处记成欠账，由后面改它们的任务还。
5. 规则脚本 `dropped.sh`：丢掉一个结果，同一行写为什么没关系。**现有的代码当场改到通过。**

这一件之后，地基齐了：P1-06 起的每件任务都只用 `zutalk_log` 的宏、`rt::stats`、`rt::queue`、`Tree`。

## 背景

- 设计在 `docs/build/phases/P1.md` §3.13.10（树）、§3.13.4（每分钟的汇总）、§3.13.12（规则脚本与欠账）、§3.6（树上有什么）。`PLAN.md` §4.4、§4.5.1 的第四、五行。
- `core/src/rt/supervise.rs`：`End`、`Part`、`Backoff`、`supervise`、`again`（P1-02 搬来的）。这一件**不改** `supervise` 和 `Backoff`（P1-09 改）。
- `zutalk_log::carry`、`carrier`（P1-03）；`rt::stats`（P1-04）。
- `core/src/app/hub.rs`：`Core::start` 里 `tokio::spawn(hub.run(mail))`；`Hub::run` 收信的循环。
- 现在起任务的地方（P1-02 之后的路径）：`app/hub.rs` 十五处、`capture/audio.rs` 三处 `thread::spawn`、`ai/write.rs` 两处和 `out/city.rs` 一处 `spawn_blocking`、`app/src/main.rs` 一处 `async_runtime::spawn`、服务器三处。
- 现在丢掉结果的地方约 45 处：`git grep -n 'let _ =\|^\s*_ = \|\.ok();\|drop(.*send' core/src app/src server/src` 看得到大部分。

## 做法

### `Tree`

接口照 `P1.md` §3.13.10。`core/Cargo.toml` 加 `tokio-util`（只要 `rt` 特性）。

- 里面是 `TaskTracker`、`CancellationToken`、这棵树的名字、`Stats`，和整棵树（连同子树）共用的一张表。`child(name)` 给一棵子树：父亲取消它也取消；它有自己的 `cancel` 和 `wait`。
- `spawn(task, work)`：
  - 表里加一行（树名、任务名、种类、什么时候起的）；`info!("task.start", tree = …, task = …)`；计数 `task.started`；高水位 `tree.tasks`。
  - 任务包三层：`zutalk_log::carry`（日志跟着走）；接住 panic（`futures_util` 的 `catch_unwind` 加 `AssertUnwindSafe`）；和树的取消 `select`。
  - 结束：表里去掉这一行；`task.end`，`code` 是 `done`、`cancelled` 或 `panic`，带 `ran_ms`。panic 用 `error!`，计数 `task.panics`；别的任务不受影响。
  - 树已经 `wait` 过（收尾了）：不起，`warn!("task.refused", …)`。
- `blocking(task, work).await -> Result<R, Broke>`：底下是 `tokio::task::spawn_blocking`，带着日志的接收者（`carrier`）；表里占一行（种类是阻塞），**不发开始和结束的事件**（写 tape 每秒十次）。里面 panic 了给回 `Err(Broke)`，不往上炸。树取消了不打断已经在跑的阻塞调用（打断不了），只是不等它。
- `cancel()`、`cancelled()`、`wait()`：`wait` 关上这棵树（之后不许再起），等它和它的子树上所有的任务结束。
- `running() -> Vec<Running>`：表的一份拷贝，`Running { tree, task, kind, for_ms }`，带 `Serialize`（自检包和快照要用）。按起的先后排。
- 表的上限 `TREE_MOST_TASKS`（64）：到了上限任务照起，只是不进表，并发一次 `stats.full`（`kind = "tasks"`）。
- **`rt::supervise::thread(name, work) -> JoinHandle<()>`**：`thread::Builder` 起一个有名字的线程，带着日志的接收者；`thread.start`、`thread.end`（`code` 是 `done` 或 `panic`，带 `ran_ms`）。线程不进树的表：它的主人是拿着 `JoinHandle` 的人。
- 用哪只钟：`for_ms`、`ran_ms` 用 `tokio::time::Instant`（暂停的时钟下是确定的）；线程用 `std::time::Instant`。
- `Tree::keep`（由 `supervise` 守着的部件）是 P1-09 的：那时 `supervise` 才有名字、状态机和上限。

### 核心的根，和每分钟的汇总

- `Core::start`：造 `Stats::new()` 和 `Tree::root("core", stats)`；hub 从根上起（`root.spawn("hub", …)`）；`Hub` 拿着根的一份克隆（后面的任务从它起子树）。hub 的循环结束时 `root.cancel()`。加 `Core::stats(&self) -> Stats`。
- `rt/stats.rs` 加 `pub async fn publish(stats: Stats, …)`：每 `STATS_SUMMARY_EVERY_MS`（60 000）拿一份 `tally()`，和上一次的比（`moved`），写一条 `info!("stats.summary", snap = 0u64, up_ms = …, rows = …)`，后面每个动过的行一条 `stat.count`、`stat.peak`、`stat.spread`、`stat.queue`（字段照 `P1.md` §3.13.3）。什么都没动：只有 `stats.summary` 一行，`rows = 0`。每次还把 `zutalk_log::lost()`、`refused()` 抄进计数 `log.lost`、`log.refused`。
- 「把一份 `Tally` 写成一组行」是一个单独的函数（`rt::stats::write(&Tally, snap)`）：P1-17 的 `stall.snapshot` 也用它。
- `Core::start` 从根上起它：`root.spawn("stats", publish(…))`。（P1-11 让它同时每秒把 `Tally` 写进 `Outlet.stats`。）

### `spawn.sh`

- 管 `core/src`、`app/src`。在 `core/src/rt/supervise.rs` 以外找：`tokio::spawn(`、`task::spawn(`、`spawn_blocking(`、`spawn_local(`、`thread::spawn(`、`thread::Builder`、`async_runtime::spawn(`、`JoinSet`。
- 欠账表和「以后」表的办法同 `bounded.sh`（P1-04）：数出来必须正好相等。照 `P1.md` §3.13.12 填：`core/src/app/hub.rs`（这一件已经还了起 hub 的那一处；其余由 P1-16、P1-19、P1-20 还，到 P1-20 为零）、`core/src/capture/audio.rs` 三处（P1-14）、`core/src/ai/write.rs` 两处、`core/src/out/city.rs` 一处（P1-21）、`app/src/main.rs` 一处（P1-23）。「以后」：`server/src/ledger.rs` 两处、`server/src/lib.rs` 一处（P4、P5）。

### `dropped.sh`，并把现有的代码改到通过

- 脚本照 `P1.md` §3.13.12，管 `core/src`、`app/src`、`server/src`、`log/src`。`select!` 里的 `_ = 某个 future =>` 不是丢弃：脚本要分得出（那一行有 `=>`）。
- 然后一处一处看现有的代码，三种处理：
  1. **真的没关系**：同一行补 `// dropped: <为什么>`。理由要是真的，比如「往 `String` 里写不会失败」「收的人没了只在应用退出时」「重复装同一个 TLS 提供者是无害的」。
  2. **其实有关系**（一个写盘的错、一个删不掉的文件被吞了）：这一件不改行为，仍然补上 `// dropped:`，但理由写实话（「删不掉就留着，下次清扫再删」），并在报告里列出来，指给会重写那一段的任务（多半是 P1-15、P1-19、P1-20、P1-21）。
  3. **写法可以不丢**：`.ok()` 换成 `if let`、把结果传回去，只在改动是一两行、明显对的时候做。
- `app/hub.rs` 里往邮箱发信的二十来处，P1-13 会全部换掉；这里先照第 1 种补理由（「邮箱没有上限，发不进去只在 hub 已经结束时」）。

## 显式与可观测

- **事件**：`task.start`、`task.end`、`task.refused`、`thread.start`、`thread.end`、`stats.summary`、`stat.count`、`stat.peak`、`stat.spread`、`stat.queue`。写进 `docs/EVENTS.md`。
- **统计**：计数 `task.started`、`task.panics`、`log.lost`、`log.refused`；高水位 `tree.tasks`。
- **数**：`TREE_MOST_TASKS`（64：P1 的树同时不到二十个任务）、`STATS_SUMMARY_EVERY_MS`（60 000：`PLAN.md` §4.5.2「每分钟」）。
- **任务**：`core` 这棵根上现在有 `hub`、`stats`。
- **欠账**：`spawn.sh` 的欠账表如上；`dropped.sh` 没有欠账。

## 先写的测试

`core/tests/supervise.rs`（加在现有的后面；装捕获；暂停的时钟，注明的除外）：

1. `a_tree_cancels_and_waits_for_all_it_started`：根上起两个任务，子树上起一个，都等着取消。`running()` 是三行，名字和树名都对。`cancel()` 再 `wait()`：三个都结束；`task.start` 三条、`task.end` 三条，`code` 都是 `cancelled`；`running()` 是空的。
2. `a_task_that_ends_on_its_own_says_done`。
3. `a_task_that_panics_is_seen_and_takes_nothing_with_it`：两个任务，一个 panic：它的 `task.end` 是 `code = "panic"`、级别 `error`；计数 `task.panics` 是 1；另一个照常跑完。
4. `a_child_can_be_cancelled_alone`：取消子树，父亲的任务还在。
5. `nothing_starts_on_a_tree_that_has_closed`：`wait()` 之后 `spawn`：没有跑，`task.refused` 一条。
6. `a_blocking_call_gives_back_what_it_made_and_a_panic_as_broke`：`blocking` 返回值；里面 panic 是 `Err`；跑着的时候 `running()` 里有它，种类是阻塞；没有 `task.start`、`task.end`。
7. `what_a_task_says_is_heard_by_whoever_started_the_tree`（多线程运行时，真实时钟）：测试线程装捕获，造根；从它 `spawn` 一个任务（在别的线程上跑），任务里发一条事件，再 `blocking` 里发一条，再 `rt::supervise::thread` 里发一条；等它们结束：三条都在捕获里。
8. `a_named_thread_says_when_it_starts_and_ends`：`thread.start`、`thread.end`；线程的名字（`std::thread::current().name()`）就是给的名字；panic 的线程 `code = "panic"`。
9. `more_tasks_than_the_table_holds_still_run`：起 `TREE_MOST_TASKS + 2` 个：都跑了；`running()` 是 64 行；`stats.full`（`kind = "tasks"`）一条。

`core/tests/stats.rs`：

10. `once_a_minute_what_moved_is_written`（暂停的时钟）：起 `publish`；加两个计数、往一个分布记三个值；过 60 虚拟秒：一条 `stats.summary`（`rows = 3`），后面三行 `stat.*`，名字和数都对。再过 60 秒什么都不动：只有一条 `stats.summary`，`rows = 0`。再动一个计数：下一分钟只有那一行。

`core/tests/app.rs`：

11. `the_core_runs_on_a_tree_with_a_name`：装捕获，`Core::start`：有 `task.start`（`tree = "core"`、`task = "hub"`）和（`task = "stats"`）各一条；`core.stats().tally()` 里计数 `task.started` 是 2。

规则脚本的证明（写进报告，不留在仓库里）：

12. `dropped.sh`：改代码之前先跑一遍，把它报出的全部贴进报告；改完通过；临时加一行 `let _ = std::fs::remove_file("x");`，它报出来；加上 `// dropped: 试` 以后不报。
13. `spawn.sh`：欠账表填好以后通过；临时在 `core/src/lib.rs` 加一行裸的 `tokio::spawn`，它报出来。

## 验收

- `scripts/build/check.sh` 全绿，`spawn.sh`、`dropped.sh` 在其中；现有的测试除了注明的以外一行没改，全部通过。
- 报告里：`dropped.sh` 改之前的输出；归到第 2 种（其实有关系）的每一处和它指给哪件任务；`spawn.sh` 的欠账表；一分钟汇总的一组真的日志行（从测试 10 的捕获里抄）。
- `git grep -n "tokio::spawn" core/src/app/hub.rs` 比改之前少一处。

## 不做

- 不改 `supervise`、`Backoff`、`again`，不加重启上限和 `Tree::keep`（P1-09）。
- 不把 hub 里别的任务、采集的线程、`ai/write.rs` 和 `out/city.rs` 的阻塞调用搬到树上（各自的任务还欠账）。
- 不加 `Outlet`、`Stats` 信号（P1-11），不写 `stall.snapshot`（P1-17）。
- 不改任何行为：`dropped.sh` 那一步只加注释，至多换一两行等价的写法。
- 不动服务器的任务。
