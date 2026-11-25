状态：完成

## 改动与文件

1. **带时间与锚点的笔记（`core/src/record/notes.rs`、`core/src/record/shelf.rs`、`core/src/record.rs`）**
   - 新增 `NoteBody`（带 `#[serde(default)]`，兼容旧单串 `Words { text }`）与 `Note`（`ts-rs` 导出 `core/bindings/Note.ts`）。
   - 实现 `make_key`（`<id>.<at_ms:010>.<rand:08x>`）、`belongs_to`、`read`、`add`、`edit`、`remove`；空正文的时间笔记作为「标记」保留（`DECISIONS.md` 第 2 条），空旧单串 `<id>` 笔记不污染列表；越权或不存在的键返回稳定码 `"note.gone"`。
   - `Recording` 新增 `notes: Vec<Note>`，`Shelf::about` 同时填充旧 `note` 与排序后的 `notes`。
2. **多版本读取与精修状态机（`core/src/record/takes.rs`、`core/src/ai/write.rs`）**
   - 新增 `TakeInfo`、`RefineFailure`、`RefineStatus`（`ts-rs` 导出）、`parse_key`、`takes::list`、`takes::read`、`RefineEvent`、`refine_step` 与 `initial_refine_status`。
   - `ai::write::Journal` 增加 `#[serde(default)] pub failed: Option<RefineFailure>`；新增 `refine_error_code`（额度钥匙遇到异步转写 `key-refused` 映射为 `"keys.async_refused"`，`DECISIONS.md` 第 4 条）、`record_failure` 与 `clear_failure`。
3. **`O(1)` 现场草稿写手（`core/src/record/shelf.rs`）**
   - 新增 `DraftWriter` 与 `Shelf::open_draft`，打开 `lines.jsonl` 后仅保留 `last_at: i64` 保证时间戳单调递增，释放 `Held` 且不维护逐行 `heads` 映射；`shelf::drafts()` 切至 `open_draft`。
4. **命令与运行接线（`core/src/app/{proto,hub,work}.rs`、`docs/EVENTS.md`、`ui/dev/mock.ts`、`ui/lang/*.json`、`core/bindings/*.ts`）**
   - 新增 `NoteOp`（`Add` / `Edit` / `Remove`）与 `Command::{Note, Refine}`（均 `needs_person() == false`，`Command::Note` 允许录音中进行）。
   - `work.rs` 新增 `Job::Note`、`Job::RefinePrepare`、`Work::write_manual`；`WriteCtx::write_job` 支持手动重精修/搬运稿精修、致命失败落盘 `Journal.failed` 并发出 `refine.state` 结构化事件；`run_survey` 跳过已有致命失败的草稿，`Job::OwnKey` 填入自己的 Soniox 钥匙后自动清除 `keys.async_refused` / `keys.none` 失败记录并重新巡检。
   - `docs/EVENTS.md` 登记 `refine.state`；8 个 `ui/lang/*.json` 补齐 `code.note.gone`、`code.tape.none`、`code.keys.async_refused`；`ui/dev/mock.ts` 补齐 `Note` 与 `Refine` 处理器；重新导出 `core/bindings/*.ts`。
5. **测试**
   - 新增 `core/tests/notes.rs`、`core/tests/takes.rs`；更新 `core/tests/app.rs`、`core/tests/write.rs`、`core/tests/retain.rs`。

## 证据

- `cargo fmt --all -- --check` 通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 9 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`）全部通过。
- `core/tests/notes.rs`（3/3 通过）：
  - `timestamped_notes_add_edit_remove_and_bare_mark`
  - `legacy_single_string_note_reads_alongside_timestamped_notes`
  - `notes_work_during_live_recording_and_never_leave_to_city`
- `core/tests/takes.rs`（非 TCP 4/4 通过；含 `wiremock` 的 `manual_refine_triggers_on_draft_retries_after_failure_and_re_refines` 已编译通过 clippy，待沙箱外完整门禁执行）：
  - `takes_list_and_read_preserves_draft_alongside_refined_takes`
  - `refine_status_transition_table`
  - `draft_writer_keeps_o1_state_over_ten_thousand_lines`（10,000 行写回、`size_of::<DraftWriter>() < 256` 字节、`prev` 全空）
  - `manual_refine_rejects_missing_audio_key_and_live_recording`
- 回归：`core/tests/shelf.rs`（13/13）、`app::commands_that_need_a_person_are_listed_in_one_table`（1/1）、`liveness::every_event_in_the_catalogue_can_be_told_apart`（1/1）、`ui` `svelte-check`（0 errors）与 `npm test`（35/35，含 8 语种词表核对）全绿。

## 遗留 / 待负责人确认

- 临时决定 #2（「标记」落为空正文时间笔记）与 #4（额度钥匙异步转写被拒停留在现场稿并落码 `keys.async_refused`）待负责人确认。
- `takes::manual_refine_triggers_on_draft_retries_after_failure_and_re_refines` 使用本地 `wiremock` TCP 端口，将于本批结束后在沙箱外随完整 `scripts/build/check.sh` 一并执行。
