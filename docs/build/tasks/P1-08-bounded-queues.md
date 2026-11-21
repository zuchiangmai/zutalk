# P1-08 没有无上限的队列

## 目标

做完以后，整个工作区里没有一条无上限的队列，而且再也加不进来：

1. hub 的邮箱有上限（256），三种来信各有满了以后的办法。
2. 采集里的三条 `std::sync::mpsc::channel()` 换成有上限的。
3. `clippy.toml` 禁掉两个无上限的构造函数（测试也守）；`scripts/build/rules/bounded.sh` 再守一遍。

这是 `PLAN.md` §7 的 I4 的前一半（后一半「外壳没有同步命令」是 P1-14）。

## 背景

- 每条队列的上限和满了怎么办，在 `docs/build/phases/P1.md` §3.7。照那张表做；这一件管其中的「泵的控制消息」「设备的『我坏了』」「hub 的邮箱」三行，其余的行由加它们的任务管。
- `core/src/app/hub.rs`：`Core(mpsc::UnboundedSender<Msg>)`、`type Sender`、`Core::send`、`Core::looked`；往邮箱里发的地方散在各个任务里：登录和认领的结果、房间的结果、城的日历、交城的进度和结果、搬运的进度（在 `spawn_blocking` 里）和结果、整场转写的进度和结果、一场录音结束了。
- `core/src/capture/audio.rs`：泵的邮箱（`Capture::by_hand` 里）、每个设备的 `ender`（`hold` 里）、读文件线程的邮箱（`Capture::start` 的 `Source::File` 分支），都是 `mpsc::channel()`。`Capture::pause`、`resume` 往泵的邮箱发；`Drop` 发 `Stop`。`core/src/capture/audio/file.rs` 的 `run` 收。
- 测试里的：`core/tests/live.rs` 末尾 `relay` 用了 `unbounded_channel`；其余的用 `grep` 找。
- 外壳 `app/src/main.rs`：`dispatch` 调 `core.send`；`look` 调 `core.looked`。

## 做法

### hub 的邮箱

1. `mpsc::channel(256)`。把「怎么发」做成邮箱自己的两个方法，让每个发的地方一眼看得出是哪一种（名字可以调整）：
   - `must(msg).await`：结果和事实，一定要到。发的人是后台的任务，等得起（慢的等快的）。在 `spawn_blocking` 里用 `blocking_send`。
   - `may(msg)`：进度（`Writing`、`Filing`、`Moving`）。`try_send`，满了就丢：下一条进度会盖掉它。
2. `Core::send(command) -> Result<(), &'static str>`：`try_send`，满了返回 `Err("busy")`。界面的命令不许等。
3. `Core::looked` 改成 `async fn`，用 `must`：外壳找到新版本这件事不能丢。
4. 外壳（最小改动）：`dispatch` 里 `core.send` 返回 `Err(code)` 时，经现有的 `fan` 发一个 `Event::Notice(code)`；`look` 里 `.await` 那个 `looked`。P1-14 再让 `dispatch` 把结果直接还给界面。
5. `ui/lang` 里 `code.busy` 已经有词（「服务忙」一类），意思对得上就沿用；对不上就改这八处的词。

### 采集

6. 泵的邮箱：`sync_channel(64)`。
   - 声源线程（`hold`）发 `Feed`、`Notice`：`try_send`，满了隔一个 `TICK` 再试，采集结束了（`over`）就走。不许无限期地堵在发送上。
   - `pause`、`resume`：`try_send`，满了丢。
   - 停止：`Capture` 和 `Turner` 之间加一个原子的停止标志，`turn` 每圈都看；`Drop` 置标志，再 `try_send(Stop)` 只为了把泵早点叫醒。停止不再依赖队列里有位置。
7. `ender`：`sync_channel(2)`，发的一律 `try_send`（满了说明已经有一条在路上）。
8. 读文件线程的邮箱：`sync_channel(8)`，同样的办法。

### 守住

9. 仓库根加 `clippy.toml`：

   ```toml
   disallowed-methods = [
     { path = "tokio::sync::mpsc::unbounded_channel", reason = "every queue has a bound, and says what happens when it is full (CLAUDE.md)" },
     { path = "std::sync::mpsc::channel", reason = "unbounded: use sync_channel, and say what happens when it is full" },
   ]
   ```

   确认四个 crate 都吃到了它（在每个 crate 里临时写一处违规，clippy 都要报）。
10. `scripts/build/rules/bounded.sh`：在 `core`、`app`、`server`、`zu` 的 `src` 和 `tests` 下找 `unbounded_channel`、`UnboundedSender`、`UnboundedReceiver`、不带容量的 `mpsc::channel(`，找到就打印「文件:行」并以非零退出；`clippy.toml` 里那两行不在了，也以非零退出。
11. 测试跟着守：要「一条不丢」的收集器用有上限的通道，满了 panic（`try_send(..).expect(..)`），不加 `#[allow]`。

### 采集的回调不分配

12. `core/tests/callback.rs`（单独一个测试文件，因为要换全局分配器）：`the_device_callback_allocates_nothing`。回调做的事就是 `offer(&mut ring, samples)`；断言这一次调用里没有分配。能取到 `assert_no_alloc` 就用它（`[dev-dependencies]`）；取不到（没有网络）就在这个文件里写一个数次数的 `GlobalAlloc`，二十行以内。

## 先写的测试

1. `core/tests/app.rs`：`a_hub_that_cannot_keep_up_refuses_commands_and_says_busy`。单线程的运行时（`#[tokio::test]`），一口气调 300 次 `core.send(Command::Pause)`，中间不 `await`（hub 的任务没有机会跑）：成功的不超过 256 次，其余都是 `Err("busy")`，两者加起来 300。然后让出去，等 `said` 里出现和成功次数一样多的 `recording.none`：收下的每一条都处理了。现在 `send` 没有返回值，这条编不过。
2. `core/tests/app.rs`：`what_the_shell_found_is_never_dropped`。照上一条把邮箱塞满，再 `core.looked(Ok("0.9.1".into())).await`：它在 hub 腾出位置以后完成，`state.update` 是 `0.9.1`。
3. `core/tests/audio.rs`：`a_stop_reaches_a_pump_that_takes_no_messages`。`Capture::by_hand`，不转，连着 `pause`/`resume` 200 次（都不堵、不 panic），丢掉 `Capture`，再转一圈：返回 `false`。
4. `core/tests/callback.rs`：见上面第 12 条。
5. 规则脚本和 lint 的证明（写进报告，不留在仓库里）：在 `core/src/lib.rs` 里临时加一行 `let _ = tokio::sync::mpsc::unbounded_channel::<u8>();`，`cargo clippy` 和 `bounded.sh` 各自的输出；去掉以后两者都过。

## 验收

- `scripts/build/check.sh` 全绿（`bounded.sh` 在其中）。
- `grep -rn "unbounded\|mpsc::channel()" core app server zu --include=*.rs` 没有结果。
- 报告里列出邮箱的每一个发信处用的是 `must` 还是 `may`，各一句理由。

## 不做

- 不加 tape 和草稿的队列（P1-09），不加 desk 的队列（P1-12）。
- 不动 `broadcast(64)`（已有上限）、`ai/write.rs` 和 `out/city.rs` 里已有的有上限通道、服务器。
- 不把外壳的命令改成 async（P1-14）。
