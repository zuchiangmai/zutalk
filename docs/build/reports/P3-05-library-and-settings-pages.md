状态：完成

## 改动与文件

完成全部录音页（`LibraryPage`，月度轨道 + 热力图 + 筛选 + 防抖搜索 + `virtua` 虚拟列表）与设置页 P3 基础壳（`SettingsPage`，安全 SVG 二维码 + 自有钥匙提交即清 + 4 档留存 + 8 语/明暗外观 + 词表 + 一键导出诊断 + 运行状况浮层开关 + 更新），并全部接入 `Shell`：

1. **全部录音页（`ui/src/pages/LibraryPage.tsx`）**：
   - 具名常量：`LIBRARY_PAGE_ROWS = 50`、`SEARCH_PAGE_MAX_HITS = 100`、`LIBRARY_ROW_ESTIMATE_PX = 64`、`LIBRARY_SEARCH_DEBOUNCE_MS = 120`。
   - 纯函数：`localTzOffsetMin()`（东正西负，与核心 `view/index.rs` 一致）与 `buildMonthHeatmap(month, rows, tzOffsetMin)`（周一为起点补白、每日频次夹紧在 `0..3`）。
   - 拆出纯 props `LibraryView` 与连总线容器 `LibraryPage`：左侧月份轨道（`Query::LibrarySummary`）+ 每日活跃热力图；顶部 120 ms 防抖搜索（`Query::Search`）与筛选胶囊（`filed_only` / `draft_only` / `audio_only` / 常用语言）；右侧 `virtua` `VList` 虚拟化月度行表与搜索命中表（支持 `has_more` 分页续载与月/筛选切换重置 `offset = 0`），行内软删除走 `removeWithUndo` 弹 8 秒撤销胶囊。
2. **设置页基础壳（`ui/src/pages/SettingsPage.tsx`）**：
   - 具名常量 `KEEP_OPTIONS = [null, 30, 7, 0]`（补齐 `30 天` 选项，消除 `GAP-2026-11-21.md` 旧版缺档）。
   - 拆出纯 props `SettingsView` 与连总线容器 `SettingsPage`：
     - 身份摘要（已登录 DID + 退出登录、登录中用 `uqr` `encode()` 纯 React `<svg><rect/></svg>` 渲染二维码，**零 `dangerouslySetInnerHTML`**、未登录本地态与登录按钮、恢复密钥复制与已存好）；
     - 自有 API 钥匙（Soniox / OpenRouter：`type="password" autoComplete="off"`，提交 `Command::OwnKey` 后立即将受控输入清空为 `''`，已有自有钥匙支持移除）；
     - 4 档音频留存（`Command::Setup`）；8 语下拉（跟随系统 + 8 语）与浅/深/跟随系统外观切换（更新 `themeSource` / `resolvedThemeSource`）；
     - 词表管理（`Query::Glossary` + `Command::Term`）；
     - 诊断与可观测：一键「导出诊断」（`Command::Diagnose`）+ 不含对话文字与密钥提示、运行状况浮层开关（更新 `overlaySource`）；
     - 更新区（非录音态显示 `Command::Update` 主按钮）。
3. **外壳接入、测试与 8 语字典**：
   - `ui/src/shell/Shell.tsx`：接入 `LibraryPage` 与 `SettingsPage`，主窗口 6 个路由页全部实装完毕。
   - `ui/tests/library_settings.test.tsx`：新增 4 组覆盖热力图纯函数、500 条录音 `virtua` 虚拟化（DOM ≤ 40）、月度切换/筛选/防抖搜索/撤销删除、以及设置页钥匙提交即清/30 天留存/外观/语言/诊断/浮层开关/SVG 二维码的测试。
   - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`：8 份字典同步补齐 15 个新键。

## 验证证据

- `cd ui && npm run check && npm test` 全部通过（`tsc` 零报错，三入口 `vite build` 成功）。
- 全部 10 个规则脚本通过（含 `scripts/build/rules/ui.sh` dsh 边界检查）。
- UI 测试 10 个文件 68 条全部通过，其中新增 `ui/tests/library_settings.test.tsx`（4 条）：
  - `pads month start to Monday and clamps daily counts to 0..3`
  - `renders bounded DOM rows (<= 40) when given 500 monthly recordings`
  - `queries selected month, applies filters, debounces Search, opens reader, and removes with undo`
  - `submits and clears OwnKey immediately, sets 30-day retention, switches theme/language, exports diagnostics, toggles overlay, and renders QR SVG`
- 本任务零 Rust 代码改动。

## 遗留与需负责人确认

- 完整扫码核对六位数、新建身份与按天按服务社区额度留 P4；对外设置子页留 P5；知识库主题包留 P6。
- 独立字幕窗与 React 观众页将在 P3-06 完成。
