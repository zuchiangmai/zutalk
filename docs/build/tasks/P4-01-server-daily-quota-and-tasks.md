# P4-01 服务器按服务按天额度与任务监督

## 目标

做完以后：

1. **还清 `spawn.sh` 的 P4 欠账**：新增 `server/src/tasks.rs`（基于工作区 `Cargo.lock` 已有的 `tokio-util::task::TaskTracker`），由 `Server` 统一持有具名监督任务表；将 `server/src/ledger.rs` 中的 2 处 `tokio::spawn`（`lease` 与 `claim`）迁到 `server.tasks.spawn("ledger.lease", ...)` 与 `server.tasks.spawn("ledger.claim", ...)`；从 `scripts/build/rules/spawn.sh` 的 `later` 表删除 `server/src/ledger.rs 2 P4`，并让 `spawn.sh` 全绿。
2. **按服务、按 UTC 自然日的额度统计与上限**：
   - `server/src/ledger.rs` 支持按服务（`soniox`、`openrouter`）与 UTC 自然日统计已用次数、每日上限、剩余次数与次日重置时间（`resets_at`）；
   - `Config.daily: [u32; 2]` 默认 `[40, 30]`（出字 40 次/天、AI 30 次/天，`server/src/main.rs` 可经 `ZUTALK_DAILY_SONIOX` / `ZUTALK_DAILY_OPENROUTER` 覆盖）；
   - 并发 `POST /lease` 在 `Book.pending` 中同时占住余额与当日服务次数，防止并发竞态击穿每日上限；
   - 余额耗尽仍返既有 `402 { "error": "quota" }`（保持全部既有测试零破坏）；余额未耗尽但当日该服务次数达上限时返 `402 { "error": "quota.daily" }`。
3. **新增 `POST /quota` 端点与增量返回字段**：
   - `POST /quota` 复用同一套 Zu 记录验签（`talk/quota` 记录 `{ "aud": origin }`，拒绝 `did:key` 与伪造/超时请求），返回 `{ "left", "resets_at", "services": { "soniox": { "used", "limit", "remaining" }, "openrouter": { "used", "limit", "remaining" } } }`；
   - `POST /lease` 与 `POST /claim` 的成功响应在保留全部既有字段（`key`、`session_cap_s`、`left`、`seconds`）的前提下，附加 `"quota": { "resets_at", "services": ... }`，便于客户端每次取钥或认领后即时同步额度。

## 背景

- 设计：`docs/build/phases/P4.md` §3.1–§3.2；`docs/PLAN.md` §2.4、§5.1、§8 P4；`docs/ui-2026-11-21/Settings.dc.html`。
- 涉及文件：
  - `server/Cargo.toml`（新增已在 `Cargo.lock` 内的 `tokio-util = { version = "0.7.20", features = ["rt"] }`）
  - 新增 `server/src/tasks.rs`
  - `server/src/ledger.rs`、`server/src/lib.rs`、`server/src/main.rs`
  - `scripts/build/rules/spawn.sh`
  - 新增 `server/tests/quota.rs`

## 做法

### 1. 服务器具名任务监督（`server/src/tasks.rs`）

- 在 `server/Cargo.toml` 添加 `tokio-util = { version = "0.7.20", features = ["rt"] }`（与 `core/Cargo.toml` 版本和特性完全一致，不引入任何新外部 crate）。
- 新建 `server/src/tasks.rs`：
  ```rust
  pub struct Tasks {
      tracker: tokio_util::task::TaskTracker,
  }
  ```
  - `Tasks::new() -> Self`；
  - `pub fn spawn<F>(&self, name: &'static str, fut: F) -> tokio::task::JoinHandle<F::Output>`：
    - 使用 `self.tracker.spawn(zutalk_log::carry(async move { ... }))`；
    - 启动时发 `info!("task.start", tree = "server", task = name)`，结束时发 `info!("task.end", tree = "server", task = name, code = "done", ran_ms = ran_ms)`（复用 `docs/EVENTS.md` 既有事件，不新增文档冗余）；
  - `pub async fn wait(&self)`：调用 `self.tracker.close(); self.tracker.wait().await;`。
- 在 `Server` 结构体增加 `pub(crate) tasks: tasks::Tasks` 字段，在 `Server::open` 初始化，并对外提供 `pub async fn shutdown(&self) { self.tasks.wait().await; }`。
- 将 `server/src/ledger.rs` 中 `lease` 与 `claim` 的 `tokio::spawn` 分别替换为 `server.tasks.spawn("ledger.lease", ...)` 与 `server.tasks.spawn("ledger.claim", ...)`。
- 从 `scripts/build/rules/spawn.sh` 的 `later` 表移除 `server/src/ledger.rs 2 P4` 一行（保留 `server/src/lib.rs 1 P5`）。

### 2. 按服务按天额度纯函数与账本折叠（`server/src/ledger.rs`）

- 命名常量（全部带单位与理由注释，过 `numbers.sh`）：
  - `pub const DAY_MS: i64 = 86_400_000;` // milliseconds in one UTC day
  - `pub const DEFAULT_DAILY_SONIOX_TIMES: u32 = 40;` // default daily Soniox key leases per resident (`Settings.dc.html`)
  - `pub const DEFAULT_DAILY_OPENROUTER_TIMES: u32 = 30;` // default daily OpenRouter key leases per resident (`Settings.dc.html`)
- `Config` 新增 `pub daily: [u32; 2]`，`Config::new` 设为 `[DEFAULT_DAILY_SONIOX_TIMES, DEFAULT_DAILY_OPENROUTER_TIMES]`；`server/src/main.rs` 读取 `ZUTALK_DAILY_SONIOX` 与 `ZUTALK_DAILY_OPENROUTER`（缺省用默认值）。
- 纯值类型与纯函数（时间 `now_ms` 作为参数，不做 IO）：
  - `pub fn day_bounds(now_ms: i64) -> (i64, i64)`：返回 `(start_ms, resets_at_ms)`，其中 `start_ms = now_ms.div_euclid(DAY_MS) * DAY_MS`，`resets_at_ms = start_ms + DAY_MS`。
  - `pub struct ServiceDaily { pub used: u32, pub limit: u32, pub remaining: u32 }`
  - `pub struct QuotaReport { pub left: u64, pub resets_at: i64, pub soniox: ServiceDaily, pub openrouter: ServiceDaily }`
  - `pub fn daily_service(used: u32, daily_cap: u32, has_grant: bool, has_balance: bool) -> ServiceDaily`：
    - `limit = if has_grant { daily_cap } else { 0 }`；
    - `remaining = if has_balance { limit.saturating_sub(used) } else { 0 }`。
- `Book` 字段更新：
  - 新增 `daily: [u32; 2]`（`Book::open` 接收 `daily: [u32; 2]`）；
  - `pending: Vec<(Cid, String, Service, u64)>`（记下正在铸钥的服务，计入当日在途次数）；
  - 新增 `pub fn quota(&self, did: &str, now_ms: i64) -> QuotaReport`：
    - 折叠得到 `purses`（含有效 grant 与 claim credit），判断 `has_grant`（存在 `p.0 == did && p.2 > now_ms`）与总余额 `left`；
    - 统计 `[day_start, resets_at)` 内该 `did` 每个 `Service` 的已落盘 `talk/lease` 数加上 `self.pending` 中同 `(did, service)` 数；
    - 调用 `daily_service` 得出 `soniox`、`openrouter` 两行。
  - 在 `Server` 暴露 `pub fn quota(&self, did: &str, now_ms: i64) -> ledger::QuotaReport`。
- `POST /lease` 更新：
  - `const QUOTA_DAILY: Refusal = Refusal(StatusCode::PAYMENT_REQUIRED, "quota.daily");`
  - 锁内先查重放 → 再查余额 `free.ok_or(QUOTA)?` → 再查当日该服务次数 `used_today >= book.daily[service as usize]` 时返 `Err(QUOTA_DAILY)` → 压入 `pending`；
  - 铸钥落盘后调用 `let report = book.quota(&did, now());`，返回 `{ "key": minted.key, "session_cap_s": minted.session_cap_s, "left": report.left, "quota": quota_json(&report) }`。
- `POST /claim` 更新：落盘后同样附带 `"quota": quota_json(&report)`。
- 新增 `pub async fn quota(State(server): State<Arc<Server>>, Json(sent): Json<Delivery>) -> Answer`：
  - `let (record, _) = server.check(sent, "talk/quota", None).await?;`
  - `let report = server.quota(&record.path.by, now());`
  - 返回 `Ok(Json(json!({ "left": report.left, "resets_at": report.resets_at, "services": { "soniox": ..., "openrouter": ... } })))`。
- 在 `server/src/lib.rs` 的 `ROUTES` 表增加 `("/quota", "quota", || post(ledger::quota))`（数组长度 `7 -> 8`）。

## 显式与可观测

- **纯函数与状态规则**：`day_bounds` 与 `daily_service` 为纯函数（时间以参数传入，零 IO），由单元测试覆盖零点跨日、负/边界时间戳、无授予/有授予但余额为 0/达日上限等全部分支。
- **具名带单位常量**：`DAY_MS`、`DEFAULT_DAILY_SONIOX_TIMES`、`DEFAULT_DAILY_OPENROUTER_TIMES` 均带单位与出处注释（`numbers.sh` 守卫）。
- **任务监督**：`server/src/tasks.rs` 具名监督 `ledger.lease` 与 `ledger.claim` 并输出 `task.start` / `task.end`（`spawn.sh` 移除 `server/src/ledger.rs 2 P4` 后全绿）。
- **结构化日志与隐私**：`POST /quota` 自动经 `noted` 中间件输出 `server.request`（`route = "quota"`），绝不打印 DID、钥匙或请求正文；故障路径（日上限 `402 quota.daily`、未授权 `did:key` `403 signer`）在测试中同时断言 HTTP 响应码与日志可见性。

## 先写的测试

新建 `server/tests/quota.rs`（以及单元风格公共 API 测试）：
1. `day_bounds_and_daily_service_rules`：验证 UTC 零点前后 `day_bounds` 精确切分、`daily_service` 在无授予/余额归零/达日上限/未达上限四态的纯值输出。
2. `quota_tracks_per_service_daily_usage_and_resets_next_day`：给 `did(1)` 授予充足余额并设 `daily = [2, 1]`；`POST /quota` 初始答 `soniox: 0/2/2`、`openrouter: 0/1/1`；租 2 次 Soniox 后第 3 次返回 `402 "quota.daily"`，同时 OpenRouter 仍可租 1 次、第 2 次返回 `402 "quota.daily"`；传入 `resets_at` 时刻调 `server.quota(&did(1), resets_at)` 断言次日计数清零、恢复 `remaining == limit`；重启服务器后账本重放结果一致。
3. `concurrent_leases_cannot_exceed_daily_cap`：充足余额、`daily = [2, 2]`，并发发 5 个 Soniox `/lease`，断言恰有 2 个成功、3 个返回 `402 "quota.daily"`，且 `server.shutdown().await` 干净收敛。
4. `quota_endpoint_requires_signed_person_record_and_logs_route`：拒绝过期记录、错误 `aud`、错误类型与本机 `did:key`（`403 "signer"`）；采集 `zutalk_log::capture` 断言 `server.request(route = "quota")` 与 `task.start`/`task.end`（`tree = "server"`, `task = "ledger.lease"`）均可见且日志不含任何 DID 或 `SECRET_KEY`。
5. 全部既有服务器测试（`leases.rs`、`grants.rs`、`claims.rs`、`auth.rs`、`rooms.rs`、`relay.rs`、`log.rs`）原样全绿。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿；
- `scripts/build/rules/spawn.sh` 中不再有 `server/src/ledger.rs` 欠账行；
- `Cargo.lock` 未引入任何新外部 crate（`tokio-util 0.7.20` 已在工作区锁文件中）。

## 不做

- 不动 `server/src/lib.rs` 的房间清扫 `tokio::spawn`（`server/src/lib.rs 1 P5` 留给 P5）；
- 不动 `server/src/room.rs` 的 `broadcast`（`bounded.sh` 标记 `P5`）；
- 本任务暂不改 `core/` 或 `ui/`。
