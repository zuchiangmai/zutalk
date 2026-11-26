# P1-16 一场录音是一个状态机、一棵被守着的树：`app/recording.rs`

## 目标

做完以后：

1. 一场录音从第一块到封好 tape，住在 `core/src/app/recording.rs`，不在 hub 的文件里。
2. 它在哪一步，是一个枚举（`Phase`）：开着、录着、暂停、收尾的每一步、封 tape、结束。转换是一个纯函数；每次转换是日志里的一条 `recording.phase`，带上一步待了多久。一次停不下来的停止，看日志就知道停在哪一步。
3. 它的每个部件都挂在 `recording` 这棵子树上，各有名字：写 tape、写草稿、每条 lane、帧任务、看守。没有裸的 `tokio::spawn`。帧任务和 lane 一样是被守着的部件：崩了重开。
4. 每个部件用一个 `Gauge` 报告自己到了哪里。看守每秒把它们采成一份 `Water`，写进 `Outlet.water`（这一件只采样、只发布；判断和动作是 P1-17）。
5. 停止的次序是确定的，停止以后说过的、录下的都在。

## 背景

- 设计：`docs/build/phases/P1.md` §3.5（`Gauge`、`Stage`、`sample`、`Water`）、§3.6（那棵树、录音的状态机）、§3.7（`audio`、`lane.events` 两条队列）、§3.13.10（`Tree`）、§3.13.11（状态机的写法）、§3.13.3（标着 P1-16 的两行）。`PLAN.md` §4.3 的水位线清单、§4.4 的监督树、`CLAUDE.md` 的规则「Every long-running task is supervised and reports a watermark」。
- `core/src/app/hub.rs`：`Task`（`record`、`listen`、`finish`、`seal`、`write`、`tell`、`tape`）、`drafted`、`tick`、`FRAME_MS`、`REALTIME`、`WHOLE`。`listen` 现在是一个大的 `select!` 循环：模式变化、lane 的事件、帧的定时器、`ended`，状态散在 `capture.is_some()`、`open` 这样的变量里；lane 是裸的 `tokio::spawn` 起的；音频的 `broadcast::channel(64)` 和 lane 事件的 `mpsc::channel(64)` 是直接造的；写 tape、写草稿的部件已经在 `recording` 子树上（P1-15）。
- `core/src/rt/supervise.rs`：`Tree`、`Tree::keep`、`Part`、`supervise`、`Restarts`。`core/src/rt/water.rs`：`Gauge`、`Stage`、`sample`、`Water`。`core/src/rt/queue.rs`：`bounded`、`fanout`。
- `core/src/live/lane.rs`：`run`、`Lane`（`edge`、`why`）、`Feed`（`sent`、`heard`、`capture()`）、`Conn::take`、`Conn::hear`、`LANE_NAMES`（P1-09）。`core/src/live/frame.rs`：`Composer`、`Fit`。
- `Outlet.water`（P1-11）现在没人写。`recording.start`、`recording.end`、`session.status`（P1-12）已经在发。

## 做法

### 搬

1. 新文件 `core/src/app/recording.rs`。搬进来的：录音任务从开始到封好 tape 的全部（现在的 `Task::record`、`listen`、`seal`，以及只有它们用的 `drafted`、`tick`、`FRAME_MS`、`REALTIME`）。留在 `hub.rs` 的：命令、状态、整场转写（`write`、`again` 的调用、`WHOLE`）和它的进度：那是后台的事，P1-19 再搬去 `work.rs`。
2. hub 开始一场录音，只是把它需要的东西交过去（id、`About`、声源、要开的流或者没有字幕的原因（`lane::captions` 的结果，P1-07）、模式的 `watch`、书架、钥匙、三个端口、五个寄存器、`recording` 子树、`ended` 之后回信的口），拿回一个可以等它结束的把手。录音任务本身也从树上起：`root.spawn("recording", …)`。

### 状态机

3. `Phase` 和它的转换（照 `P1.md` §3.13.11 的写法）：

   ```rust
   /// Where a recording is. Each closing step waits for one thing, in this order.
   pub enum Phase { Opening, Recording, Paused, Closing(Step), Sealing, Ended(&'static str) }
   pub enum Step { Capture, Tape, Lanes, Frame, Draft }       // the order of a stop
   pub enum Happened { Opened, Mode(Mode), Failed(&'static str), StepDone }   // what the task saw
   /// Pure: the next phase, and what to do to get there.
   pub fn step(phase: Phase, happened: Happened) -> (Phase, Vec<Effect>);
   ```

   - 名字可以调整；要有：一个写出全部状态的枚举、`code()`（`Closing(Step::Tape)` 的代码是 `Closing.Tape`）、一个纯的转换函数。不许再有 `capture.is_some() || open` 这样拼出来的状态。
   - 录音任务是这层壳：等一件事发生，问 `step`，做它说的事；`Phase` 变了就发一条 `info!("recording.phase", rec = Id(&id), from, to, why, ms = 上一个状态待了多久)`（`ms` 用 `tokio::time::Instant`）。到 `Ended` 带着失败的代码时用 `error!`。
   - `recording.start`、`recording.end`（P1-12）照旧，分别在进 `Opening` 之前和到 `Ended` 之后；`recording.end` 的 `code` 就是 `Ended` 带的那个。
   - 没有字幕的一场（`captions` 给的是原因）没有 lane：`Closing(Lanes)` 没有东西可等，立刻 `StepDone`。这不是另一个状态，也不是另一条路。
4. **停止的次序**就是 `Step` 的次序（写成 `Step` 上的注释，照着做）：模式变成 `Stopped`，或者 `ended` 带着代码 → `Closing(Capture)`：丢掉采集（泵交完整的块，`sink` 随之丢掉）→ `Closing(Tape)`：写 tape 的部件写完剩下的 → `Closing(Lanes)`：各条 lane 等到最后的字或 `TAIL_MS` 到时 → `Closing(Frame)`：帧任务处理完最后的事件，把最后几行交给草稿 → `Closing(Draft)`：写草稿的部件写完、同步 → 整棵子树结束（`tree.wait()`）→ `Sealing`：封 tape、把长度写上记录 → `Ended(code)`。任何一步都不许无限期地等：lane 有 `TAIL_MS + PATIENCE_MS`，其余的靠队列关闭。
5. 出错或 `ended` 带着代码：说那个代码，然后走同一个停止的次序，最后是 `Ended(那个代码)`。

### 树

6. `recording` 子树上挂的（P1-15 已经挂了前两个）：
   - `tape`、`draft`：写盘的部件（`rt::keep`）；
   - `lane.0`、`lane.1`、`lane.2`：每条 lane 是树上的一个任务，名字来自 `LANE_NAMES`。`lane::run` 里 `supervise` 外面那一层（和「停止」的 `select!`、最后的收尾）要留着，所以两种接法都行：把连接那一层交给 `tree.keep`，`run` 等它的结果；或者整个 `run` 用 `tree.spawn` 起、里面照旧直接调 `supervise`，由 lane 在 `broke` 里自己给 `part.restarts` 加一（它手里有 `Stats`，P1-10）。要的是三样：有名字；树取消它就结束；重开进 `part.restarts`；
   - `frame`：**帧任务**，从 `listen` 的循环里分出来，做成一个 `Part`，`tree.keep("frame", …)`：`open` 给一个空的 `Composer`，`run` 是「收 lane 的事件 → `Composer` → 每 `FRAME_MS` 出一帧、改状态句、把说完的行交给草稿的队列」。崩了从空的 `Composer` 重开；lane 的事件队列不随它死，重开以后接着收。P1-10 写在组帧那一处的统计和 `frame.fit`、P1-12 的 `session.status` 跟着搬进来；
   - `watch`：看守（下面第 9 条），`tree.spawn("watch", …)`；
   - 录音任务自己剩下的循环只管：模式变化、`ended`、各步做完了没有。
7. 两条队列换成有名字的：
   - 音频 → lane：`queue::fanout(&stats, "audio", AUDIO_FANOUT_BLOCKS)`（64：6.4 秒；lane 自己在积压 20 块时就跳了）；
   - lane → 帧任务：`queue::bounded(&stats, "lane.events", LANE_QUEUE_EVENTS, Full::Wait)`（64）。lane 等帧任务：两边在同一层。
8. `caption.lag_ms`：帧任务每收到一个带文字的事件（`Lines`、`Tail`），把「`Capture` 这一格现在的位置 − 这段文字的结束时间」记进这个分布（小于 0 记 0）。这是产品里量得到的「说出到上屏」里核心的那一段；它只是一次原子读加一次 `note`。

### 水位

9. 一张水位表（`Gauges`，就是一组 `Arc<Gauge>`）在开始时建好，分给各部件：
   - `Capture`：`sink` 里，每块 `set(block.t_ms + 100)`；
   - `Tape`：写 tape 的部件每写完一批 `set(最后一块的结束时间)`；
   - `Sent(n)`、`Heard(n)`、`Final(n)`：lane 多收一组 `Gauge`。发出一块以后 `Sent` 是那一块在采集钟上的结束时间；每个回复以后 `Heard` 是 `feed.capture(feed.heard)`；每次有行说完，`Final` 是最后一行的 `t1`。lane 现在为什么没连上（`Lane.why`）用 `because`；lane 结束时 `ended(code)`；
   - `Draft`：写草稿的部件每写完一批 `set(最后一行的 t1)`；
   - `Frame`：帧任务每出一帧 `set(帧的序号)`。
   - `Shell`、`Screen` 两个 `Gauge` 不属于某一场录音，放在 `Core` 里：加 `Core::pushed(seq)`、`Core::drawn(seq)`，各是一次 `set`（外壳在 P1-23、界面在 P1-24 开始调它们）。
10. **看守**（这一件只做一半）：每 `WATCH_EVERY_MS`（1000）读水位表，调 `sample(上一份, 位置, now_ms, clock_ms, paused)`，把结果 `set` 进 `water` 寄存器。`now_ms` 用 `tokio::time::Instant`（从这场录音开始算）；`clock_ms` 是录音的钟：从第一块到来时起算，去掉暂停的时间（第一块之前是 0：设备还在开，不算落后）。`Water.health` 先空着。录音结束时把 `water` 设回空的。
    - 每秒每格一条 `debug!("water.mark", snap = 0u64, stage, lane, at, moved_ms, why, ended)`。它默认不写（级别 `debug`）；`ZUTALK_LOG=debug` 时就是真机上一小时的水位记录（原计划的 `ZUTALK_WATER_LOG` 因此取消）。「把一份 `Water` 写成一组行」是一个单独的函数：P1-17 的快照也用它。
11. `lane::run` 的签名变了：`core/tests/live.rs` 的 `start` 跟着传一棵树和一组 `Gauge`。

## 显式与可观测

- **事件**：`recording.phase`、`water.mark`。写进 `docs/EVENTS.md`。`task.start`、`task.end`（`tree = "recording"`）、`part.state`（lane、帧任务）是地基发的。
- **统计**：分布 `caption.lag_ms`；队列 `audio`、`lane.events` 各一行；`part.restarts` 现在也数 lane 和帧任务。
- **状态机**：`Phase`（转换表测试第 6 条）。
- **数**：`FRAME_MS`、`WATCH_EVERY_MS`、`AUDIO_FANOUT_BLOCKS`、`LANE_QUEUE_EVENTS`；lane 里的 `BACKLOG` 改名 `BACKLOG_BLOCKS`，补上理由。
- **任务**：`core` 根上的 `recording`（录音任务自己）；`recording` 子树上的 `tape`、`draft`、`lane.N`、`frame`、`watch`。
- **欠账**：`spawn.sh` 里 `core/src/app/hub.rs` 那一行减掉起录音任务、起 lane 和封 tape 的阻塞调用那几处；`bounded.sh` 里 `core/src/app/hub.rs` 那一行删掉（音频的 `broadcast` 和 lane 的 `mpsc` 还清）。`core/src/app/recording.rs`、`core/src/live/` 是没有欠账的：里面不许出现裸的 `spawn` 和直接造的通道。原计划在这一件加的 `supervised.sh` 取消：`spawn.sh` 守的是同一件事。
- **丢弃**：帧任务往 `frame`、`session` 寄存器里写从不失败；往 `draft` 的 `offer` 被退回是 P1-15 的那条路，不是 `let _ =`。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `every_part_reports_how_far_it_has_got`：说 30 虚拟秒的话。`outlet.water` 里有 `Capture`、`Tape`、`Draft`、`Frame`、`Sent(0)`、`Heard(0)`、`Final(0)` 七格；`Capture` 在 29,900–30,100 之间；`Tape` 比它落后不到 1 秒；`Sent(0)` 落后不到 300 毫秒；`Heard(0)` 落后不到 1.5 秒；`Final(0)`、`Draft` 大于 0；每一格的 `moved_ms` 都在最近 2 秒内。统计里 `caption.lag_ms` 的个数大于 0，`p99` 不超过 1000（台架的服务 700 毫秒定稿）。
2. `paused_the_water_says_so_and_nothing_moves`：暂停 10 虚拟秒，`water.paused` 为真，各格的位置不变；继续以后又走。
3. `the_water_comes_once_a_second_and_ends_with_the_recording`：60 虚拟秒里 `water` 的序号涨了 59–61；停止以后它是空的，再过 10 秒序号不变。台架的捕获开到 `Debug` 时，`water.mark` 每秒每格一条（七格、60 秒：420 条上下，照实断言一个范围）；开在 `Info` 时一条也没有。
4. `a_stop_keeps_everything_said_and_recorded`：话说到一句的中间时停止。停止以后：tape 的长度等于话筒交出的块数 × 100；草稿的最后一行是那半句（不完整）；`session` 是 `None`；`state.writing` 里这一条从 `closing` 往下走了。**看得见**：`recording.phase` 的 `to` 依次正好是 `Recording`、`Closing.Capture`、`Closing.Tape`、`Closing.Lanes`、`Closing.Frame`、`Closing.Draft`、`Sealing`、`Ended`；`recording.end`（`code = "done"`）在最后一条 `recording.phase` 之后；`task.end`（`tree = "recording"`）五条（`tape`、`draft`、`lane.0`、`frame`、`watch`），没有一条是 `panic`。
5. `a_recording_whose_capture_ends_is_closed_the_same_way`：话筒自己结束（像合盖）：同样的收尾，说了它的代码。**看得见**：`recording.phase` 走同样的各步，最后 `Ended` 的 `why` 是那个代码、级别 `error`。

5a. `a_recording_without_a_key_has_no_lanes_and_stops_the_same_way`：台架「不放钥匙」（P1-08）。录 30 虚拟秒：`outlet.water` 里只有 `Capture`、`Tape`、`Draft`、`Frame` 四格，`Capture`、`Tape` 在走；状态句是 `AudioOnly { why: "captions.no_key" }`；暂停时它是 `Paused`，继续以后又回来（没有 lane 的一场，帧任务照样在：状态句是它管的）。停止：tape 的长度对；`recording.phase` 走同样的各步（`Closing.Lanes` 也在里面）；`task.end`（`tree = "recording"`）是 `tape`、`draft`、`frame`、`watch` 四条，没有 `lane.*`；没有 `part.state`，没有 `notice.said`。

`core/tests/recording.rs`（值进值出）：

6. `a_recording_steps_as_its_table_says`：`step` 的转换表，对每个（状态，发生的事）断言下一个状态和效果：`Opening` 遇 `Opened` → `Recording`；`Recording` 遇 `Mode(Paused)` → `Paused`，遇 `Mode(Stopped)` 或 `Failed(code)` → `Closing(Capture)`；`Closing` 的每一步遇 `StepDone` → 下一步，最后一步 → `Sealing`；`Sealing` 遇 `StepDone` → `Ended`；`Opening` 遇 `Failed(code)` 也走 `Closing`（已经起来的部件照样收尾）；`Closing` 里再来 `Mode(..)` 不变；表里每个组合都有一行。每个 `Phase` 的 `code()` 过得了 `zutalk_log::is_plain`。

`core/tests/live.rs`：

7. `a_lane_reports_what_it_sent_what_was_heard_and_what_is_final`：对着脚本服务，发 10 块、服务回到 600 毫秒、定下一行到 480 毫秒：三个 `Gauge` 是 1000、600、480（按采集钟，录音从 5 秒处加入时是 6000、5600、5480）。重连期间 `because` 是断开的原因；停止以后 `ended` 是 `done`。

`core/tests/supervise.rs` 的 `a_part_that_panics_is_opened_again`、`a_kept_part_is_a_task_of_the_tree_and_its_restarts_are_counted`（P1-09）守着帧任务走的那条路；这一件不重写它们，但报告里指出帧任务是用哪一行挂到树上的。

`core/tests/liveness.rs`：`i6_a_lane_that_dies_…` 补上 P1-09 留了注释的那一项：计数 `part.restarts` 是 1。

## 验收

- `scripts/build/check.sh` 全绿；`spawn.sh`、`bounded.sh` 的欠账表照上面改小以后通过。
- `core/src/app/hub.rs` 里没有 `fn listen`、`Composer`、`broadcast`；`core/src/app/recording.rs` 通过 `layers.sh`。
- `git grep -n "tokio::spawn\|spawn_blocking\|mpsc::channel\|broadcast::channel" core/src/app/recording.rs core/src/live core/src/rt/keep.rs` 没有结果。
- 报告里贴：一份录到第 30 秒时的 `Water`（JSON）；一次停止的全部 `recording.phase` 行（从测试第 4 条的捕获里抄）；`i1_eight_hours_…` 的数，连同那一场结束时 `caption.lag_ms` 的 `p50`、`p99`（和台架自己量的每小时 p99 并排放：两种量法应该说同一件事）。

## 不做

- 不调 `judge`，不做任何动作，`Water.health` 是空的，`Column.health` 仍由 `Link` 映射；不发 `judge.verdict`、`stall.snapshot`（P1-17）。
- 不加 `Command::Reconnect`、取钥匙的时限（P1-18）。
- 整场转写、交城、搬运不搬（P1-19）。
- 外壳和界面不调 `pushed`、`drawn`（P1-23、P1-24）。
