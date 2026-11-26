状态：完成

# P4 身份与额度：收尾与总验收报告（P4-06）

## 改动与文件（P4-06）

1. **端到端隐私、事件与统计上限总测试（[core/tests/identity.rs](../../core/tests/identity.rs)）**：
   - 新增 `FileApp` 辅助壳（在 `Core::start(cfg)` 同步启动点进入 `zutalk_log::files` 作用域而不调用内存 `capture()`，使全部核心任务与后台线程的日志真实落盘至 `<data>/logs/zutalk.jsonl`）。
   - 新增 `identity_secrets_dids_and_handles_never_reach_logs_or_diagnose`，串联全套 P4 路径：
     1. 本机 `did:key` 离线录音 + 哨兵标题 `AdoptTitleSentinel44` + 笔记 `AdoptNoteSentinel55` + 词表 `AdoptTermSentinel66`；
     2. `CreateIdentity { handle: Some("handleprobe44") }`（经本地 `MockServer` 替身目录建号并触发首次登录领回）+ `CopyRotation`（经 `Config.clip` 捕获真实 `z…` 轮换密钥）+ `RotationSaved`；
     3. 本地 `zutalk-server` 授予额度 + `RefreshQuota`；
     4. `SignOut` + `RecoverIdentity`（先喂错误恢复密钥 `zWrongRecoveryProbe99` 验证拒回 `id.recovery_key`，再喂真实轮换密钥恢复登录）；
     5. `DiscardLocal` 清理本机备份；
     6. `Command::Diagnose` 原子导出自检包（含 `about.json`、`water.json`、`stats.json`、`tasks.json` 与 `logs/*.jsonl`）。
   - 逐字节校验 `logs/*.jsonl` 与 `diagnose/**/*` 全部文件：
     - 12 项黑名单哨兵（真实 `z…` 轮换密钥、`zWrongRecoveryProbe99`、本人 `did:zu:…`、本机 `did:key:…`、`did:zu:`、`did:key:`、`handleprobe44`、`AdoptTitleSentinel44`、`AdoptNoteSentinel55`、`AdoptTermSentinel66`、`sk-minted-soniox`、数据目录路径）零命中；
     - 最新 `State` JSON 序列化零包含轮换密钥或错误恢复密钥；
     - 每行日志为平铺 JSON、零嵌套、零 `"!"` 退回值、`zutalk_log::refused()` 零增长；
     - 全部 4 个 P4 新事件（`identity.auth`、`quota.updated`、`identity.adopt`、`identity.discard_local`）与 `diagnose.wrote` 均真实出现，`stats.full` 为 0，`stats.json` 中 `Tally` 压缩序列化 `≤ 8192 B`。
2. **规则门禁升级（[scripts/build/rules/pending.sh](../../scripts/build/rules/pending.sh)）**：
   - 将匹配模式由 `ignore = "P(1-|2|3)` 升级为 `ignore = "P(1-|2|3|4)`，关闭 P4 阶段后禁止残留 P4 挂起测试。
3. **契约与经验文档同步**：
   - 更新 [docs/PLAN.md](../../docs/PLAN.md)（§2.4、§2.7、§5.3、§7 I8/I9、§8 P4 行）；
   - 更新 [docs/DESIGN.md](../../docs/DESIGN.md)（§3 概念表、§4 模块表、新增 §5.17 身份/每日额度/本机领回）；
   - 更新 [docs/LESSONS.md](../../docs/LESSONS.md)（新增「P4 里学到的（2026-11）」5 条实操规律）。

## P4 出口逐条核对（`P4.md` §1 E1–E6）

| 出口 | 状态 | 对应实现与验证 |
|---|---|---|
| E1 服务器每日按服务额度与任务监督 | 通过 | `server/src/tasks.rs`、`server/src/ledger.rs`、`POST /quota`、`server/tests/quota.rs`（4 项）；`spawn.sh` 已清零 `server/src/ledger.rs 2 P4`（提交 `540b3a4`） |
| E2 核心额度与完整身份状态机 | 通过 | `zu/src/did.rs`、`core/src/id/{identity,quota}.rs`、`AuthFlow`/`Quota`/`handle`、`core/tests/identity.rs`；未登录零服务器请求（提交 `4799299`） |
| E3 本机 `did:key` 领回与显式清理 | 通过 | `core/src/record/adopt.rs`、`adopted.json` 单人绑定、进行中录音封盘补领、`DiscardLocal`、`core/tests/adopt.rs`（4 项，提交 `50d6952`） |
| E4 界面按需身份纸与设置页额度/领回区 | 通过 | `ui/src/bus/identity.ts`、`IdentitySheet.tsx`、`SettingsPage.tsx`、8 语字典同步、`ui/tests/identity_sheet.test.tsx`（6 项）与 `library_settings.test.tsx`（+4 项，提交 `16ada43`、`6605d94`） |
| E5 I8 / I9 / 隐私红线 | 通过 | `State` 5 条 vs 100 条为 `523 B / 525 B`（比值 `1.00`，`Invalidate = 60 B`）；P4 零新增 `Stats` 行、`stats.full == 0`、`Tally ≤ 8192 B`；4 个新事件登记进 `docs/EVENTS.md`（共 76 个 Rust 事件）；全链路落盘隐私测试到位 |
| E6 门禁全绿、零新增外部依赖、无 P4 挂起 | 通过 | `pending.sh` 升级禁 `P1–P4`；`ui/package-lock.json` 零改动、`Cargo.lock` 仅 `zutalk-server` 复用既有 `tokio-util`；沙箱外全量 `scripts/build/check.sh` `check: all green`（10/10 规则、Rust 732 通过 / 0 失败 / 6 忽略、UI 13 文件 97 通过） |

## 实测指标与门禁证据

### 沙盒内实测（已执行，全部退出码 0）

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`、`ui`）：**10/10 通过**。
- 依赖核对（`git diff c158d5d -- Cargo.lock ui/package-lock.json`）：**零新增外部 crate 与 npm 包**（仅 `zutalk-server` 新增一条指向既有 `tokio-util` 的边）。
- Rust 编译与非 TCP 测试：
  - `cargo test -p zutalk-core --test identity --no-run`：编译通过（含新测试 `identity_secrets_dids_and_handles_never_reach_logs_or_diagnose`）。
  - `core/tests/identity.rs`（非 TCP）：**5/5 通过**。
  - `core/tests/adopt.rs`（非 TCP）：**3/3 通过**。
  - `core/tests/app.rs`（核心身份/书架/命令守卫 5 项）：**5/5 通过**。
  - `core/tests/liveness.rs`（I8、事件表与两条隐私落盘测试 4 项）：**4/4 通过**；I8 实测输出：
    `i8: bytes_5=523 bytes_100=525 ratio=1.00 state_pushes=(0,0) inv_bytes=(60,60)`。
- UI 门禁（`cd ui && npm run check && npm test && npm run build`）：
  - `tsc` + `vite build`（3 入口）零错误；
  - Vitest：**13 个测试文件 / 97 项测试全部通过**（含 `words.test.ts` 8 语 373 键全核对与 `identity_sheet.test.tsx` 6 项、`library_settings.test.tsx` 10 项）。

### 沙盒外全量门禁（`RUSTC_WRAPPER="" scripts/build/check.sh`）

- `rustfmt` + `clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（`bounded`、`dropped`、`events`、`hub`、`layers`、`numbers`、`pending`、`shell`、`spawn`、`ui`）：**10/10 通过**。
- Rust 全量测试：**732 passed, 0 failed, 6 ignored**（含 `core/tests/identity.rs::identity_secrets_dids_and_handles_never_reach_logs_or_diagnose`、`qr_sign_in_waits_for_confirmation_before_keeping_identity`、`core/tests/adopt.rs` 封盘补领与 `server/tests/` 全部本机端口集成测试；6 条 `ignored` 全部为需外部 API/真实硬件的长跑项，零阶段挂起）。
- UI 全量测试与构建：`tsc` + `vite build` + Vitest **13 files / 97 passed**。
- 总结论：**`check: all green`**。

## 需负责人确认或动手的事（`P4.md` §4）

1. **六位数登录核对协议（`DECISIONS.md` #5）**：`AuthFlow::Confirm { did, handle, code: Option<String> }` 与 `IdentitySheet` 的 `Check` 步已留好 `code` 渲染与协议钩子；待 `zu` 规范 `login.md` 定下两端派生算法并与 `pol` 对齐后填入。
2. **短句柄默认后缀域名**：短标签新建身份暂拼 `DEFAULT_HANDLE_DOMAIN = "zuzalu.ink"`（单一常量），若城邦采用其他域名或独立句柄认领流程，仅改该常量或对接注册接口即可。
3. **每日额度默认上限**：服务器默认 `DEFAULT_DAILY_SONIOX_TIMES = 40`、`DEFAULT_DAILY_OPENROUTER_TIMES = 30`，生产部署时经 `ZUTALK_DAILY_SONIOX` / `ZUTALK_DAILY_OPENROUTER` 按真实成本配置。
4. **线上目录与生产服务器部署**：P4 自动化测试全部运行于本机替身目录与服务器端口；真实 `plc.zuzalu.ink` 建号/恢复与生产服务器联调需由负责人在真实环境验证。
