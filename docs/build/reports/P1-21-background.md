状态：完成

# P1-21 后台一次一件；核心里再没有裸的任务和直接造的通道：报告

## 改了什么

1. **后台一次一件（`core/src/app/work.rs`）**：
   - 新增常量 `BACKGROUND_AT_ONCE_JOBS: usize = 1`（`PLAN.md` §4.1「后台任务，一次一件」），在 `Work` 上维护单一 `Arc<Semaphore>`，供整场转写（`job.write`）、交城（`job.file`）、搬运（`job.move`）共享。
   - 许可按**单次尝试**申请与释放：进入 `again` 的两次尝试间隔前即释放许可，不在等待期占着位置；每次尝试独立发一对 `job.start` / `job.done`（同一件活多次尝试共用同一个 `job` 序号），`waited_ms` 记等待许可的时长。

2. **重试退避拆成纯函数与事件回调（`core/src/rt/supervise.rs`、`core/src/app/work.rs`、`core/src/out/city.rs`）**：
   - 抽出纯函数 `pub const fn again_wait_ms(n: u32) -> u64`（第 1 次 `AGAIN_MS`，每次加倍，上限 `AGAIN_LONGEST_MS`）。
   - `again(attempt, on_wait)` 收到 `End::Retry(why)` 时回调 `on_wait(why, wait_ms, n)`，由调用者发 `warn!("job.again", kind, why, wait_ms, n)`（`Work` 与 `out/city.rs` 一致）。
   - `docs/EVENTS.md` 补齐 `job.again` 行。

3. **清零 `ai/write.rs` 与 `out/city.rs` 欠账（`core/src/ai/write.rs`、`core/src/out/city.rs`、`scripts/build/rules/bounded.sh`、`scripts/build/rules/spawn.sh`）**：
   - `write::write`、`city::file` 入口增加 `tree: &Tree` 参数。
   - 三处 `spawn_blocking`（`write.wav`、`write.length`、`city.audio`）改为挂在 `tree` 上的任务 + `tree.blocking(...)`，若阻塞读盘 `Broke` 则向通道送入错误并让任务带着失败代码结束。
   - 两条通道改为具名有上限队列：`queue::bounded(tree.stats(), "write.wav", WAV_QUEUE_CHUNKS, Full::Wait)`（`WAV_QUEUE_CHUNKS = 32`）与 `queue::bounded(tree.stats(), "city.audio", CITY_QUEUE_PARTS, Full::Wait)`（`CITY_QUEUE_PARTS = 1`），保留并阐明通道关闭后发送返回 `Err` 的丢弃理由（对应 P1-05 报告指派项）。
   - 删除 `scripts/build/rules/bounded.sh` 与 `scripts/build/rules/spawn.sh` 中全部 `core/src/` 欠账行；全仓 `core/src`（除 `core/src/rt`）裸 `spawn` 与直接通道清零。
   - `core/tests/hub/mod.rs` 的 `config_on` 补上 `devices: Arc::new(Vec::new)`，避免多测试并行时真实 CoreAudio 异步回写设备列表干扰 `state_seq` 断言；`core/tests/city.rs` 独立测试补 `rustls` 默认 provider 安装。

改动文件：`core/src/rt/supervise.rs`、`core/src/ai/write.rs`、`core/src/out/city.rs`、`core/src/app/work.rs`、`docs/EVENTS.md`、`scripts/build/rules/bounded.sh`、`scripts/build/rules/spawn.sh`、`core/tests/supervise.rs`、`core/tests/retain.rs`、`core/tests/write.rs`、`core/tests/city.rs`、`core/tests/app.rs`、`core/tests/liveness.rs`、`core/tests/hub/mod.rs`、本报告。

## 证据

### 1. 欠账表对比（`bounded.sh` 与 `spawn.sh`）

- **`scripts/build/rules/bounded.sh`**：
  - 改之前：
    ```text
    debt='
    core/src/ai/write.rs 1 P1-21
    core/src/out/city.rs 1 P1-21
    '
    ```
  - 改之后：
    ```text
    debt='
    '
    later='
    server/src/room.rs 1 P5
    '
    ```
- **`scripts/build/rules/spawn.sh`**：
  - 改之前：
    ```text
    debt='
    core/src/ai/write.rs 2 P1-21
    core/src/out/city.rs 1 P1-21
    app/src/main.rs 1 P1-23
    '
    ```
  - 改之后：
    ```text
    debt='
    app/src/main.rs 1 P1-23
    '
    later='
    server/src/ledger.rs 2 P4
    server/src/lib.rs 1 P5
    '
    ```
- `git grep -n "tokio::spawn\|spawn_blocking\|thread::spawn\|mpsc::channel\|sync_channel\|broadcast::channel" -- core/src ':!core/src/rt'`：0 结果。
- `git grep -n 'ignore = "P1-' core/`：0 结果（`i2_background_work_runs_beside_a_recording` 已解除挂起并通过）。

### 2. 测试 #4（`one_that_waits_to_try_again_does_not_hold_up_the_next`）真实日志行

```text
info  zutalk_core::app::work  job.start  kind="write" job=3 waited_ms=0
warn  zutalk_core::app::work  job.done   kind="write" job=3 code="service-busy" ms=25
warn  zutalk_core::app::work  job.again  kind="write" why="service-busy" wait_ms=60000 n=1
info  zutalk_core::app::work  job.start  kind="write" job=4 waited_ms=25
info  zutalk_core::app::work  job.done   kind="write" job=4 code="ok" ms=73
```

第一条遇到 503（`service-busy`）释放许可并进入 60 s 退避后，第二条仅等 25 ms 即拿到许可并写完，不被挡住。

### 3. 新增与解除挂起的测试（全部通过）

- `core/tests/supervise.rs`：`the_wait_before_trying_again_doubles_up_to_an_hour`、`trying_again_says_why_and_how_long_each_time`
- `core/tests/retain.rs`：`transcripts_are_written_one_at_a_time`、`one_that_waits_to_try_again_does_not_hold_up_the_next`
- `core/tests/write.rs`：`reading_the_tape_is_a_named_task_and_its_queue_is_counted`、`a_corrupted_tape_fails_write_with_tape_code`
- `core/tests/city.rs`：`reading_the_tape_is_a_named_task_and_its_queue_is_counted`
- `core/tests/app.rs`：`a_tape_that_cannot_be_read_fails_the_job_with_a_code`
- `core/tests/liveness.rs`：`i2_background_work_runs_beside_a_recording`（去 `ignore`，含 `search`/`move`/`write` 的 `job.done(ok)` 与零 `Stalled` 断言）

### 4. 完整门禁验证

- `RUSTC_WRAPPER="" bash scripts/build/check.sh`：全绿（`rustfmt`、`clippy -D warnings`、全部 7 个规则脚本含 `hub.sh`、`cargo test`：`passed 622, failed 0, ignored 7`、`ui`：`0 errors, 0 warnings`、`29 passed`）。
- 门禁收尾顺手修补两处并行/边界问题：`Msg::Sealed` 带上归属身份（`Option<String>`，不透传本机 `did:key`）并在 `Hub` 校验 `owner == self.state.me`，防止录音中途登出时原登录者封带行漏进本机书架视图；`core/tests/retain.rs` 的 `at_start_up_audio_…` 等 `job.done(settings, ok)` 落盘后再重启核心。

## 留下的，要负责人的

- `ai/write.rs` 与 `out/city.rs` 内部写记录的同步磁盘调用位于后台任务（非 hub、非现场路径），按本件「不做」留给 P2、P6 审查时一并看。
- `BACKGROUND_AT_ONCE_JOBS = 1` 按 `PLAN.md` §4.1 保持为 1；如需提高并发数须先与负责人确认。
- `spawn.sh` 中仅剩最后一处 `app/src/main.rs 1 P1-23` 欠账，待 P1-23 清零。
