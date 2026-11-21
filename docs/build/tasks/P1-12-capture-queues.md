# P1-12 采集：通道有上限，线程有名字，故障有事件

## 目标

做完以后：

1. 采集里的三条 `std::sync::mpsc::channel()` 换成有名字、有上限的队列；停止不再依赖队列里有位置。
2. 采集的三种线程（泵、每个声源、读文件）都有名字，从 `rt::supervise::thread` 起。
3. 设备开了、坏了、重开、放弃，时钟补了静音、判了结束，环丢了样本：每一样在日志里有一条事件。采集那一组故障测试除了「恢复了」也断言「看得见」。
4. 整个工作区里没有一条无上限的队列，而且再也加不进来：`clippy.toml` 禁掉两个无上限的构造函数（测试也守）；`bounded.sh` 再守一遍。
5. 设备回调里不分配内存，有测试守着。

这是 `PLAN.md` §7 的 I4 的前一半收尾。**不改设备那一层的规则**（`Pump::step`、cpal、`hold` 和 `held` 什么时候重开）：只换通道、起线程的办法，并把已经在发生的事说出来。

## 背景

- 每条队列的名字、上限和满了怎么办，在 `docs/build/phases/P1.md` §3.7（这一件管「设备的环」「泵的控制消息」「设备的『我坏了』」「读文件线程的邮箱」四行）；队列怎么造在 §3.13.9；线程怎么起在 §3.13.10；事件在 §3.13.3（标着 P1-12 的六行）；「看得见」在 §2 第二张表里采集那一行。
- `core/src/capture/audio.rs`（P1-01 之后）：
  - `Turner`（泵的一圈 `turn(wait)`）、`Capture::by_hand`、`Capture::with`（起泵线程）、`Capture::start`（`Source::File` 分支起读文件的线程）；
  - `hold`（一个声源自己的线程：开设备、守着、坏了照 `Backoff` 重开）、`held`；
  - 三处 `mpsc::channel()`：泵的邮箱（`by_hand` 里）、每个设备的 `ender`（`hold` 里）、读文件线程的邮箱；
  - `Capture::pause`、`resume` 往泵的邮箱发；`Drop` 发 `Stop` 再 `join`；
  - `offer(&mut ring, samples)`：设备回调做的全部的事。
- `core/src/capture/audio/file.rs` 的 `run` 收读文件线程的邮箱。
- `core/src/capture/clock.rs`：`Gap`（`Keep`、`Pad(ms)`、`End`）、`wall_clock_policy`。
- `Hear` 这个端口（P1-06）的签名里没有 `Stats`。采集要数数，所以真的那个 `Hear`（`Config::new` 给的闭包）多带一份：`Config::new` 之后由 `Core::start` 把 `Stats` 交进去，或者把 `Hear` 的签名加一个 `&Stats` 参数；选改动小的那种，台架的替身跟着改。
- 测试里的：`core/tests/live.rs` 末尾 `relay` 用了 `unbounded_channel`；`core/tests/audio.rs` 的 `import` 和 `manual` 还用 `mpsc::channel()`（P1-01 的报告里记着）；其余的用 `grep` 找。
- `core/tests/audio.rs` 采集那一组（P1-01 改成了测试自己转泵）：`a_source_that_dies_…`、`a_stall_the_pump_hears_…`、`a_source_that_cannot_be_opened_…`、`a_device_that_is_no_longer_the_right_one_…`、`paused_the_clock_stands_still_…`、`a_closed_lid_ends_…`、`a_device_that_hangs_in_the_system_…`。

## 做法

### 队列

1. 泵的邮箱：`queue::threads(&stats, "capture.pump", PUMP_MAIL_MSGS, Full::Wait)`（64）。
   - 声源线程（`hold`）发 `Feed`、`Notice`：`offer`，被退回就隔一个 `TICK_MS` 再试，采集结束了（`over`）就走。不许无限期地堵在发送上：这就是这一头的 `Full::Wait`。
   - `pause`、`resume`：用 `with(Full::Drop)` 的那一头 `offer`，被退回就丢，同一行 `// dropped: a pump that takes no messages is seen by the Capture mark`。
   - 停止：`Capture` 和 `Turner` 之间加一个原子的停止标志，`turn` 每圈都看；`Drop` 置标志，再 `offer(Stop)` 只为了把泵早点叫醒。停止不再依赖队列里有位置。
2. `ender`：`queue::threads(&stats, "capture.ended", ENDED_MSGS, Full::Drop)`（2）。发的一律 `offer`（满了说明已经有一条在路上）。两个声源各有一条，名字相同，统计里是一行。
3. 读文件线程的邮箱：`queue::threads(&stats, "capture.file", FILE_MAIL_MSGS, Full::Drop)`（8），同样的办法；停止靠原子标志。
4. 设备的环（`rtrb`）不经 `rt::queue`：它在设备回调里，不许加锁。它的上限是 `RING_MS`（2000）；`offer(&mut ring, …)` 放不下时把丢掉的样本数加进一个计数 `capture.ring.lost_samples`（`Counter::add` 是一次原子加，回调里能用；把手在开设备时拿好）。

### 线程

5. 三处 `thread::spawn` 换成 `rt::supervise::thread(name, …)`。名字：`capture.pump`、`capture.file`，声源用一张表 `SOURCE_THREADS: [&str; 2] = ["capture.source.0", "capture.source.1"]`。主人不变：`Capture` 拿着泵线程的 `JoinHandle`，`Drop` 时等它；声源线程看 `over` 自己走，没人等它（设备可能卡在系统里：`a_device_that_hangs_…` 守着这一点）。

### 事件（只说已经在发生的事）

6. 声源线程 `hold`：
   - 设备打开并交到了泵手里：`info!("capture.source.open", source = i, ms = 开了多久)`；
   - 设备坏了、要重开：`warn!("capture.source.broke", source = i, why, wait_ms = 这次退避多久)`，计数 `capture.reopens` 加一；
   - 重开也没用（`End::Fatal`）：`error!("capture.source.ended", source = i, why)`。
7. 泵（`Turner::turn`，它是 `Pump::step` 外面的那层壳；`Pump::step` 是规则，不发事件）：
   - 时钟的策略说 `Pad(ms)`：`warn!("capture.pad", ms)`，计数 `capture.padded_ms` 加上这个数；说 `End`：`error!("capture.end", why = "asleep")`（代码用现有通知里的那一个）；
   - 暂停、继续到了泵：`info!("capture.pause", paused)`；
   - 每交出一块，计数 `capture.blocks` 加一；
   - 每圈看一次 `capture.ring.lost_samples`：比上一圈多了，一条 `warn!("capture.ring.lost", source, samples = 多了多少)`。事件在泵线程上发，不在回调里发。
8. 这些事件每一条都要有实际的来历：照着 `hold`、`turn` 现有的分支加，不为了发事件去改分支。哪一种情况现在的代码分不出来（比如 `Pad` 是哪个声源的），就少带那个字段，并在报告里说。

### lint

9. 仓库根加 `clippy.toml`：

   ```toml
   disallowed-methods = [
     { path = "tokio::sync::mpsc::unbounded_channel", reason = "every queue has a bound, and says what happens when it is full (CLAUDE.md)" },
     { path = "std::sync::mpsc::channel", reason = "unbounded: use rt::queue, and say what happens when it is full" },
   ]
   ```

   确认五个 crate 都吃到了它（在每个 crate 里临时写一处违规，clippy 都要报）。`core/src/rt/queue.rs` 用的是有上限的三种构造，不受它影响。
10. `scripts/build/rules/bounded.sh` 加一条：`clippy.toml` 里那两行不在了，也以非零退出。
11. 测试跟着守：要「一条不丢」的收集器用有上限的通道，满了 panic（`try_send(..).expect(..)`），不加 `#[allow]`。`core/tests/live.rs` 的 `relay`、`core/tests/audio.rs` 的 `import` 和 `manual` 改掉。

### 采集的回调不分配

12. `core/tests/callback.rs`（单独一个测试文件，因为要换全局分配器）：`the_device_callback_allocates_nothing`。回调做的事就是 `offer(&mut ring, samples)`，包括环满了、往计数里加的那条路；断言这两种调用里都没有分配。能取到 `assert_no_alloc` 就用它（`[dev-dependencies]`）；取不到（没有网络）就在这个文件里写一个数次数的 `GlobalAlloc`，二十行以内。同一个文件里再断言 `Counter::add`、`Peak::set`、`Spread::note`、`Gauge::set` 都不分配。

## 显式与可观测

- **事件**：`capture.source.open`、`capture.source.broke`、`capture.source.ended`、`capture.pad`、`capture.end`、`capture.pause`、`capture.ring.lost`。写进 `docs/EVENTS.md`。`thread.start`、`thread.end`、`queue.full` 是地基自己发的。
- **统计**：计数 `capture.blocks`、`capture.padded_ms`、`capture.ring.lost_samples`、`capture.reopens`；三条队列各一行。
- **数**：`PUMP_MAIL_MSGS`（64：泵每 20 毫秒收一次，两个声源每次各一两条）、`ENDED_MSGS`（2）、`FILE_MAIL_MSGS`（8）、`RING_MS`（2000，已有的那个数补上名字和理由）、`TICK_MS`（P1-04 已改名）。
- **队列**：如上三条 `queue::threads`，加设备的环。
- **线程**：`capture.pump`、`capture.source.0`、`capture.source.1`、`capture.file`。
- **欠账**：`bounded.sh` 和 `spawn.sh` 里 `core/src/capture/audio.rs` 两行都删掉（各三处，还清）。
- **L0 的规矩**：设备回调里不发事件、不加锁、不分配；只碰环和一个原子计数。
- **隐私**：`source` 是第几个声源（0、1），不是设备的名字。

## 先写的测试

1. `core/tests/audio.rs`：`a_stop_reaches_a_pump_that_takes_no_messages`。`Capture::by_hand`，不转，连着 `pause`/`resume` 200 次（都不堵、不 panic），丢掉 `Capture`，再转一圈：返回 `false`。**看得见**：`queue.full`（`queue = "capture.pump"`、`policy = "drop"`）一条；统计里这条队列的 `full` 是 200 减 64 上下（确切的数照实断言）。
2. `core/tests/audio.rs`，采集那一组各加「看得见」（装捕获；声源线程从 `rt::supervise::thread` 起，所以它们说的话测试线程的捕获收得到）：
   - `a_source_that_dies_is_reported_padded_and_opened_again`：`capture.source.open` 两条（`source = 0`）；`capture.source.broke` 一条，`why` 是测试让它死的那个代码；`capture.pad` 至少一条；计数 `capture.reopens` 是 1、`capture.blocks` 是 17（P1-01 定下的块数）；
   - `a_stall_the_pump_hears_ends_the_device_on_its_own_thread`：`capture.source.broke`（`why` 是停流的那个代码）一条，之后 `capture.source.open` 又一条；
   - `a_source_that_cannot_be_opened_is_fatal_and_the_other_one_carries_on`：`capture.source.ended`（`source` 是打不开的那个）一条，级别 `error`；另一个声源有 `capture.source.open`；
   - `a_closed_lid_ends_the_capture_out_loud`：`capture.end` 一条，级别 `error`；
   - `paused_the_clock_stands_still_and_resumed_it_goes_on`：`capture.pause` 两条，`paused` 先真后假；
   - `a_device_that_hangs_in_the_system_holds_up_nothing`（真的泵线程）：`thread.start`（`thread = "capture.pump"`）和 `thread.end` 各一条；停止以后没有再来的块。
   这些测试的断言不许放松，块数照旧是确切的数。
3. `core/tests/audio.rs`：`samples_the_ring_could_not_take_are_counted_and_said`：往一个满的环里 `offer`，再转一圈泵：计数 `capture.ring.lost_samples` 是丢掉的样本数；`capture.ring.lost` 一条。
4. `core/tests/callback.rs`：见上面第 12 条。
5. 规则脚本和 lint 的证明（写进报告，不留在仓库里）：在 `core/src/lib.rs` 里临时加一行 `let _ = tokio::sync::mpsc::unbounded_channel::<u8>(); // dropped: 试`，`cargo clippy` 和 `bounded.sh` 各自的输出；去掉以后两者都过。把 `clippy.toml` 临时改名，`bounded.sh` 失败。

## 验收

- `scripts/build/check.sh` 全绿（`bounded.sh` 在其中）。
- `git grep -n "unbounded\|mpsc::channel()" -- core app server zu log ':!*.md'` 没有结果；`git grep -n "thread::spawn\|mpsc::channel\|sync_channel" core/src/capture` 没有结果。
- P1-01 的压力办法再跑一遍（12 路、每路 100 次 `audio` 测试、30 个空转进程），0 次失败：换了通道和线程的起法，采集那一组仍然是确定的。结果贴进报告。
- 报告里：哪些事件少带了字段、为什么（做法第 8 条）；`core/tests/audio.rs` 开头的手工清单有没有哪一项因为这次改动要负责人重看（应该没有；有就列出来）。

## 不做

- 不改 `Pump::step`、`wall_clock_policy`、cpal 那一层、`hold` 和 `held` 什么时候重开、退避多久。
- 不把 `hold` 的循环改写成状态机（`P1.md` §3.13.11 末尾：P1 不重写设备那一层）。
- 不给设备的重开设上限（声音要一直录）。
- 不加 tape 和草稿的队列（P1-13）。
- 不动 `broadcast(64)`（P1-14 换成 `queue::fanout`）、`ai/write.rs` 和 `out/city.rs` 里的通道（P1-19）、服务器。
