状态：完成

# P1-R1-02 修正 `scripts/build/water.py` 的网络活统计与同拍水位落后：报告

## 改了什么

1. **`scripts/build/water.py`**（60 行纯标准库）：
   - 新增具名常量 `NET_JOB_KINDS = frozenset({"signin", "create", "claim", "calendar", "share", "unshare", "slot", "file"})`（对照 `docs/EVENTS.md` 中 `job.start` 的网络类 `kind`），替代此前永远无法命中的 `kind.startswith("net.")`；当命中数 > 0 时打印具体种类列表 `net_jobs=N (kind1, kind2)`。
   - 按同一采样拍结算 `water.mark`（`snap == 0`），通过 `flush_tick()` 先收集整拍所有格再计算落后，使 `Draft` 比较的是同一拍的 `Final(0)`，不再受写出顺序影响。
   - 删除未使用的死常量 `BEAT_STALLED_MS`。

## 证据

### 修改前的失败输出（含 `job.start(claim)`、`job.start(share)` 与同拍 `Draft=0 / Final(0)=1000`）

```text
files=1 seq_gaps=0 log.lost=0
water Capture: samples=1 max_lag=0 missing_s=0
water Draft: samples=1 max_lag=0 missing_s=0
water Final(0): samples=1 max_lag=0 missing_s=0
net_jobs=0
caption.lag_ms: (none)
```

### 修改后的正反两例通过输出

```text
=== NEG ===
files=1 seq_gaps=0 log.lost=0
water Capture: samples=1 max_lag=0 missing_s=0
water Draft: samples=1 max_lag=1000 missing_s=0
water Final(0): samples=1 max_lag=0 missing_s=0
net_jobs=0
caption.lag_ms: (none)

=== POS ===
files=1 seq_gaps=0 log.lost=0
water Capture: samples=1 max_lag=0 missing_s=0
water Draft: samples=1 max_lag=1000 missing_s=0
water Final(0): samples=1 max_lag=0 missing_s=0
net_jobs=2 (claim, share)
caption.lag_ms: (none)

BOTH ASSERTIONS PASSED
```

全量门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh` 验证通过后本机提交。
