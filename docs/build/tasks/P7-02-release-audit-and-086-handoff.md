# P7-02 发布配置、签名更新器审计与 0.8.6 交接操作手册

## 目标

1. 对齐全仓 `0.9.0` 版本号（补齐 `ui/package.json` 缺失的 `"version": "0.9.0"`），并全面审计 `app/tauri.conf.json`、`app/capabilities/default.json`、`app/src/updates.rs`、`scripts/updates.py` 与 `server/src/main.rs` 的发布、更新器、权限与环境变量配置。
2. 在 `app/tests/updates.rs` 新增自动化发布审计测试，永久守住：全仓版本与图标一致、Capabilities 与 CSP 最小权限（更新与剪贴板绝不暴露给页面）、以及运行时源码零硬编码密钥/生产占位符/个人邮箱。
3. 按 `CLAUDE.md` 原则 7（少文档、就地收拢）将完整的 **0.8.6 交接操作步骤、服务器部署环境变量与边缘反代配置、三平台构建签名/公证注意项与 8 小时/3 OS 真机手工清单** 折叠进 `docs/RELEASE.md`（不另增零散顶层文档，绝不修改 `/Users/kant/Zuzalu/zulangue`）。

## 背景

- 设计依据：`docs/build/phases/P7.md` §0、§1（E3–E4）、§2、§3.3；`docs/RELEASE.md`；`docs/migrate.md`；`docs/DESIGN.md` §5.10、§7、§9。
- 涉及文件：
  - `ui/package.json`（补 `"version": "0.9.0"`；**不改动任何依赖项**）
  - `app/tauri.conf.json`（审计确认 `identifier`、`productName`、`bundle.icon`、`bundle.macOS.bundleVersion`、`signingIdentity`、`createUpdaterArtifacts`、`plugins.updater`、`app.security.csp`）
  - `app/capabilities/default.json`（审计确认最小权限）
  - `app/src/updates.rs`、`app/src/main.rs`、`server/src/main.rs`、`scripts/updates.py`（只读审计；仅当发现真实配置缺陷时才最小修复）
  - `app/tests/updates.rs`（新增 3 条发布审计自动化测试）
  - `docs/RELEASE.md`（升级收拢 0.8.6 交接步骤、服务器与边缘部署表、三平台与 8h 真机清单）
  - `docs/migrate.md`（末尾补一行交叉引用 `docs/RELEASE.md` §4）
- 只读参考：`/Users/kant/Zuzalu/zulangue/{Cargo.toml,justfile,scripts/bump_release_version.sh}`（当前为 `0.8.5` 构建 `65`，**严禁写入或修改该目录内任何文件**）。

## 做法

### 1. 版本对齐与配置审计

1. 在 `ui/package.json` 的 `"name": "zutalk-ui"` 下添加 `"version": "0.9.0"`；检查 `ui/package-lock.json` 顶层自身 `version` 字段是否需同步为 `"0.9.0"`（若同步，只允许改顶层包与 `packages[""]` 的自身 `version`，**绝不新增或变更任何外部依赖**）。
2. 逐项核对以下发布配置事实（若全部已符合预期则保持原样、用测试锁死；若发现真实缺陷则在测试红灯后最小修复）：
   - `Cargo.toml`：`workspace.package.version == "0.9.0"`；`zu`/`log`/`core`/`server`/`app` 全部 `version.workspace = true`；`core/Cargo.toml` 与 `server/Cargo.toml` 里的 `zu` 依赖版本为 `"0.9.0"`；
   - `app/tauri.conf.json`：
     - `identifier == "xyz.voice.zutalk"`、`productName == "ZuTalk"`；
     - 未硬编码与 `Cargo.toml` 冲突的 `"version"` 字段（Tauri v2 自动继承 `app/Cargo.toml`）；
     - `bundle.macOS.bundleVersion == "67"`（> 0.8.6 的 `66`）、`signingIdentity == "-"`、`createUpdaterArtifacts == true`；
     - `bundle.icon` 中列出的每个路径在 `app/` 下均真实存在；
     - `plugins.updater.endpoints` 唯一指向 `https://github.com/zuchiangmai/ZuTalk/releases/latest/download/latest.json`，`pubkey` 为空串占位（发版前由负责人按 `RELEASE.md` §2 填入，且 `scripts/updates.py` 拒绝空 `pubkey`）；
     - `app.security.csp` 含 `default-src 'self'` 且无任何外部 `http://`/`https://` 域名；
   - `app/capabilities/default.json`：仅限 `["main", "captions"]` 窗口，权限仅含 `core:default`、`core:window:allow-start-dragging`、`dialog:default`，无 `updater`、`clipboard`、`shell`、`fs` 权限；
   - `app/src/main.rs` 与 `server/src/main.rs`：编译期 `ZUTALK_SERVER` 与运行期 15 个服务器环境变量无硬编码真实密钥、无真实人名或个人邮箱。

### 2. 自动化发布审计测试（`app/tests/updates.rs`）

在 `app/tests/updates.rs` 新增 3 条纯文件/配置审计测试（不联网、不改磁盘状态，完全确定）：

1. `release_versions_and_manifests_are_aligned`：
   - 读取根 `Cargo.toml` 的 `workspace.package.version`（验证其为合法 semver 且等于 `env!("CARGO_PKG_VERSION")`）；
   - 验证 5 个成员 `Cargo.toml`（`zu`、`log`、`core`、`server`、`app`）均含 `version.workspace = true`，且 `core/Cargo.toml` 与 `server/Cargo.toml` 中 `zu` 依赖版本与 workspace 版本一致；
   - 验证 `ui/package.json` 的 `"version"` 等于 workspace 版本；
   - 验证 `app/tauri.conf.json` 若含 `"version"` 则必须等于 workspace 版本（或省略以继承 Cargo），验证 `bundle.icon` 数组非空且每一项对应 `app/<path>` 文件真实存在，且 `bundle.macOS.bundleVersion` 严格大于 `66`；
   - 验证 `docs/migrate-0.8.patch` 文件存在且包含 `handover.rs`、`LibraryHandover.swift` 与 `channel = "zu"` 关键段落。
2. `shell_capabilities_and_csp_keep_updater_and_clipboard_off_the_page`：
   - 解析 `app/capabilities/default.json`，断言 `windows` 恰为 `["main", "captions"]`，`permissions` 不含任何包含 `updater`、`clipboard`、`shell`、`fs` 或 `http` 的条目；
   - 解析 `app/tauri.conf.json` 的 `app.security.csp`，断言包含 `default-src 'self'` 且不包含 `http://`、`https://` 或 `unsafe-eval`。
3. `runtime_sources_contain_no_placeholder_secrets_or_personal_emails`：
   - 递归扫描 `app/src`、`core/src`、`server/src`、`zu/src`、`log/src`、`scripts/updates.py` 源码文本；
   - 断言零出现私钥头（`-----BEGIN`）、API 密钥前缀（`sk-or-v1-`、`sk-proj-`）、常见个人邮箱后缀（`@gmail.com`、`@icloud.com`、`@proton.me`、`@outlook.com`）、或生产占位标记（`REPLACE_ME`、`CHANGEME`、`YOUR_API_KEY`）。

### 3. 升级收拢 `docs/RELEASE.md`（简体中文，绝不另建平行文档）

在保留 `docs/RELEASE.md` 现有清晰平实文风的前提下，就地更新与补齐为 v0.9.0 完整发布与 0.8.6 交接操作手册：

1. **更新 §3（发一版）**：
   - 将第 1 步版本同步清单更新为 4 处检查点（根 `Cargo.toml`、`core`/`server` 的 `zu` 依赖版本、`ui/package.json`、`app/tauri.conf.json` 的 `bundleVersion`）；
   - 将第 2 步门禁命令更新为 `RUSTC_WRAPPER="" scripts/build/check.sh`（覆盖 rustfmt、clippy、全部 10 个规则脚本、全量 Rust 与 UI 测试及三入口构建）；
   - 在第 3–4 步补齐三平台实务说明：
     - macOS：ad-hoc 通用包与未来切换 Developer ID + `notarytool` 公证的路径；
     - Windows / Linux：说明构建前需用 `cargo tauri icon` 从 `app/icons/icon.png` 补齐 `.ico` / `.icns` 并挂入 `bundle.icon`，NSIS 与 AppImage 产物回到本机 Mac 上签名；
     - 提醒构建 release 时必须显式传入生产 `ZUTALK_SERVER="https://<正式域名>"`（否则默认回落到 `https://localhost:8443`）。
2. **扩充 §4（在 `zulangue` 发 0.8.6 交接版与 Sparkle `zu` 频道跳跃）**：
   - 写清从当前 `zulangue`（`0.8.5`，构建 `65`）发布 `0.8.6`（构建 `66`）的逐条可复制命令（供负责人在 `/Users/kant/Zuzalu/zulangue` 手动执行）：
     1. 核对旧仓状态干净且处于 `0.8.5`（构建 `65`）；
     2. `git apply --check /Users/kant/Zuzalu/zutalk/docs/migrate-0.8.patch` 后 `git apply`；
     3. `just bump 0.8.6`（同步 `Cargo.toml`、`MARKETING_VERSION = 0.8.6`、`CURRENT_PROJECT_VERSION = 66` 并归档发布说明）；
     4. `cargo check -p vt-ffi` 与 `just dev`（重新生成 UniFFI Swift 绑定并触发 `_sync-xcode` 将 `LibraryHandover.swift` 同步进 `ZuTalk.xcodeproj`）；
     5. 用真实 175 条录音目录副本在隔离 `CFFIXED_USER_HOME` 下启动一次，核对 `handover/index.json`、`handover/recordings/*.json` 与音频块路径完整且原数据库/音频未被修改；
     6. `just ci-check` → `just release-sparkle-adhoc`（或 `release-ship`）发布 `v0.8.6`，保存其 `appcast.xml`；
     7. 按既有 `generate_appcast --account Zulangue --channel zu --maximum-versions 2` 步骤生成含 `0.8.6`（构建 `66`）与 `0.9.0`（构建 `67`，`zu` 频道）两项的双项签名 `appcast.xml`，随 `v0.9.0` Release 一同上传并在之后每个 Latest Release 原样保留。
3. **新增 §5（服务器与前端边缘部署配置）**：
   - 逐项列表记录 `server/src/main.rs` 读取的全部 15 个环境变量（`ZUTALK_DATA`、`ZUTALK_LOG`、`ZUTALK_LISTEN`、`ZUTALK_ORIGIN`、`ZUTALK_COORDINATORS`、`ZUTALK_DAILY_SONIOX`、`ZUTALK_DAILY_OPENROUTER`、`INVITES_URL`、`INVITES_TOKEN`、`SONIOX_URL`、`SONIOX_API_KEY`、`SONIOX_MICROS_PER_SECOND`、`SONIOX_SESSION_CAP_S`、`OPENROUTER_URL`、`OPENROUTER_MANAGEMENT_KEY`、`OPENROUTER_SLICE_MICROS` — 实际共 16 个，以源码为准逐一核对列出！）、默认值、单位与敏感级别；
   - 写明前端边缘配置要求：静态托管 `ui/dist/`（`viewer.html` 及静态资源），将 `/r/*` 重写到 `viewer.html`（同源访问 `GET /rooms/{id}/events`），强制 HTTPS 与边缘限流，以及旧邀请服务上线 `/v1/retire` 并对齐 `INVITES_TOKEN == ZULANGUE_ADMIN_TOKEN`。
4. **新增 §6（发布前真机与 8 小时手工验收清单）**：
   - 集中整理 5 组勾选框：三系统音频硬件 7 项（指向 `core/tests/audio.rs`）、8 小时真机实录与内存/延迟观测、macOS/Windows/Linux 字幕窗与 WebView 视觉走查、真 Soniox/OpenRouter 钥匙读书会实测、`0.8.5 → 0.8.6 → 0.9.0` Sparkle 跳跃与 Tauri 自更新实测。
5. 在 `docs/migrate.md` 末尾补一行指向 `docs/RELEASE.md` §4，保持两处手册一致无重复。

## 显式与可观测

- **显式不变量守护**：将原本只靠肉眼核对的发布版本、图标存在性、Capabilities 最小权限、CSP 边界与无硬编码密钥/个人邮箱规则写成 `app/tests/updates.rs` 的 3 条确定性测试，每次 `check.sh` 自动执行。
- **命名常量与脚本约束**：如未修改 `app/src` / `core/src` / `server/src` 则零新增事件/常量；若审计发现并修复了生产代码，须严格遵守 `numbers.sh`、`dropped.sh`、`shell.sh`、`events.sh` 与零新增 `Stats` 行规则。
- **故障可见性**：`app/tests/updates.rs` 已有 `a_release_the_app_would_refuse_is_refused_before_it_is_published` 与 `looking_for_an_update_says_what_it_found`（验证 `update.look` 三个结果码 `none` / `ready` / `update`），连同新增的 3 条配置/权限/隐私测试构成完整发布前防线。

## 先写的测试

先在 `app/tests/updates.rs` 写入以下 3 条测试（其中 `release_versions_and_manifests_are_aligned` 在 `ui/package.json` 补 `"version": "0.9.0"` 前应**明确失败**）：

1. `release_versions_and_manifests_are_aligned`
2. `shell_capabilities_and_csp_keep_updater_and_clipboard_off_the_page`
3. `runtime_sources_contain_no_placeholder_secrets_or_personal_emails`

## 验收

- `RUSTC_WRAPPER="" cargo test -p zutalk --test updates` 全部 7 条测试通过；
- `RUSTC_WRAPPER="" scripts/build/check.sh` 一次全绿；
- `docs/RELEASE.md` 包含完整的 §1–§6（含 `server/src/main.rs` 全部环境变量精确核对、0.8.6 交接操作步骤、三平台与 8h 真机清单），且仓库内无新增平行发布文档；
- `/Users/kant/Zuzalu/zulangue` 未被任何命令写入或修改；`Cargo.lock` 零变更、`ui/package-lock.json` 无任何新增外部依赖。

## 不做

- 不修改 `/Users/kant/Zuzalu/zulangue` 内的任何文件，不运行 `zulangue` 的构建；
- 不生成真实 minisign 钥匙对、不向 `app/tauri.conf.json` 填入假 `pubkey`、不执行 `cargo tauri build` 或 `gh release`；
- 不新增外部 crate 或 npm 包，不新增 `Stats` 行；
- 不修改任何 `P6*` 文件。
