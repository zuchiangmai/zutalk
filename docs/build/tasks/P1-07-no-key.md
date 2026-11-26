# P1-07 不登录也能录（二）：自己的钥匙出字；没有钥匙只录音，说 `captions.no_key`

## 目标

`docs/PLAN.md` §2.7：没登录时，「现场字幕：设置里填了自己的 Soniox 钥匙就有；没有钥匙就是『正在录音 · 只录音』，代码 `captions.no_key`，声音照录」；「精修：要自己的钥匙」。

做完以后：

1. 「这一场有没有字幕、没有是因为什么」在开始之前就定了，是一个纯函数的输出：来自文件（`file`）、没选字幕语言（`captions.off`）、没有钥匙可取（`captions.no_key`），或者有（开哪几条流）。没有钥匙的一场**不起 lane**：它是一种正常的录法，不是一次故障，不重试、不报错。
2. 没登录、填了自己的钥匙：字幕照出，草稿逐行落盘，散场后用同一把钥匙写整场记录。全程不碰我们的服务器。
3. 没有钥匙可取的草稿**等着**：不送去写，不说话，启动时也不一条条报 `keys.none`。填了钥匙以后再清点一次，写出来。等着的有几条，日志里有一条 `write.skipped`。
4. 「一种服务的钥匙从哪里来」只有一个家：`Chain::from`。

## 背景

- 设计在 `docs/build/phases/P1.md` §3.14.5（钥匙与字幕）、§3.14.6（整场转写欠不欠）、§3.14.7（事件）、§3.11（新的给人看的代码）。P1-06 已经做了：本机身份、`Who`、hub 不再因为没登录而拒绝录音。
- `core/src/id/keys.rs`：`Chain { refused, issuer, left }`；`lease` 的次序是「自己的钥匙（没被拒过）→ issuer → `End::Fatal("keys.none")` 或 `keys.refused`」；`has_own`。没登录时 `issuer` 是空的。
- `core/src/app/proto.rs`：`From { Nowhere, Own, Granted }`，`State.keys: Vec<(String, From)>`。
- `core/src/app/hub.rs`：
  - `refresh()` 里一个闭包从 `has_own` 和 `ours.is_some()` 拼出 `From`；
  - `Task::listen`：`let streams = if live { live::plan(&about.shown) } else { Vec::new() }`，`quiet = if live { "captions.off" } else { "file" }`；没有流时状态句是 `AudioOnly { why: quiet }`；
  - 现在没有钥匙又选了字幕语言时：lane 起来，取钥匙得到 `Fatal("keys.none")`，说 `Health::Stopped("keys.none")`，状态句是 `AudioOnly { why: "keys.none" }`。后面的任务会把一条以失败结束的 lane 判成 `Dead`、说给人听、留现场（P1-17、P1-18）：对一个本来就不想填钥匙的人，那是每一场都报一次错；
  - `recover()`：对每条「草稿、有音频、不是搬来的、不在写」的录音起一次整场转写；`Task::finish` → `write()`：取不到钥匙就 `Msg::Say("keys.none")`。没有钥匙的人每次启动、每条草稿各听一遍；
  - `Command::OwnKey`：`self.keys.own(..)`。
- `core/src/live/lane.rs`：`plan(shown) -> Vec<Stream>`。
- `core/src/ai/write.rs`：`write`、`sweep`。
- `ui/lang/*.json`：`code.captions.off`、`code.keys.none`、`status.AudioOnly`；`ui/src/ui/Live.svelte` 把 `AudioOnly` 的 `why` 当代码显示；`ui/src/ui/Settings.svelte` 的钥匙一节不看有没有登录，本来就能填。
- `ui/tests/words.test.ts`：正则认 `Err("…")`；`QUIET` 是手写的一张单子。
- `core/tests/app.rs`：`a_room_carries_the_live_captions_…` 里有实时服务的替身（本机端口上的脚本）；`core/tests/hub/mod.rs` 的 `soniox`（整场转写的替身）、`nobody()`（P1-06）。

## 做法

### 1. 钥匙从哪里来：一个家

- `From` 从 `app/proto.rs` 搬到 `id/keys.rs`（`id` 不能往上用 `app`），`proto.rs` 引用它；ts-rs 的导出不变（`core/bindings/From.ts` 不该变）。
- 规则是一个纯函数 `From::of(own: bool, person: bool) -> From`：有自己的钥匙 → `Own`；没有、有人登录 → `Granted`；都没有 → `Nowhere`。`Chain::from(&self, service: Service) -> From` 读出这两个事实（有没有自己的钥匙、有没有 issuer）再问它。`refresh()` 里那个闭包删掉，改用 `Chain::from`。
- hub 判断「有没有钥匙可取」读的是它小状态里的那一份（`state.keys`，就是 `Chain::from` 的结果，每次刷新、每次 `OwnKey` 之后都是新的）；录音任务和整场转写的任务手里有钥匙链，直接问 `Chain::from`。
- `lease` 的行为一点不改。

### 2. 这一场有没有字幕（`core/src/live/lane.rs`）

```rust
/// The streams a recording opens for its captions, or the one reason it has none: the first
/// row that holds. Known before the recording starts, so a recording without captions opens
/// no connection and fails at nothing.
pub fn captions(file: bool, shown: &[String], key: bool) -> Result<Vec<Stream>, &'static str>;
```

| 次序 | 条件 | 结果 |
|---|---|---|
| 1 | 声源是文件 | `Err("file")` |
| 2 | 没有选字幕语言（`plan(shown)` 是空的） | `Err("captions.off")` |
| 3 | 实时服务没有钥匙可取（`key` 为假） | `Err("captions.no_key")` |
| 4 | 否则 | `Ok(plan(shown))` |

- hub 在 `start()` 里算（`key` 来自 `state.keys` 里出字服务那一项不是 `Nowhere`），把结果交给录音任务；`listen` 里原来那两行（`streams`、`quiet`）改成用它。`Err(code)` 就是现在的 `quiet`：不起 lane，状态句是 `AudioOnly { why: code }`，声音照录、tape 照写。
- 录音中途填了钥匙：这一场不变，从下一场起有字幕（`P1.md` §3.14.8 第 3 条）。不为它加机制。
- 已经在出字的一场，钥匙中途没了、被拒了：照旧是 lane 的失败（`keys.none`、`keys.refused`、`key`），不归这条规则管。

### 3. 草稿欠不欠整场转写（`core/src/ai/write.rs`）

```rust
/// Whether a recording is owed its whole transcript now.
pub enum Owed { Yes, No, Waits(&'static str) }   // Waits: it would be, but for the reason given
pub fn owed(draft: bool, audio: bool, moved: bool, key: bool) -> Owed;
```

| 次序 | 条件 | 结果 |
|---|---|---|
| 1 | 不是草稿，或没有音频，或是从 0.8 搬来的 | `No` |
| 2 | 整场转写的服务没有钥匙可取 | `Waits("keys.none")` |
| 3 | 否则 | `Yes` |

- `recover()` 用它清点：`Yes` 的照旧去写；`Waits` 的不写、不说、不进 `state.writing`，数出来有几条；大于 0 时发一条 `info!("write.skipped", n, why = "keys.none")`。
- 停止以后（`Task::finish`，封好 tape、长度大于 0）：同一个函数。`Waits` → 不写，发一条 `write.skipped`（`n = 1`），这一条录音留在现场稿（`Row.draft` 照旧是真，界面上看得出）。
- `Command::OwnKey` 改了出字服务的钥匙以后，再 `recover()` 一次：等着的草稿现在是 `Yes`，被写出来；之前因为钥匙被拒而停下的也会再试一次（`Chain::own` 本来就把「被拒过」忘掉）。正在写的不会起第二份（`busy`）。
- 已经在写的一条，中途取不到钥匙：照旧说那个代码。`keys.none` 这个词留着。
- 「没精修」的状态、手动再试，是 P2（`DECISIONS.md` 第 4 条）。

### 4. 界面和词

- `ui/lang/*.json` 八个文件加 `code.captions.no_key`（中文「还没填出字的钥匙」，英文「No key for live text」；其余六种照已有的 `code.keys.none` 的说法写）。
- `ui/tests/words.test.ts`：`Err("captions.no_key")` 会被现有的正则认到；`QUIET` 里手写的 `captions.off`、`file` 如果因此多余了就拿掉，不多余就把 `captions.no_key` 加进去：以门禁为准。
- `Live.svelte`、`Settings.svelte` 不用改。`ui/dev/mock.ts` 不用改。

## 显式与可观测

- **事件**（`P1.md` §3.14.7）：`write.skipped`（`n`、`why`）。写进 `docs/EVENTS.md`。`recording.start` 的 `captions` 字段是 P1-12 加那条事件时带上的，这一件不加。
- **规则是值**：`captions`、`owed`、`Chain::from` 三个都是表，各有表格测试；`lane.rs`、`write.rs` 里这两个函数不读任何东西、不发事件，发事件的是调它的 hub。
- **给人看的代码**：`captions.no_key`（`P1.md` §3.11）。
- **统计、数、队列、任务**：不加新的；欠账表不许变大。
- **隐私**：`write.skipped` 只有个数和原因的代码，没有录音的 id。

## 先写的测试

`core/tests/live.rs`：

1. `a_recording_has_captions_or_one_reason_why_not`：`captions` 的表格：四行各一例，再加次序的两例（文件而且没有钥匙 → `file`；没选语言而且没有钥匙 → `captions.off`）；`Ok` 的那一例等于 `plan(shown)`。

`core/tests/write.rs`：

2. `a_draft_is_owed_its_transcript_waits_for_a_key_or_is_owed_nothing`：`owed` 的表格（四个布尔值的每种要紧的组合）。

`core/tests/keys.rs`：

3. `a_key_comes_from_the_person_from_a_grant_or_from_nowhere`：`From::of` 的表格（四种组合）；`Chain::from`：填了自己的 → `Own`（有没有 issuer 都是）；没填、有 issuer → `Granted`；都没有 → `Nowhere`；`own(service, None)` 以后回到原来的。

`core/tests/app.rs`（多线程运行时；要看事件的装捕获）：

4. `the_status_line_says_what_most_needs_saying`（已有）：加一行 `status(false, &[], "captions.no_key")` 是 `AudioOnly { why: "captions.no_key" }`。
5. `without_a_key_sound_is_kept_and_the_status_says_why`：空的钥匙库（没登录、没有钥匙）；`Setup` 选一种字幕语言；实时服务的地址指向一个只数连接的本机端口；整场转写的地址指向一个期望 0 个请求的 wiremock。`Start`：状态句是 `AudioOnly { why: "captions.no_key" }`；录 2 秒，`Pause`、`Resume` 照常；`Stop`。之后：资料库里这一条是草稿、有音频、长度大于 0；`state.writing` 空了；**没有**说过 `keys.none`；实时服务的端口一个连接也没有；wiremock 一个请求也没有。**看得见**：`write.skipped`（`n = 1`、`why = "keys.none"`）正好一条。
6. `with_their_own_key_captions_come_without_signing_in`：`hub::nobody()`（没登录，自带钥匙）；实时服务的替身照 `a_room_carries_the_live_captions_…` 的，说一个词；整场转写的替身照 `hub::soniox`。`Start`：字幕里出现那个词；`Stop`：草稿里有那一行；整场转写的替身收到了请求，带的是那把自己的钥匙；这一条不再是草稿。`Config.server` 指向一个期望 0 个请求的 wiremock：全程没有碰我们的服务器。记录都在本机身份的书架里。
7. `drafts_without_a_key_wait_quietly_and_are_written_once_there_is_one`：`hub::nobody()` 再把那把 Soniox 钥匙拿掉（`keys::set("soniox", None)`）：没有人登录、没有钥匙，本机身份是已知的。往 `hub::local_shelf(data)` 里预先放两条有音频的草稿（照 `retain.rs` 里放草稿和 tape 的办法）。起核心，等第一份 `State`，再等 300 毫秒：`state.writing` 是空的，没有说过任何代码，整场转写的替身没有收到请求。**看得见**：`write.skipped`（`n = 2`）一条。然后 `OwnKey { service: "soniox", key: Some(..) }`：两条都被写出来（`draft` 变成假）。
8. 现有的测试不改断言，全部通过：登录着、没有额度也没有自己钥匙的那些场景里，`From` 是 `Granted`，lane 照旧去取、照旧失败。

`ui`：`npm test` 的词表测试通过（八种语言都有 `code.captions.no_key`）。

## 验收

- `scripts/build/check.sh` 全绿。
- `git grep -n "has_own" core/src` 只剩 `id/keys.rs` 里面的；`git grep -n '"captions.off"\|"file"' core/src/app` 没有结果（两个原因都只写在 `lane::captions` 里）。
- `core/src/live/` 通过 `layers.sh`（`captions` 收的是一个布尔值，不用 `From`）。
- 报告里：第 5 条跑完以后的状态句和那条 `write.skipped` 日志行；`core/bindings/` 有没有变（`From.ts` 不该变）。

## 不做

- 不改 `Chain::lease` 的次序和它回的代码，不改额度（P4）。
- 不做录音中途填钥匙就开始出字。
- 不加「没精修」的状态、不加手动精修的命令（P2）。
- 不给没有钥匙的那一场起 lane 再让它失败：那条路留给真的故障。
- 不画「去填钥匙」的引导（P3）。
