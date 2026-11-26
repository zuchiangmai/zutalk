状态：完成

# P1-11 核心往外说的都是寄存器：`Outlet`：报告

## 改了什么

1. **`Outlet` 与 `Notices`（`core/src/app/hub.rs`、`core/src/app/proto.rs`、`core/src/live/frame.rs`）**：
   - `Core::start(config) -> Result<(Core, Outlet), End>`：去掉 `out` 回调，`Hub` 不再在自身任务上做序列化或调外壳函数。
   - `Outlet`（可克隆）：持有 7 个只留最新值的 `Reading`：`state: Reading<State>`、`frame: Reading<Frame>`、`session: Reading<Option<Session>>`、`level: Reading<Level>`、`water: Reading<Water>`、`stats: Reading<Tally>`、`said: Reading<Notices>`（其中 `session`、`level`、`water` 先以默认初值挂好，由 `Hub` 持有对应 `Register` 直至停机，在 `P1-12`、`P1-16` 接上写入）。
   - `proto::Level { peak: u16, rms: u16 }` 与 `proto::Notices { next: u64, codes: Vec<String> }`（`Notices::KEEP_CODES = 16`）：带 `Serialize`、`TS`（`#[ts(export)]`）。`Voice::say(code: &'static str)` 用 `Register::change` 追加代码并裁掉超过 16 条的最旧项、`next` 加一，同时发结构化事件 `info!("notice.said", code = code)` 并计 `notices.said`。
   - `Hub::publish()` 在状态变化时调 `state_reg.set(..)` 并计 `state.sent`。
   - `Msg::Live(id, Frame)` 携带完整 `Frame`（`Frame` 补 `Default`），`Hub` 收到后 `frame_reg.set(frame)`，停止录音时重置为 `Frame::default()`；房间槽任务 `slot` 改读 `Reading<Frame>`，写出 `frame.columns` 的 JSON（无栏则清空）；删去 `Hub.live` 与 `Hub.out`。

2. **`Stats` 每秒信号（`core/src/rt/stats.rs`、`docs/EVENTS.md`、`core/bindings/`）**：
   - 新增常量 `STATS_SIGNAL_EVERY_MS = 1_000`（`PLAN.md` §4.5.2，1 Hz 以内）。`rt::stats::publish(stats, out: Register<Tally>)` 每秒同步 `log.lost` / `log.refused`，仅当 `stats.tally()` 与寄存器当前值不同时经 `change` 更新 `out`；每 `STATS_SUMMARY_EVERY_MS`（60 000 ms）照旧写 `stats.summary` 及动过的 `stat.*` 行。
   - `Tally` 加 `#[ts(export)]`，重新导出生成 `core/bindings/{Tally,Count,PeakRow,SpreadRow,QueueRow,Notices,Level}.ts`。
   - `docs/EVENTS.md` 新增 `notice.said`。

3. **词表检查正则（`ui/tests/words.test.ts`）**：
   - `coreCodes` 的 `said` 正则加入 `|\.say`，识别 `.say("…")` 中的代码字面量，同时不匹配 `info!("…")` 等日志事件代码。

4. **外壳最小转发任务（`app/src/main.rs`、`app/Cargo.toml`）**：
   - 外壳建自己的监督树根 `Tree::root("shell", core.stats())`，通过 `shell.spawn("shell.forward", ..)` 起转发任务，监听 `Outlet` 的 `state`、`frame`、`said`，分别包成现有 `Event::State`、`Event::Live(frame.columns)`、`Event::Notice` 交给 `fan`（`spawn.sh` 欠账保持不变）。

5. **统一测试台与台架探针（`core/tests/hub/mod.rs`、`core/tests/app.rs`、`core/tests/city.rs`、`core/tests/bench/mod.rs`、`core/tests/stats.rs`）**：
   - 删去 `core/tests/app.rs` 中重复的 `App` 等脚手架，全仓库 `core/tests` 只留 `core/tests/hub/mod.rs` 一份 `App`（持有 `Core`、`Outlet` 与起核前安装的 `zutalk_log::Seen`，提供基于寄存器的 `until`、`frame`、`notice`）。
   - `core/tests/bench/mod.rs` 删去回调事件队列，探针直接监听 `Outlet` 的 `state`、`frame`、`said`，并对整份 `Frame` 测量最大序列化字节数。

文件：`app/Cargo.toml`、`app/src/main.rs`、`core/bindings/{Count,Level,Notices,PeakRow,QueueRow,SpreadRow,Tally}.ts`、`core/src/app/hub.rs`、`core/src/app/proto.rs`、`core/src/live/frame.rs`、`core/src/rt/stats.rs`、`core/tests/app.rs`、`core/tests/bench/mod.rs`、`core/tests/city.rs`、`core/tests/hub/mod.rs`、`core/tests/stats.rs`、`docs/EVENTS.md`、`ui/tests/words.test.ts`、本报告。

## 证据

### 1. 新增与更新的测试

先以未接线存根验证新测试失败（红），实现后转绿：

- `core/tests/app.rs` 新增 3 条（全绿）：
  1. `whoever_reads_late_gets_what_is_there_now`：核心启动并读出首份状态后克隆 `Outlet`，`late.state.now()` 立即拿到当前状态与序号，`state.sent` 计数一致。
  2. `more_codes_than_are_kept_lose_the_oldest`：不读 `said` 连发 20 条 `Pause`（`recording.none`），`said.now()` 留 16 条、`next == 20`；日志 `notice.said` 共 20 条、统计 `notices.said == 20`；再发 1 条 `CopyRotation`（`rotation.none`）后队首最旧项淘汰、队尾为最新项。
  3. `the_tally_comes_once_a_second_while_something_moves`（暂停时钟）：前 5 虚拟秒每秒一次拒绝命令，`outlet.stats` 序号增长在 `4..=6` 且含 `notices.said == 5`；随后静默 5 虚拟秒序号不变。
- `core/tests/app.rs` 更新验证：
  4. `the_recovery_key_goes_to_the_clipboard_and_through_nothing_else`：改为监听并序列化途中每一份 `State` 与 `Notices`，断言恢复密钥未出现在任何一份载荷中（通过）。
  5. `a_room_carries_the_live_captions_or_a_recording_and_the_draft_is_kept_line_by_line` 与 `with_their_own_key_captions_come_without_signing_in`：改为从 `outlet.frame` 读取实时字幕 `Hello`。

### 2. 因寄存器只留最新值而调整断言方式的测试清单（做法第 6 条）

1. `core/tests/app.rs` — `a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach`：原测试直连不可达端口 `127.0.0.1:9`，`login = Some(..)` 到 `SignedIn(Err("offline"))` 清空 `login = None` 仅隔亚毫秒，寄存器会直接覆盖。现改为本地 `TcpListener` 替身先握手挂住连接，确定性断言 `state.login` 出现且前缀为中继 URL，再断开连接并断言 `offline` 提示与 `state.login.is_none()`。
2. `core/tests/app.rs` — `a_recording_stops_at_once_and_is_kept_whole`：`Stop` 后 `writing` 首步 `"closing"` 在极快封带并请求钥匙失败时会立即推进为 `"offline"`，因此对 `session.is_none()` 当刻断言 `matches!(step, Some("closing" | "offline"))`，随后照旧 `until` 等待并断言 `"offline"` 与封带长度。
3. `core/tests/city.rs` — `a_recording_goes_to_an_event_of_the_city_with_its_audio_and_again_without`：为防止本地 `City` 替身过快完成使 `state.filing` 非空状态一闪而过，给 `City::hold_next_post()` 加一次性 `Condvar` 闸门挂住首个 `POST`，确定性断言 `s.filing` 含该录音后再放行完成交城。
4. `core/tests/app.rs` — `the_old_library_moves_in_on_its_own_and_nothing_of_it_leaves`、`without_a_key_sound_is_kept_and_the_status_says_why`、`drafts_without_a_key_wait_quietly_and_are_written_once_there_is_one`、`until_no_sign_in_first`：原测试靠排空 `app.events` 通道检查未发某提示或未重发 `writing`；现改为对比 `outlet.state.now()` 序号未变及检查 `outlet.said` 与 `app.seen.of("notice.said")` 全量日志无意外代码，约束更强。

### 3. `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` 数据及与 `P1-10` 对比

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

| 指标 | `P1-10`（回调读 `Vec<Column>`） | `P1-11`（探针读 `Outlet.frame` 整帧 `Frame`） |
|---|---|---|
| 丢词数 | `0` | `0` |
| 每小时 p50 / p99 / max | `321 ms` / `321 ms` / `321 ms` | `321 ms` / `321 ms` / `321 ms`（完全一致） |
| 最大一帧 JSON 字节数 | `6127 B`（仅 `columns`） | `6167 B`（含 `Frame` 外层字段，`<= 8192 B`；裁前上界峰值 `7548 B`） |
| 三语压力最大一帧字节数 | `8163 B`（仅 `columns`） | `8192 B`（整份 `Frame` 恰好卡在 `8192 B` 预算内，`cut = 607`，`fit_events = 48`） |
| 捕获事件数 | `1450` | `1452`（多 2 行 `state.sent` 汇总计数） |
| 真实耗时 | `62.25 s` | `72.07 s`（同进程并发跑 9 条活性测试） |

### 4. 边界与门禁检查

- `grep -rn "Box<dyn Fn(Event)\|self.out)" core/src`：无结果。
- `grep -rn "struct App\b" core/tests`：仅 `core/tests/hub/mod.rs` 一处。
- `core/tests/stats.rs` 中 `the_fullest_tally_fits_one_channel_message`（`<= 8192 B`）与 `once_a_minute_what_moved_is_written` 通过。
- 全量门禁 `scripts/build/check.sh` 全绿：Rust `passed 545, failed 0, ignored 12`，`ui`（`svelte-check` 0 errors + `vitest` 27 passed）全部通过。

## 留下的，要负责人的

- 帧、录音状态句与电平在 `P1-12` 改为由录音任务绕开 hub 邮箱直接写 `Outlet` 寄存器，并切到 `Event = State | Signal | Notice` 协议。
