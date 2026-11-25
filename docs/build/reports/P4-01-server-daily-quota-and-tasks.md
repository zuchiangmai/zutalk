状态：完成

## 改动与文件

1. **服务器具名任务监督并还清 `spawn.sh` P4 欠账**：
   - `server/Cargo.toml`：引入工作区既有依赖 `tokio-util = { version = "0.7.20", features = ["rt"] }`（`Cargo.lock` 无任何新增外部 crate）。
   - 新增 `server/src/tasks.rs`：实现基于 `TaskTracker` 的 `Tasks`，提供具名 `spawn(name, fut)`（附带 `zutalk_log::carry`，起止输出 `task.start` / `task.end` 事件）及 `wait()` 优雅收尾。
   - `server/src/lib.rs`：`Server` 持有 `tasks: tasks::Tasks`，公开 `Server::shutdown(&self)`；`ROUTES` 从 7 条增至 8 条，加入 `("/quota", "quota", || post(ledger::quota))`。
   - `server/src/ledger.rs`：`lease` 与 `claim` 的 2 处 `tokio::spawn` 迁至 `server.tasks.spawn("ledger.lease", ...)` 与 `server.tasks.spawn("ledger.claim", ...)`。
   - `scripts/build/rules/spawn.sh`：移除 `server/src/ledger.rs 2 P4` 欠账行（保留 `server/src/lib.rs 1 P5`）。

2. **按服务、按 UTC 自然日的每日额度统计与强制上限**：
   - `server/src/ledger.rs`：
     - 具名带单位常量 `DAY_MS`、`DEFAULT_DAILY_SONIOX_TIMES`（40）、`DEFAULT_DAILY_OPENROUTER_TIMES`（30）；
     - 纯值类型与纯函数 `ServiceDaily`、`QuotaReport`、`day_bounds(now_ms)`、`daily_service(used, daily_cap, has_grant, has_balance)`；
     - `Book` 新增 `daily: [u32; 2]`，并将 `pending` 扩为 `(Cid, String, Service, u64)` 同时防余额并发超支与单日服务次数并发超支；新增 `Book::quota(&self, did, now_ms)` 与 `Server::quota(&self, did, now_ms)`；
     - `POST /lease` 校验顺序：重放（`409 replay`）→ 余额（`402 quota`）→ 当日服务上限（`402 quota.daily`）；成功响应保留 `key`/`session_cap_s`/`left` 并附加 `quota`；
     - `POST /claim` 保留 `seconds`/`left` 并附加 `quota`；
     - 新增 `POST /quota`：验签 `talk/quota` 记录（拒绝过期、错误 `aud`、错误类型与本机 `did:key`）并返回 `{ left, resets_at, services: { soniox, openrouter } }`。
   - `server/src/lib.rs` / `server/src/main.rs`：`Config` 新增 `daily: [u32; 2]`，进程启动读取 `ZUTALK_DAILY_SONIOX` / `ZUTALK_DAILY_OPENROUTER`（默认 `40` / `30`）。

3. **测试**：
   - 新增 `server/tests/quota.rs`（4 条测试）。

## 证据

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`、`ui`）：全部通过，其中 `spawn.sh` 已清零 `server/src/ledger.rs`。
- `zutalk-server` 非 TCP 集成测试全绿（共 42 通过，0 失败）：
  - `server/tests/quota.rs`（4/4）：
    - `day_bounds_and_daily_service_rules`
    - `quota_tracks_per_service_daily_usage_and_resets_next_day`
    - `concurrent_leases_cannot_exceed_daily_cap`
    - `quota_endpoint_requires_signed_person_record_and_logs_route`
  - 既有测试：`auth.rs`（8/8）、`grants.rs`（6/6）、`leases.rs`（8/8）、`log.rs`（2/2）、`relay.rs`（4/4）、`rooms.rs`（10/10）。
  - 注：`claims.rs` 需绑定本机 `127.0.0.1:0`（沙箱禁端口绑定），已通过 `clippy --all-targets` 全量编译检查，由批次末尾沙箱外 `scripts/build/check.sh` 执行验证。
- `ui`：`npm run check` 与 `npm test`（12 文件 86/86 测试）全绿。
- `Cargo.lock` 仅在 `zutalk-server` 依赖下引用既有 `tokio-util 0.7.20`，零新增外部依赖。

## 遗留 / 需负责人确认

- 生产每日额度上限暂按设计稿默认 `40`（Soniox）/ `30`（OpenRouter），部署时可通过 `ZUTALK_DAILY_SONIOX` 与 `ZUTALK_DAILY_OPENROUTER` 调整。
- `server/src/lib.rs` 的房间清扫 `tokio::spawn`（`server/src/lib.rs 1 P5`）按计划留待 P5 迁入 `server.tasks`。
