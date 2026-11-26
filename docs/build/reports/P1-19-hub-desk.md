状态：完成

# P1-19 hub 只算不做（一）：`Work`、desk、资料的读写：报告

## 改了什么

1. **拆出 `core/src/app/core.rs` 与新建 `core/src/app/work.rs`**：
   - 将 `Config`、`Clip`、`Install`、`Outlet`、`Voice`、`Core` 移入 `core/src/app/core.rs`（`hub.rs` 原样 re-export，公开路径不变），给 `Core` 增加 `running(&self) -> Vec<Running>` 供测试与自检读取根树运行表；`rt::supervise::Tree` 增加 `is_cancelled(&self) -> bool`。
   - 新建 `core/src/app/work.rs`：
     - 有名字的 `desk` 队列（`queue::bounded(&stats, "desk", DESK_QUEUE_JOBS, Full::Reject)`，`DESK_QUEUE_JOBS = 64`）和泛型 `Desk<J>` 串行执行器，挂在 `work` 子树（`root.child("work")`）上，逐件以 `tree.blocking("desk.job", ...)` 执行，捕获 panic（`Broke` → `code = "panic"`）并继续下一件。
     - `Work` 持有 `Config`、HTTP 客户端、`results`/`progress` 邮箱发信口、`said`、`work` 子树以及 `Who` / `Shelf` / `Ours` / `Chain` 身份快照；`Hub` 不再持有 `shelf`、`who`、`keys`、`ours`、`slot` 字段。
     - 每件活发出 `job.start`（`kind`、`job`、`waited_ms`）与 `job.done`（`kind`、`job`、`code`、`ms`，`code != "ok"` 为 `warn!`）；统计记入 `desk.wait_ms`、`desk.job_ms`、`jobs.done`、`jobs.failed`、`search.stale`、`refresh.merged`。注：因 `zutalk-log` 行头已占 `seq` 字段名（`log/src/line.rs` `HEAD`），作业序号字段命名为 `job` 以避免被日志层拒收。
2. **资料读写与后台任务迁出 `core/src/app/hub.rs`**：
   - `Change`、`Remove`、`Restore`、`Term`、`DestroyAudio`、`File`（`city::begin`）、`OwnKey`、身份切换 `Sign` 全部改为 `desk` 上的活，纯检查留在 hub。
   - 刷新与保留清扫（`retain::sweep`）合并进 `Job::Refresh`；在 hub 侧新增纯函数 `next_refresh(dirty, in_flight, synced)` 做脏计数合并并计数 `refresh.merged`，去掉 `take()` 中每条消息后的无条件 `refresh()`。
   - `Search` 改在 `work` 子树独立 `search` 任务（`tree.blocking("search", ...)`），带递增序号丢弃过期结果（`code = "stale"`、`search.stale += 1`）。
   - `Export` 在 `desk` 读记录并渲染，再于独立 `export.write` 任务（`tree.blocking("export.write", ...)`）写文件，不阻塞 `desk`。
   - `Share` / `Unshare` / 房间槽：读清单、分页、修改 `SHARE` 记录走 `desk`，网络请求与直播槽移至 `work` 子树上的 `net.share`、`net.unshare`、`slot` 任务（写回 `SHARE` 记录通过 `Job::PutShare` 排回 `desk`）。
   - 开始录音：`recording::run` 在 `Phase::Opening` 第一步经 `recording.blocking("recording.open", ...)` 写初始 `About`，失败则报 `shelf`、状态机走到 `Ended` 且不留 tape；`recording::seal` 整体跑在 `tree.blocking("recording.seal", ...)`；封盘后经 `Msg::Sealed` 立即更新列表并触发刷新，空录音删除改用 `tape::destroy` 并在失败时报 `tape.destroy`（处理 P1-05 报告留记）。
   - 欠写清点（`recover`/`resume`）移到 `desk`；整场转写（`job.write`、`job.sweep`）、交城（`job.file`）、搬运（`job.move`）全部迁入 `work.rs` 并挂在 `work` 子树，处理了 P1-05 报告里 `drop(tokio::spawn(task.finish()))` 的丢弃项。
3. **规则脚本与文档**：
   - `docs/EVENTS.md` 补录 `job.start`、`job.done`（列出全部 P1 `kind`，并写明两段式 `command.done` + `job.done` 与 `job` 字段名说明）。
   - `scripts/build/rules/spawn.sh` 将 `core/src/app/hub.rs` 欠账由 `11 P1-19/P1-20` 降至 `4 P1-20`（仅余登录、认领、造身份、日历 4 处）。

文件：`core/src/rt/supervise.rs`、`core/src/app.rs`、`core/src/app/core.rs`、`core/src/app/work.rs`、`core/src/app/hub.rs`、`core/src/app/recording.rs`、`docs/EVENTS.md`、`scripts/build/rules/spawn.sh`、`core/tests/bench/mod.rs`、`core/tests/liveness.rs`、`core/tests/app.rs`、`core/tests/retain.rs`、本报告。

## `hub.rs` 剩余 IO 与文件行数

- `grep -n "Shelf\|shelf\.\|tape::\|glossary::read\|glossary::put\|retain::\|room::pages\|city::begin" core/src/app/hub.rs`：空（0 处）。
- `hub.rs` 剩余 IO（全部归 P1-20 迁出）：
  - 启动：`settings.json` 读写（`Hub::run`、`Command::Setup`）、`audio::devices()`、`keys::kept()`、`keys::local()`、`carry()`（旧邀请文件与 `keys::create` 任务）；
  - 身份与钥匙库：`Command::SignIn`（`SignIn::begin` + 任务）、`Command::SignOut`（`keys::set`、`keys::local`）、`Command::CopyRotation`（`keys::rotation` + `config.clip`）、`Command::RotationSaved`（`keys::saved`）、`apply_signed` 中的邀请认领任务（`chain.claim`、`keys::carried`）；
  - 城日历：`Command::City` 任务（`city::calendar`）；
  - 更新：`Command::Update` 同步调 `(self.config.install)()`。
- 文件行数：
  - `core/src/app/hub.rs`：771 行
  - `core/src/app/work.rs`：1598 行
  - `core/src/app/recording.rs`：1536 行
  - `core/src/app/core.rs`：274 行

## 证据

### 1. 新增与取消忽略的测试（全部通过）

- `core/tests/liveness.rs`：
  - `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands`（取消 `ignore = "P1-19"`，断言堵着 FIFO 时有 `job.start(export.write)` 无同号 `job.done`、`Tree::running()` 含 `export.write`、`Pause`/`Resume`/`Change` 正常且 `command.done(ok)`，放开后收到同号 `job.done(export.write, ok)`）。
- `core/tests/app.rs`：
  - `refresh_coalescing_table_holds`（纯函数 `next_refresh` 表格测试）
  - `a_burst_of_changes_is_read_back_once_or_twice`（50 次改标题：`outlet.state` 序号涨 ≤ 3，`job.done(change)` 50 条序号递增，`job.done(refresh)` ≤ 3，`refresh.merged + refreshes >= 50`）
  - `an_export_writes_what_was_changed_just_before_it`
  - `a_search_asked_again_answers_only_the_last`（含 `job.done(search, stale)` 与 `search.stale >= 1`）
  - `a_desk_that_is_full_turns_commands_away_out_loud`（200 次改标题：`busy` 次数 + 落盘记录数 == 200，`queue.full(desk, reject)` 与统计核对）
  - `a_recording_whose_record_cannot_be_written_ends_saying_so`（`recording.end(shelf, error)`、`recording.phase` 从 `Opening` 到 `Ended`、无 tape 残留）
  - `a_job_that_panics_is_one_failed_job_and_the_desk_goes_on`（`job.done(panic, warn)` 后下一件活正常完成）
  - `no_job_says_what_it_was_about`（改标题、搜词、导出路径三类识别词均不入日志）
- 既有测试调整说明（均未放松行为断言）：
  - `the_core_runs_on_a_tree_with_a_name`：新增断言 `work/desk` 任务启动（初始任务 2 → 3）。
  - `core/tests/app.rs` 中 3 处异步导出读盘前等待 `job.done(export.write, ok)`（原依赖同步导出）。
  - `core/tests/retain.rs`：`audio_destroyed_at_the_persons_word_is_gone_for_good_and_its_text_stays` 重启后等待 `library` 含 `"draft"`（原 `len() == 3` 偶然依赖重启竞态先于空录音 `seal` 删除完成）。

### 2. `hub.turn_us` 对比（来自测试 `a_burst_of_changes_is_read_back_once_or_twice`）

- P1-13 报告（资料命令仍在 hub 上同步读写）：`Change` 单次 `11,800 µs`、`Search` `1,704 µs`，均触发 `hub.turn.slow`。
- P1-19 实测（50 次连发 `Change` + 全部消息共 102 圈）：`hub.turn_us` `p50 = 1 µs`、`p99 = 2 µs`、`most = 4 µs`，`hub.turn.slow = 0`。资料类命令已完全回到 1 ms 预算内。

### 3. 门禁与验证

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 警告）。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部通过。
- `ui`：`npm run --silent check`（0 errors, 0 warnings）、`npm test --silent`（5 文件 29/29 通过）。
- 非 TCP Rust 测试：`retain` 4/4、`app` 34/34、`liveness`（除 `i1_eight_hours` 与 wiremock `i2_background`）31/31 全过；含本地 TCP/wiremock 的测试待批次末尾由外部全量 `scripts/build/check.sh` 统一跑通。

## 留下的，要负责人的

- `hub.rs` 剩余的身份、钥匙库、设置、设备、更新、剪贴板、启动以及规则脚本 `hub.sh` 归 `P1-20`。
- 后台任务「一次一件」许可、`job.again` 及 `ai/write.rs`/`out/city.rs` 欠账归 `P1-21`。
