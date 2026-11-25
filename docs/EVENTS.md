# 事件：读日志的词典

> 2026-11-21 起（P1-03 建，P1-25 按部件整理）。设计在 `docs/build/phases/P1.md` §3.13.1–§3.13.3。`scripts/build/rules/events.sh` 守着代码里的全部事件与本表一一对应；`core/tests/liveness.rs::every_event_in_the_catalogue_can_be_told_apart` 守着本表没有重复行且全部通过 `zutalk_log::is_code`。

ZuTalk 出了什么事，只看日志就要讲得出来。日志里只有**代码和数**：没有一句话、一个名字、一把钥匙。

## 怎么读一行

- **在哪里**：应用写 `<数据目录>/logs/zutalk.jsonl`（开发构建同时打到标准错误）；服务器写 `<ZUTALK_DATA>/logs/zutalk-server.jsonl` 并同时打到标准错误。
- **轮换**：单个文件到 8 MB 改名成 `zutalk.1.jsonl`，旧文件顺延，最多留 5 个，最旧的删掉；换完新文件第一行是 `log.turned`。重启接着往现有文件末尾写。
- **级别**：环境变量 `ZUTALK_LOG=debug` 开 `debug`，否则是 `info`。

一行一个平铺 JSON 对象，不嵌套：

```json
{"t_ms":1763712000123,"seq":4182,"level":"warn","component":"zutalk_core::rt::supervise","event":"part.state","part":"lane.0","from":"Running","to":"Waiting","why":"silent","wait_ms":250,"n":1}
```

每一行开头的五个字段：

| 字段 | 意思 |
|---|---|
| `t_ms` | 墙上时钟的毫秒 |
| `seq` | 本进程的第几行，从 1 起。中间缺号表示写线程队列满过并丢了行（见 `log.lost`）。写线程自己补的 `log.turned`、`log.lost` 不占号，`seq` 为 0 |
| `level` | `error`（会丢东西或中止）、`warn`（出岔子但自己恢复或拒绝单次请求）、`info`（状态转换、命令、活）、`debug`（每秒水位、每分钟清扫，默认不开） |
| `component` | 发它的 Rust 模块路径 |
| `event` | 稳定的事件代码，见下文事件表 |

其余是事件自己的字段，同名同义：

| 字段 | 意思 |
|---|---|
| `rec` | 录音的 id（开始时间十六进制加八位随机十六进制）。只有 `recording.*`、`session.status`、`stall.snapshot` 带；其他事件靠时间归属（同一时刻至多一场录音） |
| `lane` | 第几条 lane（0 起；无 lane 的水位格为 `-1`） |
| `stage` | 水位表的哪一格（`Capture` / `Tape` / `Draft` / `Sent` / `Heard` / `Final` / `Frame` / `Shell` / `Screen`） |
| `part`、`task`、`tree`、`thread`、`queue`、`window` | 源码里写死的部件、任务、树、线程、队列名，或窗口标签 |
| `kind` | 命令、活、消息或统计的种类 |
| `code` | 结果：`ok`、`done` 或稳定的失败代码 |
| `why` | 原因代码；没有原因就是空串 `""` |
| `from`、`to` | 状态机转换前后的状态代码 |
| `n`、`job`、`snap` | 个数或第几次；活的递增序号（避开行头 `seq`）；停滞快照编号（0 表示非快照） |
| 带单位的数 | 以单位结尾：`_ms`、`_us`、`_bytes`、`_blocks`、`_lines`、`_samples` |

字段值出现 `"!"` 表示被运行时文法退回（不满足 `[A-Za-z0-9._:/-]` 或超过 64 字节，或字段名与行头冲突），次数记在 `zutalk_log::refused()` 和 `log.refused`。

## 哪些东西永远不在日志里

- 话里的任何一个词（原话与译文）、标题、说话人标签、笔记、搜索词；
- 钥匙、秘密、恢复密钥；
- DID（含本机身份的 `did:key`）、句柄、房间 id、链接、设备名、磁盘路径（`app.panic` 的源码路径除外）；
- panic 消息文本、服务器请求正文、签名者与查询串。

这由三层守住：编译期 `Plain` trait 只接受数、布尔、`&'static str` 与显式 `Id(..)`（`String` 和普通 `&str` 编不过）；运行期 `is_plain` 检查所有字符串；门禁的隐私测试（`nothing_said_or_secret_reaches_the_log` 与 `a_diagnosis_holds_nothing_that_was_said_and_no_secret`）验证落盘字节。P1 里经 `Id(..)` 进日志的只有录音 id、窗口标签与状态原因代码。

## 怎么读一组快照

当某一格刚变成 `Stalled` 或 `Dead` 时，看守用同一个递增的 `snap` 号（从 1 起，同一核心跨场不重号）连续写出一组平铺日志行。排查一次停滞只需按号拉出整组现场：

```sh
grep '"snap":1' <数据目录>/logs/zutalk.jsonl
```

同一 `snap` 号内依次是：

1. `stall.snapshot`：触发快照的录音 id、格、健康状态、原因与已不动毫秒数；
2. `water.mark`（每格一行）：此刻整份水位；
3. `stat.count` / `stat.peak` / `stat.spread` / `stat.queue`：此刻整份统计与各队列深度；
4. `task.running`：根树上此刻正在运行的所有任务与阻塞调用及其已运行毫秒数；
5. `stall.snapshot.end`：本组总行数 `lines`（含头尾），用于核对未丢行。

同一场录音里同一格、同一个原因只留一组快照，长停滞不会灌满日志。

## 自检包里有什么

发 `Command::Diagnose` 后，desk 在 `<数据目录>/diagnose/<墙上时钟毫秒>/` 下原子写出一个只含代码、数字和布尔值的文件夹（最多保留最新 3 份），不含任何记录、音频、钥匙、DID、句柄、设备名或绝对路径：

- `about.json`：版本、系统、架构、日志级别、进程已运行毫秒数（`up_ms`）、谁在签（`who`：`nobody` / `local` / `person`）、下一场设置中的代码项（`spoken`、`shown`、`sound`、`captions`）以及各服务钥匙来源（`keys`）；
- `water.json`：此刻整份水位（每格位置与健康判断）；
- `stats.json`：此刻整份统计（计数、高水位、延迟分布、每条队列容量/深度/最深/满/丢弃）；
- `tasks.json`：此刻核心根树上正在运行的全部任务和阻塞调用（含 `for_ms`）；
- `logs/`：刷新日志后复制的最近最多 2 个 `*.jsonl` 文件（上限 16 MB）。

排查次序：先看 `about.json` 确认版本、身份、钥匙来源与语言；再看 `water.json` 哪一格非 `Live`、对照 `stats.json` 队列深度与 `tasks.json` 卡住的阻塞调用；最后在 `logs/` 里搜那一格的 `judge.verdict` 和对应 `snap` 号的快照。

## 事件

### 启动与日志

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `app.start` | info | 外壳装好日志后写出的第一行，记下运行环境与级别 | `version` `os` `arch` `level` |
| `app.panic` | error | 进程 panic 钩子在任何线程 panic 时发出（不带消息文本），随后刷盘 | `thread`（无名时为 `unnamed`） `file`（工作区外仅留末两段） `line` |
| `log.turned` | info | 日志写线程轮换文件后在新文件第一行写出（`seq = 0`） | `files`（当前保留文件数） |
| `log.lost` | warn | 日志写线程发现上一行之后日志队列满过，在下一行前补写（`seq = 0`） | `n`（这段丢掉的行数） |

### 队列、树与监督

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `queue.full` | warn | 有名字的队列首次从「有位」变成「满」时发一次（边沿触发） | `queue`（P1 全部 13 个：`capture.pump`、`capture.ended`、`capture.file`、`audio`、`tape`、`lane.events`、`draft`、`hub.mail`、`desk`、`write.wav`、`city.audio`、`shell.joins`、`shell.updates`） `policy`（`wait` / `drop` / `reject` / `end`） `capacity` |
| `queue.clear` | info | 满过的队列重新成功放进元素（或广播读者追上）时发出 | `queue`（同上） `lost`（这段满期间退回或丢弃的个数） |
| `task.start` | info | `Tree::spawn` 或 `Tree::keep` 在某棵树上起一个任务时发出 | `tree` `task` |
| `task.end` | info；panic 时 error | 树上一个任务正常结束、被取消或 panic 被接住时发出 | `tree` `task` `code`（`done` / `cancelled` / `panic`） `ran_ms` |
| `task.refused` | warn | 往已经开始收尾（`wait`）的树上尝试起新任务时发出 | `tree` `task` |
| `thread.start` | info | `rt::supervise::thread` 起的具名操作系统线程启动时发出 | `thread` |
| `thread.end` | info；panic 时 error | 具名操作系统线程退出或 panic 被接住时发出 | `thread` `code`（`done` / `panic`） `ran_ms` |
| `part.state` | info；`to = "Waiting"` 时 warn | 受监督部件（`Watch` 状态机）每次状态转换时发出 | `part`（P1 全部 7 个：`tape`、`draft`、`lane.0`、`lane.1`、`lane.2`、`frame`、`shell.pump`） `from` `to`（`Opening` / `Running` / `Waiting` / `Ended`） `why` `wait_ms` `n` |
| `part.gave_up` | error | 受监督部件撞到重启上限（1 分钟 8 次或 120 秒无健康运行）放弃自愈时发出 | `part`（同上） `why` `n` |
| `keep.lost` | warn | 落盘部件（`rt::keep`）在写一批数据时失败或 panic，丢掉这一批并准备重开 | `part`（`tape` / `draft`） `n`（本批条数或块数） `why`（错误代码或 `panic`） |

### 统计与汇总

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `stats.summary` | info | `stats` 任务每分钟发出一次，随后紧跟这一分钟内动过的 `stat.*` 行 | `snap`（0） `up_ms` `rows`（随后行数） |
| `stat.count` | info | 每分钟汇总（`snap = 0`）或停滞快照（`snap > 0`）里的一条计数行 | `snap` `name` `n` |
| `stat.peak` | info | 每分钟汇总或停滞快照里的一条高水位行 | `snap` `name` `now` `most` |
| `stat.spread` | info | 每分钟汇总或停滞快照里的一条分桶延迟分布行 | `snap` `name` `n` `p50` `p99` `most` |
| `stat.queue` | info | 每分钟汇总或停滞快照里的一条队列状态行 | `snap` `name` `capacity` `depth` `deepest` `sent` `full` `lost` |
| `stats.full` | warn | 某类统计行数到达上限或名字超过 32 字节时发一次（每类一次） | `kind`（`counts` / `peaks` / `spreads` / `queues` / `tasks`） |

### 身份与核心（hub / work）

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `hub.boot` | info | hub 的启动与安装状态机 `Boot` 转换时发出 | `from` `to`（`Starting` / `Ready` / `Installing`） `why`（`boot` / `update` / `update.install`） |
| `boot.step` | info；失败时 warn | 启动流程里每一步做完时发出（`devices` 另带设备个数，不含设备名） | `step`（P1 全部 4 个：`settings`、`identity`、`invites`、`devices`） `code` `ms`（`devices` 另带 `n`） |
| `identity.who` | info；`to = "nobody"` 时 warn | 启动、登录或登出切换签名身份时发出；非首次启动而 `made = true` 表示秘密曾丢失 | `from` `to`（`nobody` / `local` / `person`） `why`（`boot` / `sign_in` / `sign_out`） `made` |
| `command.done` | info；被拒绝时 warn | hub 同步处理完一条界面命令时发出（带 IO 的命令随后看 `job.done`） | `kind`（`Command` 变体名） `code`（`ok` 或拒绝码） `took_us` |
| `command.refused` | warn | `Core::send` 遇到 hub 邮箱满或已关闭，命令未能入队时发出 | `kind` `code`（`busy`） |
| `hub.turn.slow` | warn | hub 单次消息处理超过预算 `HUB_TURN_BUDGET_US`（1000 µs）时发出 | `kind` `took_us` |
| `notice.said` | info | 核心通过 `Voice::say` 向界面发出一条稳定提示代码时发出 | `code` |
| `job.start` | info | `desk` 或 `Work` 的一件异步/阻塞活开始执行时发出 | `kind`（P1 全部 33 个：`boot`、`settings`、`sign`、`signout`、`signin`、`create`、`claim`、`clip`、`rotation_saved`、`calendar`、`install`、`refresh`、`change`、`remove`、`restore`、`term`、`destroy`、`export`、`export.write`、`search`、`share.prepare`、`share.put`、`share`、`unshare.prepare`、`unshare`、`slot`、`file.begin`、`file`、`own_key`、`diagnose`、`sweep`、`write`、`move`） `job`（递增序号） `waited_ms` |
| `job.done` | info；`code != "ok"` 时 warn | `desk` 或 `Work` 的一件活结束时发出 | `kind`（同上） `job` `code`（`ok` / `stale` / `cancelled` / `panic` 或失败码） `ms` |
| `job.again` | warn | 后台或重试任务（`write`、`file`、`create`、`claim`）遇可恢复失败，退避等待下一次重试时发出 | `kind` `why` `wait_ms` `n` |
| `write.skipped` | info | 有音频的草稿因无钥匙可取（`keys.none`）未送去整场转写，留在现场稿等待时发出 | `n` `why`（`keys.none`） |
| `diagnose.wrote` | info | `desk` 原子写完一份自检包并清理超额旧包后发出 | `files`（包内文件总数） `bytes`（总字节数） |

### 采集与落盘

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `capture.source.open` | info | 采集声源线程成功打开一个音频输入设备时发出 | `source`（声源序号，0 起） `ms` |
| `capture.source.broke` | warn | 某个音频声源中断，准备退避后重开时发出 | `source` `why` `wait_ms` |
| `capture.source.ended` | error | 某个音频声源遇到不可恢复错误（如无权限），放弃该声源时发出 | `source` `why` |
| `capture.pad` | warn | 时钟策略（`wall_clock_policy`）决定补静音以对齐墙上时钟时发出 | `ms`（补的静音毫秒数） |
| `capture.end` | error | 时钟策略判定落后超限（如合盖睡眠 > 60 秒）而主动结束采集时发出 | `why`（`asleep`） |
| `capture.pause` | info | 采集泵收到并应用暂停或继续指令时发出 | `paused` |
| `capture.ring.lost` | warn | 采集泵在周期读取中发现某声源无锁环丢过回调样本时发出 | `source` `samples` |
| `tape.pad` | warn | `tape::Writer` 收到靠后的音频块，把中间丢失的洞补成加密静音块时发出 | `from_ms` `blocks` |
| `draft.gap` | warn | 草稿队列满过又恢复后，写入一行 `…` 空缺标记覆盖丢失区间时发出 | `t0_ms` `t1_ms` `lines`（丢失行数） |

### 录音、字幕与水位看守

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `recording.start` | info | 一场录音建立并开始采集时发出 | `rec` `source`（`mic` / `system` / `both` / `file`） `lanes` `columns` `captions`（`live` / `file` / `captions.off` / `captions.no_key`） |
| `recording.phase` | info；到 `Ended` 带失败代码时 error | 录音生命周期状态机 `Phase` 每次切换时发出（含上一阶段耗时） | `rec` `from` `to`（`Opening` / `Recording` / `Paused` / `Closing.Capture` / `Closing.Tape` / `Closing.Lanes` / `Closing.Frame` / `Closing.Draft` / `Sealing` / `Ended`） `why` `ms` |
| `recording.end` | info；带失败代码时 error | 一场录音彻底收尾（封好 tape 并落盘）后发出 | `rec` `code`（`done` 或结束原因码） `ms` |
| `session.status` | info | 面向界面的录音状态句 `Status` 发生变化时发出 | `rec` `from` `to`（`Recording` / `Paused` / `AudioOnly` / `Connecting` / `Behind`） `why` |
| `lane.open` | info；失败时 warn | 一条字幕 lane 每次尝试取钥匙并拨号连接服务后发出（成败都发） | `lane` `code`（`ok` 或失败码） `keys_ms` `dial_ms`（未到拨号为 0） |
| `lane.link` | info；到 `Behind` 或非 `done` 的 `Stopped` 时 warn | 一条 lane 自身连接状态 `Link` 变化时发出 | `lane` `from` `to`（`Connecting` / `Live` / `Behind` / `Stopped`） `why` |
| `lane.gap` | warn | 一条 lane 跳边、断连或崩过重连后向帧任务和草稿报告丢失区间时发出 | `lane` `t0_ms` `t1_ms` `why` |
| `lane.kick` | warn | 看守发现 `Heard` 停滞（服务收音不回话），踢掉该 lane 迫使其重连时发出 | `lane` `why`（`silent`） |
| `lane.reconnect` | info | 收到 `Command::Reconnect` 后把已放弃（`Dead`）的 lane 重新拉起时发出 | `lane` |
| `frame.fit` | info | 组帧器 `fit` 为把单帧压进 8192 字节而让掉的层级 `Fit` 发生变化时发出 | `from` `to`（`Whole` / `Lines` / `Units` / `Tail`） `bytes` |
| `water.mark` | debug；在快照里是 warn | 看守每秒采样写出的各格水位（`snap = 0`），或停滞快照里的各格水位（`snap > 0`） | `snap` `stage` `lane`（无 lane 为 `-1`） `at` `moved_ms` `why` `ended` |
| `judge.verdict` | 到 `Live` info；`Slow` / `Stalled` warn；`Dead` error | 看守判定某一格的健康状态 `Health` 发生变化时发出 | `stage` `lane` `from` `to` `why` `action`（`none` / `reopen` / `tell` / `end`） |
| `stall.snapshot` | `Stalled` warn；`Dead` error | 某一格刚变成停滞或停了时写出的快照头 | `snap` `rec` `stage` `lane` `health` `why` `idle_ms` |
| `stall.snapshot.end` | `Stalled` warn；`Dead` error | 一组停滞快照的尾行，记录总行数以防截断 | `snap` `lines` |
| `task.running` | warn | 停滞快照里列出的根树当前运行任务或阻塞调用（每个一行） | `snap` `tree` `task` `kind`（`task` / `kept` / `blocking`） `for_ms` |

### 外壳与界面

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `shell.join` | info | 一个窗口向外壳推送任务 `shell.pump` 订阅事件通道时发出 | `window` `replaced`（是否顶掉了该窗口旧通道） |
| `shell.sink.gone` | warn | 某个窗口的通道已关闭或发送时 panic，被推送任务移出订阅表时发出 | `window` `why`（`closed` / `panic`） |
| `shell.captions` | info；失败时 warn | 外壳按 `State.captions` 变化向字幕窗投递显隐请求后发出 | `shown` `code`（`ok` / `missing` / `failed`） |
| `shell.main_thread.stalled` | warn | 外壳主线程心跳超过 `BEAT_STALLED_MS`（2000 ms）未回时发出（每次停滞仅一条） | `waited_ms` |
| `shell.main_thread.back` | info | 停滞的主线程终于执行完那次心跳回调时发出 | `stalled_ms` |
| `ui.frame.gap` | warn | 界面每秒回报中 `gaps > 0`（两次 rAF 间隔超 250 ms）时外壳写出 | `window` `gap_ms` `n` |
| `update.look` | info；出错时 warn | 外壳 `updates` 任务查了一次新版本后发出 | `code`（`none` / `ready` / `update`） |

### 服务器

| 事件 | 级别 | 谁在什么时候发 | 字段 |
|---|---|---|---|
| `server.start` | info | 服务器装好日志后写出的第一行 | `version` |
| `server.request` | info；状态 ≥ 500 时 warn | 服务器答完一个 HTTP 请求时由中间件发出（不带路径参数与正文） | `route`（`ROUTES` 表代码，未命中为 `none`） `status` `ms` |
| `server.sweep` | debug | 服务器执行一次过期房间清扫后发出 | `ms` |

## 出了事先看什么

| 症状 | 先搜哪些事件 | 怎么往下追 |
|---|---|---|
| **字幕停了** | 先看 `recording.start` 的 `captions`；再搜 `judge.verdict`、`part.gave_up`、`lane.open` | 若 `captions != "live"`，本场本来就不开字幕（如 `captions.no_key`）。否则看 `judge.verdict`：`Heard` 到 `Stalled(silent)` 会伴随 `lane.kick` 与重连；`Sent` 到 `Stalled`/`Dead` 看前面 `lane.open` 的 `code`（`connect` / `keys` / 服务错误）及 `keys_ms`、`dial_ms`，再用同一 `snap` 号拉出 `stall.snapshot` 看全部水位和队列 |
| **录音自己结束了** | 先搜 `recording.end`（级别 `error`）和紧挨其前的 `recording.phase` | 看 `recording.end` 的 `code`：`audio.stalled` 向上追 `judge.verdict(Capture)` 和 `capture.source.*`；`asleep` 追 `capture.end`；`tape` 追 `queue.full(queue = "tape")` 与 `keep.lost`、`part.state(part = "tape")`；若停在收尾卡住，看最后一条 `recording.phase` 的 `to`（停在哪一步 `Closing.*`） |
| **界面卡了** | 先搜 `shell.main_thread.stalled`、`ui.frame.gap`、`judge.verdict`（`Shell` / `Screen`） | 有 `shell.main_thread.stalled` 说明外壳主线程被系统或回调堵住，等 `shell.main_thread.back` 看持续多久；有 `ui.frame.gap` 说明前端渲染掉帧（看 `window`、`gap_ms`、`n`）；`Shell` 停滞则追 `shell.join`、`shell.sink.gone`、`part.state(part = "shell.pump")` |
| **命令没反应** | 先搜 `command.refused`、`command.done`、`hub.turn.slow`、`job.start`、`job.done` | 有 `command.refused(busy)` 看 `queue.full(hub.mail)`；有 `command.done` 且 `code != "ok"`（如 `starting`、`sign_in.first`、`busy`）直接读 `code`；`command.done(ok)` 后没结果，对照同 `kind` 的 `job.start` 有没有配对的 `job.done`，没有则说明卡在阻塞活或网络里（看自检包 `tasks.json` 或快照里的 `task.running`） |
