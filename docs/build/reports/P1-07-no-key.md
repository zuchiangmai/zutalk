状态：完成

# P1-07 不登录也能录（二）：自己的钥匙出字；没有钥匙只录音，说 `captions.no_key`：报告

## 改了什么

1. **钥匙从哪里来：一个家（`core/src/id/keys.rs`、`core/src/app/proto.rs`）**：
   - 将 `From { Nowhere, Own, Granted }` 连同文档注释与 `ts_rs::TS` 派生原样从 `core/src/app/proto.rs` 搬到 `core/src/id/keys.rs`，并在 `core/src/app/proto.rs` 中 `pub use crate::id::keys::From;`；重新运行 ts-rs 导出测试后 `core/bindings/From.ts` 零改动。
   - 新增纯函数 `From::of(own: bool, person: bool) -> From`（有自己钥匙 → `Own`，否则有人登录 → `Granted`，否则 → `Nowhere`）以及 `Chain::from(&self, service: Service) -> From`。
   - 删除 `Hub::refresh()` 里的闭包，改为调用 `self.keys.from(*s)`；`Chain::lease` 的行为与代码未动。

2. **开始前定下这一场有没有字幕（`core/src/live/lane.rs`、`core/src/capture/audio.rs`、`core/src/app/hub.rs`）**：
   - `core/src/live/lane.rs` 新增纯函数 `pub fn captions(file: bool, shown: &[String], key: bool) -> Result<Vec<Stream>, &'static str>`，按表依次判定：`file` → `Err("file")`，`plan(shown)` 为空 → `Err("captions.off")`，`!key` → `Err("captions.no_key")`，其余 → `Ok(plan(shown))`。`captions` 只收布尔值，`live` 不依赖 `From`。
   - `core/src/capture/audio.rs` 为 `Source` 增加 `pub fn code(&self) -> &'static str`（`"mic"` / `"system"` / `"both"` / `"file"`），使 `core/src/app/hub.rs` 不再手写 `"file"` 与 `"captions.off"`。
   - `Hub` 新增 `has_key(&self, service: Service) -> bool`，从 `self.state.keys` 判断钥匙是否不是 `From::Nowhere`。
   - `Hub::start()` 在起录音前调用 `lane::captions`，将结果传给 `Task::record` / `Task::listen`，并在 `Err(why)` 时直接将 `Session.status` 置为 `Status::AudioOnly { why }`；`Task::listen` 不起任何 lane，不拨任何连接，照常录声音、写 tape、支持暂停与继续。

3. **草稿欠不欠整场转写（`core/src/ai/write.rs`、`core/src/app/hub.rs`）**：
   - `core/src/ai/write.rs` 新增 `pub enum Owed { Yes, No, Waits(&'static str) }` 与纯函数 `pub fn owed(draft: bool, audio: bool, moved: bool, key: bool) -> Owed`。
   - 同步修了一处单语无 OpenRouter 钥匙时的前置检查：`write::write` 仅在确实预期需要翻译（`plan.into` 非空，且 `plan.spoken` 为空或存在 `spoken != into`）时在上传前打开 `OpenRouter` 端口；若实际转写出现意外外语行（`!todo.is_empty()`），翻译前再按需打开。这样没登录、只填了 Soniox 钥匙的单语录音（如 `spoken = ["en"], shown = ["en"]`）散场后无需 OpenRouter 钥匙也能用自己的 Soniox 钥匙写出整场记录，同时保留多语缺翻译钥匙时不上传任何音频的行为。
   - `Hub::recover()` 与 `Task::finish()` 共用 `write::owed`：`Owed::Waits(why)` 时不写、不进 `state.writing`、不说 `keys.none`，并按件数发一条 `info!("write.skipped", n = …, why = why)`（清点时 `n > 0` 发一条；停止封好 tape 后发一条 `n = 1`）。
   - `Command::OwnKey` 在改了 `Service::Soniox` 的钥匙且现已有钥匙可取时，先 `refresh()` 更新 `state.keys` 再调用 `recover()`，让等着的草稿被写出来。
   - `Hub::start()` 与 `Hub::recover()` 中起录音与收尾任务的 `tokio::spawn` 包上 `zutalk_log::carry(...)`（不改变 `spawn.sh` 欠账数），确保 `Task::finish()` 发出的 `write.skipped` 事件能被日志捕获收到。

4. **界面词表与事件文档**：
   - `ui/lang/*.json` 全部 8 个文件补上 `code.captions.no_key`（`zh-Hans`：「还没填出字的钥匙」，`en`：「No key for live text」，其余 6 种语言照 `code.keys.none` 与 `code.captions.off` 的口径补齐）。
   - `ui/tests/words.test.ts`：`coreCodes()` 已能自动识别 `lane::captions` 里的 `Err("file")`、`Err("captions.off")`、`Err("captions.no_key")`，因此从手写 `QUIET` 中拿掉多余的 `'captions.off'` 与 `'file'`，保留 `'keys.none'` 与 `'keys.refused'`。
   - `docs/EVENTS.md` 事件表新增 `write.skipped`。

文件：`core/src/id/keys.rs`、`core/src/app/proto.rs`、`core/src/capture/audio.rs`、`core/src/live/lane.rs`、`core/src/ai/write.rs`、`core/src/app/hub.rs`、`core/tests/live.rs`、`core/tests/write.rs`、`core/tests/keys.rs`、`core/tests/app.rs`、`ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`、`ui/tests/words.test.ts`、`docs/EVENTS.md`、本报告。

## 证据

### 1. 第 5 条测试的状态句与 `write.skipped` 日志行

- 状态句（`Status::AudioOnly`）：
  ```text
  Status::AudioOnly { why: "captions.no_key" }
  ```
- `write.skipped` 事件（`Task::finish` 封好 tape、查 `write::owed(true, true, false, false)` 得 `Owed::Waits("keys.none")` 后发出）：
  ```text
  Line { level: Info, component: "zutalk_core::app::hub", code: "write.skipped", fields: [("n", U64(1)), ("why", Text("keys.none"))] }
  ```

### 2. `core/bindings/` 变化检查

运行 `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib`（3 个 ts-rs 导出测试全过）后检查：

- `git status --short core/bindings/`：空
- `git diff core/bindings/`：空（`core/bindings/From.ts` 无任何变化）

### 3. 源码边界 grep 检查

- `git grep -n "has_own" core/src`：
  ```text
  core/src/id/keys.rs:505:    pub fn has_own(&self, service: Service) -> bool {
  core/src/id/keys.rs:511:        From::of(self.has_own(service), self.issuer.is_some())
  ```
- `git grep -n '"captions.off"\|"file"' core/src/app`：无任何结果（0 hits）。

### 4. 新增与更新的测试（按名列出）

- `core/tests/live.rs`：
  1. `a_recording_has_captions_or_one_reason_why_not`（已通过）
- `core/tests/write.rs`：
  2. `a_draft_is_owed_its_transcript_waits_for_a_key_or_is_owed_nothing`（已通过）
- `core/tests/keys.rs`：
  3. `a_key_comes_from_the_person_from_a_grant_or_from_nowhere`（已通过）
- `core/tests/app.rs`：
  4. `the_status_line_says_what_most_needs_saying`（更新，已通过）
  5. `without_a_key_sound_is_kept_and_the_status_says_why`（已通过）
  6. `with_their_own_key_captions_come_without_signing_in`（已通过）
  7. `drafts_without_a_key_wait_quietly_and_are_written_once_there_is_one`（已通过）
- `ui/tests/words.test.ts`：
  - 词表与全部 8 种语言测试（已通过）

### 5. 全量门禁结果

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warnings）。
- 全部 6 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部退出码 0，欠账表未改动。
- 全量门禁 `scripts/build/check.sh` 全绿：Rust `passed 495, failed 0, ignored 6`（含上述全部 TCP / wiremock 测试），界面 `svelte-check` 0 errors 0 warnings + `tsc`、`27 passed`。

## 留下的，要负责人的

- 「没精修」作为一种可见状态及手动再试命令留在 P2（`docs/build/DECISIONS.md` 第 4 条）。
- 真机无钥匙只录音与自有钥匙出字的 1 小时实录验证，留在 P1-26（出口 E9，需负责人与真实钥匙执行）。
