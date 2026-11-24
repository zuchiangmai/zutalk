# 事件：读日志的词典

> 2026-11-21 起（P1-03）。设计在 `docs/build/phases/P1.md` §3.13.1–§3.13.3。每件任务加了事件，就在下面的表里加一行；`scripts/build/rules/events.sh` 保证表和代码一一对应：代码里有、表里没有，或者表里有、代码里没有，门禁都不过。

ZuTalk 出了什么事，只看日志就要讲得出来。日志里只有**代码和数**：没有一句话、一个名字、一把钥匙。

## 在哪里

- 应用：`<数据目录>/logs/zutalk.jsonl`。开发构建同时打到标准错误。
- 服务器：`<ZUTALK_DATA>/logs/zutalk-server.jsonl`，同时打到标准错误（部署的人从那里收）。
- 一个文件写到 8 MB 就改名成 `zutalk.1.jsonl`，原来的 `.1` 变 `.2`，依次往后；一共留 5 个，最旧的删掉。换过以后，新文件的第一行是 `log.turned`。重新启动接着往现有的文件后面写。
- 级别由环境变量 `ZUTALK_LOG` 定：`debug`，否则是 `info`。

## 一行怎么读

一行一个 JSON 对象，平铺，不嵌套：

```json
{"t_ms":1763712000123,"seq":4182,"level":"warn","component":"zutalk_core::rt::supervise","event":"part.state","part":"lane.0","from":"Running","to":"Waiting","why":"silent","wait_ms":250,"n":1}
```

每一行开头的五个字段：

| 字段 | 意思 |
|---|---|
| `t_ms` | 墙上时钟的毫秒 |
| `seq` | 这个进程里的第几行，从 1 起。中间缺了号，就是丢了行（写线程来不及，见 `log.lost`）。写线程自己补的两种行（`log.turned`、`log.lost`）`seq` 是 0：它们不占号，所以缺号只表示丢行 |
| `level` | `error`、`warn`、`info`、`debug` |
| `component` | 发它的模块（Rust 的模块路径） |
| `event` | 事件的代码，下面的表里查 |

其余是事件自己的字段。同一个名字在所有事件里意思一样：

| 字段 | 意思 |
|---|---|
| `rec` | 录音的 id。只有 `recording.*`、`session.status`、`stall.snapshot` 带 |
| `lane` | 第几条 lane（0 起） |
| `stage` | 水位表的哪一格 |
| `part`、`task`、`tree`、`thread`、`queue`、`window` | 名字：都是源码里写死的字符串 |
| `kind` | 命令、活、消息的种类 |
| `code` | 结果：`ok`，或者一个稳定的失败代码 |
| `why` | 原因的代码；没有就是空串 |
| `from`、`to` | 状态机的状态 |
| `n` | 个数或第几次；`seq` 是序号；`snap` 是快照的号（0 表示不属于任何快照） |
| 带单位的数 | 名字以单位结尾：`_ms`、`_us`、`_bytes`、`_blocks`、`_lines`、`_samples` |

一个值是 `"!"`：那个字段被退回了。它不是数、布尔、代码或合规的 id（文字里只许 `[A-Za-z0-9._:/-]`，最多 64 字节），或者它的名字和开头的五个字段撞了。退回的个数记在 `zutalk_log::refused()` 里。

## 级别

| 级别 | 意思 |
|---|---|
| `error` | 会让人丢东西的 |
| `warn` | 出了岔子、自己恢复了的 |
| `info` | 状态转换、命令、活 |
| `debug` | 每帧、每块、每秒级别的；默认不开 |

## 永远不在日志里的

- 话里的任何一个词（原话和译文）、标题、说话人的标签、笔记、搜的词；
- 钥匙、秘密、恢复密钥；
- DID（本机身份的 `did:key` 也算）、句柄、房间的 id、任何链接、设备的名字、文件的路径（`app.panic` 里的是源码的路径，不在此列）；
- panic 的消息（里面可能格式化进了内容）；服务器请求的正文、签名的人、查询串。

这些由类型守着：字段只能是数、布尔、源码里写死的代码（`&'static str`）和明写的 `Id(..)`；一个 `String` 或借来的 `&str` 放进去**编不过**。进日志的 id 只有录音的 id 和窗口的标签。

## 事件

| 事件 | 级别 | 谁、什么时候 | 字段 |
|---|---|---|---|
| `app.start` | info | 外壳装好日志以后的第一行 | `version` `os` `arch` `level` |
| `app.panic` | error | panic 钩子：任何线程 panic 时。接着把日志刷到盘上，再交给原来的钩子 | `thread`（线程的名字，没有名字是 `unnamed`） `file`（源码的路径；工作区外的只留最后两段） `line` |
| `log.turned` | info | 写线程换了文件：新文件的第一行。`seq` 是 0 | `files`（换完一共几个文件） |
| `log.lost` | warn | 写线程：上一行之后队列满过，丢了行。写在下一行之前。`seq` 是 0 | `n`（丢了几行） |
| `server.start` | info | 服务器装好日志以后的第一行 | `version` |
| `server.request` | info；状态 ≥ 500 时 warn | 服务器答完一个请求 | `route`（路由表 `ROUTES` 里的代码，不是路径；没有对上任何路由是 `none`） `status` `ms` |
| `server.sweep` | debug | 服务器每分钟清扫一次房间以后 | `ms` |
| `queue.full` | warn | 一条队列从「有位置」变成「满了」的那一次 | `queue` `policy` `capacity` |
| `queue.clear` | info | 满过的队列又放进去了 | `queue` `lost`（这一段里被退回、丢掉的个数） |
| `stats.full` | warn | 统计的行数到了上限，或名字超长（一种一次） | `kind`（`counts` / `peaks` / `spreads` / `queues` / `tasks`） |
| `task.start` | info | 树上一个任务开始 | `tree` `task` |
| `task.end` | info；panic 时 error | 树上一个任务结束 | `tree` `task` `code`（`done` / `cancelled` / `panic`） `ran_ms` |
| `task.refused` | warn | 往一棵已经收尾的树上起任务 | `tree` `task` |
| `thread.start` | info | 一个有名字的线程开始 | `thread` |
| `thread.end` | info；panic 时 error | 一个有名字的线程结束 | `thread` `code`（`done` / `panic`） `ran_ms` |
| `stats.summary` | info | 每分钟一次，后面跟着这一分钟动过的行 | `snap` `up_ms` `rows` |
| `stat.count` | info | 汇总和快照里的计数行 | `snap` `name` `n` |
| `stat.peak` | info | 汇总和快照里的高水位行 | `snap` `name` `now` `most` |
| `stat.spread` | info | 汇总和快照里的分布行 | `snap` `name` `n` `p50` `p99` `most` |
| `stat.queue` | info | 汇总和快照里的队列行 | `snap` `name` `capacity` `depth` `deepest` `sent` `full` `lost` |
| `identity.who` | info；`to = "nobody"` 时 warn | 启动、登录、登出换了签记录的身份。`made` 为真而这不是第一次启动，说明秘密丢过，旧的本机书架还在盘上（`P1.md` §3.14.8 第 5 条） | `from` `to`（`nobody` / `local` / `person`） `why`（`boot` / `sign_in` / `sign_out`） `made` |
| `write.skipped` | info | 有音频的草稿没有送去写整场记录：没有钥匙可取。停止以后一条（`n = 1`）；启动或切换身份清点时有就一条 | `n` `why`（`keys.none`） |
| `part.state` | info；`to = "Waiting"` 时 warn | 受监督的部件每次状态转换 | `part` `from` `to`（`Opening` / `Running` / `Waiting` / `Ended`） `why` `wait_ms` `n` |
| `part.gave_up` | error | 受监督的部件到了重启上限，不再自己重试 | `part` `why` `n` |
| `lane.link` | info；到 `Behind`、`Stopped`（不是 `done`）时 warn | lane 的连接状态变了 | `lane` `from` `to`（`Connecting` / `Live` / `Behind` / `Stopped`） `why` |
| `lane.gap` | warn | lane 说了一段空缺 | `lane` `t0_ms` `t1_ms` `why` |
| `frame.fit` | info | 一帧为了放得下而让掉的东西变了 | `from` `to`（`Whole` / `Lines` / `Units` / `Tail`） `bytes` |
| `notice.said` | info | 一个代码被说给人听 | `code` |
| `recording.start` | info | 一场录音开始 | `rec` `source`（`mic` / `system` / `both` / `file`） `lanes` `columns` `captions`（`live` / `file` / `captions.off` / `captions.no_key`） |
| `recording.end` | info；带失败的代码时 error | 一场录音结束（到封好 tape 为止） | `rec` `code`（`done` 或结束它的失败代码） `ms` |
| `session.status` | info | 录音的状态句变了 | `rec` `from` `to`（`Recording` / `Paused` / `AudioOnly` / `Connecting` / `Behind`） `why` |
| `command.done` | info；被拒绝时 warn | hub 处理完一条命令 | `kind` `code`（`ok`：收下了；或拒绝它的代码） `took_us` |
| `command.refused` | warn | 命令没进邮箱 | `kind` `code`（`busy`） |
| `hub.turn.slow` | warn | hub 处理一条消息超过预算（`HUB_TURN_BUDGET_US`） | `kind` `took_us` |
| `capture.source.open` | info | 一个声源的设备打开了 | `source`（第几个声源，0 起） `ms` |
| `capture.source.broke` | warn | 一个声源的设备坏了，退避后重开 | `source` `why` `wait_ms` |
| `capture.source.ended` | error | 一个声源的设备重开也没用，放弃它 | `source` `why` |
| `capture.pad` | warn | 时钟的策略补了一段静音 | `ms` |
| `capture.end` | error | 时钟的策略结束了采集（如合盖睡眠） | `why`（`asleep`） |
| `capture.pause` | info | 暂停或继续到了泵 | `paused` |
| `capture.ring.lost` | warn | 泵这一圈发现某个声源的环丢过样本 | `source` `samples` |
| `keep.lost` | warn | 写盘的部件丢了一批（写出错或 panic） | `part` `n`（这一批几个） `why`（错误的代码或 `panic`） |
| `tape.pad` | warn | tape 上的洞补成了静音 | `from_ms` `blocks` |
| `draft.gap` | warn | 草稿补了一行空缺 | `t0_ms` `t1_ms` `lines`（丢了几行） |
| `recording.phase` | info；到 `Ended` 带失败代码时 error | 录音的状态机转换 | `rec` `from` `to`（`Opening` / `Recording` / `Paused` / `Closing.Capture` / `Closing.Tape` / `Closing.Lanes` / `Closing.Frame` / `Closing.Draft` / `Sealing` / `Ended`） `why` `ms`（上一个状态待了多久） |
| `water.mark` | debug；在快照里是 warn | 看守每秒采样时每格一行（`snap = 0`）；停滞快照里每格一行（`snap > 0`） | `snap` `stage` `lane`（无 lane 的格为 `-1`） `at` `moved_ms` `why` `ended` |
