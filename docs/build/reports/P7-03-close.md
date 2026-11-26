状态：完成

## 改了什么

1. **终版锁死 `scripts/build/rules/pending.sh`**：
   - 将扫描规则由 `ignore = "P(1-|2|3|4|5|6)'` 升级为 `ignore = "P[0-9]'`，彻底禁止全仓任意阶段延期挂起测试（`P0`–`P9`）；
   - 经终端正则自证确认可命中 `#[ignore = "P7: later"]` 与 `#[ignore = "P0: early"]`、放行 `#[ignore = "opens the microphone"]`；全仓仅留 6 条明确的真机硬件/真云端钥匙手工测试（`core/tests/audio.rs` 4 条、`core/tests/live.rs` 1 条、`core/tests/write.rs` 1 条）。
2. **核心文档终版收拢同步**：
   - `docs/PLAN.md`：更新 §2.4（`State.unmoved` 32 条封顶与 I8 `1332 B / 1334 B` 比值 `1.00`）、§5.3（P7 零新增外部依赖与零新增 `Stats` 行）、§7（I2/I3/I8/I9 的 P7 终版实测数据）与 §8 路线图 P7 行标为 **已做（P7-01 至 P7-03）** 并明确标注真机/发布移交清单；
   - `docs/DESIGN.md`：更新 §5.12（路径校验、12 h 静音上限与分批封盘、空 `chunks` 拒绝、`moved.json` 原子写盘、切换身份隔离、`State.unmoved` 封顶、`migrate.done` 与 `migrate_scale.rs` 175 条合成压测）、§5.14（全仓 **84 个**事件与 `pending.sh` 禁全部 `P[0-9]`）与 §9 第 9/11/12 条（严格区分 175 条合成压测与真实 175 条旧库副本上的 0.8.6 补丁）；
   - `docs/LESSONS.md`：新增「P7 里学到的（2026-11）」4 条工程经验；
   - `docs/EVENTS.md`：在「出了事先看什么」表末尾新增「**0.8 旧录音没搬过来或部分失败**」排查路径。
3. **本报告收官交付**：完整汇总结算 P7 出口 E1–E6、P1–P7 全项目实测记分牌、终版门禁实测证据与按序编排的负责人终版行动清单。

---

## P7 出口逐项核对（E1–E6）

| 出口 | 状态 | 实测证据 |
|---|---|---|
| **E1** 0.8 迁移边界加固与 175 条合成压测通过 | 通过 | 提交 `d2a51a3`（P7-01）：`core/src/record/migrate.rs` 与 `core/src/app/{work,hub}.rs` 落地路径校验（`safe_recording_name`/`safe_chunk_path`）、12 h 上限（`MIGRATE_MAX_AUDIO_MS`）与 10 s 分批 `O(1)` 静音流式封盘（`MIGRATE_SILENCE_CHUNK_SAMPLES = 160_000`）、空 `chunks` 拒收、`moved.json` 原子写盘、`SyncKind::Migrated { by }` 与 `Msg::Moved(by, res)` 身份隔离及 `State.unmoved` 32 条封顶（`STATE_UNMOVED_MAX_ROWS = 32`）；`core/tests/migrate.rs`（8/8，1.55 s）与 `core/tests/migrate_scale.rs`（2/2，7.86 s）跑通 175 条有效 + 6 条损坏 + 2 条本机 `did:key` 领回 + 2 条旧 `talk/term`→`always` + 旧 `settings.json` + 全查询 + 3 轮启动幂等防复活 + 登出防串台 |
| **E2** I8、I9 与 `Stats` 预算在大规模迁移后仍全部成立 | 通过 | `i8_what_one_change_sends_does_not_grow_with_the_library` 复测 `bytes_5=1332`、`bytes_100=1334`、**`ratio=1.00`**、`inv_bytes=(60,60)`；175 条 3 轮启动压测后 `stats.full == 0`、`Tally ≤ 8192 B`、`zutalk_log::refused()` 零增量、`logs/*.jsonl` / `diagnose/**/*` / `settings.json` 逐字节扫描零泄漏；`docs/EVENTS.md` 登记 `migrate.done`（共 **84 个**事件），`events.sh` 与 `every_event_in_the_catalogue_can_be_told_apart` 双重通过 |
| **E3** 发布配置、版本对齐、权限最小化与防泄漏自动化审计到位 | 通过 | 提交 `55d07e7`（P7-02）：`ui/package.json`（及 `ui/package-lock.json` 顶层自身字段）补齐 `"version": "0.9.0"`；`app/tests/updates.rs` 新增 3 条审计测试（`release_versions_and_manifests_are_aligned` 红灯先验后转绿、`shell_capabilities_and_csp_keep_updater_and_clipboard_off_the_page`、`runtime_sources_contain_no_placeholder_secrets_or_personal_emails`），7/7 全部通过（0.23 s） |
| **E4** 0.8.6 交接与 v0.9.0 发布操作手册折叠收拢进 `docs/RELEASE.md` | 通过 | `docs/RELEASE.md` 升级为完整 §1–§7（v0.9.0 4 处版本检查点、`check.sh` 门禁、三平台构建与 `.ico`/`.icns`、生产 `ZUTALK_SERVER`、`zulangue` 0.8.5 构建 65 → 0.8.6 构建 66 逐条命令与双项签名 `appcast.xml` `zu` 频道跳跃、`server/src/main.rs` 逐行核对的全部 **16 项**环境变量表、边缘 `/r/*` 重写、5 组真机与 8 小时手工清单）；`docs/migrate.md` 末尾补齐交叉引用；仓库内零平行发布文档，未触碰 `/Users/kant/Zuzalu/zulangue` |
| **E5** `pending.sh` 终版锁死，10 个规则脚本全绿零欠账，零新增外部依赖 | 通过 | `scripts/build/rules/pending.sh` 升级为 `ignore = "P[0-9]'`；全部 10 个 `scripts/build/rules/*.sh` 全绿退出码 0，且 `bounded.sh`/`spawn.sh`/`layers.sh` 欠账表均为 0 行；相对 `P7.base`（`93e4997`），`Cargo.lock` 零变更、`ui/package-lock.json` 仅新增 2 行自身 `"version": "0.9.0"` 无任何外部包变动；沙箱外终版 `RUSTC_WRAPPER="" scripts/build/check.sh` **`check: all green`**（Rust **791 passed / 0 failed / 6 ignored**、UI **16 文件 / 124 passed**，含 P7-R1-01 新增路由/交接命令守卫测试） |
| **E6** 文档终版同步与项目收官报告完成 | 通过 | `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`、`docs/EVENTS.md`、`docs/RELEASE.md`、`docs/migrate.md` 全部同步一致；本报告 `docs/build/reports/P7-03-close.md` 给出完整 P1–P7 实测记分牌与按序编排的负责人行动清单 |

---

## P1–P7 全项目实测记分牌

| 阶段 | 核心交付 | 自动化出口与关键实测 |
|---|---|---|
| **P1 运行时地基**（P1-01 至 P1-26） | `zutalk-log` 独立日志 crate、具名 `Tree` 监督树、有界 `rt::queue`、寄存器 `Outlet` 绕开 hub、零 IO `hub.rs`、无锁全 `async` 外壳 `shell.pump`/`beat`/`updates`、1 Hz `water`/`judge` 看守、不登录本机 `did:key` 录音与无钥匙录音、一键自检包 `Command::Diagnose` | **I1** 8 虚拟小时双语每小时 p99 = 321 ms（≤ 1 s）、最大帧 6,125 B（≤ 8,192 B）；**I2** 卡死 tape/job 不挡字幕；**I4** `clippy.toml` + `bounded.sh` + `shell.sh`；**I5** 无锁 `Fan` 穷举交错；**I6** 杀 lane/tape/pump 自愈无丢事实；**I7** 静默/断网/超时必进 `Stalled`/`Dead` 并留 `stall.snapshot` |
| **P2 数据层与视图**（P2-01 至 P2-08） | `talk/note` 边录边记与锚定标记、`record/takes.rs` 多版读取与 `RefineStatus` 状态机、48 B `O(1)` `DraftWriter`、内置 `rusqlite` WAL + FTS5 `trigram` + 1–2 字归一化子串回退、`Query::Transcript`（64 行/块）与纯函数 `Compare`、L3 可取消 `ViewEngine`、`State` 瘦身与 64 槽合并 `Invalidations` | **I2** 1,000 条库直播并发 20 查询零 `Stalled`；**I3** `DraftWriter` 48 B 写 10,000 行、100 条损坏重建 94 ms、`Tally = 390 B ≤ 8,192 B`；**I8** 1,000 条查询 0.64–7.37 ms、10 小时分块/对比 5.44–12.76 ms（全 ≤ 100 ms）；`layers.sh` P2 欠账清零 |
| **P3 界面主流程**（P3-01 至 P3-07） | React 19 + TypeScript + `virtua` dsh 架构（`useSyncExternalStore` 与 Tauri IPC 锁死在 `ui/src/bus/`）、rAF 信号合并、128 项活跃订阅 `QueryCache`、4seas/ZuTalk 设计系统与离线字体、两栏外壳/`⌘K`/`HealthOverlay`、全部 6 个主流程页、零查询 `CaptionsWindow`、React `ViewerPage`、8 语字典、`ui.sh` 规则脚本 | **I8 界面侧** 600 px 视口下 1 分钟（16 行）与 4 小时（2,560 行 = 40 块）阅读页均恒挂 11 DOM 行（仅按需拉 2 块）、15 vs 500 条资料库均恒挂 11 DOM 行、同帧突灌 50 帧 + 50 电平合并为 1 次 React 提交 |
| **P4 身份、额度与领回**（P4-01 至 P4-06） | 服务器 `Tasks` 监督与按服务 UTC 日额度（`soniox` 40 / `openrouter` 30）+ `POST /quota`、`zu::did` 建号/恢复原语、`AuthFlow` 两段扫码确认与离线句柄缓存、`record/adopt.rs` 本机 `did:key` 全量领回 + `adopted.json` 单人绑定 + 进行中录音封盘补领 + `Command::DiscardLocal`、无登录墙 `IdentitySheet` 与 `SettingsPage` 额度/备份管理 | 零新增外部依赖；`spawn.sh` P4 欠账清零；`State` 523 B / 525 B（比值 `1.00`）；真实落盘日志/自检包零秘密/DID/句柄泄漏 |
| **P5 主持、字幕窗与对外**（P5-01 至 P5-06） | 服务器无盘只留槽 `watch` 直播房间（2 分钟 TTL + `server.sweep` 取消）、核心删 `out/room.rs` 与 `talk/share` 改用纯内存持钥 `out/host.rs` + `host.lock` 两道保险、`out/city.rs` 集中 `ARCHIVE_TYPES` 表、桌面外壳无锁 4 场合 `CaptionWindowTracker` + 防抖几何 + `ExitRequested` 异步 `gone`、`HostPage`/`FileArchiveCard`（1.4 s 按住 + 8 s 撤销）/`OutboundPage`/`CaptionScenesPanel`/4 场合 `CaptionsWindow`/只留槽 `ViewerPage` | 零新增外部依赖；**全仓 `bounded.sh` / `spawn.sh` / `layers.sh` 欠账彻底归零**；`State` 1,320 B / 1,322 B（比值 `1.00`）；崩溃双保险与真实落盘隐私审计通过 |
| **P6 知识库与 AI 标题**（P6-01 至 P6-05） | `talk/topic` 与固定 `always` 主题、旧 `talk/term` 无损迁移、零新 crate 有界 UTF-8 文件提取（≤ 512 KiB）、`TopicContext` 脱敏 `Debug` + 现场/精修双路上下文注入与未选主题隔离、`ai/titles.rs` 独立信号量（`TITLE_AT_ONCE_JOBS = 1`）+ 第 4 槽 `Progress.title` + 手写标题永不覆写 + 额度键 403 草稿兜底起标题、SQLite v2 `SearchHitKind::Tag` / `LibraryFilter.tag`、`KnowledgePage` 与全链路界面 | 零新增外部依赖、零新增 `Stats` 行；`State` 1,332 B / 1,334 B（比值 `1.00`）；4 小时 `O(1)` 采样 ≤ 6,000 字符；读书会 E2E 与真实落盘隐私审计通过 |
| **P7 搬家与发布**（P7-01 至 P7-03） | 0.8 迁移路径/12 h 静音分批/空 `chunks`/`moved.json` 原子写/身份隔离/`State.unmoved` 32 条封顶/`migrate.done`、175 条多形态合成压测与真实落盘隐私审计、全仓 `0.9.0` 版本对齐、发布/权限/CSP/源码防泄漏自动化测试、收拢进 `docs/RELEASE.md` 的 0.8.6 交接与服务器 16 项环境变量手册、`pending.sh` 终版封禁全部 `P[0-9]`、核心文档终版收口 | 零新增外部依赖、零新增 `Stats` 行（38/40 计数，`stats.full == 0`、`Tally ≤ 8,192 B`）；**10/10 规则脚本零欠账**；**84 个**事件双向匹配；`State` 1,332 B / 1,334 B（**比值 `1.00`**）；`migrate.rs` 8/8、`migrate_scale.rs` 2/2、`updates.rs` 7/7、UI 16 文件 / 124 测试全绿 |

---

## 终版门禁实测证据

### 沙盒内实测

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warning）。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：**10/10 全绿，全仓零欠账**；
  - `pending.sh` 正则自证：`grep -E 'ignore = "P[0-9]'` 命中 `#[ignore = "P7: later"]` 与 `#[ignore = "P0: early"]`，放行 `#[ignore = "opens the microphone"]`；全仓仅留 6 条真实硬件/真云端钥匙 `#[ignore]` 测试。
  - `docs/EVENTS.md` 事件总数：**84 个**（`events.sh` 双向匹配通过）。
- 依赖与锁文件核验（相对 `P7.base` `93e4997`）：
  - `Cargo.lock`：**零变更**；
  - `ui/package-lock.json`：仅顶层与 `packages[""]` 新增 `"version": "0.9.0"`（2 行），零外部包变更；
  - 全仓 5 个 Rust crate 与 `ui/package.json` 版本统一为 `0.9.0`；`/Users/kant/Zuzalu/zulangue` 未受任何写操作。
- Rust 沙箱内编译与关键实测：
  - `RUSTC_WRAPPER="" cargo test --workspace --no-run`：全仓全部测试目标编译通过（40.20 s）；
  - `core/tests/migrate.rs`：**8 passed, 0 failed**（1.55 s）；
  - `core/tests/migrate_scale.rs`：**2 passed, 0 failed**（7.86 s，含 175 条有效 + 6 条损坏 + 2 本机领回 + 2 旧词表迁移 + 3 轮启动 + 真实落盘逐字节隐私审计 + 登出防串台）；
  - `app/tests/updates.rs`：**7 passed, 0 failed**（0.23 s）；
  - `core/tests/liveness.rs`（3 项关键指标，3.77 s）：
    - `every_event_in_the_catalogue_can_be_told_apart`：通过；
    - `i8_what_one_change_sends_does_not_grow_with_the_library`：`i8: bytes_5=1332 bytes_100=1334 ratio=1.00 state_pushes=(0,0) inv_bytes=(60,60)`；
    - `two_minute_recording_tally_matches_the_table`：通过（`counts = 30`、`peaks = 2`、`spreads = 10`、`queues = 6`，全仓注册 38/40 计数零新增，`stats.full == 0`、`Tally ≤ 8,192 B`）。
- 前端校验与测试（`ui/`）：
  - `npm run check && npm run build`：通过（`zutalk-ui@0.9.0`，输出 `ui/dist/{index,captions,viewer}.html` 三入口）；
  - `npm test`：Vitest **16/16 文件、124/124 测试通过**（2.04 s，含 `words.test.ts` 12/12 与 `scale_i8.test.tsx` 4/4）。

### 沙盒外全量门禁（`RUSTC_WRAPPER="" scripts/build/check.sh`）

- `rustfmt` + `clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：**10/10 全绿，全仓零欠账**。
- Rust 全量测试：**791 passed, 0 failed, 6 ignored**（含 `app/tests/updates.rs` 8/8；全部 6 条 `ignored` 均为真机硬件/真云端钥匙手工测试，零阶段延期挂起项）。
- UI 全量测试与构建：`tsc` + `vite build`（3 入口）+ Vitest **16 files / 124 passed**。
- 总结论：**`check: all green`**。

---

## 负责人终版行动清单（Owner Action Checklist）

以下 6 组事项依赖负责人的产品/规范拍板、真实硬件、真实 API 密钥、线上服务器权限或签名私钥，均未由代理代做，建议按先后顺序逐项推进：

### 1. 决策拍板与跨仓库协调（`docs/build/DECISIONS.md` 与 `docs/DESIGN.md` §7）

- [ ] 拍板 `DECISIONS.md` 临时决定：#1（React 19 dsh 架构）、#2（锚定笔记作标记）、#3（`wall_clock_policy` 300 ms / 60 s）、#4（仅用社区额度者的精修策略：保持现场草稿 + `keys.async_refused` 提示，或推进 Soniox 异步临时钥匙 / 城邦转写服务）。
- [ ] 推进跨仓库规范对齐：`DECISIONS.md` #5（`zu` `login.md` 六位数核对派生算法，与 `pol` 对齐后回填 `AuthFlow::Confirm.code`）、#6（向 `pol` 确认档案馆 v0.3 读 `talk/*` 还是 `tape/*`，改动仅在 `core/src/out/city.rs::ARCHIVE_TYPES` 一张表）、#10（`zu` 规范是否正式纳入 `did:key` 本机作者，或改用其他本机签法）。
- [ ] 确认 `DESIGN.md` §7 待定项：轮换密钥展示/复制交互、默认音频保留天数（`Setup.keep`）、0.8 迁移完成后是否提示用户清理旧目录以释放重复磁盘空间、以及未来是否引入 PDF/DOCX 讲义解析 crate。

### 2. 三平台真机硬件与 8 小时实录验收（见 `docs/RELEASE.md` §6）

- [ ] 在 macOS、Windows、Linux 真机逐项跑完 `core/tests/audio.rs` 文件头的 7 项硬件手工清单并执行 `cargo test -p zutalk-core --test audio -- --ignored`。
- [ ] 完成至少一次连续 **8 小时真机实录**，核查内存平稳、实时字幕不漂移、日志轮换（8 MB × 5）正常、停录封盘与搜索/回放/导出流畅，导出的自检包 `stats.json` 紧凑大小 `≤ 8,192 B` 且 `stats.full == 0`。
- [ ] 在 macOS / Windows / Linux 多显示器环境实测 4 个字幕场合（自看 / 字幕条 / 舞台全屏 / 面对面）的置顶、穿透点击、透明合成、拖拽位置记忆及 WebView 视觉走查（亮/暗主题、窄窗、中/英/泰/日/韩字体与 `viewer.html` 手机观众页）。

### 3. 真实 API 钥匙与读书会端到端实测

- [ ] 配好真实 Soniox 与 OpenRouter 钥匙，运行 `SONIOX_TEST_KEY=... cargo test -p zutalk-core --test live -- --ignored` 与 `write -- --ignored`。
- [ ] 用真实钥匙录制一场带主题包（专有名词 + 背景）的真实读书会，验收现场字幕识别率、整场精修、自动 AI 标题/话题标签、仅用社区额度触发 `keys.async_refused` 降级提示及城邦交档流程。

### 4. 生产服务器与边缘反代部署（见 `docs/RELEASE.md` §5）

- [ ] 为 `zutalk-server` 配齐 `docs/RELEASE.md` §5.1 全部 **16 项**环境变量（含 `ZUTALK_ORIGIN`、`ZUTALK_COORDINATORS`、`ZUTALK_DAILY_SONIOX`/`OPENROUTER`、机密项 `SONIOX_API_KEY`、`OPENROUTER_MANAGEMENT_KEY`、`INVITES_TOKEN`）并上线。
- [ ] 在 0.8 旧邀请服务（`INVITES_URL`）部署 `POST /v1/retire` 并对齐 `INVITES_TOKEN == ZULANGUE_ADMIN_TOKEN`。
- [ ] 配置边缘反代与 TLS：静态托管 `ui/dist/`、将 `/r/*` 内部重写至 `viewer.html`（保留浏览器 `#<key>`）、为 `/rooms/*/events` 关闭响应缓冲并放宽 SSE 超时、配置基础速率限制。

### 5. 更新签名钥匙对与三平台发布构建准备（见 `docs/RELEASE.md` §2–§3）

- [ ] 在本机 Mac 生成 Tauri minisign 钥匙对，私钥存入登录钥匙圈 `zutalk-update`（并在密码管理器留备份），将公钥填入 `app/tauri.conf.json` 的 `plugins.updater.pubkey` 并提交。
- [ ] 若同步发布 Windows / Linux：在 `app/` 运行 `cargo tauri icon icons/icon.png` 补齐 `icons/icon.ico` 与 `icons/icon.icns` 并加入 `app/tauri.conf.json` 的 `bundle.icon`。
- [ ] 传入生产 `ZUTALK_SERVER="https://<正式域名>"` 构建 macOS 通用包（以及可选 Windows NSIS / Linux AppImage，产物回 Mac 签名），跑 `python3 scripts/updates.py <目录>` 生成并校验 `latest.json`（可选：切到 Apple Developer ID 签名公证；升级 `tauri-cli` 后开启 `requireSignedVersion`）。

### 6. 在 `/Users/kant/Zuzalu/zulangue` 发 0.8.6 交接版并正式发布 v0.9.0（见 `docs/RELEASE.md` §4）

- [ ] 在 `/Users/kant/Zuzalu/zulangue` 按 `docs/RELEASE.md` §4.1 应用 `/Users/kant/Zuzalu/zutalk/docs/migrate-0.8.patch`、`just bump 0.8.6`（构建 `66`）、`cargo check -p vt-ffi`、`just dev` 重新生成 UniFFI 绑定并执行 `just build-local-app` 产出 `build/app/ZuTalk.app`。
- [ ] **必须先用真实 175 条录音目录的副本**在隔离 `CFFIXED_USER_HOME` 下启动 `build/app/ZuTalk.app` 跑一次 `0.8.6`，核对 `handover/index.json`、`handover/recordings/*.json` 与音频块完整且原库未被修改。
- [ ] 跑 `just ci-check`，并显式传入 `GITHUB_REPOSITORY`（与 `git remote get-url github` 一致）和 `GITHUB_REF_NAME=v0.8.6` 执行 `just release-sparkle-adhoc`（或 `just release-ship`）发布 `v0.8.6`（构建 `66`），保留其 `build/dmg/appcast.xml`；再按 `docs/RELEASE.md` §4.2 用 `generate_appcast --account Zulangue --channel zu --maximum-versions 2` 合成含 `0.8.6`（构建 `66`）与 `v0.9.0`（构建 `67`，`zu` 频道）的双项签名 `appcast.xml`。
- [ ] 发布 `v0.9.0` GitHub Release（附全部更新产物、`.sig`、`latest.json` 与双项 `appcast.xml`），并在装有 `0.8.5` 的真机 Mac 上完整实测 `0.8.5 → 0.8.6 → 0.9.0` Sparkle 跳跃与 Tauri 应用内自更新。
