# P1-02 目录照 PLAN §5.1 搬好

## 目标

做完以后，`core/src` 的目录就是 `docs/PLAN.md` §5.1 的样子，以后每一期只在自己的目录里加东西。行为一点不变：这是一次纯粹的搬家。层与层之间谁可以用谁，由一个规则脚本守着。

## 背景

- 现在 `core/src` 是平的：`app.rs`、`audio.rs`（加 `audio/file.rs`）、`city.rs`、`clock.rs`、`export.rs`、`glossary.rs`、`keys.rs`、`lang.rs`、`lines.rs`、`live.rs`、`migrate.rs`、`proto.rs`、`retain.rs`、`room.rs`、`shelf.rs`、`supervise.rs`、`tape.rs`、`write.rs`，`lib.rs` 里各一行 `pub mod`。
- 对照表和层的方向在 `docs/build/phases/P1.md` §3.1。照那张表做，一处不多一处不少。
- 用到这些路径的地方：`core/tests/*.rs`、`core/tests/common/mod.rs`、`core/tests/hub/mod.rs`、`app/src/main.rs`。
- `ui/tests/words.test.ts` 递归地扫 `core/src`，搬家不影响它。`core/bindings/*.ts` 由 `cargo test` 写出，和模块路径无关。

## 做法

1. 用 `git mv` 搬（保住历史）。仓库的写法是 `audio.rs` 加 `audio/` 目录，不用 `mod.rs`；所以新增九个只有 `pub mod …;` 的文件：`rt.rs`、`capture.rs`、`live.rs`、`record.rs`、`knowledge.rs`、`ai.rs`、`out.rs`、`id.rs`、`app.rs`，每个开头一两行注释说这一层是什么、不许做什么（抄 `PLAN.md` §4.1 那张表里它的那一行）。
2. `live.rs` 搬成 `live/lane.rs`（不是 `live/live.rs`）。`app.rs` 搬成 `app/hub.rs`，`proto.rs` 搬成 `app/proto.rs`。
3. `again` 和 `AGAIN` 从 `app/hub.rs` 搬到 `rt/supervise.rs`，改成 `pub`：它是「会过去的失败越等越久地再试」这条恢复规则，`out/city.rs` 也在用，不该往上去 `app` 里拿。
4. 改 `use` 的路径：`crate::audio::…` → `crate::capture::audio::…`，其余同理；测试和外壳里的 `zutalk_core::…` 同样。**不加别名，不加 `pub use` 让旧路径继续能用**：一样东西一个家。
5. 文件开头的模块注释里提到别的模块名的，跟着改对。
6. 加 `scripts/build/rules/layers.sh`：
   - 脚本里一张表：每个目录可以 `use crate::` 的目录（`P1.md` §3.1 第二张表）；
   - 再一张「欠账」表，一行一处：`record/retain.rs` 用 `app::proto`（P2 还）、`record/migrate.rs` 用 `app::proto`（P2 还）、`out/city.rs` 用 `app::proto`（P5 还）、`out/room.rs` 用 `app::proto`（P5 还）；
   - 对 `core/src/<目录>/**/*.rs` 里每一处 `crate::<模块>`（`use` 里的和行内的都算），不在表里、也不在欠账里，就打印「文件:行」并以非零退出；
   - 欠账表里的一行如果已经不成立（那个文件不再用它了），也以非零退出，提醒把这一行删掉；
   - 脚本自己要快（几十毫秒），只用 `grep`、`sed`、`awk`。
7. 文档：`docs/DESIGN.md` §4 的模块表把「文件」一栏改成新路径；`CLAUDE.md` 不用动。

## 显式与可观测

这是一次搬家，不加行为，所以这一件**不发事件、不数数、不改名字**：

- 日志、统计、有名字的队列和任务是紧接着的三件地基（P1-03 至 P1-05）的事，设计在 `P1.md` §3.13。那几件的规则脚本会点名搬过来的代码里不显式的地方（没有单位的 `Duration` 常量、裸的 `tokio::spawn`、没说理由的 `let _ =`），由它们当场改或者记成欠账。这一件不要顺手去改：搬家和改写混在一起，`git show -M` 就看不出哪些是纯改名了。
- `again` 和 `AGAIN` 原样搬到 `rt/supervise.rs`（`AGAIN` 还是一个 `Duration` 常量；P1-04 把它改成带单位的数）。
- 九个新的模块文件开头的注释要写上这一层的预算（抄 `PLAN.md` §4.1 表里「预算」一栏）：读代码的人先看到这一层有多少时间。
- `layers.sh` 的表里不用列 `zutalk-log`：它是核心之外的 crate（P1-03 加），每一层都可以用。

## 先写的测试

这件任务不加行为，所以不加行为测试。先做两件事当作基线：

1. 搬之前跑一遍 `cargo test --workspace`，记下通过的总数（`check.sh` 最后一行 `passed N`）。
2. 先写 `layers.sh`，在还没搬的时候它应该什么也找不到（目录还不存在）；搬完以后它通过。然后故意在 `core/src/rt/supervise.rs` 里加一行 `use crate::capture::clock::wall_ms;`，确认它报出这一行并失败，再去掉。这一步的输出写进报告。

## 验收

- `scripts/build/check.sh` 全绿；通过的测试总数和搬之前一样。
- `git status --short core/bindings` 是空的（协议没有变）。
- `git show --stat -M HEAD` 里，除新增的九个模块文件、`layers.sh`、文档以外，`core/src` 下都是改名（`rename`），内容的改动只有路径和 `again` 的搬动。
- `grep -rn "crate::app::" core/src --include=*.rs | grep -v "^core/src/app/"` 只剩欠账表里的四个文件。

## 不做

- 不拆 `app/hub.rs`（P1-14、P1-17 拆），不改任何函数的行为和签名（`again` 变 `pub` 除外）。
- 不改测试文件的名字，不动 `server/`、`zu/`、`ui/`。
- 不还那四处欠账。
- 不建还没有内容的目录（`view/` 是 P2 的）。
- 不加日志、统计、队列和任务的名字（P1-03 至 P1-05），不改常量的名字和单位（P1-04）。
