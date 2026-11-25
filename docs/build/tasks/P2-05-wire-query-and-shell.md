# P2-05 全线接入：`Core::query`、`Index` 同步、`Progress`/`Invalidate` 与外壳转发

## 目标

做完以后：

1. **单写手索引同步**（`core/src/app/work.rs`）：`desk` 独占本机 SQLite `Index` 写连接，启动与切身份自动打开/重建/对账；每次 `Change`、`Note`、`Remove`、`Restore`、`DestroyAudio`、`retain::sweep`、录音开/封、精修完成/失败、交城状态变化、0.8 搬运完成、词表修改、城日历更新都增量更新索引（或内存缓存）并经 `ViewEngine` 发出精准的 `Invalidate { view, key, version }`。
2. **异步查询入口**（`core/src/app/core.rs`）：新增 `pub async fn Core::query(&self, query: Query) -> Result<QueryReply, &'static str>`，直接委托 `ViewEngine` 走 L3，不经过 `hub` 邮箱。
3. **新增寄存器与协议变体**（`core/src/app/{core,proto}.rs`）：
   - `Outlet` 新增 `pub progress: Reading<Progress>` 与 `pub invalidated: Reading<Invalidations>`；
   - `proto::Progress { refine: Option<Writing>, file: Option<Writing>, move_old: Option<Writing> }`（`#[ts(export)]`）；
   - `Signal` 新增 `Progress(Arc<Progress>)`；`Event` 新增 `Invalidate(Invalidate)`；`proto.rs` 统一重导出 `view` 协议类型。
4. **外壳零锁接入**（`app/src/main.rs`、`app/src/pump.rs`）：
   - 新增异步命令 `#[tauri::command] async fn query(query: Query, core: State<'_, Core>) -> Result<QueryReply, String>` 并注册进 `generate_handler!`；
   - `Fan` 与 `PumpState` 转发 `Signal::Progress`（重连补发）与 `Event::Invalidate`（广播不缓存；落后超过 64 条时对全部 `ViewKind` 发全量失效）；`shell.sh`、`hub.sh` 100% 通过。
5. **平滑过渡**：本件暂不删除 `State` 上的旧字段（留给 P2-06 一次瘦身），确保 `ui/` 仅加 2 行分支适配新 `Event::Invalidate` 与 `Signal::Progress` 即可全仓绿。

## 背景

- 设计：`docs/build/phases/P2.md` §3.4、§3.6；`docs/PLAN.md` §4.1、§5.2。
- 涉及文件：
  - `core/src/app/proto.rs`、`core/src/app/core.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`
  - `app/src/main.rs`、`app/src/pump.rs`、`app/tests/pump.rs`
  - `ui/src/lib/core.svelte.ts`（适配 `Event::Invalidate` 与 `Signal::Progress`，否则 `ts-rs` 刷新 `Event.ts` 后 `else say(e.is)` 会类型报错！）

## 做法

### 1. 协议与 `Outlet`（`core/src/app/proto.rs`、`core/src/app/core.rs`）

- `proto.rs`：
  - 重导出 `crate::record::notes::Note`、`crate::record::takes::{RefineFailure, RefineStatus, TakeInfo}`、`crate::view::index::{LibraryFilter, LibraryPage, LibraryRow, LibrarySummary, MonthCount, SearchHit, SearchHitKind, SearchResult}`、`crate::view::transcript::{CompareChunk, CompareRow, DiffOp, TranscriptChunk}`、`crate::view::query::{Invalidate, OutboundRow, Query, QueryReply, QueryValue, RecordingView, ViewKind}`。
  - 新增 `pub struct Progress { pub refine: Option<Writing>, pub file: Option<Writing>, pub move_old: Option<Writing> }`（`#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)] #[ts(export)]`）。
  - `Signal` 增加 `Progress(Arc<Progress>)`；`Event` 增加 `Invalidate(Invalidate)`。
- `core.rs`：
  - `Outlet` 增加 `pub progress: Reading<Progress>` 与 `pub invalidated: Reading<Invalidations>`。
  - `Core::start` 创建 `Register<Progress>` 与 `ViewEngine::new(work_tree, config.tapes())`（拿到 `(views, invalidated_reading)`），把 `Arc<ViewEngine>` 与 `Register<Progress>` 传入 `Work` 和 `Core`。
  - `Core` 新增 `pub async fn query(&self, query: Query) -> Result<QueryReply, &'static str> { self.views.query(query).await }`。

### 2. `Work` 里的单写手 `Index` 同步与 `Progress` 更新（`core/src/app/work.rs`）

- 在 `DeskEnv` 内持有 `index: Mutex<Option<Index>>`（仅在 `desk` 单线程顺序作业中访问，绝无并发写竞争）与 `views: Arc<ViewEngine>`。
- 索引文件路径 helper：`fn index_path(data: &Path, shelf_by: &str) -> PathBuf { data.join("index").join(format!("{}.sqlite3", shelf_by.replace(':', "-"))) }`。
- `run_sign`（启动、登录、登出）：
  - 若有 `shelf`，调用 `Index::open(&index_path, &shelf, &tapes, env.root.stats())` 打开/重建/对账，存入 `env.index`，并调 `env.views.set_source(Some(shelf), Some(index_path), has_soniox_key, true)` + `env.views.invalidate_all()`；
  - 若 `Who::Nobody`，清空 `env.index`，调 `env.views.set_source(None, None, false, true)` + `env.views.invalidate_all()`。
- 每次 `desk` 写完事实立即增量同步索引并发出失效通知：
  - `Job::Change { id, change }`：`index.upsert_recording(...)`，发 `Invalidate(Recording, Some(id))` +（若改标题/行发 `Library` / `Transcript` / `Search`）；
  - `Job::Note { id, op }`：`index.upsert_recording(...)`，发 `Invalidate(Recording, Some(id))` + `Invalidate(Search, None)`；
  - `Job::Remove { id }`：`index.remove_recording(&id)`，发 `Invalidate(Library, None)` + `Invalidate(Search, None)` + `Invalidate(Recording, Some(id))`；
  - `Job::Restore { id }`：`index.upsert_recording(...)`，发 `Invalidate(Library, None)` + `Invalidate(Search, None)` + `Invalidate(Recording, Some(id))`；
  - `Job::DestroyAudio { id, .. }`：`index.update_audio(&id, false)`，发 `Invalidate(Library, None)` + `Invalidate(Recording, Some(id))`；
  - `Job::Term { .. }`：发 `Invalidate(Glossary, None)`；
  - `Job::BeginFile { id, .. }`：`index.update_filing(&id, "")`，发 `Invalidate(Recording, Some(id))` + `Invalidate(Outbound, None)`；
  - `Job::OwnKey`：更新 `views.set_has_soniox_key(...)` + `Invalidate(Recording, None)`；
  - `retain::sweep` 返回被毁 `ids`：逐个 `index.update_audio(&id, false)`，若非空发 `Invalidate(Library, None)` + `Invalidate(Recording, None)`。
- 新增轻量 `desk` 作业 `Job::IndexSync { kind: SyncKind }` 供异步后台任务（录音开、录音封、精修完成/失败、交城完成、0.8 搬运完成）在写完 pack 后顺序同步索引并发出 `Invalidate`（用 `desk.send(...)`，单写手零竞争！）：
  - 录音开启（`recording::run` 写完初始 `about` 后）→ `upsert_recording(id)` + `views.set_active_recording(Some(id))` + `Invalidate(Library)` + `Invalidate(Recording, Some(id))`；
  - 录音封盘（`after_seal`）→ `views.set_active_recording(None)` + `upsert_recording(id)` + `Invalidate(Library, Search, Recording(id), Transcript(id))`；
  - 精修完成/失败 → 更新 `views.set_active_refine(...)` + `upsert_recording(id)`（或 `update_refine_error`）+ `Invalidate(Library, Search, Recording(id), Transcript(id))`；
  - 交城状态变更 → `update_filing(id, state)` + `Invalidate(Library, Recording(id), Outbound)`；
  - 0.8 搬运完成 → `index.reconcile(shelf, tapes)` + `Invalidate(Library, Search)`；
  - `Work::city` 拿到 `Ok(calendar)` → 调 `views.set_calendar(Some(calendar))` + `Invalidate(Calendar, None)`。
- `Progress` 信号维护：
  - `Work` 持有 `progress_reg: Register<Progress>`，并在精修（`write_job`）、交城（`file_job`）、搬运（`migrate`）推进与结束时直接更新对应槽位（`refine` / `file` / `move_old`），同时同步更新 `views.set_active_refine` 供 `Query::Recording` 读取。
  - 注意：`hub.rs` 不增任何 IO 或新 `.await`，继续 100% 通过 `hub.sh`！

### 3. 外壳接线（`app/src/main.rs`、`app/src/pump.rs`）

- `app/src/main.rs`：
  - 新增 `#[tauri::command] async fn query(query: Query, core: State<'_, Core>) -> Result<QueryReply, String> { core.query(query).await.map_err(str::to_owned) }`；
  - 在 `tauri::generate_handler![dispatch, subscribe, drawn, query]` 注册。
- `app/src/pump.rs`：
  - `Fan` 增加 `progress: Option<Stamped>` 字段：
    - `join_checked` 的 `replay` 数组中加入 `self.progress.clone().map(Event::Signal)`；
    - `push` 处理 `Signal::Progress(_)` 与 `Event::Invalidate(_)`（`Invalidate` 只广播，不存入 `Fan` 缓存）。
  - `PumpState` 增加 `progress_seq: u64` 与 `invalidated_at: u64`：
    - `seed` 读取 `outlet.progress.now()` 与初始化 `invalidated_at = outlet.invalidated.now().1.next`（新启动不补发旧失效）；
    - `handle_invalidations(&mut self, inv: &Invalidations)`：若 `inv.next > self.invalidated_at`：
      - 若 `inv.items.first().is_some_and(|first| self.invalidated_at + 1 < first.version)`（溢出），按 `ViewKind::ALL` 每个发一条 `Event::Invalidate(Invalidate { view, key: None, version: inv.next })`；
      - 否则发所有 `item.version > self.invalidated_at` 的项；最后设 `self.invalidated_at = inv.next`。
    - `step_loop` 的 `tokio::select!` 增加 `outlet.progress.next()` 与 `outlet.invalidated.next()` 两个分支。

### 4. 界面最小防破坏补丁（`ui/src/lib/core.svelte.ts`）

- 在 `take(e: Event)` 中显式处理 `e.kind === 'Invalidate'`（本件暂不做重拉，留 P2-06）与 `signal.of === 'Progress'`，把最后的 `else say(e.is)` 改为 `else if (e.kind === 'Notice') say(e.is)`，保证 `npm run check` 在新 `Event.ts` 下 100% 通过。

## 显式与可观测

- **事件与统计**：复用 P2-03/P2-04 已登记的 `index.open`、`index.sync`、`query.done`、`query.slow`、`view.invalidate` 与 `job.start`/`job.done(kind = "index.sync")`；`docs/EVENTS.md` 的 `job.start` `kind` 列表补记 `index.sync` 与 `note` / `refine.prepare`（若 P2-01 尚未补记一并补齐）。
- **单写手与零锁边界**：`shell.sh` 守住 `app/src` 零锁全异步，`hub.sh` 守住 `hub.rs` 零 IO，SQLite 写操作严格收敛在 `desk` 顺序队列。
- **失效溢出安全**：`PumpState` 溢出时降级为全视图 `key = None` 失效，杜绝静默漏刷新。

## 先写的测试

1. `core/tests/view.rs` 新增全核心端到端测试：
   - `core_query_reflects_recording_notes_changes_search_takes_and_invalidates`：起完整 `Core`，录一段或导入一段、发 `Command::Note`、`Command::Change(Title)`、`Command::Refine`、`Command::Term`、`Command::DestroyAudio`，断言 `Outlet.invalidated` 收到对应 `ViewKind` 且 `version` 单调递增、`Core::query`（`LibrarySummary`/`LibraryRecent`/`Search`/`Recording`/`Transcript`/`Compare`/`Glossary`）全部返回最新数据、且 `Outlet.progress` 反映精修过程。
2. `app/tests/pump.rs` 新增测试：
   - `progress_signal_is_replayed_to_late_joiners_and_invalidations_are_not`：验证 `Signal::Progress` 重连补发、`Event::Invalidate` 只广播不补发；
   - `invalidation_overflow_falls_back_to_coarse_view_invalidations`：连推超过 64 条不同 key 的失效，验证 `pump` 自动发齐 `ViewKind::ALL` 全量失效且无重复。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（9 个规则脚本、`cargo test --workspace`、`ui` check & test）。
- `shell.sh` 与 `hub.sh` 100% 干净。

## 不做

- 不在本件删 `State` 的旧字段或改 6 个 Svelte 页面（P2-06）。
- 不写 1,000 条 / 10 小时规模压测（P2-07）。
