状态：完成

# P1-01 采集的时间：报告

## 改了什么

1. **策略。** `core/src/clock.rs`：`wall_clock_policy` 照 `DECISIONS.md` 第 3 条实现：落后 ≤ 300 ms → `Keep`，到 60 000 ms → `Pad(落后的毫秒)`，更长 → `End`。两个界限是具名常量 `JITTER_MS`、`ASLEEP_MS`，注释写明出处是临时决定第 3 条，也写了 300 ms 的代价。（用 `if` 而不用 `0..=JITTER_MS` / `..=ASLEEP_MS` 两个区间：clippy 的 `match_overlapping_arm` 认为它们重叠。）
2. **泵可以一圈一圈地转。** `core/src/audio.rs`：
   - 新的公开类型 `Turner`（泵、邮箱、各声源的 ender、sink、notice、时钟），`turn(wait) -> bool` 是原来泵线程循环体的一圈，`attached()` 数设备交到泵手里几次了；
   - `Capture::by_hand(opens, policy, now_ms, sink, notice) -> (Capture, Turner)` 起声源线程，不起泵线程；
   - `Capture::with` 就是 `by_hand` 加 `thread::spawn(move || while turner.turn(TICK) {})`。消息次序、停止时交完整的块、`over` 的置位、`Drop` 只等泵线程，都没变；
   - 两处小的补充，都不改生产行为：邮箱 `Disconnected` 当作 `Stop`（只会发生在 `Stop` 之后，原来的循环这时已经退出）；`Turner` 被丢掉时置 `over`，这样手转的测试失败中途退出时，声源线程也会结束。
3. **测试自己转泵。** `core/tests/audio.rs` 采集那一组：
   - `Run` 拿着 `Turner`。`play`：每 20 毫秒「放声音 → 钟加 20 → 转一圈」；`notice()`、`opened()`、`attached()`、`wait()`：钟不动，反复转（每圈最多等 1 毫秒），`WAIT` 只是上限；`opened()` 拿到设备后还要转到 `attached()` 加一。`until(n)` 删了，转完块已在手里。停止：丢掉 `Capture`，再转一圈，断言返回 `false`，再转一圈也是 `false`，之后没有块再来；
   - 测试的通道都是 `sync_channel(4096)`，sink 里用 `try_send().expect`（满了就是测试错了，不会卡住泵）；
   - 断言收紧到确切的数：死了又开的那条 17 块，声音、静音、声音的边界是 4、6；停流那条 72 块，边界是 10、62；打不开的那条 7 块；暂停那条 7 块；合盖 3 块；设备换了的那条断言打开 2 次、问了 2 次。17 不是 18、72 不是 73 的原因：重开时时钟还差 60 ms 没补，测试的策略把不到 100 ms 当抖动，声音接在 60 ms 之前；这是规则算出来的确定结果；
   - 停流那条原来是「边拨钟边等设备打开」，现在是确切地拨到第 5 秒泵下判断，钟停下等通知，再拨 240 ms 断言没开、拨 20 ms 断言开了；
   - 合盖原来靠「测试一口气拨两分钟，泵还没来得及读」，现在明说：钟跳两分钟，再转一圈，这一圈返回 `false`，通知是 `Ended`；
   - `a_device_that_hangs_in_the_system_holds_up_nothing` 留在真的泵线程上（`Capture::with`），写成独立的一段；拨钟时不等，等块、等通知时钟都停着；停止的上限从 1 秒改成 `WAIT`。
4. **文档。** `docs/DESIGN.md` §5.6 加一段写这次的原因和做法；§7 第 1 条改成「已照临时决定实现，数字等负责人确认」，并写出混音最多晚约 0.45 秒这个后果。`docs/PLAN.md` §8 的 P1 一行和 §9 第 3 条同步改了。

文件：`core/src/clock.rs`、`core/src/audio.rs`、`core/tests/clock.rs`、`core/tests/audio.rs`、`docs/DESIGN.md`、`docs/PLAN.md`、本报告。

## 证据

**复现（改之前）。** 在 `Run::start` 给 `Capture::with` 的 `notice` 回调里，`notices.send(notice)` 之后加 `thread::sleep(Duration::from_millis(50))`，跑 `cargo test -p zutalk-core --test audio a_source_that_dies` 八次，八次都失败：

```text
thread 'a_source_that_dies_is_reported_padded_and_opened_again' panicked at core/tests/audio.rs:598:60:
too few blocks came: Timeout                                   （5 次，各等满 5.1 秒）
thread 'a_source_that_dies_is_reported_padded_and_opened_again' panicked at core/tests/audio.rs:647:5:
no sound before it died                                        （3 次）
```

第二种是同一个抢法的另一面：泵第一次排空时钟已经被拨过去，起点晚了，前几块成了静音。复现的改动没有留在仓库里。

**新的测试。**

- `core/tests/clock.rs`：`the_app_keeps_jitter_pads_a_hole_and_ends_after_a_minute_asleep`（0、300 → `Keep`；301 → `Pad(301)`；60 000 → `Pad(60_000)`；60 001 → `End`）。它替换了原来挂起的 `the_real_policy_keeps_jitter_pads_stalls_and_ends_after_sleep`（`#[ignore = "wall_clock_policy is waiting for its owner"]`），那条的断言被新表格完全覆盖。
- `core/tests/audio.rs`：`with_the_app_policy_a_quiet_source_holds_the_mix_back_under_half_a_second`。`Script` 加了 `Script::with(inputs, policy)` 和每块交出的时刻 `at`。麦克风每 20 ms 送声音，电脑声音（`steady = false`）一直不送，跑 10 秒：时间线不断，块数 ≥ 95，每块交出时刻不晚于它结束后 450 ms。**量到的最大值是 300 ms**（20 ms 一步）；450 是给块的粒度和泵的一个 `TICK` 留的余量。
- 采集那一组按新办法重写：`a_source_that_dies_is_reported_padded_and_opened_again`、`a_stall_the_pump_hears_ends_the_device_on_its_own_thread`、`a_source_that_cannot_be_opened_is_fatal_and_the_other_one_carries_on`、`a_device_that_is_no_longer_the_right_one_is_opened_again`、`paused_the_clock_stands_still_and_resumed_it_goes_on`、`a_closed_lid_ends_the_capture_out_loud`、`a_device_that_hangs_in_the_system_holds_up_nothing`。

**`recv_timeout`。** `core/tests/audio.rs` 采集那一组里，`recv_timeout` 只出现在钟停着的地方：`nothing_from`（钟停着时等 100 ms，确认声源线程没有提前打开设备）和最后那条真线程测试里等设备、等通知、等块（都在拨钟之前或之后，拨钟的循环里不等）。

**压力（改之后）。** 照任务里的办法：30 个 `yes` 占着 CPU，12 路并行各跑 100 次 `audio` 测试的可执行文件，每次的结果写进文件再数：

```text
1200 OK
```

没有一次 `FAIL`（用了 3315 秒）。改之前同样的办法 1800 次里出过 1 次。另外在最终提交之前的一个构建上也照原样跑过一遍 1200 次，同样没有 `FAIL`。

**门禁。** `scripts/build/check.sh`：`check: all green`（cargo test 426 通过、0 失败、6 挂起；界面 27 通过）。

## 留下的、要负责人的

- `wall_clock_policy` 的数字是临时的（`DECISIONS.md` 第 3 条），等负责人确认。它的代价现在有测试量着：两路同录、电脑不出声时，混音最多晚约 0.3–0.45 秒交出。
- 真机上的手工清单（`core/tests/audio.rs` 开头）第 5 条「合盖」现在有了确定的行为：一分钟以内补静音，更长结束录音。要负责人在真机上看。
- 范围外、没动的：`core/tests/audio.rs` 里导入文件的 `import` 和手工的 `manual` 还用 `mpsc::channel()`；`core/src/audio.rs` 里三处 `mpsc::channel()`。都留给 P1-08。
