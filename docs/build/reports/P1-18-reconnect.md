状态：完成

# P1-18 连不上不是无限重试：时限、「停了」、重连：报告

## 改了什么

1. **取钥匙时限、每次建连日志与统计、重连起点（`core/src/live/lane.rs`）**：
   - `open()` 用 `timeout(Duration::from_millis(PATIENCE_MS), lane.keys.lease(Service::Soniox))` 包裹取钥匙；超时返回 `End::Retry("keys")`。`PATIENCE_MS` 注释补明取钥匙、拨号、写一帧共用此耐心值。
   - `open()` 在唯一出口发出 `lane.open`（`lane`、`code`、`keys_ms`、`dial_ms`；`code == "ok"` 用 `info!`，失败用 `warn!`），并用 `tokio::time::Instant` 记录等待时长到分布 `lane.keys_ms`、`lane.dial_ms`（未走到拨号时 `dial_ms = 0`）。
   - 新增 `Origin { edge, why }` 与 `run_from(...)`（`run(...)` 委托至 `Origin::default()`），供重连时从旧 `Sent` 水位接着报出盖住整段离线时间的空缺。到达 `Restarts` 上限返回 `End::Fatal(why)` 时发出 `Link::Stopped(why)` 并在 `Sent(n)` 水位记下 `ended(why)`。

2. **重连命令与录音重起停掉的 lane（`core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/recording.rs`、`core/src/rt/water.rs`）**：
   - `Command::Reconnect`（`kind() == "Reconnect"`、`needs_person() == false`），重新生成 `core/bindings/Command.ts`。
   - `Hub` 持有录音任务的 `reconnect: watch::Sender<u64>`；未在录音时拒绝并返回 `recording.none`；录音中通知录音任务。
   - 录音状态机新增 `Happened::Reconnect` / `Effect::Reconnect`：仅在 `Recording`、`Paused` 生效，`Opening`、`Closing(_)`、`Sealing`、`Ended(_)` 忽略且无副作用。
   - `Effect::Reconnect` 仅重新起 `ended.is_some()` 的 lane，仍按原名 `LANE_NAMES[n]` 挂在 `recording` 子树上，清掉 `Sent(n)` 的 `ended`，从旧 `Sent` 位置带原停机原因进入 `lane::run_from`，发 `info!("lane.reconnect", lane)`。无停掉的 lane 或无字幕场（文件、未选语言、`captions.no_key`）为空操作，命令仍返 `ok`。
   - `water::sample` 在某条 lane 的 `Sent(n)` 从 `ended.is_some()` 变为 `ended.is_none()` 时刷新该 lane 各格的 `moved_ms = now_ms`，避免离线时长在重连后第一拍误算为停滞。

3. **`reqwest` 建连与读超时（`core/src/app/hub.rs`）**：
   - `Core::start` 的 `reqwest::Client::builder()` 增加 `connect_timeout(CONNECT_TIMEOUT_MS)`（`10_000` ms，与 lane `PATIENCE_MS` 一致）与 `read_timeout(READ_TIMEOUT_MS)`（`30_000` ms，静默 30 秒判定死连接，同时不设整请求 `timeout` 以免打断长录音上传与交城）。

4. **最小界面与多语言（`ui/src/ui/Live.svelte`、`ui/dev/mock.ts`、`ui/lang/*.json`）**：
   - `Live.svelte` 在任意一栏 `health.is === 'Dead'` 时于状态句旁展示「重连」按钮并发送 `{ do: 'Reconnect' }`。
   - 8 份 `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json` 全部补齐 `code.keys` 与 `reconnect`；`ui/dev/mock.ts` 增加 `Reconnect` 处理器。

5. **事件词典（`docs/EVENTS.md`）**：
   - 补齐 `lane.open`、`lane.reconnect` 两行。

改动文件：`core/src/live/lane.rs`、`core/src/rt/water.rs`、`core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/recording.rs`、`core/bindings/Command.ts`、`docs/EVENTS.md`、`ui/src/ui/Live.svelte`、`ui/dev/mock.ts`、`ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`、`core/tests/bench/service.rs`、`core/tests/recording.rs`、`core/tests/live.rs`、`core/tests/app.rs`、`core/tests/liveness.rs`、`docs/build/reports/P1-16-recording-tree.md`、`docs/build/reports/P1-17-watch.md`、本报告。

## 证据

### 新增与更新的测试（全部通过）

- `core/tests/liveness.rs`：
  1. `i7_offline_is_said_and_captions_stop_trying_until_asked`（断网 20 s 报 `Stalled(connect)` 与一次 `connect` 提示、24 s 第 8 次失败后进入 `Dead(connect)`、随后 5 虚拟分钟不再拨号/不再发 `lane.open` 与 `part.state`、恢复并 `Reconnect` 后 5 s 内出字、草稿含覆盖离线时段的 `…` 空缺行、全部可观测字段与 `judge.dead == 1` 通过）
- `core/tests/live.rs`：
  2. `a_lease_that_never_comes_is_given_up_on`（永不返回的 `NeverKeys`：10 s 时 `because == "keys"`，121.75 s 经历 9 次退避后以 `End::Fatal("keys")` / `Stopped("keys")` 结束，全程 0 次拨号，`lane.open` 9 条且 `keys_ms = 10000, dial_ms = 0`，`part.gave_up` 1 条，`lane.keys_ms.n == 9`）
  3. `every_open_says_how_it_went`（正常连一次 `code = "ok"`、`info`，被拒钥匙一次 `code = "key"`、`dial_ms = 0`、`warn`）
- `core/tests/app.rs`：
  4. `reconnect_with_nothing_recording_says_so`（空闲时 `Reconnect` 报 `recording.none`，`command.done(kind = "Reconnect", code = "recording.none")`）
  5. `reconnect_starts_nothing_for_a_recording_that_never_had_captions`（无钥匙场中途 `OwnKey` + `Reconnect` 返 `ok` 但 0 条 `lane.reconnect` / `lane.open` 且状态保持 `AudioOnly { why: "captions.no_key" }`；停后再 `Start` 新场 `recording.start` 为 `captions = "live"`）
  6. `commands_that_need_a_person_are_listed_in_one_table`（补入 `Command::Reconnect`）
- `core/tests/recording.rs`：
  7. `a_recording_steps_as_its_table_says`（转换表补 `Happened::Reconnect` 全状态组合：`Recording`/`Paused` 触发 `Effect::Reconnect`，`Closing` 以后不改状态、无效果）

### `lease(` 位于 `timeout` 内的核对

```text
$ grep -n "lease(" core/src/live/lane.rs
458:        timeout(Duration::from_millis(PATIENCE_MS), lane.keys.lease(Service::Soniox)).await;
```

### 测试 #1 三个时刻的 `Water`（JSON）

1. **说出 `connect` 的那一秒（20 s，`Sent(0)` 进入 `Stalled`）**：

```json
{"now_ms":20000,"clock_ms":19850,"paused":false,"marks":[{"stage":"Capture","at":19900,"moved_ms":20000,"why":"","ended":null},{"stage":"Tape","at":19900,"moved_ms":20000,"why":"","ended":null},{"stage":"Draft","at":0,"moved_ms":0,"why":"","ended":null},{"stage":"Frame","at":8,"moved_ms":16000,"why":"","ended":null},{"stage":{"Sent":0},"at":0,"moved_ms":0,"why":"connect","ended":null},{"stage":{"Heard":0},"at":0,"moved_ms":0,"why":"","ended":null},{"stage":{"Final":0},"at":0,"moved_ms":0,"why":"","ended":null}],"health":[["Capture",{"is":"Live"}],["Tape",{"is":"Live"}],["Draft",{"is":"Live"}],["Frame",{"is":"Live"}],[{"Sent":0},{"is":"Stalled","why":"connect"}],[{"Heard":0},{"is":"Live"}],[{"Final":0},{"is":"Live"}]]}
```

2. **变成 `Dead` 的那一秒（24 s，第 8 次失败后放弃，`Sent(0).ended == "connect"`）**：

```json
{"now_ms":24000,"clock_ms":23850,"paused":false,"marks":[{"stage":"Capture","at":23900,"moved_ms":24000,"why":"","ended":null},{"stage":"Tape","at":23900,"moved_ms":24000,"why":"","ended":null},{"stage":"Draft","at":0,"moved_ms":0,"why":"","ended":null},{"stage":"Frame","at":10,"moved_ms":24000,"why":"","ended":null},{"stage":{"Sent":0},"at":0,"moved_ms":0,"why":"connect","ended":"connect"},{"stage":{"Heard":0},"at":0,"moved_ms":0,"why":"","ended":null},{"stage":{"Final":0},"at":0,"moved_ms":0,"why":"","ended":null}],"health":[["Capture",{"is":"Live"}],["Tape",{"is":"Live"}],["Draft",{"is":"Live"}],["Frame",{"is":"Live"}],[{"Sent":0},{"is":"Dead","why":"connect"}],[{"Heard":0},{"is":"Live"}],[{"Final":0},{"is":"Live"}]]}
```

3. **重连成功后的那一秒（325 s，`Sent(0)`、`Heard(0)` 回到 `Live`，`Draft` 与 `Frame` 推进）**：

```json
{"now_ms":325000,"clock_ms":324850,"paused":false,"marks":[{"stage":"Capture","at":324900,"moved_ms":325000,"why":"","ended":null},{"stage":"Tape","at":324900,"moved_ms":325000,"why":"","ended":null},{"stage":"Draft","at":324000,"moved_ms":325000,"why":"","ended":null},{"stage":"Frame","at":15,"moved_ms":325000,"why":"","ended":null},{"stage":{"Sent":0},"at":324900,"moved_ms":325000,"why":"","ended":null},{"stage":{"Heard":0},"at":324100,"moved_ms":325000,"why":"","ended":null},{"stage":{"Final":0},"at":0,"moved_ms":325000,"why":"","ended":null}],"health":[["Capture",{"is":"Live"}],["Tape",{"is":"Live"}],["Draft",{"is":"Live"}],["Frame",{"is":"Live"}],[{"Sent":0},{"is":"Live"}],[{"Heard":0},{"is":"Live"}],[{"Final":0},{"is":"Slow"}]]}
```

### 「0.8 那次事故在日志里的样子」（来自测试 #2 `a_lease_that_never_comes_is_given_up_on` 真实捕获）

```text
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=250 n=1
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=2
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=500 n=2
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=3
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=1000 n=3
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=4
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=2000 n=4
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=5
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=4000 n=5
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=6
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=8000 n=6
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=7
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=8000 n=7
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=8
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
warn  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Waiting" why="keys" wait_ms=8000 n=8
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Waiting" to="Opening" why="" wait_ms=0 n=9
warn  zutalk_core::live::lane     lane.open    lane=0 code="keys" keys_ms=10000 dial_ms=0
info  zutalk_core::rt::supervise  part.state   part="lane.0" from="Opening" to="Ended" why="keys" wait_ms=0 n=9
error zutalk_core::rt::supervise  part.gave_up part="lane.0" why="keys" n=9
warn  zutalk_core::live::lane     lane.link    lane=0 from="Connecting" to="Stopped" why="keys"
```

在完整 `Core` 录音中，看守于第 20 秒同步产出 `judge.verdict`（`Sent(0)` -> `Stalled`，`why = "keys"`，`action = "tell"`）与 1 组 `stall.snapshot`；到上限时产出 `judge.verdict`（`Sent(0)` -> `Dead`，`why = "keys"`），不再复拍现场。

### 8 小时 I1 实测数据（Batch 1 收尾单次运行）

```text
i1_eight_hours h1: words=7241 p50=321ms p99=321ms max=321ms
i1_eight_hours h2: words=7183 p50=321ms p99=321ms max=321ms
i1_eight_hours h3: words=7199 p50=321ms p99=321ms max=321ms
i1_eight_hours h4: words=7213 p50=321ms p99=321ms max=321ms
i1_eight_hours h5: words=7173 p50=321ms p99=321ms max=321ms
i1_eight_hours h6: words=7172 p50=321ms p99=321ms max=321ms
i1_eight_hours h7: words=7158 p50=321ms p99=321ms max=321ms
i1_eight_hours h8: words=7148 p50=321ms p99=321ms max=321ms
i1_eight_hours total: missing=0 max_frame=6136B bound_most=7518B caption.lag_ms=(n=57487 p50=500ms p99=500ms most=320ms) judge.verdict=0 stall.snapshot=0 events=5804 wall=93.93s
```

- 8 小时共 57,487 词，0 丢失，h1–h8 延迟 p50/p99/max 均为 `321 ms`（零漂移）；
- `caption.lag_ms` 统计：`n = 57487`，`p50 = 500 ms`（`(200, 500]` 桶上界），`p99 = 500 ms`，`most = 320 ms`；
- 全程 `judge.verdict = 0`、`stall.snapshot = 0`；实际耗时 `93.93 s`。

### 验证结果

- `cargo fmt --all -- --check`、`RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部通过。
- `git grep -n 'ignore = "P1-18' core/`：无结果。
- `ui`：`svelte-check` 0 errors 0 warnings；`vitest` 5 files / 29 tests passed。
- 沙箱内可运行 Rust 测试全部通过（含 8 小时 `i1_eight_hours_…`）。
- 完整门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh`：全绿（`rustfmt`、`clippy -D warnings`、全部 6 个规则脚本、`cargo test`：`passed 596, failed 0, ignored 9`、`ui`：`0 errors, 0 warnings`、`29 passed`）。

## 留下的，要负责人的

- 预算表、重启上限与 `CONNECT_TIMEOUT_MS`/`READ_TIMEOUT_MS` 均为临时值（`P1.md` §4 第 3 条）；后台任务（整场转写/交城/房间）挂树留给 P1-19。
