# P1-03 地基一：日志。事件只有代码和数字，由编译器管

## 目标

做完以后：

1. 仓库里有一个新的 crate `log/`（包名 `zutalk-log`）。发事件只有它的四个宏一条路；字段的值只能是数、布尔、源码里写死的代码和明写的 id：一句话、一个名字**编不过**。
2. 事件写成一行一个 JSON，落在 `<数据目录>/logs/zutalk.jsonl`，按大小轮换；发事件的人从不等磁盘。
3. 外壳和服务器在启动的第一步装上它；测试能装一个只看得见自己的捕获。
4. 规则脚本 `events.sh` 和 `docs/EVENTS.md` 守着：事件只走这一条路，代码和文档一一对应。

这是 `PLAN.md` §4.5.2 的第一条（事件日志）。现在核心、外壳、服务器里一行日志也没有。

## 背景

- 设计在 `docs/build/phases/P1.md` §3.13.1（日志在哪里、谁装）、§3.13.2（名字、字段、怎么守）、§3.13.3（事件表里标着 P1-03 的七行）、§3.13.13（测试怎么看见事件）。先读这四节。原则在 `CLAUDE.md` 第 10 条和「Events, not prints」一条。
- 工作区：根 `Cargo.toml` 的 `members` 是 `zu`、`core`、`server`、`app`。`tracing`（0.1.44）已经在 `Cargo.lock` 里，是别的库带进来的；没有任何 crate 直接依赖它。
- `app/src/main.rs`：`setup` 里先拿数据目录，再 `keys::open_store`，再起核心；末尾有一行 `eprintln!("ZuTalk could not start: …")`。
- `server/src/main.rs`：从环境变量读配置；`server/src/lib.rs` 的 `app()` 里是路由表，`serve()` 里有每分钟的清扫。
- `core/src/app/hub.rs`（P1-02 之后的路径）：`Config`、`Config::new`。
- 服务器不能依赖核心（核心的测试用服务器），所以日志不能放在 `core/src/rt/` 里：这就是它单独成一个 crate 的原因。

## 做法

### 1. crate `log/`

`log/Cargo.toml`：包名 `zutalk-log`；依赖只有 `tracing`（`default-features = false, features = ["std"]`，版本照 `Cargo.lock`）和 `serde_json`。根 `Cargo.toml` 的 `members` 加 `"log"`。`core`、`app`、`server` 各加对它的依赖。**别的 crate 的 `Cargo.toml` 里不许出现 `tracing`。**

接口照 `P1.md` §3.13.1 末尾那一段（名字可以调整，性质不能少）：

- **四个宏** `error!`、`warn!`、`info!`、`debug!`：第一个参数是字面量的事件代码，后面是 `名字 = 值`。展开成一个 `tracing` 事件（`target` 不改，就是调用处的模块路径），并带一行 `const` 断言：代码不合 `is_code` 就编不过。宏里引用 `tracing` 走 `$crate` 下面一个隐藏的再导出：用宏的 crate 自己不依赖 `tracing`。
- **`Plain`**：只给 `u8`、`u16`、`u32`、`u64`、`usize`、`i32`、`i64`、`f64`、`bool`、`&'static str`、`Id<'_>` 实现。宏把每个值过一遍 `Plain`。`String`、借来的 `&str`、`Path`、`Debug` 的东西都没有实现：编不过。
- **`Id<'a>(pub &'a str)`**：应用自己造的 id（录音的 id、窗口的标签）。造的时候不查（它可能在热的路上），写的时候查。
- **文法**：`is_code`（`const fn`）：两段或更多段小写的词用点连起来，每段是 `[a-z][a-z0-9_]*`。`is_plain`：只有 `[A-Za-z0-9._:/-]`，不超过 `TEXT_MOST_BYTES`（64）字节，可以是空的。
- **接收者**：直接实现 `tracing::Subscriber`（不加 `tracing-subscriber`）。只收事件，不管 span（`new_span` 给一个固定的号，其余是空的）。`enabled`：`target` 以 `zutalk` 开头、是事件、级别够。别的库发的一律不收。
- **一行**：`t_ms`（墙上时钟）、`seq`（这个接收者的第几行）、`level`、`component`（`target`）、`event`，然后是字段，平铺。字段的名字不许和这五个撞；撞了当作被退回。字符串值过不了 `is_plain` 的，写成 `"!"`，`refused` 加一。
- **写文件的口**：
  - 发的线程只做格式化和 `try_send`。队列是 `std::sync::mpsc::sync_channel(LOG_QUEUE_LINES)`（4096）；满了这一行丢掉，`lost` 加一。
  - 一个线程 `log.writer`（用 `thread::Builder` 起，名字就是这个）往文件追加；队列空了就刷到盘上。写下一行之前如果 `lost` 涨过，先补一行 `log.lost`（`n`）。
  - 轮换：写之前问一个纯函数「加上这一行会不会超过 `file_bytes`」。会：`zutalk.jsonl` → `zutalk.1.jsonl` → … 依次改名，一共留 `files_kept` 个，最旧的删掉；新文件的第一行是 `log.turned`。启动时接着现有的文件写。
  - 上限是一个值 `Limits { file_bytes, files_kept, queue_lines }`，`Default` 给 `LOG_FILE_BYTES`（8 MB）、`LOG_FILES_KEPT`（5）、`LOG_QUEUE_LINES`；测试给小的。
  - 往哪里写是这个 crate 里面的一个口（给它一个「打开下一个文件」的函数），测试能换成一个带闸的，好确定地把队列堵满。
  - `echo`：同一行也打到标准错误。
- **`install(dir, name, level, echo) -> io::Result<Guard>`**：建目录、起写线程、设成全进程默认的接收者、挂 panic 钩子。第二次调用是错误。`Guard` 丢掉时把队列里剩下的写完（最多等 `LOG_FLUSH_WAIT_MS`，2000），停掉写线程。
- **`files(dir, name, level, limits) -> io::Result<(Scope, Guard)>`**：同一个写文件的口，但不设成全进程的；`scope.enter()` 把它设成这个线程此刻的接收者，守卫丢掉就撤掉。隐私测试用它（P1-08）。
- **`flush(wait_ms) -> bool`**：让全进程的那一个把到此为止的都写到盘上，最多等这么久。没有装就是 `true`。
- **`lost()`、`refused()`**：全进程那一个丢了几行、退回了几个字段。
- **panic 钩子**：一条 `app.panic`：线程的名字（`Id`）、文件（`Id`）、行号。**不带消息。** 然后 `flush`，再调原来的钩子。
- **捕获**：`capture(level) -> Seen`。把一个只记在内存里的接收者设成这个线程此刻的；`Seen` 丢掉就撤掉。最多留 `CAPTURE_KEEP_EVENTS`（65 536）条，多了丢最旧的、计数。`Seen` 给：全部（按次序）、按代码取、数个数、`refused()`、`lost()`；一条事件给：级别、`component`、代码、按名字取一个字段（数、布尔或文字）。
- **带着走**：`carry(future)`（任务：谁起的它，它就说给谁听）和 `carrier()`（线程和阻塞调用：`carrier.enter()` 给一个守卫）。P1-05 的树用它们。底下是 `tracing` 的 `WithSubscriber` 和 `dispatcher::with_default`。
- 这个 crate 里每个数都是带单位的常量，旁边一句为什么（`P1.md` §3.13.12 的 `numbers.sh` 在 P1-04 加，到时会查这里）。它自己的那条队列和那个线程在这里写明了上限、满了怎么办和名字；后面的 `bounded.sh`、`spawn.sh` 不管 `log/src`（它不能用核心的 `rt`）。

### 2. 外壳装上

`app/src/main.rs`（最小的改动；P1-23 重写外壳）：

- 核心的 `Config` 加一个字段 `log: Level`（默认 `Info`）和 `Config::from_env(self) -> Self`：读 `ZUTALK_LOG`（`debug` 或 `info`；别的值当 `info`）。这是全核心唯一读环境变量的地方，注释里写明。再加 `Config::logs(&self) -> PathBuf`（`<data>/logs`）。顺手给 `Config` 现有的每个字段补齐一行注释。
- `setup` 里：数据目录 → `Config::new(..).from_env()` → `zutalk_log::install(&config.logs(), "zutalk", config.log, cfg!(debug_assertions))` → `info!("app.start", version = env!("CARGO_PKG_VERSION"), os = std::env::consts::OS, arch = std::env::consts::ARCH, level = …)` → 然后才是开钥匙库、起核心。`Guard` 交给 Tauri 管着（`app.manage`），应用退出时丢掉。
- 日志装不上（目录建不了）：不许因此起不来。`eprintln!` 一行（带 `// before the log: …`），不装，继续。
- 末尾那行 `eprintln!("ZuTalk could not start…")` 加上 `// before the log: …`（那时日志可能还没有）。

### 3. 服务器装上

- `server/src/main.rs` 开头：`install(<ZUTALK_DATA>/logs, "zutalk-server", 级别, true)`，级别同样读 `ZUTALK_LOG`；然后 `info!("server.start", version = …)`。
- `server/src/lib.rs`：
  - 路由表旁边加一张 `ROUTES` 表：每条路由的路径和它在日志里的代码（`grants`、`lease`、`claim`、`rooms`、`rooms.events`、`relay`、`healthz`）。路径里有 `{id}` 这样的占位，过不了 `is_plain`，所以日志里用代码。
  - 一层中间件：请求答完以后一条 `server.request`（`route`、`status`、`ms`）；状态 ≥ 500 用 `warn!`。**不记**请求的正文、签名的人、房间的 id、查询串。
  - `serve()` 的清扫：每次一条 `debug!("server.sweep", ms = …)`。
- 服务器别的地方不动。它的三处 `tokio::spawn` 和房间的 `broadcast` 不归这一件管（`P1.md` §3.13.12 末尾）。

### 4. `events.sh` 和 `docs/EVENTS.md`

- `scripts/build/rules/events.sh`，三条，哪条不过都打印「文件:行」并以非零退出：
  1. `core/Cargo.toml`、`app/Cargo.toml`、`server/Cargo.toml`、`zu/Cargo.toml` 里没有 `tracing`；`core/src`、`app/src`、`server/src` 里没有 `tracing::`、`use tracing`；
  2. `core/src`、`app/src`、`server/src`、`log/src` 里没有 `println!`、`eprintln!`、`print!`、`eprint!`、`dbg!`，除非那一行带 `// before the log:`；
  3. 从 `core/src`、`app/src`、`server/src`、`log/src` 里取出每个事件的代码（宏名后面第一个字符串字面量；宏可能被 rustfmt 折成几行，所以要跨行找，比如 `perl -0777 -ne 'print "$1\n" while /\b(?:error|warn|info|debug)!\(\s*"([^"]+)"/g'`），排序去重；从 `docs/EVENTS.md` 的表里取出第一栏的代码，排序去重；两边必须一样。多的、少的各自打印出来。
- `docs/EVENTS.md`（简体中文）：开头几句说这是什么、怎么读一行日志（抄 `P1.md` §3.13.1 的那一行例子和 §3.13.2 的字段表）、级别各是什么意思、哪些东西永远不在日志里。然后一张表：事件、级别、谁在什么时候发、字段。这一件写进七行：`app.start`、`app.panic`、`log.turned`、`log.lost`、`server.start`、`server.request`、`server.sweep`。**之后每件任务加自己的行。**
- `docs/DESIGN.md` §4 的模块表加一行 `log/`；§3 的概念表加一行「事件」。

## 显式与可观测

- **事件**（`P1.md` §3.13.3）：上面七个。
- **统计**：这一件还没有 `rt::stats`（P1-04）。日志自己的两个数 `lost()`、`refused()` 先放在这个 crate 里；P1-05 把它们抄进统计。
- **数**：`LOG_FILE_BYTES`、`LOG_FILES_KEPT`、`LOG_QUEUE_LINES`、`LOG_FLUSH_WAIT_MS`、`TEXT_MOST_BYTES`、`CAPTURE_KEEP_EVENTS`。8 MB 和 5 个是 `PLAN.md` §4.5.2 定的；其余在注释里说理由（4096 行：一秒几百行的突发也装得下，占的内存不到 1 MB）。
- **队列**：日志自己的那一条，`sync_channel(LOG_QUEUE_LINES)`，满了丢这一行并计数，写线程补一行 `log.lost`。
- **任务**：一个线程，名字 `log.writer`，主人是 `Guard`。
- **丢弃**：这个 crate 里每一处丢掉结果的地方，同一行写 `// dropped: <为什么没关系>`（`dropped.sh` 在 P1-05 加，会查这里）。

## 先写的测试

`log/tests/`（每个文件一个可执行文件；除了注明的，都用 `files` 或 `capture`，不碰全进程的那一个）：

1. `a_line_is_one_flat_object_of_numbers_booleans_codes_and_ids`：发一条带每种 `Plain` 的事件；文件里那一行解析出来，`t_ms`、`seq`、`level`、`component`、`event` 都在，字段的值和类型都对，没有嵌套。
2. `text_that_is_no_code_and_no_id_is_turned_away_and_counted`：`Id("two words")`、`Id("字幕")`、一个 65 字节的 `Id`、一个和 `event` 撞名的字段：值都是 `"!"`，`refused` 是 4；合法的 `Id("19a3f0c2b7e00d4c1f2")` 原样在。
3. 编不过的三件事，写成这个 crate 文档里的 `compile_fail` 文档测试（`cargo test` 会跑）：字段的值是 `String`；是借来的 `&str`；事件的代码是 `"Lane Reopen"`。每个旁边放一个只差在这一点、编得过的例子，证明编不过的原因正是它。
4. `the_grammar_of_a_code_and_of_plain_text`：`is_code`、`is_plain` 的表格测试，界限两侧都有（一段的代码不行；大写不行；64 字节行、65 不行；空串是 plain）。
5. `files_turn_at_their_size_and_the_oldest_goes`：`Limits { file_bytes: 1024, files_kept: 3, .. }`，发 100 行各约 100 字节的事件，刷盘：正好 3 个文件；每个不超过 1024 字节加一行；留下来的行 `seq` 连着、最新的一行在 `zutalk.jsonl` 的末尾；每个新文件的第一行是 `log.turned`。「该不该换」那个纯函数另有一条表格测试。
6. `a_writer_that_stands_still_loses_lines_and_says_how_many`：带闸的写口，队列 4 行；闸关着发 20 条；放开、刷盘：文件里的行数加上 `log.lost` 报的 `n` 等于 20，`seq` 缺的号正好是丢的那些。
7. `only_our_own_events_are_kept`：测试自己用 `tracing`（这个 crate 的开发依赖里本来就有）发一条 `target` 是 `hyper::client` 的事件、再开关一个 span：文件里一行也没有多。
8. `a_capture_hears_this_thread_and_whatever_it_carries_the_log_to`：装捕获；本线程发一条；起一个线程，里面 `carrier.enter()` 以后发一条；再起一个线程不带、发一条；一个多线程运行时上 `carry(async { … })` 发一条。捕获里是三条，没带的那一条不在。两个并排的线程各装各的捕获，互相看不见。
9. `log/tests/install.rs`（一个文件一条测试，因为它装全进程的）：`install` 到临时目录，发两条，`flush`：文件里有；第二次 `install` 是错误；`Guard` 丢掉以后再发的不写。
10. `log/tests/panic.rs`（同上）：`install`，起一个线程，名字 `t1`，里面 `panic!("secret words {}", 42)`，等它结束，`flush`：文件里有一条 `app.panic`，`thread` 是 `t1`，`file` 以 `panic.rs` 结尾，有 `line`；整个文件里找不到 `secret`。

`server/tests/log.rs`：

11. `a_request_leaves_its_route_its_status_and_no_more`：装捕获，对 `app()` 发一个 `/healthz` 和一个签名不对的 `/lease`（照 `server/tests/leases.rs` 的办法）：两条 `server.request`，`route` 是 `healthz` 和 `lease`，`status` 是 200 和那个拒绝的状态；`seen.refused() == 0`；每条事件的字段只有 `route`、`status`、`ms`。

规则脚本的证明（写进报告，不留在仓库里）：临时在 `core/src/lib.rs` 加一行 `println!("x");`、在 `server/src/lib.rs` 加一条 `EVENTS.md` 里没有的事件、从 `EVENTS.md` 删掉一行：`events.sh` 三次各自的输出；都去掉以后通过。

## 验收

- `scripts/build/check.sh` 全绿，`events.sh` 在其中。
- `cargo tree -p zutalk-core -e normal --depth 1 | grep tracing` 没有结果（核心不直接依赖它）；`zutalk-log` 的直接依赖只有 `tracing` 和 `serde_json`。
- 报告里：一行真的日志（从测试 1 的文件里抄）；`log/src` 多少行；门禁的耗时（改之前、改之后）。
- 在本机起一次服务器的可执行文件到「听上端口」为止再停掉（`cargo run -p zutalk-server`，数据目录用临时的），贴出它写的 `server.start` 那一行。**外壳不要启动**（会弹权限框，没人点）：它的接线由编译和审查保证，P1-26 的真机清单里有一项看 `logs/zutalk.jsonl` 的第一行。

## 不做

- 不加 `rt::stats`、`rt::queue`、树（P1-04、P1-05）；核心里除了 `Config` 的两处，不加任何事件：每个部件的事件由改它的任务加。
- 不加 `tracing-subscriber`、`tracing-appender`、`tracing-test`。
- 不用 span、`#[instrument]`。
- 不动服务器的任务和队列；不给服务器加请求以外的事件。
- 不做自检包（P1-22）、心跳和帧间隔（P1-24）。
