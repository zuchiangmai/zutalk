# P1-04 地基二：统计和有名字的队列；数都有名字和单位

## 目标

做完以后：

1. `core/src/rt/stats.rs`：计数、高水位、固定桶的分布，一个核心一份，随时读得出一份大小有上限的 `Tally`。
2. `core/src/rt/queue.rs`：队列只从这里造。每条队列有名字、有上限、写明这一种发信人遇到满了怎么办；深度和满的次数自动进统计，满了自动留一条事件。
3. 规则脚本 `numbers.sh`：时间只有毫秒一个单位，时限和上限都是带单位的常量，旁边有理由。**现有的代码当场改到通过。**
4. 规则脚本 `bounded.sh`：通道只许在 `rt/queue.rs` 里造。现有的几处记成欠账，由后面改它们的任务还。

这一件只建东西和守规矩，**不把现有的通道换成新的队列**：那是 P1-13 至 P1-21 各自的事。

## 背景

- 设计在 `docs/build/phases/P1.md` §3.13.4（统计）、§3.13.9（队列）、§3.13.12（规则脚本与欠账）、§3.7（每条队列的名字和上限）。`PLAN.md` §4.5.1 的前三行、§4.5.2 第二条。
- `zutalk-log`（P1-03）：`info!`、`warn!`、`capture`。
- `core/src/rt/`（P1-02 之后）：只有 `supervise.rs`。`rt` 不用本 crate 的任何模块（`layers.sh`）。
- 现在的通道（P1-02 之后的路径）：`app/hub.rs` 的 `unbounded_channel`（邮箱）、`broadcast::channel(64)`、`mpsc::channel(64)`；`capture/audio.rs` 三处 `mpsc::channel()`；`ai/write.rs` 的 `mpsc::channel(32)`；`out/city.rs` 的 `mpsc::channel(1)`；服务器 `room.rs` 的 `broadcast::channel(256)`。
- 现在的时限：`live/lane.rs` 的 `PATIENCE`、`KEEPALIVE`、`TAIL`（`Duration` 常量）和 `hand_over` 附近的 `from_secs(60)`、`from_secs(15)`；`capture/audio.rs` 的 `TICK`；`app/hub.rs` 的 `FRAME`、`SLOT`、`from_secs(3600)`；`rt/supervise.rs` 的 `AGAIN`（P1-02 搬来的）；`ai/write.rs` 的 `POLL`；`id/keys.rs` 的 `from_secs(30)`、`from_secs(1)` 和 `session_cap_s.map(Duration::from_secs)`；`app/src/main.rs` 的 `LOOK`；服务器 `ledger.rs` 三处、`lib.rs` 一处、`main.rs` 一处。

## 做法

### `rt/stats.rs`

照 `P1.md` §3.13.4 的接口。

- `Stats` 是一个可以克隆的把手（里面一个 `Arc`）。`counter(name)`、`peak(name)`、`spread(name)` 按名字找行：有就给同一行，没有就加。找的时候加一把锁；拿到的把手（`Counter`、`Peak`、`Spread`）之后每次只是原子操作。
- `Counter::add(n)`；`Peak::set(now)`（记现在的值，并留下最大的）；`Spread::note(value)`（落进 `SPREAD_EDGES` 的哪个桶，那个桶加一；另记个数和最大值）。都不分配、不加锁。
- 队列的那一行（上限、现在多深、最深、放进去多少、遇到满多少次、丢了多少）由 `rt/queue.rs` 写，`Stats` 只管存和读。
- **上限**：计数 `STATS_MOST_COUNTS`（40）、高水位 `STATS_MOST_PEAKS`（4）、分布 `STATS_MOST_SPREADS`（12）、队列 `STATS_MOST_QUEUES`（16）；名字不超过 `STATS_NAME_MOST_BYTES`（32）。超过了：这一种发一次 `warn!("stats.full", kind = …)`，给回一个不进表的把手（照样能加，只是读不到）。名字太长同样处理。
- `tally() -> Tally`：一份拷贝，按名字排序（次序确定）。`Tally` 和它的行带 `Serialize`、`TS`、`PartialEq`、`Clone`、`Default`：它是一种信号（P1-11 起发布）。分布的行给 `n`、`p50`、`p99`、`most`；分位数取所在桶的上沿，是一个公开的纯函数。
- `moved(before, after) -> Tally`：纯函数，只留变了的行。P1-05 的每分钟汇总用它。
- 这一件里，这个文件没有时钟，也不起任务（每分钟的汇总是 P1-05 加的那个 `publish`）。

### `rt/queue.rs`

照 `P1.md` §3.13.9 的接口。三个构造函数是对 `tokio::sync::mpsc`、`std::sync::mpsc::sync_channel`、`tokio::sync::broadcast` 的薄包装；这个文件是全仓库唯一造通道的地方，开头用 `#![expect(clippy::disallowed_methods, reason = …)]` 之类的办法为 P1-14 的 lint 留好位置（现在 lint 还没开，先不加也行，但别让 P1-14 再回来改结构）。

- 每条队列造的时候在 `Stats` 里拿一行（按名字；同名的第二条队列用同一行：一场场录音的 `tape` 队列是同一行）。
- 发的一头：
  - `offer(value)`：从不等。放进去了：`sent` 加一，深度加一，更新最深。满了：`full` 加一，把东西还回来（`Back::Full`）；没人收了：`Back::Closed`。
  - `send(value).await`、`send_blocking(value)`：等位置。等过的记一次 `full`，但不算「满了」的事件。没人收了把东西还回来。
  - `with(full)`：同一条队列，给另一种发信人，带它自己的 `Full`。
- 收的一头：包住底下那种通道原有的收法（`recv`、`try_recv`、`recv_many`、`poll_recv`；线程那种是 `recv_timeout`、`try_recv`），每收到一个深度减一。
- `fanout`：`send` 从不等（底下的 `broadcast` 自己顶掉最旧的）；每个读者落后被跳过时，`lost` 加上跳过的个数，并当作这条队列「满了」。读者要的 `len()`、`resubscribe()` 照给（lane 用它们判断自己落后）。
- **事件**（`P1.md` §3.13.3）：一条队列从「上一次放进去了」变成「这一次被退回」时发一条 `warn!("queue.full", queue = name, policy = full.code(), capacity = …)`，之后连着被退回不再发；下一次放进去时发一条 `info!("queue.clear", queue = name, lost = 这一段里退回的个数)`。「上一次怎么样」是一个原子标志：发事件这件事不许让 `offer` 加锁。
- `Full` 有 `code()`：`wait`、`drop`、`reject`、`end`。它不改变 `offer` 和 `send` 的行为（见 §3.13.9 第二条）。
- 这个文件里没有时钟，没有任务。

### `numbers.sh`，并把现有的代码改到通过

脚本照 `P1.md` §3.13.12 的五条，管 `core/src`、`app/src`、`server/src`、`log/src`。只用 `grep`、`sed`、`awk`、`perl`，要快。单位表写在脚本开头，一行一个。

然后改代码（只改名字和单位，**行为一点不变**）：

- 每个 `Duration` 常量改成带 `_MS` 的 `u64`，用的地方 `Duration::from_millis(X_MS)`：`PATIENCE_MS`、`KEEPALIVE_MS`、`TAIL_MS`、`TICK_MS`、`FRAME_MS`、`SLOT_MS`、`AGAIN_MS`、`POLL_MS`、`LOOK_MS`…名字照原来的加单位。
- 调用处直接写的数都起名字、加注释：`lane.rs` 交接的两个时限；`hub.rs`（现在可能已经在 `rt/supervise.rs` 的 `again` 里）的一小时上限；`id/keys.rs` 的三处（其中 `session_cap_s` 是协议里以秒计的字段：乘一个有名字的 `MS_PER_S` 再 `from_millis`）；服务器的五处。
- 现有的以单位结尾的常量（比如 `BEHIND_MS`、`LOOK_MS`、`Backoff` 的三个）缺注释的补上：为什么是这个数。说不出来历的，写「0.8 沿用」或者指向 `LESSONS.md` 的哪一条；真的查不到，就在报告里列出来，注释写「来历待查」，不许编。
- 测试里的数不归这个脚本管。

### `bounded.sh`

- 管 `core/src`、`app/src`。在 `core/src/rt/queue.rs` 以外找：`mpsc::channel(`、`sync_channel(`、`broadcast::channel(`、`unbounded_channel`、`UnboundedSender`、`UnboundedReceiver`；在 `core/src/capture/audio.rs` 以外找 `RingBuffer::new`。
- **欠账表**在脚本里，一行「文件、几处、谁还」。照 `P1.md` §3.13.12 的表填，处数以脚本此刻数出来的为准：`core/src/app/hub.rs`（P1-13 还邮箱，P1-16 还其余）、`core/src/capture/audio.rs`（P1-14）、`core/src/ai/write.rs`、`core/src/out/city.rs`（P1-21）。
- **「以后」表**：`server/src/room.rs` 一处（P5）。同样要数出来正好相等。
- 每个文件数出来的处数必须**正好等于**表里的数。多了：打印新犯的「文件:行」。少了：打印「把这一行改成 N」或「删掉这一行」。两种都以非零退出。
- `tests/` 不归它管：测试里的通道由 P1-14 的 clippy 规则守（只禁无上限的）。

## 显式与可观测

- **事件**：`queue.full`、`queue.clear`、`stats.full`。三行写进 `docs/EVENTS.md`。
- **统计**：这一件建的就是它。P1 的全部名字在 `P1.md` §3.13.4 的表里；这一件自己不数业务上的数。
- **数**：`SPREAD_EDGES`、五个 `STATS_*` 上限。上限的理由是那条「最坏的一份放得进 8192 字节」的测试；`SPREAD_EDGES` 的理由是 1–2–5 的刻度从 1 到 5000，够分清「一毫秒以内」「一秒以内」「几秒」。
- **队列、任务**：这一件不造任何一条真的队列，不起任务。
- **欠账**：`numbers.sh` 没有欠账（当场还清）；`bounded.sh` 的欠账表如上。

## 先写的测试

`core/tests/stats.rs`：

1. `a_name_is_one_row_however_often_it_is_asked_for`：同一个名字拿两次计数的把手，各加几次：`tally()` 里是一行，数是和。
2. `a_peak_keeps_the_most_it_has_been`；`a_spread_puts_each_value_in_its_bucket`（界限两侧：0、1、2、3、5000、5001）；`the_quantile_is_the_upper_edge_of_its_bucket`（表格）。
3. `counting_takes_no_lock_and_no_memory`：八个线程各对同一个计数加一万次：最后是八万。（「不分配」由 P1-14 的回调测试守。）
4. `more_rows_than_there_is_room_for_are_said_once_and_left_out`：装捕获；要第 41 个、第 42 个计数：`stats.full`（`kind = "counts"`）正好一条；`tally()` 里是 40 行；多出来的把手照样能 `add`。
5. `the_fullest_tally_fits_one_channel_message`：四种行都到上限，名字 32 字节，每个数 `1 << 40`：`serde_json::to_vec(&tally).len() <= 8192`。
6. `only_what_moved_is_in_the_difference`：`moved` 的表格测试（加了一行、一行的数变了、什么都没变）。
7. `two_stats_do_not_see_each_other`：两个 `Stats::new()` 用同一个名字，各数各的。

`core/tests/queue.rs`（装捕获；异步的用暂停的时钟）：

8. `a_full_queue_hands_back_says_so_once_and_counts`：上限 2，`Full::Reject`，连着 `offer` 五个：前两个进，后三个 `Back::Full`，拿回来的正是给的。`queue.full` 正好一条（`queue`、`policy = "reject"`、`capacity = 2`）。统计：`sent` 2、`full` 3、`deepest` 2、`depth` 2。收走一个，再 `offer`：进了；`queue.clear` 一条，`lost = 3`。再堵满一次：又一条 `queue.full`。
9. `a_sender_that_waits_gets_everything_through_in_order`：`Full::Wait`，上限 2，发的任务 `send` 一百个，收的每 10 虚拟毫秒收一个：一百个按次序到；没有 `queue.full` 事件。
10. `another_kind_of_sender_has_its_own_answer`：`with(Full::Drop)` 拿到的发信人被退回时，事件里的 `policy` 是 `drop`；两种发信人用的是同一条队列、同一行统计。
11. `nobody_reading_is_not_full`：收的一头丢掉以后 `offer` 是 `Back::Closed`，`send` 把东西还回来；没有 `queue.full`。
12. `a_queue_read_by_a_thread_is_the_same_queue`：`threads`，上限 2：同样的退回、计数和事件；`recv_timeout` 在没有东西时到点返回。
13. `a_reader_that_lags_skips_ahead_and_the_loss_is_counted`：`fanout`，上限 4，一个读者不读，发 10 个：读者下一次收到「落后了 6 个」，`lost` 是 6，`queue.full` 一条；另一个一直在读的读者一个不少。
14. `the_same_name_is_the_same_row`：两条都叫 `tape` 的队列先后造：统计里一行。

规则脚本的证明（写进报告，不留在仓库里）：

15. `numbers.sh`：在改代码之前先跑一遍，把它报出的全部贴进报告（这就是要改的清单）；改完以后通过。再临时加三行各犯一条（`Duration::from_secs(5)`、`const X: Duration = …`、一个没有注释的 `const Y_MS: u64 = 5;`），它各报出来。
16. `bounded.sh`：欠账表填好以后通过；临时在 `core/src/lib.rs` 加一行 `let _ = tokio::sync::mpsc::channel::<u8>(4);`，它报出这一行；临时把欠账表里一行的数加一，它说该改小。

## 验收

- `scripts/build/check.sh` 全绿，`numbers.sh`、`bounded.sh` 在其中；测试通过的总数只多不少。
- `git grep -n "Duration::from_secs\|: Duration =" core/src app/src server/src log/src` 没有结果。
- `core/src/rt/` 通过 `layers.sh`（`stats.rs`、`queue.rs` 不用 `crate::` 下 `rt` 以外的东西）。
- 报告里：`numbers.sh` 改之前的输出；改了名字的常量清单（旧名 → 新名）；注释写了「来历待查」的常量；`bounded.sh` 的欠账表。

## 不做

- 不把任何现有的通道换成 `rt::queue`（P1-13 至 P1-21）；不开 clippy 的 `disallowed-methods`（P1-14）。
- 不写树、每分钟的汇总、`Stats` 信号（P1-05、P1-11）。
- 不改任何时限的数值，不改行为。
- 不动服务器的通道和任务。
- 不加 `hdrhistogram`、`tokio-metrics`。
