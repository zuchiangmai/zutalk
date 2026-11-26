状态：完成

## 改动与文件

修复第 1 轮审查（`docs/build/reviews/P3-r1.md`）**必须改 #1**：在 `QueryCache` 对齐核心 `QuerySlot` 取消语义做按槽串行调度，消除连接真实 Rust `ViewEngine` 时的同槽互杀与永久 `loading`，并在 `startBus` 重置信号序列水位：

1. **`ui/src/bus/query.ts`**：
   - 新增枚举联合类型 `QuerySlot = 'Library' | 'Search' | 'Recording' | 'Transcript' | 'Compare' | 'Other'` 与纯函数 `querySlot(q)`，与 `core/src/view/query.rs::Query::slot()` 一一对应。
   - 新增命名常量 `QUERY_CANCEL_RETRY_LIMIT = 1`（活跃非 `Search` 条目意外遇 `query.cancelled` 时最多自动重排 1 次，超限后不将空条目遗弃在永久 `loading`）。
   - 在 `QueryCache` 内新增按槽 `slotInFlight` 与去重有界 `slotQueues`（受 `QUERY_CACHE_MAX_ENTRIES = 128` 封顶）：`Search` 槽保持立即发射以保留新搜索抢占旧 SQLite 扫描的核心语义；其余槽同一时刻仅 1 条请求在途，重复 `key` 去重合并，出队时若订阅者已全部退订则跳过执行并标 `stale = true`；`clear()` 与 `setExecutor(null)` 同步清空槽队列。
2. **`ui/src/bus/channel.ts`**：
   - `startBus(bus)` 挂载新总线前清理残留 `flushRaf`、`seenSeq` 与 `pendingSignals`，避免切换/重启总线实例后因沿用旧 `seq` 丢弃新总线前期信号。
3. **`ui/tests/bus.test.ts`**：
   - 先写 3 条回归测试并验证修复前全部失败，修复后全部转绿；同步更新 `LibraryRecent` 串行失效与乱序版本丢弃测试。

## 验证证据

- `cd ui && npm run check && npm test` 全部通过（`tsc` 零报错，三入口 `vite build` 成功）。
- 全部 10 个规则脚本通过（含 `ui.sh` 与 `pending.sh`）。
- UI 测试 12 个文件 **79 条全部通过**，其中 `ui/tests/bus.test.ts` 新增 3 条：
  - `resets seenSeq when startBus attaches a new bus instance`
  - `serializes same-slot Library and Transcript queries against Rust claim_slot cancellation while keeping Search preemptive`（严格复刻 `claim_slot` 取消语义，验证 3 条 `Library` 槽并发订阅与失效全到达 `version: 2`、3 块 `Transcript` 全 `ready` 且零取消，同时验证两条快速 `Search` 正常触发 1 次 `Search` 抢占）
  - `recovers automatically when an active entry receives a single unexpected query.cancelled`
- 本任务零 Rust 代码改动。

## 遗留与需负责人确认

- 无；继续执行 `P3-R1-02` 与 `P3-R1-03`。
