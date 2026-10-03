# FileHop · 轻渡

文件，轻松到另一边。

使用 Tauri 2、Vue 3、TypeScript 和 Rust 构建的局域网文件传输桌面应用。两台电脑运行同一个 App，即可互相发送文件。

## 本地开发

需要 Node.js 22.12+、pnpm、Rust，以及 [Tauri 对应操作系统的开发依赖](https://v2.tauri.app/start/prerequisites/)。

```sh
pnpm install
pnpm tauri dev
```

`pnpm dev` 只启动浏览器界面预览（http://localhost:1432），文件选择、设备发现和传输需要在桌面 App 中使用。

## 两台电脑互传

1. 在两台电脑上打开 FileHop，连接到可互相访问的同一局域网。
2. 系统询问局域网访问或防火墙权限时，允许 FileHop 访问当前网络。
3. 在发送方选择文件或文件夹，也可直接拖入，选择发现的另一台电脑。若没有自动发现，可输入对方 App 显示的 `IP:端口`。
4. 点击发送，首次连接时在两台电脑上核对本次连接的完整校验码（8 组，每组 4 个字符）；两边分别确认后开始传输。可同时勾选「长期信任此设备」，下次连接时本机会自动确认。
5. 接收的文件保存在设置中显示的目录。默认使用下载目录中的 `FileHop` 文件夹。

支持文件与文件夹混合队列（每次最多 100 项）、进度显示、取消、拒绝接收、长期信任设备、设备名称和接收目录设置。收到同名文件时保留原文件，为新文件选择不冲突的名称。

### 发送文件夹

点击「选择文件夹」或将文件夹拖入发送区，可与普通文件一起发送。点击发送后，每个文件夹会先自动压缩为同名 `.zip` 文件，再通过加密连接发送；压缩期间可取消。ZIP 保留原文件夹、子目录和空文件夹，接收方在接收目录中手动解压即可使用。

选择或移除队列中的文件夹不会触发压缩。压缩使用临时文件，发送完成、取消或失败后自动清理，原文件夹保持不变。文件夹中的符号链接或特殊文件不支持打包；压缩失败会显示原因，且不会开始发送本次队列。

Windows 的重解析点（包括部分云同步占位文件或目录）也不支持打包；可先将内容复制到普通本地文件夹后发送。

### 长期信任设备

首次传输时，核对两边的完整校验码，勾选「已核对另一台电脑的连接校验码」和「长期信任此设备」，再确认连接／接收。长期信任默认不勾选；只有明确选择后才会保存。

两台电脑分别信任对方后，以后选择文件或文字并点击发送，即可直接传输到对方的接收目录，无需再次确认。若只在一台电脑上启用，另一台仍需手动确认。设备身份和信任记录会保存，重启应用、修改名称或更换 IP 不需要重新信任；重装或丢失设备身份后需要重新核对。

可在「偏好设置 → 受信任的设备」中取消信任，下次连接恢复手动确认；已经确认的传输不受影响。设备名称和局域网广播信息不能用来建立信任，应用会通过加密握手核验已保存的设备公钥。

此功能使用新版连接协议，两台电脑都需要更新 FileHop，无法与旧版连接。设备私钥与信任记录保存在应用配置目录的 `settings.json` 中；请勿把该文件复制给其他设备。

### 发送文字或文案

在发送区切换到「文字」，输入或粘贴内容，选择接收设备后点击「发送文字」。可选填文件名，自动补全 `.txt`；留空时按时间自动命名。连接确认后，会在接收目录得到一个普通的 UTF-8 文本文件，中文、表情、换行和空格均原样保留。已互相信任的设备会自动确认。

每次最多发送 1 MB（按 UTF-8 字节计）的文字，不支持发送空白内容。切换文件／文字模式或页面不会丢失本次输入；发送后也保留草稿，方便重试或发给其他设备，点击「清空」可删除草稿。草稿只在本次运行期间保留，关闭应用后清除。

## 构建与验证

```sh
pnpm build
pnpm test
pnpm test:build
pnpm test:ui
pnpm build:desktop
```

安装包输出在 `src-tauri/target/release/bundle/`。在对应操作系统上构建该平台的安装包；正式分发时再配置代码签名。

发布构建使用 `build:desktop` 或 `build:windows`：脚本会映射 Rust 编译信息中的项目、用户目录和依赖缓存路径，减少可执行文件暴露构建机用户名的情况。直接运行 `pnpm tauri build` 不会经过此脚本。额外编译选项可通过 `RUSTFLAGS` 或 `CARGO_ENCODED_RUSTFLAGS` 传入；脚本添加路径映射后交给 Cargo，因此会覆盖 Cargo 配置文件中的 `rustflags`。

`node_modules/`、`dist/`、`src-tauri/target*/` 和 `artifacts/` 都是本地依赖或生成产物，不提交到源码仓库。安装包单独放到仓库的 Releases；发布前使用上述命令重新构建，不复用清理前的安装包。

### GitHub Actions 自动构建

[Build and Release CI](.github/workflows/release.yml) 参考 `tickets` 的标签发布流程，使用 Node.js 22、固定版本的 pnpm 和 Rust stable，按锁文件安装依赖。构建前运行发布脚本测试和 Rust 测试，打包时执行前端类型检查与构建；Linux 上还运行浏览器界面测试。CI 通过 `build:desktop` 保留发布构建的路径映射。

| 平台 | 架构 | 安装包 |
| --- | --- | --- |
| macOS | Apple Silicon / Intel | `.dmg` |
| Windows | x64 | NSIS `.exe` |
| Linux | x64 | `.deb` / `.AppImage` |

- 推送到 `main` / `master`，或向这两个分支提交 PR：自动测试并打包，在 Actions 运行详情的 **Artifacts** 下载，保留 14 天。
- 推送 `v*` 标签：所有平台测试、构建成功后，自动创建或更新对应的 GitHub Release 并上传安装包。带 `-` 的版本（例如 `v0.2.0-beta.1`）标记为预发布。
- 手动构建：在 **Actions → Build and Release CI → Run workflow** 选择分支；`tag` 留空只构建。填写已存在的版本标签（例如 `v0.1.0`）可重新构建该标签的源码并发布或更新 Release。选择标签运行时也会发布。

发布前同步修改 `package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 中的版本号，并运行 `cargo check --manifest-path src-tauri/Cargo.toml` 更新 `Cargo.lock`，将改动提交并推送。标签必须为 `v` 加应用版本，例如当前版本：

```sh
git tag v0.1.0
git push origin v0.1.0
```

发布使用 GitHub 自动提供的 `GITHUB_TOKEN`，无需额外配置发布令牌。macOS 包使用临时签名（ad-hoc），Windows 包未配置代码签名；面向正式分发时需配置对应平台的签名证书与 macOS 公证。流程及依赖参考 [Tauri 官方 GitHub Actions 文档](https://v2.tauri.app/distribute/pipelines/github/)。

### Windows exe

Windows x64 安装器支持简体中文与英文，并在缺少 WebView2 时联网安装运行环境。直接运行主程序需要电脑上已有 WebView2。首次运行时，允许 FileHop 通过当前私有网络的防火墙以接收文件。

在配置好 Tauri 开发环境的 Windows 电脑上重新打包：

```sh
pnpm install --frozen-lockfile
pnpm build:windows
```

安装包输出在 `src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/`。

也可按 [Tauri 官方交叉编译说明](https://v2.tauri.app/distribute/windows-installer/#build-windows-apps-on-linux-and-macos) 配置 LLVM、NSIS、Windows Rust target 和 `cargo-xwin`，然后在 macOS 上执行：

```sh
pnpm build:windows --runner cargo-xwin
```

`pnpm test` 运行 Rust 的真实回环传输测试；`pnpm test:build` 验证发布脚本的路径映射与编译参数保留；`pnpm test:ui` 使用本机 Chrome／Chromium 检查构建后的界面，自动创建并清理独立浏览器配置。找不到浏览器时可设置 `CHROME_PATH`。界面测试中的原生命令使用测试数据，实际网络、加密和文件落盘由 Rust 测试覆盖。截图和界面测试报告输出到 `artifacts/ui-check/`。

## 隐私与本地数据

- 应用没有账号系统、遥测或文件中转服务。文件通过两台设备之间的加密连接传输。
- 设备发现会向局域网广播设备名称、操作系统和连接地址。新安装默认名称为「我的电脑」，可在偏好设置中修改；已有用户保存的名称继续保留。
- 设备私钥、受信任设备和接收目录保存在系统应用配置目录的 `settings.json`。不要把真实配置、接收文件或含个人信息的截图放入公开仓库。
- 传输记录与文字草稿仅保留在本次运行期间。

## 实现说明

- Vue 负责交互；Tauri commands 调用 Rust，并读取任务状态。选取的文件内容不经过 WebView；输入的文字交给 Rust 编码为 UTF-8，并复用相同的文件传输协议。
- mDNS 用于发现局域网设备，TCP 默认端口为 `53318`，被占用时选择可用端口，以 App 实际显示的地址为准。
- Rust 使用 `snow` 实现 Noise XX 加密会话，通过持久化的 Curve25519 密钥验证设备身份，并为每次连接显示 128-bit 会话指纹作为校验码。未信任的设备需核对完整校验码并确认；已信任的公钥在握手验证后可自动确认。设备名、发现 ID 和 IP 均不能绕过身份验证。
- 设备私钥和信任记录与偏好设置一起原子保存；Unix 上的设置文件仅允许当前用户读写。保存信任失败时不会自动放行，取消信任也会持久化。
- 文件按块流式读写，增量计算 SHA-256。接收端先写临时文件，校验后再提交，防止把不完整的数据当作已完成文件。

当前版本针对局域网内的主动文件发送。尚未实现跨互联网穿透／中转、断点续传、接收后自动解压文件夹、后台托盘常驻或自动目录同步；两边 App 均需保持运行。当前传输记录用于本次运行期间的任务查看。

若设备发现失败，可检查是否使用访客 Wi-Fi、网络是否开启客户端隔离，或防火墙是否拦截 FileHop；手动地址连接仍需要两台电脑之间的网络允许访问。

## 许可证

本项目采用 [MIT License](LICENSE)。
