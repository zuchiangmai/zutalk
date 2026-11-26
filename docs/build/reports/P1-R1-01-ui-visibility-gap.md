状态：完成

# P1-R1-01 界面隐藏切回时不误报 `ui.frame.gap`：报告

## 改了什么

1. **`ui/src/lib/core.svelte.ts`**：
   - 在 `use(b: Bus)` 启动 `requestAnimationFrame` 循环的同时，检查 `typeof document !== 'undefined' && typeof document.addEventListener === 'function'` 并注册 `visibilitychange` 监听；当 `document.hidden` 为真时立刻调用既有纯函数 `vitals = frame(vitals, 0, true)` 将 `vitals.last` 复位为 `null`；在返回的清理函数中用 `removeEventListener` 移除该监听。
2. **`ui/tests/store.test.ts`**：
   - 新增测试 `resets frame timing on visibilitychange so returning from background reports no false ui.frame.gap`，按真实浏览器时序（后台期间暂停 rAF、切回前台第一帧相隔 10 s）验证切回零误报、前台真实卡顿 468 ms 准确上报 `{ gap_ms: 468, gaps: 1 }`，并验证清理函数移除 rAF 与事件监听。

## 证据

### 先写测试的失败输出（改 `core.svelte.ts` 之前）

```text
 FAIL  tests/store.test.ts > store > resets frame timing on visibilitychange so returning from background reports no false ui.frame.gap
AssertionError: expected [ { seq: 5, gap_ms: 10000, gaps: 1 } ] to deeply equal []
```

### 修复后的验证输出

```text
svelte-check found 0 errors and 0 warnings
 Test Files  6 passed (6)
      Tests  35 passed (35)
```

全量门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh` 将与 `P1-R1-02` 一并验证并核验通过。
