# ZuTalk 的设计

> 2026-11-16。负责人选定了重构稿的结构（`docs/2k-shape.md`），又要求把重构稿砍掉的功能全部补回。本文写的就是补回之后的样子：它由哪些概念构成、每个功能落在哪里、每个模块多少行。
> 七条原则在仓库根目录的 `CLAUDE.md`，这里不重复，只写它们落在哪里。行数的口径：注释和空行都算，测试、`ui/lang`、生成的类型、Zu 内核不算。行数上限已在 2026-11-21 取消（§6）。

## 1. 它是什么

ZuTalk 把一场对话变成录音的人自己签名的文字。屏幕上的现场字幕是给眼睛看的，录音时逐行存成第 0 次转写，也就是草稿；散场后用整段音频写一次，那才是记录。离开设备的东西只有一种容器：房间。唯一的例外是交到城的档案馆：交过去的是本人的记录本身（§5.11）。

## 2. 几条路

```text
现场  声音 → audio：每个声源一个线程拿着设备，泵只读环 → 100 ms 的块
             ├→ 队列 tape（600 块）→ keep 写盘任务（分批阻塞写）→ tape：加密的分钟块（音频只经 tape::Pass 离开）
             └→ 广播 audio（64 块）→ live::lane（每流一任务）→ 队列 lane.events → 帧任务（Composer / fit）
                                                                                ├→ Register<Frame> → 外壳 shell.pump → 窗口 · 直播房间的槽
                                                                                └→ 队列 draft（256 行）→ keep 写盘任务 → shelf：第 0 次转写
      （hub 不在现场的路上；电平、状态句、水位线、统计、提示也都经各自寄存器直出）
散场  tape → write（后台一次一件）→ lines 重跑 → 整场翻译与检查 → shelf：第 n 次转写
导入  音频文件 → audio::file：按样本数计时，不看墙上时钟 → 同一条路，只是没有现场字幕
导出  shelf → desk 渲染（export 表里的一行：Markdown、SRT、VTT）→ 独立写文件任务 → 文件
出口  一条录音或现场字幕 → room：在本机封好 → 我们服务器上的房间（只有密文）→ 拿到链接的人用观看页打开
交城  一条录音 → city（后台一次一件）：本人签名、写明给哪座城的记录 → 城的 Zu 主机；音频经 tape::Pass::City 分段作文件
搬入  0.8 交接的正文 → migrate（后台一次一件）：签成本人的记录，音频解开后重新封进 tape → shelf
查询  shelf 事实 → desk 单写同步 → 本机 SQLite + FTS5 投影索引 → L3 ViewEngine（只读 WAL、并发限 8、同槽可取消）→ 界面按块分页拉取
      （事实变化时仅发轻量的 Invalidate { view, key, version }，State 保持有上限的小状态）
```

## 3. 概念，各只有一个家

| 概念 | 是什么 | 家 |
|---|---|---|
| 块 | 采集时钟上 100 毫秒的单声道。丢掉的时间是静音 | `audio`、`clock` |
| 时钟 | 采集时钟：墙上时钟去掉暂停的时间，经端口传入。暂停和继续按调用那一刻的时钟盖时间 | `clock`、`audio` |
| 恢复 | 出事就从头重开。等多久是一个值 `supervise::Backoff`，声源和 Soniox 连接共用 | `supervise` |
| 行 | 一个声音用一种语言说的一句。现场和散场用同一套断句规则 | `lines`、`lang` |
| 记录 | 一条签名的 Zu 记录。改动是同一路径上多一条；草稿、转写、房间清单、词表、交城的去向都是记录 | `shelf` |
| 请求 | 对我们服务器的每个请求都是一条本人签名的记录，在 `zu::auth::WINDOW` 之内。一个端口 `keys::Ours::ask` | `keys`、`server` |
| 本机身份 | 没有人登录时签记录的 `did:key`（Ed25519 签名钥匙）；不联网、不登记，记录只留在本机书架 | `id/keys.rs`、`zu::did::local` |
| 钥匙 | 各服务的钥匙：本人自带的，或用授予的额度换来的短命钥匙；录音和房间的密钥由本机一个秘密派生。秘密只放一处：系统钥匙库，macOS 上是一个只有本人账户读得了的文件 | `keys` |
| 房间 | 只追加的密文日志，加一个只放最新一帧的槽。钥匙只在链接 `#` 后面 | `room`、`server/src/room.rs` |
| 导出器 | `EXPORTERS` 表里的一行 | `export` |
| 有没有音频 | 本机还有没有这条录音的分钟文件（`tape::kept`），`Row.audio` 把它交给界面。补写、清扫、交城都只问它 | `tape` |
| 留多久 | 音频在本机留几天，设备偏好里选；整场记录写好之前一律不删，本人销毁除外 | `retain`、`tape` |
| 词表 | 本人要听对、写对的人名和词：怎么说，各语言怎么写。一个词一条记录 | `glossary`、`shelf` |
| 交城 | 本人的记录，`to` 写城的 DID，交给城的 Zu 主机；交到哪里、交得怎样是本人的一条 `talk/filing` 记录 | `city` |
| 再试 | 会过去的失败按 `AGAIN` 起越等越久地再试，一个 `again`：整场转写、认领邀请、交城共用 | `supervise` |
| 状态、信号与提示 | 核心往外只走寄存器（`Outlet`）：有上限的小状态（`State`，13 个定界字段，不含资料库/正文/搜索/词表/日历）变了整份发；信号（`Frame`、`Session`、`Level`、`Progress`、`Water`、`Stats`）带各自序号 `seq`，只留最新一份；失效环（`Invalidations`）保留最近 64 条合并后的 `Invalidate`；提示（`Notices`）保留最近 16 个稳定代码，直接由各部件写入并发 `notice.said` | `app/core.rs`、`app/proto.rs`、`rt/signal.rs`、`view/query.rs` |
| 视图与投影索引 | 会随录音和资料增长的数据（按月统计、分页列表、全文检索、单场详情、64 行分块正文与两版对比、词表、日历、对外记录）全部由 `ViewEngine::query`（L3 `Tree::blocking` 并发限 8、同槽取消）按需拉取；本机 SQLite + FTS5 `trigram`（加 1–2 字短词回退）是可丢弃重建的投影加速器 | `view/index.rs`、`view/transcript.rs`、`view/query.rs` |
| 笔记与多版本精修 | 带时间与行锚点的多条 `talk/note`（向后兼容旧单串笔记，空文本即标记）；`talk/line` 支持读取任意旧 `take` 与版本列表；精修生命周期由 `RefineStatus` 状态机与 `Journal.failed` 持久化表达，支持 `Command::Refine` 手动触发、重试与再精修 | `record/notes.rs`、`record/takes.rs`、`ai/write.rs` |
| 寄存器 | `Register<T>` / `Reading<T>`：最新值加单调序号的 `watch` 薄包装，非阻塞、多写多读，晚来的读者立刻拿到最新一份 | `rt/signal.rs` |
| 水位线与判断 | 各部件只原子写进度位置（`Gauge`）；录音看守 1 Hz 采样得 `Water`，纯函数 `judge` 对照预算表 `BUDGETS` 得出 `Live` / `Slow` / `Stalled` / `Dead` 与动作，`due` 保证同一停滞只行动与留现场一次 | `rt/water.rs`、`app/recording.rs` |
| 端口（声音、连接、带子） | `Hear`、`Dial`、`Reels`（外加 `Drafts`、`Devices`、`Clip`、`Install`）：挂在 `Config` 上的薄端口，生产接真实设备/网络/磁盘，测试换暂停时钟下的确定替身 | `capture/audio.rs`、`live/wire.rs`、`capture/tape.rs`、`app/core.rs` |
| 重启上限 | `Restarts`（1 分钟 8 次或 120 秒无健康运行）：`Watch` 状态机到限后以 `End::Fatal` 放弃自愈并发 `part.gave_up`，交由人按 `Command::Reconnect` 重拉 | `rt/supervise.rs` |
| 事件 | 发生了什么：一个稳定的代码，后面跟着平铺的字段，只能是数、布尔、源码里写死的代码和明写的 id，一句话放不进去（编不过）。一行一个 JSON，写在 `<数据目录>/logs/zutalk.jsonl`，按大小轮换；发事件的人从不等磁盘。词典是 `docs/EVENTS.md` | `log/`（`zutalk-log`） |
| 统计 | 每个核心一份的 `Stats`：无锁原子计数、高水位、固定 13 桶延迟分布和每条队列状态；每秒作 `Stats` 信号发布，每分钟把动过的行写入 `stats.summary` | `rt/stats.rs` |
| 有名字的队列 | 全仓通道只经 `rt::queue`（`bounded` / `threads` / `fanout`）构造，写明带单位上限常量与 `Full` 策略（`Wait` / `Drop` / `Reject` / `End`），自动入统计并发边沿 `queue.full` / `queue.clear` | `rt/queue.rs` |
| 树 | `Tree`：`TaskTracker` 加 `CancellationToken` 的具名监督树，统一 `spawn` / `keep` / `blocking`（及具名 `thread`），自动带走日志接收者并维护 `Tree::running()` 运行表 | `rt/supervise.rs` |
| 自检包 | `Command::Diagnose` 在 desk 上原子写出 `<数据目录>/diagnose/<毫秒>/`（只留最新 3 份）：`about.json`、`water.json`、`stats.json`、`tasks.json` 与最近 2 份日志，按构造不含内容与秘密 | `app/work.rs` |

## 4. 模块

文件的路径相对于 `core/src`（`log`、`shell`、`ui`、`server` 除外）。`core/src` 下的目录是 `PLAN.md` §5.1 的层：`rt`、`capture`、`id`、`live`、`record`、`knowledge`、`ai`、`out`、`view`、`app`；每一层的模块文件（`rt.rs`、`capture.rs`……）开头写着这一层的预算和不许做的事；每一层只用它下面的层，表在 `scripts/build/rules/layers.sh` 里（`docs/build/phases/P1.md` §3.1、`docs/build/phases/P2.md` §3.1）。

| 模块 | 文件 | 做什么 |
|---|---|---|
| `log` | `log/src/lib.rs`、`log/src/line.rs`、`log/src/sink.rs`、`log/src/hear.rs`、`log/src/capture.rs` | 独立 crate `zutalk-log`：四个事件宏与 `Plain` 类型、平铺 JSONL 写线程 `log.writer` 与轮换、panic 钩子、测试捕获与任务/线程透传；全仓仅它依赖 `tracing` |
| `rt` | `rt.rs`、`rt/signal.rs`、`rt/water.rs`、`rt/supervise.rs`、`rt/queue.rs`、`rt/stats.rs`、`rt/keep.rs` | 运行时地基（不认识业务）：最新值寄存器、水位线与 `judge`、具名监督树与重启上限、有名字有界队列、统计与分桶分布、泛型批量阻塞落盘部件 |
| `capture` | `capture.rs`、`capture/audio.rs`、`capture/audio/file.rs`、`capture/clock.rs`、`capture/tape.rs` | 麦克风/电脑声音（cpal）与音频文件；重采样、混音、补静音、具名线程与重开；采集时钟与 `wall_clock_policy`；加密分钟块 `tape`、静音补洞、崩溃截尾与销毁 |
| `id` | `id.rs`、`id/keys.rs` | 身份（本机 `did:key` 与登录 `did:zu`，状态机 `Who`）、秘密文件与钥匙库、各服务钥匙来源（`From`）与 `Chain`、派生密钥、对服务器的签名请求、旧版邀请认领 |
| `live` | `live.rs`、`live/wire.rs`、`live/lane.rs`、`live/frame.rs`、`live/lines.rs`、`live/lang.rs` | 实时连接端口 `Dial`、每栏一条 `lane`（重连、换钥匙、取钥匙时限、跳直播沿）、增量组帧器 `Composer` 与 8 KB `fit`、断句规则、各语言档案与逐字显现单位 |
| `record` | `record.rs`、`record/shelf.rs`、`record/notes.rs`、`record/takes.rs`、`record/retain.rs`、`record/migrate.rs` | 书架（记录读写、改、删与撤销、`Writer`、O(1) 草稿写手 `DraftWriter` 与 `DraftGap`）；带时间/锚点的多条笔记 `notes`；多版本读取与精修状态机 `takes`；音频保留与清扫 `retain`；0.8 交接搬运 `migrate` |
| `knowledge` | `knowledge.rs`、`knowledge/glossary.rs` | 词表（读写、给现场各流的 `terms`/`translation_terms` 与整场转写的 `Plan.terms`；P6 升级为 `topic.rs`） |
| `ai` | `ai.rs`、`ai/write.rs` | 整场转写、整场翻译与检查、远端删除与扫尾、无钥匙草稿等待规则 `owed`、精修失败状态持久化 |
| `out` | `out.rs`、`out/export.rs`、`out/room.rs`、`out/city.rs` | 导出器表（Markdown、SRT、VTT）；房间密文封帧与写槽；交城日历、签名上传与分段音频 |
| `view` | `view.rs`、`view/index.rs`、`view/transcript.rs`、`view/query.rs` | L3 可丢弃本机投影层：SQLite WAL + FTS5 `trigram` 索引与自动重建/对账/增量同步、64 行正文分块与纯函数对比、并发限 8 可取消 `ViewEngine` 与有界 `Invalidations` 环 |
| `app` | `lib.rs`、`app.rs`、`app/core.rs`、`app/hub.rs`、`app/proto.rs`、`app/recording.rs`、`app/work.rs` | `Config`/`Core`/`Outlet` 入口（含 `Core::query`）；零 IO 状态机 `hub`（含 `Boot`）；瘦身后的协议 `proto`；一场录音的监督子树与状态机 `recording`；顺序 `desk`（单写 SQLite 同步）、导出、串行后台精修/交城/搬运与自检包 `work` |
| `shell` | `app/src/lib.rs`、`app/src/main.rs`、`app/src/pump.rs`、`app/src/beat.rs`、`app/src/windows.rs`、`app/src/updates.rs` | 无锁全 async 的 Tauri 外壳：异步 `query` 命令、独占推送状态机 `Fan`（含 `Progress` 与 `Invalidate` 转发/溢出降级）、非阻塞主线程心跳 `Beat`、不查询主线程的字幕窗显隐、无锁更新任务 |
| `ui` | `ui/src`（含 `ui/src/lib/vitals.ts`） | 主窗口、字幕窗、观看页；rAF 帧间隔采样与每秒 `drawn` 回报 |
| `server` | `server/src/lib.rs`、`server/src/main.rs`、`server/src/ledger.rs`、`server/src/relay.rs`、`server/src/room.rs` | 授予、取钥与认领账本、登录中继、密文房间与结构化请求日志 |

## 5. 补回的功能，各落在哪里

### 5.1 现场草稿逐行落盘

`shelf::Writer` 是往一个 pack 里加并覆写记录的写法：它记得每条路径现在指向哪些记录，所以加一条不用把 pack 重读一遍。`Shelf::put` 就是开一个 `Writer`、写、同步。现场草稿行键（`<id>.0.<n>`）严格唯一永不覆写，P2 起录音开始时枢纽改用 `Shelf::open_draft` 返回的 `DraftWriter`（固定 48 字节、`O(1)` 内存，不保留逐行 `heads` 表），第一条流每说完一行就追加一条记录；漏掉的一段也是一行（文字是 `…`，标为不完整）。录到一半崩溃，已经说完的行都在磁盘上（进程崩溃不丢；断电只保证到上一次同步）。拿不到整场转写的人，记录就停在这份草稿上。测试：`the_live_draft_is_kept_line_by_line_and_reads_back_as_take_0`、`i3_rebuild_queries_and_draft_stay_within_bounds`，以及枢纽测试里录音进行中就能读到草稿的那一段。

### 5.2 导入与导出

导入：音频文件是一种声源 `Source::File`。`audio/file.rs` 用 symphonia 解码，声道数以解码器给的为准，HE-AAC 明确拒绝；块的位置按样本数算，不碰墙上时钟。读文件的线程和泵听同一种消息（暂停、继续、停止）。打不开的文件在开始时就以稳定的代码拒绝（`audio.file.open`、`format`、`no_audio`、`codec`）。导入的录音没有现场字幕，状态说「只有声音 · 来自文件」，标题是文件名，散场后照常写记录。

导出：`export::EXPORTERS` 是一张表，一种格式一行；渲染是纯函数，写文件在枢纽里。路径没有扩展名时补上那一行的扩展名。界面导出阅读页正在显示的语言，字幕文件用其中第一种；路径来自 Tauri 对话框插件。

### 5.3 房间

服务器上一个房间就是一个 pack：造它的那条记录、往日志里加帧的每条记录，都是主人签名的 `talk/room` 记录 `{ room, do, frame, aud }`，经同一个 `check` 核对（类型、正文的 CID、`aud`、时间窗、签名钥匙是作者那一刻的）。所以只有一种认证；同一条记录送两次只算一次，重启后也一样。槽只在内存里，一个槽不会被换回更旧的帧。`make` 造房间（每人有上限），`log` 加一帧（每个房间有总字节上限），`slot` 换槽里的帧（空即清空），`gone` 删掉房间并告诉正在看的人。读房间不需要签名：`GET /rooms/{id}/events` 是 SSE，先补日志（按 `Last-Event-ID` 续接，不重复不遗漏），再给槽，然后实时。一个房间最后一次写入后保留 30 天，服务器重启算一次写入。

本机这边：房间的 id 是本机随机的 128 位，钥匙由本机的 `room` 秘密派生，人拥有哪些房间是 `talk/share` 记录（在 `shares.jsonl`，不算录音）。`Share { recording: None }` 造直播房间：枢纽的字幕帧经一个 `watch` 交给这个房间的任务，任务最多每 250 毫秒封一帧写进槽，录音之间槽是空的，出错只在错误变化时说一次。`Share { recording: Some(id) }` 造房间、记下、再把这条录音分页写进日志（每页带录音的抬头和放得下的行，观看页把行接起来）。`Unshare` 先从清单上拿掉（直播任务随之停止），再删服务器上的房间；删不掉就放回清单并说明原因。`State.rooms` 列出房间和链接 `{server}/r/{id}#{key}`。

观看页 `ui/viewer.html` 只凭链接读房间：钥匙从 `#` 后取，WebCrypto 开帧，按到达顺序处理，槽是现场字幕，日志是录音；钥匙不对、房间没了、看的人太多，都以代码说一次。服务器不托管页面：`ui/dist` 由服务器前面的边缘以同一个源提供，`/r/*` 指向 `viewer.html`（和 TLS、限流一样是边缘的事）。

### 5.4 逐字显现与界面语言

每个字幕带 `units`（每个显示单位结束处的 UTF-16 位置，由语言档案切：英文按词、中日文按字、泰文按 ICU 词），每栏带 `rate`（这种语言每秒显现几个单位）。界面只显现最后一行，`ui/src/lib/reveal.ts` 算到哪儿了、下一帧来时保留已经显示的部分。界面语言是 `Setup.language`，由核心保存，空则跟系统；可选的语言是 `ui/lang` 里有词的那几种。核心能处理的语言仍由 `State.languages`（来自 `lang::ALL`）给出。

### 5.5 设备的寿命（修正）

重构稿让泵线程打开和关闭设备，停止时又要等这个线程，于是一个卡住的系统调用会拖住所有声音和停止。现在：

- 每个声源一个线程（`audio::hold`）：设备在这里打开、持有、关闭，别处不碰；
- 设备回调只往环里写；泵线程只读环、按采集时钟补静音、混音，判定某个声源要重开时（停流、只送零）把理由发给那个声源的线程；
- 声源线程先报 `Restarted`、定好下一次打开的时刻（按传入的时钟和 `Backoff`），再放手设备；关设备卡住，只卡这一个线程；
- 停止：给泵发 `Stop`，泵交付完整的块后退出，并告诉各声源线程「结束了」；停止只等泵，不等任何设备调用；
- 暂停、继续、停止和声源的消息走同一条通道，按发生的顺序处理，暂停带着调用那一刻的时钟；
- 系统声音仍是 cpal。代码里写明已知的情况：macOS 拒绝权限时照样把麦克风或 tap 打开、只送零，这不报 `denied`，由「只送零」的看门狗报 `audio.silent`，重建的间隔越来越长。

测试：`a_device_that_hangs_in_the_system_holds_up_nothing`（一个设备关不掉、一个打不开，另一个照录，停止立即返回）、`a_stall_the_pump_hears_ends_the_device_on_its_own_thread`。

### 5.6 时间做成参数的测试（修正）

`a_source_that_dies_is_reported_padded_and_opened_again` 原来数真实时间里的块，满载并行时失败。现在采集的测试都用手拨的时钟（采集本来就接收时钟端口），设备是测试手里往环里写的一端；真实时间只用来限定「等一个线程回话最多等多久」。检查没有放松：块数从 14–18 收紧到 17–18，并且逐段检查声音、静音、声音。顺带发现并修了两处：

- 第一次出声时，所有声源的时间从同一刻开始（原来没声音的声源从泵第一次看到它算起，泵晚一步就让混音永远落后、停止时丢掉尾巴）。测试 `every_source_starts_with_the_first_sound_whichever_made_it`；
- `zu` 的 pack 测试在文件大小限制下运行自己，子进程的输出原来继承父进程，父进程输出到文件时会被限制打断；现在子进程的输出走管道。

2026-11-21 又修了一次（P1-01）。上面的修正没修到的地方：环里的声音不带时间，泵把「现在 − 刚排空的时长」当作声音开始的时刻，之后按「现在」补静音；测试却在不知道泵排空了没有的时候拨钟。泵排空得晚，起点就晚，块数变少（门禁里见到的 `too few blocks came`）；钟拨过去了而 `Feed` 还没到泵，泵先补一段静音，块数变多；泵读了「现在」以后测试又放了声音，多出几十毫秒。在通知回调里睡 50 毫秒就能每次复现。生产里墙上时钟不会跳，偏差只有几微秒，所以改的是测试怎么驱动泵，不是泵的规则：

- 泵线程的循环体成了公开的 `audio::Turner`，一圈是「收消息 → 按时钟走一步 → 交块 → 把重开的判断告诉声源线程」。`Capture::with` 就是 `Capture::by_hand` 加一个不停地转它的线程，行为不变；`by_hand` 不起泵线程，谁拿着 `Turner` 谁转。`Turner::attached()` 说设备交到泵手里几次了。
- 采集那一组测试自己转泵：每 20 毫秒「放声音 → 钟加 20 → 转一圈」，钟和泵严格交替；等声源线程的时候（通知、设备打开、交到泵手里）钟停着，只反复转。块数因此是确切的数：设备死了又开的那条 17 块（1.8 秒减去重开时还没补的 60 毫秒，测试的策略把不到 100 毫秒当抖动），停流那条 72 块。合盖改成「钟一下跳两分钟，再转一圈」，这才是合盖的样子。
- 只有 `a_device_that_hangs_in_the_system_holds_up_nothing` 留在真的泵线程上，它要证明的是「停止只等泵线程」；它的断言不看时间线，停止的上限从 1 秒改成 `WAIT`（防的是 3600 秒的卡死）。

### 5.7 旧版社区邀请搬过来

负责人定（2026-11-16）：用过社区邀请的人以后是城邦的成员，不是额度用户；他们的邀请无感地接着用。

- **同一个应用。** Tauri 的 `identifier` 是旧版的 bundle id `xyz.voice.zutalk`：新版作为旧版的更新装上，系统授权不用重来。
- **秘密。** `keys::get`/`set` 不变。macOS 上发布仍是 ad-hoc 签名，每次更新签名都变，钥匙串就会要密码；所以那里的秘密放在数据目录 `Secrets/secrets.json` 一个文件里（文件夹 0700，文件 0600，整份写好再改名替换）。Windows 与 Linux 仍用系统钥匙库。
- **旧令牌。** 外壳只在 macOS 上把旧版的数据目录 `~/Library/Application Support/ZuTalk` 交给核心（`Config.old`，§5.12 的搬运也从这里读）；邀请只读其中 `Secrets/community-invite.json` 这一个文件，不改不删。没有邀请就不造 `did:zu`，记录照常由本机身份签（§5.15）。
- **造身份。** 有令牌、本机没有登录身份（`did:zu`）时，核心造一个 `did:zu`：内核的 `zu::did::create` 是纯函数，用一把新的 P-256 轮换钥匙签创世操作，本人的 Zu 签名钥匙挂在 `zu-` 加由这把钥匙算出的八个字符下，没有 handle、没有服务。核心把它 POST 到 `Config.directory`，目录收下才存身份和轮换钥匙。did.md 要求先存好轮换钥匙再往下走，负责人选了以后再提醒：`State.rotation_unsaved` 让界面不挡路地提醒，`Command::RotationSaved` 说已经存好（钥匙仍留在本机）。读不出的身份不会被新造的顶替。（注：0.8 邀请无感搬运会主动找目录与服务器，与「没登录不发请求」的取舍见 `P1.md` §4 第 8 条。）
- **认领。** 本人签一条 `talk/claim` 记录 `{ token, aud }`，走 `keys::Ours::ask`，和别的请求同一种认证。成功或被明确拒绝（`invite.claimed`、`invite.unknown`）后，本机记下令牌的 SHA-256，不再发；会过去的失败按 `AGAIN` 起越等越久地再试，和整场转写、交城共用一个 `again`。
- **服务器。** `POST /claim` 照常核对记录，再用运营者配置的管理令牌请旧服务 `POST /v1/retire`。旧服务在一个事务里答出剩余秒数（配额 − 已用 − 预留，不小于 0）并停用邀请，再问只答「已退役」和 0；所以同一个令牌第二次认领得到稳定的 `claimed`。账本记下认领（不带正文，令牌不落盘），旁边是服务器自己的一句 `{ claim, to, seconds }`。折算时按实时价 `Upstream::per_second` 把秒换成 µ$，和取钥一样按现价算，所以剩下的仍是实时字幕的小时数。这笔额度不过期，排在所有会到期的授予之后用。

测试：`zu/tests/did.rs` 里造出的 DID 由内核自己的审计规则解析回那把钥匙（规格没有创建的向量）；`core/tests/keys.rs`；`server/tests/claims.rs`（旧服务的替身在本机端口上）；`core/tests/app.rs` 的 `an_invite_the_old_app_left_becomes_an_identity_with_the_hours_it_had_left`（目录和旧服务是替身，服务器是真的）。

### 5.8 音频留多久，销毁音频

负责人一贯的偏好：录音含敏感情绪，音频默认不长留，销毁音频是主操作。

- **选项。** `Setup.keep` 是设备偏好：空是一直留到本人销毁，`n` 是录音开始满 `n` 天删。界面给四个：一直保留、30 天、7 天、写好文字稿就删（0）。
- **默认：一直保留，照 0.8。** 旧版三档：`standard` 留到本人永久删除，`high` 本人要的整场转写成功后删，`maximum` 有了任何文字稿就删；默认 `standard`（`crates/vt-ffi/src/lib.rs` 的 `load_privacy_default`，`vt-store` 建档默认）。`high` 就是这里的 0 天；`maximum` 在现场草稿之后就删，新规则不许，并进 0 天。这个默认和「默认不长留」对不上，见 §7。
- **规则。** `retain::goes(keep, written, started, now)` 是值的函数：没写好整场记录不删；`keep` 为空不删；否则满 `keep` 天删。开始时间在将来算零天。它在启动时（读出资料库后）和每次整场转写写完后，对当前这个人的资料库跑一遍（`retain::sweep`）。偏好对本机所有录音生效，不像旧版每场冻结一份。移走的录音不在资料库里，音频留着等撤销。
- **搬来的录音（§5.12）。** 规则只问「整场记录写好没有」。搬来的草稿（`About.moved`）不会被送去写整场转写，所以照这条规则它的音频一直留到本人销毁，`keep` 选什么都一样；销毁之后它仍是草稿，只是再也写不成。0.8 里写过整场转写的，搬来就是写好的，从它的开始时间算天数，下一次清扫（下次启动，或下一次整场转写写完）照规则删。
- **销毁。** `Command::DestroyAudio`：录音中拒绝，不在资料库里拒绝。`tape::destroy` 删掉每一分钟，文件夹空了也删；整场转写的日志若还欠服务一次删除就留下，启动时的扫尾凭它删远端那份。阅读页直接放「销毁音频」，第二下在原处确认（`Sure.svelte`，停止分享也用它）。
- **有没有音频：一个事实。** `tape::kept` 看本机还有没有这条录音的分钟文件，`Row.audio` 把它交给界面：界面据此显示「音频」标签、销毁按钮和交城时的「音频」开关。没有音频的草稿不再补写，写的每次尝试也先看音频在不在；交城在读音频之前也再问一次（§5.11）。文字原样留着。
- **顺带修正。** `Config::new` 给整场转写的 Soniox 地址少了 `/v1`，真实应用里整场记录写不出来，留多久的规则也就永远等不到「写好」。

测试：`core/tests/retain.rs`（规则表、销毁留下欠账的日志、启动时清扫且没写好的留着、本人销毁后不再补写、写好后 0 天立即删）。

### 5.9 词表

- **放在哪：记录。** 词表是本人知道的东西，常是对话里的人名：它该跟着本人换设备，同一台设备上别人登录也读不到；设备偏好（`settings.json`）是这台设备上所有人共用的。类型 `talk/term`，像房间清单一样单独一个 pack（`terms.jsonl`）。键是「怎么说」规范化（NFKC、小写）后 SHA-256 的前 16 字节，所以同一个词再加一次是同一路径上多一条记录；拿掉是一条空记录。
- **现场。** `glossary::live` 按流配：每种写法都进 `terms`；这条流译入的每种语言，把其余写法译成那种语言的写法（没有就用怎么说的写法），进 `translation_terms`。不会把泰文写法交给译成中文的那条流。
- **整场。** `Plan.terms` 原来一直是空的，现在是录音任务开始时的词表，进整场转写的 `context.terms`、翻译请求和 `glossary` 守卫。
- **界面。** 设置页「词表」：已有的词是点一下就拿掉的标签，后面一个框写怎么说，说的语言和字幕语言各一个框写它的写法。解释只在提示里。

测试：`core/tests/glossary.rs`（每种流的配置、同一个词只一条记录、经枢纽到达现场配置和整场转写请求、登出后看不到）。

### 5.10 应用自己的更新

负责人定（2026-11-16）：新版作为旧版的更新装上之后，用 Tauri 官方的更新插件自己更新；发布时的做法在 `RELEASE.md`。

- **外壳做 IO。** 启动时和之后每 6 小时，外壳看一次 `app/tauri.conf.json` 里唯一的地址，有更新的版本就下载并核对签名，再经 `Core::looked` 告诉枢纽：`State.update` 是那个版本号，界面顶栏出现「更新」。看不成就说一次 `update`，直到某次看得顺利。
- **枢纽定什么时候装。** `Command::Update` 来时有录音就拒绝（`update.recording`）；没有才把枢纽的启动状态 `Boot` 切到 `Installing`，并交由 `Work` 在独立阻塞任务（`update.install`）里调外壳给的端口 `Config.install`：把下好的版本放到应用的位置，然后重启。`Installing` 期间枢纽照常应答别的消息，只拒绝 `Start` 与 `Import`（`update.installing`），因此安装期间同样不会开始新的录音；端口只在装不上时返回，枢纽将 `Boot` 切回 `Ready` 并说 `update.install`。
- **平台是数据。** 各平台的文件和签名写在 `latest.json` 里，由 `scripts/updates.py` 从构建好的文件生成；平台的差别是它 `KINDS` 表里的一行。
- **从 Sparkle 跳过来。** 同一个 bundle id、同一个包名、构建号大于交接录音的 0.8.6（66；新版从 67 起）、整包 ad-hoc 签名；这几条写在配置里，由 `app/tests/updates.rs` 守着。新版在旧版 appcast 的 `zu` 频道上，0.8.6 写好交接才看得到它（§5.12，`RELEASE.md` §4）。

测试：`core/tests/app.rs` 的 `an_update_is_installed_only_while_nothing_is_recording`（端口是替身）；`app/tests/updates.rs`（配置、脚本写出的清单用插件自己的 `RemoteRelease` 读、各种该拒绝的发布）。

### 5.11 交到城的档案馆

负责人定：ZuTalk 是 Zu 协议的应用，不是城（pol）的应用，两边靠 Zu 记录互通；一条录音交到城的档案馆时带多语文字，音频默认一起交，只有这一条路能带音频；不交笔记（旧版的标记）；音频已经销毁的录音只交文字。

**城今天收什么。** 城的新代码 pol V0.3.0（本机 `~/pol`，`d5cfe35`，2026-11-16）已经是 Zu 主机：`POST /.well-known/zu-recs` 收记录、`GET` 答查询，照 Zu v0.3.8 的 record.md、query.md。它还没有：上传文件（blob.md 的 `PUT /.well-known/zu-blob/<cid>`）、`/.well-known/zu-did`、`ev/event` 正文的定义、把交来的记录排成档案馆的页面。线上的城仍是 V0.2，签名格式和身份都不是 Zu 的，要交到那里得另开一套登录，违背「身份只用 Zu」，所以不接；pol 按自己的计划一次换成 V0.3 之后，这里不用改。

**做法。** 城就是一台 Zu 主机，按规范交：

- **选城、选活动**都在录音的阅读页。人填城的地址（域名；本机试城可以是 `http://127.0.0.1:<端口>`），`city::calendar` 取域名确认的 DID（map.md 的 HTTPS 方法），再查 `type=ev/event&to=<城>` 得到活动；没有正文或没有 `title` 的活动选不了，不列。界面默认选离录音开始最近的一场；音频默认开。
- **交什么**：本人的记录，`to` 写城的 DID，键里用一个随机的 `f` 替掉本机的录音 id（path.md 要键随机，记录头是公开的）——`talk/recording` 是 `About` 加上 `event`（活动记录的 `zu://` 路径）和 `audio`（每段 `{t0, file}`，不交音频时为空），`talk/line` 是当前那一次转写的每一行，`talk/speaker` 是说话人的名字。`talk/note` 不交。音频每 5 分钟一段 WAV，作文件挂在 `talk/recording` 上；读音频只有 `tape::Pass::City` 这一条。
- **有没有音频**：和别处同一个事实 `tape::kept`（§5.8）。界面只在 `Row.audio` 为真时给「音频」开关；`city::sign` 真要读音频之前再问一次，所以交之前、或重启后接着交之前，音频被本人销毁或被留多久的规则清扫掉，就只交文字，`talk/filing` 记下没带音频。启动时先清扫、再接着交，次序固定。交到一半才被销毁的，上传停在 `tape`。
- **认证**：不另登录。记录自带本人的签名，文件由记录点名，规范里这两种请求都不带别的凭证。
- **本机记下的**：`talk/filing`（在 `filings.jsonl`，键是录音 id）记城、活动、要不要音频、`f` 和状态；状态是空（在交）、`city.filed`、或停下来的代码。改状态就是同一路径多一条记录。交之前先记下，所以重启或换人登录回来会接着交。
- **怎么交**：先现查活动还在不在，再把记录签好（一次交里各次重试送的都是同一批），分批送，主机的每个词照表 `WORDS` 读；然后一段段上传音频，记着哪几段已经送到，重试只补剩下的。会过去的（连不上、忙）按 `again` 越等越久地再试，界面的进度写着在等什么。
- **失败是事实**：`city.address`、`city.unknown`、`city.unreachable`、`city.busy`、`city.event.gone`、`city.bad`、`city.big`、`city.full`、`city.deny`；文字到了、音频没收下是 `city.audio.big`、`city.audio.deny`。都在八种语言里有词。
- **同一座城再交**：沿用同一个 `f`，新签的记录和旧的在同一路径上并发，按 `at` 取新的；不带音频再交，`audio` 为空，旧的文件不再被点名，主机可以丢掉。

测试：`core/tests/city.rs`，城是本机端口上的替身，按规范验签、查 `to`、只收被现行记录点名的文件；没有对真的城发过请求。

**城那边要做的**：在城的域名上答 `/.well-known/zu-did`（一台主机跑几座城时怎么找到城的 DID，规范还没写）；收文件（blob.md）；定下 `ev/event` 正文里 `title`（字符串）和 `start`（毫秒整数）；把 `to` 是这座城的 `talk/*` 记录排成档案馆。

**没做、没证明的**：DID 文档反过来认不认这个域名（map.md 的双向核对）没查，因为城的 DID 文档怎么标还没规定；活动记录只核对了结构和正文的 CID，没去解析每个主办人的 DID 验签；不保留交出去的记录，所以重启后重试会在城里多一份同样的记录（并发，取 `at` 新的，结果不变）；撤回（送删除记录，主机随之丢掉文字和音频）没做；音频是不压缩的 WAV（每小时约 115 MB），换格式只改 `city::parts` 一处。

### 5.12 旧版录音搬过来

负责人定（2026-11-16）：用过 0.8 的人什么都不用做，就在新版里看到自己的录音（有人有 175 条）。规格在 [migrate.md](migrate.md)，0.8 那边的补丁在 [migrate-0.8.patch](migrate-0.8.patch)。

- **由 0.8 交接，新版不读旧格式。** 0.8 的文字在 SQLite（15 个 schema 版本）和 Loro 文档里（整场转写的订正、录音笔记只在 Loro）。新版直接读要加 SQLite 和 Loro 两个依赖，还要把旧版怎么叠订正、怎么绑译文、怎么切整场转写重写一遍。所以最后一个 0.8（0.8.6）把每条录音写成新版记录的正文（`handover/`），新版签名即可；音频不进交接，新版按交接给的位置和钥匙名去原处读密文。新版不加依赖。
- **越不过交接。** 新版在 appcast 的 `zu` 频道上，0.8.6（构建号 66）交接完才允许这个频道，所以自动更新到新版（构建号 67 起）的人都已交接。手动装了新版、跳过 0.8.6 的，新版说 `migrate.missing`，不猜。
- **一条录音整条搬或不搬。** id 由旧 id 定（和本机造的 id 同形），先封音频，再写各次转写、说话人、笔记，最后写 `talk/recording`；书架上有这条记录就是搬过了，删了也不再搬。断在半路，下次从头来。没搬成的以代码列在 `State.unmoved`，进度在 `State.moving`。
- **不替本人送出声音。** 搬来的草稿 `About.moved`，不算欠着整场转写：0.8 里整场转写要本人点了才写。也因此，照 §5.8 的规则，它的音频一直留到本人销毁。
- **只搬给登录的人。** 没登录时不搬 0.8 资料库（避免把 `moved.json` 锁死在临时本机身份上）；第一次以登录身份搬时记下 DID（`moved.json`），换了身份登录不再搬。P4 做领回时再一并定是否允许先搬到本机身份再领回（§5.15）。
- 现场草稿是第 0 次转写（订正在 `via` 里记为 `hand`），0.8 的整场转写是第 1 次并成为录音的文字；标记的笔记接在录音笔记后面。回收站里的录音、笔记本本身、不属于录音的笔记、术语表、分享、城的记录都不搬。

测试：`core/tests/migrate.rs`（夹具的音频和钥匙由旧仓库代码写成）、`core/tests/app.rs` 的 `the_old_library_moves_in_on_its_own_and_nothing_of_it_leaves`。

### 5.13 运行时（P1）

P1 已将 `PLAN.md` §4 与 `docs/build/phases/P1.md` §3 落地：现场路径（采集 → 队列/广播 → `tape`/`lane`/`frame` → 寄存器 → 外壳 `shell.pump` → 窗口）绕开 `hub` 与磁盘；`hub` 成为纯内存状态机（`hub.sh` 守零 IO），磁盘与网络活分别落在 `Work` 的顺序 `desk`、独立搜索/导出/网络任务及串行（许可 1）后台任务里；全仓通道收进 `rt::queue` 并用 `clippy.toml` 禁用无界通道；所有任务挂在具名监督树 `Tree` 上；`water.rs` 与看守在 1 Hz 采样、判定停滞并自动重开或按上限放弃；外壳改为无锁全 `async` 的 `shell.pump`（状态机 `Fan`）、非阻塞心跳 `shell.beat`（状态机 `Beat`）与 `updates` 任务（`shell.sh` 守零锁零同步命令）。

与 `PLAN.md` 字面不同的实现取舍及理由（详见 `P1.md` §2、§3.6、§3.9、§3.13.15）：

- **落盘改用「async 任务 + 分批阻塞调用（`rt::keep`）」而非独立线程**：同样与采集泵和现场路径隔着有界队列；且 tokio 暂停时钟会等阻塞调用而不等裸线程，台架虚拟时间因此确定不抖。
- **外壳推送用单任务独占 `Fan` 而非 `arc-swap`**：订阅表和最新信号只归 `shell.pump` 一个任务所有，彻底无共享锁，也就不需要 `loom` 建模交错（由 `app/tests/pump.rs` 穷举交错覆盖）。
- **不用 `turmoil`、`loom`、`hdrhistogram`、`tokio-metrics`、`statig`**：`Dial`/`Keys` 替身在暂停时钟下即可确定模拟不回话、断网与取钥匙超时；分布用固定 13 桶无分配实现；10 个状态机体积小，手写枚举 + 纯函数 `step` 更清晰。
- **显式与可观测细节差异（`P1.md` §3.13.15）**：`rt::queue` 显式传入 `&Stats`，`Full` 为 `Wait`/`Drop`/`Reject`/`End`（`Coalesce` 由寄存器承担）；`stall.snapshot` 与每分钟汇总按平铺行输出（前者用递增 `snap` 号串联，后者以 `stats.summary` 领头）；日志独立为 `log/` crate（服务器也要装），靠宏写入 callsite 的 `zutalk:<代码>` 记号而非模块前缀过滤外部库日志；界面画帧序号与帧间隔合并为 1 Hz 单命令 `drawn(seq, gap_ms, gaps)`；取消 `ZUTALK_WATER_LOG`，改由 `ZUTALK_LOG=debug` 写 `water.mark`；规则脚本同步管 `app/src`、`server/src`、`log/src`（服务器的 3 处 `spawn` 与 1 处房间 `broadcast` 记在 `later` 表，P4/P5 还）；`zu/` 的 `Path::new` 仅放开 Ed25519 `did:key` 作本机作者（§5.15）。

### 5.14 显式与可观测

- **日志**：写到 `<数据目录>/logs/zutalk.jsonl`（服务器为 `<ZUTALK_DATA>/logs/zutalk-server.jsonl`），每行平铺 JSON（`t_ms`、`seq`、`level`、`component`、`event` 加事件字段），按 8 MB 轮换、留 5 个；发事件的人走 4096 行有界队列，从不等磁盘。
- **防泄漏与规矩守护**：字段类型由 `Plain`（数、布尔、`&'static str`、`Id`）在编译期把关，运行期 `is_plain` 二次把关，并由隐私测试检查真实落盘字节（含笔记、搜索词与精修）与自检包。全部 72 个事件定义在 `docs/EVENTS.md`，由 5 个显式规则脚本（`events.sh`、`numbers.sh`、`bounded.sh`、`spawn.sh`、`dropped.sh`）加上 `layers.sh`、`hub.sh`、`shell.sh`、`pending.sh` 共 9 个规则脚本在门禁里守着。
- **出了事先看什么**：字幕停了看 `recording.start` 的 `captions`、`judge.verdict`、`part.gave_up` 与 `lane.open`；录音自己结束看 `recording.end` 与最后一条 `recording.phase`；界面卡了看 `shell.main_thread.stalled` 与 `ui.frame.gap`；命令没反应看 `command.refused`、`command.done` 与 `job.start`/`job.done`；索引重建看 `index.open` 与 `index.sync`；查询变慢看 `query.slow` 与 `query.done`；精修失败看 `refine.state` 与 `write.skipped`。停滞发生时按 `snap` 号拉出整组 `stall.snapshot`；要交回现场时发 `Command::Diagnose` 生成自检包。

### 5.15 不登录也能录

照 `PLAN.md` §2.7 与 `P1.md` §3.14：

- **本机身份与 `Who`**：首次启动时核心在唯一秘密文件/钥匙库生成 `identity.local`（32 字节 Ed25519 种子，不联网不登记），其 `did:key`（`zu::did::local`）作本机记录作者，书架放在 `records/did-key-…/`。签名身份由状态机 `Who`（`Nobody(code)` / `Local` / `Person`）表达，DID 永不进 `State` 或日志（日志只发 `identity.who` 的代码）。`zu::did::is_did` 不变，任何 Zu 主机按构造拒绝本机身份记录。
- **要登录的命令是一张表**：`Command::needs_person`（P1 为 `Share`、`Unshare`、`File`）在 hub 分派前拦下并回 `sign_in.first`，什么都不写；录音、导入、改标题、搜索、导出、词表、销毁音频、开字幕窗均不需登录，且没登录不向我们的服务器发任何请求。
- **没有钥匙是一种录法，不是故障**：各服务钥匙来源收口在 `id/keys.rs::From`（`Nowhere` / `Own` / `Granted`）；开录前由 `lane::captions` 决定是否开流，无钥匙时不开 lane、状态为 `AudioOnly { why: "captions.no_key" }`、全程安静只录音；散场后由 `write::owed` 判为 `Waits("keys.none")`，草稿静候（发 `write.skipped`），设置里填入自有钥匙后自动清点补写。
- **P1 到 P4 之间的过渡**：登录后把本机书架领回本人 DID 留在 P4（`DECISIONS.md` 第 11 条）。在此之前，登录看到本人 DID 的书架、登出回到本机书架，两边互不挪动、什么都不丢；一场录音由开始时的身份签到底。

### 5.16 数据层、投影索引与视图查询（P2）

P2 将 `PLAN.md` §2（事实/视图/小状态/信号）与 `docs/build/phases/P2.md` 全部落地，并还清 `layers.sh` 的两处 P2 欠账（`record/retain.rs`、`record/migrate.rs` 脱离 `app::proto`）：

- **事实扩展（`record/notes.rs`、`record/takes.rs`、`record/shelf.rs`、`ai/write.rs`）**：
  - `talk/note` 键格式为 `<id>.<at_ms:010>.<rand:08x>`，正文 `{ at, text, anchor? }`，支持边录边记（录音中不拦）、空文本作锚定标记（`DECISIONS.md` 第 2 条），并对旧单串笔记（键 `<id>`）向后兼容；新增 `Command::Note`（`Add` / `Edit` / `Remove`）。
  - `takes::list` 与 `takes::read` 可读出 `lines.jsonl` 里的任意历史 `take`（例如 `take 0` 现场稿与 `take 1` 精修稿）；`RefineStatus`（`Draft` / `Refining` / `Refined` / `Failed`）经纯函数 `refine_step` 转换并发 `refine.state`，失败代码写入 `tape/<id>/write.json`（`Journal.failed`，含额度钥匙被异步转写拒绝时的 `keys.async_refused`）；新增 `Command::Refine { id }` 手动触发、重试与再精修。
  - 现场草稿改用 `DraftWriter`（`Shelf::open_draft`，结构体 48 字节、不留逐行 `heads` 映射），写 10,000 行内存恒为 `O(1)`。
- **可丢弃的本机 SQLite + FTS5 投影索引（`view/index.rs`）**：
  - 核心仅增一项依赖 `rusqlite = { version = "0.37", features = ["bundled"] }`（静态内嵌 SQLite + FTS5 `trigram`，无系统版本差异）。
  - 索引文件落在 `<data>/index/<did>.sqlite3`（WAL 模式、`INDEX_BUSY_TIMEOUT_MS = 200`、`INDEX_SCHEMA_VERSION = 1`），由 `desk` 独占单写（通过有序 `IndexSync` 作业加 oneshot 提交回执保证落盘后可见），由 `ViewEngine` 以 `SQLITE_OPEN_READ_ONLY` 并发只读。
  - 打开前经 `IndexHealth` / `open_plan` 探测：缺失、损坏、版本变化或换身份均自动删库并逐条流式重建（`O(1)` 内存，发 `index.open`）；健康库启动时按 `(size, mtime_ns)` 快速对账。
  - 搜索支持标题、当前版正文、译文与笔记，全部经 `lang::profile::key` 归一化（NFKC、小写、泰文 SARA AM 统一）；≥ 3 字符走 FTS5 `trigram`，1–2 字符自动回退归一化子串扫描。
- **分块正文与纯函数对比（`view/transcript.rs`）**：
  - `Transcript` 固定 `TRANSCRIPT_CHUNK_LINES = 64` 行一块（带 `total_lines` / `total_chunks`）；`Compare` 按时间区间重叠（`[t0, t1]`）对齐两版并跑有界字符级 LCS（`DIFF_LCS_MAX_BYTES = 1_024`），产出满足文本重建的 `DiffOp`。
- **L3 查询引擎、小状态瘦身与外壳转发（`view/query.rs`、`app/core.rs`、`app/proto.rs`、`app/src/pump.rs`）**：
  - `State` 瘦身为 13 个定界字段，彻底移除 `library`、`open`、`hits`、`terms`、`city`、`writing`、`filing`、`moving` 与旧命令 `Open`/`Close`/`Search`。
  - 新增 `Outlet.progress`（`Progress { refine, filing, moving }`，≤ 3 槽）与 `Outlet.invalidated`（`Invalidations` 合并环，上限 `KEEP_INVALIDATIONS_EVENTS = 64`）。
  - `Core::query` 与 Tauri `#[tauri::command] async fn query` 走 `ViewEngine::query`（`Tree::blocking` 并发限 `QUERY_CONCURRENT_MAX_JOBS = 8`，同 `QuerySlot` 新查询取消并 `InterruptHandle::interrupt()` 打断旧查询，超 `QUERY_BUDGET_MS = 100` 发 `query.slow`）。
  - `app/src/pump.rs` 转发 `Signal::Progress`（订阅时重放当前值）与 `Event::Invalidate`（仅当合并环满 64 条且版本号断层时才降级为 7 个 `ViewKind` 的粗粒度全量失效）。

测试：`core/tests/notes.rs`、`core/tests/takes.rs`、`core/tests/view.rs`、`core/tests/view_scale.rs`（1,000 条录音全查询 ≤ 8 ms、10 小时 6,400 行分块与对比 ≤ 13 ms、I2 并发查询不卡字幕、I3 重建与 `Tally = 390 B` 边界、I8 单块代价与时长无关）、`core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library`（5 条 vs 100 条 `State` 字节数比 `1.00`）。

## 6. 各模块的大小（上限已取消）

负责人 2026-11-21 定：「代码取消行数限制」。`budget.toml` 和 `scripts/budget.py` 已拿掉，改动不再对着上限核对；原则 7「小，是因为结构对」照旧。下面是取消之前（2026-11-16）量到的数和当时的上限，留作各模块大小的记录。

量到的是这个结构加上补回的功能，当时的上限向上取整不超过 5%。

| 模块 | 量到 | 当时的上限 |
|---|---:|---:|
| `audio` | 767 | 770 |
| `tape` | 156 | 160 |
| `retain` | 37 | 38 |
| `live` | 479 | 480 |
| `lines` | 375 | 380 |
| `write` | 656 | 660 |
| `shelf` | 284 | 285 |
| `glossary` | 73 | 75 |
| `export` | 87 | 90 |
| `room` | 74 | 75 |
| `city` | 389 | 390 |
| `keys` | 437 | 450 |
| `migrate` | 278 | 285 |
| `app` | 1,270 | 1,280 |
| `shell` | 129 | 135 |
| `ui` | 725 | 730 |
| `server` | 839 | 880 |
| `build` | 179 | 180 |
| **合计** | **7,234** | **7,300** |

Zu 内核另计：1,550 / 1,550（为造 `did:zu` 从 1,500 调高，负责人同意）。

旧版邀请搬过来（§5.7）多了约三百行：`keys` +132（秘密文件约 55、造身份约 30、提醒、读旧令牌、认领）、`server` +104（认领、退役请求、账本里的额度）、`app` +73（搬运、`again`）、外壳 +4；`build` 少了 macOS 钥匙串的依赖。

留多久与词表（§5.8、§5.9）多了约两百二十行：`retain` 37、`glossary` 73、`tape` +24（分钟列表、在不在、销毁）、`shelf` +7（词的类型、一个通用的读法）、枢纽与协议 +41、界面 +34（设置、销毁、标签、`Sure`）。

应用自己的更新（§5.10）多了约一百四十行：外壳 +59（看、下、装、重启，插件注册）、`build` +60（`scripts/updates.py`）、`app` +22（字段、命令、端口、报告）、界面 +2。

交到城的档案馆（§5.11）多了约四百九十行：`city` 389（新模块）、`app` +65（两条命令、进度、接着交）、`ui` +33（`City.svelte`）、`shelf` +3、`tape` +2。

旧版录音搬过来（§5.12）多了约三百二十行：`migrate` 278、`shelf` +13（`moved`、`known`、`preview` 一处定义）、`app` 约 +33（后台搬运、进度、草稿不欠转写），界面 +4，外壳 −1。

四块合在一起是 +1,181（6,053 → 7,234）。合并时各留一处：不属于录音的记录只有一个读法 `Shelf::kept`（两边各写了一个，`shelf` −4）；随机键只有 `keys::fresh` 一处（房间、请求、交城原来各写一遍，`keys` +4）；「有没有音频」只有 `tape::kept`；`ui/lang` 里同词的「音频」「去掉{0}」各留一个键；交城的测试用词表与留多久的测试已有的枢纽替身（`core/tests/hub`）。

比重构稿（4,682）多出约两千五百五十行，大头是：导入（`audio/file.rs` 125）、房间（核心 74、服务器约 210、枢纽约 100、界面约 110）、导出 87、逐字显现约 50、草稿的 `Writer` 与房间清单约 60、每个声源一个线程约 40，以及上面五块。比旧版（6,953）多约两百八十行，差不多就是旧版没有的搬运（`migrate` 278）。

## 7. 留给负责人定的

1. **`clock::wall_clock_policy` 已照临时决定实现，数字等负责人确认。** 照 `docs/build/DECISIONS.md` 第 3 条：落后 ≤ 300 ms 不动（`JITTER_MS`），到 60 s 补上落后的静音，更长（`ASLEEP_MS`，多半是合盖睡眠）结束录音。一个后果：同时录麦克风和电脑声音、电脑不出声时，混音要等电脑那一路的时钟补静音，最多晚约 0.45 秒才交出（测试 `with_the_app_policy_a_quiet_source_holds_the_mix_back_under_half_a_second`，按 20 ms 一步量到 300 ms）。改数字只改 `clock.rs` 里两个常量。
2. **只用协调员额度的人写不出整场记录。** Soniox 临时钥匙只能用于实时（`transcribe_websocket`），异步转写会被拒，所以他们的记录停在现场草稿上。现在草稿逐行落盘，至少不会因崩溃丢失；没有钥匙的草稿现在是静候（`Waits("keys.none")`、`write.skipped`），不报错，填上自己的钥匙后再清点补写。从旧版邀请搬来的额度也一样只用于实时字幕。照 §5.8 的规则，这些草稿的音频也一直留到本人销毁。
3. **轮换钥匙怎么交到本人手里。** 状态只说「还没存」，命令只说「存好了」。原则 6 不许钥匙到界面，所以给本人看或复制这把钥匙的办法由负责人定。
4. **音频默认留多久。** 照 0.8 默认一直保留，和「音频默认不长留」对不上。要改只改一个值：`Setup.keep` 的默认（比如 0 天，写好文字稿就删）。另：偏好对本机所有录音生效，不再每场冻结；移走的录音音频留着等撤销，不受规则管。
5. **销毁挡不住备份。** 录音的密钥由本机一个秘密派生（`keys::derive("tape", id)`），删不掉单独一把；备份里留下的密文，拿着这个秘密仍能解开。旧版销毁时连这条录音的密钥一起删。要做到这一点，需要每条录音一把随机密钥存进秘密库，会改到录音的加密和旧录音的迁移。
6. **发布。** 旧服务的 `/v1/retire` 要部署，服务器要配 `INVITES_URL` 和 `INVITES_TOKEN`（等于旧服务的 `ZULANGUE_ADMIN_TOKEN`）。先打上 `migrate-0.8.patch`，编译、在真数据的副本上核对交接，发 0.8.6（构建号 66）；新版（构建号 67 起）再进旧版的 appcast，带 `<sparkle:channel>zu</sparkle:channel>`，0.8.6 那一项留着。做法见 `RELEASE.md` §3、§4。
7. **更新的签名钥匙。** 钥匙对由负责人造，私钥只在负责人的钥匙圈（`RELEASE.md` §2）；在那之前 `plugins.updater.pubkey` 是空的，发不了版。签名里写版本号（`requireSignedVersion`）等换了会写版本号的 tauri-cli 再开。
8. **搬来的草稿要不要补写整场转写。** 现在不自动写，也没有让本人要求补写的命令；它们的音频因此按 §5.8 一直留到本人销毁。要不要加、加在哪，由负责人定。
9. **0.8 的旧数据留不留。** 新版只读旧目录，不改不删；音频因此在磁盘上有两份，直到本人删掉旧目录。要不要在搬完后提醒或代删，由负责人定。
10. **交城是房间之外离开设备的一条路。** 原则 3 说离开设备的东西只有一种容器（房间）；交到城的档案馆的是本人的记录本身，给城公开，音频也走这条路（§5.11）。负责人定过要这样交；要不要把这一例外写进原则，由负责人定。

## 8. 旧版有、这里没有的规则和测试

| 旧的 | 现在 | 原因 |
|---|---|---|
| 房间封存（`PATCH /rooms/{id}`，`a_sealed_room_takes_no_more_writes`、核心的 `Heard::Sealed`） | 没有 | 房间只由主人写；一条录音的房间写完各页就不再写；观看页用不到「到此为止」；结束一个房间就是删掉它 |
| 核心里读房间的 `Rooms::read`（`whoever_has_the_link_reads_what_the_owner_wrote`、`without_the_key_nothing_opens`） | 测试里的读取器 `core/tests/common` | 读房间的是浏览器里的观看页；同样的断言在 `whoever_has_the_link_reads_what_the_owner_wrote_and_nobody_else_writes` 和枢纽测试里。本人多台设备之间用房间同步是以后的事 |
| 服务器分配房间 id | 本机随机 128 位，`make` 时查重 | 写房间的请求是签名记录，房间 id 要在签名之前定 |
| 房间空闲按文件修改时间算 | 按最后一次写，重启算一次写 | 槽的写入不落盘；宁可多留，不误删正在用的直播房间 |
| `Shelf::purge`（`purging_takes_the_records_off_the_device`） | 没有 | 没有调用者；删除是一条记录，可以撤销 |
| 请求头里的声明与内存随机数表（auth 的 6 条 statement 测试） | 签名记录 | 重构稿的决定；记录自己防重放，重启后仍然成立。房间写入也用它 |
| 按 DID、按地址限流（`requests_are_counted_by_did_and_by_address`） | 没有 | 交给边缘代理 |
| 结算接口、按实际用量退还（leases 的结算各条） | 每把钥匙按上限记账 | 重构稿的决定，待负责人点头（见 `2k-shape.md` §8 第 3 条） |
| `GET /grants` | 余额随取钥答复返回 | 重构稿的决定 |
| 登录中继的寿命从第一次请求算 | 从投递算 | 重构稿的决定 |
| 「只送零」先问系统有没有别的进程在放声音 | 只看零 | cpal 不提供；真安静时也会按 10、30、70… 秒重建一次，重建不丢声音 |
| 手工清单第 8 项：不录本进程的声音 | 没有 | cpal 的 tap 不排除本进程 |
| 界面的操作表 `actions.ts` 与它的 4 条测试 | 规则写在阅读页里 | 表没了。只在阅读时复制和导出：按构造成立；录音中的那一条不能分享或删除：阅读页里不出现（无测试）；一条录音只一个房间：核心去重（枢纽测试）；不能撤销的先问：停止分享在原处再问一次（无测试），删除改为可撤销 |
| 设置页的四条隐私说明 | 没有 | 负责人定过界面不放解释性文字 |
| 销毁前安全覆写（`PrivacyDestroyer::destroy_file`） | 只删文件 | APFS 与 SSD 上覆写碰不到旧的块；分钟文件本来就是密文。密钥见 §7 第 5 条 |
| 每场录音冻结一份保留策略 | 偏好对所有录音生效 | 一个事实一个地方；改了偏好，下次启动或下次写完就按新的来 |
| `pause_and_resume_reach_the_pump`（按真实时间） | `paused_the_clock_stands_still_and_resumed_it_goes_on` | 按采集时钟，检查更严 |

## 9. 没有证明的

1. cpal 的回环只有真机能证明：macOS 14.6 以下没有进程 tap；Windows 是端点回环，cpal 不处理「静音缓冲」标志；Linux 只走 f32。`core/tests/audio.rs` 开头的手工清单要在三个系统上逐项跑。
2. 设备关不掉时，那个声源的线程停在关设备上直到进程结束，这次录音里这个声源不再重开（泵照样给它补静音，另一个声源照录，停止不受影响）。也可以把卡住的设备交给另一个线程去关、让这个声源先重开，但那就违背「设备只在自己的线程上开、持、关」，这里没有做。没有真机上卡住的例子可测。
3. 服务器重启后五分钟内，有人重放一条截获的槽写入，可以把槽换回那一帧（槽的时间不落盘）。帧是密文，只有拿着钥匙的人看得到旧字幕。
4. 观看页依赖边缘把 `/r/*` 指向 `viewer.html`；仓库里只有开发时的替身（`ui/dev/rooms.ts`）。
5. 应用里录音停止后，直播房间的槽清空；应用崩溃时槽里最后一帧会留到下一次录音或服务器重启。
6. 旧服务退役了邀请、服务器还没写进账本就崩溃，这份时间就丢了：再认领只得到 `claimed`。窗口是一次 fsync。
7. 留多久的规则只在启动时和写完整场转写后跑：一台开着几周、期间没写过转写的机器，音频会比选的天数留得久。搬运写进来的录音也等下一次清扫。
8. 真的目录（`plc.zuzalu.ink`）、真的旧服务和 Sparkle 更新都没有跑过；ad-hoc 签名的新版作为旧版的更新装上，要在真机上验。Tauri 更新插件在真机上替换应用、重启，也没有跑过。
9. 0.8 那边的补丁没有编译，也没有对真数据跑过；Sparkle 频道挡住越过交接的更新，没有在真机上验。新版这边只用合成的交接验过，175 条的真库没有跑过。
10. 真的城没有收过：交城只对本机端口上的替身跑过（§5.11）。
11. 真机 1 小时实录没有跑过（出口 E9，待负责人按 `docs/build/reports/P1-26-close.md` 清单在 macOS / Windows / Linux 上执行）。
12. 175 条真库搬运（P7）尚未在真数据副本上跑过；I2 与大数据规模的 1,000 条录音索引查询与 10 小时（6,400 行 × 2 版）分块/对比已在 P2 `core/tests/view_scale.rs` 中实测通过（全部 ≤ 13 ms，并发查询下直播零 `Stalled`）。
13. 本机身份（`did:key`）签出的记录没有交给任何真的 Zu 主机试过：照规范它们会拒绝，仓库里只对本机服务器替身测过。
