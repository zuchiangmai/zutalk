# P1-05 帧：`live/frame.rs`

## 目标

做完以后：

1. 字幕的一帧有上限：每栏最多 6 行说完的加一行正在说的，最多 4 栏，序列化后不超过 8192 字节。上限由构造保证，由测试守着。
2. 组一帧的代价只取决于变了什么：说完的行只切一次；什么都没变就不出帧。
3. 组帧的规则住在 `core/src/live/frame.rs`，是值进值出；hub 里的 `View` 没有了。
4. `core/tests/liveness.rs` 的两条 `i1_*` 通过。

## 背景

- 类型和规则在 `docs/build/phases/P1.md` §3.4。`PLAN.md` §2.3 的 `Frame` 一行、§5.1 的「`frame.rs`（由现在的 `View` 搬来，输出 `Frame`）」。
- `core/src/app/hub.rs` 末尾：`View`（`new`、`take`、`columns`）、`status`、`drafted`、`KEEP = 12`；`Task::listen` 里用它们，每个 `FRAME`（100 ms）在 `view.dirty` 时发 `Msg::Live(id, view.columns())`。
- `View::columns()` 每次都对每栏的每一行调 `profile(lang).units(text)`（泰文是 ICU 切词）。一场录音里这是每秒十次、每次几十行。
- `core/src/app/proto.rs`：`Caption`、`Column`、`Event::Live(Vec<Column>)`。
- `core/src/live/lane.rs`：`Health`（`Connecting`、`Live`、`Behind`、`Stopped`）、`Event`（`Lines`、`Tail`、`Gap`、`Health`）。
- `rt::water::Health`（P1-04）：`Live`、`Slow`、`Stalled{why}`、`Dead{why}`。
- 界面：`ui/src/ui/Captions.svelte` 每栏最多画 6 行说完的，只逐字显现 `tail`；`ui/dev/talk.ts`、`ui/dev/mock.ts` 造假的 `Column`；`ui/tests/room.test.ts` 里有一个 `Column` 字面量。

## 做法

1. **改名**（两处，都是为了一个名字只指一样东西）：
   - `lane::Health` → `lane::Link`（它是 lane 自己报的连接状态）；`Event::Health { stream, state }` 跟着改成 `Event::Link { stream, state }`。从此 `Health` 只指 `rt::water::Health`。
   - `lane::Frame`（服务的一个回复）→ `lane::Reply`，`lane::frame()` → `lane::reply()`。从此 `Frame` 只指字幕的一帧。
   - 调用处：`lane.rs`、`hub.rs`、`core/tests/live.rs`、`core/tests/app.rs`、台架。
2. **`live/frame.rs`**（新文件）。照 `P1.md` §3.4：
   - 常量 `KEEP = 6`、`MAX_COLUMNS = 4`、`MAX_BYTES = 8 * 1024`。
   - 线上的类型 `Frame`、`Column`（多一个 `health: Health`）、`Caption` 住在这里（带 `Serialize`、`TS`）；`app/proto.rs` 里原来的 `Caption`、`Column` 删掉，改成引用这里的。
   - `Composer`：`View` 搬来。`take(event)` 时就把说完的行变成 `Caption`（不带 `units`）存起来，每栏只留最后 `KEEP` 行；正在说的那一行（`tail`）在它变了的时候才重新切 `units`。`frame()` 在上次之后什么都没变时返回 `None`。
   - 哪一行出现在哪一栏的规则原样保留（头两种语言共用一条双向翻译的流；一行在某一栏里：有那种语言的译文就显示译文；没有，而它已经说完或这条流只转写，就显示原话；否则还不显示）。
   - `Column.health` 现在由 `Link` 映射，一张表：`Live` → `Live`；`Behind` → `Slow`；`Connecting` → `Slow`；`Stopped("done")` → `Live`；`Stopped(code)` → `Dead{why: code}`。P1-11 换成看守的判断。
   - `t_audio`：见过的文字里最晚的结束时间（各事件里行的 `t1`、`Gap` 的 `t1`）。
   - `fit(frame) -> Frame`：纯函数，次序照 `P1.md` §3.4。`Composer::frame()` 出去的帧都过 `fit`。量大小时不要每帧都序列化：先用便宜的上界，上界超了 `MAX_BYTES` 才真的量。
   - `status(paused, links, quiet) -> Status` 从 `hub.rs` 搬到这里，逻辑不变（P1-11 再给它加「判断出来的健康」这个参数）。它返回的 `Status` 类型也从 `app/proto.rs` 搬到这里（`live` 不能往上用 `app`），`proto.rs` 引用它。`drafted` 留在 `hub.rs`。
   - `lane::plan(shown)` 最多规划 `MAX_COLUMNS` 栏：多出来的语言不起流。（`Setup` 里拒绝第五栏是 P1-07。）
3. **`hub.rs`**：`Task::listen` 用 `Composer` 代替 `View`；`frame()` 有值时发 `Msg::Live(id, frame.columns)`（协议这一件不改，P1-07 改）。`View`、`KEEP`、`status` 从 `hub.rs` 删掉。
4. **界面**：`Column` 多了 `health`，说完的行的 `units` 是空的。`ui/dev/talk.ts`、`ui/dev/mock.ts`、`ui/tests/room.test.ts` 里造 `Column` 的地方补上 `health: { is: 'Live' }`，让 `npm run check` 过。`Captions.svelte` 不用改。
5. `cargo test` 会重写 `core/bindings/`：把变了的和新增的（`Frame.ts`、`Health.ts`、`Column.ts`、`Caption.ts` 等）一起提交。

## 先写的测试

`core/tests/frame.rs`（值进值出，不起任务）：

1. `a_line_shows_in_a_column_once_it_is_in_that_columns_language`：三种语言、两条流。双向流里一行英文，带中文译文：英文栏显示原话，中文栏显示译文；译文还没到、行还开着：中文栏没有它；这条流只转写时原话直接显示。（把 `View::columns` 的注释变成断言。）
2. `finished_lines_carry_no_units_and_the_tail_does`：泰文的 `tail` 的 `units` 是 ICU 切出的词尾，说完以后那一行的 `units` 是空的。
3. `a_column_keeps_its_last_six_lines`：送 1,000 行，帧里那一栏是最后 6 行。
4. `nothing_changed_no_frame`：`frame()` 取过一次以后，没有新事件再取是 `None`；来一条 `Tail` 又有；`pause(true)` 以后也有一帧。
5. `a_frame_never_outgrows_its_budget`：4 栏，连着送 200 行各 150 个汉字的行和同样长的泰文译文，再加一条 600 字的 `tail`：每一帧 `serde_json::to_vec(&frame).len() <= MAX_BYTES`，而且每栏的 `tail` 都还在。
6. `what_gives_way_first_is_the_oldest_line_of_the_fullest_column`：`fit` 的表格测试，三步各一例（丢旧行；去掉 `tail` 的 `units`；截 `tail` 的头并补 `…`）。短的帧原样返回。
7. `a_columns_health_is_its_links`：映射表逐行。
8. `the_status_line_says_what_most_needs_saying`：从 `core/tests/app.rs` 搬来，改用 `Link`。
9. `no_more_than_four_columns`：`plan` 对 6 种语言只给 3 条流；`Composer` 的帧只有 4 栏。

`core/tests/liveness.rs`：两条 `i1_*` 如果被 P1-03 挂起了，拿掉 `ignore`。

## 验收

- `scripts/build/check.sh` 全绿，`i1_*` 在其中。
- 报告里贴 `i1_eight_hours_…` 的每小时 p50、p99、最大的一帧多少字节、真实耗时，并和 P1-03 报告里的数对比。
- `grep -n "struct View\|fn columns" core/src -r` 没有结果；`core/src/live/` 通过 `layers.sh`（`frame.rs` 不许用 `crate::app`）。

## 不做

- 不改协议的形状（`Event::Live` 还在），不动寄存器、hub 的邮箱（P1-06 至 P1-08）。
- 不把 `Column.health` 接到 `judge`（P1-11）。
- 不动观看页和 `Captions.svelte` 的画法。
