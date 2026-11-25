# P3-05 全部录音（virtua 月度虚拟列表）与设置页基础壳（含导出诊断）

## 目标

做完以后：

1. **全部录音页（`ui/src/pages/LibraryPage.tsx`，对照 `Library.dc.html`）**：
   - 顶栏标题「全部录音」+ 总数（来自 `Query::LibrarySummary`）；
   - 搜索栏（带 `⌘K` 提示）：输入关键词经 120 ms 防抖走 `Query::Search { query, limit: SEARCH_PAGE_MAX_HITS }`，展示命中记录与片段，清空后回到月度浏览；
   - 筛选胶囊行：「已交」（`filed_only`）、「现场稿」（`draft_only`）、「有音频」（`audio_only`）以及常用语言筛选（`language`），全部直接传给 `LibraryFilter`；
   - 左侧月份轨道：按 `Query::LibrarySummary` 渲染各月计数列表，选中月份高亮并切换查询 `Query::LibraryMonth { month, tz_offset_min, filter, limit: LIBRARY_PAGE_ROWS, offset }`；下方附当月每日录音活跃热力图（纯函数从已加载行的时间戳聚合每日计数）；
   - 右侧虚拟化列表（`virtua` `VList`）：用虚拟列表渲染当月录音（支持到达底部按 `has_more` 加载下一页 `offset += LIBRARY_PAGE_ROWS`），每行显示时间、标题/摘要、语言、时长、状态徽章（现场稿 / 精修失败 / 已交），点击进入 `reader(id)`，附行内软删除按钮（走 `removeWithUndo`，弹出 8 秒撤销胶囊）。
2. **设置页 P3 基础壳（`ui/src/pages/SettingsPage.tsx`，对照 `Settings.dc.html`）**：
   - **身份摘要区**：已登录显示 DID 与「退出登录」（`Command::SignOut`）；登录中显示扫码二维码（用 `uqr` 的 `renderSVG`，纯数据渲染，注意避免 `dangerouslySetInnerHTML` XSS 风险，用安全的 SVG data URL 或 `uqr` 矩阵渲染 `<svg>` 元件！）与链接；未登录显示「登录」（`Command::SignIn`）；有未保存恢复密钥时显示「复制」（`Command::CopyRotation`）与「已存好」（`Command::RotationSaved`）（完整扫码核对六位数与新建身份留 P4）；
   - **自有 API 钥匙区**（`Soniox`、`OpenRouter`）：显示当前来源徽章（`from.Own` / `from.Granted` / `from.Nowhere`；若 `Granted` 且 `state.left !== null` 显示余额），提供密码输入框 +「保存」（派发 `Command::OwnKey` 后**立即清空输入框与本地 state**，绝不滞留钥匙在界面内存中！）与已有自有钥匙时的「移除」（`key: null`）；
   - **音频留存区**：分段选择器提供设计稿全部 4 档（`一直保留: null`、`30 天: 30`、`7 天: 7`、`转录完就删: 0`），派发 `Command::Setup`；
   - **外观与语言区**：界面语言下拉（跟随系统 + 8 语，派发 `Command::Setup`）；明暗外观分段器（跟随系统 / 浅色 / 深色，经 `bus` 更新 `themeSource`）；
   - **词表基础管理区**：接 `Query::Glossary` 与 `Command::Term`（添加/移除词条，P6 升级为完整知识库主题页）；
   - **诊断与开发可观测区（`PLAN.md` §4.5.2）**：
     - 「导出诊断」按钮（派发 `Command::Diagnose`）+ 说明「不含对话文字与密钥」；
     - 运行状况浮层开关（切换 `overlaySource`，实时查看 `Water` 与 `Stats`）；
   - **更新区**：当 `state.update` 存在且未在录音时显示「更新」主按钮（`Command::Update`）。
3. **接入 `Shell.tsx`** 并补齐 8 语字典新增键。

## 背景

- 设计：`docs/build/phases/P3.md` §3.2、§3.4–§3.5；`docs/ui-2026-11-21/{Library.dc.html,Settings.dc.html}`；`docs/PLAN.md` §4.5.2、§6.3。
- 涉及文件：
  - 新增 `ui/src/pages/LibraryPage.tsx`、`ui/src/pages/SettingsPage.tsx`
  - 更新 `ui/src/shell/Shell.tsx`（接入 `LibraryPage` 与 `SettingsPage`，此时主窗口全部 6 个路由页均已实装）
  - 新增 `ui/tests/library_settings.test.tsx`
  - 更新 `ui/lang/*.json`（8 份文件）

## 做法

### 1. `LibraryPage.tsx`

- 命名常量：
  - `LIBRARY_PAGE_ROWS = 50`（单页条数，与核心 `LIBRARY_PAGE_DEFAULT_ROWS` 一致）
  - `SEARCH_PAGE_MAX_HITS = 100`（全部录音页搜索上限）
  - `LIBRARY_ROW_ESTIMATE_PX = 64`（给 `VList` 的 `itemSize`）
- 纯函数：
  - `buildMonthHeatmap(month: string, rows: LibraryRow[], tzOffsetMin: number): Array<{ day: number | null; level: 0 | 1 | 2 | 3 }>`：按周一为一周起点计算当月补白与每日录音密度等级；
  - `localTzOffsetMin(): number`：`new Date().getTimezoneOffset() * -1`（与 `core/src/view/index.rs` 约定一致：东正西负，如东八区 `+480`）。
- 月份切换或筛选改变时重置分页累积（`offset = 0`），避免旧月残影混入新月。
- 使用 `virtua` `VList` 渲染行表（搜索态同样走 `VList`），保证 500 条录音时 DOM 节点数仅受视口高度约束。

### 2. `SettingsPage.tsx`

- 安全渲染 QR 码：使用 `uqr` 的 `encode(s.login)` 得到布尔矩阵，直接以 React `<svg><rect .../></svg>` 渲染，**不使用 `dangerouslySetInnerHTML`**，既零 XSS 风险又利于测试断言。
- 钥匙表单提交后立刻把受控 input 置为 `''`（原则 6：钥匙不留在界面状态里）。
- 4 档音频留存选项常量 `KEEP_OPTIONS: readonly (number | null)[] = [null, 30, 7, 0]`，补齐此前 `GAP-2026-11-21.md` 指出的旧 Svelte 漏掉 `30 天` 的偏差。
- 导出诊断按钮点击调 `diagnose()`（发 `Command::Diagnose`），核心回 `Notice('diagnose.done')` 自动浮出底部胶囊。

## 显式与可观测

- **纯函数规则**：`buildMonthHeatmap`、`formatMonth`、过滤条件拼装均无副作用。
- **具名常量**：`LIBRARY_PAGE_ROWS = 50`、`SEARCH_PAGE_MAX_HITS = 100`、`LIBRARY_ROW_ESTIMATE_PX = 64`、`KEEP_OPTIONS`。
- **可观测**：设置页提供「导出诊断」（`Command::Diagnose`）与「运行状况浮层」开关，把 `PLAN.md` §4.5.2 的界面自检入口全部落地。
- **隐私**：自有 API 钥匙输入框为 `type="password" autocomplete="off"`，提交即清零。

## 先写的测试

新建 `ui/tests/library_settings.test.tsx`（顶部声明 `// @vitest-environment jsdom`，引入 `./dom`）：

1. **`LibraryPage` 虚拟化测试（I8）**：
   - 注入 500 条月度录音，挂载在 600 px 模拟视口下；
   - 断言 DOM 中挂载的行元素数量远小于 500（≤ 40）。
2. **`LibraryPage` 月份、筛选、搜索与撤销删除测试**：
   - 点击侧轨某个月份触发对应 `Query::LibraryMonth`；
   - 勾选「现场稿」「有音频」更新 `LibraryFilter`；
   - 输入关键词经防抖触发 `Query::Search`，渲染命中片段并点击跳转 `reader`；
   - 点击某行删除触发 `Command::Remove` 与撤销通知。
3. **`buildMonthHeatmap` 纯函数测试**：验证当月 1 号补白与每日频次映射到 0..3。
4. **`SettingsPage` 测试**：
   - 填写 Soniox 钥匙并提交：断言派发 `Command::OwnKey { service: 'soniox', key: '...' }` 且输入框值立即变为 `''`；
   - 点击 4 档留存中的 `30 天` 派发 `Command::Setup`（`keep: 30`）；
   - 切换明暗外观更新 `themeSource` 与 `resolvedThemeSource`；
   - 切换界面语言派发 `Command::Setup`；
   - 点击「导出诊断」派发 `Command::Diagnose`；
   - 切换「运行状况浮层」开关更新 `overlaySource`。

## 验收

- `cd /Users/kant/Zuzalu/zutalk && RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 8 语 `words.test.ts` 与 `ui.sh`）。

## 不做

- 不在本任务实现 P4 完整身份流（核对六位数、新建身份、按天按服务社区额度）或 P5 对外设置子页（`Outbound.dc.html`）或 P6 知识库主题包。
- 不修改任何 Rust 代码。
