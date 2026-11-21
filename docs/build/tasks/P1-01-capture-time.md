# P1-01 采集的时间：策略定下，测试不再和线程抢

## 目标

做完以后：

1. `clock::wall_clock_policy` 不再是 `todo!()`，照 `docs/build/DECISIONS.md` 第 3 条实现；真机上能录音。
2. `core/tests/audio.rs` 里采集那一组测试的结果不取决于线程怎么调度。`a_source_that_dies_is_reported_padded_and_opened_again` 不再偶尔失败，门禁可以放心地一遍遍跑。

## 背景

- `core/src/clock.rs`：`wall_clock_policy`（第 37 行）现在是 `todo!()`。
- `core/src/audio.rs`：`Capture::with`（第 429 行起）起泵线程，循环体是「收消息 → `pump.step(now)` → 交块 → 把重开的判断告诉声源线程」；`hold`、`held` 是声源线程。
- `core/tests/audio.rs` 第 531 行起「The capture: threads and notices」一组：`Run` 用一个原子数当时钟，测试线程拨它，泵线程每 20 毫秒真实时间读一次。
- `docs/DESIGN.md` §5.6 写着上一次对这条测试的修正；这一次是它没修到的地方。

**失败的原因（已经查清）。** 环里的声音不带时间。泵把「现在 − 刚排空的时长」当作声音开始的时刻（`Pump::step` 里的 `first`），之后按「现在」补静音。测试却在不知道泵排空了没有的情况下拨钟，于是有三种抢法：

1. 泵排空得晚：测试已经把钟拨过去了，起点就晚了，整条时间线变短。门禁里见到的就是这一种：只出来 15 块，`run.until(17)` 等满 5 秒，`too few blocks came`。
2. 钟拨过去了，`Msg::Feed` 还没到泵：泵先按新的钟补了一段静音，块数变多。
3. 泵读了「现在」，测试又往环里放了声音：多出几十毫秒。

**稳定复现**（先做这一步，写进报告）：在 `Run::start` 给 `Capture::with` 的 `notice` 回调里，`notices.send(notice)` 之后加 `thread::sleep(Duration::from_millis(50))`。`a_source_that_dies_…` 每次都在 `until(17)` 失败。原因是 `Feed` 和 `Notice` 在泵的同一批消息里：泵交出通知以后被拖住，测试线程先把钟拨了 240 毫秒，泵这才第一次排空那 400 毫秒的声音。

生产里没有这个问题：墙上时钟不会跳，这个偏差只有几微秒。所以要改的是测试怎么驱动泵，不是泵的规则。

## 做法

**1. 策略。** `wall_clock_policy(behind_ms)`：`≤ 300` → `Gap::Keep`；`301..=60_000` → `Gap::Pad(behind_ms)`；更大 → `Gap::End`。两个界限写成具名常量（`JITTER_MS`、`ASLEEP_MS`），注释说明出处是临时决定第 3 条。

**2. 泵可以一圈一圈地转。** 把泵线程的循环体变成一个公开的类型，线程只是不停地转它：

```rust
/// The pump's work, one turn at a time. `Capture::with` turns it on a thread of its own every
/// `TICK`; a test turns it by hand, so the clock it moves and the pump that reads it never race.
pub struct Turner { /* the pump, its mailbox, the enders, the sink, the notice, the clock */ }

impl Turner {
    /// Takes the news that has come (waiting up to `wait` for the first), steps the pump at
    /// the clock's time, and delivers what is whole. `false`: it has stopped.
    pub fn turn(&mut self, wait: Duration) -> bool;
    /// How many devices have been handed to the pump so far.
    pub fn attached(&self) -> usize;
}

impl Capture {
    /// The same capture, with no pump thread: whoever holds the `Turner` turns it.
    pub fn by_hand(opens, policy, now_ms, sink, notice) -> (Capture, Turner);
}
```

- `Capture::with` 就是 `by_hand` 加 `thread::spawn(move || while turner.turn(TICK) {})`。行为不许变：消息的次序、停止时交完整的块、`over` 的置位、`Drop` 只等泵线程。
- `by_hand` 得到的 `Capture` 没有线程可等；`Drop` 仍然发 `Stop`，由持有 `Turner` 的人再转一圈把它收掉。
- 名字可以调整，但要有这三样：一圈一圈转、不起泵线程的构造、能知道设备交到泵手里没有。

**3. 测试改成自己转。** `Run` 拿着 `Turner`：

- `play`：每 20 毫秒一步：往环里放声音 → 钟加 20 → 转一圈。钟和泵于是严格交替。
- `notice()`、`opened()`：钟不动，反复转（每圈最多等 1 毫秒），直到要等的东西到了；`WAIT` 只是「声源线程一直不回话」的上限。`opened()` 拿到设备以后，还要转到 `attached()` 加一，才算打开了。
- `until(n)` 不再需要：转完以后块已经在手里，直接断言。
- 停止：丢掉 `Capture`，再转一圈；断言这一圈返回 `false`，之后没有东西再来。
- 测试里的通道用 `std::sync::mpsc::sync_channel`（容量给够，比如 4096），不用 `channel()`：P1-08 会禁掉后者。

`a_device_that_hangs_in_the_system_holds_up_nothing` 要证明的是「停止只等泵线程」，它留在真的泵线程上（`Capture::with`）。它的断言本来就不看时间线（一条通知、至少 5 块、停止不被拖住）；把「停止不超过 1 秒」的上限改成 `WAIT`，它防的是 3600 秒的卡死。

其余几条（`a_source_that_dies_…`、`a_stall_the_pump_hears_…`、`a_source_that_cannot_be_opened_…`、`a_device_that_is_no_longer_the_right_one_…`、`paused_the_clock_stands_still_…`、`a_closed_lid_ends_…`）都改成自己转。断言不许放松；能收紧的收紧（块数现在可以是确切的数）。

**4. 文档。** `docs/DESIGN.md` §5.6 加一段写这次的原因和做法；§7 第 1 条改成「已照临时决定实现，数字等负责人确认」。

## 先写的测试

1. `core/tests/clock.rs`：`the_app_keeps_jitter_pads_a_hole_and_ends_after_a_minute_asleep`。表格：0、300 → `Keep`；301 → `Pad(301)`；60_000 → `Pad(60_000)`；60_001 → `End`。现在会 panic。
2. `core/tests/audio.rs`：`with_the_app_policy_a_quiet_source_holds_the_mix_back_under_half_a_second`。用 `Script`（让它能带策略），两个声源：麦克风每 20 毫秒送声音，电脑声音（`steady = false`）一直不送。跑 10 秒，断言每一块交出的时刻不晚于它结束后 450 毫秒，时间线不断。这个数是 300 毫秒不补静音的代价，要让负责人看得见。
3. 采集那一组，按上面的办法重写。先写 `a_source_that_dies_…`：它用到的 `Capture::by_hand` 现在不存在。
4. 复现那一步不留在仓库里，写进报告：加了 50 毫秒以后旧测试的输出。

## 验收

- `scripts/build/check.sh` 全绿。
- 压力下不再失败。在报告里贴结果：

  ```bash
  cargo test -p zutalk-core --test audio --no-run 2>&1 | tail -1   # 得到可执行文件的路径 B
  for i in $(seq 30); do (yes >/dev/null &); done
  for j in $(seq 12); do (for i in $(seq 100); do "$B" >/dev/null 2>&1 || echo FAIL; done &); done; wait
  killall yes
  ```

  改之前同样的办法 1800 次里出过 1 次 `FAIL`，门禁里也出过一次；改之后必须一次没有。
- `core/tests/audio.rs` 里，凡是钟在动的地方都没有 `recv_timeout`：等线程的地方钟都停着。

## 不做

- 不改 `Pump::step` 的规则，不改 cpal 那一层，不改声源线程 `hold`、`held` 的行为。
- 不动 `core/tests/hub/mod.rs` 和 `core/tests/app.rs` 里那个按真实时间哼的麦克风。
- 不搬目录（P1-02），不动 `audio.rs` 里的 `mpsc::channel()`（P1-08）。
