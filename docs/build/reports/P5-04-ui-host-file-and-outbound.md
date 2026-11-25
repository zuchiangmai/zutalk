状态：完成

## 改了什么

1. **主持路由与按住交城纯状态机（`ui/src/bus/{route,filing,channel,index}.ts`）**：
   - `ui/src/bus/route.ts`：新增 `{ page: 'host' }`、`{ page: 'outbound' }` 与带可选 `hostEvent?: HostEvent` 的 `{ page: 'result'; id: string; hostEvent?: HostEvent }`；新增纯函数 `leavingHostArm(prev, next)`，离开 `'host'` 跳往非 `'live'` 页面时由 `channel.ts` 钩子自动派发 `Command::HostCancel`；录音停止时将当前主持的 `hostEvent` 透传到 `result` 路由。
   - 新增 `ui/src/bus/filing.ts`：具名带单位常量 `HOLD_TO_FILE_MS = 1_400`、`FILE_GRACE_MS = 8_000`、`ESTIMATED_WAV_MB_PER_HOUR = 115`；显式状态机 `FilePhase`（`review`/`holding`/`grace`/`submitting`/`filed`/`failed`/`kept`）与纯转换函数 `filePhaseStep`；纯辅助 `pickNearestEvent` 与 `estimateAudioMb`。
2. **主持开场前、直播对外色带、按住交档案馆与设置 · 对外（纯 props 视图 + 薄总线容器）**：
   - 新增 `ui/src/ds/QrCode.tsx`：用既有 `uqr` 的 `encode()` 输出纯 JSX `<svg><path .../></svg>`（无内联 HTML）。
   - 新增 `ui/src/pages/HostPage.tsx`（`HostView` + `HostPage`，对照 `HostArm.dc.html`）：观众字幕开启时显 `.band` 色带、等待人数、二维码、截断链接、复制与放大弹窗；关闭时切回白纸 `.top`；支持记忆城地址加载日历与自动选中最近活动、点击切换活动、开始主持与取消主持。
   - 更新 `ui/src/shell/TopBar.tsx`、`ui/src/pages/LivePage.tsx`、`ui/src/shell/Sidebar.tsx`（对照 `HostLive.dc.html`）：仅在 `session.outbound` 或开场前开启观众字幕时显示 `.band` 色带；展示实时观看人数、复制链接/二维码弹窗与一键 `StopOutbound`（本机录音不停）；侧栏录音行在对外中展示对外箭头图标与活动名。
   - 新增 `ui/src/pages/FileArchiveCard.tsx`（`FileArchiveView` + `FileArchiveCard`，对照 `HostFile.dc.html`）：1.4 s 指针/键盘按住填充进度 → 8 s 可撤销倒数环（撤销立即取消，8 s 到期才发 `Command::File`），支持活动切换/城地址加载、全程录音开关与体积估算、公开提示横幅，挂载于 `ResultPage.tsx`（主持结束自动带入 `hostEvent` 展开）与 `ReaderPage.tsx`。
   - 新增 `ui/src/pages/OutboundPage.tsx`（`OutboundView` + `OutboundPage`，对照 `Outbound.dc.html`）：展示现场字幕对外状态（及一键停止对外）与 `Query::Outbound` 全部交城记录（已交 / 正在交全程录音 / 未发送），点击跳阅读页；从 `SettingsPage.tsx` 新增「对外」入口行。
   - 更新 `ui/src/pages/HomePage.tsx`（次级「主持一场」入口，未登录时按需唤起 `IdentitySheet`）、`ui/src/shell/Shell.tsx`、`ui/src/ds/{icons.tsx,controls.css}`、`ui/dev/mock.ts` 与全部 8 语 `ui/lang/*.json`（新增 48 个键，占位符全量对齐）。

## 证据

- `bash scripts/build/rules/ui.sh`：通过（零 dsh 越界、零 Svelte 残留、零外网 `@import`、零不安全 HTML 注入）。
- `cd ui && npm run check && npm run build`：通过，`ui/dist/{index,captions,viewer}.html` 全部正常构建。
- `cd ui && npm test`：**14/14 文件、103/103 测试通过**（含 `words.test.ts` 8 语一致性校验与新增 `ui/tests/host_outbound.test.tsx` 全部 6 条测试）：
  - `filePhaseStep_transition_table`
  - `leavingHostArm_triggers_cancel_except_entering_live`
  - `HostView_shows_band_qr_waiting_viewers_and_toggles_quiet_paper`
  - `LiveView_outbound_band_shows_viewers_and_stop_outbound_keeps_recording`
  - `FileArchiveView_requires_1400ms_hold_and_8s_undo_cancels_before_dispatch`
  - `OutboundView_renders_idle_or_live_and_all_filing_states`
- `ui/package-lock.json` 与 `Cargo.lock` 零改动（零新增外部依赖）。

## 遗留 / 需协调人注意

- 字幕窗场合与设置面板、4 场合独立字幕窗与新版手机观众页在下一件任务 `P5-05` 完成。
