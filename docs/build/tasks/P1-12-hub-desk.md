# P1-12 hub 只算不做（一）：`Work`、desk、资料的读写

## 目标

做完以后：

1. 有了 `core/src/app/work.rs`：hub 要做的事交给 `Work`，立刻回去收下一条消息；结果作为消息回来。
2. 碰本人资料（记录、tape、词表、房间清单、交城的去向）的每一处读写都离开了 hub 的任务：刷新资料库、打开一条录音、改动、删除与撤销、搜索、导出、销毁音频、词表、房间、交城的登记、补写的清点。
3. 连着的改动只引起一两次重读；搜索只认最后一次；导出写文件不占 desk。
4. `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` 通过：一件永远做不完的活，挡不住字幕，也挡不住命令。

身份、钥匙库、设置、设备、更新、剪贴板、启动，以及规则脚本，是 P1-13。

## 背景

- 设计：`docs/build/phases/P1.md` §3.8，先读。`PLAN.md` §4.1 的 hub、L3 两行，§4.2 第二行，`CLAUDE.md` 的规则「The hub and the live path do no IO」。
- `core/src/app/hub.rs` 里现在在 hub 的任务上做 IO 的地方（按函数名找）：
  - `refresh()`：`shelf.list()`、`shelf.read(opened)`、`glossary::read`、`shelf.kept(FILING)`、每行一次 `tape::kept`、`keys.has_own`；`take()` 在几乎每条消息之后都调它；
  - `retain()`（`retain::sweep` 删 tape）、`resume()`（`shelf.kept(FILING)`）、`recover()`（每条草稿一次 `shelf.about`）、`rooms()`（`shelf.kept(SHARE)`、`keys::derive`）；
  - 命令：`Change`（`apply`）、`Remove`、`Restore`、`Search`、`Export`（读、渲染、`fs::write`）、`DestroyAudio`、`Term`、`File`（`city::begin`）、`Share`（`shelf.read`、`room::pages`，任务里还有 `shelf.put`）、`Unshare`、`Start`（`shelf.put` 写录音的记录）；
  - `sign()` 建 `Shelf`，hub 一直拿着 `Arc<Shelf>`。
- `core/src/app/recording.rs`（P1-10）：一场录音；封 tape 以后的整场转写（`write`）还在 `hub.rs`。
- 邮箱的两种发法 `must`、`may`（P1-08）。
- `core/src/record/shelf.rs`：读写都是同步的文件操作。`Pack::read` 遇到写了一半的最后一行会跳过，所以一边写草稿一边读是安全的。

## 做法

### `Work` 与 desk

1. `Work` 拿着：`Arc<Config>`、HTTP 客户端、邮箱的发信口、`said` 寄存器、现在登录的人的东西（`Shelf`、`Ours`、钥匙链）。hub 不再有 `shelf` 这个字段；「有没有人登录」看 `state.me`。
2. **desk**：一条有上限的队列（64）和一个任务。任务一次取一件活，用一个 `spawn_blocking` 做完，把结果用 `must` 发回邮箱，再取下一件。所以 desk 上的活严格按 hub 给的次序做。
3. `Work` 的每个方法只是把一件活排进去，返回 `Result<(), &'static str>`；排不进去是 `busy`。活做失败了，结果消息里带着代码，hub 收到后 `said.say(code)`。
4. 谁登录了变了：`work.sign(me)` 也是 desk 上的一件活（换掉 `Work` 里的 `Shelf` 等），所以它之前排的活用旧的人，之后的用新的人。

### 搬过去的东西

5. **改动**（`Change`、`Remove`、`Restore`、`Term`、`DestroyAudio`、交城的登记 `city::begin`、房间清单的增删）：各是 desk 上的一件活。不需要 IO 的检查留在 hub 上先做：有没有登录、是不是正在录、是不是正在写（`busy`）、导出器在不在表里、有没有城的日历。
6. **刷新**：一件 desk 上的活，读出资料库的行（含每条有没有音频）、打开着的那一条（含它交城的去向）、词表、钥匙的来源，需要时顺手做清扫（`retain`）。结果一条消息回来，hub 把它放进 `state`。
   - 合并：改动的结果回来，hub 把「改过的次数」加一；没有刷新在路上，就排一次，并记下排的时候是第几次；刷新回来时，如果次数又涨了，再排一次。
   - `take()` 里「每条消息之后都 `refresh()`」去掉：只有会改变读到的东西的消息才记一笔。
7. **搜索**：不上 desk（它慢，不该挡住改动）。每次搜索带一个递增的序号，在自己的 `spawn_blocking` 里做；结果回来时序号不是最新的就丢掉。
8. **导出**：desk 上读出录音、渲染成文字（渲染是纯函数）；然后在**另一个** `spawn_blocking` 里写到导出的路径，结果发回。导出的路径可能是慢的盘，也可能永远写不完：它不许占着 desk。
9. **房间**：读清单、算链接、把录音分页，在 desk 上；对服务器的请求在各自的任务里；任务里原来的 `shelf.put` 改成再排一件 desk 的活。
10. **开始录音**：写录音那条记录（`About`）不在 hub 上做：交给录音任务，作为它的第一件事（在阻塞线程上）；写不成就说 `shelf`，这一场结束。hub 照旧立刻把 `session` 设上。
11. **清点欠着的**（`recover`、`resume`）：desk 上的活，回来的是「哪些录音欠整场转写」「哪些交城没交完」；hub 据此标上进度、让 `Work` 去起那些后台的任务。整场转写的任务（现在 `hub.rs` 里的 `Task::finish`、`write`）和交城、搬运的任务也搬进 `work.rs`：它们本来就不在 hub 的任务上，现在连代码也不在 hub 的文件里。

### 不变的

- 每个命令对外的结果不变：成功的改动读得回，失败的说同一个代码。变的只是代码不再同步地出来，而是稍后出来；现有的测试本来就是等它出现。
- `retain` 的规则、`apply` 的逻辑、导出器、分页，一行不改，只是换了被调用的地方。

## 先写的测试

1. `core/tests/liveness.rs`：拿掉 `i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands` 的 `ignore`。
2. `core/tests/app.rs`：`a_burst_of_changes_is_read_back_once_or_twice`。一条录音，单线程运行时，一口气发 50 次改标题（中间不 `await`）。最后标题是第 50 个；这期间 `outlet.state` 的序号涨了不超过 3。（现在的代码每条消息重读并可能重发一次。）
3. `an_export_writes_what_was_changed_just_before_it`：改标题，紧接着导出：导出的文件以新标题开头。
4. `a_search_asked_again_answers_only_the_last`：两条录音，一条里有 `alpha`，一条里有 `beta`。连发两次搜索；等到 `state.hits` 不空：它是 `beta` 的那一行。
5. `a_desk_that_is_full_turns_commands_away_out_loud`：一口气发 200 次改标题。等安静下来：说了 `busy` 的次数加上真正写进去的记录数，正好是 200。
6. `a_recording_whose_record_cannot_be_written_ends_saying_so`：把这个人的记录目录换成一个文件，再 `Start`：说 `shelf`，`session` 回到 `None`，没有留下 tape。
7. 现有的 hub 测试（`app.rs`、`glossary.rs`、`retain.rs`、`city.rs`）和 `migrate.rs` 不改断言，全部通过。

## 验收

- `scripts/build/check.sh` 全绿。`core/tests/liveness.rs` 里不再有 `ignore = "P1-12`。
- `grep -n "Shelf\|shelf\.\|tape::\|glossary::read\|glossary::put\|retain::\|room::pages\|city::begin" core/src/app/hub.rs` 没有结果。
- 报告里列出：`hub.rs` 里还剩哪些 IO（应该只有身份、钥匙库、设置、设备、更新、剪贴板这几样，P1-13 的）；`hub.rs`、`work.rs`、`recording.rs` 各多少行。

## 不做

- 不搬身份、钥匙库（`keys::` 的那些函数）、`settings.json`、`audio::devices()`、装更新、剪贴板、启动（P1-13）。
- 不加规则脚本 `hub.sh`，不做「后台一次一件」（P1-13）。
- 不建索引，不改搜索的算法，不把 `library` 移出 `State`（P2）。
- 不改 `ai/write.rs`、`out/city.rs`、`record/migrate.rs` 里面的做法。
