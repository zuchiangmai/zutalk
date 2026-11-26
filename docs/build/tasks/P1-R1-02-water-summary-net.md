# P1-R1-02 修正 `scripts/build/water.py` 的网络活统计与同拍水位落后

## 目标

修掉 P1 第 1 轮审查的**必须改 #2**（`docs/build/reviews/P1-r1.md`）：

1. 修正 `scripts/build/water.py` 的 `net_jobs` 判定，使其能真正检出所有面向我们服务器/外部网络的任务与活，而不是因为前缀不匹配永远输出 `0`。
2. 顺带修掉同文件里的两处小毛病：移除未使用的 `BEAT_STALLED_MS`，以及按同一采样拍计算 `Draft` 相对 `Final(0)` 的落后（不受 `water.mark` 行写出顺序影响）。
3. 用测试或真实样本日志验证：不含网络活的日志输出 `net_jobs=0`；含 `net.signin` / `net.create` / `net.claim` / `net.share` / `slot` 的日志准确报出 `net_jobs > 0` 并列出种类。

## 背景

- `scripts/build/water.py:48` 当前写着：
  ```python
  elif ev == "job.start" and str(r.get("kind", "")).startswith("net."): net_jobs.append(r)
  ```
- 但查看 `core/src/app/work.rs` 与 `docs/EVENTS.md:133`：
  - `task.start` 的 `task` 字段才是 `net.signin`、`net.create`、`net.claim`、`net.calendar`、`net.share`、`net.unshare`（以及直播房间的 `slot`、交城的 `job.file`）；
  - `job.start` 的 `kind` 字段则是 `"signin"`、`"create"`、`"claim"`、`"calendar"`、`"share"`、`"unshare"`、`"slot"`、`"file"`，没有任何一个以 `"net."` 开头。
- 这导致 `P1-26-close.md` 出口 E9 清单里依赖的 `net_jobs=0` 永远打印 `0`，即使程序真的发了网络活也查不出来。
- 此外，`core/src/app/recording.rs:292-301` 写出 `water.mark` 的顺序是 `Capture, Tape, Draft, Frame, Sent(0), Heard(0), Final(0)`；`water.py:30-36` 读到 `Draft` 时 `Final(0)` 还是上一秒的值。

## 做法

1. **`scripts/build/water.py`**：
   - 将网络活的定义写成显式常量集合（带注释说明依据 `docs/EVENTS.md`）：
     ```python
     # Network-facing job kinds in `job.start` (docs/EVENTS.md): none should run in an unsigned-in E9 recording.
     NET_JOB_KINDS = frozenset({"signin", "create", "claim", "calendar", "share", "unshare", "slot", "file"})
     ```
     并在扫描日志时检查 `ev == "job.start" and r.get("kind") in NET_JOB_KINDS`（同时兼容检查 `ev == "task.start" and str(r.get("task", "")).startswith("net.")` 或统一以 `job.start` 的 `NET_JOB_KINDS` 为准并打印命中的 `kind` 列表，确保不重复计数）。
   - 计算 `water.mark`（`snap == 0`）的落后时：按同一批采样（例如每遇到一次 `Capture` 结算上一拍，循环结束结算最后一拍）先收集整拍 `at` 再对所有格算 `max(0, tick_at[up_key] - tick_at[key])`，这样 `Draft` 比较的是**同一拍**的 `Final(0)`。
   - 删除未使用的 `BEAT_STALLED_MS` 常量，或若无用则不留死常量。
   - 保持脚本简短、纯 Python 标准库、零第三方依赖。
2. **确定性验证（含正反两例）**：
   - 构造两份临时 JSONL 样本（或直接加一个轻量的验证命令在报告里给出输入与输出，或在 `scripts/build/` 用 `python3 -c` 断言）：
     - 负例（未登录录音日志）：`net_jobs=0`，且 `Draft` 在首拍 `Final(0)=1000, Draft=0` 时准确报告 `max_lag=1000`（修前为 `0`）；
     - 正例（含 `job.start` `kind="claim"` 与 `kind="share"`）：`net_jobs=2`（修前错报 `0`）。

## 显式与可观测

- **规则与常量**：网络活白名单 `NET_JOB_KINDS` 作为脚本顶部具名常量，注释指向 `docs/EVENTS.md`，禁止魔法前缀猜测。
- **数与单位**：保留 `CAPTION_LAG_P99_LIMIT_MS = 1000`、`WATER_MARK_EVERY_MS = 1000`，清除未使用的死常量。
- **可观测性**：当 `net_jobs > 0` 时一并打印具体 `kind`（如 `net_jobs=2 (claim, share)`），方便 E9 核对时一眼定位哪个网络活被触发。
- **不改事件词典与 Rust 源码**：本件只修观测分析脚本 `scripts/build/water.py`（及如有需要同步更新 `P1-26-close.md` 中的输出说明）。

## 先写的测试

修改 `scripts/build/water.py` 之前，先用含 `job.start(kind="claim")` 和单拍 `Draft(0)/Final(0)=1000` 的临时 JSONL 跑现行 `python3 scripts/build/water.py`，记录其错报 `net_jobs=0` 与 `water Draft: ... max_lag=0` 的失败证据；修改后再跑同一输入，确认输出 `net_jobs=1` 与 `water Draft: ... max_lag=1000`。

## 验收

- `scripts/build/water.py` 正反两例验证通过，且在无 `net` 活的台架 `debug` 日志上输出清晰无误。
- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（9 个规则脚本、Rust 测试、UI 测试均不受影响）。
- 报告中贴出改前失败与改后通过的真实命令输出。

## 不做

- 不修改 `core/`、`app/`、`server/`、`log/`、`ui/`、`docs/EVENTS.md`。
- 不引入 Python 第三方依赖。
