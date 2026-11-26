状态：完成

# P1-02 目录照 PLAN §5.1 搬好：报告

## 改了什么

一次纯粹的搬家，行为不变。

1. **搬家**（`git mv`，照 `P1.md` §3.1 的表，一处不多一处不少）：
   - `rt/supervise.rs`；
   - `capture/audio.rs`、`capture/audio/file.rs`、`capture/clock.rs`、`capture/tape.rs`；
   - `live/lane.rs`（原 `live.rs`）、`live/lines.rs`、`live/lang.rs`；
   - `record/shelf.rs`、`record/retain.rs`、`record/migrate.rs`；
   - `knowledge/glossary.rs`；`ai/write.rs`；
   - `out/room.rs`、`out/city.rs`、`out/export.rs`；
   - `id/keys.rs`；`app/hub.rs`（原 `app.rs`）、`app/proto.rs`。
2. **九个层的模块文件**：`core/src/{rt,capture,live,record,knowledge,ai,out,id,app}.rs`，只有 `pub mod …;`，开头的注释写这一层是什么、预算、不许做什么（抄 `PLAN.md` §4.1）。`rt`、`record`、`knowledge`、`id` 在 §4.1 的表里没有自己的一行，注释照 §5.1 写它们在哪条路上、借谁的预算。`app.rs` 的注释写明 hub 现在还在做 IO，由 P1-19、P1-20 搬走。`lib.rs` 只剩九行 `pub mod`。
3. **`again` 和 `AGAIN`** 从 `app/hub.rs` 原样搬到 `rt/supervise.rs`，都改成 `pub`。`AGAIN` 还是 `Duration` 常量（P1-04 改）。`out/city.rs` 改从 `rt::supervise` 拿。
4. **路径**：`core/src` 里的 `crate::…`、`core/tests` 和 `app/src/main.rs` 里的 `zutalk_core::…` 都改成新路径。没有别名，没有 `pub use` 留旧路径。两处跟着改了名字：`app/hub.rs` 和 `core/tests/glossary.rs` 原来经 `live::{self, …}` 用 `live::Event`、`live::config` 这些，现在是 `lane::…`。两处合在一起的 `use`（`app/hub.rs` 的 `use crate::{migrate, retain, supervise::End, tape, write};`、外壳的 `use zutalk_core::{keys, proto::Command, proto::Event};`）拆成每层一行，这样层看得见。
5. **规则脚本** `scripts/build/rules/layers.sh`：
   - 一张表：每层可以用哪几层（`P1.md` §3.1 第二张表），自己那一层总是可以；
   - 一张欠账表，一行是「文件、用的路径、几处、哪一期还」：`record/retain.rs`、`record/migrate.rs`（`crate::app::proto`，P2）、`out/city.rs`、`out/room.rs`（`crate::app::proto`，P5），各 1 处；
   - 查 `core/src/<层>.rs` 和 `core/src/<层>/**/*.rs` 里每一处 `crate::<模块>`（`use` 里的和行内的；整行是注释的不算）。不在表里、也不在欠账里：打印「文件:行」，非零退出。欠账的处数必须正好相等：多了是新代码犯规，少了（包括这个文件不再用它）要把那一行改小或删掉；
   - 另外两条，为了让第一条查得住：`crate::{…}` 把层藏在花括号里，不许；`super::super::` 会越过一层，不许。现在的代码里两样都没有了；
   - 只用 `grep`、`find`、`awk`；macOS 自带的 bash 3.2 和 BSD awk 上跑通（表经环境变量交给 awk，因为 BSD awk 的 `-v` 不收换行）。约 60 毫秒。
6. **文档**：`docs/DESIGN.md` §4 的模块表「文件」一栏改成新路径，表前加一段说层和脚本；§3「再试」一行的家从 `app` 改成 `supervise`（`again` 搬了家）。`docs/PLAN.md` §5.1 的 `live/live.rs` 改成 `live/lane.rs`，并写明原因（clippy 的 `module_inception`）。`docs/migrate.md` 里 `core/src/migrate.rs` 改成 `core/src/record/migrate.rs`。`docs/ui-2026-11-21/GAP-2026-11-21.md` 是 2026-11-21 那一刻的核对，里面的旧行号和旧路径没有改。

文件：`core/src/**`（上面的改名和九个新文件、`lib.rs`）、`core/tests/*.rs`、`core/tests/common/mod.rs`、`core/tests/hub/mod.rs`、`app/src/main.rs`、`scripts/build/rules/layers.sh`、`docs/DESIGN.md`、`docs/PLAN.md`、`docs/migrate.md`、本报告。

## 证据

**测试总数不变。** 搬之前 `cargo test --workspace`：`passed 426, failed 0, ignored 6`。搬之后 `scripts/build/check.sh` 全绿，最后一行同样是 `passed 426, failed 0, ignored 6`；界面 27 条照过。没有新的行为测试（任务不加行为）。

**协议没变。** `git status --short core/bindings` 是空的。

**往上用 `app` 的只剩欠账里的四处**：

```text
$ grep -rn "crate::app::" core/src --include=*.rs | grep -v "^core/src/app/"
core/src/record/retain.rs:6:use crate::app::proto::{Row, State};
core/src/record/migrate.rs:10:use crate::app::proto::Writing;
core/src/out/city.rs:10:use crate::app::proto::Writing;
core/src/out/room.rs:6:use crate::app::proto::Open;
```

**`layers.sh` 搬之前**（把脚本放进 `HEAD` 的一个临时 worktree 里跑）：任务预计它「什么也找不到」，实际上它找到了东西。原因是平的布局里本来就有 `core/src/app.rs` 和 `core/src/live.rs`，名字正好和两层的模块文件一样，脚本就去查它们，于是报出旧路径不是层、合在一起的 `use` 不许。四行欠账的文件那时还不存在，所以也报了：

```text
core/src/live.rs:6: crate::clock is not a layer
…（live.rs 4 行，app.rs 14 行）
core/src/app.rs:20: crate::{…} hides its layers; one crate::<layer>::… per use
core/src/record/migrate.rs: 0 uses of crate::app::proto, the debt says 1: lower the row in layers.sh, or remove it
…（另外三行欠账同样）
exit=1
```

**搬之后**：`exit=0`，`real 0m0.062s`。

**故意犯规**：在 `core/src/rt/supervise.rs` 里加一行 `use crate::capture::clock::wall_ms;`：

```text
core/src/rt/supervise.rs:9: rt may not use crate::capture::clock (layers.sh: rt)
exit=1
```

去掉以后又是 `exit=0`。另外试了三种，试完都还原了：
- 在 `record/retain.rs` 末尾加 `crate::app::proto::State`、`pub use crate::{rt};`、`crate::app::hub::Core`：报出花括号、`record may not use crate::app::hub`、`2 uses of crate::app::proto, the debt is 1 (P2)` 带两处位置，`exit=1`；
- 删掉 `out/room.rs` 的 `use crate::app::proto::Open;`：报 `0 uses …, the debt says 1: lower the row in layers.sh, or remove it`，`exit=1`。

**改名在 git 里看得见。** `git show --stat -M HEAD` 里，十九个文件里有十七个是 rename。另外两个是 `app.rs → app/hub.rs`、`live.rs → live/lane.rs`：旧路径上现在是新的层模块文件，git 默认只拿已经删掉的路径去配改名，所以这两个显示成「改了 `app.rs`、新增 `app/hub.rs`」。加 `-B`（`git show --stat -M -B HEAD`）就能看到它们也是改名。任务要九个模块文件用 `app.rs`、`live.rs` 这两个名字，又要一次提交，这个结果躲不开。

## 留下的，要负责人的

- 欠账四处照表留着：`record/retain.rs`、`record/migrate.rs` 在 P2 还，`out/city.rs`、`out/room.rs` 在 P5 还。
- `again` 的注释还写着「A transcript that could not be written…」，可现在它是整场转写、认领、交城共用的规则。任务要求原样搬，所以没有改；P1-04 改 `AGAIN` 的单位时可以顺手把这句改通用。
- 没有要负责人定的事。
