状态：完成

# P1-05 地基三：有名字的任务挂在树上；丢弃要写理由：报告

## 改了什么

1. **`core/Cargo.toml`**：新增 `tokio-util = { version = "0.7.20", features = ["rt"] }`（沿用 `Cargo.lock` 中已有版本，未引入新外部下载）。
2. **`core/src/rt/supervise.rs`**：
   - 新增 `Tree`、`Running`（带 `Serialize`）、`Broke`、`TREE_MOST_TASKS`（64）、`rt::supervise::thread`。
   - `Tree::root(name, stats)` 创建根树并在 `Stats` 登记 `task.started`、`task.panics` 与高水位 `tree.tasks`；`child(name)` 创建子树，共享同一张运行表，取消随父树传递并支持独立 `cancel`/`wait`（子树任务同时向祖先 `TaskTracker` 取令牌，避免根树长期持有已结束子树引用）。
   - `Tree::spawn` 在启动前登记运行表、计 `task.started`、更 `tree.tasks` 并发 `info!("task.start", tree, task)`；执行时按三层包装：`zutalk_log::carry` 携带调用者的日志接收者、`AssertUnwindSafe + FutureExt::catch_unwind` 接住 panic、与 `token.cancelled()` 做 `select!`；结束时先清表并发 `task.end`（`code` 为 `done` / `cancelled` / `panic`，panic 为 `error` 级并计 `task.panics`，带虚拟时钟 `ran_ms`），再释放 `TaskTracker` 令牌。对已 `wait()` 收尾的树拒绝起任务并发 `warn!("task.refused", tree, task)`。运行表超过 `TREE_MOST_TASKS`（64）时任务照起、不入表，按表发一次 `warn!("stats.full", kind = "tasks")`。
   - `Tree::blocking` 基于 `tokio::task::spawn_blocking`，经 `zutalk_log::carrier()` 携带日志接收者，在运行表占一行（`kind = "blocking"`），不发 `task.start`/`task.end`；panic 或树取消返回 `Err(Broke)`，不打断已在运行的阻塞线程。
   - `rt::supervise::thread` 用 `std::thread::Builder` 起具名线程并携带日志接收者，发 `thread.start`、`thread.end`（`done`/`panic`，真实时钟 `ran_ms`）。
   - 未改既有 `supervise`、`Backoff`、`again`，未加 `Tree::keep`（留给 `P1-09`）。
3. **`core/src/rt/stats.rs`**：
   - 新增 `STATS_SUMMARY_EVERY_MS = 60_000`、`pub fn write(tally: &Tally, snap: u64)`（写 `stat.count`、`stat.peak`、`stat.spread`、`stat.queue`）、`pub async fn publish(stats: Stats)`（每分钟同步 `zutalk_log::lost()`/`refused()` 到 `log.lost`/`log.refused`，按 `moved` 写 `stats.summary` 及动过的行；无变化时只写 `rows = 0` 的 `stats.summary`）。
   - 将既有 `note_recv` 的 `// dropped:` 移至 `let _ = self` 同一行以符合 `dropped.sh`。
4. **`core/src/app/hub.rs`**：
   - `Core::start` 构造 `Stats::new()` 与 `Tree::root("core", stats)`，在根树上起 `"hub"` 和 `"stats"`（`stats::publish`）；`Hub` 持有根树克隆并在主循环退出时 `self.tree.cancel()`；新增 `Core::stats(&self) -> Stats`。
   - `core/src/app/hub.rs` 中的 `tokio::spawn` 从 13 处减至 12 处（`Core::start` 起 hub 的一处已还清）。
5. **`docs/EVENTS.md`**：补齐 10 条新事件：`task.start`、`task.end`、`task.refused`、`thread.start`、`thread.end`、`stats.summary`、`stat.count`、`stat.peak`、`stat.spread`、`stat.queue`。
6. **`scripts/build/rules/spawn.sh`** 与 **`scripts/build/rules/dropped.sh`**：新建两个规则脚本，并当场将全仓既有 48 处丢弃补齐真实理由通过 `dropped.sh`。

文件：`core/Cargo.toml`、`core/src/rt/supervise.rs`、`core/src/rt/stats.rs`、`core/src/app/hub.rs`、`core/src/ai/write.rs`、`core/src/capture/audio.rs`、`core/src/capture/tape.rs`、`core/src/live/lane.rs`、`core/src/out/city.rs`、`core/src/out/export.rs`、`core/src/record/migrate.rs`、`app/src/main.rs`、`server/src/main.rs`、`server/src/room.rs`、`core/tests/supervise.rs`、`core/tests/stats.rs`、`core/tests/app.rs`、`docs/EVENTS.md`、`scripts/build/rules/spawn.sh`、`scripts/build/rules/dropped.sh`、本报告。

## `dropped.sh` 改代码之前的输出（共 48 处）

```text
app/src/main.rs:33: discarded without `// dropped: <why>`: windows.1.iter().for_each(|last| drop(channel.send(last.clone())));
app/src/main.rs:43: discarded without `// dropped: <why>`: let _ = if state.captions { window.show() } else { window.hide() };
core/src/ai/write.rs:425: discarded without `// dropped: <why>`: let _ = tx.blocking_send(Ok(wav_header(samples)));
core/src/ai/write.rs:427: discarded without `// dropped: <why>`: let _ = read.map_err(|e| tx.blocking_send(Err(e)));
core/src/ai/write.rs:446: discarded without `// dropped: <why>`: let _ = rustls::crypto::ring::default_provider().install_default();
core/src/ai/write.rs:632: discarded without `// dropped: <why>`: let _ = fs::remove_file(dir.join(Journal::FILE));
core/src/app/hub.rs:147: discarded without `// dropped: <why>`: let _ = rustls::crypto::ring::default_provider().install_default();
core/src/app/hub.rs:177: discarded without `// dropped: <why>`: let _ = self.0.send(Msg::Do(command));
core/src/app/hub.rs:183: discarded without `// dropped: <why>`: let _ = self.0.send(Msg::Looked(found));
core/src/app/hub.rs:382: discarded without `// dropped: <why>`: let _ = tx.send(Msg::SignedIn(sign_in.finish(&http).await));
core/src/app/hub.rs:427: discarded without `// dropped: <why>`: let _ = tx.send(Msg::City(city::calendar(&http, &address).await));
core/src/app/hub.rs:451: discarded without `// dropped: <why>`: let said = city::file(&filer, &id, filing, |w| drop(tx.send(Msg::Filing(w)))).await;
core/src/app/hub.rs:452: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Filed(id, said));
core/src/app/hub.rs:530: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Claimed(keys::carried(&token).and(claimed).map_err(code)));
core/src/app/hub.rs:567: discarded without `// dropped: <why>`: let _ = tx.send(Msg::SignedIn(me));
core/src/app/hub.rs:584: discarded without `// dropped: <why>`: let progress = |writing| drop(tx.send(Msg::Moving(writing)));
core/src/app/hub.rs:586: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Moved(moved));
core/src/app/hub.rs:639: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Shared(shared.await));
core/src/app/hub.rs:660: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Shared(gone));
core/src/app/hub.rs:711: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Say(code));
core/src/app/hub.rs:713: discarded without `// dropped: <why>`: owing.into_iter().for_each(|task| drop(tokio::spawn(task.finish())));
core/src/app/hub.rs:729: discarded without `// dropped: <why>`: Err(end) => return drop(tx.send(Msg::Say(code(end)))),
core/src/app/hub.rs:738: discarded without `// dropped: <why>`: wrote.into_iter().for_each(|end| drop(tx.send(Msg::Say(code(end)))));
core/src/app/hub.rs:786: discarded without `// dropped: <why>`: let _ = self.tx.send(msg);
core/src/app/hub.rs:834: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Say("tape"));
core/src/app/hub.rs:835: discarded without `// dropped: <why>`: let _ = tx.send(Msg::Ended(id.clone()));
core/src/app/hub.rs:837: discarded without `// dropped: <why>`: let _ = audio.send(Arc::new(block));
core/src/app/hub.rs:841: discarded without `// dropped: <why>`: let _ = tx.send(match notice {
core/src/capture/audio.rs:316: discarded without `// dropped: <why>`: _ => drop(ender.send(broke(error))),
core/src/capture/audio.rs:445: discarded without `// dropped: <why>`: let _ = ender.send(End::Retry(why));
core/src/capture/audio.rs:549: discarded without `// dropped: <why>`: let _ = self.tx.send(Msg::Pause(true, (self.now_ms)()));
core/src/capture/audio.rs:553: discarded without `// dropped: <why>`: let _ = self.tx.send(Msg::Pause(false, (self.now_ms)()));
core/src/capture/audio.rs:562: discarded without `// dropped: <why>`: let _ = self.tx.send(Msg::Stop);
core/src/capture/audio.rs:564: discarded without `// dropped: <why>`: let _ = thread.join();
core/src/capture/tape.rs:89: discarded without `// dropped: <why>`: let _ = self.file.sync_data();
core/src/live/lane.rs:262: discarded without `// dropped: <why>`: let _ = stop.wait_for(|mode| *mode == Mode::Stopped).await;
core/src/live/lane.rs:265: discarded without `// dropped: <why>`: let _ = timeout(Duration::from_millis(last_words_ms), lane.lock()).await;
core/src/live/lane.rs:303: discarded without `// dropped: <why>`: let _ = self.out.send(event).await; // nobody listening is not this lane's failure
core/src/out/city.rs:337: discarded without `// dropped: <why>`: _ = tx.blocking_send(Ok((t0, file, wav)));
core/src/out/export.rs:45: discarded without `// dropped: <why>`: let _ = write!(out, "\n{who}{}\n{}\n", &clock(line.t0, '.')[..8], line.text);
core/src/out/export.rs:82: discarded without `// dropped: <why>`: let _ = write!(out, "{n}\n{start} --> {end}\n{said}\n\n");
core/src/record/migrate.rs:162: discarded without `// dropped: <why>`: let _ = fs::remove_dir_all(data.join("tape").join(&id));
core/src/rt/stats.rs:181: discarded without `// dropped: <why>`: let _ = self
server/src/main.rs:37: discarded without `// dropped: <why>`: _ = rustls::crypto::ring::default_provider().install_default();
server/src/room.rs:73: discarded without `// dropped: <why>`: _ = self.live.send(event("log", &frame).id((self.frames.len() + 1).to_string()));
server/src/room.rs:93: discarded without `// dropped: <why>`: _ = room.live.send(event("gone", ""));
server/src/room.rs:104: discarded without `// dropped: <why>`: _ = remove(server, &mut rooms, &id);
server/src/room.rs:152: discarded without `// dropped: <why>`: _ = room.live.send(event("slot", &room.slot.1));
```

### 归到第 2 种（其实有关系）的丢弃清单

以下 7 处保留现有行为，补齐了实话理由，指给重写对应模块的任务：

| 文件与行号（改前） | 原代码 | 为什么有关系 | 指给哪件任务 |
|---|---|---|---|
| `app/src/main.rs:43` | `let _ = if state.captions { window.show() } else { window.hide() };` | 显示/隐藏字幕窗的 OS 错误被吞 | `P1-23`（外壳重写 `windows.rs` 与 `shell.captions`） |
| `core/src/ai/write.rs:632` | `let _ = fs::remove_file(dir.join(Journal::FILE));` | 清理已完成日志文件的 IO 错误被吞 | `P1-21`（后台转写收进树与任务清理） |
| `core/src/app/hub.rs:713` | `owing.into_iter().for_each(\|task\| drop(tokio::spawn(task.finish())));` | 丢掉任务句柄，后台收尾若 panic 没人看见 | `P1-21`（后台收尾迁至 `Tree`） |
| `core/src/capture/audio.rs:564` | `let _ = thread.join();` | `Drop` 吞掉采集泵/文件读线程的 panic | `P1-14`（改用 `rt::supervise::thread`，panic 留 `thread.end` 事件） |
| `core/src/capture/tape.rs:89` | `let _ = self.file.sync_data();` | 关闭 tape 时 `Drop` 吞掉落盘 `fsync` 错误 | `P1-15`（写盘部件显式 `sync` 并上报失败） |
| `core/src/record/migrate.rs:162` | `let _ = fs::remove_dir_all(data.join("tape").join(&id));` | 搬运失败回滚删临时 tape 目录的错误被吞 | `P1-21`（后台搬运收进 `Work`） |
| `server/src/room.rs:104` | `_ = remove(server, &mut rooms, &id);` | 定时清扫过期房间删文件失败被吞，只等下一轮重试 | `P5`（房间服务期） |

（另注：`core/src/app/hub.rs:891` 的 `Ok(Some(0)) => drop(fs::remove_dir_all(self.tape())),` 不在 `dropped.sh` 四种字面模式内，但同样吞了空录音删目录错误，将在 `P1-16`/`P1-19` 重写录音收尾时一并处理。）

## `spawn.sh` 欠账表

| 类别 | 文件 | 处数 | 谁还 |
|---|---|---|---|
| 欠账 | `core/src/app/hub.rs` | 14 | `P1-16` / `P1-19` / `P1-20`（本件已还起 hub 的 1 处，剩余 12 处 `tokio::spawn` + 2 处 `spawn_blocking`） |
| 欠账 | `core/src/capture/audio.rs` | 3 | `P1-14` |
| 欠账 | `core/src/ai/write.rs` | 2 | `P1-21` |
| 欠账 | `core/src/out/city.rs` | 1 | `P1-21` |
| 欠账 | `app/src/main.rs` | 1 | `P1-23` |
| 以后 | `server/src/ledger.rs` | 2 | `P4` |
| 以后 | `server/src/lib.rs` | 1 | `P5` |

## 证据

### 新测试（11 条全过）

- `core/tests/supervise.rs`（新增 9 条，共 14 条全过）：
  1. `a_tree_cancels_and_waits_for_all_it_started`
  2. `a_task_that_ends_on_its_own_says_done`
  3. `a_task_that_panics_is_seen_and_takes_nothing_with_it`
  4. `a_child_can_be_cancelled_alone`
  5. `nothing_starts_on_a_tree_that_has_closed`
  6. `a_blocking_call_gives_back_what_it_made_and_a_panic_as_broke`
  7. `what_a_task_says_is_heard_by_whoever_started_the_tree`
  8. `a_named_thread_says_when_it_starts_and_ends`
  9. `more_tasks_than_the_table_holds_still_run`
- `core/tests/stats.rs`（新增 1 条，共 10 条全过）：
  10. `once_a_minute_what_moved_is_written`
- `core/tests/app.rs`（新增 1 条，通过）：
  11. `the_core_runs_on_a_tree_with_a_name`

### 一分钟汇总的真实捕获日志行（来自测试 10 `once_a_minute_what_moved_is_written`）

```text
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.start", fields: [("tree", Text("test")), ("task", Text("stats"))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stats.summary", fields: [("snap", U64(0)), ("up_ms", U64(60000)), ("rows", U64(3))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stat.count", fields: [("snap", U64(0)), ("name", Text("frame.made")), ("n", U64(7))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stat.count", fields: [("snap", U64(0)), ("name", Text("hub.turns")), ("n", U64(3))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stat.spread", fields: [("snap", U64(0)), ("name", Text("hub.turn_us")), ("n", U64(3)), ("p50", U64(20)), ("p99", U64(100)), ("most", U64(80))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stats.summary", fields: [("snap", U64(0)), ("up_ms", U64(120000)), ("rows", U64(0))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stats.summary", fields: [("snap", U64(0)), ("up_ms", U64(180000)), ("rows", U64(1))] }
Line { level: Info, component: "zutalk_core::rt::stats", code: "stat.count", fields: [("snap", U64(0)), ("name", Text("frame.made")), ("n", U64(9))] }
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.end", fields: [("tree", Text("test")), ("task", Text("stats")), ("code", Text("cancelled")), ("ran_ms", U64(180000))] }
```

### 规则脚本证明（试完均已还原）

```text
=== dropped.sh violation proof ===
core/src/lib.rs:16: discarded without `// dropped: <why>`: fn _proof_drop() { let _ = std::fs::remove_file("x"); }
exit=1
=== dropped.sh annotated proof ===
exit=0
=== spawn.sh new-spawn proof ===
core/src/lib.rs:16: task/thread spawned outside core/src/rt/supervise.rs (tokio::spawn()
exit=1
```

### 验证清单

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部退出码 0。
- `git grep -c "tokio::spawn" core/src/app/hub.rs`：由 13 降为 12（比改前少一处）。
- 全量门禁 `scripts/build/check.sh` 全绿：Rust `passed 475, failed 0, ignored 6`，界面 `svelte-check` 0 errors 0 warnings、`27 passed`。

## 留下的，要负责人的

- `spawn.sh` 欠账表里的 21 处核心/外壳任务由 `P1-14`、`P1-16`、`P1-19`、`P1-20`、`P1-21`、`P1-23` 依次还清；`server/src` 的 3 处留给 `P4`、`P5`。
