# P1-06 核心往外说的都是寄存器：`Outlet`

## 目标

做完以后：

1. `Core::start(config)` 返回 `(Core, Outlet)`，不再收回调。小状态、字幕的帧、提示，都是寄存器：只留最新的，谁来读都立刻拿到现在的值。
2. hub 不再在自己的任务上调外壳的函数（序列化、往窗口发，都不再占 hub 的时间）。
3. 核心的测试台只有一份，直接读 `Outlet`；台架的探针也读它。

这一件只换「怎么往外说」。帧仍然经 hub 的邮箱到寄存器，协议在线上的样子不变：那是 P1-07。

## 背景

- 设计：`docs/build/phases/P1.md` §3.3（`Register`、`Outlet`、`Notices`）。`PLAN.md` §2.3（信号：只留最新值；重新订阅时立即拿到当前值）、§4.4（页面重新加载零成本）。
- `core/src/rt/signal.rs`（P1-04）：`Register`、`Reading`。`core/src/live/frame.rs`（P1-05）：`Frame`。
- `core/src/app/hub.rs`：
  - `Core::start(config, out)`；`Hub.out`；`publish()`（状态变了就 `(self.out)(Event::State(..))`）；各处 `(self.out)(Event::Notice(code))`；
  - `Msg::Live(id, columns)`：hub 收到后 `self.live.send_replace(..)`（给房间的槽）并 `(self.out)(Event::Live(..))`；
  - `Hub.live`（`watch`）和 `slot()`：房间的槽已经是「读一个只留最新值的东西」的写法。
- 测试台有两份几乎一样的：`core/tests/hub/mod.rs`（`App`，给 `city`、`glossary`、`retain` 用）和 `core/tests/app.rs` 开头自己的一份。两份都用回调把事件塞进一条通道，`until` 一条条看过去。台架的探针在 `core/tests/bench/mod.rs`，P1-03 把「从哪里读」挡在了一两个函数后面。
- 外壳 `app/src/main.rs`：`out` 闭包调 `fan`。

## 做法

### 核心

1. `Outlet`：照 `P1.md` §3.3，六个 `Reading`，可以克隆。`session`、`level`、`water` 三个现在没人写（P1-07、P1-10 接上），先放着空的初值。
2. `Core::start(config) -> Result<(Core, Outlet), End>`。hub 拿着对应的 `Register`：
   - `state`：`publish()` 在状态变了的时候 `set`（`Hub.sent` 那份比较照旧）；
   - `said`：类型是 `Notices`。给 `Register<Notices>` 一个 `say(code)`：用 `change`，加到末尾，超过 `Notices::KEEP`（16）丢最旧的，`next` 加一。原来所有 `(self.out)(Event::Notice(..))` 的地方都改成它；
   - `frame`：`Msg::Live` 改成带整个 `Frame`；hub 收到后 `frame.set(..)`。停止时设成空的 `Frame`。
3. 房间的槽读 `Reading<Frame>`，写进槽的仍是 `frame.columns` 的 JSON，没有栏就清空。`Hub.live` 删掉。
4. `Event`、`State` 的定义不动。

### 测试台

5. 两份并成一份（`core/tests/hub/mod.rs`），`core/tests/app.rs` 改用它。`App` 拿着 `Core` 和 `Outlet`：
   - `until(holds)`：先看 `state.now()`，不行再等 `next()`；
   - `notice(code)`：自己记着看到 `said` 的第几条，从那里往后找；
   - 要看字幕的测试读 `outlet.frame`。
6. **寄存器只留最新值**：等一个一闪而过的中间状态的测试会漏掉它。现有的测试里至少有一处：`nothing_is_recorded_before_someone_signs_in` 先等 `login` 有值、再等它没了，而中继连不上时这两步之间只隔几毫秒。这类地方改成断言稳定的事实，或者让那个中间状态真的停得住（比如给登录的中继一个不马上拒绝的替身）。**不许**靠加长等待碰运气。把每一处这样的改动列在报告里。
7. 台架的探针改成读 `Outlet`。

### 外壳（最小的改动，P1-14 重写）

8. `app/src/main.rs`：起一个任务，读 `Outlet` 的 `state`、`frame`、`said`，变了就包成现有的 `Event`（`State`、`Live(frame.columns)`、`Notice`）交给现有的 `fan`。`fan` 的锁、同步的命令都先不动。

## 先写的测试

`core/tests/app.rs`：

1. `whoever_reads_late_gets_what_is_there_now`：核心起好、登录的人读出来以后，再克隆一份 `Outlet`：`state.now()` 立刻就是现在的状态，不用等下一次变化。
2. `more_codes_than_are_kept_lose_the_oldest`：不读 `said`，连发 20 条会被拒绝的命令（没在录音时 `Pause` → `recording.none`）；`said.now()` 里是 16 条，`next` 是 20。
3. `the_recovery_key_goes_to_the_clipboard_and_through_nothing_else`（已有）：「钥匙没有到页面」改成检查途中每一份 `State` 和 `Notices` 的序列化结果。
4. `a_room_carries_the_live_captions_…`（已有）：改成从 `outlet.frame` 读到 `Hello`，其余断言不变。
5. 其余现有的 hub 测试（`app.rs`、`glossary.rs`、`retain.rs`、`city.rs`）改用新的测试台后全部通过，断言不放松。

`core/tests/liveness.rs`：

6. 两条 `i1_*` 和其余没挂起的照旧通过（探针换了读法，数字应该不变）。

## 验收

- `scripts/build/check.sh` 全绿。
- `grep -rn "Box<dyn Fn(Event)\|self.out)" core/src` 没有结果；`core/tests` 里只有一份 `App`。
- 报告里：改了断言方式的测试清单（第 6 条做法）；`i1_eight_hours_…` 的数（和 P1-05 的对比）。

## 不做

- 帧还经 hub 的邮箱（`Msg::Live`、`Msg::Status` 都还在）；`State.session` 还在；协议不变（P1-07）。
- hub 的邮箱还是无上限的（P1-08）。
- 外壳不重写（P1-14）；界面一行不动。
