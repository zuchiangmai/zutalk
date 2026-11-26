# P5-02 核心主持模块（`out/host.rs`）、两道保险与 `out/city.rs` 分层清债

## 目标

做完以后：

1. **彻底移除旧单条录音分享，落地 `core/src/out/host.rs`**：
   - 删除 `core/src/out/room.rs`、`record/shelf.rs` 的 `SHARE` / `Share`、`app/proto.rs` 的 `Row` / `Open` / `Room` / `State.rooms` / `Command::Share` / `Command::Unshare`（并删除废弃的 `core/bindings/{Row,Open,Room}.ts`）；
   - 新建 `core/src/out/host.rs`（零 `crate::app::proto` 依赖）：
     - 保留纯加密与链接原语 `seal` / `open` / `link`，`write(&Ours, room, key, Write::{Make, Slot(Option<&[u8]>), Gone}) -> Result<u32, End>` 返回服务器回报的实时 `viewers` 人数；
     - 显式主持状态机 `HostEvent`、`HostState`（`Idle` / `Arming` / `Armed` / `Live` / `Stopping`）、`HostStepEvent` 与纯函数 `host_step`；
     - 房间密钥仅驻内存（`Zeroizing<[u8; 32]>`），绝不入 `shelf`、`settings.json` 或日志；
     - `<data>/host.lock` 原子写入且**只存 32 位十六进制房间 id**（无密钥）；
     - 主持期间自动以 `HOST_ARM_POLL_MS`（3 s，待开场刷新等待人数并保活）和 `HOST_SLOT_MS`（250 ms，录音中发加密 `Frame.columns`）+ `HOST_KEEPALIVE_MS`（30 s，静音/暂停保活，防 2 分钟服务器 TTL 误杀）同步槽并回写 `viewers` 到 `HostState` 与 `Session.viewers`；
     - 关闭观众字幕、取消主持、录音中 `Command::StopOutbound`（本机继续录、仅断对外）、停止录音/录音自动结束、退出登录，均立即发 `gone` 并删除 `host.lock`；
     - 崩溃恢复：启动（`boot`）与登录（`Signed`）发现遗留 `<data>/host.lock` 且已登录时，自动后台发 `gone`，收到 `ok` 或 `room.gone` 后删 `host.lock` 并记 `host.recover`。
2. **`out/city.rs` 归档类型表集中与全仓 `layers.sh` 欠账清零**：
   - `core/src/out/city.rs` 集中定义 `pub struct ArchiveTypes` 与 `pub const ARCHIVE_TYPES: ArchiveTypes`（`ev/event`、`talk/recording`、`talk/line`、`talk/speaker`，满足 `DECISIONS.md` #6 单表切换 `tape/*`）；
   - 以 `out/city.rs` 本地 `pub struct FilingStep { pub step: String, pub done: u32, pub of: u32 }` 取代对 `crate::app::proto::Writing` 的反向依赖（由 `app/work.rs` 包装为 `Writing`）；
   - 清空 `scripts/build/rules/layers.sh` 的全部剩余 `debt` 行（`out/city.rs` 与 `out/room.rs`）。
3. **协议、事件与最小 UI 类型同步**：
   - `State` 改为携带定界 `pub host: HostState`；`Session` 增加定界字段 `pub outbound: bool`、`pub viewers: u32`、`pub event_title: Option<String>`；
   - `Command` 新增 `HostArm { event: Option<HostEvent>, captions: bool }`（`needs_person = true`）、`HostCancel`（`needs_person = false`）、`StopOutbound`（`needs_person = false`）；
   - `Core` 新增无锁辅助 `pub fn is_hosting(&self) -> bool` 与 `pub async fn wait_host_idle(&self, max_ms: u64)`（供 P5-03 退出收尾使用）；
   - `docs/EVENTS.md` 登记 `host.state`、`host.room`、`host.recover` 及新 `job.start`/`job.done` 种类；
   - 刷新 `core/bindings/*.ts`、同步最小 UI 类型适配（去掉对 `State.rooms` 与 `Open` 的旧引用，简化 `viewer/room.ts` 为只留槽）及全部 8 语 `ui/lang/*.json` 新增 `code.*` 键，确保 `scripts/build/check.sh` 全绿。

## 背景

- 设计：`docs/build/phases/P5.md` §3.3、§3.5；`docs/PLAN.md` §2.1、§2.3、§2.4、§2.7、§5.5；`docs/build/DECISIONS.md` #6。
- 涉及文件：
  - 删除 `core/src/out/room.rs`，新增 `core/src/out/host.rs`，更新 `core/src/out.rs`、`core/src/out/city.rs`
  - `core/src/record/shelf.rs`、`core/src/app/{proto,hub,work,core,recording}.rs`
  - `scripts/build/rules/layers.sh`、`docs/EVENTS.md`
  - `core/bindings/*.ts`（删除 `Row.ts`/`Open.ts`/`Room.ts`，生成 `HostEvent.ts`/`HostState.ts` 等）
  - 最小 UI 编译适配：`ui/src/bus/words.ts`、`ui/src/shell/{Shell,TopBar}.tsx`、`ui/src/pages/LivePage.tsx`、`ui/src/captions/CaptionsWindow.tsx`、`ui/src/viewer/{room.ts,ViewerPage.tsx}`、`ui/dev/mock.ts`、`ui/lang/*.json`、`ui/tests/` 受影响的夹具
  - 删除/迁移 `core/tests/room.rs` → 新增 `core/tests/host.rs`，更新 `core/tests/{app,shelf,city}.rs`

## 做法

### 1. `core/src/out/host.rs`

- 具名带单位常量（带理由注释，过 `numbers.sh`）：
  - `pub const HOST_SLOT_MS: u64 = 250;` // max live slot publish interval (`DESIGN.md` §5.3)
  - `pub const HOST_ARM_POLL_MS: u64 = 3_000;` // waiting viewer refresh and keepalive while armed (under server 120 s TTL)
  - `pub const HOST_KEEPALIVE_MS: u64 = 30_000;` // live keepalive during silence or pause (under server 120 s TTL)
  - `pub const HOST_EXIT_WAIT_MS: u64 = 1_500;` // max wait for `gone` during clean desktop exit
  - `pub const HOST_LOCK_FILE: &str = "host.lock";`
- 加密与只留槽传输：
  - `seal(key: &[u8; 32], nonce: &[u8; 12], plain: &[u8]) -> Result<Vec<u8>, End>`
  - `open(key: &[u8; 32], frame: &[u8]) -> Result<Vec<u8>, End>`
  - `link(server: &str, id: &str, key: &[u8; 32]) -> String`
  - `pub enum Write<'a> { Make, Slot(Option<&'a [u8]>), Gone }`
  - `pub async fn write(ours: &Ours, room: &str, key: Option<&[u8; 32]>, write: Write<'_>) -> Result<u32, End>`：
    - `Make` / `Gone` 不需要 `key`（`host.lock` 崩溃清理只拿到 `room` id 即可发 `Gone`！）；
    - `Slot(Some(plain))` 用 `key` 加密并 base64；`Slot(None)` 发空串 `""`；
    - 从响应 JSON 解析 `viewers`（`u64` 截断为 `u32`，缺失取 `0`）；
    - 发出 `host.room`（`info!` 成功、`warn!` 失败）：`action`（`"make"`/`"slot"`/`"gone"`）、`code`、`viewers`、`ms`（**绝不记录 room id 或 key**）。
- `host.lock` 原子读写工具（只放 32 位小写十六进制 room id）：
  - `pub fn write_lock(data: &Path, room_id: &str) -> Result<(), &'static str>`（校验 32 位十六进制，写 `host.lock.tmp` 后原子 `rename`）；
  - `pub fn read_lock(data: &Path) -> Option<String>`（读取并 trim，校验 32 位十六进制，损坏文件返回 `None`）；
  - `pub fn clear_lock(data: &Path)`（若存在则删除，忽略 `NotFound`）。
- 显式状态机 `HostEvent`、`HostState`、`HostStepEvent` 与纯函数 `pub fn host_step(state: &HostState, event: HostStepEvent) -> HostState`：
  - `HostState::code(&self) -> &'static str`（`"Idle"` / `"Arming"` / `"Armed"` / `"Live"` / `"Stopping"`）；
  - `HostState::is_outbound(&self) -> bool`（`Armed { captions: true, .. }` 或 `Live { outbound: true, .. }`）；
  - `HostState::is_active(&self) -> bool`（非 `Idle`）；
  - 覆盖全部转换：
    - `Idle` + `Arm { event, captions: true }` → `Arming { event, captions: true }`
    - `Idle` + `Arm { event, captions: false }` → `Armed { event, captions: false, link: None, viewers: 0 }`
    - `Arming` + `RoomMade { link }` → `Armed { event, captions: true, link: Some(link), viewers: 0 }`
    - `Arming` + `RoomFailed` → `Idle`
    - `Armed` + `Arm { event, captions }` → 同配置更新 `event`；`captions: false -> true` 转 `Arming`；`captions: true -> false` 转 `Armed { event, captions: false, link: None, viewers: 0 }`（由 `Work` 后台发 `gone`）
    - `Armed` + `Viewers(n)` → `Armed { viewers: n, .. }`
    - `Armed` + `RecordingStarted` → `Live { event, outbound: captions && link.is_some(), link, viewers }`
    - `Live` + `Viewers(n)` → 若 `outbound` 则更新 `viewers: n`
    - `Live` + `StopOutbound` → `Live { event, outbound: false, link: None, viewers: 0 }`（录音继续！）
    - `Live` / `Armed` / `Arming` + `Cancel` / `RecordingStopped` → 若有活跃房间先转 `Stopping` 等 `gone` 落地或直接归 `Idle`（收到 `Stopped` 归 `Idle`）。

### 2. `core/src/out/city.rs` 与 `scripts/build/rules/layers.sh`

- 在 `core/src/out/city.rs` 顶部新增：
  ```rust
  /// Record types delivered to a city archive (`DECISIONS.md` #6).
  /// Kept in one table so aligning with archive v0.3 (`tape/*` vs `talk/*`) is a one-table change.
  pub struct ArchiveTypes {
      pub event: &'static str,
      pub recording: &'static str,
      pub line: &'static str,
      pub speaker: &'static str,
  }

  pub const ARCHIVE_TYPES: ArchiveTypes = ArchiveTypes {
      event: "ev/event",
      recording: "talk/recording",
      line: "talk/line",
      speaker: "talk/speaker",
  };
  ```
  `city::calendar` 用 `ARCHIVE_TYPES.event`，`city::records` 用 `ARCHIVE_TYPES.{recording, line, speaker}`（不再从 `record::shelf` 引 `RECORDING`/`LINE`/`SPEAKER`）。
- 删除 `use crate::app::proto::Writing;`，定义 `pub struct FilingStep { pub step: String, pub done: u32, pub of: u32 }`，`file`/`file_job` 回调改为 `impl Fn(FilingStep) + Sync`；`app/work.rs` 中将 `FilingStep` 转成 `Writing { id, step, done, of }` 写入 `progress_reg`。
- 从 `scripts/build/rules/layers.sh` 的 `debt` 中删除 `out/city.rs` 与 `out/room.rs` 两行（`debt` 清空）。

### 3. `core/src/app/{proto,hub,work,core,recording}.rs` 与 `record/shelf.rs`

- `record/shelf.rs`：删除 `SHARE`、`Share` 与 `pack_path` 中的 `SHARE` 分支。
- `app/proto.rs`：
  - 删除 `Row`、`Open`、`Room`、`State.rooms`、`Command::Share`、`Command::Unshare`；
  - `pub use crate::out::host::{HostEvent, HostState};`；
  - `State` 增加 `pub host: HostState`；
  - `Session` 增加 `pub outbound: bool`、`pub viewers: u32`、`pub event_title: Option<String>`；
  - `Command` 增加 `HostArm { event: Option<HostEvent>, captions: bool }`、`HostCancel`、`StopOutbound`；更新 `kind()` 与 `needs_person()`（`HostArm => true`，`HostCancel` / `StopOutbound => false`）。
- `app/hub.rs`（保持零 IO，过 `hub.sh`）：
  - 统一 `set_host(event, why)` 驱动 `host_step` 并在状态变化时发 `host.state`；
  - 收到 `Msg::HostViewers(n)` 时同步更新 `state.host` 并 `self.session_reg.change(...)` 更新 `Session.viewers`（若在录且 `outbound == true`）；
  - `Command::Start` 时按当前 `state.host` 初始化 `Session { id, started, status, outbound, viewers, event_title }`，并通知 `work.host_live(true)` 切换为直播发槽节奏；
  - `Command::StopOutbound`：若正在对外，立即通知 `work.host_stop_room()`（发 `gone` + 删 `host.lock`），转 `HostStepEvent::StopOutbound`，`session_reg.change` 设 `outbound = false, viewers = 0`，发提示 `self.said.say("host.stopped")`；**不停止录音**；
  - `Command::Stop` / `Msg::Ended` / `Command::HostCancel` / `Command::SignOut`：立即调用 `work.host_stop_room()` 并清理 `HostState`；
  - **不新增任何 `Stats` 行**（守住 `stats.full == 0`）。
- `app/work.rs`：
  - 内存持有 `host_live: Arc<Mutex<Option<ActiveHost>>>`（含 `room_id: String`、`key: Zeroizing<[u8; 32]>`、`ours: Ours`、`live_flag: Arc<AtomicBool>`、`cancel: CancellationToken`）；
  - `host_make`：先生成随机 32 位 room id 与随机 32 字节临时密钥（**随机生成，不从持久化秘密派生**，进一步确保无盘关联！），先写 `<data>/host.lock` → 发 `host::write(..., Make)` → 失败则清 `host.lock` 并报 `Msg::HostFailed(code)`；成功则起监督任务 `host.slot`（`Armed` 态每 `HOST_ARM_POLL_MS` 发空槽拿人数保活；`Live` 态监听 `frame.next()` 每 `HOST_SLOT_MS` 发加密 `Frame.columns`，遇静音/暂停按 `HOST_KEEPALIVE_MS` 保活；每次回报 `viewers` 发 `Msg::HostViewers(n)`），并回 `Msg::HostReady { link }`；
  - `host_stop_room`：取消 `host.slot` 任务，持克隆的 `(room_id, ours)` 起监督任务 `net.host_gone` 发 `host::write(&ours, &room_id, None, Write::Gone)`，成功或 `room.gone` 后删 `host.lock` 并回 `Msg::HostStopped`；
  - `boot` 与 `Signed`：若已登录（`snap.ours` 存在）且 `host::read_lock(&config.data)` 返回残留 `room_id`，起监督任务 `net.host_recover` 发 `Write::Gone`，收到 `Ok` 或 `End::Fatal("room.gone")` 后 `host::clear_lock(&config.data)` 并发 `info!("host.recover", code = …)`（失败发 `warn!("host.recover", code = …)`，绝不发给未登录态）。
- `app/core.rs`：
  - 保留 `state_reading: Reading<State>` 在 `Core` 上，新增 `pub fn is_hosting(&self) -> bool`（读 `state_reading.now().1.host.is_active()`）与 `pub async fn wait_host_idle(&self, max_ms: u64)`（`tokio::time::timeout` 等待 `!state.host.is_active()`）。
- `docs/EVENTS.md`：登记 `host.state`、`host.room`、`host.recover`，在 `job.start`/`job.done` 种类列表中移除 `share.*`/`unshare.*` 并补入 `host.make`、`host.slot`、`host.gone`、`host.recover`。
- 同步生成 `core/bindings/*.ts`（删除废弃 `Row.ts`/`Open.ts`/`Room.ts`），最小适配 `ui/src/{bus/words.ts,shell/Shell.tsx,shell/TopBar.tsx,pages/LivePage.tsx,captions/CaptionsWindow.tsx,viewer/room.ts,viewer/ViewerPage.tsx}`、`ui/dev/mock.ts`、`ui/tests/` 及全部 8 语 `ui/lang/*.json` 新增 `code.host.stopped`、`code.host.busy`、`code.host.cancelled`。

## 显式与可观测

- **纯状态机与纯规则**：`HostState` + `host_step`、`ARCHIVE_TYPES` 表、`host::{seal, open, link, write_lock, read_lock, clear_lock}` 均配独立测试。
- **具名带单位常量**：`HOST_SLOT_MS`、`HOST_ARM_POLL_MS`、`HOST_KEEPALIVE_MS`、`HOST_EXIT_WAIT_MS` 均带单位与理由注释（过 `numbers.sh`）。
- **无新增 `Stats` 行 & 零 `layers.sh` 欠账**：复用既有 `jobs.done`/`jobs.failed`/`commands.done`，`layers.sh` 清零。
- **结构化事件与隐私**：`host.state`、`host.room`、`host.recover` 零房间 id/密钥/链接/标题；`host.lock` 绝不存密钥；未登录调用 `HostArm` 直接拒 `sign_in.first` 且零网络请求。

## 先写的测试

1. 新建 `core/tests/host.rs`（取代旧 `core/tests/room.rs`）：
   - `host_state_transition_table`：穷举 `host_step` 全部状态×事件转换；
   - `frame_crypto_and_link_fragment_only`：验证 AES-256-GCM 封开帧与密钥只出现在 `#` 后；
   - `host_lock_stores_only_room_id_and_never_the_key`：验证原子写/读/删 `host.lock` 及损坏文件处理；
   - `host_arm_live_stop_outbound_keeps_local_recording_and_ends_room`：连真实 `zutalk-server` → `HostArm { captions: true }` → 验证 `host.lock` 落盘且只含房间 id → 观众连 SSE 断言 `State.host` 显示 `viewers == 1` → `Start` 录音 → 断言 `Session.outbound == true, Session.viewers == 1` 且观众解出实时字幕 → `StopOutbound` → 断言观众立刻收到 `gone`、房间 404、`host.lock` 删除、`Session.outbound == false, Session.viewers == 0`，而本机录音继续正常录制到 `Stop`；
   - `host_cancel_stop_recording_and_sign_out_all_send_gone_and_clear_lock`：分别测离开/取消、停止录音、退出登录三条路径均发 `gone` 并清 `host.lock`；
   - `crash_recovery_reads_host_lock_on_next_boot_and_sends_gone`：主持中直接 drop `Core`（模拟进程崩溃，`host.lock` 留在磁盘）→ 用同一数据目录重启 `Core` → 断言自动发 `gone`、清 `host.lock`、观众收到 `gone` 且日志可见 `host.recover(code = "ok")`；
   - `unauthenticated_host_arm_is_refused_without_touching_server`：未登录发 `HostArm` 返 `sign_in.first` 且服务器零请求。
2. `core/tests/city.rs` 新增断言：`city::ARCHIVE_TYPES` 涵盖 `event`/`recording`/`line`/`speaker` 四项并与打包输出一致。
3. 更新 `core/tests/app.rs` 与 `core/tests/shelf.rs`（移除已废弃 `Share`/`Unshare`/`SHARE` 测试，验证 `Command::needs_person` 表含 `HostArm => true`、`HostCancel => false`、`StopOutbound => false`）。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿；
- `scripts/build/rules/layers.sh` 的 `debt` 表为空且通过；
- `core/tests/liveness.rs::i8_what_one_change_sends_does_not_grow_with_the_library` 持续通过（比值 `1.00`），`stats.full == 0`。

## 不做

- 不做 `Setup.caption` 按场合样式与外壳 `app/src/windows.rs` 窗口控制（留 P5-03）；
- 不做完整 `HostPage` / `FileArchiveCard` / `OutboundPage` UI（留 P5-04）。
