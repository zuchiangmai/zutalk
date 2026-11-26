# P2-02 清还 `layers.sh` P2 欠账并开通 `view` 层

## 目标

做完以后：

1. `core/src/record/retain.rs` 与 `core/src/record/migrate.rs` 不再向上依赖 `crate::app::proto`。
2. `scripts/build/rules/layers.sh` 中 P2 的两行欠账（`record/retain.rs`、`record/migrate.rs`）被彻底删除，只剩留给 P5 的两行（`out/city.rs`、`out/room.rs`）。
3. `scripts/build/rules/layers.sh` 正式加入 `view` 层（允许依赖 `rt capture live record knowledge out`），`app` 层允许依赖 `view`；`core/src/lib.rs` 与 `core/src/view.rs` 就位。

## 背景

- 设计：`docs/build/phases/P2.md` §3.1；`docs/PLAN.md` §5.1。
- 涉及文件：
  - `core/src/record/retain.rs`（当前第 6 行 `use crate::app::proto::{Row, State};`）
  - `core/src/record/migrate.rs`（当前第 10 行 `use crate::app::proto::Writing;`）
  - `core/src/app/work.rs`（调用 `retain::sweep`、`retain::destroy`、`migrate::run` 的三处）
  - `core/src/lib.rs`、新增 `core/src/view.rs`
  - `scripts/build/rules/layers.sh`
  - `core/tests/retain.rs`、`core/tests/migrate.rs`

## 做法

### 1. 解耦 `core/src/record/retain.rs`

- 定义自有轻量值类型：
  ```rust
  /// What retention needs to know about one recording.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Entry {
      pub id: String,
      pub started: i64,
      pub draft: bool,
      pub audio: bool,
  }
  ```
- 改写两个函数（纯规则 `goes` 签名和行为完全不变）：
  - `pub fn sweep(tapes: &Path, keep: Option<u32>, entries: &mut [Entry], now: i64) -> Result<Vec<String>, &'static str>`：销毁到期且非草稿的音频，把对应 `entry.audio` 置为 `false`，并返回被销毁音频的录音 id 列表（供 P2-05 增量更新 SQLite 索引与发出失效通知）。
  - `pub fn destroy(tapes: &Path, entries: &[Entry], under_way: Option<&str>, id: &str) -> Result<(), &'static str>`：检查 `under_way == Some(id)` 与 `id` 在 `entries` 中存在，调 `tape::destroy`。
- 同步更新 `core/src/app/work.rs`（`run_refresh` 与 `Job::DestroyAudio`）适配新签名。

### 2. 解耦 `core/src/record/migrate.rs`

- 定义自有进度值类型：
  ```rust
  /// One progress report while moving recordings from ZuTalk 0.8.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Progress {
      pub id: String,
      pub step: &'static str,
      pub done: u32,
      pub of: u32,
  }
  ```
- `migrate::run` 回调签名改为 `mut progress: impl FnMut(Progress)`，内部构造 `Progress { id: id.clone(), step: "moving", done: done as u32, of }`。
- `core/src/app/work.rs` 在 `Work::migrate` 闭包里把 `migrate::Progress` 转成 `proto::Writing { id: p.id, step: p.step.into(), done: p.done, of: p.of }`。
- 同步更新 `core/tests/migrate.rs` 中的 `Vec<migrate::Progress>` 类型标注（断言逻辑不变）。

### 3. 开通 `view` 层并更新 `layers.sh`

- 新建 `core/src/view.rs`（模块说明注释：`//! Disposable local projections (L3, <= 100 ms): SQLite index, chunked transcripts, diffs, and queries.`），并在 `core/src/lib.rs` 导出 `pub mod view;`。
- 修改 `scripts/build/rules/layers.sh`：
  - `allowed` 表加入：
    ```text
    view: rt capture live record knowledge out
    app: rt capture live id record knowledge ai out view
    ```
  - `debt` 表删除 `record/retain.rs crate::app::proto 1 P2` 与 `record/migrate.rs crate::app::proto 1 P2` 两行。

## 显式与可观测

- **层边界显式**：`layers.sh` 从静态规则上彻底锁死 `record/` 不得回指 `app/`、且新 `view/` 层不得依赖 `app/` / `ai/` / `id/`。
- **状态机 / 事件 / 队列 / 数**：本件为纯重构与层定义，不新增状态机、事件、队列或常量；`retain::sweep` 显式返回被销毁的 id 列表，不吞掉任何删除结果。

## 先写的测试

- 在 `core/tests/retain.rs` 补一条纯单测 `sweep_destroys_only_due_written_entries_and_returns_their_ids`：不启动 `Core`，直接构造 `Vec<retain::Entry>` 验证 `retain::sweep` 返回被毁音频的 id 并置 `audio = false`，草稿与未到期项保留。
- 现有 `core/tests/retain.rs` 与 `core/tests/migrate.rs` 全部行为测试继续通过。
- 运行 `bash scripts/build/rules/layers.sh` 验证两处 P2 欠账已清零且 `view` 层生效。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿。
- `grep -n 'record/' scripts/build/rules/layers.sh` 在 `debt` 段中返回 0 行；`git grep 'app::proto' core/src/record/` 返回空。

## 不做

- 不动 `out/city.rs` 与 `out/room.rs` 的 P5 欠账。
- 不在本件引入 `rusqlite` 或实现 `view/index.rs`（P2-03）。
