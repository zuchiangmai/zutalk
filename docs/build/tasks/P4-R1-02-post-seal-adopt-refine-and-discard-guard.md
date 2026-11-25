# P4-R1-02 修复封盘补领后的精修目标书架与 `DiscardLocal` 归属守卫

## 目标

修复 P4 第 1 轮审查中的 **必须改 #2、#5**：

1. **封盘补领后精修必须写到本人书架并使用本人最新钥匙/词表（必须改 #2）**：
   - 当一场以本机 `did:key` 开始的录音在录音中途登录为本人 `Person(A)` 并于停录封盘时被 `adopt::run_one` 领回 `Person(A)` 书架后，紧接着触发的整场精修 `write_job` 必须切换到 **`Person(A)` 的 `Arc<Shelf>`、当前 `Arc<Chain>` 与当前词表** 执行，使生成的 `take = 1` 精修稿直接落在 `Person(A)` 书架并同步进 `Person(A)` 的 SQLite 索引（同时保留本机 `did:key` 书架上原本的封盘草稿 `take = 0` 原封不动）；
   - 当一场以本机 `did:key` 开始的录音在封盘时发现当前已登录的是**第二位不同的人 `Person(B)`**（`adopt::run_one` 返回 `OtherPerson` 未领回），封盘后不把 `Person(B)` 的索引或书架搅混；
   - `SyncKind::Refined` 与 `SyncKind::RefineFailed` 携带 `shelf_by: String`，在 `run_index_sync` 中仅当当前打开的 `shelf.by() == shelf_by` 时才更新当前 SQLite 索引并推送对应失效，避免切换身份窗口内的跨书架误写。
2. **`Command::DiscardLocal` 必须校验领回归属（必须改 #5）**：
   - 当本机 `did:key` 书架仍有录音或词表数据时，`Job::DiscardLocal` 必须确认 `<data>/adopted.json` 已将当前 `local_did` 绑定到当前登录的 `person_did`；否则拒绝删除并返回稳定代码 `"adopt.failed"`，发出 `warn!("identity.discard_local", code = "adopt.failed")`，保护他人或尚未领回的本机录音不被误删；
   - 当本机书架目录已不存在或空无数据时（幂等清理），照常允许清理并返回 `LocalShelf { recordings: 0, adopted }`。

## 背景

- 审查报告：`docs/build/reviews/P4-r1.md`（必须改 #2、#5）。
- 涉及文件：
  - `core/src/record/adopt.rs`
  - `core/src/app/work.rs`
  - `core/tests/adopt.rs`

## 做法

### 1. `IndexSync::recording_sealed` 回传补领后的写目标（`core/src/app/work.rs`）

- 定义内部结构体：
  ```rust
  struct SealTarget {
      shelf: Arc<Shelf>,
      keys: Arc<Chain>,
      terms: Vec<Term>,
  }
  ```
- 将 `SyncKind::RecordingSealed` 的回执通道改为 `done: oneshot::Sender<Option<SealTarget>>`：
  - 在 `run_index_sync` 处理 `SyncKind::RecordingSealed { id, started_by }` 时：
    - 若 `started_by.starts_with("did:key:")` 且当前已是 `Who::Person(person_did)` 且 `adopt::run_one` 成功领回（`sum.recordings > 0`），返回 `Some(SealTarget { shelf: person_shelf.clone(), keys: snap.keys.clone(), terms: current_terms })`（并继续发送 `DeskOut::LocalShelf(...)` 更新小状态）；
    - 若 `started_by == shelf.by()`（普通未切换身份的录音），返回 `Some(SealTarget { shelf, keys: snap.keys, terms: current_terms })`；
    - 否则（例如录音以 `Person A` 开始后退出登录，或以本机开始但当前登录的是未获领回权的 `Person B`），返回 `None`。
- 在 `WriteCtx::after_seal_inner`（`core/src/app/work.rs:3009-3034`）：
  - `let sealed_target = self.index_sync.recording_sealed(self.id.clone(), self.started_by.clone()).await;`
  - 若 `Some(target)`，将 `self.shelf = target.shelf`、`self.keys = target.keys`、`self.terms = target.terms`、`self.started_by = self.shelf.by().to_owned()`，再基于最新的 `self.keys.from(Service::Soniox)` 判断 `write::owed(...)` 并启动 `write_job()`；
  - 若 `None`（当前活跃身份已不是该录音所属书架且未领回），仍按原有 `self.shelf` 仅在非跨人场景安全收尾或跳过当前身份视图的 `index_sync`；
  - 将 `SyncKind::Refined { id, shelf_by }` 与 `SyncKind::RefineFailed { id, shelf_by, failure }` 加上 `shelf_by: String`，在 `run_index_sync` 中加守卫 `if shelf.by() == shelf_by { ... }`。

### 2. `DiscardLocal` 归属守卫（`core/src/record/adopt.rs`、`core/src/app/work.rs`）

- 在 `core/src/record/adopt.rs` 新增纯决策函数：
  ```rust
  /// Whether `person_did` may discard `local_did`'s backup given `marker` and whether any local
  /// recordings/terms still exist on disk.
  pub fn can_discard(
      marker: Option<&Adopted>,
      local_did: &str,
      person_did: &str,
      has_local_data: bool,
  ) -> bool {
      if !has_local_data {
          return true;
      }
      marker.is_some_and(|m| m.local == local_did && m.to == person_did)
  }
  ```
- 在 `core/src/app/work.rs` 的 `Job::DiscardLocal` 中：
  - 当 `keys::local_kept()` 返回 `Some(local_me)` 时，打开只读本机 `Shelf` 判断 `has_local_data`（`!shelf.list()?.is_empty() || !shelf.kept::<serde_json::Value>(TERM)?.is_empty()`），读取 `adopt::read_marker(&config.data)` 并调用 `adopt::can_discard(...)`；
  - 若返回 `false`，发 `warn!("identity.discard_local", code = "adopt.failed")` 并返回 `Err("adopt.failed")`，不删任何文件；
  - 若返回 `true`，照旧执行 `adopt::discard_local` 并发 `info!("identity.discard_local", code = "ok")`。

## 显式与可观测

- **新增纯决策函数**：`adopt::can_discard` 独立于 IO，可穷举测试所有组合。
- **结构化事件**：`Job::DiscardLocal` 拒绝非归属清理时发出 `warn!("identity.discard_local", code = "adopt.failed")` 并向界面透传稳定代码 `adopt.failed`；补领后精修继续按既有 `identity.adopt` → `refine.state` → `index.sync` 事件链完整可观测。
- **零新增 `Stats` 槽位与零新增事件名**：守住 `stats.full == 0` 与 `docs/EVENTS.md` 一致性。
- **严格层边界**：`adopt.rs` 继续不引用 `crate::knowledge`（过 `layers.sh`）。

## 先写的测试

在 `core/tests/adopt.rs` 新增：
1. `can_discard_table`：穷举 `adopt::can_discard`（无本地数据、无 marker 有数据、marker 归本人、marker 归他人、marker 归另一 `local_did`）。
2. `in_progress_local_recording_adopted_on_seal_refines_into_person_shelf`：
   - 配置指向本机替身 Soniox（异步转写返回精修文本 `Refined adopted transcript`）+ 填入自有 Soniox 钥匙；
   - 未登录以本机 `did:key` 开始录音 → 录音中途 `CreateIdentity` 登录为 `Person A` → `Command::Stop` 封盘；
   - 断言录音先封盘在本机书架（`take = 0`，`path.by == local_did`）、随后自动领回 `Person A` 书架并完成整场精修：`Person A` 的 `Query::Recording` 最终达到 `take == 1`、`draft == false`、`Query::Transcript` 含 `Refined adopted transcript`，且本机书架上的原录音保持 `take == 0` 未被精修覆盖。
3. `second_person_cannot_discard_first_persons_local_backup`：
   - `Person A` 领回本机 1 条录音并保留本机备份 → `SignOut` → `Person B` 登录（`local_shelf == { recordings: 1, adopted: false }`）→ 发 `Command::DiscardLocal`；
   - 断言收到 `Notice("adopt.failed")`、日志中出现 `identity.discard_local(code = "adopt.failed")`、本机书架与索引目录均完整保留。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 10 个规则脚本、全部 Rust/UI 测试）。

## 不做

- 不改动 `AuthFlow`、`Command` 调试格式或 `rotation_unsaved`（归 `P4-R1-01`）；
- 不新增外部依赖或新 `Stats` 计数器。
