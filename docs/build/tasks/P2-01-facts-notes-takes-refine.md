# P2-01 事实层扩展：带时间笔记、多版本读取、精修状态机与 O(1) 草稿写手

## 目标

做完以后：

1. **带时间与锚点的笔记**（`core/src/record/notes.rs`）：支持 `talk/note` 键 `<id>.<t_ms:010>.<rand8>` 与正文 `{ at, text, anchor? }`，支持空正文作为「标记」（`DECISIONS.md` 第 2 条）；完全向后兼容磁盘上旧的单串 `talk/note`（键 `<id>`，正文 `Words { text }`）。
2. **`Command::Note { id, op }`**（`Add` / `Edit` / `Remove`）：录音中和录完后都能用，不需登录；坏键或不存在回 `"note.gone"`。
3. **多版本读取**（`core/src/record/takes.rs`）：能列出一条录音已有的全部版本（`TakeInfo`：take 0、take 1、…，行数、是否现场稿、是否当前版、写入时间）并按 `take` 读出任意旧版的全部行。
4. **精修状态机与手动精修**（`core/src/record/takes.rs`、`core/src/ai/write.rs`、`core/src/app/{proto,hub,work}.rs`）：
   - `RefineStatus`（`Draft` / `Refining` / `Refined` / `Failed`）与纯转换函数 `refine_step`；
   - `ai::write::Journal` 持久化 `failed: Option<RefineFailure>`（含 `DECISIONS.md` 第 4 条：额度钥匙被异步转写拒收时落为 `"keys.async_refused"`）；
   - `Command::Refine { id }` 允许手动触发精修、重试失败的精修，或对已有精修稿/搬运稿再次精修（产出 `take + 1`）；启动巡检跳过已致命失败的草稿直到用户换钥匙或手动重试。
5. **现场草稿 `O(1)` 写手**（I3，`core/src/record/shelf.rs`）：新增 `DraftWriter`（`Shelf::open_draft`），写现场草稿不再把每行塞进 `Writer.heads`，连写 10,000 行内存占用恒定。

## 背景

- 设计：`docs/build/phases/P2.md` §3.3；`docs/PLAN.md` §2.1、§3（`LiveNotes.dc.html`、`Refine.dc.html`）；`docs/build/DECISIONS.md` 第 2、4 条。
- 涉及文件：
  - `core/src/record.rs`、`core/src/record/shelf.rs`
  - 新增 `core/src/record/notes.rs`、`core/src/record/takes.rs`
  - `core/src/ai/write.rs`
  - `core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`
  - `ui/dev/mock.ts`（为新增 `Command::Note` / `Command::Refine` 补齐 `handlers` 映射，满足 TypeScript 穷举检查）
  - `ui/lang/*.json`（8 种语言补齐 `code.note.gone`、`code.tape.none`、`code.keys.async_refused`）
  - `docs/EVENTS.md`（登记 `refine.state`）

## 做法

### 1. O(1) 现场草稿写手（`core/src/record/shelf.rs`）

- 新增 `pub struct DraftWriter { pack: Pack, by: String, key: Arc<SecretKey>, last_at: i64 }`：
  - `Shelf::open_draft(&self, id: &str) -> Result<DraftWriter, Error>`：创建目录、调 `Pack::open(self.pack(id, LINE))`，取 `held.records` 中最大 `at + 1` 初始化 `last_at`（空文件为 `i64::MIN`），随后立刻释放 `held`（不建 `heads: BTreeMap`）。
  - 为 `DraftWriter` 实现 `Draft` trait：`put(&mut self, key, said, now)` 直接用 `prev = Vec::new()` 与 `at = now.max(self.last_at)` 签名并追加，更新 `self.last_at = at + 1`；`sync` 转发给 `self.pack.sync()`。
  - `shelf::drafts()` 改为调用 `shelf.open_draft(id)`。
  - 既有 `Writer` 与 `Shelf::open` / `Shelf::put` 行为完全不变（仍用 `heads` 追踪覆盖同一路径的 `prev`）。

### 2. 带时间与锚点的笔记（`core/src/record/notes.rs` + `shelf.rs`）

- 在 `core/src/record/notes.rs` 定义：
  - `NoteBody { at: u32, text: String, #[serde(skip_serializing_if = "Option::is_none")] anchor: Option<String> }`（带 `#[serde(default)]`，兼容旧 `Words { text }`）；
  - `Note { key: String, at_ms: u32, text: String, anchor: Option<String> }`（`#[derive(Serialize, Deserialize, TS)] #[ts(export)]`）；
  - 纯函数 `pub fn make_key(id: &str, at_ms: u32, rand: u32) -> String` = `format!("{id}.{at_ms:010}.{rand:08x}")`；
  - 纯函数 `pub fn belongs_to(id: &str, key: &str) -> bool`：`key == id || key.starts_with(&format!("{id}."))`；
  - `pub fn read(shelf: &Shelf, id: &str) -> Result<Vec<Note>, Error>`：从 `about.jsonl` 读出该录音所有现行 `talk/note`（跳过已墓碑删除的；旧键 `<id>` 且 `text.is_empty()` 不返回，避免空旧串污染列表；新键 `<id>.<t_ms>.<rand>` 即便 `text == ""` 也保留，作为「标记」），按 `(at_ms, key)` 排序返回；
  - `pub fn add(shelf: &Shelf, id: &str, at_ms: u32, text: String, anchor: Option<String>, rand: u32, now: i64) -> Result<Note, Error>`；
  - `pub fn edit(shelf: &Shelf, id: &str, key: &str, text: String, now: i64) -> Result<Note, &'static str>`（先校验 `belongs_to` 与存在性，不在回 `"note.gone"`，保留原 `at` 和 `anchor`）；
  - `pub fn remove(shelf: &Shelf, id: &str, key: &str, now: i64) -> Result<(), &'static str>`（先校验 `belongs_to` 与存在性，不在回 `"note.gone"`，写 `None` 墓碑）。
- 在 `shelf.rs`：
  - 给 `Recording` 增加 `pub notes: Vec<Note>`（保留 `pub note: String` 供旧测试与兼容；`Shelf::about` 同时填充旧键 `<id>` 的 `note` 与全部 `notes`，且排序）。
  - 注意：`out/city.rs` 只取 `RECORDING`、`LINE`、`SPEAKER`，绝不读取或发送 `NOTE`。

### 3. 多版本与精修状态机（`core/src/record/takes.rs`）

- 定义 `TakeInfo`、`RefineFailure`、`RefineStatus`（照 `P2.md` §3.3.2，全部 `#[ts(export)]`）。
- 纯函数 `pub fn parse_key(id: &str, key: &str) -> Option<(u32, u32)>`：从 `<id>.<take>.<n>` 解析 `(take, n)`。
- `pub fn list(shelf: &Shelf, id: &str, current_take: u32) -> Result<Vec<TakeInfo>, Error>`：一次读 `lines.jsonl`，统计每个仍有现行行的 `take` 的行数与最新 `at`，按 `take` 升序返回。
- `pub fn read(shelf: &Shelf, id: &str, take: u32) -> Result<Vec<(String, Said)>, Error>`：读 `lines.jsonl` 中指定 `take` 的全部现行行（按字典序即说话顺序）。
- 精修状态机（显式枚举 + 纯转换函数）：
  ```rust
  pub enum RefineEvent<'a> {
      Start,
      Progress { step: &'a str, done: u32, of: u32 },
      Succeeded { take: u32 },
      Failed { code: &'a str },
      Cleared { why: Option<&'a str> },
  }
  pub fn refine_step(state: &RefineStatus, event: RefineEvent<'_>, now: i64) -> RefineStatus
  ```
  并为 `RefineStatus` 提供 `pub fn code(&self) -> &'static str`（`"Draft"` / `"Refining"` / `"Refined"` / `"Failed"`），供 `refine.state` 日志事件使用。
- 辅助纯函数 `pub fn initial_refine_status(take: u32, audio: bool, has_key: bool, failed: Option<&RefineFailure>) -> RefineStatus`：
  - `take > 0` → `Refined { take }`
  - 否则若有 `failed` → `Failed { code, at }`
  - 否则 `!audio` → `Draft { why: Some("tape.none") }`
  - 否则 `!has_key` → `Draft { why: Some("keys.none") }`
  - 否则 → `Draft { why: None }`。

### 4. `Journal.failed` 与 `DECISIONS.md` 第 4 条（`core/src/ai/write.rs`）

- `Journal` 新增 `#[serde(default)] pub failed: Option<RefineFailure>`（来自 `crate::record::takes::RefineFailure`）。
- 保持 `write::status(401 | 403) == Err(End::Fatal("key-refused"))` 与既有 `core/tests/write.rs` 全部测试不变。
- 新增纯函数 `pub fn refine_error_code(end_code: &'static str, soniox_from: crate::id::keys::From) -> &'static str`：当 `end_code == "key-refused"` 且 `soniox_from == From::Granted` 时返回 `"keys.async_refused"`，否则原样返回 `end_code`。
- 新增 `pub fn record_failure(dir: &Path, code: &str, at: i64) -> Result<(), End>` 与 `pub fn clear_failure(dir: &Path) -> Result<(), End>`（仅当 `dir.is_dir()` 时更新 `write.json`）。
- `write::write` 成功写完新版本后删除 `write.json`（已有行为），自动清除旧失败。

### 5. 命令与接线（`core/src/app/{proto,hub,work}.rs` + `ui/`）

- `proto.rs`：
  - 新增 `NoteOp`（`Add` / `Edit` / `Remove`）；
  - `Command` 新增 `Note { id: String, op: NoteOp }` 与 `Refine { id: String }`；更新 `Command::kind`（`"Note"` / `"Refine"`）和 `Command::needs_person`（两者均为 `false`）。
- `hub.rs`：
  - 处理 `Command::Note { id, op }` → `self.work.note(id, op)?`（录音中允许！）；
  - 处理 `Command::Refine { id }`：若 `self.under_way(&id)` 回 `"recording.under_way"`；若 `self.busy(&id)` 回 `"recording.writing"`；否则 `self.work.refine(id)?`；收到 `Msg::RefineReady(id)` 时像自动精修一样把 `writing(&id, "closing")` 记入状态并启动 `write_job`（带 `manual = true`）。
- `work.rs`：
  - `Job::Note { id, op }` 在 `desk` 上调用 `notes::{add, edit, remove}`（先确认 `shelf.about(&id)?.is_some()` 否则 `"recording.gone"`）；
  - `Job::RefinePrepare { id }` 在 `desk` 上校验：录音存在（`"recording.gone"`）、有音频 `tape::kept`（`"tape.none"`）、有 Soniox 钥匙（`"keys.none"`）；清掉 `Journal.failed`；返回 `DeskOut::RefineStart { id, prev_take }`；
  - `WriteCtx` 增加 `manual: bool` 与 `from_state: RefineStatus`：
    - `manual == true` 时，`write_job` 不要求 `read.about.take == 0`（允许重精修产出 `take + 1`，也允许搬运稿 `moved == true` 手动精修）；
    - 进入 `write_job` 时发 `refine.state`（`from` → `"Refining"`）；
    - 结束为 `End::Done` 时发 `refine.state`（`"Refining"` → `"Refined"`, `take = new_take`）；
    - 结束为 `End::Fatal(c)` 时调用 `refine_error_code(c, self.keys.from(Service::Soniox))`，落盘 `write::record_failure(&self.tape(), mapped, now())`，发 `refine.state`（`warn!`, `"Refining"` → `"Failed"`, `why = mapped`），并 `self.said.say(mapped)`；
  - `run_survey` 跳过 `Journal::load(&tapes.join(&r.id))` 里 `failed.is_some()` 的草稿；在 `Job::OwnKey` 新增自己的 Soniox 钥匙时，对 `failed.code == "keys.async_refused"` 或 `"keys.none"` 的草稿清除 `failed` 后再巡检，让「填了自己的钥匙后自动可精修」成立。
- `ui/dev/mock.ts`：给 `handlers` 补上 `Note: () => {}`、`Refine: () => {}`。
- `ui/lang/*.json`（全部 8 个文件）：补齐 `code.note.gone`、`code.tape.none`、`code.keys.async_refused`。

## 显式与可观测

- **状态机**：`RefineStatus`（`Draft` / `Refining` / `Refined` / `Failed`）+ 纯函数 `refine_step`（配转换表测试）。
- **事件**：新增 `refine.state`（`info`；到 `Failed` 时 `warn`），字段 `rec = Id(&id)`、`from`、`to`、`why`、`take`。同步登记进 `docs/EVENTS.md`；绝不记录笔记文本或正文。
- **数与队列**：不新增队列，不新增魔法数字。
- **任务**：手动精修复用现有受监督的 `work` 子树任务 `job.write` 与 `BACKGROUND_AT_ONCE_JOBS = 1` 信号量。
- **故障可见性**：精修致命失败测试必须同时断言 `Journal.failed` 持久化结果、错误码与日志里的 `refine.state(to = "Failed", why = ...)`。

## 先写的测试

新建 `core/tests/notes.rs` 与 `core/tests/takes.rs`：

1. `notes.rs`：
   - `timestamped_notes_add_edit_remove_and_bare_mark`：加带时间与锚点笔记、加空正文标记、编辑保留原 `at` 和 `anchor`、删除写墓碑、坏键与跨录音键回 `"note.gone"`；按 `(at_ms, key)` 排序。
   - `legacy_single_string_note_reads_alongside_timestamped_notes`：先用旧键 `<id>` + `Words` 写旧笔记，再加新时间笔记，`notes::read` 与 `Shelf::about` 同时读出且不丢旧数据。
   - `notes_work_during_live_recording_and_never_leave_to_city`：录音进行中发 `Command::Note(Add)` 成功落盘，停录后仍在；交城负载中不含任何笔记。
2. `takes.rs`：
   - `takes_list_and_read_preserves_draft_alongside_refined_takes`：写 take 0、take 1、take 2，`takes::list` 列出 3 版并标明 `is_live` / `is_current` / 行数，`takes::read(0)` 完整读回现场稿。
   - `refine_status_transition_table`：穷举 `refine_step` 全部状态与事件转换，含 `initial_refine_status` 表格。
   - `draft_writer_keeps_o1_state_over_ten_thousand_lines`：用 `Shelf::open_draft` 写 10,000 行，断言 `lines.jsonl` 读回 10,000 行、`prev` 全空、且 `DraftWriter` 不持有随行数增长的 `heads` 结构。
   - `manual_refine_triggers_on_draft_retries_after_failure_and_re_refines`（用本地 `wiremock`）：
     - 先模拟额度钥匙被 401 拒绝 → `Journal.failed` 持久化 `"keys.async_refused"`、日志出现 `refine.state(to = "Failed", why = "keys.async_refused")`；重启核心不再盲目重撞；
     - 填入自己钥匙或发 `Command::Refine { id }` → 成功写出 take 1，日志出现 `refine.state(to = "Refined", take = 1)`，`write.json` 清除；
     - 对已有 take 1 再发 `Command::Refine { id }` → 写出 take 2，take 0 / 1 / 2 全可读；
     - 无音频发 `Command::Refine` → 回 `"tape.none"`；无钥匙 → 回 `"keys.none"`；录音中 → 回 `"recording.under_way"`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 9 个规则脚本、`cargo test --workspace`、`ui` check & test）。
- `core/tests/notes.rs` 与 `core/tests/takes.rs` 全绿。
- `docs/EVENTS.md` 已登记 `refine.state` 且 `events.sh` 通过。

## 不做

- 不引入 `rusqlite`（P2-03）。
- 不删 `State` 里的旧字段，不动 `layers.sh`（P2-02、P2-06）。
- 不改 `out/city.rs` 和 `out/room.rs`。
