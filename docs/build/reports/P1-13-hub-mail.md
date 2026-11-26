状态：完成

# P1-13 hub 的邮箱有上限；每条命令留一条事件：报告

## 改了什么

1. **hub 邮箱改由 `rt::queue::bounded` 构造，三种发信人各自声明 `Full`（`core/src/app/hub.rs`）**：
   - 新增带单位和理由的公开常量 `HUB_MAIL_MSGS: usize = 256`（邮箱容量）与 `HUB_TURN_BUDGET_US: u64 = 1_000`（单圈预算，对应 `PLAN.md` §4.1）。
   - `Core::start` 中用 `queue::bounded(&stats, "hub.mail", HUB_MAIL_MSGS, Full::Reject)` 构造 `commands` 与 `mail`，再以 `commands.with(Full::Wait)` 造 `results`、以 `commands.with(Full::Drop)` 造 `progress`；删除 `mpsc::unbounded_channel`、`UnboundedSender`、`UnboundedReceiver`、`type Sender` 及 `Task::tell`。
   - `Core::send(&self, command: Command) -> Result<(), &'static str>` 经 `commands.offer(Msg::Do(command))` 非阻塞投递；遇到 `Back::Full` 或 `Back::Closed` 时发出 `warn!("command.refused", kind = kind, code = "busy")`、递增计数 `commands.refused` 并返回 `Err("busy")`。
   - `Core::looked` 改为 `pub async fn looked(&self, found: Result<String, &'static str>)`，经 `results.send(Msg::Looked(found)).await` 投递（`// dropped: the hub has ended`）。
   - 所有后台任务与录音任务的结果/终止事实走 `results.send(...).await`（`migrate` 的 `spawn_blocking` 中用 `results.send_blocking(...)`），所有进度更新走 `progress.offer(...)`。

2. **每条命令与慢圈可观测（`core/src/app/proto.rs`、`core/src/app/hub.rs`、`docs/EVENTS.md`）**：
   - `Command::kind(&self) -> &'static str`（穷举 26 个变体名，不含任何参数）与 `Msg::kind(&self) -> &'static str`（`Do(cmd)` 返回 `cmd.kind()`，其余返回变体名）。
   - `Hub::run` 在 `self.take(msg)` 前后用 `std::time::Instant` 计时：
     - 将 `took_us` 记入分布 `hub.turn_us` 并递增计数 `hub.turns`；
     - 若是命令（`Msg::Do(_)`），在发 `said` 与 `publish` 之前发出 `command.done`（`code == "ok"` 为 `info!`，拒绝时为 `warn!`，字段仅含 `kind`、`code`、`took_us`）并递增计数 `commands.done`；
     - 若 `took_us > HUB_TURN_BUDGET_US`，发出 `warn!("hub.turn.slow", kind = kind, took_us = took_us)` 并递增计数 `hub.turns.slow`。
   - `docs/EVENTS.md` 新增 `command.done`、`command.refused`、`hub.turn.slow` 三行。

3. **外壳、规则脚本、多语检查与测试台（`app/src/main.rs`、`scripts/build/rules/bounded.sh`、`core/tests/{hub/mod.rs,bench/mod.rs,app.rs}`）**：
   - `app/src/main.rs` 的 `dispatch` 捕获 `core.send(command)` 返回的 `Err(code)`，经 `fan` 发送 `Event::Notice(code.into())`；`look` 对 `app.state::<Core>().looked(...).await` 等待完成。
   - 核查 `ui/lang/*.json` 全部 8 种语言，已有 `code.busy`（如 `en.json` 为 `"Service busy"`、`zh-Hans.json` 为 `"服务繁忙"`），照 `P1.md` §3.11 沿用，无需改动。
   - `scripts/build/rules/bounded.sh` 将 `core/src/app/hub.rs` 欠账从 `6 P1-13/P1-16` 降至 `2 P1-16`（仅剩音频 `broadcast` 与 lane `mpsc`）。
   - `core/tests/hub/mod.rs`（`App::send`）与 `core/tests/bench/mod.rs`（`Bench::send`）适配 `Core::send` 的 `Result` 返回值；`core/tests/app.rs` 更新 `looked(...).await` 并新增 4 条测试。

文件：`app/src/main.rs`、`core/src/app/hub.rs`、`core/src/app/proto.rs`、`core/tests/app.rs`、`core/tests/bench/mod.rs`、`core/tests/hub/mod.rs`、`docs/EVENTS.md`、`scripts/build/rules/bounded.sh`、本报告。

## 邮箱发信处清单

| 发信处 | 消息 | 发信人 / 策略 | 理由 |
|---|---|---|---|
| `Core::send` | `Msg::Do(command)` | `commands`（`Full::Reject`，`offer`） | 界面命令绝不阻塞等待，邮箱满当场返回 `busy`。 |
| `Core::looked` | `Msg::Looked(found)` | `results`（`Full::Wait`，`send().await`） | 外壳查到更新或查更新失败是结果事实，不能丢。 |
| `Command::SignIn` 任务 | `Msg::SignedIn(..)` | `results`（`Full::Wait`，`send().await`） | 登录完成结果必须送达以切换身份与清登录码。 |
| `Command::City` 任务 | `Msg::City(..)` | `results`（`Full::Wait`，`send().await`） | 城日历拉取结果必须送达以更新 `state.city` 或报错。 |
| `Hub::file` 进度回调 | `Msg::Filing(w)` | `progress`（`Full::Drop`，`offer`） | 交城进度只看最新，下一条会覆盖当前进度。 |
| `Hub::file` 完成 | `Msg::Filed(id, said)` | `results`（`Full::Wait`，`send().await`） | 交城完成事实必须送达以清理 `state.filing` 并提示结果。 |
| `Hub::sign` 领回邀请任务 | `Msg::Claimed(..)` | `results`（`Full::Wait`，`send().await`） | 邀请认领结果必须送达以清 `invite` 并报结果。 |
| `Hub::carry` 建身份任务 | `Msg::SignedIn(me)` | `results`（`Full::Wait`，`send().await`） | 为旧邀请新造的身份结果必须送达以完成登录。 |
| `Hub::migrate` 进度回调 | `Msg::Moving(writing)` | `progress`（`Full::Drop`，`offer`） | 0.8 搬运进度只看最新，下一条会覆盖当前进度。 |
| `Hub::migrate` 完成（阻塞线程） | `Msg::Moved(moved)` | `results`（`Full::Wait`，`send_blocking`） | 搬运完成结果必须送达以清 `moving` 标志并记未搬项。 |
| `Hub::share` 任务 | `Msg::Shared(..)` | `results`（`Full::Wait`，`send().await`） | 分享房间创建结果必须送达以刷新房间列表或报错。 |
| `Hub::unshare` 任务 | `Msg::Shared(..)` | `results`（`Full::Wait`，`send().await`） | 停止分享结果必须送达以核对状态或回滚记录。 |
| `Task::record` 采集结束 / 失败 | `Msg::Ended(id)` | `results`（`Full::Wait`，`send().await`） | 录音被动终止的事实必须送达，让 hub 收束当前场次。 |
| `Task::finish` 收尾完成 | `Msg::Wrote(id)` | `results`（`Full::Wait`，`send().await`） | 整场文字稿收尾结果必须送达以清 `writing`、刷新并清理音频。 |
| `Task::write` 进度回调 | `Msg::Writing(..)` | `progress`（`Full::Drop`，`offer`） | 整场转写步骤进度只看最新，下一条会覆盖它。 |
| `Task::write` 重试提示 | `Msg::Writing(..)` | `progress`（`Full::Drop`，`offer`） | 重试等待状态属于进度展示，后续重试或完成会覆盖它。 |

## 证据

### 1. 新增测试（`core/tests/app.rs`）

先写测试并运行确认未改实现前编译失败（缺少 `HUB_MAIL_MSGS`、`Core::send` 返回 `()`、`Core::looked` 非 async），实现后全部通过：

1. `a_hub_that_cannot_keep_up_refuses_commands_and_says_busy`：单线程运行时连续调用 300 次 `core.send(Command::Pause)` 不 `await`，256 次成功、44 次返回 `Err("busy")`；让出后 `Outlet.said` 收到 256 次 `recording.none`；断言 `queue.full`（`queue = "hub.mail"`、`policy = "reject"`、`capacity = 256`）正好 1 条、`command.refused` 44 条、`command.done`（`Pause`、`recording.none`）256 条，以及统计行 `hub.mail` 的 `full == 44`、`deepest == 256`、`commands.refused == 44`、`commands.done == 256`。
2. `what_the_shell_found_is_never_dropped`：塞满邮箱后调用 `core.looked(Ok("0.9.1".into())).await`，验证其在 hub 腾出位置后完成且 `state.update == Some("0.9.1")`。
3. `every_command_leaves_its_kind_its_answer_and_nothing_it_carried`：发送 `Setup`、`Change`（带识别词标题）、`Search`（带识别词查询）、`Pause`，验证 4 条 `command.done` 的级别、种类与结果码全部匹配，每条字段严格只有 `code`、`kind`、`took_us`，整个捕获里不含标题和搜索词，且 `commands.done == 4`、`hub.turn_us.n >= 4`。
4. `progress_that_finds_the_mailbox_full_is_let_go`：在 `hub.mail` 队列层塞满 256 条后，用 `Full::Drop` 端 `offer` 立即返回 `Err(Back::Full(_))` 并记 `queue.full`（`policy = "drop"`）；随后用 `Full::Wait` 端 `send` 挂起等待，排空后结果顺利送达并触发 `queue.clear`（`lost = 1`）。

### 2. `bounded.sh` 欠账前后对比

- 改前：`core/src/app/hub.rs 6 P1-13/P1-16`
- 改后：`core/src/app/hub.rs 2 P1-16`

`git grep -n "unbounded\|Unbounded" core/src app/src` 返回空（0 处）。

### 3. 真实 JSONL 日志样例（取自 `nothing_said_or_secret_reaches_the_log` 写入磁盘的 `zutalk.jsonl`）

```json
{"t_ms":1795539138335,"seq":4,"level":"info","component":"zutalk_core::app::hub","event":"command.done","kind":"Setup","code":"ok","took_us":264}
{"t_ms":1795539138354,"seq":5,"level":"info","component":"zutalk_core::app::hub","event":"command.done","kind":"Start","code":"ok","took_us":19116}
{"t_ms":1795539138354,"seq":6,"level":"warn","component":"zutalk_core::app::hub","event":"hub.turn.slow","kind":"Start","took_us":19116}
{"t_ms":1795539138807,"seq":35,"level":"info","component":"zutalk_core::app::hub","event":"command.done","kind":"Change","code":"ok","took_us":11800}
{"t_ms":1795539138807,"seq":36,"level":"warn","component":"zutalk_core::app::hub","event":"hub.turn.slow","kind":"Change","took_us":11800}
{"t_ms":1795539138809,"seq":37,"level":"info","component":"zutalk_core::app::hub","event":"command.done","kind":"Search","code":"ok","took_us":1704}
{"t_ms":1795539138809,"seq":38,"level":"warn","component":"zutalk_core::app::hub","event":"hub.turn.slow","kind":"Search","took_us":1704}
{"t_ms":1795539139410,"seq":48,"level":"info","component":"zutalk_core::app::hub","event":"command.done","kind":"Stop","code":"ok","took_us":549}
```

其中 `Start`（19,116 µs）、`Change`（11,800 µs）、`Search`（1,704 µs）因仍在 hub 上同步读写书架并 `refresh()`，超过 `HUB_TURN_BUDGET_US`（1,000 µs）并如实触发 `hub.turn.slow`；`Setup`（264 µs）与 `Stop`（549 µs）在预算内。

### 4. 全量门禁结果

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 警告）。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部退出码 0。
- `git grep -n "unbounded\|Unbounded" core/src app/src`：0 处结果。
- 全量门禁 `scripts/build/check.sh` 全绿：Rust `passed 557, failed 0, ignored 12`，`ui`：`npm run --silent check`（0 errors, 0 warnings）与 `npm test --silent`（5 个文件、29 条全过）。

## 留下的，要负责人的

- 音频 `broadcast` 与 lane `mpsc` 通道迁入 `rt::queue` 在 `P1-16`；hub 磁盘 IO 迁出（消除 `hub.turn.slow`）在 `P1-19`、`P1-20`；外壳 `dispatch` 改为 `async` 直接返回结果在 `P1-23`。
