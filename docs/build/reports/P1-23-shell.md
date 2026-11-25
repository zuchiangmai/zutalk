状态：完成

## 改了什么、动了哪些文件

- **外壳库目标与依赖**：
  - `app/Cargo.toml`：`tokio` 新增 `sync` 特性，`[dev-dependencies]` 新增 `tokio`（`rt`、`test-util`）。
  - `app/src/lib.rs`：新增库入口，导出 `pump`、`updates`、`windows`。
- **无锁推送状态机与受监督任务（`app/src/pump.rs`）**：
  - 实现纯状态机 `Fan<S>` 与 `Sink` trait（不依赖 Tauri、不读时钟、不起任务、不发日志）：`join` 按窗口标签替换旧通道并按固定次序重放最新 `State` 与五种信号（不重放 `Notice`）；`push` 拒绝同种信号的倒退/重复序号，并用 `catch_unwind` 兜住单个通道的 panic（返回 `"closed"` / `"panic"`）。
  - 实现 `run` 与受监督启动 `start`（`shell.keep("shell.pump", ...)`）：监听 `shell.joins`（`SHELL_JOINS_MSGS = 8`，`Full::Wait`）与 `Outlet` 七个 `Reading`；发出 `shell.join`、`shell.sink.gone`（计数 `shell.sinks.gone`）、`shell.captions`；每推一帧累加 `shell.pushed` 并回调 `core.pushed(seq)`；安全游标推进 `said` 提示；仅在 `State.captions` 相对上次请求变化时调用字幕窗显隐。
  - `PumpPart` 在 `Drop` 时经无锁 `oneshot` 把完整的 `PumpState`（含 `Fan` 与 `joins` 接收端）交回监督器，内层 panic 重启后已订阅窗口无需重新订阅，全程零锁。
- **窗口与更新（`app/src/windows.rs`、`app/src/updates.rs`）**：
  - `app/src/windows.rs`：非阻塞 `set_captions` 直接投递 `show`/`hide`，不查询主线程状态，返回 `CaptionWindow::{Ok, Missing, Failed}` 由 `shell.captions` 记录结果。
  - `app/src/updates.rs`：在 `shell` 树上启动 `updates` 任务独占已下载更新包，移除 `Ready` 锁；按 `LOOK_MS` 检查并记录 `update.look`（`none` / `ready` / `update`）；`Install` 端口经 1 槽有界通道（`shell.updates`，`Full::Reject`）与 `oneshot` 回话，支持失败重试且无任何 `Mutex`。
- **薄接线与全异步命令（`app/src/main.rs`）**：
  - `dispatch` 与 `subscribe` 全改为 `async fn`，`dispatch` 直接把核心拒绝代码作为 `Err(String)` 返回界面。
  - `setup` 中移除 `block_on`（改为 `tauri::async_runtime::handle().inner().enter()`）与旧版 `shell.forward`、`Windows`、`Ready`。
- **界面拒绝提示**：
  - `ui/src/lib/core.svelte.ts`：`send` 捕获 `bus.send` 的拒绝并 `say` 出来。
  - `ui/tests/store.test.ts`：新增测试第 10 条。
- **规则脚本与事件文档**：
  - `scripts/build/rules/shell.sh`（可执行）：守 `app/src` 全部 `#[tauri::command]` 为 `async fn`、禁用 `Mutex`/`RwLock`/`.lock()`/`block_on` 以及 12 种会阻塞等主线程回话的窗口查询。
  - `scripts/build/rules/spawn.sh`：删除最后一条欠账 `app/src/main.rs 1 P1-23`（`spawn.sh` 与 `bounded.sh` 在 `core/src`、`app/src` 的欠账表均已清零）。
  - `docs/EVENTS.md`：新增 `shell.join`、`shell.sink.gone`、`shell.captions`、`update.look`。

## 证据

### 新增与保持通过的测试（全绿）

`app/tests/pump.rs`（9/9 通过）：
1. `every_interleaving_ends_with_each_window_holding_the_latest`（120 种交错全穷举）
2. `a_page_that_is_gone_is_let_go`
3. `a_signal_older_than_the_last_one_sent_is_not_sent`
4. `a_window_that_joins_late_gets_the_state_and_every_signals_latest`
5. `a_reload_while_frames_are_coming_loses_nothing_and_the_old_channel_goes_quiet`
6. `the_captions_window_follows_the_state_and_is_never_asked`
7. `codes_are_said_once_each`
8. `a_channel_that_panics_does_not_end_the_pump`
9. `a_pump_that_dies_is_started_again_and_no_window_has_to_join_again`

`app/tests/updates.rs`（3/3 通过，未改动）。

`ui/tests/store.test.ts`（第 10 条，7/7 通过；全部 `ui` 30/30 通过）：
10. `says the refusal code when a command is rejected by the bus`

### `shell.sh` 对改前 `app/src/main.rs` 的抓错证明

在修改 `app/src/main.rs` 之前运行 `bash scripts/build/rules/shell.sh`，退出码 `1`，准确报出同步命令、`Mutex`、`.lock()`、`is_visible`、`block_on`：

```text
app/src/main.rs:7: forbidden in shell (Mutex)
app/src/main.rs:19: forbidden in shell (Mutex)
app/src/main.rs:21: forbidden in shell (Mutex)
app/src/main.rs:27: #[tauri::command] must be async fn
app/src/main.rs:34: #[tauri::command] must be async fn
app/src/main.rs:35: forbidden in shell (.lock())
app/src/main.rs:42: forbidden in shell (.lock())
app/src/main.rs:45: forbidden in shell (is_visible)
app/src/main.rs:173: forbidden in shell (.lock())
app/src/main.rs:201: forbidden in shell (.lock())
app/src/main.rs:247: forbidden in shell (block_on)
EXIT=1
```

### 验收 grep 与门禁结果

- `grep -rn "Mutex\|is_visible\|block_on\|WATER_LOG\|async_runtime::spawn" app/src`：无输出（0 命中）。
- `cargo fmt --all -- --check`：通过。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 8 个规则脚本（含 `shell.sh`、欠账已空的 `spawn.sh` 与 `bounded.sh`）：全部通过。
- `ui`：`svelte-check` 0 errors / 0 warnings，`vitest` 5/5 文件、30/30 通过。

## 留待后续 / 需要负责人（已记入给 P1-26 的清单）

门禁里跑不了真实桌面窗口；以下两项需由负责人在真机上核对（将在 P1-26 汇总进总清单）：

1. 开发模式下连按十次重新加载页面，确认字幕和状态都能立刻接上，且 `<数据目录>/logs/zutalk.jsonl` 中有十条 `replaced: true` 的 `shell.join`。
2. 录音中连续开关字幕窗二十次不卡顿，且日志中有二十条 `shell.captions`。
