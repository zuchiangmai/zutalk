状态：完成

## 改了什么

1. **全仓 `0.9.0` 版本对齐（`ui/package.json`、`ui/package-lock.json`）**
   - 在 `ui/package.json` 补齐 `"version": "0.9.0"`，同步更新 `ui/package-lock.json` 顶层及 `packages[""]` 自身 `"version": "0.9.0"`（仅 2 行，零外部依赖变动），与根 `Cargo.toml`（`workspace.package.version = "0.9.0"`）、5 个 Rust crate（均 `version.workspace = true`）、`core`/`server` 的 `zu = "0.9.0"` 及 `app/tauri.conf.json`（省略顶层 `version` 继承 `app/Cargo.toml`，`bundleVersion = "67"` > 0.8.6 的 `66`）完全一致。
2. **自动化发布、权限与防泄漏审计测试（`app/tests/updates.rs`）**
   - 新增 3 条确定性配置与源码审计测试：
     - `release_versions_and_manifests_are_aligned`：核对根 `Cargo.toml` 合法 semver 且等于 `env!("CARGO_PKG_VERSION")`、5 个成员 crate 均 `version.workspace = true`、`core`/`server` 的 `zu` 依赖版本一致、`ui/package.json` 版本一致、`app/tauri.conf.json` 图标路径全部存在且 `bundleVersion > 66`、`docs/migrate-0.8.patch` 含 `handover.rs`、`LibraryHandover.swift` 与 `channel = "zu"`；
     - `shell_capabilities_and_csp_keep_updater_and_clipboard_off_the_page`：锁死 `app/capabilities/default.json` 仅授予 `["main", "captions"]` 窗口最小权限（禁止 `updater`/`clipboard`/`shell`/`fs`/`http`），锁死 `app/tauri.conf.json` 的 CSP 含 `default-src 'self'` 且无 `http://`/`https://`/`unsafe-eval`；
     - `runtime_sources_contain_no_placeholder_secrets_or_personal_emails`：递归扫描 `app/src`、`core/src`、`server/src`、`zu/src`、`log/src`、`scripts/updates.py`，确保零私钥头、API 密钥前缀、个人邮箱后缀及生产占位标记。
3. **升级收拢 `docs/RELEASE.md` 并交叉链接 `docs/migrate.md`**
   - 遵循原则 7 不另建平行文档，将发布流程、三平台图标与打包注意项（含编译期生产 `ZUTALK_SERVER` 与可选 Developer ID 公证）、在 `/Users/kant/Zuzalu/zulangue` 从 `0.8.5`（构建 65）打补丁构建并发布 `0.8.6`（构建 66）及生成双项签名 `appcast.xml`（`zu` 频道跳跃至 v0.9.0 构建 67）的逐条命令、按 `server/src/main.rs` 核对的全部 **16 项** 服务器环境变量表与边缘 `/r/*` 反代要求、以及 5 组真机与 8 小时手工验收清单全部收拢进 `docs/RELEASE.md`（§1–§7），并在 `docs/migrate.md` 末尾补齐交叉引用。

## 实测证据

- **红灯先验**：写入 3 条新测试后先跑 `RUSTC_WRAPPER="" cargo test -p zutalk --test updates`，`release_versions_and_manifests_are_aligned` 明确失败：`assertion left == right failed: ui/package.json version must match workspace version (left: None, right: Some("0.9.0"))`；其余 6 条通过。
- **修复后门禁实测**：
  - `cargo fmt --all`：通过；
  - `RUSTC_WRAPPER="" cargo clippy --workspace --all-targets -- -D warnings`：通过（0 warning）；
  - 全部 10 个 `scripts/build/rules/*.sh`：全部退出码 `0`（0 欠账）；
  - `RUSTC_WRAPPER="" cargo test -p zutalk --test updates`：7 passed，0 failed（0.23 s）：
    - `the_app_is_the_one_the_0_8_app_updates_to`
    - `the_manifest_gives_each_target_its_file_and_signature_as_the_app_reads_them`
    - `a_release_the_app_would_refuse_is_refused_before_it_is_published`
    - `looking_for_an_update_says_what_it_found`
    - `release_versions_and_manifests_are_aligned`
    - `shell_capabilities_and_csp_keep_updater_and_clipboard_off_the_page`
    - `runtime_sources_contain_no_placeholder_secrets_or_personal_emails`
  - UI 全检（`cd ui && npm run check && npm test && npm run build`）：TypeScript + Vite 三入口（`ui/dist/{index,captions,viewer}.html`）全部通过，Vitest `16` 个测试文件 / `124` 条测试全绿（2.03 s）。
- **依赖与红线核验**：`Cargo.lock` 零变更、`ui/package-lock.json` 仅新增 2 行自身 `"version": "0.9.0"` 无任何外部包改动、零新增 `Stats` 行、未对 `/Users/kant/Zuzalu/zulangue` 做任何写操作。

## 还差什么 / 需要负责人

- `app/tauri.conf.json` 的 `plugins.updater.pubkey` 保持空占位（`scripts/updates.py` 拒签空公钥），待负责人按 `docs/RELEASE.md` §2 生成真实 minisign 钥匙对后填入；
- `zulangue` 0.8.6 补丁编译/发版、三平台 `.ico`/`.icns` 生成与实际打包签名、服务器 16 项环境变量部署及 §6 真机/8 小时手工清单均归入负责人最终行动清单。
