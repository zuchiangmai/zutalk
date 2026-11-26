状态：完成

# P2-08 收尾：出口逐条核对、更新 `pending.sh`、文档并回与负责人清单：报告

## 改了什么

1. **挂起清零规则升级（`scripts/build/rules/pending.sh`）**：正则扩展为 `grep -rnE 'ignore = "P(1-|2)' core app server zu ui`，同时禁止 `ignore = "P1-` 与 `ignore = "P2`（允许指向 `P3` 及以后各期，以及既有 4 条真机手工 `audio.rs` 测试）。
2. **文档并回（5 份）**：
   - `docs/DESIGN.md`：§2 增加 L3 `view` 查询路；§3 更新 `状态、信号与提示` 并新增 `视图与投影索引`、`笔记与多版本精修`；§4 模块表补入 `record/notes.rs`、`record/takes.rs` 与 `view/` 全部 4 个文件（共 63 处路径，0 缺失）；§5.1 更新 `DraftWriter` O(1) 写手；§5.14 同步 72 个事件与新增排查路径；新增 §5.16「数据层、投影索引与视图查询（P2）」；§9 更新第 12 条（1,000 条与 10 小时规模已证）。
   - `docs/PLAN.md`：保留全部原文，在 §2.1、§2.3、§2.4、§2.5、§2.7、§4.2、§5.3、§7（I2/I3/I8/I9）与 §8（P2 一行）标明 P2 完成状态与实测数字。
   - `docs/EVENTS.md`：核对全表 72 个事件与 `events.sh` / 代码双向一致，补充 `refine.state` 的 `rec` 字段说明，并在末尾「出了事先看什么」补充 **索引损坏或重建**、**慢查询或被取消**、**精修失败或停在草稿** 三行排查路径。
   - `docs/LESSONS.md`：新增「P2 里学到的（2026-11）」五条硬经验（FTS5 `trigram` 1–2 字回退与归一化、WAL 单写多读 + `InterruptHandle` 取消与 `IndexSync` 回执、`DraftWriter` O(1) 内存、`PumpState` 合并感知溢出判定、固定上限统计表硬预算）。
   - `docs/build/README.md`：更新 `pending.sh` 守卫范围（同禁 P1/P2）及 `layers.sh` 欠账状态（P2 两项已清零，仅余 P5 两项）。

文件：`scripts/build/rules/pending.sh`、`docs/DESIGN.md`、`docs/PLAN.md`、`docs/EVENTS.md`、`docs/LESSONS.md`、`docs/build/README.md`、本报告。

---

## 1. 挂起清零与 `pending.sh` 证明

`grep -rnE 'ignore = "P(1-|2)' core app server zu ui` 返回空（仓库中不再有任何 P1 或 P2 挂起测试，仅留 4 条真机手工 `core/tests/audio.rs` 测试）。

`pending.sh` 临时违例证明（试完已还原）：

```text
=== pending.sh PROOF ===
core/tests/view_scale.rs:1068:// #[ignore = "P2-test"]
  pending P1/P2 ignored tests are not allowed after P1/P2 close
exit=1
pending.sh OK after revert
```

---

## 2. 出口逐条核对（E1–E8）

P2 提交序列：`7758eeb`（`rusqlite` 依赖）→ `9c91d3a`（P2-01）→ `4a62220`（P2-02）→ `8756a96`（P2-03）→ `746eaa9`（P2-04）→ `d035c3c`（P2-05）→ `affd17f`（P2-06）→ `e374c2e`（P2-07）→ 本件（P2-08）。

| # | 出口 | 成立 | 证据（测试、规则脚本、数字） |
|---|---|---|---|
| **E1** | 事实扩展到位 | ✔ | `core/tests/notes.rs`（3/3）与 `core/tests/takes.rs`（含 `RefineStatus` 转换表、多版本读取、`Command::Note` 边录边记与旧单串兼容、`Command::Refine` 手动触发/重试/再精修、`Journal.failed` 含 `keys.async_refused` 持久化）全绿 |
| **E2** | SQLite 索引可丢弃、可增量、搜得到 | ✔ | `core/tests/view.rs`（10/10）全绿：删库/写坏/换版本/换身份自动重建且一行不少、按 `(size, mtime_ns)` 快速对账、单写增量同步、标题/正文/译文/笔记全可搜、中/泰（含 SARA AM 异写）/英/全角与 1–2 字短词查询全命中 |
| **E3** | 分块正文与纯函数对比 | ✔ | `core/tests/view.rs` 与 `core/tests/view_scale.rs::scale_10_hour_transcript_chunk_and_compare_under_100ms` 全绿：`Transcript` 固定 64 行一块带 `total_lines`/`total_chunks`；`Compare` 按时间重叠对齐并产出满足文本重建的有界 `DiffOp`；10 小时（6,400 行 × 2 版 = 100 块）任意块与对比耗时 `5.44–12.76 ms` ≤ 100 ms |
| **E4** | `State` 瘦身 + `Query` / `Invalidate` / `Progress` 全线打通 | ✔ | `State` 瘦身为 13 个定界字段，移除 `library`/`open`/`hits`/`terms`/`city`/`writing`/`filing`/`moving` 及旧命令 `Open`/`Close`/`Search`；`Core::query` 与 Tauri `async fn query` 走 L3 `Tree::blocking` 且可取消；`Outlet.{progress, invalidated}` 经 `app/src/pump.rs`（`app/tests/pump.rs` 11/11）转发；Svelte 最小适配后 `ui` `svelte-check` 0 error / 0 warning、`37/37` 测试通过 |
| **E5** | **I8** 成立 | ✔ | `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 已转正通过：`bytes_5 = 441 B`、`bytes_100 = 441 B`、`ratio = 1.00`、`state_pushes = (0, 0)`、`inv_bytes = (60, 60)`；`core/tests/view_scale.rs::i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration` 通过（1 分钟 vs 4 小时 64 行单块 JSON 封顶 `13,573 B` / `26,045 B`，耗时均 ≤ 12 ms） |
| **E6** | **I3** 成立与千条规模达标 | ✔ | `core/tests/view_scale.rs`（5/5，整套 5.12 s）全绿：1,000 条多语种录音按月汇总/分页/最近/6 类搜索耗时 `0.64–7.37 ms` ≤ 100 ms；`DraftWriter` 结构体 `48 B`（O(1)）连写 10,000 行完好读回；100 条损坏流式重建 `94 ms`；压测全程 `stats.full = 0`、`Tally = 390 B` ≤ 8,192 B、队列深度与 `Tree::running()` 归零、`Invalidations` 封顶 64 条；I2 录音期间并发 20 次混合查询直播帧持续推进（6 → 35）、0 `Stalled` |
| **E7** | `layers.sh` P2 欠账清零 & 无挂起 P2 测试 | ✔ | `scripts/build/rules/layers.sh` 已删除 `record/retain.rs` 与 `record/migrate.rs` 两行（仅余 P5 两项 `out/city.rs`、`out/room.rs`）并守着新 `view` 层；`scripts/build/rules/pending.sh` 升级同禁 P1/P2 并通过（0 匹配） |
| **E8** | 显式与可观测（I9）持续成立 | ✔ | 全部 9 个规则脚本全绿；6 条新事件（合计 72 条）全部入 `docs/EVENTS.md` 且 `every_event_in_the_catalogue_can_be_told_apart` 通过；`nothing_said_or_secret_reaches_the_log` 扩展覆盖笔记、搜索词、查询与精修（28 类事件、0 泄露、0 拒收）；`RefineStatus` 与 `IndexHealth` 均有转换表测试 |

---

## 3. 规模压测与 I3 / I8 / I2 实测汇总

来源：`RUSTC_WRAPPER="" cargo test -p zutalk-core --test view_scale -- --nocapture` 与 `liveness`（提交 `e374c2e`）。

### 3.1 1,000 条录音规模（跨 12 个月、~20,000 行正文 + ~43,000 条检索文档，预算 ≤ 100 ms）

| 查询 | 实测耗时 | 慢查询事件 |
|---|---:|---:|
| `LibrarySummary { tz_offset_min: 480 }` | **1.00 ms** | 0 |
| `LibraryMonth`（第 2 页） | **0.65 ms** | 0 |
| `LibraryMonth`（`draft + audio` 过滤） | **0.79 ms** | 0 |
| `LibraryRecent { limit: 50, offset: 0 }` | **0.64 ms** | 0 |
| `Search` 中文 4 字 FTS5（`清迈读书`） | **0.96 ms** | 0 |
| `Search` 中文 2 字短词回退（`押金`） | **7.37 ms** | 0 |
| `Search` 泰文 SARA AM 异写 | **1.32 ms** | 0 |
| `Search` 英文大小写（`zuzaluquarterly`） | **1.23 ms** | 0 |
| `Search` 笔记（`NoteAnchorBeta`） | **1.36 ms** | 0 |
| `Search` 标题（`TitleGamma`） | **1.32 ms** | 0 |

### 3.2 10 小时正文与对比（6,400 行 × 2 版 = 100 块 × 64 行，预算 ≤ 100 ms）

| 块号 | `Query::Transcript` | `Query::Compare` | 行数 |
|---:|---:|---:|---:|
| chunk 0 | **10.47 ms** | **11.98 ms** | 64 |
| chunk 50 | **5.44 ms** | **9.92 ms** | 64 |
| chunk 99 | **12.14 ms** | **12.76 ms** | 64 |

### 3.3 I8、I3、I2 实测指标

| 维度 | 实测指标 | 结论 |
|---|---|---|
| I8 列表变更推送（5 条 vs 100 条） | `State = 441 B / 441 B`（比值 **1.00**），`Invalidate = 60 B / 60 B` | ✔ |
| I8 分块正文（1 分钟 vs 4 小时 2,560 行） | `tr_1m = 5,872 µs`，`tr_4h_c0 = 11,223 µs`，`tr_4h_c39 = 11,200 µs`（比值 **1.91**；单块 **13,573 B**） | ✔ |
| I8 分块对比（1 分钟 vs 4 小时 2,560 行） | `cmp_1m = 7,649 µs`，`cmp_4h_c0 = 9,067 µs`，`cmp_4h_c39 = 5,757 µs`（比值 **0.75**；单块 **26,045 B**） | ✔ |
| I3 损坏重建与边界 | 100 条损坏重建 **94 ms**；`Tally = 390 B`（≤ 8,192 B）；`stats.full = 0`；`DraftWriter = 48 B`；10,000 行写 **1,244 ms**；`Invalidations = 64` | ✔ |
| I2 直播并发 20 次混合查询 | 16 成功 + 4 同槽合理取消；直播帧 `6 -> 35`；`Stalled = 0`；停录后总数 `1000 -> 1001` | ✔ |

---

## 4. 与 `PLAN.md` 字面不同之处汇总

1. **小状态类型名沿用 `State`（未更名 `ShellState`）**：P2 彻底移除了全部无界字段，仅保留 13 个定界字段；为减少 P2 对临时 Svelte 界面的机械改名噪音，类型名保留 `State`，P3 React 重写时再按需调整。
2. **`Progress` 信号暂设 3 槽（`refine` / `filing` / `moving`）**：`PLAN.md` §2.3 里的「生成标题」属于 P6（`ai/titles.rs`），届时直接加一槽即可。
3. **知识库视图 P2 先提供 `Query::Glossary`**：`talk/topic` 主题包属于 P6，P2 先把既有 `terms` 移出 `State` 并通过 `Query::Glossary` 暴露。
4. **短词搜索回退**：SQLite FTS5 `trigram` 仅对 ≥ 3 字符建索引，P2 对 1–2 字符在 `search_docs.norm_text` 上做归一化 `instr` 扫描（实测 1,000 条库 7.37 ms），不引入外部分词依赖。
5. **1–2 行细节**：`Event::Invalidate` 合并环溢出判断要求同时满足 `items.len() >= 64` 与版本号断层，避免正常同键合并误触发粗粒度全量失效；后台写事实后通过 `desk` 的 `Job::IndexSync` + oneshot 提交回执保证先落 SQLite 再发 `Invalidate`。

---

## 5. `docs/DESIGN.md` §4 模块表路径存在性验证

检查命令与输出（共 63 个路径，全部存在，0 缺失）：

```text
$ python3 -c '
import os, re, sys
txt = open("docs/DESIGN.md").read()
m = re.search(r"## 4\. 模块(.*?)## 5\.", txt, re.S)
paths = []
for line in m.group(1).splitlines():
    if not line.startswith("| `"): continue
    cells = line.split("|")
    mod = cells[1].strip().strip("`")
    for p in re.findall(r"`([^`]+)`", cells[2]):
        paths.append(p if mod in ("log", "shell", "ui", "server") else f"core/src/{p}")
missing = [p for p in paths if not os.path.exists(p)]
print(f"checked={len(paths)} missing={len(missing)}")
sys.exit(1 if missing else 0)
'
checked=63 missing=0
```

---

## 6. 门禁与确定性连跑记录

### 6.1 沙箱内已验

- `cargo fmt --all -- --check`：通过
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warning）
- 全部 9 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`）：全绿
- `core/tests/view_scale.rs`：5/5 通过（5.12 s）
- `core/tests/liveness.rs`：`every_event_in_the_catalogue_can_be_told_apart`（1/1）、`nothing_said_or_secret_reaches_the_log`（1/1）、`i8_what_one_change_sends_does_not_grow_with_the_library`（1/1）全部通过
- `ui`：`npm run --silent check`（0 errors, 0 warnings）+ `npm test --silent`（6 文件 37/37 通过）

### 6.2 沙箱外连续 2 次完整 `scripts/build/check.sh`

- Run 1：全绿（全部 9 个规则脚本、`cargo test`：`passed 706, failed 0, ignored 6`、`ui`：`0 errors, 0 warnings`、`37 passed`），总耗时 `265 s`。
- Run 2：全绿（全部 9 个规则脚本、`cargo test`：`passed 706, failed 0, ignored 6`、`ui`：`0 errors, 0 warnings`、`37 passed`），总耗时 `177 s`。
- 确定性结论：2 次连续运行 0 失败、通过数完全一致（`706 Rust + 37 UI`，剩余 6 条 `ignored` 全为真机硬件/真实 Soniox API 手工测试，0 条阶段挂起测试）。
- 沙箱外收口顺手补齐 3 处测试适配：`core/tests/hub/mod.rs::App::wait_library` 等待初始 `State`（`Boot::Ready`）后再返；`core/tests/app.rs::every_command_leaves_its_kind_its_answer_and_nothing_it_carried` 的 `commands.done` 计数随 `Command::Search` 移除同步改为 3；`core/tests/liveness.rs` 的 `i2_background_work` 断言 `query.done(view="search", code="ok")` 与 `five_minute_recording` 补足 20 条真实命令。

---

## 给负责人的清单（`P2.md` §4）

- [ ] **1.「标记」作为锚定空笔记**（`DECISIONS.md` 第 2 条）：P2 照临时决定把标记落成 `talk/note`（`text: ""`、`anchor: Some(line_key)`），不另设记录类型；若决定另设类型，仅改 `core/src/record/notes.rs` 与 `core/src/view/index.rs`。
- [ ] **2. 额度钥匙被异步转写拒绝时的做法**（`DECISIONS.md` 第 4 条）：P2 照临时决定把失败落盘为 `Journal.failed = "keys.async_refused"` 并停留在现场稿，待用户填自有钥匙后通过 `Command::Refine` 重试；待与 Soniox/城定夺后在 P6 接新通道。
- [ ] **3. 新增依赖 `rusqlite`（`bundled`）**：照 `PLAN.md` §5.3 引入（`rusqlite = { version = "0.37", features = ["bundled"] }`），打包体积增约 ~1 MB（SQLite 静态库），跨 macOS / Windows / Linux 免系统版本差异。
- [ ] **4. P2 临时数字待确认**：
  - `TRANSCRIPT_CHUNK_LINES = 64`（`core/src/view/transcript.rs`）
  - `DIFF_LCS_MAX_BYTES = 1_024`（`core/src/view/transcript.rs`）
  - `LIBRARY_PAGE_DEFAULT_ROWS = 50`、`LIBRARY_PAGE_MAX_ROWS = 200`（`core/src/view/index.rs`）
  - `SEARCH_DEFAULT_HITS = 100`、`SEARCH_MAX_HITS = 200`、`SEARCH_SNIPPET_MAX_BYTES = 240`（`core/src/view/index.rs`）
  - `INDEX_BUSY_TIMEOUT_MS = 200`（`core/src/view/index.rs`）
  - `QUERY_BUDGET_MS = 100`、`QUERY_CONCURRENT_MAX_JOBS = 8`、`KEEP_INVALIDATIONS_EVENTS = 64`（`core/src/view/query.rs`）
- [ ] **5. 1–2 字短词搜索策略**：FTS5 `trigram` 原生只索引 ≥ 3 字符；P2 对 1–2 字符走 `search_docs.norm_text` 归一化子串扫描（1,000 条录音实测 7.37 ms ≤ 100 ms），不引外部分词库，请确认是否满意。
- [ ] **6. 真机核对建议（可选，与 P1 E9 一并测）**：在真机录完几场后，手动删除 `<数据目录>/index/*.sqlite3` 或填入垃圾字节再重启应用，确认资料库一行不少、日志里出现 `index.open(mode="rebuilt", why="missing"|"corrupt")`；并用中文 2 字/4 字、泰文、英文搜标题/正文/笔记确认响应顺滑。
