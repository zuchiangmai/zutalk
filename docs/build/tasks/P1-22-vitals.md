# P1-22 外壳和界面也报：主线程心跳，画到第几帧，帧间隔

## 目标

做完以后：

1. 外壳每秒往主线程投一次心跳，**不等它**。主线程超过 2 秒没接，日志里有一条 `shell.main_thread.stalled`；接上以后有一条 `shell.main_thread.back`，带一共停了多久。0.8「录音中主线程卡死被强制退出」那一类事，从此在日志里有时刻、有时长。
2. 界面每秒最多回报一次：画到第几帧，以及这一秒里两帧之间最长隔了多久、隔得太久的有几次。核心的水位表从此有 `Screen` 这一格；隔得太久的在日志里是 `ui.frame.gap`。
3. 这两样的规则都是值进值出的小函数，各有表格测试；和真的主线程、真的 `requestAnimationFrame` 之间只隔一个传进来的函数。

这是 `PLAN.md` §4.5.2 第五条「外壳和界面也报」。

## 背景

- 设计：`docs/build/phases/P1.md` §3.13.7（心跳）、§3.13.8（帧间隔）、§3.13.11（`Beat` 是一个状态机）、§3.13.3（标着 P1-22 的两行）、§3.5（`Shell`、`Screen` 两格的预算）。
- 外壳（P1-21 之后）：`app/src/lib.rs`、`pump.rs`、`windows.rs`、`updates.rs`；外壳的树 `shell`；命令 `dispatch`、`subscribe`；`shell.sh` 禁的是**等**主线程回话的调用。
- `Core::drawn(seq)`（P1-14）：一次原子写，`Screen` 那一格的位置。现在没人调。`Core::stats()`。
- Tauri：`AppHandle::run_on_main_thread(closure)` 把闭包投到主线程的事件循环，立刻返回；主线程卡着的时候闭包就排着。
- 界面：`ui/src/lib/core.svelte.ts`（`store`、`take`、`tauri`）；`ui/src/ui/Captions.svelte` 画字幕。界面现在没有自己的 `requestAnimationFrame` 循环（「信号每帧只处理最新一份」是 P3 的）。

## 做法

### 心跳（`app/src/beat.rs`）

1. 规则是一个状态机，值进值出：

   ```rust
   /// A beat posted to the main thread, and whether it has come back.
   pub enum Beat { Idle, Out { seq: u64, sent_ms: u64 }, Late { seq: u64, sent_ms: u64 } }
   pub enum Tick { Time, Back(u64) }                       // the second came round; the main thread ran beat `seq`
   pub enum Effect { Post(u64), Took(u64), Stalled(u64), Back(u64) }   // ms
   pub fn step(beat: Beat, tick: Tick, now_ms: u64) -> (Beat, Vec<Effect>);
   /// How often a beat is posted, and how long one may be out before the main thread counts as stalled (PLAN §4.5.2).
   pub const BEAT_EVERY_MS: u64 = 1_000;
   pub const BEAT_STALLED_MS: u64 = 2_000;
   ```

   - `Idle` 遇 `Time` → `Out`，`[Post(seq)]`；
   - `Out` 遇 `Back(seq)` → `Idle`，`[Took(延迟)]`；遇 `Time`：没到 `BEAT_STALLED_MS` 不变（**上一个还没回来就不投下一个**：在路上的闭包最多一个）；到了 → `Late`，`[Stalled(已经等了多久)]`；
   - `Late` 遇 `Time` 不变（只说一次）；遇 `Back(seq)` → `Idle`，`[Back(一共停了多久)]`；
   - 序号对不上的 `Back` 不变。
2. 壳：`pub async fn run(post: impl Fn(Box<dyn FnOnce() + Send>) -> bool, stats: Stats)`。每 `BEAT_EVERY_MS` 一个 `Time`；投出去的闭包在主线程上只做一件事：把自己的序号写进一个寄存器（`Register::set`，不阻塞、不分配大块），`run` 等的是这个寄存器的下一次变化。效果怎么做：
   - `Post(seq)`：调 `post`；它返回 `false`（应用在退出）就结束；
   - `Took(ms)`：记进分布 `shell.beat_ms`；
   - `Stalled(ms)`：`warn!("shell.main_thread.stalled", waited_ms = ms)`，计数 `shell.beats.stalled` 加一；
   - `Back(ms)`：`info!("shell.main_thread.back", stalled_ms = ms)`，同样记进 `shell.beat_ms`。
   时间用 `tokio::time::Instant`（测试里是暂停的时钟；真机上和墙上时钟一样走）。
3. 接线（`main.rs`）：`shell.spawn("shell.beat", beat::run(move |beat| handle.run_on_main_thread(beat).is_ok(), stats))`。
4. 它**不**进水位表、不触发任何动作、不往界面说：主线程卡着的时候界面本来就动不了，日志是唯一说得出去的地方。

### 界面的回报

5. `ui/src/lib/vitals.ts`（纯函数，没有 DOM、没有时钟）：

   ```ts
   export const FRAME_GAP_MS = 250;     // two frames further apart than this is a gap (PLAN §4.5.2)
   export const REPORT_EVERY_MS = 1000; // the core hears at most this often
   export type Vitals = { last: number | null; worst: number; gaps: number; reported: number; due: number };
   export function frame(v: Vitals, now: number, hidden: boolean): Vitals;         // one animation frame came
   export function report(v: Vitals, seq: number, now: number): [Vitals, { seq: number; gap_ms: number; gaps: number } | null];
   ```

   - `frame`：和上一帧隔了超过 `FRAME_GAP_MS` 就记一次（次数加一，留最长的）。页面藏着（`hidden`）不记，并把「上一帧」清掉：藏过以后的第一帧不算。
   - `report`：离上次回报不到 `REPORT_EVERY_MS` 不报；帧的序号没比上次报的大、也没有间隔过长，不报；否则给出要报的三个数，并把这一秒的记录清零。
6. `ui/src/lib/core.svelte.ts`：
   - 收到 `Frame` 信号时记下它的序号（`Stamped.seq`）；
   - 起一个 `requestAnimationFrame` 循环：每帧调 `frame(v, performance.now(), document.hidden)`，再问 `report(..)`；有东西要报就 `invoke('drawn', …)`（不等它，失败了不重试：下一秒还会报）。循环只做这几次比较，不碰 DOM；
   - `Bus` 的类型加 `drawn`；`ui/dev/mock.ts` 的假核心不用报。主窗口和字幕窗各报各的。
7. 外壳的第三个命令：

   ```rust
   #[tauri::command]
   async fn drawn(window: WebviewWindow, seq: u64, gap_ms: u32, gaps: u32, core: State<'_, Core>) -> Result<(), String>;
   ```

   - `core.drawn(seq)`；
   - `gaps > 0`：`warn!("ui.frame.gap", window = Id(window.label()), gap_ms, n = gaps)`，分布 `ui.gap_ms` 记 `gap_ms`，计数 `ui.gaps` 加 `gaps`。
   - 界面是不可信的输入：`gap_ms`、`gaps` 只是数，照记；`seq` 比核心发过的帧还大时不往水位表里写（`Core::drawn` 里夹住：不超过 `Frame` 那一格现在的位置）。

## 显式与可观测

- **事件**：`shell.main_thread.stalled`、`shell.main_thread.back`、`ui.frame.gap`。写进 `docs/EVENTS.md`。
- **统计**：计数 `shell.beats.stalled`、`ui.gaps`；分布 `shell.beat_ms`、`ui.gap_ms`。
- **状态机**：`Beat`（转换表测试第 1 条）。
- **数**：`BEAT_EVERY_MS`、`BEAT_STALLED_MS`（Rust）；`FRAME_GAP_MS`、`REPORT_EVERY_MS`（TypeScript）。四个都是 `PLAN.md` §4.5.2 定的。
- **队列**：没有。在路上的心跳最多一个；界面的回报每秒最多一次、失败不排队。
- **任务**：外壳的树上多一个 `shell.beat`。
- **水位**：`Screen` 这一格从此有位置（`Core::drawn`）；它的预算（5 秒不动是「慢」，没有动作）在 `BUDGETS` 里，P1-07 已经写好。
- **`shell.sh`**：照旧通过。`run_on_main_thread` 只投不等。

## 先写的测试

`app/tests/beat.rs`：

1. `the_beat_steps_as_its_table_says`：`step` 的转换表，每个（状态，`Tick`）一行，时间取界限两侧（1999、2000 毫秒）。
2. `a_main_thread_that_answers_is_timed_and_nothing_is_said`（暂停的时钟；`post` 的替身立刻在另一个任务上跑闭包；装捕获）：过 10 虚拟秒：`post` 被叫了 10 次上下；分布 `shell.beat_ms` 的个数相同；没有 `shell.main_thread.stalled`。
3. `a_main_thread_that_stands_still_is_said_once_and_its_return_says_how_long`：`post` 的替身把闭包扣住不跑。第 2 秒以后：`shell.main_thread.stalled` 正好一条（`waited_ms = 2000`）；再过 30 秒仍然只有一条，`post` 没有被再叫（在路上的只有一个）；计数 `shell.beats.stalled` 是 1。放开闭包：`shell.main_thread.back` 一条，`stalled_ms` 是从投出到放开的虚拟毫秒数；之后心跳照常。
4. `the_beat_ends_when_the_app_is_going`：`post` 返回 `false`：`run` 结束。

`ui/tests/vitals.test.ts`：

5. 「两帧隔得太久记一次，留最长的」：帧在 0、16、32、400、416、1100 毫秒来：一秒末报 `{ gap_ms: 684, gaps: 2 }`（数照实算）。
6. 「藏着的页面不算，藏过以后的第一帧也不算」。
7. 「一秒最多报一次；没画新的帧、也没有间隔过长就不报」：表格：序号没涨且没有间隔 → 不报；序号涨了 → 报，`gaps = 0`；不到一秒 → 不报。
8. 「报过以后这一秒的记录清零」。

`core/tests/liveness.rs`（台架）：

9. `the_screen_mark_follows_what_the_interface_says_it_drew`：录音中每虚拟秒调一次 `core.pushed(seq)`、`core.drawn(seq)`（照外壳和界面的样子）：`Water` 里 `Shell`、`Screen` 两格在走，是 `Live`。停掉 `drawn` 6 秒：`Screen` 是 `Slow`，`judge.verdict`（`stage = "Screen"`、`to = "Slow"`）一条，没有 `stall.snapshot`，没有动作。`drawn` 给一个比发过的帧还大的序号：`Screen` 的位置不超过 `Frame` 的。

## 验收

- `scripts/build/check.sh` 全绿。
- 报告里写明：门禁里没有真的主线程和真的浏览器。真机上要看的两件事交给负责人，记进给 P1-24 的清单：
  - 录音中把应用的主线程弄停几秒（比如开着一个系统的打开文件对话框，或者用调试器暂停几秒）：`logs/zutalk.jsonl` 里有一条 `shell.main_thread.stalled` 和一条 `shell.main_thread.back`；
  - 录音中把主窗口切到后台再切回来：没有因此多出 `ui.frame.gap`；正常录十分钟，`ui.frame.gap` 的条数写下来。
- `ui` 里新加的循环不做和历史长度成正比的事（审查：`vitals.ts` 里没有数组）。

## 不做

- 不让心跳触发任何动作（重启窗口、结束录音）：只报。
- 不画「运行状况」浮层（P3）；`store.stats`、`store.water` 只是存着。
- 不做「信号每帧只处理最新一份」（P3）：这一件的 rAF 循环只量间隔和回报。
- 不给观看页（网页上的观众）加回报：它没有核心可报。
