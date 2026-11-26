状态：完成

## 改动与文件

1. **协议与出口寄存器扩展（`core/src/app/proto.rs`、`core/src/app/core.rs`、`core/src/view/index.rs`）**
   - `proto.rs` 统一重导出 `Note`、`RefineFailure`、`RefineStatus`、`TakeInfo`、`LibraryFilter`、`LibraryPage`、`LibraryRow`、`LibrarySummary`、`MonthCount`、`SearchHit`、`SearchHitKind`、`SearchResult`、`CompareChunk`、`CompareRow`、`DiffOp`、`TranscriptChunk`、`Invalidate`、`Invalidations`、`OutboundRow`、`Query`、`QueryReply`、`QueryValue`、`RecordingView`、`ViewKind`。
   - 新增 `Progress { refine, file, move_old }`（`#[ts(export)]`）、`Signal::Progress(Arc<Progress>)` 与 `Event::Invalidate(Invalidate)`。
   - `Outlet` 新增 `progress: Reading<Progress>` 与 `invalidated: Reading<Invalidations>`；`Core::start` 共享同一个 `work` 子树初始化 `ViewEngine` 与 `Work`；新增 `pub async fn Core::query(&self, query: Query) -> Result<QueryReply, &'static str>`，完全绕开 `hub` 邮箱。
   - `Index::reconcile` 开放为 `pub` 供 `desk` 搬运完成后同步使用。
2. **`Work` 单写手索引同步与进度信号（`core/src/app/work.rs`、`core/src/app/recording.rs`）**
   - `DeskEnv` 独占 `index: Mutex<Option<Index>>` 与 `views: Arc<ViewEngine>`；`run_sign`（启动、登录、登出）按身份打开/重建/对账 `<data>/index/<did>.sqlite3` 并调用 `views.set_source(...)` 与 `views.invalidate_all()`。
   - 所有 `desk` 写事实作业（`Change`、`Note`、`RefinePrepare`、`Remove`、`Restore`、`DestroyAudio`、`Term`、`BeginFile`、`OwnKey`、`Refresh` 中的 `retain::sweep`）完成后立即增量同步索引并发出精准的 `Invalidate`。
   - 新增 `Job::IndexSync`（`kind = "index.sync"`）与带完成回执的 `IndexSync` 句柄，保证录音开（`recording::run`）、录音封（`after_seal`）、精修完成/失败（`write_job`）、交城状态变更（`file`）、0.8 搬运完成（`migrate`）全部顺序过 `desk` 单写手提交索引并失效视图；`Work::city` 拿到日历后直接更新 `views.set_calendar` 并失效 `Calendar`。
   - `write_job`、`file`、`migrate` 实时维护 `Outlet.progress`（`refine` / `file` / `move_old`）并同步 `views.set_active_refine`；`hub.rs` 零新增 IO，继续保持纯状态机。
3. **外壳接线与界面防破坏过渡（`app/src/main.rs`、`app/src/pump.rs`、`ui/src/lib/core.svelte.ts`、`docs/EVENTS.md`）**
   - `app/src/main.rs` 注册异步命令 `#[tauri::command] async fn query(query: Query, core: State<'_, Core>) -> Result<QueryReply, String>`。
   - `app/src/pump.rs`：`Fan` 新增 `progress` 槽（重连补发），`Event::Invalidate` 只广播不缓存；`PumpState` 监听 `outlet.progress` 与 `outlet.invalidated`，并在失效环溢出时自动降级为 `ViewKind::ALL` 全量失效（`key: None`）。
   - `ui/src/lib/core.svelte.ts` 显式接住 `Signal::Progress` 与 `Event::Invalidate`（视图重拉留 P2-06）。
   - `docs/EVENTS.md` 在 `job.start` 的 `kind` 列表中补记 `note`、`refine.prepare`、`index.sync`。
   - 重新生成 `core/bindings/{Event,Progress,Signal}.ts` 等 35 个 TS 绑定。

## 证据

- `cargo fmt --all -- --check` 通过。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`：35/35 通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：0 警告通过。
- 全部 9 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`）全绿。
- `core/tests/view.rs`：10/10 通过（0.43s），新增端到端测试：
  - `core_query_reflects_recording_notes_changes_search_takes_and_invalidates`（覆盖完整 `Core::start`、导入封盘、`LibrarySummary`/`LibraryRecent`/`Search`/`Recording`/`Transcript`/`Compare`/`Glossary`、`Command::Note`、`Command::Change(Title)`、`Command::Term`、`Command::DestroyAudio`、`Command::Refine` 推进与清零 `Outlet.progress` 及持久失败投影、`Outlet.invalidated` 单调递增）
- `app/tests/pump.rs`：11/11 通过（0.00s），新增 2 条测试：
  - `progress_signal_is_replayed_to_late_joiners_and_invalidations_are_not`
  - `invalidation_overflow_falls_back_to_coarse_view_invalidations`
- `core/tests/notes.rs`（3/3）、`liveness::every_event_in_the_catalogue_can_be_told_apart`（1/1）、`app/tests/{beat,updates}.rs`（9/9）、`ui` `svelte-check`（0 errors, 0 warnings）+ `npm test`（35/35）全绿。

## 遗留 / 待负责人确认

- `State` 上的旧无界字段与旧 `Open`/`Close`/`Search` 命令在本件按计划暂留过渡，将在下一件 `P2-06` 一次移除并同步迁移全部核心测试与 Svelte 界面。
