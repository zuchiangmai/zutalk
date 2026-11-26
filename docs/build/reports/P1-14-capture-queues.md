状态：完成

# P1-14 采集：通道有上限，线程有名字，故障有事件：报告

## 改了什么

1. **采集里的三条无上限通道改为有名字、有上限的 `rt::queue::threads`，停止改靠原子标志（`core/src/capture/audio.rs`、`core/src/capture/audio/file.rs`、`core/src/capture/clock.rs`）**：
   - 新增带单位与理由的常量：`PUMP_MAIL_MSGS: usize = 64`、`ENDED_MSGS: usize = 2`、`FILE_MAIL_MSGS: usize = 8`、`RING_MS: u64 = 2_000`；设备环容量改按 `RING_MS` 计算（`rate * channels * RING_MS / 1_000`）。
   - 泵的邮箱改由 `queue::threads(stats, "capture.pump", PUMP_MAIL_MSGS, Full::Wait)` 构造：
     - 声源线程（`hold`）通过 `deliver` 用 `offer` 发送 `Msg::Feed` 与 `Msg::Notice`，遇到 `Back::Full` 每隔 `TICK_MS` 重试，看到 `over` 或 `Back::Closed` 立即退出，绝不无限期堵在发送上；
     - `Capture::pause` / `resume` 持 `tx.with(Full::Drop)` 端 `offer`，满时直接丢弃（`// dropped: a pump that takes no messages is seen by the Capture mark`）；
     - `Capture` 与 `Turner` 之间新增独立原子停止标志 `stop: Arc<AtomicBool>`：`Capture::drop` 置 `stop` 再 `offer(Msg::Stop)`（仅用于尽早叫醒等待中的泵），`Turner::turn` 每圈检查 `stop`，队列全满时也能立即收尾并交出完整块后退出。
   - 每个声源的 `ender` 改由 `queue::threads(stats, "capture.ended", ENDED_MSGS, Full::Drop)` 构造，cpal 错误回调与泵的 `rebuild` 分支一律 `offer`。
   - 读文件线程邮箱改由 `queue::threads(stats, "capture.file", FILE_MAIL_MSGS, Full::Drop)` 构造，`file::run` 改为收 `&ThreadRx<Msg>` 并每圈检查原子停止标志 `&AtomicBool`。
   - 设备的环（`rtrb`）封装为 `Ring`（含 `rtrb::Producer<f32>`、每声源 `Arc<AtomicU64>` 和 `Stats::counter("capture.ring.lost_samples")`）；`offer(&mut ring, samples)` 放不下时只做原子加，无锁、无分配、不发日志事件。
   - `Hear` 签名增加 `&Stats`，`Capture::start` / `with` / `by_hand` 接收 `stats: &Stats`；`core/src/app/hub.rs`（`Config::new`、`Task::listen`）、`core/tests/hub/mod.rs`、`core/tests/bench/mic.rs`、`core/tests/audio.rs` 同步更新。

2. **采集线程全部具名，由 `rt::supervise::thread` 启动（`core/src/capture/audio.rs`）**：
   - 三处 `thread::spawn` 全部换为 `rt::supervise::thread`：泵线程 `"capture.pump"`、读文件线程 `"capture.file"`、声源线程查表 `const SOURCE_THREADS: [&str; 2] = ["capture.source.0", "capture.source.1"]`（测试里传入第三个合成声源时回退到 `"capture.source.extra"`，避免越界）。
   - `Pump::step`、`wall_clock_policy`、cpal 设备层与 `hold` / `held` 重开和退避规则均未改动。

3. **采集事件与统计（`core/src/capture/audio.rs`、`core/src/capture/clock.rs`、`docs/EVENTS.md`）**：
   - `hold`：
     - 打开成功准备交给泵时发 `info!("capture.source.open", source = i, ms = open_ms)`（位于 `deliver(Msg::Feed)` 之前，保证泵附着完成时事件严格可见）；
     - 坏了要重开时发 `warn!("capture.source.broke", source = i, why = why, wait_ms = wait_ms)`，计数 `capture.reopens` 加一；
     - 致命结束（`End::Fatal`）发 `error!("capture.source.ended", source = i, why = why)`。
   - `Turner::turn`（`Pump::step` 外的薄壳；`Clock` 仅累计只读字段 `padded_ms`，不改规则）：
     - 时钟补静音后差值大于 0 时发 `warn!("capture.pad", ms = padded_delta)`，计数 `capture.padded_ms` 累加；
     - 时钟策略返回 `End`（`!alive`）发 `error!("capture.end", why = "asleep")`；
     - 收到暂停/继续发 `info!("capture.pause", paused = paused)`；
     - 交出音频块时累加计数 `capture.blocks`；
     - 每圈（以及切换 `Feed` 前）检查各声源环的原子丢样本计数，有增量则在泵线程上发 `warn!("capture.ring.lost", source = i, samples = delta)`。
   - `docs/EVENTS.md` 新增上述 7 条事件。

4. **全工作区 `clippy.toml` 与规则脚本收口（`clippy.toml`、`scripts/build/rules/bounded.sh`、`scripts/build/rules/spawn.sh`、`core/tests/live.rs`、`core/tests/audio.rs`）**：
   - 仓库根新增 `clippy.toml`，在 `disallowed-methods` 中禁掉 `tokio::sync::mpsc::unbounded_channel` 与 `std::sync::mpsc::channel`。
   - 测试同步收紧：`core/tests/live.rs` 的 `relay` 与 `real_soniox_hears_synthesized_speech_in_two_languages` 改为有上限 `mpsc::channel(4_096)`（`try_send().expect(...)`），`core/tests/audio.rs` 的 `import` 与 `manual` 改为 `mpsc::sync_channel(ROOM)`（`try_send().expect(...)`），全仓未加任何 `#[allow]`。
   - `scripts/build/rules/bounded.sh` 删掉 `core/src/capture/audio.rs 3 P1-14` 欠账行，并新增检查：若 `clippy.toml` 缺失或不含那两条禁用路径则以非零退出。
   - `scripts/build/rules/spawn.sh` 删掉 `core/src/capture/audio.rs 3 P1-14` 欠账行。

5. **设备回调零分配测试（`core/tests/callback.rs`）**：
   - 独立测试文件安装计数 `GlobalAlloc` 包装器（不增外部依赖），先自检可观测堆分配，再断言 `audio::offer` 正常写入与满环丢样本计数两条路径分配次数均为 0，并断言 `Counter::add`、`Peak::set`、`Spread::note`、`Gauge::set` 均为 0 次分配。

文件：`clippy.toml`、`core/src/app/hub.rs`、`core/src/capture/audio.rs`、`core/src/capture/audio/file.rs`、`core/src/capture/clock.rs`、`core/tests/audio.rs`、`core/tests/bench/mic.rs`、`core/tests/callback.rs`、`core/tests/hub/mod.rs`、`core/tests/live.rs`、`docs/EVENTS.md`、`scripts/build/rules/bounded.sh`、`scripts/build/rules/spawn.sh`、本报告。

## 事件字段说明与手工清单核查

- **少带字段说明（做法第 8 条）**：
  - `capture.pad` 只带 `ms`，不带 `source`（与 `P1.md` §3.13.3 一致）：`Pump::step` 统一推进各路时钟并由 `Turner` 读取总补齐毫秒增量，未为拆出 `source` 去改 `Pump::step` 分支。
  - `capture.end` 带 `why = "asleep"`：`Notice::Ended` 本身不携带原因代码（hub 在实时录音里将其映射为 `audio.closed`），在 `Turner::turn` 的 `!alive` 分支按约定记为 `why = "asleep"`。
  - 其余 5 条事件（`capture.source.open`、`capture.source.broke`、`capture.source.ended`、`capture.pause`、`capture.ring.lost`）均完整带齐 `P1.md` §3.13.3 规定的全部字段。
- **`core/tests/audio.rs` 开头手工清单是否需重看**：
  - 不需要。本次未改动 `Pump::step`、`wall_clock_policy`、cpal 设备层与 `hold`/`held` 重开和退避规则，仅替换内部通道、增加原子停止标志、线程具名启动与日志/计数上报，7 项真机手工清单行为不变。

## 证据

### 1. 新增测试与已有故障测试的「看得见」断言

先写 `core/tests/callback.rs` 与 `core/tests/audio.rs` 的新增/增强测试并运行确认改实现前编译失败，实现后全部通过：

- `core/tests/callback.rs`：`the_device_callback_allocates_nothing`（正常 `offer`、满环 `offer` 触发 `capture.ring.lost_samples` 计数、`Counter::add`、`Peak::set`、`Spread::note`、`Gauge::set` 全部 0 分配）。
- `core/tests/audio.rs` 新增：
  - `a_stop_reaches_a_pump_that_takes_no_messages`：不转泵连发 200 次 `pause`/`resume` 后丢弃 `Capture`，再转一圈返回 `false`；断言 `queue.full`（`queue = "capture.pump"`、`policy = "drop"`、`capacity = 64`）正好 1 条，统计行 `capture.pump` 的 `full == 137`、`lost == 137`（200 次暂停/继续 + `Drop` 里 1 次叫醒 `Stop` 共 201 次，前 64 次入队、后 137 次被退回）、`deepest == 64`。
  - `samples_the_ring_could_not_take_are_counted_and_said`：填满 16 kHz 单声道 2 秒环（32 000 样本）后再 `offer` 400 样本并转泵一圈，断言计数 `capture.ring.lost_samples == 400`、`capture.ring.lost`（`warn`、`source = 0`、`samples = 400`）正好 1 条。
- `core/tests/audio.rs` 已有 6 条故障测试补齐「看得见」断言（原有精确块数断言一处未松）：
  - `a_source_that_dies_is_reported_padded_and_opened_again`：`capture.source.open` 2 条（`source = 0`）、`capture.source.broke` 1 条（`why = "test.unplugged"`、`wait_ms = 250`）、`capture.pad` 至少 1 条且 `ms` 之和等于 `capture.padded_ms`、`capture.reopens == 1`、`capture.blocks == 17`。
  - `a_stall_the_pump_hears_ends_the_device_on_its_own_thread`：按时间顺序断言 `capture.source.open` → `capture.source.broke`（`why = "audio.stalled"`）→ `capture.source.open`，`capture.reopens == 1`、`capture.blocks == 72`。
  - `a_source_that_cannot_be_opened_is_fatal_and_the_other_one_carries_on`：`capture.source.ended` 1 条（`error`、`source = 1`、`why = "test.denied"`），`capture.source.open` 1 条（`source = 0`）。
  - `paused_the_clock_stands_still_and_resumed_it_goes_on`：`capture.pause` 2 条（`paused` 先 `true` 后 `false`）。
  - `a_closed_lid_ends_the_capture_out_loud`：`capture.end` 1 条（`error`、`why = "asleep"`）。
  - `a_device_that_hangs_in_the_system_holds_up_nothing`：`thread.start` 与 `thread.end`（`thread = "capture.pump"`、`code = "done"`）各 1 条；`stop()` 后排空即 `Disconnected`，无后续块到达。

### 2. 规则脚本与 `clippy.toml` 违规证明（均已还原）

1. 在 `core/src/lib.rs` 临时加入 `let _ = tokio::sync::mpsc::unbounded_channel::<u8>(); // dropped: 试`：

```text
=== PROOF 1A: cargo clippy ===
error: use of a disallowed method `tokio::sync::mpsc::unbounded_channel`
  --> core/src/lib.rs:18:13
   |
18 |     let _ = tokio::sync::mpsc::unbounded_channel::<u8>(); // dropped: 试
   |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: every queue has a bound, and says what happens when it is full (CLAUDE.md)
   = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.96.0/index.html#disallowed_methods
   = note: `-D clippy::disallowed-methods` implied by `-D warnings`
error: could not compile `zutalk-core` (lib) due to 1 previous error
clippy exit: 101

=== PROOF 1B: bounded.sh ===
core/src/lib.rs:18: channel built outside core/src/rt/queue.rs (unbounded_channel)
bounded.sh exit: 1

=== PROOF 1C: after revert ===
BOTH PASSED AFTER REVERT
```

2. 临时将 `clippy.toml` 改名后运行 `bounded.sh`：

```text
=== PROOF 2: rename clippy.toml ===
clippy.toml is missing: unbounded channels must stay banned
bounded.sh exit: 1
BOUNDED PASSED AFTER RESTORE
```

3. 其余 4 个工作区 crate（`zu`、`zutalk-log`、`zutalk-server`、`zutalk`）分别临时加入 `std::sync::mpsc::channel::<u8>()`，全部命中 `clippy.toml` 报错并随后还原：

```text
=== CHECKING zu (zu/src/lib.rs) ===
error: use of a disallowed method `std::sync::mpsc::channel`
   = note: unbounded: use rt::queue, and say what happens when it is full
=== CHECKING zutalk-log (log/src/lib.rs) ===
error: use of a disallowed method `std::sync::mpsc::channel`
    = note: unbounded: use rt::queue, and say what happens when it is full
=== CHECKING zutalk-server (server/src/lib.rs) ===
error: use of a disallowed method `std::sync::mpsc::channel`
    = note: unbounded: use rt::queue, and say what happens when it is full
=== CHECKING zutalk (app/src/main.rs) ===
error: use of a disallowed method `std::sync::mpsc::channel`
    = note: unbounded: use rt::queue, and say what happens when it is full
```

### 3. 压力测试（30 个 `yes` 空转进程、12 路并行 × 100 次）

- 采集组 9 条测试（全部多线程泵/声源/队列/事件测试）：
  ```text
  CAPTURE STRESS (debug, 9 capture tests, 30 spinners, 12x100): 1200 OK, 0 FAIL in 32s
  ```
- 采集组 9 条 + 读文件线程 4 条（全部 13 条涉及采集线程与邮箱的测试）：
  ```text
  CAPTURE+FILE STRESS (debug, 13 threaded audio tests, 30 spinners, 12x100): 1200 OK, 0 FAIL in 400s
  ```

### 4. 沙箱验证结果

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 警告）。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部退出码 0。
- `git grep -n "unbounded\|mpsc::channel()" -- core app server zu log ':!*.md'`：空（0 结果）。
- `git grep -n "thread::spawn\|mpsc::channel\|sync_channel" core/src/capture`：空（0 结果）。
- 定向 Rust 测试：
  - `audio`：29 passed, 0 failed, 4 ignored
  - `callback`：1 passed, 0 failed
  - `queue`：7 passed, 0 failed
  - `stats`：10 passed, 0 failed
  - `supervise`：21 passed, 0 failed
- `ui`：`npm run --silent check`（0 errors, 0 warnings）与 `npm test --silent`（5 files, 29 passed）。
- 完整门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh`：全绿（`rustfmt`、`clippy -D warnings`、全部 6 个规则脚本、`cargo test`：`passed 561, failed 0, ignored 12`、`ui`：`0 errors, 0 warnings`、`29 passed`）。

## 留下的，要负责人的

- 后续通道收口：tape 与草稿队列在 `P1-15`，音频 `broadcast` 与 lane 通道在 `P1-16`，`ai/write.rs` 与 `out/city.rs` 通道在 `P1-21`。
