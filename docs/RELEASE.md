# 发布、应用内更新与 0.8.6 交接

> 2026-11-26（原稿 2026-11-16，P7-02 收拢升级）。新版怎么自己更新、负责人发一版（v0.9.0 及之后）要做的事、在旧仓库 `zulangue` 发 0.8.6 交接版并经 Sparkle 跳到新版、服务器与边缘反代部署，以及发布前真机与 8 小时手工验收清单。设计落在哪里见 `DESIGN.md` §5.10。

## 1. 应用怎么更新自己

- 外壳用 Tauri 官方的更新插件 `tauri-plugin-updater`，版本锁在 `=2.13.2`。启动时看一次，之后每 6 小时（`LOOK_MS`）看一次，直到下到一个比自己新的版本、并用配置里的公钥核对过签名；这时界面顶栏出现一个按钮「更新」。
- 按下去：有录音在进行就不装，说 `update.recording`；没有才装，装好就重启。装不上说 `update.install`，下好的版本留着，可以再按。录音的转写如果正写到一半，重启后照崩溃恢复的路重写，不丢字。
- 找不到、下不来、签名不对，都说一次 `update`，直到某次看得顺利；每次检查都写结构化事件 `update.look`（`none` / `ready` / `update`）。
- 页面没有更新插件和剪贴板插件的权限：`app/capabilities/default.json` 仅授予 `main`、`captions` 两个窗口 `core:default`、`core:window:allow-start-dragging`、`dialog:default`，且 CSP 锁在 `default-src 'self'`；看、下、装都在外壳里。开发构建（`tauri dev`）不看，它没有装好的应用可替换。
- 去哪里看只有一个值：`app/tauri.conf.json` 的 `plugins.updater.endpoints`，现在是 GitHub 上最新一个 Release 里的 `latest.json`：

  `https://github.com/zuchiangmai/ZuTalk/releases/latest/download/latest.json`

  各平台去哪里下、签名是什么，都写在这个 JSON 里，是数据；平台的差别是 `scripts/updates.py` 里 `KINDS` 表的一行。

## 2. 签名钥匙（只做一次）

这是 Tauri 的 minisign 钥匙对，和旧版 Sparkle 的 Ed25519 钥匙不是同一把。私钥只放在负责人这台 Mac 的登录钥匙圈里，服务名 `zutalk-update`，不进仓库、不进 CI；公钥写进 `app/tauri.conf.json` 的 `plugins.updater.pubkey`，随仓库提交。公钥是空的时候，`scripts/updates.py` 拒绝写清单，所以发不了版。

```sh
cargo install tauri-cli --locked             # 已经有 `cargo tauri` 就跳过
K="$(mktemp -d)/zutalk-update.key"
cargo tauri signer generate --ci -w "$K"     # 不设口令：私钥只放钥匙圈，钥匙圈本身是加密的
pbcopy < "$K"
security add-generic-password -a "$USER" -s zutalk-update -U -w "$(pbpaste)"
pbcopy < /dev/null                           # 清掉剪贴板
security find-generic-password -s zutalk-update -w | awk '{print length($0)}'
tr -d '\n' < "$K" | wc -c                    # 和上一行的数一样，才算存全了
cat "$K.pub"                                 # 公钥：贴进 app/tauri.conf.json 的 pubkey，提交
rm -P "$K"
```

私钥丢了，已经装好的应用就再也认不出新版本，只能每个人手动重装一次。所以另在负责人的密码管理器里存一份。用的时候一律 `security find-generic-password -s zutalk-update -w` 直接喂给命令或环境变量，不打印、不写进文件。

## 3. 发一版

1. **升版本（4 处同步检查点，由 `app/tests/updates.rs::release_versions_and_manifests_are_aligned` 守护）。**
   - 根目录 `Cargo.toml` 的 `workspace.package.version`（5 个成员 crate 均继承 `version.workspace = true`）；
   - `core/Cargo.toml` 与 `server/Cargo.toml` 里 `zu = { version = "<版本>", path = "../zu" }`；
   - `ui/package.json`（及 `ui/package-lock.json` 顶层自身字段）的 `"version"`；
   - `app/tauri.conf.json` 的 `bundle.macOS.bundleVersion` 加一（省略顶层 `"version"` 由 Tauri v2 继承 `app/Cargo.toml`）。构建号接着旧版往上数，永不复用（0.8.5 是 65，交接录音的 0.8.6 是 66，新版第一版 v0.9.0 是 67）。
2. **门禁。** 在仓库根目录跑全仓门禁，覆盖 `rustfmt`、`clippy -D warnings`、全部 10 个 `scripts/build/rules/*.sh`、全部 Rust 测试与 UI `check` / `test` / `build`（含 `index.html`、`captions.html`、`viewer.html` 三入口）：

   ```sh
   RUSTC_WRAPPER="" scripts/build/check.sh
   ```

3. **在 Mac 上构建、签名。** 在仓库根目录跑。通用二进制，一份给 Apple 芯片和 Intel 两种 Mac。**务必传入生产 `ZUTALK_SERVER`**（未传时 `app/src/main.rs` 编译期默认回落到 `https://localhost:8443`）：

   ```sh
   rustup target add aarch64-apple-darwin x86_64-apple-darwin
   TAURI_SIGNING_PRIVATE_KEY="$(security find-generic-password -s zutalk-update -w)" \
   TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
   ZUTALK_SERVER="https://<正式服务器域名>" \
   cargo tauri build --target universal-apple-darwin
   ```

   `target/universal-apple-darwin/release/bundle/` 下面：`macos/ZuTalk.app.tar.gz` 和它的 `.sig` 给更新用；`dmg/ZuTalk_<版本>_universal.dmg` 给新装的人，也给 Sparkle 那一跳（§4）。`codesign --verify --deep --strict` 核一下 `macos/ZuTalk.app`：整包是 ad-hoc 签名（配置里 `signingIdentity: "-"`），和旧版一样。后续如切到 Apple Developer ID 签名与公证，将 `signingIdentity` 改为证书名并在构建后跑 `xcrun notarytool submit ... --wait` + `xcrun stapler staple`，再核对 `spctl --assess --type execute`。
4. **Windows、Linux（三平台打包与图标补齐）。**
   - 当前 `app/icons/` 只含 `32x32.png`、`128x128.png`、`icon.png`。首次打 Windows / Linux 包前，在 `app/` 下执行 `cargo tauri icon icons/icon.png` 生成 `icons/icon.ico` 与 `icons/icon.icns` 并加入 `app/tauri.conf.json` 的 `bundle.icon`。
   - 在 Windows / Linux 机器上构建时不签（同样传生产 `ZUTALK_SERVER`）：
     `ZUTALK_SERVER="https://<正式服务器域名>" cargo tauri build --config '{"bundle":{"createUpdaterArtifacts":false}}'`
   - 把产物 `bundle/nsis/ZuTalk_<版本>_x64-setup.exe` 或 `bundle/appimage/ZuTalk_<版本>_amd64.AppImage` 拷回本机 Mac 上签名，私钥不离开本机 Mac：

     ```sh
     TAURI_PRIVATE_KEY="$(security find-generic-password -s zutalk-update -w)" TAURI_PRIVATE_KEY_PASSWORD="" \
     cargo tauri signer sign <文件>               # 写出 <文件>.sig；变量名以 `cargo tauri signer sign --help` 为准
     ```

     Linux 只有 AppImage 支持应用内自更新。
5. **写清单。** 把要发的文件放进一个空文件夹：每个更新文件挨着它的 `.sig`，再加上 DMG。然后：

   ```sh
   python3 scripts/updates.py <文件夹>
   ```

   它从 `Cargo.toml` 读版本，从 `app/tauri.conf.json` 读地址和公钥，写出 `<文件夹>/latest.json`。下载地址按地址推出来：`…/releases/download/v<版本>/<文件名>`，所以仓库名不会抄错。有一个文件不是用配置里那把钥匙签的、没有 `.sig`、同一个平台有两个文件，或者一个更新文件都没有，它就什么都不写，退出 1。
6. **发布。** 在 `zuchiangmai/ZuTalk` 建 `v<版本>` 的 Release，标为 Latest，传上文件夹里的全部文件，再加上 Sparkle 那一跳的 `appcast.xml`（§4）：

   ```sh
   gh release create v<版本> --repo zuchiangmai/ZuTalk --latest --title "ZuTalk <版本>" \
     --notes-file <说明> <文件夹>/* <appcast.xml>
   ```

   传完下载 `…/releases/latest/download/latest.json` 看一眼，版本对、各平台的地址都打得开。

## 4. 在 `zulangue` 发 0.8.6 交接版与 Sparkle `zu` 频道跳跃

旧版（0.8.x）用 Sparkle 读 `https://github.com/4seas-community/ZuTalk/releases/latest/download/appcast.xml`（仓库已转到 `zuchiangmai/ZuTalk`，靠 GitHub 的重定向）。旧版当前是 `0.8.5`（构建号 `65`），最后一版是 `0.8.6`（构建号 `66`）：它把录音交接给新版，交接完才看得到新版（[migrate.md](migrate.md) §4）。

### 4.1 在 `/Users/kant/Zuzalu/zulangue` 构建并发布 0.8.6（负责人手工执行）

以下命令均在旧仓库 `/Users/kant/Zuzalu/zulangue` 下运行（新版构建期间绝不改动旧仓库）：

```sh
cd /Users/kant/Zuzalu/zulangue

# 1. 核对旧仓库干净且当前处于 0.8.5（构建 65）
git status --short
grep '^version = ' Cargo.toml

# 2. 预检并应用 0.8 交接补丁（含 handover.rs、LibraryHandover.swift、channel = "zu"）
git apply --check /Users/kant/Zuzalu/zutalk/docs/migrate-0.8.patch
git apply /Users/kant/Zuzalu/zutalk/docs/migrate-0.8.patch

# 3. 升版到 0.8.6（同步 Cargo.toml、MARKETING_VERSION = 0.8.6、CURRENT_PROJECT_VERSION = 66）
just bump 0.8.6

# 4. 检查 FFI 编译并重新生成 UniFFI Swift 绑定、将 LibraryHandover.swift 同步进 ZuTalk.xcodeproj
cargo check -p vt-ffi
just dev

# 5. 用真实 175 条录音目录副本在隔离 HOME 下启动一次，核对 handover/ 完整且原库未改
TMP_HOME="$(mktemp -d)"
mkdir -p "$TMP_HOME/Library/Application Support"
cp -R "$HOME/Library/Application Support/ZuTalk" "$TMP_HOME/Library/Application Support/ZuTalk"
CFFIXED_USER_HOME="$TMP_HOME" open -W macos/ZuTalk/build/Debug/ZuTalk.app
ls -la "$TMP_HOME/Library/Application Support/ZuTalk/handover/"
jq '.recordings | length' "$TMP_HOME/Library/Application Support/ZuTalk/handover/index.json"
# 核对完后删除临时目录：rm -rf "$TMP_HOME"

# 6. 跑旧仓门禁并发布 v0.8.6（构建 66）；妥善保存这次生成的 appcast.xml 供 §4.2 合并
just ci-check
just release-sparkle-adhoc   # 或 just release-ship 直接打 tag 并发布到 GitHub
```

### 4.2 生成双项签名 `appcast.xml`（让 0.8.6 跳到 v0.9.0）

Sparkle 装新版，要五件事都成立：

- **同一个应用。** bundle id `xyz.voice.zutalk`，包名 `ZuTalk.app`：配置里的 `identifier` 和 `productName`。系统给的麦克风、录屏授权跟着 bundle id 走，不用重来。
- **构建号比 66 大。** `bundle.macOS.bundleVersion`，v0.9.0 是 `67`。Sparkle 拿已装那份的构建号和新包里的比，新的不大就当降级攻击，不装。
- **整包签名有效。** ad-hoc，`signingIdentity: "-"`。
- **用旧版的钥匙签。** DMG 用旧版 Sparkle 的 Ed25519 私钥签，appcast 本身也要签（旧版开了 `SURequireSignedFeed`）。那把私钥在这台 Mac 的登录钥匙圈里，账户名是改名前的 `Zulangue`。
- **在 `zu` 频道上。** 新版那一项带 `<sparkle:channel>zu</sparkle:channel>`。0.8.5 及更早不认这个频道，只会更新到 0.8.6；0.8.6 写好交接才允许这个频道。所以经由自动更新，没有人越过交接。

前三件由 `app/tests/updates.rs` 守着。做法：用旧仓库 `zulangue` 的 `justfile` 里 `sparkle-appcast` 下的同一份 Sparkle 2.9.4 工具（下载地址和 SHA-256 照抄那里）：

```sh
mkdir hop && cp <0.8.6 那个 Release 的 appcast.xml> hop/appcast.xml
cp target/universal-apple-darwin/release/bundle/dmg/ZuTalk_0.9.0_universal.dmg hop/
<Sparkle 工具>/bin/generate_appcast --account Zulangue --channel zu \
  --download-url-prefix "https://github.com/zuchiangmai/ZuTalk/releases/download/v0.9.0/" \
  --maximum-versions 2 -o hop/appcast.xml hop
```

`hop` 里只放 `0.8.6` 的 `appcast.xml` 和新版这一个 DMG（放了旧版的 DMG 它会去做增量包）。生成后核对 `hop/appcast.xml`：两项都在，`0.8.6` 那项 `sparkle:version` 是 `66`、不带频道，新版那项 `sparkle:version` 是 `67`、带 `<sparkle:channel>zu</sparkle:channel>`；`sparkle:edSignature=` 和 `<!-- sparkle-signatures:` 都在。然后随 §3 第 6 步一起上传。

这一跳只做一次，但这份 `appcast.xml` 要一直跟着走：以后每一个标为 Latest 的 Release 都原样再带上它（从第一个新版的 Release 下下来再传）。还没开过的旧版只认「最新那个 Release 里的 `appcast.xml`」，最新的里面没有，它们就再也跳不过来；它指向的 DMG（`0.8.6` 的和 `v0.9.0` 的）也一直不能删。旧版跳过来以后，从 `latest.json` 自己更新到最新。

## 5. 服务器与前端边缘部署配置

### 5.1 服务器环境变量（`server/src/main.rs` 全部 16 项）

`zutalk-server` 单进程启动，全部配置来自以下 16 个环境变量（与源码逐一核对）：

| 环境变量 | 默认值 | 单位 / 格式 | 敏感级别 | 用途 |
|---|---|---|---|---|
| `ZUTALK_DATA` | `data` | 目录路径 | 普通 | 持久化账本（SQLite）、临时房间与 `<ZUTALK_DATA>/logs/zutalk-server.jsonl` |
| `ZUTALK_LOG` | `info` | `info` / `debug` | 普通 | 日志级别；`debug` 开启每分钟 `server.sweep` 等调试日志 |
| `ZUTALK_LISTEN` | `127.0.0.1:8000` | `ip:port` | 普通 | HTTP 监听地址（由边缘反代终结 TLS 后转发） |
| `ZUTALK_ORIGIN` | `http://127.0.0.1:8000` | URL 前缀 | 普通 | 对外公开源站地址（如 `https://talk.example.org`），用于拼观众链接 `.../r/<id>#<key>` |
| `ZUTALK_COORDINATORS` | `""` | 逗号分隔 DID | 普通 | 社区协调人 DID 列表，仅列表内签名者可授额与调配 |
| `ZUTALK_DAILY_SONIOX` | `40` | 次 / UTC 日 | 普通 | 社区成员每日 Soniox 临时钥匙租借上限（`DEFAULT_DAILY_SONIOX_TIMES`） |
| `ZUTALK_DAILY_OPENROUTER` | `30` | 次 / UTC 日 | 普通 | 社区成员每日 OpenRouter 临时钥匙租借上限（`DEFAULT_DAILY_OPENROUTER_TIMES`） |
| `INVITES_URL` | `https://zulangue-invite.exe.xyz` | URL | 普通 | 0.8 旧版邀请服务地址，认领旧邀请时调用其 `POST /v1/retire` |
| `INVITES_TOKEN` | `""` | Bearer 密钥 | **机密** | 旧邀请服务的管理员口令（须与旧服务 `ZULANGUE_ADMIN_TOKEN` 一致），绝不写日志 |
| `SONIOX_URL` | `https://api.soniox.com` | URL | 普通 | Soniox 上游 API 根地址 |
| `SONIOX_API_KEY` | `""` | API 密钥 | **机密** | Soniox 主钥匙，用于换发短期临时钥匙，绝不写日志或离机 |
| `SONIOX_MICROS_PER_SECOND` | `50` | µ$ / 秒 | 普通 | Soniox 单秒音频计费估算（0.8 实测约 46 µ$/s 含翻译） |
| `SONIOX_SESSION_CAP_S` | `300` | 秒 | 普通 | 单次 Soniox 临时钥匙最长有效期（整段计费，短周期便于在停顿处换钥匙） |
| `OPENROUTER_URL` | `https://openrouter.ai/api/v1` | URL | 普通 | OpenRouter 上游 API 根地址 |
| `OPENROUTER_MANAGEMENT_KEY` | `""` | Provisioning 密钥 | **机密** | OpenRouter 管理钥匙，用于签发带限额的子钥匙，绝不写日志或离机 |
| `OPENROUTER_SLICE_MICROS` | `100000` | µ$ / 次 | 普通 | 单次 OpenRouter 子钥匙限额切片（$0.10，长文可续租） |

### 5.2 前端边缘反代与旧邀请服务要求

- **观众页 `/r/*` 静态托管与同源反代**：
  - 将 `ui/dist/`（包含 `viewer.html` 与 `assets/*`）静态托管在 `ZUTALK_ORIGIN` 同一源下；
  - 将所有 `GET /r/*` 路径重写（内部 rewrite，保留浏览器 URL 里的 `#<key>` 片段，片段永不发往服务器）到 `viewer.html`；
  - 将 `/rooms/*`（特别是 `GET /rooms/{id}/events` 的 SSE 长连接，关闭反代响应缓冲 `proxy_buffering off`）、`/keys/*`、`/quota`、`/invites/*`、`/signin/*` 等 API 路径反代到 `ZUTALK_LISTEN`。
- **TLS 与边缘限流**：强制 HTTPS（HSTS），在反代层为 `/rooms/*/events` 放宽读超时并为建房/认领接口配置基础速率限制。
- **0.8 旧邀请服务退场接口**：在 `INVITES_URL` 所在服务上线 `POST /v1/retire`，核对 `Authorization: Bearer <INVITES_TOKEN>`（与 `ZULANGUE_ADMIN_TOKEN` 对齐），确保 0.8 邀请无感搬运至新版后立即在旧服务作废。

## 6. 发布前真机与 8 小时手工验收清单

以下项无法在无真麦克风、无真实云端 API 钥匙或无真机图形界面的 CI/沙盒中替代，发 v0.9.0 前须逐项实测勾选：

- [ ] **三平台音频硬件 7 项清单**（逐条对照 `core/tests/audio.rs` 文件头手工清单）：在 macOS、Windows、Linux 实测热插拔麦克风、系统声权限拒绝对话框、合盖睡眠恢复（>60 s 结束并封盘不丢录音）、暂停/继续与采样率切换，并跑通 `cargo test -p zutalk-core --test audio -- --ignored`。
- [ ] **8 小时真机实录压测**：连续录音 8 小时（含屏幕常亮与切后台），核查内存无持续增长、实时字幕与录制尾延迟不漂移、日志轮换（8 MB × 5 份）正常、结束封盘与检索/回放/导出流畅，自检包 `Tally` 紧凑 JSON `≤ 8192 B`、`stats.full == 0`、`zutalk_log::refused() == 0`。
- [ ] **多显示器字幕窗与 WebView 视觉走查**：在 macOS / Windows / Linux 核查 4 个字幕场合（自看 / 字幕条 / 舞台全屏 / 面对面）的置顶、穿透点击、透明合成、拖拽位置记忆、窄窗/深浅色排版，以及 3 窗口（主窗 / 字幕窗 / 浏览器 `viewer.html`）的字体与对比度。
- [ ] **真实 Soniox / OpenRouter 钥匙读书会端到端实测**：配好真实钥匙跑通 `cargo test -p zutalk-core --test live -- --ignored` 与 `write -- --ignored`，再实际录制一场带主题包（词表 + 背景）的真实读书会，确认现场字幕、整场精修、自动 AI 标题/话题标签、仅用社区额度触发 `keys.async_refused` 降级提示与城邦交档流程正常。
- [ ] **`0.8.5 → 0.8.6 → 0.9.0` 升级与自更新实测**：在一台装有 `0.8.5`（带真录音副本）的 Mac 上依次执行「检查更新 → 装 `0.8.6`（写 `handover/`）→ 再检查更新 → 装 `0.9.0`」，核对录音无损搬入、麦克风/录屏授权保留；再在真机实测 Tauri 从 `latest.json` 覆盖 `/Applications/ZuTalk.app` 并重启。

## 7. 已知后续增强（不阻塞 v0.9.0）

- **签名里写版本号**：插件支持 `requireSignedVersion`，可防止「旧版文件配新版本号」降级。待构建机升级到会在 trusted comment 写入版本号的 `tauri-cli` 后，在 `app/tauri.conf.json` 的 `plugins.updater` 开启该项并同步在 `scripts/updates.py` 校验。
- **Apple Developer ID 签名与公证**：v0.9.0 沿用 ad-hoc 签名（与 0.8 一致，含 macOS 权限继承）；后续若申请 Developer ID 证书，按 §3 第 3 步切到签名公证并同步启用系统钥匙圈。
