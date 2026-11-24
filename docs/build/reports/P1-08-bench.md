状态：完成

# P1-08 台架：在现在的代码上跑起来，让它失败：报告

## 改了什么

1. **三个替身端口（生产行为零改动）**：
   - `core/src/capture/audio.rs`：新增 `Hearing` trait（`pause`、`resume`，`Drop` 停止采集）、`Sink`、`Tell`、`Hear`（`Arc<dyn Fn(...)>`），并为真实 `Capture` 实现 `Hearing`。
   - `core/src/live/wire.rs`（新文件）：新增 `BoxFuture`、`Socket`、`Wire`、`Dial` 与真实 `dial()`（`tokio_tungstenite::connect_async`）；`core/src/live/lane.rs` 的 `Conn.ws` 改为 `Wire`，`lane::run` 收 `Dial`，`PATIENCE_MS` 连接时限仍包在 `lane::open` 内。
   - `core/src/capture/tape.rs`：新增 `Reel` trait、`Reels` 与真实 `reels()`（`Writer::create`），读取仍只走 `tape::Pass`。
   - `core/src/app/hub.rs`：`Config` 新增 `dial`、`reels`，`hear` 升级为新 `Hear`，各字段带注释，`Config::new` 提供真实端口，`Task::listen` 改经三个端口创建采集、连接与带子。原有测试仅同步签名包装（`core/tests/live.rs`、`core/tests/hub/mod.rs`、`core/tests/app.rs`），断言未改。
   - 根 `Cargo.toml`：按步骤 2 增 `[profile.dev.package."*"] opt-level = 1`，为依赖开启一级优化以压低 8 小时模拟耗时。

2. **全核心模拟台架（`core/tests/bench/`）**：
   - `speech.rs`：无随机库确定性线性同余生成时间表（词标记 `w000001` / `词000001` 互不重复、普通句间停顿、每 10 句一段 24 秒不间断长句、三语模式下含 ≥ 150 字中文长行）。
   - `mic.rs`：基于 `tokio::time` 的话筒替身，预构造响/静两份 PCM 模板，每块头 6 个样本以 64 进制写入块序号（RMS < 4 不影响静音检测），半帧相位偏移（50 ms），支持 `pause`、`resume`、`burst(n)`、`stall`、`end`。
   - `service.rs`：跑在 `tokio::io::duplex` 内存流上的真实 RFC 6455 WebSocket 服务端替身，解出块序号，按该连接自己的音频钟与可配延迟（默认猜词 300 ms、定稿 700 ms、句末 `<end>` 400 ms、静音进度 1100 ms，连接级 1 ms 错峰避免 `tokio::select!` 同毫秒随机平局）返回 token、译文、`<end>`、`<fin>`、`finished`，支持数据化故障（`Silent`、`PanicAt`、`HangUpAt`、`ErrorAt`、`Slow`）。
   - `tape.rs`：两种带子替身——`CountingTape`（仅计数，供 8 小时测试免磁盘加密开销）与 `GatedTape`（包装真实 `tape::Writer`，支持 10 秒真实时间上限的守卫闸门与单次 `append` panic 注入）。
   - `mod.rs`：`Bench` 封装未登录本机身份（`hub::nobody()`，收尾校验 `State.me.is_none()`，支持无钥匙 `with_key = false`）、有上限协议通道（1024，满则 panic）、`zutalk_log::capture(Level::Info)` 日志捕获（收尾校验 `refused == 0`、`lost == 0`）、`core.stats().tally()` 读取，以及把协议读取隔离在探针方法后的延迟分位数与便宜帧上界计算。

3. **活性与隐私测试（`core/tests/liveness.rs`）**：
   - 写出全部要求的自测、隐私、无钥匙、I1、I2、I6、I7、I8 测试，在当前代码上逐条实跑记录结果；已通过的 7 条留在门禁中，尚待后续任务修复或补事件的 9 条挂上对应 `#[ignore = "P1-NN：…"]` / `#[ignore = "P2：…"]`。

文件：`Cargo.toml`、`core/src/capture/audio.rs`、`core/src/capture/tape.rs`、`core/src/live.rs`、`core/src/live/wire.rs`、`core/src/live/lane.rs`、`core/src/app/hub.rs`、`core/tests/live.rs`、`core/tests/hub/mod.rs`、`core/tests/app.rs`、`core/tests/bench/{mod,speech,mic,service,tape}.rs`、`core/tests/liveness.rs`、本报告。

## 每条测试在当前代码上的实跑结果

| 测试 | 当前代码结果 | 留在门禁 / 挂起到哪件任务 | 失败输出或通过说明 |
|---|---|---|---|
| `speech_schedule_is_deterministic` | 通过 | 留在门禁 | 两次生成完全相等，含 24 秒连续长句与 ≥ 150 字中文长行 |
| `fake_service_frames_parse_as_expected_tokens` | 通过 | 留在门禁 | 猜词、定稿+译文、静音进度、`<fin>`、`finished` 均经 `lane::frame` 解析通过 |
| `the_search_for_leaks_finds_one` | 通过 | 留在门禁 | 含词、含钥匙、含 `"!"`、含嵌套对象的 4 行均检出报错，干净行通过 |
| `without_a_key_the_bench_records_sound_and_no_caption` | 通过 | 留在门禁 | 状态保持 `AudioOnly(captions.no_key)`，0 字幕栏帧，0 次拨号，300 块全部写入带子 |
| `nothing_said_or_secret_reaches_the_log` | 通过 | 留在门禁 | 真实落盘 `zutalk_log::files(Debug)` 录 2 虚拟分钟、改标题、搜索，零泄露（lane panic 注入按任务说明留注释给 `P1-09` 补回） |
| `i1_three_languages_fit_a_frame` | **失败** | `#[ignore = "P1-10：一帧字节上限"]` | 延迟 `p50=321ms, p99=321ms, max=321ms`，零丢词，但 `View` 每栏保留 12 行且每行带完整 `units`，`max frame 18496 bytes > 8192 bytes` |
| `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` | **失败** | `#[ignore = "P1-10：一帧字节上限"]` | 8 小时每段 `p50=320ms, p99=320ms, max=320ms`，零丢词、零漂移，但 `max frame 15144 bytes > 8192 bytes` |
| `i2_a_tape_that_stands_still_holds_no_caption_back` | **失败** | `#[ignore = "P1-15：写 tape 独立队列"]` | `sink` 在话筒任务上同步 `tape.append`：`captions stalled while tape gate was closed: "timed out waiting for 15 frames (got 0 of 15)"` |
| `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` | **失败** | `#[ignore = "P1-19：hub 不阻塞在导出上"]` | `Export` 在 hub 任务上同步打开无读端 FIFO，堵死 hub 邮箱（`Msg::Live` 亦经 hub）：`export blocked live captions: "timed out waiting for 20 frames (got 0 of 20)"` |
| `i2_background_work_runs_beside_a_recording` | 通过 | 留在门禁 | 搜索 2000 行、搬运 0.8 三条夹具、整场转写一条草稿与现场录音并发完成，草稿句子齐全、带子长度精确 |
| `i2_background_work_runs_beside_a_recording_and_it_shows` | **失败** | `#[ignore = "P1-21：后台任务的 job.done 事件"]` | 恢复/并发部分通过，但当前代码尚未发 `job.done` 事件：`expected job.done(search, ok)` |
| `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed` | **失败** | `#[ignore = "P1-09：supervise 接住 panic 并重开"]` | 第 20 秒注入 `Wire::poll_next` panic 后裸 `tokio::spawn` 直接退出，`supervise` 未接 panic：`no word appeared after injected lane panic at 20 s` |
| `i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` | **失败** | `#[ignore = "P1-15：写 tape 的部件自愈并补静音"]` | 第 20 秒（200 块）`Writer::append` panic 直接炸毁话筒回调任务：`mic must keep delivering across 30 s, got 200 blocks` |
| `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` | 通过 | 留在门禁 | `lane.rs` 现有 `BEHIND_MS = 15_000` 在 15.1 秒丢弃静默连接并重连，20 秒内新词上屏且草稿留下 `…` 空缺行 |
| `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection_and_it_shows` | **失败** | `#[ignore = "P1-17：看守判断停滞并发出事件与快照"]` | 重连恢复已通过，但看守尚未建立：`expected Heard(0) verdicts Slow -> Stalled -> Live, saw []` |
| `i8_what_one_change_sends_does_not_grow_with_library` | **失败** | `#[ignore = "P2：状态瘦身"]` | 5 条资料库时 `State` 为 `1240 B`，100 条时为 `15580 B`，`ratio = 12.56 > 1.2` |

## 故障测试「看得见」断言的当前状态

| 故障测试 | 看得见断言内容 | 当前状态 |
|---|---|---|
| `i2_a_tape_that_stands_still_…` | 前半无 `queue.full` 且 `tape` 队列 `deepest > 0`；后半 `queue.full(tape, end)`、`recording.end(tape)` 与 `tape` 提示 | 随整条测试挂起至 `P1-15` |
| `i2_a_job_that_never_ends_…` | 堵住时有 `job.start(export.write)` 无 `job.done`、`command.done(Pause/Resume/Change, ok)`；放开后 `job.done(export.write, ok)` | 随整条测试挂起至 `P1-19` |
| `i2_background_work_…_and_it_shows` | `job.done`（`search`、`job.move`、`job.write` 均 `ok`），且无 `judge.verdict(to = Stalled)` | 拆出 `_and_it_shows` 挂起至 `P1-21`（并发恢复本尊留在门禁） |
| `i6_a_lane_that_dies_…` | `part.state(lane.0, Waiting, panic)`、`lane.link(Live)`、`lane.gap(panic)`、计数 `part.restarts == 1` | 随整条测试挂起至 `P1-09`（`P1-09` 接通 `part.state` 并按 `P1-09-rt-values.md` 将 `lane.link`/`lane.gap` 与计数分别交接给 `P1-10`、`P1-16`） |
| `i6_a_tape_writer_that_dies_…` | `part.state(tape, panic)`、`keep.lost(tape)`、`tape.pad`、计数 `keep.lost_batches == 1`、无失败 `recording.end` | 随整条测试挂起至 `P1-15` |
| `i7_a_service_that_takes_audio_…_and_it_shows` | `judge.verdict(Heard, 0)` 历经 `Slow -> Stalled(silent, reopen) -> Live`、`stall.snapshot == 1`、`lane.kick == 1`、`part.state(silent)` | 拆出 `_and_it_shows` 挂起至 `P1-17`（恢复本尊留在门禁） |

## 测量数据

### 1. `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`（8 虚拟小时）

| 小时段 | 词数 | p50 | p99 | max |
|---|---|---|---|---|
| 第 1 小时 | 7241 | 320 ms | 320 ms | 320 ms |
| 第 2 小时 | 7183 | 320 ms | 320 ms | 320 ms |
| 第 3 小时 | 7199 | 320 ms | 320 ms | 320 ms |
| 第 4 小时 | 7213 | 320 ms | 320 ms | 320 ms |
| 第 5 小时 | 7173 | 320 ms | 320 ms | 320 ms |
| 第 6 小时 | 7172 | 320 ms | 320 ms | 320 ms |
| 第 7 小时 | 7158 | 320 ms | 320 ms | 320 ms |
| 第 8 小时 | 7148 | 320 ms | 320 ms | 320 ms |

- **丢词数**：`0`
- **最大一帧 JSON 字节数**：`15144 B`（超过 `8192 B` 上限，待 `P1-10` `Composer` + `fit` 收敛）
- **捕获到的日志事件总数**：`486` 条（8 小时 = 480 分钟，即 480 条每分钟 `stats.summary` + 启动时 6 条 `task.start` / `stat.*` / `identity.who`）
- **真实耗时**：`64.82 s`（已开启 `[profile.dev.package."*"] opt-level = 1`、预构造 PCM 模板与便宜帧上界；因当前 `View::columns()` 每 100 ms 对 2 栏 × 12 行重跑一遍 `units` 切分，约占大头开销，`P1-10` 改为每行只切一次后将显著下降）

### 2. 确定性验证：`i1_three_languages_fit_a_frame` 连跑三次

```text
run 1: i1_three_languages: words=3849 missing=0 p50=321ms p99=321ms max=321ms max_frame=18496B events=36
run 2: i1_three_languages: words=3849 missing=0 p50=321ms p99=321ms max=321ms max_frame=18496B events=36
run 3: i1_three_languages: words=3849 missing=0 p50=321ms p99=321ms max=321ms max_frame=18496B events=36
```

三次的词数、缺失数、分位数、最大帧字节数与事件条数完全一致。

### 3. 隐私测试落盘日志统计（`nothing_said_or_secret_reaches_the_log`）

- **落盘总行数**：`8` 行
- **出现的事件代码集合**：`{"identity.who", "stat.count", "stat.peak", "stats.summary", "task.start"}`
- **泄露与格式检查**：全部通过 `is_plain`，无 `"!"`，未出现任何原话词、译文词、标题、搜索词、说话人标签、本机 `did:key`、服务钥匙或数据目录路径。

### 4. 门禁总耗时对比

- 改之前（`P1-07` 增量跑 `check.sh`）：约 `55 s`
- 改之后（`P1-08` 增量跑 `check.sh`，含留在门禁里的 7 条 `liveness.rs` 测试）：约 `65 s`（`i1_eight_hours` 在 `P1-10` 解除挂起进入门禁）

## 留下的，要负责人的

- `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` 当前因 `max_frame = 15144 B > 8192 B` 挂起至 `P1-10`；其当前真实耗时为 `64.82 s`（略超 60 s 目标约 4.8 s，主要耗时在旧 `View::columns()` 每帧重复切词，`P1-10` 改为只切一次后再复测）。
- 隐私测试中的「中途让 lane 崩一次」按任务说明留了注释，待 `P1-09` 让 `supervise` 接住 panic 后打开。
- 无新增需要负责人拍板的问题。
