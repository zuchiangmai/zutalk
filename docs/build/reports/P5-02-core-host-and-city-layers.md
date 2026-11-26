状态：完成

## 改了什么

1. **删除旧单条录音分享与持久化 `talk/share`，新建核心主持模块 `core/src/out/host.rs`**：
   - 删除 `core/src/out/room.rs`、`core/bindings/{Row,Open,Room}.ts`、`core/tests/room.rs`；从 `core/src/record/shelf.rs` 删除 `SHARE` / `Share` 与 `shares.jsonl` 分支；从 `core/src/app/proto.rs` 删除 `Row` / `Open` / `Room` / `State.rooms` / `Command::Share` / `Command::Unshare`；
   - `core/src/out/host.rs`（零 `crate::app::proto` 依赖）实现：
     - 具名带单位常量 `HOST_SLOT_MS = 250`、`HOST_ARM_POLL_MS = 3_000`、`HOST_KEEPALIVE_MS = 30_000`、`HOST_EXIT_WAIT_MS = 1_500`、`HOST_LOCK_FILE = "host.lock"`；
     - 纯加密与链接原语 `seal` / `open` / `link` / `is_room_id`，以及只留槽传输 `write(&Ours, room, key, Write::{Make, Slot(Option<&[u8]>), Gone}) -> Result<u32, End>`（`Make`/`Gone` 无需密钥，响应解析实时 `viewers` 并发出 `host.room`）；
     - 原子锁文件工具 `write_lock` / `read_lock` / `clear_lock`（`<data>/host.lock` 仅存 32 位小写十六进制房间 id，永不存密钥）；
     - 显式主持状态机 `HostEvent`、`HostState`（`Idle` / `Arming` / `Armed` / `Live` / `Stopping`）、`HostStepEvent` 与纯转换函数 `host_step`。
2. **`core/src/out/city.rs` 归档类型表集中与全仓 `layers.sh` 欠账清零**：
   - 集中定义 `ArchiveTypes` 与 `ARCHIVE_TYPES`（`ev/event`、`talk/recording`、`talk/line`、`talk/speaker`，对齐 `DECISIONS.md` #6 单表切换）；
   - 新增本地 `FilingStep { step, done, of }` 移除对 `crate::app::proto::Writing` 的反向依赖（由 `app/work.rs` 包装为 `Writing`）；
   - 清空 `scripts/build/rules/layers.sh` 中全部剩余 `debt`（`out/city.rs` 与 `out/room.rs`）。
3. **接入核心协议、两道保险与最小前端适配**：
   - `State` 改为定界 `host: HostState`；`Session` 增加 `outbound`、`viewers`、`event_title`；`Command` 新增 `HostArm`（`needs_person = true`）、`HostCancel`、`StopOutbound`（均更新手工脱敏 `fmt::Debug`）；
   - `Core` 增加无锁 `is_hosting()` 与 `wait_host_idle(max_ms)`（零新增 `Stats` 行）；
   - `app/hub.rs` 与 `app/work.rs` 落地内存随机 32 字节临时密钥（`Zeroizing`）、3 s 待开场轮询 / 250 ms 直播推槽 / 30 s 静音保活、关闭观众字幕 / `HostCancel` / `StopOutbound`（本机继续录音）/ `Stop` / `SignOut` 均发 `gone` 并删 `host.lock`，以及 `boot` 与 `Signed` 的 `host.lock` 崩溃恢复（`net.host_recover` + `host.recover` 事件）；
   - 更新 `docs/EVENTS.md`（登记 `host.state`、`host.room`、`host.recover` 与 `job.start`/`job.done` 种类）、重生成 `core/bindings/*.ts`、最小适配 UI（`words.ts`、`TopBar.tsx`、`Shell.tsx`、`LivePage.tsx`、`CaptionsWindow.tsx`、只留槽 `viewer/{room.ts,ViewerPage.tsx}`、`ui/dev/mock.ts`、全部 8 语 `ui/lang/*.json` 新增 `code.host.{stopped,busy,cancelled}` 与受影响测试夹具）。

## 证据

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（零警告）。
- 全部 10 个规则脚本：全绿，`scripts/build/rules/layers.sh` 的 `debt` 表已清空（零欠账）。
- `RUSTC_WRAPPER="" cargo test -p zutalk-core --lib export_bindings`：41/41 通过。
- 沙箱内 Rust 测试：
  - `core/tests/host.rs`（非 TCP 3/3 通过，全 7 条编译通过）：`host_state_transition_table`、`frame_crypto_and_link_fragment_only`、`host_lock_stores_only_room_id_and_never_the_key`；
  - `core/tests/city.rs`：`archive_types_table_matches_packed_records` 通过（全 7 条编译通过）；
  - `core/tests/shelf.rs`：12/12 通过；
  - `core/tests/app.rs`：`commands_that_need_a_person_are_listed_in_one_table` 通过（全 52 条编译通过）；
  - `core/tests/liveness.rs`：45/46 非 TCP 测试全部通过（含 `every_event_in_the_catalogue_can_be_told_apart`、`i8_what_one_change_sends_does_not_grow_with_the_library`、`two_minute_recording_tally_matches_the_table`、`nothing_said_or_secret_reaches_the_log`、`a_diagnosis_holds_nothing_that_was_said_and_no_secret`；仅依赖回环 MockServer 的 `i2_background_work_runs_beside_a_recording` 受沙箱限制）；
  - `zutalk`（桌面外壳）：20/20 通过（`beat` 5、`pump` 11、`updates` 4）。
- 前端校验与测试（`ui/`）：
  - `npm run check` & `npm run build`：通过；
  - `npm test`：13/13 文件、97/97 测试通过（含 `words.test.ts` 8 语全覆盖）；
  - `Cargo.lock` 与 `ui/package-lock.json` 无变更。

## 遗留 / 需协调人注意

- 涉及本地回环端口的 4 条 `core/tests/host.rs` 集成测试（`host_arm_live_stop_outbound_keeps_local_recording_and_ends_room`、`host_cancel_stop_recording_and_sign_out_all_send_gone_and_clear_lock`、`crash_recovery_reads_host_lock_on_next_boot_and_sends_gone`、`unauthenticated_host_arm_is_refused_without_touching_server`）与 `core/tests/{app,city,liveness}.rs` 中的 TCP 测试将在批末非沙箱环境通过 `scripts/build/check.sh` 统一跑通。
- `Setup.caption` 按场合样式与外壳 `app/src/windows.rs` 窗口控制留给 `P5-03`。
