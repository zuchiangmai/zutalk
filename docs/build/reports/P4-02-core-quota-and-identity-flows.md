状态：完成

## 改动与文件

1. **`zu` 身份原语扩展（零网络 IO、纯值函数）**：
   - `zu/src/did.rs`：
     - 新增 `create_with_aka(rotation, key, also_known_as)` 支持建号时写入 `alsoKnownAs`（`zu://…` 句柄），`create` 委托其空列表实现保持兼容；
     - 新增 `Rotation::from_multikey(text)`，精确逆向解析 `Rotation::multikey()` 导出的 P-256 私钥（`[0x86, 0x26]` + 32 字节标量，中间缓冲使用 `Zeroizing`）；
     - 新增 `add_signing_key(did, audit_log, rotation, new_key)`：复用 `audit` 全量验签并取最后一条有效操作，核对 `rotation.did_key()` 属于其 `rotationKeys`（否则返 `Error::Signer`），在 `verificationMethods` 下以 `zu-<8位base32>` 追加新设备签名钥匙并签出 low-S `plc_operation`。
   - `zu/tests/did.rs`：新增 3 条测试。

2. **核心按服务每日额度与完整身份流程**：
   - 新增 `core/src/id/quota.rs`：定义定界类型 `ServiceQuota`、`Quota`（`ts-rs` 导出），纯函数 `parse`、`parse_quota`、`remaining_for`、`is_exhausted`（过 UTC 午夜 `resets_at` 自动视作已重置），薄 IO `quota::fetch(&Ours)` 调 `POST /quota`。
   - `core/src/id/keys.rs`：`REFUSED` 增补 `("quota.daily", "keys.quota.daily")`；`Chain` 增加 `quota` 缓存并在 `lease`/`claim`/`set_quota` 同步更新；`SignIn` 保留 `zu::login` 的 `https://` 规范编码并在本机测试环回地址下透明轮询本地中继；暴露 `keep_me` / `save_unsaved_rotation` 给 `id/identity.rs`。
   - 新增 `core/src/id/identity.rs`：
     - 具名带单位常量 `DEFAULT_HANDLE_DOMAIN`、`HANDLE_MIN_BYTES`、`HANDLE_MAX_BYTES`、`DIRECTORY_WAIT_MS`；
     - 显式状态机 `AuthFlow`（`Idle` / `Scanning` / `Confirm { did, handle, code }` / `Creating` / `Recovering`，预留 `code: Option<String>` 供未来六位数核对）与纯函数 `auth_step`；
     - 纯函数 `normalize_handle` 与 `did_doc_url`；
     - `PendingSignIn`（自定义 `Debug` 全遮蔽，种子包进 `Zeroizing`）实现两段式扫码确认 `receive_sign_in` + `commit`；
     - `create_identity`、`recover_identity`、`fetch_handle` 与 `<shelf_root>/profile.json` 离线句柄原子缓存。
   - `core/src/id.rs`：导出 `identity`、`quota`。

3. **协议、枢纽、后台作业、事件表、TS 绑定与最小 UI 适配**：
   - `core/src/app/proto.rs`：`State` 以 `auth: AuthFlow` 取代裸 `login`，新增 `handle: Option<String>` 与 `quota: Option<Quota>`；`Command` 新增 `ConfirmSignIn`、`CancelSignIn`、`CreateIdentity`、`RecoverIdentity`、`RefreshQuota`（`needs_person() == true`）。
   - `core/src/app/hub.rs`：通过 `set_auth` 驱动 `AuthFlow` 并发 `identity.auth`；零 IO 校验句柄与已登录守卫；处理 `SignInPending`、`QuotaUpdated`、`QuotaFailed`、`HandleResolved`。
   - `core/src/app/work.rs`：新增 `pending_signin` 槽、`Job::ConfirmSignIn`、`net.create_manual`、`net.recover`（立即用 `Zeroizing` 包裹恢复密钥）、`net.quota`（发 `quota.updated`）、`net.profile`；仅在 `Who::Person` 时拉取额度与句柄（且有待认领旧邀请时不与 `claim` 抢跑）；未登录绝不发任何服务器请求；**零新增 `Stats` 行**。
   - `docs/EVENTS.md`：登记 `identity.auth` 与 `quota.updated`，并在 `job.start` 的 `kind` 列表补充 `confirm_signin`、`recover`、`quota`、`profile`。
   - `core/bindings/`：重新生成 `AuthFlow.ts`、`Quota.ts`、`ServiceQuota.ts` 并更新 `State.ts`、`Command.ts`。
   - `ui/dev/mock.ts`、`ui/src/pages/SettingsPage.tsx`、`ui/tests/library_settings.test.tsx` 与 8 语 `ui/lang/*.json`（补齐 `code.quota.daily`、`code.keys.quota.daily`、`code.id.handle`、`code.id.recovery_key`、`code.id.signed_in`、`code.id.no_pending`、`code.id.cancelled`、`code.quota.refreshed`）同步适配。

4. **测试**：
   - 新增 `core/tests/identity.rs`（5 条非 TCP 单测/枢纽守卫测试 + 4 条本地服务器/目录全链路集成测试）；
   - 更新 `core/tests/app.rs` 中 `commands_that_need_a_person_are_listed_in_one_table` 与 `a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach`。

## 证据

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`：40/40 通过，刷新全部 TS 绑定。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：4 个 crate 零警告通过。
- 全部 10 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`、`ui`）：全部通过。
- 非 TCP Rust 测试全绿：
  - `zu/tests/did.rs`（44/44），含新增：
    - `rotation_multikey_roundtrips_and_rejects_bad_input`
    - `create_with_aka_resolves_handle`
    - `add_signing_key_appends_new_device_key_and_rejects_wrong_rotation_key`
  - `core/tests/identity.rs`（5/5 非 TCP 通过；4 条本地端口集成测试编译零警告，留待批次末沙箱外 `scripts/build/check.sh` 运行）：
    - `auth_flow_transition_table`
    - `handle_normalization_table`
    - `quota_parsing_and_midnight_reset_rules`
    - `cached_handle_and_did_doc_url_rules`
    - `create_and_recover_hub_guards_without_network`
  - `core/tests/app.rs::commands_that_need_a_person_are_listed_in_one_table`：通过。
  - `core/tests/liveness.rs::every_event_in_the_catalogue_can_be_told_apart` 与 `i8_what_one_change_sends_does_not_grow_with_the_library`：通过。
- `ui`：`npm run check` 与 `npm test`（12 文件 86/86，含 8 语 `words.test.ts`）全绿。

## 遗留 / 需负责人确认

- 六位数登录核对码保留 `AuthFlow::Confirm { code: Option<String> }` 钩子，待 `zu` `login.md` 定稿后填入派生算法（`DECISIONS.md` #5）。
- 短句柄默认域名暂集中于单一常量 `DEFAULT_HANDLE_DOMAIN = "zuzalu.ink"`。
- 本机 `did:key` 记录领回留待下一任务 P4-03；完整身份纸与设置页额度卡片留待 P4-04 / P4-05。
