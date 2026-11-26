状态：完成

# P1-25 核对：显式与可观测（E10）：报告

## 改了什么

本件不加功能，只补齐核对中发现的测试覆盖缺口、三场故障日志序列守护、事件词典整理及 `P1.md` 三张表对齐：

1. **补齐 4 个缺少测试断言的事件覆盖**（其余 60 个已有测试断言，另 2 个为二进制 `main()` 入口事件）：
   - `server/src/lib.rs`、`server/tests/log.rs`：将 `debug!("server.sweep", ms = …)` 移入 `Server::sweep` 内（`serve()` 仍每分钟调一次，行为不变），补测试 `a_sweep_is_noted_at_debug`（先看它失败，再补实现通过）。
   - `core/tests/stats.rs`：补 `a_peak_that_moved_is_written_in_the_summary` 断言每分钟汇总里的 `stat.peak`。
   - `app/src/beat.rs`、`app/src/main.rs`、`app/tests/beat.rs`：抽出薄函数 `beat::note_frame_gap`（`drawn` 直接调用），补测试 `a_frame_gap_from_the_ui_is_logged_and_counted` 断言 `ui.frame.gap` 与 `ui.gap_ms`/`ui.gaps`。
   - `app/src/updates.rs`、`app/tests/updates.rs`：抽出薄枚举与函数 `updates::Look`、`updates::note_look`（`updates::run` 直接调用），补测试 `looking_for_an_update_says_what_it_found` 断言 `update.look`（`none` / `ready` / `update`）。
2. **`core/tests/liveness.rs` 补 5 条 E10 核对测试与代价断言**：
   - `every_event_in_the_catalogue_can_be_told_apart`：读 `docs/EVENTS.md` 断言表内无重复行、每条代码均通过 `zutalk_log::is_code`。
   - 三场故障真实文件日志序列测试（`zutalk_log::files`）：`log_tells_the_story_of_a_silent_service`、`log_tells_the_story_of_going_offline_and_reconnecting`、`log_tells_the_story_of_a_tape_writer_panic`。
   - `two_minute_recording_tally_matches_the_table`：2 虚拟分钟录音（含 lane 崩溃、搜索、导出）核对 `tally()` 全部名字均在 `P1.md` §3.13.4 表内。
   - `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` 增补第 1 小时与第 8 小时 `Tree::running()` 行数相等断言及每小时事件分布输出；`nothing_said_or_secret_reaches_the_log` 输出实测覆盖事件代码列表。
3. **`docs/EVENTS.md`**：按 8 个部件重排全部 66 个事件；补齐开头四段（「怎么读一行」「哪些东西永远不在日志里」「怎么读一组快照」「自检包里有什么」）；逐项列出 `job.*` 的 `kind`（33 个）、`part.state` 的 `part`（7 个）、`queue.*` 的 `queue`（13 个）、`boot.step` 的 `step`（4 个）；末尾新增「出了事先看什么」排查表。
4. **`docs/build/phases/P1.md`**：仅对齐 §3.13.3、§3.13.4、§3.13.11 三张表（改动清单见下文），其余各节未动。

---

## E10 逐项核对

### 1. 欠账表

`bounded.sh` 与 `spawn.sh` 在 `core/src`、`app/src` 的欠账均已清零；`later` 与 `layers.sh` 欠账原样列出：

**`scripts/build/rules/bounded.sh`（全文片段）**：

```sh
# Debt: file, count, who pays it.
debt='
'

# Server uses checked in the phase that touches the server (P1.md §3.13.12).
later='
server/src/room.rs 1 P5
'
```

**`scripts/build/rules/spawn.sh`（全文片段）**：

```sh
# Debt: file, count, who pays it.
debt='
'

# Server uses checked in the phase that touches the server (P1.md §3.13.12).
later='
server/src/ledger.rs 2 P4
server/src/lib.rs 1 P5
'
```

**`scripts/build/rules/layers.sh`（四处欠账全文）**：

```sh
debt='
record/retain.rs crate::app::proto 1 P2
record/migrate.rs crate::app::proto 1 P2
out/city.rs crate::app::proto 1 P5
out/room.rs crate::app::proto 1 P5
'
```

### 2. 事件（共 66 个，代码与 `P1.md` §3.13.3、`docs/EVENTS.md` 零差异）

代码提取结果共 66 个事件，`P1.md` §3.13.3 表共 66 个，**表里有代码没有：0；代码里有表里没有：0**。其中 64 个由自动化测试直接断言，仅 2 个为二进制 `main()` 入口事件（编译期 `is_code` + `Plain` 检查、`events.sh` 双向检查，`app.start` 列入 P1-26 负责人首次真机启动核对）：

| 事件 | 看见它的测试 |
|---|---|
| `app.panic` | `log/tests/panic.rs::a_panic_is_said_with_its_thread_and_place_and_never_its_message` |
| `app.start` | `app/src/main.rs::main` 入口事件（`events.sh` + 编译期校验；真机首次启动见 P1-26 负责人清单） |
| `boot.step` | `core/tests/app.rs::a_command_before_the_core_is_ready_is_turned_away` |
| `capture.end` | `core/tests/audio.rs::a_closed_lid_ends_the_capture_out_loud` |
| `capture.pad` | `core/tests/audio.rs::a_source_that_dies_is_reported_padded_and_opened_again` |
| `capture.pause` | `core/tests/audio.rs::paused_the_clock_stands_still_and_resumed_it_goes_on` |
| `capture.ring.lost` | `core/tests/audio.rs::samples_the_ring_could_not_take_are_counted_and_said` |
| `capture.source.broke` | `core/tests/audio.rs::a_source_that_dies_is_reported_padded_and_opened_again` |
| `capture.source.ended` | `core/tests/audio.rs::a_source_that_cannot_be_opened_is_fatal_and_the_other_one_carries_on` |
| `capture.source.open` | `core/tests/audio.rs::a_source_that_dies_is_reported_padded_and_opened_again` |
| `command.done` | `core/tests/app.rs::every_command_leaves_its_kind_its_answer_and_nothing_it_carried` |
| `command.refused` | `core/tests/app.rs::a_hub_that_cannot_keep_up_refuses_commands_and_says_busy` |
| `diagnose.wrote` | `core/tests/liveness.rs::a_diagnosis_says_how_things_are` |
| `draft.gap` | `core/tests/liveness.rs::lines_the_draft_could_not_take_become_one_gap_line` |
| `frame.fit` | `core/tests/liveness.rs::i1_three_languages_fit_a_frame` |
| `hub.boot` | `core/tests/app.rs::a_command_before_the_core_is_ready_is_turned_away` |
| `hub.turn.slow` | `core/tests/liveness.rs::five_minute_recording_with_twenty_commands_measures_hub_turns` |
| `identity.who` | `core/tests/app.rs::a_recording_needs_nobody_signed_in` |
| `job.again` | `core/tests/retain.rs::one_that_waits_to_try_again_does_not_hold_up_the_next` |
| `job.done` | `core/tests/app.rs::an_export_writes_what_was_changed_just_before_it` |
| `job.start` | `core/tests/liveness.rs::i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` |
| `judge.verdict` | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` |
| `keep.lost` | `core/tests/keep.rs::a_writer_that_breaks_is_opened_again` |
| `lane.gap` | `core/tests/live.rs::a_gap_is_an_event_with_its_edges_and_its_reason` |
| `lane.kick` | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` |
| `lane.link` | `core/tests/live.rs::every_change_of_link_is_said_once` |
| `lane.open` | `core/tests/live.rs::every_open_says_how_it_went` |
| `lane.reconnect` | `core/tests/liveness.rs::i7_offline_is_said_and_captions_stop_trying_until_asked` |
| `log.lost` | `log/tests/files.rs::a_writer_that_stands_still_loses_lines_and_says_how_many` |
| `log.turned` | `log/tests/files.rs::files_turn_at_their_size_and_the_oldest_goes` |
| `notice.said` | `core/tests/app.rs::more_codes_than_are_kept_lose_the_oldest` |
| `part.gave_up` | `core/tests/keep.rs::a_writer_that_never_opens_gives_up_and_says_why` |
| `part.state` | `core/tests/liveness.rs::i6_a_lane_that_dies_comes_back_and_marks_what_it_missed` |
| `queue.clear` | `core/tests/queue.rs::a_full_queue_hands_back_says_so_once_and_counts` |
| `queue.full` | `core/tests/queue.rs::a_full_queue_hands_back_says_so_once_and_counts` |
| `recording.end` | `core/tests/liveness.rs::a_recording_says_when_it_starts_how_it_goes_and_when_it_ends` |
| `recording.phase` | `core/tests/liveness.rs::a_stop_keeps_everything_said_and_recorded` |
| `recording.start` | `core/tests/liveness.rs::a_recording_says_when_it_starts_how_it_goes_and_when_it_ends` |
| `server.request` | `server/tests/log.rs::a_request_leaves_its_route_its_status_and_no_more` |
| `server.start` | `server/src/main.rs::main` 入口事件（`events.sh` + 编译期校验；部署启动第一行） |
| `server.sweep` | `server/tests/log.rs::a_sweep_is_noted_at_debug`（本件补） |
| `session.status` | `core/tests/liveness.rs::a_recording_says_when_it_starts_how_it_goes_and_when_it_ends` |
| `shell.captions` | `app/tests/pump.rs::the_captions_window_follows_the_state_and_is_never_asked` |
| `shell.join` | `app/tests/pump.rs::a_window_that_joins_late_gets_the_state_and_every_signals_latest` |
| `shell.main_thread.back` | `app/tests/beat.rs::a_main_thread_that_stands_still_is_said_once_and_its_return_says_how_long` |
| `shell.main_thread.stalled` | `app/tests/beat.rs::a_main_thread_that_stands_still_is_said_once_and_its_return_says_how_long` |
| `shell.sink.gone` | `app/tests/pump.rs::a_channel_that_panics_does_not_end_the_pump` |
| `stall.snapshot` | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` |
| `stall.snapshot.end` | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` |
| `stat.count` | `core/tests/stats.rs::once_a_minute_what_moved_is_written` |
| `stat.peak` | `core/tests/stats.rs::a_peak_that_moved_is_written_in_the_summary`（本件补） |
| `stat.queue` | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` |
| `stat.spread` | `core/tests/stats.rs::once_a_minute_what_moved_is_written` |
| `stats.full` | `core/tests/stats.rs::more_rows_than_there_is_room_for_are_said_once_and_left_out` |
| `stats.summary` | `core/tests/stats.rs::once_a_minute_what_moved_is_written` |
| `tape.pad` | `core/tests/liveness.rs::i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` |
| `task.end` | `core/tests/liveness.rs::a_stop_keeps_everything_said_and_recorded` |
| `task.refused` | `core/tests/supervise.rs::nothing_starts_on_a_tree_that_has_closed` |
| `task.running` | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` |
| `task.start` | `core/tests/app.rs::the_core_runs_on_a_tree_with_a_name` |
| `thread.end` | `core/tests/supervise.rs::a_named_thread_says_when_it_starts_and_ends` |
| `thread.start` | `core/tests/supervise.rs::a_named_thread_says_when_it_starts_and_ends` |
| `ui.frame.gap` | `app/tests/beat.rs::a_frame_gap_from_the_ui_is_logged_and_counted`（本件补） |
| `update.look` | `app/tests/updates.rs::looking_for_an_update_says_what_it_found`（本件补） |
| `water.mark` | `core/tests/liveness.rs::the_water_comes_once_a_second_and_ends_with_the_recording` |
| `write.skipped` | `core/tests/app.rs::without_a_key_sound_is_kept_and_the_status_says_why` |

### 3. 统计（代码与 `P1.md` §3.13.4 核对）

全仓静态提取：计数 35 个、高水位 2 个、分布 10 个、队列 13 个，与 `P1.md` §3.13.4 完全一致（唯一增量是 P1-23 外壳更新通道 `shell.updates`，已补进 §3.13.4 队列行说明）。

2 虚拟分钟录音（含 30 s 处 lane 崩溃、搜索、导出，`two_minute_recording_tally_matches_the_table`）实际 `tally()` 名字对照：

| 类别 | 2 分钟台架实测出现的名字 | 表中其余未在本场触发的名字（及原因） | 差异 |
|---|---|---|---|
| 计数（35） | 27 个：`commands.done` `commands.refused` `draft.lines` `draft.lost_lines` `frame.cut` `frame.made` `hub.turns` `hub.turns.slow` `jobs.done` `jobs.failed` `judge.dead` `judge.stalled` `keep.lost_batches` `lane.gaps` `lane.kicks` `log.lost` `log.refused` `notices.said` `part.gave_up` `part.restarts` `refresh.merged` `search.stale` `state.sent` `tape.blocks` `tape.padded_blocks` `task.panics` `task.started` | 8 个：真实采集 4 个（`capture.blocks` `capture.padded_ms` `capture.ring.lost_samples` `capture.reopens`，台架用假话筒）、外壳 4 个（`shell.pushed` `shell.sinks.gone` `shell.beats.stalled` `ui.gaps`，外壳层登记） | 0 缺 0 多 |
| 高水位（2） | 2 个：`frame.bound_bytes` `tree.tasks` | 无 | 0 缺 0 多 |
| 分布（10） | 8 个：`caption.lag_ms` `desk.job_ms` `desk.wait_ms` `draft.batch_ms` `hub.turn_us` `lane.dial_ms` `lane.keys_ms` `tape.batch_ms` | 2 个：外壳层 `shell.beat_ms`、`ui.gap_ms` | 0 缺 0 多 |
| 队列（13） | 6 个：`audio` `desk` `draft` `hub.mail` `lane.events` `tape` | 7 个：真实采集 3 个（`capture.pump` `capture.ended` `capture.file`）、整场转写/交城 2 个（`write.wav` `city.audio`）、外壳 2 个（`shell.joins` `shell.updates`） | 0 缺 0 多 |

### 4. 状态机（`P1.md` §3.13.11 全部 10 个）

| # | 状态机 | 文件路径 | 转换表测试 |
|---|---|---|---|
| 1 | `Who` | `core/src/id/keys.rs` | `core/tests/keys.rs::who_signs_is_the_person_else_the_device_else_nobody` |
| 2 | `Watch` | `core/src/rt/supervise.rs` | `core/tests/supervise.rs::the_watch_steps_as_its_table_says` |
| 3 | `Health` | `core/src/rt/water.rs` | `core/tests/water.rs::each_budget_at_its_edges`、`every_change_of_health_is_due_once` |
| 4 | `Link` | `core/src/live/lane.rs` | `core/tests/live.rs::a_link_changes_only_as_its_table_says` |
| 5 | `Fit` | `core/src/live/frame.rs` | `core/tests/frame.rs::what_gives_way_first_is_the_oldest_line_of_the_fullest_column` |
| 6 | `Status` | `core/src/live/frame.rs` | `core/tests/frame.rs::the_status_line_says_what_most_needs_saying` |
| 7 | `Phase` | `core/src/app/recording.rs` | `core/tests/recording.rs::a_recording_steps_as_its_table_says` |
| 8 | `Boot` | `core/src/app/hub.rs` | `core/tests/app.rs::the_hub_boots_as_its_table_says` |
| 9 | `Fan` | `app/src/pump.rs` | `app/tests/pump.rs::every_interleaving_ends_with_each_window_holding_the_latest` |
| 10 | `Beat` | `app/src/beat.rs` | `app/tests/beat.rs::the_beat_steps_as_its_table_says` |

### 5. 「看得见」（`P1.md` §2 第二张表逐行核对，全部 15 行成立）

| # | 表内场景 | 实际测试名 | 已断言的事件、判断、提示与计数 |
|---|---|---|---|
| 1 | lane 崩了自愈 | `core/tests/liveness.rs::i6_a_lane_that_dies_comes_back_and_marks_what_it_missed` | `part.state(lane.0, Waiting, panic)` ×1、`lane.link` 回 `Live`、`lane.gap(panic)` ×1、`part.restarts == 1` |
| 2 | tape 写入口崩了自愈 | `core/tests/liveness.rs::i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` | `part.state(tape, Waiting, panic)`、`keep.lost(tape)`、`tape.pad`、`keep.lost_batches == 1`、`recording.end(done)` |
| 3 | tape 堵住与溢满 | `core/tests/liveness.rs::i2_a_tape_that_stands_still_holds_no_caption_back` | 前半：`queue.full(tape) == 0`、`tape.deepest > 0`；后半：`queue.full(tape, end) == 1`、`recording.end(tape) == 1`、提示 `tape` |
| 4 | 导出永不返回 | `core/tests/liveness.rs::i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` | 堵着：`job.start(export.write)` 无对应 `job.done`、`Tree::running()` 含 `export.write`、`command.done(Pause/Resume/Change, ok)`；放开：`job.done(export.write, ok)` |
| 5 | 后台并发 | `core/tests/liveness.rs::i2_background_work_runs_beside_a_recording` | `job.done(search/move/write, ok)`、全程无 `judge.verdict(to = Stalled)` |
| 6 | 服务收音不回话 | `core/tests/liveness.rs::i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` | `judge.verdict(Heard, 0)` 依次 `Slow`/`Stalled(silent, reopen)`/`Live`、`stall.snapshot` ×1、`lane.kick` ×1、`part.state(lane.0, silent)` ×1、栏健康 `Slow -> Stalled(silent) -> Live`、`judge.stalled == 1`、`lane.kicks == 1` |
| 7 | 采集停流 | `core/tests/liveness.rs::i7_a_capture_that_stops_delivering_ends_the_recording_out_loud` | `judge.verdict(Capture, Stalled, audio.stalled, end)`、`stall.snapshot` ×1、`recording.end(audio.stalled)`、提示 `audio.stalled` |
| 8 | 断网到放弃后重连 | `core/tests/liveness.rs::i7_offline_is_said_and_captions_stop_trying_until_asked` | `lane.open(connect)` ×8、`part.gave_up(lane.0, connect)` ×1、`judge.verdict(Sent)` `Slow -> Stalled -> Dead -> Live`、`stall.snapshot` ×1、`connect` 提示 ×1、`command.done(Reconnect, ok)`、`lane.reconnect`、`lane.link(Live)`、`judge.dead == 1` |
| 9 | 取钥匙超时放弃 | `core/tests/live.rs::a_lease_that_never_comes_is_given_up_on` | `lane.open(keys)` ≤ 12、`part.gave_up(keys)`、`gauges.sent` `because == "keys"` 且 `ended == Some("keys")` |
| 10 | 采集故障 4 种 | `core/tests/audio.rs`：`a_source_that_dies_is_reported_padded_and_opened_again`、`a_stall_the_pump_hears_ends_the_device_on_its_own_thread`、`a_source_that_cannot_be_opened_is_fatal_and_the_other_one_carries_on`、`a_closed_lid_ends_the_capture_out_loud` | `capture.source.broke`、`capture.source.open`、`capture.source.ended`、`capture.end`、`capture.pad` 及原有 `Notice` |
| 11 | 落盘部件故障 | `core/tests/keep.rs`：`a_writer_that_breaks_is_opened_again`、`a_writer_that_never_opens_gives_up_and_says_why`、`a_full_queue_says_so_at_once` | `part.state`、`keep.lost`、`part.gave_up`、`queue.full`、`keep.lost_batches`、调用者拿到原因 |
| 12 | hub 邮箱与 desk 满 | `core/tests/app.rs::a_hub_that_cannot_keep_up_refuses_commands_and_says_busy`、`a_desk_that_is_full_turns_commands_away_out_loud` | `queue.full(hub.mail/desk, reject)`、`command.refused(busy)` / `command.done(Change, busy)`、`busy` 返回与提示、对应队列 `full` 计数等于被拒次数 |
| 13 | 草稿队列满 | `core/tests/liveness.rs::lines_the_draft_could_not_take_become_one_gap_line` | `queue.full(draft, drop)` ×1、`draft.gap` ×1、`notice.said(draft.behind)` ×1、`draft.lost_lines == gap_lines` |
| 14 | 窗口通道 panic | `app/tests/pump.rs::a_channel_that_panics_does_not_end_the_pump` | `shell.sink.gone(window = "bad", why = "panic")` ×1、`shell.sinks.gone == 1`、健康窗口照收 |
| 15 | 主线程停滞 | `app/tests/beat.rs::a_main_thread_that_stands_still_is_said_once_and_its_return_says_how_long` | `shell.main_thread.stalled` ×1、`shell.main_thread.back` ×1、`shell.beats.stalled == 1` |

### 6. 隐私核对

- `core/tests/liveness.rs::nothing_said_or_secret_reaches_the_log` 与 `a_diagnosis_holds_nothing_that_was_said_and_no_secret` 均通过（落盘文件、自检包 0 泄漏、0 退回字段 `"!"`）。
- `nothing_said_or_secret_reaches_the_log` 落盘日志实测覆盖 **23 个**事件代码：`boot.step`、`command.done`、`hub.boot`、`identity.who`、`job.done`、`job.start`、`lane.gap`、`lane.link`、`lane.open`、`notice.said`、`part.state`、`recording.end`、`recording.phase`、`recording.start`、`session.status`、`stat.count`、`stat.peak`、`stat.queue`、`stat.spread`、`stats.summary`、`task.end`、`task.start`、`water.mark`。
- 其余 43 个未在该 2 分钟正常台架录音中触发的事件：均由编译期 `Plain` 守住——其中 37 个事件根本没有 `Id` 字段（全部字段仅为数字、布尔或源码 `&'static str` 常量），剩余 6 个含 `Id` 的事件（`stall.snapshot`、`shell.join`、`shell.sink.gone`、`ui.frame.gap` 及 `app.panic` 源码路径）也只接受录音 id、窗口标签或源码路径，绝不接受对话内容、人名、标题、搜索词或密钥。

**`git grep -n "Id(" core/src app/src server/src` 逐处核对（共 12 处，无一处是人写的字）**：

| # | 位置 | 事件与字段 | 传进去的是什么 |
|---|---|---|---|
| 1 | `app/src/beat.rs:132` | `ui.frame.gap` `window = Id(window)` | Tauri `window.label()`（配置写死的窗口标签 `"main"` / `"captions"`） |
| 2 | `app/src/pump.rs:199` | `shell.sink.gone` `window = Id(&window)` | 订阅表中的窗口标签（同上） |
| 3 | `app/src/pump.rs:333` | `shell.join` `window = Id(&window)` | `subscribe` 传入的窗口标签（同上） |
| 4 | `app/src/pump.rs:335` | `shell.sink.gone` `window = Id(&window)` | 刚加入即断连时的窗口标签（同上） |
| 5 | `core/src/app/recording.rs:380` | `session.status` `rec = Id(id)`、`why = Id(&why)` | `id` 为系统生成的录音 id（十六进制时间戳+8位随机）；`why` 为 `Status::why()` 返回的稳定状态原因码（如 `"file"` / `"captions.no_key"` / `"connect"`），非用户文本 |
| 6 | `core/src/app/recording.rs:406` | `recording.phase` `rec = Id(id)` | 录音 id |
| 7 | `core/src/app/recording.rs:415` | `recording.phase` `rec = Id(id)` | 录音 id |
| 8 | `core/src/app/recording.rs:762` | `stall.snapshot` `rec = Id(rec)` | 录音 id（`Dead` 分支） |
| 9 | `core/src/app/recording.rs:773` | `stall.snapshot` `rec = Id(rec)` | 录音 id（`Stalled` 分支） |
| 10 | `core/src/app/recording.rs:958` | `recording.start` `rec = Id(&start.id)` | 录音 id |
| 11 | `core/src/app/recording.rs:1445` | `recording.end` `rec = Id(&start.id)` | 录音 id（`done` 分支） |
| 12 | `core/src/app/recording.rs:1447` | `recording.end` `rec = Id(&start.id)` | 录音 id（错误分支） |

### 7. 读日志：三场故障「只看日志讲得出发生了什么」

#### 场景一：服务收音不回话（`log_tells_the_story_of_a_silent_service`）

只看日志的叙述：录音 `1a211445b3ffe916577` 以麦克风、1 条 lane、2 栏实时字幕开始，`lane.0` 成功连上服务（`lane.open` `code="ok"`）。随后服务一直没有回话，看守先判 `Heard(0)` 进入 `Slow`，不动满 10 000 ms 后判为 `Stalled`（`why="silent"`、`action="reopen"`），并留下 61 行的 `snap=1` 现场快照。紧接着看守发出 `lane.kick`，`lane.0` 经监督退避 250 ms 重开并成功连上，报告了一段 `0..9900 ms` 的 `silent` 空缺，随后 `Heard(0)` 回到 `Live`，整场录音最终以 `done` 正常结束。

依据的真实日志行：

```json
{"t_ms":1795586022207,"seq":22,"level":"info","component":"zutalk_core::app::recording","event":"recording.start","rec":"1a211445b3ffe916577","source":"mic","lanes":1,"columns":2,"captions":"live"}
{"t_ms":1795586022220,"seq":32,"level":"info","component":"zutalk_core::live::lane","event":"lane.open","lane":0,"code":"ok","keys_ms":0,"dial_ms":0}
{"t_ms":1795586022254,"seq":39,"level":"warn","component":"zutalk_core::app::recording","event":"judge.verdict","stage":"Heard","lane":0,"from":"Live","to":"Slow","why":"","action":"none"}
{"t_ms":1795586022293,"seq":41,"level":"warn","component":"zutalk_core::app::recording","event":"judge.verdict","stage":"Heard","lane":0,"from":"Slow","to":"Stalled","why":"silent","action":"reopen"}
{"t_ms":1795586022293,"seq":42,"level":"warn","component":"zutalk_core::app::recording","event":"stall.snapshot","snap":1,"rec":"1a211445b3ffe916577","stage":"Heard","lane":0,"health":"Stalled","why":"silent","idle_ms":10000}
{"t_ms":1795586022294,"seq":102,"level":"warn","component":"zutalk_core::app::recording","event":"stall.snapshot.end","snap":1,"lines":61}
{"t_ms":1795586022294,"seq":103,"level":"warn","component":"zutalk_core::app::recording","event":"lane.kick","lane":0,"why":"silent"}
{"t_ms":1795586022294,"seq":105,"level":"warn","component":"zutalk_core::rt::supervise","event":"part.state","part":"lane.0","from":"Running","to":"Waiting","why":"silent","wait_ms":250,"n":1}
{"t_ms":1795586022295,"seq":108,"level":"info","component":"zutalk_core::live::lane","event":"lane.open","lane":0,"code":"ok","keys_ms":0,"dial_ms":0}
{"t_ms":1795586022295,"seq":110,"level":"warn","component":"zutalk_core::live::lane","event":"lane.gap","lane":0,"t0_ms":0,"t1_ms":9900,"why":"silent"}
{"t_ms":1795586022302,"seq":112,"level":"info","component":"zutalk_core::app::recording","event":"judge.verdict","stage":"Heard","lane":0,"from":"Stalled","to":"Live","why":"","action":"none"}
{"t_ms":1795586022512,"seq":132,"level":"info","component":"zutalk_core::app::recording","event":"recording.end","rec":"1a211445b3ffe916577","code":"done","ms":22060}
```

#### 场景二：断网到放弃后重连（`log_tells_the_story_of_going_offline_and_reconnecting`）

只看日志的叙述：录音 `1a211446b867ad3082e` 开始后，`lane.0` 连续 8 次拨号均因 `connect` 失败。满 20 000 ms 时看守判 `Sent(0)` 进入 `Stalled`（`why="connect"`、`action="tell"`）并留下 61 行的 `snap=1` 现场快照；第 8 次失败撞到重启上限，监督发 `part.gave_up(part="lane.0", why="connect", n=8)`，`Sent(0)` 随之转为 `Dead`。之后界面发来 `Reconnect` 命令（`command.done` `code="ok"`），核心发 `lane.reconnect(lane=0)` 重新拉起 `lane.0`，拨号成功、连接回到 `Live`、`Sent(0)` 判回 `Live`，录音最终以 `done` 正常结束。

依据的真实日志行：

```json
{"t_ms":1795586026374,"seq":22,"level":"info","component":"zutalk_core::app::recording","event":"recording.start","rec":"1a211446b867ad3082e","source":"mic","lanes":1,"columns":2,"captions":"live"}
{"t_ms":1795586026385,"seq":30,"level":"warn","component":"zutalk_core::live::lane","event":"lane.open","lane":0,"code":"connect","keys_ms":0,"dial_ms":0}
{"t_ms":1795586026532,"seq":56,"level":"warn","component":"zutalk_core::app::recording","event":"judge.verdict","stage":"Sent","lane":0,"from":"Slow","to":"Stalled","why":"connect","action":"tell"}
{"t_ms":1795586026532,"seq":57,"level":"warn","component":"zutalk_core::app::recording","event":"stall.snapshot","snap":1,"rec":"1a211446b867ad3082e","stage":"Sent","lane":0,"health":"Stalled","why":"connect","idle_ms":20000}
{"t_ms":1795586026532,"seq":117,"level":"warn","component":"zutalk_core::app::recording","event":"stall.snapshot.end","snap":1,"lines":61}
{"t_ms":1795586026555,"seq":123,"level":"error","component":"zutalk_core::rt::supervise","event":"part.gave_up","part":"lane.0","why":"connect","n":8}
{"t_ms":1795586026557,"seq":126,"level":"error","component":"zutalk_core::app::recording","event":"judge.verdict","stage":"Sent","lane":0,"from":"Stalled","to":"Dead","why":"connect","action":"tell"}
{"t_ms":1795586026598,"seq":127,"level":"info","component":"zutalk_core::app::hub","event":"command.done","kind":"Reconnect","code":"ok","took_us":6}
{"t_ms":1795586026598,"seq":128,"level":"info","component":"zutalk_core::app::recording","event":"lane.reconnect","lane":0}
{"t_ms":1795586026599,"seq":130,"level":"info","component":"zutalk_core::live::lane","event":"lane.open","lane":0,"code":"ok","keys_ms":0,"dial_ms":0}
{"t_ms":1795586026599,"seq":133,"level":"info","component":"zutalk_core::live::lane","event":"lane.link","lane":0,"from":"Connecting","to":"Live","why":""}
{"t_ms":1795586026605,"seq":134,"level":"info","component":"zutalk_core::app::recording","event":"judge.verdict","stage":"Sent","lane":0,"from":"Dead","to":"Live","why":"","action":"none"}
{"t_ms":1795586026824,"seq":159,"level":"info","component":"zutalk_core::app::recording","event":"recording.end","rec":"1a211446b867ad3082e","code":"done","ms":36060}
```

#### 场景三：tape 写入口崩一次（`log_tells_the_story_of_a_tape_writer_panic`）

只看日志的叙述：录音 `1a211445a0d19606e1d` 进行中，`tape` 落盘部件在写一块音频时发生 panic，`rt::keep` 接住后发 `keep.lost(part="tape", n=1, why="panic")` 报出丢了 1 块。随后监督将 `tape` 从 `Running` 转入 `Waiting`（`wait_ms=250`），退避后依次经过 `Opening -> Running` 重开成功。新写入口收到后续块时在 `from_ms=10000` 补入 1 块静音（`tape.pad`），整场录音未中断并最终以 `done` 正常结束。

依据的真实日志行：

```json
{"t_ms":1795586021901,"seq":22,"level":"info","component":"zutalk_core::app::recording","event":"recording.start","rec":"1a211445a0d19606e1d","source":"mic","lanes":1,"columns":2,"captions":"live"}
{"t_ms":1795586022019,"seq":39,"level":"warn","component":"zutalk_core::rt::keep","event":"keep.lost","part":"tape","n":1,"why":"panic"}
{"t_ms":1795586022019,"seq":40,"level":"warn","component":"zutalk_core::rt::supervise","event":"part.state","part":"tape","from":"Running","to":"Waiting","why":"panic","wait_ms":250,"n":1}
{"t_ms":1795586022019,"seq":41,"level":"info","component":"zutalk_core::rt::supervise","event":"part.state","part":"tape","from":"Waiting","to":"Opening","why":"","wait_ms":0,"n":2}
{"t_ms":1795586022019,"seq":42,"level":"info","component":"zutalk_core::rt::supervise","event":"part.state","part":"tape","from":"Opening","to":"Running","why":"","wait_ms":0,"n":2}
{"t_ms":1795586022020,"seq":43,"level":"warn","component":"zutalk_core::capture::tape","event":"tape.pad","from_ms":10000,"blocks":1}
{"t_ms":1795586022202,"seq":62,"level":"info","component":"zutalk_core::app::recording","event":"recording.end","rec":"1a211445a0d19606e1d","code":"done","ms":20060}
```

### 8. 数和丢弃（`numbers.sh`、`dropped.sh` 与 P1-04 / P1-05 遗留项核对）

- `numbers.sh`、`dropped.sh` 均通过。
- **P1-04 报告里 12 个「来历待查」（`Origin to be traced`）常量现状**：全部 12 个均带单位与注释，现位置如下，统一列入 P1-26 负责人数字确认清单：
  - `app/src/updates.rs:21` `LOOK_MS`（21 600 000 ms，原在 `main.rs`，P1-23 迁入）
  - `core/src/app/work.rs:43` `SLOT_MS`（250 ms，原在 `hub.rs`，P1-19 迁入）
  - `core/src/capture/audio.rs:33` `ZERO_RUN_MS`（10 000 ms）
  - `core/src/id/keys.rs:284` `CREATE_WAIT_MS`（30 000 ms）、`287` `SIGN_IN_POLL_MS`（1 000 ms）、`289` `SIGN_IN_WAIT_MS`（300 000 ms）
  - `core/src/rt/supervise.rs:46` `FIRST_MS`（250 ms）、`50` `HEALTHY_MS`（30 000 ms）
  - `server/src/ledger.rs:21` `UPSTREAM_WAIT_MS`（10 000 ms）、`23` `OPENROUTER_KEY_LIFE_MS`（3 600 000 ms）
  - `server/src/lib.rs:209` `SWEEP_EVERY_MS`（60 000 ms）
  - `server/src/main.rs:12` `DIRECTORY_WAIT_MS`（10 000 ms）
- **P1-05 报告里 7 + 1 处「其实有关系」丢弃现状**：

| 原位置（P1-05） | 现在状态 |
|---|---|
| `app/src/main.rs:43`（字幕窗 `show`/`hide`） | **已解决**（P1-23）：迁至 `app/src/windows.rs::set_captions`，失败返回 `CaptionWindow::Missing`/`Failed` 并发 `warn!("shell.captions", …)` |
| `core/src/app/hub.rs:713`（`drop(tokio::spawn(task.finish()))`） | **已解决**（P1-19/P1-21）：已迁至 `Work` 树上具名任务（`Tree::spawn`）并由 `job.start`/`job.done` 跟踪 |
| `core/src/capture/audio.rs:564`（`thread.join()`） | **已解决**（P1-14）：线程改由 `rt::supervise::thread` 启动，panic 时发 `error!("thread.end", code = "panic")`，`Drop` 处 `join()` 仅收尾 |
| `core/src/capture/tape.rs:89`（`sync_data()`） | **已解决**（P1-15）：正常收尾改由 `rt::keep` 显式调 `Reel::sync()` 上报错误；`Drop` 仅作异常兜底 |
| `core/src/app/hub.rs:891`（空录音删目录） | **已解决**（P1-16/P1-19）：录音收尾已迁入 `app/recording.rs` `Sealing` 阶段显式处理 |
| `core/src/ai/write.rs:693`（删已完成 `Journal::FILE`） | 保留实话注释：记录已写入书架，残余 journal 文件不影响数据正确性 |
| `core/src/record/migrate.rs:162`（搬运失败清临时 tape） | 保留实话注释：下次搬运会先清理该目录（第 179 行） |
| `server/src/room.rs:104`（定时清扫删过期房间文件） | 留给 P5（下一轮清扫会重试） |

### 9. 配置与环境变量

- `core/src/app/core.rs::Config` 全部 15 个公开字段均有单行文档注释。
- `git grep -nE 'env::var|option_env!|env!\(' core/src app/src` 全部 4 处，均在允许范围内：
  - `core/src/app/core.rs:113`：`std::env::var("ZUTALK_LOG")`（`Config::from_env` 唯一运行期环境变量）
  - `core/src/app/work.rs:1340`：`env!("CARGO_PKG_VERSION")`（构建时版本常量）
  - `app/src/main.rs:67`：`option_env!("ZUTALK_SERVER")`（构建时服务器地址常量）
  - `app/src/main.rs:78`：`env!("CARGO_PKG_VERSION")`（构建时版本常量）

### 10. 代价（8 小时稳态与最坏 `Tally`）

- `core/tests/stats.rs::the_fullest_tally_fits_one_channel_message`：通过（最坏 `Tally` ≤ 8192 字节）。
- `core/tests/liveness.rs::i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`：总事件数 **5 828 条**，全程 `judge.verdict = 0`、`stall.snapshot = 0`；**`Tree::running()` 行数第 1 小时与第 8 小时均为 9（完全恒定）**。
- 第 1 小时 vs 第 8 小时事件条数并排对比（排除每分钟汇总后，第 8 小时为 **0 条**，完全不随小时数增长）：

| 类别 | 第 1 小时 | 第 8 小时 |
|---|---|---|
| `stats.summary` | 60 | 60 |
| `stat.count` | 202 | 180 |
| `stat.peak` | 61 | 60 |
| `stat.queue` | 242 | 240 |
| `stat.spread` | 185 | 180 |
| 非汇总事件（启动与开录） | 37（`boot.step` 4、`command.done` 2、`hub.boot` 1、`identity.who` 1、`job.start` 4、`job.done` 4、`lane.link` 1、`lane.open` 1、`part.state` 4、`recording.phase` 1、`recording.start` 1、`session.status` 1、`task.start` 11、`task.end` 2） | **0** |
| **合计** | **787** | **720** |
| **`Tree::running()` 行数** | **9** | **9** |

---

## `P1.md` 三张表改动清单

仅改了 §3.13.3、§3.13.4、§3.13.11（§3.13.11 全部 10 行已与代码完全一致，无需改动）：

| 节 | 改的行 | 原因 |
|---|---|---|
| §3.13.3 | `stats.full` | 代码对名字超过 32 字节也发此事件，补齐 `kind` 全部 5 个值（`counts` / `peaks` / `spreads` / `queues` / `tasks`） |
| §3.13.3 | `recording.phase` | 代码在 `to = Ended` 且携带失败代码时级别为 `error` |
| §3.13.3 | `job.start`、`job.done` | 实际字段名为 `job`（因行头已占 `seq`，避免冲突） |
| §3.13.3 | `boot.step` | `step = "devices"` 另带设备数量 `n`（不含设备名） |
| §3.13.3 | `job.again` | 补注实际使用它的 4 类重试任务（`write` / `file` / `create` / `claim`） |
| §3.13.3 | `shell.captions` | 代码实际发出 `shown` 与 `code`（`ok` / `missing` / `failed`），失败时级别为 `warn` |
| §3.13.4 | 队列一行 | 补注 P1-23 新增的外壳更新通道 `shell.updates`（全仓共 13 条有名字队列） |

---

## 证据

### 先写测试的失败输出（`a_sweep_is_noted_at_debug` 改前）

```text
thread 'a_sweep_is_noted_at_debug' (11553758) panicked at server/tests/log.rs:52:5:
assertion `left == right` failed: expected one server.sweep event: []
  left: 0
 right: 1
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

### 五个「显式」规则脚本的违例证明（试完均已还原）

```text
=== PROOF 1: events.sh ===
core/src/lib.rs:16:fn _proof_ev() { println!("x"); }
  no prints: say it with an event, or mark the line `// before the log: <why>`
exit=1

=== PROOF 2: numbers.sh ===
core/src/lib.rs:16: PROOF_MS has a unit and no reason: say on its line or the one above why it is that number
exit=1

=== PROOF 3: bounded.sh ===
core/src/lib.rs:16: channel built outside core/src/rt/queue.rs (mpsc::channel::<u8>()
exit=1

=== PROOF 4: spawn.sh ===
core/src/lib.rs:16: task/thread spawned outside core/src/rt/supervise.rs (tokio::spawn()
exit=1

=== PROOF 5: dropped.sh ===
core/src/lib.rs:16: discarded without `// dropped: <why>`: fn _proof_dr() { let _ = std::fs::remove_file("x"); }
exit=1
```

### 新增测试（共 9 条）

- `server/tests/log.rs::a_sweep_is_noted_at_debug`
- `core/tests/stats.rs::a_peak_that_moved_is_written_in_the_summary`
- `app/tests/beat.rs::a_frame_gap_from_the_ui_is_logged_and_counted`
- `app/tests/updates.rs::looking_for_an_update_says_what_it_found`
- `core/tests/liveness.rs::every_event_in_the_catalogue_can_be_told_apart`
- `core/tests/liveness.rs::log_tells_the_story_of_a_silent_service`
- `core/tests/liveness.rs::log_tells_the_story_of_going_offline_and_reconnecting`
- `core/tests/liveness.rs::log_tells_the_story_of_a_tape_writer_panic`
- `core/tests/liveness.rs::two_minute_recording_tally_matches_the_table`

### 验证结果

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 8 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`shell.sh`、`spawn.sh`）：全部通过。
- `zutalk-log`、`zu`、`zutalk`（`app`）、`zutalk-server --test log`、`zutalk-core` 全部非 TCP 集成测试（含新增 9 条及 `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`）：全部通过。
- `ui`（`npm run check && npm test`）：通过。
- 注：沙箱内无法绑定本地 TCP 端口（`wiremock` / `TcpListener`），含本地端口监听的全量 `scripts/build/check.sh` 将由协调器在沙箱外连续验证 3 次。

## 留下的，要负责人的

- `app.start` 首次真机启动首行核对、12 个 `Origin to be traced` 既有常量确认、`server/src` 的 3 处任务与 1 处房间通道收口时机（P4/P5），均归入 P1-26 给负责人的总清单。
