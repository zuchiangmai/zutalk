状态：完成

## 改了什么

1. **服务器监督任务支持取消（`server/src/tasks.rs`）**：
   - `Tasks` 增加 `token: CancellationToken`，提供 `token()`（子令牌）与 `cancel()`；
   - `Tasks::wait()` 先取消长期后台任务，再关闭追踪器并等待所有在途任务干净收尾。
2. **服务器房间改为纯内存只留槽直播房间（`server/src/room.rs`、`server/src/lib.rs`）**：
   - 具名常量：`ROOM_IDLE_TTL_MS = 120_000`（2 分钟无写入过期）、`ROOM_MAX_SLOT_BYTES = 32 << 10`、`ROOM_MAX_PER_OWNER = 4`、`ROOM_MAX_VIEWERS = 500`、`SWEEP_EVERY_MS = 30_000`；
   - 显式状态机与纯函数：`SlotState::{Slot { seq, frame }, Gone}` 与 `room_expired(last_ms, now_ms, ttl_ms)`；
   - `Room` 纯驻内存、以 `tokio::sync::watch::channel` + `WatchStream` 取代 `Pack` 写盘、`frames` 无限日志与 `broadcast::channel(256)`，不再创建 `rooms/` 目录；
   - `POST /rooms` 仅支持 `make` / `slot` / `gone`（废除 `do: "log"`，返 `400 "do"`），`make` 与 `slot` 实时返回精确 `viewers`（`room.live.receiver_count()`）；`gone` 与 `sweep()` 移除房间并写入 `SlotState::Gone`，在线 SSE 观众收到 `event: gone` 后连接自动关闭；
   - `Server::start_sweeper` 与 `serve()` 将清扫循环迁入 `server.tasks.spawn("server.sweep", …)` 并监听取消；`Server::sweep` 返回移除房间数并发出 `debug!("server.sweep", ms = …, removed = …)`。
3. **清零 `bounded.sh` 与 `spawn.sh` 全仓欠账并同步测试与事件文档**：
   - 清空 `scripts/build/rules/bounded.sh` 的 `later`（删除 `server/src/room.rs 1 P5`）；
   - 清空 `scripts/build/rules/spawn.sh` 的 `later`（删除 `server/src/lib.rs 1 P5`）；
   - 更新 `docs/EVENTS.md` 中 `server.sweep` 字段为 `` `ms` `removed` ``；
   - 重写 `server/tests/rooms.rs`（7 条只留槽规格测试），同步 `server/tests/log.rs`、`core/tests/room.rs` 与 `core/tests/app.rs`（最小适配服务器只留槽行为）。

## 证据

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（零警告）。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：全部通过，`bounded.sh` 与 `spawn.sh` 的 `debt`/`later` 均已清空。
- 沙箱内 `zutalk-server` 非回环 TCP 测试全绿（共 29 条）：
  - `server/tests/rooms.rs`（7/7）：
    - `room_expired_boundary_table`
    - `only_the_owner_writes_and_log_is_rejected`
    - `slot_only_holds_latest_reports_accurate_viewers_and_rejects_replay`
    - `gone_and_two_minute_idle_ttl_notify_viewers_and_close_stream`
    - `ephemeral_rooms_do_not_persist_across_restart_and_hold_no_plaintext`
    - `rooms_enforce_slot_size_owner_count_and_viewer_caps`
    - `server_shutdown_cancels_supervised_sweeper`
  - `server/tests/auth.rs`（8/8）、`server/tests/leases.rs`（8/8）、`server/tests/log.rs`（2/2）、`server/tests/relay.rs`（4/4）；
  - `cargo test -p zutalk-core --test room --test app --no-run`：编译通过；`Cargo.lock` 无变更。

## 遗留 / 需协调人注意

- 沙箱禁用本地回环端口监听，涉及真实 TCP 端口的测试（如 `server/tests/claims.rs` 与 `core/tests/{room,app}.rs`）需在批末非沙箱环境通过 `scripts/build/check.sh` 统一跑通。
- `Command::Share` / `Command::Unshare` 与旧 `out/room.rs` 的彻底移除留在 `P5-02`。
