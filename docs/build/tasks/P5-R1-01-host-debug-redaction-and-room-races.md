# P5-R1-01 修复主持链接 `Debug` 密钥脱敏、代际守卫与条件删锁

## 目标

修复 P5 第 1 轮审查中的 **必须改 #1、#2**：

1. **`HostState` 与 `HostStepEvent` 禁止在 `Debug` 中暴露直播房间密钥（必须改 #1，原则 6）**：
   - 移除 `core/src/out/host.rs` 中 `HostState` 与 `HostStepEvent` 的自动 `#[derive(Debug)]`，手动实现 `std::fmt::Debug`，对 `link` 字段中 `#` 之后的 32 字节密钥片段一律脱敏（如 `<origin>/r/<id>#…` 或 `"…"`），确保 `format!("{:?}", host_state)`、`format!("{:?}", state)`（`proto::State` 含 `host: HostState`）与 `format!("{:?}", step_event)` 绝不包含任何房间密钥字节。
2. **消除主持状态机三处并发与锁文件竞态（必须改 #2）**：
   - **代际化 `Msg::HostStopped` 并收紧 `host_step`**：给 `Msg::HostStopped` 增加 `host_gen: u64`，`Work::host_stop_room` 在停房时传递本次停房所属代际，Hub 仅当 `host_gen == self.host_gen` 时才推进 `HostStepEvent::Stopped`（过时 `HostStopped` 带 `// dropped:` 注释丢弃）；同时收紧 `host_step`：`HostStepEvent::Stopped` 仅在 `HostState::Stopping` 切回 `HostState::Idle`，`HostState::Arming` 收到 `Stopped` 保持 `Arming`，防止先关后开字幕时旧房 `gone` 把新房冲回 `Idle` 并孤立 `host.slot`；
   - **`Arming` 期间开录取消在途建房**：`hub.rs` 处理非文件 `Command::Start` 时，若当前 `self.state.host` 处于 `HostState::Arming { .. }`，必须先 `self.bump_host_gen()` 并调用 `self.work.host_stop_room()` 取消在途 `net.host_make`（避免 `make` 随后成功起 `host.slot` 却被 `Live { outbound: false }` 忽略而泄漏）；
   - **按房间 id 条件清理 `<data>/host.lock`**：在 `core/src/out/host.rs` 提供 `pub fn clear_lock_if(data: &Path, room_id: &str)`（仅当 `read_lock(data).as_deref() == Some(room_id)` 时删除），并将 `core/src/app/work.rs` 中 `net.host_make`、`net.host_gone`、`net.host_recover` 的所有按房间清锁调用全部改为 `clear_lock_if(&data, &room_id)`，杜绝旧房清理或崩溃恢复误删新房的 `host.lock`。

## 背景

- 审查报告：`docs/build/reviews/P5-r1.md`（必须改 #1、#2）。
- 涉及文件：
  - `core/src/out/host.rs`
  - `core/src/app/hub.rs`
  - `core/src/app/work.rs`
  - `core/tests/host.rs`

## 做法

### 1. `HostState` 与 `HostStepEvent` 手动 `fmt::Debug` 脱敏（`core/src/out/host.rs`）

- 新增私有辅助函数 `fn redact_link(link: &str) -> &str` 或 `struct RedactedLink<'a>(&'a str)`：若包含 `'#'`，格式化为 `<before_hash>#…`；否则输出 `"…"`（防止将来无 `#` 的敏感串也被打印）。
- 从 `HostState` 与 `HostStepEvent` 的 `#[derive(...)]` 中去掉 `Debug`，手写 `impl std::fmt::Debug`：
  - `HostState::Armed` 与 `HostState::Live` 的 `link` 字段使用 `link.as_deref().map(RedactedLink)`；
  - `HostStepEvent::RoomMade` 的 `link` 字段使用 `RedactedLink(link)`；
  - 其余非敏感字段正常输出。
- 不改变 `Serialize` 与 `TS` 导出（界面仍正常通过 `State.host.link` 显示二维码与复制链接）。

### 2. 条件删锁 `clear_lock_if`（`core/src/out/host.rs`、`core/src/app/work.rs`）

- 在 `core/src/out/host.rs` 新增：
  ```rust
  /// Removes `<data>/host.lock` only if it still names `room_id`, preventing a slow teardown or
  /// crash-recovery task from deleting a newer room's lock file.
  pub fn clear_lock_if(data: &Path, room_id: &str) {
      if read_lock(data).as_deref() == Some(room_id) {
          clear_lock(data);
      }
  }
  ```
- 在 `core/src/app/work.rs` 将以下 6 处清锁全部改为 `host::clear_lock_if(&clear_data, &clear_id)`：
  1. `net.host_make` 取消后 `gone` 确认路径；
  2. `net.host_make` 取消且 `make` 失败路径；
  3. `net.host_make` 正常失败路径；
  4. `net.host_gone` 确认成功 / `room.gone` 路径；
  5. `net.host_recover` 成功路径；
  6. `net.host_recover` 返回 `room.gone` 路径。

### 3. `Msg::HostStopped { host_gen }` 与 `Arming` + `Start` 守卫（`core/src/out/host.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`）

- **`core/src/out/host.rs::host_step`**：
  - 将 `(HostState::Arming { .. }, HostStepEvent::Cancel { wait_gone: false } | HostStepEvent::RecordingStopped { wait_gone: false } | HostStepEvent::Stopped)` 中的 `| HostStepEvent::Stopped` 移出，使 `(HostState::Arming { .. }, HostStepEvent::Stopped)` 保持 `state.clone()`（仅 `(HostState::Stopping, HostStepEvent::Stopped)` 返回 `HostState::Idle`）。
- **`core/src/app/hub.rs` & `core/src/app/work.rs`**：
  - 将 `Msg::HostStopped` 改为 `Msg::HostStopped { host_gen: u64 }`；
  - `Work::host_stop_room(&self, stop_gen: u64) -> bool` 接收调用方最新的 `self.host_gen`（`bump_host_gen()` 之后的值），在 `net.host_gone` 结束时发送 `Msg::HostStopped { host_gen: stop_gen }`；
  - Hub 处理 `Msg::HostStopped { host_gen }` 时：
    ```rust
    Msg::HostStopped { host_gen } => {
        if host_gen != self.host_gen {
            // dropped: stale room teardown superseded by a newer host session
        } else {
            self.set_host(HostStepEvent::Stopped, "stopped");
        }
    }
    ```
  - Hub 处理 `start()`（`hub.rs:927-936`）时，若 `!file && matches!(self.state.host, HostState::Arming { .. })`，先调用：
    ```rust
    let stop_gen = self.bump_host_gen();
    let _ = self.work.host_stop_room(stop_gen); // dropped: cancel in-flight room make before starting local-only recording
    ```
    再执行 `self.set_host(HostStepEvent::RecordingStarted, "start")`。

## 显式与可观测

- **显式状态机与代际边界**：`host_step` 明确只允许 `Stopping + Stopped -> Idle`，`Arming` 不再被停房事件打断；所有异步主持消息（`HostReady`、`HostFailed`、`HostViewers`、`HostStopped`）全部带 `host_gen` 校验，过时消息带 `// dropped:` 说明，绝不跨代串台。
- **命名常量与队列/任务不变**：不新增队列、任务或 `Stats` 计数行，守住 `stats.full == 0`、`Tally ≤ 8192 B`、`bounded.sh` / `spawn.sh` / `layers.sh` 全仓零欠账。
- **结构化事件不变**：复用既有 `host.state`、`host.room`、`host.recover`、`job.start`/`job.done`，`docs/EVENTS.md` 无需新增事件。
- **隐私红线**：`HostState`、`HostStepEvent` 与 `State` 的 `Debug` 输出彻底遮蔽房间密钥片段，且 `clear_lock_if` 守住崩溃恢复第一道保险 `host.lock`。

## 先写的测试

在 `core/tests/host.rs` 新增/更新以下确定性测试：

1. `host_state_and_step_debug_redact_room_key`：构造带真实 32 字节密钥链接的 `HostState::Armed`、`HostState::Live`、`State` 与 `HostStepEvent::RoomMade`，断言 `format!("{:?}", ...)` 均不含 base64url 密钥片段且保留 `#…` 脱敏标记。
2. `host_lock_clear_if_preserves_newer_room_lock`：写入 `id_a` → 覆写 `id_b` → 调用 `clear_lock_if(dir, id_a)` 断言 `host.lock` 仍为 `id_b` → 调用 `clear_lock_if(dir, id_b)` 断言文件删除。
3. 更新 `host_state_transition_table`：显式断言 `host_step(&arming, HostStepEvent::Stopped) == arming`（不再误退 `Idle`）。
4. `rapid_captions_toggle_does_not_orphan_new_room_or_delete_new_lock`：先 `HostArm { captions: true }` 建房 R1 → 连续发送 `HostArm { captions: false }` 与 `HostArm { captions: true }`（使 R1 的 `gone` 与 R2 的 `make` 并发交错）→ 等待稳定进入 `Armed { captions: true, link: Some(link2) }` → 断言 `host.lock` 仍等于 R2 的房间 id、R1 返回 `404`、R2 可正常连接并推帧、最后 `HostCancel` 干净退回 `Idle` 且清空 `host.lock`。
5. `start_recording_while_arming_cancels_in_flight_room_without_leak`：用阻塞一次 `/rooms` `make` 请求的代理或在 `Arming` 态立刻发送 `Command::Start` → 断言录音以 `Session.outbound == false` 启动，在途建房被取消并在服务器侧清理（不遗留活跃房间与 `host.lock`）。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含全部 10 个规则脚本与 Rust/UI 全量测试）。

## 不做

- 不修改前端文件（归 `P5-R1-02`）；
- 不修改服务器协议，不新增外部依赖或新 `Stats` 计数器。
