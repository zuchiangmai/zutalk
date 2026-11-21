# P1-18 hub 只算不做（二）：启动、身份、钥匙、设置、更新；规则脚本

## 目标

做完以后：

1. `core/src/app/hub.rs` 里没有任何 IO：不读写磁盘、不碰钥匙库、不发请求、不问系统有哪些设备、不装更新，也不起任务。处理一条消息的函数都是同步的，整个文件里 `.await` 只有收信那一处。
2. `scripts/build/rules/hub.sh` 守着这一条，以后加不回来。
3. hub 是哪一种样子（还在起、可以用、正在装更新）是一个枚举；启动的每一步都在日志里，带用时。
4. `core/src/app/hub.rs` 在 `spawn.sh` 里的欠账清零。

后台一次一件、`ai/write.rs` 和 `out/city.rs` 的欠账，是 P1-19。

## 背景

- 设计：`docs/build/phases/P1.md` §3.8；§3.13.11（`Boot`）、§3.13.3（标着 P1-18 的两行）、§3.13.12（欠账表）。`PLAN.md` §4.1 的 hub、L4 两行；`CLAUDE.md`：「The hub and the live path do no IO」「The shell never blocks」。
- P1-17 之后 `hub.rs` 里还剩的 IO（按名字找）：
  - `Hub::run` 的开头：读 `settings.json`、`audio::devices()`（系统调用，可能很慢）、`keys::kept()`、`carry()`（读旧版留下的邀请文件）；
  - `sign()`：`keys::unsaved()`；起认领邀请的任务（`chain.claim`、`keys::carried`）；
  - 命令：`Setup`（写 `settings.json`）、`SignIn`（`SignIn::begin`，再起任务等结果）、`SignOut`（`keys::set`）、`CopyRotation`（`keys::rotation()`，再调外壳给的剪贴板）、`RotationSaved`（`keys::saved()`）、`OwnKey`（`keys.own`，写钥匙库）、`Update`（同步调 `config.install`，它会装更新并重启应用）；
  - `carry()` 里起造身份的任务（`keys::create`）。
- `core/src/app/work.rs`（P1-17）：`Work`、desk、后台的任务都在这里，都从 `work` 子树上起，每件活有 `job.start`、`job.done`。
- 还欠着的：`core/src/app/hub.rs` 里登录、认领、造身份、日历几处裸的 `tokio::spawn`（这一件还）；`core/src/ai/write.rs`、`core/src/out/city.rs` 里的（P1-19 还）。
- `docs/DESIGN.md` §5.10：现在「装和重启都在枢纽处理这条命令时同步完成，中间不会开始新的录音」。这一句要换一种办法保住。

## 做法

### 启动

1. `Core::start` 起 hub 以后，让 `Work` 做「启动」这件活（desk 上）：读设置、读身份、读旧邀请。结果一条消息回来（设置、登录的是谁的 DID、恢复密钥存没存、途中要说的代码）。`Work` 自己留下身份的钥匙，hub 只知道 DID。启动的每一步各发一条 `boot.step`（`step` 是 `settings`、`identity`、`invites`、`devices`；`code`；`ms`），没成的用 `warn!`：`PLAN.md` §4.5.2「启动的每一步（用时）」。
2. 设备列表另起一个任务（`work` 子树上的 `devices`，里面 `tree.blocking("devices", …)`）去问，什么时候回来什么时候放进 `state.devices`：它卡住不许拖住启动。卡住的时候，`boot.step`（`step = "devices"`）一直不出现，`Tree::running()` 里有它。日志里只有个数和用时，**没有设备的名字**。
3. 启动的结果到达之前来的命令：`starting`，拒绝。启动之后第一次发布 `State`（和现在一样：界面拿到第一份状态之前没有东西可点）。
   - hub 的这几种样子是一个枚举（`P1.md` §3.13.11）：`Boot { Starting, Ready, Installing }`，有 `code()`；转换是一个纯函数（`Starting` 遇「启动好了」→ `Ready`；`Ready` 遇「装吧」→ `Installing`；`Installing` 遇「没装上」→ `Ready`；别的不变），「这条命令现在收不收」也是它的一个纯函数（`Starting` 时全拒，`starting`；`Installing` 时拒 `Start`、`Import`，`update.installing`）。不许用几个 `bool` 拼。每次转换发一条 `info!("hub.boot", from, to, why)`。

### 身份与钥匙

4. 登录、登出、造身份、认领邀请、存恢复密钥、复制恢复密钥、设自己的钥匙：都是 `Work` 的方法。碰钥匙库和本机文件的部分在 desk 上，等中继、等目录、等服务器的部分在各自的任务里（`work` 子树上，名字 `net.signin`、`net.claim`、`net.create`、`net.calendar`）。结果回来的消息只带 hub 需要的东西（DID、登录要显示的码、存没存、成功或代码），**钥匙不经过 hub 的邮箱**。每件照 P1-17 的办法有 `job.start`、`job.done`；事件里只有种类和结果的代码，**没有 DID、句柄、登录的码、钥匙**。
5. 换了人：`Work` 先在 desk 上把自己的 `Shelf` 等换掉，再告诉 hub；hub 收到后照旧清点、刷新（P1-17 的那几件活）。
6. 复制恢复密钥：desk 上读出来，直接交给外壳给的 `clip`，回来的只是 `copied`、`clip` 或 `rotation.none`。

### 设置

7. `Setup`：hub 上先做不需要 IO 的检查（栏数），然后**立刻**把 `state.setup` 换成新的（这是本人此刻的选择，下一条 `Start` 就要用它），再让 desk 把它存下。存不下：说 `settings`，内存里的选择不退回。

### 更新

8. `Update`：正在录音 → `update.recording`（不变）。否则 hub 的 `Boot` 变成 `Installing`，让 `Work` 在一个阻塞调用里（`tree.blocking("update.install", …)`）调 `config.install`，前后是 `job.start`、`job.done`（`kind = "install"`）。装上了应用会重启；它要是返回了，回来一条消息：`Boot` 回到 `Ready`，说 `update.install`。
9. `Installing` 期间 `Start`、`Import`：`update.installing`，拒绝。这样「装的中间不会开始新的录音」仍然成立，而 hub 没有被占住。

### 还清 hub 的欠账

10. `core/src/app/hub.rs` 里剩下的裸 `tokio::spawn`（登录、认领、造身份、日历）随着上面第 4 条搬进 `Work` 的子树。做完以后 `hub.rs` 里没有任何起任务的地方；`spawn.sh` 的欠账表里 `core/src/app/hub.rs` 那一行删掉。
11. 停止以后封 tape、写长度不是 hub 的事（P1-14 已经把它放在录音任务里）。

### 规则脚本

12. `scripts/build/rules/hub.sh`，对 `core/src/app/hub.rs`：
    - 不许出现：`std::fs`、`fs::`、`tokio::fs`、`File::`、`reqwest`、`spawn_blocking`、`tokio::spawn`、`.spawn(`、`.blocking(`、`.keep(`（hub 不起任务，连从树上起也不：起任务的是 `Work`；`Core::start` 起 hub 自己的那几行如果在这个文件里，挪到 `app/core.rs` 之类的地方，或者让脚本只看 `impl Hub` 以内，选一种并写明）、`Shelf`、`tape::`、`audio::devices`、`keys::` 后面跟着函数调用（类型名如 `keys::Service` 可以）、`.install)(`、`.clip)(`；
    - 发事件（`zutalk_log` 的宏）、数数（`Counter::add`）、读 `std::time::Instant` 量一圈用了多久，不在禁单里：它们不等任何东西；
    - `.await` 只许出现一次；
    - 违反就打印「文件:行」并以非零退出。
    名单可以按实际的写法调整，但要在报告里证明它抓得住：把 P1-17 之前的 `hub.rs`（`git show`）喂给它，它要报出一长串。

### 文档

13. `docs/DESIGN.md` §5.10 里「同步完成」那一句改成现在的办法。

## 先写的测试

`core/tests/app.rs`：

0. `the_hub_boots_as_its_table_says`（值进值出）：`Boot` 的转换表，和「这条命令现在收不收」的表（每个状态 × `Start`、`Import`、`Pause`、`Setup` 几种命令）。
1. `a_command_before_the_core_is_ready_is_turned_away`：单线程运行时，`Core::start` 之后不 `await` 就 `send(Start)`：说 `starting`；等第一份 `State` 出来以后再 `Start`，正常。**看得见**：`command.done`（`kind = "Start"`、`code = "starting"`）一条；`hub.boot`（`from = "Starting"`、`to = "Ready"`）一条，在它之后才有 `code = "ok"` 的 `Start`；`boot.step` 至少三条（`settings`、`identity`、`invites`），`code` 都有。
1a. `a_device_list_that_never_comes_holds_up_nothing`：问设备的替身卡在一道闸上（给 `Config` 的 `hear` 旁边加一个问设备的端口，或者用现有能换的那一层；真的没有口，就在 `Work` 这一层用替身来测）：`hub.boot` 照样到 `Ready`，命令照答；没有 `boot.step`（`step = "devices"`）；`Tree::running()` 里有 `devices`。放开以后那一条出现，`state.devices` 有了。
2. `a_setting_takes_effect_at_once_and_is_kept`：`Setup`（字幕语言改成两种）紧接着 `Start`，中间不等：这一场的 `Frame` 有两栏。重新起一个核心：设置还在。
3. `settings_that_cannot_be_saved_say_so_and_still_hold`：把 `settings.json` 的位置占成一个文件夹：说 `settings`；`state.setup` 是刚选的。**看得见**：`job.done`（`kind = "settings"`、`code = "settings"`），级别 `warn`；`notice.said`（`code = "settings"`）。
4. `nothing_is_recorded_while_an_update_is_being_put_in_place`（真实时钟）：`install` 的替身卡在一道闸上。`Update` 之后 `Start` → `update.installing`；这期间 `Pause` 这类命令照样有回答（hub 没被占住）；放开闸（替身返回，等于没装上）→ `update.install`，之后 `Start` 正常。现有的 `an_update_is_installed_only_while_nothing_is_recording` 并进来或保留，断言不放松。**看得见**：`hub.boot` 依次 `Ready → Installing → Ready`；堵着的时候有 `job.start`（`kind = "install"`）没有 `job.done`；`command.done`（`kind = "Start"`、`code = "update.installing"`）。
5. `the_recovery_key_goes_to_the_clipboard_and_through_nothing_else`（已有）继续通过，并加两条断言：途中 `outlet.state`、`outlet.said` 的任何一份序列化结果里都没有那把钥匙；整个捕获里（每条事件的每个字段）也没有它，没有登录的那个人的 DID 和句柄。

## 显式与可观测

- **事件**：`hub.boot`、`boot.step`。写进 `docs/EVENTS.md`；`job.*` 的 `kind` 清单补上这一件新加的（`settings`、`boot`、`install`、`signin`、`claim`、`create`、`calendar`、`clip`、`own_key`…）。
- **统计**：不加新的名字（`jobs.done`、`jobs.failed`、`desk.*` 已经在数）。
- **状态机**：`Boot`（转换表测试第 0 条）。
- **数**：这一件没有新的时限和上限。
- **队列**：不造新的。
- **任务**：都在 `work` 子树上（`boot`、`devices`、`net.signin`、`net.claim`、`net.create`、`net.calendar`，和装更新的阻塞调用）。`hub.rs` 里没有任何起任务的地方。
- **欠账**：`spawn.sh` 里 `core/src/app/hub.rs` 清零。
- **`hub.turn.slow`**：这一件做完以后，hub 上没有 IO 了。台架跑一场 5 虚拟分钟的录音、中间发二十条各种命令，报告里贴 `hub.turn_us` 的 `p50`、`p99` 和 `hub.turn.slow` 的条数。它们来自真实时钟，**不进断言**；但如果 `p99` 明显超过一毫秒，找出是哪种消息（`hub.turn.slow` 的 `kind` 会说），写进报告。
- **隐私**：身份那一类活的事件里只有种类和结果的代码。
- **P1-05 报告里指给这一件的丢弃**：在这里处理。

## 验收

- `scripts/build/check.sh` 全绿（`hub.sh` 在其中）。
- 报告里：`hub.sh` 对旧的 `hub.rs` 的输出（证明它抓得住）；`hub.rs` 的 `use` 列表；`hub.rs`、`work.rs`、`recording.rs` 各多少行；`spawn.sh` 的欠账表改之前、改之后的样子；一次启动的全部日志行（从测试第 1 条的捕获里抄：`task.start`、`boot.step`、`hub.boot`）。
- `git grep -n "spawn\|blocking(" core/src/app/hub.rs` 没有结果（或者只有 `Core::start` 起 hub 自己的那几行，照 `hub.sh` 选的办法）。
- `ui/lang/*.json` 八个文件里有 `code.starting`、`code.update.installing`。

## 不做

- 不改登录的流程本身、不加新建身份和恢复密钥的命令（P4）。
- 不改额度、钥匙链的规则（P4）。
- 不做「后台一次一件」，不动 `ai/write.rs`、`out/city.rs`（P1-19）。
- 外壳那一头的 `installer`、`Ready` 的锁不动（P1-21）。
