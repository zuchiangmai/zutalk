# P1-20 自检包：`Command::Diagnose`

## 目标

做完以后：

1. 有一条命令 `Diagnose`：在 desk 上把「现在怎么样」写成一个文件夹：版本和系统、是代码的那几项设置、此刻的水位、统计（含每条队列的深度）、树上跑着的任务、最近的日志。
2. 包里**没有**任何内容和钥匙：没有记录和音频，没有标题，没有 DID 和句柄，没有设备的名字，没有带用户名的路径。这是由类型和一条测试守着的，不是靠小心。
3. 隐私测试从日志管到整个包。

这是 `PLAN.md` §4.5.2 第四条。设置里的「导出诊断」按钮、打成 zip、让人选存到哪里，是 P3。

## 背景

- 设计：`docs/build/phases/P1.md` §3.13.6（内容的表，先读）、§3.13.14（隐私测试的查法）、§3.11（两个新的给人看的代码）、§3.13.3（`diagnose.wrote`）。
- `core/src/app/work.rs`（P1-17、P1-18）：`Work`、desk、`Job`、`job.start`、`job.done`。`core/src/app/hub.rs`：命令 → `Work`。
- `Outlet` 的 `water`、`stats`（`Reading`，随时 `now()`）；`Tree::running()`（P1-05）；`Config::logs()`、`Config.log`（P1-03）；`zutalk_log::flush`（P1-03）。
- `core/src/app/proto.rs`：`Command`、`State`（`setup`、钥匙的来源）。
- 台架（P1-06）：`nothing_said_or_secret_reaches_the_log` 和查漏的那个函数。
- 界面：`ui/dev/mock.ts` 的 `handlers` 对每个命令都要有一项（类型检查守着）。

## 做法

1. **命令**：`Command::Diagnose`，没有参数。hub：`Boot` 是 `Ready` 就交给 `Work`（不看有没有登录、有没有在录：出事的时候才最需要它）。`Work::diagnose()` 排一件 desk 的活，`kind = "diagnose"`。
2. **写到哪里**：`<数据目录>/diagnose/<墙上时钟的毫秒>/`。写之前先建到一个 `.tmp` 结尾的名字里，全写完再改名：半个包不会被当成一个包。写完以后只留最新的 `DIAGNOSE_KEPT_DIRS`（3）个文件夹，旧的删掉（删不掉的下次再删，同一行写理由）。
3. **内容**（照 `P1.md` §3.13.6 的表）：
   - `about.json`：一个专门的结构体（比如 `Facts`），字段只有数、布尔和写死的代码：`version`、`os`、`arch`、`level`、`up_ms`、`signed_in`、`spoken`（语言代码的列表）、`shown`、`sound`（声源种类的代码）、`captions`、各服务钥匙的来源（代码）。**它不是 `settings.json` 的拷贝，也不从 `State` 整个序列化**：要进包的东西，一项一项写在这个结构体里。语言的代码是 `State` 里的 `String`：进结构体之前过一遍 `zutalk_log::is_plain`，过不了的换成 `"!"`。
   - `water.json`、`stats.json`、`tasks.json`：`Water`、`Tally`、`Vec<Running>` 的 JSON。`Work` 手里要有这三样的来源（两个 `Reading` 和根树的克隆）。
   - `logs/`：先 `zutalk_log::flush(LOG_FLUSH_WAIT_MS)`，再从 `Config::logs()` 复制最新的 `DIAGNOSE_LOG_FILES`（2）个 `*.jsonl`（按修改时间）。目录不存在（测试里没装全进程的日志）就没有这个子目录，不算失败。
4. **结果**：写好了 `said.say("diagnose.done")`，并发一条 `info!("diagnose.wrote", files, bytes)`；没写成说 `diagnose`，`job.done` 的 `code` 是 `diagnose`。`ui/lang/*.json` 八个文件加这两个代码的词。
5. **界面（最小）**：`ui/dev/mock.ts` 的 `handlers` 加 `Diagnose`。不画按钮（P3）。开发的人从界面的控制台 `send({ kind: 'Diagnose' })` 就能用（照 `Command` 实际的序列化形状）。
6. 它是一件 desk 的活，所以录音中也能做，不占现场的路；它读的三样都是内存里的拷贝，复制的日志最多 16 MB。

## 显式与可观测

- **事件**：`diagnose.wrote`。写进 `docs/EVENTS.md`。`command.done`（`kind = "Diagnose"`）、`job.start`、`job.done`（`kind = "diagnose"`）、`notice.said` 是已有的。
- **统计**：不加新的名字。
- **数**：`DIAGNOSE_KEPT_DIRS`（3）、`DIAGNOSE_LOG_FILES`（2）。理由：一个包最多两个日志文件 16 MB，三个包不到 50 MB。
- **队列、任务**：不造新的。desk 上的一件活。
- **隐私**：见测试第 2、3 条。绝对路径不进包：`tasks.json`、`about.json` 里没有路径；日志里本来就没有。
- **读它的人**：`docs/EVENTS.md` 末尾加一节「自检包里有什么、怎么看」：先看 `about.json`，再看 `water.json` 里哪一格不是 `Live`，再到 `logs/` 里按那一格的 `stage` 找 `judge.verdict` 和 `stall.snapshot`。

## 先写的测试

`core/tests/liveness.rs`（台架，暂停的时钟）：

1. `a_diagnosis_says_how_things_are`：录 30 虚拟秒，让一条 lane 崩一次，发 `Diagnose`，等 `diagnose.done`。`diagnose/` 下正好一个文件夹，没有 `.tmp` 留下。里面：
   - `about.json`：`version` 是包的版本，`signed_in` 是真，`shown` 是这一场的两种语言；
   - `water.json`：有 `Capture`、`Tape`、`Sent(0)` 这些格，`Capture` 的位置在 30 000 上下；
   - `stats.json`：队列里有 `hub.mail`、`tape`、`draft`、`audio`、`lane.events`、`desk`，各有 `capacity`、`depth`、`deepest`；计数里有 `part.restarts`，是 1；
   - `tasks.json`：有 `hub`、`stats`、`desk`、`recording`、`lane.0`、`frame`、`watch`，以及正在做这件活的那个阻塞调用。
   - **看得见**：`command.done`（`kind = "Diagnose"`、`code = "ok"`）、`job.done`（`kind = "diagnose"`、`code = "ok"`）、`diagnose.wrote`（`files` 等于文件夹里的文件数）各一条。
2. `a_diagnosis_holds_nothing_that_was_said_and_no_secret`：照隐私测试（P1-06）的那一场（两种语言、改标题、搜一次、一条 lane 崩一次），并且这一条用真的写文件的口，让 `Config::logs()` 指向它写的目录；发 `Diagnose`。把整个文件夹（每个文件，`logs/` 里的也算）读出来，用**同一个**查漏的函数：
   - 每个 `.json` 文件解析得了，里面每个字符串（键和值）都过得了 `is_plain`；每个 `.jsonl` 的每一行照隐私测试的规矩；
   - 找不到话里的任何一个词、标题、说话人的标签、搜的词、DID、句柄、那把服务钥匙、数据目录的路径、台架给话筒起的设备名（给台架的设备列表放一个认得出的名字）。
3. `what_goes_in_the_bundle_is_written_down_field_by_field`：`Facts` 序列化以后的键正好是 `P1.md` §3.13.6 表里 `about.json` 那一行列的那些（一个写死的列表）。以后有人往里加东西，这条测试会要他先来改这个列表。把一种语言的代码换成一句带空格的话（`State` 里它只是个 `String`）：包里那一项是 `"!"`。
4. `only_the_newest_three_are_kept`：连着做五次（墙上时钟由 `Config` 里已有的口给，或者每次之间让时钟走一毫秒）：`diagnose/` 下是三个，是最新的三个。
5. `a_diagnosis_that_cannot_be_written_says_so`：把 `diagnose` 的位置占成一个文件：说 `diagnose`；`job.done`（`code = "diagnose"`），级别 `warn`；没有 `diagnose.wrote`。

`core/tests/app.rs`：

6. `a_diagnosis_can_be_made_with_nobody_signed_in`：没登录就发 `Diagnose`：照样写好，`signed_in` 是假。

## 验收

- `scripts/build/check.sh` 全绿。
- 报告里贴：测试第 1 条那个包的目录树（文件名和大小）和 `about.json` 的全文；`water.json` 里 `Sent(0)` 那一格。
- `git grep -n "settings.json\|State" core/src/app/work.rs` 里和 `diagnose` 有关的几行，说明 `about.json` 的每一项是从哪里一项一项取的（贴进报告）。

## 不做

- 不打 zip，不加依赖。
- 不画按钮，不加「在文件夹里显示」，不让人选存到哪里（P3）。
- 不把包发到任何地方：发不发、发给谁，是本人的事。
- 不往包里加记录、tape、`settings.json`、秘密文件、钥匙库里的任何东西。
