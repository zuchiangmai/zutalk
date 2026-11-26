状态：完成

# P2-R1-02 修正 I2 并发查询测试中的 `judge.verdict` 断言字段：报告

## 改了什么

1. **`core/tests/view_scale.rs`**：
   - 将 `i2_concurrent_view_queries_do_not_stall_live_captions` 的 `judge.verdict` 过滤条件从不存在的 `e.text("verdict") == Some("Stalled")` 修正为真实字段 `matches!(e.text("to"), Some("Stalled" | "Dead"))`，并新增 `assert_eq!(bench.seen().count("stall.snapshot"), 0, ...)`。
   - 将第 133 行千条种子数据中的 `filed_state` 从 `"filed"` 修正为 `core/src/view/index.rs` 认定的 `"city.filed"`。

## 事件字段名核对（`core/tests/view_scale.rs` 对照 `docs/EVENTS.md`）

- `judge.verdict`：`to`（`"Stalled"` | `"Dead"`）✔
- `stall.snapshot`：事件计数为 `0` ✔
- `index.open`：`mode`（`"rebuilt"`）、`why`（`"corrupt"`）✔
- `query.slow`：事件计数为 `0` ✔
- `stats.full`：事件计数为 `0` ✔

## 证据

```text
running 5 tests
i8_chunk: tr_1m=18641us tr_4h_c0=16786us tr_4h_c39=6480us tr_ratio=0.35 | cmp_1m=11761us cmp_4h_c0=17048us cmp_4h_c39=11679us cmp_ratio=0.99 | tr_chunk_bytes=13573 cmp_chunk_bytes=26045
test i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration ... ok
scale_10h: seed=575ms transcript=[c0=7.99ms, c50=16.77ms, c99=5.50ms] compare=[c0=17.88ms, c50=6.16ms, c99=6.79ms]
test scale_10_hour_transcript_chunk_and_compare_under_100ms ... ok
scale_1000: seed=4696ms summary=1.67ms month_p2=1.15ms month_filt=1.37ms recent=1.27ms searches=[cjk4=1.24ms, cjk2=7.60ms, thai_sara_am=2.29ms, en_case=1.82ms, note=1.81ms, title=1.50ms]
test scale_1000_recordings_library_and_fts5_search_under_100ms ... ok
i3_bounds: rebuild_100=90ms tally_bytes=390B draft_writer=48B draft_10k_write=1297ms inv_len=64
i2_live_queries: ok=16/20 cancelled=4 frames=6->36 total_after=1001
test i3_rebuild_queries_and_draft_stay_within_bounds ... ok
test i2_concurrent_view_queries_do_not_stall_live_captions ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.60s
```
