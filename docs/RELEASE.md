# 发布与应用内更新

> 2026-11-16。新版怎么自己更新，负责人发一版要做的事，以及旧版的 Sparkle 怎么一次跳到新版。设计落在哪里见 `DESIGN.md` §5.10。

## 1. 应用怎么更新自己

- 外壳用 Tauri 官方的更新插件 `tauri-plugin-updater`，版本锁在 `=2.13.2`。启动时看一次，之后每 6 小时看一次，直到下到一个比自己新的版本、并用配置里的公钥核对过签名；这时界面顶栏出现一个按钮「更新」。
- 按下去：有录音在进行就不装，说 `update.recording`；没有才装，装好就重启。装不上说 `update.install`，下好的版本留着，可以再按。录音的转写如果正写到一半，重启后照崩溃恢复的路重写，不丢字。
- 找不到、下不来、签名不对，都说一次 `update`，直到某次看得顺利。
- 页面没有更新插件的权限：看、下、装都在外壳里。开发构建（`tauri dev`）不看，它没有装好的应用可替换。
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

1. **升版本。** 根目录 `Cargo.toml` 的 `workspace.package.version`；`app/tauri.conf.json` 的 `bundle.macOS.bundleVersion` 加一。构建号接着旧版往上数，永不复用（0.8.5 是 65，交接录音的 0.8.6 是 66，新版第一版是 67）。
2. **门禁。** `cargo test --workspace --no-fail-fast`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`，`ui/` 里 `npm run check`、`npm test`、`npm run build`。
3. **在 Mac 上构建、签名。** 在仓库根目录跑。通用二进制，一份给 Apple 芯片和 Intel 两种 Mac：

   ```sh
   rustup target add aarch64-apple-darwin x86_64-apple-darwin
   TAURI_SIGNING_PRIVATE_KEY="$(security find-generic-password -s zutalk-update -w)" \
   TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
   ZUTALK_SERVER="https://<我们的服务器>" \
   cargo tauri build --target universal-apple-darwin
   ```

   `target/universal-apple-darwin/release/bundle/` 下面：`macos/ZuTalk.app.tar.gz` 和它的 `.sig` 给更新用；`dmg/ZuTalk_<版本>_universal.dmg` 给新装的人，也给 Sparkle 那一跳（§4）。`codesign --verify --deep --strict` 核一下 `macos/ZuTalk.app`：整包是 ad-hoc 签名（配置里 `signingIdentity: "-"`），和旧版一样。
4. **Windows、Linux（有了再做）。** 在那台机器上构建，构建时不签：`cargo tauri build --config '{"bundle":{"createUpdaterArtifacts":false}}'`。把 `bundle/nsis/ZuTalk_<版本>_x64-setup.exe` 或 `bundle/appimage/ZuTalk_<版本>_amd64.AppImage` 拿到这台 Mac 上签，私钥不离开这台 Mac：

   ```sh
   TAURI_PRIVATE_KEY="$(security find-generic-password -s zutalk-update -w)" TAURI_PRIVATE_KEY_PASSWORD="" \
   cargo tauri signer sign <文件>               # 写出 <文件>.sig；变量名以 `cargo tauri signer sign --help` 为准
   ```

   Linux 只有 AppImage 能自己更新。
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

## 4. 从 Sparkle 跳过来

旧版（0.8.x）用 Sparkle 读 `https://github.com/4seas-community/ZuTalk/releases/latest/download/appcast.xml`（仓库已转到 `zuchiangmai/ZuTalk`，靠 GitHub 的重定向）。旧版的最后一版是 0.8.6（构建号 66）：它把录音交接给新版，交接完才看得到新版（[migrate.md](migrate.md) §4）。所以新版的 Release 里要有一份 `appcast.xml`，里面两项：0.8.6 那一项原样留着，给还没到 0.8.6 的人；新版的 DMG 一项，在 `zu` 频道上。已经发出去的 appcast 一个都不改。

Sparkle 装新版，要五件事都成立：

- **同一个应用。** bundle id `xyz.voice.zutalk`，包名 `ZuTalk.app`：配置里的 `identifier` 和 `productName`。系统给的麦克风、录屏授权跟着 bundle id 走，不用重来。
- **构建号比 66 大。** `bundle.macOS.bundleVersion`，现在是 67。Sparkle 拿已装那份的构建号和新包里的比，新的不大就当降级攻击，不装。
- **整包签名有效。** ad-hoc，`signingIdentity: "-"`。
- **用旧版的钥匙签。** DMG 用旧版 Sparkle 的 Ed25519 私钥签，appcast 本身也要签（旧版开了 `SURequireSignedFeed`）。那把私钥在这台 Mac 的登录钥匙圈里，账户名是改名前的 `Zulangue`。
- **在 `zu` 频道上。** 新版那一项带 `<sparkle:channel>zu</sparkle:channel>`。0.8.5 及更早不认这个频道，只会更新到 0.8.6；0.8.6 写好交接才允许这个频道。所以经由自动更新，没有人越过交接。

前三件由 `app/tests/updates.rs` 守着。做法：用旧仓库 `zulangue` 的 justfile 里 `sparkle-appcast` 下的同一份 Sparkle 2.9.4 工具（下载地址和 SHA-256 照抄那里），

```sh
mkdir hop && cp <0.8.6 那个 Release 的 appcast.xml> hop/appcast.xml
cp target/universal-apple-darwin/release/bundle/dmg/ZuTalk_<版本>_universal.dmg hop/
<Sparkle 工具>/bin/generate_appcast --account Zulangue --channel zu \
  --download-url-prefix "https://github.com/zuchiangmai/ZuTalk/releases/download/v<版本>/" \
  --maximum-versions 2 -o hop/appcast.xml hop
```

`hop` 里只放 0.8.6 的 appcast 和新版这一个 DMG：放了旧版的 DMG，它会去做增量包。生成后核对 `hop/appcast.xml`：两项都在，0.8.6 那项 `sparkle:version` 是 66、不带频道，新版那项 `sparkle:version` 是 67、带 `<sparkle:channel>zu</sparkle:channel>`；`sparkle:edSignature=` 和 `<!-- sparkle-signatures:` 都在。然后随 §3 第 6 步一起上传。

这一跳只做一次，但这份 `appcast.xml` 要一直跟着走：以后每一个标为 Latest 的 Release 都原样再带上它（从第一个新版的 Release 下下来再传）。还没开过的旧版只认「最新那个 Release 里的 appcast.xml」，最新的里面没有，它们就再也跳不过来；它指向的 DMG（0.8.6 的和新版第一版的）也一直不能删。旧版跳过来以后，从 `latest.json` 自己更新到最新。

最后在一台装着 0.8.5 的 Mac 上验：「检查更新」→ 装上 0.8.6 → 它写好交接 → 再「检查更新」→ 装上新版 → 打开是新版，录音都搬过来了，麦克风授权还在。

## 5. 还没做的

- **签名里写版本号。** 插件有 `requireSignedVersion`：签名的可信注释里写着版本号，就能挡住「旧版的文件配上新版本号」这种降级。本机的 tauri-cli 2.7.1 签出来的注释里没有版本号，所以现在没开。换用会写版本号的 CLI 之后，在 `plugins.updater` 里打开它，`scripts/updates.py` 也同时核对。
- **Windows、Linux 还没发过。** 表里有它们的行；真机上都没跑过。
- **真机没跑过的：** Sparkle 那一跳；Tauri 在 macOS 上替换 `/Applications/ZuTalk.app` 再重启（没有写权限时会弹系统的密码框）。
