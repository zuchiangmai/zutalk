# 旧版录音搬过来：0.8 交接规格

> 2026-11-16。用过 0.8 的人第一次打开新版，什么都不用做，就能在新版里看到自己的录音。本文写定 0.8 这一边要做的事（交接格式 v1、何时写、怎么挡住没交接就更新），以及新版那一边怎么收。0.8 这一边的改动在 [migrate-0.8.patch](migrate-0.8.patch)，对着旧仓库 `9251050`（0.8.5）。

## 1. 旧版的数据在哪里、是什么

全部在 `~/Library/Application Support/ZuTalk/`（0.3 以前叫 `Zulangue`，0.8 启动时已经改名搬好，`LegacyIdentityMigration.swift`）。依据都是旧仓库的代码：

| 东西 | 位置 | 格式与依据 |
|---|---|---|
| 录音目录、转写事实、订正、说话人、标记 | `zutalk.db` | SQLite，schema v23–v37（`crates/vt-store/src/migration.rs`）。`session_records` 是标题、类型、时长、创建时间、回收站；`notebook_capture_runs` 是语言档案快照（`profile_snapshot_json`）、音频位置、钥匙引用、采样率与采样格式；`realtime_utterances` + `realtime_utterance_variants` + `realtime_utterance_overrides` 是现场字幕和本人的订正（`list_visible_notebook_utterances` 把订正叠上去）；`session_speakers` + `participants` 是说话人的名字；`session_marks` 是标记和它的笔记；`audio_retention_chunks` 是每个音频块的路径、起点、是否已销毁 |
| 整场转写及其订正 | `editor-docs/<doc_id>.loro` | Loro 富文本，一个笔记本一份（`async_transcript` 标签页），每段文字带 `session_id`、`segment_id`、`timestamp_ms`、`content_owner` 等标记（`crates/vt-ffi/src/editor_api.rs`）。本人改整场转写只改这里（`applyEdit`），SQLite 里没有 |
| 录音笔记 | `block-documents/session-note-v1-<sha256>.loro`（及 `.loro.updates`） | Loro 块文档（`crates/vt-ffi/src/block_document_api.rs`） |
| 音频 | `audio/<session>/chunk.NNNNN.enc`，一分钟一个 | 一串记录：`u32` 小端长度，再是 12 字节随机 nonce、AES-256-GCM 密文和 16 字节标签（`vt-crypto/src/decrypt.rs` 的 `encrypt_to_file`，`vt-pipeline/src/recording.rs` 的 `sealed_record`）。明文是交错 PCM：录音是 16 kHz 单声道 s16，早期是 f32，导入的文件按原采样率和声道（`session_meta`/`notebook_capture_runs` 的 `sample_rate`、`channels`、`sample_format`） |
| 音频钥匙 | `Secrets/content-keys.json` | `{"version":1,"keys":{"zutalk.audio.<session>":"<64 位十六进制>"}}`，每条录音一把（`vt-crypto/src/file_key_store.rs`）。销毁音频就是删掉这把钥匙和音频块 |

所以新版要么自己读 SQLite 和 Loro，要么让旧版把它们说成中性的东西。

## 2. 为什么由 0.8 交接，而不是新版直接读

| 方案 | 代价 |
|---|---|
| (a) 新版直接读旧格式 | 读事实要 `rusqlite` + `libsqlite3-sys`（约 4.8 万行 Rust，链接系统 SQLite）；读整场转写的订正和录音笔记还要 `loro` + `loro-internal`（约 8 万行，依赖闭包近两百个 crate），而新版的设计排除了 Loro。更要紧的是要在新版里重写旧版的读法：15 个 schema 版本、订正怎么叠、多语译文怎么从收件箱绑回句子、说话人名字从哪来、整场转写的段怎么切。这是第二份旧规则（原则 3、7）。只读 SQLite 不读 Loro，就会悄悄丢掉本人改过的整场转写和笔记（原则 5） |
| (b) 0.8 最后一个版本把录音写成中性的交接文件，新版收 | 新版不加任何依赖：交接文件就是新版记录的正文，音频块是 AES-GCM，新版本来就有。旧格式的知识留在它唯一的家，旧版用它自己的读法写出阅读页上看到的东西。代价是先发一个 0.8.6，并且要保证没人越过它直接更新到新版（见 §4） |
| (c) 新版只读音频，文字靠 0.8 交接 | 就是 (b)：音频本来就不进交接文件，新版按交接给的位置去原处读密文 |

选 (b)。交接文件不是 ZuTalk 的文件格式：它是新版记录的正文还没签名的样子，只用一次。

## 3. 交接格式 v1

目录 `~/Library/Application Support/ZuTalk/handover/`：

```text
handover/
  index.json                       最后写；它在，交接才算完整
  recordings/<0.8 的 session id>.json
```

`index.json`：

```json
{ "version": 1, "recordings": ["<session id>", "..."] }
```

`recordings` 是阅读页能看到的全部录音（回收站里的不算，笔记本已删或正在永久删除的不算）。某条录音 0.8 自己读不出来，它仍在 `recordings` 里，但没有文件：新版说「这一条没搬过来」，而不是不知道它存在。

每条录音一个文件：

```json
{
  "version": 1,
  "id": "<session id>",
  "about": {
    "started": 1731900000000,
    "length_ms": 3605000,
    "title": "…",
    "spoken": ["en", "zh"],
    "shown": ["en", "zh"],
    "source": ""
  },
  "takes": [
    [ { "t0": 0, "t1": 1200, "speaker": "1", "language": "en", "text": "…",
        "translations": { "zh": "…" }, "incomplete": false,
        "via": { "text": "stt-rt-v5", "zh": "hand" } } ],
    [ "…整场转写的行，同样的形状…" ]
  ],
  "speakers": { "1": "…" },
  "note": "…",
  "audio": {
    "key": "zutalk.audio.<session id>",
    "rate": 16000, "channels": 1, "format": "s16",
    "chunks": [ { "path": "audio/<session id>/chunk.00000.enc", "t_ms": 0 } ]
  }
}
```

逐项（左边是新版的记录，右边是 0.8 的来源）：

| 字段 | 新版里是 | 0.8 从哪来 |
|---|---|---|
| `about.started` | `talk/recording` 的 `started`（毫秒） | `session_records.created_at`（UTC），也接受 RFC 3339 |
| `about.length_ms` | 没有音频时的长度；有音频时以重新封好的 tape 为准 | `session_records.duration_ms` |
| `about.title` | 标题 | `session_records.title` |
| `about.spoken` / `about.shown` | 说的语言、显示的语言 | 运行时冻结的档案：`selected_languages`（空则左右两种）；`spoken` 去掉 `subtitle_only_languages` |
| `about.source` | 声音从哪来 | 导入的是 `file`；现场录音 0.8 没记麦克风还是电脑声音，写空，不猜 |
| `takes[0]` | 第 0 次转写，现场草稿 | `list_visible_notebook_utterances`：阅读页上的现场字幕，订正已叠上。只含空白的行不算 |
| `takes[1]` | 第 1 次转写，整场转写；只有 0.8 写过并且至少一行时才有 | 笔记本 `async_transcript` 标签页的 Loro 文档，按阅读页的切法（`NotebookTranscriptProjectionStore.parse`）取这一条录音的段：同一个 `segment_id` 相邻的片段是一行，去掉开头渲染的 `[mm:ss]` |
| 行的 `t0`/`t1` | 采集时钟上的毫秒 | 现场：`source_start_ms`/`source_end_ms`；整场：`timestamp_ms`（或 `start_ms`）/`end_ms` |
| 行的 `speaker` | 说话人标签，进记录的键，只能是字母、数字、`-`、`_` | 服务给的标签；同一条录音重连后又出现的标签加 `-<epoch>`。整场转写用异步服务自己的标签，和新版一样与现场草稿共用一个标签空间 |
| 行的 `translations` | 各语言译文 | 现场：状态为 Ready 的译文变体；没有变体时用旧的 `translated_*` 两列。整场转写在阅读页上每段只是一段文字，原样交接，译文为空 |
| 行的 `incomplete` | 有没定稿的词 | `completion != complete` |
| 行的 `via` | 每一块文字出自谁：模型名，或 `hand` | 现场：订正过的（`source_edit_revision`、变体的 `edit_revision` 大于 0）是 `hand`，其余是 `realtime_model_id`；整场：带 `content_owner = user` 的段是 `hand`，其余是 `post_stop_model_id` |
| `speakers` | `talk/speaker`：标签到名字 | `session_speakers.local_display_name`，没有就用关联的 `participants.display_name` |
| `note` | `talk/note` | 录音笔记的大纲行，按层级缩进两个空格；再空一行，接上每个有笔记的标记：`[mm:ss] 笔记`。新版不再有标记，笔记的字不丢 |
| `audio` | 重新封进 tape | 见下；这台 Mac 上已经没有这条录音的音频（销毁了、按保留期删了、在别的 Mac 上录的、钥匙不在了）就是 `null` |
| `audio.key` | 去 `Secrets/content-keys.json` 找钥匙的名字 | `notebook_capture_runs.audio_key_ref` |
| `audio.rate` / `channels` / `format` | 怎么解明文 | 同表三列；`format` 是 `s16` 或 `f32` |
| `audio.chunks` | 按顺序；两块之间缺的时间是静音，文字仍对得上声音 | `audio_retention_chunks` 里没销毁的块：`local_path` 相对数据目录的路径，`start_ms` |

不交接的东西：回收站里的录音；笔记本（主题）本身和它的名字；不属于任何录音的笔记本笔记；Context Pack（术语表）；分享链接与房间；同步与附近；交到城的记录；城的账户；设置。

## 4. 0.8.6 什么时候写，怎么挡住越过它的更新

- **启动时**在后台写一次整份交接（`LibraryHandover.writeInBackground()`），**Sparkle 安装更新之前**再同步写一次（`updater(_:willInstallUpdate:)`），这样启动以后录的、改的也都在。
- 每个文件先写到旁边的 `.new`，`fsync`，再改名替换；`index.json` 最后写。读的人看到的不是旧的就是新的，不会是半份。
- 只读不改：交接不动数据库、Loro 文档、音频和钥匙。
- **新版放在 appcast 的一个频道上**：新版那一项带 `<sparkle:channel>zu</sparkle:channel>`。0.8.6 的 `allowedChannels(for:)` 只在 `handover/index.json` 存在时返回 `["zu"]`。0.8.5 及更早不认这个频道，只会更新到 0.8.6；0.8.6 交接完了才看得到新版。所以经由自动更新，没有人会越过交接。
- 新版的构建号要大于 0.8.6 的（0.8.5 是 65，0.8.6 用 66，新版至少 67）。

## 5. 新版怎么收（`core/src/record/migrate.rs`）

- 外壳只在 macOS 上把旧数据目录交给核心（`Config.old`，社区邀请也从这里读）。有身份登录后，枢纽在后台跑一次；之后每次登录都会再看一眼，已经搬过的不再搬。
- 每条录音在新版的 id 是 `{started:011x}` 加旧 id 的 SHA-256 前四个字节：和本机造的 id 一个形状，而且每次都一样。所以「搬过没有」就是本人的书架上有没有这个 id 的 `talk/recording` 记录，删掉了也算搬过；不另记账。
- 一条录音整条搬或者不搬：先把音频重新封进 `tape/<id>`（先清掉上次没搬完留下的），再写各次转写的行、说话人、笔记，最后写 `talk/recording`。中途断了，下次从头再来；写到最后一条记录之前，界面上看不到它。
- 音频：按交接给的钥匙名从 `Secrets/content-keys.json` 取钥匙，逐条记录解开，按采样格式（一张表）取样、混成单声道，用和导入文件同一个重采样器换成 16 kHz，用采集时钟切成 100 ms 的块，用本机由 `tape` 秘密派生的钥匙重新封好。明文只在内存里。旧文件只读，不改不删。
- 交接里的录音不会被自动送去写整场转写：`About.moved` 为真的草稿不算欠着的。0.8 里整场转写要本人点了才写；新版不替本人把旧录音的声音送出去。
- 只搬给一个人：第一次搬的时候在数据目录写 `moved.json`（`{"to": "<DID>"}`），之后别的身份登录，什么也不搬。
- 进度在 `State.moving`（和 `State.writing` 同一个形状：哪一条、第几条、共几条）；没搬过来的在 `State.unmoved`：它在 0.8 的标题和原因代码。下次启动会再试。

| 代码 | 意思 |
|---|---|
| `migrate.missing` | 有 0.8 的数据库，却没有交接（手动装了新版，跳过了 0.8.6）。什么都没搬 |
| `migrate.handover` | 交接文件读不了、版本不对，或者内容放不进记录（比如说话人标签有不允许的字符） |
| `migrate.key` | 钥匙文件里没有这条录音的钥匙 |
| `migrate.audio` | 音频块不见了、被改过、被截断，或者采样格式、采样率不认识 |
| `tape`、`shelf` | 本机写不进去 |

## 6. 测试与夹具

`core/tests/fixtures/old/ZuTalk/` 是一份合成的 0.8 数据目录：音频块和钥匙文件是用旧仓库自己的代码写的（`vt_pipeline::CaptureAudioJournal` 按 20 ms 一推录了 1.5 秒锯齿波，`vt_crypto::encrypt_to_file` 写了半秒的第二块和一个 48 kHz 双声道 f32 的导入文件，`vt_crypto::FileKeyStore` 存的钥匙），交接 JSON 按本规格手写，文字是编的，没有任何人的内容。测试在 `core/tests/migrate.rs` 和 `core/tests/app.rs`：整条搬过来并由本人签名、音频逐样本对上（含中间的静音）、双声道混合与重采样、只搬一次（删了也不再搬）、断在半路重来、每种失败的代码、没交接的说法、只搬给一个人、搬过来的草稿不被送去转写。

## 7. 补丁

[migrate-0.8.patch](migrate-0.8.patch) 对 `9251050` 能干净打上（`git apply --check`），内容：

- `crates/vt-ffi/src/handover.rs`：`ZuTalkCore::write_handover()`，按 §3 写交接；
- `crates/vt-ffi/src/lib.rs`：登记这个模块；
- `macos/ZuTalk/ZuTalk/App/LibraryHandover.swift`：启动时后台写、安装前同步写、频道判断；
- `SoftwareUpdateController.swift`：`allowedChannels(for:)` 与 `updater(_:willInstallUpdate:)`；
- `ZuTalkApp.swift`：启动时调用。

这份补丁**没有编译过**：负责人要求这台机器上不随意启动构建，旧仓库的 `vt-ffi` 依赖六百多个 crate。发 0.8.6 之前要在旧仓库里跑 `cargo check -p vt-ffi`、重新生成 UniFFI 绑定、Xcode 构建，再用一份真数据目录的副本在 `CFFIXED_USER_HOME` 下跑一次，核对 `handover/` 的内容。
