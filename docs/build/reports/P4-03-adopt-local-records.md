状态：完成

## 改动与文件

1. **本机 `did:key` 领回模块（`record` 层，严守 `layers.sh`）**：
   - `core/src/id/keys.rs`：新增 `local_kept()`（只读既有 `"identity.local"` 种子，不会在从未建过本机身份的设备上多生成种子）。
   - 新增 `core/src/record/adopt.rs`（导出于 `core/src/record.rs`）：
     - 定界协议类型 `LocalShelf { recordings: u32, adopted: bool }`（`ts-rs` 导出），以及 `Adopted`、`AdoptPlan`、`AdoptSummary`；
     - 纯决策函数 `adopt::plan(marker, local_did, person_did, has_local_data)` 覆盖首次领回、同人幂等补漏、他人拒绝（`OtherPerson`）与无本地数据跳过（`Nothing`）；
     - 原子标记读写 `read_marker` / `write_marker`（`<data>/adopted.json.tmp` + `sync_all` + `rename` 写入 `<data>/adopted.json`）；
     - `adopt::run` 与单条封盘补领 `adopt::run_one`：遍历所有现行 `take` 的 `talk/line`、`talk/speaker`、全部 `talk/note`（同时保留时间戳笔记、空文本锚点标记与旧单串 `<id>` 笔记），**最后**写入 `talk/recording` 保证崩溃原子性；经 `Shelf::kept::<serde_json::Value>(TERM)` 重签领回缺失的 `talk/term`（**完全不依赖 `crate::knowledge`**）；借助 `person.known(&id)` 保证幂等且绝不复活本人已删录音；绝不改动本机书架文件与 `tape/` 目录；
     - `adopt::status` 计算 `LocalShelf`；`adopt::discard_local` 删除 `<data>/records/<local_dir>` 与 `<data>/index/<local_dir>.sqlite3*`，完整保留 `adopted.json` 与 `tape/`。

2. **接入协议、枢纽、后台作业、进行中录音封盘补领与事件表**：
   - `core/src/view/query.rs`：新增 `ViewEngine::active_recording(&self)` 供 `run_sign` 排除正在录制的本机录音，防止未封盘被提前领回。
   - `core/src/app/proto.rs`：`State` 新增定界字段 `local_shelf: LocalShelf`；`Command` 新增 `DiscardLocal`（`needs_person() == true`）。
   - `core/src/app/hub.rs`：拒绝录音中调用 `DiscardLocal`（返 `discard.busy`）；处理 `Msg::LocalShelf` 与 `Msg::LocalDiscarded`（成功发 `local.discarded`）。
   - `core/src/app/work.rs`：
     - `run_sign` 在 `Who::Person` 且本机书架目录存在时，在 `Index::open` 与 `glossary::read` 之前执行 `adopt::run`，发出 `identity.adopt`（`recordings`、`terms`、`code`），并刷新 `local_shelf`；
     - `WriteCtx` 与 `SyncKind::RecordingSealed` 携带 `started_by`；若本机身份开始的录音在用户中途登录为 `Person` 后停止并封盘，先按原 `did:key` 签到底，随后在 `run_index_sync` 中自动调用 `adopt::run_one` 补领、写入本人 SQLite 索引、推送 `Invalidate` 并更新 `State.local_shelf`；
     - 新增 `Job::DiscardLocal` 清理本机备份并发 `identity.discard_local`；
     - **零新增 `Stats` 行**。
   - `docs/EVENTS.md`：新增 `identity.adopt`、`identity.discard_local`，并在 `job.start` 的 `kind` 列表补入 `discard_local`。
   - `core/bindings/`：生成 `LocalShelf.ts` 并更新 `State.ts`、`Command.ts`。
   - `ui/dev/mock.ts` 与全部 8 语 `ui/lang/*.json`：补齐 `local_shelf`、`DiscardLocal` 及 `code.adopt.failed`、`code.discard.busy`、`code.local.discarded`。

3. **测试**：
   - 新增 `core/tests/adopt.rs`（4 条测试：纯决策表、全要素领回与幂等/防复活/第二人拒绝、显式清理与录音中拒绝 + 隐私断言、进行中本机录音封盘补领）；
   - 更新 `core/tests/app.rs` 中两条 P1 过渡测试（`signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing` 与 `signing_out_goes_back_to_the_device_off_the_hub`）以及 `commands_that_need_a_person_are_listed_in_one_table`。

## 证据

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`：41/41 通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（含 `layers.sh`、`events.sh`、`dropped.sh`、`hub.sh`、`numbers.sh`、`spawn.sh`、`ui.sh`）：全部通过。
- 非 TCP Rust 测试全绿：
  - `core/tests/adopt.rs`（3/3 非 TCP 通过；`in_progress_local_recording_finishes_under_local_then_adopts_on_seal` 编译零警告，由沙箱外 `scripts/build/check.sh` 统一跑完）：
    - `adopt_plan_table`
    - `first_sign_in_adopts_all_takes_notes_speakers_terms_and_keeps_audio_and_local_intact`
    - `discard_local_clears_backup_without_touching_adopted_records_or_tape`
  - `core/tests/app.rs`（5/5）：
    - `commands_that_need_a_person_are_listed_in_one_table`
    - `signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing`
    - `signing_out_goes_back_to_the_device_off_the_hub`
    - `a_recording_is_signed_to_its_end_by_whoever_signed_its_start`
    - `what_needs_a_person_says_so_and_writes_nothing`
  - `core/tests/liveness.rs`（2/2）：
    - `every_event_in_the_catalogue_can_be_told_apart`
    - `i8_what_one_change_sends_does_not_grow_with_the_library`
- **沙箱外全量门禁 `RUSTC_WRAPPER="" scripts/build/check.sh`**：`check: all green`（`rustfmt` + `clippy -D warnings` + 全部 10/10 规则脚本 + Rust **731 passed, 0 failed, 6 ignored** + UI **12 文件 86 passed** + `vite build`）；顺带修正 `core/tests/identity.rs` 中对中继 `POST /.well-known/zu-relay/{room}` 返回码的断言（`204 NO_CONTENT`）。

## 遗留 / 需负责人确认

- 设置页领回横幅与两步 `InlineConfirm` 清除界面将在 P4-05 落地。
