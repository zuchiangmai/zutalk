# P4-R1-01 修复 `AuthFlow` 并发死锁/串台、`Command` 密钥 `Debug` 遮蔽与恢复密钥按 DID 隔离

## 目标

修复 P4 第 1 轮审查中的 **必须改 #1、#3、#4**：

1. **消除 `AuthFlow` 状态机死锁与跨次尝试串台（必须改 #1）**：
   - `hub.rs` 在 `AuthFlow::Creating` / `AuthFlow::Recovering` 期间收到 `Command::SignIn` / `CreateIdentity` / `RecoverIdentity` 时，必须先明确拒绝（返回稳定代码 `"busy"`）而不触发 `Work` 取消在途任务，保证 Hub 状态与 `Work` 后台任务严格同步、绝不永久卡死；
   - `Work` 与 `Hub` 为每次认证尝试分配单调递增的代际号 `auth_gen: u64`，携带于 `Msg::SignInPending`、`Msg::SignInFailed`（以及取消/重开时的核对），任何过期代际的异步结果到达 Hub 一律静默丢弃（写清 `// dropped:` 原因）并绝不覆盖较新的 `Scanning`/`Confirm`/`Creating`/`Recovering` 状态；
   - `IdentitySheet.tsx` 在 `Creating` / `Recovering` 时禁用「返回」按钮，且用户点 `×`、点遮罩或按 `Esc` 关闭对话框时同样派发 `CancelSignIn` 以干净取消后台建号/恢复任务。
2. **`Command` 禁止在 `Debug` 中暴露密钥（必须改 #3，原则 6）**：
   - 移除 `core/src/app/proto.rs` 中 `Command` 的 `#[derive(Debug)]`，改为手动实现 `fmt::Debug`：对 `Command::RecoverIdentity { did, .. }` 遮蔽 `key: "…"`，对 `Command::OwnKey { service, key }` 遮蔽为 `key: key.as_ref().map(|_| "…")`。
3. **按 DID 隔离 `rotation_unsaved` / `CopyRotation`，并修复 `identitySheetStep` 关窗规则（必须改 #4）**：
   - 新建身份保存 `UNSAVED` 轮换密钥时同步在密钥库记录归属 DID（键名 `"rotation.did"`），在 `core/src/id/keys.rs` 提供按当前 `person_did` 过滤的 `unsaved_for(did: &str) -> bool`、`rotation_for(did: &str) -> Result<Option<Zeroizing<String>>, End>` 与 `saved_for(did: &str) -> Result<(), End>`（保持既有 `keys::unsaved()` / `keys::rotation()` / `keys::saved()` 向后兼容，避免破坏既有 `core/tests/keys.rs`）；
   - `work.rs` 中 `run_sign` 仅当 `Who::Person(me)` 且 `keys::unsaved_for(&me.did)` 为真时置 `rotation_unsaved = true`（未登录 `Who::Local` 或换了另一位 DID 登录一律为 `false`）；`Job::Clip` 与 `Job::RotationSaved` 只读取/标记当前登录人的轮换密钥，换人后绝不把前一位 DID 的私密恢复密钥复制给新登录者；
   - 修复 `ui/src/bus/identity.ts::identitySheetStep`：当 `event.signedIn` 为真时，除 `current === 'create' && event.rotationUnsaved` 留在 `'create'` 展示恢复密钥保存视图外，其余任何打开步（`'ask'` / `'login'` / `'check'` / `'recover'`）均立即切至 `'closed'`，彻底消除空白对话框。

## 背景

- 审查报告：`docs/build/reviews/P4-r1.md`（必须改 #1、#3、#4）。
- 涉及文件：
  - `core/src/app/proto.rs`
  - `core/src/id/keys.rs`
  - `core/src/id/identity.rs`
  - `core/src/app/hub.rs`
  - `core/src/app/work.rs`
  - `core/tests/identity.rs`
  - `ui/src/bus/identity.ts`
  - `ui/src/pages/IdentitySheet.tsx`
  - `ui/tests/identity_sheet.test.tsx`

## 做法

### 1. `Command` 手动 `fmt::Debug`（`core/src/app/proto.rs`）

- 将 `Command` 的 `#[derive(Debug, Clone, PartialEq, Deserialize, TS)]` 改为 `#[derive(Clone, PartialEq, Deserialize, TS)]`。
- 为 `Command` 手动实现 `std::fmt::Debug`：
  - `Command::RecoverIdentity { did, .. }` 输出 `f.debug_struct("RecoverIdentity").field("did", did).field("key", &"…").finish()`；
  - `Command::OwnKey { service, key }` 输出 `f.debug_struct("OwnKey").field("service", service).field("key", &key.as_ref().map(|_| "…")).finish()`；
  - 其余非密钥变体正常输出变体名与字段（或直接列出各变体）。

### 2. `AuthFlow` 守卫与代际号（`core/src/app/hub.rs`、`core/src/app/work.rs`）

- 在 `Hub` 中新增 `auth_gen: u64`（初始 `0`）：
  - `Command::SignIn`、`Command::CreateIdentity`、`Command::RecoverIdentity` 入口检查：
    ```rust
    if matches!(self.state.auth, AuthFlow::Creating | AuthFlow::Recovering) {
        return Err("busy");
    }
    ```
  - 每次发起 `SignIn` / `CreateIdentity` / `RecoverIdentity` 或执行 `CancelSignIn` / `SignOut` 时递增 `self.auth_gen = self.auth_gen.wrapping_add(1)`，将最新 `gen` 传入 `work.sign_in(gen)` / `work.create_identity(normalized, gen)` / `work.recover_identity(did, key, gen)`。
  - `Msg::SignInPending { gen, did, handle, code }` 与 `Msg::SignInFailed { gen, code }` 携带 `gen: u64`（同时在 `Job::ConfirmSignIn` 失败或 `net.create` 自动建号失败时使用当前有效代际或专用标识，确保 `ConfirmSignIn` 的错误正常回报，而过期 `net.signin`/`net.create_manual`/`net.recover` 的结果若 `gen != self.auth_gen` 直接忽略并加 `// dropped: stale auth attempt superseded by newer sign-in or cancel`）。
  - 在 `Work::sign_in` 中写入 `pending_signin` 前检查 `cancel.is_cancelled()`，避免刚被取消的扫码把已清空的 `pending_signin` 重新写回。

### 3. 按 DID 隔离轮换密钥（`core/src/id/keys.rs`、`core/src/app/work.rs`、`core/src/id/identity.rs`）

- 在 `core/src/id/keys.rs` 新增常量 `const ROTATION_DID: &str = "rotation.did";`：
  - `save_unsaved_rotation(did: &str, secret: &[u8; 32])` 同时写入 `UNSAVED` 与 `ROTATION_DID`；`keys::create` 中在 `set(UNSAVED, ...)` 后同样写入 `set(ROTATION_DID, Some(did.as_bytes()))?`；
  - `rotation_matches(did: &str) -> bool`：若 `ROTATION_DID` 存在则比对相等；若 `ROTATION_DID` 不存在（兼容旧测试/旧数据直接写入 `UNSAVED`）返回 `true`；
  - 新增 `pub fn unsaved_for(did: &str) -> bool`、`pub fn rotation_for(did: &str) -> Result<Option<Zeroizing<String>>, End>`、`pub fn saved_for(did: &str) -> Result<(), End>`（仅当 `rotation_matches(did)` 为真时返回/标记，否则 `false` / `Ok(None)` / `Ok(())`）。
- 在 `core/src/app/work.rs`：
  - `run_sign`：`let rotation_unsaved = me.as_deref().is_some_and(keys::unsaved_for);`；
  - `Job::Clip`：取当前 `Who::Person(me)`（无登录返 `Err("rotation.none")`），调用 `keys::rotation_for(&me.did)?.ok_or("rotation.none")?`；
  - `Job::RotationSaved`：取当前 `Who::Person(me)`，调用 `keys::saved_for(&me.did)`。

### 4. 前端 `IdentitySheet` 与 `identitySheetStep` 修复（`ui/src/bus/identity.ts`、`ui/src/pages/IdentitySheet.tsx`）

- 在 `ui/src/bus/identity.ts::identitySheetStep`：
  ```ts
  if (event.auth.is === 'Confirm') return 'check';
  if (event.signedIn) {
    if (event.rotationUnsaved && current === 'create') return 'create';
    if (current !== 'closed') return 'closed';
  }
  return current;
  ```
- 在 `ui/src/pages/IdentitySheet.tsx`：
  - `dismiss()` 与 `Escape` 监听中将取消条件扩展为 `state.auth.is !== 'Idle'`（即 `Scanning` / `Confirm` / `Creating` / `Recovering` 关闭时均调用 `onCancelScan()`）；
  - `create` 与 `recover` 步的「返回」按钮分别加上 `disabled={creating}` 与 `disabled={recovering}`。

## 显式与可观测

- **显式状态机守卫**：`Hub` 拒绝 `Creating`/`Recovering` 期间的重叠认证命令时经既有的 `command.done(kind = ..., code = "busy")` 与 `Notice("busy")` 显式上报，不静默破坏状态机与后台任务一致性。
- **零新增 `Stats` 槽位与事件**：全部复用既有 `identity.auth`、`command.done`、`job.done`，守住 `stats.full == 0` 与 `docs/EVENTS.md` 一致性。
- **隐私红线**：`Command` 的 `fmt::Debug` 遮蔽所有密钥字段；`rotation.did` 仅保存在既有密钥文件内，绝不进日志或 `State`。
- **故障可见测试**：新增确定性测试同时断言拒绝码/状态恢复与零泄漏。

## 先写的测试

1. `core/tests/identity.rs` 新增：
   - `command_debug_redacts_recovery_and_own_keys`：对 `Command::RecoverIdentity` 与 `Command::OwnKey` 执行 `format!("{cmd:?}")`，断言均不包含明文密钥串；
   - `auth_flow_refuses_overlapping_commands_while_creating_and_allows_cancel`：用挂起响应的 `MockServer` 让 `CreateIdentity` 停在 `AuthFlow::Creating`，此时发送 `Command::SignIn` / `Command::RecoverIdentity` 断言被拒返 `busy` 且 `State.auth` 保持 `Creating`；发送 `Command::CancelSignIn` 后回归 `Idle` 且可正常重新 `SignIn`；
   - `unsaved_rotation_key_is_scoped_to_creating_did_and_never_leaks_across_accounts`：新建身份 A（`rotation_unsaved == true`）不点保存直接 `SignOut` → 断言未登录态 `rotation_unsaved == false` 且 `CopyRotation` 拒返 `rotation.none` → 以身份 B（`RecoverIdentity` 或 `ConfirmSignIn`）登录 → 断言身份 B `rotation_unsaved == false` 且 `CopyRotation` 拒返 `rotation.none`（绝不复制 A 的恢复密钥）。
2. `ui/tests/identity_sheet.test.tsx` 更新与新增：
   - 更新 `identity_sheet_step_transition_table`：断言 `signedIn: true, rotationUnsaved: true` 时 `'create'` 留 `'create'`，而 `'ask'` / `'login'` / `'check'` / `'recover'` 全部转 `'closed'`；
   - 新增断言：`Creating` / `Recovering` 态下「返回」按钮被禁用，且点 `×` 关闭会发出 `{ do: 'CancelSignIn' }`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 10 个规则脚本、全部 Rust/UI 测试）。

## 不做

- 不修改 `adopt.rs` 或 `DiscardLocal`（归 `P4-R1-02`）；
- 不新增外部依赖或新 `Stats` 计数器。
