# P1-11 停滞被看见、被处理：看守、`judge` 的动作、重连

## 目标

做完以后：

1. 录音任务里的看守每秒采样、判断、行动：服务连着不回话 10 秒，这条 lane 被重开；连不上 20 秒，原因被说出来；采集 10 秒没有交块，录音带着代码结束。
2. 每一栏的健康（`Column.health`）和录音的状态句来自 `judge`，不再来自 lane 自己的说法。`Water` 信号里有每一格的判断。
3. 取钥匙有时限；`reqwest` 有超时。0.8 的「取钥匙卡了 14 分钟，录音键一直亮」在这里是：10 秒后这条 lane 记下原因 `keys`，20 秒后界面说出来，到重启上限后这一栏是「停了」，并给出「重连」。
4. `Command::Reconnect` 把停了的 lane 重新起起来。
5. I7 的测试通过。

## 背景

- 设计：`docs/build/phases/P1.md` §3.5（规则、预算表、动作）、§3.6（重启上限、`Reconnect`）、§3.4（`Composer::judged`、`status` 的签名）。`PLAN.md` §4.3、§4.2 最后一行、附表里「取钥匙卡了 14 分钟」一行。
- `core/src/rt/water.rs`（P1-04）：`judge`、`due`（或它在那里的名字）、`BUDGETS`。`core/src/app/recording.rs`（P1-10）：看守现在只采样、发布。
- `core/src/live/lane.rs`：`open` 里 `lane.keys.lease(Service::Soniox).await?` 没有时限；`Conn::live` 的 `select!`；`run` 在 `supervise` 以 `Fatal` 结束后说 `Link::Stopped(code)`。
- `core/src/live/frame.rs`（P1-05）：`Column.health` 现在由 `Link` 映射；`status(paused, links, quiet)`。
- `core/src/app/hub.rs`：`Core::start` 里 `reqwest::Client::builder().build()` 没有任何超时；这个客户端也用来传整场转写的音频和交城，所以不能设「整个请求最多多久」。
- 界面：`ui/src/ui/Live.svelte` 的状态句；`ui/dev/mock.ts` 的 `handlers` 对每个命令都要有一项（类型检查守着）。
- `docs/LESSONS.md`：「连接真的死了必须让界面知道」「发送队列满 = 落后，不等于死亡」「静音时服务仍每 1.0–1.2 秒报一次进度」。

## 做法

### 看守

1. 每秒：`sample` → `judge(&water, BUDGETS)` → `due(..)` 得到这一秒该做的动作 → 把判断写进 `Water.health`，`set` 进寄存器 → 把各栏的健康交给帧任务（`Composer::judged`）。
2. 动作：
   - `Reopen(Heard(n))`：踢第 `n` 条 lane（见下）；
   - `Tell(code)`：`said.say(code)`；
   - `End(code)`：说这个代码，走停止的次序（已经落盘的留着）；
   - `None`：什么也不做。
3. 一栏的健康：第 `j` 栏用的是第 `max(j, 1) − 1` 条 lane；取它的 `Sent`、`Heard` 两格里最坏的一个（`Dead` > `Stalled` > `Slow` > `Live`）。P1-05 那张从 `Link` 到 `Health` 的映射表删掉：健康只有一个来源。
4. `status` 多一个参数（各栏的健康），规则写成表格能测的样子：暂停 → `Paused`；没有栏，或每一栏都是 `Stalled`/`Dead` → `AudioOnly { why }`（第一个原因；没有栏时是 `quiet`）；有栏的 `Link` 还是 `Connecting` → `Connecting`；有栏是 `Slow` → `Behind`；否则 `Recording`。

### lane

5. **踢**：`lane::run` 多收一个踢的口（一个 `watch` 或 `Notify`）。`Conn::live` 的 `select!` 里多一个分支：被踢了就以 `End::Retry("silent")` 结束这条连接。于是照常：`why` 记成 `silent`，重开，说一段空缺。
6. **取钥匙的时限**：`timeout(PATIENCE, lease)`，到时是 `End::Retry("keys")`。
7. **到了重启上限**：`supervise` 返回 `Fatal(why)`（P1-04），lane 说 `Link::Stopped(why)`，它的 `Gauge` 记 `ended(why)`。`judge` 给出 `Dead{why}` 和一次 `Tell(why)`。
8. **`Command::Reconnect`**：hub 交给录音任务；录音任务把已经结束（而录音还没停）的每条 lane 重新起起来。新的 lane 从旧的 `Sent` 水位接着（`lane::run` 能带着起点的 `edge` 和原因开始），所以它第一块到来时说一段空缺，盖住停着的那段时间。没有录音时：`recording.none`。

### 超时

9. `Core::start` 的客户端：`connect_timeout(10 s)`、`read_timeout(30 s)`。不设整个请求的 `timeout`。

### 界面（最小）

10. `Live.svelte`：有一栏是 `Dead` 时，在状态句旁给一个「重连」按钮，发 `Reconnect`。`ui/lang/*.json` 加这个按钮的词和 `code.keys`（`code.silent`、`code.shell` P1-04 已加）。`ui/dev/mock.ts` 的 `handlers` 加 `Reconnect`。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection`（已有）：收紧成 12 虚拟秒内换了连接；草稿里空缺的原因是 `silent`；这期间那一栏的健康先 `Slow`、后 `Stalled{why: "silent"}`，重开后回到 `Live`。
2. `i7_offline_is_said_and_captions_stop_trying_until_asked`：`Dial` 一直失败。
   - 第 21 秒之前：`said` 里出现一次 `connect`；那一栏是 `Stalled{why: "connect"}`；状态句是 `AudioOnly { why: "connect" }`；声音照录（`Capture`、`Tape` 两格在走）。
   - 第 30 秒之前（第 8 次失败在 23.75 秒）：那一栏是 `Dead{why: "connect"}`；原因没变，`connect` 不说第二遍。
   - 之后 5 虚拟分钟：`Dial` 被叫的次数不再增加，`said` 里仍然只有那一次 `connect`。
   - 让 `Dial` 恢复，发 `Reconnect`：5 秒内词又上屏；草稿里有一行空缺盖住整段没有字的时间。
3. `i7_a_capture_that_stops_delivering_ends_the_recording_out_loud`：话筒卡住（不交块，也不说结束）。11 虚拟秒内：`said` 里有 `audio.stalled`，`session` 是 `None`，草稿和 tape 都留着。
4. `a_service_that_lags_is_behind_not_dead`：服务的延迟调到 4 秒：那一栏是 `Slow`，状态句是 `Behind`；调回来以后是 `Live`、`Recording`。没有重连过。
5. `nothing_is_done_twice_for_one_stall`：服务一直不回话 35 秒：重开发生在第 10、20、30 秒前后各一次，不是每秒一次；`silent` 没有被当作提示说出来（`Reopen` 不是 `Tell`）。

`core/tests/live.rs`：

6. `a_lease_that_never_comes_is_given_up_on`：钥匙的替身永远不回。第 10 秒 lane 的 `because` 是 `keys`；之后照退避再取；120 秒后（没有一次健康的运行）lane 以 `Stopped("keys")` 结束。取钥匙总共不超过 12 次。全程没有拨过号。
7. `a_kick_ends_the_connection_and_the_lane_opens_another`：连着的时候踢一下：服务那头看到连接断了，新的连接进来，lane 说了一段原因是 `silent` 的空缺。

`core/tests/frame.rs`：

8. `the_status_line_says_what_most_needs_saying`：加上健康这个参数，表格逐行（上面第 4 条做法里的每一种）。

`core/tests/app.rs`：

9. `reconnect_with_nothing_recording_says_so`。

## 验收

- `scripts/build/check.sh` 全绿。`core/tests/liveness.rs` 里不再有 `ignore = "P1-11`。
- `grep -n "lease(" core/src/live/lane.rs` 的那一行在 `timeout` 里面。
- 报告里贴第 2 条测试里三个时刻的 `Water`（JSON）：说出 `connect` 的那一秒、变成 `Dead` 的那一秒、重连成功后的那一秒；再贴一次 `i1_eight_hours_…` 的数（看守每秒多做一次判断，确认没有影响）。

## 不做

- 不判断、不处理 `Shell`、`Screen` 两格以外壳为主的事：这两格的位置由 P1-14 开始报，表里它们的动作本来就是「不做」。
- 不改预算表的数字（要改先写进 `P1.md` §3.5 再改代码）。
- 设备那一层（`capture::audio::hold`）的重开不设上限，不动。
- 不做「网络回来了自己重连」：到了上限就等人点。
