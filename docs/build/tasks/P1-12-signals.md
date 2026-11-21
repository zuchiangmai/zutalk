# P1-12 现场的路绕开 hub：`Frame`、`Session`、`Level` 是信号

## 目标

做完以后：

1. 字幕的帧（`Frame`）、录音的状态句（`Session`）、电平（`Level`）由录音任务直接写进寄存器，不经 hub 的邮箱。hub 停住的时候，它们照样到达。
2. 采集线程上的闭包不再往 hub 的邮箱里发任何东西。
3. 界面和核心之间的协议是 `Event = State | Signal | Notice`（`docs/build/phases/P1.md` §3.3）；`State` 里没有 `session` 了。
4. 外壳和界面跟着改到能用；全仓库绿。
5. 一场录音的开始和结束、状态句的每次变化，都是日志里的一条事件。

`PLAN.md` §4.2 第一行：「`Msg::Live` 经过 hub 的无上限邮箱 → 录音任务直接写 `Frame` 寄存器」。

## 背景

- 设计：`P1.md` §3.3（协议、`Session`、`Level`）、§3.6（采集线程来的终止事实走 `watch`）、§3.13.3（标着 P1-12 的三行）、§3.13.11（`Status` 是一个状态机）。
- P1-11 之后：`Core::start` 返回 `(Core, Outlet)`；`Outlet` 的 `session`、`level` 没人写；帧是 hub 收到 `Msg::Live` 后写进寄存器的。
- `core/src/app/hub.rs`：
  - `Msg::Live`、`Msg::Status`、`Msg::Say`、`Msg::Ended`；`state.session`（开始时设、停止时清、`Msg::Status` 改它的 `status`）；
  - `Task::listen`：帧的定时器里 `self.tell(Msg::Live(..))`、`self.tell(Msg::Status(..))`；`sink` 和 `notice` 两个闭包跑在采集的线程上，里面 `tx.send(Msg::Say(..))`、`tx.send(Msg::Ended(..))`；
  - `Command::Setup`、`Command::DestroyAudio`（经 `retain::destroy` 看 `state.session`）。
- `core/src/app/proto.rs`：`State`（有 `session`）、`Session`、`Event`（`State`、`Live`、`Notice`）。
- `core/src/record/retain.rs`：`destroy(tapes, &State, id)`。
- 外壳 `app/src/main.rs`：P1-11 加的转发任务。
- 界面：`ui/src/lib/core.svelte.ts`（`store`、`take`）；`ui/src/ui/{App,Home,Live,Reader}.svelte` 读 `s.session`；`ui/dev/mock.ts`；`ui/tests/store.test.ts`。观看页 `ui/src/lib/room.svelte.ts` 把槽里的 `Column[]` 放进 `store.live`。

## 做法

### 核心

1. **协议**（`app/proto.rs`）：加 `Level`、`Stamped`、`Signal`；`Event` 改成 `State(Arc<State>) | Signal(Stamped) | Notice(String)`；`State` 去掉 `session`。`core/Cargo.toml` 的 `serde` 加 `rc` 特性。`Water`（P1-09）和 `Tally`（P1-04）的类型已经有了，这一件把它们放进 `Signal`。
2. **录音任务直接写**：开始录音时，hub 把 `frame`、`session`、`level`、`said` 四个寄存器的克隆交给录音任务。
   - 帧的定时器：`composer.frame()` 有值就 `frame.set(..)`；
   - 状态句变了就 `session.change(..)`，**只在寄存器里那一场的 `id` 和自己相同时才改**；同时发一条 `info!("session.status", rec = Id(&id), from, to, why)`。`Status` 加 `code()`（`Recording`、`Paused`、`AudioOnly`、`Connecting`、`Behind`），`AudioOnly { why }` 的原因进 `why` 字段。`status(..)` 本来就是纯函数，「转换」就是它的输出变了：录音任务记着上一次的，不一样才写、才发；
   - `sink` 里每块算一次 `Level { peak, rms }`，`level.set(..)`；暂停时归零；
   - 录音任务要说的代码直接 `said.say(code)`（在哪个线程上都行）。
   - `Msg::Live`、`Msg::Status` 删掉；`Msg::Say` 没人用了就删掉。
   - **开始和结束**：录音任务的第一件事是 `info!("recording.start", rec = Id(&id), source = …, lanes = …, columns = …, captions = …)`（`source` 是声源种类的代码：`mic`、`system`、`both`、`file`；不是设备的名字，也不是文件的路径。`captions` 是 `live`，或者 `lane::captions`（P1-07）给的那个没有字幕的原因：`file`、`captions.off`、`captions.no_key`）。它的最后一件事（封好 tape、写上长度以后；或者中途失败）是 `recording.end`（`rec`、`code`、`ms`）：`code` 是 `done`，或者结束它的代码，这时用 `error!`。这两条之间的事件都属于这一场（`P1.md` §3.13.2）。
3. **采集线程上的闭包不碰邮箱**：它们把终止的事实写进一个 `watch::Sender<Option<&'static str>>`（`done`，或结束它的代码）。`listen` 的循环里多一个分支等它变，再由录音任务（一个 async 任务）去说代码、告诉 hub 这一场结束了（`Msg::Ended` 仍然是给 hub 的一条消息，但发它的是 async 任务，不是线程）。这个 `watch` 不是队列（只留最新的一个事实，不会满），不归 `bounded.sh` 管；它的发送从不失败到需要丢弃。
4. **hub**：开始录音 `session.set(Some(Session { id, started, status: Connecting }))`；停止或这一场自己结束时 `session.set(None)`、`frame.set(Frame::default())`、`level.set(Level::default())`。「停止立刻把应用交还」照旧。hub 自己判断「有没有在录」用的是它的 `recording` 字段，本来就不靠 `state.session`。
5. `Command::Setup` 里 `shown` 超过 `MAX_COLUMNS` 个：拒绝，`captions.columns`。
6. `retain::destroy` 不再看 `State.session`：把「正在录的是哪一条」作为参数传进去。

### 测试台

7. 测试里读 `state.session` 的地方改读 `outlet.session`（`App` 加一个等 `session` 满足条件的方法）。台架的探针里「现在的状态句」同样改。

### 外壳（最小的改动，P1-23 重写）

8. `app/src/main.rs` 的转发任务：`Outlet` 的每个 `Reading` 变了，就包成新的 `Event`（`State`、`Signal(Stamped { seq, signal })`、`Notice`）交给 `fan`。`fan` 里「后开的窗口先收到最后一份状态」的逻辑先留着。

### 界面（最小的改动，P3 重写）

9. `ui/src/lib/core.svelte.ts`：`store` 加 `session`、`level`、`water`、`stats`（`Stats` 信号这一件只是存下来；画它的浮层是 P3）；`take` 里 `Signal` 按种类分，只收序号比这一种上次见到的大的；`Frame` 的 `columns` 放进 `store.live`（名字不改，观看页还在用它）。
10. `App.svelte`、`Home.svelte`、`Live.svelte`、`Reader.svelte` 里的 `s.session` 改读 `store.session`。
11. `ui/dev/mock.ts`：`EMPTY` 去掉 `session`；假核心用 `Signal` 发字幕和状态句。
12. `ui/lang/*.json` 八个文件加 `code.captions.columns`。
13. `core/bindings/` 里变了的和新增的类型一起提交。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `i1_captions_do_not_resend_the_state`：字幕稳定地走 60 虚拟秒，这期间 `outlet.state` 的序号没有变，`outlet.frame` 的序号涨了几百。（现在状态句一变，整个 `State` 连同资料库重发一次。）
2. `a_stop_clears_the_session_and_no_late_status_brings_it_back`：`Stop` 以后 `outlet.session` 立刻是 `None`；再过 10 虚拟秒（录音任务还在收尾）它仍是 `None`，`outlet.frame` 是空的。
3. `the_level_follows_the_sound_and_falls_to_nothing_when_paused`。
4. `a_recording_the_capture_ends_is_ended_by_the_recording_task`：话筒自己说结束（像合盖）：`session` 变成 `None`，草稿留着。断言的是结果；结构上的要求由验收里的 `grep` 守。**看得见**：`recording.end` 一条，`code` 是话筒说的那个代码，级别 `error`；`notice.said` 里有同一个代码。
4a. `a_recording_says_when_it_starts_how_it_goes_and_when_it_ends`：正常录 30 虚拟秒、暂停、继续、停止。`recording.start` 一条（`lanes = 1`、`columns = 2`、`captions = "live"`、`source` 是代码）；`session.status` 依次是 `Connecting → Recording → Paused → Recording`，`rec` 都是这一场的 id；`recording.end` 一条，`code = "done"`。两场接着录：第二场的事件带的是第二个 id。

`core/tests/app.rs`：

5. `a_fifth_column_is_refused`：`Setup` 带 5 种字幕语言 → `captions.columns`，`state.setup` 没变；`notice.said`（`code = "captions.columns"`）一条。
5a. `without_a_key_sound_is_kept_and_the_status_says_why`（P1-07）加一条断言：`recording.start` 的 `captions` 是 `captions.no_key`，`lanes` 是 0；`session.status` 只有一条，到 `AudioOnly`，`why = "captions.no_key"`。
6. 现有的测试改读 `outlet.session` 后全部通过。

`ui/tests/store.test.ts`：

7. 「一种信号只收比上次新的」：先收 `seq: 5` 的 `Frame`，再来 `seq: 4` 的，`store.live` 不变；`Session` 的 `seq: 4` 不受 `Frame` 的序号影响，照收。
8. 「录音的状态是信号」：`Signal(Session)` 进 `store.session`，`State` 不动它。`Signal(Stats)` 进 `store.stats`。

## 显式与可观测

- **事件**：`recording.start`、`recording.end`、`session.status`。写进 `docs/EVENTS.md`。
- **统计**：这一件不加新的名字（`frame.made` 等跟着组帧的代码走）。
- **状态机**：`Status`。它是 `status(..)` 的输出；P1-10 的表格测试 `the_status_line_says_what_most_needs_saying` 就是它的转换表，这一件给它补上每个状态的 `code()`。
- **数**：电平每块算一次，没有新的时限。`MAX_COLUMNS` 已有。
- **队列、任务**：不造新的队列，不起新的任务（欠账表不许变大）。终止的事实走 `watch`。
- **隐私**：`recording.start` 的 `source` 是种类的代码。设备的名字、导入文件的路径和文件名（它会成为标题）都不进日志；隐私测试（P1-08）现在断言日志里有 `recording.start` 和 `recording.end`，把它那一行「日志不是空的」的断言换成这两条。

## 验收

- `scripts/build/check.sh` 全绿。
- `grep -rn "Msg::Live\|Msg::Status\|Event::Live" core app` 没有结果；`core/src/app/hub.rs` 里 `sink`、`notice` 两个闭包不再出现 `tx`。
- 报告里再贴一次 `i1_eight_hours_…` 的数。

## 不做

- hub 的邮箱还是无上限的（P1-13）；tape 和草稿还在原处写（P1-15）；`Water` 没人写（P1-16、P1-17）。
- `State` 里的 `library`、`open`、`hits`、`terms`、`writing`、`filing`、`moving` 不动（P2）。
- 外壳不重写，`fan` 的锁还在（P1-23）。界面不做每帧只处理最新一份（P3）。
- 观看页收到的仍是 `Column[]`。
