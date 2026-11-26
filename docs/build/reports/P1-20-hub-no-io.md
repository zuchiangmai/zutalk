状态：完成

# P1-20 hub 只算不做（二）：启动、身份、钥匙、设置、更新；规则脚本：报告

## 改了什么

1. **纯状态机 `Boot` 与零 IO 的 `core/src/app/hub.rs`**：
   - 新增枚举 `Boot { Starting, Ready, Installing }`（带 `code()`）、`BootEvent { Ready, Install, InstallFailed }`、纯转换函数 `boot_step(boot, event)` 和纯判定 `Boot::accepts(&Command)`（`Starting` 拒所有命令返回 `"starting"`；`Installing` 拒 `Start`/`Import` 返回 `"update.installing"`）；每次状态变化发 `info!("hub.boot", from, to, why)`。
   - `Hub` 不再持有 `Config`、`reqwest::Client`、`signing` 或 `invite`；启动前的首份 `State` 推迟到收到 `Msg::Booted` 切为 `Boot::Ready` 之后才发布；`hub.rs` 全文件仅保留收信循环 `while let Some(msg) = mail.recv().await` 这一处 `.await`，不含任何文件、网络、钥匙库、设备、安装、剪贴板或任务启动调用。
2. **启动、设备、身份、设置、更新全部迁入 `core/src/app/work.rs` 与 `core/src/app/core.rs`**：
   - `Config` 新增 `devices: Devices` 端口（生产默认 `audio::devices`，测试可替换）；`Core::start` 构造 `Work` 后调 `work.boot()`：
     - `Job::Boot` 跑在 `desk` 上，依次读 `settings.json`、读/造身份并 `Who::of`（发 `identity.who(why = "boot")`）、读 0.8 旧邀请，每步发 `boot.step`（`step = "settings" | "identity" | "invites"`，`code`，`ms`，失败为 `warn!`）；
     - 设备列举独立跑在 `work` 子树的 `devices` 任务（`tree.blocking("devices", ...)`），发 `boot.step(step = "devices", code, ms, n)`，不带设备名，卡住也不阻塞启动。
   - 身份与钥匙：`Command::SignIn`（`net.signin`）、`Command::SignOut`（`Job::SignOut`，`kind = "signout"`，回到本机身份）、`Command::CopyRotation`（`Job::Clip`，在 `desk` 直交 `config.clip`，钥匙不经 hub 邮箱）、`Command::RotationSaved`（`Job::RotationSaved`）、`Command::City`（`net.calendar`）、0.8 造身份（`net.create`）与认领邀请（`net.claim`）全部迁入 `Work`；`Me`/`SecretKey` 不再经过 `hub.mail`；事件中无任何 DID、句柄、登录码或钥匙。
   - 设置与自有钥匙：`Command::Setup` 立即更新 `state.setup` 后排 `Job::Settings`（`kind = "settings"`）写盘，失败仅说 `settings`、不退回内存选择；`Command::OwnKey` 立即按 `From::of` 更新 `state.keys` 后排 `Job::OwnKey` 落盘并清点欠写草稿。
   - 更新：`Command::Update` 非录音时将 `Boot` 切到 `Installing`，在 `work` 子树起 `update.install` 任务（`tree.blocking("update.install", ...)`，`kind = "install"`）；若安装端口返回则回 `Ready` 并说 `update.install`。
3. **规则脚本、文档与多语词表**：
   - 新增 `scripts/build/rules/hub.sh`（对 `core/src/app/hub.rs` 禁止 `std::fs`、`fs::`、`tokio::fs`、`File::`、`reqwest`、`spawn_blocking`、`tokio::spawn`、`.spawn(`、`.blocking(`、`.keep(`、`Shelf`、`tape::`、`audio::devices`、`keys::<fn>(`、`.install)(`、`.clip)(`，并限制 `.await` ≤ 1 处；`Core::start` 位于 `core/src/app/core.rs`）。
   - `scripts/build/rules/spawn.sh` 删除 `core/src/app/hub.rs` 欠账行（hub 欠账清零）。
   - `docs/EVENTS.md` 补录 `hub.boot`、`boot.step` 及 `job.done` 新增 `kind`；`docs/DESIGN.md` §5.10 改为异步安装 + `Boot::Installing` 机制。
   - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json` 全部 8 个文件补齐 `code.starting` 与 `code.update.installing`。

文件：`core/src/app/core.rs`、`core/src/app/work.rs`、`core/src/app/hub.rs`、`scripts/build/rules/hub.sh`、`scripts/build/rules/spawn.sh`、`docs/EVENTS.md`、`docs/DESIGN.md`、`ui/lang/*.json`（8 个）、`core/tests/app.rs`、`core/tests/liveness.rs`、`core/tests/glossary.rs`、`core/tests/retain.rs`、本报告。

## 证据

### 1. `hub.sh` 抓旧 `hub.rs` 的证明（喂入 P1-19 之前 `031c117:core/src/app/hub.rs`，共报 52 处违规，非零退出；当前 `core/src/app/hub.rs` 0 处违规）

```text
.../tmp:16: forbidden in hub (tape::)
.../tmp:27: forbidden in hub (Shelf)
.../tmp:105: forbidden in hub (tape::)
.../tmp:238: forbidden in hub (reqwest)
.../tmp:308: forbidden in hub (.spawn()
.../tmp:309: forbidden in hub (.spawn()
.../tmp:406: forbidden in hub (fs::)
.../tmp:415: forbidden in hub (audio::devices)
.../tmp:416: forbidden in hub (keys::<fn>()
.../tmp:581: forbidden in hub (tokio::spawn)
.../tmp:599: forbidden in hub (.clip)()
.../tmp:628: forbidden in hub (.install)()
.../tmp:864: forbidden in hub (spawn_blocking)
（共计 52 处，含多个 .await）
```

`git grep -n "spawn\|blocking(" core/src/app/hub.rs` 输出为空（0 行）。

### 2. `core/src/app/hub.rs` 的 `use` 列表与文件行数

```rust
pub(crate) use crate::app::core::Voice;
pub use crate::app::core::{Clip, Config, Core, Devices, Install, Outlet};
pub use crate::app::proto::Notices;
use crate::app::proto::{
    Change, Command, Device, From, Level as SoundLevel, Room, Row, Session, Sound, State, Status,
    Writing,
};
pub use crate::app::recording::{DRAFT_QUEUE_LINES, TAPE_QUEUE_BLOCKS};
pub use crate::app::work::DESK_QUEUE_JOBS;
use crate::app::work::{BootOut, OwnKeyOut, Refreshed, SignedOut, Survey, Work};
use crate::capture::audio::Source;
use crate::capture::clock::wall_ms;
use crate::id::keys::Service;
use crate::live::frame::{Frame, MAX_COLUMNS};
use crate::live::lane::{self, Mode};
use crate::out::city::{Calendar, Filing};
use crate::out::export::EXPORTERS;
use crate::record::shelf::About;
use crate::rt::queue::Rx;
use crate::rt::signal::Register;
use crate::rt::stats::{Counter, Spread};
use crate::rt::supervise::{End, Tree};
use crate::rt::water::{Gauge, Water};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::watch;
use zutalk_log::{info, warn};
```

行数：
- `core/src/app/hub.rs`：746 行
- `core/src/app/work.rs`：2017 行
- `core/src/app/recording.rs`：1536 行
- `core/src/app/core.rs`：279 行

### 3. `scripts/build/rules/spawn.sh` 欠账表前后对比

改前：
```text
core/src/app/hub.rs 4 P1-20
core/src/ai/write.rs 2 P1-21
core/src/out/city.rs 1 P1-21
app/src/main.rs 1 P1-23
```

改后：
```text
core/src/ai/write.rs 2 P1-21
core/src/out/city.rs 1 P1-21
app/src/main.rs 1 P1-23
```

### 4. 一次完整启动的真实捕获日志行（摘自 `a_command_before_the_core_is_ready_is_turned_away`）

```text
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.start", fields: [("tree", Text("work")), ("task", Text("desk"))] }
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.start", fields: [("tree", Text("work")), ("task", Text("devices"))] }
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.start", fields: [("tree", Text("core")), ("task", Text("hub"))] }
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.start", fields: [("tree", Text("core")), ("task", Text("stats"))] }
Line { level: Info, component: "zutalk_core::app::work", code: "boot.step", fields: [("step", Text("settings")), ("code", Text("ok")), ("ms", U64(0))] }
Line { level: Info, component: "zutalk_core::app::work", code: "identity.who", fields: [("from", Text("nobody")), ("to", Text("local")), ("why", Text("boot")), ("made", Bool(false))] }
Line { level: Info, component: "zutalk_core::app::work", code: "boot.step", fields: [("step", Text("identity")), ("code", Text("ok")), ("ms", U64(0))] }
Line { level: Info, component: "zutalk_core::app::work", code: "boot.step", fields: [("step", Text("invites")), ("code", Text("ok")), ("ms", U64(0))] }
Line { level: Info, component: "zutalk_core::rt::supervise", code: "task.start", fields: [("tree", Text("work")), ("task", Text("job.sweep"))] }
Line { level: Info, component: "zutalk_core::app::hub", code: "hub.boot", fields: [("from", Text("Starting")), ("to", Text("Ready")), ("why", Text("boot"))] }
```

### 5. 5 虚拟分钟录音 + 20 条混合命令的 `hub.turn_us` 实测（`five_minute_recording_with_twenty_commands_measures_hub_turns`）

- `hub.turn_us`：`n = 54`，`p50 = 5 µs`，`p99 = 100 µs`，`most = 74 µs`（桶上沿 100 µs）
- `hub.turn.slow`：**0 条**（全部 54 圈均远低于 1,000 µs 预算）

### 6. 新增与更新测试（全部通过）

- `core/tests/app.rs`：
  - `the_hub_boots_as_its_table_says`（`Boot` 转换表与命令准入表）
  - `a_command_before_the_core_is_ready_is_turned_away`
  - `a_device_list_that_never_comes_holds_up_nothing`
  - `a_setting_takes_effect_at_once_and_is_kept`
  - `settings_that_cannot_be_saved_say_so_and_still_hold`
  - `nothing_is_recorded_while_an_update_is_being_put_in_place`
  - `a_key_just_set_counts_for_the_next_recording`
  - `signing_out_goes_back_to_the_device_off_the_hub`
  - `the_recovery_key_goes_to_the_clipboard_and_through_nothing_else`（补全日志零泄漏断言）
  - `the_core_runs_on_a_tree_with_a_name`（更新为 4 个初始任务含 `work/devices`）
  - 既有 7 处未等启动完成的旧测试补 `app.until(|_| true).await`（与异步启动 `Boot::Starting` 契约一致，未改任何行为断言）；`more_codes_than_are_kept_lose_the_oldest` 改为等异步 `CopyRotation` 的 `rotation.none` 提示。
- `core/tests/liveness.rs`：
  - `five_minute_recording_with_twenty_commands_measures_hub_turns`

### 7. 门禁与验证

- `cargo fmt --all -- --check`：通过。
- `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 警告）。
- 全部 7 个规则脚本（含新增 `hub.sh`）：全部通过。
- `ui`：`npm run --silent check`（0 errors, 0 warnings）、`npm test --silent`（5 文件 29/29 通过，覆盖 8 种语言的新代码词条）。
- 非 TCP Rust 测试：`retain` 4/4、`app` 42/42、`liveness` 32/32 全过；含本地 TCP/wiremock 的测试待批次末尾由外部全量 `scripts/build/check.sh` 统一验证。

## 留下的，要负责人的

- 后台任务「一次一件」许可、`job.again` 及 `ai/write.rs`/`out/city.rs` 的 `spawn.sh`/`bounded.sh` 欠账清零归 `P1-21`。
- 外壳侧 `installer`、`Ready` 的锁归 `P1-23`。
