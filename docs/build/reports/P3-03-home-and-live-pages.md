状态：完成

## 改动与文件

完成首页、可复用多栏逐字显现字幕组件与录音中（含边录边记与 `M` 键标记）主流程，接入 `Shell` 并同步全部 8 语字典：

1. **首页（`ui/src/pages/HomePage.tsx`）**：
   - 拆出纯 props 展示组件 `HomeView` 与连总线容器 `HomePage`。
   - 遵循「一屏一个主按钮」：中央圆形「开始录音」按钮（带涟漪动效，派发 `Command::Start`；已有录音时禁用并给出「回到正在进行的录音」快捷入口）。
   - 下一场配置网格：说的语言与字幕语言增删胶囊（遵守 `MAX_CAPTION_COLUMNS = 4` 上限提示）、声源三选（`Mic`/`System`/`Both`）、非系统声源时的麦克风设备下拉与留存策略摘要入口。
   - 底部次级「导入」入口（经 `bus/act.ts` 的 `pick()` 选文件并派发 `Command::Import`）；0.8 迁移进度条（`Progress.move_old`）与未搬条目失败原因码卡片（`State.unmoved`）。
2. **可复用多栏逐字显现字幕（`ui/src/pages/LiveCaptions.tsx`）**：
   - 使用 `ui/src/bus/reveal.ts`（`reveal`/`upTo`/`carry`）按语言语速逐字显现并尊重 `prefers-reduced-motion: reduce`，支持 `Reading`（6 行）、`Audience`（1 行）、`Conversation`（2 行 + 首栏 180° 翻转）三种版面。
   - 每栏标题旁按 `healthView` 显示健康度（`Live` / `Slow` / `Stalled · 原因` / `Dead · 原因`），`Dead` 栏提供「重连」按钮（`Command::Reconnect`）。
3. **录音中与边录边记（`ui/src/pages/LivePage.tsx`）**：
   - 拆出纯 props `LiveView` 与容器 `LivePage`，定义具名常量 `CLOCK_TICK_MS = 250`、`LEVEL_BARS_COUNT = 5`。
   - 纯函数 `levelBarHeights(level, paused)` 由真实 `Signal::Level`（`rms`/`peak`）驱动 5 柱电平表，暂停时归静；胶囊展示 5 种 `Status` 状态句、暂停/继续、标记（`M`）与单一主按钮「停止」（停后直达 `result(id)`）。
   - 右侧「边录边记」栏（顶部标注「不对外」）：订阅 `Query::Recording` 展示当前场笔记并支持删除；纯函数 `resolveLiveAnchor(frame, elapsedMs)` 在输入框**键入第一个字时锁定当下 `at_ms` 与字幕引文**（清空释放），按 Enter 派发 `Command::Note(Add)`；焦点不在输入框时按 `M` 或点击「标记」按钮立即写入 `text: ""` 的空标记笔记。
4. **外壳接入、测试与 8 语字典**：
   - `ui/src/shell/Shell.tsx`：接入 `HomePage` 与 `LivePage`（录音页复用同一 `TopBar` 承载笔记与字幕窗开关）。
   - `ui/tests/home_live.test.tsx`：新增 6 组覆盖首页配置/导入/0.8 迁移、多栏字幕与 `Stalled`/`Dead` 重连、`levelBarHeights` & `resolveLiveAnchor`、5 种录音状态、首字锁锚点与 `M` 键标记的 DOM/纯函数测试。
   - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`：8 份字典同步补齐 7 个新键。

## 验证证据

- 全部 10 个规则脚本通过（含 `scripts/build/rules/ui.sh` dsh 边界检查）。
- `cd ui && npm run check && npm test && npm run build` 全部通过（`tsc` 零报错，三入口 `vite build` 成功）。
- UI 测试 8 个文件 56 条全部通过：
  - 新增 `ui/tests/home_live.test.tsx`（6 条）：`HomeView` 启录/改语言/切声源/选设备/导入/0.8 迁移、`LiveCaptions` 双语 + `Stalled`/`Dead` 故障可见与重连、`levelBarHeights` 静音/强信号/暂停、`resolveLiveAnchor`、`LiveView` 5 态与暂停/继续、`LiveNotes` 首字锁 `at_ms`（推进 10 秒仍保持首字时刻）+ 输入框内输 `m` 不误触发 + 全局 `M` 键与「标记」按钮 + 「停止」。
  - `ui/tests/words.test.ts`（12 条）验证 8 语键集与占位符完全一致。
- 本任务零 Rust 改动；按批次要求保持 P3-03 工作区未提交，交由协调者跑完整门禁并提交。

## 遗留与需负责人确认

- 停止后与精修页（`ResultPage`）、虚拟化阅读页（`ReaderPage`）、全部录音（`LibraryPage`）、设置壳（`SettingsPage`）与独立字幕窗/观众页按计划在 P3-04 ～ P3-06 依次补齐。
