状态：完成

# P1-12 现场的路绕开 hub：`Frame`、`Session`、`Level` 是信号：报告

## 改了什么

1. **协议（`core/Cargo.toml`、`core/src/app/proto.rs`、`core/bindings/`）**：
   - `core/Cargo.toml` 的 `serde` 开启 `rc` 特性。
   - `proto::State` 去掉 `session`；`Level` 新增纯函数 `Level::of(pcm: &[i16]) -> Level` 计算每 100 ms 块的峰值与均方根。
   - 新增 `Stamped { seq: u64, signal: Signal }` 与 `Signal { Frame(Arc<Frame>), Session(Arc<Option<Session>>), Level(Arc<Level>), Water(Arc<Water>), Stats(Arc<Tally>) }`（`#[serde(tag = "of", content = "is")]`）。
   - `proto::Event` 改为 `State(Arc<State>) | Signal(Stamped) | Notice(String)`（`#[serde(tag = "kind", content = "is")]`）。
   - 重新生成并更新 `core/bindings/{Event,State,Signal,Stamped,Water,Mark,Stage}.ts`。

2. **录音任务直写寄存器，采集线程闭包不碰邮箱（`core/src/app/hub.rs`、`core/src/live/frame.rs`、`docs/EVENTS.md`）**：
   - `Status` 新增 `code(&self) -> &'static str` 与 `why(&self) -> &str`，`frame.rs` 新增 `status_reason` 返回对应的静态原因代码（仅 `AudioOnly` 非空）。
   - `Hub` 启动录音时先把初始 `Session` 写入 `session_reg`，再把 `frame`、`session`、`level`、`said` 四个寄存器克隆交给 `Task`；停止或收到 `Msg::Ended` 时重置 `session_reg = None`、`frame_reg = Frame::default()`、`level_reg = Level::default()`。
   - 帧定时器在 `composer.frame()` 有值且当前场仍有效时直接 `frame.set(..)`；状态句变化时仅当 `session_reg` 中当前录音的 `id` 与本任务一致才经 `Register::change` 更新，并发出 `info!("session.status", rec = Id(&id), from, to, why)`。
   - `sink` 在录音态对每块计算 `Level::of(&block.pcm)` 直接 `level.set(..)`；暂停、停止或采集自行结束时归零。
   - `sink` 与 `notice` 两个采集线程闭包不再持有或使用 `tx`：终止原因（`"done"`、`"tape"`、`"audio.closed"` 或文件解码的致命代码）写入 `watch::Sender<Option<&'static str>>`，可恢复或单路声源通知直接调 `said.say(code)`；`Task::listen` 在 async 循环中等候该 `watch`，结束时由 async 任务调 `said.say(code)` 并向 hub 发 `Msg::Ended`。
   - 删去 `Msg::Live`、`Msg::Status` 与不再使用的 `Msg::Say`（`slot`、`recover`、`Task::finish`、`Task::write` 均直接调 `Voice::say`）。
   - 录音任务开头写 `info!("recording.start", rec = Id(&id), source, lanes, columns, captions)`，封好 tape 后（或中途失败时）写 `recording.end`（`code == "done"` 为 `info!`，失败代码为 `error!`，带 `rec`、`code`、`ms`）。`docs/EVENTS.md` 新增 `recording.start`、`recording.end`、`session.status`。

3. **栏数上限与 `retain::destroy`（`core/src/app/hub.rs`、`core/src/record/retain.rs`、`ui/lang/*.json`）**：
   - `Command::Setup` 当 `setup.shown.len() > MAX_COLUMNS`（4）时以 `"captions.columns"` 拒绝，不改 `state.setup` 与磁盘文件；八个语言文件 `ui/lang/*.json` 补齐 `code.captions.columns`。
   - `retain::destroy(tapes, library, under_way, id)` 改为显式接收当前正在录音的 `id`，不再读取 `State.session`（`layers.sh` 欠账保持不变）。

4. **外壳与界面适配（`app/src/main.rs`、`ui/src/lib/core.svelte.ts`、`ui/src/ui/{App,Home,Reader}.svelte`、`ui/dev/mock.ts`、`ui/tests/store.test.ts`）**：
   - `app/src/main.rs` 的 `forward` 监听 `Outlet` 全部寄存器，分别转发 `Event::State`、五种 `Event::Signal(Stamped { seq, signal })` 与 `Event::Notice`。
   - `ui/src/lib/core.svelte.ts` 的 `store` 新增 `session`、`level`、`water`、`stats`；`take` 按信号种类各自记录最新 `seq`，丢弃旧序号，`Frame.columns` 写入 `store.live`。`App.svelte`、`Home.svelte`、`Reader.svelte` 改读 `store.session`；`ui/dev/mock.ts` 同步切到 `Signal`。

5. **测试台与台架探针（`core/tests/hub/mod.rs`、`core/tests/bench/mod.rs`、`core/tests/{app,frame,liveness,retain}.rs`）**：
   - `App` 新增 `session(..)`，`Bench` 探针与 `wait_session(..)` 改读 `Outlet.session`，全量既有测试迁离 `state.session`。

文件：`app/src/main.rs`、`core/Cargo.toml`、`core/bindings/{Event,Mark,Signal,Stage,Stamped,State,Water}.ts`、`core/src/app/hub.rs`、`core/src/app/proto.rs`、`core/src/live/frame.rs`、`core/src/record/retain.rs`、`core/tests/app.rs`、`core/tests/bench/mod.rs`、`core/tests/frame.rs`、`core/tests/hub/mod.rs`、`core/tests/liveness.rs`、`core/tests/retain.rs`、`docs/EVENTS.md`、`ui/dev/mock.ts`、`ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`、`ui/src/lib/core.svelte.ts`、`ui/src/ui/{App,Home,Reader}.svelte`、`ui/tests/store.test.ts`、本报告。

## 证据

### 1. 新增与更新的测试

先在未改实现前验证新测试失败（例如 `i1_captions_do_not_resend_the_state` 因状态句变化重发 `State` 报 `left: 4, right: 3` 失败，`a_fifth_column_is_refused` 超时失败），实现后全部转绿：

- `core/tests/liveness.rs` 新增与更新：
  1. `i1_captions_do_not_resend_the_state`：稳定出字 60 虚拟秒期间，`outlet.state` 序号保持不变（重发 0 次），`outlet.frame` 序号增长 `185`（`1 -> 186`）。
  2. `a_stop_clears_the_session_and_no_late_status_brings_it_back`：注入 3 秒服务慢响应使录音任务在 `Stop` 后仍在收尾；`Stop` 当刻 `outlet.session` 为 `None`、`outlet.frame` 为空，继续推进 10 虚拟秒后两者序号均未再动、保持为空。
  3. `the_level_follows_the_sound_and_falls_to_nothing_when_paused`：说话段 `peak >= 7000`、`rms >= 6000`，句间静音回落，`Pause` 后立即归零且暂停 3 虚拟秒内序号不动，`Stop` 后归零。
  4. `a_recording_the_capture_ends_is_ended_by_the_recording_task`：话筒自行结束（`Notice::Ended`），`outlet.session` 变为 `None`，已落盘的现场草稿保留；日志有 1 条 `error` 级 `recording.end`（`code = "audio.closed"`），`notice.said` 与 `Outlet.said` 含同一代码。
  5. `a_recording_says_when_it_starts_how_it_goes_and_when_it_ends`：录 30 虚拟秒、暂停、继续、停止，`recording.start`（`source = "mic"`、`lanes = 1`、`columns = 2`、`captions = "live"`）1 条，`session.status` 依次为 `Connecting -> Recording -> Paused -> Recording`，`recording.end`（`code = "done"`）1 条，且均带第一场 `rec`；紧接第二场带第二个 `rec`。
  6. `nothing_said_or_secret_reaches_the_log`：更新为断言落盘日志包含 `recording.start` 与 `recording.end`，隐私检查全过。
  7. `without_a_key_the_bench_records_sound_and_no_caption`：补验 `recording.start`（`captions = "captions.no_key"`、`lanes = 0`）与唯一一条 `session.status`（`to = "AudioOnly"`、`why = "captions.no_key"`）。
- `core/tests/app.rs` 与 `core/tests/frame.rs` 新增与更新：
  8. `a_fifth_column_is_refused`：`Setup` 传 5 种字幕语言返回 `captions.columns` 提示，`state.setup` 保持原 4 种语言，`notice.said`（`code = "captions.columns"`）正好 1 条。
  9. `without_a_key_sound_is_kept_and_the_status_says_why`：补充断言 `recording.start`（`captions = "captions.no_key"`、`lanes = 0`）及稳定期间唯一一条 `session.status`（`to = "AudioOnly"`、`why = "captions.no_key"`）。
  10. `the_status_line_says_what_most_needs_saying`：补充 `Status::code()` 与 `Status::why()` 五种变体断言。
- `ui/tests/store.test.ts` 新增 2 条（共 6 条全绿）：
  11. `takes only a signal newer than the last of its own kind`：先收 `Frame(seq: 5)` 再收 `Frame(seq: 4)` 时 `store.live` 不变；随后 `Session(seq: 4)` 独立计数照收。
  12. `keeps the recording session and stats as signals untouched by State`：`Signal(Session)` 写入 `store.session` 且不被后续 `State` 覆盖；`Signal(Stats)` 写入 `store.stats`。

### 2. `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` 数据及与 `P1-11` 对比

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

| 指标 | `P1-11`（经 hub `Msg::Live`） | `P1-12`（录音任务直写寄存器） |
|---|---|---|
| 丢词数 | `0` | `0` |
| 每小时 p50 / p99 / max | `321 ms` / `321 ms` / `321 ms` | `321 ms` / `321 ms` / `321 ms` |
| 最大一帧 JSON 字节数 | `6167 B`（上界峰值 `7548 B`） | `6167 B`（`<= 8192 B`；上界峰值 `7548 B`） |
| 三语压力最大一帧字节数 | `8192 B`（`cut = 607`，`fit_events = 48`） | `8192 B`（`cut = 607`，`fit_events = 48`） |
| 捕获事件数 | `1452` | `1454`（含 `recording.start`、`session.status`、`recording.end`，且少 1 次 `state.sent`） |
| 真实耗时 | `72.07 s` | `57.49 s`（绕开 hub 邮箱后同场并发测试更快，低于 60 s 目标） |

### 3. 边界与静态规则检查

- `grep -rn "Msg::Live\|Msg::Status\|Event::Live" core app`：无结果。
- `core/src/app/hub.rs` 的 `sink` 与 `notice` 闭包中不再出现 `tx`。
- 全量门禁 `scripts/build/check.sh` 全绿：Rust `passed 553, failed 0, ignored 12`，`ui`：`npm run check`（0 errors, 0 warnings）与 `npm test`（29 passed，含 `words.test.ts` 与 `store.test.ts`）通过。

## 留下的，要负责人的

- hub 邮箱有界化与命令事件在 `P1-13`；写 tape 与写草稿独立队列在 `P1-15`；录音子树、状态机与 `Water` 采样在 `P1-16`；外壳 `pump` 无锁重写在 `P1-23`；界面层帧合并在 `P3`。
