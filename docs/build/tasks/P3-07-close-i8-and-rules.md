# P3-07 P3 收尾：I8 规模/故障集成验收、规则加固、门禁升级与文档回并

## 目标

做完以后：

1. **界面侧 I8 规模与故障可见集成测试（`ui/tests/scale_i8.test.tsx`）**：
   - **I8 正文规模对比**：对比 1 分钟（16 行 = 1 块）与 4 小时（2,560 行 = 40 块）录音挂载到 `ReaderPage`，断言两者在 600 px 视口下挂载的 DOM 行节点数均 ≤ 40、拉取块数均为常数级（≤ 3 块），且 4 小时录音的渲染 DOM 数与 1 分钟录音之比受视口封顶而不随总行数线性增长；
   - **I8 资料库规模对比**：对比 15 条与 500 条录音挂载到 `LibraryPage`，断言 500 条录音下实际挂载 DOM 行节点数 ≤ 40；
   - **高频信号 rAF 合并压测**：同一帧内突灌 50 次 `Frame`（seq 1..50）与 50 次 `Level`，断言 `LivePage` 在下一帧回调后仅提交 1 次且屏幕内容严格等于第 50 帧；
   - **端到端故障可见性回归**：通过完整 `Shell` + 测试 `Bus` 串联注入 `Health::Stalled`、`Health::Dead`、`Status::AudioOnly('captions.no_key')`、`RefineStatus::Failed('keys.async_refused')`、命令拒绝 `busy`，逐一断言界面均出现清晰中文/英文文案与可交互恢复动作（重连 / 再试 / 打开设置 / 关闭提示）。
2. **规则脚本加固与升级**：
   - 核查并完善 `scripts/build/rules/ui.sh`（覆盖 dsh `useSyncExternalStore` 边界、无总线外 Tauri IPC、无 Svelte 残留、无 `console.log`、无外网字体 `@import`）；
   - 升级 `scripts/build/rules/pending.sh`，同时禁止 `ignore = "P1-`、`ignore = "P2` 与 `ignore = "P3`，且全仓通过。
3. **文档回并与收尾报告**：
   - 把 P3 实际做法与实测数字并回 `docs/PLAN.md`（§5.3、§6、§7 I8/I9、§8 P3 行）与 `docs/LESSONS.md`（新增「P3 里学到的」一节，记录 React 19 + dsh 总线、rAF 合并、jsdom `virtua` 虚拟化测量等实打实的经验）；
   - 写 `docs/build/reports/P3-07-close.md`：逐条核对 `P3.md` §1 出口 E1–E7、列出测试数与实测指标、并附上「需负责人真机核对清单」（真机 WebView 视觉走查、三系统快捷键、1 小时真机字幕延迟 I1 与字体包决策）。

## 背景

- 设计：`docs/build/phases/P3.md` §1（E1–E7）、§2（I1/I3/I8/I9）、§4；`docs/PLAN.md` §7、§8。
- 涉及文件：
  - 新增 `ui/tests/scale_i8.test.tsx`
  - 检查并完善 `scripts/build/rules/ui.sh`、`scripts/build/rules/pending.sh`
  - 更新 `docs/PLAN.md`、`docs/LESSONS.md`
  - 新增 `docs/build/reports/P3-07-close.md`

## 做法

### 1. 新增 `ui/tests/scale_i8.test.tsx`

- 顶部声明 `// @vitest-environment jsdom`，复用 `ui/tests/dom.ts` 固定 600 px 视口。
- 编写 4 组确定性测试（禁用随机时间、用可控 rAF 队列与测试 `Bus`）：
  1. `i8_reader_dom_nodes_stay_bounded_from_1_min_to_4_hours`：比较 16 行 vs 2,560 行（40 块），断言 DOM 行数 ≤ 40 且 `Transcript` 查询次数 ≤ 3；
  2. `i8_library_dom_nodes_stay_bounded_at_500_recordings`：500 条月度录音，断言 DOM 行数 ≤ 40；
  3. `raf_coalesces_50_frame_burst_into_one_ui_commit`：50 帧突发仅触发 1 次渲染提交且显示第 50 帧文本；
  4. `injected_faults_are_visible_and_actionable_across_main_flow`：覆盖 Live（`Stalled`/`Dead`+重连、`AudioOnly`）、Result（`keys.async_refused`+再试/设置）、全局命令拒绝（`NoticeToast`）与 `HealthOverlay`（`Water`+`Stats`）。

### 2. 加固 `ui.sh` 与升级 `pending.sh`

- 在 `scripts/build/rules/ui.sh` 补充一条检查：禁止 `ui/src/` 下的 `.css` 文件出现 `@import url('https://`（确保桌面应用离线零阻塞启动）。
- 更新 `scripts/build/rules/pending.sh` 的匹配正则为 `ignore = "P(1-|2|3)` 并更新脚本注释。

### 3. 全门禁验证与文档回并

- 运行 `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh`，记录 Rust 通过数、UI 测试文件数与通过数、全部 10 个规则脚本结果与三入口 `vite build` 产物体积。
- 核对 `git diff` 确认 P3 未引入任何无关 Rust 改动、未含任何人名/句柄/邮箱、未留调试遗迹。
- 更新 `docs/PLAN.md`、`docs/LESSONS.md` 并撰写 `docs/build/reports/P3-07-close.md`。

## 显式与可观测

- **出口逐条核验**：在 `P3-07-close.md` 中对 E1–E7、I1/I3/I8/I9 给出精确测试名与实测数字，不留模糊措辞。
- **规则全覆盖**：10 个规则脚本（含 `ui.sh` 与升级后的 `pending.sh`）由 `check.sh` 统一把关。
- **负责人清单诚实列出**：明确区分「自动化门禁已证明」与「真机 WebView / 视觉 / 字体包需负责人实测拍板」。

## 先写的测试

先写 `ui/tests/scale_i8.test.tsx` 的 4 组验收测试并跑绿；若测试暴露任何页面边界问题（如多余块拉取、DOM 泄漏或缺失故障提示），在本任务内一并修干净并记录在报告里。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（707 Rust 测试 + 全部 UI 测试 + `tsc` + 三入口 `vite build` + 全部 10 个规则脚本）。
- `docs/build/reports/P3-07-close.md` 完整记录 E1–E7 证据与负责人核对清单。

## 不做

- 不启动 P4（身份与额度）或 P5（主持与对外）的功能开发。
- 不推送、不打标签、不发布。
