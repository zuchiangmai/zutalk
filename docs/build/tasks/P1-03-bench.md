# P1-03 台架：在现在的代码上跑起来，让它失败

## 目标

做完以后：

1. 核心有三个端口（声音、实时服务的连接、tape 的写入口），生产行为不变。
2. `core/tests/bench/` 是一个在暂停的时钟上跑完整核心的台架：一个虚拟小时只花几秒真实时间，结果每次一样。
3. `core/tests/liveness.rs` 里有 I1、I2、I6、I7 的测试，都在**现在的代码**上跑过。通过的留着；失败的挂起（`#[ignore = "P1-NN：…"]`），报告里写明每一条现在怎么失败。失败的地方就是后面几件任务要改的地方。

`PLAN.md` §7：「先写测试：P1 的第一件事是让台架在现在的代码上跑起来，并且让它失败。」

## 背景

- 设计在 `docs/build/phases/P1.md` §2（量法）、§3.2（端口）、§3.10（台架）。先读这三节。
- `core/src/app/hub.rs`：`Config`（`hear: Hear` 现在是函数指针）；`Task::listen` 里直接 `tape::Writer::create`、`(self.config.hear)(…)`、为每条流 `tokio::spawn(live::run(…))`；`Core::start(config, out)` 的 `out` 回调收 `Event`。
- `core/src/live/lane.rs`：`open` 里 `connect_async(endpoint)`；`Conn.ws` 的类型是 `WebSocketStream<MaybeTlsStream<TcpStream>>`；`run` 的签名。
- `core/src/capture/audio.rs`：`Capture`（`pause`、`resume`、`Drop` 即停止）。`core/src/capture/tape.rs`：`Writer`。
- 现有的测试台：`core/tests/hub/mod.rs`（`signed_in`、`shelf`、`config`、`App`、整场转写服务的替身 `soniox`）、`core/tests/common/mod.rs`（旧版留下的资料夹 `old_library`）、`core/tests/live.rs`（lane 对着本机套接字上的脚本服务；`REAL_FRAMES` 是真服务发过的帧）。
- `docs/LESSONS.md`「新仓库里用真服务重新量到的」：静音时服务每 1.0–1.2 秒报一次进度；token 的 `end_ms − start_ms` 恒为 60 毫秒；47 秒连续讲话没有一个 `<end>`。服务的替身照这些做。
- 0.8 的回放夹具是真人的录音，不能进这个仓库。台架的话是编出来的。

## 做法

### 1. 三个端口

类型照 `P1.md` §3.2，一字不差（后面的任务都按这些名字写）。

- `capture/audio.rs`：`Hearing`、`Sink`、`Tell`、`Hear`；`impl Hearing for Capture`。
- `live/wire.rs`（新文件）：`Socket`、`Wire`、`Dial`、`dial()`。`lane::run` 多收一个 `dial: Dial`（或 `&Dial`）；`Conn.ws` 改成 `Wire`；「拨不通等多久」（`PATIENCE`）仍在 `lane.rs` 里包着。
- `capture/tape.rs`：`Reel`、`Reels`、`reels()`；`impl Reel for Writer`。
- `Config` 加 `dial`、`reels`，`hear` 换成新的 `Hear`；`Config::new` 给真的。`Task::listen` 改用这三个字段。
- 跟着改签名的调用处：`core/tests/live.rs` 的 `start`（传 `dial()`），`core/tests/hub/mod.rs` 和 `core/tests/app.rs` 的 `hum`（包成 `Arc`）。

别的生产代码不动。

### 2. 台架（`core/tests/bench/mod.rs`）

各部分照 `P1.md` §3.10：`Speech`（话）、`Mic`（话筒）、`Service`（服务）、`Tape`（带子，两种）、探针、`Bench`（把它们接到 `Core` 上）。补几条要紧的：

- **确定**：不用真的套接字、`std::thread::sleep`、随机数库。时间只用 `tokio::time`。话的时间表由一个写死的小算式生成（比如线性同余），每次一样。
- **话筒**把块的序号写在每块头 6 个样本里（64 进制，每个样本 0–63）。服务的替身解出序号，就知道收到的是采集钟上的哪 100 毫秒；重连、跳过以后也对得上。
- **服务**的延迟是参数，默认：音频到了 300 虚拟毫秒后回猜的词，700 毫秒后回定下的词，句末 400 毫秒后回 `<end>`；token 的时间在**这条连接自己的钟**上（从 0 数它收到的音频），和真服务一样。
- **带子**的闸等人放开时有一个真实时间的上限（10 秒），到了就 panic：现在的代码在话筒的任务上直接写 tape，没有这个上限测试会挂死，而不是失败。闸放在守卫里，测试怎么结束它都会被放开。
- **话筒**除了按钟交块，还能「一口气交 n 块」（采集的钟是它自己的）。
- **探针**对外只给「每个词第一次出现的虚拟时间」「每一帧」「现在的状态句」「说过的代码」。它现在读 `out` 回调里的 `Event::Live` 和 `State.session.status`；P1-06 会把它改成读 `Outlet`。所以把「从哪里读」挡在台架的一两个函数后面，测试本身不碰 `Event`。
- 收事件的通道用有上限的（比如 1024），满了 panic。
- 钥匙库是进程里的一个：这个文件里的测试用一把静态的 `tokio::sync::Mutex` 排队（照 `hub/mod.rs` 的 `STORE`）。登录的人、自带的 Soniox 钥匙照 `hub::signed_in()`。

### 3. 测试（`core/tests/liveness.rs`）

| 测试 | 断言 | 时钟 |
|---|---|---|
| `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first` | 两种语言（一条 `TwoWay` 流），8 虚拟小时。按小时分段：每段 p99（词说完 → 第一次出现在帧里）≤ 1000 ms；第 8 段 p99 ≤ 第 1 段 + 100 ms；除了注入的故障没有丢词；每一帧序列化成 JSON ≤ 8192 字节 | 暂停 |
| `i1_three_languages_fit_a_frame` | 三种语言（两条流），话里有 150 字的中文长行和 24 秒不停的长句，30 虚拟分钟；同样的延迟和字节断言 | 暂停 |
| `i2_a_tape_that_stands_still_holds_no_caption_back` | 录了一秒以后把带子的闸关上：之后再来 15 帧，关着的时候说的词也上了屏；放开、停止，tape 读回来一块不少。另起一场：关上闸，话筒一口气交 700 块（超过队列的 600）：录音以 `tape` 结束，草稿留着 | 真实，多线程 |
| `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` | 导出到一个没人读的 FIFO（`mkfifo`，仅 unix）。导出卡着的时候：再来 20 帧；`Pause` 后状态句变成已暂停，`Resume` 后变回来；改标题再打开，读得到新标题。然后打开 FIFO 的读端，导出做完。**FIFO 的读端放在一个守卫里，`Drop` 时一定打开**：不然现在的代码会让运行时关不掉，测试挂死。FIFO 的名字要带导出器的扩展名（`.md`）：没有扩展名的路径会被补上一个，写到别的文件里去 | 真实，多线程 |
| `i2_background_work_runs_beside_a_recording` | 录音的同时：搜一个有几千行的资料库、搬旧版的夹具资料库、对着整场转写的替身写一条别的录音的记录。三件都做完；录音的草稿里话的每一句都在，tape 的长度对 | 真实，多线程 |
| `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed` | 第 20 秒让服务的替身在 lane 的任务里 panic（`Wire` 的 `poll_next` 里）。5 虚拟秒内新的词又上屏；草稿里有一行空缺（`…`，不完整）盖住没听到的那一段；崩之前说完的行都在 | 暂停 |
| `i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence` | 第 20 秒让带子的 `append` panic 一次。录音没有结束；停止以后用 `tape::read` 读回来：长度等于录音的长度，丢的那一块是静音，前后的块原样 | 暂停 |
| `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection` | 服务收音频但一个字不回。20 虚拟秒内 lane 换了一条连接，词又上屏；草稿里有空缺 | 暂停 |
| `i8_what_one_change_sends_does_not_grow_with_the_library` | 资料库里 5 条和 100 条录音两种情况下，改一条的标题，量随后发出的 `State` 的字节数：两者之比 ≤ 1.2。直接标 `#[ignore = "P2：状态瘦身"]` | 真实 |

「词说完」的时刻 = 录音开始的虚拟时刻 + 这个词在采集钟上的结束时间。每小时的样本排序取分位数就行，不加依赖。

量「每帧 ≤ 8192 字节」不要每帧都序列化（8 小时有近 30 万帧）：先算一个便宜的上界（各段文字的字节数 + `units` 的个数 × 6 + 固定开销），只有上界创了新高才真的序列化一次。

### 4. 在现在的代码上跑

每一条都跑（挂起的用 `-- --ignored` 单独跑），把结果写进报告：通过，或者怎么失败（断言的输出）。然后：

- 通过的：不挂起，留在门禁里。
- 失败的：`#[ignore = "P1-NN：<它等什么>"]`。让它通过的任务，预计是：`i1_*` → P1-05（一帧有了上限）；`i2_a_tape_…` → P1-09；`i2_a_job_…` → P1-12；`i6_a_lane_…` → P1-04（`supervise` 接住 panic）；`i6_a_tape_writer_…` → P1-09。预计只是预计：以跑出来的为准；哪一条失败的原因和预计的不一样，报告里写清楚，并把 `NN` 指到真正该修它的任务（见 `P1.md` §5 的表）。
- 一条预计会过的没过，而且原因是现在代码的毛病：照样挂起、写明、指到该修的任务。原因是台架自己的毛病：修台架。

### 5. 8 小时那一条的耗时

它要留在门禁里。目标是真实耗时 ≤ 60 秒（门禁现在全绿约 40 秒）。超过了，按这个次序想办法，每一步量一次：

1. 台架自己的开销：响的块和静的块各预先造一份，每块只改头 6 个样本；帧不重复序列化；探针按词的次序往前找，不回头。
2. 只给依赖开优化：根 `Cargo.toml` 加 `[profile.dev.package."*"] opt-level = 1`（首次编译会慢一些，量一下门禁的总时间）。
3. 还超过：小时数不减，照实把耗时写进报告，交给审查。

## 先写的测试

就是上面那张表。次序：先写话和服务的替身，并给它们自己写两条小测试（话的时间表每次一样；服务对着一段脚本回的帧能被 `lane::frame` 读成预期的 token），再写 `i1_three_languages_fit_a_frame`（最短的一条，用来把台架调通），然后其余的。

## 验收

- `scripts/build/check.sh` 全绿。
- 报告里有：每条测试在现在代码上的结果；`i1_eight_hours_…` 的每小时 p50、p99、最大值和最大的一帧多少字节（哪怕它被挂起，也用 `--ignored` 跑出这些数）；8 小时那条的真实耗时；门禁的总耗时（改之前、改之后）。
- 同一条 `i1_*` 连跑三次，报出的数字完全一样（证明台架是确定的）。
- 端口换了以后，原有的测试除了签名以外一行没改，全部通过。

## 不做

- 不修任何一条失败的测试所指的毛病：那是后面的任务。
- 不加 `turmoil`、`hdrhistogram`、随机数库。
- 不把真人的录音或 0.8 的夹具音频搬进来。
- 不写 `i7_offline_…`、`i7_a_capture_that_stops_…`：它们要用的东西还不存在（P1-11 写）。
- 不改 `Core::start` 的签名（P1-06 改）。
