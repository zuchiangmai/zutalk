# P7-03 终版全仓门禁、`pending.sh` 锁死、文档收拢与项目收官

## 目标

1. 将 `scripts/build/rules/pending.sh` 升级为终版规则，彻底封禁所有阶段延期挂起测试（`ignore = "P[0-9]"`），并复核全部 10 个规则脚本零违规、零欠账。
2. 校验 `Cargo.lock`、`ui/package-lock.json` 完整性及零新增外部依赖，复核 `docs/EVENTS.md` 与源码事件 100% 双向匹配、`Stats` 行数与 `Tally ≤ 8192 B`、以及全仓 `RUSTC_WRAPPER="" scripts/build/check.sh` 一次全绿。
3. 将 P1–P7 的实测结果与最终状态收拢写入 `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`、`docs/EVENTS.md`，并产出最终收官报告 `docs/build/reports/P7-03-close.md`（含 P1–P7 实测记分牌与按序编排的负责人终版行动清单）。

## 背景

- 设计依据：`docs/build/phases/P7.md` §1（E5–E6）、§2、§3.4、§4；`docs/PLAN.md` §7、§8；`docs/build/DECISIONS.md`；`docs/build/reports/P5-06-close.md`（收官报告风格参考）。
- 涉及文件：
  - `scripts/build/rules/pending.sh`
  - `docs/PLAN.md`
  - `docs/DESIGN.md`
  - `docs/LESSONS.md`
  - `docs/EVENTS.md`
  - `docs/build/reports/P7-03-close.md`（新建终版收官报告）

## 做法

### 1. 终版锁死 `scripts/build/rules/pending.sh`

- 更新 `scripts/build/rules/pending.sh`：
  - 将注释更新为全项目收官口径（P1–P7 全部结束，任何 `ignore = "P[0-9]` 延期测试一律禁止，仅允许真实硬件与真实云端密钥手工测试）；
  - 将检查命令更新为：
    ```bash
    found=$(grep -rnE 'ignore = "P[0-9]' core app server zu ui || true)
    ```
  - 确认现有 6 条 `#[ignore]` 均为明确的真实硬件/真实云端密钥测试（`core/tests/audio.rs` 4 条、`core/tests/live.rs` 1 条、`core/tests/write.rs` 1 条），零阶段延期挂起项。

### 2. 全仓 10 个规则脚本、锁文件与关键指标终审

逐项运行并记录真实输出（写入 `docs/build/reports/P7-03-close.md`）：

1. **10 个规则脚本逐个核验**：
   - `bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh` 全部退出码 `0`；
   - 确认 `bounded.sh`、`spawn.sh`、`layers.sh` 的 `debt` / `later` 均为空（全仓 0 欠账）。
2. **锁文件与零新增外部依赖核验**：
   - 检查 `git diff 32c08a2 -- Cargo.lock` 为空（P7 零新增外部 Rust 依赖）；
   - 检查 `ui/package-lock.json` 相对 `32c08a2` 无任何新增外部 npm 包；
   - 核对全仓 5 个 Rust crate 与 `ui/package.json` 均为 `0.9.0`。
3. **关键不变量与规模实测复核**：
   - 跑 `RUSTC_WRAPPER="" scripts/build/check.sh`，记录 Rust 全部测试总数（`passed / failed / ignored`）与 UI Vitest 文件/测试总数及 3 入口产物构建结果；
   - 单独跑一次关键指标测试以记录精确数值：
     - `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library`（记录 `bytes_5`、`bytes_100`、`ratio=1.00`、`inv_bytes`）；
     - `core/tests/liveness.rs::every_event_in_the_catalogue_can_be_told_apart` 与事件总数（应为 `84` 个）；
     - `core/tests/liveness.rs::two_minute_recording_tally_matches_the_table`（确认 `stats.full == 0`、`Tally ≤ 8192 B`）；
     - `core/tests/migrate_scale.rs`（确认 175 条有效 + 6 条损坏 + 领回 + 主题迁移 + 二次/三次启动幂等 + 真实落盘隐私审计全部实测通过）；
     - `app/tests/updates.rs`（确认全部 7 条发布/更新/权限/隐私测试通过）。

### 3. 核心文档终版收拢（简体中文，诚实反映实测边界）

1. **`docs/PLAN.md`**：
   - §2.4 增补 P7 复测的 `State` 字节数与 `State.unmoved` 32 条封顶说明；
   - §5.3 增补「P7 实际变更：零新增外部依赖、零新增 `Stats` 行、全仓 10 规则零欠账 + `pending.sh` 封禁全部 `P[0-9]`」；
   - §7 不变量表补齐 I2（175 条合成迁移压测）、I3、I8、I9（84 个事件、`pending.sh` 终版锁死）的 P7 实测数据；
   - §8 路线图 P7 行标为 **已做（P7-01 至 P7-03）**，写清自动化出口 E1–E6 实测结果，并明确标注真机/发布实操移交负责人清单（指向 `docs/build/reports/P7-03-close.md` 与 `docs/RELEASE.md`）。
2. **`docs/DESIGN.md`**：
   - 更新 §5.12（补充 P7 路径校验、12 h 静音上限与分批封盘、`moved.json` 原子写盘、切换身份清空/隔离、`State.unmoved` 32 条上限、`migrate.done` 事件与 `core/tests/migrate_scale.rs` 175 条合成实测）；
   - 更新 §5.14（事件总数更新为 84 个，补充 `pending.sh` 禁全部 `P[0-9]`）；
   - 更新 §9 第 9、12 条（把 175 条合成全链路迁移区分于尚未在真实 175 条数据副本上跑 0.8.6 补丁的事实，绝不夸大）。
3. **`docs/LESSONS.md`**：
   - 新增「P7 里学到的（2026-11）」小节（3–4 条精炼真实经验：迁移输入按不受信任边界校验路径与静音空洞防 OOM、身份切换必须防后台 `job.move` 回写串台与 `State.unmoved` 无界膨胀、把版本/权限/CSP/防泄漏写进自动化发布测试而非靠人工记忆）。
4. **`docs/EVENTS.md`**：
   - 在「出了事先看什么」表末尾新增一行「**0.8 旧录音没搬过来或部分失败**」，给出先搜 `migrate.done`、`job.done(kind = "move")`、看 `moved`/`already`/`skipped`/`code` 与 `State.unmoved` 的排查路径。

### 4. 撰写项目终版收官报告 `docs/build/reports/P7-03-close.md`

报告结构清晰、平实、全部填写真实测得的数字：

1. `状态：完成` + `## 改了什么`；
2. `## P7 出口逐项核对（E1–E6）`；
3. `## P1–P7 全项目实测记分牌`（一张总表覆盖 P1 到 P7 的核心交付、自动化门禁结果、I1–I9 全量实测证据、规则脚本 10/10 零欠账、84 个事件、38/40 计数预算、零未清欠账）；
4. `## 终版门禁实测证据`（完整记录 `RUSTC_WRAPPER="" scripts/build/check.sh` 的 Rust / UI 测试数与各项实测值）；
5. `## 负责人终版行动清单（Owner Action Checklist）`：
   - 按推荐先后顺序编排可勾选 `[ ]` 清单，涵盖：
     1. 决策拍板与跨仓库协调（`DECISIONS.md` #1–#6、#10 与 `DESIGN.md` §7 待定项，含 `pol` 档案馆 v0.3 与 `zu` 规范）；
     2. 真实硬件与三平台手工验收（`core/tests/audio.rs` 7 项 + 8 小时真机实录 + WebView 与字幕窗实测）；
     3. 真实 API 钥匙与读书会端到端实测（Soniox / OpenRouter）；
     4. 生产服务器与边缘反代部署（`docs/RELEASE.md` §5 全部环境变量、旧服务 `/v1/retire`、边缘 `/r/*` -> `viewer.html`）；
     5. 更新签名钥匙对与三平台发布准备（`zutalk-update` minisign、`app/tauri.conf.json` `pubkey`、Windows `.ico`/NSIS、Linux AppImage、macOS 通用包/可选 Developer ID 公证）；
     6. 在 `/Users/kant/Zuzalu/zulangue` 执行 `0.8.6` 交接发布（构建 `66`，先用 175 条真数据副本验交接）与随后发布 `v0.9.0`（构建 `67`，带双项 `appcast.xml`）及真机 `0.8.5 → 0.8.6 → 0.9.0` 实测。

## 显式与可观测

- **规则脚本自证**：对升级后的 `scripts/build/rules/pending.sh` 验证其在当前仓库通过，且逻辑上能捕获任意 `ignore = "P0"` 至 `ignore = "P9"`；
- **事件词典闭环**：`scripts/build/rules/events.sh` 与 `core/tests/liveness.rs::every_event_in_the_catalogue_can_be_told_apart` 双重验证 84 个事件全部在 `docs/EVENTS.md` 登记、无重复、无未文档化、无废弃死项；
- **全仓零新增 `Stats` 行与零新增外部依赖**：在终版报告中以真实命令输出佐证 `stats.full == 0`、`Tally ≤ 8192 B` 与锁文件完整性。

## 先写的测试

本任务以门禁规则锁死与终版实测核验为主：
1. 先升级 `scripts/build/rules/pending.sh` 并验证 `bash scripts/build/rules/pending.sh` 立即通过（确认零 `P[0-9]` 挂起项）；
2. 用一条临时验证命令（不修改测试文件，仅在终端验证正则）确认 `grep -E 'ignore = "P[0-9]'` 能匹配 `#[ignore = "P7: ..."]` 与 `#[ignore = "P0: ..."]` 且放行 `#[ignore = "opens the microphone"]`；
3. 再同步文档并跑完终版 `RUSTC_WRAPPER="" scripts/build/check.sh`。

## 验收

- `scripts/build/rules/pending.sh` 使用 `ignore = "P[0-9]'` 禁止全部阶段延期测试；
- `RUSTC_WRAPPER="" scripts/build/check.sh` 一次全绿（rustfmt + clippy `-D warnings` + 全部 10 个规则脚本零欠账 + Rust 全量测试 0 failed + UI check/build/test 0 failed）；
- `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`、`docs/EVENTS.md`、`docs/build/reports/P7-03-close.md` 全部同步一致、简体中文平实无错漏、无任何真实人名/句柄/邮箱；
- `/Users/kant/Zuzalu/zulangue` 未受任何修改；未执行任何 `git push`、`git tag` 或发布操作。

## 不做

- 不修改任何业务源码逻辑（除非终版门禁暴露真实回归）；
- 不修改任何 `P6*` 评审或报告文件；
- 不触碰 `/Users/kant/Zuzalu/zulangue` 或仓库外任何目录；
- 不推送、不打标签、不发布、不伪造真机实测结果。
