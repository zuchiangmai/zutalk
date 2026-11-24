状态：完成

# P1-16 一场录音是一个状态机、一棵被守着的树：`app/recording.rs`：报告

## 改了什么

1. **新增 `core/src/app/recording.rs`（导出于 `core/src/app.rs`）**：
   - 将整场录音从开始到封好 tape 的生命周期从 `hub.rs` 迁出，包含 `REALTIME`、`FRAME_MS`（100）、`WATCH_EVERY_MS`（1000）、`AUDIO_FANOUT_BLOCKS`（64）、`LANE_QUEUE_EVENTS`（64）、`TAPE_QUEUE_BLOCKS`（600）、`DRAFT_QUEUE_LINES`（256）、`drafted`、`tick` 与 `seal`（改为 `Tree::blocking("recording.seal", …)`，不再用裸 `spawn_blocking`）。
   - 纯状态机 `Phase`（`Opening`、`Recording`、`Paused`、`Closing(Step)`、`Sealing`、`Ended(&'static str)`）、`Step`（`Capture`、`Tape`、`Lanes`、`Frame`、`Draft`）、`Happened`、`Effect`、`step(phase, ending, happened)` 与 `Phase::code()`（如 `Closing.Tape`）。每次状态转换发 `recording.phase`（`rec`、`from`、`to`、`why`、`ms`；进 `Ended` 带失败代码时用 `error!`）。
   - `recording` 子树挂起全部长任务：`tape`（`keep::start`）、`draft`（`keep::start`）、`lane.0..2`（`recording.spawn(LANE_NAMES[n], …)`）、`frame`（`start_frame` 于 `core/src/app/recording.rs:604` 经 `tree.keep("frame", …)` 挂起，崩了从空的 `Composer` 重开，复用同一 `lane.events` 队列与草稿序号）、`watch`（`recording.spawn("watch", …)`）。
   - 两条队列换成具名有界队列：`queue::fanout(&stats, "audio", AUDIO_FANOUT_BLOCKS)` 与 `queue::bounded(&stats, "lane.events", LANE_QUEUE_EVENTS, Full::Wait)`。
   - `FramePart` 统计 `caption.lag_ms` 分布（新文字到达时用 `Capture` 位置减去文字结束时间）。
   - `Gauges` 水位表（`Capture`、`Tape`、`Draft`、`Frame`、`Sent(n)`、`Heard(n)`、`Final(n)`，以及外壳/界面开始汇报后的 `Shell`、`Screen`）；`watch` 每秒采样发布到 `Outlet.water`，并经独立函数 `write_water(&water, 0)` 发 `debug!("water.mark", …)`（供 P1-17 快照复用）；停止后清空 `Outlet.water`。

2. **更新 `core/src/live/lane.rs`、`core/src/rt/{supervise,water,signal}.rs`、`core/src/app/hub.rs`**：
   - `lane.rs`：`BACKLOG` 改名 `BACKLOG_BLOCKS` 并补理由；音频和事件通道换为 `rt::queue::{FanoutRx, Tx}`；新增 `lane::Gauges`，按采集钟更新 `Sent`/`Heard`/`Final` 及 `because`/`ended`；监督改为 `supervise_counted`，将重启与放弃计入 `part.restarts`、`part.gave_up`。
   - `rt/signal.rs`：新增 `Register::seq()`，使 `Frame` 水位与寄存器序号一致；`rt/water.rs`：新增 `Gauge::clear_ended()`，并在 `sample` 从暂停恢复时刷新 `moved_ms` 避免暂停时长计入停滞；`rt/supervise.rs`：新增 `supervise_counted`。
   - `hub.rs`：删除 `fn listen`、`Composer`、`broadcast`、裸起录音/lane/封 tape；经 `root.spawn("recording", …)` 起录音任务；`Core` 新增 `pushed(seq)`、`drawn(seq)` 写 `Shell`/`Screen` 两个 `Gauge`；停止时清空 `water_reg`。

3. **事件词典与欠账**：
   - `docs/EVENTS.md` 新增 `recording.phase`、`water.mark` 两行。
   - `scripts/build/rules/bounded.sh` 删掉 `core/src/app/hub.rs` 一行；`scripts/build/rules/spawn.sh` 将 `core/src/app/hub.rs` 欠账从 14 降到 11。

改动文件：`core/src/app.rs`、`core/src/app/recording.rs`、`core/src/app/hub.rs`、`core/src/live/lane.rs`、`core/src/rt/signal.rs`、`core/src/rt/supervise.rs`、`core/src/rt/water.rs`、`docs/EVENTS.md`、`scripts/build/rules/bounded.sh`、`scripts/build/rules/spawn.sh`、`core/tests/recording.rs`、`core/tests/live.rs`、`core/tests/liveness.rs`、`core/tests/bench/mod.rs`、本报告。

## 证据

### 新增与更新的测试（全部通过）

- `core/tests/recording.rs`（1 passed）：
  1. `a_recording_steps_as_its_table_says`（全组合转换表 + 全部 `Phase::code()` 过 `is_plain`）
- `core/tests/live.rs`（duplex 子集 4 passed）：
  2. `a_lane_reports_what_it_sent_what_was_heard_and_what_is_final`
- `core/tests/liveness.rs`（24 passed，3 ignored，不含耗时 8h 与联网 `i2_background`）：
  3. `every_part_reports_how_far_it_has_got`
  4. `paused_the_water_says_so_and_nothing_moves`
  5. `the_water_comes_once_a_second_and_ends_with_the_recording`
  6. `a_stop_keeps_everything_said_and_recorded`
  7. `a_recording_whose_capture_ends_is_closed_the_same_way`
  8. `a_recording_without_a_key_has_no_lanes_and_stops_the_same_way`（说明：无钥匙场没有 `lane.*` 的 `part.state`，且所有受监督部件无 `Waiting`/重启；`tape`/`draft`/`frame` 经 `Tree::keep` 启动与正常收尾会照常记录 `Opening -> Running -> Ended`）
  9. `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed`（放开 `part.restarts == 1` 断言并通过）

### 帧任务挂树位置

- `core/src/app/recording.rs:604`：`start_frame` 调用 `tree.keep("frame", …)`，走与 `core/tests/supervise.rs` 中 `a_part_that_panics_is_opened_again`、`a_kept_part_is_a_task_of_the_tree_and_its_restarts_are_counted` 相同的路径。

### 录到第 30 秒时的 `Water`（JSON，来自测试 #1）

```json
{"now_ms":30000,"clock_ms":29850,"paused":false,"marks":[{"stage":"Capture","at":29900,"moved_ms":30000,"why":"","ended":null},{"stage":"Tape","at":29900,"moved_ms":30000,"why":"","ended":null},{"stage":"Draft","at":28580,"moved_ms":30000,"why":"","ended":null},{"stage":"Frame","at":112,"moved_ms":30000,"why":"","ended":null},{"stage":{"Sent":0},"at":29900,"moved_ms":30000,"why":"","ended":null},{"stage":{"Heard":0},"at":29700,"moved_ms":30000,"why":"","ended":null},{"stage":{"Final":0},"at":28580,"moved_ms":30000,"why":"","ended":null}],"health":[]}
```

### 一次停止的全部 `recording.phase` 行（来自测试 #4）

```text
info recording.phase rec="1a20ed562251a71cebc" from="Opening" to="Recording" why="" ms=0
info recording.phase rec="1a20ed562251a71cebc" from="Recording" to="Closing.Capture" why="" ms=12240
info recording.phase rec="1a20ed562251a71cebc" from="Closing.Capture" to="Closing.Tape" why="" ms=0
info recording.phase rec="1a20ed562251a71cebc" from="Closing.Tape" to="Closing.Lanes" why="" ms=0
info recording.phase rec="1a20ed562251a71cebc" from="Closing.Lanes" to="Closing.Frame" why="" ms=5000
info recording.phase rec="1a20ed562251a71cebc" from="Closing.Frame" to="Closing.Draft" why="" ms=0
info recording.phase rec="1a20ed562251a71cebc" from="Closing.Draft" to="Sealing" why="" ms=0
info recording.phase rec="1a20ed562251a71cebc" from="Sealing" to="Ended" why="done" ms=0
```

### 验证结果

- `cargo fmt --all -- --check`、`RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部通过。
- `git grep -n "tokio::spawn\|spawn_blocking\|mpsc::channel\|broadcast::channel" core/src/app/recording.rs core/src/live core/src/rt/keep.rs`：无结果。
- `grep -n 'fn listen\|Composer\|broadcast' core/src/app/hub.rs`：无结果。
- `ui`：`svelte-check` 0 errors 0 warnings；`vitest` 5 files / 29 tests passed。
- 注：耗时 ~2 分钟的 `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`（含 `caption.lag_ms` p50/p99）按本批执行策略在 P1-18 收尾统一运行一次，实测数据见 `P1-18-reconnect.md`（并同步补充）。

## 留下的，要负责人的

- `judge`、停滞快照与踢 lane 留给 P1-17；取钥匙时限与 `Command::Reconnect` 留给 P1-18；整场转写与后台任务留给 P1-19；外壳与界面上报 `pushed`/`drawn` 留给 P1-23/P1-24。
