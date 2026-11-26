状态：完成

# P1-26 收尾：出口逐条核对，文档并回，给负责人的清单：报告

## 改了什么

1. **挂起清零与规则脚本 `scripts/build/rules/pending.sh`**：新建可执行脚本守护仓库中不再残留 `ignore = "P1-`（允许指向 `P2` 及以后各期的标记测试）；当前门禁规则脚本共 9 个，与 `docs/build/README.md` 一一对应。
2. **真机 E9 日志汇总脚本 `scripts/build/water.py`**：60 行纯 Python 标准库脚本，读 `<数据目录>/logs/zutalk*.jsonl` 或自检包 `logs/`，汇总 `seq` 缺号与 `log.lost`、按格最大落后与缺秒（`debug` 级 `water.mark`）、每条 `judge.verdict` / `stall.snapshot` / `part.gave_up` / `shell.main_thread.stalled` / `ui.frame.gap` / `hub.turn.slow` / `identity.who` / `recording.start`、`net.*` 活数量，以及最后一份每分钟汇总里的 `caption.lag_ms` `p50`/`p99`。
3. **文档并回**：
   - `docs/DESIGN.md`：更新 §2 现场路径、§3 概念表、§4 模块表（57 处路径全存在）、新增 §5.13「运行时（P1）」、§5.14「显式与可观测」、§5.15「不登录也能录」，同步 §5.7 / §5.12 / §7 / §9。
   - `docs/PLAN.md`：保留全部原文；§2.7 逐条标明 P1 完成状态与 P4 领回；§4.2 加「P1 之后」一栏；§4.5 补实际做法与差异；§5.1 注明 `zu/` 为本机身份开的窄口；§5.3 注明实际增减依赖；§7 补齐 I1/I2/I5/I6/I7 实际量法并新增 I9；§8 更新 P1（E9 待负责人）与 P4（领回本机记录）。
   - `docs/LESSONS.md`：新增「P1 里学到的（2026-11）」五条硬经验。
   - `docs/build/README.md`：列出全部 9 个规则脚本并写明 `debt` / `later` 精确匹配规矩。

文件：`scripts/build/rules/pending.sh`、`scripts/build/water.py`、`docs/DESIGN.md`、`docs/PLAN.md`、`docs/LESSONS.md`、`docs/build/README.md`、本报告。

---

## 1. 挂起清零与 `pending.sh` 证明

`grep -rn 'ignore = "P1-' core app server zu ui` 返回空（仓库中仅保留 1 条 `#[ignore = "P2：状态瘦身"]` 的 `i8_what_one_change_sends_does_not_grow_with_the_library` 与 4 条真机手工 `audio.rs` 测试）。

`pending.sh` 临时违例证明（试完已还原）：

```text
=== pending.sh PROOF ===
core/tests/recording.rs:138:#[ignore = "P1-99：试"]
  pending P1 ignored tests are not allowed after P1 closes
exit=1
pending.sh OK after revert
```

---

## 2. 出口逐条核对（E1–E11）

| # | 出口 | 成立 | 证据（测试、规则脚本、数字） |
|---|---|---|---|
| **E1** | I1 成立 | ✔ | `i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`（8 虚拟小时，h1–h8 全部 p50/p99/max = `321 ms` ≤ 1000 ms，漂移 `0 ms`，0 丢词，最大帧 `6125 B` ≤ 8192 B）；`i1_three_languages_fit_a_frame`（30 虚拟分钟三语，p99 = `321 ms`，最大帧 `8192 B` ≤ 8192 B） |
| **E2** | I2 成立 | ✔ | `i2_a_tape_that_stands_still_holds_no_caption_back`、`i2_a_job_that_never_ends_holds_up_neither_captions_nor_commands`、`i2_background_work_runs_beside_a_recording` 通过；`scripts/build/rules/hub.sh` 通过 |
| **E3** | I4 成立 | ✔ | `clippy.toml` 禁用 `tokio::sync::mpsc::unbounded_channel` 与 `std::sync::mpsc::channel`；`scripts/build/rules/bounded.sh`、`shell.sh` 通过 |
| **E4** | I5 成立 | ✔ | `app/tests/pump.rs`（含 `every_interleaving_ends_with_each_window_holding_the_latest` 穷举交错）全过；`scripts/build/rules/shell.sh` 通过 |
| **E5** | I6 成立 | ✔ | `i6_a_lane_that_dies_comes_back_and_marks_what_it_missed`、`i6_a_tape_writer_that_dies_comes_back_and_the_hole_is_silence`、`core/tests/supervise.rs::a_part_that_panics_is_opened_again`、`core/tests/keep.rs` 全部 5 条、`app/tests/pump.rs::a_reload_while_frames_are_coming_loses_nothing_and_the_old_channel_goes_quiet` 通过 |
| **E6** | I7 在核心成立 | ✔ | `i7_a_service_that_takes_audio_and_says_nothing_is_left_for_a_new_connection`、`i7_a_capture_that_stops_delivering_ends_the_recording_out_loud`、`i7_offline_is_said_and_captions_stop_trying_until_asked`、`core/tests/water.rs` 全部 10 条、`core/tests/live.rs::a_lease_that_never_comes_is_given_up_on` 通过 |
| **E7** | 无 P1 挂起测试 | ✔ | `scripts/build/rules/pending.sh` 通过（0 匹配） |
| **E8** | 真机能录 | ✔ | `core/src/capture/clock.rs::wall_clock_policy` 已按 `DECISIONS.md` 第 3 条实现，`core/tests/clock.rs` 全部 6 条表格测试通过 |
| **E9** | 真机 1 小时不卡（不登录） | 待负责人 | `cargo build -p zutalk` 本机编译通过（未启动应用）；见下文「给负责人的清单」与 `scripts/build/water.py` |
| **E10** | I9 显式与可观测 | ✔ | 完整逐项证据见 `docs/build/reports/P1-25-audit.md`；本件重跑两条隐私测试（`nothing_said_or_secret_reaches_the_log`、`a_diagnosis_holds_nothing_that_was_said_and_no_secret`）与五个显式规则脚本（`events.sh`、`numbers.sh`、`bounded.sh`、`spawn.sh`、`dropped.sh`）均通过，无走样 |
| **E11** | 不登录也能录 | ✔ | `core/tests/app.rs` 5 条 E11 测试、`zu/tests/` 本机身份测试、`server` 拒绝本机作者测试均通过；另三项专项核对见下方 |

**E11 专项核对**：

1. **台架收尾断言 `state.me.is_none()`**：`core/tests/bench/mod.rs:848`（`Bench::finish`）在 `signed_in == false`（默认）时强制断言 `state.me.is_none()`，因此 E1、E2、E5、E6 全部在未登录核心上量出。
2. **`git grep -n '"sign_in.first"' core/src` 全部 5 处**（仅余查表 1 处、3 个需登录命令相关处与 `Chain::claim` 1 处）：
   - `core/src/app/hub.rs:525`：`Command::needs_person()` 统一分派拦截（1 处）
   - `core/src/app/work.rs:1066`、`1261`、`1281`：`File` / `Share` / `Unshare` 执行前二次确认 `ours` 存在（3 处）
   - `core/src/id/keys.rs:521`：`Chain::claim` 缺 `issuer` 返回 `End::Fatal("sign_in.first")`（1 处）
3. **`zu/tests` 从 `P1.base`（`7c97798`）至今未改动任何既有测试**：`git diff --stat $(cat docs/build/phases/P1.base) -- zu/tests` 为 `3 files changed, 76 insertions(+)`，删改行数为 **0**。

---

## 3. I1 测量对比（改之前 P1-08 vs 改之后 P1-26）

`i1_eight_hours_in_the_caption_is_as_quick_as_in_the_first`（8 虚拟小时，双语，57 487 词）：

| 小时段 | 词数 | P1-08 改前 p50 / p99 / max | P1-26 改后 p50 / p99 / max |
|---|---:|---|---|
| 第 1 小时 | 7 241 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 2 小时 | 7 183 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 3 小时 | 7 199 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 4 小时 | 7 213 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 5 小时 | 7 173 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 6 小时 | 7 172 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 7 小时 | 7 158 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| 第 8 小时 | 7 148 | 320 / 320 / 320 ms | 321 / 321 / 321 ms |
| **总计 / 边界** | **57 487（0 丢词）** | 最大帧 **15 144 B**（失败 > 8192 B）；真实耗时 64.82 s | 最大帧 **6 125 B**（≤ 8192 B）；`bound_most = 7 506 B`；`caption.lag_ms` p50/p99 = 500/500 ms（`most = 320 ms`）；`judge.verdict = 0`；`Tree::running()` h1=9 / h8=9；真实耗时 164.37 s（含完整录音树、1 Hz 水位、统计与事件捕获） |

---

## 4. 确定性与门禁耗时

1. **`i1_three_languages_fit_a_frame` 连跑 5 次**（全部指标每次完全一致）：
   ```text
   run 1: i1_three_languages: words=3921 missing=0 p50=321ms p99=321ms max=321ms max_frame=8192B bound_most=10770B cut=607 fit_events=48 events=507 wall=13.83s
   run 2: i1_three_languages: words=3921 missing=0 p50=321ms p99=321ms max=321ms max_frame=8192B bound_most=10770B cut=607 fit_events=48 events=507 wall=19.54s
   run 3: i1_three_languages: words=3921 missing=0 p50=321ms p99=321ms max=321ms max_frame=8192B bound_most=10770B cut=607 fit_events=48 events=507 wall=16.02s
   run 4: i1_three_languages: words=3921 missing=0 p50=321ms p99=321ms max=321ms max_frame=8192B bound_most=10770B cut=607 fit_events=48 events=507 wall=11.87s
   run 5: i1_three_languages: words=3921 missing=0 p50=321ms p99=321ms max=321ms max_frame=8192B bound_most=10770B cut=607 fit_events=48 events=507 wall=10.54s
   ```
2. **P1-01 采集压力测试**（`a_source_that_dies_is_reported_padded_and_opened_again`，12 路并行 × 100 次，30 个空转进程）：
   ```text
   audio stress total=1200 failures=0
   ```
3. **沙箱内已验**：`cargo fmt --all -- --check`、`RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`、全部 9 个规则脚本（含 `pending.sh`）、`zutalk-log` / `zu` / `zutalk` / `zutalk-server --test log` / `zutalk-core` 全部非 TCP 集成测试、`ui` (`npm run check && npm test`)、`cargo build -p zutalk` 全绿。
4. **`scripts/build/check.sh` 沙箱外连跑 3 次与门禁耗时**（P1 之前约 40 秒）：
   - Run 1：全绿（全部 9 个规则脚本、`cargo test`：`passed 651, failed 0, ignored 7`、`ui`：`0 errors, 0 warnings`、`34 passed`），耗时 `320 s`（首轮含增量重编）。
   - Run 2：全绿（全部 9 个规则脚本、`cargo test`：`passed 651, failed 0, ignored 7`、`ui`：`0 errors, 0 warnings`、`34 passed`），耗时 `178 s`（热缓存稳态耗时，主要花在 8 虚拟小时 `i1_eight_hours_…` 压测）。
   - Run 3：全绿（全部 9 个规则脚本、`cargo test`：`passed 651, failed 0, ignored 7`、`ui`：`0 errors, 0 warnings`、`34 passed`），耗时 `205 s`。
   - 3 次连续运行 0 失败、通过数完全一致（`651 Rust + 34 UI`）。

---

## 5. 与 `PLAN.md` 字面不同之处汇总

均已并回 `docs/DESIGN.md` §5.13 及 `docs/PLAN.md` §4.5 / §5.1 / §5.3：

1. 写盘（tape / draft）采用 `rt::keep`（async 任务 + 每批一次 `Tree::blocking`），而非裸 OS 线程（在 tokio 暂停时钟下确定性等待落盘，不会因调度跑满队列）。
2. 外壳推送采用单任务独占状态机 `Fan`（零共享锁），未引 `arc-swap` 与 `loom`（由 `app/tests/pump.rs` 穷举交错覆盖）。
3. 未引 `turmoil`（`Dial` / `Keys` 替身在暂停时钟下确定模拟不回话、断网与取钥匙超时）、未引 `hdrhistogram`（固定 13 桶无分配）、未引 `tokio-metrics`（`Tree::running()` 与 `rt::stats`）、未引 `statig`（10 个小状态机手写枚举 + `step`）。
4. `rt::queue` 显式传入 `&Stats`，`Full` 为 `Wait`/`Drop`/`Reject`/`End`（`Coalesce` 由寄存器承担）。
5. 日志独立为 `log/`（`zutalk-log`）crate（服务器共用），以宏写入的 callsite 记号 `zutalk:<代码>` 过滤外部库事件，不依赖模块路径前缀。
6. `stall.snapshot` 与每分钟汇总按平铺 JSONL 多行写出（分别以递增 `snap` 号与 `stats.summary` 串联），不打破字段平铺的类型防线。
7. 界面画帧序号与帧间隔合并为 1 Hz 单命令 `drawn(seq, gap_ms, gaps)`；取消独立 `ZUTALK_WATER_LOG`，改由 `ZUTALK_LOG=debug` 输出每秒 `water.mark`。
8. 规则脚本同时覆盖 `app/src`、`server/src`、`log/src`（服务器 3 处 `spawn` 与 1 处房间 `broadcast` 记在 `later` 表留给 P4/P5）。
9. `zu/` 内核的 `Path::new` 增开窄口接受 Ed25519 `did:key`（`zu::did::local` / `local_key`）作本机记录作者，`is_did` 不变，任何主机照旧拒绝。

---

## 6. `docs/DESIGN.md` §4 模块表路径存在性验证

检查命令与输出（共 57 个路径，全部存在，0 缺失）：

```text
$ python3 -c '
import os, re, sys
txt = open("docs/DESIGN.md").read()
m = re.search(r"## 4\. 模块(.*?)## 5\.", txt, re.S)
paths = []
for line in m.group(1).splitlines():
    if not line.startswith("| `"): continue
    cells = line.split("|")
    mod = cells[1].strip().strip("`")
    for p in re.findall(r"`([^`]+)`", cells[2]):
        paths.append(p if mod in ("log", "shell", "ui", "server") else f"core/src/{p}")
missing = [p for p in paths if not os.path.exists(p)]
print(f"checked={len(paths)} missing={len(missing)}")
sys.exit(1 if missing else 0)
'
checked=57 missing=0
```

---

## 7. `scripts/build/water.py` 在台架真实 `debug` 故障日志上的输出

场景：台架录 65 虚拟秒（未登录本机身份 + 自有钥匙、`Level::Debug` 写盘、`Fault::Silent { connections: 1 }` 注入首连静默故障，10 秒触发 `Heard(0)` `Stalled(silent)` 与 `snap=1` 快照后踢线重连恢复，60 秒处写出每分钟汇总）：

```text
$ python3 scripts/build/water.py /tmp/zutalk-water-demo
files=1 seq_gaps=0 log.lost=0
water Capture: samples=65 max_lag=0 missing_s=0
water Draft: samples=65 max_lag=0 missing_s=0
water Final(0): samples=65 max_lag=25020 missing_s=0
water Frame: samples=65 max_lag=0 missing_s=0
water Heard(0): samples=65 max_lag=9900 missing_s=0
water Sent(0): samples=65 max_lag=0 missing_s=0
water Tape: samples=65 max_lag=0 missing_s=0
verdict t=1795587243010 Heard(0) Live->Slow why= action=none
verdict t=1795587243060 Heard(0) Slow->Stalled why=silent action=reopen
verdict t=1795587243070 Heard(0) Stalled->Live why= action=none
snapshot snap=1 t=1795587243060 Heard(0) Stalled why=silent idle_ms=10000 lines=61
who t=1795587242947 nobody->local why=boot made=False
recording t=1795587242949 rec=1a21156fbc48cba56b7 captions=live lanes=1 source=mic
net_jobs=0
caption.lag_ms: n=103 p50=500ms p99=500ms most=320ms (OK <= 1000ms)
```

---

## 给负责人的清单

### A. 真机录 1 小时（出口 E9，全程不登录、不需要我们的服务器）

- [ ] **构建并带着 `debug` 日志启动**（以下命令 `cargo build -p zutalk` 已在本机验证编过，未启动应用）：
  ```bash
  cd /Users/kant/Zuzalu/zutalk
  (cd ui && npm run build)
  cargo build -p zutalk
  ZUTALK_LOG=debug ./target/debug/zutalk
  ```
  - 日志目录：macOS 在 `~/Library/Application Support/xyz.voice.zutalk/logs/`；Windows 在 `%APPDATA%\xyz.voice.zutalk\logs\`；Linux 在 `~/.local/share/xyz.voice.zutalk/logs/`。
  - **第一眼核对（P1-03）**：打开 `logs/zutalk.jsonl`，确认第一行是 `app.start`（带 `version`、`os`、`arch`、`level: "debug"`）。
- [ ] **第一场（1 小时，填自己的 Soniox 钥匙，不登录）**：
  1. 顶栏进「设置」→「出字」→「Soniox 密钥」栏填入自己的 Soniox API Key 并保存（不要点登录）。
  2. 选「麦克风 + 电脑声音」及要显示的字幕语言，开始录音 1 小时。
  3. 录音途中依次执行：
     - 插拔一次耳机（观察继续收音与字幕恢复）；
     - 断开网络约 30 秒以上再连上（观察进入 `Stalled`/`Dead` 后点重连恢复）；
     - 合盖约 10 秒再打开（在 60 秒以内应补静音继续录；若要顺便测超 60 秒结束，可另测一次）；
     - 反复开关字幕窗；
     - 若用开发模式（`ZUTALK_LOG=debug cargo tauri dev`），重新加载页面一次确认字幕立刻接上。
  4. 结束前在 WebView 开发者工具控制台发一次自检命令：
     ```js
     await window.__TAURI_INTERNALS__.invoke('dispatch', { command: { do: 'Diagnose' } })
     ```
     （自检包在 `<数据目录>/diagnose/<毫秒>/`。）
- [ ] **第二场（10 分钟，不填钥匙、只录音，不登录）**：
  1. 进「设置」→「出字」→ 点 Soniox 密钥旁的「移除」清掉钥匙。
  2. 开始录音 10 分钟后正常停止，确认界面显示「只录音」且全程无报错提示。
- [ ] **跑 `scripts/build/water.py` 核对通过标准**：
  ```bash
  python3 scripts/build/water.py "<数据目录>/logs"
  # 或直接对着自检包里的 logs/ 跑：
  python3 scripts/build/water.py "<数据目录>/diagnose/<毫秒>/logs"
  ```
  - **活性标准**：除故意断网与合盖时段外，没有 `Stalled` / `Dead`；断网期间 `Capture`、`Tape` 两格一直在走；每次停滞都能从 `verdict` / `snapshot` 说出原因；无 `shell.main_thread.stalled`；`seq_gaps=0` 且 `log.lost=0`；`caption.lag_ms` 的 `p99 <= 1000 ms`。
  - **不登录标准**：`who` 的 `to` 一直是 `local`（首次启动 `made=True`，之后 `made=False`）；`net_jobs=0`（未找过我们的服务器）；填钥匙场 `recording` 的 `captions=live`，散场后日志里有 `job.done`（`kind="write"`, `code="ok"`）。
  - **无钥匙场标准**：`recording` 的 `captions=captions.no_key`、`lanes=0`；该场 `recording.start` 到 `recording.end` 之间无 `lane.*`、`part.state`（lane）、`judge.verdict`、`stall.snapshot`、`notice.said`；散场后有 `write.skipped`；`Capture`、`Tape` 两格持续前进。
  - **交回**：交回 `<数据目录>/logs/` 或自检包文件夹。虽按构造不含内容与密钥（`P1.md` §3.13.14），发出去前仍请负责人自己扫一眼确认。
  - 优先 macOS，再 Windows、Linux。

### B. 三系统音频手工清单（`P1.md` §4 第 2 条，P1-01）

- [ ] 在 macOS、Windows、Linux 逐项跑 `core/tests/audio.rs` 文件开头的全部手工清单项（含第 5 条「合盖」：≤ 60 s 补静音、> 60 s 结束录音）。

### C. 真机外壳与主线程检查（P1-23、P1-24）

- [ ] **开发模式重载 10 次**（P1-23）：录音中连按 10 次刷新页面，字幕与状态立刻接上，日志里出现 10 条 `shell.join`（`replaced: true`）。
- [ ] **连续开关字幕窗 20 次**（P1-23）：录音中快速开关字幕窗 20 次不卡顿，日志里出现 20 条 `shell.captions`。
- [ ] **人为堵住主线程数秒**（P1-24）：录音中打开系统文件框或用调试器暂停主线程 > 2 秒后放开，日志里出现 1 条 `shell.main_thread.stalled` 与 1 条 `shell.main_thread.back`。
- [ ] **切后台再切回前台**（P1-24）：录音中切后台再切回，确认没有多出 `ui.frame.gap`；正常录 10 分钟记录 `ui.frame.gap` 条数。

### D. 要确认的数字（`P1.md` §4 第 3 条 + P1-04 12 个 `Origin to be traced` 常量）

- [ ] `wall_clock_policy`：`JITTER_MS = 300`、`ASLEEP_MS = 60_000`（`core/src/capture/clock.rs:39,42`；后果：双源同录且电脑无声时混音最多晚约 0.3–0.45 s，见 P1-01）。
- [ ] 水位预算表 `BUDGETS`（`core/src/rt/water.rs:307`）：`Capture`(慢 1 s / 停 10 s 结束)、`Tape`(落后 2 s / 停 10 s)、`Sent`(落后 2 s / 停 20 s)、`Heard`(落后 3 s / 停 10 s 重开)、`Final`(落后 30 s)、`Draft`(停 30 s)、`Shell`(慢 2 s / 停 5 s)、`Screen`(慢 5 s)。
- [ ] 重启上限：`RESTARTS_MOST_TIMES = 8`、`RESTARTS_WINDOW_MS = 60_000`、`RESTARTS_PATIENCE_MS = 120_000`（`core/src/rt/supervise.rs:63,65,68`）。
- [ ] 队列上限与后台并发：`TAPE_QUEUE_BLOCKS = 600`（60 s，满则结束）、`DRAFT_QUEUE_LINES = 256`（`core/src/app/recording.rs:50,53`）、`HUB_MAIL_MSGS = 256`（`core/src/app/hub.rs:36`）、`DESK_QUEUE_JOBS = 64`（`core/src/app/work.rs:46`）、`BACKGROUND_AT_ONCE_JOBS = 1`（`core/src/app/work.rs`，P1-21）、HTTP 超时 `CONNECT_TIMEOUT_MS = 10_000` / `READ_TIMEOUT_MS = 30_000`（`core/src/app/core.rs:26,29`，P1-18）。
- [ ] 字幕帧上限：`MAX_COLUMNS = 4`、`KEEP_LINES = 6`、`MAX_BYTES = 8 * 1024`（`core/src/live/frame.rs:14,17,20`）。
- [ ] 日志与自检包：`LOG_FILE_BYTES = 8 MiB`、`LOG_FILES_KEPT = 5`、`LOG_QUEUE_LINES = 4096`（`log/src/sink.rs:21,23,26`；注：刷盘用 `flush` 非 `fsync`，见 P1-03）；`DIAGNOSE_KEPT_DIRS = 3`、`DIAGNOSE_LOG_FILES = 2`（`core/src/app/work.rs:52,54`）。
- [ ] 活性监测阈值：`BEAT_EVERY_MS = 1_000`、`BEAT_STALLED_MS = 2_000`（`app/src/beat.rs:16,18`）；`FRAME_GAP_MS = 250`（`ui/src/lib/vitals.ts:5`）；`HUB_TURN_BUDGET_US = 1_000`（`core/src/app/hub.rs:38`）。
- [ ] P1-04 标为 `Origin to be traced` 的 12 个既有常量：`LOOK_MS = 21_600_000`（`app/src/updates.rs:21`）、`SLOT_MS = 250`（`core/src/app/work.rs:43`）、`ZERO_RUN_MS = 10_000`（`core/src/capture/audio.rs:33`）、`CREATE_WAIT_MS = 30_000` / `SIGN_IN_POLL_MS = 1_000` / `SIGN_IN_WAIT_MS = 300_000`（`core/src/id/keys.rs:284,287,289`）、`Backoff::FIRST_MS = 250` / `HEALTHY_MS = 30_000`（`core/src/rt/supervise.rs:46,50`）、`UPSTREAM_WAIT_MS = 10_000` / `OPENROUTER_KEY_LIFE_MS = 3_600_000`（`server/src/ledger.rs:21,23`）、`SWEEP_EVERY_MS = 60_000`（`server/src/lib.rs:209`）、`DIRECTORY_WAIT_MS = 10_000`（`server/src/main.rs:12`）。

### E. 架构与政策决定（`P1.md` §4 第 4–8 条 + 各任务报告）

- [ ] **过目与 `PLAN.md` 的偏离**（`P1.md` §4 第 5、6 条）：不用 `loom`、`turmoil`、`arc-swap`、`hdrhistogram`、`tokio-metrics`、`statig`，以及第 5 节表里全部 9 项。
- [ ] **录音 id 进日志**（`P1.md` §4 第 6 条）：目前临时决定进日志（只含开始时间十六进制 + 8 位随机）；若改主意，移除 `core/src/app/recording.rs` 中 `rec = Id(…)` 字段。
- [ ] **服务器 4 处任务与队列何时收口**（`P1.md` §4 第 6 条，P1-04/P1-05）：当前记在 `spawn.sh`（`server/src/ledger.rs` 2 处 P4、`server/src/lib.rs` 1 处 P5）与 `bounded.sh`（`server/src/room.rs` 1 处 P5）的 `later` 表；是否维持到 P4/P5 再抽共用 `rt` crate 或另行处理。
- [ ] **「不进日志的 id」清单**（`P1.md` §4 第 7 条）：当前仅放行录音 id 与窗口标签；如需允许 DID 前几位等进入日志，需改 `docs/EVENTS.md` 规则及 `Id` 审查口径。
- [ ] **不登录也能录 6 项决定**（`P1.md` §4 第 8 条，`DECISIONS.md` 第 10、11 条，P1-06/P1-07）：
  1. **内核收 `did:key` 作本机作者**：现状在 `zu/src/did.rs`（`local` / `local_key`）与 `zu/src/record.rs`（`Path::new`）开了窄口，`is_did` 不变；请与 zu 规范协调，撤回只需在 P4 领回前把本机书架换身份重签。
  2. **领回放在 P4**：现状 P1 到 P4 之间登录看到本人 DID 书架、登出回到本机书架，互不挪动不丢数据；提前则需在 P2 后插一件任务改 `app/work.rs` 与 `record/shelf.rs`。
  3. **0.8 邀请无感搬运 vs「没登录不发请求」**：现状未动 0.8 邀请（启动发现 `community-invite.json` 且无登录身份时仍自动造 `did:zu` 并连目录/服务器）；若改为等用户点登录才认领，改 `core/src/app/work.rs` 启动分支。
  4. **0.8 资料库没登录不搬**：现状只在有人登录时搬运（避免把 `moved.json` 锁死在临时本机身份）；若改先搬到本机身份再领回，改 `core/src/app/work.rs` 搬运触发条件。
  5. **录音中途填钥匙这一场不出字**：现状开录前由 `lane::captions` 定下，中途 `OwnKey` 下一场生效；若要当场起 lane，改 `core/src/app/recording.rs`（复刻 `Reconnect` 路径）。
  6. **秘密丢失后旧本机书架无人打开**：现状会生成新 `did:key` 并在 `identity.who` 发 `made = true` 让问题可见；扫描认领旧书架留给 P4 领回。
- [ ] **其余跨期与发布项**（来自 `DECISIONS.md` 与各报告）：
  - `DECISIONS.md` 待定项：界面框架（#1 React 19 vs Svelte 5，P3 前定）、标记（#2 笔记 anchor）、只用额度的人怎么精修（#4，P2/P6）、登录核对六位数（#5，zu `login.md`）、交城记录类型 `talk/*` vs `tape/*`（#6，pol v0.3）。
  - `layers.sh` 四处欠账：`record/retain.rs`、`record/migrate.rs`（P2 还）；`out/city.rs`、`out/room.rs`（P5 还）。
  - 后台 `ai/write.rs`、`out/city.rs` 同步写记录调用（P1-21 留给 P2/P6 审查）；P1-22 诊断导出按钮/打 zip/选路径留给 P3；I2 大数据规模（10 万行/175 条）留给 P2/P7；发布 0.8.6 交接版、部署服务器配 `INVITES_URL`/`INVITES_TOKEN` 与更新签名钥匙对留给 P7。
