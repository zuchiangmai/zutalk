# P5-R1-02 修复 `FileArchiveCard` 跨录音串台/卡死与观众页 `gone` 后异步回写竞态

## 目标

修复 P5 第 1 轮审查中的 **必须改 #3、#4**：

1. **修复 `FileArchiveCard` 跨录音串台与永久 `submitting` 卡死（必须改 #3）**：
   - 在 `ui/src/pages/ReaderPage.tsx` 与 `ui/src/pages/ResultPage.tsx` 为 `<FileArchiveCard>` 加上 `key={recording.row.id}`；同时在 `FileArchiveView` 内部监听 `recording.row.id` 变化时立刻重置本地状态（`phase`、`includeAudio`、`selectedEventPath`、`graceRemainingSec`）并取消进行中的 1.4 s 按住与 8 s 宽限计时器，确保从录音 A 切换到录音 B 时绝不把录音 B 误交城档案馆，也绝不错显录音 A 的归档状态；
   - 在 `FileArchiveCard` 挂载时，若 `calendarQuery.data === null` 且存在目标城地址（`hostEvent?.city ?? recording.filed?.city ?? state?.setup.city`），自动派发一次 `Command::City` 加载城日历（与 `HostPage.tsx` 保持一致）；
   - `FileArchiveView` 仅当 `calendar` 已加载且当前选中的活动路径存在于 `calendar.events`（与核心 `city::begin` 校验一致）、且当前录音未处于精修中（`recording.refine.is !== 'Refining'` 且 `progressRefine?.id !== row.id`）时才允许按住提交；
   - 将 `onConfirmFile` 签名改为 `(eventPath: string, audio: boolean) => Promise<boolean> | boolean | void`：`FileArchiveCard` 返回 `send({ do: 'File', id: recording.row.id, event, audio })` 的结果；当 `onConfirmFile` 异步或同步返回 `false`（命令被核心拒绝）时，`FileArchiveView` 通过 `filePhaseStep(p, { is: 'SubmitRejected' })`（或等效显式事件）立刻从 `submitting` 退回 `review` 以便重试，绝不永久卡死在 `submitting`。
2. **修复手机观众页 `ui/src/viewer/room.ts` 的 `gone`/关闭竞态与坏密钥错误处理（必须改 #4）**：
   - 在 `read(events, fragment)` 内维护 `let closed = false;`，在 `finish()`（置 `closed = true` 后幂等返回）、`slot` 异步解密回调、`onerror` 的异步 `fetch(events)` 回调以及返回的清理函数中均检查 `if (closed) return;`；
   - 确保 `gone` 到达时即使前一条 `slot` 尚在 `crypto.subtle.decrypt` 途中，`roomLiveCell` 也不会在 `gone` 之后被回写非空字幕；
   - 确保清理函数调用后旧连接不再污染新房状态，且坏密钥/解密失败只触发一次 `finish('room.bad', { is: 'BadKey' })`、不向外产生未捕获的 Promise rejection。

## 背景

- 审查报告：`docs/build/reviews/P5-r1.md`（必须改 #3、#4）。
- 涉及文件：
  - `ui/src/bus/filing.ts`
  - `ui/src/pages/FileArchiveCard.tsx`
  - `ui/src/pages/ReaderPage.tsx`
  - `ui/src/pages/ResultPage.tsx`
  - `ui/src/viewer/room.ts`
  - `ui/tests/host_outbound.test.tsx`
  - `ui/tests/room.test.ts`

## 做法

### 1. `FilePhase` 显式回退事件与 `FileArchiveCard` 防串台/防卡死（`ui/src/bus/filing.ts`、`ui/src/pages/{FileArchiveCard,ReaderPage,ResultPage}.tsx`）

- 在 `ui/src/bus/filing.ts` 为 `FilePhaseEvent` 增加 `{ is: 'SubmitRejected' }`：
  - `submitting + SubmitRejected -> review`（其余状态保持不变），并更新转换表测试。
- 在 `ui/src/pages/ReaderPage.tsx:652` 与 `ui/src/pages/ResultPage.tsx:359`：
  - 为 `<FileArchiveCard key={recording.row.id} ... />` 显式加 `key`；
  - 传入当前精修进行标志或让 `FileArchiveCard` 直接读取既有 `progressSource` 的 `progress.refine`。
- 在 `ui/src/pages/FileArchiveCard.tsx`：
  - `FileArchiveViewProps`：
    - `onConfirmFile: (eventPath: string, audio: boolean) => Promise<boolean> | boolean | void;`
    - 可选 `refining?: boolean;`（默认 `false`，`FileArchiveCard` 传入 `recording.refine.is === 'Refining' || progress.refine?.id === recording.row.id`）。
  - 在 `FileArchiveView` 中：
    - 增加 `recording.row.id` 变化监听 `useEffect`，重置 `phase`、`includeAudio`、`selectedEventPath`、`graceRemainingSec`（同步使任何在途 `grace` 定时器随 `[phase, row.id]` 清理，同时 `confirmRef` 仅允许为当前 `row.id` 触发）；
    - `canSubmitEvent` 仅当 `resolvedEvent !== null && calendar !== null && calendar.events.some((e) => e.path === resolvedEvent.path)` 时为真；在 `refining` 为真或 `!canSubmitEvent` 时禁用按住按钮（并在 `refining` 时显示明确提示，或在 `calendar === null` 且未输入城时提示加载城）；
    - 8 s `grace` 到期调用 `Promise.resolve(confirmRef.current(path, audio)).then((ok) => { if (ok === false && activeIdRef.current === rowId) setPhase((p) => filePhaseStep(p, { is: 'SubmitRejected' })); })`。
  - 在 `FileArchiveCard` 中：
    - 增加一次性自动拉取城日历的 `useEffect`（用 `loadedCityRef` 防重复）：若 `!calendarQuery.data` 且 `const targetCity = (hostEvent?.city ?? recording.filed?.city ?? state?.setup.city)?.trim()` 非空且 `loadedCityRef.current !== targetCity`，记录 `loadedCityRef.current = targetCity` 并 `void send({ do: 'City', address: targetCity })`；
    - `onConfirmFile={(event, audio) => send({ do: 'File', id: recording.row.id, event, audio })}`。

### 2. `ui/src/viewer/room.ts` 关闭守卫与无未捕获拒绝

- 在 `read(events, fragment)` 中：
  ```ts
  let closed = false;
  const finish = (code: string, event: ViewerStatusEvent) => {
    if (closed) return;
    closed = true;
    roomLiveCell.set([]);
    roomEndCell.set(code);
    roomStatusCell.update((prev) => viewerStatusStep(prev, event));
    say(code);
    source.close();
  };
  ```
- 将密钥初始化与 `slot` 串行解密链改写为返回 `Promise<CryptoKey | null>`：
  - 密钥失败时调用 `finish('room.bad', { is: 'BadKey' })` 并返回 `null`（绝不向外抛未捕获异常）；
  - 每个 `slot` 事件在 `.then(async (key) => { if (closed || !key) return null; ... if (closed) return null; then(decoded); return key; })` 中双重检查 `closed`（解密前与解密后），解密出错时调用 `finish('room.bad', { is: 'BadKey' })` 并返回 `null`；
  - `source.onerror` 中检查 `if (closed || source.readyState !== EventSource.CLOSED) return;`，并在 `fetch(events)` 的 `.then`/`.catch` 内均先检查 `if (closed) return;`；
  - 返回清理函数：`return () => { closed = true; source.close(); };`。

## 显式与可观测

- **显式状态机扩展**：`filePhaseStep` 新增 `SubmitRejected` 转换（`submitting -> review`）并全表测试；`viewerStatusStep` 保持穷举转换不变，所有异步回调统一受显式 `closed` 生命周期守卫约束。
- **遵循 `ui.sh` 边界**：不引入任何新的 `@tauri-apps/*` 或 `useSyncExternalStore` 越界调用，不加 `console.log` 或外部依赖；所有常量保持具名带单位（`HOLD_TO_FILE_MS`、`FILE_GRACE_MS`）。
- **故障可见性**：归档命令被拒时既有 `send()` 仍通过 `say(code)` 展示稳定错误码，卡片同步退回 `review` 可重试；观众页坏密钥只报一次 `room.bad`，收到 `gone` 后内存实时字幕立即且永久归零。

## 先写的测试

1. `ui/tests/host_outbound.test.tsx` 更新与新增：
   - 更新 `filePhaseStep_transition_table` 覆盖 `SubmitRejected` 全行；
   - 新增 `FileArchiveView_resets_and_cancels_grace_when_recording_id_changes`：在录音 A 按住 1.4 s 进入 8 s `grace` → 第 3 秒时 `rerender` 切到录音 B（新 `recording.row.id` 与新 `onConfirmFileB`）→ 推进 10 s → 断言 `onConfirmFileA` 与 `onConfirmFileB` 均调用 **0 次**，且录音 B 处于自身初始态（不继承录音 A 的 `grace` 或 `filed`）；
   - 新增 `FileArchiveView_recovers_to_review_when_confirm_is_rejected`：传入返回 `Promise.resolve(false)` 的 `onConfirmFile` → 走完 1.4 s + 8 s → 等待微任务后断言卡片退回 `review`（「Hold to file」重新出现）而非卡死在 `submitting`。
2. `ui/tests/room.test.ts` 新增：
   - `read_ignores_in_flight_slot_decrypt_after_gone_and_after_stop`：用假 `EventSource` 在 `read()` 中派发加密 `slot` 事件后**同步立即**派发 `gone` 事件，等微任务全部冲完后断言 `roomStatusSource.getSnapshot() === 'ended'`、`roomEndSource.getSnapshot() === 'room.gone'` 且 **`roomLiveSource.getSnapshot()` 严格为 `[]`**（未被晚到的 `openFrame` 回写）；并验证坏密钥只触发一次终态且无未捕获 rejection。

## 验收

- `RUSTC_WRAPPER="" scripts/build/check.sh` 全绿（含 `ui.sh`、`npm run check`、`npm run build`、`npm test` 与 Rust 全量测试）。

## 不做

- 不修改 Rust 代码（归 `P5-R1-01`）；
- 不新增 npm 依赖或改动翻译键数量（复用既有文案与代码）。
