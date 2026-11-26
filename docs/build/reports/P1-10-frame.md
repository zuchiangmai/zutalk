状态：完成

# P1-10 帧：`live/frame.rs`：报告

## 改了什么

1. **命名消歧（一个名字只指一样东西）**：
   - `core/src/live/lane.rs`：`lane::Health` 改名为 `lane::Link`（`Event::Health` 改为 `Event::Link`），从此 `Health` 只指 `rt::water::Health`；`lane::Frame` 改名为 `lane::Reply`、`lane::frame()` 改名为 `lane::reply()`，从此 `Frame` 只指字幕的一帧。同步更新 `hub.rs`、`core/tests/live.rs`、`core/tests/app.rs`、`core/tests/liveness.rs` 与台架。

2. **纯规则组帧与 8 KB 字节预算（`core/src/live/frame.rs`，新文件）**：
   - 常量：`KEEP_LINES = 6`（字幕窗每栏最多画 6 行已完成字幕）、`MAX_COLUMNS = 4`（设计稿最多 4 栏）、`MAX_BYTES = 8 * 1024`（Tauri 通道 8 KB 以内直接注入）。
   - 线上类型：`Frame`、`Column`（新增 `health: Health`）、`Caption`、`Status` 迁入 `live/frame.rs`（带 `Serialize`、`ts_rs::TS`），`app/proto.rs` 重新导出，更新生成 `core/bindings/{Frame,Health,Column,Caption}.ts`。
   - `Composer`：取代原 `hub.rs` 中的 `View`。已完成行到达时只转换一次且 `units` 为空，每栏只留最后 `KEEP_LINES` 行；只有正在说的 `tail` 文本或语言变化时才调 `profile(written).units(text)` 切词；无变化时 `frame()` 返回 `None`。`t_audio` 取已见行与空缺的最晚 `t1`；`Column.health` 按表从 `Link` 映射（`Live` / `Stopped("done")` → `Live`，`Behind` / `Connecting` → `Slow`，其余 `Stopped(code)` → `Dead { why }`），并预留 `judged` 接口供 `P1-17` 接入看守判断。
   - `Fit { Whole, Lines, Units, Tail }`（带 `code()`）与纯函数 `fit(frame) -> (Frame, Fit)`：先算便宜字节上界，超 `MAX_BYTES` 时才序列化，按优先级依次丢弃最满栏最旧已完成行（`Lines`）→ 清空全部 `tail.units`（`Units`）→ 二分截断 `tail.text` 前端并补 `…`（`Tail`），构造上保证 JSON 字节数恒 `<= 8192`。
   - `status(paused, links, quiet) -> Status` 迁入 `live/frame.rs`；`lane::plan(shown)` 限制最多规划 `MAX_COLUMNS` 栏。
   - `live/frame.rs` 内无时钟、无 IO、无 `zutalk_log`。

3. **可观测性事件与统计（`core/src/app/hub.rs`、`core/src/live/lane.rs`、`docs/EVENTS.md`）**：
   - `hub.rs`：删除 `View`、`KEEP`、`status`，`Task::listen` 改用 `Composer`；每出一帧计 `frame.made`、高水位 `frame.bound_bytes` 记裁前上界；`Fit != Whole` 时计 `frame.cut`；`Fit` 变化时发 `info!("frame.fit", from, to, bytes)`。
   - `lane.rs`：新增纯转换函数 `Link::next(self, to) -> Option<Link>` 与 `Link::code()`；`say(link)` 在状态变化时发 `lane.link`（到 `Behind` 或非 `done` 的 `Stopped` 为 `warn!`，其余 `info!`）；发出 `Event::Gap` 时同步发 `warn!("lane.gap", lane, t0_ms, t1_ms, why)` 并计 `lane.gaps`。
   - `docs/EVENTS.md`：新增 `lane.link`、`lane.gap`、`frame.fit`。

4. **界面与测试样本对齐**：
   - 更新 `ui/dev/talk.ts`、`ui/tests/room.test.ts`、`ui/tests/store.test.ts` 中构造 `Column` 字面量处补齐 `health: { is: 'Live' }`；微调 `core/tests/bench/speech.rs` 三语长句密度使 `i1_three_languages_fit_a_frame` 稳定触发超 8 KB 裁剪路径。

文件：`core/src/live.rs`、`core/src/live/frame.rs`、`core/src/live/lane.rs`、`core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/bindings/{Caption,Column,Frame,Health}.ts`、`core/tests/frame.rs`、`core/tests/live.rs`、`core/tests/app.rs`、`core/tests/bench/{mod,speech}.rs`、`core/tests/liveness.rs`、`docs/EVENTS.md`、`ui/dev/talk.ts`、`ui/tests/{room,store}.test.ts`、本报告。

## 证据

### 1. 新增与解除挂起的测试（全部通过）

- `core/tests/frame.rs`（新文件，9 条全绿）：
  1. `a_line_shows_in_a_column_once_it_is_in_that_columns_language`
  2. `finished_lines_carry_no_units_and_the_tail_does`
  3. `a_column_keeps_its_last_six_lines`
  4. `nothing_changed_no_frame`
  5. `a_frame_never_outgrows_its_budget`
  6. `what_gives_way_first_is_the_oldest_line_of_the_fullest_column`
  7. `a_columns_health_is_its_links`
  8. `the_status_line_says_what_most_needs_saying`（自 `app.rs` 迁来）
  9. `no_more_than_four_columns`
- `core/tests/live.rs`（新增 3 条全绿）：
  10. `a_link_changes_only_as_its_table_says`
  11. `every_change_of_link_is_said_once`
  12. `a_gap_is_an_event_with_its_edges_and_its_reason`
- `core/tests/liveness.rs`：
  13. `i1_three_languages_fit_a_frame`：解除挂起并通过（`words=3921, missing=0, p50=321ms, p99=321ms, max=321ms, max_frame=8163B <= 8192B, bound_most=10770B, cut=607, fit_events=48, wall=8.27s`）。
  14. `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`：解除挂起并通过（见下表）。
  15. `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed`：补上并启用 `lane.link`（恢复至 `Live`）与 `lane.gap`（`why = "panic"`）断言，通过。

### 2. `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` 数据及与 `P1-08` 对比

| 小时段 | 词数 | p50 | p99 | max |
|---|---|---|---|---|
| 第 1 小时 | 7241 | 321 ms | 321 ms | 321 ms |
| 第 2 小时 | 7183 | 321 ms | 321 ms | 321 ms |
| 第 3 小时 | 7199 | 321 ms | 321 ms | 321 ms |
| 第 4 小时 | 7213 | 321 ms | 321 ms | 321 ms |
| 第 5 小时 | 7173 | 321 ms | 321 ms | 321 ms |
| 第 6 小时 | 7172 | 321 ms | 321 ms | 321 ms |
| 第 7 小时 | 7158 | 321 ms | 321 ms | 321 ms |
| 第 8 小时 | 7148 | 321 ms | 321 ms | 321 ms |

| 指标 | `P1-08`（旧 `View`） | `P1-10`（`Composer` + `fit`） |
|---|---|---|
| 丢词数 | `0` | `0` |
| 每小时 p50 / p99 | `320 ms` / `320 ms`（未加 1 ms 错峰前） | `321 ms` / `321 ms`（零漂移） |
| 最大一帧 JSON 字节数 | **`15144 B`**（超上限失败） | **`6127 B`**（`<= 8192 B` 通过；裁前便宜上界峰值 `7539 B`） |
| 捕获事件数 | `486` | `1450`（含每分钟统计变化行与 `lane.link`） |
| 真实耗时 | `64.82 s` | `62.25 s` |

（三语压力场景 `i1_three_languages_fit_a_frame` 最大帧从 `P1-08` 的 `18496 B` 收敛至 `8163 B <= 8192 B`。）

### 3. 纯度与边界检查

- `grep -rn "struct View\|fn columns" core/src`：无结果。
- `grep -n "zutalk_log" core/src/live/frame.rs`：无结果。
- 全部 6 个规则脚本（`layers.sh`、`events.sh`、`numbers.sh`、`bounded.sh`、`dropped.sh`、`spawn.sh`）通过，欠账表未扩大。
- 全量门禁 `scripts/build/check.sh` 全绿（Rust `passed 539, failed 0, ignored 12`，含两条 `i1_*` 活性测试；界面 `svelte-check` 0 errors / `27 passed`）。

## 留下的，要负责人的

- 帧目前仍经 `Msg::Live(id, frame.columns)` 走 hub 邮箱和 `Event::Live`：下一步 `P1-11` 改为 `Outlet` 寄存器，`P1-12` 让录音任务绕开 hub 直接写 `Frame` 信号。
- `Column.health` 接入看守 `judge` 的结果留在 `P1-17`。
