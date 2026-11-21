# P1-11 hub 的邮箱有上限；每条命令留一条事件

## 目标

做完以后：

1. hub 的邮箱是一条有名字的队列（`hub.mail`，上限 `HUB_MAIL_MSGS` = 256），三种来信各有满了以后的办法，都写在造它的地方。
2. 界面的命令从不等：邮箱满了，当场拒绝，`busy`。
3. hub 处理的每条命令在日志里有一条 `command.done`（种类、当场的结果、用了多久）；处理一条消息超过预算，有一条 `hub.turn.slow`。

这是 `PLAN.md` §7 的 I4 的一部分（采集的通道和 lint 是 P1-12，「外壳没有同步命令」是 P1-21），也是 §4.5.2「每条命令（种类、结果代码、用时）」。

## 背景

- 每条队列的名字、上限和满了怎么办，在 `docs/build/phases/P1.md` §3.7；队列怎么造在 §3.13.9；事件在 §3.13.3（标着 P1-11 的三行）；hub 的预算在 §3.8。
- `rt::queue`（P1-04）：`bounded`、`Full`、`Tx::offer`、`send`、`send_blocking`、`with`。
- `core/src/app/hub.rs`：`Core(mpsc::UnboundedSender<Msg>)`、`type Sender`、`Core::send`、`Core::looked`；往邮箱里发的地方散在各个任务里：登录和认领的结果、房间的结果、城的日历、交城的进度和结果、搬运的进度（在 `spawn_blocking` 里）和结果、整场转写的进度和结果、一场录音结束了。P1-05 给这些发送处补了 `// dropped:`（「邮箱没有上限」那个理由从这一件起不成立了）。
- `Hub::run` 收信的循环；`take(msg)` 处理一条；`command(command) -> Result<(), &'static str>`。
- 外壳 `app/src/main.rs`：`dispatch` 调 `core.send`；`look` 调 `core.looked`。

## 做法

### 邮箱

1. 造的地方一眼看得出全部（`Core::start`）：

   ```rust
   /// The hub's mail. 256: a burst of clicks is a few dozen; the hub answers each in under a millisecond.
   const HUB_MAIL_MSGS: usize = 256;
   let (commands, mail) = queue::bounded(&stats, "hub.mail", HUB_MAIL_MSGS, Full::Reject); // the interface: never waits, is told `busy`
   let results = commands.with(Full::Wait);   // work that is done: must arrive; whoever did it can wait
   let progress = commands.with(Full::Drop);  // how far a job has got: only the latest matters
   ```

   三种发信人各拿各的那一头：`Core` 拿 `commands`；后台的任务拿 `results` 和 `progress`。把「怎么发」包成邮箱的两个方法也可以（`must(msg).await`、`may(msg)`），但 `Full` 要写在造的地方。
2. `Core::send(command) -> Result<(), &'static str>`：`commands.offer(..)`，满了返回 `Err("busy")`，并发一条 `warn!("command.refused", kind = command.kind(), code = "busy")`，计数 `commands.refused` 加一。界面的命令不许等。
3. 结果和事实（登录的结果、一场录音结束了、整场转写写好了…）：`results.send(msg).await`；在阻塞调用里用 `send_blocking`。发不进去只有一种可能：hub 已经结束，那时同一行写 `// dropped: the hub has ended`。
4. 进度（`Writing`、`Filing`、`Moving`）：`progress.offer(msg)`，被退回就丢，同一行 `// dropped: the next one replaces it`。
5. `Core::looked` 改成 `async fn`，走 `results`：外壳找到新版本这件事不能丢。
6. 外壳（最小改动）：`dispatch` 里 `core.send` 返回 `Err(code)` 时，经现有的 `fan` 发一个 `Event::Notice(code)`；`look` 里 `.await` 那个 `looked`。P1-21 再让 `dispatch` 把结果直接还给界面。
7. `ui/lang` 里 `code.busy` 已经有词（「服务忙」一类），意思对得上就沿用；对不上就改这八处的词。

### 每条命令一条事件

8. `Command` 加 `kind(&self) -> &'static str`：变体的名字（`Start`、`Pause`、`Change`…），一个 `match`。`Msg` 同样加一个 `kind()`（`Do` 给命令的种类；其余是 `Ended`、`Writing`、`Wrote`…）。
9. hub 的循环里，处理每条消息的前后各读一次 `std::time::Instant`（这是那一层薄壳，不是规则；处理的函数自己不读钟）：
   - 分布 `hub.turn_us` 记这一圈的微秒；计数 `hub.turns` 加一；
   - 是命令：一条 `command.done`（`kind`、`code`、`took_us`）。`code` 是 `command(..)` 当场的结果：`ok`，或者拒绝它的代码（这时用 `warn!`）。`ok` 的意思是「收下了」：要落盘的活后来做没做成，是那件活自己的事件（P1-17 的 `job.done`）。计数 `commands.done` 加一；
   - 超过 `HUB_TURN_BUDGET_US`（1000：`PLAN.md` §4.1 给 hub 的预算，一条命令一毫秒）：一条 `warn!("hub.turn.slow", kind, took_us)`，计数 `hub.turns.slow` 加一。**现在它会常见**：hub 还在自己读写磁盘（P1-17、P1-18 搬走）。这正是它该说的话；不要为了让日志安静而调高预算。
10. 命令被拒绝时说给人听的那个代码照旧（`said.say(code)`，P1-09）。

## 显式与可观测

- **事件**：`command.done`、`command.refused`、`hub.turn.slow`。写进 `docs/EVENTS.md`。`queue.full`、`queue.clear`（`queue = "hub.mail"`）是队列自己发的。
- **统计**：计数 `commands.done`、`commands.refused`、`hub.turns`、`hub.turns.slow`；分布 `hub.turn_us`；队列 `hub.mail` 一行（自动）。
- **数**：`HUB_MAIL_MSGS`（256）、`HUB_TURN_BUDGET_US`（1000）。
- **队列**：`hub.mail`，`queue::bounded`；命令 `Full::Reject`，结果 `Full::Wait`，进度 `Full::Drop`。
- **任务**：不起新的。`spawn.sh` 的欠账不变。
- **欠账**：`bounded.sh` 里 `core/src/app/hub.rs` 那一行减掉邮箱的那几处（`unbounded_channel` 和 `UnboundedSender` 的类型）；剩下音频的 `broadcast` 和 lane 的 `mpsc`（P1-14 还）。
- **隐私**：`command.done` 只带种类，不带命令的内容（`Change` 的新标题、`Search` 的词、`Export` 的路径都不进日志）。

## 先写的测试

`core/tests/app.rs`：

1. `a_hub_that_cannot_keep_up_refuses_commands_and_says_busy`。单线程的运行时（`#[tokio::test]`），一口气调 300 次 `core.send(Command::Pause)`，中间不 `await`（hub 的任务没有机会跑）：成功的不超过 256 次，其余都是 `Err("busy")`，两者加起来 300。然后让出去，等 `said` 里出现和成功次数一样多的 `recording.none`：收下的每一条都处理了。**看得见**：`queue.full`（`queue = "hub.mail"`、`policy = "reject"`、`capacity = 256`）正好一条；`command.refused`（`kind = "Pause"`、`code = "busy"`）的条数等于被拒绝的次数；之后 `command.done`（`kind = "Pause"`、`code = "recording.none"`）的条数等于成功的次数；统计里 `hub.mail` 的 `full` 等于被拒绝的次数，`deepest` 是 256。
2. `what_the_shell_found_is_never_dropped`。照上一条把邮箱塞满，再 `core.looked(Ok("0.9.1".into())).await`：它在 hub 腾出位置以后完成，`state.update` 是 `0.9.1`。
3. `every_command_leaves_its_kind_its_answer_and_nothing_it_carried`：发 `Setup`、`Change`（新标题是一个认得出的词）、`Search`（同样）、`Pause`：每条一条 `command.done`，`kind` 对；`code` 是 `ok` 或那个拒绝的代码；整个捕获里找不到那两个词；每条 `command.done` 的字段只有 `kind`、`code`、`took_us`。
4. `progress_that_finds_the_mailbox_full_is_let_go`（在邮箱这一层测：用 `Full::Drop` 的那一头往一条塞满的 `hub.mail` 里 `offer`）：被退回，没有等；之后用 `Full::Wait` 的那一头 `send` 的结果在腾出位置以后到达。

规则脚本：`bounded.sh` 的欠账表改小以后通过；报告里贴改之前、改之后那一行。

## 验收

- `scripts/build/check.sh` 全绿。
- `git grep -n "unbounded\|Unbounded" core/src app/src` 没有结果。
- 报告里列出邮箱的每一个发信处用的是哪一种发信人（`Reject`、`Wait`、`Drop`），各一句理由；贴一段真的日志：一条 `command.done`、一条 `hub.turn.slow`（它现在应该很容易出现：说明是哪种消息、多少微秒）。

## 不做

- 不动采集的通道，不加 `clippy.toml`（P1-12）。
- 不加 tape 和草稿的队列（P1-13），不加 desk 的队列（P1-17）。
- 不把 hub 的 IO 搬走（P1-17、P1-18）：`hub.turn.slow` 这一件只是让它看得见。
- 不把外壳的命令改成 async（P1-21）。
