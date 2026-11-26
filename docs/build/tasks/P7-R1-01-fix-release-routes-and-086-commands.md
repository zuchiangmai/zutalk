# P7-R1-01 修正 `docs/RELEASE.md` 服务器反代路由表与 `zulangue` 0.8.6 构建发布命令

## 目标

1. 修正 `docs/RELEASE.md` §5.2 边缘反代转发路由清单，使其与 `server/src/lib.rs::ROUTES` 实际 8 条路由 100% 一致，杜绝按手册部署后登录、租借钥匙与认领旧邀请 404。
2. 修正 `docs/RELEASE.md` §4.1（及 `docs/build/reports/P7-03-close.md` 对应行动清单项）的 `zulangue` 0.8.6 本地打包试跑命令、`.app` 产物路径与发布环境变量，使其与 `/Users/kant/Zuzalu/zulangue/justfile` 及 `scripts/check_release_destination.sh` 完全一致。
3. 在 `app/tests/updates.rs` 补充 1 条确定性断言，永久锁死 `docs/RELEASE.md` §5.2 覆盖 `server/src/lib.rs` 的全部核心路由前缀，防止文档与服务器路由再次漂移。

## 背景

- 审查来源：`docs/build/reviews/P7-r1.md` **必须改 #1**、**必须改 #2**。
- 涉及文件：
  - `docs/RELEASE.md`（修正 §4.1 与 §5.2）
  - `docs/build/reports/P7-03-close.md`（同步修正负责人清单 §6 第 1 条中的构建命令）
  - `app/tests/updates.rs`（在既有测试或新增小测试中核对 `docs/RELEASE.md` 包含真实服务器路由路径且不含虚假路径 `/keys/*`、`/invites/*`、`/signin/*`）
- 只读对照（**严禁修改**）：
  - `server/src/lib.rs:225-234`（`ROUTES`）
  - `/Users/kant/Zuzalu/zulangue/justfile:11,45,348-380,622,809-816,888-919`
  - `/Users/kant/Zuzalu/zulangue/scripts/check_release_destination.sh`

## 做法

### 1. 修正 `docs/RELEASE.md` §5.2 服务器反代路由清单

将 `docs/RELEASE.md` 第 182 行由错误的 `/keys/*`、`/invites/*`、`/signin/*` 修正为与 `server/src/lib.rs:225-234` 逐一对应的真实路由：

- `/healthz`（`GET`，健康检查）
- `/grants`（`POST`，协调人授额）
- `/quota`（`POST`，余额与按服务日额度查询）
- `/lease`（`POST`，租借 Soniox / OpenRouter 短期临时钥匙）
- `/claim`（`POST`，认领 0.8 旧邀请）
- `/rooms`（`POST`，主持建房/写槽/关闭）与 `/rooms/{id}/events`（`GET`，SSE 直播长连接，必须关闭反代响应缓冲 `proxy_buffering off` 并放宽读超时）
- `/.well-known/zu-relay/*`（`GET` / `POST`，扫码登录一次性中继房）

### 2. 修正 `docs/RELEASE.md` §4.1 与 `docs/build/reports/P7-03-close.md` 的 `zulangue` 0.8.6 命令

1. 在 `docs/RELEASE.md` §4.1 第 4–5 步：
   - 明确 `just dev` 只编 Rust FFI、生成 UniFFI Swift 绑定并同步 Xcode 工程，随后需跑 `just build-local-app`（对应 `zulangue/justfile:622`：`dev xcode-build`）产出 `build/app/ZuTalk.app`；
   - 第 5 步试跑命令改为 `CFFIXED_USER_HOME="$TMP_HOME" open -W build/app/ZuTalk.app`（删除不存在的 `macos/ZuTalk/build/Debug/ZuTalk.app`）。
2. 在 `docs/RELEASE.md` §4.1 第 6 步：
   - 写明 `just release-sparkle-adhoc`（及 `just release-ship`）内部调用 `sparkle-appcast` 与 `scripts/check_release_destination.sh`，必须显式传入 `GITHUB_REPOSITORY`（与 `zulangue` 的 `git remote get-url github` 一致，例如 `GITHUB_REPOSITORY="$(git remote get-url github | sed -E 's#^git@github\.com:#https://github.com/#; s#^https://github\.com/##; s#\.git$##; s#/$##')"` 或手填仓库名）和 `GITHUB_REF_NAME=v0.8.6`，例如：
     ```sh
     GITHUB_REPOSITORY="<与 git remote get-url github 一致，如 4seas-community/ZuTalk 或 zuchiangmai/ZuTalk>" \
     GITHUB_REF_NAME="v0.8.6" \
     just release-sparkle-adhoc   # 或带同样环境变量跑 just release-ship 打 tag 并发布到 GitHub
     ```
   - 注明生成的 `appcast.xml` 位于 `build/dmg/appcast.xml`（见 `zulangue/justfile:874`），供 §4.2 合并。
3. 同步更新 `docs/build/reports/P7-03-close.md` 负责人清单第 6 组第 1 条（将 `just dev` 补齐为 `just build-local-app` 产出 `build/app/ZuTalk.app`，并注明发布 `v0.8.6` 时传 `GITHUB_REPOSITORY` 与 `GITHUB_REF_NAME=v0.8.6`），同时在报告顶部或门禁段如实记录本次 `P7-R1-01` 复跑后的最新测试计数。

### 3. 在 `app/tests/updates.rs` 增加文档与路由对齐守卫

在 `app/tests/updates.rs` 中新增纯文件审计测试 `release_doc_matches_server_routes_and_handoff_commands`（或在 `release_versions_and_manifests_are_aligned` 旁独立成条）：
- 读取 `docs/RELEASE.md`，断言包含真实服务器路由 `/healthz`、`/grants`、`/quota`、`/lease`、`/claim`、`/rooms`、`/rooms/{id}/events`、`/.well-known/zu-relay/`；
- 断言不再出现已否决的虚假路径 `/keys/*`、`/invites/*`、`/signin/*` 以及不存在的 `macos/ZuTalk/build/Debug/ZuTalk.app`；
- 断言 §4.1 包含 `just build-local-app`、`build/app/ZuTalk.app`、`GITHUB_REPOSITORY` 与 `GITHUB_REF_NAME`。

## 显式与可观测

- **状态、常量与队列**：本修复不改动任何运行时状态机、队列、任务或 `Stats` 计数器（零新增常量、零新增事件、零新增 `Stats` 行）。
- **显式契约守护**：通过 `app/tests/updates.rs::release_doc_matches_server_routes_and_handoff_commands` 把生产反代路由和 0.8.6 交接命令的正确性纳入确定性测试门禁，后续改动路由或手册若再次漂移会在 `scripts/build/check.sh` 立即红灯可见。
- **故障可见性**：先写测试确认在当前 `docs/RELEASE.md` 上明确红灯失败（检出虚假路径 `/keys/*`、`/invites/*`、`/signin/*` 与错误产物路径），修正文档后转绿。

## 先写的测试

先在 `app/tests/updates.rs` 添加 `release_doc_matches_server_routes_and_handoff_commands`，运行 `RUSTC_WRAPPER="" cargo test -p zutalk --test updates` 确认其**明确失败**，再修改 `docs/RELEASE.md` 与 `docs/build/reports/P7-03-close.md` 至转绿。

## 验收

- `RUSTC_WRAPPER="" cargo test -p zutalk --test updates` 全部 8 条测试通过；
- `RUSTC_WRAPPER="" scripts/build/check.sh` 一次全绿（10 个规则脚本全绿零欠账，Rust / UI 全部通过）；
- `Cargo.lock` 与 `ui/package-lock.json` 零变更，`/Users/kant/Zuzalu/zulangue` 零改动。

## 不做

- 不修改任何运行时产品代码（`core/src`、`app/src`、`server/src`、`zu/src`、`log/src`、`ui/src`）；
- 不修改 `/Users/kant/Zuzalu/zulangue` 内任何文件；
- 不新增外部依赖或 `Stats` 行。
