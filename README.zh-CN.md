<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/logo-wordmark-dark.svg">
    <img src="./assets/logo-wordmark.svg" alt="Lanroam" width="460">
  </picture>
</h1>

<p align="center">
局域网鼠标键盘共享 — 让你在触手可及的设备之间，自由漫游。
</p>

<p align="center">
  <a href="https://github.com/zlx2019/lanroam/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/lanroam/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/zlx2019/lanroam/releases"><img src="https://img.shields.io/github/v/release/zlx2019/lanroam?color=12B5A2" alt="Release" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-12B5A2" alt="Platform" />
  <img src="https://img.shields.io/badge/Rust-1.96-dea584?logo=rust&logoColor=white" alt="Rust 1.96" />
  <img src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white" alt="Tauri 2" />
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
</p>

<p align="center">
  <a href="./README.md">English</a> · <b>简体中文</b>
</p>

---

**Lanroam** 是一个免费开源的鼠标键盘共享应用：用一台电脑的鼠标、键盘或触摸板控制身边所有的设备，剪贴板和文件也能跟着走。

每台运行着 Lanroam 的设备，都是局域网里一个对等的节点。你的几台设备加入同一个桌面组，它们的屏幕按照在桌上的摆放，排列在一张共享的画布上。设备之间点对点传输，默认为双向认证的 TLS 1.3 加密。

## ✨ 功能特性

- **操控简易** —— 通过鼠标的移入移出即可切换操控设备，也可以通过快捷键快速切换。
- **剪贴板共享** —— 在任意设备上复制的文本、图片/截图，以及 32 MB 以内的文件，都会跟着光标带到其他设备上。
- **文件传输** —— 把文件或文件夹直接拖过屏幕边缘，放到另一台电脑的桌面、文件夹或应用窗口里。
- **局域网 P2P** —— 每台设备都是对等节点，无需服务器、云端或账号；设备上下线实时感知，断线自动重连。
- **零配置发现** —— mDNS 为主、UDP 组播兜底；附近的设备自己就会出现。
- **PIN 加入** —— 在新设备上输入组内成员显示的 6 位 PIN 即可加入，PIN 通过一次绑定双方 TLS 证书的口令认证密钥交换来校验。
- **默认安全** —— 基于 QUIC 的 TLS 1.3 双向认证，设备身份以证书指纹绑定；输入只会发往你所在组的成员。
- **常驻菜单栏** —— 在菜单栏（macOS）或托盘（Windows）里暂停、锁定或切换到某台设备，也可以设为开机启动。

### ⌨️ 默认快捷键

可在 **设置 → 控制** 里改成你自己的组合键；Mac 上的 Alt 即 Option。

| 快捷键 | 作用 |
|---|---|
| `Ctrl+Alt+1..9` | 控制第 N 台设备（编号见「排列」页） |
| `Ctrl+Alt+方向键` | 控制该方向的相邻设备 |
| `Ctrl+Alt+L`、`Scroll Lock` | 把光标锁定在当前设备，再按解锁 |
| `Ctrl+Alt+Esc` | 回到本机并暂停切换，再按恢复 |

## 📥 安装

从 [Releases](https://github.com/zlx2019/lanroam/releases) 下载。

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

## 🚀 快速上手

1. 在同一局域网的每台电脑上安装并打开 Lanroam。
2. 在「设备」页的「附近」里选一台设备点**加入**，输入对方屏幕上显示的 6 位 PIN。
3. 在「排列」页把各台设备的屏幕拖成它们在桌上的位置。
4. 把光标推过相邻的屏幕边缘，就到了另一台电脑上。

## ❓ 常见问题

**光标停在 Mac 的屏幕边缘出不去，或者按键到不了另一台设备。**
Lanroam 需要「系统设置 → 隐私与安全性」中的**辅助功能**和**输入监控**权限，而且 macOS 要等应用重启后才会生效（Lanroam 会提示你重启）。使用 CLI 时，需要授权的是你的终端。

**更新版本后权限失效了（macOS）。**
目前只做了临时签名，授权与二进制本体绑定，每次新构建都会让旧授权作废 —— 而且在列表里重新勾选旧条目是没用的。请在两个列表中都移除 Lanroam，重新打开应用并授权新条目，然后重启 Lanroam。

**macOS 上始终看不到其他设备。**
macOS 15+ 会在首次启动时申请**本地网络**权限 —— 必须允许，否则设备发现会静默失败。可在「系统设置 → 隐私与安全性 → 本地网络」中重新开启。

**Windows 上始终看不到其他设备。**
设备发现和控制都需要专用网络下的入站防火墙规则，安装程序会自动添加。另外请确认 Windows 把当前网络识别为**专用网络**，而不是公用网络。

## 🔨 从源码构建

桌面端是 Tauri 2 + React（`apps/desktop`），与命令行节点共用一个无 UI 的引擎：

```text
deps/lan-kit        共享的局域网基础库：身份、双向 TLS 1.3、发现、分帧
deps/lanroam-input  键盘与鼠标：捕获、注入、键位映射、边缘切换
deps/lanroam-core   引擎：桌面组、QUIC 传输、协议、输入会话
deps/lanroam-cli    命令行节点，用于协议调试和多机测试
apps/desktop        桌面端应用
```

Rust 版本由 `rust-toolchain.toml` 固定（1.96，edition 2024）；桌面端还需要 Node 22+ 和 pnpm。

```bash
cd apps/desktop && pnpm install && pnpm tauri build  # 构建当前平台的应用
cargo build --release -p lanroam-cli                 # 构建命令行节点
```

## 🤝 参与贡献

开发时如何运行应用和 CLI、CI 会跑的检查以及开发约定，见 [CONTRIBUTING.md](./CONTRIBUTING.md)。安全问题请按 [SECURITY.md](./SECURITY.md) 的说明私下报告。

## 📄 许可证

[MIT](./LICENSE)
