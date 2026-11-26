# P2-R1-01 修正 `ViewEngine::query` 的版本号与数据源快照读取顺序

## 目标

修掉 P2 第 1 轮审查的**必须改 #1**（`docs/build/reviews/P2-r1.md`）：

1. 消除 `ViewEngine::query` 中「等待信号量许可前克隆旧 `ViewSource`、拿到许可后再读新 `version`」的并发竞态，保证任何查询绝不会把旧快照（如旧日历、旧精修态、旧身份书架/索引路径）盖上新版本号返回。
2. 在 `core/tests/view.rs` 补一条确定性回归测试，验证在查询占满信号量排队期间发生 `set_calendar` + `invalidate` 时，排队查询返回的数据与版本号严格一致（不会出现旧数据配新版本号）。

## 背景

- `docs/build/reviews/P2-r1.md` 必须改 #1；`docs/build/phases/P2.md` §3.6.2；`docs/PLAN.md` §2.5（视图按版本失效）。
- 当前代码 `core/src/view/query.rs:430-464`：
  - 第 430 行先锁 `self.0.source` 克隆整份 `src: ViewSource`；
  - 第 440–451 行在 `self.0.permits` 上异步等待许可；
  - 第 457 行才读取 `let version = self.0.version.load(Ordering::Acquire)`；
  - 第 463 行把第 430 行克隆的旧 `src` 传进 `execute_blocking`。
- 写端（`set_source` + `invalidate_all`、`set_calendar` + `invalidate`、`set_has_soniox_key` + `invalidate`、`set_active_refine` 等）总是先更新 `ViewSource` / 提交 SQLite，再在 `invalidate` 中 `version.fetch_add(1, AcqRel)`。因此读端必须**先** `version.load(Acquire)`、**再**克隆 `ViewSource` 并读盘，才能以 Acquire/Release 关系保证「读到新版本号 ⇒ 必看到新数据源与已提交快照」。

## 做法

1. **`core/src/view/query.rs`（`ViewEngine::query`）**：
   - 在占槽（`claim_slot`）之前仅读取 `ready`（短锁读一个 `bool`），若 `!ready` 按既有逻辑发 `warn!("query.done", ..., code = "starting")` 并返回 `Err("starting")`。
   - 拿到 `_permit` 且再次确认 `!cancel.is_cancelled()` 之后：
     1. 先读取 `let version = self.0.version.load(Ordering::Acquire);`；
     2. 再短锁 `self.0.source` 克隆最新 `src`；若此时 `!src.ready`，清理槽位并按 `"starting"` 返回；
     3. 再将该 `src` 传入 `self.0.tree.blocking("view.query", move || execute_blocking(query, src, run_cancel, run_interrupt))`。
   - 保持所有锁作用域严格不跨越 `.await`，保持所有既有槽位取消、慢查询预算、事件与统计不变。
2. **测试（`core/tests/view.rs`）**：
   - 新增确定性回归测试 `view_engine_never_stamps_stale_source_with_newer_version`：
     - 利用既有 `ViewEngine::set_hook` 配合 `Barrier`/`Condvar` 或频道，先发满 `QUERY_CONCURRENT_MAX_JOBS`（8）个不同槽位（或分批占用许可，注意同槽会取消旧查询——可先发起 1 个占槽阻塞查询，或用 `QuerySlot` 不冲突的方式？注意共有 6 个 `QuerySlot`，而 `QUERY_CONCURRENT_MAX_JOBS = 8`，单引擎靠不同槽位最多同时占 6 个许可；可用 `ViewEngine` 既有公开 API 测试两类路径，或者更直接地：**在 `QuerySlot` 不够 8 个时怎么确定性命中窗口？**）。
     - 注意精确分析许可数与槽位数：`QuerySlot` 有 6 个变体（`Search`, `Library`, `Recording`, `Transcript`, `Compare`, `Other`），`QUERY_CONCURRENT_MAX_JOBS = 8`；不过同槽新查询只是 `cancel` 旧查询，**旧查询在被取消前仍占着 `_permit` 直到其阻塞回调退出**！因此先用 `set_hook` 挡住 8 个发往 `Search` 槽（或任意槽）的阻塞调用（其中前 7 个被后来的取消但因 hook 未放行仍持有 `_permit`，第 8 个持有第 8 个 `_permit`），此时全部 8 个许可被确定性占满；
     - 随后发起第 9 个查询 `Query::Calendar`（落入 `Other` 槽，不被取消，稳稳停在 `permits.acquire_owned().await` 排队）；
     - 确认前 8 个都在 hook 里且第 9 个已进入等待（可短暂 `yield_now` 循环或等待 `Calendar` 已占住 `Other` 槽），接着调用 `engine.set_calendar(Some(fresh_calendar))` 与 `let v_new = engine.invalidate(ViewKind::Calendar, None)`；
     - 放行 hook（让前 8 个退出释放许可），等待第 9 个 `Query::Calendar` 返回 `reply`；
     - 断言：若 `reply.version >= v_new`，则 `reply.value` **必须** 是 `QueryValue::Calendar(Some(fresh_calendar))`（绝不允许出现 `version == v_new` 却返回 `Calendar(None)`！）。

## 显式与可观测

- **状态与转换**：不新增状态机或第二套版本机制，强化既有单调 `version` 与 `ViewSource` 之间的 Acquire/Release 线性一致不变量。
- **数与单位**：沿用 `QUERY_CONCURRENT_MAX_JOBS = 8` 与 `QUERY_BUDGET_MS = 100`，不引入无名常量。
- **队列与任务**：不新增队列或任务；`self.0.source` 锁仍只在同步短块内持有，绝不跨 `.await`。
- **事件与统计**：沿用既有 `query.done`、`query.slow`、`view.invalidate` 与统计计数，不改动事件表。
- **故障可见性**：新增回归测试同时验证被取消的前序查询记入 `query.cancelled` 且 `Query::Calendar` 在排队恢复后返回最新日历与一致版本号。

## 先写的测试

在改动 `core/src/view/query.rs` 之前，先在 `core/tests/view.rs` 写入 `view_engine_never_stamps_stale_source_with_newer_version` 并运行 `RUSTC_WRAPPER="" cargo test -p zutalk-core --test view view_engine_never_stamps_stale_source_with_newer_version`，确认在旧代码上稳定复现失败（返回 `Calendar(None)` 却带着 `version == v_new`）；再修改 `core/src/view/query.rs` 读取顺序，确认测试稳定通过。

## 验收

- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test view` 全部通过。
- 全部 9 个规则脚本（含 `bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`）通过，`cargo fmt --all -- --check` 与 `clippy --all-targets -- -D warnings` 全绿。
- 报告中贴出「先写测试的失败输出」和修复后的通过输出。

## 不做

- 不改变 `QuerySlot`、`QUERY_CONCURRENT_MAX_JOBS` 或对外协议类型。
- 不改动 `app/src` 或 `ui/`。
