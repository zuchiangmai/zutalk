# P1-19 hub 只算不做（一）：`Work`、desk、资料的读写

## 目标

做完以后：

1. 有了 `core/src/app/work.rs`：hub 要做的事交给 `Work`，立刻回去收下一条消息；结果作为消息回来。
2. 碰本人资料（记录、tape、词表、房间清单、交城的去向）的每一处读写都离开了 hub 的任务：刷新资料库、打开一条录音、改动、删除与撤销、搜索、导出、销毁音频、词表、房间、交城的登记、补写的清点。
3. 连着的改动只引起一两次重读；搜索只认最后一次；导出写文件不占 desk。
4. 每件活在日志里有开始和结束（种类、序号、等了多久、做了多久、结果）；一件卡住的活，看日志和任务表就知道是哪一件、卡了多久。
5. `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` 通过，连同它「看得见」的断言：一件永远做不完的活，挡不住字幕，也挡不住命令。

身份、钥匙库、设置、设备、更新、剪贴板、启动，以及规则脚本，是 P1-20。

## 背景

- 设计：`docs/build/phases/P1.md` §3.8，先读；§3.6（`work` 这棵子树上有什么）、§3.7（`desk` 这条队列）、§3.13.3（`job.start`、`job.done`）、§2 第二张表里这一件管的两行。`PLAN.md` §4.1 的 hub、L3 两行，§4.2 第二行，`CLAUDE.md` 的规则「The hub and the live path do no IO」。
- `core/src/app/hub.rs` 里现在在 hub 的任务上做 IO 的地方（按函数名找）：
  - `refresh()`：`shelf.list()`、`shelf.read(opened)`、`glossary::read`、`shelf.kept(FILING)`、每行一次 `tape::kept`、`keys.has_own`；`take()` 在几乎每条消息之后都调它；
  - `retain()`（`retain::sweep` 删 tape）、`resume()`（`shelf.kept(FILING)`）、`recover()`（每条草稿一次 `shelf.about`）、`rooms()`（`shelf.kept(SHARE)`、`keys::derive`）；
  - 命令：`Change`（`apply`）、`Remove`、`Restore`、`Search`、`Export`（读、渲染、`fs::write`）、`DestroyAudio`、`Term`、`File`（`city::begin`）、`Share`（`shelf.read`、`room::pages`，任务里还有 `shelf.put`）、`Unshare`、`Start`（`shelf.put` 写录音的记录）；
  - `sign()` 建 `Shelf`，hub 一直拿着 `Arc<Shelf>`。
- `core/src/app/recording.rs`（P1-16）：一场录音；封 tape 以后的整场转写（`write`）还在 `hub.rs`。
- 邮箱的三种发信人（P1-13）：命令 `Full::Reject`、结果 `Full::Wait`、进度 `Full::Drop`。`command.done`、`hub.turn.slow`（P1-13）已经在发：这一件做完以后，资料那一类命令不该再有 `hub.turn.slow`。
- `core/src/record/shelf.rs`：读写都是同步的文件操作。`Pack::read` 遇到写了一半的最后一行会跳过，所以一边写草稿一边读是安全的。

## 做法

### `Work` 与 desk

1. `Work` 拿着：`Arc<Config>`、HTTP 客户端、邮箱的发信口（结果的和进度的两头）、`said` 寄存器、自己的子树（`root.child("work")`）、现在谁在签（`Who`，P1-06）和由它来的东西：`who.me()` 的 `Shelf`、`who.person()` 的 `Ours`、钥匙链。hub 不再有 `shelf`、`who` 这两个字段：「有没有人登录」看 `state.me`，「有没有钥匙可取」看 `state.keys`。`Who::Nobody(code)` 时没有书架：要书架的活以那个代码失败。
2. **desk**：一条有名字的队列（`queue::bounded(&stats, "desk", DESK_QUEUE_JOBS, Full::Reject)`，64）和 `work` 子树上的一个任务（`desk`）。任务一次取一件活，用一个 `tree.blocking("desk.job", …)` 做完，把结果从 `Full::Wait` 的那一头发回邮箱，再取下一件。所以 desk 上的活严格按 hub 给的次序做。
   - **每件活是一个值**：`Job` 是一个枚举（`Change`、`Remove`、`Refresh`、`Export`…），有 `kind() -> &'static str`；排进去的时候带上一个递增的序号和排进去的时刻。活做的时候 panic 了（`blocking` 给回 `Broke`）：结果是代码 `panic`，desk 接着做下一件。
   - **事件**：取出一件：`info!("job.start", kind, seq, waited_ms)`；做完：`job.done`（`kind`、`seq`、`code`、`ms`），`code` 不是 `ok` 时用 `warn!`。`waited_ms`、`ms` 同时记进分布 `desk.wait_ms`、`desk.job_ms`；计数 `jobs.done`、`jobs.failed`。事件只带种类和序号：改了什么标题、搜了什么词、导出到哪里，都不进日志。
3. `Work` 的每个方法只是把一件活排进去（`offer`），返回 `Result<(), &'static str>`；被退回是 `busy`（`queue.full` 由队列自己发；hub 把 `busy` 当作这条命令的结果，所以 `command.done` 的 `code` 是 `busy`）。活做失败了，结果消息里带着代码，hub 收到后 `said.say(code)`。
4. 谁在签变了（登录、登出）：`work.sign(who, why)` 也是 desk 上的一件活（换掉 `Work` 里的 `Who`、`Shelf` 等），所以它之前排的活用旧的身份，之后的用新的。`identity.who`（P1-06）从这里发。已经起来的录音任务不受影响：它拿着开始时的书架，签到底。

### 搬过去的东西

5. **改动**（`Change`、`Remove`、`Restore`、`Term`、`DestroyAudio`、交城的登记 `city::begin`、房间清单的增删）：各是 desk 上的一件活。不需要 IO 的检查留在 hub 上先做：这条命令要不要登录而没有人登录（`Command::needs_person` 对 `state.me`，P1-06）、是不是正在录、是不是正在写（`busy`）、导出器在不在表里、有没有城的日历。
6. **刷新**：一件 desk 上的活，读出资料库的行（含每条有没有音频）、打开着的那一条（含它交城的去向）、词表、钥匙的来源，需要时顺手做清扫（`retain`）。结果一条消息回来，hub 把它放进 `state`。
   - 合并：改动的结果回来，hub 把「改过的次数」加一；没有刷新在路上，就排一次，并记下排的时候是第几次；刷新回来时，如果次数又涨了，再排一次。「该不该排」是 hub 里一个不做 IO 的小函数；每合并掉一次（脏了但没有另排）计数 `refresh.merged` 加一。
   - `take()` 里「每条消息之后都 `refresh()`」去掉：只有会改变读到的东西的消息才记一笔。
7. **搜索**：不上 desk（它慢，不该挡住改动）。每次搜索带一个递增的序号，在自己的 `tree.blocking("search", …)` 里做（外面一个 `work` 子树上的任务 `search`）；结果回来时序号不是最新的就丢掉。它同样有 `job.start`、`job.done`（`kind = "search"`）；被丢掉的那一次 `code = "stale"`，计数 `search.stale` 加一。
8. **导出**：desk 上读出录音、渲染成文字（渲染是纯函数）；然后在**另一个**阻塞调用里写到导出的路径（`work` 子树上的任务 `export.write`，里面 `tree.blocking("export.write", …)`），结果发回。导出的路径可能是慢的盘，也可能永远写不完：它不许占着 desk。它有自己的 `job.start`、`job.done`（`kind = "export.write"`）：写不完的时候，日志里有开始没有结束，`Tree::running()` 里有这一行和它已经跑了多久。
9. **房间**：读清单、算链接、把录音分页，在 desk 上；对服务器的请求在各自的任务里（`work` 子树上，名字 `net.share`、`net.unshare`、`slot`；每个一对 `job.start`、`job.done`）；任务里原来的 `shelf.put` 改成再排一件 desk 的活。
10. **开始录音**：写录音那条记录（`About`）不在 hub 上做：交给录音任务，作为它的第一件事（在阻塞线程上）；写不成就说 `shelf`，这一场结束。hub 照旧立刻把 `session` 设上。要开的流照 P1-07：hub 用 `state.keys` 算 `lane::captions`，把结果一起交过去；录音任务拿到的书架和钥匙链是 `Work` 此刻的那一份。
11. **清点欠着的**（`recover`、`resume`）：desk 上的活，回来的是「哪些录音欠整场转写」（照 `write::owed`，P1-07；因为没有钥匙而等着的不在里面，有几条就发一条 `write.skipped`）「哪些交城没交完」（没有人登录时没有）；hub 据此标上进度、让 `Work` 去起那些后台的任务。整场转写的任务（现在 `hub.rs` 里的 `Task::finish`、`write`）和交城、搬运的任务也搬进 `work.rs`：它们本来就不在 hub 的任务上，现在连代码也不在 hub 的文件里。搬的时候它们都从 `work` 子树上起，名字 `job.write`、`job.file`、`job.move`；每件一对 `job.start`、`job.done`（`kind` 是 `write`、`file`、`move`；「一次一件」和「等一等再试」的事件是 P1-21）。

### 不变的

- 每个命令对外的结果不变：成功的改动读得回，失败的说同一个代码。变的只是代码不再同步地出来，而是稍后出来；现有的测试本来就是等它出现。
- `retain` 的规则、`apply` 的逻辑、导出器、分页，一行不改，只是换了被调用的地方。

## 先写的测试

1. `core/tests/liveness.rs`：拿掉 `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` 的 `ignore`。它「看得见」的断言照 `P1.md` §2 第二张表：堵着的时候有 `job.start`（`kind = "export.write"`）而没有同一个 `seq` 的 `job.done`；`command.done`（`Pause`、`Resume`、`Change`，`code = "ok"`）各一条；`Tree::running()`（从 `core` 的根上问；给台架一个取它的口）里有 `export.write` 这一行。放开以后：`job.done`（`kind = "export.write"`、`code = "ok"`）。这一条是多线程运行时：事件能到测试线程的捕获，靠的是这些任务都从树上起。
2. `core/tests/app.rs`：`a_burst_of_changes_is_read_back_once_or_twice`。一条录音，单线程运行时，一口气发 50 次改标题（中间不 `await`）。最后标题是第 50 个；这期间 `outlet.state` 的序号涨了不超过 3。（现在的代码每条消息重读并可能重发一次。）**看得见**：`job.done`（`kind = "change"`）50 条，`seq` 递增；`job.done`（`kind = "refresh"`）不超过 3 条；计数 `refresh.merged` 加上刷新的次数不少于 50。
3. `an_export_writes_what_was_changed_just_before_it`：改标题，紧接着导出：导出的文件以新标题开头。
4. `a_search_asked_again_answers_only_the_last`：两条录音，一条里有 `alpha`，一条里有 `beta`。连发两次搜索；等到 `state.hits` 不空：它是 `beta` 的那一行。
5. `a_desk_that_is_full_turns_commands_away_out_loud`：一口气发 200 次改标题。等安静下来：说了 `busy` 的次数加上真正写进去的记录数，正好是 200。**看得见**：`queue.full`（`queue = "desk"`、`policy = "reject"`）至少一条；`command.done` 里 `code = "busy"` 的条数等于说了 `busy` 的次数，也等于统计里 `desk` 这条队列的 `full`；`job.done`（`kind = "change"`）的条数等于写进去的记录数。
6. `a_recording_whose_record_cannot_be_written_ends_saying_so`：把签名的那个身份的记录目录（测试台不登录时是本机身份的）换成一个文件，再 `Start`：说 `shelf`，`session` 回到 `None`，没有留下 tape。**看得见**：`recording.end`（`code = "shelf"`），级别 `error`；`recording.phase` 从 `Opening` 走到 `Ended`。
6a. `a_job_that_panics_is_one_failed_job_and_the_desk_goes_on`：让一件活在做的时候 panic（给 `Work` 的测试留一个口，或者用一个会让现有代码 panic 的替身端口；找不到干净的办法就在 desk 这一层用替身的活来测）：`job.done`（`code = "panic"`）一条，说了 `panic`；下一件活照常做完。
6b. `no_job_says_what_it_was_about`：改标题成一个认得出的词、搜另一个认得出的词、导出到一个带着认得出的词的路径：整个捕获里找不到这三个词。
7. 现有的 hub 测试（`app.rs`、`glossary.rs`、`retain.rs`、`city.rs`）和 `migrate.rs` 不改断言，全部通过。

## 显式与可观测

- **事件**：`job.start`、`job.done`。写进 `docs/EVENTS.md`，并列出 P1 里全部的 `kind`。`queue.full`（`desk`）、`task.*`（`tree = "work"`）是地基发的。
- **统计**：计数 `jobs.done`、`jobs.failed`、`search.stale`、`refresh.merged`；分布 `desk.wait_ms`、`desk.job_ms`；队列 `desk` 一行。
- **数**：`DESK_QUEUE_JOBS`（64：一次连着的操作几十件；满了说明磁盘卡住了，该让人知道）。
- **队列**：`desk`，`Full::Reject`。结果回邮箱走 `Full::Wait` 的那一头。
- **任务**：`core` 根上的子树 `work`；上面的 `desk`、`search`、`export.write`、`net.*`、`slot`、`job.write`、`job.file`、`job.move`。阻塞调用都是 `tree.blocking`。
- **欠账**：`spawn.sh` 里 `core/src/app/hub.rs` 那一行减掉这一件搬走的各处（交城、房间、槽、清点、搬运）；`core/src/app/work.rs` 是新文件，没有欠账。剩下的（登录、认领、造身份、日历）P1-20 还。
- **P1-05 报告里指给这一件的丢弃**：在这里处理。
- **读日志的人**：一条命令现在是两段：`command.done`（hub 收下了）和之后的 `job.done`（做完了、结果）。`docs/EVENTS.md` 里写明这一点。

## 验收

- `scripts/build/check.sh` 全绿。`core/tests/liveness.rs` 里不再有 `ignore = "P1-19`。
- `grep -n "Shelf\|shelf\.\|tape::\|glossary::read\|glossary::put\|retain::\|room::pages\|city::begin" core/src/app/hub.rs` 没有结果。
- 报告里列出：`hub.rs` 里还剩哪些 IO（应该只有身份、钥匙库、设置、设备、更新、剪贴板这几样，P1-20 的）；`hub.rs`、`work.rs`、`recording.rs` 各多少行；测试第 2 条跑完以后分布 `hub.turn_us` 的 `p99` 和 `hub.turn.slow` 的条数（和 P1-13 报告里的并排放：资料那一类命令应该不再超预算）。

## 不做

- 不搬身份、钥匙库（`keys::` 的那些函数）、`settings.json`、`audio::devices()`、装更新、剪贴板、启动（P1-20）。
- 不加规则脚本 `hub.sh`（P1-20），不做「后台一次一件」（P1-21）。
- 不建索引，不改搜索的算法，不把 `library` 移出 `State`（P2）。
- 不改 `ai/write.rs`、`out/city.rs`、`record/migrate.rs` 里面的做法。
