状态：完成

# P4-05 设置页身份、按服务每日额度与本机领回备份区

## 改动与文件

1. **重构设置页纯视图与容器（[ui/src/pages/SettingsPage.tsx](../../ui/src/pages/SettingsPage.tsx)）**：
   - 移除 P3 临时内嵌在设置页的 `QrSvg` 与 `uqr` 引用（二维码渲染统一归属 `IdentitySheet.tsx`，原则 3）。
   - 新增可单测纯函数 `quotaPercent(remaining, limit)` 与 `findServiceQuota(quota, service)`。
   - 升级 **身份区**：
     - 已登录：大号头像首字、`@handle`（若有）+ 截断 `did:zu:…`、以及「退出登录」（`SignOut`）；
     - 未登录：「仅本机」标签 + 按需打开 `IdentitySheet` 的「登录」「新建身份」「恢复身份」按钮；
     - 保留 `rotation_unsaved` 提醒行（黄底「还没存」+ 复制 + 已存好）；
     - 新增 **本机领回备份横幅**（`me !== null && local_shelf.adopted && local_shelf.recordings > 0`），复用两步红底原处确认 `InlineConfirm` 发送 `{ do: 'DiscardLocal' }`，录音中自动禁用。
   - 升级 **额度与密钥区**：
     - 社区额度卡片：已登录时展示「每天重置」标签、可选总余量估算、「刷新」按钮（`RefreshQuota`），以及 **出字**（`soniox`）与 **AI**（`openrouter`）两条按服务每日进度条与 `今天还剩 X / Y 次`；当 `limit > 0 && remaining === 0` 时展示 `warn` 进度条与「今日已用完」警示标签；`quota === null` 时给出加载/刷新提示；未登录时展示登录解锁提示并附按需打开 `IdentitySheet('ask')` 的按钮。
     - 自有密钥区：清晰标注与核心 `Chain::lease` 一致的真实规则标签「设置后优先使用自己的密钥」，保留提交即清密码输入与移除按钮。
   - 保持音频留存 4 档、8 语切换、外观切换、词表、诊断导出、运行状况浮层与更新区零回归。
2. **样式与 8 语词典**：
   - 更新 [ui/src/ds/controls.css](../../ui/src/ds/controls.css)：新增 `.bar i.warn`、`.quota-card`、`.quota-grid`、`.quota-row`、`.quota-count` 与 `.adopt-banner` 样式。
   - 同步补齐全部 8 语 [ui/lang/*.json](../../ui/lang/) 15 个新键（`en`、`zh-Hans`、`de`、`es`、`fr`、`ja`、`ko`、`th`）。

## 证据

- 扩展 [ui/tests/library_settings.test.tsx](../../ui/tests/library_settings.test.tsx)（先红后绿，新增 4 项 P4-05 专项测试并同步更新既有设置测试）：
  - `settings_shows_handle_daily_quota_bars_and_refresh`
  - `settings_highlights_exhausted_daily_quota`
  - `settings_shows_local_adoption_banner_and_two_step_discard`
  - `settings_signed_out_offers_on_demand_identity_and_quota_prompt`
- 沙盒内实测命令全部通过（退出码 0）：
  - `cd ui && npm run check && npm test && npm run build`：**13 个测试文件 / 97 项测试全部通过**（含 `words.test.ts` 8 语全键与占位符检查），`tsc` 与 `vite build` 零错误。
  - `bash scripts/build/rules/ui.sh`：通过（0 违规）。
  - `ui/package-lock.json` 与 `Cargo.lock` 零变更。

## 遗留 / 下一步

- 端到端隐私扫描与统计上限总测试、`pending.sh` 升级与 P4 收尾文档留待最后一件任务 `P4-06`。
