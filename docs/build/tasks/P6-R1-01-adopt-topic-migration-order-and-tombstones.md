# P6-R1-01 修复首次登录领回与旧词表迁移次序及自定义主题墓碑复活

## 目标

修复 P6 第 1 轮审查中的 **必须改 #1**：

1. **先迁移本人书架旧 `talk/term` 再执行 `adopt::run`**：在 `core/src/app/work.rs::run_sign` 中，将 `topic::ensure_migrated(person_shelf, now())` 移至 `adopt::run` 之前执行，防止本机 `"always"` 主题抢先落盘遮蔽老账号原有的 `terms.jsonl`。
2. **本机仅存旧 `talk/term` 时无损合入本人已有 `"always"` 主题（保持 `record` 层零 `crate::knowledge` 依赖）**：在 `core/src/record/adopt.rs::run` 中，若本机 `local_topics` 不含 `"always"`、但 `local_terms` 非空且本人书架已存在 `"always"` 主题，在纯 `serde_json::Value` 层将 `local_terms` 合并进本人 `"always"` 的 `"terms"` 数组；若本人书架尚无 `"always"`，则沿用 `adopt::run` 后的 `topic::ensure_migrated` 自动迁入，确保双向迁移均零丢失。
3. **防止重复登录复活已删除的自定义主题（守住 `LESSONS.md` P4 墓碑不变量）**：在 `core/src/record/shelf.rs` 新增 `Shelf::known_kept(&self, ty: &str) -> Result<BTreeSet<String>, Error>`，检查 pack 中全部出现过的路径键（含 `None` 墓碑）；`adopt::run` 对非 `"always"` 自定义主题跳过 `known_topics` 中已出现过的键，确保在本人书架删除过的自定义主题再次登录时永不复活。

## 背景

- 审查报告：`docs/build/reviews/P6-r1.md`（必须改 #1）。
- 涉及文件：
  - `core/src/record/shelf.rs`
  - `core/src/record/adopt.rs`
  - `core/src/app/work.rs`
  - `core/tests/adopt.rs`
- 分层红线：`core/src/record/adopt.rs` 严禁引用 `crate::knowledge`（`scripts/build/rules/layers.sh` 保持零欠账）。

## 做法

### 1. `Shelf::known_kept`（`core/src/record/shelf.rs`）

- 在 `Shelf` 上新增公开方法（紧邻 `Shelf::kept` / `Shelf::known`）：
  ```rust
  /// Every key ever written for person-level record type `ty`, removed since or not.
  pub fn known_kept(&self, ty: &str) -> Result<BTreeSet<String>, Error> {
      let held = held(self.pack("", ty))?;
      Ok(held
          .records
          .into_iter()
          .filter(|r| r.path.ty == ty)
          .map(|r| r.path.key)
          .collect())
  }
  ```
- 仅读 `self.pack("", ty)`，不存在时按既有 `held()` 语义返回空集，不新增依赖。

### 2. `adopt::run` 合并本机旧 `talk/term` 并尊重自定义主题墓碑（`core/src/record/adopt.rs`）

- 在 `adopt::run` 中读取本人主题历史键：
  ```rust
  let known_topics = person.known_kept(TOPIC).map_err(|_| "adopt.failed")?;
  ```
- 对 `local_topics` 的领回循环：
  - `k == ALWAYS_TOPIC_KEY`：沿用现有逻辑（本人无则写入，有则 `merge_always_topic_value`）；
  - `k != ALWAYS_TOPIC_KEY`：将判断条件从 `!existing_topics.contains_key(&k)` 收紧为 `!known_topics.contains(&k)`，使本人书架已删（留有 `None` 墓碑）的自定义主题永不再领回。
- 对「本机只有旧 `talk/term`、没有 `"always"` 主题，而本人已有 `"always"` 主题」的双向无损兜底：
  - 在 `local_terms` 消费之前，如果 `!local_has_always && !local_terms.is_empty()` 且 `let Some(person_always) = existing_topics.get(ALWAYS_TOPIC_KEY)`：
    - 构造临时 `serde_json::Value` 对象 `serde_json::json!({ "terms": local_terms.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>() })`（`talk/term` 记录正文结构本身即 `{ said, written }`，与 `Topic.terms` 元素一致，无需引入 `crate::knowledge`）；
    - 调用既有 `merge_always_topic_value(person_always, &legacy_as_always)`，若返回 `Some(merged)` 则加入 `adopted_topics` 写入 `person` 的 `"always"` 主题。

### 3. `run_sign` 调整迁移次序（`core/src/app/work.rs`）

- 在 `core/src/app/work.rs::run_sign` 中，当匹配 `(Who::Person(person), Some(person_shelf))` 准备调用 `adopt::run` 之前，先对 `person_shelf` 执行一次无损迁移：
  ```rust
  let _ = topic::ensure_migrated(person_shelf, now()); // dropped: best-effort legacy talk/term migration before adopt
  ```
- 保留第 2913 行（登录/切换身份收尾处）的 `topic::ensure_migrated(shelf, now())`，使「本人书架原本为空、仅领回了本机旧 `terms.jsonl`」以及本地身份启动两种路径继续自动迁移，且因 `ensure_migrated` 幂等，已迁过的书架零额外写入。

## 显式与可观测

- **显式不变量与分层边界**：`adopt.rs` 保持纯 `serde_json::Value` 合并与零 `crate::knowledge` 导入，`layers.sh` 零欠账；墓碑判定通过 `Shelf::known_kept` 显式表达，与录音的 `Shelf::known` 一致。
- **命名常量与队列/任务不变**：不新增队列、任务或 `Stats` 计数器，守住 `stats.full == 0`、`Tally ≤ 8192 B`、`bounded.sh`、`spawn.sh`、`numbers.sh`。
- **结构化事件不变**：复用既有 `topic.saved(op = "migrate", ...)` 与 `identity.adopt` 事件，`docs/EVENTS.md` 无需改动；故障与边界均由回归测试验证可见与恢复。

## 先写的测试

在 `core/tests/adopt.rs` 新增 3 条确定性回归测试：

1. `adopt_preserves_person_legacy_terms_when_local_has_always_topic`：
   - 本人书架预先只有旧 `talk/term`（`PersonLegacyTerm`，无 `topics.jsonl`）；本机书架有 `topics.jsonl` 的 `"always"` 主题（`LocalAlwaysTerm`）。
   - 启动/登录经完整 `run_sign`（或先 `ensure_migrated(person)` + `adopt::run`）验证：本人 `"always"` 主题同时包含 `PersonLegacyTerm` 与 `LocalAlwaysTerm`，零丢失，并记录 `topic.saved(op = "migrate")` 与 `identity.adopt`。
2. `adopt_merges_local_legacy_terms_into_person_existing_always_topic`：
   - 本人书架已有 `topics.jsonl` 的 `"always"` 主题（`PersonTopicTerm`）；本机书架只有旧 `talk/term`（`LocalLegacyTerm`，无 `topics.jsonl`）。
   - 执行 `adopt::run` 后，断言本人 `"always"` 主题同时包含 `PersonTopicTerm` 与 `LocalLegacyTerm`，且 `Query::Glossary` / `topic::list` 均可读出两条词。
3. `adopt_never_resurrects_custom_topic_removed_by_person`：
   - 本机书架建自定义主题 `T` → 首次登录 `adopt::run` 领回 `T` → 在本人书架调用 `topic::remove(&person, &t.id, ...)` 写墓碑 → 再次调用 `adopt::run`（模拟登出后重登、未清本机备份）。
   - 断言 `topic::get(&person, &t.id)` 仍为 `None`（绝不复活），而本机新建的另一自定义主题 `T2` 仍可正常补领。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `layers.sh` 零欠账与全量 Rust/UI 测试）。

## 不做

- 不修改前端文件（归 `P6-R1-02`）；
- 不改变 `adopted.json` 格式或新增外部依赖/事件。
