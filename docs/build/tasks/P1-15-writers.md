# P1-15 落盘离开现场的路：写 tape、写草稿各隔一条有上限的队列

## 目标

做完以后：

1. 泵线程的 `sink` 不再加密、不再写盘：它只把块放进一条有上限的队列（`tape`，`TAPE_QUEUE_BLOCKS` = 600 块，60 秒）。队列满了：现场的录音说 `tape` 并结束；导入的文件等着。
2. 组帧的循环不再写草稿：说完的行放进另一条有上限的队列（`draft`，`DRAFT_QUEUE_LINES` = 256 行）。满了：那一行不进草稿，说一次 `draft.behind`，通了以后用一行空缺盖住丢掉的那一段。
3. 两条队列后面是同一段泛型代码（`rt/keep.rs`）：一个写的部件，一批一批地在阻塞线程上写；坏了（出错或 panic）照 `supervise` 的规矩重开。它是树上一个有名字的、被守着的部件。
4. tape 上的洞是静音：写的部件重开以后接着写，中间缺的块补成封好的静音。
5. 丢了一批、补了静音、草稿补了空缺、队列满了：每一样在日志里有一条事件，在统计里有一个数。
6. `i2_a_tape_that_stands_still_holds_no_caption_back`、`i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` 通过，连同它们「看得见」的断言。

## 背景

- 设计：`docs/build/phases/P1.md` §3.6（**为什么是 async 任务加一批一个阻塞调用，不是自己起的线程，也不是长活的阻塞调用**，先读这一条）、§3.7（两条队列的名字、上限和满了怎么办）、§3.13.9（队列怎么造）、§3.13.10（`Tree::keep`、`Tree::blocking`）、§3.13.3（标着 P1-15 的三行）、§2 第二张表里这一件管的四行。`PLAN.md` §4.1 的 L2 一行、§4.2「泵线程直接写 tape」一行。
- `core/src/app/hub.rs` 的 `Task::listen`：
  - `tape::Writer` 经 `Config.reels` 建好，被 `sink` 闭包拿着，`sink` 在采集的线程上每块调一次 `append`；
  - 循环里对第一条流每一行说完的 `draft.put(..)`（`shelf::Writer`，写文件），结束时 `draft.sync()`；
  - 终止的事实经 `ended` 这个 `watch` 交回（P1-12）。
- `core/src/capture/tape.rs`：`Writer::create`、`append`（要求块按次序、没有洞）、`recover`（截掉残尾，返回长度）、`Reel`、`Reels`。一块封好以后大小固定，位置就是偏移，nonce 就是位置：**少写一块，后面的全都读不出来**。
- `core/src/record/shelf.rs`：`Shelf::open(id, LINE)` 给一个 `Writer`，`put`、`sync`。
- `core/src/rt/supervise.rs`：`Tree`（P1-05）、`supervise`、`Restarts`、`Tree::keep`（P1-09）。`core/src/rt/queue.rs`（P1-04）。
- 导入文件时 `sink` 跑在读文件的线程上（`core/src/capture/audio/file.rs` 的 `run`），它读得比写得快。
- 台架：`core/tests/bench/mod.rs` 的带子替身有一道闸，能堵住、放开、让 `append` panic 一次。

## 做法

### `rt/keep.rs`：隔着队列写

一段不认识业务的泛型代码（名字和签名可以调整，要有这些性质）：

- 给它：一棵树和部件的名字（`tape`、`draft`）；队列的名字和上限；怎么开一个写入口（`open`，可能失败）；怎么把一批东西写进去；怎么收尾（同步到盘）。
- 它给回：一个发的口，就是 `rt::queue::Tx`。发有两种：`offer`（满了立刻退回）和 `send`、`send_blocking`（等位置，给导入用）；队列在 `keep` 里用 `queue::bounded` 造，调用的人说这一头的 `Full`，另一种发信人用 `with`。
- 里面是一个 async 任务：等队列里有东西，取出现有的一批（上限 `KEEP_BATCH_ITEMS`，50：半秒的块，一次写完不到一毫秒），把写入口和这一批一起交给一个 `tree.blocking(name, …)`，写完把写入口拿回来。**一批一个阻塞调用**。每一批的用时记进调用者给的分布（`tape.batch_ms`、`draft.batch_ms`）。
- 写出错或 panic（`blocking` 给回 `Broke`）：这一批算丢了，写入口扔掉，照 `supervise`（`Backoff`、`Restarts`）重开。丢的那一批留一条 `warn!("keep.lost", part, n = 这一批几个, why)`（`why` 是错误的代码，或 `panic`），计数 `keep.lost_batches` 加一。到了重启上限就结束，并把原因交给调用者（一个回调或返回值）。
- 发的口都丢掉以后：把队列里剩下的写完，收尾，结束。调用者能等它结束。
- 它用 `tokio::time`，不读别的时钟；不 `use crate::` 除 `rt` 以外的任何东西（`rt` 的规矩）。
- 它不自己 `tokio::spawn`：从给它的那棵树上 `keep`（P1-09），所以它的每次转换有 `part.state`，放弃有 `part.gave_up`，重开的次数进 `part.restarts`，都不用它自己写。这一件里，那棵树是 hub 从根上分出来、交给录音任务的 `recording` 子树（`root.child("recording")`）；P1-16 把这场录音其余的任务也挂上去。

### tape

1. `tape::Writer` 记着自己写到哪里（下一块的时间）：
   - `create` 在一个已经有内容的文件夹上：从文件的大小算出已经写到哪里，截掉残尾（不用把整条 tape 解一遍）；
   - `append(block)`：块正好接上，就写；块在后面，先补封好的静音块直到接上（跨分钟也一样），并发一条 `warn!("tape.pad", from_ms, blocks)`（`tape.rs` 是做 IO 的模块，可以自己发；它在阻塞调用里跑，`Tree::blocking` 把日志带过去了）；块在前面（重开之前已经写过），跳过。「接上、在后面、在前面」是一个纯函数的三种结果，有表格测试。
   - 原有的行为不变：一分钟写完同步一次；`Drop` 时同步。
2. 录音任务用 `keep` 起写 tape 的部件：`open` 是 `(config.reels)(dir, key)`，队列 `tape`，上限 `TAPE_QUEUE_BLOCKS`（600）。每写完一批，计数 `tape.blocks` 加这一批的块数；`tape.padded_blocks` 加补的静音块数（`Reel::append` 的返回值带不出这个数的话，由真的 `tape::Writer` 自己拿一个 `Counter`）。
3. `sink` 里：现场的声源用 `Full::End` 的那一头 `offer`，被退回就把 `tape` 写进 `ended`（这场录音结束，已经落盘的留着；`queue.full` 是队列自己发的，`recording.end` 带着 `tape` 是 P1-12 已经有的）；`Source::File` 用 `with(Full::Wait)` 的那一头 `send_blocking`。`sink` 里只剩：算电平、放进队列、`audio.send`。
4. 写 tape 的部件到了重启上限：同样以 `tape` 结束这场录音。
5. 停止：采集停了、`sink` 丢了以后，等写 tape 的部件写完剩下的再去封（`seal`）。

### 草稿

6. 录音任务用 `keep` 起写草稿的部件：`open` 是 `shelf.open(id, LINE)`，队列 `draft`，上限 `DRAFT_QUEUE_LINES`（256），`Full::Drop`；收尾是 `sync`。每写完一批，计数 `draft.lines` 加这一批的行数。
7. 组帧的循环里对每一行 `offer`。被退回：记下这一行的起止时间，计数 `draft.lost_lines` 加一，并说一次 `draft.behind`；之后第一次发得进去时，先发一行空缺（文字 `…`、`incomplete`，起止盖住丢掉的全部），并发一条 `warn!("draft.gap", t0_ms, t1_ms, lines = 丢了几行)`，再发那一行。行的编号照旧连续。「丢了哪些、补哪一行空缺」是一个值类型（见测试第 13 条），不读钟、不发事件；事件由用它的那一处发。
8. 停止时等写草稿的部件把剩下的写完并同步，`listen` 才返回。

### 词

9. `ui/lang/*.json` 里 `code.tape.slow`、`code.draft.behind` 在 P1-09 已加；没有的话在这里补上。

## 先写的测试

`core/tests/keep.rs`（暂停的时钟，替身的写入口）：

（都装捕获；给 `keep` 一棵 `Tree::root("test", Stats::new())`。）

1. `what_is_queued_is_written_in_order_and_off_the_caller`：发 1,000 个，写入口按次序收到 1,000 个；发的一方从没等过。统计里这条队列 `sent` 是 1000、`full` 是 0；分布里记了至少 20 批（一批最多 50）。
2. `a_full_queue_says_so_at_once`：写入口堵着，第「容量 + 一批」之后的 `offer` 立刻被退回。**看得见**：`queue.full` 一条，`policy` 是造的时候说的那个。
3. `a_writer_that_breaks_is_opened_again`：写入口第三批时返回错误、第五批时 panic；`open` 被叫了三次，之后的批次都写到了；坏掉的那两批没有被重写（丢了就是丢了，由上面的人去标空缺）。**看得见**：`keep.lost` 两条，`n` 是那两批各自的个数，`why` 一条是错误的代码、一条是 `panic`；`part.state` 到 `Waiting` 的两条，`why` 相同；计数 `keep.lost_batches` 是 2、`part.restarts` 是 2。
4. `a_writer_that_never_opens_gives_up_and_says_why`：`open` 一直失败，到重启上限后结束，调用者拿到原因。**看得见**：`part.gave_up` 一条（`part` 是给的名字、`why` 是那个原因），级别 `error`。
5. `closing_writes_what_is_left`：发 100 个，立刻丢掉发的口；等它结束，100 个都在，收尾被叫了一次。

`core/tests/tape.rs`：

6. `a_hole_between_blocks_is_sealed_silence`：写 0 毫秒、500 毫秒两块；读回来 6 块，中间 4 块是静音，时间线不断。**看得见**（装捕获）：`tape.pad` 一条，`from_ms = 100`、`blocks = 4`。
7. `a_hole_across_a_minute_is_silence_in_both_files`。
8. `a_writer_opened_on_a_tape_goes_on_where_it_ends`：写 3 块，丢掉；在文件尾追加半块垃圾（像崩溃留下的）；再 `create`，写 1000 毫秒那一块；读回来 11 块，没有报错。
9. `a_block_from_before_the_end_is_skipped`。

`core/tests/liveness.rs`：

10. 拿掉 `i2_a_tape_that_stands_still_holds_no_caption_back` 的 `ignore`。它「看得见」的断言照 `P1.md` §2 第二张表：前半没有 `queue.full`，`tape` 这条队列的 `deepest` 大于 0；后半 `queue.full`（`queue = "tape"`、`policy = "end"`）一条、`recording.end`（`code = "tape"`）一条、提示里有 `tape`。
11. 拿掉 `i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` 的 `ignore`。「看得见」：`part.state`（`part = "tape"`、`to = "Waiting"`、`why = "panic"`）一条；`keep.lost`（`part = "tape"`）一条；`tape.pad` 一条，补的块数等于丢的那一批；计数 `keep.lost_batches` 是 1；`recording.end` 的 `code` 是 `done`。
12. `the_draft_is_whole_after_a_stop`（暂停的时钟）：话说到一半停止；草稿里是停止之前说完的每一行，最后一行是停止时还开着的那一行（不完整）。
13. `lines_the_draft_could_not_take_become_one_gap_line`：能在哪一层确定地测就在哪一层测。建议把「丢了哪些、补哪一行空缺」写成一个公开的小值类型（进去的是行和「发进去了没有」，出来的是要写的行），用表格测。再有一条走完整条路的（台架，带闸的草稿写入口，或者把草稿的队列堵满）：`queue.full`（`queue = "draft"`、`policy = "drop"`）一条；提示里 `draft.behind` 正好一次；通了以后 `draft.gap` 一条，`lines` 等于计数 `draft.lost_lines`；草稿里那一行空缺盖住的正是 `t0_ms` 到 `t1_ms`。

`core/tests/app.rs`（真实时钟，带闸的带子）：

14. `a_long_file_waits_for_its_tape`：闸关着，导入一个 2 分钟的文件（1200 块，超过队列）；没有 `tape` 这个代码；放开以后录音的长度是 120,000 毫秒。

## 显式与可观测

- **事件**：`keep.lost`、`tape.pad`、`draft.gap`。写进 `docs/EVENTS.md`。`part.state`、`part.gave_up`（两个部件）、`queue.full`、`queue.clear`（两条队列）是地基发的。
- **统计**：计数 `tape.blocks`、`tape.padded_blocks`、`draft.lines`、`draft.lost_lines`、`keep.lost_batches`；分布 `tape.batch_ms`、`draft.batch_ms`；队列 `tape`、`draft` 各一行。
- **数**：`TAPE_QUEUE_BLOCKS`（600：60 秒；一块 100 毫秒，磁盘卡一分钟以上就不该再装作在录）、`DRAFT_QUEUE_LINES`（256）、`KEEP_BATCH_ITEMS`（50）。前两个是临时的，`P1.md` §4 第 3 条。
- **队列**：`tape`（现场 `Full::End`，导入 `Full::Wait`）、`draft`（`Full::Drop`）。都从 `rt::queue` 造。
- **任务**：`recording` 子树上的 `tape`、`draft`，由 `Tree::keep` 起；每一批是一个 `Tree::blocking`（`tape.batch`、`draft.batch`）。`rt/keep.rs` 里没有 `tokio::spawn`、`spawn_blocking`、`thread::spawn`（`spawn.sh` 守着，新文件没有欠账）。
- **丢弃**：「这一行没进草稿」「这一批丢了」都不是 `let _ =`：它们是数、是事件、是一行空缺。
- **P1-05 报告里指给这一件的丢弃**（`tape.rs` 里被吞掉的 `sync_data` 之类）：在这里处理：能当失败报的就报（一条事件加一个代码），确实没关系的保留理由。

## 验收

- `scripts/build/check.sh` 全绿。
- `core/src/app/hub.rs` 里 `sink` 闭包不再出现 `append`；`listen` 的循环里不再出现 `draft.put`。
- `rt/keep.rs` 里起阻塞调用只有 `tree.blocking` 一种，一批一个，没有跨多批的。
- 报告里贴 `i1_eight_hours_…` 的真实耗时（每块多了一次到阻塞线程的往返，量一下多了多少）。

## 不做

- 不把录音任务拆到 `app/recording.rs`，不把 lane 和组帧搬到树上，不加水位表（P1-16）。
- 不加看守和 `judge` 的动作（P1-17）；`Tape`、`Draft` 两格的判断那时才有。
- 不动整场转写读 tape 的那一头（`tape::read`、`Pass`）。
- hub 自己的磁盘读写不动（P1-19、P1-20）。
