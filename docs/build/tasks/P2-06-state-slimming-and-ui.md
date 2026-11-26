# P2-06 状态瘦身、`Command` 精简、最小 Svelte 适配与 I8 转正

## 目标

做完以后：

1. **`State` 严格有界**（`PLAN.md` §2.4）：从 `State` 移除所有会随资料库或录音增长的字段：`library`、`open`、`hits`、`terms`、`city`、`writing`、`filing`、`moving`；`State` 字节数不再随录音数量或长度增长。
2. **移除冗余命令**（原则 3）：从 `Command` 移除 `Open`、`Close`、`Search`（全部改走 `Core::query`）；移除 `hub.rs` 中整库刷新 `mark_dirty` / `Refreshed` 里读全库与读全文的旧路径，只保留轻量的小状态刷新（`keys`、`left`、`rooms`）和 `desk` 增量同步。
3. **转正并通过 I8**：取消 `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 上的 `#[ignore = "P2：状态瘦身"]`，改一次标题后 5 条录音 vs 100 条录音的 `State` 字节数之比 ≈ 1.00（≤ 1.2）。
4. **核心测试全迁移**：`core/tests/{app,city,glossary,retain,liveness}.rs` 与 `core/tests/{hub,bench}/mod.rs` 迁移到 `Core::query` + `Outlet.progress` + `Outlet.invalidated`，全部行为断言不降级。
5. **最小 Svelte 界面适配**：`ui/`（`core.svelte.ts`、`dev/mock.ts`、`App.svelte`、`Home.svelte`、`Reader.svelte`、`Settings.svelte`、`City.svelte`、`Meta.svelte`、`ui/tests/store.test.ts`）适配瘦身后的 `State` + `query` + `Progress` + `Invalidate`，保持 `npm run check && npm test` 全绿（供 P3 换 React 19 前持续可用）。

## 背景

- 设计：`docs/build/phases/P2.md` §2（I8）、§3.6.1；`docs/PLAN.md` §2.4、§2.5、§4.2、§5.2。
- 涉及文件：
  - `core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`
  - `core/tests/hub/mod.rs`、`core/tests/bench/mod.rs`、`core/tests/{app,city,glossary,retain,liveness}.rs`
  - `core/bindings/*.ts`（`cargo test` 由 `ts-rs` 自动重新生成）
  - `ui/src/lib/core.svelte.ts`、`ui/dev/mock.ts`、`ui/src/ui/{App,Home,Reader,Settings,City,Meta}.svelte`、`ui/tests/store.test.ts`
  - `docs/EVENTS.md`（同步删去不再发出的旧 `job.start` `kind = "search"` 说明）

## 做法

### 1. 瘦身 `State` 与 `Command`（`core/src/app/proto.rs`）

- `State` 只保留有界字段：
  ```rust
  pub struct State {
      pub me: Option<String>,
      pub login: Option<String>,
      pub rotation_unsaved: bool,
      pub update: Option<String>,
      pub keys: Vec<(String, From)>,
      #[ts(type = "number | null")]
      pub left: Option<u64>,
      pub setup: Setup,
      pub devices: Vec<Device>,
      pub languages: Vec<String>,
      pub exporters: Vec<String>,
      pub rooms: Vec<Room>,
      pub unmoved: Vec<(String, String)>,
      pub captions: bool,
  }
  ```
- **保留 `pub struct Open` 和 `pub struct Row` 并加 `#[ts(export)]`**：`core/src/out/room.rs`（P5 才删单条分享）、`ui/src/lib/room.svelte.ts`、`ui/src/ui/Viewer.svelte`、`ui/tests/text.test.ts` 仍引用 `Open` / `Row`，保留导出避免无谓波及 P5 范围。
- 从 `Command` 删除 `Open { id }`、`Close`、`Search { query }`，同步更新 `Command::kind`、`Command::needs_person` 与 `core/tests/app.rs::commands_that_need_a_person_are_listed_in_one_table`。

### 2. 简化 `Hub` 与 `Work`（`core/src/app/hub.rs`、`core/src/app/work.rs`）

- `Hub`：
  - 不再存 `opened`、`search_seq`，也不再在 `State` 上维护 `library`/`open`/`hits`/`terms`/`city`/`writing`/`filing`/`moving`；
  - 保留极小的私有集合 `refining: BTreeSet<String>` 与 `filing: BTreeSet<String>` 供 `busy(&id)` 与防重复投递使用（绝不进 `State`）；
  - `Refreshed` 瘦身为只带 `rev`、`keys`、`left`、`rooms: Option<Vec<Room>>`：改标题、加笔记、删录音等不再触发 `shelf.list()` 全库重读，而是 `desk` 写完事实后直接增量更新 SQLite `Index` + 发 `Invalidate`！
  - `Command::File { id, event, audio }` 不再从 `self.state.city` 取日历，改为交 `work.begin_file(id, event, audio)`，由 `desk` 从 `ViewEngine` 缓存的 `Calendar` 读取（空则回 `"city.none"`）。
  - `Work::start_recording` 与 `Work::write_recovered` 不再由 `hub` 传入 `terms`，改为由 `Work` 直接从当前 `Shelf` / 词表缓存读取。
  - 删除旧 `Work::search`、`Msg::Searched` 与 `search.stale` 计数（由 `ViewEngine::query(Query::Search)` 与 `query.cancelled` 取代）。
  - 确认 `hub.sh` 100% 通过！

### 3. 核心测试助手与测试迁移

- `core/tests/hub/mod.rs`（`App`）与 `core/tests/bench/mod.rs`（`Bench`）新增便捷异步助手：
  - `query(&self, q: Query) -> Result<QueryReply, &'static str>`
  - `library(&self) -> Vec<LibraryRow>`（调 `Query::LibraryRecent`）
  - `recording(&self, id: &str) -> Option<RecordingView>`
  - `transcript(&self, id: &str, take: Option<u32>, chunk: u32) -> Option<TranscriptChunk>`
  - `search(&self, q: &str) -> SearchResult`
  - `glossary(&self) -> Vec<Term>`、`calendar(&self) -> Option<Calendar>`
  - `wait_invalidated(...)` / `wait_query(...)` / `wait_progress(...)`（等待寄存器变化后再查，确定无竞态！）
- 迁移 `core/tests/app.rs`、`core/tests/city.rs`、`core/tests/glossary.rs`、`core/tests/retain.rs`、`core/tests/liveness.rs` 中所有读取旧 `state.library`/`open`/`hits`/`terms`/`city`/`writing`/`filing`/`moving` 和发送旧 `Open`/`Close`/`Search` 的测试。
- **特别关注 `i8_what_one_change_sends_does_not_grow_with_the_library`**（`core/tests/liveness.rs:2688`）：
  - 去掉 `#[ignore = "P2：状态瘦身"]`！
  - `measure_state_bytes_for_library(count)` 中等待 `count` 条录音入索引（通过 `Query::LibrarySummary`），改一次标题，等待 `Invalidate(Recording, Some(target))` 且 `Query::Recording` 读到 `"renamed-once"`，同时测量：
    1. `bench.latest_state_bytes()`（5 条 vs 100 条之比 ≤ 1.2，实测 ≈ 1.00）；
    2. 本次改标题发出的 `State` 推送次数（为 0 次，因为改标题只发 `Invalidate` 不重发 `State`！）与 `Invalidate` 序列化字节数（≤ 256 B，与库大小无关）。

### 4. 最小 Svelte 界面适配（`ui/`）

- `ui/src/lib/core.svelte.ts`：
  - `Bus` 新增 `query(q: Query): Promise<QueryReply>`；`tauri` 实现为 `invoke('query', { query })`。
  - `store` 新增：
    - `progress = $state.raw<Progress>({ refine: null, file: null, move_old: null })`
    - `library = $state.raw<LibraryRow[]>([])`
    - `openId = $state.raw<string | null>(null)`、`open = $state.raw<RecordingView | null>(null)`、`openLines = $state.raw<[string, Said][]>([])`
    - `hits = $state.raw<SearchHit[]>([])`
    - `terms = $state.raw<Term[]>([])`
    - `city = $state.raw<Calendar | null>(null)`
  - 提供轻量的视图拉取函数 `refreshLibrary()`、`openRecording(id)`、`closeRecording()`、`searchRecordings(q)`、`refreshGlossary()`、`refreshCalendar()`，带每视图最新 `version` 守卫忽略过期回复；收到 `Event::Invalidate(inv)` 时按 `inv.view` / `inv.key` 刷新当前需要的视图；首次收到 `State`（且 `me` 或 boot 就绪）时拉取初始 `library`、`terms`。
- `ui/dev/mock.ts`：
  - 更新 `EMPTY: State` 为瘦身后的 13 字段；
  - `mockBus` 实现 `query(q: Query)`、发送 `Signal::Progress` 与 `Event::Invalidate`；移除旧 `Open`/`Close`/`Search` handler。
- `ui/src/ui/{App,Home,Reader,Settings,City,Meta}.svelte`：
  - 改为读 `store.library`、`store.open`、`store.openLines`、`store.hits`、`store.terms`、`store.city`、`store.progress`；
  - `Home.svelte` 点击录音调 `openRecording(row.id)`，搜索框调 `searchRecordings(query)`；`App.svelte` 返回调 `closeRecording()`；`Reader.svelte` 复制/导出继续工作（可用 `openLines` 拼文本）。
- `ui/tests/store.test.ts`：更新断言匹配瘦身 `State`，补充 `Signal::Progress` 与 `Event::Invalidate` 触发视图重拉的单元测试。
- 检查 `core/bindings/`：删去不再由 `ts-rs` 产生的孤立旧 `.ts` 文件（如有），确保 `npm run check` 零警告零错误。

## 显式与可观测

- **状态有界显式**：`State` 类型定义本身不再包含任何无界历史集合；`i8_what_one_change_sends_does_not_grow_with_the_library` 直接以序列化字节数和 `Invalidate` 大小守住不变量。
- **统计更新**：移除旧 `search.stale`（由 `query.cancelled` 承接），统计表维持在既有上限内（计数 ≤ 40、分布 ≤ 12）。
- **事件对齐**：更新 `docs/EVENTS.md` 中 `job.start` 的 `kind` 清单（移除旧 `search`，保留 `index.sync` 等），保证 `events.sh` 全绿。

## 先写的测试

1. 取消 `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 的 `#[ignore]`：在旧 `State` 上实测 `ratio > 5.0` 失败，瘦身后 `ratio ≈ 1.00 <= 1.2` 通过。
2. `ui/tests/store.test.ts` 新增：
   - 收到 `Signal::Progress` 更新 `store.progress` 且不被 `State` 覆盖；
   - 收到 `Event::Invalidate { view: 'Library', ... }` 触发 `bus.query({ view: 'LibraryRecent', ... })` 并忽略版本号更旧的乱序回复。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 9 个规则脚本、`cargo test --workspace`、`ui` `npm run check && npm test`）。
- `i8_what_one_change_sends_does_not_grow_with_the_library` 实际运行并通过（不再 ignored）；日志打印 `bytes_5`、`bytes_100` 与 `ratio`。
- `grep -rn 'ignore = "P2' core app server zu ui` 返回空。

## 不做

- 不重做界面视觉或换 React 19（P3）。
- 不删单条录音分享 `talk/share` 和 `out/room.rs`（P5）。
- 不写 1,000 条 / 10 小时规模基准测试（P2-07）。
