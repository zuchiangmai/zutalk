状态：完成

## 改了什么、动了哪些文件

- **命令与协议**：
  - `core/src/app/proto.rs`：新增无参命令 `Command::Diagnose`（`kind() == "Diagnose"`，`needs_person() == false`）。
  - `core/bindings/Command.ts`：由 `cargo test -p zutalk-core export_bindings` 自动重新生成。
  - `core/src/app/hub.rs`：`Boot::Ready`（及非 `Starting` 阶段）下收到 `Command::Diagnose` 即派给 `Work::diagnose`，不看是否登录或是否在录音；收到 `Msg::Diagnosed` 后说 `diagnose.done`；失败经既有 `Msg::DeskFailed("diagnose")` 路径说 `diagnose`。
- **desk 自检包实现**：
  - `core/src/app/core.rs`：将 `water_reg.read()` 传给 `Work::new`。
  - `core/src/app/work.rs`：
    - 新增常量 `DIAGNOSE_KEPT_DIRS = 3`、`DIAGNOSE_LOG_FILES = 2` 与公开结构体 `Facts`（字段仅含数、布尔和静态/经 `zutalk_log::is_plain` 过滤的代码；非法语言代码替换为 `"!"`）。
    - 新增 `Job::Diagnose`（`kind = "diagnose"`）与 `run_diagnose`：按字段构造 `Facts` 写入 `about.json`，写出 `water.json`、`stats.json`、`tasks.json`；调用 `zutalk_log::flush(LOG_FLUSH_WAIT_MS)` 后按修改时间复制 `Config::logs()` 下最新的最多 2 个 `*.jsonl` 到 `logs/`（目录不存在则跳过）。
    - 全包先写进 `<数据目录>/diagnose/<墙上时钟毫秒>.tmp`，写完后原子改名，清理旧包仅留最新的 3 个目录，并发出 `info!("diagnose.wrote", files, bytes)`；任一步失败清理 `.tmp` 并返回 `"diagnose"`（`job.done` 为 `warn` 级）。
- **界面与多语言词典**：
  - `ui/dev/mock.ts`：`handlers` 新增 `Diagnose`（触发 `diagnose.done` 提示）。
  - `ui/lang/{de,en,es,fr,ja,ko,th,zh-Hans}.json`：八语补齐 `code.diagnose` 与 `code.diagnose.done`。
- **事件文档**：
  - `docs/EVENTS.md`：事件表新增 `diagnose.wrote`，`job.done` 说明补充 `diagnose`，文末新增「自检包里有什么、怎么看」一节。
- **测试**：
  - `core/tests/bench/mod.rs`：`BenchOptions` 增加可控假设备列表 `devices`（默认 `"BenchFakeMicDevice77"`）、`Bench::wait_notice_count`，并新增与 `check_log_lines` 共用同一查漏规则的 `check_bundle_dir`。
  - `core/tests/liveness.rs`：新增 5 条自检包测试。
  - `core/tests/app.rs`：更新 `commands_that_need_a_person_are_listed_in_one_table`，并新增第 6 条登录态 DID 隔离测试。

## 证据

### 新增与更新的测试（全绿）

1. `core/tests/liveness.rs::a_diagnosis_says_how_things_are`
2. `core/tests/liveness.rs::a_diagnosis_holds_nothing_that_was_said_and_no_secret`
3. `core/tests/liveness.rs::what_goes_in_the_bundle_is_written_down_field_by_field`
4. `core/tests/liveness.rs::only_the_newest_three_are_kept`
5. `core/tests/liveness.rs::a_diagnosis_that_cannot_be_written_says_so`
6. `core/tests/app.rs::a_diagnosis_says_who_signs_and_never_which_did`
7. `core/tests/app.rs::commands_that_need_a_person_are_listed_in_one_table`

### 测试第 1 条产出的自检包实录

目录树与文件大小（`diagnose/1795583318192/`，台架使用内存捕获，故无 `logs/` 子目录；测试第 2 条验证了真实文件日志下的 `logs/*.jsonl` 复制）：

```text
1795583318192/
  about.json  (295 bytes)
  stats.json  (3498 bytes)
  tasks.json  (931 bytes)
  water.json  (1501 bytes)
```

`about.json` 全文：

```json
{
  "version": "0.9.0",
  "os": "macos",
  "arch": "aarch64",
  "level": "info",
  "up_ms": 266,
  "who": "local",
  "spoken": [
    "en",
    "zh"
  ],
  "shown": [
    "en",
    "zh"
  ],
  "sound": "mic",
  "captions": false,
  "keys": {
    "openrouter": "Nowhere",
    "soniox": "Own"
  }
}
```

`water.json` 中 `Sent(0)` 一格：

```json
{
  "at": 29900,
  "ended": null,
  "moved_ms": 30000,
  "stage": {
    "Sent": 0
  },
  "why": ""
}
```

### `about.json` 字段来源核对

`git grep -n "settings.json\|State" core/src/app/work.rs` 输出：

```text
core/src/app/work.rs:9:use crate::app::proto::{Change, From, Open, Room, Row, Setup, Sound, State, Writing};
core/src/app/work.rs:58:/// serialized from `State` or copied from `settings.json`.
core/src/app/work.rs:1173:                .and_then(|()| fs::write(config.data.join("settings.json"), json))
core/src/app/work.rs:1458:    // Step 1: settings.json
core/src/app/work.rs:1460:    let setup = match fs::read(env.config.data.join("settings.json")) {
core/src/app/work.rs:1644:        let mut tmp = State { library, ..State::default() };
```

`run_diagnose` 既不读 `settings.json`，也不序列化 `State`；`Facts` 的 11 项逐一取自：
- `version`：`env!("CARGO_PKG_VERSION")`
- `os` / `arch`：`std::env::consts::{OS, ARCH}`
- `level`：`env.config.log.code()`
- `up_ms`：`env.started_at.elapsed().as_millis() as u64`
- `who`：`env.identity` 内 `id.who.code()`（仅 `nobody` / `local` / `person`）
- `spoken` / `shown`：hub 传入的语言代码切片，逐项经 `zutalk_log::is_plain` 过滤，不合规换 `"!"`
- `sound`：`sound_code(sound)`（`mic` / `system` / `both`）
- `captions`：hub 传入的布尔值 `state.captions`
- `keys`：`Service::ALL` 查 `id.keys.from(*s)`（仅 `Nowhere` / `Own` / `Granted`）

### 门禁结果

- `cargo fmt --all`：通过
- `cargo clippy --workspace --all-targets -- -D warnings`：通过
- 规则脚本（`bounded.sh`、`dropped.sh`、`events.sh`、`hub.sh`、`layers.sh`、`numbers.sh`、`spawn.sh`）：全部通过
- `ui`：`svelte-check` 0 errors / 0 warnings；`vitest` 5/5 文件、29/29 用例通过（含八语 `words.test.ts`）

## 留待后续 / 需要负责人

- 设置里的「导出诊断」按钮、打 zip、选保存路径与「在文件夹里显示」留给 P3。
- 完整沙箱外全量 `scripts/build/check.sh` 将在本批最后由协调器统一跑完。
