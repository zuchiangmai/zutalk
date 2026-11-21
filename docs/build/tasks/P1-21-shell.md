# P1-21 外壳：一个任务独占推送，命令全部 async，没有锁

## 目标

做完以后：

1. 外壳里没有同步的命令、没有 `Mutex`、没有要主线程回话的窗口查询。主线程不会被等，也不等别人。
2. 往窗口推东西的是一个任务，订阅者的表和每种值的最新一份都只在它手里。页面重新加载，旧的通道被换掉；新窗口一订阅就拿到小状态和每个信号现在的值。
3. 推送的规则是一个不认识 Tauri 的状态机（`Fan`），从 `app/tests/pump.rs` 穷举交错来测。
4. 外壳报「推到第几帧」；`dispatch` 把拒绝的原因直接还给界面。
5. 外壳的任务都从它自己的树上起，各有名字；订阅、换通道、通道没了、字幕窗显隐、看新版本，都在日志里。
6. `scripts/build/rules/shell.sh` 守着第 1 条；`spawn.sh` 在 `app/src` 的欠账清零。

这是 `PLAN.md` §7 的 I5，和 I4 的后一半。主线程心跳、界面回报画到第几帧和帧间隔，是 P1-22。

## 背景

- 设计：`docs/build/phases/P1.md` §3.9；为什么不用 `arc-swap` 和 `loom`：§2 末尾；外壳的树：§3.6；事件：§3.13.3（标着 P1-21 的四行）。`PLAN.md` §4.2「外壳 `fan` 拿着锁调用 `is_visible()`」一行、§4.4「页面重新加载零成本」、§4.5.2「外壳的订阅、换通道、窗口显隐」。
- `app/src/main.rs`（P1-03、P1-09 至 P1-11 之后）：
  - 启动时装日志、发 `app.start`（P1-03）；外壳的根树 `Tree::root("shell", core.stats())` 和上面的 `shell.forward` 任务（P1-09）；
  - `Windows = Arc<Mutex<(Vec<Channel<Event>>, Option<Event>)>>`；`subscribe` 是同步命令，在主线程上拿这把锁；`fan` 拿着同一把锁调 `window.is_visible()`，而这个查询要主线程回话。两边一碰就是死锁：0.8「录音中主线程卡死被强制退出」的同一类毛病；
  - `dispatch` 是同步命令；
  - `Ready = Arc<Mutex<Option<(Update, Vec<u8>)>>>`，`installer` 拿着锁装更新；
  - `setup` 里 `block_on`；看新版本的任务是裸的 `async_runtime::spawn`（`spawn.sh` 的最后一处欠账）。
- `Outlet`（七个 `Reading`，可克隆）、`Core::send`（返回 `Result`）、`Core::looked`（async）、`Core::pushed`、`Core::stats`。
- `zutalk_core::rt`：`queue::bounded`、`supervise::Tree`、`Tree::keep`。外壳依赖核心，直接用。
- `app/Cargo.toml`：现在只有一个二进制目标；`tokio` 只开了 `time`。`app/tests/updates.rs` 只读配置文件。
- Tauri 的 `Channel::send`：8 KB 以内的消息直接注入页面，不阻塞；页面重新加载后，旧通道的 `send` 不一定报错，所以「发不出去才丢」不够，要按窗口换。
- 界面：`ui/src/lib/core.svelte.ts` 的 `tauri`（`invoke('dispatch')`、`invoke('subscribe', { channel })`）和 `take`。

## 做法

### 库目标

1. `app` 加 `src/lib.rs`，里面 `pub mod pump;`（`windows`、`updates` 也可以放进库里）。`main.rs` 只剩接线。`app/Cargo.toml` 的 `tokio` 加 `sync`、`macros`（测试再加 `test-util`、`rt`）。

### `pump.rs`

2. 照 `P1.md` §3.9 的签名：`Sink`、`Fan`、`run`。
   - `Fan::join(window, sink) -> bool`：这个窗口原来的通道被换掉（返回值说有没有换掉一个）；新的先收到 `State`（有的话），再按固定的次序收到每种信号最新的一份。提示不补。
   - `Fan::push(event) -> Vec<String>`：记成这一种的最新一份（提示不记）；发给每个通道；通道说 `false` 就拿掉，返回值是这一次被拿掉的窗口。同一种信号只往前走：序号不比上次发出去的大，就不发。
   - 通道的 `send` 里 panic 了，当作 `false`（`Fan` 里接住），那个窗口同样被拿掉，返回值里注明原因。
   - `Fan` 是纯的状态机：不起任务、不读时钟、不认识 Tauri、**不发事件**（`pump.rs` 里 `Fan` 的那一段没有 `zutalk_log`）。
3. `run`：一个循环，同时等：新的订阅、`Outlet` 的七个 `Reading`。它是 `Fan` 外面的那层壳，事件由它发：
   - 新的订阅：`fan.join(..)`，然后 `info!("shell.join", window = Id(&label), replaced)`；
   - 每推一个东西：`fan.push(..)`；被拿掉的每个窗口一条 `warn!("shell.sink.gone", window = Id(&label), why)`（`why` 是 `closed` 或 `panic`），计数 `shell.sinks.gone` 加一；
   - 每推一帧，计数 `shell.pushed` 加一，调一次「推到第几帧」（传进来的函数；`main.rs` 给的是 `core.pushed`）；
   - `said`：自己记着发到第几条，新的每条发一个 `Event::Notice`；落后太多（超过 `Notices::KEEP_CODES`）就从还留着的最旧一条接着发；
   - 字幕窗：记着自己上次要它显示还是隐藏；`State.captions` 和记着的不一样时，调一次「显示/隐藏」（传进来的函数），再记下，发一条 `info!("shell.captions", shown)`。从不问窗口现在什么样；
   - 原计划这里的 `ZUTALK_WATER_LOG` **取消**：每秒的水位已经是日志里 `debug` 级的 `water.mark`（P1-14），外壳不另写文件。
4. 推送任务崩了要重起，窗口不用重新订阅：订阅的口和 `Fan` 由外面一层拿着，里面一层崩了再进。它是外壳的树上一个被守着的部件：`shell.keep("shell.pump", …)`；重起有 `part.state`，不设重启上限以外的特殊规矩（到了上限 `part.gave_up`：那时外壳已经没法往窗口说话，日志是唯一的出口）。P1-09 的 `shell.forward` 任务删掉，`pump` 取代它。

### 命令

5. 两个命令，全是 `async fn`（第三个 `drawn` 是 P1-22 的）：

   ```rust
   #[tauri::command]
   async fn dispatch(command: Command, core: State<'_, Core>) -> Result<(), String>;      // Err: the code
   #[tauri::command]
   async fn subscribe(window: WebviewWindow, channel: Channel<Event>, joins: State<'_, Joins>) -> Result<(), String>;
   ```

   `subscribe` 只是把 `(窗口的标签, 通道)` 发给推送任务：`queue::bounded(&stats, "shell.joins", SHELL_JOINS_MSGS, Full::Wait)`（8：窗口只有两三个；命令是 async 的，等得起）。`impl Sink for Channel<Event>`。
   `dispatch` 不用自己发事件：收下的有 `command.done`，没进邮箱的有 `command.refused`（P1-11）。

### 窗口与更新

6. `windows.rs`：现在只有「让字幕窗显示/隐藏」一个函数（`show()`、`hide()` 是往主线程投递，不等回话）。它们返回的错误不是丢弃：没成就发一条事件（用 `shell.captions` 加一个 `code` 字段，或者单列一条；选一种写进 `docs/EVENTS.md`）。P3、P5 在这里加大小、位置、穿透点击。
7. `updates.rs`：外壳的树上一个任务（`updates`）拿着下好的更新。它照旧启动时和每 `LOOK_MS`（6 小时）看一次，每看一次发一条 `update.look`（`code` 是 `none`、`ready`，或者出错的代码，出错用 `warn!`）；找到了 `core.looked(..).await`。给核心的 `Install` 端口是一个闭包：把「装吧」和一个回话的口发给这个任务，然后等回话；任务装上了就重启应用（闭包永远等不到回话），装不上就回话，闭包返回。没有锁。（核心从 P1-18 起是在阻塞线程上调这个端口的，等得起。）「装吧」的口是一条 `queue::threads` 或一个 `oneshot`（一次一个，不是队列）：选简单的那个，写明。
8. `setup` 里不 `block_on`：`Core::start` 只起任务，进入运行时的上下文就够了。

### 界面（最小）

9. `ui/src/lib/core.svelte.ts`：`send`：`invoke('dispatch')` 被拒绝时，把代码 `say` 出来。`Bus` 的类型跟着改。

### 规则脚本

10. `scripts/build/rules/shell.sh`，对 `app/src`：
    - 每个 `#[tauri::command]` 后面的函数是 `async fn`；
    - 没有 `Mutex`、`RwLock`、`.lock()`、`block_on`；
    - 没有要主线程回话的窗口查询：`is_visible`、`is_focused`、`is_minimized`、`is_maximized`、`inner_size`、`outer_size`、`inner_position`、`outer_position`、`scale_factor`、`current_monitor`、`primary_monitor`、`available_monitors`；
    - 违反就打印「文件:行」并以非零退出。
    `run_on_main_thread`（只投不等）不在禁单里：P1-22 的心跳用它。
11. `spawn.sh` 的欠账表里 `app/src/main.rs` 那一行删掉：`core/src`、`app/src` 的欠账表从此是空的。

## 显式与可观测

- **事件**：`shell.join`、`shell.sink.gone`、`shell.captions`、`update.look`。写进 `docs/EVENTS.md`。`part.state`（`part = "shell.pump"`）、`task.*`（`tree = "shell"`）、`queue.full`（`shell.joins`，它是 `Full::Wait`，正常不会有）是地基发的。
- **统计**：计数 `shell.pushed`、`shell.sinks.gone`；队列 `shell.joins` 一行。外壳数的数进核心那一份 `Stats`（`core.stats()`），所以 `Stats` 信号、每分钟的汇总和自检包里都有它们。
- **状态机**：`Fan`（每个窗口：没订阅、订阅着；穷举交错的测试第 1 条就是它的转换表）。
- **数**：`SHELL_JOINS_MSGS`（8）、`LOOK_MS`（P1-04 已改名）。
- **队列**：`shell.joins`，`Full::Wait`。
- **任务**：外壳的树 `shell` 上：`shell.pump`（被守着）、`updates`。
- **欠账**：`spawn.sh` 的 `app/src/main.rs` 一处，还清。
- **隐私**：`window` 是窗口的标签（`main`、`captions`），写死在配置里的。事件里没有推出去的内容。

## 先写的测试

`app/tests/pump.rs`（装捕获的那几条注明了）：

1. `every_interleaving_ends_with_each_window_holding_the_latest`。两串动作：订阅的一串 `[join(main, a), join(captions, b), join(main, c)]`（第三个是页面重新加载），推送的一串 `[State₁, Frame 1, Session 1, Frame 2, Notice x, State₂, Frame 3]`。保持各自内部的次序，把两串所有的交错（120 种）都跑一遍，每一种都断言：
   - `b` 和 `c` 各自最后收到的 `State` 是 `State₂`，最后收到的 `Frame` 是 3 号；
   - 每个通道收到的 `Frame` 序号严格递增；
   - `c` 订阅以后，`a` 再没有收到任何东西；`join(main, c)` 的返回值说换掉了一个；
   - `Notice x` 只被当时订阅着的通道收到，各一次；之后订阅的没有补到它。
2. `a_page_that_is_gone_is_let_go`：通道说 `false` 以后不再被发；`push` 的返回值里有它。
3. `a_signal_older_than_the_last_one_sent_is_not_sent`。
4. `a_window_that_joins_late_gets_the_state_and_every_signals_latest`（暂停的时钟，真的 `run`，寄存器由测试自己建；装捕获）：先 `set` 好各个寄存器，再订阅：收到的头几条正是 `State` 和每种信号最新的一份（含 `Stats`）；之后每 `set` 一次收到一条；「推到第几帧」被叫到的序号和帧的序号一致。**看得见**：`shell.join`（`window = "main"`、`replaced = false`）一条；计数 `shell.pushed` 等于推出去的帧数。
5. `a_reload_while_frames_are_coming_loses_nothing_and_the_old_channel_goes_quiet`：帧每 100 虚拟毫秒来一份，中间同一个窗口再订阅一次：新通道第一条 `Frame` 就是当时最新的；旧通道此后一条也没有。**看得见**：第二条 `shell.join` 的 `replaced` 是真。
6. `the_captions_window_follows_the_state_and_is_never_asked`：`State.captions` 依次是假、真、真、假：「显示/隐藏」被调的是 `[真, 假]`；`shell.captions` 两条，`shown` 先真后假。
7. `codes_are_said_once_each`：`said` 里连着来 3 条；再来 20 条（超过留着的 16 条）而推送任务这期间没有机会跑：发出去的没有重复，最后一条是最新的。
8. `a_channel_that_panics_does_not_end_the_pump`：一个 `send` 会 panic 的通道订阅进来；别的窗口照样收到之后的帧。**看得见**：`shell.sink.gone`（那个窗口，`why = "panic"`）一条；计数 `shell.sinks.gone` 是 1；没有 `part.state`（推送任务自己没有崩）。
9. `a_pump_that_dies_is_started_again_and_no_window_has_to_join_again`：让里面那一层崩一次（给测试一个口：一个会让 `run` 的一圈 panic 的替身「显示/隐藏」函数）：之后的帧照样到已经订阅的窗口。**看得见**：`part.state`（`part = "shell.pump"`、`to = "Waiting"`、`why = "panic"`）一条。

`ui/tests/store.test.ts`（或新文件）：

10. 「被拒绝的命令说出代码」：假的 `Bus` 的 `send` 以 `busy` 拒绝，`store.notices` 里多一条。

规则脚本的证明（写进报告）：把现在的 `main.rs`（改之前）喂给 `shell.sh`，它报出同步命令、`Mutex`、`is_visible`、`block_on`。

## 验收

- `scripts/build/check.sh` 全绿（`shell.sh` 在其中）；`spawn.sh`、`bounded.sh` 的欠账表里没有 `core/src`、`app/src` 的行。
- `grep -rn "Mutex\|is_visible\|block_on\|WATER_LOG\|async_runtime::spawn" app/src` 没有结果。
- 报告里写明：门禁里跑不了真的窗口；真机上要看的两件事（开发模式下连按十次重新加载，字幕和状态都接得上，`logs/zutalk.jsonl` 里有十条 `replaced` 为真的 `shell.join`；录音中开关字幕窗二十次不卡，日志里有二十条 `shell.captions`）交给负责人，并已记进给 P1-24 的清单。

## 不做

- 主线程心跳、`drawn` 命令、界面的帧间隔（P1-22）。
- 字幕窗的大小、位置、穿透点击、舞台全屏（P3、P5）。
- 界面「信号先存下来，每帧只处理最新的」（P3）；不换框架。
- 观看页、房间。
- 不动 `app/tauri.conf.json`、更新的清单脚本和 `app/tests/updates.rs` 的断言。
