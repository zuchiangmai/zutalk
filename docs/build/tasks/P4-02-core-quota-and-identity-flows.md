# P4-02 核心额度模块与完整身份流程

## 目标

做完以后：

1. **`zu` 身份原语扩展（`zu/src/did.rs`）**：
   - `did::create_with_aka` 支持建号时写入 `alsoKnownAs`（`zu://…` 句柄），`did::create` 保持原签名向后兼容；
   - `Rotation::from_multikey` 精确解析 `Rotation::multikey()` 导出的 P-256 恢复密钥并擦除中间缓冲；
   - `did::add_signing_key` 验证 `did:zu` 审计日志并用恢复密钥签出追加新设备签名钥匙的 `plc_operation`。
2. **核心按服务额度模块（`core/src/id/quota.rs`）**：
   - 定义定界类型 `ServiceQuota` 与 `Quota`（`ts-rs` 导出），纯函数解析/判断每日剩余与跨日重置，薄 IO `quota::fetch(&Ours)` 走 `POST /quota`；
   - `id/keys.rs::REFUSED` 新增 `("quota.daily", "keys.quota.daily")`，`Chain` 在 `lease`/`claim`/`fetch_quota` 后同步缓存最新 `Quota`。
3. **核心身份模块（`core/src/id/identity.rs`）**：
   - 显式状态机 `AuthFlow`（`Idle` / `Scanning { url, expires_at }` / `Confirm { did, handle, code }` / `Creating` / `Recovering`）与纯函数 `auth_step`；
   - 句柄校验与规整纯函数 `normalize_handle`（支持空、短标签 `2–32` 位拼 `DEFAULT_HANDLE_DOMAIN` 或完整 Zu 句柄，非法返 `id.handle`）；
   - 两段式扫码登录：`SignIn::receive` 验签并解析 DID 文档与主句柄后返回 `PendingSignIn`（密钥停留在 `Work` 的 `Zeroizing` 槽内，绝不进 `State`/日志），待用户发 `Command::ConfirmSignIn` 才持久化，或 `Command::CancelSignIn` 立即取消并擦除；预留 `code: Option<String>` 供未来六位数核对（`DECISIONS.md` #5）；
   - `create_with_handle`（新建身份并写入可选句柄，保留 `rotation_unsaved` 提醒与 `CopyRotation`/`RotationSaved`）、`recover`（用恢复密钥为本设备追加新签名钥匙并登录，零秘密泄漏）、`fetch_handle` 与 `<data>/records/<did>/profile.json` 离线句柄缓存。
4. **协议与枢纽接入（`core/src/app/{proto,hub,work}.rs`）**：
   - `State` 用 `auth: AuthFlow` 取代裸 `login: Option<String>`，新增 `handle: Option<String>` 与 `quota: Option<Quota>`；
   - 新增命令 `ConfirmSignIn`、`CancelSignIn`、`CreateIdentity { handle }`、`RecoverIdentity { did, key }`、`RefreshQuota`（`needs_person() == true`）；
   - 仅当 `Who::Person` 时在后台拉取额度（`net.quota`）与句柄（`net.profile`），**未登录绝不向服务器发请求**；
   - 更新 `docs/EVENTS.md`（`identity.auth`、`quota.updated` 及新 `job.start`/`job.done` 种类）、重新生成 `core/bindings/*.ts`、同步最小 `ui/dev/mock.ts`、`ui/src/pages/SettingsPage.tsx` 及全部 8 语 `ui/lang/*.json` 新增 `code.*` 键，确保全仓门禁全绿。

## 背景

- 设计：`docs/build/phases/P4.md` §3.3–§3.5；`docs/PLAN.md` §2.4、§2.7、§5.1、§8 P4；`docs/build/DECISIONS.md` #5；`docs/LESSONS.md`（扫码防抢答）。
- 涉及文件：
  - `zu/src/did.rs`、`zu/tests/did.rs`
  - `core/src/id.rs`、`core/src/id/keys.rs`
  - 新增 `core/src/id/quota.rs`、`core/src/id/identity.rs`
  - `core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/src/app/work.rs`
  - `docs/EVENTS.md`
  - `core/bindings/*.ts`（由 `cargo test` 生成）
  - `ui/dev/mock.ts`、`ui/src/pages/SettingsPage.tsx`（最小 `state.auth` 类型适配，完整 UI 在 P4-04/P4-05 做）、`ui/lang/*.json`（8 语新增 `code.*`）
  - 新增 `core/tests/identity.rs`，并更新 `core/tests/app.rs` 中 `a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach` 与 `commands_that_need_a_person_are_listed_in_one_table`

## 做法

### 1. `zu/src/did.rs`

- 抽离 `pub fn create_with_aka(rotation: &Rotation, key: &PublicKey, also_known_as: &[String]) -> Result<(String, String), Error>`，在 `alsoKnownAs` 写入 `also_known_as`；原有 `create` 调 `create_with_aka(rotation, key, &[])`。
- 新增 `Rotation::from_multikey(text: &str) -> Result<Rotation, Error>`：
  - 去掉前导 `z`，Base58Btc 解码到 `Zeroizing<Vec<u8>>`；
  - 校验长度 `== 34` 且前两字节 `== [0x86, 0x26]`；
  - 取后 32 字节调 `Rotation::from_bytes`。
- 新增 `pub fn add_signing_key(did: &str, audit_log: &[u8], rotation: &Rotation, new_key: &PublicKey) -> Result<(String, Vec<String>), Error>`：
  - 解析 `audit_log` 为 JSON 数组并复用 `audit` 逻辑验证完整性，定位最后一条未被 nullify 的操作（取其 `cid` 作 `prev`，以及原 JSON 中的 `rotationKeys`、`alsoKnownAs`、`services`、`verificationMethods`）；
  - 确认 `rotation.did_key()` 在该操作的 `rotationKeys` 列表内（不在则 `Err(Error::Signer)`）；
  - 计算 `name = format!("zu-{}", &Base::Base32Lower.encode(Sha256::digest(new_key.as_bytes()))[..8])`，写入 `verificationMethods[name] = "did:key:<new_key>"`；
  - 构造 `plc_operation`（`prev: Value::Str(prev_cid.to_string())`），用 `rotation.0.sign` 计算 low-S 签名写入 `sig`，返回 `(op.to_json()?, handles)`。

### 2. `core/src/id/quota.rs` 与 `core/src/id/keys.rs`

- 新建 `core/src/id/quota.rs`（导出 `pub mod quota;` 于 `core/src/id.rs`）：
  - `ServiceQuota { service: String, used: u32, limit: u32, remaining: u32 }`、`Quota { resets_at: i64, services: Vec<ServiceQuota> }`（均为 `TS` 导出）；
  - 纯函数：`parse(json: &serde_json::Value) -> Option<(u64, Quota)>`、`parse_quota(json: &serde_json::Value) -> Option<Quota>`、`remaining_for(quota: &Quota, service: Service, now_ms: i64) -> Option<u32>`、`is_exhausted(quota: &Quota, service: Service, now_ms: i64) -> bool`（若 `now_ms >= quota.resets_at` 视为已跨日重置，不判耗尽）；
  - 薄 IO：`pub async fn fetch(ours: &Ours) -> Result<(u64, Quota), End>` 调 `ours.ask("quota", "talk/quota", json!({}))` 并解析返回。
- 在 `core/src/id/keys.rs`：
  - `REFUSED` 新增 `("quota.daily", "keys.quota.daily")`（数组 7 -> 8）；
  - `Chain` 增加 `quota: Mutex<Option<Quota>>` 缓存及 `pub fn quota(&self) -> Option<Quota>`、`pub fn set_quota(&self, left: u64, quota: Quota)`；
  - `Ours::issue` 与 `Chain::claim` 若响应带有 `"quota"` 字段则同步更新 `Chain` 缓存；
  - 暴露 `pub(crate) fn keep_me(seed: &[u8; 32], did: String) -> Result<Me, End>`（供 `id/identity.rs` 在确认登录/恢复成功后保存），并把 `SIGN_IN_WAIT_MS`、`SIGN_IN_POLL_MS`、`CREATE_WAIT_MS` 复用为 `pub(crate)` 常量。

### 3. `core/src/id/identity.rs`

- 命名常量（带单位与理由注释）：
  - `pub const DEFAULT_HANDLE_DOMAIN: &str = "zuzalu.ink";` // default handle suffix for short labels (`P4.md` §3.4)
  - `pub const HANDLE_MIN_BYTES: usize = 2;` // short handle minimum length (`Recovery.dc.html`)
  - `pub const HANDLE_MAX_BYTES: usize = 32;` // short handle maximum length (`Recovery.dc.html`)
  - `pub const DIRECTORY_WAIT_MS: u64 = 15_000;` // timeout for fetching a DID audit log from directory
- 显式状态机 `AuthFlow` + `AuthEvent` + 纯函数 `pub fn auth_step(flow: &AuthFlow, event: AuthEvent) -> AuthFlow`：
  - `AuthFlow::code(&self) -> &'static str`（`"Idle"` / `"Scanning"` / `"Confirm"` / `"Creating"` / `"Recovering"`）；
  - 任何转换均明确写出，无不可能状态组合。
- 纯函数 `pub fn normalize_handle(input: &str) -> Result<Option<String>, &'static str>`。
- `pub fn did_doc_url(did: &str, directory: &str) -> Result<String, End>`：若 `did.starts_with("did:zu:")` 先用 `zu::did::url(did, false)` 校验语法，再拼接 `format!("{directory}/{did}/log/audit")`；否则原样返回 `zu::did::url(did, false)`。
- `pub struct PendingSignIn`（**手动实现 `fmt::Debug` 遮蔽全部敏感字段**）：持有 `did: String`、`handle: Option<String>`、`seed: Zeroizing<[u8; 32]>`、`code: Option<String>`，提供 `pub fn commit(self) -> Result<(Me, Option<String>), End>`（调用 `keys::keep_me(&self.seed, self.did)`）。
- `pub async fn receive_sign_in(sign_in: &SignIn, http: &Client, directory: &str) -> Result<PendingSignIn, End>`：轮询中继（按 `SIGN_IN_POLL_MS` / `SIGN_IN_WAIT_MS`）、打开密文、向 `did_doc_url` 拉取并验签 DID 文档、校验 `received.seed(&doc)`、提取 `doc.handles.first().cloned()`、返回 `PendingSignIn`（**此时不写盘**！）。
- `pub async fn create_identity(http: &Client, directory: &str, handle: Option<String>) -> Result<(Me, Option<String>), End>`：校验 `handle` → 若有则写入 `also_known_as = vec![format!("zu://{h}")]` → 用 `zu::did::create_with_aka` 签发创世操作并 POST `{directory}/{did}` → 成功后保存 `UNSAVED` 轮换密钥与 `identity`，返回 `(Me, handle)`。
- `pub async fn recover_identity(http: &Client, directory: &str, did: &str, key: Zeroizing<String>) -> Result<(Me, Option<String>), End>`：
  - 校验 `did` 合法且为 `did:zu:`（否则 `End::Fatal("id.did")`）；
  - `Rotation::from_multikey(key.trim()).map_err(|_| End::Fatal("id.recovery_key"))?`；
  - GET `did_doc_url(did, directory)` 拉取最新审计日志；
  - 生成新 Ed25519 种子，调用 `zu::did::add_signing_key` 签发追加操作（轮换密钥不匹配返 `End::Fatal("id.recovery_key")`）；
  - POST `{directory}/{did}`，2xx 后才调用 `keys::keep_me(&seed, did.into())`，返回 `(Me, handles.into_iter().next())`。
- `pub async fn fetch_handle(http: &Client, directory: &str, did: &str) -> Result<Option<String>, End>` 与本机缓存 `read_cached_handle(shelf_root: &Path)` / `write_cached_handle(shelf_root: &Path, handle: Option<&str>)`（读写 `<shelf_root>/profile.json`，只存句柄不存密钥）。

### 4. `proto.rs`、`hub.rs`、`work.rs`、`docs/EVENTS.md` 与最小 UI 类型同步

- `core/src/app/proto.rs`：
  - `pub use crate::id::identity::AuthFlow;`、`pub use crate::id::quota::{Quota, ServiceQuota};`；
  - `State`：移除 `pub login: Option<String>`，增加 `pub auth: AuthFlow`、`pub handle: Option<String>`、`pub quota: Option<Quota>`；
  - `Command`：新增 `ConfirmSignIn`、`CancelSignIn`、`CreateIdentity { handle: Option<String> }`、`RecoverIdentity { did: String, key: String }`、`RefreshQuota`；更新 `kind()` 与 `needs_person()`（`RefreshQuota => true`，其余新身份命令为 `false`）。
- `core/src/app/hub.rs`（保持零 IO，过 `hub.sh`）：
  - 统一经 `self.set_auth(event, why)` 调用 `auth_step` 并在状态变化时发 `info!("identity.auth", from = from, to = to, why = why)`；
  - `Command::SignIn`：允许从 `Idle` / `Scanning` 重开，设置 `Scanning { url, expires_at }`；
  - `Command::ConfirmSignIn`：若非 `AuthFlow::Confirm` 返 `Err("id.no_pending")`，否则交 `work.confirm_sign_in(...)`；
  - `Command::CancelSignIn`：通知 `work.cancel_sign_in()` 并转回 `Idle`；
  - `Command::CreateIdentity { handle }`：若已登录（`self.state.me.is_some()`）返 `Err("id.signed_in")`；校验句柄失败直接返 `Err("id.handle")`；转 `Creating` 并交 `work.create_identity(handle)`；
  - `Command::RecoverIdentity { did, key }`：若已登录返 `Err("id.signed_in")`；转 `Recovering` 并交 `work.recover_identity(did, key)`；
  - `Command::RefreshQuota`：交 `work.refresh_quota()`；
  - 新增 `Msg::SignInPending { did, handle, code }`、`Msg::QuotaUpdated { left, quota }`、`Msg::HandleResolved(Option<String>)` 处理；
  - `SignOut` 清除 `state.handle = None`、`state.quota = None`、`auth -> Idle`。
- `core/src/app/work.rs`：
  - 持有 `pending_signin: Arc<Mutex<Option<PendingSignIn>>>`；`cancel_signing` 同时清空 `pending_signin`（自动 zeroize）；
  - `net.signin` 调用 `receive_sign_in`，成功后存 `pending_signin` 并发 `Msg::SignInPending`（不写盘！）；
  - `confirm_sign_in` 在 `desk` 上消费 `pending_signin.commit()`、写入 `profile.json` 并走 `run_sign`；
  - `net.create_manual`（用户主动建号）与 `net.recover`（立即将 `key` 包入 `Zeroizing`）成功后走 `Job::Sign`；
  - `run_sign` 在 `Who::Person` 时读出离线缓存 `handle`；交由 `DeskOut::Booted` / `DeskOut::Signed` 启动 `net.quota` 与 `net.profile`（**仅在 `Who::Person` 时启动，绝不打破 `nobody_signed_in_nothing_is_asked_of_our_server`**！）；
  - `net.quota` 完成时发 `info!("quota.updated", soniox_left = s, openrouter_left = o, code = "ok")`（失败时 `warn!("quota.updated", soniox_left = 0u32, openrouter_left = 0u32, code = c)`）；
  - **不新增任何 `Stats` 计数器/高水位/分布/队列行**（守住 `stats.full == 0`）。
- 更新 `docs/EVENTS.md`：在 `### 身份与核心（hub / work）` 新增 `identity.auth` 与 `quota.updated`，并在 `job.start` 的 `kind` 括号列表补充 `confirm_signin`、`recover`、`quota`、`profile`。
- 运行 `cargo test -p zutalk-core` 刷新 `core/bindings/*.ts`（删除废弃的引用，生成 `AuthFlow.ts`、`Quota.ts`、`ServiceQuota.ts` 并更新 `State.ts`/`Command.ts`）。
- 同步 `ui/dev/mock.ts`（`EMPTY` 补齐 `auth: { is: 'Idle' }`、`handle: null`、`quota: null` 与新命令处理器）、`ui/src/pages/SettingsPage.tsx`（最小适配 `state.auth.is === 'Scanning'` 替换 `state.login`）和全部 8 语 `ui/lang/*.json` 新增 `code.*` 键（`keys.quota.daily`、`id.handle`、`id.recovery_key`、`id.signed_in`、`id.no_pending`、`id.cancelled`、`quota.refreshed`）。

## 显式与可观测

- **纯状态机与纯规则**：`AuthFlow` + `auth_step`、`normalize_handle`、`quota::{parse, remaining_for, is_exhausted}` 均有独立单元测试覆盖全部分支。
- **具名带单位常量**：`HANDLE_MIN_BYTES`、`HANDLE_MAX_BYTES`、`DIRECTORY_WAIT_MS` 均带单位与理由注释（过 `numbers.sh`）。
- **无新增 `Stats` 槽位**：复用既有 `jobs.done`/`jobs.failed`/`commands.done`，确保 `stats.full == 0`。
- **结构化事件与隐私**：`identity.auth` 与 `quota.updated` 只记状态代码与剩余数字，绝不记 DID、句柄、恢复密钥、中继 URL 片段；故障（无效恢复密钥、句柄非法、日额度用尽、中继断开、确认前取消）同时断言事件与返回码可见。

## 先写的测试

1. `zu/tests/did.rs` 新增：
   - `rotation_multikey_roundtrips_and_rejects_bad_input`；
   - `create_with_aka_resolves_handle`；
   - `add_signing_key_appends_new_device_key_and_rejects_wrong_rotation_key`（生成创世 → `add_signing_key` → 拼进审计日志 → `zu::did::resolve` 断言旧钥匙与新钥匙均有效且句柄保留；错误轮换密钥返 `Error::Signer`）。
2. 新建 `core/tests/identity.rs`：
   - `auth_flow_transition_table`：穷举 `auth_step` 转换表；
   - `handle_normalization_table` 与 `quota_parsing_and_midnight_reset_rules`；
   - `qr_sign_in_waits_for_confirmation_before_keeping_identity`：本机真实 `zutalk-server`（中继）+ `MockServer`（模拟目录含 `alsoKnownAs: ["zu://demo.zuzalu.ink"]`）→ `Command::SignIn` → 模拟手机扫二维码封发签名钥匙 → 断言 `State.auth` 进入 `Confirm { did, handle: Some("demo.zuzalu.ink"), code: None }` 且 **`keys::kept()` 仍为 `None`** → 发 `CancelSignIn` 断言回归 `Idle` 且未落盘 → 再扫一次后发 `ConfirmSignIn` 断言登录成功、`State.me` 与 `State.handle` 到位且自动拉回 `State.quota`；
   - `create_identity_with_handle_holds_unsaved_rotation_key`：`Command::CreateIdentity { handle: Some("lin".into()) }` 对本机 `MockServer` 目录注册，断言 `State.me`、`State.handle == Some("lin.zuzalu.ink")`、`rotation_unsaved == true`，`CopyRotation` 送出合法 `z…` 恢复密钥且绝不进 `State`/日志，`RotationSaved` 清提醒；非法句柄直接拒 `id.handle`；
   - `recover_identity_adds_new_signing_key_and_rejects_wrong_key`：用上一步导出的恢复密钥在清空身份的设备上调 `Command::RecoverIdentity`，先喂错误密钥断言 `id.recovery_key` 且零泄漏，再喂正确密钥断言向模拟目录 POST 合法追加操作、`zu::did::resolve` 验证通过并完成登录、`rotation_unsaved == false`；
   - `refresh_quota_and_daily_exhaustion_are_visible`：对本机 `zutalk-server`（`daily = [1, 1]`）验证 `RefreshQuota` 更新 `State.quota` 及 `quota.updated` 事件，达到每日上限时触发 `keys.quota.daily`；
   - 验证 `nobody_signed_in_nothing_is_asked_of_our_server` 在新代码下继续零请求通过。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `events.sh`、`layers.sh`、`hub.sh`、`numbers.sh`、`words.test.ts`）；
- `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 持续通过（比值 `1.00`）。

## 不做

- 本任务不做本机 `did:key` 记录领回（留 P4-03）；
- 本任务不做完整 `IdentitySheet` 与新 `Settings` 额度卡片 UI（留 P4-04、P4-05）。
