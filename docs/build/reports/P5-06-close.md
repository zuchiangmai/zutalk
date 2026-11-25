状态：完成

## 改了什么

1. **端到端双保险崩溃恢复与真实落盘隐私字节审计（`core/tests/host.rs`、`core/tests/common/mod.rs`）**：
   - `core/tests/common/mod.rs`：新增 `serve_handle()` 返回 `(origin, Arc<Server>, TempDir)` 以供测试显式调用 `Server::sweep` 与检查服务器磁盘目录。
   - `core/tests/host.rs` 新增 2 条全链路验收测试（共 9 条测试）：
     1. `host_crash_recovery_double_insurance_leaves_nothing_outside`：
        - **保险一（客户端 `host.lock` 重启清理）**：主持开房、观众接 SSE 收密文字幕、`Session { outbound: true, viewers: 1 }` → 直接 drop `Core` 模拟崩溃（验证磁盘 `host.lock` 仅含 32 位房间 id、不含密钥）→ 同目录重启 `Core` → 观众立即收到 `event: gone` 且流关闭、`host.lock` 清除、`GET /rooms/{id}/events` 返回 `404`、日志可见 `host.recover(code = "ok")`；
        - **保险二（服务器 2 分钟无写入 TTL 清扫）**：主持开房、观众接 SSE → 直接 drop `Core` 且不重启客户端 → 调用 `srv.sweep(now + ROOM_IDLE_TTL_MS + 1)` → 返回 `removed == 1`、日志可见 `server.sweep(removed = 1)`、观众收到 `gone` 且流关闭、房间返回 `404`、服务器磁盘零 `rooms/` 目录。
     2. `host_city_and_caption_secrets_never_reach_logs_diagnose_lock_or_settings`：
        - 在 `multi_thread` 运行时安装真实文件日志 `zutalk_log::files`（仅在同步 `Core::start` / `core.send` 时短时进入 `scope.enter()`，绝不跨 `.await` 持有 `Entered`），跑通 4 场合 `CaptionPrefs` + `CaptionGeometry` + `Command::City` + `Command::HostArm` + 实时加密字幕（原文/译文密词） + `Command::StopOutbound` + `Command::Stop`（等待 `tape::kept` + 200 ms 确保不空录） + `Command::File` + `Command::Diagnose`；
        - 逐字节扫描 `logs/*.jsonl`、`diagnose/**/*`、存活期 `host.lock` 快照与 `settings.json`，断言房间密钥（base64url / base64）、`#fragment`、原文/译文密词、活动标题密词、DID、句柄与 API 密钥零泄漏，房间 id 在日志/自检/设置内零出现，服务器磁盘无 `rooms/` 文件；
        - 断言日志含 `host.state`、`host.room`、`diagnose.wrote`，`stats.full` 零命中，`Tally` 紧凑 JSON `≤ 8192 B`，`zutalk_log::refused()` 零增量。
2. **规则升级与全仓文档收尾**：
   - `scripts/build/rules/pending.sh`：升级为 `ignore = "P(1-|2|3|4|5)'`，同禁 P1/P2/P3/P4/P5 挂起测试；
   - 更新 `docs/PLAN.md`（P5 行标记已做，同步 §2.1/§2.2/§2.3/§2.4/§5.3/§7 I8 与 I9 实测值）；
   - 更新 `docs/DESIGN.md`（更新模块表、`§5.3` 无盘只留槽与两道保险，新增 `§5.18 主持、字幕窗场合与对外（P5）`）；
   - 更新 `docs/LESSONS.md`（新增「P5 里学到的」5 条经验）；
   - 更新 `docs/EVENTS.md`（补充「主持对外异常或崩溃残房」与「字幕窗不显示、位置错乱或穿透点不到」排查行）。

## P5 出口逐项核对（E1–E6）

| 出口 | 状态 | 实测证据 |
|---|---|---|
| **E1** 服务器无盘只留槽直播房间、清零 `bounded.sh` 与 `spawn.sh` | 通过 | `server/src/room.rs` 仅留 `make`/`slot`/`gone`（拒 `log`）、纯内存 `tokio::sync::watch<SlotState>`、`ROOM_IDLE_TTL_MS = 120_000`；`server.sweep` 迁入 `server.tasks` 并配 `CancellationToken`；`bounded.sh` 与 `spawn.sh` 的 `debt`/`later` 表均已完全清空 |
| **E2** 核心 `out/host.rs` 两道保险、`out/city.rs` 归档表、清零 `layers.sh` | 通过 | 删除 `out/room.rs`、`talk/share`、`State.rooms`、`Command::Share`/`Unshare`；`out/host.rs` 实现 `HostState`/`host_step`、内存 `Zeroizing` 密钥、`host.lock`（只存房间 id）、`StopOutbound` 与 7 条 `gone` 路径；`out/city.rs` 集中 `ARCHIVE_TYPES` 表（`DECISIONS.md` #6）；`layers.sh` 的 `debt` 表已完全清空 |
| **E3** 桌面字幕窗按场合持久化与无锁外壳管理 | 通过 | `Setup.caption: CaptionPrefs`（固定 4 场合）与 `Setup.city` 落入 `settings.json`；`app/src/windows.rs` 纯状态机 `CaptionWindowTracker::diff` + 无锁 `watch` 400 ms 防抖几何回写（零阻塞窗口查询）；`ExitRequested` 异步等待 `gone`（零 `block_on`）；`shell.sh` 全绿 |
| **E4** 界面主持流、按住交城、对外页、字幕窗场合与手机观众页 | 通过 | `HostPage`、`LivePage` 色带与一键停止对外、`FileArchiveCard`（1.4 s 按住 + 8 s 可撤销倒数环后再发 `Command::File`）、`OutboundPage`、`CaptionScenesPanel`（16:10 预览）、4 场合零查询 `CaptionsWindow`（含 `Facing` 180° 与 10 s `auto_hide`）、只留槽手机 `ViewerPage`、`ui/dev/rooms.ts`；8 语全同步、`ui.sh` 全绿、三入口构建通过 |
| **E5** 双保险实测、I8 / I9 / 隐私红线 | 通过 | `core/tests/host.rs` 新增 `host_crash_recovery_double_insurance_leaves_nothing_outside` 与 `host_city_and_caption_secrets_never_reach_logs_diagnose_lock_or_settings`；`i8_what_one_change_sends_does_not_grow_with_the_library` 实测 `bytes_5 = 1320 B`、`bytes_100 = 1322 B`、**比值 `1.00`**、`inv_bytes = (60, 60)`；`two_minute_recording_tally_matches_the_table` 实测 `counts = 30`、`peaks = 2`、`spreads = 10`、`queues = 6`、`stats.full == 0`、`Tally ≤ 8192 B` |
| **E6** 门禁全绿、零新增外部依赖、无 P5 挂起测试 | 通过 | `pending.sh` 升级禁 P1/P2/P3/P4/P5；全部 10 个规则脚本零违规零欠账；`git diff de7aa64 -- Cargo.lock ui/package-lock.json` 输出为空（零新增外部 crate 与 npm 包） |

## 证据

### 沙箱内实测

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（零警告）。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：**10/10 全绿**，其中 `bounded.sh`、`spawn.sh`、`layers.sh` 欠账表均为 **0 项**。
- 零新增依赖核验：`git diff de7aa64 -- Cargo.lock ui/package-lock.json` 为空。
- Rust 沙箱内编译与测试：
  - `RUSTC_WRAPPER="" cargo test -p zutalk-core --test host --no-run`：编译通过（共 9 条测试，含新增 2 条）；
  - `core/tests/host.rs` 非 TCP 单元测试 3/3 通过：`host_state_transition_table`、`frame_crypto_and_link_fragment_only`、`host_lock_stores_only_room_id_and_never_the_key`；
  - `core/tests/liveness.rs`：
    - `i8_what_one_change_sends_does_not_grow_with_the_library`：`i8: bytes_5=1320 bytes_100=1322 ratio=1.00 state_pushes=(0,0) inv_bytes=(60,60)`；
    - `every_event_in_the_catalogue_can_be_told_apart`：通过；
    - `two_minute_recording_tally_matches_the_table`：通过（`counts = 30`、`peaks = 2`、`spreads = 10`、`queues = 6`，`stats.full == 0`）。
- 前端校验与测试（`ui/`）：
  - `npm run check && npm run build`：通过（`ui/dist/{index,captions,viewer}.html` 三入口正常输出）；
  - `npm test`：**14/14 文件、106/106 测试通过**（含 `words.test.ts` 8 语全覆盖、`host_outbound.test.tsx` 6/6、`captions_viewer.test.tsx` 5/5、`room.test.ts` 4/4）。

### 沙箱外全量门禁（`RUSTC_WRAPPER="" scripts/build/check.sh`）

- `rustfmt` + `clippy --workspace --all-targets -- -D warnings`：零警告通过。
- 全部 10 个规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`pending.sh`、`shell.sh`、`spawn.sh`、`ui.sh`）：**10/10 全绿，全仓零欠账**。
- Rust 全量测试：**751 passed, 0 failed, 6 ignored**（含 `core/tests/host.rs` 全部 9 条测试，包括双保险崩溃恢复 `host_crash_recovery_double_insurance_leaves_nothing_outside` 与实时双语/精修/交城/自检包隐私字节审计 `host_city_and_caption_secrets_never_reach_logs_diagnose_lock_or_settings`，以及 `server/tests/` 与 `app/tests/` 全部测试）。
- UI 全量测试与构建：`tsc` + `vite build`（3 入口）+ Vitest **14 files / 106 passed**。
- 总结论：**`check: all green`**。

## 遗留 / 需负责人确认或动手的事

1. **档案馆 v0.3 记录类型（`DECISIONS.md` #6）**：`core/src/out/city.rs` 已收拢为单张常量表 `ARCHIVE_TYPES`（当前为 `talk/*`）；待 `pol` 档案馆 v0.3 定案读 `talk/*` 还是 `tape/*` 后，只改这一张表即可。
2. **线上边缘反代与 HTTPS 域名**：观众链接 `{server}/r/{id}#{key}` 依赖前端边缘把 `/r/*` 指向静态 `viewer.html` 并配置 TLS 与限流；P5 全部自动化测试跑在本机回环端口与开发替身。
3. **三平台桌面真机字幕窗验证**：macOS / Windows / Linux 多显示器下 `set_ignore_cursor_events` 穿透点击、透明背景合成、舞台全屏与拖拽后重启恢复几何，需负责人在真机实测确认。
