# P5-01 服务器无盘只留槽直播房间与清扫任务监督

## 目标

做完以后：

1. **服务器房间改为纯内存只留槽（`server/src/room.rs`）**：
   - `POST /rooms` 仅接受 `make`、`slot`、`gone` 三个动作，彻底移除单条录音持久化 `log` 语义（收到 `do: "log"` 或未知动作返 `400 "do"`）；
   - 房间不再写盘（不建 `rooms/` 目录、不用 `Pack`），仅驻内存，重启后自然清空；
   - 以 `tokio::sync::watch::channel(SlotState::Slot { seq: 0, frame: String::new() })` 取代 `broadcast::channel(256)`，最新一帧覆盖旧帧（慢观众自动合并、永不丢线重连），`room.live.receiver_count()` 实时给出精确 `viewers` 人数并在 `make`/`slot` 应答中返回；
   - 默认无写入过期时间从 30 天收紧为 **2 分钟**（`ROOM_IDLE_TTL_MS = 120_000`）；`gone` 或 `sweep()` 移除房间时写入 `SlotState::Gone`，所有在线 SSE 观众立即收到 `event: gone` 且连接干净关闭。
2. **服务器清扫循环接入具名监督树（`server/src/tasks.rs`、`server/src/lib.rs`）**：
   - `Tasks` 增加 `CancellationToken`（`token()` / `cancel()`），`Tasks::wait()` 先取消长期循环任务再等待在途任务收尾；
   - `server/src/lib.rs::serve()` 将裸 `tokio::spawn` 替换为 `server.tasks.spawn("server.sweep", …)`（每 `SWEEP_EVERY_MS = 30_000` ms 扫一次并监听 `cancel.cancelled()`）；
   - `Server::sweep` 发出 `debug!("server.sweep", ms = …, removed = removed)` 并同步更新 `docs/EVENTS.md`。
3. **清零全仓 `bounded.sh` 与 `spawn.sh` 的最后两笔欠账**：
   - 移除 `scripts/build/rules/bounded.sh` 的 `server/src/room.rs 1 P5`；
   - 移除 `scripts/build/rules/spawn.sh` 的 `server/src/lib.rs 1 P5`；
   - 同步更新 `server/tests/rooms.rs` 与 `core/tests/{room,app}.rs` 中原先依赖服务器 `do: "log"` 的断言（`Command::Share`/`Unshare` 本身留给 P5-02 替换），确保全仓 `scripts/build/check.sh` 全绿。

## 背景

- 设计：`docs/build/phases/P5.md` §3.1–§3.2；`docs/PLAN.md` §2.1、§5.1、§5.5、§8 P5。
- 涉及文件：
  - `server/src/tasks.rs`、`server/src/room.rs`、`server/src/lib.rs`
  - `scripts/build/rules/bounded.sh`、`scripts/build/rules/spawn.sh`
  - `docs/EVENTS.md`
  - `server/tests/rooms.rs`、`server/tests/common/mod.rs`（如有引用）
  - `core/tests/room.rs`、`core/tests/app.rs`（仅移除对服务器已废弃 `do: "log"` 的测试断言，完整宿主替换见 P5-02）

## 做法

### 1. `server/src/tasks.rs`

- 结构体增加 `token: tokio_util::sync::CancellationToken`：
  ```rust
  #[derive(Default)]
  pub struct Tasks {
      tracker: TaskTracker,
      token: CancellationToken,
  }
  ```
- 新增 `pub fn token(&self) -> CancellationToken { self.token.child_token() }` 与 `pub fn cancel(&self) { self.token.cancel(); }`。
- `wait(&self)` 改为先 `self.token.cancel()`，再 `self.tracker.close()`、`self.tracker.wait().await`。

### 2. `server/src/room.rs`

- 具名带单位常量（含注释，过 `numbers.sh`）：
  - `pub const ROOM_IDLE_TTL_MS: i64 = 120_000;` // 2-minute inactivity TTL for ephemeral live rooms (`PLAN.md` §5.5)
  - `pub const ROOM_MAX_SLOT_BYTES: usize = 32 << 10;` // max base64 slot payload per write, under 64 KiB body limit
  - `pub const ROOM_MAX_PER_OWNER: usize = 4;` // max concurrent live rooms held by one owner
  - `pub const ROOM_MAX_VIEWERS: usize = 500;` // max simultaneous SSE viewers per room
- 显式状态枚举与纯判定函数（供测试直接导入）：
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum SlotState {
      Slot { seq: u64, frame: String },
      Gone,
  }

  /// Pure expiration check: a live room expires strictly after `ttl_ms` without writes (`PLAN.md` §5.5).
  pub fn room_expired(last_ms: i64, now_ms: i64, ttl_ms: i64) -> bool {
      now_ms > last_ms.saturating_add(ttl_ms)
  }
  ```
- `Room` 结构体（纯内存，无 `Pack`、无 `frames`、无 `broadcast::Sender`）：
  ```rust
  pub struct Room {
      owner: String,
      slot_at: i64,
      last: i64,
      seq: u64,
      live: tokio::sync::watch::Sender<SlotState>,
  }
  ```
- `POST /rooms`（`write`）：
  - 校验 32 位小写十六进制 `id`（否则 `400 "room"`）；
  - `"make"`：重名返 `409 "taken"`，每人总数 `>= most_rooms` 返 `429 "rooms"`；创建 `watch::channel(SlotState::Slot { seq: 0, frame: String::new() }).0`，写入 `rooms` map，返回 `{ "viewers": 0 }`；
  - 其余动作查房间（不存在返 `404 "room"`）并验主人（`403 "owner"`）；
  - `"slot"`：验 `record.at > room.slot_at`（否则 `409 "replay"`），验 `frame.len() <= max_slot_bytes`（否则 `413 "full"`）；更新 `room.slot_at = record.at`、`room.last = now()`、`room.seq += 1`；调 `let _ = room.live.send_replace(SlotState::Slot { seq: room.seq, frame }); // dropped: previous slot value is replaced`；返回 `{ "viewers": room.live.receiver_count() }`；
  - `"gone"`：移除房间并调 `let _ = room.live.send_replace(SlotState::Gone); // dropped: previous slot value is replaced`；返回 `{}`；
  - 任何其他 `do`（含 `"log"`）：返回 `400 "do"`。
- `GET /rooms/{id}/events`（`events`）：
  - 查房间（`404 "room"`），验 `room.live.receiver_count() >= max_viewers`（`429 "viewers"`）；
  - 订阅 `let rx = room.live.subscribe();` 后释放锁；
  - 用 `tokio_stream::wrappers::WatchStream::new(rx)` 映射：
    - `SlotState::Slot { frame, .. } => event("slot", &frame)`
    - `SlotState::Gone => event("gone", "")`
  - 确认在 `send_replace(SlotState::Gone)` + `rooms.remove(&id)` 后，所有在读 `WatchStream` 均能收到最后一个 `event: gone` 并立即结束流（`None`）。
- `pub fn sweep(server: &Server, now_ms: i64) -> usize`：
  - 收集全部 `room_expired(room.last, now_ms, server.config.room.3)` 的 `id`，逐个移除并 `send_replace(SlotState::Gone)`，返回移除个数。

### 3. `server/src/lib.rs`、规则脚本与 `docs/EVENTS.md`

- `server/src/lib.rs`：
  - 导出 `pub use crate::room::{ROOM_IDLE_TTL_MS, ROOM_MAX_PER_OWNER, ROOM_MAX_SLOT_BYTES, ROOM_MAX_VIEWERS, SlotState, room_expired};`；
  - `Config::new` 默认 `room: (ROOM_MAX_SLOT_BYTES, ROOM_MAX_PER_OWNER, ROOM_MAX_VIEWERS, ROOM_IDLE_TTL_MS)`；
  - `Server::open` 不再创建 `rooms/` 目录，也不调用 `room::open_all`，直接初始化空 `Mutex::new(HashMap::new())`；
  - `const SWEEP_EVERY_MS: u64 = 30_000;` // sweep idle rooms every 30 s so 2-minute TTL expires promptly (`PLAN.md` §5.5)；
  - `Server::sweep(&self, now: i64) -> usize` 调 `room::sweep(self, now)` 并发 `debug!("server.sweep", ms = started.elapsed().as_millis() as u64, removed = removed)`，返回 `removed`；
  - `serve()` 改为通过 `server.tasks.spawn("server.sweep", ...)` 启动清扫循环，循环内 `tokio::select! { () = cancel.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(SWEEP_EVERY_MS)) => { sweeper.sweep(now()); } }`；`axum::serve` 返回前调用 `server.tasks.cancel()`。
- 更新 `scripts/build/rules/bounded.sh`：清空 `later`（删去 `server/src/room.rs 1 P5`）。
- 更新 `scripts/build/rules/spawn.sh`：清空 `later`（删去 `server/src/lib.rs 1 P5`）。
- 更新 `docs/EVENTS.md`：`server.sweep` 的字段列更新为 `` `ms` `removed` ``。
- 最小同步 `core/tests/room.rs` 与 `core/tests/app.rs`：把两处调用真实服务器 `Write::Log` / `Command::Share { recording: Some(id) }` 的测试改为断言实时槽只留槽行为（`Write::Log` 与 `Command::Share` 本身在 P5-02 彻底移除）。

## 显式与可观测

- **显式状态与纯函数**：`SlotState` 枚举表达槽生命周期，`room_expired(last_ms, now_ms, ttl_ms)` 纯函数带边界测试（`== ttl` 不过期、`ttl + 1` 过期）。
- **具名带单位常量**：`ROOM_IDLE_TTL_MS`、`ROOM_MAX_SLOT_BYTES`、`ROOM_MAX_PER_OWNER`、`ROOM_MAX_VIEWERS`、`SWEEP_EVERY_MS` 全带单位与理由注释（过 `numbers.sh`）。
- **零无界队列与全监督任务**：`broadcast::channel` 与裸 `tokio::spawn` 全部消除，`bounded.sh` 与 `spawn.sh` 欠账归零。
- **结构化事件与故障可见**：`server.sweep` 记 `ms` 与 `removed`；清扫任务起止记 `task.start` / `task.end`（`tree = "server", task = "server.sweep"`）；无任何房间 id 或密文进日志；房间过期、人数满、越权写入与重放均有明确 HTTP 错误码与测试。

## 先写的测试

1. `server/tests/rooms.rs` 重写为只留槽规格测试：
   - `room_expired_boundary_table`：纯函数边界测试（`0`、`ttl - 1`、`ttl`、`ttl + 1`、`i64::MAX` 饱和）；
   - `only_the_owner_writes_and_log_is_rejected`：验证 `make`/`slot`/`gone` 权限与校验，验证 `do: "log"` 返回 `400 "do"`，验证 `rooms/` 目录绝不创建任何文件；
   - `slot_only_holds_latest_reports_accurate_viewers_and_rejects_replay`：2 个并发观众接入后 `slot` 返回 `viewers == 2`，断开 1 个后返回 `viewers == 1`；旧时间戳返回 `409 "replay"`；晚来的观众只看到最新槽；
   - `gone_and_two_minute_idle_ttl_notify_viewers_and_close_stream`：显式 `gone` 与 `sweep(now + ROOM_IDLE_TTL_MS + 1)` 均向在线观众推送 `event: gone` 并干净关闭流，随后 `GET /rooms/{id}/events` 返 `404`；在 `now + ROOM_IDLE_TTL_MS - 1` 时仍存活且写入一次 `slot` 后重置 TTL；
   - `ephemeral_rooms_do_not_persist_across_restart_and_hold_no_plaintext`：重启服务器后房间自动消失；密文不落地；
   - `rooms_enforce_slot_size_owner_count_and_viewer_caps`：验证 `full`（413）、`rooms`（429）、`viewers`（429）；
   - `server_shutdown_cancels_supervised_sweeper`：验证 `Tasks` 取消能干净结束 `server.sweep` 并产出 `task.start` / `task.end`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿；
- `scripts/build/rules/bounded.sh` 与 `scripts/build/rules/spawn.sh` 的 `debt` 与 `later` 均为空且通过；
- `Cargo.lock` 无任何新增外部依赖。

## 不做

- 不改动核心 `Command::Share`/`Unshare` 协议删除与 `out/host.rs`（留 P5-02）；
- 不改动外壳 `app/src/` 与前端页面（留 P5-03 至 P5-05）。
