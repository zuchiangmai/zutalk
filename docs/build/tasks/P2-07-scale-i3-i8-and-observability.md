# P2-07 规模压测与不变量验证：1,000 条录音、10 小时正文、I3、I8、I2 与隐私

## 目标

做完以后，P2 全部性能出口与架构不变量都有确定、可重复、在门禁内稳定运行的自动化测试守着：

1. **1,000 条录音规模**（`core/tests/view_scale.rs`）：
   - 跨 12 个月、含中文 / 泰文（含 SARA AM）/ 英文标题、正文、译文与笔记的 1,000 条录音；
   - `LibrarySummary`（按月计数）、`LibraryMonth`（某月分页）、`LibraryRecent`（侧栏最近 50 条）、`Search`（中文 4 字 FTS5、中文 2 字短词回退、泰文、英文、标题、笔记）**全部 ≤ 100 ms**。
2. **10 小时正文规模**（`core/tests/view_scale.rs`）：
   - 单场 10 小时录音，take 0（现场稿）与 take 1（精修稿）各 6,400 行（100 块 × 64 行）；
   - 第 0 块、第 50 块、第 99 块的 `Query::Transcript` 与 `Query::Compare` **全部 ≤ 100 ms**，每块至多 64 行。
3. **I8（代价与历史无关）**：
   - `i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration`：对比 1 分钟（12 行）与 4 小时（2,400 行）两场录音的单块查询与对比：响应大小均受 64 行封顶，两者耗时均 ≤ 100 ms；
   - 配合 P2-06 已转正的 `i8_what_one_change_sends_does_not_grow_with_the_library`（5 条 vs 100 条 `State` 字节数比 ≈ 1.00）。
4. **I3（内存、队列与统计有上限）**：
   - `i3_rebuild_queries_and_draft_stay_within_bounds`：覆盖 1,000 条重建 + 连续 50 次查询 + `DraftWriter` 10,000 行写入，断言 `stats.full` 事件数为 0、全部 `Tally.queues` 深度归零且 `Tally` 序列化 ≤ 8192 字节、`Tree::running()` 任务数平稳不泄漏、每条 `QueryReply` 负载严格受页/块上限约束。
5. **I2（并发 SQLite 查询不挡现场字幕）**：
   - `i2_concurrent_view_queries_do_not_stall_live_captions`：录音进行中，同时在 1,000 条录音索引上并发发起多次 `Search`、`LibraryMonth`、`Transcript`、`Compare`，断言现场字幕帧照常推进、零 `Stalled`、命令照答。
6. **日志隐私扩展**（I9）：
   - 扩展 `core/tests/liveness.rs::nothing_said_or_secret_reaches_the_log`：加入带秘密哨兵词的 `Command::Note`、`Core::query(Query::Search { query: ... })`、`Query::Recording`、`Query::Transcript`、`Query::Compare`、`Command::Refine`，验证落盘 `zutalk.jsonl` 中零泄露、零 `"!"` 拒收。

## 背景

- 设计：`docs/build/phases/P2.md` §1（E5、E6、E8）、§2；`docs/PLAN.md` §7（I2、I3、I8、I9）、§8（P2 出口）。
- 涉及文件：
  - 新增 `core/tests/view_scale.rs`
  - 更新 `core/tests/liveness.rs`（`nothing_said_or_secret_reaches_the_log` 与 I2/I8 协同校验）
- **速度与确定性注意**：
  - 1,000 条录音如果逐条签 1,000 个独立目录 + 多次 `fsync`，在测试里初始化本身可能花十几秒；
  - 做法：提供测试专用的批量造库助手——
    - 一条测试用真实 `Shelf` 建 100 条录音的 pack，测真实损坏删库 + 从 pack 全量流式 `Index::open` 重建（耗时 < 1 s）；
    - 1,000 条录音与 10 小时（6,400 行 × 2 版）查询压测：通过单个 `Shelf` 批量写入（关闭或合并提交，或用单事务构造合法 SQLite 索引并通过 `Core::query` / `ViewEngine` 真实运行 SQLite + FTS5 查询），保证整个 `view_scale.rs` 总运行时间在 **3–5 秒以内**，不拖慢 `scripts/build/check.sh`！
  - `[profile.dev.package."*"]` 已开启 `opt-level = 1`，`rusqlite` 编译的 SQLite C 引擎在 `cargo test` 下即有接近 release 的查询速度（通常 1–10 ms），远低于 100 ms 阈值。

## 做法

### 1. 新建 `core/tests/view_scale.rs`

实现以下 5 条高信号测试（每条打印实测耗时供报告记录）：

1. `scale_1000_recordings_library_and_fts5_search_under_100ms`：
   - 造 1,000 条跨 12 个月的多语种录音（每条含标题、20 行多语种正文与译文、2 条带时间笔记，共约 20,000 行正文 + 2,000 条笔记）；在其中第 17、513、988 条埋入唯一检索词（中文 4 字「清迈读书」、中文 2 字「押金」、泰文含 SARA AM `น\u{e49}\u{e33}หนักพิเศษ`、英文 `ZuzaluQuarterly`、笔记词 `NoteAnchorBeta`、标题词 `TitleGamma`）。
   - 经 `ViewEngine::query`（真实走 `Tree::blocking` + `SQLITE_OPEN_READ_ONLY` WAL）依次测：
     - `Query::LibrarySummary { tz_offset_min: 480 }`：验证 `total == 1000`、12 个月之和 == 1000，耗时 ≤ 100 ms；
     - `Query::LibraryMonth`（任选 1 个月 + 翻第 2 页 + 过滤）：结果准确，耗时 ≤ 100 ms；
     - `Query::LibraryRecent { limit: 50, offset: 0 }`：返回 50 条最新顺序，耗时 ≤ 100 ms；
     - 6 次 `Query::Search`（中文 4 字、中文 2 字短词、泰文 SARA AM 异写、英文大小写、标题、笔记）：全部精准命中，每次 ≤ 100 ms。
   - 断言全部 `query.slow` 事件数为 0。

2. `scale_10_hour_transcript_chunk_and_compare_under_100ms`：
   - 造 1 场 10 小时录音（`length_ms = 36_000_000`），take 0（现场稿）6,400 行、take 1（精修稿）6,400 行（100 块 × 64 行，每第 10 行做文字修订）。
   - 经 `ViewEngine::query` 依次查 `Transcript`（chunk 0、50、99）与 `Compare`（take 0 vs take 1，chunk 0、50、99）：
     - 每块 `lines.len() == 64`（`total_lines == 6400`、`total_chunks == 100`）；
     - `Compare` 每块准确标出修改行且 `DiffOp` 满足文本重建；
     - 每一次查询耗时 ≤ 100 ms；`query.slow` 事件数为 0。

3. `i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration`：
   - 在同一索引里放入 1 分钟录音（12 行 × 2 版）与 4 小时录音（2,560 行 = 40 块 × 2 版）。
   - 分别执行 `Transcript`（首块与末块）与 `Compare`（首块与末块）：
     - 断言响应行数 ≤ 64、序列化字节数有界（与整场 2,560 行总大小无关）；
     - 4 小时录音单块与 1 分钟录音单块耗时均 ≤ 100 ms（打印两者微秒级耗时与比值）。

4. `i3_rebuild_queries_and_draft_stay_within_bounds`：
   - 真实 `Shelf` 落盘 100 条录音 → 写坏 `.sqlite3` → `Index::open` 自动流式重建（断言 `index.open(mode = "rebuilt", why = "corrupt")` 与 `index.rebuilds == 1`）；
   - 连续发起 50 次混合查询（含故意并发触发取消的搜索）+ 200 次 `invalidate`；
   - 断言：`stats.full` 事件数为 0、`Tally` 序列化字节数 ≤ 8192、所有队列 `depth == 0`、`Tree::running()` 任务数回到基线、`Invalidations.items.len() <= 64`；
   - 同时以 `Shelf::open_draft` 写入 10,000 行，验证 `DraftWriter` 仅持固定常数状态、落盘完整可读。

5. `i2_concurrent_view_queries_do_not_stall_live_captions`：
   - 启动完整核心台架（或真实时钟 `App` + 模拟麦/服务）在已有 1,000 条录音的目录上开始录音；
   - 录音进行期间并发发起 20 次 `Core::query`（`Search`、`LibraryMonth`、`LibrarySummary`、`Transcript`）；
   - 断言全部查询成功（或合理 `query.cancelled`）、录音帧持续推进、零 `judge.verdict(Stalled)`、停录后新录音立刻入库可查。

### 2. 扩展日志隐私测试（`core/tests/liveness.rs`）

- 在 `nothing_said_or_secret_reaches_the_log` 中增加：
  - `let secret_note = "SecretNotePhi77";` 经 `Command::Note(Add)` 写入并随后 `Edit`；
  - `Core::query(Query::Search { query: secret_query, limit: 50 })`、`Query::Recording`、`Query::Transcript`、`Query::Compare`、`Query::LibrarySummary`；
  - 触发一次 `Command::Refine`；
  - 把 `secret_note` 加入 `forbidden`，校验整份落盘 `logs/zutalk*.jsonl` 中绝不出现任何原话、译文、标题、笔记、搜索词或密钥，且 `refused == 0`。

## 显式与可观测

- **慢查询与统计守护**：规模压测直接断言 `query.slow == 0`、`stats.full == 0`、`zutalk_log::refused() == 0`、`query.done` 覆盖每次成功查询、`index.open(why = "corrupt")` 覆盖损坏重建。
- **隐私守护**：扩展落盘日志逐行扫描，确保新增笔记/查询/精修路径 0 敏感文本泄露。

## 先写的测试

本件即上述 5 条 `core/tests/view_scale.rs` 测试与 `nothing_said_or_secret_reaches_the_log` 扩展，先写全并在本件内跑绿、记录真实耗时数字。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿。
- 单独跑 `RUSTC_WRAPPER="" cargo test -p zutalk-core --test view_scale -- --nocapture` 记录并把 1,000 条查询耗时、10 小时分块/对比耗时、I8 比值、I3 `Tally` 字节数写入任务报告。

## 不做

- 不修改生产代码架构（除非压测暴露真实的 > 100 ms 慢查询或 `stats.full` 越限 Bug，则当场修好并说明）。
- 不做收尾文档并回（P2-08）。
