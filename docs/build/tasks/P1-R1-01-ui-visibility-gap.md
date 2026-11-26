# P1-R1-01 界面隐藏切回时不误报 `ui.frame.gap`

## 目标

修掉 P1 第 1 轮审查的**必须改 #1**（`docs/build/reviews/P1-r1.md`）：

1. 窗口切到后台（`document.hidden === true`）再切回前台时，即使后台期间浏览器完全没有调用 `requestAnimationFrame`，恢复后的第一帧也绝不算作帧间隔过长，不触发 `ui.frame.gap`。
2. `ui` 里补上确定性自动化测试，覆盖「收到 `visibilitychange` 隐藏 → 无隐藏期 rAF → 切回前台第一帧间隔远超 `FRAME_GAP_MS`」的真实浏览器时序。

## 背景

- `docs/build/phases/P1.md` §3.13.8：「页面藏着的时候（`document.hidden`）浏览器本来就不叫 rAF：不算，藏过以后的第一帧也不算。」
- `docs/build/tasks/P1-24-vitals.md` 验收清单：「录音中把主窗口切到后台再切回来：没有因此多出 `ui.frame.gap`」。
- 当前代码 `ui/src/lib/core.svelte.ts:64-82` 只在 `requestAnimationFrame` 回调 `step(now)` 里传 `document.hidden` 给 `frame(vitals, now, hidden)`。因为浏览器在 `document.hidden === true` 时暂停 rAF，`step` 在隐藏期间根本不会被调用，`vitals.last` 一直停在隐藏前那一帧的时间戳；切回前台后 `document.hidden` 已经变回 `false`，第一帧就把整段后台时间误报成 `ui.frame.gap`。

## 做法

1. **`ui/src/lib/core.svelte.ts`**：
   - 在 `use(b: Bus)` 启动 rAF 循环的同时，若 `typeof document !== 'undefined'` 且存在 `addEventListener`，注册 `visibilitychange` 监听：当 `document.hidden` 为真时立刻把状态过一遍 `vitals = frame(vitals, 0, true)`（将 `vitals.last` 清为 `null`）；在 `use` 返回的清理函数里用 `removeEventListener` 拆掉该监听。
   - 保持 `vitals.ts` 纯函数接口与 `O(1)` 空间不变，不引入数组或新机制。
2. **测试（`ui/tests/store.test.ts` 或 `ui/tests/vitals.test.ts`）**：
   - 先写失败测试：模拟浏览器真实行为——安装可控的 `requestAnimationFrame` / `cancelAnimationFrame` 与 `document`（`hidden` + `visibilitychange` 监听），调用 `use(bus)`：
     - 正常跑两帧（`t = 0`, `t = 16`）；
     - 切后台：`document.hidden = true` 并派发 `visibilitychange`，期间**不调用** rAF；
     - 10 秒后切回前台：`document.hidden = false` 并派发 `visibilitychange`，再跑恢复后的第一帧（`t = 10_016`）与下一帧（`t = 10_032`）；
     - 断言 `bus.drawn` 没有任何 `gaps > 0` 的上报（或当 `seq` 未涨时完全不调用 `drawn`）；
     - 随后在前台真正卡顿一帧（`t = 10_500`，间隔 `468 ms > FRAME_GAP_MS`），推进到 `t = 11_100` 后断言 `bus.drawn` 准确上报 `{ gap_ms: 468, gaps: 1 }`；
     - 调用 `use` 返回的清理函数，断言监听器和 rAF 均已移除。

## 显式与可观测

- **状态与转换**：沿用 `vitals.ts` 的 `Vitals`（`last: number | null`）与纯函数 `frame(v, now, hidden)`；隐藏事件通过同一纯函数复位 `last = null`，不新增状态字段或第二套机制。
- **数与单位**：沿用 `FRAME_GAP_MS = 250`、`REPORT_EVERY_MS = 1000`，不引入无名常量。
- **队列与任务**：不新增队列或后台任务；事件监听由 `use()` 返回的清理函数显式释放。
- **事件与统计**：不新增事件或统计；确保真机切后台再切回时 `ui.frame.gap`、`ui.gaps`、`ui.gap_ms` 不被误报污染，而前台真实卡顿仍照常上报。
- **故障可见性**：新增测试同时断言「切后台切回 0 误报」与「前台真实卡顿 468 ms 被 `drawn` 如实上报」。

## 先写的测试

在改 `ui/src/lib/core.svelte.ts` 之前先写上述测试并运行 `cd ui && npm test`，确认测试因切回前台第一帧误报 `gaps = 1, gap_ms = 10000` 而失败；再补上 `visibilitychange` 监听，确认通过。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（或在沙箱内先过 `ui` `npm run check && npm test` 与全部规则脚本，再由协调器跑全量 `check.sh`）。
- 报告中贴出「先写测试的失败输出」和修复后的通过输出。

## 不做

- 不修改 `ui/src/lib/vitals.ts` 的阈值或纯函数签名（除非必要）。
- 不改动 Rust 侧 `beat.rs` 或 `main.rs`。
