# P4-03 本机 `did:key` 记录领回与显式清理

## 目标

做完以后：

1. **领回模块（`core/src/record/adopt.rs`）**：
   - 首次以某个 `did:zu` 身份登录时，把本机 `did:key` 书架下的全部现行记录——`talk/recording`、全部历史 `take` 的 `talk/line`、`talk/speaker`、全部 `talk/note`（含时间戳笔记、锚点标记与旧单串笔记）以及 `talk/term` 词表——全部用本人 `did:zu` 签名钥匙重新签名并写入本人书架；
   - 音频 `tape/<id>/` 原封不动（`tape` 钥匙源自设备秘密 `"tape"`，与身份无关），领回后的录音立即可读、可精修、可销毁；
   - 原子写入 `<data>/adopted.json` 绑定 `local -> person_did`：同一位本人再次登录/重启幂等补漏且绝不复活本人已删录音（借 `person_shelf.known(&id)`），**第二位不同 `did:zu` 登录时绝不再次领回**；
   - 领回后保留本机 `did:key` 书架完整不动（便于退出登录查看或撤销，`PLAN.md` §2.7），直到用户显式发起 `Command::DiscardLocal`。
2. **进行中录音封盘后补领**：
   - 若用户在本机身份下正在录音时中途登录为本人 `did:zu`，该录音继续按开始时的本机身份签到底（守住 P1-06 不变量）；一旦停止并封盘，自动将该场录音领回本人书架、同步 SQLite 索引并推送 `Invalidate`。
3. **显式清理本机备份（`Command::DiscardLocal`）与小状态暴露**：
   - `State` 新增定界字段 `local_shelf: LocalShelf { recordings: u32, adopted: bool }`；
   - `Command::DiscardLocal`（`needs_person() == true`）删除 `<data>/records/<local_dir>` 与 `<data>/index/<local_dir>.sqlite3`（保留 `adopted.json` 与 `tape/`），若当前正在录制本机录音则拒绝并返 `discard.busy`；
   - 发出结构化事件 `identity.adopt` 与 `identity.discard_local`（零标题、零 DID）。

## 背景

- 设计：`docs/build/phases/P4.md` §3.4–§3.5；`docs/PLAN.md` §2.7；`docs/build/DECISIONS.md` #9–#11。
- 涉及文件：
  - `core/src/record.rs`、新增 `core/src/record/adopt.rs`
  - `core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`
  - `docs/EVENTS.md`
  - `core/bindings/*.ts`
  - `ui/dev/mock.ts`、`ui/lang/*.json`（8 语新增 `code.adopt.failed`、`code.discard.busy`、`code.local.discarded`）
  - 新增 `core/tests/adopt.rs`，并更新 `core/tests/app.rs` 的 2 条 P1 过渡测试与 `commands_that_need_a_person_are_listed_in_one_table`

## 做法

### 1. `core/src/record/adopt.rs`（严格留在 `record` 层，不引用 `crate::knowledge`！）

- 导出 `pub mod adopt;` 于 `core/src/record.rs`。
- 数据结构：
  ```rust
  #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
  #[ts(export)]
  pub struct LocalShelf {
      pub recordings: u32,
      pub adopted: bool,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct Adopted {
      pub local: String,
      pub to: String,
      pub at: i64,
      pub recordings: u32,
      pub terms: u32,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum AdoptPlan {
      Adopt,
      OtherPerson,
      Nothing,
  }

  #[derive(Debug, Clone, Default, PartialEq, Eq)]
  pub struct AdoptSummary {
      pub plan: AdoptPlan,
      pub recordings: u32,
      pub terms: u32,
      pub adopted_ids: Vec<String>,
  }
  ```
- 纯决策函数 `pub fn plan(marker: Option<&Adopted>, local_did: &str, person_did: &str, has_local_data: bool) -> AdoptPlan`：
  - 若 `marker` 存在且 `marker.local == local_did && marker.to != person_did` → `AdoptPlan::OtherPerson`；
  - 否则若 `!has_local_data` → `AdoptPlan::Nothing`；
  - 否则 → `AdoptPlan::Adopt`。
- 标记读写：`read_marker(data: &Path) -> Result<Option<Adopted>, &'static str>` 与 `write_marker(data: &Path, marker: &Adopted) -> Result<(), &'static str>`（写 `adopted.json.tmp` 后 `fs::rename` 到 `adopted.json`，原子落盘）。
- 领回主函数 `pub fn run(data: &Path, local: &Shelf, person: &Shelf, exclude_ids: &[String], now: i64) -> Result<AdoptSummary, &'static str>`：
  - 列出 `local` 现行录音（排除 `exclude_ids`）与现行词表 `local.kept::<serde_json::Value>(TERM)`；
  - 查 `marker` 并调用 `plan(...)`；若为 `OtherPerson` 或 `Nothing` 直接返回；
  - 对每条录音 `rec`：若 `person.known(&rec.id)?` 为真则跳过（幂等且不复活已删除录音）；否则：
    1. 用 `takes::list(local, &rec.id, rec.about.take)` 遍历**所有现行 take**，逐个 `takes::read(local, &rec.id, info.take)` 收集 `(key, Some(said))` 写入 `person.put(&rec.id, LINE, &all_lines, now)`；
    2. 写入全部 `talk/speaker`：`person.put(&rec.id, SPEAKER, &speakers, now)`；
    3. 写入全部 `talk/note`：对 `rec.notes` 中每条笔记保留原 `key` 与 `NoteBody { at: n.at_ms, text: n.text, anchor: n.anchor }`（若是旧单串键 `key == rec.id` 同样原键写入），调用 `person.put(&rec.id, NOTE, &note_pairs, now)`；
    4. **最后**写入 `talk/recording`：`person.put(&rec.id, RECORDING, &[(rec.id.clone(), Some(rec.about.clone()))], now)`，确保中途崩溃不会留下半成品；
    5. 记录 `adopted_ids.push(rec.id)`。
  - 词表领回：读取 `person.kept::<serde_json::Value>(TERM)` 已有键集合，仅将 `local` 上目标书架尚无的 `(key, Some(val))` 经 `person.put("", TERM, &missing_terms, now)` 重签写入（完全不依赖 `crate::knowledge`，严守 `layers.sh`）。
  - 写/更新 `adopted.json`（保留首次 `at`，累加 `recordings` 与 `terms`）。
  - **绝不修改或删除 `local` 书架任何文件，也绝不动 `tape/` 目录**。
- 辅助函数：
  - `pub fn status(data: &Path, local: Option<&Shelf>, person_did: Option<&str>) -> LocalShelf`：计算本机书架现存录音数及是否已被当前 `person_did` 领回；
  - `pub fn discard_local(data: &Path, local_did: &str) -> Result<(), &'static str>`：删除 `<data>/records/<local_dir>` 与 `<data>/index/<local_dir>.sqlite3`（若不存在视作成功），**保留 `adopted.json` 与 `tape/`**。

### 2. 接入 `proto.rs`、`hub.rs`、`work.rs`

- `proto.rs`：
  - `State` 增加 `pub local_shelf: LocalShelf`；
  - `Command` 新增 `DiscardLocal`（`kind() => "DiscardLocal"`，`needs_person() => true`）。
- `hub.rs`：
  - `Command::DiscardLocal`：若 `self.recording.is_some()` 返 `Err("discard.busy")`；否则交 `self.work.discard_local()`；
  - `SignedOut` 携带 `local_shelf: LocalShelf`；新增 `Msg::LocalShelf(LocalShelf)` 与 `Msg::LocalDiscarded(Result<LocalShelf, &'static str>)`，同步刷新 `self.state.local_shelf`，清理成功发 `self.said.say("local.discarded")`。
- `work.rs`：
  - 新增 `Job::DiscardLocal`；
  - 在 `run_sign` 中当 `who` 为 `Who::Person(person)` 时：
    - 尝试读出本机身份 `keys::local()`（只读既有种子，不因无本机种子报错）；若存在且本机书架目录存在，构建 `local_shelf` 调 `adopt::run(&config.data, &local_shelf, &person_shelf, busy, now())`；
    - 发出 `info!("identity.adopt", recordings = sum.recordings, terms = sum.terms, code = code)`（出错时 `warn!` 并压入 `say`）；
    - 在其后才执行 `Index::open`（自动把刚领回的录音重建/对账进本人 SQLite 索引）与 `env.views.invalidate_all()`，并更新 `env.terms`；
    - 计算 `local_shelf = adopt::status(...)` 返回给 `SignedOut`。
  - **进行中本机录音封盘后补领**：
    - `start_recording` 记录 `started_by: String = shelf.by().to_owned()` 并传入 `WriteCtx` / `IndexSync::recording_sealed(id, started_by)`；
    - 在 `run_index_sync` 处理 `SyncKind::RecordingSealed { id, started_by }` 时：若 `started_by` 是 `did:key:` 而当前 `identity.who` 已是 `Who::Person(person)`，则打开该本机书架对单条录音 `id` 调用 `adopt::run`（仅处理 `id`，不排除它），若领回成功发 `identity.adopt`、`idx.upsert_recording`、`views.invalidate(Library / Search / Recording / Transcript)` 并通过 `results.send(Msg::LocalShelf(...))` 更新 `State.local_shelf`！
  - `Job::DiscardLocal` 执行 `adopt::discard_local`、发 `identity.discard_local(code = "ok")` 并返回更新后的 `LocalShelf { recordings: 0, adopted: ... }`。
- 更新 `docs/EVENTS.md`：添加 `identity.adopt` 与 `identity.discard_local`，并在 `job.start` 的 `kind` 列表补入 `discard_local`。
- 更新 `core/tests/app.rs` 中两条 P1 过渡测试：
  - `signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing`：改为验证首次登录领回 `id` 到本人书架、**本机书架字节快照依然原封不动**、退出登录后仍回到本机书架看到 `id`；
  - `signing_out_goes_back_to_the_device_off_the_hub`：同样验证首次登录领回 `local_id` 且退出登录回到本机书架；
  - 更新 `commands_that_need_a_person_are_listed_in_one_table` 加入 `DiscardLocal => true`。
- 刷新 `core/bindings/*.ts`（生成 `LocalShelf.ts`、更新 `State.ts`/`Command.ts`），同步 `ui/dev/mock.ts` 与全部 8 语 `ui/lang/*.json` 新增 `code.adopt.failed`、`code.discard.busy`、`code.local.discarded`。

## 显式与可观测

- **纯决策函数**：`adopt::plan` 为纯函数，覆盖首次领回、同人幂等续领、他人拒绝、空书架跳过全部分支。
- **结构化事件**：`identity.adopt`（`recordings`、`terms`、`code`）与 `identity.discard_local`（`code`）登记在 `docs/EVENTS.md`，只含数字和稳定代码，零标题、零 DID；无新增 `Stats` 行。
- **层与丢弃规矩**：`adopt.rs` 不碰 `crate::knowledge`（过 `layers.sh`）；所有丢弃带 `// dropped:` 理由（过 `dropped.sh`）。

## 先写的测试

新建 `core/tests/adopt.rs`：
1. `adopt_plan_table`：穷举 `adopt::plan` 全部组合。
2. `first_sign_in_adopts_all_takes_notes_speakers_terms_and_keeps_audio_and_local_intact`：
   - 本机 `did:key` 书架写入一条含 take 0 + take 1、说话人名、时间戳笔记、锚点空标记、旧单串笔记、加密 `tape` 与 2 条 `talk/term` 的录音；
   - 执行 `adopt::run` 到 `Person A`：断言每条新记录的 `path.by == person_A` 且用 `Person A` 公钥验签通过；take 0 与 take 1 行数内容全在；全部笔记与词表全在；`tape` 可直接经 `tape::Pass` 解密读出；本机书架原目录字节零改动；
   - 再次对 `Person A` 执行 `adopt::run` 幂等返回 `0`；即使 `Person A` 删除该录音再登录也不会复活它；
   - 对 `Person B` 执行 `adopt::run` 返回 `AdoptPlan::OtherPerson`、`0` 条领回且 `Person B` 书架为空。
3. `in_progress_local_recording_finishes_under_local_then_adopts_on_seal`：
   - 未登录开始录音（记下本机 `did:key`）→ 录音进行中签入 `Person A` → 验证录音持续以本机身份写草稿 → `Command::Stop` 封盘后断言该场录音自动出现在 `Person A` 的 `Query::LibraryRecent` 与 `Query::Transcript` 中、音频仍在、`identity.adopt` 事件可见。
4. `discard_local_clears_backup_without_touching_adopted_records_or_tape`：
   - 领回后检查 `State.local_shelf == LocalShelf { recordings: 1, adopted: true }` → 发 `Command::DiscardLocal` → 断言本机书架与本机索引目录删除、`adopted.json` 与 `tape/` 仍在、`Person A` 录音与音频完好、`State.local_shelf.recordings == 0`、`identity.discard_local(code = "ok")` 可见；录音中发 `Command::DiscardLocal` 拒返 `discard.busy`。
5. 隐私断言：上述全流程日志中零出现 `did:key:`、`did:zu:`、录音标题或笔记文本，`zutalk_log::refused() == 0`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `layers.sh`、`events.sh`、`words.test.ts` 与全部 Rust/UI 测试）。

## 不做

- 不改动 0.8 迁移逻辑 `record/migrate.rs`（`moved.json` 照旧只给登录本人搬运）；
- 不在本任务画设置页领回横幅或身份纸 UI（留 P4-04、P4-05）。
