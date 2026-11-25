# P4-04 界面按需身份纸（Ask / Login / Check / Create + Recovery / Recover）

## 目标

做完以后：

1. **无登录墙原则落地（`PLAN.md` §2.7）**：打开应用直接进入首页，不弹登录墙，点击开始录音立即用本机身份开录；身份纸仅在用户主动点侧栏本地身份入口、点需要身份的功能、或在核心返回 `sign_in.first` 时按需弹出。
2. **显式身份纸状态机（`ui/src/bus/identity.ts`）**：
   - `IdentitySheetStep = 'closed' | 'ask' | 'login' | 'check' | 'create' | 'recover'` + 纯函数 `identitySheetStep(step, event) -> IdentitySheetStep`；
   - `identitySheetSource`、`openIdentitySheet(step)`、`closeIdentitySheet()` 经 `ui/src/bus/` 统一导出；
   - 当 `state.auth.is === 'Confirm'` 时自动切入 `'check'`；当在 `'login'`/`'check'`/`'recover'` 完成登录（`state.me` 从空变为非空）时自动关闭；当在 `'create'` 完成新建且 `state.rotation_unsaved === true` 时停留在恢复密钥保存视图，直到点击「已存好」或「以后再存」再关闭；当收到 `Notice('sign_in.first')` 且当前为 `'closed'` 时自动打开 `'ask'`。
3. **按需身份纸组件（`ui/src/pages/IdentitySheet.tsx`）**：
   - 严格按 dsh 架构拆分为纯 props `IdentitySheetView` + 薄总线容器 `IdentitySheet`（通过 `scripts/build/rules/ui.sh`）；
   - 完整落地五步流程（对照 `Ask.dc.html`、`Login.dc.html`、`Check.dc.html`、`Recovery.dc.html`）：
     - **Ask**：欢迎文案 + 本机无身份说明 + 「扫码登录」「新建身份」「使用恢复密钥恢复」+ 关闭 `×`；
     - **Login**：`uqr` 渲染真实二维码（纯 React SVG `<rect>`，零 `dangerouslySetInnerHTML`）、三步扫码指引、5 分钟倒计时进度条与剩余分秒、复制链接、换一个码（`SignIn`）、切换恢复密钥登录、关闭 `×`（发送 `CancelSignIn`）；
     - **Check**：展示 `@handle`（若有）与截断 `did:zu:…`，当 `auth.code !== null` 时展示预留的六位数核对区（「不一致」发 `CancelSignIn` 回 `'login'`，「一致，登录」发 `ConfirmSignIn`）；
     - **Create / Recovery**：可选 `@handle` 输入（2–32 位小写字母/数字/`-` 实时校验提示）+ 「新建身份」（`CreateIdentity`）；建好后在 `rotation_unsaved` 状态下展示遮罩占位 `••••••••••••••••••••••••`（绝不在 DOM 显示明文私钥）+ 「复制」（`CopyRotation`）+ 「已存好」（`RotationSaved`）+ 「以后再存」；
     - **Recover**：输入 `did:zu:…` 与 `type="password"` `autoComplete="off"` 恢复密钥框，**提交瞬间立即清空 React 组件中的恢复密钥输入态**，发送 `RecoverIdentity`（绝不写进总线、缓存或 `localStorage`）。
4. **侧栏、外壳与全功能 Mock 同步**：
   - `Sidebar.tsx` 底部已登录时显示 `@handle`（无句柄时显示截断 DID）与头像首字；未登录时点击底部本机身份条目打开 `IdentitySheet('ask')`；
   - `Shell.tsx` 挂载 `<IdentitySheet />`；
   - `ui/dev/mock.ts` 完整模拟 `SignIn` → `Scanning` → `Confirm` → `ConfirmSignIn` / `CancelSignIn`、`CreateIdentity`（带可选句柄 + `rotation_unsaved`）、`RecoverIdentity`、`RefreshQuota`、`DiscardLocal`；
   - 全部 8 语 `ui/lang/*.json` 补齐身份纸与辅助文案（`words.test.ts` 全绿）。

## 背景

- 设计：`docs/build/phases/P4.md` §3.6；`docs/PLAN.md` §2.7、§3；`docs/build/DECISIONS.md` #5；`docs/ui-2026-11-21/{Ask,Login,Check,Recovery}.dc.html`、`zt.css`。
- 涉及文件：
  - 新增 `ui/src/bus/identity.ts`，更新 `ui/src/bus/channel.ts`、`ui/src/bus/index.ts`
  - 新增 `ui/src/pages/IdentitySheet.tsx`
  - 更新 `ui/src/shell/Sidebar.tsx`、`ui/src/shell/Shell.tsx`、`ui/src/ds/controls.css`
  - 更新 `ui/dev/mock.ts`、`ui/lang/*.json`（8 语）
  - 新增 `ui/tests/identity_sheet.test.tsx`，按需更新 `ui/tests/shell.test.tsx`

## 做法

### 1. 总线身份状态机（`ui/src/bus/identity.ts` & `channel.ts`）

- 在 `ui/src/bus/identity.ts` 定义：
  - 命名常量：
    - `SIGN_IN_WINDOW_MS = 300_000` // matches core SIGN_IN_WAIT_MS (5 minutes)
    - `COUNTDOWN_TICK_MS = 1_000` // 1 Hz countdown refresh while Scanning
  - 纯函数 `shortDid(did: string): string`（如 `did:zu:7kq2mxw4…f9a`）、`validateHandleInput(raw: string): 'empty' | 'ok' | 'invalid'`；
  - 类型 `IdentitySheetStep = 'closed' | 'ask' | 'login' | 'check' | 'create' | 'recover'` 与 `IdentitySheetEvent`：
    - `{ is: 'Open'; step: Exclude<IdentitySheetStep, 'closed'> }`
    - `{ is: 'Close' }`
    - `{ is: 'AuthChanged'; auth: AuthFlow; signedIn: boolean; rotationUnsaved: boolean }`
    - `{ is: 'SignInRequired' }`
  - 纯转换函数 `identitySheetStep(current: IdentitySheetStep, event: IdentitySheetEvent): IdentitySheetStep`：
    - `Open(step)` → `step`；
    - `Close` → `'closed'`；
    - `SignInRequired` → 当前 `'closed'` 时转 `'ask'`，否则保持现状；
    - `AuthChanged`:
      - 若 `auth.is === 'Confirm'` → `'check'`；
      - 若 `signedIn && !rotationUnsaved` 且当前在 `'ask' | 'login' | 'check' | 'recover'` → `'closed'`；
      - 若 `signedIn && rotationUnsaved` 且当前在 `'create'` → 留在 `'create'`（展示恢复密钥保存视图）；
      - 若 `signedIn && !rotationUnsaved` 且当前在 `'create'` → `'closed'`；
      - 其余保持 `current`。
  - `identitySheetCell = createCell<IdentitySheetStep>('closed')`，导出 `identitySheetSource`、`openIdentitySheet`、`closeIdentitySheet`、`dispatchIdentityEvent`、`resetIdentitySheet`（并在 `resetChannel()` 中复位）。
- 在 `ui/src/bus/channel.ts`：
  - 收到 `Event::State` 时调用 `dispatchIdentityEvent({ is: 'AuthChanged', auth: e.is.auth, signedIn: e.is.me !== null, rotationUnsaved: e.is.rotation_unsaved })`；
  - 收到 `Event::Notice` 且 `e.is === 'sign_in.first'` 时调用 `dispatchIdentityEvent({ is: 'SignInRequired' })`。
- 从 `ui/src/bus/index.ts` 导出 `identity.ts`。

### 2. 纯 props 视图与容器（`ui/src/pages/IdentitySheet.tsx`）

- 严格遵守 `ui.sh`：不导入 `@tauri-apps/*`、不调用 `useSyncExternalStore`、无 `console.log`、无 `dangerouslySetInnerHTML`。
- `IdentitySheetView` 接收纯 props：`step`、`state`、`nowMs`、`onStep`、`onClose`、`onStartScan`、`onCancelScan`、`onConfirmSignIn`、`onCreate`、`onRecover`、`onCopyRotation`、`onRotationSaved`、`onCopyUrl`。
- 各步要点：
  - 使用 `.scrim` + `.dialog`（`role="dialog"`, `aria-modal="true"`，ESC 键关闭；若当前在 `Scanning`/`Confirm` 关闭时自动调 `onCancelScan`）；
  - `login` 步：若 `state.auth.is === 'Scanning'`，用 `uqr` 绘 SVG 二维码，计算剩余时间 `remainingMs = Math.max(0, state.auth.expires_at - nowMs)` 与进度百分比 `Math.round((remainingMs / SIGN_IN_WINDOW_MS) * 100)`；展示 3 条编号步骤、「复制链接」、「换一个码」、「使用恢复密钥登录」；
  - `check` 步：若 `state.auth.is === 'Confirm'`，展示 `handle`（带 `@` 前缀）与 `shortDid(did)`；若 `state.auth.code` 非空则渲染大号等宽六位数（为 `DECISIONS.md` #5 预留）；「不一致」→ `onCancelScan()` 并回 `'login'`（或重新发 `SignIn`），「一致，登录」→ `onConfirmSignIn()`；
  - `create` 步：未登录且未 `Creating` 时展示 `@` 句柄输入 + 校验提示 + 「新建身份」提交按钮；`auth.is === 'Creating'` 时禁用按钮显示加载态；登录成功且 `state.rotation_unsaved` 为真时切换为恢复密钥保存视图（遮罩串 `••••••••••••••••••••••••`、`aria-label`、按钮「复制」「已存好」「以后再存」）；
  - `recover` 步：DID 输入框（placeholder `did:zu:…`）+ `type="password"` `autoComplete="off"` 恢复密钥输入框（placeholder `z…`），表单提交时先取局部 `trimmedDid`/`trimmedKey` 立刻执行 `setRecoveryKey('')` 清空组件状态，再调 `onRecover(trimmedDid, trimmedKey)`；`auth.is === 'Recovering'` 显示进行中态。
- `IdentitySheet` 容器：仅在 `step === 'login'` 且 `state.auth.is === 'Scanning'` 时启动 `COUNTDOWN_TICK_MS` 定时器更新 `nowMs`；切到 `'login'` 且 `auth.is === 'Idle'` 时自动派发 `send({ do: 'SignIn' })`。

### 3. 侧栏、外壳、样式与 Mock

- `ui/src/shell/Sidebar.tsx`：
  - 新增 `handle: string | null` 与 `onOpenIdentity: () => void` props；
  - 已登录显示 `handle ? '@' + handle : shortDid(me)`，头像首字优先取 `handle` 首字母；
  - 未登录时侧栏底部身份区域渲染为可点击按钮（`aria-label` / 文本 `t('identity.local')` + 提示可登录），点击调 `onOpenIdentity()`。
- `ui/src/shell/Shell.tsx`：向 `Sidebar` 传 `handle={state?.handle ?? null}` 与 `onOpenIdentity={() => openIdentitySheet('ask')}`，并渲染 `<IdentitySheet />`。
- `ui/src/ds/controls.css`：按 `docs/ui-2026-11-21/zt.css` 补全 `.scrim`、`.dialog`、`.dialog-t`、`.dialog-d`、`.dialog-a`、`.dialog-x`、`.qr` 与 `.check-digits` 样式（严守零外网 `@import`）。
- `ui/dev/mock.ts`：完善 `SignIn`（先 `Scanning`，2.5 秒后转 `Confirm { did, handle: 'demo.zuzalu.ink', code: null }`）、`ConfirmSignIn`（完成登录并填入 `quota` 与 `local_shelf`）、`CancelSignIn`、`CreateIdentity`、`RecoverIdentity`、`RefreshQuota`、`DiscardLocal`。
- 全部 8 语 `ui/lang/*.json` 补齐新键（保证占位符完全一致）。

## 显式与可观测

- **显式状态机**：`IdentitySheetStep` + 纯函数 `identitySheetStep` 配完整转换表测试；无多个零散 `boolean` 拼对话框状态。
- **具名带单位常量**：`SIGN_IN_WINDOW_MS`、`COUNTDOWN_TICK_MS` 带单位与注释。
- **隐私与边界**：恢复密钥输入提交即清、不入总线或日志；轮换密钥绝不渲染到 DOM；`ui.sh` 守卫全部 dsh 边界。
- **故障可见**：`sign_in.first` 自动拉起按需身份纸；无效句柄输入在表单即时提示且禁用提交。

## 先写的测试

新建 `ui/tests/identity_sheet.test.tsx`（标注 `// @vitest-environment jsdom`）：
1. `identity_sheet_step_transition_table`：穷举 `identitySheetStep` 全部状态与事件组合。
2. `home_starts_recording_without_login_wall`：未登录态渲染 `Shell`，断言无 `IdentitySheet` 对话框，点开始录音直接发出 `{ do: 'Start' }` 且不弹登录纸。
3. `sign_in_qr_confirm_and_cancel_flow`：点侧栏本地身份打开 `Ask` → 点「扫码登录」发 `{ do: 'SignIn' }` 并渲染二维码与倒计时 → 收到 `auth: Confirm { did, handle: 'demo.zuzalu.ink', code: null }` 自动切到 `Check` 显示 `@demo.zuzalu.ink` 与短 DID → 点「不一致」发 `{ do: 'CancelSignIn' }`；再次进入 `Check` 点「一致，登录」发 `{ do: 'ConfirmSignIn' }` 且登录后对话框自动关闭。
4. `create_identity_handle_validation_and_recovery_key_flow`：打开 `Create` → 输入非法句柄（如 `A!`）显示校验提示并拦截 → 输入 `lin` 提交发 `{ do: 'CreateIdentity', handle: 'lin' }` → 收到 `me != null, rotation_unsaved: true` 显示遮罩密钥（绝不渲染明文）→ 点「复制」发 `{ do: 'CopyRotation' }` → 点「已存好」发 `{ do: 'RotationSaved' }` 并关闭对话框。
5. `recover_identity_clears_secret_input_immediately_on_submit`：打开 `Recover` → 输入 DID 与恢复密钥 `zTestRecoveryKey123` → 提交发 `{ do: 'RecoverIdentity', did, key }` 且密码输入框 `.value` 在提交当拍即清空为 `""`。
6. `sign_in_first_notice_opens_ask_sheet_on_demand`：未登录时收到 `Event::Notice('sign_in.first')`，断言 `IdentitySheet` 自动打开 `Ask`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `scripts/build/rules/ui.sh`、`words.test.ts` 与全部 UI/Rust 测试）。

## 不做

- 不在本任务重构设置页的额度卡片与本机领回横幅（留 P4-05）。
