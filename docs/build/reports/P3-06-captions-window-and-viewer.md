状态：完成

## 改动与文件

完成独立字幕窗（`CaptionsWindow`）、React 观众页（`ViewerPage`）与全部 8 语字典核齐：

1. **独立字幕窗（`ui/src/captions/CaptionsWindow.tsx`）**：
   - 拆出纯 props `CaptionsWindowView` 与连总线容器 `CaptionsWindow`，挂载在 `ui/captions.html`（`data-window="captions"`）。
   - **严格零查询隔离**：仅订阅 `frameSource`、`stateSource`（取 `setup.mode` 与 `rooms`）、`resolvedThemeSource` 与 `langSource`，不调用 `useQuery`，收到任何 `Invalidate` 事件时 `bus.query` 调用次数恒为 `0`。
   - 对齐 `CaptionsLatest.dc.html`：深森林绿背景（`#012A1F`）、整窗 `data-tauri-drag-region` 拖拽区、有活动对外房间时显示「对外」色带小标、右侧提供版面切换（`Audience` 最新一句 ↔ `Conversation` 面对面 180° 翻转，派发 `Command::Setup`）与关闭按钮（派发 `Command::Captions { show: false }`）。
   - 复用 `LiveCaptions`（与 `LivePage`、`ViewerPage` 共用同一套 `reveal.ts` 逐字显现与每栏 `Slow`/`Stalled`/`Dead` 健康态 + 重连按钮，践行原则 3）。
2. **React 观众页与加密房间源（`ui/src/viewer/ViewerPage.tsx`、`ui/src/viewer/room.ts`）**：
   - `room.ts` 保留全部 AES-256-GCM 解密接口（`roomKey`、`openFrame`、`read`），补充只读 `roomEndSource` 暴露房间结束/错误码（`room.gone`、`room.bad`、`room.viewers`、`offline`），`room.test.ts` 100% 原样通过。
   - `ViewerPage.tsx` 拆出纯 props `ViewerView` 与连房间源容器 `ViewerPage`，挂载在 `ui/viewer.html`（`data-window="viewer"`）：支持多语实时字幕单语过滤胶囊、复用 `LiveCaptions`、`ViewerEnded` 明确结束态（`role="status"`）、以及兼容既有 `/r/rec` 共享录音日志渲染。
3. **8 语字典核齐与测试**：
   - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`：补齐 P3-06 的 5 个新键（全 8 语总计各 312 个键），核查非英语字典无英文占位漏翻、占位符完全一致。
   - `ui/tests/captions_viewer.test.tsx`：新增 4 条测试覆盖 3 栏拖拽字幕窗与版面翻转/关闭/`Dead` 故障可见、字幕窗零查询隔离、观众页语言过滤与 `room.gone` 状态、以及共享录音日志渲染。

## 验证证据

- `cd ui && npm run check && npm test` 全部通过（`tsc` 零报错，三入口 `index.html` / `captions.html` / `viewer.html` 生产构建全部成功）。
- 全部 10 个规则脚本通过（含 `scripts/build/rules/ui.sh`）。
- UI 测试 11 个文件 72 条全部通过，其中：
  - 新增 `ui/tests/captions_viewer.test.tsx`（4 条）：
    - `renders 3-column drag-region captions, switches Audience <-> Conversation, closes, and surfaces Dead health`
    - `never issues any Query calls even when State, Frame, and Invalidate events arrive`
    - `filters multi-language columns to a single language and renders room.gone status`
    - `renders shared recording log when open payload arrives`
  - 既有 `ui/tests/room.test.ts`（3 条）与 `ui/tests/words.test.ts`（12 条，覆盖全部 8 语 312 键）全绿。
- 本任务零 Rust 代码改动。

## 遗留与需负责人确认

- 字幕窗的场合预设、窗口位置记忆、穿透点击与舞台全屏以及新版手机观众页、删除旧单场分享分支均按计划留待 P5。
- 接下来进入 P3-07 完成 I8 规模与端到端故障集成验收、规则加固/升级与文档回并。
