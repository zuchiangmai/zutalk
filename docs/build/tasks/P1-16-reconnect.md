# P1-16 连不上不是无限重试：时限、「停了」、重连

## 目标

做完以后：

1. 取钥匙有时限；`reqwest` 有超时。0.8 的「取钥匙卡了 14 分钟，录音键一直亮」在这里是：10 秒后这条 lane 记下原因 `keys`，20 秒后界面说出来，到重启上限后这一栏是「停了」，并给出「重连」。
2. 一条 lane 到了重启上限就停下，不再自己重试；它那一栏是 `Dead{why}`，原因只说一次。
3. `Command::Reconnect` 把停了的 lane 重新起起来。
4. lane 每开一次连接，日志里有一条 `lane.open`：成了没有、卡在哪一步、取钥匙和拨号各用了多久。
5. `i7_offline_is_said_and_captions_stop_trying_until_asked`、`a_lease_that_never_comes_is_given_up_on` 通过，连同「看得见」的断言。I7 在核心这一层成立。

## 背景

- 设计：`docs/build/phases/P1.md` §3.5（`Dead` 的规则）、§3.6（重启上限、`Reconnect`）、§3.13.3（标着 P1-16 的两行）、§2 第二张表里这一件管的两行。`PLAN.md` §4.2 最后一行、§4.3 末段、附表里「取钥匙卡了 14 分钟」一行。
- `core/src/live/lane.rs`：`open` 里 `lane.keys.lease(Service::Soniox).await?` 没有时限；拨号已经有 `timeout(PATIENCE_MS)`；`run` 在监督以 `Fatal` 结束后说 `Link::Stopped(code)`；lane 的 `Gauge`（P1-14）；踢的口（P1-15）。
- `core/src/rt/supervise.rs`（P1-07）：`Restarts`、`part.gave_up`。
- `core/src/app/recording.rs`（P1-14、P1-15）：`recording` 子树、看守、`Phase`。
- `core/src/app/hub.rs`：`Core::start` 里 `reqwest::Client::builder().build()` 没有任何超时；这个客户端也用来传整场转写的音频和交城，所以不能设「整个请求最多多久」。
- 界面：`ui/src/ui/Live.svelte` 的状态句；`ui/dev/mock.ts` 的 `handlers` 对每个命令都要有一项（类型检查守着）。

## 做法

### lane

1. **取钥匙的时限**：`timeout(PATIENCE_MS, lease)`，到时是 `End::Retry("keys")`。`PATIENCE_MS` 的注释补一句：取钥匙、拨号、写一帧都用它。
2. **每开一次连接一条事件**：`open` 的出口（成和不成都经过的一处）发 `lane.open`（`lane`、`code`、`keys_ms`、`dial_ms`）。`code` 是 `ok`，或者这一次没成的原因（`keys`、`connect`、`key`…），没成时用 `warn!`。两个用时用 `tokio::time::Instant`（是等待，不是干活），同时记进分布 `lane.keys_ms`、`lane.dial_ms`；没走到拨号那一步时 `dial_ms` 是 0。
3. **到了重启上限**：监督返回 `Fatal(why)`（P1-07，它自己发 `part.gave_up`），lane 说 `Link::Stopped(why)`，它的 `Gauge` 记 `ended(why)`。`judge` 给出 `Dead{why}` 和一次 `Tell(why)`；看守照 P1-15 发 `judge.verdict`。同一格、同一个原因的现场在变成 `Stalled` 时已经留过，变成 `Dead` 不再留（`due` 管着）。
4. **`Command::Reconnect`**：hub 交给录音任务；录音任务把已经结束（而录音还没停）的每条 lane 重新起起来，仍然用它原来的名字挂在 `recording` 子树上，并发一条 `info!("lane.reconnect", lane)`。新的 lane 从旧的 `Sent` 水位接着（`lane::run` 能带着起点的 `edge` 和原因开始），所以它第一块到来时说一段空缺，盖住停着的那段时间；它的 `Gauge` 的 `ended` 清掉。没有录音时：`recording.none`。没有停了的 lane 时：什么也不做，命令照样是 `ok`。
   - 录音的状态机（`Phase`）不为它多一个状态：重连只在 `Recording`、`Paused` 里有效，`Closing` 以后来的 `Reconnect` 不做事。把这一行加进 P1-14 的转换表测试。

### 超时

5. `Core::start` 的客户端：`connect_timeout(CONNECT_TIMEOUT_MS)`（10 000）、`read_timeout(READ_TIMEOUT_MS)`（30 000），都是带单位的常量，注释说理由（连不上 10 秒和 lane 的耐心一致；30 秒读不到一个字节的连接是死的，而整场转写的上传是一直有字节在走的）。不设整个请求的 `timeout`。

### 界面（最小）

6. `Live.svelte`：有一栏是 `Dead` 时，在状态句旁给一个「重连」按钮，发 `Reconnect`。`ui/lang/*.json` 加这个按钮的词和 `code.keys`（`code.silent`、`code.shell` P1-07 已加）。`ui/dev/mock.ts` 的 `handlers` 加 `Reconnect`。

## 显式与可观测

- **事件**：`lane.open`、`lane.reconnect`。写进 `docs/EVENTS.md`。`part.gave_up`、`judge.verdict`、`command.done`（`kind = "Reconnect"`）是已有的。
- **统计**：分布 `lane.keys_ms`、`lane.dial_ms`。
- **数**：`CONNECT_TIMEOUT_MS`、`READ_TIMEOUT_MS`；`PATIENCE_MS`（已有）。重启上限的三个数在 `Restarts`（P1-07），这一件不改。
- **队列、任务**：不造新的队列。重新起的 lane 是 `recording` 子树上的任务，名字不变。
- **0.8 那次事故在日志里的样子**（写进报告，从测试第 2 条的捕获里抄）：一串 `lane.open`（`code = "keys"`、`keys_ms = 10000`）和 `part.state`（退避越来越长），第 20 秒一条 `judge.verdict`（到 `Stalled`，`why = "keys"`、`action = "tell"`）和一组 `stall.snapshot`，到上限时一条 `part.gave_up` 和一条 `judge.verdict`（到 `Dead`）。不用猜它在干什么。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `i7_offline_is_said_and_captions_stop_trying_until_asked`：`Dial` 一直失败。
   - 第 21 秒之前：`said` 里出现一次 `connect`；那一栏是 `Stalled{why: "connect"}`；状态句是 `AudioOnly { why: "connect" }`；声音照录（`Capture`、`Tape` 两格在走）。
   - 第 30 秒之前（第 8 次失败在 23.75 秒）：那一栏是 `Dead{why: "connect"}`；原因没变，`connect` 不说第二遍。
   - 之后 5 虚拟分钟：`Dial` 被叫的次数不再增加，`said` 里仍然只有那一次 `connect`。
   - 让 `Dial` 恢复，发 `Reconnect`：5 秒内词又上屏；草稿里有一行空缺盖住整段没有字的时间。
   - **看得见**（照 `P1.md` §2 第二张表）：`lane.open`（`code = "connect"`）正好八条；`part.gave_up`（`part = "lane.0"`、`why = "connect"`、`n = 8`）一条，级别 `error`；`judge.verdict`（`stage = "Sent"`）到 `Stalled`、再到 `Dead`；`stall.snapshot` 只有一组；`notice.said`（`code = "connect"`）一条；那 5 分钟里没有新的 `lane.open`、没有新的 `part.state`。重连以后：`command.done`（`kind = "Reconnect"`、`code = "ok"`）、`lane.reconnect`（`lane = 0`）、`lane.open`（`code = "ok"`）、`lane.link` 到 `Live`、`judge.verdict` 回到 `Live`、`lane.gap` 一条。计数 `judge.dead` 是 1。

`core/tests/live.rs`：

2. `a_lease_that_never_comes_is_given_up_on`：钥匙的替身永远不回。第 10 秒 lane 的 `because` 是 `keys`；之后照退避再取；120 秒后（没有一次健康的运行）lane 以 `Stopped("keys")` 结束。取钥匙总共不超过 12 次。全程没有拨过号。**看得见**：`lane.open`（`code = "keys"`、`keys_ms = 10000`、`dial_ms = 0`）的条数等于取钥匙的次数；`part.gave_up`（`why = "keys"`）一条；`lane.link` 最后到 `Stopped`，`why = "keys"`；lane 的 `Gauge` 的 `ended` 是 `keys`；分布 `lane.keys_ms` 的个数等于取钥匙的次数。
3. `every_open_says_how_it_went`：对着脚本服务正常连一次、被服务拒绝钥匙一次：`lane.open` 两条，`code` 是 `ok` 和那个拒绝的代码。

`core/tests/app.rs`：

4. `reconnect_with_nothing_recording_says_so`：`recording.none`；`command.done`（`kind = "Reconnect"`、`code = "recording.none"`）。

`core/tests/recording.rs`：

5. 转换表补一行：`Closing` 以后来的重连不改变状态、没有效果。

## 验收

- `scripts/build/check.sh` 全绿。`core/tests/liveness.rs` 里不再有 `ignore = "P1-16`。
- `grep -n "lease(" core/src/live/lane.rs` 的那一行在 `timeout` 里面。
- 报告里贴第 1 条测试里三个时刻的 `Water`（JSON）：说出 `connect` 的那一秒、变成 `Dead` 的那一秒、重连成功后的那一秒；再贴「0.8 那次事故在日志里的样子」那一段真的日志行。

## 不做

- 不做「网络回来了自己重连」：到了上限就等人点。
- 不改预算表和重启上限的数字。
- 设备那一层的重开不设上限，不动。
- 不给整场转写、交城的请求加整体的时限（它们有自己的进度和再试，P1-19）。
