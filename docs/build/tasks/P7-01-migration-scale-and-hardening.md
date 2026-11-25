# P7-01 0.8 大规模迁移实测与边界加固

## 目标

1. 修复 `core/src/record/migrate.rs` 与 `core/src/app/{work,hub}.rs` 在审计中发现的 5 处迁移边界隐患：交接路径穿越防护、超大静音空洞 OOM 与空 `chunks` 防护、`moved.json` 原子写盘、切换身份/登出防串台以及 `State.unmoved` 定界（I8）。
2. 新增 1 条零隐私字段的结构化事件 `migrate.done`（`moved`、`already`、`skipped`、`code`、`ms`），同步写入 `docs/EVENTS.md` 且**零新增 `Stats` 行**。
3. 在 `core/tests/migrate_scale.rs` 建成 175 条真实多形态合成 0.8 资料库端到端压测，一次性跑通真实 `Core` 启动 + 0.8 迁移 + 本机 `did:key` 领回 + 旧 `talk/term` → `talk/topic` `"always"` 迁移 + SQLite v2 索引全查询 + 二次启动幂等 + 删后不复活 + I8/`Stats` 预算 + 真实落盘隐私字节审计。

## 背景

- 设计依据：`docs/build/phases/P7.md` §0、§1（E1–E2）、§2、§3.1–§3.2；`docs/PLAN.md` §2.4、§2.7、§4.5、§5.12、§7、§8（P7）；`docs/migrate.md`。
- 涉及文件：
  - `core/src/record/migrate.rs`（`Handed`、`run`、`shelve`、`reseal`、`old_key`）
  - `core/src/app/work.rs`（`Work::migrate`、`SyncKind::Migrated`、`IndexSync::migrated`）
  - `core/src/app/hub.rs`（`apply_signed`、`Msg::Moved`、`migrate`）
  - `docs/EVENTS.md`（新增 `migrate.done` 事件行）
  - `core/tests/migrate.rs`（补充边界单元测试）
  - `core/tests/migrate_scale.rs`（新建 175 条规模端到端压测与真实落盘隐私审计）

## 做法

### 1. `core/src/record/migrate.rs` 边界加固与 `MoveReport`

1. **命名常量（带单位与理由注释，严格通过 `numbers.sh`）**：
   - `pub const MIGRATE_MAX_AUDIO_MS: u64 = 12 * 60 * 60 * 1_000;` // 12-hour cap per migrated recording prevents corrupt `t_ms` OOM (`P7.md` §3.2)
   - `const MIGRATE_SILENCE_CHUNK_SAMPLES: usize = 160_000;` // 10 s at 16 kHz: stream silence gaps in bounded chunks instead of one giant `Vec`
   - `pub const STATE_UNMOVED_MAX_ROWS: usize = 32;` // bounds `State.unmoved` under I8 even when many handover entries fail (`P7.md` §3.2)
2. **纯校验与定界函数**：
   - `pub fn safe_recording_name(name: &str) -> bool`：拒绝空串、含 `/` 或 `\` 的名字，并确认 `Path::new(name).components()` 恰有 1 个 `Component::Normal(_)`（拒绝 `..`、`.` 与任何跨目录路径）。
   - `pub fn safe_chunk_path(path: &str) -> bool`：拒绝空串、含 `\` 的路径，并确认 `Path::new(path).components()` 非空且全部为 `Component::Normal(_)`（拒绝根路径、盘符前缀、`.`、`..`）。
   - `pub fn cap_unmoved(mut skipped: Vec<(String, String)>) -> Vec<(String, String)>`：`skipped.truncate(STATE_UNMOVED_MAX_ROWS); skipped`。
3. **`MoveReport` 与 `run_report`（保持 `run` 签名不变）**：
   - 定义：
     ```rust
     #[derive(Debug, Clone, Default, PartialEq, Eq)]
     pub struct MoveReport {
         pub moved: u32,
         pub already: u32,
         pub skipped: Vec<(String, String)>,
     }
     ```
   - 实现 `pub fn run_report((old, data): (&Path, &Path), shelf: &Shelf, me: &str, now: impl Fn() -> i64, mut progress: impl FnMut(Progress)) -> Result<MoveReport, &'static str>`：
     - `moved.json` 首次落盘改为原子写：写 `data.join("moved.json.tmp")`、`write_all` + `sync_all` + `fs::rename` 到 `moved.json`；
     - 遍历 `index.recordings` 前先校验 `safe_recording_name(name)`，不合法则记 `(String::new(), "migrate.handover".into())` 跳过；
     - 已在 `shelf.known(&id)?` 时递增 `already` 并跳过；
     - `shelve(...)` 成功递增 `moved`，失败清残余 `tape/<id>` 并记入 `skipped`。
   - 既有 `pub fn run(...) -> Result<Vec<(String, String)>, &'static str>` 改为调用 `run_report(...).map(|r| r.skipped)`，不破坏任何既有测试或调用方。
4. **`reseal` 静音分批与空 `chunks` 拒绝**：
   - 若 `audio.chunks.is_empty()`，直接返回 `Err("migrate.audio")`；
   - 循环处理每个 `chunk` 前：校验 `safe_chunk_path(&chunk.path)` 与 `(chunk.t_ms as u64) <= MIGRATE_MAX_AUDIO_MS`，不满足返 `Err("migrate.audio")`；
   - 块间静音补齐改为分批循环：计算 `mut gap = from.saturating_sub(at);`，每轮向 `pcm` 填充 `gap.min(MIGRATE_SILENCE_CHUNK_SAMPLES)` 个 `0i16` 并调用 `seal(&mut pcm, &mut at)?`，直到 `gap == 0`（内存恒为 `O(1)`）。

### 2. `core/src/app/{work,hub}.rs` 身份隔离、`State.unmoved` 封顶与 `migrate.done`

1. **`core/src/app/work.rs`**：
   - `SyncKind::Migrated { by: String }` 与 `IndexSync::migrated(&self, by: String)`：在 `desk` 执行 `SyncKind::Migrated { by }` 时先核对当前活跃 `shelf.by() == by`，不匹配则静默跳过（不污染已切换身份的 SQLite 索引）；匹配时运行 `idx.reconcile(&shelf, &tapes)` 并依次失效 `ViewKind::Library`、`ViewKind::Search`、`ViewKind::Recording`、`ViewKind::Transcript`。
   - `Work::migrate(&self, me: String)` 改调 `migrate::run_report`，完成后发出结构化事件 `migrate.done`（字段：`moved`、`already`、`skipped`、`code`、`ms`；仅当 `code == "ok"` 且 `skipped == 0` 用 `info!`，否则 `warn!`），并在 `Ok` 时等待 `index_sync.migrated(me.clone()).await` 后发送 `Msg::Moved(me, skipped)`。
   - **绝不新增任何 `Stats` 计数、高水位、分布或队列行**。
2. **`core/src/app/hub.rs`**：
   - `Msg::Moved(String, Result<Vec<(String, String)>, &'static str>)`：
     - 清除 `self.moving = false`；
     - 仅当 `self.state.me.as_deref() == Some(by.as_str())` 时写 `self.state.unmoved = migrate::cap_unmoved(skipped?)`；若期间已退出或换人，则显式丢弃（带 `// dropped: stale migration result from previous identity` 注释）。
   - `apply_signed` 开头立即执行 `self.state.unmoved.clear();`，防止登出或切换身份后残留上一位用户的失败列表。
3. **`docs/EVENTS.md`**：
   - 在 `## 事件` → `### 身份与核心（hub / work）` 表中登记 `migrate.done` 一行，字段写清 `moved`、`already`、`skipped`、`code`、`ms` 及级别口径，确保 `scripts/build/rules/events.sh` 与 `every_event_in_the_catalogue_can_be_told_apart` 全绿。

### 3. 175 条真实形态合成 0.8 资料库端到端压测（`core/tests/migrate_scale.rs`）

1. 在测试内构建完整的合成 0.8 目录（全部文本均为虚构无真实人名/真实内容，音频密文严格按 0.8 格式 `u32_le_len || nonce_12 || aes256gcm_ct` 与 `Secrets/content-keys.json` 生成，复用 `aes_gcm` 与 `core/src` 公开 API，**零新增 dev-dependency**）：
   - **175 条有效 0.8 录音**，时间横跨 6 个自然月，涵盖：
     - 多语现场草稿（`takes.len() == 1`，中/英/泰/日/韩混合，含 `via.text == "hand"` 手改行与 `incomplete == true` 残句）；
     - 多语双版已精修录音（`takes.len() == 2`，含 `take 0` 现场稿与 `take 1` 精修稿，可验证 `Query::Transcript` 两版切换与 `Query::Compare`）；
     - 多说话人映射（`speakers: {"1": ..., "2": ...}`）；
     - 录音笔记与拼接标记文本（`note` 含虚构秘词与 `[00:12] ...` 行）；
     - 有音频录音（覆盖 `s16` 16 kHz 单声道分两块带 0.5 s 块间静音、以及 `f32` 48 kHz 双声道导入文件，占比约 20 条以控制测试耗时在数秒内，其余 155 条为无音频 `audio: null`）；
   - **6 条各类损坏/异常条目**并列在 `index.json` 中（覆盖：缺录音 JSON 文件、版本 `2` 不匹配、缺音频块文件、缺 `content-keys.json` 钥匙、路径穿越录音名 `../escape`、音频块 `t_ms` 超出 `MIGRATE_MAX_AUDIO_MS`）。
2. 同时在新的 `data` 目录预置升级前遗留状态，验证多路径协同：
   - 一份旧版 `settings.json`（只含早期字段，缺少 P5 `caption`/`city` 与 P6 `topics`）；
   - 本机 `did:key` 书架下的 2 条本机录音 + 2 条旧扁平 `talk/term` 词表（模拟用户在登录前先离线录了两场并存了词表，然后带着已登录 `did:zu` 启动，一次性触发 `adopt` 领回 + `talk/term` → `talk/topic` `"always"` 迁移 + 0.8 175 条搬家！）。
3. **验证全部核心断言**（在 `#[tokio::test(flavor = "multi_thread")]` 下启用真实落盘日志 `zutalk_log::files`，仅在同步 `Core::start`/`core.send` 时短时 `scope.enter()`，不跨 `.await` 持守卫）：
   - **无损搬入与全查询立即生效**：
     - `Query::LibrarySummary` 总数等于 `175 + 2 = 177`，月份分布覆盖全部 6 个月；
     - `Query::LibraryMonth` 分页总数与 `LibrarySummary` 逐月一致；
     - `Query::Search` 对中文 2 字短词、≥3 字词、泰文词、笔记内词均立即命中正确录音；
     - `Query::Recording` 正确返回说话人、笔记与版本列表（双版录音含 `take 0` 与 `take 1`）；
     - `Query::Transcript` 对抽样录音的 `take 0` 与 `take 1` 完整返回原文、译文与 `via["text"] == "hand"`；
     - `Query::Topics` 与 `Query::Glossary` 确认本机旧 `talk/term` 已无损迁入固定 `"always"` 主题；
     - 抽样核对有音频录音的 `tape::kept` 为真、无音频录音为假；搬入草稿 `About.moved == true` 未触发自动精修（`Progress.refine == None`）。
   - **损坏条目隔离与可观测**：
     - 应用正常就绪零崩溃，`State.unmoved.len() == 6` 且每条均带预期错误码（`migrate.handover` / `migrate.audio` / `migrate.key`）；
     - 落盘日志含 `migrate.done`，且 `moved == 175`、`already == 0`、`skipped == 6`、`code == "ok"`。
   - **二次启动幂等与删后不复活**：
     - 关闭 `Core` 后用同一 `data` + `old` 重启，断言 `LibrarySummary.total == 177`，落盘日志第二次 `migrate.done` 显示 `moved == 0`、`already == 175`、`skipped == 6`；
     - 发 `Command::Remove` 删除 1 条已迁移录音后再重启第三次，断言 `LibrarySummary.total == 176`，被删录音绝不复活。
   - **I8、`Stats` 与真实落盘隐私字节审计**：
     - 发 `Command::Diagnose` 生成自检包；
     - 逐字节扫描 `logs/*.jsonl`、`diagnose/**/*` 与 `settings.json`，断言全部秘词（标题秘词、多语正文与译文秘词、笔记秘词、说话人名秘词、词表秘词、音频密钥 hex、DID）零出现；
     - 断言日志中零 `stats.full` 事件、`Tally` 紧凑 JSON `≤ 8192 B`、`zutalk_log::refused()` 零增量。
4. 若编写或运行该 175 条压测过程中发现任何额外迁移或索引边界 bug，在本任务内一并根治并补齐回归测试。

## 显式与可观测

- **显式纯函数与常量**：
  - `safe_recording_name`、`safe_chunk_path`、`cap_unmoved` 为零 IO 纯函数，配全表单元测试；
  - `MIGRATE_MAX_AUDIO_MS`、`MIGRATE_SILENCE_CHUNK_SAMPLES`、`STATE_UNMOVED_MAX_ROWS` 均带单位与理由注释，过 `numbers.sh`；
  - 严守 `layers.sh` 单向分层（`record/migrate.rs` 不碰 `knowledge`/`view`/`app`，`hub.rs` 零 IO 过 `hub.sh`）。
- **队列与任务**：
  - 不新增任何队列或未监督任务；继续沿用 `work` 监督树下的 `job.move` 与 `BACKGROUND_AT_ONCE_JOBS = 1` 串行许可；
  - 不新增任何 `Stats` 行（维持 `stats.full == 0`）。
- **结构化事件**：
  - 新增 `migrate.done`（`moved`、`already`、`skipped`、`code`、`ms`，零内容/标题/id/DID），双向同步进 `docs/EVENTS.md` 并过 `events.sh`。
- **故障可见性测试**：
  - 6 类损坏交接、路径穿越、超长 `t_ms`、空 `chunks` 与中途切身份均同时断言恢复行为（不崩、不串台、不 OOM）与可见信号（`State.unmoved`、`migrate.done` 精确计数）。

## 先写的测试

1. `core/tests/migrate.rs` 新增：
   - `safe_paths_and_unmoved_cap_rules`（表驱动测试 `safe_recording_name`、`safe_chunk_path`、`cap_unmoved`）；
   - `corrupt_audio_edge_cases_are_rejected_without_oom`（空 `chunks`、路径穿越 `../secret`、`t_ms > MIGRATE_MAX_AUDIO_MS` 均返回 `migrate.audio` 且不留残余 tape）；
   - `run_report_counts_moved_already_and_skipped_accurately`（核对首次搬入与二次重跑的 `moved`/`already`/`skipped` 计数与 `moved.json` 完整性）。
2. `core/tests/migrate_scale.rs` 新增：
   - `scale_175_recordings_migration_with_adoption_topics_queries_idempotency_and_privacy`（完整 175 条 + 6 损坏 + 本机领回 + 旧 `talk/term`→`always` + 旧 `settings.json` + 全查询 + 三次启动幂等/防复活 + 真实落盘隐私与 `Tally` 审计）；
   - `sign_out_clears_unmoved_and_drops_stale_migration_result`（验证切身份/登出清空 `State.unmoved` 且不串台）。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 一次全绿；
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test migrate --test migrate_scale` 全部通过且单次运行稳定在 15 秒以内；
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test liveness i8_what_one_change_sends_does_not_grow_with_the_library` 输出 `ratio=1.00`；
- `scripts/build/rules/events.sh`、`numbers.sh`、`layers.sh`、`hub.sh` 全绿；`Cargo.lock` 零变更。

## 不做

- 不修改 `/Users/kant/Zuzalu/zulangue` 或 `docs/migrate-0.8.patch`；
- 不新增任何外部 crate 或 `Stats` 计数/分布行；
- 不改变 `State.unmoved` 的公开 JSON 类型（仍为 `Vec<(String, String)>`），不新增 UI 词典键；
- 不修改任何 `P6*` 评审或报告文件。
