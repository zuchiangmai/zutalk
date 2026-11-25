状态：完成

## 改动与文件

1. **小状态彻底瘦身与命令精简（`core/src/app/proto.rs`、`core/src/view/query.rs`）**
   - 从 `State` 移除全部随录音与历史增长的字段：`library`、`open`、`hits`、`terms`、`city`、`writing`、`filing`、`moving`；仅保留 13 个类型上有界字段（`me`、`login`、`rotation_unsaved`、`update`、`keys`、`left`、`setup`、`devices`、`languages`、`exporters`、`rooms`、`unmoved`、`captions`）。
   - 为 `Row` 与 `Open` 显式加 `#[ts(export)]`，保持 `out/room.rs`、`Viewer.svelte`、`room.svelte.ts`、`text.test.ts` 在 P5 前继续可用。
   - 从 `Command`（含 `kind()`、`needs_person()`）移除旧 `Open`、`Close`、`Search`。
   - `ViewEngine` 新增 `calendar()` 供 `desk` 的 `Job::BeginFile` 直接读取缓存的城日历。
2. **精简 `Hub` 与 `Work`（`core/src/app/{core,hub,recording,work}.rs`、`docs/EVENTS.md`）**
   - `Hub` 改以极小私有集合 `refining: BTreeSet<String>`、`filing: BTreeSet<String>` 维护忙态防重，移除 `opened`、`search_seq` 与 `Msg::{RecordingOpened, Sealed, Writing, Filing, Moving, Searched, DeskChanged}`。
   - `Refreshed` 瘦身为只含 `rev`、`keys`、`left`、`rooms`；`desk` 的 `Change`、`Note`、`Remove`、`Restore`、`Term`、`DestroyAudio` 直接增量更新 SQLite 索引并发出精准 `Invalidate`，返回 `DeskOut::Silent`（不再触发整库扫描或重发 `State`）。
   - `Work` 缓存 `terms: Arc<Mutex<Vec<Term>>>`（由 `desk` 在 `run_sign` 与 `Job::Term` 同步），移除旧 `Work::search`、`search.stale` 计数与 `progress: Tx<Msg>` 通道；`docs/EVENTS.md` 同步从 `job.start` `kind` 清单移除旧 `search`。
   - 重新导出全部 37 个 TS 绑定（含更新后的 `State.ts`、`Command.ts`、`Row.ts`、`Open.ts`）。
3. **核心测试助手、测试全迁移与 I8 转正（`core/tests/{hub,bench}/mod.rs`、`core/tests/{app,city,glossary,retain,notes,takes,liveness}.rs`）**
   - `App` 与 `Bench` 新增 `query`、`library`、`recording`、`transcript`、`search`、`glossary`、`calendar`、`wait_library`、`wait_recording`、`wait_progress`、`wait_invalidated` 等异步助手；全部 7 份集成测试文件迁移到位并零警告编译通过。
   - 取消 `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 的 `#[ignore = "P2：状态瘦身"]`，补充断言：改标题触发 0 次 `State` 推送、`Invalidate` 负载 ≤ 256 字节、5 条 vs 100 条录音 `State` 字节数比 ≤ 1.2。
   - 更新 `liveness.rs` 2 分钟统计审计白名单（移除旧 `search.stale`，补齐 P2 新增的 `query.done`、`query.cancelled`、`index.rebuilds`、`view.invalidated`、`query.ms`、`index.rebuild_ms`）。
4. **最小 Svelte 界面适配与单测（`ui/src/lib/{core,words}.svelte.ts`、`ui/dev/mock.ts`、`ui/src/ui/{App,Home,Live,Reader,Settings,City,Meta}.svelte`、`ui/tests/store.test.ts`）**
   - `Bus` 增加 `query`（`tauri` 走 `invoke('query', { query })`）；`store` 增持 `progress` 与视图缓存（`library`、`openId`、`open`、`openLines`、`hits`、`terms`、`city`），带按视图版本号守卫防乱序覆盖，并按 `Event::Invalidate` 精准重拉。
   - `ui/tests/store.test.ts` 更新为瘦身 `State` 并新增 `Signal::Progress` 与 `Event::Invalidate` 乱序版本号守卫测试。

## 证据

- `cargo fmt --all -- --check` 通过。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`：37/37 通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：0 错误 0 警告（含全部 TCP 测试目标）。
- 全部 9 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`）全绿。
- `grep -rn 'ignore = "P2' core app server zu ui` 返回空（0 条挂起 P2 测试）。
- **I8 实测**（`liveness::i8_what_one_change_sends_does_not_grow_with_the_library`）：
  - `bytes_5 = 441 B`、`bytes_100 = 441 B`、`ratio = 1.00`（阈值 ≤ 1.2）
  - 改标题触发 `state_pushes = (0, 0)`，`inv_bytes = (60, 60)` B（阈值 ≤ 256 B）
- 非 TCP 核心与外壳测试全绿：`liveness::nothing_said_or_secret_reaches_the_log`（1/1）、`view.rs`（10/10）、`notes.rs`（3/3）、`takes.rs` 非 TCP（4/4）、`app.rs` 非 TCP 11 条全过（含 `a_burst_of_changes_is_read_back_once_or_twice` 实证 50 次改标题 0 次 `State` 推送与 0 次 `refresh`）、`zutalk`（`beat` 5/5、`pump` 11/11、`updates` 4/4）。
- UI：`svelte-check` 0 errors 0 warnings；`npm test` 37/37 通过（新增 2 条 `store.test.ts` 用例）。

## 遗留 / 待负责人确认

- TCP 集成测试（`city.rs`、`retain.rs`、`glossary.rs` 及 `app.rs`/`takes.rs`/`liveness.rs` 中启动本地端口的测试）已全部迁移并通过 `clippy --all-targets -D warnings` 编译校验，将在 P2 收尾沙盒外连跑全量 `scripts/build/check.sh` 时执行。
- 下一件 `P2-07` 实现 1,000 条录音 / 10 小时正文规模压测、I3 / I8 / I2 与扩展隐私测试。
