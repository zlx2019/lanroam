<p align="center">
  <img src="./assets/logo.svg" width="96" alt="Lanroam logo" />
</p>

<h1 align="center">Lanroam</h1>

<p align="center">
让你在触手可及的视界，自由漫游。
</p>


<p align="center">
  <a href="https://github.com/zlx2019/lanroam/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/lanroam/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-3ccfbe" alt="Platform" />
</p>

<p align="center">
  <a href="./README.md">English</a> · <b>简体中文</b>
</p>

---

**Lanroam** 是一个免费开源的用于鼠标、键盘共享应用。可通过一台电脑的鼠标、键盘或触摸板来控制所有的设备，并且支持剪切板、文件传输同步。

每台运行着 Lanroam 的设备，都是局域网里一个对等的节点。你的几台设备加入同一个桌面组，它们的屏幕按照在桌上的摆放，排列在一张共享的画布上。设备之间点对点传输，默认为双向认证的 TLS 1.3 加密。

## ✨ 功能特性

- 🔗 **局域网 P2P** —— 每台设备都是对等节点；无需服务端、云端或账号。
- 📡 **零配置发现** —— mDNS 为主、UDP 组播兜底；附近的设备自己就会出现。
- 🤝 **PIN 加入** —— 在新设备上输入组内成员显示的 6 位 PIN 即可加入，PIN 通过一次绑定双方 TLS 证书的口令认证密钥交换来校验。
- 🔐 **默认安全** —— 基于 QUIC 的 TLS 1.3 双向认证，设备身份以证书指纹绑定；输入只会发往你所在组的成员。
- 🗂️ **常驻菜单栏** —— 在菜单栏（macOS）或托盘（Windows）里暂停、锁定或切换到某台设备，也可以设为开机启动。

## 🗺️ 路线图

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | 共享的局域网基础库、QUIC 传输、集成测试用 CLI | ✅ |
| M1 | 键鼠捕获与注入（macOS ↔ Windows） | ✅ |
| M2 | 桌面组、屏幕布局、边缘穿越、快捷键 | ✅ |
| M3 | 桌面端：屏幕排列、配对、托盘、设置 | 🚧 |
| M4 | 剪贴板交接 | |
| M5 | 设备之间直接拖拽文件 | |

## 📥 安装

第一个版本发布后，从 [Releases](https://github.com/zlx2019/lanroam/releases) 下载。

| 平台 | 系统要求 | 安装包 |
|---|---|---|
| macOS（Apple 芯片） | macOS 13 Ventura 及以上 | `Lanroam-x.y.z-macos-aarch64.dmg` |
| macOS（Intel） | macOS 13 Ventura 及以上 | `Lanroam-x.y.z-macos-x64.dmg` |
| Windows | Windows 10 及以上，x64 | `Lanroam-x.y.z-windows-x64-setup.exe` |

每个版本也附带命令行节点（`lanroam-cli`），用法见 [CONTRIBUTING.md](./CONTRIBUTING.md#the-command-line-node)。

### 🍎 macOS

把 Lanroam 拖进「应用程序」。首次启动时，Lanroam 会引导你授予捕获和注入输入所需的**辅助功能**与**输入监控**权限。

### 🪟 Windows

安装程序会在专用网络下为 Lanroam 放行防火墙，设备发现和控制都需要它。

> 目前的安装包都没有签名。macOS 上第一次打开请右键点应用选**打开**（macOS 15 及以上：「系统设置 → 隐私与安全性」里点**仍要打开**），或者运行 `xattr -cr /Applications/Lanroam.app`；Windows SmartScreen 也可能弹出确认提示。

### 🧪 开发版（Windows）

每次推送都会构建应用（`Lanroam.exe`，免安装）和命令行节点（`lanroam-cli.exe`），并发布到滚动更新的 [`dev` 预发布版](https://github.com/zlx2019/lanroam/releases/tag/dev)。用下面的命令把最新版下载到 `%LOCALAPPDATA%\Lanroam\dev`：

```powershell
irm "https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1?$(Get-Random)" | iex
```

第一次请在管理员权限的 PowerShell 里加上 `-Firewall` 运行，放行来自局域网的连接：

```powershell
& ([scriptblock]::Create((irm "https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1?$(Get-Random)"))) -Firewall
```

## 🔒 安全

Lanroam 会转发按键、操控其他电脑的光标，所以细则需要讲明白：

- **数据不出你的局域网。** 输入在设备之间点对点传输，走 TLS 1.3，且只发往你所在组的成员。没有服务端，也没有任何遥测。
- **你敲的内容会经过网络。** 控制另一台设备时，你在那边敲的每个按键（包括密码）都会经网络加密传输。
- **组内成员彼此完全信任。** 任何成员都能控制其他成员、放新设备进组、把已有设备移出。只加入由你自己的设备组成的组。
- **加入必须输入 PIN。** PIN 显示在组内成员的屏幕上，通过绑定双方证书指纹的 SPAKE2 校验：最多尝试 3 次，之后冷却 30 秒。冒充你所选设备的第三方拿不到任何可利用的信息。
- **设备发现在局域网内公开。** mDNS 会明文广播每台设备的名称、ID、证书指纹、平台、系统版本和桌面组 ID。
- **身份密钥以明文存放**在 `~/.lanroam` 下，只有你的用户账号能读取。能访问该账号的人可以冒充这台设备。

完整说明及漏洞报告方式见 [SECURITY.md](./SECURITY.md)。

## ❓ 常见问题

**光标停在 Mac 的屏幕边缘出不去，或者按键到不了另一台设备。**
Lanroam 需要「系统设置 → 隐私与安全性」中的**辅助功能**和**输入监控**权限，而且 macOS 要等应用重启后才会生效（Lanroam 会提示你重启）。使用 CLI 时，需要授权的是你的终端。

**更新版本后权限失效了（macOS）。**
目前只做了临时签名，授权与二进制本体绑定，每次新构建都会让旧授权作废 —— 而且在列表里重新勾选旧条目是没用的。请在两个列表中都移除 Lanroam，重新打开应用并授权新条目，然后重启 Lanroam。

**macOS 上始终看不到其他设备。**
macOS 15+ 会在首次启动时申请**本地网络**权限 —— 必须允许，否则设备发现会静默失败。可在「系统设置 → 隐私与安全性 → 本地网络」中重新开启。

**Windows 上始终看不到其他设备。**
设备发现和控制都需要专用网络下的入站防火墙规则：安装程序会自动添加（开发版请按上文加上 `-Firewall` 运行一次更新脚本）。另外请确认 Windows 把当前网络识别为**专用网络**，而不是公用网络。

**光标穿越的位置不对，或者穿不过去。**
只有布局里相接的两块屏幕之间才能穿越。打开主窗口的**排列**页，其他设备会在自己的屏幕上显示编号，对照着拖成和桌面一致的摆放。

**在另一台电脑上滚动方向是反的。**
开了「自然滚动」的 Mac 控制 PC 时，滚动方向会反过来。在被控制的那台电脑上打开「设置 → 控制 → 反转滚动」，滚动速度也在那里调。

**音量键调的是另一台电脑的音量。**
控制其他设备时，媒体键和音量键默认发给被控设备。想留在眼前这台电脑上，在「设置 → 控制 → 媒体键」里选**留在本机**。

**有些窗口不响应鼠标和键盘。**
安全输入无法远程操控：Windows 上的锁屏、UAC 提示和以管理员身份运行的窗口都控制不了；macOS 上的安全输入（密码框、开启了「安全键盘输入」的终端）会让键盘无法被捕获。

**应用和 CLI 能同时运行吗？**
不能。它们共用 `~/.lanroam`，在局域网里是同一台设备；启动其中一个前先退出另一个。

## 🔨 从源码构建

桌面端是 Tauri 2 + React（`apps/desktop`），与命令行节点共用一个无 UI 的引擎：

```text
deps/lan-kit        共享的局域网基础库：身份、双向 TLS 1.3、发现、分帧
deps/lanroam-input  键盘与鼠标：捕获、注入、键位映射、边缘切换
deps/lanroam-core   引擎：桌面组、QUIC 传输、协议、输入会话
deps/lanroam-cli    命令行节点，用于协议调试和多机测试
apps/desktop        桌面端应用
```

Rust 版本由 `rust-toolchain.toml` 固定；桌面端还需要 Node 22+ 和 pnpm。

```bash
cd apps/desktop && pnpm install && pnpm tauri build  # 构建当前平台的应用
cargo build --release -p lanroam-cli                 # 构建命令行节点
```

## 🤝 参与贡献

开发时如何运行应用和 CLI、CI 会跑的检查以及开发约定，见 [CONTRIBUTING.md](./CONTRIBUTING.md)。安全问题请按 [SECURITY.md](./SECURITY.md) 的说明私下报告。

## 📄 许可证

[MIT](./LICENSE)
