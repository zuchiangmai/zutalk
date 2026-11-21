# P1-21 后台一次一件；核心里再没有裸的任务和直接造的通道

## 目标

做完以后：

1. 后台的活（整场转写、交城、搬运）一次只做一件；一件在等着再试的时候不占着位置。
2. 每一次尝试在日志里有开始和结束；每一次「等一等再试」有一条 `job.again`：为什么、等多久、第几次。
3. `ai/write.rs`、`out/city.rs` 里读 tape 的阻塞调用从树上起，两条通道是有名字的队列。`spawn.sh`、`bounded.sh` 在 `core/src` 的欠账清零：核心里起任务只有树一条路，造通道只有 `rt::queue` 一条路。
4. I2 的三条测试都在门禁里通过，连同「看得见」的断言。

## 背景

- 设计：`docs/build/phases/P1.md` §3.8（「后台一次一件」一条）、§3.7（`write.wav`、`city.audio` 两条队列）、§3.13.3（`job.again`）、§3.13.12（欠账表）、§2 第二张表里 `i2_background_work_…` 一行。`PLAN.md` §4.1 的 L4 一行。
- `core/src/app/work.rs`（P1-19、P1-20）：`Work`、desk、`work` 子树；整场转写、交城、搬运的任务（`job.write`、`job.file`、`job.move`）已经从树上起，各有 `job.start`、`job.done`；现在它们可以同时跑。
- `core/src/rt/supervise.rs` 的 `again`：会过去的失败，先等 `AGAIN_MS`（一分钟），再越等越久，最多 `AGAIN_LONGEST_MS`（一小时）。它是一个循环，等的时候不说话。
- `core/src/ai/write.rs`：`wav(..)` 里一条 `mpsc::channel(32)` 和一个 `spawn_blocking`（把 tape 读成 WAV 流，收的一头用 `poll_recv` 包成上传的 body）；另一处 `spawn_blocking` 读 tape 的长度。
- `core/src/out/city.rs`：`parts(..)` 里一条 `mpsc::channel(1)` 和一个 `spawn_blocking`（把 tape 读成一段段 WAV）。
- `core/tests/write.rs`、`core/tests/city.rs`、`core/tests/retain.rs`：直接调这两个模块的入口；整场转写的替身是 wiremock。

## 做法

### 后台一次一件

1. `Work` 里一个 `Semaphore`，许可的个数是一个有名字的常量 `BACKGROUND_AT_ONCE_JOBS`（1：`PLAN.md` §4.1「后台任务，一次一件」）。整场转写、交城、搬运在真正干活之前拿许可，干完放掉。**每一次尝试拿一次**：`again` 在两次尝试之间的等待里不拿着许可，不然一条连不上服务的录音会把别的都挡一小时。
2. `job.start` 的 `waited_ms` 是等许可等了多久；每一次尝试是一对 `job.start`、`job.done`（同一件活的几次尝试 `seq` 相同）。`job.done` 的 `code` 是这一次尝试的结果：`ok`，或者那个失败的代码。
3. **等一等再试是一条事件**：`again` 在 `rt/supervise.rs` 里，是规则那一层，不发事件。把它拆成值和壳（照 `P1.md` §3.13.11 的写法）：一个纯函数给出「第 `n` 次失败以后等多久」（`AGAIN_MS` 起，每次加倍，到 `AGAIN_LONGEST_MS` 为止；表格测试），`again` 的循环用它，并在每次要等的时候叫一个调用者给的回调（原因、等多久、第几次）。`Work` 在回调里发 `warn!("job.again", kind, why, wait_ms, n)`。`out/city.rs` 里用 `again` 的地方照样。
4. 停止以后封 tape、写长度不是后台，不拿许可（P1-16 已经把它放在录音任务里）。

### `ai/write.rs`、`out/city.rs` 的欠账

5. 它们读 tape 的三处 `spawn_blocking` 改成 `tree.blocking(…)`（名字 `write.wav`、`write.length`、`city.audio`），所以这两个模块的入口多收一棵 `&Tree`（`Work` 把自己的子树递进去；测试给 `Tree::root("test", Stats::new())`）。`blocking` 给回的是一个要 `await` 的 future，而 `wav(..)`、`parts(..)` 现在是「起了就返回、数据从通道里来」：让它们从树上起一个任务（`tree.spawn("write.wav", …)`），任务里 `tree.blocking(..).await`。读 tape 时 panic 了（`Broke`）：通道里送一个错误，上传因此失败，这件活的 `job.done` 带着代码。
6. 两条通道改成 `queue::bounded(&stats, "write.wav", WAV_QUEUE_CHUNKS, Full::Wait)`（32）和 `queue::bounded(&stats, "city.audio", CITY_QUEUE_PARTS, Full::Wait)`（1）。读的一头等上传的一头：慢的等更慢的。`write.rs` 用的 `poll_recv`，`rt::queue::Rx` 要给得出（P1-04 留了这个口；没有就在这里补上）。
7. **除此以外这两个文件里面的做法不动**：不改上传、轮询、翻译、检查的逻辑，不给整场转写的各步加事件（`job.start`、`job.done`、`job.again` 和已有的进度够看；更细的是 P6）。这三处阻塞调用是长活的（读完整条 tape）：它们在后台的路上，不在暂停时钟的台架里跑，照旧。
8. `spawn.sh`、`bounded.sh` 的欠账表里 `core/src/` 的行全部删掉。两个脚本里只剩 `app/src/main.rs`（`spawn.sh`，P1-23）和服务器的「以后」表。

## 显式与可观测

- **事件**：`job.again`。写进 `docs/EVENTS.md`。
- **统计**：队列 `write.wav`、`city.audio` 各一行；不加新的计数。
- **数**：`BACKGROUND_AT_ONCE_JOBS`（1）、`WAV_QUEUE_CHUNKS`（32）、`CITY_QUEUE_PARTS`（1）；`AGAIN_MS`、`AGAIN_LONGEST_MS`（已有）。
- **队列**：`write.wav`、`city.audio`，都是 `Full::Wait`。
- **任务**：`work` 子树上的 `write.wav`、`city.audio`（和里面的阻塞调用）。
- **欠账**：`spawn.sh`、`bounded.sh` 在 `core/src` 清零。从此核心里新写的任何 `tokio::spawn`、`mpsc::channel` 都会被门禁拦下。
- **读日志的人**：一条连不上服务的录音，在日志里是一串「`job.start` → `job.done`（失败的代码）→ `job.again`（等多久）」，中间夹着别的活照常做完：看得出它没有挡住谁。
- **P1-05 报告里指给这一件的丢弃**（`write.rs`、`city.rs` 里发不进去就算了的那几处）：现在的理由是「上传已经结束，没有人收」：照 `rt::queue` 的返回值写清楚（`send` 给回 `Err`：没人读了）。

## 先写的测试

`core/tests/supervise.rs`：

1. `the_wait_before_trying_again_doubles_up_to_an_hour`：那个纯函数的表格：第 1 次 `AGAIN_MS`，第 2 次两倍…不超过 `AGAIN_LONGEST_MS`。
2. `trying_again_says_why_and_how_long_each_time`（暂停的时钟）：一件前两次失败（会过去的）、第三次成的活：回调被叫了两次，`n` 是 1、2，`wait_ms` 是表里的头两个数；一件失败得没救的（`Fatal`）：回调一次也没叫。

`core/tests/retain.rs` 或 `core/tests/app.rs`（真实时钟，整场转写的替身是 wiremock；装捕获）：

3. `transcripts_are_written_one_at_a_time`：启动时有三条欠着整场转写的草稿（没有人登录，自带钥匙：`hub::nobody()`）。替身给上传的回答加 200 毫秒的延迟，并记下每个请求的起止：没有两个上传在时间上重叠；三条最后都写好了。**看得见**：`job.start`、`job.done`（`kind = "write"`）在捕获里严格交替（开始、结束、开始、结束…），三对，`code` 都是 `ok`；后两条的 `waited_ms` 大于 0。
4. `one_that_waits_to_try_again_does_not_hold_up_the_next`：替身对第一条的上传回 503（会过去的失败），对第二条正常：第二条写好了，第一条的进度写着在等什么。**看得见**：`job.done`（`kind = "write"`，`code` 是那个会过去的失败的代码）一条，之后 `job.again`（`kind = "write"`、`n = 1`、`wait_ms` 等于 `AGAIN_MS`）一条；第二条的 `job.start` 在 `job.again` 之后、远早于 `AGAIN_MS` 到期。

`core/tests/write.rs`、`core/tests/city.rs`：

5. 现有的测试除了多传一棵树以外不改，全部通过。各加一条：`reading_the_tape_is_a_named_task_and_its_queue_is_counted`：整场转写（交城）做完以后，捕获里有 `task.start`（`task = "write.wav"` / `"city.audio"`）和对应的 `task.end`（`code = "done"`）；统计里那条队列的 `sent` 大于 0、`deepest` 不超过上限。
6. `a_tape_that_cannot_be_read_fails_the_job_with_a_code`：tape 的文件被截坏：这件活以现有的那个代码失败（不是挂住、不是 panic 到运行时）；`job.done` 带着它。

`core/tests/liveness.rs`：

7. `i2_background_work_runs_beside_a_recording`、`i2_a_job_that_never_ends_…`、`i2_a_tape_that_stands_still_…` 三条都不带 `ignore`，都通过。第一条「看得见」的断言（P1-08 可能把它拆成了两条，现在并回一条）：`job.done` 里搜索（`search`）、搬运（`move`）、整场转写（`write`）各至少一条，`code = "ok"`；这期间没有 `judge.verdict` 到 `Stalled`。

## 验收

- `scripts/build/check.sh` 全绿。`core/tests/liveness.rs` 里不再有 `ignore = "P1-21`、`"P1-20`。
- `git grep -n "tokio::spawn\|spawn_blocking\|thread::spawn\|mpsc::channel\|sync_channel\|broadcast::channel" -- core/src ':!core/src/rt'` 没有结果。
- 报告里：`spawn.sh`、`bounded.sh` 两张欠账表改之前、改之后的样子；测试第 4 条里那一串真的日志行（`job.start`、`job.done`、`job.again`、第二条的 `job.start`）。

## 不做

- 不改整场转写、交城、搬运本身的做法；不改 `again` 等多久。
- 不把 `ai/write.rs`、`out/city.rs` 里面写记录的地方改成阻塞线程：它们在后台的任务里，不在 hub 和现场的路上；报告里记一笔，留给 P2、P6 的审查看。
- 不让后台的活能同时做两件（那个常量是 1；要改先问负责人）。
- 不动服务器。
