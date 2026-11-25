# ZuTalk 的架构与路线图（草案）

> 2026-11-21 起草，等负责人定稿。定稿后并入 `DESIGN.md`，本文删除。
> 依据：
> - 本仓库现在的代码（未提交，7,234 行）；
> - 0.8 在真机上的日志：`~/Library/Application Support/ZuTalk/logs/`，含 11-16 字幕停了 14 分钟、录音中两次被强制退出；
> - 11-21 凌晨画的界面重设计，25 张画板，已从临时目录复制到 `docs/ui-2026-11-21/`；
> - 同一会话里一个代理对「新设计 vs 现在核心」逐项做的核对；
> - zumind 决策 0160（pol 的前端照 dsh 式 React）。

## 0. 一句话

ZuTalk 的毛病从来不是「数据错」，而是「流动停了」：录得越久越慢、一处卡全体停、停了没人知道、重启才好。新架构把**活性**和**数据正确**放在同一个等级：

- 三种数据各走各的路；
- 每条路有时间预算，每个队列有上限；
- 每个部件报告自己的进展，可以随时丢掉重来；
- 这些都由测试守着。

界面重设计带来的功能（边录边记、精修稿对比、知识库、主持、按住交档案馆、需要时才问身份）全部落在这套结构里，不另开机制。

---

## 1. 原则（修订版）

`CLAUDE.md` 的七条不变，改其中两条的措辞，再加两条。

**原则 2：两种速度 → 每条路有自己的时间预算。**
- 现场字幕给眼睛看，散场以后写记录。这一条照旧。
- 推广成：采集、现场、记录、查询、后台、界面各有预算，**快的路永远不等慢的路**。

**原则 3 中的「一个状态整份发给界面」改为：**

> 一个**有上限的**状态整份发给界面。
> 会随录音和资料增长的东西是**视图**：由记录算出，界面按范围去取，核心只说哪里变了。
> 现场字幕和进度是**信号**：只留最新的那一份。

信号、状态、视图是三种不同的东西，各自只有一种传法，不算多出来的第二套机制。

**新增原则 8：活性是事实。** 每条路有预算，每个队列有上限，每个部件报告自己的进展；停滞要被看见、被处理，不能被干等。

**新增原则 9：代价与历史无关。** 任何一次更新的代价，只取决于变化量和屏幕大小，不取决于录了多久、存了多少。

---

## 2. 数据：三种，以及它们之间的关系

### 2.1 事实（Zu 记录与音频，只追加，属于本人）

| 类型 | 键 | 正文 | 来自设计稿 | 现在的状态 |
|---|---|---|---|---|
| `talk/recording` | 录音 id | `About`：开始时间、长度、标题、说的语言、字幕语言、声源、当前用哪一版（`take`）、用了哪些主题（`topics`），以及话题词（`tags`，见下方说明） | 标题与话题 | **P6 已做**：`About` 补齐 `topics`、`tags`、`title_by`、`title_candidates`（均 `#[serde(default)]` 向后兼容） |
| `talk/line` | `<id>.<take>.<n>` | 一行：时间、说话人、语言、原文、译文、每一部分的来源（`via`） | 现场稿 / 精修稿 / 对比 | 有；旧的版本要能读出来（**P2-01 已做**：`record/takes.rs` 支持读任意 `take` 与版本列表，现场草稿改用 `O(1)` 内存 `DraftWriter`） |
| `talk/speaker` | `<id>.<label>` | 名字 | 读一场录音 | 有 |
| `talk/note` | `<id>.<t_ms>.<rand>` | 一条笔记：`{ at, text, anchor? }`；`anchor` 指向某一行 | 边录边记、读页的笔记 | **改**：现在是整场一个字符串（**P2-01 已做**：`record/notes.rs`，键 `<id>.<at_ms:010>.<rand:08x>`，向后兼容旧单串笔记，边录边记不拦） |
| `talk/topic` | 主题 id | 一个主题：名称、领域、背景、词表、译法、来源文件；外加一个固定的「每场都用」主题 | 知识库 | **P6 已做**：`topics.jsonl` 取代旧扁平 `talk/term`（首次打开自动无损迁移旧词入固定 `always` 主题，兼容 `Command::Term`/`Query::Glossary` 与本机领回 `adopt`） |
| `talk/filing` | 录音 id | 交到哪座城、哪场活动、带不带音频、状态 | 交到档案馆、设置里的「对外」 | 有 |
| tape（不是记录） | 录音 id / 分钟 | 加密的分钟块 | 音频 | 有 |

> **`tags` 和 `topics` 不是一回事。** `tags` 是从内容算出来的话题词，例如「续一年 · 押金」，界面上写作「话题」。`topics` 是本人在知识库里准备的主题。

几处取舍：

- **单条录音的分享**：设计稿已经删掉（`Share` 画板不在最终的画布上）。去掉 `talk/share` 的录音页和服务器上的房间日志；直播房间只留建、写槽、删三个动作。（**P5-01/02 已做**：删除 `out/room.rs`、`talk/share`、`State.rooms`、`Command::Share`/`Unshare` 与服务器 `rooms/` 落盘和 `log` 动作，仅留无盘 `make`/`slot`/`gone`。）
- **主持不记忆**：直播房间**不再是记录**，只活在这一场录音里（见 §5.5）。（**P5-02 已做**：`out/host.rs` 纯内存持钥 + 仅存 32 位房间 id 的 `host.lock` 两道保险。）
- **标记**：0.8 的重写决定里写着「删掉标记功能」，新设计里又有「标记」按钮（M），它把一条笔记钉在当前那句话上。**这一条请负责人确认**（§9 第 2 条）。本文暂且只把它当作笔记的 `anchor`，不另设标记类型。

### 2.2 设备偏好与秘密（不是记录）

- `settings.json`：
  - 下一场录音的设置（说的语言、字幕语言、声源、选中的主题）；
  - 界面语言、明暗、音频留多久；
  - 字幕窗在每种场合下的样式：行数、字号、背景、字体、位置、置顶、锁定、自动隐去。（**P5-03 已做**：`Setup.caption: CaptionPrefs` 固定 4 场合 `self_view`/`band`/`stage`/`facing` 与 `Setup.city` 落盘 `settings.json`，旧配置向后兼容。）
- 秘密：照旧放在一处。macOS 上是 `Secrets/secrets.json`。

### 2.3 信号（只留最新值；重新订阅时立即拿到当前值）

| 信号 | 内容 | 频率 | 上限 |
|---|---|---|---|
| `Frame` | `seq`、`t_audio`；每栏最近 K 行加正在说的那一行；每栏的健康状态 | ≤10 Hz | 序列化后 ≤ 8 KB（Tauri Channel 走直接注入的上限） |
| `Session` | 录音状态（录音中 / 已暂停 / 只录音 / 文字慢了 / 连接中）、开始时间、对外与否、几个人在看 | 变化时发 | 固定（**P5-02 已做**：补齐 `outbound`、`viewers`、`event_title` 定界标量） |
| `Level` | 输入电平 | ≤15 Hz | 固定 |
| `Progress` | 精修、交城、搬运、生成标题，各自一份 `{id, step, done, of}` | ≤2 Hz | 每种任务一份（**P2-05 + P6-03 已做**：`Outlet.progress` 承载 `refine` / `file` / `move_old` / `title` 全部 4 槽，独立信号量 `TITLE_AT_ONCE_JOBS = 1` 绝不阻塞精修或交城） |
| `Water` | 各层的水位线和判断结果（§4.3） | 1 Hz | 固定 |

### 2.4 小状态（整份发送，类型上有上限）

身份（DID、句柄、要不要存恢复密钥）、城、额度（出字、AI 各一条，每天的次数）、各服务钥匙的来源、下一场的设置、字幕窗是否显示、有没有新版本、提示的队列（最多 N 条）。

**不再放进去**：资料库、正文、搜索结果、词表、日历。（**P2-06 已做**：`State` 瘦身为 13 个定界字段，移除 `library`/`open`/`hits`/`terms`/`city`/`writing`/`filing`/`moving` 及旧命令 `Open`/`Close`/`Search`，I8 字节数比 `1.00`；**P4-02/03 补齐**：以 `auth: AuthFlow` 取代裸 `login`，增补定界 `handle`、`quota`（固定 2 服务）与 `local_shelf`（`recordings` + `adopted`），5 条 vs 100 条录音 `State` 为 `523 B / 525 B`，比值仍为 `1.00`；**P5-02/03 补齐**：移除 `rooms: Vec<Room>` 改用定界 `host: HostState`（至多 1 场），并在 `Setup` 中加入固定 4 场合 `CaptionPrefs` 与 `city`，5 条 vs 100 条录音 `State` 为 `1320 B / 1322 B`，比值恒为 `1.00`；**P6-02 补齐**：`Setup` 增补封顶 `MAX_SELECTED_TOPICS = 8` 且只存随机十六进制 id 的 `topics: Vec<String>`，5 条 vs 100 条录音 `State` 为 `1332 B / 1334 B`，比值恒为 `1.00`。）

### 2.5 视图（由事实算出，可以丢掉重建，按范围取，带版本号）

| 视图 | 查询 | 怎么维护 |
|---|---|---|
| `library` | 按月分组的计数；某个月的行；最近 N 条（给侧栏用） | 本机的 SQLite 索引，从 pack 重建；写入事实时同步更新（**P2-03/05 已做**：`LibrarySummary` / `LibraryMonth` / `LibraryRecent`） |
| `search` | 关键词 → 命中的录音和行，范围是标题、话题、正文、译文、笔记 | SQLite FTS5 的 `trigram` 分词器，中文、泰文、英文都能搜，不需要分词库（**P2-03 + P6-03 已做**：含 1–2 字短词归一化子串回退、SARA AM 统一及 `INDEX_SCHEMA_VERSION = 2` `SearchHitKind::Tag` / `LibraryFilter.tag`） |
| `recording` | 抬头 + 有哪几版 + 笔记 + 交城状态 + 主题与标题候选 | 读 `about.jsonl` 和笔记（**P2-04 + P6-03 已做**：`Query::Recording` 另带 `RefineStatus`、`topics`、`title_candidates`） |
| `transcript` | `(id, take, chunk)` → 64 行一块，每块带版本号 | 按块缓存，一块不变就不重读、不重画（**P2-04 已做**：`Query::Transcript`，从 SQLite `take_lines` 按块切取，录音中直读草稿 pack） |
| `compare` | `(id, take a, take b, chunk)` → 逐行的差别 | 纯函数，按块算（**P2-04 已做**：`Query::Compare`，时间重叠对齐 + 1024 B 封顶字符级 LCS `DiffOp`） |
| `knowledge` | 主题列表；一个主题的详情 | 读 `topics.jsonl`（**P6-01 已做**：`ViewKind::Knowledge`、`Query::Topics` / `Query::Topic { id }`，并保持 `Query::Glossary` 指向固定 `always` 主题） |
| `calendar` | 一座城的活动 | 现取，只在内存里缓存（**P2-04/05 已做**：`Query::Calendar`） |
| `outbound` | 已交、正在交、没送到 | 读 `filings.jsonl`（**P2-04 已做**：`Query::Outbound`） |

核心只推送 `Invalidate { view, key, version }`；界面自己决定要不要重新拉。（**P2-04/05 已做**：经 `Outlet.invalidated` 有界 64 槽合并环与 `app/src/pump.rs` 转发，溢出退回 7 类粗粒度失效。）

### 2.6 它们之间的关系

```text
            采集块(t) ──► tape（事实）──────────────────────────────┐
                │                                                    │ 散场
                ▼                                                    ▼
   lanes ─► tokens ─► 断句 ─┬─► 说完的行 ─► talk/line take 0（事实）  精修 ─► talk/line take n
                             │                                       标题/话题 ─► talk/recording
                             └─► 正在说的那一行
                                       │
           Frame（信号）= 最近 K 行 + 正在说的那一行 + 健康状态 ─► 主窗口 / 字幕窗 / 直播房间的槽
   笔记 ─► talk/note（事实，钉在 t 或某一行上）
   主题 ─► 现场每条流的 context（terms / translation_terms / general / text）
          精修时的 context
   事实 ──投影──► 视图（library / search / transcript / …）──按范围拉──► 界面
                       └─ Invalidate(version) ─────────────────────────► 界面
```

一句话：**事实只追加；视图由事实算出；信号是此刻。界面只读信号和视图，只发命令。**

---

### 2.7 不登录也能录（2026-11-21 负责人：「不登录，也可以录音。登录只是有额外的功能。」）

- **本机身份**：第一次打开时，核心在本机造一把签名钥匙，用它的 `did:key` 作本机身份。不联网，也不登记。钥匙放在唯一的秘密文件里。没登录时，所有记录都用它签，放在 `records/<本机身份>/`。（P1-06 已做：`keys::local`、`Who`、`zu::did::local`。）
- **不登录能做的**：
  - 录音、导入；（P1-06 已做：不再回 `sign_in.first`。）
  - 现场字幕：设置里填了自己的 Soniox 钥匙就有；没有钥匙就是「正在录音 · 只录音」，代码 `captions.no_key`，声音照录；（P1-07 已做：`lane::captions`。）
  - 精修：要自己的钥匙；（P1-07 做到有钥匙就写、没有就静候发 `write.skipped`；**P2-01 已补齐** `RefineStatus` 状态机、`Journal.failed` 持久化（含 `keys.async_refused`）与 `Command::Refine` 手动触发/重试/再精修。）
  - 笔记、标题、说话人、知识库、导出、字幕窗、音频留多久。（P1-06 已做：变更/搜索/导出/词表/销毁音频/字幕窗均不需登录。）
- **要登录的**：
  - 主持和对外（直播房间）；
  - 交到档案馆；
  - 城给的额度；
  - 恢复密钥和换设备。（P1-06 + P4 已做：`Command::needs_person` 拦下 `Share`/`Unshare`/`File`/`RefreshQuota`/`DiscardLocal`，回 `sign_in.first` 且不写任何东西；其余随 P5 补行。）
- 按下这些功能时才弹登录的纸，没有登录墙。设计稿里「按下开始录音时弹出」那一张，改为在按下要登录的功能时弹。（**P4-04 已做**：`ui/src/bus/identity.ts` 显式状态机 + `IdentitySheet.tsx` 按需弹出，首页与录音零拦截。）
- **登录以后：领回本机的记录。** 第一次以某个 DID 登录时，把本机身份名下的记录全部用这个 DID 重新签一遍，放进这个人的书架（和 §5.12 搬运 0.8 录音的路一样：签成本人的记录）。音频不动：tape 的钥匙由本机秘密派生，与身份无关。（**P4-03 已做**：`core/src/record/adopt.rs` 重签全部 `take`、说话人、全部笔记、`talk/recording` 与词表，并同步 SQLite 索引与 `Invalidate`。）
  - 领完记下「本机身份已归某个 DID」，之后不再领给别人。（**P4-03 已做**：`<data>/adopted.json` 单人绑定，`OtherPerson` 拒绝第二位 DID。）
  - 本机身份的书架留着，等本人删除，以便撤销。（**P4-03/05 已做**：显式 `Command::DiscardLocal` + 设置页红底两步 `InlineConfirm` 清除，保留 `adopted.json` 与 `tape/`。）
  - 登录前正在录的那一场，录完再领。（**P4-03 已做**：`started_by` 为 `did:key` 的进行中录音按原身份签到底，封盘后经 `adopt::run_one` 自动补领。）
- **退出登录**：回到本机身份。已领走的记录只在那个 DID 的书架里。（P1-06 + P4-03 已做。）
- **服务器**：没登录不向我们的服务器发任何请求。（P1-06 已做，由 `nobody_signed_in_nothing_is_asked_of_our_server` 守着。）

## 3. 界面重设计逐屏对照

| 画板 | 用到的数据 | 落在哪个模块 | 阶段 |
|---|---|---|---|
| 首页 · 开始录音 | 状态里的下一场设置；视图 `library`（侧栏）；主题选择 | ui/home，core/app | P3 |
| 录音中 · 只在本机 | 信号 `Frame`、`Session`、`Level`；命令 Pause / Stop | live、ui/live | P1 + P3 |
| 录音中 · 边录边记 | 同上，加命令 `Note{at, anchor}`；视图 `recording.notes` | record/notes | P2 + P3 |
| 停止以后 | 信号 `Progress(refine)`；视图 `transcript(take 0)` | write、view | P2 + P3 |
| 精修稿（写好 / 精修中 / 没精修 / 没写成） | 视图 `recording.takes`、`transcript`、`compare`；命令 `Refine{id}`、`Retry` | write、view/compare | P2 + P6 |
| 读一场录音 | 视图 `transcript`（虚拟列表）；命令 Change、Note；「记进知识库」 | view、knowledge | P3 + P6 |
| 全部录音（录音很多时） | 视图 `library`（按月）、`search`；⌘K 快捷键 | view/index | P2 + P3 |
| 标题与话题 | `talk/recording` 的 title、tags 及其来源；后台任务 | ai/titles | P6 |
| 知识库 | 视图 `knowledge`；命令 Topic、FromFile | knowledge | P6 |
| 主持 · 开场前 | 视图 `calendar`；状态里的主持中这一场（不落盘）；房间链接和二维码；等待人数 | out/host | P5 |
| 主持 · 对外中 | 信号 `Session.outbound`、`viewers`；命令 StopOutbound | out/host、server | P5 |
| 主持 · 交到档案馆 | 视图 `recording`；命令 `File`，界面上按住 1.4 秒、留 8 秒可撤销 | out/city | P5 |
| 设置 · 对外 | 视图 `outbound` | out/city | P5 |
| 设置 | 状态（身份、额度、钥匙）；设备偏好 | id、quota | P4 |
| 身份 · 需要时才问 / 扫码 / 核对数字 / 新建身份 | 命令 SignIn、Create、Recover；状态里的登录进度。只在按下要登录的功能时弹出（§2.7），录音不要登录 | id | P4（不登录也能录：P1） |
| 状态一览 | 每个状态都是数据，不是临时提示（§4.3） | 全部 | 贯穿 |
| 设计语言 | 白纸表示只在本机，色带表示对外；颜色、字体、动效 | ui/ds | P3 |
| 字幕窗 · 近看 / 场合与设置 | 信号 `Frame`；设备偏好里的样式；窗口的大小、位置、穿透点击、全屏 | shell/windows、ui/captions | P3 + P5 |
| 观众手机 · 现场字幕 / 字幕结束 | 房间槽里的 `Frame`（密文）、`gone` | ui/viewer、server | P5 |

---

## 4. 运行时：分层与隔离

### 4.1 分层

| 层 | 预算 | 跑在哪里 | 禁止做的事 | 输入 → 输出 |
|---|---|---|---|---|
| L0 采集 | 几毫秒（硬实时） | 设备回调；每个声源一个线程 | 分配内存、加锁、读写磁盘 | 设备 → `rtrb` |
| L1 泵 | 100 ms 一块 | 泵线程 | 读写磁盘 | `rtrb` → broadcast(64)，以及给 tape 的有上限队列 |
| L1 现场 | 说出到上屏 p99 ≤ 1 秒 | 每条 lane 一个任务，外加帧任务 | 读写磁盘、等其他层 | broadcast → `Frame` 寄存器 |
| L2 记录 | 几秒 | 写 tape 的线程；写草稿的线程 | 回头挡住 L1 | 有上限的队列；满了就报 `tape` 并结束这场录音 |
| L3 查询 | ≤100 ms，可以取消 | `spawn_blocking` 工作池 | 持有全局锁 | 查询 → 结果；新的查询来了，旧的取消 |
| L4 后台 | 几分钟 | 后台任务，一次一件 | 抢占前面几层 | 进度作为信号 |
| hub | ≤1 ms 一条命令 | 一个任务 | 读写磁盘、做网络请求 | 命令 → 小状态 + 交给工作线程的副作用 |
| 外壳 | 不阻塞 | Tauri 主线程 + 推送任务 | 持锁时调用窗口查询；同步命令 | 寄存器 → Channel |
| 界面 | 一帧 16 ms | 网页主线程 | 做和历史长度成正比的工作 | 信号（每帧最多处理一次）+ 视图（按范围拉） |

**规则：** 高层不等低层。层与层之间只有两种连接：
- 有上限的队列，并写明满了以后怎么办；
- 最新值寄存器。

### 4.2 现在的代码要动的地方

| 现在 | 改成 | P1 之后 |
|---|---|---|
| `Msg::Live` 经过 hub 的无上限邮箱 | 录音任务直接写 `Frame` 寄存器（`watch`）；房间的 `slot()` 和外壳的推送任务都读它。房间现在已经是这样写的，照抄过来 | 已做（P1-11、P1-12）：`Frame`、`Session`、`Level`、`Water`、`Stats`、`Notices` 全由寄存器（`Outlet`）直出，绕开 hub |
| hub 在 `refresh()`、`Search`、`Export` 里同步读写磁盘 | 交给 L3 工作池；结果作为消息回到 hub；刷新先标「脏」，再合并成一次 | 已做（P1-19、P1-20）：`hub.rs` 零 IO（`hub.sh` 守着），读写与刷新合并在 `Work` 的顺序 `desk`，搜索与导出写盘在独立任务 |
| `mpsc::unbounded_channel` | 全部换成有上限的；用 clippy 的 `disallowed-methods` 禁掉 | 已做（P1-04、P1-13、P1-14、P1-21）：全仓核心与外壳通道收进 `rt::queue`，`clippy.toml` 与 `bounded.sh` 守着 |
| 外壳 `fan` 拿着锁调用 `is_visible()`；`subscribe` 是同步命令 | 外壳不持锁（用 `arc-swap` 存订阅者和最后一份状态）；所有命令 async；字幕窗该不该显示由自己记住，不去问系统；页面重新加载后清掉旧的 Channel | 已做（P1-23、P1-24）：改为单任务独占状态机 `Fan`（未用 `arc-swap`），零锁全 async，`windows.rs` 不查询系统，`shell.sh` 守着 |
| 泵线程直接写 tape（`write_all`，每分钟同步一次磁盘） | 泵只往有上限的队列里放，由写 tape 的线程落盘 | 已做（P1-15）：经 600 块有界队列交给 `rt::keep`（async 任务 + 分批阻塞调用，而非独立线程），坏了重开静音补洞 |
| `State` 里装着 library、open、hits、terms | 移到视图（§2.5） | **P2-06 已做**：`library`/`open`/`hits`/`terms`/`city`/`writing`/`filing`/`moving` 全移出 `State`，改走 `Core::query`、`Outlet.progress` 与 `Outlet.invalidated`；`i8_*` 标记测试已转正通过 |
| `Backoff` 封顶 8 秒，然后无限重试 | 加「重启次数上限」：一分钟内失败 N 次，或停滞超过 T，就把这条判为「停了」，界面显示原因并给出「重连」 | 已做（P1-09、P1-17、P1-18）：`Restarts`（1 分钟 8 次或 120 秒无健康运行）到限转 `Dead` 发 `part.gave_up`，`Command::Reconnect` 重拉 |

### 4.3 活性：水位线与判断

每层报告一个单调递增的位置：

- 采集：采到第几毫秒（`captured_ms`）；
- 每条 lane：发出去多少（`sent_ms`）、服务处理到哪里（`heard_ms`）、定稿到哪里（`final_ms`）；
- 帧任务：发到第几帧（`frame_seq`）及其时间；
- 外壳：推到第几帧（`pushed_seq`）；
- 界面：画到第几帧（`drawn_seq`），由界面回报。

再用一个纯函数做判断：

```rust
judge(&Water, now, &Budgets) -> Vec<(Stage, Health, Action)>
```

- `Health` 有四种：`Live`、`Slow`、`Stalled`、`Dead(code)`。
- `Action` 有：`None`、`Reopen(lane)`、`Tell(code)`、`EndRecording(code)`。

状态一览里的「正在录音 · 只录音」「文字慢了一点」「没联网」「没有麦克风权限」，都是 `judge` 的输出，有固定的代码。0.8 那次「取钥匙卡了 14 分钟」在这里会表现为：`Stalled(keys)` 超过 T，界面显示原因，并且换一条路取钥匙或者重开连接，不再静默重试 39 次。

### 4.4 恢复

- **监督树**：录音任务管 lane、帧任务、写 tape、写草稿；hub 管录音任务和后台任务；外壳管推送任务。都用 `tokio-util` 的 `TaskTracker` 加 `CancellationToken`，配合现有的 `supervise.rs`。
- **页面重新加载零成本**：订阅 = 一份小状态 + 每个信号的当前值；视图让界面重新拉。
- **崩溃之后**：tape 截掉残尾；草稿已经逐行落盘；上次留下的主持房间在下次启动时删掉（§5.5）；中断的精修和交城继续。

---

### 4.5 显式与可观测（2026-11-21 负责人：「写的需要是显式的，并且可观测的」）

0.8 的故障多半是这样查出来的：先到系统日志里翻，再在主线程卡死时抓一次采样，一天以后才拼出原因。现在的重写版核心里一行日志都没有。这一节把「显式」和「可观测」写成可以检查的规矩。从 P1 起，每件任务都照着做。

#### 4.5.1 显式：读代码就知道会发生什么

| 规矩 | 具体 | 怎么查 |
|---|---|---|
| 状态是枚举，转换是函数 | 录音、lane、连接、监督、主持、交城、精修，各有一个 `enum` 写出全部状态，加一个纯函数 `step(state, event, now) -> (state, effects)`。不用几个 `bool` 拼出状态；不可能的状态写不出来 | 审查；每个状态机有转换表测试 |
| 数有名字、有单位、有出处 | 时限、上限、预算、重试，都是有名字的常量，名字带单位（`_MS`、`_BLOCKS`、`_BYTES`），注释写为什么是这个数，或者指向 `DECISIONS.md`。代码里不出现没有名字的数 | 规则脚本 `numbers.sh`：`core/src` 里 `Duration::from_*`、`channel(`、`sleep` 的参数必须是常量名 |
| 队列在构造处写明上限和满了怎么办 | 用 `rt::queue::bounded(name, capacity, Full::Drop/Coalesce/Reject/End)`，不直接调 `mpsc::channel` | 规则脚本 `bounded.sh` |
| 每个任务有名字、有主人 | 起任务只用 `rt::supervise::Tree::spawn(name, …)`；没有匿名的 `tokio::spawn`、`thread::spawn` | 规则脚本 `spawn.sh`（`rt/` 自己除外） |
| 丢弃要写理由 | `let _ = …` 丢掉一个 `Result` 或发送失败，同一行写 `// dropped: <为什么没关系>`；不能没关系的就当失败报出来 | 规则脚本 `dropped.sh` |
| 副作用只走有名字的端口 | 磁盘、网络、设备、时钟、随机数，都是 `Config` 上有名字的端口；规则函数的参数里看得到它用到的一切 | 已有：规则不做 IO；`hub.sh` |
| 配置一处写全 | `Config` 的每个字段都有注释；没有藏在别处的环境变量（只允许 `ZUTALK_` 开头、并列在 `Config::from_env` 一处） | 审查 |

> **P1 实际做法（§4.5.1）**：10 个状态机均用手写枚举 + 纯函数 `step` 并配转换表测试（未引 `statig`）；`rt::queue` 的构造额外传入 `&Stats` 以自动登记统计，`Full` 为 `Wait`/`Drop`/`Reject`/`End`（`Coalesce` 归最新值寄存器 `rt::signal::Register`）；五个规则脚本同步覆盖 `core/src`、`app/src`、`server/src`、`log/src`（`spawn.sh`、`bounded.sh` 对 `server/src` 的 4 处既有项记在 `later` 表，留待 P4/P5 还）。

#### 4.5.2 可观测：不用调试器，就知道它在做什么、卡在哪里

**一、事件日志：结构化，只有代码和数字。**

- 用 `tracing`。写到 `<数据目录>/logs/zutalk.jsonl`，一行一个 JSON；按大小轮换（每个 8 MB，留 5 个）；开发时同时打到标准错误。
- 每个事件有：时间、`component`（模块路径）、`event`（稳定的代码，比如 `lane.reopen`、`queue.full`、`judge.stalled`）、相关的 id（录音 id、lane 序号、帧序号），以及数字。
- **永远不记**：说的话、译文、标题、笔记、人名、钥匙、链接里 `#` 后面的部分。原则 6 照旧。事件的字段类型只许是代码、id、数字、布尔值。台架里有一条测试：跑完一场录音，翻遍日志，找不到任何一个「话」里的词。
- **必须记的**：
  - 每个状态机的每次转换（从哪个状态到哪个状态，因为什么）；
  - 每次开、关、重开、退避（多久）、到达重启上限；
  - 每次队列满（哪个队列、怎么处理的）；
  - `judge` 的每次判断变化；
  - 每条命令（种类、结果代码、用时）；
  - 每件 desk 和后台的活（种类、用时、结果）；
  - 启动的每一步（用时）；
  - 外壳的订阅、换通道、窗口显隐；
  - 每分钟一行汇总（见下面第二条）。
- 级别是数据：`error` 是会让人丢东西的，`warn` 是自己恢复了的，`info` 是状态转换，`debug` 是每帧、每块级别的（默认不开）。

**二、计数与分布：在进程里，随时可读。**

- `rt::stats`：计数器（帧数、字节、重开次数、队列满次数、丢块数）、高水位（每个队列最深到过多少）、分布（各阶段延迟，用固定的桶，不用分配内存）。
- 每分钟写一行汇总进日志；也作为信号 `Stats` 给界面（1 Hz 以内，大小固定）。

**三、水位线（§4.3）是现在怎么样，事件日志是怎么变成这样的。**

`Water` 每秒一份。看守判断一格「停滞」或「停了」时，把当时的整份 `Water`、`Stats` 和任务树的快照写成一条 `stall.snapshot` 事件，同一格同一个原因只写一次。

**四、自检包：一键导出。**

`Command::Diagnose` 在 desk 上生成一个文件夹（之后打成 zip），内容是：
- 版本、系统、设置（不含秘密）；
- 最近的日志；
- 当前的 `Water`、`Stats`、任务树；
- 每个队列的深度。

不含任何内容和钥匙。设置里有一个「导出诊断」（P3 画），文件由本人决定要不要发出去。

**五、外壳和界面也报。**

- 外壳每秒用 `run_on_main_thread` 投一次「心跳」，不等它回来：回来的时候记下延迟。超过 2 秒没回来，记 `shell.main_thread.stalled`，回来以后记一共停了多久。这是 0.8 的主线程看门狗，但不阻塞任何东西。
- 界面每次画了一帧，回报帧序号（`drawn`）；`requestAnimationFrame` 两次之间隔了超过 250 ms，记一条 `ui.frame.gap`，经命令送回核心的日志。
- 开发构建里，界面上有一个可以打开的「运行状况」浮层：画 `Water` 和 `Stats`（P3 画）。

**六、可观测也是验收的一部分。**

- 台架里每种注入的故障，除了断言「恢复了」，还要断言「看得见」：日志里有对应的事件代码，`Water` 里有对应的判断。
- 审查时，「出了这种事，看日志能不能知道」是一条必查项。

> **P1 实际做法（§4.5.2，详见 `P1.md` §3.13.15）**：日志独立成 crate `log/`（`zutalk-log`，服务器共用，全仓仅它依赖 `tracing`），靠宏写入 callsite 记号 `zutalk:<代码>` 过滤外部库事件；66 个稳定事件全登记在 `docs/EVENTS.md` 并由 `events.sh` 双向守卫；统计用固定 13 桶（未引 `hdrhistogram`）；`stall.snapshot` 与每分钟汇总按平铺行输出（分别以递增 `snap` 号和 `stats.summary` 串联）；界面帧序号与帧间隔合并为 1 Hz 单命令 `drawn(seq, gap_ms, gaps)`；取消独立 `ZUTALK_WATER_LOG`，改由 `ZUTALK_LOG=debug` 输出每秒 `water.mark`；E10 已在 P1-25 逐项核对（含三场故障「只看日志」序列测试与两条落盘隐私测试）。

---

## 5. 模块

### 5.1 目录

```text
zu/                     Zu 协议内核（不变；P1-06 为本机身份开窄口：`zu::did::local` / `local_key`，仅 `Path::new` 接受 Ed25519 `did:key` 作作者，`is_did` 不变，任何主机不收，见 `P1.md` §3.14.2）
core/src/
  rt/                   运行时，不认识任何业务
    signal.rs           Register<T>：最新值 + seq；watch 的薄包装，带大小断言
    water.rs            水位线、预算、judge()
    supervise.rs        Backoff、supervise，加上重启次数上限
  capture/              L0–L2
    audio.rs  audio/file.rs  clock.rs  tape.rs（加写盘线程）
  live/                 L1
    lane.rs  lines.rs  lang.rs  frame.rs（由现在的 View 搬来，输出 Frame）
                        lane.rs 是原来的 live.rs：不叫 live/live.rs，因为 clippy 的 module_inception
  record/               事实
    shelf.rs  notes.rs  takes.rs  retain.rs  migrate.rs
  knowledge/
    topic.rs            主题包，取代 glossary.rs；给现场的 context 与精修的 context
  ai/                   L4
    write.rs            精修（整场转写、翻译、检查）
    titles.rs           标题与话题（OpenRouter）
  view/                 L3，可丢弃的投影
    index.rs            SQLite：library、search（FTS5 trigram）、计数
    transcript.rs       分块、版本号、compare
    query.rs            Query → Result；Invalidate
  out/
    host.rs             这一场的直播房间：建、写槽、人数、删；不落盘
    city.rs             交城（record 的类型要和档案馆 v0.3 对齐，§9 第 6 条）
    export.rs
  id/
    identity.rs         登录（扫码、核对数字）、新建、恢复密钥、句柄
    keys.rs             各服务钥匙、派生密钥、签名请求
    quota.rs            城给的额度：出字、AI，每天的次数
  app/
    hub.rs              命令 → 小状态 + 副作用（交给工作线程）
    proto.rs            Command / Query / Event / Shell 状态 / 信号类型（ts-rs 生成）
app/src/                外壳
  main.rs  pump.rs（信号 → Channel）  windows.rs（字幕窗的大小、位置、穿透点击、舞台全屏）
  updates.rs
ui/                     见 §6
server/src/
  room.rs               只有槽：make / slot / gone / viewers；房间存活时间改为「多久没写就失效」
  ledger.rs  relay.rs  quota（按服务、按天）
```

### 5.2 协议

```text
界面 → 核心:  dispatch(Command) : async → Result<(), Code>
             query(Query)       : async → (Value, version)
核心 → 界面:  Channel<Event>
             Event = Shell(ShellState)                  // 小状态，整份
                   | Signal(Frame | Session | Level | Progress | Water)
                   | Invalidate { view, key, version }
                   | Notice(Code)
```

- 每种信号带 `seq`。界面收到后先存下来，在 `requestAnimationFrame` 里只处理最新那一份。
- 测试断言 `Frame` 序列化后 ≤ 8 KB。

### 5.3 依赖（只加这些）

- 核心：`arc-swap`、`tokio-util`、`rusqlite`（bundled，用作索引）、`hdrhistogram`；
- 只在调试和测试时用：`assert_no_alloc`、`tokio-metrics`、`turmoil`、`loom`；
- 可选：`statig`（录音和 lane 的状态机）；
- 界面：`virtua`（虚拟列表）。

> **P1 实际变更**：新增工作区 crate `log/`（`zutalk-log`，依赖 `tracing` 与 `serde_json`，`tracing` 本已是间接依赖）；核心加 `zutalk-log`、`tokio-util`（`rt`），`serde` 开 `rc` 特性；外壳加 `zutalk-log`、`tokio`（`sync`、`macros`）；服务器加 `zutalk-log`。**P1 未引入**：`arc-swap`（外壳改为单任务独占 `Fan`，无共享状态）、`hdrhistogram`（固定 13 桶无分配实现）、`loom`（无共享锁，由 `app/tests/pump.rs` 穷举交错）、`turmoil`（`Dial`/`Keys` 替身在暂停时钟下确定模拟断网/不回话/超时）、`tokio-metrics`（已有 `Tree::running()` 与 `rt::stats`）、`statig`（状态机小，手写枚举 + `step`）、`assert_no_alloc`（未需新加依赖，回调零分配由 `core/tests/callback.rs` 测）；`rusqlite` 留给 P2，`virtua` 留给 P3。

> **P2 实际变更**：在 `core/Cargo.toml` 仅新增一项依赖 `rusqlite = { version = "0.37", features = ["bundled"] }`（内置 SQLite + FTS5 `trigram`），无其他新增 crate；同时完成 `core/src/record/{notes,takes}.rs` 与 `core/src/view/{index,transcript,query}.rs`，清零 `layers.sh` 的两处 P2 欠账（`record/retain.rs`、`record/migrate.rs`）。`virtua` 留给 P3。

> **P3 实际变更**：`ui/package.json` 彻底移除 Svelte 三项（`svelte`、`svelte-check`、`@sveltejs/vite-plugin-svelte`），新增运行时依赖 `react@^19`、`react-dom@^19`、`virtua@^0.56`（连同既有 `@tauri-apps/api`、`@tauri-apps/plugin-dialog`、`uqr`）及开发/测试依赖 `@vitejs/plugin-react`、`@types/react`、`@types/react-dom`、`@testing-library/react`、`jsdom`；Rust 侧零改动、零协议变更。

> **P4 实际变更**：**零新增外部依赖**（`Cargo.lock` 与 `ui/package-lock.json` 无新增外部包）；仅在 `server/Cargo.toml` 复用锁文件既有的 `tokio-util = { version = "0.7.20", features = ["rt"] }` 以搭建 `server/src/tasks.rs` 监督树并还清 `spawn.sh` 的 P4 欠账。

> **P5 实际变更**：**零新增外部依赖**（`Cargo.lock` 与 `ui/package-lock.json` 零变更）；服务器复用既有 `tokio::sync::watch` / `WatchStream` 与 `tokio_util::sync::CancellationToken`，界面二维码复用既有 `uqr`。全仓 `bounded.sh`、`spawn.sh`、`layers.sh` 三份规则脚本欠账**全部清零**。

> **P6 实际变更**：**零新增外部依赖**（`Cargo.lock` 与 `ui/package-lock.json` 零变更）；知识库文件导入使用标准库 `std::fs` 实现有界 UTF-8 `.txt`/`.md`/`.csv`/`.tsv` 提取（上限 512 KiB），零新增 `Stats` 计数/分布行，全仓 10 个规则脚本零欠账全绿。

### 5.4 写法规矩（加进 `CLAUDE.md` 的 Rules）

- 没有无上限的队列；每个队列写明满了以后怎么办。
- 外壳没有同步命令；任何锁都不跨越 await，也不跨越窗口调用。
- hub 和 L1 里不读写磁盘、不做网络请求；只有 `capture`、`record`、`view`、`out`、`id` 的端口做 IO。
- 界面的列表一律虚拟化；不按历史长度做工作。
- 每个新的长期运行任务都挂在监督树上，并报告水位线。

### 5.5 主持：这一场的东西，不落盘

- 「开始主持」：在服务器上建房间，把房间 id 和钥匙**只放在内存里**，另写一个本机文件 `host.lock`（只放房间 id）。
- 每次写槽时，服务器回报人数，进信号 `Session.viewers`。
- 停止对外、停止录音、离开首页、应用退出：都发 `gone`，并删掉 `host.lock`。
- 崩溃以后：下次启动看到 `host.lock`，就发 `gone`。服务器那边，一个房间 2 分钟没有写入就失效（现在是 30 天）。两道保险。
- 交到档案馆只在录完以后，一场一次，按住才交；和主持是两件事。

---

## 6. 界面

### 6.1 先定框架（§9 第 1 条）

pol 已经按 0160 定成 dsh 式 React：组件只拿 props，数据以「源」（`getSnapshot` + `subscribe`）和「动作」到达。这和本文的「信号 / 视图 / 命令」一一对应。

建议 ZuTalk 的界面也照这个写。理由：
- 两个产品共用 4seas 设计系统、登录与身份的界面、同一份质量标准；
- 新设计的屏是现在的好几倍，现有的 725 行 Svelte 本来就要重写，现在换框架代价最低。

不换也可以：Svelte 5 的 runes 一样能写出源和动作。请负责人定。

### 6.2 结构（不管用哪个框架）

```text
ui/src/
  bus/       channel.ts（订阅；信号先存下来，每帧只处理最新的）
             query.ts（视图缓存：key → {value, version}；收到 Invalidate 再决定要不要拉）
             act.ts（命令）
  ds/        设计系统：纸与色带、颜色、Inter / 霞鹜文楷 / Oxanium、动效时长；来自 System 画板
  shell/     两栏版面、侧栏（按月收起）、顶部色带、提示、⌘K
  pages/     home  live  result  reader  library  knowledge  host  settings  identity
  captions/  字幕窗：场合、样式；只订阅 Frame + 设备偏好
  viewer/    观众页：只读房间的槽
```

### 6.3 界面的不变量

- 只有一个主按钮；橙色只表示正在收音，红色只表示撤不回；离开设备的东西只有色带一种样子。这些来自设计稿上写的规矩。
- 阅读页和全部录音都是虚拟列表；DOM 里的节点数只和屏幕高度有关。
- 每个信号旁边都能显示它的新鲜度；字幕停了，界面上看得见。

> **P3 实际做法（§6）**：照 `DECISIONS.md` 第 1 条采用 React 19 + TypeScript + `virtua` 的 dsh 式源/动作结构；`useSyncExternalStore` 与 `@tauri-apps/*` IPC 严格收口在 `ui/src/bus/` 并由 `scripts/build/rules/ui.sh` 静态守卫（同时禁绝 Svelte 残留、`console.log`、CSS 外网 `@import` 与 `dangerouslySetInnerHTML`）。`ui/src/bus/` 实现 rAF 信号合并、单调序守卫、128 项上限活跃订阅 `QueryCache`（含多块聚合 `useQueries`）、`routeStep` 路由状态机与 8 语 312 键字典；`ui/src/ds/` 落地 4seas/ZuTalk 亮/暗令牌、离线字体栈、`InlineConfirm` 两步确认机与 `stateView` 穷举映射；`ui/src/shell/` 与 `ui/src/pages/` 实装两栏外壳、`⌘K` 搜索、开发态 `HealthOverlay` 与全部 6 个主流程页，外加零查询隔离的 `CaptionsWindow` 与保留 AES-256-GCM 解密的 `ViewerPage`。外观偏好在 P3 零 Rust 改动约束下暂存 WebView `localStorage`。

---

## 7. 测试与守护

| 不变量 | 怎么测 |
|---|---|
| I1 说出到上屏 p99 ≤ 1 秒，与录音时长无关 | 模拟台架：暂停的 tokio 时间，用 0.8 的回放夹具拼成 8 小时；按小时分段，p99 不能上升。**P1 实测**：`core/tests/liveness.rs::i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`（8 虚拟小时双语，每小时 p99=321 ms，最大帧 6125 B ≤ 8192 B）与 `i1_three_languages_fit_a_frame`（30 虚拟分钟三语） |
| I2 后台再忙也不影响字幕 | 同一台架上同时跑：10 万行的搜索、175 条搬运、导出、精修。**P1 实测**：`i2_a_tape_that_stands_still_holds_no_caption_back`、`i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands`、`i2_background_work_runs_beside_a_recording` 加 `scripts/build/rules/hub.sh`。**P2 实测**：`core/tests/view_scale.rs::i2_concurrent_view_queries_do_not_stall_live_captions`（1,000 条录音库上直播期间并发 20 次混合 SQLite 查询，帧持续推进、零 `Stalled`，175 条真库搬运留 P7） |
| I3 内存有上限 | 长跑测试中各进程的内存不增长；所有队列的长度都有记录。**P2 实测**：`core/tests/view_scale.rs::i3_rebuild_queries_and_draft_stay_within_bounds`（`DraftWriter` 48 B 恒定写 10,000 行、100 条损坏流式重建 94 ms、50 次混合查询 + 200 次失效后 `Invalidations` 封顶 64、`stats.full = 0`、`Tally = 390 B ≤ 8192 B`、队列深度与 `Tree::running()` 全归零） |
| I4 没有无上限的队列，外壳没有同步命令 | clippy 的 `disallowed-methods`，加一个检查脚本（`clippy.toml` + `bounded.sh` + `shell.sh`） |
| I5 外壳不会死锁 | 用 `loom` 给外壳的订阅和推送协议建模；也测「页面重新加载时正好在推送」。**P1 实测**：单任务独占 `Fan` 无锁，`app/tests/pump.rs::every_interleaving_ends_with_each_window_holding_the_latest` 穷举交错 + `a_reload_while_frames_are_coming_loses_nothing_and_the_old_channel_goes_quiet` + `shell.sh` |
| I6 任何部件都能被杀掉并自愈 | 随机杀掉 lane、帧任务、写盘线程、网页，N 秒内恢复，事实不丢失。**P1 实测**：`i6_a_lane_that_dies_comes_back_and_marks_what_it_missed`、`i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence`、`core/tests/supervise.rs::a_part_that_panics_is_opened_again`、`core/tests/keep.rs`、`app/tests/pump.rs::a_pump_that_dies_is_started_again_and_no_window_has_to_join_again` |
| I7 停滞一定被看见 | 用 `turmoil` 模拟：Soniox 连着但不回话、取钥匙卡住、断网；`judge` 在预算内给出 `Stalled` 并采取动作。**P1 实测**：暂停时钟 + `Dial`/`Keys` 替身：`core/tests/water.rs`、`i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection`、`i7_a_capture_that_stops_delivering_ends_the_recording_out_loud`、`i7_offline_is_said_and_captions_stop_trying_until_asked`、`core/tests/live.rs::a_lease_that_never_comes_is_given_up_on` |
| I8 每次更新的代价与历史无关 | 测 `Frame` 生成、视图查询、界面渲染的耗时，在 1 分钟和 4 小时两种长度下之比 ≈ 1。**P2 实测**：`i8_what_one_change_sends_does_not_grow_with_the_library` 已转正通过（5 条 vs 100 条 `State = 441 B / 441 B`，比值 `1.00`，`Invalidate = 60 B`）；`core/tests/view_scale.rs::i8_transcript_and_compare_chunk_cost_does_not_grow_with_duration`（1 分钟 vs 4 小时 2,560 行，64 行单块 JSON 封顶 13.6 KB / 26.0 KB，耗时均 ≤ 12 ms）。**P3 界面侧实测**：`ui/tests/scale_i8.test.tsx` 在 600 px 模拟视口下实测 1 分钟（16 行）与 4 小时（2,560 行 = 40 块）阅读页挂载 DOM 行数均为 `11`（比值 `1.00`，远 ≤ 40），4 小时仅按需拉取 `2` 块（绝不拉第 39 块）；15 条与 500 条资料库挂载 DOM 行数均为 `11`（比值 `1.00`）；同帧突灌 50 帧 `Frame` + 50 次 `Level` 合并为 `1` 次 React 提交。**P4 复测**：5 条 vs 100 条 `State = 523 B / 525 B`（比值 `1.00`）。**P5 复测**：5 条 vs 100 条 `State = 1320 B / 1322 B`（比值 `1.00`）。**P6 复测**：`Setup.topics` 封顶 8 项后 5 条 vs 100 条 `State = 1332 B / 1334 B`（比值恒为 `1.00`），`Invalidate = 60 B`；`ai::titles::sample_transcript` 对 4 小时（2,560 行）录音 `O(1)` 采样封顶 `6,000` 字符；`scale_i8.test.tsx` 保持绿灯 |
| **I9 显式与可观测**（原则 10，§4.5） | **P1–P6 实测**：全部 10 个规则脚本全绿且 **`bounded.sh` / `spawn.sh` / `layers.sh` 全仓欠账归零**，`pending.sh` 升级同禁 P1–P6，83 个 Rust 事件入 `docs/EVENTS.md`（P6 新增 `topic.saved`、`topic.context`、`titles.done`），P6 零新增 `Stats` 行（38/40 计数）、`stats.full == 0`、`Tally ≤ 8192 B`；P6 新增 4 组纯函数规则与转换表测试（`topic::normalize` / `size_chars`、`topic::assemble_context`、`titles::title_plan`、界面 `topicBudgetPercent` / `topicDraftStep` / `findLineIndexForTime`）；端到端读书会测试与真实落盘隐私审计 `core/tests/knowledge.rs` 验证 `logs/*.jsonl`、`diagnose/**/*` 与 `settings.json` 零内容/主题名/文件名/标题/话题泄漏且 `zutalk_log::refused() == 0` |
| 真机 | `core/tests/audio.rs` 开头的手工清单，三个系统；8 小时实录一次 |

**先写测试**：P1 的第一件事是让台架在现在的代码上跑起来，并且让它失败。失败的地方就是要改的地方。

---

## 8. 路线图

每一期都有「出口」，过了才进下一期。时间是估计，按一个人加一个 Opus 代理算，可以两条线并行。

| 期 | 内容 | 出口 | 估计 |
|---|---|---|---|
| **P0 定稿** | 负责人定 §9 的几件事；本文并入 `DESIGN.md`；`CLAUDE.md` 加原则 8、9 和 §5.4 的规矩；设计稿进仓库（已复制到 `docs/ui-2026-11-21/`） | 文档定稿 | 1–2 天 |
| **P1 运行时地基** | 模拟台架和 I1–I8（先让它失败）；`rt/`（signal、water、supervise）；`Frame` 绕开 hub；外壳无锁、全部 async、`pump.rs`；hub 不再读写磁盘；有上限的队列加 lint；tape 写盘线程；`wall_clock_policy`（照临时决定实现，数字等负责人确认）。**已做（P1-01 至 P1-26）**：补齐显式与可观测（`zutalk-log`、9 个规则脚本、自检包、心跳）与不登录也能录（§2.7 归 P1 部分） | I1、I2、I4、I5、I6、I7（核心）、I9、E11 全在门禁通过；**出口 E9（真机录 1 小时不卡）等负责人执行**（见 `docs/build/reports/P1-26-close.md`） | 1.5 周 |
| **P2 数据层** | `view/`（SQLite 索引、FTS、分块、compare）；`Query` 与 `Invalidate`；状态瘦身；能读旧版本；带时间的笔记；精修可以手动触发、失败会留下状态。**已做（P2-01 至 P2-08）**：`record/{notes,takes}.rs`、`DraftWriter`、`view/{index,transcript,query}.rs`、`State` 瘦身、`Outlet.{progress,invalidated}`、Tauri `async fn query`、Svelte 最小适配、`layers.sh` P2 欠账清零、`pending.sh` 升级 | **E1–E8 全通过**：I2、I3、I8、I9 均在门禁守着；1,000 条录音查询 0.64–7.37 ms、10 小时正文与对比 5.44–12.76 ms（全 ≤ 100 ms）；见 `docs/build/reports/P2-08-close.md` | 1.5 周 |
| **P3 界面主流程** | 框架定下；`ds/` 设计系统；两栏版面；首页、录音中、边录边记、停止以后、精修稿、读一场录音、全部录音；字幕窗基本款；状态一览里各种状态都有画法。**已做（P3-01 至 P3-07）**：零 Rust 协议改动，切至 React 19 + TypeScript + `virtua` dsh 结构；落地 `bus/`（rAF 合并、128 项活跃订阅 `QueryCache`、`routeStep`）、`ds/`（亮/暗令牌、`InlineConfirm`、`stateView`）、`shell/`（侧栏、顶栏、通知、`⌘K`、`HealthOverlay`）、全部 6 个主流程页、零查询 `CaptionsWindow`、React `ViewerPage`、8 语 312 键字典、`ui.sh` 与升级后 `pending.sh` | **E1–E7 自动化出口全通过**（12 份 UI 测试 76 条 + 10 个规则脚本 + 3 入口构建全绿；I8 阅读 16 vs 2,560 行与资料库 15 vs 500 条均恒挂 11 个 DOM 行、50 帧突灌合并 1 次提交）；**真机 WebView 视觉走查与 1 小时实录 I1 延迟等负责人核对**（见 `docs/build/reports/P3-07-close.md`） | 2 周（可以和 P2 后半并行：先用假数据） |
| **P4 身份与额度** | 需要时才问；扫码登录加核对数字（要先补 zu `login.md`，§9 第 5 条）；新建身份、恢复密钥、句柄；额度按服务、按天（服务器 + 城）；**领回本机身份名下的记录（§2.7）**。**已做（P4-01 至 P4-06）**：`server/src/tasks.rs` 监督账本任务并还清 `spawn.sh` P4 欠账、`POST /quota` 与按服务 UTC 日上限；`zu::did` 补 `create_with_aka`/`Rotation::from_multikey`/`add_signing_key`；核心 `id/{identity,quota}.rs`（`AuthFlow` 两段扫码确认、建号、恢复、句柄离线缓存、按服务额度）与 `record/adopt.rs`（首次登录重签领回、`adopted.json` 单人绑定、进行中本机录音封盘补领、`DiscardLocal`）；界面 `bus/identity.ts`、`IdentitySheet.tsx`（无登录墙按需弹出）与 `SettingsPage.tsx`（`@handle`+短 DID、出字/AI 每日进度条、本机领回备份横幅与两步清除）、8 语全量词条、全链路隐私落盘验收与 `pending.sh` 升级禁 P4 | **E1–E6 自动化出口全通过**（零新增外部依赖，10 个规则脚本全绿，I8 `State` 523 B / 525 B 比值 `1.00`，`stats.full == 0`，日志/自检包秘密与隐私零泄漏；**六位数派生算法、短句柄域名、线上目录与生产额度参数待负责人确认**，见 `docs/build/reports/P4-06-close.md`） | 1–1.5 周（服务器部分可以和 P3 并行） |
| **P5 主持与对外** | `out/host.rs`（不落盘、人数、两道保险）；房间只留槽；删掉单条录音的分享；色带；字幕窗的场合、大小、穿透点击、舞台全屏；观众页；按住交档案馆、8 秒撤销；记录类型和档案馆对齐。**已做（P5-01 至 P5-06）**：服务器无盘只留槽 `watch` 直播房间（2 分钟 TTL + `Tasks` 监督 `server.sweep`），清零 `bounded.sh` 与 `spawn.sh`；核心删除 `out/room.rs` 与 `talk/share`、新建 `out/host.rs`（`HostState` 状态机、内存持钥、`host.lock` 两道保险、`StopOutbound`）、`out/city.rs` 集中 `ARCHIVE_TYPES` 单表并清零 `layers.sh`；外壳 `Setup.caption` 4 场合持久化与无锁 `CaptionWindowTracker` + 防抖几何回写 + `ExitRequested` 异步 `gone`；界面 `HostPage`、直播色带、`FileArchiveCard`（1.4 s 按住 + 8 s 撤销）、`OutboundPage`、`CaptionScenesPanel`（16:10 预览）、4 场合零查询 `CaptionsWindow`（含 `Facing` 180° 与 10 s 隐去）、只留槽手机 `ViewerPage`、8 语全同步与 `pending.sh` 升级禁 P5 | **E1–E6 自动化出口全通过**（零新增外部依赖，全仓规则欠账彻底归零，I8 `State` `1320 B / 1322 B` 比值 `1.00`，双保险崩溃清理与落盘隐私审计全过；**档案馆 v0.3 类型确认、线上边缘反代与三平台真机字幕窗验证待负责人确认**，见 `docs/build/reports/P5-06-close.md`） | 1.5 周 |
| **P6 知识库与 AI** | `talk/topic` 与知识库页；主题送进现场和精修的 context；标题与话题；「记进知识库」；只用额度的人怎么精修（§9 第 4 条）。**已做（P6-01 至 P6-05）**：`knowledge/topic.rs`（`talk/topic`、固定 `always` 主题、旧 `talk/term` 无损迁移、零新 crate 有界 UTF-8 文件提取与建议词、`record/adopt.rs` 纯 JSON 领回守 `layers.sh`）；`TopicContext` 脱敏 `Debug` + `live/lane.rs` & `ai/write.rs` 双路上下文注入与未选主题隔离（`DECISIONS.md` #7）；`ai/titles.rs`（`title_plan`、`O(1)` 采样、OpenRouter 严格 JSON 零留存、独立 `TITLE_AT_ONCE_JOBS = 1` 信号量 + 第 4 槽 `Progress.title`、手写标题永不被覆写、额度键异步拒绝后草稿仍自动起标题）；SQLite `INDEX_SCHEMA_VERSION = 2` 索引 `title_by`/`tags`/`topics` 与 `SearchHitKind::Tag`/`LibraryFilter.tag`；界面 `KnowledgePage`、首页主题选择器、阅读页字重区分/换标题候选弹窗/话题胶囊跳转/「+ 记进知识库」、全部录音页话题副标题与过滤、8 语同步、端到端读书会与真实落盘隐私审计、`pending.sh` 升级禁 P6 | **E1–E6 自动化出口全通过**（零新增外部依赖、零新增 `Stats` 行、`stats.full == 0`、`Tally ≤ 8192 B`、I8 `State` `1332 B / 1334 B` 比值 `1.00`、真实落盘零泄漏；**真机真钥匙读书会实测与是否引入 PDF/DOCX 解析 crate 待负责人确认**，见 `docs/build/reports/P6-05-close.md`） | 1.5 周 |
| **P7 搬家与发布** | 打上并发布 0.8.6 交接补丁；在 175 条真数据的副本上搬一遍；更新签名钥匙；Sparkle 的 `zu` 频道；三个系统的手工清单；8 小时实录 | 发布清单（`RELEASE.md`）全部打钩 | 1 周 |

关键路径：P0 → P1 → P2 → P3 → P5 → P7。P4 的服务器部分、P6 的提示词与评测可以提前并行。

---

## 9. 请负责人定的事

1. **界面框架**：照 pol 改用 dsh 式 React，还是留在 Svelte 5（§6.1）。建议前者。
2. **「标记」回不回来**：0.8 重写时说过「删掉标记功能」，新设计里又有「标记 · M」。本文把它读成「把笔记钉在一句话上」，不另设标记类型。对不对？
3. **`wall_clock_policy`**：落后多少毫秒算抖动、补多少静音、多久算合盖要结束录音。已照 `docs/build/DECISIONS.md` 第 3 条实现（≤ 300 ms 不动、到 60 s 补静音、更长结束录音），数字等负责人确认。
4. **只用额度的人怎么精修**：服务器发的 Soniox 临时钥匙只能做实时识别，异步转写会被拒。三条路：
   - 服务器代发能做异步转写的钥匙（要和 Soniox 确认能不能这样发）；
   - 精修改用实时接口，以 1 倍速重放录音（慢，质量也低一档）；
   - 城提供一项转写服务。
5. **核对数字**：zu `login.md` 现在没有「两边各显示六位数」这一步（第 150 行把它列为未决问题）。要和 pol 一起先补进规范，ZuTalk 再实现。
6. **交给档案馆的记录类型**：现在交的是 `talk/*`；据 11-21 的核对，档案馆 v0.3 读的是 `tape/*`。以哪边为准？
7. **主题的「背景」要不要随声音发给出字服务**：设计稿写着「用到它的录音，会把这一页随声音发给出字服务」。这是隐私上的选择，请确认。
8. **原则修订**：§1 的两处改写和两条新增，是否照此写进 `CLAUDE.md`。

---

## 附：0.8 的经验在新结构里落在哪里

| 0.8 的事故 | 新结构里由谁防住 |
|---|---|
| 录得越久越卡（LazyVStack、全量比较） | 原则 9；`Frame` 有上限；虚拟列表；I8 |
| 断档后整场重拉，越慢越断 | 信号不排队；视图带版本号，按范围拉 |
| 计时器累加的逐字显示 | 显示由时间推算（`reveal.ts`），已有 |
| 滚动回调和布局互相触发 | 跟随最新行由虚拟列表负责；界面状态不写回 |
| 取钥匙卡了 14 分钟，录音键一直亮 | 水位线 + `judge`；重启次数上限；I7 |
| 麦克风停了 45 分钟 | 全零检测和重开（已有），加上采集水位线 |
| 录音中主线程卡死被强制退出 | 外壳无锁、全部 async；hub 不做 IO；I2、I5 |
