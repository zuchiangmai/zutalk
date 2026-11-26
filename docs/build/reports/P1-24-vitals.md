状态：完成

## 改了什么、动了哪些文件

- **外壳主线程心跳（`app/src/beat.rs`、`app/src/lib.rs`、`app/src/main.rs`）**：
  - `app/src/beat.rs`：实现纯状态机 `Beat { Idle, Out { seq, sent_ms }, Late { seq, sent_ms } }`、`Tick { Time, Back(u64) }`、`Effect { Post(u64), Took(u64), Stalled(u64), Back(u64) }` 与纯转换函数 `step(beat, tick, now_ms)`，命名常量 `BEAT_EVERY_MS = 1_000`、`BEAT_STALLED_MS = 2_000`。同一时刻最多一个在路上的心跳闭包，超时 2 秒仅报一次 `Stalled`，序号不匹配的 `Back` 忽略。
  - `beat::run(post, stats)`：经 `Register<u64>` 非阻塞接收主线程回写，记 `shell.beat_ms` 分布与 `shell.beats.stalled` 计数，发出 `warn!("shell.main_thread.stalled", waited_ms)` 与 `info!("shell.main_thread.back", stalled_ms)`。
  - `app/src/lib.rs`：导出 `pub mod beat;`。
  - `app/src/main.rs`：在 `shell` 树上启动 `shell.beat`（投递 `handle.run_on_main_thread`，只投不等）。
- **界面帧间隔与画到第几帧回报（`ui/src/lib/vitals.ts`、`ui/src/lib/core.svelte.ts`、`app/src/main.rs`、`core/src/app/core.rs`）**：
  - `ui/src/lib/vitals.ts`：纯函数 `frame(v, now, hidden)` 与 `report(v, seq, now)`，命名常量 `FRAME_GAP_MS = 250`、`REPORT_EVERY_MS = 1000`；页面隐藏时清空 `last`，恢复后的第一帧不记间隔；每秒最多报一次，闲时静默，报后清零。
  - `ui/src/lib/core.svelte.ts`：`Bus` 增加可选 `drawn` 方法（`tauri` 经 fire-and-forget `invoke('drawn', { seq, gap_ms, gaps })` 上报，`ui/dev/mock.ts` 不报）；`use` 中启动仅做 O(1) 比较与上报的 `requestAnimationFrame` 循环（不碰 DOM，仅读 `document.hidden`）。
  - `app/src/main.rs`：新增第三个异步命令 `drawn(window, seq, gap_ms, gaps, core, vitals)`，调用 `core.drawn(seq)`，当 `gaps > 0` 时发出 `warn!("ui.frame.gap", window = Id(window.label()), gap_ms, n = gaps)` 并记 `ui.gap_ms` 分布与 `ui.gaps` 计数（句柄在 `setup` 预取进 `UiVitals`，热路径纯原子操作）。
  - `core/src/app/core.rs`：`Core` 持有 `frame: Register<Frame>`，`Core::drawn(seq)` 校验不可信输入，忽略超过当前 `Frame` 序号的值并保持单调，保证 `Screen` 水位绝不超过 `Frame`。
- **事件文档与测试**：
  - `docs/EVENTS.md`：新增 `shell.main_thread.stalled`、`shell.main_thread.back`、`ui.frame.gap`。
  - `app/tests/beat.rs`：新增第 1–4 条测试。
  - `ui/tests/vitals.test.ts`：新增第 5–8 条测试。
  - `core/tests/bench/mod.rs`、`core/tests/liveness.rs`：暴露 `Bench::pushed` / `Bench::drawn` 并新增第 9 条 `Screen` 水位与序号夹紧测试。

## 证据

### 新增测试（全绿）

`app/tests/beat.rs`（4/4 通过）：
1. `the_beat_steps_as_its_table_says`（含 `1999 ms` / `2000 ms` 边界）
2. `a_main_thread_that_answers_is_timed_and_nothing_is_said`
3. `a_main_thread_that_stands_still_is_said_once_and_its_return_says_how_long`
4. `the_beat_ends_when_the_app_is_going`

`ui/tests/vitals.test.ts`（4/4 通过）：
5. `records each gap over FRAME_GAP_MS and keeps the longest`（`[0, 16, 32, 400, 416, 1100]` → `{ seq: 1, gap_ms: 684, gaps: 2 }`）
6. `ignores hidden pages and the first frame after returning from hidden`
7. `reports at most once per second and stays silent when idle`
8. `clears the second’s gap counters after reporting`

`core/tests/liveness.rs`（第 9 条通过）：
9. `the_screen_mark_follows_what_the_interface_says_it_drew`

### O(1) 无数组审查（`ui/src/lib/vitals.ts`）

- `Vitals` 为固定 5 个标量字段（`last`、`worst`、`gaps`、`reported`、`due`），`vitals.ts` 内**没有任何数组或随历史增长的结构**（仅返回函数签名要求的 2 元固定元组 `[Vitals, DrawnReport | null]`）。
- `core.svelte.ts` 的 `requestAnimationFrame` 循环每帧只做常数次数值比较，不碰 DOM、不分配历史列表，单帧开销与录音时长无关（原则 9）。

### 门禁结果

- `cargo fmt --all -- --check`：通过
- `cargo clippy --workspace --all-targets -- -D warnings`：通过
- 全部 8 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`shell.sh`、`spawn.sh`）：全部通过
- `cargo test -p zutalk --tests`：16/16 通过（`beat` 4 + `pump` 9 + `updates` 3）
- `cargo test -p zutalk-core --test liveness the_screen_mark_follows_what_the_interface_says_it_drew`：1/1 通过
- `ui`：`svelte-check` 0 errors / 0 warnings，`vitest` 6/6 文件、34/34 通过
- 完整门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh`：全绿（`rustfmt`、`clippy -D warnings`、全部 8 个规则脚本含 `shell.sh`、`cargo test`：`passed 642, failed 0, ignored 7`、`ui`：`0 errors, 0 warnings`、`34 passed`）

## 留待后续 / 需要负责人（已记入给 P1-26 的清单）

门禁里没有真实主线程和真实浏览器；以下两项需由负责人在真机上核对（将在 P1-26 汇总进总清单）：

1. 录音中把应用主线程停住几秒（如打开系统文件对话框或用调试器暂停几秒），核对 `<数据目录>/logs/zutalk.jsonl` 里出现一条 `shell.main_thread.stalled` 和一条 `shell.main_thread.back`。
2. 录音中把主窗口切到后台再切回前台，核对没有因此多出 `ui.frame.gap`；正常录十分钟并记录 `ui.frame.gap` 条数。
