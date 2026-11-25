状态：完成

## 改了什么

1. **0.8 迁移边界加固与结构化计数（`core/src/record/migrate.rs`）**
   - 新增 3 个带单位与理由注释的命名常量：`MIGRATE_MAX_AUDIO_MS = 12 * 60 * 60 * 1_000`（单条迁移录音 12 小时静音上限，防止损坏 `t_ms` 触发 OOM）、`MIGRATE_SILENCE_CHUNK_SAMPLES = 160_000`（16 kHz 下每次 10 秒分批流式补静音，内存恒为 `O(1)`）与 `STATE_UNMOVED_MAX_ROWS = 32`（约束 `State.unmoved` 行数守住 I8）。
   - 新增 3 个零 IO 纯函数：`safe_recording_name(name)`（拒绝空串、`/`、`\` 及非单段 `Component::Normal`）、`safe_chunk_path(path)`（拒绝绝对路径、`\`、`.`、`..` 及空段）与 `cap_unmoved(skipped)`（截断至前 32 条）。
   - 新增 `MoveReport { moved, already, skipped }` 与 `run_report(...) -> Result<MoveReport, &'static str>`，既有 `run(...)` 委托至 `run_report`，保持 100% 公开 API 兼容。
   - `moved.json` 改由 `write_owner` 经 `moved.json.tmp` → `write_all` → `sync_all` → `fs::rename` 原子落盘。
   - `reseal` 拒绝空 `audio.chunks`、非法块路径与超过 `MIGRATE_MAX_AUDIO_MS` 的 `chunk.t_ms`（均返回 `migrate.audio`），块间静音改为按 `MIGRATE_SILENCE_CHUNK_SAMPLES` 分批流式写入 `seal`。
2. **身份隔离、`State.unmoved` 封顶与 `migrate.done` 结构化事件（`core/src/app/{work,hub}.rs`、`docs/EVENTS.md`）**
   - `SyncKind::Migrated { by }` / `IndexSync::migrated(by)` 在 `desk` 同步前核对当前活跃 `shelf.by() == by`，匹配时执行 `idx.reconcile(&shelf, &tapes)` 并失效 `Library`、`Search`、`Recording`、`Transcript` 四个视图。
   - `Work::migrate` 调用 `migrate::run_report`，完成后发出零隐私字段结构化事件 `migrate.done`（`moved`、`already`、`skipped`、`code`、`ms`；仅 `code == "ok" && skipped == 0` 为 `info!`，否则 `warn!`），并附带目标身份发送 `Msg::Moved(me, res)`；**零新增 `Stats` 行**。
   - `Hub::apply_signed` 与 `Command::SignOut` 清空 `self.state.unmoved`；`Msg::Moved(by, res)` 仅当 `self.state.me == Some(by)` 时经 `migrate::cap_unmoved` 写入，否则显式丢弃（带 `// dropped:` 理由注释）。
   - 在 `docs/EVENTS.md`「身份与核心（hub / work）」登记 `migrate.done`（全仓事件总数 `84`）。
3. **边界单测与 175 条真实形态规模压测（`core/tests/migrate.rs`、`core/tests/migrate_scale.rs`）**
   - `core/tests/migrate.rs` 新增 3 条单元测试；
   - `core/tests/migrate_scale.rs` 新建 2 条端到端规模与竞态/隐私审计测试。

## 实测证据

- **红灯先验**：写入 5 条新测试后先跑 `RUSTC_WRAPPER="" cargo test -p zutalk-core --test migrate --test migrate_scale --no-run`，确认因 `safe_recording_name`、`safe_chunk_path`、`cap_unmoved`、`MoveReport`、`run_report`、`MIGRATE_MAX_AUDIO_MS`、`STATE_UNMOVED_MAX_ROWS` 尚未实现而编译失败。
- **格式、静态检查与全部 10 个规则脚本**：
  - `cargo fmt --all`：通过；
  - `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warning）；
  - 全部 10 个 `scripts/build/rules/*.sh`（`bounded` / `dropped` / `events` / `hub` / `layers` / `numbers` / `pending` / `shell` / `spawn` / `ui`）：全部退出码 `0`（0 欠账）；
  - `docs/EVENTS.md` 事件总数：`84`（`events.sh` 与 `every_event_in_the_catalogue_can_be_told_apart` 双重通过）。
- **新增与关联测试实测**：
  - `core/tests/migrate.rs`：8 passed，0 failed（1.51 s），含新增：
    - `safe_paths_and_unmoved_cap_rules`
    - `corrupt_audio_edge_cases_are_rejected_without_oom`
    - `run_report_counts_moved_already_and_skipped_accurately`
  - `core/tests/migrate_scale.rs`：2 passed，0 failed（7.90 s，远低于 15 s 预算）：
    - `scale_175_recordings_migration_with_adoption_topics_queries_idempotency_and_privacy`：175 条有效（6 个自然月、中/英/泰/日/韩多语、草稿/双版精修、手改、残句、说话人、笔记、10 条 `s16` 16 kHz 双块含静音 + 10 条 `f32` 48 kHz 双声道音频 + 155 条无音频）+ 6 条损坏条目 + 2 条本机 `did:key` 录音 + 2 条旧 `talk/term` 词表 + 旧 `settings.json` 经真实 `Core` 启动三轮（首搬 `moved=175/already=0/skipped=6` → 次启幂等 `0/175/6` → `Command::Remove` 删 1 条 → 三启防复活 `total=176`、`0/175/6`）；全查询（`LibrarySummary`/`LibraryMonth`/`Search` 含 2 字与多字中文/泰文/笔记/`Recording`/`Transcript`/`Compare`/`Topics`/`Glossary`）即时命中；真实落盘 `logs/*.jsonl`、`diagnose/**/*` 与 `settings.json` 逐字节扫描零秘词/密钥/DID 泄漏，`stats.full == 0`，`zutalk_log::refused()` 零增量，`Tally` 紧凑 JSON `≤ 8192 B`；
    - `sign_out_clears_unmoved_and_drops_stale_migration_result`：验证 40 条失败被封顶至 `STATE_UNMOVED_MAX_ROWS = 32`、登出即清空，以及后台 `job.move` 在登出后返回时陈旧 `Msg::Moved` 被丢弃不串台。
  - `core/tests/liveness.rs`：
    - `every_event_in_the_catalogue_can_be_told_apart`：通过；
    - `i8_what_one_change_sends_does_not_grow_with_the_library`：通过，`bytes_5=1332 bytes_100=1334 ratio=1.00 state_pushes=(0,0) inv_bytes=(60,60)`。
- **依赖与边界核验**：`Cargo.lock` 零变更，零新增外部 crate，零新增 `Stats` 行，未触碰 `/Users/kant/Zuzalu/zulangue`。

## 还差什么 / 需要负责人

- 本任务为纯代码与合成规模压测验证；全仓外沙盒 `scripts/build/check.sh` 将在 P7 批次结束后由协调者复跑；用真实 175 条 0.8 资料库副本在隔离目录验 `0.8.6` 补丁交接归入负责人最终验收清单。
