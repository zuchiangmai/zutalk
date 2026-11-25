状态：完成

# P2-R1-01 修正 `ViewEngine::query` 的版本号与数据源快照读取顺序：报告

## 改了什么

1. **`core/src/view/query.rs`（`ViewEngine::query`）**：
   - 占槽（`claim_slot`）前仅短锁读取 `ready: bool`，不再提前克隆整份 `ViewSource`。
   - 获取信号量许可 `_permit` 并二次确认未取消后，先执行 `let version = self.0.version.load(Ordering::Acquire)`，再短锁克隆最新 `src: ViewSource`（若此时 `!src.ready` 则清理槽位并返回 `Err("starting")`），随后传入 `tree.blocking("view.query", ...)`。
   - 与写端「先更新 `ViewSource`/提交 SQLite，再 `version.fetch_add(1, AcqRel)`」形成严格的 Acquire/Release 顺序，杜绝旧快照盖上新版本号的竞态。
2. **`core/tests/view.rs`**：
   - 新增确定性回归测试 `view_engine_never_stamps_stale_source_with_newer_version`：逐个占满 8 个信号量许可，排入 `Query::Calendar`，在排队期间调用 `set_calendar(Some(fresh))` + `invalidate(ViewKind::Calendar, None)`，放行后断言返回的日历与 `v_new` 严格一致且前序被取代查询记入 `query.cancelled`。

## 证据

### 先写测试的失败输出（改 `core/src/view/query.rs` 之前）

```text
thread 'view_engine_never_stamps_stale_source_with_newer_version' panicked at core/tests/view.rs:1231:5:
stale Calendar(None) must never be stamped with version 1 >= v_new 1: reply=QueryReply { version: 1, value: Calendar(None) }
test view_engine_never_stamps_stale_source_with_newer_version ... FAILED
```

### 修复后的验证输出

```text
running 11 tests
test view_engine_never_stamps_stale_source_with_newer_version ... ok
...
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.57s
```

`cargo fmt --all -- --check`、`clippy --workspace --all-targets -- -D warnings`、全部 9 个规则脚本均通过；随后经完整 `scripts/build/check.sh` 全绿后提交。
