# P4-06 隐私与不变量总验收、升级 `pending.sh` 与 P4 收尾

## 目标

做完以后：

1. **端到端隐私与可观测总测试到位**：
   - 在 `core/tests/identity.rs`（并同步加固 `core/tests/liveness.rs`）新增真实落盘隐私测试 `identity_secrets_dids_and_handles_never_reach_logs_or_diagnose`，串联运行 P4 全部路径：
     - 本机离线录音 + 笔记 + 词表；
     - `CreateIdentity`（带句柄）+ `CopyRotation` + `RotationSaved`；
     - 首次登录领回本机记录 + `RefreshQuota`；
     - `SignOut` 后以 `RecoverIdentity`（含故意输错一次 + 正确恢复密钥一次）重新登回；
     - `DiscardLocal`；
     - `Command::Diagnose` 生成自检包。
   - 逐字节检查真实落盘的 `logs/*.jsonl`、整个 `diagnose/` 目录以及 `State` 序列化结果，断言：
     - 轮换密钥（`z…` multikey）、恢复密钥输入串、原始签名种子绝不出现在日志、自检包或 `State`；
     - 本人 `did:zu:…`、本机 `did:key:…`、句柄、中继 URL 片段、录音标题/正文/笔记绝不出现在日志或自检包；
     - `zutalk_log::refused() == 0`；
     - `stats.full == 0` 且 `Tally` 序列化字节 ≤ 8192 B；
     - 全部 4 个 P4 新事件（`identity.auth`、`quota.updated`、`identity.adopt`、`identity.discard_local`）均在日志中真实出现且无 `events.sh` 漂移。
2. **规则门禁升级**：
   - 升级 `scripts/build/rules/pending.sh` 正则，将 `ignore = "P(1-|2|3)` 升级为 `ignore = "P(1-|2|3|4)`，确保 P4 关闭后零遗留 P4 挂起测试；
   - 核对全部 10 个规则脚本（含已清空 `server/src/ledger.rs 2 P4` 的 `spawn.sh`）全绿。
3. **文档与收尾报告同步**：
   - 更新 `docs/PLAN.md`（§2.4、§2.7、§5.3、§7 I8/I9、§8 P4 进展）、`docs/DESIGN.md`（补 P4 身份、额度与本机领回小节）、`docs/LESSONS.md`（追加「P4 里学到的」）；
   - 写出 `docs/build/reports/P4-06-close.md`，记录实测门禁数据（Rust/UI 测试数、10 个规则脚本结果、`State` 字节数与 I8 比值、`stats.full` 与 `Tally` 字节数、隐私扫描清单）与待负责人确认清单。

## 背景

- 设计：`docs/build/phases/P4.md` §1 E1–E6、§2、§4；`docs/PLAN.md` §4.5、§7、§8 P4。
- 涉及文件：
  - `core/tests/identity.rs`、`core/tests/liveness.rs`（必要时补断言）
  - `scripts/build/rules/pending.sh`
  - `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`
  - 新增 `docs/build/reports/P4-06-close.md`

## 做法

### 1. 全链路隐私、统计上限与 I8 审计测试

- 在 `core/tests/identity.rs` 新增 `identity_secrets_dids_and_handles_never_reach_logs_or_diagnose`：
  - 使用 `zutalk_log::files` 打开真实文件日志（`Level::Debug`），挂接本机 `MockServer`（模拟 PLC 目录）与本机 `zutalk-server`；
  - 依次跑完：离线录一场含哨兵标题 `AdoptTitleSentinel44` 与笔记 `AdoptNoteSentinel55` 的录音并加词表 → `CreateIdentity { handle: Some("handleprobe44".into()) }` → `CopyRotation`（经 `Config.clip` 捕获真实 `z…` 轮换密钥哨兵）→ `RotationSaved` → 等待领回完成 + `RefreshQuota` → `SignOut` → `RecoverIdentity`（先喂错误哨兵 `zWrongRecoveryProbe99`，再喂真实轮换密钥）→ `DiscardLocal` → `Diagnose`；
  - 落盘后读取 `logs/*.jsonl` 与 `diagnose/**/*` 全部文件，断言：
    1. 敏感黑名单（真实轮换密钥、`zWrongRecoveryProbe99`、本人 DID、本机 DID、`did:zu:`、`did:key:`、`handleprobe44`、`AdoptTitleSentinel44`、`AdoptNoteSentinel55`）在所有日志行和自检包文件中均**零出现**；
    2. 真实轮换密钥与错误恢复密钥哨兵绝不出现在最新 `State` 的 JSON 序列化字节中；
    3. 日志中出现 `identity.auth`、`quota.updated`、`identity.adopt`、`identity.discard_local`、`diagnose.wrote`，且 `stats.full` 出现次数为 `0`、`zutalk_log::refused()` 无增长；
    4. `Tally` JSON 字节数 `<= 8192`。
- 确认 `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 与 `every_event_in_the_catalogue_can_be_told_apart` 持续通过并记录实际 `State` 字节数与比值。

### 2. 升级 `scripts/build/rules/pending.sh`

- 将匹配模式由 `ignore = "P(1-|2|3)` 更新为 `ignore = "P(1-|2|3|4)`，更新注释与提示文案提及 `P1/P2/P3/P4`。

### 3. 更新文档与撰写收尾报告

- `docs/PLAN.md`：在 §2.4、§2.7、§5.3（注明 P4 零新增外部依赖，仅 `server/Cargo.toml` 复用锁文件既有 `tokio-util`）、§7（I8/I9 实测）、§8 P4 行写明 P4 已做事项与实测出口。
- `docs/DESIGN.md`：更新 §3 概念表、§4 模块表（补 `id/identity.rs`、`id/quota.rs`、`record/adopt.rs`、`server/src/tasks.rs`）并新增 §5.17（P4 身份、按服务每日额度与本机记录领回）。
- `docs/LESSONS.md`：追加「P4 里学到的（2026-11）」小节，只记本次 P4 真实踩坑或验证到的工程规律。
- 新建 `docs/build/reports/P4-06-close.md`：逐条核对 `P4.md` E1–E6 出口，附全量 `check.sh` 输出摘要、实测指标表与「待负责人确认/动手清单」。

## 显式与可观测

- 本任务直接用真实落盘字节扫描与统计/事件断言为 P4 的原则 6（隐私）和原则 10（显式与可观测）做总验收；
- 所有新增测试均为确定性本机端口测试，绝不依赖外网或脆弱真实睡眠等待。

## 先写的测试

- `core/tests/identity.rs::identity_secrets_dids_and_handles_never_reach_logs_or_diagnose`（覆盖日志、自检包、`State` 秘密隔离、`stats.full == 0`、`Tally <= 8192 B` 与 P4 全部 4 个事件）。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（全部 10 个规则脚本 + `rustfmt` + `clippy -D warnings` + 全部 Rust 测试 + `tsc` + `vite build` + 全部 UI 测试）；
- `git diff` 核对无任何未预期外部依赖加入 `Cargo.lock` / `ui/package-lock.json`；
- `docs/build/reports/P4-06-close.md` 齐备。

## 不做

- 不提前实现 P5（主持、直播房间删单条分享、字幕窗场合与舞台全屏、按住交档案馆）或 P6（知识库与 AI 标题）。
