# P3-R1-01 QueryCache 按核心 QuerySlot 串行化同槽查询与重置总线序列号

## 目标

做完以后：

1. **按核心 `QuerySlot` 对齐调度，消除真实 Rust `ViewEngine` 下的同槽互杀卡死**：
   - `ui/src/bus/query.ts` 按与 `core/src/view/query.rs::Query::slot()` 一致的规则映射每条查询所属的槽位（`Library` / `Search` / `Recording` / `Transcript` / `Compare` / `Other`）。
   - `Search` 槽保持立即发射（保留新搜索关键词打断旧 SQLite 搜索的核心设计）。
   - 其余多键共存槽（`Library`、`Transcript`、`Compare`、`Recording`、`Other`）在 `QueryCache` 内按槽串行执行不同键的请求：同一时刻同一槽位最多只有 1 条请求在途，避免 `Shell`（`LibraryRecent` + `LibrarySummary` + `LibraryMonth`）或 `TranscriptList`（多块 `Transcript` / `Compare`）在初次挂载、翻页或 `Invalidate` 时并发进入同一 `QuerySlot` 被 Rust 互相 `query.cancelled`。
   - 同槽排队遵循有界与去重原则：同一 `key` 重复入队合并为 1 次；出队执行前若条目已无订阅者（`subscribers === 0`）或已失效移除，跳过网络/IPC 调用并保留 `stale = true`。
   - 若某活跃条目（`subscribers > 0` 且 `reqId === entry.reqSeq`）仍因竞态意外收到 `query.cancelled`，不再永久遗弃在 `status: 'loading'`：标记 `stale = true` 并最多自动补拉 `QUERY_CANCEL_RETRY_LIMIT = 1` 次，确保视图不卡死。
2. **`startBus` 重置每类信号序列水位**：
   - `ui/src/bus/channel.ts` 的 `startBus(bus)` 接入新总线前重置 `seenSeq` 并清理可能残留的 `pendingSignals`，避免切换/重启总线实例后因沿用旧 `seq` 误丢新总线的初始信号。

## 背景

- 审查来源：`docs/build/reviews/P3-r1.md` **必须改 #1**。
- 涉及文件：
  - `ui/src/bus/query.ts`
  - `ui/src/bus/channel.ts`
  - `ui/tests/bus.test.ts`
- 对照参考（只读，不改 Rust）：`core/src/view/query.rs:221-233, 427-439, 510-530`。

## 做法

1. 在 `ui/src/bus/query.ts` 中新增：
   - 类型 `QuerySlot = 'Library' | 'Search' | 'Recording' | 'Transcript' | 'Compare' | 'Other'` 与纯函数 `querySlot(q: Query): QuerySlot`（精确对应 Rust `Query::slot()`）。
   - 命名常量 `QUERY_CANCEL_RETRY_LIMIT = 1`（活跃条目意外遇 `query.cancelled` 时最多重排 1 次，防止死循环）。
   - 在 `QueryCache` 内部维护 `slotInFlight: Map<QuerySlot, string>` 与 `slotQueues: Map<QuerySlot, string[]>`（队列按 `key` 去重，上限自然受 `QUERY_CACHE_MAX_ENTRIES = 128` 封顶）：
     - `enqueueFetch(entry)`：对 `Search` 槽直接调用 `runFetch(entry)`；对其他槽，若同槽无在途请求则立即执行，若同槽正在执行其他 `key` 则将 `entry.key` 去重入队，并在执行前提前将尚无数据的条目置为 `loading`；若同槽正在执行同一个 `key`，标记待重拉或排队在当前完成后再执行最新一次。
     - `drainSlot(slot)`：上一条完成后，从队列头部弹出下一个仍存在于 `entries` 且 `subscribers > 0` 的 `key` 继续执行；已退订的条目仅设 `stale = true` 并跳过。
     - `clear()` 与 `setExecutor(null)` 时清空 `slotInFlight` 与 `slotQueues`。
     - `catch` 分支处理 `query.cancelled`：若 `entry.subscribers > 0 && reqId === entry.reqSeq && entry.cancelRetries < QUERY_CANCEL_RETRY_LIMIT`，递增 `entry.cancelRetries`、`entry.stale = true` 并重新入槽；若已超过重试上限且无已有数据，保持不吞错或将 `stale = true` 以待下次订阅/失效，绝不静默永久卡死。收到成功回执时把 `entry.cancelRetries = 0`。
2. 在 `ui/src/bus/channel.ts` 的 `startBus(bus)` 开头重置 `seenSeq` 与 `pendingSignals`（并保留 `resetChannel()` 原行为）。

## 显式与可观测

- **显式状态与映射**：`QuerySlot` 枚举联合类型 + 纯函数 `querySlot(q)` 与核心 `Query::slot()` 一一对应。
- **具名常量**：`QUERY_CANCEL_RETRY_LIMIT = 1`（带注释说明上限与防抖动原因）；槽队列天然以 `QUERY_CACHE_MAX_ENTRIES = 128` 去重封顶。
- **无静默丢弃**：不再把同槽并发导致的 `query.cancelled` 静默遗弃成永久 `loading`；退订跳过与重试上限均有显式注释说明。
- **故障可见性测试**：模拟核心 `ViewEngine` 同槽取消语义，同时验证多键共存槽零互杀与 `Search` 槽正常取消。

## 先写的测试

在 `ui/tests/bus.test.ts` 新增回归测试：

1. **模拟核心 `QuerySlot` 取消的并发查询测试**：
   - 构造一个严格复刻 `core/src/view/query.rs::claim_slot` 行为的 `query` 替身（同槽在途新请求到达时立即将旧请求 `reject('query.cancelled')`）；
   - 同步订阅 `LibraryRecent`、`LibrarySummary`、`LibraryMonth`（均属 `Library` 槽），断言 3 条查询全部成功到达 `status: 'ready'` 且零条被 `query.cancelled` 杀死；
   - 推送 `Event::Invalidate { view: 'Library', key: null, version: 2 }`，断言 3 条活跃查询全部顺序重拉更新到 `version: 2`；
   - 同步订阅同一录音的 `Transcript` chunk `0`、`1`、`2`（均属 `Transcript` 槽），断言 3 块全部 `ready` 且内容正确；
   - 连续发出两条 `Search` 查询（`Search` 槽），断言新搜索仍立即发出并取消旧搜索，最终仅返回新搜索结果。
2. **意外 `query.cancelled` 恢复测试**：注入一次 `query.cancelled` 后第二次成功，断言活跃条目自动恢复为 `ready`，不卡在 `loading`。
3. **重启总线重置 `seenSeq` 测试**：`startBus(bus1)` 收到 `Frame seq=5` 并 `stop()` 后，`startBus(bus2)` 收到 `Frame seq=1`，断言 `frameSource` 正常接收到新总线的 `seq=1`。

## 验收

- `RUSTC_WRAPPER="" bash scripts/build/check.sh` 全绿（零 Rust 文件改动，全部 10 个规则脚本、707 Rust 测试、全部 UI 测试、`tsc` 与三入口 `vite build` 全部通过）。

## 不做

- 不修改任何 Rust 代码或 `core/bindings/*.ts`。
