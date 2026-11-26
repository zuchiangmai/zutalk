状态：完成

# P1-03 地基一：日志：报告

## 改了什么

1. **新 crate `log/`（`zutalk-log`）**，根 `Cargo.toml` 的 `members` 加了 `"log"`。直接依赖只有 `tracing`（只开 `std`）和 `serde_json`；开发依赖 `tempfile`、`tokio`（测试 8 的多线程运行时）。
   - `log/src/lib.rs`：四个宏 `error!`、`warn!`、`info!`、`debug!`（第一个参数是字面量的代码，带 `const` 断言 `is_code`；每个值过一遍 `Plain`；经 `$crate::__private::tracing` 展开，用宏的 crate 不依赖 `tracing`）；`Plain`（封住的 trait，只给 `u8 u16 u32 u64 usize i32 i64 f64 bool &'static str Id<'_>`）；`Id`；`is_code`（`const fn`）、`is_plain`；`Level`（含 `code()`、`from_setting()`）；`TEXT_MOST_BYTES`。三件编不过的事写成 `compile_fail` 文档测试，每个旁边一个只差这一点、编得过的例子。
   - `log/src/line.rs`：一条事件读成平铺的字段（一次），文件和捕获共用；不 plain 的文字、和开头五个字段撞名的字段、`Debug` 值、非有限的 `f64` 一律写成 `"!"` 并计数。JSON 一行手写拼接（字符串经 `serde_json` 转义），字段按发出的次序。
   - `log/src/hear.rs`：接收者，直接实现 `tracing::Subscriber`。只收事件，span 一律给同一个号、什么都不记。
   - `log/src/sink.rs`：写文件的口。发的线程只格式化、`try_send` 进 `sync_channel(queue_lines)`；满了这一行丢掉，计数。线程 `log.writer`（`thread::Builder` 起，主人是 `Guard`）追加写；队列空了就 flush；写下一行之前（以及空闲时）丢过行就先补一行 `log.lost`。轮换：纯函数 `must_turn(written, line, most)`，改名一路往后，留 `files_kept` 个，新文件第一行 `log.turned`；启动时接着现有的文件写。`Limits`、`Open`（打开文件的口，测试换成带闸的）、`install`、`files`、`files_with`、`flush`、`lost`、`refused`、panic 钩子。
   - `log/src/capture.rs`：`capture(level) -> Seen`（`all`、`of`、`count`、`refused`、`lost`；`Line` 给 `level`、`component`、`code`、按名字取字段）、`carry`、`carrier`/`Carrier::enter`。
   - 数：`LOG_FILE_BYTES`（8 MB）、`LOG_FILES_KEPT`（5）、`LOG_QUEUE_LINES`（4096）、`LOG_FLUSH_WAIT_MS`（2000）、`TEXT_MOST_BYTES`（64）、`CAPTURE_KEEP_EVENTS`（65 536），另有内部的 `OWN_KEEP_EVENTS`（16）、`LINE_GUESS_BYTES`（192），旁边都有一句为什么。丢弃都在同一行写了 `// dropped: …`。
2. **核心**：`core/src/app/hub.rs` 的 `Config` 加字段 `log: Level`（默认 `Info`）、`Config::from_env(self)`（读 `ZUTALK_LOG`，注释写明这是全核心唯一读环境变量的地方）、`Config::logs()`（`<data>/logs`）；`Config` 每个字段都补了一行注释。核心里没有加任何事件。
3. **外壳** `app/src/main.rs`：`setup` 里先 `Config::new(..).from_env()`，再 `zutalk_log::install(&config.logs(), "zutalk", config.log, cfg!(debug_assertions))`，再 `info!("app.start", …)`，然后才开钥匙库、起核心。装不上：`eprintln!` 一行（带 `// before the log:`），照常起。`Guard` 交给 `app.manage`。另外把 `.run(ctx)` 换成 `.build(ctx)` 再 `app.run(…)`，在 `RunEvent::Exit` 时 `zutalk_log::flush(LOG_FLUSH_WAIT_MS)`：Tauri 退出时不一定丢掉托管的状态，这样队列里剩下的行一定写到盘上（行为与原来的 `run` 相同：`Builder::run` 本来就是 `build` 加 `run`）。末尾那行 `eprintln!` 加了 `// before the log:`。
4. **服务器**：
   - `server/src/main.rs` 开头装日志（`<ZUTALK_DATA>/logs/zutalk-server.jsonl`，同时打到标准错误；级别读 `ZUTALK_LOG`），然后 `server.start`。装不上同样只打一行、照常起。
   - `server/src/lib.rs`：路由改成一张表 `ROUTES`，一行是「路径、日志里的代码、谁来答」，路由由这张表折叠出来，所以路径和代码不会走样；中间件 `noted` 在请求答完以后发 `server.request`（`route`、`status`、`ms`；≥ 500 用 `warn!`；没对上任何路由是 `none`）；清扫每次 `debug!("server.sweep", ms)`。
   - `server/Cargo.toml`：axum 加特性 `matched-path`（中间件要拿到匹配的路由；不是新依赖）。
5. **规则脚本** `scripts/build/rules/events.sh`：三条，照任务写；第三条跨行取代码前先去掉注释行（`log/src/lib.rs` 的文档例子里有示范用的代码），`docs/EVENTS.md` 里只读 `## 事件` 这一节的表。只用 `grep`、`find`、`perl`、`awk`、`comm`，macOS 自带的 bash 3.2 上跑通。
6. **文档**：新 `docs/EVENTS.md`（在哪里、一行怎么读、字段表、级别、永远不在日志里的、事件表七行）；`docs/DESIGN.md` §3 加「事件」一行、§4 加 `log` 一行；`docs/build/phases/P1.md` §3.13.1 改了两句、§3.13.15 的表加了一行（见下面「和任务字面不同的地方」）。

文件：`Cargo.toml`、`Cargo.lock`、`log/**`、`core/Cargo.toml`、`core/src/app/hub.rs`、`app/Cargo.toml`、`app/src/main.rs`、`server/Cargo.toml`、`server/src/lib.rs`、`server/src/main.rs`、`server/tests/log.rs`、`scripts/build/rules/events.sh`、`docs/EVENTS.md`、`docs/DESIGN.md`、`docs/build/phases/P1.md`、本报告。

## 和任务字面不同的地方

1. **「只收 `target` 以 `zutalk` 开头的」改成「只收我们的宏造的」。** 宏把 callsite 的名字写成 `zutalk:<代码>`，接收者认这个记号（还要求后半是合法的代码）。原因：测试 crate 自己发的事件，模块路径是 `lines`、`install` 这样的名字，照字面会被扔掉（测试 9 就发不出来）；以后台架里用 `carry` 的替身也一样。要守的本来就是「字段经过编译器检查的才收」，记号说的正是这件事。`target` 没改，照旧是调用处的模块路径，写进 `component`。测试 7 里另加了一条：用 `tracing` 直接发、`target` 写成 `zutalk_core::app::hub` 的事件，同样不收。`P1.md` §3.13.1 那一句已经改了。
2. **写线程自己补的行（`log.turned`、`log.lost`）`seq` 是 0。** 号在发的线程上取（这样丢掉的行在文件里留下缺号，测试 6 要的就是这个）。如果写线程补的行也占号，它取号时已经排在队列里的行号比它小、写得比它晚，轮换删掉旧文件以后，留下的号会出现「缺号却没丢行」，测试 5 的「`seq` 连着」也就不确定了。所以这两种行不占号，缺号只表示丢行。`docs/EVENTS.md` 和 `P1.md` §3.13.1 写明了。这两种行由写线程经同一个宏发出（写线程把自己线程的接收者设成一个只记在内存里的），所以 `events.sh` 照样找得到它们的代码，它们也不会回到自己正在清空的队列里。
3. **`files_with`**：任务说写的口可以换，没有给名字。`files` 用真的文件，`files_with` 多收一个 `Open`。
4. **写不进文件的行**（打不开、写失败）也算进 `lost()`，但不发 `log.lost`（那一行同样写不进去）。

## 证据

**新的测试**（都过；`log/` 一共 14 条测试加 7 条文档测试，其中 3 条 `compile_fail`）：

- `log/tests/lines.rs`：`a_line_is_one_flat_object_of_numbers_booleans_codes_and_ids`、`text_that_is_no_code_and_no_id_is_turned_away_and_counted`、`the_grammar_of_a_code_and_of_plain_text`（也查了 `Level::from_setting`）、`only_our_own_events_are_kept`、`a_capture_keeps_the_level_it_was_asked_for`；
- `log/tests/files.rs`：`files_turn_at_their_size_and_the_oldest_goes`、`a_file_turns_before_the_line_that_would_take_it_past_its_size`（`must_turn` 的表格）、`a_writer_that_stands_still_loses_lines_and_says_how_many`、`a_file_is_written_on_after_a_restart`；
- `log/tests/capture.rs`：`a_capture_hears_this_thread_and_whatever_it_carries_the_log_to`、`two_captures_side_by_side_hear_only_their_own`、`a_capture_that_is_dropped_hears_no_more_and_the_one_before_is_back`；
- `log/tests/install.rs`：`the_log_is_installed_once_and_its_guard_ends_it`；
- `log/tests/panic.rs`：`a_panic_is_said_with_its_thread_and_place_and_never_its_message`；
- `log/src/lib.rs` 的文档测试：`String`、借来的 `&str`、`"Lane Reopen"` 三条 `compile_fail`，各配一条编得过的；
- `server/tests/log.rs`：`a_request_leaves_its_route_its_status_and_no_more`（另外查了：路径里有房间 id 的请求记成 `rooms.events`，没对上路由的记成 `none`；一个请求只留这一行）。

**编不过的原因正是那一点**（临时写成一个 example 编译，看错误）：

```text
error[E0277]: the trait bound `String: Plain` is not satisfied
error[E0080]: evaluation panicked: an event's code is two or more lower-case words joined by dots
error[E0521]: borrowed data escapes outside of function
```

**测试先失败的证明**：把接收者的「只收我们的」去掉，`only_our_own_events_are_kept` 失败（`lines.rs:164`），还原后通过。

**不偶发**：`files`、`capture`、`lines` 三个测试二进制各连跑 200 次。第一次跑时 `a_writer_that_stands_still_…` 200 次里失败了 14 次：写线程手里拿着一行等在闸前时，如果它取那一行时已经看到丢行，会先写一条 `log.lost`，之后再写一条，一共两条，而测试原来断言「正好一条」。行为是对的（每行要么写了，要么算进 `n`），改成断言一到两条、`n` 加起来；之后 `files` 连跑 1000 次，0 失败。闸放在「打开文件」上，所以写线程手里最多一行、队列四行，20 行里至少丢 15 行，结果确定，不靠时间。

**一行真的日志**（测试 1 那条事件写出来的样子；`component` 是发它的 crate）：

```json
{"t_ms":1795265625997,"seq":1,"level":"info","component":"one","event":"test.every_kind","a":1,"b":2,"c":3,"d":4,"e":5,"f":-6,"g":-7,"h":0.5,"i":true,"j":"a.code","k":"19a3f0c2b7e00d4c1f2"}
```

**服务器真的起了一次**（`ZUTALK_DATA=<临时目录> ZUTALK_LISTEN=127.0.0.1:18431 cargo run -p zutalk-server`，等到端口听上，`curl /healthz`，再停掉）。`logs/zutalk-server.jsonl` 和标准错误里是同样两行：

```json
{"t_ms":1795265616026,"seq":1,"level":"info","component":"zutalk_server","event":"server.start","version":"0.9.0"}
{"t_ms":1795265617025,"seq":2,"level":"info","component":"zutalk_server","event":"server.request","route":"healthz","status":200,"ms":0}
```

外壳没有启动（任务说不要）。

**依赖**：

```text
$ cargo tree -p zutalk-core -e normal --depth 1 | grep tracing
（没有输出）
$ cargo tree -p zutalk-log -e normal --depth 1
zutalk-log v0.9.0 (<仓库>/log)
├── serde_json v1.0.154
└── tracing v0.1.44
```

**`events.sh` 的证明**（每次试完都还原了）：

```text
--- core/src/lib.rs 末尾加 `pub fn x() { println!("x"); }`
core/src/lib.rs:15:pub fn x() { println!("x"); }
  no prints: say it with an event, or mark the line `// before the log: <why>`
exit=1
--- server/src/lib.rs 末尾加一条 `zutalk_log::info!("server.extra", n = 1u8)`
  not in docs/EVENTS.md: server.extra
exit=1
--- docs/EVENTS.md 删掉 `server.sweep` 那一行
  not in docs/EVENTS.md: server.sweep
exit=1
--- docs/EVENTS.md 加一行没人发的 `ghost.event`
  in docs/EVENTS.md, said nowhere: ghost.event
exit=1
--- core/Cargo.toml 加 `tracing = "0.1"`，core/src/lib.rs 加 `use tracing::info;`
core/Cargo.toml:46:tracing = "0.1"
  only log/Cargo.toml may name tracing
core/src/lib.rs:15:use tracing::info;
  events go through zutalk_log's macros, not tracing
exit=1
--- 都去掉
exit=0
```

**门禁**：`scripts/build/check.sh` 全绿，`passed 448, failed 0, ignored 6`（改之前 426 条），界面 27 条照过。耗时（都是编译好了以后再跑一次）：改之前 34.1 秒，改之后 35.6 秒。

**`log/src` 的行数**：一共 1038 行（`lib.rs` 259、`sink.rs` 422、`line.rs` 158、`capture.rs` 126、`hear.rs` 73），其中三成上下是文档注释和文档测试。

## 留下的，要负责人的

- 外壳的接线只由编译保证：真机第一次启动时，看 `<数据目录>/logs/zutalk.jsonl` 的第一行是不是 `app.start`（`P1.md` §4 第 1 条的真机清单，P1-26 写进去）。
- 「刷到盘上」做的是把缓冲交给系统（`flush`），不是 `fsync`：进程崩了、被杀了，交给系统的都在；整台机器断电，最后一点可能没有。要 `fsync` 的话每次空闲多一次系统调用，先不做。
- 服务器的三处 `tokio::spawn`、房间的 `broadcast`、`Duration::from_secs` 没有动，照 `P1.md` §3.13.12 分别归 P1-04（`numbers.sh` 当场还清服务器的数）和「以后」表。
- `P1.md` §3.13.15 那张「和 `PLAN.md` 字面不同」的表加了一行「认宏的记号，不按 `target` 前缀」，P1-26 并回文档时一起请负责人过目。
- 没有要负责人定的新问题。
