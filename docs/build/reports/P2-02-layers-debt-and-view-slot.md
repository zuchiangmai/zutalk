状态：完成

## 改动与文件

1. **解耦 `core/src/record/retain.rs`**
   - 移除对 `crate::app::proto::{Row, State}` 的向上依赖，定义自有轻量值类型 `Entry { id, started, draft, audio }`。
   - `retain::sweep` 改为接受 `keep: Option<u32>` 与 `entries: &mut [Entry]`，销毁到期非草稿音频、置 `audio = false`，并显式返回被销毁音频的录音 id 列表（`Result<Vec<String>, &'static str>`）。
   - `retain::destroy` 改为接受 `entries: &[Entry]`。
2. **解耦 `core/src/record/migrate.rs`**
   - 移除对 `crate::app::proto::Writing` 的向上依赖，定义自有值类型 `Progress { id, step, done, of }`。
   - `migrate::run` 回调签名改为 `mut progress: impl FnMut(Progress)`。
3. **适配 `core/src/app/work.rs` 与测试**
   - `Work::migrate`、`Job::DestroyAudio`、`run_refresh` 适配新签名并移除 `work.rs` 中多余的 `State` 导入。
   - `core/tests/retain.rs` 新增单测 `sweep_destroys_only_due_written_entries_and_returns_their_ids`；`core/tests/migrate.rs` 改用 `migrate::Progress`。
4. **开通 `view` 层并清还 P2 欠账（`core/src/view.rs`、`core/src/lib.rs`、`scripts/build/rules/layers.sh`）**
   - 新增 `core/src/view.rs` 并在 `core/src/lib.rs` 导出 `pub mod view;`。
   - `scripts/build/rules/layers.sh` 加入 `view: rt capture live record knowledge out`，`app` 增加 `view` 依赖许可，删除 `record/retain.rs` 与 `record/migrate.rs` 两行 P2 欠账（仅余 P5 的 `out/city.rs`、`out/room.rs`）。

## 证据

- `cargo fmt --all -- --check` 通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 9 个规则脚本全绿，其中 `bash scripts/build/rules/layers.sh` 通过，`grep -n 'record/' scripts/build/rules/layers.sh` 返回 0 行，`git grep 'app::proto' core/src/record/` 返回空。
- `core/tests/retain.rs`（非 TCP 5/5 通过，含新增 `sweep_destroys_only_due_written_entries_and_returns_their_ids`）。
- `core/tests/migrate.rs`（5/5 通过）。
- `ui` `svelte-check`（0 errors）与 `npm test`（35/35）全绿。

## 遗留 / 待负责人确认

- `out/city.rs` 与 `out/room.rs` 的两行 `layers.sh` 欠账按计划留至 P5 清理。
- `retain::sweep` 返回的被销毁 id 列表将在 P2-05 用于增量更新 SQLite 索引与推送失效。
