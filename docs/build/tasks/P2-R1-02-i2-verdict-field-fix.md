# P2-R1-02 修正 I2 并发查询测试中的 `judge.verdict` 断言字段

## 目标

修掉 P2 第 1 轮审查的**必须改 #2**（`docs/build/reviews/P2-r1.md`）：

1. 修正 `core/tests/view_scale.rs::i2_concurrent_view_queries_do_not_stall_live_captions` 中检查不存在的字段名 `e.text("verdict")` 的永真断言，改为检查真实字段 `e.text("to")`（`Stalled` / `Dead`）以及 `stall.snapshot` 计数为 0，使 I2 门禁测试真正能抓出停滞或死亡判定。
2. 顺手修正同文件千条种子数据中 `filed_state` 写错成 `"filed"`（应为 `"city.filed"`）的问题。

## 背景

- `docs/build/reviews/P2-r1.md` 必须改 #2；`docs/build/phases/P2.md` §1 E6、§2 I2；`docs/EVENTS.md:179`。
- `core/src/app/recording.rs:700-742` 发出 `judge.verdict` 事件的字段是 `stage`、`lane`、`from`、`to`、`why`、`action`，没有 `verdict` 字段。
- `core/tests/view_scale.rs:1024-1030` 写了 `.filter(|e| e.text("verdict") == Some("Stalled"))`，永远返回 `0`，即使并发查询真的拖垮了字幕也无法让测试失败。
- `core/tests/view_scale.rs:133` 写入 `let filed_state = if (i / 12) % 5 == 0 { "filed" } else { "" };`，而 `core/src/view/index.rs:980, 1039` 认定的已交城代码是 `"city.filed"`。

## 做法

1. **`core/tests/view_scale.rs`**：
   - 将第 1024–1030 行改为：
     ```rust
     let stalled_or_dead = bench
         .seen()
         .of("judge.verdict")
         .into_iter()
         .filter(|e| matches!(e.text("to"), Some("Stalled" | "Dead")))
         .count();
     assert_eq!(stalled_or_dead, 0, "concurrent SQLite queries must never stall live captions");
     assert_eq!(
         bench.seen().count("stall.snapshot"),
         0,
         "concurrent SQLite queries must never trigger a stall snapshot"
     );
     ```
   - 将第 133 行 `"filed"` 改为 `"city.filed"`，并在 `scale_1000_recordings_library_and_fts5_search_under_100ms` 中顺手验证一次 `filed_only: true` 或断言已交城行确实带有 `r.filed == true`（可选，只要保持总耗时与 ≤ 100 ms 预算清晰）。
   - 检查 `core/tests/view_scale.rs` 其余事件字段断言（`index.open`、`query.slow`、`stats.full`）均与 `docs/EVENTS.md` 字段名严格一致。

## 显式与可观测

- **状态与转换**：不改产品代码状态机。
- **数与单位**：沿用 `QUERY_BUDGET_MS` 等既有命名常量。
- **队列与任务**：不新增队列或任务。
- **事件与统计**：严格对齐 `docs/EVENTS.md` 中 `judge.verdict` 的 `to` 字段与 `stall.snapshot` 事件，杜绝测试因字段名拼错出现假阴性。
- **故障可见性**：验证测试中若人为检查 `judge.verdict` 的真实 `to` 字段（对照 `core/tests/liveness.rs:1280` 写法），在并发查询下确实为 0 `Stalled`、0 `Dead`、0 `stall.snapshot`。

## 先写的测试

本任务本身就是修正门禁测试断言：修改前先核对 `core/src/app/recording.rs:700-742` 与 `docs/EVENTS.md:179` 确认字段为 `to`；修改后运行 `RUSTC_WRAPPER="" cargo test -p zutalk-core --test view_scale -- --nocapture`，确认全部 5 条测试通过且数字健康。

## 验收

- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test view_scale -- --nocapture` 全部 5 条通过。
- `cargo fmt --all -- --check`、`clippy --all-targets -- -D warnings` 与全部 9 个规则脚本通过。
- 报告中记录修改后的测试输出与 `view_scale.rs` 全部事件字段名核对结果。

## 不做

- 不放宽 I2/I3/I8 或 ≤ 100 ms 预算的任何阈值。
- 不修改产品代码（如发现修正断言后真出现了 `Stalled`，则停下来如实汇报根因，不要掩盖）。
