# P1-10 一场录音是一棵被守着的树：`app/recording.rs`

## 目标

做完以后：

1. 一场录音从第一块到封好 tape，住在 `core/src/app/recording.rs`，不在 hub 的文件里。
2. 它的每个部件都挂在一棵 `Tree` 上：采集、写 tape、写草稿、每条 lane、帧任务。没有裸的 `tokio::spawn`。帧任务和 lane 一样由 `supervise` 守着：崩了重开。
3. 每个部件用一个 `Gauge` 报告自己到了哪里。录音任务每秒把它们采成一份 `Water`，写进 `Outlet.water`（这一件只采样、只发布；判断和动作是 P1-11）。
4. 停止的次序是确定的，停止以后说过的、录下的都在。

## 背景

- 设计：`docs/build/phases/P1.md` §3.5（`Gauge`、`Stage`、`sample`、`Water`）、§3.6（那棵树）。`PLAN.md` §4.3 的水位线清单、§4.4 的监督树、`CLAUDE.md` 的规则「Every long-running task is supervised and reports a watermark」。
- `core/src/app/hub.rs`：`Task`（`record`、`listen`、`finish`、`seal`、`write`、`tell`、`tape`）、`drafted`、`tick`、`FRAME`、`REALTIME`、`WHOLE`。`listen` 现在是一个大的 `select!` 循环：模式变化、lane 的事件、帧的定时器、`ended`；lane 是 `tokio::spawn` 起的；写 tape、写草稿的部件已经是 `rt::keep`（P1-09）。
- `core/src/rt/supervise.rs`：`Part`、`supervise`、`Restarts`、`Tree`。`core/src/rt/water.rs`：`Gauge`、`Stage`、`sample`、`Water`。
- `core/src/live/lane.rs`：`run`、`Lane`（`edge`、`why`）、`Feed`（`sent`、`heard`、`capture()`）、`Conn::take`、`Conn::hear`。`core/src/live/frame.rs`：`Composer`。
- `Outlet.water`（P1-06）现在没人写。

## 做法

### 搬

1. 新文件 `core/src/app/recording.rs`。搬进来的：录音任务从开始到封好 tape 的全部（现在的 `Task::record`、`listen`、`seal`，以及只有它们用的 `drafted`、`tick`、`FRAME`、`REALTIME`）。留在 `hub.rs` 的：命令、状态、整场转写（`write`、`again` 的调用、`WHOLE`）和它的进度：那是后台的事，P1-12、P1-13 再搬去 `work.rs`。
2. hub 开始一场录音，只是把它需要的东西交过去（id、`About`、声源、模式的 `watch`、书架、钥匙、三个端口、五个寄存器、`ended` 之后回信的口），拿回一个可以等它结束的把手。

### 树

3. 录音任务一开始建一棵 `Tree`。挂上去的：
   - 写 tape、写草稿的部件（`rt::keep` 的任务）；
   - 每条 lane（`lane::run` 自己在里面 `supervise` 连接）；
   - **帧任务**：从 `listen` 的循环里分出来，做成一个 `Part`：`open` 给一个空的 `Composer`，`run` 是「收 lane 的事件 → `Composer` → 每 `FRAME` 出一帧、改状态句、把说完的行交给草稿的队列」。它由 `supervise` 守着，崩了从空的 `Composer` 重开；lane 的事件队列（`mpsc`，64）不随它死，重开以后接着收。
   - 看守（下面第 6 条）。
   - 录音任务自己剩下的循环只管：模式变化（暂停、继续、停止）和 `ended`。
4. **停止的次序**（写成注释，照着做）：模式变成 `Stopped` → 丢掉采集（泵交完整的块，`sink` 随之丢掉）→ 写 tape 的部件写完剩下的 → 各条 lane 等到最后的字或 `TAIL` 到时 → 帧任务处理完最后的事件，把最后几行交给草稿 → 写草稿的部件写完、同步 → 整棵树结束 → 封 tape、把长度写上记录。任何一步都不许无限期地等：lane 有 `TAIL + PATIENCE`，其余的靠队列关闭。
5. `listen` 出错或 `ended` 带着代码：说那个代码，然后走同一个停止的次序。

### 水位

6. 一张水位表（`Gauges`，就是一组 `Arc<Gauge>`）在开始时建好，分给各部件：
   - `Capture`：`sink` 里，每块 `set(block.t_ms + 100)`；
   - `Tape`：写 tape 的部件每写完一批 `set(最后一块的结束时间)`；
   - `Sent(n)`、`Heard(n)`、`Final(n)`：lane 多收一组 `Gauge`。发出一块以后 `Sent` 是那一块在采集钟上的结束时间；每个回复以后 `Heard` 是 `feed.capture(feed.heard)`；每次有行说完，`Final` 是最后一行的 `t1`。lane 现在为什么没连上（`Lane.why`）用 `because`；lane 结束时 `ended(code)`；
   - `Draft`：写草稿的部件每写完一批 `set(最后一行的 t1)`；
   - `Frame`：帧任务每出一帧 `set(帧的序号)`。
   - `Shell`、`Screen` 两个 `Gauge` 不属于某一场录音，放在 `Core` 里：加 `Core::pushed(seq)`、`Core::drawn(seq)`，各是一次 `set`（外壳和界面在 P1-14 开始调它们）。
7. **看守**（这一件只做一半）：录音任务里一个每秒一次的定时器，读水位表，调 `sample(上一份, 位置, now_ms, clock_ms, paused)`，把结果 `set` 进 `water` 寄存器。`now_ms` 用 `tokio::time::Instant`（从这场录音开始算）；`clock_ms` 是录音的钟：从第一块到来时起算，去掉暂停的时间（第一块之前是 0：设备还在开，不算落后）。`Water.health` 先空着。录音结束时把 `water` 设回空的。
8. `lane::run` 的签名变了：`core/tests/live.rs` 的 `start` 跟着传一组 `Gauge`。

### 守住

9. 加 `scripts/build/rules/supervised.sh`：`core/src/app/recording.rs`、`core/src/live/`、`core/src/rt/keep.rs` 里没有 `tokio::spawn(`、`thread::spawn(`、`spawn_local(`（任务都经 `Tree` 起）；找到就打印并以非零退出。`rt/supervise.rs` 是 `Tree` 的家，不在此列。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `every_part_reports_how_far_it_has_got`：说 30 虚拟秒的话。`outlet.water` 里有 `Capture`、`Tape`、`Draft`、`Frame`、`Sent(0)`、`Heard(0)`、`Final(0)` 七格；`Capture` 在 29,900–30,100 之间；`Tape` 比它落后不到 1 秒；`Sent(0)` 落后不到 300 毫秒；`Heard(0)` 落后不到 1.5 秒；`Final(0)`、`Draft` 大于 0；每一格的 `moved_ms` 都在最近 2 秒内。
2. `paused_the_water_says_so_and_nothing_moves`：暂停 10 虚拟秒，`water.paused` 为真，各格的位置不变；继续以后又走。
3. `the_water_comes_once_a_second_and_ends_with_the_recording`：60 虚拟秒里 `water` 的序号涨了 59–61；停止以后它是空的，再过 10 秒序号不变。
4. `a_stop_keeps_everything_said_and_recorded`：话说到一句的中间时停止。停止以后：tape 的长度等于话筒交出的块数 × 100；草稿的最后一行是那半句（不完整）；`session` 是 `None`；`state.writing` 里这一条从 `closing` 往下走了。
5. `a_recording_whose_capture_ends_is_closed_the_same_way`：话筒自己结束（像合盖）：同样的收尾，说了它的代码。

`core/tests/live.rs`：

6. `a_lane_reports_what_it_sent_what_was_heard_and_what_is_final`：对着脚本服务，发 10 块、服务回到 600 毫秒、定下一行到 480 毫秒：三个 `Gauge` 是 1000、600、480（按采集钟，录音从 5 秒处加入时是 6000、5600、5480）。重连期间 `because` 是断开的原因；停止以后 `ended` 是 `done`。

`core/tests/supervise.rs` 的 `a_part_that_panics_is_opened_again`（P1-04）守着帧任务走的那条路；这一件不重写它，但报告里指出帧任务是用哪一行挂到 `supervise` 上的。

## 验收

- `scripts/build/check.sh` 全绿（`supervised.sh` 在其中）。
- `core/src/app/hub.rs` 里没有 `fn listen`、`Composer`、`broadcast::channel`；`core/src/app/recording.rs` 通过 `layers.sh`。
- 报告里贴一份录到第 30 秒时的 `Water`（JSON），和 `i1_eight_hours_…` 的数。

## 不做

- 不调 `judge`，不做任何动作，`Water.health` 是空的，`Column.health` 仍由 `Link` 映射（P1-11）。
- 不加 `Command::Reconnect`、取钥匙的时限（P1-11）。
- 整场转写、交城、搬运不搬（P1-12、P1-13）。
- 外壳和界面不调 `pushed`、`drawn`（P1-14）。
