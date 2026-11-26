# P1-17 停滞被看见、被处理：看守照 `judge` 行动，留下判断和现场

## 目标

做完以后：

1. 录音任务里的看守每秒采样、判断、行动：服务连着不回话 10 秒，这条 lane 被重开；连不上 20 秒，原因被说出来；采集 10 秒没有交块，录音带着代码结束。
2. 每一栏的健康（`Column.health`）和录音的状态句来自 `judge`，不再来自 lane 自己的说法。`Water` 信号里有每一格的判断。
3. 每一格的健康一变，日志里有一条 `judge.verdict`（从什么到什么、为什么、做了什么）。一格刚变成停滞或停了，日志里留下一组 `stall.snapshot`：当时的整份水位、整份统计、树上跑着的每个任务。同一格、同一个原因只留一次。
4. `i7_a_service_that_takes_audio_and_says_nothing_…`、`i7_a_capture_that_stops_delivering_…` 通过，连同它们「看得见」的断言。

取钥匙的时限、到了重启上限以后的「停了」和「重连」是 P1-18。

## 背景

- 设计：`docs/build/phases/P1.md` §3.5（规则、预算表、动作）、§3.4（`Composer::judged`、`status` 的签名）、§3.13.5（`stall.snapshot`）、§3.13.3（标着 P1-17 的四行）、§2 第二张表里这一件管的两行。`PLAN.md` §4.3、§4.5.2 第三条。
- `core/src/rt/water.rs`（P1-09）：`judge`、`due`（它给的 `Due` 里有动作、`Changed`、`Snapshot`）、`BUDGETS`。`core/src/app/recording.rs`（P1-16）：看守现在只采样、发布；「把一份 `Water` 写成一组 `water.mark`」的函数。
- `rt::stats::write(&Tally, snap)`（P1-05）；`Tree::running()`（P1-05）。
- `core/src/live/lane.rs`：`Conn::live` 的 `select!`；`Link`、`lane.link`（P1-10）。
- `core/src/live/frame.rs`（P1-10）：`Column.health` 现在由 `Link` 映射；`status(paused, links, quiet)`。
- `docs/LESSONS.md`：「连接真的死了必须让界面知道」「发送队列满 = 落后，不等于死亡」「静音时服务仍每 1.0–1.2 秒报一次进度」。

## 做法

### 看守

1. 每秒：`sample` → `judge(&water, BUDGETS)` → `due(..)` 得到这一秒该做的事 → 把判断写进 `Water.health`，`set` 进寄存器 → 把各栏的健康交给帧任务（`Composer::judged`）。看守是这层壳：`judge` 和 `due` 是规则，不发事件；看守照 `due` 说的去做、去说。
2. 动作：
   - `Reopen(Heard(n))`：踢第 `n` 条 lane（见下）；
   - `Tell(code)`：`said.say(code)`；
   - `End(code)`：说这个代码，交给录音的状态机（`Happened::Failed(code)`，P1-16），走停止的次序（已经落盘的留着）；
   - `None`：什么也不做。
3. **判断是事件**：`due` 给的每个 `Changed { stage, from, to }` 发一条 `judge.verdict`（`stage`、`lane`、`from`、`to`、`why`、`action`）。`action` 是这一格此刻该做的动作的代码（`none`、`reopen`、`tell`、`end`）。级别跟着 `to`：`Live` 用 `info!`，`Slow`、`Stalled` 用 `warn!`，`Dead` 用 `error!`。计数：到 `Stalled` 时 `judge.stalled` 加一，到 `Dead` 时 `judge.dead` 加一。
4. **现场**：`due` 给的每个 `Snapshot { stage }` 留一组行（`P1.md` §3.13.5），一个函数做完：
   - 取一个新的 `snap` 号（这个核心里递增，从 1 起；计数器放在 `Stats` 旁边或录音任务里都行，但两场录音之间不重号）；
   - `stall.snapshot`（`snap`、`rec`、`stage`、`lane`、`health`、`why`、`idle_ms`）：`Stalled` 用 `warn!`，`Dead` 用 `error!`；
   - 当时的 `Water`：每格一条 `warn!("water.mark", snap, …)`（P1-16 的那个函数，带上 `snap`）；
   - 当时的 `Tally`：`rt::stats::write(&tally, snap)`，每一行都写；
   - 树上跑着的：`tree.running()`（要整棵树的，从根上问）每行一条 `warn!("task.running", snap, tree, task, kind, for_ms)`；
   - `stall.snapshot.end`（`snap`、`lines`）。
   它在看守的任务上做，是内存里的拷贝加上往日志的队列里放一百来行，不碰磁盘。
5. 一栏的健康：第 `j` 栏用的是第 `max(j, 1) − 1` 条 lane；取它的 `Sent`、`Heard` 两格里最坏的一个（`Dead` > `Stalled` > `Slow` > `Live`）。P1-10 那张从 `Link` 到 `Health` 的映射表删掉：健康只有一个来源。
6. `status` 多一个参数（各栏的健康），规则写成表格能测的样子：暂停 → `Paused`；没有栏，或每一栏都是 `Stalled`/`Dead` → `AudioOnly { why }`（第一个原因；没有栏时是 `quiet`）；有栏的 `Link` 还是 `Connecting` → `Connecting`；有栏是 `Slow` → `Behind`；否则 `Recording`。状态句的变化照旧是 `session.status`（P1-12）。

### lane

7. **踢**：`lane::run` 多收一个踢的口（一个 `watch` 或 `Notify`）。`Conn::live` 的 `select!` 里多一个分支：被踢了就以 `End::Retry("silent")` 结束这条连接。于是照常：`why` 记成 `silent`，重开，说一段空缺。看守踢的时候发一条 `warn!("lane.kick", lane, why = "silent")`，计数 `lane.kicks` 加一。

## 显式与可观测

- **事件**：`judge.verdict`、`stall.snapshot`、`stall.snapshot.end`、`task.running`、`lane.kick`。写进 `docs/EVENTS.md`，并在那里写一小段「怎么读一组快照」（`grep '"snap":N'`）。
- **统计**：计数 `judge.stalled`、`judge.dead`、`lane.kicks`。
- **状态机**：每一格的 `Health`。它是 `judge` 的输出，`due` 说它变没变、该不该行动、该不该留现场；这三样的表格测试在 `core/tests/water.rs`（P1-09），这一件不重写，只在需要时补行。
- **数**：`WATCH_EVERY_MS`（P1-16）；预算都在 `BUDGETS` 表里，这一件不改数。
- **队列、任务**：不造新的队列，不起新的任务。踢的口是一个 `watch` 或 `Notify`，不是队列。
- **代价**：没有事的时候，看守每秒只多一次 `judge` 和 `due`（几十个数的比较），不发任何事件。一组快照一百来行，一场录音里同一格、同一个原因只有一组：日志不会被一次长的停滞灌满。
- **隐私**：`why` 都是预算表和 lane 里写死的代码；`rec` 是录音的 id。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection`（已有；P1-08 可能把它拆成了两条，现在并回一条）：收紧成 12 虚拟秒内换了连接；草稿里空缺的原因是 `silent`；这期间那一栏的健康先 `Slow`、后 `Stalled{why: "silent"}`，重开后回到 `Live`。**看得见**（照 `P1.md` §2 第二张表）：`judge.verdict`（`stage = "Heard"`、`lane = 0`）依次三条：到 `Slow`、到 `Stalled`（`why = "silent"`、`action = "reopen"`）、到 `Live`；`stall.snapshot` 正好一条，同一个 `snap` 号下面的 `water.mark` 和那一秒 `Water` 里的格数一样多、至少一条 `stat.queue`、至少五条 `task.running`（里面有 `lane.0` 和 `watch`），`stall.snapshot.end` 的 `lines` 等于这一组真的行数；`lane.kick` 一条；`part.state`（`part = "lane.0"`、`why = "silent"`）一条；计数 `judge.stalled`、`lane.kicks` 都是 1。
2. `i7_a_capture_that_stops_delivering_ends_the_recording_out_loud`：话筒卡住（不交块，也不说结束）。11 虚拟秒内：`said` 里有 `audio.stalled`，`session` 是 `None`，草稿和 tape 都留着。**看得见**：`judge.verdict`（`stage = "Capture"`）到 `Slow`、再到 `Stalled`（`why = "audio.stalled"`、`action = "end"`）；`stall.snapshot` 一组；`recording.phase` 从 `Recording` 到 `Closing.Capture`，`why = "audio.stalled"`；`recording.end`（`code = "audio.stalled"`），级别 `error`。
3. `a_service_that_lags_is_behind_not_dead`：服务的延迟调到 4 秒：那一栏是 `Slow`，状态句是 `Behind`；调回来以后是 `Live`、`Recording`。没有重连过。**看得见**：`judge.verdict` 到 `Slow`、回到 `Live` 各一条；**没有** `stall.snapshot`，没有 `lane.kick`。
4. `nothing_is_done_twice_for_one_stall`：服务一直不回话 35 秒：重开发生在第 10、20、30 秒前后各一次，不是每秒一次；`silent` 没有被当作提示说出来（`Reopen` 不是 `Tell`）。**看得见**：`lane.kick` 三条；`stall.snapshot` 只有一组（同一格、同一个原因）；`judge.verdict` 到 `Stalled` 的次数和它真的从别的状态变成 `Stalled` 的次数一样，不是每秒一条。
5. `a_recording_with_nothing_wrong_leaves_no_verdict_behind`：好好地录 5 虚拟分钟：`judge.verdict` 里没有 `Stalled`、没有 `Dead`；没有 `stall.snapshot`；计数 `judge.stalled` 是 0。（开头连接的那一两秒有没有 `Slow`，照实断言。）
5a. `a_recording_without_a_key_is_quiet_from_start_to_stop`：台架「不放钥匙」（P1-08），录 5 虚拟分钟再停：一条 `judge.verdict` 也没有（没有 lane 可判；`Capture`、`Tape`、`Draft` 一直是 `Live`）；没有 `stall.snapshot`、`lane.kick`、`notice.said`；状态句从头到尾是 `AudioOnly { why: "captions.no_key" }`；`Water.health` 里四格都是 `Live`。不填钥匙是一种录法，不是一次故障（`P1.md` §3.14.5）。

`core/tests/live.rs`：

6. `a_kick_ends_the_connection_and_the_lane_opens_another`：连着的时候踢一下：服务那头看到连接断了，新的连接进来，lane 说了一段原因是 `silent` 的空缺。**看得见**：`lane.link` 从 `Live` 到 `Connecting` 再到 `Live`；`lane.gap`（`why = "silent"`）一条。

`core/tests/frame.rs`：

7. `the_status_line_says_what_most_needs_saying`：加上健康这个参数，表格逐行（上面第 6 条做法里的每一种）。

## 验收

- `scripts/build/check.sh` 全绿。`core/tests/liveness.rs` 里不再有 `ignore = "P1-17`。
- 报告里贴：第 1 条测试里那一组快照的全部行（真的日志的样子：这是负责人以后查停滞时会看到的东西）；第 2 条里 `Stalled` 那一秒的 `Water`（JSON）；`i1_eight_hours_…` 的数（看守每秒多做一次判断，确认没有影响），和那 8 小时里一共有几条 `judge.verdict`、几组快照（应该只有开头连接时的几条，没有快照）。
- 隐私测试（P1-08）照旧通过：它现在的那一场里会有一次 lane 崩溃，日志里多了 `judge.verdict` 这些行。

## 不做

- 不加取钥匙的时限、请求的超时、`Command::Reconnect`，不处理「到了重启上限」（P1-18）；`i7_offline_…` 和 `a_lease_that_never_comes_…` 是那一件的。
- 不判断、不处理 `Shell`、`Screen` 两格以外壳为主的事：这两格的位置由 P1-23、P1-24 开始报，表里它们的动作本来就是「不做」。
- 不改预算表的数字（要改先写进 `P1.md` §3.5 再改代码）。
- 设备那一层（`capture::audio::hold`）的重开不设上限，不动。
- 不把快照做成一条嵌套的事件，不为它放宽字段的类型（`P1.md` §3.13.5）。
