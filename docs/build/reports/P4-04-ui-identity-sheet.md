状态：完成

# P4-04 界面按需身份纸（Ask / Login / Check / Create + Recovery / Recover）

## 改动与文件

1. **显式身份纸总线状态机**：
   - 新增 [ui/src/bus/identity.ts](../../ui/src/bus/identity.ts)：定义 `SIGN_IN_WINDOW_MS = 300_000`、`COUNTDOWN_TICK_MS = 1_000` 与纯函数 `shortDid`、`validateHandleInput`、`identitySheetStep`；通过 `createCell` 暴露 `identitySheetSource`、`openIdentitySheet`、`closeIdentitySheet`、`dispatchIdentityEvent`、`resetIdentitySheet`。
   - 更新 [ui/src/bus/channel.ts](../../ui/src/bus/channel.ts) 与 [ui/src/bus/index.ts](../../ui/src/bus/index.ts)：收到 `Event::State` 时同步驱动 `AuthChanged`（`Confirm` 自动切 `'check'`，登录完成且无未存轮换密钥时自动关闭，新建身份含 `rotation_unsaved: true` 时停留在恢复密钥保存页）；收到 `sign_in.first` 通知（或命令拒绝）且当前为 `'closed'` 时自动打开 `'ask'`；`resetChannel()` 同步复位身份纸。
2. **按需身份纸纯视图与薄容器**：
   - 新增 [ui/src/pages/IdentitySheet.tsx](../../ui/src/pages/IdentitySheet.tsx)：拆分为纯 props `IdentitySheetView` 与总线容器 `IdentitySheet`，零 `@tauri-apps/*`、零 `useSyncExternalStore`、零 `dangerouslySetInnerHTML`。
   - 使用既有 `uqr` 渲染纯 React `<svg><rect .../></svg>` 二维码；覆盖五步流程：
     - `Ask`：无登录墙提示 + 扫码登录 / 新建身份 / 恢复密钥入口；
     - `Login`：二维码、三步扫码指引、5 分钟倒计时进度条、复制链接、换一个码、切换恢复密钥登录与 ESC/关闭自动 `CancelSignIn`；
     - `Check`：展示 `@handle` 与截断 DID，预留 `auth.code !== null` 大号六位数核对区（`DECISIONS.md` #5），「不一致」发 `CancelSignIn`，「一致，登录」发 `ConfirmSignIn`；
     - `Create / Recovery`：实时校验可选 `@handle` 并拦截非法输入；新建完成后在 `rotation_unsaved` 状态仅展示遮罩占位 `••••••••••••••••••••••••`（绝不渲染明文私钥）与「复制」（`CopyRotation`）/「已存好」（`RotationSaved`）/「以后再存」；
     - `Recover`：DID 与 `type="password"` `autoComplete="off"` 恢复密钥输入，提交瞬间即清空组件内秘密状态并发送 `RecoverIdentity`。
3. **侧栏、外壳、样式、Mock 与 8 语词典**：
   - 更新 [ui/src/shell/Sidebar.tsx](../../ui/src/shell/Sidebar.tsx) 与 [ui/src/shell/Shell.tsx](../../ui/src/shell/Shell.tsx)：已登录显示 `@handle`（或截断 DID）与头像首字；未登录时底部「仅本机」为可点击按钮按需打开 `IdentitySheet('ask')`；外壳挂载 `<IdentitySheet />`。
   - 更新 [ui/src/ds/controls.css](../../ui/src/ds/controls.css)：补全居中 `.scrim.center`、`.dialog`、`.dialog-t/.dialog-d/.dialog-a/.dialog-x`、`.qr-row/.qr-steps/.qr-timer`、`.check-digits`、`.side-id` 与表单前缀/错误样式。
   - 更新 [ui/dev/mock.ts](../../ui/dev/mock.ts)：完善 `SignIn`（2.5 秒后切 `Confirm`）、`ConfirmSignIn`（带 `quota` 与 `local_shelf`）、`CancelSignIn`、`CreateIdentity`、`RecoverIdentity`、`RefreshQuota`、`DiscardLocal`。
   - 同步补齐全部 8 语 [ui/lang/*.json](../../ui/lang/) 33 个新键（`en`、`zh-Hans`、`de`、`es`、`fr`、`ja`、`ko`、`th`）。

## 证据

- 新增 [ui/tests/identity_sheet.test.tsx](../../ui/tests/identity_sheet.test.tsx)（6 项测试，先红后绿）：
  - `identity_sheet_step_transition_table`
  - `home_starts_recording_without_login_wall`
  - `sign_in_qr_confirm_and_cancel_flow`
  - `create_identity_handle_validation_and_recovery_key_flow`
  - `recover_identity_clears_secret_input_immediately_on_submit`
  - `sign_in_first_notice_opens_ask_sheet_on_demand`
- 扩展 [ui/tests/shell.test.tsx](../../ui/tests/shell.test.tsx)：新增侧栏 `@handle` 显示与未登录点击打开身份纸测试。
- 沙盒内实测命令全部通过（退出码 0）：
  - `cd ui && npm run check && npm test && npm run build`：**13 个测试文件 / 93 项测试全部通过**（含 `words.test.ts` 8 语全键与占位符检查），`tsc` 与 `vite build` 零错误。
  - `bash scripts/build/rules/ui.sh`：通过（0 违规）。
  - `ui/package-lock.json` 与 `Cargo.lock` 零变更。

## 遗留 / 下一步

- 设置页完整身份卡、按服务每日额度进度条与本机领回备份横幅留待下一件任务 `P4-05`。
