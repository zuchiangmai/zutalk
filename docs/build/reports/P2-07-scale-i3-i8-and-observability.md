状态：完成

## 改动与涉及文件

1. **新增规模压测与架构不变量套件 `core/tests/view_scale.rs`**（5 条高信号测试，整套运行 5.12 s）：
   - `scale_1000_recordings_library_and_fts5_search_under_100ms`：在单事务内构造跨 12 个月（`2025-01`–`2025-12`）的 1,000 条多语种录音（每条含标题、20 行中/泰/英正文与译文、2 条笔记，约 20,000 行正文 + 43,000 条检索文档），经 `ViewEngine::query`（真实走 `Tree::blocking` + `SQLITE_OPEN_READ_ONLY` WAL）实测 `LibrarySummary`、`LibraryMonth`（第 2 页 + 组合过滤）、`LibraryRecent`（50 条）与 6 类 `Search`（中文 4 字 FTS5、中文 2 字短词回退、泰文 SARA AM 异写、英文大小写、笔记、标题），全部 ≤ 100 ms，`query.slow == 0`，`zutalk_log::refused() == 0`。
   - `scale_10_hour_transcript_chunk_and_compare_under_100ms`：构造单场 10 小时录音（`length_ms = 36_000_000`），take 0 与 take 1 各 6,400 行（100 块 × 64 行，每第 10 行做修订）；经 `ViewEngine::query` 查询第 0、50、99 块的 `Transcript` 与 `Compare`，断言每块严格 64 行、修改行标记精确、`DiffOp` 满足 `text_a`/`text_b` 文本重建、全部 ≤ 100 ms 且 `query.slow == 0`。
   - `i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration`：在同一索引中对比 1 分钟录音（12 行 × 2 版）与 4 小时录音（2,560 行 = 40 块 × 2 版）的首块/末块 `Transcript` 与 `Compare`，验证单块行数 ≤ 64、序列化负载有界且与整场时长无关、全部 ≤ 100 ms。
   - `i3_rebuild_queries_and_draft_stay_within_bounds`：经真实 `Shelf` 落盘 100 条签名录音 → 写坏 `.sqlite3` → `Index::open` 自动流式重建并断言 `index.open(mode = "rebuilt", why = "corrupt")` 与 `index.rebuilds == 1`；连续执行 50 次混合查询（含 10 路并发重叠搜索触发取消）+ 200 次 `invalidate`，断言 `stats.full == 0`、`Tally` 序列化 ≤ 8,192 B、所有队列 `depth == 0`、`Tree::running()` 归零、`Invalidations.items.len() <= 64`；再以 `Shelf::open_draft` 写入 10,000 行，验证 `size_of::<DraftWriter>() == 48 B` 且落盘完整可读。
   - `i2_concurrent_view_queries_do_not_stall_live_captions`：在预置 1,000 条录音的目录上启动完整 `Bench` 台架，录音进行期间分 4 轮并发发起 20 次 `Core::query`（`Search`、`LibraryMonth`、`LibrarySummary`、`Transcript`、`Compare`），断言现场字幕帧持续推进（6 → 35 帧）、零 `judge.verdict(Stalled)`、所有查询成功或合理 `query.cancelled`，停录后新录音立即入库（`total: 1000 -> 1001`）。

2. **扩展日志隐私测试 `core/tests/liveness.rs`**（`nothing_said_or_secret_reaches_the_log`）：
   - 增加带哨兵词 `SecretNotePhi77` / `SecretNoteEditPhi78` 的 `Command::Note(Add)` 与 `Command::Note(Edit)`；
   - 增加 `Core::query` 全路径调用（`Search`、`Recording`、`Transcript`、`Compare`、`LibrarySummary`）；
   - 停录后触发一次 `Command::Refine`（验证 `refine.state` 路径）；
   - 断言落盘 `zutalk*.jsonl` 包含 `query.done` / `index.open` / `index.sync` / `refine.state` / `view.invalidate` 等 28 类事件、零原话/译文/标题/笔记/搜索词/密钥泄露、且 `zutalk_log::refused()` 增量为 0。

## 实测数据与证据

### 1. 1,000 条录音规模（预算 ≤ 100 ms，`query.slow = 0`）

| 查询 | 实测耗时 | 结果摘要 |
|---|---:|---|
| `LibrarySummary { tz_offset_min: 480 }` | **1.00 ms** | `total = 1000`，12 个月之和 = 1000 |
| `LibraryMonth`（`2025-06` 第 2 页） | **0.65 ms** | 20 行，`has_more = true` |
| `LibraryMonth`（`2025-06` `draft+audio` 过滤） | **0.79 ms** | 过滤结果全部满足条件 |
| `LibraryRecent { limit: 50, offset: 0 }` | **0.64 ms** | 50 行，按时间倒序 |
| `Search` 中文 4 字 FTS5（`清迈读书`） | **0.96 ms** | 命中 `rec-0017` (`Line`) |
| `Search` 中文 2 字短词回退（`押金`） | **7.37 ms** | 命中 `rec-0017` (`Line`) |
| `Search` 泰文 SARA AM 异写（`น\u{e4d}\u{e49}\u{e32}หนักพิเศษ`） | **1.32 ms** | 命中 `rec-0513` (`Line`) |
| `Search` 英文大小写（`zuzaluquarterly`） | **1.23 ms** | 命中 `rec-0513` (`Translation`) |
| `Search` 笔记（`NoteAnchorBeta`） | **1.36 ms** | 命中 `rec-0988` (`Note`) |
| `Search` 标题（`TitleGamma`） | **1.32 ms** | 命中 `rec-0988` (`Title`) |

### 2. 10 小时正文与对比（6,400 行 × 2 版 = 100 块 × 64 行，预算 ≤ 100 ms，`query.slow = 0`）

| 块号 | `Query::Transcript` | `Query::Compare` | 行数 |
|---:|---:|---:|---:|
| chunk 0 | **10.47 ms** | **11.98 ms** | 64 |
| chunk 50 | **5.44 ms** | **9.92 ms** | 64 |
| chunk 99 | **12.14 ms** | **12.76 ms** | 64 |

### 3. I8（代价与历史无关）

- **单块查询与对比**（1 分钟 12 行 vs 4 小时 2,560 行 = 40 块）：
  - `Transcript`：1m = **5,872 µs**，4h chunk 0 = **11,223 µs**，4h chunk 39 = **11,200 µs**（比值 `1.91`；单块 JSON = **13,573 B**）
  - `Compare`：1m = **7,649 µs**，4h chunk 0 = **9,067 µs**，4h chunk 39 = **5,757 µs**（比值 `0.75`；单块 JSON = **26,045 B**）
- **列表变更推送**（`liveness::i8_what_one_change_sends_does_not_grow_with_the_library`）：
  - `bytes_5 = 441 B`，`bytes_100 = 441 B`，`ratio = 1.00`，`state_pushes = (0, 0)`，`inv_bytes = (60, 60)`

### 4. I3（内存、队列与统计有上限）

- 100 条真实签名录音损坏重建耗时：**94 ms**（`index.open(mode="rebuilt", why="corrupt")`，`index.rebuilds = 1`）
- `Tally` 序列化大小：**390 B**（上限 8,192 B），`stats.full = 0`，全部队列 `depth = 0`，`Tree::running()` 归零
- `Invalidations` 200 次推送后封顶：**64** 条（`KEEP_INVALIDATIONS_EVENTS`）
- `DraftWriter` 结构体固定大小：**48 B**（O(1)）；连续写 10,000 行耗时 **1,244 ms**，读回完整 10,000 行

### 5. I2 与 I9（并发查询不挡现场字幕 + 隐私日志零泄露）

- `i2_concurrent_view_queries_do_not_stall_live_captions`：20 次并发查询（16 成功 + 4 同槽合理取消），直播帧 `6 -> 35`，`Stalled = 0`，停录后库总数 `1000 -> 1001`
- `nothing_said_or_secret_reaches_the_log`：覆盖 28 类日志事件（含 `index.open`、`index.sync`、`query.done`、`refine.state`、`view.invalidate`），零敏感文本泄露，零 `"!"` 拒收

### 6. 门禁校验

- `cargo fmt --all -- --check`：通过
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warning）
- 9 条规则脚本（`bounded`, `dropped`, `events`, `hub`, `layers`, `numbers`, `pending`, `shell`, `spawn`）：全绿
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test view_scale -- --nocapture`：5/5 通过（5.12 s）
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test liveness nothing_said_or_secret_reaches_the_log`：1/1 通过（2.37 s）
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --test liveness i8_what_one_change_sends_does_not_grow_with_the_library`：1/1 通过（2.46 s）
- `cd ui && npm run --silent check && npm test --silent`：0 error / 0 warning，37/37 通过

## 遗留与待交接

- 无遗留阻塞；进入 `P2-08-close` 升级 `pending.sh` 并完成 P2 文档并回与收尾报告。
