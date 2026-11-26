# P2-08 P2 收尾：出口逐条核对、更新 `pending.sh`、文档并回与负责人清单

## 目标

做完以后：

1. **挂起清零规则升级**（`scripts/build/rules/pending.sh`）：同时禁止 `ignore = "P1-` 与 `ignore = "P2`（允许指向 P3 及以后各期，以及既有 4 条真机手工 `audio.rs` 测试）。
2. **P2 全部出口（E1–E8）逐条核对并留证**：跑全套门禁 `scripts/build/check.sh`（连续 2 次确认确定性与总耗时），收集全部测试数、规则脚本状态、1,000 条 / 10 小时实测耗时、I3 / I8 数据。
3. **文档并回**：把 P2 的实际结构、类型、协议、模块路径、依赖、不变量实测与经验并回：
   - `docs/DESIGN.md`（新增 `view/` 与 `record/{notes,takes}.rs` 模块表项，更新小状态/视图/查询/信号契约与 `§5` 数据层章节，验证模块表路径 100% 存在）；
   - `docs/PLAN.md`（保留原文，在 §2.1/§2.3/§2.4/§2.5、§4.2、§5.1/§5.2/§5.3、§7、§8 标明 P2 已完成与实际做法）；
   - `docs/EVENTS.md`（复核全部事件表与实际字段完全一致，并补充「查询慢 / 索引重建 / 精修失败」排查行）；
   - `docs/LESSONS.md`（新增「P2 里学到的」条目，如 FTS5 `trigram` 短词回退 + 归一化、WAL 单写多读与 `InterruptHandle` 取消、`DraftWriter` O(1) 内存等）；
   - `docs/build/README.md`（更新规则脚本与 `layers.sh` 欠账说明）。
4. **写收尾报告** `docs/build/reports/P2-08-close.md`，附「给负责人的清单」（`P2.md` §4 的待拍板项与真机核对建议）。

## 背景

- 设计与出口：`docs/build/phases/P2.md` §1（E1–E8）、§2、§4；参考 P1 收尾报告 `docs/build/reports/P1-26-close.md` 的格式与严格度。
- 涉及文件：
  - `scripts/build/rules/pending.sh`
  - `docs/DESIGN.md`、`docs/PLAN.md`、`docs/EVENTS.md`、`docs/LESSONS.md`、`docs/build/README.md`
  - 新增 `docs/build/reports/P2-08-close.md`

## 做法

### 1. 升级 `scripts/build/rules/pending.sh`

- 检查正则扩展为同时匹配 `ignore = "P1-` 与 `ignore = "P2`（例如 `grep -rnE 'ignore = "P(1-|2)' core app server zu ui`），非空即报错退出 1。
- 临时注入一条 `#[ignore = "P2-test"]` 证明脚本会拒绝退出 1，随后还原并把证明贴进报告。

### 2. 逐条核对 E1–E8 与门禁确定性

- 核对：
  - **E1**：`core/tests/notes.rs`、`core/tests/takes.rs` 全绿；
  - **E2**：`core/tests/view.rs` 索引重建/损坏恢复/对账/增量同步/中泰英全角与短词搜索全绿；
  - **E3**：`core/tests/view.rs` 64 行分块 + 纯函数 `compare` 全绿；
  - **E4**：`State` 瘦身、`Core::query`、Tauri `async fn query`、`pump.rs` `Progress`/`Invalidate`、`ui` `npm run check && npm test` 全绿；
  - **E5**：`i8_what_one_change_sends_does_not_grow_with_the_library` 与 `i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration` 通过并记录比值；
  - **E6**：`core/tests/view_scale.rs` 1,000 条录音与 10 小时正文全部 ≤ 100 ms、I3 边界与 I2 并发查询通过；
  - **E7**：`scripts/build/rules/layers.sh` 零 P2 欠账、`scripts/build/rules/pending.sh` 零 P1/P2 挂起测试；
  - **E8**：全部 9 个规则脚本全绿、`nothing_said_or_secret_reaches_the_log`（含笔记与搜索词）通过、`every_event_in_the_catalogue_can_be_told_apart` 通过。
- 跑 2 次完整 `RUSTC_WRAPPER="" scripts/build/check.sh`，记录每次通过数与耗时。
- 跑一次 `docs/DESIGN.md` §4 模块表路径存在性 Python 校验（要求 0 缺失）。

### 3. 文档并回与收尾报告

- 按目标第 3 条更新 5 份文档（简体中文，平实直接，不含任何人名/句柄/邮箱）。
- 写 `docs/build/reports/P2-08-close.md`：包含 E1–E8 核对表、规模与 I3/I8 实测数字表、与 `PLAN.md` 字面差异汇总、门禁连跑记录、模块表路径验证输出，以及「给负责人的清单」。

## 显式与可观测

- **规则脚本收口**：9 个规则脚本全绿，其中 `pending.sh` 新增 P2 守护，`layers.sh` 仅余 P5 两行欠账，`events.sh` 守住 P1+P2 全部稳定事件。
- **排障词典完善**：`docs/EVENTS.md` 末尾排查表补充索引损坏重建、慢查询（`query.slow`）、精修失败（`refine.state`）三行排查路径。

## 先写的测试

- 本件的测试验证是 `pending.sh` 违例证明 + `DESIGN.md` §4 模块路径存在性校验 + 2 次全量 `scripts/build/check.sh` 回归。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 连续 2 次全绿。
- `docs/build/reports/P2-08-close.md` 写完，E1–E8 全部打钩且有真实数字与命令输出支撑。

## 不做

- 不修改核心或外壳业务逻辑代码（除非收尾检查发现文档与代码微小不一致）。
- 不启动 P3（React 19 界面重写）。
