# P4-05 设置页身份、按服务每日额度与本机领回备份区

## 目标

做完以后：

1. **设置页身份区完整对齐 `Settings.dc.html`**：
   - 已登录时展示头像首字、`@handle`（若有）与截断 `did:zu:…`、以及「退出登录」（`SignOut`）；
   - 未登录时展示「只在本机」标签，并提供「扫码登录」「新建身份」「恢复身份」入口按钮（分别以 `'login'` / `'create'` / `'recover'` 打开按需 `IdentitySheet`，替代原 P3 在设置页内嵌粗糙二维码块的做法）；
   - 保留 `rotation_unsaved` 提醒行（黄底「还没存」标签 + 「复制」 + 「已存好」）；
   - 新增 **本机领回备份横幅**（当 `state.me !== null && state.local_shelf.adopted && state.local_shelf.recordings > 0`）：清晰说明已从本机身份领回 `N` 场录音、本机备份仍保留以便撤销，并提供红底两步原处确认 `InlineConfirm` 按钮「清除本机备份」发送 `{ do: 'DiscardLocal' }`。
2. **设置页社区额度卡片（按服务、按天，`Settings.dc.html`）**：
   - 已登录时展示「社区额度」卡片：
     - 「每天重置」提示与「刷新」按钮（发送 `{ do: 'RefreshQuota' }`）；
     - 两条独立进度条行：**出字**（`soniox`）与 **AI**（`openrouter`），各自显示 `今天还剩 X / Y 次` 与剩余比例进度条；
     - 当某服务 `limit > 0 && remaining === 0` 时以 `warn` 色明确标出「今日已用完」；当 `state.quota === null` 时显示加载/暂不可用状态并支持点击刷新（若有 `state.left !== null` 可附带显示剩余时间估算，不丢信息）；
   - 未登录时展示精简提示卡片（说明社区额度需要登录解锁），附单一「登录」按钮打开 `IdentitySheet('ask')`。
3. **自有密钥与社区额度优先级说明清晰如实**：
   - 明确标注各服务当前钥匙来源（`From`：`Own` / `Granted` / `Nowhere`）与真实优先级说明（填入自有密钥后优先使用自有密钥，清掉后回到社区额度，与 `core/src/id/keys.rs::Chain::lease` 一致，不写误导文案）；
   - 保持提交自有密钥即刻清空密码输入框与「移除」能力。
4. **保持全部既有设置区零回归**（音频留存 4 档、8 语切换、明暗外观、词表管理、导出诊断、运行状况浮层开关、应用更新）且全部 8 语 `ui/lang/*.json` 同步绿灯。

## 背景

- 设计：`docs/build/phases/P4.md` §3.6；`docs/PLAN.md` §2.7、§3；`docs/ui-2026-11-21/Settings.dc.html`、`zt.css`。
- 涉及文件：
  - `ui/src/pages/SettingsPage.tsx`
  - `ui/src/ds/controls.css`（额度卡片栅格与进度条样式）
  - `ui/lang/*.json`（8 语）
  - `ui/tests/library_settings.test.tsx`

## 做法

### 1. 纯值辅助函数与 `SettingsView` 重构（`ui/src/pages/SettingsPage.tsx`）

- 移除 P3 临时内嵌在 `SettingsPage.tsx` 的 `QrSvg` 块（二维码统一归 `IdentitySheet.tsx`，原则 3「同类只有一个家」）。
- 新增纯函数（无副作用，可直接单测）：
  - `quotaPercent(remaining: number, limit: number): number`：`limit <= 0 ? 0 : Math.min(100, Math.max(0, Math.round((remaining / limit) * 100)))`；
  - `findServiceQuota(quota: Quota | null, service: 'soniox' | 'openrouter'): ServiceQuota | null`。
- 扩展 `SettingsViewProps`（保持纯 props 零总线依赖）：
  - `onOpenIdentity: (step: 'ask' | 'login' | 'create' | 'recover') => void`（取代裸 `onSignIn`）；
  - `onRefreshQuota: () => void`；
  - `onDiscardLocal: () => void`。
- **身份区**：
  - 已登录：头像首字（`handle` 优先，其次 `me`）、主行显示 `handle ? '@' + handle : shortDid(me)`、副行等宽显示 `shortDid(me)`（当有 `handle` 时）、右侧「退出登录」；
  - 未登录：「只在本机」标签 + 「登录」「新建身份」「恢复身份」按钮；
  - `state.rotation_unsaved`：黄底标签 + 「复制」+「已存好」；
  - `state.me && state.local_shelf.adopted && state.local_shelf.recordings > 0`：展示 `.card` 领回状态行（`t('settings.local_adopted', state.local_shelf.recordings)`）+ 红底两步 `InlineConfirm`（`label={t('settings.local_discard')}`，`confirmLabel={t('settings.local_discard_confirm')}`，确认调 `onDiscardLocal`，录音中禁用）。
- **额度与密钥区**：
  - 社区额度卡片：
    - 已登录：标题「社区额度」+ 标签「每天重置」+「刷新」按钮；两条网格行：
      - 出字（`soniox`）：标签 + `.bar`（`width: ${pct}%`，剩余为 0 时带 `.warn` 样式）+ `今天还剩 X / Y 次`（若 `remaining === 0 && limit > 0` 附 `Tag tone="warn"`）；
      - AI（`openrouter`）：同上；
      - 若 `state.quota === null` 显示占位说明 `t('settings.quota_loading')`。
    - 未登录：卡片说明 `t('settings.quota_signin_tip')` + 「登录」按钮触发 `onOpenIdentity('ask')`。
  - 自有密钥区：
    - 副标题标出真实规则标签 `t('settings.own_keys_precedence')`（「设置后优先使用自己的密钥」）；
    - `Soniox` / `OpenRouter` 两行保持 `From` 徽章、密码输入框（提交即清空）、保存与移除按钮。
- `SettingsPage` 容器连接 `openIdentitySheet`、`send({ do: 'RefreshQuota' })`、`send({ do: 'DiscardLocal' })` 与其余既有命令。

### 2. 样式与 8 语词典

- 在 `ui/src/ds/controls.css` 补充 `.quota-grid`（3 列 `56px minmax(0, 1fr) auto`，对应 `Settings.dc.html`）、`.bar > i.warn`（耗尽警示色）与 `.adopt-banner` 样式。
- 在全部 8 语 `ui/lang/*.json` 补齐设置页身份/额度/领回新键，确保 `words.test.ts` 全部通过。

## 显式与可观测

- **纯展示与复用既有状态机**：清除本机备份直接复用经过转换表验证的红底两步 `InlineConfirm`（防止误删本机撤销备份），严格遵守「红色只表示撤不回」。
- **故障与边界态可见**：每日额度耗尽（`0 / 40`）、未拉回额度（`quota === null`）、未登录无额度、轮换密钥未存、本机备份仍在五种状态在 DOM 上均有明确词条与可操作按钮。
- **dsh 红线**：`SettingsPage.tsx` 不碰 `@tauri-apps/*` 或 `useSyncExternalStore`，通过 `scripts/build/rules/ui.sh`。

## 先写的测试

扩展 `ui/tests/library_settings.test.tsx`：
1. `settings_shows_handle_daily_quota_bars_and_refresh`：已登录含 `handle: 'demo.zuzalu.ink'` 与 `quota`（`soniox: 34/40`, `openrouter: 26/30`），断言渲染 `@demo.zuzalu.ink`、截断 DID、两条每日剩余文案与进度条；点击「刷新」发出 `{ do: 'RefreshQuota' }`；点击「退出登录」发出 `{ do: 'SignOut' }`。
2. `settings_highlights_exhausted_daily_quota`：`soniox` 剩余 `0 / 40` 时断言警示标签可见。
3. `settings_shows_local_adoption_banner_and_two_step_discard`：`local_shelf: { recordings: 3, adopted: true }` 时断言横幅显示 `3`，首次点击「清除本机备份」进入二次确认态（尚未发命令），第二次点击发出 `{ do: 'DiscardLocal' }`。
4. `settings_signed_out_offers_on_demand_identity_and_quota_prompt`：未登录态断言显示「只在本机」、社区额度登录提示与按需打开身份纸入口，自有密钥仍可照常保存与移除且提交即清输入。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `ui.sh`、`words.test.ts`、全部 UI/Rust 测试与 `vite build`）。

## 不做

- 不在本任务实现主持/对外或知识库页面（留 P5、P6）；
- 不在本任务升级 `pending.sh` 或写 P4 收尾报告（留 P4-06）。
