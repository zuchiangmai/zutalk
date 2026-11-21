# P1-13 hub 只算不做（二）：启动、身份、钥匙、设置、更新；规则脚本

## 目标

做完以后：

1. `core/src/app/hub.rs` 里没有任何 IO：不读写磁盘、不碰钥匙库、不发请求、不问系统有哪些设备、不装更新。处理一条消息的函数都是同步的，整个文件里 `.await` 只有收信那一处。
2. `scripts/build/rules/hub.sh` 守着这一条，以后加不回来。
3. 后台的活（整场转写、交城、搬运）一次只做一件；一件在等着再试的时候不占着位置。
4. I2 的三条测试都在门禁里通过。

## 背景

- 设计：`docs/build/phases/P1.md` §3.8。`PLAN.md` §4.1 的 hub、L4 两行；`CLAUDE.md`：「The hub and the live path do no IO」「The shell never blocks」。
- P1-12 之后 `hub.rs` 里还剩的 IO（按名字找）：
  - `Hub::run` 的开头：读 `settings.json`、`audio::devices()`（系统调用，可能很慢）、`keys::kept()`、`carry()`（读旧版留下的邀请文件）；
  - `sign()`：`keys::unsaved()`；起认领邀请的任务（`chain.claim`、`keys::carried`）；
  - 命令：`Setup`（写 `settings.json`）、`SignIn`（`SignIn::begin`，再起任务等结果）、`SignOut`（`keys::set`）、`CopyRotation`（`keys::rotation()`，再调外壳给的剪贴板）、`RotationSaved`（`keys::saved()`）、`OwnKey`（`keys.own`，写钥匙库）、`Update`（同步调 `config.install`，它会装更新并重启应用）；
  - `carry()` 里起造身份的任务（`keys::create`）。
- `core/src/app/work.rs`（P1-12）：`Work`、desk、后台的任务都在这里。
- `docs/DESIGN.md` §5.10：现在「装和重启都在枢纽处理这条命令时同步完成，中间不会开始新的录音」。这一句要换一种办法保住。
- `core/src/rt/supervise.rs` 的 `again`：会过去的失败，先等一分钟，再越等越久，最多一小时。

## 做法

### 启动

1. `Core::start` 起 hub 以后，让 `Work` 做「启动」这件活（desk 上）：读设置、读身份、读旧邀请。结果一条消息回来（设置、登录的是谁的 DID、恢复密钥存没存、途中要说的代码）。`Work` 自己留下身份的钥匙，hub 只知道 DID。
2. 设备列表另起一个 `spawn_blocking` 去问，什么时候回来什么时候放进 `state.devices`：它卡住不许拖住启动。
3. 启动的结果到达之前来的命令：`starting`，拒绝。启动之后第一次发布 `State`（和现在一样：界面拿到第一份状态之前没有东西可点）。

### 身份与钥匙

4. 登录、登出、造身份、认领邀请、存恢复密钥、复制恢复密钥、设自己的钥匙：都是 `Work` 的方法。碰钥匙库和本机文件的部分在 desk 上，等中继、等目录、等服务器的部分在各自的任务里。结果回来的消息只带 hub 需要的东西（DID、登录要显示的码、存没存、成功或代码），**钥匙不经过 hub 的邮箱**。
5. 换了人：`Work` 先在 desk 上把自己的 `Shelf` 等换掉，再告诉 hub；hub 收到后照旧清点、刷新（P1-12 的那几件活）。
6. 复制恢复密钥：desk 上读出来，直接交给外壳给的 `clip`，回来的只是 `copied`、`clip` 或 `rotation.none`。

### 设置

7. `Setup`：hub 上先做不需要 IO 的检查（栏数），然后**立刻**把 `state.setup` 换成新的（这是本人此刻的选择，下一条 `Start` 就要用它），再让 desk 把它存下。存不下：说 `settings`，内存里的选择不退回。

### 更新

8. `Update`：正在录音 → `update.recording`（不变）。否则 hub 记下「正在装」，让 `Work` 在一个 `spawn_blocking` 里调 `config.install`。装上了应用会重启；它要是返回了，回来一条消息：hub 取消「正在装」，说 `update.install`。
9. 「正在装」期间 `Start`、`Import`：`update.installing`，拒绝。这样「装的中间不会开始新的录音」仍然成立，而 hub 没有被占住。

### 后台一次一件

10. `Work` 里一个 `Semaphore(1)`。整场转写、交城、搬运在真正干活之前拿许可，干完放掉。**每一次尝试拿一次**：`again` 在两次尝试之间的等待里不拿着许可，不然一条连不上服务的录音会把别的都挡一小时。
11. 停止以后封 tape、写长度不是后台，不拿许可（P1-10 已经把它放在录音任务里）。

### 规则脚本

12. `scripts/build/rules/hub.sh`，对 `core/src/app/hub.rs`：
    - 不许出现：`std::fs`、`fs::`、`tokio::fs`、`File::`、`reqwest`、`spawn_blocking`、`tokio::spawn`、`Shelf`、`tape::`、`audio::devices`、`keys::` 后面跟着函数调用（类型名如 `keys::Service` 可以）、`.install)(`、`.clip)(`；
    - `.await` 只许出现一次；
    - 违反就打印「文件:行」并以非零退出。
    名单可以按实际的写法调整，但要在报告里证明它抓得住：把 P1-12 之前的 `hub.rs`（`git show`）喂给它，它要报出一长串。

### 文档

13. `docs/DESIGN.md` §5.10 里「同步完成」那一句改成现在的办法。

## 先写的测试

`core/tests/app.rs`：

1. `a_command_before_the_core_is_ready_is_turned_away`：单线程运行时，`Core::start` 之后不 `await` 就 `send(Start)`：说 `starting`；等第一份 `State` 出来以后再 `Start`，正常。
2. `a_setting_takes_effect_at_once_and_is_kept`：`Setup`（字幕语言改成两种）紧接着 `Start`，中间不等：这一场的 `Frame` 有两栏。重新起一个核心：设置还在。
3. `settings_that_cannot_be_saved_say_so_and_still_hold`：把 `settings.json` 的位置占成一个文件夹：说 `settings`；`state.setup` 是刚选的。
4. `nothing_is_recorded_while_an_update_is_being_put_in_place`（真实时钟）：`install` 的替身卡在一道闸上。`Update` 之后 `Start` → `update.installing`；这期间 `Pause` 这类命令照样有回答（hub 没被占住）；放开闸（替身返回，等于没装上）→ `update.install`，之后 `Start` 正常。现有的 `an_update_is_installed_only_while_nothing_is_recording` 并进来或保留，断言不放松。
5. `the_recovery_key_goes_to_the_clipboard_and_through_nothing_else`（已有）继续通过，并加一条断言：途中 `outlet.state`、`outlet.said` 的任何一份序列化结果里都没有那把钥匙。

`core/tests/retain.rs` 或 `core/tests/app.rs`（真实时钟，整场转写的替身是 wiremock）：

6. `transcripts_are_written_one_at_a_time`：登录时有三条欠着整场转写的草稿。替身给上传的回答加 200 毫秒的延迟，并记下每个请求的起止：没有两个上传在时间上重叠；三条最后都写好了。
7. `one_that_waits_to_try_again_does_not_hold_up_the_next`：替身对第一条的上传回 503（会过去的失败），对第二条正常：第二条写好了，第一条的进度写着在等什么。

`core/tests/liveness.rs`：

8. `i2_background_work_runs_beside_a_recording`、`i2_a_job_that_never_ends_…`、`i2_a_tape_that_stands_still_…` 三条都不带 `ignore`，都通过。

## 验收

- `scripts/build/check.sh` 全绿（`hub.sh` 在其中）。
- 报告里：`hub.sh` 对旧的 `hub.rs` 的输出（证明它抓得住）；`hub.rs` 的 `use` 列表；`hub.rs`、`work.rs`、`recording.rs` 各多少行。
- `ui/lang/*.json` 八个文件里有 `code.starting`、`code.update.installing`。

## 不做

- 不改登录的流程本身、不加新建身份和恢复密钥的命令（P4）。
- 不改额度、钥匙链的规则（P4）。
- 不把 `ai/write.rs`、`out/city.rs` 里面写记录的地方改成阻塞线程：它们在后台的任务里，不在 hub 和现场的路上；报告里记一笔，留给 P2、P6 的审查看。
- 外壳那一头的 `installer`、`Ready` 的锁不动（P1-14）。
