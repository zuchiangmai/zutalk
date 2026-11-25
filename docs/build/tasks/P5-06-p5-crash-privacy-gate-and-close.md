# P5-06 崩溃双保险与隐私门禁验收、规则升级与 P5 收尾

## 目标

做完以后：

1. **端到端验证 P5 核心出口——「主持一场、崩溃一次、重开应用：外面不留任何东西」**：
   - 在真实 `zutalk-server` + 真实 `Core` 上跑通两条保险路径：
     - **保险一（客户端 `host.lock` 重启清理）**：主持开房、观众接 SSE 收密文字幕、`Session.viewers == 1` → 直接 drop `Core` 模拟崩溃（`host.lock` 留在磁盘、仅含 32 位房间 id、不含密钥）→ 用同一数据目录重启 `Core` → 自动向服务器发 `gone`、删除 `host.lock`、观众立即收到 `event: gone` 且流关闭、`GET /rooms/{id}/events` 返 `404`、日志可见 `host.recover(code = "ok")`；
     - **保险二（服务器 2 分钟无写入 TTL 清扫）**：主持开房、观众接 SSE → 直接 drop `Core` 且不重启客户端 → 推进服务器清扫 `server.sweep(now + ROOM_IDLE_TTL_MS + 1)` → 服务器清掉房间、观众立即收到 `event: gone` 且流关闭、`GET /rooms/{id}/events` 返 `404`、服务器磁盘零 `rooms/` 残留文件、日志可见 `server.sweep(removed = 1)`。
2. **P5 全链路真实落盘隐私字节审计**：
   - 在 `multi_thread` 运行时安装真实文件日志（`<data>/logs/zutalk.jsonl`）与服务器文件日志（`<server>/logs/zutalk-server.jsonl`），跑完主持直播 → 停止对外 → 交城档案馆 → 修改字幕窗场合与几何 → `Command::Diagnose` 全流程；
   - 逐字节扫描 `logs/zutalk.jsonl`、`logs/zutalk-server.jsonl`、`diagnose/`、`host.lock`（存活期间快照）与 `settings.json`，断言：
     - 房间密钥（原始/base64url）、链接 `#` 后段、对话原文与译文、活动标题、DID、句柄**绝不出现**在客户端日志、服务器日志、自检包、`host.lock` 或 `settings.json` 中；
     - 房间 id **绝不出现**在客户端日志、服务器日志、自检包或 `settings.json` 中；
     - `zutalk_log::refused() == 0`。
3. **I8 / I9 / `rt::stats` 与全仓规则零欠账核验**：
   - 确认 `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 在含 `HostState` 与 4 场合 `CaptionPrefs` 后，5 条 vs 100 条录音 `State` 字节数比值恒为 `1.00`；
   - 确认全流程 `stats.full == 0` 且 `Tally` JSON 体积 `≤ 8192 B`；
   - 确认 `bounded.sh`、`spawn.sh`、`layers.sh` 三份规则脚本均已**零欠账**；
   - 升级 `scripts/build/rules/pending.sh` 从 `P(1-|2|3|4)` 到 `P(1-|2|3|4|5)`，同禁 P1/P2/P3/P4/P5 挂起测试。
4. **文档与收尾报告同步**：
   - 更新 `docs/PLAN.md`（P5 行标记已做、同步 §2.1/§2.2/§2.3/§2.4/§5.3/§7 I8 与 I9 实测数据）；
   - 更新 `docs/DESIGN.md`（更新 §5.3 房间现状为无盘只留槽 + 主持两道保险，新增 §5.19 P5 主持与对外总结）；
   - 更新 `docs/LESSONS.md`（新增「P5 里学到的」条目）；
   - 核对 `docs/EVENTS.md` 与代码 100% 一致并补充「主持 / 字幕窗故障先看什么」排查行；
   - 撰写收尾报告 `docs/build/reports/P5-06-close.md`（逐项核对 P5.md 出口 E1–E6、记录实测数据、列出留给负责人的真机/协调清单）。

## 背景

- 设计：`docs/build/phases/P5.md` §1–§4；`docs/PLAN.md` §4.5、§5.5、§7、§8 P5。
- 涉及文件：
  - `core/tests/host.rs`（扩充双保险与真实落盘隐私字节审计）
  - `core/tests/liveness.rs`（核验 I8 与 `stats.full == 0`）
  - `scripts/build/rules/pending.sh`
  - `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`、`docs/EVENTS.md`
  - 新增 `docs/build/reports/P5-06-close.md`

## 做法

### 1. 端到端双保险与真实落盘隐私测试（`core/tests/host.rs`）

- 参照 P4-06 在 `core/tests/identity.rs` 中测试真实文件日志的经验（`docs/LESSONS.md` P4 第一条：仅在 `Core::start(cfg)` 同步进入 `scope.enter()`，绝不把 `Entered` 守卫跨 `.await` 持有）：
  1. `host_crash_recovery_double_insurance_leaves_nothing_outside`：
     - 完整测试保险一（`Core` 崩溃留 `host.lock` → 重启 `Core` 自动 `gone`、清 `host.lock`、观众收 `gone`、房间 404、`host.recover` 入日志）与保险二（`Core` 崩溃不重启 → `server.sweep(now + ROOM_IDLE_TTL_MS + 1)` 清房间、观众收 `gone`、房间 404、`server.sweep` 记 `removed = 1`、服务器磁盘零 `rooms/` 文件）。
  2. `host_city_and_caption_secrets_never_reach_logs_diagnose_lock_or_settings`：
     - 用真实 `zutalk_log::files` 跑完主持开房 + 发加密字幕 + `StopOutbound` + 交城档案馆 + 修改字幕窗场合与几何 + `Command::Diagnose`；
     - 读取 `logs/zutalk.jsonl`、`diagnose/` 全部文件、`settings.json` 及存活期 `host.lock` 快照，逐字节断言所有敏感探针（房间密钥、链接 `#` 后段、对话密词、译文密词、活动标题密词、本人 DID、句柄；以及日志/自检/设置内的房间 id）零命中；
     - 断言日志中按序出现 `host.state`、`host.room`、`diagnose.wrote`，`stats.full` 零命中，且 `zutalk_log::refused() == 0`；
     - 校验 `Tally` 序列化字节 `≤ 8192`。

### 2. 规则升级与文档收尾

- 更新 `scripts/build/rules/pending.sh`：
  - 正则改为 `ignore = "P(1-|2|3|4|5)'` 并更新注释与提示信息。
- 核对零外部依赖：`git diff 6e099ee -- Cargo.lock ui/package-lock.json` 确认无任何新增外部包。
- 核对三份欠账规则脚本零欠账：`bounded.sh`、`spawn.sh`、`layers.sh` 均无任何剩余欠账行。
- 同步更新 `docs/PLAN.md`、`docs/DESIGN.md`、`docs/LESSONS.md`、`docs/EVENTS.md`。
- 撰写 `docs/build/reports/P5-06-close.md`，记录：
  - 全部 6 项出口（E1–E6）实测证据（测试名、`State` 5 vs 100 字节数与比值、`Tally` 字节数、`stats.full == 0`、10 个规则脚本全绿结果）；
  - 零欠账达成状态（`bounded.sh` / `spawn.sh` / `layers.sh` 全部归零）；
  - 留给负责人的清单（档案馆 v0.3 类型确认、边缘反代与 TLS、三平台真机字幕窗验证）。

## 显式与可观测

- **故障可恢复且可见**：双保险测试同时断言外部残留清零与日志事件（`host.recover`、`server.sweep`、`host.state`、`host.room`）准确落盘。
- **隐私硬断言**：落盘字节审计覆盖客户端日志、自检包、`host.lock`、`settings.json`，且断言 `zutalk_log::refused() == 0`。
- **统计硬预算**：显式断言 `stats.full == 0` 与 `Tally ≤ 8192 B`；I8 比值 `1.00`。

## 先写的测试

1. `core/tests/host.rs` 新增：
   - `host_crash_recovery_double_insurance_leaves_nothing_outside`
   - `host_city_and_caption_secrets_never_reach_logs_diagnose_lock_or_settings`
2. 复跑 `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 与 `every_event_in_the_catalogue_can_be_told_apart` 并记录实测数值写入 `P5-06-close.md`。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含升级后的 `pending.sh` 与全部 10 个规则脚本、`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --workspace`、`ui` `check` & `test`）；
- `npm --prefix ui run build` 全绿；
- `docs/build/reports/P5-06-close.md` 完整记录所有出口实测值与负责人清单。

## 不做

- 不提前实现 P6 知识库（`talk/topic`）或 AI 标题/话题生成；
- 不推送、不打标签、不触碰仓库外文件。
