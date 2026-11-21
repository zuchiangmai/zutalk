# P1-09 落盘离开现场的路：写 tape、写草稿各隔一条有上限的队列

## 目标

做完以后：

1. 泵线程的 `sink` 不再加密、不再写盘：它只把块放进一条有上限的队列（600 块，60 秒）。队列满了：现场的录音说 `tape` 并结束；导入的文件等着。
2. 组帧的循环不再写草稿：说完的行放进另一条有上限的队列（256 行）。满了：那一行不进草稿，说一次 `draft.behind`，通了以后用一行空缺盖住丢掉的那一段。
3. 两条队列后面是同一段泛型代码（`rt/keep.rs`）：一个写的部件，一批一批地在阻塞线程上写；坏了（出错或 panic）照 `supervise` 的规矩重开。
4. tape 上的洞是静音：写的部件重开以后接着写，中间缺的块补成封好的静音。
5. `i2_a_tape_that_stands_still_holds_no_caption_back`、`i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` 通过。

## 背景

- 设计：`docs/build/phases/P1.md` §3.6（**为什么是 async 任务加一批一个 `spawn_blocking`，不是 `std::thread`，也不是长活的 `spawn_blocking`**，先读这一条）、§3.7（两条队列的上限和满了怎么办）。`PLAN.md` §4.1 的 L2 一行、§4.2「泵线程直接写 tape」一行。
- `core/src/app/hub.rs` 的 `Task::listen`：
  - `tape::Writer` 经 `Config.reels` 建好，被 `sink` 闭包拿着，`sink` 在采集的线程上每块调一次 `append`；
  - 循环里对第一条流每一行说完的 `draft.put(..)`（`shelf::Writer`，写文件），结束时 `draft.sync()`；
  - 终止的事实经 `ended` 这个 `watch` 交回（P1-07）。
- `core/src/capture/tape.rs`：`Writer::create`、`append`（要求块按次序、没有洞）、`recover`（截掉残尾，返回长度）、`Reel`、`Reels`。一块封好以后大小固定，位置就是偏移，nonce 就是位置：**少写一块，后面的全都读不出来**。
- `core/src/record/shelf.rs`：`Shelf::open(id, LINE)` 给一个 `Writer`，`put`、`sync`。
- `core/src/rt/supervise.rs`（P1-04）：`supervise`、`Restarts`、`Tree`。
- 导入文件时 `sink` 跑在读文件的线程上（`core/src/capture/audio/file.rs` 的 `run`），它读得比写得快。
- 台架：`core/tests/bench/mod.rs` 的带子替身有一道闸，能堵住、放开、让 `append` panic 一次。

## 做法

### `rt/keep.rs`：隔着队列写

一段不认识业务的泛型代码（名字和签名可以调整，要有这些性质）：

- 给它：队列的容量；怎么开一个写入口（`open`，可能失败）；怎么把一批东西写进去；怎么收尾（同步到盘）。
- 它给回：一个发的口。发有两种：`try_send`（满了立刻说不行）和等着发（给导入用）。
- 里面是一个 async 任务：等队列里有东西，取出现有的一批（有上限，比如 50 个），把写入口和这一批一起交给一个 `spawn_blocking`，写完把写入口拿回来。**一批一个 `spawn_blocking`**。
- 写出错或 panic：这一批算丢了，写入口扔掉，照 `supervise`（`Backoff`、`Restarts`）重开。到了重启上限就结束，并把原因交给调用者（一个回调或返回值）。
- 发的口都丢掉以后：把队列里剩下的写完，收尾，结束。调用者能等它结束。
- 它用 `tokio::time`，不读别的时钟；不 `use crate::` 除 `rt` 以外的任何东西（`rt` 的规矩）。
- 它不自己 `tokio::spawn`：给回一个 future（或者收一棵 `rt::supervise::Tree`），由调用者挂上去。P1-10 起一场录音的任务都挂在同一棵树上。

### tape

1. `tape::Writer` 记着自己写到哪里（下一块的时间）：
   - `create` 在一个已经有内容的文件夹上：从文件的大小算出已经写到哪里，截掉残尾（不用把整条 tape 解一遍）；
   - `append(block)`：块正好接上，就写；块在后面，先补封好的静音块直到接上（跨分钟也一样）；块在前面（重开之前已经写过），跳过。
   - 原有的行为不变：一分钟写完同步一次；`Drop` 时同步。
2. 录音任务用 `keep` 起写 tape 的部件：`open` 是 `(config.reels)(dir, key)`，容量 600。
3. `sink` 里：现场的声源 `try_send`，不行就把 `tape` 写进 `ended`（这场录音结束，已经落盘的留着）；`Source::File` 用等着发的那一种。`sink` 里只剩：算电平、放进队列、`audio.send`。
4. 写 tape 的部件到了重启上限：同样以 `tape` 结束这场录音。
5. 停止：采集停了、`sink` 丢了以后，等写 tape 的部件写完剩下的再去封（`seal`）。

### 草稿

6. 录音任务用 `keep` 起写草稿的部件：`open` 是 `shelf.open(id, LINE)`，容量 256；收尾是 `sync`。
7. 组帧的循环里对每一行 `try_send`。不行：记下这一行的起止时间，并说一次 `draft.behind`；之后第一次发得进去时，先发一行空缺（文字 `…`、`incomplete`，起止盖住丢掉的全部），再发那一行。行的编号照旧连续。
8. 停止时等写草稿的部件把剩下的写完并同步，`listen` 才返回。

### 词

9. `ui/lang/*.json` 里 `code.tape.slow`、`code.draft.behind` 在 P1-04 已加；没有的话在这里补上。

## 先写的测试

`core/tests/keep.rs`（暂停的时钟，替身的写入口）：

1. `what_is_queued_is_written_in_order_and_off_the_caller`：发 1,000 个，写入口按次序收到 1,000 个；发的一方从没等过。
2. `a_full_queue_says_so_at_once`：写入口堵着，第「容量 + 一批」之后的 `try_send` 立刻失败。
3. `a_writer_that_breaks_is_opened_again`：写入口第三批时返回错误、第五批时 panic；`open` 被叫了三次，之后的批次都写到了；坏掉的那两批没有被重写（丢了就是丢了，由上面的人去标空缺）。
4. `a_writer_that_never_opens_gives_up_and_says_why`：`open` 一直失败，到重启上限后结束，调用者拿到原因。
5. `closing_writes_what_is_left`：发 100 个，立刻丢掉发的口；等它结束，100 个都在，收尾被叫了一次。

`core/tests/tape.rs`：

6. `a_hole_between_blocks_is_sealed_silence`：写 0 毫秒、500 毫秒两块；读回来 6 块，中间 4 块是静音，时间线不断。
7. `a_hole_across_a_minute_is_silence_in_both_files`。
8. `a_writer_opened_on_a_tape_goes_on_where_it_ends`：写 3 块，丢掉；在文件尾追加半块垃圾（像崩溃留下的）；再 `create`，写 1000 毫秒那一块；读回来 11 块，没有报错。
9. `a_block_from_before_the_end_is_skipped`。

`core/tests/liveness.rs`：

10. 拿掉 `i2_a_tape_that_stands_still_holds_no_caption_back` 的 `ignore`。
11. 拿掉 `i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` 的 `ignore`。
12. `the_draft_is_whole_after_a_stop`（暂停的时钟）：话说到一半停止；草稿里是停止之前说完的每一行，最后一行是停止时还开着的那一行（不完整）。
13. `lines_the_draft_could_not_take_become_one_gap_line`：能在哪一层确定地测就在哪一层测。建议把「丢了哪些、补哪一行空缺」写成一个公开的小值类型（进去的是行和「发进去了没有」，出来的是要写的行），用表格测。

`core/tests/app.rs`（真实时钟，带闸的带子）：

14. `a_long_file_waits_for_its_tape`：闸关着，导入一个 2 分钟的文件（1200 块，超过队列）；没有 `tape` 这个代码；放开以后录音的长度是 120,000 毫秒。

## 验收

- `scripts/build/check.sh` 全绿。
- `core/src/app/hub.rs` 里 `sink` 闭包不再出现 `append`；`listen` 的循环里不再出现 `draft.put`。
- `rt/keep.rs` 里只有 `spawn_blocking`，没有 `std::thread::spawn`，也没有跨多批的 `spawn_blocking`。
- 报告里贴 `i1_eight_hours_…` 的真实耗时（每块多了一次到阻塞线程的往返，量一下多了多少）。

## 不做

- 不把录音任务拆到 `app/recording.rs`，不加水位表（P1-10）。
- 不加看守和 `judge` 的动作（P1-11）；`Tape`、`Draft` 两格的判断那时才有。
- 不动整场转写读 tape 的那一头（`tape::read`、`Pass`）。
- hub 自己的磁盘读写不动（P1-12、P1-13）。
