# P1-06 不登录也能录（一）：本机身份签记录，书架在它名下

## 目标

负责人 2026-11-21 定：「不登录，也可以录音。登录只是有额外的功能。」（`docs/PLAN.md` §2.7，`docs/build/DECISIONS.md` 第 9、10、11 条。）

做完以后：

1. 第一次起核心时，本机造一把签名钥匙，它的 `did:key` 是**本机身份**。不联网，不登记，钥匙和别的秘密放在一处。
2. 没有人登录时，记录由本机身份签，放在 `records/<本机身份>/`。录音、导入、读、改、删与撤销、搜索、导出、词表、销毁音频、字幕窗都不要登录：hub 不再以 `sign_in.first` 拒绝它们。
3. 「现在谁在签」是一个枚举（`Who`）。它每变一次（启动、登录、登出），日志里有一条 `identity.who`，里面没有任何 DID。
4. 要登录的命令（房间、交城）在一张表里；没登录时它们回答 `sign_in.first`，并且什么都不写。
5. 没登录时，核心不向我们的服务器发任何请求；本机身份签的记录，我们的服务器不收。都有测试守着。

没有钥匙时只录音（`captions.no_key`）和没有钥匙的草稿怎么办，是 P1-07。登录以后把本机身份名下的记录领回到本人的 DID 下，是 P4（`P1.md` §3.14.8）。

## 背景

- 设计在 `docs/build/phases/P1.md` §3.14，先读（§3.14.2 内核、§3.14.3 身份、§3.14.4 hub、§3.14.7 事件、§3.14.8 留给以后的）。
- 内核 `zu/`：
  - `zu/src/did.rs`：`is_did` 只收 `zu` 和 `web` 两种方法（did.md § Accepted DID Methods）；`url`、`resolve` 对别的方法回 `Error::Method`；
  - `zu/src/path.rs`：`Path::new` 要 `is_did(by)`；`zu/src/record.rs`：`Record::try_from` 经 `Path::new` 读 `by`，`to` 另用 `is_did`；`Record::verify` 的 `key_at` 由调用的人给；
  - `zu/src/key.rs`：`PublicKey` 的 `Display` 和 `FromStr` 就是 Ed25519 的 multikey（`z6Mk…`，48 个字符）；
  - `zu/src/query.rs`：`Query::url` 用 `Path::new` 检查 `by`；
  - `zu/tests/path.rs`、`record.rs`、`did.rs` 里有规范给的「不收」的例子（`did:plc`、secp256k1 的 `did:key:zQ3sh…`、`to` 是 `did:key`）。这些例子**一条不改**，都要照旧通过。
- 核心（P1-02 之后的路径）：
  - `core/src/id/keys.rs`：`Me { did, key }`；`kept()` 读秘密 `identity`（32 字节种子加 DID）；`get`、`set`、`random`；`Chain::claim` 没有登录的人时回 `sign_in.first`；
  - `core/src/app/hub.rs`：`Hub.shelf: Option<Arc<Shelf>>`、`Hub.ours: Option<Ours>`；`shelf()` 在没有书架时回 `sign_in.first`；`sign(me: Option<Me>)` 建书架（`records/<DID，冒号换成连字符>`）、`Ours`、钥匙链，设 `state.me`；`Hub::run` 开头读 `keys::kept()`；`Command::SignOut`；`Msg::SignedIn`；`share`、`unshare`、`file` 各自查 `ours`；`Command::File` 先 `city::begin`（会写一条 `talk/filing`）再查登录；`migrate()` 只在 `state.me` 有值时搬；`carry()` 在有 0.8 邀请时自己造一个 `did:zu`；
  - `core/src/record/shelf.rs`：`Shelf::new(root, by, key)`；写记录都经 `Path::new(&self.by, …)`。
- 服务器 `server/src/lib.rs` 的 `check`：先 `Record::try_from`，再 `directory.doc(by)`，再 `verify`。
- 测试：`core/tests/app.rs` 的 `store(signed_in)`、`nothing_is_recorded_before_someone_signs_in`、`an_invite_the_old_app_left_…`（里面有「没有邀请时 `Start` 被拒」两行）；`core/tests/hub/mod.rs` 的 `signed_in()`、`shelf()`；`server/tests/leases.rs`（怎么签一个请求）。
- 地基已有：`zutalk_log` 的宏和 `capture`（P1-03）；hub 从核心的根树上起，所以它发的事件，测试线程上的捕获收得到（P1-05）。
- 界面不拦录音：`ui/src` 里没有按 `state.me` 挡「开始」的地方，这一件不动界面。

## 做法

### 1. 内核：记录的作者可以是本机身份

照 `P1.md` §3.14.2。改动要小，写在一处：

- `zu/src/did.rs` 加两个纯函数：

  ```rust
  /// A device's own identity: the `did:key` of an Ed25519 Zu signing key. It names no host and
  /// is in no directory, so no host takes a record by it (§ Accepted DID Methods): such a record
  /// stays on the device that signed it.
  pub fn local(key: &PublicKey) -> String;            // "did:key:" + the multikey
  /// The key such a DID names; `None` for any other DID, and for a `did:key` of another curve.
  pub fn local_key(did: &str) -> Option<PublicKey>;   // one key, one string: `local(&key) == did`
  ```

- `zu/src/path.rs`：`Path::new` 的 `by` 是 `is_did(by)` 或 `local_key(by).is_some()`。注释写明这是为本机的 pack 开的口子，指向 `docs/build/DECISIONS.md` 第 10 条。
- **不改**：`is_did`（所以记录的 `to`、登录、查询的 `to` 照旧不收 `did:key`）；`url`、`resolve`（照旧回 `Error::Method`：没有地方解析它）；`Record::verify`。
- `zu/src/query.rs`：`Query::url` 对 `by` 加一行 `is_did` 的检查（和它对 `to` 的检查并排）：向主机问一个本机身份的记录没有意义，照旧是 `Error::Path`。

### 2. 本机身份和 `Who`（`core/src/id/keys.rs`）

照 `P1.md` §3.14.3，名字照那里的写：

- `local() -> Result<(Me, bool), End>`：读秘密 `identity.local`（32 字节种子）；没有就 `random::<32>()` 造一个、`set` 存下。`Me.did` 是 `zu::did::local(&公钥)`。返回的布尔值说是不是这一次刚造的。不联网。失败是已有的代码（`keys.store`、`keys.random`）。
- `Who`：`Nobody(&'static str)`、`Local(Me)`、`Person(Me)`，带 `code()`、`me()`、`person()`。
- `Who::of(kept: Option<Me>, local: Result<Me, End>) -> Who`：纯函数（有登录的人 → `Person`；没有、本机身份读得出 → `Local`；本机身份也读不出 → `Nobody(那个代码)`）。
- `Who`、`Me` 不实现 `Debug`、`Serialize`：钥匙和 DID 都出不去。

### 3. hub（`core/src/app/hub.rs`）

- `Hub` 多一个字段 `who: Who`（初值 `Nobody("starting")`，启动读完就换掉）。`sign(me: Option<Me>)` 改成 `sign(who: Who, why: &'static str)`：
  - 书架：`who.me()` 的（目录的算法不变：`records/<DID，冒号换成连字符>`）；`Nobody` 时没有书架；
  - `ours`、钥匙链的 issuer：只有 `who.person()` 有；
  - `state.me`：`who.person()` 的 DID。**本机身份的 `did:key` 不进 `State`**；
  - 发一条 `info!("identity.who", from = 旧的.code(), to = 新的.code(), why, made)`；`Nobody` 时用 `warn!`，并把它的代码说给人听（照现在说通知的办法）；
  - 其余照旧（清点、清扫、房间、接着交、补写、搬运）。
- 启动：`keys::kept()`（读不出：说那个代码，当作没有，照旧）；**总是** `keys::local()`（没有就造；这样登出时一定有地方回去）；`sign(Who::of(..), "boot")`，`made` 是 `local()` 给的那个布尔值。
- `Msg::SignedIn(Ok(me))` → `sign(Who::Person(me), "sign_in")`。`Command::SignOut` → 忘掉 `identity` 以后回到本机身份：`sign(Who::of(None, 再读一次本机身份), "sign_out")`。这两处 `made` 是假。
- `shelf()`：没有书架时回 `Nobody` 带的那个代码（比如 `keys.store`），不再是 `sign_in.first`。
- **要登录的命令是一张表**：`app/proto.rs` 给 `Command` 加 `needs_person(&self) -> bool`，一个写全了每个变体的 `match`（不许用 `_ =>`：以后加命令的人必须在这里表态）。P1 里是真的只有 `Share`、`Unshare`、`File`。`Hub::command` 在分派之前查一次：`command.needs_person()` 而 `self.who.person()` 是空的 → `Err("sign_in.first")`。所以 `File` 在 `city::begin` 之前就被拒绝，什么都不写。`share`、`unshare`、`file` 里各自的 `ok_or("sign_in.first")` 留着（它们同时是取值），但走不到。
- **一场录音由开始时的那个身份签到底**：录音任务手里的书架是开始时的（现在已经是这样：`Task` 拿着自己的 `Arc<Shelf>`）。不改，补一条测试。
- **0.8 的东西照旧**：`migrate()` 只搬给登录的人（`state.me` 有值），没登录不搬；`carry()` 照旧（有邀请时自己造一个 `did:zu` 并登录：这是 2026-11-16 定的无感搬运）。这两处只加注释，说明为什么不搬给本机身份（`P1.md` §3.14.8）。

### 4. 测试的家什

- `core/tests/hub/mod.rs`：
  - `LOCAL_SEED: [u8; 32] = [7; 32]`；`nobody()`：一个空的钥匙库，只放本机身份的种子（`identity.local`）和自带的 Soniox 钥匙；`local_did()`、`local_shelf(data)`（照 `shelf()` 的写法，目录从 DID 算）；
  - `signed_in()` 照旧（它不放本机身份的种子：核心会自己造一个）。
- `core/tests/app.rs` 的 `store(false)` 就是一个空的钥匙库：核心自己造本机身份。

### 5. 文档

- `docs/DESIGN.md`：§3 的概念表加一行「本机身份」（没有人登录时签记录的 `did:key`；家在 `id/keys.rs`、`zu::did::local`）；「钥匙」一行里「身份（收到的或本机造的）」后面补上它；§5.7 里「没有它就什么都不变，录音照样先要登录」改成现在的样子（没有邀请就不造 `did:zu`，由本机身份签）。
- `docs/EVENTS.md` 加 `identity.who` 一行。

## 显式与可观测

- **事件**（`P1.md` §3.14.7）：`identity.who`（`from`、`to`、`why`、`made`）。写进 `docs/EVENTS.md`，并写明：`made` 为真而这不是第一次启动，说明秘密丢过，旧的本机书架还在盘上（`P1.md` §3.14.8 第 5 条）。
- **状态机**：`Who`（`P1.md` §3.13.11 的表里加了一行）。`Who::of` 的表格测试是它的转换表；`code()` 是 `nobody`、`local`、`person`。
- **隐私**：`identity.who` 的字段只有代码和一个布尔值。DID（本机身份的也算）不进日志、不进 `State`。事件里不许出现 `Id(did)`。
- **统计、数、队列、任务**：不加新的。`spawn.sh`、`bounded.sh` 的欠账表不许变大；新写的 `let _ =` 照 `dropped.sh` 写理由。
- **给人看的代码**：不加新的。`sign_in.first` 的词已有，留着。

## 先写的测试

`zu/tests/did.rs`：

1. `a_devices_own_identity_is_the_did_key_of_its_signing_key`：`local(&key)` 是 `did:key:` 加那把钥匙的 multikey；`local_key(&local(&key)) == Some(key)`。`local_key` 对这些都是 `None`：一个 `did:zu`、一个 `did:web`、secp256k1 的 `did:key:zQ3sh…`、P-256 的（`Rotation::did_key()`）、`did:key:z6Mk`（太短）、同一把钥匙的另一种写法（大写的 `DID:key:…`、末尾多一个字符）。`is_did(&local(&key))` 是假；`url`、`resolve` 对它是 `Error::Method`。

`zu/tests/record.rs`（或 `path.rs`）：

2. `a_record_by_a_devices_own_identity_is_held_and_checked_by_its_key_alone`：用本机身份作 `by` 签一条记录；写进一个 pack 再读回来，一样；`verify` 用 `|by, key, _| local_key(by) == Some(*key)` 通过；把 `by` 换成另一把钥匙的本机身份（签名的钥匙不变），同一个 `key_at` 给出 `Error::Signer`。`to` 写成一个本机身份：照旧 `Error::Field("to")`。
3. `zu/tests/query.rs`：`Query::of(&local, "talk/line").url(..)` 是 `Err(Error::Path)`。
4. 内核原有的测试一行不改，全部通过（尤其 `path.rs` 里「不收」的那张单子）。

`server/tests/leases.rs`（或新文件）：

5. `a_request_signed_by_a_devices_own_identity_is_refused`：照现有的办法签一个 `/lease` 的请求，`by` 是本机身份、签名是对的：服务器拒绝（4xx），账本里没有多出任何东西。在报告里写明它是在哪一步被拒的（应该是查不到这个 DID 的文档）。

`core/tests/keys.rs`：

6. `who_signs_is_the_person_else_the_device_else_nobody`：`Who::of` 的表格（四种组合）；每种的 `code()`、`me()`、`person()`。
7. `the_devices_identity_is_made_once_and_is_the_same_ever_after`：空的钥匙库里 `local()` 两次：第一次 `made` 为真，第二次为假，DID 一样，是 `did:key:z6Mk…`；用它签的记录过得了第 2 条的那个 `key_at`。

`core/tests/app.rs`（多线程运行时，照现有的 `App`；要看事件的在起核心之前 `zutalk_log::capture(Level::Info)`）：

8. `a_recording_needs_nobody_signed_in`（取代 `nothing_is_recorded_before_someone_signs_in` 的前一半）：空的钥匙库。第一份 `State` 里 `me` 是空的。`Start`：有了 `session`；`Stop`；资料库里有这一条。`records/` 下正好一个目录，名字以 `did-key-z6Mk` 开头；用 `zu::Pack::read` 读它的 `about.jsonl`：每条记录的 `by` 是同一个本机身份，`verify` 过得了。改标题、搜索、导出、`Remove` 再 `Restore`，都不说 `sign_in.first`。**看得见**：`identity.who`（`from = "nobody"`、`to = "local"`、`why = "boot"`、`made = true`）正好一条；整个捕获里找不到那个 `did:key`（每条事件的每个字段）。
9. `a_sign_in_shows_its_code_and_says_when_the_relay_is_out_of_reach`：原测试的后一半（`SignIn` → `login` 有值 → `offline` → `login` 没了），断言不变，单独成一条。
10. `the_same_device_is_the_same_identity_the_next_time`：录一条，丢掉核心，再起一个：资料库里还是那一条；`records/` 下还是一个目录；第二次的 `identity.who` 里 `made` 是假。
11. `signing_in_and_out_changes_whose_shelf_is_open_and_moves_nothing`：没登录时录一条（记下 `records/` 的目录树）。**不换钥匙库**，只往里放一个登录的人（`keys::set("identity", …)`，照 `store(true)` 里那一行；换了钥匙库，本机身份的种子就没了），重起核心：`me` 是那个人，资料库是空的。`SignOut`：`me` 空了，资料库里又是那一条。本机身份那个目录里的文件一个字节没变。**看得见**：`identity.who`（`from = "person"`、`to = "local"`、`why = "sign_out"`、`made = false`）。
12. `what_needs_a_person_says_so_and_writes_nothing`：没登录，有一条录音。`Share { recording: None }`、`Share { recording: Some(id) }`、`Unshare`、`File`：各说一次 `sign_in.first`；本机书架里没有 `shares.jsonl`、`filings.jsonl`。再加一条值的测试：`Command::needs_person` 对每一个变体的答案（一张写全的表）。
13. `a_recording_is_signed_to_its_end_by_whoever_signed_its_start`：登录着开始录音；录音中 `SignOut`；`Stop`，等它封好。这一条录音的全部记录（抬头、最后写上的长度）都在那个人的书架里、由那个人签；本机身份的书架里没有它。
14. `nobody_signed_in_nothing_is_asked_of_our_server`：`Config.server` 指向一个 wiremock，期望收到 0 个请求。没登录：起核心、设置、录 2 秒、停止、改标题、搜索、导出、`Share`（被拒）。最后 `verify()`：一个请求也没有。
15. `an_invite_the_old_app_left_…`（已有）：「没有邀请时」那一段里 `Start` → `sign_in.first` 两行，改成断言 `me` 是空的、目录的替身没有收到请求。其余不变。
16. 其余现有的测试（`app.rs`、`glossary.rs`、`retain.rs`、`city.rs`、`migrate.rs`）不改断言，全部通过。

`Who::Nobody` 在 hub 这一层：如果钥匙库的替身（`keyring_core::mock`）能注入错误，加一条 `secrets_that_cannot_be_read_mean_nobody_signs_and_it_is_said`（启动时说 `keys.store`；`Start` 回 `keys.store`；`identity.who` 的 `to` 是 `nobody`，级别 `warn`）。注入不了，就在报告里写明这一支只有第 6 条的表格守着。

## 验收

- `scripts/build/check.sh` 全绿；`zu` 原有的测试一行没改。
- `git grep -n '"sign_in.first"' core/src` 只剩：`Hub::command` 里查表的那一处、`share`、`unshare`、`file` 里的取值、`Chain::claim`。`shelf()` 里没有它。
- 报告里：内核改了哪几行（`git diff --stat zu/src`，应该是十几行）；`a_recording_needs_nobody_signed_in` 跑完以后 `records/` 的目录树（文件名，DID 的那一段只留前 12 个字符）；一条真的 `identity.who` 日志行；第 5 条里服务器是在哪一步拒绝的。
- `core/bindings/` 没有变（`State`、`Command` 的形状没动）。

## 不做

- 不做领回（把本机身份的记录重新签到本人的 DID 下），不记「本机身份已归谁」，不删本机的书架（P4）。
- 不做 `captions.no_key`，不动钥匙链、`From`、整场转写欠不欠（P1-07）。
- 不把 0.8 的资料库搬给本机身份，不改邀请的搬运。
- 不改 `is_did`、`url`、`resolve`，不改内核已有的任何一条测试。
- 不动界面；不做「按下要登录的功能时弹出登录」（P3、P4）。
- 不改登录的流程本身（P4）。
