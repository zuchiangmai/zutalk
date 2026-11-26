状态：完成

## 改动与文件

完成 P3 界面主流程收尾（`P3-07-close-i8-and-rules`）：补齐界面侧 I8 规模与端到端故障可见集成验收测试、加固 `ui.sh` 并升级 `pending.sh`、回并 P3 实际做法与实测数字到 `docs/PLAN.md` 与 `docs/LESSONS.md`：

1. **I8 规模与故障可见验收测试（`ui/tests/scale_i8.test.tsx`）**：
   - `i8_reader_dom_nodes_stay_bounded_from_1_min_to_4_hours`：对比 1 分钟（16 行 = 1 块）与 4 小时（2,560 行 = 40 块）录音在 600 px 模拟视口下挂载 `ReaderPage`；实测 DOM `<li data-row-index>` 行数均为 `11`（比值 `1.00`，远 ≤ 40），拉取块数分别为 `1` 块与 `2` 块（≤ 3，绝未拉取第 39 块）。
   - `i8_library_dom_nodes_stay_bounded_at_500_recordings`：对比 15 条与 500 条录音在 600 px 视口下挂载 `LibraryView`；实测 DOM `[data-lib-row-index]` 行数均为 `11`（比值 `1.00`，远 ≤ 40）。
   - `raf_coalesces_50_frame_burst_into_one_ui_commit`：在同一帧间隔内突灌 50 次 `Frame`（seq 1..50）与 50 次 `Level`（seq 1..50），通过 React `Profiler` 实测帧回调前 `0` 次提交、下一次 rAF 仅 `1` 次提交，屏幕内容严格显示第 50 帧。
   - `injected_faults_are_visible_and_actionable_across_main_flow`：串联完整 `Shell` 注入 `Status::AudioOnly('captions.no_key')`、`Health::Stalled('silent')`、`Health::Dead('offline')` + 点击「重连」、停止转入 `ResultPage` 呈现 `RefineStatus::Failed('keys.async_refused')` +「再试」/「打开设置」、命令拒绝 `'busy'` 弹出可关闭 `NoticeToast`，以及 `Alt+H` 唤起 `HealthOverlay` 展示 `Water` 与 `Stats`。
2. **规则脚本加固与升级**：
   - `scripts/build/rules/ui.sh`：新增第 5 条禁止 `ui/src/**/*.css` 出现外网 `@import` URL（保证桌面离线启动零阻塞），并新增第 6 条禁止 `ui/src` 出现 `dangerouslySetInnerHTML`（并同步清理 `ui/src/pages/SettingsPage.tsx` 注释措辞）。
   - `scripts/build/rules/pending.sh`：升级正则为 `ignore = "P(1-|2|3)`，同禁 P1/P2/P3 挂起忽略测试。
3. **文档回并**：
   - `docs/PLAN.md`：更新 §5.3（P3 依赖变更）、§6.3（P3 实际架构做法）、§7（I8/I9 界面侧实测数据）与 §8（P3 路线图实际完成与出口状态）。
   - `docs/LESSONS.md`：新增「P3 里学到的（2026-11）」一节，记录 dsh 总线隔离、`useQueries` 视口块订阅、jsdom 下 `virtua` `ResizeObserver` 异步时序与 `offsetParent` 测量、单行说话人编辑 + IME 保护、以及 Node / jsdom 混合测试环境等实践经验。

## P3 出口逐条核对（`docs/build/phases/P3.md` §1）

| 出口 | 状态 | 实测证据 |
|---|---|---|
| E1 框架与 dsh 总线到位 | 通过 | `ui/` 零 Svelte 文件与依赖；`scripts/build/rules/ui.sh` 守卫 `useSyncExternalStore` 与 `@tauri-apps/*` 仅存于 `ui/src/bus/`；`ui/tests/bus.test.ts`（13 条）与 `ui/tests/vitals.test.ts`（4 条）验证 rAF 合并、单调序守卫、128 项活跃订阅 `QueryCache`、`routeStep` 与 `visibilitychange` 归零 |
| E2 设计系统、两栏版面、⌘K 与运行状况浮层到位 | 通过 | `ui/src/ds/` 与 `ui/src/shell/` 全量落地；`ui/tests/shell.test.tsx`（10 条）验证 `confirmStep`、`stateView` 穷举映射、侧栏按月折叠、`TopBar` 白纸/对外色带、`NoticeToast` 撤销、`⌘K` 防抖搜索与 `HealthOverlay` |
| E3 首页、录音中与边录边记主流程走通 | 通过 | `ui/tests/home_live.test.tsx`（6 条）验证首页单一主按钮/配置/导入/0.8 迁移、`LiveCaptions` 逐字显现与 `Stalled`/`Dead` 重连、`LivePage` 5 态/5 柱电平表与 `LiveNotes` 首字锁锚点 + `M` 键标记 |
| E4 停止以后、精修与阅读页走通（含 `virtua` 分块） | 通过 | `ui/tests/result_reader.test.tsx`（8 条）验证 `ResultPage` 精修四态（含 `keys.async_refused`）、`ReaderPage` 4 小时 2,560 行分块虚拟化、`Compare` 字符级高亮、IME 保护行内编辑、说话人/标题编辑、笔记增改删、两步确认销毁音频与软删撤销 |
| E5 全部录音（`virtua`）、设置壳、字幕窗与观众页走通 | 通过 | `ui/tests/library_settings.test.tsx`（4 条）验证 500 条虚拟列表、月轨/热力图/筛选/防抖搜索/撤销删除、设置页安全 SVG 二维码/自有钥匙提交即清/4 档留存/8 语/主题/词表/导出诊断/浮层开关；`ui/tests/captions_viewer.test.tsx`（4 条）+ `ui/tests/room.test.ts`（3 条）验证字幕窗零查询隔离与 `Conversation` 翻转、观众页语言过滤与 AES-256-GCM 解密 |
| E6 **I8** 界面侧成立与故障可见（I9） | 通过 | `ui/tests/scale_i8.test.tsx`（4 条）：阅读页 1 分钟（16 行）vs 4 小时（2,560 行 = 40 块）DOM 行数均 `11`（比值 `1.00`，拉块 `1` / `2`）；资料库 15 vs 500 条 DOM 行数均 `11`（比值 `1.00`）；50 帧突灌合并为 `1` 次提交；全链路 6 类注入故障均在 DOM 可见且带恢复操作 |
| E7 门禁全绿、无 P3 挂起测试、8 语字典零缺漏 | 通过 | 全部 10 个规则脚本通过（含加固 `ui.sh` 与升级 `pending.sh`）；`ui/tests/words.test.ts`（12 条）验证 8 语各 312 键与占位符完全一致；全仓 `scripts/build/check.sh` 一次全绿（707 Rust + 76 UI）；自 P2 收尾（`a37be35`）以来 Rust 侧改动文件数为 `0` |

## 规则违规证明与构建产物实测

1. **`scripts/build/rules/ui.sh` 违规证明**：临时向 `ui/src/ds/tokens.css` 注入 `@import url('https://fonts.googleapis.com/css2?family=Inter');`，实测输出：
   ```text
   ui/src/ds/tokens.css:153:@import url('https://fonts.googleapis.com/css2?family=Inter');
     external @import URLs are forbidden in ui/src CSS (desktop startup must stay offline-safe)
   ui.sh exit=1
   ```
   回滚后 `bash scripts/build/rules/ui.sh` 返回 `exit=0`。
2. **`scripts/build/rules/pending.sh` 违规证明**：临时向 `ui/tests/scale_i8.test.tsx` 注入 `// ignore = "P3-proof"`，实测输出：
   ```text
   ui/tests/scale_i8.test.tsx:506:// ignore = "P3-proof"
     pending P1/P2/P3 ignored tests are not allowed after P1/P2/P3 close
   pending.sh exit=1
   ```
   回滚后 `bash scripts/build/rules/pending.sh` 返回 `exit=0`。
3. **界面检查与测试实测**：
   - `cd ui && npx tsc -p tsconfig.json && npx vite build --emptyOutDir` 零错误通过（构建耗时 `108ms`）：
     - `dist/index.html`：`0.39 kB`（gzip `0.26 kB`）
     - `dist/viewer.html`：`0.41 kB`（gzip `0.28 kB`）
     - `dist/captions.html`：`0.42 kB`（gzip `0.28 kB`）
     - `dist/assets/main-*.css`：`24.19 kB`（gzip `5.85 kB`）
     - `dist/assets/main-*.js`：`438.27 kB`（gzip `131.08 kB`）
   - `cd ui && npm test`：**12 个测试文件、76 条测试全部通过**（`bus` 13、`captions_viewer` 4、`home_live` 6、`library_settings` 4、`result_reader` 8、`reveal` 6、`room` 3、`scale_i8` 4、`shell` 10、`text` 2、`vitals` 4、`words` 12）。
   - 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）全部退出 `0`。
   - 全仓完整门禁 `RUSTC_WRAPPER="" bash scripts/build/check.sh`：**一次全绿**（`rustfmt`、`clippy -D warnings`、全部 10 个规则脚本、`cargo test`：`passed 707, failed 0, ignored 6`、`ui`：`tsc` 零错、`12 files / 76 passed`）。

## 需负责人真机核对与拍板清单

以下事项无法由 jsdom / 自动化台架替代，请负责人在真机与产品层确认：

1. **真机 WebView 视觉与交互走查（macOS / Windows / Linux）**：核对两栏版面、亮/暗主题、白纸=本机 / 色带=对外 / 橙=录音 / 红=撤不回语义、中/泰/日/韩/英文排版、`⌘K` / `M` / `Alt+H` 快捷键手感与中文/日文/韩文 IME 行内编辑体验。
2. **真机 1 小时实录 I1 延迟验收**：在真实 Tauri WebView 录 1 小时并开启独立字幕窗，确认 `ui.frame.gap` 无异常跳升、字幕顺滑上屏。
3. **离线字体包决策**：当前 CSS 使用 `Inter` / `LXGW WenKai` / `Oxanium` 本地与系统回退栈（零外网阻塞）；是否将 `.woff2` 字体文件打进桌面安装包请负责人拍板。
4. **明暗主题跨窗口同步决策**：P3 为保持零 Rust 协议改动，将主题偏好暂存 `localStorage`；是否后续并入 `Setup` / `settings.json` 以便与独立字幕窗同步，可在 P5 字幕窗偏好时一并定夺。
5. **后续阶段边界**：完整扫码核对六位数/新建身份/社区额度与领回（P4）；主持开场/对外色带人数/按住交档案馆/字幕窗场合与穿透点击/新版手机观众页（P5）；知识库主题包与自动标题话题（P6）。
