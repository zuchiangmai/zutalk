# 必须带走的经验

> 摘自旧仓库 zulangue 的 `docs/architecture/rewrite-10x.md` §6，是旧版用真实录音换来的。每一条在新代码里要有对应的测试，或者写明为什么不再适用。

> 写这份清单时，现场还要逐句用 LLM 翻译、断线还要重放去重。按现在的设计（见 [DESIGN.md](DESIGN.md)），现场只给眼睛看：「重放并去重」「最多替换一次」「逐句翻译的期限与对冲」这几条不再适用，其余照旧。


这些是仓库三个半月、约 58 个版本里用真实录音换来的，加上这次五份研究的发现。新代码要逐条有对应，多数写成测试。

**Soniox**

- 一条流只能有一个目标语言；`two_way` 遇到第三种语言只转写不翻译；默认每个项目 10 路并发。
- 实时吞吐锁死在 1.00x。只允许断线后的有界追赶：内存留 15 s 音频，重连后 2 倍速补送并去重；超过 15 s 跳到直播沿，记一段「转写空缺」。
- 密集连续讲话几乎不触发 `<end>`（段长中位数 49 s）：行超过 10 s 且有 200 ms 真静音就请求 finalize，25 s 硬上限。
- finalize 回执的时钟偏移要校正；`<fin>` 按先进先出配对。
- 服务商自己划的边界上，未提交的投机文本是下一句的开头，**丢掉**（旧库 222 条 partial 行里 172 条是这么来的）；其他边界照旧冻结。
- 只含空白的段不成行（旧库 264 行整行只是一个空格）。
- 转写可能整场不带语言标签：先记 `und`，有证据再提升。
- 临时钥匙的预留额度同时就是 `max_session_duration_seconds`：长录音在安静处主动换班、续预留；时长到上限回 403，不能当认证失败。
- 发送队列满 = 落后，不等于死亡；连接真的死了必须让界面知道（旧实现返回 Ok，字幕停了录音键还亮着）。
- 暂停时发 keepalive。录后整场转写：远端删除是成功的前提，远端 id 先落盘，启动时扫尾。

**翻译与 OpenRouter**

- 关键错误的头号来源是识别。专有词表同时喂给 Soniox（`terms`、`translation_terms`）和 LLM；乱码必须输出「听不清」。
- 泰语说话人性别从文字看不出来：规定用不带 ผม/ฉัน、ครับ/ค่ะ 的礼貌说法。
- 数字守卫比结构化的值；直接比数字串每个引擎 9–12 次误报。
- 逐句翻译关掉「思考」：DeepSeek flash 开着时延迟从 0.96 s 到 1.6 s；Sonnet 5.5 关不掉，只能用 `low`。
- OpenRouter 在服务商接下请求后一律回 HTTP 200，失败写在正文或流的最后一块里（`finish_reason: "error"`）；已经输出过内容就不再兜底。
- 严格 JSON 是按端点支持的：Claude 在 Bedrock 上不支持，要求严格时只剩 Vertex。
- 发出去的钥匙也要在请求里带零留存参数，不依赖账户设置是否正确。

**文字**

- 标点规整显式列出标点区段，不用 `is_alphanumeric`，否则吃掉泰文的元音与声调符号（ำ）。
- 汉字之间的空格保留原样；泰文逐字显现以 ICU4X 切出的词为单位。

**音频**

- 别的应用抢麦：重启采集，按墙上时间补静音（旧版因此静默丢过 45 分钟）。在 AVAudioEngine 上学到的，换成 cpal 后逐系统重新证明。
- 合盖睡眠会结束录音；睡眠唤醒事件没有现成的跨平台库，每个系统自己写约 100–150 行。
- 导入 AAC 以解码器 `spec()` 的声道数为准；symphonia 不支持 HE-AAC，要明确拒绝。
- Windows 的 WASAPI 回环在静音时不回调，放一路无声的保活流。
- 音频只走「交到城的档案馆」这一条路。

**身份**

- 扫码链接防不住「别人拍下二维码、用自己的身份抢答」：采用前必须让用户看到登进来的是谁。
- 服务器校验 DID 日志一律去城里取最新的，不信客户端带来的（被撤销的钥匙可以拿一段旧日志来）。
- 城的签名形状比 RFC 8032 更严（edShape）：通用库会接受城拒绝的签名，所以保留自写的校验。

**界面与测试**

- 行是不可变值；列表虚拟化；不要每帧逐行比较全文。
- Tauri 普通 event 不为低延迟设计，实时数据走 `Channel`。
- 测试不碰已安装应用的数据与偏好：每个测试用独立的数据目录。
- 现有的离线重放台架是最值钱的测试资产，新代码第一天就接上。

## 新仓库里用真服务重新量到的（2026-11-15，Soniox stt-rt-v6）

- v6 的 `<fin>` 不再多算时间：四次收尾，服务时钟都正好等于已发的音频。旧版的时钟校正（S4）不需要了，测试里有断言守着。
- 47 秒连续讲话没有一个 `<end>`；只靠 25 秒硬上限会把词组从中间切开。所以保留「满 10 秒后遇 200 毫秒停顿就收尾」。
- token 的 `end_ms − start_ms` 恒为 60 毫秒，所以一行的结束时间偏早。
- 静音时服务仍每 1.0–1.2 秒报一次进度；连上时 1.4 秒的积压 0.7 秒消化完。

## 发布签名与钥匙串（2026-11-16 补记）

- macOS 上 ad-hoc 签名的应用，钥匙串条目的访问权限绑在这一次构建的签名上；每次更新签名都变，读自己存的秘密就弹钥匙串密码框。旧版因此把凭证和邀请令牌搬到 `Application Support/ZuTalk/Secrets/` 下只有本人可读（0600）的文件。负责人定：在有稳定的 Developer ID 之前，新版在 macOS 上也这样存；Windows 与 Linux 仍用系统钥匙库。
- 新版是同一个应用的更新（bundle id `xyz.voice.zutalk`，同一个 appcast），用户不用重新安装；改 bundle id 会让麦克风、录屏等系统授权全部重来。

## P1 里学到的（2026-11）

- **测试拨钟要和读钟的线程握手。** 环里的音频样本不带时间戳，测试若在不知道泵线程排空了没有的情况下拨钟，压力并发下必然偶发块数偏多或偏少（P1-01）。把泵循环体抽出成 `audio::Turner`，让测试手转泵与拨钟严格交替后，1200 次满载压力 0 失败。
- **暂停的 tokio 时钟不等自己起的 OS 线程，只等阻塞任务。** 台架比真实时间快几百倍，若落盘用裸 OS 线程，系统稍一调度队列就溢出；把落盘做成 `rt::keep`（async 任务 + 每批一个 `Tree::blocking` 调用），虚拟时间正好等每批写完，既与现场路隔离，又完全确定。
- **寄存器只留最新值，测试不能等一闪而过的中间状态。** `Register<T>` 是 `watch` 的最新值语义，连续两次写入之间读者可能跳过前一次；断言状态跃迁或事件轨迹应查结构化日志或稳定终态，而非赌能拦到寄存器里的瞬态值。
- **测试要看见核心在别的线程上发的事件，任务就得从树上起。** `tracing` 的线程局部接收者默认不跨 `spawn` / `spawn_blocking` / 线程；全仓统一走 `Tree::spawn` / `Tree::keep` / `Tree::blocking` / `rt::supervise::thread` 自动带走 `zutalk_log::carry` / `carrier`，才让并发测试各看各的日志、互不串台。
- **事件的字段由类型管着，一句话根本编不进日志。** 用 `Plain` trait 把事件字段收紧为数、布尔、`&'static str` 和显式 `Id(..)` 后，对话内容、标题、搜索词或秘密连编译都过不去；同时在调用点写 `zutalk:<代码>` 记号，既隔离第三方库日志，又让测试 crate 自己发的事件也能被捕获。

## P2 里学到的（2026-11）

- **FTS5 `trigram` 原生只索引 ≥ 3 字符，中文 1–2 字必须显式回退，且索引和查询要共用同一归一化函数。** SQLite `trigram` 对「押金」这类 2 字词不会建三元组；对 1–2 字符在 `search_docs.norm_text` 上做 `instr` 子串扫描（1,000 条录音实测 7.37 ms）既不引外部分词库又不漏结果。同时索引与查询统一走 `lang::profile::key`（NFKC + 小写 + 泰文 SARA AM 规整），才保证泰文异写和全半角大小写一致命中。
- **单写多读 WAL 配合 `InterruptHandle` 取消与 `IndexSync` 提交回执，才既不挡查询又不读到旧库。** 写入全归 `desk` 一把连接，并发查询在 `ViewEngine` 信号量（上限 8）内开只读连接并登记 `InterruptHandle`，新查询同槽立即打断旧 SQL；而后台任务（精修、录音收尾、交城、搬运）写完事实后必须经 `IndexSync` 带 oneshot 回执等 SQLite 事务提交再发 `Invalidate`，否则界面收到失效立刻查库会读到未提交的旧快照。
- **现场草稿行键永不覆写，不该用通用 `Writer` 留一辈子 `heads` 映射。** 通用 `shelf::Writer` 为支持同路径覆写会保留每条路径的 `heads`，10 小时草稿会白占数千项；拆出专用的只追加 `DraftWriter`（结构体仅 48 字节、`O(1)` 内存），写 10,000 行不涨内存，且落盘格式与 `Writer` 完全一致。
- **合并型失效环的溢出判定不能只看版本号差值，必须看环是否真被挤满。** `Invalidations::push` 按 `(view, key)` 合并旧条目，合并本身会让 `items[0].version` 跳号；外壳 `PumpState` 只有在 `items.len() >= 64` 且 `invalidated_at + 1 < first.version` 时才判定发生了溢出并降级为 7 类粗粒度失效，避免正常合并误触发全量重刷。
- **固定上限的统计表是一种硬预算。** `Stats` 有固定行数上限（计数 40、高水位 4、分布 12、队列 16）和 8 KB 序列化上限；P2 新增 4 个计数和 2 个分布时同步移除了淘汰的 `search.stale`，并把 `stats.full == 0` 与 `Tally <= 8192 B` 写进规模压测断言，防止指标悄悄超标失明。

## P3 里学到的（2026-11）

- **dsh 总线把 `useSyncExternalStore` 和 Tauri IPC 锁死在 `ui/src/bus/` 一处，页面测试就能脱离 Tauri 飞快跑完。** 所有组件拆成「纯 props 视图 + 薄总线容器」，配合静态规则脚本 `ui.sh` 禁止总线外出现 `useSyncExternalStore`、`@tauri-apps/*`、`invoke(`、`new Channel`、外网 CSS `@import` 与 `dangerouslySetInnerHTML`，既守住离线桌面启动与 XSS 边界，也让全量 76 条 UI 测试在 1.3 秒内确定性跑完。
- **动态分块虚拟列表靠总线层的 `useQueries` 聚合，既不打破 Hooks 规则也不拉离屏块。** 阅读页按 64 行分块拉取 `Query::Transcript` / `Query::Compare`，在 `bus/query.ts` 把当前可见块组合为单个复合 `Source`；滚动离开旧块即自动退订，收到 `Invalidate` 绝不重拉离屏块，4 小时（2,560 行 = 40 块）实录在 600 px 视口下仅拉取 2 块、仅挂载 11 个 DOM 行。
- **jsdom 下测 `virtua` 必须还原真实浏览器的两项时序。** `virtua` 在 `ResizeObserver.observe(scrollEl)` 之后才初始化滚动监听器，若测试替身在 `observe()` 内同步回回调会读到未初始化状态；同时 jsdom 默认 `offsetParent === null`，会导致 `virtua` 一直保留 `visibility: hidden` 跳过项高测量。改由 `queueMicrotask` 异步派发尺寸并在 `ui/tests/dom.ts` 补齐 `offsetParent` 后，虚拟列表在 jsdom 中能真实按 600 px 视口裁剪并测出确定 DOM 节点数。
- **行内说话人编辑必须同时按 `lineKey` 定位，且所有行内提交都要防 IME 组合键。** 若只按 `speaker` 编号判断编辑态，整页同说话人的多个 `autoFocus` 输入框会互相抢焦点立即触发 `onBlur` 关闭编辑；同时必须检查 `e.nativeEvent.isComposing || e.keyCode === 229`，避免中文/日文/韩文输入法按回车选词时误提交。
- **把 `test.environment` 保持在 Node、仅 DOM 测试文件标注 `// @vitest-environment jsdom`，避免 `crypto.subtle` 跨 realm 冲突。** `room.test.ts` 的 AES-256-GCM 加密测试与 `words.test.ts` 字典全量核对原生跑在 Node 下既快又无 jsdom `ArrayBuffer` 跨 realm 报错。

## P4 里学到的（2026-11）

- **在 `multi_thread` 运行时里测真实落盘文件日志，不能复用带 `zutalk_log::capture()` 的测试壳，也不能把 `Entered` 守卫跨 `.await` 持有。** `App::with` 会安装线程局部内存捕获器，顶掉 `zutalk_log::files`；而 `Entered` 是 `!Send`。只在 `Core::start(cfg)` 同步启动那一刻进入 `scope.enter()`，核心监督树会自动把文件日志接收者透传给全部后台任务与线程，既能跑多核真实本地端口，又能把每条事件真实落进 `<data>/logs/zutalk.jsonl` 供 `Command::Diagnose` 打包审计。
- **本机记录领回放在 `record` 层而绝不碰 `knowledge` 层，靠 `Shelf::kept::<Value>(TERM)` 就守住了单向依赖。** 领回需要同时重签录音和词表；若在 `record/adopt.rs` 引入 `crate::knowledge`，会直接破坏 `layers.sh` 的分层红线。直接按通用 JSON 值重签 `talk/term` 记录既保持零跨层依赖，也对词表未来字段扩展天然兼容。
- **并发防超支必须在「余额」和「每日次数」两把尺子上共用同一个 `pending` 预占账。** 如果 `Book::pending` 只扣余额、不记 `(did, service)` 在途件数，两个并发 `POST /lease` 在当日只剩 1 次额度时会同时通过上限检查并双双铸钥。把 `service` 编进 `pending` 并在 `Book::quota` 与 `lease` 里同步计入在途次数，并发压测下绝不超发。
- **领回必须靠 `person.known(&id)`（含墓碑）而不是 `person.about(&id)` 判重，否则会复活用户已删的录音。** 用户把领回来的录音删掉后，本人书架里留的是空值墓碑；如果下次登录只看现行清单，会把本机备份里同一条录音再次领回来。用 `known` 检查路径历史即可同时保证幂等、防复活、又允许同人补领后来新录的本机录音。
- **`Stats` 接近硬上限（38/40 计数）时，新业务观测应优先走结构化事件而非滥加计数器。** P4 全程零新增 `Stats` 计数/分布行，身份/额度/领回全部复用 `jobs.done`/`jobs.failed`/`commands.done` 配合 4 个语义清晰的事件（`identity.auth`、`quota.updated`、`identity.adopt`、`identity.discard_local`），稳守 `stats.full == 0` 与 `Tally ≤ 8192 B`。
