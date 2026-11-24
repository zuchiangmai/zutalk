状态：完成

# P1-06 不登录也能录（一）：本机身份签记录，书架在它名下：报告

## 改了什么

1. **Zu 内核（`zu/src/`）**：
   - `zu/src/did.rs`：新增两个纯函数 `local(key: &PublicKey) -> String`（返回 `did:key:<multikey>`）与 `local_key(did: &str) -> Option<PublicKey>`（只认 Ed25519 签名钥匙的规范 `did:key:z6Mk…`，校验 `local(&key) == did`）。未改 `is_did`、`url`、`resolve` 与 `Record::verify`。
   - `zu/src/path.rs`：`Path::new` 允许作者 `by` 为 `is_did(by) || local_key(by).is_some()`，并写明这是为本机 pack 开的窄口子（指向 `docs/build/DECISIONS.md` 第 10 条）。
   - `zu/src/query.rs`：`Query::url` 对 `by` 补上 `rule(is_did(by), Error::Path)?`，向主机查询本机身份的记录仍回 `Error::Path`。
   - 原有 `zu/tests/` 里的测试一行未改。

2. **本机身份与 `Who`（`core/src/id/keys.rs`）**：
   - 新增 `pub fn local() -> Result<(Me, bool), End>`：读秘密 `identity.local`（32 字节种子），没有就用 `random::<32>()` 生成并存下，DID 为 `zu::did::local(&key.public())`，不联网，布尔值标明是不是本次刚造。
   - 新增 `pub enum Who { Nobody(&'static str), Local(Me), Person(Me) }`，带纯转换 `Who::of(kept, local)` 以及 `code()`（`"nobody"` / `"local"` / `"person"`）、`me()`、`person()`。`Who` 与 `Me` 均不实现 `Debug` 和 `Serialize`。

3. **要登录的命令表（`core/src/app/proto.rs`）**：
   - 为 `Command` 新增 `pub fn needs_person(&self) -> bool`，穷举全部 26 个变体（不用 `_ =>`），仅 `Share`、`Unshare`、`File` 返回 `true`。未改动任何 `State`/`Command` 字段形状，`core/bindings/` 零变化。

4. **Hub（`core/src/app/hub.rs`）**：
   - `Hub` 新增字段 `who: Who`（初值 `Who::Nobody("starting")`）。
   - `Hub::sign(who: Who, why: &'static str, made: bool)` 按 `who.me()` 开书架（`records/<DID，冒号换成连字符>/`），按 `who.person()` 建 `Ours`、钥匙链 issuer 与 `state.me`（本机 `did:key` 不进 `State` 也不进日志）；每次切换发 `identity.who`（`Nobody` 时为 `warn!` 并把错误代码说成通知，其余为 `info!`）。
   - 启动时读 `keys::kept()` 并**总是**调 `keys::local()`（保证退出登录有本机身份可回），再 `self.sign(Who::of(kept, local), "boot", made)`；`Msg::SignedIn` 走 `"sign_in"`、`made = false`；`Command::SignOut` 忘掉 `identity` 后回到本机身份，走 `"sign_out"`、`made = false`。
   - `Hub::shelf()` 没书架时返回 `self.who.me().err()` 带的代码（如 `keys.store`），不再回 `sign_in.first`。
   - `Hub::command` 分派前统一查 `command.needs_person() && self.who.person().is_none()`，直接回 `Err("sign_in.first")`，使 `File` 在 `city::begin` 写 `talk/filing` 之前就被拒。`share`、`unshare`、`file` 里各自的 `ok_or("sign_in.first")` 保留。
   - `Task::seal` 仅在 `length_ms == 0 || recording.about.length_ms != length_ms` 时才追加新的 `talk/recording` 记录，避免重启或登出重开已有草稿书架时重复追加同一长度的记录。
   - `carry()` 与 `migrate()` 保持既有行为不变，补充注释指向 `docs/build/phases/P1.md` §3.14.8。

5. **测试与文档**：
   - `core/tests/hub/mod.rs`：新增 `LOCAL_SEED`、`nobody()`、`local_did()`、`local_shelf(data)`。
   - 新增/更新测试见下节；`core/tests/app.rs` 利用 `keyring_core::mock::Cred::set_error` 覆盖了 `Who::Nobody` 在 hub 层的行为（`secrets_that_cannot_be_read_mean_nobody_signs_and_it_is_said`）。
   - `docs/DESIGN.md`：§3 概念表新增「本机身份」；§4 `keys` 行补上本机身份与 `Who`；§5.7 改写没有邀请时的行为说明。
   - `docs/EVENTS.md`：事件表新增 `identity.who`。

文件：`zu/src/did.rs`、`zu/src/path.rs`、`zu/src/query.rs`、`zu/tests/did.rs`、`zu/tests/record.rs`、`zu/tests/query.rs`、`server/tests/leases.rs`、`core/src/id/keys.rs`、`core/src/app/proto.rs`、`core/src/app/hub.rs`、`core/tests/keys.rs`、`core/tests/hub/mod.rs`、`core/tests/app.rs`、`docs/DESIGN.md`、`docs/EVENTS.md`、本报告。

## 证据

### 1. 内核改动统计（`git diff --stat zu/src`）

```text
 zu/src/did.rs   | 14 ++++++++++++++
 zu/src/path.rs  | 12 +++++++++---
 zu/src/query.rs |  1 +
 3 files changed, 24 insertions(+), 3 deletions(-)
```

### 2. `a_recording_needs_nobody_signed_in` 跑完后 `records/` 目录树（DID 仅留前 12 字符）

```text
records/
└── did-key-z6Mk…/
    └── 1a20d0ee996ce23f457/
        ├── about.jsonl
        └── lines.jsonl
```

### 3. 一条真实捕获的 `identity.who` 日志行（来自 `a_recording_needs_nobody_signed_in`）

```text
Line { level: Info, component: "zutalk_core::app::hub", code: "identity.who", fields: [("from", Text("nobody")), ("to", Text("local")), ("why", Text("boot")), ("made", Bool(true))] }
```

### 4. 服务器拒绝本机身份请求的步骤（`a_request_signed_by_a_devices_own_identity_is_refused`）

- 请求头能够过 `Record::try_from`（因为内核 `Path::new` 放行本机 `did:key`），`ty`、正文 CID、`aud` 与时间窗也合法；
- 接着进入 `Server::check` 查作者 DID 文档（`self.directory.doc(&record.path.by).await?`）与验签（`record.verify(|_, key, at| doc.key_at(key, at), at_of)`）：
  - 在测试里，替身目录 `Keys` 对该 `did:key` 返回空钥匙列表的 `Doc`，紧接着 `record.verify` 因 `doc.key_at` 为假返回 `Err(zu::Error::Signer)`，服务器回 `403 Forbidden`（`{"error":"signer"}`），账本零写入；
  - 在生产服务器里（`server/src/main.rs` 的 `Resolver::doc`），`self.directory.doc` 第一行调用 `zu::did::url(did, false)` 就因 `zu::Error::Method` 直接返回同一个 `Refusal(StatusCode::FORBIDDEN, "signer")`，不会发起任何外部 HTTP 请求。

### 5. 新增与更新的测试（按名列出）

- `zu/tests/did.rs`：
  1. `a_devices_own_identity_is_the_did_key_of_its_signing_key`
- `zu/tests/record.rs`：
  2. `a_record_by_a_devices_own_identity_is_held_and_checked_by_its_key_alone`
- `zu/tests/query.rs`：
  3. `a_query_for_a_devices_own_identity_is_refused`
- `server/tests/leases.rs`：
  4. `a_request_signed_by_a_devices_own_identity_is_refused`
- `core/tests/keys.rs`：
  5. `who_signs_is_the_person_else_the_device_else_nobody`
  6. `the_devices_identity_is_made_once_and_is_the_same_ever_after`
- `core/tests/app.rs`：
  7. `commands_that_need_a_person_are_listed_in_one_table`
  8. `a_recording_needs_nobody_signed_in`
  9. `a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach`
  10. `the_same_device_is_the_same_identity_the_next_time`
  11. `signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing`
  12. `what_needs_a_person_says_so_and_writes_nothing`
  13. `a_recording_is_signed_to_its_end_by_whoever_signed_its_start`
  14. `nobody_signed_in_nothing_is_asked_of_our_server`（使用 `wiremock::MockServer`，需沙箱外跑全量门禁验证）
  15. `secrets_that_cannot_be_read_mean_nobody_signs_and_it_is_said`
  16. `an_invite_the_old_app_left_becomes_an_identity_with_the_hours_it_had_left`（更新「没有邀请时」段为断言 `me` 为空且目录替身未收到请求；需沙箱外跑全量门禁验证）

### 6. 全量门禁结果

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部退出码 0，欠账表未扩大。
- `git grep -n '"sign_in.first"' core/src`：仅剩 5 处（`Hub::command` 查表、`Hub::file`、`Hub::share`、`Hub::unshare`、`Chain::claim`），`shelf()` 中无它。
- `core/bindings/`：`git status --short core/bindings/` 为空（零改动）。
- 全量门禁 `scripts/build/check.sh` 全绿：Rust `passed 489, failed 0, ignored 6`（含 wiremock 与 TCP 替身测试），界面 `svelte-check` 0 errors 0 warnings、Vitest `27 passed`。

## 留下的，要负责人的

- 没有钥匙时只录音（`captions.no_key`）与草稿等待，由 `P1-07` 实现。
- 登录以后把本机身份书架里的记录领回到本人 DID 下，留在 `P4`（`docs/build/DECISIONS.md` 第 11 条）。
- 内核 `Path` 允许 `did:key` 作本机记录作者是临时决定（`docs/build/DECISIONS.md` 第 10 条），后续需负责人与 zu 规范协调。
