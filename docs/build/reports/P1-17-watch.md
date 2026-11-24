状态：完成

# P1-17 停滞被看见、被处理：看守照 `judge` 行动，留下判断和现场：报告

## 改了什么

1. **看守接入 `judge`、`due` 与停滞快照（`core/src/app/recording.rs`、`core/src/app/hub.rs`）**：
   - `watch_task` 在 `t = 0` 采样初值，之后每 `WATCH_EVERY_MS`（1000 ms）执行 `sample` → `judge(&next, BUDGETS)` → `due(&before, &mut next, &verdicts)`，将带 `health` 的 `Water` 写进 `Outlet.water`，并把每栏健康（第 `j` 栏取第 `max(j, 1) - 1` 条 lane 的 `Sent`/`Heard` 较差者）经 `watch::Sender<Vec<Health>>` 送给帧任务（`Composer::judged`）。
   - 执行 `due` 的三类输出：
     - `Due::Changed`：发 `judge.verdict`（`stage`、`lane`、`from`、`to`、`why`、`action`；`Live` 用 `info!`，`Slow`/`Stalled` 用 `warn!`，`Dead` 用 `error!`），递增计数 `judge.stalled` / `judge.dead`。
     - `Due::Snapshot`：从 `Hub` 持有的跨场单调计数器 `next_snap`（自 1 起）取号，经 `write_snapshot` 写出整组现场：`stall.snapshot` → `write_water(&next, snap)`（`warn!("water.mark", snap, …)`）→ `stats::write(&tally, snap)` → `root.running()` 每行 `warn!("task.running", snap, tree, task, kind, for_ms)` → `stall.snapshot.end`（带精确总行数 `lines`）。
     - `Due::Action`：`Reopen(Heard(n))` 踢对应 lane 并记录 `warn!("lane.kick", lane, why = "silent")` 及 `lane.kicks += 1`；`Tell(code)` 调 `said.say(code)`；`End(code)` 触发录音状态机 `Happened::Failed(code)` 走有序收尾并保留已落盘内容。

2. **单一健康来源与状态句（`core/src/live/frame.rs`、`core/src/rt/water.rs`）**：
   - 删除 `frame.rs` 中 `Link -> Health` 的映射表 `link_health`；`Composer` 的 `Column.health` 仅来自 `Composer::judged`。
   - `status(paused, columns, links, quiet)` 接入各栏健康，按规则表裁定：`Paused` > 无栏或全栏 `Stalled`/`Dead`（`AudioOnly`） > 有 `Connecting` > 有 `Slow`（`Behind`） > `Recording`。
   - `rt/water.rs` 新增 `Health::rank`、`Health::worst` 与 `verdict_why`。

3. **踢 lane（`core/src/live/lane.rs`）**：
   - `lane::run` 增加 `kick: watch::Receiver<u64>`；`Conn::live` 打开连接时忽略旧踢信号，在 `select!` 中响应新踢信号并以 `End::Retry("silent")` 结束连接，照常退避重连并报 `silent` 空缺。

4. **事件词典（`docs/EVENTS.md`）**：
   - 新增 `judge.verdict`、`stall.snapshot`、`stall.snapshot.end`、`task.running`、`lane.kick` 五行，并补写「怎么读一组快照」（`grep '"snap":1'`）。

改动文件：`core/src/rt/water.rs`、`core/src/live/lane.rs`、`core/src/live/frame.rs`、`core/src/app/hub.rs`、`core/src/app/recording.rs`、`docs/EVENTS.md`、`core/tests/frame.rs`、`core/tests/live.rs`、`core/tests/liveness.rs`、`core/tests/bench/mod.rs`、`core/tests/bench/service.rs`、本报告。

## 证据

### 新增与更新的测试（全部通过）

- `core/tests/frame.rs`（9 passed）：
  1. `a_columns_health_comes_only_from_judge`
  2. `the_status_line_says_what_most_needs_saying`（覆盖 `status(paused, columns, links, quiet)` 全部规则行）
- `core/tests/live.rs`（duplex 子集通过）：
  3. `a_kick_ends_the_connection_and_the_lane_opens_another`
- `core/tests/liveness.rs`（29 passed，0 failed，2 ignored，仓库内不再有任何 `ignore = "P1-17"`）：
  4. `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection`（合并原 `_and_it_shows`，收紧至 12 虚拟秒内换连，断言 `judge.verdict`、`stall.snapshot` 51 行、1 `lane.kick`、`part.state(why = "silent")`、`judge.stalled == 1`、`lane.kicks == 1`、栏健康 `Slow -> Stalled(silent) -> Live`）
  5. `i7_a_capture_that_stops_delivering_ends_the_recording_out_loud`
  6. `a_service_that_lags_is_behind_not_dead`
  7. `nothing_is_done_twice_for_one_stall`
  8. `a_recording_with_nothing_wrong_leaves_no_verdict_behind`
  9. `a_recording_without_a_key_is_quiet_from_start_to_stop`
  10. `nothing_said_or_secret_reaches_the_log`（含 lane 崩溃与 `judge.verdict` 新事件，隐私测试照旧通过）

### 测试 #1 快照的全部 51 行（`snap = 1`）

```text
warn  zutalk_core::app::recording  stall.snapshot       snap=1 rec="1a20ee17caf24f61412" stage="Heard" lane=0 health="Stalled" why="silent" idle_ms=10000
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Capture" lane=-1 at=9900 moved_ms=10000 why="" ended=""
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Tape" lane=-1 at=9900 moved_ms=10000 why="" ended=""
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Draft" lane=-1 at=0 moved_ms=0 why="" ended=""
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Frame" lane=-1 at=4 moved_ms=5000 why="" ended=""
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Sent" lane=0 at=9900 moved_ms=10000 why="" ended=""
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Heard" lane=0 at=0 moved_ms=0 why="" ended=""
warn  zutalk_core::app::recording  water.mark           snap=1 stage="Final" lane=0 at=0 moved_ms=0 why="" ended=""
info  zutalk_core::rt::stats       stat.count           snap=1 name="commands.done" n=2
info  zutalk_core::rt::stats       stat.count           snap=1 name="commands.refused" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="draft.lines" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="draft.lost_lines" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="frame.cut" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="frame.made" n=4
info  zutalk_core::rt::stats       stat.count           snap=1 name="hub.turns" n=2
info  zutalk_core::rt::stats       stat.count           snap=1 name="hub.turns.slow" n=1
info  zutalk_core::rt::stats       stat.count           snap=1 name="judge.dead" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="judge.stalled" n=1
info  zutalk_core::rt::stats       stat.count           snap=1 name="keep.lost_batches" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="lane.gaps" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="lane.kicks" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="log.lost" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="log.refused" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="notices.said" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="part.gave_up" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="part.restarts" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="state.sent" n=3
info  zutalk_core::rt::stats       stat.count           snap=1 name="tape.blocks" n=99
info  zutalk_core::rt::stats       stat.count           snap=1 name="tape.padded_blocks" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="task.panics" n=0
info  zutalk_core::rt::stats       stat.count           snap=1 name="task.started" n=8
info  zutalk_core::rt::stats       stat.peak            snap=1 name="frame.bound_bytes" now=308 most=308
info  zutalk_core::rt::stats       stat.peak            snap=1 name="tree.tasks" now=8 most=10
info  zutalk_core::rt::stats       stat.spread          snap=1 name="caption.lag_ms" n=0 p50=0 p99=0 most=0
info  zutalk_core::rt::stats       stat.spread          snap=1 name="draft.batch_ms" n=0 p50=0 p99=0 most=0
info  zutalk_core::rt::stats       stat.spread          snap=1 name="hub.turn_us" n=2 p50=500 p99=10810 most=10810
info  zutalk_core::rt::stats       stat.spread          snap=1 name="tape.batch_ms" n=99 p50=1 p99=1 most=0
info  zutalk_core::rt::stats       stat.queue           snap=1 name="audio" capacity=64 depth=0 deepest=1 sent=99 full=0 lost=0
info  zutalk_core::rt::stats       stat.queue           snap=1 name="draft" capacity=256 depth=0 deepest=0 sent=0 full=0 lost=0
info  zutalk_core::rt::stats       stat.queue           snap=1 name="hub.mail" capacity=256 depth=0 deepest=1 sent=2 full=0 lost=0
info  zutalk_core::rt::stats       stat.queue           snap=1 name="lane.events" capacity=64 depth=0 deepest=1 sent=2 full=0 lost=0
info  zutalk_core::rt::stats       stat.queue           snap=1 name="tape" capacity=600 depth=0 deepest=1 sent=99 full=0 lost=0
warn  zutalk_core::app::recording  task.running         snap=1 tree="core" task="hub" kind="task" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="core" task="stats" kind="task" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="core" task="recording" kind="task" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="recording" task="tape" kind="kept" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="recording" task="draft" kind="kept" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="recording" task="lane.0" kind="task" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="recording" task="frame" kind="kept" for_ms=10000
warn  zutalk_core::app::recording  task.running         snap=1 tree="recording" task="watch" kind="task" for_ms=10000
warn  zutalk_core::app::recording  stall.snapshot.end   snap=1 lines=51
```

### 测试 #2 `Stalled` 那一秒的 `Water`（JSON）

```json
{"now_ms":16000,"clock_ms":15850,"paused":false,"marks":[{"stage":"Capture","at":5000,"moved_ms":6000,"why":"","ended":null},{"stage":"Tape","at":5000,"moved_ms":6000,"why":"","ended":null},{"stage":"Draft","at":3780,"moved_ms":5000,"why":"","ended":null},{"stage":"Frame","at":15,"moved_ms":6000,"why":"","ended":null},{"stage":{"Sent":0},"at":5000,"moved_ms":6000,"why":"","ended":null},{"stage":{"Heard":0},"at":5000,"moved_ms":7000,"why":"","ended":null},{"stage":{"Final":0},"at":3780,"moved_ms":5000,"why":"","ended":null}],"health":[["Capture",{"is":"Stalled","why":"audio.stalled"}],["Tape",{"is":"Live"}],["Draft",{"is":"Live"}],["Frame",{"is":"Live"}],[{"Sent":0},{"is":"Live"}],[{"Heard":0},{"is":"Live"}],[{"Final":0},{"is":"Live"}]]}
```

### 验证结果

- `cargo fmt --all -- --check`、`RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 6 个规则脚本：通过。
- `git grep -n 'ignore = "P1-17' core/`：无结果。
- `ui`：`svelte-check` 0 errors 0 warnings；`vitest` 5 files / 29 tests passed。
- 注：耗时 ~2 分钟的 `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`（含 `judge.verdict` 条数与快照组数）按本批执行策略在 P1-18 收尾统一运行一次，实测数据见 `P1-18-reconnect.md`。

## 留下的，要负责人的

- 取钥匙时限、`reqwest` 超时、重启上限后的 `Dead` 与 `Command::Reconnect` 留给 P1-18；预算表数字均为临时值（`P1.md` §4 第 3 条）；`Shell`/`Screen` 由 P1-23/P1-24 上报。
